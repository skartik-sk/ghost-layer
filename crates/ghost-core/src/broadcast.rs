//! Broadcast Scheduler.
//!
//! Rules (Nym baseline hygiene):
//! 1. Never broadcast over the sync connection: every send goes through a [`Broadcaster`] that
//!    opens a fresh upstream connection.
//! 2. Never broadcast right after the wallet reached the tip: every tx waits a random delay.
//! 3. Restart-safe: queued txs live in sqlite and resume on startup; deduped by txid.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rand::RngExt;
use rusqlite::{Connection, params};
use serde::Serialize;
use tokio::sync::watch;
use tracing::{info, warn};

/// Delay window. Uniform in `[min, max]`: bounded (so `hold` mode never exceeds a wallet's
/// patience) and without a mode near zero (an exponential would often send almost immediately).
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Policy {
    pub min_delay: Duration,
    pub max_delay: Duration,
}

impl Policy {
    pub fn profile(name: &str) -> Option<Self> {
        let (a, b) = match name {
            "demo" => (10, 40),
            "privacy" => (60, 600),
            _ => return None,
        };
        Some(Self { min_delay: Duration::from_secs(a), max_delay: Duration::from_secs(b) })
    }

    pub fn sample(&self) -> Duration {
        let (a, b) = (self.min_delay.as_millis() as u64, self.max_delay.as_millis() as u64);
        Duration::from_millis(rand::rng().random_range(a..=b.max(a)))
    }
}

/// What one real broadcast attempt produced.
#[derive(Clone, Debug)]
pub struct Sent {
    pub conn_id: u64,
    pub transport: String,
    pub endpoint: String,
    pub error_code: i32,
    pub error_message: String,
}

/// Sends raw tx bytes upstream over a fresh connection. Implemented by ghost-transport users.
pub trait Broadcaster: Send + Sync + 'static {
    fn broadcast(&self, raw: Vec<u8>) -> impl std::future::Future<Output = anyhow::Result<Sent>> + Send;
}

/// Final state of a queued tx, delivered to a waiting wallet call.
#[derive(Clone, Debug)]
pub enum Outcome {
    Accepted(Sent),
    Rejected(Sent),
    Failed(String),
}

#[derive(Clone, Debug, Serialize)]
pub struct Row {
    pub txid: String,
    pub status: String,
    pub received_at_ms: i64,
    pub scheduled_at_ms: i64,
    pub sent_at_ms: Option<i64>,
    pub conn_id: Option<i64>,
    pub transport: Option<String>,
    pub endpoint: Option<String>,
    pub error_code: Option<i64>,
    pub error_message: Option<String>,
}

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as i64
}

/// Computes the txid (ZIP 244 for v5+, incl. NU7 tx formats) as explorers display it.
pub fn txid_of(raw: &[u8]) -> anyhow::Result<String> {
    // The branch id only matters for pre-v5 txs; v5+ carry their own in the header.
    let tx = zcash_primitives::transaction::Transaction::read(raw, zcash_protocol::consensus::BranchId::Nu7)
        .map_err(|e| anyhow::anyhow!("not a valid Zcash transaction: {e}"))?;
    Ok(tx.txid().to_string())
}

pub struct Queue {
    db: Mutex<Connection>,
}

impl Queue {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let db = Connection::open(path)?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS broadcasts (
               txid TEXT PRIMARY KEY, raw BLOB NOT NULL, status TEXT NOT NULL,
               received_at_ms INTEGER NOT NULL, scheduled_at_ms INTEGER NOT NULL, sent_at_ms INTEGER,
               conn_id INTEGER, transport TEXT, endpoint TEXT, error_code INTEGER, error_message TEXT);",
        )?;
        Ok(Self { db: Mutex::new(db) })
    }

    pub fn list(&self) -> anyhow::Result<Vec<Row>> {
        let db = self.db.lock().unwrap();
        let mut st = db.prepare(
            "SELECT txid,status,received_at_ms,scheduled_at_ms,sent_at_ms,conn_id,transport,endpoint,error_code,error_message
             FROM broadcasts ORDER BY received_at_ms",
        )?;
        let rows = st.query_map([], |r| {
            Ok(Row {
                txid: r.get(0)?,
                status: r.get(1)?,
                received_at_ms: r.get(2)?,
                scheduled_at_ms: r.get(3)?,
                sent_at_ms: r.get(4)?,
                conn_id: r.get(5)?,
                transport: r.get(6)?,
                endpoint: r.get(7)?,
                error_code: r.get(8)?,
                error_message: r.get(9)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    fn get(&self, txid: &str) -> anyhow::Result<Option<Row>> {
        Ok(self.list()?.into_iter().find(|r| r.txid == txid))
    }

    fn insert(&self, txid: &str, raw: &[u8], scheduled_at_ms: i64) -> anyhow::Result<()> {
        self.db.lock().unwrap().execute(
            "INSERT INTO broadcasts (txid,raw,status,received_at_ms,scheduled_at_ms) VALUES (?1,?2,'pending',?3,?4)",
            params![txid, raw, now_ms(), scheduled_at_ms],
        )?;
        Ok(())
    }

    fn pending(&self) -> anyhow::Result<Vec<(String, Vec<u8>, i64)>> {
        let db = self.db.lock().unwrap();
        let mut st = db.prepare("SELECT txid,raw,scheduled_at_ms FROM broadcasts WHERE status='pending'")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    fn reschedule(&self, txid: &str, at_ms: i64) -> anyhow::Result<()> {
        self.db.lock().unwrap().execute("UPDATE broadcasts SET scheduled_at_ms=?2 WHERE txid=?1", params![txid, at_ms])?;
        Ok(())
    }

    fn finish(&self, txid: &str, status: &str, sent: Option<&Sent>, err: Option<&str>) -> anyhow::Result<()> {
        self.db.lock().unwrap().execute(
            // Raw tx bytes are wiped once the outcome is final (only `failed` keeps them for retry).
            "UPDATE broadcasts SET raw = CASE WHEN ?2 = 'failed' THEN raw ELSE x'' END, status=?2, sent_at_ms=?3, conn_id=?4, transport=?5, endpoint=?6,
             error_code=?7, error_message=?8 WHERE txid=?1",
            params![
                txid,
                status,
                now_ms(),
                sent.map(|s| s.conn_id as i64),
                sent.map(|s| s.transport.clone()),
                sent.map(|s| s.endpoint.clone()),
                sent.map(|s| s.error_code as i64),
                sent.map(|s| s.error_message.clone()).or(err.map(String::from)),
            ],
        )?;
        Ok(())
    }
}

const RETRIES: u32 = 3;

pub struct Scheduler<B: Broadcaster> {
    queue: Arc<Queue>,
    policy: Policy,
    broadcaster: Arc<B>,
    waiters: Mutex<std::collections::HashMap<String, watch::Receiver<Option<Outcome>>>>,
}

impl<B: Broadcaster> Scheduler<B> {
    pub fn new(queue: Arc<Queue>, policy: Policy, broadcaster: B) -> Arc<Self> {
        Arc::new(Self { queue, policy, broadcaster: Arc::new(broadcaster), waiters: Default::default() })
    }

    /// Re-schedules txs left pending by a previous run (restart safety).
    pub fn resume(self: &Arc<Self>) -> anyhow::Result<usize> {
        let pending = self.queue.pending()?;
        for (txid, raw, at) in &pending {
            // An overdue tx must not go out the instant we restart (and never together with other
            // resumed txs): give each its own fresh random delay.
            let at = if *at <= now_ms() {
                let at = now_ms() + self.policy.sample().as_millis() as i64;
                self.queue.reschedule(txid, at)?;
                at
            } else {
                *at
            };
            info!(target: "broadcast", %txid, in_s = (at - now_ms()) / 1000, "resuming queued tx from previous run");
            self.spawn(txid.clone(), raw.clone(), at);
        }
        Ok(pending.len())
    }

    /// Queues a tx and returns a receiver that resolves to its real outcome.
    pub fn submit(self: &Arc<Self>, raw: Vec<u8>) -> anyhow::Result<(String, watch::Receiver<Option<Outcome>>)> {
        let txid = txid_of(&raw)?;
        if let Some(rx) = self.waiters.lock().unwrap().get(&txid) {
            info!(target: "broadcast", %txid, "duplicate submit; joining existing broadcast");
            return Ok((txid, rx.clone()));
        }
        if let Some(row) = self.queue.get(&txid)? {
            // Already handled in an earlier run: report its recorded result, never re-send.
            info!(target: "broadcast", %txid, status = %row.status, "duplicate of a recorded tx");
            let out = match row.status.as_str() {
                "accepted" | "rejected" => {
                    let sent = Sent {
                        conn_id: row.conn_id.unwrap_or_default() as u64,
                        transport: row.transport.unwrap_or_default(),
                        endpoint: row.endpoint.unwrap_or_default(),
                        error_code: row.error_code.unwrap_or_default() as i32,
                        error_message: row.error_message.unwrap_or_default(),
                    };
                    if row.status == "accepted" { Outcome::Accepted(sent) } else { Outcome::Rejected(sent) }
                }
                _ => Outcome::Failed(row.error_message.unwrap_or_else(|| "previous attempt failed".into())),
            };
            return Ok((txid, watch::channel(Some(out)).1));
        }
        let delay = self.policy.sample();
        let at = now_ms() + delay.as_millis() as i64;
        self.queue.insert(&txid, &raw, at)?;
        info!(target: "broadcast", %txid, delay_s = delay.as_secs_f64(), "queued; will broadcast on a fresh connection after random delay");
        Ok((txid.clone(), self.spawn(txid, raw, at)))
    }

    fn spawn(self: &Arc<Self>, txid: String, raw: Vec<u8>, at_ms: i64) -> watch::Receiver<Option<Outcome>> {
        let (tx, rx) = watch::channel(None);
        self.waiters.lock().unwrap().insert(txid.clone(), rx.clone());
        let this = self.clone();
        tokio::spawn(async move {
            let wait = (at_ms - now_ms()).max(0) as u64;
            tokio::time::sleep(Duration::from_millis(wait)).await;
            let outcome = this.attempt(&txid, raw, at_ms).await;
            this.waiters.lock().unwrap().remove(&txid);
            let _ = tx.send(Some(outcome));
        });
        rx
    }

    async fn attempt(&self, txid: &str, raw: Vec<u8>, at_ms: i64) -> Outcome {
        let mut last_err = String::new();
        for n in 1..=RETRIES {
            match self.broadcaster.broadcast(raw.clone()).await {
                Ok(sent) => {
                    let accepted = sent.error_code == 0;
                    let status = if accepted { "accepted" } else { "rejected" };
                    if let Err(e) = self.queue.finish(txid, status, Some(&sent), None) {
                        warn!(target: "broadcast", %txid, "could not record result: {e:#}");
                    }
                    info!(target: "broadcast", %txid, status, conn = sent.conn_id, transport = %sent.transport,
                        endpoint = %sent.endpoint, scheduled_ms = at_ms, actual_ms = now_ms(),
                        late_ms = now_ms() - at_ms, code = sent.error_code, msg = %sent.error_message, "broadcast done");
                    return if accepted { Outcome::Accepted(sent) } else { Outcome::Rejected(sent) };
                }
                Err(e) => {
                    last_err = format!("{e:#}");
                    warn!(target: "broadcast", %txid, attempt = n, "broadcast attempt failed: {last_err}");
                    if n < RETRIES {
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
        }
        if let Err(e) = self.queue.finish(txid, "failed", None, Some(&last_err)) {
            warn!(target: "broadcast", %txid, "could not record failure: {e:#}");
        }
        Outcome::Failed(last_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_within_profile() {
        let p = Policy::profile("demo").unwrap();
        for _ in 0..1000 {
            let d = p.sample();
            assert!(d >= p.min_delay && d <= p.max_delay);
        }
    }

    #[test]
    fn rejects_garbage_tx() {
        assert!(txid_of(&[1, 2, 3]).is_err());
    }
}

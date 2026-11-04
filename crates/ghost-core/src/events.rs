//! Upstream event log: exactly what the light-wallet server can observe, recorded by Ghost Layer
//! itself (one JSON line per upstream request/response). Same shape as ShadeCheck's observer
//! `events.jsonl`, so `ghost-attacker` and `ghost dashboard` can analyse it directly.
//!
//! Recorded: connection (session) id, transport (direct / nym-sync / nym-broadcast), method,
//! time, block heights, and for broadcasts the payload size + SHA-256 (the server sees the full
//! raw tx anyway). Never recorded: addresses, keys, memos, raw transactions.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

pub struct EventLog {
    file: Mutex<Option<File>>,
    seq: AtomicU64,
    /// Distinguishes connection ids across gateway restarts (ids restart at 1 per process).
    run: u128,
}

fn now_ns() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos()
}

impl EventLog {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let f = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self { file: Mutex::new(Some(f)), seq: AtomicU64::new(0), run: now_ns() / 1_000_000 })
    }

    pub fn disabled() -> Self {
        Self { file: Mutex::new(None), seq: AtomicU64::new(0), run: 0 }
    }

    pub fn record(&self, method: &str, phase: &str, conn: u64, transport: &str, mut metadata: Value) {
        let mut guard = self.file.lock().unwrap();
        let Some(f) = guard.as_mut() else { return };
        if let Some(m) = metadata.as_object_mut() {
            m.insert("transport".into(), json!(transport));
        }
        let line = json!({
            "schema_version": 1,
            "sequence": self.seq.fetch_add(1, Ordering::Relaxed) + 1,
            "mode": "ghost-upstream",
            "method": method,
            "phase": phase,
            "timestamp_ns": now_ns() as u64,
            "session": format!("{}-{conn}", self.run),
            "metadata": metadata,
        });
        // Best effort: a full disk must never break the wallet's request.
        let _ = writeln!(f, "{line}");
    }
}

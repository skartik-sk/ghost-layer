//! ghost-attacker: an honest-but-curious light-wallet server operator.
//!
//! Input: the observer log the server side records (ShadeCheck `events.jsonl`: per request the
//! transport session = one TCP connection, wall-clock time, RPC method, block range, payload hash).
//! For every `SendTransaction` it tries to link the payment to a wallet's sync activity:
//!
//! 1. **Same-connection** — the send arrived on a connection that also synced.
//! 2. **Sync-then-send** — the send arrived ≤ T s after some session reached the chain tip.
//! 3. **Range fingerprint** — a sync session's first range start sits right after an earlier
//!    session's last end (resume-point chaining), linking the sessions into one history.
//! 4. **Same IP** — not observable from ShadeCheck logs (peer addresses are HMAC'd); the caller
//!    states whether all traffic shared one source IP (true for local runs).

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct Event {
    method: String,
    phase: String,
    timestamp_ns: u64,
    session: String,
    #[serde(default)]
    metadata: serde_json::Value,
}

/// How many blocks below a previous end a resume may start (wallet reorg re-check window).
const RESUME_WINDOW: u64 = 10;

#[derive(Serialize, Clone, Debug)]
pub struct SendFinding {
    pub index: usize,
    pub payload_sha256: String,
    pub same_connection: bool,
    pub sync_then_send: bool,
    /// Seconds from the most recent "reached tip" event to this send (None if no sync seen).
    pub tip_to_send_s: Option<f64>,
    pub same_ip: bool,
    pub linked: bool,
    pub linked_by: Vec<&'static str>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Analysis {
    pub source: String,
    pub threshold_s: f64,
    pub sends: Vec<SendFinding>,
    pub sync_sessions: usize,
    pub fingerprint_linked_sessions: usize,
    pub upstream_connections: usize,
    pub median_tip_to_send_s: Option<f64>,
}

impl Analysis {
    pub fn count(&self, f: impl Fn(&SendFinding) -> bool) -> usize {
        self.sends.iter().filter(|s| f(s)).count()
    }
    pub fn rate(&self) -> f64 {
        if self.sends.is_empty() { 0.0 } else { 100.0 * self.count(|s| s.linked) as f64 / self.sends.len() as f64 }
    }
}

fn is_sync(method: &str) -> bool {
    !matches!(method, "SendTransaction")
}

pub fn analyze(path: &Path, threshold_s: f64, single_source_ip: bool) -> anyhow::Result<Analysis> {
    let text = std::fs::read_to_string(path)?;
    let mut events: Vec<Event> = Vec::new();
    for (n, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        events.push(serde_json::from_str(line).map_err(|e| anyhow::anyhow!("{}:{}: {e}", path.display(), n + 1))?);
    }
    events.sort_by_key(|e| e.timestamp_ns);

    let synced_sessions: HashSet<&str> =
        events.iter().filter(|e| is_sync(&e.method)).map(|e| e.session.as_str()).collect();
    let all_sessions: HashSet<&str> = events.iter().map(|e| e.session.as_str()).collect();

    // "Reached tip" moments: a GetBlockRange response carrying the highest height seen so far
    // (from GetLatestBlock responses or block ranges).
    let mut best: u64 = 0;
    let mut tip_times: Vec<u64> = Vec::new();
    for e in &events {
        let h = e.metadata.get("height").and_then(|v| v.as_u64());
        match (e.method.as_str(), e.phase.as_str(), h) {
            ("GetLatestBlock", "response", Some(h)) => best = best.max(h),
            ("GetBlockRange", "response", Some(h)) if h >= best => {
                best = h;
                tip_times.push(e.timestamp_ns);
            }
            _ => {}
        }
    }

    let mut sends = Vec::new();
    for e in events.iter().filter(|e| e.method == "SendTransaction" && e.phase == "request") {
        if e.metadata.get("payload_size").and_then(|v| v.as_u64()).unwrap_or(0) == 0 {
            continue;
        }
        let same_connection = synced_sessions.contains(e.session.as_str());
        let last_tip = tip_times.iter().rev().find(|&&t| t <= e.timestamp_ns).copied();
        let gap = last_tip.map(|t| (e.timestamp_ns - t) as f64 / 1e9);
        let sync_then_send = gap.is_some_and(|g| g <= threshold_s);
        let mut linked_by = Vec::new();
        if same_connection {
            linked_by.push("same-connection");
        }
        if sync_then_send {
            linked_by.push("sync-then-send");
        }
        sends.push(SendFinding {
            index: sends.len() + 1,
            payload_sha256: e.metadata.get("payload_sha256").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            same_connection,
            sync_then_send,
            tip_to_send_s: gap,
            same_ip: single_source_ip,
            linked: !linked_by.is_empty(),
            linked_by,
        });
    }

    // Range fingerprint: per sync session, (first start, last end) of its GetBlockRange requests.
    let mut ranges: BTreeMap<u64, (String, u64, u64)> = BTreeMap::new(); // first_ts -> (session, first_start, max_end)
    let mut by_session: BTreeMap<String, (u64, u64, u64)> = BTreeMap::new();
    for e in events.iter().filter(|e| e.method == "GetBlockRange" && e.phase == "request") {
        let s = e.metadata.get("start_height").and_then(|v| v.as_u64()).unwrap_or(0);
        let t = e.metadata.get("end_height").and_then(|v| v.as_u64()).unwrap_or(0);
        let (lo, hi) = (s.min(t), s.max(t));
        let entry = by_session.entry(e.session.clone()).or_insert((e.timestamp_ns, lo, hi));
        entry.2 = entry.2.max(hi);
    }
    for (sess, (ts, first, end)) in &by_session {
        ranges.insert(*ts, (sess.clone(), *first, *end));
    }
    let ordered: Vec<_> = ranges.values().cloned().collect();
    let mut fingerprint_linked = 0;
    for (j, (_, first, _)) in ordered.iter().enumerate() {
        let matches = ordered[..j]
            .iter()
            .filter(|(_, _, end)| *first + RESUME_WINDOW >= *end && *first <= end + 1)
            .count();
        if matches == 1 {
            fingerprint_linked += 1;
        }
    }

    let mut gaps: Vec<f64> = sends.iter().filter_map(|s| s.tip_to_send_s).collect();
    gaps.sort_by(|a, b| a.total_cmp(b));
    let median = if gaps.is_empty() {
        None
    } else if gaps.len() % 2 == 1 {
        Some(gaps[gaps.len() / 2])
    } else {
        Some((gaps[gaps.len() / 2 - 1] + gaps[gaps.len() / 2]) / 2.0)
    };

    Ok(Analysis {
        source: path.display().to_string(),
        threshold_s,
        sends,
        sync_sessions: by_session.len(),
        fingerprint_linked_sessions: fingerprint_linked,
        upstream_connections: all_sessions.len(),
        median_tip_to_send_s: median,
    })
}

fn frac(a: &Analysis, f: impl Fn(&SendFinding) -> bool) -> String {
    format!("{} / {}", a.count(f), a.sends.len())
}

fn secs(v: Option<f64>) -> String {
    v.map(|s| format!("{s:.1} s")).unwrap_or_else(|| "—".into())
}

/// Markdown table comparing runs (before vs after), as in spec B6.2.
pub fn markdown(before: &Analysis, after: &Analysis, nym_note: Option<&str>) -> String {
    let t = before.threshold_s;
    let mut m = String::new();
    m.push_str("| Heuristic | Without Ghost Layer | With Ghost Layer (direct) | With Ghost Layer + Nym (testnet) |\n|---|---|---|---|\n");
    m.push_str(&format!("| Same-connection | {} | {} | — |\n", frac(before, |s| s.same_connection), frac(after, |s| s.same_connection)));
    m.push_str(&format!("| Sync-then-send ≤ {t:.0} s | {} | {} | — |\n", frac(before, |s| s.sync_then_send), frac(after, |s| s.sync_then_send)));
    m.push_str(&format!(
        "| Range fingerprint (sessions chained) | {} of {} sessions | {} of {} sessions | — |\n",
        before.fingerprint_linked_sessions, before.sync_sessions, after.fingerprint_linked_sessions, after.sync_sessions
    ));
    m.push_str(&format!(
        "| Same IP | {} (local run: one source IP) | {} (expected still linked without Nym) | {} |\n",
        frac(before, |s| s.same_ip),
        frac(after, |s| s.same_ip),
        nym_note.unwrap_or("see `ghost ip --nym`")
    ));
    m.push_str(&format!("| Median tip→broadcast time | {} | {} | — |\n", secs(before.median_tip_to_send_s), secs(after.median_tip_to_send_s)));
    m.push_str(&format!(
        "| **Overall linkage rate (same-connection or sync-then-send)** | **{:.0} %** | **{:.0} %** | — |\n",
        before.rate(),
        after.rate()
    ));
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(method: &str, phase: &str, ts_s: f64, session: &str, meta: serde_json::Value) -> String {
        serde_json::json!({"method": method, "phase": phase, "timestamp_ns": (ts_s * 1e9) as u64, "session": session, "metadata": meta})
            .to_string()
    }

    #[test]
    fn links_sync_then_send_on_same_connection_but_not_split_delayed() {
        let dir = std::env::temp_dir().join(format!("ghost-attack-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lines = [
            // before-style: wallet A syncs to tip 100 and sends 2 s later on the same connection
            ev("GetLatestBlock", "response", 0.0, "A", serde_json::json!({"height": 100})),
            ev("GetBlockRange", "request", 0.1, "A", serde_json::json!({"start_height": 50, "end_height": 100})),
            ev("GetBlockRange", "response", 1.0, "A", serde_json::json!({"height": 100})),
            ev("SendTransaction", "request", 3.0, "A", serde_json::json!({"payload_size": 10, "payload_sha256": "aa"})),
            // after-style: send on a fresh connection C, 30 s after the tip
            ev("SendTransaction", "request", 31.0, "C", serde_json::json!({"payload_size": 10, "payload_sha256": "bb"})),
            // second session resumes exactly at A's end + 1 -> chained
            ev("GetBlockRange", "request", 40.0, "B", serde_json::json!({"start_height": 101, "end_height": 120})),
        ];
        let f = dir.join("events.jsonl");
        std::fs::write(&f, lines.join("\n")).unwrap();
        let a = analyze(&f, 10.0, false).unwrap();
        assert_eq!(a.sends.len(), 2);
        assert!(a.sends[0].same_connection && a.sends[0].sync_then_send);
        assert!(!a.sends[1].same_connection && !a.sends[1].sync_then_send);
        assert_eq!(a.fingerprint_linked_sessions, 1);
        assert_eq!(a.rate(), 50.0);
        let _ = std::fs::remove_dir_all(dir);
    }
}

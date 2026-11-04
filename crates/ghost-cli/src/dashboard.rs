//! `ghost dashboard`: a live "what the server sees" page on 127.0.0.1:9070 (loopback only).
//!
//! Every value comes from real local state, read fresh on each request:
//! the broadcast queue (sqlite), Ghost Layer's upstream event log (what the server observed),
//! the ghost-attacker heuristics run on that log, exit IPs measured through the Nym clients,
//! and the committed before/after runs in `evidence/`. Missing sources are sent as null and the
//! page shows "no data yet". The page itself is embedded in the binary; no external requests.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ghost_core::broadcast::{Queue, now_ms};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::{Fail, NymArgs, data_dir, explorer_links, mask_ip, nym_exit};

const PAGE: &str = include_str!("dashboard.html");

#[derive(clap::Args)]
pub(crate) struct DashArgs {
    /// Listen address (loopback only)
    #[arg(long, default_value = "127.0.0.1:9070")]
    listen: SocketAddr,
    /// Ghost Layer upstream event log to analyse (default ~/.ghost-layer/upstream-events.jsonl)
    #[arg(long)]
    events: Option<PathBuf>,
    /// Evidence directory with shadecheck/{before,after}/events.jsonl for the before/after panel
    #[arg(long, default_value = "evidence")]
    evidence: PathBuf,
    /// Sync-then-send threshold for the attacker view (seconds)
    #[arg(long, default_value_t = 10.0)]
    threshold: f64,
    #[command(flatten)]
    nym: NymArgs,
}

/// Measures what the server sees on each path every 60 s (real HTTPS probes, as `ghost ip`).
async fn measure_loop(nym: NymArgs, slot: Arc<Mutex<Value>>) {
    loop {
        let direct = ghost_transport::public_ip(None).await;
        let up = |a: SocketAddr| async move { tokio::net::TcpStream::connect(a).await.is_ok() };
        let use_nym = nym.nym || (up(nym.nym_sync).await && up(nym.nym_broadcast).await);
        let (sync, bcast) = if use_nym {
            (ghost_transport::public_ip(Some(nym.nym_sync)).await, ghost_transport::public_ip(Some(nym.nym_broadcast)).await)
        } else {
            (direct.as_ref().map(|s| mask_ip(s)).map_err(|e| anyhow::anyhow!("{e:#}")),
             direct.as_ref().map(|s| mask_ip(s)).map_err(|e| anyhow::anyhow!("{e:#}")))
        };
        let err = [&sync, &bcast].iter().find_map(|r| r.as_ref().err().map(|e| format!("{e:#}")));
        let v = json!({
            "nym": use_nym,
            "your_ip_masked": direct.as_ref().ok().map(|s| mask_ip(s)),
            "sync_ip": sync.as_ref().ok(), "broadcast_ip": bcast.as_ref().ok(),
            "sync_exit": if use_nym { nym_exit("ghost-sync") } else { None },
            "broadcast_exit": if use_nym { nym_exit("ghost-bcast") } else { None },
            "unlinkable": use_nym && matches!((&sync, &bcast), (Ok(a), Ok(b)) if a != b),
            "measured_at_ms": now_ms(),
            "error": err,
        });
        *slot.lock().unwrap() = v;
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

fn state(a: &DashArgs, events: &PathBuf, server_sees: &Value) -> Value {
    let queue = Queue::open(&data_dir().join("ghost.sqlite")).ok().and_then(|q| q.list().ok()).map(|rows| {
        rows.into_iter().rev().take(50).map(|r| json!({
            "txid": r.txid, "status": r.status, "mode": r.mode,
            "received_at_ms": r.received_at_ms, "scheduled_at_ms": r.scheduled_at_ms, "sent_at_ms": r.sent_at_ms,
            "conn_id": r.conn_id, "transport": r.transport, "endpoint": r.endpoint,
            // On success lightwalletd puts the txid in the message field: only show real errors.
            "error_message": if r.status == "accepted" { None } else {
                r.error_message.map(|m| m.chars().filter(|c| !c.is_control()).take(300).collect::<String>()) },
            "explorer": explorer_links(&r.txid),
        })).collect::<Vec<_>>()
    });

    // Grid: the GetBlockRange requests the server actually received.
    let grid = std::fs::read_to_string(events).ok().map(|text| {
        let ranges: Vec<Value> = text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|e| e["phase"] == "request" && e["method"].as_str().is_some_and(|m| m.starts_with("GetBlockRange")))
            .map(|e| json!({"start": e["metadata"]["start_height"], "end": e["metadata"]["end_height"],
                "aligned": e["metadata"]["aligned"].as_bool().unwrap_or(false),
                "conn": e["session"], "ts_ms": e["timestamp_ns"].as_u64().unwrap_or(0) / 1_000_000}))
            .collect();
        let aligned = ranges.iter().filter(|r| r["aligned"] == true).count();
        let n = ranges.len();
        json!({"ranges": ranges.into_iter().rev().take(10).collect::<Vec<_>>(), "aligned": aligned, "unaligned": n - aligned})
    });

    let attacker = ghost_attacker::analyze(events, a.threshold, false).ok().map(|an| json!({
        "sends": an.sends, "rate": if an.sends.is_empty() { 0.0 } else { an.rate() / 100.0 },
        "sync_sessions": an.sync_sessions, "fingerprint_linked_sessions": an.fingerprint_linked_sessions, "threshold_s": an.threshold_s,
    }));

    let ev = |r: &str| a.evidence.join("shadecheck").join(r).join("events.jsonl");
    let before_after = match (ghost_attacker::analyze(&ev("before"), a.threshold, true), ghost_attacker::analyze(&ev("after"), a.threshold, true)) {
        (Ok(b), Ok(f)) => json!({"before_rate": b.rate(), "after_rate": f.rate(), "before_n": b.sends.len(), "after_n": f.sends.len(),
            "source": format!("{} (real local runs)", a.evidence.join("shadecheck").display())}),
        _ => Value::Null,
    };

    json!({
        "schema_version": 1,
        "now_ms": now_ms(),
        "server_sees": if server_sees.is_null() { Value::Null } else { server_sees.clone() },
        "queue": queue,
        "grid": grid,
        "attacker": attacker,
        "before_after": before_after,
    })
}

async fn respond(mut sock: tokio::net::TcpStream, a: Arc<DashArgs>, events: Arc<PathBuf>, seen: Arc<Mutex<Value>>) {
    let mut buf = [0u8; 4096];
    let Ok(Ok(n)) = tokio::time::timeout(Duration::from_secs(5), sock.read(&mut buf)).await else { return };
    let req = String::from_utf8_lossy(&buf[..n]);
    let line = req.lines().next().unwrap_or_default();
    let (status, ctype, body) = match line.split_whitespace().take(2).collect::<Vec<_>>().as_slice() {
        ["GET", "/"] | ["GET", "/index.html"] => ("200 OK", "text/html; charset=utf-8", PAGE.to_string()),
        ["GET", "/api/state"] => {
            let s = seen.lock().unwrap().clone();
            ("200 OK", "application/json", state(&a, &events, &s).to_string())
        }
        ["GET", _] => ("404 Not Found", "text/plain", "not found".into()),
        _ => ("405 Method Not Allowed", "text/plain", "GET only".into()),
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\
         Content-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; img-src 'self' data:\r\n\
         X-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = sock.write_all(head.as_bytes()).await;
    let _ = sock.write_all(body.as_bytes()).await;
}

pub(crate) async fn dashboard(a: DashArgs, json_out: bool) -> Result<u8, Fail> {
    if !a.listen.ip().is_loopback() {
        return Err(Fail::usage("the dashboard is loopback-only", "use --listen 127.0.0.1:<port>"));
    }
    let listener = TcpListener::bind(a.listen).await
        .map_err(|e| Fail::runtime(format!("cannot listen on {}: {e}", a.listen), "is another dashboard running? try --listen 127.0.0.1:9071"))?;
    let events = Arc::new(a.events.clone().unwrap_or_else(|| data_dir().join("upstream-events.jsonl")));
    let seen = Arc::new(Mutex::new(Value::Null));
    tokio::spawn(measure_loop(a.nym.clone(), seen.clone()));
    if json_out {
        println!("{}", json!({"schema_version": 1, "ok": true, "url": format!("http://{}/", a.listen), "events": events.as_ref()}));
    } else {
        println!("👻 Ghost Layer dashboard: http://{}/  (loopback only; Ctrl-C to stop)", a.listen);
        println!("   reading: queue ~/.ghost-layer/ghost.sqlite · upstream events {} · evidence {}", events.display(), a.evidence.display());
    }
    let a = Arc::new(a);
    loop {
        let Ok((sock, peer)) = listener.accept().await else { continue };
        if !peer.ip().is_loopback() {
            continue;
        }
        tokio::spawn(respond(sock, a.clone(), events.clone(), seen.clone()));
    }
}

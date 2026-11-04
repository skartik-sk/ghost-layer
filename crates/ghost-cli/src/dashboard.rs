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

/// A Pay/Proof action: runs `ghost --json <cmd>` (the same code path as the CLI) and streams its
/// progress lines (stderr) to the page.
#[derive(Clone, Default, serde::Serialize)]
struct Job {
    kind: String,
    started_ms: i64,
    status: String,
    progress: Vec<String>,
    result: Option<Value>,
    exit_code: Option<i32>,
}

type Jobs = Arc<Mutex<(u64, std::collections::HashMap<u64, Job>)>>;

fn start_job(jobs: &Jobs, kind: &str, args: Vec<String>) -> u64 {
    let id = {
        let mut j = jobs.lock().unwrap();
        j.0 += 1;
        let id = j.0;
        j.1.insert(id, Job { kind: kind.into(), started_ms: now_ms(), status: "running".into(), ..Default::default() });
        id
    };
    let jobs = jobs.clone();
    tokio::spawn(async move {
        use tokio::io::AsyncBufReadExt;
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ghost"));
        let child = tokio::process::Command::new(exe).arg("--json").args(&args)
            .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
            .kill_on_drop(true).spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                if let Some(j) = jobs.lock().unwrap().1.get_mut(&id) {
                    j.status = "done".into();
                    j.result = Some(json!({"ok": false, "error": e.to_string()}));
                }
                return;
            }
        };
        let mut err = tokio::io::BufReader::new(child.stderr.take().expect("piped")).lines();
        let mut out = child.stdout.take().expect("piped");
        let reader = tokio::spawn(async move {
            let mut s = String::new();
            let _ = out.read_to_string(&mut s).await;
            s
        });
        while let Ok(Some(line)) = err.next_line().await {
            let clean: String = line.chars().filter(|c| !c.is_control()).take(300).collect();
            if let Some(j) = jobs.lock().unwrap().1.get_mut(&id) {
                j.progress.push(clean);
            }
        }
        let code = child.wait().await.ok().and_then(|s| s.code());
        let stdout = reader.await.unwrap_or_default();
        let result = stdout.lines().last().and_then(|l| serde_json::from_str::<Value>(l).ok());
        if let Some(j) = jobs.lock().unwrap().1.get_mut(&id) {
            j.status = "done".into();
            j.exit_code = code;
            j.result = result;
        }
    });
    id
}

/// Validates a Pay/Proof request from the page and turns it into `ghost` arguments.
fn job_args(a: &DashArgs, path: &str, body: &Value) -> Result<(String, Vec<String>), String> {
    let field = |k: &str| body.get(k).and_then(|v| v.as_str()).map(str::trim).unwrap_or("").to_string();
    let mut args: Vec<String> = Vec::new();
    match path {
        "/api/pay" => {
            let (Some(w), Some(i)) = (&a.wallet, &a.identity) else {
                return Err("Pay is disabled: start `ghost dashboard --wallet <dir> --identity <age file>`".into());
            };
            let to = field("to");
            if !(to.starts_with("utest1") || to.starts_with("ztestsapling1") || to.starts_with("tm")) || to.len() > 600 {
                return Err("recipient must be a testnet address (utest1…, ztestsapling1…, tm…)".into());
            }
            let amount = field("amount");
            let zat = crate::pay::taz_to_zat(&amount).map_err(|f| f.msg)?;
            let cap = crate::pay::taz_to_zat(&a.max_amount).map_err(|f| f.msg)?;
            if zat > cap {
                return Err(format!("amount above this dashboard's cap of {} TAZ", a.max_amount));
            }
            let memo = field("memo");
            if memo.len() > 512 || memo.chars().any(char::is_control) {
                return Err("memo: up to 512 bytes of plain text".into());
            }
            args.extend(["pay".into(), "--wallet".into(), w.display().to_string(), "--identity".into(), i.display().to_string(),
                "--to".into(), to, "--amount".into(), amount]);
            if !memo.is_empty() {
                args.extend(["--memo".into(), memo]);
            }
            if let Some(d) = &a.devtool {
                args.extend(["--devtool".into(), d.display().to_string()]);
            }
        }
        "/api/proof/create" => {
            let Some(u) = &a.ufvk_file else {
                return Err("Proof creation is disabled: start `ghost dashboard --ufvk-file <sender UFVK file>`".into());
            };
            let txid = field("txid").to_ascii_lowercase();
            if txid.len() != 64 || !txid.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err("txid must be 64 hex characters".into());
            }
            let msg = field("msg");
            if msg.len() > 256 || msg.chars().any(char::is_control) {
                return Err("message: up to 256 bytes of plain text".into());
            }
            args.extend(["proof".into(), "create".into(), "--ufvk-file".into(), u.display().to_string(), "--txid".into(), txid, "--msg".into(), msg]);
            if let Some(i) = body.get("action").and_then(|v| v.as_u64()) {
                args.extend(["--action".into(), i.to_string()]);
            }
        }
        "/api/proof/verify" => {
            let p = field("proof");
            if !p.starts_with("zecproof1") || p.len() > 4000 || !p[9..].chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                return Err("paste a zecproof1… string".into());
            }
            args.extend(["proof".into(), "verify".into(), p]);
        }
        _ => return Err("not found".into()),
    }
    if !a.nym.on() {
        args.push("--direct".into());
    }
    let kind = path.trim_start_matches("/api/").replace('/', "-");
    Ok((kind, args))
}

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
    /// Ghost Wallet "Pay" tab: zcash-devtool wallet to pay from (fixed here; the page can't choose it)
    #[arg(long)]
    wallet: Option<PathBuf>,
    /// Its age identity file (path only, passed to zcash-devtool)
    #[arg(long)]
    identity: Option<PathBuf>,
    /// "Proof" tab: the paying wallet's UFVK file, to create ZecProofs for its payments
    #[arg(long)]
    ufvk_file: Option<PathBuf>,
    /// zcash-devtool binary for the Pay tab
    #[arg(long)]
    devtool: Option<PathBuf>,
    /// Max TAZ per payment from the Pay tab
    #[arg(long, default_value = "0.01")]
    max_amount: String,
    #[command(flatten)]
    nym: NymArgs,
}

/// Measures what the server sees on each path every 60 s (real HTTPS probes, as `ghost ip`).
async fn measure_loop(nym: NymArgs, slot: Arc<Mutex<Value>>) {
    loop {
        let direct = ghost_transport::public_ip(None).await;
        let up = |a: SocketAddr| async move { tokio::net::TcpStream::connect(a).await.is_ok() };
        let use_nym = nym.on();
        let _ = up;
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
        "app": {
            "pay_enabled": a.wallet.is_some() && a.identity.is_some(),
            "proof_create_enabled": a.ufvk_file.is_some(),
            "max_amount_taz": a.max_amount,
            "nym": a.nym.on(),
        },
        "server_sees": if server_sees.is_null() { Value::Null } else { server_sees.clone() },
        "queue": queue,
        "grid": grid,
        "attacker": attacker,
        "before_after": before_after,
    })
}

/// Reads one HTTP request (headers + Content-Length body, max 16 KiB).
async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<(String, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = tokio::time::timeout(Duration::from_secs(5), sock.read(&mut chunk)).await.ok()?.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > 16 * 1024 {
            return None;
        }
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..pos]).to_string();
            let len = head.lines().find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.trim().parse::<usize>().ok())).unwrap_or(0);
            if len > 16 * 1024 {
                return None;
            }
            while buf.len() < pos + 4 + len {
                let n = tokio::time::timeout(Duration::from_secs(5), sock.read(&mut chunk)).await.ok()?.ok()?;
                if n == 0 {
                    return None;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            return Some((head, buf[pos + 4..pos + 4 + len].to_vec()));
        }
    }
}

async fn respond(mut sock: tokio::net::TcpStream, a: Arc<DashArgs>, events: Arc<PathBuf>, seen: Arc<Mutex<Value>>, jobs: Jobs, token: Arc<String>) {
    let Some((req, body_in)) = read_request(&mut sock).await else { return };
    let line = req.lines().next().unwrap_or_default();
    // DNS-rebinding guard: only answer requests addressed to the loopback name we listen on.
    let port = a.listen.port();
    let host_ok = req.lines().skip(1).find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("host")).map(|(_, v)| v.trim().to_string()))
        .is_some_and(|h| [format!("127.0.0.1:{port}"), format!("localhost:{port}"), format!("[::1]:{port}")].contains(&h));
    let parts: Vec<&str> = line.split_whitespace().take(2).collect();
    // Pay/Proof/job endpoints need the per-run token that `ghost dashboard` printed to YOUR terminal,
    // so other local processes (or users) can't drive payments; the read-only Live view stays open.
    let hdr = |name: &str| req.lines().skip(1).find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.trim().to_string()));
    let path = parts.get(1).copied().unwrap_or_default();
    let needs_token = path.starts_with("/api/job/") || path.starts_with("/api/pay") || path.starts_with("/api/proof/");
    let token_ok = hdr("x-ghost-token").is_some_and(|t| t.len() == token.len() && t.bytes().zip(token.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0);
    let (status, ctype, body) = if !host_ok {
        ("421 Misdirected Request", "text/plain", "bad Host header".to_string())
    } else if needs_token && !token_ok {
        ("401 Unauthorized", "application/json", json!({"error": "missing or wrong token: open the URL that `ghost dashboard` printed"}).to_string())
    } else { match parts.as_slice() {
        ["GET", p] if *p == "/" || *p == "/index.html" || p.starts_with("/?") => ("200 OK", "text/html; charset=utf-8", PAGE.to_string()),
        ["GET", "/api/state"] => {
            let s = seen.lock().unwrap().clone();
            ("200 OK", "application/json", state(&a, &events, &s).to_string())
        }
        ["GET", p] if p.starts_with("/api/job/") => {
            let job = p["/api/job/".len()..].parse::<u64>().ok().and_then(|id| jobs.lock().unwrap().1.get(&id).cloned());
            match job {
                Some(j) => ("200 OK", "application/json", serde_json::to_string(&j).unwrap_or_default()),
                None => ("404 Not Found", "application/json", json!({"error": "no such job"}).to_string()),
            }
        }
        ["POST", p @ ("/api/pay" | "/api/proof/create" | "/api/proof/verify")] => {
            // CSRF guard: a custom header + JSON content type can't be sent cross-site without a CORS
            // preflight, which this server never approves.
            let header = |name: &str| req.lines().skip(1).find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.trim().to_string()));
            let body: Value = serde_json::from_slice(&body_in).unwrap_or(Value::Null);
            if header("x-ghost").as_deref() != Some("1") || !header("content-type").is_some_and(|c| c.starts_with("application/json")) {
                ("403 Forbidden", "application/json", json!({"error": "missing X-Ghost header"}).to_string())
            } else if *p == "/api/pay" && jobs.lock().unwrap().1.values().any(|j| j.kind == "pay" && j.status == "running") {
                ("409 Conflict", "application/json", json!({"error": "a payment is already in progress"}).to_string())
            } else {
                match job_args(&a, p, &body) {
                    Ok((kind, args)) => {
                        let id = start_job(&jobs, &kind, args);
                        ("200 OK", "application/json", json!({"job": id}).to_string())
                    }
                    Err(e) => ("400 Bad Request", "application/json", json!({"error": e}).to_string()),
                }
            }
        }
        ["GET", _] | ["POST", _] => ("404 Not Found", "text/plain", "not found".into()),
        _ => ("405 Method Not Allowed", "text/plain", "GET/POST only".into()),
    } };
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
    let jobs: Jobs = Arc::new(Mutex::new((0, Default::default())));
    let token = Arc::new({
        use rand::RngExt;
        let b: [u8; 16] = rand::rng().random();
        hex::encode(b)
    });
    tokio::spawn(measure_loop(a.nym.clone(), seen.clone()));
    if json_out {
        println!("{}", json!({"schema_version": 1, "ok": true, "url": format!("http://{}/?t={}", a.listen, token), "events": events.as_ref()}));
    } else {
        println!("👻 Ghost Wallet: http://{}/?t={}  (Pay · Proof · Live; loopback only; Ctrl-C to stop)", a.listen, token);
        println!("   the ?t= token authorises Pay/Proof for this run only; don't share it");
        println!("   pay: {}  ·  proof create: {}  ·  cap {} TAZ  ·  {}",
            if a.wallet.is_some() && a.identity.is_some() { "enabled" } else { "disabled (--wallet/--identity)" },
            if a.ufvk_file.is_some() { "enabled" } else { "disabled (--ufvk-file)" }, a.max_amount,
            if a.nym.on() { "Nym" } else { "--direct" });
        println!("   reading: queue ~/.ghost-layer/ghost.sqlite · upstream events {} · evidence {}", events.display(), a.evidence.display());
    }
    let a = Arc::new(a);
    loop {
        let Ok((sock, peer)) = listener.accept().await else { continue };
        if !peer.ip().is_loopback() {
            continue;
        }
        tokio::spawn(respond(sock, a.clone(), events.clone(), seen.clone(), jobs.clone(), token.clone()));
    }
}

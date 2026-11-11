//! `ghost pay`: the whole private-payment flow in one command.
//!
//! 1. use the running gateway on 127.0.0.1:9067, or start one (`ghost up [--nym]`) for this payment
//! 2. sync the wallet through it   (zcash-devtool, as a subprocess)
//! 3. send through it              (zcash-devtool; Ghost queues it, waits a random delay, broadcasts
//!    on a fresh connection) — with a live countdown from the real queue
//! 4. poll until mined, print explorer links
//!
//! Keys stay in the wallet: Ghost only passes the identity *path* to zcash-devtool and never reads it.

use std::io::{IsTerminal, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ghost_core::broadcast::{Queue, now_ms};
use ghost_transport::proto::ChainSpec;
use ghost_transport::{Transport, Upstream};
use serde_json::json;

use crate::{Fail, NymArgs, data_dir, explorer_links, tx_state};

#[derive(clap::Args)]
pub(crate) struct PayArgs {
    /// zcash-devtool wallet directory (default: the one `ghost init` created)
    #[arg(long)]
    wallet: Option<PathBuf>,
    /// age identity file of that wallet (only its path is passed to zcash-devtool; Ghost never reads it)
    #[arg(long)]
    identity: Option<PathBuf>,
    /// Recipient address (utest1…, ztestsapling…, tm…)
    #[arg(long)]
    to: String,
    /// Amount in TAZ, e.g. 0.001
    #[arg(long)]
    amount: String,
    /// Optional memo (shielded recipients only)
    #[arg(long)]
    memo: Option<String>,
    /// Upstream lightwalletd server (used only if Ghost has to start the gateway)
    #[arg(long, default_value = crate::DEFAULT_SERVER)]
    server: String,
    #[arg(long, default_value = crate::DEFAULT_LISTEN)]
    listen: SocketAddr,
    /// Broadcast delay profile if Ghost starts the gateway
    #[arg(long, default_value = "demo", value_parser = ["demo", "privacy"])]
    profile: String,
    /// zcash-devtool binary (default: $ZCASH_DEVTOOL, the config, next to ghost, or on PATH)
    #[arg(long)]
    devtool: Option<PathBuf>,
    /// Give up waiting for the block after this many seconds
    #[arg(long, default_value_t = 900)]
    mine_timeout: u64,
    #[command(flatten)]
    nym: NymArgs,
}

/// "0.001" → 100000 zatoshis, exactly (no floats).
pub(crate) fn taz_to_zat(s: &str) -> Result<u64, Fail> {
    let bad = || Fail::usage(format!("bad amount '{s}'"), "use TAZ with up to 8 decimals, e.g. --amount 0.001");
    let (int, frac) = s.trim().split_once('.').unwrap_or((s.trim(), ""));
    if frac.len() > 8 || (int.is_empty() && frac.is_empty()) || !int.chars().chain(frac.chars()).all(|c| c.is_ascii_digit()) {
        return Err(bad());
    }
    let int: u64 = if int.is_empty() { 0 } else { int.parse().map_err(|_| bad())? };
    let frac: u64 = format!("{frac:0<8}").parse().map_err(|_| bad())?;
    let zat = int.checked_mul(100_000_000).and_then(|v| v.checked_add(frac)).ok_or_else(bad)?;
    if zat == 0 {
        return Err(Fail::usage("amount must be > 0", "e.g. --amount 0.001"));
    }
    Ok(zat)
}

struct Ui {
    json: bool,
    tty: bool,
}

impl Ui {
    fn step(&self, msg: &str) {
        if self.json {
            eprintln!("{msg}");
        } else {
            println!("{msg}");
        }
    }
}

async fn gateway_alive(listen: SocketAddr) -> Option<u64> {
    let up = Upstream::parse(&listen.to_string()).ok()?;
    let mut c = Transport::direct(up, "local").fresh().await.ok()?.client;
    Some(c.get_latest_block(ChainSpec {}).await.ok()?.into_inner().height)
}

/// Stops the gateway we started (never one the user started).
pub(crate) struct Started(Option<Child>);
impl Drop for Started {
    fn drop(&mut self) {
        if let Some(c) = self.0.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// Use the gateway already listening on `listen`, or start `ghost up` for as long as the returned guard lives.
pub(crate) async fn ensure_gateway(listen: SocketAddr, upstream: &str, profile: &str, nym: &NymArgs, say: &dyn Fn(&str))
    -> Result<(Started, &'static str), Fail> {
    if let Some(tip) = gateway_alive(listen).await {
        say(&format!("👻 using the running Ghost Layer on {listen} (tip {tip})"));
        return Ok((Started(None), "existing"));
    }
    nym.require_up().await?;
    let log = data_dir().join("pay-gateway.log");
    let _ = std::fs::create_dir_all(data_dir());
    let listen_s = listen.to_string();
    let mut cmd = Command::new(std::env::current_exe().map_err(|e| Fail::runtime(e.to_string(), "reinstall ghost"))?);
    cmd.args(["up", "--server", upstream, "--listen", &listen_s, "--profile", profile]);
    if !nym.on() {
        cmd.arg("--direct");
    } else {
        cmd.args(["--nym-sync", &nym.nym_sync.to_string(), "--nym-broadcast", &nym.nym_broadcast.to_string(), "--tor", &nym.tor.to_string()]);
        if nym.no_tor {
            cmd.arg("--no-tor");
        }
    }
    let errf = std::fs::File::create(&log).map_err(|e| Fail::runtime(format!("{}: {e}", log.display()), "check ~/.ghost-layer"))?;
    let child = cmd.stdout(Stdio::null()).stderr(errf).spawn().map_err(|e| Fail::runtime(e.to_string(), "run `ghost up` yourself"))?;
    let started = Started(Some(child));
    say(&format!("👻 starting Ghost Layer{} on {listen} …", if nym.on() { " (Nym)" } else { "" }));
    for _ in 0..240 {
        if gateway_alive(listen).await.is_some() {
            return Ok((started, "started"));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(Fail::runtime(format!("gateway did not start in 240 s (log: {})", log.display()), "run `ghost doctor` / check your Nym clients"))
}

pub(crate) fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\u{1b}' {
            for d in it.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
        } else if !c.is_control() || c == '\n' {
            out.push(c);
        }
    }
    out
}

pub(crate) async fn pay(a: PayArgs, json: bool) -> Result<u8, Fail> {
    let ui = Ui { json, tty: std::io::stdout().is_terminal() && !json };
    let zat = taz_to_zat(&a.amount)?;
    if !(a.to.starts_with("utest1") || a.to.starts_with("ztestsapling1") || a.to.starts_with("tm")) {
        return Err(Fail::usage("recipient must be a testnet address (utest1…, ztestsapling1…, tm…)", "Ghost Layer pay is testnet-only"));
    }
    let Some((wallet, identity)) = crate::setup::wallet_paths(a.wallet.clone(), a.identity.clone()) else {
        return Err(Fail::usage("no wallet given and none set up", "run `ghost init` once, or pass --wallet <dir> --identity <age file>"));
    };
    if !wallet.join("data.sqlite").exists() {
        return Err(Fail::usage(format!("{} is not a zcash-devtool wallet", wallet.display()), "run `ghost init` to create one"));
    }
    if !identity.exists() {
        return Err(Fail::usage(format!("identity file {} not found", identity.display()), "pass the age identity used at `wallet init -i`"));
    }
    let dt = crate::setup::devtool(a.devtool.as_ref());
    crate::setup::check_devtool(&dt)?;
    let server = a.listen.to_string();
    let t0 = Instant::now();

    // 1. gateway
    let (_started, gw_mode) = ensure_gateway(a.listen, &a.server, &a.profile, &a.nym, &|m| ui.step(m)).await?;

    // 2. sync through the gateway
    ui.step("🔄 syncing wallet through Ghost Layer (grid-aligned) …");
    // A sync interrupted by a dropped mixnet connection resumes where it stopped: retry it.
    for attempt in 1..=3 {
        let out = Command::new(&dt)
            .args(["wallet", "-w"]).arg(&wallet).args(["sync", "-s", &server])
            .stdin(Stdio::null()).output()
            .map_err(|e| Fail::runtime(e.to_string(), "check --devtool"))?;
        if out.status.success() {
            break;
        }
        let err = strip_ansi(&String::from_utf8_lossy(&out.stderr));
        let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").to_string();
        if attempt == 3 || !last.contains("transport") {
            return Err(Fail::runtime(format!("wallet sync failed: {last}"), "run `ghost doctor`; is the wallet for testnet?"));
        }
        ui.step(&format!("   ↻ sync interrupted ({last}); resuming ({attempt}/2) …"));
    }
    ui.step("✅ synced");

    // 3. send, with a live countdown from Ghost's real broadcast queue
    let mut cmd = Command::new(&dt);
    cmd.args(["wallet", "-w"]).arg(&wallet).args(["send", "-i"]).arg(&identity)
        .args(["--address", &a.to, "--value", &zat.to_string(), "-s", &server]);
    if let Some(m) = &a.memo {
        cmd.args(["--memo", m]);
    }
    ui.step(&format!("💸 sending {} TAZ — Ghost holds it for a random delay, then broadcasts on a fresh connection …", a.amount));
    let send_started = now_ms();
    let child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()
        .map_err(|e| Fail::runtime(e.to_string(), "check --devtool"))?;
    let waiter = tokio::task::spawn_blocking(move || child.wait_with_output());
    let queue = Queue::open(&data_dir().join("ghost.sqlite")).ok();
    let mut seen: Option<(String, i64, i64)> = None; // txid, received, scheduled
    while !waiter.is_finished() {
        if let Some(q) = &queue {
            if let Ok(rows) = q.list() {
                if let Some(r) = rows.iter().rev().find(|r| r.received_at_ms >= send_started - 5_000) {
                    if seen.is_none() {
                        ui.step(&format!("   ⏳ queued {}… random delay {:.1} s", &r.txid[..12], (r.scheduled_at_ms - r.received_at_ms) as f64 / 1000.0));
                    }
                    seen = Some((r.txid.clone(), r.received_at_ms, r.scheduled_at_ms));
                    if ui.tty && r.status == "pending" {
                        let left = (r.scheduled_at_ms - now_ms()) as f64 / 1000.0;
                        let msg = if left > 0.0 { format!("broadcast in {left:5.1} s") } else { "broadcasting on a fresh connection…".into() };
                        print!("\r   {msg}        ");
                        let _ = std::io::stdout().flush();
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    if ui.tty {
        println!();
    }
    let out = waiter.await.map_err(|e| Fail::runtime(e.to_string(), "retry"))?.map_err(|e| Fail::runtime(e.to_string(), "retry"))?;
    let stdout = strip_ansi(&String::from_utf8_lossy(&out.stdout));
    let stderr = strip_ansi(&String::from_utf8_lossy(&out.stderr));
    if !out.status.success() {
        let line = stderr.lines().rev().find(|l| l.starts_with("Error")).unwrap_or("send failed").to_string();
        let hint = if line.contains("Insufficient balance") {
            "not enough spendable funds: zcash-devtool needs ~10 confirmations on received/change notes (~4 min); wait and retry"
        } else if line.contains("transport error") || line.contains("unavailable") {
            "Ghost could not reach the network through Nym, so nothing was sent and your IP was not exposed; retry in a few minutes (`ghost status` shows the tx)"
        } else if line.contains("SendFailed") || line.contains("code:") {
            "the network rejected the transaction; see `ghost status`"
        } else {
            "see `ghost status` and the wallet output above"
        };
        let code = if line.contains("SendFailed") { 3 } else { 1 };
        return Err(Fail { code, msg: line, hint });
    }
    let txid = stdout.lines().map(str::trim).find(|l| l.len() == 64 && l.chars().all(|c| c.is_ascii_hexdigit()))
        .ok_or_else(|| Fail::runtime("no txid in wallet output", "see `ghost status`"))?.to_string();
    let row = queue.as_ref().and_then(|q| q.list().ok()).and_then(|rows| rows.into_iter().find(|r| r.txid == txid));
    if let Some(r) = &row {
        ui.step(&format!("✅ broadcast accepted after a {:.1} s random delay — fresh connection #{} via {}",
            (r.scheduled_at_ms - r.received_at_ms) as f64 / 1000.0, r.conn_id.unwrap_or_default(), r.transport.clone().unwrap_or_default()));
    }
    ui.step(&format!("   txid {txid}"));

    // 4. wait until mined (lookup via the broadcast path, see `ghost verify`)
    ui.step("⛏  waiting for the next block (~25–75 s) …");
    let deadline = Instant::now() + Duration::from_secs(a.mine_timeout);
    let st = loop {
        match tx_state(&txid, &a.server, &a.nym).await {
            Ok(s) if s.state == "mined" => break s,
            Ok(_) | Err(_) if Instant::now() < deadline => tokio::time::sleep(Duration::from_secs(10)).await,
            Ok(s) => return Err(Fail::runtime(format!("not mined after {} s (state: {})", a.mine_timeout, s.state), "check later with `ghost verify <txid>`")),
            Err(e) => return Err(e),
        }
    };
    let links = explorer_links(&txid);
    if json {
        println!("{}", json!({"schema_version": 1, "ok": true, "txid": txid, "amount_zat": zat, "to": a.to,
            "gateway": gw_mode, "nym": a.nym.on(), "height": st.height, "confirmations": st.confs,
            "delay_s": row.as_ref().map(|r| (r.scheduled_at_ms - r.received_at_ms) as f64 / 1000.0),
            "broadcast_conn": row.as_ref().and_then(|r| r.conn_id), "transport": row.as_ref().and_then(|r| r.transport.clone()),
            "elapsed_s": t0.elapsed().as_secs_f64(), "explorer": links}));
    } else {
        println!("🎉 mined at height {} ({} confirmation(s)) — total {:.0} s", st.height.unwrap_or_default(), st.confs, t0.elapsed().as_secs_f64());
        for l in &links {
            println!("   🔗 {l}");
        }
    }
    let _ = seen;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::taz_to_zat;

    #[test]
    fn amounts_are_exact() {
        assert_eq!(taz_to_zat("0.001").ok(), Some(100_000));
        assert_eq!(taz_to_zat("1").ok(), Some(100_000_000));
        assert_eq!(taz_to_zat(".5").ok(), Some(50_000_000));
        assert_eq!(taz_to_zat("0.00000001").ok(), Some(1));
        for bad in ["0", "", ".", "1.123456789", "-1", "1e3", "abc", "0.0"] {
            assert!(taz_to_zat(bad).is_err(), "{bad}");
        }
    }
}

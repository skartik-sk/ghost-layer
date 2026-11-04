//! `ghost` — Ghost Layer CLI. Exit codes: 0 ok, 1 runtime failure, 2 usage/config error,
//! 3 upstream rejected tx.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use ghost_core::broadcast::{Policy, Queue, Scheduler};
use ghost_core::grid::BlockCache;
use ghost_gateway::{FreshConnBroadcaster, SendMode};

use clap::{Parser, Subcommand};
use ghost_transport::proto::{ChainSpec, Empty, TxFilter};
use ghost_transport::{Transport, Upstream};
use serde_json::json;

const DEFAULT_SERVER: &str = "testnet.zec.rocks:443";
const DEFAULT_LISTEN: &str = "127.0.0.1:9067";

#[derive(Parser)]
#[command(name = "ghost", version, about = "Ghost Layer: unlink your Zcash payments from your wallet sync")]
struct Cli {
    /// Machine-readable JSON output
    #[arg(long, global = true)]
    json: bool,
    /// More logging (upstream request log)
    #[arg(short, long, global = true)]
    verbose: bool,
    /// Only errors
    #[arg(short, long, global = true)]
    quiet: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start the gateway; point your wallet at 127.0.0.1:9067
    Up(UpArgs),
    /// Check upstream, chain, TLS, port and storage
    /// Broadcast queue: pending/accepted/failed, delays, fresh connection ids, explorer links
    Status,
    /// Is this tx mined? Height, confirmations, explorer links (exit 0 only when mined)
    Verify {
        txid: String,
        #[arg(long, default_value = DEFAULT_SERVER)]
        server: String,
    },
    Doctor {
        #[arg(long, default_value = DEFAULT_SERVER)]
        server: String,
        #[arg(long, default_value = DEFAULT_LISTEN)]
        listen: SocketAddr,
    },
}

#[derive(clap::Args)]
struct UpArgs {
    /// Upstream lightwalletd server used for sync (host:port)
    #[arg(long, default_value = DEFAULT_SERVER)]
    server: String,
    /// Broadcast through a different server than you sync from (destination splitting)
    #[arg(long)]
    broadcast_server: Option<String>,
    /// Listen address for the wallet
    #[arg(long, default_value = DEFAULT_LISTEN)]
    listen: SocketAddr,
    /// Allow a non-loopback listen address
    #[arg(long)]
    allow_remote: bool,
    /// Broadcast delay profile: demo = 10-40 s, privacy = 60-600 s
    #[arg(long, default_value = "demo", value_parser = ["demo", "privacy"])]
    profile: String,
    /// hold: wallet waits for the real broadcast result; queue: answer at once, track in `ghost status`
    #[arg(long, default_value = "hold", value_parser = ["hold", "queue"])]
    send_mode: String,
}

/// Error carrying its exit code and a "what to do next" hint.
struct Fail {
    code: u8,
    msg: String,
    hint: &'static str,
}

impl Fail {
    fn usage(msg: impl Into<String>, hint: &'static str) -> Self {
        Self { code: 2, msg: msg.into(), hint }
    }
    fn runtime(msg: impl Into<String>, hint: &'static str) -> Self {
        Self { code: 1, msg: msg.into(), hint }
    }
}

pub fn data_dir() -> PathBuf {
    std::env::var_os("GHOST_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".ghost-layer"))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let level = if cli.quiet { "error" } else if cli.verbose { "info" } else { "warn,upstream=info,broadcast=info,grid=info" };
    tracing_subscriber::fmt()
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| level.into()))
        .with_writer(std::io::stderr)
        .init();

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let json = cli.json;
    match rt.block_on(run(cli)) {
        Ok(code) => ExitCode::from(code),
        Err(f) => {
            if json {
                println!("{}", json!({"ok": false, "error": f.msg, "hint": f.hint}));
            } else {
                eprintln!("❌ {}\n   → {}", f.msg, f.hint);
            }
            ExitCode::from(f.code)
        }
    }
}

async fn run(cli: Cli) -> Result<u8, Fail> {
    match cli.cmd {
        Cmd::Up(a) => up(a, cli.json).await,
        Cmd::Status => status(cli.json).await,
        Cmd::Verify { txid, server } => verify(&txid, &server, cli.json).await,
        Cmd::Doctor { server, listen } => doctor(&server, listen, cli.json).await,
    }
}

fn parse_upstream(server: &str) -> Result<Upstream, Fail> {
    Upstream::parse(server).map_err(|e| Fail::usage(e.to_string(), "use host:port, e.g. testnet.zec.rocks:443"))
}

fn open_queue() -> Result<Arc<Queue>, Fail> {
    Queue::open(&data_dir().join("ghost.sqlite"))
        .map(Arc::new)
        .map_err(|e| Fail::runtime(format!("cannot open queue db: {e:#}"), "check that GHOST_HOME (~/.ghost-layer) is writable"))
}

async fn up(a: UpArgs, json: bool) -> Result<u8, Fail> {
    let listen = a.listen;
    if !listen.ip().is_loopback() && !a.allow_remote {
        return Err(Fail::usage(
            format!("refusing to listen on non-loopback {listen}"),
            "pass --allow-remote if you really want other machines to use this gateway",
        ));
    }
    let upstream = parse_upstream(&a.server)?;
    let bcast_upstream = match &a.broadcast_server {
        Some(s) => parse_upstream(s)?,
        None => upstream.clone(),
    };
    let policy = Policy::profile(&a.profile).expect("clap restricts profiles");
    let send_mode = if a.send_mode == "queue" { SendMode::Queue } else { SendMode::Hold };

    let sync = Transport::direct(upstream.clone(), "direct");
    let broadcaster = FreshConnBroadcaster { transport: Transport::direct(bcast_upstream.clone(), "direct") };
    let scheduler = Scheduler::new(open_queue()?, policy, broadcaster);
    let cache = BlockCache::open(&data_dir().join("blocks.sqlite"))
        .map_err(|e| Fail::runtime(format!("cannot open block cache: {e:#}"), "check that ~/.ghost-layer is writable"))?;
    let gw = ghost_gateway::Gateway::connect(&sync, scheduler.clone(), send_mode, cache).await.map_err(|e| {
        Fail::runtime(format!("cannot reach upstream {}: {e:#}", upstream.uri()), "run `ghost doctor` to diagnose")
    })?;
    let resumed = scheduler
        .resume()
        .map_err(|e| Fail::runtime(format!("cannot resume queue: {e:#}"), "inspect ~/.ghost-layer/ghost.sqlite"))?;

    if json {
        println!("{}", json!({"ok": true, "listen": listen.to_string(), "sync_upstream": upstream.uri(),
            "broadcast_upstream": bcast_upstream.uri(), "transport": "direct", "profile": a.profile,
            "policy": policy, "send_mode": a.send_mode, "resumed_pending": resumed}));
    } else {
        println!("👻 Ghost Layer up on {listen}");
        println!("   sync      → {} (one long-lived connection)", upstream.uri());
        println!("   broadcast → {} (fresh connection per tx)", bcast_upstream.uri());
        println!("   Point your wallet at {listen} (e.g. zcash-devtool ... -s {listen})");
        println!("   ✅ broadcasts never share the sync connection");
        println!("   ✅ random broadcast delay {}–{} s ({} profile, send-mode {})",
            policy.min_delay.as_secs(), policy.max_delay.as_secs(), a.profile, a.send_mode);
        println!("   ✅ grid-aligned sync: upstream sees only 1152-block cells (nym-swizzle-zcash rule)");
        println!("   ✅ wallet headers stripped");
        println!("   ⚠️  not hidden in direct mode: your IP (same on both paths) — use --nym");
        if resumed > 0 {
            println!("   ↻ resumed {resumed} queued broadcast(s) from the previous run");
        }
    }
    ghost_gateway::serve(gw, listen)
        .await
        .map_err(|e| Fail::runtime(format!("gateway stopped: {e:#}"), "is the port already in use? try `ghost doctor`"))?;
    Ok(0)
}

fn explorer_links(txid: &str) -> Vec<String> {
    vec![
        format!("https://testnet.zcashexplorer.app/transactions/{txid}"),
        format!("https://zexplorer.app/testnet/tx/{txid}"),
    ]
}

fn fmt_ms(ms: i64) -> String {
    let secs = ms / 1000;
    format!("{:02}:{:02}:{:02}Z", (secs / 3600) % 24, (secs / 60) % 60, secs % 60)
}

async fn status(json: bool) -> Result<u8, Fail> {
    let rows = open_queue()?.list().map_err(|e| Fail::runtime(format!("{e:#}"), "inspect ~/.ghost-layer/ghost.sqlite"))?;
    if json {
        let v: Vec<_> = rows
            .iter()
            .map(|r| json!({"row": r, "explorer": explorer_links(&r.txid),
                "delay_s": (r.scheduled_at_ms - r.received_at_ms) as f64 / 1000.0}))
            .collect();
        println!("{}", json!({"ok": true, "broadcasts": v}));
        return Ok(0);
    }
    if rows.is_empty() {
        println!("No broadcasts yet. Send a payment from a wallet pointed at {DEFAULT_LISTEN}.");
    }
    for r in &rows {
        let icon = match r.status.as_str() { "accepted" => "✅", "pending" => "⏳", _ => "❌" };
        println!("{icon} {} {}", r.status, r.txid);
        println!("   received {}  scheduled {} (+{:.1} s random delay)  sent {}",
            fmt_ms(r.received_at_ms), fmt_ms(r.scheduled_at_ms),
            (r.scheduled_at_ms - r.received_at_ms) as f64 / 1000.0,
            r.sent_at_ms.map(fmt_ms).unwrap_or_else(|| "-".into()));
        if let Some(c) = r.conn_id {
            println!("   fresh connection #{c} via {} → {}", r.transport.clone().unwrap_or_default(), r.endpoint.clone().unwrap_or_default());
        }
        if r.status != "accepted" {
            if let Some(m) = &r.error_message {
                println!("   upstream said: {}", m.chars().filter(|c| !c.is_control()).collect::<String>());
            }
        }
        for l in explorer_links(&r.txid) {
            println!("   🔗 {l}");
        }
    }
    Ok(0)
}

async fn verify(txid: &str, server: &str, json: bool) -> Result<u8, Fail> {
    let mut hash = hex::decode(txid).map_err(|_| Fail::usage("txid must be 64 hex chars", "copy the txid exactly as printed"))?;
    if hash.len() != 32 {
        return Err(Fail::usage("txid must be 64 hex chars", "copy the txid exactly as printed"));
    }
    hash.reverse(); // lightwalletd expects the txid in internal (little-endian) byte order
    let upstream = parse_upstream(server)?;
    let mut c = Transport::direct(upstream.clone(), "direct")
        .fresh()
        .await
        .map_err(|e| Fail::runtime(format!("cannot reach {}: {e:#}", upstream.uri()), "run `ghost doctor`"))?
        .client;
    let tip = c.get_latest_block(ChainSpec {}).await.map_err(|e| Fail::runtime(e.to_string(), "run `ghost doctor`"))?.into_inner().height;
    let res = c.get_transaction(TxFilter { block: None, index: 0, hash }).await;
    let (state, height, confs) = match res {
        Ok(t) => match t.into_inner().height {
            0 => ("mempool", None, 0),
            u64::MAX => ("orphaned", None, 0),
            h => ("mined", Some(h), tip.saturating_sub(h) + 1),
        },
        Err(s) if s.code() == tonic::Code::NotFound || s.message().contains("not found") || s.message().contains("No such") => {
            ("not_found", None, 0)
        }
        Err(s) => return Err(Fail::runtime(format!("GetTransaction: {s}"), "retry; the server may be busy")),
    };
    let links = explorer_links(txid);
    if json {
        println!("{}", json!({"ok": state == "mined", "txid": txid, "state": state, "height": height,
            "confirmations": confs, "tip": tip, "explorer": links}));
    } else {
        match state {
            "mined" => println!("✅ mined at height {} — {confs} confirmation(s) (tip {tip})", height.unwrap()),
            "mempool" => println!("⏳ in mempool, not mined yet (tip {tip}) → run again in ~30 s"),
            "orphaned" => println!("⚠️  mined on a fork, not in the main chain → wait, it may be re-mined"),
            _ => println!("❌ not found on {} → check the txid, or wait if you just sent it", upstream.uri()),
        }
        println!("   txid {txid}");
        for l in &links {
            println!("   🔗 {l}");
        }
    }
    Ok(if state == "mined" { 0 } else { 1 })
}

#[derive(serde::Serialize)]
struct Check {
    name: &'static str,
    ok: bool,
    detail: String,
}

async fn doctor(server: &str, listen: SocketAddr, json: bool) -> Result<u8, Fail> {
    let upstream = parse_upstream(server)?;
    let mut checks = Vec::new();
    let mut add = |name, ok, detail: String| checks.push(Check { name, ok, detail });

    match Transport::direct(upstream.clone(), "direct").fresh().await {
        Err(e) => add("upstream reachable", false, format!("{}: {e:#}", upstream.uri())),
        Ok(conn) => {
            let mut c = conn.client;
            add("upstream reachable", true, upstream.uri());
            add("tls", upstream.tls, if upstream.tls { "TLS (webpki roots)".into() } else { "plaintext (loopback)".into() });
            match c.get_lightd_info(Empty {}).await {
                Ok(i) => {
                    let i = i.into_inner();
                    add("chain = test", i.chain_name == "test", format!("chain_name={:?}", i.chain_name));
                    add("protos compatible", true, format!("{} {} (protocol {:?})", i.vendor, i.version, i.lightwallet_protocol_version));
                    add("consensus branch", !i.consensus_branch_id.is_empty(), format!("{} (backend {} {}; next upgrade: {})", i.consensus_branch_id, i.zcashd_build, i.zcashd_subversion, if i.upgrade_name.is_empty() { "none".to_string() } else { format!("{} @ {}", i.upgrade_name, i.upgrade_height) }));
                }
                Err(e) => add("chain = test", false, format!("GetLightdInfo: {e}")),
            }
            match c.get_latest_block(ChainSpec {}).await {
                Ok(b) => add("tip height", true, b.into_inner().height.to_string()),
                Err(e) => add("tip height", false, e.to_string()),
            }
        }
    }

    // Port: free (gateway not running) or our gateway answering on it — both are fine.
    let port_detail = match tokio::net::TcpListener::bind(listen).await {
        Ok(_) => (true, format!("{listen} free")),
        Err(_) => {
            let local = Upstream::parse(&listen.to_string()).expect("socket addr parses");
            match Transport::direct(local, "local").fresh().await {
                Ok(mut conn) => match conn.client.get_latest_block(ChainSpec {}).await {
                    Ok(b) => (true, format!("{listen} in use by a working gateway (tip {})", b.into_inner().height)),
                    Err(e) => (false, format!("{listen} in use, not a gateway: {e}")),
                },
                Err(e) => (false, format!("{listen} in use by something else: {e}")),
            }
        }
    };
    add("listen port", port_detail.0, port_detail.1);

    let dir = data_dir();
    let probe = dir.join(".write-test");
    let writable = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&probe, b"ok")).and_then(|_| std::fs::remove_file(&probe));
    add("storage writable", writable.is_ok(), format!("{} {}", dir.display(), writable.err().map(|e| e.to_string()).unwrap_or_default()));

    let all_ok = checks.iter().all(|c| c.ok);
    if json {
        println!("{}", json!({"ok": all_ok, "checks": checks}));
    } else {
        for c in &checks {
            println!("{} {:<20} {}", if c.ok { "✅" } else { "❌" }, c.name, c.detail);
        }
        if !all_ok {
            println!("→ fix the ❌ items above; for upstream problems check your network or try another --server");
        }
    }
    Ok(if all_ok { 0 } else { 1 })
}

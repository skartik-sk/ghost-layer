//! `ghost` — Ghost Layer CLI. Exit codes: 0 ok, 1 runtime failure, 2 usage/config error,
//! 3 upstream rejected tx.

use std::net::SocketAddr;
use std::collections::BTreeMap;
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
    /// Linkage heuristics over an observer log (ShadeCheck events.jsonl) — the attacker's view
    Attack {
        #[arg(long)]
        events: PathBuf,
        /// Sync-then-send threshold in seconds
        #[arg(long, default_value_t = 10.0)]
        threshold: f64,
        /// All traffic came from one source IP (true for local runs)
        #[arg(long)]
        single_ip: bool,
    },
    /// Before/after reports: ShadeCheck (--before/--after dirs) or attacker (--attack with events files)
    Report {
        #[arg(long)]
        before: PathBuf,
        #[arg(long)]
        after: PathBuf,
        /// Treat --before/--after as observer events.jsonl files and run the linkage heuristics
        #[arg(long)]
        attack: bool,
        #[arg(long, default_value_t = 10.0)]
        threshold: f64,
        /// Output markdown file (default evidence/BEFORE_AFTER.md or evidence/ATTACK_RESULTS.md)
        #[arg(long)]
        out: Option<PathBuf>,
        /// Extra note for the Nym column (e.g. measured `ghost ip --nym` result)
        #[arg(long)]
        nym_note: Option<String>,
    },
    /// Which IP the server sees on the SYNC and BROADCAST paths (real measurement)
    Ip {
        #[command(flatten)]
        nym: NymArgs,
    },
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
        #[command(flatten)]
        nym: NymArgs,
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
    #[command(flatten)]
    nym: NymArgs,
}

#[derive(clap::Args, Clone)]
struct NymArgs {
    /// Route sync and broadcast through two Nym SOCKS5 clients with different exits (fail closed)
    #[arg(long)]
    nym: bool,
    /// Nym SOCKS5 client used for SYNC
    #[arg(long, default_value = "127.0.0.1:1080")]
    nym_sync: SocketAddr,
    /// Nym SOCKS5 client used for BROADCAST (must be a different client/exit than sync)
    #[arg(long, default_value = "127.0.0.1:1081")]
    nym_broadcast: SocketAddr,
}

impl NymArgs {
    fn transports(&self, sync: Upstream, bcast: Upstream) -> Result<(Transport, Transport), Fail> {
        if !self.nym {
            return Ok((Transport::direct(sync, "direct"), Transport::direct(bcast, "direct")));
        }
        if self.nym_sync == self.nym_broadcast {
            return Err(Fail::usage("--nym-sync and --nym-broadcast must differ", "run scripts/nym_setup.sh (ports 1080 and 1081)"));
        }
        Ok((Transport::nym(sync, "nym-sync", self.nym_sync), Transport::nym(bcast, "nym-broadcast", self.nym_broadcast)))
    }

    /// Fail closed: refuse to start with --nym unless both clients are listening on different exits.
    async fn require_up(&self) -> Result<(), Fail> {
        if self.nym {
            let ip = |id| nym_exit(id).and_then(|s| s.split('(').nth(1).map(|x| x.trim_end_matches(')').to_string()));
            if let (Some(a), Some(b)) = (ip("ghost-sync"), ip("ghost-bcast")) {
                if a == b {
                    return Err(Fail::runtime(
                        format!("sync and broadcast Nym clients use the same exit IP {a}"),
                        "run scripts/nym_setup.sh up --reinit to pick two different exits",
                    ));
                }
            }
        }
        for a in [self.nym_sync, self.nym_broadcast] {
            if self.nym && tokio::net::TcpStream::connect(a).await.is_err() {
                return Err(Fail::runtime(
                    format!("Nym SOCKS5 client {a} is not running (refusing to fall back to direct)"),
                    "start them with scripts/nym_setup.sh up",
                ));
            }
        }
        Ok(())
    }
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
        Cmd::Ip { nym } => ip(nym, cli.json).await,
        Cmd::Attack { events, threshold, single_ip } => attack(&events, threshold, single_ip, cli.json),
        Cmd::Report { before, after, attack: true, threshold, out, nym_note } => {
            attack_report(&before, &after, threshold, out, nym_note.as_deref(), cli.json)
        }
        Cmd::Report { before, after, out, .. } => shadecheck_report(&before, &after, out, cli.json),
        Cmd::Verify { txid, server } => verify(&txid, &server, cli.json).await,
        Cmd::Doctor { server, listen, nym } => doctor(&server, listen, nym, cli.json).await,
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

    a.nym.require_up().await?;
    let (sync, bcast) = a.nym.transports(upstream.clone(), bcast_upstream.clone())?;
    let broadcaster = FreshConnBroadcaster { transport: bcast };
    let scheduler = Scheduler::new(open_queue()?, policy, broadcaster, if a.nym.nym { "nym" } else { "direct" });
    let cache = BlockCache::open(&data_dir().join("blocks.sqlite"))
        .map_err(|e| Fail::runtime(format!("cannot open block cache: {e:#}"), "check that ~/.ghost-layer is writable"))?;
    let gw = ghost_gateway::Gateway::connect(&sync, scheduler.clone(), send_mode, cache).await.map_err(|e| {
        Fail::runtime(format!("cannot reach upstream {}: {e:#}", upstream.uri()), "run `ghost doctor` to diagnose")
    })?;
    let (resumed, held) = scheduler
        .resume()
        .map_err(|e| Fail::runtime(format!("cannot resume queue: {e:#}"), "inspect ~/.ghost-layer/ghost.sqlite"))?;

    if json {
        println!("{}", json!({"ok": true, "listen": listen.to_string(), "sync_upstream": upstream.uri(),
            "broadcast_upstream": bcast_upstream.uri(), "nym": a.nym.nym, "profile": a.profile,
            "policy": policy, "send_mode": a.send_mode, "resumed_pending": resumed, "held_nym_only": held}));
    } else {
        println!("👻 Ghost Layer up on {listen}");
        println!("   sync      → {} (one long-lived connection)", upstream.uri());
        println!("   broadcast → {} (fresh connection per tx)", bcast_upstream.uri());
        println!("   Point your wallet at {listen} (e.g. zcash-devtool ... -s {listen})");
        println!("   ✅ broadcasts never share the sync connection");
        println!("   ✅ random broadcast delay {}–{} s ({} profile, send-mode {})",
            policy.min_delay.as_secs(), policy.max_delay.as_secs(), a.profile, a.send_mode);
        println!("   ✅ grid-aligned GetBlockRange/GetBlock: upstream sees 1152-block cells (nym-swizzle-zcash rule)");
        println!("   ⚠️  still visible to the server: tree-state/subtree heights, pool-type filter, transparent-address queries");
        println!("   ✅ wallet headers stripped");
        if a.nym.nym {
            println!("   ✅ Nym mixnet: sync via {} / broadcast via {} (different exits; fail closed)", a.nym.nym_sync, a.nym.nym_broadcast);
        } else {
            println!("   ⚠️  not hidden in direct mode: your IP (same on both paths) — use --nym");
        }
        if resumed > 0 {
            println!("   ↻ resumed {resumed} queued broadcast(s) from the previous run (fresh random delays)");
        }
        if held > 0 {
            println!("   ⏸  {held} broadcast(s) queued under Nym are held: restart with --nym to send them");
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

fn attack(events: &std::path::Path, threshold: f64, single_ip: bool, json: bool) -> Result<u8, Fail> {
    let a = ghost_attacker::analyze(events, threshold, single_ip)
        .map_err(|e| Fail::usage(format!("{e:#}"), "pass the observer's events.jsonl"))?;
    if json {
        println!("{}", serde_json::to_string(&a).expect("serializable"));
        return Ok(0);
    }
    println!("👁  attacker view of {} ({} upstream connections, {} sync sessions)", a.source, a.upstream_connections, a.sync_sessions);
    for s in &a.sends {
        let gap = s.tip_to_send_s.map(|g| format!("{g:.1} s after tip")).unwrap_or_else(|| "no tip seen".into());
        let verdict = if s.linked { format!("🔗 LINKED by {}", s.linked_by.join(" + ")) } else { "✅ not linked".into() };
        println!("  tx #{} ({}…): {verdict}; {gap}", s.index, &s.payload_sha256[..s.payload_sha256.len().min(12)]);
    }
    println!("  range fingerprint: {} of {} sync sessions chained to an earlier one", a.fingerprint_linked_sessions, a.sync_sessions);
    println!("  overall linkage rate: {:.0} %", a.rate());
    Ok(0)
}

fn write_out(out: Option<PathBuf>, default: &str, body: &str) -> Result<PathBuf, Fail> {
    let path = out.unwrap_or_else(|| PathBuf::from(default));
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    std::fs::write(&path, body).map_err(|e| Fail::runtime(format!("cannot write {}: {e}", path.display()), "check the path"))?;
    Ok(path)
}

fn attack_report(before: &std::path::Path, after: &std::path::Path, t: f64, out: Option<PathBuf>, nym: Option<&str>, json: bool) -> Result<u8, Fail> {
    let load = |p| ghost_attacker::analyze(p, t, true).map_err(|e| Fail::usage(format!("{e:#}"), "pass two observer events.jsonl files"));
    let (b, a) = (load(before)?, load(after)?);
    let body = format!(
        "# Attacker linkage test (real runs)\n\nModel: honest-but-curious light-wallet server. Input: the server-side observer log \
(ShadeCheck `events.jsonl`; session = one TCP connection). Generated by `ghost report --attack` at {}.\n\n\
- Before: `{}` — {} payments, {} upstream connections\n- After: `{}` — {} payments, {} upstream connections\n\n{}\n\
Notes: local runs share one source IP (127.0.0.1), so the same-IP heuristic links every payment in both columns; \
only Nym changes that (see the Nym column). Range fingerprint = a session whose first requested start lies within {} blocks \
below/at an earlier session's last end (resume-point chaining) matching exactly one earlier session.\n",
        chrono_now(), b.source, b.sends.len(), b.upstream_connections, a.source, a.sends.len(), a.upstream_connections,
        ghost_attacker::markdown(&b, &a, nym), 10
    );
    let path = write_out(out, "evidence/ATTACK_RESULTS.md", &body)?;
    if json {
        println!("{}", json!({"ok": true, "out": path, "before": b, "after": a}));
    } else {
        println!("{body}\n→ wrote {}", path.display());
    }
    Ok(0)
}

fn chrono_now() -> String {
    let ms = ghost_core::broadcast::now_ms();
    format!("unix {} ({})", ms / 1000, fmt_ms(ms))
}

/// Summarises every ShadeCheck report JSON in a directory: status and findings per rule/severity.
fn shadecheck_summary(dir: &std::path::Path) -> Result<Vec<(String, String, BTreeMap<String, usize>, Vec<String>)>, Fail> {
    let mut rows = Vec::new();
    let entries = std::fs::read_dir(dir).map_err(|e| Fail::usage(format!("{}: {e}", dir.display()), "pass the ShadeCheck output directory"))?;
    let mut files: Vec<_> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
    files.sort();
    for f in files {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&f).unwrap_or_default()) else { continue };
        let Some(findings) = v.get("findings").and_then(|x| x.as_array()) else { continue };
        let mut sev: BTreeMap<String, usize> = BTreeMap::new();
        let mut detail = Vec::new();
        for x in findings {
            let key = format!("{} {}", x["rule_id"].as_str().unwrap_or("?"), x["severity"].as_str().unwrap_or("?"));
            *sev.entry(key).or_default() += 1;
            if let Some(o) = x["observed_behavior"].as_str() {
                detail.push(format!("{}: {}", x["rule_id"].as_str().unwrap_or("?"), o));
            }
        }
        rows.push((f.file_name().unwrap().to_string_lossy().to_string(), v["status"].as_str().unwrap_or("?").to_string(), sev, detail));
    }
    if rows.is_empty() {
        return Err(Fail::usage(format!("no ShadeCheck report JSON in {}", dir.display()), "run `shadecheck test ... --output <dir>/x.json` first"));
    }
    Ok(rows)
}

fn shadecheck_report(before: &std::path::Path, after: &std::path::Path, out: Option<PathBuf>, json: bool) -> Result<u8, Fail> {
    let (b, a) = (shadecheck_summary(before)?, shadecheck_summary(after)?);
    let fmt_sev = |m: &BTreeMap<String, usize>| if m.is_empty() { "none".to_string() } else { m.iter().map(|(k, v)| format!("{v}× {k}")).collect::<Vec<_>>().join(", ") };
    let mut body = format!("# ShadeCheck before / after (real runs)\n\nGenerated by `ghost report` at {}. Reports are ShadeCheck's own JSON output, unmodified, in `{}` and `{}`.\n\n| Report | Before: status | Before: findings | After: status | After: findings |\n|---|---|---|---|---|\n", chrono_now(), before.display(), after.display());
    for (name, st, sev, _) in &b {
        let other = a.iter().find(|r| &r.0 == name);
        body.push_str(&format!("| `{name}` | {st} | {} | {} | {} |\n", fmt_sev(sev),
            other.map(|r| r.1.clone()).unwrap_or("—".into()), other.map(|r| fmt_sev(&r.2)).unwrap_or("—".into())));
    }
    for (label, rows) in [("Before", &b), ("After", &a)] {
        body.push_str(&format!("\n## {label}: observed behaviour (first 8 per report)\n\n"));
        for (name, _, _, detail) in rows.iter() {
            for d in detail.iter().take(8) {
                body.push_str(&format!("- `{name}` — {d}\n"));
            }
        }
    }
    body.push_str("\n> ShadeCheck caveat: a pass means no violation of the tested rule was observed in the executed environment; it does not establish anonymity.\n");
    let path = write_out(out, "evidence/BEFORE_AFTER.md", &body)?;
    if json {
        println!("{}", json!({"ok": true, "out": path}));
    } else {
        println!("{body}\n→ wrote {}", path.display());
    }
    Ok(0)
}

fn nym_exit(id: &str) -> Option<String> {
    let line = std::fs::read_to_string(data_dir().join("nym").join(format!("{id}.provider"))).ok()?;
    let mut it = line.split_whitespace();
    Some(format!("exit node {} ({})", it.next()?, it.next()?))
}

async fn ip(n: NymArgs, json: bool) -> Result<u8, Fail> {
    let measure = |s| async move { ghost_transport::public_ip(s).await.map_err(|e| format!("{e:#}")) };
    let direct = measure(None).await;
    let (sync, bcast) = if n.nym {
        n.require_up().await?;
        (measure(Some(n.nym_sync)).await, measure(Some(n.nym_broadcast)).await)
    } else {
        (direct.clone(), direct.clone())
    };
    let show = |r: &Result<String, String>| r.clone().unwrap_or_else(|e| format!("error: {e}"));
    let unlinkable = matches!((&sync, &bcast, &direct), (Ok(s), Ok(b), Ok(d)) if s != b && s != d && b != d);
    if json {
        println!("{}", json!({"ok": sync.is_ok() && bcast.is_ok(), "nym": n.nym, "your_ip": show(&direct),
            "sync_path_ip": show(&sync), "broadcast_path_ip": show(&bcast), "paths_unlinkable_by_ip": unlinkable,
            "sync_exit": nym_exit("ghost-sync"), "broadcast_exit": nym_exit("ghost-bcast")}));
    } else {
        println!("your real IP            {}", show(&direct));
        if n.nym {
            println!("server sees on SYNC      {}  via Nym {} [{}]", show(&sync), n.nym_sync, nym_exit("ghost-sync").unwrap_or_default());
            println!("server sees on BROADCAST {}  via Nym {} [{}]", show(&bcast), n.nym_broadcast, nym_exit("ghost-bcast").unwrap_or_default());
            println!("{}", if unlinkable { "✅ three different IPs: the server cannot link sync and broadcast by IP" } else { "❌ IPs overlap — check your Nym clients" });
        } else {
            println!("server sees on SYNC      {}  (direct)", show(&sync));
            println!("server sees on BROADCAST {}  (direct)", show(&bcast));
            println!("⚠️  same IP on both paths → linkable by IP. Use `ghost ip --nym` after scripts/nym_setup.sh");
        }
    }
    Ok(if sync.is_ok() && bcast.is_ok() { 0 } else { 1 })
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

async fn doctor(server: &str, listen: SocketAddr, nym: NymArgs, json: bool) -> Result<u8, Fail> {
    let upstream = parse_upstream(server)?;
    let mut checks = Vec::new();
    let mut add = |name, ok, detail: String| checks.push(Check { name, ok, detail });

    // In --nym mode the upstream is only ever contacted through the mixnet.
    let probe = if nym.nym { Transport::nym(upstream.clone(), "nym-sync", nym.nym_sync) } else { Transport::direct(upstream.clone(), "direct") };
    match probe.fresh().await {
        Err(e) => add("upstream reachable", false, format!("{} via {}: {e:#}", upstream.uri(), probe.label)),
        Ok(conn) => {
            let mut c = conn.client;
            add("upstream reachable", true, format!("{} via {}", upstream.uri(), probe.label));
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

    if nym.nym {
        for (name, addr, id) in [("nym sync client", nym.nym_sync, "ghost-sync"), ("nym broadcast client", nym.nym_broadcast, "ghost-bcast")] {
            match ghost_transport::public_ip(Some(addr)).await {
                Ok(ip) => add(name, true, format!("{addr} up, exit IP {ip} [{}]", nym_exit(id).unwrap_or_default())),
                Err(e) => add(name, false, format!("{addr}: {e:#} → scripts/nym_setup.sh up")),
            }
        }
    }
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

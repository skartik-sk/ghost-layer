//! `ghost` — Ghost Layer CLI. Exit codes: 0 ok, 1 runtime failure, 2 usage/config error,
//! 3 upstream rejected tx.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use ghost_transport::proto::{ChainSpec, Empty};
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
    Up {
        /// Upstream lightwalletd server (host:port)
        #[arg(long, default_value = DEFAULT_SERVER)]
        server: String,
        /// Listen address for the wallet
        #[arg(long, default_value = DEFAULT_LISTEN)]
        listen: SocketAddr,
        /// Allow a non-loopback listen address
        #[arg(long)]
        allow_remote: bool,
    },
    /// Check upstream, chain, TLS, port and storage
    Doctor {
        #[arg(long, default_value = DEFAULT_SERVER)]
        server: String,
        #[arg(long, default_value = DEFAULT_LISTEN)]
        listen: SocketAddr,
    },
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
    let level = if cli.quiet { "error" } else if cli.verbose { "info" } else { "warn,upstream=info,ghost=info" };
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
        Cmd::Up { server, listen, allow_remote } => up(&server, listen, allow_remote, cli.json).await,
        Cmd::Doctor { server, listen } => doctor(&server, listen, cli.json).await,
    }
}

fn parse_upstream(server: &str) -> Result<Upstream, Fail> {
    Upstream::parse(server).map_err(|e| Fail::usage(e.to_string(), "use host:port, e.g. testnet.zec.rocks:443"))
}

async fn up(server: &str, listen: SocketAddr, allow_remote: bool, json: bool) -> Result<u8, Fail> {
    if !listen.ip().is_loopback() && !allow_remote {
        return Err(Fail::usage(
            format!("refusing to listen on non-loopback {listen}"),
            "pass --allow-remote if you really want other machines to use this gateway",
        ));
    }
    let upstream = parse_upstream(server)?;
    let sync = Transport::direct(upstream.clone(), "direct");
    let gw = ghost_gateway::Gateway::connect(&sync).await.map_err(|e| {
        Fail::runtime(format!("cannot reach upstream {}: {e:#}", upstream.uri()), "run `ghost doctor` to diagnose")
    })?;

    if json {
        println!("{}", json!({"ok": true, "listen": listen.to_string(), "upstream": upstream.uri(), "sync_transport": "direct"}));
    } else {
        println!("👻 Ghost Layer up on {listen}  →  {}", upstream.uri());
        println!("   Point your wallet at {listen} (e.g. zcash-devtool -s {listen})");
        println!("   ✅ wallet headers stripped; one long-lived sync connection");
        println!("   ⚠️  not yet hidden in this mode: your IP (enable --nym)");
    }
    ghost_gateway::serve(gw, listen)
        .await
        .map_err(|e| Fail::runtime(format!("gateway stopped: {e:#}"), "is the port already in use? try `ghost doctor`"))?;
    Ok(0)
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

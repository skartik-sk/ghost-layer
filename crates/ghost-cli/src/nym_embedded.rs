//! Nym built in: the two SOCKS5 mixnet clients run inside `ghost` (nym-sdk), so users no longer build
//! nym-socks5-client from source (there is no macOS arm64 release of it).
//!
//! Same contract as `scripts/nym_setup.sh`: SYNC on 127.0.0.1:1080 and BROADCAST on 127.0.0.1:1081, through
//! two DIFFERENT exits (network requesters on different gateways), recorded in ~/.ghost-layer/nym/*.provider so
//! `require_up` keeps failing closed if the exits ever match.
//!
//! A watchdog probes each client through the mixnet every minute. A client that fails two probes in a row is
//! replaced by a fresh one on a newly chosen exit (exits degrade every 20-30 min in our testnet runs, which used to
//! need a manual `nym_setup.sh up --reinit`). While a client is being replaced its port is closed, so traffic fails
//! closed instead of falling back to direct.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use ghost_transport::proto::ChainSpec;
use ghost_transport::{Transport, Upstream};
use nym_sdk::mixnet::{MixnetClientBuilder, NetworkRequesterSelector, Recipient, Socks5, Socks5MixnetClient};
use tokio::sync::Mutex;

use crate::data_dir;

/// Exits the two paths currently use (gateway identities), so a replacement never picks the other path's exit.
type InUse = Arc<Mutex<[Option<String>; 2]>>;

const IDS: [&str; 2] = ["ghost-sync", "ghost-bcast"];
const PROBE_EVERY: Duration = Duration::from_secs(60);
const PROBE_TIMEOUT: Duration = Duration::from_secs(45);

fn gateway_of(r: &Recipient) -> String {
    r.gateway().to_base58_string()
}

/// Pick a requester (weighted by performance) whose gateway is not in `avoid`: the other path's exit, and exits
/// that already failed for us.
async fn pick(avoid: &[String]) -> anyhow::Result<Recipient> {
    for _ in 0..16 {
        let r = NetworkRequesterSelector::any().resolve().await?;
        if !avoid.contains(&gateway_of(&r)) {
            return Ok(r);
        }
    }
    anyhow::bail!("could not find another usable Nym exit in the directory")
}

fn record(id: &str, r: &Recipient) {
    let dir = data_dir().join("nym");
    let _ = std::fs::create_dir_all(&dir);
    // Same columns as nym_setup.sh ("node ip provider"); the gateway identity stands in for the IP, which is
    // what require_up compares (different gateway = different exit).
    let gw = gateway_of(r);
    let _ = std::fs::write(dir.join(format!("{id}.provider")), format!("embedded gw:{} {r}\n", &gw[..gw.len().min(12)]));
}

/// Start a client for path `idx` and keep it only once a real request through it succeeds; a dead exit is
/// dropped and another one tried (up to 4).
async fn start_one(idx: usize, bind: SocketAddr, target: &str, in_use: &InUse) -> anyhow::Result<Socks5MixnetClient> {
    let mut bad: Vec<String> = Vec::new();
    for _ in 0..4 {
        let mut avoid = bad.clone();
        avoid.extend(in_use.lock().await[1 - idx].clone());
        let r = pick(&avoid).await?;
        let client = connect(r, bind).await?;
        if probe(bind, target).await {
            in_use.lock().await[idx] = Some(gateway_of(&r));
            record(IDS[idx], &r);
            return Ok(client);
        }
        eprintln!("👻 Nym {}: exit gw:{} did not answer; trying another …", IDS[idx], &gateway_of(&r)[..12]);
        bad.push(gateway_of(&r));
        client.disconnect().await;
    }
    anyhow::bail!("4 Nym exits in a row did not carry traffic")
}

/// An ephemeral SOCKS5 client to requester `r`, with anonymous replies: the exit answers through reply SURBs
/// and never learns this client's Nym address (same as nym-socks5-client `--use-reply-surbs true`;
/// the SDK default is off).
async fn connect(r: Recipient, bind: SocketAddr) -> anyhow::Result<Socks5MixnetClient> {
    let mut cfg = Socks5::new(r.to_string());
    cfg.bind_address = bind;
    cfg.send_anonymously = true;
    Ok(MixnetClientBuilder::new_ephemeral().socks5_config(cfg).build()?.connect_to_mixnet_via_socks5().await?)
}

/// A real request through this client: TLS to the upstream server and GetLatestBlock. (A bare SOCKS CONNECT is
/// not enough: the Nym client answers it locally even when the exit is dead.)
async fn probe(bind: SocketAddr, target: &str) -> bool {
    let call = async {
        let up = Upstream::parse(target).ok()?;
        let mut c = Transport::nym(up, "nym-probe", bind).fresh().await.ok()?.client;
        c.get_latest_block(ChainSpec {}).await.ok().map(|_| ())
    };
    matches!(tokio::time::timeout(PROBE_TIMEOUT, call).await, Ok(Some(())))
}

/// Keep path `idx` healthy forever: replace its client after two failed probes in a row.
async fn watch(idx: usize, bind: SocketAddr, target: String, mut client: Socks5MixnetClient, in_use: InUse) {
    let mut fails = 0u32;
    loop {
        tokio::time::sleep(PROBE_EVERY).await;
        if probe(bind, &target).await {
            fails = 0;
            continue;
        }
        fails += 1;
        if fails < 2 {
            continue;
        }
        eprintln!("👻 Nym {}: exit stopped carrying traffic; switching to a new exit …", IDS[idx]);
        client.disconnect().await;
        in_use.lock().await[idx] = None;
        // Keep trying; until a new client is up the port stays closed and Ghost fails closed.
        client = loop {
            match start_one(idx, bind, &target, &in_use).await {
                Ok(c) => break c,
                Err(e) => {
                    eprintln!("👻 Nym {}: no exit yet ({e:#}); retrying in 30 s", IDS[idx]);
                    tokio::time::sleep(Duration::from_secs(30)).await;
                }
            }
        };
        eprintln!("👻 Nym {}: back up via a new exit", IDS[idx]);
        fails = 0;
    }
}

/// Start both clients (sync first, then a broadcast client on a different exit) and their watchdogs.
/// They live until the process exits. `target` is the upstream `host:port` the probes connect to.
pub(crate) async fn start(sync: SocketAddr, bcast: SocketAddr, target: &str) -> anyhow::Result<()> {
    let in_use: InUse = Arc::new(Mutex::new([None, None]));
    let s = start_one(0, sync, target, &in_use).await?;
    let b = start_one(1, bcast, target, &in_use).await?;
    tokio::spawn(watch(0, sync, target.to_string(), s, in_use.clone()));
    tokio::spawn(watch(1, bcast, target.to_string(), b, in_use));
    Ok(())
}

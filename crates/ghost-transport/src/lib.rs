//! Upstream transports (direct TLS, Nym SOCKS5) and the generated lightwalletd protocol.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tonic::transport::{Channel, ClientTlsConfig, Endpoint};

pub mod proto {
    tonic::include_proto!("cash.z.wallet.sdk.rpc");
}

pub type Client = proto::compact_tx_streamer_client::CompactTxStreamerClient<Channel>;

/// Max gRPC message size we accept/send (compact blocks and raw txs can be large).
pub const MAX_MSG: usize = 64 * 1024 * 1024;

/// A lightwalletd-compatible server, e.g. `testnet.zec.rocks:443`.
#[derive(Clone, Debug)]
pub struct Upstream {
    pub host: String,
    pub port: u16,
    pub tls: bool,
}

impl Upstream {
    /// Parses `host:port`. TLS is on unless the host is loopback (same rule as zcash-devtool).
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        let s = s.trim_start_matches("https://").trim_start_matches("http://");
        let (host, port) = s
            .rsplit_once(':')
            .ok_or_else(|| anyhow::anyhow!("server must be host:port, got '{s}'"))?;
        let port: u16 = port.parse().map_err(|_| anyhow::anyhow!("bad port in '{s}'"))?;
        let tls = !matches!(host, "localhost" | "127.0.0.1" | "::1");
        Ok(Self { host: host.to_string(), port, tls })
    }

    pub fn uri(&self) -> String {
        let scheme = if self.tls { "https" } else { "http" };
        format!("{scheme}://{}:{}", self.host, self.port)
    }

    fn endpoint(&self) -> anyhow::Result<Endpoint> {
        // No custom user-agent: tonic's default ("tonic/<ver>") is what every tonic-based wallet
        // (e.g. zcash-devtool) sends, so it adds nothing unique. `:authority` is the upstream host.
        let mut ep = Endpoint::from_shared(self.uri())?
            .connect_timeout(Duration::from_secs(30))
            .tcp_nodelay(true);
        if self.tls {
            ep = ep.tls_config(ClientTlsConfig::new().with_webpki_roots().domain_name(&self.host))?;
        }
        Ok(ep)
    }
}

static CONN_SEQ: AtomicU64 = AtomicU64::new(1);

/// One upstream connection with an id we can log (so evidence shows sync vs broadcast sockets).
pub struct Conn {
    pub id: u64,
    pub client: Client,
}

/// How a role (SYNC or BROADCAST) reaches the upstream.
#[derive(Clone, Debug)]
pub struct Transport {
    pub upstream: Upstream,
    pub label: &'static str,
}

impl Transport {
    pub fn direct(upstream: Upstream, label: &'static str) -> Self {
        Self { upstream, label }
    }

    /// Opens a brand-new connection (new TCP + TLS + HTTP/2). Never pooled or reused.
    pub async fn fresh(&self) -> anyhow::Result<Conn> {
        let channel = self.upstream.endpoint()?.connect().await?;
        let id = CONN_SEQ.fetch_add(1, Ordering::Relaxed);
        Ok(Conn { id, client: wrap(channel) })
    }
}

pub fn wrap(channel: Channel) -> Client {
    Client::new(channel)
        .max_decoding_message_size(MAX_MSG)
        .max_encoding_message_size(MAX_MSG)
}

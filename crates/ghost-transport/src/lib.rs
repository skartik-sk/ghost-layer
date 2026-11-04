//! Upstream transports (direct TLS, Nym SOCKS5) and the generated lightwalletd protocol.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use hyper_util::rt::TokioIo;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_socks::tcp::Socks5Stream;

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
    /// Nym SOCKS5 client to tunnel through. When set there is no code path to a direct
    /// connection: if the Nym client is down, the call fails (fail closed).
    pub socks: Option<SocketAddr>,
}

impl Transport {
    pub fn direct(upstream: Upstream, label: &'static str) -> Self {
        Self { upstream, label, socks: None }
    }

    pub fn nym(upstream: Upstream, label: &'static str, socks: SocketAddr) -> Self {
        Self { upstream, label, socks: Some(socks) }
    }

    /// Opens a brand-new connection (new TCP or SOCKS5 stream + TLS + HTTP/2). Never pooled.
    pub async fn fresh(&self) -> anyhow::Result<Conn> {
        let ep = self.upstream.endpoint()?;
        let channel = match self.socks {
            None => ep.connect().await?,
            Some(proxy) => {
                // Mixnet setup is slow; give it longer than a direct connect.
                let ep = ep.connect_timeout(Duration::from_secs(120));
                ep.connect_with_connector(SocksConnector { proxy })
                    .await
                    .map_err(|e| anyhow::anyhow!("via Nym SOCKS5 {proxy} (fail-closed, no direct fallback): {e}"))?
            }
        };
        let id = CONN_SEQ.fetch_add(1, Ordering::Relaxed);
        Ok(Conn { id, client: wrap(channel) })
    }
}

/// tower connector: TCP through a SOCKS5 proxy. The hostname is sent to the proxy, so DNS is
/// resolved by the Nym exit, never locally. tonic layers TLS on top.
#[derive(Clone, Copy, Debug)]
struct SocksConnector {
    proxy: SocketAddr,
}

type BoxErr = Box<dyn std::error::Error + Send + Sync>;

impl tower::Service<http::Uri> for SocksConnector {
    type Response = TokioIo<TcpStream>;
    type Error = BoxErr;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, BoxErr>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), BoxErr>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: http::Uri) -> Self::Future {
        let proxy = self.proxy;
        Box::pin(async move {
            let host = uri.host().ok_or("uri without host")?.to_string();
            let port = uri.port_u16().unwrap_or(443);
            let s = Socks5Stream::connect(proxy, (host.as_str(), port)).await?;
            Ok(TokioIo::new(s.into_inner()))
        })
    }
}

/// The public IP a server sees for a path: HTTPS GET to an IP-echo service, direct or through a
/// SOCKS5 proxy (hostname resolved by the proxy). A real measurement, not a config lookup.
pub async fn public_ip(socks: Option<SocketAddr>) -> anyhow::Result<String> {
    const HOST: &str = "api.ipify.org";
    let tcp = match socks {
        None => TcpStream::connect((HOST, 443)).await?,
        Some(p) => Socks5Stream::connect(p, (HOST, 443)).await?.into_inner(),
    };
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let mut tls = tokio_rustls::TlsConnector::from(Arc::new(cfg))
        .connect(rustls::pki_types::ServerName::try_from(HOST)?, tcp)
        .await?;
    tls.write_all(format!("GET / HTTP/1.1\r\nHost: {HOST}\r\nUser-Agent: curl/8\r\nConnection: close\r\n\r\n").as_bytes())
        .await?;
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(90), tls.read_to_end(&mut buf)).await??;
    let text = String::from_utf8_lossy(&buf);
    let body = text.split("\r\n\r\n").nth(1).unwrap_or_default().trim();
    let ip: String = body.chars().filter(|c| c.is_ascii_hexdigit() || *c == '.' || *c == ':').collect();
    anyhow::ensure!(!ip.is_empty() && ip.len() <= 45, "unexpected reply from {HOST}");
    Ok(ip)
}

pub fn wrap(channel: Channel) -> Client {
    Client::new(channel)
        .max_decoding_message_size(MAX_MSG)
        .max_encoding_message_size(MAX_MSG)
}

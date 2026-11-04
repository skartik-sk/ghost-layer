//! Upstream transports (direct TLS, Nym SOCKS5) and the generated lightwalletd protocol.

pub mod proto {
    tonic::include_proto!("cash.z.wallet.sdk.rpc");
}

//! ghost-gateway: a local lightwalletd-compatible `CompactTxStreamer` server.
//!
//! Passthrough RPCs go over the long-lived SYNC connection. Every upstream request is rebuilt
//! from the message body only, so no wallet metadata (User-Agent, X-Forwarded-For, Forwarded,
//! Via, ...) is ever forwarded upstream.

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

use ghost_core::broadcast::{Broadcaster, Outcome, Scheduler, Sent};

use futures::{Stream, StreamExt};
use ghost_transport::proto::compact_tx_streamer_server::{CompactTxStreamer, CompactTxStreamerServer};
use ghost_transport::proto::*;
use ghost_transport::{Client, MAX_MSG, Transport};
use tonic::{Request, Response, Status, Streaming};
use tracing::info;

type BoxStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send + 'static>>;

/// How `SendTransaction` answers the wallet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendMode {
    /// Hold the wallet's call until the real broadcast finishes; return the real upstream reply.
    Hold,
    /// Answer as soon as the tx is queued; the real result is tracked in `ghost status`.
    Queue,
}

/// Broadcasts each tx over a brand-new upstream connection, then drops it.
pub struct FreshConnBroadcaster {
    pub transport: Transport,
}

impl Broadcaster for FreshConnBroadcaster {
    async fn broadcast(&self, raw: Vec<u8>) -> anyhow::Result<Sent> {
        let mut conn = self.transport.fresh().await?;
        info!(target: "upstream", conn = conn.id, path = "broadcast", transport = self.transport.label,
            method = "SendTransaction", "fresh connection opened for this tx only");
        let resp = conn.client.send_transaction(RawTransaction { data: raw, height: 0 }).await?.into_inner();
        Ok(Sent {
            conn_id: conn.id,
            transport: self.transport.label.to_string(),
            endpoint: self.transport.upstream.uri(),
            error_code: resp.error_code,
            error_message: resp.error_message,
        })
    }
}

pub struct Gateway {
    sync: Client,
    sync_conn_id: u64,
    scheduler: Arc<Scheduler<FreshConnBroadcaster>>,
    send_mode: SendMode,
}

impl Gateway {
    /// Opens the single long-lived SYNC connection.
    pub async fn connect(
        sync: &Transport,
        scheduler: Arc<Scheduler<FreshConnBroadcaster>>,
        send_mode: SendMode,
    ) -> anyhow::Result<Self> {
        let conn = sync.fresh().await?;
        info!(target: "upstream", conn = conn.id, transport = sync.label, server = %sync.upstream.uri(), "sync connection opened");
        Ok(Self { sync: conn.client, sync_conn_id: conn.id, scheduler, send_mode })
    }

    fn client(&self) -> Client {
        self.sync.clone()
    }

    fn log(&self, method: &str, detail: String) {
        info!(target: "upstream", conn = self.sync_conn_id, path = "sync", method, "{detail}");
    }
}

fn passthrough_stream<T: Send + 'static>(s: Streaming<T>) -> BoxStream<T> {
    Box::pin(s)
}

macro_rules! unary {
    ($self:ident, $req:ident, $method:ident, $name:literal) => {{
        let msg = $req.into_inner();
        $self.log($name, msg.summary());
        let resp = $self.client().$method(Request::new(msg)).await?;
        Ok(Response::new(resp.into_inner()))
    }};
}

macro_rules! streaming {
    ($self:ident, $req:ident, $method:ident, $name:literal) => {{
        let msg = $req.into_inner();
        $self.log($name, msg.summary());
        let resp = $self.client().$method(Request::new(msg)).await?;
        Ok(Response::new(passthrough_stream(resp.into_inner())))
    }};
}

#[allow(deprecated)] // deprecated nullifier RPCs must still pass through for older wallets
#[tonic::async_trait]
impl CompactTxStreamer for Gateway {
    async fn get_latest_block(&self, r: Request<ChainSpec>) -> Result<Response<BlockId>, Status> {
        unary!(self, r, get_latest_block, "GetLatestBlock")
    }
    async fn get_block(&self, r: Request<BlockId>) -> Result<Response<CompactBlock>, Status> {
        unary!(self, r, get_block, "GetBlock")
    }
    async fn get_block_nullifiers(&self, r: Request<BlockId>) -> Result<Response<CompactBlock>, Status> {
        unary!(self, r, get_block_nullifiers, "GetBlockNullifiers")
    }

    type GetBlockRangeStream = BoxStream<CompactBlock>;
    async fn get_block_range(&self, r: Request<BlockRange>) -> Result<Response<Self::GetBlockRangeStream>, Status> {
        streaming!(self, r, get_block_range, "GetBlockRange")
    }

    type GetBlockRangeNullifiersStream = BoxStream<CompactBlock>;
    async fn get_block_range_nullifiers(
        &self,
        r: Request<BlockRange>,
    ) -> Result<Response<Self::GetBlockRangeNullifiersStream>, Status> {
        streaming!(self, r, get_block_range_nullifiers, "GetBlockRangeNullifiers")
    }

    async fn get_transaction(&self, r: Request<TxFilter>) -> Result<Response<RawTransaction>, Status> {
        unary!(self, r, get_transaction, "GetTransaction")
    }

    /// Never sent over the sync connection: queued, delayed, then broadcast on a fresh one.
    async fn send_transaction(&self, r: Request<RawTransaction>) -> Result<Response<SendResponse>, Status> {
        let raw = r.into_inner().data;
        let (txid, mut rx) = self
            .scheduler
            .submit(raw)
            .map_err(|e| Status::invalid_argument(format!("ghost: {e:#}")))?;
        if self.send_mode == SendMode::Queue {
            // The wallet only learns "queued"; `ghost status` shows the real outcome.
            return Ok(Response::new(SendResponse { error_code: 0, error_message: txid }));
        }
        let outcome = loop {
            if let Some(o) = rx.borrow_and_update().clone() {
                break o;
            }
            rx.changed().await.map_err(|_| Status::internal("ghost: broadcast task ended unexpectedly"))?;
        };
        match outcome {
            Outcome::Accepted(s) | Outcome::Rejected(s) => {
                Ok(Response::new(SendResponse { error_code: s.error_code, error_message: s.error_message }))
            }
            Outcome::Failed(e) => Err(Status::unavailable(format!("ghost: broadcast of {txid} failed: {e}"))),
        }
    }

    type GetTaddressTxidsStream = BoxStream<RawTransaction>;
    async fn get_taddress_txids(
        &self,
        r: Request<TransparentAddressBlockFilter>,
    ) -> Result<Response<Self::GetTaddressTxidsStream>, Status> {
        streaming!(self, r, get_taddress_txids, "GetTaddressTxids")
    }

    type GetTaddressTransactionsStream = BoxStream<RawTransaction>;
    async fn get_taddress_transactions(
        &self,
        r: Request<TransparentAddressBlockFilter>,
    ) -> Result<Response<Self::GetTaddressTransactionsStream>, Status> {
        streaming!(self, r, get_taddress_transactions, "GetTaddressTransactions")
    }

    async fn get_taddress_balance(&self, r: Request<AddressList>) -> Result<Response<Balance>, Status> {
        unary!(self, r, get_taddress_balance, "GetTaddressBalance")
    }

    async fn get_taddress_balance_stream(
        &self,
        r: Request<Streaming<Address>>,
    ) -> Result<Response<Balance>, Status> {
        self.log("GetTaddressBalanceStream", String::new());
        // Collect first so a client-side stream error fails the call instead of being dropped.
        let addrs: Vec<Address> = r.into_inner().collect::<Vec<_>>().await.into_iter().collect::<Result<_, _>>()?;
        let resp = self
            .client()
            .get_taddress_balance_stream(futures::stream::iter(addrs))
            .await
            ?;
        Ok(Response::new(resp.into_inner()))
    }

    type GetMempoolTxStream = BoxStream<CompactTx>;
    async fn get_mempool_tx(&self, r: Request<GetMempoolTxRequest>) -> Result<Response<Self::GetMempoolTxStream>, Status> {
        streaming!(self, r, get_mempool_tx, "GetMempoolTx")
    }

    type GetMempoolStreamStream = BoxStream<RawTransaction>;
    async fn get_mempool_stream(&self, r: Request<Empty>) -> Result<Response<Self::GetMempoolStreamStream>, Status> {
        streaming!(self, r, get_mempool_stream, "GetMempoolStream")
    }

    async fn get_tree_state(&self, r: Request<BlockId>) -> Result<Response<TreeState>, Status> {
        unary!(self, r, get_tree_state, "GetTreeState")
    }
    async fn get_latest_tree_state(&self, r: Request<Empty>) -> Result<Response<TreeState>, Status> {
        unary!(self, r, get_latest_tree_state, "GetLatestTreeState")
    }

    type GetSubtreeRootsStream = BoxStream<SubtreeRoot>;
    async fn get_subtree_roots(&self, r: Request<GetSubtreeRootsArg>) -> Result<Response<Self::GetSubtreeRootsStream>, Status> {
        streaming!(self, r, get_subtree_roots, "GetSubtreeRoots")
    }

    async fn get_address_utxos(&self, r: Request<GetAddressUtxosArg>) -> Result<Response<GetAddressUtxosReplyList>, Status> {
        unary!(self, r, get_address_utxos, "GetAddressUtxos")
    }

    type GetAddressUtxosStreamStream = BoxStream<GetAddressUtxosReply>;
    async fn get_address_utxos_stream(
        &self,
        r: Request<GetAddressUtxosArg>,
    ) -> Result<Response<Self::GetAddressUtxosStreamStream>, Status> {
        streaming!(self, r, get_address_utxos_stream, "GetAddressUtxosStream")
    }

    async fn get_lightd_info(&self, r: Request<Empty>) -> Result<Response<LightdInfo>, Status> {
        unary!(self, r, get_lightd_info, "GetLightdInfo")
    }
    async fn get_standard_fee(&self, r: Request<Empty>) -> Result<Response<StandardFee>, Status> {
        unary!(self, r, get_standard_fee, "GetStandardFee")
    }
    async fn ping(&self, r: Request<Duration>) -> Result<Response<PingResponse>, Status> {
        unary!(self, r, ping, "Ping")
    }
}

/// Serves the gateway (plaintext h2c) until the future is dropped.
pub async fn serve(gw: Gateway, listen: SocketAddr) -> anyhow::Result<()> {
    let svc = CompactTxStreamerServer::new(gw)
        .max_decoding_message_size(MAX_MSG)
        .max_encoding_message_size(MAX_MSG);
    tonic::transport::Server::builder().add_service(svc).serve(listen).await?;
    Ok(())
}

/// What the request log may record about a request: block heights only. Addresses, txids and
/// raw transactions are never logged (they would link the user's activity on disk).
trait Summary {
    fn summary(&self) -> String {
        String::new()
    }
}

impl Summary for BlockRange {
    fn summary(&self) -> String {
        let h = |b: &Option<BlockId>| b.as_ref().map(|b| b.height).unwrap_or_default();
        format!("range={}..={}", h(&self.start), h(&self.end))
    }
}

impl Summary for BlockId {
    fn summary(&self) -> String {
        format!("height={}", self.height)
    }
}

impl Summary for GetSubtreeRootsArg {
    fn summary(&self) -> String {
        format!("start_index={} max={}", self.start_index, self.max_entries)
    }
}

macro_rules! no_detail {
    ($($t:ty),*) => { $(impl Summary for $t {})* };
}
no_detail!(ChainSpec, TxFilter, RawTransaction, TransparentAddressBlockFilter, AddressList, GetMempoolTxRequest,
    Empty, GetAddressUtxosArg, Duration);

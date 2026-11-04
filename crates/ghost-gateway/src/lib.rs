//! ghost-gateway: a local lightwalletd-compatible `CompactTxStreamer` server.
//!
//! Passthrough RPCs go over the long-lived SYNC connection. Every upstream request is rebuilt
//! from the message body only, so no wallet metadata (User-Agent, X-Forwarded-For, Forwarded,
//! Via, ...) is ever forwarded upstream.

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ghost_core::grid::{self, BlockCache};
use prost::Message;

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
            // Upstream text is untrusted: strip control chars (terminal escape injection).
            error_message: resp.error_message.chars().filter(|c| !c.is_control()).take(500).collect(),
        })
    }
}

pub struct Gateway {
    sync: Client,
    sync_conn_id: u64,
    scheduler: Arc<Scheduler<FreshConnBroadcaster>>,
    send_mode: SendMode,
    grid: Arc<GridFetcher>,
}

/// Fetches whole grid cells over the SYNC connection and serves wallets from them.
struct GridFetcher {
    sync: Client,
    conn_id: u64,
    cache: BlockCache,
    tip: AtomicU64,
}

impl GridFetcher {
    async fn refresh_tip(&self) -> Result<u64, Status> {
        info!(target: "upstream", conn = self.conn_id, path = "sync", method = "GetLatestBlock", "tip refresh for grid planner");
        let h = self.sync.clone().get_latest_block(ChainSpec {}).await?.into_inner().height;
        self.tip.store(h, Ordering::Relaxed);
        Ok(h)
    }

    /// Returns cell blocks (height, encoded CompactBlock), from cache or one aligned upstream request.
    async fn cell(&self, kind: &str, nullifiers: bool, pools: &[i32], cell: (u64, u64), tip: u64) -> Result<Vec<(u64, Vec<u8>)>, Status> {
        let internal = |e: anyhow::Error| Status::internal(format!("ghost cache: {e:#}"));
        if self.cache.has_cell(kind, cell, tip).map_err(internal)? {
            info!(target: "grid", cell_start = cell.0, cell_end = cell.1, "cell served from cache (no upstream request)");
            return self.cache.get(kind, cell.0, cell.1).map_err(internal);
        }
        let method = if nullifiers { "GetBlockRangeNullifiers" } else { "GetBlockRange" };
        info!(target: "upstream", conn = self.conn_id, path = "sync", method, aligned = grid::is_aligned(cell.0, cell.1, tip),
            "range={}..={} (grid cell)", cell.0, cell.1);
        let req = BlockRange {
            start: Some(BlockId { height: cell.0, hash: vec![] }),
            end: Some(BlockId { height: cell.1, hash: vec![] }),
            pool_types: pools.to_vec(),
        };
        let mut c = self.sync.clone();
        let mut stream = if nullifiers {
            #[allow(deprecated)]
            c.get_block_range_nullifiers(req).await?.into_inner()
        } else {
            c.get_block_range(req).await?.into_inner()
        };
        let mut out = Vec::new();
        while let Some(b) = stream.next().await {
            let b = b?;
            out.push((b.height, b.encode_to_vec()));
        }
        self.cache.put_cell(kind, cell, &out).map_err(internal)?;
        Ok(out)
    }

    async fn range(self: Arc<Self>, msg: BlockRange, nullifiers: bool) -> Result<BoxStream<CompactBlock>, Status> {
        let a = msg.start.as_ref().map(|b| b.height).unwrap_or_default();
        let b = msg.end.as_ref().map(|b| b.height).unwrap_or_default();
        let (lo, hi, descending) = if a <= b { (a, b, false) } else { (b, a, true) };
        let mut tip = self.tip.load(Ordering::Relaxed);
        if hi > tip {
            tip = self.refresh_tip().await?;
        }
        if hi > tip {
            return Err(Status::out_of_range(format!("requested height {hi} is above the chain tip {tip}")));
        }
        let plan = grid::plan(lo, hi, tip);
        let mut pools = msg.pool_types.clone();
        pools.sort();
        let kind = format!("{}:{pools:?}", if nullifiers { "nf" } else { "cb" });
        info!(target: "grid", wallet_range = %format!("{a}..={b}"), emitted = %format!("{}..={}", plan.emitted.0, plan.emitted.1),
            cells = plan.cells.len(), spacing = plan.spacing, "planned grid-aligned fetch");

        let (tx, rx) = tokio::sync::mpsc::channel::<Result<CompactBlock, Status>>(64);
        tokio::spawn(async move {
            let mut held = Vec::new(); // only used for descending requests
            for cell in plan.cells {
                match self.cell(&kind, nullifiers, &pools, cell, tip).await {
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                        return;
                    }
                    Ok(blocks) => {
                        for (h, data) in blocks.into_iter().filter(|(h, _)| (lo..=hi).contains(h)) {
                            let blk = CompactBlock::decode(data.as_slice())
                                .map_err(|e| Status::internal(format!("ghost cache decode at {h}: {e}")));
                            if descending {
                                held.push(blk);
                            } else if tx.send(blk).await.is_err() {
                                return; // wallet went away
                            }
                        }
                    }
                }
            }
            for blk in held.into_iter().rev() {
                if tx.send(blk).await.is_err() {
                    return;
                }
            }
        });
        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }
}

impl Gateway {
    /// Opens the single long-lived SYNC connection.
    pub async fn connect(
        sync: &Transport,
        scheduler: Arc<Scheduler<FreshConnBroadcaster>>,
        send_mode: SendMode,
        cache: BlockCache,
    ) -> anyhow::Result<Self> {
        let conn = sync.fresh().await?;
        info!(target: "upstream", conn = conn.id, transport = sync.label, server = %sync.upstream.uri(), "sync connection opened");
        let grid = Arc::new(GridFetcher { sync: conn.client.clone(), conn_id: conn.id, cache, tip: AtomicU64::new(0) });
        Ok(Self { sync: conn.client, sync_conn_id: conn.id, scheduler, send_mode, grid })
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
        let resp: Result<Response<BlockId>, Status> = unary!(self, r, get_latest_block, "GetLatestBlock");
        if let Ok(b) = &resp {
            self.grid.tip.store(b.get_ref().height, Ordering::Relaxed);
        }
        resp
    }
    async fn get_block(&self, r: Request<BlockId>) -> Result<Response<CompactBlock>, Status> {
        unary!(self, r, get_block, "GetBlock")
    }
    async fn get_block_nullifiers(&self, r: Request<BlockId>) -> Result<Response<CompactBlock>, Status> {
        unary!(self, r, get_block_nullifiers, "GetBlockNullifiers")
    }

    type GetBlockRangeStream = BoxStream<CompactBlock>;
    /// Grid-aligned: upstream sees only whole grid cells; the wallet gets exactly its range.
    async fn get_block_range(&self, r: Request<BlockRange>) -> Result<Response<Self::GetBlockRangeStream>, Status> {
        Ok(Response::new(self.grid.clone().range(r.into_inner(), false).await?))
    }

    type GetBlockRangeNullifiersStream = BoxStream<CompactBlock>;
    async fn get_block_range_nullifiers(
        &self,
        r: Request<BlockRange>,
    ) -> Result<Response<Self::GetBlockRangeNullifiersStream>, Status> {
        Ok(Response::new(self.grid.clone().range(r.into_inner(), true).await?))
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

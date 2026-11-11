//! ghost-gateway: a local lightwalletd-compatible `CompactTxStreamer` server.
//!
//! Passthrough RPCs go over the long-lived SYNC connection. Every upstream request is rebuilt
//! from the message body only, so no wallet metadata (User-Agent, X-Forwarded-For, Forwarded,
//! Via, ...) is ever forwarded upstream.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Instant;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ghost_core::events::EventLog;
use ghost_core::grid::{self, BlockCache};
use serde_json::json;
use sha2::{Digest, Sha256};
use prost::Message;

use ghost_core::broadcast::{Broadcaster, Outcome, Scheduler, Sent};

use futures::{Stream, StreamExt};
use ghost_transport::proto::compact_tx_streamer_server::{CompactTxStreamer, CompactTxStreamerServer};
use ghost_transport::proto::*;
use ghost_transport::{Client, MAX_MSG, Transport};
use tonic::{Request, Response, Status, Streaming};
use tracing::{info, warn};

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
    pub events: Arc<EventLog>,
}

impl Broadcaster for FreshConnBroadcaster {
    async fn broadcast(&self, raw: Vec<u8>) -> anyhow::Result<Sent> {
        let mut conn = self.transport.fresh().await?;
        info!(target: "upstream", conn = conn.id, path = "broadcast", transport = self.transport.label,
            method = "SendTransaction", "fresh connection opened for this tx only");
        self.events.record("SendTransaction", "request", conn.id, self.transport.label,
            json!({"payload_size": raw.len(), "payload_sha256": hex::encode(Sha256::digest(&raw))}));
        let resp = conn.client.send_transaction(RawTransaction { data: raw, height: 0 }).await?.into_inner();
        self.events.record("SendTransaction", "response", conn.id, self.transport.label, json!({"error_code": resp.error_code}));
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
    events: Arc<EventLog>,
    sync_transport: Transport,
    /// One upstream SYNC connection per wallet connection (keyed by the wallet's local socket),
    /// so separate wallet sessions are separate connections upstream too.
    sessions: tokio::sync::Mutex<HashMap<SocketAddr, SyncSession>>,
    scheduler: Arc<Scheduler<FreshConnBroadcaster>>,
    send_mode: SendMode,
    grid: Arc<GridFetcher>,
}

#[derive(Clone)]
struct SyncSession {
    client: Client,
    conn_id: u64,
    last_used: Instant,
}

/// Upstream sync connections idle longer than this are dropped.
const SESSION_IDLE: std::time::Duration = std::time::Duration::from_secs(120);

/// Grid cell cache and tip, shared by all sync sessions.
struct GridFetcher {
    cache: BlockCache,
    tip: AtomicU64,
    events: Arc<EventLog>,
    label: &'static str,
}

impl GridFetcher {
    async fn refresh_tip(&self, mut c: Client, conn_id: u64) -> Result<u64, Status> {
        info!(target: "upstream", conn = conn_id, path = "sync", method = "GetLatestBlock", "tip refresh for grid planner");
        self.events.record("GetLatestBlock", "request", conn_id, self.label, json!({}));
        let h = c.get_latest_block(ChainSpec {}).await?.into_inner().height;
        self.events.record("GetLatestBlock", "response", conn_id, self.label, json!({"height": h}));
        self.tip.store(h, Ordering::Relaxed);
        Ok(h)
    }

    /// Returns cell blocks (height, encoded CompactBlock), from cache or one aligned upstream request.
    #[allow(clippy::too_many_arguments)]
    async fn cell(&self, mut c: Client, conn_id: u64, kind: &str, nullifiers: bool, pools: &[i32], cell: (u64, u64), tip: u64) -> Result<Vec<(u64, Vec<u8>)>, Status> {
        let internal = |e: anyhow::Error| Status::internal(format!("ghost cache: {e:#}"));
        if self.cache.has_cell(kind, cell, tip).map_err(internal)? {
            info!(target: "grid", cell_start = cell.0, cell_end = cell.1, "cell served from cache (no upstream request)");
            return self.cache.get(kind, cell.0, cell.1).map_err(internal);
        }
        let method = if nullifiers { "GetBlockRangeNullifiers" } else { "GetBlockRange" };
        info!(target: "upstream", conn = conn_id, path = "sync", method, aligned = grid::is_aligned(cell.0, cell.1, tip),
            "range={}..={} (grid cell)", cell.0, cell.1);
        self.events.record(method, "request", conn_id, self.label,
            json!({"start_height": cell.0, "end_height": cell.1, "aligned": grid::is_aligned(cell.0, cell.1, tip)}));
        let req = BlockRange {
            start: Some(BlockId { height: cell.0, hash: vec![] }),
            end: Some(BlockId { height: cell.1, hash: vec![] }),
            pool_types: pools.to_vec(),
        };
        let mut stream = if nullifiers {
            #[allow(deprecated)]
            c.get_block_range_nullifiers(req).await?.into_inner()
        } else {
            c.get_block_range(req).await?.into_inner()
        };
        // Validate before caching: one block per height, contiguous inside the cell, hash-linked.
        let bad = |why: String| Status::data_loss(format!("ghost: upstream cell {}..={} rejected: {why}", cell.0, cell.1));
        let mut out = Vec::new();
        let mut prev: Option<(u64, Vec<u8>)> = None;
        while let Some(b) = stream.next().await {
            let b = b?;
            let want = prev.as_ref().map(|(h, _)| h + 1).unwrap_or(cell.0);
            if b.height != want || b.height > cell.1 {
                return Err(bad(format!("expected height {want}, got {}", b.height)));
            }
            if let Some((_, ph)) = &prev {
                if !b.prev_hash.is_empty() && !ph.is_empty() && &b.prev_hash != ph {
                    return Err(bad(format!("block {} does not link to its predecessor", b.height)));
                }
            }
            prev = Some((b.height, b.hash.clone()));
            out.push((b.height, b.encode_to_vec()));
        }
        if out.len() as u64 != cell.1 - cell.0 + 1 {
            return Err(bad(format!("got {} blocks, expected {}", out.len(), cell.1 - cell.0 + 1)));
        }
        self.events.record(method, "response", conn_id, self.label, json!({"height": cell.1, "blocks": out.len()}));
        self.cache.put_cell(kind, cell, &out).map_err(internal)?;
        Ok(out)
    }

    async fn single(self: Arc<Self>, c: Client, conn_id: u64, height: u64, nullifiers: bool) -> Result<CompactBlock, Status> {
        let id = Some(BlockId { height, hash: vec![] });
        let mut s = self.range(c, conn_id, BlockRange { start: id.clone(), end: id, pool_types: vec![] }, nullifiers).await?;
        s.next().await.unwrap_or_else(|| Err(Status::not_found(format!("block {height} not found"))))
    }

    async fn range(self: Arc<Self>, c: Client, conn_id: u64, msg: BlockRange, nullifiers: bool) -> Result<BoxStream<CompactBlock>, Status> {
        let a = msg.start.as_ref().map(|b| b.height).unwrap_or_default();
        let b = msg.end.as_ref().map(|b| b.height).unwrap_or_default();
        let (lo, hi, descending) = if a <= b { (a, b, false) } else { (b, a, true) };
        let mut tip = self.tip.load(Ordering::Relaxed);
        if hi > tip {
            tip = self.refresh_tip(c.clone(), conn_id).await?;
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
                // A mixnet stream can drop mid-cell; the channel re-dials through the same transport on the next
                // call, so retry the whole cell (blocks are only cached once a cell is complete and validated).
                // Validation failures (data_loss etc.) are never retried.
                let mut attempt = 0;
                let res = loop {
                    let r = self.cell(c.clone(), conn_id, &kind, nullifiers, &pools, cell, tip).await;
                    match &r {
                        Err(e) if attempt < 2 && is_transport_error(e) => {
                            attempt += 1;
                            warn!(target: "upstream", conn = conn_id, path = "sync", "cell {}..={} failed ({}); retry {attempt}/2",
                                cell.0, cell.1, e.message());
                            tokio::time::sleep(std::time::Duration::from_secs(3 * attempt)).await;
                        }
                        _ => break r,
                    }
                };
                match res {
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
    /// Checks the upstream is reachable (probe connection, then dropped). Sync connections are
    /// opened lazily, one per wallet connection.
    pub async fn connect(
        sync: &Transport,
        scheduler: Arc<Scheduler<FreshConnBroadcaster>>,
        send_mode: SendMode,
        cache: BlockCache,
        events: Arc<EventLog>,
    ) -> anyhow::Result<Self> {
        let mut probe = sync.fresh().await?;
        let tip = probe.client.get_latest_block(ChainSpec {}).await?.into_inner().height;
        info!(target: "upstream", conn = probe.id, transport = sync.label, server = %sync.upstream.uri(), tip, "startup probe ok (connection closed)");
        drop(probe);
        let grid = Arc::new(GridFetcher { cache, tip: AtomicU64::new(tip), events: events.clone(), label: sync.label });
        Ok(Self { events, sync_transport: sync.clone(), sessions: Default::default(), scheduler, send_mode, grid })
    }

    /// The upstream SYNC connection for this wallet connection (opened on first use).
    async fn session(&self, wallet: Option<SocketAddr>) -> Result<(Client, u64), Status> {
        let key = wallet.unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 0)));
        let mut map = self.sessions.lock().await;
        map.retain(|k, s| {
            let keep = s.last_used.elapsed() < SESSION_IDLE;
            if !keep {
                info!(target: "upstream", conn = s.conn_id, path = "sync", wallet = %k, "idle sync session closed");
            }
            keep
        });
        if let Some(s) = map.get_mut(&key) {
            s.last_used = Instant::now();
            return Ok((s.client.clone(), s.conn_id));
        }
        let conn = self
            .sync_transport
            .fresh()
            .await
            .map_err(|e| Status::unavailable(format!("ghost: sync upstream unavailable: {e:#}")))?;
        info!(target: "upstream", conn = conn.id, path = "sync", transport = self.sync_transport.label, wallet = %key,
            "new sync connection for a new wallet session");
        map.insert(key, SyncSession { client: conn.client.clone(), conn_id: conn.id, last_used: Instant::now() });
        Ok((conn.client, conn.id))
    }

    fn log(&self, conn: u64, method: &str, detail: String) {
        info!(target: "upstream", conn, path = "sync", method, "{detail}");
        self.events.record(method, "request", conn, self.sync_transport.label, json!({}));
    }
}

/// Connection-level failures (worth a retry on a re-dialed channel), as opposed to answers or validation errors.
fn is_transport_error(e: &Status) -> bool {
    use tonic::Code;
    matches!(e.code(), Code::Unavailable | Code::Unknown | Code::Cancelled | Code::DeadlineExceeded)
        || (e.code() == Code::Internal && e.message().contains("transport"))
}

fn passthrough_stream<T: Send + 'static>(s: Streaming<T>) -> BoxStream<T> {
    Box::pin(s)
}

macro_rules! unary {
    ($self:ident, $req:ident, $method:ident, $name:literal) => {{
        let (mut c, conn) = $self.session($req.remote_addr()).await?;
        let msg = $req.into_inner();
        $self.log(conn, $name, msg.summary());
        let resp = c.$method(Request::new(msg)).await?;
        Ok(Response::new(resp.into_inner()))
    }};
}

macro_rules! streaming {
    ($self:ident, $req:ident, $method:ident, $name:literal) => {{
        let (mut c, conn) = $self.session($req.remote_addr()).await?;
        let msg = $req.into_inner();
        $self.log(conn, $name, msg.summary());
        let resp = c.$method(Request::new(msg)).await?;
        Ok(Response::new(passthrough_stream(resp.into_inner())))
    }};
}

#[allow(deprecated)] // deprecated nullifier RPCs must still pass through for older wallets
#[tonic::async_trait]
impl CompactTxStreamer for Gateway {
    async fn get_latest_block(&self, r: Request<ChainSpec>) -> Result<Response<BlockId>, Status> {
        let (_, conn) = self.session(r.remote_addr()).await?;
        let resp: Result<Response<BlockId>, Status> = unary!(self, r, get_latest_block, "GetLatestBlock");
        if let Ok(b) = &resp {
            self.grid.tip.store(b.get_ref().height, Ordering::Relaxed);
            self.events.record("GetLatestBlock", "response", conn, self.sync_transport.label, json!({"height": b.get_ref().height}));
        }
        resp
    }
    /// By height: served from its grid cell, so upstream never sees the exact height.
    /// By hash only: passed through (a hash names the block anyway).
    async fn get_block(&self, r: Request<BlockId>) -> Result<Response<CompactBlock>, Status> {
        if r.get_ref().hash.is_empty() {
            let (c, conn) = self.session(r.remote_addr()).await?;
            return Ok(Response::new(self.grid.clone().single(c, conn, r.into_inner().height, false).await?));
        }
        unary!(self, r, get_block, "GetBlock")
    }
    async fn get_block_nullifiers(&self, r: Request<BlockId>) -> Result<Response<CompactBlock>, Status> {
        if r.get_ref().hash.is_empty() {
            let (c, conn) = self.session(r.remote_addr()).await?;
            return Ok(Response::new(self.grid.clone().single(c, conn, r.into_inner().height, true).await?));
        }
        unary!(self, r, get_block_nullifiers, "GetBlockNullifiers")
    }

    type GetBlockRangeStream = BoxStream<CompactBlock>;
    /// Grid-aligned: upstream sees only whole grid cells; the wallet gets exactly its range.
    async fn get_block_range(&self, r: Request<BlockRange>) -> Result<Response<Self::GetBlockRangeStream>, Status> {
        let (c, conn) = self.session(r.remote_addr()).await?;
        Ok(Response::new(self.grid.clone().range(c, conn, r.into_inner(), false).await?))
    }

    type GetBlockRangeNullifiersStream = BoxStream<CompactBlock>;
    async fn get_block_range_nullifiers(
        &self,
        r: Request<BlockRange>,
    ) -> Result<Response<Self::GetBlockRangeNullifiersStream>, Status> {
        let (c, conn) = self.session(r.remote_addr()).await?;
        Ok(Response::new(self.grid.clone().range(c, conn, r.into_inner(), true).await?))
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
        let (mut c, conn) = self.session(r.remote_addr()).await?;
        self.log(conn, "GetTaddressBalanceStream", String::new());
        // Collect first so a client-side stream error fails the call instead of being dropped.
        let addrs: Vec<Address> = r.into_inner().collect::<Vec<_>>().await.into_iter().collect::<Result<_, _>>()?;
        let resp = c
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

#[cfg(test)]
mod tests {
    use super::is_transport_error;
    use tonic::Status;

    #[test]
    fn only_connection_failures_are_retried() {
        assert!(is_transport_error(&Status::unknown("transport error")));
        assert!(is_transport_error(&Status::unavailable("connection reset")));
        assert!(is_transport_error(&Status::internal("h2 transport error")));
        assert!(!is_transport_error(&Status::data_loss("ghost: upstream cell rejected")));
        assert!(!is_transport_error(&Status::out_of_range("above tip")));
        assert!(!is_transport_error(&Status::internal("ghost cache: disk full")));
    }
}

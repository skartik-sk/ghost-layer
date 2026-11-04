#!/usr/bin/env python3
"""Broadcast-pipeline check without spending funds.

Fetches an already-mined transaction through Ghost Layer (sync path) and re-submits its raw
bytes through Ghost Layer's SendTransaction. The upstream must reject it as a duplicate; the
point is to exercise the real pipeline (txid, queue, random delay, fresh connection, Nym
broadcast exit) end to end. This is NOT a payment and is never reported as one.

Usage: python3 -I scripts/pipeline_check.py <shadecheck_dir> [gateway=127.0.0.1:9067]
(uses ShadeCheck's generated lightwalletd stubs; needs grpcio from its venv)
"""
import sys
import time

sys.path.insert(0, sys.argv[1])
import grpc  # noqa: E402
from shadecheck.generated import service_pb2 as pb  # noqa: E402
from shadecheck.generated import service_pb2_grpc as rpc  # noqa: E402

gw = sys.argv[2] if len(sys.argv) > 2 else "127.0.0.1:9067"
stub = rpc.CompactTxStreamerStub(grpc.insecure_channel(gw))
tip = stub.GetLatestBlock(pb.ChainSpec(), timeout=300).height
txid = None
for h in range(tip - 20, tip - 200, -1):
    blk = stub.GetBlock(pb.BlockID(height=h), timeout=300)
    if blk.vtx:
        txid, height = blk.vtx[0].txid, h  # internal byte order, as TxFilter expects
        break
if txid is None:
    sys.exit("no transaction found in the last 200 blocks")
raw = stub.GetTransaction(pb.TxFilter(hash=txid), timeout=300)
print(f"picked mined tx {txid[::-1].hex()} at height {height} ({len(raw.data)} bytes)")
t0 = time.time()
resp = stub.SendTransaction(pb.RawTransaction(data=raw.data), timeout=900)
print(f"SendTransaction returned after {time.time() - t0:.1f} s: code={resp.errorCode} message={resp.errorMessage[:160]!r}")
print("expected: a non-zero code (duplicate/already mined) — the pipeline ran, nothing was paid")

# Broadcast pipeline over Nym — real testnet, no funds spent (2026-11-04T12:09:45Z)

Re-submitted an already-mined testnet tx through `ghost up --nym` (scripts/pipeline_check.py) to exercise the real
broadcast path. **This is not a payment**; upstream correctly rejected it as already committed.

```
2026-11-04T12:08:09.001902Z  INFO upstream: startup probe ok (connection closed) conn=1 transport="nym-sync" server=https://testnet.zec.rocks:443 tip=4581986
2026-11-04T12:08:24.181469Z  INFO upstream: new sync connection for a new wallet session conn=2 path="sync" transport="nym-sync" wallet=127.0.0.1:50779
2026-11-04T12:08:44.341672Z  INFO broadcast: queued; will broadcast on a fresh connection after random delay txid=14434aabc6b892b7201e5f3f80ad0be12392bae628fcc75dcd4b12effa8eb3a5 delay_s=14.702
2026-11-04T12:09:09.069857Z  INFO upstream: fresh connection opened for this tx only conn=3 path="broadcast" transport="nym-broadcast" method="SendTransaction"
2026-11-04T12:09:17.639303Z  INFO broadcast: broadcast done txid=14434aabc6b892b7201e5f3f80ad0be12392bae628fcc75dcd4b12effa8eb3a5 status="rejected" conn=3 transport=nym-broadcast endpoint=https://testnet.zec.rocks:443 scheduled_ms=1793794139042 actual_ms=1793794157639 late_ms=18597 code=-1 msg=any transaction with the same effects will be rejected from the mempool until a chain reset: transaction 
```

`ghost status`:
```
❌ rejected 14434aabc6b892b7201e5f3f80ad0be12392bae628fcc75dcd4b12effa8eb3a5
   received 12:08:44Z  scheduled 12:08:59Z (+14.7 s random delay)  sent 12:09:17Z
   fresh connection #3 via nym-broadcast → https://testnet.zec.rocks:443
   upstream said: any transaction with the same effects will be rejected from the mempool until a chain reset: transaction was committed to the best chain
   🔗 https://testnet.zcashexplorer.app/transactions/14434aabc6b892b7201e5f3f80ad0be12392bae628fcc75dcd4b12effa8eb3a5
   🔗 https://zexplorer.app/testnet/tx/14434aabc6b892b7201e5f3f80ad0be12392bae628fcc75dcd4b12effa8eb3a5
```

- Sync used connection #2 (nym-sync, exit 195.238.126.176); the broadcast used a fresh connection #3 (nym-broadcast, exit 157.254.54.45).
- Random delay 14.7 s (demo profile); `late_ms` = mixnet connect + send time.
- Ghost Layer's ZIP-244 txid (computed locally from raw bytes with zcash_primitives @ eb38d7a) equals the on-chain txid of this NU7 tx.

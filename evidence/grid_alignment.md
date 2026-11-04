# Step 4 evidence: grid-aligned sync (real testnet, 2026-11-04T11:49:24Z)

Upstream: testnet.zec.rocks:443. Grid rule: nym-swizzle-zcash @ e416f724 (spacing max(1152, 144*2^j), wire cells of 1152).

## What the wallets asked Ghost Layer for (never sent upstream)
```
planned grid-aligned fetch wallet_range=4580000..=4581946 emitted=4578048..=4581946 cells=4 spacing=2304
planned grid-aligned fetch wallet_range=4581921..=4581947 emitted=4581504..=4581947 cells=1 spacing=1152
```

## What the upstream server actually received
```
range=4578048..=4579199 (grid cell) conn=1 path="sync" method="GetBlockRange" aligned=true
range=4579200..=4580351 (grid cell) conn=1 path="sync" method="GetBlockRange" aligned=true
range=4580352..=4581503 (grid cell) conn=1 path="sync" method="GetBlockRange" aligned=true
range=4581504..=4581946 (grid cell) conn=1 path="sync" method="GetBlockRange" aligned=true
range=4581504..=4581947 (grid cell) conn=1 path="sync" method="GetBlockRange" aligned=true
```

- Upstream GetBlockRange requests: 5, unaligned: 0
- Every cell start is a multiple of 1152; the last cell ends at the public tip (honest limit: the tip cell is partial).
- Both wallets reached 100% synced (w2: height 4581946 from birthday 4580000; w1: 4581947).

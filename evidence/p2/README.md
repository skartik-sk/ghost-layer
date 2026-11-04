# Priority 2: previously untested paths, now tested for real (testnet, 2026-11-04)

Gateway: `ghost up --listen 127.0.0.1:9077 --send-mode queue --profile demo` (direct mode, own `GHOST_HOME`),
sender: zcash-devtool wallet `w2`, 0.0001 TAZ each to our other wallet.

## `--send-mode queue`: the wallet returns immediately, `ghost status` tracks the real outcome

| txid | wallet call returned | random delay | real outcome (`ghost status`) | mined |
|---|---|---|---|---|
| [`2382433a…6e25177d`](https://testnet.zcashexplorer.app/transactions/2382433aeeed74c7ace9f6bd65738a4d11148a1460165c00082fa56b6e25177d) | 3.6 s (status `pending`) | 11.0 s | `accepted`, fresh connection #3 | height 4582374 |
| [`cae830ee…62857980`](https://testnet.zcashexplorer.app/transactions/cae830eefdddd0b5f9a8fbf1d8b0884b494890ca9b91aa56b202085262857980) | immediately (status `pending`) | 34.8 s | `accepted`, fresh connection #5 | yes |

## Restart-resume: queued tx, ghost killed, restarted after its send time → fresh delay, sent exactly once

1. Queued [`98e28102…8ade22aa`](https://testnet.zcashexplorer.app/transactions/98e28102f21d12c2572f56d64a7bbd2c9a8086ec5ec81ef9c1dc85908ade22aa) at 14:52:35Z with a 10.3 s random delay (scheduled 14:52:45Z); **killed `ghost` immediately**. Queue: `pending`, 0 broadcasts (`resume_1_before_kill.log`).
2. Restarted at 14:52:51Z, after the original send time. It did **not** send instantly; it gave the overdue tx a **fresh random delay** (15 s):
   ```
   14:52:51Z INFO broadcast: resuming queued tx from previous run txid=98e28102… in_s=15
   14:53:07Z INFO upstream: fresh connection opened for this tx only conn=2 path="broadcast"
   14:53:08Z INFO broadcast: broadcast done txid=98e28102… status="accepted" conn=2
   ```
3. Broadcasts of this tx: before kill **0**, after restart **1** → **sent exactly once**. Mined at height **4582390**.

(An earlier attempt at this test didn't kill the gateway: the kill command received two PIDs. That tx was simply sent normally and is listed above as the second queue-mode payment.)

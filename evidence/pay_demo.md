# `ghost pay`: one-command private payment (real testnet, 2026-11-04)

```
ghost pay --wallet ./w1 --identity ./w1.age --to utest1kxp77… --amount 0.001 --memo "ghost pay demo" --nym
```

Terminal recording (`script`; the live countdown line is overwritten in place, a few frames shown):

```
^D👻 starting Ghost Layer (Nym) on 127.0.0.1:9067 …
🔄 syncing wallet through Ghost Layer (grid-aligned) …
✅ synced
💸 sending 0.001 TAZ — Ghost holds it for a random delay, then broadcasts on a fresh connection …
   ⏳ queued a23746a70c69… random delay 12.3 s
   broadcast in  10.0 s        
✅ broadcast accepted after a 12.3 s random delay — fresh connection #4 via nym-broadcast
   txid a23746a70c69ed2bcd63137a9dfffdda234a5e4ac036bbcb4cf67b20645e0d99
⛏  waiting for the next block (~25–75 s) …
🎉 mined at height 4582420 (1 confirmation(s)) — total 134 s
   🔗 https://testnet.zcashexplorer.app/transactions/a23746a70c69ed2bcd63137a9dfffdda234a5e4ac036bbcb4cf67b20645e0d99
   🔗 https://zexplorer.app/testnet/tx/a23746a70c69ed2bcd63137a9dfffdda234a5e4ac036bbcb4cf67b20645e0d99
```

Receiver's viewing key confirms amount **and memo** (`ghost verify --ufvk`):
```
🔑 ironwood incoming — 0.00100000 TAZ (100000 zat), output #0, memo: ghost pay demo
```

`ghost --json pay … --amount 0.0005 --nym` (second real payment) → [`pay_demo.json`](pay_demo.json), validated against `docs/schemas/pay.schema.json` by `cargo test`:
```json
{
    "amount_zat": 50000,
    "broadcast_conn": 4,
    "confirmations": 1,
    "delay_s": 16.979,
    "elapsed_s": 123.59620225,
    "explorer": [
        "https://testnet.zcashexplorer.app/transactions/7f8ce30d189691180fb0e2f4f19d4d016f7cba9cbb1e40dc7156d190607c9579",
        "https://zexplorer.app/testnet/tx/7f8ce30d189691180fb0e2f4f19d4d016f7cba9cbb1e40dc7156d190607c9579"
    ],
    "gateway": "started",
    "height": 4582424,
    "nym": true,
    "ok": true,
    "schema_version": 1,
    "to": "utest1kxp7766pclyr6239xlfa3yn0243ue3uxaumt370mxg4wvstrjdpdp0dz4mm4ff7gz5427ccupmlpm6p2qq84zwa9y99yzxx4nc43ldct",
    "transport": "nym-broadcast",
    "txid": "7f8ce30d189691180fb0e2f4f19d4d016f7cba9cbb1e40dc7156d190607c9579"
}
```

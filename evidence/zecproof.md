# ZecProof: single-payment proof (real testnet, 2026-11-04T15:37:39Z)

Spec B10.2. The **sender** derives the outgoing cipher key (OCK) of one output from their OVK (`PRF_ock(ovk, cv, cmx, epk)`, orchard 0.16 `NoteEncryptionDomain<IronwoodVersion>`); a verifier recovers that output with `try_output_recovery_with_ock` and checks the note commitment against the on-chain `cmx` (the library checks it; we compare again explicitly).

Payment: our `ghost pay` payment [`a23746a7…645e0d99`](https://testnet.zcashexplorer.app/transactions/a23746a70c69ed2bcd63137a9dfffdda234a5e4ac036bbcb4cf67b20645e0d99) (0.001 TAZ, memo "ghost pay demo").

## Create (sender, with their UFVK from a file; it never leaves the machine)
```
$ ghost proof create --ufvk-file sender.ufvk --txid a23746a7…645e0d99 --msg "invoice #42"
🧾 ZecProof for 0.001 TAZ to utest1kxp7766pclyr62… (action #0, ironwood, height 4582420)
zecproof1eyJ2IjoxLCJuZXR3b3JrIjoidGVzdCIsInBvb2wiOiJpcm9ud29vZCIsInR4aWQiOiJhMjM3NDZhNzBjNjllZDJiY2Q2MzEzN2E5ZGZmZmRkYTIzNGE1ZTRhYzAzNmJiY2I0Y2Y2N2IyMDY0NWUwZDk5IiwiYWN0aW9uX2luZGV4IjowLCJvY2tfaGV4IjoiYThiMmU2ZWM5ODJhMzg0YTdlYjI4Y2EyYTc3MmM1MWY2NTNkMjYyMjE5NmYxNDNhNGIwNWNiYzEwYTI4ZTg2ZiIsIm1zZyI6Imludm9pY2UgIzQyIn0
   reveals only this one output; your viewing key stayed on this machine
```

## Verify (anyone)
```
$ ghost proof verify zecproof1…
✅ VALID ZecProof: a payment of 0.00100000 TAZ (100000 zat) was made
   to        utest1kxp7766pclyr6239xlfa3yn0243ue3uxaumt370mxg4wvstrjdpdp0dz4mm4ff7gz5427ccupmlpm6p2qq84zwa9y99yzxx4nc43ldct
   memo      ghost pay demo
   tx        a23746a70c69ed2bcd63137a9dfffdda234a5e4ac036bbcb4cf67b20645e0d99 (ironwood action #0), mined at height 4582420
   message   invoice #42  (label only: NOT signed, anyone holding the proof can change it)
   note commitment matches the chain; only this one output was revealed
   ⚠️  proves the payment happened, not who is presenting this proof
   🔗 https://testnet.zcashexplorer.app/transactions/a23746a70c69ed2bcd63137a9dfffdda234a5e4ac036bbcb4cf67b20645e0d99
   🔗 https://zexplorer.app/testnet/tx/a23746a70c69ed2bcd63137a9dfffdda234a5e4ac036bbcb4cf67b20645e0d99
```

Decoded proof payload: `{"v":1,"network":"test","pool":"ironwood","txid":"a23746a70c69ed2bcd63137a9dfffdda234a5e4ac036bbcb4cf67b20645e0d99","action_index":0,"ock_hex":"a8b2e6ec982a384a7eb28ca2a772c51f653d2622196f143a4b05cbc10a28e86f","msg":"invoice #42"}`

## Forgery attempts (all rejected)

| Attempt | Result |
|---|---|
| OCK changed by one hex digit | ❌ INVALID: the OCK does not decrypt this output, or its note commitment does not match the chain |
| same OCK applied to another of our txs | ❌ INVALID: the OCK does not decrypt this output, or its note commitment does not match the chain |
| same OCK, other action (#1) | ❌ INVALID: the OCK does not decrypt this output, or its note commitment does not match the chain |
| claims mainnet | ❌ INVALID: unsupported proof (v1, network main) |
| garbage string | ❌ INVALID: bad base64url encoding |
| the **receiver's** UFVK tries to create a proof | ❌ this UFVK did not send any shielded output in that tx (or not that --action) |

## Honest limits

- The `msg` field is a **label, not signed**: anyone holding the proof can edit it and the proof still verifies (`msg_signed: false` in JSON). Only the payment facts (recipient, amount, memo, height) are cryptographically checked.
- A proof shows a payment happened; it does not prove who presents it (transferable).
- It reveals exactly one output (recipient, amount, memo) to whoever sees the proof. Full ZIP 311 payment disclosures are future work.

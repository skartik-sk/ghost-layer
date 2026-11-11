# zip311

**Prove you paid someone in shielded Zcash, without revealing anything else.**

A [ZIP 311](https://zips.z.cash/zip-0311) payment disclosure is a short string (`zpd1…`) that the
**sender** of a transaction creates and anyone can check against the chain. It proves:

- **who sent it**: the discloser controls the spend authority of a note the transaction spent.
  This uses a fresh zero-knowledge proof and a signature; no address or key is revealed.
- **what was paid**: for the outputs they choose, the recipient, amount and memo.

It reveals nothing else: no viewing key, no sender address, no other outputs, no other
transactions. An optional message (for example the verifier's challenge) is signed into it, so the
disclosure cannot be edited or re-used for a different challenge.

| | supported |
|---|---|
| Orchard / Ironwood spend authority | ✅ |
| Orchard / Ironwood / Sapling outputs | ✅ |
| Sapling spend authority, transparent inputs, ZIP 304 address proof | not yet |

ZIP 311 is a Draft and specifies only Sapling. The Orchard construction and the byte/text encoding
used here are written up in [SPEC.md](SPEC.md) as a proposal for the ZIP.

## Use

```toml
zip311 = "0.1"
```

**Sender (wallet; holds the spending key):**

```rust,ignore
let data = zip311::TxData::from_tx(&tx, &network, mined_height);
let outputs = zip311::sent_orchard_outputs(&data, &fvk.to_ovk(Scope::External)); // payments, not change
let spend = zip311::OrchardSpend {
    pool: zip311::Pool::Ironwood, index, fvk, ask, note,
    merkle_path, // the note's witness at the transaction's anchor
};
let pd = zip311::create(&data, zip311::coin_type(&network), b"invoice 42".to_vec(), outputs, &[spend], rand::rng())?;
println!("{}", pd.encode()); // zpd1…
```

**Anyone (no keys):**

```rust,ignore
let pd = zip311::PaymentDisclosure::decode(text)?;
let tx = /* fetch pd.txid_hex() from a lightwalletd / node, check it is mined */;
let v = zip311::verify(&pd, &zip311::TxData::from_tx(&tx, &network, height), zip311::coin_type(&network))?;
for o in v.outputs { println!("{} zat to {:?}", o.value_zat, o.recipient); }
```

Proving takes a few seconds, and verifying about one. The Halo2 keys are built once per process.
With `default-features = false` the crate does not depend on `zcash_primitives`, so you can fill
`TxData` from your own librustzcash version (it needs only `orchard` and `sapling-crypto` types).

## Try it on testnet

- Create: `zcash-devtool wallet disclose --identity w.age --txid <txid> --msg "…"`. This is a
  small patch to zcash-devtool, linked from Ghost Layer.
- Verify: `ghost proof verify zpd1…` from [Ghost Layer](https://github.com/skartik-sk/ghost-layer).

## Tests

`cargo test -p zip311 --release` builds a real Ironwood bundle with Halo2 proofs, discloses the
payment, verifies it, and checks that every forgery and tampering case is rejected: changed
message, stripped spend, swapped `rk`/`cv`, wrong action, someone else's key, wrong witness, wrong
network.

## Security

This is not audited. The spend-authority construction reuses the Orchard circuit and RedPallas
unchanged; see SPEC.md for the argument. A valid disclosure shows that whoever made it controlled
the spend authority. It does not show who is presenting it. Bind a fresh challenge in `msg` to stop
replay.

MIT licence.

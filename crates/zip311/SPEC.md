# ZIP 311 for Orchard and Ironwood, plus an encoding: a proposal

[ZIP 311](https://zips.z.cash/zip-0311) (Draft) defines payment disclosures for Sapling and leaves
three TODOs: *Add support for Orchard*, *Decide on payment disclosure versioning*, and *Define
encodings for unsigned and signed payment disclosures*. This document records the choices the
`zip311` crate makes for them. Everything else follows the ZIP as written.

## 1. Orchard (and Ironwood) spend authority

Orchard has no separate Spend description: spends and outputs are combined into Actions, proven by
one Halo2 circuit. A disclosure entry for an Orchard-protocol spend is:

| field | size | meaning |
|---|---|---|
| `pool` | 1 | 1 = Orchard, 2 = Ironwood |
| `index` | 4 (LE) | index into the transaction's actions for that pool |
| `cv_net` | 32 | value commitment of the fresh action |
| `rk` | 32 | randomised spend validating key of the fresh action |
| `cmx` | 32 | note commitment of the fresh action's output |
| `proof` | CompactSize + bytes | Halo2 proof of one action |
| `spendAuthSig` | 64 | RedPallas SpendAuth signature, appended after the unsigned encoding |

**Creating.** The sender builds a new one-action bundle, with the same pool, protocol version and
flags as the transaction's bundle, that spends the same note **at the transaction's anchor**. This
reuses the witness the wallet already used, as the ZIP does for Sapling. The note gives the same
nullifier `nf`, and a fresh `α` gives `rk = SpendAuthSig.RandomizePublic(α, ak)`. The action's
output is the builder's zero-valued dummy, or the fabricated self-output when cross-address
transfers are disabled. The sender proves the action, encodes the unsigned disclosure, and signs
`digest` with `rsk = SpendAuthSig.RandomizePrivate(α, ask)`.

**Verifying.** For the entry at `index`, take `anchor` and `flags` from the transaction's bundle
and `nf` from `tx.actions[index]`. Check that `rk` is not the identity. Verify `proof` against the
instance `(anchor, cv_net, nf, rk, cmx, enableSpends, enableOutputs, disableCrossAddress)` with the
circuit version that the bundle's protocol version selects. Then verify `spendAuthSig` under `rk`
on `digest`.

Why this works. The circuit shows that whoever made the proof knows a note in the tree at
`anchor`, its nullifier is `nf`, and its spend validating key re-randomises to `rk`. The signature
shows they know the matching `ask`. So the discloser controls the spend authority of a note this
transaction spent, which is exactly what ZIP 311 asks for. Because `α` is fresh, `rk` cannot be
linked to the `rk` in the transaction. The anchor and `nf` are already public, so nothing new about
the sender is revealed.

Orchard and Ironwood use the same circuit. Ironwood actions are recovered with the Ironwood note
encryption domain (V3 note plaintexts).

## 2. Outputs

Each output entry is `pool:u8 || index:u32le || ock:[32]`. Pool 0 means Sapling (the index is into
`shieldedOutputs`). Pools 1 and 2 mean Orchard and Ironwood (the index is into actions). The
verifier recovers the note with the OCK and checks that its commitment equals the on-chain
`cmu`/`cmx`.

## 3. Encoding and versioning

```text
unsigned = version:u8 (= 1) || coinType:u32le || txid:[32] || msg:CompactSize+bytes
        || nOutputs:CompactSize || output*
        || nSpends:CompactSize  || spend* (without signatures)
signed   = unsigned || spendAuthSig:[64]*  (one per spend, in order)
digest   = BLAKE2b-256(personalization = "ZIP311Signed" || coinType:u32le, unsigned)
text     = "zpd1" || base64url-without-padding(signed)
```

- `txid` uses internal byte order, the reverse of the usual display.
- `coinType` is part of the encoding, not only of the digest, so a verifier can reject a
  disclosure from another network before it fetches anything.
- CompactSize values must be canonical. Decoders should cap sizes; this crate caps `msg` at
  4096 bytes, items at 256 and proofs at 64 KiB.
- A future format bumps `version` and the `zpd` digit together.
- Bech32m is not used because one Orchard proof is about 5 KB, which is above Bech32m's length
  limit. base64url keeps the string copy-pasteable and URL-safe.

## 4. Not covered (yet)

- **Sapling spend authority.** This needs a Groth16 Spend proof and the Sapling parameters.
- **Transparent inputs.** BIP 322 is itself still a TODO in the ZIP.
- **ZIP 304 sender-address proofs** (optional in the ZIP).

A disclosure must still prove at least one spend, as the ZIP requires. So today a disclosure can
only be made for transactions that spend at least one Orchard or Ironwood note.

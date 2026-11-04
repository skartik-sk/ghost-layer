# Next steps (30 days)

1. **Native Nym hybrid by default:** `nym-smoldvpn` for fast sync and `nym-smolmix` for broadcast, with a fresh mixnet identity per broadcast and no separate client binary. Benchmark against direct and SOCKS (mixnet connection setup was 5–84 s in our runs). Docs: <https://nym.com/docs/developers/smolmix>.
2. **Upstream feedback:**
   - `nym-swizzle-zcash` (<https://github.com/nymtech/nym/tree/develop/sdk/rust/nym-swizzle-zcash>): report how a *gateway* uses `grid::quantize` (no wallet-side scheduler possible) and propose a gateway-mode helper; ask about grid-aligning `GetTreeState`/`GetSubtreeRoots`.
   - Share the split-broadcast + random-delay design with zecmix (<https://github.com/apeirs/zecmix>), whose README lists `--split` and random delay as future plans.
   - ShadeCheck (<https://github.com/0xaje/ShadeCheck->): SC-002 currently flags every non-empty broadcast regardless of connection or timing. Propose a session-aware variant (broadcast on a connection that also synced, or within N s of a tip event), so a split/delayed broadcast can be distinguished.
3. **"Ghost mode" in a wallet's network layer** (Zingolib / Zakura): build the tx when the delay fires (fixes the expiry-height hint), quantize ranges wallet-side.
4. **ZecProof → ZIP 311** (<https://github.com/zcash/zips/pull/387>): "invisible by default, provable by choice" single-payment proofs.
5. Research ZK attribute proofs over payments (needs Halo2/Pallas-compatible circuits).

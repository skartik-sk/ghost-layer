# Draft issue for nymtech/nym: `nym-swizzle-zcash` from a gateway (not posted yet)

**Title:** nym-swizzle-zcash: feedback from using `grid::quantize` in a lightwalletd gateway (Ghost Layer)

Hi Nym team. We built [Ghost Layer](https://github.com/skartik-sk/ghost-layer), a local lightwalletd-compatible gateway
that any unmodified wallet can point at. It uses `nym_swizzle_zcash::grid::quantize` for sync (crates.io 1.21.6;
`grid.rs` is byte-identical to `develop` @ e416f724) and two `nym-socks5-client`s (sync / broadcast, different exits).

What worked: every upstream `GetBlockRange` was a whole 1152-block cell (5/5 aligned on public testnet), and the
attacker's range-chaining heuristic dropped from 4/5 to 0/5 sessions in our local before/after test.

Feedback / questions:
1. **Gateway mode.** A gateway receives *built* transactions, so `broadcast::Scheduler` (which builds when the delay
   fires) can't be used; we wrote a bounded uniform delay instead. Would a "gateway profile" (delay only, no rebuild)
   be welcome in the crate, with docs on the expiry-height trade-off?
2. **Other height-revealing RPCs.** `GetTreeState(height)` and `GetSubtreeRoots` still reveal exact heights (e.g. the
   birthday). Is there a recommended quantization for those?
3. **Tip cell.** The last cell is capped at the tip, so its size is unique per tip; testers like ShadeCheck flag it.
   Is a tip-padding strategy planned?
4. **Exit churn.** In our runs a pair of exits lasted ~20–30 min before SOCKS timeouts; a health-check helper for
   picking/rotating exits would help integrators fail closed gracefully.

Evidence: https://github.com/skartik-sk/ghost-layer/tree/main/evidence

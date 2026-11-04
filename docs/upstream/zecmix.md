# Draft issue/discussion for apeirs/zecmix (not posted yet)

**Title:** Split broadcast + random delay (your "Future plans"): an implementation you may want to compare against

Hi! Your README lists `--split` mode and an optional random delay before broadcast as future plans. We implemented
both in [Ghost Layer](https://github.com/skartik-sk/ghost-layer), a separate Rust gateway, and measured them:

- every `SendTransaction` on a brand-new TLS/SOCKS connection (never the sync one), through a *different* Nym exit;
- a bounded uniform random delay (10–40 s demo / 60–600 s privacy), restart-safe (sqlite queue, fresh delay after
  restart, sent exactly once);
- grid-aligned sync with `nym-swizzle-zcash`.

On 5 real testnet payments through Nym an honest-but-curious server linked 0/5 by connection, timing, IP or range
chaining (local before/after: 100 % → 0 %). Your README's exit-selection and DNS notes helped us; credited in our README.

Happy to share notes or help bring the split mode to zecmix if useful.

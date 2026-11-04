# Ghost Wallet: an app built on Ghost Layer (real testnet, 2026-11-04)

`ghost dashboard --wallet <dir> --identity <age file> --ufvk-file <sender UFVK file>` serves a 3-tab app on
**127.0.0.1:9070** (loopback only, embedded page, strict CSP, no CDNs):

| Tab | What it does | Code path |
|---|---|---|
| **Pay** | address + amount + memo → live random-delay countdown → explorer link when mined | runs `ghost --json pay` (same code as the CLI); zcash-devtool holds the keys |
| **Proof** | create a ZecProof for a past payment, or paste one to verify | runs `ghost --json proof create/verify` |
| **Live** | what the server sees: exits, queue countdowns, grid, attacker view, before/after | reads `ghost-core` queue, `ghost-attacker` on the event log, `ghost-transport` IP probes |

Size: **339 lines of Rust + 557 lines of HTML/CSS/JS** (non-blank) on top of the Ghost Layer crates.

## Real payment through the UI

Pay tab → `utest1kxp77…`, 0.0005 TAZ, memo "via Ghost Wallet UI":
- live countdown: `⏳ broadcast in 21.0 s (random delay)` → [`wallet-pay-countdown.png`](wallet-pay-countdown.png)
- result: mined at height **4582573** after a 23.0 s random delay, fresh connection #4 via `nym-broadcast`
  → [`4c21d3b2…62dd7fba`](https://testnet.zcashexplorer.app/transactions/4c21d3b27666b6a33a8fa1432f4c07580ace042d969bf8a46346420362dd7fba) ([`wallet-pay-mined.png`](wallet-pay-mined.png))

## Real proof through the UI

Proof tab (txid pre-filled from the payment) → created a ZecProof (Ironwood action #1, the payment output;
#0 is change) → verified: **✅ VALID 0.0005 TAZ, memo "via Ghost Wallet UI", height 4582573**
([`wallet-proof.png`](wallet-proof.png), [`wallet-proof-verify.png`](wallet-proof-verify.png)).

## Guards tested (curl against the running app)

| Request | Response |
|---|---|
| POST /api/pay without `X-Ghost` header (CSRF) | 403 `missing X-Ghost header` |
| amount 5 TAZ (cap 0.01) | 400 `amount above this dashboard's cap of 0.01 TAZ` |
| mainnet address | 400 `recipient must be a testnet address` |
| `Host: evil.example` (DNS rebinding) | 421 |
| malformed proof | 400 `paste a zecproof1… string` |

The wallet, identity and UFVK file are fixed at startup; the page can't choose them.

Other screenshots: [`dashboard.png`](dashboard.png) (Live), [`dashboard-mobile-light.png`](dashboard-mobile-light.png),
[`wallet-pay-mobile-light.png`](wallet-pay-mobile-light.png), [`explorer-after-tx.png`](explorer-after-tx.png) (public explorer: our tx, shielded, v6).

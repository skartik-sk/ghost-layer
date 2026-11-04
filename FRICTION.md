# Friction log

Tagged: [SDK] [testnet] [Nym] [docs] [ShadeCheck]

- [testnet] 2026-11-04 17:30 IST — Valar faucet (`faucet.testnet.valargroup.dev`) refused every claim with
  "the wallet cannot pay this address" (zecd code -5/-8) for a UA (Orchard+Sapling+t), an Orchard/Ironwood-only
  UA, and a transparent address. Its status showed `paidCount: 0` for the day, last payout ~12 days earlier:
  the faucet itself is broken, not the address.
- [testnet] Fauzec API returned `ip_on_cooldown` (shared venue IP already claimed). Funds requested from mentors.
- [SDK] zcash-devtool `wallet init --help` still lists upgrades only up to `nu6_3`, but the code supports NU7
  (`src/data.rs`) and Ironwood. Regtest needs `--features regtest_support` (not in the default build).
- [docs] librustzcash's `zcash_client_backend/proto/*.proto` are symlinks into `lightwallet-protocol`; the
  canonical `zcash/lightwallet-protocol` repo is newer (adds `GetStandardFee`).
- [Nym] No macOS arm64 `nym-socks5-client` binary in release `nym-binaries-v2026.21-hanoi`; built from source.
- [ShadeCheck] SC-002 (as implemented, `shadecheck/core.py:90-114`) raises a finding for *every* non-empty
  `SendTransaction`, regardless of connection or timing; HIGH needs a `GetMempoolStream` match, which
  zcash-devtool never calls. A split/delayed broadcast therefore can't change its verdict — reported as is.
- [ShadeCheck] SC-001 requires every range to be exactly `chunk_size`; a grid cell capped at the tip (regtest
  tip ≈ 400 < 1152) is always flagged for size even when aligned.
- [ShadeCheck] The zingo-cli path needs a zingolib build on Rust 1.99 (we have 1.98), so we drove
  zcash-devtool through the observer instead.
- [SDK] Regtest funding with zcash-devtool: shielding fails with "immature transparent coinbase spend" while
  Zebra mines to the wallet's own address (newest coinbase always immature); mine to another address first.
- [Nym] Opening the first gRPC connection through a fresh nym-socks5-client took ~84 s; later ones 5–20 s.
  The 1.1.86 `credential_requests_database` config quirk did not occur on 1.1.88. No credentials were needed.
- [testnet] `nym-swizzle-zcash`'s broadcast scheduler is wallet-side (builds the tx when the delay fires);
  a gateway receives built txs, so we wrote our own uniform-delay scheduler.

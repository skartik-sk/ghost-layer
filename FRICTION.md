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

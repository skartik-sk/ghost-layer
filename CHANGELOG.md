# Changelog

## v0.2.0 (2026-11-11)

After the Free Society Hack (2nd place, Privacy track) we worked on the gaps the judges scored lowest:
Easy, Continuing and Works. Real runs: [`evidence/v0.2/`](evidence/v0.2/README.md).

### Easier
- **Nym is built in.** The two SOCKS5 mixnet clients run inside `ghost` (nym-sdk 1.21.6); no
  `nym-socks5-client` to build. `ghost up`, `pay` and `init` start them when nothing is running;
  `ghost nym` keeps them up for other wallets.
- **`ghost init`** creates a testnet wallet through Ghost and saves it as the default, so
  `ghost pay --to … --amount …`, `ghost dashboard` and `ghost mcp` need no `--wallet/--identity`.
- **Prebuilt binaries**: `ghost` + `zcash-devtool` for macOS (arm64, x86_64) and Linux x86_64, with
  `scripts/install.sh` (SHA-256 checked).

### More reliable
- Each Nym exit is used only after a real request through it succeeds; a watchdog replaces exits that
  stop carrying traffic.
- **Tor fallback for broadcasts**: if Nym can't get a payment out and a Tor SOCKS port is running, the
  same transaction goes over Tor on a new circuit. Never direct. `--tor`, `--no-tor`.
- The gateway retries a grid cell when a mixnet stream drops; `ghost pay` resumes an interrupted sync;
  broadcasts retry for about 3 minutes on fresh connections.
- CI: tests, a build without the built-in Nym, `clippy -D warnings`, and a check that the Nym pins hold.

### More private
- `GetTreeState` heights (often the wallet birthday) are sent among 3 decoys, one per neighbouring grid
  cell, in random order.
- The shielded pool filter is normalised: upstream always gets Sapling+Orchard+Ironwood and Ghost prunes
  blocks locally.
- Built-in Nym clients use anonymous replies (the SDK default would let the exit learn the client's Nym
  address).
- `ghost init` makes the wallet folder 0700 and the age identity 0600 (zcash-devtool creates it 0644).

### Changed
- `rusqlite` 0.41 → 0.32.1 and `jsonschema` (dev) 0.60 → 0.33, to share `libsqlite3-sys`/`getrandom`
  with nym-sdk. Install from crates.io with `cargo install --locked ghost-layer`.
- `ghost-layer-transport`: `Transport` gained an `isolate` field and a `Transport::tor` constructor.
- `ghost-layer-gateway`: `FreshConnBroadcaster` gained a `fallback` field.

## v0.1.0 (2026-11-04)

Free Society Hack submission: split broadcast, random delay, grid-aligned sync, Nym; ZecProof; Ghost
Wallet; MCP server.

# AGENTS.md: working on Ghost Layer as an AI coding agent

## Rules (non-negotiable)

1. **Never fake anything.** No mocked transactions, hard-coded results, edited reports or invented numbers. Every number in `evidence/` and on the dashboard comes from a real run. Never special-case a testing tool (ShadeCheck) to pass.
2. **Keys stay in the wallet.** Ghost never reads, stores or logs seeds, mnemonics or spending keys. Wallet operations go through zcash-devtool as a subprocess; Ghost only passes the identity *path*. Never commit `wallets/`, `*.age`, or anything from `~/.ghost-layer`.
3. **`main` must stay green.** Before pushing: `cargo build --release && cargo test` (add `GHOST_ONLINE_TESTS=1` when online), `scripts/e2e_local.sh` and `scripts/e2e_testnet.sh` must pass. Never push a failure.
4. **Testnet only.** NU7 is active (branch `0x77190AD9`, v6 txs, v4 invalid). Orchard is sealed: shielded funds are in **Ironwood**. Server `testnet.zec.rocks:443`.
5. **Privacy regressions are bugs:** no wallet metadata upstream, no broadcast on a sync connection, no direct fallback in `--nym` mode, no addresses/txids in the request log, user IP masked in output.

## Build & test

```bash
cargo build --release                         # target/release/ghost
cargo test                                    # unit + JSON-contract tests (offline)
GHOST_ONLINE_TESTS=1 cargo test -p ghost-cli --test json_contract   # + real testnet checks
cargo clippy --release
scripts/gen_llms_full.sh                      # after changing CLI help or schemas
```

End-to-end (real money on testnet, real local regtest network):

```bash
# local before/after: ShadeCheck observer + Ghost Layer + attacker (needs Docker, ShadeCheck, funded regtest wallets)
SHADECHECK_DIR=… GHOST_REGTEST_WALLETS=… ZCASH_DEVTOOL=… scripts/e2e_local.sh
# real testnet payment through Ghost Layer, verified on-chain
scripts/nym_setup.sh up
MODE=nym WALLET_DIR=… IDENTITY=… TO_ADDRESS=utest1… ZCASH_DEVTOOL=… scripts/e2e_testnet.sh
```

## Layout

| Path | What |
|---|---|
| `proto/` | vendored lightwallet protocol (zcash/lightwallet-protocol ec53075) |
| `crates/ghost-core` | `grid` (nym-swizzle-zcash quantize + cell cache), `broadcast` (sqlite queue, random delay, restart-safe), `events` (upstream event log). No network code. |
| `crates/ghost-transport` | direct TLS + Nym SOCKS5 connector, `public_ip`, generated protocol |
| `crates/ghost-gateway` | tonic `CompactTxStreamer` server (all 21 RPCs) |
| `crates/ghost-attacker` | linkage heuristics over an observer/event log |
| `crates/ghost-cli` | `ghost` binary: `up pay status verify ip doctor explain attack report dashboard mcp` |
| `scripts/` | `nym_setup.sh`, `e2e_local.sh`, `e2e_testnet.sh`, `regtest_payment.sh`, `pipeline_check.py`, `gen_llms_full.sh` |
| `evidence/` | committed real outputs (never edit by hand except clearly-labelled interpretation sections) |
| `docs/schemas/` | JSON Schemas of every `--json` output |

## Conventions

- Match the surrounding style; small, simple changes; no unrequested refactors.
- Every error: exit code (1 runtime, 2 usage, 3 rejected) + a "→ what to do next" hint; `--json` everywhere with `schema_version`.
- New `--json` fields: update `docs/schemas/` and the contract test in the same commit.
- Upstream text is untrusted: strip control characters before printing.

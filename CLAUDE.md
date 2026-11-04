# CLAUDE.md

Read [AGENTS.md](AGENTS.md) first: it holds the rules, build/test commands and layout for this repo.

Short version:
- Never fake results; every number comes from a real run.
- Keys stay in the wallet (zcash-devtool); Ghost never reads seeds or spending keys.
- `main` stays green: `cargo test`, `scripts/e2e_local.sh` and `scripts/e2e_testnet.sh` before pushing.
- Zcash testnet only; NU7 active; shielded funds are in Ironwood (Orchard sealed); server `testnet.zec.rocks:443`.
- Using Ghost Layer *from* an agent: see [skills/ghost-layer/SKILL.md](skills/ghost-layer/SKILL.md) and `ghost mcp`.

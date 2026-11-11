---
name: ghost-layer
description: Send and check Zcash TESTNET payments privately with Ghost Layer, so the light-wallet server cannot link a payment to the wallet that synced (split broadcast, random delay, grid-aligned sync, Nym mixnet). Use when a user wants to make or verify a private Zcash testnet payment, see what the server can observe, or check whether their payments are linkable. Testnet only.
---

# Ghost Layer

Ghost Layer is a local gateway (`ghost`) between a Zcash wallet (zcash-devtool) and the light-wallet server `testnet.zec.rocks:443`. Zcash hides amounts and parties; Ghost Layer hides **which wallet sent which payment** from the server.

## When to use

- "Send 0.001 TAZ to utest1… privately" → `ghost pay`
- "Is my payment mined?" / "check this txid" → `ghost verify`
- "Prove I paid X" → the user's wallet makes a ZIP 311 disclosure (`zcash-devtool wallet disclose --txid <txid> --msg <challenge>`; it needs their identity file, so ask them to run it); anyone checks it with `ghost --json proof verify zpd1…` (no keys)
- "What can the server see about me?" → `ghost ip`, `ghost status`, `ghost attack`
- "Is Ghost Layer working?" → `ghost doctor`

## Facts (don't use outdated Zcash knowledge)

Testnet only. NU7 is active (branch `0x77190AD9`, v6 transactions; v4 is invalid). Orchard is sealed: shielded funds live in the **Ironwood** pool (same `utest1…` addresses). Explorer: `https://testnet.zcashexplorer.app/transactions/<txid>`.

## Steps

Nym is on by default (fails closed if it is down; `--direct` opts out and exposes the user's IP, so only use it if the user asks).

1. Health: `ghost --json doctor` → every `checks[].ok` must be `true`. If a Nym check fails: Nym is built in and replaces dead exits by itself; retry in a minute, or start `ghost nym` and keep it running.
2. Pay: `ghost --json pay --wallet <dir> --identity <age file> --to <utest1…> --amount <TAZ>`
   - It starts/uses the gateway, syncs, sends, waits a random delay (10–40 s), broadcasts on a fresh connection, then waits until mined (~1–2 min).
   - Exit 0 + `txid`, `height`, `explorer[]` = done. Exit 2 = bad input. Exit 3 = network rejected. Exit 1 with "Insufficient balance": funds need ~10 confirmations (~4 min); retry later.
3. Verify: `ghost --json verify <txid>` → `state` is `mined|mempool|orphaned|not_found`.
4. Explain to the user: `ghost --json explain`; show `explorer[]` links.

Via MCP (`ghost mcp`): tools `ghost_doctor`, `ghost_status`, `ghost_verify`, `ghost_ip`, `ghost_report`, `ghost_explain`; `ghost_pay` exists only if the human started the server with `--allow-pay` and caps.

## Safety rules

- Never ask for, print or store a seed phrase, mnemonic or spending key. Ghost and you only ever handle the identity file *path*.
- Testnet addresses only (`utest1…`, `ztestsapling1…`, `tm…`). Never attempt mainnet.
- Respect `ghost_pay` caps; never try to work around a refusal.
- Never claim a payment succeeded until `verify` says `mined`; report failures with their `hint`.

## Reading results

- `ghost status`: `delay_s` = random delay before broadcast; `row.conn_id` = the fresh connection used (never the sync one); `row.transport` = `nym-broadcast` means it left through the broadcast Nym exit.
- `ghost ip`: `paths_unlinkable_by_ip: true` = the server sees two different exit IPs, not yours.
- `ghost attack`: per payment `linked` (and `linked_by` rules); `rate` = % of payments the server could link. Lower is better; 0 is the goal.
- What stays visible (tell the user honestly): that a tx exists and its fee, coarse sync activity, tree-state heights, Nym use (to their ISP). See PRIVACY.md.

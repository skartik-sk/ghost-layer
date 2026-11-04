# `--json` contract (schema_version 1)

Every `ghost` command accepts `--json` and prints **one JSON object on the last line of stdout**.
Logs go to stderr. Every object has `"schema_version": 1` and `"ok": <bool>`.
Machine-readable schemas (JSON Schema 2020-12) live in [`docs/schemas/`](schemas/), and
`crates/ghost-cli/tests/json_contract.rs` validates the **real** output of the binary against them
(`cargo test -p ghost-cli --test json_contract`; add `GHOST_ONLINE_TESTS=1` for the testnet checks).

## Exit codes

| Code | Meaning |
|---|---|
| 0 | ok (for `verify`/`pay`: the tx is mined) |
| 1 | runtime failure (network, upstream, Nym client down, or `verify`: not mined yet) |
| 2 | usage / config error (bad txid, bad amount, missing wallet…) |
| 3 | the network rejected the transaction (`pay`) |

## Shapes

| Command | Schema | Key fields |
|---|---|---|
| any failure | [`error`](schemas/error.schema.json) | `ok:false`, `error`, `hint` (what to do next) |
| `ghost up` (first line) | [`up`](schemas/up.schema.json) | `listen`, `sync_upstream`, `broadcast_upstream`, `nym`, `profile`, `policy{min_delay,max_delay}`, `send_mode`, `resumed_pending`, `held_nym_only` |
| `ghost pay` | [`pay`](schemas/pay.schema.json) | `txid`, `amount_zat`, `to`, `gateway: existing\|started`, `nym`, `height`, `confirmations`, `delay_s`, `broadcast_conn`, `transport`, `elapsed_s`, `explorer[]` |
| `ghost status` | [`status`](schemas/status.schema.json) | `broadcasts[]{row{txid,status,received_at_ms,scheduled_at_ms,sent_at_ms,conn_id,transport,endpoint,error_code,error_message,mode}, delay_s, explorer[]}` |
| `ghost verify <txid>` | [`verify`](schemas/verify.schema.json) | `state: mined\|mempool\|orphaned\|not_found`, `height`, `confirmations`, `tip`, `explorer[]` |
| `ghost ip` | [`ip`](schemas/ip.schema.json) | `nym`, `your_ip` (masked), `sync_path_ip`, `broadcast_path_ip`, `paths_unlinkable_by_ip`, `sync_exit`, `broadcast_exit` |
| `ghost doctor` | [`doctor`](schemas/doctor.schema.json) | `checks[]{name, ok, detail}` |
| `ghost explain` | [`explain`](schemas/explain.schema.json) | `leaks[]{leak, what, why, ghost_layer}`, `not_hidden[]` |
| `ghost attack --events f` | [`attack`](schemas/attack.schema.json) | `sends[]{index, linked, linked_by[], same_connection, sync_then_send, same_ip, tip_to_send_s}`, `rate` (percent), `sync_sessions`, `fingerprint_linked_sessions`, `median_tip_to_send_s` |
| `ghost report --attack …` | [`report-attack`](schemas/report-attack.schema.json) | `out`, `before`, `after`, `nym` (analyses as above) |
| `ghost report --before d --after d` | [`report`](schemas/report.schema.json) | `out` |
| `ghost dashboard` (first line) | [`dashboard`](schemas/dashboard.schema.json) | `url`, `events` |

`ghost dashboard` also serves `GET /api/state` (`schema_version` 1): `now_ms`, `server_sees`, `queue[]`,
`grid{ranges[],aligned,unaligned}`, `attacker{sends[],rate (0–1)}`, `before_after{before_rate,after_rate}`; any part may be `null` ("no data yet").

Breaking changes bump `schema_version`; new fields may be added within a version.

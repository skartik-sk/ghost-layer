# v0.2 evidence: built-in Nym, Tor fallback, ghost init (Zcash testnet, 2026-11-11)

All runs on public testnet (`testnet.zec.rocks:443`, NU7) with the `improve/easy` build. No external
`nym-socks5-client`: Nym ran inside `ghost` (nym-sdk 1.21.6). Your IP is masked as `49.43.x.x`.

## Mined payments

| txid | sync | broadcast | random delay | result |
|---|---|---|---|---|
| [`2be9df6e…ccad8ed3`](https://testnet.zcashexplorer.app/transactions/2be9df6eb8b5aff03cd3b64157ae7370a85e7f322a746acfc6809bcaccad8ed3) | built-in Nym | **Tor fallback**: the Nym broadcast connection was closed by the peer mid-upload, so the same tx went out over Tor on a new circuit | 22.2 s | mined at 4607658, 205 s end to end |
| [`9a842e90…ebd36996`](https://testnet.zcashexplorer.app/transactions/9a842e90c82f9e278ca0d76f7206254c38f3bbc604e565131044a4ecebd36996) | built-in Nym | built-in Nym (anonymous replies), `--no-tor` | 12.9 s | mined at 4607706, 152 s end to end |

Gateway log lines for the two broadcasts:

```
09:26:30 upstream: fresh connection opened for this tx only conn=4 path="broadcast" transport="nym-broadcast"
09:27:24 WARN broadcast: nym-broadcast could not reach the network (transport error … peer closed connection …)
09:27:25 upstream: fresh connection opened for this tx only conn=5 path="broadcast" transport="tor-broadcast"
09:27:26 broadcast done txid=2be9df6e… status="accepted" conn=5 transport=tor-broadcast

09:45:02 upstream: fresh connection opened for this tx only conn=4 path="broadcast" transport="nym-broadcast"
09:45:06 broadcast done txid=9a842e90… status="accepted" conn=4 transport=nym-broadcast
```

`ghost ip` with the built-in clients: the server saw `85.17.162.41` on SYNC and `5.188.181.140` on BROADCAST
(two Nym exits; three different IPs including yours).

## Failures we hit (and what changed because of them)

- **Mixnet stream dropped mid-sync** (26 of 32 grid cells): the gateway now retries a cell on a re-dialed
  connection, and `ghost pay` resumes an interrupted sync.
- **Dead exits.** Over the day, 20+ Nym exits accepted a SOCKS connection but never carried traffic, and the Nym
  directory API timed out once. Exits are now used only after a real `GetLatestBlock` through them, and a watchdog
  replaces exits that stop working.
- **Three payments were not sent** (`a7caf224…`, `119624a6…`, `ef88769f…`): the broadcast exit failed and no
  working replacement was found within the retry window. Each time Ghost **failed closed**: nothing was sent
  directly, the hint says so, and the wallet's notes return when those txs expire. This is why the Tor fallback
  exists; with it, the next payment went through.
- The SDK's SOCKS5 default is `send_anonymously: false` (the exit would learn the client's Nym address). The
  built-in clients now set it to `true`, matching `nym-socks5-client --use-reply-surbs true` from v0.1.
- nym-socks5-client-core 1.21.6 panics when its tasks are dropped during runtime shutdown; `ghost` now exits
  directly once a command is done (event log and queue are written through, so nothing is lost).

## Speed

Sync over the built-in Nym: about **4 s per 1152-block grid cell** (~290 blocks/s) when the stream is healthy.
Starting the two clients and testing their exits takes 1-2 minutes. Tor bootstrapped in under 10 s.

## ghost init

```
$ ghost init --direct --listen 127.0.0.1:19067     # (Nym clients were not running for this first test)
👻 starting Ghost Layer on 127.0.0.1:19067 …
🔑 creating a testnet wallet (zcash-devtool makes the seed and the age identity; Ghost never reads them) …
✅ wallet ready: …/wallet
```

4.6 s. Wallet dir 0700, `identity.age` 0600 (zcash-devtool creates it 0644), `ufvk.txt` 0600.

## Tree-state decoys and the pool filter (privacy)

Testnet sync of the `ghost init` wallet through `ghost up` (Nym). The wallet asked for one tree state (its
birthday, `4607256`); the upstream log shows four, one per consecutive grid cell, in random order:

```
upstream: height=4604894 (one of 4, grid-covered) conn=4 path="sync" method="GetTreeState"
upstream: height=4606240 (one of 4, grid-covered) conn=4 path="sync" method="GetTreeState"
upstream: height=4603572 (one of 4, grid-covered) conn=4 path="sync" method="GetTreeState"
upstream: height=4607256 (one of 4, grid-covered) conn=4 path="sync" method="GetTreeState"
```

Cells 3996, 3997, 3998, 3999 (height / 1152). A unit test checks over 2000 draws that the real cell lands in
every position of the window. Regtest chains shorter than 4 cells skip the cover (too few cells to hide in).

The shielded pool filter is normalised: upstream always gets Sapling+Orchard+Ironwood (+transparent only if
the wallet asks for it) and Ghost prunes each block locally to the wallet's set (unit-tested).

`scripts/e2e_local.sh` with these changes: 5 + 5 real regtest payments, linkage **5/5 → 0/5**, 0 unaligned
`GetBlockRange` (`evidence/runs/20261111T100652Z`). An earlier run the same day (Tor fallback build) measured
**4/5 → 0/5** (`evidence/runs/20261111T100223Z`): one "before" payment did not trip the heuristics.

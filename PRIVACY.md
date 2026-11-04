# Privacy & trust note

## 1. What stays private?

- **On-chain:** sender, receiver, amount and memo of shielded (Ironwood) payments. That is Zcash itself; Ghost Layer doesn't touch keys or transactions.
- **Network, with Ghost Layer:**
  - Broadcasts **never share the sync connection**: every `SendTransaction` uses a brand-new TCP/SOCKS5 + TLS + HTTP/2 connection that is closed afterwards.
  - Broadcasts happen after a **random delay** (demo 10–40 s, privacy 60–600 s), never right after the wallet reaches the tip. Measured locally: median tip→broadcast went from 3.0 s to 22.4 s, and sync-then-send linkage from 5/5 to 0/5.
  - **Grid-aligned sync**: `GetBlockRange` and `GetBlock(height)` reach the server only as whole 1152-block grid cells (Nym's `nym-swizzle-zcash` rule), so the exact resume point isn't revealed (range chaining 4/5 → 0/5 sessions).
  - Each wallet connection gets its own upstream sync connection, so separate sessions aren't chained by a shared socket.
  - **With `--nym`:** the server sees two *different* Nym exit IPs for sync and broadcast, never yours (measured: `195.238.126.176` ≠ `157.254.54.45`). DNS is resolved by the exit. It **fails closed**: if a Nym client is down, the wallet gets `UNAVAILABLE`, never a direct connection. `ghost doctor --nym` and `ghost verify --nym` also contact the server only via Nym (verify uses the broadcast exit, never the sync exit, so a txid lookup cannot link the two).
  - Wallet request headers (User-Agent, `X-Forwarded-For`, `Forwarded`, `Via`, …) are never forwarded. Requests are rebuilt from the message body.

## 2. What does the app reveal, and to whom?

| To whom | What |
|---|---|
| Light-wallet server | that a client syncs, and which grid cells it fetches (coarse, not zero; the tip cell is partial); tree-state / subtree-root heights (`GetTreeState`, `GetSubtreeRoots`: these can hint at the wallet birthday); the pool-type filter a wallet sends; transparent-address queries (`GetAddressUtxos`, `GetTaddress*`); the raw transaction at broadcast time; `GetTransaction` lookups. |
| Server, **without Nym** | your IP on both paths (same IP → linkable; our attacker test reports this honestly as 5/5 linked). |
| Nym | the exit knows *some* Nym user talks to the server; your ISP sees that you use Nym. |
| Chain (everyone) | that a transaction exists, its fee and its expiry height. Note that the expiry height is set by the wallet when it builds the tx, *before* Ghost Layer's delay, so it can hint at the wallet's sync height (swizzle's wallet-side scheduler avoids this; a gateway can't). |
| Local disk (`~/.ghost-layer`) | broadcast queue (txid, timing, status; raw bytes are wiped once final) and a compact-block cache. Protect your machine; delete the directory to erase. The stderr log contains txids of your broadcasts (required for verifiability) but no addresses. |

## 3. What must users trust?

The light-wallet server for data integrity (Ghost Layer checks cells are contiguous and hash-linked, but cannot validate consensus); the Ghost Layer code; the wallet (keys never leave it); Nym, if enabled; their own machine and OS.

## 4. What would we fix next?

Native Nym hybrid by default (smoldvpn for fast sync, smolmix for broadcast); cover traffic for sync; grid-aligning tree-state queries; normalising the pool-type filter; multi-server destination splitting by default; birthday-height leakage; wallet-side integration so the tx is *built* when the delay fires (fixes the expiry-height hint); mobile; payment proofs (ZIP 311).

---

- **ShadeCheck PASS ≠ anonymity.** In our runs ShadeCheck still FAILs: SC-001 because the tip cell is shorter than a full cell, and SC-002 because this version flags *every* broadcast. We report those results unchanged, see `evidence/BEFORE_AFTER.md`.
- Global passive adversaries watching the whole internet are out of scope.

# Final pre-submission audit (main, 2026-11-05 ~00:10 IST)

Verified against the real repo state on `main`. ✅ pass · ❌ fail · ⚠️ risk.

## A. Event rules
| | Check | Result | Evidence |
|---|---|---|---|
| A1 | Fresh repo, first commit after 11:00 4 Nov; work before cutoff | ✅ / ⚠️ | first commit `2026-11-04 16:58 Step 1: scaffold…`; 2 landing-page-only commits after 00:00 (`00:00`, `00:05`), inside the organisers' 00:20 extension |
| A2 | No copied code | ✅ | dependencies only (crates.io); zecmix / ShadeCheck / Nym credited in README "Prior work & credits"; ShadeCheck is run as an external tool, its reports unmodified |
| A3 | No secrets in files or history | ✅ | `git log -p --all`: AGE-SECRET-KEY 0, mnemonic/seed 0, secret-extended-key 0, API keys 0, user IP 0; tracked `.age`/wallet/sqlite files 0; all "your real IP" lines masked (`x.x`) |
| A4 | Public repo, README renders | ✅ | `gh repo view` → PUBLIC |
| A5 | Final main pushed | ✅ | local = origin/main (hash in the final commit message of this file) |

## B. Privacy track
| | Check | Result | Evidence |
|---|---|---|---|
| B1 | Testnet, NU7 (0x77190AD9), unified addresses, Ironwood | ✅ | `evidence/testnet_tx.md` (raw tx headers: v6, branch 0x77190AD9); `ghost doctor` "consensus branch 77190ad9"; `evidence/ufvk_verify.md` (ironwood incoming) |
| B2 | Stadium txid real and mined | ✅ | `ghost verify 09f0f0b0…ce69` → mined at 4582118 (812 confs); `4d8f96ec…4970` → mined at 4582106 (825 confs). Explorer returns 403 to curl (bot block) but opens in a browser: `evidence/app/explorer-after-tx.png` |
| B3 | README lists every technology + versions + design decisions | ✅ | README "Technologies (versions pinned)" and "Design decisions" |
| B4 | PRIVACY.md answers the 4 questions; "a pass is not anonymity"; SC-002 | ✅ | PRIVACY.md §1–§4 + closing notes |
| B5 | NEXT_STEPS.md 30-day plan with links | ⚠️ | plan + links present; upstream issues are **drafted, not posted** (`docs/upstream/`) |
| B6 | Public URL / local by design | ✅ | README "Ghost Wallet" + PRIVACY.md (loopback only); landing: https://skartik-sk.github.io/ghost-layer/ (and https://ghost-layer-eight.vercel.app) |

## C. Judging criteria
| | Check | Result | Evidence |
|---|---|---|---|
| C1 | Works (re-run on final main) | ✅ | `scripts/e2e_local.sh` on main: 5 + 5 real regtest payments, linkage 5/5 → 0/5, 0 unaligned, exit 0 (`evidence/runs/20261104T183541Z`). `scripts/e2e_testnet.sh` (Nym): see bottom |
| C2 | Private by design | ✅ | loopback-only listeners `main.rs:305`, `dashboard.rs:364,392`; fail-closed Nym `main.rs:209`, `ghost-transport/src/lib.rs:102`; requests rebuilt, no wallet headers `ghost-gateway/src/lib.rs:3`; log redacted to heights `ghost-gateway/src/lib.rs:448`; keys never read `pay.rs:9`; MCP `ghost_pay` off by default + caps `mcp.rs:131,204,210`; dashboard token `dashboard.rs:317` |
| C3 | Easy | ⚠️ | one-command `ghost pay` + `cargo install ghost-layer` (installed from crates.io and run). **No EASY_TEST.md**: a test with someone who didn't build it has not been done; not faked |
| C4 | Needed (real sources) | ✅ | README "Why this is needed": ShadeCheck README (two HIGH SC-002 on Zingolib 7.0.0), nym-swizzle-zcash README ("Both leaks survive any mixnet…"), zecmix future plans. No mentor quotes claimed |
| C5 | Continuing (posted issues) | ⚠️ | drafts ready in `docs/upstream/` (Nym, zecmix, ShadeCheck, Zaino); **not posted** (needs the author's OK) |
| C6 | Bonus: reusable + Nym | ✅ | 5 crates on crates.io (`ghost-layer-core`, `-transport`, `-gateway`, `-attacker`, `ghost-layer`); Nym: two exits, fail-closed, N=5 real testnet payments 0/5 linked (`evidence/ATTACK_RESULTS.md`) |

## D. Consistency
| | Check | Result | Evidence |
|---|---|---|---|
| D1 | Claims only merged work | ✅ | ZecProof (`ghost proof`) and Ghost Wallet (`ghost dashboard`) are on main, with evidence (`evidence/zecproof.md`, `evidence/app/`) |
| D2 | Numbers match | ✅ | 100% → 0% local, Nym 0/5, 3.0 s → 22.4 s (Nym 32.4 s): `evidence/ATTACK_RESULTS.md` = README = landing = Stadium text |
| D3 | README links | ✅ | all relative links exist; external links 200 (crates.io page blocks curl; API confirms `ghost-layer 0.1.0`) |
| D4 | GitHub About | ✅ | "…Measured with an attacker test (100% → 0% linked), checked with ShadeCheck." |

## Open items (only the author can do)
- Post the upstream drafts (`docs/upstream/`), then add their URLs to NEXT_STEPS.md.
- Have someone who didn't build it run the quickstart; record it as EASY_TEST.md.
- Add the Cloudflare DNS record `A ghost.skartik.dev 76.76.21.21` (DNS only) so https://ghost.skartik.dev resolves.

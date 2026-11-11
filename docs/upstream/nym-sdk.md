# nymtech/nym: issue drafts from embedding nym-sdk in Ghost Layer

Status: drafted, not filed. Found while moving Ghost Layer from an external `nym-socks5-client` to
`nym-sdk` 1.21.6 (the latest on crates.io as of 2026-11-11).

---

## 1. `nym-sdk` 1.21.6 from crates.io does not compile in a fresh project

**Repro**

```
cargo new t && cd t
cargo add nym-sdk@1.21.6 tokio --features tokio/full
cargo build
```

```
error[E0433]: cannot find `crypto_common` in `cipher`
error[E0432]: unresolved imports `nym_client_core_config_types::old::old_config_v1_1_13`, …
error[E0432]: unresolved import `crate::config::disk_persistence::old_v1_1_33`
error: could not compile `nym-client-core` (lib) due to 4 previous errors
```

**Cause:** `nym-sdk` 1.21.6 depends on `nym-client-core = "^1.21.6"` (and similar caret requirements), so cargo
picks the newer 1.22.x inner crates that were published without a matching `nym-sdk`. 74 `nym-*` crates
resolve to 1.22.x; they are not compatible with `nym-sdk` 1.21.6.

**Workaround:** pin every `nym-*` crate back to 1.21.6 with `cargo update -p <crate>@1.22.x --precise 1.21.6`
(our script: `scripts/pin_nym.sh`, about 6 rounds because of the dependency order). With that, it builds.

**Suggested fix:** publish a `nym-sdk` release matching the 1.22.x crates, or use `~1.21.6`/`=1.21.6`
requirements between the inner crates of one release.

---

## 2. `Socks5::new` defaults to `send_anonymously: false`

`nym_socks5_client_core::config::Socks5::new` sets `send_anonymously: false`, and
`Socks5MixnetClient::connect_with` / `connect_new` use it with no way to change it. Without reply SURBs
the network requester learns the client's Nym address to reply to it. The standalone
`nym-socks5-client` exposes `--use-reply-surbs true`; SDK users get the weaker default silently.

**Suggestion:** default to `true` in the SDK (or add a `send_anonymously` option to `connect_with`) and
document the trade-off. We now build the client with `MixnetClientBuilder::socks5_config(cfg)` and set it
ourselves.

---

## 3. `nym-socks5-client-core` panics on shutdown

When the process exits while a SOCKS5 connection is open:

```
thread 'tokio-rt-worker' panicked at nym-socks5-client-core-1.21.6/src/socks/client.rs:201:18:
called `Result::unwrap()` on an `Err` value: TrySendError { kind: Disconnected }
ERROR nym_task::cancellation::manager: panicked at … issuing global cancellation
```

The channel's receiver is gone during runtime teardown; `unwrap()` there turns a normal shutdown into a panic.
Returning or logging on `Disconnected` would avoid it.

---

## 4. Observation: exits that accept SOCKS connections but don't carry traffic

On 2026-11-11, testing exits picked by `NetworkRequesterSelector::any()` with a real request (TLS + gRPC
`GetLatestBlock` to `testnet.zec.rocks:443`), over 20 requesters accepted the SOCKS5 CONNECT but never
returned data, and once the directory API (`/api/v2/nym-nodes/described`) timed out. `any()` weights by
performance, so these were "good" by the directory's measure. A health signal that reflects carried
traffic would help SDK users pick working exits. (Our workaround: probe each exit with a real request
before use and replace exits that stop working.)

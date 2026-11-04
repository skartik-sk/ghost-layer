# Draft issue for zingolabs/zaino (not posted yet)

**Title:** NU7 regtest: blocks rejected with "invalid consensus branch id" (placeholder 0xffff_fffe vs 0x77190AD9)

Zebra 7.0.0 on regtest with `[network.testnet_parameters.activation_heights] NU7 = 1` produces blocks whose chain tip
consensus branch is `77190ad9` (`getblockchaininfo`). zainod 0.10.1 and 0.10.2 (`-no-tls` images) then loop on:

    WARN zaino_chain_head_service::service: ChainHead failed to advance; retrying, error: validator unavailable:
    deserialize: parse error: invalid consensus branch id

`packages/zaino-state/src/chain_index/network_adoption.rs` uses a placeholder NU7 branch (`0xffff_fffe`, with a note
that the real id is a TODO). Public testnet runs NU7 with branch `0x77190AD9` (librustzcash `zcash_protocol` docs the
same). Would updating the constant (and regtest activation handling) be welcome? We hit this while trying to run a
local NU7 before/after privacy experiment for Ghost Layer (https://github.com/skartik-sk/ghost-layer).

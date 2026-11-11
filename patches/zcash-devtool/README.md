# Patches to zcash-devtool

The release binaries bundle [zcash-devtool](https://github.com/zcash/zcash-devtool) at
`DEVTOOL_REV` (see `.github/workflows/release.yml`) with these patches applied in order. Both are
meant for upstream; until they are merged, this is the build that ships.

| patch | what |
|---|---|
| `0001-identity-file-mode-0600.patch` | create the generated age identity file with mode 0600 (it was world-readable 0644) |
| `0002-wallet-disclose-zip311.patch` | `zcash-devtool wallet disclose --txid … [--msg …]`: ZIP 311 payment disclosure (`zpd1…`) for Ironwood spends, via the [`zip311`](../../crates/zip311) crate |

Apply them by hand:

```sh
git clone https://github.com/zcash/zcash-devtool && cd zcash-devtool
git checkout 0b3436bb8d235c67cd36b800cb45a7b1b489c415
for p in /path/to/ghost-layer/patches/zcash-devtool/*.patch; do git apply "$p"; done
cargo build --release --locked
```

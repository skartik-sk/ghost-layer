# zcash/zcash-devtool: PR draft

**Title:** `wallet init`: create the generated age identity with mode 0600

**Status:** patch ready on a local branch (`fix/identity-file-mode`), not opened yet.

## Problem

When `-i/--identity` points to a file that does not exist, `wallet init` generates an age identity and writes
it with `tokio::fs::File::create_new` (`src/commands/wallet/init.rs`, line 101 at `0b3436b`, unchanged on `main`).
That uses the default mode `0666 & umask`, so with the common umask `022` the file is **0644**: readable by
every local user. The file holds the X25519 secret that decrypts the wallet's mnemonic.

Seen in practice: Ghost Layer's `ghost init` drives `zcash-devtool wallet init` and found the new
`identity.age` at `-rw-r--r--`.

The same pattern is in `wallet restore-mnemonic` (`restore_mnemonic.rs:93`) and `zip48 init`
(`zip48/init.rs:45`).

## Fix

```rust
// Write it to the provided path so we have it for next time. The file holds the secret key
// that decrypts the mnemonic, so on Unix create it readable by the owner only (0600)
// instead of inheriting the default 0666 & umask (usually 0644, world-readable).
let mut open = tokio::fs::OpenOptions::new();
open.write(true).create_new(true);
#[cfg(unix)]
open.mode(0o600);
let mut f = open.open(opts.identity).await?;
```

The PR applies the same change to the other two call sites. Existing identity files are not touched.

## How to verify

```
umask 022
zcash-devtool wallet -w /tmp/w init --name t -i /tmp/w.age -n test
stat -f %Lp /tmp/w.age      # macOS (Linux: stat -c %a): 600 with the patch, 644 without
```

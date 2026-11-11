//! `ghost init`: from nothing to a testnet wallet that pays through Ghost Layer, in one command.
//!
//! It creates a zcash-devtool wallet *through the gateway* (so even setup doesn't show the server your IP),
//! saves the wallet's viewing key for ZecProofs, and writes `~/.ghost-layer/config.json` so that
//! `ghost pay`, `ghost dashboard` and `ghost mcp` work without --wallet/--identity.
//!
//! Keys stay in the wallet: zcash-devtool generates the age identity and the seed; Ghost only stores paths
//! and the UFVK (a viewing key, which cannot spend).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::pay::{ensure_gateway, strip_ansi};
use crate::{Fail, NymArgs, data_dir};


#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub(crate) struct Config {
    pub wallet: Option<PathBuf>,
    pub identity: Option<PathBuf>,
    pub ufvk_file: Option<PathBuf>,
    pub devtool: Option<PathBuf>,
}

fn config_path() -> PathBuf {
    data_dir().join("config.json")
}

/// The saved config, or an empty one (a missing or unreadable file just means "not initialised").
pub(crate) fn load() -> Config {
    std::fs::read(config_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save(c: &Config) -> Result<(), Fail> {
    let p = config_path();
    std::fs::create_dir_all(data_dir()).map_err(|e| Fail::runtime(format!("{}: {e}", data_dir().display()), "check permissions"))?;
    let body = serde_json::to_vec_pretty(c).map_err(|e| Fail::runtime(e.to_string(), "report a bug"))?;
    std::fs::write(&p, body).map_err(|e| Fail::runtime(format!("{}: {e}", p.display()), "check permissions"))
}

/// zcash-devtool to run: --devtool, $ZCASH_DEVTOOL, the config, next to `ghost` (release archive), or PATH.
pub(crate) fn devtool(flag: Option<&PathBuf>) -> PathBuf {
    if let Some(p) = flag.cloned().or_else(|| std::env::var_os("ZCASH_DEVTOOL").map(PathBuf::from)).or_else(|| load().devtool) {
        return p;
    }
    if let Some(sib) = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("zcash-devtool"))) {
        if sib.is_file() {
            return sib;
        }
    }
    PathBuf::from("zcash-devtool")
}

pub(crate) fn devtool_missing(dt: &Path) -> Fail {
    Fail::usage(
        format!("zcash-devtool not found ({})", dt.display()),
        "install it with the release installer (scripts/install.sh), or: cargo install --locked --git https://github.com/zcash/zcash-devtool --rev 0b3436bb8d235c67cd36b800cb45a7b1b489c415; or pass --devtool <path>",
    )
}

pub(crate) fn check_devtool(dt: &Path) -> Result<(), Fail> {
    match Command::new(dt).arg("--help").stdout(Stdio::null()).stderr(Stdio::null()).status() {
        Ok(_) => Ok(()),
        Err(_) => Err(devtool_missing(dt)),
    }
}

/// Wallet and identity: the flags if given, else the ones `ghost init` saved.
pub(crate) fn wallet_paths(wallet: Option<PathBuf>, identity: Option<PathBuf>) -> Option<(PathBuf, PathBuf)> {
    let c = load();
    Some((wallet.or(c.wallet)?, identity.or(c.identity)?))
}

#[derive(clap::Args)]
pub(crate) struct InitArgs {
    /// Wallet directory to create (default ~/.ghost-layer/wallet)
    #[arg(long)]
    dir: Option<PathBuf>,
    /// Account name
    #[arg(long, default_value = "ghost")]
    name: String,
    /// Upstream lightwalletd server
    #[arg(long, default_value = crate::DEFAULT_SERVER)]
    server: String,
    #[arg(long, default_value = crate::DEFAULT_LISTEN)]
    listen: SocketAddr,
    /// zcash-devtool binary (default: $ZCASH_DEVTOOL, next to ghost, or on PATH)
    #[arg(long)]
    devtool: Option<PathBuf>,
    #[command(flatten)]
    nym: NymArgs,
}

/// `Default Address: utest1…` and `UFVK: uviewtest1…` from zcash-devtool's text output.
fn field(out: &str, key: &str) -> Option<String> {
    out.lines().find_map(|l| l.trim().strip_prefix(key).map(|v| v.trim().to_string())).filter(|v| !v.is_empty())
}

fn run(dt: &Path, wallet: &Path, args: &[&str]) -> Result<String, Fail> {
    let out = Command::new(dt).arg("wallet").arg("-w").arg(wallet).args(args).stdin(Stdio::null()).output()
        .map_err(|_| devtool_missing(dt))?;
    let stdout = strip_ansi(&String::from_utf8_lossy(&out.stdout));
    if !out.status.success() {
        let err = strip_ansi(&String::from_utf8_lossy(&out.stderr));
        let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no output").to_string();
        return Err(Fail::runtime(format!("zcash-devtool {} failed: {last}", args[0]), "run `ghost doctor`, then retry `ghost init`"));
    }
    Ok(stdout)
}

pub(crate) async fn init(a: InitArgs, json: bool) -> Result<u8, Fail> {
    let say = |m: &str| if json { eprintln!("{m}") } else { println!("{m}") };
    let dt = devtool(a.devtool.as_ref());
    check_devtool(&dt)?;
    let dir = a.dir.clone().unwrap_or_else(|| data_dir().join("wallet"));
    let identity = dir.join("identity.age");
    let created = !dir.join("data.sqlite").exists();

    if created {
        // The wallet asks the server for the chain tip at init: do that through Ghost too.
        let (_gw, _) = ensure_gateway(a.listen, &a.server, "demo", &a.nym, &say).await?;
        std::fs::create_dir_all(&dir).map_err(|e| Fail::runtime(format!("{}: {e}", dir.display()), "check permissions"))?;
        say("🔑 creating a testnet wallet (zcash-devtool makes the seed and the age identity; Ghost never reads them) …");
        run(&dt, &dir, &["init", "--name", &a.name, "-i", &identity.to_string_lossy(), "-n", "test", "-s", &a.listen.to_string()])?;
    } else {
        say(&format!("👻 wallet already exists at {}: keeping it", dir.display()));
    }
    if !identity.exists() {
        return Err(Fail::usage(format!("{} is missing", identity.display()), "pass --dir for a wallet created by `ghost init`"));
    }
    // zcash-devtool leaves the identity (it decrypts the seed) world-readable: owner-only. Ghost changes the
    // mode only; it never opens the file.
    restrict(&dir, 0o700)?;
    restrict(&identity, 0o600)?;

    let accounts = run(&dt, &dir, &["list-accounts"])?;
    let addrs = run(&dt, &dir, &["list-addresses"])?;
    let address = field(&addrs, "Default Address:").ok_or_else(|| Fail::runtime("no address in zcash-devtool output", "run zcash-devtool wallet list-addresses"))?;
    let ufvk_file = match field(&accounts, "UFVK:") {
        Some(ufvk) => {
            let p = dir.join("ufvk.txt");
            write_private(&p, &ufvk)?;
            Some(p)
        }
        None => None,
    };
    let cfg = Config { wallet: Some(dir.clone()), identity: Some(identity.clone()), ufvk_file: ufvk_file.clone(), devtool: Some(dt.clone()) };
    save(&cfg)?;

    if json {
        println!("{}", json!({"schema_version": 1, "ok": true, "created": created, "wallet": dir, "identity": identity,
            "ufvk_file": ufvk_file, "address": address, "config": config_path()}));
    } else {
        println!("✅ wallet ready: {}", dir.display());
        println!("   address  {address}");
        println!("   config   {} (ghost pay / dashboard / mcp now need no --wallet flags)", config_path().display());
        println!();
        println!("Next:");
        println!("  1. Get testnet TAZ for that address. A faucet that worked for us (third-party, proof-of-work):");
        println!("       npx zcash-faucet {address}");
        println!("  2. Open Ghost Wallet:   ghost dashboard");
        println!("     or pay from here:    ghost pay --to <utest1…> --amount 0.001");
    }
    Ok(0)
}

fn restrict(p: &Path, mode: u32) -> Result<(), Fail> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode))
            .map_err(|e| Fail::runtime(format!("{}: {e}", p.display()), "check permissions"))?;
    }
    #[cfg(not(unix))]
    let _ = (p, mode);
    Ok(())
}

/// A viewing key can't spend, but it reveals the wallet's history: owner-only.
fn write_private(p: &Path, body: &str) -> Result<(), Fail> {
    let err = |e: std::io::Error| Fail::runtime(format!("{}: {e}", p.display()), "check permissions");
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(p).map_err(err)?;
        f.write_all(body.as_bytes()).map_err(err)
    }
    #[cfg(not(unix))]
    {
        std::fs::write(p, body).map_err(err)
    }
}

#[cfg(test)]
mod tests {
    use super::field;

    #[test]
    fn parses_devtool_output() {
        let out = "Account AccountUuid(x)\n     Default Address: utest1abc\n     UFVK: uviewtest1def\n";
        assert_eq!(field(out, "Default Address:").as_deref(), Some("utest1abc"));
        assert_eq!(field(out, "UFVK:").as_deref(), Some("uviewtest1def"));
        assert_eq!(field(out, "Missing:"), None);
        assert_eq!(field("     UFVK: \n", "UFVK:"), None);
    }
}

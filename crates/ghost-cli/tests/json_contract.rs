//! The `--json` contract: run the real `ghost` binary and validate its output against
//! docs/schemas/*.schema.json. Offline commands always run; network ones (verify on a real
//! testnet txid, doctor) run when GHOST_ONLINE_TESTS=1.

use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn ghost(home: &Path, args: &[&str]) -> (i32, serde_json::Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_ghost"))
        .arg("--json")
        .arg("--quiet")
        .args(args)
        .env("GHOST_HOME", home)
        .output()
        .expect("run ghost");
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().last().unwrap_or_else(|| panic!("no JSON output for {args:?}"));
    let v = serde_json::from_str(line).unwrap_or_else(|e| panic!("{args:?}: not JSON ({e}): {line}"));
    (out.status.code().unwrap_or(-1), v)
}

fn check(schema: &str, v: &serde_json::Value) {
    let path = root().join("docs/schemas").join(format!("{schema}.schema.json"));
    let s: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).expect("schema file")).expect("schema JSON");
    let validator = jsonschema::validator_for(&s).expect("valid schema");
    let errors: Vec<String> = validator.iter_errors(v).map(|e| format!("{} at {}", e, e.instance_path())).collect();
    assert!(errors.is_empty(), "{schema}: {errors:#?}\n{v}");
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ghost-json-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn offline_outputs_match_schemas() {
    let home = tmp("home");
    let (code, v) = ghost(&home, &["explain"]);
    assert_eq!(code, 0);
    check("explain", &v);

    let (code, v) = ghost(&home, &["status"]);
    assert_eq!(code, 0);
    check("status", &v);

    let (code, v) = ghost(&home, &["verify", "not-a-txid"]);
    assert_eq!(code, 2, "usage errors exit 2");
    check("error", &v);

    let (code, v) = ghost(&home, &["pay", "--wallet", "/nonexistent", "--identity", "/x", "--to", "utest1x", "--amount", "0.001"]);
    assert_eq!(code, 2);
    check("error", &v);

    // Real committed observer logs (local before/after runs).
    let ev = root().join("evidence/shadecheck");
    let (code, v) = ghost(&home, &["attack", "--events", ev.join("after/events.jsonl").to_str().unwrap()]);
    assert_eq!(code, 0);
    check("attack", &v);

    let out = home.join("ATTACK.md");
    let (code, v) = ghost(&home, &["report", "--attack", "--before", ev.join("before/events.jsonl").to_str().unwrap(),
        "--after", ev.join("after/events.jsonl").to_str().unwrap(), "--out", out.to_str().unwrap()]);
    assert_eq!(code, 0);
    check("report-attack", &v);
    assert_eq!(v["before"]["sends"].as_array().unwrap().len(), 5);

    let out = home.join("BA.md");
    let (code, v) = ghost(&home, &["report", "--before", ev.join("before").to_str().unwrap(), "--after",
        ev.join("after").to_str().unwrap(), "--out", out.to_str().unwrap()]);
    assert_eq!(code, 0);
    check("report", &v);
    let _ = std::fs::remove_dir_all(home);
}

/// Real `ghost --json pay` output saved from a testnet payment (evidence/pay_demo.json).
#[test]
fn recorded_pay_output_matches_schema() {
    let text = std::fs::read_to_string(root().join("evidence/pay_demo.json")).expect("evidence/pay_demo.json");
    let v: serde_json::Value = serde_json::from_str(text.trim()).expect("JSON");
    check("pay", &v);
    assert_eq!(v["ok"], true);
}

#[test]
fn online_outputs_match_schemas() {
    if std::env::var("GHOST_ONLINE_TESTS").as_deref() != Ok("1") {
        eprintln!("skipped: set GHOST_ONLINE_TESTS=1 (needs testnet.zec.rocks)");
        return;
    }
    let home = tmp("online");
    // Our real "after" payment (Ghost Layer + Nym), mined at 4582118.
    let (code, v) = ghost(&home, &["verify", "--direct", "09f0f0b01942a658197f65563a977c66fc19f8823268b0e547470e022bd3ce69"]);
    assert_eq!(code, 0);
    check("verify", &v);
    assert_eq!(v["state"], "mined");
    assert_eq!(v["height"], 4582118);

    // ZecProof committed in evidence/zecproof.md (real payment a23746a7…): valid, and a forged one is not.
    let md = std::fs::read_to_string(root().join("evidence/zecproof.md")).unwrap();
    let proof = md.split_whitespace().find(|w| w.starts_with("zecproof1ey")).expect("proof in evidence").to_string();
    let (code, v) = ghost(&home, &["proof", "verify", "--direct", &proof]);
    assert_eq!(code, 0);
    check("proof-verify", &v);
    assert_eq!(v["valid"], true);
    assert_eq!(v["value_zat"], 100000);
    let (code, v) = ghost(&home, &["proof", "verify", "--direct", "zecproof1AAAA"]);
    assert_eq!(code, 1);
    check("proof-verify", &v);
    assert_eq!(v["valid"], false);

    let (_, v) = ghost(&home, &["doctor", "--direct", "--listen", "127.0.0.1:19068"]);
    check("doctor", &v);
    let _ = std::fs::remove_dir_all(home);
}

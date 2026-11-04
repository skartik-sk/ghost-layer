//! ZecProof: a single-payment proof. "Invisible by default, provable by choice."
//!
//! The SENDER derives the outgoing cipher key (OCK) of exactly one output of a mined transaction
//! from their outgoing viewing key (OVK, from the UFVK): `ock = PRF_ock(ovk, cv, cmx, epk)`.
//! The OCK decrypts that one output's out-ciphertext and nothing else; the OVK never leaves the
//! machine. A verifier fetches the transaction, recovers the note with the OCK, and checks that
//! the recovered note's commitment equals the on-chain `cmx` (so the proof can't be forged).
//!
//! Proof string: `zecproof1` + base64url(JSON {v, network, pool, txid, action_index, ock_hex, msg}).
//! It proves a payment happened (recipient, amount, memo, height), not who presents it: anyone
//! holding the string can show it (transferable). Full ZIP 311 payment disclosures are future work.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use orchard::keys::Scope;
use orchard::note::ExtractedNoteCommitment;
use orchard::note_encryption::{IronwoodVersion, NoteEncryptionDomain, OrchardVersion};
use serde::{Deserialize, Serialize};
use serde_json::json;
use zcash_keys::address::UnifiedAddress;
use zcash_keys::keys::UnifiedFullViewingKey;
use zcash_note_encryption::{Domain, OutgoingCipherKey, ShieldedOutput, try_output_recovery_with_ock};
use zcash_primitives::transaction::Transaction;
use zcash_protocol::consensus::{BlockHeight, BranchId, Network};
use zcash_protocol::memo::{Memo, MemoBytes};

use crate::{DEFAULT_SERVER, Fail, NymArgs, TxState, explorer_links, tx_state};

const PREFIX: &str = "zecproof1";

#[derive(clap::Subcommand)]
pub(crate) enum ProofCmd {
    /// Create a proof for ONE output of a payment you SENT (needs your UFVK; it never leaves this machine)
    Create {
        #[arg(long)]
        txid: String,
        /// Action index within the Ironwood (or Orchard) bundle; omit to auto-pick the single payment output
        #[arg(long)]
        action: Option<usize>,
        /// Optional message bound into the proof (e.g. an invoice id)
        #[arg(long, default_value = "")]
        msg: String,
        /// Your UFVK (prefer --ufvk-file or GHOST_UFVK)
        #[arg(long, env = "GHOST_UFVK", hide_env_values = true)]
        ufvk: Option<String>,
        #[arg(long, conflicts_with = "ufvk")]
        ufvk_file: Option<std::path::PathBuf>,
        #[arg(long, default_value = DEFAULT_SERVER)]
        server: String,
        #[command(flatten)]
        nym: NymArgs,
    },
    /// Verify a zecproof1… string against the chain
    Verify {
        proof: String,
        #[arg(long, default_value = DEFAULT_SERVER)]
        server: String,
        #[command(flatten)]
        nym: NymArgs,
    },
}

#[derive(Serialize, Deserialize, Debug)]
struct Payload {
    v: u32,
    network: String,
    pool: String,
    txid: String,
    action_index: usize,
    ock_hex: String,
    msg: String,
}

struct Recovered {
    recipient: String,
    value_zat: u64,
    memo: String,
}

fn memo_text(m: &[u8; 512]) -> String {
    match MemoBytes::from_bytes(m).ok().and_then(|b| Memo::try_from(b).ok()) {
        Some(Memo::Text(t)) => t.to_string().chars().filter(|c| !c.is_control()).collect(),
        Some(Memo::Empty) => "(empty)".into(),
        Some(_) => "(non-text memo)".into(),
        None => "(invalid memo)".into(),
    }
}

async fn fetch(txid: &str, server: &str, nym: &NymArgs) -> Result<(Transaction, u64), Fail> {
    let TxState { state, height, raw, .. } = tx_state(txid, server, nym).await?;
    let Some(h) = height else {
        return Err(Fail::runtime(format!("tx is {state}, not mined"), "proofs are only for mined transactions"));
    };
    let params = Network::TestNetwork;
    let bh = BlockHeight::from_u32(h as u32);
    let tx = Transaction::read(&raw[..], BranchId::for_height(&params, bh))
        .map_err(|e| Fail::runtime(format!("cannot parse tx: {e}"), "check the txid"))?;
    Ok((tx, h))
}

/// Recover one action with an OCK in the given domain version; checks cmx explicitly.
fn recover<V: orchard::note_encryption::DomainVersion>(
    action: &orchard::Action<orchard::primitives::redpallas::Signature<orchard::primitives::redpallas::SpendAuth>>,
    ock: &OutgoingCipherKey,
) -> Option<Recovered> {
    let domain = NoteEncryptionDomain::<V>::for_action(action);
    let (note, addr, memo) = try_output_recovery_with_ock(&domain, ock, action, &action.encrypted_note().out_ciphertext)?;
    // Defence in depth: the library already checks the commitment; we compare cmx explicitly too.
    if ExtractedNoteCommitment::from(note.commitment()) != *action.cmx() {
        return None;
    }
    let recipient = UnifiedAddress::from_receivers(Some(addr), None, None, None, None)
        .map(|ua| ua.encode(&Network::TestNetwork))
        .unwrap_or_else(|| "(unencodable)".into());
    Some(Recovered { recipient, value_zat: note.value().inner(), memo: memo_text(&memo) })
}

fn ock_for<V: orchard::note_encryption::DomainVersion>(
    ovk: &orchard::keys::OutgoingViewingKey,
    action: &orchard::Action<orchard::primitives::redpallas::Signature<orchard::primitives::redpallas::SpendAuth>>,
) -> OutgoingCipherKey {
    type A = orchard::Action<orchard::primitives::redpallas::Signature<orchard::primitives::redpallas::SpendAuth>>;
    let cmstar = <A as ShieldedOutput<NoteEncryptionDomain<V>>>::cmstar_bytes(action);
    let epk = <A as ShieldedOutput<NoteEncryptionDomain<V>>>::ephemeral_key(action);
    <NoteEncryptionDomain<V> as Domain>::derive_ock(ovk, action.cv_net(), &cmstar, &epk)
}

pub(crate) async fn run(cmd: ProofCmd, json_out: bool) -> Result<u8, Fail> {
    match cmd {
        ProofCmd::Create { txid, action, msg, ufvk, ufvk_file, server, nym } => {
            let ufvk = match ufvk_file {
                Some(f) => std::fs::read_to_string(&f).map_err(|e| Fail::usage(format!("{}: {e}", f.display()), "pass a readable UFVK file"))?.trim().to_string(),
                None => ufvk.ok_or_else(|| Fail::usage("missing --ufvk-file / GHOST_UFVK", "the SENDER's uviewtest1… key (never a spending key)"))?,
            };
            let key = UnifiedFullViewingKey::decode(&Network::TestNetwork, &ufvk)
                .map_err(|e| Fail::usage(format!("not a testnet UFVK: {e}"), "pass the sender wallet's uviewtest1… key"))?;
            let ovk = key.orchard().ok_or_else(|| Fail::usage("UFVK has no Orchard/Ironwood component", "use a wallet with a shielded account"))?
                .to_ovk(Scope::External);
            if msg.len() > 256 {
                return Err(Fail::usage("--msg is limited to 256 bytes", "shorten it"));
            }
            let (tx, height) = fetch(&txid, &server, &nym).await?;
            // Find the action(s) this wallet sent to someone (external OVK recovers them).
            let mut found = Vec::new();
            for (pool, bundle) in [("ironwood", tx.ironwood_bundle()), ("orchard", tx.orchard_bundle())] {
                let Some(b) = bundle else { continue };
                for (i, a) in b.actions().iter().enumerate() {
                    if action.is_some_and(|want| want != i) {
                        continue;
                    }
                    let (ock, rec) = if pool == "ironwood" {
                        let ock = ock_for::<IronwoodVersion>(&ovk, a);
                        let r = recover::<IronwoodVersion>(a, &ock);
                        (ock, r)
                    } else {
                        let ock = ock_for::<OrchardVersion>(&ovk, a);
                        let r = recover::<OrchardVersion>(a, &ock);
                        (ock, r)
                    };
                    if let Some(r) = rec {
                        found.push((pool, i, ock, r));
                    }
                }
            }
            let (pool, idx, ock, rec) = match found.len() {
                0 => return Err(Fail::runtime("this UFVK did not send any shielded output in that tx (or not that --action)",
                    "use the SENDER wallet's UFVK; change outputs use the internal key and are not provable here")),
                1 => found.remove(0),
                _ => return Err(Fail::usage(format!("several outputs sent by this wallet: actions {:?}", found.iter().map(|f| f.1).collect::<Vec<_>>()),
                    "pick one with --action <i>")),
            };
            let payload = Payload { v: 1, network: "test".into(), pool: pool.into(), txid: txid.to_ascii_lowercase(),
                action_index: idx, ock_hex: hex::encode(ock.0), msg: msg.clone() };
            let proof = format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).expect("serializable")));
            if json_out {
                println!("{}", json!({"schema_version": 1, "ok": true, "proof": proof, "txid": payload.txid, "pool": pool,
                    "action_index": idx, "height": height, "recipient": rec.recipient, "value_zat": rec.value_zat, "memo": rec.memo, "msg": msg}));
            } else {
                println!("🧾 ZecProof for {} TAZ to {}… (action #{idx}, {pool}, height {height})", rec.value_zat as f64 / 1e8, &rec.recipient[..20.min(rec.recipient.len())]);
                println!("{proof}");
                println!("   reveals only this one output; your viewing key stayed on this machine");
            }
            Ok(0)
        }
        ProofCmd::Verify { proof, server, nym } => {
            let invalid = |reason: String, json_out: bool| -> Result<u8, Fail> {
                if json_out {
                    println!("{}", json!({"schema_version": 1, "ok": false, "valid": false, "reason": reason}));
                } else {
                    println!("❌ INVALID: {reason}");
                }
                Ok(1)
            };
            let Some(body) = proof.trim().strip_prefix(PREFIX) else { return invalid("not a zecproof1… string".into(), json_out) };
            let Ok(bytes) = URL_SAFE_NO_PAD.decode(body) else { return invalid("bad base64url encoding".into(), json_out) };
            let Ok(p) = serde_json::from_slice::<Payload>(&bytes) else { return invalid("bad proof payload".into(), json_out) };
            if p.v != 1 || p.network != "test" {
                return invalid(format!("unsupported proof (v{}, network {})", p.v, p.network), json_out);
            }
            let Ok(ock_bytes) = <[u8; 32]>::try_from(hex::decode(&p.ock_hex).unwrap_or_default()) else {
                return invalid("bad OCK".into(), json_out);
            };
            let ock = OutgoingCipherKey(ock_bytes);
            let (tx, height) = match fetch(&p.txid, &server, &nym).await {
                Ok(v) => v,
                Err(f) if f.code == 2 => return invalid(f.msg, json_out),
                Err(f) => return Err(f),
            };
            let bundle = match p.pool.as_str() {
                "ironwood" => tx.ironwood_bundle(),
                "orchard" => tx.orchard_bundle(),
                other => return invalid(format!("unknown pool {other}"), json_out),
            };
            let Some(action) = bundle.and_then(|b| b.actions().get(p.action_index)) else {
                return invalid(format!("tx has no {} action #{}", p.pool, p.action_index), json_out);
            };
            let rec = if p.pool == "ironwood" { recover::<IronwoodVersion>(action, &ock) } else { recover::<OrchardVersion>(action, &ock) };
            let Some(rec) = rec else {
                return invalid("the OCK does not decrypt this output, or its note commitment does not match the chain".into(), json_out);
            };
            let links = explorer_links(&p.txid);
            if json_out {
                println!("{}", json!({"schema_version": 1, "ok": true, "valid": true, "txid": p.txid, "height": height, "pool": p.pool,
                    "action_index": p.action_index, "recipient": rec.recipient, "value_zat": rec.value_zat, "memo": rec.memo,
                    "msg": p.msg, "msg_signed": false, "explorer": links}));
            } else {
                println!("✅ VALID ZecProof: a payment of {:.8} TAZ ({} zat) was made", rec.value_zat as f64 / 1e8, rec.value_zat);
                println!("   to        {}", rec.recipient);
                println!("   memo      {}", rec.memo);
                println!("   tx        {} ({} action #{}), mined at height {height}", p.txid, p.pool, p.action_index);
                if !p.msg.is_empty() {
                    println!("   message   {}  (label only: NOT signed, anyone holding the proof can change it)", p.msg.chars().filter(|c| !c.is_control()).collect::<String>());
                }
                println!("   note commitment matches the chain; only this one output was revealed");
                println!("   ⚠️  proves the payment happened, not who is presenting this proof");
                for l in &links {
                    println!("   🔗 {l}");
                }
            }
            Ok(0)
        }
    }
}

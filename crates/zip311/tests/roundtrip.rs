//! End-to-end with real Halo2 proofs: build an Ironwood "transaction" bundle that spends a note,
//! disclose the payment, verify it, and check that forgeries and tampering are rejected.
//! Slow in debug builds; run with `cargo test -p zip311 --release`.

use incrementalmerkletree::{Hashable, Level};
use orchard::builder::{Builder, BundleType};
use orchard::bundle::{Authorized, BundleVersion};
use orchard::keys::{FullViewingKey, PreparedIncomingViewingKey, Scope, SpendAuthorizingKey, SpendingKey};
use orchard::note_encryption::IronwoodDomain;
use orchard::tree::{MerkleHashOrchard, MerklePath};
use orchard::value::NoteValue;
use orchard::{Anchor, Bundle, Note};
use sapling::note_encryption::Zip212Enforcement;
use zip311::*;

const VERSION: BundleVersion = BundleVersion::ironwood_v3();

fn key(b: u8) -> (SpendingKey, FullViewingKey) {
    let sk = SpendingKey::from_bytes([b; 32]).unwrap();
    let fvk = FullViewingKey::from(&sk);
    (sk, fvk)
}

/// A note of `value` paid to `fvk`, by trial-decrypting a freshly built (unproven) bundle.
fn receive(fvk: &FullViewingKey, value: u64) -> Note {
    let mut rng = rand::rng();
    let mut b = Builder::new(BundleType::DEFAULT, VERSION, VERSION.default_flags(), Anchor::empty_tree()).unwrap();
    b.add_output(None, fvk.address_at(0u32, Scope::External), NoteValue::from_raw(value), [0; 512]).unwrap();
    let (bundle, _) = b.build::<i64>(&mut rng).unwrap().unwrap();
    let ivk = PreparedIncomingViewingKey::new(&fvk.to_ivk(Scope::External));
    bundle
        .actions()
        .iter()
        .find_map(|a| zcash_note_encryption::try_note_decryption(&IronwoodDomain::for_action(a), &ivk, a))
        .map(|(n, _, _)| n)
        .expect("our output")
}

/// The note as the only leaf of the commitment tree.
fn witness(note: &Note) -> (MerklePath, Anchor) {
    let path = MerklePath::from_parts(0, std::array::from_fn(|i| MerkleHashOrchard::empty_root(Level::from(i as u8))));
    let anchor = path.root(note.commitment().into());
    (path, anchor)
}

struct Fixture {
    bundle: Bundle<Authorized, i64>,
    sk: SpendingKey,
    fvk: FullViewingKey,
    note: Note,
    path: MerklePath,
    spend_index: u32,
}

/// Sender (key 1) pays 60_000 zat with memo "thanks" to recipient (key 2), change back to itself.
fn fixture() -> Fixture {
    let mut rng = rand::rng();
    let (sk, fvk) = key(1);
    let (_, rfvk) = key(2);
    let note = receive(&fvk, 100_000);
    let (path, anchor) = witness(&note);
    let mut b = Builder::new(BundleType::DEFAULT, VERSION, VERSION.default_flags(), anchor).unwrap();
    b.add_spend(fvk.clone(), note, path.clone()).unwrap();
    let mut memo = [0u8; 512];
    memo[..6].copy_from_slice(b"thanks");
    b.add_output(Some(fvk.to_ovk(Scope::External)), rfvk.address_at(0u32, Scope::External), NoteValue::from_raw(60_000), memo).unwrap();
    b.add_output(Some(fvk.to_ovk(Scope::Internal)), fvk.address_at(0u32, Scope::Internal), NoteValue::from_raw(30_000), [0xf6; 512]).unwrap();
    let (unauth, _) = b.build::<i64>(&mut rng).unwrap().unwrap();
    let pk = orchard::circuit::ProvingKey::build(VERSION.circuit_version());
    let bundle = unauth
        .create_proof(&pk, &mut rng)
        .unwrap()
        .apply_signatures(&mut rng, [0x42; 32], &[SpendAuthorizingKey::from(&sk)])
        .unwrap();
    // The transaction itself must be valid.
    bundle.verify_proof(&orchard::circuit::VerifyingKey::build(VERSION.circuit_version())).unwrap();
    let nf = note.nullifier(&fvk);
    let spend_index = bundle.actions().iter().position(|a| *a.nullifier() == nf).unwrap() as u32;
    Fixture { bundle, sk, fvk, note, path, spend_index }
}

fn tx(f: &Fixture) -> TxData<'_, i64> {
    TxData { txid: [0xab; 32], sapling: None, orchard: None, ironwood: Some(&f.bundle), zip212: Zip212Enforcement::On }
}

fn spend(f: &Fixture) -> OrchardSpend {
    OrchardSpend {
        pool: Pool::Ironwood,
        index: f.spend_index,
        fvk: f.fvk.clone(),
        ask: SpendAuthorizingKey::from(&f.sk),
        note: f.note,
        merkle_path: f.path.clone(),
    }
}

#[test]
#[cfg_attr(debug_assertions, ignore = "Halo2 proving is slow in debug; run with --release")]
fn disclose_verify_and_reject_tampering() {
    let f = fixture();
    let data = tx(&f);
    let mut rng = rand::rng();

    // Only the payment (external OVK) is found; change uses the internal OVK.
    let outputs = sent_orchard_outputs(&data, &f.fvk.to_ovk(Scope::External));
    assert_eq!(outputs.len(), 1);

    let pd = create(&data, 1, b"invoice 42".to_vec(), outputs, &[spend(&f)], &mut rng).unwrap();
    let text = pd.encode();
    println!("disclosure: {} chars", text.len());
    let pd = PaymentDisclosure::decode(&text).unwrap();

    let v = verify(&pd, &data, 1).unwrap();
    assert_eq!(v.msg, b"invoice 42");
    assert_eq!(v.spends, vec![(Pool::Ironwood, f.spend_index)]);
    assert_eq!(v.outputs.len(), 1);
    assert_eq!(v.outputs[0].value_zat, 60_000);
    assert_eq!(&v.outputs[0].memo[..6], b"thanks");
    match &v.outputs[0].recipient {
        Recipient::Orchard(a) => assert_eq!(*a, key(2).1.address_at(0u32, Scope::External)),
        r => panic!("unexpected recipient {r:?}"),
    }

    // Wrong network.
    assert!(matches!(verify(&pd, &data, 133), Err(Error::CoinTypeMismatch { .. })));
    // Another transaction.
    let mut other = pd.clone();
    other.txid[0] ^= 1;
    assert_eq!(verify(&other, &data, 1).unwrap_err(), Error::TxidMismatch);
    // Changing the message breaks the signature (non-malleable).
    let mut m = pd.clone();
    m.msg = b"invoice 43".to_vec();
    assert!(matches!(verify(&m, &data, 1), Err(Error::InvalidSignature { .. })));
    // Adding an output someone else found breaks the signature too.
    let mut m = pd.clone();
    m.outputs.clear();
    assert!(matches!(verify(&m, &data, 1), Err(Error::InvalidSignature { .. })));
    // Stripping the spend authority is not allowed.
    let mut m = pd.clone();
    m.spends.clear();
    assert_eq!(verify(&m, &data, 1).unwrap_err(), Error::NoSpendAuthority);
    // A tampered proof, rk or cv fails the proof.
    for i in 0..3 {
        let mut m = pd.clone();
        match i {
            0 => m.spends[0].proof[100] ^= 1,
            1 => m.spends[0].rk = f.bundle.actions().first().rk().into(),
            _ => m.spends[0].cv_net = f.bundle.actions().first().cv_net().to_bytes(),
        }
        assert!(matches!(verify(&m, &data, 1), Err(Error::InvalidProof { .. })), "case {i}");
    }
    // Claiming another action's nullifier fails the proof.
    let mut m = pd.clone();
    m.spends[0].index = (f.spend_index + 1) % f.bundle.actions().len() as u32;
    assert!(matches!(verify(&m, &data, 1), Err(Error::InvalidProof { .. })));
    // A wrong OCK does not decrypt.
    let mut m = pd.clone();
    m.outputs[0].ock[0] ^= 1;
    assert!(matches!(verify(&m, &data, 1), Err(Error::InvalidSignature { .. }) | Err(Error::OutputNotRecovered { .. })));

    // Someone who did not spend the note cannot create one, even with the right output keys.
    let (sk3, fvk3) = key(3);
    let mut s = spend(&f);
    s.fvk = fvk3;
    s.ask = SpendAuthorizingKey::from(&sk3);
    assert!(matches!(create(&data, 1, vec![], vec![], &[s], &mut rng), Err(Error::NotTheSpentNote { .. })));
    // Right note, wrong ask: signing fails.
    let mut s = spend(&f);
    s.ask = SpendAuthorizingKey::from(&sk3);
    assert!(matches!(create(&data, 1, vec![], vec![], &[s], &mut rng), Err(Error::Build(_))));
    // Wrong witness.
    let mut s = spend(&f);
    s.merkle_path = MerklePath::from_parts(1, s.merkle_path.auth_path());
    assert!(matches!(create(&data, 1, vec![], vec![], &[s], &mut rng), Err(Error::WrongAnchor { .. })));
}

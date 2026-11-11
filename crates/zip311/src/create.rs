//! Creating a payment disclosure (sender side; needs the spend authorizing key).

use orchard::builder::{Builder, BundleType};
use orchard::keys::{FullViewingKey, OutgoingViewingKey, SpendAuthorizingKey};
use orchard::note::ExtractedNoteCommitment;
use orchard::note_encryption::{IronwoodVersion, NoteEncryptionDomain, OrchardVersion};
use orchard::primitives::redpallas::{Signature, SpendAuth};
use orchard::tree::MerklePath;
use orchard::Note;
use rand::{CryptoRng, Rng};
use sapling::bundle::{GrothProofBytes, OutputDescription};
use zcash_note_encryption::{Domain, ShieldedOutput};

use crate::verify::{check_structure, recover_action, recover_sapling};
use crate::{Error, OutputDisclosure, PaymentDisclosure, Pool, SpendAuthority, TxData, proving_key};

pub(crate) type OrchardAction = orchard::Action<Signature<SpendAuth>>;

/// What the sender's wallet supplies to prove it controlled one spent Orchard/Ironwood note.
///
/// `merkle_path` must be the note's witness at the **transaction's own anchor** (ZIP 311 reuses
/// the transaction's anchor, so wallets only need the witness they already used to spend).
pub struct OrchardSpend {
    pub pool: Pool,
    /// Index of the action in the transaction whose nullifier this note produced.
    pub index: u32,
    pub fvk: FullViewingKey,
    pub ask: SpendAuthorizingKey,
    pub note: Note,
    pub merkle_path: MerklePath,
}

/// The OCK of one Orchard/Ironwood action, derived from the sender's outgoing viewing key.
pub fn orchard_ock(ovk: &OutgoingViewingKey, pool: Pool, action: &OrchardAction) -> [u8; 32] {
    fn ock<V: orchard::note_encryption::DomainVersion>(ovk: &OutgoingViewingKey, a: &OrchardAction) -> [u8; 32] {
        let cmstar = <OrchardAction as ShieldedOutput<NoteEncryptionDomain<V>>>::cmstar_bytes(a);
        let epk = <OrchardAction as ShieldedOutput<NoteEncryptionDomain<V>>>::ephemeral_key(a);
        <NoteEncryptionDomain<V> as Domain>::derive_ock(ovk, a.cv_net(), &cmstar, &epk).0
    }
    match pool {
        Pool::Ironwood => ock::<IronwoodVersion>(ovk, action),
        _ => ock::<OrchardVersion>(ovk, action),
    }
}

/// The OCK of one Sapling output, derived from the sender's Sapling outgoing viewing key.
pub fn sapling_ock(ovk: &sapling::keys::OutgoingViewingKey, output: &OutputDescription<GrothProofBytes>) -> [u8; 32] {
    sapling::note_encryption::prf_ock(ovk, output.cv(), &output.cmu().to_bytes(), output.ephemeral_key()).0
}

/// Every Orchard and Ironwood output of the transaction this OVK can recover, as disclosures.
///
/// Use the external-scope OVK for payments to others (change uses the internal scope).
pub fn sent_orchard_outputs<V>(tx: &TxData<V>, ovk: &OutgoingViewingKey) -> Vec<OutputDisclosure> {
    let mut out = Vec::new();
    for pool in [Pool::Orchard, Pool::Ironwood] {
        let Some(b) = tx.action_bundle(pool) else { continue };
        for (i, a) in b.actions().iter().enumerate() {
            let ock = orchard_ock(ovk, pool, a);
            if recover_action(pool, a, &ock).is_some() {
                out.push(OutputDisclosure { pool, index: i as u32, ock });
            }
        }
    }
    out
}

/// Every Sapling output of the transaction this OVK can recover, as disclosures.
pub fn sent_sapling_outputs<V>(tx: &TxData<V>, ovk: &sapling::keys::OutgoingViewingKey) -> Vec<OutputDisclosure> {
    let Some(b) = tx.sapling else { return Vec::new() };
    b.shielded_outputs()
        .iter()
        .enumerate()
        .filter_map(|(i, o)| {
            let ock = sapling_ock(ovk, o);
            recover_sapling(o, &ock, tx.zip212).map(|_| OutputDisclosure { pool: Pool::Sapling, index: i as u32, ock })
        })
        .collect()
}

/// Create a signed payment disclosure for `tx`.
///
/// For each [`OrchardSpend`], builds a fresh one-action Orchard proof that spends the same note at
/// the transaction's anchor (so the action's nullifier equals the transaction's), with a fresh
/// re-randomised key `rk`; then signs the ZIP 311 digest with `ask` re-randomised the same way.
/// `outputs` are checked to decrypt before anything is signed.
///
/// Proving takes a few seconds per spend (the proving key is built once per process).
pub fn create<V, R: Rng + CryptoRng>(
    tx: &TxData<V>,
    coin_type: u32,
    msg: Vec<u8>,
    outputs: Vec<OutputDisclosure>,
    spends: &[OrchardSpend],
    mut rng: R,
) -> Result<PaymentDisclosure, Error> {
    let build = |e: &dyn std::fmt::Debug| Error::Build(format!("{e:?}"));
    let mut pd = PaymentDisclosure { coin_type, txid: tx.txid, msg, outputs, spends: Vec::new() };
    let mut pending = Vec::with_capacity(spends.len());

    for s in spends {
        let (pool, index) = (s.pool, s.index);
        if pool == Pool::Sapling {
            return Err(Error::UnsupportedPool(pool));
        }
        let bundle = tx.action_bundle(pool).ok_or(Error::NoSuchPool(pool))?;
        let action = bundle.actions().get(index as usize).ok_or(Error::IndexOutOfRange { pool, index })?;
        if s.note.nullifier(&s.fvk) != *action.nullifier() {
            return Err(Error::NotTheSpentNote { pool, index });
        }
        if s.merkle_path.root(ExtractedNoteCommitment::from(s.note.commitment())) != *bundle.anchor() {
            return Err(Error::WrongAnchor { pool, index });
        }

        let version = crate::disclosure_version(bundle.bundle_version());
        let kind = BundleType::Transactional { bundle_required: true, pad_to_minimum: Some(1) };
        let mut b = Builder::new(kind, version, *bundle.flags(), *bundle.anchor()).map_err(|e| build(&e))?;
        b.add_spend(s.fvk.clone(), s.note, s.merkle_path.clone()).map_err(|e| build(&e))?;
        let (mut pczt, _) = b.build_for_pczt(&mut rng).map_err(|e| build(&e))?;
        pczt.create_proof(proving_key(version.circuit_version()), &mut rng).map_err(|e| build(&e))?;

        let [a] = pczt.actions().as_slice() else {
            return Err(Error::Build(format!("expected one action, got {}", pczt.actions().len())));
        };
        if a.spend().nullifier() != action.nullifier() {
            return Err(Error::NotTheSpentNote { pool, index });
        }
        let proof = pczt.zkproof().as_ref().ok_or_else(|| Error::Build("no proof".into()))?;
        pd.spends.push(SpendAuthority {
            pool,
            index,
            cv_net: a.cv_net().to_bytes(),
            rk: a.spend().rk().into(),
            cmx: a.output().cmx().to_bytes(),
            proof: proof.as_ref().to_vec(),
            spend_auth_sig: [0; 64],
        });
        pending.push(pczt);
    }

    // Everything must check out (indices, outputs decrypt) before we sign anything.
    check_structure(&pd, tx)?;
    for o in &pd.outputs {
        crate::verify::recover_output(tx, o)?;
    }

    let digest = pd.digest();
    for (i, (mut pczt, s)) in pending.into_iter().zip(spends).enumerate() {
        let a = &mut pczt.actions_mut()[0];
        a.sign(digest, &s.ask, &mut rng).map_err(|e| build(&e))?;
        let sig = a.spend().spend_auth_sig().as_ref().ok_or_else(|| Error::Build("not signed".into()))?;
        pd.spends[i].spend_auth_sig = sig.into();
    }
    Ok(pd)
}

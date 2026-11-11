//! Verifying a payment disclosure (anyone; no keys).

use std::collections::HashSet;

use orchard::Address;
use orchard::bundle::Flags;
use orchard::circuit::Instance;
use orchard::note::ExtractedNoteCommitment;
use orchard::note_encryption::{IronwoodVersion, NoteEncryptionDomain, OrchardVersion};
use orchard::primitives::redpallas::{Signature, SpendAuth, VerificationKey};
use orchard::value::ValueCommitment;
use orchard::Proof;
use sapling::bundle::{GrothProofBytes, OutputDescription};
use sapling::note_encryption::{Zip212Enforcement, try_sapling_output_recovery_with_ock};
use zcash_note_encryption::{OutgoingCipherKey, try_output_recovery_with_ock};

use crate::create::OrchardAction;
use crate::{Error, OutputDisclosure, PaymentDisclosure, Pool, TxData, verifying_key};

/// Who a disclosed output paid.
#[derive(Clone, Debug)]
pub enum Recipient {
    Sapling(sapling::PaymentAddress),
    /// An Orchard-protocol address (used by both the Orchard and Ironwood pools).
    Orchard(Address),
}

/// One output whose contents the disclosure proves.
#[derive(Clone, Debug)]
pub struct VerifiedOutput {
    pub pool: Pool,
    pub index: u32,
    pub recipient: Recipient,
    pub value_zat: u64,
    /// Raw 512-byte memo field (decode with `zcash_protocol::memo`).
    pub memo: [u8; 512],
}

/// What a valid disclosure proves.
#[derive(Clone, Debug)]
pub struct Verified {
    pub txid: [u8; 32],
    /// The message the sender signed (e.g. your challenge). Check it is what you expect.
    pub msg: Vec<u8>,
    /// Actions whose spend authority the discloser proved (they spent the note those actions nullify).
    pub spends: Vec<(Pool, u32)>,
    pub outputs: Vec<VerifiedOutput>,
}

impl Verified {
    /// The txid as usually displayed (byte-reversed hex).
    pub fn txid_hex(&self) -> String {
        crate::txid_hex(&self.txid)
    }
}

pub(crate) fn recover_action(pool: Pool, a: &OrchardAction, ock: &[u8; 32]) -> Option<(orchard::Note, Address, [u8; 512])> {
    fn go<V: orchard::note_encryption::DomainVersion>(a: &OrchardAction, ock: &OutgoingCipherKey) -> Option<(orchard::Note, Address, [u8; 512])> {
        let domain = NoteEncryptionDomain::<V>::for_action(a);
        let r = try_output_recovery_with_ock(&domain, ock, a, &a.encrypted_note().out_ciphertext)?;
        // The library checks the commitment already; compare cmx explicitly as well.
        (ExtractedNoteCommitment::from(r.0.commitment()) == *a.cmx()).then_some(r)
    }
    let ock = OutgoingCipherKey(*ock);
    match pool {
        Pool::Orchard => go::<OrchardVersion>(a, &ock),
        Pool::Ironwood => go::<IronwoodVersion>(a, &ock),
        Pool::Sapling => None,
    }
}

pub(crate) fn recover_sapling(
    o: &OutputDescription<GrothProofBytes>,
    ock: &[u8; 32],
    zip212: Zip212Enforcement,
) -> Option<(sapling::Note, sapling::PaymentAddress, [u8; 512])> {
    try_sapling_output_recovery_with_ock(&OutgoingCipherKey(*ock), o, zip212)
}

pub(crate) fn recover_output<V>(tx: &TxData<V>, o: &OutputDisclosure) -> Result<VerifiedOutput, Error> {
    let (pool, index) = (o.pool, o.index);
    let not = Error::OutputNotRecovered { pool, index };
    let (recipient, value_zat, memo) = match pool {
        Pool::Sapling => {
            let b = tx.sapling.ok_or(Error::NoSuchPool(pool))?;
            let out = b.shielded_outputs().get(index as usize).ok_or(Error::IndexOutOfRange { pool, index })?;
            let (note, addr, memo) = recover_sapling(out, &o.ock, tx.zip212).ok_or(not)?;
            (Recipient::Sapling(addr), note.value().inner(), memo)
        }
        Pool::Orchard | Pool::Ironwood => {
            let b = tx.action_bundle(pool).ok_or(Error::NoSuchPool(pool))?;
            let a = b.actions().get(index as usize).ok_or(Error::IndexOutOfRange { pool, index })?;
            let (note, addr, memo) = recover_action(pool, a, &o.ock).ok_or(not)?;
            (Recipient::Orchard(addr), note.value().inner(), memo)
        }
    };
    Ok(VerifiedOutput { pool, index, recipient, value_zat, memo })
}

fn pool_len<V>(tx: &TxData<V>, pool: Pool, spends: bool) -> Option<usize> {
    match pool {
        Pool::Sapling if spends => tx.sapling.map(|b| b.shielded_spends().len()),
        Pool::Sapling => tx.sapling.map(|b| b.shielded_outputs().len()),
        _ => tx.action_bundle(pool).map(|b| b.actions().len()),
    }
}

/// ZIP 311 structural checks: txid, ≥1 spend, indices unique and in range.
pub(crate) fn check_structure<V>(pd: &PaymentDisclosure, tx: &TxData<V>) -> Result<(), Error> {
    if pd.txid != tx.txid {
        return Err(Error::TxidMismatch);
    }
    if pd.spends.is_empty() {
        return Err(Error::NoSpendAuthority);
    }
    for (items, spends) in [(pd.outputs.iter().map(|o| (o.pool, o.index)).collect::<Vec<_>>(), false),
                            (pd.spends.iter().map(|s| (s.pool, s.index)).collect(), true)] {
        let mut seen = HashSet::new();
        for (pool, index) in items {
            if !seen.insert((pool, index)) {
                return Err(Error::DuplicateIndex { pool, index });
            }
            let len = pool_len(tx, pool, spends).ok_or(Error::NoSuchPool(pool))?;
            if index as usize >= len {
                return Err(Error::IndexOutOfRange { pool, index });
            }
        }
    }
    Ok(())
}

/// Verify `pd` against the mined transaction `tx` on the network with `coin_type`.
///
/// The caller must have fetched `tx` from the chain and checked that it was mined (and at the
/// height used to build `tx`). Returns what the disclosure proves, or the first failure.
pub fn verify<V>(pd: &PaymentDisclosure, tx: &TxData<V>, coin_type: u32) -> Result<Verified, Error> {
    if pd.coin_type != coin_type {
        return Err(Error::CoinTypeMismatch { expected: coin_type, found: pd.coin_type });
    }
    check_structure(pd, tx)?;
    let digest = pd.digest();

    for s in &pd.spends {
        let (pool, index) = (s.pool, s.index);
        if pool == Pool::Sapling {
            return Err(Error::UnsupportedPool(pool));
        }
        let bundle = tx.action_bundle(pool).ok_or(Error::NoSuchPool(pool))?;
        let action = &bundle.actions()[index as usize];
        let bad = Error::InvalidProof { pool, index };

        let rk = VerificationKey::<SpendAuth>::try_from(s.rk).map_err(|_| bad.clone())?;
        let cv_net = Option::from(ValueCommitment::from_bytes(&s.cv_net)).ok_or(bad.clone())?;
        let cmx = Option::from(ExtractedNoteCommitment::from_bytes(&s.cmx)).ok_or(bad.clone())?;
        let flags: Flags = *bundle.flags();
        // Anchor and nullifier come from the transaction: the proof is about *its* spent note.
        let instance = Instance::from_parts(*bundle.anchor(), cv_net, *action.nullifier(), rk.clone(), cmx, flags)
            .ok_or(bad.clone())?;
        let vk = verifying_key(bundle.bundle_version().circuit_version());
        Proof::new(s.proof.clone()).verify(vk, &[instance]).map_err(|_| bad)?;
        rk.verify(&digest, &Signature::<SpendAuth>::from(s.spend_auth_sig))
            .map_err(|_| Error::InvalidSignature { pool, index })?;
    }

    let outputs = pd.outputs.iter().map(|o| recover_output(tx, o)).collect::<Result<Vec<_>, _>>()?;
    Ok(Verified {
        txid: pd.txid,
        msg: pd.msg.clone(),
        spends: pd.spends.iter().map(|s| (s.pool, s.index)).collect(),
        outputs,
    })
}

//! Read-only decryption of one transaction with a Unified Full Viewing Key (`ghost verify --ufvk`).
//! Uses librustzcash @ eb38d7a (`decrypt_transaction`), including the NU7 **Ironwood** pool:
//! Ironwood outputs are Orchard-shaped and decrypted with the UFVK's Orchard key, but are reported
//! separately (`ironwood_outputs`). A viewing key cannot spend; Ghost never handles spending keys.

use std::collections::HashMap;

use serde::Serialize;
use zcash_client_backend::{TransferType, decrypt_transaction};
use zcash_keys::keys::UnifiedFullViewingKey;
use zcash_primitives::transaction::Transaction;
use zcash_protocol::consensus::{BlockHeight, BranchId, Network};
use zcash_protocol::memo::{Memo, MemoBytes};

use crate::Fail;

#[derive(Serialize, Debug)]
pub(crate) struct Output {
    pub pool: &'static str,
    pub index: usize,
    pub value_zat: u64,
    pub transfer: &'static str,
    pub memo: String,
}

fn memo_text(m: &MemoBytes) -> String {
    match Memo::try_from(m) {
        Ok(Memo::Text(t)) => t.to_string().chars().filter(|c| !c.is_control()).collect(),
        Ok(Memo::Empty) => "(empty)".into(),
        Ok(_) => "(non-text memo)".into(),
        Err(_) => "(invalid memo)".into(),
    }
}

fn transfer(t: TransferType) -> &'static str {
    match t {
        TransferType::Incoming => "incoming",
        TransferType::AccountInternal => "change",
        TransferType::WalletInternal => "wallet-internal",
        TransferType::Outgoing => "outgoing",
    }
}

pub(crate) fn decrypt(ufvk: &str, raw: &[u8], height: u64) -> Result<Vec<Output>, Fail> {
    let params = Network::TestNetwork;
    let key = UnifiedFullViewingKey::decode(&params, ufvk.trim())
        .map_err(|e| Fail::usage(format!("not a testnet UFVK: {e}"), "pass the uviewtest1… key of the receiving wallet"))?;
    let h = BlockHeight::from_u32(u32::try_from(height).map_err(|_| Fail::runtime("height out of range", "check the txid"))?);
    let tx = Transaction::read(raw, BranchId::for_height(&params, h))
        .map_err(|e| Fail::runtime(format!("cannot parse transaction: {e}"), "check the txid"))?;
    let keys = HashMap::from([(0usize, key)]);
    let d = decrypt_transaction(&params, Some(h), None, &tx, &keys);
    let mut out = Vec::new();
    for o in d.sapling_outputs() {
        out.push(Output { pool: "sapling", index: o.index(), value_zat: o.note_value().into_u64(), transfer: transfer(o.transfer_type()), memo: memo_text(o.memo()) });
    }
    for o in d.orchard_outputs() {
        out.push(Output { pool: "orchard", index: o.index(), value_zat: o.note().0.value().inner(), transfer: transfer(o.transfer_type()), memo: memo_text(o.memo()) });
    }
    for o in d.ironwood_outputs() {
        out.push(Output { pool: "ironwood", index: o.index(), value_zat: o.note().0.value().inner(), transfer: transfer(o.transfer_type()), memo: memo_text(o.memo()) });
    }
    Ok(out)
}

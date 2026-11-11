//! A real disclosure for a real Zcash testnet payment (tx 4d66c913…, mined at height 4608562 on
//! 2026-11-11, created with `zcash-devtool wallet disclose`). The raw transaction is public chain data.
#![cfg(feature = "tx")]

use zcash_primitives::transaction::Transaction;
use zcash_protocol::consensus::{BlockHeight, BranchId, Network};
use zcash_protocol::memo::{Memo, MemoBytes};

const TXID: &str = "4d66c913f474717dcf2f7d5e54bb59f972a41e29099cf53b76d1eafddccecfc6";
const HEIGHT: u32 = 4_608_562;

#[test]
#[cfg_attr(debug_assertions, ignore = "Halo2 key generation is slow in debug; run with --release")]
fn verifies_real_testnet_disclosure() {
    let net = Network::TestNetwork;
    let h = BlockHeight::from_u32(HEIGHT);
    let raw = hex_decode(include_str!("data/testnet_4d66c913.tx.hex").trim());
    let tx = Transaction::read(&raw[..], BranchId::for_height(&net, h)).unwrap();
    assert_eq!(tx.txid().to_string(), TXID);
    let data = zip311::TxData::from_tx(&tx, &net, h);

    let pd = zip311::PaymentDisclosure::decode(include_str!("data/testnet_4d66c913.zpd")).unwrap();
    assert_eq!(pd.txid_hex(), TXID);
    let v = zip311::verify(&pd, &data, zip311::COIN_TYPE_TESTNET).unwrap();
    assert_eq!(v.msg, b"challenge: zip311-demo-2026-11-11");
    assert_eq!(v.spends, vec![(zip311::Pool::Ironwood, 0)]);
    assert_eq!(v.outputs.len(), 1);
    assert_eq!(v.outputs[0].value_zat, 50_000);
    let memo = Memo::try_from(MemoBytes::from_bytes(&v.outputs[0].memo).unwrap()).unwrap();
    assert!(matches!(memo, Memo::Text(t) if &*t == "ZIP 311 demo payment"));

    // Same disclosure on mainnet's coin type, or with any byte of the message changed, fails.
    assert!(zip311::verify(&pd, &data, zip311::COIN_TYPE_MAINNET).is_err());
    let mut m = pd.clone();
    m.msg[0] ^= 1;
    assert!(matches!(zip311::verify(&m, &data, zip311::COIN_TYPE_TESTNET), Err(zip311::Error::InvalidSignature { .. })));
}

fn hex_decode(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

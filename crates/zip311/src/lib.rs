//! # ZIP 311 payment disclosures
//!
//! A payment disclosure lets the **sender** of a shielded Zcash transaction prove, to anyone they
//! choose:
//!
//! - that they **control the spend authority** used in that transaction (a fresh zero-knowledge
//!   proof for a note the transaction spent, plus a signature under a re-randomised key), and
//! - what **chosen outputs** of that transaction contain (recipient, amount, memo), by revealing
//!   only the per-output outgoing cipher key (OCK).
//!
//! Nothing else is revealed: not the sender's address or viewing key, not their other
//! transactions, not the other outputs. Verification needs only the disclosure and the mined
//! transaction.
//!
//! This crate implements [ZIP 311] (Draft) for the **Orchard** and **Ironwood** pools (spend
//! authority and outputs) and for **Sapling outputs**. ZIP 311 leaves the Orchard construction
//! and the byte/text encodings as TODOs; the choices made here are written down in
//! `SPEC.md` (shipped with this crate) as a proposal for the ZIP. Not yet supported: Sapling
//! spend authority, transparent inputs (BIP 322 is still a TODO in the ZIP), and the optional
//! ZIP 304 sender-address proof.
//!
//! ```ignore
//! # fn demo(tx: &zcash_primitives::transaction::Transaction, height: zcash_protocol::consensus::BlockHeight,
//! #         spend: zip311::OrchardSpend, ovk: orchard::keys::OutgoingViewingKey) -> Result<(), zip311::Error> {
//! use zcash_protocol::consensus::Network;
//! let net = Network::TestNetwork;
//! let data = zip311::TxData::from_tx(tx, &net, height);
//! // Sender (wallet side; needs the spending key for the spend authority):
//! let outputs = zip311::sent_orchard_outputs(&data, &ovk);
//! let pd = zip311::create(&data, zip311::coin_type(&net), b"invoice 42".to_vec(), outputs, &[spend], rand::rng())?;
//! let text = pd.encode(); // "zpd1…"
//! // Anyone (no keys):
//! let pd = zip311::PaymentDisclosure::decode(&text)?;
//! let report = zip311::verify(&pd, &data, zip311::coin_type(&net))?;
//! # Ok(()) }
//! ```
//!
//! [ZIP 311]: https://zips.z.cash/zip-0311

mod create;
mod encoding;
mod verify;

pub use create::{OrchardSpend, create, orchard_ock, sapling_ock, sent_orchard_outputs, sent_sapling_outputs};
pub use encoding::{FORMAT_VERSION, TEXT_PREFIX};
pub use verify::{Recipient, Verified, VerifiedOutput, verify};

use std::fmt;
use std::sync::OnceLock;

use orchard::circuit::{OrchardCircuitVersion, ProvingKey, VerifyingKey};
use orchard::bundle::Authorized as OrchardAuthorized;
use sapling::bundle::Authorized as SaplingAuthorized;
use sapling::note_encryption::Zip212Enforcement;
#[cfg(feature = "tx")]
use zcash_primitives::transaction::{Transaction, components::sapling::zip212_enforcement};
#[cfg(feature = "tx")]
use zcash_protocol::consensus::{BlockHeight, NetworkType, Parameters};
#[cfg(feature = "tx")]
use zcash_protocol::value::ZatBalance;

/// SLIP-44 coin type of Zcash mainnet (index form).
pub const COIN_TYPE_MAINNET: u32 = 133;
/// SLIP-44 coin type of every Zcash test network (index form).
pub const COIN_TYPE_TESTNET: u32 = 1;

/// BLAKE2b personalisation prefix for the signed digest (ZIP 311); the 4-byte coin type follows.
pub const SIG_PERSONALIZATION: &[u8; 12] = b"ZIP311Signed";

/// A shielded pool that a disclosed output or spend belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Pool {
    Sapling,
    Orchard,
    Ironwood,
}

impl Pool {
    pub(crate) fn to_byte(self) -> u8 {
        match self {
            Pool::Sapling => 0,
            Pool::Orchard => 1,
            Pool::Ironwood => 2,
        }
    }
    pub(crate) fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(Pool::Sapling),
            1 => Some(Pool::Orchard),
            2 => Some(Pool::Ironwood),
            _ => None,
        }
    }
    /// Lower-case name ("sapling", "orchard", "ironwood").
    pub fn name(self) -> &'static str {
        match self {
            Pool::Sapling => "sapling",
            Pool::Orchard => "orchard",
            Pool::Ironwood => "ironwood",
        }
    }
}

impl fmt::Display for Pool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// One disclosed output: its position in the transaction and the key that decrypts only it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputDisclosure {
    pub pool: Pool,
    /// Index into the pool's outputs (Sapling) or actions (Orchard/Ironwood).
    pub index: u32,
    /// Outgoing cipher key for this one output.
    pub ock: [u8; 32],
}

/// Proof of spend authority for one Orchard/Ironwood action of the transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpendAuthority {
    /// [`Pool::Orchard`] or [`Pool::Ironwood`].
    pub pool: Pool,
    /// Index of the action whose nullifier is being claimed.
    pub index: u32,
    /// Public inputs of the fresh action proof (the nullifier and anchor come from the transaction).
    pub cv_net: [u8; 32],
    pub rk: [u8; 32],
    pub cmx: [u8; 32],
    /// Halo2 proof for one action, at the transaction's own anchor.
    pub proof: Vec<u8>,
    /// RedPallas SpendAuth signature under `rk` over the disclosure digest.
    pub spend_auth_sig: [u8; 64],
}

/// A ZIP 311 payment disclosure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaymentDisclosure {
    /// SLIP-44 coin type, index form: 133 mainnet, 1 testnet/regtest. Bound into the signed digest.
    pub coin_type: u32,
    /// Transaction id, in its internal byte order (as `TxId::as_ref()`; displayed reversed).
    pub txid: [u8; 32],
    /// Free-form message, e.g. a challenge from the verifier. Covered by the signatures.
    pub msg: Vec<u8>,
    pub outputs: Vec<OutputDisclosure>,
    pub spends: Vec<SpendAuthority>,
}

impl PaymentDisclosure {
    /// The txid as it is usually displayed (byte-reversed hex).
    pub fn txid_hex(&self) -> String {
        txid_hex(&self.txid)
    }
}

pub(crate) fn txid_hex(txid: &[u8; 32]) -> String {
    txid.iter().rev().map(|b| format!("{b:02x}")).collect()
}

/// SLIP-44 coin type (index form) for a network: 133 for mainnet, 1 otherwise.
#[cfg(feature = "tx")]
pub fn coin_type(params: &impl Parameters) -> u32 {
    match params.network_type() {
        NetworkType::Main => COIN_TYPE_MAINNET,
        NetworkType::Test | NetworkType::Regtest => COIN_TYPE_TESTNET,
    }
}

/// The parts of a mined transaction a disclosure refers to.
///
/// Build it with `TxData::from_tx` (feature `tx`); the fields are public so callers (and tests) can supply
/// bundles from elsewhere. The caller is responsible for having checked that the transaction was
/// mined at the height used for `zip212`.
pub struct TxData<'a, V> {
    pub txid: [u8; 32],
    pub sapling: Option<&'a sapling::Bundle<SaplingAuthorized, V>>,
    pub orchard: Option<&'a orchard::Bundle<OrchardAuthorized, V>>,
    pub ironwood: Option<&'a orchard::Bundle<OrchardAuthorized, V>>,
    /// ZIP 212 rule for Sapling note plaintexts at the mined height.
    pub zip212: Zip212Enforcement,
}

#[cfg(feature = "tx")]
impl<'a> TxData<'a, ZatBalance> {
    /// Take the shielded bundles of `tx`, mined at `height` on `params`.
    pub fn from_tx(tx: &'a Transaction, params: &impl Parameters, height: BlockHeight) -> Self {
        TxData {
            txid: *tx.txid().as_ref(),
            sapling: tx.sapling_bundle(),
            orchard: tx.orchard_bundle(),
            ironwood: tx.ironwood_bundle(),
            zip212: zip212_enforcement(params, height),
        }
    }
}

impl<V> TxData<'_, V> {
    pub(crate) fn action_bundle(&self, pool: Pool) -> Option<&orchard::Bundle<OrchardAuthorized, V>> {
        match pool {
            Pool::Orchard => self.orchard,
            Pool::Ironwood => self.ironwood,
            Pool::Sapling => None,
        }
    }
}

/// Everything that can go wrong creating, decoding or verifying a disclosure.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The bytes or text are not a well-formed disclosure.
    Encoding(String),
    /// The disclosure is for another transaction.
    TxidMismatch,
    /// The disclosure is for another network (coin type).
    CoinTypeMismatch { expected: u32, found: u32 },
    /// ZIP 311 requires at least one proof of spend authority.
    NoSpendAuthority,
    /// The transaction has no bundle for this pool.
    NoSuchPool(Pool),
    /// An index is out of range for the transaction.
    IndexOutOfRange { pool: Pool, index: u32 },
    /// The same index appears twice.
    DuplicateIndex { pool: Pool, index: u32 },
    /// Spend authority is not implemented for this pool (Sapling).
    UnsupportedPool(Pool),
    /// Creating: the note is not the one spent by that action (nullifier differs).
    NotTheSpentNote { pool: Pool, index: u32 },
    /// Creating: the Merkle path does not lead to the transaction's anchor.
    WrongAnchor { pool: Pool, index: u32 },
    /// Creating: building or proving the action failed.
    Build(String),
    /// Verifying: the zero-knowledge proof of spend authority is invalid.
    InvalidProof { pool: Pool, index: u32 },
    /// Verifying: the spend authorisation signature is invalid.
    InvalidSignature { pool: Pool, index: u32 },
    /// Verifying: the OCK does not decrypt this output, or the note does not match the chain.
    OutputNotRecovered { pool: Pool, index: u32 },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Encoding(e) => write!(f, "malformed payment disclosure: {e}"),
            Error::TxidMismatch => f.write_str("the disclosure is for a different transaction"),
            Error::CoinTypeMismatch { expected, found } => {
                write!(f, "the disclosure is for coin type {found}, expected {expected} (wrong network)")
            }
            Error::NoSpendAuthority => f.write_str("the disclosure proves no spend authority (ZIP 311 requires at least one)"),
            Error::NoSuchPool(p) => write!(f, "the transaction has no {p} bundle"),
            Error::IndexOutOfRange { pool, index } => write!(f, "{pool} index {index} is out of range"),
            Error::DuplicateIndex { pool, index } => write!(f, "{pool} index {index} appears more than once"),
            Error::UnsupportedPool(p) => write!(f, "spend authority for the {p} pool is not supported"),
            Error::NotTheSpentNote { pool, index } => {
                write!(f, "{pool} action {index} does not spend this note (nullifier differs)")
            }
            Error::WrongAnchor { pool, index } => {
                write!(f, "the Merkle path for {pool} action {index} does not lead to the transaction's anchor")
            }
            Error::Build(e) => write!(f, "could not build the spend proof: {e}"),
            Error::InvalidProof { pool, index } => write!(f, "invalid spend-authority proof for {pool} action {index}"),
            Error::InvalidSignature { pool, index } => write!(f, "invalid spend-authority signature for {pool} action {index}"),
            Error::OutputNotRecovered { pool, index } => {
                write!(f, "{pool} output {index}: the key does not decrypt it, or its note does not match the chain")
            }
        }
    }
}

impl std::error::Error for Error {}

/// Proving / verifying keys are expensive to build (seconds); build each version once per process.
const VERSIONS: [OrchardCircuitVersion; 3] = [
    OrchardCircuitVersion::InsecurePreNu6_2,
    OrchardCircuitVersion::FixedPostNu6_2,
    OrchardCircuitVersion::PostNu6_3,
];

fn slot(v: OrchardCircuitVersion) -> usize {
    VERSIONS.iter().position(|x| *x == v).expect("known circuit version")
}

pub(crate) fn proving_key(v: OrchardCircuitVersion) -> &'static ProvingKey {
    static PK: [OnceLock<ProvingKey>; 3] = [OnceLock::new(), OnceLock::new(), OnceLock::new()];
    PK[slot(v)].get_or_init(|| ProvingKey::build(v))
}

pub(crate) fn verifying_key(v: OrchardCircuitVersion) -> &'static VerifyingKey {
    static VK: [OnceLock<VerifyingKey>; 3] = [OnceLock::new(), OnceLock::new(), OnceLock::new()];
    VK[slot(v)].get_or_init(|| VerifyingKey::build(v))
}

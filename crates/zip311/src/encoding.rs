//! Byte and text encodings (ZIP 311 leaves these as TODOs; this is the proposal in SPEC.md).
//!
//! ```text
//! unsigned = version:u8 (=1) || coin_type:u32le || txid:[32] || msg:bytes
//!         || n_outputs:cs || { pool:u8 || index:u32le || ock:[32] }*
//!         || n_spends:cs  || { pool:u8 || index:u32le || cv_net:[32] || rk:[32] || cmx:[32] || proof:bytes }*
//! signed   = unsigned || { spend_auth_sig:[64] }*        (one per spend, same order)
//! bytes    = compactsize length || data;  cs = Bitcoin CompactSize
//! digest   = BLAKE2b-256(personal = "ZIP311Signed" || coin_type:u32le, unsigned)
//! text     = "zpd1" || base64url-nopad(signed)
//! ```

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::{Error, OutputDisclosure, PaymentDisclosure, Pool, SIG_PERSONALIZATION, SpendAuthority};

/// Version byte of the encoding defined here.
pub const FORMAT_VERSION: u8 = 1;
/// Text form prefix ("Zcash Payment Disclosure", format 1).
pub const TEXT_PREFIX: &str = "zpd1";

// Decoding limits, so a hostile string cannot make us allocate without bound.
const MAX_MSG: usize = 4096;
const MAX_ITEMS: usize = 256;
const MAX_PROOF: usize = 1 << 16;
const MAX_TEXT: usize = 2 * 1024 * 1024;

fn put_cs(out: &mut Vec<u8>, n: usize) {
    let n = n as u64;
    match n {
        0..=0xfc => out.push(n as u8),
        0xfd..=0xffff => {
            out.push(0xfd);
            out.extend_from_slice(&(n as u16).to_le_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(0xfe);
            out.extend_from_slice(&(n as u32).to_le_bytes());
        }
        _ => {
            out.push(0xff);
            out.extend_from_slice(&n.to_le_bytes());
        }
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if self.0.len() < n {
            return Err(Error::Encoding("truncated".into()));
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().expect("4 bytes")))
    }
    fn arr<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        Ok(self.take(N)?.try_into().expect("N bytes"))
    }
    /// CompactSize, rejecting non-canonical forms and values above `max`.
    fn cs(&mut self, max: usize) -> Result<usize, Error> {
        let n = match self.u8()? {
            b @ 0..=0xfc => b as u64,
            0xfd => {
                let v = u16::from_le_bytes(self.arr()?) as u64;
                if v < 0xfd { return Err(Error::Encoding("non-canonical length".into())); }
                v
            }
            0xfe => {
                let v = u32::from_le_bytes(self.arr()?) as u64;
                if v <= 0xffff { return Err(Error::Encoding("non-canonical length".into())); }
                v
            }
            0xff => {
                let v = u64::from_le_bytes(self.arr()?);
                if v <= 0xffff_ffff { return Err(Error::Encoding("non-canonical length".into())); }
                v
            }
        };
        if n > max as u64 {
            return Err(Error::Encoding(format!("length {n} exceeds limit {max}")));
        }
        Ok(n as usize)
    }
    fn bytes(&mut self, max: usize) -> Result<Vec<u8>, Error> {
        let n = self.cs(max)?;
        Ok(self.take(n)?.to_vec())
    }
}

impl PaymentDisclosure {
    /// The encoding without signatures (what the signatures cover).
    pub fn unsigned_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 + self.spends.len() * 5200);
        out.push(FORMAT_VERSION);
        out.extend_from_slice(&self.coin_type.to_le_bytes());
        out.extend_from_slice(&self.txid);
        put_cs(&mut out, self.msg.len());
        out.extend_from_slice(&self.msg);
        put_cs(&mut out, self.outputs.len());
        for o in &self.outputs {
            out.push(o.pool.to_byte());
            out.extend_from_slice(&o.index.to_le_bytes());
            out.extend_from_slice(&o.ock);
        }
        put_cs(&mut out, self.spends.len());
        for s in &self.spends {
            out.push(s.pool.to_byte());
            out.extend_from_slice(&s.index.to_le_bytes());
            out.extend_from_slice(&s.cv_net);
            out.extend_from_slice(&s.rk);
            out.extend_from_slice(&s.cmx);
            put_cs(&mut out, s.proof.len());
            out.extend_from_slice(&s.proof);
        }
        out
    }

    /// The 32-byte digest every spend-authority signature signs.
    pub fn digest(&self) -> [u8; 32] {
        let mut personal = [0u8; 16];
        personal[..12].copy_from_slice(SIG_PERSONALIZATION);
        personal[12..].copy_from_slice(&self.coin_type.to_le_bytes());
        let h = blake2b_simd::Params::new().hash_length(32).personal(&personal).hash(&self.unsigned_bytes());
        h.as_bytes().try_into().expect("32 bytes")
    }

    /// The full (signed) byte encoding.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = self.unsigned_bytes();
        for s in &self.spends {
            out.extend_from_slice(&s.spend_auth_sig);
        }
        out
    }

    /// Parse the signed byte encoding. This checks the form only; use [`crate::verify`] to check it.
    pub fn from_bytes(b: &[u8]) -> Result<Self, Error> {
        let mut r = Reader(b);
        let v = r.u8()?;
        if v != FORMAT_VERSION {
            return Err(Error::Encoding(format!("unknown format version {v}")));
        }
        let coin_type = r.u32()?;
        let txid = r.arr()?;
        let msg = r.bytes(MAX_MSG)?;
        let n = r.cs(MAX_ITEMS)?;
        let mut outputs = Vec::with_capacity(n);
        for _ in 0..n {
            let pool = Pool::from_byte(r.u8()?).ok_or_else(|| Error::Encoding("unknown pool".into()))?;
            outputs.push(OutputDisclosure { pool, index: r.u32()?, ock: r.arr()? });
        }
        let n = r.cs(MAX_ITEMS)?;
        let mut spends = Vec::with_capacity(n);
        for _ in 0..n {
            let pool = Pool::from_byte(r.u8()?).ok_or_else(|| Error::Encoding("unknown pool".into()))?;
            spends.push(SpendAuthority {
                pool,
                index: r.u32()?,
                cv_net: r.arr()?,
                rk: r.arr()?,
                cmx: r.arr()?,
                proof: r.bytes(MAX_PROOF)?,
                spend_auth_sig: [0; 64],
            });
        }
        for s in &mut spends {
            s.spend_auth_sig = r.arr()?;
        }
        if !r.0.is_empty() {
            return Err(Error::Encoding("trailing bytes".into()));
        }
        Ok(PaymentDisclosure { coin_type, txid, msg, outputs, spends })
    }

    /// Text form: `zpd1` + base64url (no padding).
    pub fn encode(&self) -> String {
        format!("{TEXT_PREFIX}{}", URL_SAFE_NO_PAD.encode(self.to_bytes()))
    }

    /// Parse the text form (surrounding whitespace is ignored).
    pub fn decode(s: &str) -> Result<Self, Error> {
        let s = s.trim();
        if s.len() > MAX_TEXT {
            return Err(Error::Encoding("too long".into()));
        }
        let body = s.strip_prefix(TEXT_PREFIX).ok_or_else(|| Error::Encoding(format!("does not start with {TEXT_PREFIX}")))?;
        let bytes = URL_SAFE_NO_PAD.decode(body).map_err(|_| Error::Encoding("bad base64url".into()))?;
        Self::from_bytes(&bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PaymentDisclosure {
        PaymentDisclosure {
            coin_type: 1,
            txid: [7; 32],
            msg: b"challenge".to_vec(),
            outputs: vec![OutputDisclosure { pool: Pool::Ironwood, index: 1, ock: [9; 32] }],
            spends: vec![SpendAuthority {
                pool: Pool::Ironwood,
                index: 0,
                cv_net: [1; 32],
                rk: [2; 32],
                cmx: [3; 32],
                proof: vec![4; 300],
                spend_auth_sig: [5; 64],
            }],
        }
    }

    #[test]
    fn roundtrip_bytes_and_text() {
        let pd = sample();
        assert_eq!(PaymentDisclosure::from_bytes(&pd.to_bytes()).unwrap(), pd);
        let t = pd.encode();
        assert!(t.starts_with("zpd1"));
        assert_eq!(PaymentDisclosure::decode(&format!("  {t}\n")).unwrap(), pd);
    }

    #[test]
    fn digest_excludes_signatures_and_binds_everything_else() {
        let pd = sample();
        let mut other = pd.clone();
        other.spends[0].spend_auth_sig = [6; 64];
        assert_eq!(pd.digest(), other.digest());
        for change in [
            |p: &mut PaymentDisclosure| p.msg.push(b'!'),
            |p: &mut PaymentDisclosure| p.coin_type = 133,
            |p: &mut PaymentDisclosure| p.txid[0] ^= 1,
            |p: &mut PaymentDisclosure| p.outputs[0].ock[0] ^= 1,
            |p: &mut PaymentDisclosure| p.spends[0].proof[0] ^= 1,
            |p: &mut PaymentDisclosure| p.spends[0].rk[0] ^= 1,
        ] {
            let mut p = pd.clone();
            change(&mut p);
            assert_ne!(pd.digest(), p.digest());
        }
    }

    #[test]
    fn rejects_malformed() {
        let b = sample().to_bytes();
        assert!(PaymentDisclosure::from_bytes(&b[..b.len() - 1]).is_err());
        let mut long = b.clone();
        long.push(0);
        assert!(PaymentDisclosure::from_bytes(&long).is_err());
        let mut v = b.clone();
        v[0] = 2;
        assert!(PaymentDisclosure::from_bytes(&v).is_err());
        assert!(PaymentDisclosure::decode("zecproof1abc").is_err());
        assert!(PaymentDisclosure::decode("zpd1!!!").is_err());
        // non-canonical CompactSize for msg length: 0xfd 0x09 0x00
        let mut nc = b[..37].to_vec();
        nc.extend_from_slice(&[0xfd, 9, 0]);
        nc.extend_from_slice(&b[38..]);
        assert!(PaymentDisclosure::from_bytes(&nc).is_err());
    }
}

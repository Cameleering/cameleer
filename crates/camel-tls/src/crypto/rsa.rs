//! RSA signatures: PKCS#1 v1.5 and RSASSA-PSS (RFC 8017) verify and sign.
//!
//! Private-key operations use a fixed square-and-multiply-always Montgomery
//! exponentiation over the full exponent width (no CRT, no blinding). This hides
//! the exponent bit pattern from simple timing measurements but is not
//! hardened against cache or power side channels.

use super::bigint::{bit_len, from_be_bytes, lt, to_be_bytes, MontCtx};
use super::hash::HashAlg;
use super::rng;
use crate::util::ct_eq;

#[derive(Clone)]
pub struct RsaPublicKey {
    ctx: MontCtx,
    e: Vec<u64>,
    /// Modulus size in bytes.
    k: usize,
    mod_bits: usize,
}

#[derive(Clone)]
pub struct RsaPrivateKey {
    pub public: RsaPublicKey,
    d: Vec<u64>,
    /// The original PKCS#1 encoding (with CRT parameters), when parsed from one.
    pub(crate) pkcs1: Option<Vec<u8>>,
}

impl std::fmt::Debug for RsaPublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RsaPublicKey({} bits)", self.mod_bits)
    }
}

impl std::fmt::Debug for RsaPrivateKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RsaPrivateKey({} bits)", self.public.mod_bits)
    }
}

fn digest_info_prefix(h: HashAlg) -> &'static [u8] {
    match h {
        HashAlg::Sha1 => &[0x30, 0x21, 0x30, 0x09, 0x06, 0x05, 0x2b, 0x0e, 0x03, 0x02, 0x1a, 0x05, 0x00, 0x04, 0x14],
        HashAlg::Sha256 => &[
            0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01, 0x05, 0x00, 0x04, 0x20,
        ],
        HashAlg::Sha384 => &[
            0x30, 0x41, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x02, 0x05, 0x00, 0x04, 0x30,
        ],
        HashAlg::Sha512 => &[
            0x30, 0x51, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x03, 0x05, 0x00, 0x04, 0x40,
        ],
    }
}

fn mgf1(hash: HashAlg, seed: &[u8], len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + hash.output_len());
    let mut counter = 0u32;
    while out.len() < len {
        let mut input = seed.to_vec();
        input.extend_from_slice(&counter.to_be_bytes());
        out.extend(hash.hash(&input));
        counter += 1;
    }
    out.truncate(len);
    out
}

impl RsaPublicKey {
    /// Builds a key from big-endian modulus and exponent.
    pub fn new(n: &[u8], e: &[u8]) -> Option<RsaPublicKey> {
        let n = {
            let mut i = 0;
            while i < n.len() && n[i] == 0 {
                i += 1;
            }
            &n[i..]
        };
        if n.len() < 64 || n.len() > 1024 || n[n.len() - 1] & 1 == 0 {
            return None; // < 512 bits or > 8192 bits or even
        }
        let limbs = n.len().div_ceil(8);
        let nl = from_be_bytes(n, limbs)?;
        let el = from_be_bytes(e, limbs)?;
        if el.iter().all(|x| *x == 0) || !lt(&el, &nl) {
            return None;
        }
        let mod_bits = bit_len(&nl);
        Some(RsaPublicKey { ctx: MontCtx::new(nl), e: el, k: mod_bits.div_ceil(8), mod_bits })
    }

    pub fn modulus_bits(&self) -> usize {
        self.mod_bits
    }

    /// Big-endian (modulus, public exponent).
    pub fn components(&self) -> (Vec<u8>, Vec<u8>) {
        let n = to_be_bytes(&self.ctx.m, self.k);
        let mut e = to_be_bytes(&self.e, self.k);
        while e.len() > 1 && e[0] == 0 {
            e.remove(0);
        }
        (n, e)
    }

    pub fn size(&self) -> usize {
        self.k
    }

    fn encrypt_raw(&self, sig: &[u8]) -> Option<Vec<u8>> {
        if sig.len() != self.k {
            return None;
        }
        let s = from_be_bytes(sig, self.ctx.n())?;
        if !lt(&s, &self.ctx.m) {
            return None;
        }
        let m = self.ctx.pow(&s, &self.e, false);
        Some(to_be_bytes(&m, self.k))
    }

    /// RSASSA-PKCS1-v1_5 verification over a digest.
    pub fn verify_pkcs1(&self, hash: HashAlg, digest: &[u8], sig: &[u8]) -> bool {
        let em = match self.encrypt_raw(sig) {
            Some(e) => e,
            None => return false,
        };
        match pkcs1_encode(hash, digest, self.k) {
            Some(expect) => ct_eq(&em, &expect),
            None => false,
        }
    }

    /// RSASSA-PSS verification over a digest with MGF1 using the same hash.
    /// `salt_len` = None accepts any salt length.
    pub fn verify_pss(&self, hash: HashAlg, digest: &[u8], sig: &[u8], salt_len: Option<usize>) -> bool {
        let em_full = match self.encrypt_raw(sig) {
            Some(e) => e,
            None => return false,
        };
        let em_bits = self.mod_bits - 1;
        let em_len = em_bits.div_ceil(8);
        let em = if em_len < self.k {
            if em_full[0] != 0 {
                return false;
            }
            &em_full[1..]
        } else {
            &em_full[..]
        };
        let hlen = hash.output_len();
        if digest.len() != hlen || em_len < hlen + 2 || em[em_len - 1] != 0xbc {
            return false;
        }
        let masked_db = &em[..em_len - hlen - 1];
        let h = &em[em_len - hlen - 1..em_len - 1];
        let top_bits = 8 * em_len - em_bits;
        let top_mask: u8 = if top_bits == 0 { 0 } else { 0xffu8 << (8 - top_bits) };
        if masked_db[0] & top_mask != 0 {
            return false;
        }
        let db_mask = mgf1(hash, h, masked_db.len());
        let mut db: Vec<u8> = masked_db.iter().zip(db_mask.iter()).map(|(a, b)| a ^ b).collect();
        db[0] &= !top_mask;
        let pos = match db.iter().position(|b| *b != 0) {
            Some(p) => p,
            None => return false,
        };
        if db[pos] != 1 {
            return false;
        }
        let salt = &db[pos + 1..];
        if let Some(sl) = salt_len {
            if salt.len() != sl {
                return false;
            }
        }
        let mut mp = vec![0u8; 8];
        mp.extend_from_slice(digest);
        mp.extend_from_slice(salt);
        ct_eq(&hash.hash(&mp), h)
    }
}

fn pkcs1_encode(hash: HashAlg, digest: &[u8], k: usize) -> Option<Vec<u8>> {
    let prefix = digest_info_prefix(hash);
    let t_len = prefix.len() + digest.len();
    if digest.len() != hash.output_len() || k < t_len + 11 {
        return None;
    }
    let mut em = vec![0x00, 0x01];
    em.resize(k - t_len - 1, 0xff);
    em.push(0);
    em.extend_from_slice(prefix);
    em.extend_from_slice(digest);
    Some(em)
}

impl RsaPrivateKey {
    /// Builds a private key from big-endian n, e, d.
    pub fn new(n: &[u8], e: &[u8], d: &[u8]) -> Option<RsaPrivateKey> {
        let public = RsaPublicKey::new(n, e)?;
        let d = from_be_bytes(d, public.ctx.n())?;
        if !lt(&d, &public.ctx.m) {
            return None;
        }
        Some(RsaPrivateKey { public, d, pkcs1: None })
    }

    /// The PKCS#1 `RSAPrivateKey` encoding this key was parsed from (keys built with
    /// [`RsaPrivateKey::new`] lack the CRT parameters and have none).
    pub fn pkcs1_der(&self) -> Option<&[u8]> {
        self.pkcs1.as_deref()
    }

    fn decrypt_raw(&self, em: &[u8]) -> Option<Vec<u8>> {
        let m = from_be_bytes(em, self.public.ctx.n())?;
        if !lt(&m, &self.public.ctx.m) {
            return None;
        }
        let s = self.public.ctx.pow(&m, &self.d, true);
        let sig = to_be_bytes(&s, self.public.k);
        // guard against faults: verify before releasing
        if self.public.encrypt_raw(&sig)? != to_be_bytes(&m, self.public.k) {
            return None;
        }
        Some(sig)
    }

    pub fn sign_pkcs1(&self, hash: HashAlg, digest: &[u8]) -> Option<Vec<u8>> {
        let em = pkcs1_encode(hash, digest, self.public.k)?;
        self.decrypt_raw(&em)
    }

    /// RSASSA-PSS signature with salt length = hash length (as TLS 1.3 requires).
    pub fn sign_pss(&self, hash: HashAlg, digest: &[u8]) -> Option<Vec<u8>> {
        let salt = rng::random_bytes(hash.output_len());
        self.sign_pss_with_salt(hash, digest, &salt)
    }

    pub fn sign_pss_with_salt(&self, hash: HashAlg, digest: &[u8], salt: &[u8]) -> Option<Vec<u8>> {
        let hlen = hash.output_len();
        let em_bits = self.public.mod_bits - 1;
        let em_len = em_bits.div_ceil(8);
        if digest.len() != hlen || em_len < hlen + salt.len() + 2 {
            return None;
        }
        let mut mp = vec![0u8; 8];
        mp.extend_from_slice(digest);
        mp.extend_from_slice(salt);
        let h = hash.hash(&mp);
        let db_len = em_len - hlen - 1;
        let mut db = vec![0u8; db_len - salt.len() - 1];
        db.push(1);
        db.extend_from_slice(salt);
        let mask = mgf1(hash, &h, db_len);
        let mut em: Vec<u8> = db.iter().zip(mask.iter()).map(|(a, b)| a ^ b).collect();
        let top_bits = 8 * em_len - em_bits;
        if top_bits > 0 {
            em[0] &= 0xff >> top_bits;
        }
        em.extend_from_slice(&h);
        em.push(0xbc);
        let mut full = vec![0u8; self.public.k - em_len];
        full.extend(em);
        self.decrypt_raw(&full)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::unhex;

    // Arbitrary 1024-bit odd value (structure checks only; signature vectors
    // against OpenSSL live in keys.rs tests).
    const N: &str = "c7f6a2d7ec1d1a1ac3cd29b2a6a04e5e3e4b2aca69f2c3c4e94f3dbfc40b2c7c\
2b0b0f13c69c43c4a18c0bfa6e7bc5ff10a36eb4b55cb43e3098ce3a85c3e1d1\
6f4f80bd2a9c1d13ba88da2bc4bf1ad3b6d15d5d71d6f9c6c55c0a1f33f01a1e\
ab38f19c0e7c70e9c6c2fb7e43cd7d5e2c91b88dbdc7aa2a4b9cb7df5dc33d81";

    #[test]
    fn rejects_bad_keys() {
        assert!(RsaPublicKey::new(&unhex("0f"), &[3]).is_none());
        let mut even = unhex(N);
        let last = even.len() - 1;
        even[last] &= 0xfe;
        assert!(RsaPublicKey::new(&even, &[1, 0, 1]).is_none());
    }
}

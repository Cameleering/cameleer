//! Key Vault cryptography of the emulator: RSA keys (RSA-OAEP, RSA-OAEP-256, RSA1_5
//! encryption / key wrapping, RS*/PS* signatures) and symmetric `oct` / `oct-HSM` keys
//! (A128/A192/A256GCM encryption, A128/A192/A256KW key wrapping, RFC 3394). Generated RSA keys
//! all use one fixed 2048-bit key pair (key generation is not emulated); imported keys use
//! their own material.

use camel_tls::crypto::aes::{Aes, AesGcm};
use camel_tls::crypto::bigint::{from_be_bytes, lt, to_be_bytes, MontCtx};
use camel_tls::crypto::hash::HashAlg;
use camel_tls::crypto::rng::random_bytes;

/// RSA key material (big-endian n, e, d).
#[derive(Clone)]
pub(super) struct Rsa {
    pub n: Vec<u8>,
    pub e: Vec<u8>,
    pub d: Vec<u8>,
}

const FIXED_RSA_PEM: &str = include_str!("../../testdata/rsa2048.pkcs8.pem");

fn strip(b: &[u8]) -> Vec<u8> {
    let i = b.iter().position(|x| *x != 0).unwrap_or(b.len());
    b[i..].to_vec()
}

impl Rsa {
    /// The fixed key pair used for generated RSA keys.
    pub fn fixed() -> Rsa {
        let key = camel_tls::keys::PrivateKey::from_pem(FIXED_RSA_PEM.as_bytes()).expect("fixed RSA key");
        let camel_tls::keys::PrivateKey::Rsa(r) = key else { panic!("not an RSA key") };
        let der = r.pkcs1_der().expect("PKCS#1 encoding").to_vec();
        let mut outer = camel_tls::asn1::Der::new(&der);
        let mut seq = outer.sequence().expect("RSAPrivateKey");
        let _version = seq.uint().expect("version");
        let n = strip(seq.uint().expect("n"));
        let e = strip(seq.uint().expect("e"));
        let d = strip(seq.uint().expect("d"));
        Rsa { n, e, d }
    }

    fn k(&self) -> usize {
        self.n.len()
    }

    fn modpow(&self, input: &[u8], exp: &[u8]) -> Option<Vec<u8>> {
        let limbs = self.n.len().div_ceil(8);
        let m = from_be_bytes(&self.n, limbs)?;
        let ctx = MontCtx::new(m.clone());
        let x = from_be_bytes(input, limbs)?;
        if !lt(&x, &m) {
            return None;
        }
        let e = from_be_bytes(exp, limbs)?;
        Some(to_be_bytes(&ctx.pow(&x, &e, true), self.k()))
    }

    pub fn private(&self) -> Option<camel_tls::crypto::rsa::RsaPrivateKey> {
        camel_tls::crypto::rsa::RsaPrivateKey::new(&self.n, &self.e, &self.d)
    }

    pub fn public(&self) -> Option<camel_tls::crypto::rsa::RsaPublicKey> {
        camel_tls::crypto::rsa::RsaPublicKey::new(&self.n, &self.e)
    }

    pub fn encrypt(&self, alg: &str, msg: &[u8]) -> Result<Vec<u8>, String> {
        let k = self.k();
        let em = match alg {
            "RSA1_5" => {
                if msg.len() + 11 > k {
                    return Err("message too long".into());
                }
                let mut ps = Vec::new();
                while ps.len() < k - msg.len() - 3 {
                    ps.extend(random_bytes(k).into_iter().filter(|b| *b != 0));
                }
                ps.truncate(k - msg.len() - 3);
                [vec![0, 2], ps, vec![0], msg.to_vec()].concat()
            }
            _ => {
                let h = oaep_hash(alg)?;
                let hl = h.output_len();
                if msg.len() + 2 * hl + 2 > k {
                    return Err("message too long".into());
                }
                let mut db = h.hash(b"");
                db.extend(vec![0u8; k - msg.len() - 2 * hl - 2]);
                db.push(1);
                db.extend_from_slice(msg);
                let seed = random_bytes(hl);
                let db_mask = mgf1(h, &seed, k - hl - 1);
                let masked_db: Vec<u8> = db.iter().zip(db_mask).map(|(a, b)| a ^ b).collect();
                let seed_mask = mgf1(h, &masked_db, hl);
                let masked_seed: Vec<u8> = seed.iter().zip(seed_mask).map(|(a, b)| a ^ b).collect();
                [vec![0], masked_seed, masked_db].concat()
            }
        };
        self.modpow(&em, &self.e).ok_or_else(|| "encryption failed".into())
    }

    pub fn decrypt(&self, alg: &str, c: &[u8]) -> Result<Vec<u8>, String> {
        let k = self.k();
        if c.len() != k {
            return Err("ciphertext length does not match the key size".into());
        }
        let em = self.modpow(c, &self.d).ok_or("decryption failed")?;
        match alg {
            "RSA1_5" => {
                if em[0] != 0 || em[1] != 2 {
                    return Err("decryption failed".into());
                }
                let sep = em[2..].iter().position(|b| *b == 0).ok_or("decryption failed")? + 2;
                Ok(em[sep + 1..].to_vec())
            }
            _ => {
                let h = oaep_hash(alg)?;
                let hl = h.output_len();
                let (masked_seed, masked_db) = (&em[1..1 + hl], &em[1 + hl..]);
                let seed: Vec<u8> = masked_seed.iter().zip(mgf1(h, masked_db, hl)).map(|(a, b)| a ^ b).collect();
                let db: Vec<u8> = masked_db.iter().zip(mgf1(h, &seed, k - hl - 1)).map(|(a, b)| a ^ b).collect();
                if em[0] != 0 || db[..hl] != h.hash(b"")[..] {
                    return Err("decryption failed".into());
                }
                let one = db[hl..].iter().position(|b| *b != 0).ok_or("decryption failed")? + hl;
                if db[one] != 1 {
                    return Err("decryption failed".into());
                }
                Ok(db[one + 1..].to_vec())
            }
        }
    }
}

fn oaep_hash(alg: &str) -> Result<HashAlg, String> {
    match alg {
        "RSA-OAEP" => Ok(HashAlg::Sha1),
        "RSA-OAEP-256" => Ok(HashAlg::Sha256),
        other => Err(format!("Algorithm {} is not valid for an RSA key", other)),
    }
}

fn mgf1(hash: HashAlg, seed: &[u8], len: usize) -> Vec<u8> {
    let mut out = Vec::new();
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

/// Key size in bytes required by a symmetric algorithm.
pub(super) fn oct_key_len(alg: &str) -> Option<usize> {
    match alg {
        "A128GCM" | "A128KW" => Some(16),
        "A192GCM" | "A192KW" => Some(24),
        "A256GCM" | "A256KW" => Some(32),
        _ => None,
    }
}

/// AES-GCM: (ciphertext, iv, tag).
pub(super) fn gcm_encrypt(key: &[u8], aad: &[u8], plain: &[u8]) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let iv: [u8; 12] = random_bytes(12).try_into().expect("12 bytes");
    let mut out = AesGcm::new(key).seal(&iv, aad, plain);
    let tag = out.split_off(out.len() - 16);
    (out, iv.to_vec(), tag)
}

pub(super) fn gcm_decrypt(key: &[u8], iv: &[u8], aad: &[u8], cipher: &[u8], tag: &[u8]) -> Option<Vec<u8>> {
    let iv: [u8; 12] = iv.try_into().ok()?;
    AesGcm::new(key).open(&iv, aad, &[cipher, tag].concat())
}

const KW_IV: [u8; 8] = [0xA6; 8];

/// AES key wrap (RFC 3394).
pub(super) fn aes_kw_wrap(kek: &[u8], key: &[u8]) -> Option<Vec<u8>> {
    if key.len() < 16 || key.len() % 8 != 0 {
        return None;
    }
    let aes = Aes::new(kek);
    let n = key.len() / 8;
    let mut a = KW_IV;
    let mut r: Vec<[u8; 8]> = key.chunks(8).map(|c| c.try_into().unwrap()).collect();
    for j in 0..6 {
        for i in 0..n {
            let mut b = [0u8; 16];
            b[..8].copy_from_slice(&a);
            b[8..].copy_from_slice(&r[i]);
            aes.encrypt_block(&mut b);
            let t = ((n * j) + i + 1) as u64;
            a.copy_from_slice(&b[..8]);
            for (x, y) in a.iter_mut().zip(t.to_be_bytes()) {
                *x ^= y;
            }
            r[i].copy_from_slice(&b[8..]);
        }
    }
    Some([a.to_vec(), r.concat()].concat())
}

/// AES key unwrap (RFC 3394); None when the integrity check fails.
pub(super) fn aes_kw_unwrap(kek: &[u8], wrapped: &[u8]) -> Option<Vec<u8>> {
    if wrapped.len() < 24 || wrapped.len() % 8 != 0 {
        return None;
    }
    let aes = Aes::new(kek);
    let n = wrapped.len() / 8 - 1;
    let mut a: [u8; 8] = wrapped[..8].try_into().ok()?;
    let mut r: Vec<[u8; 8]> = wrapped[8..].chunks(8).map(|c| c.try_into().unwrap()).collect();
    for j in (0..6).rev() {
        for i in (0..n).rev() {
            let t = ((n * j) + i + 1) as u64;
            for (x, y) in a.iter_mut().zip(t.to_be_bytes()) {
                *x ^= y;
            }
            let mut b = [0u8; 16];
            b[..8].copy_from_slice(&a);
            b[8..].copy_from_slice(&r[i]);
            aes.decrypt_block(&mut b);
            a.copy_from_slice(&b[..8]);
            r[i].copy_from_slice(&b[8..]);
        }
    }
    (a == KW_IV).then(|| r.concat())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3394_vector() {
        let kek: Vec<u8> = (0u8..16).collect();
        let key: Vec<u8> = vec![0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
        let w = aes_kw_wrap(&kek, &key).unwrap();
        assert_eq!(crate::util::hex(&w), "1fa68b0a8112b447aef34bd8fb5a7b829d3e862371d2cfe5");
        assert_eq!(aes_kw_unwrap(&kek, &w).unwrap(), key);
    }

    #[test]
    fn rsa_round_trips() {
        let k = Rsa::fixed();
        for alg in ["RSA-OAEP", "RSA-OAEP-256", "RSA1_5"] {
            let c = k.encrypt(alg, b"secret").unwrap();
            assert_eq!(k.decrypt(alg, &c).unwrap(), b"secret");
        }
    }
}

//! Password-based encryption used by key stores: PBES2 (PBKDF2 + AES-CBC or
//! 3DES-CBC, RFC 8018) and the PKCS#12 PBE schemes (RFC 7292 appendix C:
//! `pbeWithSHAAnd3-KeyTripleDES-CBC`, `pbeWithSHAAnd2-KeyTripleDES-CBC`,
//! `pbeWithSHAAnd128BitRC2-CBC`, `pbeWithSHAAnd40BitRC2-CBC`), plus PKCS#8
//! `EncryptedPrivateKeyInfo`.

use crate::asn1::{self, Der, TAG_INTEGER, TAG_OCTET_STRING, TAG_SEQUENCE};
use crate::crypto::aes::{cbc_decrypt, cbc_encrypt, Aes};
use crate::crypto::des::TripleDes;
use crate::crypto::hash::HashAlg;
use crate::crypto::kdf::{bmp_password, pbkdf2, pkcs12_kdf, PKCS12_IV, PKCS12_KEY};
use crate::crypto::rc2::Rc2;
use crate::crypto::rng;

pub const OID_PBES2: &str = "1.2.840.113549.1.5.13";
pub const OID_PBKDF2: &str = "1.2.840.113549.1.5.12";
pub const OID_PBE_SHA_3DES: &str = "1.2.840.113549.1.12.1.3";
pub const OID_PBE_SHA_2DES: &str = "1.2.840.113549.1.12.1.4";
pub const OID_PBE_SHA_RC2_128: &str = "1.2.840.113549.1.12.1.5";
pub const OID_PBE_SHA_RC2_40: &str = "1.2.840.113549.1.12.1.6";
pub const OID_HMAC_SHA1: &str = "1.2.840.113549.2.7";
pub const OID_HMAC_SHA256: &str = "1.2.840.113549.2.9";
pub const OID_HMAC_SHA384: &str = "1.2.840.113549.2.10";
pub const OID_HMAC_SHA512: &str = "1.2.840.113549.2.11";
pub const OID_AES128_CBC: &str = "2.16.840.1.101.3.4.1.2";
pub const OID_AES192_CBC: &str = "2.16.840.1.101.3.4.1.22";
pub const OID_AES256_CBC: &str = "2.16.840.1.101.3.4.1.42";
pub const OID_DES_EDE3_CBC: &str = "1.2.840.113549.3.7";

/// Upper bound on KDF iterations accepted from a file (denial-of-service guard).
const MAX_ITERATIONS: u64 = 10_000_000;

fn e<T: std::fmt::Display>(x: T) -> String {
    x.to_string()
}

pub fn prf_hash(oid: &str) -> Option<HashAlg> {
    match oid {
        OID_HMAC_SHA1 => Some(HashAlg::Sha1),
        OID_HMAC_SHA256 => Some(HashAlg::Sha256),
        OID_HMAC_SHA384 => Some(HashAlg::Sha384),
        OID_HMAC_SHA512 => Some(HashAlg::Sha512),
        _ => None,
    }
}

fn iterations(d: &mut Der) -> Result<u32, String> {
    let n = d.small_uint().map_err(e)?;
    if n == 0 || n > MAX_ITERATIONS {
        return Err(format!("unsupported iteration count {n}"));
    }
    Ok(n as u32)
}

/// Decrypts `data` encrypted with the algorithm described by the DER
/// AlgorithmIdentifier `alg` (complete SEQUENCE encoding).
pub fn decrypt(alg: &[u8], data: &[u8], password: &str) -> Result<Vec<u8>, String> {
    let mut outer = Der::new(alg);
    let mut a = outer.sequence().map_err(e)?;
    let oid = a.oid().map_err(e)?;
    let wrong = || "decryption failed (wrong password?)".to_string();
    match oid.as_str() {
        OID_PBES2 => {
            let mut p = a.sequence().map_err(e)?;
            let mut kdf = p.sequence().map_err(e)?;
            let kdf_oid = kdf.oid().map_err(e)?;
            if kdf_oid != OID_PBKDF2 {
                return Err(format!("unsupported PBES2 key derivation function {kdf_oid}"));
            }
            let mut kp = kdf.sequence().map_err(e)?;
            let salt = kp.octet_string().map_err(e)?.to_vec();
            let iter = iterations(&mut kp)?;
            let mut key_len = None;
            if kp.peek_tag() == Some(TAG_INTEGER) {
                key_len = Some(kp.small_uint().map_err(e)? as usize);
            }
            let mut prf = HashAlg::Sha1;
            if kp.peek_tag() == Some(TAG_SEQUENCE) {
                let mut pa = kp.sequence().map_err(e)?;
                let po = pa.oid().map_err(e)?;
                prf = prf_hash(&po).ok_or_else(|| format!("unsupported PBKDF2 PRF {po}"))?;
            }
            let mut enc = p.sequence().map_err(e)?;
            let enc_oid = enc.oid().map_err(e)?;
            let iv = enc.octet_string().map_err(e)?.to_vec();
            let klen = match enc_oid.as_str() {
                OID_AES128_CBC => 16,
                OID_AES192_CBC => 24,
                OID_AES256_CBC => 32,
                OID_DES_EDE3_CBC => 24,
                other => return Err(format!("unsupported PBES2 encryption scheme {other}")),
            };
            if key_len.is_some_and(|k| k != klen) {
                return Err("PBES2 key length mismatch".into());
            }
            let key = pbkdf2(prf, password.as_bytes(), &salt, iter, klen);
            if enc_oid == OID_DES_EDE3_CBC {
                cbc_decrypt(&TripleDes::new(&key), &iv, data).ok_or_else(wrong)
            } else {
                cbc_decrypt(&Aes::new(&key), &iv, data).ok_or_else(wrong)
            }
        }
        OID_PBE_SHA_3DES | OID_PBE_SHA_2DES | OID_PBE_SHA_RC2_128 | OID_PBE_SHA_RC2_40 => {
            let mut p = a.sequence().map_err(e)?;
            let salt = p.octet_string().map_err(e)?.to_vec();
            let iter = iterations(&mut p)?;
            let pw = bmp_password(password);
            let h = HashAlg::Sha1;
            let klen = match oid.as_str() {
                OID_PBE_SHA_3DES => 24,
                OID_PBE_SHA_2DES | OID_PBE_SHA_RC2_128 => 16,
                _ => 5,
            };
            let key = pkcs12_kdf(h, &pw, &salt, PKCS12_KEY, iter, klen);
            let iv = pkcs12_kdf(h, &pw, &salt, PKCS12_IV, iter, 8);
            match oid.as_str() {
                OID_PBE_SHA_3DES | OID_PBE_SHA_2DES => cbc_decrypt(&TripleDes::new(&key), &iv, data).ok_or_else(wrong),
                _ => cbc_decrypt(&Rc2::new(&key, klen * 8), &iv, data).ok_or_else(wrong),
            }
        }
        other => Err(format!("unsupported encryption algorithm {other}")),
    }
}

/// Encrypts with PBES2 (PBKDF2-HMAC-SHA256, AES-256-CBC); returns
/// (AlgorithmIdentifier DER, ciphertext).
pub fn encrypt_pbes2(data: &[u8], password: &str, iterations: u32) -> (Vec<u8>, Vec<u8>) {
    let salt = rng::random_bytes(16);
    let iv = rng::random_bytes(16);
    let key = pbkdf2(HashAlg::Sha256, password.as_bytes(), &salt, iterations, 32);
    let ct = cbc_encrypt(&Aes::new(&key), &iv, data);
    let alg = asn1::seq(&[
        asn1::oid(OID_PBES2),
        asn1::seq(&[
            asn1::seq(&[
                asn1::oid(OID_PBKDF2),
                asn1::seq(&[
                    asn1::octet_string(&salt),
                    asn1::uint(&iterations.to_be_bytes()),
                    asn1::seq(&[asn1::oid(OID_HMAC_SHA256), asn1::tlv(asn1::TAG_NULL, &[])]),
                ]),
            ]),
            asn1::seq(&[asn1::oid(OID_AES256_CBC), asn1::octet_string(&iv)]),
        ]),
    ]);
    (alg, ct)
}

/// Encrypts with a PKCS#12 PBE scheme (`OID_PBE_SHA_3DES` or `OID_PBE_SHA_RC2_40`), for
/// building legacy-format test files.
pub fn encrypt_pkcs12_legacy(oid: &str, data: &[u8], password: &str, iterations: u32) -> (Vec<u8>, Vec<u8>) {
    let salt = rng::random_bytes(8);
    let pw = bmp_password(password);
    let klen = if oid == OID_PBE_SHA_3DES { 24 } else { 5 };
    let key = pkcs12_kdf(HashAlg::Sha1, &pw, &salt, PKCS12_KEY, iterations, klen);
    let iv = pkcs12_kdf(HashAlg::Sha1, &pw, &salt, PKCS12_IV, iterations, 8);
    let ct = if oid == OID_PBE_SHA_3DES {
        cbc_encrypt(&TripleDes::new(&key), &iv, data)
    } else {
        cbc_encrypt(&Rc2::new(&key, 40), &iv, data)
    };
    let alg = asn1::seq(&[asn1::oid(oid), asn1::seq(&[asn1::octet_string(&salt), asn1::uint(&iterations.to_be_bytes())])]);
    (alg, ct)
}

/// Decrypts a PKCS#8 `EncryptedPrivateKeyInfo`, returning the inner PrivateKeyInfo DER.
pub fn decrypt_encrypted_private_key_info(der: &[u8], password: &str) -> Result<Vec<u8>, String> {
    let mut outer = Der::new(der);
    let mut s = outer.sequence().map_err(e)?;
    let alg = s.expect(TAG_SEQUENCE).map_err(e)?;
    let data = s.expect(TAG_OCTET_STRING).map_err(e)?;
    decrypt(alg.raw, data.value, password)
}

/// Builds a PKCS#8 `EncryptedPrivateKeyInfo` (PBES2, AES-256-CBC).
pub fn encrypt_private_key_info(pkcs8: &[u8], password: &str) -> Vec<u8> {
    let (alg, ct) = encrypt_pbes2(pkcs8, password, 10_000);
    asn1::seq(&[alg, asn1::octet_string(&ct)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips() {
        let data = b"some private key bytes, longer than one block".to_vec();
        let (alg, ct) = encrypt_pbes2(&data, "secret", 100);
        assert_eq!(decrypt(&alg, &ct, "secret").unwrap(), data);
        assert!(decrypt(&alg, &ct, "wrong").is_err() || decrypt(&alg, &ct, "wrong").unwrap() != data);
        for oid in [OID_PBE_SHA_3DES, OID_PBE_SHA_RC2_40] {
            let (alg, ct) = encrypt_pkcs12_legacy(oid, &data, "pässword", 7);
            assert_eq!(decrypt(&alg, &ct, "pässword").unwrap(), data);
        }
    }
}

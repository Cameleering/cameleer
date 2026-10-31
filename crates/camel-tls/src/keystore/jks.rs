//! Java key stores: Sun JKS (`FEEDFEED`) and JCEKS (`CECECECE`).
//!
//! Integrity: SHA-1 over the password (UTF-16BE), the string "Mighty Aphrodite" and
//! the store content. Private keys are protected with the JKS key protector
//! (OID 1.3.6.1.4.1.42.2.17.1.1: SHA-1 keystream XOR with a SHA-1 check value) or,
//! in JCEKS, `PBEWithMD5AndTripleDES` (OID 1.3.6.1.4.1.42.2.19.1). JCEKS secret-key
//! entries (serialized Java objects) are not supported.

use super::{KeyStore, KeyStoreEntry};
use crate::asn1::{self, Der, TAG_OCTET_STRING, TAG_SEQUENCE};
use crate::crypto::aes::cbc_decrypt;
use crate::crypto::des::TripleDes;
use crate::crypto::hash::{Digest, Sha1};
use crate::crypto::md5::Md5;
use crate::crypto::rng;
use crate::keys::PrivateKey;
use crate::util::ct_eq;

pub const JKS_MAGIC: u32 = 0xFEED_FEED;
pub const JCEKS_MAGIC: u32 = 0xCECE_CECE;
const OID_JKS_KEY_PROTECTOR: &str = "1.3.6.1.4.1.42.2.17.1.1";
const OID_PBE_MD5_3DES: &str = "1.3.6.1.4.1.42.2.19.1";

struct Reader<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.p + n > self.d.len() {
            return Err("JKS: truncated key store".into());
        }
        let s = &self.d[self.p..self.p + n];
        self.p += n;
        Ok(s)
    }
    fn u16(&mut self) -> Result<u16, String> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn utf(&mut self) -> Result<String, String> {
        let n = self.u16()? as usize;
        let b = self.take(n)?;
        Ok(decode_modified_utf8(b))
    }
    fn bytes32(&mut self) -> Result<&'a [u8], String> {
        let n = self.u32()? as usize;
        self.take(n)
    }
}

/// Java "modified UTF-8" (NUL as C0 80, supplementary characters as surrogate pairs).
fn decode_modified_utf8(b: &[u8]) -> String {
    let mut units: Vec<u16> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as u32;
        if c < 0x80 {
            units.push(c as u16);
            i += 1;
        } else if c & 0xe0 == 0xc0 && i + 1 < b.len() {
            units.push((((c & 0x1f) << 6) | (b[i + 1] as u32 & 0x3f)) as u16);
            i += 2;
        } else if i + 2 < b.len() {
            units.push((((c & 0x0f) << 12) | ((b[i + 1] as u32 & 0x3f) << 6) | (b[i + 2] as u32 & 0x3f)) as u16);
            i += 3;
        } else {
            break;
        }
    }
    String::from_utf16_lossy(&units)
}

fn encode_modified_utf8(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for u in s.encode_utf16() {
        match u {
            1..=0x7f => out.push(u as u8),
            0 | 0x80..=0x7ff => {
                out.push(0xc0 | (u >> 6) as u8);
                out.push(0x80 | (u & 0x3f) as u8);
            }
            _ => {
                out.push(0xe0 | (u >> 12) as u8);
                out.push(0x80 | ((u >> 6) & 0x3f) as u8);
                out.push(0x80 | (u & 0x3f) as u8);
            }
        }
    }
    out
}

fn utf16be(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(|u| u.to_be_bytes()).collect()
}

fn integrity_digest(password: &str, content: &[u8]) -> Vec<u8> {
    let mut h = Sha1::new();
    h.update(&utf16be(password));
    h.update(b"Mighty Aphrodite");
    h.update(content);
    h.finish()
}

/// Sun's proprietary JKS key protector.
fn jks_unprotect(enc: &[u8], password: &str) -> Result<Vec<u8>, String> {
    if enc.len() < 40 {
        return Err("JKS: protected key too short".into());
    }
    let pw = utf16be(password);
    let (salt, rest) = enc.split_at(20);
    let (data, check) = rest.split_at(rest.len() - 20);
    let plain = jks_keystream_xor(&pw, salt, data);
    let mut h = Sha1::new();
    h.update(&pw);
    h.update(&plain);
    if !ct_eq(&h.finish(), check) {
        return Err("JKS: cannot recover key (wrong key password)".into());
    }
    Ok(plain)
}

fn jks_keystream_xor(pw: &[u8], salt: &[u8], data: &[u8]) -> Vec<u8> {
    let mut digest = salt.to_vec();
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.chunks(20) {
        let mut h = Sha1::new();
        h.update(pw);
        h.update(&digest);
        digest = h.finish();
        out.extend(chunk.iter().zip(&digest).map(|(a, b)| a ^ b));
    }
    out
}

fn jks_protect(plain: &[u8], password: &str) -> Vec<u8> {
    let pw = utf16be(password);
    let salt = rng::random_bytes(20);
    let mut out = salt.clone();
    out.extend(jks_keystream_xor(&pw, &salt, plain));
    let mut h = Sha1::new();
    h.update(&pw);
    h.update(plain);
    out.extend(h.finish());
    out
}

/// JCEKS `PBEWithMD5AndTripleDES` (com.sun.crypto.provider.PBES1Core).
fn jceks_unprotect(params: &[u8], enc: &[u8], password: &str) -> Result<Vec<u8>, String> {
    let mut p = Der::new(params).sequence().map_err(|e| e.to_string())?;
    let salt = p.octet_string().map_err(|e| e.to_string())?.to_vec();
    let iter = p.small_uint().map_err(|e| e.to_string())?;
    if salt.len() != 8 || iter == 0 || iter > 10_000_000 {
        return Err("JCEKS: bad PBE parameters".into());
    }
    let pw: Vec<u8> = password.chars().map(|c| (c as u32 & 0x7f) as u8).collect();
    let mut first = salt[..4].to_vec();
    let second = salt[4..].to_vec();
    if first == second {
        first.reverse();
    }
    let mut derived = Vec::new();
    for half in [first, second] {
        let mut to_hash = half;
        for _ in 0..iter {
            let mut h = Md5::new();
            h.update(&to_hash);
            h.update(&pw);
            to_hash = h.finish();
        }
        derived.extend(to_hash);
    }
    let cipher = TripleDes::new(&derived[..24]);
    cbc_decrypt(&cipher, &derived[24..32], enc).ok_or_else(|| "JCEKS: cannot recover key (wrong key password)".to_string())
}

fn unprotect_key(der: &[u8], password: &str) -> Result<PrivateKey, String> {
    let mut s = Der::new(der).sequence().map_err(|e| e.to_string())?;
    let mut alg = s.sequence().map_err(|e| e.to_string())?;
    let oid = alg.oid().map_err(|e| e.to_string())?;
    let enc = s.expect(TAG_OCTET_STRING).map_err(|e| e.to_string())?.value;
    let plain = match oid.as_str() {
        OID_JKS_KEY_PROTECTOR => jks_unprotect(enc, password)?,
        OID_PBE_MD5_3DES => {
            let params = alg.expect(TAG_SEQUENCE).map_err(|e| e.to_string())?;
            jceks_unprotect(params.raw, enc, password)?
        }
        other => crate::pbe::decrypt(&asn1::seq(&[asn1::oid(other)]), enc, password)
            .map_err(|_| format!("JKS: unsupported key protection algorithm {other}"))?,
    };
    PrivateKey::from_pkcs8(&plain).map_err(|e| format!("JKS: private key: {e}"))
}

/// Parses a JKS or JCEKS store. Without `store_password` the integrity check is skipped
/// (as Java does for trust stores loaded without a password).
pub fn parse(data: &[u8], store_password: Option<&str>, key_password: Option<&str>) -> Result<KeyStore, String> {
    if data.len() < 32 {
        return Err("JKS: file too short".into());
    }
    let (content, digest) = data.split_at(data.len() - 20);
    if let Some(pw) = store_password {
        if !ct_eq(&integrity_digest(pw, content), digest) {
            return Err("Keystore was tampered with, or password was incorrect".into());
        }
    }
    let mut r = Reader { d: content, p: 0 };
    let magic = r.u32()?;
    let jceks = magic == JCEKS_MAGIC;
    if magic != JKS_MAGIC && !jceks {
        return Err("JKS: bad magic".into());
    }
    let version = r.u32()?;
    if version != 1 && version != 2 {
        return Err(format!("JKS: unsupported version {version}"));
    }
    let count = r.u32()?;
    let mut ks = KeyStore::new();
    let read_cert = |r: &mut Reader| -> Result<Vec<u8>, String> {
        if version == 2 {
            let typ = r.utf()?;
            if typ != "X.509" {
                return Err(format!("JKS: unsupported certificate type {typ}"));
            }
        }
        Ok(r.bytes32()?.to_vec())
    };
    for _ in 0..count {
        let tag = r.u32()?;
        let alias = r.utf()?;
        let _timestamp = r.take(8)?;
        match tag {
            1 => {
                let protected = r.bytes32()?.to_vec();
                let n = r.u32()? as usize;
                if n > 64 {
                    return Err("JKS: certificate chain too long".into());
                }
                let mut chain = Vec::with_capacity(n);
                for _ in 0..n {
                    chain.push(read_cert(&mut r)?);
                }
                let kp = key_password.or(store_password).ok_or_else(|| format!("JKS: a key password is needed for entry '{alias}'"))?;
                let key = unprotect_key(&protected, kp)?;
                ks.entries.push(KeyStoreEntry::Key { alias, key, chain });
            }
            2 => {
                let der = read_cert(&mut r)?;
                ks.entries.push(KeyStoreEntry::Cert { alias, der });
            }
            3 => return Err(format!("JCEKS: secret key entry '{alias}' is not supported")),
            other => return Err(format!("JKS: unknown entry tag {other}")),
        }
    }
    Ok(ks)
}

/// Writes a JKS store (keys protected with the JKS key protector).
pub fn write(ks: &KeyStore, store_password: &str, key_password: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&JKS_MAGIC.to_be_bytes());
    out.extend_from_slice(&2u32.to_be_bytes());
    out.extend_from_slice(&(ks.entries.len() as u32).to_be_bytes());
    let now_ms = (crate::x509::now_unix() as u64) * 1000;
    let put_utf = |out: &mut Vec<u8>, s: &str| {
        let b = encode_modified_utf8(s);
        out.extend_from_slice(&(b.len() as u16).to_be_bytes());
        out.extend_from_slice(&b);
    };
    let put_cert = |out: &mut Vec<u8>, der: &[u8]| {
        let b = encode_modified_utf8("X.509");
        out.extend_from_slice(&(b.len() as u16).to_be_bytes());
        out.extend_from_slice(&b);
        out.extend_from_slice(&(der.len() as u32).to_be_bytes());
        out.extend_from_slice(der);
    };
    for e in &ks.entries {
        match e {
            KeyStoreEntry::Key { alias, key, chain } => {
                out.extend_from_slice(&1u32.to_be_bytes());
                put_utf(&mut out, alias);
                out.extend_from_slice(&now_ms.to_be_bytes());
                let protected = jks_protect(&key.to_pkcs8(), key_password);
                let epki = asn1::seq(&[asn1::seq(&[asn1::oid(OID_JKS_KEY_PROTECTOR), asn1::tlv(asn1::TAG_NULL, &[])]), asn1::octet_string(&protected)]);
                out.extend_from_slice(&(epki.len() as u32).to_be_bytes());
                out.extend_from_slice(&epki);
                out.extend_from_slice(&(chain.len() as u32).to_be_bytes());
                for c in chain {
                    put_cert(&mut out, c);
                }
            }
            KeyStoreEntry::Cert { alias, der } => {
                out.extend_from_slice(&2u32.to_be_bytes());
                put_utf(&mut out, alias);
                out.extend_from_slice(&now_ms.to_be_bytes());
                put_cert(&mut out, der);
            }
        }
    }
    let d = integrity_digest(store_password, &out);
    out.extend(d);
    out
}

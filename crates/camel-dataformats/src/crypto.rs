//! `crypto` data format (Camel's JCE based `CryptoDataFormat`): symmetric encryption with an
//! optional appended HMAC.
//!
//! Options:
//! * `algorithm` (default `DES/CBC/PKCS5Padding`): `AES`, `DES`, `DESede` (`TripleDES`) with
//!   modes `ECB`, `CBC`, `CTR`, `CFB`, `OFB`, `GCM` (AES, 12 byte IV, 16 byte tag appended)
//!   and paddings `PKCS5Padding`/`PKCS7Padding`/`NoPadding`. A bare `AES` means
//!   `AES/ECB/PKCS5Padding` like the JCE.
//! * `keyRef`: registry bean with the key (`Vec<u8>`, `Value::Bytes`, or a `String` that
//!   is `hex:...`, `base64:...` or raw text); `key` gives it inline in the same syntax.
//!   The `CamelCryptoKey` header overrides it per message.
//! * `initVectorRef` / `initVector`: the IV (same syntax); the `CamelCryptoInitVector`
//!   header overrides it. Without one a random IV is generated (decrypting then needs
//!   `inline=true`).
//! * `inline` (default false): write the IV in front of the output as Camel does
//!   (4-byte big-endian length followed by the IV) and read it from there when decrypting.
//! * `shouldAppendHMAC` (default true): append `HMAC(key, ciphertext)` and verify it on
//!   unmarshal. `macAlgorithm` `HmacSHA1` (default), `HmacSHA256`, `HmacSHA384`, `HmacSHA512`.
//! * `buffersize`, `cryptoProvider`, `algorithmParameterRef` are accepted and ignored.

use std::sync::Arc;

use camel_core::dataformat::DataFormat;
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::Value;
use camel_tls::crypto::aes::{Aes, AesGcm, BlockCipher};
use camel_tls::crypto::des::{Des, TripleDes};
use camel_tls::crypto::hash::HashAlg;
use camel_tls::crypto::rng;
use camel_tls::util::ct_eq;

use crate::{body_bytes, Opts};

/// Header with a per-message key.
pub const KEY: &str = "CamelCryptoKey";
/// Header with a per-message initialization vector.
pub const INIT_VECTOR: &str = "CamelCryptoInitVector";

fn crypto_err(msg: impl Into<String>) -> CamelError {
    CamelError::new("GeneralSecurityException", msg)
}

/// Parse key material: `hex:..`, `base64:..`, otherwise raw UTF-8 bytes.
pub fn parse_key_material(s: &str) -> Result<Vec<u8>> {
    if let Some(h) = s.strip_prefix("hex:") {
        return camel_tls::util::try_unhex(h.trim()).ok_or_else(|| CamelError::illegal_argument("Invalid hex key material"));
    }
    if let Some(b) = s.strip_prefix("base64:") {
        return camel_core::dataformat::base64::decode(b.trim());
    }
    Ok(s.as_bytes().to_vec())
}

fn value_bytes(v: &Value) -> Result<Vec<u8>> {
    match v {
        Value::Bytes(b) => Ok(b.clone()),
        Value::String(s) => parse_key_material(s),
        other => Ok(other.to_bytes()),
    }
}

/// Look up key material in the exchange's registry (`Vec<u8>`, `Value`, `String`).
pub(crate) fn registry_bytes(ex: &Exchange, name: &str) -> Result<Option<Vec<u8>>> {
    let name = name.trim_start_matches('#').trim_start_matches("bean:");
    let ctx = match ex.context() {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };
    let r = ctx.registry();
    if let Some(b) = r.lookup::<Vec<u8>>(name) {
        return Ok(Some((*b).clone()));
    }
    if let Some(v) = r.lookup::<Value>(name) {
        return value_bytes(&v).map(Some);
    }
    if let Some(s) = r.lookup::<String>(name) {
        return parse_key_material(&s).map(Some);
    }
    if let Some(s) = r.lookup::<&'static str>(name) {
        return parse_key_material(&s).map(Some);
    }
    Ok(None)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CipherAlg {
    Aes,
    Des,
    DesEde,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Ecb,
    Cbc,
    Ctr,
    Cfb,
    Ofb,
    Gcm,
}

/// A parsed JCE transformation (`AES/CBC/PKCS5Padding`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transformation {
    pub alg: CipherAlg,
    pub mode: Mode,
    pub padding: bool,
}

impl Transformation {
    pub fn parse(s: &str) -> Result<Transformation> {
        let parts: Vec<&str> = s.split('/').map(|p| p.trim()).collect();
        let alg = match parts[0].to_ascii_uppercase().as_str() {
            "AES" | "AES128" | "AES192" | "AES256" | "AES_128" | "AES_256" => CipherAlg::Aes,
            "DES" => CipherAlg::Des,
            "DESEDE" | "TRIPLEDES" | "3DES" => CipherAlg::DesEde,
            other => return Err(crypto_err(format!("Unsupported cipher algorithm: {}", other))),
        };
        let mode = match parts.get(1).map(|m| m.to_ascii_uppercase()) {
            None => Mode::Ecb,
            Some(m) => match m.as_str() {
                "ECB" => Mode::Ecb,
                "CBC" => Mode::Cbc,
                "CTR" => Mode::Ctr,
                "CFB" | "CFB128" | "CFB64" => Mode::Cfb,
                "OFB" | "OFB128" | "OFB64" => Mode::Ofb,
                "GCM" => Mode::Gcm,
                other => return Err(crypto_err(format!("Unsupported cipher mode: {}", other))),
            },
        };
        let padding = match parts.get(2).map(|m| m.to_ascii_uppercase()) {
            None => matches!(mode, Mode::Ecb | Mode::Cbc),
            Some(p) => match p.as_str() {
                "PKCS5PADDING" | "PKCS7PADDING" => true,
                "NOPADDING" => false,
                other => return Err(crypto_err(format!("Unsupported padding: {}", other))),
            },
        };
        if mode == Mode::Gcm && alg != CipherAlg::Aes {
            return Err(crypto_err("GCM requires AES"));
        }
        Ok(Transformation { alg, mode, padding })
    }

    pub fn block_size(&self) -> usize {
        if self.alg == CipherAlg::Aes {
            16
        } else {
            8
        }
    }

    /// IV length for this mode (0 for ECB).
    pub fn iv_len(&self) -> usize {
        match self.mode {
            Mode::Ecb => 0,
            Mode::Gcm => 12,
            _ => self.block_size(),
        }
    }
}

enum Block {
    Aes(Aes),
    Des(Des),
    Tdes(TripleDes),
}

impl Block {
    fn new(alg: CipherAlg, key: &[u8]) -> Result<Block> {
        Ok(match alg {
            CipherAlg::Aes => {
                if ![16, 24, 32].contains(&key.len()) {
                    return Err(crypto_err(format!("Invalid AES key length: {} bytes", key.len())));
                }
                Block::Aes(Aes::new(key))
            }
            CipherAlg::Des => {
                if key.len() < 8 {
                    return Err(crypto_err("Invalid DES key length"));
                }
                Block::Des(Des::new(&key[..8]))
            }
            CipherAlg::DesEde => {
                let k = match key.len() {
                    24 => key.to_vec(),
                    16 => [key, &key[..8]].concat(),
                    _ => return Err(crypto_err("Invalid DESede key length")),
                };
                Block::Tdes(TripleDes::new(&k))
            }
        })
    }
    fn enc(&self, b: &mut [u8]) {
        match self {
            Block::Aes(c) => c.encrypt(b),
            Block::Des(c) => c.encrypt(b),
            Block::Tdes(c) => c.encrypt(b),
        }
    }
    fn dec(&self, b: &mut [u8]) {
        match self {
            Block::Aes(c) => c.decrypt(b),
            Block::Des(c) => c.decrypt(b),
            Block::Tdes(c) => c.decrypt(b),
        }
    }
}

/// Encrypt with a transformation (no HMAC, no inline IV).
pub fn encrypt(t: &Transformation, key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    if t.mode == Mode::Gcm {
        let n: [u8; 12] = iv.try_into().map_err(|_| crypto_err("GCM requires a 12 byte IV"))?;
        if ![16, 32].contains(&key.len()) {
            return Err(crypto_err("Invalid AES-GCM key length"));
        }
        return Ok(AesGcm::new(key).seal(&n, &[], data));
    }
    let c = Block::new(t.alg, key)?;
    let bs = t.block_size();
    if t.mode != Mode::Ecb && iv.len() != bs {
        return Err(crypto_err(format!("Wrong IV length: must be {} bytes long", bs)));
    }
    let mut buf = data.to_vec();
    match t.mode {
        Mode::Ecb | Mode::Cbc => {
            if t.padding {
                let p = bs - buf.len() % bs;
                buf.extend(std::iter::repeat_n(p as u8, p));
            } else if buf.len() % bs != 0 {
                return Err(crypto_err(format!("Input length not multiple of {} bytes", bs)));
            }
            let mut prev = iv.to_vec();
            for chunk in buf.chunks_mut(bs) {
                if t.mode == Mode::Cbc {
                    chunk.iter_mut().zip(&prev).for_each(|(b, p)| *b ^= p);
                }
                c.enc(chunk);
                if t.mode == Mode::Cbc {
                    prev.copy_from_slice(chunk);
                }
            }
        }
        _ => stream(&c, t.mode, iv, &mut buf, true),
    }
    Ok(buf)
}

fn stream(c: &Block, mode: Mode, iv: &[u8], buf: &mut [u8], encrypting: bool) {
    let bs = iv.len();
    let mut reg = iv.to_vec();
    for chunk in buf.chunks_mut(bs) {
        let mut ks = reg.clone();
        c.enc(&mut ks);
        match mode {
            Mode::Ctr => {
                chunk.iter_mut().zip(&ks).for_each(|(b, k)| *b ^= k);
                for i in (0..bs).rev() {
                    reg[i] = reg[i].wrapping_add(1);
                    if reg[i] != 0 {
                        break;
                    }
                }
            }
            Mode::Ofb => {
                chunk.iter_mut().zip(&ks).for_each(|(b, k)| *b ^= k);
                reg = ks;
            }
            _ => {
                // CFB (full block)
                let ct: Vec<u8> = if encrypting {
                    chunk.iter_mut().zip(&ks).for_each(|(b, k)| *b ^= k);
                    chunk.to_vec()
                } else {
                    let ct = chunk.to_vec();
                    chunk.iter_mut().zip(&ks).for_each(|(b, k)| *b ^= k);
                    ct
                };
                reg[..ct.len()].copy_from_slice(&ct);
            }
        }
    }
}

/// Decrypt with a transformation (no HMAC, no inline IV).
pub fn decrypt(t: &Transformation, key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    if t.mode == Mode::Gcm {
        let n: [u8; 12] = iv.try_into().map_err(|_| crypto_err("GCM requires a 12 byte IV"))?;
        return AesGcm::new(key).open(&n, &[], data).ok_or_else(|| crypto_err("Tag mismatch!"));
    }
    let c = Block::new(t.alg, key)?;
    let bs = t.block_size();
    if t.mode != Mode::Ecb && iv.len() != bs {
        return Err(crypto_err(format!("Wrong IV length: must be {} bytes long", bs)));
    }
    let mut buf = data.to_vec();
    match t.mode {
        Mode::Ecb | Mode::Cbc => {
            if buf.len() % bs != 0 {
                return Err(crypto_err(format!("Input length must be multiple of {} when decrypting with padded cipher", bs)));
            }
            let mut prev = iv.to_vec();
            for chunk in buf.chunks_mut(bs) {
                let saved = chunk.to_vec();
                c.dec(chunk);
                if t.mode == Mode::Cbc {
                    chunk.iter_mut().zip(&prev).for_each(|(b, p)| *b ^= p);
                    prev = saved;
                }
            }
            if t.padding {
                let p = *buf.last().ok_or_else(|| crypto_err("Given final block not properly padded"))? as usize;
                if p == 0 || p > bs || buf.len() < p || buf[buf.len() - p..].iter().any(|b| *b as usize != p) {
                    return Err(crypto_err("Given final block not properly padded. Such issues can arise if a bad key is used during decryption."));
                }
                buf.truncate(buf.len() - p);
            }
        }
        _ => stream(&c, t.mode, iv, &mut buf, false),
    }
    Ok(buf)
}

fn mac_alg(name: &str) -> Result<HashAlg> {
    Ok(match name.to_ascii_uppercase().replace('-', "").as_str() {
        "HMACSHA1" => HashAlg::Sha1,
        "HMACSHA256" => HashAlg::Sha256,
        "HMACSHA384" => HashAlg::Sha384,
        "HMACSHA512" => HashAlg::Sha512,
        other => return Err(crypto_err(format!("Unsupported MAC algorithm: {}", other))),
    })
}

/// The `crypto` data format.
pub struct CryptoDataFormat {
    pub transformation: Transformation,
    pub key: Option<Vec<u8>>,
    pub key_ref: Option<String>,
    pub init_vector: Option<Vec<u8>>,
    pub init_vector_ref: Option<String>,
    pub inline: bool,
    pub should_append_hmac: bool,
    pub mac: HashAlg,
}

impl CryptoDataFormat {
    /// A data format for `algorithm` with a fixed key.
    pub fn new(algorithm: &str, key: &[u8]) -> Result<CryptoDataFormat> {
        Ok(CryptoDataFormat {
            transformation: Transformation::parse(algorithm)?,
            key: Some(key.to_vec()),
            key_ref: None,
            init_vector: None,
            init_vector_ref: None,
            inline: false,
            should_append_hmac: true,
            mac: HashAlg::Sha1,
        })
    }

    fn key(&self, ex: &Exchange) -> Result<Vec<u8>> {
        if let Some(k) = ex.message.headers.get(KEY) {
            return value_bytes(k);
        }
        if let Some(k) = &self.key {
            return Ok(k.clone());
        }
        if let Some(r) = &self.key_ref {
            return registry_bytes(ex, r)?.ok_or_else(|| CamelError::new("NoSuchBeanException", format!("No bean could be found in the registry for: {}", r)));
        }
        Err(crypto_err("A valid encryption Key is required. Either configure the CryptoDataFormat with a key or provide one in a header using the header name 'CamelCryptoKey'"))
    }

    fn iv(&self, ex: &Exchange) -> Result<Option<Vec<u8>>> {
        if let Some(k) = ex.message.headers.get(INIT_VECTOR) {
            return value_bytes(k).map(Some);
        }
        if let Some(k) = &self.init_vector {
            return Ok(Some(k.clone()));
        }
        if let Some(r) = &self.init_vector_ref {
            return registry_bytes(ex, r)?.ok_or_else(|| CamelError::new("NoSuchBeanException", format!("No bean could be found in the registry for: {}", r))).map(Some);
        }
        Ok(None)
    }
}

impl DataFormat for CryptoDataFormat {
    fn name(&self) -> &str {
        "crypto"
    }
    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        let key = self.key(ex)?;
        let t = &self.transformation;
        let iv = match self.iv(ex)? {
            Some(iv) => iv,
            None => rng::random_bytes(t.iv_len()),
        };
        let data = body_bytes(&ex.message.body);
        let ct = encrypt(t, &key, &iv, &data)?;
        let mut out = Vec::new();
        if self.inline {
            out.extend_from_slice(&(iv.len() as u32).to_be_bytes());
            out.extend_from_slice(&iv);
        }
        out.extend_from_slice(&ct);
        if self.should_append_hmac {
            out.extend(self.mac.hmac(&key, &ct));
        }
        ex.message.body = Value::Bytes(out);
        Ok(())
    }
    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        let data = body_bytes(&ex.message.body);
        if data.is_empty() {
            ex.message.body = Value::Null;
            return Ok(());
        }
        let key = self.key(ex)?;
        let t = &self.transformation;
        let (iv, mut rest) = if self.inline {
            if data.len() < 4 {
                return Err(crypto_err("Missing inlined initialization vector"));
            }
            let n = u32::from_be_bytes(data[..4].try_into().unwrap()) as usize;
            if data.len() < 4 + n {
                return Err(crypto_err("Truncated inlined initialization vector"));
            }
            (data[4..4 + n].to_vec(), &data[4 + n..])
        } else {
            match self.iv(ex)? {
                Some(iv) => (iv, &data[..]),
                None if t.iv_len() == 0 => (Vec::new(), &data[..]),
                None => return Err(crypto_err("An initialization vector is required (initVectorRef or inline=true)")),
            }
        };
        if self.should_append_hmac {
            let ml = self.mac.output_len();
            if rest.len() < ml {
                return Err(crypto_err("Input stream was tampered with: missing HMAC"));
            }
            let (ct, mac) = rest.split_at(rest.len() - ml);
            if !ct_eq(&self.mac.hmac(&key, ct), mac) {
                return Err(crypto_err("Input stream was tampered with: HMAC check failed"));
            }
            rest = ct;
        }
        ex.message.body = Value::Bytes(decrypt(t, &key, &iv, rest)?);
        Ok(())
    }
}

pub(crate) fn create(o: &Opts) -> Result<Arc<dyn DataFormat>> {
    Ok(Arc::new(CryptoDataFormat {
        transformation: Transformation::parse(o.get(&["algorithm"]).unwrap_or("DES/CBC/PKCS5Padding"))?,
        key: o.get(&["key"]).map(parse_key_material).transpose()?,
        key_ref: o.get(&["keyRef"]).map(String::from),
        init_vector: o.get(&["initVector", "initializationVector"]).map(parse_key_material).transpose()?,
        init_vector_ref: o.get(&["initVectorRef"]).map(String::from),
        inline: o.bool(&["inline"], false)?,
        should_append_hmac: o.bool(&["shouldAppendHMAC", "shouldAppendHmac"], true)?,
        mac: mac_alg(o.get(&["macAlgorithm"]).unwrap_or("HmacSHA1"))?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use camel_tls::util::{hex, unhex};

    #[test]
    fn nist_sp800_38a_vectors() {
        // F.2.1 CBC-AES128.Encrypt, F.5.1 CTR-AES128, F.3.13 CFB128-AES128, F.4.1 OFB-AES128
        let key = unhex("2b7e151628aed2a6abf7158809cf4f3c");
        let pt = unhex("6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e51");
        let iv = unhex("000102030405060708090a0b0c0d0e0f");
        let cbc = Transformation::parse("AES/CBC/NoPadding").unwrap();
        assert_eq!(hex(&encrypt(&cbc, &key, &iv, &pt).unwrap()), "7649abac8119b246cee98e9b12e9197d5086cb9b507219ee95db113a917678b2");
        let ctr_iv = unhex("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff");
        let ctr = Transformation::parse("AES/CTR/NoPadding").unwrap();
        let c = encrypt(&ctr, &key, &ctr_iv, &pt).unwrap();
        assert_eq!(hex(&c), "874d6191b620e3261bef6864990db6ce9806f66b7970fdff8617187bb9fffdff");
        assert_eq!(decrypt(&ctr, &key, &ctr_iv, &c).unwrap(), pt);
        let cfb = Transformation::parse("AES/CFB/NoPadding").unwrap();
        let c = encrypt(&cfb, &key, &iv, &pt).unwrap();
        assert_eq!(hex(&c), "3b3fd92eb72dad20333449f8e83cfb4ac8a64537a0b3a93fcde3cdad9f1ce58b");
        assert_eq!(decrypt(&cfb, &key, &iv, &c).unwrap(), pt);
        let ofb = Transformation::parse("AES/OFB/NoPadding").unwrap();
        assert_eq!(hex(&encrypt(&ofb, &key, &iv, &pt).unwrap()), "3b3fd92eb72dad20333449f8e83cfb4a7789508d16918f03f53c52dac54ed825");
        // ECB F.1.1
        let ecb = Transformation::parse("AES/ECB/NoPadding").unwrap();
        assert_eq!(hex(&encrypt(&ecb, &key, &[], &pt[..16]).unwrap()), "3ad77bb40d7a3660a89ecaf32466ef97");
    }

    #[test]
    fn gcm_test_case_3() {
        // McGrew & Viega GCM spec, test case 3
        let key = unhex("feffe9928665731c6d6a8f9467308308");
        let iv = unhex("cafebabefacedbaddecaf888");
        let pt = unhex("d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a721c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b391aafd255");
        let t = Transformation::parse("AES/GCM/NoPadding").unwrap();
        let c = encrypt(&t, &key, &iv, &pt).unwrap();
        assert_eq!(hex(&c[c.len() - 16..]), "4d5c2af327cd64a62cf35abd2ba6fab4");
        assert_eq!(decrypt(&t, &key, &iv, &c).unwrap(), pt);
    }

    #[test]
    fn des_vector() {
        // classic DES test: key 133457799BBCDFF1, pt 0123456789ABCDEF -> 85E813540F0AB405
        let t = Transformation::parse("DES/ECB/NoPadding").unwrap();
        assert_eq!(hex(&encrypt(&t, &unhex("133457799bbcdff1"), &[], &unhex("0123456789abcdef")).unwrap()), "85e813540f0ab405");
    }
}

//! `pgp` data format: OpenPGP (RFC 4880 / RFC 9580) encryption, decryption, signing and
//! verification, compatible with GnuPG.
//!
//! Supported: v4 keys - RSA, EdDSA (Ed25519), ECDSA and ECDH (Curve25519, NIST P-256/P-384);
//! ASCII-armored or binary keyrings (public and secret, several keys per ring); secret keys
//! protected with iterated+salted / salted / simple S2K (SHA-1/SHA-2) and AES-128/192/256 or
//! TripleDES, SHA-1 (254) or checksum (255) protected; PKESK v3 + SEIPD v1 with MDC
//! (`integrity=true`, default) or legacy SED (`integrity=false`); AES-128/192/256 and
//! TripleDES session keys; decryption of GnuPG's OCB Encrypted Data packets (tag 20); ZIP, ZLIB or no compression; one-pass signed messages (RSA,
//! EdDSA, ECDSA with SHA-1/256/384/512) and their verification.
//! Not supported: CAST5/IDEA/Blowfish/Twofish/Camellia, BZip2, v5/v6 keys, AEAD (SEIPD v2,
//! EAX, OCB encryption), password-only (SKESK) messages, cleartext signatures, DSA/ElGamal.
//!
//! Options (Camel's names):
//! * marshal (encrypt): `keyFileName` (public keyring) or `encryptionKeyRing` (inline
//!   armored keyring or `#bean`), `keyUserid` / `keyUserids` (user ID parts selecting the
//!   recipients; default: every key in the ring), `armored` (default false), `integrity`
//!   (default true), `algorithm` (symmetric: `9`/`AES256` default, `8`/`AES192`,
//!   `7`/`AES128`, `2`/`TRIPLEDES`), `compressionAlgorithm` (`1` ZIP default, `2` ZLIB,
//!   `0` none), `hashAlgorithm` (`8` SHA256 default, `2`, `9`, `10`), `fileName` (literal
//!   data name, default the `CamelFileName` header or `_CONSOLE`), and for signing
//!   `signatureKeyFileName` / `signatureKeyRing` (secret keyring), `signatureKeyUserid` /
//!   `signatureKeyUserids`, `signaturePassword`.
//! * unmarshal (decrypt): `keyFileName` / `encryptionKeyRing` (secret keyring),
//!   `keyUserid` (restricts the keys), `password`; verification with
//!   `signatureKeyFileName` / `signatureKeyRing` (public keyring), `signatureKeyUserid(s)`
//!   (accepted signers) and `signatureVerificationOption`: `optional` (default: verify
//!   when signed), `required`, `no_signature_allowed`, `ignore`.
//! * Per message overrides with the `CamelPGPDataFormat*` headers (`KeyUserid`,
//!   `KeyUserids`, `KeyPassword`, `SignatureKeyUserid`, `SignatureKeyUserids`,
//!   `SignatureKeyPassword`, `KeyFileName`, `SignatureKeyFileName`, `EncryptionKeyRing`,
//!   `SignatureKeyRing`, `EncryptionAlgorithm`, `SignatureAlgorithm`, `CompressionAlgorithm`).
//!   Marshal sets `CamelPGPDataFormatNumberOfEncryptionKeys` and
//!   `CamelPGPDataFormatNumberOfSigningKeys`.

pub mod key;
pub mod message;
pub mod ocb;
pub mod packet;

use std::sync::Arc;

use cameleer_core::dataformat::DataFormat;
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::props::FILE_NAME;
use cameleer_core::message::Exchange;
use cameleer_core::Value;

use crate::{body_bytes, Opts};

pub use key::{generate_ed25519, parse_keyring, Cert, GeneratedKey, Key};
pub use message::{decrypt, encrypt, sign, Decrypted, Signer, WriteOptions};

pub(crate) fn pgp_err(msg: impl Into<String>) -> CamelError {
    CamelError::new("PGPException", msg)
}

const H: &str = "CamelPGPDataFormat";

/// Signature verification options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verification {
    Optional,
    Required,
    NoSignatureAllowed,
    Ignore,
}

/// The `pgp` data format.
#[derive(Clone, Debug)]
pub struct PgpDataFormat {
    pub key_file_name: Option<String>,
    pub encryption_key_ring: Option<String>,
    pub key_userids: Vec<String>,
    pub password: Option<String>,
    pub armored: bool,
    pub integrity: bool,
    pub algorithm: u8,
    pub compression: u8,
    pub hash: u8,
    pub file_name: Option<String>,
    pub signature_key_file_name: Option<String>,
    pub signature_key_ring: Option<String>,
    pub signature_key_userids: Vec<String>,
    pub signature_password: Option<String>,
    pub verification: Verification,
}

impl Default for PgpDataFormat {
    fn default() -> Self {
        PgpDataFormat {
            key_file_name: None,
            encryption_key_ring: None,
            key_userids: Vec::new(),
            password: None,
            armored: false,
            integrity: true,
            algorithm: packet::SYM_AES256,
            compression: message::COMP_ZIP,
            hash: key::HASH_SHA256,
            file_name: None,
            signature_key_file_name: None,
            signature_key_ring: None,
            signature_key_userids: Vec::new(),
            signature_password: None,
            verification: Verification::Optional,
        }
    }
}

fn header_str(ex: &Exchange, name: &str) -> Option<String> {
    ex.message.headers.get(&format!("{}{}", H, name)).filter(|v| !v.is_null()).map(|v| v.to_string())
}

fn list(s: &str) -> Vec<String> {
    s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()
}

fn header_list(ex: &Exchange, name: &str) -> Option<Vec<String>> {
    ex.message.headers.get(&format!("{}{}", H, name)).map(|v| match v {
        Value::Array(a) => a.iter().map(|x| x.to_string()).collect(),
        other => list(&other.to_string()),
    })
}

fn sym_alg(s: &str) -> Result<u8> {
    Ok(match s.trim().to_ascii_uppercase().as_str() {
        "AES256" | "AES_256" | "9" => packet::SYM_AES256,
        "AES192" | "AES_192" | "8" => packet::SYM_AES192,
        "AES128" | "AES_128" | "7" => packet::SYM_AES128,
        "TRIPLEDES" | "TRIPLE_DES" | "3DES" | "2" => packet::SYM_TRIPLEDES,
        other => return Err(CamelError::illegal_argument(format!("Unsupported PGP symmetric algorithm: {}", other))),
    })
}

fn hash_id(s: &str) -> Result<u8> {
    Ok(match s.trim().to_ascii_uppercase().replace('-', "").as_str() {
        "SHA256" | "8" => key::HASH_SHA256,
        "SHA1" | "2" => key::HASH_SHA1,
        "SHA384" | "9" => key::HASH_SHA384,
        "SHA512" | "10" => key::HASH_SHA512,
        other => return Err(CamelError::illegal_argument(format!("Unsupported PGP hash algorithm: {}", other))),
    })
}

fn comp_id(s: &str) -> Result<u8> {
    Ok(match s.trim().to_ascii_uppercase().as_str() {
        "ZIP" | "1" => message::COMP_ZIP,
        "ZLIB" | "2" => message::COMP_ZLIB,
        "UNCOMPRESSED" | "NONE" | "0" => message::COMP_NONE,
        other => return Err(CamelError::illegal_argument(format!("Unsupported PGP compression algorithm: {}", other))),
    })
}

/// Load a keyring from inline text (`-----BEGIN PGP ...`), a registry bean (`#name`, holding
/// a `String` or `Vec<u8>`), or a file.
fn load_ring(ex: &Exchange, inline: Option<&str>, file: Option<&str>) -> Result<Option<Vec<Cert>>> {
    if let Some(r) = inline {
        if r.starts_with('#') {
            let b = crate::crypto::registry_bytes(ex, r)?.ok_or_else(|| CamelError::new("NoSuchBeanException", format!("No keyring bean found for {}", r)))?;
            return parse_keyring(&b).map(Some);
        }
        return parse_keyring(r.as_bytes()).map(Some);
    }
    if let Some(f) = file {
        return parse_keyring(&crate::read_resource(f)?).map(Some);
    }
    Ok(None)
}

impl PgpDataFormat {
    fn enc_ring(&self, ex: &Exchange) -> Result<Vec<Cert>> {
        let inline = header_str(ex, "EncryptionKeyRing").or_else(|| self.encryption_key_ring.clone());
        let file = header_str(ex, "KeyFileName").or_else(|| self.key_file_name.clone());
        load_ring(ex, inline.as_deref(), file.as_deref())?.ok_or_else(|| pgp_err("Either keyFileName or encryptionKeyRing must be set"))
    }

    fn sig_ring(&self, ex: &Exchange) -> Result<Option<Vec<Cert>>> {
        let inline = header_str(ex, "SignatureKeyRing").or_else(|| self.signature_key_ring.clone());
        let file = header_str(ex, "SignatureKeyFileName").or_else(|| self.signature_key_file_name.clone());
        load_ring(ex, inline.as_deref(), file.as_deref())
    }

    fn userids(&self, ex: &Exchange) -> Vec<String> {
        if let Some(l) = header_list(ex, "KeyUserids") {
            return l;
        }
        if let Some(u) = header_str(ex, "KeyUserid") {
            return vec![u];
        }
        self.key_userids.clone()
    }

    fn sig_userids(&self, ex: &Exchange) -> Vec<String> {
        if let Some(l) = header_list(ex, "SignatureKeyUserids") {
            return l;
        }
        if let Some(u) = header_str(ex, "SignatureKeyUserid") {
            return vec![u];
        }
        self.signature_key_userids.clone()
    }
}

fn select<'a>(certs: &'a [Cert], ids: &[String]) -> Vec<&'a Cert> {
    certs.iter().filter(|c| ids.is_empty() || ids.iter().any(|i| c.matches_user_id(i))).collect()
}

impl DataFormat for PgpDataFormat {
    fn name(&self) -> &str {
        "pgp"
    }

    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        let ring = self.enc_ring(ex)?;
        let ids = self.userids(ex);
        let mut recipients: Vec<&Key> = Vec::new();
        for c in select(&ring, &ids) {
            // prefer encryption subkeys
            if let Some(k) = c.subkeys.iter().rev().find(|k| k.can_encrypt()).or(if c.primary.can_encrypt() { Some(&c.primary) } else { None }) {
                recipients.push(k);
            }
        }
        if recipients.is_empty() {
            return Err(pgp_err(format!("Cannot PGP encrypt message. No public encryption key found for the User Ids {:?} in the public keyring. Either specify other User IDs or add correct public keys to the keyring.", ids)));
        }
        let mut opts = WriteOptions {
            sym: header_str(ex, "EncryptionAlgorithm").map(|s| sym_alg(&s)).transpose()?.unwrap_or(self.algorithm),
            compression: header_str(ex, "CompressionAlgorithm").map(|s| comp_id(&s)).transpose()?.unwrap_or(self.compression),
            hash: header_str(ex, "SignatureAlgorithm").map(|s| hash_id(&s)).transpose()?.unwrap_or(self.hash),
            integrity: self.integrity,
            file_name: self.file_name.clone().or_else(|| ex.message.headers.get(FILE_NAME).map(|v| v.to_string())).unwrap_or_else(|| "_CONSOLE".into()),
        };
        if opts.file_name.is_empty() {
            opts.file_name = "_CONSOLE".into();
        }
        let sig_ring = self.sig_ring(ex)?;
        let mut signers = Vec::new();
        if let Some(sr) = &sig_ring {
            let pw = header_str(ex, "SignatureKeyPassword").or_else(|| self.signature_password.clone());
            let sids = self.sig_userids(ex);
            for c in select(sr, &sids) {
                let k = if c.primary.can_sign() && c.primary.has_secret() { Some(&c.primary) } else { c.subkeys.iter().find(|k| k.can_sign() && k.has_secret()) };
                if let Some(k) = k {
                    signers.push(Signer { key: k, secret: k.unlock(pw.as_deref())? });
                }
            }
            if signers.is_empty() {
                return Err(pgp_err(format!("Cannot PGP sign message. No secret key found for User IDs {:?}.", sids)));
            }
        }
        let n_rec = recipients.len();
        let n_sig = signers.len();
        let data = body_bytes(&ex.message.body);
        let bin = encrypt(&data, &recipients, &signers, &opts)?;
        ex.message.body = if self.armored { Value::String(packet::armor("MESSAGE", &bin)) } else { Value::Bytes(bin) };
        ex.message.headers.set(format!("{}NumberOfEncryptionKeys", H), n_rec as i64);
        ex.message.headers.set(format!("{}NumberOfSigningKeys", H), n_sig as i64);
        Ok(())
    }

    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        let data = body_bytes(&ex.message.body);
        if data.is_empty() {
            ex.message.body = Value::Null;
            return Ok(());
        }
        let ring = self.enc_ring(ex)?;
        let ids = self.userids(ex);
        let pw = header_str(ex, "KeyPassword").or_else(|| self.password.clone());
        let secret: Vec<(Cert, Option<String>)> = select(&ring, &ids).into_iter().map(|c| (c.clone(), pw.clone())).collect();
        let verify = if self.verification == Verification::Ignore { Vec::new() } else { self.sig_ring(ex)?.unwrap_or_default() };
        let d = decrypt(&data, &secret, &verify)?;
        if !d.encrypted {
            return Err(pgp_err("The input message body is not an encrypted PGP message"));
        }
        let sids = self.sig_userids(ex);
        match self.verification {
            Verification::Ignore => {}
            Verification::NoSignatureAllowed if !d.signatures.is_empty() => return Err(pgp_err("PGP message contains a signature although a signature is not expected (signatureVerificationOption=no_signature_allowed)")),
            Verification::NoSignatureAllowed => {}
            v => {
                if d.signatures.is_empty() {
                    if v == Verification::Required {
                        return Err(pgp_err("PGP message does not contain any signatures although a signature is expected (signatureVerificationOption=required)"));
                    }
                } else if !verify.is_empty() {
                    let ok = d.signatures.iter().any(|s| s.valid && (sids.is_empty() || s.user_ids.as_ref().map(|u| u.iter().any(|x| sids.iter().any(|i| x.contains(i.as_str())))).unwrap_or(false)));
                    if !ok {
                        let known = d.signatures.iter().any(|s| s.user_ids.is_some());
                        return Err(pgp_err(if known { "Verification of the PGP signature failed" } else { "PGP message is signed but the signature key is not in the signature public keyring (or not allowed by the signature user IDs)" }));
                    }
                } else if v == Verification::Required {
                    return Err(pgp_err("Signature verification required but no signature keyring configured (signatureKeyFileName)"));
                }
            }
        }
        ex.message.body = Value::Bytes(d.data);
        Ok(())
    }
}

pub(crate) fn create(o: &Opts) -> Result<Arc<dyn DataFormat>> {
    let mut ids = o.get(&["keyUserids"]).map(list).unwrap_or_default();
    if let Some(u) = o.get(&["keyUserid"]) {
        ids.insert(0, u.to_string());
    }
    let mut sids = o.get(&["signatureKeyUserids"]).map(list).unwrap_or_default();
    if let Some(u) = o.get(&["signatureKeyUserid"]) {
        sids.insert(0, u.to_string());
    }
    Ok(Arc::new(PgpDataFormat {
        key_file_name: o.get(&["keyFileName"]).map(String::from),
        encryption_key_ring: o.get(&["encryptionKeyRing", "keyring"]).map(String::from),
        key_userids: ids,
        password: o.get(&["password"]).map(String::from),
        armored: o.bool(&["armored"], false)?,
        integrity: o.bool(&["integrity"], true)?,
        algorithm: o.get(&["algorithm"]).map(sym_alg).transpose()?.unwrap_or(packet::SYM_AES256),
        compression: o.get(&["compressionAlgorithm"]).map(comp_id).transpose()?.unwrap_or(message::COMP_ZIP),
        hash: o.get(&["hashAlgorithm"]).map(hash_id).transpose()?.unwrap_or(key::HASH_SHA256),
        file_name: o.get(&["fileName"]).map(String::from),
        signature_key_file_name: o.get(&["signatureKeyFileName"]).map(String::from),
        signature_key_ring: o.get(&["signatureKeyRing"]).map(String::from),
        signature_key_userids: sids,
        signature_password: o.get(&["signaturePassword"]).map(String::from),
        verification: match o.get(&["signatureVerificationOption"]).map(|s| s.to_ascii_lowercase()) {
            None => Verification::Optional,
            Some(s) if s == "optional" => Verification::Optional,
            Some(s) if s == "required" => Verification::Required,
            Some(s) if s == "no_signature_allowed" => Verification::NoSignatureAllowed,
            Some(s) if s == "ignore" => Verification::Ignore,
            Some(s) => return Err(CamelError::illegal_argument(format!("Invalid signatureVerificationOption: {}", s))),
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::key::HASH_SHA512;

    #[test]
    fn ed25519_round_trip_signed() {
        let alice = generate_ed25519("Alice <alice@example.com>", Some("pw")).unwrap();
        let bob = generate_ed25519("Bob <bob@example.com>", None).unwrap();
        let bob_pub = parse_keyring(bob.public_keyring.as_bytes()).unwrap();
        let alice_sec = parse_keyring(alice.secret_keyring.as_bytes()).unwrap();
        let alice_pub = parse_keyring(alice.public_keyring.as_bytes()).unwrap();
        let bob_sec = parse_keyring(bob.secret_keyring.as_bytes()).unwrap();
        let signer = Signer { key: &alice_sec[0].primary, secret: alice_sec[0].primary.unlock(Some("pw")).unwrap() };
        for (sym, comp, integrity) in [(packet::SYM_AES256, message::COMP_ZIP, true), (packet::SYM_AES128, message::COMP_ZLIB, true), (packet::SYM_TRIPLEDES, message::COMP_NONE, false)] {
            let opts = WriteOptions { sym, compression: comp, integrity, hash: HASH_SHA512, ..Default::default() };
            let msg = encrypt(b"hello pgp", &[&bob_pub[0].subkeys[0]], std::slice::from_ref(&signer), &opts).unwrap();
            let d = decrypt(&msg, &[(bob_sec[0].clone(), None)], &alice_pub).unwrap();
            assert_eq!(d.data, b"hello pgp");
            assert_eq!(d.integrity_protected, integrity);
            assert_eq!(d.signatures.len(), 1);
            assert!(d.signatures[0].valid);
            // wrong verification key
            let d = decrypt(&msg, &[(bob_sec[0].clone(), None)], &bob_pub).unwrap();
            assert!(!d.signatures[0].valid || d.signatures[0].user_ids.is_none());
        }
        // tampering is detected
        let mut msg = encrypt(b"hello pgp", &[&bob_pub[0].subkeys[0]], &[], &WriteOptions::default()).unwrap();
        let n = msg.len();
        msg[n - 5] ^= 1;
        assert!(decrypt(&msg, &[(bob_sec[0].clone(), None)], &[]).is_err());
        // wrong key
        let msg = encrypt(b"x", &[&bob_pub[0].subkeys[0]], &[], &WriteOptions::default()).unwrap();
        assert!(decrypt(&msg, &[(alice_sec[0].clone(), Some("pw".into()))], &[]).is_err());
    }
}

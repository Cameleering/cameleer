//! PKCS#12 (RFC 7292) reader and writer.
//!
//! Reading supports password integrity (MacData with HMAC-SHA1/SHA-224/SHA-256/
//! SHA-384/SHA-512 and the PKCS#12 KDF), `data` and `encryptedData` safe contents
//! (PBES2 with PBKDF2-HMAC-SHA* and AES-CBC/3DES, legacy
//! `pbeWithSHAAnd3-KeyTripleDES-CBC`, `pbeWithSHAAnd2-KeyTripleDES-CBC`,
//! `pbeWithSHAAnd128BitRC2-CBC`, `pbeWithSHAAnd40BitRC2-CBC`), key bags, shrouded
//! key bags, certificate bags and nested safe-contents bags. Public-key integrity
//! and privacy modes (signedData / envelopedData) are not supported.

use super::{KeyStore, KeyStoreEntry};
use crate::asn1::{self, Der, Tlv, TAG_OCTET_STRING, TAG_SEQUENCE, TAG_SET};
use crate::crypto::hash::HashAlg;
use crate::crypto::kdf::{bmp_password, pkcs12_kdf, PKCS12_MAC};
use crate::crypto::rng;
use crate::keys::PrivateKey;
use crate::pbe;
use crate::util::ct_eq;
use crate::x509::Certificate;

const OID_DATA: &str = "1.2.840.113549.1.7.1";
const OID_ENCRYPTED_DATA: &str = "1.2.840.113549.1.7.6";
const OID_KEY_BAG: &str = "1.2.840.113549.1.12.10.1.1";
const OID_SHROUDED_KEY_BAG: &str = "1.2.840.113549.1.12.10.1.2";
const OID_CERT_BAG: &str = "1.2.840.113549.1.12.10.1.3";
const OID_SAFE_CONTENTS_BAG: &str = "1.2.840.113549.1.12.10.1.6";
const OID_X509_CERT: &str = "1.2.840.113549.1.9.22.1";
const OID_FRIENDLY_NAME: &str = "1.2.840.113549.1.9.20";
const OID_LOCAL_KEY_ID: &str = "1.2.840.113549.1.9.21";
/// Java's "trusted for any usage" attribute marking trusted certificate entries.
const OID_ORACLE_TRUSTED: &str = "2.16.840.1.113894.746875.1.1";
const OID_ANY_EKU: &str = "2.5.29.37.0";
const OID_SHA1: &str = "1.3.14.3.2.26";
const OID_SHA224: &str = "2.16.840.1.101.3.4.2.4";
const OID_SHA256: &str = "2.16.840.1.101.3.4.2.1";
const OID_SHA384: &str = "2.16.840.1.101.3.4.2.2";
const OID_SHA512: &str = "2.16.840.1.101.3.4.2.3";

fn e<T: std::fmt::Display>(x: T) -> String {
    format!("PKCS#12: {x}")
}

#[derive(Default)]
struct Attrs {
    name: Option<String>,
    key_id: Option<Vec<u8>>,
    trusted: bool,
}

fn parse_attrs(d: &mut Der) -> Result<Attrs, String> {
    let mut a = Attrs::default();
    if let Some(set) = d.optional(TAG_SET).map_err(e)? {
        let mut r = set.reader();
        while !r.is_empty() {
            let mut attr = r.sequence().map_err(e)?;
            let oid = attr.oid().map_err(e)?;
            let values = attr.expect(TAG_SET).map_err(e)?;
            let mut vr = values.reader();
            match oid.as_str() {
                OID_FRIENDLY_NAME => {
                    if let Ok(v) = vr.read() {
                        a.name = asn1::decode_string(&v);
                    }
                }
                OID_LOCAL_KEY_ID => a.key_id = vr.octet_string().ok().map(|v| v.to_vec()),
                OID_ORACLE_TRUSTED => a.trusted = true,
                _ => {}
            }
        }
    }
    Ok(a)
}

/// `[0] IMPLICIT OCTET STRING` content, primitive or constructed.
fn implicit_octets(t: &Tlv) -> Result<Vec<u8>, String> {
    match t.tag {
        0x80 => Ok(t.value.to_vec()),
        0xa0 => {
            let mut out = Vec::new();
            let mut r = t.reader();
            while !r.is_empty() {
                out.extend_from_slice(r.octet_string().map_err(e)?);
            }
            Ok(out)
        }
        other => Err(e(format!("unexpected encrypted content tag {other:#04x}"))),
    }
}

/// An OCTET STRING that may be constructed (BER).
fn octets(t: &Tlv) -> Result<Vec<u8>, String> {
    match t.tag {
        TAG_OCTET_STRING => Ok(t.value.to_vec()),
        0x24 => {
            let mut out = Vec::new();
            let mut r = t.reader();
            while !r.is_empty() {
                out.extend_from_slice(r.octet_string().map_err(e)?);
            }
            Ok(out)
        }
        other => Err(e(format!("expected OCTET STRING, found tag {other:#04x}"))),
    }
}

fn hash_from_oid(oid: &str) -> Option<HashAlg> {
    match oid {
        OID_SHA1 => Some(HashAlg::Sha1),
        OID_SHA256 => Some(HashAlg::Sha256),
        OID_SHA384 => Some(HashAlg::Sha384),
        OID_SHA512 => Some(HashAlg::Sha512),
        _ => None,
    }
}

struct Collected {
    keys: Vec<(Attrs, PrivateKey)>,
    certs: Vec<(Attrs, Vec<u8>)>,
}

fn parse_safe_contents(data: &[u8], key_password: &str, out: &mut Collected, depth: usize) -> Result<(), String> {
    if depth > 4 {
        return Err(e("safe contents nested too deeply"));
    }
    let mut outer = Der::new(data);
    let mut bags = outer.sequence().map_err(e)?;
    while !bags.is_empty() {
        let mut bag = bags.sequence().map_err(e)?;
        let oid = bag.oid().map_err(e)?;
        let value = bag.expect(0xa0).map_err(e)?;
        let attrs = parse_attrs(&mut bag)?;
        let mut v = value.reader();
        match oid.as_str() {
            OID_KEY_BAG => {
                let der = v.expect(TAG_SEQUENCE).map_err(e)?;
                out.keys.push((attrs, PrivateKey::from_pkcs8(der.raw).map_err(e)?));
            }
            OID_SHROUDED_KEY_BAG => {
                let der = v.expect(TAG_SEQUENCE).map_err(e)?;
                let plain = pbe::decrypt_encrypted_private_key_info(der.raw, key_password)
                    .map_err(|x| e(format!("cannot decrypt private key: {x}")))?;
                out.keys.push((attrs, PrivateKey::from_pkcs8(&plain).map_err(|x| e(format!("private key (wrong key password?): {x}")))?));
            }
            OID_CERT_BAG => {
                let mut cb = v.sequence().map_err(e)?;
                let cert_type = cb.oid().map_err(e)?;
                let cv = cb.expect(0xa0).map_err(e)?;
                if cert_type == OID_X509_CERT {
                    let t = cv.reader().read().map_err(e)?;
                    out.certs.push((attrs, octets(&t)?));
                }
            }
            OID_SAFE_CONTENTS_BAG => {
                let inner = v.expect(TAG_SEQUENCE).map_err(e)?;
                parse_safe_contents(inner.raw, key_password, out, depth + 1)?;
            }
            _ => {} // CRL and secret bags are ignored
        }
    }
    Ok(())
}

/// Parses a PKCS#12 file. `password` is the integrity/privacy password,
/// `key_password` (default: `password`) decrypts shrouded key bags.
pub fn parse(data: &[u8], password: &str, key_password: Option<&str>) -> Result<KeyStore, String> {
    let mut outer = Der::new(data);
    let mut pfx = outer.sequence().map_err(|x| e(format!("not a PKCS#12 / key store file ({x})")))?;
    if pfx.small_uint().map_err(e)? != 3 {
        return Err(e("unsupported PFX version"));
    }
    let mut auth_safe = pfx.sequence().map_err(e)?;
    let ct = auth_safe.oid().map_err(e)?;
    if ct != OID_DATA {
        return Err(e(format!("unsupported integrity mode (content type {ct}); only password integrity is supported")));
    }
    let content = auth_safe.expect(0xa0).map_err(e)?;
    let auth_data = octets(&content.reader().read().map_err(e)?)?;
    if !pfx.is_empty() {
        // MacData
        let mut mac_data = pfx.sequence().map_err(e)?;
        let mut digest_info = mac_data.sequence().map_err(e)?;
        let mut alg = digest_info.sequence().map_err(e)?;
        let hoid = alg.oid().map_err(e)?;
        if hoid == OID_SHA224 {
            return Err(e("SHA-224 MAC not supported"));
        }
        let h = hash_from_oid(&hoid).ok_or_else(|| e(format!("unsupported MAC algorithm {hoid}")))?;
        let expected = digest_info.octet_string().map_err(e)?.to_vec();
        let salt = mac_data.octet_string().map_err(e)?.to_vec();
        let iter = if mac_data.is_empty() { 1 } else { mac_data.small_uint().map_err(e)? };
        if iter == 0 || iter > 10_000_000 {
            return Err(e("bad MAC iteration count"));
        }
        let key = pkcs12_kdf(h, &bmp_password(password), &salt, PKCS12_MAC, iter as u32, h.output_len());
        let mac = h.hmac(&key, &auth_data);
        // some tools derive the MAC key from an empty password as zero-length input
        let ok = ct_eq(&mac, &expected)
            || (password.is_empty() && ct_eq(&h.hmac(&pkcs12_kdf(h, &[], &salt, PKCS12_MAC, iter as u32, h.output_len()), &auth_data), &expected));
        if !ok {
            return Err("PKCS#12 MAC verification failed: keystore password was incorrect".into());
        }
    }
    let key_password = key_password.unwrap_or(password);
    let mut col = Collected { keys: Vec::new(), certs: Vec::new() };
    let mut seq = Der::new(&auth_data);
    let mut infos = seq.sequence().map_err(e)?;
    while !infos.is_empty() {
        let mut ci = infos.sequence().map_err(e)?;
        let ct = ci.oid().map_err(e)?;
        let content = ci.expect(0xa0).map_err(e)?;
        match ct.as_str() {
            OID_DATA => {
                let sc = octets(&content.reader().read().map_err(e)?)?;
                parse_safe_contents(&sc, key_password, &mut col, 0)?;
            }
            OID_ENCRYPTED_DATA => {
                let mut ed = content.reader().sequence().map_err(e)?;
                let _version = ed.small_uint().map_err(e)?;
                let mut eci = ed.sequence().map_err(e)?;
                let _ct = eci.oid().map_err(e)?;
                let alg = eci.expect(TAG_SEQUENCE).map_err(e)?;
                let enc = eci.read().map_err(e)?;
                let ciphertext = implicit_octets(&enc)?;
                let plain = pbe::decrypt(alg.raw, &ciphertext, password).map_err(|x| e(format!("cannot decrypt safe contents: {x}")))?;
                parse_safe_contents(&plain, key_password, &mut col, 0)?;
            }
            other => return Err(e(format!("unsupported safe contents type {other}"))),
        }
    }
    Ok(assemble(col))
}

fn assemble(col: Collected) -> KeyStore {
    let mut ks = KeyStore::new();
    let mut used = vec![false; col.certs.len()];
    let cert_ders: Vec<Vec<u8>> = col.certs.iter().map(|(_, d)| d.clone()).collect();
    for (i, (attrs, key)) in col.keys.into_iter().enumerate() {
        // leaf by localKeyId, else by public key
        let pk = key.public_key();
        let leaf = attrs
            .key_id
            .as_ref()
            .and_then(|id| col.certs.iter().position(|(a, _)| a.key_id.as_ref() == Some(id)))
            .or_else(|| {
                col.certs.iter().position(|(_, d)| Certificate::from_der(d).ok().and_then(|c| c.public_key).is_some_and(|k| k.same_as(&pk)))
            });
        let mut chain = Vec::new();
        if let Some(l) = leaf {
            used[l] = true;
            let mut ordered = vec![cert_ders[l].clone()];
            ordered.extend(cert_ders.iter().enumerate().filter(|(j, _)| *j != l).map(|(_, d)| d.clone()));
            chain = super::order_chain(&key, &ordered);
            // certificates that complete the chain belong to the key entry
            for c in &chain[1..] {
                if let Some(j) = cert_ders.iter().position(|d| d == c) {
                    if col.certs[j].0.key_id.is_none() && !col.certs[j].0.trusted {
                        used[j] = true;
                    }
                }
            }
        }
        let alias = attrs.name.clone().or_else(|| leaf.and_then(|l| col.certs[l].0.name.clone())).unwrap_or_else(|| format!("{}", i + 1));
        ks.entries.push(KeyStoreEntry::Key { alias, key, chain });
    }
    // Certificates completing a key's chain (OpenSSL -certfile / -chain) belong to that
    // entry, as in Java; the remaining ones are trusted-certificate entries.
    for (j, (attrs, der)) in col.certs.into_iter().enumerate() {
        if used[j] && !attrs.trusted {
            continue;
        }
        if attrs.key_id.is_some() && !attrs.trusted {
            continue;
        }
        let alias = attrs.name.unwrap_or_else(|| format!("cert{j}"));
        ks.entries.push(KeyStoreEntry::Cert { alias, der });
    }
    ks
}

/// Encryption profile used by [`write`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pkcs12Profile {
    /// PBES2 / PBKDF2-HMAC-SHA256 / AES-256-CBC, HMAC-SHA256 MAC (OpenSSL 3, Java 17+).
    Modern,
    /// pbeWithSHAAnd3-KeyTripleDES-CBC keys, pbeWithSHAAnd40BitRC2-CBC certificates,
    /// HMAC-SHA1 MAC (OpenSSL 1.x / older Java defaults).
    Legacy,
}

fn attr(oid: &str, value: Vec<u8>) -> Vec<u8> {
    asn1::seq(&[asn1::oid(oid), asn1::set(&[value])])
}

fn bmp(s: &str) -> Vec<u8> {
    let v: Vec<u8> = s.encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
    asn1::tlv(asn1::TAG_BMP_STRING, &v)
}

fn bag(oid: &str, value: Vec<u8>, attrs: Vec<Vec<u8>>) -> Vec<u8> {
    let mut parts = vec![asn1::oid(oid), asn1::explicit(0, &value)];
    if !attrs.is_empty() {
        parts.push(asn1::set(&attrs));
    }
    asn1::seq(&parts)
}

fn cert_bag(der: &[u8], attrs: Vec<Vec<u8>>) -> Vec<u8> {
    bag(OID_CERT_BAG, asn1::seq(&[asn1::oid(OID_X509_CERT), asn1::explicit(0, &asn1::octet_string(der))]), attrs)
}

/// Serialises a key store as PKCS#12 protected by `password` (also the key password).
pub fn write(ks: &KeyStore, password: &str, profile: Pkcs12Profile) -> Vec<u8> {
    let iterations = 2048;
    let mut key_bags = Vec::new();
    let mut cert_bags = Vec::new();
    for (i, entry) in ks.entries.iter().enumerate() {
        match entry {
            KeyStoreEntry::Key { alias, key, chain } => {
                let id = {
                    let mut v = rng::random_bytes(4);
                    v.extend_from_slice(&(i as u32).to_be_bytes());
                    v
                };
                let attrs = vec![attr(OID_FRIENDLY_NAME, bmp(alias)), attr(OID_LOCAL_KEY_ID, asn1::octet_string(&id))];
                let (alg, ct) = match profile {
                    Pkcs12Profile::Modern => pbe::encrypt_pbes2(&key.to_pkcs8(), password, iterations),
                    Pkcs12Profile::Legacy => pbe::encrypt_pkcs12_legacy(pbe::OID_PBE_SHA_3DES, &key.to_pkcs8(), password, iterations),
                };
                key_bags.push(bag(OID_SHROUDED_KEY_BAG, asn1::seq(&[alg, asn1::octet_string(&ct)]), attrs.clone()));
                for (j, c) in chain.iter().enumerate() {
                    cert_bags.push(cert_bag(c, if j == 0 { attrs.clone() } else { Vec::new() }));
                }
            }
            KeyStoreEntry::Cert { alias, der } => {
                let trusted = attr(OID_ORACLE_TRUSTED, asn1::oid(OID_ANY_EKU));
                cert_bags.push(cert_bag(der, vec![attr(OID_FRIENDLY_NAME, bmp(alias)), trusted]));
            }
        }
    }
    let data_ci = |content: &[u8]| asn1::seq(&[asn1::oid(OID_DATA), asn1::explicit(0, &asn1::octet_string(content))]);
    let mut infos = Vec::new();
    if !cert_bags.is_empty() {
        let sc = asn1::seq(&cert_bags);
        let (alg, ct) = match profile {
            Pkcs12Profile::Modern => pbe::encrypt_pbes2(&sc, password, iterations),
            Pkcs12Profile::Legacy => pbe::encrypt_pkcs12_legacy(pbe::OID_PBE_SHA_RC2_40, &sc, password, iterations),
        };
        let eci = asn1::seq(&[asn1::oid(OID_DATA), alg, asn1::tlv(0x80, &ct)]);
        infos.push(asn1::seq(&[asn1::oid(OID_ENCRYPTED_DATA), asn1::explicit(0, &asn1::seq(&[asn1::uint(&[0]), eci]))]));
    }
    if !key_bags.is_empty() {
        infos.push(data_ci(&asn1::seq(&key_bags)));
    }
    let auth_safe = asn1::seq(&infos);
    let (h, hoid) = match profile {
        Pkcs12Profile::Modern => (HashAlg::Sha256, OID_SHA256),
        Pkcs12Profile::Legacy => (HashAlg::Sha1, OID_SHA1),
    };
    let salt = rng::random_bytes(8);
    let mac_key = pkcs12_kdf(h, &bmp_password(password), &salt, PKCS12_MAC, iterations, h.output_len());
    let mac = h.hmac(&mac_key, &auth_safe);
    let mac_data = asn1::seq(&[
        asn1::seq(&[asn1::seq(&[asn1::oid(hoid), asn1::tlv(asn1::TAG_NULL, &[])]), asn1::octet_string(&mac)]),
        asn1::octet_string(&salt),
        asn1::uint(&iterations.to_be_bytes()),
    ]);
    asn1::seq(&[asn1::uint(&[3]), data_ci(&auth_safe), mac_data])
}

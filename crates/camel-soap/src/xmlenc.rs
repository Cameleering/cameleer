//! WS-Security XML Encryption of the SOAP body: the body content is encrypted with a fresh
//! AES key (`aes128-cbc`, `aes256-cbc`, `aes128-gcm`, `aes256-gcm`) in an
//! `xenc:EncryptedData` (`Type=...#Content`), the key is transported in an
//! `xenc:EncryptedKey` (RSA-OAEP, `rsa-oaep-mgf1p` with SHA-1 or `xmlenc11#rsa-oaep` with a
//! SHA-256 digest) in the `wsse:Security` header, referencing the data through an
//! `xenc:ReferenceList`. Decryption reverses this (CBC padding is read ISO 10126 style).

use camel_core::error::{CamelError, Result};
use camel_tls::crypto::aes::{Aes, AesGcm};
use camel_tls::crypto::hash::HashAlg;
use camel_tls::keys::{PrivateKey, PublicKey};
use camel_tls::pem::{base64_decode, base64_encode};
use camel_tls::x509::Certificate;
use camel_xml::dom::{Document, NodeId, NodeType, QName};

use crate::envelope::SoapVersion;
use crate::wssec::{BASE64_ENCODING, WSSE_NS};

pub const XENC_NS: &str = "http://www.w3.org/2001/04/xmlenc#";
pub const XENC11_NS: &str = "http://www.w3.org/2009/xmlenc11#";
const DSIG_NS: &str = "http://www.w3.org/2000/09/xmldsig#";
const RSA_OAEP_MGF1P: &str = "http://www.w3.org/2001/04/xmlenc#rsa-oaep-mgf1p";
const RSA_OAEP_11: &str = "http://www.w3.org/2009/xmlenc11#rsa-oaep";
const SHA1_URI: &str = "http://www.w3.org/2000/09/xmldsig#sha1";
const SHA256_URI: &str = "http://www.w3.org/2001/04/xmlenc#sha256";
const CONTENT_TYPE: &str = "http://www.w3.org/2001/04/xmlenc#Content";
const ELEMENT_TYPE: &str = "http://www.w3.org/2001/04/xmlenc#Element";
const THUMBPRINT: &str = "http://docs.oasis-open.org/wss/oasis-wss-soap-message-security-1.1#ThumbprintSHA1";

/// Symmetric data encryption algorithm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncAlg {
    Aes128Cbc,
    Aes256Cbc,
    Aes128Gcm,
    Aes256Gcm,
}

impl EncAlg {
    pub fn parse(s: &str) -> Option<EncAlg> {
        let l = s.trim().to_ascii_lowercase();
        let l = l.rsplit('#').next().unwrap_or(&l).to_string();
        Some(match l.as_str() {
            "aes128-cbc" | "aes128" => EncAlg::Aes128Cbc,
            "aes256-cbc" | "aes256" => EncAlg::Aes256Cbc,
            "aes128-gcm" => EncAlg::Aes128Gcm,
            "aes256-gcm" => EncAlg::Aes256Gcm,
            _ => return None,
        })
    }

    pub fn uri(self) -> String {
        match self {
            EncAlg::Aes128Cbc => format!("{}aes128-cbc", XENC_NS),
            EncAlg::Aes256Cbc => format!("{}aes256-cbc", XENC_NS),
            EncAlg::Aes128Gcm => format!("{}aes128-gcm", XENC11_NS),
            EncAlg::Aes256Gcm => format!("{}aes256-gcm", XENC11_NS),
        }
    }

    fn key_len(self) -> usize {
        match self {
            EncAlg::Aes128Cbc | EncAlg::Aes128Gcm => 16,
            _ => 32,
        }
    }

    fn encrypt(self, key: &[u8], data: &[u8]) -> Vec<u8> {
        match self {
            EncAlg::Aes128Cbc | EncAlg::Aes256Cbc => {
                let aes = Aes::new(key);
                let iv = camel_tls::crypto::rng::random_bytes(16);
                let pad = 16 - data.len() % 16;
                let mut buf = data.to_vec();
                buf.extend(std::iter::repeat_n(pad as u8, pad));
                let mut out = iv.clone();
                let mut prev: [u8; 16] = iv.try_into().unwrap();
                for chunk in buf.chunks(16) {
                    let mut b = [0u8; 16];
                    for i in 0..16 {
                        b[i] = chunk[i] ^ prev[i];
                    }
                    aes.encrypt_block(&mut b);
                    out.extend_from_slice(&b);
                    prev = b;
                }
                out
            }
            EncAlg::Aes128Gcm | EncAlg::Aes256Gcm => {
                let gcm = AesGcm::new(key);
                let nonce: [u8; 12] = camel_tls::crypto::rng::random_bytes(12).try_into().unwrap();
                let mut out = nonce.to_vec();
                out.extend(gcm.seal(&nonce, &[], data));
                out
            }
        }
    }

    fn decrypt(self, key: &[u8], data: &[u8]) -> Option<Vec<u8>> {
        match self {
            EncAlg::Aes128Cbc | EncAlg::Aes256Cbc => {
                if data.len() < 32 || data.len() % 16 != 0 {
                    return None;
                }
                let aes = Aes::new(key);
                let mut prev: [u8; 16] = data[..16].try_into().ok()?;
                let mut out = Vec::with_capacity(data.len() - 16);
                for chunk in data[16..].chunks(16) {
                    let c: [u8; 16] = chunk.try_into().ok()?;
                    let mut b = c;
                    aes.decrypt_block(&mut b);
                    for i in 0..16 {
                        b[i] ^= prev[i];
                    }
                    out.extend_from_slice(&b);
                    prev = c;
                }
                let pad = *out.last()? as usize;
                if pad == 0 || pad > 16 || pad > out.len() {
                    return None;
                }
                out.truncate(out.len() - pad);
                Some(out)
            }
            EncAlg::Aes128Gcm | EncAlg::Aes256Gcm => {
                if data.len() < 28 {
                    return None;
                }
                let nonce: [u8; 12] = data[..12].try_into().ok()?;
                AesGcm::new(key).open(&nonce, &[], &data[12..])
            }
        }
    }
}

fn q(prefix: &str, local: &str, uri: &str) -> QName {
    QName::ns(prefix, local, uri)
}

fn child(doc: &Document, parent: NodeId, uri: &str, local: &str) -> Option<NodeId> {
    doc.child_elements_named(parent, uri, local).next()
}

fn descendants(doc: &Document, root: NodeId, uri: &str, local: &str, out: &mut Vec<NodeId>) {
    for c in doc.element_children(root) {
        if doc.namespace_uri(c) == uri && doc.local_name(c) == local {
            out.push(c);
        }
        descendants(doc, c, uri, local, out);
    }
}

/// Move the last child of `parent` to position `at`.
fn move_last_to(doc: &mut Document, parent: NodeId, at: usize) {
    let kids = &mut doc.node_mut(parent).children;
    if let Some(last) = kids.pop() {
        let at = at.min(kids.len());
        kids.insert(at, last);
    }
}

/// The SOAP Header and wsse:Security elements of an envelope (created when missing).
fn security_element(doc: &mut Document, version: SoapVersion) -> Result<NodeId> {
    let env = doc.document_element().ok_or_else(|| CamelError::illegal_argument("Empty envelope"))?;
    let soap_ns = doc.namespace_uri(env).to_string();
    let prefix = doc.name(env).prefix.clone();
    let header = match doc.element_children(env).into_iter().find(|&c| doc.local_name(c) == "Header") {
        Some(h) => h,
        None => {
            let h = doc.append_element(env, q(&prefix, "Header", &soap_ns));
            move_last_to(doc, env, 0);
            h
        }
    };
    if let Some(s) = child(doc, header, WSSE_NS, "Security") {
        return Ok(s);
    }
    let s = doc.append_element(header, q("wsse", "Security", WSSE_NS));
    doc.declare_namespace(s, "wsse", WSSE_NS);
    doc.set_attribute(s, q(&prefix, "mustUnderstand", &soap_ns), if version == SoapVersion::V11 { "1" } else { "true" });
    move_last_to(doc, header, 0);
    Ok(s)
}

/// A standalone serialization of a node (in-scope namespaces declared on it).
fn standalone(doc: &Document, n: NodeId) -> String {
    match doc.node_type(n) {
        NodeType::Element => {
            let mut d = Document::new();
            d.import_node(doc, n, 0);
            d.document_element().map(|r| d.to_xml(r)).unwrap_or_default()
        }
        _ => doc.to_xml(n),
    }
}

/// Encrypt the SOAP body content for the holder of `recipient` (an RSA key).
pub fn encrypt_envelope(envelope: &str, version: SoapVersion, recipient: &PublicKey, cert: Option<&Certificate>, alg: EncAlg) -> Result<String> {
    let PublicKey::Rsa(rsa) = recipient else { return Err(CamelError::illegal_argument("XML Encryption key transport needs an RSA public key")) };
    let mut doc = Document::parse(envelope).map_err(|e| CamelError::illegal_argument(format!("Invalid envelope: {}", e)))?;
    let env = doc.document_element().ok_or_else(|| CamelError::illegal_argument("Empty envelope"))?;
    let body = doc.element_children(env).into_iter().find(|&c| doc.local_name(c) == "Body").ok_or_else(|| CamelError::illegal_argument("Envelope has no Body"))?;
    let plaintext: String = doc.children(body).iter().map(|&c| standalone(&doc, c)).collect();
    let key = camel_tls::crypto::rng::random_bytes(alg.key_len());
    let cipher = alg.encrypt(&key, plaintext.as_bytes());
    let wrapped = rsa.encrypt_oaep(HashAlg::Sha1, &[], &key).ok_or_else(|| CamelError::illegal_argument("RSA key too small for OAEP key transport"))?;
    let data_id = format!("ED-{}", camel_core::message::generate_id().replace('-', ""));
    // body: EncryptedData
    for c in doc.children(body).to_vec() {
        doc.node_mut(c).parent = None;
    }
    doc.node_mut(body).children.clear();
    let ed = doc.append_element(body, q("xenc", "EncryptedData", XENC_NS));
    doc.declare_namespace(ed, "xenc", XENC_NS);
    doc.set_attribute(ed, QName::local("Id"), data_id.clone());
    doc.set_attribute(ed, QName::local("Type"), CONTENT_TYPE);
    let em = doc.append_element(ed, q("xenc", "EncryptionMethod", XENC_NS));
    doc.set_attribute(em, QName::local("Algorithm"), alg.uri());
    let cd = doc.append_element(ed, q("xenc", "CipherData", XENC_NS));
    let cv = doc.append_element(cd, q("xenc", "CipherValue", XENC_NS));
    doc.append_text(cv, &base64_encode(&cipher));
    // header: EncryptedKey (first, so that it is processed before a signature)
    let sec = security_element(&mut doc, version)?;
    let ek = doc.append_element(sec, q("xenc", "EncryptedKey", XENC_NS));
    doc.declare_namespace(ek, "xenc", XENC_NS);
    doc.set_attribute(ek, QName::local("Id"), format!("EK-{}", &data_id[3..]));
    let em = doc.append_element(ek, q("xenc", "EncryptionMethod", XENC_NS));
    doc.set_attribute(em, QName::local("Algorithm"), RSA_OAEP_MGF1P);
    let dm = doc.append_element(em, q("ds", "DigestMethod", DSIG_NS));
    doc.declare_namespace(dm, "ds", DSIG_NS);
    doc.set_attribute(dm, QName::local("Algorithm"), SHA1_URI);
    if let Some(c) = cert {
        let ki = doc.append_element(ek, q("ds", "KeyInfo", DSIG_NS));
        doc.declare_namespace(ki, "ds", DSIG_NS);
        let str_ = doc.append_element(ki, q("wsse", "SecurityTokenReference", WSSE_NS));
        let kid = doc.append_element(str_, q("wsse", "KeyIdentifier", WSSE_NS));
        doc.set_attribute(kid, QName::local("ValueType"), THUMBPRINT);
        doc.set_attribute(kid, QName::local("EncodingType"), BASE64_ENCODING);
        doc.append_text(kid, &base64_encode(&HashAlg::Sha1.hash(&c.der)));
    }
    let cd = doc.append_element(ek, q("xenc", "CipherData", XENC_NS));
    let cv = doc.append_element(cd, q("xenc", "CipherValue", XENC_NS));
    doc.append_text(cv, &base64_encode(&wrapped));
    let rl = doc.append_element(ek, q("xenc", "ReferenceList", XENC_NS));
    let dr = doc.append_element(rl, q("xenc", "DataReference", XENC_NS));
    doc.set_attribute(dr, QName::local("URI"), format!("#{}", data_id));
    move_last_to(&mut doc, sec, 0);
    Ok(doc.to_xml_string())
}

/// Whether an envelope carries encrypted data.
pub fn is_encrypted(envelope: &str) -> bool {
    envelope.contains(XENC_NS) && envelope.contains("EncryptedData")
}

fn decrypt_err(m: impl Into<String>) -> CamelError {
    CamelError::new(crate::envelope::SOAP_FAULT, format!("The signature or decryption was invalid: {}", m.into()))
}

/// Decrypt the encrypted parts of an envelope with the recipient's private key.
pub fn decrypt_envelope(envelope: &str, key: &PrivateKey) -> Result<String> {
    let PrivateKey::Rsa(rsa) = key else { return Err(CamelError::illegal_argument("XML Encryption key transport needs an RSA private key")) };
    let mut doc = Document::parse(envelope).map_err(|e| CamelError::illegal_argument(format!("Invalid envelope: {}", e)))?;
    let root = doc.document_element().ok_or_else(|| decrypt_err("empty envelope"))?;
    let mut keys = Vec::new();
    descendants(&doc, root, XENC_NS, "EncryptedKey", &mut keys);
    if keys.is_empty() {
        return Err(decrypt_err("no xenc:EncryptedKey"));
    }
    for ek in keys {
        let alg = child(&doc, ek, XENC_NS, "EncryptionMethod").and_then(|m| doc.attribute(m, "Algorithm")).unwrap_or(RSA_OAEP_MGF1P).to_string();
        let hash = match alg.as_str() {
            RSA_OAEP_MGF1P => HashAlg::Sha1,
            RSA_OAEP_11 => {
                let d = child(&doc, ek, XENC_NS, "EncryptionMethod").and_then(|m| child(&doc, m, DSIG_NS, "DigestMethod")).and_then(|d| doc.attribute(d, "Algorithm")).unwrap_or(SHA1_URI);
                if d == SHA256_URI {
                    HashAlg::Sha256
                } else {
                    HashAlg::Sha1
                }
            }
            other => return Err(decrypt_err(format!("unsupported key transport {}", other))),
        };
        let cv = child(&doc, ek, XENC_NS, "CipherData").and_then(|c| child(&doc, c, XENC_NS, "CipherValue")).ok_or_else(|| decrypt_err("EncryptedKey without CipherValue"))?;
        let wrapped = base64_decode(doc.string_value(cv).trim()).ok_or_else(|| decrypt_err("invalid base64 in EncryptedKey"))?;
        let sym = rsa.decrypt_oaep(hash, &[], &wrapped).ok_or_else(|| decrypt_err("cannot unwrap the content encryption key"))?;
        let refs: Vec<String> = child(&doc, ek, XENC_NS, "ReferenceList")
            .map(|rl| doc.child_elements_named(rl, XENC_NS, "DataReference").filter_map(|d| doc.attribute(d, "URI").map(|u| u.trim_start_matches('#').to_string())).collect())
            .unwrap_or_default();
        let mut datas = Vec::new();
        descendants(&doc, root, XENC_NS, "EncryptedData", &mut datas);
        let datas: Vec<NodeId> = datas.into_iter().filter(|&d| refs.is_empty() || doc.attribute(d, "Id").is_some_and(|id| refs.iter().any(|r| r == id))).collect();
        for ed in datas {
            let alg_uri = child(&doc, ed, XENC_NS, "EncryptionMethod").and_then(|m| doc.attribute(m, "Algorithm")).unwrap_or("").to_string();
            let alg = EncAlg::parse(&alg_uri).ok_or_else(|| decrypt_err(format!("unsupported data encryption algorithm {}", alg_uri)))?;
            let cv = child(&doc, ed, XENC_NS, "CipherData").and_then(|c| child(&doc, c, XENC_NS, "CipherValue")).ok_or_else(|| decrypt_err("EncryptedData without CipherValue"))?;
            let data = base64_decode(doc.string_value(cv).trim()).ok_or_else(|| decrypt_err("invalid base64 in EncryptedData"))?;
            let plain = alg.decrypt(&sym, &data).ok_or_else(|| decrypt_err("decryption failed"))?;
            let text = String::from_utf8(plain).map_err(|_| decrypt_err("decrypted data is not UTF-8"))?;
            let parent = doc.parent(ed).ok_or_else(|| decrypt_err("detached EncryptedData"))?;
            let element_type = doc.attribute(ed, "Type") == Some(ELEMENT_TYPE);
            // namespaces in scope of the EncryptedData apply to the plaintext
            let decls: String = doc.in_scope_namespaces(parent).iter().filter(|(p, _)| p != "xml").map(|(p, u)| if p.is_empty() { format!(" xmlns=\"{}\"", u) } else { format!(" xmlns:{}=\"{}\"", p, u) }).collect();
            let frag = Document::parse(&format!("<w{}>{}</w>", decls, text)).map_err(|e| decrypt_err(format!("decrypted content is not XML: {}", e)))?;
            let w = frag.document_element().unwrap_or(0);
            let pos = doc.children(parent).iter().position(|&c| c == ed).unwrap_or(0);
            doc.node_mut(parent).children.retain(|&c| c != ed);
            doc.node_mut(ed).parent = None;
            let before = doc.children(parent).len();
            for &c in frag.children(w) {
                match frag.node_type(c) {
                    NodeType::Text => {
                        doc.append_text_node(parent, frag.value(c));
                    }
                    _ => {
                        doc.import_node(&frag, c, parent);
                    }
                }
            }
            let added: Vec<NodeId> = doc.node_mut(parent).children.drain(before..).collect();
            let _ = element_type;
            for (k, id) in added.into_iter().enumerate() {
                doc.node_mut(parent).children.insert(pos + k, id);
            }
        }
        if let Some(p) = doc.parent(ek) {
            doc.node_mut(p).children.retain(|&c| c != ek);
        }
    }
    Ok(doc.to_xml_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::build_envelope;

    #[test]
    fn roundtrip_all_algorithms() {
        let key = PrivateKey::from_pem(include_bytes!("../tests/fixtures/rsa2048.pem")).unwrap();
        let pk = key.public_key();
        let env = build_envelope(SoapVersion::V11, &[], &["<m:x xmlns:m=\"urn:m\">secret <b>value</b></m:x>".into()]);
        for alg in [EncAlg::Aes128Cbc, EncAlg::Aes256Cbc, EncAlg::Aes128Gcm, EncAlg::Aes256Gcm] {
            let enc = encrypt_envelope(&env, SoapVersion::V11, &pk, None, alg).unwrap();
            assert!(!enc.contains("secret"), "{}", enc);
            assert!(is_encrypted(&enc));
            let dec = decrypt_envelope(&enc, &key).unwrap();
            assert!(dec.contains("secret <b>value</b></m:x>"), "{}", dec);
            assert!(!dec.contains("EncryptedKey"));
            let tampered = enc.replacen("<xenc:CipherValue>", "<xenc:CipherValue>AAAA", 2);
            assert!(decrypt_envelope(&tampered, &key).is_err());
        }
    }
}

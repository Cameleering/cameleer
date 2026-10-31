//! Key and trust stores: PEM, PKCS#12 (`.p12` / `.pfx`), Java JKS and JCEKS.
//!
//! [`KeyStore::load`] detects the format from the content (JKS magic `FEEDFEED`,
//! JCEKS magic `CECECECE`, PEM text, otherwise PKCS#12 or a single DER
//! certificate). A store holds private-key entries (key + certificate chain, leaf
//! first) and trusted-certificate entries, like `java.security.KeyStore`.
//!
//! ```
//! use camel_tls::keystore::KeyStore;
//! let id = camel_tls::self_signed("localhost");
//! let p12 = KeyStore::from_key_entry("server", id.key.clone(), vec![id.cert_der.clone()]).to_pkcs12("changeit");
//! let ks = KeyStore::load(&p12, Some("changeit"), None).unwrap();
//! assert_eq!(ks.key_entry(Some("server")).unwrap().1.len(), 1);
//! ```

pub mod jks;
pub mod pkcs12;

use crate::keys::PrivateKey;
use crate::pem;
use crate::x509::Certificate;

/// One key store entry.
#[derive(Clone, Debug)]
pub enum KeyStoreEntry {
    /// A private key with its certificate chain (DER, leaf first).
    Key { alias: String, key: PrivateKey, chain: Vec<Vec<u8>> },
    /// A trusted certificate (DER).
    Cert { alias: String, der: Vec<u8> },
}

impl KeyStoreEntry {
    pub fn alias(&self) -> &str {
        match self {
            KeyStoreEntry::Key { alias, .. } | KeyStoreEntry::Cert { alias, .. } => alias,
        }
    }
}

/// Key store format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyStoreFormat {
    Pem,
    Pkcs12,
    Jks,
    Jceks,
    Der,
}

impl KeyStoreFormat {
    /// Detects the format of key store bytes.
    pub fn detect(data: &[u8]) -> KeyStoreFormat {
        if data.starts_with(&jks::JKS_MAGIC.to_be_bytes()) {
            KeyStoreFormat::Jks
        } else if data.starts_with(&jks::JCEKS_MAGIC.to_be_bytes()) {
            KeyStoreFormat::Jceks
        } else if data.windows(10).any(|w| w == b"-----BEGIN") {
            KeyStoreFormat::Pem
        } else if Certificate::from_der(data).is_ok() {
            KeyStoreFormat::Der
        } else {
            KeyStoreFormat::Pkcs12
        }
    }

    /// Parses a `keyStoreType` / `trustStoreType` name (`PKCS12`, `JKS`, `JCEKS`, `PEM`).
    pub fn from_name(name: &str) -> Option<KeyStoreFormat> {
        match name.trim().to_ascii_uppercase().as_str() {
            "PKCS12" | "P12" | "PFX" => Some(KeyStoreFormat::Pkcs12),
            "JKS" => Some(KeyStoreFormat::Jks),
            "JCEKS" => Some(KeyStoreFormat::Jceks),
            "PEM" => Some(KeyStoreFormat::Pem),
            "DER" | "CER" | "CRT" => Some(KeyStoreFormat::Der),
            _ => None,
        }
    }
}

/// An in-memory key store.
#[derive(Clone, Debug, Default)]
pub struct KeyStore {
    pub entries: Vec<KeyStoreEntry>,
}

impl KeyStore {
    pub fn new() -> KeyStore {
        KeyStore::default()
    }

    /// A store with one key entry.
    pub fn from_key_entry(alias: &str, key: PrivateKey, chain: Vec<Vec<u8>>) -> KeyStore {
        KeyStore { entries: vec![KeyStoreEntry::Key { alias: alias.to_string(), key, chain }] }
    }

    pub fn add_key(mut self, alias: &str, key: PrivateKey, chain: Vec<Vec<u8>>) -> KeyStore {
        self.entries.push(KeyStoreEntry::Key { alias: alias.to_string(), key, chain });
        self
    }

    pub fn add_cert(mut self, alias: &str, der: Vec<u8>) -> KeyStore {
        self.entries.push(KeyStoreEntry::Cert { alias: alias.to_string(), der });
        self
    }

    /// Loads a store of any supported format. `store_password` protects the store
    /// (integrity / PKCS#12 encryption); `key_password` the private keys (defaults to
    /// the store password).
    pub fn load(data: &[u8], store_password: Option<&str>, key_password: Option<&str>) -> Result<KeyStore, String> {
        KeyStore::load_as(data, KeyStoreFormat::detect(data), store_password, key_password)
    }

    /// Loads a store of the given format.
    pub fn load_as(data: &[u8], format: KeyStoreFormat, store_password: Option<&str>, key_password: Option<&str>) -> Result<KeyStore, String> {
        let key_password = key_password.or(store_password);
        match format {
            KeyStoreFormat::Pem => KeyStore::from_pem(data, key_password),
            KeyStoreFormat::Der => {
                Certificate::from_der(data).map_err(|e| e.to_string())?;
                Ok(KeyStore::new().add_cert("cert", data.to_vec()))
            }
            KeyStoreFormat::Pkcs12 => pkcs12::parse(data, store_password.unwrap_or(""), key_password),
            KeyStoreFormat::Jks | KeyStoreFormat::Jceks => jks::parse(data, store_password, key_password),
        }
    }

    /// Loads a store from a file.
    pub fn load_file(path: impl AsRef<std::path::Path>, store_password: Option<&str>, key_password: Option<&str>) -> Result<KeyStore, String> {
        let data = std::fs::read(path.as_ref()).map_err(|e| format!("{}: {e}", path.as_ref().display()))?;
        KeyStore::load(&data, store_password, key_password).map_err(|e| format!("{}: {e}", path.as_ref().display()))
    }

    /// PEM: every CERTIFICATE becomes a trusted entry; a private key (plain or
    /// `ENCRYPTED PRIVATE KEY`) becomes a key entry with all certificates as its chain.
    pub fn from_pem(data: &[u8], key_password: Option<&str>) -> Result<KeyStore, String> {
        let blocks = pem::parse_pem(data)?;
        let certs: Vec<Vec<u8>> = blocks.iter().filter(|b| b.label == "CERTIFICATE" || b.label == "TRUSTED CERTIFICATE").map(|b| b.der.clone()).collect();
        let mut ks = KeyStore::new();
        if blocks.iter().any(|b| b.label.ends_with("PRIVATE KEY")) {
            let key = PrivateKey::from_pem_with_password(data, key_password)?;
            let chain = order_chain(&key, &certs);
            ks = ks.add_key("key", key, chain);
        }
        for (i, c) in certs.into_iter().enumerate() {
            ks = ks.add_cert(&format!("cert{i}"), c);
        }
        Ok(ks)
    }

    /// The key entry `alias` (case-insensitive), or the first key entry.
    pub fn key_entry(&self, alias: Option<&str>) -> Option<(&PrivateKey, &[Vec<u8>])> {
        self.entries.iter().find_map(|e| match e {
            KeyStoreEntry::Key { alias: a, key, chain } if alias.is_none_or(|x| x.eq_ignore_ascii_case(a)) => Some((key, chain.as_slice())),
            _ => None,
        })
    }

    /// Trusted-certificate entries (DER).
    pub fn trusted_certs(&self) -> Vec<Vec<u8>> {
        self.entries
            .iter()
            .filter_map(|e| match e {
                KeyStoreEntry::Cert { der, .. } => Some(der.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn aliases(&self) -> Vec<&str> {
        self.entries.iter().map(|e| e.alias()).collect()
    }

    /// Serialises as PKCS#12 (PBES2/AES-256-CBC, HMAC-SHA256 MAC - the OpenSSL 3 default).
    pub fn to_pkcs12(&self, password: &str) -> Vec<u8> {
        pkcs12::write(self, password, pkcs12::Pkcs12Profile::Modern)
    }

    /// Serialises as a Java JKS key store.
    pub fn to_jks(&self, store_password: &str, key_password: &str) -> Vec<u8> {
        jks::write(self, store_password, key_password)
    }
}

/// Orders `certs` into a chain starting with the certificate matching `key`
/// (followed by its issuers, as far as present). Falls back to the given order.
pub fn order_chain(key: &PrivateKey, certs: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let parsed: Vec<Option<Certificate>> = certs.iter().map(|d| Certificate::from_der(d).ok()).collect();
    let pk = key.public_key();
    let leaf = parsed.iter().position(|c| c.as_ref().and_then(|c| c.public_key.as_ref()).is_some_and(|k| k.same_as(&pk)));
    let Some(mut idx) = leaf else { return certs.to_vec() };
    let mut out = vec![certs[idx].clone()];
    let mut used = vec![false; certs.len()];
    used[idx] = true;
    while let Some(cur) = &parsed[idx] {
        if cur.is_self_issued() {
            break;
        }
        match parsed.iter().enumerate().position(|(j, c)| !used[j] && c.as_ref().is_some_and(|c| c.subject_raw == cur.issuer_raw)) {
            Some(j) => {
                used[j] = true;
                out.push(certs[j].clone());
                idx = j;
            }
            None => break,
        }
    }
    out
}

//! Client and server configuration builders.

use super::{CipherSuite, NamedGroup, TlsError, TlsResult, TlsVersion};
use crate::keys::{PrivateKey, PublicKey};
use crate::pem;
use crate::x509::{Certificate, RootStore};
use std::path::Path;

fn default_suites() -> Vec<CipherSuite> {
    CipherSuite::ALL.to_vec()
}

fn default_groups() -> Vec<NamedGroup> {
    NamedGroup::ALL.to_vec()
}

/// Client-side configuration. Build with [`TlsClientConfig::builder`].
#[derive(Clone, Debug)]
pub struct TlsClientConfig {
    pub(crate) roots: RootStore,
    pub(crate) verify: bool,
    pub(crate) verify_hostname: bool,
    pub(crate) alpn: Vec<Vec<u8>>,
    pub(crate) min_version: TlsVersion,
    pub(crate) max_version: TlsVersion,
    pub(crate) suites: Vec<CipherSuite>,
    /// Preference order; a key share is sent for the first one (HelloRetryRequest
    /// handles servers that want another).
    pub(crate) groups: Vec<NamedGroup>,
}

impl Default for TlsClientConfig {
    fn default() -> Self {
        TlsClientConfig::builder().build()
    }
}

impl TlsClientConfig {
    /// Starts a builder that trusts the system CA bundle.
    pub fn builder() -> TlsClientConfigBuilder {
        TlsClientConfigBuilder {
            cfg: TlsClientConfig {
                roots: RootStore::empty(),
                verify: true,
                verify_hostname: true,
                alpn: Vec::new(),
                min_version: TlsVersion::Tls12,
                max_version: TlsVersion::Tls13,
                suites: default_suites(),
                groups: default_groups(),
            },
            system_roots: true,
        }
    }

    pub fn roots(&self) -> &RootStore {
        &self.roots
    }
}

/// Builder for [`TlsClientConfig`].
pub struct TlsClientConfigBuilder {
    cfg: TlsClientConfig,
    system_roots: bool,
}

impl TlsClientConfigBuilder {
    /// Whether to include the system trust store (default true).
    pub fn system_roots(mut self, yes: bool) -> Self {
        self.system_roots = yes;
        self
    }

    /// Replaces the custom roots with `store` (system roots are still added unless disabled).
    pub fn root_store(mut self, store: RootStore) -> Self {
        self.cfg.roots = store;
        self
    }

    /// Adds CA certificates from PEM text.
    pub fn add_root_pem(mut self, pem: &[u8]) -> TlsResult<Self> {
        self.cfg.roots.add_pem(pem).map_err(TlsError::Config)?;
        Ok(self)
    }

    /// Adds CA certificates from a PEM file.
    pub fn add_root_pem_file(mut self, path: impl AsRef<Path>) -> TlsResult<Self> {
        self.cfg.roots.add_pem_file(path).map_err(TlsError::Config)?;
        Ok(self)
    }

    /// Adds a DER CA certificate.
    pub fn add_root_der(mut self, der: &[u8]) -> TlsResult<Self> {
        self.cfg.roots.add_der(der).map_err(TlsError::Config)?;
        Ok(self)
    }

    /// DANGEROUS: disables certificate chain and hostname verification (the
    /// handshake signature is still checked against the presented key). For tests only.
    pub fn insecure_skip_verify(mut self, skip: bool) -> Self {
        self.cfg.verify = !skip;
        self
    }

    /// Whether the certificate must match the server name (default true). With
    /// `false` the chain is still verified against the trust store.
    pub fn verify_hostname(mut self, yes: bool) -> Self {
        self.cfg.verify_hostname = yes;
        self
    }

    /// ALPN protocols to offer, e.g. `["h2", "http/1.1"]`.
    pub fn alpn_protocols(mut self, protos: &[&str]) -> Self {
        self.cfg.alpn = protos.iter().map(|p| p.as_bytes().to_vec()).collect();
        self
    }

    pub fn versions(mut self, min: TlsVersion, max: TlsVersion) -> Self {
        self.cfg.min_version = min;
        self.cfg.max_version = max.max(min);
        self
    }

    /// Cipher suites in preference order.
    pub fn cipher_suites(mut self, suites: &[CipherSuite]) -> Self {
        self.cfg.suites = suites.to_vec();
        self
    }

    /// Key exchange groups in preference order (the first gets a key share).
    pub fn groups(mut self, groups: &[NamedGroup]) -> Self {
        self.cfg.groups = groups.to_vec();
        self
    }

    pub fn build(mut self) -> TlsClientConfig {
        if self.system_roots {
            let mut store = RootStore::system();
            store.extend(&self.cfg.roots);
            self.cfg.roots = store;
        }
        self.cfg
    }
}

/// Server-side configuration. Build with [`TlsServerConfig::builder`].
#[derive(Clone, Debug)]
pub struct TlsServerConfig {
    pub(crate) cert_chain: Vec<Vec<u8>>,
    pub(crate) key: PrivateKey,
    pub(crate) alpn: Vec<Vec<u8>>,
    pub(crate) min_version: TlsVersion,
    pub(crate) max_version: TlsVersion,
    pub(crate) suites: Vec<CipherSuite>,
    pub(crate) groups: Vec<NamedGroup>,
}

impl TlsServerConfig {
    pub fn builder() -> TlsServerConfigBuilder {
        TlsServerConfigBuilder {
            chain: Vec::new(),
            key: None,
            alpn: Vec::new(),
            min_version: TlsVersion::Tls12,
            max_version: TlsVersion::Tls13,
            suites: default_suites(),
            groups: default_groups(),
            error: None,
        }
    }

    /// Shortcut: certificate chain PEM + private key PEM.
    pub fn from_pem(cert_chain_pem: &[u8], key_pem: &[u8]) -> TlsResult<TlsServerConfig> {
        TlsServerConfig::builder().cert_chain_pem(cert_chain_pem).private_key_pem(key_pem).build()
    }

    /// Shortcut: PEM files.
    pub fn from_pem_files(cert_chain: impl AsRef<Path>, key: impl AsRef<Path>) -> TlsResult<TlsServerConfig> {
        TlsServerConfig::builder().cert_chain_pem_file(cert_chain).private_key_pem_file(key).build()
    }

    pub fn cert_chain(&self) -> &[Vec<u8>] {
        &self.cert_chain
    }
}

/// Builder for [`TlsServerConfig`]; errors are reported by `build()`.
pub struct TlsServerConfigBuilder {
    chain: Vec<Vec<u8>>,
    key: Option<PrivateKey>,
    alpn: Vec<Vec<u8>>,
    min_version: TlsVersion,
    max_version: TlsVersion,
    suites: Vec<CipherSuite>,
    groups: Vec<NamedGroup>,
    error: Option<String>,
}

impl TlsServerConfigBuilder {
    fn fail(mut self, e: String) -> Self {
        self.error.get_or_insert(e);
        self
    }

    /// Certificate chain (leaf first) from PEM text.
    pub fn cert_chain_pem(mut self, data: &[u8]) -> Self {
        match pem::parse_pem(data) {
            Ok(blocks) => {
                self.chain = blocks.into_iter().filter(|b| b.label == "CERTIFICATE").map(|b| b.der).collect();
                if self.chain.is_empty() {
                    return self.fail("no CERTIFICATE blocks in certificate PEM".into());
                }
                self
            }
            Err(e) => self.fail(e),
        }
    }

    pub fn cert_chain_pem_file(self, path: impl AsRef<Path>) -> Self {
        match std::fs::read(path.as_ref()) {
            Ok(d) => self.cert_chain_pem(&d),
            Err(e) => {
                let msg = format!("{}: {e}", path.as_ref().display());
                self.fail(msg)
            }
        }
    }

    /// Certificate chain (leaf first) as DER.
    pub fn cert_chain_der(mut self, chain: Vec<Vec<u8>>) -> Self {
        self.chain = chain;
        self
    }

    /// Private key PEM (PKCS#8, SEC1 `EC PRIVATE KEY` or PKCS#1 `RSA PRIVATE KEY`).
    pub fn private_key_pem(mut self, data: &[u8]) -> Self {
        match PrivateKey::from_pem(data) {
            Ok(k) => {
                self.key = Some(k);
                self
            }
            Err(e) => self.fail(e),
        }
    }

    pub fn private_key_pem_file(self, path: impl AsRef<Path>) -> Self {
        match std::fs::read(path.as_ref()) {
            Ok(d) => self.private_key_pem(&d),
            Err(e) => {
                let msg = format!("{}: {e}", path.as_ref().display());
                self.fail(msg)
            }
        }
    }

    /// Private key DER (any supported format).
    pub fn private_key_der(mut self, der: &[u8]) -> Self {
        match PrivateKey::from_der(der) {
            Ok(k) => {
                self.key = Some(k);
                self
            }
            Err(e) => self.fail(e),
        }
    }

    pub fn private_key(mut self, key: PrivateKey) -> Self {
        self.key = Some(key);
        self
    }

    pub fn alpn_protocols(mut self, protos: &[&str]) -> Self {
        self.alpn = protos.iter().map(|p| p.as_bytes().to_vec()).collect();
        self
    }

    pub fn versions(mut self, min: TlsVersion, max: TlsVersion) -> Self {
        self.min_version = min;
        self.max_version = max.max(min);
        self
    }

    pub fn cipher_suites(mut self, suites: &[CipherSuite]) -> Self {
        self.suites = suites.to_vec();
        self
    }

    pub fn groups(mut self, groups: &[NamedGroup]) -> Self {
        self.groups = groups.to_vec();
        self
    }

    pub fn build(self) -> TlsResult<TlsServerConfig> {
        if let Some(e) = self.error {
            return Err(TlsError::Config(e));
        }
        if self.chain.is_empty() {
            return Err(TlsError::Config("no certificate chain configured".into()));
        }
        let key = self.key.ok_or_else(|| TlsError::Config("no private key configured".into()))?;
        let leaf = Certificate::from_der(&self.chain[0]).map_err(|e| TlsError::Config(format!("leaf certificate: {e}")))?;
        let matches = match (&leaf.public_key, key.public_key()) {
            (Some(PublicKey::Ec { point: a, .. }), PublicKey::Ec { point: b, .. }) => *a == b,
            (Some(PublicKey::Rsa(_)), PublicKey::Rsa(_)) => PublicKey::from_spki(&leaf.spki).is_ok() && rsa_matches(&leaf, &key),
            _ => false,
        };
        if !matches {
            return Err(TlsError::Config("private key does not match the leaf certificate".into()));
        }
        if self.suites.is_empty() || self.groups.is_empty() {
            return Err(TlsError::Config("empty cipher suite or group list".into()));
        }
        Ok(TlsServerConfig {
            cert_chain: self.chain,
            key,
            alpn: self.alpn,
            min_version: self.min_version,
            max_version: self.max_version,
            suites: self.suites,
            groups: self.groups,
        })
    }
}

fn rsa_matches(leaf: &Certificate, key: &PrivateKey) -> bool {
    // sign a probe and verify with the certificate key
    let h = crate::crypto::hash::HashAlg::Sha256;
    let digest = h.hash(b"camel-tls key check");
    match (key, &leaf.public_key) {
        (PrivateKey::Rsa(k), Some(PublicKey::Rsa(p))) => match k.sign_pkcs1(h, &digest) {
            Some(sig) => p.verify_pkcs1(h, &digest, &sig),
            None => false,
        },
        _ => false,
    }
}

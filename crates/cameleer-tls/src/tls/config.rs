//! Client and server configuration builders.

use super::session::{ClientSessionCache, ServerSessions};
use super::{CipherSuite, NamedGroup, TlsError, TlsResult, TlsVersion};
use crate::keys::PrivateKey;
use crate::pem;
use crate::x509::{Certificate, Crl, RootStore};
use std::path::Path;
use std::sync::Arc;

/// Whether a server requests / requires client certificates (Camel
/// `clientAuthentication=NONE|WANT|REQUIRE`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClientAuth {
    #[default]
    None,
    /// Request a certificate; connections without one are accepted.
    Want,
    /// Require a certificate (handshake fails without a valid one).
    Need,
}

impl ClientAuth {
    /// Parses `NONE` / `WANT` / `REQUIRE` (also `NEED`, `true`/`false`, `optional`).
    pub fn parse(s: &str) -> Option<ClientAuth> {
        match s.trim().to_ascii_uppercase().as_str() {
            "NONE" | "FALSE" | "NO" | "OFF" | "" => Some(ClientAuth::None),
            "WANT" | "OPTIONAL" | "REQUEST" | "REQUESTED" => Some(ClientAuth::Want),
            "REQUIRE" | "REQUIRED" | "NEED" | "TRUE" | "YES" | "ON" => Some(ClientAuth::Need),
            _ => None,
        }
    }
}

/// A certificate chain (DER, leaf first) with its private key.
#[derive(Clone, Debug)]
pub struct CertifiedKey {
    pub chain: Vec<Vec<u8>>,
    pub key: PrivateKey,
}

impl CertifiedKey {
    /// Checks that the key matches the leaf certificate.
    pub fn new(chain: Vec<Vec<u8>>, key: PrivateKey) -> TlsResult<CertifiedKey> {
        if chain.is_empty() {
            return Err(TlsError::Config("no certificate chain configured".into()));
        }
        let leaf = Certificate::from_der(&chain[0]).map_err(|e| TlsError::Config(format!("leaf certificate: {e}")))?;
        if !leaf.public_key.as_ref().is_some_and(|p| p.same_as(&key.public_key())) {
            return Err(TlsError::Config("private key does not match the leaf certificate".into()));
        }
        Ok(CertifiedKey { chain, key })
    }

    /// From PEM certificate chain and PEM private key (optionally encrypted).
    pub fn from_pem(chain_pem: &[u8], key_pem: &[u8], key_password: Option<&str>) -> TlsResult<CertifiedKey> {
        let chain: Vec<Vec<u8>> =
            pem::parse_pem(chain_pem).map_err(TlsError::Config)?.into_iter().filter(|b| b.label == "CERTIFICATE").map(|b| b.der).collect();
        let key = PrivateKey::from_pem_with_password(key_pem, key_password).map_err(TlsError::Config)?;
        CertifiedKey::new(chain, key)
    }
}

/// Default session lifetime (seconds), like Java's `SSLSessionContext` default.
pub const DEFAULT_SESSION_LIFETIME: u32 = 86_400;

fn default_suites() -> Vec<CipherSuite> {
    CipherSuite::ALL.to_vec()
}

fn default_groups() -> Vec<NamedGroup> {
    NamedGroup::ALL.to_vec()
}

/// OCSP stapling on the client (`status_request`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OcspMode {
    /// Do not request a stapled response (default).
    #[default]
    Off,
    /// Request one and validate it when the server staples (revoked / invalid fails).
    Request,
    /// Request one and fail the handshake without a valid "good" response.
    Require,
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
    /// Client certificate for mutual TLS.
    pub(crate) client_cert: Option<Arc<CertifiedKey>>,
    pub(crate) crls: Arc<Vec<Crl>>,
    /// Session cache for resumption (None disables resumption).
    pub(crate) session_cache: Option<Arc<ClientSessionCache>>,
    /// OCSP stapling.
    pub(crate) ocsp: OcspMode,
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
                client_cert: None,
                crls: Arc::new(Vec::new()),
                session_cache: Some(Arc::new(ClientSessionCache::default())),
                ocsp: OcspMode::Off,
            },
            system_roots: true,
        }
    }

    pub fn roots(&self) -> &RootStore {
        &self.roots
    }

    /// The session cache (None when resumption is disabled).
    pub fn session_cache(&self) -> Option<&Arc<ClientSessionCache>> {
        self.session_cache.as_ref()
    }

    /// Whether a client certificate is configured.
    pub fn has_client_certificate(&self) -> bool {
        self.client_cert.is_some()
    }

    /// A copy of this configuration offering the given ALPN protocols.
    pub fn with_alpn(&self, protos: &[&str]) -> TlsClientConfig {
        let mut c = self.clone();
        c.alpn = protos.iter().map(|p| p.as_bytes().to_vec()).collect();
        c
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

    /// Client certificate (mutual TLS): sent when the server requests one.
    pub fn client_certificate(mut self, ck: CertifiedKey) -> Self {
        self.cfg.client_cert = Some(Arc::new(ck));
        self
    }

    /// Client certificate from PEM chain + PEM key.
    pub fn client_cert_pem(self, chain_pem: &[u8], key_pem: &[u8]) -> TlsResult<Self> {
        Ok(self.client_certificate(CertifiedKey::from_pem(chain_pem, key_pem, None)?))
    }

    /// Certificate revocation lists checked during chain validation.
    pub fn crls(mut self, crls: Vec<Crl>) -> Self {
        self.cfg.crls = Arc::new(crls);
        self
    }

    /// Enables (default) or disables TLS session resumption (TLS 1.3 PSK tickets,
    /// TLS 1.2 session tickets and IDs) with a fresh cache.
    pub fn session_resumption(mut self, yes: bool) -> Self {
        self.cfg.session_cache = if yes { Some(Arc::new(ClientSessionCache::default())) } else { None };
        self
    }

    /// Uses `cache` (e.g. shared between configurations) for resumption.
    pub fn session_cache(mut self, cache: Arc<ClientSessionCache>) -> Self {
        self.cfg.session_cache = Some(cache);
        self
    }

    /// OCSP stapling: request (and validate) the server's stapled OCSP response.
    pub fn ocsp_stapling(mut self, mode: OcspMode) -> Self {
        self.cfg.ocsp = mode;
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
    pub(crate) client_auth: ClientAuth,
    pub(crate) client_roots: RootStore,
    pub(crate) crls: Arc<Vec<Crl>>,
    /// Resumption state (None disables resumption).
    pub(crate) sessions: Option<Arc<ServerSessions>>,
    /// DER OCSP response stapled when the client sends `status_request`.
    pub(crate) ocsp_response: Option<Arc<Vec<u8>>>,
    /// TLS 1.3 0-RTT: max_early_data_size advertised in tickets (0 = disabled).
    pub(crate) max_early_data: u32,
    /// 0-RTT anti-replay: accepted ticket age skew, and the window of the binder cache.
    pub(crate) early_data_window_ms: u64,
    /// 0-RTT anti-replay: PSK binders of accepted early data (until expiry, ms).
    pub(crate) early_replay: Arc<std::sync::Mutex<std::collections::HashMap<Vec<u8>, u64>>>,
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
            client_auth: ClientAuth::None,
            client_roots: RootStore::empty(),
            crls: Vec::new(),
            resumption: true,
            tickets: true,
            lifetime: DEFAULT_SESSION_LIFETIME,
            ocsp_response: None,
            max_early_data: 0,
            early_data_window_ms: 10_000,
            error: None,
        }
    }

    /// Replaces the stapled OCSP response (e.g. after refreshing it from the responder).
    pub fn set_ocsp_response(&mut self, der: Option<Vec<u8>>) {
        self.ocsp_response = der.map(Arc::new);
    }

    pub fn client_auth(&self) -> ClientAuth {
        self.client_auth
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

/// What another TLS engine needs to serve the same configuration as a [`TlsServerConfig`]
/// (used by the HTTP server, which runs on simoom's TLS).
#[derive(Clone, Debug)]
pub struct ServerExport {
    /// The certificate chain, DER, leaf first.
    pub chain: Vec<Vec<u8>>,
    /// The private key, PKCS#8 DER.
    pub key_pkcs8: Vec<u8>,
    /// ALPN protocols in preference order.
    pub alpn: Vec<Vec<u8>>,
    pub min_version: TlsVersion,
    pub max_version: TlsVersion,
    pub client_auth: ClientAuth,
    /// The trust anchors that client certificates are verified against, DER.
    pub client_roots: Vec<Vec<u8>>,
    /// Whether session resumption is on.
    pub resumption: bool,
}

impl TlsServerConfig {
    /// The certificate, key and options, for an engine other than this crate's.
    pub fn export(&self) -> ServerExport {
        ServerExport {
            chain: self.cert_chain.clone(),
            key_pkcs8: self.key.to_pkcs8(),
            alpn: self.alpn.clone(),
            min_version: self.min_version,
            max_version: self.max_version,
            client_auth: self.client_auth,
            client_roots: self.client_roots.certificates().map(|c| c.der.clone()).collect(),
            resumption: self.sessions.is_some(),
        }
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
    client_auth: ClientAuth,
    client_roots: RootStore,
    crls: Vec<Crl>,
    resumption: bool,
    tickets: bool,
    lifetime: u32,
    ocsp_response: Option<Vec<u8>>,
    max_early_data: u32,
    early_data_window_ms: u64,
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

    /// Certificate chain and key in one.
    pub fn certified_key(mut self, ck: CertifiedKey) -> Self {
        self.chain = ck.chain;
        self.key = Some(ck.key);
        self
    }

    /// Client certificate authentication mode (default [`ClientAuth::None`]).
    pub fn client_auth(mut self, mode: ClientAuth) -> Self {
        self.client_auth = mode;
        self
    }

    /// Trust anchors for client certificates (also advertised as certificate_authorities).
    pub fn client_roots(mut self, roots: RootStore) -> Self {
        self.client_roots = roots;
        self
    }

    /// Adds client-certificate trust anchors from PEM text.
    pub fn add_client_root_pem(mut self, pem: &[u8]) -> Self {
        if let Err(e) = self.client_roots.add_pem(pem) {
            return self.fail(format!("client trust store: {e}"));
        }
        self
    }

    /// Certificate revocation lists checked when validating client certificates.
    pub fn crls(mut self, crls: Vec<Crl>) -> Self {
        self.crls = crls;
        self
    }

    /// Enables (default) or disables session resumption.
    pub fn session_resumption(mut self, yes: bool) -> Self {
        self.resumption = yes;
        self
    }

    /// Whether to issue stateless (encrypted) session tickets (default true); with
    /// `false` sessions are kept in a server-side cache: TLS 1.2 uses session IDs and
    /// TLS 1.3 tickets carry only a random cache identity.
    pub fn session_tickets(mut self, yes: bool) -> Self {
        self.tickets = yes;
        self
    }

    /// A DER OCSP response for the leaf certificate, stapled (TLS 1.3 certificate entry
    /// extension / TLS 1.2 CertificateStatus) when clients send `status_request`.
    pub fn ocsp_response(mut self, der: Vec<u8>) -> Self {
        self.ocsp_response = Some(der);
        self
    }

    /// Opt-in TLS 1.3 0-RTT: tickets allow up to `max_bytes` of early data, which resumed
    /// clients may send with their ClientHello (it is replayable: accept only idempotent
    /// requests). Early data is accepted only for the first PSK identity, without
    /// HelloRetryRequest, with a ticket age within the anti-replay window and a PSK binder
    /// not seen before in that window; it is delivered by the first reads.
    pub fn early_data(mut self, max_bytes: u32) -> Self {
        self.max_early_data = max_bytes;
        self
    }

    /// 0-RTT anti-replay window (ticket age tolerance and binder cache lifetime), default 10s.
    pub fn early_data_window(mut self, window: std::time::Duration) -> Self {
        self.early_data_window_ms = window.as_millis().max(1) as u64;
        self
    }

    /// Session / ticket lifetime in seconds (default 86400, at most 7 days).
    pub fn session_lifetime_secs(mut self, secs: u32) -> Self {
        self.lifetime = secs;
        self
    }

    pub fn build(self) -> TlsResult<TlsServerConfig> {
        if let Some(e) = self.error {
            return Err(TlsError::Config(e));
        }
        let key = self.key.ok_or_else(|| TlsError::Config("no private key configured".into()))?;
        let ck = CertifiedKey::new(self.chain, key)?;
        if self.suites.is_empty() || self.groups.is_empty() {
            return Err(TlsError::Config("empty cipher suite or group list".into()));
        }
        if self.client_auth != ClientAuth::None && self.client_roots.is_empty() {
            return Err(TlsError::Config("client authentication requires a trust store for client certificates".into()));
        }
        Ok(TlsServerConfig {
            cert_chain: ck.chain,
            key: ck.key,
            alpn: self.alpn,
            min_version: self.min_version,
            max_version: self.max_version,
            suites: self.suites,
            groups: self.groups,
            client_auth: self.client_auth,
            client_roots: self.client_roots,
            crls: Arc::new(self.crls),
            sessions: if self.resumption { Some(Arc::new(ServerSessions::new(self.lifetime, self.tickets))) } else { None },
            ocsp_response: self.ocsp_response.map(Arc::new),
            max_early_data: self.max_early_data,
            early_data_window_ms: self.early_data_window_ms,
            early_replay: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        })
    }
}

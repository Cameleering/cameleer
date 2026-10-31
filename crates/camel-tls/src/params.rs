//! [`SslContextParameters`]: the rust-camel equivalent of Camel's
//! `SSLContextParameters` - one description of trust material, key material and
//! protocol options from which client and server TLS configurations are built.
//!
//! Key and trust stores may be PEM files, PKCS#12 (`.p12`/`.pfx`), Java JKS or JCEKS
//! (format detected from the content or set with `keyStoreType`/`trustStoreType`):
//! a "key store" holds the certificate chain (leaf first) and the private key (or a
//! PEM certificate chain plus a separate PEM key file, optionally an `ENCRYPTED PRIVATE
//! KEY`); a "trust store" holds CA certificates (all certificates of a PEM file, the
//! trusted-certificate entries of a PKCS#12/JKS store).
//!
//! Mutual TLS: a client with key material presents it when the server asks;
//! a server requests client certificates with `client_authentication`
//! (`NONE`/`WANT`/`REQUIRE`) and validates them against its trust store (and CRLs).
//!
//! ```
//! use camel_tls::SslContextParameters;
//! let id = camel_tls::self_signed("localhost");
//! let server = SslContextParameters::new().key_pair_pem(&id.cert_pem, &id.key_pem);
//! let client = SslContextParameters::new().trust_pem(&id.cert_pem);
//! let _scfg = server.server_config().unwrap();
//! let _ccfg = client.client_config().unwrap();
//! ```

use crate::keystore::{KeyStore, KeyStoreFormat};
use crate::pem;
use crate::tls::config::{CertifiedKey, ClientAuth, TlsClientConfig, TlsServerConfig};
use crate::tls::{TlsError, TlsVersion};
use crate::x509::{Crl, RootStore};
use std::path::PathBuf;

/// Source of key/trust material: a file path (any supported format) or inline PEM text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PemSource {
    File(PathBuf),
    Inline(String),
}

impl PemSource {
    pub fn load(&self) -> Result<Vec<u8>, TlsError> {
        match self {
            PemSource::File(p) => std::fs::read(p).map_err(|e| TlsError::Config(format!("{}: {e}", p.display()))),
            PemSource::Inline(s) => Ok(s.as_bytes().to_vec()),
        }
    }

    /// `file:` prefixes are accepted; text containing `-----BEGIN` is inline PEM.
    pub fn parse(s: &str) -> PemSource {
        let s = s.trim();
        if s.contains("-----BEGIN") {
            PemSource::Inline(s.to_string())
        } else {
            PemSource::File(PathBuf::from(s.strip_prefix("file:").unwrap_or(s)))
        }
    }
}

/// Trust, key and protocol settings shared by TLS clients and servers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SslContextParameters {
    /// CA certificates to trust (PEM).
    pub trust_store: Vec<PemSource>,
    /// Whether to also trust the system CA bundle. `None` = only when no
    /// `trust_store` is configured (Camel semantics: configured trust managers replace the default).
    pub use_system_trust_store: Option<bool>,
    /// Certificate chain (leaf first); may also contain the private key.
    pub cert_chain: Option<PemSource>,
    /// Private key (PKCS#8, SEC1 or PKCS#1 PEM). If absent it is read from `cert_chain`.
    pub private_key: Option<PemSource>,
    /// DANGEROUS: skip certificate chain and hostname verification (client side).
    pub insecure_skip_verify: bool,
    /// Client side: whether the server certificate must match the host name
    /// (`None` = true). The chain is verified either way unless `insecure_skip_verify`.
    pub hostname_verification: Option<bool>,
    /// ALPN protocols to offer (client) / accept (server).
    pub alpn: Vec<String>,
    pub min_version: Option<TlsVersion>,
    pub max_version: Option<TlsVersion>,
    /// Overrides the SNI / hostname used for verification on client connections.
    pub server_name: Option<String>,
    /// Key store password (`keystorePassword`): store integrity / PKCS#12 encryption.
    pub key_store_password: Option<String>,
    /// Private key password (`keyPassword`, default: the key store password).
    pub key_password: Option<String>,
    /// `PKCS12`, `JKS`, `JCEKS` or `PEM` (default: detected).
    pub key_store_type: Option<String>,
    /// Alias of the key entry to use (`certAlias`, default: the first key entry).
    pub cert_alias: Option<String>,
    pub trust_store_password: Option<String>,
    pub trust_store_type: Option<String>,
    /// Server side: client certificate authentication (`clientAuthentication`).
    pub client_authentication: Option<ClientAuth>,
    /// CRL files (PEM `X509 CRL` or DER) checked during chain validation.
    pub crl_files: Vec<PemSource>,
    /// Session resumption (default true).
    pub session_resumption: Option<bool>,
    /// Session lifetime in seconds (`sessionTimeout`, default 86400).
    pub session_timeout: Option<u32>,
}

/// Parses `TLSv1.2` / `TLSv1.3` / `1.2` / `TLS` (any) style protocol names.
pub fn parse_tls_version(s: &str) -> Option<TlsVersion> {
    let v = s.trim().to_ascii_lowercase();
    let v = v.trim_start_matches("tlsv").trim_start_matches("tls");
    match v.trim_start_matches(['v', '_', '-']) {
        "1.2" | "12" | "1_2" => Some(TlsVersion::Tls12),
        "1.3" | "13" | "1_3" => Some(TlsVersion::Tls13),
        _ => None,
    }
}

fn truthy(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "yes" | "on" | "1")
}

impl SslContextParameters {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a CA PEM file to the trust store.
    pub fn trust_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.trust_store.push(PemSource::File(path.into()));
        self
    }

    /// Adds inline CA PEM text to the trust store.
    pub fn trust_pem(mut self, pem: &str) -> Self {
        self.trust_store.push(PemSource::Inline(pem.to_string()));
        self
    }

    /// Whether to trust the system CA bundle in addition to `trust_store`.
    pub fn system_trust_store(mut self, yes: bool) -> Self {
        self.use_system_trust_store = Some(yes);
        self
    }

    /// Certificate chain + private key from PEM files.
    pub fn key_pair_files(mut self, cert_chain: impl Into<PathBuf>, key: impl Into<PathBuf>) -> Self {
        self.cert_chain = Some(PemSource::File(cert_chain.into()));
        self.private_key = Some(PemSource::File(key.into()));
        self
    }

    /// Certificate chain + private key from inline PEM text.
    pub fn key_pair_pem(mut self, cert_chain: &str, key: &str) -> Self {
        self.cert_chain = Some(PemSource::Inline(cert_chain.to_string()));
        self.private_key = Some(PemSource::Inline(key.to_string()));
        self
    }

    /// A single PEM "key store" file with both certificates and the private key.
    pub fn key_store_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.cert_chain = Some(PemSource::File(path.into()));
        self.private_key = None;
        self
    }

    /// A key store file (PKCS#12, JKS, JCEKS or PEM) with its password.
    pub fn key_store(mut self, path: impl Into<PathBuf>, password: &str) -> Self {
        self.cert_chain = Some(PemSource::File(path.into()));
        self.private_key = None;
        self.key_store_password = Some(password.to_string());
        self
    }

    /// A trust store file (PKCS#12, JKS, JCEKS, PEM or DER) with its password.
    pub fn trust_store(mut self, path: impl Into<PathBuf>, password: Option<&str>) -> Self {
        self.trust_store.push(PemSource::File(path.into()));
        self.trust_store_password = password.map(str::to_string);
        self
    }

    pub fn key_password(mut self, password: &str) -> Self {
        self.key_password = Some(password.to_string());
        self
    }

    pub fn cert_alias(mut self, alias: &str) -> Self {
        self.cert_alias = Some(alias.to_string());
        self
    }

    /// Server side: client certificate authentication mode.
    pub fn client_authentication(mut self, mode: ClientAuth) -> Self {
        self.client_authentication = Some(mode);
        self
    }

    /// Adds a CRL file.
    pub fn crl_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.crl_files.push(PemSource::File(path.into()));
        self
    }

    /// Enables (default) or disables session resumption.
    pub fn session_resumption(mut self, yes: bool) -> Self {
        self.session_resumption = Some(yes);
        self
    }

    /// DANGEROUS: disables server certificate verification (tests only).
    pub fn insecure(mut self, yes: bool) -> Self {
        self.insecure_skip_verify = yes;
        self
    }

    /// Whether the server certificate must match the host name (default true).
    pub fn hostname_verification(mut self, yes: bool) -> Self {
        self.hostname_verification = Some(yes);
        self
    }

    pub fn alpn_protocols(mut self, protos: &[&str]) -> Self {
        self.alpn = protos.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn versions(mut self, min: TlsVersion, max: TlsVersion) -> Self {
        self.min_version = Some(min);
        self.max_version = Some(max);
        self
    }

    pub fn with_server_name(mut self, name: &str) -> Self {
        self.server_name = Some(name.to_string());
        self
    }

    /// Whether key material is configured (needed for servers).
    pub fn has_key_material(&self) -> bool {
        self.cert_chain.is_some()
    }

    fn version_range(&self) -> (TlsVersion, TlsVersion) {
        let min = self.min_version.unwrap_or(TlsVersion::Tls12);
        let max = self.max_version.unwrap_or(TlsVersion::Tls13).max(min);
        (min, max)
    }

    fn store_format(name: &Option<String>, data: &[u8]) -> Result<KeyStoreFormat, TlsError> {
        match name.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(n) => KeyStoreFormat::from_name(n).ok_or_else(|| TlsError::Config(format!("unsupported key store type {n}"))),
            None => Ok(KeyStoreFormat::detect(data)),
        }
    }

    /// The configured certificate chain and private key (None without key material).
    pub fn key_material(&self) -> Result<Option<CertifiedKey>, TlsError> {
        let Some(src) = &self.cert_chain else { return Ok(None) };
        let data = src.load()?;
        let what = |e: String| TlsError::Config(format!("key store: {e}"));
        let key_pw = self.key_password.as_deref().or(self.key_store_password.as_deref());
        if let Some(k) = &self.private_key {
            // certificate chain + separate key file
            let chain: Vec<Vec<u8>> = match Self::store_format(&self.key_store_type, &data)? {
                KeyStoreFormat::Pem => pem::parse_pem(&data).map_err(what)?.into_iter().filter(|b| b.label == "CERTIFICATE").map(|b| b.der).collect(),
                KeyStoreFormat::Der => vec![data.clone()],
                _ => return Err(TlsError::Config("a separate private key needs a PEM or DER certificate chain".into())),
            };
            let kdata = k.load()?;
            let key = crate::keys::PrivateKey::from_pem_with_password(&kdata, key_pw)
                .or_else(|e| crate::keys::PrivateKey::from_der(&kdata).map_err(|_| e))
                .or_else(|e| match key_pw {
                    // DER EncryptedPrivateKeyInfo (e.g. pgjdbc's .pk8 key files)
                    Some(pw) => crate::pbe::decrypt_encrypted_private_key_info(&kdata, pw)
                        .and_then(|der| crate::keys::PrivateKey::from_pkcs8(&der).map_err(|x| x.to_string()))
                        .map_err(|_| e),
                    None => Err(e),
                })
                .map_err(|e| TlsError::Config(format!("private key: {e}")))?;
            return CertifiedKey::new(chain, key).map(Some);
        }
        let fmt = Self::store_format(&self.key_store_type, &data)?;
        let ks = KeyStore::load_as(&data, fmt, self.key_store_password.as_deref(), key_pw).map_err(what)?;
        let (key, chain) = ks.key_entry(self.cert_alias.as_deref()).ok_or_else(|| {
            TlsError::Config(match &self.cert_alias {
                Some(a) => format!("key store has no private key entry '{a}' (aliases: {})", ks.aliases().join(", ")),
                None => "key store contains no private key".into(),
            })
        })?;
        CertifiedKey::new(chain.to_vec(), key.clone()).map(Some)
    }

    /// The configured trust anchors (empty when no trust store is set).
    pub fn trust_roots(&self) -> Result<RootStore, TlsError> {
        let mut roots = RootStore::empty();
        for t in &self.trust_store {
            let data = t.load()?;
            match Self::store_format(&self.trust_store_type, &data)? {
                KeyStoreFormat::Pem => {
                    roots.add_pem(&data).map_err(|e| TlsError::Config(format!("trust store: {e}")))?;
                }
                fmt => {
                    let ks = KeyStore::load_as(&data, fmt, self.trust_store_password.as_deref(), None)
                        .map_err(|e| TlsError::Config(format!("trust store: {e}")))?;
                    let certs = ks.trusted_certs();
                    if certs.is_empty() {
                        return Err(TlsError::Config("trust store contains no trusted certificate entries".into()));
                    }
                    for c in certs {
                        roots.add_der(&c).map_err(|e| TlsError::Config(format!("trust store: {e}")))?;
                    }
                }
            }
        }
        Ok(roots)
    }

    /// The configured CRLs.
    pub fn crls(&self) -> Result<Vec<Crl>, TlsError> {
        let mut out = Vec::new();
        for c in &self.crl_files {
            out.extend(Crl::parse_all(&c.load()?).map_err(|e| TlsError::Config(format!("CRL: {e}")))?);
        }
        Ok(out)
    }

    /// Builds a client configuration (presenting the key material, if any, as client
    /// certificate when the server requests one).
    pub fn client_config(&self) -> Result<TlsClientConfig, TlsError> {
        let system = self.use_system_trust_store.unwrap_or(self.trust_store.is_empty());
        let mut b = TlsClientConfig::builder().system_roots(system).root_store(self.trust_roots()?);
        if let Some(ck) = self.key_material()? {
            b = b.client_certificate(ck);
        }
        let protos: Vec<&str> = self.alpn.iter().map(|s| s.as_str()).collect();
        let (min, max) = self.version_range();
        Ok(b.insecure_skip_verify(self.insecure_skip_verify)
            .verify_hostname(self.hostname_verification.unwrap_or(true))
            .crls(self.crls()?)
            .session_resumption(self.session_resumption.unwrap_or(true))
            .alpn_protocols(&protos)
            .versions(min, max)
            .build())
    }

    /// Builds a server configuration (requires key material). With client
    /// authentication the trust store validates client certificates.
    pub fn server_config(&self) -> Result<TlsServerConfig, TlsError> {
        let ck = self.key_material()?.ok_or_else(|| TlsError::Config("no server certificate configured (keyStore / certificate chain)".into()))?;
        let protos: Vec<&str> = self.alpn.iter().map(|s| s.as_str()).collect();
        let (min, max) = self.version_range();
        let mut b = TlsServerConfig::builder()
            .certified_key(ck)
            .alpn_protocols(&protos)
            .versions(min, max)
            .session_resumption(self.session_resumption.unwrap_or(true))
            .session_lifetime_secs(self.session_timeout.unwrap_or(crate::tls::config::DEFAULT_SESSION_LIFETIME));
        let mode = self.client_authentication.unwrap_or_default();
        if mode != ClientAuth::None {
            let roots = self.trust_roots()?;
            if roots.is_empty() {
                return Err(TlsError::Config(format!("clientAuthentication={mode:?} requires a trust store for client certificates")));
            }
            b = b.client_auth(mode).client_roots(roots).crls(self.crls()?);
        }
        b.build()
    }

    /// Reads parameters from properties below `prefix` (e.g. `camel.ssl.`), using
    /// relaxed key matching (case and `-` insensitive). Recognised keys:
    /// `keyStore` (combined PEM) or `certFile`/`certChain` + `keyFile`/`privateKey`,
    /// `trustStore` (comma-separated CA PEM files or inline PEM), `useSystemTrustStore`,
    /// `trustAllCertificates`/`insecureSkipVerify`, `hostnameVerification`, `secureSocketProtocol` (one version),
    /// `secureSocketProtocols` (list: min..max), `minVersion`, `maxVersion`,
    /// `alpn`/`applicationProtocols`, `serverName`, `keystorePassword`, `keyPassword`,
    /// `keyStoreType`, `certAlias`, `trustStorePassword`, `trustStoreType`,
    /// `clientAuthentication` (`NONE`/`WANT`/`REQUIRE`, also `serverParameters.clientAuthentication`,
    /// `clientAuth`, `needClientAuth=true`, `wantClientAuth=true`), `crlFile`,
    /// `sessionTimeout`, `sessionResumption`. Returns `None` when none is set.
    pub fn from_properties(prefix: &str, get: &dyn Fn(&str) -> Option<String>, keys: &[String]) -> Result<Option<Self>, TlsError> {
        let norm = |k: &str| k.to_ascii_lowercase().replace(['-', '_'], "");
        let lookup = |names: &[&str]| -> Option<String> {
            for k in keys {
                if let Some(rest) = k.strip_prefix(prefix) {
                    let n = norm(rest);
                    if names.iter().any(|x| norm(x) == n) {
                        if let Some(v) = get(k) {
                            if !v.trim().is_empty() {
                                return Some(v);
                            }
                        }
                    }
                }
            }
            None
        };
        let mut p = SslContextParameters::new();
        let mut any = false;
        if let Some(v) = lookup(&["keyStore", "keyStoreFile", "keyStore.resource"]) {
            p.cert_chain = Some(PemSource::parse(&v));
            any = true;
        }
        if let Some(v) = lookup(&["certFile", "certChain", "certificate", "certChainFile"]) {
            p.cert_chain = Some(PemSource::parse(&v));
            any = true;
        }
        if let Some(v) = lookup(&["keyFile", "privateKey", "privateKeyFile"]) {
            p.private_key = Some(PemSource::parse(&v));
            any = true;
        }
        if let Some(v) = lookup(&["trustStore", "trustStoreFile", "trustStore.resource", "caFile", "trustedCertificates"]) {
            if v.contains("-----BEGIN") {
                p.trust_store.push(PemSource::Inline(v));
            } else {
                p.trust_store.extend(v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(PemSource::parse));
            }
            any = true;
        }
        if let Some(v) = lookup(&["useSystemTrustStore", "systemTrustStore"]) {
            p.use_system_trust_store = Some(truthy(&v));
            any = true;
        }
        if let Some(v) = lookup(&["trustAllCertificates", "insecureSkipVerify", "insecure"]) {
            p.insecure_skip_verify = truthy(&v);
            any = true;
        }
        if let Some(v) = lookup(&["hostnameVerification", "verifyHostname"]) {
            p.hostname_verification = Some(truthy(&v));
            any = true;
        }
        if let Some(v) = lookup(&["secureSocketProtocol", "protocol"]) {
            if let Some(ver) = parse_tls_version(&v) {
                p.min_version = Some(ver);
                p.max_version = Some(ver);
            }
            any = true;
        }
        if let Some(v) = lookup(&["secureSocketProtocols", "protocols", "enabledProtocols"]) {
            let vs: Vec<TlsVersion> = v.split(',').filter_map(parse_tls_version).collect();
            if !vs.is_empty() {
                p.min_version = vs.iter().min().copied();
                p.max_version = vs.iter().max().copied();
            }
            any = true;
        }
        if let Some(v) = lookup(&["minVersion"]).and_then(|v| parse_tls_version(&v)) {
            p.min_version = Some(v);
            any = true;
        }
        if let Some(v) = lookup(&["maxVersion"]).and_then(|v| parse_tls_version(&v)) {
            p.max_version = Some(v);
            any = true;
        }
        if let Some(v) = lookup(&["alpn", "applicationProtocols", "alpnProtocols"]) {
            p.alpn = v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            any = true;
        }
        if let Some(v) = lookup(&["serverName", "sniHostName"]) {
            p.server_name = Some(v.trim().to_string());
            any = true;
        }
        if let Some(v) = lookup(&["keystorePassword", "keyStore.password", "keyStorePass"]) {
            p.key_store_password = Some(v);
            any = true;
        }
        if let Some(v) = lookup(&["keyPassword", "keyManagers.keyPassword", "privateKeyPassword"]) {
            p.key_password = Some(v);
            any = true;
        }
        if let Some(v) = lookup(&["keyStoreType", "keyStore.type"]) {
            p.key_store_type = Some(v.trim().to_string());
            any = true;
        }
        if let Some(v) = lookup(&["certAlias", "keyAlias", "alias"]) {
            p.cert_alias = Some(v.trim().to_string());
            any = true;
        }
        if let Some(v) = lookup(&["trustStorePassword", "trustStore.password"]) {
            p.trust_store_password = Some(v);
            any = true;
        }
        if let Some(v) = lookup(&["trustStoreType", "trustStore.type"]) {
            p.trust_store_type = Some(v.trim().to_string());
            any = true;
        }
        if let Some(v) = lookup(&["clientAuthentication", "serverParameters.clientAuthentication", "clientAuth"]) {
            p.client_authentication =
                Some(ClientAuth::parse(&v).ok_or_else(|| TlsError::Config(format!("invalid clientAuthentication '{v}' (NONE, WANT or REQUIRE)")))?);
            any = true;
        }
        if let Some(v) = lookup(&["needClientAuth"]) {
            if truthy(&v) {
                p.client_authentication = Some(ClientAuth::Need);
            }
            any = true;
        }
        if let Some(v) = lookup(&["wantClientAuth"]) {
            if truthy(&v) && p.client_authentication != Some(ClientAuth::Need) {
                p.client_authentication = Some(ClientAuth::Want);
            }
            any = true;
        }
        if let Some(v) = lookup(&["crlFile", "crlFiles", "crl"]) {
            p.crl_files.extend(v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(PemSource::parse));
            any = true;
        }
        if let Some(v) = lookup(&["sessionTimeout"]) {
            p.session_timeout = Some(v.trim().parse().map_err(|_| TlsError::Config(format!("invalid sessionTimeout '{v}'")))?);
            any = true;
        }
        if let Some(v) = lookup(&["sessionResumption", "sessionCaching", "sessionCacheEnabled"]) {
            p.session_resumption = Some(truthy(&v));
            any = true;
        }
        Ok(if any { Some(p) } else { None })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn versions_parse() {
        assert_eq!(parse_tls_version("TLSv1.2"), Some(TlsVersion::Tls12));
        assert_eq!(parse_tls_version("TLSv1.3"), Some(TlsVersion::Tls13));
        assert_eq!(parse_tls_version("1.3"), Some(TlsVersion::Tls13));
        assert_eq!(parse_tls_version("SSLv3"), None);
    }

    #[test]
    fn properties_and_configs() {
        let id = crate::certgen::self_signed("localhost");
        let dir = std::env::temp_dir().join(format!("camel-tls-params-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ks = dir.join("ks.pem");
        std::fs::write(&ks, format!("{}{}", id.cert_pem, id.key_pem)).unwrap();
        let ca = dir.join("ca.pem");
        std::fs::write(&ca, &id.cert_pem).unwrap();
        let mut m = BTreeMap::new();
        m.insert("camel.ssl.keystore".to_string(), ks.display().to_string());
        m.insert("camel.ssl.trust-store".to_string(), ca.display().to_string());
        m.insert("camel.ssl.secureSocketProtocol".to_string(), "TLSv1.3".to_string());
        m.insert("other.x".to_string(), "y".to_string());
        let keys: Vec<String> = m.keys().cloned().collect();
        let p = SslContextParameters::from_properties("camel.ssl.", &|k| m.get(k).cloned(), &keys).unwrap().unwrap();
        assert_eq!(p.min_version, Some(TlsVersion::Tls13));
        assert_eq!(p.trust_store.len(), 1);
        assert!(p.server_config().is_ok());
        let c = p.client_config().unwrap();
        assert_eq!(c.roots().len(), 1, "system roots are not added when a trust store is set");
        let none = SslContextParameters::from_properties("camel.nossl.", &|k| m.get(k).cloned(), &keys).unwrap();
        assert!(none.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keystore_and_client_auth_properties() {
        let dir = std::env::temp_dir().join(format!("camel-tls-params-ks-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pki = crate::testing::mtls();
        let ks = dir.join("server.jks");
        let store = KeyStore::from_key_entry("srv", pki.server.key.clone(), vec![pki.server.cert_der.clone()]);
        std::fs::write(&ks, store.to_jks("storepw", "keypw")).unwrap();
        let ts = dir.join("trust.p12");
        std::fs::write(&ts, KeyStore::new().add_cert("ca", pki.ca.cert_der.clone()).to_pkcs12("trustpw")).unwrap();
        let crl = dir.join("ca.crl");
        std::fs::write(&crl, pem::encode_pem("X509 CRL", &crate::certgen::issue_crl(&pki.ca, &[], 1))).unwrap();
        let mut m = BTreeMap::new();
        m.insert("camel.ssl.keyStore".to_string(), ks.display().to_string());
        m.insert("camel.ssl.keystorePassword".to_string(), "storepw".to_string());
        m.insert("camel.ssl.keyPassword".to_string(), "keypw".to_string());
        m.insert("camel.ssl.keyStoreType".to_string(), "JKS".to_string());
        m.insert("camel.ssl.certAlias".to_string(), "srv".to_string());
        m.insert("camel.ssl.trustStore".to_string(), ts.display().to_string());
        m.insert("camel.ssl.trustStorePassword".to_string(), "trustpw".to_string());
        m.insert("camel.ssl.serverParameters.clientAuthentication".to_string(), "REQUIRE".to_string());
        m.insert("camel.ssl.crlFile".to_string(), crl.display().to_string());
        m.insert("camel.ssl.sessionTimeout".to_string(), "600".to_string());
        let keys: Vec<String> = m.keys().cloned().collect();
        let p = SslContextParameters::from_properties("camel.ssl.", &|k| m.get(k).cloned(), &keys).unwrap().unwrap();
        assert_eq!(p.client_authentication, Some(ClientAuth::Need));
        assert_eq!(p.session_timeout, Some(600));
        let scfg = p.server_config().unwrap();
        assert_eq!(scfg.client_auth(), ClientAuth::Need);
        assert_eq!(p.crls().unwrap().len(), 1);
        assert!(p.client_config().unwrap().has_client_certificate());
        let mut bad = m.clone();
        bad.insert("camel.ssl.clientAuthentication".to_string(), "SOMETIMES".to_string());
        let bad_keys: Vec<String> = bad.keys().cloned().collect();
        assert!(SslContextParameters::from_properties("camel.ssl.", &|k| bad.get(k).cloned(), &bad_keys).is_err());
        // client authentication without a trust store is a configuration error
        let no_trust = SslContextParameters { trust_store: Vec::new(), ..p };
        assert!(no_trust.server_config().is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_key_material_is_reported() {
        let e = SslContextParameters::new().server_config().unwrap_err();
        assert!(e.to_string().contains("no server certificate"));
    }
}

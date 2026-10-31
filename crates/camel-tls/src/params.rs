//! [`SslContextParameters`]: the rust-camel equivalent of Camel's
//! `SSLContextParameters` - one description of trust material, key material and
//! protocol options from which client and server TLS configurations are built.
//!
//! Key and trust stores are PEM files (Java keystores are not supported): a
//! "key store" is a PEM file holding the certificate chain (leaf first) and the
//! private key, or two separate files; a "trust store" is one or more PEM files with
//! CA certificates.
//!
//! ```
//! use camel_tls::SslContextParameters;
//! let id = camel_tls::self_signed("localhost");
//! let server = SslContextParameters::new().key_pair_pem(&id.cert_pem, &id.key_pem);
//! let client = SslContextParameters::new().trust_pem(&id.cert_pem);
//! let _scfg = server.server_config().unwrap();
//! let _ccfg = client.client_config().unwrap();
//! ```

use crate::pem;
use crate::tls::config::{TlsClientConfig, TlsServerConfig};
use crate::tls::{TlsError, TlsVersion};
use std::path::PathBuf;

/// Source of PEM material: a file path or inline PEM text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PemSource {
    File(PathBuf),
    Inline(String),
}

impl PemSource {
    fn load(&self) -> Result<Vec<u8>, TlsError> {
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

    /// Builds a client configuration.
    pub fn client_config(&self) -> Result<TlsClientConfig, TlsError> {
        let system = self.use_system_trust_store.unwrap_or(self.trust_store.is_empty());
        let mut b = TlsClientConfig::builder().system_roots(system);
        for t in &self.trust_store {
            b = b.add_root_pem(&t.load()?)?;
        }
        let protos: Vec<&str> = self.alpn.iter().map(|s| s.as_str()).collect();
        let (min, max) = self.version_range();
        Ok(b.insecure_skip_verify(self.insecure_skip_verify)
            .verify_hostname(self.hostname_verification.unwrap_or(true))
            .alpn_protocols(&protos).versions(min, max).build())
    }

    /// Builds a server configuration (requires `cert_chain`).
    pub fn server_config(&self) -> Result<TlsServerConfig, TlsError> {
        let chain = self
            .cert_chain
            .as_ref()
            .ok_or_else(|| TlsError::Config("no server certificate configured (keyStore / certificate chain)".into()))?
            .load()?;
        let key = match &self.private_key {
            Some(k) => k.load()?,
            None => {
                // combined key store: pick the private key block out of the chain file
                let blocks = pem::parse_pem(&chain).map_err(TlsError::Config)?;
                let kb = blocks
                    .iter()
                    .find(|b| b.label.ends_with("PRIVATE KEY"))
                    .ok_or_else(|| TlsError::Config("key store PEM contains no private key".into()))?;
                pem::encode_pem(&kb.label, &kb.der).into_bytes()
            }
        };
        let protos: Vec<&str> = self.alpn.iter().map(|s| s.as_str()).collect();
        let (min, max) = self.version_range();
        TlsServerConfig::builder().cert_chain_pem(&chain).private_key_pem(&key).alpn_protocols(&protos).versions(min, max).build()
    }

    /// Reads parameters from properties below `prefix` (e.g. `camel.ssl.`), using
    /// relaxed key matching (case and `-` insensitive). Recognised keys:
    /// `keyStore` (combined PEM) or `certFile`/`certChain` + `keyFile`/`privateKey`,
    /// `trustStore` (comma-separated CA PEM files or inline PEM), `useSystemTrustStore`,
    /// `trustAllCertificates`/`insecureSkipVerify`, `hostnameVerification`, `secureSocketProtocol` (one version),
    /// `secureSocketProtocols` (list: min..max), `minVersion`, `maxVersion`,
    /// `alpn`/`applicationProtocols`, `serverName`. Returns `None` when none is set.
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
    fn missing_key_material_is_reported() {
        let e = SslContextParameters::new().server_config().unwrap_err();
        assert!(e.to_string().contains("no server certificate"));
    }
}

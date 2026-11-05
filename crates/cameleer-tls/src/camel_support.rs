//! Camel integration of [`SslContextParameters`]: registry lookup
//! (`sslContextParameters=#ssl`), the global parameters (`camel.ssl.*` properties or a
//! bound bean) used with `useGlobalSslContextParameters=true`, the embedded server
//! parameters (`camel.server.ssl.*`), and error mapping.
//!
//! Resolution for an endpoint ([`take_endpoint_ssl`]):
//! 1. the endpoint option `sslContextParameters=#name` (a registry bean),
//! 2. the component property `camel.component.<scheme>.sslContextParameters=#name`,
//! 3. if `useGlobalSslContextParameters=true` (endpoint option or
//!    `camel.component.<scheme>.useGlobalSslContextParameters`): the global parameters,
//! 4. otherwise none - TLS endpoints (`https:`, `ftps:`...) then use the global
//!    parameters if `camel.ssl.enabled=true`, else the system trust store.

use crate::params::SslContextParameters;
use crate::tls::config::{ClientAuth, TlsClientConfig, TlsServerConfig};
use crate::tls::TlsError;
use cameleer_core::error::{CamelError, Result};
use cameleer_core::util::Parameters;
use cameleer_core::context::CamelContext;
use std::sync::{Arc, Once, OnceLock};
use std::time::Duration;

use crate::net::NetStream;

/// Registry name of the global SSL context parameters.
pub const GLOBAL_SSL_CONTEXT_PARAMETERS: &str = "CamelGlobalSSLContextParameters";

/// Exception kinds used for TLS failures (hierarchy: `SSLHandshakeException` /
/// `SSLPeerUnverifiedException` -> `SSLException` -> `IOException`).
pub mod kinds {
    pub const SSL: &str = "SSLException";
    pub const SSL_HANDSHAKE: &str = "SSLHandshakeException";
    pub const SSL_PEER_UNVERIFIED: &str = "SSLPeerUnverifiedException";
}

fn register_kinds() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        cameleer_core::error::register_kind_parent(kinds::SSL, cameleer_core::error::kinds::IO);
        cameleer_core::error::register_kind_parent(kinds::SSL_HANDSHAKE, kinds::SSL);
        cameleer_core::error::register_kind_parent(kinds::SSL_PEER_UNVERIFIED, kinds::SSL);
    });
}

/// Maps a TLS configuration/handshake error to a Camel exception.
pub fn tls_error(e: &TlsError, what: &str) -> CamelError {
    register_kinds();
    let kind = match e {
        TlsError::Certificate(_) => kinds::SSL_PEER_UNVERIFIED,
        TlsError::Config(_) => cameleer_core::error::kinds::ILLEGAL_ARGUMENT,
        TlsError::Io(_) | TlsError::Closed => cameleer_core::error::kinds::IO,
        _ => kinds::SSL_HANDSHAKE,
    };
    CamelError::new(kind, format!("{}: {}", e, what))
}

/// Maps an I/O error from a TLS connection attempt (handshake failures are carried
/// as `InvalidData` errors wrapping a [`TlsError`]) to a Camel exception, or `None`
/// if it is a plain I/O error the caller should map itself.
pub fn tls_io_error(e: &std::io::Error, what: &str) -> Option<CamelError> {
    let inner = e.get_ref()?.downcast_ref::<TlsError>()?;
    Some(tls_error(inner, what))
}

/// Binds the global SSL context parameters (used with `useGlobalSslContextParameters=true`).
pub fn set_global_ssl_context_parameters(ctx: &CamelContext, params: SslContextParameters) {
    ctx.registry().bind(GLOBAL_SSL_CONTEXT_PARAMETERS, params);
}

fn props_params(ctx: &CamelContext, prefix: &str) -> Result<Option<SslContextParameters>> {
    let all = ctx.properties().all();
    let keys: Vec<String> = all.keys().cloned().collect();
    SslContextParameters::from_properties(prefix, &|k| ctx.properties().get(k), &keys).map_err(|e| tls_error(&e, prefix))
}

/// Property lookup with relaxed binding (`camel.server.sslEnabled` also matches
/// `camel.server.ssl-enabled` / `camel.server.ssl_enabled`, case-insensitively).
fn prop(ctx: &CamelContext, key: &str) -> Option<String> {
    if let Some(v) = ctx.properties().get(key) {
        return Some(v);
    }
    let norm = |k: &str| k.to_ascii_lowercase().replace(['-', '_'], "");
    let want = norm(key);
    ctx.properties().all().into_iter().find(|(k, _)| norm(k) == want).map(|(_, v)| v)
}

fn prop_true(ctx: &CamelContext, key: &str) -> bool {
    prop(ctx, key).map(|v| v.trim().eq_ignore_ascii_case("true")).unwrap_or(false)
}

/// The global parameters: the bean bound as [`GLOBAL_SSL_CONTEXT_PARAMETERS`], else
/// `camel.ssl.*` properties when `camel.ssl.enabled=true`.
pub fn global_ssl_context_parameters(ctx: &CamelContext) -> Result<Option<Arc<SslContextParameters>>> {
    if let Some(p) = ctx.registry().lookup::<SslContextParameters>(GLOBAL_SSL_CONTEXT_PARAMETERS) {
        return Ok(Some(p));
    }
    if prop_true(ctx, "camel.ssl.enabled") {
        let p = props_params(ctx, "camel.ssl.")?.unwrap_or_default();
        return Ok(Some(Arc::new(p)));
    }
    Ok(None)
}

/// Looks up `#name` / `#bean:name` / `name` in the registry.
pub fn lookup_ssl_context_parameters(ctx: &CamelContext, reference: &str) -> Result<Arc<SslContextParameters>> {
    let name = reference.trim();
    let name = name.strip_prefix("#bean:").or_else(|| name.strip_prefix('#')).unwrap_or(name);
    ctx.registry().lookup::<SslContextParameters>(name).ok_or_else(|| {
        CamelError::no_such_bean(name).with_cause(CamelError::illegal_argument(format!(
            "No SslContextParameters bean bound as '{}' (sslContextParameters={})",
            name, reference
        )))
    })
}

/// Consumes `sslContextParameters` and `useGlobalSslContextParameters` from the
/// endpoint options and resolves the parameters to use (see the module docs).
pub fn take_endpoint_ssl(ctx: &CamelContext, params: &mut Parameters, scheme: &str) -> Result<Option<Arc<SslContextParameters>>> {
    let reference = params.take("sslContextParameters").or_else(|| prop(ctx, &format!("camel.component.{}.sslContextParameters", scheme)));
    let use_global = match params.take("useGlobalSslContextParameters") {
        Some(v) => v.trim().eq_ignore_ascii_case("true"),
        None => prop_true(ctx, &format!("camel.component.{}.useGlobalSslContextParameters", scheme)),
    };
    if let Some(r) = reference.filter(|r| !r.trim().is_empty()) {
        return lookup_ssl_context_parameters(ctx, &r).map(Some);
    }
    if use_global {
        return match global_ssl_context_parameters(ctx)? {
            Some(p) => Ok(Some(p)),
            None => Err(CamelError::illegal_argument(format!(
                "useGlobalSslContextParameters=true on {} but no global SSL context parameters are configured (camel.ssl.enabled=true / {})",
                scheme, GLOBAL_SSL_CONTEXT_PARAMETERS
            ))),
        };
    }
    Ok(None)
}

/// The parameters a TLS client uses: the explicit ones, else the global ones
/// (if configured), else defaults (system trust store, full verification).
pub fn effective_client_params(ctx: &CamelContext, explicit: Option<&Arc<SslContextParameters>>) -> Result<Arc<SslContextParameters>> {
    if let Some(p) = explicit {
        return Ok(p.clone());
    }
    Ok(global_ssl_context_parameters(ctx)?.unwrap_or_default())
}

/// Builds a client config from [`effective_client_params`].
pub fn client_config(ctx: &CamelContext, explicit: Option<&Arc<SslContextParameters>>) -> Result<TlsClientConfig> {
    let p = effective_client_params(ctx, explicit)?;
    p.client_config().map_err(|e| tls_error(&e, "sslContextParameters"))
}

/// Builds a server config from the explicit parameters, else the embedded-server
/// parameters ([`server_ssl_context_parameters`]), else the global ones.
pub fn server_config(ctx: &CamelContext, explicit: Option<&Arc<SslContextParameters>>, what: &str) -> Result<TlsServerConfig> {
    server_config_with_client_auth(ctx, explicit, what, None)
}

/// Like [`server_config`], overriding the client authentication mode (endpoint options
/// such as `needClientAuth=true` / `clientAuth=REQUIRE`).
pub fn server_config_with_client_auth(
    ctx: &CamelContext,
    explicit: Option<&Arc<SslContextParameters>>,
    what: &str,
    client_auth: Option<ClientAuth>,
) -> Result<TlsServerConfig> {
    let p = match explicit {
        Some(p) => p.clone(),
        None => match server_ssl_context_parameters(ctx)?.or(global_ssl_context_parameters(ctx)?) {
            Some(p) => p,
            None => {
                return Err(CamelError::illegal_argument(format!(
                    "{} requires a server certificate: set sslContextParameters=#bean, camel.server.ssl.keyStore (or certFile/keyFile) or camel.ssl.keyStore",
                    what
                )))
            }
        },
    };
    let p = match client_auth {
        Some(mode) => Arc::new(SslContextParameters { client_authentication: Some(mode), ..(*p).clone() }),
        None => p,
    };
    p.server_config().map_err(|e| tls_error(&e, what))
}

/// Consumes the endpoint options selecting client certificate authentication:
/// `clientAuth` / `clientAuthentication` (`NONE`, `WANT`, `REQUIRE`), `needClientAuth=true`
/// (REQUIRE) and `wantClientAuth=true` (WANT).
pub fn take_client_auth(params: &mut Parameters) -> Result<Option<ClientAuth>> {
    let mut mode = None;
    for key in ["clientAuth", "clientAuthentication"] {
        if let Some(v) = params.take(key) {
            mode = Some(ClientAuth::parse(&v).ok_or_else(|| CamelError::illegal_argument(format!("Invalid {}: {} (NONE, WANT or REQUIRE)", key, v)))?);
        }
    }
    for (key, value) in [("needClientAuth", ClientAuth::Need), ("wantClientAuth", ClientAuth::Want)] {
        if let Some(v) = params.take(key) {
            match v.trim().to_ascii_lowercase().as_str() {
                "true" => {
                    if mode != Some(ClientAuth::Need) {
                        mode = Some(value);
                    }
                }
                "false" => {
                    if mode.is_none() {
                        mode = Some(ClientAuth::None);
                    }
                }
                other => return Err(CamelError::illegal_argument(format!("Invalid boolean value for {}: {}", key, other))),
            }
        }
    }
    Ok(mode)
}

/// Key / trust store options of a component (Kafka `sslKeystoreLocation`, Spring
/// `spring.rabbitmq.ssl.key-store`, PostgreSQL `sslcert`...), applied on top of the
/// endpoint's [`SslContextParameters`] by [`StoreOptions::apply`].
#[derive(Clone, Debug, Default)]
pub struct StoreOptions {
    /// Key store (PKCS#12, JKS, JCEKS or PEM with certificate chain + key).
    pub key_store: Option<String>,
    /// Separate PEM private key file (with `key_store` holding the certificate chain).
    pub private_key: Option<String>,
    pub key_store_password: Option<String>,
    pub key_password: Option<String>,
    pub key_store_type: Option<String>,
    pub trust_store: Option<String>,
    pub trust_store_password: Option<String>,
    pub trust_store_type: Option<String>,
}

impl StoreOptions {
    pub fn is_empty(&self) -> bool {
        self.key_store.is_none() && self.trust_store.is_none() && self.private_key.is_none()
    }

    /// Applies the options to `p` (a key store replaces the configured key material,
    /// a trust store the configured trust store).
    pub fn apply(&self, p: &mut SslContextParameters) {
        let blank = |o: &Option<String>| o.as_ref().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        if let Some(k) = blank(&self.key_store) {
            p.cert_chain = Some(crate::params::PemSource::parse(&k));
            p.private_key = blank(&self.private_key).map(|k| crate::params::PemSource::parse(&k));
        }
        if let Some(v) = &self.key_store_password {
            p.key_store_password = Some(v.clone());
        }
        if let Some(v) = &self.key_password {
            p.key_password = Some(v.clone());
        }
        if let Some(v) = blank(&self.key_store_type) {
            p.key_store_type = Some(v);
        }
        if let Some(t) = blank(&self.trust_store) {
            p.trust_store = vec![crate::params::PemSource::parse(&t)];
        }
        if let Some(v) = &self.trust_store_password {
            p.trust_store_password = Some(v.clone());
        }
        if let Some(v) = blank(&self.trust_store_type) {
            p.trust_store_type = Some(v);
        }
    }
}

/// Exchange headers describing a verified client certificate chain (leaf first), named
/// like Camel Netty's `CamelNettySSLClientCert*` headers: `{prefix}SubjectName`,
/// `{prefix}IssuerName`, `{prefix}SerialNo`, `{prefix}NotBefore`, `{prefix}NotAfter`
/// (distinguished names most-specific attribute first, times as RFC 3339 UTC).
pub fn client_cert_headers(prefix: &str, chain: &[Vec<u8>]) -> Vec<(String, String)> {
    let Some(leaf) = chain.first().and_then(|d| crate::x509::Certificate::from_der(d).ok()) else { return Vec::new() };
    let serial: String = {
        let mut s = leaf.serial.as_slice();
        while s.len() > 1 && s[0] == 0 {
            s = &s[1..];
        }
        crate::util::hex(s)
    };
    vec![
        (format!("{}SubjectName", prefix), leaf.subject.to_dn_string()),
        (format!("{}IssuerName", prefix), leaf.issuer.to_dn_string()),
        (format!("{}SerialNo", prefix), serial),
        (format!("{}NotBefore", prefix), rfc3339(leaf.not_before)),
        (format!("{}NotAfter", prefix), rfc3339(leaf.not_after)),
    ]
}

fn rfc3339(unix: i64) -> String {
    let (y, m, d) = crate::asn1::civil_from_days(unix.div_euclid(86400));
    let s = unix.rem_euclid(86400);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, m, d, s / 3600, s / 60 % 60, s % 60)
}

/// The embedded HTTP server parameters: `camel.server.ssl.*` properties
/// (`camel.server.ssl.keyStore`, `certFile`, `keyFile`, ... - same keys as `camel.ssl.*`),
/// or the global parameters when `camel.server.useGlobalSslContextParameters=true`.
pub fn server_ssl_context_parameters(ctx: &CamelContext) -> Result<Option<Arc<SslContextParameters>>> {
    if prop_true(ctx, "camel.server.useGlobalSslContextParameters") {
        return global_ssl_context_parameters(ctx);
    }
    Ok(props_params(ctx, "camel.server.ssl.")?.map(Arc::new))
}

/// Whether the embedded HTTP server should listen with TLS (`camel.server.sslEnabled`,
/// `camel.server.ssl.enabled`, or `camel.server.useGlobalSslContextParameters=true`).
pub fn server_ssl_enabled(ctx: &CamelContext) -> bool {
    prop_true(ctx, "camel.server.sslEnabled") || prop_true(ctx, "camel.server.ssl.enabled") || prop_true(ctx, "camel.server.useGlobalSslContextParameters")
}

/// Applies `SslContextParameters::server_name` (SNI override) or returns `host`.
pub fn sni_name<'a>(params: Option<&'a SslContextParameters>, host: &'a str) -> &'a str {
    params.and_then(|p| p.server_name.as_deref()).unwrap_or(host)
}

/// Resolved TLS settings of a (non-HTTP) endpoint: built by [`endpoint_tls`] when
/// TLS is enabled. Client/server configurations are built once and cached.
#[derive(Clone, Debug)]
pub struct EndpointTls {
    pub params: Arc<SslContextParameters>,
    client: Arc<OnceLock<std::result::Result<Arc<TlsClientConfig>, String>>>,
    server: Arc<OnceLock<std::result::Result<Arc<TlsServerConfig>, String>>>,
}

impl EndpointTls {
    /// TLS settings from explicit parameters.
    pub fn new(params: Arc<SslContextParameters>) -> EndpointTls {
        EndpointTls { params, client: Arc::new(OnceLock::new()), server: Arc::new(OnceLock::new()) }
    }

    /// The client configuration.
    pub fn client_config(&self) -> Result<Arc<TlsClientConfig>> {
        self.client
            .get_or_init(|| self.params.client_config().map(Arc::new).map_err(|e| e.to_string()))
            .clone()
            .map_err(|e| { register_kinds(); CamelError::illegal_argument(format!("TLS client configuration: {}", e)) })
    }

    /// The server configuration (needs key material).
    pub fn server_config(&self, what: &str) -> Result<Arc<TlsServerConfig>> {
        self.server
            .get_or_init(|| self.params.server_config().map(Arc::new).map_err(|e| e.to_string()))
            .clone()
            .map_err(|e| CamelError::illegal_argument(format!("{} (TLS server): {}", what, e)))
    }

    /// The name used for SNI and certificate verification when connecting to `host`.
    pub fn server_name<'a>(&'a self, host: &'a str) -> &'a str {
        self.params.server_name.as_deref().unwrap_or(host)
    }

    /// Connects to `host:port` and performs the TLS handshake.
    pub fn connect(&self, host: &str, port: u16, timeout: Duration) -> Result<NetStream> {
        let cfg = self.client_config()?;
        let what = format!("{}:{}", host, port);
        let tcp = crate::net::connect_tcp(host, port, timeout).map_err(|e| connect_error(e, &what))?;
        NetStream::client(tcp, self.server_name(host), &cfg).map_err(|e| io_to_camel(e, &what))
    }

    /// Client handshake over an already connected socket.
    pub fn handshake_client(&self, tcp: std::net::TcpStream, host: &str) -> Result<NetStream> {
        let cfg = self.client_config()?;
        NetStream::client(tcp, self.server_name(host), &cfg).map_err(|e| io_to_camel(e, host))
    }

    /// Upgrades a plain connection (STARTTLS-style).
    pub fn start_tls(&self, stream: NetStream, host: &str) -> Result<NetStream> {
        let cfg = self.client_config()?;
        stream.start_tls_client(self.server_name(host), &cfg).map_err(|e| io_to_camel(e, host))
    }

    /// Server handshake on an accepted connection.
    pub fn accept(&self, tcp: std::net::TcpStream, what: &str) -> Result<NetStream> {
        let cfg = self.server_config(what)?;
        NetStream::server(tcp, &cfg).map_err(|e| io_to_camel(e, what))
    }
}

fn connect_error(e: std::io::Error, what: &str) -> CamelError {
    match e.kind() {
        std::io::ErrorKind::ConnectionRefused => CamelError::new(cameleer_core::error::kinds::CONNECT, format!("Connection refused: {}", what)),
        std::io::ErrorKind::TimedOut => CamelError::new(cameleer_core::error::kinds::CONNECT, format!("Connect timed out: {}", what)),
        _ => CamelError::io(format!("{}: {}", e, what)),
    }
}

/// Maps an I/O error from a TLS connection to a Camel exception (TLS failures become
/// `SSLHandshakeException`/`SSLPeerUnverifiedException`, other errors `IOException`).
pub fn io_to_camel(e: std::io::Error, what: &str) -> CamelError {
    tls_io_error(&e, what).unwrap_or_else(|| CamelError::io(format!("{}: {}", e, what)))
}

/// Resolves the TLS settings of an endpoint. TLS is enabled when `enabled_by_default`
/// (e.g. `ftps:`, `smtps:`), when one of the boolean `flags` options is true (e.g.
/// `ssl=true`), or when `sslContextParameters` is given. Parameters: explicit
/// (`sslContextParameters` / `useGlobalSslContextParameters`), else global, else defaults.
pub fn endpoint_tls(ctx: &CamelContext, params: &mut Parameters, scheme: &str, enabled_by_default: bool, flags: &[&str]) -> Result<Option<EndpointTls>> {
    let explicit = take_endpoint_ssl(ctx, params, scheme)?;
    let mut enabled = enabled_by_default || explicit.is_some();
    for f in flags {
        if let Some(v) = params.take(f) {
            match v.trim().to_ascii_lowercase().as_str() {
                "true" => enabled = true,
                "false" => {}
                other => return Err(CamelError::illegal_argument(format!("Invalid boolean value for {}: {}", f, other))),
            }
        }
    }
    if !enabled {
        return Ok(None);
    }
    Ok(Some(EndpointTls::new(effective_client_params(ctx, explicit.as_ref())?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_resolution() {
        let ctx = CamelContext::new();
        let id = crate::certgen::self_signed("localhost");
        ctx.registry().bind("ssl", SslContextParameters::new().trust_pem(&id.cert_pem));
        let mut p = Parameters::new([("sslContextParameters".to_string(), "#ssl".to_string())].into_iter().collect());
        let r = take_endpoint_ssl(&ctx, &mut p, "https").unwrap().unwrap();
        assert_eq!(r.trust_store.len(), 1);
        assert!(p.is_empty());

        let mut p = Parameters::new([("sslContextParameters".to_string(), "#nope".to_string())].into_iter().collect());
        assert!(take_endpoint_ssl(&ctx, &mut p, "https").is_err());

        let mut p = Parameters::new([("useGlobalSslContextParameters".to_string(), "true".to_string())].into_iter().collect());
        assert!(take_endpoint_ssl(&ctx, &mut p, "https").is_err(), "no global configured");
        ctx.properties().set("camel.ssl.enabled", "true");
        ctx.properties().set("camel.ssl.trustAllCertificates", "true");
        let mut p = Parameters::new([("useGlobalSslContextParameters".to_string(), "true".to_string())].into_iter().collect());
        let g = take_endpoint_ssl(&ctx, &mut p, "https").unwrap().unwrap();
        assert!(g.insecure_skip_verify);

        assert!(!server_ssl_enabled(&ctx));
        ctx.properties().set("camel.server.ssl-enabled", "true");
        assert!(server_ssl_enabled(&ctx), "relaxed property binding");

        set_global_ssl_context_parameters(&ctx, SslContextParameters::new().with_server_name("x"));
        assert_eq!(global_ssl_context_parameters(&ctx).unwrap().unwrap().server_name.as_deref(), Some("x"));
    }

    #[test]
    fn error_kinds() {
        let e = tls_error(&TlsError::AlertReceived(40), "x");
        assert!(e.is_a("IOException"));
        assert!(e.is_a(kinds::SSL));
    }
}

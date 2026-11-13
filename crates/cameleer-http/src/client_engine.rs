//! The HTTP client on [simoom-client](https://crates.io/crates/simoom-client): one blocking client
//! per set of settings, shared by every request that uses them, so that connection pooling,
//! HTTP/2 multiplexing, proxies and TLS session resumption work across calls.
//! See `docs/design/simoom-adoption.md`.

use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use simoom_client as sc;

use cameleer_core::error::{kinds, CamelError, Result};
use cameleer_tls::TlsClientConfig;

use crate::client::{ProxyAddr, ProxyConfig, Url};
use crate::http::{HttpHeaders, HttpResponse};

/// The settings that make two requests share a simoom client (and its connections).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct EngineKey {
    pub connect_ms: u64,
    pub read_ms: u64,
    pub max_redirects: usize,
    pub keep_alive: bool,
    pub per_origin: usize,
    pub http2: bool,
    /// Whether the client may open `https` connections: the system trust store is loaded only then
    /// (it costs a few hundred certificates to parse, which a plain HTTP call never needs).
    pub https: bool,
    pub max_head: usize,
    pub max_body: u64,
    /// The address of the `Arc<TlsClientConfig>` (0: the default configuration).
    pub tls: usize,
    pub server_name: Option<String>,
    pub proxy: Option<String>,
}

/// What a request needs besides its URL: the settings of the [`crate::client::HttpClient`] it came from.
pub(crate) struct EngineSettings<'a> {
    pub key: EngineKey,
    pub tls: Option<&'a TlsClientConfig>,
    pub proxy: Option<&'a ProxyConfig>,
}

fn engines() -> &'static Mutex<HashMap<EngineKey, sc::Client>> {
    static ENGINES: OnceLock<Mutex<HashMap<EngineKey, sc::Client>>> = OnceLock::new();
    ENGINES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn duration(ms: u64) -> Option<Duration> {
    if ms == 0 {
        None
    } else {
        Some(Duration::from_millis(ms))
    }
}

fn tls_options(cfg: Option<&TlsClientConfig>, server_name: Option<&str>, https: bool) -> Result<sc::tls::TlsOptions> {
    if !https {
        return Ok(sc::tls::TlsOptions { roots: sc::tls::RootStore::empty(), client_cert: None, server_name: None, resumption: false });
    }
    let mut opts = sc::tls::TlsOptions::default();
    opts.server_name = server_name.map(str::to_string);
    let Some(cfg) = cfg else { return Ok(opts) };
    let x = cfg.export();
    if !x.verify || !x.verify_hostname {
        return Err(CamelError::illegal_argument(
            "The HTTP client always verifies the server certificate and its host name; trust a private server by adding its certificate or CA to the trust store (insecureSkipVerify and a disabled host name check are not available here)",
        ));
    }
    let mut roots = sc::tls::RootStore::empty();
    for der in &x.roots {
        roots.add_der(der).map_err(|e| CamelError::illegal_argument(format!("Invalid trust anchor: {e}")))?;
    }
    opts.roots = roots;
    if !x.client_chain.is_empty() {
        opts.client_cert = Some(
            sc::tls::CertifiedKey::from_der(x.client_chain, &x.client_key_pkcs8).map_err(|e| CamelError::illegal_argument(format!("Invalid client certificate: {e}")))?,
        );
    }
    opts.resumption = x.resumption;
    Ok(opts)
}

fn proxy_config(p: &ProxyConfig) -> sc::ProxyConfig {
    let addr = |a: &ProxyAddr| {
        let mut out = sc::ProxyAddr::new(a.host.clone(), a.port);
        if let Some(c) = &a.credentials {
            let (u, pw) = c.split_once(':').unwrap_or((c.as_str(), ""));
            out = out.with_credentials(u, pw);
        }
        out
    };
    let mut cfg = sc::ProxyConfig::default();
    cfg.http = p.http.as_ref().map(addr);
    cfg.https = p.https.as_ref().map(addr);
    cfg.no_proxy = p.no_proxy.clone();
    cfg
}

/// The simoom client for `settings`, created on first use and shared afterwards.
pub(crate) fn engine(settings: &EngineSettings<'_>) -> Result<sc::Client> {
    let mut map = engines().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(c) = map.get(&settings.key) {
        return Ok(c.clone());
    }
    let client = build(settings)?;
    // many distinct configurations (a new TLS configuration per call) must not pile up
    if map.len() > 256 {
        map.clear();
    }
    map.insert(settings.key.clone(), client.clone());
    Ok(client)
}

/// Idle pooled connections of the shared clients.
pub fn pooled_connections() -> usize {
    engines().lock().unwrap_or_else(|p| p.into_inner()).values().map(|c| c.pool_stats().idle).sum()
}

/// A new simoom client for `settings` (not shared).
pub(crate) fn build(settings: &EngineSettings<'_>) -> Result<sc::Client> {
    let k = &settings.key;
    let mut cfg = sc::Config::default();
    cfg.connect_timeout = duration(k.connect_ms);
    cfg.read_timeout = duration(k.read_ms);
    cfg.write_timeout = duration(k.read_ms);
    cfg.max_redirects = k.max_redirects;
    cfg.keep_alive = k.keep_alive;
    cfg.pool_max_idle_per_origin = k.per_origin.max(1);
    cfg.version = if k.http2 { sc::HttpVersion::Http2 } else { sc::HttpVersion::Http1 };
    cfg.max_head = k.max_head;
    cfg.max_body = Some(k.max_body);
    // bodies come back as sent: the producer decodes `Content-Encoding` itself when asked to
    cfg.decompress = false;
    cfg.proxy = settings.proxy.map(proxy_config);
    Ok(sc::ClientBuilder::default().config(cfg).tls(tls_options(settings.tls, k.server_name.as_deref(), k.https)?).build())
}

/// A simoom error as the Camel exception the old client raised.
pub(crate) fn camel_error(e: sc::Error, what: &str) -> CamelError {
    camel_error_at(e, what, what)
}

/// Like [`camel_error`]; `authority` (`host:port`) names the peer of connection errors, as the old client did.
pub(crate) fn camel_error_at(e: sc::Error, what: &str, authority: &str) -> CamelError {
    use sc::ErrorKind as K;
    match &e.kind {
        K::Connect | K::Timeout(sc::TimeoutKind::Connect) if e.message.contains("refused") => CamelError::new(kinds::CONNECT, format!("Connection refused: {authority}")),
        K::Connect | K::Timeout(sc::TimeoutKind::Connect) => CamelError::new(kinds::CONNECT, format!("{}", e.message)),
        K::Timeout(_) => CamelError::new(kinds::SOCKET_TIMEOUT, format!("Read timed out: {what}")),
        K::Tls => {
            let m = e.message.to_ascii_lowercase();
            let certificate = m.contains("certificate") || m.contains("verif") || m.contains("untrusted") || m.contains("issuer") || m.contains("expired") || m.contains("hostname") || m.contains("host name");
            let err = if certificate {
                cameleer_tls::TlsError::Certificate(cert_error(&e.message))
            } else {
                // 40 = handshake_failure
                cameleer_tls::TlsError::Protocol { alert: 40, message: e.message.clone() }
            };
            cameleer_tls::camel_support::tls_error(&err, what)
        }
        K::Closed => CamelError::io(format!("Connection closed unexpectedly: {what}")),
        // a TLS server answers plain HTTP with an alert record: the status line is not HTTP at all
        K::Protocol if e.message.contains("invalid response head") && e.message.contains("InvalidVersion") => {
            CamelError::io(format!("Connection closed unexpectedly: the answer is not HTTP (plain http:// sent to a port that speaks TLS?): {what}"))
        }
        K::Status(_) => CamelError::io(format!("{}: {what}", e.message)),
        _ => CamelError::io(format!("{}: {what}", e.message)),
    }
}

/// The certificate failure inside a simoom TLS error message, as the [`cameleer_tls::CertError`] the
/// old client raised (the texts are the same, so the Camel messages and the docs stay true).
fn cert_error(message: &str) -> cameleer_tls::CertError {
    use cameleer_tls::CertError;
    // "... TLS error (Certificate(<detail>)): <peer>" -> <detail>
    let detail = match message.find("Certificate(") {
        Some(i) => {
            let rest = &message[i + "Certificate(".len()..];
            rest.rfind("))").map(|j| &rest[..j]).unwrap_or(rest)
        }
        None => message,
    };
    let quoted = |s: &str| -> Vec<String> { s.split('\'').skip(1).step_by(2).map(str::to_string).collect() };
    if let Some(r) = detail.strip_prefix("unable to find trusted issuer ") {
        let q = quoted(r);
        if q.len() >= 2 {
            return CertError::UnknownIssuer { issuer: q[0].clone(), subject: q[1].clone() };
        }
    }
    if let Some(r) = detail.strip_prefix("certificate is not valid for ") {
        let q = quoted(r);
        if let Some(host) = q.first() {
            let names = r.split("(valid for: ").nth(1).map(|n| n.trim_end_matches(')').split(", ").map(str::to_string).collect()).unwrap_or_default();
            return CertError::HostnameMismatch { host: host.clone(), names };
        }
    }
    if let Some(r) = detail.strip_prefix("certificate expired: ") {
        return CertError::Expired { subject: r.to_string() };
    }
    if let Some(r) = detail.strip_prefix("certificate not yet valid: ") {
        return CertError::NotYetValid { subject: r.to_string() };
    }
    CertError::Parse(detail.to_string())
}

/// The head of a simoom response as a [`HttpResponse`] without a body.
pub(crate) fn head(resp: &sc::Response) -> HttpResponse {
    let mut out = HttpResponse::new(resp.status());
    out.reason = resp.reason().to_string();
    out.version = match resp.version() {
        sc::Version::Http10 => crate::http::Version::Http10,
        _ => crate::http::Version::Http11,
    };
    for (k, v) in resp.headers().iter() {
        out.headers.add(k.to_string(), String::from_utf8_lossy(v).into_owned());
    }
    // a body without a length is chunked (or ends with the connection); the old client reported both so
    out.chunked = resp.content_length().is_none() && !matches!(resp.status(), 100..=199 | 204 | 304);
    out
}

/// What to send: the request line and headers, and the body.
pub(crate) struct Call<'a> {
    pub method: &'a str,
    pub url: &'a Url,
    pub headers: &'a HttpHeaders,
    pub body: sc::Body,
    pub chunked: bool,
    pub follow_redirects: bool,
}

/// Send `call` and return the response with its body still to read.
pub(crate) fn send(client: &sc::Client, call: Call<'_>) -> Result<sc::Response> {
    let what = call.url.to_string();
    let mut req = client.request(call.method, &what).accept_any_status().follow_redirects(call.follow_redirects);
    for (k, v) in call.headers.iter() {
        // the client writes the framing and the connection handling itself
        if k.eq_ignore_ascii_case("content-length") || k.eq_ignore_ascii_case("transfer-encoding") {
            continue;
        }
        // keep-alive is the client's business, but `Connection: close` is the caller's to ask for
        if k.eq_ignore_ascii_case("connection") && !v.trim().eq_ignore_ascii_case("close") {
            continue;
        }
        req = req.header(k, v);
    }
    req = req.body(call.body);
    if call.chunked {
        req = req.chunked(true);
    }
    req.send().map_err(|e| camel_error_at(e, &what, &call.url.authority()))
}

/// Read a response body completely, up to the client's `max_body`.
pub(crate) fn read_all(resp: sc::Response, what: &str) -> Result<Vec<u8>> {
    resp.bytes().map_err(|e| camel_error(e, what))
}

/// A response body as a `Read`, for a lazy stream.
pub(crate) fn reader(resp: sc::Response) -> Box<dyn Read + Send> {
    Box::new(resp.into_body())
}

pub(crate) fn key(
    connect: Duration,
    read: Duration,
    max_redirects: usize,
    keep_alive: bool,
    per_origin: usize,
    http2: bool,
    https: bool,
    max_head: usize,
    max_body: usize,
    tls: Option<&Arc<TlsClientConfig>>,
    server_name: Option<&str>,
    proxy: Option<&ProxyConfig>,
) -> EngineKey {
    EngineKey {
        connect_ms: connect.as_millis() as u64,
        read_ms: read.as_millis() as u64,
        max_redirects,
        keep_alive,
        per_origin,
        http2,
        https,
        max_head,
        max_body: max_body as u64,
        tls: tls.map(|t| Arc::as_ptr(t) as usize).unwrap_or(0),
        server_name: server_name.map(str::to_string),
        proxy: proxy.map(|p| format!("{p:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simoom_certificate_errors_become_the_old_camel_messages() {
        let m = |t: &str| cert_error(t).to_string();
        assert_eq!(
            m("TLS error (Certificate(unable to find trusted issuer 'CN=ca' for 'CN=leaf')): https://h:1"),
            "unable to find trusted issuer 'CN=ca' for 'CN=leaf'"
        );
        assert_eq!(
            m("TLS error (Certificate(certificate is not valid for 'wrong.example' (valid for: localhost, 127.0.0.1)))"),
            "certificate is not valid for 'wrong.example' (valid for: localhost, 127.0.0.1)"
        );
        assert_eq!(m("TLS error (Certificate(certificate expired: CN=leaf))"), "certificate expired: CN=leaf");
        assert_eq!(m("something else"), "certificate parse error: something else");
    }
}

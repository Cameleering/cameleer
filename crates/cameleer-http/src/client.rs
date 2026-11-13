//! HTTP/1.1 client for `http://` and `https://` URLs (TLS via `cameleer-tls`, with SNI
//! and hostname verification), with optional HTTP proxy support (absolute-form
//! requests for `http`, `CONNECT` tunnels for `https`, `NO_PROXY` exclusions).
//!
//! ```no_run
//! use cameleer_http::client::HttpClient;
//! use std::time::Duration;
//! let client = HttpClient::new();
//! let resp = client.request("GET", "http://localhost:8080/hello", &[("Accept", "text/plain")], None, Some(Duration::from_secs(5))).unwrap();
//! println!("{} {}", resp.status, resp.body_text());
//! ```

use std::io::{ErrorKind, Read};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use cameleer_tls::TlsClientConfig;

use cameleer_core::error::{kinds, CamelError, Result};

use crate::client_engine as engine;
use crate::http::{HttpHeaders, HttpResponse, Limits};

/// A parsed `http://` or `https://` URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    /// Raw path (always starts with `/`).
    pub path: String,
    /// Raw query (without `?`).
    pub query: Option<String>,
}

impl Url {
    /// Parse an absolute `http[s]://host[:port][/path][?query]` URL.
    pub fn parse(url: &str) -> Result<Url> {
        let url = url.trim();
        let (scheme, rest) = url
            .split_once("://")
            .ok_or_else(|| CamelError::illegal_argument(format!("Invalid URL (missing scheme): {}", url)))?;
        let scheme = scheme.to_ascii_lowercase();
        if scheme != "http" && scheme != "https" {
            return Err(CamelError::illegal_argument(format!("Unsupported URL scheme '{}': {}", scheme, url)));
        }
        let rest = rest.split('#').next().unwrap_or("");
        let split = rest.find(['/', '?']).unwrap_or(rest.len());
        let (authority, path_query) = rest.split_at(split);
        let authority = authority.rsplit('@').next().unwrap_or(authority);
        let default_port = if scheme == "https" { 443 } else { 80 };
        let (host, port) = parse_authority(authority, default_port).map_err(|m| CamelError::illegal_argument(format!("{}: {}", m, url)))?;
        let (path, query) = match path_query.find('?') {
            Some(i) => (&path_query[..i], Some(path_query[i + 1..].to_string())),
            None => (path_query, None),
        };
        let path = if path.is_empty() { "/".to_string() } else { path.to_string() };
        Ok(Url { scheme, host, port, path, query })
    }

    /// `host:port` (IPv6 hosts in brackets).
    pub fn authority(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    /// Value for the `Host` header.
    pub fn host_header(&self) -> String {
        let h = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        if self.port == self.default_port() {
            h
        } else {
            format!("{}:{}", h, self.port)
        }
    }

    /// Whether the scheme is `https`.
    pub fn is_https(&self) -> bool {
        self.scheme == "https"
    }

    /// 443 for `https`, 80 otherwise.
    pub fn default_port(&self) -> u16 {
        if self.is_https() {
            443
        } else {
            80
        }
    }

    /// Path plus query (`/a/b?x=1`).
    pub fn request_target(&self) -> String {
        match &self.query {
            Some(q) if !q.is_empty() => encode_target(&format!("{}?{}", self.path, q)),
            _ => encode_target(&self.path),
        }
    }

    /// Resolve a (possibly relative) reference such as a `Location` header.
    pub fn join(&self, reference: &str) -> Result<Url> {
        if reference.contains("://") {
            return Url::parse(reference);
        }
        let mut u = self.clone();
        if let Some(r) = reference.strip_prefix("//") {
            return Url::parse(&format!("{}://{}", self.scheme, r));
        }
        let (p, q) = match reference.find('?') {
            Some(i) => (&reference[..i], Some(reference[i + 1..].to_string())),
            None => (reference, None),
        };
        if p.starts_with('/') {
            u.path = p.to_string();
        } else if !p.is_empty() {
            let base = match self.path.rfind('/') {
                Some(i) => &self.path[..=i],
                None => "/",
            };
            u.path = format!("{}{}", base, p);
        }
        u.query = q;
        Ok(u)
    }
}

/// Percent-encodes the bytes a request-target may not contain (RFC 9112 3.2: space, controls,
/// `"` `<` `>` `\` `^` `` ` `` `{` `|` `}` `[` `]` and anything outside ASCII), and leaves `%` and
/// everything else alone, so an already encoded target is unchanged. Strict servers (Tomcat,
/// simoom, Envoy) refuse the raw characters; curl and Java's client encode them as well.
/// The ASCII bytes (besides space and controls) that a request-target may not contain.
const UNSAFE_IN_TARGET: &[u8] = &[b'"', b'<', b'>', b'\\', b'^', b'`', b'{', b'|', b'}', b'[', b']'];

fn encode_target(target: &str) -> String {
    if target.bytes().all(|b| (0x21..0x7f).contains(&b) && !UNSAFE_IN_TARGET.contains(&b)) {
        return target.to_string();
    }
    let mut out = String::with_capacity(target.len() + 8);
    for b in target.bytes() {
        if (0x21..0x7f).contains(&b) && !UNSAFE_IN_TARGET.contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

impl std::fmt::Display for Url {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}://{}{}", self.scheme, self.host_header(), self.request_target())
    }
}

/// Parse `host[:port]` / `[v6][:port]`.
pub fn parse_authority(authority: &str, default_port: u16) -> std::result::Result<(String, u16), String> {
    if authority.is_empty() {
        return Err("Missing host".into());
    }
    if let Some(rest) = authority.strip_prefix('[') {
        let end = rest.find(']').ok_or("Invalid IPv6 host")?;
        let host = rest[..end].to_string();
        let port = match rest[end + 1..].strip_prefix(':') {
            Some(p) => p.parse().map_err(|_| format!("Invalid port '{}'", p))?,
            None => default_port,
        };
        return Ok((host, port));
    }
    match authority.rsplit_once(':') {
        Some((h, p)) => Ok((h.to_string(), p.parse().map_err(|_| format!("Invalid port '{}'", p))?)),
        None => Ok((authority.to_string(), default_port)),
    }
}

pub(crate) fn map_io_error(e: std::io::Error, what: &str) -> CamelError {
    if let Some(t) = cameleer_tls::camel_support::tls_io_error(&e, what) {
        return t;
    }
    match e.kind() {
        ErrorKind::ConnectionRefused => CamelError::new(kinds::CONNECT, format!("Connection refused: {}", what)),
        ErrorKind::TimedOut | ErrorKind::WouldBlock => CamelError::new(kinds::SOCKET_TIMEOUT, format!("Read timed out: {}", what)),
        _ => CamelError::io(format!("{}: {}", e, what)),
    }
}

/// An HTTP proxy address (`http://[user:pass@]host:port`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyAddr {
    pub host: String,
    pub port: u16,
    /// `user:password` for `Proxy-Authorization: Basic`.
    pub credentials: Option<String>,
}

impl ProxyAddr {
    /// Parse `host:port`, `http://host:port` or `http://user:pass@host:port/`.
    pub fn parse(s: &str) -> Result<ProxyAddr> {
        let s = s.trim();
        let rest = match s.split_once("://") {
            Some((scheme, r)) => {
                if !scheme.eq_ignore_ascii_case("http") {
                    return Err(CamelError::illegal_argument(format!("Unsupported proxy scheme '{}' (only http proxies are supported): {}", scheme, s)));
                }
                r
            }
            None => s,
        };
        let authority = rest.split('/').next().unwrap_or(rest);
        let (credentials, hostport) = match authority.rsplit_once('@') {
            Some((c, h)) => (Some(crate::http::percent_decode(c, false)), h),
            None => (None, authority),
        };
        let (host, port) = parse_authority(hostport, 80).map_err(|m| CamelError::illegal_argument(format!("Invalid proxy '{}': {}", s, m)))?;
        Ok(ProxyAddr { host, port, credentials })
    }
}

/// Proxy settings: separate proxies for `http` and `https` targets plus `NO_PROXY`
/// exclusions (host names, `.domain` suffixes, `*`, IP addresses and CIDR ranges).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProxyConfig {
    pub http: Option<ProxyAddr>,
    pub https: Option<ProxyAddr>,
    pub no_proxy: Vec<String>,
}

impl ProxyConfig {
    /// One proxy for both schemes.
    pub fn new(proxy: ProxyAddr) -> ProxyConfig {
        ProxyConfig { http: Some(proxy.clone()), https: Some(proxy), no_proxy: Vec::new() }
    }

    /// Adds `NO_PROXY`-style exclusions (comma separated).
    pub fn with_no_proxy(mut self, list: &str) -> ProxyConfig {
        self.no_proxy.extend(list.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()));
        self
    }

    /// Reads `HTTPS_PROXY`/`https_proxy`, `HTTP_PROXY`/`http_proxy` and `NO_PROXY`/`no_proxy`
    /// from the process environment (`None` when no proxy variable is set).
    pub fn from_env() -> Option<ProxyConfig> {
        ProxyConfig::from_vars(&|k| std::env::var(k).ok())
    }

    /// Like [`ProxyConfig::from_env`] with a custom variable lookup.
    pub fn from_vars(get: &dyn Fn(&str) -> Option<String>) -> Option<ProxyConfig> {
        let var = |names: &[&str]| names.iter().find_map(|n| get(n)).filter(|v| !v.trim().is_empty());
        let https = var(&["HTTPS_PROXY", "https_proxy"]).and_then(|v| ProxyAddr::parse(&v).ok());
        let http = var(&["HTTP_PROXY", "http_proxy"]).and_then(|v| ProxyAddr::parse(&v).ok());
        if http.is_none() && https.is_none() {
            return None;
        }
        let cfg = ProxyConfig { http, https, no_proxy: Vec::new() };
        Some(match var(&["NO_PROXY", "no_proxy"]) {
            Some(n) => cfg.with_no_proxy(&n),
            None => cfg,
        })
    }

    /// Whether `host` matches a `NO_PROXY` entry.
    pub fn bypass(&self, host: &str) -> bool {
        let host = host.trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase();
        let ip: Option<std::net::IpAddr> = host.parse().ok();
        self.no_proxy.iter().any(|entry| {
            let e = entry.trim().to_ascii_lowercase();
            if e == "*" {
                return true;
            }
            if let Some((net, bits)) = e.split_once('/') {
                return ip.map(|ip| cidr_contains(net, bits, ip)).unwrap_or(false);
            }
            let e = e.trim_start_matches("*.");
            let e = e.strip_prefix('.').unwrap_or(e);
            // entries may carry a port (host:port); compare the host part
            let e = if e.parse::<std::net::IpAddr>().is_ok() { e } else { e.split(':').next().unwrap_or(e) };
            host == e || host.ends_with(&format!(".{}", e))
        })
    }

    /// The proxy to use for `url`, if any.
    pub fn proxy_for(&self, url: &Url) -> Option<&ProxyAddr> {
        let p = if url.is_https() { self.https.as_ref() } else { self.http.as_ref() };
        p.filter(|_| !self.bypass(&url.host))
    }
}

fn cidr_contains(net: &str, bits: &str, ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    let (Ok(net), Ok(bits)) = (net.parse::<IpAddr>(), bits.parse::<u32>()) else { return false };
    match (net, ip) {
        (IpAddr::V4(n), IpAddr::V4(i)) if bits <= 32 => {
            let mask = if bits == 0 { 0 } else { u32::MAX << (32 - bits) };
            u32::from(n) & mask == u32::from(i) & mask
        }
        (IpAddr::V6(n), IpAddr::V6(i)) if bits <= 128 => {
            let mask = if bits == 0 { 0 } else { u128::MAX << (128 - bits) };
            u128::from(n) & mask == u128::from(i) & mask
        }
        _ => false,
    }
}

/// The default TLS client configuration (system trust store, full verification).
pub fn default_tls_config() -> Arc<TlsClientConfig> {
    static CFG: OnceLock<Arc<TlsClientConfig>> = OnceLock::new();
    CFG.get_or_init(|| Arc::new(TlsClientConfig::builder().alpn_protocols(&["http/1.1"]).build())).clone()
}

/// The body of a request.
pub enum RequestBody<'a> {
    None,
    /// In memory (chunked on the wire when [`HttpClient::chunked_requests`] is set).
    Bytes(&'a [u8]),
    /// Streamed; sent with `Content-Length` when the length is known, otherwise chunked.
    Stream(Box<dyn Read + Send>, Option<u64>),
}

/// HTTP client settings, on simoom-client: HTTP/1.1 and HTTP/2, TLS with full verification (it
/// cannot be turned off), proxies, redirects. Requests with the same settings share connections
/// when [`HttpClient::keep_alive`] is on; otherwise each request uses a new connection
/// (`Connection: close`). Use [`HttpConnection`] for one persistent connection.
#[derive(Clone, Debug)]
pub struct HttpClient {
    pub connect_timeout: Duration,
    /// Default socket read/write timeout (overridden by the per-request timeout).
    pub read_timeout: Duration,
    pub follow_redirects: bool,
    pub max_redirects: usize,
    pub limits: Limits,
    /// Send request bodies with chunked transfer coding.
    pub chunked_requests: bool,
    /// TLS settings for `https` URLs (default: system trust store, full verification).
    pub tls: Option<Arc<TlsClientConfig>>,
    /// Overrides the TLS server name (SNI and certificate hostname check).
    pub tls_server_name: Option<String>,
    /// Proxy settings (none by default; see [`ProxyConfig::from_env`]).
    pub proxy: Option<ProxyConfig>,
    /// Reuse connections (HTTP/1.1 keep-alive, HTTP/2 multiplexing) instead of one connection per request.
    pub keep_alive: bool,
    /// Speak HTTP/2 (prior knowledge for `http`, ALPN for `https`).
    pub http2: bool,
    /// Idle connections kept per origin when `keep_alive` is on.
    pub connections_per_route: usize,
}

impl Default for HttpClient {
    fn default() -> Self {
        HttpClient {
            connect_timeout: Duration::from_secs(30),
            read_timeout: Duration::from_secs(60),
            follow_redirects: false,
            max_redirects: 10,
            limits: Limits { max_body_bytes: 256 * 1024 * 1024, ..Limits::default() },
            chunked_requests: false,
            tls: None,
            tls_server_name: None,
            proxy: None,
            keep_alive: false,
            http2: false,
            connections_per_route: 20,
        }
    }
}

impl HttpClient {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reuse connections (see [`HttpClient::keep_alive`]).
    pub fn with_keep_alive(mut self, on: bool) -> Self {
        self.keep_alive = on;
        self
    }

    /// Uses `config` for `https` URLs.
    pub fn with_tls(mut self, config: TlsClientConfig) -> Self {
        self.tls = Some(Arc::new(config));
        self
    }

    /// Uses the given proxy settings.
    pub fn with_proxy(mut self, proxy: ProxyConfig) -> Self {
        self.proxy = Some(proxy);
        self
    }

    /// Uses the proxy settings from `HTTPS_PROXY` / `HTTP_PROXY` / `NO_PROXY`.
    pub fn with_env_proxy(mut self) -> Self {
        self.proxy = ProxyConfig::from_env();
        self
    }

    /// Perform a request. `timeout` overrides the socket read timeout.
    pub fn request(&self, method: &str, url: &str, headers: &[(&str, &str)], body: Option<&[u8]>, timeout: Option<Duration>) -> Result<HttpResponse> {
        let url = Url::parse(url)?;
        let h: HttpHeaders = headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        self.execute(method, &url, &h, body, timeout)
    }

    /// `GET` convenience.
    pub fn get(&self, url: &str) -> Result<HttpResponse> {
        self.request("GET", url, &[], None, None)
    }

    /// `POST` convenience.
    pub fn post(&self, url: &str, content_type: &str, body: &[u8]) -> Result<HttpResponse> {
        self.request("POST", url, &[("Content-Type", content_type)], Some(body), None)
    }

    fn engine(&self, read: Duration, https: bool) -> Result<simoom_client::Client> {
        let key = engine::key(
            self.connect_timeout,
            read,
            self.max_redirects,
            self.keep_alive,
            self.connections_per_route,
            self.http2,
            https || self.follow_redirects,
            self.limits.max_header_bytes,
            self.limits.max_body_bytes,
            self.tls.as_ref(),
            self.tls_server_name.as_deref(),
            self.proxy.as_ref(),
        );
        engine::engine(&engine::EngineSettings { key, tls: self.tls.as_deref(), proxy: self.proxy.as_ref() })
    }

    /// Perform a request against a parsed URL, following redirects if enabled. The whole response
    /// body is read.
    pub fn execute(&self, method: &str, url: &Url, headers: &HttpHeaders, body: Option<&[u8]>, timeout: Option<Duration>) -> Result<HttpResponse> {
        let what = url.to_string();
        let (mut head, mut reader) = self.execute_streaming(method, url, headers, body.map_or(RequestBody::None, RequestBody::Bytes), timeout)?;
        let mut all = Vec::new();
        reader.read_to_end(&mut all).map_err(|e| map_io_error(e, &what))?;
        head.body = all;
        Ok(head)
    }

    /// Perform a request and return the response head with its body as a reader that is read as
    /// it arrives (event streams, large downloads), and optionally stream the request body.
    pub fn execute_streaming(
        &self,
        method: &str,
        url: &Url,
        headers: &HttpHeaders,
        body: RequestBody<'_>,
        timeout: Option<Duration>,
    ) -> Result<(HttpResponse, Box<dyn Read + Send>)> {
        let read = timeout.unwrap_or(self.read_timeout);
        let client = self.engine(read, url.is_https())?;
        let chunked = self.chunked_requests && matches!(body, RequestBody::Bytes(_));
        let body = match body {
            RequestBody::None => simoom_client::Body::Empty,
            RequestBody::Bytes(b) => simoom_client::Body::Bytes(b.to_vec()),
            RequestBody::Stream(r, len) => simoom_client::Body::Reader { reader: r, len },
        };
        let resp = engine::send(&client, engine::Call { method: &method.to_ascii_uppercase(), url, headers, body, chunked, follow_redirects: self.follow_redirects })?;
        let head = engine::head(&resp);
        Ok((head, engine::reader(resp)))
    }
}

/// Perform a single request with default client settings.
pub fn request(method: &str, url: &str, headers: &[(&str, &str)], body: Option<&[u8]>, timeout: Option<Duration>) -> Result<HttpResponse> {
    HttpClient::new().request(method, url, headers, body, timeout)
}

/// Idle keep-alive connections of every client (for tests and diagnostics).
pub fn pooled_connections() -> usize {
    engine::pooled_connections()
}

/// A persistent (keep-alive) client connection to one server: requests go over the same connection
/// until the server closes it.
pub struct HttpConnection {
    client: simoom_client::Client,
    base: String,
    host_header: String,
    tls: bool,
    closed: bool,
}

impl HttpConnection {
    /// Connect to `host:port` (plain http).
    pub fn connect(host: &str, port: u16, timeout: Duration) -> Result<HttpConnection> {
        HttpConnection::connect_with(host, port, None, timeout)
    }

    /// Connect to `host:port`, over TLS when `tls` is given (SNI/verification use `host`). The
    /// connection is opened by the first request.
    pub fn connect_with(host: &str, port: u16, tls: Option<&TlsClientConfig>, timeout: Duration) -> Result<HttpConnection> {
        let key = engine::key(timeout, timeout, 10, true, 1, false, tls.is_some(), 64 * 1024, 256 * 1024 * 1024, None, None, None);
        let client = engine::build(&engine::EngineSettings { key, tls, proxy: None })?;
        let scheme = if tls.is_some() { "https" } else { "http" };
        let authority = if host.contains(':') { format!("[{}]:{}", host, port) } else { format!("{}:{}", host, port) };
        Ok(HttpConnection { client, base: format!("{}://{}", scheme, authority), host_header: authority, tls: tls.is_some(), closed: false })
    }

    /// Send a request on this connection and read the response.
    pub fn send(&mut self, method: &str, target: &str, headers: &[(&str, &str)], body: Option<&[u8]>) -> Result<HttpResponse> {
        if self.closed {
            return Err(CamelError::io("Connection closed by server"));
        }
        let url = Url::parse(&format!("{}{}", self.base, target))?;
        let mut h: HttpHeaders = headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        if !h.contains("Host") {
            h.set("Host", self.host_header.clone());
        }
        let what = target.to_string();
        let sent = body.map_or(simoom_client::Body::Empty, |b| simoom_client::Body::Bytes(b.to_vec()));
        let resp = engine::send(&self.client, engine::Call { method: &method.to_ascii_uppercase(), url: &url, headers: &h, body: sent, chunked: false, follow_redirects: false })?;
        let mut head = engine::head(&resp);
        head.body = engine::read_all(resp, &what)?;
        if head.headers.has_token("Connection", "close") {
            self.closed = true;
        }
        Ok(head)
    }

    /// Whether the server signalled that the connection will be closed.
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Whether the connection uses TLS.
    pub fn is_tls(&self) -> bool {
        self.tls
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_targets_are_percent_encoded_for_strict_servers() {
        // a FHIR search: `|` is not allowed in a request-target (Tomcat, simoom and Envoy refuse it)
        let u = Url::parse("http://h/Observation?subject=Patient/1&code=http://loinc.org|29463-7").unwrap();
        assert_eq!(u.request_target(), "/Observation?subject=Patient/1&code=http://loinc.org%7C29463-7");
        assert_eq!(encode_target("/a b?x=[1]&y={z}"), "/a%20b?x=%5B1%5D&y=%7Bz%7D");
        // non-ASCII goes out as UTF-8 bytes; an already encoded target is unchanged
        assert_eq!(encode_target("/caf\u{e9}"), "/caf%C3%A9");
        assert_eq!(encode_target("/a%7Cb?q=%20"), "/a%7Cb?q=%20");
    }

    #[test]
    fn parse_urls() {
        let u = Url::parse("http://example.com:8080/a/b?x=1").unwrap();
        assert_eq!((u.host.as_str(), u.port, u.path.as_str(), u.query.as_deref()), ("example.com", 8080, "/a/b", Some("x=1")));
        let u = Url::parse("http://[::1]/").unwrap();
        assert_eq!(u.host, "::1");
        assert_eq!(u.port, 80);
        assert_eq!(u.authority(), "[::1]:80");
        let u = Url::parse("http://h?q").unwrap();
        assert_eq!(u.request_target(), "/?q");
        let u = Url::parse("https://h/").unwrap();
        assert_eq!((u.port, u.host_header()), (443, "h".to_string()));
        assert_eq!(Url::parse("https://h:8443/").unwrap().host_header(), "h:8443");
        let base = Url::parse("http://h:1/a/b").unwrap();
        assert_eq!(base.join("c").unwrap().path, "/a/c");
        assert_eq!(base.join("/z?q=1").unwrap().request_target(), "/z?q=1");
        assert_eq!(base.join("http://o/p").unwrap().host, "o");
    }

    #[test]
    fn proxy_config_and_no_proxy() {
        let vars = |k: &str| match k {
            "HTTPS_PROXY" => Some("http://u:p%40ss@proxy.local:3128".to_string()),
            "no_proxy" => Some("localhost,.internal,10.0.0.0/8,example.org:443".to_string()),
            _ => None,
        };
        let cfg = ProxyConfig::from_vars(&vars).unwrap();
        let p = cfg.https.clone().unwrap();
        assert_eq!((p.host.as_str(), p.port, p.credentials.as_deref()), ("proxy.local", 3128, Some("u:p@ss")));
        assert!(cfg.http.is_none());
        assert!(cfg.bypass("localhost") && cfg.bypass("a.internal") && cfg.bypass("10.1.2.3") && cfg.bypass("example.org"));
        assert!(!cfg.bypass("example.com") && !cfg.bypass("11.0.0.1"));
        assert!(cfg.proxy_for(&Url::parse("https://example.com/").unwrap()).is_some());
        assert!(cfg.proxy_for(&Url::parse("http://example.com/").unwrap()).is_none());
        assert!(cfg.proxy_for(&Url::parse("https://svc.internal/").unwrap()).is_none());
        assert!(ProxyConfig::from_vars(&|_| None).is_none());
    }

    #[test]
    fn connection_refused_maps_to_connect_exception() {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let err = HttpClient::new().get(&format!("http://127.0.0.1:{}/", port)).unwrap_err();
        assert_eq!(err.kind, kinds::CONNECT);
    }
}

/// An HTTP client configured from endpoint options, for producers of components that call
/// HTTP APIs (FHIR, SOAP, REST): TLS from `sslContextParameters`, timeouts, basic auth.
#[derive(Clone)]
pub struct EndpointClient {
    pub http: HttpClient,
    pub basic: Option<(String, String)>,
    pub timeout: Duration,
}

impl EndpointClient {
    /// Build from endpoint options: `sslContextParameters`, `receiveTimeout` /
    /// `connectionTimeout` (ms), `username` / `password` (HTTP basic auth).
    pub fn from_params(ctx: &cameleer_core::context::CamelContext, scheme: &str, params: &mut cameleer_core::util::Parameters) -> Result<EndpointClient> {
        let ssl = cameleer_tls::camel_support::take_endpoint_ssl(ctx, params, scheme)?;
        let mut http = HttpClient::new();
        if ssl.is_some() {
            http = http.with_tls(cameleer_tls::camel_support::client_config(ctx, ssl.as_ref())?);
        }
        let timeout = Duration::from_millis(params.take_u64("receiveTimeout", 60_000)?);
        http.connect_timeout = Duration::from_millis(params.take_u64("connectionTimeout", 30_000)?);
        http.read_timeout = timeout;
        let basic = match (params.take("username"), params.take("password")) {
            (Some(u), p) => Some((u, p.unwrap_or_default())),
            _ => None,
        };
        Ok(EndpointClient { http, basic, timeout })
    }

    /// Send a request.
    pub fn send(&self, method: &str, url: &str, headers: &[(String, String)], body: Option<&[u8]>) -> Result<HttpResponse> {
        let u = Url::parse(url)?;
        let mut h = HttpHeaders::new();
        for (k, v) in headers {
            h.set(k.clone(), v.clone());
        }
        if let Some((user, pw)) = &self.basic {
            h.set("Authorization", format!("Basic {}", cameleer_tls::pem::base64_encode(format!("{}:{}", user, pw).as_bytes())));
        }
        self.http.execute(method, &u, &h, body, Some(self.timeout))
    }
}

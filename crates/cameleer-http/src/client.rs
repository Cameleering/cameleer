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

use std::io::{BufReader, ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use cameleer_tls::net::NetStream;
use cameleer_tls::TlsClientConfig;

use cameleer_core::error::{kinds, CamelError, Result};

use crate::http::{read_response, response_keep_alive, write_request, HttpError, HttpHeaders, HttpResponse, Limits};

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

fn map_http_error(e: HttpError, what: &str) -> CamelError {
    match e {
        HttpError::Io(e) => map_io_error(e, what),
        HttpError::Timeout => CamelError::new(kinds::SOCKET_TIMEOUT, format!("Read timed out: {}", what)),
        other => CamelError::io(format!("{}: {}", other, what)),
    }
}

/// Connect to `host:port` with a timeout, trying each resolved address.
pub fn connect(host: &str, port: u16, timeout: Duration) -> Result<TcpStream> {
    let what = format!("{}:{}", host, port);
    let addrs: Vec<SocketAddr> = (host, port).to_socket_addrs().map_err(|e| map_io_error(e, &what))?.collect();
    let mut last = None;
    for a in addrs {
        match TcpStream::connect_timeout(&a, timeout) {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    Err(match last {
        Some(e) if e.kind() == ErrorKind::TimedOut => CamelError::new(kinds::CONNECT, format!("Connect timed out: {}", what)),
        Some(e) => map_io_error(e, &what),
        None => CamelError::new(kinds::CONNECT, format!("Unknown host: {}", host)),
    })
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

/// Opens a connection to the target of `url`: directly or through `proxy`
/// (a `CONNECT` tunnel for `https`), then performs the TLS handshake for `https`.
/// Returns the stream and whether requests must use the absolute form (plain http via proxy).
pub fn open_stream(
    url: &Url,
    tls: Option<&TlsClientConfig>,
    sni: Option<&str>,
    proxy: Option<&ProxyAddr>,
    connect_timeout: Duration,
    io_timeout: Option<Duration>,
) -> Result<(NetStream, bool)> {
    let what = url.to_string();
    let tcp = match proxy {
        Some(p) => connect(&p.host, p.port, connect_timeout)?,
        None => connect(&url.host, url.port, connect_timeout)?,
    };
    let _ = tcp.set_read_timeout(io_timeout);
    let _ = tcp.set_write_timeout(io_timeout);
    let _ = tcp.set_nodelay(true);
    if !url.is_https() {
        return Ok((NetStream::plain(tcp), proxy.is_some()));
    }
    if let Some(p) = proxy {
        connect_tunnel(&tcp, &url.authority(), p).map_err(|e| match e {
            TunnelError::Io(e) => map_io_error(e, &format!("proxy {}:{}", p.host, p.port)),
            TunnelError::Refused(msg) => CamelError::io(format!("Proxy CONNECT to {} failed: {}", url.authority(), msg)),
        })?;
    }
    let default_cfg;
    let cfg = match tls {
        Some(c) => c,
        None => {
            default_cfg = default_tls_config();
            &default_cfg
        }
    };
    let name = sni.unwrap_or(&url.host);
    let s = NetStream::client(tcp, name, cfg).map_err(|e| map_io_error(e, &what))?;
    Ok((s, false))
}

enum TunnelError {
    Io(std::io::Error),
    Refused(String),
}

/// Sends `CONNECT authority` to the proxy and consumes its response head byte by
/// byte (nothing past the blank line is read, so the TLS handshake starts cleanly).
fn connect_tunnel(tcp: &TcpStream, authority: &str, proxy: &ProxyAddr) -> std::result::Result<(), TunnelError> {
    let mut req = format!("CONNECT {} HTTP/1.1\r\nHost: {}\r\n", authority, authority);
    if let Some(c) = &proxy.credentials {
        req.push_str(&format!("Proxy-Authorization: Basic {}\r\n", crate::http::base64_encode(c.as_bytes())));
    }
    req.push_str("\r\n");
    let mut w = tcp;
    w.write_all(req.as_bytes()).map_err(TunnelError::Io)?;
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    let mut r = tcp;
    while !head.ends_with(b"\r\n\r\n") && !head.ends_with(b"\n\n") {
        if r.read(&mut b).map_err(TunnelError::Io)? == 0 {
            return Err(TunnelError::Refused("proxy closed the connection".into()));
        }
        head.push(b[0]);
        if head.len() > 64 * 1024 {
            return Err(TunnelError::Refused("proxy response too large".into()));
        }
    }
    let text = String::from_utf8_lossy(&head);
    let status_line = text.lines().next().unwrap_or("").trim().to_string();
    let code: u16 = status_line.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    if (200..300).contains(&code) {
        Ok(())
    } else {
        Err(TunnelError::Refused(status_line))
    }
}

/// HTTP client settings. Each request uses a new connection (`Connection: close`);
/// use [`HttpConnection`] for keep-alive.
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
    /// Reuse connections from a process-wide pool (HTTP/1.1 keep-alive) instead of one
    /// connection per request. Requests through a proxy, with a custom TLS server name or with
    /// chunked bodies still use their own connection.
    pub keep_alive: bool,
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
        }
    }
}

impl HttpClient {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reuse pooled keep-alive connections (see [`HttpClient::keep_alive`]).
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

    /// Perform a request against a parsed URL, following redirects if enabled.
    pub fn execute(&self, method: &str, url: &Url, headers: &HttpHeaders, body: Option<&[u8]>, timeout: Option<Duration>) -> Result<HttpResponse> {
        let mut method = method.to_ascii_uppercase();
        let mut url = url.clone();
        let mut body = body.map(|b| b.to_vec());
        let mut redirects = 0;
        loop {
            let resp = self.execute_once(&method, &url, headers, body.as_deref(), timeout)?;
            if self.follow_redirects && matches!(resp.status, 301 | 302 | 303 | 307 | 308) && redirects < self.max_redirects {
                if let Some(loc) = resp.headers.get("Location") {
                    url = url.join(loc)?;
                    redirects += 1;
                    if matches!(resp.status, 301..=303) && method != "HEAD" {
                        method = "GET".into();
                        body = None;
                    }
                    continue;
                }
            }
            return Ok(resp);
        }
    }

    fn execute_once(&self, method: &str, url: &Url, headers: &HttpHeaders, body: Option<&[u8]>, timeout: Option<Duration>) -> Result<HttpResponse> {
        let what = url.to_string();
        let t = timeout.unwrap_or(self.read_timeout);
        let t = if t.is_zero() { None } else { Some(t) };
        let proxy = self.proxy.as_ref().and_then(|p| p.proxy_for(url));
        if self.keep_alive && proxy.is_none() && self.tls_server_name.is_none() && !(self.chunked_requests && body.is_some()) {
            return self.execute_pooled(method, url, headers, body, t);
        }
        let (stream, absolute) = open_stream(url, self.tls.as_deref(), self.tls_server_name.as_deref(), proxy, self.connect_timeout, t)?;
        let mut h = headers.clone();
        if !h.contains("Host") {
            h.set("Host", url.host_header());
        }
        h.set("Connection", "close");
        let target = if absolute {
            if let Some(c) = proxy.and_then(|p| p.credentials.as_ref()) {
                h.set("Proxy-Authorization", format!("Basic {}", crate::http::base64_encode(c.as_bytes())));
            }
            url.to_string()
        } else {
            url.request_target()
        };
        let mut buf = Vec::new();
        write_request(&mut buf, method, &target, &h, body, self.chunked_requests && body.is_some()).map_err(|e| map_io_error(e, &what))?;
        (&stream).write_all(&buf).map_err(|e| map_io_error(e, &what))?;
        let mut r = BufReader::new(&stream);
        let resp = read_response(&mut r, method, &self.limits).map_err(|e| map_http_error(e, &what))?;
        let _ = stream.close_notify();
        Ok(resp)
    }
}

/// Perform a single request with default client settings.
pub fn request(method: &str, url: &str, headers: &[(&str, &str)], body: Option<&[u8]>, timeout: Option<Duration>) -> Result<HttpResponse> {
    HttpClient::new().request(method, url, headers, body, timeout)
}

/// Idle keep-alive connections, by origin and TLS configuration.
struct Pool {
    idle: Mutex<HashMap<String, Vec<(Instant, HttpConnection)>>>,
}

/// Connections idle longer than this are closed (servers commonly drop them after 60 s).
const POOL_IDLE: Duration = Duration::from_secs(30);
/// Idle connections kept per origin.
const POOL_MAX_IDLE: usize = 16;

fn pool() -> &'static Pool {
    static POOL: OnceLock<Pool> = OnceLock::new();
    POOL.get_or_init(|| Pool { idle: Mutex::new(HashMap::new()) })
}

impl Pool {
    fn take(&self, key: &str) -> Option<HttpConnection> {
        let mut idle = self.idle.lock().unwrap_or_else(|p| p.into_inner());
        let list = idle.get_mut(key)?;
        while let Some((at, c)) = list.pop() {
            if at.elapsed() < POOL_IDLE && !c.is_closed() {
                return Some(c);
            }
        }
        None
    }
    fn put(&self, key: String, c: HttpConnection) {
        let mut idle = self.idle.lock().unwrap_or_else(|p| p.into_inner());
        let list = idle.entry(key).or_default();
        list.retain(|(at, _)| at.elapsed() < POOL_IDLE);
        if list.len() < POOL_MAX_IDLE {
            list.push((Instant::now(), c));
        }
    }
}

/// Connections in the keep-alive pool (for tests and diagnostics).
pub fn pooled_connections() -> usize {
    pool().idle.lock().unwrap_or_else(|p| p.into_inner()).values().map(|v| v.len()).sum()
}

impl HttpClient {
    /// A request on a pooled connection. A reused connection the server already closed is
    /// replaced once (the request had not reached the server); timeouts are never retried.
    fn execute_pooled(&self, method: &str, url: &Url, headers: &HttpHeaders, body: Option<&[u8]>, t: Option<Duration>) -> Result<HttpResponse> {
        let default_cfg;
        let tls = if url.is_https() {
            Some(match &self.tls {
                Some(c) => c.as_ref(),
                None => {
                    default_cfg = default_tls_config();
                    default_cfg.as_ref()
                }
            })
        } else {
            None
        };
        let key = format!("{}://{}#{:p}", if url.is_https() { "https" } else { "http" }, url.authority(), tls.map(|c| c as *const TlsClientConfig).unwrap_or(std::ptr::null()));
        let hv: Vec<(String, String)> = headers.iter().filter(|(k, _)| !k.eq_ignore_ascii_case("Connection")).map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let mut h: Vec<(&str, &str)> = hv.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let host = url.host_header();
        if !headers.contains("Host") {
            h.push(("Host", host.as_str()));
        }
        let target = url.request_target();
        for attempt in 0..2 {
            let (mut conn, reused) = match pool().take(&key) {
                Some(c) => (c, true),
                None => (HttpConnection::connect_with(&url.host, url.port, tls, self.connect_timeout)?, false),
            };
            conn.set_timeout(t);
            match conn.send(method, &target, &h, body) {
                Ok(resp) => {
                    if !conn.is_closed() {
                        pool().put(key, conn);
                    }
                    return Ok(resp);
                }
                Err(e) if reused && attempt == 0 && e.kind != kinds::SOCKET_TIMEOUT => continue,
                Err(e) => return Err(e),
            }
        }
        Err(CamelError::io(format!("Connection to {} failed", url.authority())))
    }
}

/// A persistent (keep-alive) client connection to one server.
pub struct HttpConnection {
    stream: NetStream,
    reader: BufReader<NetStream>,
    host_header: String,
    limits: Limits,
    closed: bool,
}

impl HttpConnection {
    /// Connect to `host:port` (plain http).
    pub fn connect(host: &str, port: u16, timeout: Duration) -> Result<HttpConnection> {
        HttpConnection::connect_with(host, port, None, timeout)
    }

    /// Connect to `host:port`, over TLS when `tls` is given (SNI/verification use `host`).
    pub fn connect_with(host: &str, port: u16, tls: Option<&TlsClientConfig>, timeout: Duration) -> Result<HttpConnection> {
        let tcp = connect(host, port, timeout)?;
        let _ = tcp.set_read_timeout(Some(timeout));
        let _ = tcp.set_write_timeout(Some(timeout));
        let _ = tcp.set_nodelay(true);
        let stream = match tls {
            Some(cfg) => NetStream::client(tcp, host, cfg).map_err(|e| map_io_error(e, host))?,
            None => NetStream::plain(tcp),
        };
        let reader = BufReader::new(stream.try_clone().map_err(|e| map_io_error(e, host))?);
        Ok(HttpConnection { stream, reader, host_header: format!("{}:{}", host, port), limits: Limits::default(), closed: false })
    }

    /// Send a request on this connection and read the response.
    pub fn send(&mut self, method: &str, target: &str, headers: &[(&str, &str)], body: Option<&[u8]>) -> Result<HttpResponse> {
        if self.closed {
            return Err(CamelError::io("Connection closed by server"));
        }
        let mut h: HttpHeaders = headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        if !h.contains("Host") {
            h.set("Host", self.host_header.clone());
        }
        let mut buf = Vec::new();
        write_request(&mut buf, method, target, &h, body, false).map_err(|e| map_io_error(e, target))?;
        (&self.stream).write_all(&buf).map_err(|e| map_io_error(e, target))?;
        let resp = read_response(&mut self.reader, method, &self.limits).map_err(|e| map_http_error(e, target))?;
        if !response_keep_alive(&resp) {
            self.closed = true;
        }
        Ok(resp)
    }

    /// Set the read and write timeout for the next requests.
    pub fn set_timeout(&self, t: Option<Duration>) {
        let _ = self.stream.set_read_timeout(t);
        let _ = self.stream.set_write_timeout(t);
    }

    /// Whether the server signalled that the connection will be closed.
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Whether the connection uses TLS.
    pub fn is_tls(&self) -> bool {
        self.stream.is_tls()
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

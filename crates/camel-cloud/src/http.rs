//! A small HTTP/1.1 REST client over `std::net::TcpStream`, wrapped with
//! [`camel_tls::TlsStream`] for `https` URLs, plus the request/response codec shared
//! with the embedded mock servers.
//!
//! Proxies: an explicit [`Proxy`] (endpoint options `proxyHost`/`proxyPort`/`proxyProtocol`)
//! or, by default, the `HTTPS_PROXY` / `HTTP_PROXY` environment variables honouring
//! `NO_PROXY` (host suffixes, `*`, IPv4 CIDRs; loopback addresses are never proxied from
//! the environment). `https` goes through the proxy with `CONNECT`; plain `http` uses an
//! absolute request target. Each request uses its own connection (`Connection: close`).

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use camel_core::error::{kinds as core_kinds, CamelError, Result};
use camel_tls::{TlsClientConfig, TlsStream};

use crate::util::base64_encode;

/// A parsed `http(s)://` URL. `path` and `query` are kept as sent on the wire (encoded).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub path: String,
    pub query: Option<String>,
}

impl Url {
    /// Parse an absolute URL.
    pub fn parse(url: &str) -> Result<Url> {
        let (scheme, rest) = url
            .trim()
            .split_once("://")
            .ok_or_else(|| CamelError::illegal_argument(format!("Invalid URL (missing scheme): {}", url)))?;
        let scheme = scheme.to_ascii_lowercase();
        let default_port = match scheme.as_str() {
            "http" => 80,
            "https" => 443,
            _ => return Err(CamelError::illegal_argument(format!("Unsupported URL scheme: {}", url))),
        };
        let split = rest.find(['/', '?']).unwrap_or(rest.len());
        let (authority, pq) = rest.split_at(split);
        let authority = authority.rsplit('@').next().unwrap_or(authority);
        let (host, port) = if let Some(r) = authority.strip_prefix('[') {
            let (h, p) = r.split_once(']').ok_or_else(|| CamelError::illegal_argument(format!("Invalid URL: {}", url)))?;
            (h.to_string(), p.strip_prefix(':').map(|p| p.parse::<u16>()).transpose())
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), Some(p.parse::<u16>()).transpose()),
                None => (authority.to_string(), Ok(None)),
            }
        };
        let port = port.map_err(|_| CamelError::illegal_argument(format!("Invalid port in URL: {}", url)))?.unwrap_or(default_port);
        if host.is_empty() {
            return Err(CamelError::illegal_argument(format!("Invalid URL (missing host): {}", url)));
        }
        let (path, query) = match pq.split_once('?') {
            Some((p, q)) => (p, Some(q.to_string())),
            None => (pq, None),
        };
        Ok(Url { scheme, host: host.to_ascii_lowercase(), port, path: if path.is_empty() { "/".into() } else { path.into() }, query })
    }

    pub fn is_https(&self) -> bool {
        self.scheme == "https"
    }

    fn default_port(&self) -> u16 {
        if self.is_https() {
            443
        } else {
            80
        }
    }

    /// Value of the `Host` header (port omitted when it is the scheme default).
    pub fn host_header(&self) -> String {
        let h = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        if self.port == self.default_port() {
            h
        } else {
            format!("{}:{}", h, self.port)
        }
    }

    /// `host:port`.
    pub fn authority(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    /// Path plus query.
    pub fn target(&self) -> String {
        match &self.query {
            Some(q) if !q.is_empty() => format!("{}?{}", self.path, q),
            _ => self.path.clone(),
        }
    }

    /// `scheme://host[:port]` (no path).
    pub fn origin(&self) -> String {
        format!("{}://{}", self.scheme, self.host_header())
    }
}

impl std::fmt::Display for Url {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}{}", self.origin(), self.target())
    }
}

/// An outgoing request.
#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    pub url: Url,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn new(method: &str, url: Url) -> Request {
        Request { method: method.to_string(), url, headers: Vec::new(), body: Vec::new() }
    }
    pub fn header(mut self, name: &str, value: impl Into<String>) -> Request {
        self.set_header(name, value);
        self
    }
    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Request {
        self.body = body.into();
        self
    }
    /// Replace (or add) a header.
    pub fn set_header(&mut self, name: &str, value: impl Into<String>) {
        self.headers.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
        self.headers.push((name.to_string(), value.into()));
    }
    pub fn get_header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// A received response.
#[derive(Clone, Debug, Default)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn new(status: u16) -> Response {
        Response { status, headers: Vec::new(), body: Vec::new() }
    }
    /// Adds a header; `Content-Type` replaces an earlier one (a response has one type).
    pub fn with_header(mut self, name: &str, value: impl Into<String>) -> Response {
        if name.eq_ignore_ascii_case("Content-Type") {
            self.headers.retain(|(k, _)| !k.eq_ignore_ascii_case("Content-Type"));
        }
        self.headers.push((name.to_string(), value.into()));
        self
    }
    pub fn with_body(mut self, body: impl Into<Vec<u8>>) -> Response {
        self.body = body.into();
        self
    }
    /// A JSON response.
    pub fn json(status: u16, json: impl Into<String>) -> Response {
        Response::new(status).with_header("Content-Type", "application/json").with_body(json.into().into_bytes())
    }
    /// An XML response.
    pub fn xml(status: u16, xml: impl Into<String>) -> Response {
        Response::new(status).with_header("Content-Type", "application/xml").with_body(xml.into().into_bytes())
    }
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Proxy selection for a client.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Proxy {
    /// `HTTPS_PROXY`/`HTTP_PROXY`/`NO_PROXY` from the environment (default).
    #[default]
    Environment,
    /// Never use a proxy.
    None,
    /// Always use this proxy (`http://[user:pass@]host:port`).
    Explicit(String),
}

trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send> Stream for T {}

/// The REST client.
#[derive(Clone)]
pub struct HttpClient {
    pub timeout: Duration,
    pub proxy: Proxy,
    tls: Option<Arc<TlsClientConfig>>,
    insecure: bool,
}

impl std::fmt::Debug for HttpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpClient").field("timeout", &self.timeout).field("proxy", &self.proxy).field("insecure", &self.insecure).finish()
    }
}

impl Default for HttpClient {
    fn default() -> Self {
        HttpClient::new()
    }
}

fn default_tls(insecure: bool) -> Arc<TlsClientConfig> {
    static SECURE: OnceLock<Arc<TlsClientConfig>> = OnceLock::new();
    static INSECURE: OnceLock<Arc<TlsClientConfig>> = OnceLock::new();
    if insecure {
        INSECURE.get_or_init(|| Arc::new(TlsClientConfig::builder().system_roots(false).insecure_skip_verify(true).alpn_protocols(&["http/1.1"]).build())).clone()
    } else {
        SECURE.get_or_init(|| Arc::new(TlsClientConfig::builder().alpn_protocols(&["http/1.1"]).build())).clone()
    }
}

impl HttpClient {
    pub fn new() -> HttpClient {
        HttpClient { timeout: Duration::from_secs(60), proxy: Proxy::Environment, tls: None, insecure: false }
    }
    /// Use a custom TLS configuration (e.g. extra trusted roots).
    pub fn with_tls_config(mut self, cfg: TlsClientConfig) -> HttpClient {
        self.tls = Some(Arc::new(cfg));
        self
    }
    /// Skip certificate verification (`trustAllCertificates=true`); for tests only.
    pub fn trust_all(mut self, yes: bool) -> HttpClient {
        self.insecure = yes;
        self
    }
    pub fn with_proxy(mut self, proxy: Proxy) -> HttpClient {
        self.proxy = proxy;
        self
    }
    pub fn with_timeout(mut self, timeout: Duration) -> HttpClient {
        self.timeout = timeout;
        self
    }

    /// The proxy URL to use for `url`, if any.
    pub fn proxy_for(&self, url: &Url) -> Option<String> {
        match &self.proxy {
            Proxy::None => None,
            Proxy::Explicit(p) => Some(p.clone()),
            Proxy::Environment => env_proxy_for(url, &|k| std::env::var(k).ok()),
        }
    }

    fn connect_tcp(&self, authority: &str) -> Result<TcpStream> {
        let addrs: Vec<_> = authority.to_socket_addrs().map_err(|e| connect_err(authority, e))?.collect();
        let mut last = None;
        for a in addrs {
            match TcpStream::connect_timeout(&a, self.timeout.min(Duration::from_secs(30))) {
                Ok(s) => {
                    let _ = s.set_read_timeout(Some(self.timeout));
                    let _ = s.set_write_timeout(Some(self.timeout));
                    let _ = s.set_nodelay(true);
                    return Ok(s);
                }
                Err(e) => last = Some(e),
            }
        }
        Err(connect_err(authority, last.unwrap_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no address"))))
    }

    /// Send a request and read the full response.
    pub fn send(&self, req: &Request) -> Result<Response> {
        let mut reader = self.write_request(req, None)?;
        read_response(&mut reader, &req.method).map_err(|e| self.io_err(req, e))
    }

    /// Send a request whose body is streamed from `body` (`len` bytes; `req.body` is ignored).
    pub fn send_streaming(&self, req: &Request, body: &mut dyn Read, len: u64) -> Result<Response> {
        let mut reader = self.write_request(req, Some((body, len)))?;
        read_response(&mut reader, &req.method).map_err(|e| self.io_err(req, e))
    }

    /// Send a request and stream a successful (2xx) response body into `sink` instead of
    /// buffering it; the returned response then has an empty body. Error responses are buffered.
    /// Returns the response and the number of bytes written to `sink`.
    pub fn send_to(&self, req: &Request, sink: &mut dyn Write) -> Result<(Response, u64)> {
        let mut reader = self.write_request(req, None)?;
        let (status, headers) = read_response_head(&mut reader).map_err(|e| self.io_err(req, e))?;
        if !(200..300).contains(&status) || req.method == "HEAD" || status == 204 {
            let body = if req.method == "HEAD" || status == 204 { Vec::new() } else { read_body(&mut reader, &headers, true).map_err(|e| self.io_err(req, e))? };
            return Ok((Response { status, headers, body }, 0));
        }
        let n = copy_body(&mut reader, &headers, sink).map_err(|e| self.io_err(req, e))?;
        Ok((Response { status, headers, body: Vec::new() }, n))
    }

    fn io_err(&self, req: &Request, e: io::Error) -> CamelError {
        CamelError::new(core_kinds::IO, format!("I/O error talking to {}: {}", req.url.origin(), e))
    }

    fn write_request(&self, req: &Request, streamed: Option<(&mut dyn Read, u64)>) -> Result<BufReader<Box<dyn Stream>>> {
        let url = &req.url;
        let proxy = self.proxy_for(url).map(|p| Url::parse(&p).map(|u| (u, proxy_auth(&p)))).transpose()?;
        let mut absolute_target = false;
        let mut stream: Box<dyn Stream> = match (&proxy, url.is_https()) {
            (None, false) => Box::new(self.connect_tcp(&url.authority())?),
            (None, true) => Box::new(self.tls_wrap(self.connect_tcp(&url.authority())?, url)?),
            (Some((p, _)), false) => {
                absolute_target = true;
                Box::new(self.connect_tcp(&p.authority())?)
            }
            (Some((p, auth)), true) => {
                let mut s = self.connect_tcp(&p.authority())?;
                connect_tunnel(&mut s, &url.authority(), auth.as_deref())?;
                Box::new(self.tls_wrap(s, url)?)
            }
        };
        let target = if absolute_target { url.to_string() } else { url.target() };
        let mut head = format!("{} {} HTTP/1.1\r\n", req.method, target);
        if req.get_header("Host").is_none() {
            head.push_str(&format!("Host: {}\r\n", url.host_header()));
        }
        for (k, v) in &req.headers {
            if k.eq_ignore_ascii_case("content-length") || k.eq_ignore_ascii_case("connection") {
                continue;
            }
            head.push_str(&format!("{}: {}\r\n", k, v));
        }
        if let (true, Some((_, Some(auth)))) = (absolute_target, &proxy) {
            head.push_str(&format!("Proxy-Authorization: {}\r\n", auth));
        }
        if req.get_header("User-Agent").is_none() {
            head.push_str("User-Agent: rust-camel-cloud/0.1\r\n");
        }
        let body_len = streamed.as_ref().map(|(_, l)| *l).unwrap_or(req.body.len() as u64);
        if body_len > 0 || matches!(req.method.as_str(), "POST" | "PUT" | "PATCH") {
            head.push_str(&format!("Content-Length: {}\r\n", body_len));
        }
        head.push_str("Connection: close\r\n\r\n");
        let io_err = |e: io::Error| self.io_err(req, e);
        stream.write_all(head.as_bytes()).map_err(io_err)?;
        match streamed {
            Some((body, len)) => {
                let n = io::copy(&mut body.take(len), &mut stream).map_err(io_err)?;
                if n != len {
                    return Err(io_err(io::Error::new(io::ErrorKind::UnexpectedEof, format!("request body ended after {} of {} bytes", n, len))));
                }
            }
            None => stream.write_all(&req.body).map_err(io_err)?,
        }
        stream.flush().map_err(io_err)?;
        Ok(BufReader::new(stream))
    }

    fn tls_wrap(&self, tcp: TcpStream, url: &Url) -> Result<TlsStream<TcpStream>> {
        let cfg = match &self.tls {
            Some(c) => c.clone(),
            None => default_tls(self.insecure),
        };
        TlsStream::connect(tcp, &url.host, &cfg).map_err(|e| CamelError::new("SSLHandshakeException", format!("TLS handshake with {} failed: {}", url.authority(), e)))
    }
}

fn connect_err(authority: &str, e: io::Error) -> CamelError {
    CamelError::new(core_kinds::CONNECT, format!("Unable to connect to {}: {}", authority, e))
}

fn proxy_auth(proxy_url: &str) -> Option<String> {
    let rest = proxy_url.split_once("://").map(|(_, r)| r).unwrap_or(proxy_url);
    let authority = rest.split('/').next().unwrap_or("");
    let (userinfo, _) = authority.rsplit_once('@')?;
    Some(format!("Basic {}", base64_encode(crate::util::percent_decode(userinfo, false).as_bytes())))
}

fn connect_tunnel(s: &mut TcpStream, authority: &str, auth: Option<&str>) -> Result<()> {
    let mut req = format!("CONNECT {} HTTP/1.1\r\nHost: {}\r\n", authority, authority);
    if let Some(a) = auth {
        req.push_str(&format!("Proxy-Authorization: {}\r\n", a));
    }
    req.push_str("\r\n");
    let err = |m: String| CamelError::new(core_kinds::CONNECT, m);
    s.write_all(req.as_bytes()).map_err(|e| err(format!("Proxy CONNECT failed: {}", e)))?;
    // read the response head byte-by-byte so no tunnelled bytes are consumed
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        let n = s.read(&mut b).map_err(|e| err(format!("Proxy CONNECT failed: {}", e)))?;
        if n == 0 || head.len() > 16 * 1024 {
            return Err(err("Proxy closed the connection during CONNECT".into()));
        }
        head.push(b[0]);
    }
    let line = String::from_utf8_lossy(&head);
    let status = line.split_whitespace().nth(1).and_then(|s| s.parse::<u16>().ok()).unwrap_or(0);
    if !(200..300).contains(&status) {
        return Err(err(format!("Proxy refused CONNECT to {}: {}", authority, line.lines().next().unwrap_or(""))));
    }
    Ok(())
}

/// Proxy selection from environment variables (`env` looks a variable up).
pub fn env_proxy_for(url: &Url, env: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let get = |names: &[&str]| names.iter().find_map(|n| env(n).filter(|v| !v.trim().is_empty()));
    let proxy = if url.is_https() { get(&["HTTPS_PROXY", "https_proxy"]) } else { get(&["HTTP_PROXY", "http_proxy"]) }?;
    if is_loopback(&url.host) {
        return None;
    }
    if let Some(no) = get(&["NO_PROXY", "no_proxy"]) {
        if no_proxy_matches(&no, &url.host, url.port) {
            return None;
        }
    }
    Some(if proxy.contains("://") { proxy } else { format!("http://{}", proxy) })
}

fn is_loopback(host: &str) -> bool {
    host == "localhost" || host.ends_with(".localhost") || host.parse::<IpAddr>().map(|ip| ip.is_loopback()).unwrap_or(false)
}

/// Whether `host` is excluded by a `NO_PROXY` list.
pub fn no_proxy_matches(list: &str, host: &str, port: u16) -> bool {
    let host = host.to_ascii_lowercase();
    for entry in list.split(',').map(|e| e.trim().to_ascii_lowercase()).filter(|e| !e.is_empty()) {
        if entry == "*" {
            return true;
        }
        let (pattern, eport) = match entry.rsplit_once(':') {
            Some((h, p)) if !h.contains(':') && p.chars().all(|c| c.is_ascii_digit()) => (h.to_string(), p.parse::<u16>().ok()),
            _ => (entry.clone(), None),
        };
        if eport.is_some_and(|p| p != port) {
            continue;
        }
        if let Some((net, bits)) = pattern.split_once('/') {
            if let (Ok(IpAddr::V4(n)), Ok(IpAddr::V4(h)), Ok(bits)) = (net.parse::<IpAddr>(), host.parse::<IpAddr>(), bits.parse::<u32>()) {
                let mask = if bits == 0 { 0 } else { u32::MAX << (32 - bits.min(32)) };
                if u32::from(n) & mask == u32::from(h) & mask {
                    return true;
                }
            }
            continue;
        }
        let p = pattern.trim_start_matches("*.").trim_start_matches('.');
        if host == p || host.ends_with(&format!(".{}", p)) {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// codec
// ---------------------------------------------------------------------------

const MAX_BODY: usize = 512 * 1024 * 1024;

fn read_line<R: BufRead>(r: &mut R) -> io::Result<Option<String>> {
    let mut buf = Vec::new();
    let n = r.read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Ok(None);
    }
    while buf.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
        buf.pop();
    }
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

fn read_headers<R: BufRead>(r: &mut R) -> io::Result<Vec<(String, String)>> {
    let mut headers = Vec::new();
    loop {
        let line = read_line(r)?.ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "unexpected end of headers"))?;
        if line.is_empty() {
            return Ok(headers);
        }
        if headers.len() > 500 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "too many headers"));
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
}

fn find<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

fn read_body<R: BufRead>(r: &mut R, headers: &[(String, String)], to_eof: bool) -> io::Result<Vec<u8>> {
    if find(headers, "Transfer-Encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked")) {
        let mut out = Vec::new();
        loop {
            let line = read_line(r)?.ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "unexpected end of chunked body"))?;
            let size = usize::from_str_radix(line.split(';').next().unwrap_or("").trim(), 16)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad chunk size"))?;
            if size == 0 {
                while read_line(r)?.is_some_and(|l| !l.is_empty()) {}
                return Ok(out);
            }
            if out.len() + size > MAX_BODY {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "body too large"));
            }
            let start = out.len();
            out.resize(start + size, 0);
            r.read_exact(&mut out[start..])?;
            read_line(r)?;
        }
    }
    if let Some(len) = find(headers, "Content-Length") {
        let len: usize = len.trim().parse().map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad Content-Length"))?;
        if len > MAX_BODY {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "body too large"));
        }
        let mut out = vec![0; len];
        r.read_exact(&mut out)?;
        return Ok(out);
    }
    let mut out = Vec::new();
    if to_eof {
        match r.read_to_end(&mut out) {
            Ok(_) => {}
            // TLS peers that close without close_notify
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {}
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

/// Read a response status line and headers (skipping `1xx` responses).
pub fn read_response_head<R: BufRead>(r: &mut R) -> io::Result<(u16, Vec<(String, String)>)> {
    loop {
        let line = read_line(r)?.ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed before response"))?;
        let status: u16 = line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("bad status line: {}", line)))?;
        let headers = read_headers(r)?;
        if (100..200).contains(&status) {
            continue;
        }
        return Ok((status, headers));
    }
}

/// Copy a message body (chunked, Content-Length or until EOF) into `sink` without buffering it.
pub fn copy_body<R: BufRead>(r: &mut R, headers: &[(String, String)], sink: &mut dyn Write) -> io::Result<u64> {
    if find(headers, "Transfer-Encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked")) {
        let mut total = 0u64;
        loop {
            let line = read_line(r)?.ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "unexpected end of chunked body"))?;
            let size = u64::from_str_radix(line.split(';').next().unwrap_or("").trim(), 16).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad chunk size"))?;
            if size == 0 {
                while read_line(r)?.is_some_and(|l| !l.is_empty()) {}
                return Ok(total);
            }
            let n = io::copy(&mut r.take(size), sink)?;
            if n != size {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "unexpected end of chunk"));
            }
            total += n;
            read_line(r)?;
        }
    }
    if let Some(len) = find(headers, "Content-Length") {
        let len: u64 = len.trim().parse().map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad Content-Length"))?;
        let n = io::copy(&mut r.take(len), sink)?;
        if n != len {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, format!("body ended after {} of {} bytes", n, len)));
        }
        return Ok(n);
    }
    match io::copy(r, sink) {
        Ok(n) => Ok(n),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(0),
        Err(e) => Err(e),
    }
}

/// Read a response (`method` decides whether a body follows).
pub fn read_response<R: BufRead>(r: &mut R, method: &str) -> io::Result<Response> {
    let (status, headers) = read_response_head(r)?;
    let body = if method == "HEAD" || status == 204 || status == 304 { Vec::new() } else { read_body(r, &headers, true)? };
    Ok(Response { status, headers, body })
}

/// A request as received by a server.
#[derive(Clone, Debug)]
pub struct ServerRequest {
    pub method: String,
    /// Raw (encoded) path.
    pub path: String,
    /// Raw query (without `?`, empty when absent).
    pub query: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl ServerRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        find(&self.headers, name)
    }
    /// Decoded query parameters.
    pub fn query_params(&self) -> Vec<(String, String)> {
        crate::util::parse_form(&self.query)
    }
    pub fn query_param(&self, name: &str) -> Option<String> {
        self.query_params().into_iter().find(|(k, _)| k == name).map(|(_, v)| v)
    }
    /// Decoded path segments (empty segments dropped).
    pub fn segments(&self) -> Vec<String> {
        self.path.split('/').filter(|s| !s.is_empty()).map(|s| crate::util::percent_decode(s, false)).collect()
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Read a request (`None` at clean end of stream).
pub fn read_request<R: BufRead>(r: &mut R) -> io::Result<Option<ServerRequest>> {
    let line = match read_line(r)? {
        None => return Ok(None),
        Some(l) if l.is_empty() => match read_line(r)? {
            None => return Ok(None),
            Some(l) => l,
        },
        Some(l) => l,
    };
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("/").to_string();
    let headers = read_headers(r)?;
    let body = read_body(r, &headers, false)?;
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };
    Ok(Some(ServerRequest { method, path, query, headers, body }))
}

/// Write a response (Content-Length framing).
pub fn write_response<W: Write>(w: &mut W, resp: &Response, head_request: bool, keep_alive: bool) -> io::Result<()> {
    let mut head = format!("HTTP/1.1 {} {}\r\n", resp.status, reason(resp.status));
    let mut has_len = false;
    for (k, v) in &resp.headers {
        if k.eq_ignore_ascii_case("connection") {
            continue;
        }
        if k.eq_ignore_ascii_case("content-length") {
            has_len = true;
        }
        head.push_str(&format!("{}: {}\r\n", k, v));
    }
    if !has_len {
        head.push_str(&format!("Content-Length: {}\r\n", resp.body.len()));
    }
    head.push_str(if keep_alive { "Connection: keep-alive\r\n\r\n" } else { "Connection: close\r\n\r\n" });
    w.write_all(head.as_bytes())?;
    if !head_request {
        w.write_all(&resp.body)?;
    }
    w.flush()
}

fn reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        206 => "Partial Content",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        412 => "Precondition Failed",
        500 => "Internal Server Error",
        _ => "Status",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_parsing() {
        let u = Url::parse("https://bucket.s3.eu-west-1.amazonaws.com/a%20b?x=1").unwrap();
        assert_eq!(u.port, 443);
        assert_eq!(u.host_header(), "bucket.s3.eu-west-1.amazonaws.com");
        assert_eq!(u.target(), "/a%20b?x=1");
        let u = Url::parse("http://127.0.0.1:4566").unwrap();
        assert_eq!(u.path, "/");
        assert_eq!(u.host_header(), "127.0.0.1:4566");
        assert!(Url::parse("ftp://x").is_err());
        let u = Url::parse("http://[::1]:8080/p").unwrap();
        assert_eq!(u.host, "::1");
        assert_eq!(u.authority(), "[::1]:8080");
    }

    #[test]
    fn no_proxy_rules() {
        assert!(no_proxy_matches("*", "x.com", 443));
        assert!(no_proxy_matches("example.com", "s3.example.com", 443));
        assert!(no_proxy_matches(".example.com", "example.com", 443));
        assert!(no_proxy_matches("*.svc.cluster.local", "a.svc.cluster.local", 443));
        assert!(!no_proxy_matches("example.com", "badexample.com", 443));
        assert!(no_proxy_matches("10.0.0.0/8", "10.1.2.3", 443));
        assert!(!no_proxy_matches("10.0.0.0/8", "11.1.2.3", 443));
        assert!(no_proxy_matches("host:8443", "host", 8443));
        assert!(!no_proxy_matches("host:8443", "host", 443));
    }

    #[test]
    fn env_proxy_selection() {
        let env = |k: &str| match k {
            "HTTPS_PROXY" => Some("proxy.local:3128".to_string()),
            "NO_PROXY" => Some("internal.corp".to_string()),
            _ => None,
        };
        let u = Url::parse("https://sqs.us-east-1.amazonaws.com/").unwrap();
        assert_eq!(env_proxy_for(&u, &env).as_deref(), Some("http://proxy.local:3128"));
        assert_eq!(env_proxy_for(&Url::parse("https://a.internal.corp/").unwrap(), &env), None);
        assert_eq!(env_proxy_for(&Url::parse("https://127.0.0.1:9/").unwrap(), &env), None);
        assert_eq!(env_proxy_for(&Url::parse("http://example.com/").unwrap(), &env), None);
    }

    #[test]
    fn chunked_response() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        let r = read_response(&mut &raw[..], "GET").unwrap();
        assert_eq!(r.text(), "hello world");
        // streamed copy of the same body
        let mut reader = &raw[..];
        let (status, headers) = read_response_head(&mut reader).unwrap();
        let mut sink = Vec::new();
        assert_eq!((status, copy_body(&mut reader, &headers, &mut sink).unwrap()), (200, 11));
        assert_eq!(sink, b"hello world");
        let short = b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc";
        let mut reader = &short[..];
        let (_, headers) = read_response_head(&mut reader).unwrap();
        assert!(copy_body(&mut reader, &headers, &mut Vec::new()).is_err(), "truncated bodies are errors");
        assert_eq!(proxy_auth("http://u:p%40ss@h:1"), Some(format!("Basic {}", base64_encode(b"u:p@ss"))));
    }
}

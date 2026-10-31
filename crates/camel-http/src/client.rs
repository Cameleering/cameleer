//! HTTP/1.1 client (plain `http` only - no TLS is available in a std-only build).
//!
//! ```no_run
//! use camel_http::client::HttpClient;
//! use std::time::Duration;
//! let client = HttpClient::new();
//! let resp = client.request("GET", "http://localhost:8080/hello", &[("Accept", "text/plain")], None, Some(Duration::from_secs(5))).unwrap();
//! println!("{} {}", resp.status, resp.body_text());
//! ```

use std::io::{BufReader, ErrorKind};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use camel_core::error::{kinds, CamelError, Result};

use crate::http::{read_response, response_keep_alive, write_request, HttpError, HttpHeaders, HttpResponse, Limits};

/// A parsed `http://` URL.
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
    /// Parse an absolute `http://host[:port][/path][?query]` URL. `https` is rejected.
    pub fn parse(url: &str) -> Result<Url> {
        let url = url.trim();
        let (scheme, rest) = url
            .split_once("://")
            .ok_or_else(|| CamelError::illegal_argument(format!("Invalid URL (missing scheme): {}", url)))?;
        let scheme = scheme.to_ascii_lowercase();
        if scheme == "https" {
            return Err(https_not_supported(url));
        }
        if scheme != "http" {
            return Err(CamelError::illegal_argument(format!("Unsupported URL scheme '{}': {}", scheme, url)));
        }
        let rest = rest.split('#').next().unwrap_or("");
        let split = rest.find(['/', '?']).unwrap_or(rest.len());
        let (authority, path_query) = rest.split_at(split);
        let authority = authority.rsplit('@').next().unwrap_or(authority);
        let (host, port) = parse_authority(authority, 80).map_err(|m| CamelError::illegal_argument(format!("{}: {}", m, url)))?;
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
        if self.port == 80 {
            h
        } else {
            format!("{}:{}", h, self.port)
        }
    }

    /// Path plus query (`/a/b?x=1`).
    pub fn request_target(&self) -> String {
        match &self.query {
            Some(q) if !q.is_empty() => format!("{}?{}", self.path, q),
            _ => self.path.clone(),
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

/// The error returned for `https` URLs.
pub fn https_not_supported(url: &str) -> CamelError {
    CamelError::new(
        kinds::UNSUPPORTED_OPERATION,
        format!("HTTPS is not supported by rust-camel (std-only build without TLS); use plain http: {}", url),
    )
}

pub(crate) fn map_io_error(e: std::io::Error, what: &str) -> CamelError {
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
        }
    }
}

impl HttpClient {
    pub fn new() -> Self {
        Self::default()
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
        let stream = connect(&url.host, url.port, self.connect_timeout)?;
        let t = timeout.unwrap_or(self.read_timeout);
        let t = if t.is_zero() { None } else { Some(t) };
        let _ = stream.set_read_timeout(t);
        let _ = stream.set_write_timeout(t);
        let _ = stream.set_nodelay(true);
        let mut h = headers.clone();
        if !h.contains("Host") {
            h.set("Host", url.host_header());
        }
        h.set("Connection", "close");
        let mut w = &stream;
        write_request(&mut w, method, &url.request_target(), &h, body, self.chunked_requests && body.is_some()).map_err(|e| map_io_error(e, &what))?;
        let mut r = BufReader::new(&stream);
        read_response(&mut r, method, &self.limits).map_err(|e| map_http_error(e, &what))
    }
}

/// Perform a single request with default client settings.
pub fn request(method: &str, url: &str, headers: &[(&str, &str)], body: Option<&[u8]>, timeout: Option<Duration>) -> Result<HttpResponse> {
    HttpClient::new().request(method, url, headers, body, timeout)
}

/// A persistent (keep-alive) client connection to one server.
pub struct HttpConnection {
    stream: TcpStream,
    reader: BufReader<TcpStream>,
    host_header: String,
    limits: Limits,
    closed: bool,
}

impl HttpConnection {
    /// Connect to `host:port`.
    pub fn connect(host: &str, port: u16, timeout: Duration) -> Result<HttpConnection> {
        let stream = connect(host, port, timeout)?;
        let _ = stream.set_read_timeout(Some(timeout));
        let _ = stream.set_write_timeout(Some(timeout));
        let _ = stream.set_nodelay(true);
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
        let mut w = &self.stream;
        write_request(&mut w, method, target, &h, body, false).map_err(|e| map_io_error(e, target))?;
        let resp = read_response(&mut self.reader, method, &self.limits).map_err(|e| map_http_error(e, target))?;
        if !response_keep_alive(&resp) {
            self.closed = true;
        }
        Ok(resp)
    }

    /// Whether the server signalled that the connection will be closed.
    pub fn is_closed(&self) -> bool {
        self.closed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(Url::parse("https://h/").unwrap_err().message.contains("HTTPS is not supported"));
        let base = Url::parse("http://h:1/a/b").unwrap();
        assert_eq!(base.join("c").unwrap().path, "/a/c");
        assert_eq!(base.join("/z?q=1").unwrap().request_target(), "/z?q=1");
        assert_eq!(base.join("http://o/p").unwrap().host, "o");
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

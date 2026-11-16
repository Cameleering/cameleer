//! HTTP message model (headers, request, response) and the helpers around it: query string and
//! form-urlencoded parsing, URL coding, media types, dates. The wire format is simoom's (server
//! and client), std only.

use std::net::SocketAddr;

/// HTTP header list preserving order and original spelling; lookups are case-insensitive.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HttpHeaders {
    entries: Vec<(String, String)>,
}

impl HttpHeaders {
    pub fn new() -> Self {
        Self::default()
    }
    /// First value of a header.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
    /// All values of a header.
    pub fn get_all(&self, name: &str) -> Vec<&str> {
        self.entries.iter().filter(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str()).collect()
    }
    /// Replace all values of a header with a single value.
    pub fn set(&mut self, name: impl Into<String>, value: impl Into<String>) {
        let name = name.into();
        self.remove(&name);
        self.entries.push((name, value.into()));
    }
    /// Append a value (multi-valued headers).
    pub fn add(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.entries.push((name.into(), value.into()));
    }
    /// Remove all values of a header; returns true if any was removed.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
        before != self.entries.len()
    }
    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }
    /// True if a comma separated header contains `token` (case-insensitive), e.g.
    /// `has_token("Connection", "close")`.
    pub fn has_token(&self, name: &str, token: &str) -> bool {
        self.get_all(name).iter().any(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case(token)))
    }
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl<K: Into<String>, V: Into<String>> FromIterator<(K, V)> for HttpHeaders {
    fn from_iter<T: IntoIterator<Item = (K, V)>>(iter: T) -> Self {
        let mut h = HttpHeaders::new();
        for (k, v) in iter {
            h.add(k, v);
        }
        h
    }
}

/// HTTP protocol version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Version {
    Http10,
    #[default]
    Http11,
}

impl Version {
    pub fn as_str(&self) -> &'static str {
        match self {
            Version::Http10 => "HTTP/1.0",
            Version::Http11 => "HTTP/1.1",
        }
    }
}

/// A parsed HTTP request.
#[derive(Clone, Debug, Default)]
pub struct HttpRequest {
    /// Method in upper case (`GET`, `POST`...).
    pub method: String,
    /// The raw request-target (`/path?query`).
    pub target: String,
    /// The raw (still percent-encoded) path.
    pub raw_path: String,
    /// The percent-decoded path.
    pub path: String,
    /// The raw query string (without `?`), if any.
    pub raw_query: Option<String>,
    pub version: Version,
    pub headers: HttpHeaders,
    pub body: Vec<u8>,
    /// Remote peer address (set by the server).
    pub remote_addr: Option<SocketAddr>,
    /// HTTPS with client authentication: the verified client certificate chain (DER, leaf first).
    pub peer_certificates: Vec<Vec<u8>>,
    /// Streamed request body (a `cameleer_tls::stream_body` handle) when the handler asked for
    /// streaming ([`crate::server::HttpHandler::stream_request`]); `body` is empty then.
    pub body_stream: Option<cameleer_core::Value>,
}

impl HttpRequest {
    /// Create a request for `method` and request-target `target`.
    pub fn new(method: &str, target: &str) -> Self {
        let (raw_path, raw_query) = split_target(target);
        HttpRequest {
            method: method.to_ascii_uppercase(),
            target: target.to_string(),
            path: percent_decode(&raw_path, false),
            raw_path,
            raw_query,
            ..Default::default()
        }
    }
    /// Decoded query parameters in order.
    pub fn query_params(&self) -> Vec<(String, String)> {
        self.raw_query.as_deref().map(parse_query_string).unwrap_or_default()
    }
    /// First query parameter value with the given name.
    pub fn query_param(&self, name: &str) -> Option<String> {
        self.query_params().into_iter().find(|(k, _)| k == name).map(|(_, v)| v)
    }
    pub fn content_type(&self) -> Option<&str> {
        self.headers.get("Content-Type")
    }
    /// True if the body is `application/x-www-form-urlencoded`.
    pub fn is_form(&self) -> bool {
        self.content_type().map(|c| c.to_ascii_lowercase().starts_with("application/x-www-form-urlencoded")).unwrap_or(false)
    }
    /// Decoded form parameters (empty if the body is not form-urlencoded).
    pub fn form_params(&self) -> Vec<(String, String)> {
        if !self.is_form() {
            return Vec::new();
        }
        parse_query_string(&String::from_utf8_lossy(&self.body))
    }
    /// Body as (lossy) UTF-8 text.
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// An HTTP response.
#[derive(Clone, Debug, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    /// Reason phrase; empty means the standard phrase for the status.
    pub reason: String,
    pub version: Version,
    pub headers: HttpHeaders,
    pub body: Vec<u8>,
    /// Send the body with `Transfer-Encoding: chunked` (HTTP/1.1 only). Set by
    /// [`read_response`] when the received response was chunked.
    pub chunked: bool,
    /// A `cameleer_tls::stream_body` handle sent instead of `body`, with chunked transfer coding
    /// (HTTP/1.1) without buffering the payload.
    pub stream: Option<cameleer_core::Value>,
}

impl Default for HttpResponse {
    fn default() -> Self {
        HttpResponse::new(200)
    }
}

impl HttpResponse {
    pub fn new(status: u16) -> Self {
        HttpResponse { status, reason: String::new(), version: Version::Http11, headers: HttpHeaders::new(), body: Vec::new(), chunked: false, stream: None }
    }
    /// A `text/plain` response.
    pub fn text(status: u16, text: impl Into<String>) -> Self {
        let mut r = HttpResponse::new(status);
        r.headers.set("Content-Type", "text/plain; charset=UTF-8");
        r.body = text.into().into_bytes();
        r
    }
    /// An `application/json` response.
    pub fn json(status: u16, json: impl Into<String>) -> Self {
        let mut r = HttpResponse::new(status);
        r.headers.set("Content-Type", "application/json");
        r.body = json.into().into_bytes();
        r
    }
    /// Add a header (a repeated name adds another value).
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.add(name, value);
        self
    }
    /// Set the `Content-Type`, replacing the one `text`/`json` put there.
    pub fn with_content_type(mut self, value: &str) -> Self {
        self.headers.set("Content-Type", value);
        self
    }
    pub fn with_body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }
    pub fn with_chunked(mut self, chunked: bool) -> Self {
        self.chunked = chunked;
        self
    }
    /// Reason phrase (explicit or standard).
    pub fn reason_phrase(&self) -> &str {
        if self.reason.is_empty() {
            reason_phrase(self.status)
        } else {
            &self.reason
        }
    }
    /// Body as (lossy) UTF-8 text.
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    pub fn content_type(&self) -> Option<&str> {
        self.headers.get("Content-Type")
    }
}

/// Size limits applied while reading messages.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Maximum size of the request/status line plus all headers.
    pub max_header_bytes: usize,
    /// Maximum number of header fields.
    pub max_headers: usize,
    /// Maximum body size.
    pub max_body_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { max_header_bytes: 64 * 1024, max_headers: 100, max_body_bytes: 10 * 1024 * 1024 }
    }
}

fn is_token(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

/// True if `name` is a valid header field name.
pub fn is_valid_header_name(name: &str) -> bool {
    is_token(name)
}

/// True if `value` can be written as a header value (no CR/LF/NUL).
pub fn is_valid_header_value(value: &str) -> bool {
    !value.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0)
}

/// Split a request-target into raw path and raw query.
pub fn split_target(target: &str) -> (String, Option<String>) {
    let target = target.split('#').next().unwrap_or("");
    match target.find('?') {
        Some(i) => (target[..i].to_string(), Some(target[i + 1..].to_string())),
        None => (target.to_string(), None),
    }
}

/// Percent-decode; `plus_as_space` decodes `+` as a space (query strings / forms).
pub fn percent_decode(s: &str, plus_as_space: bool) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' if plus_as_space => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let h = std::str::from_utf8(&bytes[i + 1..i + 3])
                    .ok()
                    .filter(|h| h.bytes().all(|b| b.is_ascii_hexdigit()))
                    .and_then(|h| u8::from_str_radix(h, 16).ok());
                match h {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Percent-encode a query/form component (space as `%20`).
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// Percent-encode a path, keeping `/` separators.
pub fn percent_encode_path(s: &str) -> String {
    s.split('/').map(percent_encode).collect::<Vec<_>>().join("/")
}

/// Parse `a=1&b=x+y` (query string or form body) into decoded pairs.
pub fn parse_query_string(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.find('=') {
            Some(i) => (percent_decode(&p[..i], true), percent_decode(&p[i + 1..], true)),
            None => (percent_decode(p, true), String::new()),
        })
        .collect()
}

/// Encode pairs as a query string / form body.
pub fn encode_query<K: AsRef<str>, V: AsRef<str>>(pairs: &[(K, V)]) -> String {
    pairs.iter().map(|(k, v)| format!("{}={}", percent_encode(k.as_ref()), percent_encode(v.as_ref()))).collect::<Vec<_>>().join("&")
}

/// Standard reason phrase for a status code.
pub fn reason_phrase(code: u16) -> &'static str {
    match code {
        100 => "Continue",
        101 => "Switching Protocols",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        203 => "Non-Authoritative Information",
        204 => "No Content",
        205 => "Reset Content",
        206 => "Partial Content",
        300 => "Multiple Choices",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        402 => "Payment Required",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        407 => "Proxy Authentication Required",
        408 => "Request Timeout",
        409 => "Conflict",
        410 => "Gone",
        411 => "Length Required",
        412 => "Precondition Failed",
        413 => "Content Too Large",
        414 => "URI Too Long",
        415 => "Unsupported Media Type",
        416 => "Range Not Satisfiable",
        417 => "Expectation Failed",
        418 => "I'm a teapot",
        422 => "Unprocessable Content",
        426 => "Upgrade Required",
        428 => "Precondition Required",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        505 => "HTTP Version Not Supported",
        _ => match code / 100 {
            1 => "Informational",
            2 => "Success",
            3 => "Redirection",
            4 => "Client Error",
            _ => "Server Error",
        },
    }
}

/// True if a content type denotes text (text/*, JSON, XML, form, javascript, yaml).
pub fn is_text_content_type(ct: &str) -> bool {
    let ct = ct.to_ascii_lowercase();
    let mime = ct.split(';').next().unwrap_or("").trim();
    mime.starts_with("text/")
        || mime.ends_with("/json")
        || mime.ends_with("+json")
        || mime.ends_with("/xml")
        || mime.ends_with("+xml")
        || mime == "application/x-www-form-urlencoded"
        || mime == "application/javascript"
        || mime.ends_with("/yaml")
        || mime.ends_with("/x-yaml")
        || ct.contains("charset=")
}

/// Does a media type match a pattern (`*/*`, `text/*`, exact)? Parameters are ignored.
pub fn media_type_matches(pattern: &str, media: &str) -> bool {
    let p = pattern.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    let m = media.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    if p == "*/*" || p == "*" || m == "*/*" {
        return true;
    }
    if p == m {
        return true;
    }
    let (pt, ps) = p.split_once('/').unwrap_or((&p, ""));
    let (mt, ms) = m.split_once('/').unwrap_or((&m, ""));
    (ps == "*" && pt == mt) || (ms == "*" && pt == mt)
}

/// Standard base64 encoding (used for basic auth).
pub fn base64_encode(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for c in input.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Current time formatted as an IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`).
pub fn http_date_now() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    http_date(secs)
}

/// Format epoch seconds as an IMF-fixdate.
pub fn http_date(secs: i64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let (y, m, d, h, mi, s) = cameleer_core::util_time::civil_from_epoch(secs);
    let dow = DAYS[secs.div_euclid(86_400).rem_euclid(7) as usize];
    format!("{}, {:02} {} {} {:02}:{:02}:{:02} GMT", dow, d, MONTHS[(m - 1) as usize], y, h, mi, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_splits_its_target_and_decodes_the_query() {
        let req = HttpRequest::new("POST", "/a%20b/c?x=1&y=a+b#frag");
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/a b/c");
        assert_eq!(req.raw_path, "/a%20b/c");
        assert_eq!(req.query_params(), vec![("x".into(), "1".into()), ("y".into(), "a b".into())]);
    }

    #[test]
    fn helpers() {
        assert_eq!(percent_decode("a%2Fb+c%", false), "a/b+c%");
        assert_eq!(percent_decode("a+b%41", true), "a bA");
        assert_eq!(encode_query(&[("a b", "c&d")]), "a%20b=c%26d");
        assert_eq!(base64_encode(b"user:pass"), "dXNlcjpwYXNz");
        assert_eq!(base64_encode(b"ab"), "YWI=");
        assert_eq!(http_date(784111777), "Sun, 06 Nov 1994 08:49:37 GMT");
        assert!(media_type_matches("application/*", "application/json; charset=utf-8"));
        assert!(!media_type_matches("text/plain", "application/json"));
        assert!(is_text_content_type("application/vnd.api+json"));
    }
}

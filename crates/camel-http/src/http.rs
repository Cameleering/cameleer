//! HTTP/1.1 message model and wire format (RFC 9112 subset), std only.
//!
//! Supports request/response parsing and writing, `Content-Length` and
//! `Transfer-Encoding: chunked` bodies (both directions), `Expect: 100-continue`,
//! keep-alive negotiation, query string / form-urlencoded parsing and URL decoding.

use std::fmt;
use std::io::{self, BufRead, Read, Write};
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
    fn parse(s: &str) -> Option<Version> {
        match s {
            "HTTP/1.1" => Some(Version::Http11),
            "HTTP/1.0" => Some(Version::Http10),
            _ => None,
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
    /// Whether the client wants the connection kept open after this request.
    pub fn wants_keep_alive(&self) -> bool {
        match self.version {
            Version::Http11 => !self.headers.has_token("Connection", "close"),
            Version::Http10 => self.headers.has_token("Connection", "keep-alive"),
        }
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
}

impl Default for HttpResponse {
    fn default() -> Self {
        HttpResponse::new(200)
    }
}

impl HttpResponse {
    pub fn new(status: u16) -> Self {
        HttpResponse { status, reason: String::new(), version: Version::Http11, headers: HttpHeaders::new(), body: Vec::new(), chunked: false }
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
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.add(name, value);
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

/// Errors raised while reading HTTP messages.
#[derive(Debug)]
pub enum HttpError {
    Io(io::Error),
    /// The peer did not send data in time.
    Timeout,
    /// Malformed message.
    BadRequest(String),
    /// Request line + headers exceed the limit.
    HeaderTooLarge,
    /// Body exceeds the limit.
    PayloadTooLarge,
    /// Unsupported feature (e.g. unknown transfer coding).
    NotImplemented(String),
    /// Unsupported `Expect` header.
    ExpectationFailed,
    /// Connection closed in the middle of a message.
    UnexpectedEof,
}

impl HttpError {
    /// The response status a server should answer with, if any.
    pub fn status(&self) -> Option<u16> {
        match self {
            HttpError::BadRequest(_) => Some(400),
            HttpError::HeaderTooLarge => Some(431),
            HttpError::PayloadTooLarge => Some(413),
            HttpError::NotImplemented(_) => Some(501),
            HttpError::ExpectationFailed => Some(417),
            HttpError::Timeout => Some(408),
            HttpError::Io(_) | HttpError::UnexpectedEof => None,
        }
    }
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HttpError::Io(e) => write!(f, "I/O error: {}", e),
            HttpError::Timeout => write!(f, "Read timed out"),
            HttpError::BadRequest(m) => write!(f, "Bad request: {}", m),
            HttpError::HeaderTooLarge => write!(f, "Request header fields too large"),
            HttpError::PayloadTooLarge => write!(f, "Payload too large"),
            HttpError::NotImplemented(m) => write!(f, "Not implemented: {}", m),
            HttpError::ExpectationFailed => write!(f, "Expectation failed"),
            HttpError::UnexpectedEof => write!(f, "Connection closed unexpectedly"),
        }
    }
}

impl std::error::Error for HttpError {}

impl From<io::Error> for HttpError {
    fn from(e: io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => HttpError::Timeout,
            io::ErrorKind::UnexpectedEof => HttpError::UnexpectedEof,
            _ => HttpError::Io(e),
        }
    }
}

/// Read a CRLF (or LF) terminated line, consuming at most `budget` bytes.
/// Returns `Ok(None)` on EOF before any byte.
fn read_line<R: BufRead>(r: &mut R, budget: &mut usize, too_large: fn() -> HttpError) -> Result<Option<String>, HttpError> {
    let mut buf = Vec::new();
    loop {
        let available = r.fill_buf()?;
        if available.is_empty() {
            if buf.is_empty() {
                return Ok(None);
            }
            return Err(HttpError::UnexpectedEof);
        }
        let (consume, done) = match available.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (available.len(), false),
        };
        if consume > *budget {
            return Err(too_large());
        }
        *budget -= consume;
        buf.extend_from_slice(&available[..consume]);
        r.consume(consume);
        if done {
            break;
        }
    }
    buf.pop(); // '\n'
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    String::from_utf8(buf).map(Some).map_err(|_| HttpError::BadRequest("invalid UTF-8 in header".into()))
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

/// Read the start line and header fields. Returns `None` on clean EOF.
fn read_head<R: BufRead>(r: &mut R, limits: &Limits) -> Result<Option<(String, HttpHeaders)>, HttpError> {
    let mut budget = limits.max_header_bytes;
    let too_large: fn() -> HttpError = || HttpError::HeaderTooLarge;
    // tolerate leading empty lines (RFC 9112 2.2)
    let start = loop {
        match read_line(r, &mut budget, too_large)? {
            None => return Ok(None),
            Some(l) if l.is_empty() => continue,
            Some(l) => break l,
        }
    };
    let mut headers = HttpHeaders::new();
    loop {
        let line = read_line(r, &mut budget, too_large)?.ok_or(HttpError::UnexpectedEof)?;
        if line.is_empty() {
            break;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            return Err(HttpError::BadRequest("obsolete header line folding".into()));
        }
        let colon = line.find(':').ok_or_else(|| HttpError::BadRequest(format!("malformed header: {}", line)))?;
        let name = &line[..colon];
        if !is_token(name) {
            return Err(HttpError::BadRequest(format!("invalid header name: {}", name)));
        }
        if headers.len() >= limits.max_headers {
            return Err(HttpError::HeaderTooLarge);
        }
        headers.add(name, line[colon + 1..].trim());
    }
    Ok(Some((start, headers)))
}

/// How a message body is delimited.
enum BodyKind {
    None,
    Length(usize),
    Chunked,
    UntilClose,
}

fn body_kind(headers: &HttpHeaders, is_request: bool) -> Result<BodyKind, HttpError> {
    if let Some(te) = headers.get_all("Transfer-Encoding").last().copied() {
        let last = te.split(',').next_back().map(|s| s.trim().to_ascii_lowercase()).unwrap_or_default();
        if last == "chunked" {
            return Ok(BodyKind::Chunked);
        }
        if is_request {
            return Err(HttpError::NotImplemented(format!("Transfer-Encoding: {}", te)));
        }
        return Ok(BodyKind::UntilClose);
    }
    let lengths = headers.get_all("Content-Length");
    if !lengths.is_empty() {
        let mut value: Option<usize> = None;
        for l in lengths {
            for part in l.split(',') {
                let part = part.trim();
                if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(HttpError::BadRequest(format!("invalid Content-Length: {}", l)));
                }
                let n: usize = part.parse().map_err(|_| HttpError::PayloadTooLarge)?;
                if value.is_some_and(|v| v != n) {
                    return Err(HttpError::BadRequest("conflicting Content-Length values".into()));
                }
                value = Some(n);
            }
        }
        return Ok(BodyKind::Length(value.unwrap_or(0)));
    }
    Ok(if is_request { BodyKind::None } else { BodyKind::UntilClose })
}

/// Decode a chunked body (trailers are read and discarded).
pub fn read_chunked<R: BufRead>(r: &mut R, max: usize) -> Result<Vec<u8>, HttpError> {
    let mut body = Vec::new();
    loop {
        let mut budget = 4096;
        let line = read_line(r, &mut budget, || HttpError::BadRequest("chunk size line too long".into()))?
            .ok_or(HttpError::UnexpectedEof)?;
        let size_str = line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_str, 16).map_err(|_| HttpError::BadRequest(format!("invalid chunk size: {}", line)))?;
        if size == 0 {
            // trailers
            let mut budget = 16 * 1024;
            loop {
                let t = read_line(r, &mut budget, || HttpError::HeaderTooLarge)?.ok_or(HttpError::UnexpectedEof)?;
                if t.is_empty() {
                    break;
                }
            }
            return Ok(body);
        }
        if body.len() + size > max {
            return Err(HttpError::PayloadTooLarge);
        }
        let start = body.len();
        body.resize(start + size, 0);
        r.read_exact(&mut body[start..])?;
        let mut crlf = [0u8; 2];
        r.read_exact(&mut crlf[..1])?;
        if crlf[0] == b'\r' {
            r.read_exact(&mut crlf[1..])?;
        }
    }
}

/// Read a request from `r`. If the request carries `Expect: 100-continue`, an interim
/// `100 Continue` response is written to `w` before the body is read.
/// Returns `Ok(None)` when the connection was closed before a new request started.
pub fn read_request<R: BufRead, W: Write>(r: &mut R, w: Option<&mut W>, limits: &Limits) -> Result<Option<HttpRequest>, HttpError> {
    let (start, headers) = match read_head(r, limits)? {
        None => return Ok(None),
        Some(h) => h,
    };
    let mut parts = start.split(' ').filter(|s| !s.is_empty());
    let (method, target, version) = match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(m), Some(t), Some(v), None) => (m, t, v),
        _ => return Err(HttpError::BadRequest(format!("malformed request line: {}", start))),
    };
    if !is_token(method) {
        return Err(HttpError::BadRequest(format!("invalid method: {}", method)));
    }
    let version = Version::parse(version).ok_or_else(|| HttpError::BadRequest(format!("unsupported version: {}", version)))?;
    if version == Version::Http11 && !headers.contains("Host") {
        return Err(HttpError::BadRequest("missing Host header".into()));
    }
    // absolute-form targets (proxy style): keep only path + query
    let target = if let Some(rest) = target.strip_prefix("http://").or_else(|| target.strip_prefix("https://")) {
        match rest.find('/') {
            Some(i) => rest[i..].to_string(),
            None => "/".to_string(),
        }
    } else {
        target.to_string()
    };
    if !target.starts_with('/') && target != "*" {
        return Err(HttpError::BadRequest(format!("invalid request target: {}", target)));
    }
    let mut req = HttpRequest::new(method, &target);
    req.method = method.to_string();
    req.version = version;
    req.headers = headers;
    let kind = body_kind(&req.headers, true)?;
    let expects_body = matches!(kind, BodyKind::Chunked) || matches!(kind, BodyKind::Length(n) if n > 0);
    if let Some(expect) = req.headers.get("Expect") {
        if !expect.eq_ignore_ascii_case("100-continue") {
            return Err(HttpError::ExpectationFailed);
        }
        if let BodyKind::Length(n) = kind {
            if n > limits.max_body_bytes {
                return Err(HttpError::PayloadTooLarge);
            }
        }
        if expects_body && version == Version::Http11 {
            if let Some(w) = w {
                w.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
                w.flush()?;
            }
        }
    }
    req.body = match kind {
        BodyKind::None | BodyKind::UntilClose => Vec::new(),
        BodyKind::Length(n) => {
            if n > limits.max_body_bytes {
                return Err(HttpError::PayloadTooLarge);
            }
            let mut b = vec![0u8; n];
            r.read_exact(&mut b)?;
            b
        }
        BodyKind::Chunked => read_chunked(r, limits.max_body_bytes)?,
    };
    Ok(Some(req))
}

fn status_has_no_body(status: u16) -> bool {
    (100..200).contains(&status) || status == 204 || status == 304
}

/// Read a response to a request made with `request_method`. Interim `1xx` responses
/// (other than `101`) are skipped.
pub fn read_response<R: BufRead>(r: &mut R, request_method: &str, limits: &Limits) -> Result<HttpResponse, HttpError> {
    loop {
        let (start, headers) = read_head(r, limits)?.ok_or(HttpError::UnexpectedEof)?;
        let mut parts = start.splitn(3, ' ');
        let version = parts.next().and_then(Version::parse).ok_or_else(|| HttpError::BadRequest(format!("malformed status line: {}", start)))?;
        let status: u16 = parts
            .next()
            .and_then(|s| s.parse().ok())
            .filter(|s| (100..1000).contains(s))
            .ok_or_else(|| HttpError::BadRequest(format!("malformed status line: {}", start)))?;
        let reason = parts.next().unwrap_or("").to_string();
        if (100..200).contains(&status) && status != 101 {
            continue;
        }
        let mut resp = HttpResponse { status, reason, version, headers, body: Vec::new(), chunked: false };
        if request_method.eq_ignore_ascii_case("HEAD") || status_has_no_body(status) {
            return Ok(resp);
        }
        match body_kind(&resp.headers, false)? {
            BodyKind::None => {}
            BodyKind::Length(n) => {
                if n > limits.max_body_bytes {
                    return Err(HttpError::PayloadTooLarge);
                }
                resp.body = vec![0u8; n];
                r.read_exact(&mut resp.body)?;
            }
            BodyKind::Chunked => {
                resp.body = read_chunked(r, limits.max_body_bytes)?;
                resp.chunked = true;
            }
            BodyKind::UntilClose => {
                let mut b = Vec::new();
                r.take(limits.max_body_bytes as u64 + 1).read_to_end(&mut b)?;
                if b.len() > limits.max_body_bytes {
                    return Err(HttpError::PayloadTooLarge);
                }
                resp.body = b;
            }
        }
        return Ok(resp);
    }
}

/// Whether the connection can be reused after this response.
pub fn response_keep_alive(resp: &HttpResponse) -> bool {
    match resp.version {
        Version::Http11 => !resp.headers.has_token("Connection", "close"),
        Version::Http10 => resp.headers.has_token("Connection", "keep-alive"),
    }
}

/// Write a body using chunked transfer coding.
pub fn write_chunked_body<W: Write>(w: &mut W, body: &[u8], chunk_size: usize) -> io::Result<()> {
    for chunk in body.chunks(chunk_size.max(1)) {
        write!(w, "{:X}\r\n", chunk.len())?;
        w.write_all(chunk)?;
        w.write_all(b"\r\n")?;
    }
    w.write_all(b"0\r\n\r\n")
}

/// Chunk size used when writing chunked bodies.
pub const CHUNK_SIZE: usize = 8192;

fn is_framing_header(name: &str) -> bool {
    name.eq_ignore_ascii_case("Content-Length") || name.eq_ignore_ascii_case("Transfer-Encoding") || name.eq_ignore_ascii_case("Connection")
}

/// Write a response. `head_request`: the request was `HEAD` (no body is sent).
/// `keep_alive`: whether the connection stays open. The response version should be
/// the request version; chunked encoding falls back to `Content-Length` on HTTP/1.0.
pub fn write_response<W: Write>(w: &mut W, resp: &HttpResponse, head_request: bool, keep_alive: bool) -> io::Result<()> {
    let mut out = Vec::with_capacity(256 + resp.body.len());
    write!(out, "{} {} {}\r\n", resp.version.as_str(), resp.status, resp.reason_phrase())?;
    let mut has_date = false;
    for (k, v) in resp.headers.iter() {
        if is_framing_header(k) || !is_valid_header_name(k) || !is_valid_header_value(v) {
            continue;
        }
        if k.eq_ignore_ascii_case("Date") {
            has_date = true;
        }
        write!(out, "{}: {}\r\n", k, v)?;
    }
    if !has_date {
        write!(out, "Date: {}\r\n", http_date_now())?;
    }
    let no_body = status_has_no_body(resp.status);
    let chunked = !no_body && resp.chunked && resp.version == Version::Http11;
    if !no_body {
        if chunked {
            out.extend_from_slice(b"Transfer-Encoding: chunked\r\n");
        } else {
            write!(out, "Content-Length: {}\r\n", resp.body.len())?;
        }
    }
    match (resp.version, keep_alive) {
        (_, false) => out.extend_from_slice(b"Connection: close\r\n"),
        (Version::Http10, true) => out.extend_from_slice(b"Connection: keep-alive\r\n"),
        _ => {}
    }
    out.extend_from_slice(b"\r\n");
    if !no_body && !head_request {
        if chunked {
            write_chunked_body(&mut out, &resp.body, CHUNK_SIZE)?;
        } else {
            out.extend_from_slice(&resp.body);
        }
    }
    w.write_all(&out)?;
    w.flush()
}

/// Write a request. A body is sent with `Content-Length` unless `chunked` is true.
/// Framing headers in `headers` are ignored.
pub fn write_request<W: Write>(w: &mut W, method: &str, target: &str, headers: &HttpHeaders, body: Option<&[u8]>, chunked: bool) -> io::Result<()> {
    let mut out = Vec::with_capacity(256 + body.map(|b| b.len()).unwrap_or(0));
    write!(out, "{} {} HTTP/1.1\r\n", method, target)?;
    for (k, v) in headers.iter() {
        if k.eq_ignore_ascii_case("Content-Length") || k.eq_ignore_ascii_case("Transfer-Encoding") {
            continue;
        }
        if !is_valid_header_name(k) || !is_valid_header_value(v) {
            continue;
        }
        write!(out, "{}: {}\r\n", k, v)?;
    }
    match body {
        Some(b) if chunked => {
            out.extend_from_slice(b"Transfer-Encoding: chunked\r\n\r\n");
            write_chunked_body(&mut out, b, CHUNK_SIZE)?;
        }
        Some(b) => {
            write!(out, "Content-Length: {}\r\n\r\n", b.len())?;
            out.extend_from_slice(b);
        }
        None => {
            if matches!(method, "POST" | "PUT" | "PATCH") {
                out.extend_from_slice(b"Content-Length: 0\r\n");
            }
            out.extend_from_slice(b"\r\n");
        }
    }
    w.write_all(&out)?;
    w.flush()
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
    let (y, m, d, h, mi, s) = camel_core::util_time::civil_from_epoch(secs);
    let dow = DAYS[secs.div_euclid(86_400).rem_euclid(7) as usize];
    format!("{}, {:02} {} {} {:02}:{:02}:{:02} GMT", dow, d, MONTHS[(m - 1) as usize], y, h, mi, s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    fn parse(raw: &str) -> Result<Option<HttpRequest>, HttpError> {
        let mut r = BufReader::new(raw.as_bytes());
        read_request::<_, Vec<u8>>(&mut r, None, &Limits::default())
    }

    #[test]
    fn parses_request_with_length() {
        let req = parse("POST /a%20b/c?x=1&y=a+b HTTP/1.1\r\nHost: h\r\nContent-Length: 5\r\n\r\nhello").unwrap().unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/a b/c");
        assert_eq!(req.raw_path, "/a%20b/c");
        assert_eq!(req.query_params(), vec![("x".into(), "1".into()), ("y".into(), "a b".into())]);
        assert_eq!(req.body, b"hello");
        assert!(req.wants_keep_alive());
    }

    #[test]
    fn parses_chunked_request_and_continue() {
        let raw = "PUT /x HTTP/1.1\r\nHost: h\r\nExpect: 100-continue\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6;ext=1\r\n world\r\n0\r\nTrailer: x\r\n\r\n";
        let mut r = BufReader::new(raw.as_bytes());
        let mut w: Vec<u8> = Vec::new();
        let req = read_request(&mut r, Some(&mut w), &Limits::default()).unwrap().unwrap();
        assert_eq!(req.body, b"hello world");
        assert_eq!(String::from_utf8(w).unwrap(), "HTTP/1.1 100 Continue\r\n\r\n");
    }

    #[test]
    fn rejects_bad_requests() {
        assert!(matches!(parse("GET / HTTP/1.1\r\n\r\n"), Err(HttpError::BadRequest(_))));
        assert!(matches!(parse("GARBAGE\r\n\r\n"), Err(HttpError::BadRequest(_))));
        assert!(parse("").unwrap().is_none());
        let limits = Limits { max_body_bytes: 3, ..Default::default() };
        let mut r = BufReader::new("POST / HTTP/1.1\r\nHost: h\r\nContent-Length: 5\r\n\r\nhello".as_bytes());
        assert!(matches!(read_request::<_, Vec<u8>>(&mut r, None, &limits), Err(HttpError::PayloadTooLarge)));
        let limits = Limits { max_header_bytes: 20, ..Default::default() };
        let mut r = BufReader::new("GET /aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa HTTP/1.1\r\nHost: h\r\n\r\n".as_bytes());
        assert!(matches!(read_request::<_, Vec<u8>>(&mut r, None, &limits), Err(HttpError::HeaderTooLarge)));
        assert!(matches!(parse("POST / HTTP/1.1\r\nHost: h\r\nTransfer-Encoding: gzip\r\n\r\n"), Err(HttpError::NotImplemented(_))));
    }

    #[test]
    fn writes_and_reads_responses() {
        let resp = HttpResponse::text(200, "hello world").with_chunked(true);
        let mut out = Vec::new();
        write_response(&mut out, &resp, false, true).unwrap();
        let s = String::from_utf8(out.clone()).unwrap();
        assert!(s.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(s.contains("Transfer-Encoding: chunked\r\n"));
        let mut r = BufReader::new(&out[..]);
        let back = read_response(&mut r, "GET", &Limits::default()).unwrap();
        assert_eq!(back.body, b"hello world");
        assert!(back.chunked);

        let mut out = Vec::new();
        write_response(&mut out, &HttpResponse::new(204), false, false).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(!s.contains("Content-Length"));
        assert!(s.contains("Connection: close"));
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

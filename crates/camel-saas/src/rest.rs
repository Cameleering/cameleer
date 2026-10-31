//! Shared REST plumbing for the SaaS connectors: a request builder over the std-only
//! HTTP(S) client of `camel-http`, JSON / form / multipart bodies, Basic and Bearer
//! authentication, error mapping, and a scheduled-poll consumer helper.

use std::sync::Arc;
use std::time::Duration;

use camel_core::component::scheduled::{ScheduledPollConfig, ScheduledPollConsumer};
use camel_core::component::support::process_consumer_exchange;
use camel_core::context::WeakContext;
use camel_core::error::{kinds, register_kind_parent, CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::Processor;
use camel_core::util::{url_encode, Parameters};
use camel_core::value::Value;
use camel_http::{HttpClient, HttpHeaders};

/// Kind of errors reported by a remote API (HTTP 200 with an error payload, or a
/// logical failure). A `RuntimeException`.
pub const API_EXCEPTION: &str = "ApiException";

pub(crate) fn register_kinds() {
    static INIT: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    INIT.get_or_init(|| {
        register_kind_parent(API_EXCEPTION, kinds::RUNTIME);
    });
}

/// An API error.
pub fn api_error(service: &str, m: impl Into<String>) -> CamelError {
    register_kinds();
    CamelError::new(API_EXCEPTION, format!("{}: {}", service, m.into()))
}

/// Standard base64 encoding.
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Join a base URL and a path.
pub fn join_url(base: &str, path: &str) -> String {
    if path.starts_with("http://") || path.starts_with("https://") {
        return path.to_string();
    }
    format!("{}/{}", base.trim_end_matches('/'), path.trim_start_matches('/'))
}

/// Encode query parameters (`a=1&b=2`).
pub fn query(params: &[(&str, String)]) -> String {
    params.iter().map(|(k, v)| format!("{}={}", url_encode(k), url_encode(v))).collect::<Vec<_>>().join("&")
}

/// A multipart/form-data part.
pub struct Part {
    pub name: String,
    pub filename: Option<String>,
    pub content_type: Option<String>,
    pub data: Vec<u8>,
}

/// An HTTP request under construction.
pub struct Req {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

impl Req {
    pub fn new(method: &str, url: impl Into<String>) -> Req {
        Req { method: method.to_string(), url: url.into(), headers: vec![("Accept".into(), "application/json".into())], body: None }
    }

    pub fn get(url: impl Into<String>) -> Req {
        Req::new("GET", url)
    }

    pub fn post(url: impl Into<String>) -> Req {
        Req::new("POST", url)
    }

    pub fn header(mut self, k: &str, v: impl Into<String>) -> Req {
        self.headers.retain(|(hk, _)| !hk.eq_ignore_ascii_case(k));
        self.headers.push((k.to_string(), v.into()));
        self
    }

    pub fn bearer(self, token: &str) -> Req {
        self.header("Authorization", format!("Bearer {}", token))
    }

    pub fn basic(self, user: &str, password: &str) -> Req {
        self.header("Authorization", format!("Basic {}", base64(format!("{}:{}", user, password).as_bytes())))
    }

    pub fn json(mut self, v: &Value) -> Req {
        self.body = Some(v.to_json().into_bytes());
        self.header("Content-Type", "application/json; charset=utf-8")
    }

    pub fn form(mut self, params: &[(&str, String)]) -> Req {
        self.body = Some(query(params).into_bytes());
        self.header("Content-Type", "application/x-www-form-urlencoded")
    }

    pub fn multipart(mut self, parts: &[Part]) -> Req {
        let boundary = format!("----rustcamel{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        let mut body = Vec::new();
        for p in parts {
            body.extend_from_slice(format!("--{}\r\nContent-Disposition: form-data; name=\"{}\"", boundary, p.name).as_bytes());
            if let Some(f) = &p.filename {
                body.extend_from_slice(format!("; filename=\"{}\"", f).as_bytes());
            }
            body.extend_from_slice(b"\r\n");
            if let Some(ct) = &p.content_type {
                body.extend_from_slice(format!("Content-Type: {}\r\n", ct).as_bytes());
            }
            body.extend_from_slice(b"\r\n");
            body.extend_from_slice(&p.data);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());
        self.body = Some(body);
        self.header("Content-Type", format!("multipart/form-data; boundary={}", boundary))
    }

    pub fn raw(mut self, content_type: &str, body: Vec<u8>) -> Req {
        self.body = Some(body);
        self.header("Content-Type", content_type)
    }
}

/// A response.
#[derive(Debug, Clone)]
pub struct Resp {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Resp {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn json(&self) -> Result<Value> {
        if self.body.is_empty() {
            return Ok(Value::Null);
        }
        camel_core::json::parse(&self.text())
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// A small REST client.
#[derive(Clone)]
pub struct Rest {
    client: Arc<HttpClient>,
}

impl Default for Rest {
    fn default() -> Self {
        Rest::new(Duration::from_secs(30))
    }
}

impl Rest {
    pub fn new(read_timeout: Duration) -> Rest {
        let mut c = HttpClient::new();
        c.read_timeout = read_timeout;
        c.connect_timeout = Duration::from_secs(10);
        Rest { client: Arc::new(c) }
    }

    /// Send a request.
    pub fn send(&self, req: Req) -> Result<Resp> {
        let url = camel_http::Url::parse(&req.url)?;
        let mut h = HttpHeaders::new();
        h.set("User-Agent", "rust-camel");
        for (k, v) in &req.headers {
            h.set(k.clone(), v.clone());
        }
        let r = self.client.execute(&req.method, &url, &h, req.body.as_deref(), None)?;
        Ok(Resp { status: r.status, headers: r.headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(), body: r.body })
    }

    /// Send a request and fail with `HttpOperationFailedException` on a non-2xx status.
    pub fn send_ok(&self, req: Req) -> Result<Resp> {
        let url = req.url.clone();
        let r = self.send(req)?;
        if !r.is_success() {
            return Err(http_failed(&url, &r));
        }
        Ok(r)
    }

    /// Send and parse a JSON response (non-2xx fails).
    pub fn json(&self, req: Req) -> Result<Value> {
        self.send_ok(req)?.json()
    }
}

/// `HttpOperationFailedException` for a response.
pub fn http_failed(url: &str, r: &Resp) -> CamelError {
    let mut text = r.text();
    if text.len() > 2000 {
        text.truncate(2000);
    }
    CamelError::new(kinds::HTTP_OPERATION_FAILED, format!("HTTP operation failed invoking {} with statusCode: {}, body: {}", strip_secrets(url), r.status, text))
}

/// Remove credentials embedded in URLs (telegram bot tokens) from messages.
fn strip_secrets(url: &str) -> String {
    match url.find("/bot") {
        Some(i) => {
            let rest = &url[i + 4..];
            let end = rest.find('/').unwrap_or(rest.len());
            format!("{}/bot***{}", &url[..i], &rest[end..])
        }
        None => url.to_string(),
    }
}

/// Take a required option.
pub fn required(params: &mut Parameters, name: &str, component: &str) -> Result<String> {
    params.take(name).filter(|s| !s.is_empty()).ok_or_else(|| CamelError::illegal_argument(format!("{}: the option '{}' is required", component, name)))
}

/// Feed one message into a route processor (logging unhandled failures).
pub fn emit(ctx: &WeakContext, processor: &dyn Processor, logger: &str, body: Value, headers: Vec<(&str, Value)>) -> bool {
    let mut ex = Exchange::new(ctx.upgrade());
    ex.message.body = body;
    for (k, v) in headers {
        ex.message.headers.set(k, v);
    }
    process_consumer_exchange(logger, processor, &mut ex)
}

/// Build a scheduled poll consumer from endpoint parameters.
pub fn poll_config(params: &mut Parameters, default_delay: Duration) -> Result<ScheduledPollConfig> {
    ScheduledPollConfig::from_params(params, Duration::from_millis(500).min(default_delay), default_delay)
}

/// A scheduled poll consumer running `poll` (returns the number of messages processed).
pub fn poll_consumer(name: &str, cfg: ScheduledPollConfig, poll: impl Fn() -> Result<usize> + Send + Sync + 'static) -> ScheduledPollConsumer {
    ScheduledPollConsumer::new(name.to_string(), cfg, poll)
}

/// Header value as string.
pub fn header_str(ex: &Exchange, name: &str) -> Option<String> {
    ex.message.headers.get(name).filter(|v| !v.is_null()).map(|v| v.to_string())
}

/// Body as text (None for null).
pub fn body_text(ex: &Exchange) -> Option<String> {
    match &ex.message.body {
        Value::Null => None,
        Value::Bytes(b) => Some(String::from_utf8_lossy(b).into_owned()),
        other => Some(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(base64(b"user:pass"), "dXNlcjpwYXNz");
        assert_eq!(base64(b"a"), "YQ==");
        assert_eq!(join_url("http://h/api/", "/x"), "http://h/api/x");
        assert_eq!(query(&[("a b", "c&d".into())]), "a%20b=c%26d");
        assert_eq!(strip_secrets("https://api.telegram.org/bot123:ABC/sendMessage"), "https://api.telegram.org/bot***/sendMessage");
    }
}

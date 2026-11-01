//! Camel HTTP binding: mapping between HTTP messages and exchanges, and the HTTP
//! header filter strategy.

use camel_core::message::{Exchange, ExchangePattern, Headers};
use camel_core::value::Value;

use crate::http::{is_text_content_type, is_valid_header_name, is_valid_header_value, HttpHeaders, HttpRequest, HttpResponse};

/// Header names (Camel `Exchange` constants) used by the HTTP components.
pub mod headers {
    pub const HTTP_METHOD: &str = "CamelHttpMethod";
    pub const HTTP_URI: &str = "CamelHttpUri";
    pub const HTTP_URL: &str = "CamelHttpUrl";
    pub const HTTP_PATH: &str = "CamelHttpPath";
    pub const HTTP_QUERY: &str = "CamelHttpQuery";
    pub const HTTP_RAW_QUERY: &str = "CamelHttpRawQuery";
    pub const HTTP_RESPONSE_CODE: &str = "CamelHttpResponseCode";
    pub const HTTP_RESPONSE_TEXT: &str = "CamelHttpResponseText";
    pub const HTTP_CHUNKED: &str = "CamelHttpChunked";
    pub const HTTP_REMOTE_ADDRESS: &str = "CamelHttpRemoteAddress";
    /// Verified TLS client certificate (mutual TLS) of the request: the leaf in PEM.
    pub const HTTP_SSL_CLIENT_CERT: &str = "CamelHttpSslClientCert";
    /// Prefix of `CamelHttpSslClientCertSubjectName`, `...IssuerName`, `...SerialNo`,
    /// `...NotBefore` and `...NotAfter`.
    pub const HTTP_SSL_CLIENT_CERT_PREFIX: &str = "CamelHttpSslClientCert";
    pub const CONTENT_TYPE: &str = "Content-Type";
    /// Exchange property holding details of an `HttpOperationFailedException`.
    pub const HTTP_OPERATION_FAILED: &str = "CamelHttpOperationFailed";
}

/// Headers never copied between HTTP messages and exchanges in the outbound direction
/// (Camel `HttpHeaderFilterStrategy` + hop-by-hop headers).
const OUT_FILTER: &[&str] = &[
    "content-length",
    "content-type",
    "host",
    "cache-control",
    "connection",
    "date",
    "pragma",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "via",
    "warning",
    "keep-alive",
    "te",
    "expect",
    "proxy-connection",
    "accept-encoding",
    "content-encoding",
];

/// True if a message header must not be written to an HTTP message (Camel internal
/// headers `Camel*` / `org.apache.camel*`, framing and hop-by-hop headers).
pub fn filter_outbound(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("camel") || lower.starts_with("org.apache.camel") || OUT_FILTER.contains(&lower.as_str())
}

/// True if an HTTP header must not be copied into the exchange.
pub fn filter_inbound(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "transfer-encoding" || lower == "connection" || lower == "keep-alive" || lower == "content-length"
}

/// Copy HTTP headers into message headers (repeated headers become arrays).
pub fn copy_http_headers(src: &HttpHeaders, dst: &mut Headers, filter: impl Fn(&str) -> bool) {
    for (k, v) in src.iter() {
        if filter(k) {
            continue;
        }
        add_header_value(dst, k, Value::String(v.to_string()));
    }
}

/// Add a header, turning repeated values into an array.
pub fn add_header_value(dst: &mut Headers, key: &str, v: Value) {
    match dst.get_mut(key) {
        Some(Value::Array(a)) => a.push(v),
        Some(existing) => {
            let old = std::mem::take(existing);
            *existing = Value::Array(vec![old, v]);
        }
        None => dst.set(key, v),
    }
}

/// Write message headers as HTTP headers (filtered; arrays become repeated headers).
pub fn write_message_headers(src: &Headers, dst: &mut HttpHeaders) {
    for (k, v) in src.iter() {
        if filter_outbound(k) || !is_valid_header_name(k) {
            continue;
        }
        let values: Vec<String> = match v {
            Value::Array(a) => a.iter().map(|x| x.to_string()).collect(),
            Value::Null => continue,
            other => vec![other.to_string()],
        };
        for s in values {
            if is_valid_header_value(&s) {
                dst.add(k, s);
            }
        }
    }
}

/// Convert an HTTP body to a message body: empty → null, text content types (or valid
/// UTF-8 without content type) → string, otherwise bytes.
pub fn body_to_value(body: &[u8], content_type: Option<&str>) -> Value {
    if body.is_empty() {
        return Value::Null;
    }
    let text = match content_type {
        Some(ct) => is_text_content_type(ct),
        None => true,
    };
    if text {
        if let Ok(s) = std::str::from_utf8(body) {
            return Value::String(s.to_string());
        }
    }
    Value::Bytes(body.to_vec())
}

/// Convert a message body to HTTP bytes and a default content type.
/// Structured values (objects/arrays) are rendered as JSON (`application/json`).
pub fn value_to_body(v: &Value) -> (Vec<u8>, Option<&'static str>) {
    match v {
        Value::Null => (Vec::new(), None),
        Value::Bytes(b) => (b.clone(), None),
        Value::String(s) => (s.as_bytes().to_vec(), None),
        Value::Object(_) | Value::Array(_) => (v.to_json().into_bytes(), Some("application/json")),
        other => (other.to_string().into_bytes(), Some("text/plain")),
    }
}

/// Options of the consumer-side binding.
#[derive(Clone, Debug)]
pub struct ConsumerBinding {
    pub map_http_message_headers: bool,
    pub map_http_message_body: bool,
    pub map_form_url_encoded_body: bool,
    pub mute_exception: bool,
    pub chunked: bool,
    /// Default response content type (first `produces` entry).
    pub produces: Option<String>,
    /// `disableStreamCache`: request bodies arrive as `camel_tls::stream_body` handles.
    pub disable_stream_cache: bool,
}

impl Default for ConsumerBinding {
    fn default() -> Self {
        ConsumerBinding {
            map_http_message_headers: true,
            map_http_message_body: true,
            disable_stream_cache: false,
            map_form_url_encoded_body: true,
            mute_exception: false,
            chunked: false,
            produces: None,
        }
    }
}

impl ConsumerBinding {
    /// Populate an exchange from a request (Camel `HttpBinding.readRequest`).
    pub fn read_request(&self, req: &HttpRequest, path_params: &[(String, String)], exchange: &mut Exchange) {
        exchange.pattern = ExchangePattern::InOut;
        let msg = &mut exchange.message;
        if self.map_http_message_headers {
            copy_http_headers(&req.headers, &mut msg.headers, filter_inbound);
            for (k, v) in req.query_params() {
                add_header_value(&mut msg.headers, &k, Value::String(v));
            }
            if self.map_form_url_encoded_body {
                for (k, v) in req.form_params() {
                    add_header_value(&mut msg.headers, &k, Value::String(v));
                }
            }
        }
        for (k, v) in path_params {
            msg.headers.set(k.clone(), v.clone());
        }
        msg.headers.set(headers::HTTP_METHOD, req.method.clone());
        msg.headers.set(headers::HTTP_URI, req.target.clone());
        msg.headers.set(headers::HTTP_PATH, req.raw_path.clone());
        if let Some(q) = &req.raw_query {
            msg.headers.set(headers::HTTP_QUERY, crate::http::percent_decode(q, false));
            msg.headers.set(headers::HTTP_RAW_QUERY, q.clone());
        }
        if let Some(a) = req.remote_addr {
            msg.headers.set(headers::HTTP_REMOTE_ADDRESS, a.to_string());
        }
        if let Some(leaf) = req.peer_certificates.first() {
            msg.headers.set(headers::HTTP_SSL_CLIENT_CERT, camel_tls::pem::encode_pem("CERTIFICATE", leaf));
            for (k, v) in camel_tls::camel_support::client_cert_headers(headers::HTTP_SSL_CLIENT_CERT_PREFIX, &req.peer_certificates) {
                msg.headers.set(k, v);
            }
        }
        if let Some(ct) = req.content_type() {
            msg.headers.set(headers::CONTENT_TYPE, ct.to_string());
        }
        if self.map_http_message_body {
            msg.body = match &req.body_stream {
                Some(h) => h.clone(),
                None => body_to_value(&req.body, req.content_type()),
            };
        }
    }

    /// Build the response from a processed exchange (Camel `HttpBinding.writeResponse`).
    pub fn write_response(&self, exchange: &Exchange) -> HttpResponse {
        let msg = &exchange.message;
        let current = msg.headers.get(headers::HTTP_RESPONSE_CODE).and_then(|v| v.as_i64()).filter(|c| (100..=599).contains(c)).map(|c| c as u16);
        if let Some(e) = &exchange.exception {
            let code = current.unwrap_or(500);
            let mut resp = HttpResponse::new(code);
            if !self.mute_exception {
                resp.headers.set("Content-Type", "text/plain; charset=UTF-8");
                let mut text = e.to_string();
                text.push('\n');
                resp.body = text.into_bytes();
            }
            return resp;
        }
        let streamed = camel_tls::stream_body::is_available(&msg.body);
        let (body, default_ct) = if streamed { (Vec::new(), Some("application/octet-stream")) } else { value_to_body(&msg.body) };
        let blank = !streamed && match &msg.body {
            Value::Null => true,
            Value::String(s) => s.trim().is_empty(),
            _ => false,
        };
        let mut code = current.unwrap_or(200);
        if code != 500 && blank {
            code = current.unwrap_or(204);
        }
        let mut resp = HttpResponse::new(code);
        if let Some(text) = msg.headers.get(headers::HTTP_RESPONSE_TEXT) {
            resp.reason = text.to_string();
        }
        write_message_headers(&msg.headers, &mut resp.headers);
        let ct = msg
            .headers
            .get(headers::CONTENT_TYPE)
            .map(|v| v.to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| default_ct.map(|s| s.to_string()))
            .or_else(|| self.produces.as_ref().map(|p| p.split(',').next().unwrap_or("").trim().to_string()));
        if let Some(ct) = ct {
            if (streamed || !body.is_empty()) && is_valid_header_value(&ct) {
                resp.headers.set("Content-Type", ct);
            }
        }
        resp.chunked = self.chunked || msg.headers.get(headers::HTTP_CHUNKED).map(|v| v.is_truthy()).unwrap_or(false);
        resp.body = body;
        if streamed {
            resp.stream = Some(msg.body.clone());
            resp.chunked = true;
        }
        resp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_mapping() {
        let mut req = HttpRequest::new("POST", "/a/b?x=1&x=2&y=hello%20world");
        req.headers.add("Content-Type", "application/json");
        req.headers.add("Host", "h");
        req.body = b"{\"a\":1}".to_vec();
        let mut ex = Exchange::default();
        ConsumerBinding::default().read_request(&req, &[("id".into(), "7".into())], &mut ex);
        assert_eq!(ex.header("x"), Some(&Value::Array(vec!["1".into(), "2".into()])));
        assert_eq!(ex.header("y"), Some(&Value::from("hello world")));
        assert_eq!(ex.header("id"), Some(&Value::from("7")));
        assert_eq!(ex.header(headers::HTTP_METHOD), Some(&Value::from("POST")));
        assert_eq!(ex.header(headers::HTTP_PATH), Some(&Value::from("/a/b")));
        assert_eq!(ex.body(), &Value::from("{\"a\":1}"));
    }

    #[test]
    fn response_mapping() {
        let b = ConsumerBinding::default();
        let mut ex = Exchange::default();
        assert_eq!(b.write_response(&ex).status, 204);
        ex.set_body(Value::object([("a", 1)]));
        ex.set_header("CamelFoo", "x");
        ex.set_header("X-Custom", "y");
        let r = b.write_response(&ex);
        assert_eq!(r.status, 200);
        assert_eq!(r.headers.get("Content-Type"), Some("application/json"));
        assert_eq!(r.headers.get("X-Custom"), Some("y"));
        assert!(r.headers.get("CamelFoo").is_none());
        ex.set_header(headers::HTTP_RESPONSE_CODE, 201);
        assert_eq!(b.write_response(&ex).status, 201);
        ex.set_exception(camel_core::error::CamelError::illegal_argument("boom"));
        ex.message.headers.remove(headers::HTTP_RESPONSE_CODE);
        let r = b.write_response(&ex);
        assert_eq!(r.status, 500);
        assert!(r.body_text().contains("boom"));
    }
}

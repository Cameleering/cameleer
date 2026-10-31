//! `http` producer component (`https` is rejected: no TLS in a std-only build).
//!
//! `http://host:port/path?httpMethod=POST&throwExceptionOnFailure=true&...`
//!
//! Supported options: `httpMethod`, `bridgeEndpoint`, `throwExceptionOnFailure`,
//! `okStatusCodeRange`, `connectTimeout`, `socketTimeout`/`soTimeout`/`responseTimeout`,
//! `followRedirects`, `copyHeaders`, `skipRequestHeaders`, `skipResponseHeaders`,
//! `userAgent`, `authUsername`/`authPassword`/`authMethod` (Basic), `chunked`,
//! `getWithBody`, `deleteWithBody`, `ignoreResponseBody`, `customHostHeader`,
//! `preserveHostHeader` (and some accepted-and-ignored options). Any other query
//! parameter is forwarded to the target URL.
//!
//! Message mapping: the method comes from header `CamelHttpMethod`, else the
//! `httpMethod` option, else `POST` when there is a body and `GET` otherwise. Headers
//! `CamelHttpUri` (ignored with `bridgeEndpoint`), `CamelHttpPath` (appended to the
//! endpoint path) and `CamelHttpQuery`/`CamelHttpRawQuery` (replace the query) override the
//! target. Non-Camel headers are sent as request headers. The response body becomes the
//! message body; `CamelHttpResponseCode`, `CamelHttpResponseText` and the response headers
//! are set. Responses outside `okStatusCodeRange` fail with `HttpOperationFailedException`
//! when `throwExceptionOnFailure=true`; see [`HttpOperationFailed`].

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use camel_core::context::CamelContext;
use camel_core::error::{kinds, CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::{Component, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::value::Value;

use crate::binding::{add_header_value, body_to_value, headers, value_to_body, write_message_headers};
use crate::client::{https_not_supported, HttpClient, Url};
use crate::http::{base64_encode, percent_encode, HttpHeaders};

/// The `http` (and erroring `https`) component.
pub struct HttpComponent {
    scheme: String,
}

impl HttpComponent {
    pub fn new(scheme: &str) -> Self {
        HttpComponent { scheme: scheme.to_string() }
    }
}

/// Endpoint options of the `http` producer.
#[derive(Clone, Debug)]
pub struct HttpProducerOptions {
    pub http_method: Option<String>,
    pub bridge_endpoint: bool,
    pub throw_exception_on_failure: bool,
    pub ok_status_code_range: Vec<(u16, u16)>,
    pub connect_timeout: Duration,
    pub socket_timeout: Duration,
    pub follow_redirects: bool,
    pub copy_headers: bool,
    pub skip_request_headers: bool,
    pub skip_response_headers: bool,
    pub user_agent: Option<String>,
    pub auth: Option<(String, String)>,
    pub chunked: bool,
    pub get_with_body: bool,
    pub delete_with_body: bool,
    pub ignore_response_body: bool,
    pub custom_host_header: Option<String>,
    pub preserve_host_header: bool,
}

fn parse_ranges(s: &str) -> Result<Vec<(u16, u16)>> {
    s.split(',')
        .map(|r| {
            let r = r.trim();
            let (a, b) = r.split_once('-').unwrap_or((r, r));
            let a: u16 = a.trim().parse().map_err(|_| CamelError::illegal_argument(format!("Invalid okStatusCodeRange: {}", s)))?;
            let b: u16 = b.trim().parse().map_err(|_| CamelError::illegal_argument(format!("Invalid okStatusCodeRange: {}", s)))?;
            Ok((a, b))
        })
        .collect()
}

impl Component for HttpComponent {
    fn create_endpoint(&self, _ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        if self.scheme.eq_ignore_ascii_case("https") || remaining.starts_with("https://") {
            return Err(https_not_supported(uri));
        }
        let remaining = remaining.strip_prefix("http://").unwrap_or(remaining);
        let mut base = Url::parse(&format!("http://{}", remaining))?;
        let socket_timeout = match params.take("socketTimeout").or_else(|| params.take("soTimeout")).or_else(|| params.take("responseTimeout")) {
            Some(v) => camel_core::util::parse_duration(&v)?,
            None => Duration::from_secs(60),
        };
        let auth_user = params.take("authUsername");
        let auth_pass = params.take("authPassword").unwrap_or_default();
        if let Some(m) = params.take("authMethod") {
            if !m.eq_ignore_ascii_case("basic") {
                return Err(CamelError::illegal_argument(format!("Unsupported authMethod: {} (only Basic is supported)", m)));
            }
        }
        let opts = HttpProducerOptions {
            http_method: params.take("httpMethod").map(|m| m.to_ascii_uppercase()),
            bridge_endpoint: params.take_bool("bridgeEndpoint", false)?,
            throw_exception_on_failure: params.take_bool("throwExceptionOnFailure", true)?,
            ok_status_code_range: parse_ranges(&params.take_string("okStatusCodeRange", "200-299"))?,
            connect_timeout: params.take_duration("connectTimeout", Duration::from_secs(30))?,
            socket_timeout,
            follow_redirects: params.take_bool("followRedirects", false)?,
            copy_headers: params.take_bool("copyHeaders", true)?,
            skip_request_headers: params.take_bool("skipRequestHeaders", false)?,
            skip_response_headers: params.take_bool("skipResponseHeaders", false)?,
            user_agent: params.take("userAgent"),
            auth: auth_user.map(|u| (u, auth_pass)),
            chunked: params.take_bool("chunked", false)?,
            get_with_body: params.take_bool("getWithBody", false)?,
            delete_with_body: params.take_bool("deleteWithBody", false)?,
            ignore_response_body: params.take_bool("ignoreResponseBody", false)?,
            custom_host_header: params.take("customHostHeader"),
            preserve_host_header: params.take_bool("preserveHostHeader", false)?,
        };
        for ignored in [
            "connectionRequestTimeout",
            "disableStreamCache",
            "headerFilterStrategy",
            "httpClientConfigurer",
            "clientConnectionManager",
            "connectionClose",
            "connectionsPerRoute",
            "maxTotalConnections",
            "cookieHandler",
            "clearExpiredCookies",
            "transferException",
            "lazyStartProducer",
            "useSystemProperties",
            "httpBinding",
        ] {
            params.take(ignored);
        }
        // forward unknown parameters as query parameters
        let rest: BTreeMap<String, String> = params.take_all();
        if !rest.is_empty() {
            let extra = rest.iter().map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v))).collect::<Vec<_>>().join("&");
            base.query = Some(match base.query.take() {
                Some(q) if !q.is_empty() => format!("{}&{}", q, extra),
                _ => extra,
            });
        }
        Ok(Arc::new(HttpEndpoint { uri: uri.to_string(), base, opts }))
    }

    fn lenient_properties(&self) -> bool {
        true
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// An `http` producer endpoint.
pub struct HttpEndpoint {
    uri: String,
    base: Url,
    opts: HttpProducerOptions,
}

impl HttpEndpoint {
    /// The target URL (including forwarded query parameters).
    pub fn target(&self) -> &Url {
        &self.base
    }
    pub fn options(&self) -> &HttpProducerOptions {
        &self.opts
    }
}

impl Endpoint for HttpEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(HttpProducer { base: self.base.clone(), opts: self.opts.clone() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Sends exchanges as HTTP requests.
pub struct HttpProducer {
    base: Url,
    opts: HttpProducerOptions,
}

impl Service for HttpProducer {}

impl HttpProducer {
    fn target_url(&self, ex: &Exchange) -> Result<Url> {
        let mut url = self.base.clone();
        if !self.opts.bridge_endpoint {
            if let Some(u) = ex.header(headers::HTTP_URI).map(|v| v.to_string()) {
                let lower = u.to_ascii_lowercase();
                if lower.starts_with("https://") {
                    return Err(https_not_supported(&u));
                }
                if lower.starts_with("http://") {
                    url = Url::parse(&u)?;
                }
            }
        }
        if let Some(p) = ex.header(headers::HTTP_PATH).map(|v| v.to_string()).filter(|p| !p.is_empty()) {
            let base = url.path.trim_end_matches('/');
            url.path = format!("{}/{}", base, p.trim_start_matches('/'));
        }
        if let Some(q) = ex.header(headers::HTTP_RAW_QUERY).or_else(|| ex.header(headers::HTTP_QUERY)).map(|v| v.to_string()) {
            url.query = if q.is_empty() { None } else { Some(encode_query_string(&q)) };
        }
        Ok(url)
    }
}

/// Make a query string safe for the request line (encode spaces and other unsafe
/// characters, keep `=`/`&` and existing escapes).
fn encode_query_string(q: &str) -> String {
    let mut out = String::with_capacity(q.len());
    for b in q.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~=&%+/:,;@!$'()*?".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// Details of a failed HTTP operation (Camel `HttpOperationFailedException`).
#[derive(Clone, Debug, PartialEq)]
pub struct HttpOperationFailed {
    pub uri: String,
    pub status_code: u16,
    pub status_text: String,
    pub redirect_location: Option<String>,
    pub response_body: Option<String>,
    pub response_headers: BTreeMap<String, String>,
}

impl HttpOperationFailed {
    /// Read the details stored on an exchange that failed with `HttpOperationFailedException`.
    pub fn from_exchange(ex: &Exchange) -> Option<HttpOperationFailed> {
        let v = ex.property(headers::HTTP_OPERATION_FAILED)?;
        let s = |k: &str| v.get(k).filter(|x| !x.is_null()).map(|x| x.to_string());
        Some(HttpOperationFailed {
            uri: s("uri").unwrap_or_default(),
            status_code: v.get("statusCode").and_then(|x| x.as_i64()).unwrap_or(0) as u16,
            status_text: s("statusText").unwrap_or_default(),
            redirect_location: s("redirectLocation"),
            response_body: s("responseBody"),
            response_headers: v
                .get("responseHeaders")
                .and_then(|h| h.as_object())
                .map(|o| o.iter().map(|(k, v)| (k.clone(), v.to_string())).collect())
                .unwrap_or_default(),
        })
    }

    /// The status code encoded in an `HttpOperationFailedException` message.
    pub fn status_code_of(err: &CamelError) -> Option<u16> {
        let e = err.chain().find(|e| e.kind == kinds::HTTP_OPERATION_FAILED)?;
        let i = e.message.find("statusCode: ")? + "statusCode: ".len();
        e.message[i..].chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().ok()
    }

    fn to_value(&self) -> Value {
        let mut m = BTreeMap::new();
        m.insert("uri".to_string(), Value::from(self.uri.clone()));
        m.insert("statusCode".to_string(), Value::Int(self.status_code as i64));
        m.insert("statusText".to_string(), Value::from(self.status_text.clone()));
        m.insert("redirectLocation".to_string(), self.redirect_location.clone().map(Value::from).unwrap_or(Value::Null));
        m.insert("responseBody".to_string(), self.response_body.clone().map(Value::from).unwrap_or(Value::Null));
        m.insert(
            "responseHeaders".to_string(),
            Value::Object(self.response_headers.iter().map(|(k, v)| (k.clone(), Value::from(v.clone()))).collect()),
        );
        Value::Object(m)
    }
}

impl Processor for HttpProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let url = self.target_url(ex)?;
        let has_body = !ex.body().is_null();
        let method = ex
            .header(headers::HTTP_METHOD)
            .map(|v| v.to_string().to_ascii_uppercase())
            .filter(|m| !m.is_empty())
            .or_else(|| self.opts.http_method.clone())
            .unwrap_or_else(|| if has_body { "POST".into() } else { "GET".into() });
        let send_body = has_body
            && match method.as_str() {
                "GET" => self.opts.get_with_body,
                "DELETE" => self.opts.delete_with_body,
                "HEAD" | "OPTIONS" | "TRACE" => false,
                _ => true,
            };
        let (body, default_ct) = value_to_body(ex.body());
        let mut req_headers = HttpHeaders::new();
        if self.opts.copy_headers && !self.opts.skip_request_headers {
            write_message_headers(&ex.message.headers, &mut req_headers);
        }
        if send_body {
            let ct = ex.header(headers::CONTENT_TYPE).map(|v| v.to_string()).filter(|s| !s.is_empty()).or_else(|| default_ct.map(String::from));
            if let Some(ct) = ct {
                req_headers.set("Content-Type", ct);
            }
        }
        if let Some(h) = &self.opts.custom_host_header {
            req_headers.set("Host", h.clone());
        } else if self.opts.preserve_host_header {
            if let Some(h) = ex.header("Host").map(|v| v.to_string()) {
                req_headers.set("Host", h);
            }
        }
        if !req_headers.contains("User-Agent") {
            req_headers.set("User-Agent", self.opts.user_agent.clone().unwrap_or_else(|| format!("rust-camel/{}", env!("CARGO_PKG_VERSION"))));
        }
        if let Some((u, p)) = &self.opts.auth {
            req_headers.set("Authorization", format!("Basic {}", base64_encode(format!("{}:{}", u, p).as_bytes())));
        }
        let client = HttpClient {
            connect_timeout: self.opts.connect_timeout,
            read_timeout: self.opts.socket_timeout,
            follow_redirects: self.opts.follow_redirects,
            chunked_requests: self.opts.chunked,
            ..HttpClient::default()
        };
        let resp = client.execute(&method, &url, &req_headers, if send_body { Some(&body) } else { None }, None)?;

        // populate the response
        let ct = resp.headers.get("Content-Type").map(|s| s.to_string());
        let msg = &mut ex.message;
        if !self.opts.skip_response_headers {
            let mut seen: Vec<String> = Vec::new();
            for (k, v) in resp.headers.iter() {
                if crate::binding::filter_inbound(k) {
                    continue;
                }
                let lower = k.to_ascii_lowercase();
                if !seen.contains(&lower) {
                    msg.headers.remove(k);
                    seen.push(lower);
                }
                add_header_value(&mut msg.headers, k, Value::String(v.to_string()));
            }
        }
        msg.headers.set(headers::HTTP_RESPONSE_CODE, resp.status as i64);
        msg.headers.set(headers::HTTP_RESPONSE_TEXT, resp.reason_phrase().to_string());
        msg.body = if self.opts.ignore_response_body { Value::Null } else { body_to_value(&resp.body, ct.as_deref()) };

        let ok = self.opts.ok_status_code_range.iter().any(|(a, b)| (*a..=*b).contains(&resp.status));
        if !ok && self.opts.throw_exception_on_failure {
            let location = resp.headers.get("Location").map(|s| s.to_string());
            let info = HttpOperationFailed {
                uri: url.to_string(),
                status_code: resp.status,
                status_text: resp.reason_phrase().to_string(),
                redirect_location: location.clone(),
                response_body: if resp.body.is_empty() { None } else { Some(resp.body_text()) },
                response_headers: resp.headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            };
            ex.set_property(headers::HTTP_OPERATION_FAILED, info.to_value());
            let mut message = format!("HTTP operation failed invoking {} with statusCode: {}", url, resp.status);
            if let Some(l) = location {
                message.push_str(&format!(", redirectLocation: {}", l));
            }
            return Err(CamelError::new(kinds::HTTP_OPERATION_FAILED, message));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(parse_ranges("200-299").unwrap(), vec![(200, 299)]);
        assert_eq!(parse_ranges("200-204,209,301-304").unwrap(), vec![(200, 204), (209, 209), (301, 304)]);
        assert!(parse_ranges("abc").is_err());
    }

    #[test]
    fn status_code_from_error() {
        let e = CamelError::new(kinds::HTTP_OPERATION_FAILED, "HTTP operation failed invoking http://x/ with statusCode: 404");
        assert_eq!(HttpOperationFailed::status_code_of(&e), Some(404));
        assert_eq!(encode_query_string("a=b c&d=é"), "a=b%20c&d=%C3%A9");
    }
}

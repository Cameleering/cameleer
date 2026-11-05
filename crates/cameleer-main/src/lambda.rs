//! AWS Lambda custom runtime (`provided.al2023`): `cameleer` serves Lambda invocations itself,
//! with no runtime library or adapter in front of it.
//!
//! When `AWS_LAMBDA_RUNTIME_API` is set (Lambda sets it for custom runtimes), `cameleer run`
//! starts the routes as usual and then, instead of waiting for a signal, loops over the
//! [Lambda Runtime API](https://docs.aws.amazon.com/lambda/latest/dg/runtimes-api.html):
//!
//! 1. `GET /2018-06-01/runtime/invocation/next` blocks until there is an event;
//! 2. the event is handled (below);
//! 3. the result goes to `.../invocation/<id>/response`, a failure to `.../invocation/<id>/error`.
//!
//! A failure while starting (bad route file, unknown component) is reported to
//! `/runtime/init/error` before the process exits, so it shows up in the function's logs and
//! the invoke response.
//!
//! **How an event is handled.**
//! - HTTP events (API Gateway REST and HTTP APIs, function URLs, ALB) go to the `platform-http`
//!   routes, exactly as if the request had come over a socket; the HTTP response becomes the
//!   proxy-integration response (`statusCode`, `headers`, `body`, `isBase64Encoded`). The same
//!   route file runs on Lambda and in a container.
//! - Any other event (S3, SQS, EventBridge, a direct invoke) is sent InOut to the handler
//!   endpoint with the event JSON as the body: `camel.main.lambda-handler` if set, else the
//!   function's handler setting (`_HANDLER`) when it is an endpoint URI (contains `:`), else
//!   `direct:lambda`. The reply body is the function's result (strings as they are, structured
//!   values as JSON).
//!
//! Headers on the exchange: `CamelAwsLambdaRequestId`, `CamelAwsLambdaDeadlineMs` (epoch ms),
//! `CamelAwsLambdaFunctionArn`, `CamelAwsLambdaTraceId`, and when present
//! `CamelAwsLambdaClientContext` / `CamelAwsLambdaCognitoIdentity`.
//!
//! Deploy: name the binary `bootstrap` (it then runs the route files of `$LAMBDA_TASK_ROOT`
//! with no arguments) or use a `bootstrap` script that runs `cameleer run <files>`. See
//! `docs/DEPLOYMENT.md`.

use std::time::Duration;

use cameleer_core::prelude::*;
use cameleer_core::value::Value;
use cameleer_http::http::{HttpRequest, HttpResponse};
use cameleer_http::HttpClient;

/// Environment variable Lambda sets to `host:port` of the Runtime API.
pub const RUNTIME_API_ENV: &str = "AWS_LAMBDA_RUNTIME_API";
const API_VERSION: &str = "2018-06-01";
/// The handler endpoint when neither `camel.main.lambda-handler` nor `_HANDLER` names one.
pub const DEFAULT_HANDLER: &str = "direct:lambda";

/// Whether this process runs as a Lambda custom runtime.
pub fn runtime_api() -> Option<String> {
    std::env::var(RUNTIME_API_ENV).ok().filter(|v| !v.trim().is_empty())
}

/// The endpoint non-HTTP events are sent to (see the module documentation).
pub fn handler_endpoint(property: Option<&str>, handler_env: Option<&str>) -> String {
    if let Some(p) = property.map(str::trim).filter(|p| !p.is_empty()) {
        return p.to_string();
    }
    match handler_env.map(str::trim) {
        Some(h) if h.contains(':') => h.to_string(),
        _ => DEFAULT_HANDLER.to_string(),
    }
}

/// A client of the Lambda Runtime API.
pub struct LambdaRuntime {
    base: String,
    client: HttpClient,
    handler: String,
}

/// One invocation, as returned by `/invocation/next`.
#[derive(Debug, Clone, Default)]
pub struct Invocation {
    pub request_id: String,
    pub deadline_ms: Option<i64>,
    pub function_arn: Option<String>,
    pub trace_id: Option<String>,
    pub client_context: Option<String>,
    pub cognito_identity: Option<String>,
    pub event: Vec<u8>,
}

/// What an invocation produced.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Response(Vec<u8>),
    /// (errorType, errorMessage)
    Error(String, String),
}

impl LambdaRuntime {
    /// `api` is `host:port` (the value of `AWS_LAMBDA_RUNTIME_API`).
    pub fn new(api: &str, handler: impl Into<String>) -> Self {
        let mut client = HttpClient::new();
        client.connect_timeout = Duration::from_secs(5);
        LambdaRuntime { base: format!("http://{}/{}/runtime", api.trim(), API_VERSION), client, handler: handler.into() }
    }

    pub fn handler(&self) -> &str {
        &self.handler
    }

    /// Report a startup failure (`/runtime/init/error`).
    pub fn init_error(&self, error: &CamelError) -> Result<()> {
        let body = error_json(&error.kind, &error.message);
        self.post(&format!("{}/init/error", self.base), &body, Some(&error.kind)).map(|_| ())
    }

    /// Wait for the next invocation. The Runtime API holds the request until an event
    /// arrives, and Lambda freezes the process in between, so there is no read timeout to
    /// speak of: a day is long enough for any wait.
    pub fn next(&self) -> Result<Invocation> {
        let url = format!("{}/invocation/next", self.base);
        let resp = self.client.request("GET", &url, &[], None, Some(Duration::from_secs(86_400)))?;
        if resp.status != 200 {
            return Err(CamelError::io(format!("Lambda Runtime API {} answered {}: {}", url, resp.status, resp.body_text())));
        }
        let h = |n: &str| resp.headers.get(n).map(str::to_string);
        let request_id = h("Lambda-Runtime-Aws-Request-Id").ok_or_else(|| CamelError::io("Lambda Runtime API: no Lambda-Runtime-Aws-Request-Id header"))?;
        Ok(Invocation {
            request_id,
            deadline_ms: h("Lambda-Runtime-Deadline-Ms").and_then(|v| v.trim().parse().ok()),
            function_arn: h("Lambda-Runtime-Invoked-Function-Arn"),
            trace_id: h("Lambda-Runtime-Trace-Id"),
            client_context: h("Lambda-Runtime-Client-Context"),
            cognito_identity: h("Lambda-Runtime-Cognito-Identity"),
            event: resp.body,
        })
    }

    /// Send the outcome of an invocation.
    pub fn complete(&self, request_id: &str, outcome: &Outcome) -> Result<()> {
        match outcome {
            Outcome::Response(body) => self.post(&format!("{}/invocation/{}/response", self.base, request_id), body, None),
            Outcome::Error(kind, message) => {
                self.post(&format!("{}/invocation/{}/error", self.base, request_id), &error_json(kind, message), Some(kind))
            }
        }
        .map(|_| ())
    }

    fn post(&self, url: &str, body: &[u8], error_type: Option<&str>) -> Result<HttpResponse> {
        let mut headers = vec![("Content-Type", "application/json")];
        if let Some(t) = error_type {
            headers.push(("Lambda-Runtime-Function-Error-Type", t));
        }
        let resp = self.client.request("POST", url, &headers, Some(body), Some(Duration::from_secs(30)))?;
        if resp.status >= 300 {
            // 413 (payload too large) and 400 are not retried: the invocation is lost either way
            cameleer_core::camel_warn!("Lambda", "Runtime API {} answered {}: {}", url, resp.status, resp.body_text());
        }
        Ok(resp)
    }

    /// Handle one invocation with the context's routes.
    pub fn handle(&self, ctx: &CamelContext, inv: &Invocation) -> Outcome {
        let text = String::from_utf8_lossy(&inv.event);
        if let Ok(event) = cameleer_core::json::parse(&text) {
            if let Some(mut req) = http_request(&event) {
                if let Some(resp) = cameleer_http::registry::registry().dispatch(&mut req) {
                    return Outcome::Response(proxy_response(&event, &resp).to_json().into_bytes());
                }
            }
        }
        let template = ctx.create_producer_template();
        let result = template.request(&self.handler, |ex| {
            ex.message.body = Value::String(text.into_owned());
            ex.message.set_header("CamelAwsLambdaRequestId", inv.request_id.clone());
            if let Some(d) = inv.deadline_ms {
                ex.message.set_header("CamelAwsLambdaDeadlineMs", Value::Int(d));
            }
            for (name, v) in [
                ("CamelAwsLambdaFunctionArn", &inv.function_arn),
                ("CamelAwsLambdaTraceId", &inv.trace_id),
                ("CamelAwsLambdaClientContext", &inv.client_context),
                ("CamelAwsLambdaCognitoIdentity", &inv.cognito_identity),
            ] {
                if let Some(v) = v {
                    ex.message.set_header(name, v.clone());
                }
            }
        });
        match result {
            Ok(ex) => Outcome::Response(body_bytes(&ex.message.body)),
            Err(e) => Outcome::Error(e.kind.clone(), e.message.clone()),
        }
    }

    /// Serve invocations until `stop()` says so or `max` invocations were handled.
    /// Returns the number handled.
    pub fn run(&self, ctx: &CamelContext, max: Option<u64>, stop: &dyn Fn() -> bool) -> Result<u64> {
        let mut handled = 0u64;
        let mut failures = 0u32;
        while !stop() && max.map(|m| handled < m).unwrap_or(true) {
            let inv = match self.next() {
                Ok(inv) => {
                    failures = 0;
                    inv
                }
                Err(e) => {
                    // the Runtime API is local and always there; give up after repeated failures
                    failures += 1;
                    if failures >= 5 {
                        return Err(e);
                    }
                    cameleer_core::camel_warn!("Lambda", "Cannot get the next invocation: {}", e);
                    std::thread::sleep(Duration::from_millis(50 * failures as u64));
                    continue;
                }
            };
            let outcome = self.handle(ctx, &inv);
            if let Outcome::Error(kind, message) = &outcome {
                cameleer_core::camel_error!("Lambda", "Invocation {} failed: {}: {}", inv.request_id, kind, message);
            }
            if let Err(e) = self.complete(&inv.request_id, &outcome) {
                cameleer_core::camel_warn!("Lambda", "Cannot report invocation {}: {}", inv.request_id, e);
            }
            handled += 1;
        }
        Ok(handled)
    }
}

fn error_json(kind: &str, message: &str) -> Vec<u8> {
    Value::object([("errorMessage", Value::String(message.to_string())), ("errorType", Value::String(kind.to_string()))]).to_json().into_bytes()
}

/// The function result for a reply body: bytes and strings as they are, structured values as
/// JSON, no body as an empty payload.
fn body_bytes(body: &Value) -> Vec<u8> {
    match body {
        Value::Object(_) | Value::Array(_) => body.to_json().into_bytes(),
        other => other.to_bytes(),
    }
}

fn get_str<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut cur = v;
    for p in path {
        cur = cur.get(p)?;
    }
    cur.as_str()
}

/// The HTTP request in an API Gateway (REST `1.0` or HTTP API `2.0` payload), function URL or
/// ALB event; `None` for other events.
pub fn http_request(event: &Value) -> Option<HttpRequest> {
    event.as_object()?;
    let v2 = get_str(event, &["version"]) == Some("2.0");
    let (method, path, query) = if v2 {
        let method = get_str(event, &["requestContext", "http", "method"])?;
        let path = get_str(event, &["rawPath"]).or_else(|| get_str(event, &["requestContext", "http", "path"]))?;
        (method.to_string(), path.to_string(), get_str(event, &["rawQueryString"]).unwrap_or("").to_string())
    } else {
        let method = get_str(event, &["httpMethod"])?;
        event.get("requestContext")?;
        let path = get_str(event, &["path"])?;
        // multiValueQueryStringParameters keeps repeated keys; queryStringParameters is the fallback
        let mut pairs: Vec<(String, String)> = Vec::new();
        if let Some(m) = event.get("multiValueQueryStringParameters").and_then(|m| m.as_object()) {
            for (k, vs) in m {
                for v in vs.as_array().into_iter().flatten() {
                    pairs.push((k.clone(), v.as_str().unwrap_or("").to_string()));
                }
            }
        } else if let Some(m) = event.get("queryStringParameters").and_then(|m| m.as_object()) {
            for (k, v) in m {
                pairs.push((k.clone(), v.as_str().unwrap_or("").to_string()));
            }
        }
        let query = pairs.iter().map(|(k, v)| format!("{}={}", encode_query(k), encode_query(v))).collect::<Vec<_>>().join("&");
        (method.to_string(), path.to_string(), query)
    };
    let target = if query.is_empty() { path } else { format!("{}?{}", path, query) };
    let mut req = HttpRequest::new(&method, &target);
    if let Some(h) = event.get("multiValueHeaders").and_then(|h| h.as_object()) {
        for (k, vs) in h {
            for v in vs.as_array().into_iter().flatten() {
                req.headers.add(k.clone(), v.as_str().unwrap_or("").to_string());
            }
        }
    } else if let Some(h) = event.get("headers").and_then(|h| h.as_object()) {
        for (k, v) in h {
            if let Some(v) = v.as_str() {
                req.headers.add(k.clone(), v.to_string());
            }
        }
    }
    if v2 {
        // HTTP API payload 2.0 moves cookies out of the headers
        if let Some(c) = event.get("cookies").and_then(|c| c.as_array()) {
            let cookies: Vec<&str> = c.iter().filter_map(|c| c.as_str()).collect();
            if !cookies.is_empty() {
                req.headers.set("Cookie", cookies.join("; "));
            }
        }
    }
    if let Some(body) = get_str(event, &["body"]) {
        let b64 = event.get("isBase64Encoded").and_then(|b| b.as_bool()).unwrap_or(false);
        req.body = if b64 { cameleer_core::dataformat::base64::decode(body).unwrap_or_default() } else { body.as_bytes().to_vec() };
    }
    Some(req)
}

fn encode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// The proxy-integration response for an HTTP response: text bodies as they are, binary
/// bodies base64-encoded; repeated headers as `multiValueHeaders` (REST API) or joined (HTTP
/// API, where `Set-Cookie` goes to `cookies`).
pub fn proxy_response(event: &Value, resp: &HttpResponse) -> Value {
    let v2 = get_str(event, &["version"]) == Some("2.0");
    let text = std::str::from_utf8(&resp.body).ok().filter(|_| !is_binary(resp.headers.get("Content-Type")));
    let (body, b64) = match text {
        Some(t) => (t.to_string(), false),
        None => (cameleer_core::dataformat::base64::encode(&resp.body, false), true),
    };
    let mut headers: Vec<(String, Value)> = Vec::new();
    let mut multi: Vec<(String, Value)> = Vec::new();
    let mut cookies: Vec<Value> = Vec::new();
    for (k, v) in resp.headers.iter() {
        if v2 && k.eq_ignore_ascii_case("Set-Cookie") {
            cookies.push(Value::String(v.to_string()));
            continue;
        }
        match headers.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case(k)) {
            Some((_, Value::String(existing))) => {
                existing.push_str(", ");
                existing.push_str(v);
            }
            _ => headers.push((k.to_string(), Value::String(v.to_string()))),
        }
        match multi.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case(k)) {
            Some((_, Value::Array(a))) => a.push(Value::String(v.to_string())),
            _ => multi.push((k.to_string(), Value::Array(vec![Value::String(v.to_string())]))),
        }
    }
    let mut out: Vec<(String, Value)> = vec![
        ("statusCode".into(), Value::Int(resp.status as i64)),
        ("headers".into(), Value::object(headers)),
        ("body".into(), Value::String(body)),
        ("isBase64Encoded".into(), Value::Bool(b64)),
    ];
    if v2 {
        if !cookies.is_empty() {
            out.push(("cookies".into(), Value::Array(cookies)));
        }
    } else if event.get("multiValueHeaders").is_some() {
        out.push(("multiValueHeaders".into(), Value::object(multi)));
    }
    Value::object(out)
}

fn is_binary(content_type: Option<&str>) -> bool {
    let Some(ct) = content_type else { return false };
    let ct = ct.to_ascii_lowercase();
    !(ct.starts_with("text/") || ct.contains("json") || ct.contains("xml") || ct.contains("javascript") || ct.contains("x-www-form-urlencoded") || ct.contains("yaml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handler_endpoint_resolution() {
        assert_eq!(handler_endpoint(Some("direct:mine"), Some("seda:x")), "direct:mine");
        assert_eq!(handler_endpoint(None, Some("direct:fn")), "direct:fn");
        assert_eq!(handler_endpoint(None, Some("bootstrap")), DEFAULT_HANDLER);
        assert_eq!(handler_endpoint(Some("  "), None), DEFAULT_HANDLER);
    }

    #[test]
    fn http_api_v2_event_becomes_a_request() {
        let ev = cameleer_core::json::parse(
            r#"{"version":"2.0","rawPath":"/orders/7","rawQueryString":"a=1&b=x%20y","cookies":["s=1","t=2"],
                "headers":{"content-type":"application/json","x-k":"v"},
                "requestContext":{"http":{"method":"POST","path":"/orders/7"}},
                "body":"eyJhIjoxfQ==","isBase64Encoded":true}"#,
        )
        .unwrap();
        let r = http_request(&ev).unwrap();
        assert_eq!(r.method, "POST");
        assert_eq!(r.path, "/orders/7");
        assert_eq!(r.query_param("b").as_deref(), Some("x y"));
        assert_eq!(r.headers.get("Content-Type"), Some("application/json"));
        assert_eq!(r.headers.get("Cookie"), Some("s=1; t=2"));
        assert_eq!(r.body_text(), r#"{"a":1}"#);
    }

    #[test]
    fn rest_api_v1_event_becomes_a_request() {
        let ev = cameleer_core::json::parse(
            r#"{"httpMethod":"GET","path":"/hi","requestContext":{"stage":"prod"},
                "multiValueQueryStringParameters":{"tag":["a","b c"]},
                "multiValueHeaders":{"Accept":["text/plain"]},"body":null}"#,
        )
        .unwrap();
        let r = http_request(&ev).unwrap();
        assert_eq!(r.method, "GET");
        assert_eq!(r.raw_query.as_deref(), Some("tag=a&tag=b%20c"));
        assert_eq!(r.headers.get("accept"), Some("text/plain"));
        assert!(r.body.is_empty());
    }

    #[test]
    fn other_events_are_not_http() {
        for e in [r#"{"Records":[{"eventSource":"aws:s3"}]}"#, r#""just a string""#, r#"{"httpMethod":"GET"}"#] {
            assert!(http_request(&cameleer_core::json::parse(e).unwrap()).is_none(), "{e}");
        }
    }

    #[test]
    fn proxy_responses() {
        let v2 = cameleer_core::json::parse(r#"{"version":"2.0"}"#).unwrap();
        let mut r = HttpResponse::new(201);
        r.headers.add("Content-Type", "application/json");
        r.headers.add("Set-Cookie", "a=1");
        r.body = br#"{"ok":true}"#.to_vec();
        let p = proxy_response(&v2, &r);
        assert_eq!(p.get("statusCode"), Some(&Value::Int(201)));
        assert_eq!(get_str(&p, &["body"]), Some(r#"{"ok":true}"#));
        assert_eq!(p.get("isBase64Encoded"), Some(&Value::Bool(false)));
        assert_eq!(p.get("cookies").and_then(|c| c.as_array()).map(|c| c.len()), Some(1));
        assert!(p.get("headers").unwrap().get("Set-Cookie").is_none());
        let mut bin = HttpResponse::new(200);
        bin.headers.add("Content-Type", "image/png");
        bin.body = vec![0x89, b'P', b'N', b'G'];
        let p = proxy_response(&v2, &bin);
        assert_eq!(p.get("isBase64Encoded"), Some(&Value::Bool(true)));
        assert_eq!(get_str(&p, &["body"]), Some("iVBORw=="));
    }

    #[test]
    fn reply_bodies() {
        assert_eq!(body_bytes(&Value::String("hi".into())), b"hi");
        assert_eq!(body_bytes(&Value::Null), b"");
        assert_eq!(body_bytes(&cameleer_core::json::parse(r#"{"a":[1]}"#).unwrap()), br#"{"a":[1]}"#);
    }
}

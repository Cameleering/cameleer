//! The `cxfrs:` component (Camel `camel-cxf-rest`), a modest JAX-RS style REST endpoint.
//!
//! There are no Java resource classes: `resourceClasses` is accepted (and ignored) and the
//! resource model can instead be described with `operations`:
//!
//! ```text
//! cxfrs:http://0.0.0.0:9000/api?operations=getCustomer:GET:/customers/{id};addCustomer:POST:/customers
//! ```
//!
//! **Consumer** — serves every request under the base address. Headers: `operationName`
//! (the matching operation, else the HTTP method), `CamelHttpMethod`, `CamelHttpPath` (path
//! relative to the base address), `CamelHttpUri`, `CamelHttpQuery`, `Content-Type`, `Accept`,
//! path template parameters and (binding style `SimpleConsumer`, the default) query
//! parameters. With `operations` set, a request matching none gets `404`. The body is the
//! entity (JSON entities are parsed into values). The reply body: objects / arrays → JSON,
//! strings → `text/plain` (or `produces`), bytes as is, null → `204`; `CamelHttpResponseCode`
//! sets the status and `Content-Type` the media type. Exceptions answer `500` with the message.
//!
//! **Producer** — `cxfrs:http://host:port/base`: the request uses `CamelHttpMethod` (default
//! GET, POST with a body), `CamelHttpPath` (appended to the base), `CamelHttpQuery`,
//! `Content-Type` and `Accept`; objects / arrays are sent as JSON. The response body is the
//! entity text (JSON parsed into a value when `parseJson=true`), with `CamelHttpResponseCode`
//! and `Content-Type` headers. Status ≥ 300 fails with `HttpOperationFailedException` unless
//! `throwExceptionOnFailure=false`. Options `username`/`password`, `sslContextParameters`,
//! `receiveTimeout`, `connectionTimeout`.

use std::any::Any;
use std::sync::Arc;

use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{CamelError, Result};
use camel_core::message::{Exchange, ExchangePattern};
use camel_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use camel_core::util::{url_decode, Parameters};
use camel_core::value::Value;
use camel_http::registry::{PathTemplate, RouteHandler};
use camel_http::{HttpRequest, HttpResponse};

use crate::transport::{http_failed, Binding, Client, ListenAddress};

/// The `cxfrs` component.
#[derive(Default)]
pub struct CxfRsComponent;

#[derive(Clone)]
struct Operation {
    name: String,
    method: String,
    template: PathTemplate,
}

struct Config {
    address: String,
    operations: Vec<Operation>,
    simple_consumer: bool,
    produces: Option<String>,
    throw_on_failure: bool,
    parse_json: bool,
    client: Client,
    listen: Option<std::result::Result<ListenAddress, String>>,
}

impl Component for CxfRsComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        if remaining.starts_with("bean:") {
            return Err(CamelError::illegal_argument("cxfrs:bean: endpoints need a Spring bean; use cxfrs:http://host:port/path"));
        }
        let address = params.take("address").unwrap_or_else(|| remaining.to_string());
        params.take("resourceClasses");
        params.take("serviceBeans");
        let mut operations = Vec::new();
        if let Some(spec) = params.take("operations") {
            for item in spec.split(';').map(str::trim).filter(|s| !s.is_empty()) {
                let mut it = item.splitn(3, ':');
                let (Some(name), Some(method), Some(path)) = (it.next(), it.next(), it.next()) else {
                    return Err(CamelError::illegal_argument(format!("cxfrs operations entry '{}' must be name:METHOD:/path", item)));
                };
                operations.push(Operation { name: name.to_string(), method: method.to_ascii_uppercase(), template: PathTemplate::parse(path) });
            }
        }
        let simple_consumer = !params.take("bindingStyle").map(|b| b.eq_ignore_ascii_case("Default")).unwrap_or(false);
        let listen = if address.starts_with("http://") || address.starts_with("https://") || address.starts_with('/') {
            let mut p2 = params.clone();
            Some(ListenAddress::parse(ctx, "cxfrs", &address, &mut p2).map_err(|e| e.message))
        } else {
            None
        };
        let client = Client::from_params(ctx, "cxfrs", params)?;
        let cfg = Config {
            address,
            operations,
            simple_consumer,
            produces: params.take("produces"),
            throw_on_failure: params.take_bool("throwExceptionOnFailure", true)?,
            parse_json: params.take_bool("parseJson", false)?,
            client,
            listen,
        };
        for k in ["httpClientAPI", "loggingFeatureEnabled", "providers", "features", "synchronous", "lazyStartProducer", "bridgeErrorHandler", "exchangePattern", "clientAuth", "useGlobalSslContextParameters", "performInvocation", "propagateContexts", "ignoreDeleteMethodMessageBody", "maxClientCacheSize"] {
            params.take(k);
        }
        Ok(Arc::new(CxfRsEndpoint { uri: uri.to_string(), cfg: Arc::new(cfg), ctx: ctx.downgrade(), binding: Arc::new(Binding::default()) }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A cxfrs endpoint.
pub struct CxfRsEndpoint {
    uri: String,
    cfg: Arc<Config>,
    ctx: WeakContext,
    binding: Arc<Binding>,
}

impl CxfRsEndpoint {
    /// The bound port of a started consumer.
    pub fn local_port(&self) -> Option<u16> {
        self.binding.port()
    }
}

impl Endpoint for CxfRsEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        if !self.cfg.address.starts_with("http://") && !self.cfg.address.starts_with("https://") {
            return Err(CamelError::illegal_argument(format!("cxfrs producer {} needs an http(s) address", self.uri)));
        }
        Ok(Arc::new(CxfRsProducer { cfg: self.cfg.clone() }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        let listen = self.cfg.listen.clone().ok_or_else(|| CamelError::illegal_argument(format!("cxfrs consumer {} needs an http(s)://host:port/path address", self.uri)))?.map_err(CamelError::illegal_argument)?;
        Ok(Arc::new(CxfRsConsumer { cfg: self.cfg.clone(), ctx: self.ctx.clone(), processor, listen, binding: self.binding.clone() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn entity_value(body: &[u8], content_type: Option<&str>) -> Value {
    if body.is_empty() {
        return Value::Null;
    }
    let text = String::from_utf8_lossy(body).into_owned();
    if content_type.map(|c| c.to_ascii_lowercase().contains("json")).unwrap_or(false) {
        if let Ok(v) = camel_core::json::parse(&text) {
            return v;
        }
    }
    if std::str::from_utf8(body).is_ok() {
        Value::String(text)
    } else {
        Value::Bytes(body.to_vec())
    }
}

fn entity_bytes(v: &Value) -> (Option<Vec<u8>>, &'static str) {
    match v {
        Value::Null => (None, "text/plain"),
        Value::Object(_) | Value::Array(_) => (Some(v.to_json().into_bytes()), "application/json"),
        Value::Bytes(b) => (Some(b.clone()), "application/octet-stream"),
        other => (Some(other.to_string().into_bytes()), "text/plain"),
    }
}

// ---------------------------------------------------------------------- consumer

struct CxfRsConsumer {
    cfg: Arc<Config>,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
    listen: ListenAddress,
    binding: Arc<Binding>,
}

fn handle(cfg: &Config, base: &str, ctx: &WeakContext, processor: &Arc<dyn Processor>, req: &mut HttpRequest) -> HttpResponse {
    let Some(ctx) = ctx.upgrade() else { return HttpResponse::text(503, "Service Unavailable") };
    let rel = req.path.strip_prefix(base.trim_end_matches('/')).unwrap_or(&req.path).to_string();
    let rel = if rel.is_empty() { "/".to_string() } else { rel };
    let mut path_params = Vec::new();
    let op_name = if cfg.operations.is_empty() {
        req.method.clone()
    } else {
        let found = cfg.operations.iter().find_map(|o| {
            if o.method != req.method && !(req.method == "HEAD" && o.method == "GET") {
                return None;
            }
            o.template.matches(&rel, false).map(|p| (o.name.clone(), p))
        });
        match found {
            Some((n, p)) => {
                path_params = p;
                n
            }
            None => return HttpResponse::text(404, format!("No resource method matches {} {}", req.method, rel)),
        }
    };
    let mut ex = Exchange::new(Some(ctx));
    ex.pattern = ExchangePattern::InOut;
    {
        let h = &mut ex.message.headers;
        h.set("operationName", op_name);
        h.set("CamelHttpMethod", req.method.clone());
        h.set("CamelHttpPath", rel);
        h.set("CamelHttpUri", req.raw_path.clone());
        if let Some(q) = &req.raw_query {
            h.set("CamelHttpQuery", q.clone());
        }
        for (k, v) in req.headers.iter() {
            if k.eq_ignore_ascii_case("content-length") || k.eq_ignore_ascii_case("host") || k.eq_ignore_ascii_case("connection") {
                continue;
            }
            h.set(k.to_string(), v.to_string());
        }
        for (k, v) in &path_params {
            h.set(k.clone(), url_decode(v));
        }
        if cfg.simple_consumer {
            for (k, v) in req.query_params() {
                h.set(k, v);
            }
        }
    }
    ex.message.body = entity_value(&req.body, req.content_type());
    if let Err(e) = processor.process(&mut ex) {
        ex.set_exception(e);
    }
    if let Some(e) = ex.take_exception() {
        let status = ex.message.headers.get("CamelHttpResponseCode").and_then(|v| v.as_i64()).filter(|s| *s >= 400).unwrap_or(500) as u16;
        return HttpResponse::text(status, e.message);
    }
    let (body, default_ct) = entity_bytes(&ex.message.body);
    let status = ex.message.headers.get("CamelHttpResponseCode").and_then(|v| v.as_i64()).map(|s| s as u16).unwrap_or(if body.is_some() { 200 } else { 204 });
    let ct = ex
        .message
        .headers
        .get("Content-Type")
        .map(|v| v.to_string())
        // a Content-Type left over from the request does not describe the reply
        .filter(|c| !c.is_empty() && req.content_type() != Some(c.as_str()))
        .or_else(|| if default_ct == "text/plain" { cfg.produces.clone() } else { None })
        .unwrap_or_else(|| default_ct.to_string());
    let mut resp = HttpResponse::new(status);
    if let Some(b) = body {
        resp = resp.with_header("Content-Type", &ct).with_body(b);
    }
    resp
}

impl Service for CxfRsConsumer {
    fn start(&self) -> Result<()> {
        let (cfg, ctx, processor) = (self.cfg.clone(), self.ctx.clone(), self.processor.clone());
        let base = self.listen.path.clone();
        let handler: RouteHandler = Arc::new(move |req: &mut HttpRequest, _p: Vec<(String, String)>| handle(&cfg, &base, &ctx, &processor, req));
        self.binding.start(&self.listen, true, handler)
    }
    fn stop(&self) -> Result<()> {
        self.binding.stop();
        Ok(())
    }
}

// ---------------------------------------------------------------------- producer

struct CxfRsProducer {
    cfg: Arc<Config>,
}

impl Service for CxfRsProducer {}

impl Processor for CxfRsProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let h = |n: &str| ex.message.headers.get(n).filter(|v| !v.is_null()).map(|v| v.to_string()).filter(|s| !s.is_empty());
        let (body, default_ct) = entity_bytes(&ex.message.body);
        let method = h("CamelHttpMethod").map(|m| m.to_ascii_uppercase()).unwrap_or_else(|| if body.is_some() { "POST".into() } else { "GET".into() });
        let mut url = self.cfg.address.trim_end_matches('/').to_string();
        if let Some(p) = h("CamelHttpPath") {
            if !p.starts_with('/') {
                url.push('/');
            }
            url.push_str(&p);
        }
        if let Some(q) = h("CamelHttpQuery") {
            url.push(if url.contains('?') { '&' } else { '?' });
            url.push_str(&q);
        }
        let mut headers = Vec::new();
        let send_body = if method == "GET" || method == "HEAD" || method == "DELETE" { None } else { body };
        if send_body.is_some() {
            headers.push(("Content-Type".to_string(), h("Content-Type").unwrap_or_else(|| default_ct.to_string())));
        }
        if let Some(a) = h("Accept") {
            headers.push(("Accept".to_string(), a));
        }
        let resp = self.cfg.client.send(&method, &url, &headers, send_body.as_deref())?;
        if resp.status >= 300 && self.cfg.throw_on_failure {
            return Err(http_failed(&url, &resp));
        }
        ex.message.headers.set("CamelHttpResponseCode", resp.status as i64);
        let ct = resp.content_type().map(str::to_string);
        if let Some(c) = &ct {
            ex.message.headers.set("Content-Type", c.clone());
        }
        ex.message.body = if self.cfg.parse_json {
            entity_value(&resp.body, ct.as_deref())
        } else if resp.body.is_empty() {
            Value::Null
        } else {
            Value::String(resp.body_text())
        };
        Ok(())
    }
}

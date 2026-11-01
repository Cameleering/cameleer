//! The `cxfrs:` component (Camel `camel-cxf-rest`), a modest JAX-RS style REST endpoint.
//!
//! The resource model is described by [`ResourceDescriptor`] beans (the stand-in for JAX-RS
//! resource classes: class-level path, methods with HTTP method, path template, `consumes`
//! and `produces` media types) given as `resourceClasses=#customers[,#orders]`, or inline
//! with `operations`:
//!
//! ```text
//! cxfrs:http://0.0.0.0:9000/api?operations=getCustomer:GET:/customers/{id};addCustomer:POST:/customers
//! cxfrs:http://0.0.0.0:9000/api?resourceClasses=#customers&providers=#csvProvider
//! ```
//!
//! Requests matching a resource method's path but not its `consumes` get `415`, not its
//! `produces` (`Accept`) `406`. Entity providers ([`EntityProvider`] beans, `providers=#a,#b`
//! — JAX-RS `MessageBodyReader`/`Writer`) read request entities and write response
//! entities of their media types (consumer), and write request / read response entities
//! (producer); JSON is built in.
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
    consumes: Vec<String>,
    produces: Vec<String>,
}

/// A resource method of a [`ResourceDescriptor`].
#[derive(Clone, Debug, Default)]
pub struct ResourceMethod {
    pub name: String,
    pub method: String,
    pub path: String,
    pub consumes: Vec<String>,
    pub produces: Vec<String>,
}

impl ResourceMethod {
    pub fn new(name: &str, method: &str, path: &str) -> ResourceMethod {
        ResourceMethod { name: name.into(), method: method.to_ascii_uppercase(), path: path.into(), ..Default::default() }
    }
    pub fn consumes(mut self, media: &str) -> Self {
        self.consumes.push(media.to_string());
        self
    }
    pub fn produces(mut self, media: &str) -> Self {
        self.produces.push(media.to_string());
        self
    }
}

/// A JAX-RS style resource description (`resourceClasses=#bean`).
#[derive(Clone, Debug, Default)]
pub struct ResourceDescriptor {
    /// Class-level path (`@Path` on the class), prefixed to every method path.
    pub path: String,
    pub methods: Vec<ResourceMethod>,
    /// Class-level `@Consumes` / `@Produces` defaults.
    pub consumes: Vec<String>,
    pub produces: Vec<String>,
}

impl ResourceDescriptor {
    pub fn new(path: &str) -> ResourceDescriptor {
        ResourceDescriptor { path: path.to_string(), ..Default::default() }
    }
    pub fn method(mut self, m: ResourceMethod) -> Self {
        self.methods.push(m);
        self
    }
    pub fn consumes(mut self, media: &str) -> Self {
        self.consumes.push(media.to_string());
        self
    }
    pub fn produces(mut self, media: &str) -> Self {
        self.produces.push(media.to_string());
        self
    }
}

type ReadFn = dyn Fn(&str, &[u8]) -> Result<Value> + Send + Sync;
type WriteFn = dyn Fn(&str, &Value) -> Result<Vec<u8>> + Send + Sync;

/// An entity provider (`providers=#bean`): reads and writes entities of its media types.
#[derive(Clone)]
pub struct EntityProvider {
    pub media_types: Vec<String>,
    read: Arc<ReadFn>,
    write: Arc<WriteFn>,
}

impl EntityProvider {
    pub fn new(media_types: &[&str], read: impl Fn(&str, &[u8]) -> Result<Value> + Send + Sync + 'static, write: impl Fn(&str, &Value) -> Result<Vec<u8>> + Send + Sync + 'static) -> EntityProvider {
        EntityProvider { media_types: media_types.iter().map(|s| s.to_string()).collect(), read: Arc::new(read), write: Arc::new(write) }
    }
    fn handles(&self, media: &str) -> bool {
        self.media_types.iter().any(|m| media_matches(m, media))
    }
}

/// `type/subtype` match with `*` wildcards (parameters ignored).
fn media_matches(pattern: &str, actual: &str) -> bool {
    let norm = |s: &str| s.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    let (p, a) = (norm(pattern), norm(actual));
    let (pt, ps) = p.split_once('/').unwrap_or((p.as_str(), "*"));
    let (at, as_) = a.split_once('/').unwrap_or((a.as_str(), "*"));
    (pt == "*" || at == "*" || pt == at) && (ps == "*" || as_ == "*" || ps == as_)
}

fn provider_for<'a>(providers: &'a [EntityProvider], media: Option<&str>) -> Option<&'a EntityProvider> {
    let m = media?;
    providers.iter().find(|p| p.handles(m))
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
    providers: Vec<EntityProvider>,
}

impl Component for CxfRsComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        if remaining.starts_with("bean:") {
            return Err(CamelError::illegal_argument("cxfrs:bean: endpoints need a Spring bean; use cxfrs:http://host:port/path"));
        }
        let address = params.take("address").unwrap_or_else(|| remaining.to_string());
        params.take("serviceBeans");
        let mut operations = Vec::new();
        for r in params.take("resourceClasses").unwrap_or_default().split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let Some(bean) = r.strip_prefix('#') else { continue }; // Java class names: ignored
            let d = ctx.registry().lookup::<ResourceDescriptor>(bean).ok_or_else(|| CamelError::no_such_bean(bean))?;
            for m in &d.methods {
                let path = format!("{}/{}", d.path.trim_end_matches('/'), m.path.trim_start_matches('/'));
                let path = if path.starts_with('/') { path } else { format!("/{}", path) };
                operations.push(Operation {
                    name: m.name.clone(),
                    method: m.method.clone(),
                    template: PathTemplate::parse(path.trim_end_matches('/')),
                    consumes: if m.consumes.is_empty() { d.consumes.clone() } else { m.consumes.clone() },
                    produces: if m.produces.is_empty() { d.produces.clone() } else { m.produces.clone() },
                });
            }
        }
        let mut providers = Vec::new();
        for p in params.take("providers").unwrap_or_default().split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let Some(bean) = p.strip_prefix('#') else { continue };
            let pr = ctx.registry().lookup::<EntityProvider>(bean).ok_or_else(|| CamelError::no_such_bean(bean))?;
            providers.push((*pr).clone());
        }
        if let Some(spec) = params.take("operations") {
            for item in spec.split(';').map(str::trim).filter(|s| !s.is_empty()) {
                let mut it = item.splitn(3, ':');
                let (Some(name), Some(method), Some(path)) = (it.next(), it.next(), it.next()) else {
                    return Err(CamelError::illegal_argument(format!("cxfrs operations entry '{}' must be name:METHOD:/path", item)));
                };
                operations.push(Operation { name: name.to_string(), method: method.to_ascii_uppercase(), template: PathTemplate::parse(path), consumes: Vec::new(), produces: Vec::new() });
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
            providers,
        };
        for k in ["httpClientAPI", "loggingFeatureEnabled", "features", "synchronous", "lazyStartProducer", "bridgeErrorHandler", "exchangePattern", "clientAuth", "useGlobalSslContextParameters", "performInvocation", "propagateContexts", "ignoreDeleteMethodMessageBody", "maxClientCacheSize"] {
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
    let mut chosen_produces: Option<String> = None;
    let op_name = if cfg.operations.is_empty() {
        req.method.clone()
    } else {
        let rel_t = if rel.len() > 1 { rel.trim_end_matches('/') } else { rel.as_str() };
        let cands: Vec<(&Operation, Vec<(String, String)>)> = cfg
            .operations
            .iter()
            .filter(|o| o.method == req.method || (req.method == "HEAD" && o.method == "GET"))
            .filter_map(|o| o.template.matches(rel_t, false).map(|p| (o, p)))
            .collect();
        if cands.is_empty() {
            return HttpResponse::text(404, format!("No resource method matches {} {}", req.method, rel));
        }
        let ct = req.content_type().map(str::to_string);
        let by_media: Vec<_> = cands.into_iter().filter(|(o, _)| o.consumes.is_empty() || req.body.is_empty() || ct.as_deref().is_some_and(|c| o.consumes.iter().any(|m| media_matches(m, c)))).collect();
        if by_media.is_empty() {
            return HttpResponse::text(415, format!("Unsupported Media Type: {}", ct.unwrap_or_default()));
        }
        let accept: Vec<String> = req.headers.get("Accept").map(|a| a.split(',').map(|x| x.trim().to_string()).collect()).unwrap_or_default();
        let pick = by_media.into_iter().find_map(|(o, p)| {
            if o.produces.is_empty() || accept.is_empty() {
                return Some((o, p, o.produces.first().cloned()));
            }
            let m = o.produces.iter().find(|pm| accept.iter().any(|a| media_matches(a, pm)))?;
            Some((o, p, Some(m.clone())))
        });
        match pick {
            Some((o, p, prod)) => {
                path_params = p;
                chosen_produces = prod;
                o.name.clone()
            }
            None => return HttpResponse::text(406, format!("Not Acceptable: {}", accept.join(", "))),
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
    ex.message.body = match provider_for(&cfg.providers, req.content_type()) {
        Some(p) if !req.body.is_empty() => match (p.read)(req.content_type().unwrap_or(""), &req.body) {
            Ok(v) => v,
            Err(e) => return HttpResponse::text(400, format!("Cannot read the entity: {}", e.message)),
        },
        _ => entity_value(&req.body, req.content_type()),
    };
    if let Err(e) = processor.process(&mut ex) {
        ex.set_exception(e);
    }
    if let Some(e) = ex.take_exception() {
        let status = ex.message.headers.get("CamelHttpResponseCode").and_then(|v| v.as_i64()).filter(|s| *s >= 400).unwrap_or(500) as u16;
        return HttpResponse::text(status, e.message);
    }
    let explicit_ct = ex
        .message
        .headers
        .get("Content-Type")
        .map(|v| v.to_string())
        // a Content-Type left over from the request does not describe the reply
        .filter(|c| !c.is_empty() && req.content_type() != Some(c.as_str()));
    let target = explicit_ct.clone().or_else(|| chosen_produces.clone());
    let (body, default_ct) = match provider_for(&cfg.providers, target.as_deref()) {
        Some(p) if !ex.message.body.is_null() => match (p.write)(target.as_deref().unwrap_or(""), &ex.message.body) {
            Ok(b) => (Some(b), "application/octet-stream"),
            Err(e) => return HttpResponse::text(500, format!("Cannot write the entity: {}", e.message)),
        },
        _ => entity_bytes(&ex.message.body),
    };
    let status = ex.message.headers.get("CamelHttpResponseCode").and_then(|v| v.as_i64()).map(|s| s as u16).unwrap_or(if body.is_some() { 200 } else { 204 });
    let ct = explicit_ct
        .or(chosen_produces)
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
        let (body, default_ct) = match provider_for(&self.cfg.providers, h("Content-Type").as_deref()) {
            Some(p) if !ex.message.body.is_null() => (Some((p.write)(&h("Content-Type").unwrap_or_default(), &ex.message.body)?), "application/octet-stream"),
            _ => entity_bytes(&ex.message.body),
        };
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
        ex.message.body = if let Some(p) = provider_for(&self.cfg.providers, ct.as_deref()).filter(|_| !resp.body.is_empty()) {
            (p.read)(ct.as_deref().unwrap_or(""), &resp.body)?
        } else if self.cfg.parse_json {
            entity_value(&resp.body, ct.as_deref())
        } else if resp.body.is_empty() {
            Value::Null
        } else {
            Value::String(resp.body_text())
        };
        Ok(())
    }
}

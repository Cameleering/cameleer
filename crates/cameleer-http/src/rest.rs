//! REST DSL (Camel `rest()` / `restConfiguration()`), the `rest:` component and
//! OpenAPI 3 document generation.
//!
//! ```no_run
//! use cameleer_core::prelude::*;
//! use cameleer_http::rest::{RestDslExt, RestBindingMode, RestParam};
//! let ctx = CamelContext::new();
//! cameleer_http::register(&ctx);
//! ctx.add_routes(|b| {
//!     b.rest_configuration().component("platform-http").port(8080).context_path("/api").binding_mode(RestBindingMode::Json);
//!     b.rest("/users")
//!         .get("/{id}").route_id("getUser").param(RestParam::path("id")).to("direct:getUser")
//!         .post("").consumes("application/json").to("direct:createUser");
//!     b.from("direct:getUser").set_body(simple("user ${header.id}"));
//!     b.from("direct:createUser").set_header("CamelHttpResponseCode", constant(201));
//! }).unwrap();
//! ```
//!
//! Each rest verb becomes a route `from("platform-http:<contextPath><path><verbPath>?httpMethodRestrict=VERB")`
//! (or `jetty:http://host:port/...` etc. for the other HTTP consumer components) that
//! applies the binding and calls the verb's `to` endpoint. An OpenAPI 3 JSON document
//! is served at `<contextPath><apiContextPath>` (default `/api-doc`) unless disabled.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use cameleer_core::context::{CamelContext, WeakContext};
use cameleer_core::dsl::RouteBuilder;
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::model::RouteDefinition;
use cameleer_core::spi::{Component, Consumer, Endpoint, Processor, Producer};
use cameleer_core::util::Parameters;
use cameleer_core::value::{ObjectMap, Value};

use crate::binding::headers;
use crate::http::percent_encode;

/// REST binding mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RestBindingMode {
    /// No binding.
    #[default]
    Off,
    /// Bind JSON when the content type is JSON.
    Auto,
    /// Always bind JSON.
    Json,
}

impl RestBindingMode {
    /// Parse `off`, `auto`, `json` (case-insensitive).
    pub fn parse(s: &str) -> Result<RestBindingMode> {
        match s.to_ascii_lowercase().as_str() {
            "off" => Ok(RestBindingMode::Off),
            "auto" => Ok(RestBindingMode::Auto),
            "json" => Ok(RestBindingMode::Json),
            other => Err(CamelError::illegal_argument(format!("Unsupported rest binding mode: {} (off, auto, json)", other))),
        }
    }
}

/// Global REST configuration (`restConfiguration()`).
#[derive(Clone, Debug)]
pub struct RestConfiguration {
    pub component: String,
    /// `http` (default) or `https` (the consumer then serves TLS; see `platform-http`).
    pub scheme: String,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub context_path: String,
    pub binding_mode: RestBindingMode,
    pub api_docs: bool,
    pub api_context_path: String,
    pub api_properties: BTreeMap<String, String>,
    pub enable_cors: bool,
    pub cors_headers: BTreeMap<String, String>,
    pub endpoint_properties: BTreeMap<String, String>,
    pub skip_binding_on_error_code: bool,
}

impl Default for RestConfiguration {
    fn default() -> Self {
        RestConfiguration {
            component: "platform-http".into(),
            scheme: "http".into(),
            host: None,
            port: None,
            context_path: String::new(),
            binding_mode: RestBindingMode::Off,
            api_docs: true,
            api_context_path: "/api-doc".into(),
            api_properties: BTreeMap::new(),
            enable_cors: false,
            cors_headers: BTreeMap::new(),
            endpoint_properties: BTreeMap::new(),
            skip_binding_on_error_code: true,
        }
    }
}

/// Parameter location.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestParamType {
    Path,
    Query,
    Header,
    Body,
    FormData,
}

/// A documented rest parameter (`param()`).
#[derive(Clone, Debug)]
pub struct RestParam {
    pub name: String,
    pub kind: RestParamType,
    pub required: bool,
    pub description: Option<String>,
    pub data_type: String,
    pub default_value: Option<String>,
    pub allowable_values: Vec<String>,
}

impl RestParam {
    pub fn new(name: &str, kind: RestParamType) -> Self {
        RestParam {
            name: name.to_string(),
            kind,
            required: kind == RestParamType::Path,
            description: None,
            data_type: "string".into(),
            default_value: None,
            allowable_values: Vec::new(),
        }
    }
    pub fn path(name: &str) -> Self {
        Self::new(name, RestParamType::Path)
    }
    pub fn query(name: &str) -> Self {
        Self::new(name, RestParamType::Query)
    }
    pub fn header(name: &str) -> Self {
        Self::new(name, RestParamType::Header)
    }
    pub fn body(name: &str) -> Self {
        Self::new(name, RestParamType::Body)
    }
    pub fn required(mut self, r: bool) -> Self {
        self.required = r;
        self
    }
    pub fn description(mut self, d: &str) -> Self {
        self.description = Some(d.to_string());
        self
    }
    pub fn data_type(mut self, t: &str) -> Self {
        self.data_type = t.to_string();
        self
    }
    pub fn default_value(mut self, v: &str) -> Self {
        self.default_value = Some(v.to_string());
        self
    }
    pub fn allowable_values(mut self, v: &[&str]) -> Self {
        self.allowable_values = v.iter().map(|s| s.to_string()).collect();
        self
    }
}

/// One rest operation (`get`, `post`...).
#[derive(Clone, Debug, Default)]
pub struct VerbDefinition {
    pub method: String,
    pub path: String,
    pub to: Option<String>,
    pub route_id: Option<String>,
    pub description: Option<String>,
    pub consumes: Option<String>,
    pub produces: Option<String>,
    pub binding_mode: Option<RestBindingMode>,
    pub params: Vec<RestParam>,
    pub responses: Vec<(u16, String)>,
    pub type_name: Option<String>,
    pub out_type: Option<String>,
    pub api_docs: bool,
    pub deprecated: bool,
}

/// A `rest("/path")` block.
#[derive(Clone, Debug, Default)]
pub struct RestDefinition {
    pub path: String,
    pub description: Option<String>,
    pub consumes: Option<String>,
    pub produces: Option<String>,
    pub binding_mode: Option<RestBindingMode>,
    pub tag: Option<String>,
    pub verbs: Vec<VerbDefinition>,
}

/// All REST DSL definitions of a route builder.
#[derive(Clone, Debug, Default)]
pub struct RestDsl {
    pub configuration: RestConfiguration,
    pub rests: Vec<RestDefinition>,
}

#[derive(Default)]
struct RestState {
    shared: Arc<Mutex<RestDsl>>,
    registered: bool,
}

const EXTENSION_KEY: &str = "cameleer-http.rest";

/// Adds `rest_configuration()` and `rest()` to [`RouteBuilder`].
pub trait RestDslExt {
    /// Configure the REST DSL (`restConfiguration()`).
    fn rest_configuration(&mut self) -> RestConfigurationBuilder;
    /// Start a rest block for `path` (`rest("/users")`).
    fn rest(&mut self, path: &str) -> RestBuilder;
    /// Shared REST definitions of this builder (for loaders and tooling).
    fn rest_dsl(&mut self) -> Arc<Mutex<RestDsl>>;
}

impl RestDslExt for RouteBuilder {
    fn rest_dsl(&mut self) -> Arc<Mutex<RestDsl>> {
        let state = self.extension_mut::<RestState>(EXTENSION_KEY);
        let shared = state.shared.clone();
        if !state.registered {
            state.registered = true;
            let s2 = shared.clone();
            self.generators.push(Box::new(move |ctx: &CamelContext| {
                let dsl = s2.lock().unwrap().clone();
                generate_routes(ctx, &dsl)
            }));
        }
        shared
    }

    fn rest_configuration(&mut self) -> RestConfigurationBuilder {
        RestConfigurationBuilder { shared: self.rest_dsl() }
    }

    fn rest(&mut self, path: &str) -> RestBuilder {
        let shared = self.rest_dsl();
        let index = {
            let mut d = shared.lock().unwrap();
            d.rests.push(RestDefinition { path: path.to_string(), ..Default::default() });
            d.rests.len() - 1
        };
        RestBuilder { shared, index }
    }
}

/// Fluent builder for [`RestConfiguration`].
pub struct RestConfigurationBuilder {
    shared: Arc<Mutex<RestDsl>>,
}

impl RestConfigurationBuilder {
    fn with(&mut self, f: impl FnOnce(&mut RestConfiguration)) -> &mut Self {
        f(&mut self.shared.lock().unwrap().configuration);
        self
    }
    /// HTTP consumer component (`platform-http` (default), `jetty`, `undertow`, `netty-http`).
    pub fn component(&mut self, c: &str) -> &mut Self {
        let c = c.to_string();
        self.with(|cfg| cfg.component = c)
    }
    /// `http` (default) or `https`: with `https` the consumers serve TLS using the
    /// embedded server certificate (`camel.server.ssl.*`, the global `camel.ssl.*`
    /// parameters, or an `sslContextParameters` endpoint property).
    pub fn scheme(&mut self, scheme: &str) -> &mut Self {
        let sc = scheme.to_ascii_lowercase();
        self.with(|cfg| cfg.scheme = sc)
    }
    pub fn host(&mut self, h: &str) -> &mut Self {
        let h = h.to_string();
        self.with(|cfg| cfg.host = Some(h))
    }
    /// Port (0 = pick a free port; shared by all rest routes with `platform-http`).
    pub fn port(&mut self, p: u16) -> &mut Self {
        self.with(|cfg| cfg.port = Some(p))
    }
    pub fn context_path(&mut self, p: &str) -> &mut Self {
        let p = p.to_string();
        self.with(|cfg| cfg.context_path = p)
    }
    pub fn binding_mode(&mut self, m: RestBindingMode) -> &mut Self {
        self.with(|cfg| cfg.binding_mode = m)
    }
    /// Enable/disable the OpenAPI document (enabled by default).
    pub fn api_docs(&mut self, enabled: bool) -> &mut Self {
        self.with(|cfg| cfg.api_docs = enabled)
    }
    /// Path of the OpenAPI document below the context path (default `/api-doc`).
    pub fn api_context_path(&mut self, p: &str) -> &mut Self {
        let p = p.to_string();
        self.with(|cfg| {
            cfg.api_context_path = p;
            cfg.api_docs = true;
        })
    }
    /// OpenAPI info properties: `api.title`, `api.version`, `api.description`.
    pub fn api_property(&mut self, k: &str, v: &str) -> &mut Self {
        let (k, v) = (k.to_string(), v.to_string());
        self.with(|cfg| {
            cfg.api_properties.insert(k, v);
        })
    }
    pub fn enable_cors(&mut self, e: bool) -> &mut Self {
        self.with(|cfg| cfg.enable_cors = e)
    }
    pub fn cors_header(&mut self, k: &str, v: &str) -> &mut Self {
        let (k, v) = (k.to_string(), v.to_string());
        self.with(|cfg| {
            cfg.cors_headers.insert(k, v);
        })
    }
    /// Extra option added to every consumer endpoint uri.
    pub fn endpoint_property(&mut self, k: &str, v: &str) -> &mut Self {
        let (k, v) = (k.to_string(), v.to_string());
        self.with(|cfg| {
            cfg.endpoint_properties.insert(k, v);
        })
    }
    pub fn skip_binding_on_error_code(&mut self, s: bool) -> &mut Self {
        self.with(|cfg| cfg.skip_binding_on_error_code = s)
    }
}

/// Fluent builder for a [`RestDefinition`]. Settings such as `description`,
/// `consumes`, `produces` and `binding_mode` apply to the last verb, or to the rest
/// block if no verb was added yet.
pub struct RestBuilder {
    shared: Arc<Mutex<RestDsl>>,
    index: usize,
}

impl RestBuilder {
    fn rest_mut(&mut self, f: impl FnOnce(&mut RestDefinition)) -> &mut Self {
        f(&mut self.shared.lock().unwrap().rests[self.index]);
        self
    }
    fn verb_or_rest(&mut self, on_verb: impl FnOnce(&mut VerbDefinition), on_rest: impl FnOnce(&mut RestDefinition)) -> &mut Self {
        self.rest_mut(|r| match r.verbs.last_mut() {
            Some(v) => on_verb(v),
            None => on_rest(r),
        })
    }
    fn last_verb(&mut self, f: impl FnOnce(&mut VerbDefinition)) -> &mut Self {
        self.rest_mut(|r| {
            if let Some(v) = r.verbs.last_mut() {
                f(v)
            }
        })
    }

    /// Add an operation for `method` on `path` (relative to the rest path; `""` = the rest path).
    pub fn verb(&mut self, method: &str, path: &str) -> &mut Self {
        let v = VerbDefinition { method: method.to_ascii_uppercase(), path: path.to_string(), api_docs: true, ..Default::default() };
        self.rest_mut(|r| r.verbs.push(v))
    }
    pub fn get(&mut self, path: &str) -> &mut Self {
        self.verb("GET", path)
    }
    pub fn post(&mut self, path: &str) -> &mut Self {
        self.verb("POST", path)
    }
    pub fn put(&mut self, path: &str) -> &mut Self {
        self.verb("PUT", path)
    }
    pub fn delete(&mut self, path: &str) -> &mut Self {
        self.verb("DELETE", path)
    }
    pub fn patch(&mut self, path: &str) -> &mut Self {
        self.verb("PATCH", path)
    }
    pub fn head(&mut self, path: &str) -> &mut Self {
        self.verb("HEAD", path)
    }
    pub fn options(&mut self, path: &str) -> &mut Self {
        self.verb("OPTIONS", path)
    }
    /// Endpoint the last verb routes to.
    pub fn to(&mut self, uri: &str) -> &mut Self {
        let uri = uri.to_string();
        self.last_verb(|v| v.to = Some(uri))
    }
    /// Route id of the generated route for the last verb.
    pub fn route_id(&mut self, id: &str) -> &mut Self {
        let id = id.to_string();
        self.last_verb(|v| v.route_id = Some(id))
    }
    pub fn description(&mut self, d: &str) -> &mut Self {
        let (a, b) = (d.to_string(), d.to_string());
        self.verb_or_rest(|v| v.description = Some(a), |r| r.description = Some(b))
    }
    pub fn consumes(&mut self, c: &str) -> &mut Self {
        let (a, b) = (c.to_string(), c.to_string());
        self.verb_or_rest(|v| v.consumes = Some(a), |r| r.consumes = Some(b))
    }
    pub fn produces(&mut self, p: &str) -> &mut Self {
        let (a, b) = (p.to_string(), p.to_string());
        self.verb_or_rest(|v| v.produces = Some(a), |r| r.produces = Some(b))
    }
    pub fn binding_mode(&mut self, m: RestBindingMode) -> &mut Self {
        self.verb_or_rest(|v| v.binding_mode = Some(m), |r| r.binding_mode = Some(m))
    }
    /// OpenAPI tag of the rest block.
    pub fn tag(&mut self, t: &str) -> &mut Self {
        let t = t.to_string();
        self.rest_mut(|r| r.tag = Some(t))
    }
    /// Document a parameter of the last verb.
    pub fn param(&mut self, p: RestParam) -> &mut Self {
        self.last_verb(|v| v.params.push(p))
    }
    /// Document a response of the last verb.
    pub fn response_message(&mut self, code: u16, message: &str) -> &mut Self {
        let m = message.to_string();
        self.last_verb(|v| v.responses.push((code, m)))
    }
    /// Input type name (documentation only).
    pub fn type_(&mut self, t: &str) -> &mut Self {
        let t = t.to_string();
        self.last_verb(|v| v.type_name = Some(t))
    }
    /// Output type name (documentation only).
    pub fn out_type(&mut self, t: &str) -> &mut Self {
        let t = t.to_string();
        self.last_verb(|v| v.out_type = Some(t))
    }
    /// Include the last verb in the API document (default true).
    pub fn api_docs(&mut self, enabled: bool) -> &mut Self {
        self.last_verb(|v| v.api_docs = enabled)
    }
    pub fn deprecated(&mut self) -> &mut Self {
        self.last_verb(|v| v.deprecated = true)
    }
}

// ---------------------------------------------------------------------------
// route generation
// ---------------------------------------------------------------------------

/// Join path pieces with single slashes; result starts with `/` and has no trailing slash.
pub fn join_paths(parts: &[&str]) -> String {
    let segs: Vec<&str> = parts.iter().flat_map(|p| p.split('/')).filter(|s| !s.is_empty()).collect();
    format!("/{}", segs.join("/"))
}

fn consumer_uri(cfg: &RestConfiguration, path: &str, method: Option<&str>, consumes: Option<&str>, produces: Option<&str>) -> String {
    let mut q: Vec<(String, String)> = Vec::new();
    if let Some(m) = method {
        q.push(("httpMethodRestrict".into(), m.to_string()));
    }
    if let Some(c) = consumes {
        q.push(("consumes".into(), c.to_string()));
    }
    if let Some(p) = produces {
        q.push(("produces".into(), p.to_string()));
    }
    for (k, v) in &cfg.endpoint_properties {
        q.push((k.clone(), v.clone()));
    }
    let base = if cfg.component == "platform-http" {
        if let Some(p) = cfg.port {
            q.push(("port".into(), p.to_string()));
        }
        if let Some(h) = &cfg.host {
            q.push(("host".into(), h.clone()));
        }
        if cfg.scheme == "https" && !cfg.endpoint_properties.contains_key("ssl") {
            q.push(("ssl".into(), "true".into()));
        }
        format!("platform-http:{}", path)
    } else if matches!(cfg.component.as_str(), "jetty" | "undertow" | "netty-http") {
        let scheme = if cfg.scheme == "https" { "https" } else { "http" };
        format!("{}:{}://{}:{}{}", cfg.component, scheme, cfg.host.as_deref().unwrap_or("0.0.0.0"), cfg.port.unwrap_or(crate::platform::DEFAULT_PORT), path)
    } else {
        format!("{}:{}", cfg.component, path)
    };
    if q.is_empty() {
        base
    } else {
        format!("{}?{}", base, q.iter().map(|(k, v)| format!("{}={}", k, percent_encode(v))).collect::<Vec<_>>().join("&"))
    }
}

fn is_json_content_type(ex: &Exchange) -> bool {
    ex.header(headers::CONTENT_TYPE).map(|v| v.to_string().to_ascii_lowercase().contains("json")).unwrap_or(false)
}

/// Incoming binding: JSON body → structured `Value`.
fn binding_in(mode: RestBindingMode) -> impl Fn(&mut Exchange) -> Result<()> + Send + Sync {
    move |ex: &mut Exchange| {
        if mode == RestBindingMode::Off || (mode == RestBindingMode::Auto && !is_json_content_type(ex)) {
            return Ok(());
        }
        let text = match ex.body() {
            Value::String(s) => s.clone(),
            Value::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
            _ => return Ok(()),
        };
        if text.trim().is_empty() {
            ex.set_body(Value::Null);
            return Ok(());
        }
        match cameleer_core::json::parse(&text) {
            Ok(v) => ex.set_body(v),
            Err(e) => {
                ex.set_header(headers::HTTP_RESPONSE_CODE, 400);
                ex.set_header(headers::CONTENT_TYPE, "text/plain; charset=UTF-8");
                ex.set_body(format!("Invalid JSON in request body: {}", e.message));
                ex.set_route_stop(true);
            }
        }
        Ok(())
    }
}

/// Outgoing binding: structured `Value` → JSON text.
fn binding_out(mode: RestBindingMode, skip_on_error: bool, cors: Option<BTreeMap<String, String>>) -> impl Fn(&mut Exchange) -> Result<()> + Send + Sync {
    move |ex: &mut Exchange| {
        if let Some(c) = &cors {
            for (k, v) in c {
                ex.set_header(k.clone(), v.clone());
            }
        }
        if mode == RestBindingMode::Off {
            return Ok(());
        }
        let code = ex.header(headers::HTTP_RESPONSE_CODE).and_then(|v| v.as_i64()).unwrap_or(200);
        if skip_on_error && code >= 300 {
            return Ok(());
        }
        let structured = matches!(ex.body(), Value::Object(_) | Value::Array(_))
            || (mode == RestBindingMode::Json && matches!(ex.body(), Value::Int(_) | Value::Float(_) | Value::Bool(_)));
        if structured {
            let json = ex.body().to_json();
            ex.set_body(json);
            ex.set_header(headers::CONTENT_TYPE, "application/json");
        } else if mode == RestBindingMode::Json && ex.body().as_str().is_some() && !is_json_content_type(ex) {
            ex.set_header(headers::CONTENT_TYPE, "application/json");
        }
        Ok(())
    }
}

fn cors_headers(cfg: &RestConfiguration) -> BTreeMap<String, String> {
    let mut h = BTreeMap::new();
    h.insert("Access-Control-Allow-Origin".to_string(), "*".to_string());
    h.insert("Access-Control-Allow-Methods".to_string(), "GET, HEAD, POST, PUT, DELETE, TRACE, OPTIONS, CONNECT, PATCH".to_string());
    h.insert(
        "Access-Control-Allow-Headers".to_string(),
        "Origin, Accept, X-Requested-With, Content-Type, Access-Control-Request-Method, Access-Control-Request-Headers".to_string(),
    );
    h.insert("Access-Control-Max-Age".to_string(), "3600".to_string());
    for (k, v) in &cfg.cors_headers {
        h.insert(k.clone(), v.clone());
    }
    h
}

/// Generate the routes backing a REST DSL.
pub fn generate_routes(_ctx: &CamelContext, dsl: &RestDsl) -> Result<Vec<RouteDefinition>> {
    let cfg = &dsl.configuration;
    let mut routes = Vec::new();
    let cors = if cfg.enable_cors { Some(cors_headers(cfg)) } else { None };
    let mut options_paths: Vec<String> = Vec::new();
    for rest in &dsl.rests {
        for verb in &rest.verbs {
            let path = join_paths(&[&cfg.context_path, &rest.path, &verb.path]);
            let to = verb.to.clone().ok_or_else(|| {
                CamelError::new(cameleer_core::error::kinds::FAILED_TO_CREATE_ROUTE, format!("Rest {} {} has no to() endpoint", verb.method, path))
            })?;
            let consumes = verb.consumes.clone().or_else(|| rest.consumes.clone());
            let produces = verb.produces.clone().or_else(|| rest.produces.clone());
            let mode = verb.binding_mode.or(rest.binding_mode).unwrap_or(cfg.binding_mode);
            let uri = consumer_uri(cfg, &path, Some(&verb.method), consumes.as_deref(), produces.as_deref());
            let mut def = RouteDefinition::new(uri);
            if let Some(id) = &verb.route_id {
                def.route_id(id.clone());
            }
            if let Some(d) = verb.description.clone().or_else(|| rest.description.clone()) {
                def.route_description(d);
            }
            if mode != RestBindingMode::Off {
                def.process(binding_in(mode));
            }
            def.to(to);
            if mode != RestBindingMode::Off || cors.is_some() {
                def.process(binding_out(mode, cfg.skip_binding_on_error_code, cors.clone()));
            }
            routes.push(def);
            if cors.is_some() && !options_paths.contains(&path) && verb.method != "OPTIONS" {
                options_paths.push(path);
            }
        }
    }
    if let Some(c) = &cors {
        for path in options_paths {
            let mut def = RouteDefinition::new(consumer_uri(cfg, &path, Some("OPTIONS"), None, None));
            let headers = c.clone();
            def.process(move |ex: &mut Exchange| {
                for (k, v) in &headers {
                    ex.set_header(k.clone(), v.clone());
                }
                ex.set_header(crate::binding::headers::HTTP_RESPONSE_CODE, 200);
                ex.set_body(Value::Null);
                Ok(())
            });
            routes.push(def);
        }
    }
    if cfg.api_docs && !dsl.rests.is_empty() {
        let doc = openapi_document(dsl).to_json_pretty();
        let path = join_paths(&[&cfg.context_path, &cfg.api_context_path]);
        let mut def = RouteDefinition::new(consumer_uri(cfg, &path, Some("GET"), None, None));
        def.route_id(format!("rest-api-doc{}", path.replace('/', "-")));
        let cors = cors.clone();
        def.process(move |ex: &mut Exchange| {
            ex.set_body(doc.clone());
            ex.set_header(headers::CONTENT_TYPE, "application/json");
            if let Some(c) = &cors {
                for (k, v) in c {
                    ex.set_header(k.clone(), v.clone());
                }
            }
            Ok(())
        });
        routes.push(def);
    }
    Ok(routes)
}

fn obj(items: Vec<(&str, Value)>) -> Value {
    Value::Object(items.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn schema_for(data_type: &str) -> Value {
    let t = match data_type.to_ascii_lowercase().as_str() {
        "int" | "integer" | "long" => "integer",
        "number" | "double" | "float" => "number",
        "boolean" | "bool" => "boolean",
        "array" => "array",
        "object" => "object",
        _ => "string",
    };
    obj(vec![("type", Value::from(t))])
}

/// Build an OpenAPI 3.0 document from REST definitions.
pub fn openapi_document(dsl: &RestDsl) -> Value {
    let cfg = &dsl.configuration;
    let prop = |k: &str, d: &str| cfg.api_properties.get(k).cloned().unwrap_or_else(|| d.to_string());
    let mut info = vec![("title", Value::from(prop("api.title", "Cameleer REST API"))), ("version", Value::from(prop("api.version", "1.0.0")))];
    if let Some(d) = cfg.api_properties.get("api.description") {
        info.push(("description", Value::from(d.clone())));
    }
    let mut paths: ObjectMap = ObjectMap::new();
    let mut tags: Vec<String> = Vec::new();
    for rest in &dsl.rests {
        for verb in rest.verbs.iter().filter(|v| v.api_docs) {
            let path = join_paths(&[&rest.path, &verb.path]);
            let mut op: ObjectMap = ObjectMap::new();
            if let Some(id) = &verb.route_id {
                op.insert("operationId".into(), Value::from(id.clone()));
            }
            if let Some(d) = verb.description.clone() {
                op.insert("summary".into(), Value::from(d));
            }
            let tag = rest.tag.clone().unwrap_or_else(|| join_paths(&[&rest.path]).trim_start_matches('/').to_string());
            if !tag.is_empty() {
                if !tags.contains(&tag) {
                    tags.push(tag.clone());
                }
                op.insert("tags".into(), Value::Array(vec![Value::from(tag)]));
            }
            if verb.deprecated {
                op.insert("deprecated".into(), Value::Bool(true));
            }
            // parameters: declared + undeclared template params
            let mut params: Vec<Value> = Vec::new();
            let mut declared: Vec<String> = Vec::new();
            let mut body_param: Option<&RestParam> = None;
            for p in &verb.params {
                let location = match p.kind {
                    RestParamType::Path => "path",
                    RestParamType::Query => "query",
                    RestParamType::Header => "header",
                    RestParamType::Body | RestParamType::FormData => {
                        body_param = Some(p);
                        continue;
                    }
                };
                declared.push(p.name.clone());
                let mut schema = schema_for(&p.data_type);
                if let (Some(d), Some(o)) = (&p.default_value, schema.as_object_mut()) {
                    o.insert("default".into(), Value::from(d.clone()));
                }
                if let (false, Some(o)) = (p.allowable_values.is_empty(), schema.as_object_mut()) {
                    o.insert("enum".into(), Value::Array(p.allowable_values.iter().map(|s| Value::from(s.clone())).collect()));
                }
                let mut po = vec![
                    ("name", Value::from(p.name.clone())),
                    ("in", Value::from(location)),
                    ("required", Value::Bool(p.required || p.kind == RestParamType::Path)),
                    ("schema", schema),
                ];
                if let Some(d) = &p.description {
                    po.push(("description", Value::from(d.clone())));
                }
                params.push(obj(po));
            }
            for name in crate::registry::PathTemplate::parse(&path).param_names() {
                if !declared.contains(&name) {
                    params.push(obj(vec![
                        ("name", Value::from(name)),
                        ("in", Value::from("path")),
                        ("required", Value::Bool(true)),
                        ("schema", schema_for("string")),
                    ]));
                }
            }
            if !params.is_empty() {
                op.insert("parameters".into(), Value::Array(params));
            }
            let consumes = verb.consumes.clone().or_else(|| rest.consumes.clone());
            if consumes.is_some() || body_param.is_some() || verb.type_name.is_some() {
                let ct = consumes.unwrap_or_else(|| "application/json".into());
                let mut content = ObjectMap::new();
                for c in ct.split(',') {
                    content.insert(c.trim().to_string(), obj(vec![("schema", obj(vec![("type", Value::from("object"))]))]));
                }
                let mut rb = vec![("content", Value::Object(content))];
                if let Some(bp) = body_param {
                    rb.push(("required", Value::Bool(bp.required)));
                    if let Some(d) = &bp.description {
                        rb.push(("description", Value::from(d.clone())));
                    }
                }
                op.insert("requestBody".into(), obj(rb));
            }
            let produces = verb.produces.clone().or_else(|| rest.produces.clone());
            let mut responses: ObjectMap = ObjectMap::new();
            let resp_list = if verb.responses.is_empty() { vec![(200u16, "Success".to_string())] } else { verb.responses.clone() };
            for (code, msg) in resp_list {
                let mut r = vec![("description", Value::from(msg))];
                if let (Some(p), true) = (&produces, (200..300).contains(&code)) {
                    let mut content = ObjectMap::new();
                    for c in p.split(',') {
                        content.insert(c.trim().to_string(), obj(vec![("schema", obj(vec![("type", Value::from("object"))]))]));
                    }
                    r.push(("content", Value::Object(content)));
                }
                responses.insert(code.to_string(), obj(r));
            }
            op.insert("responses".into(), Value::Object(responses));
            let entry = paths.entry(path).or_insert_with(|| Value::Object(ObjectMap::new()));
            if let Some(o) = entry.as_object_mut() {
                o.insert(verb.method.to_ascii_lowercase(), Value::Object(op));
            }
        }
    }
    let server_url = if cfg.context_path.is_empty() { "/".to_string() } else { join_paths(&[&cfg.context_path]) };
    let mut doc = vec![
        ("openapi", Value::from("3.0.3")),
        ("info", obj(info)),
        ("servers", Value::Array(vec![obj(vec![("url", Value::from(server_url))])])),
        ("paths", Value::Object(paths)),
    ];
    if !tags.is_empty() {
        doc.push(("tags", Value::Array(tags.into_iter().map(|t| obj(vec![("name", Value::from(t))])).collect())));
    }
    obj(doc)
}

// ---------------------------------------------------------------------------
// rest: component
// ---------------------------------------------------------------------------

/// The `rest:` component: `rest:get:/users/{id}` consumers delegate to the HTTP
/// consumer component (option `componentName`, property `camel.rest.component`, or
/// `platform-http`); producers (`rest:get:/users/{id}?host=localhost:8080`) delegate to
/// the `http` component, filling `{param}` placeholders from headers.
#[derive(Default)]
pub struct RestComponent;

/// A `rest:` endpoint.
pub struct RestEndpoint {
    uri: String,
    method: String,
    path: String,
    consumer_uri: String,
    host: Option<String>,
    producer_component: String,
    ctx: WeakContext,
}

impl Component for RestComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let (method, rest) = remaining
            .split_once(':')
            .ok_or_else(|| CamelError::illegal_argument(format!("Invalid rest uri (expected rest:method:path): {}", uri)))?;
        let path = match rest.split_once(':') {
            Some((p, template)) => join_paths(&[p, template]),
            None => join_paths(&[rest]),
        };
        let method = method.to_ascii_uppercase();
        let component = params
            .take("componentName")
            .or_else(|| ctx.properties().get("camel.rest.component"))
            .unwrap_or_else(|| "platform-http".into());
        let mut cfg = RestConfiguration { component, ..Default::default() };
        if let Some(p) = params.take("port") {
            cfg.port = Some(p.parse().map_err(|_| CamelError::illegal_argument(format!("Invalid port: {}", p)))?);
        }
        let consumes = params.take("consumes");
        let produces = params.take("produces");
        let host = params.take("host");
        let producer_component = params.take("producerComponentName").unwrap_or_else(|| "http".into());
        params.take("inType");
        params.take("outType");
        params.take("routeId");
        params.take("description");
        params.take("apiDoc");
        params.take("bindingMode");
        let consumer_uri = consumer_uri(&cfg, &path, Some(&method), consumes.as_deref(), produces.as_deref());
        Ok(Arc::new(RestEndpoint { uri: uri.to_string(), method, path, consumer_uri, host, producer_component, ctx: ctx.downgrade() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Endpoint for RestEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        let ctx = self.ctx.upgrade().ok_or_else(|| CamelError::illegal_state("CamelContext has been dropped"))?;
        ctx.get_endpoint(&self.consumer_uri)?.create_consumer(processor)
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        let host = self.host.clone().ok_or_else(|| CamelError::illegal_argument(format!("Rest producer {} requires the host option", self.uri)))?;
        Ok(Arc::new(RestProducer {
            host,
            method: self.method.clone(),
            path: self.path.clone(),
            component: self.producer_component.clone(),
            ctx: self.ctx.clone(),
        }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct RestProducer {
    host: String,
    method: String,
    path: String,
    component: String,
    ctx: WeakContext,
}

impl cameleer_core::spi::Service for RestProducer {}

impl Processor for RestProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let ctx = self.ctx.upgrade().ok_or_else(|| CamelError::illegal_state("CamelContext has been dropped"))?;
        let template = crate::registry::PathTemplate::parse(&self.path);
        let mut path = self.path.clone();
        for name in template.param_names() {
            let v = ex.header(&name).map(|v| v.to_string()).unwrap_or_default();
            path = path.replace(&format!("{{{}}}", name), &percent_encode(&v));
        }
        let (component, host) = match self.host.strip_prefix("https://") {
            Some(h) if self.component == "http" => ("https", h),
            Some(h) => (self.component.as_str(), h),
            None => (self.component.as_str(), self.host.trim_start_matches("http://")),
        };
        let host = host.trim_end_matches('/');
        let uri = format!("{}://{}{}?httpMethod={}", component, host, path, self.method);
        let producer = ctx.get_producer(&uri)?;
        ex.message.headers.remove(headers::HTTP_METHOD);
        ex.message.headers.remove(headers::HTTP_PATH);
        ex.message.headers.remove(headers::HTTP_URI);
        producer.process(ex)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_uris() {
        assert_eq!(join_paths(&["/api/", "users", "/{id}"]), "/api/users/{id}");
        assert_eq!(join_paths(&["", "/", ""]), "/");
        let cfg = RestConfiguration { port: Some(9000), ..Default::default() };
        assert_eq!(
            consumer_uri(&cfg, "/a/{id}", Some("GET"), Some("application/json"), None),
            "platform-http:/a/{id}?httpMethodRestrict=GET&consumes=application%2Fjson&port=9000"
        );
        let cfg = RestConfiguration { component: "jetty".into(), port: Some(9001), ..Default::default() };
        assert_eq!(consumer_uri(&cfg, "/x", None, None, None), "jetty:http://0.0.0.0:9001/x");
    }

    #[test]
    fn openapi() {
        let mut dsl = RestDsl::default();
        dsl.rests.push(RestDefinition {
            path: "/users".into(),
            verbs: vec![VerbDefinition {
                method: "GET".into(),
                path: "/{id}".into(),
                route_id: Some("getUser".into()),
                produces: Some("application/json".into()),
                params: vec![RestParam::query("verbose").data_type("boolean")],
                api_docs: true,
                ..Default::default()
            }],
            ..Default::default()
        });
        let doc = openapi_document(&dsl);
        assert_eq!(doc.get("openapi"), Some(&Value::from("3.0.3")));
        let op = doc.get("paths").unwrap().get("/users/{id}").unwrap().get("get").unwrap();
        assert_eq!(op.get("operationId"), Some(&Value::from("getUser")));
        assert_eq!(op.get("parameters").unwrap().as_array().unwrap().len(), 2);
    }
}

//! REST DSL support for the YAML (and XML) DSL: the `rest` and `restConfiguration`
//! top-level elements, implemented with [`cameleer_http::rest::RestDslExt`].
//!
//! ```yaml
//! - restConfiguration:
//!     component: platform-http
//!     port: 8080
//!     contextPath: /api
//!     bindingMode: json
//! - rest:
//!     path: /users
//!     get:
//!       - path: /{id}
//!         id: getUser
//!         produces: application/json
//!         param:
//!           - name: id
//!             type: path
//!             dataType: integer
//!         to: direct:getUser
//!     post:
//!       - to: direct:createUser
//! ```
//!
//! `restConfiguration` may come before or after the `rest` elements, in the same file or in
//! any other file loaded before the context starts: like in Camel, the REST configuration is
//! global. `rest` elements loaded before the context starts are validated at load time and
//! turned into routes when the context starts, all together (one OpenAPI document), with
//! every `restConfiguration` loaded by then. `rest` elements loaded into a running context
//! are turned into routes at once with the configurations loaded so far.

use std::sync::{Arc, Mutex, Once};

use cameleer_core::context::WeakContext;
use cameleer_core::dsl::RouteBuilder;
use cameleer_core::prelude::*;
use cameleer_http::rest::{RestBindingMode, RestConfiguration, RestDefinition, RestDslExt, RestParam, RestParamType, VerbDefinition};
use cameleer_yaml::{endpoint_uri, yaml_state, TopLevelContext};

/// HTTP verbs of a `rest` element.
const VERBS: &[&str] = &["get", "post", "put", "delete", "patch", "head", "options"];

/// Registry flag set once an OpenAPI route has been generated for a context.
const API_DOC_FLAG: &str = "cameleer-main.yaml-rest.apiDoc";

struct ApiDocGenerated;

/// Registry name of the [`PendingRests`] service of a context.
const PENDING_FLAG: &str = "cameleer-main.yaml-rest.pending";

/// `rest` elements loaded before the context started; turned into routes when the context
/// starts (services start before the pending routes are created).
struct PendingRests {
    ctx: WeakContext,
    rests: Mutex<Vec<Value>>,
}

impl Service for PendingRests {
    fn start(&self) -> Result<()> {
        let rests: Vec<Value> = std::mem::take(&mut *self.rests.lock().unwrap());
        if rests.is_empty() {
            return Ok(());
        }
        let Some(ctx) = self.ctx.upgrade() else { return Ok(()) };
        let refs: Vec<&Value> = rests.iter().collect();
        let mut routes = build_rest_routes(&ctx, &refs, &[])?;
        cameleer_yaml::YamlLoader::new().apply_route_configurations_to(&ctx, &mut routes)?;
        for r in routes {
            ctx.add_route_definition(r)?;
        }
        Ok(())
    }
}

fn pending_rests(ctx: &CamelContext) -> Arc<PendingRests> {
    static CREATE: Mutex<()> = Mutex::new(());
    let _g = CREATE.lock().unwrap();
    if let Some(p) = ctx.registry().lookup::<PendingRests>(PENDING_FLAG) {
        return p;
    }
    let p = Arc::new(PendingRests { ctx: ctx.downgrade(), rests: Mutex::new(Vec::new()) });
    ctx.registry().bind_arc(PENDING_FLAG, p.clone() as Arc<dyn std::any::Any + Send + Sync>);
    let _ = ctx.add_service(p.clone());
    p
}

/// Build the routes of `rests` with every `restConfiguration` loaded into the context, plus
/// `extra_configs` (the ones of the document being loaded).
fn build_rest_routes(ctx: &CamelContext, rests: &[&Value], extra_configs: &[Value]) -> Result<Vec<cameleer_core::model::RouteDefinition>> {
    let mut b = RouteBuilder::new(ctx.clone());
    let shared = b.rest_dsl();
    {
        let mut dsl = shared.lock().unwrap();
        let mut configs: Vec<Value> = yaml_state(ctx).rest_configurations();
        configs.extend(extra_configs.iter().cloned());
        for c in &configs {
            apply_rest_configuration(ctx, &mut dsl.configuration, c)?;
        }
        if ctx.registry().contains(API_DOC_FLAG) {
            dsl.configuration.api_docs = false;
        }
        for r in rests {
            dsl.rests.push(parse_rest(ctx, r)?);
        }
        if dsl.configuration.api_docs {
            ctx.registry().bind_arc(API_DOC_FLAG, Arc::new(ApiDocGenerated));
        }
    }
    b.into_definitions()
}

/// Register the `rest` top-level handler with the YAML DSL (idempotent).
pub fn register() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| cameleer_yaml::register_top_level_handler("rest", rest_handler));
}

fn s(ctx: &CamelContext, v: &Value) -> Result<String> {
    let raw = match v {
        Value::Null | Value::Array(_) | Value::Object(_) => return Err(CamelError::illegal_argument(format!("expected a scalar but found {}", v.type_name()))),
        other => other.to_string(),
    };
    if raw.contains("{{") {
        ctx.resolve_placeholders(&raw)
    } else {
        Ok(raw)
    }
}

fn opt_s(ctx: &CamelContext, v: &Value, key: &str) -> Result<Option<String>> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(x) => s(ctx, x).map(Some),
    }
}

fn opt_bool(ctx: &CamelContext, v: &Value, key: &str) -> Result<Option<bool>> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(x) => {
            let t = s(ctx, x)?;
            Value::String(t.clone()).as_bool().map(Some).ok_or_else(|| CamelError::illegal_argument(format!("option '{}': '{}' is not a boolean", key, t)))
        }
    }
}

/// `[{key: k, value: v}]` (Camel shape) or a plain mapping.
fn key_values(ctx: &CamelContext, v: Option<&Value>) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    match v {
        None | Some(Value::Null) => {}
        Some(Value::Array(items)) => {
            for it in items {
                let k = opt_s(ctx, it, "key")?.ok_or_else(|| CamelError::illegal_argument("property requires a 'key'"))?;
                let val = opt_s(ctx, it, "value")?.unwrap_or_default();
                out.push((k, val));
            }
        }
        Some(Value::Object(m)) if m.contains_key("key") => {
            let k = opt_s(ctx, v.unwrap(), "key")?.unwrap_or_default();
            out.push((k, opt_s(ctx, v.unwrap(), "value")?.unwrap_or_default()));
        }
        Some(Value::Object(m)) => {
            for (k, x) in m {
                out.push((k.clone(), s(ctx, x)?));
            }
        }
        Some(other) => return Err(CamelError::illegal_argument(format!("expected a list of key/value pairs but found {}", other.type_name()))),
    }
    Ok(out)
}

/// Apply a `restConfiguration` element.
pub fn apply_rest_configuration(ctx: &CamelContext, cfg: &mut RestConfiguration, v: &Value) -> Result<()> {
    let map = match v {
        Value::Object(m) => m.clone(),
        Value::Null => return Ok(()),
        other => return Err(CamelError::illegal_argument(format!("restConfiguration must be a mapping but found {}", other.type_name()))),
    };
    for (k, x) in &map {
        let wrap = |e: CamelError| CamelError::illegal_argument(format!("restConfiguration option '{}': {}", k, e.message));
        match k.as_str() {
            "component" => cfg.component = s(ctx, x).map_err(wrap)?,
            "host" => cfg.host = Some(s(ctx, x).map_err(wrap)?),
            "scheme" => cfg.scheme = s(ctx, x).map_err(wrap)?.to_ascii_lowercase(),
            "port" => {
                let p = s(ctx, x).map_err(wrap)?;
                cfg.port = Some(p.trim().parse::<u16>().map_err(|_| wrap(CamelError::illegal_argument(format!("invalid port '{}'", p))))?);
            }
            "contextPath" => cfg.context_path = s(ctx, x).map_err(wrap)?,
            "bindingMode" => cfg.binding_mode = RestBindingMode::parse(&s(ctx, x).map_err(wrap)?).map_err(wrap)?,
            "apiContextPath" => {
                cfg.api_context_path = s(ctx, x).map_err(wrap)?;
                cfg.api_docs = true;
            }
            "apiDocs" => cfg.api_docs = opt_bool(ctx, v, k).map_err(wrap)?.unwrap_or(true),
            "enableCORS" | "enableCors" => cfg.enable_cors = opt_bool(ctx, v, k).map_err(wrap)?.unwrap_or(false),
            "skipBindingOnErrorCode" => cfg.skip_binding_on_error_code = opt_bool(ctx, v, k).map_err(wrap)?.unwrap_or(true),
            "corsHeaders" => cfg.cors_headers.extend(key_values(ctx, Some(x)).map_err(wrap)?),
            "apiProperty" | "apiProperties" => cfg.api_properties.extend(key_values(ctx, Some(x)).map_err(wrap)?),
            "endpointProperty" | "endpointProperties" | "consumerProperty" | "consumerProperties" => {
                cfg.endpoint_properties.extend(key_values(ctx, Some(x)).map_err(wrap)?)
            }
            other => cameleer_core::camel_debug!("cameleer-main", "restConfiguration option '{}' is not supported and is ignored", other),
        }
    }
    Ok(())
}

fn param_type(t: &str) -> Result<RestParamType> {
    Ok(match t.to_ascii_lowercase().as_str() {
        "path" => RestParamType::Path,
        "query" => RestParamType::Query,
        "header" => RestParamType::Header,
        "body" => RestParamType::Body,
        "formdata" => RestParamType::FormData,
        other => return Err(CamelError::illegal_argument(format!("invalid rest param type '{}' (path, query, header, body, formData)", other))),
    })
}

fn parse_param(ctx: &CamelContext, v: &Value) -> Result<RestParam> {
    let name = opt_s(ctx, v, "name")?.ok_or_else(|| CamelError::illegal_argument("rest param requires a 'name'"))?;
    let kind = param_type(&opt_s(ctx, v, "type")?.unwrap_or_else(|| "path".into()))?;
    let mut p = RestParam::new(&name, kind);
    if let Some(r) = opt_bool(ctx, v, "required")? {
        p = p.required(r);
    }
    if let Some(d) = opt_s(ctx, v, "description")? {
        p = p.description(&d);
    }
    if let Some(d) = opt_s(ctx, v, "dataType")? {
        p = p.data_type(&d);
    }
    if let Some(d) = opt_s(ctx, v, "defaultValue")? {
        p = p.default_value(&d);
    }
    let allowable: Vec<String> = match v.get("allowableValues") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a.iter().map(|x| x.get("value").cloned().unwrap_or_else(|| x.clone()).to_string()).collect(),
        Some(other) => other.to_string().split(',').map(|s| s.trim().to_string()).collect(),
    };
    if !allowable.is_empty() {
        let refs: Vec<&str> = allowable.iter().map(|s| s.as_str()).collect();
        p = p.allowable_values(&refs);
    }
    Ok(p)
}

fn items(v: Option<&Value>) -> Vec<&Value> {
    match v {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a.iter().collect(),
        Some(other) => vec![other],
    }
}

/// The `to` endpoint of a verb: `to: uri`, `to: {uri, parameters}`, or (XML) a `to` step.
fn verb_to(ctx: &CamelContext, v: &Value) -> Result<Option<String>> {
    if let Some(t) = v.get("to").or_else(|| v.get("toD")) {
        return endpoint_uri(t).map(Some);
    }
    if let Some(Value::Array(steps)) = v.get("steps") {
        for st in steps {
            if let Some(t) = st.get("to") {
                return endpoint_uri(t).map(Some);
            }
        }
    }
    let _ = ctx;
    Ok(None)
}

fn parse_verb(ctx: &CamelContext, method: &str, v: &Value) -> Result<VerbDefinition> {
    let mut d = VerbDefinition { method: method.to_ascii_uppercase(), api_docs: true, ..Default::default() };
    d.path = opt_s(ctx, v, "path")?.or(opt_s(ctx, v, "uri")?).unwrap_or_default();
    d.to = verb_to(ctx, v)?;
    d.route_id = opt_s(ctx, v, "id")?.or(opt_s(ctx, v, "routeId")?);
    d.description = opt_s(ctx, v, "description")?;
    d.consumes = opt_s(ctx, v, "consumes")?;
    d.produces = opt_s(ctx, v, "produces")?;
    if let Some(m) = opt_s(ctx, v, "bindingMode")? {
        d.binding_mode = Some(RestBindingMode::parse(&m)?);
    }
    for p in items(v.get("param")) {
        d.params.push(parse_param(ctx, p)?);
    }
    for r in items(v.get("responseMessage")) {
        let code = opt_s(ctx, r, "code")?.unwrap_or_else(|| "200".into());
        let code: u16 = code.trim().parse().map_err(|_| CamelError::illegal_argument(format!("invalid responseMessage code '{}'", code)))?;
        d.responses.push((code, opt_s(ctx, r, "message")?.unwrap_or_default()));
    }
    d.type_name = opt_s(ctx, v, "type")?;
    d.out_type = opt_s(ctx, v, "outType")?;
    if let Some(b) = opt_bool(ctx, v, "apiDocs")? {
        d.api_docs = b;
    }
    d.deprecated = opt_bool(ctx, v, "deprecated")?.unwrap_or(false);
    Ok(d)
}

/// Parse a `rest` element.
pub fn parse_rest(ctx: &CamelContext, v: &Value) -> Result<RestDefinition> {
    let mut r = RestDefinition {
        path: opt_s(ctx, v, "path")?.unwrap_or_default(),
        description: opt_s(ctx, v, "description")?,
        consumes: opt_s(ctx, v, "consumes")?,
        produces: opt_s(ctx, v, "produces")?,
        tag: opt_s(ctx, v, "tag")?,
        ..Default::default()
    };
    if let Some(m) = opt_s(ctx, v, "bindingMode")? {
        r.binding_mode = Some(RestBindingMode::parse(&m)?);
    }
    for verb in VERBS {
        for item in items(v.get(verb)) {
            r.verbs.push(parse_verb(ctx, verb, item)?);
        }
    }
    // generic `verb: [{method: GET, ...}]`
    for item in items(v.get("verb")) {
        let m = opt_s(ctx, item, "method")?.ok_or_else(|| CamelError::illegal_argument("verb requires a 'method'"))?;
        r.verbs.push(parse_verb(ctx, &m, item)?);
    }
    Ok(r)
}

fn rest_handler(tl: &mut TopLevelContext<'_>, v: &Value) -> Result<()> {
    let all = tl.document_elements("rest");
    let rests: Vec<&Value> = if all.len() > 1 {
        // all `rest` elements of the document are generated together, at the first one
        if !std::ptr::eq(all[0], v) {
            return Ok(());
        }
        all
    } else {
        vec![v]
    };
    let ctx = tl.camel().clone();
    // configurations of earlier files and of this document (also after the rest elements)
    let doc_configs: Vec<Value> = tl.document_elements("restConfiguration").into_iter().cloned().collect();
    if !ctx.is_started() {
        // validate now (errors report the file location), build the routes at startup so that
        // restConfiguration elements of files loaded later apply too
        let mut probe = RestConfiguration::default();
        for c in yaml_state(&ctx).rest_configurations().iter().chain(doc_configs.iter()) {
            apply_rest_configuration(&ctx, &mut probe, c).map_err(|e| tl.error(e.message))?;
        }
        for r in &rests {
            parse_rest(&ctx, r).map_err(|e| tl.error(e.message))?;
        }
        pending_rests(&ctx).rests.lock().unwrap().extend(rests.into_iter().cloned());
        return Ok(());
    }
    for route in build_rest_routes(&ctx, &rests, &doc_configs).map_err(|e| tl.error(e.message))? {
        tl.add_route(route);
    }
    Ok(())
}

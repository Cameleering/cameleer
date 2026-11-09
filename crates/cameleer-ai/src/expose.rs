//! Existing routes as tools, with no change to the routes (`camel.server.mcp-expose-routes`,
//! `cameleer run --mcp-expose`).
//!
//! A pattern selects routes by their consumer:
//!
//! - `direct:*`, `direct:orders-*`, `seda:*`: the consumer endpoint URI (without options);
//! - `rest:*`, `rest:/orders*`: REST DSL operations, by their full path;
//! - `route:orders-*`: route ids.
//!
//! A REST operation becomes a tool whose arguments are its documented path, query and header
//! parameters (and `body` for a body parameter, POST, PUT and PATCH), with their types, descriptions, defaults and
//! allowed values; path placeholders without a `param` are required strings. Any other route
//! takes `body` (any JSON value, the message body) and `headers` (an object of message headers).
//!
//! Exposed tools carry the tag `exposed`. GET and HEAD operations are read-only; every other
//! exposed tool is destructive, so its calls need approval (over MCP the client asks its user)
//! unless an override says otherwise. Overrides live in properties, keyed by route id:
//! `camel.server.mcp-expose-routes.<routeId>.<option>` with `name`, `title`, `description`,
//! `inputSchema`, the hints (`readOnlyHint`, `destructiveHint`, `idempotentHint`,
//! `openWorldHint`) and every `ai-tool` envelope option (`approval`, `timeout`, `rateLimit`,
//! `idempotency`, `redact`, `authorize`, `cacheTtl`, ...).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use cameleer_core::context::{CamelContext, Route};
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::spi::Processor;
use cameleer_core::util::Parameters;
use cameleer_core::value::{ObjectMap, Value};
use cameleer_http::binding::headers as http;
use cameleer_http::rest::{rest_operations, RestOperation, RestParamType};

use crate::component::tool_policy;
use crate::mcp::glob_match;
use crate::registry::{AiRegistry, ToolEntry};
use crate::tool::{parse_schema_option, Hints, ToolDefinition};

/// The tag every exposed tool carries.
pub const EXPOSED_TAG: &str = "exposed";
/// The property that lists the patterns.
pub const PROPERTY: &str = "camel.server.mcp-expose-routes";

/// Schemes whose routes are tools, resources or prompts already, never exposed again.
const SKIPPED_SCHEMES: [&str; 4] = ["ai-tool", "ai-resource", "ai-prompt", "mcp"];

/// Expose the routes that match `patterns` as tools in the context's tool registry; returns
/// the names of the tools, in route order. `props` holds the per-route overrides.
pub fn expose_routes(ctx: &CamelContext, patterns: &[String], props: &BTreeMap<String, String>) -> Result<Vec<String>> {
    if patterns.is_empty() {
        return Ok(Vec::new());
    }
    let registry = AiRegistry::of(ctx)?;
    let ops = rest_operations(ctx);
    let default_timeout = match props.get("camel.server.mcp-tool-timeout").or_else(|| props.get("camel.server.mcpToolTimeout")) {
        Some(t) => cameleer_core::util::parse_duration(t.trim())?,
        None => Duration::from_secs(20),
    };
    let mut names = Vec::new();
    for route in ctx.routes() {
        let scheme = route.endpoint_uri.split(':').next().unwrap_or("");
        if SKIPPED_SCHEMES.contains(&scheme) {
            continue;
        }
        let op = rest_operation_of(&route, &ops);
        if !matches(patterns, &route, op) {
            continue;
        }
        // the generated api-doc route and CORS preflight routes are infrastructure, not operations
        if op.is_none() && (route.id.starts_with("rest-api-doc") || method_of(&route.endpoint_uri).as_deref() == Some("OPTIONS")) {
            continue;
        }
        let prefix = format!("{}.{}.", PROPERTY, route.id);
        let mut overrides: BTreeMap<String, String> = props.iter().filter_map(|(k, v)| k.strip_prefix(&prefix).map(|o| (o.to_string(), v.clone()))).collect();
        let entry = tool_for(ctx, &route, op, &mut overrides, default_timeout)?;
        if let Some(unknown) = overrides.keys().next() {
            return Err(CamelError::illegal_argument(format!("Unknown option {}{} for an exposed route", prefix, unknown)));
        }
        names.push(entry.definition.name.clone());
        registry.register_tool(entry)?;
    }
    Ok(names)
}

fn matches(patterns: &[String], route: &Route, op: Option<&RestOperation>) -> bool {
    let uri = plain_uri(&route.endpoint_uri);
    patterns.iter().any(|p| {
        let p = p.trim();
        if let Some(rest) = p.strip_prefix("rest:") {
            op.is_some_and(|o| glob_match(rest, &o.path))
        } else if let Some(id) = p.strip_prefix("route:") {
            glob_match(id, &route.id)
        } else {
            glob_match(&plain_uri(p), &uri)
        }
    })
}

/// The URI without options, and `scheme://x` written as `scheme:x`.
fn plain_uri(uri: &str) -> String {
    let base = uri.split('?').next().unwrap_or(uri);
    match base.split_once("://") {
        Some((scheme, rest)) if !matches!(scheme, "http" | "https") && !rest.contains("://") => format!("{}:{}", scheme, rest),
        _ => base.to_string(),
    }
}

fn method_of(uri: &str) -> Option<String> {
    let query = uri.split_once('?')?.1;
    query.split('&').find_map(|kv| kv.strip_prefix("httpMethodRestrict=")).map(|m| m.to_ascii_uppercase())
}

/// The path a `platform-http:` (or `jetty:`/`undertow:`/`netty-http:` with a URL) route serves.
fn path_of(uri: &str) -> Option<String> {
    let base = uri.split('?').next()?;
    let (scheme, rest) = base.split_once(':')?;
    // the context normalizes `scheme:x` to `scheme://x`
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    match scheme {
        "platform-http" => Some(if rest.starts_with('/') { rest.to_string() } else { format!("/{}", rest) }),
        "jetty" | "undertow" | "netty-http" => {
            let url = rest.split_once("://").map(|(_, r)| r)?;
            Some(format!("/{}", url.split_once('/').map(|(_, p)| p).unwrap_or("")))
        }
        _ => None,
    }
}

fn rest_operation_of<'a>(route: &Route, ops: &'a [RestOperation]) -> Option<&'a RestOperation> {
    let path = path_of(&route.endpoint_uri)?;
    let method = method_of(&route.endpoint_uri)?;
    ops.iter().find(|o| o.method == method && o.path == path)
}

fn tool_for(ctx: &CamelContext, route: &Arc<Route>, op: Option<&RestOperation>, o: &mut BTreeMap<String, String>, default_timeout: Duration) -> Result<ToolEntry> {
    let name = o.remove("name").unwrap_or_else(|| tool_name(route, op));
    if name.is_empty() || name.len() > 64 || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')) {
        return Err(CamelError::illegal_argument(format!(
            "Route {} would be exposed as tool '{}', which is not a valid tool name (1-64 of A-Z a-z 0-9 _ - .); set {}.{}.name",
            route.id, name, PROPERTY, route.id
        )));
    }
    let description = o.remove("description").or_else(|| route.description.clone()).or_else(|| op.and_then(|o| o.description.clone())).unwrap_or_else(|| match op {
        Some(op) => format!("Calls the REST operation {} {}.", op.method, op.path),
        None => format!("Runs the route {} ({}) with the given message body and headers, and returns the resulting body.", route.id, plain_uri(&route.endpoint_uri)),
    });
    let read_only = op.is_some_and(|o| matches!(o.method.as_str(), "GET" | "HEAD"));
    let flag = |o: &mut BTreeMap<String, String>, k: &str| -> Result<Option<bool>> {
        match o.remove(k) {
            None => Ok(None),
            Some(v) => match v.trim().to_ascii_lowercase().as_str() {
                "true" => Ok(Some(true)),
                "false" => Ok(Some(false)),
                _ => Err(CamelError::illegal_argument(format!("{} must be true or false, got {}", k, v))),
            },
        }
    };
    let idempotent_method = op.is_some_and(|o| matches!(o.method.as_str(), "GET" | "HEAD" | "PUT" | "DELETE"));
    let hints = Hints {
        read_only: flag(o, "readOnlyHint")?.or(Some(read_only)),
        destructive: flag(o, "destructiveHint")?.or(Some(!read_only)),
        idempotent: flag(o, "idempotentHint")?.or(if idempotent_method { Some(true) } else { None }),
        open_world: flag(o, "openWorldHint")?,
    };
    let input_schema = match o.remove("inputSchema") {
        Some(s) => parse_schema_option("inputSchema", &s)?,
        None => match op {
            Some(op) => rest_schema(op),
            None => generic_schema(),
        },
    };
    let definition = ToolDefinition {
        name,
        title: o.remove("title"),
        description,
        tags: vec![EXPOSED_TAG.to_string()],
        input_schema,
        output_schema: None,
        hints: hints.clone(),
        version: o.remove("version").unwrap_or_else(|| "1".into()),
        searchable: true,
        return_direct: false,
    };
    // the envelope options are the ai-tool ones; whatever tool_policy does not take stays in
    // `o` and is reported as unknown
    let keys: Vec<String> = o.keys().cloned().collect();
    let mut params = Parameters::new(std::mem::take(o));
    let policy = tool_policy(ctx, &mut params, &hints, default_timeout)?;
    for k in keys {
        if let Some(v) = params.take(&k) {
            o.insert(k, v);
        }
    }
    let processor: Arc<dyn Processor> = Arc::new(ExposedRoute { route: route.clone(), op: op.cloned() });
    Ok(ToolEntry::new(definition, policy, processor, ctx))
}

/// The tool's name: the route id when it was chosen by hand, else the `direct:` name, else the
/// REST method and path (`get_orders_id`), else the route id.
fn tool_name(route: &Route, op: Option<&RestOperation>) -> String {
    let generated = route.id.strip_prefix("route").is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
    if !generated {
        return sanitize(&route.id);
    }
    if let Some(op) = op {
        let path: Vec<String> = op.path.split('/').filter(|s| !s.is_empty()).map(|s| s.trim_matches(|c| c == '{' || c == '}').to_string()).collect();
        return sanitize(&format!("{}_{}", op.method.to_ascii_lowercase(), path.join("_")));
    }
    let uri = plain_uri(&route.endpoint_uri);
    match uri.split_once(':') {
        Some(("direct" | "seda" | "vm" | "direct-vm", name)) if !name.is_empty() => sanitize(name),
        _ => sanitize(&route.id),
    }
}

fn sanitize(s: &str) -> String {
    let mut out: String = s.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') { c } else { '_' }).collect();
    out.truncate(64);
    out
}

fn obj(items: Vec<(&str, Value)>) -> Value {
    Value::Object(items.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

/// `body` (any JSON value) and `headers` (an object), nothing else.
fn generic_schema() -> Value {
    obj(vec![
        ("type", Value::from("object")),
        (
            "properties",
            obj(vec![
                ("body", obj(vec![("description", Value::from("The message body: text, or any JSON value."))])),
                ("headers", obj(vec![("type", Value::from("object")), ("description", Value::from("Message headers to set, by name."))])),
            ]),
        ),
        ("additionalProperties", Value::Bool(false)),
    ])
}

fn json_type(data_type: &str) -> &'static str {
    match data_type.to_ascii_lowercase().as_str() {
        "int" | "integer" | "long" | "short" => "integer",
        "number" | "double" | "float" | "decimal" => "number",
        "boolean" | "bool" => "boolean",
        "array" => "array",
        "object" => "object",
        _ => "string",
    }
}

/// The documented parameters of a REST operation as a JSON Schema; path placeholders that have
/// no `param` are required strings, and a body parameter (or a POST, PUT or PATCH) adds `body`.
fn rest_schema(op: &RestOperation) -> Value {
    let mut props = ObjectMap::new();
    let mut required: Vec<Value> = Vec::new();
    let mut has_body = matches!(op.method.as_str(), "POST" | "PUT" | "PATCH");
    for p in &op.params {
        if p.kind == RestParamType::Body {
            has_body = true;
            continue;
        }
        if p.kind == RestParamType::FormData {
            continue;
        }
        let mut s = vec![("type", Value::from(json_type(&p.data_type)))];
        let place = match p.kind {
            RestParamType::Path => "path",
            RestParamType::Query => "query",
            _ => "header",
        };
        let desc = match &p.description {
            Some(d) => format!("{} ({} parameter)", d, place),
            None => format!("The {} parameter {}.", place, p.name),
        };
        s.push(("description", Value::from(desc)));
        if let Some(d) = &p.default_value {
            s.push(("default", Value::from(d.as_str())));
        }
        if !p.allowable_values.is_empty() {
            s.push(("enum", Value::Array(p.allowable_values.iter().map(|v| Value::from(v.as_str())).collect())));
        }
        props.insert(p.name.clone(), obj(s));
        if p.required || p.kind == RestParamType::Path {
            required.push(Value::from(p.name.as_str()));
        }
    }
    for seg in op.path.split('/') {
        if let Some(name) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            if !props.contains_key(name) {
                props.insert(name.to_string(), obj(vec![("type", Value::from("string")), ("description", Value::from(format!("The path parameter {}.", name)))]));
                required.push(Value::from(name));
            }
        }
    }
    if has_body {
        let desc = match &op.type_name {
            Some(t) => format!("The request body ({}).", t),
            None => "The request body: a JSON value, or text.".to_string(),
        };
        props.insert("body".to_string(), obj(vec![("description", Value::from(desc))]));
        if op.params.iter().any(|p| p.kind == RestParamType::Body && p.required) {
            required.push(Value::from("body"));
        }
    }
    let mut schema = vec![("type", Value::from("object")), ("properties", Value::Object(props)), ("additionalProperties", Value::Bool(false))];
    if !required.is_empty() {
        schema.push(("required", Value::Array(required)));
    }
    obj(schema)
}

/// Runs an exposed route: turns the tool's arguments (set as headers by the registry) into the
/// message the route's consumer would have produced, then the route's reply into a tool result.
struct ExposedRoute {
    route: Arc<Route>,
    op: Option<RestOperation>,
}

impl Processor for ExposedRoute {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        match &self.op {
            Some(op) => {
                self.to_rest_request(op, ex);
                self.route.processor().process(ex)?;
                from_rest_reply(ex);
            }
            None => {
                if let Some(b) = ex.message.headers.remove("body") {
                    ex.set_body(b);
                }
                if let Some(Value::Object(h)) = ex.message.headers.remove("headers") {
                    for (k, v) in h.iter() {
                        ex.set_header(k.as_str(), v.clone());
                    }
                }
                self.route.processor().process(ex)?;
            }
        }
        Ok(())
    }
}

impl ExposedRoute {
    /// What the HTTP consumer sets: method, path with the placeholders filled in, query, and the
    /// body as JSON text; path, query and header parameters stay as headers, as the consumer
    /// leaves them.
    fn to_rest_request(&self, op: &RestOperation, ex: &mut Exchange) {
        let body = ex.message.headers.remove("body");
        let value = |ex: &Exchange, name: &str| ex.header(name).map(|v| v.to_string());
        let path: Vec<String> = op
            .path
            .split('/')
            .map(|seg| match seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                Some(name) => cameleer_http::http::percent_encode(&value(ex, name).unwrap_or_default()),
                None => seg.to_string(),
            })
            .collect();
        let path = path.join("/");
        let query: Vec<(String, String)> = op.params.iter().filter(|p| p.kind == RestParamType::Query).filter_map(|p| value(ex, &p.name).map(|v| (p.name.clone(), v))).collect();
        let raw_query = query.iter().map(|(k, v)| format!("{}={}", cameleer_http::http::percent_encode(k), cameleer_http::http::percent_encode(v))).collect::<Vec<_>>().join("&");
        ex.set_header(http::HTTP_METHOD, op.method.as_str());
        ex.set_header(http::HTTP_PATH, path.as_str());
        ex.set_header(http::HTTP_URI, if raw_query.is_empty() { path.clone() } else { format!("{}?{}", path, raw_query) });
        if !raw_query.is_empty() {
            ex.set_header(http::HTTP_RAW_QUERY, raw_query.as_str());
            ex.set_header(http::HTTP_QUERY, query.iter().map(|(k, v)| format!("{}={}", k, v)).collect::<Vec<_>>().join("&"));
        }
        match body {
            None | Some(Value::Null) => ex.set_body(Value::Null),
            Some(Value::String(s)) => {
                let json = cameleer_core::json::parse(&s).is_ok();
                ex.set_body(s);
                if json {
                    ex.set_header(http::CONTENT_TYPE, "application/json");
                }
            }
            Some(v) => {
                ex.set_body(v.to_json());
                ex.set_header(http::CONTENT_TYPE, "application/json");
            }
        }
    }
}

/// A status of 400 or more is a refusal the model sees (`CamelAiToolIsError`); a JSON text reply
/// becomes structured content.
fn from_rest_reply(ex: &mut Exchange) {
    let code = ex.header(http::HTTP_RESPONSE_CODE).and_then(|v| v.as_i64()).unwrap_or(200);
    if code >= 400 {
        let text = match ex.body() {
            Value::Null => format!("The operation answered HTTP {}", code),
            Value::Bytes(b) => format!("HTTP {}: {}", code, String::from_utf8_lossy(b)),
            v => format!("HTTP {}: {}", code, v.to_string()),
        };
        ex.set_body(text);
        ex.set_header(crate::headers::IS_ERROR, true);
        return;
    }
    let json = ex.header(http::CONTENT_TYPE).is_some_and(|c| c.to_string().to_ascii_lowercase().contains("json"));
    let text = match ex.body() {
        Value::String(s) => Some(s.clone()),
        Value::Bytes(b) => Some(String::from_utf8_lossy(b).into_owned()),
        _ => None,
    };
    if let Some(t) = text {
        if json || t.trim_start().starts_with('{') || t.trim_start().starts_with('[') {
            if let Ok(v) = cameleer_core::json::parse(&t) {
                if matches!(v, Value::Object(_) | Value::Array(_)) {
                    ex.set_body(v);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_and_methods() {
        assert_eq!(plain_uri("direct://lookup?timeout=5"), "direct:lookup");
        assert_eq!(plain_uri("direct:lookup"), "direct:lookup");
        assert_eq!(method_of("platform-http:/orders/{id}?httpMethodRestrict=get&produces=x"), Some("GET".into()));
        assert_eq!(path_of("platform-http:/api/orders/{id}?httpMethodRestrict=GET"), Some("/api/orders/{id}".into()));
        assert_eq!(path_of("platform-http:///orders/{id}?httpMethodRestrict=GET"), Some("/orders/{id}".into()));
        assert_eq!(path_of("jetty://http://0.0.0.0:8080/api/orders"), Some("/api/orders".into()));
        assert_eq!(path_of("jetty:http://0.0.0.0:8080/api/orders?httpMethodRestrict=GET"), Some("/api/orders".into()));
        assert_eq!(sanitize("get orders/{id}"), "get_orders__id_");
    }
}

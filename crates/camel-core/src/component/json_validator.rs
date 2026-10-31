//! `json-validator:` component - validates the message body (or a header) against a JSON
//! Schema (Camel `camel-json-validator`).
//!
//! `json-validator:schemas/order.json?failOnNullBody=true&headerName=&contentCache=false`
//!
//! The schema resource is loaded like a template resource (`file:`, `classpath:` (a
//! relative path), `ref:`, plain path; see [`template_support`](super::template_support)).
//! A string/bytes body is parsed as JSON; structured bodies (`Value` objects, arrays,
//! numbers...) are validated directly. On failure the exchange fails with a
//! `JsonValidationException` (a `ValidationException`) whose message lists every error
//! (`$.name: is missing but it is required`, networknt-style); the errors are also
//! available on the exchange property `CamelJsonValidationErrors` (a list of strings)
//! when the exception is raised. A null body fails with `NoJsonBodyValidationException`
//! unless `failOnNullBody=false` (`NoJsonHeaderValidationException` / `failOnNullHeader`
//! with `headerName`).
//!
//! Supported JSON Schema (draft-04 to draft-07, 2019-09 subset): boolean schemas,
//! `type` (incl. lists), `enum`, `const`, `minimum`/`maximum`, `exclusiveMinimum` /
//! `exclusiveMaximum` (numeric and draft-04 boolean forms), `multipleOf`, `minLength` /
//! `maxLength`, `pattern` (via [`crate::regex`]), `format` (`date-time`, `date`, `time`,
//! `email`, `ipv4`, `uuid`, `hostname`, `uri`; others are ignored), `items` (schema or
//! tuple) / `additionalItems`, `minItems` / `maxItems`, `uniqueItems`, `contains`,
//! `properties`, `required`, `additionalProperties`, `patternProperties`,
//! `propertyNames`, `minProperties` / `maxProperties`, `dependencies` /
//! `dependentRequired`, `allOf` / `anyOf` / `oneOf` / `not`, `if` / `then` / `else`, and
//! `$ref` to local JSON pointers (`#`, `#/definitions/x`, `#/$defs/x`). Remote `$ref`s
//! are not supported.

use std::any::Any;
use std::sync::{Arc, Mutex, OnceLock};

use crate::context::{CamelContext, WeakContext};
use crate::error::{kinds, register_kind_parent, CamelError, Result};
use crate::message::Exchange;
use crate::regex::Regex;
use crate::spi::{Component, Endpoint, Processor, Producer, Service};
use crate::util::Parameters;
use crate::value::Value;

use super::template_support::load_resource;

/// Exception kind raised when validation fails.
pub const JSON_VALIDATION_EXCEPTION: &str = "JsonValidationException";
/// Exception kind raised for a null body.
pub const NO_JSON_BODY_VALIDATION_EXCEPTION: &str = "NoJsonBodyValidationException";
/// Exception kind raised for a missing header.
pub const NO_JSON_HEADER_VALIDATION_EXCEPTION: &str = "NoJsonHeaderValidationException";
/// Exchange property holding the list of validation errors.
pub const JSON_VALIDATION_ERRORS: &str = "CamelJsonValidationErrors";

fn register_kinds() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        register_kind_parent(JSON_VALIDATION_EXCEPTION, kinds::VALIDATION);
        register_kind_parent(NO_JSON_BODY_VALIDATION_EXCEPTION, kinds::VALIDATION);
        register_kind_parent(NO_JSON_HEADER_VALIDATION_EXCEPTION, kinds::VALIDATION);
    });
}

/// The `json-validator` component.
#[derive(Default)]
pub struct JsonValidatorComponent;

impl Component for JsonValidatorComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        register_kinds();
        if remaining.trim().is_empty() {
            return Err(CamelError::illegal_argument("json-validator requires a schema resource uri"));
        }
        let cfg = Config {
            resource_uri: remaining.to_string(),
            fail_on_null_body: params.take_bool("failOnNullBody", true)?,
            fail_on_null_header: params.take_bool("failOnNullHeader", true)?,
            header_name: params.take("headerName").filter(|s| !s.is_empty()),
            content_cache: params.take_bool("contentCache", false)?,
        };
        for k in ["allowContextMapAll", "allowTemplateFromHeader", "errorHandler", "schemaLoader", "uriSchemaLoader", "objectMapper", "disabledDeserializationFeatures", "enabledDeserializationFeatures", "lazyStartProducer"] {
            params.take(k);
        }
        Ok(Arc::new(JsonValidatorEndpoint { uri: uri.to_string(), cfg, cache: Arc::new(Mutex::new(None)), ctx: ctx.downgrade() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Clone, Debug)]
struct Config {
    resource_uri: String,
    fail_on_null_body: bool,
    fail_on_null_header: bool,
    header_name: Option<String>,
    content_cache: bool,
}

/// A json-validator endpoint (producer only).
pub struct JsonValidatorEndpoint {
    uri: String,
    cfg: Config,
    cache: Arc<Mutex<Option<Arc<Value>>>>,
    ctx: WeakContext,
}

impl Endpoint for JsonValidatorEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(JsonValidatorProducer { cfg: self.cfg.clone(), cache: self.cache.clone(), ctx: self.ctx.clone() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct JsonValidatorProducer {
    cfg: Config,
    cache: Arc<Mutex<Option<Arc<Value>>>>,
    ctx: WeakContext,
}

impl Service for JsonValidatorProducer {}

impl JsonValidatorProducer {
    fn schema(&self, ex: &Exchange) -> Result<Arc<Value>> {
        let mut c = self.cache.lock().unwrap();
        if let (true, Some(s)) = (self.cfg.content_cache, c.as_ref()) {
            return Ok(s.clone());
        }
        let ctx = ex.context.clone().or_else(|| self.ctx.upgrade());
        let text = load_resource(ctx.as_ref(), &self.cfg.resource_uri)?;
        let schema = Arc::new(
            crate::json::parse(&text)
                .map_err(|e| CamelError::illegal_argument(format!("Invalid JSON schema {}: {}", self.cfg.resource_uri, e.message)))?,
        );
        if self.cfg.content_cache {
            *c = Some(schema.clone());
        }
        Ok(schema)
    }
}

impl Processor for JsonValidatorProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let content = match &self.cfg.header_name {
            Some(h) => match ex.header(h) {
                Some(v) if !v.is_null() => v.clone(),
                _ if self.cfg.fail_on_null_header => {
                    return Err(CamelError::new(NO_JSON_HEADER_VALIDATION_EXCEPTION, format!("No header with name {} present", h)))
                }
                _ => return Ok(()),
            },
            None => match ex.body() {
                Value::Null if self.cfg.fail_on_null_body => {
                    return Err(CamelError::new(NO_JSON_BODY_VALIDATION_EXCEPTION, format!("No body available on exchange {}", ex.exchange_id)))
                }
                Value::Null => return Ok(()),
                b => b.clone(),
            },
        };
        let value = match &content {
            Value::String(s) => parse_json_content(s)?,
            Value::Bytes(b) => parse_json_content(&String::from_utf8_lossy(b))?,
            other => other.clone(),
        };
        let schema = self.schema(ex)?;
        let errors = validate(&schema, &value);
        if errors.is_empty() {
            return Ok(());
        }
        ex.set_property(JSON_VALIDATION_ERRORS, Value::Array(errors.iter().map(|e| Value::String(e.clone())).collect()));
        Err(validation_exception(&errors))
    }
}

fn parse_json_content(s: &str) -> Result<Value> {
    crate::json::parse(s).map_err(|e| {
        register_kinds();
        CamelError::new(JSON_VALIDATION_EXCEPTION, format!("JSON validation error with 1 errors:\n$: invalid JSON content: {}", e.message))
    })
}

/// Build the `JsonValidationException` for a list of errors.
pub fn validation_exception(errors: &[String]) -> CamelError {
    register_kinds();
    CamelError::new(JSON_VALIDATION_EXCEPTION, format!("JSON validation error with {} errors:\n{}", errors.len(), errors.join("\n")))
}

/// Validate `instance` against `schema`; returns the error messages (empty = valid).
pub fn validate(schema: &Value, instance: &Value) -> Vec<String> {
    let v = Validator { root: schema };
    let mut errors = Vec::new();
    v.validate(instance, schema, "$", &mut errors, 0);
    errors
}

struct Validator<'a> {
    root: &'a Value,
}

fn json_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Int(_) => "integer",
        Value::Float(f) if f.fract() == 0.0 && f.is_finite() => "integer",
        Value::Float(_) => "number",
        Value::String(_) | Value::Bytes(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn type_matches(v: &Value, t: &str) -> bool {
    let jt = json_type(v);
    jt == t || (t == "number" && jt == "integer")
}

fn short(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_json(),
    }
}

fn str_len(v: &Value) -> Option<usize> {
    match v {
        Value::String(s) => Some(s.chars().count()),
        Value::Bytes(b) => Some(String::from_utf8_lossy(b).chars().count()),
        _ => None,
    }
}

fn child_path(path: &str, key: &str) -> String {
    if !key.is_empty() && key.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') {
        format!("{}.{}", path, key)
    } else {
        format!("{}['{}']", path, key)
    }
}

impl<'a> Validator<'a> {
    fn resolve_ref(&self, r: &str) -> Option<&'a Value> {
        let pointer = r.strip_prefix('#')?;
        let mut cur = self.root;
        for raw in pointer.split('/').skip(1) {
            let seg = crate::util::url_decode(&raw.replace('+', "%2B")).replace("~1", "/").replace("~0", "~");
            cur = match cur {
                Value::Object(m) => m.get(&seg)?,
                Value::Array(a) => a.get(seg.parse::<usize>().ok()?)?,
                _ => return None,
            };
        }
        Some(cur)
    }

    fn is_valid(&self, v: &Value, s: &Value, depth: usize) -> bool {
        let mut e = Vec::new();
        self.validate(v, s, "$", &mut e, depth);
        e.is_empty()
    }

    fn validate(&self, v: &Value, s: &Value, path: &str, errs: &mut Vec<String>, depth: usize) {
        if depth > 64 {
            errs.push(format!("{}: schema nesting too deep (recursive $ref?)", path));
            return;
        }
        let m = match s {
            Value::Bool(true) => return,
            Value::Bool(false) => {
                errs.push(format!("{}: schema for this element is false, no value is allowed", path));
                return;
            }
            Value::Object(m) => m,
            _ => return,
        };
        let num = |k: &str| m.get(k).and_then(|x| x.as_f64());
        if let Some(Value::String(r)) = m.get("$ref") {
            match self.resolve_ref(r) {
                Some(target) => self.validate(v, target, path, errs, depth + 1),
                None => errs.push(format!("{}: cannot resolve $ref {}", path, r)),
            }
        }
        // type
        match m.get("type") {
            Some(Value::String(t)) if !type_matches(v, t) => {
                errs.push(format!("{}: {} found, {} expected", path, json_type(v), t));
            }
            Some(Value::Array(ts)) => {
                let names: Vec<String> = ts.iter().map(short).collect();
                if !names.iter().any(|t| type_matches(v, t)) {
                    errs.push(format!("{}: {} found, [{}] expected", path, json_type(v), names.join(", ")));
                }
            }
            _ => {}
        }
        if let Some(Value::Array(options)) = m.get("enum") {
            if !options.iter().any(|o| json_eq(o, v)) {
                let names: Vec<String> = options.iter().map(short).collect();
                errs.push(format!("{}: does not have a value in the enumeration [{}]", path, names.join(", ")));
            }
        }
        if let Some(c) = m.get("const") {
            if !json_eq(c, v) {
                errs.push(format!("{}: must be a constant value {}", path, short(c)));
            }
        }
        // numbers
        if let (Some(x), true) = (v.as_f64(), matches!(v, Value::Int(_) | Value::Float(_))) {
            let excl_bool = |k: &str| matches!(m.get(k), Some(Value::Bool(true)));
            if let Some(min) = num("minimum") {
                if excl_bool("exclusiveMinimum") && x <= min {
                    errs.push(format!("{}: must have an exclusive minimum value of {}", path, fmt_num(min)));
                } else if x < min {
                    errs.push(format!("{}: must have a minimum value of {}", path, fmt_num(min)));
                }
            }
            if let Some(max) = num("maximum") {
                if excl_bool("exclusiveMaximum") && x >= max {
                    errs.push(format!("{}: must have an exclusive maximum value of {}", path, fmt_num(max)));
                } else if x > max {
                    errs.push(format!("{}: must have a maximum value of {}", path, fmt_num(max)));
                }
            }
            if let Some(Value::Int(_) | Value::Float(_)) = m.get("exclusiveMinimum") {
                let min = num("exclusiveMinimum").unwrap();
                if x <= min {
                    errs.push(format!("{}: must have an exclusive minimum value of {}", path, fmt_num(min)));
                }
            }
            if let Some(Value::Int(_) | Value::Float(_)) = m.get("exclusiveMaximum") {
                let max = num("exclusiveMaximum").unwrap();
                if x >= max {
                    errs.push(format!("{}: must have an exclusive maximum value of {}", path, fmt_num(max)));
                }
            }
            if let Some(d) = num("multipleOf") {
                if d > 0.0 {
                    let q = x / d;
                    if (q - q.round()).abs() > 1e-9 {
                        errs.push(format!("{}: must be multiple of {}", path, fmt_num(d)));
                    }
                }
            }
        }
        // strings
        if let Some(len) = str_len(v) {
            if let Some(min) = num("minLength") {
                if (len as f64) < min {
                    errs.push(format!("{}: must be at least {} characters long", path, fmt_num(min)));
                }
            }
            if let Some(max) = num("maxLength") {
                if (len as f64) > max {
                    errs.push(format!("{}: may only be {} characters long", path, fmt_num(max)));
                }
            }
            let text = v.to_string();
            if let Some(Value::String(p)) = m.get("pattern") {
                match Regex::new(p) {
                    Ok(re) if !re.is_match(&text) => errs.push(format!("{}: does not match the regex pattern {}", path, p)),
                    Ok(_) => {}
                    Err(e) => errs.push(format!("{}: invalid pattern {}: {}", path, p, e.message)),
                }
            }
            if let Some(Value::String(f)) = m.get("format") {
                if let Some(false) = check_format(f, &text) {
                    errs.push(format!("{}: does not match the {} pattern", path, f));
                }
            }
        }
        // arrays
        if let Value::Array(items) = v {
            if let Some(min) = num("minItems") {
                if (items.len() as f64) < min {
                    errs.push(format!("{}: there must be a minimum of {} items in the array", path, fmt_num(min)));
                }
            }
            if let Some(max) = num("maxItems") {
                if (items.len() as f64) > max {
                    errs.push(format!("{}: there must be a maximum of {} items in the array", path, fmt_num(max)));
                }
            }
            if let Some(Value::Bool(true)) = m.get("uniqueItems") {
                let dup = items.iter().enumerate().any(|(i, a)| items[i + 1..].iter().any(|b| json_eq(a, b)));
                if dup {
                    errs.push(format!("{}: the items in the array must be unique", path));
                }
            }
            match m.get("items") {
                Some(Value::Array(tuple)) => {
                    for (i, item) in items.iter().enumerate() {
                        let p = format!("{}[{}]", path, i);
                        match tuple.get(i) {
                            Some(sch) => self.validate(item, sch, &p, errs, depth + 1),
                            None => match m.get("additionalItems") {
                                Some(Value::Bool(false)) => {
                                    errs.push(format!("{}: index '{}' is not defined in the schema and the schema does not allow additional items", path, i))
                                }
                                Some(sch @ Value::Object(_)) => self.validate(item, sch, &p, errs, depth + 1),
                                _ => {}
                            },
                        }
                    }
                }
                Some(sch) => {
                    for (i, item) in items.iter().enumerate() {
                        self.validate(item, sch, &format!("{}[{}]", path, i), errs, depth + 1);
                    }
                }
                None => {}
            }
            if let Some(c) = m.get("contains") {
                if !items.iter().any(|it| self.is_valid(it, c, depth + 1)) {
                    errs.push(format!("{}: does not contain an element that passes these validations: {}", path, c.to_json()));
                }
            }
        }
        // objects
        if let Value::Object(obj) = v {
            if let Some(Value::Array(req)) = m.get("required") {
                for r in req.iter().filter_map(|r| r.as_str()) {
                    if !obj.contains_key(r) {
                        errs.push(format!("{}: is missing but it is required", child_path(path, r)));
                    }
                }
            }
            if let Some(min) = num("minProperties") {
                if (obj.len() as f64) < min {
                    errs.push(format!("{}: should have a minimum of {} properties", path, fmt_num(min)));
                }
            }
            if let Some(max) = num("maxProperties") {
                if (obj.len() as f64) > max {
                    errs.push(format!("{}: should have a maximum of {} properties", path, fmt_num(max)));
                }
            }
            let props = m.get("properties").and_then(|p| p.as_object());
            let pattern_props: Vec<(Regex, &Value)> = m
                .get("patternProperties")
                .and_then(|p| p.as_object())
                .map(|pp| pp.iter().filter_map(|(k, s)| Regex::new(k).ok().map(|r| (r, s))).collect())
                .unwrap_or_default();
            for (k, val) in obj {
                let p = child_path(path, k);
                let mut matched = false;
                if let Some(sch) = props.and_then(|ps| ps.get(k)) {
                    matched = true;
                    self.validate(val, sch, &p, errs, depth + 1);
                }
                for (re, sch) in &pattern_props {
                    if re.is_match(k) {
                        matched = true;
                        self.validate(val, sch, &p, errs, depth + 1);
                    }
                }
                if !matched {
                    match m.get("additionalProperties") {
                        Some(Value::Bool(false)) => errs.push(format!(
                            "{}: is not defined in the schema and the schema does not allow additional properties",
                            p
                        )),
                        Some(sch @ Value::Object(_)) => self.validate(val, sch, &p, errs, depth + 1),
                        _ => {}
                    }
                }
                if let Some(pn) = m.get("propertyNames") {
                    let mut sub = Vec::new();
                    self.validate(&Value::String(k.clone()), pn, &p, &mut sub, depth + 1);
                    if !sub.is_empty() {
                        errs.push(format!("{}: property name '{}' is not valid: {}", path, k, sub.join("; ")));
                    }
                }
            }
            let deps = m.get("dependentRequired").or_else(|| m.get("dependencies")).and_then(|d| d.as_object());
            if let Some(deps) = deps {
                for (k, dep) in deps {
                    if !obj.contains_key(k) {
                        continue;
                    }
                    match dep {
                        Value::Array(names) => {
                            for n in names.iter().filter_map(|n| n.as_str()) {
                                if !obj.contains_key(n) {
                                    errs.push(format!("{}: has a missing property {} which is dependent required", child_path(path, n), k));
                                }
                            }
                        }
                        sch @ (Value::Object(_) | Value::Bool(_)) => self.validate(v, sch, path, errs, depth + 1),
                        _ => {}
                    }
                }
            }
        }
        // combinators
        if let Some(Value::Array(all)) = m.get("allOf") {
            for sch in all {
                self.validate(v, sch, path, errs, depth + 1);
            }
        }
        if let Some(Value::Array(any)) = m.get("anyOf") {
            if !any.iter().any(|sch| self.is_valid(v, sch, depth + 1)) {
                let mut detail = Vec::new();
                for sch in any {
                    self.validate(v, sch, path, &mut detail, depth + 1);
                }
                errs.push(format!("{}: should be valid to any of the schemas ({})", path, detail.join("; ")));
            }
        }
        if let Some(Value::Array(one)) = m.get("oneOf") {
            let n = one.iter().filter(|sch| self.is_valid(v, sch, depth + 1)).count();
            if n != 1 {
                errs.push(format!("{}: should be valid to one and only one of the schemas, but {} are valid", path, n));
            }
        }
        if let Some(not) = m.get("not") {
            if self.is_valid(v, not, depth + 1) {
                errs.push(format!("{}: should not be valid to the schema {}", path, not.to_json()));
            }
        }
        if let Some(cond) = m.get("if") {
            if self.is_valid(v, cond, depth + 1) {
                if let Some(t) = m.get("then") {
                    self.validate(v, t, path, errs, depth + 1);
                }
            } else if let Some(e) = m.get("else") {
                self.validate(v, e, path, errs, depth + 1);
            }
        }
    }
}

/// JSON equality: numbers compare by value (1 == 1.0), strings and bytes by content.
fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Bool(_), Value::Int(_) | Value::Float(_)) | (Value::Int(_) | Value::Float(_), Value::Bool(_)) => false,
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| json_eq(p, q)),
        (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().all(|(k, p)| y.get(k).map(|q| json_eq(p, q)).unwrap_or(false)),
        _ => a == b,
    }
}

fn fmt_num(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        format!("{}", x as i64)
    } else {
        format!("{}", x)
    }
}

/// Check a string `format`; None for unknown formats (ignored).
fn check_format(format: &str, s: &str) -> Option<bool> {
    fn re(p: &str, s: &str) -> bool {
        Regex::new(p).map(|r| r.is_full_match(s)).unwrap_or(true)
    }
    let date = r"\d{4}-(0[1-9]|1[0-2])-(0[1-9]|[12]\d|3[01])";
    let time = r"([01]\d|2[0-3]):[0-5]\d:([0-5]\d|60)(\.\d+)?([zZ]|[+-]([01]\d|2[0-3]):[0-5]\d)";
    Some(match format {
        "date" => re(date, s),
        "time" => re(time, s),
        "date-time" => re(&format!("{}[tT ]{}", date, time), s),
        "email" => re(r"[^@\s]+@[^@\s]+\.[^@\s]+", s),
        "ipv4" => {
            let parts: Vec<&str> = s.split('.').collect();
            parts.len() == 4 && parts.iter().all(|p| !p.is_empty() && p.len() <= 3 && p.parse::<u8>().is_ok() && (p.len() == 1 || !p.starts_with('0')))
        }
        "uuid" => re(r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}", s),
        "hostname" => {
            s.len() <= 253 && s.split('.').all(|l| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        }
        "uri" => re(r"[a-zA-Z][a-zA-Z0-9+.\-]*:\S*", s),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::parse;

    fn errs(schema: &str, doc: &str) -> Vec<String> {
        validate(&parse(schema).unwrap(), &parse(doc).unwrap())
    }

    #[test]
    fn types_and_required() {
        let s = r#"{"type":"object","required":["name","age"],"properties":{"name":{"type":"string"},"age":{"type":"integer","minimum":0}}}"#;
        assert!(errs(s, r#"{"name":"a","age":3}"#).is_empty());
        assert_eq!(errs(s, r#"{"age":"x"}"#), ["$.name: is missing but it is required", "$.age: string found, integer expected"]);
        assert_eq!(errs(s, r#"{"name":"a","age":-1}"#), ["$.age: must have a minimum value of 0"]);
        assert_eq!(errs(s, "[]"), ["$: array found, object expected"]);
        assert!(errs(r#"{"type":"integer"}"#, "2.0").is_empty());
        assert!(errs(r#"{"type":"number"}"#, "2").is_empty());
        assert!(errs(r#"{"type":["string","null"]}"#, "null").is_empty());
        assert_eq!(errs(r#"{"type":["string","null"]}"#, "1").len(), 1);
    }

    #[test]
    fn numbers_strings() {
        assert_eq!(errs(r#"{"exclusiveMinimum":0}"#, "0"), ["$: must have an exclusive minimum value of 0"]);
        assert_eq!(errs(r#"{"minimum":0,"exclusiveMinimum":true}"#, "0"), ["$: must have an exclusive minimum value of 0"]);
        assert_eq!(errs(r#"{"maximum":10}"#, "11"), ["$: must have a maximum value of 10"]);
        assert_eq!(errs(r#"{"exclusiveMaximum":10}"#, "10").len(), 1);
        assert_eq!(errs(r#"{"multipleOf":0.5}"#, "1.25").len(), 1);
        assert!(errs(r#"{"multipleOf":0.5}"#, "1.5").is_empty());
        assert_eq!(errs(r#"{"minLength":2,"maxLength":3}"#, r#""a""#), ["$: must be at least 2 characters long"]);
        assert_eq!(errs(r#"{"maxLength":3}"#, r#""abcd""#), ["$: may only be 3 characters long"]);
        assert!(errs(r#"{"maxLength":2}"#, r#""éé""#).is_empty());
        assert_eq!(errs(r#"{"pattern":"^[a-z]+$"}"#, r#""ab1""#), ["$: does not match the regex pattern ^[a-z]+$"]);
        assert!(errs(r#"{"pattern":"b"}"#, r#""abc""#).is_empty());
        assert_eq!(errs(r#"{"enum":["a","b"]}"#, r#""c""#), ["$: does not have a value in the enumeration [a, b]"]);
        assert_eq!(errs(r#"{"const":1}"#, "2"), ["$: must be a constant value 1"]);
        assert!(errs(r#"{"const":1}"#, "1.0").is_empty());
        assert!(errs(r#"{"format":"email"}"#, r#""a@b.io""#).is_empty());
        assert_eq!(errs(r#"{"format":"date-time"}"#, r#""2024-13-01T00:00:00Z""#).len(), 1);
        assert!(errs(r#"{"format":"date-time"}"#, r#""2024-12-01T10:00:00+02:00""#).is_empty());
        assert_eq!(errs(r#"{"format":"ipv4"}"#, r#""256.1.1.1""#).len(), 1);
        assert!(errs(r#"{"format":"whatever"}"#, r#""x""#).is_empty());
    }

    #[test]
    fn arrays() {
        let s = r#"{"type":"array","items":{"type":"integer"},"minItems":1,"maxItems":3,"uniqueItems":true}"#;
        assert!(errs(s, "[1,2]").is_empty());
        assert_eq!(errs(s, "[]"), ["$: there must be a minimum of 1 items in the array"]);
        assert_eq!(errs(s, "[1,2,3,4]"), ["$: there must be a maximum of 3 items in the array"]);
        assert_eq!(errs(s, "[1,1]"), ["$: the items in the array must be unique"]);
        assert_eq!(errs(s, r#"[1,"x"]"#), ["$[1]: string found, integer expected"]);
        let t = r#"{"items":[{"type":"string"},{"type":"integer"}],"additionalItems":false}"#;
        assert!(errs(t, r#"["a",1]"#).is_empty());
        assert_eq!(errs(t, r#"["a",1,2]"#).len(), 1);
        assert_eq!(errs(r#"{"contains":{"const":5}}"#, "[1,2]").len(), 1);
        assert!(errs(r#"{"contains":{"const":5}}"#, "[1,5]").is_empty());
    }

    #[test]
    fn objects() {
        let s = r#"{"properties":{"a":{}},"patternProperties":{"^x_":{"type":"integer"}},"additionalProperties":false}"#;
        assert!(errs(s, r#"{"a":1,"x_1":2}"#).is_empty());
        assert_eq!(errs(s, r#"{"b":1}"#), ["$.b: is not defined in the schema and the schema does not allow additional properties"]);
        assert_eq!(errs(s, r#"{"x_1":"s"}"#), ["$.x_1: string found, integer expected"]);
        assert_eq!(errs(r#"{"additionalProperties":{"type":"string"}}"#, r#"{"q":1}"#).len(), 1);
        assert_eq!(errs(r#"{"minProperties":2}"#, r#"{"q":1}"#).len(), 1);
        assert_eq!(errs(r#"{"propertyNames":{"maxLength":2}}"#, r#"{"abc":1}"#).len(), 1);
        assert_eq!(errs(r#"{"dependencies":{"a":["b"]}}"#, r#"{"a":1}"#).len(), 1);
        assert_eq!(errs(r#"{"properties":{"o":{"required":["x"]}}}"#, r#"{"o":{}}"#), ["$.o.x: is missing but it is required"]);
    }

    #[test]
    fn combinators_and_refs() {
        assert!(errs(r#"{"anyOf":[{"type":"string"},{"type":"integer"}]}"#, "1").is_empty());
        assert_eq!(errs(r#"{"anyOf":[{"type":"string"},{"type":"integer"}]}"#, "true").len(), 1);
        assert_eq!(errs(r#"{"allOf":[{"minimum":1},{"maximum":2}]}"#, "3"), ["$: must have a maximum value of 2"]);
        assert_eq!(errs(r#"{"oneOf":[{"type":"number"},{"type":"integer"}]}"#, "1").len(), 1);
        assert!(errs(r#"{"oneOf":[{"type":"number"},{"type":"integer"}]}"#, "1.5").is_empty());
        assert_eq!(errs(r#"{"not":{"type":"string"}}"#, r#""s""#).len(), 1);
        assert_eq!(errs(r#"{"if":{"minimum":10},"then":{"multipleOf":5},"else":{"maximum":3}}"#, "12").len(), 1);
        assert_eq!(errs(r#"{"if":{"minimum":10},"then":{"multipleOf":5},"else":{"maximum":3}}"#, "4").len(), 1);
        assert!(errs(r#"{"if":{"minimum":10},"then":{"multipleOf":5},"else":{"maximum":3}}"#, "15").is_empty());
        let s = r##"{"definitions":{"pos":{"type":"integer","minimum":1}},"$defs":{"name":{"type":"string"}},
            "properties":{"n":{"$ref":"#/definitions/pos"},"s":{"$ref":"#/$defs/name"},"child":{"$ref":"#"}}}"##;
        assert!(errs(s, r#"{"n":1,"s":"x","child":{"n":2}}"#).is_empty());
        assert_eq!(errs(s, r#"{"n":0,"child":{"s":1}}"#), ["$.child.s: integer found, string expected", "$.n: must have a minimum value of 1"]);
        assert_eq!(errs(r##"{"$ref":"#/nope"}"##, "1").len(), 1);
        assert!(errs("true", "1").is_empty());
        assert_eq!(errs("false", "1").len(), 1);
    }
}

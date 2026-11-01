//! Shared support for template components (`mustache`, `string-template`, `velocity`,
//! `freemarker`): resource loading, the template data model and a generic
//! endpoint/producer.
//!
//! Template resources: `file:path` (also `file://path`), `classpath:path` and
//! `resource:classpath:path` (read as a path relative to the working directory, as
//! there is no classpath), `resource:file:path`, `ref:beanName` (a `String` bound in the
//! registry) or a plain path.
//!
//! Common endpoint options: `allowTemplateFromHeader` (default false; enables the
//! template and resource-uri headers), `contentCache` (default false: the resource is
//! re-read for every exchange), `allowContextMapAll` (adds `camelContext` to the data
//! model), `encoding` (accepted, UTF-8 is always used).
//!
//! The data model ([`template_context`]) mirrors Camel's `ExchangeHelper.createVariableMap`:
//! `body`, `headers`, `variables`, `exchange` (`exchange.properties`, `exchange.exchangeId`,
//! `exchange.pattern`, `exchange.message`/`exchange.in`), `request`/`in`
//! (`body`, `headers`, `messageId`) and, with `allowContextMapAll`, `camelContext` (`name`).

use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::context::{CamelContext, WeakContext};
use crate::error::{CamelError, Result};
use crate::message::Exchange;
use crate::spi::{Endpoint, Processor, Producer, Service};
use crate::util::Parameters;
use crate::value::{ObjectMap, Value};

/// Loads partial templates by name (returns None when not found).
pub type PartialLoader<'a> = &'a dyn Fn(&str) -> Option<String>;

/// A template engine.
pub trait TemplateEngine: Send + Sync {
    /// Render `template` against the data model `context`.
    fn render(&self, template: &str, context: &Value, partials: PartialLoader<'_>) -> Result<String>;
}

/// Names of the per-component headers.
#[derive(Clone, Copy, Debug)]
pub struct TemplateHeaders {
    /// Header carrying a template (used when `allowTemplateFromHeader=true`).
    pub template: &'static str,
    /// Header carrying a resource uri overriding the endpoint's one.
    pub resource_uri: &'static str,
}

/// Resolve a resource uri to a file path.
pub fn resource_path(uri: &str) -> PathBuf {
    let mut s = uri.trim();
    s = s.strip_prefix("resource:").unwrap_or(s);
    for p in ["file://", "file:", "classpath:"] {
        if let Some(r) = s.strip_prefix(p) {
            s = r;
            break;
        }
    }
    PathBuf::from(s)
}

/// Load a template resource (see the module documentation for the supported forms).
pub fn load_resource(ctx: Option<&CamelContext>, uri: &str) -> Result<String> {
    if let Some(name) = uri.trim().strip_prefix("ref:").or_else(|| uri.trim().strip_prefix("bean:")) {
        let ctx = ctx.ok_or_else(|| CamelError::illegal_state("No CamelContext to look up template bean"))?;
        if let Some(s) = ctx.registry().lookup::<String>(name) {
            return Ok((*s).clone());
        }
        if let Some(s) = ctx.registry().lookup::<&'static str>(name) {
            return Ok((*s).to_string());
        }
        return Err(CamelError::new(crate::error::kinds::NO_SUCH_BEAN, format!("No String bean named {} for template {}", name, uri)));
    }
    let path = resource_path(uri);
    std::fs::read_to_string(&path).map_err(|e| {
        let kind = if e.kind() == std::io::ErrorKind::NotFound { crate::error::kinds::FILE_NOT_FOUND } else { crate::error::kinds::IO };
        CamelError::new(kind, format!("Cannot load resource {}: {}", uri, e))
    })
}

/// Look up a dotted path (`headers.foo`, `body.items.0.name`) in a data model value.
/// Object keys fall back to a case-insensitive match (Camel headers are case-insensitive).
pub fn lookup_path<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = root;
    for seg in path.split('.') {
        cur = lookup_key(cur, seg)?;
    }
    Some(cur)
}

/// Look up a single key/index in an object or array value.
pub fn lookup_key<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(m) => m.get(key).or_else(|| m.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v)),
        Value::Array(a) => match key {
            "size" | "length" => None,
            _ => key.parse::<usize>().ok().and_then(|i| a.get(i)),
        },
        _ => None,
    }
}

/// Build the template data model for an exchange.
pub fn template_context(ex: &Exchange, allow_context_map_all: bool) -> Value {
    let headers = ex.message.headers.to_value();
    let variables: ObjectMap =
        ex.properties.iter().filter_map(|(k, v)| k.strip_prefix("CamelVariable.").map(|n| (n.to_string(), v.clone()))).collect();
    let message = Value::object([
        ("body", ex.message.body.clone()),
        ("headers", headers.clone()),
        ("messageId", Value::String(ex.message.message_id.clone())),
    ]);
    let exchange = Value::object([
        ("properties", Value::from(ex.properties.clone())),
        ("exchangeId", Value::String(ex.exchange_id.clone())),
        ("pattern", Value::String(format!("{:?}", ex.pattern))),
        ("message", message.clone()),
        ("in", message.clone()),
        ("variables", Value::Object(variables.clone())),
    ]);
    let mut m = ObjectMap::new();
    m.insert("body".into(), ex.message.body.clone());
    m.insert("headers".into(), headers);
    m.insert("variables".into(), Value::Object(variables));
    m.insert("exchange".into(), exchange);
    m.insert("request".into(), message.clone());
    m.insert("in".into(), message);
    if allow_context_map_all {
        let name = ex.context.as_ref().map(|c| c.name().to_string()).unwrap_or_default();
        m.insert("camelContext".into(), Value::object([("name", Value::String(name))]));
    }
    Value::Object(m)
}

/// Common template endpoint options.
#[derive(Clone, Debug)]
pub struct TemplateOptions {
    pub allow_template_from_header: bool,
    pub content_cache: bool,
    pub allow_context_map_all: bool,
}

impl TemplateOptions {
    /// Consume the common options from endpoint parameters.
    pub fn from_params(params: &mut Parameters) -> Result<Self> {
        let o = TemplateOptions {
            allow_template_from_header: params.take_bool("allowTemplateFromHeader", false)?,
            content_cache: params.take_bool("contentCache", false)?,
            allow_context_map_all: params.take_bool("allowContextMapAll", false)?,
        };
        for k in ["encoding", "lazyStartProducer", "synchronous"] {
            params.take(k);
        }
        Ok(o)
    }
}

/// A generic template endpoint (producer only): renders the template and sets the result as body.
pub struct TemplateEndpoint {
    uri: String,
    resource_uri: String,
    options: TemplateOptions,
    headers: TemplateHeaders,
    engine: Arc<dyn TemplateEngine>,
    cache: Arc<Mutex<Option<String>>>,
    ctx: WeakContext,
}

impl TemplateEndpoint {
    pub fn new(
        ctx: &CamelContext,
        uri: &str,
        resource_uri: &str,
        options: TemplateOptions,
        headers: TemplateHeaders,
        engine: Arc<dyn TemplateEngine>,
    ) -> Result<Self> {
        if resource_uri.trim().is_empty() {
            return Err(CamelError::illegal_argument(format!("{}: a template resource uri is required", uri)));
        }
        Ok(TemplateEndpoint {
            uri: uri.to_string(),
            resource_uri: resource_uri.to_string(),
            options,
            headers,
            engine,
            cache: Arc::new(Mutex::new(None)),
            ctx: ctx.downgrade(),
        })
    }
    pub fn resource_uri(&self) -> &str {
        &self.resource_uri
    }
}

impl Endpoint for TemplateEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(TemplateProducer {
            resource_uri: self.resource_uri.clone(),
            options: self.options.clone(),
            headers: self.headers,
            engine: self.engine.clone(),
            cache: self.cache.clone(),
            ctx: self.ctx.clone(),
        }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct TemplateProducer {
    resource_uri: String,
    options: TemplateOptions,
    headers: TemplateHeaders,
    engine: Arc<dyn TemplateEngine>,
    cache: Arc<Mutex<Option<String>>>,
    ctx: WeakContext,
}

impl Service for TemplateProducer {}

fn base_dir(uri: &str) -> PathBuf {
    resource_path(uri).parent().map(Path::to_path_buf).unwrap_or_default()
}

impl Processor for TemplateProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let ctx = ex.context.clone().or_else(|| self.ctx.upgrade());
        let mut dir = base_dir(&self.resource_uri);
        let template = if self.options.allow_template_from_header && ex.header(self.headers.template).is_some() {
            ex.message.remove_header(self.headers.template).map(|v| v.to_string()).unwrap_or_default()
        } else if self.options.allow_template_from_header && ex.header(self.headers.resource_uri).is_some() {
            let uri = ex.message.remove_header(self.headers.resource_uri).map(|v| v.to_string()).unwrap_or_default();
            dir = base_dir(&uri);
            load_resource(ctx.as_ref(), &uri)?
        } else if self.options.content_cache {
            let mut c = self.cache.lock().unwrap();
            match c.as_ref() {
                Some(t) => t.clone(),
                None => {
                    let t = load_resource(ctx.as_ref(), &self.resource_uri)?;
                    *c = Some(t.clone());
                    t
                }
            }
        } else {
            load_resource(ctx.as_ref(), &self.resource_uri)?
        };
        let model = template_context(ex, self.options.allow_context_map_all);
        let partials = |name: &str| -> Option<String> {
            let candidates = [dir.join(name), dir.join(format!("{}.mustache", name))];
            candidates.iter().find_map(|p| std::fs::read_to_string(p).ok())
        };
        let out = self.engine.render(&template, &model, &partials)?;
        ex.set_body(out);
        Ok(())
    }
}

/// Java `toString()` rendering used by the JVM template engines: strings raw, lists as
/// `[a, b]`, maps as `{k=v, k2=v2}`, null as `null`.
pub fn java_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Array(a) => format!("[{}]", a.iter().map(java_string).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!("{{{}}}", o.iter().map(|(k, x)| format!("{}={}", k, java_string(x))).collect::<Vec<_>>().join(", ")),
        other => other.to_string(),
    }
}

fn java_index(len: usize, v: &Value) -> Option<usize> {
    v.as_i64().filter(|i| *i >= 0 && (*i as usize) < len).map(|i| i as usize)
}

/// Invoke a common Java method on a template value (`String`, `List`, `Map`, numbers,
/// Camel message/exchange maps). Returns None when the method is not available, so
/// engines can report the reference as unresolved.
pub fn java_method(target: &Value, name: &str, args: &[Value]) -> Option<Value> {
    let s = |i: usize| args.get(i).map(|a| a.to_string()).unwrap_or_default();
    let n = |i: usize| args.get(i).and_then(|a| a.as_i64()).unwrap_or(0);
    match (name, args.len()) {
        ("toString", 0) => return Some(Value::String(java_string(target))),
        ("equals", 1) => return Some(Value::Bool(crate::value::values_equal(target, &args[0]))),
        ("getClass", 0) => {
            let full = target.java_type_name().unwrap_or("java.lang.Object");
            return Some(Value::object([("name", Value::from(full)), ("simpleName", Value::from(full.rsplit('.').next().unwrap_or(full)))]));
        }
        _ => {}
    }
    match target {
        Value::String(_) | Value::Bytes(_) => {
            let t = target.to_string();
            let chars: Vec<char> = t.chars().collect();
            Some(match (name, args.len()) {
                ("toUpperCase", _) => Value::String(t.to_uppercase()),
                ("toLowerCase", _) => Value::String(t.to_lowercase()),
                ("length", 0) => Value::Int(chars.len() as i64),
                ("trim" | "strip", 0) => Value::String(t.trim().to_string()),
                ("isEmpty", 0) => Value::Bool(t.is_empty()),
                ("isBlank", 0) => Value::Bool(t.trim().is_empty()),
                ("charAt", 1) => Value::String(chars.get(n(0) as usize)?.to_string()),
                ("substring", 1) => Value::String(chars.get(n(0) as usize..)?.iter().collect()),
                ("substring", 2) => Value::String(chars.get(n(0) as usize..n(1) as usize)?.iter().collect()),
                ("indexOf", 1) => Value::Int(t.find(&s(0)).map(|b| t[..b].chars().count() as i64).unwrap_or(-1)),
                ("lastIndexOf", 1) => Value::Int(t.rfind(&s(0)).map(|b| t[..b].chars().count() as i64).unwrap_or(-1)),
                ("contains", 1) => Value::Bool(t.contains(&s(0))),
                ("startsWith", 1) => Value::Bool(t.starts_with(&s(0))),
                ("endsWith", 1) => Value::Bool(t.ends_with(&s(0))),
                ("replace", 2) => Value::String(t.replace(&s(0), &s(1))),
                ("replaceAll", 2) => Value::String(crate::regex::Regex::new(&s(0)).ok()?.replace_all(&t, &s(1))),
                ("replaceFirst", 2) => Value::String(crate::regex::Regex::new(&s(0)).ok()?.replace_first(&t, &s(1))),
                ("matches", 1) => Value::Bool(crate::regex::Regex::new(&s(0)).ok()?.is_full_match(&t)),
                ("split", 1) => Value::from(crate::regex::Regex::new(&s(0)).ok()?.split(&t)),
                ("equalsIgnoreCase", 1) => Value::Bool(t.eq_ignore_ascii_case(&s(0))),
                ("concat", 1) => Value::String(format!("{}{}", t, s(0))),
                ("compareTo", 1) => Value::Int(t.cmp(&s(0)) as i64),
                ("getBytes", 0) => Value::Bytes(t.into_bytes()),
                ("repeat", 1) => Value::String(t.repeat(n(0).max(0) as usize)),
                _ => return None,
            })
        }
        Value::Array(a) => Some(match (name, args.len()) {
            ("size", 0) => Value::Int(a.len() as i64),
            ("isEmpty", 0) => Value::Bool(a.is_empty()),
            ("get", 1) => a.get(java_index(a.len(), &args[0])?)?.clone(),
            ("contains", 1) => Value::Bool(a.iter().any(|x| crate::value::values_equal(x, &args[0]))),
            ("indexOf", 1) => Value::Int(a.iter().position(|x| crate::value::values_equal(x, &args[0])).map(|i| i as i64).unwrap_or(-1)),
            ("subList", 2) => Value::Array(a.get(n(0) as usize..n(1) as usize)?.to_vec()),
            _ => return None,
        }),
        Value::Object(o) => {
            let get = |k: &str| lookup_key(target, k).cloned();
            Some(match (name, args.len()) {
                ("get", 1) => get(&s(0)).unwrap_or(Value::Null),
                ("getOrDefault", 2) => get(&s(0)).unwrap_or_else(|| args[1].clone()),
                ("containsKey", 1) => Value::Bool(get(&s(0)).is_some()),
                ("containsValue", 1) => Value::Bool(o.values().any(|x| crate::value::values_equal(x, &args[0]))),
                ("size", 0) => Value::Int(o.len() as i64),
                ("isEmpty", 0) => Value::Bool(o.is_empty()),
                ("keySet", 0) => Value::Array(o.keys().map(|k| Value::String(k.clone())).collect()),
                ("values", 0) => Value::Array(o.values().cloned().collect()),
                ("entrySet", 0) => Value::Array(o.iter().map(|(k, v)| Value::object([("key", Value::String(k.clone())), ("value", v.clone())])).collect()),
                // Camel Message / Exchange accessors on the data model maps.
                ("getHeader", 1 | 2) => get("headers").and_then(|h| lookup_key(&h, &s(0)).cloned()).or_else(|| args.get(1).cloned()).unwrap_or(Value::Null),
                ("getProperty", 1 | 2) => get("properties").and_then(|h| lookup_key(&h, &s(0)).cloned()).or_else(|| args.get(1).cloned()).unwrap_or(Value::Null),
                ("getVariable", 1) => get("variables").and_then(|h| lookup_key(&h, &s(0)).cloned()).unwrap_or(Value::Null),
                ("getIn" | "getMessage", 0) => get("in").or_else(|| get("message"))?,
                (m, 0) if (m.len() > 3 && m.starts_with("get")) || (m.len() > 2 && m.starts_with("is")) => {
                    let prop = m.strip_prefix("get").or_else(|| m.strip_prefix("is")).unwrap_or(m);
                    let mut cs = prop.chars();
                    let first = cs.next()?;
                    let lower = format!("{}{}", first.to_ascii_lowercase(), cs.as_str());
                    get(&lower).or_else(|| get(prop))?
                }
                _ => return None,
            })
        }
        Value::Int(_) | Value::Float(_) => {
            let f = target.as_f64()?;
            Some(match (name, args.len()) {
                ("intValue" | "longValue" | "shortValue" | "byteValue", 0) => Value::Int(f as i64),
                ("doubleValue" | "floatValue", 0) => Value::Float(f),
                ("compareTo", 1) => Value::Int(f.partial_cmp(&args[0].as_f64()?).map(|o| o as i64).unwrap_or(0)),
                _ => return None,
            })
        }
        Value::Bool(b) => match (name, args.len()) {
            ("booleanValue", 0) => Some(Value::Bool(*b)),
            _ => None,
        },
        Value::Null => None,
    }
}

/// JavaBean-style property access (`$x.foo`): map key, list index, then `getFoo()`/`isFoo()`.
pub fn java_property(target: &Value, prop: &str) -> Option<Value> {
    match target {
        Value::Object(_) => return lookup_key(target, prop).cloned(),
        Value::Array(a) => {
            if let Ok(i) = prop.parse::<usize>() {
                return a.get(i).cloned();
            }
        }
        _ => {}
    }
    let mut cs = prop.chars();
    let first = cs.next()?;
    let cap = format!("{}{}", first.to_ascii_uppercase(), cs.as_str());
    java_method(target, &format!("get{}", cap), &[]).or_else(|| java_method(target, &format!("is{}", cap), &[]))
}

/// A token of a template lexer, for [`gobble_lines`].
pub trait GobbleToken {
    /// The text of a text token.
    fn text_mut(&mut self) -> Option<&mut String>;
    /// Whether this (directive) token swallows its line when alone on it.
    fn gobbles(&self) -> bool;
}

/// Remove the indentation before and the line break after directives (or runs of
/// directives) that are alone on their line (Velocity's default `lines` space gobbling,
/// FreeMarker's white-space stripping).
pub fn gobble_lines<T: GobbleToken>(toks: &mut [T]) {
    let mut i = 0;
    while i < toks.len() {
        if !toks[i].gobbles() {
            i += 1;
            continue;
        }
        // a run of directives [i, j)
        let mut j = i + 1;
        while j < toks.len() && toks[j].gobbles() {
            j += 1;
        }
        let before_ok = if i == 0 {
            true
        } else {
            match toks[i - 1].text_mut() {
                Some(t) => {
                    let ls = t.rfind('\n').map(|p| p + 1);
                    let tail_blank = t[ls.unwrap_or(0)..].chars().all(|c| c == ' ' || c == '\t');
                    tail_blank && (ls.is_some() || i == 1)
                }
                None => false,
            }
        };
        let after_ok = before_ok
            && (j == toks.len() || {
                let last = j + 1 == toks.len();
                match toks[j].text_mut() {
                    Some(t) => match t.find('\n') {
                        Some(p) => t[..p].chars().all(|c| c == ' ' || c == '\t' || c == '\r'),
                        None => last && t.chars().all(|c| c == ' ' || c == '\t'),
                    },
                    None => false,
                }
            });
        if after_ok {
            if i > 0 {
                if let Some(t) = toks[i - 1].text_mut() {
                    let ls = t.rfind('\n').map(|p| p + 1).unwrap_or(0);
                    t.truncate(ls);
                }
            }
            if j < toks.len() {
                if let Some(t) = toks[j].text_mut() {
                    *t = match t.find('\n') {
                        Some(p) => t[p + 1..].to_string(),
                        None => String::new(),
                    };
                }
            }
        }
        i = j;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_lookup() {
        assert_eq!(resource_path("file:/tmp/a.txt"), PathBuf::from("/tmp/a.txt"));
        assert_eq!(resource_path("classpath:t/a.mustache"), PathBuf::from("t/a.mustache"));
        assert_eq!(resource_path("resource:classpath:a"), PathBuf::from("a"));
        assert_eq!(resource_path("x/y.st"), PathBuf::from("x/y.st"));
        let v = Value::object([("headers", Value::object([("Foo", Value::from(1))])), ("list", Value::Array(vec![Value::from("a")]))]);
        assert_eq!(lookup_path(&v, "headers.foo"), Some(&Value::from(1)));
        assert_eq!(lookup_path(&v, "list.0"), Some(&Value::from("a")));
        assert!(lookup_path(&v, "nope.x").is_none());
    }

    #[test]
    fn context_model() {
        let mut ex = Exchange::with_body(None, "hi");
        ex.set_header("a", 1);
        ex.set_property("p", "v");
        ex.set_property("CamelVariable.x", "y");
        let m = template_context(&ex, false);
        assert_eq!(lookup_path(&m, "body"), Some(&Value::from("hi")));
        assert_eq!(lookup_path(&m, "exchange.properties.p"), Some(&Value::from("v")));
        assert_eq!(lookup_path(&m, "variables.x"), Some(&Value::from("y")));
        assert_eq!(lookup_path(&m, "request.headers.a"), Some(&Value::from(1)));
        assert!(lookup_path(&m, "camelContext").is_none());
    }
}

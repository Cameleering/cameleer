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
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::context::{CamelContext, WeakContext};
use crate::error::{CamelError, Result};
use crate::message::Exchange;
use crate::spi::{Endpoint, Processor, Producer, Service};
use crate::util::Parameters;
use crate::value::Value;

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
    let variables: BTreeMap<String, Value> =
        ex.properties.iter().filter_map(|(k, v)| k.strip_prefix("CamelVariable.").map(|n| (n.to_string(), v.clone()))).collect();
    let message = Value::object([
        ("body", ex.message.body.clone()),
        ("headers", headers.clone()),
        ("messageId", Value::String(ex.message.message_id.clone())),
    ]);
    let exchange = Value::object([
        ("properties", Value::Object(ex.properties.clone())),
        ("exchangeId", Value::String(ex.exchange_id.clone())),
        ("pattern", Value::String(format!("{:?}", ex.pattern))),
        ("message", message.clone()),
        ("in", message.clone()),
        ("variables", Value::Object(variables.clone())),
    ]);
    let mut m: BTreeMap<String, Value> = BTreeMap::new();
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

//! The Camel YAML DSL loader.
//!
//! Loads route definitions written in the [Camel YAML DSL] into a [`CamelContext`]:
//!
//! ```yaml
//! - route:
//!     id: hello
//!     from:
//!       uri: direct:start
//!       steps:
//!         - setBody:
//!             simple: "Hello ${body}"
//!         - to: mock:result
//! ```
//!
//! Supported top-level elements: `route`, `from` (short form), `beans`, `routeTemplate`
//! (with `parameters` and local `beans`), `templatedRoute` (`routeId`, `prefixId`,
//! `parameters`, `{{param:default}}`), `routeConfiguration` (`onException`,
//! `onCompletion`, `intercept`, `interceptFrom`, `interceptSendToEndpoint`,
//! `errorHandler`; applied to the routes referencing its `id` with
//! `routeConfigurationId`, or to all routes without `routeConfigurationId` when it has
//! no `id`), `errorHandler` (context wide: `deadLetterChannel`, `defaultErrorHandler`,
//! `noErrorHandler`, `refErrorHandler`), and top-level `onException` / `onCompletion` /
//! `intercept*` (anonymous route configurations). All of these, plus
//! `restConfiguration`, `rest` and `dataFormats`, are also stored raw in the
//! [`YamlState`] and passed to the top-level extension hooks
//! ([`register_top_level_handler`]; camel-main registers the REST DSL this way).
//!
//! Routes accept `id`, `description`, `group`, `autoStartup`, `startupOrder`,
//! `routeConfigurationId`, `errorHandler`, `errorHandlerRef` and `routePolicy`.
//!
//! ## EIPs
//!
//! Built-in steps cover the EIPs of the Rust DSL with the Camel 4 YAML property names:
//! `to`, `toD`, `setBody`, `setHeader(s)`, `filter`, `choice` (`when` / `otherwise`),
//! `split`, `aggregate`, `multicast`, `recipientList`, `routingSlip`, `dynamicRouter`,
//! `wireTap`, `enrich`, `pollEnrich`, `loop`, `loadBalance`, `resequence`, `sort`,
//! `pipeline`, `claimCheck`, `doTry` (`doCatch` / `doFinally`), `onException`,
//! `onCompletion`, `intercept*`, `throttle`, `delay`, `sample`, `idempotentConsumer`,
//! `circuitBreaker` (`onFallback`), `validate`, `rollback`, `throwException`, `threads`,
//! `transacted`, `policy`, `bean`, `script`, ... (see [`StepRegistry::names`]).
//! Bean references (`aggregationStrategy`, `idempotentRepository`, `ref`, ...) are
//! looked up in the context registry (a leading `#` / `#bean:` is ignored).
//!
//! ## Step registry
//!
//! Each step (`to`, `setBody`, `filter`, ...) is mapped to the route model by a
//! [`StepHandler`] registered in a [`StepRegistry`]. Handlers receive a
//! [`StepContext`] (access to the [`RouteDefinition`] being built, recursive
//! [`StepContext::load_steps`] for nested `steps:`, expression helpers, error
//! reporting with source locations) and the step's configuration value.
//! Additional EIPs are wired with [`register_step`] (global) or
//! [`StepRegistry::register`] (per loader).
//!
//! [Camel YAML DSL]: https://camel.apache.org/components/latest/others/yaml-dsl.html

use std::any::Any;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use camel_core::builder::ExpressionDefinition;
use camel_core::error::{kinds, register_kind_parent};
use camel_core::log::Level;
use camel_core::message::ExchangePattern;
use camel_core::model::{ProcessorDefinition, RouteDefinition};
use camel_core::processor::errors::CamelContextErrorExt;
use camel_core::processor::basic::*;
use camel_core::util::url_encode;
use camel_core::{camel_warn, CamelContext, CamelError, Result, Value, value::ObjectMap};

use crate::yaml::{self, Node, PathSegment};

mod config;
mod eips;
mod eips_extra;

pub use config::{apply_redelivery_policy, parse_error_handler, parse_redelivery_overrides, RedeliveryExtras, ROUTE_CONFIGURATION_ITEMS};
pub use eips::{exception_kind, ref_name, resolve_aggregation_strategy};

/// Exception kind used for YAML DSL errors (a child of `IllegalArgumentException`).
pub const YAML_DSL_ERROR: &str = "YamlDeserializationException";

/// Route extension key under which the raw YAML route configuration is stored
/// (`route.extension::<Value>(ROUTE_YAML_EXTENSION)`).
pub const ROUTE_YAML_EXTENSION: &str = "yaml";

/// Top-level elements that are stored raw (and passed to extension hooks).
pub const STORED_TOP_LEVEL: &[&str] = &[
    "routeConfiguration",
    "restConfiguration",
    "rest",
    "errorHandler",
    "onException",
    "onCompletion",
    "intercept",
    "interceptFrom",
    "interceptSendToEndpoint",
    "dataFormats",
];

/// Expression language keys recognised in YAML expressions.
pub const LANGUAGES: &[&str] = &[
    "simple",
    "constant",
    "header",
    "exchangeProperty",
    "variable",
    "jsonpath",
    "xpath",
    "jq",
    "tokenize",
    "xtokenize",
    "method",
    "groovy",
    "js",
    "javaScript",
    "python",
    "ognl",
    "mvel",
    "spel",
    "datasonnet",
    "csimple",
    "joor",
    "java",
    "hl7terser",
    "xquery",
    "ref",
    "wasm",
    "language",
];

fn dsl_error(message: impl Into<String>) -> CamelError {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| register_kind_parent(YAML_DSL_ERROR, kinds::ILLEGAL_ARGUMENT));
    CamelError::new(YAML_DSL_ERROR, message)
}

// ---------------------------------------------------------------------------
// Handler types and global registrations
// ---------------------------------------------------------------------------

/// Maps one YAML step to the route model.
pub type StepHandler = Arc<dyn Fn(&mut StepContext<'_>, &Value) -> Result<()> + Send + Sync>;

/// Handles a top-level element (e.g. `rest`) – may generate routes.
pub type TopLevelHandler = Arc<dyn Fn(&mut TopLevelContext<'_>, &Value) -> Result<()> + Send + Sync>;

/// Instantiates a bean declared in a `beans:` block (Rust cannot instantiate Java
/// classes by name, so applications register factories per bean `type`). The factory
/// typically binds the bean into `ctx.registry()`.
pub type BeanFactory = Arc<dyn Fn(&CamelContext, &BeanDefinition) -> Result<()> + Send + Sync>;

fn global_steps() -> &'static Mutex<BTreeMap<String, StepHandler>> {
    static G: OnceLock<Mutex<BTreeMap<String, StepHandler>>> = OnceLock::new();
    G.get_or_init(Default::default)
}
fn global_top_level() -> &'static Mutex<BTreeMap<String, TopLevelHandler>> {
    static G: OnceLock<Mutex<BTreeMap<String, TopLevelHandler>>> = OnceLock::new();
    G.get_or_init(Default::default)
}
fn global_bean_factories() -> &'static Mutex<BTreeMap<String, BeanFactory>> {
    static G: OnceLock<Mutex<BTreeMap<String, BeanFactory>>> = OnceLock::new();
    G.get_or_init(Default::default)
}

/// Register a step handler globally: every loader created afterwards (including the
/// one used by [`load_routes`]) knows the step. Overrides built-in handlers of the same name.
pub fn register_step<F>(name: impl Into<String>, handler: F)
where
    F: Fn(&mut StepContext<'_>, &Value) -> Result<()> + Send + Sync + 'static,
{
    global_steps().lock().unwrap().insert(name.into(), Arc::new(handler));
}

/// Register a global handler for a top-level element (e.g. `rest`, `routeConfiguration`).
pub fn register_top_level_handler<F>(name: impl Into<String>, handler: F)
where
    F: Fn(&mut TopLevelContext<'_>, &Value) -> Result<()> + Send + Sync + 'static,
{
    global_top_level().lock().unwrap().insert(name.into(), Arc::new(handler));
}

/// Register a global factory for beans of the given `type` (e.g. `com.foo.MyBean`).
pub fn register_bean_factory<F>(type_name: impl Into<String>, factory: F)
where
    F: Fn(&CamelContext, &BeanDefinition) -> Result<()> + Send + Sync + 'static,
{
    global_bean_factories().lock().unwrap().insert(type_name.into(), Arc::new(factory));
}

// ---------------------------------------------------------------------------
// Step registry
// ---------------------------------------------------------------------------

/// Registry of step handlers keyed by step name.
#[derive(Clone, Default)]
pub struct StepRegistry {
    handlers: BTreeMap<String, StepHandler>,
}

impl StepRegistry {
    /// A registry without any handler.
    pub fn empty() -> Self {
        StepRegistry::default()
    }

    /// The built-in handlers for the EIPs available in camel-core.
    pub fn core() -> Self {
        let mut r = StepRegistry::empty();
        register_core_steps(&mut r);
        r
    }

    /// Built-in handlers plus all globally registered ones ([`register_step`]).
    pub fn with_defaults() -> Self {
        let mut r = StepRegistry::core();
        for (k, v) in global_steps().lock().unwrap().iter() {
            r.handlers.insert(k.clone(), v.clone());
        }
        r
    }

    /// Register (or replace) a handler.
    pub fn register<F>(&mut self, name: impl Into<String>, handler: F) -> &mut Self
    where
        F: Fn(&mut StepContext<'_>, &Value) -> Result<()> + Send + Sync + 'static,
    {
        self.handlers.insert(name.into(), Arc::new(handler));
        self
    }

    /// Register an already shared handler.
    pub fn register_arc(&mut self, name: impl Into<String>, handler: StepHandler) -> &mut Self {
        self.handlers.insert(name.into(), handler);
        self
    }

    pub fn remove(&mut self, name: &str) -> Option<StepHandler> {
        self.handlers.remove(name)
    }

    pub fn get(&self, name: &str) -> Option<&StepHandler> {
        self.handlers.get(name)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.handlers.contains_key(name)
    }

    /// Names of all registered steps (sorted).
    pub fn names(&self) -> Vec<String> {
        self.handlers.keys().cloned().collect()
    }
}

// ---------------------------------------------------------------------------
// Source location tracking
// ---------------------------------------------------------------------------

/// The document being loaded (used to report source locations).
pub struct Source<'a> {
    /// File name, if loaded from a file.
    pub name: Option<&'a str>,
    /// Document index in a multi-document stream (None for single documents).
    pub doc_index: Option<usize>,
    /// The document value.
    pub value: &'a Value,
    /// The document node tree (with positions), if available.
    pub node: Option<&'a Node>,
}

impl<'a> Source<'a> {
    fn synthetic(value: &'a Value) -> Self {
        Source { name: None, doc_index: None, value, node: None }
    }

    fn path_string(&self, path: &[PathSegment]) -> String {
        let mut s = String::new();
        if let Some(i) = self.doc_index {
            s.push_str(&format!("document[{}]", i));
        }
        for seg in path {
            s.push_str(&seg.to_string());
        }
        if s.starts_with('.') {
            s.remove(0);
        }
        if s.is_empty() {
            s.push_str("<root>");
        }
        s
    }

    /// Human readable location: `at route[0].from.steps[1] (line 7, column 11 in routes.yaml)`.
    pub fn location(&self, path: &[PathSegment]) -> String {
        let mut s = format!("at {}", self.path_string(path));
        if let Some(node) = self.node {
            let (line, column) = node.position(path);
            s.push_str(&format!(" (line {}, column {}", line, column));
            if let Some(name) = self.name {
                s.push_str(&format!(" in {}", name));
            }
            s.push(')');
        } else if let Some(name) = self.name {
            s.push_str(&format!(" (in {})", name));
        }
        s
    }

    /// Find the path of `target` (by identity) below the value at `base`.
    fn find_path(&self, base: &[PathSegment], target: &Value) -> Option<Vec<PathSegment>> {
        let mut cur = self.value;
        for seg in base {
            cur = match seg {
                PathSegment::Key(k) => cur.as_object()?.get(k)?,
                PathSegment::Index(i) => cur.as_array()?.get(*i)?,
            };
        }
        let mut path = base.to_vec();
        if search(cur, target, &mut path, 4) {
            Some(path)
        } else {
            None
        }
    }
}

fn search(cur: &Value, target: &Value, path: &mut Vec<PathSegment>, depth: usize) -> bool {
    if std::ptr::eq(cur, target) {
        return true;
    }
    if depth == 0 {
        return false;
    }
    match cur {
        Value::Object(m) => {
            for (k, v) in m {
                path.push(PathSegment::Key(k.clone()));
                if search(v, target, path, depth - 1) {
                    return true;
                }
                path.pop();
            }
        }
        Value::Array(a) => {
            for (i, v) in a.iter().enumerate() {
                path.push(PathSegment::Index(i));
                if search(v, target, path, depth - 1) {
                    return true;
                }
                path.pop();
            }
        }
        _ => {}
    }
    false
}

// ---------------------------------------------------------------------------
// Step context
// ---------------------------------------------------------------------------

/// Context passed to step handlers.
pub struct StepContext<'a> {
    route: &'a mut RouteDefinition,
    camel: &'a CamelContext,
    loader: &'a YamlLoader,
    source: &'a Source<'a>,
    path: Vec<PathSegment>,
    step_name: String,
    pending_id: Option<String>,
    /// Blocks opened with [`StepContext::push_open_block`] in the current step list.
    dangling: usize,
}

impl<'a> StepContext<'a> {
    /// The route definition being built.
    pub fn route(&mut self) -> &mut RouteDefinition {
        self.route
    }

    pub fn camel(&self) -> &CamelContext {
        self.camel
    }

    pub fn loader(&self) -> &YamlLoader {
        self.loader
    }

    /// Name of the step being loaded.
    pub fn step_name(&self) -> &str {
        &self.step_name
    }

    /// The `id` configured on the current step (not yet applied).
    pub fn step_id(&self) -> Option<&str> {
        self.pending_id.as_deref()
    }

    /// Take the `id` of the current step (it will then not be applied automatically).
    pub fn take_step_id(&mut self) -> Option<String> {
        self.pending_id.take()
    }

    /// Path of the current step, e.g. `[0].route.from.steps[2].filter`.
    pub fn path(&self) -> &[PathSegment] {
        &self.path
    }

    /// Human readable location of the current step.
    pub fn location(&self) -> String {
        self.source.location(&self.path)
    }

    /// A DSL error mentioning the current step and its location.
    pub fn error(&self, message: impl std::fmt::Display) -> CamelError {
        dsl_error(format!("Error in step '{}' {}: {}", self.step_name, self.location(), message))
    }

    /// Add a definition at the current position. The step `id` is applied to it.
    pub fn add(&mut self, mut def: Box<dyn ProcessorDefinition>) {
        if let Some(id) = self.pending_id.take() {
            def.set_id(id);
        }
        self.route.add(def);
    }

    /// Open a block definition (subsequent definitions go inside it until [`end`](Self::end)).
    /// The step `id` is applied to it.
    pub fn push_block(&mut self, mut def: Box<dyn ProcessorDefinition>) {
        if let Some(id) = self.pending_id.take() {
            def.set_id(id);
        }
        self.route.push_block(def);
    }

    /// Close the innermost open block.
    pub fn end(&mut self) {
        self.route.end();
    }

    /// Open a block that extends to the end of the enclosing step list (like `threads`
    /// in Camel, where the remaining steps run inside it). It is closed automatically.
    pub fn push_open_block(&mut self, def: Box<dyn ProcessorDefinition>) {
        self.push_block(def);
        self.dangling += 1;
    }

    /// Open `def` as a block, load `steps` into it and close it.
    pub fn add_block(&mut self, def: Box<dyn ProcessorDefinition>, steps: Option<&Value>) -> Result<()> {
        self.push_block(def);
        let r = match steps {
            Some(s) => self.load_steps(s),
            None => Ok(()),
        };
        self.end();
        r
    }

    /// Load a list of steps (a YAML sequence of single-key maps) at the current position.
    /// `steps` is usually a value nested in the current step's configuration
    /// (e.g. `cfg.get("steps")`), which lets errors report precise locations.
    pub fn load_steps(&mut self, steps: &Value) -> Result<()> {
        let base = self.source.find_path(&self.path, steps).unwrap_or_else(|| {
            let mut p = self.path.clone();
            p.push(PathSegment::Key("steps".into()));
            p
        });
        match steps {
            Value::Null => Ok(()),
            Value::Array(items) => {
                let saved = std::mem::replace(&mut self.dangling, 0);
                let mut r = Ok(());
                for (i, item) in items.iter().enumerate() {
                    let mut p = base.clone();
                    p.push(PathSegment::Index(i));
                    r = self.load_step_at(item, p);
                    if r.is_err() {
                        break;
                    }
                }
                for _ in 0..self.dangling {
                    self.route.end();
                }
                self.dangling = saved;
                r
            }
            other => Err(dsl_error(format!(
                "Expected a list of steps but found {} {}",
                other.type_name(),
                self.source.location(&base)
            ))),
        }
    }

    fn load_step_at(&mut self, item: &Value, path: Vec<PathSegment>) -> Result<()> {
        let (name, cfg): (&str, &Value) = match item {
            Value::Object(m) if m.len() == 1 => {
                let (k, v) = m.iter().next().unwrap();
                (k.as_str(), v)
            }
            Value::Object(m) => {
                let keys: Vec<&str> = m.keys().map(|k| k.as_str()).collect();
                return Err(dsl_error(format!(
                    "A step must have exactly one key (the step name) but found [{}] {}",
                    keys.join(", "),
                    self.source.location(&path)
                )));
            }
            Value::String(s) => (s.as_str(), &Value::Null),
            other => {
                return Err(dsl_error(format!("Invalid step: expected a mapping but found {} {}", other.type_name(), self.source.location(&path))))
            }
        };
        if is_disabled(cfg) {
            return Ok(());
        }
        let handler = match self.loader.steps.get(name) {
            Some(h) => h.clone(),
            None => {
                let mut p = path.clone();
                p.push(PathSegment::Key(name.to_string()));
                return Err(dsl_error(format!(
                    "Unknown step '{}' {}. Supported steps: {}",
                    name,
                    self.source.location(&p),
                    self.loader.steps.names().join(", ")
                )));
            }
        };
        let mut p = path;
        p.push(PathSegment::Key(name.to_string()));
        let saved_path = std::mem::replace(&mut self.path, p);
        let saved_name = std::mem::replace(&mut self.step_name, name.to_string());
        let saved_id = std::mem::replace(&mut self.pending_id, cfg.get("id").and_then(scalar_string));
        let r = handler(self, cfg);
        let r = r.map_err(|e| {
            if e.kind == YAML_DSL_ERROR {
                e
            } else {
                dsl_error(format!("Error in step '{}' {}: {}", self.step_name, self.location(), e.message)).with_cause(e)
            }
        });
        self.path = saved_path;
        self.step_name = saved_name;
        self.pending_id = saved_id;
        r
    }

    // --- helpers for handlers ---

    /// Parse an expression from a step configuration (see [`parse_expression`]).
    pub fn expression(&self, cfg: &Value) -> Result<ExpressionDefinition> {
        parse_expression(cfg).map_err(|e| self.error(e.message))
    }

    /// Endpoint uri from a string or `{uri, parameters}`.
    pub fn uri(&self, cfg: &Value) -> Result<String> {
        endpoint_uri(cfg).map_err(|e| self.error(e.message))
    }

    /// A mandatory string option of the step configuration (first of the given keys).
    pub fn required_str(&self, cfg: &Value, keys: &[&str]) -> Result<String> {
        for k in keys {
            if let Some(s) = cfg.get(k).and_then(scalar_string) {
                return Ok(s);
            }
        }
        Err(self.error(format!("missing required option '{}'", keys[0])))
    }

    /// An optional string option.
    pub fn opt_str(&self, cfg: &Value, key: &str) -> Option<String> {
        cfg.get(key).and_then(scalar_string)
    }

    /// An optional boolean option (`true`/`"true"`), resolving property placeholders.
    pub fn opt_bool(&self, cfg: &Value, key: &str) -> Result<Option<bool>> {
        match cfg.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => resolve_bool(self.camel, v).map(Some).map_err(|e| self.error(e.message)),
        }
    }
}

fn is_disabled(cfg: &Value) -> bool {
    match cfg.get("disabled") {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

/// Render a scalar as a string (None for null and collections).
pub fn scalar_string(v: &Value) -> Option<String> {
    match v {
        Value::Null | Value::Array(_) | Value::Object(_) => None,
        other => Some(other.to_string()),
    }
}

fn option_string(v: &Value) -> String {
    match v {
        Value::Array(a) => a.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(","),
        other => other.to_string(),
    }
}

fn resolve_bool(ctx: &CamelContext, v: &Value) -> Result<bool> {
    match v {
        Value::Bool(b) => Ok(*b),
        Value::String(s) => {
            let r = ctx.resolve_placeholders(s)?;
            Value::String(r.clone()).as_bool().ok_or_else(|| dsl_error(format!("'{}' is not a boolean", r)))
        }
        other => Err(dsl_error(format!("expected a boolean but found {}", other.type_name()))),
    }
}

// ---------------------------------------------------------------------------
// Expressions
// ---------------------------------------------------------------------------

/// Find the expression in a step configuration. Supported forms:
///
/// - `simple: "${body}"` (any key from [`LANGUAGES`])
/// - `simple: {expression: "${body}", resultType: int, ...}` (other keys become language options)
/// - `expression: {simple: ...}` / `expression: {constant: ...}`
/// - `tokenize: {token: ","}` (no expression text)
/// - `language: {language: groovy, expression: "..."}`
///
/// Returns `None` if the configuration contains no expression.
pub fn find_expression(cfg: &Value) -> Result<Option<ExpressionDefinition>> {
    let map = match cfg {
        Value::Object(m) => m,
        _ => return Ok(None),
    };
    let langs: Vec<&String> = map.keys().filter(|k| LANGUAGES.contains(&k.as_str())).collect();
    if langs.len() > 1 {
        return Err(dsl_error(format!(
            "only one expression language may be specified but found: {}",
            langs.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
        )));
    }
    if let Some(lang) = langs.first() {
        return language_expression(lang, &map[*lang]).map(Some);
    }
    match map.get("expression") {
        Some(Value::Object(_)) => find_expression(&map["expression"])?
            .map(Some)
            .ok_or_else(|| dsl_error("'expression' must contain an expression language (e.g. simple, constant, header)")),
        Some(v @ (Value::String(_) | Value::Int(_) | Value::Float(_) | Value::Bool(_))) => {
            Ok(Some(ExpressionDefinition::language("simple", v.to_string())))
        }
        _ => Ok(None),
    }
}

/// Parse an expression (see [`find_expression`]); a plain scalar is taken as a Simple expression.
pub fn parse_expression(cfg: &Value) -> Result<ExpressionDefinition> {
    match cfg {
        Value::String(s) => Ok(ExpressionDefinition::language("simple", s.clone())),
        Value::Int(_) | Value::Float(_) | Value::Bool(_) => Ok(ExpressionDefinition::language("constant", cfg.to_string())),
        _ => find_expression(cfg)?.ok_or_else(|| {
            dsl_error(format!("missing expression: expected one of {} or 'expression'", LANGUAGES.join(", ")))
        }),
    }
}

fn language_expression(lang: &str, v: &Value) -> Result<ExpressionDefinition> {
    match v {
        Value::Object(opts) => {
            let mut name = lang.to_string();
            if lang == "language" {
                name = opts
                    .get("language")
                    .and_then(scalar_string)
                    .ok_or_else(|| dsl_error("'language' expression requires a 'language' option"))?;
            }
            let text = opts.get("expression").and_then(scalar_string).unwrap_or_default();
            let mut def = ExpressionDefinition::language(name, text);
            for (k, ov) in opts {
                if k == "expression" || k == "id" || (lang == "language" && k == "language") || ov.is_null() {
                    continue;
                }
                def = def.option(k.clone(), option_string(ov));
            }
            Ok(def)
        }
        Value::Null => Ok(ExpressionDefinition::language(lang, "")),
        Value::Array(_) => Err(dsl_error(format!("invalid '{}' expression: expected a string or a mapping", lang))),
        // `tokenize: ","` is a shorthand for `tokenize: {token: ","}`
        other if lang == "tokenize" => Ok(ExpressionDefinition::language(lang, "").option("token", other.to_string())),
        other => Ok(ExpressionDefinition::language(lang, other.to_string())),
    }
}

// ---------------------------------------------------------------------------
// URIs
// ---------------------------------------------------------------------------

/// Build an endpoint uri from a base uri and a `parameters` map. Values are
/// URL-encoded unless they contain placeholders (`{{..}}`, `${..}`) or are `RAW(..)`.
pub fn build_uri(uri: &str, parameters: Option<&Value>) -> Result<String> {
    let params = match parameters {
        None | Some(Value::Null) => return Ok(uri.to_string()),
        Some(Value::Object(m)) => m,
        Some(other) => return Err(dsl_error(format!("'parameters' must be a mapping but found {}", other.type_name()))),
    };
    let mut out = uri.to_string();
    let mut first = !uri.contains('?');
    for (k, v) in params {
        if v.is_null() {
            continue;
        }
        let s = option_string(v);
        let raw = s.contains("{{") || s.contains("${") || s.starts_with("RAW(") || s.starts_with("RAW{");
        out.push(if first { '?' } else { '&' });
        first = false;
        out.push_str(k);
        out.push('=');
        if raw {
            out.push_str(&s);
        } else {
            out.push_str(&url_encode(&s));
        }
    }
    Ok(out)
}

/// Endpoint uri from a string or a `{uri, parameters}` mapping.
pub fn endpoint_uri(cfg: &Value) -> Result<String> {
    match cfg {
        Value::String(s) => Ok(s.clone()),
        Value::Object(m) => {
            let uri = m.get("uri").and_then(scalar_string).ok_or_else(|| dsl_error("missing required option 'uri'"))?;
            build_uri(&uri, m.get("parameters"))
        }
        other => Err(dsl_error(format!("expected an endpoint uri but found {}", other.type_name()))),
    }
}

// ---------------------------------------------------------------------------
// Core step handlers
// ---------------------------------------------------------------------------

fn pattern_of(ctx: &StepContext<'_>, cfg: &Value) -> Result<Option<ExchangePattern>> {
    match ctx.opt_str(cfg, "pattern") {
        Some(p) => ExchangePattern::parse(&p).map(Some).map_err(|e| ctx.error(e.message)),
        None => Ok(None),
    }
}

fn to_handler(default_pattern: Option<ExchangePattern>) -> impl Fn(&mut StepContext<'_>, &Value) -> Result<()> + Send + Sync {
    move |ctx, cfg| {
        let uri = ctx.uri(cfg)?;
        let pattern = pattern_of(ctx, cfg)?.or(default_pattern);
        ctx.add(Box::new(ToDefinition { id: None, uri, pattern }));
        Ok(())
    }
}

fn set_handler(kind: &'static str) -> impl Fn(&mut StepContext<'_>, &Value) -> Result<()> + Send + Sync {
    move |ctx, cfg| {
        let e = ctx.expression(cfg)?;
        let def = match kind {
            "setBody" => SetDefinition::body(e),
            "transform" => SetDefinition::transform(e),
            "setHeader" => SetDefinition::header(ctx.required_str(cfg, &["name", "headerName"])?, e),
            "setProperty" => SetDefinition::property(ctx.required_str(cfg, &["name", "propertyName"])?, e),
            _ => SetDefinition::variable(ctx.required_str(cfg, &["name", "variableName"])?, e),
        };
        ctx.add(Box::new(def));
        Ok(())
    }
}

/// `name` from a string config or a mapping (first matching key).
fn name_of(ctx: &StepContext<'_>, cfg: &Value, keys: &[&str]) -> Result<String> {
    match cfg {
        Value::String(s) => Ok(s.clone()),
        _ => ctx.required_str(cfg, keys),
    }
}

fn excludes_of(ctx: &StepContext<'_>, cfg: &Value) -> Vec<String> {
    match cfg.get("excludePattern") {
        Some(Value::Array(a)) => a.iter().filter_map(scalar_string).collect(),
        _ => ctx
            .opt_str(cfg, "excludePattern")
            .map(|s| s.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect())
            .unwrap_or_default(),
    }
}

fn register_core_steps(r: &mut StepRegistry) {
    eips::register_eip_steps(r);
    eips_extra::register_extra_steps(r);
    r.register("to", to_handler(None));
    r.register("inOnly", to_handler(Some(ExchangePattern::InOnly)));
    r.register("inOut", to_handler(Some(ExchangePattern::InOut)));
    r.register("toD", |ctx, cfg| {
        let uri = ctx.uri(cfg)?;
        let pattern = pattern_of(ctx, cfg)?;
        let ignore = ctx.opt_bool(cfg, "ignoreInvalidEndpoint")?.unwrap_or(false);
        ctx.add(Box::new(ToDynamicDefinition {
            id: None,
            uri: ExpressionDefinition::language("simple", uri),
            pattern,
            ignore_invalid_endpoint: ignore,
        }));
        Ok(())
    });
    r.register("setBody", set_handler("setBody"));
    r.register("transform", set_handler("transform"));
    r.register("setHeader", set_handler("setHeader"));
    r.register("setProperty", set_handler("setProperty"));
    r.register("setVariable", set_handler("setVariable"));
    r.register("setHeaders", |ctx, cfg| {
        let list = cfg.get("headers").ok_or_else(|| ctx.error("missing required option 'headers'"))?;
        let items = list.as_array().ok_or_else(|| ctx.error("'headers' must be a list"))?;
        for h in items {
            let name = ctx.required_str(h, &["name"])?;
            let e = ctx.expression(h)?;
            ctx.add(Box::new(SetDefinition::header(name, e)));
        }
        Ok(())
    });
    r.register("setVariables", |ctx, cfg| {
        let list = cfg.get("variables").ok_or_else(|| ctx.error("missing required option 'variables'"))?;
        let items = list.as_array().ok_or_else(|| ctx.error("'variables' must be a list"))?;
        for h in items {
            let name = ctx.required_str(h, &["name"])?;
            let e = ctx.expression(h)?;
            ctx.add(Box::new(SetDefinition::variable(name, e)));
        }
        Ok(())
    });
    r.register("removeHeader", |ctx, cfg| {
        let name = name_of(ctx, cfg, &["name", "headerName"])?;
        ctx.add(Box::new(RemoveDefinition { id: None, target: RemoveTarget::Header(name) }));
        Ok(())
    });
    r.register("removeHeaders", |ctx, cfg| {
        let pattern = name_of(ctx, cfg, &["pattern"])?;
        let excludes = excludes_of(ctx, cfg);
        ctx.add(Box::new(RemoveDefinition { id: None, target: RemoveTarget::Headers { pattern, excludes } }));
        Ok(())
    });
    r.register("removeProperty", |ctx, cfg| {
        let name = name_of(ctx, cfg, &["name", "propertyName"])?;
        ctx.add(Box::new(RemoveDefinition { id: None, target: RemoveTarget::Property(name) }));
        Ok(())
    });
    r.register("removeProperties", |ctx, cfg| {
        let pattern = name_of(ctx, cfg, &["pattern"])?;
        let excludes = excludes_of(ctx, cfg);
        ctx.add(Box::new(RemoveDefinition { id: None, target: RemoveTarget::Properties { pattern, excludes } }));
        Ok(())
    });
    r.register("removeVariable", |ctx, cfg| {
        let name = name_of(ctx, cfg, &["name", "variableName"])?;
        ctx.add(Box::new(RemoveDefinition { id: None, target: RemoveTarget::Variable(name) }));
        Ok(())
    });
    r.register("convertBodyTo", |ctx, cfg| {
        let type_name = name_of(ctx, cfg, &["type"])?;
        let mandatory = ctx.opt_bool(cfg, "mandatory")?.unwrap_or(false);
        ctx.add(Box::new(ConvertDefinition { id: None, type_name, header: None, mandatory }));
        Ok(())
    });
    r.register("convertHeaderTo", |ctx, cfg| {
        let header = ctx.required_str(cfg, &["name", "headerName"])?;
        let type_name = ctx.required_str(cfg, &["type"])?;
        let mandatory = ctx.opt_bool(cfg, "mandatory")?.unwrap_or(false);
        ctx.add(Box::new(ConvertDefinition { id: None, type_name, header: Some(header), mandatory }));
        Ok(())
    });
    r.register("log", |ctx, cfg| {
        let (message, level, log_name, marker) = match cfg {
            Value::String(s) => (s.clone(), Level::Info, None, None),
            _ => {
                let message = ctx.required_str(cfg, &["message"])?;
                let level = match ctx.opt_str(cfg, "loggingLevel") {
                    Some(l) => Level::parse(&l).ok_or_else(|| ctx.error(format!("invalid loggingLevel '{}'", l)))?,
                    None => Level::Info,
                };
                (message, level, ctx.opt_str(cfg, "logName"), ctx.opt_str(cfg, "marker"))
            }
        };
        ctx.add(Box::new(LogDefinition { id: None, message: ExpressionDefinition::language("simple", message), level, log_name, marker }));
        Ok(())
    });
    r.register("process", |ctx, cfg| {
        let reference = name_of(ctx, cfg, &["ref"])?;
        ctx.add(Box::new(ProcessDefinition { id: None, processor: None, reference: Some(reference) }));
        Ok(())
    });
    r.register("stop", |ctx, _cfg| {
        ctx.add(Box::new(StopDefinition { id: None }));
        Ok(())
    });
    r.register("setExchangePattern", |ctx, cfg| {
        let p = name_of(ctx, cfg, &["pattern"])?;
        let pattern = ExchangePattern::parse(&p).map_err(|e| ctx.error(e.message))?;
        ctx.add(Box::new(SetExchangePatternDefinition { id: None, pattern }));
        Ok(())
    });
    r.register("step", |ctx, cfg| {
        ctx.add_block(Box::new(StepDefinition { id: None, outputs: Default::default() }), cfg.get("steps"))
    });
}

// ---------------------------------------------------------------------------
// Beans, templates and stored configuration
// ---------------------------------------------------------------------------

/// A bean declared in a `beans:` block.
#[derive(Clone, Debug)]
pub struct BeanDefinition {
    pub name: String,
    /// The bean `type` (a Java class name in Camel).
    pub type_name: Option<String>,
    /// The `properties` mapping.
    pub properties: ObjectMap,
    /// The full raw definition.
    pub raw: Value,
    /// Whether a bean factory created the bean.
    pub resolved: bool,
}

/// A parameter of a route template.
#[derive(Clone, Debug, PartialEq)]
pub struct TemplateParameter {
    pub name: String,
    pub default_value: Option<String>,
    pub required: bool,
    pub description: Option<String>,
}

/// A parsed `routeTemplate`.
#[derive(Clone, Debug)]
pub struct RouteTemplateDefinition {
    pub id: String,
    pub description: Option<String>,
    pub parameters: Vec<TemplateParameter>,
    /// The route-shaped template body (`{from: {...}, ...}`).
    pub route: Value,
    /// Template local `beans` (created with the bean factories when the template is
    /// instantiated, after parameter substitution).
    pub beans: Vec<Value>,
}

impl RouteTemplateDefinition {
    /// Parse the value of a `routeTemplate` element.
    pub fn parse(v: &Value) -> Result<Self> {
        let id = v.get("id").and_then(scalar_string).ok_or_else(|| dsl_error("routeTemplate requires an 'id'"))?;
        let mut parameters = Vec::new();
        match v.get("parameters") {
            None | Some(Value::Null) => {}
            Some(Value::Array(items)) => {
                for p in items {
                    let name = p
                        .get("name")
                        .and_then(scalar_string)
                        .ok_or_else(|| dsl_error(format!("routeTemplate '{}': every parameter requires a 'name'", id)))?;
                    let default_value = p.get("defaultValue").and_then(scalar_string);
                    let required = match p.get("required") {
                        Some(r) => r.as_bool().unwrap_or(true),
                        None => default_value.is_none(),
                    };
                    parameters.push(TemplateParameter { name, default_value, required, description: p.get("description").and_then(scalar_string) });
                }
            }
            Some(_) => return Err(dsl_error(format!("routeTemplate '{}': 'parameters' must be a list", id))),
        }
        let route = if let Some(r) = v.get("route") {
            r.clone()
        } else if let Some(f) = v.get("from") {
            Value::object([("from", f.clone())])
        } else {
            return Err(dsl_error(format!("routeTemplate '{}' requires a 'route' or 'from'", id)));
        };
        let beans = match v.get("beans") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(a)) => a.clone(),
            Some(_) => return Err(dsl_error(format!("routeTemplate '{}': 'beans' must be a list", id))),
        };
        Ok(RouteTemplateDefinition { id, description: v.get("description").and_then(scalar_string), parameters, route, beans })
    }

    /// Parameter values for an instantiation: declared defaults overridden by `params`.
    /// Fails if a required parameter has no value.
    pub fn resolve_parameters(&self, params: &BTreeMap<String, String>) -> Result<BTreeMap<String, String>> {
        let missing: Vec<&str> = self
            .parameters
            .iter()
            .filter(|p| p.required && p.default_value.is_none() && !params.contains_key(&p.name))
            .map(|p| p.name.as_str())
            .collect();
        if !missing.is_empty() {
            return Err(dsl_error(format!("Route template {} the following mandatory parameters are missing: {}", self.id, missing.join(", "))));
        }
        let mut values: BTreeMap<String, String> = BTreeMap::new();
        for p in &self.parameters {
            if let Some(d) = &p.default_value {
                values.insert(p.name.clone(), d.clone());
            }
        }
        for (k, v) in params {
            values.insert(k.clone(), v.clone());
        }
        Ok(values)
    }

    /// Substitute `{{param}}` placeholders and return the route-shaped value.
    /// `{{param:default}}` uses the inline default when the parameter has no value.
    pub fn instantiate(&self, params: &BTreeMap<String, String>) -> Result<Value> {
        let values = self.resolve_parameters(params)?;
        let declared: Vec<&str> = self.parameters.iter().map(|p| p.name.as_str()).collect();
        Ok(substitute_declared(&self.route, &values, &declared))
    }
}

/// Replace `{{name}}` (and `{{name:default}}`) placeholders whose name is in `params`
/// in all string scalars. Other placeholders are left for the properties component.
pub fn substitute(v: &Value, params: &BTreeMap<String, String>) -> Value {
    substitute_declared(v, params, &[])
}

/// Like [`substitute`]; additionally `{{name:default}}` of a `declared` parameter
/// without value is replaced by its inline default.
fn substitute_declared(v: &Value, params: &BTreeMap<String, String>, declared: &[&str]) -> Value {
    match v {
        Value::String(s) if s.contains("{{") => Value::String(substitute_text(s, params, declared)),
        Value::Array(a) => Value::Array(a.iter().map(|x| substitute_declared(x, params, declared)).collect()),
        Value::Object(m) => Value::Object(m.iter().map(|(k, x)| (k.clone(), substitute_declared(x, params, declared))).collect()),
        other => other.clone(),
    }
}

fn substitute_text(s: &str, params: &BTreeMap<String, String>, declared: &[&str]) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("{{") {
        let Some(len) = rest[start + 2..].find("}}") else { break };
        let inner = &rest[start + 2..start + 2 + len];
        let (name, default) = match inner.split_once(':') {
            Some((n, d)) => (n.trim(), Some(d)),
            None => (inner.trim(), None),
        };
        out.push_str(&rest[..start]);
        match (params.get(name), default) {
            (Some(v), _) => out.push_str(v),
            (None, Some(d)) if declared.contains(&name) => out.push_str(d),
            _ => out.push_str(&rest[start..start + 4 + len]),
        }
        rest = &rest[start + 4 + len..];
    }
    out.push_str(rest);
    out
}

/// Prefix the `id` of every step below `v` (templatedRoute `prefixId`).
fn prefix_step_ids(v: &mut Value, prefix: &str) {
    match v {
        Value::Object(m) => {
            for (k, x) in m.iter_mut() {
                if k == "id" {
                    if let Value::String(s) = x {
                        *s = format!("{}{}", prefix, s);
                    }
                } else {
                    prefix_step_ids(x, prefix);
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| prefix_step_ids(x, prefix)),
        _ => {}
    }
}

#[derive(Default)]
struct StateInner {
    beans: Vec<BeanDefinition>,
    templates: BTreeMap<String, RouteTemplateDefinition>,
    configurations: Vec<(String, Value)>,
}

/// Per-context state of the YAML loader: declared beans, route templates and raw
/// configuration elements (`routeConfiguration`, `rest`, ...). Obtain it with [`yaml_state`].
#[derive(Default)]
pub struct YamlState {
    inner: Mutex<StateInner>,
}

/// Registry name under which the [`YamlState`] is bound.
pub const STATE_BEAN_NAME: &str = "camel-yaml.state";

/// Get (or create) the YAML state of a context.
pub fn yaml_state(ctx: &CamelContext) -> Arc<YamlState> {
    static CREATE: Mutex<()> = Mutex::new(());
    let _g = CREATE.lock().unwrap();
    if let Some(s) = ctx.registry().lookup::<YamlState>(STATE_BEAN_NAME) {
        return s;
    }
    let s: Arc<YamlState> = Arc::new(YamlState::default());
    ctx.registry().bind_arc(STATE_BEAN_NAME, s.clone() as Arc<dyn Any + Send + Sync>);
    s
}

impl YamlState {
    /// All declared beans.
    pub fn beans(&self) -> Vec<BeanDefinition> {
        self.inner.lock().unwrap().beans.clone()
    }

    pub fn bean(&self, name: &str) -> Option<BeanDefinition> {
        self.inner.lock().unwrap().beans.iter().find(|b| b.name == name).cloned()
    }

    /// All route templates by id.
    pub fn route_templates(&self) -> BTreeMap<String, RouteTemplateDefinition> {
        self.inner.lock().unwrap().templates.clone()
    }

    pub fn route_template(&self, id: &str) -> Option<RouteTemplateDefinition> {
        self.inner.lock().unwrap().templates.get(id).cloned()
    }

    /// Add (or replace) a route template.
    pub fn add_route_template(&self, t: RouteTemplateDefinition) {
        self.inner.lock().unwrap().templates.insert(t.id.clone(), t);
    }

    /// Raw configuration elements of the given kind (e.g. `routeConfiguration`, `rest`).
    pub fn configurations(&self, kind: &str) -> Vec<Value> {
        self.inner.lock().unwrap().configurations.iter().filter(|(k, _)| k == kind).map(|(_, v)| v.clone()).collect()
    }

    /// All raw configuration elements as (kind, value) in load order.
    pub fn all_configurations(&self) -> Vec<(String, Value)> {
        self.inner.lock().unwrap().configurations.clone()
    }

    pub fn route_configurations(&self) -> Vec<Value> {
        self.configurations("routeConfiguration")
    }
    pub fn rest_configurations(&self) -> Vec<Value> {
        self.configurations("restConfiguration")
    }
    pub fn rests(&self) -> Vec<Value> {
        self.configurations("rest")
    }
    pub fn error_handlers(&self) -> Vec<Value> {
        self.configurations("errorHandler")
    }

    fn add_bean(&self, b: BeanDefinition) {
        let mut g = self.inner.lock().unwrap();
        g.beans.retain(|x| x.name != b.name);
        g.beans.push(b);
    }

    fn add_configuration(&self, kind: &str, v: Value) {
        self.inner.lock().unwrap().configurations.push((kind.to_string(), v));
    }
}

// ---------------------------------------------------------------------------
// Top-level extension context
// ---------------------------------------------------------------------------

/// Context passed to top-level handlers ([`register_top_level_handler`]).
pub struct TopLevelContext<'a> {
    camel: &'a CamelContext,
    loader: &'a YamlLoader,
    source: &'a Source<'a>,
    path: Vec<PathSegment>,
    kind: String,
    routes: &'a mut Vec<RouteDefinition>,
}

impl<'a> TopLevelContext<'a> {
    pub fn camel(&self) -> &CamelContext {
        self.camel
    }
    pub fn loader(&self) -> &YamlLoader {
        self.loader
    }
    /// The element name (e.g. `rest`).
    pub fn kind(&self) -> &str {
        &self.kind
    }
    pub fn location(&self) -> String {
        self.source.location(&self.path)
    }
    pub fn error(&self, message: impl std::fmt::Display) -> CamelError {
        dsl_error(format!("Error in '{}' {}: {}", self.kind, self.location(), message))
    }
    /// All top-level elements of the given kind in the document being loaded (in
    /// document order), e.g. every `restConfiguration` of the file, including ones
    /// that come after the element being handled.
    pub fn document_elements(&self, kind: &str) -> Vec<&'a Value> {
        let doc: &'a Value = self.source.value;
        match doc {
            Value::Array(items) => items
                .iter()
                .filter_map(|it| match it {
                    Value::Object(m) if m.len() == 1 => m.get(kind),
                    _ => None,
                })
                .collect(),
            Value::Object(m) if m.len() == 1 => m.get(kind).into_iter().collect(),
            _ => Vec::new(),
        }
    }

    /// Add a generated route.
    pub fn add_route(&mut self, route: RouteDefinition) {
        self.routes.push(route);
    }
    /// Build a route from a route-shaped value (`{id, from: {uri, steps}}`), typically a
    /// value nested in the element (positions are then reported precisely).
    pub fn build_route(&self, route: &Value) -> Result<RouteDefinition> {
        let path = self.source.find_path(&self.path, route).unwrap_or_else(|| self.path.clone());
        self.loader.build_route_at(self.camel, self.source, route, path)
    }
    /// Create a route from `uri` and load `steps` into it.
    pub fn build_route_from(&self, uri: &str, steps: &Value) -> Result<RouteDefinition> {
        let mut def = RouteDefinition::new(uri);
        let path = self.source.find_path(&self.path, steps).map(|mut p| {
            p.pop();
            p
        });
        let path = path.unwrap_or_else(|| self.path.clone());
        self.loader.load_steps_at(self.camel, self.source, &mut def, steps, path)?;
        Ok(def)
    }
}

// ---------------------------------------------------------------------------
// Loader
// ---------------------------------------------------------------------------

/// The YAML DSL loader. [`YamlLoader::new`] includes the built-in and globally
/// registered step handlers, top-level handlers and bean factories.
#[derive(Clone)]
pub struct YamlLoader {
    pub steps: StepRegistry,
    top_level: BTreeMap<String, TopLevelHandler>,
    bean_factories: BTreeMap<String, BeanFactory>,
}

impl Default for YamlLoader {
    fn default() -> Self {
        YamlLoader::new()
    }
}

impl YamlLoader {
    pub fn new() -> Self {
        YamlLoader {
            steps: StepRegistry::with_defaults(),
            top_level: global_top_level().lock().unwrap().clone(),
            bean_factories: global_bean_factories().lock().unwrap().clone(),
        }
    }

    /// A loader with a custom step registry.
    pub fn with_steps(steps: StepRegistry) -> Self {
        YamlLoader { steps, ..YamlLoader::new() }
    }

    /// Register a step handler on this loader.
    pub fn register_step<F>(&mut self, name: impl Into<String>, handler: F) -> &mut Self
    where
        F: Fn(&mut StepContext<'_>, &Value) -> Result<()> + Send + Sync + 'static,
    {
        self.steps.register(name, handler);
        self
    }

    /// Register a top-level element handler on this loader.
    pub fn register_top_level<F>(&mut self, name: impl Into<String>, handler: F) -> &mut Self
    where
        F: Fn(&mut TopLevelContext<'_>, &Value) -> Result<()> + Send + Sync + 'static,
    {
        self.top_level.insert(name.into(), Arc::new(handler));
        self
    }

    /// Register a bean factory on this loader.
    pub fn register_bean_factory<F>(&mut self, type_name: impl Into<String>, factory: F) -> &mut Self
    where
        F: Fn(&CamelContext, &BeanDefinition) -> Result<()> + Send + Sync + 'static,
    {
        self.bean_factories.insert(type_name.into(), Arc::new(factory));
        self
    }

    /// Parse YAML (possibly multi-document) and return the route definitions. Side
    /// effects (beans, templates, stored configuration) are applied to `ctx`.
    pub fn load_routes(&self, ctx: &CamelContext, text: &str) -> Result<Vec<RouteDefinition>> {
        self.load_named(ctx, text, None)
    }

    fn load_named(&self, ctx: &CamelContext, text: &str, name: Option<&str>) -> Result<Vec<RouteDefinition>> {
        let nodes = yaml::parse_nodes(text).map_err(|e| {
            let mut ce: CamelError = e.into();
            if let Some(n) = name {
                ce.message = format!("{} in {}", ce.message, n);
            }
            ce
        })?;
        let multi = nodes.len() > 1;
        let mut routes = Vec::new();
        for (i, node) in nodes.iter().enumerate() {
            let value = node.to_value().map_err(CamelError::from)?;
            let source = Source { name, doc_index: if multi { Some(i) } else { None }, value: &value, node: Some(node) };
            routes.extend(self.load_document(ctx, &source)?);
        }
        self.apply_route_configurations(ctx, &mut routes)?;
        Ok(routes)
    }

    /// Load routes from an already parsed value (a list of top-level elements).
    pub fn load_routes_from_value(&self, ctx: &CamelContext, value: &Value) -> Result<Vec<RouteDefinition>> {
        let mut routes = self.load_document(ctx, &Source::synthetic(value))?;
        self.apply_route_configurations(ctx, &mut routes)?;
        Ok(routes)
    }

    /// Link route definitions built outside of [`YamlLoader::load_routes`] (e.g. with
    /// [`YamlLoader::build_route`]) to the `routeConfiguration`s loaded into `ctx`.
    pub fn apply_route_configurations_to(&self, ctx: &CamelContext, routes: &mut [RouteDefinition]) -> Result<()> {
        self.apply_route_configurations(ctx, routes)
    }

    /// Load routes from a file (`.xml` files are loaded with the XML DSL).
    pub fn load_routes_from_file(&self, ctx: &CamelContext, path: impl AsRef<Path>) -> Result<Vec<RouteDefinition>> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| CamelError::io(format!("Cannot read {}: {}", path.display(), e)))?;
        let name = path.display().to_string();
        if path.extension().map(|e| e.eq_ignore_ascii_case("xml")).unwrap_or(false) {
            return crate::xml::load_routes_with(self, ctx, &text);
        }
        self.load_named(ctx, &text, Some(&name))
    }

    /// Load and add routes to the context.
    pub fn add_routes(&self, ctx: &CamelContext, text: &str) -> Result<()> {
        for r in self.load_routes(ctx, text)? {
            ctx.add_route_definition(r)?;
        }
        Ok(())
    }

    /// Build a route from a route-shaped value (`{id, from: {uri, steps}}`).
    pub fn build_route(&self, ctx: &CamelContext, route: &Value) -> Result<RouteDefinition> {
        self.build_route_at(ctx, &Source::synthetic(route), route, Vec::new())
    }

    /// Load `steps` into an existing route definition.
    pub fn build_steps(&self, ctx: &CamelContext, route: &mut RouteDefinition, steps: &Value) -> Result<()> {
        self.load_steps_at(ctx, &Source::synthetic(steps), route, steps, Vec::new())
    }

    fn load_steps_at(&self, camel: &CamelContext, source: &Source<'_>, route: &mut RouteDefinition, steps: &Value, path: Vec<PathSegment>) -> Result<()> {
        let mut sc = StepContext { route, camel, loader: self, source, path, step_name: "from".into(), pending_id: None, dangling: 0 };
        sc.load_steps(steps)
    }

    fn load_document(&self, ctx: &CamelContext, source: &Source<'_>) -> Result<Vec<RouteDefinition>> {
        let doc = source.value;
        // `kamelet:` endpoints may be used by any YAML route
        crate::kamelet::register_kamelet_component(ctx);
        // Kamelet / Pipe / KameletBinding resources
        if let Some(routes) = crate::kamelet::load_resource_document(self, ctx, doc)
            .map_err(|e| if e.kind == YAML_DSL_ERROR { dsl_error(format!("{} {}", e.message, source.location(&[]))) } else { e })?
        {
            return Ok(routes);
        }
        let single;
        let items: &[Value] = match doc {
            Value::Null => return Ok(Vec::new()),
            Value::Array(a) => a,
            Value::Object(_) => {
                single = [doc.clone()];
                &single
            }
            other => return Err(dsl_error(format!("Expected a list of routes but found {} {}", other.type_name(), source.location(&[])))),
        };
        let is_array = matches!(doc, Value::Array(_));
        let path_of = |i: usize| if is_array { vec![PathSegment::Index(i)] } else { Vec::new() };
        let mut entries: Vec<(usize, &str, &Value)> = Vec::new();
        for (i, item) in items.iter().enumerate() {
            match item {
                Value::Object(m) if m.len() == 1 => {
                    let (k, v) = m.iter().next().unwrap();
                    entries.push((i, k.as_str(), v));
                }
                Value::Object(m) if m.contains_key("from") && !m.contains_key("route") => {
                    // a route given without the `route:` wrapper
                    entries.push((i, "", item));
                }
                Value::Object(m) => {
                    let keys: Vec<&str> = m.keys().map(|k| k.as_str()).collect();
                    return Err(dsl_error(format!(
                        "Each top-level element must have exactly one key but found [{}] {}",
                        keys.join(", "),
                        source.location(&path_of(i))
                    )));
                }
                other => {
                    return Err(dsl_error(format!(
                        "Invalid top-level element: expected a mapping but found {} {}",
                        other.type_name(),
                        source.location(&path_of(i))
                    )))
                }
            }
        }
        let state = yaml_state(ctx);
        // pass 1: beans and templates
        for (i, kind, v) in &entries {
            let mut path = path_of(*i);
            path.push(PathSegment::Key(kind.to_string()));
            match *kind {
                "beans" => self.load_beans(ctx, &state, source, v, &path)?,
                "routeTemplate" | "routeTemplates" => {
                    let list: Vec<&Value> = match v {
                        Value::Array(a) => a.iter().map(|x| x.get("routeTemplate").unwrap_or(x)).collect(),
                        _ => vec![*v],
                    };
                    for t in list {
                        let t = RouteTemplateDefinition::parse(t).map_err(|e| dsl_error(format!("{} {}", e.message, source.location(&path))))?;
                        state.add_route_template(t);
                    }
                }
                _ => {}
            }
        }
        // pass 2: everything else, in order
        let mut routes = Vec::new();
        for (i, kind, v) in &entries {
            let mut path = path_of(*i);
            if !kind.is_empty() {
                path.push(PathSegment::Key(kind.to_string()));
            }
            match *kind {
                "beans" | "routeTemplate" | "routeTemplates" => {}
                "route" | "" => routes.push(self.build_route_at(ctx, source, v, path)?),
                "from" => {
                    let mut p = path.clone();
                    p.pop();
                    let wrapper = &items[*i];
                    routes.push(self.build_route_at(ctx, source, wrapper, p)?);
                }
                "templatedRoute" | "templatedRoutes" => {
                    let list: Vec<&Value> = match v {
                        Value::Array(a) => a.iter().map(|x| x.get("templatedRoute").unwrap_or(x)).collect(),
                        _ => vec![*v],
                    };
                    for t in list {
                        routes.push(self.instantiate_templated_route(ctx, &state, t).map_err(|e| {
                            if e.kind == YAML_DSL_ERROR && e.message.contains(" at ") {
                                e
                            } else {
                                dsl_error(format!("{} {}", e.message, source.location(&path)))
                            }
                        })?);
                    }
                }
                other => {
                    let known = STORED_TOP_LEVEL.contains(&other);
                    let handler = self.top_level.get(other).cloned();
                    if !known && handler.is_none() {
                        let mut supported: Vec<&str> = vec!["route", "from", "beans", "routeTemplate", "templatedRoute"];
                        supported.extend(STORED_TOP_LEVEL.iter());
                        let extra: Vec<&str> = self.top_level.keys().map(|s| s.as_str()).collect();
                        supported.extend(extra);
                        return Err(dsl_error(format!(
                            "Unknown top-level element '{}' {}. Supported elements: {}",
                            other,
                            source.location(&path),
                            supported.join(", ")
                        )));
                    }
                    state.add_configuration(other, (*v).clone());
                    match other {
                        "routeConfiguration" => {
                            // validate eagerly (errors get precise locations); applied to the
                            // routes when the whole input has been loaded
                            self.build_route_configuration(ctx, source, v, path.clone())?;
                        }
                        "errorHandler" => {
                            let eh = parse_error_handler(ctx, v)
                                .map_err(|e| dsl_error(format!("Invalid errorHandler {}: {}", source.location(&path), e.message)))?;
                            ctx.set_error_handler(eh);
                        }
                        item if ROUTE_CONFIGURATION_ITEMS.contains(&item) => {
                            let wrapped = Value::object([(item, (*v).clone())]);
                            self.build_route_configuration(ctx, source, &wrapped, path_of(*i))?;
                        }
                        _ => {}
                    }
                    if let Some(h) = handler {
                        let mut tl = TopLevelContext { camel: ctx, loader: self, source, path: path.clone(), kind: other.to_string(), routes: &mut routes };
                        h(&mut tl, v).map_err(|e| {
                            if e.kind == YAML_DSL_ERROR {
                                e
                            } else {
                                dsl_error(format!("Error in '{}' {}: {}", other, source.location(&path), e.message)).with_cause(e)
                            }
                        })?;
                    }
                }
            }
        }
        Ok(routes)
    }

    /// Create the beans of a `beans` list (with the registered bean factories) and record them
    /// in the YAML state. `label` names the list in error messages.
    pub fn load_bean_list(&self, ctx: &CamelContext, beans: &Value, label: &str) -> Result<()> {
        let state = yaml_state(ctx);
        self.load_beans(ctx, &state, &Source::synthetic(beans), beans, &[PathSegment::Key(label.to_string())])
    }

    fn load_beans(&self, ctx: &CamelContext, state: &YamlState, source: &Source<'_>, v: &Value, path: &[PathSegment]) -> Result<()> {
        let items = match v {
            Value::Array(a) => a,
            Value::Null => return Ok(()),
            other => return Err(dsl_error(format!("'beans' must be a list but found {} {}", other.type_name(), source.location(path)))),
        };
        for (i, b) in items.iter().enumerate() {
            let mut p = path.to_vec();
            p.push(PathSegment::Index(i));
            let name = b.get("name").and_then(scalar_string).ok_or_else(|| dsl_error(format!("bean requires a 'name' {}", source.location(&p))))?;
            let type_name = b.get("type").and_then(scalar_string).map(|t| t.trim_start_matches("#class:").to_string());
            let properties = b.get("properties").and_then(|x| x.as_object()).cloned().unwrap_or_default();
            let mut def = BeanDefinition { name: name.clone(), type_name: type_name.clone(), properties, raw: b.clone(), resolved: false };
            let factory = type_name.as_ref().and_then(|t| self.bean_factories.get(t)).cloned();
            match factory {
                Some(f) => {
                    f(ctx, &def).map_err(|e| dsl_error(format!("Failed to create bean '{}' {}: {}", name, source.location(&p), e.message)).with_cause(e))?;
                    def.resolved = true;
                }
                None => camel_warn!(
                    "camel-yaml",
                    "No bean factory registered for bean '{}' of type {:?}; the bean definition is recorded but not instantiated",
                    name,
                    type_name
                ),
            }
            state.add_bean(def);
        }
        Ok(())
    }

    fn instantiate_templated_route(&self, ctx: &CamelContext, state: &YamlState, v: &Value) -> Result<RouteDefinition> {
        let template_id = v
            .get("routeTemplateRef")
            .and_then(scalar_string)
            .ok_or_else(|| dsl_error("templatedRoute requires 'routeTemplateRef'"))?;
        let mut params = BTreeMap::new();
        match v.get("parameters") {
            None | Some(Value::Null) => {}
            Some(Value::Array(items)) => {
                for p in items {
                    let name = p.get("name").and_then(scalar_string).ok_or_else(|| dsl_error("templatedRoute parameter requires a 'name'"))?;
                    let value = p.get("value").and_then(scalar_string).unwrap_or_default();
                    params.insert(name, value);
                }
            }
            Some(Value::Object(m)) => {
                for (k, val) in m {
                    params.insert(k.clone(), scalar_string(val).unwrap_or_default());
                }
            }
            Some(_) => return Err(dsl_error("templatedRoute 'parameters' must be a list")),
        }
        let route_id = v.get("routeId").and_then(scalar_string);
        let template = state.route_template(&template_id).ok_or_else(|| dsl_error(format!("Cannot find route template with id '{}'", template_id)))?;
        if let Some(beans) = v.get("beans") {
            self.load_beans(ctx, state, &Source::synthetic(beans), beans, &[PathSegment::Key("beans".into())])?;
        }
        let prefix = v.get("prefixId").and_then(scalar_string);
        self.instantiate_template_with_prefix(ctx, &template, route_id.as_deref(), prefix.as_deref(), &params)
    }

    /// Create a route from a template.
    pub fn instantiate_template(
        &self,
        ctx: &CamelContext,
        template: &RouteTemplateDefinition,
        route_id: Option<&str>,
        params: &BTreeMap<String, String>,
    ) -> Result<RouteDefinition> {
        self.instantiate_template_with_prefix(ctx, template, route_id, None, params)
    }

    /// Create a route from a template; `prefix_id` is prepended to the id of every step.
    pub fn instantiate_template_with_prefix(
        &self,
        ctx: &CamelContext,
        template: &RouteTemplateDefinition,
        route_id: Option<&str>,
        prefix_id: Option<&str>,
        params: &BTreeMap<String, String>,
    ) -> Result<RouteDefinition> {
        let mut value = template.instantiate(params)?;
        if !template.beans.is_empty() {
            let values = template.resolve_parameters(params)?;
            let declared: Vec<&str> = template.parameters.iter().map(|p| p.name.as_str()).collect();
            let beans = substitute_declared(&Value::Array(template.beans.clone()), &values, &declared);
            let state = yaml_state(ctx);
            self.load_beans(ctx, &state, &Source::synthetic(&beans), &beans, &[PathSegment::Key(format!("routeTemplate[{}].beans", template.id))])?;
        }
        if let Some(prefix) = prefix_id {
            if let Value::Object(m) = &mut value {
                for (k, x) in m.iter_mut() {
                    if k == "from" || k == "steps" {
                        prefix_step_ids(x, prefix);
                    }
                }
            }
        }
        let source = Source::synthetic(&value);
        let mut def = self
            .build_route_at(ctx, &source, &value, vec![PathSegment::Key(format!("routeTemplate[{}]", template.id))])?;
        if let Some(id) = route_id {
            def.id = Some(id.to_string());
        }
        def.properties.insert("routeTemplateId".into(), template.id.clone());
        if let Some(p) = prefix_id {
            def.properties.insert("prefixId".into(), p.to_string());
        }
        Ok(def)
    }

    fn build_route_at(&self, ctx: &CamelContext, source: &Source<'_>, v: &Value, path: Vec<PathSegment>) -> Result<RouteDefinition> {
        let map = v.as_object().ok_or_else(|| dsl_error(format!("A route must be a mapping but found {} {}", v.type_name(), source.location(&path))))?;
        let from = map.get("from").ok_or_else(|| dsl_error(format!("A route requires 'from' {}", source.location(&path))))?;
        let mut from_path = path.clone();
        from_path.push(PathSegment::Key("from".into()));
        let uri = endpoint_uri(from).map_err(|e| dsl_error(format!("Invalid 'from' {}: {}", source.location(&from_path), e.message)))?;
        let mut def = RouteDefinition::new(uri);
        for (k, val) in map {
            let mut kp = path.clone();
            kp.push(PathSegment::Key(k.clone()));
            let bad = |msg: String| dsl_error(format!("Invalid route option '{}' {}: {}", k, source.location(&kp), msg));
            match k.as_str() {
                "from" | "steps" => {}
                "id" | "routeId" => def.id = scalar_string(val),
                "description" => def.description = scalar_string(val),
                "group" => def.group = scalar_string(val),
                "autoStartup" => def.auto_startup = resolve_bool(ctx, val).map_err(|e| bad(e.message))?,
                "startupOrder" => {
                    def.startup_order = Some(val.as_i64().ok_or_else(|| bad(format!("expected an integer but found {}", val)))? as i32)
                }
                "errorHandler" => {
                    let eh = parse_error_handler(ctx, val).map_err(|e| bad(e.message))?;
                    def.error_handler(eh);
                }
                "errorHandlerRef" => {
                    let eh = parse_error_handler(ctx, &Value::object([("refErrorHandler", val.clone())])).map_err(|e| bad(e.message))?;
                    def.error_handler(eh);
                }
                "routePolicy" | "routePolicyRef" => {
                    let names: Vec<String> = match val {
                        Value::Array(a) => a.iter().filter_map(scalar_string).collect(),
                        other => scalar_string(other).map(|s| s.split(',').map(|p| p.trim().to_string()).collect()).unwrap_or_default(),
                    };
                    for n in names.iter().map(|n| ref_name(n)).filter(|n| !n.is_empty()) {
                        if ctx.registry().lookup::<Arc<dyn camel_core::processor::errors::RoutePolicy>>(&n).is_none() {
                            return Err(bad(format!("No RoutePolicy named '{}' found in the registry", n)));
                        }
                        def.route_policy_ref(n);
                    }
                }
                _ => {
                    let s = match val {
                        Value::Array(_) | Value::Object(_) => val.to_json(),
                        other => other.to_string(),
                    };
                    def.properties.insert(k.clone(), s);
                }
            }
        }
        def.extensions.insert(ROUTE_YAML_EXTENSION.to_string(), Box::new(v.clone()));
        if let Some(steps) = from.get("steps") {
            self.load_steps_at(ctx, source, &mut def, steps, from_path.clone())?;
        }
        if let Some(steps) = map.get("steps") {
            self.load_steps_at(ctx, source, &mut def, steps, path.clone())?;
        }
        Ok(def)
    }
}

// ---------------------------------------------------------------------------
// Convenience functions
// ---------------------------------------------------------------------------

/// Parse YAML DSL text and return the route definitions (beans, templates and stored
/// configuration are applied to `ctx`).
pub fn load_routes(ctx: &CamelContext, yaml: &str) -> Result<Vec<RouteDefinition>> {
    YamlLoader::new().load_routes(ctx, yaml)
}

/// Parse YAML DSL text and add the routes to the context.
pub fn add_routes_from_yaml(ctx: &CamelContext, yaml: &str) -> Result<()> {
    YamlLoader::new().add_routes(ctx, yaml)
}

/// Load route definitions from a YAML (or `.xml`) file.
pub fn load_routes_from_file(ctx: &CamelContext, path: impl AsRef<Path>) -> Result<Vec<RouteDefinition>> {
    YamlLoader::new().load_routes_from_file(ctx, path)
}

/// Load routes from a YAML (or `.xml`) file and add them to the context.
pub fn add_routes_from_file(ctx: &CamelContext, path: impl AsRef<Path>) -> Result<()> {
    for r in load_routes_from_file(ctx, path)? {
        ctx.add_route_definition(r)?;
    }
    Ok(())
}

/// Instantiate a route template previously loaded into `ctx` and add the route.
pub fn add_route_from_template(ctx: &CamelContext, template_id: &str, route_id: Option<&str>, params: &BTreeMap<String, String>) -> Result<()> {
    let template = yaml_state(ctx)
        .route_template(template_id)
        .ok_or_else(|| dsl_error(format!("Cannot find route template with id '{}'", template_id)))?;
    let loader = YamlLoader::new();
    let def = loader.instantiate_template(ctx, &template, route_id, params)?;
    let mut defs = [def];
    loader.apply_route_configurations(ctx, &mut defs)?;
    let [def] = defs;
    ctx.add_route_definition(def)
}

// ---------------------------------------------------------------------------
// Dumping
// ---------------------------------------------------------------------------

fn expression_to_value(e: &ExpressionDefinition) -> Value {
    match e {
        ExpressionDefinition::Language { language, expression, options } => {
            if options.is_empty() {
                Value::object([(language.clone(), Value::String(expression.clone()))])
            } else {
                let mut m: ObjectMap = options.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect();
                if !expression.is_empty() {
                    m.insert("expression".into(), Value::String(expression.clone()));
                }
                Value::object([(language.clone(), Value::Object(m))])
            }
        }
        _ => Value::object([("expression", Value::String("<rust expression>".into()))]),
    }
}

fn merge(mut a: ObjectMap, b: Value) -> ObjectMap {
    if let Value::Object(m) = b {
        a.extend(m);
    }
    a
}

/// Convert a processor definition back to its YAML step shape (best effort; unknown
/// EIPs are rendered by name with their label and children).
pub fn step_to_value(d: &dyn ProcessorDefinition) -> Value {
    let any = d.as_any();
    let mut cfg: ObjectMap = ObjectMap::new();
    if let Some(id) = d.id() {
        cfg.insert("id".into(), Value::String(id.to_string()));
    }
    let name: String = d.name().to_string();
    if let Some(t) = any.downcast_ref::<ToDefinition>() {
        cfg.insert("uri".into(), Value::String(t.uri.clone()));
        if let Some(p) = t.pattern {
            cfg.insert("pattern".into(), Value::String(format!("{:?}", p)));
        }
    } else if let Some(t) = any.downcast_ref::<ToDynamicDefinition>() {
        let uri = match &t.uri {
            ExpressionDefinition::Language { expression, .. } => expression.clone(),
            _ => "<rust expression>".to_string(),
        };
        cfg.insert("uri".into(), Value::String(uri));
        if t.ignore_invalid_endpoint {
            cfg.insert("ignoreInvalidEndpoint".into(), Value::Bool(true));
        }
    } else if let Some(s) = any.downcast_ref::<SetDefinition>() {
        match &s.target {
            SetTarget::Body => {}
            SetTarget::Header(n) | SetTarget::Property(n) | SetTarget::Variable(n) => {
                cfg.insert("name".into(), Value::String(n.clone()));
            }
        }
        cfg = merge(cfg, expression_to_value(&s.expression));
    } else if let Some(r) = any.downcast_ref::<RemoveDefinition>() {
        let (key, v, ex): (&str, &String, Option<&Vec<String>>) = match &r.target {
            RemoveTarget::Header(n) | RemoveTarget::Property(n) | RemoveTarget::Variable(n) => ("name", n, None),
            RemoveTarget::Headers { pattern, excludes } | RemoveTarget::Properties { pattern, excludes } => ("pattern", pattern, Some(excludes)),
        };
        cfg.insert(key.into(), Value::String(v.clone()));
        if let Some(ex) = ex.filter(|e| !e.is_empty()) {
            cfg.insert("excludePattern".into(), Value::String(ex.join(",")));
        }
    } else if let Some(c) = any.downcast_ref::<ConvertDefinition>() {
        cfg.insert("type".into(), Value::String(c.type_name.clone()));
        if let Some(h) = &c.header {
            cfg.insert("name".into(), Value::String(h.clone()));
        }
        if c.mandatory {
            cfg.insert("mandatory".into(), Value::Bool(true));
        }
    } else if let Some(l) = any.downcast_ref::<LogDefinition>() {
        let msg = match &l.message {
            ExpressionDefinition::Language { expression, .. } => expression.clone(),
            _ => "<rust expression>".to_string(),
        };
        cfg.insert("message".into(), Value::String(msg));
        if l.level != Level::Info {
            cfg.insert("loggingLevel".into(), Value::String(l.level.as_str().into()));
        }
        if let Some(n) = &l.log_name {
            cfg.insert("logName".into(), Value::String(n.clone()));
        }
    } else if let Some(p) = any.downcast_ref::<ProcessDefinition>() {
        cfg.insert("ref".into(), Value::String(p.reference.clone().unwrap_or_else(|| "<rust processor>".into())));
    } else if let Some(p) = any.downcast_ref::<SetExchangePatternDefinition>() {
        cfg.insert("pattern".into(), Value::String(format!("{:?}", p.pattern)));
    } else if any.is::<StopDefinition>() || any.is::<StepDefinition>() {
        // no options besides id/steps
    } else {
        let label = d.label();
        if label != name {
            cfg.insert("description".into(), Value::String(label));
        }
    }
    let children = d.outputs();
    if !children.is_empty() {
        cfg.insert("steps".into(), Value::Array(children.into_iter().map(step_to_value).collect()));
    }
    Value::object([(name, Value::Object(cfg))])
}

/// Convert a route definition to the YAML DSL shape (`{route: {...}}`).
pub fn route_definition_to_value(def: &RouteDefinition) -> Value {
    let mut route: ObjectMap = ObjectMap::new();
    if let Some(id) = &def.id {
        route.insert("id".into(), Value::String(id.clone()));
    }
    if let Some(d) = &def.description {
        route.insert("description".into(), Value::String(d.clone()));
    }
    if let Some(g) = &def.group {
        route.insert("group".into(), Value::String(g.clone()));
    }
    if !def.auto_startup {
        route.insert("autoStartup".into(), Value::Bool(false));
    }
    if let Some(o) = def.startup_order {
        route.insert("startupOrder".into(), Value::Int(o as i64));
    }
    let mut from: ObjectMap = ObjectMap::new();
    from.insert("uri".into(), Value::String(def.input.clone()));
    from.insert("steps".into(), Value::Array(def.outputs.iter().map(|o| step_to_value(o.as_ref())).collect()));
    route.insert("from".into(), Value::Object(from));
    Value::object([("route", Value::Object(route))])
}

/// Dump route definitions as YAML DSL.
pub fn dump_route_definitions_as_yaml(defs: &[RouteDefinition]) -> String {
    yaml::to_yaml(&Value::Array(defs.iter().map(route_definition_to_value).collect()))
}

/// Dump the routes of a context as YAML DSL (created routes; definitions are rendered
/// with [`route_definition_to_value`], others with id / uri / description only).
pub fn dump_routes_as_yaml(ctx: &CamelContext) -> String {
    let mut items = Vec::new();
    for r in ctx.routes() {
        let guard = r.definition.lock().unwrap();
        match guard.as_ref() {
            Some(def) => {
                let mut v = route_definition_to_value(def);
                if def.id.is_none() {
                    if let Some(Value::Object(m)) = v.as_object_mut().and_then(|o| o.get_mut("route")) {
                        m.insert("id".into(), Value::String(r.id.clone()));
                    }
                }
                items.push(v);
            }
            None => {
                let mut route = ObjectMap::new();
                route.insert("id".to_string(), Value::String(r.id.clone()));
                if let Some(d) = &r.description {
                    route.insert("description".to_string(), Value::String(d.clone()));
                }
                route.insert("from".to_string(), Value::object([("uri", Value::String(r.endpoint_uri.clone()))]));
                items.push(Value::object([("route", Value::Object(route))]));
            }
        }
    }
    yaml::to_yaml(&Value::Array(items))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expressions_all_forms() {
        let y = |s: &str| crate::yaml::parse(s).unwrap();
        let dbg = |e: ExpressionDefinition| format!("{:?}", e);
        assert_eq!(dbg(parse_expression(&y("simple: \"${body}\"")).unwrap()), "simple{${body}}");
        assert_eq!(dbg(parse_expression(&y("expression:\n  simple:\n    expression: x\n    resultType: int")).unwrap()), "simple{x}{\"resultType\": \"int\"}");
        assert_eq!(dbg(parse_expression(&y("constant: 5")).unwrap()), "constant{5}");
        assert_eq!(dbg(parse_expression(&y("header: foo")).unwrap()), "header{foo}");
        assert_eq!(dbg(parse_expression(&y("jsonpath: $.a")).unwrap()), "jsonpath{$.a}");
        assert_eq!(dbg(parse_expression(&y("tokenize: {token: ','}")).unwrap()), "tokenize{}{\"token\": \",\"}");
        assert_eq!(dbg(parse_expression(&y("exchangeProperty: p")).unwrap()), "exchangeProperty{p}");
        assert_eq!(dbg(parse_expression(&y("method: {ref: myBean, method: hi}")).unwrap()), "method{}{\"method\": \"hi\", \"ref\": \"myBean\"}");
        assert_eq!(dbg(parse_expression(&y("xpath: /a/b")).unwrap()), "xpath{/a/b}");
        assert_eq!(dbg(parse_expression(&y("jq: .foo")).unwrap()), "jq{.foo}");
        assert_eq!(dbg(parse_expression(&y("language: {language: groovy, expression: 1+1}")).unwrap()), "groovy{1+1}");
        assert_eq!(dbg(parse_expression(&y("expression:\n  constant: hi")).unwrap()), "constant{hi}");
        assert_eq!(dbg(parse_expression(&y("name: x\nsimple: y")).unwrap()), "simple{y}");
        assert_eq!(dbg(parse_expression(&y("\"${body}\"")).unwrap()), "simple{${body}}");
        assert!(parse_expression(&y("simple: a\nconstant: b")).unwrap_err().message.contains("only one"));
        assert!(parse_expression(&y("name: x")).unwrap_err().message.contains("missing expression"));
    }

    #[test]
    fn uri_building() {
        let y = |s: &str| crate::yaml::parse(s).unwrap();
        assert_eq!(endpoint_uri(&y("direct:a")).unwrap(), "direct:a");
        assert_eq!(
            endpoint_uri(&y("uri: timer:tick\nparameters:\n  period: 1000\n  repeatCount: 5")).unwrap(),
            "timer:tick?period=1000&repeatCount=5"
        );
        assert_eq!(endpoint_uri(&y("uri: log:a?level=INFO\nparameters:\n  showAll: true")).unwrap(), "log:a?level=INFO&showAll=true");
        assert_eq!(
            endpoint_uri(&y("uri: x:y\nparameters:\n  q: a b&c\n  p: '{{prop}}'\n  r: RAW(a+b)\n  s: '#bean'")).unwrap(),
            "x:y?q=a%20b%26c&p={{prop}}&r=RAW(a+b)&s=%23bean"
        );
        assert!(endpoint_uri(&y("parameters: {a: 1}")).unwrap_err().message.contains("uri"));
    }

    #[test]
    fn template_substitution() {
        let v = crate::yaml::parse(
            "id: t\nparameters:\n  - name: a\n  - name: b\n    defaultValue: B\nfrom:\n  uri: direct:{{a}}\n  steps:\n    - to: mock:{{b}}-{{other}}",
        )
        .unwrap();
        let t = RouteTemplateDefinition::parse(&v).unwrap();
        assert_eq!(t.parameters.len(), 2);
        assert!(t.parameters[0].required);
        assert!(!t.parameters[1].required);
        let mut p = BTreeMap::new();
        let e = t.instantiate(&p).unwrap_err();
        assert!(e.message.contains("mandatory parameters are missing: a"), "{}", e);
        p.insert("a".to_string(), "x".to_string());
        let r = t.instantiate(&p).unwrap();
        assert_eq!(r.get("from").unwrap().get("uri").unwrap(), &Value::from("direct:x"));
        let to = r.get("from").unwrap().get("steps").unwrap().get("0").unwrap().get("to").unwrap();
        assert_eq!(to, &Value::from("mock:B-{{other}}"));
    }
}

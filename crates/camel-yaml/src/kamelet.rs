//! Kamelets (Camel 3.x/4 route snippets with a typed interface) and Pipes.
//!
//! A Kamelet is a YAML resource:
//!
//! ```yaml
//! apiVersion: camel.apache.org/v1
//! kind: Kamelet
//! metadata:
//!   name: greeting-source
//!   labels:
//!     camel.apache.org/kamelet.type: source
//! spec:
//!   definition:
//!     required: [message]
//!     properties:
//!       message: {type: string}
//!       period: {type: integer, default: 1000}
//!   template:                 # (`spec.flow` is accepted as legacy name)
//!     from:
//!       uri: timer:tick
//!       parameters:
//!         period: "{{period}}"
//!       steps:
//!         - setBody:
//!             constant: "{{message}}"
//!         - to: kamelet:sink
//! ```
//!
//! Register Kamelets with [`add_kamelet`] / [`add_kamelet_definition`] /
//! [`load_kamelets_from_dir`], or by loading a YAML document of `kind: Kamelet` through
//! the YAML DSL loader. Built-in Kamelets ([`BUILTIN_KAMELETS`]: `timer-source`,
//! `log-sink`, `log-action`, `insert-header-action`, `set-body-action`, mirroring the
//! Apache Camel Kamelets catalog) are always available.
//!
//! The `kamelet:` component ([`register_kamelet_component`]; registered automatically
//! by the YAML loader and by [`add_kamelet`]) uses them:
//!
//! - `from: kamelet:timer-source?message=hi` - **source**: the Kamelet route (template
//!   `from` + steps) is created when the consumer starts; its `kamelet:sink` feeds the
//!   consuming route.
//! - `to: kamelet:log-sink?showHeaders=true` - **sink/action**: the Kamelet route starts
//!   at `kamelet:source`; the exchange is processed synchronously and returns to the
//!   caller at `kamelet:sink` (or at the end of the Kamelet route).
//! - `kamelet:name/routeId?...` sets the id of the generated route (default `name-N`).
//!
//! `{{property}}` placeholders of the template are resolved from, in order: the uri
//! parameters, the application properties `camel.kamelet.<name>.<routeId>.<property>`
//! and `camel.kamelet.<name>.<property>`, the declared `default`. Missing required
//! properties fail the endpoint creation. `{{?property}}` marks an optional value: when
//! it has no value the option (a `parameters` entry or a `key=value` uri query option)
//! is removed. Other placeholders are left to the context's property placeholders.
//!
//! `kind: Pipe` (and the older `KameletBinding`) resources with `spec.source`,
//! `spec.steps` and `spec.sink` are turned into a route whose id is `metadata.name`
//! ([`pipe_to_route`]). Each endpoint is a `uri`, a `ref` to a Kamelet with `properties`,
//! or a `ref` of another kind: Strimzi `KafkaTopic` (`kafka:<name>`), Knative `Channel`/
//! `Broker` (`vm:<name>`, an in-process channel), Kubernetes `Service`
//! (`http://<name>[.<namespace>]`), `Endpoint`, or kinds handled by a resolver registered
//! with [`register_pipe_ref_resolver`]. `dataTypes: {in|out: {format: X}}` on an endpoint
//! inserts a `transformDataType` to `X` (validated against the Kamelet's
//! `spec.dataTypes.<slot>.types`, whose `format`/`mediaType` are applied) after a source /
//! action and before an action / sink. `spec.errorHandler` becomes the route error handler:
//! `sink` (dead letter channel to its `endpoint` with redelivery `parameters`), `log`
//! (default error handler) or `none`.

use std::any::Any;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use camel_core::context::WeakContext;
use camel_core::model::RouteDefinition;
use camel_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::{CamelContext, CamelError, Result, Value};

use crate::dsl::{build_uri, scalar_string, YamlLoader};

/// Registry name of the per-context [`KameletRegistry`].
pub const KAMELET_REGISTRY_BEAN: &str = "camel-yaml.kamelets";
/// Label holding the Kamelet type (`source`, `sink`, `action`).
pub const KAMELET_TYPE_LABEL: &str = "camel.apache.org/kamelet.type";
/// Route property set on routes generated from a Kamelet (value: the Kamelet name).
pub const KAMELET_ROUTE_PROPERTY: &str = "kamelet";

fn kamelet_error(msg: impl Into<String>) -> CamelError {
    CamelError::new(crate::dsl::YAML_DSL_ERROR, msg)
}

// ---------------------------------------------------------------------------
// Definitions
// ---------------------------------------------------------------------------

/// A declared Kamelet property.
#[derive(Clone, Debug, PartialEq)]
pub struct KameletProperty {
    pub name: String,
    pub default: Option<String>,
    pub required: bool,
    pub title: Option<String>,
    pub description: Option<String>,
    pub type_name: Option<String>,
}

/// A parsed Kamelet.
#[derive(Clone, Debug)]
pub struct KameletDefinition {
    pub name: String,
    /// `source`, `sink` or `action` (from the `camel.apache.org/kamelet.type` label).
    pub kamelet_type: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub properties: Vec<KameletProperty>,
    /// The route-shaped template (`{from: {uri, parameters, steps}}`).
    pub template: Value,
    /// Template local beans (`spec.template.beans`), created for each Kamelet route.
    pub beans: Vec<Value>,
    /// Declared data types (`spec.dataTypes.in|out|error`).
    pub data_types: BTreeMap<String, KameletDataTypes>,
    /// The whole resource.
    pub raw: Value,
}

/// The data types a Kamelet declares for one slot (`in`, `out`, `error`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KameletDataTypes {
    pub default: Option<String>,
    /// name -> (format, mediaType)
    pub types: BTreeMap<String, (Option<String>, Option<String>)>,
}

impl KameletDataTypes {
    fn parse(v: &Value) -> Self {
        let mut d = KameletDataTypes { default: v.get("default").and_then(scalar_string), types: BTreeMap::new() };
        if let Some(Value::Object(types)) = v.get("types") {
            for (name, t) in types {
                d.types.insert(name.clone(), (t.get("format").and_then(scalar_string), t.get("mediaType").and_then(scalar_string)));
            }
        }
        d
    }
}

impl KameletDefinition {
    /// Parse a Kamelet resource value.
    pub fn parse(v: &Value) -> Result<Self> {
        let kind = v.get("kind").and_then(scalar_string).unwrap_or_default();
        if kind != "Kamelet" {
            return Err(kamelet_error(format!("Expected a resource of kind Kamelet but found '{}'", kind)));
        }
        let metadata = v.get("metadata");
        let name = metadata
            .and_then(|m| m.get("name"))
            .and_then(scalar_string)
            .ok_or_else(|| kamelet_error("A Kamelet requires 'metadata.name'"))?;
        let kamelet_type = metadata.and_then(|m| m.get("labels")).and_then(|l| l.get(KAMELET_TYPE_LABEL)).and_then(scalar_string);
        let spec = v.get("spec").ok_or_else(|| kamelet_error(format!("Kamelet '{}' requires a 'spec'", name)))?;
        let definition = spec.get("definition");
        let required_list: Vec<String> = definition
            .and_then(|d| d.get("required"))
            .and_then(|r| r.as_array())
            .map(|a| a.iter().filter_map(scalar_string).collect())
            .unwrap_or_default();
        let mut properties = Vec::new();
        if let Some(Value::Object(props)) = definition.and_then(|d| d.get("properties")) {
            for (pname, p) in props {
                let required = required_list.contains(pname) || matches!(p.get("required"), Some(Value::Bool(true)));
                properties.push(KameletProperty {
                    name: pname.clone(),
                    default: p.get("default").and_then(scalar_string),
                    required,
                    title: p.get("title").and_then(scalar_string),
                    description: p.get("description").and_then(scalar_string),
                    type_name: p.get("type").and_then(scalar_string),
                });
            }
        }
        for r in &required_list {
            if !properties.iter().any(|p| &p.name == r) {
                properties.push(KameletProperty { name: r.clone(), default: None, required: true, title: None, description: None, type_name: None });
            }
        }
        let tpl = spec
            .get("template")
            .or_else(|| spec.get("flow"))
            .ok_or_else(|| kamelet_error(format!("Kamelet '{}' requires 'spec.template' (or 'spec.flow')", name)))?;
        let template = if let Some(r) = tpl.get("route") {
            r.clone()
        } else if tpl.get("from").is_some() {
            Value::object([("from", tpl.get("from").unwrap().clone())])
        } else {
            return Err(kamelet_error(format!("Kamelet '{}': the template requires a 'from'", name)));
        };
        if template.get("from").and_then(|f| crate::dsl::endpoint_uri(f).ok()).is_none() {
            return Err(kamelet_error(format!("Kamelet '{}': the template 'from' requires a 'uri'", name)));
        }
        let beans = match tpl.get("beans") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(a)) => a.clone(),
            Some(_) => return Err(kamelet_error(format!("Kamelet '{}': 'spec.template.beans' must be a list", name))),
        };
        for b in &beans {
            if b.get("name").and_then(scalar_string).is_none() {
                return Err(kamelet_error(format!("Kamelet '{}': every template bean requires a 'name'", name)));
            }
        }
        let mut data_types = BTreeMap::new();
        if let Some(Value::Object(dt)) = spec.get("dataTypes") {
            for (slot, v) in dt {
                data_types.insert(slot.clone(), KameletDataTypes::parse(v));
            }
        }
        Ok(KameletDefinition {
            name,
            kamelet_type,
            data_types,
            title: definition.and_then(|d| d.get("title")).and_then(scalar_string),
            description: definition.and_then(|d| d.get("description")).and_then(scalar_string),
            properties,
            template,
            beans,
            raw: v.clone(),
        })
    }

    /// Parse a Kamelet from YAML text.
    pub fn parse_yaml(text: &str) -> Result<Self> {
        let v = crate::yaml::parse(text).map_err(CamelError::from)?;
        Self::parse(&v)
    }

    /// Resolve the property values for a route: uri parameters, then application
    /// properties, then defaults. Fails on missing required properties.
    pub fn resolve_properties(&self, ctx: &CamelContext, route_id: Option<&str>, given: &BTreeMap<String, String>) -> Result<BTreeMap<String, String>> {
        let props = ctx.properties();
        let mut values = BTreeMap::new();
        let mut missing = Vec::new();
        for p in &self.properties {
            let v = given
                .get(&p.name)
                .cloned()
                .or_else(|| route_id.and_then(|r| props.get(&format!("camel.kamelet.{}.{}.{}", self.name, r, p.name))))
                .or_else(|| props.get(&format!("camel.kamelet.{}.{}", self.name, p.name)))
                .or_else(|| p.default.clone());
            match v {
                Some(v) => {
                    values.insert(p.name.clone(), v);
                }
                None if p.required => missing.push(p.name.clone()),
                None => {}
            }
        }
        // undeclared parameters are passed through as well
        for (k, v) in given {
            values.entry(k.clone()).or_insert_with(|| v.clone());
        }
        if !missing.is_empty() {
            return Err(kamelet_error(format!("Kamelet {} the following mandatory properties are missing: {}", self.name, missing.join(", "))));
        }
        Ok(values)
    }

    /// The registry name of the template local bean `name` for the Kamelet route `route_id`.
    pub fn local_bean_name(name: &str, route_id: &str) -> String {
        format!("{}-{}", name, route_id)
    }

    /// Create the template local beans (`spec.template.beans`) of the Kamelet route
    /// `route_id`, as Camel does for route template local beans: each bean is created with
    /// the registered bean factories (Kamelet property placeholders in its definition are
    /// resolved) and bound under a name unique to the route; `{{beanName}}` in the template
    /// resolves to that name (`#bean:{{beanName}}`). The names are added to `values`.
    pub fn create_local_beans(&self, ctx: &CamelContext, route_id: &str, values: &mut BTreeMap<String, String>) -> Result<()> {
        if self.beans.is_empty() {
            return Ok(());
        }
        let mut declared: Vec<&str> = self.properties.iter().map(|p| p.name.as_str()).collect();
        for b in &self.beans {
            let name = b.get("name").and_then(scalar_string).unwrap_or_default();
            values.insert(name.clone(), Self::local_bean_name(&name, route_id));
        }
        let names: Vec<String> = self.beans.iter().filter_map(|b| b.get("name").and_then(scalar_string)).collect();
        declared.extend(names.iter().map(|s| s.as_str()));
        let beans: Vec<Value> = self
            .beans
            .iter()
            .map(|b| {
                let mut b = substitute(b, values, &declared);
                if let Value::Object(m) = &mut b {
                    let name = m.get("name").and_then(scalar_string).unwrap_or_default();
                    m.insert("name".into(), Value::String(Self::local_bean_name(&name, route_id)));
                }
                b
            })
            .collect();
        YamlLoader::new()
            .load_bean_list(ctx, &Value::Array(beans), &format!("kamelet[{}].beans", self.name))
            .map_err(|e| kamelet_error(format!("Kamelet {}: {}", self.name, e.message)))
    }

    /// Build the route-shaped value of a Kamelet route: placeholders substituted,
    /// `kamelet:source`/`kamelet:sink` bound to `route_id`.
    pub fn instantiate(&self, route_id: &str, values: &BTreeMap<String, String>) -> Value {
        let mut declared: Vec<&str> = self.properties.iter().map(|p| p.name.as_str()).collect();
        let bean_names: Vec<String> = self.beans.iter().filter_map(|b| b.get("name").and_then(scalar_string)).collect();
        declared.extend(bean_names.iter().map(|s| s.as_str()));
        let mut v = substitute(&self.template, values, &declared);
        bind_internal_endpoints(&mut v, route_id);
        if let Value::Object(m) = &mut v {
            m.insert("id".into(), Value::String(route_id.to_string()));
        }
        v
    }
}

/// The property name of an unresolved optional placeholder `{{?name}}` (whole string).
fn unresolved_optional(s: &str, values: &BTreeMap<String, String>) -> Option<String> {
    let inner = s.trim().strip_prefix("{{?")?.strip_suffix("}}")?;
    if inner.contains("{{") || values.contains_key(inner) {
        return None;
    }
    Some(inner.to_string())
}

/// Substitute `{{name}}`, `{{name:default}}` and `{{?name}}` placeholders of Kamelet properties.
fn substitute(v: &Value, values: &BTreeMap<String, String>, declared: &[&str]) -> Value {
    match v {
        Value::String(s) if s.contains("{{") => Value::String(substitute_text(&remove_optional_query(s, values), values, declared)),
        Value::Array(a) => Value::Array(a.iter().map(|x| substitute(x, values, declared)).collect()),
        Value::Object(m) => Value::Object(
            m.iter()
                .filter(|(_, x)| !matches!(x, Value::String(s) if unresolved_optional(s, values).is_some()))
                .map(|(k, x)| (k.clone(), substitute(x, values, declared)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Remove `key={{?name}}` query options without value from a uri string.
fn remove_optional_query(s: &str, values: &BTreeMap<String, String>) -> String {
    let Some((base, query)) = s.split_once('?') else { return s.to_string() };
    if !query.contains("{{?") || base.contains("{{?") {
        return s.to_string();
    }
    let kept: Vec<&str> = query
        .split('&')
        .filter(|kv| {
            let val = kv.split_once('=').map(|(_, v)| v).unwrap_or("");
            unresolved_optional(val, values).is_none()
        })
        .collect();
    if kept.is_empty() {
        base.to_string()
    } else {
        format!("{}?{}", base, kept.join("&"))
    }
}

fn substitute_text(s: &str, values: &BTreeMap<String, String>, declared: &[&str]) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("{{") {
        let Some(len) = rest[start + 2..].find("}}") else { break };
        let inner = &rest[start + 2..start + 2 + len];
        let (optional, inner_name) = match inner.strip_prefix('?') {
            Some(n) => (true, n),
            None => (false, inner),
        };
        let (name, default) = match inner_name.split_once(':') {
            Some((n, d)) => (n.trim(), Some(d)),
            None => (inner_name.trim(), None),
        };
        out.push_str(&rest[..start]);
        match (values.get(name), default) {
            (Some(v), _) => out.push_str(v),
            (None, Some(d)) if declared.contains(&name) => out.push_str(d),
            (None, _) if optional => {}
            _ => out.push_str(&rest[start..start + 4 + len]),
        }
        rest = &rest[start + 4 + len..];
    }
    out.push_str(rest);
    out
}

/// Rewrite `kamelet:source` / `kamelet:sink` uris to carry `routeId`.
fn bind_internal_endpoints(v: &mut Value, route_id: &str) {
    match v {
        Value::String(s) => {
            for name in ["kamelet:source", "kamelet:sink"] {
                if s == name || s.starts_with(&format!("{}?", name)) {
                    let sep = if s.contains('?') { '&' } else { '?' };
                    *s = format!("{}{}routeId={}", s, sep, camel_core::util::url_encode(route_id));
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| bind_internal_endpoints(x, route_id)),
        Value::Object(m) => m.values_mut().for_each(|x| bind_internal_endpoints(x, route_id)),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// The Kamelets known to a context (see [`kamelet_registry`]).
#[derive(Default)]
pub struct KameletRegistry {
    kamelets: RwLock<BTreeMap<String, Arc<KameletDefinition>>>,
}

impl KameletRegistry {
    /// Add (or replace) a Kamelet.
    pub fn add(&self, def: KameletDefinition) {
        self.kamelets.write().unwrap().insert(def.name.clone(), Arc::new(def));
    }
    /// A Kamelet by name: registered ones first, then the built-in catalog.
    pub fn get(&self, name: &str) -> Option<Arc<KameletDefinition>> {
        if let Some(k) = self.kamelets.read().unwrap().get(name) {
            return Some(k.clone());
        }
        let text = BUILTIN_KAMELETS.iter().find(|(n, _)| *n == name)?.1;
        let def = KameletDefinition::parse_yaml(text).expect("built-in kamelet must be valid");
        let def = Arc::new(def);
        self.kamelets.write().unwrap().insert(name.to_string(), def.clone());
        Some(def)
    }
    /// Names of the registered Kamelets (built-ins only once used).
    pub fn names(&self) -> Vec<String> {
        self.kamelets.read().unwrap().keys().cloned().collect()
    }
    pub fn remove(&self, name: &str) -> bool {
        self.kamelets.write().unwrap().remove(name).is_some()
    }
}

/// Get (or create) the Kamelet registry of a context.
pub fn kamelet_registry(ctx: &CamelContext) -> Arc<KameletRegistry> {
    static CREATE: Mutex<()> = Mutex::new(());
    let _g = CREATE.lock().unwrap();
    if let Some(r) = ctx.registry().lookup::<KameletRegistry>(KAMELET_REGISTRY_BEAN) {
        return r;
    }
    let r = Arc::new(KameletRegistry::default());
    ctx.registry().bind_arc(KAMELET_REGISTRY_BEAN, r.clone() as Arc<dyn Any + Send + Sync>);
    r
}

/// Register the `kamelet` component on a context (idempotent).
pub fn register_kamelet_component(ctx: &CamelContext) {
    if !ctx.has_component("kamelet") {
        ctx.add_component("kamelet", KameletComponent::default());
    }
}

/// Parse a Kamelet from YAML and register it (and the `kamelet` component). Returns its name.
pub fn add_kamelet(ctx: &CamelContext, yaml: &str) -> Result<String> {
    let def = KameletDefinition::parse_yaml(yaml)?;
    let name = def.name.clone();
    add_kamelet_definition(ctx, def);
    Ok(name)
}

/// Register a parsed Kamelet (and the `kamelet` component).
pub fn add_kamelet_definition(ctx: &CamelContext, def: KameletDefinition) {
    register_kamelet_component(ctx);
    kamelet_registry(ctx).add(def);
}

/// Load every Kamelet of a directory: `*.kamelet.yaml`/`*.kamelet.yml` files must be
/// Kamelets; other `.yaml`/`.yml` files are loaded when they are of `kind: Kamelet`
/// and skipped otherwise. Returns the names of the loaded Kamelets (sorted by file name).
pub fn load_kamelets_from_dir(ctx: &CamelContext, dir: impl AsRef<Path>) -> Result<Vec<String>> {
    let dir = dir.as_ref();
    register_kamelet_component(ctx);
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| CamelError::io(format!("Cannot read Kamelet directory {}: {}", dir.display(), e)))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && matches!(p.extension().and_then(|e| e.to_str()), Some("yaml") | Some("yml")))
        .collect();
    files.sort();
    let mut names = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f).map_err(|e| CamelError::io(format!("Cannot read {}: {}", f.display(), e)))?;
        let fname = f.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
        let explicit = fname.ends_with(".kamelet.yaml") || fname.ends_with(".kamelet.yml");
        let v = crate::yaml::parse(&text).map_err(|e| {
            let ce: CamelError = e.into();
            kamelet_error(format!("{} in {}", ce.message, f.display()))
        })?;
        if !explicit && v.get("kind").and_then(scalar_string).as_deref() != Some("Kamelet") {
            continue;
        }
        let def = KameletDefinition::parse(&v).map_err(|e| kamelet_error(format!("{} (in {})", e.message, f.display())))?;
        names.push(def.name.clone());
        add_kamelet_definition(ctx, def);
    }
    Ok(names)
}

// ---------------------------------------------------------------------------
// Resource documents (Kamelet, Pipe, KameletBinding) in the YAML loader
// ---------------------------------------------------------------------------

/// Handle a YAML document that is a Camel resource (`apiVersion` + `kind`):
/// Kamelets are registered, Pipes/KameletBindings become routes. Returns None for
/// ordinary route documents.
pub fn load_resource_document(loader: &YamlLoader, ctx: &CamelContext, doc: &Value) -> Result<Option<Vec<RouteDefinition>>> {
    let Value::Object(m) = doc else { return Ok(None) };
    if !(m.contains_key("apiVersion") && m.contains_key("kind")) {
        return Ok(None);
    }
    let kind = m.get("kind").and_then(scalar_string).unwrap_or_default();
    match kind.as_str() {
        "Kamelet" => {
            add_kamelet_definition(ctx, KameletDefinition::parse(doc)?);
            Ok(Some(Vec::new()))
        }
        "Pipe" | "KameletBinding" => {
            register_kamelet_component(ctx);
            let route = pipe_to_route_with(Some(ctx), doc)?;
            Ok(Some(vec![loader.build_route(ctx, &route)?]))
        }
        "Integration" => {
            // Camel K integration: spec.flows / spec.sources are not supported, but flows are plain routes
            match doc.get("spec").and_then(|s| s.get("flows")) {
                Some(flows) => Ok(Some(loader.load_routes_from_value(ctx, flows)?)),
                None => Err(kamelet_error("Integration resources are only supported with 'spec.flows'")),
            }
        }
        other => Err(kamelet_error(format!("Unsupported resource kind '{}' (supported: Kamelet, Pipe, KameletBinding)", other))),
    }
}

/// Resolves a Pipe `ref` of a kind other than `Kamelet` to an endpoint uri:
/// `(kind, apiVersion, name, namespace) -> uri`.
pub type PipeRefResolver = Arc<dyn Fn(&str, Option<&str>, &str, Option<&str>) -> Option<String> + Send + Sync>;

fn ref_resolvers() -> &'static RwLock<Vec<PipeRefResolver>> {
    static R: std::sync::OnceLock<RwLock<Vec<PipeRefResolver>>> = std::sync::OnceLock::new();
    R.get_or_init(Default::default)
}

/// Register a resolver for Pipe references of non-Kamelet kinds (consulted before the
/// built-in ones).
pub fn register_pipe_ref_resolver(f: impl Fn(&str, Option<&str>, &str, Option<&str>) -> Option<String> + Send + Sync + 'static) {
    ref_resolvers().write().unwrap().insert(0, Arc::new(f));
}

/// Built-in references: Strimzi `KafkaTopic` -> `kafka:<name>`; Knative `Channel`,
/// `InMemoryChannel`, `Broker` -> `vm:<name>` (an in-process channel shared by the
/// integrations of this process, standing in for the Knative eventing mesh); Kubernetes
/// `Service` -> `http://<name>[.<namespace>]`; `Endpoint`/`Uri` with the uri as name.
fn builtin_ref(kind: &str, name: &str, namespace: Option<&str>) -> Option<String> {
    Some(match kind {
        "KafkaTopic" => format!("kafka:{}", name),
        "Channel" | "InMemoryChannel" | "KafkaChannel" | "Broker" => format!("vm:{}", name),
        "Service" => match namespace {
            Some(ns) => format!("http://{}.{}", name, ns),
            None => format!("http://{}", name),
        },
        "Endpoint" | "Uri" => name.to_string(),
        _ => return None,
    })
}

/// The endpoint of a Pipe source/step/sink: (uri, Kamelet name when it is a Kamelet).
fn pipe_endpoint_ref(v: &Value, what: &str) -> Result<(String, Option<String>)> {
    let props = v.get("properties");
    if let Some(uri) = v.get("uri").and_then(scalar_string) {
        return Ok((build_uri(&uri, props)?, None));
    }
    let r = v.get("ref").ok_or_else(|| kamelet_error(format!("Pipe {} requires a 'ref' or a 'uri'", what)))?;
    let kind = r.get("kind").and_then(scalar_string).unwrap_or_else(|| "Kamelet".into());
    let name = r.get("name").and_then(scalar_string).ok_or_else(|| kamelet_error(format!("Pipe {} 'ref' requires a 'name'", what)))?;
    if kind == "Kamelet" {
        return Ok((build_uri(&format!("kamelet:{}", name), props)?, Some(name)));
    }
    let api = r.get("apiVersion").and_then(scalar_string);
    let ns = r.get("namespace").and_then(scalar_string);
    let resolved = ref_resolvers().read().unwrap().iter().find_map(|f| f(&kind, api.as_deref(), &name, ns.as_deref()));
    let uri = resolved.or_else(|| builtin_ref(&kind, &name, ns.as_deref())).ok_or_else(|| {
        kamelet_error(format!(
            "Pipe {}: references of kind '{}' are not supported (Kamelet, KafkaTopic, Channel, Broker, Service, Endpoint, or a registered resolver)",
            what, kind
        ))
    })?;
    Ok((build_uri(&uri, props)?, None))
}

fn pipe_endpoint(v: &Value, what: &str) -> Result<String> {
    pipe_endpoint_ref(v, what).map(|(u, _)| u)
}

/// The steps applying a Pipe endpoint's `dataTypes.<slot>.format`: `transformDataType`
/// to the format (the `format` declared by the Kamelet for that type name, else the name)
/// and the declared `mediaType` as `Content-Type`.
fn data_type_steps(ctx: Option<&CamelContext>, v: &Value, kamelet: Option<&str>, slot: &str, what: &str) -> Result<Vec<Value>> {
    let Some(format) = v.get("dataTypes").and_then(|d| d.get(slot)).and_then(|d| d.get("format")).and_then(scalar_string) else {
        return Ok(Vec::new());
    };
    let declared = match (ctx, kamelet) {
        (Some(c), Some(k)) => kamelet_registry(c).get(k).and_then(|d| d.data_types.get(slot).cloned()),
        _ => None,
    };
    let (target, media) = match &declared {
        Some(d) if !d.types.is_empty() => {
            let (f, m) = d.types.get(&format).cloned().ok_or_else(|| {
                kamelet_error(format!(
                    "Pipe {}: Kamelet '{}' does not support the {} data type '{}' (supported: {})",
                    what,
                    kamelet.unwrap_or_default(),
                    slot,
                    format,
                    d.types.keys().cloned().collect::<Vec<_>>().join(", ")
                ))
            })?;
            (f.unwrap_or_else(|| format.clone()), m)
        }
        _ => (format.clone(), None),
    };
    let mut steps = vec![Value::object([("transformDataType", Value::object([("toType", Value::String(target))]))])];
    if let Some(m) = media {
        steps.push(Value::object([("setHeader", Value::object([("name", Value::from("Content-Type")), ("constant", Value::String(m))]))]));
    }
    Ok(steps)
}

/// The route `errorHandler` of a Pipe `spec.errorHandler`: `sink` (dead letter channel to
/// `endpoint` — a Kamelet `ref` or a `uri` — with `parameters`), `log` (default error
/// handler with `parameters`), `none`. Parameters are redelivery options
/// (`maximumRedeliveries`, `redeliveryDelay`, ...) or `useOriginalMessage`,
/// `useOriginalBody`, `deadLetterHandleNewException`.
fn pipe_error_handler(v: &Value) -> Result<Value> {
    let Value::Object(m) = v else { return Err(kamelet_error("Pipe 'errorHandler' must be a mapping")) };
    let (kind, cfg) = m.iter().next().ok_or_else(|| kamelet_error("Pipe 'errorHandler' is empty"))?;
    let split = |params: Option<&Value>| -> (camel_core::value::ObjectMap, camel_core::value::ObjectMap) {
        let mut top = camel_core::value::ObjectMap::new();
        let mut policy = camel_core::value::ObjectMap::new();
        if let Some(Value::Object(p)) = params {
            for (k, x) in p {
                if ["useOriginalMessage", "useOriginalBody", "deadLetterHandleNewException"].contains(&k.as_str()) {
                    top.insert(k.clone(), x.clone());
                } else {
                    policy.insert(k.clone(), x.clone());
                }
            }
        }
        if !policy.is_empty() {
            top.insert("redeliveryPolicy".into(), Value::Object(policy.clone()));
        }
        (top, policy)
    };
    Ok(match kind.as_str() {
        "none" => Value::object([("noErrorHandler", Value::Object(Default::default()))]),
        "log" => {
            let (top, _) = split(cfg.get("parameters"));
            Value::object([("defaultErrorHandler", Value::Object(top))])
        }
        "sink" => {
            let ep = cfg.get("endpoint").ok_or_else(|| kamelet_error("Pipe errorHandler 'sink' requires an 'endpoint'"))?;
            let uri = pipe_endpoint(ep, "errorHandler sink")?;
            let (mut top, _) = split(cfg.get("parameters"));
            top.insert("deadLetterUri".into(), Value::String(uri));
            Value::object([("deadLetterChannel", Value::Object(top))])
        }
        "ref" => Value::object([("refErrorHandler", cfg.clone())]),
        other => return Err(kamelet_error(format!("Pipe errorHandler '{}' is not supported (sink, log, none, ref)", other))),
    })
}

/// Convert a `Pipe` / `KameletBinding` resource into a route-shaped value.
pub fn pipe_to_route(doc: &Value) -> Result<Value> {
    pipe_to_route_with(None, doc)
}

/// [`pipe_to_route`] with the context whose Kamelets validate the requested data types.
pub fn pipe_to_route_with(ctx: Option<&CamelContext>, doc: &Value) -> Result<Value> {
    let name = doc.get("metadata").and_then(|m| m.get("name")).and_then(scalar_string);
    let spec = doc.get("spec").ok_or_else(|| kamelet_error("A Pipe requires a 'spec'"))?;
    let source_v = spec.get("source").ok_or_else(|| kamelet_error("A Pipe requires 'spec.source'"))?;
    let (source, source_k) = pipe_endpoint_ref(source_v, "source")?;
    let mut steps = data_type_steps(ctx, source_v, source_k.as_deref(), "out", "source")?;
    if let Some(Value::Array(items)) = spec.get("steps") {
        for (i, s) in items.iter().enumerate() {
            let what = format!("step {}", i);
            let (uri, k) = pipe_endpoint_ref(s, &what)?;
            steps.extend(data_type_steps(ctx, s, k.as_deref(), "in", &what)?);
            steps.push(Value::object([("to", Value::String(uri))]));
            steps.extend(data_type_steps(ctx, s, k.as_deref(), "out", &what)?);
        }
    }
    if let Some(sink) = spec.get("sink") {
        let (uri, k) = pipe_endpoint_ref(sink, "sink")?;
        steps.extend(data_type_steps(ctx, sink, k.as_deref(), "in", "sink")?);
        steps.push(Value::object([("to", Value::String(uri))]));
    }
    let mut route = camel_core::value::ObjectMap::new();
    if let Some(n) = name {
        route.insert("id".to_string(), Value::String(n));
    }
    if let Some(eh) = spec.get("errorHandler") {
        route.insert("errorHandler".to_string(), pipe_error_handler(eh)?);
    }
    route.insert("from".to_string(), Value::object([("uri", Value::String(source)), ("steps", Value::Array(steps))]));
    Ok(Value::Object(route))
}

// ---------------------------------------------------------------------------
// The kamelet component
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Slots {
    /// Consumers of `kamelet:source?routeId=..` (the Kamelet routes' inputs).
    sources: RwLock<HashMap<String, Arc<dyn Processor>>>,
    /// Consumers of `kamelet:<name>` (routes consuming from a source Kamelet), keyed by the Kamelet route id.
    sinks: RwLock<HashMap<String, Arc<dyn Processor>>>,
}

static ROUTE_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// The `kamelet` component (see the [module documentation](self)).
#[derive(Default)]
pub struct KameletComponent {
    slots: Arc<Slots>,
}

impl Component for KameletComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let path = remaining.trim_matches('/');
        let (name, route_id) = match path.split_once('/') {
            Some((n, r)) => (n.to_string(), Some(r.to_string()).filter(|r| !r.is_empty())),
            None => (path.to_string(), None),
        };
        if name == "source" || name == "sink" {
            let route_id = params.take("routeId").ok_or_else(|| {
                CamelError::illegal_argument(format!("kamelet:{} can only be used inside a Kamelet template", name))
            })?;
            return Ok(Arc::new(InternalEndpoint { uri: uri.to_string(), source: name == "source", route_id, slots: self.slots.clone() }));
        }
        let kamelet = kamelet_registry(ctx)
            .get(&name)
            .ok_or_else(|| CamelError::illegal_argument(format!("Kamelet with id {} not found in locations: registry, built-in catalog", name)))?;
        let given = params.take_all();
        // validate required properties eagerly
        kamelet.resolve_properties(ctx, route_id.as_deref(), &given)?;
        Ok(Arc::new(KameletEndpoint { uri: uri.to_string(), kamelet, route_id, params: given, ctx: ctx.downgrade(), slots: self.slots.clone() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// An endpoint using a Kamelet (`kamelet:name?...`).
pub struct KameletEndpoint {
    uri: String,
    kamelet: Arc<KameletDefinition>,
    route_id: Option<String>,
    params: BTreeMap<String, String>,
    ctx: WeakContext,
    slots: Arc<Slots>,
}

impl KameletEndpoint {
    pub fn kamelet(&self) -> &KameletDefinition {
        &self.kamelet
    }
}

/// Creates/removes the route generated from a Kamelet.
struct KameletRoute {
    kamelet: Arc<KameletDefinition>,
    route_id_hint: Option<String>,
    params: BTreeMap<String, String>,
    ctx: WeakContext,
    current: Mutex<Option<String>>,
}

impl KameletRoute {
    fn new(ep: &KameletEndpoint) -> Self {
        KameletRoute {
            kamelet: ep.kamelet.clone(),
            route_id_hint: ep.route_id.clone(),
            params: ep.params.clone(),
            ctx: ep.ctx.clone(),
            current: Mutex::new(None),
        }
    }

    fn next_id(&self, ctx: &CamelContext) -> String {
        if let Some(id) = &self.route_id_hint {
            return id.clone();
        }
        let existing = ctx.route_ids();
        loop {
            let id = format!("{}-{}", self.kamelet.name, ROUTE_COUNTER.fetch_add(1, Ordering::Relaxed) + 1);
            if !existing.contains(&id) {
                return id;
            }
        }
    }

    /// The id of the Kamelet route, creating the route if needed.
    fn ensure(&self, on_id: impl FnOnce(&str)) -> Result<String> {
        let mut cur = self.current.lock().unwrap();
        if let Some(id) = cur.as_ref() {
            return Ok(id.clone());
        }
        let ctx = self.ctx.upgrade().ok_or_else(|| CamelError::illegal_state("CamelContext is gone"))?;
        let id = self.next_id(&ctx);
        let mut values = self.kamelet.resolve_properties(&ctx, Some(&id), &self.params)?;
        self.kamelet.create_local_beans(&ctx, &id, &mut values)?;
        let value = self.kamelet.instantiate(&id, &values);
        let mut def = YamlLoader::new().build_route(&ctx, &value).map_err(|e| {
            CamelError::new(e.kind.clone(), format!("Failed to create route from Kamelet {}: {}", self.kamelet.name, e.message)).with_cause(e)
        })?;
        def.properties.insert(KAMELET_ROUTE_PROPERTY.into(), self.kamelet.name.clone());
        def.properties.insert("routeTemplateId".into(), self.kamelet.name.clone());
        for (k, v) in &values {
            def.properties.insert(format!("templateParameter.{}", k), v.clone());
        }
        on_id(&id);
        ctx.add_route_definition(def)?;
        *cur = Some(id.clone());
        Ok(id)
    }

    fn remove(&self) {
        let id = self.current.lock().unwrap().take();
        if let (Some(id), Some(ctx)) = (id, self.ctx.upgrade()) {
            if ctx.route(&id).is_some() {
                let _ = ctx.stop_route(&id);
                let _ = ctx.remove_route(&id);
            }
        }
    }

    fn ctx_started(&self) -> bool {
        self.ctx.upgrade().map(|c| c.is_started()).unwrap_or(false)
    }
}

impl Endpoint for KameletEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(KameletProducer { route: KameletRoute::new(self), slots: self.slots.clone() }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        Ok(Arc::new(KameletConsumer { route: KameletRoute::new(self), slots: self.slots.clone(), processor }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Sends exchanges through a sink/action Kamelet route.
struct KameletProducer {
    route: KameletRoute,
    slots: Arc<Slots>,
}

impl Service for KameletProducer {
    fn start(&self) -> Result<()> {
        if self.route.ctx_started() {
            self.route.ensure(|_| {})?;
        }
        Ok(())
    }
    fn stop(&self) -> Result<()> {
        self.route.remove();
        Ok(())
    }
}

impl Processor for KameletProducer {
    fn process(&self, exchange: &mut camel_core::Exchange) -> Result<()> {
        let id = self.route.ensure(|_| {})?;
        let consumer = self.slots.sources.read().unwrap().get(&id).cloned();
        match consumer {
            Some(p) => p.process(exchange),
            None => Err(CamelError::new(
                camel_core::error::kinds::DIRECT_CONSUMER_NOT_AVAILABLE,
                format!(
                    "Kamelet {} route {} has no running kamelet:source consumer (the Kamelet template must start with 'from: kamelet:source' to be used as a sink or action)",
                    self.route.kamelet.name, id
                ),
            )),
        }
    }
}

/// Consumes from a source Kamelet route.
struct KameletConsumer {
    route: KameletRoute,
    slots: Arc<Slots>,
    processor: Arc<dyn Processor>,
}

impl Service for KameletConsumer {
    fn start(&self) -> Result<()> {
        let slots = self.slots.clone();
        let processor = self.processor.clone();
        self.route.ensure(move |id| {
            slots.sinks.write().unwrap().insert(id.to_string(), processor);
        })?;
        Ok(())
    }
    fn stop(&self) -> Result<()> {
        let id = self.route.current.lock().unwrap().clone();
        self.route.remove();
        if let Some(id) = id {
            self.slots.sinks.write().unwrap().remove(&id);
        }
        Ok(())
    }
}

/// `kamelet:source?routeId=..` / `kamelet:sink?routeId=..` inside a Kamelet route.
struct InternalEndpoint {
    uri: String,
    source: bool,
    route_id: String,
    slots: Arc<Slots>,
}

impl Endpoint for InternalEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        if !self.source {
            return Err(CamelError::illegal_argument("kamelet:sink cannot be used as a consumer"));
        }
        Ok(Arc::new(SourceConsumer { route_id: self.route_id.clone(), slots: self.slots.clone(), processor }))
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        if self.source {
            return Err(CamelError::illegal_argument("kamelet:source cannot be used as a producer"));
        }
        Ok(Arc::new(SinkProducer { route_id: self.route_id.clone(), slots: self.slots.clone() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct SourceConsumer {
    route_id: String,
    slots: Arc<Slots>,
    processor: Arc<dyn Processor>,
}

impl Service for SourceConsumer {
    fn start(&self) -> Result<()> {
        self.slots.sources.write().unwrap().insert(self.route_id.clone(), self.processor.clone());
        Ok(())
    }
    fn stop(&self) -> Result<()> {
        self.slots.sources.write().unwrap().remove(&self.route_id);
        Ok(())
    }
}

struct SinkProducer {
    route_id: String,
    slots: Arc<Slots>,
}

impl Service for SinkProducer {}

impl Processor for SinkProducer {
    fn process(&self, exchange: &mut camel_core::Exchange) -> Result<()> {
        let consumer = self.slots.sinks.read().unwrap().get(&self.route_id).cloned();
        match consumer {
            // source Kamelet: hand over to the consuming route
            Some(p) => p.process(exchange),
            // sink/action Kamelet used as producer: return to the caller
            None => Ok(()),
        }
    }
}

// ---------------------------------------------------------------------------
// Built-in Kamelets
// ---------------------------------------------------------------------------

/// Built-in Kamelets (name, YAML), adapted from the Apache Camel Kamelets catalog.
pub const BUILTIN_KAMELETS: &[(&str, &str)] = &[
    ("timer-source", TIMER_SOURCE),
    ("log-sink", LOG_SINK),
    ("log-action", LOG_ACTION),
    ("insert-header-action", INSERT_HEADER_ACTION),
    ("set-body-action", SET_BODY_ACTION),
];

const TIMER_SOURCE: &str = r#"
apiVersion: camel.apache.org/v1
kind: Kamelet
metadata:
  name: timer-source
  labels:
    camel.apache.org/kamelet.type: source
spec:
  definition:
    title: Timer Source
    description: Produces periodic messages with a custom payload.
    required:
      - message
    type: object
    properties:
      period:
        title: Period
        description: The interval (in milliseconds) to wait between producing the next message.
        type: integer
        default: 1000
      message:
        title: Message
        description: The message to generate.
        type: string
        example: hello world
      contentType:
        title: Content Type
        description: The content type of the generated message.
        type: string
        default: text/plain
      repeatCount:
        title: Repeat Count
        description: Specifies a maximum limit of number of fires
        type: integer
  template:
    from:
      uri: timer:tick
      parameters:
        period: "{{period}}"
        repeatCount: "{{?repeatCount}}"
      steps:
        - setBody:
            constant: "{{message}}"
        - setHeader:
            name: Content-Type
            constant: "{{contentType}}"
        - to: kamelet:sink
"#;

const LOG_SINK: &str = r#"
apiVersion: camel.apache.org/v1
kind: Kamelet
metadata:
  name: log-sink
  labels:
    camel.apache.org/kamelet.type: sink
spec:
  definition:
    title: Log Sink
    description: Logs all data that it receives, useful for debugging.
    type: object
    properties:
      loggerName:
        title: Logger Name
        description: Name of the logging category to use
        type: string
        default: log-sink
      level:
        title: Log Level
        description: Logging level to use
        type: string
        default: INFO
      logMask:
        title: Log Mask
        description: Mask sensitive information like password or passphrase in the log
        type: boolean
        default: false
      marker:
        title: Marker
        description: An optional Marker name to use
        type: string
      multiline:
        title: Multiline
        description: If enabled then each information is outputted on a newline
        type: boolean
        default: false
      showAllProperties:
        title: Show All Properties
        description: Show all of the exchange properties (both internal and custom)
        type: boolean
        default: false
      showBody:
        title: Show Body
        description: Show the message body
        type: boolean
        default: true
      showBodyType:
        title: Show Body Type
        description: Show the body Java type
        type: boolean
        default: true
      showExchangePattern:
        title: Show Exchange Pattern
        description: Shows the Message Exchange Pattern (or MEP for short)
        type: boolean
        default: true
      showHeaders:
        title: Show Headers
        description: Show the headers received
        type: boolean
        default: false
      showProperties:
        title: Show Properties
        description: Show the exchange properties (only custom). Use showAllProperties to show both internal and custom properties.
        type: boolean
        default: false
      showStreams:
        title: Show Streams
        description: Show the stream bodies (they may not be available in following steps)
        type: boolean
        default: false
      showCachedStreams:
        title: Show Cached Streams
        description: Whether Camel should show cached stream bodies or not.
        type: boolean
        default: true
  template:
    from:
      uri: kamelet:source
      steps:
        - to:
            uri: "log:{{loggerName}}"
            parameters:
              level: "{{level}}"
              logMask: "{{logMask}}"
              marker: "{{?marker}}"
              multiline: "{{multiline}}"
              showAllProperties: "{{showAllProperties}}"
              showBody: "{{showBody}}"
              showBodyType: "{{showBodyType}}"
              showExchangePattern: "{{showExchangePattern}}"
              showHeaders: "{{showHeaders}}"
              showProperties: "{{showProperties}}"
              showStreams: "{{showStreams}}"
              showCachedStreams: "{{showCachedStreams}}"
"#;

const LOG_ACTION: &str = r#"
apiVersion: camel.apache.org/v1
kind: Kamelet
metadata:
  name: log-action
  labels:
    camel.apache.org/kamelet.type: action
spec:
  definition:
    title: Log Action
    description: Logs all data that flows between source and sink, useful for debugging purposes.
    type: object
    properties:
      loggerName:
        title: Logger Name
        description: Name of the logging category to use
        type: string
        default: log-action
      level:
        title: Log Level
        description: Logging level to use
        type: string
        default: INFO
      logMask:
        title: Log Mask
        description: Mask sensitive information like password or passphrase in the log
        type: boolean
        default: false
      marker:
        title: Marker
        description: An optional Marker name to use
        type: string
      multiline:
        title: Multiline
        description: If enabled then each information is outputted on a newline
        type: boolean
        default: false
      showAllProperties:
        title: Show All Properties
        description: Show all of the exchange properties (both internal and custom)
        type: boolean
        default: false
      showBody:
        title: Show Body
        description: Show the message body
        type: boolean
        default: true
      showBodyType:
        title: Show Body Type
        description: Show the body Java type
        type: boolean
        default: true
      showExchangePattern:
        title: Show Exchange Pattern
        description: Shows the Message Exchange Pattern (or MEP for short)
        type: boolean
        default: true
      showHeaders:
        title: Show Headers
        description: Show the headers received
        type: boolean
        default: false
      showProperties:
        title: Show Properties
        description: Show the exchange properties (only custom). Use showAllProperties to show both internal and custom properties.
        type: boolean
        default: false
      showStreams:
        title: Show Streams
        description: Show the stream bodies (they may not be available in following steps)
        type: boolean
        default: false
      showCachedStreams:
        title: Show Cached Streams
        description: Whether Camel should show cached stream bodies or not.
        type: boolean
        default: true
  template:
    from:
      uri: kamelet:source
      steps:
        - to:
            uri: "log:{{loggerName}}"
            parameters:
              level: "{{level}}"
              logMask: "{{logMask}}"
              marker: "{{?marker}}"
              multiline: "{{multiline}}"
              showAllProperties: "{{showAllProperties}}"
              showBody: "{{showBody}}"
              showBodyType: "{{showBodyType}}"
              showExchangePattern: "{{showExchangePattern}}"
              showHeaders: "{{showHeaders}}"
              showProperties: "{{showProperties}}"
              showStreams: "{{showStreams}}"
              showCachedStreams: "{{showCachedStreams}}"
        - to: kamelet:sink
"#;

const INSERT_HEADER_ACTION: &str = r#"
apiVersion: camel.apache.org/v1
kind: Kamelet
metadata:
  name: insert-header-action
  labels:
    camel.apache.org/kamelet.type: action
spec:
  definition:
    title: Insert Header Action
    description: Adds a header with a constant value to the message in transit.
    required:
      - name
      - value
    type: object
    properties:
      name:
        title: Name
        description: The name of the header to insert.
        type: string
      value:
        title: Value
        description: The value of the header (it can use the Simple language).
        type: string
  template:
    from:
      uri: kamelet:source
      steps:
        - setHeader:
            name: "{{name}}"
            simple: "{{value}}"
        - to: kamelet:sink
"#;

const SET_BODY_ACTION: &str = r#"
apiVersion: camel.apache.org/v1
kind: Kamelet
metadata:
  name: set-body-action
  labels:
    camel.apache.org/kamelet.type: action
spec:
  definition:
    title: Set Body Action
    description: Sets the body of the message in transit.
    required:
      - value
    type: object
    properties:
      value:
        title: Value
        description: The new body (it can use the Simple language).
        type: string
  template:
    from:
      uri: kamelet:source
      steps:
        - setBody:
            simple: "{{value}}"
        - to: kamelet:sink
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_parse() {
        for (name, text) in BUILTIN_KAMELETS {
            let k = KameletDefinition::parse_yaml(text).unwrap();
            assert_eq!(&k.name, name);
            assert!(k.kamelet_type.is_some());
        }
        let t = KameletDefinition::parse_yaml(TIMER_SOURCE).unwrap();
        let msg = t.properties.iter().find(|p| p.name == "message").unwrap();
        assert!(msg.required);
        assert_eq!(t.properties.iter().find(|p| p.name == "period").unwrap().default.as_deref(), Some("1000"));
    }

    #[test]
    fn substitution_and_optional_values() {
        let t = KameletDefinition::parse_yaml(TIMER_SOURCE).unwrap();
        let ctx = CamelContext::new();
        let mut given = BTreeMap::new();
        given.insert("message".to_string(), "hi".to_string());
        let values = t.resolve_properties(&ctx, Some("r1"), &given).unwrap();
        let v = t.instantiate("r1", &values);
        assert_eq!(v.get("id").unwrap().as_str(), Some("r1"));
        let from = v.get("from").unwrap();
        let params = from.get("parameters").unwrap();
        assert_eq!(params.get("period").unwrap().as_str(), Some("1000"));
        assert!(params.get("repeatCount").is_none(), "optional value without value is removed");
        let steps = from.get("steps").unwrap().as_array().unwrap();
        assert_eq!(steps[0].get("setBody").unwrap().get("constant").unwrap().as_str(), Some("hi"));
        assert_eq!(steps[2].get("to").unwrap().as_str(), Some("kamelet:sink?routeId=r1"));
        assert!(t.resolve_properties(&ctx, None, &BTreeMap::new()).is_err());
        ctx.properties().set("camel.kamelet.timer-source.message", "from-props");
        assert_eq!(t.resolve_properties(&ctx, None, &BTreeMap::new()).unwrap()["message"], "from-props");
        ctx.properties().set("camel.kamelet.timer-source.r9.message", "per-route");
        assert_eq!(t.resolve_properties(&ctx, Some("r9"), &BTreeMap::new()).unwrap()["message"], "per-route");
    }

    #[test]
    fn optional_query_removal() {
        let mut values = BTreeMap::new();
        values.insert("a".to_string(), "1".to_string());
        assert_eq!(remove_optional_query("log:x?a={{a}}&b={{?b}}", &values), "log:x?a={{a}}");
        assert_eq!(remove_optional_query("log:x?b={{?b}}", &values), "log:x");
        assert_eq!(substitute_text("x={{?a}} y={{?b}}", &values, &[]), "x=1 y=");
    }

    #[test]
    fn pipe_conversion() {
        let doc = crate::yaml::parse(
            r#"
apiVersion: camel.apache.org/v1
kind: Pipe
metadata:
  name: my-pipe
spec:
  source:
    ref: {kind: Kamelet, apiVersion: camel.apache.org/v1, name: timer-source}
    properties: {message: "hello world"}
  steps:
    - ref: {kind: Kamelet, name: insert-header-action}
      properties: {name: h, value: v}
  sink:
    uri: mock:out
"#,
        )
        .unwrap();
        let r = pipe_to_route(&doc).unwrap();
        assert_eq!(r.get("id").unwrap().as_str(), Some("my-pipe"));
        assert_eq!(r.get("from").unwrap().get("uri").unwrap().as_str(), Some("kamelet:timer-source?message=hello%20world"));
        let steps = r.get("from").unwrap().get("steps").unwrap().as_array().unwrap();
        assert_eq!(steps[0].get("to").unwrap().as_str(), Some("kamelet:insert-header-action?name=h&value=v"));
        assert_eq!(steps[1].get("to").unwrap().as_str(), Some("mock:out"));
    }
}

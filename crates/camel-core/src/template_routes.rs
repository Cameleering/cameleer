//! Route templates in the Rust DSL (Camel `routeTemplate` / `TemplatedRouteBuilder`).
//!
//! ```
//! use std::collections::BTreeMap;
//! use camel_core::prelude::*;
//! use camel_core::template_routes::{TemplateParam, TemplatedRouteBuilder};
//!
//! let ctx = CamelContext::new();
//! ctx.add_route_template(
//!     "greeter",
//!     &[TemplateParam::required("name"), TemplateParam::optional("greeting", "Hello")],
//!     |b, p| {
//!         b.from(format!("direct:{}", p.get("name")))
//!             .set_body(constant(format!("{} {}", p.get("greeting"), p.get("name"))))
//!             .to("mock:greetings");
//!     },
//! );
//! let id = TemplatedRouteBuilder::new(&ctx, "greeter").route_id("hello-bob").parameter("name", "bob").add().unwrap();
//! assert_eq!(id, "hello-bob");
//! let mut params = BTreeMap::new();
//! params.insert("name".to_string(), "alice".to_string());
//! let generated = ctx.add_route_from_template("", "greeter", params).unwrap();
//! assert!(generated.starts_with("route"));
//! ```

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::context::CamelContext;
use crate::dsl::RouteBuilder;
use crate::error::{CamelError, Result};
use crate::model::RouteDefinition;

/// Route definition property holding the id of the template a route was created from.
pub const ROUTE_TEMPLATE_ID: &str = "routeTemplateId";

/// A template parameter declaration.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TemplateParam {
    pub name: String,
    pub default: Option<String>,
    pub required: bool,
    pub description: Option<String>,
}

impl TemplateParam {
    /// A required parameter without default.
    pub fn required(name: impl Into<String>) -> Self {
        TemplateParam { name: name.into(), default: None, required: true, description: None }
    }
    /// An optional parameter with a default value.
    pub fn optional(name: impl Into<String>, default: impl Into<String>) -> Self {
        TemplateParam { name: name.into(), default: Some(default.into()), required: false, description: None }
    }
    /// An optional parameter without default.
    pub fn optional_no_default(name: impl Into<String>) -> Self {
        TemplateParam { name: name.into(), default: None, required: false, description: None }
    }
    pub fn with_description(mut self, d: impl Into<String>) -> Self {
        self.description = Some(d.into());
        self
    }
}

/// The resolved parameters passed to a template's builder function.
#[derive(Clone, Debug, Default)]
pub struct TemplateParams {
    values: BTreeMap<String, String>,
    route_id: String,
    template_id: String,
}

impl TemplateParams {
    /// Value of a parameter (empty string when not set).
    pub fn get(&self, name: &str) -> &str {
        self.values.get(name).map(|s| s.as_str()).unwrap_or("")
    }
    pub fn get_opt(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(|s| s.as_str())
    }
    pub fn get_or<'a>(&'a self, name: &str, default: &'a str) -> &'a str {
        self.get_opt(name).unwrap_or(default)
    }
    pub fn contains(&self, name: &str) -> bool {
        self.values.contains_key(name)
    }
    pub fn values(&self) -> &BTreeMap<String, String> {
        &self.values
    }
    /// The id of the route being created.
    pub fn route_id(&self) -> &str {
        &self.route_id
    }
    pub fn template_id(&self) -> &str {
        &self.template_id
    }
    /// Replace `{{name}}` occurrences of template parameters in `text`.
    pub fn resolve(&self, text: &str) -> String {
        let mut out = text.to_string();
        for (k, v) in &self.values {
            out = out.replace(&format!("{{{{{}}}}}", k), v);
        }
        out
    }
}

/// Builds the route of a template.
pub type TemplateBuilderFn = Arc<dyn Fn(&mut RouteBuilder, &TemplateParams) + Send + Sync>;

/// A registered route template.
pub struct RouteTemplate {
    pub id: String,
    pub parameters: Vec<TemplateParam>,
    pub description: Option<String>,
    builder: TemplateBuilderFn,
}

impl RouteTemplate {
    /// Validate and resolve parameters (defaults applied).
    pub fn resolve_parameters(&self, given: &BTreeMap<String, String>) -> Result<BTreeMap<String, String>> {
        let mut values = given.clone();
        let mut missing = Vec::new();
        for p in &self.parameters {
            if values.contains_key(&p.name) {
                continue;
            }
            match &p.default {
                Some(d) => {
                    values.insert(p.name.clone(), d.clone());
                }
                None if p.required => missing.push(p.name.clone()),
                None => {}
            }
        }
        if !missing.is_empty() {
            return Err(CamelError::illegal_argument(format!(
                "Route template {} the following mandatory parameters must be provided: {}",
                self.id,
                missing.join(", ")
            )));
        }
        Ok(values)
    }
}

static TEMPLATE_ROUTE_COUNTER: AtomicUsize = AtomicUsize::new(0);

impl CamelContext {
    /// Register a route template built with the Rust DSL. The builder must create exactly one route.
    pub fn add_route_template<F>(&self, id: &str, params: &[TemplateParam], f: F)
    where
        F: Fn(&mut RouteBuilder, &TemplateParams) + Send + Sync + 'static,
    {
        self.add_route_template_with_description(id, None, params, f)
    }

    /// Like [`CamelContext::add_route_template`] with a description.
    pub fn add_route_template_with_description<F>(&self, id: &str, description: Option<&str>, params: &[TemplateParam], f: F)
    where
        F: Fn(&mut RouteBuilder, &TemplateParams) + Send + Sync + 'static,
    {
        let t = RouteTemplate { id: id.to_string(), parameters: params.to_vec(), description: description.map(|s| s.to_string()), builder: Arc::new(f) };
        crate::management::state(self).templates.write().unwrap().insert(id.to_string(), Arc::new(t));
    }

    /// Ids of the registered route templates.
    pub fn route_template_ids(&self) -> Vec<String> {
        crate::management::state(self).templates.read().unwrap().keys().cloned().collect()
    }

    pub fn route_template(&self, id: &str) -> Option<Arc<RouteTemplate>> {
        crate::management::state(self).templates.read().unwrap().get(id).cloned()
    }

    pub fn remove_route_template(&self, id: &str) -> bool {
        crate::management::state(self).templates.write().unwrap().remove(id).is_some()
    }

    /// Create a route from a template. An empty `route_id` generates one. Returns the route id.
    pub fn add_route_from_template(&self, route_id: &str, template_id: &str, params: BTreeMap<String, String>) -> Result<String> {
        self.add_route_from_template_with(route_id, template_id, params, None)
    }

    fn add_route_from_template_with(
        &self,
        route_id: &str,
        template_id: &str,
        params: BTreeMap<String, String>,
        configurer: Option<&dyn Fn(&mut RouteDefinition)>,
    ) -> Result<String> {
        let t = self
            .route_template(template_id)
            .ok_or_else(|| CamelError::illegal_argument(format!("Cannot find RouteTemplate with id {}", template_id)))?;
        let values = t.resolve_parameters(&params)?;
        let route_id = if route_id.is_empty() { self.generate_template_route_id() } else { route_id.to_string() };
        if self.route_ids().contains(&route_id) {
            return Err(CamelError::new(crate::error::kinds::FAILED_TO_CREATE_ROUTE, format!("Route with id {} already exists", route_id)));
        }
        let tp = TemplateParams { values, route_id: route_id.clone(), template_id: template_id.to_string() };
        let mut b = RouteBuilder::new(self.clone());
        (t.builder)(&mut b, &tp);
        let mut defs = b.into_definitions()?;
        if defs.len() != 1 {
            return Err(CamelError::illegal_argument(format!("Route template {} must define exactly one route (found {})", template_id, defs.len())));
        }
        let mut def = defs.pop().unwrap();
        def.id = Some(route_id.clone());
        def.properties.insert(ROUTE_TEMPLATE_ID.to_string(), template_id.to_string());
        for (k, v) in tp.values() {
            def.properties.insert(format!("templateParameter.{}", k), v.clone());
        }
        if let Some(c) = configurer {
            c(&mut def);
        }
        self.add_route_definition(def)?;
        Ok(route_id)
    }

    fn generate_template_route_id(&self) -> String {
        let existing = self.route_ids();
        loop {
            let n = TEMPLATE_ROUTE_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
            let id = format!("route{}", n);
            if !existing.contains(&id) {
                return id;
            }
        }
    }
}

/// Fluent builder for creating a route from a template (Camel `TemplatedRouteBuilder`).
pub struct TemplatedRouteBuilder {
    ctx: CamelContext,
    template_id: String,
    route_id: String,
    params: BTreeMap<String, String>,
    configurer: Option<Box<dyn Fn(&mut RouteDefinition)>>,
}

impl TemplatedRouteBuilder {
    pub fn new(ctx: &CamelContext, template_id: &str) -> Self {
        TemplatedRouteBuilder { ctx: ctx.clone(), template_id: template_id.to_string(), route_id: String::new(), params: BTreeMap::new(), configurer: None }
    }
    pub fn route_id(mut self, id: &str) -> Self {
        self.route_id = id.to_string();
        self
    }
    pub fn parameter(mut self, name: &str, value: impl ToString) -> Self {
        self.params.insert(name.to_string(), value.to_string());
        self
    }
    pub fn parameters(mut self, params: BTreeMap<String, String>) -> Self {
        self.params.extend(params);
        self
    }
    /// Customize the created route definition (e.g. description, auto startup).
    pub fn configure(mut self, f: impl Fn(&mut RouteDefinition) + 'static) -> Self {
        self.configurer = Some(Box::new(f));
        self
    }
    /// Create the route; returns its id.
    pub fn add(self) -> Result<String> {
        let TemplatedRouteBuilder { ctx, template_id, route_id, params, configurer } = self;
        ctx.add_route_from_template_with(&route_id, &template_id, params, configurer.as_deref())
    }
}

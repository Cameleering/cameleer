//! `bean:` and `class:` components - invoke a bean from the registry.
//!
//! `bean:name?method=foo&scope=Singleton`
//!
//! The registry entry may be:
//! * an `Arc<dyn Bean>` (see [`bind_bean`]) - invoked with the method name,
//! * a [`BeanMethods`] value - a set of named closures,
//! * an `Arc<dyn Processor>` (bound with `Registry::bind_processor`) - the method is ignored.
//!
//! The method can be overridden per message with the `CamelBeanMethodName` header.
//! A method given as `foo(${body})` is called with the name `foo` (parameter binding is
//! up to the bean). `class:` is an alias resolving the name in the registry.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::context::{CamelContext, Registry, WeakContext};
use crate::error::{kinds, CamelError, Result};
use crate::message::Exchange;
use crate::spi::{Component, Endpoint, Processor, Producer, Service};
use crate::util::Parameters;
use crate::value::Value;

/// Header to select the bean method per message.
pub const BEAN_METHOD_NAME: &str = "CamelBeanMethodName";

/// A bean that can be invoked by the `bean:` component and the bean EIP.
pub trait Bean: Send + Sync {
    /// Invoke `method` (None = the bean's default method) on the exchange.
    fn invoke(&self, method: Option<&str>, exchange: &mut Exchange) -> Result<()>;
}

impl<F> Bean for F
where
    F: Fn(Option<&str>, &mut Exchange) -> Result<()> + Send + Sync,
{
    fn invoke(&self, method: Option<&str>, exchange: &mut Exchange) -> Result<()> {
        self(method, exchange)
    }
}

type MethodFn = Arc<dyn Fn(&mut Exchange) -> Result<()> + Send + Sync>;

/// A bean made of named methods (closures). Methods registered with
/// [`BeanMethods::method_value`] return a value which becomes the message body
/// (like a Java bean method's return value).
#[derive(Clone, Default)]
pub struct BeanMethods {
    methods: BTreeMap<String, MethodFn>,
    default: Option<String>,
}

impl BeanMethods {
    pub fn new() -> Self {
        Self::default()
    }
    /// A method operating on the exchange.
    pub fn method(mut self, name: &str, f: impl Fn(&mut Exchange) -> Result<()> + Send + Sync + 'static) -> Self {
        if self.default.is_none() {
            self.default = Some(name.to_string());
        }
        self.methods.insert(name.to_string(), Arc::new(f));
        self
    }
    /// A method returning the new body.
    pub fn method_value(self, name: &str, f: impl Fn(&Exchange) -> Result<Value> + Send + Sync + 'static) -> Self {
        self.method(name, move |ex: &mut Exchange| {
            let v = f(ex)?;
            ex.set_body(v);
            Ok(())
        })
    }
    /// Set the method used when none is specified (defaults to the first registered).
    pub fn default_method(mut self, name: &str) -> Self {
        self.default = Some(name.to_string());
        self
    }
    pub fn method_names(&self) -> Vec<String> {
        self.methods.keys().cloned().collect()
    }
}

impl Bean for BeanMethods {
    fn invoke(&self, method: Option<&str>, exchange: &mut Exchange) -> Result<()> {
        let name = match method {
            Some(m) => m.to_string(),
            None if self.methods.len() == 1 => self.methods.keys().next().cloned().unwrap_or_default(),
            None => self.default.clone().ok_or_else(|| {
                CamelError::new("AmbiguousMethodCallException", "Ambiguous method invocation: no method specified")
            })?,
        };
        let f = self.methods.get(&name).ok_or_else(|| {
            CamelError::new("MethodNotFoundException", format!("Method with name: {} not found on bean. Methods: {:?}", name, self.method_names()))
        })?;
        f(exchange)
    }
}

/// Bind a bean in the registry so that `bean:name` can invoke it.
pub fn bind_bean(registry: &Registry, name: impl Into<String>, bean: impl Bean + 'static) {
    let b: Arc<dyn Bean> = Arc::new(bean);
    registry.bind(name, b);
}

/// Resolve a registry entry to something invokable.
pub fn lookup_bean(registry: &Registry, name: &str) -> Option<Arc<dyn Bean>> {
    if let Some(b) = registry.lookup::<Arc<dyn Bean>>(name) {
        return Some((*b).clone());
    }
    if let Some(b) = registry.lookup::<BeanMethods>(name) {
        return Some(b as Arc<dyn Bean>);
    }
    if let Some(p) = registry.lookup_processor(name) {
        return Some(Arc::new(move |_m: Option<&str>, ex: &mut Exchange| p.process(ex)));
    }
    None
}

/// Strip parameters from a method spec: `foo(${body}, 1)` -> `foo`.
fn method_name(spec: &str) -> &str {
    spec.split('(').next().unwrap_or(spec).trim()
}

/// Invoke a registry bean by name (used by the bean component and bean EIP).
pub fn invoke_bean(ctx: &CamelContext, name: &str, method: Option<&str>, exchange: &mut Exchange) -> Result<()> {
    let bean = lookup_bean(ctx.registry(), name).ok_or_else(|| CamelError::no_such_bean(name))?;
    let header = exchange.header(BEAN_METHOD_NAME).map(|v| v.to_string());
    let m = header.as_deref().or(method).map(method_name).filter(|m| !m.is_empty());
    bean.invoke(m, exchange)
}

/// The bean component (`bean:` and `class:`).
#[derive(Default)]
pub struct BeanComponent;

impl Component for BeanComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let method = params.take("method").filter(|m| !m.is_empty());
        for k in ["scope", "cache", "multiParameterArray", "lazyStartProducer"] {
            params.take(k);
        }
        params.take_prefixed("parameters.");
        params.take_prefixed("bean.");
        if remaining.is_empty() {
            return Err(CamelError::illegal_argument("bean name is required"));
        }
        Ok(Arc::new(BeanEndpoint { uri: uri.to_string(), name: remaining.to_string(), method, ctx: ctx.downgrade() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A bean endpoint (producer only).
pub struct BeanEndpoint {
    uri: String,
    name: String,
    method: Option<String>,
    ctx: WeakContext,
}

impl Endpoint for BeanEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(BeanProducer { name: self.name.clone(), method: self.method.clone(), ctx: self.ctx.clone() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct BeanProducer {
    name: String,
    method: Option<String>,
    ctx: WeakContext,
}

impl Service for BeanProducer {}

impl Processor for BeanProducer {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        let ctx = match exchange.context.clone().or_else(|| self.ctx.upgrade()) {
            Some(c) => c,
            None => return Err(CamelError::new(kinds::ILLEGAL_STATE, "CamelContext is not available")),
        };
        invoke_bean(&ctx, &self.name, self.method.as_deref(), exchange)
    }
}

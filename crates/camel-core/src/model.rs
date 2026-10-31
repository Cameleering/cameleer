//! Route model: route definitions made of processor definitions. The Rust DSL
//! (`dsl` module) and the YAML DSL both build this model; the model is turned into
//! runtime processors when a route is created.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::context::CamelContext;
use crate::error::{CamelError, Result};
use crate::processor::pipeline::Pipeline;
use crate::spi::{Processor, Service};

/// A node in a route definition that can create a runtime processor.
pub trait ProcessorDefinition: Send + Sync + Any {
    /// The EIP name (e.g. `to`, `filter`, `choice`), used for ids and diagnostics.
    fn name(&self) -> &str;

    /// Create the runtime processor.
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>>;

    /// Optional node id (`.id("x")` in the DSL).
    fn id(&self) -> Option<&str> {
        None
    }
    fn set_id(&mut self, _id: String) {}

    /// Block nodes (filter, split, choice...) accept child outputs until `.end()`.
    fn is_block(&self) -> bool {
        false
    }
    /// Add a child output to a block node.
    fn add_output(&mut self, _def: Box<dyn ProcessorDefinition>) -> Result<()> {
        Err(CamelError::illegal_state(format!("{} does not accept outputs", self.name())))
    }
    /// Child outputs (for introspection/dumping).
    fn outputs(&self) -> Vec<&dyn ProcessorDefinition> {
        Vec::new()
    }
    /// Short textual description for route dumps.
    fn label(&self) -> String {
        self.name().to_string()
    }
    /// Whether this node should be wrapped in the error-handling channel (default true).
    fn wrap_in_channel(&self) -> bool {
        true
    }

    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// Implements the boilerplate `as_any`/`as_any_mut`/id methods for a definition struct
/// that has an `id: Option<String>` field.
#[macro_export]
macro_rules! impl_definition_boilerplate {
    () => {
        fn id(&self) -> Option<&str> {
            self.id.as_deref()
        }
        fn set_id(&mut self, id: String) {
            self.id = Some(id);
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    };
}

/// Hook to wrap each processor of a route ("channel"), used for error handling,
/// interceptors, tracing, etc. Installed on the [`RouteContext`].
pub trait ChannelWrapper: Send + Sync {
    /// Wrap `processor` created from `definition`.
    fn wrap(&self, rc: &RouteContext, definition: &dyn ProcessorDefinition, processor: Arc<dyn Processor>) -> Result<Arc<dyn Processor>>;
}

/// Context used while creating the runtime processors of one route.
pub struct RouteContext {
    pub context: CamelContext,
    pub route_id: String,
    /// Services (stateful processors, producers) to start/stop with the route.
    services: Mutex<Vec<Arc<dyn Service>>>,
    /// Channel wrappers applied (in order) to each processor created via
    /// [`RouteContext::create_child_processor`].
    pub channel_wrappers: Vec<Arc<dyn ChannelWrapper>>,
    /// Free-form attributes for extensions (error handler config, etc.).
    pub attributes: Mutex<BTreeMap<String, Arc<dyn Any + Send + Sync>>>,
    /// Counter for generating node ids.
    node_counter: std::sync::atomic::AtomicUsize,
}

impl RouteContext {
    pub fn new(context: CamelContext, route_id: impl Into<String>) -> Self {
        RouteContext {
            context,
            route_id: route_id.into(),
            services: Mutex::new(Vec::new()),
            channel_wrappers: Vec::new(),
            attributes: Mutex::new(BTreeMap::new()),
            node_counter: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Register a service to be started before the route consumer and stopped after it.
    pub fn add_service(&self, s: Arc<dyn Service>) {
        self.services.lock().unwrap().push(s);
    }

    pub fn take_services(&self) -> Vec<Arc<dyn Service>> {
        std::mem::take(&mut self.services.lock().unwrap())
    }

    /// Next generated node id, e.g. `to3`.
    pub fn next_node_id(&self, name: &str) -> String {
        let n = self.node_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        format!("{}{}", name, n)
    }

    pub fn set_attribute<T: Any + Send + Sync>(&self, key: &str, v: T) {
        self.attributes.lock().unwrap().insert(key.to_string(), Arc::new(v));
    }

    pub fn attribute<T: Any + Send + Sync>(&self, key: &str) -> Option<Arc<T>> {
        self.attributes.lock().unwrap().get(key).cloned().and_then(|a| a.downcast::<T>().ok())
    }

    /// Create a processor for a single definition, wrapped in the channel.
    pub fn create_processor(&self, def: &dyn ProcessorDefinition) -> Result<Arc<dyn Processor>> {
        let p = def.create_processor(self).map_err(|e| {
            if e.kind == crate::error::kinds::FAILED_TO_CREATE_ROUTE {
                e
            } else {
                CamelError::new(
                    crate::error::kinds::FAILED_TO_CREATE_ROUTE,
                    format!("Failed to create route {} at: >>> {} <<< because of {}", self.route_id, def.label(), e.message),
                )
                .with_cause(e)
            }
        })?;
        if !def.wrap_in_channel() {
            return Ok(p);
        }
        let mut wrapped = p;
        for w in &self.channel_wrappers {
            wrapped = w.wrap(self, def, wrapped)?;
        }
        Ok(wrapped)
    }

    /// Create a pipeline processor from a list of child definitions.
    pub fn create_child_processor(&self, defs: &[Box<dyn ProcessorDefinition>]) -> Result<Arc<dyn Processor>> {
        let mut procs = Vec::with_capacity(defs.len());
        for d in defs {
            procs.push(self.create_processor(d.as_ref())?);
        }
        if procs.len() == 1 {
            return Ok(procs.pop().unwrap());
        }
        Ok(Arc::new(Pipeline::new(procs)))
    }
}

/// Definition of a route: `from(uri)` followed by outputs.
pub struct RouteDefinition {
    pub id: Option<String>,
    pub description: Option<String>,
    pub group: Option<String>,
    /// Input endpoint uri.
    pub input: String,
    /// Top-level outputs.
    pub outputs: Vec<Box<dyn ProcessorDefinition>>,
    /// Open blocks while building with the fluent DSL.
    pub(crate) stack: Vec<Box<dyn ProcessorDefinition>>,
    pub auto_startup: bool,
    pub startup_order: Option<i32>,
    /// Route-scoped extension data (error handler definitions, onException clauses,
    /// route policies, ...). Keyed by extension name.
    pub extensions: BTreeMap<String, Box<dyn Any + Send + Sync>>,
    /// Errors that happened while building with the fluent DSL (reported on add).
    pub(crate) build_errors: Vec<CamelError>,
    /// Whether this is a template-generated or rest-generated route, etc.
    pub properties: BTreeMap<String, String>,
}

impl std::fmt::Debug for RouteDefinition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Route[{} -> {}]", self.input, self.dump_outputs(&self.outputs))
    }
}

impl RouteDefinition {
    pub fn new(input: impl Into<String>) -> Self {
        RouteDefinition {
            id: None,
            description: None,
            group: None,
            input: input.into(),
            outputs: Vec::new(),
            stack: Vec::new(),
            auto_startup: true,
            startup_order: None,
            extensions: BTreeMap::new(),
            build_errors: Vec::new(),
            properties: BTreeMap::new(),
        }
    }

    fn dump_outputs(&self, outs: &[Box<dyn ProcessorDefinition>]) -> String {
        let parts: Vec<String> = outs.iter().map(|o| dump_def(o.as_ref())).collect();
        format!("[{}]", parts.join(", "))
    }

    /// Add a processor definition at the current position (inside the innermost open block).
    pub fn add(&mut self, def: Box<dyn ProcessorDefinition>) -> &mut Self {
        if let Some(top) = self.stack.last_mut() {
            if let Err(e) = top.add_output(def) {
                self.build_errors.push(e);
            }
        } else {
            self.outputs.push(def);
        }
        self
    }

    /// Open a block: the definition becomes the target of subsequent `add` calls until `end()`.
    pub fn push_block(&mut self, def: Box<dyn ProcessorDefinition>) -> &mut Self {
        self.stack.push(def);
        self
    }

    /// Close the innermost open block.
    pub fn end(&mut self) -> &mut Self {
        match self.stack.pop() {
            Some(def) => {
                self.add(def);
            }
            None => self.build_errors.push(CamelError::illegal_state("end() called without an open block")),
        }
        self
    }

    /// Mutable access to the innermost open block, downcast to `T`.
    pub fn current_block_mut<T: 'static>(&mut self) -> Option<&mut T> {
        self.stack.last_mut().and_then(|b| b.as_any_mut().downcast_mut::<T>())
    }

    /// Find the innermost open block of type `T`, closing blocks above it.
    /// Used e.g. by `when()` after nested blocks inside a previous `when`.
    pub fn unwind_to_block<T: 'static>(&mut self) -> Option<&mut T> {
        let pos = self.stack.iter().rposition(|b| b.as_any().is::<T>())?;
        while self.stack.len() > pos + 1 {
            self.end();
        }
        self.current_block_mut::<T>()
    }

    /// The last added definition at the current level (for `.id()`).
    pub fn last_mut(&mut self) -> Option<&mut Box<dyn ProcessorDefinition>> {
        // We cannot reach into a block's outputs generically for mutation, so ids are
        // applied to the most recently added top-level item or the open block itself.
        if let Some(top) = self.stack.last_mut() {
            return Some(top);
        }
        self.outputs.last_mut()
    }

    /// Record a DSL build error (reported when the route is added).
    pub fn add_build_error(&mut self, e: CamelError) {
        self.build_errors.push(e);
    }

    /// Finish building: close any open blocks and return accumulated errors.
    pub fn finish(&mut self) -> Result<()> {
        while !self.stack.is_empty() {
            self.end();
        }
        if let Some(e) = self.build_errors.first() {
            return Err(e.clone());
        }
        Ok(())
    }

    pub fn extension<T: 'static>(&self, key: &str) -> Option<&T> {
        self.extensions.get(key).and_then(|b| b.downcast_ref::<T>())
    }

    pub fn extension_mut<T: Any + Send + Sync + Default>(&mut self, key: &str) -> &mut T {
        if !self.extensions.get(key).map(|b| b.is::<T>()).unwrap_or(false) {
            self.extensions.insert(key.to_string(), Box::new(T::default()));
        }
        self.extensions.get_mut(key).unwrap().downcast_mut::<T>().unwrap()
    }

    /// Textual dump of the route (for diagnostics / tests).
    pub fn dump(&self) -> String {
        format!("from({}) -> {}", self.input, self.dump_outputs(&self.outputs))
    }
}

fn dump_def(d: &dyn ProcessorDefinition) -> String {
    let children = d.outputs();
    if children.is_empty() {
        d.label()
    } else {
        let parts: Vec<String> = children.into_iter().map(dump_def).collect();
        format!("{}[{}]", d.label(), parts.join(", "))
    }
}

/// A generic block definition with child outputs; other block EIPs can embed it.
#[derive(Default)]
pub struct OutputList {
    pub outputs: Vec<Box<dyn ProcessorDefinition>>,
}

impl OutputList {
    pub fn push(&mut self, d: Box<dyn ProcessorDefinition>) {
        self.outputs.push(d)
    }
    pub fn refs(&self) -> Vec<&dyn ProcessorDefinition> {
        self.outputs.iter().map(|o| o.as_ref()).collect()
    }
}

/// A definition that wraps an arbitrary closure creating a processor. Handy for
/// extensions that do not want to declare a struct.
pub struct CustomDefinition {
    pub id: Option<String>,
    pub name: String,
    pub factory: Box<dyn Fn(&RouteContext) -> Result<Arc<dyn Processor>> + Send + Sync>,
}

impl ProcessorDefinition for CustomDefinition {
    fn name(&self) -> &str {
        &self.name
    }
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        (self.factory)(rc)
    }
    impl_definition_boilerplate!();
}

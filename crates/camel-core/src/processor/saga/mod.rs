//! Saga EIP (Apache Camel `saga()`): long running actions with compensation.
//!
//! A `saga` block starts (or joins) a saga managed by a [`CamelSagaService`]. Every saga
//! block registers a *step* with the saga coordinator: an optional `compensation`
//! endpoint, an optional `completion` endpoint, `option`s (expressions evaluated when the
//! step begins and passed to the compensation/completion endpoints as headers) and an
//! optional `timeout`. When the saga is compensated (failure, timeout or
//! `saga:compensate`), the compensation endpoints of all steps are called in reverse
//! order; when it completes (success with `completionMode=AUTO`, or `saga:complete`),
//! the completion endpoints are called. Both receive the `Long-Running-Action` header
//! with the saga id.
//!
//! Propagation (as in Camel): `REQUIRED` (join or create), `REQUIRES_NEW` (always a new
//! saga, the outer one is restored afterwards), `MANDATORY` (fail without a saga),
//! `SUPPORTS` (join if present), `NOT_SUPPORTED` (suspend the current saga),
//! `NEVER` (fail if a saga is present).
//!
//! Services: [`InMemorySagaService`] (default, bound lazily in the registry under
//! [`SAGA_SERVICE_BEAN`]) and [`lra::LraSagaService`] (a Narayana LRA compatible REST
//! coordinator client). Install a service with
//! [`CamelContextSagaExt::set_saga_service`].

pub mod in_memory;
pub mod lra;

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::builder::ExpressionDefinition;
use crate::context::{CamelContext, WeakContext};
use crate::error::{kinds, CamelError, Result};
use crate::impl_definition_boilerplate;
use crate::message::Exchange;
use crate::model::{OutputList, ProcessorDefinition, RouteContext};
use crate::spi::{Component, Endpoint, Expression, Processor, Producer, Service};
use crate::util::Parameters;
use crate::value::Value;

pub use in_memory::InMemorySagaService;
pub use lra::LraSagaService;

/// Header carrying the saga id (Camel `Exchange.SAGA_LONG_RUNNING_ACTION`).
pub const SAGA_LONG_RUNNING_ACTION: &str = "Long-Running-Action";
/// Registry name of the context's saga service.
pub const SAGA_SERVICE_BEAN: &str = "CamelSagaService";

/// Saga propagation (Camel `SagaPropagation`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SagaPropagation {
    /// Join the current saga or create a new one.
    #[default]
    Required,
    /// Always create a new saga; the current one is suspended and restored afterwards.
    RequiresNew,
    /// Join the current saga; fail when there is none.
    Mandatory,
    /// Join the current saga if any, otherwise run without a saga.
    Supports,
    /// Suspend the current saga while the block runs.
    NotSupported,
    /// Fail when a saga is in progress.
    Never,
}

impl SagaPropagation {
    /// Parse `REQUIRED`, `REQUIRES_NEW`, `requiresNew`, ... (case-insensitive, `_`/`-` ignored).
    pub fn parse(s: &str) -> Result<Self> {
        let n: String = s.chars().filter(|c| *c != '_' && *c != '-').collect::<String>().to_ascii_lowercase();
        Ok(match n.as_str() {
            "required" => SagaPropagation::Required,
            "requiresnew" => SagaPropagation::RequiresNew,
            "mandatory" => SagaPropagation::Mandatory,
            "supports" => SagaPropagation::Supports,
            "notsupported" => SagaPropagation::NotSupported,
            "never" => SagaPropagation::Never,
            _ => return Err(CamelError::illegal_argument(format!("Unknown saga propagation: {}", s))),
        })
    }
}

/// Saga completion mode (Camel `SagaCompletionMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SagaCompletionMode {
    /// The saga completes when the exchange that created it completes successfully.
    #[default]
    Auto,
    /// The saga is completed with `saga:complete` (or compensated with `saga:compensate`,
    /// on failure, or on timeout).
    Manual,
}

impl SagaCompletionMode {
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_uppercase().as_str() {
            "AUTO" => Ok(SagaCompletionMode::Auto),
            "MANUAL" => Ok(SagaCompletionMode::Manual),
            _ => Err(CamelError::illegal_argument(format!("Unknown saga completion mode: {}", s))),
        }
    }
}

/// A step registered with a saga coordinator (Camel `CamelSagaStep`).
#[derive(Clone, Default)]
pub struct CamelSagaStep {
    /// Endpoint uri called on compensation.
    pub compensation: Option<String>,
    /// Endpoint uri called on completion.
    pub completion: Option<String>,
    /// Options evaluated when the step begins; passed as headers.
    pub options: Vec<(String, Arc<dyn Expression>)>,
    /// The saga is compensated if it did not complete within this time after the step began.
    pub timeout: Option<Duration>,
}

impl std::fmt::Debug for CamelSagaStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CamelSagaStep")
            .field("compensation", &self.compensation)
            .field("completion", &self.completion)
            .field("options", &self.options.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>())
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl CamelSagaStep {
    /// Evaluate the step options against an exchange (null values are skipped).
    pub fn evaluate_options(&self, exchange: &Exchange) -> Result<BTreeMap<String, Value>> {
        let mut m = BTreeMap::new();
        for (k, e) in &self.options {
            let v = e.evaluate(exchange)?;
            if !v.is_null() {
                m.insert(k.clone(), v);
            }
        }
        Ok(m)
    }
}

/// The coordinator of one saga (Camel `CamelSagaCoordinator`).
pub trait CamelSagaCoordinator: Send + Sync {
    /// The saga id (value of the `Long-Running-Action` header).
    fn id(&self) -> String;
    /// Register a step (fails if the saga is not running any more).
    fn begin_step(&self, exchange: &Exchange, step: &CamelSagaStep) -> Result<()>;
    /// Compensate the saga: call the compensation endpoints in reverse order.
    fn compensate(&self, exchange: &Exchange) -> Result<()>;
    /// Complete the saga: call the completion endpoints in reverse order.
    fn complete(&self, exchange: &Exchange) -> Result<()>;
}

/// A saga service (Camel `CamelSagaService`): creates and finds coordinators.
pub trait CamelSagaService: Send + Sync {
    /// Create a new saga.
    fn new_saga(&self, exchange: &Exchange) -> Result<Arc<dyn CamelSagaCoordinator>>;
    /// Find a saga by id.
    fn get_saga(&self, id: &str) -> Result<Option<Arc<dyn CamelSagaCoordinator>>>;
    /// Called once per saga EIP when its route is created.
    fn register_step(&self, _step: &CamelSagaStep) -> Result<()> {
        Ok(())
    }
}

/// Saga service management on the [`CamelContext`].
pub trait CamelContextSagaExt {
    /// Install the saga service used by saga EIPs without an explicit `saga_service` ref.
    fn set_saga_service(&self, service: Arc<dyn CamelSagaService>);
    /// The context's saga service (an [`InMemorySagaService`] is created on first use).
    fn saga_service(&self) -> Arc<dyn CamelSagaService>;
}

fn service_lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

impl CamelContextSagaExt for CamelContext {
    fn set_saga_service(&self, service: Arc<dyn CamelSagaService>) {
        let _g = service_lock().lock().unwrap();
        self.registry().bind(SAGA_SERVICE_BEAN, service);
    }
    fn saga_service(&self) -> Arc<dyn CamelSagaService> {
        let _g = service_lock().lock().unwrap();
        if let Some(s) = self.registry().lookup::<Arc<dyn CamelSagaService>>(SAGA_SERVICE_BEAN) {
            return (*s).clone();
        }
        let s: Arc<dyn CamelSagaService> = Arc::new(InMemorySagaService::new(self));
        self.registry().bind(SAGA_SERVICE_BEAN, s.clone());
        s
    }
}

/// Resolve a saga service: a registry reference, or the context default.
pub fn resolve_saga_service(ctx: &CamelContext, reference: Option<&str>) -> Result<Arc<dyn CamelSagaService>> {
    match reference {
        Some(name) => {
            let name = name.trim_start_matches('#').trim_start_matches("bean:");
            if let Some(s) = ctx.registry().lookup::<Arc<dyn CamelSagaService>>(name) {
                return Ok((*s).clone());
            }
            if let Some(s) = ctx.registry().lookup::<InMemorySagaService>(name) {
                return Ok(s);
            }
            if let Some(s) = ctx.registry().lookup::<LraSagaService>(name) {
                return Ok(s);
            }
            Err(CamelError::new(kinds::NO_SUCH_BEAN, format!("No CamelSagaService named {} found in the registry", name)))
        }
        None => Ok(ctx.saga_service()),
    }
}

/// The coordinator of the saga the exchange is bound to (from `Long-Running-Action`).
pub fn current_coordinator(exchange: &Exchange, service: &dyn CamelSagaService) -> Result<Option<Arc<dyn CamelSagaCoordinator>>> {
    match exchange.header(SAGA_LONG_RUNNING_ACTION) {
        Some(v) if !v.is_null() && !v.to_string().is_empty() => service.get_saga(&v.to_string()),
        _ => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Definition
// ---------------------------------------------------------------------------

/// Model of the `saga` EIP (a block).
pub struct SagaDefinition {
    pub id: Option<String>,
    pub propagation: SagaPropagation,
    pub completion_mode: SagaCompletionMode,
    pub timeout: Option<Duration>,
    pub compensation: Option<String>,
    pub completion: Option<String>,
    pub options: Vec<(String, ExpressionDefinition)>,
    /// Registry name of the saga service (default: the context's service).
    pub saga_service: Option<String>,
    pub outputs: OutputList,
}

impl Default for SagaDefinition {
    fn default() -> Self {
        SagaDefinition {
            id: None,
            propagation: SagaPropagation::Required,
            completion_mode: SagaCompletionMode::Auto,
            timeout: None,
            compensation: None,
            completion: None,
            options: Vec::new(),
            saga_service: None,
            outputs: OutputList::default(),
        }
    }
}

impl SagaDefinition {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ProcessorDefinition for SagaDefinition {
    fn name(&self) -> &str {
        "saga"
    }
    fn label(&self) -> String {
        format!("saga[{:?}]", self.propagation)
    }
    fn is_block(&self) -> bool {
        true
    }
    fn add_output(&mut self, def: Box<dyn ProcessorDefinition>) -> Result<()> {
        self.outputs.push(def);
        Ok(())
    }
    fn outputs(&self) -> Vec<&dyn ProcessorDefinition> {
        self.outputs.refs()
    }
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        let ctx = &rc.context;
        let mut step = CamelSagaStep { timeout: self.timeout, ..Default::default() };
        // endpoints are resolved (validated) at route creation, as in Camel
        if let Some(u) = &self.compensation {
            step.compensation = Some(ctx.get_endpoint(u)?.uri().to_string());
        }
        if let Some(u) = &self.completion {
            step.completion = Some(ctx.get_endpoint(u)?.uri().to_string());
        }
        for (k, e) in &self.options {
            step.options.push((k.clone(), e.resolve_expression(ctx)?));
        }
        let service = resolve_saga_service(ctx, self.saga_service.as_deref())?;
        service.register_step(&step)?;
        let child: Arc<dyn Processor> = if self.outputs.outputs.is_empty() {
            Arc::new(crate::processor::pipeline::Pipeline::new(vec![]))
        } else {
            rc.create_child_processor(&self.outputs.outputs)?
        };
        Ok(Arc::new(SagaProcessor { propagation: self.propagation, completion_mode: self.completion_mode, step, child, service }))
    }
    impl_definition_boilerplate!();
}

// ---------------------------------------------------------------------------
// Processor
// ---------------------------------------------------------------------------

/// Runtime processor of the saga EIP.
pub struct SagaProcessor {
    propagation: SagaPropagation,
    completion_mode: SagaCompletionMode,
    step: CamelSagaStep,
    child: Arc<dyn Processor>,
    service: Arc<dyn CamelSagaService>,
}

impl SagaProcessor {
    fn run_child(&self, exchange: &mut Exchange) {
        crate::processor::pipeline::process_safely(self.child.as_ref(), exchange);
    }

    fn join(&self, coordinator: Arc<dyn CamelSagaCoordinator>, exchange: &mut Exchange) -> Result<()> {
        coordinator.begin_step(exchange, &self.step)?;
        self.run_child(exchange);
        Ok(())
    }

    fn start_new(&self, exchange: &mut Exchange, restore_previous: bool) -> Result<()> {
        let previous = exchange.header(SAGA_LONG_RUNNING_ACTION).cloned();
        let coordinator = self.service.new_saga(exchange)?;
        exchange.set_header(SAGA_LONG_RUNNING_ACTION, coordinator.id());
        let r = match coordinator.begin_step(exchange, &self.step) {
            Ok(()) => {
                self.run_child(exchange);
                self.handle_completion(coordinator.as_ref(), exchange);
                Ok(())
            }
            Err(e) => {
                let _ = coordinator.compensate(exchange);
                Err(e)
            }
        };
        if restore_previous {
            match previous {
                Some(p) => exchange.set_header(SAGA_LONG_RUNNING_ACTION, p),
                None => {
                    exchange.message.headers.remove(SAGA_LONG_RUNNING_ACTION);
                }
            }
        }
        r
    }

    fn handle_completion(&self, coordinator: &dyn CamelSagaCoordinator, exchange: &mut Exchange) {
        if exchange.is_failed() {
            if let Err(e) = coordinator.compensate(exchange) {
                crate::camel_warn!("saga", "Error compensating saga {}: {}", coordinator.id(), e);
            }
        } else if self.completion_mode == SagaCompletionMode::Auto {
            if let Err(e) = coordinator.complete(exchange) {
                exchange.set_exception(e);
            }
        }
    }
}

impl Processor for SagaProcessor {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        let current = current_coordinator(exchange, self.service.as_ref())?;
        match self.propagation {
            SagaPropagation::Required => match current {
                Some(c) => self.join(c, exchange),
                None => self.start_new(exchange, false),
            },
            SagaPropagation::RequiresNew => self.start_new(exchange, true),
            SagaPropagation::Mandatory => match current {
                Some(c) => self.join(c, exchange),
                None => Err(CamelError::new(kinds::CAMEL_EXCHANGE, "Message cannot proceed: no saga in progress (propagation MANDATORY)")),
            },
            SagaPropagation::Supports => match current {
                Some(c) => self.join(c, exchange),
                None => {
                    self.run_child(exchange);
                    Ok(())
                }
            },
            SagaPropagation::NotSupported => {
                let saved = exchange.message.headers.remove(SAGA_LONG_RUNNING_ACTION);
                self.run_child(exchange);
                if let Some(s) = saved {
                    exchange.set_header(SAGA_LONG_RUNNING_ACTION, s);
                }
                Ok(())
            }
            SagaPropagation::Never => match current {
                Some(c) => Err(CamelError::new(
                    kinds::CAMEL_EXCHANGE,
                    format!("Message cannot proceed: a saga is already in progress ({}) and propagation is NEVER", c.id()),
                )),
                None => {
                    self.run_child(exchange);
                    Ok(())
                }
            },
        }
    }
}

// ---------------------------------------------------------------------------
// saga: component
// ---------------------------------------------------------------------------

/// The `saga:` component: `saga:complete` and `saga:compensate` complete or compensate the
/// saga the exchange is bound to (the `Long-Running-Action` header), using the context's
/// saga service.
#[derive(Default)]
pub struct SagaComponent;

#[derive(Clone, Copy, PartialEq)]
enum SagaAction {
    Complete,
    Compensate,
}

impl Component for SagaComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, _params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let action = match remaining.trim_matches('/') {
            "complete" | "COMPLETE" => SagaAction::Complete,
            "compensate" | "COMPENSATE" => SagaAction::Compensate,
            other => return Err(CamelError::illegal_argument(format!("Unsupported saga action: {} (use complete or compensate)", other))),
        };
        Ok(Arc::new(SagaEndpoint { uri: uri.to_string(), ctx: ctx.downgrade(), action }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct SagaEndpoint {
    uri: String,
    ctx: WeakContext,
    action: SagaAction,
}

impl Endpoint for SagaEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(SagaProducer { ctx: self.ctx.clone(), action: self.action }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct SagaProducer {
    ctx: WeakContext,
    action: SagaAction,
}

impl Service for SagaProducer {}

impl Processor for SagaProducer {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        let ctx = match exchange.context.clone().or_else(|| self.ctx.upgrade()) {
            Some(c) => c,
            None => return Err(CamelError::illegal_state("CamelContext is gone")),
        };
        let what = if self.action == SagaAction::Complete { "complete" } else { "compensate" };
        let service = ctx.saga_service();
        let coordinator = current_coordinator(exchange, service.as_ref())?.ok_or_else(|| {
            CamelError::illegal_state(format!("Current exchange is not bound to a saga context: cannot {}", what))
        })?;
        match self.action {
            SagaAction::Complete => coordinator.complete(exchange),
            SagaAction::Compensate => coordinator.compensate(exchange),
        }
    }
}

/// Register the `saga:` component (called for every new context).
pub fn register(ctx: &CamelContext) {
    ctx.add_component_factory("saga", || Arc::new(SagaComponent));
}

// ---------------------------------------------------------------------------
// shared helpers for coordinators
// ---------------------------------------------------------------------------

/// Send a finalization (compensation/completion) message to `uri`: an empty body with
/// the `Long-Running-Action` header and the step options as headers. Returns an error
/// if sending fails or the exchange fails.
pub fn send_finalization(ctx: &CamelContext, uri: &str, saga_id: &str, values: &BTreeMap<String, Value>) -> Result<()> {
    let producer = ctx.get_producer(uri)?;
    let mut ex = Exchange::new(Some(ctx.clone()));
    ex.set_header(SAGA_LONG_RUNNING_ACTION, saga_id.to_string());
    for (k, v) in values {
        ex.set_header(k.clone(), v.clone());
    }
    producer.process(&mut ex)?;
    match ex.exception.take() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

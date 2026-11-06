//! Generic component / endpoint / producer / polling-consumer scaffolding shared by all
//! cloud components. A component supplies a [`CloudService`] (its parsed configuration plus
//! the producer and poll logic); this module adapts it to the cameleer-core SPI.

use std::any::Any;
use std::sync::Arc;

use cameleer_core::component::scheduled::{ScheduledPollConfig, ScheduledPollConsumer};
use cameleer_core::context::{CamelContext, WeakContext};
use cameleer_core::error::{kinds as core_kinds, CamelError, Result};
use cameleer_core::message::{props, Exchange};
use cameleer_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use cameleer_core::util::Parameters;

use crate::util::Opts;

/// Behaviour of one cloud endpoint.
pub trait CloudService: Send + Sync + 'static {
    /// Producer: perform the operation for an exchange.
    fn produce(&self, ex: &mut Exchange) -> Result<()>;
    /// Consumer: one poll; feed exchanges to `processor`, return how many were processed.
    fn poll(&self, _ctx: Option<CamelContext>, _processor: &dyn Processor, _logger: &str) -> Result<usize> {
        Err(CamelError::new(core_kinds::UNSUPPORTED_OPERATION, "This endpoint does not support consumers"))
    }
    /// Scheduling of the consumer (None = no consumer support).
    fn scheduler(&self) -> Option<&ScheduledPollConfig> {
        None
    }
    /// A consumer that is not a scheduled poller (e.g. a streaming consumer); `None` (default)
    /// uses [`CloudService::scheduler`] + [`CloudService::poll`].
    fn custom_consumer(&self, _ctx: &WeakContext, _uri: &str, _processor: &Arc<dyn Processor>) -> Option<Result<Arc<dyn Consumer>>> {
        None
    }
    /// Called when a producer or consumer starts (auto-create resources...).
    fn on_start(&self) -> Result<()> {
        Ok(())
    }
    /// Whether this service is a chat model with tool calling (see [`CloudService::chat`]).
    fn supports_chat(&self) -> bool {
        false
    }
    /// One model turn in the neutral form (used by the `agent` step and the `tags` tool loop).
    fn chat(&self, _request: &cameleer_core::ai::ChatRequest) -> Result<cameleer_core::ai::ChatResponse> {
        Err(CamelError::new(core_kinds::UNSUPPORTED_OPERATION, "This endpoint is not a chat model"))
    }
    /// The configured system message, used when a request has none.
    fn system_message(&self) -> Option<String> {
        None
    }
    /// Called when a producer or consumer stops (flush pending work...).
    fn on_stop(&self) -> Result<()> {
        Ok(())
    }
}

/// Factory creating a service from the endpoint `remaining` path and options.
pub type ServiceFactory = fn(&str, &mut Opts) -> Result<Arc<dyn CloudService>>;

/// A component for one scheme.
pub struct CloudComponent {
    scheme: &'static str,
    factory: ServiceFactory,
}

impl CloudComponent {
    pub fn new(scheme: &'static str, factory: ServiceFactory) -> CloudComponent {
        CloudComponent { scheme, factory }
    }
}

impl Component for CloudComponent {
    fn create_endpoint(&self, context: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let mut opts = Opts::new(params, context, self.scheme);
        let svc = (self.factory)(remaining.trim_start_matches('/'), &mut opts)?;
        // chat models take the tool options (Camel's names on openai/llm)
        let tools = if svc.supports_chat() { Some(cameleer_core::ai::ToolLoopOptions::take(params)?) } else { None };
        Ok(Arc::new(CloudEndpoint { uri: uri.to_string(), ctx: context.downgrade(), svc, tools }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// An endpoint of a cloud component.
pub struct CloudEndpoint {
    uri: String,
    ctx: WeakContext,
    svc: Arc<dyn CloudService>,
    tools: Option<cameleer_core::ai::ToolLoopOptions>,
}

/// A chat service as a neutral chat model.
struct ServiceChat(Arc<dyn CloudService>);

impl cameleer_core::ai::ChatModel for ServiceChat {
    fn chat(&self, request: &cameleer_core::ai::ChatRequest) -> Result<cameleer_core::ai::ChatResponse> {
        if request.system.is_none() {
            if let Some(s) = self.0.system_message() {
                let mut r = request.clone();
                r.system = Some(s);
                return self.0.chat(&r);
            }
        }
        self.0.chat(request)
    }
}

impl CloudEndpoint {
    /// The service (for downcasting in tests via [`CloudEndpoint::service`]).
    pub fn service(&self) -> &Arc<dyn CloudService> {
        &self.svc
    }
}

impl Endpoint for CloudEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(CloudProducer { svc: self.svc.clone(), tools: self.tools.clone() }))
    }
    fn chat_model(&self) -> Option<Arc<dyn cameleer_core::ai::ChatModel>> {
        if !self.svc.supports_chat() {
            return None;
        }
        let base: Arc<dyn cameleer_core::ai::ChatModel> = Arc::new(ServiceChat(self.svc.clone()));
        match &self.tools {
            Some(t) => t.wrap(base).ok(),
            None => Some(base),
        }
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        if let Some(c) = self.svc.custom_consumer(&self.ctx, &self.uri, &processor) {
            return c;
        }
        let cfg = self
            .svc
            .scheduler()
            .cloned()
            .ok_or_else(|| CamelError::new(core_kinds::UNSUPPORTED_OPERATION, format!("Endpoint {} does not support consumers", self.uri)))?;
        let svc = self.svc.clone();
        let ctx = self.ctx.clone();
        let logger = self.uri.clone();
        let poll = ScheduledPollConsumer::new(self.uri.clone(), cfg, move || svc.poll(ctx.upgrade(), processor.as_ref(), &logger));
        Ok(Arc::new(CloudConsumer { poll, svc: self.svc.clone() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct CloudProducer {
    svc: Arc<dyn CloudService>,
    tools: Option<cameleer_core::ai::ToolLoopOptions>,
}

impl Service for CloudProducer {
    fn start(&self) -> Result<()> {
        self.svc.on_start()
    }
    fn stop(&self) -> Result<()> {
        self.svc.on_stop()
    }
}

impl Processor for CloudProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        // with tags (or a recording) the chat goes through the neutral model and the tool loop;
        // an object body is still sent to the API as it is
        if let Some(t) = self.tools.as_ref().filter(|t| t.is_active()) {
            if !matches!(ex.body(), cameleer_core::value::Value::Object(_)) {
                let model: Arc<dyn cameleer_core::ai::ChatModel> = Arc::new(ServiceChat(self.svc.clone()));
                cameleer_core::ai::chat_exchange(ex, model, None, t)?;
                return Ok(());
            }
        }
        self.svc.produce(ex)
    }
}

/// A scheduled polling consumer.
pub struct CloudConsumer {
    poll: ScheduledPollConsumer,
    svc: Arc<dyn CloudService>,
}

impl CloudConsumer {
    /// Run one poll now.
    pub fn poll_once(&self) -> Result<usize> {
        self.poll.poll_once()
    }
}

impl Service for CloudConsumer {
    fn start(&self) -> Result<()> {
        self.svc.on_start()?;
        self.poll.start()
    }
    fn stop(&self) -> Result<()> {
        self.poll.stop()?;
        self.svc.on_stop()
    }
}

/// Set the Camel batch properties of a consumed exchange.
pub fn set_batch(ex: &mut Exchange, index: usize, size: usize) {
    ex.set_property(props::BATCH_INDEX, index as i64);
    ex.set_property(props::BATCH_SIZE, size as i64);
    ex.set_property(props::BATCH_COMPLETE, index + 1 == size);
}

/// Process a consumed exchange; true when it completed without exception.
pub fn process(logger: &str, processor: &dyn Processor, ex: &mut Exchange) -> bool {
    cameleer_core::component::support::process_consumer_exchange(logger, processor, ex)
}

/// Resolve the operation: header, then endpoint option, then default.
pub fn operation(ex: &Exchange, header: &str, configured: &Option<String>, default: &str) -> String {
    crate::util::header_str(ex, header).or_else(|| configured.clone()).unwrap_or_else(|| default.to_string())
}

/// Unsupported operation error.
pub fn unsupported(component: &str, op: &str) -> CamelError {
    CamelError::illegal_argument(format!("Unsupported {} operation: {}", component, op))
}

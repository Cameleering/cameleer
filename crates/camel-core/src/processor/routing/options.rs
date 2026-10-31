//! Generic EIP options shared by several routing EIPs (parallelProcessing,
//! aggregationStrategy, stopOnException, timeout, ...), and the [`Deferred`]
//! wrapper the fluent DSL uses to configure non-block EIPs after they were added.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::error::Result;
use crate::model::{ProcessorDefinition, RouteContext};
use crate::spi::Processor;

use super::support::StrategyDefinition;

/// An option that can be applied to a routing EIP definition.
#[derive(Clone, Debug)]
pub enum EipOption {
    ParallelProcessing(bool),
    Streaming(bool),
    StopOnException(bool),
    ShareUnitOfWork(bool),
    /// Timeout (split/multicast/recipientList: parallel timeout; resequence: batch/stream timeout).
    Timeout(Duration),
    AggregationStrategy(StrategyDefinition),
    /// Delimiter (split string bodies, recipientList/routingSlip/dynamicRouter uris).
    Delimiter(String),
    IgnoreInvalidEndpoints(bool),
    /// Copy option (loop: copy the input per iteration; wireTap: copy the exchange).
    Copy(bool),
    AggregateOnException(bool),
    /// Size of the thread pool used for parallel/asynchronous processing.
    PoolSize(usize),
}

impl EipOption {
    /// The Camel name of the option (for error messages).
    pub fn name(&self) -> &'static str {
        match self {
            EipOption::ParallelProcessing(_) => "parallelProcessing",
            EipOption::Streaming(_) => "streaming",
            EipOption::StopOnException(_) => "stopOnException",
            EipOption::ShareUnitOfWork(_) => "shareUnitOfWork",
            EipOption::Timeout(_) => "timeout",
            EipOption::AggregationStrategy(_) => "aggregationStrategy",
            EipOption::Delimiter(_) => "delimiter",
            EipOption::IgnoreInvalidEndpoints(_) => "ignoreInvalidEndpoints",
            EipOption::Copy(_) => "copy",
            EipOption::AggregateOnException(_) => "aggregateOnException",
            EipOption::PoolSize(_) => "executorService",
        }
    }
}

/// Implemented by definitions that accept generic [`EipOption`]s.
pub trait EipOptions {
    /// Apply the option; return false if this EIP does not support it.
    fn apply_option(&mut self, option: &EipOption) -> bool;
}

/// Type-erased access to a deferred definition's options.
pub trait OptionTarget: Send + Sync {
    fn apply(&self, option: &EipOption) -> bool;
    /// Run `f` with the inner definition as `Any` (for EIP specific options).
    fn with_any(&self, f: &mut dyn FnMut(&mut dyn std::any::Any));
}

/// Wraps a non-block definition behind a shared handle so that the fluent DSL can
/// keep configuring it (e.g. `.recipient_list(e).parallel_processing()`) after it
/// has been added to its parent block.
pub struct Deferred<T: ProcessorDefinition + EipOptions> {
    pub inner: Arc<Mutex<T>>,
    name: String,
}

impl<T: ProcessorDefinition + EipOptions> Deferred<T> {
    pub fn new(def: T) -> Self {
        let name = def.name().to_string();
        Deferred { inner: Arc::new(Mutex::new(def)), name }
    }
}

impl<T: ProcessorDefinition + EipOptions> OptionTarget for Mutex<T> {
    fn apply(&self, option: &EipOption) -> bool {
        self.lock().unwrap().apply_option(option)
    }
    fn with_any(&self, f: &mut dyn FnMut(&mut dyn std::any::Any)) {
        let mut g = self.lock().unwrap();
        f(g.as_any_mut())
    }
}

impl<T: ProcessorDefinition + EipOptions> ProcessorDefinition for Deferred<T> {
    fn name(&self) -> &str {
        &self.name
    }
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        self.inner.lock().unwrap().create_processor(rc)
    }
    fn id(&self) -> Option<&str> {
        // the id lives inside the mutex; it cannot be borrowed out
        None
    }
    fn set_id(&mut self, id: String) {
        self.inner.lock().unwrap().set_id(id)
    }
    fn label(&self) -> String {
        self.inner.lock().unwrap().label()
    }
    fn wrap_in_channel(&self) -> bool {
        self.inner.lock().unwrap().wrap_in_channel()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Settings shared by the EIPs built on the multicast engine.
#[derive(Clone, Debug, Default)]
pub struct MulticastSettings {
    pub parallel_processing: bool,
    pub streaming: bool,
    pub stop_on_exception: bool,
    pub share_unit_of_work: bool,
    pub timeout: Option<Duration>,
    pub strategy: Option<StrategyDefinition>,
    pub pool_size: Option<usize>,
}

impl MulticastSettings {
    /// Apply a generic option; returns false if not a multicast option.
    pub fn apply(&mut self, option: &EipOption) -> bool {
        match option {
            EipOption::ParallelProcessing(b) => self.parallel_processing = *b,
            EipOption::Streaming(b) => self.streaming = *b,
            EipOption::StopOnException(b) => self.stop_on_exception = *b,
            EipOption::ShareUnitOfWork(b) => self.share_unit_of_work = *b,
            EipOption::Timeout(d) => self.timeout = Some(*d),
            EipOption::AggregationStrategy(s) => self.strategy = Some(s.clone()),
            EipOption::PoolSize(n) => self.pool_size = Some(*n),
            _ => return false,
        }
        true
    }
}

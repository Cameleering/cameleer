//! Core service provider interfaces: Processor, Expression, Predicate, Endpoint,
//! Producer, Consumer, Component, Language, Service.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::context::CamelContext;
use crate::error::{CamelError, Result};
use crate::message::Exchange;
use crate::util::Parameters;
use crate::value::Value;

/// A processor operates on an exchange. Returning `Err` marks the exchange as failed
/// (the routing engine stores the error in `exchange.exception`).
pub trait Processor: Send + Sync {
    fn process(&self, exchange: &mut Exchange) -> Result<()>;

    /// [`Processor::process`] inside a timed sequence (route statistics): `start` is a
    /// [`crate::clock::ticks`] reading taken just before this call, or 0. Returns the
    /// reading taken at the end, or 0 if this processor took none. Instrumented steps of a
    /// pipeline pass the end of one step on as the start of the next, so the clock is read
    /// once per step instead of twice. Wrappers forward it; the default does not time.
    fn process_timed(&self, exchange: &mut Exchange, _start: u64) -> (Result<()>, u64) {
        (self.process(exchange), 0)
    }
}

impl<F> Processor for F
where
    F: Fn(&mut Exchange) -> Result<()> + Send + Sync,
{
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        self(exchange)
    }
}

impl Processor for Arc<dyn Processor> {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        (**self).process(exchange)
    }
    fn process_timed(&self, exchange: &mut Exchange, start: u64) -> (Result<()>, u64) {
        (**self).process_timed(exchange, start)
    }
}

/// Lifecycle for services (consumers, producers, stateful processors).
pub trait Service: Send + Sync {
    fn start(&self) -> Result<()> {
        Ok(())
    }
    fn stop(&self) -> Result<()> {
        Ok(())
    }
    /// The suspend/resume view of this service (Camel `SuspendableService`), if supported.
    /// Consumers returning `Some` are suspended in place by `CamelContext::suspend_route`
    /// (threads, sockets and queues are kept); others are stopped instead, as in Camel.
    fn as_suspendable(&self) -> Option<&dyn SuspendableService> {
        None
    }
}

/// A service that can be suspended and resumed without being stopped (Camel
/// `SuspendableService`). A suspended consumer creates no new exchanges: seda leaves
/// messages queued, timer/scheduler/cron skip fires, the file consumer skips polls and the
/// HTTP consumer answers `503 Service Unavailable`.
pub trait SuspendableService: Send + Sync {
    fn suspend(&self) -> Result<()>;
    fn resume(&self) -> Result<()>;
    fn is_suspended(&self) -> bool;
}

/// An expression evaluates to a value against an exchange.
pub trait Expression: Send + Sync {
    fn evaluate(&self, exchange: &Exchange) -> Result<Value>;
}

impl<F> Expression for F
where
    F: Fn(&Exchange) -> Result<Value> + Send + Sync,
{
    fn evaluate(&self, exchange: &Exchange) -> Result<Value> {
        self(exchange)
    }
}

/// A predicate evaluates to true/false against an exchange.
pub trait Predicate: Send + Sync {
    fn matches(&self, exchange: &Exchange) -> Result<bool>;
}

/// Wrap a closure as a predicate.
pub struct FnPredicate<F>(pub F);
impl<F> Predicate for FnPredicate<F>
where
    F: Fn(&Exchange) -> Result<bool> + Send + Sync,
{
    fn matches(&self, exchange: &Exchange) -> Result<bool> {
        (self.0)(exchange)
    }
}

/// Adapts an expression to a predicate using Camel's `ObjectHelper.evaluateValuePredicate`
/// ([`Value::is_predicate_true`](crate::value::Value::is_predicate_true)).
pub struct ExpressionPredicate(pub Arc<dyn Expression>);
impl Predicate for ExpressionPredicate {
    fn matches(&self, exchange: &Exchange) -> Result<bool> {
        Ok(self.0.evaluate(exchange)?.is_predicate_true())
    }
}

/// Adapts a predicate to an expression returning a boolean value.
pub struct PredicateExpression(pub Arc<dyn Predicate>);
impl Expression for PredicateExpression {
    fn evaluate(&self, exchange: &Exchange) -> Result<Value> {
        Ok(Value::Bool(self.0.matches(exchange)?))
    }
}

/// An expression language (simple, constant, header, jsonpath, tokenize, ...).
pub trait Language: Send + Sync {
    fn name(&self) -> &str;
    /// Create an expression. `options` are language specific settings
    /// (e.g. `token` for tokenize, `resultType` for jsonpath).
    fn create_expression(&self, text: &str, options: &BTreeMap<String, String>) -> Result<Arc<dyn Expression>>;
    /// Create a predicate. Defaults to `ObjectHelper.evaluateValuePredicate` of the expression.
    fn create_predicate(&self, text: &str, options: &BTreeMap<String, String>) -> Result<Arc<dyn Predicate>> {
        Ok(Arc::new(ExpressionPredicate(self.create_expression(text, options)?)))
    }
}

/// A producer sends exchanges to an endpoint.
pub trait Producer: Processor + Service {}
impl<T: Processor + Service> Producer for T {}

/// A consumer receives messages from an endpoint and feeds them to a processor.
pub trait Consumer: Service {}
impl<T: Service> Consumer for T {}

/// A consumer that is polled for messages (Camel `PollingConsumer`).
pub trait PollingConsumer: Service {
    /// Receive a message, waiting at most `timeout` (None = no wait, Some(0) = no wait).
    fn receive(&self, timeout: Option<std::time::Duration>) -> Result<Option<Exchange>>;
}

/// An endpoint is a named message channel created by a component from a URI.
pub trait Endpoint: Send + Sync + Any {
    /// The endpoint uri (as given, after placeholder resolution).
    fn uri(&self) -> &str;
    /// Create a producer for sending to this endpoint.
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Err(CamelError::new(
            crate::error::kinds::UNSUPPORTED_OPERATION,
            format!("Endpoint {} does not support producers", self.uri()),
        ))
    }
    /// Create a consumer that feeds received exchanges into `processor`.
    fn create_consumer(&self, _processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        Err(CamelError::new(
            crate::error::kinds::UNSUPPORTED_OPERATION,
            format!("Endpoint {} does not support consumers", self.uri()),
        ))
    }
    /// The chat model behind this endpoint, for model components that support tool calling
    /// (used by the `agent` step).
    #[cfg(feature = "agent")]
    fn chat_model(&self) -> Option<Arc<dyn crate::ai::ChatModel>> {
        None
    }
    /// Create a polling consumer (used by pollEnrich and ConsumerTemplate).
    fn create_polling_consumer(&self) -> Result<Arc<dyn PollingConsumer>> {
        Err(CamelError::new(
            crate::error::kinds::UNSUPPORTED_OPERATION,
            format!("Endpoint {} does not support polling consumers", self.uri()),
        ))
    }
    /// Whether the endpoint can be shared (cached) - default true.
    fn is_singleton(&self) -> bool {
        true
    }
    /// Lifecycle hooks.
    fn start(&self) -> Result<()> {
        Ok(())
    }
    fn stop(&self) -> Result<()> {
        Ok(())
    }
    /// For downcasting to concrete endpoint types (e.g. MockEndpoint).
    fn as_any(&self) -> &dyn Any;
}

/// A component is a factory of endpoints for a URI scheme.
pub trait Component: Send + Sync {
    /// Create an endpoint. `remaining` is the part after `scheme:` (without `//` and
    /// the query). Consume the options you understand from `params`; any leftover
    /// options cause endpoint resolution to fail (unless `lenient_properties`).
    fn create_endpoint(
        &self,
        context: &CamelContext,
        uri: &str,
        remaining: &str,
        params: &mut Parameters,
    ) -> Result<Arc<dyn Endpoint>>;

    /// If true, unknown endpoint options are allowed (e.g. http passes them as query).
    fn lenient_properties(&self) -> bool {
        false
    }
    fn start(&self) -> Result<()> {
        Ok(())
    }
    fn stop(&self) -> Result<()> {
        Ok(())
    }
    fn as_any(&self) -> &dyn Any;
}

/// Strategy for merging exchanges (aggregate, split, multicast, enrich...).
/// `old` is None on the first invocation. Return the aggregated exchange.
pub trait AggregationStrategy: Send + Sync {
    fn aggregate(&self, old: Option<Exchange>, new: Exchange) -> Result<Exchange>;
    /// Called when an aggregation group times out (optional).
    fn timeout(&self, _old: &Exchange, _index: usize, _total: usize, _timeout_ms: u64) {}
    /// Called when the aggregation completes (optional), may modify the result.
    fn on_completion(&self, _exchange: &mut Exchange) {}
    /// Whether `aggregate` never returns an error. The aggregate EIP then moves the group
    /// out of its repository instead of copying it for every exchange (a failing strategy
    /// must leave the group in the repository untouched, as in Camel).
    fn never_fails(&self) -> bool {
        false
    }
    /// Aggregate `new` into the group in place (`None` for a new group). The default calls
    /// [`AggregationStrategy::aggregate`]; strategies that only add to the group override it
    /// so the group exchange is not moved out and back for every exchange. The aggregate
    /// EIP only uses it when [`AggregationStrategy::never_fails`] is true.
    fn aggregate_into(&self, group: &mut Option<Exchange>, new: Exchange) -> Result<()> {
        let old = group.take();
        *group = Some(self.aggregate(old, new)?);
        Ok(())
    }
}

impl<F> AggregationStrategy for F
where
    F: Fn(Option<Exchange>, Exchange) -> Result<Exchange> + Send + Sync,
{
    fn aggregate(&self, old: Option<Exchange>, new: Exchange) -> Result<Exchange> {
        self(old, new)
    }
}

/// Notified of context/route/exchange events (Camel `EventNotifier`).
pub trait EventNotifier: Send + Sync {
    fn notify(&self, event: &crate::context::CamelEvent);
}

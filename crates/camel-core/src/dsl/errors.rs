//! Error handling and flow control DSL methods.
//!
//! ```ignore
//! ctx.add_routes(|b| {
//!     b.error_handler(dead_letter_channel("mock:dead").maximum_redeliveries(2).redelivery_delay(0));
//!     b.on_exception(&["IOException"]).handled(true).to("mock:io").end();
//!     b.from("direct:a")
//!         .do_try()
//!             .to("mock:try")
//!         .do_catch(&["IllegalArgumentException"])
//!             .to("mock:catch")
//!         .do_finally()
//!             .to("mock:finally")
//!         .end_do_try()
//!         .to("mock:result");
//! })?;
//! ```

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::builder::IntoExpression;
use crate::error::CamelError;
use crate::log::Level;
use crate::model::RouteDefinition;
use crate::processor::errors::circuit_breaker::{CircuitBreakerConfig, CircuitBreakerDefinition};
use crate::processor::errors::do_try::DoTryDefinition;
use crate::processor::errors::handler::ErrorHandlerBuilder;
use crate::processor::errors::hook::{builder_ext, RoutePolicies, RoutePolicyRef, ROUTE_ERROR_HANDLER, ROUTE_POLICIES};
use crate::processor::errors::idempotent::{IdempotentConsumerDefinition, IdempotentRepository};
use crate::processor::errors::intercept::{InterceptDefinition, InterceptKind};
use crate::processor::errors::misc::{
    Policy, PolicyDefinition, RollbackDefinition, RollbackMode, ThreadsDefinition, ThrowExceptionDefinition, TransactedDefinition,
    ValidateDefinition,
};
use crate::processor::errors::on_completion::{OnCompletionDefinition, OnCompletionMode};
use crate::processor::errors::on_exception::OnExceptionDefinition;
use crate::processor::errors::route_policy::RoutePolicy;
use crate::processor::errors::throttle::{DelayDefinition, SampleDefinition, ThrottleConfig, ThrottleDefinition};
use crate::processor::errors::IntoBoolOrPredicate;
use crate::spi::Processor;

use super::RouteBuilder;

const LAST_FLOW: &str = "camel.errors.lastFlow";

/// Handle to the most recently added throttle/delay, so options can follow it.
enum LastFlow {
    Throttle(Arc<Mutex<ThrottleConfig>>),
    Delay(Arc<Mutex<bool>>),
}

// ---------------------------------------------------------------------------
// Builder-scoped configuration
// ---------------------------------------------------------------------------

impl RouteBuilder {
    /// Error handler for all routes of this builder (unless a route sets its own).
    pub fn error_handler(&mut self, eh: ErrorHandlerBuilder) -> &mut Self {
        builder_ext(self).shared.lock().unwrap().error_handler = Some(eh);
        self
    }

    fn scoped_block(&mut self, name: &str, def: Box<dyn crate::model::ProcessorDefinition>) -> &mut RouteDefinition {
        let ext = builder_ext(self);
        let mut r = RouteDefinition::new(name);
        r.push_block(def);
        ext.pending.push(r);
        ext.pending.last_mut().unwrap()
    }

    /// Builder-scoped `onException` clause (applies to all routes of this builder).
    /// Returns the clause block: configure it and add outputs with the normal DSL.
    pub fn on_exception(&mut self, exceptions: &[&str]) -> &mut RouteDefinition {
        self.scoped_block("onException", Box::new(OnExceptionDefinition::new(exceptions)))
    }

    /// Builder-scoped `onCompletion` (used by routes without a route-scoped onCompletion).
    pub fn on_completion(&mut self) -> &mut RouteDefinition {
        self.scoped_block("onCompletion", Box::new(OnCompletionDefinition::default()))
    }

    /// Intercept every processing step of this builder's routes.
    pub fn intercept(&mut self) -> &mut RouteDefinition {
        self.scoped_block("intercept", Box::new(InterceptDefinition::new(InterceptKind::All)))
    }

    /// Intercept exchanges consumed from endpoints matching `pattern` (`*` = all).
    pub fn intercept_from(&mut self, pattern: impl Into<String>) -> &mut RouteDefinition {
        self.scoped_block("interceptFrom", Box::new(InterceptDefinition::new(InterceptKind::From(pattern.into()))))
    }

    /// Intercept sends to endpoints matching `pattern` (e.g. `mock:*`).
    pub fn intercept_send_to_endpoint(&mut self, pattern: impl Into<String>) -> &mut RouteDefinition {
        self.scoped_block(
            "interceptSendToEndpoint",
            Box::new(InterceptDefinition::new(InterceptKind::SendToEndpoint { pattern: pattern.into(), skip_send_to_original_endpoint: false })),
        )
    }
}

// ---------------------------------------------------------------------------
// Route-scoped configuration and EIPs
// ---------------------------------------------------------------------------

impl RouteDefinition {
    fn with_block<T: 'static>(&mut self, what: &str, f: impl FnOnce(&mut T)) -> &mut Self {
        match self.unwind_to_block::<T>() {
            Some(b) => f(b),
            None => self.add_build_error(CamelError::illegal_state(format!("{} must be used inside its block", what))),
        }
        self
    }

    fn with_on_exception(&mut self, what: &str, f: impl FnOnce(&mut OnExceptionDefinition)) -> &mut Self {
        self.with_block::<OnExceptionDefinition>(&format!("{} (onException option)", what), f)
    }

    // --- error handler / onException ---

    /// Route-level error handler (overrides builder and context level).
    pub fn error_handler(&mut self, eh: ErrorHandlerBuilder) -> &mut Self {
        self.extensions.insert(ROUTE_ERROR_HANDLER.to_string(), Box::new(eh));
        self
    }

    /// Route-scoped `onException` clause block (close with `end()`).
    pub fn on_exception(&mut self, exceptions: &[&str]) -> &mut Self {
        self.push_block(Box::new(OnExceptionDefinition::new(exceptions)))
    }

    /// Mark the exception as handled (`true`/`false` or a predicate).
    pub fn handled(&mut self, h: impl IntoBoolOrPredicate) -> &mut Self {
        let h = h.into_bool_or_predicate();
        self.with_on_exception("handled", |d| d.handled = Some(h))
    }

    /// Continue routing as if the exception did not happen (`true`/`false` or a predicate).
    pub fn continued(&mut self, c: impl IntoBoolOrPredicate) -> &mut Self {
        let c = c.into_bool_or_predicate();
        self.with_on_exception("continued", |d| d.continued = Some(c))
    }

    /// Additional condition for the innermost onException / doCatch / onCompletion /
    /// intercept block. (Camel's `when` on interceptors is spelled `on_when` here.)
    pub fn on_when(&mut self, p: impl IntoExpression) -> &mut Self {
        let p = p.into_expression();
        let pos = self.stack.iter().rposition(|b| {
            let a = b.as_any();
            a.is::<OnExceptionDefinition>() || a.is::<DoTryDefinition>() || a.is::<OnCompletionDefinition>() || a.is::<InterceptDefinition>()
        });
        let Some(pos) = pos else {
            self.add_build_error(CamelError::illegal_state("on_when must be used inside onException, doCatch, onCompletion or intercept"));
            return self;
        };
        while self.stack.len() > pos + 1 {
            self.end();
        }
        let top = self.stack.last_mut().unwrap().as_any_mut();
        if let Some(d) = top.downcast_mut::<OnExceptionDefinition>() {
            d.on_when = Some(p);
        } else if let Some(d) = top.downcast_mut::<OnCompletionDefinition>() {
            d.on_when = Some(p);
        } else if let Some(d) = top.downcast_mut::<InterceptDefinition>() {
            d.on_when = Some(p);
        } else if let Some(d) = top.downcast_mut::<DoTryDefinition>() {
            if !d.set_on_when(p) {
                self.add_build_error(CamelError::illegal_state("on_when on doTry must follow do_catch"));
            }
        }
        self
    }

    /// Keep redelivering while the predicate is true (onException option).
    pub fn retry_while(&mut self, p: impl IntoExpression) -> &mut Self {
        let p = p.into_expression();
        self.with_on_exception("retry_while", |d| d.retry_while = Some(p))
    }

    pub fn maximum_redeliveries(&mut self, n: i32) -> &mut Self {
        self.with_on_exception("maximum_redeliveries", |d| d.policy.maximum_redeliveries = Some(n))
    }

    /// Redelivery delay in millis (onException option).
    pub fn redelivery_delay(&mut self, millis: u64) -> &mut Self {
        self.with_on_exception("redelivery_delay", |d| d.policy.redelivery_delay = Some(millis))
    }

    pub fn back_off_multiplier(&mut self, m: f64) -> &mut Self {
        self.with_on_exception("back_off_multiplier", |d| d.policy.back_off_multiplier = Some(m))
    }

    pub fn use_exponential_back_off(&mut self) -> &mut Self {
        self.with_on_exception("use_exponential_back_off", |d| d.policy.use_exponential_back_off = Some(true))
    }

    pub fn maximum_redelivery_delay(&mut self, millis: u64) -> &mut Self {
        self.with_on_exception("maximum_redelivery_delay", |d| d.policy.maximum_redelivery_delay = Some(millis))
    }

    pub fn use_collision_avoidance(&mut self) -> &mut Self {
        self.with_on_exception("use_collision_avoidance", |d| d.policy.use_collision_avoidance = Some(true))
    }

    pub fn delay_pattern(&mut self, pattern: impl Into<String>) -> &mut Self {
        let pattern = pattern.into();
        self.with_on_exception("delay_pattern", |d| d.policy.delay_pattern = Some(pattern))
    }

    pub fn retry_attempted_log_level(&mut self, l: Level) -> &mut Self {
        self.with_on_exception("retry_attempted_log_level", |d| d.policy.retry_attempted_log_level = Some(l))
    }

    pub fn retries_exhausted_log_level(&mut self, l: Level) -> &mut Self {
        self.with_on_exception("retries_exhausted_log_level", |d| d.policy.retries_exhausted_log_level = Some(l))
    }

    pub fn log_stack_trace(&mut self, b: bool) -> &mut Self {
        self.with_on_exception("log_stack_trace", |d| d.policy.log_stack_trace = Some(b))
    }

    pub fn log_exhausted(&mut self, b: bool) -> &mut Self {
        self.with_on_exception("log_exhausted", |d| d.policy.log_exhausted = Some(b))
    }

    /// Use the original input message in the onException outputs.
    pub fn use_original_message(&mut self) -> &mut Self {
        self.with_on_exception("use_original_message", |d| d.use_original_message = true)
    }

    /// Use the original input body in the onException outputs.
    pub fn use_original_body(&mut self) -> &mut Self {
        self.with_on_exception("use_original_body", |d| d.use_original_body = true)
    }

    /// Processor invoked before each redelivery (onException option).
    pub fn on_redelivery(&mut self, p: impl Processor + 'static) -> &mut Self {
        let p: Arc<dyn Processor> = Arc::new(p);
        self.with_on_exception("on_redelivery", |d| d.on_redelivery = Some(p))
    }

    /// Processor invoked each time the exception occurs (onException option).
    pub fn on_exception_occurred(&mut self, p: impl Processor + 'static) -> &mut Self {
        let p: Arc<dyn Processor> = Arc::new(p);
        self.with_on_exception("on_exception_occurred", |d| d.on_exception_occurred = Some(p))
    }

    // --- onCompletion ---

    /// Route-scoped `onCompletion` block (close with `end()`).
    pub fn on_completion(&mut self) -> &mut Self {
        self.push_block(Box::new(OnCompletionDefinition::default()))
    }

    /// Only run the onCompletion when the exchange completed successfully.
    pub fn on_complete_only(&mut self) -> &mut Self {
        self.with_block::<OnCompletionDefinition>("on_complete_only", |d| d.mode = OnCompletionMode::OnCompleteOnly)
    }

    /// Only run the onCompletion when the exchange failed.
    pub fn on_failure_only(&mut self) -> &mut Self {
        self.with_block::<OnCompletionDefinition>("on_failure_only", |d| d.mode = OnCompletionMode::OnFailureOnly)
    }

    /// Run the onCompletion before the consumer sends the response (can change it).
    pub fn mode_before_consumer(&mut self) -> &mut Self {
        self.with_block::<OnCompletionDefinition>("mode_before_consumer", |d| d.before_consumer = true)
    }

    /// Run the onCompletion after the consumer is done (default).
    pub fn mode_after_consumer(&mut self) -> &mut Self {
        self.with_block::<OnCompletionDefinition>("mode_after_consumer", |d| d.before_consumer = false)
    }

    // --- intercept ---

    /// Do not send to the intercepted endpoint (interceptSendToEndpoint option).
    pub fn skip_send_to_original_endpoint(&mut self) -> &mut Self {
        self.with_block::<InterceptDefinition>("skip_send_to_original_endpoint", |d| {
            if let InterceptKind::SendToEndpoint { skip_send_to_original_endpoint, .. } = &mut d.kind {
                *skip_send_to_original_endpoint = true;
            }
        })
    }

    // --- doTry ---

    /// Open a `doTry` block.
    pub fn do_try(&mut self) -> &mut Self {
        self.push_block(Box::new(DoTryDefinition::default()))
    }

    /// Start a `doCatch` clause for the given exception kinds (empty = everything).
    pub fn do_catch(&mut self, exceptions: &[&str]) -> &mut Self {
        self.with_block::<DoTryDefinition>("do_catch", |d| d.add_catch(exceptions))
    }

    /// Start the `doFinally` section.
    pub fn do_finally(&mut self) -> &mut Self {
        self.with_block::<DoTryDefinition>("do_finally", |d| d.start_finally())
    }

    /// Close the `doTry` block (closing any nested blocks first).
    pub fn end_do_try(&mut self) -> &mut Self {
        if self.unwind_to_block::<DoTryDefinition>().is_some() {
            self.end();
        } else {
            self.add_build_error(CamelError::illegal_state("end_do_try() without do_try()"));
        }
        self
    }

    // --- throttle / delay / sample ---

    /// Throttle to at most `maximum_requests` per `time_period`.
    pub fn throttle(&mut self, maximum_requests: usize, time_period: Duration) -> &mut Self {
        let d = ThrottleDefinition::new(maximum_requests, time_period);
        self.extensions.insert(LAST_FLOW.to_string(), Box::new(LastFlow::Throttle(d.config.clone())));
        self.add(Box::new(d))
    }

    /// Throttle per correlation key (each key gets its own window).
    pub fn throttle_correlated(&mut self, maximum_requests: usize, time_period: Duration, correlation: impl IntoExpression) -> &mut Self {
        let d = ThrottleDefinition::new(maximum_requests, time_period);
        d.config.lock().unwrap().correlation = Some(correlation.into_expression());
        self.extensions.insert(LAST_FLOW.to_string(), Box::new(LastFlow::Throttle(d.config.clone())));
        self.add(Box::new(d))
    }

    /// Reject exchanges exceeding the throttle rate with `ThrottlerRejectedExecutionException`.
    pub fn reject_execution(&mut self, reject: bool) -> &mut Self {
        match self.extensions.get(LAST_FLOW).and_then(|b| b.downcast_ref::<LastFlow>()) {
            Some(LastFlow::Throttle(c)) => c.lock().unwrap().reject_execution = reject,
            _ => self.add_build_error(CamelError::illegal_state("reject_execution must follow throttle")),
        }
        self
    }

    /// Async delayed flag for throttle/delay (accepted; delays run on the calling thread).
    pub fn async_delayed(&mut self, b: bool) -> &mut Self {
        match self.extensions.get(LAST_FLOW).and_then(|b| b.downcast_ref::<LastFlow>()) {
            Some(LastFlow::Throttle(c)) => c.lock().unwrap().async_delayed = b,
            Some(LastFlow::Delay(d)) => *d.lock().unwrap() = b,
            None => self.add_build_error(CamelError::illegal_state("async_delayed must follow throttle or delay")),
        }
        self
    }

    /// Delay the exchange by the expression's value in millis.
    pub fn delay(&mut self, millis: impl IntoExpression) -> &mut Self {
        let flag = Arc::new(Mutex::new(false));
        self.extensions.insert(LAST_FLOW.to_string(), Box::new(LastFlow::Delay(flag.clone())));
        self.add(Box::new(DelayDefinition { id: None, expression: millis.into_expression(), async_delayed: flag }))
    }

    /// Let one exchange through per period; others are stopped.
    pub fn sample(&mut self, period: Duration) -> &mut Self {
        self.add(Box::new(SampleDefinition { id: None, period: Some(period), message_frequency: None }))
    }

    /// Let every n-th exchange through; others are stopped.
    pub fn sample_every(&mut self, n: u64) -> &mut Self {
        self.add(Box::new(SampleDefinition { id: None, period: None, message_frequency: Some(n) }))
    }

    // --- idempotent consumer ---

    /// Open an idempotent consumer block keyed by `expression`.
    pub fn idempotent_consumer(&mut self, expression: impl IntoExpression, repository: Arc<dyn IdempotentRepository>) -> &mut Self {
        self.push_block(Box::new(IdempotentConsumerDefinition::new(expression.into_expression(), repository)))
    }

    /// Idempotent consumer option: add keys eagerly (default true).
    pub fn eager(&mut self, b: bool) -> &mut Self {
        self.with_block::<IdempotentConsumerDefinition>("eager", |d| d.eager = b)
    }

    /// Idempotent consumer option: skip duplicates (default true).
    pub fn skip_duplicate(&mut self, b: bool) -> &mut Self {
        self.with_block::<IdempotentConsumerDefinition>("skip_duplicate", |d| d.skip_duplicate = b)
    }

    /// Idempotent consumer option: remove keys of failed exchanges (default true).
    pub fn remove_on_failure(&mut self, b: bool) -> &mut Self {
        self.with_block::<IdempotentConsumerDefinition>("remove_on_failure", |d| d.remove_on_failure = b)
    }

    /// Idempotent consumer option: complete right after the block (default false).
    pub fn completion_eager(&mut self, b: bool) -> &mut Self {
        self.with_block::<IdempotentConsumerDefinition>("completion_eager", |d| d.completion_eager = b)
    }

    // --- circuit breaker ---

    /// Open a circuit breaker block (optionally with `on_fallback()`), closed with `end()`.
    pub fn circuit_breaker(&mut self) -> &mut Self {
        self.push_block(Box::new(CircuitBreakerDefinition::default()))
    }

    /// Start the fallback section of the circuit breaker.
    pub fn on_fallback(&mut self) -> &mut Self {
        self.with_block::<CircuitBreakerDefinition>("on_fallback", |d| {
            d.in_fallback = true;
            d.fallback.get_or_insert_with(Default::default);
        })
    }

    fn with_cb(&mut self, what: &str, f: impl FnOnce(&mut CircuitBreakerConfig)) -> &mut Self {
        self.with_block::<CircuitBreakerDefinition>(what, |d| f(&mut d.config))
    }

    /// Replace the whole circuit breaker configuration.
    pub fn circuit_breaker_config(&mut self, c: CircuitBreakerConfig) -> &mut Self {
        self.with_cb("circuit_breaker_config", |d| *d = c)
    }

    pub fn failure_rate_threshold(&mut self, percent: f64) -> &mut Self {
        self.with_cb("failure_rate_threshold", |c| c.failure_rate_threshold = percent)
    }

    pub fn minimum_number_of_calls(&mut self, n: usize) -> &mut Self {
        self.with_cb("minimum_number_of_calls", |c| c.minimum_number_of_calls = n)
    }

    pub fn sliding_window_size(&mut self, n: usize) -> &mut Self {
        self.with_cb("sliding_window_size", |c| c.sliding_window_size = n)
    }

    pub fn wait_duration_in_open_state(&mut self, d: Duration) -> &mut Self {
        self.with_cb("wait_duration_in_open_state", |c| c.wait_duration_in_open_state = d)
    }

    pub fn permitted_number_of_calls_in_half_open_state(&mut self, n: usize) -> &mut Self {
        self.with_cb("permitted_number_of_calls_in_half_open_state", |c| c.permitted_number_of_calls_in_half_open_state = n)
    }

    pub fn timeout_enabled(&mut self, b: bool) -> &mut Self {
        self.with_cb("timeout_enabled", |c| c.timeout_enabled = b)
    }

    pub fn timeout_duration(&mut self, d: Duration) -> &mut Self {
        self.with_cb("timeout_duration", |c| c.timeout_duration = d)
    }

    // --- misc EIPs ---

    /// Validate with a predicate (`PredicateValidationException` on failure).
    pub fn validate(&mut self, predicate: impl IntoExpression) -> &mut Self {
        self.add(Box::new(ValidateDefinition { id: None, predicate: predicate.into_expression() }))
    }

    /// Fail with `RollbackExchangeException`.
    pub fn rollback(&mut self, message: impl Into<String>) -> &mut Self {
        self.add(Box::new(RollbackDefinition { id: None, message: Some(message.into()), mode: RollbackMode::Throw }))
    }

    /// Mark the exchange rollback-only (stops routing without an exception).
    pub fn mark_rollback_only(&mut self) -> &mut Self {
        self.add(Box::new(RollbackDefinition { id: None, message: None, mode: RollbackMode::MarkRollbackOnly }))
    }

    /// Mark only the last transaction rollback-only (stops routing).
    pub fn mark_rollback_only_last(&mut self) -> &mut Self {
        self.add(Box::new(RollbackDefinition { id: None, message: None, mode: RollbackMode::MarkRollbackOnlyLast }))
    }

    /// Fail with an exception of the given kind (message is a Simple expression).
    pub fn throw_exception(&mut self, kind: impl Into<String>, message: impl Into<String>) -> &mut Self {
        self.add(Box::new(ThrowExceptionDefinition { id: None, kind: kind.into(), message: message.into() }))
    }

    /// Continue routing on a thread pool of `pool_size` threads (block; `end()` optional).
    pub fn threads(&mut self, pool_size: usize) -> &mut Self {
        self.push_block(Box::new(ThreadsDefinition { id: None, pool_size, thread_name: "Threads".to_string(), outputs: Default::default() }))
    }

    /// Mark the route as transacted (no-op marker).
    pub fn transacted(&mut self) -> &mut Self {
        self.add(Box::new(TransactedDefinition { id: None, reference: None }))
    }

    /// Mark the route as transacted with a named policy (no-op marker).
    pub fn transacted_ref(&mut self, policy: impl Into<String>) -> &mut Self {
        self.add(Box::new(TransactedDefinition { id: None, reference: Some(policy.into()) }))
    }

    /// Open a policy block whose processors are wrapped by `policy`.
    pub fn policy(&mut self, policy: Arc<dyn Policy>) -> &mut Self {
        self.push_block(Box::new(PolicyDefinition { id: None, policy: Some(policy), reference: None, outputs: Default::default() }))
    }

    /// Open a policy block using an `Arc<dyn Policy>` bound in the registry.
    pub fn policy_ref(&mut self, name: impl Into<String>) -> &mut Self {
        self.push_block(Box::new(PolicyDefinition { id: None, policy: None, reference: Some(name.into()), outputs: Default::default() }))
    }

    /// Attach a route policy.
    pub fn route_policy(&mut self, policy: Arc<dyn RoutePolicy>) -> &mut Self {
        self.extension_mut::<RoutePolicies>(ROUTE_POLICIES).0.push(RoutePolicyRef::Policy(policy));
        self
    }

    /// Attach a route policy bound in the registry as `Arc<dyn RoutePolicy>`.
    pub fn route_policy_ref(&mut self, name: impl Into<String>) -> &mut Self {
        self.extension_mut::<RoutePolicies>(ROUTE_POLICIES).0.push(RoutePolicyRef::Ref(name.into()));
        self
    }
}

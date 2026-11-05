//! Error handlers and redelivery.
//!
//! Like Apache Camel, the error handler sits in the "channel" in front of every
//! processor of a route: when a step fails it is redelivered (per the
//! [`RedeliveryPolicy`]) and when redeliveries are exhausted the failure processor runs
//! (matching `onException` clause outputs, or the dead letter channel endpoint).

use std::sync::Arc;
use std::time::Duration;

use crate::builder::{ExpressionDefinition, IntoExpression};
use crate::context::CamelContext;
use crate::error::{kind_distance, CamelError, Result};
use crate::log::Level;
use crate::message::{props, Exchange};
use crate::spi::{Predicate, Processor};

use super::{log_at, matches_opt, random_f64, restore_message, set_caught_exception, ResolvedBool, ORIGINAL_MESSAGE};

// ---------------------------------------------------------------------------
// Redelivery policy
// ---------------------------------------------------------------------------

/// Redelivery settings (Camel `RedeliveryPolicy`).
#[derive(Clone, Debug, PartialEq)]
pub struct RedeliveryPolicy {
    /// Maximum redelivery attempts; `-1` = forever, `0` = no redelivery (default).
    pub maximum_redeliveries: i32,
    /// Initial redelivery delay in millis (default 1000).
    pub redelivery_delay: u64,
    /// Multiplier for exponential back-off (default 2.0).
    pub back_off_multiplier: f64,
    pub use_exponential_back_off: bool,
    /// Upper bound for the delay in millis (default 60000).
    pub maximum_redelivery_delay: u64,
    pub use_collision_avoidance: bool,
    /// Collision avoidance factor (default 0.15 = +/-15%).
    pub collision_avoidance_factor: f64,
    pub retry_attempted_log_level: Level,
    pub retries_exhausted_log_level: Level,
    pub log_stack_trace: bool,
    pub log_exhausted: bool,
    pub log_retry_attempted: bool,
    /// Delay pattern such as `5:1000;10:5000` (attempt >= N waits the given millis).
    pub delay_pattern: Option<String>,
    /// Camel `asyncDelayedRedelivery`: when the exchange was picked up by an asynchronous
    /// consumer (seda, timer), the consumer thread is released during the redelivery delay
    /// (see [`crate::async_delay`]). Synchronous callers (direct, `ProducerTemplate`) still wait.
    pub async_delayed_redelivery: bool,
}

impl Default for RedeliveryPolicy {
    fn default() -> Self {
        RedeliveryPolicy {
            maximum_redeliveries: 0,
            redelivery_delay: 1000,
            back_off_multiplier: 2.0,
            use_exponential_back_off: false,
            maximum_redelivery_delay: 60_000,
            use_collision_avoidance: false,
            collision_avoidance_factor: 0.15,
            retry_attempted_log_level: Level::Debug,
            retries_exhausted_log_level: Level::Error,
            log_stack_trace: true,
            log_exhausted: true,
            log_retry_attempted: true,
            delay_pattern: None,
            async_delayed_redelivery: false,
        }
    }
}

impl RedeliveryPolicy {
    /// Delay before redelivery attempt number `attempt` (1-based), in millis.
    pub fn calculate_delay(&self, attempt: i32) -> u64 {
        let mut delay: f64 = if let Some(p) = &self.delay_pattern {
            pattern_delay(p, attempt) as f64
        } else if self.use_exponential_back_off && attempt > 1 {
            self.redelivery_delay as f64 * self.back_off_multiplier.powi(attempt - 1)
        } else {
            self.redelivery_delay as f64
        };
        if self.use_collision_avoidance && delay > 0.0 {
            let variance = (random_f64() * 2.0 - 1.0) * self.collision_avoidance_factor;
            delay += delay * variance;
        }
        if self.maximum_redelivery_delay > 0 && delay > self.maximum_redelivery_delay as f64 {
            delay = self.maximum_redelivery_delay as f64;
        }
        delay.max(0.0) as u64
    }
}

fn pattern_delay(pattern: &str, attempt: i32) -> u64 {
    let mut result = 0;
    for group in pattern.split(';') {
        let mut it = group.splitn(2, ':');
        let (Some(a), Some(d)) = (it.next(), it.next()) else { continue };
        let (Ok(a), Ok(d)) = (a.trim().parse::<i32>(), d.trim().parse::<u64>()) else { continue };
        if attempt >= a {
            result = d;
        }
    }
    result
}

/// Partial redelivery settings (used by `onException` clauses) that override the
/// error handler's policy.
#[derive(Clone, Debug, Default)]
pub struct RedeliveryOverrides {
    pub maximum_redeliveries: Option<i32>,
    pub redelivery_delay: Option<u64>,
    pub back_off_multiplier: Option<f64>,
    pub use_exponential_back_off: Option<bool>,
    pub maximum_redelivery_delay: Option<u64>,
    pub use_collision_avoidance: Option<bool>,
    pub retry_attempted_log_level: Option<Level>,
    pub retries_exhausted_log_level: Option<Level>,
    pub log_stack_trace: Option<bool>,
    pub log_exhausted: Option<bool>,
    pub delay_pattern: Option<String>,
    pub async_delayed_redelivery: Option<bool>,
}

impl RedeliveryOverrides {
    pub fn apply(&self, base: &RedeliveryPolicy) -> RedeliveryPolicy {
        let mut p = base.clone();
        macro_rules! ov {
            ($($f:ident),*) => { $( if let Some(v) = &self.$f { p.$f = v.clone(); } )* };
        }
        ov!(
            maximum_redeliveries,
            redelivery_delay,
            back_off_multiplier,
            use_exponential_back_off,
            maximum_redelivery_delay,
            use_collision_avoidance,
            retry_attempted_log_level,
            retries_exhausted_log_level,
            log_stack_trace,
            log_exhausted,
            async_delayed_redelivery
        );
        if let Some(d) = &self.delay_pattern {
            p.delay_pattern = Some(d.clone());
        }
        p
    }
}

// ---------------------------------------------------------------------------
// Error handler builder (DSL)
// ---------------------------------------------------------------------------

/// Which error handler.
#[derive(Clone, Debug, PartialEq)]
pub enum ErrorHandlerKind {
    /// `DefaultErrorHandler`: no redelivery by default, logs exhausted failures, the
    /// exchange stays failed.
    Default,
    /// `DeadLetterChannel`: moves exhausted exchanges to the endpoint (handled).
    DeadLetterChannel(String),
    /// `NoErrorHandler`: no error handling at all (no onException either).
    NoErrorHandler,
}

/// Error handler configuration built with [`default_error_handler`],
/// [`dead_letter_channel`] or [`no_error_handler`].
#[derive(Clone)]
pub struct ErrorHandlerBuilder {
    pub kind: ErrorHandlerKind,
    pub policy: RedeliveryPolicy,
    pub on_redelivery: Option<Arc<dyn Processor>>,
    pub on_exception_occurred: Option<Arc<dyn Processor>>,
    pub on_prepare_failure: Option<Arc<dyn Processor>>,
    pub retry_while: Option<ExpressionDefinition>,
    pub use_original_message: bool,
    pub use_original_body: bool,
    pub dead_letter_handle_new_exception: bool,
}

impl std::fmt::Debug for ErrorHandlerBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ErrorHandler[{:?}, {:?}]", self.kind, self.policy)
    }
}

/// The default error handler.
pub fn default_error_handler() -> ErrorHandlerBuilder {
    ErrorHandlerBuilder::new(ErrorHandlerKind::Default)
}

/// A dead letter channel sending exhausted exchanges to `uri`.
pub fn dead_letter_channel(uri: impl Into<String>) -> ErrorHandlerBuilder {
    let mut b = ErrorHandlerBuilder::new(ErrorHandlerKind::DeadLetterChannel(uri.into()));
    // the DLC handles the failure, so the stack trace is not logged by default
    b.policy.log_stack_trace = false;
    b
}

/// No error handling.
pub fn no_error_handler() -> ErrorHandlerBuilder {
    ErrorHandlerBuilder::new(ErrorHandlerKind::NoErrorHandler)
}

impl ErrorHandlerBuilder {
    pub fn new(kind: ErrorHandlerKind) -> Self {
        ErrorHandlerBuilder {
            kind,
            policy: RedeliveryPolicy::default(),
            on_redelivery: None,
            on_exception_occurred: None,
            on_prepare_failure: None,
            retry_while: None,
            use_original_message: false,
            use_original_body: false,
            dead_letter_handle_new_exception: true,
        }
    }

    pub fn maximum_redeliveries(mut self, n: i32) -> Self {
        self.policy.maximum_redeliveries = n;
        self
    }
    /// Redelivery delay in millis.
    pub fn redelivery_delay(mut self, millis: u64) -> Self {
        self.policy.redelivery_delay = millis;
        self
    }
    pub fn back_off_multiplier(mut self, m: f64) -> Self {
        self.policy.back_off_multiplier = m;
        self
    }
    pub fn use_exponential_back_off(mut self) -> Self {
        self.policy.use_exponential_back_off = true;
        self
    }
    pub fn maximum_redelivery_delay(mut self, millis: u64) -> Self {
        self.policy.maximum_redelivery_delay = millis;
        self
    }
    pub fn use_collision_avoidance(mut self) -> Self {
        self.policy.use_collision_avoidance = true;
        self
    }
    /// Enable collision avoidance with the given factor (e.g. 0.15).
    pub fn collision_avoidance_factor(mut self, f: f64) -> Self {
        self.policy.use_collision_avoidance = true;
        self.policy.collision_avoidance_factor = f;
        self
    }
    pub fn retry_attempted_log_level(mut self, l: Level) -> Self {
        self.policy.retry_attempted_log_level = l;
        self
    }
    pub fn retries_exhausted_log_level(mut self, l: Level) -> Self {
        self.policy.retries_exhausted_log_level = l;
        self
    }
    pub fn log_stack_trace(mut self, b: bool) -> Self {
        self.policy.log_stack_trace = b;
        self
    }
    pub fn log_exhausted(mut self, b: bool) -> Self {
        self.policy.log_exhausted = b;
        self
    }
    pub fn log_retry_attempted(mut self, b: bool) -> Self {
        self.policy.log_retry_attempted = b;
        self
    }
    pub fn delay_pattern(mut self, p: impl Into<String>) -> Self {
        self.policy.delay_pattern = Some(p.into());
        self
    }
    /// Release the consumer thread of asynchronous consumers during redelivery delays
    /// (Camel `asyncDelayedRedelivery`).
    pub fn async_delayed_redelivery(mut self, b: bool) -> Self {
        self.policy.async_delayed_redelivery = b;
        self
    }
    /// Replace the whole redelivery policy.
    pub fn redelivery_policy(mut self, p: RedeliveryPolicy) -> Self {
        self.policy = p;
        self
    }
    /// Processor invoked before each redelivery attempt.
    pub fn on_redelivery(mut self, p: impl Processor + 'static) -> Self {
        self.on_redelivery = Some(Arc::new(p));
        self
    }
    /// Processor invoked each time an exception occurs (before redelivery decisions).
    pub fn on_exception_occurred(mut self, p: impl Processor + 'static) -> Self {
        self.on_exception_occurred = Some(Arc::new(p));
        self
    }
    /// Processor invoked just before the exchange is handed to the failure processor / DLC.
    pub fn on_prepare_failure(mut self, p: impl Processor + 'static) -> Self {
        self.on_prepare_failure = Some(Arc::new(p));
        self
    }
    /// Keep redelivering while the predicate is true (overrules `maximum_redeliveries`).
    pub fn retry_while(mut self, p: impl IntoExpression) -> Self {
        self.retry_while = Some(p.into_expression());
        self
    }
    /// Send the original input message (as it entered the route) to the DLC.
    pub fn use_original_message(mut self) -> Self {
        self.use_original_message = true;
        self
    }
    /// Send the original input body (current headers) to the DLC.
    pub fn use_original_body(mut self) -> Self {
        self.use_original_body = true;
        self
    }
    /// Whether a new exception thrown while sending to the DLC is handled (default true).
    pub fn dead_letter_handle_new_exception(mut self, b: bool) -> Self {
        self.dead_letter_handle_new_exception = b;
        self
    }

    pub fn is_dead_letter_channel(&self) -> bool {
        matches!(self.kind, ErrorHandlerKind::DeadLetterChannel(_))
    }
}

// ---------------------------------------------------------------------------
// Runtime
// ---------------------------------------------------------------------------

/// Runtime form of an `onException` clause.
pub(crate) struct ExceptionPolicy {
    pub exceptions: Vec<String>,
    pub route_scoped: bool,
    pub on_when: Option<Arc<dyn Predicate>>,
    pub retry_while: Option<Arc<dyn Predicate>>,
    pub handled: Option<ResolvedBool>,
    pub continued: Option<ResolvedBool>,
    pub policy: RedeliveryPolicy,
    pub on_redelivery: Option<Arc<dyn Processor>>,
    pub on_exception_occurred: Option<Arc<dyn Processor>>,
    pub use_original_message: bool,
    pub use_original_body: bool,
    /// The clause outputs (None when the clause has no outputs).
    pub processor: Option<Arc<dyn Processor>>,
}

/// Runtime error handler of one route.
pub(crate) struct ErrorHandlerRuntime {
    pub route_id: String,
    pub dead_letter_uri: Option<String>,
    pub dead_letter: Option<Arc<dyn Processor>>,
    pub policy: RedeliveryPolicy,
    pub retry_while: Option<Arc<dyn Predicate>>,
    pub on_redelivery: Option<Arc<dyn Processor>>,
    pub on_exception_occurred: Option<Arc<dyn Processor>>,
    pub on_prepare_failure: Option<Arc<dyn Processor>>,
    pub use_original_message: bool,
    pub use_original_body: bool,
    pub dead_letter_handle_new_exception: bool,
    pub policies: Vec<ExceptionPolicy>,
}

impl ErrorHandlerRuntime {
    pub(crate) fn new(route_id: &str, b: &ErrorHandlerBuilder, ctx: &CamelContext) -> Result<Self> {
        let (dead_letter_uri, dead_letter) = match &b.kind {
            ErrorHandlerKind::DeadLetterChannel(uri) => {
                let p: Arc<dyn Processor> = ctx.get_producer(uri)?;
                (Some(uri.clone()), Some(p))
            }
            _ => (None, None),
        };
        Ok(ErrorHandlerRuntime {
            route_id: route_id.to_string(),
            dead_letter_uri,
            dead_letter,
            policy: b.policy.clone(),
            retry_while: b.retry_while.as_ref().map(|p| p.resolve_predicate(ctx)).transpose()?,
            on_redelivery: b.on_redelivery.clone(),
            on_exception_occurred: b.on_exception_occurred.clone(),
            on_prepare_failure: b.on_prepare_failure.clone(),
            use_original_message: b.use_original_message,
            use_original_body: b.use_original_body,
            dead_letter_handle_new_exception: b.dead_letter_handle_new_exception,
            policies: Vec::new(),
        })
    }

    /// Whether any part of this handler needs the original message snapshot.
    pub(crate) fn needs_original(&self) -> bool {
        self.use_original_message
            || self.use_original_body
            || self.policies.iter().any(|p| p.use_original_body || p.use_original_message)
    }

    /// Find the `onException` clause for an error (Camel's DefaultExceptionPolicyStrategy):
    /// walk the cause chain from the innermost cause outwards; for the first exception in
    /// the chain that any clause matches, the clause with the closest kind wins
    /// (route-scoped clauses win ties). `onWhen` filters candidates.
    pub(crate) fn find_policy(&self, ex: &Exchange, err: &CamelError) -> Option<&ExceptionPolicy> {
        find_exception_policy(&self.policies, ex, err)
    }

    /// Handle an exhausted failure: run the failure processor and decide whether the
    /// exchange is handled, continued, or stays failed.
    fn handle_exhausted(&self, ex: &mut Exchange, err: CamelError, policy: Option<&ExceptionPolicy>, attempts: i32) {
        let rp = policy.map(|p| &p.policy).unwrap_or(&self.policy);
        set_caught_exception(ex, &err);
        if let Some(ep) = ex.property(props::TO_ENDPOINT).cloned() {
            ex.set_property(props::FAILURE_ENDPOINT, ep);
        }
        ex.set_property(props::FAILURE_ROUTE_ID, self.route_id.clone());
        let use_orig_msg = policy.map(|p| p.use_original_message).unwrap_or(false) || self.use_original_message;
        let use_orig_body = policy.map(|p| p.use_original_body).unwrap_or(false) || self.use_original_body;
        if use_orig_msg || use_orig_body {
            if let Some(snap) = ex.property(ORIGINAL_MESSAGE).cloned() {
                restore_message(ex, &snap, use_orig_msg);
            }
        }
        // clear the exception so the failure processor can route the exchange
        ex.exception = None;
        let is_dlc = self.dead_letter.is_some();
        let continued = policy.and_then(|p| p.continued.as_ref()).map(|c| c.eval(ex)).unwrap_or(false);
        let handled = !continued && policy.and_then(|p| p.handled.as_ref()).map(|h| h.eval(ex)).unwrap_or(is_dlc);

        let clause_processor = policy.and_then(|p| p.processor.clone());
        let (failure_processor, to_dlc) = match clause_processor {
            Some(p) => (Some(p), false),
            None => (self.dead_letter.clone(), is_dlc),
        };
        if let Some(prep) = &self.on_prepare_failure {
            if let Err(e) = prep.process(ex) {
                crate::camel_warn!("ErrorHandler", "Error in onPrepareFailure processor: {}", e);
                ex.exception = None;
            }
        }
        if let Some(fp) = failure_processor {
            let saved_stop = ex.is_route_stop();
            if saved_stop {
                ex.set_route_stop(false);
            }
            if let Err(e) = fp.process(ex) {
                ex.set_exception(e);
            }
            if let Some(new_err) = ex.exception.take() {
                if to_dlc && self.dead_letter_handle_new_exception {
                    crate::camel_warn!(
                        "DeadLetterChannel",
                        "Failed delivery to dead letter channel: {} on ExchangeId: {}. The new exception is being handled as deadLetterHandleNewException=true: {}",
                        self.dead_letter_uri.as_deref().unwrap_or(""),
                        ex.exchange_id,
                        new_err
                    );
                } else {
                    crate::camel_error!(
                        "ErrorHandler",
                        "Exception occurred while trying to handle previously thrown exception on exchangeId: {} using: failure processor. The previous and the new exception will be logged in the following. Previous: {} New: {}",
                        ex.exchange_id,
                        err,
                        new_err
                    );
                    ex.remove_property(props::REDELIVERY_EXHAUSTED);
                    ex.set_exception(new_err);
                    ex.set_property(props::REDELIVERY_EXHAUSTED, true);
                    return;
                }
            }
            // a nested route's error handler may have flagged exhaustion
            ex.remove_property(props::REDELIVERY_EXHAUSTED);
            if saved_stop {
                ex.set_route_stop(true);
            }
        }
        if continued {
            ex.remove_property(props::REDELIVERY_EXHAUSTED);
            crate::camel_debug!("ErrorHandler", "Exchange {} continued after exception: {}", ex.exchange_id, err);
            return;
        }
        if handled {
            ex.set_property(props::ERRORHANDLER_HANDLED, true);
            ex.remove_property(props::REDELIVERY_EXHAUSTED);
            ex.set_route_stop(true);
            crate::camel_debug!("ErrorHandler", "Exchange {} handled after exception: {}", ex.exchange_id, err);
            return;
        }
        ex.set_property(props::ERRORHANDLER_HANDLED, false);
        if rp.log_exhausted {
            let level = rp.retries_exhausted_log_level;
            let stack = rp.log_stack_trace;
            log_at(level, "DefaultErrorHandler", || {
                let mut m = format!(
                    "Failed delivery for (MessageId: {} on ExchangeId: {}). Exhausted after delivery attempt: {} caught: {}. Processed by failure processor: {}",
                    ex.message.message_id,
                    ex.exchange_id,
                    attempts + 1,
                    err,
                    match (policy.and_then(|p| p.processor.as_ref()), &self.dead_letter_uri) {
                        (Some(_), _) => "onException".to_string(),
                        (None, Some(u)) => u.clone(),
                        (None, None) => "null".to_string(),
                    }
                );
                if !crate::history::message_history(ex).is_empty() {
                    m.push_str("\n\n");
                    m.push_str(&crate::history::format_message_history(ex));
                } else if stack {
                    m.push_str(&format!("\n\nStacktrace\n---------------------------------------------------------------------------------------------------------------------------------------\n{}", format_chain(&err)));
                }
                m
            });
        }
        ex.set_exception(err);
        ex.set_property(props::REDELIVERY_EXHAUSTED, true);
    }
}

pub(crate) fn find_exception_policy<'a>(policies: &'a [ExceptionPolicy], ex: &Exchange, err: &CamelError) -> Option<&'a ExceptionPolicy> {
    if policies.is_empty() {
        return None;
    }
    let chain: Vec<&CamelError> = err.chain().collect();
    for e in chain.iter().rev() {
        let mut best: Option<(usize, bool, &ExceptionPolicy)> = None;
        for p in policies {
            let Some(d) = p.exceptions.iter().filter_map(|k| kind_distance(&e.kind, k)).min() else { continue };
            let better = match best {
                None => true,
                Some((bd, broute, _)) => d < bd || (d == bd && p.route_scoped && !broute),
            };
            if better && matches_opt(&p.on_when, ex) {
                best = Some((d, p.route_scoped, p));
            }
        }
        if let Some((_, _, p)) = best {
            return Some(p);
        }
    }
    None
}

fn format_chain(err: &CamelError) -> String {
    err.chain()
        .enumerate()
        .map(|(i, e)| if i == 0 { format!("{}: {}", e.kind, e.message) } else { format!("Caused by: {}: {}", e.kind, e.message) })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The error handling channel wrapped around every processor of a route.
pub(crate) struct ErrorChannel {
    pub inner: Arc<dyn Processor>,
    pub eh: Arc<ErrorHandlerRuntime>,
}

impl Processor for ErrorChannel {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        if let Err(e) = self.inner.process(ex) {
            ex.set_exception(e);
        }
        if ex.exception.is_none() {
            return Ok(());
        }
        self.after_attempt(ex)
    }

    fn process_timed(&self, ex: &mut Exchange, start: u64) -> (Result<()>, u64) {
        let (r, end) = self.inner.process_timed(ex, start);
        if let Err(e) = r {
            ex.set_exception(e);
        }
        if ex.exception.is_none() {
            return (Ok(()), end);
        }
        (self.after_attempt(ex), 0)
    }
}

impl ErrorChannel {
    /// Error handling after the first attempt: redeliveries, then the exhausted handling.
    fn after_attempt(&self, ex: &mut Exchange) -> Result<()> {
        let mut attempt: i32 = 0;
        let mut run_inner = false;
        loop {
            if run_inner {
                if let Err(e) = self.inner.process(ex) {
                    ex.set_exception(e);
                }
            }
            run_inner = true;
            let Some(err) = ex.exception.clone() else { return Ok(()) };
            if super::is_error_handler_suppressed(ex) || super::is_exhausted(ex) {
                return Ok(());
            }
            let policy = self.eh.find_policy(ex, &err);
            let rp = policy.map(|p| &p.policy).unwrap_or(&self.eh.policy);
            if let Some(p) = policy.and_then(|p| p.on_exception_occurred.clone()).or_else(|| self.eh.on_exception_occurred.clone()) {
                if let Err(e) = p.process(ex) {
                    crate::camel_warn!("ErrorHandler", "Error in onExceptionOccurred processor: {}", e);
                }
                ex.exception = Some(err.clone());
            }
            let retry_while = policy.and_then(|p| p.retry_while.clone()).or_else(|| self.eh.retry_while.clone());
            let should_retry = match &retry_while {
                Some(p) => p.matches(ex).unwrap_or(false),
                None => rp.maximum_redeliveries < 0 || attempt < rp.maximum_redeliveries,
            };
            if !should_retry {
                self.eh.handle_exhausted(ex, err, policy, attempt);
                return Ok(());
            }
            attempt += 1;
            if rp.log_retry_attempted {
                log_at(rp.retry_attempted_log_level, "ErrorHandler", || {
                    format!(
                        "Failed delivery for (MessageId: {} on ExchangeId: {}). On delivery attempt: {} caught: {}",
                        ex.message.message_id,
                        ex.exchange_id,
                        attempt - 1,
                        err
                    )
                });
            }
            ex.set_header(props::REDELIVERED, true);
            ex.set_header(props::REDELIVERY_COUNTER, attempt);
            if retry_while.is_none() && rp.maximum_redeliveries >= 0 {
                ex.set_header(props::REDELIVERY_MAX_COUNTER, rp.maximum_redeliveries);
            }
            let delay = rp.calculate_delay(attempt);
            if rp.async_delayed_redelivery {
                crate::async_delay::async_sleep(Duration::from_millis(delay));
            } else if delay > 0 {
                std::thread::sleep(Duration::from_millis(delay));
            }
            ex.exception = None;
            if let Some(p) = policy.and_then(|p| p.on_redelivery.clone()).or_else(|| self.eh.on_redelivery.clone()) {
                if let Err(e) = p.process(ex) {
                    ex.set_exception(e);
                    run_inner = false;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays() {
        let mut p = RedeliveryPolicy { redelivery_delay: 100, ..Default::default() };
        assert_eq!(p.calculate_delay(1), 100);
        assert_eq!(p.calculate_delay(3), 100);
        p.use_exponential_back_off = true;
        assert_eq!(p.calculate_delay(1), 100);
        assert_eq!(p.calculate_delay(2), 200);
        assert_eq!(p.calculate_delay(3), 400);
        p.maximum_redelivery_delay = 300;
        assert_eq!(p.calculate_delay(3), 300);
        p.delay_pattern = Some("2:10;4:50".into());
        assert_eq!(p.calculate_delay(1), 0);
        assert_eq!(p.calculate_delay(2), 10);
        assert_eq!(p.calculate_delay(3), 10);
        assert_eq!(p.calculate_delay(7), 50);
        let c = RedeliveryPolicy { redelivery_delay: 1000, use_collision_avoidance: true, ..Default::default() };
        for _ in 0..50 {
            let d = c.calculate_delay(1);
            assert!((850..=1150).contains(&d), "{}", d);
        }
    }

    #[test]
    fn overrides() {
        let base = RedeliveryPolicy::default();
        let o = RedeliveryOverrides { maximum_redeliveries: Some(3), ..Default::default() };
        let p = o.apply(&base);
        assert_eq!(p.maximum_redeliveries, 3);
        assert_eq!(p.redelivery_delay, 1000);
    }
}

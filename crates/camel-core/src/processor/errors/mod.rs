//! Error handling, interceptors and flow-control EIPs.
//!
//! - [`handler`]: error handlers (`DefaultErrorHandler`, `DeadLetterChannel`, `NoErrorHandler`),
//!   the redelivery policy and the per-step error handling channel.
//! - [`on_exception`]: `onException` clauses and exception policy matching.
//! - [`hook`]: the [`crate::context::RouteFactoryHook`] wiring error handling, interceptors,
//!   onCompletion and route policies into every route.
//! - [`do_try`]: `doTry` / `doCatch` / `doFinally`.
//! - [`intercept`]: `intercept`, `interceptFrom`, `interceptSendToEndpoint`.
//! - [`on_completion`]: `onCompletion`.
//! - [`throttle`]: `throttle`, `delay`, `sample`.
//! - [`idempotent`]: idempotent consumer and repositories.
//! - [`circuit_breaker`]: circuit breaker EIP.
//! - [`misc`]: `validate`, `rollback`, `throwException`, `threads`, `transacted`, `policy`.
//! - [`route_policy`]: route policies.

pub mod circuit_breaker;
pub mod do_try;
pub mod handler;
pub mod hook;
pub mod idempotent;
pub mod intercept;
pub mod misc;
pub mod on_completion;
pub mod on_exception;
pub mod route_policy;
pub mod throttle;

use std::sync::Arc;

use crate::builder::{ExpressionDefinition, ValueBuilder};
use crate::context::CamelContext;
use crate::error::{CamelError, Result};
use crate::message::{props, Exchange, Headers};
use crate::spi::Predicate;
use crate::value::Value;

pub use circuit_breaker::{CircuitBreakerConfig, CircuitBreakerDefinition, CircuitBreakerState};
pub use do_try::DoTryDefinition;
pub use handler::{dead_letter_channel, default_error_handler, no_error_handler, ErrorHandlerBuilder, ErrorHandlerKind, RedeliveryPolicy};
pub use hook::{CamelContextErrorExt, ErrorsRouteHook};
pub use idempotent::{memory_idempotent_repository, IdempotentConsumerDefinition, IdempotentRepository, MemoryIdempotentRepository};
pub use intercept::{InterceptDefinition, InterceptKind};
pub use misc::Policy;
pub use on_completion::{OnCompletionDefinition, OnCompletionMode};
pub use on_exception::OnExceptionDefinition;
pub use route_policy::{RoutePolicy, ThrottlingInflightRoutePolicy, ThrottlingScope};

/// Internal exchange property: nesting depth of blocks (doTry, circuitBreaker) inside
/// which the error handler must not handle failures (they are handled by the block).
pub const TRY_ROUTE_BLOCK: &str = "CamelTryRouteBlock";
/// Property set by `markRollbackOnlyLast()`.
pub const ROLLBACK_ONLY_LAST: &str = "CamelRollbackOnlyLast";
/// Internal property holding a snapshot of the message as it entered the current route
/// (only captured when `useOriginalMessage`/`useOriginalBody` is configured).
pub const ORIGINAL_MESSAGE: &str = "CamelErrorHandlerOriginalMessage";
/// Internal property recording the onCompletion definitions registered on an exchange.
pub const ON_COMPLETION_REGISTERED: &str = "CamelOnCompletionRegistered";

/// Convert an error (with its cause chain) to a Value object `{kind, message, cause?}`.
/// This is how `CamelExceptionCaught` is stored (Simple's `${exception.message}` reads it).
pub fn error_to_value(e: &CamelError) -> Value {
    let mut items: Vec<(&str, Value)> = vec![("kind", Value::from(e.kind.as_str())), ("message", Value::from(e.message.as_str()))];
    if let Some(c) = &e.cause {
        items.push(("cause", error_to_value(c)));
    }
    Value::object(items)
}

/// Reconstruct a typed error from a Value produced by [`error_to_value`].
pub fn value_to_error(v: &Value) -> Option<CamelError> {
    let kind = v.get("kind")?.as_str()?.to_string();
    let message = v.get("message").map(|m| m.to_string()).unwrap_or_default();
    let mut e = CamelError::new(kind, message);
    if let Some(c) = v.get("cause").and_then(value_to_error) {
        e = e.with_cause(c);
    }
    Some(e)
}

/// The typed copy of the `CamelExceptionCaught` property (set by error handlers,
/// onException, doCatch, circuit breaker fallbacks and onCompletion).
pub fn caught_exception(ex: &Exchange) -> Option<CamelError> {
    ex.property(props::EXCEPTION_CAUGHT).and_then(value_to_error)
}

/// Store `e` as the `CamelExceptionCaught` property.
pub fn set_caught_exception(ex: &mut Exchange, e: &CamelError) {
    ex.set_property(props::EXCEPTION_CAUGHT, error_to_value(e));
}

/// True when the error handler is suppressed for this exchange (inside doTry etc.).
pub fn is_error_handler_suppressed(ex: &Exchange) -> bool {
    ex.property(TRY_ROUTE_BLOCK).and_then(|v| v.as_i64()).unwrap_or(0) > 0
}

pub(crate) fn enter_suppressed(ex: &mut Exchange) {
    let n = ex.property(TRY_ROUTE_BLOCK).and_then(|v| v.as_i64()).unwrap_or(0);
    ex.set_property(TRY_ROUTE_BLOCK, n + 1);
}

pub(crate) fn exit_suppressed(ex: &mut Exchange) {
    let n = ex.property(TRY_ROUTE_BLOCK).and_then(|v| v.as_i64()).unwrap_or(0);
    if n <= 1 {
        ex.remove_property(TRY_ROUTE_BLOCK);
    } else {
        ex.set_property(TRY_ROUTE_BLOCK, n - 1);
    }
}

/// True when the error handler gave up on this exchange (redeliveries exhausted).
pub fn is_exhausted(ex: &Exchange) -> bool {
    ex.property(props::REDELIVERY_EXHAUSTED).map(|v| v.is_truthy()).unwrap_or(false)
}

/// Snapshot of a message (body + headers) as a Value.
pub(crate) fn snapshot_message(ex: &Exchange) -> Value {
    Value::object([("body", ex.message.body.clone()), ("headers", ex.message.headers.to_value())])
}

/// Context global option enabling original-message tracking (Camel `allowUseOriginalMessage`),
/// switched on automatically when a route uses `useOriginalMessage`/`useOriginalBody`.
pub(crate) const ALLOW_USE_ORIGINAL_MESSAGE: &str = "CamelAllowUseOriginalMessage";

/// Mark a sub-exchange (split/multicast/recipientList copy) as starting a new unit of work:
/// if original-message tracking is active, the sub-exchange message becomes its original message.
pub(crate) fn begin_child_unit_of_work(ex: &mut Exchange) {
    if ex.properties.contains_key(ORIGINAL_MESSAGE) {
        let snap = snapshot_message(ex);
        ex.set_property(ORIGINAL_MESSAGE, snap);
    }
}

/// Restore body (and optionally headers) from a snapshot produced by [`snapshot_message`].
pub(crate) fn restore_message(ex: &mut Exchange, snap: &Value, headers_too: bool) {
    if let Some(b) = snap.get("body") {
        ex.message.body = b.clone();
    }
    if headers_too {
        if let Some(h) = snap.get("headers").and_then(|h| h.as_object()) {
            ex.message.headers = h.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<Headers>();
        }
    }
}

/// A boolean option that can be a constant or a predicate (`handled(true)`,
/// `handled(header("x").is_equal_to(1))`).
#[derive(Clone, Debug)]
pub enum BoolOrPredicate {
    Bool(bool),
    Predicate(ExpressionDefinition),
}

impl BoolOrPredicate {
    pub(crate) fn resolve(&self, ctx: &CamelContext) -> Result<ResolvedBool> {
        Ok(match self {
            BoolOrPredicate::Bool(b) => ResolvedBool::Const(*b),
            BoolOrPredicate::Predicate(p) => ResolvedBool::Pred(p.resolve_predicate(ctx)?),
        })
    }
}

/// Conversion into [`BoolOrPredicate`].
pub trait IntoBoolOrPredicate {
    fn into_bool_or_predicate(self) -> BoolOrPredicate;
}
impl IntoBoolOrPredicate for bool {
    fn into_bool_or_predicate(self) -> BoolOrPredicate {
        BoolOrPredicate::Bool(self)
    }
}
impl IntoBoolOrPredicate for ValueBuilder {
    fn into_bool_or_predicate(self) -> BoolOrPredicate {
        BoolOrPredicate::Predicate(self.def)
    }
}
impl IntoBoolOrPredicate for ExpressionDefinition {
    fn into_bool_or_predicate(self) -> BoolOrPredicate {
        BoolOrPredicate::Predicate(self)
    }
}

/// Runtime form of [`BoolOrPredicate`].
#[derive(Clone)]
pub(crate) enum ResolvedBool {
    Const(bool),
    Pred(Arc<dyn Predicate>),
}

impl ResolvedBool {
    pub(crate) fn eval(&self, ex: &Exchange) -> bool {
        match self {
            ResolvedBool::Const(b) => *b,
            ResolvedBool::Pred(p) => p.matches(ex).unwrap_or(false),
        }
    }
}

/// Evaluate an optional predicate (None = true).
pub(crate) fn matches_opt(p: &Option<Arc<dyn Predicate>>, ex: &Exchange) -> bool {
    match p {
        None => true,
        Some(p) => match p.matches(ex) {
            Ok(b) => b,
            Err(e) => {
                crate::camel_warn!("ErrorHandler", "Error evaluating predicate: {}", e);
                false
            }
        },
    }
}

/// Resolve an optional predicate definition.
pub(crate) fn resolve_opt_pred(d: &Option<ExpressionDefinition>, ctx: &CamelContext) -> Result<Option<Arc<dyn Predicate>>> {
    d.as_ref().map(|d| d.resolve_predicate(ctx)).transpose()
}

/// Log at a dynamic level.
pub(crate) fn log_at(level: crate::log::Level, logger: &str, msg: impl FnOnce() -> String) {
    if crate::log::enabled(level) {
        crate::log::log(level, logger, msg());
    }
}

/// Simple std-only pseudo random number in `[0, 1)` (for collision avoidance).
pub(crate) fn random_f64() -> f64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEED: AtomicU64 = AtomicU64::new(0);
    let mut s = SEED.load(Ordering::Relaxed);
    if s == 0 {
        s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1) | 1;
    }
    s ^= s << 13;
    s ^= s >> 7;
    s ^= s << 17;
    SEED.store(s, Ordering::Relaxed);
    (s >> 11) as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_value_roundtrip() {
        let e = CamelError::io("outer").with_cause(CamelError::illegal_argument("inner"));
        let v = error_to_value(&e);
        assert_eq!(v.get("message").unwrap().as_str(), Some("outer"));
        assert_eq!(value_to_error(&v).unwrap(), e);
    }

    #[test]
    fn random_in_range() {
        for _ in 0..100 {
            let r = random_f64();
            assert!((0.0..1.0).contains(&r));
        }
    }
}

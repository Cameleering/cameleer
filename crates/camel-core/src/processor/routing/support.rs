//! Shared helpers for the routing EIPs: lazily created thread pools, lazily
//! resolved expressions, aggregation strategy references, result copying and a
//! tiny std-only random number generator.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::builder::ExpressionDefinition;
use crate::context::CamelContext;
use crate::error::{CamelError, Result};
use crate::executor::ThreadPool;
use crate::message::Exchange;
use crate::spi::{AggregationStrategy, Expression, Predicate, Processor, Service};
use crate::value::Value;

/// Default size of the per-EIP thread pools (Camel's default thread pool profile).
pub const DEFAULT_POOL_SIZE: usize = 10;

// ---------------------------------------------------------------------------
// LazyPool
// ---------------------------------------------------------------------------

/// A thread pool created on first use and shut down when the owning route stops.
/// Registered as a route service so that a restarted route gets a fresh pool.
pub struct LazyPool {
    name: String,
    size: usize,
    pool: Mutex<Option<Arc<ThreadPool>>>,
}

impl LazyPool {
    pub fn new(name: impl Into<String>, size: usize) -> Arc<LazyPool> {
        Arc::new(LazyPool { name: name.into(), size: size.max(1), pool: Mutex::new(None) })
    }

    /// The pool, created if needed.
    pub fn get(&self) -> Arc<ThreadPool> {
        let mut p = self.pool.lock().unwrap();
        if let Some(pool) = p.as_ref() {
            if !pool.is_shutdown() {
                return pool.clone();
            }
        }
        let pool = ThreadPool::new(&self.name, self.size);
        *p = Some(pool.clone());
        pool
    }

    /// Run a job on the pool; runs it on the caller thread if the pool rejects it
    /// (Camel's `CallerRuns` rejection policy).
    pub fn execute(&self, job: impl FnOnce() + Send + 'static) {
        let pool = self.get();
        let slot = Arc::new(Mutex::new(Some(Box::new(job) as Box<dyn FnOnce() + Send>)));
        let s2 = slot.clone();
        let accepted = pool.execute(move || {
            if let Some(j) = s2.lock().unwrap().take() {
                j();
            }
        });
        if !accepted {
            if let Some(j) = slot.lock().unwrap().take() {
                j();
            }
        }
    }
}

/// A thread pool name in Camel style: `Camel (route) thread #N - name`.
pub fn pool_name(route_id: &str, eip: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    format!("Camel ({}) thread #{} - {}", route_id, n, eip)
}

impl Service for LazyPool {
    fn stop(&self) -> Result<()> {
        let p = self.pool.lock().unwrap().take();
        if let Some(p) = p {
            p.shutdown();
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Lazily resolved expressions (for strategies built outside a route context)
// ---------------------------------------------------------------------------

/// An expression definition resolved on first use against the exchange's context.
pub struct LazyExpr {
    def: ExpressionDefinition,
    cell: OnceLock<Result<Arc<dyn Expression>>>,
}

impl LazyExpr {
    pub fn new(def: ExpressionDefinition) -> Self {
        LazyExpr { def, cell: OnceLock::new() }
    }
}

impl Expression for LazyExpr {
    fn evaluate(&self, ex: &Exchange) -> Result<Value> {
        if let ExpressionDefinition::Expression(e) = &self.def {
            return e.evaluate(ex);
        }
        match self.cell.get_or_init(|| self.def.resolve_expression(ex.context()?)) {
            Ok(e) => e.evaluate(ex),
            Err(e) => Err(e.clone()),
        }
    }
}

/// A predicate definition resolved on first use against the exchange's context.
pub struct LazyPred {
    def: ExpressionDefinition,
    cell: OnceLock<Result<Arc<dyn Predicate>>>,
}

impl LazyPred {
    pub fn new(def: ExpressionDefinition) -> Self {
        LazyPred { def, cell: OnceLock::new() }
    }
}

impl Predicate for LazyPred {
    fn matches(&self, ex: &Exchange) -> Result<bool> {
        if let ExpressionDefinition::Predicate(p) = &self.def {
            return p.matches(ex);
        }
        match self.cell.get_or_init(|| self.def.resolve_predicate(ex.context()?)) {
            Ok(p) => p.matches(ex),
            Err(e) => Err(e.clone()),
        }
    }
}

// ---------------------------------------------------------------------------
// Aggregation strategy references
// ---------------------------------------------------------------------------

/// An aggregation strategy given as an instance or as a registry reference
/// (`aggregationStrategy = "#myStrategy"` / `aggregationStrategyRef`).
#[derive(Clone)]
pub enum StrategyDefinition {
    Instance(Arc<dyn AggregationStrategy>),
    Ref(String),
}

impl std::fmt::Debug for StrategyDefinition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StrategyDefinition::Instance(_) => f.write_str("<AggregationStrategy>"),
            StrategyDefinition::Ref(r) => write!(f, "ref:{}", r),
        }
    }
}

impl StrategyDefinition {
    pub fn instance(s: impl AggregationStrategy + 'static) -> Self {
        StrategyDefinition::Instance(Arc::new(s))
    }

    /// Resolve against the context registry.
    pub fn resolve(&self, ctx: &CamelContext) -> Result<Arc<dyn AggregationStrategy>> {
        match self {
            StrategyDefinition::Instance(s) => Ok(s.clone()),
            StrategyDefinition::Ref(name) => aggregation_strategy_ref(ctx, name),
        }
    }
}

/// Look up an aggregation strategy bound in the registry
/// (with `Registry::bind_aggregation_strategy`). A leading `#` / `#bean:` is ignored.
pub fn aggregation_strategy_ref(ctx: &CamelContext, name: &str) -> Result<Arc<dyn AggregationStrategy>> {
    let n = name.trim_start_matches("#bean:").trim_start_matches('#');
    ctx.registry().lookup_aggregation_strategy(n).ok_or_else(|| CamelError::no_such_bean(n))
}

// ---------------------------------------------------------------------------
// Exchange helpers
// ---------------------------------------------------------------------------

/// Run a processor, storing an `Err` as the exchange exception.
pub fn run_processor(p: &dyn Processor, ex: &mut Exchange) {
    if let Err(e) = p.process(ex) {
        ex.set_exception(e);
    }
}

/// Copy the result of `source` into `target`, keeping `target`'s identity
/// (exchange id, pattern, context, on-completions): message, merged properties,
/// exception and rollback flag (Camel `ExchangeHelper.copyResults`).
pub fn copy_results(target: &mut Exchange, source: Exchange) {
    let Exchange { message, properties, exception, rollback_only, on_completions, .. } = source;
    target.message = message;
    for (k, v) in properties {
        target.properties.insert(k, v);
    }
    target.exception = exception;
    target.rollback_only = target.rollback_only || rollback_only;
    target.on_completions.extend(on_completions);
}

/// Saves the given properties of an exchange so that they can be restored after
/// sub-exchange results have been merged back (avoids leaking e.g. `CamelSplitIndex`).
pub struct SavedProperties(Vec<(&'static str, Option<Value>)>);

impl SavedProperties {
    pub fn save(ex: &Exchange, keys: &[&'static str]) -> Self {
        SavedProperties(keys.iter().map(|k| (*k, ex.properties.get(*k).cloned())).collect())
    }
    pub fn restore(self, ex: &mut Exchange) {
        for (k, v) in self.0 {
            match v {
                Some(v) => {
                    ex.properties.insert(k.to_string(), v);
                }
                None => {
                    ex.properties.remove(k);
                }
            }
        }
    }
}

/// Evaluate a "list of things" expression result the way Camel's
/// `ObjectHelper.createIterator` does: arrays iterate their elements, null is empty,
/// strings are split on `delimiter` (unless it is `None`), objects iterate their
/// entries as `{key, value}` objects, everything else is a single element.
pub fn to_iterable(v: Value, delimiter: Option<&str>) -> Vec<Value> {
    match v {
        Value::Null => Vec::new(),
        Value::Array(a) => a,
        Value::String(s) => match delimiter {
            Some(d) if !d.is_empty() => {
                if s.is_empty() {
                    return Vec::new();
                }
                s.split(d).map(|p| Value::String(p.to_string())).collect()
            }
            _ => vec![Value::String(s)],
        },
        Value::Object(o) => o.into_iter().map(|(k, v)| Value::object([("key", Value::String(k)), ("value", v)])).collect(),
        other => vec![other],
    }
}

/// Convert an `i64` milliseconds setting to a duration (negative = None).
pub fn millis(ms: i64) -> Option<Duration> {
    if ms < 0 {
        None
    } else {
        Some(Duration::from_millis(ms as u64))
    }
}

// ---------------------------------------------------------------------------
// Random numbers (std only)
// ---------------------------------------------------------------------------

static RNG_STATE: AtomicU64 = AtomicU64::new(0);

/// A pseudo random number in `0..bound` (xorshift64*; not cryptographic).
pub fn random_below(bound: usize) -> usize {
    if bound <= 1 {
        return 0;
    }
    let mut current = RNG_STATE.load(Ordering::Relaxed);
    loop {
        let mut x = if current == 0 {
            (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x9E37_79B9_7F4A_7C15)
                ^ (std::process::id() as u64).rotate_left(32))
                | 1
        } else {
            current
        };
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        match RNG_STATE.compare_exchange(current, x, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as usize % bound,
            Err(actual) => current = actual,
        }
    }
}

/// Register an exception kind once (idempotent).
pub fn ensure_kind(kind: &'static str, parent: &'static str) {
    static REGISTERED: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut r = REGISTERED.lock().unwrap();
    if !r.contains(&kind) {
        crate::error::register_kind_parent(kind, parent);
        r.push(kind);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iterable_conversion() {
        assert_eq!(to_iterable(Value::from("a,b"), Some(",")).len(), 2);
        assert_eq!(to_iterable(Value::from("a,b"), None).len(), 1);
        assert!(to_iterable(Value::Null, Some(",")).is_empty());
        assert_eq!(to_iterable(Value::from(vec![Value::from(1), Value::from(2), Value::from(3)]), None).len(), 3);
    }

    #[test]
    fn random_is_in_range() {
        for _ in 0..1000 {
            assert!(random_below(7) < 7);
        }
    }
}

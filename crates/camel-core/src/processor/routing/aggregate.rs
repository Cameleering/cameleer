//! Aggregator EIP.
//!
//! Incoming exchanges are grouped by a correlation expression and merged with an
//! [`AggregationStrategy`]; when a group completes the aggregated exchange is routed
//! through the block's outputs. The incoming exchange itself continues after the
//! aggregate block unchanged (as in Camel).
//!
//! Completion conditions: `completion_size` (fixed or expression),
//! `completion_predicate`, `completion_timeout` (inactivity timeout per group,
//! fixed or expression), `completion_interval` (all groups, periodically),
//! `completion_from_batch_consumer` (`CamelBatchSize`),
//! `completion_on_new_correlation_group`, forced on stop (`force_completion_on_stop`),
//! and by the strategy (set `CamelAggregationCompleteCurrentGroup` on the aggregated
//! exchange) or by sending an exchange with `CamelAggregationCompleteAllGroups`
//! (/`...Inclusive`).
//!
//! The completed exchange has `CamelAggregatedSize`, `CamelAggregatedCompletedBy`
//! (`size`, `predicate`, `timeout`, `interval`, `consumer`, `force`, `strategy`,
//! `new`) and `CamelAggregatedCorrelationKey` set.
//!
//! Timeouts and intervals are handled by a background thread started/stopped with
//! the route; completed groups found by it are routed on that thread.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::builder::ExpressionDefinition;
use crate::error::{kinds, CamelError, Result};
use crate::impl_definition_boilerplate;
use crate::message::{props, Exchange};
use crate::model::{OutputList, ProcessorDefinition, RouteContext};
use crate::spi::{AggregationStrategy, Expression, Predicate, Processor, Service};
use crate::value::Value;

use super::options::{EipOption, EipOptions};
use super::strategies::{AGGREGATION_COMPLETE_ALL_GROUPS, AGGREGATION_COMPLETE_ALL_GROUPS_INCLUSIVE, AGGREGATION_COMPLETE_CURRENT_GROUP};
use super::support::{ensure_kind, run_processor, StrategyDefinition};

/// Exception kind raised for messages with a closed correlation key.
pub const CLOSED_CORRELATION_KEY: &str = "ClosedCorrelationKeyException";

// ---------------------------------------------------------------------------
// Repository
// ---------------------------------------------------------------------------

/// Storage of in-progress aggregation groups (Camel `AggregationRepository`).
pub trait AggregationRepository: Send + Sync {
    /// Store the aggregated exchange for a key; returns the previous one.
    fn add(&self, key: &str, exchange: Exchange) -> Option<Exchange>;
    fn get(&self, key: &str) -> Option<Exchange>;
    /// Remove the group (when completed).
    fn remove(&self, key: &str) -> Option<Exchange>;
    /// Confirm that a completed exchange was processed.
    fn confirm(&self, _exchange_id: &str) {}
    /// Keys of the groups in progress (insertion order).
    fn keys(&self) -> Vec<String>;
}

/// Default in-memory repository.
#[derive(Default)]
pub struct MemoryAggregationRepository {
    map: Mutex<HashMap<String, (u64, Exchange)>>,
    seq: AtomicU64,
}

impl MemoryAggregationRepository {
    pub fn new() -> Self {
        Self::default()
    }
}

impl AggregationRepository for MemoryAggregationRepository {
    fn add(&self, key: &str, exchange: Exchange) -> Option<Exchange> {
        let mut m = self.map.lock().unwrap();
        let seq = m.get(key).map(|(s, _)| *s).unwrap_or_else(|| self.seq.fetch_add(1, Ordering::Relaxed));
        m.insert(key.to_string(), (seq, exchange)).map(|(_, e)| e)
    }
    fn get(&self, key: &str) -> Option<Exchange> {
        self.map.lock().unwrap().get(key).map(|(_, e)| e.clone())
    }
    fn remove(&self, key: &str) -> Option<Exchange> {
        self.map.lock().unwrap().remove(key).map(|(_, e)| e)
    }
    fn keys(&self) -> Vec<String> {
        let m = self.map.lock().unwrap();
        let mut v: Vec<(u64, String)> = m.iter().map(|(k, (s, _))| (*s, k.clone())).collect();
        v.sort();
        v.into_iter().map(|(_, k)| k).collect()
    }
}

// ---------------------------------------------------------------------------
// Definition
// ---------------------------------------------------------------------------

/// Aggregate definition (block: outputs receive the completed aggregates).
pub struct AggregateDefinition {
    pub id: Option<String>,
    pub correlation: ExpressionDefinition,
    pub strategy: Option<StrategyDefinition>,
    pub outputs: OutputList,
    pub completion_size: Option<usize>,
    pub completion_size_expression: Option<ExpressionDefinition>,
    pub completion_predicate: Option<ExpressionDefinition>,
    pub completion_timeout: Option<Duration>,
    pub completion_timeout_expression: Option<ExpressionDefinition>,
    pub completion_interval: Option<Duration>,
    pub completion_from_batch_consumer: bool,
    pub completion_on_new_correlation_group: bool,
    pub eager_check_completion: bool,
    pub force_completion_on_stop: bool,
    pub discard_on_completion_timeout: bool,
    pub ignore_invalid_correlation_keys: bool,
    /// Close keys of completed groups (value = max number of closed keys remembered, 0 = unbounded).
    pub close_correlation_key_on_completion: Option<usize>,
    pub repository: Option<Arc<dyn AggregationRepository>>,
}

impl AggregateDefinition {
    pub fn new(correlation: ExpressionDefinition, strategy: Option<StrategyDefinition>) -> Self {
        AggregateDefinition {
            id: None,
            correlation,
            strategy,
            outputs: OutputList::default(),
            completion_size: None,
            completion_size_expression: None,
            completion_predicate: None,
            completion_timeout: None,
            completion_timeout_expression: None,
            completion_interval: None,
            completion_from_batch_consumer: false,
            completion_on_new_correlation_group: false,
            eager_check_completion: false,
            force_completion_on_stop: false,
            discard_on_completion_timeout: false,
            ignore_invalid_correlation_keys: false,
            close_correlation_key_on_completion: None,
            repository: None,
        }
    }
}

impl EipOptions for AggregateDefinition {
    fn apply_option(&mut self, option: &EipOption) -> bool {
        match option {
            EipOption::AggregationStrategy(s) => self.strategy = Some(s.clone()),
            EipOption::Timeout(d) => self.completion_timeout = Some(*d),
            _ => return false,
        }
        true
    }
}

impl ProcessorDefinition for AggregateDefinition {
    fn name(&self) -> &str {
        "aggregate"
    }
    fn label(&self) -> String {
        format!("aggregate[{:?}]", self.correlation)
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
        ensure_kind(CLOSED_CORRELATION_KEY, kinds::CAMEL_EXCHANGE);
        let strategy = match &self.strategy {
            Some(s) => s.resolve(&rc.context)?,
            None => return Err(CamelError::illegal_argument("AggregationStrategy must be set on aggregate")),
        };
        let has_timeout = self.completion_timeout.is_some() || self.completion_timeout_expression.is_some();
        if !(has_timeout
            || self.completion_interval.is_some()
            || self.completion_size.is_some()
            || self.completion_size_expression.is_some()
            || self.completion_predicate.is_some()
            || self.completion_from_batch_consumer)
        {
            return Err(CamelError::illegal_state(
                "At least one of the completions options [completionTimeout, completionInterval, completionSize, completionPredicate, completionFromBatchConsumer] must be set",
            ));
        }
        if has_timeout && self.completion_interval.is_some() {
            return Err(CamelError::illegal_argument("Only one of completionTimeout and completionInterval can be used, not both."));
        }
        let resolve = |e: &Option<ExpressionDefinition>| -> Result<Option<Arc<dyn Expression>>> {
            match e {
                Some(e) => Ok(Some(e.resolve_expression(&rc.context)?)),
                None => Ok(None),
            }
        };
        let child = rc.create_child_processor(&self.outputs.outputs)?;
        let inner = Arc::new(AggInner {
            correlation: self.correlation.resolve_expression(&rc.context)?,
            strategy,
            repo: self.repository.clone().unwrap_or_else(|| Arc::new(MemoryAggregationRepository::new())),
            output: child,
            size: self.completion_size,
            size_expr: resolve(&self.completion_size_expression)?,
            predicate: match &self.completion_predicate {
                Some(p) => Some(p.resolve_predicate(&rc.context)?),
                None => None,
            },
            timeout: self.completion_timeout,
            timeout_expr: resolve(&self.completion_timeout_expression)?,
            interval: self.completion_interval,
            batch_consumer: self.completion_from_batch_consumer,
            on_new_group: self.completion_on_new_correlation_group,
            eager: self.eager_check_completion,
            force_on_stop: self.force_completion_on_stop,
            discard_on_timeout: self.discard_on_completion_timeout,
            ignore_invalid_keys: self.ignore_invalid_correlation_keys,
            close_keys: self.close_correlation_key_on_completion,
            route_id: rc.route_id.clone(),
            state: Mutex::new(AggState::default()),
            cond: Condvar::new(),
        });
        let p = Arc::new(AggregateProcessor { inner, thread: Mutex::new(None) });
        rc.add_service(p.clone());
        Ok(p)
    }
    impl_definition_boilerplate!();
}

// ---------------------------------------------------------------------------
// Runtime
// ---------------------------------------------------------------------------

#[derive(Default)]
struct AggState {
    deadlines: HashMap<String, Instant>,
    closed: HashSet<String>,
    closed_order: VecDeque<String>,
    next_interval: Option<Instant>,
    running: bool,
}

struct AggInner {
    correlation: Arc<dyn Expression>,
    strategy: Arc<dyn AggregationStrategy>,
    repo: Arc<dyn AggregationRepository>,
    output: Arc<dyn Processor>,
    size: Option<usize>,
    size_expr: Option<Arc<dyn Expression>>,
    predicate: Option<Arc<dyn Predicate>>,
    timeout: Option<Duration>,
    timeout_expr: Option<Arc<dyn Expression>>,
    interval: Option<Duration>,
    batch_consumer: bool,
    on_new_group: bool,
    eager: bool,
    force_on_stop: bool,
    discard_on_timeout: bool,
    ignore_invalid_keys: bool,
    close_keys: Option<usize>,
    route_id: String,
    state: Mutex<AggState>,
    cond: Condvar,
}

/// A completed group ready to be emitted.
struct Completed {
    key: String,
    exchange: Exchange,
    by: &'static str,
}

fn aggregated_size(ex: &Exchange) -> usize {
    ex.properties.get(props::AGGREGATED_SIZE).and_then(|v| v.as_i64()).unwrap_or(0).max(0) as usize
}

impl AggInner {
    fn is_completed(&self, ex: &Exchange) -> Result<Option<&'static str>> {
        if let Some(p) = &self.predicate {
            if p.matches(ex)? {
                return Ok(Some("predicate"));
            }
        }
        let size = aggregated_size(ex);
        if let Some(e) = &self.size_expr {
            let v = e.evaluate(ex)?;
            if let Some(n) = v.as_i64() {
                if n > 0 && size as i64 >= n {
                    return Ok(Some("size"));
                }
            }
        }
        if let Some(n) = self.size {
            if n > 0 && size >= n {
                return Ok(Some("size"));
            }
        }
        if self.batch_consumer {
            if let Some(b) = ex.properties.get(props::BATCH_SIZE).and_then(|v| v.as_i64()) {
                if b > 0 && size as i64 >= b {
                    return Ok(Some("consumer"));
                }
            }
        }
        Ok(None)
    }

    fn close_key(&self, st: &mut AggState, key: &str) {
        if let Some(max) = self.close_keys {
            if st.closed.insert(key.to_string()) {
                st.closed_order.push_back(key.to_string());
                if max > 0 {
                    while st.closed_order.len() > max {
                        if let Some(k) = st.closed_order.pop_front() {
                            st.closed.remove(&k);
                        }
                    }
                }
            }
        }
    }

    /// Remove a group and mark it completed.
    fn complete_group(&self, st: &mut AggState, key: &str, by: &'static str, out: &mut Vec<Completed>) {
        st.deadlines.remove(key);
        if let Some(ex) = self.repo.remove(key) {
            self.close_key(st, key);
            out.push(Completed { key: key.to_string(), exchange: ex, by });
        }
    }

    fn complete_all(&self, st: &mut AggState, by: &'static str, out: &mut Vec<Completed>) {
        for k in self.repo.keys() {
            self.complete_group(st, &k, by, out);
        }
    }

    fn emit(&self, completed: Vec<Completed>) {
        for Completed { key, mut exchange, by } in completed {
            exchange.set_property(props::AGGREGATED_COMPLETED_BY, by);
            exchange.set_property(props::AGGREGATED_CORRELATION_KEY, key);
            self.strategy.on_completion(&mut exchange);
            exchange.exception = None;
            exchange.set_route_stop(false);
            exchange.properties.remove(props::ROUTE_STOP);
            let id = exchange.exchange_id.clone();
            run_processor(self.output.as_ref(), &mut exchange);
            if let Some(e) = &exchange.exception {
                crate::camel_warn!(
                    "aggregate",
                    "Error processing aggregated exchange {} on route {}: {}",
                    exchange.exchange_id,
                    self.route_id,
                    e
                );
            }
            exchange.run_on_completions();
            self.repo.confirm(&id);
        }
    }

    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        let complete_all = exchange.properties.get(AGGREGATION_COMPLETE_ALL_GROUPS).map(|v| v.is_truthy()).unwrap_or(false);
        let complete_all_inclusive =
            exchange.properties.get(AGGREGATION_COMPLETE_ALL_GROUPS_INCLUSIVE).map(|v| v.is_truthy()).unwrap_or(false);
        if complete_all {
            let mut out = Vec::new();
            {
                let mut st = self.state.lock().unwrap();
                self.complete_all(&mut st, "force", &mut out);
            }
            self.emit(out);
            return Ok(());
        }
        let key = self.correlation.evaluate(exchange)?;
        let invalid = match &key {
            Value::Null => true,
            Value::String(s) => s.is_empty(),
            _ => false,
        };
        if invalid {
            if self.ignore_invalid_keys {
                crate::camel_debug!("aggregate", "Invalid correlation key. This Exchange will be ignored: {}", exchange.exchange_id);
                return Ok(());
            }
            return Err(CamelError::new(
                kinds::CAMEL_EXCHANGE,
                format!("Invalid correlation key. Exchange[{}]", exchange.exchange_id),
            ));
        }
        let key = key.to_string();
        let mut out = Vec::new();
        {
            let mut st = self.state.lock().unwrap();
            if st.closed.contains(&key) {
                return Err(CamelError::new(CLOSED_CORRELATION_KEY, format!("The correlation key [{}] has been closed", key)));
            }
            let old = self.repo.get(&key);
            if old.is_none() && self.on_new_group {
                for k in self.repo.keys() {
                    if k != key {
                        self.complete_group(&mut st, &k, "new", &mut out);
                    }
                }
            }
            let size = old.as_ref().map(aggregated_size).unwrap_or(0) + 1;
            let mut incoming = exchange.correlated_copy();
            incoming.properties.remove(AGGREGATION_COMPLETE_ALL_GROUPS_INCLUSIVE);
            let mut by = None;
            if self.eager {
                incoming.set_property(props::AGGREGATED_SIZE, size);
                by = self.is_completed(&incoming)?;
                incoming.properties.remove(props::AGGREGATED_SIZE);
            }
            let mut agg = self.strategy.aggregate(old, incoming)?;
            agg.set_property(props::AGGREGATED_SIZE, size);
            if !self.eager {
                by = self.is_completed(&agg)?;
            }
            if agg.properties.remove(AGGREGATION_COMPLETE_CURRENT_GROUP).map(|v| v.is_truthy()).unwrap_or(false) && by.is_none() {
                by = Some("strategy");
            }
            match by {
                Some(by) => {
                    st.deadlines.remove(&key);
                    self.repo.remove(&key);
                    self.close_key(&mut st, &key);
                    out.push(Completed { key: key.clone(), exchange: agg, by });
                }
                None => {
                    self.repo.add(&key, agg);
                    let timeout = match &self.timeout_expr {
                        Some(e) => e.evaluate(exchange)?.as_i64().filter(|t| *t > 0).map(|t| Duration::from_millis(t as u64)).or(self.timeout),
                        None => self.timeout,
                    };
                    if let Some(t) = timeout {
                        st.deadlines.insert(key.clone(), Instant::now() + t);
                        self.cond.notify_all();
                    }
                }
            }
            if complete_all_inclusive {
                self.complete_all(&mut st, "force", &mut out);
            }
        }
        self.emit(out);
        Ok(())
    }

    /// Background loop handling timeouts and intervals.
    fn run_checker(self: Arc<Self>) {
        let mut st = self.state.lock().unwrap();
        loop {
            if !st.running {
                break;
            }
            let now = Instant::now();
            let mut out = Vec::new();
            let expired: Vec<String> = st.deadlines.iter().filter(|(_, d)| **d <= now).map(|(k, _)| k.clone()).collect();
            for k in expired {
                st.deadlines.remove(&k);
                if self.discard_on_timeout {
                    if self.repo.remove(&k).is_some() {
                        self.close_key(&mut st, &k);
                        crate::camel_debug!("aggregate", "Discarding aggregated group {} on completion timeout", k);
                    }
                } else {
                    self.complete_group(&mut st, &k, "timeout", &mut out);
                }
            }
            if let (Some(iv), Some(next)) = (self.interval, st.next_interval) {
                if next <= now {
                    self.complete_all(&mut st, "interval", &mut out);
                    st.next_interval = Some(now + iv);
                }
            }
            if !out.is_empty() {
                if let Some(t) = self.timeout {
                    let ms = t.as_millis() as u64;
                    for c in out.iter().filter(|c| c.by == "timeout") {
                        let n = aggregated_size(&c.exchange);
                        self.strategy.timeout(&c.exchange, n, n, ms);
                    }
                }
                drop(st);
                self.emit(out);
                st = self.state.lock().unwrap();
                continue;
            }
            let mut wake = st.deadlines.values().min().copied();
            if let Some(n) = st.next_interval {
                wake = Some(wake.map_or(n, |w| w.min(n)));
            }
            let wait = match wake {
                Some(w) => w.saturating_duration_since(Instant::now()),
                None => Duration::from_secs(3600),
            };
            st = self.cond.wait_timeout(st, wait.max(Duration::from_millis(1))).unwrap().0;
        }
    }
}

/// The aggregate processor (also a route service running the timeout checker).
pub struct AggregateProcessor {
    inner: Arc<AggInner>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl AggregateProcessor {
    /// Number of groups in progress.
    pub fn in_progress_groups(&self) -> usize {
        self.inner.repo.keys().len()
    }

    /// Force completion of all groups now (completed by `force`).
    pub fn force_completion(&self) {
        let mut out = Vec::new();
        {
            let mut st = self.inner.state.lock().unwrap();
            self.inner.complete_all(&mut st, "force", &mut out);
        }
        self.inner.emit(out);
    }
}

impl Processor for AggregateProcessor {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        self.inner.process(exchange)
    }
}

impl Service for AggregateProcessor {
    fn start(&self) -> Result<()> {
        let needs_thread = self.inner.timeout.is_some() || self.inner.timeout_expr.is_some() || self.inner.interval.is_some();
        {
            let mut st = self.inner.state.lock().unwrap();
            st.running = true;
            st.next_interval = self.inner.interval.map(|i| Instant::now() + i);
        }
        if needs_thread {
            let mut t = self.thread.lock().unwrap();
            if t.is_none() {
                let inner = self.inner.clone();
                let h = std::thread::Builder::new()
                    .name(format!("Camel ({}) thread - AggregateTimeoutChecker", self.inner.route_id))
                    .spawn(move || inner.run_checker())
                    .map_err(|e| CamelError::illegal_state(format!("Cannot start aggregate timeout checker: {}", e)))?;
                *t = Some(h);
            }
        }
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        {
            let mut st = self.inner.state.lock().unwrap();
            st.running = false;
            self.inner.cond.notify_all();
        }
        if let Some(h) = self.thread.lock().unwrap().take() {
            if h.thread().id() != std::thread::current().id() {
                let _ = h.join();
            }
        }
        if self.inner.force_on_stop {
            self.force_completion();
        }
        Ok(())
    }
}

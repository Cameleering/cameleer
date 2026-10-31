//! Resequencer EIP (batch and stream modes).
//!
//! Like Camel, the incoming exchange is "consumed" by the resequencer: copies are
//! buffered and later routed (in order) through the block's outputs, either on the
//! caller thread (batch full / in-order successor available) or on a background
//! thread (timeouts). The incoming exchange continues after the block unchanged.
//!
//! - **batch** (default): collect up to `batch_size` exchanges (default 100) or until
//!   `batch_timeout` (default 1s) elapsed since the first one, sort them by the
//!   expression and emit. Duplicates (equal keys) are dropped unless
//!   `allow_duplicates`; `reverse` sorts descending.
//! - **stream**: the expression gives a sequence number (integer). Exchanges are
//!   emitted as soon as they are the successor of the last emitted one; otherwise
//!   they wait at most `timeout` (default 1s) for the gap to be filled. At most
//!   `capacity` (default 1000) exchanges are buffered (the oldest is emitted when
//!   full). `reject_old` fails exchanges older than the last delivered one.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::builder::ExpressionDefinition;
use crate::error::{CamelError, Result};
use crate::impl_definition_boilerplate;
use crate::message::Exchange;
use crate::model::{OutputList, ProcessorDefinition, RouteContext};
use crate::spi::{Expression, Processor, Service};
use crate::value::Value;

use super::options::{EipOption, EipOptions};
use super::sort::natural_order;
use super::support::{ensure_kind, run_processor};

/// Exception kind for exchanges rejected by the stream resequencer.
pub const MESSAGE_REJECTED: &str = "MessageRejectedException";

/// Resequencer configuration.
#[derive(Clone, Debug)]
pub enum ResequencerConfig {
    Batch { batch_size: usize, batch_timeout: Duration, allow_duplicates: bool, reverse: bool, ignore_invalid_exchanges: bool },
    Stream { capacity: usize, timeout: Duration, reject_old: bool, ignore_invalid_exchanges: bool },
}

impl ResequencerConfig {
    pub fn batch() -> Self {
        ResequencerConfig::Batch {
            batch_size: 100,
            batch_timeout: Duration::from_millis(1000),
            allow_duplicates: false,
            reverse: false,
            ignore_invalid_exchanges: false,
        }
    }
    pub fn stream() -> Self {
        ResequencerConfig::Stream { capacity: 1000, timeout: Duration::from_millis(1000), reject_old: false, ignore_invalid_exchanges: false }
    }
}

/// Resequence block.
pub struct ResequenceDefinition {
    pub id: Option<String>,
    pub expression: ExpressionDefinition,
    pub config: ResequencerConfig,
    pub outputs: OutputList,
}

impl ResequenceDefinition {
    pub fn new(expression: ExpressionDefinition) -> Self {
        ResequenceDefinition { id: None, expression, config: ResequencerConfig::batch(), outputs: OutputList::default() }
    }
}

impl EipOptions for ResequenceDefinition {
    fn apply_option(&mut self, option: &EipOption) -> bool {
        match (option, &mut self.config) {
            (EipOption::Timeout(d), ResequencerConfig::Batch { batch_timeout, .. }) => *batch_timeout = *d,
            (EipOption::Timeout(d), ResequencerConfig::Stream { timeout, .. }) => *timeout = *d,
            _ => return false,
        }
        true
    }
}

impl ProcessorDefinition for ResequenceDefinition {
    fn name(&self) -> &str {
        "resequence"
    }
    fn label(&self) -> String {
        format!("resequence[{:?}]", self.expression)
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
        ensure_kind(MESSAGE_REJECTED, crate::error::kinds::CAMEL_EXCHANGE);
        let inner = Arc::new(ReseqInner {
            expression: self.expression.resolve_expression(&rc.context)?,
            config: self.config.clone(),
            output: rc.create_child_processor(&self.outputs.outputs)?,
            route_id: rc.route_id.clone(),
            state: Mutex::new(ReseqState::default()),
            cond: Condvar::new(),
            emit_lock: Mutex::new(()),
        });
        let p = Arc::new(ResequenceProcessor { inner, thread: Mutex::new(None) });
        rc.add_service(p.clone());
        Ok(p)
    }
    impl_definition_boilerplate!();
}

#[derive(Default)]
struct ReseqState {
    running: bool,
    // batch mode
    batch: Vec<(Value, Exchange)>,
    batch_started: Option<Instant>,
    // stream mode: seq -> (exchange, deadline)
    stream: BTreeMap<i64, (Exchange, Instant)>,
    last_delivered: Option<i64>,
}

struct ReseqInner {
    expression: Arc<dyn Expression>,
    config: ResequencerConfig,
    output: Arc<dyn Processor>,
    route_id: String,
    state: Mutex<ReseqState>,
    cond: Condvar,
    /// Serializes emission so that output order is preserved across threads.
    emit_lock: Mutex<()>,
}

impl ReseqInner {
    fn send(&self, list: Vec<Exchange>) {
        for mut ex in list {
            run_processor(self.output.as_ref(), &mut ex);
            if let Some(e) = &ex.exception {
                crate::camel_warn!("resequence", "Error processing resequenced exchange {} on route {}: {}", ex.exchange_id, self.route_id, e);
            }
            ex.run_on_completions();
        }
    }

    fn sort_batch(&self, mut batch: Vec<(Value, Exchange)>) -> Vec<Exchange> {
        let (allow_dup, reverse) = match &self.config {
            ResequencerConfig::Batch { allow_duplicates, reverse, .. } => (*allow_duplicates, *reverse),
            _ => (true, false),
        };
        batch.sort_by(|a, b| natural_order(&a.0, &b.0));
        if !allow_dup {
            batch.dedup_by(|b, a| natural_order(&a.0, &b.0) == std::cmp::Ordering::Equal);
        }
        if reverse {
            batch.reverse();
        }
        batch.into_iter().map(|(_, e)| e).collect()
    }

    /// Drain in-order stream elements (successors of the last delivered one).
    fn drain_successors(st: &mut ReseqState, out: &mut Vec<Exchange>) {
        while let Some(last) = st.last_delivered {
            match st.stream.remove(&(last + 1)) {
                Some((ex, _)) => {
                    out.push(ex);
                    st.last_delivered = Some(last + 1);
                }
                None => break,
            }
        }
    }

    /// Deliver all stream elements up to and including `seq`.
    fn release_up_to(st: &mut ReseqState, seq: i64, out: &mut Vec<Exchange>) {
        let keys: Vec<i64> = st.stream.range(..=seq).map(|(k, _)| *k).collect();
        for k in keys {
            if let Some((ex, _)) = st.stream.remove(&k) {
                out.push(ex);
                st.last_delivered = Some(st.last_delivered.map_or(k, |l| l.max(k)));
            }
        }
        Self::drain_successors(st, out);
    }

    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        let ignore_invalid = match &self.config {
            ResequencerConfig::Batch { ignore_invalid_exchanges, .. } => *ignore_invalid_exchanges,
            ResequencerConfig::Stream { ignore_invalid_exchanges, .. } => *ignore_invalid_exchanges,
        };
        let key = match self.expression.evaluate(exchange) {
            Ok(v) if !v.is_null() => v,
            Ok(_) | Err(_) if ignore_invalid => return Ok(()),
            Ok(_) => return Err(CamelError::new(crate::error::kinds::CAMEL_EXCHANGE, "Resequencer expression evaluated to null")),
            Err(e) => return Err(e),
        };
        let copy = exchange.correlated_copy();
        let _emit = self.emit_lock.lock().unwrap();
        let mut out = Vec::new();
        {
            let mut st = self.state.lock().unwrap();
            match &self.config {
                ResequencerConfig::Batch { batch_size, .. } => {
                    st.batch.push((key, copy));
                    if st.batch.len() >= *batch_size {
                        let batch = std::mem::take(&mut st.batch);
                        st.batch_started = None;
                        out = self.sort_batch(batch);
                    } else if st.batch_started.is_none() {
                        st.batch_started = Some(Instant::now());
                        self.cond.notify_all();
                    }
                }
                ResequencerConfig::Stream { capacity, timeout, reject_old, .. } => {
                    let seq = match key.as_i64() {
                        Some(s) => s,
                        None if ignore_invalid => return Ok(()),
                        None => {
                            return Err(CamelError::new(
                                crate::error::kinds::CAMEL_EXCHANGE,
                                format!("Resequencer sequence number is not a number: {}", key),
                            ))
                        }
                    };
                    if let Some(last) = st.last_delivered {
                        if seq <= last && *reject_old {
                            return Err(CamelError::new(
                                MESSAGE_REJECTED,
                                format!("Rejecting old message with sequence {} (last delivered {})", seq, last),
                            ));
                        }
                    }
                    st.stream.insert(seq, (copy, Instant::now() + *timeout));
                    Self::drain_successors(&mut st, &mut out);
                    while st.stream.len() > *capacity {
                        let first = *st.stream.keys().next().unwrap();
                        Self::release_up_to(&mut st, first, &mut out);
                    }
                    self.cond.notify_all();
                }
            }
        }
        self.send(out);
        Ok(())
    }

    fn run_timer(self: Arc<Self>) {
        let mut st = self.state.lock().unwrap();
        loop {
            if !st.running {
                break;
            }
            let now = Instant::now();
            let wake = match &self.config {
                ResequencerConfig::Batch { batch_timeout, .. } => st.batch_started.map(|s| s + *batch_timeout),
                ResequencerConfig::Stream { .. } => st.stream.values().map(|(_, d)| *d).min(),
            };
            match wake {
                Some(w) if w <= now => {
                    drop(st);
                    {
                        let _emit = self.emit_lock.lock().unwrap();
                        let mut out = Vec::new();
                        {
                            let mut s = self.state.lock().unwrap();
                            match &self.config {
                                ResequencerConfig::Batch { batch_timeout, .. } => {
                                    if s.batch_started.map(|b| b + *batch_timeout <= Instant::now()).unwrap_or(false) {
                                        let batch = std::mem::take(&mut s.batch);
                                        s.batch_started = None;
                                        out = self.sort_batch(batch);
                                    }
                                }
                                ResequencerConfig::Stream { .. } => {
                                    let now = Instant::now();
                                    let expired = s.stream.iter().filter(|(_, (_, d))| *d <= now).map(|(k, _)| *k).max();
                                    if let Some(seq) = expired {
                                        Self::release_up_to(&mut s, seq, &mut out);
                                    }
                                }
                            }
                        }
                        self.send(out);
                    }
                    st = self.state.lock().unwrap();
                }
                Some(w) => {
                    st = self.cond.wait_timeout(st, (w - now).max(Duration::from_millis(1))).unwrap().0;
                }
                None => {
                    st = self.cond.wait_timeout(st, Duration::from_secs(3600)).unwrap().0;
                }
            }
        }
    }

    /// Emit everything that is buffered (on stop).
    fn flush(&self) {
        let _emit = self.emit_lock.lock().unwrap();
        let out = {
            let mut st = self.state.lock().unwrap();
            let batch = std::mem::take(&mut st.batch);
            st.batch_started = None;
            let mut out = self.sort_batch(batch);
            let stream = std::mem::take(&mut st.stream);
            if let Some(last) = stream.keys().last() {
                st.last_delivered = Some(*last);
            }
            out.extend(stream.into_values().map(|(e, _)| e));
            out
        };
        self.send(out);
    }
}

/// The resequencer processor (also a route service running the timer thread).
pub struct ResequenceProcessor {
    inner: Arc<ReseqInner>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Processor for ResequenceProcessor {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        self.inner.process(exchange)
    }
}

impl Service for ResequenceProcessor {
    fn start(&self) -> Result<()> {
        self.inner.state.lock().unwrap().running = true;
        let mut t = self.thread.lock().unwrap();
        if t.is_none() {
            let inner = self.inner.clone();
            let h = std::thread::Builder::new()
                .name(format!("Camel ({}) thread - Resequencer", self.inner.route_id))
                .spawn(move || inner.run_timer())
                .map_err(|e| CamelError::illegal_state(format!("Cannot start resequencer thread: {}", e)))?;
            *t = Some(h);
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
        self.inner.flush();
        Ok(())
    }
}

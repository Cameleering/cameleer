//! Multicast EIP and the shared "multicast engine" used by split, multicast and
//! recipientList: process a list of sub-exchanges (sequentially or in parallel),
//! aggregate their results and merge the outcome back into the original exchange.
//!
//! Default results (as in Apache Camel 3/4):
//! - **split** without a strategy keeps the original exchange unchanged
//!   (`UseOriginalAggregationStrategy`), but a sub-exchange failure is propagated.
//! - **multicast** and **recipientList** without a strategy use the *last reply*
//!   as the outgoing message (`UseLatestAggregationStrategy`); a failure of any
//!   sub-exchange sticks to the result.

use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::error::{kinds, CamelError, Result};
use crate::impl_definition_boilerplate;
use crate::message::{props, Exchange};
use crate::model::{OutputList, ProcessorDefinition, RouteContext};
use crate::spi::{AggregationStrategy, Processor};

use super::options::{EipOption, EipOptions, MulticastSettings};
use super::strategies::UseLatestAggregationStrategy;
use super::support::{copy_results, run_processor, pool_name, LazyPool, SavedProperties, DEFAULT_POOL_SIZE};

/// Properties of the original exchange that are restored after merging results.
const RESTORED_PROPS: &[&str] = &[
    props::CORRELATION_ID,
    props::SPLIT_INDEX,
    props::SPLIT_SIZE,
    props::SPLIT_COMPLETE,
    props::MULTICAST_INDEX,
    props::MULTICAST_COMPLETE,
];

/// Runtime engine shared by split / multicast / recipientList.
pub struct MulticastEngine {
    parallel: bool,
    streaming: bool,
    stop_on_exception: bool,
    share_unit_of_work: bool,
    timeout: Option<Duration>,
    /// None = UseOriginal semantics (original kept, failures propagated).
    strategy: Option<Arc<dyn AggregationStrategy>>,
    pool: Option<Arc<LazyPool>>,
}

impl MulticastEngine {
    /// Build an engine from settings; registers the thread pool (if parallel) as a route service.
    /// `default_latest`: use `UseLatestAggregationStrategy` when no strategy is configured.
    pub fn create(rc: &RouteContext, s: &MulticastSettings, name: &str, default_latest: bool) -> Result<Self> {
        let strategy = match &s.strategy {
            Some(sd) => Some(sd.resolve(&rc.context)?),
            None if default_latest => Some(Arc::new(UseLatestAggregationStrategy) as Arc<dyn AggregationStrategy>),
            None => None,
        };
        let pool = if s.parallel_processing {
            let p = LazyPool::new(pool_name(&rc.route_id, name),s.pool_size.unwrap_or(DEFAULT_POOL_SIZE));
            rc.add_service(p.clone());
            Some(p)
        } else {
            None
        };
        Ok(MulticastEngine {
            parallel: s.parallel_processing,
            streaming: s.streaming,
            stop_on_exception: s.stop_on_exception,
            share_unit_of_work: s.share_unit_of_work,
            timeout: s.timeout,
            strategy,
            pool,
        })
    }

    fn finish_sub(&self, original: &mut Exchange, mut sub: Exchange) -> Exchange {
        if self.share_unit_of_work {
            original.on_completions.extend(std::mem::take(&mut sub.on_completions));
        } else {
            sub.run_on_completions();
        }
        sub
    }

    fn stop_with(&self, original: &mut Exchange, mut sub: Exchange, index: usize) {
        if let Some(e) = sub.exception.take() {
            let msg = format!(
                "{} processing failed for number {}. Exchange[{}]",
                if self.parallel { "Parallel" } else { "Sequential" },
                index,
                sub.exchange_id
            );
            sub.exception = Some(CamelError::new(kinds::CAMEL_EXCHANGE, msg).with_cause(e));
        }
        copy_results(original, sub);
    }

    /// Process the `(sub-exchange, processor)` pairs and merge the result into `original`.
    pub fn execute(&self, original: &mut Exchange, mut pairs: Vec<(Exchange, Arc<dyn Processor>)>) -> Result<()> {
        if !self.share_unit_of_work {
            // each sub-exchange is a new unit of work: its "original message" (useOriginalMessage)
            // is the sub-exchange message itself (e.g. the split part)
            for (sub, _) in pairs.iter_mut() {
                crate::processor::errors::begin_child_unit_of_work(sub);
            }
        }
        let saved = SavedProperties::save(original, RESTORED_PROPS);
        let mut agg = Aggregation { strategy: self.strategy.clone(), result: None, first_error: None };
        let outcome = if self.parallel { self.run_parallel(original, pairs, &mut agg) } else { self.run_sequential(original, pairs, &mut agg) };
        match outcome {
            Err(e) => original.set_exception(e),
            Ok(true) => {} // stopped on exception: result already copied
            Ok(false) => match &self.strategy {
                Some(s) => {
                    if let Some(mut r) = agg.result.take() {
                        s.on_completion(&mut r);
                        copy_results(original, r);
                    }
                }
                None => {
                    if let Some(e) = agg.first_error.take() {
                        original.set_exception(e);
                    }
                }
            },
        }
        saved.restore(original);
        Ok(())
    }

    /// Returns Ok(true) if processing stopped because of stopOnException.
    fn run_sequential(&self, original: &mut Exchange, pairs: Vec<(Exchange, Arc<dyn Processor>)>, agg: &mut Aggregation) -> Result<bool> {
        for (i, (mut sub, p)) in pairs.into_iter().enumerate() {
            run_processor(p.as_ref(), &mut sub);
            let sub = self.finish_sub(original, sub);
            if self.stop_on_exception && (sub.is_failed() || sub.rollback_only) {
                self.stop_with(original, sub, i);
                return Ok(true);
            }
            agg.add(sub)?;
        }
        Ok(false)
    }

    fn run_parallel(&self, original: &mut Exchange, pairs: Vec<(Exchange, Arc<dyn Processor>)>, agg: &mut Aggregation) -> Result<bool> {
        let pool = self.pool.as_ref().ok_or_else(|| CamelError::illegal_state("No thread pool for parallel processing"))?;
        let total = pairs.len();
        let (tx, rx) = mpsc::channel::<(usize, Exchange)>();
        for (i, (sub, p)) in pairs.into_iter().enumerate() {
            let tx = tx.clone();
            pool.execute(move || {
                let mut sub = sub;
                run_processor(p.as_ref(), &mut sub);
                let _ = tx.send((i, sub));
            });
        }
        drop(tx);
        let deadline = self.timeout.map(|t| Instant::now() + t);
        let mut slots: Vec<Option<Exchange>> = (0..total).map(|_| None).collect();
        let mut done = vec![false; total];
        let mut next = 0usize;
        let mut received = 0usize;
        let mut timed_out = false;
        while received < total {
            let msg = match deadline {
                Some(d) => {
                    let now = Instant::now();
                    if now >= d {
                        timed_out = true;
                        break;
                    }
                    match rx.recv_timeout(d - now) {
                        Ok(m) => Some(m),
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            timed_out = true;
                            break;
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => None,
                    }
                }
                None => rx.recv().ok(),
            };
            let (i, sub) = match msg {
                Some(m) => m,
                None => break,
            };
            received += 1;
            done[i] = true;
            let sub = self.finish_sub(original, sub);
            if self.stop_on_exception && (sub.is_failed() || sub.rollback_only) {
                self.stop_with(original, sub, i);
                return Ok(true);
            }
            if self.streaming {
                agg.add(sub)?;
            } else {
                slots[i] = Some(sub);
                while next < total {
                    match slots[next].take() {
                        Some(s) => {
                            agg.add(s)?;
                            next += 1;
                        }
                        None => break,
                    }
                }
            }
        }
        // aggregate what is left (in order) after a timeout
        for s in slots.iter_mut().skip(next) {
            if let Some(s) = s.take() {
                agg.add(s)?;
            }
        }
        if received < total {
            if timed_out {
                let ms = self.timeout.map(|t| t.as_millis() as u64).unwrap_or(0);
                if let (Some(s), Some(r)) = (&self.strategy, agg.result.as_ref()) {
                    for (i, d) in done.iter().enumerate() {
                        if !d {
                            s.timeout(r, i, total, ms);
                        }
                    }
                }
                crate::camel_debug!("multicast", "Parallel processing timed out after {} ms ({} of {} done)", ms, received, total);
            } else {
                return Err(CamelError::new(
                    kinds::CAMEL_EXCHANGE,
                    format!("Parallel processing failed: {} of {} sub-exchanges did not complete", total - received, total),
                ));
            }
        }
        Ok(false)
    }
}

struct Aggregation {
    strategy: Option<Arc<dyn AggregationStrategy>>,
    result: Option<Exchange>,
    first_error: Option<CamelError>,
}

impl Aggregation {
    fn add(&mut self, sub: Exchange) -> Result<()> {
        match &self.strategy {
            Some(s) => {
                let old = self.result.take();
                self.result = Some(s.aggregate(old, sub)?);
            }
            None => {
                if self.first_error.is_none() {
                    if let Some(e) = sub.exception {
                        self.first_error = Some(e);
                    }
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// multicast definition
// ---------------------------------------------------------------------------

/// Multicast EIP: each output of the block receives a copy of the exchange.
/// Default result: the last reply (Camel `UseLatestAggregationStrategy`).
#[derive(Default)]
pub struct MulticastDefinition {
    pub id: Option<String>,
    pub outputs: OutputList,
    pub settings: MulticastSettings,
}

impl MulticastDefinition {
    pub fn new() -> Self {
        Self::default()
    }
}

impl EipOptions for MulticastDefinition {
    fn apply_option(&mut self, option: &EipOption) -> bool {
        self.settings.apply(option)
    }
}

struct MulticastProcessor {
    engine: MulticastEngine,
    processors: Vec<Arc<dyn Processor>>,
}

impl Processor for MulticastProcessor {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        let n = self.processors.len();
        let pairs = self
            .processors
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let mut sub = exchange.correlated_copy();
                sub.set_property(props::MULTICAST_INDEX, i);
                sub.set_property(props::MULTICAST_COMPLETE, i + 1 == n);
                (sub, p.clone())
            })
            .collect();
        self.engine.execute(exchange, pairs)
    }
}

impl ProcessorDefinition for MulticastDefinition {
    fn name(&self) -> &str {
        "multicast"
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
        let mut processors = Vec::new();
        for d in &self.outputs.outputs {
            processors.push(rc.create_processor(d.as_ref())?);
        }
        let engine = MulticastEngine::create(rc, &self.settings, "multicast", true)?;
        Ok(Arc::new(MulticastProcessor { engine, processors }))
    }
    impl_definition_boilerplate!();
}

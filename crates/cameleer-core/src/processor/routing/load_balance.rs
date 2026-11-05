//! Load Balancer EIP: the outputs of the block are the targets; a policy picks
//! which target(s) process each exchange.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::builder::ExpressionDefinition;
use crate::error::{CamelError, Result};
use crate::impl_definition_boilerplate;
use crate::message::Exchange;
use crate::model::{OutputList, ProcessorDefinition, RouteContext};
use crate::spi::{Expression, Processor};

use super::support::{copy_results, random_below, run_processor};

/// A custom load balancer (Camel `LoadBalancer`).
pub trait LoadBalancer: Send + Sync {
    /// Process the exchange using one or more of the given processors.
    fn process(&self, exchange: &mut Exchange, processors: &[Arc<dyn Processor>]) -> Result<()>;
}

/// The load balancing policy.
#[derive(Clone)]
pub enum LoadBalancerPolicy {
    RoundRobin,
    Random,
    /// Same key (expression) goes to the same target.
    Sticky(ExpressionDefinition),
    /// Weighted round robin (`round_robin = true`) or weighted random, with one ratio per target.
    Weighted { round_robin: bool, ratios: Vec<usize> },
    /// Send to all targets (a copy each).
    Topic,
    /// Failover to the next target on failure.
    Failover(FailoverSettings),
    Custom(Arc<dyn LoadBalancer>),
}

impl std::fmt::Debug for LoadBalancerPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadBalancerPolicy::RoundRobin => f.write_str("roundRobin"),
            LoadBalancerPolicy::Random => f.write_str("random"),
            LoadBalancerPolicy::Sticky(e) => write!(f, "sticky[{:?}]", e),
            LoadBalancerPolicy::Weighted { round_robin, ratios } => write!(f, "weighted[roundRobin={}, {:?}]", round_robin, ratios),
            LoadBalancerPolicy::Topic => f.write_str("topic"),
            LoadBalancerPolicy::Failover(s) => write!(f, "failover[{:?}]", s),
            LoadBalancerPolicy::Custom(_) => f.write_str("custom"),
        }
    }
}

/// Failover options.
#[derive(Clone, Debug)]
pub struct FailoverSettings {
    /// Exception kinds to fail over on (empty = any exception). Matched with `is_a`.
    pub exceptions: Vec<String>,
    /// -1 = unlimited, 0 = no failover, n = at most n failover attempts.
    pub maximum_failover_attempts: i32,
    /// Start at the next target for each exchange and wrap around when failing over.
    pub round_robin: bool,
    /// Remember the last good target and start there.
    pub sticky: bool,
}

impl Default for FailoverSettings {
    fn default() -> Self {
        FailoverSettings { exceptions: vec![], maximum_failover_attempts: -1, round_robin: false, sticky: false }
    }
}

/// Load balance block.
pub struct LoadBalanceDefinition {
    pub id: Option<String>,
    pub policy: LoadBalancerPolicy,
    pub outputs: OutputList,
}

impl Default for LoadBalanceDefinition {
    fn default() -> Self {
        LoadBalanceDefinition { id: None, policy: LoadBalancerPolicy::RoundRobin, outputs: OutputList::default() }
    }
}

// ---------------------------------------------------------------------------
// built-in balancers
// ---------------------------------------------------------------------------

struct RoundRobin(AtomicUsize);
impl LoadBalancer for RoundRobin {
    fn process(&self, exchange: &mut Exchange, ps: &[Arc<dyn Processor>]) -> Result<()> {
        if ps.is_empty() {
            return Ok(());
        }
        let i = self.0.fetch_add(1, Ordering::SeqCst) % ps.len();
        ps[i].process(exchange)
    }
}

struct RandomLb;
impl LoadBalancer for RandomLb {
    fn process(&self, exchange: &mut Exchange, ps: &[Arc<dyn Processor>]) -> Result<()> {
        if ps.is_empty() {
            return Ok(());
        }
        ps[random_below(ps.len())].process(exchange)
    }
}

struct Sticky {
    expression: Arc<dyn Expression>,
    map: Mutex<HashMap<String, usize>>,
    next: AtomicUsize,
}
impl LoadBalancer for Sticky {
    fn process(&self, exchange: &mut Exchange, ps: &[Arc<dyn Processor>]) -> Result<()> {
        if ps.is_empty() {
            return Ok(());
        }
        let key = self.expression.evaluate(exchange)?.to_string();
        let i = *self.map.lock().unwrap().entry(key).or_insert_with(|| self.next.fetch_add(1, Ordering::SeqCst)) % ps.len();
        ps[i].process(exchange)
    }
}

/// Weighted balancer following Camel's runtime-ratio algorithm.
struct Weighted {
    round_robin: bool,
    ratios: Vec<usize>,
    state: Mutex<(Vec<usize>, usize)>,
}
impl Weighted {
    fn choose(&self) -> usize {
        let mut st = self.state.lock().unwrap();
        if st.0.iter().all(|w| *w == 0) {
            st.0 = self.ratios.clone();
            st.1 = 0;
        }
        if self.round_robin {
            loop {
                if st.1 >= st.0.len() {
                    st.1 = 0;
                }
                let c = st.1;
                if st.0[c] > 0 {
                    st.0[c] -= 1;
                    st.1 += 1;
                    return c;
                }
                st.1 += 1;
            }
        } else {
            let candidates: Vec<usize> = (0..st.0.len()).filter(|i| st.0[*i] > 0).collect();
            let c = candidates[random_below(candidates.len())];
            st.0[c] -= 1;
            c
        }
    }
}
impl LoadBalancer for Weighted {
    fn process(&self, exchange: &mut Exchange, ps: &[Arc<dyn Processor>]) -> Result<()> {
        if ps.is_empty() {
            return Ok(());
        }
        let i = self.choose();
        ps[i].process(exchange)
    }
}

struct Topic;
impl LoadBalancer for Topic {
    fn process(&self, exchange: &mut Exchange, ps: &[Arc<dyn Processor>]) -> Result<()> {
        for p in ps {
            let mut copy = exchange.clone();
            run_processor(p.as_ref(), &mut copy);
            exchange.on_completions.extend(std::mem::take(&mut copy.on_completions));
            if let Some(e) = copy.exception {
                if exchange.exception.is_none() {
                    exchange.exception = Some(e);
                }
            }
        }
        Ok(())
    }
}

struct Failover {
    s: FailoverSettings,
    counter: AtomicUsize,
    last_good: Mutex<Option<usize>>,
}
impl Failover {
    fn should_failover(&self, e: &CamelError) -> bool {
        self.s.exceptions.is_empty() || self.s.exceptions.iter().any(|k| e.chain().any(|c| c.is_a(k)))
    }
}
impl LoadBalancer for Failover {
    fn process(&self, exchange: &mut Exchange, ps: &[Arc<dyn Processor>]) -> Result<()> {
        let n = ps.len();
        if n == 0 {
            return Ok(());
        }
        let mut index = if self.s.sticky {
            self.last_good.lock().unwrap().unwrap_or(0)
        } else if self.s.round_robin {
            self.counter.fetch_add(1, Ordering::SeqCst) % n
        } else {
            0
        };
        let original = exchange.clone();
        let mut attempts: i64 = 0;
        loop {
            let mut attempt = original.clone();
            run_processor(ps[index].as_ref(), &mut attempt);
            let failed_over = match &attempt.exception {
                None => false,
                Some(e) => {
                    let max = self.s.maximum_failover_attempts as i64;
                    let exhausted = max >= 0 && attempts >= max;
                    // without round robin every target is tried at most once
                    let tried_all = !self.s.round_robin && attempts + 1 >= n as i64;
                    self.should_failover(e) && !exhausted && !tried_all
                }
            };
            if !failed_over {
                if attempt.exception.is_none() && self.s.sticky {
                    *self.last_good.lock().unwrap() = Some(index);
                }
                copy_results(exchange, attempt);
                return Ok(());
            }
            crate::camel_debug!("failover", "Failover attempt {} from target {} for exchange {}", attempts + 1, index, exchange.exchange_id);
            attempts += 1;
            index = (index + 1) % n;
        }
    }
}

struct LoadBalanceProcessor {
    balancer: Arc<dyn LoadBalancer>,
    processors: Vec<Arc<dyn Processor>>,
}

impl Processor for LoadBalanceProcessor {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        self.balancer.process(exchange, &self.processors)
    }
}

impl ProcessorDefinition for LoadBalanceDefinition {
    fn name(&self) -> &str {
        "loadBalance"
    }
    fn label(&self) -> String {
        format!("loadBalance[{:?}]", self.policy)
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
        let balancer: Arc<dyn LoadBalancer> = match &self.policy {
            LoadBalancerPolicy::RoundRobin => Arc::new(RoundRobin(AtomicUsize::new(0))),
            LoadBalancerPolicy::Random => Arc::new(RandomLb),
            LoadBalancerPolicy::Sticky(e) => {
                Arc::new(Sticky { expression: e.resolve_expression(&rc.context)?, map: Mutex::new(HashMap::new()), next: AtomicUsize::new(0) })
            }
            LoadBalancerPolicy::Weighted { round_robin, ratios } => {
                if ratios.len() != processors.len() {
                    return Err(CamelError::illegal_argument(format!(
                        "Loadbalacing with {} should match number of distributions {}",
                        processors.len(),
                        ratios.len()
                    )));
                }
                if ratios.iter().all(|r| *r == 0) {
                    return Err(CamelError::illegal_argument("Weighted load balancer requires at least one positive ratio"));
                }
                Arc::new(Weighted { round_robin: *round_robin, ratios: ratios.clone(), state: Mutex::new((ratios.clone(), 0)) })
            }
            LoadBalancerPolicy::Topic => Arc::new(Topic),
            LoadBalancerPolicy::Failover(s) => Arc::new(Failover { s: s.clone(), counter: AtomicUsize::new(0), last_good: Mutex::new(None) }),
            LoadBalancerPolicy::Custom(c) => c.clone(),
        };
        Ok(Arc::new(LoadBalanceProcessor { balancer, processors }))
    }
    impl_definition_boilerplate!();
}

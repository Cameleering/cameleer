//! Circuit breaker EIP (resilience4j-style state machine: CLOSED / OPEN / HALF_OPEN).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::error::{kinds, CamelError, Result};
use crate::impl_definition_boilerplate;
use crate::message::{props, Exchange};
use crate::model::{OutputList, ProcessorDefinition, RouteContext};
use crate::processor::pipeline::{process_safely, Pipeline};
use crate::spi::Processor;

use super::{enter_suppressed, exit_suppressed, set_caught_exception};

/// Circuit breaker settings (names follow Camel's resilience4j configuration).
#[derive(Clone, Debug)]
pub struct CircuitBreakerConfig {
    /// Failure rate (percent) at or above which the breaker opens (default 50).
    pub failure_rate_threshold: f64,
    /// Minimum calls before the failure rate is evaluated (default 100).
    pub minimum_number_of_calls: usize,
    /// Count-based sliding window size (default 100).
    pub sliding_window_size: usize,
    /// How long the breaker stays open before moving to half-open (default 60s).
    pub wait_duration_in_open_state: Duration,
    /// Calls permitted in half-open state (default 10).
    pub permitted_number_of_calls_in_half_open_state: usize,
    pub timeout_enabled: bool,
    /// Timeout of the protected calls when `timeout_enabled` (default 1s).
    pub timeout_duration: Duration,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        CircuitBreakerConfig {
            failure_rate_threshold: 50.0,
            minimum_number_of_calls: 100,
            sliding_window_size: 100,
            wait_duration_in_open_state: Duration::from_secs(60),
            permitted_number_of_calls_in_half_open_state: 10,
            timeout_enabled: false,
            timeout_duration: Duration::from_secs(1),
        }
    }
}

/// Circuit breaker state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CircuitBreakerState {
    Closed,
    Open,
    HalfOpen,
}

impl CircuitBreakerState {
    pub fn as_str(&self) -> &'static str {
        match self {
            CircuitBreakerState::Closed => "CLOSED",
            CircuitBreakerState::Open => "OPEN",
            CircuitBreakerState::HalfOpen => "HALF_OPEN",
        }
    }
}

/// The circuit breaker block with an optional `onFallback` section.
#[derive(Default)]
pub struct CircuitBreakerDefinition {
    pub id: Option<String>,
    pub config: CircuitBreakerConfig,
    pub outputs: OutputList,
    pub fallback: Option<OutputList>,
    /// Whether new outputs go to the fallback section.
    pub in_fallback: bool,
}

impl ProcessorDefinition for CircuitBreakerDefinition {
    fn name(&self) -> &str {
        "circuitBreaker"
    }
    fn is_block(&self) -> bool {
        true
    }
    fn add_output(&mut self, def: Box<dyn ProcessorDefinition>) -> Result<()> {
        if self.in_fallback {
            self.fallback.get_or_insert_with(OutputList::default).push(def);
        } else {
            self.outputs.push(def);
        }
        Ok(())
    }
    fn outputs(&self) -> Vec<&dyn ProcessorDefinition> {
        let mut v = self.outputs.refs();
        if let Some(f) = &self.fallback {
            v.extend(f.refs());
        }
        v
    }
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        let child: Arc<dyn Processor> =
            if self.outputs.outputs.is_empty() { Arc::new(Pipeline::new(vec![])) } else { rc.create_child_processor(&self.outputs.outputs)? };
        let fallback = match &self.fallback {
            Some(f) if !f.outputs.is_empty() => Some(rc.create_child_processor(&f.outputs)?),
            Some(_) => Some(Arc::new(Pipeline::new(vec![])) as Arc<dyn Processor>),
            None => None,
        };
        let name = self.id.clone().unwrap_or_else(|| rc.next_node_id("circuitBreaker"));
        Ok(Arc::new(CircuitBreakerProcessor { name, config: self.config.clone(), child, fallback, state: Mutex::new(StateMachine::default()) }))
    }
    impl_definition_boilerplate!();
}

#[derive(Debug)]
struct StateMachine {
    state: CircuitBreakerState,
    window: VecDeque<bool>,
    opened_at: Option<Instant>,
    half_open_permits: usize,
    half_open_results: Vec<bool>,
}

impl Default for StateMachine {
    fn default() -> Self {
        StateMachine { state: CircuitBreakerState::Closed, window: VecDeque::new(), opened_at: None, half_open_permits: 0, half_open_results: Vec::new() }
    }
}

fn failure_rate(results: &[bool]) -> f64 {
    if results.is_empty() {
        return 0.0;
    }
    results.iter().filter(|f| **f).count() as f64 * 100.0 / results.len() as f64
}

impl StateMachine {
    fn open(&mut self) {
        self.state = CircuitBreakerState::Open;
        self.opened_at = Some(Instant::now());
        self.half_open_results.clear();
        self.half_open_permits = 0;
    }
    fn close(&mut self) {
        self.state = CircuitBreakerState::Closed;
        self.window.clear();
        self.opened_at = None;
        self.half_open_results.clear();
        self.half_open_permits = 0;
    }

    /// Try to acquire permission for a call.
    fn acquire(&mut self, cfg: &CircuitBreakerConfig) -> bool {
        if self.state == CircuitBreakerState::Open {
            let elapsed = self.opened_at.map(|t| t.elapsed()).unwrap_or_default();
            if elapsed >= cfg.wait_duration_in_open_state {
                self.state = CircuitBreakerState::HalfOpen;
                self.half_open_permits = 0;
                self.half_open_results.clear();
            } else {
                return false;
            }
        }
        match self.state {
            CircuitBreakerState::Closed => true,
            CircuitBreakerState::HalfOpen => {
                if self.half_open_permits < cfg.permitted_number_of_calls_in_half_open_state.max(1) {
                    self.half_open_permits += 1;
                    true
                } else {
                    false
                }
            }
            CircuitBreakerState::Open => false,
        }
    }

    fn record(&mut self, failure: bool, cfg: &CircuitBreakerConfig) {
        match self.state {
            CircuitBreakerState::Closed => {
                let size = cfg.sliding_window_size.max(1);
                self.window.push_back(failure);
                while self.window.len() > size {
                    self.window.pop_front();
                }
                let min = cfg.minimum_number_of_calls.clamp(1, size);
                if self.window.len() >= min {
                    let results: Vec<bool> = self.window.iter().copied().collect();
                    if failure_rate(&results) >= cfg.failure_rate_threshold {
                        self.open();
                    }
                }
            }
            CircuitBreakerState::HalfOpen => {
                self.half_open_results.push(failure);
                if self.half_open_results.len() >= cfg.permitted_number_of_calls_in_half_open_state.max(1) {
                    if failure_rate(&self.half_open_results) >= cfg.failure_rate_threshold {
                        self.open();
                    } else {
                        self.close();
                    }
                }
            }
            CircuitBreakerState::Open => {}
        }
    }
}

struct CircuitBreakerProcessor {
    name: String,
    config: CircuitBreakerConfig,
    child: Arc<dyn Processor>,
    fallback: Option<Arc<dyn Processor>>,
    state: Mutex<StateMachine>,
}

impl CircuitBreakerProcessor {
    /// Run the protected children; returns true if the call timed out.
    fn run_child(&self, ex: &mut Exchange) -> bool {
        if !self.config.timeout_enabled {
            enter_suppressed(ex);
            process_safely(self.child.as_ref(), ex);
            exit_suppressed(ex);
            return false;
        }
        let mut copy = ex.clone();
        let child = self.child.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            enter_suppressed(&mut copy);
            process_safely(child.as_ref(), &mut copy);
            exit_suppressed(&mut copy);
            let _ = tx.send(copy);
        });
        match rx.recv_timeout(self.config.timeout_duration) {
            Ok(done) => {
                ex.message = done.message;
                ex.properties = done.properties;
                ex.exception = done.exception;
                ex.rollback_only = done.rollback_only;
                false
            }
            Err(_) => {
                ex.set_exception(CamelError::timeout(format!(
                    "CircuitBreaker '{}' timed out after {}ms",
                    self.name,
                    self.config.timeout_duration.as_millis()
                )));
                true
            }
        }
    }

    fn run_fallback(&self, ex: &mut Exchange, err: &CamelError) -> Result<()> {
        match &self.fallback {
            Some(fb) => {
                ex.exception = None;
                set_caught_exception(ex, err);
                ex.set_property(props::RESPONSE_FROM_FALLBACK, true);
                fb.process(ex)
            }
            None => {
                ex.set_property(props::RESPONSE_FROM_FALLBACK, false);
                Err(err.clone())
            }
        }
    }
}

impl Processor for CircuitBreakerProcessor {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let permitted = self.state.lock().unwrap().acquire(&self.config);
        if !permitted {
            ex.set_property(props::CIRCUIT_BREAKER_STATE, self.state.lock().unwrap().state.as_str());
            ex.set_property(props::RESPONSE_SUCCESSFUL_EXECUTION, false);
            ex.set_property(props::RESPONSE_SHORT_CIRCUITED, true);
            ex.set_property(props::RESPONSE_TIMED_OUT, false);
            let err = CamelError::new(kinds::CALL_NOT_PERMITTED, format!("CircuitBreaker '{}' is OPEN and does not permit further calls", self.name));
            return self.run_fallback(ex, &err);
        }
        let timed_out = self.run_child(ex);
        let failed = ex.is_failed();
        let state = {
            let mut s = self.state.lock().unwrap();
            s.record(failed, &self.config);
            s.state
        };
        ex.set_property(props::CIRCUIT_BREAKER_STATE, state.as_str());
        ex.set_property(props::RESPONSE_SHORT_CIRCUITED, false);
        ex.set_property(props::RESPONSE_TIMED_OUT, timed_out);
        ex.set_property(props::RESPONSE_SUCCESSFUL_EXECUTION, !failed);
        if !failed {
            ex.set_property(props::RESPONSE_FROM_FALLBACK, false);
            return Ok(());
        }
        let err = ex.exception.take().unwrap();
        self.run_fallback(ex, &err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_machine() {
        let cfg = CircuitBreakerConfig {
            minimum_number_of_calls: 2,
            sliding_window_size: 4,
            wait_duration_in_open_state: Duration::from_millis(30),
            permitted_number_of_calls_in_half_open_state: 1,
            ..Default::default()
        };
        let mut s = StateMachine::default();
        assert!(s.acquire(&cfg));
        s.record(false, &cfg);
        assert!(s.acquire(&cfg));
        s.record(true, &cfg); // 1/2 = 50% -> open
        assert_eq!(s.state, CircuitBreakerState::Open);
        assert!(!s.acquire(&cfg));
        std::thread::sleep(Duration::from_millis(40));
        assert!(s.acquire(&cfg));
        assert_eq!(s.state, CircuitBreakerState::HalfOpen);
        assert!(!s.acquire(&cfg)); // only one permitted
        s.record(false, &cfg);
        assert_eq!(s.state, CircuitBreakerState::Closed);
    }
}

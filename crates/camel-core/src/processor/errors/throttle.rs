//! Flow control: `throttle`, `delay`, `sample`.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::builder::ExpressionDefinition;
use crate::error::{kinds, CamelError, Result};
use crate::impl_definition_boilerplate;
use crate::message::Exchange;
use crate::model::{ProcessorDefinition, RouteContext};
use crate::spi::{Expression, Processor};
use crate::value::Value;

// ---------------------------------------------------------------------------
// throttle
// ---------------------------------------------------------------------------

/// Throttle options (shared with the DSL so options can be set after `throttle(...)`).
#[derive(Clone, Debug)]
pub struct ThrottleConfig {
    pub maximum_requests: usize,
    pub time_period: Duration,
    /// Fail with `ThrottlerRejectedExecutionException` instead of waiting.
    pub reject_execution: bool,
    /// Accepted for compatibility; delays are always applied on the calling thread.
    pub async_delayed: bool,
    /// Throttle per correlation key.
    pub correlation: Option<ExpressionDefinition>,
}

/// The throttle EIP: at most `maximum_requests` exchanges per `time_period`
/// (sliding time window), optionally per correlation key.
pub struct ThrottleDefinition {
    pub id: Option<String>,
    pub config: Arc<Mutex<ThrottleConfig>>,
}

impl ThrottleDefinition {
    pub fn new(maximum_requests: usize, time_period: Duration) -> Self {
        ThrottleDefinition {
            id: None,
            config: Arc::new(Mutex::new(ThrottleConfig { maximum_requests, time_period, reject_execution: false, async_delayed: false, correlation: None })),
        }
    }
}

impl ProcessorDefinition for ThrottleDefinition {
    fn name(&self) -> &str {
        "throttle"
    }
    fn label(&self) -> String {
        let c = self.config.lock().unwrap();
        format!("throttle[{} per {}ms]", c.maximum_requests, c.time_period.as_millis())
    }
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        let c = self.config.lock().unwrap().clone();
        if c.maximum_requests == 0 {
            return Err(CamelError::illegal_argument("throttle maximumRequests must be > 0"));
        }
        Ok(Arc::new(Throttler {
            max: c.maximum_requests,
            period: c.time_period,
            reject: c.reject_execution,
            correlation: c.correlation.as_ref().map(|e| e.resolve_expression(&rc.context)).transpose()?,
            windows: Mutex::new(HashMap::new()),
        }))
    }
    impl_definition_boilerplate!();
}

struct Throttler {
    max: usize,
    period: Duration,
    reject: bool,
    correlation: Option<Arc<dyn Expression>>,
    windows: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl Processor for Throttler {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let key = match &self.correlation {
            Some(e) => e.evaluate(ex)?.to_string(),
            None => String::new(),
        };
        loop {
            let wait = {
                let mut w = self.windows.lock().unwrap();
                let q = w.entry(key.clone()).or_default();
                let now = Instant::now();
                while let Some(front) = q.front() {
                    if now.duration_since(*front) >= self.period {
                        q.pop_front();
                    } else {
                        break;
                    }
                }
                if q.len() < self.max {
                    q.push_back(now);
                    return Ok(());
                }
                if self.reject {
                    return Err(CamelError::new(
                        kinds::THROTTLER_REJECTED,
                        format!("Exceeded the max throttle rate of {} within {}ms", self.max, self.period.as_millis()),
                    ));
                }
                (*q.front().unwrap() + self.period).saturating_duration_since(now)
            };
            std::thread::sleep(wait.max(Duration::from_millis(1)));
        }
    }
}

// ---------------------------------------------------------------------------
// delay
// ---------------------------------------------------------------------------

/// The delay EIP: waits for the number of millis computed by the expression
/// (an integer, or a duration string like `2s`).
pub struct DelayDefinition {
    pub id: Option<String>,
    pub expression: ExpressionDefinition,
    /// Accepted for compatibility; the delay is always applied on the calling thread.
    pub async_delayed: Arc<Mutex<bool>>,
}

impl ProcessorDefinition for DelayDefinition {
    fn name(&self) -> &str {
        "delay"
    }
    fn label(&self) -> String {
        format!("delay[{:?}]", self.expression)
    }
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        let e = self.expression.resolve_expression(&rc.context)?;
        Ok(Arc::new(move |ex: &mut Exchange| {
            let v = e.evaluate(ex)?;
            let d = match &v {
                Value::Null => Duration::ZERO,
                Value::Int(i) => Duration::from_millis((*i).max(0) as u64),
                Value::Float(f) => Duration::from_millis(f.max(0.0) as u64),
                other => crate::util::parse_duration(&other.to_string())?,
            };
            if d > Duration::ZERO {
                std::thread::sleep(d);
            }
            Ok(())
        }))
    }
    impl_definition_boilerplate!();
}

// ---------------------------------------------------------------------------
// sample
// ---------------------------------------------------------------------------

/// The sampling throttler: lets one exchange through per period (or every n-th exchange);
/// other exchanges are stopped.
pub struct SampleDefinition {
    pub id: Option<String>,
    pub period: Option<Duration>,
    pub message_frequency: Option<u64>,
}

impl ProcessorDefinition for SampleDefinition {
    fn name(&self) -> &str {
        "sample"
    }
    fn create_processor(&self, _rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        if let Some(n) = self.message_frequency {
            if n == 0 {
                return Err(CamelError::illegal_argument("sample messageFrequency must be > 0"));
            }
            let counter = Mutex::new(0u64);
            return Ok(Arc::new(move |ex: &mut Exchange| {
                let mut c = counter.lock().unwrap();
                *c += 1;
                if *c % n != 0 {
                    ex.set_route_stop(true);
                }
                Ok(())
            }));
        }
        let period = self.period.unwrap_or(Duration::from_secs(1));
        let last: Mutex<Option<Instant>> = Mutex::new(None);
        Ok(Arc::new(move |ex: &mut Exchange| {
            let mut l = last.lock().unwrap();
            let now = Instant::now();
            let pass = match *l {
                None => true,
                Some(t) => now.duration_since(t) >= period,
            };
            if pass {
                *l = Some(now);
            } else {
                ex.set_route_stop(true);
            }
            Ok(())
        }))
    }
    impl_definition_boilerplate!();
}

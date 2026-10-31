//! `timer:` component - generates message exchanges when a timer fires.
//!
//! `timer:name?period=1000&delay=1000&repeatCount=0&fixedRate=false&includeMetadata=false`
//!
//! Each fired exchange has a null body and the exchange property `CamelTimerCounter`
//! (1-based). With `includeMetadata=true` the properties `CamelTimerName`,
//! `CamelTimerPeriod`, `CamelTimerFiredTime` and the headers `CamelTimerFiredTime`,
//! `CamelTimerCounter` and `CamelMessageTimestamp` are set as well.

use std::any::Any;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::context::{CamelContext, WeakContext};
use crate::error::{CamelError, Result};
use crate::message::{props, ExchangePattern};
use crate::spi::{Component, Consumer, Endpoint, Processor, Service};
use crate::util::Parameters;
use crate::value::Value;

use super::support::{new_exchange, parse_duration_with_unit, process_consumer_exchange, BackgroundThreads, StopFlag};

/// The timer component.
#[derive(Default)]
pub struct TimerComponent;

/// Timer endpoint configuration.
#[derive(Clone, Debug)]
pub struct TimerConfig {
    pub name: String,
    pub period: Duration,
    pub delay: Duration,
    pub repeat_count: u64,
    pub fixed_rate: bool,
    pub include_metadata: bool,
    pub pattern: ExchangePattern,
    /// Absolute time (epoch millis) of the first fire, from the `time` option.
    pub time: Option<i64>,
}

impl Component for TimerComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let period = match params.take("period") {
            Some(v) => parse_duration_with_unit(&v, 1.0)?,
            None => Duration::from_millis(1000),
        };
        let delay = match params.take("delay") {
            Some(v) => parse_duration_with_unit(&v, 1.0)?,
            None => Duration::from_millis(1000),
        };
        let repeat_count = params.take_i64("repeatCount", 0)?.max(0) as u64;
        let fixed_rate = params.take_bool("fixedRate", false)?;
        let include_metadata = params.take_bool("includeMetadata", false)?;
        let pattern = match params.take("exchangePattern") {
            Some(p) => ExchangePattern::parse(&p)?,
            None => ExchangePattern::InOnly,
        };
        let time_pattern = params.take("pattern");
        let time = match params.take("time") {
            Some(t) => Some(parse_time(&t, time_pattern.as_deref())?),
            None => None,
        };
        for k in ["synchronous", "daemon", "runLoggingLevel", "timer", "bridgeErrorHandler", "exceptionHandler"] {
            params.take(k);
        }
        Ok(Arc::new(TimerEndpoint {
            uri: uri.to_string(),
            ctx: ctx.downgrade(),
            config: TimerConfig { name: remaining.to_string(), period, delay, repeat_count, fixed_rate, include_metadata, pattern, time },
        }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Parse the `time` option (`yyyy-MM-dd HH:mm:ss` or `yyyy-MM-dd'T'HH:mm:ss`) as UTC epoch millis.
fn parse_time(t: &str, _pattern: Option<&str>) -> Result<i64> {
    let err = || CamelError::illegal_argument(format!("Cannot parse timer time: {}", t));
    let t = t.trim().replace('T', " ");
    let (date, time) = t.split_once(' ').unwrap_or((t.as_str(), "00:00:00"));
    let d: Vec<&str> = date.split('-').collect();
    let h: Vec<&str> = time.split(':').collect();
    if d.len() != 3 {
        return Err(err());
    }
    let num = |s: &str| s.trim().parse::<u32>().map_err(|_| err());
    let year: i64 = d[0].trim().parse().map_err(|_| err())?;
    let (hh, mm, ss) = (num(h.first().unwrap_or(&"0"))?, num(h.get(1).unwrap_or(&"0"))?, num(h.get(2).unwrap_or(&"0"))?);
    Ok(crate::util_time::epoch_from_civil(year, num(d[1])?, num(d[2])?, hh, mm, ss) * 1000)
}

/// A timer endpoint (consumer only).
pub struct TimerEndpoint {
    uri: String,
    ctx: WeakContext,
    config: TimerConfig,
}

impl TimerEndpoint {
    pub fn config(&self) -> &TimerConfig {
        &self.config
    }
}

impl Endpoint for TimerEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        Ok(Arc::new(TimerConsumer { config: self.config.clone(), ctx: self.ctx.clone(), processor, threads: BackgroundThreads::new() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct TimerConsumer {
    config: TimerConfig,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
    threads: BackgroundThreads,
}

fn run_timer(config: &TimerConfig, ctx: &WeakContext, processor: &dyn Processor, flag: &StopFlag) {
    let logger = format!("timer://{}", config.name);
    let mut initial = config.delay;
    if let Some(t) = config.time {
        let now = crate::util_time::now_millis();
        initial = Duration::from_millis((t - now).max(0) as u64);
    }
    if flag.sleep(initial) {
        return;
    }
    let mut scheduled = Instant::now();
    let mut counter: u64 = 0;
    loop {
        if config.repeat_count > 0 && counter >= config.repeat_count {
            break;
        }
        counter += 1;
        let fired = Instant::now();
        let mut ex = new_exchange(ctx);
        ex.pattern = config.pattern;
        ex.set_property(props::TIMER_COUNTER, Value::from(counter));
        if config.include_metadata {
            let now = crate::util_time::now_millis();
            ex.set_property(props::TIMER_NAME, config.name.clone());
            ex.set_property(props::TIMER_PERIOD, Value::from(config.period.as_millis() as i64));
            ex.set_property(props::TIMER_FIRED_TIME, Value::from(now));
            ex.set_header(props::TIMER_FIRED_TIME, Value::from(now));
            ex.set_header(props::TIMER_COUNTER, Value::from(counter));
            ex.set_header("CamelMessageTimestamp", Value::from(now));
        }
        process_consumer_exchange(&logger, processor, &mut ex);
        if config.period.is_zero() && config.repeat_count == 0 {
            // period=0: fire once (like a one-shot java.util.Timer task)
            break;
        }
        if config.repeat_count > 0 && counter >= config.repeat_count {
            break;
        }
        if flag.is_stopped() {
            break;
        }
        let next = if config.fixed_rate {
            scheduled += config.period;
            scheduled
        } else {
            fired + config.period
        };
        if flag.sleep_until(next) {
            break;
        }
    }
}

impl Service for TimerConsumer {
    fn start(&self) -> Result<()> {
        let (config, ctx, processor) = (self.config.clone(), self.ctx.clone(), self.processor.clone());
        self.threads
            .spawn(&format!("Camel timer://{}", self.config.name), move |flag| run_timer(&config, &ctx, processor.as_ref(), &flag))
    }
    fn stop(&self) -> Result<()> {
        self.threads.stop();
        Ok(())
    }
}

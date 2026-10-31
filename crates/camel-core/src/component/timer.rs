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

use super::support::{new_exchange, parse_duration_with_unit, process_consumer_exchange, BackgroundThreads, StopFlag, ThreadSpawner};

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

/// Parse the `time` option (`yyyy-MM-dd HH:mm:ss` or `yyyy-MM-dd'T'HH:mm:ss`) as epoch millis.
/// Like Camel (`SimpleDateFormat`), the time is a local time of the default time zone
/// ([`crate::tz::TimeZone::local`], honouring `TZ`); a trailing `Z` or offset (`+02:00`) or a
/// bracketed zone id (`[Europe/Paris]`) selects another zone.
fn parse_time(t: &str, _pattern: Option<&str>) -> Result<i64> {
    let err = || CamelError::illegal_argument(format!("Cannot parse timer time: {}", t));
    let mut t = t.trim().to_string();
    let mut zone = crate::tz::TimeZone::local();
    if let Some(open) = t.find('[') {
        let id = t[open + 1..].trim_end_matches(']').to_string();
        zone = crate::tz::TimeZone::parse(&id)?;
        t.truncate(open);
    } else if let Some(stripped) = t.strip_suffix('Z') {
        zone = std::sync::Arc::new(crate::tz::TimeZone::utc());
        t = stripped.to_string();
    } else if t.len() > 19 && t.is_char_boundary(19) {
        if let Some(off) = crate::tz::parse_fixed_offset(&t[19..]) {
            zone = std::sync::Arc::new(crate::tz::TimeZone::fixed("offset", off));
            t.truncate(19);
        }
    }
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
    let local = crate::util_time::epoch_from_civil(year, num(d[1])?, num(d[2])?, hh, mm, ss);
    Ok(zone.resolve_local(local) * 1000)
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

/// Shared state of the timer threads of one consumer.
struct TimerRun {
    config: TimerConfig,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
    spawner: ThreadSpawner,
    name: String,
}

fn run_timer(run: Arc<TimerRun>, flag: Arc<StopFlag>) {
    let config = &run.config;
    let mut initial = config.delay;
    if let Some(t) = config.time {
        let now = crate::util_time::now_millis();
        initial = Duration::from_millis((t - now).max(0) as u64);
    }
    if flag.sleep(initial) {
        return;
    }
    timer_loop(run.clone(), flag, 0, Instant::now(), None);
}

/// The timer loop from a given state. `last_fired` is set when resuming after a fire (on a
/// replacement thread after an asynchronous delay released the previous one, see
/// [`crate::async_delay`]), so the schedule continues without waiting for that exchange.
fn timer_loop(run: Arc<TimerRun>, flag: Arc<StopFlag>, mut counter: u64, mut scheduled: Instant, mut last_fired: Option<Instant>) {
    let config = &run.config;
    let logger = format!("timer://{}", config.name);
    loop {
        if let Some(fired) = last_fired {
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
        if config.repeat_count > 0 && counter >= config.repeat_count {
            break;
        }
        counter += 1;
        let fired = Instant::now();
        let mut ex = new_exchange(&run.ctx);
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
        let (r2, f2) = (run.clone(), flag.clone());
        let on_release = move || {
            let r3 = r2.clone();
            if let Err(e) = r2.spawner.spawn(&r2.name, f2, move |flag| timer_loop(r3, flag, counter, scheduled, Some(fired))) {
                crate::camel_warn!("timer", "Cannot start a replacement timer thread for {}: {}", r2.name, e);
            }
        };
        let (_, released) = crate::async_delay::run_detachable(on_release, || process_consumer_exchange(&logger, run.processor.as_ref(), &mut ex));
        if released {
            return;
        }
        last_fired = Some(fired);
    }
}

impl Service for TimerConsumer {
    fn start(&self) -> Result<()> {
        let name = format!("Camel timer://{}", self.config.name);
        let run = Arc::new(TimerRun {
            config: self.config.clone(),
            ctx: self.ctx.clone(),
            processor: self.processor.clone(),
            spawner: self.threads.spawner(),
            name: name.clone(),
        });
        self.threads.spawn(&name, move |flag| run_timer(run, flag))
    }
    fn stop(&self) -> Result<()> {
        self.threads.stop();
        Ok(())
    }
}

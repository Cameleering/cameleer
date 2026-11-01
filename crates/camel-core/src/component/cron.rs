//! `cron:` component (Camel `camel-cron`) and the trigger runner shared with the
//! [`quartz`](super::quartz) component and the `scheduler=quartz` scheduled poll consumer.
//!
//! `cron:name?schedule=0/5+*+*+*+*+?&timeZone=Europe/Paris` (fixed offsets such as `GMT+02:00` and
//! zoneinfo region ids; without `timeZone` the default zone of the process is used)
//!
//! The `schedule` is a [cron expression](super::cron_expr) (`+` in the uri is decoded as a
//! space). Each fire sends an exchange with a null body and the Quartz headers
//! `fireTime`, `scheduledFireTime`, `nextFireTime`, `previousFireTime` (epoch millis,
//! Camel uses `java.util.Date`), `refireCount`, `jobRunTime`, `triggerName` and
//! `triggerGroup`, plus the exchange properties `CamelTimerName`, `CamelTimerFiredTime`
//! and `CamelTimerCounter`. Fires missed while the route was busy are skipped
//! (Quartz "do nothing" misfire policy).

use std::any::Any;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::context::{CamelContext, WeakContext};
use crate::error::{CamelError, Result};
use crate::message::{props, ExchangePattern};
use crate::spi::{Component, Consumer, Endpoint, Processor, Service, SuspendableService};
use crate::util::Parameters;
use crate::util_time::now_millis;
use crate::value::Value;

pub use super::cron_expr::{parse_time_zone, parse_zone, CronExpression};
use super::support::{new_exchange, process_consumer_exchange, BackgroundThreads, StopFlag, SuspendState};

/// Quartz header names set on fired exchanges.
pub mod headers {
    pub const FIRE_TIME: &str = "fireTime";
    pub const SCHEDULED_FIRE_TIME: &str = "scheduledFireTime";
    pub const NEXT_FIRE_TIME: &str = "nextFireTime";
    pub const PREVIOUS_FIRE_TIME: &str = "previousFireTime";
    pub const REFIRE_COUNT: &str = "refireCount";
    pub const JOB_RUN_TIME: &str = "jobRunTime";
    pub const TRIGGER_NAME: &str = "triggerName";
    pub const TRIGGER_GROUP: &str = "triggerGroup";
}

/// When a trigger fires.
#[derive(Clone, Debug)]
pub enum Trigger {
    /// Fire according to a cron expression.
    Cron(CronExpression),
    /// Fire every `interval`; `repeat_count` extra fires after the first one (-1 = forever).
    Simple { interval: Duration, repeat_count: i64 },
}

/// Configuration of a trigger-driven consumer.
#[derive(Clone, Debug)]
pub struct TriggerConfig {
    pub name: String,
    pub group: String,
    pub trigger: Trigger,
    /// Delay before the trigger becomes active (Quartz `triggerStartDelay`).
    pub start_delay: Duration,
    pub pattern: ExchangePattern,
    /// Logger / thread name.
    pub logger: String,
}

/// Sleep until the next fire time of `cron` strictly after `after_ms` and not before
/// "now". Returns the scheduled fire time (epoch millis), or None when stopped or when
/// the expression never fires again.
pub fn sleep_until_next_fire(cron: &CronExpression, after_ms: i64, flag: &StopFlag) -> Option<i64> {
    let base = after_ms.max(now_millis() - 1);
    let next = cron.next_after_millis(base)?;
    sleep_until_millis(next, flag).then_some(next)
}

/// Sleep until the wall clock reaches `epoch_ms`. Returns false if stopped first.
pub fn sleep_until_millis(epoch_ms: i64, flag: &StopFlag) -> bool {
    loop {
        let now = now_millis();
        if now >= epoch_ms {
            return !flag.is_stopped();
        }
        if flag.sleep_until(Instant::now() + Duration::from_millis((epoch_ms - now) as u64)) {
            return false;
        }
    }
}

/// Run a trigger loop on the current thread until stopped or the trigger is exhausted.
pub fn run_trigger(config: &TriggerConfig, ctx: &WeakContext, processor: &dyn Processor, flag: &StopFlag, suspend: &SuspendState) {
    let start = now_millis() + config.start_delay.as_millis() as i64;
    let mut counter: u64 = 0;
    let mut previous: Option<i64> = None;
    let fire = |scheduled: i64, next: Option<i64>, previous: Option<i64>, counter: u64| {
        if suspend.get() {
            return; // suspended: skip this fire
        }
        let fired = now_millis();
        let mut ex = new_exchange(ctx);
        ex.pattern = config.pattern;
        ex.set_header(headers::FIRE_TIME, Value::from(fired));
        ex.set_header(headers::SCHEDULED_FIRE_TIME, Value::from(scheduled));
        if let Some(n) = next {
            ex.set_header(headers::NEXT_FIRE_TIME, Value::from(n));
        }
        if let Some(p) = previous {
            ex.set_header(headers::PREVIOUS_FIRE_TIME, Value::from(p));
        }
        ex.set_header(headers::REFIRE_COUNT, Value::from(0));
        ex.set_header(headers::JOB_RUN_TIME, Value::from(-1));
        ex.set_header(headers::TRIGGER_NAME, config.name.clone());
        ex.set_header(headers::TRIGGER_GROUP, config.group.clone());
        ex.set_property(props::TIMER_NAME, config.name.clone());
        ex.set_property(props::TIMER_FIRED_TIME, Value::from(fired));
        ex.set_property(props::TIMER_COUNTER, Value::from(counter));
        process_consumer_exchange(&config.logger, processor, &mut ex);
    };
    match &config.trigger {
        Trigger::Cron(cron) => {
            let mut after = start - 1;
            loop {
                let Some(scheduled) = sleep_until_next_fire(cron, after, flag) else { return };
                counter += 1;
                fire(scheduled, cron.next_after_millis(scheduled), previous, counter);
                previous = Some(scheduled);
                after = scheduled;
                if flag.is_stopped() {
                    return;
                }
            }
        }
        Trigger::Simple { interval, repeat_count } => {
            let interval_ms = interval.as_millis() as i64;
            let mut scheduled = start;
            loop {
                if !sleep_until_millis(scheduled, flag) {
                    return;
                }
                counter += 1;
                let more = *repeat_count < 0 || (counter as i64) <= *repeat_count;
                let next = if more { Some(scheduled + interval_ms) } else { None };
                fire(scheduled, next, previous, counter);
                previous = Some(scheduled);
                if !more || flag.is_stopped() {
                    return;
                }
                scheduled += interval_ms.max(1);
                // skip misfired executions
                let now = now_millis();
                if scheduled < now && interval_ms > 0 {
                    scheduled += ((now - scheduled) / interval_ms) * interval_ms;
                }
            }
        }
    }
}

/// An endpoint driven by a [`TriggerConfig`] (used by `cron:` and `quartz:`).
pub struct TriggerEndpoint {
    uri: String,
    ctx: WeakContext,
    config: TriggerConfig,
}

impl TriggerEndpoint {
    pub fn new(uri: &str, ctx: &CamelContext, config: TriggerConfig) -> Self {
        TriggerEndpoint { uri: uri.to_string(), ctx: ctx.downgrade(), config }
    }
    pub fn config(&self) -> &TriggerConfig {
        &self.config
    }
}

impl Endpoint for TriggerEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        Ok(Arc::new(TriggerConsumer { config: self.config.clone(), ctx: self.ctx.clone(), processor, threads: BackgroundThreads::new(), suspend: SuspendState::new() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct TriggerConsumer {
    config: TriggerConfig,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
    threads: BackgroundThreads,
    suspend: Arc<SuspendState>,
}

impl Service for TriggerConsumer {
    fn start(&self) -> Result<()> {
        let (config, ctx, processor, suspend) = (self.config.clone(), self.ctx.clone(), self.processor.clone(), self.suspend.clone());
        self.threads.spawn(&format!("Camel {}", self.config.logger), move |flag| run_trigger(&config, &ctx, processor.as_ref(), &flag, &suspend))
    }
    fn stop(&self) -> Result<()> {
        self.threads.stop();
        self.suspend.set(false);
        Ok(())
    }
    fn as_suspendable(&self) -> Option<&dyn SuspendableService> {
        Some(&*self.suspend)
    }
}

pub(crate) fn take_pattern(params: &mut Parameters) -> Result<ExchangePattern> {
    match params.take("exchangePattern") {
        Some(p) => ExchangePattern::parse(&p),
        None => Ok(ExchangePattern::InOnly),
    }
}

/// The `cron` component.
#[derive(Default)]
pub struct CronComponent;

impl Component for CronComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let name = remaining.trim_matches('/').to_string();
        if name.is_empty() {
            return Err(CamelError::illegal_argument("The cron endpoint requires a name: cron:name?schedule=..."));
        }
        let schedule = params.take("schedule").ok_or_else(|| CamelError::illegal_argument("The cron endpoint requires the 'schedule' option"))?;
        let tz = params.take("timeZone").or_else(|| params.take("trigger.timeZone"));
        let cron = CronExpression::parse_with_default_zone(&schedule, tz.as_deref())?;
        let pattern = take_pattern(params)?;
        for k in ["bridgeErrorHandler", "exceptionHandler"] {
            params.take(k);
        }
        let config = TriggerConfig {
            logger: format!("cron://{}", name),
            name,
            group: "camel-cron".to_string(),
            trigger: Trigger::Cron(cron),
            start_delay: Duration::ZERO,
            pattern,
        };
        Ok(Arc::new(TriggerEndpoint::new(uri, ctx, config)))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

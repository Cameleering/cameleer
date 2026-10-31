//! `quartz:` component - cron and simple triggers (Camel `camel-quartz`, without a
//! Quartz scheduler: each endpoint runs its own trigger thread, there is no job store
//! or clustering).
//!
//! - `quartz://group/name?cron=0+0/5+12-18+?+*+MON-FRI&trigger.timeZone=GMT+02:00`
//! - `quartz:name?trigger.repeatInterval=1000&trigger.repeatCount=-1` (simple trigger:
//!   `repeatCount` extra fires after the first one, `-1` = forever, the default;
//!   `repeatInterval` defaults to 1000 ms)
//!
//! Other options: `triggerStartDelay` (ms, default 500; the first simple fire happens
//! after it unless `fireNow=true`), `startDelayedSeconds`, `exchangePattern`. Quartz job
//! options (`deleteJob`, `pauseJob`, `durableJob`, `stateful`, `recoverableJob`,
//! `usingFixedCamelContextName`, `customCalendar`, `job.*`, other `trigger.*`, ...)
//! are accepted and ignored. Headers: see the [`cron`](super::cron) component.

use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

use crate::context::CamelContext;
use crate::error::{CamelError, Result};
use crate::spi::{Component, Endpoint};
use crate::util::Parameters;

use super::cron::{take_pattern, CronExpression, Trigger, TriggerConfig, TriggerEndpoint};

/// The `quartz` component.
#[derive(Default)]
pub struct QuartzComponent;

impl Component for QuartzComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let path = remaining.trim_matches('/');
        let (group, name) = match path.split_once('/') {
            Some((g, n)) => (g.to_string(), n.to_string()),
            None => ("Camel".to_string(), path.to_string()),
        };
        if name.is_empty() {
            return Err(CamelError::illegal_argument("The quartz endpoint requires a timer name: quartz:group/name"));
        }
        let trigger_opts = params.take_prefixed("trigger.");
        let tz = trigger_opts.get("timeZone").cloned().or_else(|| params.take("timeZone"));
        let mut start_delay = Duration::from_millis(params.take_i64("triggerStartDelay", 500)?.max(0) as u64);
        let fire_now = params.take_bool("fireNow", false)?;
        let delayed_secs = params.take_i64("startDelayedSeconds", 0)?.max(0) as u64;
        let trigger = match params.take("cron") {
            Some(c) => Trigger::Cron(CronExpression::parse_with_default_zone(&c, tz.as_deref())?),
            None => {
                let num = |k: &str, d: i64| -> Result<i64> {
                    match trigger_opts.get(k) {
                        Some(v) => v.trim().parse().map_err(|_| CamelError::illegal_argument(format!("Invalid trigger.{}: {}", k, v))),
                        None => Ok(d),
                    }
                };
                let interval = num("repeatInterval", 1000)?.max(0) as u64;
                if fire_now {
                    start_delay = Duration::ZERO;
                }
                Trigger::Simple { interval: Duration::from_millis(interval), repeat_count: num("repeatCount", -1)? }
            }
        };
        start_delay += Duration::from_secs(delayed_secs);
        let pattern = take_pattern(params)?;
        params.take_prefixed("job.");
        for k in [
            "deleteJob",
            "pauseJob",
            "durableJob",
            "stateful",
            "recoverableJob",
            "usingFixedCamelContextName",
            "customCalendar",
            "jobParameters",
            "triggerParameters",
            "ignoreExpiredNextFireTime",
            "autoStartScheduler",
            "prefixJobNameWithEndpointId",
            "bridgeErrorHandler",
            "exceptionHandler",
        ] {
            params.take(k);
        }
        let config = TriggerConfig { logger: format!("quartz://{}/{}", group, name), name, group, trigger, start_delay, pattern };
        Ok(Arc::new(TriggerEndpoint::new(uri, ctx, config)))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

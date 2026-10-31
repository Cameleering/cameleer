//! Reusable scheduled poll consumer (Camel `ScheduledPollConsumer`), used by the
//! `scheduler` and `file` components.
//!
//! Supported options: `initialDelay`, `delay`, `useFixedDelay`, `repeatCount`, `greedy`,
//! `sendEmptyMessageWhenIdle`, `backoffMultiplier`, `backoffIdleThreshold`,
//! `backoffErrorThreshold`, `runLoggingLevel`, `startScheduler`, `timeUnit`,
//! `scheduler` (`spring`/`quartz`/`none`), `scheduler.cron` (a [cron expression](super::cron_expr),
//! with `scheduler.timeZone` / `scheduler.trigger.timeZone`; when set, polls happen at the
//! cron fire times and `initialDelay`/`delay`/backoff are ignored; other `scheduler.*` options
//! are accepted and ignored),
//! `pollStrategy`, `bridgeErrorHandler`, `exceptionHandler`, `exchangePattern` (accepted).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::error::{CamelError, Result};
use crate::log::Level;
use crate::spi::Service;
use crate::util::Parameters;

use super::support::{parse_duration_with_unit, time_unit_millis, BackgroundThreads, StopFlag};

/// Scheduling configuration of a [`ScheduledPollConsumer`].
#[derive(Clone, Debug)]
pub struct ScheduledPollConfig {
    pub initial_delay: Duration,
    pub delay: Duration,
    pub use_fixed_delay: bool,
    /// Maximum number of polls (0 = unlimited).
    pub repeat_count: u64,
    /// Poll again immediately when the previous poll processed messages.
    pub greedy: bool,
    pub send_empty_message_when_idle: bool,
    pub backoff_multiplier: u32,
    pub backoff_idle_threshold: u32,
    pub backoff_error_threshold: u32,
    pub run_logging_level: Level,
    /// If false the scheduler thread is not started; call [`ScheduledPollConsumer::poll_once`] manually.
    pub start_scheduler: bool,
    /// Poll at the fire times of this cron expression (`scheduler.cron`) instead of using delays.
    pub cron: Option<super::cron_expr::CronExpression>,
}

impl ScheduledPollConfig {
    pub fn new(initial_delay: Duration, delay: Duration) -> Self {
        ScheduledPollConfig {
            initial_delay,
            delay,
            use_fixed_delay: true,
            repeat_count: 0,
            greedy: false,
            send_empty_message_when_idle: false,
            backoff_multiplier: 0,
            backoff_idle_threshold: 0,
            backoff_error_threshold: 0,
            run_logging_level: Level::Trace,
            start_scheduler: true,
            cron: None,
        }
    }

    /// Consume the scheduled-poll options from endpoint parameters.
    pub fn from_params(params: &mut Parameters, default_initial: Duration, default_delay: Duration) -> Result<Self> {
        let unit = match params.take("timeUnit") {
            Some(u) => time_unit_millis(&u)?,
            None => 1.0,
        };
        let mut c = ScheduledPollConfig::new(default_initial, default_delay);
        if let Some(v) = params.take("initialDelay") {
            c.initial_delay = parse_duration_with_unit(&v, unit)?;
        }
        if let Some(v) = params.take("delay") {
            c.delay = parse_duration_with_unit(&v, unit)?;
        }
        c.use_fixed_delay = params.take_bool("useFixedDelay", true)?;
        c.repeat_count = params.take_u64("repeatCount", 0)?;
        c.greedy = params.take_bool("greedy", false)?;
        c.send_empty_message_when_idle = params.take_bool("sendEmptyMessageWhenIdle", false)?;
        c.backoff_multiplier = params.take_u64("backoffMultiplier", 0)? as u32;
        c.backoff_idle_threshold = params.take_u64("backoffIdleThreshold", 0)? as u32;
        c.backoff_error_threshold = params.take_u64("backoffErrorThreshold", 0)? as u32;
        if let Some(l) = params.take("runLoggingLevel") {
            c.run_logging_level = Level::parse(&l).ok_or_else(|| CamelError::illegal_argument(format!("Invalid runLoggingLevel: {}", l)))?;
        }
        c.start_scheduler = params.take_bool("startScheduler", true)?;
        if let Some(s) = params.take("scheduler") {
            match s.as_str() {
                "" | "none" | "spring" | "quartz" | "default" => {}
                other if other.starts_with('#') => {}
                other => return Err(CamelError::illegal_argument(format!("Unknown scheduler: {}", other))),
            }
        }
        let sched_opts = params.take_prefixed("scheduler.");
        if let Some(expr) = sched_opts.get("cron") {
            let tz = sched_opts.get("timeZone").or_else(|| sched_opts.get("trigger.timeZone"));
            c.cron = Some(super::cron_expr::CronExpression::parse_with_time_zone(expr, tz.map(|s| s.as_str()))?);
        }
        for k in ["pollStrategy", "bridgeErrorHandler", "exceptionHandler", "exchangePattern", "scheduledExecutorService", "poolSize"] {
            params.take(k);
        }
        if (c.backoff_idle_threshold > 0 || c.backoff_error_threshold > 0) && c.backoff_multiplier == 0 {
            return Err(CamelError::illegal_argument(
                "backoffMultiplier must be a positive value when backoffIdleThreshold or backoffErrorThreshold is in use",
            ));
        }
        Ok(c)
    }
}

type PollFn = Arc<dyn Fn() -> Result<usize> + Send + Sync>;
type EmptyFn = Arc<dyn Fn() + Send + Sync>;

struct PollInner {
    name: String,
    config: ScheduledPollConfig,
    poll: PollFn,
    empty: Option<EmptyFn>,
    polls: AtomicU64,
    lock: Mutex<()>,
}

impl PollInner {
    fn run_poll(&self) -> Result<usize> {
        let _g = self.lock.lock().unwrap();
        self.polls.fetch_add(1, Ordering::SeqCst);
        crate::log::log(self.config.run_logging_level, &self.name, "Scheduled task started");
        let r = (self.poll)();
        if let (Ok(0), true, Some(empty)) = (&r, self.config.send_empty_message_when_idle, &self.empty) {
            empty();
        }
        crate::log::log(self.config.run_logging_level, &self.name, "Scheduled task completed");
        r
    }

    fn run_cron_loop(&self, cron: &super::cron_expr::CronExpression, flag: &StopFlag) {
        let c = &self.config;
        let mut after = crate::util_time::now_millis() - 1;
        let mut count = 0u64;
        while c.repeat_count == 0 || count < c.repeat_count {
            let Some(scheduled) = super::cron::sleep_until_next_fire(cron, after, flag) else { return };
            after = scheduled;
            count += 1;
            loop {
                match self.run_poll() {
                    Ok(n) if n > 0 && c.greedy && !flag.is_stopped() => continue,
                    Ok(_) => {}
                    Err(e) => crate::camel_warn!(&self.name, "Consumer poll failed: {}", e),
                }
                break;
            }
            if flag.is_stopped() {
                return;
            }
        }
    }

    fn run_loop(&self, flag: &StopFlag) {
        let c = &self.config;
        if let Some(cron) = &c.cron {
            self.run_cron_loop(cron, flag);
            return;
        }
        if flag.sleep(c.initial_delay) {
            return;
        }
        let mut next = Instant::now();
        let mut count = 0u64;
        let (mut idle, mut errors, mut skip) = (0u32, 0u32, 0u32);
        loop {
            if c.repeat_count > 0 && count >= c.repeat_count {
                break;
            }
            if skip > 0 {
                skip -= 1;
                crate::camel_trace!(&self.name, "Backoff: skipping poll ({} left)", skip);
            } else {
                count += 1;
                loop {
                    match self.run_poll() {
                        Ok(n) => {
                            errors = 0;
                            if n == 0 {
                                idle += 1;
                            } else {
                                idle = 0;
                                if c.greedy && !flag.is_stopped() {
                                    continue;
                                }
                            }
                        }
                        Err(e) => {
                            idle = 0;
                            errors += 1;
                            crate::camel_warn!(&self.name, "Consumer poll failed: {}", e);
                        }
                    }
                    break;
                }
                if c.backoff_multiplier > 0 {
                    if c.backoff_idle_threshold > 0 && idle >= c.backoff_idle_threshold {
                        skip = c.backoff_multiplier;
                        idle = 0;
                    } else if c.backoff_error_threshold > 0 && errors >= c.backoff_error_threshold {
                        skip = c.backoff_multiplier;
                        errors = 0;
                    }
                }
            }
            if flag.is_stopped() {
                break;
            }
            if c.use_fixed_delay {
                if flag.sleep(c.delay) {
                    break;
                }
            } else {
                next += c.delay;
                let now = Instant::now();
                if next < now {
                    next = now;
                }
                if flag.sleep_until(next) {
                    break;
                }
            }
        }
    }
}

/// A consumer that runs a poll function on a background thread according to a
/// [`ScheduledPollConfig`]. The poll function returns the number of messages processed.
pub struct ScheduledPollConsumer {
    inner: Arc<PollInner>,
    threads: BackgroundThreads,
}

impl ScheduledPollConsumer {
    pub fn new(name: impl Into<String>, config: ScheduledPollConfig, poll: impl Fn() -> Result<usize> + Send + Sync + 'static) -> Self {
        ScheduledPollConsumer {
            inner: Arc::new(PollInner {
                name: name.into(),
                config,
                poll: Arc::new(poll),
                empty: None,
                polls: AtomicU64::new(0),
                lock: Mutex::new(()),
            }),
            threads: BackgroundThreads::new(),
        }
    }

    /// Callback used to send an empty message when a poll found nothing and
    /// `sendEmptyMessageWhenIdle` is enabled.
    pub fn with_empty_message(mut self, f: impl Fn() + Send + Sync + 'static) -> Self {
        Arc::get_mut(&mut self.inner).expect("configure before start").empty = Some(Arc::new(f));
        self
    }

    pub fn config(&self) -> &ScheduledPollConfig {
        &self.inner.config
    }

    /// Run a single poll now (e.g. with `startScheduler=false`).
    pub fn poll_once(&self) -> Result<usize> {
        self.inner.run_poll()
    }

    /// Number of polls executed so far.
    pub fn poll_count(&self) -> u64 {
        self.inner.polls.load(Ordering::SeqCst)
    }

    /// Whether the scheduler thread is still running.
    pub fn is_running(&self) -> bool {
        self.threads.is_running()
    }
}

impl Service for ScheduledPollConsumer {
    fn start(&self) -> Result<()> {
        if !self.inner.config.start_scheduler || self.threads.is_running() {
            return Ok(());
        }
        let inner = self.inner.clone();
        self.threads.spawn(&format!("Camel ({}) thread", self.inner.name), move |flag| inner.run_loop(&flag))
    }
    fn stop(&self) -> Result<()> {
        self.threads.stop();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn repeat_count_and_greedy() {
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        let mut cfg = ScheduledPollConfig::new(Duration::ZERO, Duration::from_millis(1));
        cfg.repeat_count = 3;
        let c = ScheduledPollConsumer::new("t", cfg, move || {
            n2.fetch_add(1, Ordering::SeqCst);
            Ok(0)
        });
        c.start().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while c.is_running() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        c.stop().unwrap();
        assert_eq!(n.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn cron_parsed_and_backoff_validated() {
        let mut p = Parameters::new(
            [("scheduler".to_string(), "quartz".to_string()), ("scheduler.cron".to_string(), "0 * * * *".to_string())].into_iter().collect(),
        );
        let c = ScheduledPollConfig::from_params(&mut p, Duration::ZERO, Duration::ZERO).unwrap();
        assert_eq!(c.cron.unwrap().expression(), "0 * * * *");
        assert!(p.is_empty());
        let mut p = Parameters::new([("scheduler.cron".to_string(), "bad cron".to_string())].into_iter().collect());
        assert!(ScheduledPollConfig::from_params(&mut p, Duration::ZERO, Duration::ZERO).is_err());
        let mut p = Parameters::new([("backoffIdleThreshold".to_string(), "2".to_string())].into_iter().collect());
        assert!(ScheduledPollConfig::from_params(&mut p, Duration::ZERO, Duration::ZERO).is_err());
        let mut p = Parameters::new(
            [("delay".to_string(), "2".to_string()), ("timeUnit".to_string(), "SECONDS".to_string())].into_iter().collect(),
        );
        let c = ScheduledPollConfig::from_params(&mut p, Duration::ZERO, Duration::ZERO).unwrap();
        assert_eq!(c.delay, Duration::from_secs(2));
    }
}

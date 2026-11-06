//! `timer:` component - generates message exchanges when a timer fires.
//!
//! `timer:name?period=1000&delay=1000&repeatCount=0&fixedRate=false&includeMetadata=false`
//!
//! Each fired exchange has a null body and the exchange property `CamelTimerCounter`
//! (1-based). With `includeMetadata=true` the properties `CamelTimerName`,
//! `CamelTimerPeriod`, `CamelTimerFiredTime` and the headers `CamelTimerFiredTime`,
//! `CamelTimerCounter` and `CamelMessageTimestamp` are set as well.
//!
//! Like Camel's `TimerComponent`, which keeps one `java.util.Timer` per timer name, all
//! consumers of the same timer name share one thread: their tasks run one after the other on
//! it, so a slow task delays the others of that name. The thread starts with the first
//! consumer of the name and exits when the last one stops.

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{JoinHandle, ThreadId};
use std::time::{Duration, Instant};

use crate::context::{CamelContext, WeakContext};
use crate::error::{CamelError, Result};
use crate::message::{props, ExchangePattern};
use crate::spi::{Component, Consumer, Endpoint, Processor, Service, SuspendableService};
use crate::util::Parameters;
use crate::value::Value;

use super::support::{new_exchange, parse_duration_with_unit, process_consumer_exchange, SuspendState};

/// The timer component.
#[derive(Default)]
pub struct TimerComponent {
    /// The timers by name (Camel's `TimerComponent.timers`).
    timers: Mutex<HashMap<String, Arc<SharedTimer>>>,
}

impl TimerComponent {
    /// Number of timer threads currently running (one per timer name in use, plus threads
    /// still finishing an exchange after an asynchronous delay released them).
    pub fn thread_count(&self) -> usize {
        self.timers.lock().unwrap().values().map(|t| t.state.lock().unwrap().threads).sum()
    }

    fn timer(&self, name: &str) -> Arc<SharedTimer> {
        self.timers.lock().unwrap().entry(name.to_string()).or_insert_with(|| Arc::new(SharedTimer::new(name))).clone()
    }
}

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
            timer: self.timer(remaining),
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
    timer: Arc<SharedTimer>,
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
        Ok(Arc::new(TimerConsumer {
            config: self.config.clone(),
            ctx: self.ctx.clone(),
            processor,
            timer: self.timer.clone(),
            task: Mutex::new(None),
            suspend: SuspendState::new(),
        }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// What a scheduled task does when it fires.
struct TimerJob {
    config: TimerConfig,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
    suspend: Arc<SuspendState>,
    logger: String,
}

/// A task scheduled on a timer (one per started consumer).
struct Task {
    id: u64,
    job: Arc<TimerJob>,
    due: Instant,
    /// The fixed-rate schedule (`scheduleAtFixedRate` keeps the original cadence).
    scheduled: Instant,
    counter: u64,
}

impl Task {
    /// Schedule the next execution after one that started at `started` (as
    /// `java.util.Timer`, before the task runs: fixed delay counts from the start of the
    /// previous execution).
    fn reschedule(&mut self, started: Instant) {
        let period = self.job.config.period;
        if self.job.config.fixed_rate {
            self.scheduled = later(self.scheduled, period);
            self.due = self.scheduled;
        } else {
            self.due = later(started, period);
        }
    }
}

/// `base + d`, or a time decades away when that is not representable (a huge `delay` or
/// `period` means "practically never").
fn later(base: Instant, d: Duration) -> Instant {
    base.checked_add(d).unwrap_or_else(|| base + Duration::from_secs(30 * 365 * 86_400))
}

#[derive(Default)]
struct TimerState {
    tasks: Vec<Task>,
    next_id: u64,
    /// Generation of the thread that runs the queue (`None`: no thread).
    scheduler: Option<u64>,
    next_gen: u64,
    handle: Option<JoinHandle<()>>,
    /// Tasks whose exchange is being processed, and the thread processing it.
    running: Vec<(u64, ThreadId)>,
    /// Timer threads alive (the scheduler and released threads finishing an exchange).
    threads: usize,
}

/// One timer name: a task queue run by one thread (Camel's `java.util.Timer`).
struct SharedTimer {
    thread_name: String,
    state: Mutex<TimerState>,
    cond: Condvar,
}

/// How often a suspended one-shot task checks whether it was resumed.
const SUSPENDED_POLL: Duration = Duration::from_millis(20);

impl SharedTimer {
    fn new(name: &str) -> SharedTimer {
        SharedTimer { thread_name: format!("Camel timer://{}", name), state: Mutex::new(TimerState::default()), cond: Condvar::new() }
    }

    /// Start a scheduler thread (the caller holds the state lock).
    fn start_thread(self: &Arc<Self>, st: &mut TimerState) -> Result<()> {
        let gen = st.next_gen;
        let me = self.clone();
        let h = std::thread::Builder::new()
            .name(self.thread_name.clone())
            .spawn(move || me.run(gen))
            .map_err(|e| CamelError::illegal_state(format!("Cannot spawn thread {}: {}", self.thread_name, e)))?;
        st.next_gen += 1;
        st.scheduler = Some(gen);
        st.handle = Some(h);
        st.threads += 1;
        Ok(())
    }

    /// Schedule a task; returns its id.
    fn schedule(self: &Arc<Self>, job: Arc<TimerJob>) -> Result<u64> {
        let mut initial = job.config.delay;
        if let Some(t) = job.config.time {
            initial = Duration::from_millis((t - crate::util_time::now_millis()).max(0) as u64);
        }
        let due = later(Instant::now(), initial);
        let mut st = self.state.lock().unwrap();
        if st.scheduler.is_none() {
            self.start_thread(&mut st)?;
        }
        let id = st.next_id;
        st.next_id += 1;
        st.tasks.push(Task { id, job, due, scheduled: due, counter: 0 });
        self.cond.notify_all();
        Ok(id)
    }

    /// Cancel a task and wait for an exchange it is processing (unless that is the calling
    /// thread). When it was the last task, the timer thread exits and is joined.
    fn cancel(&self, id: u64) {
        let me = std::thread::current().id();
        let mut st = self.state.lock().unwrap();
        st.tasks.retain(|t| t.id != id);
        self.cond.notify_all();
        while st.running.iter().any(|&(t, th)| t == id && th != me) {
            st = self.cond.wait(st).unwrap();
        }
        let handle = if st.tasks.is_empty() && st.scheduler.is_some() {
            st.scheduler = None;
            st.handle.take()
        } else {
            None
        };
        drop(st);
        if let Some(h) = handle {
            if h.thread().id() != me {
                let _ = h.join();
            }
        }
    }

    /// Remove a task without waiting (the consumer was dropped without being stopped).
    fn forget(&self, id: u64) {
        self.state.lock().unwrap().tasks.retain(|t| t.id != id);
        self.cond.notify_all();
    }

    /// The scheduler loop of thread generation `gen`.
    fn run(self: Arc<Self>, gen: u64) {
        let me = std::thread::current().id();
        let mut st = self.state.lock().unwrap();
        loop {
            if st.scheduler != Some(gen) {
                break;
            }
            if st.tasks.is_empty() {
                // idle: the thread exits, the next schedule() starts a new one
                st.scheduler = None;
                break;
            }
            let now = Instant::now();
            let (i, due) = st.tasks.iter().enumerate().map(|(i, t)| (i, t.due)).min_by_key(|&(_, d)| d).expect("task");
            if due > now {
                st = self.cond.wait_timeout(st, due - now).unwrap().0;
                continue;
            }
            let task = &mut st.tasks[i];
            let job = task.job.clone();
            let config = &job.config;
            if job.suspend.get() {
                // suspended: the task keeps its schedule but does not fire (as Camel's
                // TimerConsumer); a one-shot task waits until it is resumed
                if config.period.is_zero() {
                    task.due = later(now, SUSPENDED_POLL);
                } else {
                    task.reschedule(now);
                }
                continue;
            }
            task.counter += 1;
            let (id, counter) = (task.id, task.counter);
            let last = (config.period.is_zero() && config.repeat_count == 0) || (config.repeat_count > 0 && counter >= config.repeat_count);
            if last {
                st.tasks.remove(i);
            } else {
                task.reschedule(now);
            }
            st.running.push((id, me));
            drop(st);
            let released = self.fire(&job, counter);
            st = self.state.lock().unwrap();
            if let Some(p) = st.running.iter().position(|&(t, th)| t == id && th == me) {
                st.running.remove(p);
            }
            self.cond.notify_all();
            if released {
                if st.scheduler.is_some() {
                    // a replacement thread runs the queue now
                    break;
                }
                // the replacement could not be started: carry on here
                st.scheduler = Some(gen);
            }
        }
        st.threads -= 1;
        self.cond.notify_all();
    }

    /// Process one exchange. Returns true when an asynchronous delay released this thread
    /// (a replacement thread took over the queue, see [`crate::async_delay`]).
    fn fire(self: &Arc<Self>, job: &TimerJob, counter: u64) -> bool {
        let config = &job.config;
        let mut ex = new_exchange(&job.ctx);
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
        let me = self.clone();
        let on_release = move || {
            let mut st = me.state.lock().unwrap();
            if let Err(e) = me.start_thread(&mut st) {
                st.scheduler = None;
                crate::camel_warn!("timer", "Cannot start a replacement timer thread for {}: {}", me.thread_name, e);
            }
        };
        let (_, released) = crate::async_delay::run_detachable(on_release, || process_consumer_exchange(&job.logger, job.processor.as_ref(), &mut ex));
        released
    }
}

struct TimerConsumer {
    config: TimerConfig,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
    timer: Arc<SharedTimer>,
    task: Mutex<Option<u64>>,
    suspend: Arc<SuspendState>,
}

impl Service for TimerConsumer {
    fn start(&self) -> Result<()> {
        let mut task = self.task.lock().unwrap();
        if let Some(old) = task.take() {
            self.timer.cancel(old);
        }
        let job = Arc::new(TimerJob {
            config: self.config.clone(),
            ctx: self.ctx.clone(),
            processor: self.processor.clone(),
            suspend: self.suspend.clone(),
            logger: format!("timer://{}", self.config.name),
        });
        *task = Some(self.timer.schedule(job)?);
        Ok(())
    }
    fn stop(&self) -> Result<()> {
        let task = self.task.lock().unwrap().take();
        if let Some(id) = task {
            self.timer.cancel(id);
        }
        self.suspend.set(false);
        Ok(())
    }
    fn as_suspendable(&self) -> Option<&dyn SuspendableService> {
        Some(&*self.suspend)
    }
}

impl Drop for TimerConsumer {
    fn drop(&mut self) {
        if let Some(id) = self.task.get_mut().ok().and_then(|t| t.take()) {
            self.timer.forget(id);
        }
    }
}

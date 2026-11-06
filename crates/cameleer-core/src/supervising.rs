//! Supervising route controller (Camel `SupervisingRouteController`).
//!
//! By default a route that fails to start makes `ctx.start()` fail. With
//! `ctx.enable_supervising_route_controller(config)` the context starts anyway and the
//! failing routes are restarted in the background with a back-off:
//!
//! * the first restart happens `initial_delay + back_off_delay` after the failure,
//! * restart `n` waits `back_off_delay * back_off_multiplier^(n-1)` (capped by
//!   `back_off_max_delay` when non-zero),
//! * after `back_off_max_attempts` restarts (0 = unlimited) the route is *exhausted*
//!   and no longer retried.
//!
//! The restarts run on one background thread, which runs only while a route waits for a
//! restart: it starts at the first failure and exits when nothing is left to retry.
//!
//! [`CamelContext::route_controller_status`] reports, per route, whether it is started,
//! restarting or exhausted, the number of attempts and the last error.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::context::{CamelContext, CamelEvent, RouteStartupFailureHandler, ServiceStatus, WeakContext};
use crate::error::{CamelError, Result};
use crate::spi::EventNotifier;
use crate::value::Value;

/// Configuration of the supervising route controller.
#[derive(Clone, Debug)]
pub struct SupervisingRouteControllerConfig {
    /// Extra delay before the first restart attempt.
    pub initial_delay: Duration,
    /// Delay between restart attempts.
    pub back_off_delay: Duration,
    /// Maximum delay between attempts (zero = no maximum).
    pub back_off_max_delay: Duration,
    /// Multiplier applied to the delay after each attempt (1.0 = fixed delay).
    pub back_off_multiplier: f64,
    /// Maximum number of restart attempts (0 = unlimited).
    pub back_off_max_attempts: u32,
    /// Only supervise routes matching these comma separated patterns (None = all).
    pub include_routes: Option<String>,
    /// Do not supervise routes matching these comma separated patterns.
    pub exclude_routes: Option<String>,
}

impl Default for SupervisingRouteControllerConfig {
    fn default() -> Self {
        SupervisingRouteControllerConfig {
            initial_delay: Duration::ZERO,
            back_off_delay: Duration::from_secs(2),
            back_off_max_delay: Duration::ZERO,
            back_off_multiplier: 1.0,
            back_off_max_attempts: 0,
            include_routes: None,
            exclude_routes: None,
        }
    }
}

impl SupervisingRouteControllerConfig {
    pub fn initial_delay(mut self, d: Duration) -> Self {
        self.initial_delay = d;
        self
    }
    pub fn back_off_delay(mut self, d: Duration) -> Self {
        self.back_off_delay = d;
        self
    }
    pub fn back_off_max_delay(mut self, d: Duration) -> Self {
        self.back_off_max_delay = d;
        self
    }
    pub fn back_off_multiplier(mut self, m: f64) -> Self {
        self.back_off_multiplier = m;
        self
    }
    pub fn back_off_max_attempts(mut self, n: u32) -> Self {
        self.back_off_max_attempts = n;
        self
    }
    pub fn include_routes(mut self, p: &str) -> Self {
        self.include_routes = Some(p.to_string());
        self
    }
    pub fn exclude_routes(mut self, p: &str) -> Self {
        self.exclude_routes = Some(p.to_string());
        self
    }

    fn supervises(&self, route_id: &str) -> bool {
        let m = |p: &Option<String>| p.as_deref().map(|p| p.split(',').map(str::trim).any(|p| crate::util::matches_pattern(route_id, p)));
        m(&self.include_routes).unwrap_or(true) && !m(&self.exclude_routes).unwrap_or(false)
    }

    /// Delay before restart attempt `n` (1-based).
    pub fn delay_for(&self, n: u32) -> Duration {
        let factor = self.back_off_multiplier.max(1.0).powi(n.saturating_sub(1) as i32);
        let mut d = Duration::from_secs_f64(self.back_off_delay.as_secs_f64() * factor);
        if !self.back_off_max_delay.is_zero() && d > self.back_off_max_delay {
            d = self.back_off_max_delay;
        }
        if n == 1 {
            d += self.initial_delay;
        }
        d
    }
}

/// State of a supervised route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SupervisedState {
    Started,
    Restarting,
    Exhausted,
    Stopped,
}

impl SupervisedState {
    pub fn as_str(&self) -> &'static str {
        match self {
            SupervisedState::Started => "Started",
            SupervisedState::Restarting => "Restarting",
            SupervisedState::Exhausted => "Exhausted",
            SupervisedState::Stopped => "Stopped",
        }
    }
}

/// Status of one route in the route controller.
#[derive(Clone, Debug)]
pub struct RouteControllerEntry {
    pub route_id: String,
    pub state: SupervisedState,
    /// Start attempts so far (including the initial one) for failing routes.
    pub attempts: u32,
    pub last_error: Option<String>,
    /// Time until the next restart attempt.
    pub next_attempt_in: Option<Duration>,
}

/// Report of [`CamelContext::route_controller_status`].
#[derive(Clone, Debug)]
pub struct RouteControllerStatus {
    /// True if the supervising controller is enabled.
    pub supervising: bool,
    pub routes: Vec<RouteControllerEntry>,
}

impl RouteControllerStatus {
    pub fn route(&self, id: &str) -> Option<&RouteControllerEntry> {
        self.routes.iter().find(|r| r.route_id == id)
    }
    /// Routes currently failing (restarting or exhausted).
    pub fn failing(&self) -> Vec<&RouteControllerEntry> {
        self.routes.iter().filter(|r| matches!(r.state, SupervisedState::Restarting | SupervisedState::Exhausted)).collect()
    }
    pub fn to_value(&self) -> Value {
        let routes = self
            .routes
            .iter()
            .map(|r| {
                Value::object([
                    ("routeId", Value::from(r.route_id.as_str())),
                    ("state", Value::from(r.state.as_str())),
                    ("attempts", Value::from(r.attempts)),
                    ("lastError", r.last_error.as_deref().map(Value::from).unwrap_or(Value::Null)),
                    ("nextAttemptMillis", r.next_attempt_in.map(|d| Value::from(d.as_millis() as u64)).unwrap_or(Value::Null)),
                ])
            })
            .collect();
        Value::object([("controller", Value::from(if self.supervising { "SupervisingRouteController" } else { "DefaultRouteController" })), ("routes", Value::Array(routes))])
    }
}

struct Failing {
    attempts: u32,
    last_error: String,
    next_at: Instant,
    exhausted: bool,
}

#[derive(Default)]
struct Inner {
    failing: BTreeMap<String, Failing>,
    paused: bool,
    thread_running: bool,
}

/// The controller (one per context).
pub struct SupervisingRouteController {
    config: RwLock<SupervisingRouteControllerConfig>,
    ctx: WeakContext,
    inner: Mutex<Inner>,
    cond: Condvar,
    attempt_lock: Mutex<()>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl SupervisingRouteController {
    pub fn config(&self) -> SupervisingRouteControllerConfig {
        self.config.read().unwrap().clone()
    }

    fn ensure_thread(self: &Arc<Self>) {
        let mut inner = self.inner.lock().unwrap();
        if inner.thread_running {
            return;
        }
        inner.thread_running = true;
        drop(inner);
        let me = self.clone();
        let h = std::thread::Builder::new().name("camel-route-controller".into()).spawn(move || me.run()).ok();
        if h.is_none() {
            self.inner.lock().unwrap().thread_running = false;
        }
        *self.thread.lock().unwrap() = h;
    }

    fn run(self: Arc<Self>) {
        loop {
            let due = {
                let mut inner = self.inner.lock().unwrap();
                loop {
                    if inner.paused || self.ctx.upgrade().is_none() {
                        inner.thread_running = false;
                        return;
                    }
                    let now = Instant::now();
                    let next = inner.failing.iter().filter(|(_, f)| !f.exhausted).min_by_key(|(_, f)| f.next_at).map(|(id, f)| (id.clone(), f.next_at));
                    match next {
                        Some((id, at)) if at <= now => break id,
                        Some((_, at)) => {
                            inner = self.cond.wait_timeout(inner, at - now).unwrap().0;
                        }
                        None => {
                            // nothing left to retry: the thread exits, the next failure
                            // starts a new one
                            inner.thread_running = false;
                            return;
                        }
                    }
                }
            };
            self.attempt(&due);
        }
    }

    fn attempt(&self, route_id: &str) {
        let _g = self.attempt_lock.lock().unwrap();
        if self.inner.lock().unwrap().paused {
            return;
        }
        let Some(ctx) = self.ctx.upgrade() else { return };
        let remove = |me: &Self| {
            me.inner.lock().unwrap().failing.remove(route_id);
        };
        if ctx.status() != ServiceStatus::Started {
            return;
        }
        let Some(route) = ctx.route(route_id) else {
            remove(self);
            return;
        };
        if route.status() == ServiceStatus::Started {
            remove(self);
            return;
        }
        let result = ctx.start_route(route_id);
        let cfg = self.config();
        let mut inner = self.inner.lock().unwrap();
        match result {
            Ok(()) => {
                let attempts = inner.failing.remove(route_id).map(|f| f.attempts + 1).unwrap_or(1);
                crate::camel_info!("SupervisingRouteController", "Route {} started (attempt {})", route_id, attempts);
            }
            Err(e) => {
                if let Some(f) = inner.failing.get_mut(route_id) {
                    f.attempts += 1;
                    f.last_error = e.to_string();
                    let restarts = f.attempts - 1;
                    if cfg.back_off_max_attempts > 0 && restarts >= cfg.back_off_max_attempts {
                        f.exhausted = true;
                        crate::camel_warn!(
                            "SupervisingRouteController",
                            "Restarting route {} exhausted after {} attempts: {}",
                            route_id,
                            f.attempts,
                            f.last_error
                        );
                    } else {
                        f.next_at = Instant::now() + cfg.delay_for(restarts + 1);
                        crate::camel_info!("SupervisingRouteController", "Failed restarting route {} (attempt {}): {}", route_id, f.attempts, e);
                    }
                }
            }
        }
    }

    fn on_failure(self: &Arc<Self>, route_id: &str, error: &CamelError) -> Result<()> {
        let cfg = self.config();
        if !cfg.supervises(route_id) {
            return Err(error.clone());
        }
        crate::camel_warn!("SupervisingRouteController", "Failed to start route {}, will retry: {}", route_id, error);
        self.inner.lock().unwrap().failing.insert(
            route_id.to_string(),
            Failing { attempts: 1, last_error: error.to_string(), next_at: Instant::now() + cfg.delay_for(1), exhausted: false },
        );
        self.cond.notify_all();
        self.ensure_thread();
        Ok(())
    }

    fn pause(&self) {
        {
            let _g = self.attempt_lock.lock().unwrap();
            self.inner.lock().unwrap().paused = true;
        }
        self.cond.notify_all();
        let h = self.thread.lock().unwrap().take();
        if let Some(h) = h {
            if h.thread().id() != std::thread::current().id() {
                let _ = h.join();
            }
        }
    }

    fn resume(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.paused = false;
        inner.failing.clear();
    }
}

struct Handler(Arc<SupervisingRouteController>);

impl RouteStartupFailureHandler for Handler {
    fn on_route_startup_failure(&self, _ctx: &CamelContext, route_id: &str, error: &CamelError) -> Result<()> {
        self.0.on_failure(route_id, error)
    }
}

struct Lifecycle(Arc<SupervisingRouteController>);

impl EventNotifier for Lifecycle {
    fn notify(&self, event: &CamelEvent) {
        match event {
            CamelEvent::ContextStarting => self.0.resume(),
            CamelEvent::ContextStopping => self.0.pause(),
            CamelEvent::RouteRemoved(id) => {
                self.0.inner.lock().unwrap().failing.remove(id);
            }
            _ => {}
        }
    }
}

/// Routes currently failing to start under supervision: (route id, last error).
pub fn failing_routes(ctx: &CamelContext) -> Vec<(String, String)> {
    match crate::management::state(ctx).supervising.read().unwrap().as_ref() {
        Some(c) => c.inner.lock().unwrap().failing.iter().map(|(k, f)| (k.clone(), f.last_error.clone())).collect(),
        None => Vec::new(),
    }
}

/// Whether the supervising route controller's thread is running. It runs only while routes
/// wait for a restart: it starts when a route fails to start and exits when no route is left
/// to retry (restarted, exhausted or removed).
pub fn controller_thread_running(ctx: &CamelContext) -> bool {
    match crate::management::state(ctx).supervising.read().unwrap().as_ref() {
        Some(c) => c.inner.lock().unwrap().thread_running,
        None => false,
    }
}

impl CamelContext {
    /// Enable the supervising route controller (call before `start()`). Calling it again
    /// updates the configuration.
    pub fn enable_supervising_route_controller(&self, config: SupervisingRouteControllerConfig) {
        let st = crate::management::state(self);
        let mut slot = st.supervising.write().unwrap();
        if let Some(c) = slot.as_ref() {
            *c.config.write().unwrap() = config;
            return;
        }
        let c = Arc::new(SupervisingRouteController {
            config: RwLock::new(config),
            ctx: self.downgrade(),
            inner: Mutex::new(Inner::default()),
            cond: Condvar::new(),
            attempt_lock: Mutex::new(()),
            thread: Mutex::new(None),
        });
        *slot = Some(c.clone());
        drop(slot);
        self.set_route_startup_failure_handler(Some(Arc::new(Handler(c.clone()))));
        self.add_event_notifier(Lifecycle(c));
    }

    /// Whether the supervising route controller is enabled.
    pub fn is_supervising_route_controller(&self) -> bool {
        crate::management::state(self).supervising.read().unwrap().is_some()
    }

    /// Status of all routes as seen by the route controller.
    pub fn route_controller_status(&self) -> RouteControllerStatus {
        let ctrl = crate::management::state(self).supervising.read().unwrap().clone();
        let now = Instant::now();
        let routes = self
            .routes()
            .iter()
            .map(|r| {
                let failing = ctrl.as_ref().and_then(|c| {
                    c.inner.lock().unwrap().failing.get(&r.id).map(|f| (f.attempts, f.last_error.clone(), f.exhausted, f.next_at))
                });
                match failing {
                    Some((attempts, err, exhausted, next_at)) if r.status() != ServiceStatus::Started => RouteControllerEntry {
                        route_id: r.id.clone(),
                        state: if exhausted { SupervisedState::Exhausted } else { SupervisedState::Restarting },
                        attempts,
                        last_error: Some(err),
                        next_attempt_in: if exhausted { None } else { Some(next_at.saturating_duration_since(now)) },
                    },
                    _ => RouteControllerEntry {
                        route_id: r.id.clone(),
                        state: if r.status() == ServiceStatus::Started { SupervisedState::Started } else { SupervisedState::Stopped },
                        attempts: 0,
                        last_error: None,
                        next_attempt_in: None,
                    },
                }
            })
            .collect();
        RouteControllerStatus { supervising: ctrl.is_some(), routes }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_delays() {
        let c = SupervisingRouteControllerConfig::default()
            .initial_delay(Duration::from_millis(100))
            .back_off_delay(Duration::from_millis(10))
            .back_off_multiplier(2.0)
            .back_off_max_delay(Duration::from_millis(50));
        assert_eq!(c.delay_for(1), Duration::from_millis(110));
        assert_eq!(c.delay_for(2), Duration::from_millis(20));
        assert_eq!(c.delay_for(3), Duration::from_millis(40));
        assert_eq!(c.delay_for(4), Duration::from_millis(50));
        let c = c.include_routes("a*").exclude_routes("ab");
        assert!(c.supervises("ax"));
        assert!(!c.supervises("ab"));
        assert!(!c.supervises("b"));
    }
}

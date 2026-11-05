//! Route policies (Camel `RoutePolicy`): callbacks around every exchange of a route.

use std::collections::HashMap;
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use crate::message::Exchange;

/// A route policy, attached with `.route_policy(...)` on a route.
pub trait RoutePolicy: Send + Sync {
    /// Called once when the route is created.
    fn on_init(&self, _route_id: &str) {}
    /// Called when an exchange enters the route.
    fn on_exchange_begin(&self, _route_id: &str, _exchange: &mut Exchange) {}
    /// Called when an exchange is done with the route.
    fn on_exchange_done(&self, _route_id: &str, _exchange: &Exchange) {}
}

/// Scope of a [`ThrottlingInflightRoutePolicy`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThrottlingScope {
    /// Count inflight exchanges per route.
    Route,
    /// Count inflight exchanges over all routes using this policy.
    Context,
}

/// Throttles routes based on the number of inflight exchanges.
///
/// Camel suspends the route consumer when `max_inflight_exchanges` is exceeded and
/// resumes it once the inflight count drops below `resume_percent_of_max`. Cameleer's
/// consumers are synchronous, so this policy instead blocks new exchanges at the route
/// entrance until the inflight count drops below the resume threshold, which yields the
/// same upper bound on concurrent exchanges.
pub struct ThrottlingInflightRoutePolicy {
    pub max_inflight_exchanges: usize,
    pub resume_percent_of_max: usize,
    pub scope: ThrottlingScope,
    state: Mutex<HashMap<String, usize>>,
    cond: Condvar,
    throttled: Mutex<u64>,
}

impl Default for ThrottlingInflightRoutePolicy {
    fn default() -> Self {
        Self::new(1000)
    }
}

impl ThrottlingInflightRoutePolicy {
    pub fn new(max_inflight_exchanges: usize) -> Self {
        ThrottlingInflightRoutePolicy {
            max_inflight_exchanges: max_inflight_exchanges.max(1),
            resume_percent_of_max: 70,
            scope: ThrottlingScope::Route,
            state: Mutex::new(HashMap::new()),
            cond: Condvar::new(),
            throttled: Mutex::new(0),
        }
    }
    pub fn with_scope(mut self, scope: ThrottlingScope) -> Self {
        self.scope = scope;
        self
    }
    pub fn with_resume_percent_of_max(mut self, p: usize) -> Self {
        self.resume_percent_of_max = p.clamp(1, 100);
        self
    }
    fn key(&self, route_id: &str) -> String {
        match self.scope {
            ThrottlingScope::Route => route_id.to_string(),
            ThrottlingScope::Context => String::new(),
        }
    }
    /// Current inflight count tracked by this policy for a route (or the context).
    pub fn inflight(&self, route_id: &str) -> usize {
        self.state.lock().unwrap().get(&self.key(route_id)).copied().unwrap_or(0)
    }
    /// Number of exchanges that had to wait because the limit was reached.
    pub fn throttled_count(&self) -> u64 {
        *self.throttled.lock().unwrap()
    }
}

impl RoutePolicy for ThrottlingInflightRoutePolicy {
    fn on_exchange_begin(&self, route_id: &str, _exchange: &mut Exchange) {
        let key = self.key(route_id);
        let mut s = self.state.lock().unwrap();
        let current = s.get(&key).copied().unwrap_or(0);
        if current >= self.max_inflight_exchanges {
            *self.throttled.lock().unwrap() += 1;
            crate::camel_debug!("ThrottlingInflightRoutePolicy", "Throttling route {}: {} inflight exchanges", route_id, current);
            let resume = (self.max_inflight_exchanges * self.resume_percent_of_max / 100).clamp(1, self.max_inflight_exchanges);
            while s.get(&key).copied().unwrap_or(0) >= resume {
                s = self.cond.wait_timeout(s, Duration::from_millis(50)).unwrap().0;
            }
        }
        *s.entry(key).or_insert(0) += 1;
    }

    fn on_exchange_done(&self, route_id: &str, _exchange: &Exchange) {
        let key = self.key(route_id);
        let mut s = self.state.lock().unwrap();
        if let Some(c) = s.get_mut(&key) {
            *c = c.saturating_sub(1);
        }
        self.cond.notify_all();
    }
}

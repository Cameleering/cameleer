//! Health checks (Camel `camel-health`).
//!
//! A [`HealthCheck`] has an id, a group, readiness/liveness flags and a `call()`
//! returning a [`HealthCheckResult`] (`UP`, `DOWN` or `UNKNOWN`, a message and details).
//! Every context has a [`HealthCheckRegistry`] ([`CamelContext::health_check_registry`])
//! with the built-in checks:
//!
//! | id | readiness | liveness | UP when |
//! |---|---|---|---|
//! | `context` | yes | yes | the context is started |
//! | `routes` | yes | no | all auto-startup routes are started |
//! | `consumers` | yes | no | every started route has a running consumer |
//!
//! Components and applications add checks with [`CamelContext::register_health_check`]
//! or dynamic ones with [`HealthCheckRegistry::add_repository`].

use std::sync::{Arc, RwLock};

use crate::context::{CamelContext, ServiceStatus, WeakContext};
use crate::value::Value;

/// Health state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealthState {
    Up,
    Down,
    Unknown,
}

impl HealthState {
    pub fn as_str(&self) -> &'static str {
        match self {
            HealthState::Up => "UP",
            HealthState::Down => "DOWN",
            HealthState::Unknown => "UNKNOWN",
        }
    }
}

impl std::fmt::Display for HealthState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Result of a health check invocation.
#[derive(Clone, Debug)]
pub struct HealthCheckResult {
    /// Id of the check (filled in by the registry when empty).
    pub id: String,
    pub state: HealthState,
    pub message: Option<String>,
    /// Extra details (a JSON object).
    pub details: Value,
}

impl HealthCheckResult {
    pub fn new(state: HealthState) -> Self {
        HealthCheckResult { id: String::new(), state, message: None, details: Value::Object(Default::default()) }
    }
    pub fn up() -> Self {
        Self::new(HealthState::Up)
    }
    pub fn down(message: impl Into<String>) -> Self {
        Self::new(HealthState::Down).with_message(message)
    }
    pub fn unknown() -> Self {
        Self::new(HealthState::Unknown)
    }
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }
    pub fn with_detail(mut self, key: &str, value: impl Into<Value>) -> Self {
        if !matches!(self.details, Value::Object(_)) {
            self.details = Value::Object(Default::default());
        }
        if let Some(o) = self.details.as_object_mut() {
            o.insert(key.to_string(), value.into());
        }
        self
    }
    /// MicroProfile-health style JSON (`{"name", "status", "message", "data"}`).
    pub fn to_value(&self) -> Value {
        let mut m = std::collections::BTreeMap::new();
        m.insert("name".to_string(), Value::from(self.id.as_str()));
        m.insert("status".to_string(), Value::from(self.state.as_str()));
        if let Some(msg) = &self.message {
            m.insert("message".to_string(), Value::from(msg.as_str()));
        }
        if self.details.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
            m.insert("data".to_string(), self.details.clone());
        }
        Value::Object(m)
    }
}

/// A health check (Camel `HealthCheck`).
pub trait HealthCheck: Send + Sync {
    fn id(&self) -> &str;
    fn group(&self) -> &str {
        "camel"
    }
    fn is_readiness(&self) -> bool {
        true
    }
    fn is_liveness(&self) -> bool {
        true
    }
    fn call(&self) -> HealthCheckResult;
}

/// A health check from a closure.
pub struct FnHealthCheck<F> {
    pub id: String,
    pub group: String,
    pub readiness: bool,
    pub liveness: bool,
    pub f: F,
}

impl<F: Fn() -> HealthCheckResult + Send + Sync> FnHealthCheck<F> {
    /// A readiness+liveness check in group `camel`.
    pub fn new(id: impl Into<String>, f: F) -> Self {
        FnHealthCheck { id: id.into(), group: "camel".into(), readiness: true, liveness: true, f }
    }
    pub fn readiness_only(mut self) -> Self {
        self.liveness = false;
        self
    }
    pub fn liveness_only(mut self) -> Self {
        self.readiness = false;
        self
    }
    pub fn group(mut self, g: impl Into<String>) -> Self {
        self.group = g.into();
        self
    }
}

impl<F: Fn() -> HealthCheckResult + Send + Sync> HealthCheck for FnHealthCheck<F> {
    fn id(&self) -> &str {
        &self.id
    }
    fn group(&self) -> &str {
        &self.group
    }
    fn is_readiness(&self) -> bool {
        self.readiness
    }
    fn is_liveness(&self) -> bool {
        self.liveness
    }
    fn call(&self) -> HealthCheckResult {
        (self.f)()
    }
}

/// Provides health checks dynamically (Camel `HealthCheckRepository`).
pub type HealthCheckRepository = Arc<dyn Fn() -> Vec<Arc<dyn HealthCheck>> + Send + Sync>;

/// Aggregated result of several checks.
#[derive(Clone, Debug)]
pub struct HealthReport {
    pub state: HealthState,
    pub checks: Vec<HealthCheckResult>,
}

impl HealthReport {
    pub fn is_up(&self) -> bool {
        self.state == HealthState::Up
    }
    /// `{"status": "UP", "checks": [...]}`.
    pub fn to_value(&self) -> Value {
        Value::object([
            ("status", Value::from(self.state.as_str())),
            ("checks", Value::Array(self.checks.iter().map(|c| c.to_value()).collect())),
        ])
    }
    pub fn check(&self, id: &str) -> Option<&HealthCheckResult> {
        self.checks.iter().find(|c| c.id == id)
    }
}

/// The checks of a context.
pub struct HealthCheckRegistry {
    checks: RwLock<Vec<Arc<dyn HealthCheck>>>,
    repositories: RwLock<Vec<HealthCheckRepository>>,
    disabled: RwLock<Vec<String>>,
}

impl HealthCheckRegistry {
    pub fn new() -> Self {
        HealthCheckRegistry { checks: RwLock::new(Vec::new()), repositories: RwLock::new(Vec::new()), disabled: RwLock::new(Vec::new()) }
    }

    /// A registry with the built-in `context`, `routes` and `consumers` checks.
    pub fn with_builtins(ctx: &CamelContext) -> Self {
        let r = Self::new();
        let w = ctx.downgrade();
        r.register(Arc::new(ContextHealthCheck(w.clone())));
        r.register(Arc::new(RoutesHealthCheck(w.clone())));
        r.register(Arc::new(ConsumersHealthCheck(w)));
        r
    }

    /// Register (or replace by id) a check.
    pub fn register(&self, check: Arc<dyn HealthCheck>) {
        let mut c = self.checks.write().unwrap();
        c.retain(|x| x.id() != check.id());
        c.push(check);
    }
    pub fn unregister(&self, id: &str) -> bool {
        let mut c = self.checks.write().unwrap();
        let n = c.len();
        c.retain(|x| x.id() != id);
        n != c.len()
    }
    pub fn add_repository(&self, repo: HealthCheckRepository) {
        self.repositories.write().unwrap().push(repo);
    }
    /// Enable or disable a check by id (disabled checks are not invoked).
    pub fn set_enabled(&self, id: &str, enabled: bool) {
        let mut d = self.disabled.write().unwrap();
        d.retain(|x| x != id);
        if !enabled {
            d.push(id.to_string());
        }
    }

    /// All enabled checks (registered + repositories).
    pub fn checks(&self) -> Vec<Arc<dyn HealthCheck>> {
        let disabled = self.disabled.read().unwrap().clone();
        let mut v = self.checks.read().unwrap().clone();
        for r in self.repositories.read().unwrap().iter() {
            v.extend(r());
        }
        v.retain(|c| !disabled.iter().any(|d| d == c.id()));
        v
    }

    pub fn ids(&self) -> Vec<String> {
        self.checks().iter().map(|c| c.id().to_string()).collect()
    }

    fn invoke(check: &Arc<dyn HealthCheck>) -> HealthCheckResult {
        let mut r = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check.call())) {
            Ok(r) => r,
            Err(_) => HealthCheckResult::down("health check panicked"),
        };
        if r.id.is_empty() {
            r.id = check.id().to_string();
        }
        r
    }

    /// Invoke one check by id.
    pub fn call(&self, id: &str) -> Option<HealthCheckResult> {
        self.checks().iter().find(|c| c.id() == id).map(Self::invoke)
    }

    fn aggregate(&self, filter: impl Fn(&dyn HealthCheck) -> bool) -> HealthReport {
        let checks: Vec<HealthCheckResult> = self.checks().iter().filter(|c| filter(c.as_ref())).map(Self::invoke).collect();
        // UNKNOWN does not make the aggregate DOWN (as in Camel)
        let state = if checks.iter().any(|c| c.state == HealthState::Down) { HealthState::Down } else { HealthState::Up };
        HealthReport { state, checks }
    }

    /// All checks.
    pub fn health(&self) -> HealthReport {
        self.aggregate(|_| true)
    }
    /// Readiness checks.
    pub fn readiness(&self) -> HealthReport {
        self.aggregate(|c| c.is_readiness())
    }
    /// Liveness checks.
    pub fn liveness(&self) -> HealthReport {
        self.aggregate(|c| c.is_liveness())
    }
}

impl Default for HealthCheckRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl CamelContext {
    /// The health check registry of this context.
    pub fn health_check_registry(&self) -> Arc<HealthCheckRegistry> {
        crate::management::state(self).health.clone()
    }
    /// Register a health check.
    pub fn register_health_check(&self, check: impl HealthCheck + 'static) {
        self.health_check_registry().register(Arc::new(check));
    }
    /// Aggregated readiness.
    pub fn health_readiness(&self) -> HealthReport {
        self.health_check_registry().readiness()
    }
    /// Aggregated liveness.
    pub fn health_liveness(&self) -> HealthReport {
        self.health_check_registry().liveness()
    }
    /// All checks.
    pub fn health(&self) -> HealthReport {
        self.health_check_registry().health()
    }
}

// ---------------------------------------------------------------------------
// Built-in checks
// ---------------------------------------------------------------------------

struct ContextHealthCheck(WeakContext);

impl HealthCheck for ContextHealthCheck {
    fn id(&self) -> &str {
        "context"
    }
    fn call(&self) -> HealthCheckResult {
        let Some(ctx) = self.0.upgrade() else { return HealthCheckResult::down("CamelContext is gone") };
        let st = ctx.status();
        let r = if st == ServiceStatus::Started { HealthCheckResult::up() } else { HealthCheckResult::down(format!("CamelContext is {}", st)) };
        r.with_detail("context.name", ctx.name()).with_detail("context.status", st.to_string())
    }
}

struct RoutesHealthCheck(WeakContext);

impl HealthCheck for RoutesHealthCheck {
    fn id(&self) -> &str {
        "routes"
    }
    fn is_liveness(&self) -> bool {
        false
    }
    fn call(&self) -> HealthCheckResult {
        let Some(ctx) = self.0.upgrade() else { return HealthCheckResult::down("CamelContext is gone") };
        let failing = crate::supervising::failing_routes(&ctx);
        let mut down = Vec::new();
        let mut details = std::collections::BTreeMap::new();
        for r in ctx.routes() {
            let st = r.status();
            details.insert(format!("route.{}", r.id), Value::from(st.to_string()));
            // a stopped or suspended auto-startup route is not consuming: not ready
            if r.auto_startup && st != ServiceStatus::Started {
                down.push(r.id.clone());
            }
        }
        for (id, err) in failing {
            details.insert(format!("route.{}.error", id), Value::from(err));
        }
        let mut res = if ctx.status() != ServiceStatus::Started {
            HealthCheckResult::down(format!("CamelContext is {}", ctx.status()))
        } else if down.is_empty() {
            HealthCheckResult::up()
        } else {
            HealthCheckResult::down(format!("Routes not started: {}", down.join(", ")))
        };
        res.details = Value::Object(details);
        res
    }
}

struct ConsumersHealthCheck(WeakContext);

impl HealthCheck for ConsumersHealthCheck {
    fn id(&self) -> &str {
        "consumers"
    }
    fn is_liveness(&self) -> bool {
        false
    }
    fn call(&self) -> HealthCheckResult {
        let Some(ctx) = self.0.upgrade() else { return HealthCheckResult::down("CamelContext is gone") };
        let mut down = Vec::new();
        let mut details = std::collections::BTreeMap::new();
        for r in ctx.routes() {
            let running = r.has_consumer();
            details.insert(format!("consumer.{}", r.id), Value::from(if running { "UP" } else { "DOWN" }));
            if (r.status() == ServiceStatus::Started && !running) || (r.auto_startup && r.status() != ServiceStatus::Started) {
                down.push(r.id.clone());
            }
        }
        let mut res = if down.is_empty() { HealthCheckResult::up() } else { HealthCheckResult::down(format!("Consumers not running: {}", down.join(", "))) };
        res.details = Value::Object(details);
        res
    }
}

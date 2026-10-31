//! Tracer and backlog tracer (Camel `Tracer` / `BacklogTracer`).
//!
//! When tracing is enabled (`ctx.set_tracing(true)`), every step of every route is
//! logged (logger `org.apache.camel.Tracing`, level INFO) as
//!
//! ```text
//! >>> (route1) to[mock:out] <<< Exchange[Id: ..., ExchangePattern: InOnly, Headers: {...}, BodyType: String, Body: hello]
//! ```
//!
//! and published to the registered [`TraceEventHandler`]s. The route input is traced
//! as `from[uri]`. The backlog tracer (`ctx.set_backlog_tracing(true)`) keeps the last
//! N trace events in memory so that they can be queried (dev console `/q/dev/trace`).
//! Tracing can be switched on and off at runtime; routes do not need to be re-created.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use crate::component::log::ExchangeFormatter;
use crate::context::CamelContext;
use crate::log::Level;
use crate::message::Exchange;
use crate::value::Value;

/// Logger name used by the tracer.
pub const TRACING_LOGGER: &str = "org.apache.camel.Tracing";

/// One traced step.
#[derive(Clone, Debug)]
pub struct TraceEvent {
    /// Sequence number (unique per tracer).
    pub uid: u64,
    /// Epoch millis.
    pub timestamp: i64,
    pub route_id: String,
    pub node_id: String,
    pub node_label: String,
    /// True for the route input (`from`).
    pub first: bool,
    pub exchange_id: String,
    pub headers: Value,
    pub body: String,
    pub body_type: String,
    /// The formatted trace line.
    pub message: String,
}

impl TraceEvent {
    pub fn to_value(&self) -> Value {
        Value::object([
            ("uid", Value::from(self.uid)),
            ("timestamp", Value::Int(self.timestamp)),
            ("routeId", Value::from(self.route_id.as_str())),
            ("nodeId", Value::from(self.node_id.as_str())),
            ("nodeLabel", Value::from(self.node_label.as_str())),
            ("first", Value::Bool(self.first)),
            ("exchangeId", Value::from(self.exchange_id.as_str())),
            ("headers", self.headers.clone()),
            ("bodyType", Value::from(self.body_type.as_str())),
            ("body", Value::from(self.body.as_str())),
        ])
    }
}

/// Receives trace events (Camel `TraceEventHandler`). Closures implement it.
pub trait TraceEventHandler: Send + Sync {
    fn trace_event(&self, event: &TraceEvent);
}

impl<F> TraceEventHandler for F
where
    F: Fn(&TraceEvent) + Send + Sync,
{
    fn trace_event(&self, event: &TraceEvent) {
        self(event)
    }
}

/// The tracer of a context (see [`CamelContext::tracer`]).
pub struct Tracer {
    enabled: AtomicBool,
    log_enabled: AtomicBool,
    log_level: RwLock<Level>,
    pattern: RwLock<Option<String>>,
    handlers: RwLock<Vec<Arc<dyn TraceEventHandler>>>,
    backlog_enabled: AtomicBool,
    backlog_size: AtomicUsize,
    backlog: Mutex<VecDeque<TraceEvent>>,
    body_max_chars: AtomicUsize,
    counter: AtomicU64,
}

impl Default for Tracer {
    fn default() -> Self {
        Tracer {
            enabled: AtomicBool::new(false),
            log_enabled: AtomicBool::new(true),
            log_level: RwLock::new(Level::Info),
            pattern: RwLock::new(None),
            handlers: RwLock::new(Vec::new()),
            backlog_enabled: AtomicBool::new(false),
            backlog_size: AtomicUsize::new(1000),
            backlog: Mutex::new(VecDeque::new()),
            body_max_chars: AtomicUsize::new(1000),
            counter: AtomicU64::new(0),
        }
    }
}

impl Tracer {
    /// Standard tracing (logging + handlers).
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }
    /// Whether trace lines are logged (default true; handlers are always called).
    pub fn set_log_enabled(&self, enabled: bool) {
        self.log_enabled.store(enabled, Ordering::Relaxed);
    }
    pub fn set_log_level(&self, level: Level) {
        *self.log_level.write().unwrap() = level;
    }
    /// Only trace routes/nodes whose id matches one of the comma separated patterns
    /// (wildcards and regular expressions as in Camel's `tracePattern`).
    pub fn set_trace_pattern(&self, pattern: Option<&str>) {
        *self.pattern.write().unwrap() = pattern.map(|s| s.to_string());
    }
    /// Maximum number of body characters captured (default 1000).
    pub fn set_body_max_chars(&self, n: usize) {
        self.body_max_chars.store(n, Ordering::Relaxed);
    }
    pub fn add_handler(&self, h: impl TraceEventHandler + 'static) {
        self.handlers.write().unwrap().push(Arc::new(h));
    }
    pub fn clear_handlers(&self) {
        self.handlers.write().unwrap().clear();
    }

    /// Backlog tracing (keeps the last `backlog_size` events in memory).
    pub fn set_backlog_enabled(&self, enabled: bool) {
        self.backlog_enabled.store(enabled, Ordering::Relaxed);
    }
    pub fn is_backlog_enabled(&self) -> bool {
        self.backlog_enabled.load(Ordering::Relaxed)
    }
    pub fn set_backlog_size(&self, n: usize) {
        self.backlog_size.store(n.max(1), Ordering::Relaxed);
        let mut b = self.backlog.lock().unwrap();
        while b.len() > n.max(1) {
            b.pop_front();
        }
    }
    /// The traced events in the backlog (oldest first).
    pub fn backlog_events(&self) -> Vec<TraceEvent> {
        self.backlog.lock().unwrap().iter().cloned().collect()
    }
    /// Backlog events of one route or node id.
    pub fn backlog_events_for(&self, id: &str) -> Vec<TraceEvent> {
        self.backlog.lock().unwrap().iter().filter(|e| e.route_id == id || e.node_id == id).cloned().collect()
    }
    /// Remove and return all backlog events (Camel `dumpAllTracedMessages`).
    pub fn dump_backlog(&self) -> Vec<TraceEvent> {
        self.backlog.lock().unwrap().drain(..).collect()
    }
    pub fn clear_backlog(&self) {
        self.backlog.lock().unwrap().clear();
    }
    /// The backlog as a JSON array.
    pub fn backlog_value(&self) -> Value {
        Value::Array(self.backlog_events().iter().map(|e| e.to_value()).collect())
    }

    /// Whether any kind of tracing is active.
    pub fn is_active(&self) -> bool {
        self.is_enabled() || self.is_backlog_enabled()
    }

    fn matches(&self, route_id: &str, node_id: &str) -> bool {
        match &*self.pattern.read().unwrap() {
            None => true,
            Some(p) => p.split(',').map(str::trim).any(|p| crate::util::matches_pattern(route_id, p) || crate::util::matches_pattern(node_id, p)),
        }
    }

    /// Trace one step (called by the management instrumentation).
    pub fn trace(&self, route_id: &str, node_id: &str, label: &str, first: bool, ex: &Exchange) {
        if !self.matches(route_id, node_id) {
            return;
        }
        let max = self.body_max_chars.load(Ordering::Relaxed);
        let fmt = ExchangeFormatter { show_exchange_id: true, show_headers: true, max_chars: max, ..Default::default() };
        let message = format!(">>> ({}) {} <<< {}", route_id, label, fmt.format(ex));
        let mut body = if ex.body().is_null() { String::new() } else { ex.body().to_string() };
        if body.chars().count() > max {
            body = body.chars().take(max).collect();
        }
        let event = TraceEvent {
            uid: self.counter.fetch_add(1, Ordering::Relaxed) + 1,
            timestamp: crate::management::now_millis(),
            route_id: route_id.to_string(),
            node_id: node_id.to_string(),
            node_label: label.to_string(),
            first,
            exchange_id: ex.exchange_id.clone(),
            headers: ex.message.headers.to_value(),
            body,
            body_type: ex.body().type_name().to_string(),
            message,
        };
        if self.is_enabled() {
            if self.log_enabled.load(Ordering::Relaxed) {
                crate::log::log(*self.log_level.read().unwrap(), TRACING_LOGGER, &event.message);
            }
            let handlers = self.handlers.read().unwrap().clone();
            for h in handlers {
                h.trace_event(&event);
            }
        }
        if self.is_backlog_enabled() {
            let size = self.backlog_size.load(Ordering::Relaxed);
            let mut b = self.backlog.lock().unwrap();
            while b.len() >= size {
                b.pop_front();
            }
            b.push_back(event);
        }
    }
}

impl CamelContext {
    /// The context's tracer.
    pub fn tracer(&self) -> Arc<Tracer> {
        crate::management::state(self).tracer.clone()
    }
    /// Enable/disable tracing (`camel.main.tracing`). Takes effect immediately.
    pub fn set_tracing(&self, enabled: bool) {
        self.tracer().set_enabled(enabled);
    }
    pub fn is_tracing(&self) -> bool {
        self.tracer().is_enabled()
    }
    /// Enable/disable the backlog tracer (`camel.main.backlog-tracing`).
    pub fn set_backlog_tracing(&self, enabled: bool) {
        self.tracer().set_backlog_enabled(enabled);
    }
    pub fn is_backlog_tracing(&self) -> bool {
        self.tracer().is_backlog_enabled()
    }
}

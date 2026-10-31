//! Management and statistics (Camel's JMX management / `ManagedCamelContext`).
//!
//! Every context gets a [`RouteFactoryHook`] (installed by
//! [`crate::extensions::register_builtin_extensions`]) that wraps each route and each
//! processor ("node") of a route with an instrumentation processor. The instrumentation
//! collects performance counters (exchanges total/failed, min/max/mean/last processing
//! time, timestamps) per route and per node, and drives the [`crate::tracer`] and the
//! [`crate::history`] (message history) features.
//!
//! The collected data is exposed through [`ManagedCamelContext`]:
//!
//! ```
//! use camel_core::prelude::*;
//! let ctx = CamelContext::new();
//! ctx.add_routes(|b| { b.from("direct:a").route_id("a").to("mock:out"); }).unwrap();
//! ctx.start().unwrap();
//! ctx.create_producer_template().send_body("direct:a", "x").unwrap();
//! let mc = ctx.managed_context();
//! assert_eq!(mc.route("a").unwrap().stats.exchanges_total, 1);
//! assert!(mc.prometheus().contains("camel_exchanges_total{camelContext="));
//! ctx.stop().unwrap();
//! ```
//!
//! Node ids are the definition's `id()` (`.id("x")` in the DSL) or generated in
//! definition order with [`RouteContext::next_node_id`] (`to1`, `log2`, ...).

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::context::{CamelContext, CamelEvent, RouteFactoryHook, ServiceStatus};
use crate::error::{CamelError, Result};
use crate::health::HealthCheckRegistry;
use crate::message::Exchange;
use crate::model::{ChannelWrapper, ProcessorDefinition, RouteContext, RouteDefinition};
use crate::spi::{EventNotifier, Processor};
use crate::tracer::Tracer;
use crate::value::{ObjectMap, Value};

/// Registry name of the per-context [`ManagementState`].
pub const MANAGEMENT_STATE: &str = "camel.management.state";
const ROUTE_ATTR: &str = "camel.management.route";

pub(crate) fn now_millis() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Performance counters
// ---------------------------------------------------------------------------

/// Lock-free performance counters (route or processor level).
pub struct PerfCounters {
    total: AtomicU64,
    failed: AtomicU64,
    total_us: AtomicU64,
    min_us: AtomicU64,
    max_us: AtomicU64,
    last_us: AtomicU64,
    first_ts: AtomicI64,
    last_ts: AtomicI64,
    last_failure_ts: AtomicI64,
    reset_ts: AtomicI64,
}

impl Default for PerfCounters {
    fn default() -> Self {
        PerfCounters {
            total: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            total_us: AtomicU64::new(0),
            min_us: AtomicU64::new(u64::MAX),
            max_us: AtomicU64::new(0),
            last_us: AtomicU64::new(0),
            first_ts: AtomicI64::new(0),
            last_ts: AtomicI64::new(0),
            last_failure_ts: AtomicI64::new(0),
            reset_ts: AtomicI64::new(now_millis()),
        }
    }
}

impl PerfCounters {
    /// Record one processed exchange.
    pub fn record(&self, elapsed: Duration, failed: bool) {
        let us = elapsed.as_micros() as u64;
        let now = now_millis();
        self.total.fetch_add(1, Ordering::Relaxed);
        if failed {
            self.failed.fetch_add(1, Ordering::Relaxed);
            self.last_failure_ts.store(now, Ordering::Relaxed);
        }
        self.total_us.fetch_add(us, Ordering::Relaxed);
        self.min_us.fetch_min(us, Ordering::Relaxed);
        self.max_us.fetch_max(us, Ordering::Relaxed);
        self.last_us.store(us, Ordering::Relaxed);
        let _ = self.first_ts.compare_exchange(0, now, Ordering::Relaxed, Ordering::Relaxed);
        self.last_ts.store(now, Ordering::Relaxed);
    }

    /// Reset all counters to zero.
    pub fn reset(&self) {
        self.total.store(0, Ordering::Relaxed);
        self.failed.store(0, Ordering::Relaxed);
        self.total_us.store(0, Ordering::Relaxed);
        self.min_us.store(u64::MAX, Ordering::Relaxed);
        self.max_us.store(0, Ordering::Relaxed);
        self.last_us.store(0, Ordering::Relaxed);
        self.first_ts.store(0, Ordering::Relaxed);
        self.last_ts.store(0, Ordering::Relaxed);
        self.last_failure_ts.store(0, Ordering::Relaxed);
        self.reset_ts.store(now_millis(), Ordering::Relaxed);
    }

    /// A consistent-enough copy of the counters.
    pub fn snapshot(&self) -> PerfSnapshot {
        let total = self.total.load(Ordering::Relaxed);
        let failed = self.failed.load(Ordering::Relaxed).min(total);
        let total_us = self.total_us.load(Ordering::Relaxed);
        let min = self.min_us.load(Ordering::Relaxed);
        let opt = |v: i64| if v == 0 { None } else { Some(v) };
        PerfSnapshot {
            exchanges_total: total,
            exchanges_failed: failed,
            exchanges_completed: total - failed,
            total_processing_micros: total_us,
            total_processing_time: total_us / 1000,
            mean_processing_time: if total > 0 { total_us / total / 1000 } else { 0 },
            min_processing_time: if min == u64::MAX { 0 } else { min / 1000 },
            max_processing_time: self.max_us.load(Ordering::Relaxed) / 1000,
            last_processing_time: self.last_us.load(Ordering::Relaxed) / 1000,
            max_processing_micros: self.max_us.load(Ordering::Relaxed),
            first_exchange_timestamp: opt(self.first_ts.load(Ordering::Relaxed)),
            last_exchange_timestamp: opt(self.last_ts.load(Ordering::Relaxed)),
            last_exchange_failure_timestamp: opt(self.last_failure_ts.load(Ordering::Relaxed)),
            reset_timestamp: self.reset_ts.load(Ordering::Relaxed),
        }
    }
}

/// Snapshot of [`PerfCounters`]; processing times are in milliseconds, timestamps in epoch millis.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PerfSnapshot {
    pub exchanges_total: u64,
    pub exchanges_failed: u64,
    pub exchanges_completed: u64,
    pub total_processing_time: u64,
    pub total_processing_micros: u64,
    pub mean_processing_time: u64,
    pub min_processing_time: u64,
    pub max_processing_time: u64,
    pub max_processing_micros: u64,
    pub last_processing_time: u64,
    pub first_exchange_timestamp: Option<i64>,
    pub last_exchange_timestamp: Option<i64>,
    pub last_exchange_failure_timestamp: Option<i64>,
    pub reset_timestamp: i64,
}

impl PerfSnapshot {
    /// JSON-friendly representation (Camel attribute names).
    pub fn to_value(&self) -> Value {
        let ts = |v: Option<i64>| v.map(Value::Int).unwrap_or(Value::Null);
        Value::object([
            ("exchangesTotal", Value::from(self.exchanges_total)),
            ("exchangesFailed", Value::from(self.exchanges_failed)),
            ("exchangesCompleted", Value::from(self.exchanges_completed)),
            ("totalProcessingTime", Value::from(self.total_processing_time)),
            ("meanProcessingTime", Value::from(self.mean_processing_time)),
            ("minProcessingTime", Value::from(self.min_processing_time)),
            ("maxProcessingTime", Value::from(self.max_processing_time)),
            ("lastProcessingTime", Value::from(self.last_processing_time)),
            ("firstExchangeCompletedTimestamp", ts(self.first_exchange_timestamp)),
            ("lastExchangeCompletedTimestamp", ts(self.last_exchange_timestamp)),
            ("lastExchangeFailureTimestamp", ts(self.last_exchange_failure_timestamp)),
            ("resetTimestamp", Value::Int(self.reset_timestamp)),
        ])
    }
}

/// Counters of one processor (node) of a route.
pub struct NodeCounters {
    pub route_id: String,
    pub node_id: String,
    /// EIP name (`to`, `filter`, ...).
    pub name: String,
    /// Definition label (`to[mock:out]`).
    pub label: String,
    /// Nesting level (0 = top-level output of the route).
    pub level: usize,
    /// Position in the route definition (pre-order).
    pub index: usize,
    pub perf: PerfCounters,
}

/// Management data of one route.
pub struct RouteMgmt {
    pub route_id: String,
    pub perf: PerfCounters,
    nodes: Mutex<Vec<Arc<NodeCounters>>>,
    /// Pre-order outline of the definition: (level, node id, label).
    outline: Mutex<Vec<(usize, String, String)>>,
    input: String,
}

impl RouteMgmt {
    pub fn nodes(&self) -> Vec<Arc<NodeCounters>> {
        let mut v = self.nodes.lock().unwrap().clone();
        v.sort_by_key(|n| n.index);
        v
    }
}

/// Context-level exchange counters (root exchanges).
#[derive(Default)]
pub struct ContextCounters {
    completed: AtomicU64,
    failed: AtomicU64,
}

struct CountingNotifier(Arc<ContextCounters>);

impl EventNotifier for CountingNotifier {
    fn notify(&self, event: &CamelEvent) {
        match event {
            CamelEvent::ExchangeCompleted { .. } => {
                self.0.completed.fetch_add(1, Ordering::Relaxed);
            }
            CamelEvent::ExchangeFailed { .. } => {
                self.0.failed.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Per-context state
// ---------------------------------------------------------------------------

/// Per-context management state (bound in the registry as [`MANAGEMENT_STATE`]).
pub struct ManagementState {
    routes: RwLock<HashMap<String, Arc<RouteMgmt>>>,
    counters: Arc<ContextCounters>,
    statistics_enabled: AtomicBool,
    /// The tracer (standard + backlog).
    pub tracer: Arc<Tracer>,
    pub(crate) history_enabled: AtomicBool,
    /// Health checks.
    pub health: Arc<HealthCheckRegistry>,
    pub(crate) templates: RwLock<BTreeMap<String, Arc<crate::template_routes::RouteTemplate>>>,
    pub(crate) supervising: RwLock<Option<Arc<crate::supervising::SupervisingRouteController>>>,
}

impl ManagementState {
    fn new(ctx: &CamelContext) -> Self {
        ManagementState {
            routes: RwLock::new(HashMap::new()),
            counters: Arc::new(ContextCounters::default()),
            statistics_enabled: AtomicBool::new(true),
            tracer: Arc::new(Tracer::default()),
            history_enabled: AtomicBool::new(false),
            health: Arc::new(HealthCheckRegistry::with_builtins(ctx)),
            templates: RwLock::new(BTreeMap::new()),
            supervising: RwLock::new(None),
        }
    }

    /// Management data of a route.
    pub fn route(&self, id: &str) -> Option<Arc<RouteMgmt>> {
        self.routes.read().unwrap().get(id).cloned()
    }
}

static STATE_LOCK: Mutex<()> = Mutex::new(());

/// The management state of a context (created on first use).
pub fn state(ctx: &CamelContext) -> Arc<ManagementState> {
    if let Some(s) = ctx.registry().lookup::<ManagementState>(MANAGEMENT_STATE) {
        return s;
    }
    let _g = STATE_LOCK.lock().unwrap();
    if let Some(s) = ctx.registry().lookup::<ManagementState>(MANAGEMENT_STATE) {
        return s;
    }
    let s = Arc::new(ManagementState::new(ctx));
    ctx.registry().bind_arc(MANAGEMENT_STATE, s.clone());
    ctx.add_event_notifier(CountingNotifier(s.counters.clone()));
    s
}

/// Install the management hook on a new context (called for every context).
pub fn register(ctx: &CamelContext) {
    let st = state(ctx);
    ctx.add_route_factory_hook(Arc::new(ManagementHook { state: st }));
}

// ---------------------------------------------------------------------------
// Route factory hook and instrumentation
// ---------------------------------------------------------------------------

struct ManagementHook {
    state: Arc<ManagementState>,
}

/// Pre-computed ids of the definitions of a route, keyed by definition address.
struct NodeIndex(HashMap<usize, (String, usize, usize)>);

fn def_addr(d: &dyn ProcessorDefinition) -> usize {
    d as *const dyn ProcessorDefinition as *const () as usize
}

fn walk(defs: &[&dyn ProcessorDefinition], level: usize, rc: &RouteContext, out: &mut Vec<(usize, String, String, usize)>) {
    for d in defs {
        let id = d.id().map(|s| s.to_string()).unwrap_or_else(|| rc.next_node_id(d.name()));
        out.push((level, id, d.label(), def_addr(*d)));
        walk(&d.outputs(), level + 1, rc, out);
    }
}

impl RouteFactoryHook for ManagementHook {
    fn before_create(&self, def: &RouteDefinition, rc: &mut RouteContext) -> Result<()> {
        let mut out = Vec::new();
        let top: Vec<&dyn ProcessorDefinition> = def.outputs.iter().map(|b| b.as_ref()).collect();
        walk(&top, 0, rc, &mut out);
        let mut index = HashMap::new();
        let mut outline = Vec::new();
        for (i, (level, id, label, addr)) in out.into_iter().enumerate() {
            index.insert(addr, (id.clone(), level, i));
            outline.push((level, id, label));
        }
        let mgmt = Arc::new(RouteMgmt {
            route_id: rc.route_id.clone(),
            perf: PerfCounters::default(),
            nodes: Mutex::new(Vec::new()),
            outline: Mutex::new(outline),
            input: def.input.clone(),
        });
        self.state.routes.write().unwrap().insert(rc.route_id.clone(), mgmt.clone());
        rc.set_attribute(ROUTE_ATTR, mgmt.clone());
        rc.set_attribute("camel.management.nodeIndex", NodeIndex(index));
        rc.channel_wrappers.push(Arc::new(InstrumentationWrapper { state: self.state.clone(), mgmt }));
        Ok(())
    }

    fn wrap_route(&self, def: &RouteDefinition, rc: &RouteContext, processor: Arc<dyn Processor>) -> Result<Arc<dyn Processor>> {
        let Some(mgmt) = rc.attribute::<Arc<RouteMgmt>>(ROUTE_ATTR).map(|m| (*m).clone()) else { return Ok(processor) };
        let uri = rc.context.get_endpoint(&def.input).map(|e| e.uri().to_string()).unwrap_or_else(|_| def.input.clone());
        Ok(Arc::new(RouteInstrumentation { state: self.state.clone(), mgmt, label: format!("from[{}]", uri), inner: processor }))
    }
}

struct InstrumentationWrapper {
    state: Arc<ManagementState>,
    mgmt: Arc<RouteMgmt>,
}

impl ChannelWrapper for InstrumentationWrapper {
    fn wrap(&self, rc: &RouteContext, def: &dyn ProcessorDefinition, processor: Arc<dyn Processor>) -> Result<Arc<dyn Processor>> {
        let known = rc.attribute::<NodeIndex>("camel.management.nodeIndex").and_then(|i| i.0.get(&def_addr(def)).cloned());
        let (node_id, level, index) = match known {
            Some(k) => k,
            None => (def.id().map(|s| s.to_string()).unwrap_or_else(|| rc.next_node_id(def.name())), 0, usize::MAX / 2),
        };
        let node = Arc::new(NodeCounters {
            route_id: rc.route_id.clone(),
            node_id,
            name: def.name().to_string(),
            label: def.label(),
            level,
            index,
            perf: PerfCounters::default(),
        });
        {
            let mut nodes = self.mgmt.nodes.lock().unwrap();
            // the same definition may be wrapped more than once (e.g. re-created); keep the latest
            nodes.retain(|n| n.node_id != node.node_id);
            nodes.push(node.clone());
        }
        Ok(Arc::new(InstrumentedProcessor { state: self.state.clone(), node, inner: processor }))
    }
}

struct InstrumentedProcessor {
    state: Arc<ManagementState>,
    node: Arc<NodeCounters>,
    inner: Arc<dyn Processor>,
}

impl Processor for InstrumentedProcessor {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let n = &self.node;
        if self.state.tracer.is_active() {
            self.state.tracer.trace(&n.route_id, &n.node_id, &n.label, false, ex);
        }
        let hist = if self.state.history_enabled.load(Ordering::Relaxed) {
            Some(crate::history::begin(ex, &n.route_id, &n.node_id, &n.label))
        } else {
            None
        };
        let stats = self.state.statistics_enabled.load(Ordering::Relaxed);
        let start = Instant::now();
        let r = self.inner.process(ex);
        let elapsed = start.elapsed();
        if stats {
            n.perf.record(elapsed, r.is_err() || ex.is_failed());
        }
        if let Some(i) = hist {
            crate::history::end(ex, i, &n.node_id, elapsed);
        }
        r
    }
}

struct RouteInstrumentation {
    state: Arc<ManagementState>,
    mgmt: Arc<RouteMgmt>,
    label: String,
    inner: Arc<dyn Processor>,
}

impl Processor for RouteInstrumentation {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let id = &self.mgmt.route_id;
        if self.state.tracer.is_active() {
            self.state.tracer.trace(id, id, &self.label, true, ex);
        }
        let hist = if self.state.history_enabled.load(Ordering::Relaxed) {
            Some(crate::history::begin(ex, id, id, &self.label))
        } else {
            None
        };
        let start = Instant::now();
        let r = self.inner.process(ex);
        let elapsed = start.elapsed();
        if self.state.statistics_enabled.load(Ordering::Relaxed) {
            self.mgmt.perf.record(elapsed, r.is_err() || ex.is_failed());
        }
        if let Some(i) = hist {
            crate::history::end(ex, i, id, elapsed);
        }
        r
    }
}

// ---------------------------------------------------------------------------
// Managed views
// ---------------------------------------------------------------------------

/// Snapshot of a processor (node) of a route.
#[derive(Clone, Debug)]
pub struct ManagedProcessor {
    pub route_id: String,
    pub node_id: String,
    pub name: String,
    pub label: String,
    pub level: usize,
    pub index: usize,
    pub stats: PerfSnapshot,
}

impl ManagedProcessor {
    pub fn to_value(&self) -> Value {
        let mut v = Value::object([
            ("routeId", Value::from(self.route_id.as_str())),
            ("processorId", Value::from(self.node_id.as_str())),
            ("processorName", Value::from(self.name.as_str())),
            ("label", Value::from(self.label.as_str())),
            ("level", Value::from(self.level)),
            ("index", Value::from(self.index)),
        ]);
        if let (Some(o), Value::Object(s)) = (v.as_object_mut(), self.stats.to_value()) {
            o.insert("statistics".into(), Value::Object(s));
        }
        v
    }
}

/// Snapshot of a route (Camel `ManagedRouteMBean`).
#[derive(Clone, Debug)]
pub struct ManagedRoute {
    pub route_id: String,
    pub endpoint_uri: String,
    pub status: ServiceStatus,
    pub uptime_millis: u64,
    pub description: Option<String>,
    pub group: Option<String>,
    pub auto_startup: bool,
    pub startup_order: i32,
    pub exchanges_inflight: usize,
    pub stats: PerfSnapshot,
    pub processors: Vec<ManagedProcessor>,
    /// Text dump of the route with node ids.
    pub dump: String,
}

impl ManagedRoute {
    pub fn uptime(&self) -> String {
        format_uptime(self.uptime_millis)
    }

    pub fn to_value(&self) -> Value {
        let opt = |s: &Option<String>| s.as_deref().map(Value::from).unwrap_or(Value::Null);
        let mut m = ObjectMap::new();
        m.insert("routeId".into(), Value::from(self.route_id.as_str()));
        m.insert("from".into(), Value::from(self.endpoint_uri.as_str()));
        m.insert("state".into(), Value::from(self.status.to_string()));
        m.insert("uptime".into(), Value::from(self.uptime()));
        m.insert("uptimeMillis".into(), Value::from(self.uptime_millis));
        m.insert("description".into(), opt(&self.description));
        m.insert("group".into(), opt(&self.group));
        m.insert("autoStartup".into(), Value::Bool(self.auto_startup));
        m.insert("startupOrder".into(), Value::from(self.startup_order));
        m.insert("exchangesInflight".into(), Value::from(self.exchanges_inflight));
        m.insert("statistics".into(), self.stats.to_value());
        m.insert("processors".into(), Value::Array(self.processors.iter().map(|p| p.to_value()).collect()));
        m.insert("dump".into(), Value::from(self.dump.as_str()));
        Value::Object(m)
    }
}

/// An endpoint known to the context.
#[derive(Clone, Debug)]
pub struct ManagedEndpoint {
    pub uri: String,
    pub scheme: String,
    /// Route ids consuming from this endpoint.
    pub consumers: Vec<String>,
}

impl ManagedEndpoint {
    pub fn to_value(&self) -> Value {
        Value::object([
            ("uri", Value::from(self.uri.as_str())),
            ("scheme", Value::from(self.scheme.as_str())),
            ("consumers", Value::from(self.consumers.clone())),
        ])
    }
}

/// Human readable uptime (`1h2m3s`, `512ms`), like Camel's `TimeUtils.printDuration`.
pub fn format_uptime(ms: u64) -> String {
    if ms < 1000 {
        return format!("{}ms", ms);
    }
    let secs = ms / 1000;
    let (d, h, m, s) = (secs / 86400, (secs % 86400) / 3600, (secs % 3600) / 60, secs % 60);
    let mut out = String::new();
    if d > 0 {
        let _ = write!(out, "{}d", d);
    }
    if h > 0 {
        let _ = write!(out, "{}h", h);
    }
    if m > 0 {
        let _ = write!(out, "{}m", m);
    }
    if s > 0 || out.is_empty() {
        let _ = write!(out, "{}s", s);
    }
    out
}

/// Management view of a context (Camel `ManagedCamelContextMBean`): attributes,
/// statistics and operations. Obtain it with [`CamelContext::managed_context`].
#[derive(Clone)]
pub struct ManagedCamelContext {
    ctx: CamelContext,
    state: Arc<ManagementState>,
}

impl CamelContext {
    /// The management view of this context.
    pub fn managed_context(&self) -> ManagedCamelContext {
        ManagedCamelContext { ctx: self.clone(), state: state(self) }
    }

    /// Enable/disable collection of route and processor statistics (default enabled).
    pub fn set_statistics_enabled(&self, enabled: bool) {
        state(self).statistics_enabled.store(enabled, Ordering::Relaxed);
    }
}

impl ManagedCamelContext {
    pub fn context(&self) -> &CamelContext {
        &self.ctx
    }
    pub fn name(&self) -> String {
        self.ctx.name().to_string()
    }
    pub fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }
    pub fn status(&self) -> ServiceStatus {
        self.ctx.status()
    }
    pub fn uptime_millis(&self) -> u64 {
        self.ctx.uptime().map(|d| d.as_millis() as u64).unwrap_or(0)
    }
    pub fn uptime(&self) -> String {
        format_uptime(self.uptime_millis())
    }
    /// Root exchanges completed (successfully or not).
    pub fn exchanges_total(&self) -> u64 {
        self.exchanges_completed() + self.exchanges_failed()
    }
    pub fn exchanges_completed(&self) -> u64 {
        self.state.counters.completed.load(Ordering::Relaxed)
    }
    pub fn exchanges_failed(&self) -> u64 {
        self.state.counters.failed.load(Ordering::Relaxed)
    }
    pub fn exchanges_inflight(&self) -> usize {
        self.ctx.inflight_count()
    }

    fn managed_route(&self, r: &crate::context::Route) -> ManagedRoute {
        let mgmt = self.state.route(&r.id);
        let (stats, processors, dump) = match &mgmt {
            Some(m) => {
                let procs: Vec<ManagedProcessor> = m
                    .nodes()
                    .iter()
                    .map(|n| ManagedProcessor {
                        route_id: n.route_id.clone(),
                        node_id: n.node_id.clone(),
                        name: n.name.clone(),
                        label: n.label.clone(),
                        level: n.level,
                        index: n.index,
                        stats: n.perf.snapshot(),
                    })
                    .collect();
                let mut dump = format!("route[{}] from[{}]", r.id, m.input);
                for (level, id, label) in m.outline.lock().unwrap().iter() {
                    let _ = write!(dump, "\n{}{}: {}", "  ".repeat(level + 1), id, label);
                }
                (m.perf.snapshot(), procs, dump)
            }
            None => (PerfSnapshot::default(), Vec::new(), r.definition_dump.clone()),
        };
        ManagedRoute {
            route_id: r.id.clone(),
            endpoint_uri: r.endpoint_uri.clone(),
            status: r.status(),
            uptime_millis: r.uptime().map(|d| d.as_millis() as u64).unwrap_or(0),
            description: r.description.clone(),
            group: r.group.clone(),
            auto_startup: r.auto_startup,
            startup_order: r.startup_order,
            exchanges_inflight: self.ctx.inner.inflight.route_size(&r.id),
            stats,
            processors,
            dump,
        }
    }

    /// All routes, in the order they were added.
    pub fn routes(&self) -> Vec<ManagedRoute> {
        self.ctx.routes().iter().map(|r| self.managed_route(r)).collect()
    }

    pub fn route(&self, id: &str) -> Option<ManagedRoute> {
        self.ctx.route(id).map(|r| self.managed_route(&r))
    }

    /// Processor statistics of a route.
    pub fn processors(&self, route_id: &str) -> Vec<ManagedProcessor> {
        self.route(route_id).map(|r| r.processors).unwrap_or_default()
    }

    /// Statistics of one processor by node id (searching all routes).
    pub fn processor(&self, node_id: &str) -> Option<ManagedProcessor> {
        self.routes().into_iter().flat_map(|r| r.processors).find(|p| p.node_id == node_id)
    }

    /// Endpoints in the endpoint registry.
    pub fn endpoints(&self) -> Vec<ManagedEndpoint> {
        let routes = self.ctx.routes();
        self.ctx
            .endpoint_uris()
            .into_iter()
            .map(|uri| {
                let scheme = uri.split(':').next().unwrap_or("").to_string();
                let consumers = routes.iter().filter(|r| r.endpoint_uri == uri).map(|r| r.id.clone()).collect();
                ManagedEndpoint { uri, scheme, consumers }
            })
            .collect()
    }

    /// Registered component names.
    pub fn components(&self) -> Vec<String> {
        self.ctx.component_names()
    }

    // --- operations ---

    pub fn start_route(&self, id: &str) -> Result<()> {
        self.ctx.start_route(id)
    }
    pub fn stop_route(&self, id: &str) -> Result<()> {
        self.ctx.stop_route(id)
    }
    pub fn suspend_route(&self, id: &str) -> Result<()> {
        self.ctx.suspend_route(id)
    }
    pub fn resume_route(&self, id: &str) -> Result<()> {
        self.ctx.resume_route(id)
    }
    /// Remove a stopped route.
    pub fn remove_route(&self, id: &str) -> Result<bool> {
        let removed = self.ctx.remove_route(id)?;
        if removed {
            self.state.routes.write().unwrap().remove(id);
        }
        Ok(removed)
    }

    /// Reset the statistics of one route (and its processors).
    pub fn reset_route_statistics(&self, id: &str) -> Result<()> {
        let m = self.state.route(id).ok_or_else(|| CamelError::illegal_argument(format!("No route with id {}", id)))?;
        m.perf.reset();
        for n in m.nodes() {
            n.perf.reset();
        }
        if let Some(r) = self.ctx.route(id) {
            r.stats.exchanges_total.store(0, Ordering::Relaxed);
            r.stats.exchanges_failed.store(0, Ordering::Relaxed);
            r.stats.total_processing_ms.store(0, Ordering::Relaxed);
            r.stats.last_processing_ms.store(0, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Reset all statistics (context, routes, processors).
    pub fn reset_statistics(&self) {
        self.state.counters.completed.store(0, Ordering::Relaxed);
        self.state.counters.failed.store(0, Ordering::Relaxed);
        for r in self.ctx.routes() {
            let _ = self.reset_route_statistics(&r.id);
        }
    }

    // --- export ---

    /// Context attributes as JSON.
    pub fn to_value(&self) -> Value {
        let routes = self.ctx.routes();
        let started = routes.iter().filter(|r| r.status() == ServiceStatus::Started).count();
        Value::object([
            ("name", Value::from(self.name())),
            ("version", Value::from(self.version())),
            ("state", Value::from(self.status().to_string())),
            ("uptime", Value::from(self.uptime())),
            ("uptimeMillis", Value::from(self.uptime_millis())),
            ("exchangesTotal", Value::from(self.exchanges_total())),
            ("exchangesCompleted", Value::from(self.exchanges_completed())),
            ("exchangesFailed", Value::from(self.exchanges_failed())),
            ("exchangesInflight", Value::from(self.exchanges_inflight())),
            ("totalRoutes", Value::from(routes.len())),
            ("startedRoutes", Value::from(started)),
            ("tracing", Value::Bool(self.state.tracer.is_enabled())),
            ("backlogTracing", Value::Bool(self.state.tracer.is_backlog_enabled())),
            ("messageHistory", Value::Bool(self.state.history_enabled.load(Ordering::Relaxed))),
        ])
    }

    /// All routes as a JSON array.
    pub fn routes_value(&self) -> Value {
        Value::Array(self.routes().iter().map(|r| r.to_value()).collect())
    }

    /// Endpoints as a JSON array.
    pub fn endpoints_value(&self) -> Value {
        Value::Array(self.endpoints().iter().map(|e| e.to_value()).collect())
    }

    /// Everything: context, routes, endpoints and components.
    pub fn dump_value(&self) -> Value {
        Value::object([
            ("context", self.to_value()),
            ("routes", self.routes_value()),
            ("endpoints", self.endpoints_value()),
            ("components", Value::from(self.components())),
        ])
    }

    /// Prometheus text exposition (camel-micrometer style metric names).
    pub fn prometheus(&self) -> String {
        let ctx_label = escape_label(self.ctx.name());
        let routes = self.routes();
        let mut out = String::new();
        let mut family = |name: &str, kind: &str, help: &str, samples: Vec<(String, String)>| {
            let _ = writeln!(out, "# HELP {} {}", name, help);
            let _ = writeln!(out, "# TYPE {} {}", name, kind);
            for (labels, v) in samples {
                let _ = writeln!(out, "{}{{{}}} {}", name, labels, v);
            }
        };
        let rl = |r: &ManagedRoute| format!("camelContext=\"{}\",routeId=\"{}\"", ctx_label, escape_label(&r.route_id));
        let per_route = |f: &dyn Fn(&ManagedRoute) -> String| routes.iter().map(|r| (rl(r), f(r))).collect::<Vec<_>>();
        family("camel_exchanges_total", "counter", "Total number of processed exchanges", per_route(&|r| r.stats.exchanges_total.to_string()));
        family(
            "camel_exchanges_succeeded_total",
            "counter",
            "Number of successfully completed exchanges",
            per_route(&|r| r.stats.exchanges_completed.to_string()),
        );
        family("camel_exchanges_failed_total", "counter", "Number of failed exchanges", per_route(&|r| r.stats.exchanges_failed.to_string()));
        family("camel_exchanges_inflight", "gauge", "Number of inflight exchanges", per_route(&|r| r.exchanges_inflight.to_string()));
        // route policy summary (count / sum / max in seconds)
        let _ = writeln!(out, "# HELP camel_route_policy_seconds Route processing time");
        let _ = writeln!(out, "# TYPE camel_route_policy_seconds summary");
        for r in &routes {
            let l = rl(r);
            let _ = writeln!(out, "camel_route_policy_seconds_count{{{}}} {}", l, r.stats.exchanges_total);
            let _ = writeln!(out, "camel_route_policy_seconds_sum{{{}}} {}", l, secs(r.stats.total_processing_micros));
        }
        let _ = writeln!(out, "# HELP camel_route_policy_seconds_max Route maximum processing time");
        let _ = writeln!(out, "# TYPE camel_route_policy_seconds_max gauge");
        for r in &routes {
            let _ = writeln!(out, "camel_route_policy_seconds_max{{{}}} {}", rl(r), secs(r.stats.max_processing_micros));
        }
        let mut family = |name: &str, kind: &str, help: &str, samples: Vec<(String, String)>| {
            let _ = writeln!(out, "# HELP {} {}", name, help);
            let _ = writeln!(out, "# TYPE {} {}", name, kind);
            for (labels, v) in samples {
                let _ = writeln!(out, "{}{{{}}} {}", name, labels, v);
            }
        };
        let cl = format!("camelContext=\"{}\"", ctx_label);
        let running = routes.iter().filter(|r| r.status == ServiceStatus::Started).count();
        family("camel_routes_running_routes", "gauge", "Number of running routes", vec![(cl.clone(), running.to_string())]);
        family("camel_routes_added_routes", "gauge", "Number of added routes", vec![(cl.clone(), routes.len().to_string())]);
        family(
            "camel_context_exchanges_total",
            "counter",
            "Total number of exchanges completed in the context",
            vec![(cl.clone(), self.exchanges_total().to_string())],
        );
        family(
            "camel_context_exchanges_failed_total",
            "counter",
            "Number of failed exchanges in the context",
            vec![(cl.clone(), self.exchanges_failed().to_string())],
        );
        family("camel_context_uptime_seconds", "gauge", "Context uptime", vec![(cl.clone(), format!("{:.3}", self.uptime_millis() as f64 / 1000.0))]);
        family(
            "camel_context_status",
            "gauge",
            "Context status (1 = started)",
            vec![(cl, if self.status() == ServiceStatus::Started { "1" } else { "0" }.to_string())],
        );
        let procs: Vec<&ManagedProcessor> = routes.iter().flat_map(|r| r.processors.iter()).collect();
        let pl = |p: &ManagedProcessor| {
            format!("camelContext=\"{}\",routeId=\"{}\",nodeId=\"{}\"", ctx_label, escape_label(&p.route_id), escape_label(&p.node_id))
        };
        family(
            "camel_processor_exchanges_total",
            "counter",
            "Exchanges processed by a processor",
            procs.iter().map(|p| (pl(p), p.stats.exchanges_total.to_string())).collect(),
        );
        family(
            "camel_processor_exchanges_failed_total",
            "counter",
            "Exchanges failed in a processor",
            procs.iter().map(|p| (pl(p), p.stats.exchanges_failed.to_string())).collect(),
        );
        family(
            "camel_processor_seconds_sum",
            "counter",
            "Total processing time of a processor",
            procs.iter().map(|p| (pl(p), secs(p.stats.total_processing_micros))).collect(),
        );
        out
    }
}

fn secs(us: u64) -> String {
    format!("{:.6}", us as f64 / 1_000_000.0)
}

fn escape_label(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptime_format() {
        assert_eq!(format_uptime(12), "12ms");
        assert_eq!(format_uptime(1000), "1s");
        assert_eq!(format_uptime(3_723_000), "1h2m3s");
    }

    #[test]
    fn perf_counters() {
        let p = PerfCounters::default();
        p.record(Duration::from_millis(5), false);
        p.record(Duration::from_millis(15), true);
        let s = p.snapshot();
        assert_eq!((s.exchanges_total, s.exchanges_failed, s.exchanges_completed), (2, 1, 1));
        assert_eq!((s.min_processing_time, s.max_processing_time, s.last_processing_time, s.mean_processing_time), (5, 15, 15, 10));
        assert!(s.last_exchange_failure_timestamp.is_some());
        p.reset();
        assert_eq!(p.snapshot().exchanges_total, 0);
        assert_eq!(p.snapshot().min_processing_time, 0);
    }
}

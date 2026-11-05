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
//! use cameleer_core::prelude::*;
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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::context::{CamelContext, RouteFactoryHook, ServiceStatus};
use crate::error::{CamelError, Result};
use crate::health::HealthCheckRegistry;
use crate::message::Exchange;
use crate::model::{ChannelWrapper, ProcessorDefinition, RouteContext, RouteDefinition};
use crate::stripe::{bump, slot, Padded, CELLS};
use crate::spi::Processor;
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
///
/// Times are kept as raw [`crate::clock`] ticks and converted to micros and epoch millis
/// in [`PerfCounters::snapshot`], so recording an exchange is a few atomic operations.
/// The counters are striped ([`crate::stripe`]): each thread updates its own cache line,
/// and a snapshot combines the stripes.
pub struct PerfCounters {
    stripes: [Padded<PerfStripe>; CELLS],
    reset_ts: AtomicI64,
    /// Sums of `total`, `failed` and `total_ticks` at the last reset (see [`crate::stripe`]).
    base: [AtomicU64; 3],
}

struct PerfStripe {
    total: AtomicU64,
    failed: AtomicU64,
    total_ticks: AtomicU64,
    min_ticks: AtomicU64,
    max_ticks: AtomicU64,
    last_ticks: AtomicU64,
    /// Tick readings of the first, last and last failed exchange (0 = none).
    first_at: AtomicU64,
    last_at: AtomicU64,
    last_failure_at: AtomicU64,
}

impl Default for PerfStripe {
    fn default() -> Self {
        PerfStripe {
            total: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            total_ticks: AtomicU64::new(0),
            min_ticks: AtomicU64::new(u64::MAX),
            max_ticks: AtomicU64::new(0),
            last_ticks: AtomicU64::new(0),
            first_at: AtomicU64::new(0),
            last_at: AtomicU64::new(0),
            last_failure_at: AtomicU64::new(0),
        }
    }
}

impl Default for PerfCounters {
    fn default() -> Self {
        PerfCounters { stripes: Default::default(), reset_ts: AtomicI64::new(now_millis()), base: Default::default() }
    }
}

impl PerfCounters {
    /// Record one processed exchange.
    pub fn record(&self, elapsed: Duration, failed: bool) {
        let end = crate::clock::ticks();
        self.record_ticks(end.saturating_sub(crate::clock::duration_to_ticks(elapsed)), end, failed);
    }

    /// Record one exchange timed with [`crate::clock::ticks`] readings.
    #[inline]
    pub fn record_ticks(&self, start: u64, end: u64, failed: bool) {
        let slot = slot();
        let s = &self.stripes[slot.index];
        let t = end.saturating_sub(start);
        let at = end.max(1);
        bump(&s.total, 1, slot);
        if failed {
            bump(&s.failed, 1, slot);
            s.last_failure_at.store(at, Ordering::Relaxed);
        }
        bump(&s.total_ticks, t, slot);
        // read before the read-modify-writes: min and max rarely change
        if t < s.min_ticks.load(Ordering::Relaxed) {
            s.min_ticks.fetch_min(t, Ordering::Relaxed);
        }
        if t > s.max_ticks.load(Ordering::Relaxed) {
            s.max_ticks.fetch_max(t, Ordering::Relaxed);
        }
        s.last_ticks.store(t, Ordering::Relaxed);
        if s.first_at.load(Ordering::Relaxed) == 0 {
            let _ = s.first_at.compare_exchange(0, at, Ordering::Relaxed, Ordering::Relaxed);
        }
        s.last_at.store(at, Ordering::Relaxed);
    }

    /// Reset all counters to zero.
    pub fn reset(&self) {
        // counters: a baseline (a stripe's owner may be writing its cells right now)
        let (total, failed, ticks) = self.sums();
        self.base[0].store(total, Ordering::Relaxed);
        self.base[1].store(failed, Ordering::Relaxed);
        self.base[2].store(ticks, Ordering::Relaxed);
        for s in &self.stripes {
            s.min_ticks.store(u64::MAX, Ordering::Relaxed);
            s.max_ticks.store(0, Ordering::Relaxed);
            s.last_ticks.store(0, Ordering::Relaxed);
            s.first_at.store(0, Ordering::Relaxed);
            s.last_at.store(0, Ordering::Relaxed);
            s.last_failure_at.store(0, Ordering::Relaxed);
        }
        self.reset_ts.store(now_millis(), Ordering::Relaxed);
    }

    fn sums(&self) -> (u64, u64, u64) {
        let ld = |a: &AtomicU64| a.load(Ordering::Relaxed);
        self.stripes.iter().fold((0u64, 0u64, 0u64), |(a, b, c), s| {
            (a.wrapping_add(ld(&s.total)), b.wrapping_add(ld(&s.failed)), c.wrapping_add(ld(&s.total_ticks)))
        })
    }

    /// A consistent-enough copy of the counters.
    pub fn snapshot(&self) -> PerfSnapshot {
        let us = |t: u64| (crate::clock::ticks_to_nanos(t) + 500) / 1000;
        let ts = |t: u64| if t == 0 { None } else { Some(crate::clock::ticks_to_epoch_millis(t)) };
        let ld = |a: &AtomicU64| a.load(Ordering::Relaxed);
        let (total, failed, total_ticks) = self.sums();
        let total = total.wrapping_sub(ld(&self.base[0]));
        let failed = failed.wrapping_sub(ld(&self.base[1]));
        let total_ticks = total_ticks.wrapping_sub(ld(&self.base[2]));
        let (mut min, mut max) = (u64::MAX, 0u64);
        let (mut first_at, mut last_at, mut last_ticks, mut last_failure_at) = (0u64, 0u64, 0u64, 0u64);
        for s in &self.stripes {
            min = min.min(ld(&s.min_ticks));
            max = max.max(ld(&s.max_ticks));
            let f = ld(&s.first_at);
            if f != 0 && (first_at == 0 || f < first_at) {
                first_at = f;
            }
            // the last exchange is the one of the stripe that recorded last
            let l = ld(&s.last_at);
            if l > last_at {
                last_at = l;
                last_ticks = ld(&s.last_ticks);
            }
            last_failure_at = last_failure_at.max(ld(&s.last_failure_at));
        }
        let failed = failed.min(total);
        let total_us = us(total_ticks);
        let max_us = us(max);
        PerfSnapshot {
            exchanges_total: total,
            exchanges_failed: failed,
            exchanges_completed: total - failed,
            total_processing_micros: total_us,
            total_processing_time: total_us / 1000,
            mean_processing_time: if total > 0 { total_us / total / 1000 } else { 0 },
            min_processing_time: if min == u64::MAX { 0 } else { us(min) / 1000 },
            max_processing_time: max_us / 1000,
            last_processing_time: us(last_ticks) / 1000,
            max_processing_micros: max_us,
            first_exchange_timestamp: ts(first_at),
            last_exchange_timestamp: ts(last_at),
            last_exchange_failure_timestamp: ts(last_failure_at),
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

// ---------------------------------------------------------------------------
// Per-context state
// ---------------------------------------------------------------------------

/// How much performance data management collects (Camel's `ManagementStatisticsLevel`).
///
/// - `Extended` and `Default`: route and processor (per-step) statistics. Cameleer has no
///   extended (load) statistics yet, so `Extended` collects the same as `Default`.
/// - `RoutesOnly` (Cameleer's default): route statistics and the context counters only.
/// - `Off`: no route or processor statistics (the context's completed/failed counters, which
///   cost one striped add per exchange, are still kept).
///
/// Apache Camel collects statistics only when `camel-management` is loaded (then at
/// `Default`); without it there are none. Cameleer always has management, so its default
/// sits in between: the cheap route-level numbers on, per-step timing opt-in
/// (`set_statistics_level(Default)`, `camel.main.jmxManagementStatisticsLevel=Default`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagementStatisticsLevel {
    Extended,
    Default,
    RoutesOnly,
    Off,
}

impl ManagementStatisticsLevel {
    /// Parse Camel's names (case-insensitive, `routes-only` also accepted).
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().replace(['-', '_'], "").as_str() {
            "extended" => Ok(Self::Extended),
            "default" => Ok(Self::Default),
            "routesonly" => Ok(Self::RoutesOnly),
            "off" => Ok(Self::Off),
            _ => Err(CamelError::illegal_argument(format!("Unknown ManagementStatisticsLevel: {}", s))),
        }
    }
    fn to_u8(self) -> u8 {
        match self {
            Self::Extended => 3,
            Self::Default => 2,
            Self::RoutesOnly => 1,
            Self::Off => 0,
        }
    }
    fn from_u8(v: u8) -> Self {
        match v {
            3 => Self::Extended,
            2 => Self::Default,
            1 => Self::RoutesOnly,
            _ => Self::Off,
        }
    }
}

/// Per-context management state (bound in the registry as [`MANAGEMENT_STATE`]).
pub struct ManagementState {
    routes: RwLock<HashMap<String, Arc<RouteMgmt>>>,
    /// Context-level exchange counters (root exchanges), counted by the route processor.
    counters: Arc<crate::context::ExchangeCounters>,
    /// A [`ManagementStatisticsLevel`].
    statistics_level: std::sync::atomic::AtomicU8,
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
            counters: ctx.inner.exchange_counters.clone(),
            statistics_level: std::sync::atomic::AtomicU8::new(ManagementStatisticsLevel::RoutesOnly.to_u8()),
            tracer: Arc::new(Tracer::default()),
            history_enabled: AtomicBool::new(false),
            health: Arc::new(HealthCheckRegistry::with_builtins(ctx)),
            templates: RwLock::new(BTreeMap::new()),
            supervising: RwLock::new(None),
        }
    }

    #[inline]
    fn level(&self) -> u8 {
        self.statistics_level.load(Ordering::Relaxed)
    }
    #[inline]
    fn route_stats(&self) -> bool {
        self.level() >= ManagementStatisticsLevel::RoutesOnly.to_u8()
    }
    #[inline]
    fn processor_stats(&self) -> bool {
        self.level() >= ManagementStatisticsLevel::Default.to_u8()
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
        let timer: Arc<dyn crate::context::RouteTimer> = Arc::new(RouteTimer { state: self.state.clone(), mgmt: mgmt.clone() });
        rc.set_attribute(crate::context::ROUTE_TIMER_ATTR, timer);
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

/// Route-level statistics, fed by the route processor's timing of each exchange.
struct RouteTimer {
    state: Arc<ManagementState>,
    mgmt: Arc<RouteMgmt>,
}

impl crate::context::RouteTimer for RouteTimer {
    fn record(&self, start: u64, end: u64, failed: bool) {
        if self.state.route_stats() {
            self.mgmt.perf.record_ticks(start, end, failed);
        }
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
        self.process_timed(ex, 0).0
    }

    fn process_timed(&self, ex: &mut Exchange, start: u64) -> (Result<()>, u64) {
        let n = &self.node;
        let stats = self.state.processor_stats();
        let tracing = self.state.tracer.is_active();
        if !stats && !tracing && !self.state.history_enabled.load(Ordering::Relaxed) {
            // nothing to record for this step
            return self.inner.process_timed(ex, start);
        }
        if tracing {
            self.state.tracer.trace(&n.route_id, &n.node_id, &n.label, false, ex);
        }
        let hist = if self.state.history_enabled.load(Ordering::Relaxed) {
            Some(crate::history::begin(ex, &n.route_id, &n.node_id, &n.label))
        } else {
            None
        };
        let start = if start != 0 { start } else { crate::clock::ticks() };
        // a block (choice, filter...) passes out the last reading of its children
        let (r, inner_end) = self.inner.process_timed(ex, 0);
        let end = if inner_end != 0 { inner_end } else { crate::clock::ticks() };
        if stats {
            n.perf.record_ticks(start, end, r.is_err() || ex.is_failed());
        }
        if let Some(i) = hist {
            crate::history::end(ex, i, &n.node_id, crate::clock::elapsed(start, end));
        }
        (r, end)
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
        self.process_timed(ex, 0).0
    }

    fn process_timed(&self, ex: &mut Exchange, outer_start: u64) -> (Result<()>, u64) {
        let id = &self.mgmt.route_id;
        if self.state.tracer.is_active() {
            self.state.tracer.trace(id, id, &self.label, true, ex);
        }
        let hist = if self.state.history_enabled.load(Ordering::Relaxed) {
            Some(crate::history::begin(ex, id, id, &self.label))
        } else {
            None
        };
        // route statistics are recorded by the route processor (`RouteTimer`)
        let Some(i) = hist else { return self.inner.process_timed(ex, outer_start) };
        let start = crate::clock::ticks();
        let r = self.inner.process(ex);
        let end = crate::clock::ticks();
        crate::history::end(ex, i, id, crate::clock::elapsed(start, end));
        (r, end)
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

    /// Collect route and processor statistics (`ManagementStatisticsLevel::Default`) or none
    /// (`Off`). See [`CamelContext::set_statistics_level`].
    pub fn set_statistics_enabled(&self, enabled: bool) {
        self.set_statistics_level(if enabled { ManagementStatisticsLevel::Default } else { ManagementStatisticsLevel::Off });
    }

    /// How much performance data management collects (default `RoutesOnly`).
    pub fn set_statistics_level(&self, level: ManagementStatisticsLevel) {
        state(self).statistics_level.store(level.to_u8(), Ordering::Relaxed);
    }

    pub fn statistics_level(&self) -> ManagementStatisticsLevel {
        ManagementStatisticsLevel::from_u8(state(self).level())
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
        self.state.counters.completed.get()
    }
    pub fn exchanges_failed(&self) -> u64 {
        self.state.counters.failed.get()
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
            r.stats.reset();
        }
        Ok(())
    }

    /// Reset all statistics (context, routes, processors).
    pub fn reset_statistics(&self) {
        self.state.counters.completed.reset();
        self.state.counters.failed.reset();
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

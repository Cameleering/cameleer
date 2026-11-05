//! CamelContext: the runtime holding components, endpoints, routes, registry,
//! languages and property placeholders.

use std::any::Any;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::{Duration, Instant};

use crate::dsl::RouteBuilder;
use crate::error::{kinds, CamelError, Result};
use crate::message::Exchange;
use crate::model::{ChannelWrapper, RouteContext, RouteDefinition};
use crate::spi::{Component, Consumer, Endpoint, EventNotifier, Language, Processor, Producer, Service};
use crate::template::{ConsumerTemplate, ProducerTemplate};
use crate::util::{EndpointUri, Parameters};
use crate::value::Value;

/// Lifecycle status of the context or a route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceStatus {
    Initialized,
    Starting,
    Started,
    Suspended,
    Stopping,
    Stopped,
}

impl std::fmt::Display for ServiceStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

/// Events published to [`EventNotifier`]s.
#[derive(Clone, Debug)]
pub enum CamelEvent {
    ContextStarting,
    ContextStarted,
    ContextStopping,
    ContextStopped,
    RouteAdded(String),
    RouteStarted(String),
    RouteStopped(String),
    RouteRemoved(String),
    RouteSuspended(String),
    RouteResumed(String),
    ContextSuspended,
    ContextResumed,
    ExchangeCreated { exchange_id: String, route_id: Option<String> },
    ExchangeCompleted { exchange_id: String, route_id: Option<String>, elapsed_ms: u64 },
    ExchangeFailed { exchange_id: String, route_id: Option<String>, error: CamelError },
    ExchangeSending { exchange_id: String, endpoint: String },
    ExchangeSent { exchange_id: String, endpoint: String, elapsed_ms: u64 },
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Bean registry (Camel `Registry`): name -> any value.
#[derive(Default)]
pub struct Registry {
    beans: RwLock<HashMap<String, Arc<dyn Any + Send + Sync>>>,
}

impl Registry {
    /// Bind a bean. Use [`Registry::bind_processor`] for processors.
    pub fn bind<T: Any + Send + Sync>(&self, name: impl Into<String>, bean: T) {
        self.beans.write().unwrap().insert(name.into(), Arc::new(bean));
    }
    /// Bind an already shared bean.
    pub fn bind_arc(&self, name: impl Into<String>, bean: Arc<dyn Any + Send + Sync>) {
        self.beans.write().unwrap().insert(name.into(), bean);
    }
    pub fn lookup<T: Any + Send + Sync>(&self, name: &str) -> Option<Arc<T>> {
        self.beans.read().unwrap().get(name).cloned().and_then(|b| b.downcast::<T>().ok())
    }
    pub fn lookup_any(&self, name: &str) -> Option<Arc<dyn Any + Send + Sync>> {
        self.beans.read().unwrap().get(name).cloned()
    }
    pub fn contains(&self, name: &str) -> bool {
        self.beans.read().unwrap().contains_key(name)
    }
    /// All beans of type `T` (Camel `findByTypeWithName`), sorted by name.
    pub fn find_by_type<T: Any + Send + Sync>(&self) -> Vec<(String, Arc<T>)> {
        let mut v: Vec<(String, Arc<T>)> =
            self.beans.read().unwrap().iter().filter_map(|(k, b)| b.clone().downcast::<T>().ok().map(|t| (k.clone(), t))).collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }
    pub fn unbind(&self, name: &str) -> bool {
        self.beans.write().unwrap().remove(name).is_some()
    }
    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.beans.read().unwrap().keys().cloned().collect();
        v.sort();
        v
    }
    /// Bind a processor (looked up by `process(ref)`, `bean:` and `ref:` endpoints).
    pub fn bind_processor(&self, name: impl Into<String>, p: impl Processor + 'static) {
        let p: Arc<dyn Processor> = Arc::new(p);
        self.bind(name, p);
    }
    pub fn lookup_processor(&self, name: &str) -> Option<Arc<dyn Processor>> {
        self.lookup::<Arc<dyn Processor>>(name).map(|a| (*a).clone())
    }
    /// Bind an aggregation strategy (looked up by `aggregationStrategy` refs).
    pub fn bind_aggregation_strategy(&self, name: impl Into<String>, s: impl crate::spi::AggregationStrategy + 'static) {
        let s: Arc<dyn crate::spi::AggregationStrategy> = Arc::new(s);
        self.bind(name, s);
    }
    pub fn lookup_aggregation_strategy(&self, name: &str) -> Option<Arc<dyn crate::spi::AggregationStrategy>> {
        self.lookup::<Arc<dyn crate::spi::AggregationStrategy>>(name).map(|a| (*a).clone())
    }
}

// ---------------------------------------------------------------------------
// Properties (placeholders)
// ---------------------------------------------------------------------------

/// A properties function (Camel `PropertiesFunction`): resolves the remainder of a
/// `{{prefix:remainder}}` placeholder. `Ok(None)` means "not found" (the placeholder default,
/// if any, is then used); errors abort the resolution.
pub type PropertiesFunction = Arc<dyn Fn(&str) -> Result<Option<String>> + Send + Sync>;

/// Property placeholder support: `{{key}}`, `{{key:default}}`, `{{env:NAME}}`,
/// `{{sys:name}}` (environment), custom functions (`{{prefix:remainder[:default]}}`, see
/// [`PropertiesComponent::add_function`]), with nested resolution.
#[derive(Default)]
pub struct PropertiesComponent {
    props: RwLock<BTreeMap<String, String>>,
    /// Higher-priority override properties.
    overrides: RwLock<BTreeMap<String, String>>,
    functions: RwLock<BTreeMap<String, PropertiesFunction>>,
}

impl PropertiesComponent {
    pub fn set(&self, key: impl Into<String>, value: impl Into<String>) {
        self.props.write().unwrap().insert(key.into(), value.into());
    }
    pub fn set_override(&self, key: impl Into<String>, value: impl Into<String>) {
        self.overrides.write().unwrap().insert(key.into(), value.into());
    }
    pub fn set_all(&self, map: BTreeMap<String, String>) {
        self.props.write().unwrap().extend(map);
    }
    pub fn get(&self, key: &str) -> Option<String> {
        if let Some(v) = self.overrides.read().unwrap().get(key) {
            return Some(v.clone());
        }
        if let Some(v) = self.props.read().unwrap().get(key) {
            return Some(v.clone());
        }
        if let Some(name) = key.strip_prefix("env:").or_else(|| key.strip_prefix("env.")) {
            return std::env::var(name).ok().or_else(|| std::env::var(name.to_ascii_uppercase().replace(['.', '-'], "_")).ok());
        }
        if let Some(name) = key.strip_prefix("sys:").or_else(|| key.strip_prefix("sys.")) {
            return std::env::var(name).ok();
        }
        // Camel also resolves OS env variables using the uppercase/underscore convention
        std::env::var(format!("CAMEL_PROPERTY_{}", key.to_ascii_uppercase().replace(['.', '-'], "_"))).ok()
    }
    /// Register a properties function for `{{prefix:...}}` placeholders (replaces an existing one).
    pub fn add_function(&self, prefix: impl Into<String>, f: PropertiesFunction) {
        self.functions.write().unwrap().insert(prefix.into(), f);
    }
    /// Whether a function is registered for `prefix`.
    pub fn has_function(&self, prefix: &str) -> bool {
        self.functions.read().unwrap().contains_key(prefix)
    }
    fn function_for(&self, key: &str) -> Option<(PropertiesFunction, usize)> {
        let (prefix, _) = key.split_once(':')?;
        self.functions.read().unwrap().get(prefix).map(|f| (f.clone(), prefix.len() + 1))
    }
    /// Look a key up, calling a registered function for `prefix:` keys.
    pub fn lookup(&self, key: &str) -> Result<Option<String>> {
        if let Some((f, n)) = self.function_for(key) {
            return f(&key[n..]);
        }
        Ok(self.get(key))
    }
    pub fn all(&self) -> BTreeMap<String, String> {
        let mut m = self.props.read().unwrap().clone();
        m.extend(self.overrides.read().unwrap().clone());
        m
    }

    /// Resolve all `{{...}}` placeholders in `text`.
    pub fn resolve(&self, text: &str) -> Result<String> {
        self.resolve_depth(text, 0)
    }

    fn resolve_depth(&self, text: &str, depth: usize) -> Result<String> {
        if !text.contains("{{") {
            return Ok(text.to_string());
        }
        if depth > 20 {
            return Err(CamelError::illegal_argument(format!("Circular property placeholder reference in: {}", text)));
        }
        let mut out = String::new();
        let mut rest = text;
        while let Some(start) = rest.find("{{") {
            out.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            // find matching close, supporting nesting
            let mut level = 1;
            let bytes = after.as_bytes();
            let mut j = 0;
            let mut end = None;
            while j + 1 < bytes.len() + 1 && j < bytes.len() {
                if after[j..].starts_with("{{") {
                    level += 1;
                    j += 2;
                    continue;
                }
                if after[j..].starts_with("}}") {
                    level -= 1;
                    if level == 0 {
                        end = Some(j);
                        break;
                    }
                    j += 2;
                    continue;
                }
                j += 1;
            }
            let end = end.ok_or_else(|| CamelError::illegal_argument(format!("Unclosed property placeholder in: {}", text)))?;
            let inner = self.resolve_depth(&after[..end], depth + 1)?;
            let (key, default) = match self.function_for(&inner) {
                Some((_, n)) => split_function_key_default(&inner, n),
                None => split_key_default(&inner),
            };
            let v = match self.lookup(key)? {
                Some(v) => v,
                None => match default {
                    Some(d) => d.to_string(),
                    None => {
                        return Err(CamelError::illegal_argument(format!(
                            "Property with key [{}] not found in properties from text: {}",
                            key, text
                        )))
                    }
                },
            };
            out.push_str(&self.resolve_depth(&v, depth + 1)?);
            rest = &after[end + 2..];
        }
        out.push_str(rest);
        Ok(out)
    }
}

/// `prefix:name[:default]` where the function prefix is `prefix_len` bytes (with the colon).
fn split_function_key_default(s: &str, prefix_len: usize) -> (&str, Option<&str>) {
    match s[prefix_len..].find(':') {
        Some(i) => (&s[..prefix_len + i], Some(&s[prefix_len + i + 1..])),
        None => (s, None),
    }
}

fn split_key_default(s: &str) -> (&str, Option<&str>) {
    // `env:NAME:default` and `sys:name:default` have a prefix containing ':'
    let (prefix_len, body) = if let Some(b) = s.strip_prefix("env:") {
        (4, b)
    } else if let Some(b) = s.strip_prefix("sys:") {
        (4, b)
    } else {
        (0, s)
    };
    match body.find(':') {
        Some(i) => (&s[..prefix_len + i], Some(&body[i + 1..])),
        None => (s, None),
    }
}

// ---------------------------------------------------------------------------
// Inflight repository
// ---------------------------------------------------------------------------

/// Inflight exchange counts, in total and per route. Each route has its own atomic counter
/// (created once), so counting an exchange takes no lock and allocates nothing.
#[derive(Default)]
pub struct InflightRepository {
    per_route: RwLock<HashMap<String, Arc<crate::stripe::Gauge>>>,
}

impl InflightRepository {
    /// The counter of a route, created on first use.
    pub(crate) fn counter(&self, route_id: &str) -> Arc<crate::stripe::Gauge> {
        if let Some(c) = self.per_route.read().unwrap().get(route_id) {
            return c.clone();
        }
        self.per_route.write().unwrap().entry(route_id.to_string()).or_default().clone()
    }
    pub fn add(&self, route_id: &str) {
        Self::add_to(&self.counter(route_id));
    }
    pub fn remove(&self, route_id: &str) {
        Self::remove_from(&self.counter(route_id));
    }
    pub(crate) fn add_to(route: &crate::stripe::Gauge) {
        route.inc();
    }
    pub(crate) fn remove_from(route: &crate::stripe::Gauge) {
        // striped: the sum never reads below zero (see `Gauge::get`)
        route.dec();
    }
    /// The total over all routes (no shared counter: every route thread would contend on it).
    pub fn size(&self) -> usize {
        self.per_route.read().unwrap().values().map(|c| c.get()).sum()
    }
    pub fn route_size(&self, route_id: &str) -> usize {
        self.per_route.read().unwrap().get(route_id).map(|c| c.get()).unwrap_or(0)
    }
}

// ---------------------------------------------------------------------------
// Route
// ---------------------------------------------------------------------------

/// Statistics for a route.
#[derive(Default)]
pub struct RouteStats {
    exchanges_total: crate::stripe::Counter,
    exchanges_failed: crate::stripe::Counter,
    total_processing_ms: crate::stripe::Counter,
    last_processing_ms: AtomicU64,
}

impl RouteStats {
    pub fn exchanges_total(&self) -> u64 {
        self.exchanges_total.get()
    }
    pub fn exchanges_failed(&self) -> u64 {
        self.exchanges_failed.get()
    }
    pub fn total_processing_ms(&self) -> u64 {
        self.total_processing_ms.get()
    }
    pub fn last_processing_ms(&self) -> u64 {
        self.last_processing_ms.load(Ordering::Relaxed)
    }
    fn record(&self, elapsed_ms: u64, failed: bool) {
        self.exchanges_total.add(1);
        if elapsed_ms > 0 {
            self.total_processing_ms.add(elapsed_ms);
        }
        // a plain store: only written when it changes (the cache line stays shared)
        if self.last_processing_ms.load(Ordering::Relaxed) != elapsed_ms {
            self.last_processing_ms.store(elapsed_ms, Ordering::Relaxed);
        }
        if failed {
            self.exchanges_failed.add(1);
        }
    }
    pub fn reset(&self) {
        self.exchanges_total.reset();
        self.exchanges_failed.reset();
        self.total_processing_ms.reset();
        self.last_processing_ms.store(0, Ordering::Relaxed);
    }
    pub fn snapshot(&self) -> Value {
        let total = self.exchanges_total.get();
        let failed = self.exchanges_failed.get();
        let ms = self.total_processing_ms.get();
        Value::object([
            ("exchangesTotal", Value::from(total)),
            ("exchangesFailed", Value::from(failed)),
            ("exchangesCompleted", Value::from(total - failed.min(total))),
            ("totalProcessingTime", Value::from(ms)),
            ("meanProcessingTime", Value::from(if total > 0 { ms / total } else { 0 })),
            ("lastProcessingTime", Value::from(self.last_processing_ms.load(Ordering::Relaxed))),
        ])
    }
}

/// A runtime route.
pub struct Route {
    pub id: String,
    pub endpoint_uri: String,
    pub description: Option<String>,
    pub group: Option<String>,
    pub auto_startup: bool,
    pub startup_order: i32,
    pub definition_dump: String,
    pub definition: Mutex<Option<RouteDefinition>>,
    endpoint: Arc<dyn Endpoint>,
    processor: Arc<dyn Processor>,
    services: Vec<Arc<dyn Service>>,
    consumer: Mutex<Option<Arc<dyn Consumer>>>,
    status: RwLock<ServiceStatus>,
    pub stats: Arc<RouteStats>,
    pub uptime_start: Mutex<Option<Instant>>,
}

impl Route {
    pub fn status(&self) -> ServiceStatus {
        *self.status.read().unwrap()
    }
    pub fn endpoint(&self) -> &Arc<dyn Endpoint> {
        &self.endpoint
    }
    pub fn processor(&self) -> &Arc<dyn Processor> {
        &self.processor
    }
    pub fn uptime(&self) -> Option<Duration> {
        self.uptime_start.lock().unwrap().map(|s| s.elapsed())
    }
    /// Whether the route currently has a running consumer.
    pub fn has_consumer(&self) -> bool {
        self.consumer.lock().unwrap().is_some()
    }
}

/// Handles the failure of an auto-startup route to start (route controller SPI, see
/// `supervising::SupervisingRouteController`). Returning `Ok` lets the context keep
/// starting; returning `Err` fails the context startup (the default behaviour).
pub trait RouteStartupFailureHandler: Send + Sync {
    fn on_route_startup_failure(&self, ctx: &CamelContext, route_id: &str, error: &CamelError) -> Result<()>;
}

/// Context-level counters of completed and failed root exchanges (management's
/// `ExchangesCompleted` / `ExchangesFailed` of the context).
#[derive(Default)]
pub struct ExchangeCounters {
    pub completed: crate::stripe::Counter,
    pub failed: crate::stripe::Counter,
}

/// Records the route-level processing time of each exchange. Management registers one in
/// the route context ([`ROUTE_TIMER_ATTR`]); the route processor, which times every
/// exchange anyway, feeds it, so a route is timed once.
pub trait RouteTimer: Send + Sync {
    fn record(&self, start_ticks: u64, end_ticks: u64, failed: bool);
}

/// Route context attribute holding an `Arc<dyn RouteTimer>`.
pub const ROUTE_TIMER_ATTR: &str = "camel.route.timer";

/// Wraps a route's processor: sets exchange metadata, tracks inflight/statistics,
/// emits events and runs on-completion callbacks (the unit of work).
struct RouteProcessor {
    route_id: Arc<str>,
    endpoint_uri: Arc<str>,
    inner: Arc<dyn Processor>,
    ctx: WeakContext,
    stats: Arc<RouteStats>,
    inflight: Arc<crate::stripe::Gauge>,
    counters: Arc<ExchangeCounters>,
    notifiers: Arc<AtomicBool>,
    timer: Option<Arc<dyn RouteTimer>>,
}

impl Processor for RouteProcessor {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        // events are only built when someone listens (they clone ids)
        let notify = self.notifiers.load(Ordering::Acquire);
        // the context is only needed to attach it to the exchange or to notify
        let ctx = if exchange.context.is_none() || notify {
            match self.ctx.upgrade() {
                Some(c) => Some(c),
                None => return Err(CamelError::illegal_state("CamelContext has been dropped")),
            }
        } else {
            None
        };
        if exchange.context.is_none() {
            exchange.context = ctx.clone();
        }
        let is_root = exchange.from_route_id.is_none();
        if is_root {
            exchange.from_route_id = Some(self.route_id.clone());
            exchange.from_endpoint = Some(self.endpoint_uri.clone());
            if let (true, Some(ctx)) = (notify, &ctx) {
                ctx.notify(&CamelEvent::ExchangeCreated { exchange_id: exchange.exchange_id.to_string(), route_id: Some(self.route_id.to_string()) });
            }
        }
        InflightRepository::add_to(&self.inflight);
        let start = crate::clock::ticks();
        let (r, end) = self.inner.process_timed(exchange, start);
        if let Err(e) = r {
            exchange.set_exception(e);
        }
        // the end reading of the last instrumented step, if the route passed it out
        let end = if end != 0 { end } else { crate::clock::ticks() };
        if let Some(t) = &self.timer {
            t.record(start, end, exchange.is_failed());
        }
        let elapsed = crate::clock::ticks_to_millis(end.saturating_sub(start));
        InflightRepository::remove_from(&self.inflight);
        self.stats.record(elapsed, exchange.is_failed());
        if is_root {
            let c = &self.counters;
            if exchange.exception.is_some() { &c.failed } else { &c.completed }.add(1);
        }
        // root exchanges with listeners: completion events, then the unit of work ends
        let ctx = match (is_root && notify, ctx) {
            (true, Some(c)) => c,
            _ => {
                if is_root {
                    exchange.run_on_completions();
                }
                return Ok(());
            }
        };
        if let Some(e) = &exchange.exception {
            ctx.notify(&CamelEvent::ExchangeFailed { exchange_id: exchange.exchange_id.to_string(), route_id: Some(self.route_id.to_string()), error: e.clone() });
        } else {
            ctx.notify(&CamelEvent::ExchangeCompleted {
                exchange_id: exchange.exchange_id.to_string(),
                route_id: Some(self.route_id.to_string()),
                elapsed_ms: elapsed,
            });
        }
        exchange.run_on_completions();
        Ok(())
    }
}

/// Hook invoked when a route definition is turned into a runtime route; extensions
/// (error handling, interceptors, onCompletion, route policies) use it to install
/// channel wrappers and wrap the route processor.
pub trait RouteFactoryHook: Send + Sync {
    /// Called before processors are created; may add channel wrappers.
    fn before_create(&self, _def: &RouteDefinition, _rc: &mut RouteContext) -> Result<()> {
        Ok(())
    }
    /// Called after the route processor (pipeline) is created; may wrap it.
    fn wrap_route(&self, _def: &RouteDefinition, _rc: &RouteContext, processor: Arc<dyn Processor>) -> Result<Arc<dyn Processor>> {
        Ok(processor)
    }
}

// ---------------------------------------------------------------------------
// CamelContext
// ---------------------------------------------------------------------------

pub(crate) struct ContextInner {
    name: String,
    components: RwLock<HashMap<String, Arc<dyn Component>>>,
    component_factories: RwLock<HashMap<String, Arc<dyn Fn() -> Arc<dyn Component> + Send + Sync>>>,
    endpoints: RwLock<HashMap<String, Arc<dyn Endpoint>>>,
    producers: RwLock<HashMap<String, Arc<dyn Producer>>>,
    /// `get_producer` results keyed by the uri exactly as given (without placeholders), so
    /// repeated sends to the same uri skip parsing and normalizing it.
    producers_by_uri: RwLock<HashMap<String, Arc<dyn Producer>>>,
    /// Changes whenever `producers_by_uri` is cleared and is unique across contexts, so a
    /// thread's cached lookup (`LAST_PRODUCER`) can tell when it is stale.
    producer_epoch: AtomicU64,
    registry: Registry,
    properties: PropertiesComponent,
    languages: RwLock<HashMap<String, Arc<dyn Language>>>,
    pending_routes: Mutex<Vec<RouteDefinition>>,
    routes: RwLock<Vec<Arc<Route>>>,
    status: RwLock<ServiceStatus>,
    notifiers: RwLock<Vec<Arc<dyn EventNotifier>>>,
    has_notifiers: Arc<AtomicBool>,
    route_hooks: RwLock<Vec<Arc<dyn RouteFactoryHook>>>,
    channel_wrappers: RwLock<Vec<Arc<dyn ChannelWrapper>>>,
    pub(crate) inflight: InflightRepository,
    pub(crate) exchange_counters: Arc<ExchangeCounters>,
    route_counter: AtomicUsize,
    shutdown_timeout: RwLock<Duration>,
    started_at: Mutex<Option<Instant>>,
    services: RwLock<Vec<Arc<dyn Service>>>,
    global_options: RwLock<BTreeMap<String, String>>,
    /// Whether any global option is set (lets the per-exchange lookups skip the lock).
    has_global_options: AtomicBool,
    startup_failure_handler: RwLock<Option<Arc<dyn RouteStartupFailureHandler>>>,
    type_converter: Arc<crate::typeconverter::TypeConverterRegistry>,
}

static PRODUCER_EPOCHS: AtomicU64 = AtomicU64::new(0);

type LastProducer = Option<(usize, u64, String, Arc<dyn Producer>)>;

thread_local! {
    /// The last `get_producer` result of this thread: (context, epoch, uri, producer).
    static LAST_PRODUCER: std::cell::RefCell<LastProducer> = const { std::cell::RefCell::new(None) };
}

/// The Camel context. Cheap to clone (shared handle).
#[derive(Clone)]
pub struct CamelContext {
    pub(crate) inner: Arc<ContextInner>,
}

/// Weak handle to a context (to avoid reference cycles in endpoints/consumers).
#[derive(Clone)]
pub struct WeakContext(Weak<ContextInner>);

impl WeakContext {
    pub fn upgrade(&self) -> Option<CamelContext> {
        self.0.upgrade().map(|inner| CamelContext { inner })
    }
    /// Whether this is a handle to `ctx` (no reference count is touched).
    pub fn is(&self, ctx: &CamelContext) -> bool {
        std::ptr::eq(self.0.as_ptr(), Arc::as_ptr(&ctx.inner))
    }
}

impl std::fmt::Debug for CamelContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CamelContext({})", self.inner.name)
    }
}

impl Default for CamelContext {
    fn default() -> Self {
        Self::new()
    }
}

/// `queue-size` -> `queueSize`.
pub fn dashed_to_camel_case(s: &str) -> String {
    let mut out = String::new();
    let mut up = false;
    for c in s.chars() {
        if c == '-' {
            up = true;
        } else if up {
            out.extend(c.to_uppercase());
            up = false;
        } else {
            out.push(c);
        }
    }
    out
}

static CONTEXT_COUNTER: AtomicUsize = AtomicUsize::new(0);

impl CamelContext {
    /// Create a new context with the built-in components and languages registered.
    pub fn new() -> Self {
        let n = CONTEXT_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
        Self::with_name(format!("camel-{}", n))
    }

    pub fn with_name(name: impl Into<String>) -> Self {
        let ctx = CamelContext {
            inner: Arc::new(ContextInner {
                name: name.into(),
                components: RwLock::new(HashMap::new()),
                component_factories: RwLock::new(HashMap::new()),
                endpoints: RwLock::new(HashMap::new()),
                producers: RwLock::new(HashMap::new()),
                producers_by_uri: RwLock::new(HashMap::new()),
                producer_epoch: AtomicU64::new(PRODUCER_EPOCHS.fetch_add(1, Ordering::AcqRel) + 1),
                registry: Registry::default(),
                properties: PropertiesComponent::default(),
                languages: RwLock::new(HashMap::new()),
                pending_routes: Mutex::new(Vec::new()),
                routes: RwLock::new(Vec::new()),
                status: RwLock::new(ServiceStatus::Initialized),
                notifiers: RwLock::new(Vec::new()),
                has_notifiers: Arc::new(AtomicBool::new(false)),
                route_hooks: RwLock::new(Vec::new()),
                channel_wrappers: RwLock::new(Vec::new()),
                inflight: InflightRepository::default(),
                exchange_counters: Arc::new(ExchangeCounters::default()),
                route_counter: AtomicUsize::new(0),
                shutdown_timeout: RwLock::new(Duration::from_secs(45)),
                started_at: Mutex::new(None),
                services: RwLock::new(Vec::new()),
                global_options: RwLock::new(BTreeMap::new()),
                has_global_options: AtomicBool::new(false),
                startup_failure_handler: RwLock::new(None),
                type_converter: Arc::new(crate::typeconverter::TypeConverterRegistry::new()),
            }),
        };
        crate::component::register_builtin_components(&ctx);
        crate::language::register_builtin_languages(&ctx);
        crate::extensions::register_builtin_extensions(&ctx);
        ctx
    }

    pub fn name(&self) -> &str {
        &self.inner.name
    }

    pub fn downgrade(&self) -> WeakContext {
        WeakContext(Arc::downgrade(&self.inner))
    }

    pub fn status(&self) -> ServiceStatus {
        *self.inner.status.read().unwrap()
    }

    /// The type converter registry (Camel `getTypeConverterRegistry()`).
    pub fn type_converter(&self) -> Arc<crate::typeconverter::TypeConverterRegistry> {
        self.inner.type_converter.clone()
    }

    pub fn is_started(&self) -> bool {
        self.status() == ServiceStatus::Started
    }

    pub fn uptime(&self) -> Option<Duration> {
        self.inner.started_at.lock().unwrap().map(|s| s.elapsed())
    }

    // --- registry / properties ---

    pub fn registry(&self) -> &Registry {
        &self.inner.registry
    }

    pub fn properties(&self) -> &PropertiesComponent {
        &self.inner.properties
    }

    /// Resolve `{{...}}` property placeholders.
    pub fn resolve_placeholders(&self, text: &str) -> Result<String> {
        self.inner.properties.resolve(text)
    }

    /// Global options (e.g. `CamelLogDebugBodyMaxChars`).
    pub fn set_global_option(&self, key: impl Into<String>, value: impl Into<String>) {
        self.inner.global_options.write().unwrap().insert(key.into(), value.into());
        self.inner.has_global_options.store(true, Ordering::Release);
    }
    #[inline]
    pub fn global_option(&self, key: &str) -> Option<String> {
        if !self.inner.has_global_options.load(Ordering::Acquire) {
            return None;
        }
        self.inner.global_options.read().unwrap().get(key).cloned()
    }

    pub fn set_shutdown_timeout(&self, d: Duration) {
        *self.inner.shutdown_timeout.write().unwrap() = d;
    }

    /// Timeout for graceful shutdown of routes (default 45 seconds, as in Camel).
    pub fn shutdown_timeout(&self) -> Duration {
        *self.inner.shutdown_timeout.read().unwrap()
    }

    // --- components / endpoints ---

    /// Register a component instance under a scheme.
    pub fn add_component(&self, scheme: impl Into<String>, component: impl Component + 'static) {
        self.add_component_arc(scheme, Arc::new(component));
    }

    pub fn add_component_arc(&self, scheme: impl Into<String>, component: Arc<dyn Component>) {
        let scheme = scheme.into();
        if self.is_started() {
            if let Err(e) = component.start() {
                crate::camel_warn!("CamelContext", "Failed to start component {}: {}", scheme, e);
            }
        }
        self.inner.components.write().unwrap().insert(scheme, component);
    }

    /// Register a lazily created component (created on first use).
    pub fn add_component_factory(&self, scheme: impl Into<String>, f: impl Fn() -> Arc<dyn Component> + Send + Sync + 'static) {
        self.inner.component_factories.write().unwrap().insert(scheme.into(), Arc::new(f));
    }

    pub fn has_component(&self, scheme: &str) -> bool {
        self.inner.components.read().unwrap().contains_key(scheme)
            || self.inner.component_factories.read().unwrap().contains_key(scheme)
    }

    pub fn component_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.inner.components.read().unwrap().keys().cloned().collect();
        for k in self.inner.component_factories.read().unwrap().keys() {
            if !v.contains(k) {
                v.push(k.clone());
            }
        }
        v.sort();
        v
    }

    /// Get a component, creating it from a factory if needed.
    pub fn get_component(&self, scheme: &str) -> Option<Arc<dyn Component>> {
        if let Some(c) = self.inner.components.read().unwrap().get(scheme) {
            return Some(c.clone());
        }
        let factory = self.inner.component_factories.read().unwrap().get(scheme).cloned()?;
        let mut comps = self.inner.components.write().unwrap();
        if let Some(c) = comps.get(scheme) {
            return Some(c.clone());
        }
        let c = factory();
        if self.is_started() {
            let _ = c.start();
        }
        comps.insert(scheme.to_string(), c.clone());
        Some(c)
    }

    /// Set a component-level option (Camel `camel.component.<scheme>.<option>`): used as the
    /// default of the endpoint option with the same name for endpoints created afterwards.
    pub fn set_component_option(&self, scheme: &str, option: &str, value: impl Into<String>) {
        self.set_global_option(format!("camel.component.{}.{}", scheme, option), value);
    }

    /// The component-level options of a scheme: `set_component_option` values and
    /// `camel.component.<scheme>.<option>` properties (dashed names are accepted:
    /// `queue-size` = `queueSize`).
    pub fn component_options(&self, scheme: &str) -> BTreeMap<String, String> {
        let prefix = format!("camel.component.{}.", scheme);
        let mut out = BTreeMap::new();
        let mut add = |k: &str, v: &str| {
            if let Some(opt) = k.strip_prefix(&prefix) {
                if !opt.is_empty() && !opt.contains('.') {
                    out.insert(dashed_to_camel_case(opt), self.resolve_placeholders(v).unwrap_or_else(|_| v.to_string()));
                }
            }
        };
        for (k, v) in self.inner.properties.all() {
            add(&k, &v);
        }
        for (k, v) in self.inner.global_options.read().unwrap().iter() {
            add(k, v);
        }
        out
    }

    /// Typed access to a component.
    pub fn component<T: 'static>(&self, scheme: &str) -> Option<Arc<dyn Component>> {
        self.get_component(scheme).filter(|c| c.as_any().is::<T>())
    }

    /// Resolve (or create and cache) an endpoint for a URI.
    pub fn get_endpoint(&self, uri: &str) -> Result<Arc<dyn Endpoint>> {
        let resolved = self.resolve_placeholders(uri.trim())?;
        // Allow "ref:name" lookups of endpoints bound in the registry
        let parsed = EndpointUri::parse(&resolved)?;
        let key = parsed.normalized();
        if let Some(e) = self.inner.endpoints.read().unwrap().get(&key) {
            return Ok(e.clone());
        }
        let endpoint = self.create_endpoint_unstarted(&parsed, &key)?;
        if endpoint.is_singleton() {
            let mut eps = self.inner.endpoints.write().unwrap();
            if let Some(existing) = eps.get(&key) {
                return Ok(existing.clone());
            }
            endpoint.start()?;
            eps.insert(key, endpoint.clone());
        } else {
            endpoint.start()?;
        }
        Ok(endpoint)
    }

    /// Check that an endpoint URI resolves, exactly as [`get_endpoint`](Self::get_endpoint)
    /// would (unknown scheme, unknown or invalid options fail), without starting the endpoint
    /// or caching it. Used by `cameleer validate`: nothing is started and no connection is
    /// opened.
    pub fn check_endpoint(&self, uri: &str) -> Result<()> {
        let resolved = self.resolve_placeholders(uri.trim())?;
        let parsed = EndpointUri::parse(&resolved)?;
        let key = parsed.normalized();
        if self.inner.endpoints.read().unwrap().contains_key(&key) {
            return Ok(());
        }
        self.create_endpoint_unstarted(&parsed, &key).map(|_| ())
    }

    /// Create an endpoint with its component and reject unknown parameters (not started,
    /// not cached).
    fn create_endpoint_unstarted(&self, parsed: &EndpointUri, key: &str) -> Result<Arc<dyn Endpoint>> {
        // Like Camel, endpoints are created with (and report) the normalized uri:
        // `mock:result` -> `mock://result`, parameters sorted.
        let component = self.get_component(&parsed.scheme).ok_or_else(|| {
            CamelError::resolve_endpoint_failed(key, format!("No component found with scheme: {}", parsed.scheme))
        })?;
        let mut params = Parameters::new(parsed.params.clone());
        // component-level options (`camel.component.<scheme>.<option>` properties or
        // `set_component_option`) are defaults for endpoint options not given in the uri;
        // ones the endpoint does not know are ignored
        let mut injected = Vec::new();
        if !component.lenient_properties() {
            for (k, v) in self.component_options(&parsed.scheme) {
                if params.get(&k).is_none() {
                    params.insert(k.clone(), v);
                    injected.push(k);
                }
            }
        }
        let endpoint = component
            .create_endpoint(self, key, &parsed.path, &mut params)
            .map_err(|e| {
                if e.kind == kinds::RESOLVE_ENDPOINT_FAILED {
                    e
                } else {
                    CamelError::resolve_endpoint_failed(key, &e.message).with_cause(e)
                }
            })?;
        for k in injected {
            params.take(&k);
        }
        if !params.is_empty() && !component.lenient_properties() {
            let unknown: Vec<String> = params.keys().cloned().collect();
            return Err(CamelError::resolve_endpoint_failed(
                key,
                format!(
                    "There are {} parameters that couldn't be set on the endpoint. Check the uri if the parameters are spelt correctly and that they are properties of the endpoint. Unknown parameters=[{}]",
                    unknown.len(),
                    unknown.join(", ")
                ),
            ));
        }
        Ok(endpoint)
    }

    /// The normalized form of an endpoint uri (placeholders resolved), as reported by
    /// `Endpoint::uri` and the `CamelToEndpoint` property: `mock:a` -> `mock://a`.
    /// Returns the uri unchanged when it cannot be parsed.
    pub fn normalize_endpoint_uri(&self, uri: &str) -> String {
        let resolved = self.resolve_placeholders(uri.trim()).unwrap_or_else(|_| uri.trim().to_string());
        crate::util::normalize_uri(&resolved).unwrap_or(resolved)
    }

    /// Typed endpoint lookup, e.g. `ctx.endpoint::<MockEndpoint>("mock:out")`.
    pub fn endpoint<T: 'static>(&self, uri: &str) -> Result<Arc<dyn Endpoint>> {
        let e = self.get_endpoint(uri)?;
        if e.as_any().is::<T>() {
            Ok(e)
        } else {
            Err(CamelError::illegal_argument(format!("Endpoint {} is not of the requested type", uri)))
        }
    }

    /// Get a cached, started producer for a URI (used by toD, templates, recipient list...).
    pub fn get_producer(&self, uri: &str) -> Result<Arc<dyn Producer>> {
        // uris with placeholders or registry references may resolve differently over time
        // this thread's last lookup (a template sends to the same uri again and again)
        let ctx_key = Arc::as_ptr(&self.inner) as usize;
        let epoch = self.inner.producer_epoch.load(Ordering::Acquire);
        let hit = LAST_PRODUCER
            .try_with(|c| match c.try_borrow().as_deref() {
                Ok(Some((k, e, u, p))) if *k == ctx_key && *e == epoch && u == uri => Some(p.clone()),
                _ => None,
            })
            .ok()
            .flatten();
        if let Some(p) = hit {
            return Ok(p);
        }
        // only cacheable uris are ever inserted, so look up first
        let cached = self.inner.producers_by_uri.read().unwrap().get(uri).cloned();
        if let Some(p) = cached {
            let _ = LAST_PRODUCER.try_with(|c| {
                // busy while a send borrows it (a nested send): just don't cache
                if let Ok(mut c) = c.try_borrow_mut() {
                    *c = Some((ctx_key, epoch, uri.to_string(), p.clone()));
                }
            });
            return Ok(p);
        }
        let cacheable = !uri.contains("{{") && !uri.trim_start().starts_with("ref:");
        let endpoint = self.get_endpoint(uri)?;
        let key = endpoint.uri().to_string();
        let key = crate::util::normalize_uri(&key).unwrap_or(key);
        let cached = self.inner.producers.read().unwrap().get(&key).cloned();
        let p = match cached {
            Some(p) => p,
            None => {
                let p = endpoint.create_producer()?;
                p.start()?;
                self.inner.producers.write().unwrap().insert(key, p.clone());
                p
            }
        };
        if cacheable {
            self.inner.producers_by_uri.write().unwrap().insert(uri.to_string(), p.clone());
        }
        Ok(p)
    }

    /// Run `f` with the producer for `uri`. A repeated uri borrows this thread's cached
    /// producer instead of cloning it (a reference count shared by every sending thread).
    pub fn with_producer<R>(&self, uri: &str, f: impl FnOnce(&dyn Producer) -> R) -> Result<R> {
        let ctx_key = Arc::as_ptr(&self.inner) as usize;
        let epoch = self.inner.producer_epoch.load(Ordering::Acquire);
        let mut f = Some(f);
        let hit = LAST_PRODUCER
            .try_with(|c| match c.try_borrow().as_deref() {
                Ok(Some((k, e, u, p))) if *k == ctx_key && *e == epoch && u == uri => f.take().map(|f| f(&**p)),
                _ => None,
            })
            .ok()
            .flatten();
        if let Some(r) = hit {
            return Ok(r);
        }
        let p = self.get_producer(uri)?;
        Ok((f.take().expect("not called yet"))(&*p))
    }

    /// All cached endpoints' uris.
    pub fn endpoint_uris(&self) -> Vec<String> {
        let mut v: Vec<String> = self.inner.endpoints.read().unwrap().values().map(|e| e.uri().to_string()).collect();
        v.sort();
        v
    }

    /// Typed access to the mock endpoint for a uri (`mock:name`).
    pub fn mock_endpoint(&self, uri: &str) -> Result<crate::component::mock::MockEndpoint> {
        let uri = if uri.starts_with("mock:") { uri.to_string() } else { format!("mock:{}", uri) };
        let e = self.get_endpoint(&uri)?;
        e.as_any()
            .downcast_ref::<crate::component::mock::MockEndpoint>()
            .cloned()
            .ok_or_else(|| CamelError::illegal_argument(format!("{} is not a mock endpoint", uri)))
    }

    // --- languages ---

    pub fn add_language(&self, language: impl Language + 'static) {
        let l: Arc<dyn Language> = Arc::new(language);
        self.inner.languages.write().unwrap().insert(l.name().to_string(), l);
    }

    pub fn add_language_arc(&self, language: Arc<dyn Language>) {
        self.inner.languages.write().unwrap().insert(language.name().to_string(), language);
    }

    pub fn resolve_language(&self, name: &str) -> Result<Arc<dyn Language>> {
        self.inner.languages.read().unwrap().get(name).cloned().ok_or_else(|| CamelError::no_such_language(name))
    }

    pub fn language_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.inner.languages.read().unwrap().keys().cloned().collect();
        v.sort();
        v
    }

    // --- events / extension hooks ---

    pub fn add_event_notifier(&self, n: impl EventNotifier + 'static) {
        self.inner.notifiers.write().unwrap().push(Arc::new(n));
        self.inner.has_notifiers.store(true, Ordering::Release);
    }

    /// Whether any event notifier is registered. Hot paths check this before building an
    /// event, which clones ids.
    pub fn has_event_notifiers(&self) -> bool {
        self.inner.has_notifiers.load(Ordering::Acquire)
    }

    pub fn notify(&self, event: &CamelEvent) {
        if !self.has_event_notifiers() {
            return;
        }
        let notifiers = self.inner.notifiers.read().unwrap().clone();
        for n in notifiers {
            n.notify(event);
        }
    }

    /// Register a hook applied to every route created after this call.
    pub fn add_route_factory_hook(&self, hook: Arc<dyn RouteFactoryHook>) {
        self.inner.route_hooks.write().unwrap().push(hook);
    }

    /// Register a context-wide channel wrapper (applied to all processors in all routes).
    pub fn add_channel_wrapper(&self, w: Arc<dyn ChannelWrapper>) {
        self.inner.channel_wrappers.write().unwrap().push(w);
    }

    /// Install the handler deciding what happens when an auto-startup route fails to start.
    pub fn set_route_startup_failure_handler(&self, h: Option<Arc<dyn RouteStartupFailureHandler>>) {
        *self.inner.startup_failure_handler.write().unwrap() = h;
    }

    fn start_auto_route(&self, route: &Arc<Route>) -> Result<()> {
        match self.start_route_internal(route) {
            Ok(()) => Ok(()),
            Err(e) => {
                let h = self.inner.startup_failure_handler.read().unwrap().clone();
                match h {
                    Some(h) => h.on_route_startup_failure(self, &route.id, &e),
                    None => Err(e),
                }
            }
        }
    }

    /// Register a service started/stopped with the context.
    pub fn add_service(&self, s: Arc<dyn Service>) -> Result<()> {
        if self.is_started() {
            s.start()?;
        }
        self.inner.services.write().unwrap().push(s);
        Ok(())
    }

    // --- routes ---

    /// Add routes using the fluent builder:
    /// ```ignore
    /// ctx.add_routes(|b| { b.from("direct:a").to("mock:b"); })?;
    /// ```
    pub fn add_routes<F>(&self, f: F) -> Result<()>
    where
        F: FnOnce(&mut RouteBuilder),
    {
        let mut b = RouteBuilder::new(self.clone());
        f(&mut b);
        self.add_route_builder(b)
    }

    pub fn add_route_builder(&self, b: RouteBuilder) -> Result<()> {
        let defs = b.into_definitions()?;
        for d in defs {
            self.add_route_definition(d)?;
        }
        Ok(())
    }

    /// Add a single route definition. If the context is started the route is created
    /// and started immediately; otherwise it is created on `start()`.
    pub fn add_route_definition(&self, mut def: RouteDefinition) -> Result<()> {
        def.finish()?;
        if self.is_started() {
            let route = self.create_route(def)?;
            self.inner.routes.write().unwrap().push(route.clone());
            self.notify(&CamelEvent::RouteAdded(route.id.clone()));
            if route.auto_startup {
                self.start_auto_route(&route)?;
            }
        } else {
            self.inner.pending_routes.lock().unwrap().push(def);
        }
        Ok(())
    }

    /// Give the pending (not yet created) routes without an id their generated id now
    /// (`route1`, ... in the order they would get it at startup) and return the ids of all
    /// pending routes.
    pub fn assign_pending_route_ids(&self) -> Vec<String> {
        let n = self.inner.pending_routes.lock().unwrap().len();
        for i in 0..n {
            let missing = self.inner.pending_routes.lock().unwrap().get(i).map(|d| d.id.is_none()).unwrap_or(false);
            if missing {
                let id = self.next_route_id();
                if let Some(d) = self.inner.pending_routes.lock().unwrap().get_mut(i) {
                    d.id = Some(id);
                }
            }
        }
        self.inner.pending_routes.lock().unwrap().iter().filter_map(|d| d.id.clone()).collect()
    }

    fn next_route_id(&self) -> String {
        loop {
            let n = self.inner.route_counter.fetch_add(1, Ordering::Relaxed) + 1;
            let id = format!("route{}", n);
            if self.route(&id).is_none() && !self.inner.pending_routes.lock().unwrap().iter().any(|d| d.id.as_deref() == Some(&id)) {
                return id;
            }
        }
    }

    fn create_route(&self, mut def: RouteDefinition) -> Result<Arc<Route>> {
        let id = match &def.id {
            Some(id) => id.clone(),
            None => {
                let id = self.next_route_id();
                def.id = Some(id.clone());
                id
            }
        };
        if self.route(&id).is_some() {
            return Err(CamelError::new(kinds::FAILED_TO_CREATE_ROUTE, format!("Route with id {} already exists", id)));
        }
        let mut rc = RouteContext::new(self.clone(), id.clone());
        rc.channel_wrappers.extend(self.inner.channel_wrappers.read().unwrap().iter().cloned());
        let hooks = self.inner.route_hooks.read().unwrap().clone();
        for h in &hooks {
            h.before_create(&def, &mut rc)?;
        }
        let endpoint = self.get_endpoint(&def.input).map_err(|e| {
            CamelError::new(kinds::FAILED_TO_CREATE_ROUTE, format!("Failed to create route {}: {}", id, e.message)).with_cause(e)
        })?;
        let mut processor = rc.create_child_processor(&def.outputs)?;
        if def.outputs.is_empty() {
            processor = Arc::new(crate::processor::pipeline::Pipeline::new(vec![]));
        }
        for h in &hooks {
            processor = h.wrap_route(&def, &rc, processor)?;
        }
        let stats = Arc::new(RouteStats::default());
        let route_processor: Arc<dyn Processor> = Arc::new(RouteProcessor {
            route_id: Arc::from(id.as_str()),
            endpoint_uri: Arc::from(endpoint.uri()),
            inner: processor,
            ctx: self.downgrade(),
            stats: stats.clone(),
            inflight: self.inner.inflight.counter(&id),
            counters: self.inner.exchange_counters.clone(),
            notifiers: self.inner.has_notifiers.clone(),
            timer: rc.attribute::<Arc<dyn RouteTimer>>(ROUTE_TIMER_ATTR).map(|t| (*t).clone()),
        });
        let services = rc.take_services();
        Ok(Arc::new(Route {
            id,
            endpoint_uri: endpoint.uri().to_string(),
            description: def.description.clone(),
            group: def.group.clone(),
            auto_startup: def.auto_startup,
            startup_order: def.startup_order.unwrap_or(1000),
            definition_dump: def.dump(),
            definition: Mutex::new(Some(def)),
            endpoint,
            processor: route_processor,
            services,
            consumer: Mutex::new(None),
            status: RwLock::new(ServiceStatus::Initialized),
            stats,
            uptime_start: Mutex::new(None),
        }))
    }

    fn start_route_internal(&self, route: &Arc<Route>) -> Result<()> {
        if route.status() == ServiceStatus::Started {
            return Ok(());
        }
        *route.status.write().unwrap() = ServiceStatus::Starting;
        let r = (|| -> Result<()> {
            for s in &route.services {
                s.start()?;
            }
            let consumer = route.endpoint.create_consumer(route.processor.clone())?;
            consumer.start()?;
            *route.consumer.lock().unwrap() = Some(consumer);
            Ok(())
        })();
        match r {
            Ok(()) => {
                *route.status.write().unwrap() = ServiceStatus::Started;
                *route.uptime_start.lock().unwrap() = Some(Instant::now());
                crate::camel_debug!("CamelContext", "Route: {} started and consuming from: {}", route.id, route.endpoint_uri);
                self.notify(&CamelEvent::RouteStarted(route.id.clone()));
                Ok(())
            }
            Err(e) => {
                *route.status.write().unwrap() = ServiceStatus::Stopped;
                Err(CamelError::new(kinds::FAILED_TO_START_ROUTE, format!("Failed to start route {} because of {}", route.id, e.message))
                    .with_cause(e))
            }
        }
    }

    fn stop_route_internal(&self, route: &Arc<Route>, timeout: Duration) -> Result<()> {
        if matches!(route.status(), ServiceStatus::Stopped | ServiceStatus::Initialized) {
            return Ok(());
        }
        *route.status.write().unwrap() = ServiceStatus::Stopping;
        let consumer = route.consumer.lock().unwrap().take();
        if let Some(c) = consumer {
            if let Err(e) = c.stop() {
                crate::camel_warn!("CamelContext", "Error stopping consumer for route {}: {}", route.id, e);
            }
        }
        // graceful: wait for inflight exchanges of this route
        let deadline = Instant::now() + timeout;
        while self.inner.inflight.route_size(&route.id) > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if self.inner.inflight.route_size(&route.id) > 0 {
            crate::camel_warn!("CamelContext", "Timeout waiting for inflight exchanges on route {}", route.id);
        }
        for s in route.services.iter().rev() {
            if let Err(e) = s.stop() {
                crate::camel_warn!("CamelContext", "Error stopping service on route {}: {}", route.id, e);
            }
        }
        *route.status.write().unwrap() = ServiceStatus::Stopped;
        *route.uptime_start.lock().unwrap() = None;
        self.notify(&CamelEvent::RouteStopped(route.id.clone()));
        Ok(())
    }

    /// Start the context: start components, create and start routes.
    pub fn start(&self) -> Result<()> {
        if self.is_started() {
            return Ok(());
        }
        if self.status() == ServiceStatus::Suspended {
            return self.resume();
        }
        *self.inner.status.write().unwrap() = ServiceStatus::Starting;
        self.notify(&CamelEvent::ContextStarting);
        let result = (|| -> Result<()> {
            crate::startup::step("components", "start", || -> Result<()> {
                let comps: Vec<Arc<dyn Component>> = self.inner.components.read().unwrap().values().cloned().collect();
                for c in comps {
                    c.start()?;
                }
                for s in self.inner.services.read().unwrap().iter() {
                    s.start()?;
                }
                Ok(())
            })?;
            let pending: Vec<RouteDefinition> = std::mem::take(&mut *self.inner.pending_routes.lock().unwrap());
            crate::startup::step("routes", "create", || -> Result<()> {
                for def in pending {
                    let name = def.id.clone().unwrap_or_default();
                    let r = crate::startup::step("route", name, || self.create_route(def))?;
                    self.inner.routes.write().unwrap().push(r.clone());
                    self.notify(&CamelEvent::RouteAdded(r.id.clone()));
                }
                Ok(())
            })?;
            // Mark started before starting consumers so routes added later start immediately.
            *self.inner.status.write().unwrap() = ServiceStatus::Started;
            let mut all = self.routes();
            all.sort_by_key(|r| r.startup_order);
            crate::startup::step("routes", "start", || -> Result<()> {
                for r in all {
                    if r.auto_startup {
                        crate::startup::step("consumer", r.id.clone(), || self.start_auto_route(&r))?;
                    }
                }
                Ok(())
            })
        })();
        match result {
            Ok(()) => {
                *self.inner.started_at.lock().unwrap() = Some(Instant::now());
                let started = self.routes().iter().filter(|r| r.status() == ServiceStatus::Started).count();
                crate::camel_info!(
                    "CamelContext",
                    "Cameleer {} ({}) started ({} routes, {} started)",
                    env!("CARGO_PKG_VERSION"),
                    self.name(),
                    self.routes().len(),
                    started
                );
                self.notify(&CamelEvent::ContextStarted);
                Ok(())
            }
            Err(e) => {
                let _ = self.stop();
                Err(e)
            }
        }
    }

    /// Stop the context: stop routes (reverse startup order, graceful), components and services.
    pub fn stop(&self) -> Result<()> {
        let st = self.status();
        if matches!(st, ServiceStatus::Stopped | ServiceStatus::Stopping) {
            return Ok(());
        }
        *self.inner.status.write().unwrap() = ServiceStatus::Stopping;
        self.notify(&CamelEvent::ContextStopping);
        let timeout = *self.inner.shutdown_timeout.read().unwrap();
        let mut routes = self.routes();
        routes.sort_by_key(|r| std::cmp::Reverse(r.startup_order));
        for r in &routes {
            let _ = self.stop_route_internal(r, timeout);
        }
        self.inner.producers_by_uri.write().unwrap().clear();
        self.inner.producer_epoch.store(PRODUCER_EPOCHS.fetch_add(1, Ordering::AcqRel) + 1, Ordering::Release);
        let producers: Vec<Arc<dyn Producer>> = self.inner.producers.write().unwrap().drain().map(|(_, p)| p).collect();
        for p in producers {
            let _ = p.stop();
        }
        for s in self.inner.services.read().unwrap().iter().rev() {
            let _ = s.stop();
        }
        let eps: Vec<Arc<dyn Endpoint>> = self.inner.endpoints.read().unwrap().values().cloned().collect();
        for e in eps {
            let _ = e.stop();
        }
        let comps: Vec<Arc<dyn Component>> = self.inner.components.read().unwrap().values().cloned().collect();
        for c in comps {
            let _ = c.stop();
        }
        *self.inner.status.write().unwrap() = ServiceStatus::Stopped;
        let uptime = self.uptime().map(|d| format!("{:.3}s", d.as_secs_f64())).unwrap_or_default();
        crate::camel_info!("CamelContext", "Cameleer ({}) shutdown in {} (uptime)", self.name(), uptime);
        self.notify(&CamelEvent::ContextStopped);
        Ok(())
    }

    /// All runtime routes.
    pub fn routes(&self) -> Vec<Arc<Route>> {
        self.inner.routes.read().unwrap().clone()
    }

    pub fn route(&self, id: &str) -> Option<Arc<Route>> {
        self.inner.routes.read().unwrap().iter().find(|r| r.id == id).cloned()
    }

    pub fn route_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.routes().iter().map(|r| r.id.clone()).collect();
        for d in self.inner.pending_routes.lock().unwrap().iter() {
            if let Some(id) = &d.id {
                ids.push(id.clone());
            }
        }
        ids
    }

    pub fn route_status(&self, id: &str) -> Option<ServiceStatus> {
        self.route(id).map(|r| r.status())
    }

    pub fn start_route(&self, id: &str) -> Result<()> {
        let r = self.route(id).ok_or_else(|| CamelError::illegal_argument(format!("No route with id {}", id)))?;
        if r.status() == ServiceStatus::Suspended {
            return self.resume_route(id);
        }
        self.start_route_internal(&r)
    }

    pub fn stop_route(&self, id: &str) -> Result<()> {
        let r = self.route(id).ok_or_else(|| CamelError::illegal_argument(format!("No route with id {}", id)))?;
        let timeout = *self.inner.shutdown_timeout.read().unwrap();
        self.stop_route_internal(&r, timeout)
    }

    /// Suspend a route (Camel `suspendRoute`). When the route's consumer is a
    /// [`SuspendableService`](crate::spi::SuspendableService) it is suspended in place (its
    /// threads/sockets/queues stay, no new exchanges are created) and in-flight exchanges are
    /// allowed to complete; otherwise the route is stopped, as Camel does for consumers that
    /// do not support suspension. The route status becomes `Suspended` either way.
    pub fn suspend_route(&self, id: &str) -> Result<()> {
        let r = self.route(id).ok_or_else(|| CamelError::illegal_argument(format!("No route with id {}", id)))?;
        match r.status() {
            ServiceStatus::Suspended => return Ok(()),
            ServiceStatus::Started => {}
            _ => return Ok(()),
        }
        let consumer = r.consumer.lock().unwrap().clone();
        let in_place = match consumer.as_ref().and_then(|c| c.as_suspendable()) {
            Some(s) => {
                s.suspend()?;
                true
            }
            None => false,
        };
        if in_place {
            let timeout = *self.inner.shutdown_timeout.read().unwrap();
            let deadline = Instant::now() + timeout;
            while self.inner.inflight.route_size(id) > 0 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
        } else {
            self.stop_route(id)?;
        }
        *r.status.write().unwrap() = ServiceStatus::Suspended;
        crate::camel_info!("CamelContext", "Route: {} is suspended", id);
        self.notify(&CamelEvent::RouteSuspended(id.to_string()));
        Ok(())
    }

    /// Resume a suspended route (Camel `resumeRoute`): a suspended-in-place consumer is
    /// resumed, a stopped one is started again. A route that is not suspended is started.
    pub fn resume_route(&self, id: &str) -> Result<()> {
        let r = self.route(id).ok_or_else(|| CamelError::illegal_argument(format!("No route with id {}", id)))?;
        if r.status() != ServiceStatus::Suspended {
            return self.start_route(id);
        }
        let consumer = r.consumer.lock().unwrap().clone();
        match consumer.as_ref().and_then(|c| c.as_suspendable()) {
            Some(s) if s.is_suspended() => {
                s.resume()?;
                *r.status.write().unwrap() = ServiceStatus::Started;
            }
            _ => {
                *r.status.write().unwrap() = ServiceStatus::Stopped;
                self.start_route(id)?;
            }
        }
        crate::camel_info!("CamelContext", "Route: {} resumed", id);
        self.notify(&CamelEvent::RouteResumed(id.to_string()));
        Ok(())
    }

    /// Whether the route's consumer is currently suspended in place (rather than stopped).
    pub fn is_route_suspended_in_place(&self, id: &str) -> bool {
        self.route(id)
            .and_then(|r| r.consumer.lock().unwrap().clone())
            .and_then(|c| c.as_suspendable().map(|s| s.is_suspended()))
            .unwrap_or(false)
    }

    /// Suspend the context (Camel `CamelContext.suspend()`): all started routes are
    /// suspended (in reverse startup order) and the context status becomes `Suspended`.
    pub fn suspend(&self) -> Result<()> {
        if self.status() != ServiceStatus::Started {
            return Ok(());
        }
        let mut routes = self.routes();
        routes.sort_by_key(|r| std::cmp::Reverse(r.startup_order));
        for r in routes.iter().filter(|r| r.status() == ServiceStatus::Started) {
            self.suspend_route(&r.id)?;
        }
        *self.inner.status.write().unwrap() = ServiceStatus::Suspended;
        self.notify(&CamelEvent::ContextSuspended);
        Ok(())
    }

    /// Resume a suspended context: the suspended routes are resumed in startup order.
    pub fn resume(&self) -> Result<()> {
        if self.status() != ServiceStatus::Suspended {
            return Ok(());
        }
        *self.inner.status.write().unwrap() = ServiceStatus::Started;
        let mut routes = self.routes();
        routes.sort_by_key(|r| r.startup_order);
        for r in routes.iter().filter(|r| r.status() == ServiceStatus::Suspended) {
            self.resume_route(&r.id)?;
        }
        self.notify(&CamelEvent::ContextResumed);
        Ok(())
    }

    /// Remove a stopped route. Returns false if the route is not stopped.
    pub fn remove_route(&self, id: &str) -> Result<bool> {
        let r = match self.route(id) {
            Some(r) => r,
            None => return Ok(false),
        };
        if !matches!(r.status(), ServiceStatus::Stopped | ServiceStatus::Initialized | ServiceStatus::Suspended) {
            return Ok(false);
        }
        if r.status() == ServiceStatus::Suspended && r.consumer.lock().unwrap().is_some() {
            self.stop_route(id)?;
        }
        self.inner.routes.write().unwrap().retain(|x| x.id != id);
        self.notify(&CamelEvent::RouteRemoved(id.to_string()));
        Ok(true)
    }

    /// Number of inflight exchanges.
    pub fn inflight_count(&self) -> usize {
        self.inner.inflight.size()
    }

    // --- templates ---

    pub fn create_producer_template(&self) -> ProducerTemplate {
        ProducerTemplate::new(self.clone())
    }

    pub fn create_consumer_template(&self) -> ConsumerTemplate {
        ConsumerTemplate::new(self.clone())
    }

    /// Create a new exchange bound to this context.
    pub fn create_exchange(&self) -> Exchange {
        Exchange::new(Some(self.clone()))
    }
}

impl Drop for ContextInner {
    fn drop(&mut self) {
        // Best effort: stop consumers of routes still running.
        if let Ok(routes) = self.routes.read() {
            for r in routes.iter() {
                if let Ok(mut c) = r.consumer.lock() {
                    if let Some(c) = c.take() {
                        let _ = c.stop();
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_endpoint_validates_without_caching() {
        let ctx = CamelContext::new();
        ctx.check_endpoint("timer:t?period=10").unwrap();
        assert!(ctx.endpoint_uris().is_empty(), "check_endpoint must not cache: {:?}", ctx.endpoint_uris());
        let e = ctx.check_endpoint("timer:t?perod=10").unwrap_err();
        assert_eq!(e.kind, kinds::RESOLVE_ENDPOINT_FAILED);
        assert!(e.message.contains("Unknown parameters=[perod]"), "{}", e.message);
        let e = ctx.check_endpoint("nosuch:x").unwrap_err();
        assert!(e.message.contains("No component found with scheme: nosuch"), "{}", e.message);
        // get_endpoint is unchanged: creates and caches
        ctx.get_endpoint("timer:t?period=10").unwrap();
        assert_eq!(ctx.endpoint_uris().len(), 1);
        ctx.check_endpoint("timer:t?period=10").unwrap();
        assert!(ctx.get_endpoint("timer:t?perod=10").is_err());
    }

    #[test]
    fn placeholders() {
        let p = PropertiesComponent::default();
        p.set("a", "1");
        p.set("b", "x{{a}}");
        assert_eq!(p.resolve("v={{b}}").unwrap(), "v=x1");
        assert_eq!(p.resolve("{{missing:def}}").unwrap(), "def");
        assert!(p.resolve("{{missing}}").is_err());
        std::env::set_var("RC_TEST_ENV", "e");
        assert_eq!(p.resolve("{{env:RC_TEST_ENV}}").unwrap(), "e");
        assert_eq!(p.resolve("{{env:RC_NOPE:dflt}}").unwrap(), "dflt");
    }

    #[test]
    fn properties_functions() {
        let p = PropertiesComponent::default();
        p.add_function(
            "vault",
            Arc::new(|k: &str| match k {
                "db/user" => Ok(Some("scott".to_string())),
                "boom" => Err(CamelError::illegal_state("vault down")),
                _ => Ok(None),
            }),
        );
        assert!(p.has_function("vault"));
        assert_eq!(p.resolve("u={{vault:db/user}}").unwrap(), "u=scott");
        assert_eq!(p.resolve("{{vault:db/user:x}}").unwrap(), "scott");
        assert_eq!(p.resolve("{{vault:missing:fallback}}").unwrap(), "fallback");
        assert!(p.resolve("{{vault:missing}}").is_err());
        assert_eq!(p.resolve("{{vault:boom}}").unwrap_err().message, "vault down");
        assert_eq!(p.lookup("vault:db/user").unwrap().as_deref(), Some("scott"));
    }
}

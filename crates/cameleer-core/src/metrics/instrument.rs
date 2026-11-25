//! Camel's built-in meters (`camel.metrics.*`, Camel's `camel-micrometer-prometheus`; and the
//! same set with `camel-opentelemetry-metrics`' names and units):
//!
//! | part | Micrometer | OpenTelemetry |
//! |---|---|---|
//! | route policy | timer `camel.route.policy`; counters `camel.exchanges.total`, `.succeeded`, `.failed`, `.failures.handled`, `.external.redeliveries`; tags `camelContext`, `kind=CamelRoute`, `routeId`, `eventType=route` (and `routeId=""`, `eventType=context` for the context) | histogram `camel.route.policy` in `milliseconds`; the same counters |
//! | exchange event notifier | timer `camel.exchange.event.notifier` (`eventType` `ExchangeCompletedEvent`, `ExchangeFailedEvent`, `ExchangeSentEvent`; `endpointName`, `failed`, `routeId`, `kind=CamelExchangeEvent`); gauges `camel.exchanges.inflight` per route and `camel.exchanges.last.timestamp` | histograms `camel.exchange.elapsed` and `camel.exchange.sent`; gauges `camel.exchanges.inflight`, `camel.exchanges.last.time` |
//! | route event notifier | gauges `camel.routes.added`, `camel.routes.running` (unit `routes`; `kind=CamelRoute`, `eventType=RouteEvent`) | the same |
//! | message history | timer `camel.message.history` (`routeId`, `nodeId`, `kind=CamelMessageHistory`) | histogram `camel.message.history` |
//! | `app.info` | gauge with `camel.version`, `camel.context`, `camel.runtime.provider`, `camel.runtime.version` | the same |
//!
//! With `namingStrategy=legacy` the Micrometer names are Camel 3's (`CamelRoutePolicy`,
//! `CamelExchangesSucceeded`, ...). [`install`] adds hooks and wrappers only for the enabled
//! parts; without it nothing runs.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::context::{CamelContext, RouteFactoryHook, ServiceStatus};
use crate::error::Result;
use crate::message::Exchange;
use crate::model::{ChannelWrapper, ProcessorDefinition, RouteContext, RouteDefinition};
use crate::spi::{AsyncProcessor, Blocking, Endpoint, ProcessFuture, Processor, Producer, Service, SuspendableService};
use crate::telemetry::ProducerWrapper;

use super::{Counter, Distribution, Gauge, MeterRegistry, Timer, OTEL_BOUNDS};

/// Which component's names and units the built-in meters use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    /// `camel.metrics.*` (camel-micrometer-prometheus): timers in seconds.
    Micrometer,
    /// `camel.opentelemetry.metrics.*` (camel-opentelemetry-metrics): histograms in a time unit.
    OpenTelemetry,
}

/// `camel.metrics.routePolicyLevel`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolicyLevel {
    All,
    Context,
    Route,
}

/// The settings of the built-in meters (Camel Main's `MetricsConfigurationProperties`).
#[derive(Clone, Debug)]
pub struct MetricsConfig {
    pub flavor: Flavor,
    /// `namingStrategy=legacy`: Camel 3 names.
    pub legacy_names: bool,
    pub route_policy: bool,
    pub route_policy_level: PolicyLevel,
    /// Routes not measured by the route policy (`routePolicyExcludePattern`).
    pub route_policy_exclude: Vec<String>,
    pub message_history: bool,
    pub exchange_event_notifier: bool,
    /// `endpointName` without the options (`baseEndpointURIExchangeEventNotifier`).
    pub base_endpoint_uri: bool,
    pub record_last_exchange_timestamp: bool,
    pub route_event_notifier: bool,
    pub skip_camel_info: bool,
    /// OpenTelemetry: the unit name of durations and how many fit in a second.
    pub time_unit: (String, f64),
}

impl Default for MetricsConfig {
    fn default() -> Self {
        MetricsConfig {
            flavor: Flavor::Micrometer,
            legacy_names: false,
            route_policy: true,
            route_policy_level: PolicyLevel::All,
            route_policy_exclude: Vec::new(),
            message_history: false,
            exchange_event_notifier: true,
            base_endpoint_uri: true,
            record_last_exchange_timestamp: true,
            route_event_notifier: true,
            skip_camel_info: false,
            time_unit: ("milliseconds".into(), 1e3),
        }
    }
}

/// Camel 3's name of a meter (Camel's `MicrometerUtils.legacyName`): `.` as `-`, camel case,
/// first letter upper case (`camel.route.policy` is `CamelRoutePolicy`).
pub fn legacy_name(name: &str) -> String {
    name.split('.').map(|p| {
        let mut c = p.chars();
        match c.next() {
            Some(f) => f.to_ascii_uppercase().to_string() + c.as_str(),
            None => String::new(),
        }
    })
    .collect()
}

/// Records durations into a timer (seconds) or an OpenTelemetry histogram (its unit).
#[derive(Clone)]
enum Rec {
    Timer(Arc<Timer>),
    Hist(Arc<Distribution>, f64),
}

impl Rec {
    fn record(&self, d: std::time::Duration) {
        match self {
            Rec::Timer(t) => t.record(d),
            Rec::Hist(h, per_sec) => h.record((d.as_secs_f64() * per_sec).trunc()),
        }
    }
}

struct Names {
    cfg: MetricsConfig,
    registry: Arc<MeterRegistry>,
    context: String,
}

impl Names {
    fn n(&self, name: &str) -> String {
        if self.cfg.legacy_names && self.cfg.flavor == Flavor::Micrometer && name != "camel.exchanges.last.timestamp" && name != "app.info" {
            legacy_name(name)
        } else {
            name.to_string()
        }
    }
    fn tags(&self, pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut t = vec![("camelContext".to_string(), self.context.clone())];
        t.extend(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        t
    }
    fn duration(&self, name: &str, tags: &[(String, String)], desc: &str) -> Rec {
        let name = self.n(name);
        match self.cfg.flavor {
            Flavor::Micrometer => Rec::Timer(self.registry.timer(&name, tags, Some(desc)).unwrap_or_else(|_| Arc::new(Timer(Distribution::new(super::DEFAULT_BOUNDS))))),
            Flavor::OpenTelemetry => Rec::Hist(
                self.registry.histogram(&name, tags, Some(desc), &self.cfg.time_unit.0, OTEL_BOUNDS).unwrap_or_else(|_| Arc::new(Distribution::new(OTEL_BOUNDS))),
                self.cfg.time_unit.1,
            ),
        }
    }
    fn counter(&self, name: &str, tags: &[(String, String)], desc: &str) -> Arc<Counter> {
        self.registry.counter(&self.n(name), tags, Some(desc)).unwrap_or_default()
    }
}

/// The route policy meters of one route, or of the context.
struct PolicyMeters {
    time: Rec,
    total: Arc<Counter>,
    succeeded: Arc<Counter>,
    failed: Arc<Counter>,
    handled: Arc<Counter>,
    redeliveries: Arc<Counter>,
}

impl PolicyMeters {
    fn new(n: &Names, route_id: &str, event_type: &str, desc: &str) -> PolicyMeters {
        let tags = n.tags(&[("kind", "CamelRoute"), ("routeId", route_id), ("eventType", event_type)]);
        PolicyMeters {
            time: n.duration("camel.route.policy", &tags, desc),
            total: n.counter("camel.exchanges.total", &tags, "Total number of processed exchanges"),
            succeeded: n.counter("camel.exchanges.succeeded", &tags, "Number of successfully completed exchanges"),
            failed: n.counter("camel.exchanges.failed", &tags, "Number of failed exchanges"),
            handled: n.counter("camel.exchanges.failures.handled", &tags, "Number of failures handled"),
            redeliveries: n.counter("camel.exchanges.external.redeliveries", &tags, "Number of external initiated redeliveries (such as from JMS broker)"),
        }
    }
    fn record(&self, d: std::time::Duration, ex: &Exchange, failed: bool) {
        self.time.record(d);
        self.total.increment(1.0);
        if failed {
            self.failed.increment(1.0);
        } else {
            self.succeeded.increment(1.0);
            if ex.property(crate::message::props::ERRORHANDLER_HANDLED).and_then(|v| v.as_bool()).unwrap_or(false) {
                self.handled.increment(1.0);
            }
            if ex.header("JMSRedelivered").and_then(|v| v.as_bool()).unwrap_or(false) {
                self.redeliveries.increment(1.0);
            }
        }
    }
}

/// The installed built-in meters of a context.
pub struct Instrumentation {
    names: Arc<Names>,
    context_policy: Option<Arc<PolicyMeters>>,
    last: Option<Arc<Gauge>>,
}

/// Installs the built-in meters in `ctx` (before its routes are created) and returns its
/// registry.
pub fn install(ctx: &CamelContext, cfg: MetricsConfig) -> Arc<MeterRegistry> {
    let registry = MeterRegistry::of(ctx);
    let names = Arc::new(Names { cfg: cfg.clone(), registry: registry.clone(), context: ctx.name().to_string() });
    let context_policy = (cfg.route_policy && cfg.route_policy_level != PolicyLevel::Route).then(|| Arc::new(PolicyMeters::new(&names, "", "context", "CamelContext performance metrics")));
    let last = (cfg.exchange_event_notifier && cfg.record_last_exchange_timestamp).then(|| {
        let (name, unit) = match cfg.flavor {
            Flavor::Micrometer => ("camel.exchanges.last.timestamp", ""),
            Flavor::OpenTelemetry => ("camel.exchanges.last.time", "milliseconds"),
        };
        let _ = unit;
        registry.gauge(name, &[], Some("Last exchange processed time in milliseconds since the Unix epoch")).unwrap_or_default()
    });
    let inst = Arc::new(Instrumentation { names: names.clone(), context_policy, last });
    if cfg.route_event_notifier {
        let tags = names.tags(&[("kind", "CamelRoute"), ("eventType", "RouteEvent")]);
        let w = ctx.downgrade();
        registry.fn_gauge_with_unit(&names.n("camel.routes.added"), &tags, Some("Number of routes added"), "routes", move || w.upgrade().map(|c| c.routes().len() as f64).unwrap_or(0.0));
        let w = ctx.downgrade();
        registry.fn_gauge_with_unit(&names.n("camel.routes.running"), &tags, Some("Number of routes running"), "routes", move || {
            w.upgrade().map(|c| c.routes().iter().filter(|r| r.status() == ServiceStatus::Started).count() as f64).unwrap_or(0.0)
        });
    }
    if !cfg.skip_camel_info {
        let tags = vec![
            ("camel.version".to_string(), crate::VERSION.to_string()),
            ("camel.context".to_string(), ctx.name().to_string()),
            ("camel.runtime.provider".to_string(), "Main".to_string()),
            ("camel.runtime.version".to_string(), crate::VERSION.to_string()),
        ];
        registry.fn_gauge("app.info", &tags, Some("Application info"), || 1.0);
    }
    ctx.add_route_factory_hook(Arc::new(Hook { inst: inst.clone(), ctx: ctx.downgrade() }));
    if cfg.exchange_event_notifier {
        ctx.add_producer_wrapper(Arc::new(SentWrapper { inst }));
    }
    registry
}

struct Hook {
    inst: Arc<Instrumentation>,
    ctx: crate::context::WeakContext,
}

impl RouteFactoryHook for Hook {
    fn before_create(&self, _def: &RouteDefinition, rc: &mut RouteContext) -> Result<()> {
        if self.inst.names.cfg.message_history {
            rc.channel_wrappers.push(Arc::new(HistoryWrapper { inst: self.inst.clone() }));
        }
        Ok(())
    }

    fn wrap_route(&self, def: &RouteDefinition, rc: &RouteContext, processor: Arc<dyn Processor>) -> Result<Arc<dyn Processor>> {
        let n = &self.inst.names;
        let cfg = &n.cfg;
        let route_id = rc.route_id.clone();
        let uri = rc.context.get_endpoint(&def.input).map(|e| e.uri().to_string()).unwrap_or_else(|_| def.input.clone());
        let excluded = cfg.route_policy_exclude.iter().any(|p| crate::util::matches_pattern(&route_id, p));
        let route = (cfg.route_policy && cfg.route_policy_level != PolicyLevel::Context && !excluded).then(|| Arc::new(PolicyMeters::new(n, &route_id, "route", "Route performance metrics")));
        let context = if excluded { None } else { self.inst.context_policy.clone() };
        let mut done = None;
        if cfg.exchange_event_notifier {
            let endpoint = endpoint_name(&uri, cfg.base_endpoint_uri);
            let (name, desc) = match cfg.flavor {
                Flavor::Micrometer => ("camel.exchange.event.notifier", "Time taken for exchange processing"),
                Flavor::OpenTelemetry => ("camel.exchange.elapsed", "Time taken to complete exchange"),
            };
            let mk = |event: &str, failed: &str| n.duration(name, &n.tags(&[("kind", "CamelExchangeEvent"), ("eventType", event), ("routeId", &route_id), ("endpointName", &endpoint), ("failed", failed)]), desc);
            done = Some([mk("ExchangeCompletedEvent", "false"), mk("ExchangeFailedEvent", "true")]);
            let w = self.ctx.clone();
            let r = route_id.clone();
            n.registry.fn_gauge(&n.n("camel.exchanges.inflight"), &n.tags(&[("kind", "CamelExchangeEvent"), ("routeId", &route_id)]), Some("Route inflight messages"), move || {
                w.upgrade().map(|c| c.inflight_route_count(&r) as f64).unwrap_or(0.0)
            });
        }
        if route.is_none() && context.is_none() && done.is_none() {
            return Ok(processor);
        }
        Ok(Arc::new(RouteMeters { route_id: Arc::from(route_id.as_str()), route, context, done, last: self.inst.last.clone(), inner: processor }))
    }
}

/// `endpointName`: the URI, without its options with `baseEndpointURI`.
fn endpoint_name(uri: &str, base: bool) -> String {
    if base {
        uri.split('?').next().unwrap_or(uri).to_string()
    } else {
        uri.to_string()
    }
}

struct RouteMeters {
    route_id: Arc<str>,
    route: Option<Arc<PolicyMeters>>,
    context: Option<Arc<PolicyMeters>>,
    /// The exchange event notifier's timers: [completed, failed].
    done: Option<[Rec; 2]>,
    last: Option<Arc<Gauge>>,
    inner: Arc<dyn Processor>,
}

impl RouteMeters {
    fn finish(&self, ex: &Exchange, start: u64, failed: bool) {
        let d = crate::clock::elapsed(start, crate::clock::ticks());
        if let Some(m) = &self.route {
            m.record(d, ex, failed);
        }
        // the context counts each exchange once: in the route it started in
        let root = ex.from_route_id.as_deref() == Some(&*self.route_id);
        if root {
            if let Some(m) = &self.context {
                m.record(d, ex, failed);
            }
            if let Some(t) = &self.done {
                t[failed as usize].record(d);
            }
            if let Some(g) = &self.last {
                g.set(crate::util_time::now_millis() as f64);
            }
        }
    }
}

impl Processor for RouteMeters {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let start = crate::clock::ticks();
        let r = self.inner.process(ex);
        self.finish(ex, start, r.is_err() || ex.is_failed());
        r
    }
    fn as_async(&self) -> Option<&dyn AsyncProcessor> {
        self.inner.as_async().map(|_| self as &dyn AsyncProcessor)
    }
    fn blocking(&self) -> Blocking {
        self.inner.blocking()
    }
}

impl AsyncProcessor for RouteMeters {
    fn process_async<'a>(&'a self, ex: &'a mut Exchange) -> ProcessFuture<'a> {
        Box::pin(async move {
            let inner = self.inner.as_async().expect("as_async checked the inner processor");
            let start = crate::clock::ticks();
            let r = inner.process_async(ex).await;
            self.finish(ex, start, r.is_err() || ex.is_failed());
            r
        })
    }
}

// ---------------------------------------------------------------------------
// sends (ExchangeSentEvent)
// ---------------------------------------------------------------------------

struct SentWrapper {
    inst: Arc<Instrumentation>,
}

impl ProducerWrapper for SentWrapper {
    fn wrap(&self, endpoint: &Arc<dyn Endpoint>, producer: Arc<dyn Producer>) -> Arc<dyn Producer> {
        let endpoint = endpoint_name(endpoint.uri(), self.inst.names.cfg.base_endpoint_uri);
        Arc::new(SentMeters { names: self.inst.names.clone(), endpoint, by_route: RwLock::new(HashMap::new()), inner: producer })
    }
}

struct SentMeters {
    names: Arc<Names>,
    endpoint: String,
    /// Timers per sending route: [ok, failed].
    by_route: RwLock<HashMap<String, Arc<[Rec; 2]>>>,
    inner: Arc<dyn Producer>,
}

impl SentMeters {
    fn timers(&self, route: &str) -> Arc<[Rec; 2]> {
        if let Some(t) = self.by_route.read().unwrap().get(route) {
            return t.clone();
        }
        let n = &self.names;
        let (name, desc) = match n.cfg.flavor {
            Flavor::Micrometer => ("camel.exchange.event.notifier", "Time taken to send message to the endpoint"),
            Flavor::OpenTelemetry => ("camel.exchange.sent", "Time taken to send message to the endpoint"),
        };
        let mk = |failed: &str| {
            let mut pairs = vec![("kind", "CamelExchangeEvent"), ("eventType", "ExchangeSentEvent"), ("endpointName", self.endpoint.as_str()), ("failed", failed)];
            if !(route.is_empty() && n.cfg.flavor == Flavor::OpenTelemetry) {
                pairs.push(("routeId", route));
            }
            n.duration(name, &n.tags(&pairs), desc)
        };
        let t = Arc::new([mk("false"), mk("true")]);
        self.by_route.write().unwrap().insert(route.to_string(), t.clone());
        t
    }
    fn record(&self, ex: &Exchange, start: u64, failed: bool) {
        let route = ex.from_route_id.as_deref().unwrap_or("");
        self.timers(route)[failed as usize].record(crate::clock::elapsed(start, crate::clock::ticks()));
    }
}

impl Processor for SentMeters {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let start = crate::clock::ticks();
        let r = self.inner.process(ex);
        self.record(ex, start, r.is_err() || ex.is_failed());
        r
    }
    fn as_async(&self) -> Option<&dyn AsyncProcessor> {
        self.inner.as_async().map(|_| self as &dyn AsyncProcessor)
    }
    fn blocking(&self) -> Blocking {
        self.inner.blocking()
    }
}

impl AsyncProcessor for SentMeters {
    fn process_async<'a>(&'a self, ex: &'a mut Exchange) -> ProcessFuture<'a> {
        Box::pin(async move {
            let inner = self.inner.as_async().expect("as_async checked the inner producer");
            let start = crate::clock::ticks();
            let r = inner.process_async(ex).await;
            self.record(ex, start, r.is_err() || ex.is_failed());
            r
        })
    }
}

impl Service for SentMeters {
    fn start(&self) -> Result<()> {
        self.inner.start()
    }
    fn stop(&self) -> Result<()> {
        self.inner.stop()
    }
    fn as_suspendable(&self) -> Option<&dyn SuspendableService> {
        self.inner.as_suspendable()
    }
}

// ---------------------------------------------------------------------------
// message history
// ---------------------------------------------------------------------------

struct HistoryWrapper {
    inst: Arc<Instrumentation>,
}

impl ChannelWrapper for HistoryWrapper {
    fn wrap(&self, rc: &RouteContext, def: &dyn ProcessorDefinition, processor: Arc<dyn Processor>) -> Result<Arc<dyn Processor>> {
        let n = &self.inst.names;
        let node = def.id().map(|s| s.to_string()).or_else(|| crate::management::node_id(rc, def)).unwrap_or_else(|| def.name().to_string());
        let rec = n.duration("camel.message.history", &n.tags(&[("kind", "CamelMessageHistory"), ("routeId", &rc.route_id), ("nodeId", &node)]), "Node performance metrics");
        Ok(Arc::new(HistoryMeter { rec, inner: processor }))
    }
}

struct HistoryMeter {
    rec: Rec,
    inner: Arc<dyn Processor>,
}

impl Processor for HistoryMeter {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let start = crate::clock::ticks();
        let r = self.inner.process(ex);
        self.rec.record(crate::clock::elapsed(start, crate::clock::ticks()));
        r
    }
    fn as_async(&self) -> Option<&dyn AsyncProcessor> {
        self.inner.as_async().map(|_| self as &dyn AsyncProcessor)
    }
    fn blocking(&self) -> Blocking {
        self.inner.blocking()
    }
}

impl AsyncProcessor for HistoryMeter {
    fn process_async<'a>(&'a self, ex: &'a mut Exchange) -> ProcessFuture<'a> {
        Box::pin(async move {
            let inner = self.inner.as_async().expect("as_async checked the inner processor");
            let start = crate::clock::ticks();
            let r = inner.process_async(ex).await;
            self.rec.record(crate::clock::elapsed(start, crate::clock::ticks()));
            r
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn legacy_names() {
        assert_eq!(super::legacy_name("camel.route.policy"), "CamelRoutePolicy");
        assert_eq!(super::legacy_name("camel.exchanges.succeeded"), "CamelExchangesSucceeded");
        assert_eq!(super::legacy_name("camel.exchanges.failures.handled"), "CamelExchangesFailuresHandled");
    }
}

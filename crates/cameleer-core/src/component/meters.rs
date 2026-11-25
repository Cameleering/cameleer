//! The metrics components, all on the context's [`MeterRegistry`]:
//!
//! - `micrometer:[counter|summary|timer]:name` (Camel `camel-micrometer`): options
//!   `metricsDescription`, `tags.<key>=<value>`, `action` (`start`/`stop`), `value`, `increment`,
//!   `decrement`, all Simple expressions; the tag `camelContext` is always added. Without a type
//!   the endpoint is a counter.
//! - `metrics:[gauge|counter|histogram|meter|timer]:name` (Camel `camel-metrics`, Dropwizard):
//!   options `action`, `mark`, `value`, `increment`, `decrement` (numbers) and `subject` (gauge).
//!   Without a type the endpoint is a meter. Dropwizard metrics have no tags.
//! - `opentelemetry-metrics:[counter|summary|timer]:name` (Camel 4.17 `camel-opentelemetry-metrics`):
//!   options `metricsDescription`, `attributes.<key>=<value>`, `action`, `unit` (of timers, default
//!   `MILLISECONDS`), `value`, `increment`, `decrement` (Simple, whole numbers). The counter is an
//!   up-down counter, summaries and timers are histograms with the OpenTelemetry SDK's bounds.
//!
//! Headers override the options: `CamelMetricsName`, `CamelMetricsDescription`,
//! `CamelMetricsTags` / `CamelMetricsAttributes` (an object or `k=v,k2=v2`),
//! `CamelMetricsCounterIncrement`, `CamelMetricsCounterDecrement`, `CamelMetricsHistogramValue`,
//! `CamelMetricsTimerAction`, `CamelMetricsMeterMark`, `CamelMetricsGaugeSubject`. Every
//! `CamelMetrics*` header is removed after the step. A timer keeps its start in the exchange
//! property `timer:<name>` between `start` and `stop`.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use crate::context::{CamelContext, WeakContext};
use crate::error::{kinds, CamelError, Result};
use crate::message::Exchange;
use crate::metrics::MeterRegistry;
use crate::spi::{Component, Endpoint, Expression, Processor, Producer, Service};
use crate::util::Parameters;
use crate::value::Value;

pub const HEADER_TIMER_ACTION: &str = "CamelMetricsTimerAction";
pub const HEADER_HISTOGRAM_VALUE: &str = "CamelMetricsHistogramValue";
pub const HEADER_COUNTER_DECREMENT: &str = "CamelMetricsCounterDecrement";
pub const HEADER_COUNTER_INCREMENT: &str = "CamelMetricsCounterIncrement";
pub const HEADER_METRIC_NAME: &str = "CamelMetricsName";
pub const HEADER_METRIC_DESCRIPTION: &str = "CamelMetricsDescription";
pub const HEADER_METRIC_TAGS: &str = "CamelMetricsTags";
pub const HEADER_METRIC_ATTRIBUTES: &str = "CamelMetricsAttributes";
pub const HEADER_METER_MARK: &str = "CamelMetricsMeterMark";
pub const HEADER_GAUGE_SUBJECT: &str = "CamelMetricsGaugeSubject";

/// Which Camel component an endpoint behaves as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    Micrometer,
    Dropwizard,
    OpenTelemetry,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Counter,
    Summary,
    Timer,
    Gauge,
    Meter,
}

/// The `micrometer`, `metrics` and `opentelemetry-metrics` components.
pub struct MetersComponent {
    flavor: Flavor,
}

impl MetersComponent {
    pub fn micrometer() -> Self {
        MetersComponent { flavor: Flavor::Micrometer }
    }
    pub fn dropwizard() -> Self {
        MetersComponent { flavor: Flavor::Dropwizard }
    }
    pub fn opentelemetry() -> Self {
        MetersComponent { flavor: Flavor::OpenTelemetry }
    }
}

fn kind_of(flavor: Flavor, t: &str) -> Result<Kind> {
    let bad = || CamelError::illegal_argument(format!("Unsupported meter type {}", t));
    match flavor {
        // exact and case-sensitive, as Camel's MicrometerUtils.getByName
        Flavor::Micrometer => match t {
            "counter" => Ok(Kind::Counter),
            "summary" => Ok(Kind::Summary),
            "timer" => Ok(Kind::Timer),
            _ => Err(bad()),
        },
        Flavor::Dropwizard => match t {
            "gauge" => Ok(Kind::Gauge),
            "counter" => Ok(Kind::Counter),
            "histogram" => Ok(Kind::Summary),
            "meter" => Ok(Kind::Meter),
            "timer" => Ok(Kind::Timer),
            _ => Err(CamelError::illegal_argument(format!("Unknown metrics type \"{}\"", t))),
        },
        Flavor::OpenTelemetry => match t.to_ascii_lowercase().as_str() {
            "counter" => Ok(Kind::Counter),
            "summary" => Ok(Kind::Summary),
            "timer" => Ok(Kind::Timer),
            _ => Err(CamelError::illegal_argument(format!("Unsupported instrument type {}", t))),
        },
    }
}

/// A value of an option: a Simple expression (micrometer, opentelemetry-metrics) or a literal
/// (metrics).
#[derive(Clone)]
enum Opt {
    Expr(Arc<dyn Expression>),
    Literal(String),
}

impl Opt {
    fn eval(&self, ex: &Exchange) -> Result<Option<Value>> {
        match self {
            Opt::Expr(e) => {
                let v = e.evaluate(ex)?;
                Ok((!v.is_null()).then_some(v))
            }
            Opt::Literal(s) => Ok(Some(Value::String(s.clone()))),
        }
    }
}

/// The time unit of an `opentelemetry-metrics` timer (Java `TimeUnit`).
fn time_unit(s: &str) -> Result<(String, f64)> {
    let per_sec = match s.to_ascii_uppercase().as_str() {
        "NANOSECONDS" => 1e9,
        "MICROSECONDS" => 1e6,
        "MILLISECONDS" => 1e3,
        "SECONDS" => 1.0,
        "MINUTES" => 1.0 / 60.0,
        "HOURS" => 1.0 / 3600.0,
        "DAYS" => 1.0 / 86400.0,
        _ => return Err(CamelError::illegal_argument(format!("Invalid unit {}: one of NANOSECONDS, MICROSECONDS, MILLISECONDS, SECONDS, MINUTES, HOURS, DAYS", s))),
    };
    Ok((s.to_ascii_lowercase(), per_sec))
}

impl Component for MetersComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let (kind, name) = match remaining.split_once(':') {
            Some((t, n)) => (kind_of(self.flavor, t)?, n.to_string()),
            None => (if self.flavor == Flavor::Dropwizard { Kind::Meter } else { Kind::Counter }, remaining.to_string()),
        };
        if name.is_empty() {
            return Err(CamelError::illegal_argument(format!("{}: a metric name is required", uri)));
        }
        let simple = |s: String| -> Result<Opt> {
            if self.flavor == Flavor::Dropwizard {
                Ok(Opt::Literal(s))
            } else {
                Ok(Opt::Expr(ctx.resolve_language("simple")?.create_expression(&s, &BTreeMap::new())?))
            }
        };
        let mut opt = |k: &str| -> Result<Option<Opt>> { params.take(k).map(&simple).transpose() };
        let description = opt("metricsDescription")?;
        let action = opt("action")?;
        let value = opt("value")?;
        let increment = opt("increment")?;
        let decrement = opt("decrement")?;
        let mark = opt("mark")?;
        let subject = params.take("subject");
        let unit = match (self.flavor, params.take("unit")) {
            (Flavor::OpenTelemetry, u) => Some(time_unit(u.as_deref().unwrap_or("MILLISECONDS"))?),
            (_, Some(u)) => return Err(CamelError::illegal_argument(format!("Unknown option unit={}", u))),
            _ => None,
        };
        let mut tags = Vec::new();
        let prefix = if self.flavor == Flavor::OpenTelemetry { "attributes." } else { "tags." };
        if self.flavor != Flavor::Dropwizard {
            for (k, v) in params.take_prefixed(prefix) {
                tags.push((simple(k)?, simple(v)?));
            }
            // the deprecated `tags=k1=v1,k2=v2` of camel-micrometer
            if let Some(t) = params.take("tags") {
                crate::camel_warn!("micrometer", "The tags=k=v,... option is deprecated: use tags.k=v");
                for (k, v) in crate::metrics::parse_tags(&t) {
                    tags.push((Opt::Literal(k), Opt::Literal(v)));
                }
            }
        }
        let name = if self.flavor == Flavor::Dropwizard { Opt::Literal(name) } else { simple(name)? };
        let ep = MetersEndpoint {
            uri: uri.to_string(),
            flavor: self.flavor,
            kind,
            name,
            description,
            tags,
            action,
            value,
            increment,
            decrement,
            mark,
            subject,
            unit,
            ctx: ctx.downgrade(),
        };
        Ok(Arc::new(ep))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct MetersEndpoint {
    uri: String,
    flavor: Flavor,
    kind: Kind,
    name: Opt,
    description: Option<Opt>,
    tags: Vec<(Opt, Opt)>,
    action: Option<Opt>,
    value: Option<Opt>,
    increment: Option<Opt>,
    decrement: Option<Opt>,
    mark: Option<Opt>,
    subject: Option<String>,
    /// opentelemetry-metrics timers: the unit's name and how many units are in a second.
    unit: Option<(String, f64)>,
    ctx: WeakContext,
}

impl Endpoint for MetersEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        let ctx = self.ctx.upgrade().ok_or_else(|| CamelError::illegal_state("CamelContext has been dropped"))?;
        let ep = MetersEndpoint {
            uri: self.uri.clone(),
            flavor: self.flavor,
            kind: self.kind,
            name: self.name.clone(),
            description: self.description.clone(),
            tags: self.tags.clone(),
            action: self.action.clone(),
            value: self.value.clone(),
            increment: self.increment.clone(),
            decrement: self.decrement.clone(),
            mark: self.mark.clone(),
            subject: self.subject.clone(),
            unit: self.unit.clone(),
            ctx: self.ctx.clone(),
        };
        let registry = MeterRegistry::of(&ctx);
        // a Dropwizard gauge with a subject exists from the start, as in Camel
        if let (Flavor::Dropwizard, Kind::Gauge, Some(s), Opt::Literal(n)) = (ep.flavor, ep.kind, &ep.subject, &ep.name) {
            if let Ok(v) = s.trim().parse::<f64>() {
                if let Ok(g) = registry.gauge(n, &[], None) {
                    g.set(v);
                }
            }
        }
        Ok(Arc::new(MetersProducer { ep, registry, context_name: ctx.name().to_string() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct MetersProducer {
    ep: MetersEndpoint,
    registry: Arc<MeterRegistry>,
    context_name: String,
}

impl Service for MetersProducer {}

fn header_str(ex: &Exchange, h: &str) -> Option<String> {
    ex.header(h).filter(|v| !v.is_null()).map(|v| v.to_string()).filter(|s| !s.is_empty())
}

fn tag_header(v: &Value) -> Vec<(String, String)> {
    match v {
        Value::Object(o) => o.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        Value::Array(a) => a.iter().flat_map(tag_header).collect(),
        Value::Null => Vec::new(),
        o => crate::metrics::parse_tags(&o.to_string()),
    }
}

fn number(v: &Value, what: &str, name: &str) -> Result<f64> {
    v.as_f64().ok_or_else(|| CamelError::new(kinds::TYPE_CONVERSION, format!("The {} ({}) of {} is not a number", what, v, name)))
}

impl MetersProducer {
    fn run(&self, ex: &mut Exchange) -> Result<()> {
        let ep = &self.ep;
        let name = match header_str(ex, HEADER_METRIC_NAME) {
            Some(n) => n,
            None => ep.name.eval(ex)?.map(|v| v.to_string()).unwrap_or_default(),
        };
        let description = match header_str(ex, HEADER_METRIC_DESCRIPTION) {
            Some(d) => Some(d),
            None => match &ep.description {
                Some(d) => d.eval(ex)?.map(|v| v.to_string()),
                None => None,
            },
        };
        let mut tags: Vec<(String, String)> = Vec::new();
        if ep.flavor != Flavor::Dropwizard {
            for (k, v) in &ep.tags {
                let k = k.eval(ex)?.map(|v| v.to_string()).unwrap_or_default();
                let v = v.eval(ex)?.map(|v| v.to_string()).unwrap_or_default();
                tags.push((k, v));
            }
            let header = if ep.flavor == Flavor::OpenTelemetry { HEADER_METRIC_ATTRIBUTES } else { HEADER_METRIC_TAGS };
            if let Some(h) = ex.header(header) {
                for (k, v) in tag_header(h) {
                    tags.retain(|(o, _)| *o != k);
                    tags.push((k, v));
                }
            }
            tags.retain(|(k, _)| k != "camelContext");
            tags.push(("camelContext".to_string(), self.context_name.clone()));
        }
        let desc = description.as_deref();
        let mismatch = |e: crate::metrics::KindMismatch| CamelError::illegal_argument(format!("Meter {} is already registered as a {}", e.0.name, e.1));
        let opt_or_header = |h: &str, o: &Option<Opt>| -> Result<Option<Value>> {
            match ex.header(h).filter(|v| !v.is_null()) {
                Some(v) => Ok(Some(v.clone())),
                None => match o {
                    Some(o) => o.eval(ex),
                    None => Ok(None),
                },
            }
        };
        match ep.kind {
            Kind::Counter => {
                let inc = opt_or_header(HEADER_COUNTER_INCREMENT, &ep.increment)?;
                let dec = opt_or_header(HEADER_COUNTER_DECREMENT, &ep.decrement)?;
                let delta = match (&inc, &dec) {
                    (Some(i), _) => number(i, "increment", &name)?,
                    (None, Some(d)) => -number(d, "decrement", &name)?,
                    (None, None) if ep.increment.is_some() || ep.decrement.is_some() => {
                        return Err(CamelError::illegal_argument(format!("The increment or decrement of counter {} did not evaluate to a number", name)));
                    }
                    (None, None) => 1.0,
                };
                match ep.flavor {
                    // Micrometer's Counter.increment(-dec): a monotonic counter ignores it
                    Flavor::Micrometer => self.registry.counter(&name, &tags, desc).map_err(mismatch)?.increment(delta),
                    Flavor::OpenTelemetry => self.registry.up_down_counter(&name, &tags, desc, "").map_err(mismatch)?.add(delta.trunc()),
                    Flavor::Dropwizard => self.registry.up_down_counter(&name, &tags, desc, "").map_err(mismatch)?.add(delta.trunc()),
                }
            }
            Kind::Summary => {
                let v = opt_or_header(HEADER_HISTOGRAM_VALUE, &ep.value)?;
                match v {
                    Some(v) => {
                        let v = number(&v, "value", &name)?;
                        match ep.flavor {
                            Flavor::OpenTelemetry => self.registry.histogram(&name, &tags, desc, "", crate::metrics::OTEL_BOUNDS).map_err(mismatch)?.record(v.trunc()),
                            Flavor::Dropwizard => self.registry.summary(&name, &tags, desc).map_err(mismatch)?.record(v.trunc()),
                            Flavor::Micrometer => self.registry.summary(&name, &tags, desc).map_err(mismatch)?.record(v),
                        }
                    }
                    None if ep.flavor == Flavor::Dropwizard => crate::camel_warn!("metrics", "Cannot update histogram {} with a null value", name),
                    None => {}
                }
            }
            Kind::Meter => {
                let m = opt_or_header(HEADER_METER_MARK, &ep.mark)?.map(|v| number(&v, "mark", &name)).transpose()?.unwrap_or(1.0);
                self.registry.meter(&name, &tags, desc).map_err(mismatch)?.mark(m.max(0.0) as u64);
            }
            Kind::Gauge => {
                let subject = ex.header(HEADER_GAUGE_SUBJECT).filter(|v| !v.is_null()).cloned().or_else(|| ep.subject.clone().map(Value::String));
                match subject.as_ref().and_then(|v| v.as_f64()) {
                    Some(v) => self.registry.gauge(&name, &tags, desc).map_err(mismatch)?.set(v),
                    None if subject.is_some() => crate::camel_warn!("metrics", "Gauge {}: the subject is not a number", name),
                    None => crate::camel_info!("metrics", "No subject found for Gauge \"{}\". Ignoring...", name),
                }
            }
            Kind::Timer => {
                let action = opt_or_header(HEADER_TIMER_ACTION, &ep.action)?.map(|v| v.to_string().trim().to_ascii_lowercase());
                let prop = format!("timer:{}", name);
                match action.as_deref() {
                    Some("start") => {
                        if ex.property(&prop).is_none() {
                            ex.set_property(prop, Value::Int(crate::clock::ticks() as i64));
                        } else if ep.flavor == Flavor::Dropwizard {
                            crate::camel_warn!("metrics", "Timer \"{}\" already running", name);
                        }
                    }
                    Some("stop") => match ex.properties.remove(&prop).and_then(|v| v.as_i64()) {
                        Some(start) => {
                            let d = crate::clock::elapsed(start as u64, crate::clock::ticks());
                            self.record_timer(&name, &tags, desc, d).map_err(mismatch)?;
                        }
                        None if ep.flavor == Flavor::Dropwizard => crate::camel_warn!("metrics", "Timer \"{}\" not found", name),
                        None => {}
                    },
                    Some(o) => return Err(CamelError::new(kinds::TYPE_CONVERSION, format!("Invalid timer action {} for {}: start or stop", o, name))),
                    None if ep.flavor == Flavor::Dropwizard => crate::camel_warn!("metrics", "No action provided for timer \"{}\"", name),
                    None => {}
                }
            }
        }
        Ok(())
    }

    fn record_timer(&self, name: &str, tags: &[(String, String)], desc: Option<&str>, d: Duration) -> std::result::Result<(), crate::metrics::KindMismatch> {
        match (&self.ep.flavor, &self.ep.unit) {
            (Flavor::OpenTelemetry, Some((unit, per_sec))) => {
                let h = self.registry.histogram(name, tags, desc, unit, crate::metrics::OTEL_BOUNDS)?;
                h.record((d.as_secs_f64() * per_sec).trunc());
            }
            _ => self.registry.timer(name, tags, desc)?.record(d),
        }
        Ok(())
    }
}

impl Processor for MetersProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let r = self.run(ex);
        ex.message.headers.remove_matching("CamelMetrics*", &[]);
        r
    }
}

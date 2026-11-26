//! Distributed tracing with OpenTelemetry semantics (Camel `camel-opentelemetry`, built on
//! `camel-tracing`).
//!
//! [`install`] adds to a context, before its routes are created:
//!
//! - a **route span** per exchange and route (Camel's `TracingRoutePolicy`): its parent is the
//!   span of the exchange when one is active (a `direct` or `seda` call from another route),
//!   otherwise the context extracted from the message headers (`traceparent`, `tracestate`,
//!   `baggage`, mapped from HTTP headers, Kafka record headers, JMS/AMQP properties or gRPC
//!   metadata by the consumer), otherwise a new trace starts;
//! - a **producer span** around every send to an endpoint (`to`, `toD`, `wireTap`, `enrich`,
//!   recipient lists, producer templates; Camel's `ExchangeSendingEvent`/`ExchangeSentEvent`),
//!   which injects its context into the message headers, so the producer carries it on the wire;
//! - with `traceProcessors`, an **internal span** per processor (Camel's
//!   `OpenTelemetryTracingStrategy`).
//!
//! Span names, kinds and attributes come from per-component decorators ([`decorator`]), as in
//! `camel-tracing`. Finished, sampled spans go to a [`SpanProcessor`] (the OTLP exporter of
//! `cameleer-opentelemetry`, or [`span::InMemorySpanProcessor`] in tests).
//!
//! Without [`install`] nothing of this runs: no hook, no wrapper, no per-exchange check.

pub mod context;
pub mod decorator;
pub mod span;

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::context::{CamelContext, RouteFactoryHook};
use crate::error::{CamelError, Result};
use crate::message::Exchange;
use crate::model::{ChannelWrapper, ProcessorDefinition, RouteContext, RouteDefinition};
use crate::spi::{AsyncProcessor, Blocking, Endpoint, ProcessFuture, Processor, Producer, Service, SuspendableService};
use crate::value::Value;

pub use context::{SpanContext, BAGGAGE, TRACEPARENT, TRACESTATE};
pub use span::{AttrValue, Attributes, SpanData, SpanEvent, SpanKind, SpanProcessor, StatusCode};

/// Exchange property holding the active span's context: 25 bytes, trace id (16), span id (8)
/// and trace flags (1).
pub const ACTIVE_SPAN: &str = "CamelOpenTelemetryActiveSpan";
/// Exchange property holding the active span's `tracestate` (absent when empty).
pub const ACTIVE_TRACESTATE: &str = "CamelOpenTelemetryActiveTraceState";
/// Exchange property holding the trace's baggage (a `baggage` header value; absent when empty).
pub const ACTIVE_BAGGAGE: &str = "CamelOpenTelemetryBaggage";
/// Exchange header with the active trace id, with `traceHeadersInclusion` (Camel's name).
pub const TRACE_ID_HEADER: &str = "CAMEL_TRACE_ID";
/// Exchange header with the active span id, with `traceHeadersInclusion` (Camel's name).
pub const SPAN_ID_HEADER: &str = "CAMEL_SPAN_ID";
/// Registry name of the installed [`Telemetry`].
pub const REGISTRY_NAME: &str = "cameleer.telemetry";

/// Head sampling (OpenTelemetry samplers, `OTEL_TRACES_SAMPLER`).
#[derive(Clone, Debug)]
pub enum Sampler {
    AlwaysOn,
    AlwaysOff,
    /// Samples a fraction of the traces, decided from the trace id.
    TraceIdRatio(f64),
    /// Follows the parent's sampled flag; a root span uses the inner sampler.
    ParentBased(Box<Sampler>),
}

impl Sampler {
    /// Whether a new span is sampled.
    pub fn should_sample(&self, parent: Option<&SpanContext>, trace_id: &[u8; 16]) -> bool {
        match self {
            Sampler::AlwaysOn => true,
            Sampler::AlwaysOff => false,
            Sampler::TraceIdRatio(r) => {
                if *r >= 1.0 {
                    return true;
                }
                // the lower 8 bytes of the trace id, as the Java SDK does
                let x = u64::from_be_bytes(trace_id[8..].try_into().unwrap()) >> 1;
                (x as f64) < r * (i64::MAX as f64)
            }
            Sampler::ParentBased(root) => match parent {
                Some(p) => p.is_sampled(),
                None => root.should_sample(None, trace_id),
            },
        }
    }
}

/// The settings of the tracer (`camel.opentelemetry.*` and the SDK's `OTEL_*` settings).
#[derive(Clone, Debug)]
pub struct TelemetryConfig {
    /// The instrumentation scope name (`camel.opentelemetry.instrumentationName`, default `camel`).
    pub instrumentation_name: String,
    /// Endpoint URIs, route ids or processor ids that are not traced
    /// (`camel.opentelemetry.excludePatterns`, comma separated, wildcards and regular
    /// expressions as everywhere in Camel).
    pub exclude_patterns: Vec<String>,
    /// Only these endpoint URIs, route ids or processor names are traced
    /// (`camel.opentelemetry2.includePatterns`, e.g. `log*,direct*,setBody*`; empty: all).
    pub include_patterns: Vec<String>,
    /// A span for every processor too (`camel.opentelemetry.traceProcessors`, default false).
    pub trace_processors: bool,
    /// With `trace_processors`, no spans for Camel's own DSL processors (`log`, `setBody`, ...),
    /// only for `process` and `bean` steps (`camel.opentelemetry2.disableCoreProcessors`).
    pub disable_core_processors: bool,
    /// Only routes and processors with an id given by the author are traced
    /// (`camel.opentelemetry2.traceCustomIdOnly`): routes with a generated id (`route1`) get no
    /// spans, nor do their sends; processor spans need `trace_processors` and a custom `.id()`.
    pub trace_custom_id_only: bool,
    /// Also write the active span's ids into the exchange headers `CAMEL_TRACE_ID` and
    /// `CAMEL_SPAN_ID` (`camel.opentelemetry2.traceHeadersInclusion`).
    pub trace_headers_inclusion: bool,
    /// Encode header keys for components that need it (`camel.opentelemetry.encoding`,
    /// default false): `-` becomes `_$dash$_` and `.` becomes `_$dot$_`, as Camel's JMS key
    /// format.
    pub encoding: bool,
    pub sampler: Sampler,
    /// Propagate `traceparent`/`tracestate` (the `tracecontext` propagator).
    pub propagate_trace_context: bool,
    /// Propagate `baggage` (the `baggage` propagator).
    pub propagate_baggage: bool,
    /// Most attributes per span (`OTEL_SPAN_ATTRIBUTE_COUNT_LIMIT`, default 128).
    pub attribute_count_limit: usize,
    /// Most events per span (`OTEL_SPAN_EVENT_COUNT_LIMIT`, default 128).
    pub event_count_limit: usize,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        TelemetryConfig {
            instrumentation_name: "camel".into(),
            exclude_patterns: Vec::new(),
            include_patterns: Vec::new(),
            trace_processors: false,
            disable_core_processors: false,
            trace_custom_id_only: false,
            trace_headers_inclusion: false,
            encoding: false,
            sampler: Sampler::ParentBased(Box::new(Sampler::AlwaysOn)),
            propagate_trace_context: true,
            propagate_baggage: true,
            attribute_count_limit: 128,
            event_count_limit: 128,
        }
    }
}

/// Span counters of a [`Telemetry`] (striped: threads do not share a cache line).
#[derive(Default)]
pub struct TelemetryStats {
    started: crate::stripe::Counter,
    ended: crate::stripe::Counter,
}

impl TelemetryStats {
    /// Spans started (sampled or not).
    pub fn started(&self) -> u64 {
        self.started.get()
    }
    /// Sampled spans handed to the span processor.
    pub fn ended(&self) -> u64 {
        self.ended.get()
    }
}

/// The installed tracer of a context.
pub struct Telemetry {
    cfg: TelemetryConfig,
    processor: Arc<dyn SpanProcessor>,
    scope: Arc<str>,
    stats: TelemetryStats,
}

/// Whether [`install`] ran in this process (log lines look up the current span only then).
static INSTALLED: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// The span the code on this thread runs in: (trace id, span id, sampled).
    static CURRENT: Cell<Option<([u8; 16], [u8; 8])>> = const { Cell::new(None) };
}

/// The trace and span id of the span the current thread runs in (for log lines), if any.
#[inline]
pub fn current_ids() -> Option<([u8; 16], [u8; 8])> {
    if !INSTALLED.load(Ordering::Relaxed) {
        return None;
    }
    CURRENT.try_with(|c| c.get()).ok().flatten()
}

/// Runs `f` with `ids` as the current span of the thread.
pub fn with_current<R>(ids: Option<([u8; 16], [u8; 8])>, f: impl FnOnce() -> R) -> R {
    let prev = CURRENT.with(|c| c.replace(ids));
    struct Restore(Option<([u8; 16], [u8; 8])>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = CURRENT.try_with(|c| c.set(self.0));
        }
    }
    let _r = Restore(prev);
    f()
}

/// A future that is the current span of its thread whenever it is polled.
struct InSpan<F> {
    ids: Option<([u8; 16], [u8; 8])>,
    inner: F,
}

impl<F: std::future::Future + Unpin> std::future::Future for InSpan<F> {
    type Output = F::Output;
    fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<F::Output> {
        let ids = self.ids;
        with_current(ids, || std::pin::Pin::new(&mut self.inner).poll(cx))
    }
}

/// Installs the tracer in `ctx`. Call it before the routes are created (before
/// `ctx.start()`); routes created earlier are not traced.
pub fn install(ctx: &CamelContext, cfg: TelemetryConfig, processor: Arc<dyn SpanProcessor>) -> Arc<Telemetry> {
    let scope: Arc<str> = Arc::from(cfg.instrumentation_name.as_str());
    let t = Arc::new(Telemetry { cfg, processor, scope, stats: TelemetryStats::default() });
    INSTALLED.store(true, Ordering::Relaxed);
    ctx.registry().bind_arc(REGISTRY_NAME, t.clone());
    ctx.add_route_factory_hook(Arc::new(TelemetryHook { t: t.clone() }));
    ctx.add_producer_wrapper(Arc::new(TelemetryProducers { t: t.clone() }));
    t
}

/// The tracer installed in `ctx`, if any.
pub fn telemetry(ctx: &CamelContext) -> Option<Arc<Telemetry>> {
    ctx.registry().lookup::<Telemetry>(REGISTRY_NAME)
}

/// What a decorator says about a span before it starts.
pub struct SpanStart {
    pub name: String,
    pub kind: SpanKind,
    pub attributes: Attributes,
}

/// A started span; [`Telemetry::end`] finishes it.
pub struct OpenSpan {
    pub context: SpanContext,
    parent: Option<SpanContext>,
    /// Whether the parent was the exchange's active span (restored when this one ends).
    parent_active: bool,
    sampled: bool,
    name: String,
    kind: SpanKind,
    start_unix_nano: u64,
    /// The tick clock at the start (cheaper than `Instant` on every platform).
    started: u64,
    attributes: Attributes,
    shared: Arc<Attributes>,
}

fn active_bytes(c: &SpanContext) -> Value {
    let mut b = Vec::with_capacity(25);
    b.extend_from_slice(&c.trace_id);
    b.extend_from_slice(&c.span_id);
    b.push(c.flags);
    Value::Bytes(b)
}

impl OpenSpan {
    pub fn ids(&self) -> Option<([u8; 16], [u8; 8])> {
        Some((self.context.trace_id, self.context.span_id))
    }
    pub fn set_attribute(&mut self, k: impl Into<String>, v: impl Into<AttrValue>) {
        let k = k.into();
        let v = v.into();
        match self.attributes.iter_mut().find(|(o, _)| *o == k) {
            Some(slot) => slot.1 = v,
            None => self.attributes.push((k, v)),
        }
    }
}

/// Where a new span's parent comes from.
pub enum Parent {
    /// The exchange's active span, else the message headers (route spans).
    ExchangeOrHeaders,
    /// The exchange's active span (producer and processor spans).
    Exchange,
}

fn now_unix_nano() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0)
}

fn header_str(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        // Kafka record headers and other binary carriers arrive as bytes
        Value::Bytes(b) => String::from_utf8(b.clone()).ok(),
        Value::Array(a) => a.first().and_then(header_str),
        Value::Null => None,
        o => Some(o.to_string()),
    }
}

/// Encodes a header key as Camel's JMS key format (`-` as `_$dash$_`, `.` as `_$dot$_`).
pub fn encode_key(k: &str) -> String {
    k.replace('-', "_$dash$_").replace('.', "_$dot$_")
}

/// Decodes [`encode_key`].
pub fn decode_key(k: &str) -> String {
    k.replace("_$dash$_", "-").replace("_$dot$_", ".")
}

impl Telemetry {
    pub fn config(&self) -> &TelemetryConfig {
        &self.cfg
    }
    pub fn stats(&self) -> &TelemetryStats {
        &self.stats
    }
    pub fn processor(&self) -> &Arc<dyn SpanProcessor> {
        &self.processor
    }

    /// Whether an endpoint URI, route id or processor id is excluded.
    pub fn is_excluded(&self, id: &str) -> bool {
        Self::matches_any(&self.cfg.exclude_patterns, id)
    }

    /// Whether any of `ids` (an endpoint URI, a route id, a processor name) is not to be traced:
    /// one matches an exclude pattern, or include patterns are set and none matches.
    pub fn is_skipped(&self, ids: &[&str]) -> bool {
        if ids.iter().any(|id| self.is_excluded(id)) {
            return true;
        }
        !self.cfg.include_patterns.is_empty() && !ids.iter().any(|id| Self::matches_any(&self.cfg.include_patterns, id))
    }

    fn matches_any(patterns: &[String], id: &str) -> bool {
        if patterns.is_empty() {
            return false;
        }
        // as Camel's `EndpointHelper.matchEndpoint`: the uri as given, normalized, and with
        // `://` written as `:` (so `mock:quiet*` matches `mock://quiet`)
        let normalized = crate::util::normalize_uri(id).ok();
        let short = id.replacen("://", ":", 1);
        patterns.iter().any(|p| {
            crate::util::matches_pattern(id, p) || crate::util::matches_pattern(&short, p) || normalized.as_deref().map(|n| crate::util::matches_pattern(n, p)).unwrap_or(false)
        })
    }

    /// Whether a route id was given by the author (not generated as `route<N>`).
    pub fn is_custom_route_id(id: &str) -> bool {
        !(id.len() > 5 && id.starts_with("route") && id[5..].bytes().all(|b| b.is_ascii_digit()))
    }

    /// With `traceCustomIdOnly`, whether the exchange is in a route that is not traced.
    fn in_untraced_route(&self, ex: &Exchange) -> bool {
        self.cfg.trace_custom_id_only && ex.from_route_id.as_deref().map(|r| !Self::is_custom_route_id(r)).unwrap_or(false)
    }

    fn key<'a>(&self, k: &'a str) -> std::borrow::Cow<'a, str> {
        if self.cfg.encoding {
            std::borrow::Cow::Owned(encode_key(k))
        } else {
            std::borrow::Cow::Borrowed(k)
        }
    }

    fn get_header(&self, ex: &Exchange, k: &str) -> Option<String> {
        ex.header(&self.key(k)).or_else(|| ex.header(k)).and_then(header_str)
    }

    /// The context extracted from the message headers (encoded keys when `encoding` is on).
    pub fn extract(&self, ex: &Exchange) -> Option<SpanContext> {
        if !self.cfg.propagate_trace_context {
            return None;
        }
        let mut c = SpanContext::parse_traceparent(&self.get_header(ex, TRACEPARENT)?)?;
        if let Some(ts) = self.get_header(ex, TRACESTATE).and_then(|t| context::parse_tracestate(&t)) {
            c.trace_state = ts;
        }
        Some(c)
    }

    /// The baggage of the message headers, validated and re-encoded.
    pub fn extract_baggage(&self, ex: &Exchange) -> Option<String> {
        if !self.cfg.propagate_baggage {
            return None;
        }
        self.get_header(ex, BAGGAGE).map(|b| context::format_baggage(&context::parse_baggage(&b))).filter(|b| !b.is_empty())
    }

    /// The active span of the exchange.
    pub fn active(ex: &Exchange) -> Option<SpanContext> {
        let Value::Bytes(b) = ex.property(ACTIVE_SPAN)? else { return None };
        if b.len() != 25 {
            return None;
        }
        let mut c = SpanContext { trace_id: b[..16].try_into().ok()?, span_id: b[16..24].try_into().ok()?, flags: b[24], trace_state: String::new(), remote: false };
        if let Some(ts) = ex.property(ACTIVE_TRACESTATE).and_then(|v| v.as_str()) {
            c.trace_state = ts.to_string();
        }
        Some(c)
    }

    /// Starts a span from everything known up front (tests and custom spans).
    pub fn start(&self, ex: &mut Exchange, parent: Parent, start: SpanStart) -> OpenSpan {
        let SpanStart { name, kind, attributes } = start;
        self.start_with(ex, parent, Arc::default(), move |_| (name, kind, attributes))
    }

    /// Starts a span and makes it the exchange's active span. `describe` gives the name, kind
    /// and per-exchange attributes; it runs only for a sampled span.
    pub fn start_with(&self, ex: &mut Exchange, parent: Parent, shared: Arc<Attributes>, describe: impl FnOnce(&Exchange) -> (String, SpanKind, Attributes)) -> OpenSpan {
        self.stats.started.add(1);
        let mut baggage = None;
        let parent_ctx = match parent {
            Parent::Exchange => Self::active(ex),
            Parent::ExchangeOrHeaders => match Self::active(ex) {
                Some(c) => Some(c),
                None => {
                    if ex.property(ACTIVE_BAGGAGE).is_none() {
                        baggage = self.extract_baggage(ex);
                    }
                    self.extract(ex)
                }
            },
        };
        let parent_active = parent_ctx.as_ref().map(|p| !p.remote).unwrap_or(false);
        let trace_id = parent_ctx.as_ref().map(|p| p.trace_id).unwrap_or_else(context::new_trace_id);
        let sampled = self.cfg.sampler.should_sample(parent_ctx.as_ref(), &trace_id);
        let context = SpanContext {
            trace_id,
            span_id: context::new_span_id(),
            flags: (parent_ctx.as_ref().map(|p| p.flags).unwrap_or(0) & !1) | sampled as u8,
            trace_state: parent_ctx.as_ref().map(|p| p.trace_state.clone()).unwrap_or_default(),
            remote: false,
        };
        ex.set_property(ACTIVE_SPAN, active_bytes(&context));
        // a child keeps its parent's trace state; only a span from headers changes it
        if !parent_active {
            if context.trace_state.is_empty() {
                ex.properties.remove(ACTIVE_TRACESTATE);
            } else {
                ex.set_property(ACTIVE_TRACESTATE, context.trace_state.clone());
            }
        }
        if let Some(b) = baggage {
            ex.set_property(ACTIVE_BAGGAGE, b);
        }
        if self.cfg.trace_headers_inclusion {
            ex.set_header(TRACE_ID_HEADER, context.trace_id_hex());
            ex.set_header(SPAN_ID_HEADER, context.span_id_hex());
        }
        let (name, kind, attributes) = if sampled { describe(ex) } else { (String::new(), SpanKind::Internal, Vec::new()) };
        OpenSpan {
            context,
            parent: parent_ctx,
            parent_active,
            sampled,
            name,
            kind,
            start_unix_nano: if sampled { now_unix_nano() } else { 0 },
            started: if sampled { crate::clock::ticks() } else { 0 },
            attributes,
            shared,
        }
    }

    /// Writes the span's context into the message headers (`traceparent`, `tracestate`,
    /// `baggage`), so the producer sends it.
    pub fn inject(&self, ex: &mut Exchange, span: &OpenSpan) {
        if self.cfg.propagate_trace_context {
            ex.set_header(self.key(TRACEPARENT), span.context.traceparent());
            if span.context.trace_state.is_empty() {
                ex.message.headers.remove(&self.key(TRACESTATE));
            } else {
                ex.set_header(self.key(TRACESTATE), span.context.trace_state.clone());
            }
        }
        if self.cfg.propagate_baggage {
            if let Some(b) = ex.property(ACTIVE_BAGGAGE).and_then(|v| v.as_str()).map(str::to_string) {
                ex.set_header(self.key(BAGGAGE), b);
            }
        }
    }

    /// Finishes a span: records the failure of the exchange (status `ERROR`, attribute
    /// `error`, an `exception` event), restores the previous active span and hands the span
    /// to the span processor.
    pub fn end(&self, ex: &mut Exchange, mut span: OpenSpan, error: Option<&CamelError>) {
        if self.cfg.trace_headers_inclusion {
            if let (Some(p), true) = (&span.parent, span.parent_active) {
                ex.set_header(SPAN_ID_HEADER, p.span_id_hex());
            }
        }
        match (&span.parent, span.parent_active) {
            (Some(p), true) => ex.set_property(ACTIVE_SPAN, active_bytes(p)),
            _ => {
                ex.properties.remove(ACTIVE_SPAN);
                ex.properties.remove(ACTIVE_TRACESTATE);
            }
        }
        if !span.sampled {
            return;
        }
        let end = span.start_unix_nano + crate::clock::ticks_to_nanos(crate::clock::ticks().saturating_sub(span.started));
        let error = error.or(ex.exception.as_ref());
        let mut events = Vec::new();
        let (status, status_message) = match error {
            Some(e) => {
                // as Camel's decorators: attribute `error`, an `error` event, status ERROR
                span.set_attribute("error", true);
                if self.cfg.event_count_limit > 0 {
                    events.push(SpanEvent {
                        name: "error".into(),
                        time_unix_nano: end,
                        attributes: vec![
                            ("error.kind".into(), AttrValue::from("Exception")),
                            ("message".into(), AttrValue::String(e.message.clone())),
                            ("exception.type".into(), AttrValue::String(e.kind.clone())),
                        ],
                    });
                }
                (StatusCode::Error, e.message.clone())
            }
            None => (StatusCode::Unset, String::new()),
        };
        let limit = self.cfg.attribute_count_limit.saturating_sub(span.shared.len());
        let dropped = span.attributes.len().saturating_sub(limit) as u32;
        span.attributes.truncate(limit);
        let parent = span.parent.as_ref();
        self.stats.ended.add(1);
        self.processor.on_end(SpanData {
            trace_id: span.context.trace_id,
            span_id: span.context.span_id,
            parent_span_id: parent.map(|p| p.span_id).unwrap_or([0; 8]),
            parent_remote: parent.map(|p| p.remote).unwrap_or(false),
            trace_state: span.context.trace_state,
            flags: span.context.flags,
            name: span.name,
            kind: span.kind,
            start_unix_nano: span.start_unix_nano,
            end_unix_nano: end,
            attributes: span.attributes,
            shared_attributes: span.shared,
            dropped_attributes: dropped,
            events,
            status,
            status_message,
            scope_name: self.scope.clone(),
        });
    }

    /// Flushes the span processor.
    pub fn force_flush(&self, timeout: Duration) -> bool {
        self.processor.force_flush(timeout)
    }

    /// Flushes and stops the span processor.
    pub fn shutdown(&self, timeout: Duration) {
        self.processor.shutdown(timeout)
    }
}

// ---------------------------------------------------------------------------
// route spans
// ---------------------------------------------------------------------------

struct TelemetryHook {
    t: Arc<Telemetry>,
}

impl RouteFactoryHook for TelemetryHook {
    fn before_create(&self, _def: &RouteDefinition, rc: &mut RouteContext) -> Result<()> {
        if self.t.cfg.trace_processors {
            rc.channel_wrappers.push(Arc::new(ProcessorSpans { t: self.t.clone() }));
        }
        Ok(())
    }

    fn wrap_route(&self, def: &RouteDefinition, rc: &RouteContext, processor: Arc<dyn Processor>) -> Result<Arc<dyn Processor>> {
        let uri = rc.context.get_endpoint(&def.input).map(|e| e.uri().to_string()).unwrap_or_else(|_| def.input.clone());
        if self.t.is_skipped(&[&uri, &rc.route_id]) || (self.t.cfg.trace_custom_id_only && !Telemetry::is_custom_route_id(&rc.route_id)) {
            return Ok(processor);
        }
        let d = decorator::prepare(&uri, decorator::Role::Consumer, Some(&rc.route_id));
        Ok(Arc::new(RouteSpan { t: self.t.clone(), d, inner: processor }))
    }
}

struct RouteSpan {
    t: Arc<Telemetry>,
    d: decorator::Prepared,
    inner: Arc<dyn Processor>,
}

impl RouteSpan {
    fn open(&self, ex: &mut Exchange) -> OpenSpan {
        self.t.start_with(ex, Parent::ExchangeOrHeaders, self.d.shared.clone(), |ex| decorator::dynamic(&self.d, ex))
    }
    fn close(&self, ex: &mut Exchange, mut span: OpenSpan, r: &Result<()>) {
        if span.sampled {
            decorator::finish(&self.d, ex, &mut span);
        }
        let err = r.as_ref().err();
        self.t.end(ex, span, err);
    }
}

impl Processor for RouteSpan {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let span = self.open(ex);
        let r = with_current(span.ids(), || self.inner.process(ex));
        self.close(ex, span, &r);
        r
    }
    fn as_async(&self) -> Option<&dyn AsyncProcessor> {
        self.inner.as_async().map(|_| self as &dyn AsyncProcessor)
    }
    fn blocking(&self) -> Blocking {
        self.inner.blocking()
    }
}

impl AsyncProcessor for RouteSpan {
    fn process_async<'a>(&'a self, ex: &'a mut Exchange) -> ProcessFuture<'a> {
        Box::pin(async move {
            let inner = self.inner.as_async().expect("as_async checked the inner processor");
            let span = self.open(ex);
            let ids = span.ids();
            let r = InSpan { ids, inner: inner.process_async(ex) }.await;
            self.close(ex, span, &r);
            r
        })
    }
}

// ---------------------------------------------------------------------------
// producer spans
// ---------------------------------------------------------------------------

/// Wraps the producers the context creates (see [`CamelContext::add_producer_wrapper`]).
pub trait ProducerWrapper: Send + Sync {
    fn wrap(&self, endpoint: &Arc<dyn Endpoint>, producer: Arc<dyn Producer>) -> Arc<dyn Producer>;
}

struct TelemetryProducers {
    t: Arc<Telemetry>,
}

impl ProducerWrapper for TelemetryProducers {
    fn wrap(&self, endpoint: &Arc<dyn Endpoint>, producer: Arc<dyn Producer>) -> Arc<dyn Producer> {
        let uri = endpoint.uri().to_string();
        if self.t.is_skipped(&[&uri]) {
            return producer;
        }
        let d = decorator::prepare(&uri, decorator::Role::Producer, None);
        Arc::new(ProducerSpan { t: self.t.clone(), d, inner: producer })
    }
}

struct ProducerSpan {
    t: Arc<Telemetry>,
    d: decorator::Prepared,
    inner: Arc<dyn Producer>,
}

impl ProducerSpan {
    fn open(&self, ex: &mut Exchange) -> OpenSpan {
        let span = self.t.start_with(ex, Parent::Exchange, self.d.shared.clone(), |ex| {
            let (name, kind, mut attrs) = decorator::dynamic(&self.d, ex);
            if let Some(r) = &ex.from_route_id {
                attrs.push(("camel.route.id".into(), AttrValue::String(r.to_string())));
            }
            (name, kind, attrs)
        });
        self.t.inject(ex, &span);
        span
    }
    fn close(&self, ex: &mut Exchange, mut span: OpenSpan, r: &Result<()>) {
        if span.sampled {
            decorator::finish(&self.d, ex, &mut span);
        }
        self.t.end(ex, span, r.as_ref().err());
    }
}

impl Processor for ProducerSpan {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        if self.t.in_untraced_route(ex) {
            return self.inner.process(ex);
        }
        let span = self.open(ex);
        let r = with_current(span.ids(), || self.inner.process(ex));
        self.close(ex, span, &r);
        r
    }
    fn as_async(&self) -> Option<&dyn AsyncProcessor> {
        self.inner.as_async().map(|_| self as &dyn AsyncProcessor)
    }
    fn blocking(&self) -> Blocking {
        self.inner.blocking()
    }
}

impl AsyncProcessor for ProducerSpan {
    fn process_async<'a>(&'a self, ex: &'a mut Exchange) -> ProcessFuture<'a> {
        Box::pin(async move {
            let inner = self.inner.as_async().expect("as_async checked the inner producer");
            if self.t.in_untraced_route(ex) {
                return inner.process_async(ex).await;
            }
            let span = self.open(ex);
            let r = InSpan { ids: span.ids(), inner: inner.process_async(ex) }.await;
            self.close(ex, span, &r);
            r
        })
    }
}

impl Service for ProducerSpan {
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
// processor spans (traceProcessors)
// ---------------------------------------------------------------------------

struct ProcessorSpans {
    t: Arc<Telemetry>,
}

impl ChannelWrapper for ProcessorSpans {
    fn wrap(&self, rc: &RouteContext, def: &dyn ProcessorDefinition, processor: Arc<dyn Processor>) -> Result<Arc<dyn Processor>> {
        // Camel's processor span name: `<id>-<shortName>`, e.g. `log1-log`
        let custom = def.id().is_some();
        let id = def.id().map(|s| s.to_string()).or_else(|| crate::management::node_id(rc, def)).unwrap_or_else(|| def.name().to_string());
        let custom_step = matches!(def.name(), "process" | "bean");
        if self.t.is_skipped(&[&id, &format!("{}-{}", id, def.name())])
            || (self.t.cfg.disable_core_processors && !custom_step)
            || (self.t.cfg.trace_custom_id_only && (!custom || !Telemetry::is_custom_route_id(&rc.route_id)))
        {
            return Ok(processor);
        }
        let shared = Arc::new(vec![
            ("camel.processor.id".into(), AttrValue::String(id.clone())),
            ("camel.route.id".into(), AttrValue::String(rc.route_id.clone())),
            ("op".into(), AttrValue::from("EVENT_PROCESS")),
        ]);
        Ok(Arc::new(ProcessorSpan { t: self.t.clone(), name: format!("{}-{}", id, def.name()), shared, inner: processor }))
    }
}

struct ProcessorSpan {
    t: Arc<Telemetry>,
    /// `<id>-<shortName>`
    name: String,
    shared: Arc<Attributes>,
    inner: Arc<dyn Processor>,
}

impl ProcessorSpan {
    fn open(&self, ex: &mut Exchange) -> OpenSpan {
        self.t.start_with(ex, Parent::Exchange, self.shared.clone(), |ex| (self.name.clone(), SpanKind::Internal, vec![("exchangeId".into(), AttrValue::String(ex.exchange_id.to_string()))]))
    }
}

impl Processor for ProcessorSpan {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let span = self.open(ex);
        let r = with_current(span.ids(), || self.inner.process(ex));
        self.t.end(ex, span, r.as_ref().err());
        r
    }
    fn as_async(&self) -> Option<&dyn AsyncProcessor> {
        self.inner.as_async().map(|_| self as &dyn AsyncProcessor)
    }
    fn blocking(&self) -> Blocking {
        self.inner.blocking()
    }
}

impl AsyncProcessor for ProcessorSpan {
    fn process_async<'a>(&'a self, ex: &'a mut Exchange) -> ProcessFuture<'a> {
        Box::pin(async move {
            let inner = self.inner.as_async().expect("as_async checked the inner processor");
            let span = self.open(ex);
            let r = InSpan { ids: span.ids(), inner: inner.process_async(ex) }.await;
            self.t.end(ex, span, r.as_ref().err());
            r
        })
    }
}

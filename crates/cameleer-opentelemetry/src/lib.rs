//! # Cameleer OpenTelemetry
//!
//! Distributed tracing for Cameleer routes, as Camel's `camel-opentelemetry2` (and the older
//! `camel-opentelemetry`): the tracer itself (route, producer and processor spans, W3C Trace
//! Context and Baggage propagation through the message headers) is in
//! [`cameleer_core::telemetry`]; this crate exports the spans over **OTLP/HTTP with protobuf**
//! to an OpenTelemetry Collector or any OTLP backend (Jaeger, Tempo, Honeycomb, Grafana Cloud,
//! Datadog's OTLP intake...).
//!
//! - [`proto`]: the OTLP protobuf encoding (and a decoder for tests);
//! - [`exporter`]: the OTLP/HTTP exporter (gzip, headers, TLS with full verification and
//!   optional client certificates, retries of 429/502/503/504 with backoff and `Retry-After`);
//! - [`batch`]: the batch span processor (bounded queue, drop and count when full);
//! - [`config`]: the `OTEL_*` environment variables;
//! - [`testing`]: an embedded OTLP receiver.
//!
//! [`install`] reads `camel.opentelemetry.*` and the `OTEL_*` settings and installs the tracer
//! in a context; `cameleer run` does it when `camel.opentelemetry.enabled=true`.

use std::sync::Arc;
use std::time::Duration;

use cameleer_core::context::CamelContext;
use cameleer_core::error::{CamelError, Result};
use cameleer_core::telemetry::{self, SpanData, SpanProcessor, Telemetry, TelemetryConfig};

pub mod batch;
pub mod config;
pub mod exporter;
pub mod metrics;
pub mod proto;
pub mod testing;

pub use batch::{BatchConfig, BatchSpanProcessor, BatchStats};
pub use config::{ExporterKind, OtlpConfig};
pub use exporter::{ExportResult, OtlpHttpExporter, SpanExporter};

/// Camel's options (`camel.opentelemetry.*` and `camel.opentelemetry2.*`), in Camel's spelling.
pub const OPTIONS: &[&str] = &[
    "enabled",
    "instrumentationName",
    "encoding",
    "excludePatterns",
    "traceProcessors",
    "includePatterns",
    "disableCoreProcessors",
    "traceCustomIdOnly",
    "traceHeadersInclusion",
];

/// Drops every span (`OTEL_TRACES_EXPORTER=none`): spans are still created, so the context
/// still propagates and log lines still carry ids.
struct Discard;
impl SpanProcessor for Discard {
    fn on_end(&self, _span: SpanData) {}
}

/// The installed tracer and its exporter.
pub struct OpenTelemetry {
    pub telemetry: Arc<Telemetry>,
    pub batch: Option<Arc<BatchSpanProcessor>>,
    pub config: OtlpConfig,
}

impl OpenTelemetry {
    /// Exports what is queued, waiting at most `timeout`.
    pub fn force_flush(&self, timeout: Duration) -> bool {
        self.telemetry.force_flush(timeout)
    }
    /// Flushes and stops the exporter.
    pub fn shutdown(&self, timeout: Duration) {
        self.telemetry.shutdown(timeout)
    }
}

/// An option of `camel.opentelemetry.*` or, when that is not set, of `camel.opentelemetry2.*`
/// (Camel Main gives the first one precedence too), in camelCase or kebab-case.
fn camel_option(props: &dyn Fn(&str) -> Option<String>, name: &str) -> Option<String> {
    let kebab: String = name.chars().flat_map(|c| if c.is_ascii_uppercase() { vec!['-', c.to_ascii_lowercase()] } else { vec![c] }).collect();
    ["camel.opentelemetry", "camel.opentelemetry2"]
        .iter()
        .find_map(|prefix| props(&format!("{}.{}", prefix, name)).or_else(|| props(&format!("{}.{}", prefix, kebab))))
        .map(|v| v.trim().to_string())
}

fn flag(v: Option<String>, name: &str) -> Result<Option<bool>> {
    match v.as_deref().map(str::to_ascii_lowercase).as_deref() {
        None => Ok(None),
        Some("true") => Ok(Some(true)),
        Some("false") => Ok(Some(false)),
        Some(o) => Err(CamelError::illegal_argument(format!("camel.opentelemetry.{} must be true or false: {}", name, o))),
    }
}

/// Whether the properties turn tracing on: `camel.opentelemetry.enabled=true`, or (as in Camel
/// Main) any other `camel.opentelemetry.*` option set without `enabled=false`.
pub fn is_enabled(props: &dyn Fn(&str) -> Option<String>) -> Result<bool> {
    if let Some(e) = flag(camel_option(props, "enabled"), "enabled")? {
        return Ok(e);
    }
    Ok(OPTIONS[1..].iter().any(|o| camel_option(props, o).is_some()))
}

/// The tracer settings from `camel.opentelemetry.*` and `OTEL_*`.
pub fn telemetry_config(props: &dyn Fn(&str) -> Option<String>) -> Result<TelemetryConfig> {
    let get = config::lookup_with(props);
    let mut c = TelemetryConfig::default();
    if let Some(n) = camel_option(props, "instrumentationName").filter(|n| !n.is_empty()) {
        c.instrumentation_name = n;
    }
    let list = |p: String| -> Vec<String> { p.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect() };
    if let Some(p) = camel_option(props, "excludePatterns") {
        c.exclude_patterns = list(p);
    }
    if let Some(p) = camel_option(props, "includePatterns") {
        c.include_patterns = list(p);
    }
    c.trace_processors = flag(camel_option(props, "traceProcessors"), "traceProcessors")?.unwrap_or(false);
    c.disable_core_processors = flag(camel_option(props, "disableCoreProcessors"), "disableCoreProcessors")?.unwrap_or(false);
    c.trace_custom_id_only = flag(camel_option(props, "traceCustomIdOnly"), "traceCustomIdOnly")?.unwrap_or(false);
    c.trace_headers_inclusion = flag(camel_option(props, "traceHeadersInclusion"), "traceHeadersInclusion")?.unwrap_or(false);
    c.encoding = flag(camel_option(props, "encoding"), "encoding")?.unwrap_or(false);
    c.sampler = config::sampler(&get)?;
    (c.propagate_trace_context, c.propagate_baggage) = config::propagators(&get)?;
    let limit = |specific: &str, default: usize| -> Result<usize> {
        match get(specific).or_else(|| get("otel.attribute.count.limit").filter(|_| specific.contains("attribute"))) {
            None => Ok(default),
            Some(v) => v.parse().map_err(|_| CamelError::illegal_argument(format!("{} must be a number: {}", config::env_name(specific), v))),
        }
    };
    c.attribute_count_limit = limit("otel.span.attribute.count.limit", 128)?;
    c.event_count_limit = limit("otel.span.event.count.limit", 128)?;
    Ok(c)
}

/// Installs tracing in `ctx` when the properties enable it and `OTEL_SDK_DISABLED` is not
/// `true`. Call before the routes are created. `props` looks up a property (the environment
/// is read for the `otel.*` names that are not properties).
pub fn install(ctx: &CamelContext, props: &dyn Fn(&str) -> Option<String>) -> Result<Option<OpenTelemetry>> {
    if !is_enabled(props)? {
        return Ok(None);
    }
    let get = config::lookup_with(props);
    if get("otel.sdk.disabled").map(|v| v.eq_ignore_ascii_case("true")).unwrap_or(false) {
        cameleer_core::camel_info!(exporter::LOGGER, "OpenTelemetry is disabled (OTEL_SDK_DISABLED=true)");
        return Ok(None);
    }
    let tcfg = telemetry_config(props)?;
    let ocfg = OtlpConfig::from_lookup(&get)?;
    Ok(Some(install_with(ctx, tcfg, ocfg)?))
}

/// Installs tracing with explicit settings.
pub fn install_with(ctx: &CamelContext, tcfg: TelemetryConfig, ocfg: OtlpConfig) -> Result<OpenTelemetry> {
    let bcfg = BatchConfig {
        max_queue_size: ocfg.max_queue_size,
        max_export_batch_size: ocfg.max_export_batch_size,
        schedule_delay: ocfg.schedule_delay,
        // OTEL_EXPORTER_OTLP_TIMEOUT is the budget of one batch, retries included
        export_timeout: ocfg.timeout.min(ocfg.export_timeout),
    };
    let (processor, batch): (Arc<dyn SpanProcessor>, Option<Arc<BatchSpanProcessor>>) = match ocfg.exporter {
        ExporterKind::Otlp => {
            let b = Arc::new(BatchSpanProcessor::new(Arc::new(OtlpHttpExporter::new(&ocfg)?), bcfg));
            (b.clone(), Some(b))
        }
        ExporterKind::Console => {
            let b = Arc::new(BatchSpanProcessor::new(Arc::new(exporter::ConsoleExporter), bcfg));
            (b.clone(), Some(b))
        }
        ExporterKind::None => (Arc::new(Discard), None),
    };
    let what = match ocfg.exporter {
        ExporterKind::Otlp => format!("exporting to {}", ocfg.endpoint),
        ExporterKind::Console => "logging spans".to_string(),
        ExporterKind::None => "not exporting".to_string(),
    };
    cameleer_core::camel_info!(exporter::LOGGER, "OpenTelemetry tracing enabled ({}), service {}", what, ocfg.resource[0].1);
    let t = telemetry::install(ctx, tcfg, processor);
    if let Some(b) = &batch {
        ctx.registry().bind_arc(STATS_NAME, b.stats());
    }
    Ok(OpenTelemetry { telemetry: t, batch, config: ocfg })
}

/// Registry name of the exporter's [`BatchStats`] (read by `/q/metrics`).
pub const STATS_NAME: &str = "cameleer.opentelemetry.stats";

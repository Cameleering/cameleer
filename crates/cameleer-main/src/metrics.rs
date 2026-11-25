//! Camel Main's metrics options: `camel.metrics.*` (Camel's `camel-micrometer-prometheus`) and
//! `camel.opentelemetry.metrics.*` (the `camel-opentelemetry-metrics` starter), turned into the
//! built-in meters of `cameleer_core::metrics::instrument`.

use std::collections::BTreeMap;

use cameleer_core::error::{CamelError, Result};
use cameleer_core::metrics::instrument::{Flavor, MetricsConfig, PolicyLevel};

use crate::console::prop;

fn flag(props: &BTreeMap<String, String>, k: &str) -> Result<Option<bool>> {
    match prop(props, k).map(|v| v.trim().to_ascii_lowercase()) {
        None => Ok(None),
        Some(v) if v == "true" => Ok(Some(true)),
        Some(v) if v == "false" => Ok(Some(false)),
        Some(v) => Err(CamelError::illegal_argument(format!("{} must be true or false: {}", k, v))),
    }
}

fn level(v: Option<String>) -> PolicyLevel {
    match v.as_deref().map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        None | Some("all") => PolicyLevel::All,
        Some("context") => PolicyLevel::Context,
        // as Camel: anything else is the route level only
        Some(_) => PolicyLevel::Route,
    }
}

/// The built-in meters the properties ask for, if any. Both `camel.metrics.enabled` and
/// `camel.opentelemetry.metrics.enabled` is refused, as Camel refuses both route policies on a
/// route.
pub fn config(props: &BTreeMap<String, String>) -> Result<Option<MetricsConfig>> {
    let micrometer = flag(props, "camel.metrics.enabled")?.unwrap_or(false);
    let otel = flag(props, "camel.opentelemetry.metrics.enabled")?.unwrap_or(false);
    if micrometer && otel {
        return Err(CamelError::illegal_argument("camel.metrics.enabled and camel.opentelemetry.metrics.enabled are both true: choose one (Camel allows one route policy per route)"));
    }
    let mut c = MetricsConfig::default();
    if micrometer {
        let f = |k: &str, d: bool| -> Result<bool> { Ok(flag(props, &format!("camel.metrics.{}", k))?.unwrap_or(d)) };
        c.flavor = Flavor::Micrometer;
        c.legacy_names = match prop(props, "camel.metrics.naming-strategy").map(|v| v.trim().to_ascii_lowercase()) {
            None => false,
            Some(v) if v == "default" => false,
            Some(v) if v == "legacy" => true,
            Some(v) => return Err(CamelError::illegal_argument(format!("camel.metrics.namingStrategy must be default or legacy: {}", v))),
        };
        c.route_policy = f("enable-route-policy", true)?;
        c.route_policy_level = level(prop(props, "camel.metrics.route-policy-level"));
        c.message_history = f("enable-message-history", false)?;
        c.exchange_event_notifier = f("enable-exchange-event-notifier", true)?;
        // Camel's spelling is baseEndpointURIExchangeEventNotifier
        c.base_endpoint_uri = props.get("camel.metrics.baseEndpointURIExchangeEventNotifier").map(|v| v.trim() != "false").unwrap_or(true)
            && f("base-endpoint-uri-exchange-event-notifier", true)?;
        c.record_last_exchange_timestamp = f("record-last-exchange-timestamp", true)?;
        c.route_event_notifier = f("enable-route-event-notifier", true)?;
        c.skip_camel_info = f("skip-camel-info", false)?;
        if let Some(v) = prop(props, "camel.metrics.text-format-version") {
            if v.trim() != "0.0.4" {
                return Err(CamelError::illegal_argument(format!("camel.metrics.textFormatVersion={} is not supported: the Prometheus endpoint writes 0.0.4", v)));
            }
        }
        if let Some(b) = prop(props, "camel.metrics.binders") {
            cameleer_core::camel_warn!("Main", "camel.metrics.binders={} is ignored: there is no JVM to bind", b);
        }
        return Ok(Some(c));
    }
    if otel {
        let f = |k: &str, d: bool| -> Result<bool> { Ok(flag(props, &format!("camel.opentelemetry.metrics.{}", k))?.unwrap_or(d)) };
        c.flavor = Flavor::OpenTelemetry;
        c.route_policy = f("enable-route-policy", true)?;
        c.route_policy_level = level(prop(props, "camel.opentelemetry.metrics.route-policy-level"));
        c.route_policy_exclude = prop(props, "camel.opentelemetry.metrics.route-policy-exclude-pattern")
            .map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
            .unwrap_or_default();
        c.message_history = f("enable-message-history", false)?;
        c.exchange_event_notifier = f("enable-exchange-event-notifier", true)?;
        c.base_endpoint_uri = f("base-endpoint-uri-exchange-event-notifier", true)?;
        c.route_event_notifier = f("enable-route-event-notifier", true)?;
        return Ok(Some(c));
    }
    Ok(None)
}

/// `camel.metrics.logMetricsOnShutdown`: the format (`json` or `prometheus`) and the name filters.
pub fn log_on_shutdown(props: &BTreeMap<String, String>) -> Result<Option<(String, Vec<String>)>> {
    if !flag(props, "camel.metrics.log-metrics-on-shutdown")?.unwrap_or(false) {
        return Ok(None);
    }
    let format = prop(props, "camel.metrics.log-metrics-on-shutdown-format").map(|v| v.trim().to_ascii_lowercase()).unwrap_or_else(|| "json".into());
    if format != "json" && format != "prometheus" {
        return Err(CamelError::illegal_argument(format!("camel.metrics.logMetricsOnShutdownFormat must be json or prometheus: {}", format)));
    }
    let filters = prop(props, "camel.metrics.log-metrics-on-shutdown-filters").map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()).unwrap_or_default();
    Ok(Some((format, filters)))
}

/// The meters as JSON (`logMetricsOnShutdownFormat=json`): one object per meter with its name,
/// tags, type and values.
pub fn meters_json(meters: &[(cameleer_core::metrics::MeterId, cameleer_core::metrics::Registered)]) -> String {
    use cameleer_core::metrics::MeterKind;
    use cameleer_core::value::Value;
    let list: Vec<Value> = meters
        .iter()
        .map(|(id, r)| {
            let tags = Value::Object(id.tags.iter().map(|(k, v)| (k.clone(), Value::from(v.as_str()))).collect());
            let mut o = vec![("name", Value::from(id.name.as_str())), ("tags", tags), ("type", Value::from(r.kind.type_name()))];
            match &r.kind {
                MeterKind::Counter(c) => o.push(("count", Value::Float(c.count()))),
                MeterKind::Gauge(g) | MeterKind::UpDownCounter(g) => o.push(("value", Value::Float(g.value()))),
                MeterKind::FnGauge(g) => o.push(("value", Value::Float((g.0)()))),
                MeterKind::Meter(m) => {
                    o.push(("count", Value::Int(m.count() as i64)));
                    o.push(("meanRate", Value::Float(m.mean_rate())));
                }
                MeterKind::Timer(t) => {
                    o.push(("count", Value::Int(t.0.count() as i64)));
                    o.push(("totalTimeSeconds", Value::Float(t.0.total())));
                    o.push(("maxSeconds", Value::Float(t.0.max())));
                }
                MeterKind::Summary(d) => {
                    o.push(("count", Value::Int(d.count() as i64)));
                    o.push(("total", Value::Float(d.total())));
                    o.push(("max", Value::Float(d.max())));
                }
                MeterKind::LongTaskTimer(t) => o.push(("activeTasks", Value::Int(t.active_tasks() as i64))),
            }
            Value::object(o)
        })
        .collect();
    Value::Array(list).to_string()
}

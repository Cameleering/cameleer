//! Exporter configuration from the OpenTelemetry SDK environment variables
//! (`OTEL_EXPORTER_OTLP_*`, `OTEL_SERVICE_NAME`, `OTEL_RESOURCE_ATTRIBUTES`, `OTEL_BSP_*`,
//! `OTEL_TRACES_*`), or their property spelling (`otel.exporter.otlp.endpoint`), which wins
//! over the environment as system properties do in the Java SDK's autoconfiguration.

use std::time::Duration;

use cameleer_core::error::{CamelError, Result};
use cameleer_core::telemetry::span::{AttrValue, Attributes};
use cameleer_core::telemetry::Sampler;

/// Looks up one setting by its property name (`otel.exporter.otlp.endpoint`).
pub type Lookup<'a> = &'a dyn Fn(&str) -> Option<String>;

/// A lookup that reads `props` first, then the environment variable (`OTEL_EXPORTER_OTLP_ENDPOINT`
/// for `otel.exporter.otlp.endpoint`).
pub fn lookup_with<'a>(props: &'a dyn Fn(&str) -> Option<String>) -> impl Fn(&str) -> Option<String> + 'a {
    move |name: &str| {
        props(name).or_else(|| std::env::var(env_name(name)).ok()).map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
    }
}

/// The environment variable of a property name: upper case, `.` and `-` as `_`.
pub fn env_name(prop: &str) -> String {
    prop.chars().map(|c| if c == '.' || c == '-' { '_' } else { c.to_ascii_uppercase() }).collect()
}

/// What happens to finished spans (`OTEL_TRACES_EXPORTER`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExporterKind {
    /// OTLP over HTTP with protobuf (`otlp`, the default).
    Otlp,
    /// One log line per span (`console`, or `logging` as the Java SDK calls it).
    Console,
    /// Spans are created and propagated but not exported (`none`).
    None,
}

/// The settings of the OTLP exporter and the batch span processor.
#[derive(Clone, Debug)]
pub struct OtlpConfig {
    pub exporter: ExporterKind,
    /// The full traces URL (`.../v1/traces`).
    pub endpoint: String,
    pub headers: Vec<(String, String)>,
    /// Timeout of one export request (`OTEL_EXPORTER_OTLP_TIMEOUT`, default 10 s).
    pub timeout: Duration,
    pub gzip: bool,
    /// CA certificates (PEM file) to trust instead of the system store.
    pub certificate: Option<String>,
    /// Client certificate and key (PEM files) for mutual TLS.
    pub client_certificate: Option<String>,
    pub client_key: Option<String>,
    pub resource: Attributes,
    /// Delay between two exports (`OTEL_BSP_SCHEDULE_DELAY`, default 5000 ms).
    pub schedule_delay: Duration,
    /// Time allowed for one export including its retries (`OTEL_BSP_EXPORT_TIMEOUT`, 30000 ms).
    pub export_timeout: Duration,
    /// Spans kept waiting for export; more are dropped (`OTEL_BSP_MAX_QUEUE_SIZE`, 2048).
    pub max_queue_size: usize,
    /// Spans per export request (`OTEL_BSP_MAX_EXPORT_BATCH_SIZE`, 512).
    pub max_export_batch_size: usize,
    /// Retry attempts after a retryable failure (5 attempts in all, as the Java SDK).
    pub max_attempts: u32,
    /// First retry delay (1 s), growing 1.5x to at most `max_backoff` (5 s), with jitter.
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}

impl Default for OtlpConfig {
    fn default() -> Self {
        OtlpConfig {
            exporter: ExporterKind::Otlp,
            endpoint: "http://localhost:4318/v1/traces".into(),
            headers: Vec::new(),
            timeout: Duration::from_secs(10),
            gzip: false,
            certificate: None,
            client_certificate: None,
            client_key: None,
            resource: default_resource(None, &[]),
            schedule_delay: Duration::from_millis(5000),
            export_timeout: Duration::from_millis(30000),
            max_queue_size: 2048,
            max_export_batch_size: 512,
            max_attempts: 5,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(5),
        }
    }
}

fn millis(get: Lookup, name: &str, default: Duration) -> Result<Duration> {
    match get(name) {
        None => Ok(default),
        Some(v) => v.parse::<u64>().map(Duration::from_millis).map_err(|_| CamelError::illegal_argument(format!("{} ({}) must be milliseconds: {}", name, env_name(name), v))),
    }
}

fn count(get: Lookup, name: &str, default: usize) -> Result<usize> {
    match get(name) {
        None => Ok(default),
        Some(v) => v.parse::<usize>().ok().filter(|n| *n > 0).ok_or_else(|| CamelError::illegal_argument(format!("{} ({}) must be a positive number: {}", name, env_name(name), v))),
    }
}

impl OtlpConfig {
    /// Reads the settings; the traces-specific variable wins over the general one.
    pub fn from_lookup(get: Lookup) -> Result<OtlpConfig> {
        let mut c = OtlpConfig::default();
        let traces_or = |specific: &str, general: &str| get(specific).or_else(|| get(general));
        c.exporter = match get("otel.traces.exporter").as_deref().map(str::to_ascii_lowercase).as_deref() {
            None | Some("otlp") => ExporterKind::Otlp,
            Some("console") | Some("logging") => ExporterKind::Console,
            Some("none") => ExporterKind::None,
            Some(o) => return Err(CamelError::illegal_argument(format!("OTEL_TRACES_EXPORTER={} is not supported (otlp, console, logging, none)", o))),
        };
        let protocol = traces_or("otel.exporter.otlp.traces.protocol", "otel.exporter.otlp.protocol").unwrap_or_else(|| "http/protobuf".into());
        if c.exporter == ExporterKind::Otlp && protocol != "http/protobuf" {
            return Err(CamelError::illegal_argument(format!("OTEL_EXPORTER_OTLP_PROTOCOL={} is not supported: Cameleer exports OTLP over HTTP with protobuf (http/protobuf)", protocol)));
        }
        if let Some(e) = get("otel.exporter.otlp.traces.endpoint") {
            c.endpoint = e;
        } else if let Some(e) = get("otel.exporter.otlp.endpoint") {
            // the signal path is appended to the base url
            c.endpoint = format!("{}/v1/traces", e.trim_end_matches('/'));
        }
        if !c.endpoint.starts_with("http://") && !c.endpoint.starts_with("https://") {
            return Err(CamelError::illegal_argument(format!("The OTLP endpoint must be an http or https url: {}", c.endpoint)));
        }
        for h in [get("otel.exporter.otlp.headers"), get("otel.exporter.otlp.traces.headers")].into_iter().flatten() {
            for (k, v) in parse_list(&h)? {
                c.headers.retain(|(o, _)| !o.eq_ignore_ascii_case(&k));
                c.headers.push((k, v));
            }
        }
        if let Some(t) = traces_or("otel.exporter.otlp.traces.timeout", "otel.exporter.otlp.timeout") {
            c.timeout = Duration::from_millis(t.parse().map_err(|_| CamelError::illegal_argument(format!("OTEL_EXPORTER_OTLP_TIMEOUT must be milliseconds: {}", t)))?);
        }
        c.gzip = match traces_or("otel.exporter.otlp.traces.compression", "otel.exporter.otlp.compression").as_deref() {
            None | Some("none") => false,
            Some("gzip") => true,
            Some(o) => return Err(CamelError::illegal_argument(format!("OTEL_EXPORTER_OTLP_COMPRESSION={} is not supported (gzip, none)", o))),
        };
        c.certificate = traces_or("otel.exporter.otlp.traces.certificate", "otel.exporter.otlp.certificate");
        c.client_certificate = traces_or("otel.exporter.otlp.traces.client.certificate", "otel.exporter.otlp.client.certificate");
        c.client_key = traces_or("otel.exporter.otlp.traces.client.key", "otel.exporter.otlp.client.key");
        if c.client_certificate.is_some() != c.client_key.is_some() {
            return Err(CamelError::illegal_argument("OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE and OTEL_EXPORTER_OTLP_CLIENT_KEY must be set together"));
        }
        let attrs = match get("otel.resource.attributes") {
            Some(a) => parse_list(&a)?,
            None => Vec::new(),
        };
        c.resource = default_resource(get("otel.service.name").as_deref(), &attrs);
        c.schedule_delay = millis(get, "otel.bsp.schedule.delay", c.schedule_delay)?;
        c.export_timeout = millis(get, "otel.bsp.export.timeout", c.export_timeout)?;
        c.max_queue_size = count(get, "otel.bsp.max.queue.size", c.max_queue_size)?;
        c.max_export_batch_size = count(get, "otel.bsp.max.export.batch.size", c.max_export_batch_size)?.min(c.max_queue_size);
        Ok(c)
    }
}

/// Parses `key=value,key2=value2` with percent-encoded values (`OTEL_RESOURCE_ATTRIBUTES`,
/// `OTEL_EXPORTER_OTLP_HEADERS`).
pub fn parse_list(s: &str) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for item in s.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let (k, v) = item.split_once('=').ok_or_else(|| CamelError::illegal_argument(format!("Invalid key=value item: {}", item)))?;
        let k = k.trim();
        if k.is_empty() {
            return Err(CamelError::illegal_argument(format!("Invalid key=value item: {}", item)));
        }
        out.push((k.to_string(), percent_decode(v.trim())));
    }
    Ok(out)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 3 <= b.len() && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() {
            if let Ok(x) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(x);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The resource: `service.name` (`OTEL_SERVICE_NAME`, else `service.name` from the attributes,
/// else `unknown_service:<executable>`), the other attributes, and the `telemetry.sdk.*`
/// attributes.
pub fn default_resource(service_name: Option<&str>, attrs: &[(String, String)]) -> Attributes {
    let from_attrs = attrs.iter().find(|(k, _)| k == "service.name").map(|(_, v)| v.clone());
    let name = service_name.map(str::to_string).or(from_attrs).unwrap_or_else(|| {
        let exe = std::env::current_exe().ok().and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned())).unwrap_or_else(|| "cameleer".into());
        format!("unknown_service:{}", exe)
    });
    let mut r: Attributes = vec![("service.name".into(), AttrValue::String(name))];
    for (k, v) in attrs {
        if k != "service.name" && !k.starts_with("telemetry.sdk.") {
            r.push((k.clone(), AttrValue::String(v.clone())));
        }
    }
    r.push(("telemetry.sdk.name".into(), "cameleer".into()));
    r.push(("telemetry.sdk.language".into(), "rust".into()));
    r.push(("telemetry.sdk.version".into(), env!("CARGO_PKG_VERSION").into()));
    r
}

/// The sampler of `OTEL_TRACES_SAMPLER` and `OTEL_TRACES_SAMPLER_ARG` (default
/// `parentbased_always_on`).
pub fn sampler(get: Lookup) -> Result<Sampler> {
    let ratio = || -> Result<f64> {
        match get("otel.traces.sampler.arg") {
            None => Ok(1.0),
            Some(a) => a.parse::<f64>().ok().filter(|r| (0.0..=1.0).contains(r)).ok_or_else(|| CamelError::illegal_argument(format!("OTEL_TRACES_SAMPLER_ARG must be a ratio between 0 and 1: {}", a))),
        }
    };
    Ok(match get("otel.traces.sampler").as_deref().map(str::to_ascii_lowercase).as_deref() {
        None | Some("parentbased_always_on") => Sampler::ParentBased(Box::new(Sampler::AlwaysOn)),
        Some("always_on") => Sampler::AlwaysOn,
        Some("always_off") => Sampler::AlwaysOff,
        Some("traceidratio") => Sampler::TraceIdRatio(ratio()?),
        Some("parentbased_always_off") => Sampler::ParentBased(Box::new(Sampler::AlwaysOff)),
        Some("parentbased_traceidratio") => Sampler::ParentBased(Box::new(Sampler::TraceIdRatio(ratio()?))),
        Some(o) => return Err(CamelError::illegal_argument(format!("OTEL_TRACES_SAMPLER={} is not supported", o))),
    })
}

/// The propagators of `OTEL_PROPAGATORS` (default `tracecontext,baggage`): (trace context,
/// baggage). `none` turns both off; other propagators (`b3`, `jaeger`, ...) are refused.
pub fn propagators(get: Lookup) -> Result<(bool, bool)> {
    let Some(v) = get("otel.propagators") else { return Ok((true, true)) };
    let mut out = (false, false);
    for p in v.split(',').map(|p| p.trim().to_ascii_lowercase()).filter(|p| !p.is_empty()) {
        match p.as_str() {
            "tracecontext" => out.0 = true,
            "baggage" => out.1 = true,
            "none" => return Ok((false, false)),
            o => return Err(CamelError::illegal_argument(format!("OTEL_PROPAGATORS: {} is not supported (tracecontext, baggage, none)", o))),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn cfg(pairs: &[(&str, &str)]) -> Result<OtlpConfig> {
        let m: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        OtlpConfig::from_lookup(&|k: &str| m.get(k).cloned())
    }

    #[test]
    fn endpoints_headers_and_resource() {
        let c = cfg(&[]).unwrap();
        assert_eq!(c.endpoint, "http://localhost:4318/v1/traces");
        assert!(matches!(&c.resource[0].1, AttrValue::String(s) if s.starts_with("unknown_service:")));
        let c = cfg(&[
            ("otel.exporter.otlp.endpoint", "https://collector:4318/"),
            ("otel.exporter.otlp.headers", "Authorization=Bearer%20abc, x-a = 1"),
            ("otel.exporter.otlp.traces.headers", "x-a=2"),
            ("otel.service.name", "orders"),
            ("otel.resource.attributes", "service.name=ignored,deployment.environment=prod%2Ceu"),
            ("otel.exporter.otlp.compression", "gzip"),
            ("otel.bsp.max.queue.size", "10"),
            ("otel.bsp.max.export.batch.size", "512"),
        ])
        .unwrap();
        assert_eq!(c.endpoint, "https://collector:4318/v1/traces");
        assert_eq!(c.headers, vec![("Authorization".into(), "Bearer abc".into()), ("x-a".into(), "2".into())]);
        assert_eq!(c.resource[0], ("service.name".into(), AttrValue::from("orders")));
        assert_eq!(c.resource[1], ("deployment.environment".into(), AttrValue::from("prod,eu")));
        assert!(c.gzip);
        assert_eq!((c.max_queue_size, c.max_export_batch_size), (10, 10));
        let c = cfg(&[("otel.exporter.otlp.endpoint", "http://a:1"), ("otel.exporter.otlp.traces.endpoint", "http://b:2/custom")]).unwrap();
        assert_eq!(c.endpoint, "http://b:2/custom");
        assert!(cfg(&[("otel.exporter.otlp.protocol", "grpc")]).is_err());
        assert!(cfg(&[("otel.exporter.otlp.client.key", "k.pem")]).is_err());
        assert!(cfg(&[("otel.bsp.max.queue.size", "0")]).is_err());
        assert_eq!(cfg(&[("otel.traces.exporter", "logging")]).unwrap().exporter, ExporterKind::Console);
        assert_eq!(env_name("otel.exporter.otlp.traces.client.key"), "OTEL_EXPORTER_OTLP_TRACES_CLIENT_KEY");
    }

    #[test]
    fn samplers_and_propagators() {
        let m = |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|(a, _)| *a == k).map(|(_, v)| v.to_string());
        assert!(matches!(sampler(&m(&[])).unwrap(), Sampler::ParentBased(b) if matches!(*b, Sampler::AlwaysOn)));
        assert!(matches!(sampler(&m(&[("otel.traces.sampler", "traceidratio"), ("otel.traces.sampler.arg", "0.25")])).unwrap(), Sampler::TraceIdRatio(r) if r == 0.25));
        assert!(sampler(&m(&[("otel.traces.sampler", "traceidratio"), ("otel.traces.sampler.arg", "2")])).is_err());
        assert_eq!(propagators(&m(&[])).unwrap(), (true, true));
        assert_eq!(propagators(&m(&[("otel.propagators", "tracecontext")])).unwrap(), (true, false));
        assert!(propagators(&m(&[("otel.propagators", "b3")])).is_err());
    }
}

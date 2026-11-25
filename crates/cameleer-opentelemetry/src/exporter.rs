//! The OTLP/HTTP span exporter: `POST` of an `ExportTraceServiceRequest` in protobuf
//! (`Content-Type: application/x-protobuf`), optionally gzip-compressed, with the configured
//! headers, over the shared HTTP client (TLS with full verification).
//!
//! Failures are handled as the OTLP/HTTP specification says: `429`, `502`, `503` and `504`
//! and transport errors are retried with exponential backoff and jitter, waiting at least what a
//! `Retry-After` header asks for, while the export's time budget lasts; any other status is
//! final and the batch is dropped. A `200` with a partial success is logged.

use std::time::{Duration, Instant};

use cameleer_core::error::{CamelError, Result};
use cameleer_core::telemetry::span::{Attributes, SpanData};
use cameleer_http::client::HttpClient;
use cameleer_http::http::HttpHeaders;

use crate::config::OtlpConfig;
use crate::proto;

/// Logger name of the exporter.
pub const LOGGER: &str = "org.apache.camel.opentelemetry.OtlpExporter";

/// The outcome of one export call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportResult {
    Success,
    /// The batch was dropped: (reason, whether the last failure was retryable).
    Failure(String, bool),
}

/// Sends batches of spans somewhere.
pub trait SpanExporter: Send + Sync {
    /// Exports one batch within `budget` (retries included).
    fn export(&self, spans: &[SpanData], budget: Duration) -> ExportResult;
}

/// The OTLP/HTTP exporter.
pub struct OtlpHttpExporter {
    url: cameleer_http::client::Url,
    client: HttpClient,
    headers: Vec<(String, String)>,
    gzip: bool,
    timeout: Duration,
    resource: Attributes,
    max_attempts: u32,
    initial_backoff: Duration,
    max_backoff: Duration,
}

impl OtlpHttpExporter {
    pub fn new(cfg: &OtlpConfig) -> Result<OtlpHttpExporter> {
        let url = cameleer_http::client::Url::parse(&cfg.endpoint)?;
        let mut client = HttpClient::new().with_keep_alive(true);
        client.connect_timeout = cfg.timeout;
        client.read_timeout = cfg.timeout;
        client.connections_per_route = 2;
        if url.is_https() {
            let mut b = cameleer_tls::TlsClientConfig::builder().alpn_protocols(&["http/1.1"]);
            if let Some(ca) = &cfg.certificate {
                // a configured CA replaces the system store, as in the SDKs
                b = b.system_roots(false).add_root_pem_file(ca).map_err(|e| CamelError::illegal_argument(format!("OTEL_EXPORTER_OTLP_CERTIFICATE {}: {}", ca, e)))?;
            }
            if let (Some(cert), Some(key)) = (&cfg.client_certificate, &cfg.client_key) {
                let read = |p: &str| std::fs::read(p).map_err(|e| CamelError::io(format!("Cannot read {}: {}", p, e)));
                b = b.client_cert_pem(&read(cert)?, &read(key)?).map_err(|e| CamelError::illegal_argument(format!("OTLP client certificate: {}", e)))?;
            }
            client = client.with_tls(b.build());
        }
        Ok(OtlpHttpExporter {
            url,
            client,
            headers: cfg.headers.clone(),
            gzip: cfg.gzip,
            timeout: cfg.timeout,
            resource: cfg.resource.clone(),
            max_attempts: cfg.max_attempts.max(1),
            initial_backoff: cfg.initial_backoff,
            max_backoff: cfg.max_backoff,
        })
    }

    /// The request body of a batch (compressed when gzip is on).
    pub fn body(&self, spans: &[SpanData]) -> Vec<u8> {
        let raw = proto::encode_export_request(&self.resource, env!("CARGO_PKG_VERSION"), spans);
        if self.gzip {
            cameleer_core::dataformat::compress::gzip_compress(&raw, 6)
        } else {
            raw
        }
    }

    fn attempt(&self, body: &[u8], timeout: Duration) -> std::result::Result<(), (String, bool, Option<Duration>)> {
        let mut h = HttpHeaders::new();
        h.set("Content-Type", "application/x-protobuf");
        if self.gzip {
            h.set("Content-Encoding", "gzip");
        }
        h.set("User-Agent", concat!("cameleer-opentelemetry/", env!("CARGO_PKG_VERSION")));
        for (k, v) in &self.headers {
            h.set(k.clone(), v.clone());
        }
        match self.client.execute("POST", &self.url, &h, Some(body), Some(timeout)) {
            Err(e) => Err((format!("{}: {}", self.url, e.message), true, None)),
            Ok(r) if (200..300).contains(&r.status) => {
                let (rejected, msg) = proto::decode_partial_success(&r.body);
                if rejected != 0 || !msg.is_empty() {
                    cameleer_core::camel_warn!(LOGGER, "The OTLP receiver rejected {} spans: {}", rejected, msg);
                }
                Ok(())
            }
            Ok(r) => {
                let retryable = matches!(r.status, 429 | 502 | 503 | 504);
                let retry_after = r.headers.get("retry-after").and_then(parse_retry_after);
                let text = String::from_utf8_lossy(&r.body[..r.body.len().min(200)]).into_owned();
                Err((format!("{} answered {} {}", self.url, r.status, text.trim()), retryable, retry_after))
            }
        }
    }
}

/// `Retry-After` as delay-seconds; an HTTP date is relative to now.
pub fn parse_retry_after(v: &str) -> Option<Duration> {
    let v = v.trim();
    if let Ok(s) = v.parse::<u64>() {
        return Some(Duration::from_secs(s));
    }
    let at = parse_imf_fixdate(v)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    Some(Duration::from_secs(at.saturating_sub(now).max(0) as u64))
}

/// Parses an IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`) to epoch seconds.
fn parse_imf_fixdate(v: &str) -> Option<i64> {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let p: Vec<&str> = v.split_whitespace().collect();
    if p.len() != 6 || p[5] != "GMT" {
        return None;
    }
    let d: u32 = p[1].parse().ok()?;
    let m = MONTHS.iter().position(|x| *x == p[2])? as u32 + 1;
    let y: i64 = p[3].parse().ok()?;
    let t: Vec<u32> = p[4].split(':').map(|x| x.parse().ok()).collect::<Option<_>>()?;
    if t.len() != 3 {
        return None;
    }
    Some(cameleer_core::util_time::epoch_from_civil(y, m, d, t[0], t[1], t[2]))
}

/// The delay before retry `n` (1-based): `initial * 1.5^(n-1)`, capped, with up to 20% jitter
/// either way.
pub fn backoff(initial: Duration, max: Duration, n: u32) -> Duration {
    let base = initial.as_secs_f64() * 1.5f64.powi(n.saturating_sub(1) as i32);
    let base = base.min(max.as_secs_f64());
    let jitter = (cameleer_core::telemetry::context::random_u64() % 1000) as f64 / 1000.0 * 0.4 - 0.2;
    Duration::from_secs_f64((base * (1.0 + jitter)).max(0.0))
}

impl SpanExporter for OtlpHttpExporter {
    fn export(&self, spans: &[SpanData], budget: Duration) -> ExportResult {
        if spans.is_empty() {
            return ExportResult::Success;
        }
        let deadline = Instant::now() + budget;
        let body = self.body(spans);
        let mut n = 0;
        loop {
            n += 1;
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return ExportResult::Failure("export timeout".into(), true);
            }
            match self.attempt(&body, self.timeout.min(left)) {
                Ok(()) => return ExportResult::Success,
                Err((msg, false, _)) => return ExportResult::Failure(msg, false),
                Err((msg, true, after)) => {
                    if n >= self.max_attempts {
                        return ExportResult::Failure(msg, true);
                    }
                    let wait = backoff(self.initial_backoff, self.max_backoff, n).max(after.unwrap_or_default());
                    if Instant::now() + wait >= deadline {
                        return ExportResult::Failure(format!("{} (no time left to retry)", msg), true);
                    }
                    cameleer_core::camel_debug!(LOGGER, "Export failed ({}), retry {} in {:?}", msg, n, wait);
                    std::thread::sleep(wait);
                }
            }
        }
    }
}

/// Logs one line per span (`OTEL_TRACES_EXPORTER=console` or `logging`).
pub struct ConsoleExporter;

impl SpanExporter for ConsoleExporter {
    fn export(&self, spans: &[SpanData], _budget: Duration) -> ExportResult {
        for s in spans {
            let attrs: Vec<String> = s.attributes.iter().map(|(k, v)| format!("{}={}", k, v)).collect();
            cameleer_core::camel_info!(
                "org.apache.camel.opentelemetry.LoggingSpanExporter",
                "'{}' : {} {} {} {:?} [tracer: {}] {{{}}}",
                s.name,
                cameleer_core::telemetry::context::hex(&s.trace_id),
                cameleer_core::telemetry::context::hex(&s.span_id),
                s.kind.as_str(),
                s.duration(),
                s.scope_name,
                attrs.join(", ")
            );
        }
        ExportResult::Success
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_is_capped() {
        let i = Duration::from_secs(1);
        let m = Duration::from_secs(5);
        for _ in 0..50 {
            let b1 = backoff(i, m, 1).as_secs_f64();
            assert!((0.8..=1.2).contains(&b1), "{b1}");
            let b3 = backoff(i, m, 3).as_secs_f64();
            assert!((1.8..=2.7).contains(&b3), "{b3}");
            assert!(backoff(i, m, 20).as_secs_f64() <= 6.0);
        }
        assert_eq!(parse_retry_after(" 3 "), Some(Duration::from_secs(3)));
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), Some(Duration::ZERO));
        assert_eq!(parse_retry_after("soon"), None);
    }
}

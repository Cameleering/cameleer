//! `splunk-hec:` component (Camel `camel-splunk-hec`, since Camel 3.3): publish events to Splunk
//! through the HTTP Event Collector.
//!
//! `splunk-hec:splunkURL?token=...` where `splunkURL` is `host:port` of the HEC listener
//! (`splunk-hec:splunk.example.com:8088?token=8b7d0b2e-...`). Producer only.
//!
//! Each exchange is one `POST {https|http}://{splunkURL}{splunkEndpoint}` with
//! `Authorization: Splunk {token}` and a JSON body like Camel's:
//!
//! ```json
//! {"sourcetype":"camel","source":"camel","index":"camel","host":"myhost","time":1700000000,
//!  "event":{"body":...,"headers":{...}}}
//! ```
//!
//! - `event` is `{"body": body, "headers": headers}`; with `bodyOnly=true` it is the body alone,
//!   with `headersOnly=true` the headers alone (`bodyOnly` wins when both are set).
//! - Bytes bodies are sent as text (a JSON string); objects, arrays, strings and numbers as JSON.
//!   A list body is one event whose `event` is the array (Camel does not split or batch).
//! - `host` defaults to the local host name (`unknown` when it cannot be found); it is the
//!   event's `host` field, not the server connected to.
//! - `time` comes from the `CamelSplunkHECIndexTime` header, else the `time` option, else is left
//!   out (Splunk then uses the arrival time).
//! - `splunkEndpoint` (default `/services/collector/event`) is appended as given, query included
//!   (`/services/collector/event?auto_extract_timestamp=true`); the same envelope is posted to
//!   `/services/collector/raw`, as in Camel.
//! - Only status 200 is success; anything else fails with `RuntimeCamelException`
//!   (`Splunk HEC request failed: HTTP/1.1 403 Forbidden` and the response body on the next line).
//!
//! TLS: `https=true` (default) verifies the server certificate and host name against
//! `sslContextParameters` (endpoint, `camel.component.splunk-hec.sslContextParameters`, or the
//! global parameters with `useGlobalSslContextParameters=true`), else the system trust store.
//! `skipTlsVerify=true` is refused: Cameleer never turns certificate verification off; trust a
//! private Splunk by adding its certificate or CA to the trust store.

use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

use cameleer_core::context::CamelContext;
use cameleer_core::error::{kinds, register_kind_parent, CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::spi::{Component, Endpoint, Processor, Producer, Service};
use cameleer_core::util::Parameters;
use cameleer_core::value::{ObjectMap, Value};
use cameleer_http::{HttpClient, HttpHeaders, Url};

/// Header with the epoch time of the event (overrides the `time` option).
pub const INDEX_TIME: &str = "CamelSplunkHECIndexTime";

/// Kind of the failure for a non-200 answer (Camel's `RuntimeCamelException`).
pub const RUNTIME_CAMEL_EXCEPTION: &str = "RuntimeCamelException";

const DEFAULT_ENDPOINT: &str = "/services/collector/event";

/// The `splunk-hec` component.
#[derive(Default)]
pub struct SplunkHecComponent;

/// The endpoint configuration (Camel's `SplunkHECConfiguration`).
#[derive(Clone, Debug)]
pub struct SplunkHecConfig {
    pub splunk_url: String,
    pub token: String,
    pub index: Option<String>,
    pub source: Option<String>,
    pub source_type: Option<String>,
    pub host: Option<String>,
    pub splunk_endpoint: String,
    pub time: Option<i64>,
    pub https: bool,
    pub body_only: bool,
    pub headers_only: bool,
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Camel's token check: `^\w{8}-\w{4}-\w{4}-\w{4}-\w{12}$`.
fn valid_token(t: &str) -> bool {
    let parts: Vec<&str> = t.split('-').collect();
    parts.len() == 5 && parts.iter().zip([8, 4, 4, 4, 12]).all(|(p, n)| p.chars().count() == n && p.chars().all(is_word))
}

/// A host name (labels of letters, digits and `-`, not starting or ending with `-`) or an IPv4
/// literal, as Camel's `DomainValidator` (local names allowed) or `InetAddressValidator`.
fn valid_host(h: &str) -> bool {
    if h.parse::<std::net::Ipv4Addr>().is_ok() {
        return true;
    }
    let h = h.strip_suffix('.').unwrap_or(h);
    !h.is_empty()
        && h.len() <= 253
        && h.split('.').all(|l| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        && !h.rsplit('.').next().unwrap_or("").chars().all(|c| c.is_ascii_digit())
}

/// Validates `host:port` like Camel's `SplunkHECEndpoint` (`^(.*?):(\d+)$`).
fn parse_splunk_url(s: &str) -> Result<(String, u16)> {
    let invalid = || CamelError::illegal_argument(format!("Invalid Splunk URL provided: {}", s));
    let (host, port) = s.rsplit_once(':').ok_or_else(invalid)?;
    if port.is_empty() || !port.chars().all(|c| c.is_ascii_digit()) {
        return Err(invalid());
    }
    if !valid_host(host) {
        return Err(CamelError::illegal_argument(format!("Invalid hostname: {}", host)));
    }
    match port.parse::<u32>() {
        Ok(p) if (1..=65535).contains(&p) => Ok((host.to_string(), p as u16)),
        _ => Err(CamelError::illegal_argument(format!("Invalid port: {}", port))),
    }
}

/// The local host name (Camel's `HostUtils.getLocalHostName()`), `unknown` when not found.
pub fn local_host_name() -> String {
    for var in ["HOSTNAME", "COMPUTERNAME"] {
        if let Ok(v) = std::env::var(var) {
            if !v.trim().is_empty() {
                return v.trim().to_string();
            }
        }
    }
    for file in ["/etc/hostname", "/proc/sys/kernel/hostname"] {
        if let Ok(v) = std::fs::read_to_string(file) {
            if !v.trim().is_empty() {
                return v.trim().to_string();
            }
        }
    }
    "unknown".to_string()
}

impl Component for SplunkHecComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let splunk_url = remaining.trim_start_matches("//").trim_end_matches('/').to_string();
        let token = params.take("token").unwrap_or_default();
        let opt = |params: &mut Parameters, k: &str, d: Option<&str>| params.take(k).or(d.map(String::from));
        let time = match params.take("time") {
            Some(t) => Some(t.trim().parse::<i64>().map_err(|_| CamelError::illegal_argument(format!("Invalid number for option time: {}", t)))?),
            None => None,
        };
        let cfg = SplunkHecConfig {
            splunk_url,
            token,
            index: opt(params, "index", Some("camel")),
            source: opt(params, "source", Some("camel")),
            source_type: opt(params, "sourceType", Some("camel")),
            host: params.take("host").filter(|h| !h.is_empty()),
            splunk_endpoint: params.take("splunkEndpoint").filter(|e| !e.is_empty()).unwrap_or_else(|| DEFAULT_ENDPOINT.into()),
            time,
            https: params.take_bool("https", true)?,
            body_only: params.take_bool("bodyOnly", false)?,
            headers_only: params.take_bool("headersOnly", false)?,
        };
        let refused = || {
            CamelError::illegal_argument(
                "splunk-hec: skipTlsVerify is not supported: Cameleer always verifies the server certificate and its host name. Trust a private Splunk by adding its certificate or CA to the trust store: sslContextParameters=#bean, or camel.ssl.enabled=true with camel.ssl.trustStore=ca.crt",
            )
        };
        if params.take_bool("skipTlsVerify", false)? {
            return Err(refused());
        }
        let explicit = cameleer_tls::camel_support::take_endpoint_ssl(ctx, params, "splunk-hec")?;
        let mut http = HttpClient::new();
        http.connect_timeout = Duration::from_secs(10);
        http.read_timeout = Duration::from_secs(60);
        http.keep_alive = true;
        if cfg.https {
            if let Some(p) = explicit.clone().or(cameleer_tls::camel_support::global_ssl_context_parameters(ctx)?) {
                if p.insecure_skip_verify {
                    return Err(refused());
                }
                http = http.with_tls(cameleer_tls::camel_support::client_config(ctx, Some(&p))?);
            }
        }
        params.take("lazyStartProducer");
        params.take("autowiredEnabled");
        Ok(Arc::new(SplunkHecEndpoint { uri: uri.to_string(), cfg, http: Arc::new(http) }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A `splunk-hec` endpoint.
pub struct SplunkHecEndpoint {
    uri: String,
    cfg: SplunkHecConfig,
    http: Arc<HttpClient>,
}

impl SplunkHecEndpoint {
    /// The configuration.
    pub fn config(&self) -> &SplunkHecConfig {
        &self.cfg
    }
}

impl Endpoint for SplunkHecEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        // checked when the route starts, like Camel's SplunkHECEndpoint.doInit, so a route with
        // `{{env:...}}` placeholders resolves before the values are known
        parse_splunk_url(&self.cfg.splunk_url)?;
        if self.cfg.token.is_empty() {
            return Err(CamelError::illegal_argument("A token must be defined"));
        }
        if !valid_token(&self.cfg.token) {
            return Err(CamelError::illegal_argument("Invalid Splunk HEC token provided"));
        }
        let url = format!("{}://{}{}", if self.cfg.https { "https" } else { "http" }, self.cfg.splunk_url, self.cfg.splunk_endpoint);
        let url = Url::parse(&url)?;
        let host = self.cfg.host.clone().unwrap_or_else(local_host_name);
        Ok(Arc::new(SplunkHecProducer { cfg: self.cfg.clone(), url, host, http: self.http.clone() }))
    }
    fn create_consumer(&self, _processor: Arc<dyn Processor>) -> Result<Arc<dyn cameleer_core::spi::Consumer>> {
        Err(CamelError::new(kinds::UNSUPPORTED_OPERATION, "The splunk-hec component cannot be used as a consumer"))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct SplunkHecProducer {
    cfg: SplunkHecConfig,
    url: Url,
    host: String,
    http: Arc<HttpClient>,
}

impl Service for SplunkHecProducer {}

/// The event body: bytes become text, everything else is sent as JSON as it is.
fn event_body(ex: &Exchange) -> Value {
    match &ex.message.body {
        Value::Bytes(b) => Value::String(String::from_utf8_lossy(b).into_owned()),
        other => other.clone(),
    }
}

fn index_time(ex: &Exchange, default: Option<i64>) -> Result<Option<i64>> {
    match ex.message.headers.get(INDEX_TIME) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => match v {
            Value::Int(i) => Ok(Some(*i)),
            Value::Float(f) => Ok(Some(*f as i64)),
            other => other
                .to_string()
                .trim()
                .parse::<i64>()
                .map(Some)
                .map_err(|_| CamelError::new(kinds::TYPE_CONVERSION, format!("Cannot convert header {} value '{}' to a Long", INDEX_TIME, other))),
        },
    }
}

impl SplunkHecProducer {
    /// The JSON payload of one exchange (Camel's `createPayload`).
    fn payload(&self, ex: &Exchange) -> Result<Value> {
        let mut m = ObjectMap::new();
        if let Some(v) = &self.cfg.source_type {
            m.insert("sourcetype".into(), Value::from(v.as_str()));
        }
        if let Some(v) = &self.cfg.source {
            m.insert("source".into(), Value::from(v.as_str()));
        }
        if let Some(v) = &self.cfg.index {
            m.insert("index".into(), Value::from(v.as_str()));
        }
        m.insert("host".into(), Value::from(self.host.as_str()));
        if let Some(t) = index_time(ex, self.cfg.time)? {
            m.insert("time".into(), Value::Int(t));
        }
        let event = if self.cfg.body_only {
            event_body(ex)
        } else if self.cfg.headers_only {
            ex.message.headers.to_value()
        } else {
            let mut e = ObjectMap::new();
            e.insert("body".into(), event_body(ex));
            e.insert("headers".into(), ex.message.headers.to_value());
            Value::Object(e)
        };
        m.insert("event".into(), event);
        Ok(Value::Object(m))
    }
}

impl Processor for SplunkHecProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let body = self.payload(ex)?.to_json();
        let mut h = HttpHeaders::new();
        h.set("Authorization", format!("Splunk {}", self.cfg.token));
        h.set("Content-Type", "application/json; charset=UTF-8");
        h.set("User-Agent", format!("Cameleer Splunk HEC/{}", env!("CARGO_PKG_VERSION")));
        let r = self.http.execute("POST", &self.url, &h, Some(body.as_bytes()), None)?;
        if r.status != 200 {
            static INIT: std::sync::OnceLock<()> = std::sync::OnceLock::new();
            INIT.get_or_init(|| register_kind_parent(RUNTIME_CAMEL_EXCEPTION, kinds::RUNTIME));
            return Err(CamelError::new(
                RUNTIME_CAMEL_EXCEPTION,
                format!("Splunk HEC request failed: HTTP/1.1 {} {}\n{}", r.status, cameleer_http::http::reason_phrase(r.status), String::from_utf8_lossy(&r.body)),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splunk_url_and_token_checks_follow_camel() {
        assert_eq!(parse_splunk_url("localhost:8088").unwrap(), ("localhost".into(), 8088));
        assert_eq!(parse_splunk_url("10.0.0.5:443").unwrap(), ("10.0.0.5".into(), 443));
        assert_eq!(parse_splunk_url("splunk.example.com:8088").unwrap().0, "splunk.example.com");
        assert!(parse_splunk_url("localhost").unwrap_err().message.contains("Invalid Splunk URL"));
        assert!(parse_splunk_url("localhost:").is_err());
        assert!(parse_splunk_url("bad_host:8088").unwrap_err().message.contains("Invalid hostname"));
        assert!(parse_splunk_url("localhost:0").unwrap_err().message.contains("Invalid port"));
        assert!(parse_splunk_url("localhost:70000").unwrap_err().message.contains("Invalid port"));
        assert!(valid_token("11111111-2222-3333-4444-555555555555"));
        assert!(valid_token("abcdefgh-ijkl-mnop-qrst-uvwxyz012345"));
        assert!(!valid_token("not-a-token"));
        assert!(!valid_token("11111111-2222-3333-4444-55555555555"));
        assert!(!valid_token("11111111-2222-3333-4444-5555555555.5"));
    }
}

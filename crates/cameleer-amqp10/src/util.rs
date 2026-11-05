//! Shared helpers: base64, Azure SAS tokens and connection strings, AMQP urls, endpoint
//! option parsing, error kinds and value conversions between AMQP and Camel.

use std::collections::BTreeMap;
use std::time::Duration;

use cameleer_core::error::{CamelError, Result};
use cameleer_core::util::Parameters;
use cameleer_core::value::Value;

use crate::codec::AmqpValue;
use crate::types::AmqpError;

/// Exception kinds raised by this crate.
pub mod kinds {
    /// Generic AMQP failure (a remote error condition); a subclass of `IOException`.
    pub const AMQP: &str = "AmqpException";
    /// SASL authentication failed.
    pub const AUTHENTICATION_FAILURE: &str = "AuthenticationFailureException";
    /// `amqp:unauthorized-access` (e.g. invalid or missing SAS token).
    pub const UNAUTHORIZED: &str = "SecurityException";
    /// `amqp:not-found`.
    pub const NOT_FOUND: &str = "AmqpNotFoundException";
}

/// Register the exception kind hierarchy (idempotent).
pub fn register_kinds() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        cameleer_core::error::register_kind_parent(kinds::AMQP, "IOException");
        cameleer_core::error::register_kind_parent(kinds::NOT_FOUND, kinds::AMQP);
        cameleer_core::error::register_kind_parent(kinds::AUTHENTICATION_FAILURE, "SecurityException");
        cameleer_core::error::register_kind_parent("SecurityException", "RuntimeException");
    });
}

/// Map a remote AMQP error to a Camel exception.
pub fn amqp_error(e: &AmqpError, what: &str) -> CamelError {
    register_kinds();
    let kind = match e.condition.as_str() {
        crate::types::cond::UNAUTHORIZED_ACCESS => kinds::UNAUTHORIZED,
        crate::types::cond::NOT_FOUND => kinds::NOT_FOUND,
        _ => kinds::AMQP,
    };
    CamelError::new(kind, format!("{}: {}", what, e))
}

/// A plain AMQP failure.
pub fn amqp_failure(msg: impl Into<String>) -> CamelError {
    register_kinds();
    CamelError::new(kinds::AMQP, msg)
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding.
pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Decode standard or url-safe base64 (padding optional, whitespace ignored).
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Seconds since the Unix epoch.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Milliseconds since the Unix epoch.
pub fn now_millis() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Build an Azure Shared Access Signature token
/// (`SharedAccessSignature sr=<uri>&sig=<HMAC-SHA256>&se=<expiry>&skn=<key name>`).
/// The key is used as its UTF-8 bytes (as the Azure SDKs do).
pub fn sas_token(resource_uri: &str, key_name: &str, key: &str, expiry_secs: u64) -> String {
    let sr = cameleer_core::util::form_encode(resource_uri);
    let to_sign = format!("{}\n{}", sr, expiry_secs);
    let sig = base64_encode(&cameleer_tls::crypto::hash::HashAlg::Sha256.hmac(key.as_bytes(), to_sign.as_bytes()));
    format!("SharedAccessSignature sr={}&sig={}&se={}&skn={}", sr, cameleer_core::util::form_encode(&sig), expiry_secs, cameleer_core::util::form_encode(key_name))
}

/// A parsed SAS token.
#[derive(Clone, Debug, PartialEq)]
pub struct SasToken {
    pub resource: String,
    pub signature: String,
    pub expiry: u64,
    pub key_name: String,
}

/// Parse a SAS token (with or without the `SharedAccessSignature ` prefix).
pub fn parse_sas_token(token: &str) -> Option<SasToken> {
    let t = token.trim();
    let t = t.strip_prefix("SharedAccessSignature").unwrap_or(t).trim();
    let mut m = BTreeMap::new();
    for part in t.split('&') {
        let (k, v) = part.split_once('=')?;
        m.insert(k.to_string(), cameleer_core::util::url_decode(v));
    }
    Some(SasToken { resource: m.get("sr")?.clone(), signature: m.get("sig")?.clone(), expiry: m.get("se")?.parse().ok()?, key_name: m.get("skn")?.clone() })
}

/// Verify a SAS token against `key` (signature and expiry).
pub fn verify_sas_token(token: &SasToken, key: &str) -> bool {
    if token.expiry < now_secs() {
        return false;
    }
    let to_sign = format!("{}\n{}", cameleer_core::util::form_encode(&token.resource), token.expiry);
    let sig = cameleer_tls::crypto::hash::HashAlg::Sha256.hmac(key.as_bytes(), to_sign.as_bytes());
    match base64_decode(&token.signature) {
        Some(given) => cameleer_tls::util::ct_eq(&given, &sig),
        None => false,
    }
}

/// An Azure connection string
/// (`Endpoint=sb://ns.servicebus.windows.net/;SharedAccessKeyName=..;SharedAccessKey=..;EntityPath=..`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConnectionString {
    /// Host (fully qualified namespace).
    pub host: String,
    pub port: Option<u16>,
    pub key_name: Option<String>,
    pub key: Option<String>,
    /// A ready SAS token (`SharedAccessSignature=...`).
    pub sas: Option<String>,
    pub entity_path: Option<String>,
    /// `UseDevelopmentEmulator=true`: plain AMQP (port 5672), as the Azure emulators.
    pub emulator: bool,
}

/// Parse an Azure connection string. Spaces in the key (a `+` decoded from an unencoded
/// endpoint uri) are turned back into `+`.
pub fn parse_connection_string(s: &str) -> Result<ConnectionString> {
    let mut cs = ConnectionString::default();
    for part in s.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (k, v) = part.split_once('=').ok_or_else(|| CamelError::illegal_argument(format!("Invalid connection string part: {}", part)))?;
        match k.trim().to_ascii_lowercase().as_str() {
            "endpoint" => {
                let v = v.trim();
                let rest = v.split_once("://").map(|(_, r)| r).unwrap_or(v);
                let hostport = rest.split('/').next().unwrap_or("");
                match hostport.rsplit_once(':') {
                    Some((h, p)) if !h.contains(']') || h.ends_with(']') => {
                        cs.host = h.trim_matches(['[', ']']).to_string();
                        cs.port = Some(p.parse().map_err(|_| CamelError::illegal_argument(format!("Invalid port in endpoint: {}", v)))?);
                    }
                    _ => cs.host = hostport.to_string(),
                }
            }
            "sharedaccesskeyname" => cs.key_name = Some(v.trim().to_string()),
            "sharedaccesskey" => cs.key = Some(v.trim().replace(' ', "+")),
            "sharedaccesssignature" => cs.sas = Some(v.trim().to_string()),
            "entitypath" => cs.entity_path = Some(v.trim().to_string()).filter(|s| !s.is_empty()),
            "usedevelopmentemulator" => cs.emulator = v.trim().eq_ignore_ascii_case("true"),
            _ => {}
        }
    }
    if cs.host.is_empty() {
        return Err(CamelError::illegal_argument("Connection string has no Endpoint"));
    }
    Ok(cs)
}

/// A parsed `amqp://` / `amqps://` url.
#[derive(Clone, Debug, PartialEq)]
pub struct AmqpUrl {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    /// `amqpws://` / `amqpwss://`: AMQP over WebSockets (path in `ws_path`).
    pub websocket: bool,
    /// The WebSocket path (the url path, `/` by default).
    pub ws_path: String,
}

/// Parse `amqp[s]://[user:pass@]host[:port]` (also `failover:(...)` - first address - and
/// bare `host:port`). Default ports 5672 / 5671.
pub fn parse_amqp_url(url: &str) -> Result<AmqpUrl> {
    let mut u = url.trim();
    if let Some(rest) = u.strip_prefix("failover:") {
        u = rest.trim_start_matches('(');
        if let Some(i) = u.find([')', ',']) {
            u = &u[..i];
        }
    }
    if let Some(i) = u.find('?') {
        u = &u[..i];
    }
    let (scheme, rest) = match u.split_once("://") {
        Some((s, r)) => (s.to_ascii_lowercase(), r),
        None => ("amqp".to_string(), u),
    };
    let tls = matches!(scheme.as_str(), "amqps" | "amqpwss" | "ssl" | "tls");
    let websocket = matches!(scheme.as_str(), "amqpws" | "amqpwss");
    let ws_path = match rest.find('/') {
        Some(i) => rest[i..].to_string(),
        None => "/".to_string(),
    };
    let rest = rest.split('/').next().unwrap_or("");
    let (userinfo, hostport) = match rest.rsplit_once('@') {
        Some((ui, hp)) => (Some(ui), hp),
        None => (None, rest),
    };
    let (username, password) = match userinfo {
        Some(ui) => match ui.split_once(':') {
            Some((a, b)) => (Some(cameleer_core::util::url_decode(a)), Some(cameleer_core::util::url_decode(b))),
            None => (Some(cameleer_core::util::url_decode(ui)), None),
        },
        None => (None, None),
    };
    let default_port = match (websocket, tls) {
        (true, true) => 443,
        (true, false) => 80,
        (false, true) => 5671,
        (false, false) => 5672,
    };
    let bad = || CamelError::illegal_argument(format!("Invalid AMQP url: {}", url));
    let (host, port) = if let Some(r) = hostport.strip_prefix('[') {
        let end = r.find(']').ok_or_else(bad)?;
        let port = match r[end + 1..].strip_prefix(':') {
            Some(p) => p.parse().map_err(|_| bad())?,
            None => default_port,
        };
        (r[..end].to_string(), port)
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.parse().map_err(|_| bad())?),
            None => (hostport.to_string(), default_port),
        }
    };
    let host = if host.is_empty() { "localhost".to_string() } else { host };
    Ok(AmqpUrl { tls, host, port, username, password, websocket, ws_path })
}

/// Lookup of component-level properties by option name.
pub type ComponentLookup<'a> = Box<dyn Fn(&str) -> Option<String> + 'a>;

/// Option parsing helpers over endpoint parameters with a component-level fallback.
pub struct Opts<'a> {
    pub params: &'a mut Parameters,
    pub comp: ComponentLookup<'a>,
}

impl<'a> Opts<'a> {
    pub fn new(ctx: &'a cameleer_core::context::CamelContext, scheme: &'a str, params: &'a mut Parameters) -> Opts<'a> {
        let props = ctx.properties();
        Opts { params, comp: Box::new(move |k: &str| props.get(&format!("camel.component.{}.{}", scheme, k))) }
    }
    /// The endpoint option, else the component property.
    pub fn get(&mut self, key: &str) -> Option<String> {
        self.params.take(key).or_else(|| (self.comp)(key))
    }
    pub fn string(&mut self, key: &str) -> Option<String> {
        self.get(key).filter(|s| !s.trim().is_empty())
    }
    pub fn bool(&mut self, key: &str, default: bool) -> Result<bool> {
        match self.get(key).as_deref().map(|s| s.trim().to_ascii_lowercase()) {
            None => Ok(default),
            Some(s) if s == "true" => Ok(true),
            Some(s) if s == "false" => Ok(false),
            Some(o) => Err(CamelError::illegal_argument(format!("Invalid boolean for {}: {}", key, o))),
        }
    }
    pub fn num<T: std::str::FromStr>(&mut self, key: &str, default: T) -> Result<T> {
        match self.get(key) {
            None => Ok(default),
            Some(s) => s.trim().parse().map_err(|_| CamelError::illegal_argument(format!("Invalid number for {}: {}", key, s))),
        }
    }
    /// Milliseconds (a number) or a duration (`5s`).
    pub fn millis(&mut self, key: &str, default: Duration) -> Result<Duration> {
        match self.get(key) {
            None => Ok(default),
            Some(s) => match s.trim().parse::<i64>() {
                Ok(n) => Ok(Duration::from_millis(n.max(0) as u64)),
                Err(_) => cameleer_core::util::parse_duration(s.trim()).map_err(|_| CamelError::illegal_argument(format!("Invalid duration for {}: {}", key, s))),
            },
        }
    }
    /// Remove accepted-but-ignored options.
    pub fn ignore(&mut self, keys: &[&str]) {
        for k in keys {
            self.params.take(k);
        }
    }
}

/// Options accepted by every endpoint and ignored.
pub const COMMON_IGNORED: &[&str] = &["lazyStartProducer", "bridgeErrorHandler", "exceptionHandler", "exchangePattern", "synchronous", "autoStartup"];

/// Convert an AMQP value to a Camel value.
pub fn to_camel(v: &AmqpValue) -> Value {
    use AmqpValue as A;
    match v {
        A::Null => Value::Null,
        A::Boolean(b) => Value::Bool(*b),
        A::Ubyte(_) | A::Ushort(_) | A::Uint(_) | A::Byte(_) | A::Short(_) | A::Int(_) | A::Long(_) | A::Timestamp(_) => Value::Int(v.as_i64().unwrap_or(0)),
        A::Ulong(n) => {
            if *n <= i64::MAX as u64 {
                Value::Int(*n as i64)
            } else {
                Value::String(n.to_string())
            }
        }
        A::Float(f) => Value::Float(*f as f64),
        A::Double(f) => Value::Float(*f),
        A::Decimal32(_) | A::Decimal64(_) | A::Decimal128(_) => Value::String(format!("{:?}", v)),
        A::Char(c) => Value::String(c.to_string()),
        A::Uuid(u) => Value::String(crate::message::format_uuid(u)),
        A::Binary(b) => Value::Bytes(b.clone()),
        A::String(s) | A::Symbol(s) => Value::String(s.clone()),
        A::List(l) | A::Array(l) => Value::Array(l.iter().map(to_camel).collect()),
        A::Map(m) => Value::Object(m.iter().map(|(k, v)| (k.as_str().map(String::from).unwrap_or_else(|| to_camel(k).to_string()), to_camel(v))).collect()),
        A::Described(_, v) => to_camel(v),
    }
}

/// Convert a Camel value to an AMQP value (objects become maps with string keys).
pub fn to_amqp(v: &Value) -> AmqpValue {
    match v {
        Value::Null => AmqpValue::Null,
        Value::Bool(b) => AmqpValue::Boolean(*b),
        Value::Int(i) => AmqpValue::Long(*i),
        Value::Float(f) => AmqpValue::Double(*f),
        Value::String(s) => AmqpValue::String(s.clone()),
        Value::Bytes(b) => AmqpValue::Binary(b.clone()),
        Value::Array(a) => AmqpValue::List(a.iter().map(to_amqp).collect()),
        Value::Object(o) => AmqpValue::Map(o.iter().map(|(k, v)| (AmqpValue::String(k.clone()), to_amqp(v))).collect()),
    }
}

/// Convert a Camel value to a "simple" AMQP value suitable for application-properties
/// (no maps/lists: those are rendered as JSON text).
pub fn to_amqp_simple(v: &Value) -> AmqpValue {
    match v {
        Value::Array(_) | Value::Object(_) => AmqpValue::String(v.to_json()),
        other => to_amqp(other),
    }
}

/// A process-unique, hard to guess hex id.
pub fn unique_id() -> String {
    crate::message::random_uuid().iter().map(|b| format!("{:02x}", b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_decode("Zm9vYmE=").unwrap(), b"fooba");
        assert_eq!(base64_decode("Zm9vYmE").unwrap(), b"fooba");
        assert!(base64_decode("@@").is_none());
    }

    #[test]
    fn sas_tokens() {
        // known answer computed with the Azure SDK algorithm:
        // HMAC-SHA256(key, urlencode(uri) + "\n" + expiry), base64, urlencoded
        let t = sas_token("sb://ns.servicebus.windows.net/q", "RootManageSharedAccessKey", "secret", 1_700_000_000);
        assert!(t.starts_with("SharedAccessSignature sr=sb%3A%2F%2Fns.servicebus.windows.net%2Fq&sig="));
        assert!(t.ends_with("&se=1700000000&skn=RootManageSharedAccessKey"));
        let p = parse_sas_token(&t).unwrap();
        assert_eq!(p.resource, "sb://ns.servicebus.windows.net/q");
        let sig = base64_decode(&p.signature).unwrap();
        let expect = cameleer_tls::crypto::hash::HashAlg::Sha256.hmac(b"secret", b"sb%3A%2F%2Fns.servicebus.windows.net%2Fq\n1700000000");
        assert_eq!(sig, expect);
        // expired
        assert!(!verify_sas_token(&p, "secret"));
        let fresh = parse_sas_token(&sas_token("sb://h/q", "k", "secret", now_secs() + 60)).unwrap();
        assert!(verify_sas_token(&fresh, "secret"));
        assert!(!verify_sas_token(&fresh, "wrong"));
    }

    #[test]
    fn connection_strings_and_urls() {
        let cs = parse_connection_string("Endpoint=sb://ns.servicebus.windows.net/;SharedAccessKeyName=Root;SharedAccessKey=a b=;EntityPath=hub").unwrap();
        assert_eq!(cs.host, "ns.servicebus.windows.net");
        assert_eq!(cs.key.as_deref(), Some("a+b="));
        assert_eq!(cs.entity_path.as_deref(), Some("hub"));
        let cs = parse_connection_string("Endpoint=sb://127.0.0.1:5673;SharedAccessKeyName=k;SharedAccessKey=v;UseDevelopmentEmulator=true;").unwrap();
        assert_eq!((cs.host.as_str(), cs.port, cs.emulator), ("127.0.0.1", Some(5673), true));
        assert!(parse_connection_string("SharedAccessKey=x").is_err());
        let u = parse_amqp_url("amqps://u:p%40@broker:1234").unwrap();
        assert_eq!(u, AmqpUrl { tls: true, host: "broker".into(), port: 1234, username: Some("u".into()), password: Some("p@".into()), websocket: false, ws_path: "/".into() });
        let w = parse_amqp_url("amqpwss://ns.servicebus.windows.net/$servicebus/websocket").unwrap();
        assert!(w.websocket && w.tls && w.port == 443 && w.ws_path == "/$servicebus/websocket");
        assert_eq!(parse_amqp_url("amqp://h").unwrap().port, 5672);
        assert_eq!(parse_amqp_url("failover:(amqp://a:1,amqp://b:2)").unwrap().host, "a");
        assert_eq!(parse_amqp_url("amqp://[::1]:9").unwrap().host, "::1");
    }
}

//! Helpers shared by the connectors: address parsing, header access, exchange
//! snapshots for repositories and a small threaded TCP server for the embedded test
//! servers.

use std::collections::HashMap;
use std::io;
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use camel_core::context::CamelContext;
use camel_core::error::{CamelError, Result};
use camel_core::message::{Exchange, ExchangePattern};
use camel_core::value::{ObjectMap, Value};
use camel_tls::net::NetStream;
use camel_tls::pem::{base64_decode, base64_encode};
use camel_tls::TlsServerConfig;

/// Splits `host[:port]` (IPv6 in brackets) using `default_port` when no port is given.
pub fn split_host_port(s: &str, default_port: u16) -> Result<(String, u16)> {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix('[') {
        let (h, tail) = rest.split_once(']').ok_or_else(|| CamelError::illegal_argument(format!("Invalid address: {}", s)))?;
        let port = match tail.strip_prefix(':') {
            Some(p) => p.parse().map_err(|_| CamelError::illegal_argument(format!("Invalid port in {}", s)))?,
            None => default_port,
        };
        return Ok((h.to_string(), port));
    }
    match s.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => {
            let port = p.parse().map_err(|_| CamelError::illegal_argument(format!("Invalid port in {}", s)))?;
            Ok((if h.is_empty() { "localhost".to_string() } else { h.to_string() }, port))
        }
        _ => Ok((if s.is_empty() { "localhost".to_string() } else { s.to_string() }, default_port)),
    }
}

/// A non-null header.
pub fn header<'a>(ex: &'a Exchange, name: &str) -> Option<&'a Value> {
    ex.header(name).filter(|v| !v.is_null())
}

/// A non-null header as text.
pub fn header_str(ex: &Exchange, name: &str) -> Option<String> {
    header(ex, name).map(|v| v.to_string())
}

/// A header as an integer (numbers or numeric text).
pub fn header_i64(ex: &Exchange, name: &str) -> Result<Option<i64>> {
    match header(ex, name) {
        None => Ok(None),
        Some(v) => v
            .as_i64()
            .or_else(|| v.to_string().trim().parse().ok())
            .map(Some)
            .ok_or_else(|| CamelError::illegal_argument(format!("Header {} is not a number: {}", name, v))),
    }
}

/// A header as a boolean.
pub fn header_bool(ex: &Exchange, name: &str) -> Option<bool> {
    header(ex, name).map(|v| v.as_bool().unwrap_or_else(|| v.to_string().trim().eq_ignore_ascii_case("true")))
}

/// Process-unique counter (for ids).
pub fn next_id() -> u64 {
    static N: AtomicU64 = AtomicU64::new(1);
    N.fetch_add(1, Ordering::Relaxed)
}

/// Milliseconds since the Unix epoch.
pub fn now_millis() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Lower-case hex.
pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

/// Parses hex text.
pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// Percent-decodes URI text.
pub fn percent_decode(s: &str) -> String {
    camel_core::util::url_decode(s)
}

// ---------------------------------------------------------------------------
// exchange snapshots
// ---------------------------------------------------------------------------

fn enc(v: &Value) -> Value {
    match v {
        Value::Bytes(b) => Value::object([("$bytes", Value::String(base64_encode(b)))]),
        Value::Array(a) => Value::Array(a.iter().map(enc).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), enc(v))).collect()),
        other => other.clone(),
    }
}

fn dec(v: Value) -> Value {
    match v {
        Value::Object(o) => {
            if o.len() == 1 {
                if let Some(Value::String(s)) = o.get("$bytes") {
                    if let Some(b) = base64_decode(s) {
                        return Value::Bytes(b);
                    }
                }
            }
            Value::Object(o.into_iter().map(|(k, v)| (k, dec(v))).collect())
        }
        Value::Array(a) => Value::Array(a.into_iter().map(dec).collect()),
        other => other,
    }
}

/// Serializes an exchange snapshot (id, pattern, body, headers, properties) as JSON
/// (same layout as the JDBC aggregation repository of `camel-sql`).
pub fn exchange_to_json(ex: &Exchange) -> String {
    let headers: ObjectMap = ex.message.headers.iter().map(|(k, v)| (k.to_string(), enc(v))).collect();
    let props: ObjectMap = ex.properties.iter().map(|(k, v)| (k.clone(), enc(v))).collect();
    let mut m = ObjectMap::new();
    m.insert("exchangeId".to_string(), Value::String(ex.exchange_id.clone()));
    m.insert("pattern".to_string(), Value::String(format!("{:?}", ex.pattern)));
    m.insert("body".to_string(), enc(&ex.message.body));
    m.insert("headers".to_string(), Value::Object(headers));
    m.insert("properties".to_string(), Value::Object(props));
    if let Some(f) = &ex.from_endpoint {
        m.insert("fromEndpoint".to_string(), Value::String(f.clone()));
    }
    if let Some(r) = &ex.from_route_id {
        m.insert("fromRouteId".to_string(), Value::String(r.clone()));
    }
    Value::Object(m).to_json()
}

/// Restores an exchange from [`exchange_to_json`] output.
pub fn exchange_from_json(text: &str, ctx: Option<CamelContext>) -> Result<Exchange> {
    let Value::Object(mut m) = camel_core::json::parse(text)? else {
        return Err(CamelError::new(camel_core::error::kinds::TYPE_CONVERSION, "Stored exchange is not a JSON object"));
    };
    let mut ex = Exchange::new(ctx);
    if let Some(Value::String(id)) = m.remove("exchangeId") {
        ex.exchange_id = id;
    }
    if let Some(Value::String(p)) = m.remove("pattern") {
        ex.pattern = ExchangePattern::parse(&p).unwrap_or_default();
    }
    ex.message.body = dec(m.remove("body").unwrap_or(Value::Null));
    if let Some(Value::Object(h)) = m.remove("headers") {
        for (k, v) in h {
            ex.message.headers.set(k, dec(v));
        }
    }
    if let Some(Value::Object(p)) = m.remove("properties") {
        for (k, v) in p {
            ex.properties.insert(k, dec(v));
        }
    }
    if let Some(Value::String(f)) = m.remove("fromEndpoint") {
        ex.from_endpoint = Some(f);
    }
    if let Some(Value::String(r)) = m.remove("fromRouteId") {
        ex.from_route_id = Some(r);
    }
    Ok(ex)
}

/// Converts a body to a JSON-like value: JSON text and bytes are parsed, other values are kept.
pub fn body_to_json_value(v: &Value) -> Result<Value> {
    match v {
        Value::String(s) => camel_core::json::parse(s),
        Value::Bytes(b) => camel_core::json::parse(&String::from_utf8_lossy(b)),
        other => Ok(other.clone()),
    }
}

// ---------------------------------------------------------------------------
// embedded TCP server
// ---------------------------------------------------------------------------

struct ServerShared {
    running: AtomicBool,
    conns: Mutex<HashMap<u64, TcpStream>>,
}

/// A thread-per-connection TCP server (plain or TLS) used by the embedded test servers.
/// Dropping it stops it.
pub struct TcpServer {
    shared: Arc<ServerShared>,
    addr: SocketAddr,
    accept: Mutex<Option<JoinHandle<()>>>,
}

impl TcpServer {
    /// Binds `127.0.0.1:0` and serves each connection with `handler` on its own thread.
    pub fn start(name: &str, tls: Option<Arc<TlsServerConfig>>, handler: impl Fn(NetStream) + Send + Sync + 'static) -> io::Result<TcpServer> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let shared = Arc::new(ServerShared { running: AtomicBool::new(true), conns: Mutex::new(HashMap::new()) });
        let handler = Arc::new(handler);
        let sh = shared.clone();
        let tname = name.to_string();
        let accept = std::thread::Builder::new().name(format!("{}-accept", name)).spawn(move || {
            for conn in listener.incoming() {
                if !sh.running.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(tcp) = conn else { continue };
                let id = next_id();
                if let Ok(c) = tcp.try_clone() {
                    sh.conns.lock().unwrap().insert(id, c);
                }
                let (h, sh2, tls) = (handler.clone(), sh.clone(), tls.clone());
                let _ = std::thread::Builder::new().name(format!("{}-conn", tname)).spawn(move || {
                    let _ = tcp.set_nodelay(true);
                    let stream = match &tls {
                        Some(cfg) => NetStream::server(tcp, cfg),
                        None => Ok(NetStream::plain(tcp)),
                    };
                    if let Ok(s) = stream {
                        h(s);
                    }
                    if let Some(c) = sh2.conns.lock().unwrap().remove(&id) {
                        let _ = c.shutdown(Shutdown::Both);
                    }
                });
            }
        })?;
        Ok(TcpServer { shared, addr, accept: Mutex::new(Some(accept)) })
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Number of open client connections.
    pub fn connection_count(&self) -> usize {
        self.shared.conns.lock().unwrap().len()
    }

    /// Stops accepting and closes all connections.
    pub fn stop(&self) {
        if !self.shared.running.swap(false, Ordering::SeqCst) {
            return;
        }
        let _ = TcpStream::connect(self.addr);
        if let Some(h) = self.accept.lock().unwrap().take() {
            let _ = h.join();
        }
        for (_, c) in self.shared.conns.lock().unwrap().drain() {
            let _ = c.shutdown(Shutdown::Both);
        }
    }
}

impl Drop for TcpServer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_port() {
        assert_eq!(split_host_port("h:1", 2).unwrap(), ("h".to_string(), 1));
        assert_eq!(split_host_port("h", 2).unwrap(), ("h".to_string(), 2));
        assert_eq!(split_host_port("[::1]:5", 2).unwrap(), ("::1".to_string(), 5));
        assert!(split_host_port("h:x", 2).is_err());
        assert_eq!(unhex(&hex(&[1, 0xab])).unwrap(), vec![1, 0xab]);
    }

    #[test]
    fn snapshot_round_trip() {
        let mut ex = Exchange::new(None);
        ex.set_body(Value::Bytes(vec![1, 2]));
        ex.set_header("a", 1);
        let back = exchange_from_json(&exchange_to_json(&ex), None).unwrap();
        assert_eq!(back.body(), &Value::Bytes(vec![1, 2]));
        assert_eq!(back.exchange_id, ex.exchange_id);
    }
}

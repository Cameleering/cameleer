//! Shared helpers: host/port and broker url parsing, connecting with timeouts, a small
//! threaded TCP server (for the embedded brokers), unique ids and clock helpers.

use std::collections::hash_map::RandomState;
use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};
use std::io::ErrorKind;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use camel_core::error::{kinds, CamelError, Result};
use camel_core::util::Parameters;

/// Split `host[:port]` (IPv6 hosts in brackets). An empty host means `localhost`.
pub fn split_host_port(s: &str, default_port: u16) -> Result<(String, u16)> {
    let bad = || CamelError::illegal_argument(format!("Invalid host:port: {}", s));
    let s = s.trim().trim_end_matches('/');
    if let Some(rest) = s.strip_prefix('[') {
        let end = rest.find(']').ok_or_else(bad)?;
        let host = rest[..end].to_string();
        let after = &rest[end + 1..];
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse().map_err(|_| bad())?,
            None if after.is_empty() => default_port,
            None => return Err(bad()),
        };
        return Ok((host, port));
    }
    match s.rfind(':') {
        Some(i) => {
            let port = s[i + 1..].parse().map_err(|_| bad())?;
            let host = if i == 0 { "localhost".to_string() } else { s[..i].to_string() };
            Ok((host, port))
        }
        None if s.is_empty() => Ok(("localhost".to_string(), default_port)),
        None => Ok((s.to_string(), default_port)),
    }
}

/// Whether a broker url selects TLS: `ssl://`, `amqps://`, `stomp+ssl://`, `stomp+tls://`,
/// `stomps://`, `tls://`, `nio+ssl://` (also as the first address of a `failover:(...)` list).
pub fn broker_url_is_tls(url: &str) -> bool {
    let mut u = url.trim();
    if let Some(rest) = u.strip_prefix("failover:").or_else(|| u.strip_prefix("discovery:")) {
        u = rest.trim_start_matches('(');
    }
    match u.find("://") {
        Some(i) => {
            let scheme = u[..i].to_ascii_lowercase();
            matches!(scheme.as_str(), "ssl" | "tls" | "amqps" | "stomps" | "stomp+ssl" | "stomp+tls" | "ssl+stomp" | "nio+ssl" | "mqtt+ssl")
        }
        None => false,
    }
}

/// Parse a broker url such as `tcp://host:61613`, `stomp://host:port`, `nio://...`,
/// `amqp://host:port`, `failover:(tcp://a:1,tcp://b:2)?opts` (the first address is used)
/// or a bare `host:port`. Returns `(host, port)`.
pub fn parse_broker_url(url: &str, default_port: u16) -> Result<(String, u16)> {
    let mut u = url.trim();
    if let Some(rest) = u.strip_prefix("failover:").or_else(|| u.strip_prefix("discovery:")) {
        u = rest.trim_start_matches('(');
        if let Some(i) = u.find([')', ',']) {
            u = &u[..i];
        }
    }
    if let Some(i) = u.find('?') {
        u = &u[..i];
    }
    if let Some(i) = u.find("://") {
        u = &u[i + 3..];
    }
    // strip user info and path
    if let Some(i) = u.rfind('@') {
        u = &u[i + 1..];
    }
    if let Some(i) = u.find('/') {
        u = &u[..i];
    }
    split_host_port(u, default_port)
}

/// `host:port` suitable for `bind`/`connect` (brackets IPv6 literals).
pub fn host_port(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{}]:{}", host, port)
    } else {
        format!("{}:{}", host, port)
    }
}

/// Map an I/O error to a Camel exception (`ConnectException`, `SocketTimeoutException`, `IOException`).
pub fn io_error(e: std::io::Error, what: &str) -> CamelError {
    match e.kind() {
        ErrorKind::ConnectionRefused => CamelError::new(kinds::CONNECT, format!("Connection refused: {}", what)),
        ErrorKind::TimedOut | ErrorKind::WouldBlock => CamelError::new(kinds::SOCKET_TIMEOUT, format!("Read timed out: {}", what)),
        _ => CamelError::io(format!("{}: {}", e, what)),
    }
}

/// Connect to `host:port` with a timeout, trying each resolved address.
pub fn connect(host: &str, port: u16, timeout: Duration) -> Result<TcpStream> {
    let what = host_port(host, port);
    let host = if host == "0.0.0.0" || host.is_empty() { "127.0.0.1" } else { host };
    let addrs: Vec<SocketAddr> = (host, port).to_socket_addrs().map_err(|e| io_error(e, &what))?.collect();
    let mut last = None;
    for a in addrs {
        let r = if timeout.is_zero() { TcpStream::connect(a) } else { TcpStream::connect_timeout(&a, timeout) };
        match r {
            Ok(s) => {
                let _ = s.set_nodelay(true);
                return Ok(s);
            }
            Err(e) => last = Some(e),
        }
    }
    Err(match last {
        Some(e) if e.kind() == ErrorKind::TimedOut => CamelError::new(kinds::CONNECT, format!("Connect timed out: {}", what)),
        Some(e) => io_error(e, &what),
        None => CamelError::new(kinds::CONNECT, format!("Unknown host: {}", host)),
    })
}

/// A process-unique, hard to guess id (hex), e.g. for correlation ids and temp queues.
pub fn unique_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut h = RandomState::new().build_hasher();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    h.write_u64(n);
    h.write_u128(now_nanos());
    format!("{:016x}{:04x}", h.finish(), n & 0xffff)
}

fn now_nanos() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}

/// Milliseconds since the Unix epoch.
pub fn now_millis() -> i64 {
    (now_nanos() / 1_000_000) as i64
}

/// Remove options that are accepted for compatibility but have no effect.
pub fn take_ignored(params: &mut Parameters, keys: &[&str]) {
    for k in keys {
        params.take(k);
    }
}

/// Common accepted-and-ignored endpoint options.
pub const COMMON_IGNORED: &[&str] = &["lazyStartProducer", "bridgeErrorHandler", "exceptionHandler", "exchangePattern", "synchronous", "autoStartup"];

type Handler = Arc<dyn Fn(TcpStream, u64) + Send + Sync>;

/// A small threaded TCP server: one accept thread plus one thread per connection.
/// `stop()` closes the listener, shuts down all open connections and joins the threads.
pub struct TcpServer {
    local: SocketAddr,
    running: Arc<AtomicBool>,
    accept: Mutex<Option<JoinHandle<()>>>,
    conns: Arc<Mutex<HashMap<u64, TcpStream>>>,
    workers: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl TcpServer {
    /// Bind `host:port` (port 0 = ephemeral) and serve each connection with
    /// `handler(stream, connection_id)`.
    pub fn bind(host: &str, port: u16, name: &str, handler: impl Fn(TcpStream, u64) + Send + Sync + 'static) -> Result<TcpServer> {
        let addr = host_port(host, port);
        let listener = camel_tls::net::bind_listener(&addr).map_err(|e| CamelError::io(format!("Failed to bind {} on {}: {}", name, addr, e)))?;
        let local = listener.local_addr().map_err(|e| CamelError::io(e.to_string()))?;
        listener.set_nonblocking(true).map_err(|e| CamelError::io(e.to_string()))?;
        let running = Arc::new(AtomicBool::new(true));
        let conns: Arc<Mutex<HashMap<u64, TcpStream>>> = Arc::new(Mutex::new(HashMap::new()));
        let workers: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));
        let handler: Handler = Arc::new(handler);
        let (run, c2, w2, tname) = (running.clone(), conns.clone(), workers.clone(), name.to_string());
        let accept = std::thread::Builder::new()
            .name(format!("{}-accept-{}", name, local.port()))
            .spawn(move || {
                let ids = AtomicU64::new(1);
                while run.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((s, _)) => {
                            let _ = s.set_nonblocking(false);
                            let _ = s.set_nodelay(true);
                            let id = ids.fetch_add(1, Ordering::Relaxed);
                            if let Ok(c) = s.try_clone() {
                                c2.lock().unwrap().insert(id, c);
                            }
                            let (h, c3) = (handler.clone(), c2.clone());
                            let spawned = std::thread::Builder::new().name(format!("{}-conn-{}", tname, id)).spawn(move || {
                                h(s, id);
                                c3.lock().unwrap().remove(&id);
                            });
                            if let Ok(j) = spawned {
                                let mut w = w2.lock().unwrap();
                                w.retain(|j| !j.is_finished());
                                w.push(j);
                            }
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(5)),
                    }
                }
            })
            .map_err(|e| CamelError::io(e.to_string()))?;
        Ok(TcpServer { local, running, accept: Mutex::new(Some(accept)), conns, workers })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    pub fn port(&self) -> u16 {
        self.local.port()
    }

    /// Number of currently open connections.
    pub fn connection_count(&self) -> usize {
        self.conns.lock().unwrap().len()
    }

    /// Forcibly close every open connection (the server keeps accepting).
    pub fn drop_connections(&self) {
        for s in self.conns.lock().unwrap().values() {
            let _ = camel_tls::net::shutdown_socket(s, std::net::Shutdown::Both);
        }
    }

    /// Stop accepting, close all connections and join the threads.
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(a) = self.accept.lock().unwrap().take() {
            let _ = a.join();
        }
        self.drop_connections();
        let me = std::thread::current().id();
        let workers: Vec<JoinHandle<()>> = std::mem::take(&mut *self.workers.lock().unwrap());
        for w in workers {
            if w.thread().id() != me {
                let _ = w.join();
            }
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
    fn broker_urls() {
        assert_eq!(parse_broker_url("tcp://localhost:61613", 1).unwrap(), ("localhost".into(), 61613));
        assert_eq!(parse_broker_url("failover:(tcp://a:1,tcp://b:2)?randomize=false", 9).unwrap(), ("a".into(), 1));
        assert_eq!(parse_broker_url("amqp://guest:guest@rabbit/vhost", 5672).unwrap(), ("rabbit".into(), 5672));
        assert_eq!(parse_broker_url("127.0.0.1:99", 1).unwrap(), ("127.0.0.1".into(), 99));
        assert_eq!(parse_broker_url("tcp://[::1]:7", 1).unwrap(), ("::1".into(), 7));
        assert!(parse_broker_url("tcp://h:x", 1).is_err());
    }

    #[test]
    fn ids_are_unique() {
        assert_ne!(unique_id(), unique_id());
    }
}

//! Shared networking helpers: host/port parsing, connecting with timeouts, a small
//! threaded TCP server, unique ids and lenient percent decoding.

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

/// Split `host[:port]` (IPv6 hosts in brackets). An empty host means `localhost`.
pub fn split_host_port(s: &str, default_port: u16) -> Result<(String, u16)> {
    let s = s.trim();
    let bad = || CamelError::illegal_argument(format!("Invalid host:port: {}", s));
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

/// A process-unique, pseudo random hex id (not cryptographically secure).
pub fn unique_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut h = RandomState::new().build_hasher();
    h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    format!("{:016x}", h.finish())
}

/// Percent-decode (`%XX`) without turning `+` into a space (MQTT wildcards).
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Some(v) = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Current time in epoch milliseconds.
pub fn now_millis() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

type Handler = Arc<dyn Fn(TcpStream) + Send + Sync>;

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
    /// Bind `host:port` (port 0 = ephemeral) and serve each connection with `handler`.
    pub fn bind(host: &str, port: u16, name: &str, handler: impl Fn(TcpStream) + Send + Sync + 'static) -> Result<TcpServer> {
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
                                h(s);
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

    /// The bound address.
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    /// The bound port.
    pub fn port(&self) -> u16 {
        self.local.port()
    }

    /// Number of currently open connections.
    pub fn connection_count(&self) -> usize {
        self.conns.lock().unwrap().len()
    }

    /// Stop accepting, close all connections and join the threads.
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(a) = self.accept.lock().unwrap().take() {
            let _ = a.join();
        }
        for s in self.conns.lock().unwrap().values() {
            let _ = camel_tls::net::shutdown_socket(s, std::net::Shutdown::Both);
        }
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
    fn host_port_parsing() {
        assert_eq!(split_host_port("h:9092", 1).unwrap(), ("h".to_string(), 9092));
        assert_eq!(split_host_port("h", 1883).unwrap(), ("h".to_string(), 1883));
        assert_eq!(split_host_port("[::1]:5", 1).unwrap(), ("::1".to_string(), 5));
        assert!(split_host_port("h:x", 1).is_err());
        assert_eq!(percent_decode("a%2Fb+c%"), "a/b+c%");
        assert_ne!(unique_id(), unique_id());
    }
}

//! Shared networking helpers: URI authority parsing, connecting with timeouts, a small
//! threaded TCP server, CRLF line reading and pseudo random bytes.

use std::collections::hash_map::RandomState;
use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};
use std::io::{BufRead, BufReader, ErrorKind};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use cameleer_core::error::{kinds, CamelError, Result};
use cameleer_core::util::{url_decode, Parameters};
use cameleer_tls::camel_support::{io_to_camel, EndpointTls};
use cameleer_tls::net::NetStream;

/// `[user[:password]@]host[:port][/path]` split into its parts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Authority {
    pub user: Option<String>,
    pub password: Option<String>,
    pub host: String,
    pub port: u16,
    /// Path without the leading `/` (may be empty).
    pub path: String,
}

impl Authority {
    /// Parse the remaining part of an endpoint uri (after `scheme://`).
    pub fn parse(remaining: &str, default_port: u16) -> Result<Authority> {
        let (auth, path) = match remaining.find('/') {
            Some(i) => (&remaining[..i], &remaining[i + 1..]),
            None => (remaining, ""),
        };
        let (userinfo, hostport) = match auth.rfind('@') {
            Some(i) => (Some(&auth[..i]), &auth[i + 1..]),
            None => (None, auth),
        };
        let (user, password) = match userinfo {
            None => (None, None),
            Some(u) => match u.find(':') {
                Some(i) => (Some(url_decode(&u[..i])), Some(url_decode(&u[i + 1..]))),
                None => (Some(url_decode(u)), None),
            },
        };
        let (host, port) = split_host_port(hostport, default_port)?;
        Ok(Authority { user: user.filter(|u| !u.is_empty()), password, host, port, path: path.to_string() })
    }
}

/// Split `host[:port]` (IPv6 hosts in brackets). An empty host means `0.0.0.0`.
pub fn split_host_port(s: &str, default_port: u16) -> Result<(String, u16)> {
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
            let host = if i == 0 { "0.0.0.0".to_string() } else { s[..i].to_string() };
            Ok((host, port))
        }
        None if s.is_empty() => Ok(("0.0.0.0".to_string(), default_port)),
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
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    Err(match last {
        Some(e) if e.kind() == ErrorKind::TimedOut => CamelError::new(kinds::CONNECT, format!("Connect timed out: {}", what)),
        Some(e) => io_error(e, &what),
        None => CamelError::new(kinds::CONNECT, format!("Unknown host: {}", host)),
    })
}

/// Connect to `host:port`; with `tls` the TLS handshake is performed right away
/// (implicit TLS). TLS failures map to `SSLHandshakeException`/`SSLPeerUnverifiedException`.
pub fn connect_stream(host: &str, port: u16, timeout: Duration, tls: Option<&EndpointTls>) -> Result<NetStream> {
    let tcp = connect(host, port, timeout)?;
    match tls {
        Some(t) => {
            let cfg = t.client_config()?;
            if !timeout.is_zero() {
                let _ = tcp.set_read_timeout(Some(timeout));
            }
            NetStream::client(tcp, t.server_name(host), &cfg).map_err(|e| io_to_camel(e, &host_port(host, port)))
        }
        None => Ok(NetStream::plain(tcp)),
    }
}

/// Upgrades a line-protocol session to TLS in place (STARTTLS / STLS / `AUTH TLS`).
/// Fails if the server already sent data past the positive reply.
pub fn upgrade_stream(reader: &mut BufReader<NetStream>, writer: &mut NetStream, tls: &EndpointTls, host: &str) -> Result<()> {
    if !reader.buffer().is_empty() {
        return Err(CamelError::io(format!("Unexpected data from {} before the TLS handshake", host)));
    }
    let plain = writer.try_clone().map_err(|e| io_error(e, host))?;
    let secured = tls.start_tls(plain, host)?;
    *reader = BufReader::new(secured.try_clone().map_err(|e| io_error(e, host))?);
    *writer = secured;
    Ok(())
}

/// Read one line terminated by `\n` (an optional `\r` is removed). `Ok(None)` on EOF.
pub fn read_line<R: BufRead>(r: &mut R, max: usize) -> std::io::Result<Option<String>> {
    let mut buf = Vec::new();
    let n = std::io::Read::take(r, max as u64 + 2).read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Ok(None);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
    } else if buf.len() > max {
        return Err(std::io::Error::new(ErrorKind::InvalidData, "line too long"));
    }
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

/// Pseudo random bytes (seeded by the std hasher's random keys); good enough for
/// WebSocket keys/masks and message ids, not for cryptography.
pub fn random_bytes(n: usize) -> Vec<u8> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let state = RandomState::new();
    let mut out = Vec::with_capacity(n + 8);
    while out.len() < n {
        let mut h = state.build_hasher();
        h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        out.extend_from_slice(&h.finish().to_le_bytes());
    }
    out.truncate(n);
    out
}

/// Remove options that are accepted for compatibility but have no effect.
pub fn take_ignored(params: &mut Parameters, keys: &[&str]) {
    for k in keys {
        params.take(k);
    }
}

/// Common accepted-and-ignored endpoint options.
pub const COMMON_IGNORED: &[&str] = &["lazyStartProducer", "bridgeErrorHandler", "exceptionHandler", "exchangePattern", "synchronous"];

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
        let listener = cameleer_tls::net::bind_listener(&addr).map_err(|e| CamelError::io(format!("Failed to bind {} on {}: {}", name, addr, e)))?;
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

    /// Stop accepting, close all connections and join the threads.
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(a) = self.accept.lock().unwrap().take() {
            let _ = a.join();
        }
        for s in self.conns.lock().unwrap().values() {
            let _ = cameleer_tls::net::shutdown_socket(s, std::net::Shutdown::Both);
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
    fn authority_parsing() {
        let a = Authority::parse("admin:se%40cret@ftp.example.com:2121/in/box", 21).unwrap();
        assert_eq!(a.user.as_deref(), Some("admin"));
        assert_eq!(a.password.as_deref(), Some("se@cret"));
        assert_eq!(a.host, "ftp.example.com");
        assert_eq!(a.port, 2121);
        assert_eq!(a.path, "in/box");
        let a = Authority::parse("localhost", 25).unwrap();
        assert_eq!((a.user, a.host.as_str(), a.port, a.path.as_str()), (None, "localhost", 25, ""));
        let a = Authority::parse("[::1]:99/x", 1).unwrap();
        assert_eq!((a.host.as_str(), a.port), ("::1", 99));
        assert!(Authority::parse("host:abc", 1).is_err());
    }

    #[test]
    fn random_is_random() {
        assert_eq!(random_bytes(20).len(), 20);
        assert_ne!(random_bytes(16), random_bytes(16));
    }
}

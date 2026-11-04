//! Multi-threaded HTTP/1.1 server on `std::net::TcpListener`.
//!
//! Connections are served by a [`camel_core::executor::ThreadPool`]; each connection
//! supports keep-alive (with an idle timeout), `Expect: 100-continue`, request size
//! limits and read/write timeouts. [`HttpServer::stop`] is graceful: it stops accepting
//! (waking the blocking accept with a loopback connection), waits for in-flight
//! requests up to `shutdown_timeout`, then closes idle connections.
//!
//! ```no_run
//! use camel_http::server::{HttpServer, ServerConfig};
//! use camel_http::http::{HttpRequest, HttpResponse};
//! let server = HttpServer::bind("127.0.0.1:0", ServerConfig::default(), |req: &mut HttpRequest| {
//!     HttpResponse::text(200, format!("hello {}", req.path))
//! }).unwrap();
//! println!("listening on {}", server.local_addr());
//! server.stop();
//! ```

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use camel_core::executor::ThreadPool;
use camel_tls::net::NetStream;
use camel_tls::TlsServerConfig;

use crate::http::{read_chunked, read_request_head, write_response, BodyKind, BodyReader, HttpRequest, HttpResponse, Limits};

/// Handles a request and produces a response.
pub trait HttpHandler: Send + Sync {
    fn handle(&self, request: &mut HttpRequest) -> HttpResponse;

    /// Whether the body of this request (head only, body still on the connection) should be
    /// streamed: it is then passed as a `camel_tls::stream_body` handle in
    /// [`HttpRequest::body_stream`] instead of being read into memory (the connection is
    /// closed after such a request).
    fn stream_request(&self, _request: &HttpRequest) -> bool {
        false
    }
}

impl<F> HttpHandler for F
where
    F: Fn(&mut HttpRequest) -> HttpResponse + Send + Sync,
{
    fn handle(&self, request: &mut HttpRequest) -> HttpResponse {
        self(request)
    }
}

/// Server tuning.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// Name used for worker threads.
    pub name: String,
    /// Number of worker threads (each serves one connection at a time).
    pub pool_size: usize,
    pub limits: Limits,
    /// Timeout while reading a request once it started.
    pub read_timeout: Duration,
    /// Idle timeout between requests on a keep-alive connection.
    pub keep_alive_timeout: Duration,
    pub write_timeout: Duration,
    /// How long `stop` waits for in-flight requests.
    pub shutdown_timeout: Duration,
    /// Maximum requests served on one connection.
    pub max_keep_alive_requests: usize,
    /// Serve HTTPS with this TLS configuration (None = plain HTTP).
    pub tls: Option<Arc<TlsServerConfig>>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            name: "camel-http".into(),
            pool_size: 32,
            limits: Limits::default(),
            read_timeout: Duration::from_secs(30),
            keep_alive_timeout: Duration::from_secs(15),
            write_timeout: Duration::from_secs(30),
            shutdown_timeout: Duration::from_secs(10),
            max_keep_alive_requests: 1000,
            tls: None,
        }
    }
}

struct ServerInner {
    running: AtomicBool,
    connections: Mutex<HashMap<u64, TcpStream>>,
    next_id: AtomicU64,
    busy: AtomicUsize,
    pool: Arc<ThreadPool>,
    handler: Arc<dyn HttpHandler>,
    config: ServerConfig,
}

/// A running HTTP server. Dropping it stops it.
pub struct HttpServer {
    inner: Arc<ServerInner>,
    local_addr: SocketAddr,
    accept_thread: Mutex<Option<JoinHandle<()>>>,
}

impl HttpServer {
    /// Bind to `addr` (port 0 picks a free port) and start serving with `handler`.
    pub fn bind(addr: impl ToSocketAddrs, config: ServerConfig, handler: impl HttpHandler + 'static) -> io::Result<HttpServer> {
        Self::bind_arc(addr, config, Arc::new(handler))
    }

    /// Like [`HttpServer::bind`] with a shared handler.
    pub fn bind_arc(addr: impl ToSocketAddrs, config: ServerConfig, handler: Arc<dyn HttpHandler>) -> io::Result<HttpServer> {
        let listener = camel_tls::net::bind_listener(addr)?;
        let local_addr = listener.local_addr()?;
        let pool = ThreadPool::new(&format!("{}-{}", config.name, local_addr.port()), config.pool_size);
        let inner = Arc::new(ServerInner {
            running: AtomicBool::new(true),
            connections: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            busy: AtomicUsize::new(0),
            pool,
            handler,
            config,
        });
        let inner2 = inner.clone();
        let accept_thread = std::thread::Builder::new()
            .name(format!("{}-accept-{}", inner.config.name, local_addr.port()))
            .spawn(move || accept_loop(inner2, listener))?;
        camel_core::camel_debug!("camel-http", "HTTP server listening on {}", local_addr);
        Ok(HttpServer { inner, local_addr, accept_thread: Mutex::new(Some(accept_thread)) })
    }

    /// The bound address.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Whether the server speaks HTTPS.
    pub fn is_tls(&self) -> bool {
        self.inner.config.tls.is_some()
    }

    /// The bound port.
    pub fn port(&self) -> u16 {
        self.local_addr.port()
    }

    pub fn is_running(&self) -> bool {
        self.inner.running.load(Ordering::SeqCst)
    }

    /// Number of open connections.
    pub fn connection_count(&self) -> usize {
        self.inner.connections.lock().unwrap().len()
    }

    /// Gracefully stop the server. Idempotent.
    pub fn stop(&self) {
        if !self.inner.running.swap(false, Ordering::SeqCst) {
            return;
        }
        // wake the blocking accept() with a loopback connection
        let wake = wake_addr(self.local_addr);
        let woke = TcpStream::connect_timeout(&wake, Duration::from_secs(1)).is_ok();
        if let Some(t) = self.accept_thread.lock().unwrap().take() {
            if woke {
                let _ = t.join();
            }
        }
        // wait for in-flight requests
        let deadline = Instant::now() + self.inner.config.shutdown_timeout;
        while self.inner.busy.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        // close remaining (idle or stuck) connections
        for s in self.inner.connections.lock().unwrap().values() {
            let _ = camel_tls::net::shutdown_socket(s, std::net::Shutdown::Both);
        }
        self.inner.pool.shutdown();
        camel_core::camel_debug!("camel-http", "HTTP server on {} stopped", self.local_addr);
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn wake_addr(addr: SocketAddr) -> SocketAddr {
    let ip = match addr.ip() {
        IpAddr::V4(v) if v.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(v) if v.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    SocketAddr::new(ip, addr.port())
}

fn accept_loop(inner: Arc<ServerInner>, listener: TcpListener) {
    for stream in listener.incoming() {
        if !inner.running.load(Ordering::SeqCst) {
            break;
        }
        match stream {
            Ok(stream) => {
                let id = inner.next_id.fetch_add(1, Ordering::Relaxed);
                match stream.try_clone() {
                    Ok(clone) => {
                        inner.connections.lock().unwrap().insert(id, clone);
                    }
                    Err(_) => continue,
                }
                let inner2 = inner.clone();
                if !inner.pool.execute(move || {
                    serve_connection(&inner2, &stream);
                    inner2.connections.lock().unwrap().remove(&id);
                }) {
                    inner.connections.lock().unwrap().remove(&id);
                }
            }
            Err(_) => {
                if !inner.running.load(Ordering::SeqCst) {
                    break;
                }
                // e.g. too many open files: back off a little
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

struct BusyGuard<'a>(&'a AtomicUsize);
impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn serve_connection(inner: &ServerInner, tcp: &TcpStream) {
    let cfg = &inner.config;
    let _ = tcp.set_nodelay(true);
    let _ = tcp.set_write_timeout(Some(cfg.write_timeout));
    let peer = tcp.peer_addr().ok();
    let stream = match tcp.try_clone() {
        Ok(t) => match &cfg.tls {
            Some(tls) => {
                let _ = tcp.set_read_timeout(Some(cfg.read_timeout));
                match NetStream::server(t, tls) {
                    Ok(s) => s,
                    Err(e) => {
                        camel_core::camel_debug!("camel-http", "TLS handshake with {:?} failed: {}", peer, e);
                        let _ = tcp.shutdown(std::net::Shutdown::Both);
                        return;
                    }
                }
            }
            None => NetStream::plain(t),
        },
        Err(_) => return,
    };
    let stream = &stream;
    let mut reader = BufReader::new(stream);
    let mut writer = stream;
    let mut served = 0usize;
    loop {
        if !inner.running.load(Ordering::SeqCst) {
            break;
        }
        // wait (idle) for the next request
        let idle = if served == 0 { cfg.read_timeout } else { cfg.keep_alive_timeout };
        let _ = stream.set_read_timeout(Some(idle));
        match reader.fill_buf() {
            Ok([]) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        let _ = stream.set_read_timeout(Some(cfg.read_timeout));
        inner.busy.fetch_add(1, Ordering::SeqCst);
        let _guard = BusyGuard(&inner.busy);
        let head = read_request_head(&mut reader, Some(&mut writer), &cfg.limits).and_then(|h| match h {
            None => Ok(None),
            Some((req, kind)) => Ok(Some((req, kind))),
        });
        let (mut req, kind) = match head {
            Ok(Some(r)) => r,
            Ok(None) => break,
            Err(e) => {
                if let Some(status) = e.status() {
                    let resp = HttpResponse::text(status, format!("{}\n", e));
                    let _ = write_response(&mut writer, &resp, false, false);
                }
                break;
            }
        };
        let mut streamed = false;
        if matches!(kind, BodyKind::Chunked | BodyKind::Length(_)) && inner.handler.stream_request(&req) {
            // hand the rest of the connection (buffered bytes first) to a lazy body reader
            let buffered = reader.buffer().to_vec();
            reader.consume(buffered.len());
            match stream.try_clone() {
                Ok(owned) => {
                    let src = std::io::BufReader::new(std::io::Cursor::new(buffered).chain(owned));
                    let body = BodyReader::new(src, kind);
                    let len = body.content_length();
                    req.body_stream = Some(camel_tls::stream_body::register(Box::new(body), len, &req.target));
                    streamed = true;
                }
                Err(_) => break,
            }
        } else {
            let body = match kind {
                BodyKind::Length(n) if n > cfg.limits.max_body_bytes => Err(crate::http::HttpError::PayloadTooLarge),
                BodyKind::Length(n) => {
                    let mut b = vec![0u8; n];
                    reader.read_exact(&mut b).map(|_| b).map_err(crate::http::HttpError::from)
                }
                BodyKind::Chunked => read_chunked(&mut reader, cfg.limits.max_body_bytes),
                _ => Ok(Vec::new()),
            };
            match body {
                Ok(b) => req.body = b,
                Err(e) => {
                    if let Some(status) = e.status() {
                        let resp = HttpResponse::text(status, format!("{}\n", e));
                        let _ = write_response(&mut writer, &resp, false, false);
                    }
                    break;
                }
            }
        }
        req.remote_addr = peer;
        if !stream.peer_certificates().is_empty() {
            req.peer_certificates = stream.peer_certificates().to_vec();
        }
        served += 1;
        let mut keep_alive = !streamed
            && req.wants_keep_alive()
            && inner.running.load(Ordering::SeqCst)
            && served < cfg.max_keep_alive_requests
            && inner.pool.pending_count() == 0;
        let head = req.method == "HEAD";
        let version = req.version;
        let handler = inner.handler.clone();
        let mut resp = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler.handle(&mut req))) {
            Ok(r) => r,
            Err(_) => HttpResponse::text(500, "Internal Server Error"),
        };
        resp.version = version;
        if resp.headers.has_token("Connection", "close") {
            keep_alive = false;
        }
        let failed = write_response(&mut writer, &resp, head, keep_alive).is_err();
        if let Some(h) = &req.body_stream {
            camel_tls::stream_body::release(h);
        }
        if failed || !keep_alive {
            break;
        }
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
    let _ = tcp.shutdown(std::net::Shutdown::Both);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn serves_and_stops() {
        let server = HttpServer::bind("127.0.0.1:0", ServerConfig::default(), |req: &mut HttpRequest| {
            HttpResponse::text(200, format!("{} {}", req.method, req.path))
        })
        .unwrap();
        let mut s = TcpStream::connect(server.local_addr()).unwrap();
        s.write_all(b"GET /a HTTP/1.1\r\nHost: x\r\n\r\nGET /b HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        assert!(out.contains("GET /a"), "{}", out);
        assert!(out.contains("GET /b"), "{}", out);
        // an idle keep-alive connection must not block stop
        let mut idle = TcpStream::connect(server.local_addr()).unwrap();
        idle.write_all(b"GET /c HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut buf = [0u8; 512];
        let _ = idle.read(&mut buf).unwrap();
        let start = Instant::now();
        server.stop();
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(TcpStream::connect(server.local_addr()).is_err() || !server.is_running());
    }
}

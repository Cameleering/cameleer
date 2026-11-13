//! The HTTP/1.1 server, an event loop per core on [simoom](https://crates.io/crates/simoom).
//!
//! A few loop threads do all the socket work, so an idle connection costs a few hundred bytes
//! and no thread. Handlers still look blocking: a route runs on the loop thread while it is fast
//! and on simoom's offload pool (at most [`ServerConfig::pool_size`] threads) while it blocks, and
//! a handler that never blocks ([`HttpHandler::is_async`], the MCP endpoint) answers through a
//! responder from any thread. See `server_simoom.rs` for how a request is routed. Plain HTTP and
//! HTTPS ([`ServerConfig::tls`], TLS 1.2 and 1.3 with optional client certificates), keep-alive,
//! `Expect: 100-continue`, size limits and timeouts. [`HttpServer::stop`] is graceful: it stops
//! accepting, lets in-flight requests finish for up to `shutdown_timeout`, then closes the rest.
//!
//! ```no_run
//! use cameleer_http::server::{HttpServer, ServerConfig};
//! use cameleer_http::http::{HttpRequest, HttpResponse};
//! let server = HttpServer::bind("127.0.0.1:0", ServerConfig::default(), |req: &mut HttpRequest| {
//!     HttpResponse::text(200, format!("hello {}", req.path))
//! }).unwrap();
//! println!("listening on {}", server.local_addr());
//! server.stop();
//! ```

use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;
use std::io;

use cameleer_tls::TlsServerConfig;

use crate::http::{HttpRequest, HttpResponse, Limits};
use crate::server_simoom::SimoomEngine;

/// Delivers a response later (see [`HttpHandler::handle_async`]).
pub type Responder = Box<dyn FnOnce(HttpResponse) + Send>;

/// Handles a request and produces a response.
pub trait HttpHandler: Send + Sync {
    fn handle(&self, request: &mut HttpRequest) -> HttpResponse;

    /// Identifies the route a request is dispatched to (0 = unknown). An event-loop engine uses it
    /// to learn per route whether the handler is fast enough to run on the loop thread.
    fn route_key(&self, _request: &HttpRequest) -> u64 {
        0
    }

    /// Whether this request is answered by [`HttpHandler::handle_async`]: the handler never
    /// blocks the calling thread, it hands the work on and answers through the responder, from
    /// any thread, later. An event-loop engine calls it on the loop thread; the thread-per-
    /// connection engine waits for the answer.
    fn is_async(&self, _request: &HttpRequest) -> bool {
        false
    }

    /// Answer `request` through `respond` (exactly once, from any thread). Only called when
    /// [`HttpHandler::is_async`] is true.
    fn handle_async(&self, mut request: HttpRequest, respond: Responder) {
        respond(self.handle(&mut request));
    }

    /// Whether the body of this request (head only, body still on the connection) should be
    /// streamed: it is then passed as a `cameleer_tls::stream_body` handle in
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
    /// Maximum threads for routes that block (a call to another service, a database). They start
    /// on demand and exit when idle. Connections cost no thread; this limits only the requests
    /// that are being worked on at the same time. 200 is the default of Tomcat's and Jetty's pools.
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
            name: "cameleer-http".into(),
            pool_size: 200,
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

/// A running HTTP server. Dropping it stops it.
pub struct HttpServer {
    engine: SimoomEngine,
    local_addr: SocketAddr,
    tls: bool,
    running: std::sync::atomic::AtomicBool,
}

impl HttpServer {
    /// Bind to `addr` (port 0 picks a free port) and start serving with `handler`.
    pub fn bind(addr: impl ToSocketAddrs, config: ServerConfig, handler: impl HttpHandler + 'static) -> io::Result<HttpServer> {
        Self::bind_arc(addr, config, Arc::new(handler))
    }

    /// Like [`HttpServer::bind`] with a shared handler.
    pub fn bind_arc(addr: impl ToSocketAddrs, config: ServerConfig, handler: Arc<dyn HttpHandler>) -> io::Result<HttpServer> {
        let tls = config.tls.is_some();
        let engine = SimoomEngine::start(addr, &config, handler)?;
        let local_addr = engine.local_addr();
        cameleer_core::camel_debug!("cameleer-http", "HTTP server listening on {}", local_addr);
        Ok(HttpServer { engine, local_addr, tls, running: std::sync::atomic::AtomicBool::new(true) })
    }

    /// The bound address.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Whether the server speaks HTTPS.
    pub fn is_tls(&self) -> bool {
        self.tls
    }

    /// The bound port.
    pub fn port(&self) -> u16 {
        self.local_addr.port()
    }

    pub fn is_running(&self) -> bool {
        self.running.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Number of open connections.
    pub fn connection_count(&self) -> usize {
        self.engine.connection_count()
    }

    /// Gracefully stop the server. Idempotent.
    pub fn stop(&self) {
        if !self.running.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        self.engine.stop();
        cameleer_core::camel_debug!("cameleer-http", "HTTP server on {} stopped", self.local_addr);
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpStream;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

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

    fn keep_alive_requests(addr: SocketAddr, n: usize) -> usize {
        // returns how many responses asked to close the connection
        let s = TcpStream::connect(addr).unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut w = s;
        let mut closes = 0;
        for _ in 0..n {
            w.write_all(b"GET /k HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
            let mut len = 0;
            loop {
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let l = line.trim_end().to_ascii_lowercase();
                if l.is_empty() {
                    break;
                }
                if l == "connection: close" {
                    closes += 1;
                }
                if let Some(v) = l.strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0u8; len];
            r.read_exact(&mut body).unwrap();
            if closes > 0 {
                break;
            }
        }
        closes
    }

    #[test]
    fn a_request_head_split_across_packets_is_read() {
        let server = HttpServer::bind("127.0.0.1:0", ServerConfig::default(), |_req: &mut HttpRequest| HttpResponse::text(200, "ok")).unwrap();
        let mut s = TcpStream::connect(server.local_addr()).unwrap();
        s.set_nodelay(true).unwrap();
        s.write_all(b"GET /a HTTP/1.1\r\nHo").unwrap();
        std::thread::sleep(Duration::from_millis(50));
        s.write_all(b"st: x\r\n\r\nPOST /b HTTP/1.1\r\nHost: x\r\nContent-Length: 4\r\n\r\nab").unwrap();
        std::thread::sleep(Duration::from_millis(50));
        s.write_all(b"cdGET /c HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").unwrap();
        let mut all = String::new();
        s.read_to_string(&mut all).unwrap();
        assert_eq!(all.matches("HTTP/1.1 200").count(), 3, "{all}");
        server.stop();
    }

    #[test]
    fn keep_alive_survives_new_connections_while_workers_are_idle() {
        let cfg = ServerConfig { pool_size: 8, ..ServerConfig::default() };
        let server = HttpServer::bind("127.0.0.1:0", cfg, |_req: &mut HttpRequest| HttpResponse::text(200, "ok")).unwrap();
        let addr = server.local_addr();
        let stop = Arc::new(AtomicBool::new(false));
        // connection churn: every new connection is briefly a pending job of the pool
        let churn = {
            let stop = stop.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let _ = keep_alive_requests(addr, 1);
                }
            })
        };
        let persistent: Vec<_> = (0..4).map(|_| std::thread::spawn(move || keep_alive_requests(addr, 300))).collect();
        let closes: usize = persistent.into_iter().map(|h| h.join().unwrap()).sum();
        stop.store(true, Ordering::SeqCst);
        churn.join().unwrap();
        assert_eq!(closes, 0, "keep-alive connections were closed although workers were idle");
        server.stop();
    }

    #[test]
    fn more_concurrent_clients_than_the_old_32_workers_keep_their_connections() {
        // 64 clients is a modest load for an HTTP service; with the default limit none of them
        // should be pushed to reconnect after each response. The handler takes 1 ms, like a
        // route that does some work, so all 64 connections are open at the same time.
        let server = HttpServer::bind("127.0.0.1:0", ServerConfig::default(), |_req: &mut HttpRequest| {
            std::thread::sleep(Duration::from_millis(1));
            HttpResponse::text(200, "ok")
        })
        .unwrap();
        let addr = server.local_addr();
        let barrier = Arc::new(std::sync::Barrier::new(64));
        let clients: Vec<_> = (0..64)
            .map(|_| {
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    keep_alive_requests(addr, 20)
                })
            })
            .collect();
        let closes: usize = clients.into_iter().map(|c| c.join().unwrap()).sum();
        assert_eq!(closes, 0);
        server.stop();
    }
}

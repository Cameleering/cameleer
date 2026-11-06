//! Multi-threaded HTTP/2 server (h2c prior knowledge and/or TLS with ALPN `h2`).
//!
//! Each connection gets a reader and a writer thread; every request stream is handled on
//! its own thread by an [`H2Handler`], which receives the request headers and the
//! [`H2Stream`] to read the body from and write the response to.
//!
//! ```no_run
//! use cameleer_grpc::h2::{H2Config, H2Server, H2Stream, ServerRequest};
//! let server = H2Server::bind("127.0.0.1:0", H2Config::default(), None, |req: ServerRequest, stream: H2Stream| {
//!     let _body = stream.read_body(None);
//!     let _ = ServerRequest::respond(&stream, 200, &[("content-type", "text/plain")], format!("hello {}", req.path).as_bytes());
//! }).unwrap();
//! println!("listening on {}", server.local_addr());
//! server.stop();
//! ```

use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

use cameleer_tls::TlsServerConfig;

use super::conn::{H2Config, H2Error, H2Stream, NewStreamFn, Role, Shared};
use super::transport::Transport;
use crate::hpack::HeaderList;

/// A received request (headers only; read the body from the stream).
#[derive(Clone, Debug)]
pub struct ServerRequest {
    pub method: String,
    pub path: String,
    pub scheme: String,
    pub authority: String,
    /// Regular (non pseudo) headers, lower-case names.
    pub headers: HeaderList,
    pub peer_addr: Option<SocketAddr>,
}

impl ServerRequest {
    fn from_headers(h: HeaderList, peer: Option<SocketAddr>) -> Self {
        let get = |n: &str| h.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone()).unwrap_or_default();
        ServerRequest {
            method: get(":method"),
            path: get(":path"),
            scheme: get(":scheme"),
            authority: get(":authority"),
            headers: h.iter().filter(|(k, _)| !k.starts_with(':')).cloned().collect(),
            peer_addr: peer,
        }
    }

    /// First value of a header.
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers.iter().find(|(k, _)| *k == name).map(|(_, v)| v.as_str())
    }

    /// Convenience: sends a complete response (status, headers, body) on `stream`.
    pub fn respond(stream: &H2Stream, status: u16, headers: &[(&str, &str)], body: &[u8]) -> Result<(), H2Error> {
        let mut h = vec![(":status".to_string(), status.to_string())];
        for (n, v) in headers {
            h.push((n.to_ascii_lowercase(), v.to_string()));
        }
        if body.is_empty() {
            stream.send_headers(&h, true)
        } else {
            h.push(("content-length".into(), body.len().to_string()));
            stream.send_headers(&h, false)?;
            stream.send_data(body, true)
        }
    }
}

/// Handles one request stream (runs on its own thread).
pub trait H2Handler: Send + Sync {
    fn handle(&self, request: ServerRequest, stream: H2Stream);
}

impl<F> H2Handler for F
where
    F: Fn(ServerRequest, H2Stream) + Send + Sync,
{
    fn handle(&self, request: ServerRequest, stream: H2Stream) {
        self(request, stream)
    }
}

struct Inner {
    running: AtomicBool,
    connections: Mutex<Vec<Weak<Shared>>>,
}

/// A running HTTP/2 server.
pub struct H2Server {
    local: SocketAddr,
    inner: Arc<Inner>,
    accept: Mutex<Option<JoinHandle<()>>>,
    tls: bool,
}

impl H2Server {
    /// Binds `addr` and starts serving. With `tls = Some(..)` connections use TLS (the
    /// server config should offer ALPN `h2`; it is added when missing is not possible, so
    /// configure it with `.alpn_protocols(&["h2"])`), otherwise cleartext h2c.
    pub fn bind<H: H2Handler + 'static>(addr: &str, config: H2Config, tls: Option<TlsServerConfig>, handler: H) -> Result<H2Server, H2Error> {
        let listener = TcpListener::bind(addr).map_err(|e| H2Error::Io(format!("cannot bind {}: {}", addr, e)))?;
        let local = listener.local_addr().map_err(|e| H2Error::Io(e.to_string()))?;
        let inner = Arc::new(Inner { running: AtomicBool::new(true), connections: Mutex::new(Vec::new()) });
        let handler: Arc<dyn H2Handler> = Arc::new(handler);
        let is_tls = tls.is_some();
        let tls = tls.map(Arc::new);
        let inner2 = inner.clone();
        let accept = std::thread::Builder::new()
            .name(format!("h2-accept-{}", local.port()))
            .spawn(move || {
                for s in listener.incoming() {
                    if !inner2.running.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(s) = s else {
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    };
                    let (inner3, handler, config, tls) = (inner2.clone(), handler.clone(), config.clone(), tls.clone());
                    let _ = std::thread::Builder::new().name("h2-conn".into()).spawn(move || {
                        if let Err(e) = serve_connection(s, &inner3, handler, config, tls) {
                            cameleer_core::camel_debug!("h2", "HTTP/2 connection failed: {}", e);
                        }
                    });
                }
            })
            .map_err(|e| H2Error::Io(e.to_string()))?;
        Ok(H2Server { local, inner, accept: Mutex::new(Some(accept)), tls: is_tls })
    }

    /// The bound address (useful with port 0).
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    /// Whether the server uses TLS.
    pub fn is_tls(&self) -> bool {
        self.tls
    }

    /// Number of live connections.
    pub fn connection_count(&self) -> usize {
        let mut c = self.inner.connections.lock().unwrap();
        c.retain(|w| w.upgrade().map(|s| !s.is_closed()).unwrap_or(false));
        c.len()
    }

    /// Graceful stop: stops accepting, sends GOAWAY on every connection, waits up to
    /// `timeout` for in-flight streams, then closes the connections.
    pub fn stop_graceful(&self, timeout: Duration) {
        if !self.inner.running.swap(false, Ordering::SeqCst) {
            return;
        }
        let wake = SocketAddr::new(
            if self.local.ip().is_unspecified() {
                if self.local.is_ipv4() {
                    std::net::Ipv4Addr::LOCALHOST.into()
                } else {
                    std::net::Ipv6Addr::LOCALHOST.into()
                }
            } else {
                self.local.ip()
            },
            self.local.port(),
        );
        if let Ok(s) = TcpStream::connect_timeout(&wake, Duration::from_secs(1)) {
            let _ = s.shutdown(Shutdown::Both);
        }
        if let Some(h) = self.accept.lock().unwrap().take() {
            let _ = h.join();
        }
        let conns: Vec<Arc<Shared>> = self.inner.connections.lock().unwrap().drain(..).filter_map(|w| w.upgrade()).collect();
        for c in &conns {
            c.goaway();
        }
        for c in &conns {
            c.wait_idle(timeout);
            c.close();
        }
    }

    /// Stops the server (graceful with a 2 second grace period).
    pub fn stop(&self) {
        self.stop_graceful(Duration::from_secs(2));
    }
}

impl Drop for H2Server {
    fn drop(&mut self) {
        self.stop_graceful(Duration::from_millis(100));
    }
}

fn serve_connection(sock: TcpStream, inner: &Arc<Inner>, handler: Arc<dyn H2Handler>, config: H2Config, tls: Option<Arc<TlsServerConfig>>) -> Result<(), H2Error> {
    let transport = match tls {
        Some(cfg) => {
            let _ = sock.set_read_timeout(Some(config.connect_timeout));
            let _ = sock.set_write_timeout(Some(config.connect_timeout));
            let stream = cameleer_tls::net::accept_tls(sock, &cfg).map_err(|e| H2Error::Io(format!("TLS handshake failed: {}", e)))?;
            if let Some(p) = stream.alpn_protocol() {
                if p != b"h2" {
                    return Err(H2Error::Protocol(super::ErrorCode::HTTP_1_1_REQUIRED, "client negotiated a protocol other than h2".into()));
                }
            }
            let _ = stream.get_ref().set_read_timeout(None);
            Transport::tls(stream).map_err(|e| H2Error::Io(e.to_string()))?
        }
        None => Transport::tcp(sock).map_err(|e| H2Error::Io(e.to_string()))?,
    };
    let shared = start_server_connection(transport, config, handler)?;
    if !inner.running.load(Ordering::SeqCst) {
        shared.close();
        return Ok(());
    }
    let mut conns = inner.connections.lock().unwrap();
    conns.retain(|w| w.strong_count() > 0);
    conns.push(Arc::downgrade(&shared));
    Ok(())
}

fn start_server_connection(transport: Transport, config: H2Config, handler: Arc<dyn H2Handler>) -> Result<Arc<Shared>, H2Error> {
    let peer = transport.peer_addr;
    let on_new: NewStreamFn = Box::new(move |stream: H2Stream, headers: HeaderList| {
        let handler = handler.clone();
        let req = ServerRequest::from_headers(headers, peer);
        let _ = std::thread::Builder::new().name("h2-stream".into()).spawn(move || handler.handle(req, stream));
    });
    Shared::start(Role::Server, transport, config, Some(on_new))
}

/// Serves HTTP/2 on a connection accepted elsewhere (e.g. by an HTTP/1.1 server that
/// detected the client preface or negotiated ALPN `h2`). The client preface must still be
/// unread in `transport`. Returns once the preface was checked; the connection is then
/// served by background threads until it closes.
pub fn serve_transport<H: H2Handler + 'static>(transport: Transport, config: H2Config, handler: H) -> Result<(), H2Error> {
    start_server_connection(transport, config, Arc::new(handler)).map(|_| ())
}

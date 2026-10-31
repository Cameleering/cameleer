//! Embedded in-memory emulators of the cloud services, for tests (in the spirit of
//! LocalStack, Azurite and the Google Cloud emulators). Every emulator **verifies the
//! authentication** of each request: AWS SigV4 signatures are recomputed with the
//! registered secret ([`AwsMock`]), Azure Shared Key signatures and account SAS tokens are
//! recomputed with the account key ([`AzuriteMock`]), and Google bearer tokens must come from
//! the emulator's OAuth token endpoint, which validates the RS256 JWT assertion with the
//! service account's public key ([`GcpMock`]).
//!
//! All servers bind `127.0.0.1` on an ephemeral port; point the components at them with
//! `uriEndpointOverride=<mock.url()>&overrideEndpoint=true`.
//!
//! [`MockServer`] is the generic HTTP/1.1 server (optionally TLS, with a generated
//! self-signed certificate) and [`ConnectProxy`] a minimal `CONNECT` proxy for proxy tests.

pub mod aws;
pub mod azure;
pub mod gcp;

pub use aws::AwsMock;
pub use azure::AzuriteMock;
pub use gcp::GcpMock;

use std::io::{self, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use camel_tls::{TlsServerConfig, TlsStream};

use crate::http::{read_request, write_response, Response, ServerRequest};

/// Request handler of a [`MockServer`].
pub type Handler = Arc<dyn Fn(&ServerRequest) -> Response + Send + Sync>;

/// A small threaded HTTP/1.1 server (one thread per connection, keep-alive supported).
pub struct MockServer {
    port: u16,
    tls: bool,
    stop: Arc<AtomicBool>,
    requests: Arc<AtomicUsize>,
    accept: Option<JoinHandle<()>>,
    /// PEM of the self-signed certificate when TLS is enabled.
    pub cert_pem: Option<String>,
}

trait Conn: Read + Write + Send {}
impl<T: Read + Write + Send> Conn for T {}

impl MockServer {
    /// Start a plain HTTP server.
    pub fn start(handler: Handler) -> io::Result<MockServer> {
        Self::start_inner(handler, None)
    }

    /// Start an HTTPS server with a generated self-signed certificate for `localhost` / 127.0.0.1.
    pub fn start_tls(handler: Handler) -> io::Result<MockServer> {
        let ss = camel_tls::self_signed("localhost");
        let cfg = TlsServerConfig::from_pem(ss.cert_pem.as_bytes(), ss.key_pem.as_bytes()).map_err(|e| io::Error::other(e.to_string()))?;
        let mut s = Self::start_inner(handler, Some(Arc::new(cfg)))?;
        s.cert_pem = Some(ss.cert_pem);
        Ok(s)
    }

    fn start_inner(handler: Handler, tls: Option<Arc<TlsServerConfig>>) -> io::Result<MockServer> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(AtomicUsize::new(0));
        let (stop2, req2, is_tls) = (stop.clone(), requests.clone(), tls.is_some());
        let accept = std::thread::Builder::new().name(format!("mock-server-{}", port)).spawn(move || {
            for conn in listener.incoming() {
                if stop2.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(sock) = conn else { continue };
                let (h, tls, reqs, stop3) = (handler.clone(), tls.clone(), req2.clone(), stop2.clone());
                std::thread::spawn(move || {
                    let _ = sock.set_read_timeout(Some(Duration::from_secs(30)));
                    let _ = sock.set_nodelay(true);
                    let stream: Box<dyn Conn> = match tls {
                        Some(cfg) => match TlsStream::accept(sock, &cfg) {
                            Ok(s) => Box::new(s),
                            Err(_) => return,
                        },
                        None => Box::new(sock),
                    };
                    serve(stream, h, reqs, stop3);
                });
            }
        })?;
        Ok(MockServer { port, tls: is_tls, stop, requests, accept: Some(accept), cert_pem: None })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Base URL (`http://127.0.0.1:port`, or `https://localhost:port` with TLS).
    pub fn url(&self) -> String {
        if self.tls {
            format!("https://localhost:{}", self.port)
        } else {
            format!("http://127.0.0.1:{}", self.port)
        }
    }

    /// Number of requests served.
    pub fn request_count(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    /// Stop accepting connections.
    pub fn stop(&mut self) {
        if !self.stop.swap(true, Ordering::SeqCst) {
            let _ = TcpStream::connect(("127.0.0.1", self.port));
            if let Some(h) = self.accept.take() {
                let _ = h.join();
            }
        }
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn serve(stream: Box<dyn Conn>, handler: Handler, requests: Arc<AtomicUsize>, stop: Arc<AtomicBool>) {
    let mut reader = BufReader::new(stream);
    loop {
        let req = match read_request(&mut reader) {
            Ok(Some(r)) => r,
            _ => return,
        };
        requests.fetch_add(1, Ordering::SeqCst);
        let keep_alive = !req.header("Connection").is_some_and(|c| c.eq_ignore_ascii_case("close")) && !stop.load(Ordering::SeqCst);
        let resp = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(&req)))
            .unwrap_or_else(|_| Response::new(500).with_body(&b"handler panicked"[..]));
        let w = reader.get_mut();
        if write_response(w, &resp, req.method == "HEAD", keep_alive).is_err() || !keep_alive {
            let _ = w.flush();
            return;
        }
    }
}

/// A minimal HTTP `CONNECT` proxy (tunnels bytes both ways); counts tunnels opened.
pub struct ConnectProxy {
    port: u16,
    tunnels: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
}

impl ConnectProxy {
    pub fn start() -> io::Result<ConnectProxy> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let tunnels = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (t2, s2) = (tunnels.clone(), stop.clone());
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                if s2.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut client) = conn else { continue };
                let t3 = t2.clone();
                std::thread::spawn(move || {
                    let mut head = Vec::new();
                    let mut b = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") {
                        match client.read(&mut b) {
                            Ok(1) => head.push(b[0]),
                            _ => return,
                        }
                    }
                    let line = String::from_utf8_lossy(&head).lines().next().unwrap_or("").to_string();
                    let mut parts = line.split_whitespace();
                    if parts.next() != Some("CONNECT") {
                        let _ = client.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n");
                        return;
                    }
                    let target = parts.next().unwrap_or("").to_string();
                    let Ok(upstream) = TcpStream::connect(&target) else {
                        let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n");
                        return;
                    };
                    t3.fetch_add(1, Ordering::SeqCst);
                    let _ = client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n");
                    let (mut c2, mut u2) = (client.try_clone().unwrap(), upstream.try_clone().unwrap());
                    let (mut c1, mut u1) = (client, upstream);
                    let a = std::thread::spawn(move || {
                        let _ = io::copy(&mut c2, &mut u2);
                        let _ = u2.shutdown(Shutdown::Write);
                    });
                    let _ = io::copy(&mut u1, &mut c1);
                    let _ = c1.shutdown(Shutdown::Write);
                    let _ = a.join();
                });
            }
        });
        Ok(ConnectProxy { port, tunnels, stop })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Number of successful CONNECT tunnels.
    pub fn tunnel_count(&self) -> usize {
        self.tunnels.load(Ordering::SeqCst)
    }
}

impl Drop for ConnectProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

/// The RSA test key (PKCS#8 PEM) used for generated service-account keys.
pub const TEST_RSA_KEY_PEM: &str = include_str!("../../testdata/rsa2048.pkcs8.pem");

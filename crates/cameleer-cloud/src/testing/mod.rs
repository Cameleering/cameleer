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

pub mod alibaba;
pub mod aws;
pub mod azure;
pub mod azure_services;
pub mod gcp;
pub mod imds;

pub use alibaba::SlsMock;
pub use aws::AwsMock;
pub use azure::AzuriteMock;
pub use azure_services::AzureMock;
pub use gcp::GcpMock;
pub use imds::ImdsMock;

use std::io::{self, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use cameleer_tls::TlsServerConfig;

use crate::http::{read_request, write_response, Response, ServerRequest};

/// Signing key of the emulated Microsoft Entra ID access tokens (shared by the emulators so
/// that Azurite accepts the tokens issued by [`AzureMock`]).
const ENTRA_TOKEN_KEY: &[u8] = b"cameleer-cloud-testing-entra-token-key";

/// Issue an emulated Entra ID access token `header.payload.signature` (HMAC) for `scope`.
/// The scripted tool calls in a user message: `call NAME {json}`, several separated by `;`.
/// The model mocks (Azure OpenAI, Bedrock Converse, Vertex AI) call these tools when tools are
/// offered, and answer `Done: <results>` once the results come back.
pub fn scripted_tool_calls(text: &str) -> Vec<(String, cameleer_core::value::Value)> {
    let mut out = Vec::new();
    for part in text.split(';') {
        let part = part.trim();
        let Some(rest) = part.strip_prefix("call ") else { continue };
        let (name, args) = match rest.find(' ') {
            Some(i) => (&rest[..i], rest[i..].trim()),
            None => (rest, "{}"),
        };
        let args = cameleer_core::json::parse(args).unwrap_or(cameleer_core::value::Value::Object(cameleer_core::value::ObjectMap::new()));
        out.push((name.trim().to_string(), args));
    }
    out
}

pub fn issue_entra_token(scope: &str, subject: &str, ttl_secs: i64) -> String {
    let payload = format!("{}|{}|{}|{}", scope, subject, crate::util::now_secs() + ttl_secs, crate::util::random_hex(8));
    let body = format!("eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.{}", crate::util::base64url_encode(payload.as_bytes()));
    let sig = crate::util::base64url_encode(&crate::util::hmac_sha256(ENTRA_TOKEN_KEY, body.as_bytes()));
    format!("{}.{}", body, sig)
}

/// Verify an emulated Entra ID token for `scope`: the subject (client / identity id) when the
/// signature is valid and the token is not expired.
pub fn verify_entra_token(token: &str, scope: &str) -> Option<String> {
    let (body, sig) = token.rsplit_once('.')?;
    let expected = crate::util::base64url_encode(&crate::util::hmac_sha256(ENTRA_TOKEN_KEY, body.as_bytes()));
    if !crate::util::ct_eq(expected.as_bytes(), sig.as_bytes()) {
        return None;
    }
    let payload = body.split_once('.')?.1;
    let raw = crate::util::base64_decode(&payload.replace('-', "+").replace('_', "/")).or_else(|| crate::util::base64_decode(&format!("{}{}", payload.replace('-', "+").replace('_', "/"), "=".repeat((4 - payload.len() % 4) % 4))))?;
    let text = String::from_utf8(raw).ok()?;
    let mut parts = text.split('|');
    let (sc, sub, exp) = (parts.next()?, parts.next()?, parts.next()?.parse::<i64>().ok()?);
    (sc == scope && exp > crate::util::now_secs()).then(|| sub.to_string())
}

/// Request handler of a [`MockServer`].
pub type Handler = Arc<dyn Fn(&ServerRequest) -> Response + Send + Sync>;

/// HTTP/2 request handler of a [`MockServer`] (cleartext h2c prior-knowledge connections): the request
/// with its body read, and the way to answer it.
pub type H2Handler = Arc<dyn Fn(ServerRequest, H2Reply) + Send + Sync>;

/// The answer to an HTTP/2 request: all at once ([`H2Reply::respond`]) or streamed ([`H2Reply::stream`]).
pub struct H2Reply {
    responder: simoom::Responder,
}

impl H2Reply {
    /// Sends the whole response.
    pub fn respond(self, status: u16, headers: &[(&str, &str)], body: &[u8]) {
        let mut r = simoom::Response::new(status);
        for (n, v) in headers {
            r = r.header(n.to_ascii_lowercase(), v);
        }
        self.responder.send(r.body(body.to_vec()));
    }

    /// Sends the response head; the body follows through the returned [`H2Body`].
    pub fn stream(self, status: u16, headers: &[(&str, &str)]) -> H2Body {
        let mut r = simoom::Response::new(status);
        for (n, v) in headers {
            r = r.header(n.to_ascii_lowercase(), v);
        }
        H2Body { sink: self.responder.stream(r) }
    }
}

/// The body of a streamed HTTP/2 response.
pub struct H2Body {
    sink: simoom::BodySink,
}

impl H2Body {
    /// Sends body bytes (waits while the client reads slowly).
    pub fn send(&mut self, data: &[u8]) -> io::Result<()> {
        self.sink.write(data)
    }

    /// True once the client cancelled the stream or went away.
    pub fn is_closed(&self) -> bool {
        self.sink.is_closed()
    }

    /// Ends the body.
    pub fn finish(self) {
        self.sink.finish();
    }
}

/// A simoom server for the HTTP/2 connections of a [`MockServer`]: the mock's own accept loop hands
/// them over through a tunnel, because simoom accepts its own connections only.
fn start_h2_server(h2: H2Handler) -> io::Result<simoom::ServerHandle> {
    let service = move |req: &mut simoom::Request<'_>, reply: simoom::Reply<'_>| -> simoom::Outcome {
        let h2 = h2.clone();
        reply.offload(req, move |req, reply| {
            let mut body = req.body().to_vec();
            if let Some(mut src) = req.take_body() {
                if src.read_to_end(&mut body).is_err() {
                    return reply.status(400).body_static(b"bad request body");
                }
            }
            let target = String::from_utf8_lossy(req.target()).into_owned();
            let (path, query) = match target.split_once('?') {
                Some((p, q)) => (p.to_string(), q.to_string()),
                None => (target, String::new()),
            };
            let mut headers: Vec<(String, String)> = req.headers().map(|(n, v)| (String::from_utf8_lossy(n).to_ascii_lowercase(), String::from_utf8_lossy(v).into_owned())).collect();
            if let Some(h) = req.host() {
                headers.push(("host".into(), String::from_utf8_lossy(h).into_owned()));
            }
            let request = ServerRequest { method: String::from_utf8_lossy(req.method()).into_owned(), path, query, headers, body };
            let responder = reply.defer();
            // the handler may stream for a long time: its own thread, not a pool worker
            let spawned = std::thread::Builder::new().name("mock-h2".into()).spawn(move || h2(request, H2Reply { responder }));
            if spawned.is_err() {
                return simoom::Outcome::Deferred;
            }
            simoom::Outcome::Deferred
        })
    };
    simoom::Server::builder().threads(1).service(service).listen("127.0.0.1:0").start()
}

/// Copies bytes both ways between `client` and the local `port` until either side closes.
fn tunnel(client: TcpStream, port: u16) {
    let Ok(up) = TcpStream::connect(("127.0.0.1", port)) else { return };
    let (Ok(mut c2), Ok(mut u2)) = (client.try_clone(), up.try_clone()) else { return };
    let (mut c1, mut u1) = (client, up);
    let back = std::thread::spawn(move || {
        let _ = io::copy(&mut u2, &mut c2);
        let _ = cameleer_tls::net::shutdown_socket(&c2, Shutdown::Both);
        let _ = cameleer_tls::net::shutdown_socket(&u2, Shutdown::Both);
    });
    let _ = io::copy(&mut c1, &mut u1);
    let _ = cameleer_tls::net::shutdown_socket(&u1, Shutdown::Both);
    let _ = cameleer_tls::net::shutdown_socket(&c1, Shutdown::Both);
    let _ = back.join();
}

/// A small threaded HTTP/1.1 server (one thread per connection, keep-alive supported).
pub struct MockServer {
    port: u16,
    tls: bool,
    stop: Arc<AtomicBool>,
    requests: Arc<AtomicUsize>,
    accept: Option<JoinHandle<()>>,
    /// The server of the HTTP/2 connections, when the mock serves any.
    h2_server: Option<simoom::ServerHandle>,
    /// PEM of the self-signed certificate when TLS is enabled.
    pub cert_pem: Option<String>,
}

trait Conn: Read + Write + Send {}
impl<T: Read + Write + Send> Conn for T {}

impl MockServer {
    /// Start a plain HTTP server.
    pub fn start(handler: Handler) -> io::Result<MockServer> {
        Self::start_inner(handler, None, None)
    }

    /// Start a plain HTTP server that also serves HTTP/2 (h2c prior knowledge: connections
    /// starting with the client preface) on the same port with `h2`.
    pub fn start_with_h2(handler: Handler, h2: H2Handler) -> io::Result<MockServer> {
        Self::start_inner(handler, None, Some(h2))
    }

    /// Start an HTTPS server with a generated self-signed certificate for `localhost` / 127.0.0.1.
    pub fn start_tls(handler: Handler) -> io::Result<MockServer> {
        let ss = cameleer_tls::self_signed("localhost");
        let cfg = TlsServerConfig::from_pem(ss.cert_pem.as_bytes(), ss.key_pem.as_bytes()).map_err(|e| io::Error::other(e.to_string()))?;
        let mut s = Self::start_inner(handler, Some(Arc::new(cfg)), None)?;
        s.cert_pem = Some(ss.cert_pem);
        Ok(s)
    }

    fn start_inner(handler: Handler, tls: Option<Arc<TlsServerConfig>>, h2: Option<H2Handler>) -> io::Result<MockServer> {
        let listener = cameleer_tls::net::bind_loopback()?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(AtomicUsize::new(0));
        let (stop2, req2, is_tls) = (stop.clone(), requests.clone(), tls.is_some());
        let h2_server = match (h2, &tls) {
            (Some(h), None) => Some(start_h2_server(h)?),
            _ => None,
        };
        let h2_port = h2_server.as_ref().map(|s| s.local_addr().port());
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
                    if let (Some(port), None) = (h2_port, &tls) {
                        let mut pre = [0u8; 3];
                        if matches!(sock.peek(&mut pre), Ok(3)) && &pre == b"PRI" {
                            let _ = sock.set_read_timeout(None);
                            reqs.fetch_add(1, Ordering::SeqCst);
                            tunnel(sock, port);
                            return;
                        }
                    }
                    let stream: Box<dyn Conn> = match tls {
                        Some(cfg) => match cameleer_tls::net::accept_tls(sock, &cfg) {
                            Ok(s) => Box::new(s),
                            Err(_) => return,
                        },
                        None => Box::new(sock),
                    };
                    serve(stream, h, reqs, stop3);
                });
            }
        })?;
        Ok(MockServer { port, tls: is_tls, stop, requests, accept: Some(accept), h2_server, cert_pem: None })
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
        if let Some(h) = self.h2_server.take() {
            h.stop(Duration::from_millis(200));
            let _ = std::thread::Builder::new().name("mock-h2-stop".into()).spawn(move || h.join());
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
        let listener = cameleer_tls::net::bind_loopback()?;
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

//! gRPC server (byte-level messages) on simoom.
//!
//! One simoom server (an event loop per core, HTTP/2 over h2c prior knowledge, `Upgrade: h2c` or TLS
//! with ALPN `h2`) runs the HTTP/2 side. Each call gets its own thread, so a handler may block while
//! it waits for the next request message or for the client to cancel.
//!
//! How the pieces fit:
//!
//! * simoom calls the service when the first request bytes arrive (the server's `body_buffer` is 1
//!   byte), not when the request has ended: a bidirectional call is answered while the client is
//!   still sending. A call whose request is complete at once (a unary call) is handed over with its
//!   body already read.
//! * The request body reaches the handler through a channel fed by a reader thread, so `recv` can
//!   wait with the call's deadline.
//! * The response head is sent with the first message (or a trailers-only answer when the call ends
//!   without one), the status goes out as trailers.

use std::io::Read;
use std::net::SocketAddr;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use cameleer_core::executor::ThreadPool;
use cameleer_tls::TlsServerConfig;
use simoom::{BodySink, Outcome, Reply, Request, Responder, Response};

use super::{encode_grpc_message, frame_message_with, is_reserved_header, parse_timeout, Code, MessageDecoder, Metadata, Status};
use crate::h2::H2Config;

/// Implements the methods of one or more services. Return the final status of the call.
pub trait GrpcService: Send + Sync {
    fn call(&self, call: &ServerCall) -> Status;
}

impl<F> GrpcService for F
where
    F: Fn(&ServerCall) -> Status + Send + Sync,
{
    fn call(&self, call: &ServerCall) -> Status {
        self(call)
    }
}

/// The threads of all calls, in one pool for the process: a call needs up to three (the call itself, the
/// reader of its request messages and, with a deadline, the handler), and starting threads was what limited
/// the server to about 11,000 calls per second. The pool is a cached one: it starts a worker only when none is
/// idle, no matter how many are busy (a call may block for as long as its stream lives), and a worker that
/// stays idle for 30 seconds exits. It is never shut down, so stopping a server does not wait for a call.
fn pool() -> &'static Arc<ThreadPool> {
    static POOL: OnceLock<Arc<ThreadPool>> = OnceLock::new();
    POOL.get_or_init(|| ThreadPool::with_keep_alive("grpc", 1 << 20, Duration::from_secs(30)))
}

/// What the reader thread passes on.
enum Chunk {
    Data(Vec<u8>),
    End,
    /// The stream was reset or the connection dropped.
    Failed,
}

struct Incoming {
    rx: Receiver<Chunk>,
    decoder: MessageDecoder,
}

struct CallState {
    send_encoding: Option<String>,
    finished: bool,
    response_headers: Metadata,
    trailers: Metadata,
    /// The response has not started: the responder answers the whole call (trailers only) or
    /// opens the stream with the first message.
    responder: Option<Responder>,
}

struct CallInner {
    service: String,
    method: String,
    metadata: Metadata,
    user_agent: Option<String>,
    deadline: Option<Instant>,
    peer: Option<SocketAddr>,
    accept_encoding: Option<String>,
    incoming: Mutex<Incoming>,
    /// Lock order: `sink`, then `state`.
    sink: Mutex<Option<BodySink>>,
    state: Mutex<CallState>,
    /// The client cancelled the call or went away.
    cancelled: Arc<AtomicBool>,
    observer: AtomicI64,
}

/// One server-side call. Cheap to clone (shared state).
#[derive(Clone)]
pub struct ServerCall {
    inner: Arc<CallInner>,
}

impl ServerCall {
    /// Fully-qualified service name (`helloworld.Greeter`).
    pub fn service(&self) -> &str {
        &self.inner.service
    }
    /// Method name as in the path (`SayHello`).
    pub fn method(&self) -> &str {
        &self.inner.method
    }
    /// Id of the response observer handed to routes (`routeControlledStreamObserver`).
    pub fn observer_id(&self) -> Option<i64> {
        Some(self.inner.observer.load(Ordering::Relaxed)).filter(|i| *i != 0)
    }
    /// Associates a response observer id with this call.
    pub fn set_observer_id(&self, id: i64) {
        self.inner.observer.store(id, Ordering::Relaxed);
    }
    /// Request metadata (user headers, lower-case).
    pub fn metadata(&self) -> &Metadata {
        &self.inner.metadata
    }
    /// The client's `user-agent`.
    pub fn user_agent(&self) -> Option<&str> {
        self.inner.user_agent.as_deref()
    }
    /// The client's deadline (from `grpc-timeout`).
    pub fn deadline(&self) -> Option<Instant> {
        self.inner.deadline
    }
    pub fn peer_addr(&self) -> Option<SocketAddr> {
        self.inner.peer
    }

    /// True once the call was finished (e.g. deadline exceeded) or cancelled by the client.
    pub fn is_cancelled(&self) -> bool {
        if self.inner.state.lock().unwrap().finished || self.inner.cancelled.load(Ordering::SeqCst) {
            return true;
        }
        // a writer holds the sink while it waits for the client to read: not cancelled by that alone
        self.inner.sink.try_lock().ok().and_then(|g| g.as_ref().map(|s| s.is_closed())).unwrap_or(false)
    }

    /// The client's `grpc-accept-encoding`.
    pub fn accept_encoding(&self) -> Option<&str> {
        self.inner.accept_encoding.as_deref()
    }

    /// Compresses response messages with `encoding` (`gzip` / `deflate`) when the client
    /// accepts it; returns whether it is used.
    pub fn set_send_compression(&self, encoding: &str) -> bool {
        let ok = self.accept_encoding().map(|a| a.split(',').any(|e| e.trim().eq_ignore_ascii_case(encoding))).unwrap_or(false);
        if ok {
            self.inner.state.lock().unwrap().send_encoding = Some(encoding.to_ascii_lowercase());
        }
        ok
    }

    /// Adds a response header (before the first message).
    pub fn add_response_header(&self, name: &str, value: &str) {
        self.inner.state.lock().unwrap().response_headers.push((name.to_ascii_lowercase(), value.to_string()));
    }

    /// Adds trailing metadata.
    pub fn add_trailer(&self, name: &str, value: &str) {
        self.inner.state.lock().unwrap().trailers.push((name.to_ascii_lowercase(), value.to_string()));
    }

    fn gone(&self) -> Status {
        Status::new(Code::Cancelled, "call cancelled by the client")
    }

    /// Receives the next request message; `Ok(None)` when the client half-closed.
    pub fn recv(&self) -> Result<Option<Vec<u8>>, Status> {
        let mut inc = self.inner.incoming.lock().unwrap();
        loop {
            if let Some(m) = inc.decoder.next_message()? {
                return Ok(Some(m));
            }
            let chunk = match self.inner.deadline {
                Some(d) => {
                    let left = d.checked_duration_since(Instant::now()).ok_or_else(|| Status::new(Code::DeadlineExceeded, "deadline exceeded"))?;
                    match inc.rx.recv_timeout(left) {
                        Ok(c) => c,
                        Err(RecvTimeoutError::Timeout) => return Err(Status::new(Code::DeadlineExceeded, "deadline exceeded")),
                        Err(RecvTimeoutError::Disconnected) => Chunk::Failed,
                    }
                }
                None => inc.rx.recv().unwrap_or_else(|_| Chunk::Failed),
            };
            match chunk {
                Chunk::Data(c) => inc.decoder.push(&c),
                Chunk::End => {
                    if !inc.decoder.is_empty() {
                        return Err(Status::new(Code::Internal, "truncated request message"));
                    }
                    return Ok(None);
                }
                Chunk::Failed => return Err(self.gone()),
            }
        }
    }

    /// Opens the response stream when it is not open yet. Needs the sink lock held by the caller.
    fn open(&self, sink: &mut Option<BodySink>) -> Result<(), Status> {
        let mut st = self.inner.state.lock().unwrap();
        if st.finished {
            return Err(Status::new(Code::Cancelled, "call already finished"));
        }
        if sink.is_none() {
            let Some(responder) = st.responder.take() else { return Err(self.gone()) };
            let mut head = Response::new(200).header("content-type", "application/grpc").header("grpc-accept-encoding", "identity,gzip,deflate");
            if let Some(e) = &st.send_encoding {
                head = head.header("grpc-encoding", e);
            }
            for (n, v) in st.response_headers.iter().filter(|(n, _)| !is_reserved_header(n)) {
                head = head.header(n, v);
            }
            *sink = Some(responder.stream(head));
        }
        Ok(())
    }

    /// Sends one response message.
    pub fn send(&self, msg: &[u8]) -> Result<(), Status> {
        let mut guard = self.inner.sink.lock().unwrap();
        self.open(&mut guard)?;
        let enc = self.inner.state.lock().unwrap().send_encoding.clone();
        match guard.as_mut() {
            Some(sink) => sink.write(&frame_message_with(msg, enc.as_deref())).map_err(|_| self.gone()),
            None => Err(self.gone()),
        }
    }

    /// Completes the call with `status` (idempotent).
    pub fn finish(&self, status: &Status) {
        let mut guard = self.inner.sink.lock().unwrap();
        let mut st = self.inner.state.lock().unwrap();
        if st.finished {
            return;
        }
        st.finished = true;
        let mut t: Vec<(String, String)> = vec![("grpc-status".to_string(), (status.code as i32).to_string())];
        if !status.message.is_empty() {
            t.push(("grpc-message".to_string(), encode_grpc_message(&status.message)));
        }
        t.extend(st.trailers.iter().filter(|(n, _)| !is_reserved_header(n)).cloned());
        t.extend(status.metadata.iter().filter(|(n, _)| !is_reserved_header(n)).cloned());
        if let Some(sink) = guard.take() {
            let refs: Vec<(&str, &str)> = t.iter().map(|(n, v)| (n.as_str(), v.as_str())).collect();
            sink.finish_with_trailers(&refs);
        } else if let Some(responder) = st.responder.take() {
            // no message was sent: a trailers-only response
            let mut r = Response::new(200).header("content-type", "application/grpc");
            for (n, v) in &t {
                r = r.header(n, v);
            }
            responder.send(r);
        }
    }
}

/// A running gRPC server.
pub struct GrpcServer {
    handle: Mutex<Option<simoom::ServerHandle>>,
    addr: SocketAddr,
}

impl GrpcServer {
    /// Binds `addr` and serves `service` (plaintext h2c, or TLS when `tls` is given —
    /// configure the TLS server with ALPN `h2`).
    pub fn bind(addr: &str, config: H2Config, tls: Option<TlsServerConfig>, max_message_size: usize, service: Arc<dyn GrpcService>) -> Result<GrpcServer, Status> {
        let unavailable = |e: String| Status::new(Code::Unavailable, e);
        let mut limits = simoom::Limits::default();
        // A call starts with its first request bytes, whatever their number, and a stream never
        // ends because of its length or a quiet spell: messages are bounded by `max_message_size`.
        limits.body_buffer = 1;
        limits.max_body = 1 << 62;
        limits.min_body_rate = 0;
        limits.keep_alive_timeout = Duration::from_secs(3600);
        limits.max_concurrent_streams = config.max_concurrent_streams;
        limits.header_table_size = config.header_table_size;
        if config.max_header_list_size > 0 {
            limits.max_header_list = config.max_header_list_size;
        }
        limits.write_timeout = config.write_timeout;
        let svc = move |req: &mut Request<'_>, reply: Reply<'_>| -> Outcome { dispatch(&service, max_message_size, req, reply) };
        let builder = simoom::Server::builder().limits(limits).service(svc);
        let builder = match &tls {
            Some(t) => builder.listen_tls(addr).tls(cameleer_http::server_simoom::server_tls(t).map_err(|e| unavailable(e.to_string()))?),
            None => builder.listen(addr),
        };
        let handle = builder.start().map_err(|e| unavailable(format!("cannot bind {}: {}", addr, e)))?;
        let addr = handle.local_addr();
        Ok(GrpcServer { handle: Mutex::new(Some(handle)), addr })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Number of live connections.
    pub fn connection_count(&self) -> usize {
        self.handle.lock().unwrap_or_else(|p| p.into_inner()).as_ref().map_or(0, |h| h.stats().active)
    }

    /// Graceful stop (GOAWAY, wait for in-flight calls up to `timeout`). Returns once the port can be
    /// bound again.
    pub fn stop(&self, timeout: Duration) {
        let handle = self.handle.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(h) = handle {
            h.stop(timeout);
            let _ = std::thread::Builder::new().name("grpc-stop".into()).spawn(move || h.join());
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                if std::net::TcpListener::bind(self.addr).is_ok() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

impl Drop for GrpcServer {
    fn drop(&mut self) {
        self.stop(Duration::from_millis(100));
    }
}

/// Runs on a loop thread: checks the request head, hands the call to its own thread.
fn dispatch(service: &Arc<dyn GrpcService>, max_message_size: usize, req: &mut Request<'_>, reply: Reply<'_>) -> Outcome {
    if req.method() != b"POST" {
        return reply.status(405).header("allow", "POST").empty();
    }
    let ct = req.header("content-type").map(|v| String::from_utf8_lossy(v).into_owned()).unwrap_or_default();
    if !ct.starts_with("application/grpc") {
        return reply.status(415).header("content-type", "text/plain").body_copy(b"gRPC requires content-type application/grpc");
    }
    let path = req.path_str().to_string();
    let (svc, method) = match path.strip_prefix('/').and_then(|p| p.split_once('/')) {
        Some((s, m)) if !s.is_empty() && !m.is_empty() => (s.to_string(), m.to_string()),
        _ => (String::new(), String::new()),
    };
    let text = |name: &str| req.header(name).map(|v| String::from_utf8_lossy(v).into_owned());
    let deadline = text("grpc-timeout").and_then(|t| parse_timeout(&t)).map(|d| Instant::now() + d);
    let encoding = text("grpc-encoding");
    let metadata: Metadata = req
        .headers()
        .map(|(n, v)| (String::from_utf8_lossy(n).to_ascii_lowercase(), String::from_utf8_lossy(v).into_owned()))
        .filter(|(n, _)| !n.starts_with(':') && !is_reserved_header(n))
        .collect();
    let user_agent = text("user-agent");
    let accept_encoding = text("grpc-accept-encoding");
    let peer = req.peer_addr();

    // the request body: complete already, or streamed through a reader thread
    let cancelled = Arc::new(AtomicBool::new(false));
    let (tx, rx) = channel();
    let first = req.body().to_vec();
    match req.take_body() {
        Some(mut src) => {
            if !first.is_empty() {
                let _ = tx.send(Chunk::Data(first));
            }
            let gone = cancelled.clone();
            let started = pool().execute(move || {
                let mut buf = vec![0u8; 16 * 1024];
                loop {
                    match src.read(&mut buf) {
                        Ok(0) => {
                            let _ = tx.send(Chunk::End);
                            return;
                        }
                        Ok(n) => {
                            if tx.send(Chunk::Data(buf[..n].to_vec())).is_err() {
                                return;
                            }
                        }
                        Err(_) => {
                            gone.store(true, Ordering::SeqCst);
                            let _ = tx.send(Chunk::Failed);
                            return;
                        }
                    }
                }
            });
            if !started {
                return reply.status(503).body_static(b"cannot start a thread for the request body");
            }
        }
        None => {
            if !first.is_empty() {
                let _ = tx.send(Chunk::Data(first));
            }
            let _ = tx.send(Chunk::End);
        }
    }

    let responder = reply.defer();
    let flag = cancelled.clone();
    responder.on_cancel(move || flag.store(true, Ordering::SeqCst));
    let call = ServerCall {
        inner: Arc::new(CallInner {
            service: svc,
            method,
            metadata,
            user_agent,
            deadline,
            peer,
            accept_encoding,
            incoming: Mutex::new(Incoming { rx, decoder: MessageDecoder::new(encoding, max_message_size) }),
            sink: Mutex::new(None),
            state: Mutex::new(CallState { send_encoding: None, finished: false, response_headers: Vec::new(), trailers: Vec::new(), responder: Some(responder) }),
            cancelled,
            observer: AtomicI64::new(0),
        }),
    };
    if call.service().is_empty() {
        call.finish(&Status::new(Code::Unimplemented, format!("malformed method path {}", path)));
        return Outcome::Deferred;
    }
    let (service, c2) = (service.clone(), call.clone());
    if !pool().execute(move || run(service, c2)) {
        call.finish(&Status::new(Code::ResourceExhausted, "cannot start a thread for the call"));
    }
    Outcome::Deferred
}

/// The handler's status; a panic is an internal error.
fn guarded(service: &Arc<dyn GrpcService>, call: &ServerCall) -> Status {
    catch_unwind(AssertUnwindSafe(|| service.call(call))).unwrap_or_else(|_| Status::new(Code::Internal, "handler panicked"))
}

fn run(service: Arc<dyn GrpcService>, call: ServerCall) {
    let Some(deadline) = call.deadline() else {
        let st = guarded(&service, &call);
        call.finish(&st);
        return;
    };
    // run the handler on its own thread so the deadline can be enforced
    let (tx, rx) = channel();
    let (svc, c2) = (service.clone(), call.clone());
    let started = pool().execute(move || {
        let st = guarded(&svc, &c2);
        let _ = tx.send(st);
    });
    if !started {
        call.finish(&Status::new(Code::ResourceExhausted, "cannot start a thread for the handler"));
        return;
    }
    match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(st) => call.finish(&st),
        Err(RecvTimeoutError::Timeout) => call.finish(&Status::new(Code::DeadlineExceeded, "deadline exceeded")),
        Err(RecvTimeoutError::Disconnected) => call.finish(&Status::new(Code::Internal, "handler panicked")),
    }
}

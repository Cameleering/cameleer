//! gRPC server (byte-level messages) on top of [`crate::h2::H2Server`].

use std::net::SocketAddr;
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cameleer_tls::TlsServerConfig;

use super::{encode_grpc_message, frame_message_with, is_reserved_header, parse_timeout, Code, MessageDecoder, Metadata, Status};
use crate::h2::{H2Config, H2Error, H2Server, H2Stream, ServerRequest};

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

struct CallState {
    send_encoding: Option<String>,
    headers_sent: bool,
    finished: bool,
    response_headers: Metadata,
    trailers: Metadata,
}

struct CallInner {
    stream: H2Stream,
    service: String,
    method: String,
    metadata: Metadata,
    user_agent: Option<String>,
    deadline: Option<Instant>,
    peer: Option<SocketAddr>,
    accept_encoding: Option<String>,
    decoder: Mutex<MessageDecoder>,
    state: Mutex<CallState>,
    observer: std::sync::atomic::AtomicI64,
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
        Some(self.inner.observer.load(std::sync::atomic::Ordering::Relaxed)).filter(|i| *i != 0)
    }
    /// Associates a response observer id with this call.
    pub fn set_observer_id(&self, id: i64) {
        self.inner.observer.store(id, std::sync::atomic::Ordering::Relaxed);
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
        self.inner.state.lock().unwrap().finished || self.inner.stream.reset_code().is_some()
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

    fn map_err(&self, e: H2Error) -> Status {
        match e {
            H2Error::StreamReset(_) => Status::new(Code::Cancelled, "call cancelled by the client"),
            other => Status::from(other),
        }
    }

    /// Receives the next request message; `Ok(None)` when the client half-closed.
    pub fn recv(&self) -> Result<Option<Vec<u8>>, Status> {
        let mut dec = self.inner.decoder.lock().unwrap();
        loop {
            if let Some(m) = dec.next_message()? {
                return Ok(Some(m));
            }
            let left = match self.inner.deadline {
                Some(d) => Some(d.checked_duration_since(Instant::now()).ok_or_else(|| Status::new(Code::DeadlineExceeded, "deadline exceeded"))?),
                None => None,
            };
            match self.inner.stream.recv_data(left) {
                Ok(Some(c)) => dec.push(&c),
                Ok(None) => {
                    if !dec.is_empty() {
                        return Err(Status::new(Code::Internal, "truncated request message"));
                    }
                    return Ok(None);
                }
                Err(H2Error::Timeout) => return Err(Status::new(Code::DeadlineExceeded, "deadline exceeded")),
                Err(e) => return Err(self.map_err(e)),
            }
        }
    }

    fn ensure_headers(&self) -> Result<(), Status> {
        let mut st = self.inner.state.lock().unwrap();
        if st.finished {
            return Err(Status::new(Code::Cancelled, "call already finished"));
        }
        if !st.headers_sent {
            let mut h = vec![
                (":status".to_string(), "200".to_string()),
                ("content-type".to_string(), "application/grpc".to_string()),
                ("grpc-accept-encoding".to_string(), "identity,gzip,deflate".to_string()),
            ];
            if let Some(e) = &st.send_encoding {
                h.push(("grpc-encoding".to_string(), e.clone()));
            }
            h.extend(st.response_headers.iter().filter(|(n, _)| !is_reserved_header(n)).cloned());
            self.inner.stream.send_headers(&h, false).map_err(|e| self.map_err(e))?;
            st.headers_sent = true;
        }
        Ok(())
    }

    /// Sends one response message.
    pub fn send(&self, msg: &[u8]) -> Result<(), Status> {
        self.ensure_headers()?;
        let enc = self.inner.state.lock().unwrap().send_encoding.clone();
        self.inner.stream.send_data(&frame_message_with(msg, enc.as_deref()), false).map_err(|e| self.map_err(e))
    }

    /// Completes the call with `status` (idempotent).
    pub fn finish(&self, status: &Status) {
        let mut st = self.inner.state.lock().unwrap();
        if st.finished {
            return;
        }
        st.finished = true;
        let mut t = Vec::new();
        if !st.headers_sent {
            t.push((":status".to_string(), "200".to_string()));
            t.push(("content-type".to_string(), "application/grpc".to_string()));
        }
        t.push(("grpc-status".to_string(), (status.code as i32).to_string()));
        if !status.message.is_empty() {
            t.push(("grpc-message".to_string(), encode_grpc_message(&status.message)));
        }
        t.extend(st.trailers.iter().filter(|(n, _)| !is_reserved_header(n)).cloned());
        t.extend(status.metadata.iter().filter(|(n, _)| !is_reserved_header(n)).cloned());
        st.headers_sent = true;
        let _ = self.inner.stream.send_headers(&t, true);
    }
}

/// A running gRPC server.
pub struct GrpcServer {
    h2: H2Server,
}

impl GrpcServer {
    /// Binds `addr` and serves `service` (plaintext h2c, or TLS when `tls` is given —
    /// configure the TLS server with ALPN `h2`).
    pub fn bind(addr: &str, config: H2Config, tls: Option<TlsServerConfig>, max_message_size: usize, service: Arc<dyn GrpcService>) -> Result<GrpcServer, Status> {
        let h2 = H2Server::bind(addr, config, tls, move |req: ServerRequest, stream: H2Stream| handle(&service, max_message_size, req, stream))
            .map_err(|e| Status::new(Code::Unavailable, e.to_string()))?;
        Ok(GrpcServer { h2 })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.h2.local_addr()
    }

    /// Graceful stop (GOAWAY, wait for in-flight calls up to `timeout`).
    pub fn stop(&self, timeout: Duration) {
        self.h2.stop_graceful(timeout);
    }

    /// The underlying HTTP/2 server.
    pub fn h2(&self) -> &H2Server {
        &self.h2
    }
}

fn handle(service: &Arc<dyn GrpcService>, max_message_size: usize, req: ServerRequest, stream: H2Stream) {
    if req.method != "POST" {
        let _ = ServerRequest::respond(&stream, 405, &[("allow", "POST")], b"");
        return;
    }
    let ct = req.header("content-type").unwrap_or("").to_string();
    if !ct.starts_with("application/grpc") {
        let _ = ServerRequest::respond(&stream, 415, &[("content-type", "text/plain")], b"gRPC requires content-type application/grpc");
        return;
    }
    let (svc, method) = match req.path.strip_prefix('/').and_then(|p| p.split_once('/')) {
        Some((s, m)) if !s.is_empty() && !m.is_empty() => (s.to_string(), m.to_string()),
        _ => (String::new(), String::new()),
    };
    let deadline = req.header("grpc-timeout").and_then(parse_timeout).map(|d| Instant::now() + d);
    let encoding = req.header("grpc-encoding").map(|s| s.to_string());
    let call = ServerCall {
        inner: Arc::new(CallInner {
            stream,
            service: svc,
            method,
            metadata: req.headers.iter().filter(|(n, _)| !is_reserved_header(n)).cloned().collect(),
            user_agent: req.header("user-agent").map(|s| s.to_string()),
            deadline,
            peer: req.peer_addr,
            accept_encoding: req.header("grpc-accept-encoding").map(|s| s.to_string()),
            decoder: Mutex::new(MessageDecoder::new(encoding, max_message_size)),
            state: Mutex::new(CallState { send_encoding: None, headers_sent: false, finished: false, response_headers: Vec::new(), trailers: Vec::new() }),
            observer: std::sync::atomic::AtomicI64::new(0),
        }),
    };
    if call.service().is_empty() {
        call.finish(&Status::new(Code::Unimplemented, format!("malformed method path {}", req.path)));
        return;
    }
    match deadline {
        None => {
            let st = service.call(&call);
            call.finish(&st);
        }
        Some(d) => {
            // run the handler on its own thread so the deadline can be enforced
            let (tx, rx) = channel();
            let (svc, c2) = (service.clone(), call.clone());
            let spawned = std::thread::Builder::new().name("grpc-call".into()).spawn(move || {
                let st = svc.call(&c2);
                let _ = tx.send(st);
            });
            if spawned.is_err() {
                call.finish(&Status::new(Code::ResourceExhausted, "cannot spawn handler thread"));
                return;
            }
            match rx.recv_timeout(d.saturating_duration_since(Instant::now())) {
                Ok(st) => call.finish(&st),
                Err(RecvTimeoutError::Timeout) => call.finish(&Status::new(Code::DeadlineExceeded, "deadline exceeded")),
                Err(RecvTimeoutError::Disconnected) => call.finish(&Status::new(Code::Internal, "handler panicked")),
            }
        }
    }
}

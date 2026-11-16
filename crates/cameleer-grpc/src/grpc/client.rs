//! gRPC client (byte-level messages) on [simoom-client](https://crates.io/crates/simoom-client)'s
//! full-duplex HTTP/2 calls: one channel per target, connections pooled and re-established by the
//! engine (`docs/design/simoom-adoption.md`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use simoom_client as sc;

use cameleer_tls::TlsClientConfig;

use super::{code_from_http_status, format_timeout, frame_message_with, is_reserved_header, status_from_trailers, Code, MessageDecoder, Metadata, Status};
use crate::h2::H2Config;

/// User agent sent with every call.
pub const USER_AGENT: &str = concat!("grpc-cameleer/", env!("CARGO_PKG_VERSION"));

/// A gRPC channel to one target (`host:port`), plaintext (h2c) or TLS.
///
/// ```no_run
/// use cameleer_grpc::grpc::GrpcClient;
/// let client = GrpcClient::plaintext("127.0.0.1:50051");
/// let (reply, _md) = client.unary("/helloworld.Greeter/SayHello", b"\x0a\x03Bob", &vec![], None).unwrap();
/// ```
pub struct GrpcClient {
    addr: String,
    tls: Option<(String, TlsClientConfig)>,
    /// HTTP/2 settings used for new connections.
    pub config: H2Config,
    /// Maximum accepted response message size (default 4 MiB).
    pub max_message_size: usize,
    /// `grpc-encoding` used for request messages (`gzip` / `deflate`; None = identity).
    pub send_compression: Option<String>,
    /// Plaintext connections start with an HTTP/1.1 `Upgrade: h2c` request
    /// (`negotiationType=PLAINTEXT_UPGRADE`) instead of prior knowledge.
    pub upgrade: bool,
    engine: Mutex<Option<sc::Client>>,
    /// Opens streams one at a time, so that calls racing on a cold channel share the first connection
    /// instead of each opening their own.
    open: Mutex<()>,
}

impl GrpcClient {
    /// Plaintext channel (HTTP/2 prior knowledge).
    pub fn plaintext(addr: &str) -> GrpcClient {
        GrpcClient { addr: addr.to_string(), tls: None, config: H2Config::default(), max_message_size: 4 << 20, send_compression: None, upgrade: false, engine: Mutex::new(None), open: Mutex::new(()) }
    }

    /// TLS channel; the server certificate and its name are always verified against the trust anchors of `tls`.
    pub fn tls(addr: &str, server_name: &str, tls: TlsClientConfig) -> GrpcClient {
        GrpcClient { tls: Some((server_name.to_string(), tls)), ..GrpcClient::plaintext(addr) }
    }

    /// The target address.
    pub fn target(&self) -> &str {
        &self.addr
    }

    /// The engine behind the channel, created on first use (the public settings can be changed until then).
    fn engine(&self) -> Result<sc::Client, Status> {
        let mut guard = self.engine.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(c) = guard.as_ref() {
            return Ok(c.clone());
        }
        let mut cfg = sc::Config::default();
        cfg.version = sc::HttpVersion::Http2;
        cfg.h2c_upgrade = self.upgrade;
        cfg.connect_timeout = Some(self.config.connect_timeout);
        // a call waits as long as its deadline says: no idle timeout of its own
        cfg.read_timeout = None;
        cfg.write_timeout = Some(self.config.write_timeout);
        cfg.decompress = false;
        cfg.h2.initial_window_size = self.config.initial_window_size;
        cfg.h2.connection_window_size = self.config.connection_window_size;
        cfg.h2.max_frame_size = self.config.max_frame_size;
        cfg.h2.header_table_size = self.config.header_table_size;
        if self.config.max_header_list_size > 0 {
            cfg.h2.max_header_list_size = self.config.max_header_list_size;
        }
        let tls = match &self.tls {
            Some((name, c)) => cameleer_http::client_engine::tls_options(Some(c), Some(name), true),
            None => cameleer_http::client_engine::tls_options(None, None, false),
        }
        .map_err(|e| Status::new(Code::InvalidArgument, e.to_string()))?;
        let client = sc::ClientBuilder::default().config(cfg).tls(tls).build();
        *guard = Some(client.clone());
        Ok(client)
    }

    /// Closes the pooled connections (calls in flight finish); the next call connects again.
    pub fn close(&self) {
        if let Some(c) = self.engine.lock().unwrap_or_else(|p| p.into_inner()).take() {
            c.close_idle();
        }
    }

    /// Starts a call on `path` (`/package.Service/Method`).
    pub fn start_call(&self, path: &str, metadata: &Metadata, deadline: Option<Duration>) -> Result<ClientCall, Status> {
        let engine = self.engine()?;
        let mut headers = sc::Headers::new();
        headers.append("content-type", "application/grpc");
        headers.append("te", "trailers");
        headers.append("user-agent", USER_AGENT);
        headers.append("grpc-accept-encoding", "identity,gzip,deflate");
        if let Some(d) = deadline {
            headers.append("grpc-timeout", format_timeout(d));
        }
        if let Some(e) = &self.send_compression {
            headers.append("grpc-encoding", e);
        }
        for (k, v) in metadata {
            let k = k.to_ascii_lowercase();
            if !is_reserved_header(&k) {
                headers.append(&k, v);
            }
        }
        let url = format!("{}://{}{}", if self.tls.is_some() { "https" } else { "http" }, self.addr, path);
        let opened = {
            let _one_at_a_time = self.open.lock().unwrap_or_else(|p| p.into_inner());
            engine.call("POST", &url, &headers)
        };
        let call = opened.map_err(|e| {
            if e.is_connect() || matches!(e.kind, sc::ErrorKind::Tls | sc::ErrorKind::Closed | sc::ErrorKind::Io(_) | sc::ErrorKind::Timeout(sc::TimeoutKind::Tls)) {
                Status::new(Code::Unavailable, format!("cannot connect to {}: {}", self.addr, e))
            } else {
                Status::from(e)
            }
        })?;
        let sender = call.sender();
        Ok(ClientCall {
            send_encoding: self.send_compression.clone(),
            call,
            sender,
            cancelled: Arc::new(AtomicBool::new(false)),
            deadline: deadline.map(|d| Instant::now() + d),
            decoder: MessageDecoder::new(None, self.max_message_size),
            buf: Vec::new(),
            headers: None,
            status: None,
        })
    }

    /// Unary call: returns the response message and the response metadata (headers + trailers).
    pub fn unary(&self, path: &str, request: &[u8], metadata: &Metadata, deadline: Option<Duration>) -> Result<(Vec<u8>, Metadata), Status> {
        let mut call = self.start_call(path, metadata, deadline)?;
        call.send(request)?;
        call.close_send()?;
        let first = call.recv()?;
        let msg = first.ok_or_else(|| Status::new(Code::Internal, "no response message received for unary call"))?;
        if call.recv()?.is_some() {
            return Err(Status::new(Code::Internal, "more than one response message for unary call"));
        }
        Ok((msg, call.metadata()))
    }

    /// Server-streaming call: `on_message` is invoked for each response message.
    pub fn server_streaming(
        &self,
        path: &str,
        request: &[u8],
        metadata: &Metadata,
        deadline: Option<Duration>,
        on_message: &mut dyn FnMut(Vec<u8>) -> Result<(), Status>,
    ) -> Result<Metadata, Status> {
        let mut call = self.start_call(path, metadata, deadline)?;
        call.send(request)?;
        call.close_send()?;
        while let Some(m) = call.recv()? {
            if let Err(s) = on_message(m) {
                call.cancel();
                return Err(s);
            }
        }
        Ok(call.metadata())
    }

    /// Client-streaming (or bidi) call sending all `requests`, collecting all responses.
    pub fn streaming(&self, path: &str, requests: &[Vec<u8>], metadata: &Metadata, deadline: Option<Duration>) -> Result<(Vec<Vec<u8>>, Metadata), Status> {
        let mut call = self.start_call(path, metadata, deadline)?;
        for r in requests {
            call.send(r)?;
        }
        call.close_send()?;
        let mut out = Vec::new();
        while let Some(m) = call.recv()? {
            out.push(m);
        }
        Ok((out, call.metadata()))
    }
}

/// How long `recv` waits before it looks at the cancel flag and the deadline again.
const WAIT_SLICE: Duration = Duration::from_millis(100);

/// An in-flight call.
pub struct ClientCall {
    send_encoding: Option<String>,
    call: sc::Call,
    sender: sc::CallSender,
    /// Set by `cancel` (on either half) so a thread blocked in `recv` notices within one slice:
    /// the engine's cancel does not wake a reader that is already waiting.
    cancelled: Arc<AtomicBool>,
    deadline: Option<Instant>,
    decoder: MessageDecoder,
    buf: Vec<u8>,
    headers: Option<Metadata>,
    status: Option<Status>,
}

impl ClientCall {
    fn expired(&self) -> Result<(), Status> {
        match self.deadline {
            Some(d) if Instant::now() >= d => {
                self.call.cancel();
                Err(Status::new(Code::DeadlineExceeded, "deadline exceeded"))
            }
            _ => Ok(()),
        }
    }

    fn map_err(&self, e: sc::Error) -> Status {
        if e.is_timeout() {
            self.call.cancel();
            return Status::new(Code::DeadlineExceeded, "deadline exceeded");
        }
        Status::from(e)
    }

    /// Sends one request message.
    pub fn send(&self, msg: &[u8]) -> Result<(), Status> {
        self.expired()?;
        self.sender.send(&frame_message_with(msg, self.send_encoding.as_deref())).map_err(|e| self.map_err(e))
    }

    /// Half-closes the request stream.
    pub fn close_send(&self) -> Result<(), Status> {
        self.sender.finish().map_err(|e| self.map_err(e))
    }

    /// A handle that sends requests while another thread receives responses.
    pub fn sender(&self) -> CallSender {
        CallSender { sender: self.sender.clone(), cancelled: self.cancelled.clone(), send_encoding: self.send_encoding.clone() }
    }

    /// Cancels the call (RST_STREAM CANCEL).
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.call.cancel();
    }

    /// The end of the next wait: the deadline, or one slice from now when that is sooner.
    fn slice(&self) -> Instant {
        let slice = Instant::now() + WAIT_SLICE;
        self.deadline.map_or(slice, |d| d.min(slice))
    }

    /// What a failed wait means: `None` to wait on (a slice ended with the deadline still ahead).
    fn failure(&self, e: sc::Error) -> Option<Status> {
        if self.cancelled.load(Ordering::SeqCst) {
            return Some(Status::new(Code::Cancelled, "call cancelled"));
        }
        if e.is_timeout() {
            return match self.deadline {
                Some(d) if Instant::now() >= d => {
                    self.call.cancel();
                    Some(Status::new(Code::DeadlineExceeded, "deadline exceeded"))
                }
                _ => None,
            };
        }
        Some(Status::from(e))
    }

    /// Response headers (available after the first `recv`).
    pub fn response_headers(&self) -> Option<&Metadata> {
        self.headers.as_ref()
    }

    /// The final status (after `recv` returned `None` or an error).
    pub fn status(&self) -> Option<&Status> {
        self.status.as_ref()
    }

    /// Response metadata: user headers plus trailing metadata.
    pub fn metadata(&self) -> Metadata {
        let mut m: Metadata = self.headers.clone().unwrap_or_default().into_iter().filter(|(n, _)| !is_reserved_header(n)).collect();
        if let Some(s) = &self.status {
            m.extend(s.metadata.iter().cloned());
        }
        m
    }

    fn finish(&mut self, status: Status) -> Result<Option<Vec<u8>>, Status> {
        self.status = Some(status.clone());
        if status.is_ok() {
            Ok(None)
        } else {
            Err(status)
        }
    }

    /// Receives the next response message; `Ok(None)` when the call completed with OK,
    /// `Err(status)` when it failed.
    pub fn recv(&mut self) -> Result<Option<Vec<u8>>, Status> {
        if let Some(s) = &self.status {
            return if s.is_ok() { Ok(None) } else { Err(s.clone()) };
        }
        if self.headers.is_none() {
            self.expired()?;
            let (http_status, h) = loop {
                if self.cancelled.load(Ordering::SeqCst) {
                    return self.finish(Status::new(Code::Cancelled, "call cancelled"));
                }
                let until = self.slice();
                match self.call.response_until(Some(until)) {
                    Ok((s, h)) => break (s, h.to_strings()),
                    Err(e) => match self.failure(e) {
                        Some(st) => return self.finish(st),
                        None => continue,
                    },
                }
            };
            if http_status != 200 {
                return self.finish(Status::new(code_from_http_status(http_status), format!("HTTP status {}", http_status)));
            }
            if let Some(st) = status_from_trailers(&h) {
                // trailers-only response
                self.headers = Some(Vec::new());
                return self.finish(st);
            }
            let ct = h.iter().find(|(n, _)| n == "content-type").map(|(_, v)| v.as_str()).unwrap_or("");
            if !ct.starts_with("application/grpc") {
                return self.finish(Status::new(Code::Unknown, format!("invalid content-type: {}", ct)));
            }
            self.decoder.encoding = h.iter().find(|(n, _)| n == "grpc-encoding").map(|(_, v)| v.clone());
            self.headers = Some(h);
        }
        if self.buf.is_empty() {
            self.buf = vec![0u8; 32 * 1024];
        }
        loop {
            match self.decoder.next_message() {
                Ok(Some(m)) => return Ok(Some(m)),
                Ok(None) => {}
                Err(s) => {
                    self.cancel();
                    return self.finish(s);
                }
            }
            if self.cancelled.load(Ordering::SeqCst) {
                return self.finish(Status::new(Code::Cancelled, "call cancelled"));
            }
            self.expired()?;
            let until = self.slice();
            match self.call.read_until(&mut self.buf, Some(until)) {
                Ok(0) => {
                    let trailers = self.call.trailers().map(|t| t.to_strings()).unwrap_or_default();
                    let st = status_from_trailers(&trailers).unwrap_or_else(|| Status::new(Code::Internal, "missing grpc-status trailer"));
                    if !self.decoder.is_empty() && st.is_ok() {
                        return self.finish(Status::new(Code::Internal, "truncated response message"));
                    }
                    return self.finish(st);
                }
                Ok(n) => self.decoder.push(&self.buf[..n]),
                Err(e) => match self.failure(e) {
                    Some(st) => return self.finish(st),
                    None => {}
                },
            }
        }
    }
}

/// Sending half of a [`ClientCall`] (for streaming calls driven from another thread).
#[derive(Clone)]
pub struct CallSender {
    sender: sc::CallSender,
    cancelled: Arc<AtomicBool>,
    send_encoding: Option<String>,
}

impl CallSender {
    /// Sends one request message.
    pub fn send(&self, msg: &[u8]) -> Result<(), Status> {
        self.sender.send(&frame_message_with(msg, self.send_encoding.as_deref())).map_err(Status::from)
    }
    /// Half-closes the request stream.
    pub fn close_send(&self) -> Result<(), Status> {
        self.sender.finish().map_err(Status::from)
    }
    /// Cancels the call.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.sender.cancel();
    }
}

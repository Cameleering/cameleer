//! gRPC client (byte-level messages) over a lazily (re)connected HTTP/2 connection.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use camel_tls::TlsClientConfig;

use super::{code_from_http_status, format_timeout, frame_message_with, is_reserved_header, status_from_trailers, Code, MessageDecoder, Metadata, Status};
use crate::h2::{ErrorCode, H2Config, H2Connection, H2Error, H2Stream};

/// User agent sent with every call.
pub const USER_AGENT: &str = concat!("grpc-rust-camel/", env!("CARGO_PKG_VERSION"));

/// A gRPC channel to one target (`host:port`), plaintext (h2c) or TLS.
///
/// ```no_run
/// use camel_grpc::grpc::GrpcClient;
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
    conn: Mutex<Option<H2Connection>>,
}

impl GrpcClient {
    /// Plaintext channel (HTTP/2 prior knowledge).
    pub fn plaintext(addr: &str) -> GrpcClient {
        GrpcClient { addr: addr.to_string(), tls: None, config: H2Config::default(), max_message_size: 4 << 20, send_compression: None, conn: Mutex::new(None) }
    }

    /// TLS channel; `tls` should offer ALPN `h2` (added automatically when absent is not
    /// possible, so build it with `.alpn_protocols(&["h2"])`).
    pub fn tls(addr: &str, server_name: &str, tls: TlsClientConfig) -> GrpcClient {
        GrpcClient { tls: Some((server_name.to_string(), tls)), ..GrpcClient::plaintext(addr) }
    }

    /// The target address.
    pub fn target(&self) -> &str {
        &self.addr
    }

    /// The current connection, connecting (or reconnecting) when needed.
    pub fn connection(&self) -> Result<H2Connection, Status> {
        let mut guard = self.conn.lock().unwrap();
        if let Some(c) = guard.as_ref() {
            if !c.is_closed() {
                return Ok(c.clone());
            }
        }
        let c = match &self.tls {
            None => H2Connection::connect(&self.addr, self.config.clone()),
            Some((name, cfg)) => H2Connection::connect_tls(&self.addr, name, cfg, self.config.clone()),
        }
        .map_err(|e| Status::new(Code::Unavailable, format!("cannot connect to {}: {}", self.addr, e)))?;
        *guard = Some(c.clone());
        Ok(c)
    }

    /// Closes the underlying connection.
    pub fn close(&self) {
        if let Some(c) = self.conn.lock().unwrap().take() {
            c.close();
        }
    }

    /// Starts a call on `path` (`/package.Service/Method`).
    pub fn start_call(&self, path: &str, metadata: &Metadata, deadline: Option<Duration>) -> Result<ClientCall, Status> {
        let conn = self.connection()?;
        let mut headers = vec![
            (":method".to_string(), "POST".to_string()),
            (":scheme".to_string(), if conn.is_tls() { "https" } else { "http" }.to_string()),
            (":path".to_string(), path.to_string()),
            (":authority".to_string(), conn.authority().to_string()),
            ("content-type".to_string(), "application/grpc".to_string()),
            ("te".to_string(), "trailers".to_string()),
            ("user-agent".to_string(), USER_AGENT.to_string()),
            ("grpc-accept-encoding".to_string(), "identity,gzip,deflate".to_string()),
        ];
        if let Some(d) = deadline {
            headers.push(("grpc-timeout".to_string(), format_timeout(d)));
        }
        if let Some(e) = &self.send_compression {
            headers.push(("grpc-encoding".to_string(), e.clone()));
        }
        for (k, v) in metadata {
            let k = k.to_ascii_lowercase();
            if !is_reserved_header(&k) {
                headers.push((k, v.clone()));
            }
        }
        let deadline = deadline.map(|d| Instant::now() + d);
        let stream = match deadline {
            Some(d) => conn.open_stream_timeout(&headers, false, d.saturating_duration_since(Instant::now())),
            None => conn.open_stream(&headers, false),
        }
        .map_err(Status::from)?;
        Ok(ClientCall {
            send_encoding: self.send_compression.clone(),
            stream: Arc::new(stream),
            deadline,
            decoder: MessageDecoder::new(None, self.max_message_size),
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

/// An in-flight call.
pub struct ClientCall {
    send_encoding: Option<String>,
    stream: Arc<H2Stream>,
    deadline: Option<Instant>,
    decoder: MessageDecoder,
    headers: Option<Metadata>,
    status: Option<Status>,
}

impl ClientCall {
    fn left(&self) -> Result<Option<Duration>, Status> {
        match self.deadline {
            None => Ok(None),
            Some(d) => {
                let now = Instant::now();
                if now >= d {
                    self.stream.reset(ErrorCode::CANCEL);
                    return Err(Status::new(Code::DeadlineExceeded, "deadline exceeded"));
                }
                Ok(Some(d - now))
            }
        }
    }

    fn map_err(&self, e: H2Error) -> Status {
        if e == H2Error::Timeout {
            self.stream.reset(ErrorCode::CANCEL);
            return Status::new(Code::DeadlineExceeded, "deadline exceeded");
        }
        Status::from(e)
    }

    /// Sends one request message.
    pub fn send(&self, msg: &[u8]) -> Result<(), Status> {
        let left = self.left()?;
        self.stream.send_data_timeout(&frame_message_with(msg, self.send_encoding.as_deref()), false, left).map_err(|e| self.map_err(e))
    }

    /// Half-closes the request stream.
    pub fn close_send(&self) -> Result<(), Status> {
        self.stream.send_data(&[], true).map_err(|e| self.map_err(e))
    }

    /// A handle that sends requests while another thread receives responses.
    pub fn sender(&self) -> CallSender {
        CallSender { stream: self.stream.clone(), send_encoding: self.send_encoding.clone() }
    }

    /// Cancels the call (RST_STREAM CANCEL).
    pub fn cancel(&self) {
        self.stream.reset(ErrorCode::CANCEL);
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
            let left = self.left()?;
            let h = match self.stream.recv_headers(left).map_err(|e| self.map_err(e))? {
                Some(h) => h,
                None => return self.finish(Status::new(Code::Internal, "stream closed without response headers")),
            };
            let http_status: u16 = h.iter().find(|(n, _)| n == ":status").and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
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
        loop {
            match self.decoder.next_message() {
                Ok(Some(m)) => return Ok(Some(m)),
                Ok(None) => {}
                Err(s) => {
                    self.cancel();
                    return self.finish(s);
                }
            }
            let left = self.left()?;
            match self.stream.recv_data(left) {
                Ok(Some(chunk)) => self.decoder.push(&chunk),
                Ok(None) => {
                    let left = self.left()?;
                    let trailers = self.stream.recv_headers(left).map_err(|e| self.map_err(e))?.unwrap_or_default();
                    let st = status_from_trailers(&trailers).unwrap_or_else(|| Status::new(Code::Internal, "missing grpc-status trailer"));
                    if !self.decoder.is_empty() && st.is_ok() {
                        return self.finish(Status::new(Code::Internal, "truncated response message"));
                    }
                    return self.finish(st);
                }
                Err(e) => {
                    let s = self.map_err(e);
                    return self.finish(s);
                }
            }
        }
    }
}

/// Sending half of a [`ClientCall`] (for streaming calls driven from another thread).
#[derive(Clone)]
pub struct CallSender {
    stream: Arc<H2Stream>,
    send_encoding: Option<String>,
}

impl CallSender {
    /// Sends one request message.
    pub fn send(&self, msg: &[u8]) -> Result<(), Status> {
        self.stream.send_data(&frame_message_with(msg, self.send_encoding.as_deref()), false).map_err(Status::from)
    }
    /// Half-closes the request stream.
    pub fn close_send(&self) -> Result<(), Status> {
        self.stream.send_data(&[], true).map_err(Status::from)
    }
    /// Cancels the call.
    pub fn cancel(&self) {
        self.stream.reset(ErrorCode::CANCEL);
    }
}

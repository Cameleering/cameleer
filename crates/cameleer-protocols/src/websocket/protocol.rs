//! RFC 6455 WebSocket protocol: opening handshake (server and client), framing with
//! masking, fragmentation, ping/pong and the closing handshake.
//!
//! ```no_run
//! use std::time::Duration;
//! use cameleer_protocols::websocket::{WsConnection, WsMessage};
//! let c = WsConnection::connect("localhost", 9292, "/echo", Duration::from_secs(5)).unwrap();
//! c.send_text("hello").unwrap();
//! if let Some(WsMessage::Text(reply)) = c.read_message().unwrap() {
//!     println!("{}", reply);
//! }
//! c.close(1000, "bye");
//! ```

use std::io::{BufRead, BufReader, Read, Write};
use std::net::SocketAddr;

use cameleer_tls::camel_support::EndpointTls;
use cameleer_tls::net::NetStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use cameleer_core::dataformat::base64;
use cameleer_core::error::{kinds, CamelError, Result};

use super::sha1::sha1;
use cameleer_tls::net::{connect_stream, io_error, random_bytes, read_line};

/// The GUID appended to the client key (RFC 6455 section 1.3).
pub const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Compute `Sec-WebSocket-Accept` for a `Sec-WebSocket-Key`.
pub fn accept_key(key: &str) -> String {
    base64::encode(&sha1(format!("{}{}", key.trim(), WS_GUID).as_bytes()), false)
}

/// Frame opcodes.
pub mod opcode {
    pub const CONTINUATION: u8 = 0x0;
    pub const TEXT: u8 = 0x1;
    pub const BINARY: u8 = 0x2;
    pub const CLOSE: u8 = 0x8;
    pub const PING: u8 = 0x9;
    pub const PONG: u8 = 0xA;
}

/// A single frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub fin: bool,
    pub opcode: u8,
    pub masked: bool,
    pub payload: Vec<u8>,
}

/// A complete (reassembled) data message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WsMessage {
    Text(String),
    Binary(Vec<u8>),
}

fn proto_err(msg: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, msg.to_string())
}

/// Read one frame (unmasking the payload). `max` limits the payload size.
pub fn read_frame<R: Read>(r: &mut R, max: usize) -> std::io::Result<Frame> {
    let mut h = [0u8; 2];
    r.read_exact(&mut h)?;
    let fin = h[0] & 0x80 != 0;
    if h[0] & 0x70 != 0 {
        return Err(proto_err("reserved bits set"));
    }
    let opcode = h[0] & 0x0F;
    let masked = h[1] & 0x80 != 0;
    let mut len = (h[1] & 0x7F) as u64;
    if len == 126 {
        let mut b = [0u8; 2];
        r.read_exact(&mut b)?;
        len = u16::from_be_bytes(b) as u64;
    } else if len == 127 {
        let mut b = [0u8; 8];
        r.read_exact(&mut b)?;
        len = u64::from_be_bytes(b);
    }
    if opcode >= 0x8 && (len > 125 || !fin) {
        return Err(proto_err("invalid control frame"));
    }
    if len > max as u64 {
        return Err(std::io::Error::new(std::io::ErrorKind::OutOfMemory, "frame too large"));
    }
    let mut mask = [0u8; 4];
    if masked {
        r.read_exact(&mut mask)?;
    }
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload)?;
    if masked {
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= mask[i % 4];
        }
    }
    Ok(Frame { fin, opcode, masked, payload })
}

/// Encode one frame; `mask` is required for client-to-server frames.
pub fn encode_frame(fin: bool, opcode: u8, payload: &[u8], mask: Option<[u8; 4]>) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 14);
    out.push(if fin { 0x80 } else { 0 } | (opcode & 0x0F));
    let m = if mask.is_some() { 0x80 } else { 0 };
    let len = payload.len();
    if len < 126 {
        out.push(m | len as u8);
    } else if len <= 0xFFFF {
        out.push(m | 126);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        out.push(m | 127);
        out.extend_from_slice(&(len as u64).to_be_bytes());
    }
    match mask {
        Some(k) => {
            out.extend_from_slice(&k);
            out.extend(payload.iter().enumerate().map(|(i, b)| b ^ k[i % 4]));
        }
        None => out.extend_from_slice(payload),
    }
    out
}

/// A parsed HTTP request head (handshake).
#[derive(Clone, Debug, Default)]
pub struct HandshakeRequest {
    pub method: String,
    /// Request target (path and query).
    pub target: String,
    pub headers: Vec<(String, String)>,
}

impl HandshakeRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
    /// The path without the query string.
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or("/")
    }
    /// Whether this is a valid WebSocket upgrade request; returns the client key.
    pub fn websocket_key(&self) -> std::result::Result<&str, &'static str> {
        if !self.method.eq_ignore_ascii_case("GET") {
            return Err("method must be GET");
        }
        if !self.header("Upgrade").map(|v| v.to_ascii_lowercase().contains("websocket")).unwrap_or(false) {
            return Err("missing Upgrade: websocket");
        }
        if !self.header("Connection").map(|v| v.to_ascii_lowercase().contains("upgrade")).unwrap_or(false) {
            return Err("missing Connection: Upgrade");
        }
        if self.header("Sec-WebSocket-Version").map(|v| v.trim()) != Some("13") {
            return Err("unsupported Sec-WebSocket-Version (13 required)");
        }
        self.header("Sec-WebSocket-Key").filter(|k| !k.trim().is_empty()).ok_or("missing Sec-WebSocket-Key")
    }
}

/// Read an HTTP request head.
pub fn read_request<R: BufRead>(r: &mut R) -> std::io::Result<HandshakeRequest> {
    let line = read_line(r, 8192)?.ok_or_else(|| proto_err("connection closed"))?;
    let mut parts = line.split_whitespace();
    let mut req = HandshakeRequest {
        method: parts.next().unwrap_or("").to_string(),
        target: parts.next().unwrap_or("/").to_string(),
        headers: Vec::new(),
    };
    loop {
        let l = read_line(r, 8192)?.ok_or_else(|| proto_err("connection closed"))?;
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            req.headers.push((k.trim().to_string(), v.trim().to_string()));
        }
        if req.headers.len() > 100 {
            return Err(proto_err("too many headers"));
        }
    }
    Ok(req)
}

/// Write the `101 Switching Protocols` response.
pub fn write_accept<W: Write>(w: &mut W, key: &str, protocol: Option<&str>) -> std::io::Result<()> {
    let mut resp = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n",
        accept_key(key)
    );
    if let Some(p) = protocol {
        resp.push_str(&format!("Sec-WebSocket-Protocol: {}\r\n", p));
    }
    resp.push_str("\r\n");
    w.write_all(resp.as_bytes())?;
    w.flush()
}

/// Write an HTTP error response (handshake rejected).
pub fn write_reject<W: Write>(w: &mut W, status: u16, reason: &str, message: &str) -> std::io::Result<()> {
    let resp = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\nSec-WebSocket-Version: 13\r\n\r\n{}",
        status,
        reason,
        message.len(),
        message
    );
    w.write_all(resp.as_bytes())?;
    w.flush()
}

static IDS: AtomicU64 = AtomicU64::new(1);

/// An open WebSocket connection (either side). Sending is thread safe; one thread
/// should read with [`WsConnection::read_message`].
pub struct WsConnection {
    id: String,
    reader: Mutex<BufReader<NetStream>>,
    writer: Mutex<NetStream>,
    client: bool,
    closed: AtomicBool,
    close_sent: AtomicBool,
    remote: Option<SocketAddr>,
    path: String,
    max_message_size: usize,
    max_frame_size: usize,
}

impl WsConnection {
    /// Wrap an upgraded stream. `client` connections mask outgoing frames.
    pub fn new(stream: NetStream, reader: BufReader<NetStream>, client: bool, path: &str) -> std::io::Result<WsConnection> {
        let remote = stream.peer_addr().ok();
        let n = IDS.fetch_add(1, Ordering::Relaxed);
        let rnd: String = random_bytes(6).iter().map(|b| format!("{:02x}", b)).collect();
        Ok(WsConnection {
            id: format!("{}-{}", rnd, n),
            reader: Mutex::new(reader),
            writer: Mutex::new(stream),
            client,
            closed: AtomicBool::new(false),
            close_sent: AtomicBool::new(false),
            remote,
            path: path.to_string(),
            max_message_size: 64 * 1024 * 1024,
            max_frame_size: 65536,
        })
    }

    /// Open a client connection: `ws://host:port/path`.
    pub fn connect(host: &str, port: u16, path: &str, timeout: Duration) -> Result<WsConnection> {
        Self::connect_with(host, port, path, timeout, &[])
    }

    /// Open a client connection sending extra handshake headers.
    pub fn connect_with(host: &str, port: u16, path: &str, timeout: Duration, headers: &[(String, String)]) -> Result<WsConnection> {
        Self::connect_secure(host, port, path, timeout, headers, None)
    }

    /// Open a client connection, over TLS (`wss://`) when `tls` is given.
    pub fn connect_secure(host: &str, port: u16, path: &str, timeout: Duration, headers: &[(String, String)], tls: Option<&EndpointTls>) -> Result<WsConnection> {
        let what = format!("{}://{}:{}{}", if tls.is_some() { "wss" } else { "ws" }, host, port, path);
        let stream = connect_stream(host, port, timeout, tls)?;
        let _ = stream.set_nodelay(true);
        let t = if timeout.is_zero() { None } else { Some(timeout) };
        let _ = stream.set_read_timeout(t);
        let key = base64::encode(&random_bytes(16), false);
        let path = if path.is_empty() { "/" } else { path };
        let default_port = if tls.is_some() { 443 } else { 80 };
        let host_header = if port == default_port { host.to_string() } else { format!("{}:{}", host, port) };
        let mut req = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {}\r\nSec-WebSocket-Version: 13\r\n",
            path, host_header, key
        );
        for (k, v) in headers {
            req.push_str(&format!("{}: {}\r\n", k, v));
        }
        req.push_str("\r\n");
        let mut w = stream.try_clone().map_err(|e| io_error(e, &what))?;
        w.write_all(req.as_bytes()).map_err(|e| io_error(e, &what))?;
        let mut reader = BufReader::new(stream.try_clone().map_err(|e| io_error(e, &what))?);
        let status = read_line(&mut reader, 8192).map_err(|e| io_error(e, &what))?.unwrap_or_default();
        let mut accept = None;
        loop {
            let l = read_line(&mut reader, 8192).map_err(|e| io_error(e, &what))?.unwrap_or_default();
            if l.is_empty() {
                break;
            }
            if let Some((k, v)) = l.split_once(':') {
                if k.trim().eq_ignore_ascii_case("Sec-WebSocket-Accept") {
                    accept = Some(v.trim().to_string());
                }
            }
        }
        if status.split_whitespace().nth(1) != Some("101") {
            return Err(CamelError::new(kinds::IO, format!("WebSocket handshake with {} failed: {}", what, status)));
        }
        if accept.as_deref() != Some(accept_key(&key).as_str()) {
            return Err(CamelError::new(kinds::IO, format!("WebSocket handshake with {} failed: invalid Sec-WebSocket-Accept", what)));
        }
        let _ = stream.set_read_timeout(None);
        WsConnection::new(stream, reader, true, path).map_err(|e| io_error(e, &what))
    }

    /// Limit the size of received messages (default 64 MiB).
    pub fn set_max_message_size(&mut self, n: usize) {
        self.max_message_size = n.max(1);
    }

    /// Outgoing messages larger than this are fragmented (default 64 KiB; 0 = never).
    pub fn set_max_frame_size(&mut self, n: usize) {
        self.max_frame_size = n;
    }

    /// Unique connection key.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Whether the connection uses TLS (`wss`).
    pub fn is_secure(&self) -> bool {
        self.writer.lock().map(|w| w.is_tls()).unwrap_or(false)
    }

    /// The request path of the connection.
    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn remote_addr(&self) -> Option<SocketAddr> {
        self.remote
    }

    pub fn is_open(&self) -> bool {
        !self.closed.load(Ordering::SeqCst)
    }

    fn mask(&self) -> Option<[u8; 4]> {
        if self.client {
            let b = random_bytes(4);
            Some([b[0], b[1], b[2], b[3]])
        } else {
            None
        }
    }

    fn write_frames(&self, frames: &[(bool, u8, &[u8])]) -> std::io::Result<()> {
        let mut w = self.writer.lock().unwrap();
        for (fin, op, payload) in frames {
            w.write_all(&encode_frame(*fin, *op, payload, self.mask()))?;
        }
        w.flush()
    }

    fn send_data(&self, op: u8, data: &[u8], frag: usize) -> Result<()> {
        if !self.is_open() {
            return Err(CamelError::io(format!("WebSocket connection {} is closed", self.id)));
        }
        let what = format!("websocket {}", self.id);
        if frag == 0 || data.len() <= frag {
            return self.write_frames(&[(true, op, data)]).map_err(|e| io_error(e, &what));
        }
        let chunks: Vec<&[u8]> = data.chunks(frag).collect();
        let n = chunks.len();
        let frames: Vec<(bool, u8, &[u8])> =
            chunks.into_iter().enumerate().map(|(i, c)| (i + 1 == n, if i == 0 { op } else { opcode::CONTINUATION }, c)).collect();
        self.write_frames(&frames).map_err(|e| io_error(e, &what))
    }

    /// Send a text message (fragmented above the max frame size).
    pub fn send_text(&self, text: &str) -> Result<()> {
        self.send_data(opcode::TEXT, text.as_bytes(), self.max_frame_size)
    }

    /// Send a binary message.
    pub fn send_binary(&self, data: &[u8]) -> Result<()> {
        self.send_data(opcode::BINARY, data, self.max_frame_size)
    }

    /// Send a message.
    pub fn send(&self, msg: &WsMessage) -> Result<()> {
        match msg {
            WsMessage::Text(t) => self.send_text(t),
            WsMessage::Binary(b) => self.send_binary(b),
        }
    }

    /// Send a message split into frames of at most `fragment_size` bytes.
    pub fn send_fragmented(&self, msg: &WsMessage, fragment_size: usize) -> Result<()> {
        match msg {
            WsMessage::Text(t) => self.send_data(opcode::TEXT, t.as_bytes(), fragment_size.max(1)),
            WsMessage::Binary(b) => self.send_data(opcode::BINARY, b, fragment_size.max(1)),
        }
    }

    /// Send a ping (the peer answers with a pong, handled by the reader).
    pub fn ping(&self, payload: &[u8]) -> Result<()> {
        self.write_frames(&[(true, opcode::PING, &payload[..payload.len().min(125)])]).map_err(|e| io_error(e, "websocket ping"))
    }

    /// Start the closing handshake (code 1000 = normal closure).
    pub fn close(&self, code: u16, reason: &str) {
        if self.close_sent.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut payload = code.to_be_bytes().to_vec();
        payload.extend_from_slice(&reason.as_bytes()[..reason.len().min(123)]);
        let _ = self.write_frames(&[(true, opcode::CLOSE, &payload)]);
        self.closed.store(true, Ordering::SeqCst);
    }

    /// Close the TCP connection immediately.
    pub fn shutdown(&self) {
        self.closed.store(true, Ordering::SeqCst);
        if let Ok(w) = self.writer.lock() {
            let _ = w.shutdown(std::net::Shutdown::Both);
        }
    }

    fn fail(&self, code: u16, reason: &str) -> std::io::Result<Option<WsMessage>> {
        self.close(code, reason);
        self.shutdown();
        Ok(None)
    }

    /// Read the next data message, answering pings and handling the closing handshake.
    /// Returns `Ok(None)` when the connection was closed.
    pub fn read_message(&self) -> std::io::Result<Option<WsMessage>> {
        let mut r = self.reader.lock().unwrap();
        let mut buf: Vec<u8> = Vec::new();
        let mut msg_op: Option<u8> = None;
        loop {
            let f = match read_frame(&mut *r, self.max_message_size) {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::OutOfMemory => return self.fail(1009, "message too big"),
                Err(e) if e.kind() == std::io::ErrorKind::InvalidData => return self.fail(1002, "protocol error"),
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    self.closed.store(true, Ordering::SeqCst);
                    return Ok(None);
                }
                Err(e) => {
                    self.closed.store(true, Ordering::SeqCst);
                    return Err(e);
                }
            };
            // clients must mask, servers must not
            if f.masked == self.client {
                return self.fail(1002, if self.client { "masked server frame" } else { "unmasked client frame" });
            }
            match f.opcode {
                opcode::PING => {
                    let _ = self.write_frames(&[(true, opcode::PONG, &f.payload)]);
                }
                opcode::PONG => {}
                opcode::CLOSE => {
                    let code = if f.payload.len() >= 2 { u16::from_be_bytes([f.payload[0], f.payload[1]]) } else { 1000 };
                    self.close(code, "");
                    self.closed.store(true, Ordering::SeqCst);
                    if !self.client {
                        // the server closes the TCP connection first
                        drop(r);
                        self.shutdown();
                    }
                    return Ok(None);
                }
                opcode::TEXT | opcode::BINARY => {
                    if msg_op.is_some() {
                        return self.fail(1002, "expected continuation frame");
                    }
                    msg_op = Some(f.opcode);
                    buf = f.payload;
                }
                opcode::CONTINUATION => {
                    if msg_op.is_none() {
                        return self.fail(1002, "unexpected continuation frame");
                    }
                    if buf.len() + f.payload.len() > self.max_message_size {
                        return self.fail(1009, "message too big");
                    }
                    buf.extend_from_slice(&f.payload);
                }
                _ => return self.fail(1002, "unknown opcode"),
            }
            if f.fin && f.opcode <= opcode::BINARY {
                let data = std::mem::take(&mut buf);
                return match msg_op.take() {
                    Some(opcode::TEXT) => match String::from_utf8(data) {
                        Ok(s) => Ok(Some(WsMessage::Text(s))),
                        Err(_) => self.fail(1007, "invalid UTF-8"),
                    },
                    _ => Ok(Some(WsMessage::Binary(data))),
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc6455_handshake_example() {
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn frames_round_trip() {
        // RFC 6455 5.7 examples
        assert_eq!(encode_frame(true, opcode::TEXT, b"Hello", None), vec![0x81, 0x05, 0x48, 0x65, 0x6c, 0x6c, 0x6f]);
        let masked = encode_frame(true, opcode::TEXT, b"Hello", Some([0x37, 0xfa, 0x21, 0x3d]));
        assert_eq!(masked, vec![0x81, 0x85, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d, 0x51, 0x58]);
        let f = read_frame(&mut &masked[..], 1000).unwrap();
        assert_eq!((f.fin, f.opcode, f.masked, f.payload.as_slice()), (true, opcode::TEXT, true, &b"Hello"[..]));
        let big = vec![7u8; 70_000];
        let enc = encode_frame(false, opcode::BINARY, &big, None);
        assert_eq!(&enc[..2], &[0x02, 127]);
        assert_eq!(read_frame(&mut &enc[..], 100_000).unwrap().payload.len(), 70_000);
        let mid = encode_frame(true, opcode::BINARY, &[1u8; 256], None);
        assert_eq!(&mid[..4], &[0x82, 126, 0x01, 0x00]);
        assert!(read_frame(&mut &enc[..], 1000).is_err());
    }
}

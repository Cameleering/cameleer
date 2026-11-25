//! `lumberjack:` component (Camel `camel-lumberjack`, since Camel 2.18): receive log events
//! with the Lumberjack protocol, as Beats (Filebeat, Winlogbeat...), Logstash and
//! logstash-forwarder send them.
//!
//! `lumberjack:host[:port]` (default port 5044, the Beats default), consumer only:
//!
//! ```yaml
//! - from:
//!     uri: lumberjack:0.0.0.0:5044
//!     steps:
//!       - setBody:
//!           simple: ${body[message]}
//!       - to: stream:out
//! ```
//!
//! Protocol, as Camel's `LumberjackFrameDecoder` reads it (versions `1` and `2`, all integers
//! 32-bit big-endian):
//!
//! | frame | layout |
//! |---|---|
//! | window | `version 'W' size` |
//! | JSON (v2) | `version 'J' seq length json` |
//! | data (v1) | `version 'D' seq pairs (keyLength key valueLength value)*` |
//! | compressed | `version 'C' length zlib(frames)` |
//! | ACK (sent) | `version 'A' windowSize` |
//!
//! Each event (`J` or `D` frame) is one exchange whose body is the event: a map of the JSON
//! object (`${body[message]}`, `${body[host][name]}`), or a map of strings for a data frame. No
//! headers are set. The events of a window go through the route in order; when every one of
//! them succeeded the server answers with one ACK carrying the window size and the window's
//! version byte. When an exchange fails, no ACK is sent and the connection is closed, so the
//! shipper sends the window again (at-least-once, as in Camel).
//!
//! TLS: `sslContextParameters` (endpoint, `camel.component.lumberjack.sslContextParameters`, or
//! the global ones with `useGlobalSslContextParameters=true`) with key material turns the server
//! into a TLS server; client certificates are requested when the parameters ask for them.
//! Without parameters the server listens on plain TCP.

use std::any::Any;
use std::io::{BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};

use cameleer_core::component::support::process_consumer_exchange;
use cameleer_core::context::{CamelContext, WeakContext};
use cameleer_core::dataformat::compress::zlib_decompress;
use cameleer_core::error::{kinds, CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use cameleer_core::util::Parameters;
use cameleer_core::value::{ObjectMap, Value};
use cameleer_tls::camel_support::{take_endpoint_ssl, EndpointTls};
use cameleer_tls::net::{split_host_port, take_ignored, NetStream, TcpServer, COMMON_IGNORED};

/// The default port (Beats' `output.logstash` default).
pub const DEFAULT_PORT: u16 = 5044;

/// Protocol version 1 (logstash-forwarder).
pub const VERSION_V1: u8 = b'1';
/// Protocol version 2 (Beats).
pub const VERSION_V2: u8 = b'2';
pub const TYPE_WINDOW: u8 = b'W';
pub const TYPE_COMPRESS: u8 = b'C';
pub const TYPE_JSON: u8 = b'J';
pub const TYPE_DATA: u8 = b'D';
pub const TYPE_ACKNOWLEDGE: u8 = b'A';

/// The largest frame payload accepted (a JSON event, a data value or a compressed block).
pub const MAX_PAYLOAD: usize = 64 * 1024 * 1024;

/// The `lumberjack` component.
#[derive(Default)]
pub struct LumberjackComponent;

impl Component for LumberjackComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let rest = remaining.trim_start_matches("//").trim_end_matches('/');
        if rest.is_empty() {
            return Err(CamelError::illegal_argument("lumberjack: a host is required (lumberjack:host[:port])"));
        }
        let (host, port) = split_host_port(rest, DEFAULT_PORT)?;
        let tls = take_endpoint_ssl(ctx, params, "lumberjack")?.map(EndpointTls::new);
        take_ignored(params, COMMON_IGNORED);
        params.take("autowiredEnabled");
        Ok(Arc::new(LumberjackEndpoint { uri: uri.to_string(), ctx: ctx.downgrade(), host, port, tls, bound: Arc::new(Mutex::new(None)) }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A `lumberjack` endpoint.
pub struct LumberjackEndpoint {
    uri: String,
    ctx: WeakContext,
    host: String,
    port: u16,
    tls: Option<EndpointTls>,
    bound: Arc<Mutex<Option<u16>>>,
}

impl LumberjackEndpoint {
    /// The bound port while the consumer runs (useful with port 0).
    pub fn local_port(&self) -> Option<u16> {
        *self.bound.lock().unwrap()
    }
}

impl Endpoint for LumberjackEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Err(CamelError::new(kinds::UNSUPPORTED_OPERATION, "The Lumberjack component cannot be used as a producer"))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        Ok(Arc::new(LumberjackConsumer {
            ctx: self.ctx.clone(),
            host: self.host.clone(),
            port: self.port,
            tls: self.tls.clone(),
            processor,
            logger: self.uri.clone(),
            bound: self.bound.clone(),
            server: Mutex::new(None),
        }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct LumberjackConsumer {
    ctx: WeakContext,
    host: String,
    port: u16,
    tls: Option<EndpointTls>,
    processor: Arc<dyn Processor>,
    logger: String,
    bound: Arc<Mutex<Option<u16>>>,
    server: Mutex<Option<TcpServer>>,
}

impl Service for LumberjackConsumer {
    fn start(&self) -> Result<()> {
        let mut g = self.server.lock().unwrap();
        if g.is_some() {
            return Ok(());
        }
        if let Some(t) = &self.tls {
            // fail at start, not on the first connection, when there is no key material
            t.server_config("lumberjack consumer")?;
        }
        let conn = Arc::new(Connection { ctx: self.ctx.clone(), tls: self.tls.clone(), processor: self.processor.clone(), logger: self.logger.clone() });
        let s = TcpServer::bind(&self.host, self.port, "lumberjack", move |tcp| conn.serve(tcp))?;
        *self.bound.lock().unwrap() = Some(s.port());
        *g = Some(s);
        Ok(())
    }
    fn stop(&self) -> Result<()> {
        if let Some(s) = self.server.lock().unwrap().take() {
            s.stop();
        }
        *self.bound.lock().unwrap() = None;
        Ok(())
    }
}

struct Connection {
    ctx: WeakContext,
    tls: Option<EndpointTls>,
    processor: Arc<dyn Processor>,
    logger: String,
}

/// An open window: its version, its announced size and the events read so far.
struct Window {
    version: u8,
    size: u32,
    events: Vec<Value>,
}

/// Frame decoding state of one connection.
#[derive(Default)]
pub struct Decoder {
    window: Option<Window>,
}

/// A decoded, complete window.
pub struct DecodedWindow {
    pub version: u8,
    pub size: u32,
    pub events: Vec<Value>,
    /// False for a window replaced by a new `W` frame before all its events arrived.
    pub complete: bool,
}

fn proto(m: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, m.into())
}

fn read_u32(r: &mut dyn Read) -> std::io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_be_bytes(b))
}

fn read_bytes(r: &mut dyn Read, len: u32) -> std::io::Result<Vec<u8>> {
    let len = len as usize;
    if len > MAX_PAYLOAD {
        return Err(proto(format!("Lumberjack frame payload of {} bytes exceeds the limit of {} bytes", len, MAX_PAYLOAD)));
    }
    let mut out = Vec::with_capacity(len.min(1 << 20));
    r.take(len as u64).read_to_end(&mut out)?;
    if out.len() != len {
        return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "truncated Lumberjack frame"));
    }
    Ok(out)
}

fn read_string(r: &mut dyn Read) -> std::io::Result<String> {
    let len = read_u32(r)?;
    let b = read_bytes(r, len)?;
    String::from_utf8(b).map_err(|_| proto("Lumberjack data frame with invalid UTF-8"))
}

impl Decoder {
    /// A new decoder.
    pub fn new() -> Decoder {
        Decoder::default()
    }

    /// Reads one frame (recursing into compressed frames) and passes every window that it
    /// completes to `on_window`. `Ok(false)` at a clean end of stream before a frame.
    pub fn read_frame(&mut self, r: &mut dyn Read, on_window: &mut dyn FnMut(DecodedWindow) -> std::io::Result<()>) -> std::io::Result<bool> {
        let mut head = [0u8; 2];
        match r.read(&mut head[..1])? {
            0 => return Ok(false),
            _ => r.read_exact(&mut head[1..])?,
        }
        let (version, kind) = (head[0], head[1]);
        if version != VERSION_V1 && version != VERSION_V2 {
            return Err(proto(format!("Unsupported frame version={}", version as char)));
        }
        if let Some(w) = &self.window {
            if w.version != version {
                return Err(proto(format!("Protocol version changed during session from {} to {}", w.version as char, version as char)));
            }
        }
        match kind {
            TYPE_WINDOW => {
                let size = read_u32(r)?;
                if let Some(old) = self.window.take() {
                    cameleer_core::camel_warn!("lumberjack", "New window size received but the previous window was not complete, sending {} of {} events", old.events.len(), old.size);
                    on_window(DecodedWindow { version: old.version, size: old.size, events: old.events, complete: false })?;
                }
                self.window = Some(Window { version, size, events: Vec::with_capacity((size as usize).min(1024)) });
            }
            TYPE_JSON => {
                let _seq = read_u32(r)?;
                let len = read_u32(r)?;
                let payload = read_bytes(r, len)?;
                let text = std::str::from_utf8(&payload).map_err(|_| proto("Lumberjack JSON frame with invalid UTF-8"))?;
                let v = cameleer_core::json::parse(text).map_err(|e| proto(format!("Invalid JSON in Lumberjack frame: {}", e.message)))?;
                self.add(v, on_window)?;
            }
            TYPE_DATA => {
                let _seq = read_u32(r)?;
                let pairs = read_u32(r)?;
                let mut m = ObjectMap::new();
                for _ in 0..pairs {
                    let k = read_string(r)?;
                    let v = read_string(r)?;
                    m.insert(k, Value::String(v));
                }
                self.add(Value::Object(m), on_window)?;
            }
            TYPE_COMPRESS => {
                let len = read_u32(r)?;
                let packed = read_bytes(r, len)?;
                let plain = zlib_decompress(&packed).map_err(|e| proto(format!("Invalid compressed Lumberjack frame: {}", e.message)))?;
                let mut inner = std::io::Cursor::new(plain);
                while self.read_frame(&mut inner, on_window)? {}
            }
            other => return Err(proto(format!("Unsupported frame type={}", other as char))),
        }
        Ok(true)
    }

    fn add(&mut self, event: Value, on_window: &mut dyn FnMut(DecodedWindow) -> std::io::Result<()>) -> std::io::Result<()> {
        let w = self.window.as_mut().ok_or_else(|| proto("Lumberjack event received before a window frame"))?;
        w.events.push(event);
        if w.events.len() as u64 == w.size as u64 {
            let w = self.window.take().unwrap();
            on_window(DecodedWindow { version: w.version, size: w.size, events: w.events, complete: true })?;
        }
        Ok(())
    }
}

/// The ACK frame for a window: `version 'A' size`.
pub fn ack_frame(version: u8, size: u32) -> [u8; 6] {
    let s = size.to_be_bytes();
    [version, TYPE_ACKNOWLEDGE, s[0], s[1], s[2], s[3]]
}

impl Connection {
    fn serve(&self, tcp: TcpStream) {
        let peer = tcp.peer_addr().map(|a| a.to_string()).unwrap_or_default();
        let stream = match &self.tls {
            Some(t) => match t.accept(tcp, "lumberjack consumer") {
                Ok(s) => s,
                Err(e) => {
                    cameleer_core::camel_debug!(&self.logger, "lumberjack TLS handshake with {} failed: {}", peer, e);
                    return;
                }
            },
            None => NetStream::plain(tcp),
        };
        let mut reader = BufReader::new(&stream);
        let mut decoder = Decoder::new();
        let mut on_window = |w: DecodedWindow| -> std::io::Result<()> {
            for event in w.events {
                let mut ex = Exchange::new(self.ctx.upgrade());
                ex.message.body = event;
                if !process_consumer_exchange(&self.logger, self.processor.as_ref(), &mut ex) {
                    // no ACK: the shipper sends the window again on a new connection
                    return Err(std::io::Error::other("exchange failed"));
                }
            }
            if w.complete {
                (&stream).write_all(&ack_frame(w.version, w.size))?;
                (&stream).flush()?;
            }
            Ok(())
        };
        loop {
            match decoder.read_frame(&mut reader, &mut on_window) {
                Ok(true) => {}
                Ok(false) => break,
                Err(e) => {
                    match e.kind() {
                        std::io::ErrorKind::InvalidData => cameleer_core::camel_warn!(&self.logger, "Caught an exception while reading from {}, closing channel: {}", peer, e),
                        _ => cameleer_core::camel_debug!(&self.logger, "lumberjack connection from {} closed: {}", peer, e),
                    }
                    break;
                }
            }
        }
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lumberjack_client::{compressed_frame, data_frame, json_frame, window_frame};

    fn decode(bytes: &[u8]) -> std::io::Result<Vec<DecodedWindow>> {
        let mut d = Decoder::new();
        let mut out = Vec::new();
        let mut r = std::io::Cursor::new(bytes.to_vec());
        while d.read_frame(&mut r, &mut |w| {
            out.push(w);
            Ok(())
        })? {}
        Ok(out)
    }

    #[test]
    fn json_data_and_compressed_frames() {
        let mut b = window_frame(VERSION_V2, 3);
        b.extend(json_frame(VERSION_V2, 1, "{\"message\":\"hello\",\"host\":{\"name\":\"web\"}}"));
        let mut inner = json_frame(VERSION_V2, 2, "[1,2]");
        inner.extend(json_frame(VERSION_V2, 3, "\"text\""));
        b.extend(compressed_frame(VERSION_V2, &inner));
        let w = decode(&b).unwrap();
        assert_eq!(w.len(), 1);
        assert!(w[0].complete);
        assert_eq!(w[0].size, 3);
        assert_eq!(w[0].events[0].get("host").and_then(|h| h.get("name")), Some(&Value::from("web")));
        assert_eq!(w[0].events[1], Value::Array(vec![Value::Int(1), Value::Int(2)]));
        assert_eq!(w[0].events[2], Value::from("text"));

        let mut b = window_frame(VERSION_V1, 1);
        b.extend(data_frame(VERSION_V1, 1, &[("line", "a log line"), ("file", "/var/log/x")]));
        let w = decode(&b).unwrap();
        assert_eq!(w[0].version, VERSION_V1);
        assert_eq!(w[0].events[0].get("file"), Some(&Value::from("/var/log/x")));
        assert_eq!(ack_frame(VERSION_V1, 1), [b'1', b'A', 0, 0, 0, 1]);
    }

    #[test]
    fn protocol_errors() {
        let err = |b: &[u8]| decode(b).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(err(b"3W\0\0\0\x01").contains("Unsupported frame version=3"));
        assert!(err(b"2X").contains("Unsupported frame type=X"));
        assert!(err(&json_frame(VERSION_V2, 1, "{}")).contains("before a window"));
        let mut b = window_frame(VERSION_V2, 2);
        b.extend(json_frame(VERSION_V1, 1, "{}"));
        assert!(err(&b).contains("Protocol version changed"));
        let mut b = window_frame(VERSION_V2, 1);
        b.extend(b"2J\0\0\0\x01\xff\xff\xff\xff");
        assert!(err(&b).contains("exceeds the limit"));
        // an incomplete window replaced by a new one is passed on as incomplete
        let mut b = window_frame(VERSION_V2, 2);
        b.extend(json_frame(VERSION_V2, 1, "{}"));
        b.extend(window_frame(VERSION_V2, 1));
        b.extend(json_frame(VERSION_V2, 1, "{}"));
        let w = decode(&b).unwrap();
        assert_eq!((w.len(), w[0].complete, w[1].complete), (2, false, true));
    }
}

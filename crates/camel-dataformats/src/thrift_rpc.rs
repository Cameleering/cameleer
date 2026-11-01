//! `thrift:` component (Camel `camel-thrift`): Thrift RPC clients and servers driven by a
//! runtime IDL (see [`crate::thrift`]) instead of generated service classes.
//!
//! `thrift://host:port/[package.]Service?method=name&idlFile=calc.thrift`
//!
//! - **Producer**: calls `method` (or header `CamelThriftMethodName`). The body holds the
//!   arguments: an array (positional), an object keyed by argument name, a single value for a
//!   one-argument method or null for none. The reply's return value becomes the body (null for
//!   `void`/`oneway`). Declared exceptions fail the exchange with the exception struct name
//!   as kind (message = its `message`/`why`/first string field; the struct is in header
//!   `CamelThriftException`); `TApplicationException`s fail with kind `TApplicationException`.
//! - **Consumer**: a Thrift server. Each call is routed with the arguments as an array body
//!   and header `CamelThriftMethodName`; the resulting body is the return value. An exchange
//!   failing with the name of a declared exception (or a body that is an object holding
//!   `@exception` = name) returns that exception, any other failure a
//!   `TApplicationException` (`INTERNAL_ERROR`); unknown methods `UNKNOWN_METHOD`.
//!
//! Options: `idlFile` or `schema` (inline IDL; required), `method`, `exchangeProtocol`
//! (`BINARY` default, `COMPACT`, `JSON` = TJSONProtocol), `transport` (`FRAMED` default or
//! `BUFFERED` (plain socket); consumers accept both on the same port), `negotiationType`
//! (`PLAINTEXT`, `SSL` with `sslContextParameters`), `clientTimeout` (ms, 0 = none),
//! `synchronous` (accepted), `compressionType` (`NONE` or `ZLIB`: TZlibTransport, a zlib
//! stream sync-flushed after every message; both sides must use it).

use std::any::Any;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{CamelError, Result};
use camel_core::message::{Exchange, ExchangePattern};
use camel_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::value::{ObjectMap, Value};
use camel_tls::camel_support::{endpoint_tls, EndpointTls};
use camel_tls::net::NetStream;

use crate::thrift::{self, message_type, Idl, MessageHeader, Protocol, TMethod};

/// Header with the method name.
pub const METHOD_NAME_HEADER: &str = "CamelThriftMethodName";
/// Header with a declared exception struct received by the producer.
pub const EXCEPTION_HEADER: &str = "CamelThriftException";

/// `TApplicationException` types.
pub mod app_exception {
    pub const UNKNOWN: i32 = 0;
    pub const UNKNOWN_METHOD: i32 = 1;
    pub const INVALID_MESSAGE_TYPE: i32 = 2;
    pub const WRONG_METHOD_NAME: i32 = 3;
    pub const BAD_SEQUENCE_ID: i32 = 4;
    pub const MISSING_RESULT: i32 = 5;
    pub const INTERNAL_ERROR: i32 = 6;
    pub const PROTOCOL_ERROR: i32 = 7;
}

fn terr(kind: &str, msg: impl Into<String>) -> CamelError {
    CamelError::new(kind, msg)
}

/// Wire protocol of an endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireProtocol {
    Binary,
    Compact,
    Json,
}

/// Endpoint configuration.
#[derive(Clone)]
pub struct ThriftConfig {
    pub host: String,
    pub port: u16,
    pub service: String,
    pub method: Option<String>,
    pub idl: Arc<Idl>,
    pub protocol: WireProtocol,
    pub framed: bool,
    /// `compressionType=ZLIB`.
    pub zlib: bool,
    pub tls: Option<EndpointTls>,
    pub client_timeout: Duration,
}

/// The `thrift` component.
#[derive(Default)]
pub struct ThriftComponent;

impl Component for ThriftComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let (addr, service) = remaining.split_once('/').unwrap_or((remaining, ""));
        let (host, port) = match addr.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.parse::<u16>().map_err(|_| CamelError::illegal_argument(format!("Invalid port in {}", uri)))?),
            None => return Err(CamelError::illegal_argument(format!("thrift endpoint needs host:port: {}", uri))),
        };
        let service = service.trim_matches('/');
        let service = service.rsplit('.').next().unwrap_or(service);
        let service = service.trim_end_matches("$Iface").trim_end_matches("$Client").to_string();
        let src = match (params.take("schema").or_else(|| params.take("idl")), params.take("idlFile")) {
            (Some(s), _) => s,
            (None, Some(f)) => String::from_utf8(crate::read_resource(&f)?).map_err(|_| CamelError::illegal_argument("IDL file is not UTF-8"))?,
            _ => return Err(CamelError::illegal_argument("The thrift component requires an IDL (idlFile or schema)")),
        };
        let idl = Idl::parse(&src)?;
        let service = if service.is_empty() && idl.services.len() == 1 { idl.services.keys().next().unwrap().clone() } else { service };
        if !idl.services.contains_key(&service) {
            return Err(CamelError::illegal_argument(format!("Unknown thrift service {} in {}", service, uri)));
        }
        let protocol = match params.take_string("exchangeProtocol", "BINARY").to_ascii_uppercase().as_str() {
            "BINARY" => WireProtocol::Binary,
            "COMPACT" => WireProtocol::Compact,
            "JSON" => WireProtocol::Json,
            other => return Err(CamelError::illegal_argument(format!("Unsupported exchangeProtocol: {} (BINARY, COMPACT, JSON)", other))),
        };
        let framed = match params.take_string("transport", "FRAMED").to_ascii_uppercase().as_str() {
            "FRAMED" => true,
            "BUFFERED" | "SOCKET" | "PLAIN" => false,
            other => return Err(CamelError::illegal_argument(format!("Unsupported transport: {}", other))),
        };
        let ssl = match params.take_string("negotiationType", "PLAINTEXT").to_ascii_uppercase().as_str() {
            "PLAINTEXT" => false,
            "SSL" | "TLS" => true,
            other => return Err(CamelError::illegal_argument(format!("Unsupported negotiationType: {}", other))),
        };
        let zlib = match params.take_string("compressionType", "NONE").to_ascii_uppercase().as_str() {
            "NONE" => false,
            "ZLIB" => true,
            other => return Err(CamelError::illegal_argument(format!("Unsupported compressionType: {} (NONE or ZLIB)", other))),
        };
        let tls = endpoint_tls(ctx, params, "thrift", ssl, &[])?;
        if ssl && tls.is_none() {
            return Err(CamelError::illegal_argument("negotiationType=SSL requires sslContextParameters"));
        }
        let method = params.take("method");
        if let Some(m) = &method {
            if idl.service_method(&service, m).is_none() {
                return Err(CamelError::illegal_argument(format!("Unknown method {} of thrift service {}", m, service)));
            }
        }
        let cfg = ThriftConfig {
            host,
            port,
            service,
            method,
            idl: Arc::new(idl),
            protocol,
            framed,
            zlib,
            tls,
            client_timeout: Duration::from_millis(params.take_u64("clientTimeout", 0)?),
        };
        for k in ["synchronous", "bridgeErrorHandler", "lazyStartProducer", "exceptionHandler", "exchangePattern", "maxPoolSize", "poolSize"] {
            params.take(k);
        }
        Ok(Arc::new(ThriftEndpoint { uri: uri.to_string(), ctx: ctx.downgrade(), cfg, port: Arc::new(Mutex::new(None)) }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A `thrift` endpoint.
pub struct ThriftEndpoint {
    uri: String,
    ctx: WeakContext,
    cfg: ThriftConfig,
    port: Arc<Mutex<Option<u16>>>,
}

impl ThriftEndpoint {
    /// The bound port while a consumer runs.
    pub fn local_port(&self) -> Option<u16> {
        *self.port.lock().unwrap()
    }
}

impl Endpoint for ThriftEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(ThriftProducer { cfg: self.cfg.clone(), conn: Mutex::new(None), seq: AtomicI32::new(0) }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        Ok(Arc::new(ThriftConsumer {
            cfg: self.cfg.clone(),
            ctx: self.ctx.clone(),
            processor,
            port: self.port.clone(),
            running: Arc::new(AtomicBool::new(false)),
            addr: Mutex::new(None),
        }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// --------------------------------------------------------------------------- codec

/// Encode a message (without transport framing).
pub fn encode_message(idl: &Idl, proto: WireProtocol, h: &MessageHeader, struct_name: &str, v: &Value) -> Result<Vec<u8>> {
    match proto {
        WireProtocol::Json => Ok(thrift::tjson_message(idl, h, struct_name, v)?.into_bytes()),
        WireProtocol::Binary | WireProtocol::Compact => {
            let p = if proto == WireProtocol::Binary { Protocol::Binary } else { Protocol::Compact };
            let mut out = Vec::new();
            thrift::write_message_header(&mut out, p, h);
            out.extend(thrift::serialize(idl, struct_name, v, p)?);
            Ok(out)
        }
    }
}

/// A received message: header plus the undecoded body.
enum Body {
    Bin(Vec<u8>, Protocol),
    Json(Value),
}

impl Body {
    fn decode(&self, idl: &Idl, struct_name: &str) -> Result<Value> {
        match self {
            Body::Bin(b, p) => Ok(thrift::deserialize_prefix(idl, struct_name, b, *p)?.0),
            Body::Json(v) => thrift::from_tjson_struct_value(idl, struct_name, v),
        }
    }
}

/// Complete message length at the start of `buf` for unframed transports (`None` = need more).
fn unframed_len(idl: &Idl, proto: WireProtocol, buf: &[u8], skip_struct: &dyn Fn(&MessageHeader) -> Option<String>) -> Result<Option<usize>> {
    match proto {
        WireProtocol::Json => Ok(thrift::json_value_len(buf)),
        _ => {
            let p = if proto == WireProtocol::Binary { Protocol::Binary } else { Protocol::Compact };
            let (h, n) = match thrift::read_message_header(buf, p) {
                Ok(x) => x,
                Err(e) if e.message.contains("Unexpected end") => return Ok(None),
                Err(e) => return Err(e),
            };
            // decode generically (unknown struct) to find the end
            let name = skip_struct(&h);
            let r = match name {
                Some(sn) if idl.structs.contains_key(&sn) => thrift::deserialize_prefix(idl, &sn, &buf[n..], p).map(|(_, used)| used),
                _ => generic_struct_len(&buf[n..], p),
            };
            match r {
                Ok(used) => Ok(Some(n + used)),
                Err(e) if e.message.contains("Unexpected end") => Ok(None),
                Err(e) => Err(e),
            }
        }
    }
}

fn generic_struct_len(buf: &[u8], p: Protocol) -> Result<usize> {
    let idl = Idl { structs: [("__any".to_string(), Vec::new())].into_iter().collect(), ..Default::default() };
    thrift::deserialize_prefix(&idl, "__any", buf, p).map(|(_, n)| n)
}

fn parse_message(proto: WireProtocol, data: &[u8]) -> Result<(MessageHeader, Body)> {
    match proto {
        WireProtocol::Json => {
            let text = std::str::from_utf8(data).map_err(|_| terr("TProtocolException", "TJSONProtocol message is not UTF-8"))?;
            let (h, v) = thrift::parse_tjson_message(text)?;
            Ok((h, Body::Json(v)))
        }
        _ => {
            let p = if proto == WireProtocol::Binary { Protocol::Binary } else { Protocol::Compact };
            let (h, n) = thrift::read_message_header(data, p)?;
            Ok((h, Body::Bin(data[n..].to_vec(), p)))
        }
    }
}

/// Reads one message from a stream, buffering leftovers in `buf`.
struct MsgReader {
    buf: Vec<u8>,
    zin: Option<ZlibIn>,
    /// `Some(header_sent)` with zlib compression.
    zout: Option<bool>,
}

/// Inbound side of a TZlibTransport: the raw stream is re-inflated (with a terminating empty
/// final block appended) whenever it ends at a sync-flush point (`00 00 FF FF`).
#[derive(Default)]
struct ZlibIn {
    raw: Vec<u8>,
    emitted: usize,
}

impl ZlibIn {
    fn feed(&mut self, data: &[u8]) -> std::io::Result<Vec<u8>> {
        self.raw.extend_from_slice(data);
        if self.raw.len() < 6 || !self.raw.ends_with(&[0, 0, 0xff, 0xff]) {
            return Ok(Vec::new());
        }
        let mut d = self.raw[2..].to_vec();
        d.extend_from_slice(&[0x03, 0x00]);
        let (out, _) = camel_core::dataformat::compress::inflate_raw(&d).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.message))?;
        let new = out.get(self.emitted..).unwrap_or_default().to_vec();
        self.emitted = out.len();
        Ok(new)
    }
}

/// Deflates `data` as one sync-flushed zlib segment (non-final block + empty stored block).
pub fn zlib_sync_segment(data: &[u8]) -> Vec<u8> {
    use camel_core::dataformat::compress::{deflate, inflate_raw};
    let mut block = deflate(data, 6);
    block[0] &= 0xfe; // not the final block
    // the empty stored block header follows the end-of-block code: its three zero bits
    // either fit in the padding of the last byte or need one more byte
    for extra in [&[0u8, 0, 0xff, 0xff][..], &[0u8, 0, 0, 0xff, 0xff][..]] {
        let mut cand = block.clone();
        cand.extend_from_slice(extra);
        let mut probe = cand.clone();
        probe.extend_from_slice(&[0x03, 0x00]);
        if matches!(inflate_raw(&probe), Ok((ref o, _)) if o == data) {
            return cand;
        }
    }
    // fall back to stored blocks
    let mut out = Vec::new();
    for c in data.chunks(65535) {
        out.push(0);
        out.extend_from_slice(&(c.len() as u16).to_le_bytes());
        out.extend_from_slice(&(!(c.len() as u16)).to_le_bytes());
        out.extend_from_slice(c);
    }
    out.extend_from_slice(&[0, 0, 0, 0xff, 0xff]);
    out
}

impl MsgReader {
    fn new(zlib: bool) -> MsgReader {
        MsgReader { buf: Vec::new(), zin: if zlib { Some(ZlibIn::default()) } else { None }, zout: if zlib { Some(false) } else { None } }
    }

    /// Applies the outbound transport (zlib) to framed message bytes.
    fn wrap_out(&mut self, data: Vec<u8>) -> Vec<u8> {
        match self.zout.as_mut() {
            None => data,
            Some(started) => {
                let mut out = Vec::new();
                if !*started {
                    out.extend_from_slice(&[0x78, 0x9c]);
                    *started = true;
                }
                out.extend(zlib_sync_segment(&data));
                out
            }
        }
    }

    fn read_exact_buf<S: Read>(&mut self, s: &mut S, n: usize) -> std::io::Result<bool> {
        while self.buf.len() < n {
            let mut tmp = [0u8; 8192];
            let k = s.read(&mut tmp)?;
            if k == 0 {
                return Ok(false);
            }
            match self.zin.as_mut() {
                Some(z) => {
                    let out = z.feed(&tmp[..k])?;
                    self.buf.extend_from_slice(&out);
                }
                None => self.buf.extend_from_slice(&tmp[..k]),
            }
        }
        Ok(true)
    }

    /// Next message bytes. `framed`: `Some(true/false)` fixed, `None` = detect per message.
    fn next<S: Read>(&mut self, s: &mut S, framed: Option<bool>, idl: &Idl, proto: WireProtocol, struct_for: &dyn Fn(&MessageHeader) -> Option<String>) -> Result<Option<(Vec<u8>, bool)>> {
        let io = |e: std::io::Error| CamelError::io(format!("thrift: {}", e));
        if !self.read_exact_buf(s, 1).map_err(io)? {
            return Ok(None);
        }
        let framed = framed.unwrap_or_else(|| !matches!(self.buf[0], 0x80 | 0x82 | b'['));
        if framed {
            if !self.read_exact_buf(s, 4).map_err(io)? {
                return Ok(None);
            }
            let n = u32::from_be_bytes(self.buf[..4].try_into().unwrap()) as usize;
            if n > 256 * 1024 * 1024 {
                return Err(terr("TTransportException", format!("Frame size ({}) larger than max length", n)));
            }
            if !self.read_exact_buf(s, 4 + n).map_err(io)? {
                return Err(terr("TTransportException", "Connection closed inside a frame"));
            }
            let msg = self.buf[4..4 + n].to_vec();
            self.buf.drain(..4 + n);
            return Ok(Some((msg, true)));
        }
        loop {
            if let Some(n) = unframed_len(idl, proto, &self.buf, struct_for)? {
                let msg = self.buf[..n].to_vec();
                self.buf.drain(..n);
                return Ok(Some((msg, false)));
            }
            let want = self.buf.len() + 1;
            if !self.read_exact_buf(s, want).map_err(io)? {
                return Err(terr("TTransportException", "Connection closed inside a message"));
            }
        }
    }
}

fn frame(msg: Vec<u8>, framed: bool) -> Vec<u8> {
    if !framed {
        return msg;
    }
    let mut out = (msg.len() as u32).to_be_bytes().to_vec();
    out.extend(msg);
    out
}

/// Builds the `_args` struct value from a body.
pub fn args_from_body(m: &TMethod, body: &Value) -> Result<Value> {
    let mut o = ObjectMap::new();
    match body {
        Value::Null => {}
        Value::Array(a) => {
            if a.len() > m.args.len() {
                return Err(CamelError::illegal_argument(format!("Method {} takes {} arguments but {} were given", m.name, m.args.len(), a.len())));
            }
            for (f, v) in m.args.iter().zip(a) {
                o.insert(f.name.clone(), v.clone());
            }
        }
        Value::Object(obj) if m.args.len() != 1 || m.args.iter().all(|f| obj.contains_key(&f.name)) => {
            for f in &m.args {
                if let Some(v) = obj.get(&f.name) {
                    o.insert(f.name.clone(), v.clone());
                }
            }
        }
        other if m.args.len() == 1 => {
            let v = match other {
                Value::String(s) if matches!(m.args[0].ty, thrift::TType::Struct(_) | thrift::TType::List(_) | thrift::TType::Map(..) | thrift::TType::Set(_)) => camel_core::json::parse(s).unwrap_or_else(|_| other.clone()),
                _ => other.clone(),
            };
            o.insert(m.args[0].name.clone(), v);
        }
        other => return Err(CamelError::illegal_argument(format!("Cannot map a {} body to the arguments of {}", other.type_name(), m.name))),
    }
    Ok(Value::Object(o))
}

fn exception_message(v: &Value) -> String {
    let o = match v.as_object() {
        Some(o) => o,
        None => return v.to_string(),
    };
    for k in ["message", "why", "msg", "reason", "error"] {
        if let Some(Value::String(s)) = o.get(k) {
            return s.clone();
        }
    }
    o.iter().find_map(|(_, v)| v.as_str().map(String::from)).unwrap_or_else(|| v.to_json())
}

// --------------------------------------------------------------------------- producer

struct ThriftProducer {
    cfg: ThriftConfig,
    conn: Mutex<Option<(NetStream, MsgReader)>>,
    seq: AtomicI32,
}

impl Service for ThriftProducer {
    fn stop(&self) -> Result<()> {
        if let Some((s, _)) = self.conn.lock().unwrap().take() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
        Ok(())
    }
}

impl ThriftProducer {
    fn open(&self) -> Result<(NetStream, MsgReader)> {
        let host = if self.cfg.host == "0.0.0.0" { "127.0.0.1" } else { &self.cfg.host };
        let what = format!("thrift://{}:{}", host, self.cfg.port);
        let s = match &self.cfg.tls {
            Some(t) => t.connect(host, self.cfg.port, Duration::from_secs(30))?,
            None => NetStream::connect(host, self.cfg.port, None, Duration::from_secs(30)).map_err(|e| CamelError::new(camel_core::error::kinds::CONNECT, format!("{}: {}", e, what)))?,
        };
        let _ = s.set_nodelay(true);
        let t = if self.cfg.client_timeout.is_zero() { None } else { Some(self.cfg.client_timeout) };
        let _ = s.set_read_timeout(t);
        Ok((s, MsgReader::new(self.cfg.zlib)))
    }

    fn call(&self, method: &TMethod, args: &Value, thrown: &mut Option<Value>) -> Result<Value> {
        let idl = &*self.cfg.idl;
        let seqid = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let h = MessageHeader { name: method.name.clone(), mtype: if method.oneway { message_type::ONEWAY } else { message_type::CALL }, seqid };
        let (svc, _) = idl.service_method(&self.cfg.service, &method.name).unwrap();
        let args_name = format!("{}.{}_args", svc.name, method.name);
        let result_name = format!("{}.{}_result", svc.name, method.name);
        let message = frame(encode_message(idl, self.cfg.protocol, &h, &args_name, args)?, self.cfg.framed);
        let mut guard = self.conn.lock().unwrap();
        let mut attempt = 0;
        let msg = loop {
            attempt += 1;
            if guard.is_none() {
                *guard = Some(self.open()?);
            }
            let (s, r) = guard.as_mut().unwrap();
            let payload = r.wrap_out(message.clone());
            let sent = (&*s).write_all(&payload).and_then(|_| (&*s).flush());
            if sent.is_err() && attempt == 1 {
                *guard = None;
                continue;
            }
            sent.map_err(|e| CamelError::io(format!("thrift send: {}", e)))?;
            if method.oneway {
                return Ok(Value::Null);
            }
            let rn = result_name.clone();
            match r.next(&mut &*s, Some(self.cfg.framed), idl, self.cfg.protocol, &move |_| Some(rn.clone())) {
                Ok(Some((m, _))) => break m,
                Ok(None) if attempt == 1 => {
                    *guard = None;
                }
                Ok(None) => {
                    *guard = None;
                    return Err(terr("TTransportException", "Connection closed before a reply was received"));
                }
                Err(e) => {
                    *guard = None;
                    let timeout = e.message.contains("timed out") || e.message.contains("WouldBlock") || e.message.contains("temporarily unavailable");
                    return Err(if timeout { terr("TTransportException", format!("Timed out waiting for the reply to {}", method.name)) } else { e });
                }
            }
        };
        drop(guard);
        let (rh, body) = parse_message(self.cfg.protocol, &msg)?;
        if rh.seqid != seqid {
            return Err(terr("TApplicationException", format!("{} failed: out of sequence response", method.name)));
        }
        if rh.mtype == message_type::EXCEPTION {
            let e = body.decode(idl, "TApplicationException")?;
            return Err(terr("TApplicationException", e.get("message").map(|m| m.to_string()).unwrap_or_default()));
        }
        let res = body.decode(idl, &result_name)?;
        if let Some(v) = res.get("success") {
            return Ok(v.clone());
        }
        for t in &method.throws {
            if let Some(v) = res.get(&t.name) {
                let kind = match &t.ty {
                    thrift::TType::Struct(n) => n.clone(),
                    _ => "TException".into(),
                };
                *thrown = Some(v.clone());
                return Err(terr(&kind, exception_message(v)));
            }
        }
        if method.ret.is_none() {
            return Ok(Value::Null);
        }
        Err(terr("TApplicationException", format!("{} failed: unknown result", method.name)))
    }
}

impl Processor for ThriftProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let name = ex.header(METHOD_NAME_HEADER).map(|v| v.to_string()).filter(|s| !s.is_empty()).or_else(|| self.cfg.method.clone());
        let name = name.ok_or_else(|| CamelError::illegal_argument("No thrift method given (option method or header CamelThriftMethodName)"))?;
        let (_, m) = self.cfg.idl.service_method(&self.cfg.service, &name).ok_or_else(|| CamelError::illegal_argument(format!("Unknown method {} of thrift service {}", name, self.cfg.service)))?;
        let m = m.clone();
        let args = args_from_body(&m, ex.body())?;
        let mut thrown = None;
        match self.call(&m, &args, &mut thrown) {
            Ok(v) => {
                ex.set_body(v);
                Ok(())
            }
            Err(e) => {
                if let Some(v) = thrown {
                    ex.set_header(EXCEPTION_HEADER, v);
                }
                Err(e)
            }
        }
    }
}

// --------------------------------------------------------------------------- consumer

struct ThriftConsumer {
    cfg: ThriftConfig,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
    port: Arc<Mutex<Option<u16>>>,
    running: Arc<AtomicBool>,
    addr: Mutex<Option<std::net::SocketAddr>>,
}

struct Handler {
    cfg: ThriftConfig,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
}

impl Handler {
    fn serve(&self, tcp: TcpStream) {
        let _ = tcp.set_nodelay(true);
        let stream = match &self.cfg.tls {
            Some(t) => match t.accept(tcp, "thrift consumer") {
                Ok(s) => s,
                Err(_) => return,
            },
            None => NetStream::plain(tcp),
        };
        let idl = &*self.cfg.idl;
        let svc = self.cfg.service.clone();
        let struct_for = move |h: &MessageHeader| idl.service_method(&svc, &h.name).map(|(s, m)| format!("{}.{}_args", s.name, m.name));
        let mut reader = MsgReader::new(self.cfg.zlib);
        loop {
            let (msg, framed) = match reader.next(&mut &stream, None, idl, self.cfg.protocol, &struct_for) {
                Ok(Some(x)) => x,
                _ => break,
            };
            let reply = match self.handle(&msg) {
                Ok(Some(r)) => r,
                Ok(None) => continue,
                Err(_) => break,
            };
            let out = reader.wrap_out(frame(reply, framed));
            if (&stream).write_all(&out).and_then(|_| (&stream).flush()).is_err() {
                break;
            }
        }
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }

    fn app_exception(&self, h: &MessageHeader, ty: i32, msg: &str) -> Result<Option<Vec<u8>>> {
        let mut o = ObjectMap::new();
        o.insert("message".into(), Value::String(msg.to_string()));
        o.insert("type".into(), Value::Int(ty as i64));
        let rh = MessageHeader { name: h.name.clone(), mtype: message_type::EXCEPTION, seqid: h.seqid };
        Ok(Some(encode_message(&self.cfg.idl, self.cfg.protocol, &rh, "TApplicationException", &Value::Object(o))?))
    }

    fn handle(&self, msg: &[u8]) -> Result<Option<Vec<u8>>> {
        let idl = &*self.cfg.idl;
        let (h, body) = parse_message(self.cfg.protocol, msg)?;
        let oneway = h.mtype == message_type::ONEWAY;
        if h.mtype != message_type::CALL && !oneway {
            return self.app_exception(&h, app_exception::INVALID_MESSAGE_TYPE, "Invalid message type");
        }
        let Some((svc, m)) = idl.service_method(&self.cfg.service, &h.name) else {
            return if oneway { Ok(None) } else { self.app_exception(&h, app_exception::UNKNOWN_METHOD, &format!("Invalid method name: '{}'", h.name)) };
        };
        let args = match body.decode(idl, &format!("{}.{}_args", svc.name, m.name)) {
            Ok(a) => a,
            Err(e) => return if oneway { Ok(None) } else { self.app_exception(&h, app_exception::PROTOCOL_ERROR, &e.message) },
        };
        let arr: Vec<Value> = m.args.iter().map(|f| args.get(&f.name).cloned().unwrap_or(Value::Null)).collect();
        let mut ex = Exchange::new(self.ctx.upgrade());
        ex.pattern = if oneway { ExchangePattern::InOnly } else { ExchangePattern::InOut };
        ex.set_body(Value::Array(arr));
        ex.set_header(METHOD_NAME_HEADER, m.name.clone());
        if let Err(e) = self.processor.process(&mut ex) {
            ex.set_exception(e);
        }
        if oneway {
            return Ok(None);
        }
        let result_name = format!("{}.{}_result", svc.name, m.name);
        let mut res = ObjectMap::new();
        let declared = |kind: &str| m.throws.iter().find(|t| matches!(&t.ty, thrift::TType::Struct(n) if n == kind)).cloned();
        if let Some(e) = ex.exception.clone() {
            match declared(&e.kind) {
                Some(t) => {
                    let thrift::TType::Struct(sn) = &t.ty else { unreachable!() };
                    let mut o = ObjectMap::new();
                    if let Some(f) = idl.structs.get(sn).and_then(|fs| fs.iter().find(|f| f.ty == thrift::TType::String)) {
                        o.insert(f.name.clone(), Value::String(e.message.clone()));
                    }
                    res.insert(t.name.clone(), Value::Object(o));
                }
                None => return self.app_exception(&h, app_exception::INTERNAL_ERROR, &format!("Internal error processing {}: {}", m.name, e.message)),
            }
        } else {
            let out = ex.body().clone();
            let thrown = out.get("@exception").map(|v| v.to_string()).and_then(|k| declared(&k));
            match thrown {
                Some(t) => {
                    let mut o = out.as_object().cloned().unwrap_or_default();
                    o.remove("@exception");
                    res.insert(t.name.clone(), Value::Object(o));
                }
                None if m.ret.is_some() => {
                    if out.is_null() {
                        return self.app_exception(&h, app_exception::MISSING_RESULT, &format!("{} failed: unknown result", m.name));
                    }
                    let out = match (&out, m.ret.as_ref()) {
                        (Value::String(s), Some(thrift::TType::Struct(_) | thrift::TType::List(_) | thrift::TType::Map(..) | thrift::TType::Set(_))) => camel_core::json::parse(s).unwrap_or(out.clone()),
                        _ => out,
                    };
                    res.insert("success".into(), out);
                }
                None => {}
            }
        }
        let rh = MessageHeader { name: h.name.clone(), mtype: message_type::REPLY, seqid: h.seqid };
        match encode_message(idl, self.cfg.protocol, &rh, &result_name, &Value::Object(res)) {
            Ok(b) => Ok(Some(b)),
            Err(e) => self.app_exception(&h, app_exception::INTERNAL_ERROR, &e.message),
        }
    }
}

impl Service for ThriftConsumer {
    fn start(&self) -> Result<()> {
        if self.running.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let host = if self.cfg.host == "localhost" { "127.0.0.1" } else { &self.cfg.host };
        let l = TcpListener::bind((host, self.cfg.port)).map_err(|e| CamelError::io(format!("thrift consumer cannot bind {}:{}: {}", host, self.cfg.port, e)))?;
        let addr = l.local_addr().map_err(|e| CamelError::io(e.to_string()))?;
        *self.port.lock().unwrap() = Some(addr.port());
        *self.addr.lock().unwrap() = Some(addr);
        let h = Arc::new(Handler { cfg: self.cfg.clone(), ctx: self.ctx.clone(), processor: self.processor.clone() });
        let running = self.running.clone();
        std::thread::Builder::new()
            .name("thrift-server".into())
            .spawn(move || {
                for c in l.incoming() {
                    if !running.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(s) = c {
                        let h = h.clone();
                        let _ = std::thread::Builder::new().name("thrift-conn".into()).spawn(move || h.serve(s));
                    }
                }
            })
            .map_err(|e| CamelError::io(e.to_string()))?;
        Ok(())
    }
    fn stop(&self) -> Result<()> {
        if self.running.swap(false, Ordering::SeqCst) {
            if let Some(a) = self.addr.lock().unwrap().take() {
                let ip = if a.ip().is_unspecified() { "127.0.0.1".parse().unwrap() } else { a.ip() };
                let _ = TcpStream::connect_timeout(&std::net::SocketAddr::new(ip, a.port()), Duration::from_secs(1));
            }
        }
        *self.port.lock().unwrap() = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDL: &str = r#"
        enum Operation { ADD = 1, SUBTRACT = 2, MULTIPLY = 3, DIVIDE = 4 }
        struct Work { 1: i32 num1 = 0, 2: i32 num2, 3: Operation op, 4: optional string comment }
        exception InvalidOperation { 1: i32 whatOp, 2: string why }
        service Base { void ping() }
        service Calculator extends Base {
            i32 add(1:i32 num1, 2:i32 num2),
            i32 calculate(1:i32 logid, 2:Work w) throws (1:InvalidOperation ouch),
            oneway void zip()
        }
    "#;

    #[test]
    fn message_round_trip() {
        let idl = Idl::parse(IDL).unwrap();
        let h = MessageHeader { name: "add".into(), mtype: message_type::CALL, seqid: 7 };
        let v = camel_core::json::parse(r#"{"num1":1,"num2":2}"#).unwrap();
        for p in [WireProtocol::Binary, WireProtocol::Compact, WireProtocol::Json] {
            let b = encode_message(&idl, p, &h, "Calculator.add_args", &v).unwrap();
            let (h2, body) = parse_message(p, &b).unwrap();
            assert_eq!(h2, h);
            assert_eq!(body.decode(&idl, "Calculator.add_args").unwrap(), v);
            if p != WireProtocol::Json {
                let n = unframed_len(&idl, p, &b, &|_| None).unwrap();
                assert_eq!(n, Some(b.len()));
                assert_eq!(unframed_len(&idl, p, &b[..b.len() - 1], &|_| None).unwrap(), None);
            }
        }
        // strict binary header bytes
        let b = encode_message(&idl, WireProtocol::Binary, &h, "Calculator.add_args", &v).unwrap();
        assert_eq!(&b[..4], &[0x80, 0x01, 0x00, 0x01]);
        let j = encode_message(&idl, WireProtocol::Json, &h, "Calculator.add_args", &v).unwrap();
        assert_eq!(String::from_utf8(j).unwrap(), r#"[1,"add",1,7,{"1":{"i32":1},"2":{"i32":2}}]"#);
    }

    #[test]
    fn zlib_segments() {
        let mut stream = vec![0x78, 0x9c];
        let msgs: Vec<Vec<u8>> = (0..20).map(|i| format!("message {} {}", i, "abc".repeat(i * 7)).into_bytes()).collect();
        for m in &msgs {
            stream.extend(zlib_sync_segment(m));
        }
        let mut z = ZlibIn::default();
        let mut out = Vec::new();
        for chunk in stream.chunks(5) {
            out.extend(z.feed(chunk).unwrap());
        }
        assert_eq!(out, msgs.concat());
    }
}

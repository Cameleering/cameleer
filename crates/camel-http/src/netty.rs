//! `netty` component: TCP and UDP sockets (Camel `camel-netty`), std only.
//!
//! `netty:tcp://host:port?textline=true&sync=true` / `netty:udp://host:port`
//!
//! - Consumer: a server (TCP listener or bound UDP socket). Each received message
//!   (a delimited line with `textline=true`, otherwise one read / datagram of raw bytes)
//!   is routed; with `sync=true` the resulting body is written back as the reply.
//! - Producer: a client. Sends the body; with `sync=true` waits for one reply message
//!   (`requestTimeout`) which becomes the body.
//!
//! Options: `textline` (false), `sync` (true), `delimiter` (`LINE`|`NULL`),
//! `autoAppendDelimiter` (true), `decoderMaxLineLength` (1024), `encoding` (UTF-8,
//! US-ASCII, ISO-8859-1), `allowDefaultCodec` (true; the default codec is raw bytes),
//! `disconnect` (false), `disconnectOnNoReply` (true), `keepAlive` (true),
//! `reuseAddress` (true), `tcpNoDelay` (true), `requestTimeout` (0 = none for TCP,
//! 30s for UDP), `connectTimeout` (10s), plus accepted-and-ignored tuning options.
//! Headers set on consumed exchanges: `CamelNettyRemoteAddress`, `CamelNettyLocalAddress`.
//! Setting header `CamelNettyCloseChannelWhenComplete=true` closes the connection after
//! the reply. Port `0` picks a free port (see [`crate::local_port`]).
//!
//! TLS (TCP only): `ssl=true` with `sslContextParameters=#bean` (or
//! `useGlobalSslContextParameters=true` / the global `camel.ssl.*` parameters), or the
//! netty-style `keyStoreFile` (PEM with certificate chain and key) and `trustStoreFile`
//! (CA PEM) options. `hostnameVerification` (default false, as in Camel netty) controls
//! whether the producer checks the server certificate's host name; the chain is always
//! verified against the trust store. Client certificates (`needClientAuth`) are not supported.

use std::any::Any;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{kinds, CamelError, Result};
use camel_core::executor::ThreadPool;
use camel_core::message::{Exchange, ExchangePattern};
use camel_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::value::Value;
use camel_tls::camel_support::{endpoint_tls, EndpointTls};
use camel_tls::net::NetStream;
use camel_tls::SslContextParameters;

use crate::client::{connect, map_io_error, parse_authority};

/// Header: remote address of the peer.
pub const NETTY_REMOTE_ADDRESS: &str = "CamelNettyRemoteAddress";
/// Header: local address.
pub const NETTY_LOCAL_ADDRESS: &str = "CamelNettyLocalAddress";
/// Header: close the channel after this exchange.
pub const NETTY_CLOSE_CHANNEL_WHEN_COMPLETE: &str = "CamelNettyCloseChannelWhenComplete";

/// Transport protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    Tcp,
    Udp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Charset {
    Utf8,
    Ascii,
    Latin1,
}

impl Charset {
    fn parse(s: &str) -> Result<Charset> {
        match s.to_ascii_uppercase().replace('_', "-").as_str() {
            "UTF-8" | "UTF8" => Ok(Charset::Utf8),
            "US-ASCII" | "ASCII" => Ok(Charset::Ascii),
            "ISO-8859-1" | "LATIN1" | "ISO8859-1" => Ok(Charset::Latin1),
            other => Err(CamelError::illegal_argument(format!("Unsupported encoding: {}", other))),
        }
    }
    fn decode(&self, b: &[u8]) -> String {
        match self {
            Charset::Latin1 => b.iter().map(|&c| c as char).collect(),
            _ => String::from_utf8_lossy(b).into_owned(),
        }
    }
    fn encode(&self, s: &str) -> Vec<u8> {
        match self {
            Charset::Latin1 => s.chars().map(|c| if (c as u32) < 256 { c as u8 } else { b'?' }).collect(),
            Charset::Ascii => s.chars().map(|c| if c.is_ascii() { c as u8 } else { b'?' }).collect(),
            Charset::Utf8 => s.as_bytes().to_vec(),
        }
    }
}

/// Endpoint configuration.
#[derive(Clone, Debug)]
pub struct NettyConfig {
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    pub textline: bool,
    pub sync: bool,
    /// Line delimiter byte (`\n` for LINE, `\0` for NULL).
    pub delimiter: u8,
    pub auto_append_delimiter: bool,
    pub max_line_length: usize,
    charset: Charset,
    pub disconnect: bool,
    pub disconnect_on_no_reply: bool,
    pub keep_alive: bool,
    pub reuse_address: bool,
    pub tcp_no_delay: bool,
    pub request_timeout: Duration,
    pub connect_timeout: Duration,
    pub worker_count: usize,
    /// TLS settings when `ssl=true`.
    pub tls: Option<EndpointTls>,
}

/// The `netty` component.
#[derive(Default)]
pub struct NettyComponent;

impl Component for NettyComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let (protocol, rest) = if let Some(r) = remaining.strip_prefix("tcp://").or_else(|| remaining.strip_prefix("tcp:")) {
            (Protocol::Tcp, r)
        } else if let Some(r) = remaining.strip_prefix("udp://").or_else(|| remaining.strip_prefix("udp:")) {
            (Protocol::Udp, r)
        } else {
            return Err(CamelError::illegal_argument(format!("Unsupported netty protocol in {} (expected tcp:// or udp://)", uri)));
        };
        let authority = rest.trim_end_matches('/');
        let (host, port) = parse_authority(authority, 0).map_err(CamelError::illegal_argument)?;
        let delimiter = match params.take_string("delimiter", "LINE").to_ascii_uppercase().as_str() {
            "LINE" => b'\n',
            "NULL" => 0u8,
            other => return Err(CamelError::illegal_argument(format!("Invalid delimiter: {} (LINE or NULL)", other))),
        };
        for unsupported in ["encoders", "decoders", "encoder", "decoder"] {
            if params.take(unsupported).is_some() {
                camel_core::camel_warn!("netty", "Option {} is not supported by rust-camel netty and is ignored", unsupported);
            }
        }
        let key_store = params.take("keyStoreFile").or_else(|| params.take("keyStoreResource"));
        let trust_store = params.take("trustStoreFile").or_else(|| params.take("trustStoreResource"));
        let hostname_verification = params.take_bool("hostnameVerification", false)?;
        // Camel netty: keyStoreFormat (JKS, PKCS12 or PEM; detected when absent) and passphrase
        let key_store_format = params.take("keyStoreFormat");
        let passphrase = params.take("passphrase");
        let client_auth = camel_tls::camel_support::take_client_auth(params)?;
        let mut tls = endpoint_tls(ctx, params, "netty", false, &["ssl"])?;
        if let Some(t) = tls.as_mut() {
            let mut p: SslContextParameters = (*t.params).clone();
            if let Some(k) = &key_store {
                p.cert_chain = Some(camel_tls::PemSource::parse(k));
                p.private_key = None;
            }
            if let Some(ts) = &trust_store {
                p.trust_store = vec![camel_tls::PemSource::parse(ts)];
            }
            if let Some(f) = &key_store_format {
                p.key_store_type = Some(f.clone());
                p.trust_store_type = Some(f.clone());
            }
            if let Some(pw) = &passphrase {
                p.key_store_password = Some(pw.clone());
                p.trust_store_password = Some(pw.clone());
            }
            if client_auth.is_some() {
                p.client_authentication = client_auth;
            }
            if p.hostname_verification.is_none() {
                p.hostname_verification = Some(hostname_verification);
            }
            *t = EndpointTls::new(Arc::new(p));
        }
        if tls.is_some() && protocol == Protocol::Udp {
            return Err(CamelError::illegal_argument("ssl=true is only supported with netty:tcp (no DTLS)"));
        }
        let cfg = NettyConfig {
            protocol,
            host,
            port,
            textline: params.take_bool("textline", false)?,
            sync: params.take_bool("sync", true)?,
            delimiter,
            auto_append_delimiter: params.take_bool("autoAppendDelimiter", true)?,
            max_line_length: params.take_usize("decoderMaxLineLength", 1024)?,
            charset: Charset::parse(&params.take_string("encoding", "UTF-8"))?,
            disconnect: params.take_bool("disconnect", false)?,
            disconnect_on_no_reply: params.take_bool("disconnectOnNoReply", true)?,
            keep_alive: params.take_bool("keepAlive", true)?,
            reuse_address: params.take_bool("reuseAddress", true)?,
            tcp_no_delay: params.take_bool("tcpNoDelay", true)?,
            request_timeout: params.take_duration("requestTimeout", Duration::ZERO)?,
            connect_timeout: params.take_duration("connectTimeout", Duration::from_secs(10))?,
            worker_count: params.take_usize("workerCount", 16)?,
            tls,
        };
        params.take_bool("allowDefaultCodec", true)?;
        for ignored in [
            "bossCount",
            "receiveBufferSize",
            "sendBufferSize",
            "reuseChannel",
            "lazyChannelCreation",
            "broadcast",
            "udpConnectionlessSending",
            "synchronous",
            "transferExchange",
            "usingExecutorService",
            "backlog",
            "producerPoolEnabled",
            "producerPoolMaxTotal",
            "producerPoolMinIdle",
            "producerPoolMaxIdle",
            "clientMode",
            "allowSerializedHeaders",
            "textlineDelimiter",
            "lazyStartProducer",
            "bridgeErrorHandler",
            "exchangePattern",
            "options",
            "passphrase",
            "keyStoreFormat",
            "securityProvider",
            "sslHandler",
            "enabledProtocols",
        ] {
            params.take(ignored);
        }
        Ok(Arc::new(NettyEndpoint { uri: uri.to_string(), cfg, ctx: ctx.downgrade(), bound_port: Arc::new(Mutex::new(None)) }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A `netty` endpoint.
pub struct NettyEndpoint {
    uri: String,
    cfg: NettyConfig,
    ctx: WeakContext,
    bound_port: Arc<Mutex<Option<u16>>>,
}

impl NettyEndpoint {
    /// The actual bound port while a consumer is running.
    pub fn local_port(&self) -> Option<u16> {
        *self.bound_port.lock().unwrap()
    }
    pub fn config(&self) -> &NettyConfig {
        &self.cfg
    }
}

impl Endpoint for NettyEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(NettyProducer { cfg: self.cfg.clone() }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        Ok(Arc::new(NettyConsumer {
            cfg: self.cfg.clone(),
            ctx: self.ctx.clone(),
            processor,
            bound_port: self.bound_port.clone(),
            state: Mutex::new(None),
        }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---------------------------------------------------------------------------
// codec helpers
// ---------------------------------------------------------------------------

/// Read one message: a delimited line (textline) or one chunk of raw bytes.
/// Returns `Ok(None)` on EOF.
fn read_message<R: BufRead>(r: &mut R, cfg: &NettyConfig) -> std::io::Result<Option<Vec<u8>>> {
    if cfg.textline {
        let mut buf = Vec::new();
        let n = r.by_ref().take(cfg.max_line_length as u64 + 2).read_until(cfg.delimiter, &mut buf)?;
        if n == 0 {
            return Ok(None);
        }
        if buf.last() == Some(&cfg.delimiter) {
            buf.pop();
        } else if buf.len() > cfg.max_line_length {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("frame length exceeds {}", cfg.max_line_length)));
        }
        if cfg.delimiter == b'\n' && buf.last() == Some(&b'\r') {
            buf.pop();
        }
        Ok(Some(buf))
    } else {
        let data = r.fill_buf()?;
        if data.is_empty() {
            return Ok(None);
        }
        let v = data.to_vec();
        r.consume(v.len());
        Ok(Some(v))
    }
}

fn decode_body(bytes: &[u8], cfg: &NettyConfig) -> Value {
    if cfg.textline {
        Value::String(cfg.charset.decode(bytes))
    } else {
        Value::Bytes(bytes.to_vec())
    }
}

fn encode_body(v: &Value, cfg: &NettyConfig) -> Vec<u8> {
    let mut out = match v {
        Value::Bytes(b) => b.clone(),
        Value::Null => Vec::new(),
        other => cfg.charset.encode(&other.to_string()),
    };
    if cfg.textline && cfg.auto_append_delimiter && out.last() != Some(&cfg.delimiter) {
        out.push(cfg.delimiter);
    }
    out
}

fn bind_addr(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{}]:{}", host, port)
    } else {
        format!("{}:{}", host, port)
    }
}

fn loopback_for(addr: SocketAddr) -> SocketAddr {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    let ip = match addr.ip() {
        IpAddr::V4(v) if v.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(v) if v.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    SocketAddr::new(ip, addr.port())
}

// ---------------------------------------------------------------------------
// consumer
// ---------------------------------------------------------------------------

struct Running {
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    local: SocketAddr,
    connections: Arc<Mutex<HashMap<u64, TcpStream>>>,
    pool: Option<Arc<ThreadPool>>,
}

struct NettyConsumer {
    cfg: NettyConfig,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
    bound_port: Arc<Mutex<Option<u16>>>,
    state: Mutex<Option<Running>>,
}

/// Route one received message; returns the reply bytes (None = no reply).
fn route_message(
    ctx: &WeakContext,
    processor: &Arc<dyn Processor>,
    cfg: &NettyConfig,
    data: &[u8],
    remote: Option<SocketAddr>,
    local: Option<SocketAddr>,
    client_certs: &[Vec<u8>],
) -> (Option<Vec<u8>>, bool) {
    let ctx = match ctx.upgrade() {
        Some(c) => c,
        None => return (None, true),
    };
    let mut ex = Exchange::new(Some(ctx));
    ex.pattern = if cfg.sync { ExchangePattern::InOut } else { ExchangePattern::InOnly };
    ex.set_body(decode_body(data, cfg));
    if let Some(r) = remote {
        ex.set_header(NETTY_REMOTE_ADDRESS, r.to_string());
    }
    if let Some(l) = local {
        ex.set_header(NETTY_LOCAL_ADDRESS, l.to_string());
    }
    // mutual TLS (needClientAuth): Camel's CamelNettySSLClientCert* headers
    for (k, v) in camel_tls::camel_support::client_cert_headers("CamelNettySSLClientCert", client_certs) {
        ex.set_header(&k, v);
    }
    if let Err(e) = processor.process(&mut ex) {
        ex.set_exception(e);
    }
    let close = cfg.disconnect || ex.header(NETTY_CLOSE_CHANNEL_WHEN_COMPLETE).map(|v| v.is_truthy()).unwrap_or(false);
    if !cfg.sync {
        return (None, close);
    }
    let reply = match &ex.exception {
        Some(e) => Value::String(e.to_string()),
        None => ex.body().clone(),
    };
    if reply.is_null() {
        return (None, close || cfg.disconnect_on_no_reply);
    }
    (Some(encode_body(&reply, cfg)), close)
}

fn serve_tcp(tcp: TcpStream, cfg: NettyConfig, ctx: WeakContext, processor: Arc<dyn Processor>, running: Arc<AtomicBool>) {
    let _ = tcp.set_nodelay(cfg.tcp_no_delay);
    let remote = tcp.peer_addr().ok();
    let local = tcp.local_addr().ok();
    let stream = match &cfg.tls {
        Some(t) => match t.accept(tcp, "netty consumer") {
            Ok(s) => s,
            Err(e) => {
                camel_core::camel_debug!("netty", "TLS handshake with {:?} failed: {}", remote, e);
                return;
            }
        },
        None => NetStream::plain(tcp),
    };
    let mut reader = BufReader::new(&stream);
    let mut writer = &stream;
    while running.load(Ordering::SeqCst) {
        let data = match read_message(&mut reader, &cfg) {
            Ok(Some(d)) => d,
            _ => break,
        };
        let (reply, close) = route_message(&ctx, &processor, &cfg, &data, remote, local, stream.peer_certificates());
        if let Some(r) = reply {
            if writer.write_all(&r).and_then(|_| writer.flush()).is_err() {
                break;
            }
        }
        if close {
            break;
        }
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

impl Service for NettyConsumer {
    fn start(&self) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if state.is_some() {
            return Ok(());
        }
        let addr = bind_addr(&self.cfg.host, self.cfg.port);
        let running = Arc::new(AtomicBool::new(true));
        let connections: Arc<Mutex<HashMap<u64, TcpStream>>> = Arc::new(Mutex::new(HashMap::new()));
        let r = match self.cfg.protocol {
            Protocol::Tcp => {
                let listener = TcpListener::bind(&addr).map_err(|e| CamelError::io(format!("Failed to bind netty tcp consumer on {}: {}", addr, e)))?;
                let local = listener.local_addr().map_err(|e| CamelError::io(e.to_string()))?;
                let pool = ThreadPool::new(&format!("netty-tcp-{}", local.port()), self.cfg.worker_count);
                let (cfg, ctx, processor, run, conns, pool2) =
                    (self.cfg.clone(), self.ctx.clone(), self.processor.clone(), running.clone(), connections.clone(), pool.clone());
                let thread = std::thread::Builder::new()
                    .name(format!("netty-accept-{}", local.port()))
                    .spawn(move || {
                        let ids = AtomicU64::new(1);
                        for s in listener.incoming() {
                            if !run.load(Ordering::SeqCst) {
                                break;
                            }
                            let Ok(s) = s else {
                                std::thread::sleep(Duration::from_millis(10));
                                continue;
                            };
                            let id = ids.fetch_add(1, Ordering::Relaxed);
                            if let Ok(c) = s.try_clone() {
                                conns.lock().unwrap().insert(id, c);
                            }
                            let (cfg, ctx, processor, run2, conns2) = (cfg.clone(), ctx.clone(), processor.clone(), run.clone(), conns.clone());
                            pool2.execute(move || {
                                serve_tcp(s, cfg, ctx, processor, run2);
                                conns2.lock().unwrap().remove(&id);
                            });
                        }
                    })
                    .map_err(|e| CamelError::io(e.to_string()))?;
                Running { running, thread: Some(thread), local, connections, pool: Some(pool) }
            }
            Protocol::Udp => {
                let socket = UdpSocket::bind(&addr).map_err(|e| CamelError::io(format!("Failed to bind netty udp consumer on {}: {}", addr, e)))?;
                let local = socket.local_addr().map_err(|e| CamelError::io(e.to_string()))?;
                let _ = socket.set_read_timeout(Some(Duration::from_millis(50)));
                let (cfg, ctx, processor, run) = (self.cfg.clone(), self.ctx.clone(), self.processor.clone(), running.clone());
                let thread = std::thread::Builder::new()
                    .name(format!("netty-udp-{}", local.port()))
                    .spawn(move || {
                        let mut buf = vec![0u8; 65536];
                        while run.load(Ordering::SeqCst) {
                            let (n, from) = match socket.recv_from(&mut buf) {
                                Ok(x) => x,
                                Err(_) => continue,
                            };
                            let mut data = &buf[..n];
                            if cfg.textline {
                                if data.last() == Some(&cfg.delimiter) {
                                    data = &data[..data.len() - 1];
                                }
                                if cfg.delimiter == b'\n' && data.last() == Some(&b'\r') {
                                    data = &data[..data.len() - 1];
                                }
                            }
                            let (reply, _) = route_message(&ctx, &processor, &cfg, data, Some(from), Some(local), &[]);
                            if let Some(r) = reply {
                                let _ = socket.send_to(&r, from);
                            }
                        }
                    })
                    .map_err(|e| CamelError::io(e.to_string()))?;
                Running { running, thread: Some(thread), local, connections, pool: None }
            }
        };
        *self.bound_port.lock().unwrap() = Some(r.local.port());
        camel_core::camel_info!("netty", "Netty consumer bound to {:?}://{}", self.cfg.protocol, r.local);
        *state = Some(r);
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        let Some(mut r) = self.state.lock().unwrap().take() else {
            return Ok(());
        };
        r.running.store(false, Ordering::SeqCst);
        let woke = match self.cfg.protocol {
            Protocol::Tcp => TcpStream::connect_timeout(&loopback_for(r.local), Duration::from_secs(1)).is_ok(),
            Protocol::Udp => true,
        };
        if let Some(t) = r.thread.take() {
            if woke {
                let _ = t.join();
            }
        }
        for s in r.connections.lock().unwrap().values() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
        if let Some(p) = r.pool.take() {
            p.shutdown();
        }
        *self.bound_port.lock().unwrap() = None;
        Ok(())
    }
}

impl Drop for NettyConsumer {
    fn drop(&mut self) {
        let _ = Service::stop(self);
    }
}

// ---------------------------------------------------------------------------
// producer
// ---------------------------------------------------------------------------

struct NettyProducer {
    cfg: NettyConfig,
}

impl Service for NettyProducer {}

impl NettyProducer {
    fn timeout_error(&self) -> CamelError {
        CamelError::new(
            kinds::EXCHANGE_TIMED_OUT,
            format!("No response received from remote server: {}:{} within {} millis", self.cfg.host, self.cfg.port, self.cfg.request_timeout.as_millis()),
        )
    }

    fn send_tcp(&self, ex: &mut Exchange) -> Result<()> {
        let what = format!("{}:{}", self.cfg.host, self.cfg.port);
        let stream = match &self.cfg.tls {
            Some(t) => t.connect(&self.cfg.host, self.cfg.port, self.cfg.connect_timeout)?,
            None => NetStream::plain(connect(&self.cfg.host, self.cfg.port, self.cfg.connect_timeout)?),
        };
        let _ = stream.set_nodelay(self.cfg.tcp_no_delay);
        let timeout = if self.cfg.request_timeout.is_zero() { None } else { Some(self.cfg.request_timeout) };
        let _ = stream.set_read_timeout(timeout);
        let mut w = &stream;
        let payload = encode_body(ex.body(), &self.cfg);
        w.write_all(&payload).and_then(|_| w.flush()).map_err(|e| map_io_error(e, &what))?;
        if self.cfg.sync {
            let mut reader = BufReader::new(&stream);
            match read_message(&mut reader, &self.cfg) {
                Ok(Some(d)) => ex.set_body(decode_body(&d, &self.cfg)),
                Ok(None) => ex.set_body(Value::Null),
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                    return Err(self.timeout_error())
                }
                Err(e) => return Err(map_io_error(e, &what)),
            }
        }
        let _ = stream.shutdown(std::net::Shutdown::Both);
        Ok(())
    }

    fn send_udp(&self, ex: &mut Exchange) -> Result<()> {
        use std::net::ToSocketAddrs;
        let what = format!("{}:{}", self.cfg.host, self.cfg.port);
        let target = (self.cfg.host.as_str(), self.cfg.port)
            .to_socket_addrs()
            .map_err(|e| map_io_error(e, &what))?
            .next()
            .ok_or_else(|| CamelError::new(kinds::CONNECT, format!("Unknown host: {}", self.cfg.host)))?;
        let bind = if target.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" };
        let socket = UdpSocket::bind(bind).map_err(|e| map_io_error(e, &what))?;
        let payload = encode_body(ex.body(), &self.cfg);
        socket.send_to(&payload, target).map_err(|e| map_io_error(e, &what))?;
        if self.cfg.sync {
            let timeout = if self.cfg.request_timeout.is_zero() { Duration::from_secs(30) } else { self.cfg.request_timeout };
            let _ = socket.set_read_timeout(Some(timeout));
            let mut buf = vec![0u8; 65536];
            match socket.recv_from(&mut buf) {
                Ok((n, _)) => {
                    let mut data = &buf[..n];
                    if self.cfg.textline && data.last() == Some(&self.cfg.delimiter) {
                        data = &data[..n - 1];
                    }
                    ex.set_body(decode_body(data, &self.cfg));
                }
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => return Err(self.timeout_error()),
                Err(e) => return Err(map_io_error(e, &what)),
            }
        }
        Ok(())
    }
}

impl Processor for NettyProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        match self.cfg.protocol {
            Protocol::Tcp => self.send_tcp(ex),
            Protocol::Udp => self.send_udp(ex),
        }
    }
}

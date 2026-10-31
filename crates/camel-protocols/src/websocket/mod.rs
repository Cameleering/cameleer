//! WebSocket components (RFC 6455, plain `ws` only):
//!
//! | scheme | role |
//! |---|---|
//! | `websocket://host:port/path` | server (camel-websocket): consumer receives messages, producer sends to connections |
//! | `vertx-websocket:host:port/path` | server like `websocket` (headers `CamelVertxWebsocket.*`); with `consumeAsClient=true` the consumer connects as a client; a producer without a local server endpoint acts as a client |
//! | `ws://host:port/path` | client: the consumer receives the server's messages, the producer sends on the same connection |
//!
//! Server consumers on the same host/port share one listener (`websocket` defaults to
//! `0.0.0.0:9292`, `vertx-websocket` to port 0 = ephemeral, see [`crate::local_port`]).
//! A `websocket` producer also starts the server for its path, so clients can connect
//! to receive broadcasts.
//!
//! **Consumer headers**: `CamelWebsocketConnectionKey`, `CamelWebsocketRemoteAddress`
//! (vertx: `CamelVertxWebsocket.connectionKey`, `CamelVertxWebsocket.remoteAddress`,
//! `CamelVertxWebsocket.event` = `MESSAGE`/`OPEN`/`CLOSE`). Text messages become `String`
//! bodies, binary messages `Bytes`.
//!
//! **Producer**: sends the body (bytes as a binary message, anything else as text) to the
//! connection(s) named by the connection key header (comma separated for vertx), or to
//! every connection of the path with `sendToAll=true` / header `CamelWebsocketSendToAll`
//! (`CamelVertxWebsocket.sendToAll`).
//!
//! **Options**: `sendToAll`, `maxTextMessageSize`/`maxBinaryMessageSize`/`maxMessageSize`
//! (incoming limit, default 64 MiB), `maxFrameSize` (outgoing fragmentation, 64 KiB),
//! `allowedOrigins` (comma separated, `*` = any), `subProtocol`/`subProtocols`,
//! `fireWebSocketConnectionEvents` (vertx), `consumeAsClient` (vertx), `connectTimeout`
//! (10s), `reconnectInterval` (client consumers, 0 = no reconnect).

pub mod protocol;
pub mod sha1;

use std::any::Any;
use std::collections::HashMap;
use std::io::BufReader;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

use camel_core::component::support::process_consumer_exchange;
use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::value::Value;

pub use protocol::{accept_key, WsConnection, WsMessage};

use crate::common::{split_host_port, take_ignored, TcpServer, COMMON_IGNORED};

/// Header: connection key (camel-websocket).
pub const CONNECTION_KEY: &str = "CamelWebsocketConnectionKey";
/// Header: send to all connections (camel-websocket).
pub const SEND_TO_ALL: &str = "CamelWebsocketSendToAll";
/// Header: remote address (camel-websocket).
pub const REMOTE_ADDRESS: &str = "CamelWebsocketRemoteAddress";
/// Header: connection key (vertx-websocket).
pub const VERTX_CONNECTION_KEY: &str = "CamelVertxWebsocket.connectionKey";
/// Header: send to all (vertx-websocket).
pub const VERTX_SEND_TO_ALL: &str = "CamelVertxWebsocket.sendToAll";
/// Header: remote address (vertx-websocket).
pub const VERTX_REMOTE_ADDRESS: &str = "CamelVertxWebsocket.remoteAddress";
/// Header: event type `MESSAGE`, `OPEN`, `CLOSE` (vertx-websocket).
pub const VERTX_EVENT: &str = "CamelVertxWebsocket.event";
/// Exception kind for send failures.
pub const WEBSOCKET_SEND_EXCEPTION: &str = "WebsocketSendException";

/// Which Camel component a websocket endpoint mimics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    /// `websocket` (camel-websocket).
    Camel,
    /// `vertx-websocket`.
    Vertx,
    /// `ws` client endpoints.
    Client,
}

impl Flavor {
    fn key_header(&self) -> &'static str {
        if *self == Flavor::Vertx {
            VERTX_CONNECTION_KEY
        } else {
            CONNECTION_KEY
        }
    }
    fn remote_header(&self) -> &'static str {
        if *self == Flavor::Vertx {
            VERTX_REMOTE_ADDRESS
        } else {
            REMOTE_ADDRESS
        }
    }
    fn send_to_all_header(&self) -> &'static str {
        if *self == Flavor::Vertx {
            VERTX_SEND_TO_ALL
        } else {
            SEND_TO_ALL
        }
    }
}

/// Endpoint options.
#[derive(Clone, Debug)]
pub struct WsOptions {
    pub send_to_all: bool,
    pub max_message_size: usize,
    pub max_frame_size: usize,
    pub allowed_origins: Vec<String>,
    pub sub_protocols: Vec<String>,
    pub fire_events: bool,
    pub consume_as_client: bool,
    pub connect_timeout: Duration,
    pub reconnect_interval: Duration,
}

impl WsOptions {
    fn parse(params: &mut Parameters) -> Result<WsOptions> {
        let mut max = 64 * 1024 * 1024usize;
        for k in ["maxTextMessageSize", "maxBinaryMessageSize", "maxMessageSize"] {
            if let Some(v) = params.take(k) {
                max = v.parse().map_err(|_| CamelError::illegal_argument(format!("Invalid {}: {}", k, v)))?;
            }
        }
        let list = |s: Option<String>| s.map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()).unwrap_or_default();
        let o = WsOptions {
            send_to_all: params.take_bool("sendToAll", false)?,
            max_message_size: max.max(1),
            max_frame_size: params.take_usize("maxFrameSize", 65536)?,
            allowed_origins: list(params.take("allowedOrigins").or_else(|| params.take("allowedOriginPattern"))),
            sub_protocols: list(params.take("subProtocol").or_else(|| params.take("subProtocols")).or_else(|| params.take("clientSubProtocols"))),
            fire_events: params.take_bool("fireWebSocketConnectionEvents", false)?,
            consume_as_client: params.take_bool("consumeAsClient", false)?,
            connect_timeout: params.take_duration("connectTimeout", Duration::from_secs(10))?,
            reconnect_interval: params.take_duration("reconnectInterval", Duration::ZERO)?,
        };
        if params.take("sslContextParameters").is_some() {
            return Err(CamelError::new(camel_core::error::kinds::UNSUPPORTED_OPERATION, "TLS (wss) is not supported by rust-camel"));
        }
        take_ignored(params, COMMON_IGNORED);
        take_ignored(
            params,
            &[
                "sessionSupport",
                "staticResources",
                "crossOriginFilterOn",
                "filterPath",
                "enableJmx",
                "minThreads",
                "maxThreads",
                "bufferSize",
                "maxIdleTime",
                "router",
                "serverOptions",
                "clientOptions",
                "reconnectInitialDelay",
                "reconnectMaxAttempts",
                "useGlobalSslContextParameters",
                "handshakeHeaders",
            ],
        );
        Ok(o)
    }
}

fn normalize_path(p: &str) -> String {
    let p = p.split('?').next().unwrap_or("");
    let p = p.trim_end_matches('/');
    if p.starts_with('/') {
        p.to_string()
    } else {
        format!("/{}", p)
    }
}

// ---------------------------------------------------------------------------
// server
// ---------------------------------------------------------------------------

struct ConsumerHandler {
    processor: Arc<dyn Processor>,
    ctx: WeakContext,
    flavor: Flavor,
    fire_events: bool,
    logger: String,
}

impl ConsumerHandler {
    fn route(&self, conn: &WsConnection, body: Value, event: &str) {
        let mut ex = Exchange::new(self.ctx.upgrade());
        ex.set_body(body);
        ex.set_header(self.flavor.key_header(), conn.id().to_string());
        if let Some(a) = conn.remote_addr() {
            ex.set_header(self.flavor.remote_header(), a.to_string());
        }
        if self.flavor == Flavor::Vertx {
            ex.set_header(VERTX_EVENT, event.to_string());
        }
        process_consumer_exchange(&self.logger, self.processor.as_ref(), &mut ex);
    }

    fn message(&self, conn: &WsConnection, msg: WsMessage) {
        let body = match msg {
            WsMessage::Text(t) => Value::String(t),
            WsMessage::Binary(b) => Value::Bytes(b),
        };
        self.route(conn, body, "MESSAGE");
    }
}

struct PathEntry {
    users: usize,
    consumer: Option<Arc<ConsumerHandler>>,
    options: WsOptions,
}

/// A WebSocket server shared by the endpoints on one host/port.
pub struct WsServer {
    host: String,
    port: u16,
    tcp: Mutex<Option<TcpServer>>,
    paths: Mutex<HashMap<String, PathEntry>>,
    conns: Mutex<HashMap<String, Arc<WsConnection>>>,
}

type Registry = Arc<Mutex<HashMap<(String, u16), Arc<WsServer>>>>;

impl WsServer {
    /// The actual listening port (while started).
    pub fn local_port(&self) -> Option<u16> {
        self.tcp.lock().unwrap().as_ref().map(|t| t.port())
    }

    /// Open connections, optionally only those of `path`.
    pub fn connections(&self, path: Option<&str>) -> Vec<Arc<WsConnection>> {
        let p = path.map(normalize_path);
        self.conns.lock().unwrap().values().filter(|c| p.as_deref().map(|p| c.path() == p).unwrap_or(true)).cloned().collect()
    }

    fn add_path(self: &Arc<Self>, path: &str, consumer: Option<Arc<ConsumerHandler>>, options: &WsOptions) -> Result<()> {
        {
            let mut paths = self.paths.lock().unwrap();
            let e = paths.entry(path.to_string()).or_insert_with(|| PathEntry { users: 0, consumer: None, options: options.clone() });
            e.users += 1;
            if consumer.is_some() {
                e.consumer = consumer;
                e.options = options.clone();
            }
        }
        let mut tcp = self.tcp.lock().unwrap();
        if tcp.is_none() {
            let weak: Weak<WsServer> = Arc::downgrade(self);
            let t = TcpServer::bind(&self.host, self.port, "websocket", move |s| {
                if let Some(srv) = weak.upgrade() {
                    srv.handle(s);
                }
            })?;
            camel_core::camel_info!("websocket", "WebSocket server listening on {}", t.local_addr());
            *tcp = Some(t);
        }
        Ok(())
    }

    /// Returns true if the server has no more paths (and was stopped).
    fn remove_path(&self, path: &str, consumer: bool) -> bool {
        let empty = {
            let mut paths = self.paths.lock().unwrap();
            if let Some(e) = paths.get_mut(path) {
                e.users = e.users.saturating_sub(1);
                if consumer {
                    e.consumer = None;
                }
                if e.users == 0 {
                    paths.remove(path);
                    for c in self.connections(Some(path)) {
                        c.close(1001, "going away");
                        c.shutdown();
                    }
                }
            }
            paths.is_empty()
        };
        if empty {
            if let Some(t) = self.tcp.lock().unwrap().take() {
                for c in self.connections(None) {
                    c.close(1001, "going away");
                    c.shutdown();
                }
                t.stop();
            }
        }
        empty
    }

    fn handle(self: Arc<Self>, stream: TcpStream) {
        let _ = stream.set_nodelay(true);
        let Ok(rs) = stream.try_clone() else { return };
        let mut reader = BufReader::new(rs);
        let mut w = &stream;
        let Ok(req) = protocol::read_request(&mut reader) else { return };
        let path = normalize_path(req.path());
        let options = self.paths.lock().unwrap().get(&path).map(|e| e.options.clone());
        let Some(options) = options else {
            let _ = protocol::write_reject(&mut w, 404, "Not Found", &format!("No websocket endpoint for {}", path));
            return;
        };
        let key = match req.websocket_key() {
            Ok(k) => k.to_string(),
            Err(msg) => {
                let _ = protocol::write_reject(&mut w, 400, "Bad Request", msg);
                return;
            }
        };
        if !options.allowed_origins.is_empty() && !options.allowed_origins.iter().any(|o| o == "*") {
            let origin = req.header("Origin").unwrap_or("");
            if !options.allowed_origins.iter().any(|o| o.eq_ignore_ascii_case(origin)) {
                let _ = protocol::write_reject(&mut w, 403, "Forbidden", "Origin not allowed");
                return;
            }
        }
        let protocol = req.header("Sec-WebSocket-Protocol").and_then(|offered| {
            offered.split(',').map(|p| p.trim()).find(|p| options.sub_protocols.iter().any(|s| s.eq_ignore_ascii_case(p))).map(|s| s.to_string())
        });
        if !options.sub_protocols.is_empty() && protocol.is_none() {
            let _ = protocol::write_reject(&mut w, 400, "Bad Request", "No supported sub protocol");
            return;
        }
        if protocol::write_accept(&mut w, &key, protocol.as_deref()).is_err() {
            return;
        }
        let Ok(write_half) = stream.try_clone() else { return };
        let Ok(mut conn) = WsConnection::new(write_half, reader, false, &path) else { return };
        conn.set_max_message_size(options.max_message_size);
        conn.set_max_frame_size(options.max_frame_size);
        let conn = Arc::new(conn);
        self.conns.lock().unwrap().insert(conn.id().to_string(), conn.clone());
        let handler = || self.paths.lock().unwrap().get(&path).and_then(|e| e.consumer.clone());
        if let Some(h) = handler().filter(|h| h.fire_events) {
            h.route(&conn, Value::Null, "OPEN");
        }
        while let Ok(Some(msg)) = conn.read_message() {
            if let Some(h) = handler() {
                h.message(&conn, msg);
            }
        }
        self.conns.lock().unwrap().remove(conn.id());
        if let Some(h) = handler().filter(|h| h.fire_events) {
            h.route(&conn, Value::Null, "CLOSE");
        }
        conn.shutdown();
    }
}

fn get_or_create_server(reg: &Registry, host: &str, port: u16) -> Arc<WsServer> {
    let mut r = reg.lock().unwrap();
    if let Some(s) = r.get(&(host.to_string(), port)) {
        return s.clone();
    }
    if port != 0 {
        if let Some(s) = r.values().find(|s| s.local_port() == Some(port)) {
            return s.clone();
        }
    }
    let s = Arc::new(WsServer { host: host.to_string(), port, tcp: Mutex::new(None), paths: Mutex::new(HashMap::new()), conns: Mutex::new(HashMap::new()) });
    r.insert((host.to_string(), port), s.clone());
    s
}

fn find_server(reg: &Registry, host: &str, port: u16, path: &str) -> Option<Arc<WsServer>> {
    let r = reg.lock().unwrap();
    let has_path = |s: &Arc<WsServer>| s.paths.lock().unwrap().contains_key(path);
    if let Some(s) = r.get(&(host.to_string(), port)).filter(|s| has_path(s)) {
        return Some(s.clone());
    }
    if port != 0 {
        return r.values().find(|s| s.local_port() == Some(port) && has_path(s)).cloned();
    }
    None
}

fn release_server(reg: &Registry, server: &Arc<WsServer>, path: &str, consumer: bool) {
    if server.remove_path(path, consumer) {
        reg.lock().unwrap().retain(|_, s| !Arc::ptr_eq(s, server));
    }
}

// ---------------------------------------------------------------------------
// client
// ---------------------------------------------------------------------------

/// A client connection shared by the consumer and producer of a client endpoint.
struct WsClient {
    host: String,
    port: u16,
    path: String,
    options: WsOptions,
    conn: Mutex<Option<Arc<WsConnection>>>,
    consumer: Mutex<Option<Arc<ConsumerHandler>>>,
    readers: Mutex<Vec<JoinHandle<()>>>,
    stopping: AtomicBool,
}

impl WsClient {
    fn connection(self: &Arc<Self>) -> Result<Arc<WsConnection>> {
        let mut guard = self.conn.lock().unwrap();
        if let Some(c) = guard.as_ref().filter(|c| c.is_open()) {
            return Ok(c.clone());
        }
        let mut c = WsConnection::connect(&self.host, self.port, &self.path, self.options.connect_timeout)?;
        c.set_max_message_size(self.options.max_message_size);
        c.set_max_frame_size(self.options.max_frame_size);
        let c = Arc::new(c);
        *guard = Some(c.clone());
        let (me, conn) = (self.clone(), c.clone());
        let h = std::thread::Builder::new()
            .name(format!("ws-client-{}", self.port))
            .spawn(move || me.read_loop(conn))
            .map_err(|e| CamelError::io(e.to_string()))?;
        let mut readers = self.readers.lock().unwrap();
        readers.retain(|h| !h.is_finished());
        readers.push(h);
        Ok(c)
    }

    fn read_loop(self: Arc<Self>, conn: Arc<WsConnection>) {
        while let Ok(Some(msg)) = conn.read_message() {
            let h = self.consumer.lock().unwrap().clone();
            if let Some(h) = h {
                h.message(&conn, msg);
            }
        }
        conn.shutdown();
        {
            let mut g = self.conn.lock().unwrap();
            if g.as_ref().map(|c| Arc::ptr_eq(c, &conn)).unwrap_or(false) {
                *g = None;
            }
        }
        // reconnect client consumers
        let interval = self.options.reconnect_interval;
        while !interval.is_zero() && !self.stopping.load(Ordering::SeqCst) && self.consumer.lock().unwrap().is_some() {
            std::thread::sleep(interval);
            if self.stopping.load(Ordering::SeqCst) {
                break;
            }
            if self.connection().is_ok() {
                break;
            }
        }
    }

    fn close(&self) {
        if let Some(c) = self.conn.lock().unwrap().take() {
            c.close(1000, "");
            c.shutdown();
        }
        let readers: Vec<JoinHandle<()>> = std::mem::take(&mut *self.readers.lock().unwrap());
        let me = std::thread::current().id();
        for h in readers {
            if h.thread().id() != me {
                let _ = h.join();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// component / endpoint
// ---------------------------------------------------------------------------

/// The `websocket`, `vertx-websocket` and `ws` components.
pub struct WebSocketComponent {
    flavor: Flavor,
    servers: Registry,
}

impl WebSocketComponent {
    pub fn new(flavor: Flavor) -> Self {
        WebSocketComponent { flavor, servers: Arc::new(Mutex::new(HashMap::new())) }
    }
}

impl Component for WebSocketComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let default_port = match self.flavor {
            Flavor::Camel => 9292,
            Flavor::Vertx => 0,
            Flavor::Client => 80,
        };
        let (authority, path) = match remaining.find('/') {
            Some(i) => (&remaining[..i], &remaining[i..]),
            None if remaining.contains(':') || self.flavor == Flavor::Client => (remaining, "/"),
            None => ("", remaining),
        };
        let (host, port) = split_host_port(authority, default_port)?;
        let options = WsOptions::parse(params)?;
        let path = normalize_path(path);
        let client = if self.flavor == Flavor::Client || (self.flavor == Flavor::Vertx) {
            let h = if host == "0.0.0.0" { "127.0.0.1".to_string() } else { host.clone() };
            Some(Arc::new(WsClient {
                host: h,
                port,
                path: path.clone(),
                options: options.clone(),
                conn: Mutex::new(None),
                consumer: Mutex::new(None),
                readers: Mutex::new(Vec::new()),
                stopping: AtomicBool::new(false),
            }))
        } else {
            None
        };
        Ok(Arc::new(WebSocketEndpoint {
            uri: uri.to_string(),
            ctx: ctx.downgrade(),
            flavor: self.flavor,
            host,
            port,
            path,
            options,
            servers: self.servers.clone(),
            client,
        }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A websocket endpoint.
pub struct WebSocketEndpoint {
    uri: String,
    ctx: WeakContext,
    flavor: Flavor,
    host: String,
    port: u16,
    path: String,
    options: WsOptions,
    servers: Registry,
    client: Option<Arc<WsClient>>,
}

impl WebSocketEndpoint {
    /// The actual port of the server this endpoint's consumer (or producer) runs on.
    pub fn local_port(&self) -> Option<u16> {
        find_server(&self.servers, &self.host, self.port, &self.path).and_then(|s| s.local_port())
    }

    /// Keys of the open server-side connections of this endpoint's path.
    pub fn connection_keys(&self) -> Vec<String> {
        find_server(&self.servers, &self.host, self.port, &self.path)
            .map(|s| s.connections(Some(&self.path)).iter().map(|c| c.id().to_string()).collect())
            .unwrap_or_default()
    }

    fn is_client_consumer(&self) -> bool {
        self.flavor == Flavor::Client || (self.flavor == Flavor::Vertx && self.options.consume_as_client)
    }
}

impl Endpoint for WebSocketEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(WsProducer {
            flavor: self.flavor,
            host: self.host.clone(),
            port: self.port,
            path: self.path.clone(),
            options: self.options.clone(),
            servers: self.servers.clone(),
            client: self.client.clone(),
            started: Mutex::new(None),
        }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        let handler = Arc::new(ConsumerHandler {
            processor,
            ctx: self.ctx.clone(),
            flavor: self.flavor,
            fire_events: self.options.fire_events,
            logger: self.uri.clone(),
        });
        if self.is_client_consumer() {
            return Ok(Arc::new(WsClientConsumer { client: self.client.clone().expect("client"), handler }));
        }
        Ok(Arc::new(WsServerConsumer {
            host: self.host.clone(),
            port: self.port,
            path: self.path.clone(),
            options: self.options.clone(),
            servers: self.servers.clone(),
            handler,
            server: Mutex::new(None),
        }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct WsServerConsumer {
    host: String,
    port: u16,
    path: String,
    options: WsOptions,
    servers: Registry,
    handler: Arc<ConsumerHandler>,
    server: Mutex<Option<Arc<WsServer>>>,
}

impl Service for WsServerConsumer {
    fn start(&self) -> Result<()> {
        let mut g = self.server.lock().unwrap();
        if g.is_some() {
            return Ok(());
        }
        let s = get_or_create_server(&self.servers, &self.host, self.port);
        if let Err(e) = s.add_path(&self.path, Some(self.handler.clone()), &self.options) {
            release_server(&self.servers, &s, &self.path, true);
            return Err(e);
        }
        *g = Some(s);
        Ok(())
    }
    fn stop(&self) -> Result<()> {
        if let Some(s) = self.server.lock().unwrap().take() {
            release_server(&self.servers, &s, &self.path, true);
        }
        Ok(())
    }
}

struct WsClientConsumer {
    client: Arc<WsClient>,
    handler: Arc<ConsumerHandler>,
}

impl Service for WsClientConsumer {
    fn start(&self) -> Result<()> {
        self.client.stopping.store(false, Ordering::SeqCst);
        *self.client.consumer.lock().unwrap() = Some(self.handler.clone());
        match self.client.connection() {
            Ok(_) => Ok(()),
            Err(e) if !self.client.options.reconnect_interval.is_zero() => {
                camel_core::camel_warn!(&self.handler.logger, "Cannot connect websocket client (will retry): {}", e);
                let c = self.client.clone();
                let _ = std::thread::Builder::new().name("ws-reconnect".into()).spawn(move || {
                    while !c.stopping.load(Ordering::SeqCst) && c.connection().is_err() {
                        std::thread::sleep(c.options.reconnect_interval);
                    }
                });
                Ok(())
            }
            Err(e) => {
                *self.client.consumer.lock().unwrap() = None;
                Err(e)
            }
        }
    }
    fn stop(&self) -> Result<()> {
        self.client.stopping.store(true, Ordering::SeqCst);
        *self.client.consumer.lock().unwrap() = None;
        self.client.close();
        Ok(())
    }
}

struct WsProducer {
    flavor: Flavor,
    host: String,
    port: u16,
    path: String,
    options: WsOptions,
    servers: Registry,
    client: Option<Arc<WsClient>>,
    /// The server a `websocket` producer registered its path on.
    started: Mutex<Option<Arc<WsServer>>>,
}

fn message_of(v: &Value) -> WsMessage {
    match v {
        Value::Bytes(b) => WsMessage::Binary(b.clone()),
        Value::Null => WsMessage::Text(String::new()),
        other => WsMessage::Text(other.to_string()),
    }
}

impl Service for WsProducer {
    fn start(&self) -> Result<()> {
        if self.flavor == Flavor::Camel {
            let mut g = self.started.lock().unwrap();
            if g.is_none() {
                let s = get_or_create_server(&self.servers, &self.host, self.port);
                s.add_path(&self.path, None, &self.options)?;
                *g = Some(s);
            }
        }
        Ok(())
    }
    fn stop(&self) -> Result<()> {
        if let Some(s) = self.started.lock().unwrap().take() {
            release_server(&self.servers, &s, &self.path, false);
        }
        if let Some(c) = &self.client {
            if c.consumer.lock().unwrap().is_none() {
                c.close();
            }
        }
        Ok(())
    }
}

impl Processor for WsProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let msg = message_of(ex.body());
        let server = if self.flavor == Flavor::Client { None } else { find_server(&self.servers, &self.host, self.port, &self.path) };
        let Some(server) = server else {
            // client mode (ws:, or vertx-websocket without a local server endpoint)
            let client = self.client.as_ref().ok_or_else(|| CamelError::new(WEBSOCKET_SEND_EXCEPTION, "No websocket server or client available"))?;
            return client.connection()?.send(&msg);
        };
        let send_all = self.options.send_to_all
            || ex.header(self.flavor.send_to_all_header()).or_else(|| ex.header("websocket.sendToAll")).map(|v| v.is_truthy()).unwrap_or(false);
        let targets: Vec<Arc<WsConnection>> = if send_all {
            server.connections(Some(&self.path))
        } else {
            let keys: Vec<String> = match ex.header(self.flavor.key_header()).or_else(|| ex.header("websocket.connectionKey")) {
                Some(Value::Array(a)) => a.iter().map(|v| v.to_string()).collect(),
                Some(v) if !v.is_null() => v.to_string().split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
                _ => {
                    return Err(CamelError::new(WEBSOCKET_SEND_EXCEPTION, "Failed to send message to single connection; connection key not set."));
                }
            };
            let conns = server.conns.lock().unwrap();
            let found: Vec<Arc<WsConnection>> = keys.iter().filter_map(|k| conns.get(k).cloned()).collect();
            if found.is_empty() {
                return Err(CamelError::new(WEBSOCKET_SEND_EXCEPTION, format!("Failed to send message to single connection; connection not found: {}", keys.join(","))));
            }
            found
        };
        let mut errors = Vec::new();
        for c in targets {
            if let Err(e) = c.send(&msg) {
                errors.push(format!("{}: {}", c.id(), e.message));
            }
        }
        if !errors.is_empty() {
            return Err(CamelError::new(WEBSOCKET_SEND_EXCEPTION, format!("Failed to send message to connection(s): {}", errors.join("; "))));
        }
        Ok(())
    }
}

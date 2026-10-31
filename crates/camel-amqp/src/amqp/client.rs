//! A blocking AMQP 0-9-1 client (the RabbitMQ protocol) on `std::net`.
//!
//! * connection negotiation (`connection.start/tune/open`), PLAIN authentication,
//!   virtual hosts, channel-max / frame-max negotiation and heartbeats (a background
//!   thread sends heartbeats, the reader treats two missed intervals as a dead peer);
//! * channels with synchronous RPCs: `exchange.declare/delete`, `queue.declare/bind/
//!   unbind/purge/delete`, `basic.qos`, `basic.consume/cancel`, `basic.get`,
//!   `basic.recover`, `confirm.select`;
//! * asynchronous `basic.publish` (with properties and header tables, body frames split
//!   at frame-max), `basic.ack/nack/reject`, publisher confirms
//!   ([`Channel::wait_for_confirms`]) and returned (unroutable mandatory) messages;
//! * deliveries are pushed to a per-consumer [`std::sync::mpsc::Receiver`].
//!
//! One reader thread per connection demultiplexes frames by channel.

use std::collections::{BTreeSet, HashMap};
use std::io::{BufReader, Write};
use std::net::Shutdown;

use camel_tls::camel_support::EndpointTls;
use camel_tls::net::NetStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use camel_core::component::support::StopFlag;
use camel_core::error::{kinds, CamelError, Result};
use camel_core::value::Value;

use super::codec::*;
use crate::common::{connect, io_error};

/// Exception kind of an authentication failure.
pub const AUTHENTICATION_FAILURE: &str = "AuthenticationFailureException";
/// Exception kind of a broker-side close of a channel or connection.
pub const SHUTDOWN_SIGNAL: &str = "ShutdownSignalException";

/// Connection settings.
#[derive(Clone, Debug)]
pub struct AmqpConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub vhost: String,
    /// Requested heartbeat interval in seconds (0 = disabled).
    pub heartbeat: u16,
    pub connect_timeout: Duration,
    /// Requested frame-max (0 = no limit).
    pub frame_max: u32,
    /// Requested channel-max (0 = no limit).
    pub channel_max: u16,
    /// How long synchronous RPCs wait for the broker.
    pub rpc_timeout: Duration,
    /// Reported in the `connection_name` client property.
    pub connection_name: Option<String>,
    /// TLS (`amqps`).
    pub tls: Option<EndpointTls>,
}

impl Default for AmqpConfig {
    fn default() -> Self {
        AmqpConfig {
            host: "localhost".into(),
            port: 5672,
            username: "guest".into(),
            password: "guest".into(),
            vhost: "/".into(),
            heartbeat: 60,
            connect_timeout: Duration::from_secs(30),
            frame_max: 131_072,
            channel_max: 2047,
            rpc_timeout: Duration::from_secs(30),
            connection_name: None,
            tls: None,
        }
    }
}

/// A delivered (or fetched, or returned) message.
#[derive(Clone, Debug, Default)]
pub struct Delivery {
    pub consumer_tag: String,
    pub delivery_tag: u64,
    pub redelivered: bool,
    pub exchange: String,
    pub routing_key: String,
    pub properties: BasicProperties,
    pub body: Vec<u8>,
    /// `basic.get-ok` message count (messages left in the queue).
    pub message_count: u32,
}

/// A message returned by the broker (mandatory publish with no route).
#[derive(Clone, Debug, Default)]
pub struct Returned {
    pub reply_code: u16,
    pub reply_text: String,
    pub exchange: String,
    pub routing_key: String,
    pub properties: BasicProperties,
    pub body: Vec<u8>,
}

/// Result of `queue.declare`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueueInfo {
    pub name: String,
    pub message_count: u32,
    pub consumer_count: u32,
}

/// Options of `queue.declare`.
#[derive(Clone, Debug, Default)]
pub struct QueueOptions {
    pub passive: bool,
    pub durable: bool,
    pub exclusive: bool,
    pub auto_delete: bool,
    pub arguments: Table,
}

/// Options of `exchange.declare`.
#[derive(Clone, Debug, Default)]
pub struct ExchangeOptions {
    pub passive: bool,
    pub durable: bool,
    pub auto_delete: bool,
    pub internal: bool,
    pub arguments: Table,
}

#[derive(Default)]
struct Confirms {
    enabled: bool,
    next_seq: u64,
    outstanding: BTreeSet<u64>,
    nacked: bool,
}

struct ChanState {
    id: u16,
    rpc_lock: Mutex<()>,
    rpc_tx: Mutex<Sender<Method>>,
    rpc_rx: Mutex<Receiver<Method>>,
    get_tx: Mutex<Sender<Option<Delivery>>>,
    get_rx: Mutex<Receiver<Option<Delivery>>>,
    consumers: Mutex<HashMap<String, Sender<Delivery>>>,
    closed: Mutex<Option<CamelError>>,
    confirms: Mutex<Confirms>,
    confirm_cv: Condvar,
    returns: Mutex<Vec<Returned>>,
}

impl ChanState {
    fn new(id: u16) -> ChanState {
        let (rpc_tx, rpc_rx) = mpsc::channel();
        let (get_tx, get_rx) = mpsc::channel();
        ChanState {
            id,
            rpc_lock: Mutex::new(()),
            rpc_tx: Mutex::new(rpc_tx),
            rpc_rx: Mutex::new(rpc_rx),
            get_tx: Mutex::new(get_tx),
            get_rx: Mutex::new(get_rx),
            consumers: Mutex::new(HashMap::new()),
            closed: Mutex::new(None),
            confirms: Mutex::new(Confirms::default()),
            confirm_cv: Condvar::new(),
            returns: Mutex::new(Vec::new()),
        }
    }
    fn mark_closed(&self, e: CamelError) {
        let mut c = self.closed.lock().unwrap();
        if c.is_none() {
            *c = Some(e);
        }
        drop(c);
        self.consumers.lock().unwrap().clear();
        let _g = self.confirms.lock().unwrap();
        self.confirm_cv.notify_all();
    }
}

struct Inner {
    writer: Mutex<NetStream>,
    frame_max: u32,
    channel_max: u16,
    heartbeat: u16,
    rpc_timeout: Duration,
    channels: Mutex<HashMap<u16, Arc<ChanState>>>,
    next_channel: Mutex<u16>,
    closed: Mutex<Option<CamelError>>,
    open: AtomicBool,
    close_ok: Mutex<Option<Sender<()>>>,
    stop: Arc<StopFlag>,
    threads: Mutex<Vec<JoinHandle<()>>>,
    server_properties: Table,
    publish_seq: AtomicU64,
}

impl Inner {
    fn write(&self, buf: &[u8]) -> Result<()> {
        if !self.open.load(Ordering::SeqCst) {
            return Err(self.closed_error());
        }
        let mut w = self.writer.lock().unwrap();
        w.write_all(buf).and_then(|_| w.flush()).map_err(|e| io_error(e, "AMQP connection"))
    }
    fn send_method(&self, channel: u16, m: &Method) -> Result<()> {
        let mut out = Vec::new();
        put_frame(&mut out, FRAME_METHOD, channel, &m.encode());
        self.write(&out)
    }
    fn closed_error(&self) -> CamelError {
        self.closed.lock().unwrap().clone().unwrap_or_else(|| CamelError::new(kinds::IO, "AMQP connection is closed"))
    }
    fn mark_closed(&self, e: CamelError) {
        {
            let mut c = self.closed.lock().unwrap();
            if c.is_none() {
                *c = Some(e.clone());
            }
        }
        self.open.store(false, Ordering::SeqCst);
        let chans: Vec<Arc<ChanState>> = self.channels.lock().unwrap().values().cloned().collect();
        for c in chans {
            c.mark_closed(e.clone());
        }
        self.stop.stop();
    }
}

fn close_error(m: &Method, what: &str) -> CamelError {
    CamelError::new(
        SHUTDOWN_SIGNAL,
        format!("{} error; protocol method: #method<{}.close>(reply-code={}, reply-text={}, class-id={}, method-id={})", what, what, m.num(0), m.str(1), m.num(2), m.num(3)),
    )
}

/// An open AMQP connection. Dropping it closes the connection.
pub struct Connection {
    inner: Arc<Inner>,
}

impl Connection {
    /// Connect, authenticate (PLAIN) and open the virtual host.
    pub fn open(cfg: &AmqpConfig) -> Result<Connection> {
        let tcp = connect(&cfg.host, cfg.port, cfg.connect_timeout)?;
        let what = format!("{}://{}:{}", if cfg.tls.is_some() { "amqps" } else { "amqp" }, cfg.host, cfg.port);
        let stream = match &cfg.tls {
            Some(t) => {
                let _ = tcp.set_read_timeout(Some(cfg.connect_timeout.max(Duration::from_millis(100))));
                t.handshake_client(tcp, &cfg.host)?
            }
            None => NetStream::plain(tcp),
        };
        let ioe = |e: std::io::Error| io_error(e, &what);
        stream.set_read_timeout(Some(cfg.connect_timeout.max(Duration::from_millis(100)))).map_err(ioe)?;
        let mut reader = BufReader::new(stream.try_clone().map_err(ioe)?);
        let mut w = stream.try_clone().map_err(ioe)?;
        w.write_all(PROTOCOL_HEADER).map_err(ioe)?;
        let read_method = |reader: &mut BufReader<NetStream>| -> Result<Method> {
            loop {
                let f = read_frame(reader, 0).map_err(|e| {
                    if e.kind() == std::io::ErrorKind::UnexpectedEof {
                        CamelError::new(AUTHENTICATION_FAILURE, format!("Connection closed by broker during handshake (authentication failure?): {}", what))
                    } else {
                        io_error(e, &what)
                    }
                })?;
                match f.kind {
                    FRAME_METHOD => return Method::decode(&f.payload).map_err(|e| io_error(e, &what)),
                    FRAME_HEARTBEAT => continue,
                    k => return Err(CamelError::io(format!("Unexpected frame type {} during handshake", k))),
                }
            }
        };
        let start = read_method(&mut reader)?;
        if !start.is(m::CONNECTION_START) {
            return Err(CamelError::io(format!("Expected connection.start, got {}", start.name())));
        }
        let mechanisms = start.str(3);
        if !mechanisms.split_whitespace().any(|x| x == "PLAIN") {
            return Err(CamelError::new(AUTHENTICATION_FAILURE, format!("Broker does not support PLAIN authentication (offers {})", mechanisms)));
        }
        let server_properties = start.table(2);
        let mut caps = Table::new();
        for c in ["publisher_confirms", "consumer_cancel_notify", "basic.nack", "exchange_exchange_bindings", "authentication_failure_close"] {
            caps.insert(c.into(), Value::Bool(true));
        }
        let mut props = Table::new();
        props.insert("product".into(), Value::from("rust-camel"));
        props.insert("version".into(), Value::from(env!("CARGO_PKG_VERSION")));
        props.insert("platform".into(), Value::from("Rust"));
        props.insert("capabilities".into(), Value::Object(caps));
        if let Some(n) = &cfg.connection_name {
            props.insert("connection_name".into(), Value::from(n.as_str()));
        }
        let mut response = vec![0u8];
        response.extend_from_slice(cfg.username.as_bytes());
        response.push(0);
        response.extend_from_slice(cfg.password.as_bytes());
        write_method(&mut w, 0, &Method::new(m::CONNECTION_START_OK, vec![Arg::Table(props), Arg::ShortStr("PLAIN".into()), Arg::LongStr(response), Arg::ShortStr("en_US".into())]))
            .map_err(ioe)?;
        let tune = read_method(&mut reader)?;
        if tune.is(m::CONNECTION_CLOSE) {
            let _ = write_method(&mut w, 0, &Method::new(m::CONNECTION_CLOSE_OK, vec![]));
            let kind = if tune.num(0) == reply::ACCESS_REFUSED as u64 { AUTHENTICATION_FAILURE } else { SHUTDOWN_SIGNAL };
            return Err(CamelError::new(kind, format!("{} (reply-code={})", tune.str(1), tune.num(0))));
        }
        if !tune.is(m::CONNECTION_TUNE) {
            return Err(CamelError::io(format!("Expected connection.tune, got {}", tune.name())));
        }
        let nego = |client: u64, server: u64| -> u64 {
            if client == 0 || server == 0 {
                client.max(server)
            } else {
                client.min(server)
            }
        };
        let channel_max = nego(cfg.channel_max as u64, tune.num(0)) as u16;
        let frame_max = nego(cfg.frame_max as u64, tune.num(1)) as u32;
        let heartbeat = nego(cfg.heartbeat as u64, tune.num(2)) as u16;
        write_method(&mut w, 0, &Method::new(m::CONNECTION_TUNE_OK, vec![Arg::Short(channel_max), Arg::Long(frame_max), Arg::Short(heartbeat)])).map_err(ioe)?;
        write_method(&mut w, 0, &Method::new(m::CONNECTION_OPEN, vec![Arg::ShortStr(cfg.vhost.clone()), Arg::ShortStr(String::new()), Arg::Bit(false)])).map_err(ioe)?;
        let ok = read_method(&mut reader)?;
        if ok.is(m::CONNECTION_CLOSE) {
            let _ = write_method(&mut w, 0, &Method::new(m::CONNECTION_CLOSE_OK, vec![]));
            return Err(close_error(&ok, "connection"));
        }
        if !ok.is(m::CONNECTION_OPEN_OK) {
            return Err(CamelError::io(format!("Expected connection.open-ok, got {}", ok.name())));
        }
        // reader: detect a dead peer after two missed heartbeats
        let read_timeout = if heartbeat > 0 { Some(Duration::from_secs(heartbeat as u64 * 2) + Duration::from_millis(500)) } else { None };
        stream.set_read_timeout(read_timeout).map_err(ioe)?;
        let inner = Arc::new(Inner {
            writer: Mutex::new(w),
            frame_max,
            channel_max,
            heartbeat,
            rpc_timeout: cfg.rpc_timeout,
            channels: Mutex::new(HashMap::new()),
            next_channel: Mutex::new(1),
            closed: Mutex::new(None),
            open: AtomicBool::new(true),
            close_ok: Mutex::new(None),
            stop: StopFlag::new(),
            threads: Mutex::new(Vec::new()),
            server_properties,
            publish_seq: AtomicU64::new(0),
        });
        let i2 = inner.clone();
        let reader_thread = std::thread::Builder::new()
            .name(format!("amqp-reader-{}:{}", cfg.host, cfg.port))
            .spawn(move || reader_loop(i2, reader))
            .map_err(|e| CamelError::io(e.to_string()))?;
        inner.threads.lock().unwrap().push(reader_thread);
        if heartbeat > 0 {
            let i3 = inner.clone();
            let interval = Duration::from_millis(heartbeat as u64 * 500);
            let hb = std::thread::Builder::new()
                .name("amqp-heartbeat".into())
                .spawn(move || {
                    let stop = i3.stop.clone();
                    while !stop.sleep(interval) {
                        let mut w = i3.writer.lock().unwrap();
                        if write_heartbeat(&mut *w).is_err() {
                            break;
                        }
                    }
                })
                .map_err(|e| CamelError::io(e.to_string()))?;
            inner.threads.lock().unwrap().push(hb);
        }
        Ok(Connection { inner })
    }

    /// Whether the connection is still open.
    pub fn is_open(&self) -> bool {
        self.inner.open.load(Ordering::SeqCst)
    }
    /// Why the connection was closed (if it was).
    pub fn close_reason(&self) -> Option<CamelError> {
        self.inner.closed.lock().unwrap().clone()
    }
    /// Negotiated heartbeat (seconds).
    pub fn heartbeat(&self) -> u16 {
        self.inner.heartbeat
    }
    /// Negotiated frame-max.
    pub fn frame_max(&self) -> u32 {
        self.inner.frame_max
    }
    /// The broker's `connection.start` server properties.
    pub fn server_properties(&self) -> &Table {
        &self.inner.server_properties
    }

    /// Open a new channel.
    pub fn channel(&self) -> Result<Channel> {
        let id = {
            let mut next = self.inner.next_channel.lock().unwrap();
            let chans = self.inner.channels.lock().unwrap();
            let max = if self.inner.channel_max == 0 { u16::MAX } else { self.inner.channel_max };
            let mut id = *next;
            let mut tries = 0u32;
            while chans.contains_key(&id) || id == 0 || id > max {
                id = if id >= max { 1 } else { id + 1 };
                tries += 1;
                if tries > max as u32 + 1 {
                    return Err(CamelError::io("No free AMQP channel ids"));
                }
            }
            *next = if id >= max { 1 } else { id + 1 };
            id
        };
        let state = Arc::new(ChanState::new(id));
        self.inner.channels.lock().unwrap().insert(id, state.clone());
        let ch = Channel { conn: self.inner.clone(), state };
        ch.rpc(Method::new(m::CHANNEL_OPEN, vec![Arg::ShortStr(String::new())]), m::CHANNEL_OPEN_OK)?;
        Ok(ch)
    }

    /// Close the connection gracefully (`connection.close`), then the socket.
    pub fn close(&self) {
        if self.inner.open.load(Ordering::SeqCst) {
            let (tx, rx) = mpsc::channel();
            *self.inner.close_ok.lock().unwrap() = Some(tx);
            if self.inner.send_method(0, &Method::new(m::CONNECTION_CLOSE, vec![Arg::Short(reply::SUCCESS), Arg::ShortStr("OK".into()), Arg::Short(0), Arg::Short(0)])).is_ok() {
                let _ = rx.recv_timeout(Duration::from_secs(2));
            }
        }
        self.inner.mark_closed(CamelError::new(SHUTDOWN_SIGNAL, "connection closed by application"));
        let _ = self.inner.writer.lock().unwrap().shutdown(Shutdown::Both);
        let me = std::thread::current().id();
        let threads: Vec<JoinHandle<()>> = std::mem::take(&mut *self.inner.threads.lock().unwrap());
        for t in threads {
            if t.thread().id() != me {
                let _ = t.join();
            }
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.close();
    }
}

struct PendingContent {
    method: Method,
    props: BasicProperties,
    size: u64,
    body: Vec<u8>,
    header_seen: bool,
}

fn reader_loop(inner: Arc<Inner>, mut reader: BufReader<NetStream>) {
    let mut pending: HashMap<u16, PendingContent> = HashMap::new();
    let reason = loop {
        let f = match read_frame(&mut reader, inner.frame_max) {
            Ok(f) => f,
            Err(e) => {
                let msg = match e.kind() {
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => "Missed heartbeats from the broker; connection is dead".to_string(),
                    _ => format!("AMQP connection lost: {}", e),
                };
                break CamelError::io(msg);
            }
        };
        match f.kind {
            FRAME_HEARTBEAT => continue,
            FRAME_METHOD => {
                let method = match Method::decode(&f.payload) {
                    Ok(m) => m,
                    Err(e) => break CamelError::io(format!("Invalid AMQP frame: {}", e)),
                };
                if f.channel == 0 {
                    if method.is(m::CONNECTION_CLOSE) {
                        let _ = inner.send_method(0, &Method::new(m::CONNECTION_CLOSE_OK, vec![]));
                        break close_error(&method, "connection");
                    }
                    if method.is(m::CONNECTION_CLOSE_OK) {
                        if let Some(tx) = inner.close_ok.lock().unwrap().take() {
                            let _ = tx.send(());
                        }
                        break CamelError::new(SHUTDOWN_SIGNAL, "connection closed by application");
                    }
                    continue;
                }
                let Some(ch) = inner.channels.lock().unwrap().get(&f.channel).cloned() else { continue };
                if method.is(m::BASIC_DELIVER) || method.is(m::BASIC_GET_OK) || method.is(m::BASIC_RETURN) {
                    pending.insert(f.channel, PendingContent { method, props: BasicProperties::default(), size: 0, body: Vec::new(), header_seen: false });
                    continue;
                }
                handle_channel_method(&inner, &ch, method);
            }
            FRAME_HEADER => {
                if let Some(p) = pending.get_mut(&f.channel) {
                    match decode_content_header(&f.payload) {
                        Ok((_, size, props)) => {
                            p.size = size;
                            p.props = props;
                            p.header_seen = true;
                        }
                        Err(e) => break CamelError::io(format!("Invalid content header: {}", e)),
                    }
                    if p.size == 0 {
                        let p = pending.remove(&f.channel).unwrap();
                        complete_content(&inner, f.channel, p);
                    }
                }
            }
            FRAME_BODY => {
                let done = match pending.get_mut(&f.channel) {
                    Some(p) if p.header_seen => {
                        p.body.extend_from_slice(&f.payload);
                        p.body.len() as u64 >= p.size
                    }
                    _ => false,
                };
                if done {
                    let p = pending.remove(&f.channel).unwrap();
                    complete_content(&inner, f.channel, p);
                }
            }
            _ => {}
        }
    };
    inner.mark_closed(reason);
}

fn complete_content(inner: &Arc<Inner>, channel: u16, p: PendingContent) {
    let Some(ch) = inner.channels.lock().unwrap().get(&channel).cloned() else { return };
    let m = &p.method;
    if m.is(m::BASIC_DELIVER) {
        let d = Delivery {
            consumer_tag: m.str(0),
            delivery_tag: m.num(1),
            redelivered: m.bit(2),
            exchange: m.str(3),
            routing_key: m.str(4),
            properties: p.props,
            body: p.body,
            message_count: 0,
        };
        let tx = ch.consumers.lock().unwrap().get(&d.consumer_tag).cloned();
        if let Some(tx) = tx {
            let _ = tx.send(d);
        }
    } else if m.is(m::BASIC_GET_OK) {
        let d = Delivery {
            consumer_tag: String::new(),
            delivery_tag: m.num(0),
            redelivered: m.bit(1),
            exchange: m.str(2),
            routing_key: m.str(3),
            properties: p.props,
            body: p.body,
            message_count: m.num(4) as u32,
        };
        let _ = ch.get_tx.lock().unwrap().send(Some(d));
    } else if m.is(m::BASIC_RETURN) {
        ch.returns.lock().unwrap().push(Returned {
            reply_code: m.num(0) as u16,
            reply_text: m.str(1),
            exchange: m.str(2),
            routing_key: m.str(3),
            properties: p.props,
            body: p.body,
        });
    }
}

fn handle_channel_method(inner: &Arc<Inner>, ch: &Arc<ChanState>, method: Method) {
    if method.is(m::BASIC_ACK) || method.is(m::BASIC_NACK) {
        let tag = method.num(0);
        let multiple = method.bit(1);
        let mut c = ch.confirms.lock().unwrap();
        if multiple {
            let keep = c.outstanding.split_off(&(tag + 1));
            c.outstanding = keep;
        } else {
            c.outstanding.remove(&tag);
        }
        if method.is(m::BASIC_NACK) {
            c.nacked = true;
        }
        ch.confirm_cv.notify_all();
        return;
    }
    if method.is(m::BASIC_GET_EMPTY) {
        let _ = ch.get_tx.lock().unwrap().send(None);
        return;
    }
    if method.is(m::BASIC_CANCEL) {
        // consumer cancelled by the broker (queue deleted)
        ch.consumers.lock().unwrap().remove(&method.str(0));
        let _ = inner.send_method(ch.id, &Method::new(m::BASIC_CANCEL_OK, vec![Arg::ShortStr(method.str(0))]));
        return;
    }
    if method.is(m::CHANNEL_FLOW) {
        let _ = inner.send_method(ch.id, &Method::new(m::CHANNEL_FLOW_OK, vec![Arg::Bit(method.bit(0))]));
        return;
    }
    if method.is(m::CHANNEL_CLOSE) {
        let _ = inner.send_method(ch.id, &Method::new(m::CHANNEL_CLOSE_OK, vec![]));
        ch.mark_closed(close_error(&method, "channel"));
        let _ = ch.rpc_tx.lock().unwrap().send(method);
        inner.channels.lock().unwrap().remove(&ch.id);
        return;
    }
    let _ = ch.rpc_tx.lock().unwrap().send(method);
}

/// A consumer subscription created by [`Channel::basic_consume`].
pub struct Subscription {
    pub consumer_tag: String,
    pub deliveries: Receiver<Delivery>,
}

/// An AMQP channel. Cheap to clone; all clones share the channel.
#[derive(Clone)]
pub struct Channel {
    conn: Arc<Inner>,
    state: Arc<ChanState>,
}

fn ss(s: &str) -> Arg {
    Arg::ShortStr(s.to_string())
}

impl Channel {
    /// The channel number.
    pub fn id(&self) -> u16 {
        self.state.id
    }
    /// Whether the channel (and its connection) is open.
    pub fn is_open(&self) -> bool {
        self.state.closed.lock().unwrap().is_none() && self.conn.open.load(Ordering::SeqCst)
    }
    /// Why the channel was closed.
    pub fn close_reason(&self) -> Option<CamelError> {
        self.state.closed.lock().unwrap().clone().or_else(|| self.conn.closed.lock().unwrap().clone())
    }
    fn check_open(&self) -> Result<()> {
        if let Some(e) = self.state.closed.lock().unwrap().clone() {
            return Err(e);
        }
        if !self.conn.open.load(Ordering::SeqCst) {
            return Err(self.conn.closed_error());
        }
        Ok(())
    }

    fn rpc(&self, method: Method, expect: (u16, u16)) -> Result<Method> {
        let _g = self.state.rpc_lock.lock().unwrap();
        self.check_open()?;
        let rx = self.state.rpc_rx.lock().unwrap();
        while rx.try_recv().is_ok() {}
        self.conn.send_method(self.state.id, &method)?;
        let deadline = Instant::now() + self.conn.rpc_timeout;
        loop {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(reply) if reply.is(expect) => return Ok(reply),
                Ok(reply) if reply.is(m::CHANNEL_CLOSE) => return Err(close_error(&reply, "channel")),
                Ok(_) => continue,
                Err(RecvTimeoutError::Timeout) => {
                    self.check_open()?;
                    if Instant::now() >= deadline {
                        return Err(CamelError::new(kinds::TIMEOUT, format!("Timeout waiting for reply to {}", method.name())));
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return Err(self.conn.closed_error()),
            }
        }
    }

    /// `exchange.declare`.
    pub fn exchange_declare(&self, name: &str, kind: &str, o: &ExchangeOptions) -> Result<()> {
        let m = Method::new(
            m::EXCHANGE_DECLARE,
            vec![Arg::Short(0), ss(name), ss(kind), Arg::Bit(o.passive), Arg::Bit(o.durable), Arg::Bit(o.auto_delete), Arg::Bit(o.internal), Arg::Bit(false), Arg::Table(o.arguments.clone())],
        );
        self.rpc(m, m::EXCHANGE_DECLARE_OK).map(|_| ())
    }
    /// `exchange.delete`.
    pub fn exchange_delete(&self, name: &str, if_unused: bool) -> Result<()> {
        self.rpc(Method::new(m::EXCHANGE_DELETE, vec![Arg::Short(0), ss(name), Arg::Bit(if_unused), Arg::Bit(false)]), m::EXCHANGE_DELETE_OK).map(|_| ())
    }
    /// `queue.declare`; an empty name asks the broker to generate one.
    pub fn queue_declare(&self, name: &str, o: &QueueOptions) -> Result<QueueInfo> {
        let m = Method::new(
            m::QUEUE_DECLARE,
            vec![Arg::Short(0), ss(name), Arg::Bit(o.passive), Arg::Bit(o.durable), Arg::Bit(o.exclusive), Arg::Bit(o.auto_delete), Arg::Bit(false), Arg::Table(o.arguments.clone())],
        );
        let r = self.rpc(m, m::QUEUE_DECLARE_OK)?;
        Ok(QueueInfo { name: r.str(0), message_count: r.num(1) as u32, consumer_count: r.num(2) as u32 })
    }
    /// `queue.bind`.
    pub fn queue_bind(&self, queue: &str, exchange: &str, routing_key: &str, args: &Table) -> Result<()> {
        self.rpc(Method::new(m::QUEUE_BIND, vec![Arg::Short(0), ss(queue), ss(exchange), ss(routing_key), Arg::Bit(false), Arg::Table(args.clone())]), m::QUEUE_BIND_OK)
            .map(|_| ())
    }
    /// `queue.unbind`.
    pub fn queue_unbind(&self, queue: &str, exchange: &str, routing_key: &str, args: &Table) -> Result<()> {
        self.rpc(Method::new(m::QUEUE_UNBIND, vec![Arg::Short(0), ss(queue), ss(exchange), ss(routing_key), Arg::Table(args.clone())]), m::QUEUE_UNBIND_OK).map(|_| ())
    }
    /// `queue.purge`; returns the number of purged messages.
    pub fn queue_purge(&self, queue: &str) -> Result<u32> {
        Ok(self.rpc(Method::new(m::QUEUE_PURGE, vec![Arg::Short(0), ss(queue), Arg::Bit(false)]), m::QUEUE_PURGE_OK)?.num(0) as u32)
    }
    /// `queue.delete`; returns the number of deleted messages.
    pub fn queue_delete(&self, queue: &str, if_unused: bool, if_empty: bool) -> Result<u32> {
        Ok(self.rpc(Method::new(m::QUEUE_DELETE, vec![Arg::Short(0), ss(queue), Arg::Bit(if_unused), Arg::Bit(if_empty), Arg::Bit(false)]), m::QUEUE_DELETE_OK)?.num(0) as u32)
    }
    /// `basic.qos` (prefetch count; `global` applies it to the whole channel).
    pub fn basic_qos(&self, prefetch_count: u16, global: bool) -> Result<()> {
        self.rpc(Method::new(m::BASIC_QOS, vec![Arg::Long(0), Arg::Short(prefetch_count), Arg::Bit(global)]), m::BASIC_QOS_OK).map(|_| ())
    }
    /// `basic.consume`. An empty tag generates one client-side.
    pub fn basic_consume(&self, queue: &str, tag: &str, no_ack: bool, exclusive: bool, args: &Table) -> Result<Subscription> {
        let tag = if tag.is_empty() { format!("amq.ctag-{}", crate::common::unique_id()) } else { tag.to_string() };
        let (tx, rx) = mpsc::channel();
        self.state.consumers.lock().unwrap().insert(tag.clone(), tx);
        let m = Method::new(m::BASIC_CONSUME, vec![Arg::Short(0), ss(queue), ss(&tag), Arg::Bit(false), Arg::Bit(no_ack), Arg::Bit(exclusive), Arg::Bit(false), Arg::Table(args.clone())]);
        if let Err(e) = self.rpc(m, m::BASIC_CONSUME_OK) {
            self.state.consumers.lock().unwrap().remove(&tag);
            return Err(e);
        }
        Ok(Subscription { consumer_tag: tag, deliveries: rx })
    }
    /// `basic.cancel`.
    pub fn basic_cancel(&self, tag: &str) -> Result<()> {
        let r = self.rpc(Method::new(m::BASIC_CANCEL, vec![ss(tag), Arg::Bit(false)]), m::BASIC_CANCEL_OK).map(|_| ());
        self.state.consumers.lock().unwrap().remove(tag);
        r
    }
    /// `basic.get`: fetch one message (None when the queue is empty).
    pub fn basic_get(&self, queue: &str, no_ack: bool) -> Result<Option<Delivery>> {
        let _g = self.state.rpc_lock.lock().unwrap();
        self.check_open()?;
        let rx = self.state.get_rx.lock().unwrap();
        while rx.try_recv().is_ok() {}
        self.conn.send_method(self.state.id, &Method::new(m::BASIC_GET, vec![Arg::Short(0), ss(queue), Arg::Bit(no_ack)]))?;
        let deadline = Instant::now() + self.conn.rpc_timeout;
        loop {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(d) => return Ok(d),
                Err(RecvTimeoutError::Timeout) => {
                    self.check_open()?;
                    if Instant::now() >= deadline {
                        return Err(CamelError::new(kinds::TIMEOUT, "Timeout waiting for basic.get reply"));
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return Err(self.conn.closed_error()),
            }
        }
    }
    /// `basic.publish`. Returns the publish sequence number in confirm mode (else 0).
    pub fn basic_publish(&self, exchange: &str, routing_key: &str, mandatory: bool, props: &BasicProperties, body: &[u8]) -> Result<u64> {
        self.check_open()?;
        let seq = {
            let mut c = self.state.confirms.lock().unwrap();
            if c.enabled {
                c.next_seq += 1;
                let s = c.next_seq;
                c.outstanding.insert(s);
                s
            } else {
                0
            }
        };
        let m = Method::new(m::BASIC_PUBLISH, vec![Arg::Short(0), ss(exchange), ss(routing_key), Arg::Bit(mandatory), Arg::Bit(false)]);
        let mut out = Vec::with_capacity(body.len() + 128);
        put_content(&mut out, self.state.id, &m, props, body, self.conn.frame_max);
        self.conn.publish_seq.fetch_add(1, Ordering::Relaxed);
        self.conn.write(&out)?;
        Ok(seq)
    }
    /// `basic.ack`.
    pub fn basic_ack(&self, delivery_tag: u64, multiple: bool) -> Result<()> {
        self.check_open()?;
        self.conn.send_method(self.state.id, &Method::new(m::BASIC_ACK, vec![Arg::LongLong(delivery_tag), Arg::Bit(multiple)]))
    }
    /// `basic.nack`.
    pub fn basic_nack(&self, delivery_tag: u64, multiple: bool, requeue: bool) -> Result<()> {
        self.check_open()?;
        self.conn.send_method(self.state.id, &Method::new(m::BASIC_NACK, vec![Arg::LongLong(delivery_tag), Arg::Bit(multiple), Arg::Bit(requeue)]))
    }
    /// `basic.reject`.
    pub fn basic_reject(&self, delivery_tag: u64, requeue: bool) -> Result<()> {
        self.check_open()?;
        self.conn.send_method(self.state.id, &Method::new(m::BASIC_REJECT, vec![Arg::LongLong(delivery_tag), Arg::Bit(requeue)]))
    }
    /// `basic.recover`: redeliver all unacknowledged messages of this channel.
    pub fn basic_recover(&self, requeue: bool) -> Result<()> {
        self.rpc(Method::new(m::BASIC_RECOVER, vec![Arg::Bit(requeue)]), m::BASIC_RECOVER_OK).map(|_| ())
    }
    /// `confirm.select`: enable publisher confirms on this channel.
    pub fn confirm_select(&self) -> Result<()> {
        if self.state.confirms.lock().unwrap().enabled {
            return Ok(());
        }
        self.rpc(Method::new(m::CONFIRM_SELECT, vec![Arg::Bit(false)]), m::CONFIRM_SELECT_OK)?;
        self.state.confirms.lock().unwrap().enabled = true;
        Ok(())
    }
    /// Wait until every published message was confirmed. Returns false if any was nacked.
    pub fn wait_for_confirms(&self, timeout: Duration) -> Result<bool> {
        let deadline = Instant::now() + timeout;
        let mut c = self.state.confirms.lock().unwrap();
        if !c.enabled {
            return Err(CamelError::illegal_state("Channel is not in confirm mode"));
        }
        while !c.outstanding.is_empty() {
            if let Some(e) = self.state.closed.lock().unwrap().clone() {
                return Err(e);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(CamelError::new(kinds::TIMEOUT, format!("Timeout waiting for {} publisher confirms", c.outstanding.len())));
            }
            c = self.state.confirm_cv.wait_timeout(c, (deadline - now).min(Duration::from_millis(100))).unwrap().0;
        }
        let ok = !c.nacked;
        c.nacked = false;
        Ok(ok)
    }
    /// Take the messages returned by the broker so far.
    pub fn take_returned(&self) -> Vec<Returned> {
        std::mem::take(&mut *self.state.returns.lock().unwrap())
    }
    /// Close the channel (`channel.close`).
    pub fn close(&self) -> Result<()> {
        if self.check_open().is_err() {
            self.conn.channels.lock().unwrap().remove(&self.state.id);
            return Ok(());
        }
        let r = self.rpc(Method::new(m::CHANNEL_CLOSE, vec![Arg::Short(reply::SUCCESS), ss("OK"), Arg::Short(0), Arg::Short(0)]), m::CHANNEL_CLOSE_OK);
        self.state.mark_closed(CamelError::new(SHUTDOWN_SIGNAL, "channel closed by application"));
        self.conn.channels.lock().unwrap().remove(&self.state.id);
        r.map(|_| ())
    }
}

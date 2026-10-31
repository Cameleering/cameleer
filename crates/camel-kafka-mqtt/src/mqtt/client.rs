//! A blocking MQTT 3.1.1 / 5.0 client: CONNECT with credentials and will, keep-alive
//! pings, QoS 0/1/2 publish and receive flows, (un)subscribe, automatic reconnect with
//! re-subscription.
//!
//! Received messages are handed to a dispatcher thread that calls the message handler;
//! QoS 1 messages are acknowledged (PUBACK) and QoS 2 messages are received (PUBREC)
//! after the handler returns, like Eclipse Paho.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, ErrorKind, Write};
use camel_tls::camel_support::EndpointTls;
use camel_tls::net::NetStream;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use camel_core::error::{CamelError, Result};

use super::packet::*;
use crate::net::{connect, io_error};

/// Exception kind for MQTT failures.
pub const MQTT_EXCEPTION: &str = "MqttException";

/// The MQTT protocol version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MqttVersion {
    /// MQTT 3.1.1 (protocol level 4).
    V311,
    /// MQTT 5.0 (protocol level 5).
    V5,
}

impl MqttVersion {
    pub fn is_v5(&self) -> bool {
        *self == MqttVersion::V5
    }
}

/// Connection options.
#[derive(Clone, Debug)]
pub struct MqttOptions {
    pub host: String,
    pub port: u16,
    pub client_id: String,
    pub version: MqttVersion,
    /// Clean session (3.1.1) / clean start (5.0).
    pub clean_session: bool,
    /// Keep-alive interval (zero disables pings).
    pub keep_alive: Duration,
    pub username: Option<String>,
    pub password: Option<String>,
    pub will: Option<Will>,
    pub connect_timeout: Duration,
    /// Time to wait for acknowledgements.
    pub ack_timeout: Duration,
    pub auto_reconnect: bool,
    pub max_reconnect_delay: Duration,
    /// MQTT 5 session expiry interval in seconds (None = not sent).
    pub session_expiry_interval: Option<u32>,
    /// TLS (`ssl://` broker urls).
    pub tls: Option<EndpointTls>,
}

impl MqttOptions {
    /// Options with defaults (clean session, 60s keep-alive, auto reconnect).
    pub fn new(host: &str, port: u16, client_id: &str) -> MqttOptions {
        MqttOptions {
            host: host.to_string(),
            port,
            client_id: client_id.to_string(),
            version: MqttVersion::V311,
            clean_session: true,
            keep_alive: Duration::from_secs(60),
            username: None,
            password: None,
            will: None,
            connect_timeout: Duration::from_secs(30),
            ack_timeout: Duration::from_secs(30),
            auto_reconnect: true,
            max_reconnect_delay: Duration::from_secs(128),
            session_expiry_interval: None,
            tls: None,
        }
    }
}

/// A received message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MqttMessage {
    pub topic: String,
    pub payload: Vec<u8>,
    pub qos: u8,
    pub retain: bool,
    pub dup: bool,
    pub pkid: u16,
    pub props: Properties,
}

/// Callback for received messages.
pub type MessageHandler = Arc<dyn Fn(MqttMessage) + Send + Sync>;

struct Connection {
    writer: Mutex<NetStream>,
    generation: u64,
    last_send: Mutex<Instant>,
}

impl Connection {
    fn send(&self, p: &Packet, v5: bool) -> std::io::Result<()> {
        let bytes = p.encode(v5);
        let mut w = self.writer.lock().unwrap();
        w.write_all(&bytes)?;
        w.flush()?;
        *self.last_send.lock().unwrap() = Instant::now();
        Ok(())
    }
    fn shutdown(&self) {
        let _ = self.writer.lock().unwrap().shutdown(std::net::Shutdown::Both);
    }
}

struct Inner {
    opts: MqttOptions,
    conn: Mutex<Option<Arc<Connection>>>,
    conn_cond: Condvar,
    pending: Mutex<HashMap<u16, Sender<Packet>>>,
    next_pkid: AtomicU16,
    subscriptions: Mutex<Vec<(String, u8)>>,
    stopped: AtomicBool,
    generation: AtomicU64,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl Inner {
    fn v5(&self) -> bool {
        self.opts.version.is_v5()
    }
    fn current(&self) -> Option<Arc<Connection>> {
        self.conn.lock().unwrap().clone()
    }
    fn pkid(&self) -> u16 {
        loop {
            let id = self.next_pkid.fetch_add(1, Ordering::Relaxed);
            if id != 0 && !self.pending.lock().unwrap().contains_key(&id) {
                return id;
            }
        }
    }
    /// Wait for a connection (during a reconnect) up to the connect timeout.
    fn wait_connection(&self) -> Result<Arc<Connection>> {
        let deadline = Instant::now() + self.opts.connect_timeout.max(Duration::from_millis(1));
        let mut g = self.conn.lock().unwrap();
        loop {
            if self.stopped.load(Ordering::SeqCst) {
                return Err(CamelError::new(MQTT_EXCEPTION, "MQTT client is disconnected"));
            }
            if let Some(c) = g.as_ref() {
                return Ok(c.clone());
            }
            let now = Instant::now();
            if now >= deadline || !self.opts.auto_reconnect {
                return Err(CamelError::new(MQTT_EXCEPTION, format!("Client is not connected to tcp://{}:{}", self.opts.host, self.opts.port)));
            }
            g = self.conn_cond.wait_timeout(g, deadline - now).unwrap().0;
        }
    }
    fn set_connection(&self, c: Option<Arc<Connection>>) {
        *self.conn.lock().unwrap() = c;
        self.conn_cond.notify_all();
    }
}

/// An MQTT client handle. Dropping it disconnects.
pub struct MqttClient {
    inner: Arc<Inner>,
}

type Dispatch = (MqttMessage, Option<(Packet, u64)>);

fn connack_error(code: u8, v5: bool) -> CamelError {
    let reason = match (v5, code) {
        (false, 1) => "unacceptable protocol version",
        (false, 2) => "identifier rejected",
        (false, 3) => "server unavailable",
        (false, 4) => "bad user name or password",
        (false, 5) => "not authorized",
        (true, 0x84) => "unsupported protocol version",
        (true, 0x85) => "client identifier not valid",
        (true, 0x86) => "bad user name or password",
        (true, 0x87) => "not authorized",
        (true, 0x88) => "server unavailable",
        _ => "connection refused",
    };
    let kind = if matches!((v5, code), (false, 4) | (false, 5) | (true, 0x86) | (true, 0x87)) { "MqttSecurityException" } else { MQTT_EXCEPTION };
    CamelError::new(kind, format!("MQTT connection refused: {} (reason code {})", reason, code))
}

fn establish(opts: &MqttOptions) -> Result<(NetStream, BufReader<NetStream>, bool)> {
    let what = format!("{}://{}:{}", if opts.tls.is_some() { "ssl" } else { "tcp" }, opts.host, opts.port);
    let tcp = connect(&opts.host, opts.port, opts.connect_timeout)?;
    let _ = tcp.set_write_timeout(Some(Duration::from_secs(10)));
    let stream = match &opts.tls {
        Some(t) => t.handshake_client(tcp, &opts.host)?,
        None => NetStream::plain(tcp),
    };
    let v5 = opts.version.is_v5();
    let mut props = Properties::new();
    if v5 {
        if let Some(e) = opts.session_expiry_interval {
            props.push(props::SESSION_EXPIRY_INTERVAL, PropValue::U32(e));
        }
    }
    let connect_packet = Packet::Connect(Connect {
        level: if v5 { 5 } else { 4 },
        client_id: opts.client_id.clone(),
        clean_session: opts.clean_session,
        keep_alive: opts.keep_alive.as_secs().min(65535) as u16,
        username: opts.username.clone(),
        password: opts.password.as_ref().map(|p| p.as_bytes().to_vec()),
        will: opts.will.clone(),
        props,
    });
    (&stream).write_all(&connect_packet.encode(v5)).map_err(|e| io_error(e, &what))?;
    let t = if opts.connect_timeout.is_zero() { Duration::from_secs(30) } else { opts.connect_timeout };
    stream.set_read_timeout(Some(t)).map_err(|e| io_error(e, &what))?;
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| io_error(e, &what))?);
    match read_packet(&mut reader, v5, 1 << 20).map_err(|e| io_error(e, &what))? {
        Some(Packet::ConnAck { session_present, code, .. }) => {
            if code != 0 {
                let _ = stream.shutdown(std::net::Shutdown::Both);
                return Err(connack_error(code, v5));
            }
            Ok((stream, reader, session_present))
        }
        Some(other) => Err(CamelError::new(MQTT_EXCEPTION, format!("Expected CONNACK from {} but got {:?}", what, other))),
        None => Err(CamelError::new(MQTT_EXCEPTION, format!("Connection closed by {} before CONNACK", what))),
    }
}

impl MqttClient {
    /// Connect (synchronously; fails when the first connection fails) and start the I/O
    /// threads. `handler` receives messages of subscribed topics.
    pub fn connect(opts: MqttOptions, handler: Option<MessageHandler>) -> Result<MqttClient> {
        let (stream, reader, _) = establish(&opts)?;
        let inner = Arc::new(Inner {
            opts,
            conn: Mutex::new(None),
            conn_cond: Condvar::new(),
            pending: Mutex::new(HashMap::new()),
            next_pkid: AtomicU16::new(1),
            subscriptions: Mutex::new(Vec::new()),
            stopped: AtomicBool::new(false),
            generation: AtomicU64::new(1),
            threads: Mutex::new(Vec::new()),
        });
        let conn = Arc::new(Connection {
            writer: Mutex::new(stream.try_clone().map_err(|e| CamelError::io(e.to_string()))?),
            generation: 1,
            last_send: Mutex::new(Instant::now()),
        });
        inner.set_connection(Some(conn.clone()));
        let (tx, rx) = mpsc::channel::<Dispatch>();
        let i2 = inner.clone();
        let io = std::thread::Builder::new()
            .name(format!("mqtt-io-{}", inner.opts.client_id))
            .spawn(move || io_loop(i2, stream, reader, conn, tx))
            .map_err(|e| CamelError::io(e.to_string()))?;
        let i3 = inner.clone();
        let disp = std::thread::Builder::new()
            .name(format!("mqtt-dispatch-{}", inner.opts.client_id))
            .spawn(move || dispatch_loop(i3, rx, handler))
            .map_err(|e| CamelError::io(e.to_string()))?;
        inner.threads.lock().unwrap().extend([io, disp]);
        Ok(MqttClient { inner })
    }

    /// Whether the client is currently connected.
    pub fn is_connected(&self) -> bool {
        self.inner.current().is_some()
    }

    /// The client options.
    pub fn options(&self) -> &MqttOptions {
        &self.inner.opts
    }

    fn request(&self, build: impl Fn(u16) -> Packet, what: &str) -> Result<Packet> {
        let conn = self.inner.wait_connection()?;
        let pkid = self.inner.pkid();
        let (tx, rx) = mpsc::channel();
        self.inner.pending.lock().unwrap().insert(pkid, tx);
        if let Err(e) = conn.send(&build(pkid), self.inner.v5()) {
            self.inner.pending.lock().unwrap().remove(&pkid);
            conn.shutdown();
            return Err(io_error(e, what));
        }
        let r = rx.recv_timeout(self.inner.opts.ack_timeout);
        self.inner.pending.lock().unwrap().remove(&pkid);
        match r {
            Ok(p) => Ok(p),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(CamelError::new(MQTT_EXCEPTION, format!("Timed out waiting for the acknowledgement of {}", what))),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(CamelError::new(MQTT_EXCEPTION, format!("Connection lost before the acknowledgement of {}", what))),
        }
    }

    /// Publish a message; for QoS 1/2 waits until the flow completes (PUBACK / PUBCOMP).
    pub fn publish(&self, topic: &str, payload: &[u8], qos: u8, retain: bool) -> Result<()> {
        self.publish_with(topic, payload, qos, retain, Properties::new())
    }

    /// Publish with MQTT 5 properties (ignored for 3.1.1).
    pub fn publish_with(&self, topic: &str, payload: &[u8], qos: u8, retain: bool, props: Properties) -> Result<()> {
        if !valid_topic(topic) {
            return Err(CamelError::illegal_argument(format!("Invalid MQTT topic name for publishing: '{}'", topic)));
        }
        if qos > 2 {
            return Err(CamelError::illegal_argument(format!("Invalid MQTT QoS: {}", qos)));
        }
        let what = format!("PUBLISH to {}", topic);
        if qos == 0 {
            let conn = self.inner.wait_connection()?;
            let p = Packet::Publish(Publish { dup: false, qos: 0, retain, topic: topic.to_string(), pkid: 0, payload: payload.to_vec(), props });
            return conn.send(&p, self.inner.v5()).map_err(|e| {
                conn.shutdown();
                io_error(e, &what)
            });
        }
        let resp = self.request(
            |pkid| Packet::Publish(Publish { dup: false, qos, retain, topic: topic.to_string(), pkid, payload: payload.to_vec(), props: props.clone() }),
            &what,
        )?;
        let reason = match resp {
            Packet::PubAck { reason, .. } | Packet::PubComp { reason, .. } | Packet::PubRec { reason, .. } => reason,
            other => return Err(CamelError::new(MQTT_EXCEPTION, format!("Unexpected acknowledgement {:?}", other))),
        };
        if reason >= 0x80 {
            return Err(CamelError::new(MQTT_EXCEPTION, format!("{} rejected by the broker (reason code 0x{:02x})", what, reason)));
        }
        Ok(())
    }

    /// Subscribe to topic filters; returns the granted QoS per filter. A rejected filter
    /// (0x80 or higher) fails. Subscriptions are restored after a reconnect.
    pub fn subscribe(&self, filters: &[(String, u8)]) -> Result<Vec<u8>> {
        for (f, _) in filters {
            if !valid_filter(f) {
                return Err(CamelError::illegal_argument(format!("Invalid MQTT topic filter: '{}'", f)));
            }
        }
        let resp = self.request(|pkid| Packet::Subscribe { pkid, filters: filters.to_vec(), props: Properties::new() }, "SUBSCRIBE")?;
        let codes = match resp {
            Packet::SubAck { codes, .. } => codes,
            other => return Err(CamelError::new(MQTT_EXCEPTION, format!("Unexpected response to SUBSCRIBE: {:?}", other))),
        };
        if let Some((i, c)) = codes.iter().enumerate().find(|(_, c)| **c >= 0x80) {
            return Err(CamelError::new(MQTT_EXCEPTION, format!("Subscription to '{}' rejected (reason code 0x{:02x})", filters.get(i).map(|f| f.0.as_str()).unwrap_or(""), c)));
        }
        let mut subs = self.inner.subscriptions.lock().unwrap();
        for f in filters {
            subs.retain(|(x, _)| *x != f.0);
            subs.push(f.clone());
        }
        Ok(codes)
    }

    /// Unsubscribe from topic filters.
    pub fn unsubscribe(&self, filters: &[String]) -> Result<()> {
        self.inner.subscriptions.lock().unwrap().retain(|(f, _)| !filters.contains(f));
        let resp = self.request(|pkid| Packet::Unsubscribe { pkid, filters: filters.to_vec(), props: Properties::new() }, "UNSUBSCRIBE")?;
        match resp {
            Packet::UnsubAck { .. } => Ok(()),
            other => Err(CamelError::new(MQTT_EXCEPTION, format!("Unexpected response to UNSUBSCRIBE: {:?}", other))),
        }
    }

    /// Send DISCONNECT, close the connection and join the I/O threads.
    pub fn disconnect(&self) {
        if self.inner.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(c) = self.inner.current() {
            let _ = c.send(&Packet::Disconnect { reason: 0, props: Properties::new() }, self.inner.v5());
            c.shutdown();
        }
        self.inner.set_connection(None);
        let me = std::thread::current().id();
        let threads: Vec<JoinHandle<()>> = std::mem::take(&mut *self.inner.threads.lock().unwrap());
        for t in threads {
            if t.thread().id() != me {
                let _ = t.join();
            }
        }
    }

    /// Drop the connection without DISCONNECT (simulates a network failure; the will
    /// message is published and the client reconnects when `auto_reconnect` is set).
    pub fn kill_connection(&self) {
        if let Some(c) = self.inner.current() {
            c.shutdown();
        }
    }
}

impl Drop for MqttClient {
    fn drop(&mut self) {
        self.disconnect();
    }
}

fn dispatch_loop(inner: Arc<Inner>, rx: Receiver<Dispatch>, handler: Option<MessageHandler>) {
    while let Ok((msg, ack)) = rx.recv() {
        if let Some(h) = &handler {
            h(msg);
        }
        if let Some((packet, generation)) = ack {
            if let Some(c) = inner.current() {
                if c.generation == generation {
                    let _ = c.send(&packet, inner.v5());
                }
            }
        }
    }
}

fn io_loop(inner: Arc<Inner>, mut stream: NetStream, mut reader: BufReader<NetStream>, mut conn: Arc<Connection>, tx: Sender<Dispatch>) {
    let v5 = inner.v5();
    let keep_alive = inner.opts.keep_alive;
    let poll = Duration::from_millis(100);
    let mut inbound_qos2: HashSet<u16> = HashSet::new();
    let mut ping_sent: Option<Instant> = None;
    loop {
        // wait for data, sending keep-alive pings while idle
        let mut lost = false;
        if reader.buffer().is_empty() {
            let _ = stream.set_read_timeout(Some(poll));
            match reader.fill_buf() {
                Ok([]) => lost = true,
                Ok(_) => {}
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => {
                    if inner.stopped.load(Ordering::SeqCst) {
                        break;
                    }
                    if !keep_alive.is_zero() {
                        if let Some(sent) = ping_sent {
                            if sent.elapsed() > keep_alive {
                                lost = true; // no PINGRESP
                            }
                        } else if conn.last_send.lock().unwrap().elapsed() >= keep_alive {
                            if conn.send(&Packet::PingReq, v5).is_err() {
                                lost = true;
                            }
                            ping_sent = Some(Instant::now());
                        }
                    }
                    if !lost {
                        continue;
                    }
                }
                Err(_) => lost = true,
            }
        }
        if !lost {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
            match read_packet(&mut reader, v5, 256 * 1024 * 1024) {
                Ok(Some(p)) => {
                    match p {
                        Packet::Publish(p) => {
                            let msg = MqttMessage { topic: p.topic, payload: p.payload, qos: p.qos, retain: p.retain, dup: p.dup, pkid: p.pkid, props: p.props };
                            match msg.qos {
                                0 => {
                                    let _ = tx.send((msg, None));
                                }
                                1 => {
                                    let ack = Packet::PubAck { pkid: msg.pkid, reason: 0, props: Properties::new() };
                                    let _ = tx.send((msg, Some((ack, conn.generation))));
                                }
                                _ => {
                                    let rec = Packet::PubRec { pkid: msg.pkid, reason: 0, props: Properties::new() };
                                    if inbound_qos2.insert(msg.pkid) {
                                        let _ = tx.send((msg, Some((rec, conn.generation))));
                                    } else {
                                        let _ = conn.send(&rec, v5); // duplicate: already delivered
                                    }
                                }
                            }
                        }
                        Packet::PubRel { pkid, .. } => {
                            inbound_qos2.remove(&pkid);
                            let _ = conn.send(&Packet::PubComp { pkid, reason: 0, props: Properties::new() }, v5);
                        }
                        Packet::PubRec { pkid, reason, props } => {
                            if reason >= 0x80 {
                                if let Some(s) = inner.pending.lock().unwrap().remove(&pkid) {
                                    let _ = s.send(Packet::PubRec { pkid, reason, props });
                                }
                            } else {
                                let _ = conn.send(&Packet::PubRel { pkid, reason: 0, props: Properties::new() }, v5);
                            }
                        }
                        p @ (Packet::PubAck { .. } | Packet::PubComp { .. } | Packet::SubAck { .. } | Packet::UnsubAck { .. }) => {
                            let pkid = match &p {
                                Packet::PubAck { pkid, .. } | Packet::PubComp { pkid, .. } | Packet::SubAck { pkid, .. } | Packet::UnsubAck { pkid, .. } => *pkid,
                                _ => 0,
                            };
                            if let Some(s) = inner.pending.lock().unwrap().remove(&pkid) {
                                let _ = s.send(p);
                            }
                        }
                        Packet::PingResp => ping_sent = None,
                        Packet::Disconnect { .. } => lost = true,
                        _ => {}
                    }
                }
                _ => lost = true,
            }
        }
        if !lost {
            continue;
        }
        // connection lost
        conn.shutdown();
        inner.set_connection(None);
        inner.pending.lock().unwrap().clear(); // waiting requests fail with "connection lost"
        ping_sent = None;
        if inner.stopped.load(Ordering::SeqCst) || !inner.opts.auto_reconnect {
            break;
        }
        camel_core::camel_warn!("mqtt", "MQTT client {} lost its connection to tcp://{}:{}; reconnecting", inner.opts.client_id, inner.opts.host, inner.opts.port);
        let mut delay = Duration::from_millis(50);
        let reconnected = loop {
            if inner.stopped.load(Ordering::SeqCst) {
                break None;
            }
            match establish(&inner.opts) {
                Ok(r) => break Some(r),
                Err(_) => {
                    let until = Instant::now() + delay;
                    while Instant::now() < until && !inner.stopped.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    delay = (delay * 2).min(inner.opts.max_reconnect_delay.max(Duration::from_millis(50)));
                }
            }
        };
        let Some((s, r, session_present)) = reconnected else { break };
        let Ok(w) = s.try_clone() else { break };
        let generation = inner.generation.fetch_add(1, Ordering::SeqCst) + 1;
        conn = Arc::new(Connection { writer: Mutex::new(w), generation, last_send: Mutex::new(Instant::now()) });
        stream = s;
        reader = r;
        inbound_qos2.clear();
        // restore subscriptions (SUBACKs are ignored)
        let subs = inner.subscriptions.lock().unwrap().clone();
        if !subs.is_empty() && (inner.opts.clean_session || !session_present) {
            let pkid = inner.pkid();
            let _ = conn.send(&Packet::Subscribe { pkid, filters: subs, props: Properties::new() }, v5);
        }
        inner.set_connection(Some(conn.clone()));
        camel_core::camel_info!("mqtt", "MQTT client {} reconnected", inner.opts.client_id);
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

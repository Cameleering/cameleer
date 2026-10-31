//! An embedded STOMP 1.2 broker for tests (an ActiveMQ-flavoured stand-in).
//!
//! * destinations `/queue/<name>` (point-to-point, competing consumers, round-robin),
//!   `/topic/<name>` (publish-subscribe, non-durable) and `/temp-queue/<name>` /
//!   `/temp-topic/<name>` (deleted when the creating connection closes); a destination
//!   without a prefix is a queue;
//! * subscriptions with `ack:auto`, `client` (cumulative) and `client-individual`,
//!   `activemq.prefetchSize` (unacked window, default unlimited), server-side JMS
//!   `selector`s ([`crate::selector`]);
//! * message priority ordering (`priority`, 0-9, default 4), expiration (`expires`,
//!   epoch millis), `persistent`, broker-assigned `message-id` and `timestamp`;
//! * `NACK`, `ABORT` of a transaction containing `ACK`s, `UNSUBSCRIBE` and disconnects
//!   redeliver unacknowledged messages with `redelivered:true`; after
//!   `max_redeliveries` (6) redeliveries a message moves to `/queue/ActiveMQ.DLQ`;
//! * transactions (`BEGIN`/`COMMIT`/`ABORT`) buffering `SEND`s and `ACK`/`NACK`s;
//! * receipts, `ERROR` frames (then the connection is closed), login/passcode checks,
//!   heart-beats.

use std::collections::{HashMap, VecDeque};
use std::io::{BufReader, Write};

use camel_tls::net::NetStream;
use camel_tls::TlsServerConfig;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camel_core::error::Result;
use camel_core::value::Value;

use crate::common::{now_millis, TcpServer};
use crate::selector::Selector;
use crate::stomp::client::AckMode;
use crate::stomp::frame::{read_frame, Frame};
pub use crate::stomp::normalize_destination;

/// The dead letter queue.
pub const DLQ: &str = "/queue/ActiveMQ.DLQ";

/// A message stored or delivered by the broker.
#[derive(Clone, Debug)]
pub struct StompMessage {
    pub id: String,
    pub destination: String,
    /// User and JMS headers (`correlation-id`, `reply-to`, `type`, custom properties...).
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub priority: u8,
    /// Expiration (epoch millis, 0 = never).
    pub expires: i64,
    pub timestamp: i64,
    pub persistent: bool,
    pub redelivered: bool,
    pub redelivery_count: u32,
}

impl StompMessage {
    pub fn header(&self, k: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }
    pub fn body_string(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    fn expired(&self, now: i64) -> bool {
        self.expires > 0 && self.expires <= now
    }
    /// Selector property lookup: JMS header names plus custom headers.
    fn property(&self, name: &str) -> Option<Value> {
        Some(match name {
            "JMSMessageID" => Value::from(self.id.as_str()),
            "JMSPriority" => Value::Int(self.priority as i64),
            "JMSTimestamp" => Value::Int(self.timestamp),
            "JMSExpiration" => Value::Int(self.expires),
            "JMSDeliveryMode" => Value::from(if self.persistent { "PERSISTENT" } else { "NON_PERSISTENT" }),
            "JMSCorrelationID" => return self.header("correlation-id").map(Value::from),
            "JMSType" => return self.header("type").map(Value::from),
            "JMSRedelivered" => Value::Bool(self.redelivered),
            "JMSXDeliveryCount" => Value::Int(self.redelivery_count as i64 + 1),
            other => return self.header(other).map(Value::from),
        })
    }
}

/// Headers the broker manages itself (not stored as user headers).
const RESERVED: &[&str] = &["destination", "transaction", "receipt", "content-length", "message-id", "subscription", "ack", "redelivered", "priority", "expires", "timestamp", "persistent"];

struct Sub {
    conn: u64,
    id: String,
    dest: String,
    ack: AckMode,
    selector: Option<Selector>,
    prefetch: usize,
    pending: Vec<(String, StompMessage)>,
}

enum TxOp {
    Send(StompMessage),
    Ack(String),
    Nack(String),
}

struct ConnSt {
    writer: Arc<Mutex<NetStream>>,
    txs: HashMap<String, Vec<TxOp>>,
    temps: Vec<String>,
}

#[derive(Default)]
struct State {
    queues: HashMap<String, VecDeque<StompMessage>>,
    subs: Vec<Sub>,
    rr: HashMap<String, usize>,
    conns: HashMap<u64, ConnSt>,
    ids: u64,
    acks: u64,
}

fn is_topic(dest: &str) -> bool {
    dest.starts_with("/topic/") || dest.starts_with("/temp-topic/")
}

struct Shared {
    state: Mutex<State>,
    users: Vec<(String, String)>,
    heartbeat: (u64, u64),
    max_redeliveries: u32,
    frames: AtomicU64,
    running: Arc<AtomicBool>,
}

fn write_frame(c: &ConnSt, f: &Frame) {
    let mut w = c.writer.lock().unwrap();
    let _ = w.write_all(&f.encode()).and_then(|_| w.flush());
}

impl State {
    fn deliver(&mut self, sub_idx: usize, msg: StompMessage) {
        self.acks += 1;
        let ack_id = format!("{}#{}", msg.id, self.acks);
        let sub = &self.subs[sub_idx];
        let mut f = Frame::new("MESSAGE")
            .header("subscription", sub.id.clone())
            .header("message-id", msg.id.clone())
            .header("destination", msg.destination.clone())
            .header("priority", msg.priority.to_string())
            .header("timestamp", msg.timestamp.to_string())
            .header("expires", msg.expires.to_string())
            .header("persistent", msg.persistent.to_string());
        if msg.redelivered {
            f = f.header("redelivered", "true");
        }
        if sub.ack != AckMode::Auto {
            f = f.header("ack", ack_id.clone());
        }
        for (k, v) in &msg.headers {
            f.headers.push((k.clone(), v.clone()));
        }
        f.body = msg.body.clone();
        if let Some(c) = self.conns.get(&sub.conn) {
            write_frame(c, &f);
        }
        if sub.ack != AckMode::Auto {
            self.subs[sub_idx].pending.push((ack_id, msg));
        }
    }

    fn selector_ok(sub: &Sub, msg: &StompMessage) -> bool {
        match &sub.selector {
            None => true,
            Some(s) => s.matches(&|n| msg.property(n)),
        }
    }

    /// Route a message: topics fan out, queues enqueue (by priority) and dispatch.
    fn route(&mut self, msg: StompMessage) {
        if msg.expired(now_millis()) {
            return;
        }
        if is_topic(&msg.destination) {
            let targets: Vec<usize> = (0..self.subs.len()).filter(|i| self.subs[*i].dest == msg.destination && Self::selector_ok(&self.subs[*i], &msg)).collect();
            for i in targets {
                self.deliver(i, msg.clone());
            }
            return;
        }
        let dest = msg.destination.clone();
        let q = self.queues.entry(dest.clone()).or_default();
        let pos = q.iter().position(|m| m.priority < msg.priority).unwrap_or(q.len());
        q.insert(pos, msg);
        self.dispatch(&dest);
    }

    fn requeue_front(&mut self, mut msg: StompMessage, max_redeliveries: u32, sub_idx: Option<usize>) {
        msg.redelivered = true;
        msg.redelivery_count += 1;
        if msg.redelivery_count > max_redeliveries {
            msg.headers.push(("dlqDeliveryFailureCause".into(), format!("Delivery failed {} times", msg.redelivery_count)));
            msg.headers.push(("originalDestination".into(), msg.destination.clone()));
            msg.destination = DLQ.to_string();
            msg.redelivered = false;
            self.route(msg);
            return;
        }
        if is_topic(&msg.destination) {
            if let Some(i) = sub_idx {
                self.deliver(i, msg);
            }
            return;
        }
        let dest = msg.destination.clone();
        let q = self.queues.entry(dest.clone()).or_default();
        // in front of the other messages of the same priority
        let pos = q.iter().position(|m| m.priority <= msg.priority).unwrap_or(q.len());
        q.insert(pos, msg);
        self.dispatch(&dest);
    }

    fn dispatch(&mut self, dest: &str) {
        if is_topic(dest) {
            return;
        }
        let now = now_millis();
        let mut i = 0;
        loop {
            let Some(q) = self.queues.get_mut(dest) else { return };
            if i >= q.len() {
                return;
            }
            if q[i].expired(now) {
                q.remove(i);
                continue;
            }
            let candidates: Vec<usize> = (0..self.subs.len()).filter(|s| self.subs[*s].dest == dest).collect();
            if candidates.is_empty() {
                return;
            }
            let start = *self.rr.get(dest).unwrap_or(&0);
            let n = candidates.len();
            let mut chosen = None;
            for k in 0..n {
                let si = candidates[(start + k) % n];
                let s = &self.subs[si];
                let has_room = s.ack == AckMode::Auto || s.prefetch == 0 || s.pending.len() < s.prefetch;
                if has_room && Self::selector_ok(s, &q[i]) {
                    chosen = Some((si, (start + k + 1) % n));
                    break;
                }
            }
            match chosen {
                Some((si, next)) => {
                    let msg = q.remove(i).unwrap();
                    self.rr.insert(dest.to_string(), next);
                    self.deliver(si, msg);
                }
                None => i += 1,
            }
        }
    }

    /// Find a pending delivery by ack id; returns (sub index, messages acked).
    fn take_acked(&mut self, conn: u64, ack_id: &str) -> Option<(usize, Vec<StompMessage>)> {
        for (si, s) in self.subs.iter_mut().enumerate() {
            if s.conn != conn {
                continue;
            }
            if let Some(pos) = s.pending.iter().position(|(a, _)| a == ack_id) {
                let taken: Vec<StompMessage> = if s.ack == AckMode::Client { s.pending.drain(..=pos).map(|(_, m)| m).collect() } else { vec![s.pending.remove(pos).1] };
                return Some((si, taken));
            }
        }
        None
    }
}

/// Builder for [`StompBroker`].
pub struct StompBrokerBuilder {
    users: Vec<(String, String)>,
    heartbeat: (u64, u64),
    max_redeliveries: u32,
    host: String,
    port: u16,
    tls: Option<Arc<TlsServerConfig>>,
}

impl StompBrokerBuilder {
    /// Serve STOMP over TLS (`ssl://` / `stomp+ssl://` broker urls) with this configuration.
    pub fn tls(mut self, cfg: TlsServerConfig) -> Self {
        self.tls = Some(Arc::new(cfg));
        self
    }
    /// Require a login (any number of users; none = anonymous access).
    pub fn user(mut self, login: &str, passcode: &str) -> Self {
        self.users.push((login.into(), passcode.into()));
        self
    }
    /// Heart-beats the broker offers: `(can send ms, wants to receive ms)`.
    pub fn heartbeat(mut self, send_ms: u64, receive_ms: u64) -> Self {
        self.heartbeat = (send_ms, receive_ms);
        self
    }
    /// Redeliveries before a message goes to the DLQ (default 6).
    pub fn max_redeliveries(mut self, n: u32) -> Self {
        self.max_redeliveries = n;
        self
    }
    pub fn bind(mut self, host: &str, port: u16) -> Self {
        self.host = host.into();
        self.port = port;
        self
    }
    pub fn start(self) -> Result<StompBroker> {
        let running = Arc::new(AtomicBool::new(true));
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            users: self.users,
            heartbeat: self.heartbeat,
            max_redeliveries: self.max_redeliveries,
            frames: AtomicU64::new(0),
            running,
        });
        let s2 = shared.clone();
        let tls = self.tls.clone();
        let server = TcpServer::bind(&self.host, self.port, "stomp-broker", move |stream, id| {
            let s = match &tls {
                Some(cfg) => {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                    match NetStream::server(stream, cfg) {
                        Ok(s) => {
                            let _ = s.set_read_timeout(None);
                            s
                        }
                        Err(_) => return,
                    }
                }
                None => NetStream::plain(stream),
            };
            serve(&s2, s, id)
        })?;
        Ok(StompBroker { server, shared })
    }
}

/// The embedded STOMP broker. Stopped on drop.
pub struct StompBroker {
    server: TcpServer,
    shared: Arc<Shared>,
}

impl StompBroker {
    /// Start on `127.0.0.1` with an ephemeral port, anonymous access.
    pub fn start() -> Result<StompBroker> {
        Self::builder().start()
    }
    pub fn builder() -> StompBrokerBuilder {
        StompBrokerBuilder { users: Vec::new(), heartbeat: (0, 0), max_redeliveries: 6, host: "127.0.0.1".into(), port: 0, tls: None }
    }
    pub fn port(&self) -> u16 {
        self.server.port()
    }
    /// `tcp://127.0.0.1:<port>`.
    pub fn url(&self) -> String {
        format!("tcp://127.0.0.1:{}", self.port())
    }
    pub fn stop(&self) {
        self.shared.running.store(false, Ordering::SeqCst);
        self.server.stop();
    }
    pub fn connection_count(&self) -> usize {
        self.server.connection_count()
    }
    /// Abruptly close every client connection.
    pub fn drop_connections(&self) {
        self.server.drop_connections();
    }
    /// Frames received from clients so far.
    pub fn frames_received(&self) -> u64 {
        self.shared.frames.load(Ordering::SeqCst)
    }
    /// Messages waiting in a queue (`name` or `/queue/name`).
    pub fn queue_depth(&self, name: &str) -> usize {
        self.shared.state.lock().unwrap().queues.get(&normalize_destination(name)).map(|q| q.len()).unwrap_or(0)
    }
    /// Snapshot of the messages waiting in a queue.
    pub fn queue_messages(&self, name: &str) -> Vec<StompMessage> {
        self.shared.state.lock().unwrap().queues.get(&normalize_destination(name)).map(|q| q.iter().cloned().collect()).unwrap_or_default()
    }
    /// Delivered but unacknowledged messages of a destination.
    pub fn pending_count(&self, name: &str) -> usize {
        let d = normalize_destination(name);
        self.shared.state.lock().unwrap().subs.iter().filter(|s| s.dest == d).map(|s| s.pending.len()).sum()
    }
    /// Number of subscriptions on a destination.
    pub fn subscription_count(&self, name: &str) -> usize {
        let d = normalize_destination(name);
        self.shared.state.lock().unwrap().subs.iter().filter(|s| s.dest == d).count()
    }
    /// Destinations that currently have queued messages or subscriptions.
    pub fn destinations(&self) -> Vec<String> {
        let st = self.shared.state.lock().unwrap();
        let mut v: Vec<String> = st.queues.keys().cloned().chain(st.subs.iter().map(|s| s.dest.clone())).collect();
        v.sort();
        v.dedup();
        v
    }
    /// Send a message from the test side.
    pub fn send(&self, destination: &str, headers: &[(&str, &str)], body: &[u8]) {
        let mut st = self.shared.state.lock().unwrap();
        let mut f = Frame::new("SEND").header("destination", normalize_destination(destination));
        for (k, v) in headers {
            f.headers.push((k.to_string(), v.to_string()));
        }
        f.body = body.to_vec();
        let msg = to_message(&mut st, &f);
        st.route(msg);
    }
}

impl Drop for StompBroker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn to_message(st: &mut State, f: &Frame) -> StompMessage {
    st.ids += 1;
    let headers: Vec<(String, String)> = f.headers.iter().filter(|(k, _)| !RESERVED.contains(&k.as_str())).cloned().collect();
    StompMessage {
        id: format!("ID:rust-camel-broker-{}", st.ids),
        destination: normalize_destination(f.get("destination").unwrap_or("")),
        headers,
        body: f.body.clone(),
        priority: f.get("priority").and_then(|p| p.trim().parse::<u8>().ok()).unwrap_or(4).min(9),
        expires: f.get("expires").and_then(|p| p.trim().parse::<i64>().ok()).unwrap_or(0),
        timestamp: now_millis(),
        persistent: f.get("persistent").map(|p| p == "true").unwrap_or(false),
        redelivered: false,
        redelivery_count: 0,
    }
}

fn error_frame(msg: &str, receipt: Option<&str>) -> Frame {
    let mut f = Frame::new("ERROR").header("message", msg).header("content-type", "text/plain");
    if let Some(r) = receipt {
        f = f.header("receipt-id", r);
    }
    f.body = msg.as_bytes().to_vec();
    f
}

/// Process one client frame. `Err(message)` sends an ERROR and closes the connection.
fn handle(shared: &Shared, conn: u64, f: &Frame) -> std::result::Result<bool, String> {
    let mut st = shared.state.lock().unwrap();
    let max = shared.max_redeliveries;
    match f.command.as_str() {
        "SEND" => {
            let dest = f.get("destination").ok_or("SEND frame without destination header")?;
            if dest.is_empty() {
                return Err("SEND frame with an empty destination".into());
            }
            let msg = to_message(&mut st, f);
            match f.get("transaction") {
                Some(tx) => {
                    let c = st.conns.get_mut(&conn).ok_or("unknown connection")?;
                    c.txs.get_mut(tx).ok_or_else(|| format!("Invalid transaction id: {}", tx))?.push(TxOp::Send(msg));
                }
                None => st.route(msg),
            }
        }
        "SUBSCRIBE" => {
            let dest = normalize_destination(f.get("destination").ok_or("SUBSCRIBE frame without destination header")?);
            let id = f.get("id").ok_or("SUBSCRIBE frame without id header")?.to_string();
            let ack = match f.get("ack") {
                None => AckMode::Auto,
                Some(a) => AckMode::parse(a).ok_or_else(|| format!("Invalid ack mode: {}", a))?,
            };
            let selector = match f.get("selector").filter(|s| !s.trim().is_empty()) {
                Some(s) => Some(Selector::parse(s).map_err(|e| e.message)?),
                None => None,
            };
            let prefetch = f.get("activemq.prefetchSize").and_then(|p| p.parse::<usize>().ok()).unwrap_or(0);
            if st.subs.iter().any(|s| s.conn == conn && s.id == id) {
                return Err(format!("Duplicate subscription id: {}", id));
            }
            if dest.starts_with("/temp-") {
                if let Some(c) = st.conns.get_mut(&conn) {
                    if !c.temps.contains(&dest) {
                        c.temps.push(dest.clone());
                    }
                }
            }
            st.subs.push(Sub { conn, id, dest: dest.clone(), ack, selector, prefetch, pending: Vec::new() });
            st.dispatch(&dest);
        }
        "UNSUBSCRIBE" => {
            let id = f.get("id").ok_or("UNSUBSCRIBE frame without id header")?;
            if let Some(pos) = st.subs.iter().position(|s| s.conn == conn && s.id == id) {
                let sub = st.subs.remove(pos);
                for (_, m) in sub.pending {
                    st.requeue_front(m, max, None);
                }
            }
        }
        "ACK" | "NACK" => {
            let id = f.get("id").or_else(|| f.get("message-id")).ok_or("ACK/NACK frame without id header")?.to_string();
            if let Some(tx) = f.get("transaction") {
                let c = st.conns.get_mut(&conn).ok_or("unknown connection")?;
                let ops = c.txs.get_mut(tx).ok_or_else(|| format!("Invalid transaction id: {}", tx))?;
                ops.push(if f.command == "ACK" { TxOp::Ack(id) } else { TxOp::Nack(id) });
                return Ok(true);
            }
            apply_ack(&mut st, conn, &id, f.command == "ACK", max);
        }
        "BEGIN" => {
            let tx = f.get("transaction").ok_or("BEGIN frame without transaction header")?.to_string();
            let c = st.conns.get_mut(&conn).ok_or("unknown connection")?;
            if c.txs.contains_key(&tx) {
                return Err(format!("Transaction {} already started", tx));
            }
            c.txs.insert(tx, Vec::new());
        }
        "COMMIT" | "ABORT" => {
            let tx = f.get("transaction").ok_or("COMMIT/ABORT frame without transaction header")?.to_string();
            let ops = st.conns.get_mut(&conn).ok_or("unknown connection")?.txs.remove(&tx).ok_or_else(|| format!("Invalid transaction id: {}", tx))?;
            let commit = f.command == "COMMIT";
            for op in ops {
                match op {
                    TxOp::Send(m) if commit => st.route(m),
                    TxOp::Send(_) => {}
                    TxOp::Ack(id) => apply_ack(&mut st, conn, &id, commit, max),
                    TxOp::Nack(id) => apply_ack(&mut st, conn, &id, false, max),
                }
            }
        }
        "DISCONNECT" => return Ok(false),
        "CONNECT" | "STOMP" => return Err("Already connected".into()),
        other => return Err(format!("Unknown STOMP action: {}", other)),
    }
    Ok(true)
}

/// Acknowledge (`ack=true`) or redeliver (`ack=false`) a pending delivery.
fn apply_ack(st: &mut State, conn: u64, ack_id: &str, ack: bool, max: u32) {
    let Some((si, msgs)) = st.take_acked(conn, ack_id) else { return };
    let dest = st.subs[si].dest.clone();
    if !ack {
        for m in msgs.into_iter().rev() {
            st.requeue_front(m, max, Some(si));
        }
    }
    st.dispatch(&dest);
}

fn cleanup(st: &mut State, conn: u64, max: u32) {
    let mut pending = Vec::new();
    let mut i = 0;
    while i < st.subs.len() {
        if st.subs[i].conn == conn {
            let s = st.subs.remove(i);
            pending.extend(s.pending.into_iter().map(|(_, m)| m));
        } else {
            i += 1;
        }
    }
    if let Some(c) = st.conns.remove(&conn) {
        for t in c.temps {
            st.queues.remove(&t);
        }
    }
    for m in pending.into_iter().rev() {
        if !is_topic(&m.destination) {
            st.requeue_front(m, max, None);
        }
    }
}

fn serve(shared: &Arc<Shared>, stream: NetStream, conn: u64) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let Ok(rs) = stream.try_clone() else { return };
    let mut reader = BufReader::new(rs);
    let writer = Arc::new(Mutex::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    }));
    let send_now = |f: &Frame| {
        let mut w = writer.lock().unwrap();
        let _ = w.write_all(&f.encode()).and_then(|_| w.flush());
    };
    let Ok(Some(connect)) = read_frame(&mut reader, 64 * 1024 * 1024) else { return };
    if connect.command != "CONNECT" && connect.command != "STOMP" {
        send_now(&error_frame("Expected CONNECT", None));
        return;
    }
    if let Some(v) = connect.get("accept-version") {
        if !v.split(',').any(|x| matches!(x.trim(), "1.0" | "1.1" | "1.2")) {
            send_now(&error_frame("Supported protocol versions are 1.0,1.1,1.2", None));
            return;
        }
    }
    if !shared.users.is_empty() {
        let (l, p) = (connect.get("login").unwrap_or(""), connect.get("passcode").unwrap_or(""));
        if !shared.users.iter().any(|(u, pw)| u == l && pw == p) {
            send_now(&error_frame(&format!("Authentication failed: user '{}'", l), None));
            std::thread::sleep(Duration::from_millis(20));
            return;
        }
    }
    let (cx, cy) = connect
        .get("heart-beat")
        .and_then(|h| h.split_once(','))
        .map(|(a, b)| (a.trim().parse::<u64>().unwrap_or(0), b.trim().parse::<u64>().unwrap_or(0)))
        .unwrap_or((0, 0));
    let (sx, sy) = shared.heartbeat;
    let send_every = if sx > 0 && cy > 0 { sx.max(cy) } else { 0 };
    let expect_every = if sy > 0 && cx > 0 { sy.max(cx) } else { 0 };
    if expect_every > 0 {
        let _ = stream.set_read_timeout(Some(Duration::from_millis(expect_every * 2 + 500)));
    }
    send_now(
        &Frame::new("CONNECTED")
            .header("version", "1.2")
            .header("heart-beat", format!("{},{}", sx, sy))
            .header("session", format!("session-{}", conn))
            .header("server", format!("rust-camel-stomp-broker/{}", env!("CARGO_PKG_VERSION"))),
    );
    shared.state.lock().unwrap().conns.insert(conn, ConnSt { writer: writer.clone(), txs: HashMap::new(), temps: Vec::new() });
    let alive = Arc::new(AtomicBool::new(true));
    let hb = if send_every > 0 {
        let (w, a, r) = (writer.clone(), alive.clone(), shared.running.clone());
        std::thread::Builder::new()
            .name(format!("stomp-broker-hb-{}", conn))
            .spawn(move || {
                let mut waited = 0u64;
                while a.load(Ordering::SeqCst) && r.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(10));
                    waited += 10;
                    if waited >= send_every {
                        waited = 0;
                        let mut g = w.lock().unwrap();
                        if g.write_all(b"\n").and_then(|_| g.flush()).is_err() {
                            break;
                        }
                    }
                }
            })
            .ok()
    } else {
        None
    };
    loop {
        let f = match read_frame(&mut reader, 64 * 1024 * 1024) {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::InvalidData {
                    send_now(&error_frame(&format!("Malformed frame: {}", e), None));
                }
                break;
            }
        };
        shared.frames.fetch_add(1, Ordering::SeqCst);
        let receipt = f.get("receipt").map(String::from);
        match handle(shared, conn, &f) {
            Ok(keep) => {
                if let Some(r) = &receipt {
                    send_now(&Frame::new("RECEIPT").header("receipt-id", r.clone()));
                }
                if !keep {
                    break;
                }
            }
            Err(msg) => {
                send_now(&error_frame(&msg, receipt.as_deref()));
                break;
            }
        }
    }
    alive.store(false, Ordering::SeqCst);
    {
        let mut st = shared.state.lock().unwrap();
        cleanup(&mut st, conn, shared.max_redeliveries);
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
    if let Some(t) = hb {
        let _ = t.join();
    }
}

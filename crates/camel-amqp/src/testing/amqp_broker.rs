//! An embedded AMQP 0-9-1 broker for tests (a small RabbitMQ stand-in).
//!
//! Supports PLAIN authentication, virtual hosts (one shared namespace), heartbeats,
//! `direct` / `fanout` / `topic` / `headers` exchanges (plus the default exchange and
//! the pre-declared `amq.*` exchanges), queues (server-named, exclusive, auto-delete,
//! `x-message-ttl`, `x-dead-letter-exchange`, `x-dead-letter-routing-key`), bindings,
//! consumers with round-robin dispatch, `basic.qos` prefetch (per consumer or global),
//! acks / nacks / rejects with requeue or dead-lettering (`x-death` header), `basic.get`,
//! `basic.recover`, mandatory publishes (`basic.return`), publisher confirms and per
//! message expiration. Unacknowledged messages are requeued (redelivered) when their
//! channel or connection closes. Messages are kept in memory only.
//!
//! ```no_run
//! let broker = camel_amqp::testing::AmqpBroker::start().unwrap();
//! let uri = format!("rabbitmq:orders?hostname=127.0.0.1&portNumber={}&queue=q1", broker.port());
//! # let _ = uri;
//! broker.stop();
//! ```

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{BufReader, Read, Write};
use std::net::TcpStream;

use camel_tls::net::NetStream;
use camel_tls::TlsServerConfig;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use camel_core::error::Result;
use camel_core::value::{values_equal, Value};

use crate::amqp::codec::*;
use crate::common::{unique_id, TcpServer};

/// A message stored in a queue (as seen by [`AmqpBroker::queue_messages`]).
#[derive(Clone, Debug)]
pub struct StoredMessage {
    pub exchange: String,
    pub routing_key: String,
    pub properties: BasicProperties,
    pub body: Vec<u8>,
    pub redelivered: bool,
}

impl StoredMessage {
    /// The body as (lossy) UTF-8 text.
    pub fn body_string(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

#[derive(Clone)]
struct Msg {
    m: StoredMessage,
    expires_at: Option<Instant>,
}

struct Binding {
    queue: String,
    key: String,
    args: Table,
}

struct Exch {
    kind: String,
    bindings: Vec<Binding>,
}

struct QConsumer {
    conn: u64,
    channel: u16,
    tag: String,
    no_ack: bool,
    prefetch: u16,
}

struct Queue {
    exclusive_owner: Option<u64>,
    auto_delete: bool,
    args: Table,
    ready: VecDeque<Msg>,
    consumers: Vec<QConsumer>,
    rr: usize,
    had_consumer: bool,
}

struct Unacked {
    queue: String,
    tag: String,
    msg: Msg,
}

#[derive(Default)]
struct BChan {
    next_tag: u64,
    unacked: BTreeMap<u64, Unacked>,
    prefetch: u16,
    global_prefetch: u16,
    confirm: bool,
    publish_seq: u64,
    last_queue: String,
}

struct Conn {
    writer: Arc<Mutex<NetStream>>,
    frame_max: u32,
}

#[derive(Default)]
struct State {
    exchanges: HashMap<String, Exch>,
    queues: HashMap<String, Queue>,
    channels: HashMap<(u64, u16), BChan>,
    conns: HashMap<u64, Conn>,
}

type ChannelError = (u16, String);

fn err(code: u16, text: impl Into<String>) -> ChannelError {
    let t = text.into();
    let name = match code {
        reply::NOT_FOUND => "NOT_FOUND",
        reply::ACCESS_REFUSED => "ACCESS_REFUSED",
        reply::RESOURCE_LOCKED => "RESOURCE_LOCKED",
        reply::PRECONDITION_FAILED => "PRECONDITION_FAILED",
        reply::NOT_IMPLEMENTED => "NOT_IMPLEMENTED",
        _ => "COMMAND_INVALID",
    };
    (code, format!("{} - {}", name, t))
}

/// Topic routing-key matching (`*` = exactly one word, `#` = zero or more words).
pub fn topic_matches(pattern: &str, key: &str) -> bool {
    fn rec(p: &[&str], k: &[&str]) -> bool {
        match p.first() {
            None => k.is_empty(),
            Some(&"#") => (0..=k.len()).any(|i| rec(&p[1..], &k[i..])),
            Some(&"*") => !k.is_empty() && rec(&p[1..], &k[1..]),
            Some(w) => !k.is_empty() && k[0] == *w && rec(&p[1..], &k[1..]),
        }
    }
    let p: Vec<&str> = pattern.split('.').collect();
    let k: Vec<&str> = if key.is_empty() { Vec::new() } else { key.split('.').collect() };
    rec(&p, &k)
}

fn headers_match(binding: &Table, headers: Option<&Table>) -> bool {
    let all = !matches!(binding.get("x-match").and_then(|v| v.as_str()), Some("any") | Some("any-with-x"));
    let empty = Table::new();
    let h = headers.unwrap_or(&empty);
    let mut any_matched = false;
    for (k, v) in binding {
        if k.starts_with("x-") {
            continue;
        }
        let ok = match h.get(k) {
            None => false,
            Some(hv) => v.is_null() || values_equal(v, hv),
        };
        if all && !ok {
            return false;
        }
        any_matched |= ok;
    }
    all || any_matched
}

struct Shared {
    state: Mutex<State>,
    users: Vec<(String, String)>,
    vhosts: Vec<String>,
    heartbeat: u16,
    frame_max: u32,
    heartbeats: AtomicU64,
    running: Arc<AtomicBool>,
}

/// Builder for [`AmqpBroker`].
pub struct AmqpBrokerBuilder {
    users: Vec<(String, String)>,
    vhosts: Vec<String>,
    heartbeat: u16,
    frame_max: u32,
    host: String,
    port: u16,
    tls: Option<Arc<TlsServerConfig>>,
}

impl AmqpBrokerBuilder {
    /// Serve AMQP over TLS (`amqps`) with this configuration.
    pub fn tls(mut self, cfg: TlsServerConfig) -> Self {
        self.tls = Some(Arc::new(cfg));
        self
    }
    /// Add a user (the default user `guest`/`guest` is replaced by the first call).
    pub fn user(mut self, name: &str, password: &str) -> Self {
        if self.users.len() == 1 && self.users[0].0 == "guest" {
            self.users.clear();
        }
        self.users.push((name.into(), password.into()));
        self
    }
    /// Add a virtual host (`/` always exists).
    pub fn vhost(mut self, v: &str) -> Self {
        self.vhosts.push(v.into());
        self
    }
    /// Heartbeat interval the broker proposes in `connection.tune` (seconds, 0 = none).
    pub fn heartbeat(mut self, secs: u16) -> Self {
        self.heartbeat = secs;
        self
    }
    /// Frame-max the broker proposes.
    pub fn frame_max(mut self, n: u32) -> Self {
        self.frame_max = n;
        self
    }
    /// Bind address (default `127.0.0.1:0`).
    pub fn bind(mut self, host: &str, port: u16) -> Self {
        self.host = host.into();
        self.port = port;
        self
    }
    pub fn start(self) -> Result<AmqpBroker> {
        let running = Arc::new(AtomicBool::new(true));
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            users: self.users,
            vhosts: self.vhosts,
            heartbeat: self.heartbeat,
            frame_max: self.frame_max,
            heartbeats: AtomicU64::new(0),
            running: running.clone(),
        });
        {
            let mut st = shared.state.lock().unwrap();
            for (n, k) in [("", "direct"), ("amq.direct", "direct"), ("amq.fanout", "fanout"), ("amq.topic", "topic"), ("amq.headers", "headers"), ("amq.match", "headers")] {
                st.exchanges.insert(n.into(), Exch { kind: k.into(), bindings: Vec::new() });
            }
        }
        let s2 = shared.clone();
        let tls = self.tls.clone();
        let server = TcpServer::bind(&self.host, self.port, "amqp-broker", move |stream, id| {
            if let Some(s) = wrap(stream, tls.as_deref()) {
                serve(&s2, s, id);
            }
        })?;
        Ok(AmqpBroker { server, shared, tls: self.tls.is_some() })
    }
}

/// The embedded AMQP 0-9-1 broker. Stopped on drop.
pub struct AmqpBroker {
    server: TcpServer,
    shared: Arc<Shared>,
    tls: bool,
}

/// Wraps an accepted connection (TLS handshake when configured).
fn wrap(stream: TcpStream, tls: Option<&TlsServerConfig>) -> Option<NetStream> {
    match tls {
        Some(cfg) => {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
            let s = NetStream::server(stream, cfg).ok()?;
            let _ = s.set_read_timeout(None);
            Some(s)
        }
        None => Some(NetStream::plain(stream)),
    }
}

impl AmqpBroker {
    /// Start on `127.0.0.1` with an ephemeral port and user `guest`/`guest`.
    pub fn start() -> Result<AmqpBroker> {
        Self::builder().start()
    }
    pub fn builder() -> AmqpBrokerBuilder {
        AmqpBrokerBuilder { users: vec![("guest".into(), "guest".into())], vhosts: vec!["/".into()], heartbeat: 0, frame_max: 131_072, host: "127.0.0.1".into(), port: 0, tls: None }
    }
    pub fn port(&self) -> u16 {
        self.server.port()
    }
    /// `amqp://127.0.0.1:<port>` (`amqps://` with TLS).
    pub fn url(&self) -> String {
        format!("{}://127.0.0.1:{}", if self.tls { "amqps" } else { "amqp" }, self.port())
    }
    /// Stop the broker and close every connection.
    pub fn stop(&self) {
        self.shared.running.store(false, Ordering::SeqCst);
        self.server.stop();
    }
    /// Number of open client connections.
    pub fn connection_count(&self) -> usize {
        self.server.connection_count()
    }
    /// Abruptly close every client connection (simulates a broker restart).
    pub fn drop_connections(&self) {
        self.server.drop_connections();
    }
    /// Heartbeat frames received from clients so far.
    pub fn heartbeats_received(&self) -> u64 {
        self.shared.heartbeats.load(Ordering::SeqCst)
    }
    /// Names of all queues.
    pub fn queue_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.shared.state.lock().unwrap().queues.keys().cloned().collect();
        v.sort();
        v
    }
    /// Whether a queue exists.
    pub fn has_queue(&self, name: &str) -> bool {
        self.shared.state.lock().unwrap().queues.contains_key(name)
    }
    /// Messages ready for delivery in a queue (0 if the queue does not exist).
    pub fn queue_depth(&self, name: &str) -> usize {
        self.shared.state.lock().unwrap().queues.get(name).map(|q| q.ready.len()).unwrap_or(0)
    }
    /// Delivered but not yet acknowledged messages of a queue.
    pub fn unacked_count(&self, name: &str) -> usize {
        let st = self.shared.state.lock().unwrap();
        st.channels.values().map(|c| c.unacked.values().filter(|u| u.queue == name).count()).sum()
    }
    /// Number of consumers on a queue.
    pub fn consumer_count(&self, name: &str) -> usize {
        self.shared.state.lock().unwrap().queues.get(name).map(|q| q.consumers.len()).unwrap_or(0)
    }
    /// Snapshot of the ready messages of a queue.
    pub fn queue_messages(&self, name: &str) -> Vec<StoredMessage> {
        self.shared.state.lock().unwrap().queues.get(name).map(|q| q.ready.iter().map(|m| m.m.clone()).collect()).unwrap_or_default()
    }
    /// The type of an exchange.
    pub fn exchange_type(&self, name: &str) -> Option<String> {
        self.shared.state.lock().unwrap().exchanges.get(name).map(|e| e.kind.clone())
    }
    /// `(queue, routing key)` bindings of an exchange.
    pub fn bindings(&self, exchange: &str) -> Vec<(String, String)> {
        self.shared.state.lock().unwrap().exchanges.get(exchange).map(|e| e.bindings.iter().map(|b| (b.queue.clone(), b.key.clone())).collect()).unwrap_or_default()
    }
    /// Declare a queue from the test side.
    pub fn declare_queue(&self, name: &str, args: Table) {
        let mut st = self.shared.state.lock().unwrap();
        st.queues.entry(name.into()).or_insert_with(|| new_queue(None, false, args));
    }
    /// Publish a message from the test side. Returns the number of queues it was routed to.
    pub fn publish(&self, exchange: &str, routing_key: &str, properties: BasicProperties, body: &[u8]) -> usize {
        let mut st = self.shared.state.lock().unwrap();
        let msg = StoredMessage { exchange: exchange.into(), routing_key: routing_key.into(), properties, body: body.to_vec(), redelivered: false };
        route_and_enqueue(&mut st, msg).len()
    }
}

impl Drop for AmqpBroker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn new_queue(owner: Option<u64>, auto_delete: bool, args: Table) -> Queue {
    Queue { exclusive_owner: owner, auto_delete, args, ready: VecDeque::new(), consumers: Vec::new(), rr: 0, had_consumer: false }
}

fn ttl_of(props: &BasicProperties, queue_args: &Table) -> Option<Instant> {
    let msg_ttl = props.expiration.as_deref().and_then(|e| e.trim().parse::<u64>().ok());
    let q_ttl = queue_args.get("x-message-ttl").and_then(|v| v.as_i64()).map(|v| v.max(0) as u64);
    let ttl = match (msg_ttl, q_ttl) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    ttl.map(|t| Instant::now() + Duration::from_millis(t))
}

/// Route a message through its exchange, enqueue it and dispatch. Returns the queues.
fn route_and_enqueue(st: &mut State, msg: StoredMessage) -> Vec<String> {
    let queues = route(st, &msg.exchange, &msg.routing_key, msg.properties.headers.as_ref());
    for q in &queues {
        if let Some(queue) = st.queues.get_mut(q) {
            let expires_at = ttl_of(&msg.properties, &queue.args);
            queue.ready.push_back(Msg { m: msg.clone(), expires_at });
        }
    }
    for q in &queues {
        dispatch(st, q);
    }
    queues
}

fn route(st: &State, exchange: &str, rk: &str, headers: Option<&Table>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if exchange.is_empty() {
        if st.queues.contains_key(rk) {
            out.push(rk.to_string());
        }
        return out;
    }
    let Some(ex) = st.exchanges.get(exchange) else { return out };
    for b in &ex.bindings {
        let hit = match ex.kind.as_str() {
            "fanout" => true,
            "topic" => topic_matches(&b.key, rk),
            "headers" => headers_match(&b.args, headers),
            _ => b.key == rk,
        };
        if hit && !out.contains(&b.queue) && st.queues.contains_key(&b.queue) {
            out.push(b.queue.clone());
        }
    }
    out
}

/// Dead-letter a message from `queue` (reason `rejected` / `expired`) if the queue has a DLX.
fn dead_letter(st: &mut State, queue: &str, mut msg: StoredMessage, reason: &str) {
    let Some(q) = st.queues.get(queue) else { return };
    let Some(dlx) = q.args.get("x-dead-letter-exchange").map(|v| v.to_string()) else { return };
    let rk = q.args.get("x-dead-letter-routing-key").map(|v| v.to_string()).unwrap_or_else(|| msg.routing_key.clone());
    let mut headers = msg.properties.headers.take().unwrap_or_default();
    let mut deaths: Vec<Value> = headers.get("x-death").and_then(|v| v.as_array().cloned()).unwrap_or_default();
    let mut found = false;
    for d in deaths.iter_mut() {
        if let Some(o) = d.as_object_mut() {
            if o.get("queue").map(|v| v.to_string()).as_deref() == Some(queue) && o.get("reason").map(|v| v.to_string()).as_deref() == Some(reason) {
                let c = o.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
                o.insert("count".into(), Value::Int(c + 1));
                found = true;
            }
        }
    }
    if !found {
        deaths.insert(
            0,
            Value::object([
                ("queue", Value::from(queue)),
                ("reason", Value::from(reason)),
                ("exchange", Value::from(msg.exchange.as_str())),
                ("routing-keys", Value::Array(vec![Value::from(msg.routing_key.as_str())])),
                ("count", Value::Int(1)),
            ]),
        );
    }
    if !headers.contains_key("x-first-death-queue") {
        headers.insert("x-first-death-queue".into(), Value::from(queue));
        headers.insert("x-first-death-reason".into(), Value::from(reason));
        headers.insert("x-first-death-exchange".into(), Value::from(msg.exchange.as_str()));
    }
    headers.insert("x-death".into(), Value::Array(deaths));
    msg.properties.headers = Some(headers);
    msg.properties.expiration = None;
    msg.exchange = dlx;
    msg.routing_key = rk;
    msg.redelivered = false;
    route_and_enqueue(st, msg);
}

fn deliver_frames(conn: &Conn, channel: u16, method: &Method, m: &StoredMessage) {
    let mut out = Vec::new();
    put_content(&mut out, channel, method, &m.properties, &m.body, conn.frame_max);
    let mut w = conn.writer.lock().unwrap();
    let _ = w.write_all(&out).and_then(|_| w.flush());
}

fn send(st: &State, conn: u64, channel: u16, m: &Method) {
    if let Some(c) = st.conns.get(&conn) {
        let mut w = c.writer.lock().unwrap();
        let _ = write_method(&mut *w, channel, m);
    }
}

/// Push ready messages of a queue to consumers with spare prefetch capacity.
fn dispatch(st: &mut State, queue: &str) {
    loop {
        // drop expired messages at the head
        let expired = {
            let Some(q) = st.queues.get_mut(queue) else { return };
            match q.ready.front() {
                Some(m) if m.expires_at.is_some_and(|t| t <= Instant::now()) => q.ready.pop_front(),
                _ => None,
            }
        };
        if let Some(m) = expired {
            dead_letter(st, queue, m.m, "expired");
            continue;
        }
        let State { queues, channels, conns, .. } = st;
        let Some(q) = queues.get_mut(queue) else { return };
        if q.ready.is_empty() || q.consumers.is_empty() {
            return;
        }
        let n = q.consumers.len();
        let mut chosen = None;
        for i in 0..n {
            let idx = (q.rr + i) % n;
            let c = &q.consumers[idx];
            if c.no_ack {
                chosen = Some(idx);
                break;
            }
            let Some(ch) = channels.get(&(c.conn, c.channel)) else { continue };
            let mine = ch.unacked.values().filter(|u| u.tag == c.tag).count();
            let per_ok = c.prefetch == 0 || mine < c.prefetch as usize;
            let glob_ok = ch.global_prefetch == 0 || ch.unacked.len() < ch.global_prefetch as usize;
            if per_ok && glob_ok {
                chosen = Some(idx);
                break;
            }
        }
        let Some(idx) = chosen else { return };
        q.rr = idx + 1;
        let msg = q.ready.pop_front().unwrap();
        let c = &q.consumers[idx];
        let Some(ch) = channels.get_mut(&(c.conn, c.channel)) else { return };
        ch.next_tag += 1;
        let tag = ch.next_tag;
        let method = Method::new(
            m::BASIC_DELIVER,
            vec![Arg::ShortStr(c.tag.clone()), Arg::LongLong(tag), Arg::Bit(msg.m.redelivered), Arg::ShortStr(msg.m.exchange.clone()), Arg::ShortStr(msg.m.routing_key.clone())],
        );
        if let Some(conn) = conns.get(&c.conn) {
            deliver_frames(conn, c.channel, &method, &msg.m);
        }
        if !c.no_ack {
            ch.unacked.insert(tag, Unacked { queue: queue.to_string(), tag: c.tag.clone(), msg });
        }
    }
}

/// Requeue (front, in original order, marked redelivered) a set of unacked messages.
fn requeue(st: &mut State, items: Vec<Unacked>) {
    let mut touched: Vec<String> = Vec::new();
    for u in items.into_iter().rev() {
        if let Some(q) = st.queues.get_mut(&u.queue) {
            let mut msg = u.msg;
            msg.m.redelivered = true;
            q.ready.push_front(msg);
            if !touched.contains(&u.queue) {
                touched.push(u.queue.clone());
            }
        }
    }
    for q in touched {
        dispatch(st, &q);
    }
}

fn remove_consumers(st: &mut State, pred: impl Fn(&QConsumer) -> bool) {
    let mut to_delete = Vec::new();
    for (name, q) in st.queues.iter_mut() {
        let before = q.consumers.len();
        q.consumers.retain(|c| !pred(c));
        if q.consumers.len() != before && q.consumers.is_empty() && q.auto_delete && q.had_consumer {
            to_delete.push(name.clone());
        }
    }
    for n in to_delete {
        delete_queue(st, &n);
    }
}

fn delete_queue(st: &mut State, name: &str) -> u32 {
    let Some(q) = st.queues.remove(name) else { return 0 };
    for ex in st.exchanges.values_mut() {
        ex.bindings.retain(|b| b.queue != name);
    }
    for c in &q.consumers {
        send(st, c.conn, c.channel, &Method::new(m::BASIC_CANCEL, vec![Arg::ShortStr(c.tag.clone()), Arg::Bit(true)]));
    }
    q.ready.len() as u32
}

/// Close down a channel: requeue its unacked messages, remove its consumers.
fn cleanup_channel(st: &mut State, conn: u64, channel: u16) {
    remove_consumers(st, |c| c.conn == conn && c.channel == channel);
    if let Some(ch) = st.channels.remove(&(conn, channel)) {
        let items: Vec<Unacked> = ch.unacked.into_values().collect();
        requeue(st, items);
    }
}

fn cleanup_connection(st: &mut State, conn: u64) {
    let chans: Vec<u16> = st.channels.keys().filter(|(c, _)| *c == conn).map(|(_, ch)| *ch).collect();
    for ch in chans {
        cleanup_channel(st, conn, ch);
    }
    let exclusive: Vec<String> = st.queues.iter().filter(|(_, q)| q.exclusive_owner == Some(conn)).map(|(n, _)| n.clone()).collect();
    for q in exclusive {
        delete_queue(st, &q);
    }
    st.conns.remove(&conn);
}

fn take_acked(ch: &mut BChan, tag: u64, multiple: bool) -> std::result::Result<Vec<Unacked>, ChannelError> {
    if multiple {
        let keys: Vec<u64> = ch.unacked.keys().copied().filter(|k| tag == 0 || *k <= tag).collect();
        if keys.is_empty() && tag != 0 {
            return Err(err(reply::PRECONDITION_FAILED, format!("unknown delivery tag {}", tag)));
        }
        Ok(keys.into_iter().filter_map(|k| ch.unacked.remove(&k)).collect())
    } else {
        match ch.unacked.remove(&tag) {
            Some(u) => Ok(vec![u]),
            None => Err(err(reply::PRECONDITION_FAILED, format!("unknown delivery tag {}", tag))),
        }
    }
}

/// Handle one channel method (publishes are handled separately).
fn handle_method(shared: &Shared, conn: u64, channel: u16, method: &Method) -> std::result::Result<(), ChannelError> {
    let mut guard = shared.state.lock().unwrap();
    let st = &mut *guard;
    let reply = |st: &State, m: Method| send(st, conn, channel, &m);
    match method.id() {
        m::EXCHANGE_DECLARE => {
            let (name, kind, passive, no_wait) = (method.str(1), method.str(2), method.bit(3), method.bit(7));
            match st.exchanges.get(&name) {
                Some(e) if !passive && e.kind != kind => {
                    return Err(err(reply::PRECONDITION_FAILED, format!("inequivalent arg 'type' for exchange '{}' in vhost '/': received '{}' but current is '{}'", name, kind, e.kind)))
                }
                Some(_) => {}
                None if passive => return Err(err(reply::NOT_FOUND, format!("no exchange '{}' in vhost '/'", name))),
                None => {
                    if name.starts_with("amq.") {
                        return Err(err(reply::ACCESS_REFUSED, format!("exchange name '{}' contains reserved prefix 'amq.*'", name)));
                    }
                    if !["direct", "fanout", "topic", "headers"].contains(&kind.as_str()) {
                        return Err(err(reply::COMMAND_INVALID, format!("invalid exchange type '{}'", kind)));
                    }
                    st.exchanges.insert(name, Exch { kind, bindings: Vec::new() });
                }
            }
            if !no_wait {
                reply(st, Method::new(m::EXCHANGE_DECLARE_OK, vec![]));
            }
        }
        m::EXCHANGE_DELETE => {
            let name = method.str(1);
            if let Some(e) = st.exchanges.get(&name) {
                if method.bit(2) && !e.bindings.is_empty() {
                    return Err(err(reply::PRECONDITION_FAILED, format!("exchange '{}' in use", name)));
                }
                if !name.is_empty() && !name.starts_with("amq.") {
                    st.exchanges.remove(&name);
                }
            }
            if !method.bit(3) {
                reply(st, Method::new(m::EXCHANGE_DELETE_OK, vec![]));
            }
        }
        m::QUEUE_DECLARE => {
            let mut name = method.str(1);
            let (passive, exclusive, auto_delete, no_wait) = (method.bit(2), method.bit(4), method.bit(5), method.bit(6));
            if name.is_empty() {
                name = format!("amq.gen-{}", unique_id());
            }
            match st.queues.get(&name) {
                Some(q) => {
                    if let Some(owner) = q.exclusive_owner {
                        if owner != conn {
                            return Err(err(reply::RESOURCE_LOCKED, format!("cannot obtain exclusive access to locked queue '{}' in vhost '/'", name)));
                        }
                    }
                }
                None if passive => return Err(err(reply::NOT_FOUND, format!("no queue '{}' in vhost '/'", name))),
                None => {
                    st.queues.insert(name.clone(), new_queue(exclusive.then_some(conn), auto_delete, method.table(7)));
                }
            }
            if let Some(ch) = st.channels.get_mut(&(conn, channel)) {
                ch.last_queue = name.clone();
            }
            let q = &st.queues[&name];
            let (mc, cc) = (q.ready.len() as u32, q.consumers.len() as u32);
            if !no_wait {
                reply(st, Method::new(m::QUEUE_DECLARE_OK, vec![Arg::ShortStr(name), Arg::Long(mc), Arg::Long(cc)]));
            }
        }
        m::QUEUE_BIND | m::QUEUE_UNBIND => {
            let bind = method.is(m::QUEUE_BIND);
            let mut queue = method.str(1);
            if queue.is_empty() {
                queue = st.channels.get(&(conn, channel)).map(|c| c.last_queue.clone()).unwrap_or_default();
            }
            let (exchange, key) = (method.str(2), method.str(3));
            let args = method.table(if bind { 5 } else { 4 });
            if !st.queues.contains_key(&queue) {
                return Err(err(reply::NOT_FOUND, format!("no queue '{}' in vhost '/'", queue)));
            }
            if exchange.is_empty() {
                return Err(err(reply::ACCESS_REFUSED, "operation not permitted on the default exchange"));
            }
            let Some(ex) = st.exchanges.get_mut(&exchange) else {
                return Err(err(reply::NOT_FOUND, format!("no exchange '{}' in vhost '/'", exchange)));
            };
            if bind {
                if !ex.bindings.iter().any(|b| b.queue == queue && b.key == key && b.args == args) {
                    ex.bindings.push(Binding { queue, key, args });
                }
                if !method.bit(4) {
                    reply(st, Method::new(m::QUEUE_BIND_OK, vec![]));
                }
            } else {
                ex.bindings.retain(|b| !(b.queue == queue && b.key == key));
                reply(st, Method::new(m::QUEUE_UNBIND_OK, vec![]));
            }
        }
        m::QUEUE_PURGE => {
            let name = method.str(1);
            let Some(q) = st.queues.get_mut(&name) else { return Err(err(reply::NOT_FOUND, format!("no queue '{}' in vhost '/'", name))) };
            let n = q.ready.len() as u32;
            q.ready.clear();
            if !method.bit(2) {
                reply(st, Method::new(m::QUEUE_PURGE_OK, vec![Arg::Long(n)]));
            }
        }
        m::QUEUE_DELETE => {
            let name = method.str(1);
            if let Some(q) = st.queues.get(&name) {
                if method.bit(2) && !q.consumers.is_empty() {
                    return Err(err(reply::PRECONDITION_FAILED, format!("queue '{}' in vhost '/' in use", name)));
                }
                if method.bit(3) && !q.ready.is_empty() {
                    return Err(err(reply::PRECONDITION_FAILED, format!("queue '{}' in vhost '/' is not empty", name)));
                }
            }
            let n = delete_queue(st, &name);
            if !method.bit(4) {
                reply(st, Method::new(m::QUEUE_DELETE_OK, vec![Arg::Long(n)]));
            }
        }
        m::BASIC_QOS => {
            let (count, global) = (method.num(1) as u16, method.bit(2));
            if let Some(ch) = st.channels.get_mut(&(conn, channel)) {
                if global {
                    ch.global_prefetch = count;
                } else {
                    ch.prefetch = count;
                }
            }
            reply(st, Method::new(m::BASIC_QOS_OK, vec![]));
            let names: Vec<String> = st.queues.keys().cloned().collect();
            for n in names {
                dispatch(st, &n);
            }
        }
        m::BASIC_CONSUME => {
            let mut queue = method.str(1);
            if queue.is_empty() {
                queue = st.channels.get(&(conn, channel)).map(|c| c.last_queue.clone()).unwrap_or_default();
            }
            let mut tag = method.str(2);
            if tag.is_empty() {
                tag = format!("amq.ctag-{}", unique_id());
            }
            let (no_ack, exclusive, no_wait) = (method.bit(4), method.bit(5), method.bit(6));
            let prefetch = st.channels.get(&(conn, channel)).map(|c| c.prefetch).unwrap_or(0);
            let Some(q) = st.queues.get_mut(&queue) else { return Err(err(reply::NOT_FOUND, format!("no queue '{}' in vhost '/'", queue))) };
            if let Some(owner) = q.exclusive_owner {
                if owner != conn {
                    return Err(err(reply::RESOURCE_LOCKED, format!("cannot obtain exclusive access to locked queue '{}'", queue)));
                }
            }
            if exclusive && !q.consumers.is_empty() {
                return Err(err(reply::ACCESS_REFUSED, format!("queue '{}' in vhost '/' in exclusive use", queue)));
            }
            q.consumers.push(QConsumer { conn, channel, tag: tag.clone(), no_ack, prefetch });
            q.had_consumer = true;
            if !no_wait {
                reply(st, Method::new(m::BASIC_CONSUME_OK, vec![Arg::ShortStr(tag)]));
            }
            dispatch(st, &queue);
        }
        m::BASIC_CANCEL => {
            let tag = method.str(0);
            remove_consumers(st, |c| c.conn == conn && c.channel == channel && c.tag == tag);
            if !method.bit(1) {
                reply(st, Method::new(m::BASIC_CANCEL_OK, vec![Arg::ShortStr(tag)]));
            }
        }
        m::BASIC_GET => {
            let (queue, no_ack) = (method.str(1), method.bit(2));
            let msg = loop {
                let Some(q) = st.queues.get_mut(&queue) else { return Err(err(reply::NOT_FOUND, format!("no queue '{}' in vhost '/'", queue))) };
                match q.ready.pop_front() {
                    Some(m) if m.expires_at.is_some_and(|t| t <= Instant::now()) => {
                        dead_letter(st, &queue, m.m, "expired");
                        continue;
                    }
                    other => break other,
                }
            };
            match msg {
                None => reply(st, Method::new(m::BASIC_GET_EMPTY, vec![Arg::ShortStr(String::new())])),
                Some(msg) => {
                    let left = st.queues.get(&queue).map(|q| q.ready.len()).unwrap_or(0) as u32;
                    let Some(ch) = st.channels.get_mut(&(conn, channel)) else { return Ok(()) };
                    ch.next_tag += 1;
                    let tag = ch.next_tag;
                    let method = Method::new(
                        m::BASIC_GET_OK,
                        vec![Arg::LongLong(tag), Arg::Bit(msg.m.redelivered), Arg::ShortStr(msg.m.exchange.clone()), Arg::ShortStr(msg.m.routing_key.clone()), Arg::Long(left)],
                    );
                    if let Some(c) = st.conns.get(&conn) {
                        deliver_frames(c, channel, &method, &msg.m);
                    }
                    if !no_ack {
                        if let Some(ch) = st.channels.get_mut(&(conn, channel)) {
                            ch.unacked.insert(tag, Unacked { queue, tag: String::new(), msg });
                        }
                    }
                }
            }
        }
        m::BASIC_ACK => {
            let Some(ch) = st.channels.get_mut(&(conn, channel)) else { return Ok(()) };
            let acked = take_acked(ch, method.num(0), method.bit(1))?;
            let mut queues: Vec<String> = acked.into_iter().map(|u| u.queue).collect();
            queues.dedup();
            for q in queues {
                dispatch(st, &q);
            }
        }
        m::BASIC_REJECT | m::BASIC_NACK => {
            let (multiple, requeue_it) = if method.is(m::BASIC_NACK) { (method.bit(1), method.bit(2)) } else { (false, method.bit(1)) };
            let Some(ch) = st.channels.get_mut(&(conn, channel)) else { return Ok(()) };
            let items = take_acked(ch, method.num(0), multiple)?;
            if requeue_it {
                requeue(st, items);
            } else {
                let mut queues = Vec::new();
                for u in items {
                    dead_letter(st, &u.queue, u.msg.m, "rejected");
                    queues.push(u.queue);
                }
                queues.dedup();
                for q in queues {
                    dispatch(st, &q);
                }
            }
        }
        m::BASIC_RECOVER | m::BASIC_RECOVER_ASYNC => {
            let items: Vec<Unacked> = st.channels.get_mut(&(conn, channel)).map(|c| std::mem::take(&mut c.unacked).into_values().collect()).unwrap_or_default();
            requeue(st, items);
            if method.is(m::BASIC_RECOVER) {
                reply(st, Method::new(m::BASIC_RECOVER_OK, vec![]));
            }
        }
        m::CONFIRM_SELECT => {
            if let Some(ch) = st.channels.get_mut(&(conn, channel)) {
                ch.confirm = true;
            }
            if !method.bit(0) {
                reply(st, Method::new(m::CONFIRM_SELECT_OK, vec![]));
            }
        }
        m::TX_SELECT => reply(st, Method::new(m::TX_SELECT_OK, vec![])),
        m::TX_COMMIT => reply(st, Method::new(m::TX_COMMIT_OK, vec![])),
        m::TX_ROLLBACK => reply(st, Method::new(m::TX_ROLLBACK_OK, vec![])),
        m::CHANNEL_FLOW => reply(st, Method::new(m::CHANNEL_FLOW_OK, vec![Arg::Bit(method.bit(0))])),
        m::CHANNEL_FLOW_OK | m::BASIC_CANCEL_OK => {}
        _ => return Err(err(reply::NOT_IMPLEMENTED, format!("method {} not implemented", method.name()))),
    }
    Ok(())
}

fn handle_publish(shared: &Shared, conn: u64, channel: u16, method: &Method, props: BasicProperties, body: Vec<u8>) -> std::result::Result<(), ChannelError> {
    let mut guard = shared.state.lock().unwrap();
    let st = &mut *guard;
    let (exchange, rk, mandatory) = (method.str(1), method.str(2), method.bit(3));
    if !st.exchanges.contains_key(&exchange) {
        return Err(err(reply::NOT_FOUND, format!("no exchange '{}' in vhost '/'", exchange)));
    }
    let msg = StoredMessage { exchange: exchange.clone(), routing_key: rk.clone(), properties: props, body, redelivered: false };
    let routed = route_and_enqueue(st, msg.clone());
    if routed.is_empty() && mandatory {
        let ret = Method::new(m::BASIC_RETURN, vec![Arg::Short(reply::NO_ROUTE), Arg::ShortStr("NO_ROUTE".into()), Arg::ShortStr(exchange), Arg::ShortStr(rk)]);
        if let Some(c) = st.conns.get(&conn) {
            deliver_frames(c, channel, &ret, &msg);
        }
    }
    if let Some(ch) = st.channels.get_mut(&(conn, channel)) {
        if ch.confirm {
            ch.publish_seq += 1;
            let seq = ch.publish_seq;
            send(st, conn, channel, &Method::new(m::BASIC_ACK, vec![Arg::LongLong(seq), Arg::Bit(false)]));
        }
    }
    Ok(())
}

struct PendingPublish {
    method: Method,
    props: BasicProperties,
    size: u64,
    body: Vec<u8>,
    header: bool,
}

fn serve(shared: &Arc<Shared>, stream: NetStream, conn: u64) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let Ok(rs) = stream.try_clone() else { return };
    let mut reader = BufReader::new(rs);
    let writer = Arc::new(Mutex::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    }));
    let write = |m: &Method| {
        let mut w = writer.lock().unwrap();
        write_method(&mut *w, 0, m)
    };
    let mut header = [0u8; 8];
    if reader.read_exact(&mut header).is_err() {
        return;
    }
    if &header != PROTOCOL_HEADER {
        let _ = writer.lock().unwrap().write_all(PROTOCOL_HEADER);
        return;
    }
    let mut caps = Table::new();
    for c in ["publisher_confirms", "exchange_exchange_bindings", "basic.nack", "consumer_cancel_notify", "per_consumer_qos", "authentication_failure_close"] {
        caps.insert(c.into(), Value::Bool(true));
    }
    let mut sp = Table::new();
    sp.insert("product".into(), Value::from("rust-camel embedded AMQP broker"));
    sp.insert("version".into(), Value::from(env!("CARGO_PKG_VERSION")));
    sp.insert("capabilities".into(), Value::Object(caps));
    if write(&Method::new(m::CONNECTION_START, vec![Arg::Octet(0), Arg::Octet(9), Arg::Table(sp), Arg::LongStr(b"PLAIN AMQPLAIN".to_vec()), Arg::LongStr(b"en_US".to_vec())])).is_err() {
        return;
    }
    let next_method = |reader: &mut BufReader<NetStream>| -> Option<Method> {
        loop {
            let f = read_frame(reader, 0).ok()?;
            if f.kind == FRAME_HEARTBEAT {
                continue;
            }
            if f.kind != FRAME_METHOD {
                return None;
            }
            return Method::decode(&f.payload).ok();
        }
    };
    let Some(start_ok) = next_method(&mut reader) else { return };
    let resp = start_ok.bytes(2);
    let parts: Vec<&[u8]> = resp.split(|b| *b == 0).collect();
    let (user, pass) = if parts.len() == 3 { (String::from_utf8_lossy(parts[1]).into_owned(), String::from_utf8_lossy(parts[2]).into_owned()) } else { (String::new(), String::new()) };
    if start_ok.str(1) != "PLAIN" || !shared.users.iter().any(|(u, p)| *u == user && *p == pass) {
        let _ = write(&Method::new(
            m::CONNECTION_CLOSE,
            vec![
                Arg::Short(reply::ACCESS_REFUSED),
                Arg::ShortStr(format!("ACCESS_REFUSED - Login was refused using authentication mechanism PLAIN. User '{}'", user)),
                Arg::Short(0),
                Arg::Short(0),
            ],
        ));
        std::thread::sleep(Duration::from_millis(20));
        return;
    }
    if write(&Method::new(m::CONNECTION_TUNE, vec![Arg::Short(2047), Arg::Long(shared.frame_max), Arg::Short(shared.heartbeat)])).is_err() {
        return;
    }
    let Some(tune_ok) = next_method(&mut reader) else { return };
    let frame_max = tune_ok.num(1) as u32;
    let heartbeat = tune_ok.num(2) as u16;
    let Some(open) = next_method(&mut reader) else { return };
    let vhost = open.str(0);
    if !shared.vhosts.contains(&vhost) {
        let _ = write(&Method::new(m::CONNECTION_CLOSE, vec![Arg::Short(530), Arg::ShortStr(format!("NOT_ALLOWED - vhost {} not found", vhost)), Arg::Short(10), Arg::Short(40)]));
        return;
    }
    if write(&Method::new(m::CONNECTION_OPEN_OK, vec![Arg::ShortStr(String::new())])).is_err() {
        return;
    }
    shared.state.lock().unwrap().conns.insert(conn, Conn { writer: writer.clone(), frame_max });
    let alive = Arc::new(AtomicBool::new(true));
    let hb_thread = if heartbeat > 0 {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(heartbeat as u64 * 2) + Duration::from_millis(500)));
        let (w, a, running) = (writer.clone(), alive.clone(), shared.running.clone());
        std::thread::Builder::new()
            .name(format!("amqp-broker-hb-{}", conn))
            .spawn(move || {
                let step = Duration::from_millis(20);
                let interval = Duration::from_millis(heartbeat as u64 * 500);
                let mut last = Instant::now();
                while a.load(Ordering::SeqCst) && running.load(Ordering::SeqCst) {
                    std::thread::sleep(step);
                    if last.elapsed() >= interval {
                        last = Instant::now();
                        if write_heartbeat(&mut *w.lock().unwrap()).is_err() {
                            break;
                        }
                    }
                }
            })
            .ok()
    } else {
        None
    };
    let mut closing: Vec<u16> = Vec::new();
    let mut pending: HashMap<u16, PendingPublish> = HashMap::new();
    let channel_error = |channel: u16, method: Option<&Method>, e: ChannelError, closing: &mut Vec<u16>| {
        let mut st = shared.state.lock().unwrap();
        let (c, mm) = method.map(|m| (m.class, m.method)).unwrap_or((0, 0));
        send(&st, conn, channel, &Method::new(m::CHANNEL_CLOSE, vec![Arg::Short(e.0), Arg::ShortStr(e.1), Arg::Short(c), Arg::Short(mm)]));
        cleanup_channel(&mut st, conn, channel);
        closing.push(channel);
    };
    while let Ok(f) = read_frame(&mut reader, frame_max) {
        match f.kind {
            FRAME_HEARTBEAT => {
                shared.heartbeats.fetch_add(1, Ordering::SeqCst);
            }
            FRAME_METHOD => {
                let Ok(method) = Method::decode(&f.payload) else { break };
                if f.channel == 0 {
                    if method.is(m::CONNECTION_CLOSE) {
                        let _ = write(&Method::new(m::CONNECTION_CLOSE_OK, vec![]));
                        break;
                    }
                    continue;
                }
                let ch = f.channel;
                if closing.contains(&ch) {
                    if method.is(m::CHANNEL_CLOSE_OK) || method.is(m::CHANNEL_CLOSE) {
                        closing.retain(|c| *c != ch);
                        if method.is(m::CHANNEL_CLOSE) {
                            let st = shared.state.lock().unwrap();
                            send(&st, conn, ch, &Method::new(m::CHANNEL_CLOSE_OK, vec![]));
                        }
                    }
                    continue;
                }
                match method.id() {
                    m::CHANNEL_OPEN => {
                        let mut st = shared.state.lock().unwrap();
                        st.channels.insert((conn, ch), BChan::default());
                        send(&st, conn, ch, &Method::new(m::CHANNEL_OPEN_OK, vec![Arg::LongStr(Vec::new())]));
                    }
                    m::CHANNEL_CLOSE => {
                        let mut st = shared.state.lock().unwrap();
                        cleanup_channel(&mut st, conn, ch);
                        send(&st, conn, ch, &Method::new(m::CHANNEL_CLOSE_OK, vec![]));
                        pending.remove(&ch);
                    }
                    m::CHANNEL_CLOSE_OK => {}
                    m::BASIC_PUBLISH => {
                        pending.insert(ch, PendingPublish { method, props: BasicProperties::default(), size: 0, body: Vec::new(), header: false });
                    }
                    _ => {
                        if let Err(e) = handle_method(shared, conn, ch, &method) {
                            channel_error(ch, Some(&method), e, &mut closing);
                        }
                    }
                }
            }
            FRAME_HEADER => {
                let ch = f.channel;
                let complete = match pending.get_mut(&ch) {
                    Some(p) => match decode_content_header(&f.payload) {
                        Ok((_, size, props)) => {
                            p.size = size;
                            p.props = props;
                            p.header = true;
                            size == 0
                        }
                        Err(_) => break,
                    },
                    None => false,
                };
                if complete {
                    let p = pending.remove(&ch).unwrap();
                    if let Err(e) = handle_publish(shared, conn, ch, &p.method, p.props, p.body) {
                        channel_error(ch, Some(&p.method), e, &mut closing);
                    }
                }
            }
            FRAME_BODY => {
                let ch = f.channel;
                let complete = match pending.get_mut(&ch) {
                    Some(p) if p.header => {
                        p.body.extend_from_slice(&f.payload);
                        p.body.len() as u64 >= p.size
                    }
                    _ => false,
                };
                if complete {
                    let p = pending.remove(&ch).unwrap();
                    if let Err(e) = handle_publish(shared, conn, ch, &p.method, p.props, p.body) {
                        channel_error(ch, Some(&p.method), e, &mut closing);
                    }
                }
            }
            _ => break,
        }
    }
    alive.store(false, Ordering::SeqCst);
    {
        let mut st = shared.state.lock().unwrap();
        cleanup_connection(&mut st, conn);
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
    if let Some(t) = hb_thread {
        let _ = t.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_matching() {
        assert!(topic_matches("orders.*", "orders.eu"));
        assert!(!topic_matches("orders.*", "orders.eu.de"));
        assert!(topic_matches("orders.#", "orders.eu.de"));
        assert!(topic_matches("orders.#", "orders"));
        assert!(topic_matches("#", ""));
        assert!(topic_matches("*.eu.#", "orders.eu"));
        assert!(!topic_matches("*.eu.#", "eu"));
        assert!(topic_matches("a.#.z", "a.b.c.z"));
        assert!(!topic_matches("a.b", "a.c"));
    }

    #[test]
    fn headers_exchange_matching() {
        let mut b = Table::new();
        b.insert("x-match".into(), Value::from("all"));
        b.insert("format".into(), Value::from("pdf"));
        b.insert("type".into(), Value::from("report"));
        let mut h = Table::new();
        h.insert("format".into(), Value::from("pdf"));
        assert!(!headers_match(&b, Some(&h)));
        h.insert("type".into(), Value::from("report"));
        assert!(headers_match(&b, Some(&h)));
        b.insert("x-match".into(), Value::from("any"));
        h.remove("type");
        assert!(headers_match(&b, Some(&h)));
        assert!(!headers_match(&b, None));
    }
}

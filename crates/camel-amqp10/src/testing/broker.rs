//! [`Amqp10Broker`]: an embedded AMQP 1.0 broker for tests.
//!
//! * SASL `PLAIN`, `SCRAM-SHA-256`, `ANONYMOUS` (and `EXTERNAL` over TLS); optional TLS.
//! * Address based nodes: queues (competing consumers, round robin), topics (declared, or
//!   addressed with the `topic` capability: one subscription per receiver, durable
//!   subscriptions for durable terminus), dynamic (temporary) nodes.
//! * Link credit, drain, pre-settled and unsettled deliveries, dispositions
//!   (accepted / rejected / released / modified with redelivery and `delivery-count`),
//!   JMS selectors (`apache.org:selector-filter:string` filters), message expiry,
//!   multi-frame transfers (frames split by the peer's max-frame-size), heartbeats.
//! * Local transactions: a coordinator link (`amqp:coordinator:list` target) declares and
//!   discharges transactions; transactional transfers are buffered and transactional
//!   dispositions held until the commit (a rollback drops the sends and returns the
//!   acquired messages, redelivered; open transactions roll back with their connection).
//! * `rcv-settle-mode = second` (both directions), JMS 2.0 shared subscriptions (`shared` /
//!   `global` capabilities, `name|...` link names), AMQP over WebSockets (an HTTP upgrade
//!   is detected on the same port, subprotocols `AMQPWSB10` / `amqp`).
//! * Azure emulation ([`BrokerBuilder::azure`]): the `$cbs` node validates SAS tokens
//!   (HMAC-SHA256) and links / management requests need a token covering the entity;
//!   Service Bus queues, topics with `topic/subscriptions/name`, peek-lock (lock tokens as
//!   delivery tags, `x-opt-locked-until`, lock expiry), abandon (modified) with
//!   `MaxDeliveryCount`, dead-lettering (rejected) to `entity/$deadletterqueue`,
//!   scheduled messages (`x-opt-scheduled-enqueue-time` and the
//!   `com.microsoft:schedule-message` / `cancel-scheduled-message` / `peek-message`
//!   management operations), sequence numbers and enqueued times; Event Hubs with
//!   partitions (`hub/Partitions/N`, partition keys), consumer groups
//!   (`hub/ConsumerGroups/cg/Partitions/N`), offset / sequence-number / enqueued-time
//!   filters and the `$management` `READ` operations (`com.microsoft:eventhub`,
//!   `com.microsoft:partition`); Azure AD bearer tokens ([`BrokerBuilder::azure_jwt`],
//!   CBS `type=jwt`); Service Bus sessions (session filter, session locks with expiry and
//!   `renew-session-lock`, session state, [`BrokerBuilder::session_entity`]), `renew-lock`,
//!   deferral (`modified` with `undeliverable-here`, `receive-by-sequence-number`,
//!   `update-disposition`); Event Hubs receiver epochs (`com.microsoft:epoch`: a higher or
//!   equal epoch steals the partition with `amqp:link:stolen`).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use camel_amqp::common::TcpServer;
use camel_amqp::selector::Selector;
use camel_core::error::Result;
use camel_core::value::Value;
use camel_tls::net::NetStream;
use camel_tls::TlsServerConfig;

use crate::codec::AmqpValue;
use crate::message::{id_to_string, Body, Message};
use crate::transport::Transport;
use crate::types::*;
use crate::util::{now_millis, parse_sas_token, verify_sas_token};
use crate::ws::AmqpStream;

pub use crate::types::SELECTOR_FILTER;
const SELECTOR_FILTER_CODE: u64 = 0x0000_468C_0000_0004;
const PRODUCER_CREDIT: u32 = 1000;
pub use crate::azure::{EPOCH_PROPERTY, SESSION_FILTER};
use crate::azure::SESSION_FILTER_CODE;

// ---------------------------------------------------------------------------
// configuration
// ---------------------------------------------------------------------------

/// Azure emulation settings.
#[derive(Clone, Debug)]
pub struct AzureConfig {
    /// Namespace host used in connection strings (e.g. `localhost`).
    pub namespace: String,
    /// SAS key name -> key.
    pub keys: HashMap<String, String>,
    /// Accepted Azure AD (`jwt`) bearer tokens.
    pub jwt: HashSet<String>,
}

/// Builder for [`Amqp10Broker`].
pub struct BrokerBuilder {
    users: HashMap<String, String>,
    allow_anonymous: Option<bool>,
    tls: Option<Arc<TlsServerConfig>>,
    azure: Option<AzureConfig>,
    max_frame_size: u32,
    idle_timeout: Duration,
    heartbeats: bool,
    topics: Vec<String>,
    subscriptions: Vec<(String, String)>,
    event_hubs: Vec<(String, u32)>,
    max_delivery_count: u32,
    lock_duration: Duration,
    session_entities: Vec<String>,
}

impl BrokerBuilder {
    /// Require SASL PLAIN / SCRAM with this user (repeatable). Disables anonymous access
    /// unless [`allow_anonymous`](Self::allow_anonymous) is set.
    pub fn user(mut self, user: &str, password: &str) -> Self {
        self.users.insert(user.to_string(), password.to_string());
        self
    }
    pub fn allow_anonymous(mut self, yes: bool) -> Self {
        self.allow_anonymous = Some(yes);
        self
    }
    /// Serve `amqps` (TLS from the first byte).
    pub fn tls(mut self, cfg: TlsServerConfig) -> Self {
        self.tls = Some(Arc::new(cfg));
        self
    }
    /// Our max-frame-size (default 64 KiB; transfers to us are split accordingly).
    pub fn max_frame_size(mut self, n: u32) -> Self {
        self.max_frame_size = n.max(MIN_MAX_FRAME_SIZE);
        self
    }
    /// Our idle timeout (default none).
    pub fn idle_timeout(mut self, d: Duration) -> Self {
        self.idle_timeout = d;
        self
    }
    /// Whether to send heartbeats to peers that ask for them (default true; tests of
    /// the client's idle timeout disable them).
    pub fn heartbeats(mut self, yes: bool) -> Self {
        self.heartbeats = yes;
        self
    }
    /// Declare an address as a (JMS) topic.
    pub fn topic(mut self, name: &str) -> Self {
        self.topics.push(name.to_string());
        self
    }
    /// Azure emulation with one SAS key (repeat [`azure_key`](Self::azure_key) for more).
    pub fn azure(mut self, key_name: &str, key: &str) -> Self {
        let mut keys = HashMap::new();
        keys.insert(key_name.to_string(), key.to_string());
        self.azure = Some(AzureConfig { namespace: "localhost".into(), keys, jwt: HashSet::new() });
        self
    }
    /// Azure emulation: accept this Azure AD bearer token on `$cbs` (`type=jwt`) for any
    /// audience (enables the Azure emulation when not yet enabled).
    pub fn azure_jwt(mut self, token: &str) -> Self {
        let a = self.azure.get_or_insert_with(|| AzureConfig { namespace: "localhost".into(), keys: HashMap::new(), jwt: HashSet::new() });
        a.jwt.insert(token.to_string());
        self
    }
    /// Service Bus: a session-enabled queue (or `topic/subscriptions/name`): receivers
    /// must lock a session with the `com.microsoft:session-filter`.
    pub fn session_entity(mut self, path: &str) -> Self {
        self.session_entities.push(path.to_ascii_lowercase());
        self
    }
    pub fn azure_key(mut self, key_name: &str, key: &str) -> Self {
        if let Some(a) = &mut self.azure {
            a.keys.insert(key_name.to_string(), key.to_string());
        }
        self
    }
    /// Service Bus topic subscription (created up front so messages sent before a
    /// consumer attaches are kept).
    pub fn subscription(mut self, topic: &str, name: &str) -> Self {
        self.subscriptions.push((topic.to_string(), name.to_string()));
        self
    }
    /// Event Hub with `partitions` partitions.
    pub fn event_hub(mut self, name: &str, partitions: u32) -> Self {
        self.event_hubs.push((name.to_string(), partitions.max(1)));
        self
    }
    /// Service Bus MaxDeliveryCount (default 10).
    pub fn max_delivery_count(mut self, n: u32) -> Self {
        self.max_delivery_count = n.max(1);
        self
    }
    /// Service Bus peek-lock duration (default 30 s).
    pub fn lock_duration(mut self, d: Duration) -> Self {
        self.lock_duration = d;
        self
    }

    /// Start on an ephemeral port of 127.0.0.1.
    pub fn start(self) -> Result<Amqp10Broker> {
        let mut st = BrokerState::default();
        for t in &self.topics {
            st.topics.insert(t.clone());
        }
        for (t, s) in &self.subscriptions {
            st.queues.entry(sub_key(t, s)).or_insert_with(Queue::durable);
        }
        st.session_entities = self.session_entities.iter().cloned().collect();
        for (hub, n) in &self.event_hubs {
            st.hubs.insert(hub.to_ascii_lowercase(), *n);
            for p in 0..*n {
                st.logs.insert(format!("{}/{}", hub.to_ascii_lowercase(), p), PartitionLog::default());
            }
        }
        let inner = Arc::new(Inner {
            state: Mutex::new(st),
            cond: Condvar::new(),
            allow_anonymous: self.allow_anonymous.unwrap_or(self.users.is_empty()),
            users: self.users,
            tls: self.tls,
            azure: self.azure,
            max_frame_size: self.max_frame_size,
            idle_timeout: self.idle_timeout,
            heartbeats: self.heartbeats,
            max_delivery_count: self.max_delivery_count,
            lock_duration: self.lock_duration,
            running: AtomicBool::new(true),
        });
        let i2 = inner.clone();
        let server = TcpServer::bind("127.0.0.1", 0, "amqp10-broker", move |tcp, id| serve(&i2, tcp, id))?;
        let i3 = inner.clone();
        let house = std::thread::Builder::new()
            .name("amqp10-broker-housekeeping".into())
            .spawn(move || {
                while i3.running.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(10));
                    let mut st = i3.lock();
                    i3.housekeeping(&mut st);
                }
            })
            .map_err(|e| camel_core::error::CamelError::io(e.to_string()))?;
        Ok(Amqp10Broker { inner, server, house: Mutex::new(Some(house)) })
    }
}

// ---------------------------------------------------------------------------
// state
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Stored {
    msg: Message,
    seq: i64,
    enqueued: i64,
    delivery_count: u32,
}

#[derive(Default)]
struct Queue {
    messages: VecDeque<Stored>,
    /// Topic subscription selector (applied when publishing).
    selector: Option<Selector>,
    /// Owner link of a temporary node / non-durable subscription (deleted with it).
    owner: Option<LinkKey>,
    /// JMS topic of a subscription queue.
    topic: Option<String>,
}

impl Queue {
    fn durable() -> Queue {
        Queue::default()
    }
}

#[derive(Default)]
struct PartitionLog {
    events: Vec<(i64, Stored)>,
    next_offset: i64,
}

type LinkKey = (u64, u16, u32);

#[derive(Clone)]
enum LinkKind {
    /// Client sends to this address.
    Producer(String),
    /// Client receives from a queue key.
    Consumer(String),
    /// Client receives from an Event Hubs partition log (`key` = hub/partition).
    Log { key: String, cursor: usize, group: String },
    /// Client declares and discharges transactions.
    Coordinator,
}

struct BLink {
    kind: LinkKind,
    credit: u32,
    delivery_count: u32,
    presettle: bool,
    selector: Option<Selector>,
    /// Delivery being received: id, settled, payload, transaction id.
    partial: Option<(u32, bool, Vec<u8>, Option<Vec<u8>>)>,
    /// Receivers on management nodes: the target address (reply-to).
    reply_address: Option<String>,
    /// Peek-lock Service Bus receiver.
    peek_lock: bool,
    /// `rcv-settle-mode = second`.
    rcv_second: bool,
    /// Service Bus session this receiver locked.
    session: Option<String>,
    /// Event Hubs receiver epoch.
    epoch: Option<i64>,
}

struct BSession {
    next_outgoing_id: u32,
    next_delivery_id: u32,
    links: HashMap<u32, BLink>,
}

struct Conn {
    transport: Arc<Transport>,
    container_id: String,
    max_frame_out: u32,
    sessions: HashMap<u16, BSession>,
    /// Audience paths authorized through `$cbs`.
    authorized: Vec<String>,
}

struct Inflight {
    queue: String,
    handle: u32,
    stored: Stored,
    locked_until: Option<i64>,
    /// Service Bus lock token (RFC 4122 byte order).
    lock_token: Option<[u8; 16]>,
}

/// An open transaction: buffered sends and transactional settlements.
#[derive(Default)]
struct Txn {
    conn: u64,
    sends: Vec<(u16, String, Message)>,
    settles: Vec<(Inflight, DeliveryState)>,
}

#[derive(Default)]
struct BrokerState {
    conns: HashMap<u64, Conn>,
    queues: BTreeMap<String, Queue>,
    topics: std::collections::HashSet<String>,
    hubs: HashMap<String, u32>,
    logs: BTreeMap<String, PartitionLog>,
    next_seq: i64,
    temp_counter: u64,
    rr: usize,
    hub_rr: usize,
    scheduled: Vec<(i64, String, Stored)>,
    inflight: HashMap<(u64, u16, u32), Inflight>,
    txns: HashMap<Vec<u8>, Txn>,
    next_txn: u64,
    committed_txns: u64,
    rolled_back_txns: u64,
    session_entities: HashSet<String>,
    /// (entity, session id) -> (locking link, locked until).
    session_locks: HashMap<(String, String), (LinkKey, i64)>,
    session_states: HashMap<(String, String), Vec<u8>>,
    /// Deferred messages per entity.
    deferred: HashMap<String, Vec<Stored>>,
    /// Deferred messages received in peek-lock mode: lock token -> (entity, message, locked until).
    deferred_locked: HashMap<[u8; 16], (String, Stored, i64)>,
}

struct Inner {
    state: Mutex<BrokerState>,
    cond: Condvar,
    users: HashMap<String, String>,
    allow_anonymous: bool,
    tls: Option<Arc<TlsServerConfig>>,
    azure: Option<AzureConfig>,
    max_frame_size: u32,
    idle_timeout: Duration,
    heartbeats: bool,
    max_delivery_count: u32,
    lock_duration: Duration,
    running: AtomicBool,
}

fn sub_key(topic: &str, sub: &str) -> String {
    format!("{}/subscriptions/{}", topic.to_ascii_lowercase(), sub.to_ascii_lowercase())
}

const DLQ: &str = "$deadletterqueue";

// ---------------------------------------------------------------------------
// the broker handle
// ---------------------------------------------------------------------------

/// An embedded AMQP 1.0 broker (see the module docs). Stopped on drop.
pub struct Amqp10Broker {
    inner: Arc<Inner>,
    server: TcpServer,
    house: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Amqp10Broker {
    pub fn builder() -> BrokerBuilder {
        BrokerBuilder {
            users: HashMap::new(),
            allow_anonymous: None,
            tls: None,
            azure: None,
            max_frame_size: 65536,
            idle_timeout: Duration::ZERO,
            heartbeats: true,
            topics: Vec::new(),
            subscriptions: Vec::new(),
            event_hubs: Vec::new(),
            max_delivery_count: 10,
            lock_duration: Duration::from_secs(30),
            session_entities: Vec::new(),
        }
    }
    /// Start with defaults (anonymous access).
    pub fn start() -> Result<Amqp10Broker> {
        Self::builder().start()
    }
    pub fn port(&self) -> u16 {
        self.server.port()
    }
    /// `amqp://127.0.0.1:port` (or `amqps://localhost:port` with TLS).
    pub fn url(&self) -> String {
        if self.inner.tls.is_some() {
            format!("amqps://localhost:{}", self.port())
        } else {
            format!("amqp://127.0.0.1:{}", self.port())
        }
    }
    /// An Azure connection string for this broker (`UseDevelopmentEmulator=true` without TLS).
    pub fn connection_string(&self, key_name: &str, key: &str) -> String {
        if self.inner.tls.is_some() {
            format!("Endpoint=sb://localhost:{}/;SharedAccessKeyName={};SharedAccessKey={}", self.port(), key_name, key)
        } else {
            format!("Endpoint=sb://127.0.0.1:{}/;SharedAccessKeyName={};SharedAccessKey={};UseDevelopmentEmulator=true", self.port(), key_name, key)
        }
    }
    /// Number of messages waiting in a queue (or subscription / dead-letter queue path).
    pub fn queue_depth(&self, address: &str) -> usize {
        let st = self.inner.lock();
        st.queues.get(&address.to_ascii_lowercase()).map(|q| q.messages.len()).unwrap_or(0)
    }
    /// The messages waiting in a queue.
    pub fn queue_messages(&self, address: &str) -> Vec<Message> {
        let st = self.inner.lock();
        st.queues.get(&address.to_ascii_lowercase()).map(|q| q.messages.iter().map(|s| s.msg.clone()).collect()).unwrap_or_default()
    }
    /// Number of unsettled deliveries.
    pub fn inflight_count(&self) -> usize {
        self.inner.lock().inflight.len()
    }
    /// Number of scheduled (not yet enqueued) messages.
    pub fn scheduled_count(&self) -> usize {
        self.inner.lock().scheduled.len()
    }
    /// Number of consumer links reading from a queue key.
    pub fn consumer_count(&self, address: &str) -> usize {
        let key = address.to_ascii_lowercase();
        let st = self.inner.lock();
        st.conns.values().flat_map(|c| c.sessions.values()).flat_map(|s| s.links.values()).filter(|l| matches!(&l.kind, LinkKind::Consumer(k) if *k == key)).count()
    }
    /// Number of receivers subscribed to a topic.
    pub fn subscriber_count(&self, topic: &str) -> usize {
        let st = self.inner.lock();
        st.queues.values().filter(|q| q.topic.as_deref() == Some(topic)).count()
    }
    /// Number of events stored in an Event Hubs partition.
    pub fn partition_size(&self, hub: &str, partition: u32) -> usize {
        let st = self.inner.lock();
        st.logs.get(&format!("{}/{}", hub.to_ascii_lowercase(), partition)).map(|l| l.events.len()).unwrap_or(0)
    }
    /// Number of open connections.
    pub fn connection_count(&self) -> usize {
        self.inner.lock().conns.len()
    }
    /// Number of open (undischarged) transactions.
    pub fn open_transactions(&self) -> usize {
        self.inner.lock().txns.len()
    }
    /// Committed and rolled back transactions so far.
    pub fn transaction_counts(&self) -> (u64, u64) {
        let st = self.inner.lock();
        (st.committed_txns, st.rolled_back_txns)
    }
    /// Number of deferred messages of a Service Bus entity.
    pub fn deferred_count(&self, entity: &str) -> usize {
        let st = self.inner.lock();
        let key = entity.to_ascii_lowercase();
        st.deferred.get(&key).map(|v| v.len()).unwrap_or(0) + st.deferred_locked.values().filter(|(e, _, _)| *e == key).count()
    }
    /// The state stored for a Service Bus session.
    pub fn session_state(&self, entity: &str, session: &str) -> Option<Vec<u8>> {
        self.inner.lock().session_states.get(&(entity.to_ascii_lowercase(), session.to_string())).cloned()
    }
    /// The Service Bus session ids currently locked on an entity.
    pub fn locked_sessions(&self, entity: &str) -> Vec<String> {
        let key = entity.to_ascii_lowercase();
        let mut v: Vec<String> = self.inner.lock().session_locks.keys().filter(|(e, _)| *e == key).map(|(_, s)| s.clone()).collect();
        v.sort();
        v
    }
    /// The lock expiry (epoch millis) of the unsettled peek-lock deliveries of an entity.
    pub fn lock_expirations(&self, entity: &str) -> Vec<i64> {
        let key = entity.to_ascii_lowercase();
        self.inner.lock().inflight.values().filter(|i| i.queue == key).filter_map(|i| i.locked_until).collect()
    }
    /// Event Hubs: the receivers attached to a partition of a consumer group, as their epochs.
    pub fn partition_receivers(&self, hub: &str, group: &str, partition: u32) -> Vec<Option<i64>> {
        let key = format!("{}/{}", hub.to_ascii_lowercase(), partition);
        let group = group.to_ascii_lowercase();
        let st = self.inner.lock();
        st.conns
            .values()
            .flat_map(|c| c.sessions.values())
            .flat_map(|s| s.links.values())
            .filter(|l| matches!(&l.kind, LinkKind::Log { key: k, group: g, .. } if *k == key && *g == group))
            .map(|l| l.epoch)
            .collect()
    }
    /// Wait until `f` holds (polls every 5 ms, up to `timeout`).
    pub fn wait_for(&self, timeout: Duration, f: impl Fn(&Amqp10Broker) -> bool) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if f(self) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        f(self)
    }
    /// Close every client connection abruptly.
    pub fn drop_connections(&self) {
        self.server.drop_connections();
    }
    /// Stop the broker.
    pub fn stop(&self) {
        self.inner.running.store(false, Ordering::SeqCst);
        self.server.stop();
        if let Some(h) = self.house.lock().unwrap().take() {
            let _ = h.join();
        }
    }
}

impl Drop for Amqp10Broker {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------------------
// connection handling
// ---------------------------------------------------------------------------

fn serve(inner: &Arc<Inner>, tcp: TcpStream, id: u64) {
    let _ = tcp.set_read_timeout(Some(Duration::from_secs(30)));
    let stream = match &inner.tls {
        Some(cfg) => match NetStream::server(tcp, cfg) {
            Ok(s) => s,
            Err(_) => return,
        },
        None => NetStream::plain(tcp),
    };
    let _ = serve_conn(inner, stream, id);
    inner.cleanup_conn(id);
}

fn serve_conn(inner: &Arc<Inner>, net: NetStream, id: u64) -> std::io::Result<()> {
    use std::io::Write;
    let mut stream = AmqpStream::from(net);
    let mut header = read_protocol_header(&mut stream)?;
    if header.starts_with(b"GET ") {
        // AMQP over WebSockets: answer the upgrade, then read the protocol header from it
        let net = stream.net().try_clone()?;
        stream = AmqpStream::websocket_server(net, &header)?;
        header = read_protocol_header(&mut stream)?;
    }
    let sasl_required = !inner.users.is_empty() || !inner.allow_anonymous;
    if header == SASL_HEADER {
        stream.write_all(&SASL_HEADER)?;
        if !sasl_server(inner, &mut stream)? {
            return Ok(());
        }
        header = read_protocol_header(&mut stream)?;
    } else if header == AMQP_HEADER && sasl_required {
        stream.write_all(&SASL_HEADER)?;
        return Ok(());
    }
    if header != AMQP_HEADER {
        stream.write_all(&AMQP_HEADER)?;
        return Ok(());
    }
    stream.write_all(&AMQP_HEADER)?;
    let _ = stream.set_read_timeout(None);
    let transport = Transport::new(stream)?;
    let mut reader = transport.reader()?;
    {
        let mut st = inner.lock();
        st.conns.insert(id, Conn { transport: transport.clone(), container_id: String::new(), max_frame_out: MIN_MAX_FRAME_SIZE, sessions: HashMap::new(), authorized: Vec::new() });
    }
    let t2 = transport.clone();
    transport.spawn_heartbeat(&format!("amqp10-broker-hb-{}", id), move || {
        let _ = t2.write_frame(&Frame::amqp(0, Performative::Close(Some(AmqpError::new(cond::RESOURCE_LIMIT_EXCEEDED, "local-idle-timeout expired")))));
    });
    transport.local_idle_ms.store(inner.idle_timeout.as_millis() as u64, Ordering::Relaxed);
    loop {
        let f = match read_frame(&mut reader, inner.max_frame_size) {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::InvalidData {
                    let _ = transport.write_frame(&Frame::amqp(0, Performative::Close(Some(AmqpError::new(cond::FRAMING_ERROR, e.to_string())))));
                }
                break;
            }
        };
        transport.touch_read();
        let mut st = inner.lock();
        match inner.handle(&mut st, id, f) {
            Ok(true) => {}
            Ok(false) => break,
            Err(e) => {
                let _ = transport.write_frame(&Frame::amqp(0, Performative::Close(Some(e))));
                break;
            }
        }
        drop(st);
        inner.cond.notify_all();
    }
    transport.close();
    Ok(())
}

fn sasl_server(inner: &Inner, stream: &mut AmqpStream) -> std::io::Result<bool> {
    use std::io::Write;
    let mut mechs: Vec<String> = Vec::new();
    if !inner.users.is_empty() {
        mechs.push("SCRAM-SHA-256".into());
        mechs.push("PLAIN".into());
    }
    if inner.allow_anonymous {
        if inner.users.is_empty() {
            mechs.push("PLAIN".into());
        }
        mechs.push("ANONYMOUS".into());
        if inner.tls.is_some() {
            mechs.push("EXTERNAL".into());
        }
    }
    let send = |s: &mut AmqpStream, f: SaslFrame| s.write_all(&Frame { channel: 0, body: FrameBody::Sasl(f) }.encode());
    send(stream, SaslFrame::Mechanisms(mechs.clone()))?;
    let next = |s: &mut AmqpStream| -> std::io::Result<Option<SaslFrame>> {
        Ok(match read_frame(s, 65536)? {
            Some(Frame { body: FrameBody::Sasl(f), .. }) => Some(f),
            _ => None,
        })
    };
    let Some(SaslFrame::Init { mechanism, initial_response, .. }) = next(stream)? else { return Ok(false) };
    let ok = if !mechs.iter().any(|m| m == &mechanism) {
        false
    } else {
        match mechanism.as_str() {
            "ANONYMOUS" | "EXTERNAL" => true,
            "PLAIN" => match camel_tls::sasl::parse_plain(&initial_response.unwrap_or_default()) {
                Some((_, u, p)) => inner.users.is_empty() || inner.users.get(&u) == Some(&p),
                None => false,
            },
            "SCRAM-SHA-256" => {
                let mech = camel_tls::sasl::ScramMechanism::from_name("SCRAM-SHA-256").unwrap();
                let users = inner.users.clone();
                let mut srv = camel_tls::sasl::ScramServer::new(mech, move |u: &str| users.get(u).map(|p| camel_tls::sasl::ScramCredentials::derive(mech, p, b"camel-amqp10-salt", 4096)));
                let first = String::from_utf8_lossy(&initial_response.unwrap_or_default()).to_string();
                match srv.handle_client_first(&first) {
                    Ok(server_first) => {
                        send(stream, SaslFrame::Challenge(server_first.into_bytes()))?;
                        match next(stream)? {
                            Some(SaslFrame::Response(r)) => match srv.handle_client_final(&String::from_utf8_lossy(&r)) {
                                Ok(server_final) => {
                                    send(stream, SaslFrame::Outcome { code: 0, additional_data: Some(server_final.into_bytes()) })?;
                                    return Ok(true);
                                }
                                Err(_) => false,
                            },
                            _ => false,
                        }
                    }
                    Err(_) => false,
                }
            }
            _ => false,
        }
    };
    send(stream, SaslFrame::Outcome { code: if ok { 0 } else { 1 }, additional_data: None })?;
    Ok(ok)
}

/// Lookup of JMS header names and application properties for selectors.
fn selector_lookup(s: &Stored) -> impl Fn(&str) -> Option<Value> + '_ {
    move |name: &str| {
        let p = s.msg.properties.as_ref();
        match name {
            "JMSMessageID" => p.and_then(|p| p.message_id.as_ref()).map(|v| Value::String(id_to_string(v))),
            "JMSCorrelationID" => p.and_then(|p| p.correlation_id.as_ref()).map(|v| Value::String(id_to_string(v))),
            "JMSType" => p.and_then(|p| p.subject.clone()).map(Value::String),
            "JMSPriority" => Some(Value::Int(s.msg.header.as_ref().map(|h| h.priority as i64).unwrap_or(4))),
            "JMSDeliveryMode" => Some(Value::String(if s.msg.header.as_ref().is_some_and(|h| h.durable) { "PERSISTENT" } else { "NON_PERSISTENT" }.into())),
            "JMSTimestamp" => p.and_then(|p| p.creation_time).map(Value::Int),
            "JMSXGroupID" => p.and_then(|p| p.group_id.clone()).map(Value::String),
            "JMSXDeliveryCount" => Some(Value::Int(s.delivery_count as i64 + 1)),
            other => s.msg.app_property(other).map(crate::util::to_camel),
        }
    }
}

fn selector_of(source: &Source) -> std::result::Result<Option<Selector>, AmqpError> {
    for (_, v) in &source.filter {
        let is_sel = v.descriptor_symbol() == Some(SELECTOR_FILTER) || v.descriptor_code() == Some(SELECTOR_FILTER_CODE);
        if is_sel {
            if let Some(text) = v.undescribed().as_str() {
                if text.contains("amqp.annotation.") {
                    continue;
                }
                return Selector::parse(text).map(Some).map_err(|e| AmqpError::new(cond::INVALID_FIELD, format!("invalid selector: {}", e.message)));
            }
        }
    }
    Ok(None)
}

/// Event Hubs position filter: `amqp.annotation.x-opt-offset > '12'` etc.
fn event_position(source: &Source, log: &PartitionLog) -> usize {
    for (_, v) in &source.filter {
        let Some(text) = v.undescribed().as_str() else { continue };
        let Some(rest) = text.trim().strip_prefix("amqp.annotation.") else { continue };
        let (field, rest) = match rest.find([' ', '>', '=']) {
            Some(i) => (&rest[..i], rest[i..].trim()),
            None => continue,
        };
        let (op, val) = if let Some(v) = rest.strip_prefix(">=") {
            (">=", v)
        } else if let Some(v) = rest.strip_prefix('>') {
            (">", v)
        } else if let Some(v) = rest.strip_prefix('=') {
            ("=", v)
        } else {
            continue;
        };
        let val = val.trim().trim_matches('\'');
        if val == "@latest" {
            return log.events.len();
        }
        let Ok(n) = val.parse::<i64>() else { continue };
        let key = |(off, s): &(i64, Stored)| -> i64 {
            match field {
                "x-opt-offset" => *off,
                "x-opt-sequence-number" => s.seq,
                _ => s.enqueued,
            }
        };
        return log.events.iter().position(|e| if op == ">" { key(e) > n } else { key(e) >= n }).unwrap_or(log.events.len());
    }
    log.events.len()
}

fn reply(status: i64, desc: &str, body: Body) -> Message {
    let mut m = Message { body, ..Default::default() };
    m.set_app_property("status-code", AmqpValue::Int(status as i32));
    m.set_app_property("status-description", AmqpValue::str(desc));
    m.set_app_property("statusCode", AmqpValue::Int(status as i32));
    m
}

fn path_of(audience: &str) -> String {
    let rest = audience.split_once("://").map(|(_, r)| r).unwrap_or(audience);
    let path = rest.split_once('/').map(|(_, p)| p).unwrap_or("");
    path.trim_matches('/').to_ascii_lowercase()
}

fn covers(authorized: &str, entity: &str) -> bool {
    let e = entity.trim_matches('/').to_ascii_lowercase();
    authorized.is_empty() || e == authorized || e.starts_with(&format!("{}/", authorized))
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, BrokerState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn azure(&self) -> bool {
        self.azure.is_some()
    }

    fn write(st: &BrokerState, conn: u64, f: Frame) {
        if let Some(c) = st.conns.get(&conn) {
            let _ = c.transport.write_frame(&f);
        }
    }

    /// Process a frame; `Ok(false)` ends the connection.
    fn handle(&self, st: &mut BrokerState, cid: u64, f: Frame) -> std::result::Result<bool, AmqpError> {
        let ch = f.channel;
        let (p, payload) = match f.body {
            FrameBody::Empty => return Ok(true),
            FrameBody::Sasl(_) => return Err(AmqpError::new(cond::FRAMING_ERROR, "unexpected SASL frame")),
            FrameBody::Amqp(p, payload) => (p, payload),
        };
        match p {
            Performative::Open(o) => {
                let c = st.conns.get_mut(&cid).unwrap();
                c.container_id = o.container_id.clone();
                c.max_frame_out = o.max_frame_size.max(MIN_MAX_FRAME_SIZE);
                if self.heartbeats {
                    c.transport.remote_idle_ms.store(o.idle_time_out as u64, Ordering::Relaxed);
                }
                let open = Open {
                    container_id: "camel-amqp10-broker".into(),
                    hostname: None,
                    max_frame_size: self.max_frame_size,
                    channel_max: 255,
                    idle_time_out: self.idle_timeout.as_millis() as u32,
                    offered_capabilities: vec!["ANONYMOUS-RELAY".into()],
                    ..Default::default()
                };
                Self::write(st, cid, Frame::amqp(0, Performative::Open(open)));
            }
            Performative::Begin(_) => {
                st.conns.get_mut(&cid).unwrap().sessions.insert(ch, BSession { next_outgoing_id: 0, next_delivery_id: 0, links: HashMap::new() });
                let b = Begin { remote_channel: Some(ch), next_outgoing_id: 0, incoming_window: u32::MAX >> 1, outgoing_window: u32::MAX >> 1, handle_max: u32::MAX, properties: vec![] };
                Self::write(st, cid, Frame::amqp(ch, Performative::Begin(b)));
            }
            Performative::End(_) => {
                let handles: Vec<u32> = st.conns.get(&cid).and_then(|c| c.sessions.get(&ch)).map(|s| s.links.keys().copied().collect()).unwrap_or_default();
                for h in handles {
                    self.remove_link(st, (cid, ch, h));
                }
                st.conns.get_mut(&cid).unwrap().sessions.remove(&ch);
                Self::write(st, cid, Frame::amqp(ch, Performative::End(None)));
                self.dispatch(st);
            }
            Performative::Close(_) => {
                Self::write(st, cid, Frame::amqp(0, Performative::Close(None)));
                return Ok(false);
            }
            Performative::Attach(a) => self.attach(st, cid, ch, a)?,
            Performative::Detach(d) => {
                self.remove_link(st, (cid, ch, d.handle));
                Self::write(st, cid, Frame::amqp(ch, Performative::Detach(Detach { handle: d.handle, closed: true, error: None })));
                self.dispatch(st);
            }
            Performative::Flow(f) => {
                if let Some(h) = f.handle {
                    let mut echo = None;
                    if let Some(l) = st.conns.get_mut(&cid).and_then(|c| c.sessions.get_mut(&ch)).and_then(|s| s.links.get_mut(&h)) {
                        if !matches!(l.kind, LinkKind::Producer(_)) {
                            let dc = f.delivery_count.unwrap_or(l.delivery_count);
                            let c = dc.wrapping_add(f.link_credit.unwrap_or(0)).wrapping_sub(l.delivery_count);
                            l.credit = if c > u32::MAX >> 1 { 0 } else { c };
                        }
                        if f.echo {
                            echo = Some(h);
                        }
                    }
                    self.dispatch(st);
                    if f.drain {
                        let drained = st.conns.get_mut(&cid).and_then(|c| c.sessions.get_mut(&ch)).and_then(|s| s.links.get_mut(&h)).map(|l| {
                            l.delivery_count = l.delivery_count.wrapping_add(l.credit);
                            l.credit = 0;
                        });
                        if drained.is_some() {
                            echo = Some(h);
                        }
                    }
                    if let Some(h) = echo {
                        self.send_link_flow(st, cid, ch, h);
                    }
                }
            }
            Performative::Transfer(t) => self.transfer(st, cid, ch, t, payload)?,
            Performative::Disposition(d) => {
                if d.role == Role::Receiver {
                    let last = d.last.unwrap_or(d.first);
                    let mut id = d.first;
                    loop {
                        if let Some(inf) = st.inflight.remove(&(cid, ch, id)) {
                            match d.state.clone().unwrap_or(DeliveryState::Accepted) {
                                DeliveryState::Transactional { txn_id, outcome } => {
                                    let outcome = outcome.map(|o| *o).unwrap_or(DeliveryState::Accepted);
                                    match st.txns.get_mut(&txn_id) {
                                        Some(tx) => tx.settles.push((inf, outcome)),
                                        None => self.settle(st, inf, outcome),
                                    }
                                }
                                state => self.settle(st, inf, state),
                            }
                        }
                        if id == last {
                            break;
                        }
                        id = id.wrapping_add(1);
                    }
                    if !d.settled {
                        // the receiver settles second: settle as the sender, then it settles
                        Self::write(st, cid, Frame::amqp(ch, Performative::Disposition(Disposition { role: Role::Sender, first: d.first, last: d.last, settled: true, state: d.state.clone(), batchable: false })));
                    }
                    self.dispatch(st);
                }
            }
        }
        Ok(true)
    }

    fn send_link_flow(&self, st: &BrokerState, cid: u64, ch: u16, h: u32) {
        let Some(c) = st.conns.get(&cid) else { return };
        let Some(s) = c.sessions.get(&ch) else { return };
        let Some(l) = s.links.get(&h) else { return };
        let f = Flow {
            next_incoming_id: Some(0),
            incoming_window: u32::MAX >> 1,
            next_outgoing_id: s.next_outgoing_id,
            outgoing_window: u32::MAX >> 1,
            handle: Some(h),
            delivery_count: Some(l.delivery_count),
            link_credit: Some(l.credit),
            available: Some(0),
            drain: false,
            echo: false,
            properties: vec![],
        };
        let _ = c.transport.write_frame(&Frame::amqp(ch, Performative::Flow(f)));
    }

    fn refuse(&self, st: &BrokerState, cid: u64, ch: u16, a: &Attach, e: AmqpError) {
        let resp = Attach {
            name: a.name.clone(),
            handle: a.handle,
            role: if a.role == Role::Sender { Role::Receiver } else { Role::Sender },
            snd_settle_mode: a.snd_settle_mode,
            rcv_settle_mode: a.rcv_settle_mode,
            source: None,
            target: None,
            initial_delivery_count: if a.role == Role::Receiver { Some(0) } else { None },
            max_message_size: None,
            offered_capabilities: vec![],
            desired_capabilities: vec![],
            properties: vec![],
        };
        Self::write(st, cid, Frame::amqp(ch, Performative::Attach(resp)));
        Self::write(st, cid, Frame::amqp(ch, Performative::Detach(Detach { handle: a.handle, closed: true, error: Some(e) })));
    }

    /// Azure: whether the connection holds a token covering `entity`.
    fn authorized(&self, st: &BrokerState, cid: u64, entity: &str) -> bool {
        if !self.azure() {
            return true;
        }
        let e = entity.trim_matches('/');
        if e == "$cbs" || e == "$management" || e.is_empty() {
            return true;
        }
        st.conns.get(&cid).is_some_and(|c| c.authorized.iter().any(|a| covers(a, e)))
    }

    fn is_management(addr: &str) -> bool {
        addr == "$cbs" || addr == "$management" || addr.ends_with("/$management")
    }

    fn attach(&self, st: &mut BrokerState, cid: u64, ch: u16, a: Attach) -> std::result::Result<(), AmqpError> {
        if !st.conns.get(&cid).is_some_and(|c| c.sessions.contains_key(&ch)) {
            return Err(AmqpError::new(cond::INVALID_FIELD, "attach on an unknown session"));
        }
        let key: LinkKey = (cid, ch, a.handle);
        let mut source = a.source.clone();
        let mut target = a.target.clone();
        let mut presettle = false;
        let mut peek_lock = false;
        let mut selector = None;
        let mut reply_address = None;
        let mut session: Option<String> = None;
        let mut epoch: Option<i64> = None;
        let mut resp_properties: Vec<(AmqpValue, AmqpValue)> = Vec::new();
        let kind = match a.role {
            Role::Sender => {
                // client sends: we receive from it
                let Some(t) = &a.target else {
                    self.refuse(st, cid, ch, &a, AmqpError::new(cond::INVALID_FIELD, "target required"));
                    return Ok(());
                };
                if t.coordinator {
                    LinkKind::Coordinator
                } else {
                let addr = if t.dynamic {
                    st.temp_counter += 1;
                    let name = format!("temp-queue-{}-{}", cid, st.temp_counter);
                    st.queues.insert(name.clone(), Queue { owner: Some(key), ..Default::default() });
                    target.as_mut().unwrap().address = Some(name.clone());
                    name
                } else {
                    t.address.clone().unwrap_or_default()
                };
                let entity = addr.trim_end_matches("/$management").to_string();
                if !addr.is_empty() && !self.authorized(st, cid, &entity) {
                    self.refuse(st, cid, ch, &a, AmqpError::new(cond::UNAUTHORIZED_ACCESS, format!("Unauthorized access to '{}': put a token on $cbs first", addr)));
                    return Ok(());
                }
                if self.azure() && !Self::is_management(&addr) {
                    if let Some(Err(err)) = self.hub_partition_for_send(st, &addr) {
                        self.refuse(st, cid, ch, &a, err);
                        return Ok(());
                    }
                }
                if t.capabilities.iter().any(|c| c == "topic") {
                    st.topics.insert(addr.clone());
                }
                LinkKind::Producer(addr)
                }
            }
            Role::Receiver => {
                let Some(s) = &a.source else {
                    self.refuse(st, cid, ch, &a, AmqpError::new(cond::INVALID_FIELD, "source required"));
                    return Ok(());
                };
                presettle = a.snd_settle_mode == SndSettleMode::Settled;
                if s.dynamic {
                    st.temp_counter += 1;
                    let name = format!("temp-queue-{}-{}", cid, st.temp_counter);
                    st.queues.insert(name.clone(), Queue { owner: Some(key), ..Default::default() });
                    source.as_mut().unwrap().address = Some(name.clone());
                    LinkKind::Consumer(name)
                } else {
                    let addr = s.address.clone().unwrap_or_default();
                    if Self::is_management(&addr) {
                        reply_address = a.target.as_ref().and_then(|t| t.address.clone());
                        presettle = true;
                        let name = format!("#reply-{}-{}-{}", cid, ch, a.handle);
                        st.queues.insert(name.clone(), Queue { owner: Some(key), ..Default::default() });
                        LinkKind::Consumer(name)
                    } else {
                        if !self.authorized(st, cid, &addr) {
                            self.refuse(st, cid, ch, &a, AmqpError::new(cond::UNAUTHORIZED_ACCESS, format!("Unauthorized access to '{}': put a token on $cbs first", addr)));
                            return Ok(());
                        }
                        match selector_of(s) {
                            Ok(sel) => selector = sel,
                            Err(e) => {
                                self.refuse(st, cid, ch, &a, e);
                                return Ok(());
                            }
                        }
                        let lower = addr.to_ascii_lowercase();
                        let parts: Vec<&str> = lower.split('/').collect();
                        if self.azure() && parts.len() == 5 && parts[1] == "consumergroups" && parts[3] == "partitions" {
                            // Event Hubs consumer
                            let log_key = format!("{}/{}", parts[0], parts[4]);
                            let Some(log) = st.logs.get(&log_key) else {
                                self.refuse(st, cid, ch, &a, AmqpError::new(cond::NOT_FOUND, format!("Event hub partition '{}' not found", addr)));
                                return Ok(());
                            };
                            presettle = true;
                            let cursor = event_position(s, log);
                            let group = parts[2].to_string();
                            epoch = a.properties.iter().find(|(k, _)| k.as_str() == Some(EPOCH_PROPERTY)).and_then(|(_, v)| v.as_i64());
                            if let Err(e) = self.claim_epoch(st, &log_key, &group, epoch) {
                                self.refuse(st, cid, ch, &a, e);
                                return Ok(());
                            }
                            LinkKind::Log { cursor, key: log_key, group }
                        } else if self.azure() {
                            let entity = lower.trim_end_matches(&format!("/{}", DLQ)).to_string();
                            let session_filter = s.filter.iter().map(|(_, v)| v).find(|v| v.descriptor_symbol() == Some(SESSION_FILTER) || v.descriptor_code() == Some(SESSION_FILTER_CODE));
                            match session_filter {
                                Some(f) => {
                                    let requested = f.undescribed().as_str().map(String::from);
                                    match self.lock_session(st, &entity, requested, key) {
                                        Ok((sid, until)) => {
                                            if let Some(src) = source.as_mut() {
                                                for (k, v) in src.filter.iter_mut() {
                                                    if v.descriptor_symbol() == Some(SESSION_FILTER) || v.descriptor_code() == Some(SESSION_FILTER_CODE) {
                                                        *k = AmqpValue::sym(SESSION_FILTER);
                                                        *v = AmqpValue::Described(Box::new(AmqpValue::sym(SESSION_FILTER)), Box::new(AmqpValue::str(sid.clone())));
                                                    }
                                                }
                                            }
                                            resp_properties.push((AmqpValue::sym("com.microsoft:locked-until-utc"), AmqpValue::Timestamp(until)));
                                            session = Some(sid);
                                        }
                                        Err(e) => {
                                            self.refuse(st, cid, ch, &a, e);
                                            return Ok(());
                                        }
                                    }
                                }
                                None if st.session_entities.contains(&entity) && !lower.ends_with(DLQ) => {
                                    self.refuse(st, cid, ch, &a, AmqpError::new(cond::NOT_ALLOWED, format!("It is not possible for an entity that requires sessions to create a non-sessionful message receiver: {}", entity)));
                                    return Ok(());
                                }
                                None => {}
                            }
                            if parts.len() >= 3 && parts[1] == "subscriptions" {
                                st.queues.entry(lower.clone()).or_default();
                            } else if st.hubs.contains_key(parts[0]) {
                                self.refuse(st, cid, ch, &a, AmqpError::new(cond::NOT_FOUND, format!("Invalid Event Hubs receiver address '{}'", addr)));
                                return Ok(());
                            } else {
                                st.queues.entry(lower.clone()).or_default();
                            }
                            peek_lock = !presettle;
                            LinkKind::Consumer(lower)
                        } else if st.topics.contains(&addr) || s.capabilities.iter().any(|c| c == "topic") {
                            st.topics.insert(addr.clone());
                            let durable = s.durable > 0 || s.expiry_policy.as_deref() == Some("never");
                            let shared = s.capabilities.iter().any(|c| c == "shared");
                            let container = st.conns.get(&cid).map(|c| c.container_id.clone()).unwrap_or_default();
                            let name = if shared {
                                // JMS 2.0 shared subscriptions: link names `name`, `name|global`, `name|volatileN`...
                                let sub = a.name.split('|').next().unwrap_or(&a.name);
                                let scope = if s.capabilities.iter().any(|c| c == "global") { String::new() } else { container };
                                format!("#shared-{}-{}-{}", if durable { "d" } else { "v" }, scope, sub)
                            } else if durable {
                                format!("#durable-{}-{}", container, a.name)
                            } else {
                                format!("#sub-{}-{}-{}", cid, ch, a.handle)
                            };
                            let exists = st.queues.contains_key(&name);
                            let q = st.queues.entry(name.clone()).or_default();
                            q.topic = Some(addr.clone());
                            if !shared || !exists {
                                q.selector = selector.take();
                            } else {
                                selector = None;
                            }
                            q.owner = if durable || shared { None } else { Some(key) };
                            LinkKind::Consumer(name)
                        } else {
                            st.queues.entry(addr.clone()).or_default();
                            LinkKind::Consumer(addr)
                        }
                    }
                }
            }
        };
        let is_producer = matches!(kind, LinkKind::Producer(_) | LinkKind::Coordinator);
        let rcv_second = a.rcv_settle_mode == RcvSettleMode::Second;
        let link = BLink { kind, credit: 0, delivery_count: 0, presettle, selector, partial: None, reply_address, peek_lock, rcv_second, session, epoch };
        let sess = st.conns.get_mut(&cid).unwrap().sessions.get_mut(&ch).unwrap();
        sess.links.insert(a.handle, link);
        let resp = Attach {
            name: a.name.clone(),
            handle: a.handle,
            role: if a.role == Role::Sender { Role::Receiver } else { Role::Sender },
            snd_settle_mode: a.snd_settle_mode,
            rcv_settle_mode: a.rcv_settle_mode,
            source,
            target,
            initial_delivery_count: if a.role == Role::Receiver { Some(0) } else { None },
            max_message_size: None,
            offered_capabilities: vec![],
            desired_capabilities: vec![],
            properties: resp_properties,
        };

        Self::write(st, cid, Frame::amqp(ch, Performative::Attach(resp)));
        if is_producer {
            if let Some(l) = st.conns.get_mut(&cid).and_then(|c| c.sessions.get_mut(&ch)).and_then(|s| s.links.get_mut(&a.handle)) {
                l.credit = PRODUCER_CREDIT;
            }
            self.send_link_flow(st, cid, ch, a.handle);
        }
        Ok(())
    }

    /// Event Hubs: `Some(Ok(partition))` when `addr` is an event hub (or one of its partitions).
    fn hub_partition_for_send(&self, st: &BrokerState, addr: &str) -> Option<std::result::Result<Option<u32>, AmqpError>> {
        let lower = addr.to_ascii_lowercase();
        let mut parts = lower.split('/');
        let hub = parts.next()?;
        let n = *st.hubs.get(hub)?;
        match (parts.next(), parts.next()) {
            (None, _) => Some(Ok(None)),
            (Some("partitions"), Some(p)) => match p.parse::<u32>() {
                Ok(p) if p < n => Some(Ok(Some(p))),
                _ => Some(Err(AmqpError::new(cond::NOT_FOUND, format!("Partition '{}' not found in event hub '{}'", p, hub)))),
            },
            _ => Some(Err(AmqpError::new(cond::NOT_FOUND, format!("Invalid event hub address '{}'", addr)))),
        }
    }

    fn remove_link(&self, st: &mut BrokerState, key: LinkKey) {
        let (cid, ch, h) = key;
        let mut removed_queue = None;
        if let Some(s) = st.conns.get_mut(&cid).and_then(|c| c.sessions.get_mut(&ch)) {
            if let Some(LinkKind::Consumer(q)) = s.links.remove(&h).map(|l| l.kind) {
                removed_queue = Some(q);
            }
        }
        st.session_locks.retain(|_, (k, _)| *k != key);
        // a non-durable shared subscription ends with its last consumer
        if let Some(q) = removed_queue.filter(|q| q.starts_with("#shared-v-")) {
            let in_use = st.conns.values().flat_map(|c| c.sessions.values()).flat_map(|s| s.links.values()).any(|l| matches!(&l.kind, LinkKind::Consumer(k) if *k == q));
            if !in_use {
                st.queues.remove(&q);
            }
        }
        // requeue the link's unsettled deliveries (redelivered)
        let ids: Vec<(u64, u16, u32)> = st.inflight.iter().filter(|((c, s, _), i)| *c == cid && *s == ch && i.handle == h).map(|(k, _)| *k).collect();
        for id in ids {
            if let Some(mut inf) = st.inflight.remove(&id) {
                inf.stored.delivery_count += 1;
                if let Some(q) = st.queues.get_mut(&inf.queue) {
                    q.messages.push_front(inf.stored);
                }
            }
        }
        st.queues.retain(|_, q| q.owner != Some(key));
    }

    fn cleanup_conn(&self, cid: u64) {
        let mut st = self.lock();
        // open transactions of the connection roll back
        let open: Vec<Vec<u8>> = st.txns.iter().filter(|(_, t)| t.conn == cid).map(|(k, _)| k.clone()).collect();
        for id in open {
            if let Some(tx) = st.txns.remove(&id) {
                self.discharge(&mut st, tx, true);
            }
        }

        let keys: Vec<LinkKey> = st.conns.get(&cid).map(|c| c.sessions.iter().flat_map(|(ch, s)| s.links.keys().map(move |h| (cid, *ch, *h))).collect()).unwrap_or_default();
        for k in keys {
            self.remove_link(&mut st, k);
        }
        let ids: Vec<(u64, u16, u32)> = st.inflight.keys().filter(|(c, _, _)| *c == cid).copied().collect();
        for id in ids {
            if let Some(mut inf) = st.inflight.remove(&id) {
                inf.stored.delivery_count += 1;
                if let Some(q) = st.queues.get_mut(&inf.queue) {
                    q.messages.push_front(inf.stored);
                }
            }
        }
        if let Some(c) = st.conns.remove(&cid) {
            c.transport.close();
        }
        self.dispatch(&mut st);
        drop(st);
        self.cond.notify_all();
    }

    fn transfer(&self, st: &mut BrokerState, cid: u64, ch: u16, t: Transfer, payload: Vec<u8>) -> std::result::Result<(), AmqpError> {
        let link = st.conns.get_mut(&cid).and_then(|c| c.sessions.get_mut(&ch)).and_then(|s| s.links.get_mut(&t.handle));
        let Some(l) = link else { return Err(AmqpError::new(cond::INVALID_FIELD, format!("transfer on unknown handle {}", t.handle))) };
        let coordinator = matches!(l.kind, LinkKind::Coordinator);
        let addr = match &l.kind {
            LinkKind::Producer(a) => a.clone(),
            LinkKind::Coordinator => String::new(),
            _ => return Err(AmqpError::new(cond::INVALID_FIELD, "transfer on a sending link")),
        };
        let rcv_second = l.rcv_second;
        if l.partial.is_none() {
            let id = t.delivery_id.ok_or_else(|| AmqpError::new(cond::INVALID_FIELD, "transfer without delivery-id"))?;
            let txn = match &t.state {
                Some(DeliveryState::Transactional { txn_id, .. }) => Some(txn_id.clone()),
                _ => None,
            };
            l.partial = Some((id, false, Vec::new(), txn));
            l.delivery_count = l.delivery_count.wrapping_add(1);
            l.credit = l.credit.saturating_sub(1);
        }
        let p = l.partial.as_mut().unwrap();
        p.1 |= t.settled.unwrap_or(false);
        p.2.extend_from_slice(&payload);
        if t.aborted {
            l.partial = None;
            return Ok(());
        }
        if t.more {
            return Ok(());
        }
        let (delivery_id, settled, bytes, txn) = l.partial.take().unwrap();
        let topup = l.credit < PRODUCER_CREDIT / 2;
        if topup {
            l.credit = PRODUCER_CREDIT;
        }
        let state = match Message::decode(&bytes) {
            Err(e) => DeliveryState::Rejected(Some(AmqpError::new(cond::DECODE_ERROR, e.0))),
            Ok(m) if coordinator => self.coordinate(st, cid, &m),
            Ok(m) => match txn {
                Some(id) => match st.txns.get_mut(&id) {
                    Some(tx) => {
                        tx.sends.push((ch, addr.clone(), m));
                        DeliveryState::Transactional { txn_id: id, outcome: Some(Box::new(DeliveryState::Accepted)) }
                    }
                    None => DeliveryState::Rejected(Some(AmqpError::new(cond::TRANSACTION_UNKNOWN_ID, "unknown transaction id"))),
                },
                None => match self.route(st, cid, ch, &addr, m) {
                    Ok(()) => DeliveryState::Accepted,
                    Err(e) => DeliveryState::Rejected(Some(e)),
                },
            },
        };
        if !settled {
            // rcv-settle-mode second: the sender settles first, we settle on its disposition
            Self::write(st, cid, Frame::amqp(ch, Performative::Disposition(Disposition { role: Role::Receiver, first: delivery_id, last: None, settled: !rcv_second, state: Some(state), batchable: false })));
        }
        if topup {
            self.send_link_flow(st, cid, ch, t.handle);
        }
        self.dispatch(st);
        Ok(())
    }

    /// A message on the transaction coordinator: `declare` or `discharge`.
    fn coordinate(&self, st: &mut BrokerState, cid: u64, m: &Message) -> DeliveryState {
        let Body::Value(v) = &m.body else {
            return DeliveryState::Rejected(Some(AmqpError::new(cond::DECODE_ERROR, "coordinator messages carry an amqp-value body")));
        };
        let fields = v.undescribed().as_list().unwrap_or(&[]);
        match v.descriptor_code() {
            Some(desc::DECLARE) => {
                st.next_txn += 1;
                let id = format!("txn-{}", st.next_txn).into_bytes();
                st.txns.insert(id.clone(), Txn { conn: cid, ..Default::default() });
                DeliveryState::Declared(id)
            }
            Some(desc::DISCHARGE) => {
                let id = crate::codec::field(fields, 0).as_binary().unwrap_or(&[]).to_vec();
                let fail = crate::codec::field(fields, 1).as_bool().unwrap_or(false);
                match st.txns.remove(&id) {
                    Some(tx) if tx.conn == cid => {
                        self.discharge(st, tx, fail);
                        DeliveryState::Accepted
                    }
                    Some(tx) => {
                        st.txns.insert(id, tx);
                        DeliveryState::Rejected(Some(AmqpError::new(cond::TRANSACTION_UNKNOWN_ID, "transaction of another connection")))
                    }
                    None => DeliveryState::Rejected(Some(AmqpError::new(cond::TRANSACTION_UNKNOWN_ID, "unknown transaction id"))),
                }
            }
            _ => DeliveryState::Rejected(Some(AmqpError::new(cond::NOT_IMPLEMENTED, "unsupported coordinator message"))),
        }
    }

    /// Commit (apply buffered sends and settlements) or roll back (drop the sends,
    /// make the acquired messages available again, redelivered).
    fn discharge(&self, st: &mut BrokerState, tx: Txn, fail: bool) {
        if fail {
            st.rolled_back_txns += 1;
            for (mut inf, _) in tx.settles.into_iter().rev() {
                inf.stored.delivery_count += 1;
                self.requeue(st, inf);
            }
        } else {
            st.committed_txns += 1;
            for (ch, addr, m) in tx.sends {
                let _ = self.route(st, tx.conn, ch, &addr, m);
            }
            for (inf, outcome) in tx.settles {
                self.settle(st, inf, outcome);
            }
        }
        self.dispatch(st);
    }

    /// Return an unsettled message to its queue (Azure: dead-letter after MaxDeliveryCount).
    fn requeue(&self, st: &mut BrokerState, inf: Inflight) {
        if self.azure() && inf.stored.delivery_count >= self.max_delivery_count && !inf.queue.starts_with('#') {
            self.dead_letter(st, inf, "MaxDeliveryCountExceeded", "Message could not be consumed after the maximum delivery attempts.");
        } else if let Some(q) = st.queues.get_mut(&inf.queue) {
            q.messages.push_front(inf.stored);
        }
    }

    fn new_stored(st: &mut BrokerState, msg: Message) -> Stored {

        st.next_seq += 1;
        let delivery_count = msg.header.as_ref().map(|h| h.delivery_count).unwrap_or(0);
        Stored { msg, seq: st.next_seq, enqueued: now_millis(), delivery_count }
    }

    /// Route a message sent to `addr` (anonymous relay: the `to` property).
    fn route(&self, st: &mut BrokerState, cid: u64, ch: u16, addr: &str, msg: Message) -> std::result::Result<(), AmqpError> {
        let addr = if addr.is_empty() { msg.properties.as_ref().and_then(|p| p.to.clone()).unwrap_or_default() } else { addr.to_string() };
        if addr.is_empty() {
            return Err(AmqpError::new(cond::NOT_FOUND, "no address"));
        }
        if addr == "$cbs" {
            let resp = self.cbs(st, cid, &msg);
            self.respond(st, cid, ch, &addr, &msg, resp);
            return Ok(());
        }
        if Self::is_management(&addr) {
            let entity = addr.trim_end_matches("$management").trim_end_matches('/').to_string();
            let resp = self.management(st, cid, &entity, &msg);
            self.respond(st, cid, ch, &addr, &msg, resp);
            return Ok(());
        }
        if !self.authorized(st, cid, &addr) {
            return Err(AmqpError::new(cond::UNAUTHORIZED_ACCESS, format!("Unauthorized access to '{}'", addr)));
        }
        if let Some(when) = msg.annotation("x-opt-scheduled-enqueue-time").and_then(|v| v.as_i64()) {
            if when > now_millis() {
                let s = Self::new_stored(st, msg);
                st.scheduled.push((when, addr, s));
                return Ok(());
            }
        }
        let s = Self::new_stored(st, msg);
        self.enqueue(st, &addr, s)
    }

    fn enqueue(&self, st: &mut BrokerState, addr: &str, s: Stored) -> std::result::Result<(), AmqpError> {
        if self.azure() {
            if let Some(r) = self.hub_partition_for_send(st, addr) {
                let p = r?;
                let hub = addr.split('/').next().unwrap().to_ascii_lowercase();
                let n = st.hubs[&hub];
                let p = match p {
                    Some(p) => p,
                    None => match s.msg.annotation("x-opt-partition-key").and_then(|v| v.as_str()) {
                        Some(k) => (k.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32))) % n,
                        None => {
                            st.hub_rr += 1;
                            (st.hub_rr as u32) % n
                        }
                    },
                };
                let log = st.logs.get_mut(&format!("{}/{}", hub, p)).unwrap();
                let off = log.next_offset;
                log.next_offset += s.msg.encode().len() as i64;
                log.events.push((off, s));
                return Ok(());
            }
            let lower = addr.to_ascii_lowercase();
            let prefix = format!("{}/subscriptions/", lower);
            let subs: Vec<String> = st.queues.keys().filter(|k| k.starts_with(&prefix) && !k.ends_with(DLQ)).cloned().collect();
            if !subs.is_empty() {
                for k in subs {
                    st.queues.get_mut(&k).unwrap().messages.push_back(s.clone());
                }
            } else {
                st.queues.entry(lower).or_default().messages.push_back(s);
            }
            return Ok(());
        }
        if st.topics.contains(addr) {
            let lookup = selector_lookup(&s);
            let subs: Vec<String> = st.queues.iter().filter(|(_, q)| q.topic.as_deref() == Some(addr) && q.selector.as_ref().is_none_or(|sel| sel.matches(&lookup))).map(|(k, _)| k.clone()).collect();
            for k in subs {
                st.queues.get_mut(&k).unwrap().messages.push_back(s.clone());
            }
            return Ok(());
        }
        st.queues.entry(addr.to_string()).or_default().messages.push_back(s);
        Ok(())
    }

    /// Queue a management response to the receiver whose target is the reply-to address.
    fn respond(&self, st: &mut BrokerState, cid: u64, ch: u16, node: &str, req: &Message, mut resp: Message) {
        let p = req.props();
        if let Some(id) = p.message_id.clone() {
            resp.props_mut().correlation_id = Some(id);
        }
        let reply_to = p.reply_to.clone();
        let mut target = None;
        if let Some(c) = st.conns.get(&cid) {
            for (sch, s) in &c.sessions {
                for (h, l) in &s.links {
                    if l.reply_address.is_some() && (reply_to.is_none() || l.reply_address == reply_to) && (target.is_none() || *sch == ch) {
                        target = Some(format!("#reply-{}-{}-{}", cid, sch, h));
                    }
                }
            }
        }
        let _ = node;
        if let Some(q) = target {
            let s = Self::new_stored(st, resp);
            if let Some(queue) = st.queues.get_mut(&q) {
                queue.messages.push_back(s);
            }
        }
    }

    fn cbs(&self, st: &mut BrokerState, cid: u64, req: &Message) -> Message {
        let Some(az) = &self.azure else { return reply(404, "CBS not enabled", Body::Empty) };
        if req.app_property("operation").and_then(|v| v.as_str()) != Some("put-token") {
            return reply(400, "unsupported operation", Body::Empty);
        }
        let name = req.app_property("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let token = match &req.body {
            Body::Value(v) => v.as_str().unwrap_or("").to_string(),
            _ => String::new(),
        };
        if req.app_property("type").and_then(|v| v.as_str()) == Some(crate::cbs::JWT_TOKEN_TYPE) {
            if !az.jwt.contains(&token) {
                return reply(401, "Unauthorized: invalid or expired Azure AD token", Body::Empty);
            }
            if let Some(c) = st.conns.get_mut(&cid) {
                c.authorized.push(path_of(&name));
            }
            return reply(202, "Accepted", Body::Empty);
        }
        let Some(t) = parse_sas_token(&token) else { return reply(401, "Unauthorized: malformed token", Body::Empty) };
        let Some(key) = az.keys.get(&t.key_name) else { return reply(401, "Unauthorized: unknown SharedAccessKeyName", Body::Empty) };
        if !verify_sas_token(&t, key) {
            return reply(401, "Unauthorized: invalid signature or expired token", Body::Empty);
        }
        let token_path = path_of(&t.resource);
        let audience_path = path_of(&name);
        if !covers(&token_path, &audience_path) {
            return reply(401, "Unauthorized: token does not cover the audience", Body::Empty);
        }
        if let Some(c) = st.conns.get_mut(&cid) {
            c.authorized.push(audience_path);
        }
        reply(202, "Accepted", Body::Empty)
    }

    fn management(&self, st: &mut BrokerState, cid: u64, entity: &str, req: &Message) -> Message {
        let op = req.app_property("operation").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let body = match &req.body {
            Body::Value(v) => v.clone(),
            _ => AmqpValue::Null,
        };
        if op == "READ" {
            let name = req.app_property("name").and_then(|v| v.as_str()).unwrap_or(entity).to_ascii_lowercase();
            if !self.authorized(st, cid, &name) {
                return reply(401, "Unauthorized", Body::Empty);
            }
            let Some(n) = st.hubs.get(&name).copied() else { return reply(404, "event hub not found", Body::Empty) };
            let ty = req.app_property("type").and_then(|v| v.as_str()).unwrap_or("");
            if ty == "com.microsoft:partition" {
                let pid = req.app_property("partition").and_then(|v| v.as_str()).unwrap_or("0").to_string();
                let Some(log) = st.logs.get(&format!("{}/{}", name, pid)) else { return reply(404, "partition not found", Body::Empty) };
                let last = log.events.last();
                let m = vec![
                    (AmqpValue::str("name"), AmqpValue::str(name.clone())),
                    (AmqpValue::str("partition"), AmqpValue::str(pid)),
                    (AmqpValue::str("begin_sequence_number"), AmqpValue::Long(log.events.first().map(|e| e.1.seq).unwrap_or(-1))),
                    (AmqpValue::str("last_enqueued_sequence_number"), AmqpValue::Long(last.map(|e| e.1.seq).unwrap_or(-1))),
                    (AmqpValue::str("last_enqueued_offset"), AmqpValue::str(last.map(|e| e.0).unwrap_or(-1).to_string())),
                    (AmqpValue::str("last_enqueued_time_utc"), AmqpValue::Timestamp(last.map(|e| e.1.enqueued).unwrap_or(0))),
                    (AmqpValue::str("is_partition_empty"), AmqpValue::Boolean(log.events.is_empty())),
                ];
                return reply(200, "OK", Body::Value(AmqpValue::Map(m)));
            }
            let m = vec![
                (AmqpValue::str("name"), AmqpValue::str(name.clone())),
                (AmqpValue::str("created_at"), AmqpValue::Timestamp(0)),
                (AmqpValue::str("partition_count"), AmqpValue::Int(n as i32)),
                (AmqpValue::str("partition_ids"), AmqpValue::Array((0..n).map(|p| AmqpValue::str(p.to_string())).collect())),
            ];
            return reply(200, "OK", Body::Value(AmqpValue::Map(m)));
        }
        if !self.authorized(st, cid, entity) {
            return reply(401, "Unauthorized", Body::Empty);
        }
        let key = entity.to_ascii_lowercase();
        match op.as_str() {
            "com.microsoft:schedule-message" => {
                let mut seqs = Vec::new();
                let list = body.map_get("messages").and_then(|v| v.as_list()).unwrap_or(&[]).to_vec();
                for item in list {
                    let Some(bytes) = item.map_get("message").and_then(|v| v.as_binary()) else { continue };
                    let Ok(msg) = Message::decode(bytes) else { return reply(400, "invalid message", Body::Empty) };
                    let when = msg.annotation("x-opt-scheduled-enqueue-time").and_then(|v| v.as_i64()).unwrap_or(0);
                    let s = Self::new_stored(st, msg);
                    seqs.push(AmqpValue::Long(s.seq));
                    if when > now_millis() {
                        st.scheduled.push((when, key.clone(), s));
                    } else {
                        let _ = self.enqueue(st, &key, s);
                    }
                }
                reply(200, "OK", Body::Value(AmqpValue::Map(vec![(AmqpValue::str("sequence-numbers"), AmqpValue::Array(seqs))])))
            }
            "com.microsoft:cancel-scheduled-message" => {
                let seqs: Vec<i64> = body.map_get("sequence-numbers").and_then(|v| v.as_list()).unwrap_or(&[]).iter().filter_map(|v| v.as_i64()).collect();
                st.scheduled.retain(|(_, _, s)| !seqs.contains(&s.seq));
                reply(200, "OK", Body::Empty)
            }
            "com.microsoft:peek-message" => {
                let from = body.map_get("from-sequence-number").and_then(|v| v.as_i64()).unwrap_or(0);
                let count = body.map_get("message-count").and_then(|v| v.as_i64()).unwrap_or(1).max(1) as usize;
                let mut all: Vec<Stored> = st.queues.get(&key).map(|q| q.messages.iter().cloned().collect()).unwrap_or_default();
                all.extend(st.inflight.values().filter(|i| i.queue == key).map(|i| i.stored.clone()));
                all.sort_by_key(|s| s.seq);
                let items: Vec<AmqpValue> = all
                    .into_iter()
                    .filter(|s| s.seq >= from)
                    .take(count)
                    .map(|s| AmqpValue::Map(vec![(AmqpValue::str("message"), AmqpValue::Binary(self.decorate(&key, &s, None).encode()))]))
                    .collect();
                if items.is_empty() {
                    return reply(204, "No Content", Body::Empty);
                }
                reply(200, "OK", Body::Value(AmqpValue::Map(vec![(AmqpValue::str("messages"), AmqpValue::List(items))])))
            }
            "com.microsoft:renew-lock" => {
                let now = now_millis();
                let until = now + self.lock_duration.as_millis() as i64;
                let mut exp = Vec::new();
                for t in uuids(body.map_get("lock-tokens")) {
                    if let Some(inf) = st.inflight.values_mut().find(|i| i.lock_token == Some(t) && i.queue == key) {
                        inf.locked_until = Some(until);
                        exp.push(AmqpValue::Timestamp(until));
                    } else if let Some(d) = st.deferred_locked.get_mut(&t) {
                        d.2 = until;
                        exp.push(AmqpValue::Timestamp(until));
                    } else {
                        return reply(410, "The lock supplied is invalid. Either the lock expired, or the message has already been removed from the queue.", Body::Empty);
                    }
                }
                reply(200, "OK", Body::Value(AmqpValue::Map(vec![(AmqpValue::str("expirations"), AmqpValue::Array(exp))])))
            }
            "com.microsoft:receive-by-sequence-number" => {
                let seqs: Vec<i64> = body.map_get("sequence-numbers").and_then(|v| v.as_list()).unwrap_or(&[]).iter().filter_map(|v| v.as_i64()).collect();
                let peek_lock = body.map_get("receiver-settle-mode").and_then(|v| v.as_u64()).unwrap_or(1) != 0;
                let deferred = st.deferred.entry(key.clone()).or_default();
                let mut found = Vec::new();
                for seq in &seqs {
                    match deferred.iter().position(|s| s.seq == *seq) {
                        Some(i) => found.push(deferred.remove(i)),
                        None => {
                            // put back what was taken: the operation fails as a whole
                            deferred.extend(found);
                            return reply(404, &format!("Failed to lookup deferred message with sequence number {}", seq), Body::Empty);
                        }
                    }
                }
                let until = now_millis() + self.lock_duration.as_millis() as i64;
                let mut items = Vec::new();
                for s in found {
                    let mut entry = vec![(AmqpValue::str("message"), AmqpValue::Binary(self.decorate(&key, &s, if peek_lock { Some(until) } else { None }).encode()))];
                    if peek_lock {
                        let token = crate::message::random_uuid();
                        entry.push((AmqpValue::str("lock-token"), AmqpValue::Uuid(token)));
                        st.deferred_locked.insert(token, (key.clone(), s, until));
                    }
                    items.push(AmqpValue::Map(entry));
                }
                reply(200, "OK", Body::Value(AmqpValue::Map(vec![(AmqpValue::str("messages"), AmqpValue::List(items))])))
            }
            "com.microsoft:update-disposition" => {
                let status = body.map_get("disposition-status").and_then(|v| v.as_str()).unwrap_or("").to_ascii_lowercase();
                let reason = body.map_get("deadletter-reason").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let desc = body.map_get("deadletter-description").and_then(|v| v.as_str()).unwrap_or("").to_string();
                for t in uuids(body.map_get("lock-tokens")) {
                    if let Some((entity, stored, _)) = st.deferred_locked.remove(&t) {
                        let inf = Inflight { queue: entity.clone(), handle: 0, stored, locked_until: None, lock_token: None };
                        match status.as_str() {
                            "completed" => {}
                            "suspended" => self.dead_letter(st, inf, &reason, &desc),
                            _ => st.deferred.entry(entity).or_default().push(inf.stored),
                        }
                        continue;
                    }
                    let k = st.inflight.iter().find(|(_, i)| i.lock_token == Some(t)).map(|(k, _)| *k);
                    let Some(inf) = k.and_then(|k| st.inflight.remove(&k)) else {
                        return reply(410, "The lock supplied is invalid.", Body::Empty);
                    };
                    let state = match status.as_str() {
                        "completed" => DeliveryState::Accepted,
                        "suspended" => {
                            let mut e = AmqpError::new("com.microsoft:dead-letter", "dead-lettered");
                            e.info = vec![(AmqpValue::sym("DeadLetterReason"), AmqpValue::str(reason.clone())), (AmqpValue::sym("DeadLetterErrorDescription"), AmqpValue::str(desc.clone()))];
                            DeliveryState::Rejected(Some(e))
                        }
                        "defered" | "deferred" => DeliveryState::Modified { delivery_failed: false, undeliverable_here: true, message_annotations: vec![] },
                        _ => DeliveryState::Modified { delivery_failed: true, undeliverable_here: false, message_annotations: vec![] },
                    };
                    self.settle(st, inf, state);
                }
                self.dispatch(st);
                reply(200, "OK", Body::Empty)
            }
            "com.microsoft:renew-session-lock" | "com.microsoft:get-session-state" | "com.microsoft:set-session-state" => {
                let Some(sid) = body.map_get("session-id").and_then(|v| v.as_str()).map(String::from) else { return reply(400, "session-id required", Body::Empty) };
                let k = (key.clone(), sid.clone());
                match op.as_str() {
                    "com.microsoft:renew-session-lock" => {
                        let until = now_millis() + self.lock_duration.as_millis() as i64;
                        match st.session_locks.get_mut(&k) {
                            Some(l) => {
                                l.1 = until;
                                reply(200, "OK", Body::Value(AmqpValue::Map(vec![(AmqpValue::str("expiration"), AmqpValue::Timestamp(until))])))
                            }
                            None => reply(410, &format!("The session lock has expired on the MessageSession. Session: {}", sid), Body::Empty),
                        }
                    }
                    "com.microsoft:get-session-state" => {
                        let v = st.session_states.get(&k).map(|b| AmqpValue::Binary(b.clone())).unwrap_or(AmqpValue::Null);
                        reply(200, "OK", Body::Value(AmqpValue::Map(vec![(AmqpValue::str("session-state"), v)])))
                    }
                    _ => {
                        match body.map_get("session-state").and_then(|v| v.as_binary()) {
                            Some(b) => {
                                st.session_states.insert(k, b.to_vec());
                            }
                            None => {
                                st.session_states.remove(&k);
                            }
                        }
                        reply(200, "OK", Body::Empty)
                    }
                }
            }
            _ => reply(501, &format!("operation {} not supported", op), Body::Empty),
        }
    }

    /// Event Hubs epochs: a receiver with epoch `e` disconnects the receivers of the
    /// partition (and consumer group) with a lower or equal epoch or none; it is refused
    /// when a receiver with a higher epoch exists. A receiver without epoch is refused
    /// while an epoch receiver exists.
    fn claim_epoch(&self, st: &mut BrokerState, log_key: &str, group: &str, epoch: Option<i64>) -> std::result::Result<(), AmqpError> {
        let mut existing: Vec<(LinkKey, Option<i64>)> = Vec::new();
        for (cid, c) in &st.conns {
            for (ch, s) in &c.sessions {
                for (h, l) in &s.links {
                    if matches!(&l.kind, LinkKind::Log { key, group: g, .. } if key == log_key && g == group) {
                        existing.push(((*cid, *ch, *h), l.epoch));
                    }
                }
            }
        }
        match epoch {
            None => {
                if let Some(e) = existing.iter().find_map(|(_, e)| *e) {
                    return Err(AmqpError::new(cond::STOLEN, format!("Receiver could not be created: a receiver with epoch '{}' already exists on this partition", e)));
                }
            }
            Some(e) => {
                if let Some(higher) = existing.iter().find_map(|(_, x)| x.filter(|x| *x > e)) {
                    return Err(AmqpError::new(cond::STOLEN, format!("Receiver with epoch '{}' cannot be created because a receiver with a higher epoch '{}' already exists", e, higher)));
                }
                for (k, _) in existing {
                    let err = AmqpError::new(cond::STOLEN, format!("New receiver with higher epoch of '{}' is created hence current receiver is getting disconnected.", e));
                    Self::write(st, k.0, Frame::amqp(k.1, Performative::Detach(Detach { handle: k.2, closed: true, error: Some(err) })));
                    self.remove_link(st, k);
                }
            }
        }
        Ok(())
    }

    /// Service Bus sessions: lock `requested` (or the next session with messages that is
    /// not locked) of `entity` for a receiver link. Returns the session id and lock expiry.
    fn lock_session(&self, st: &mut BrokerState, entity: &str, requested: Option<String>, link: LinkKey) -> std::result::Result<(String, i64), AmqpError> {
        let now = now_millis();
        let until = now + self.lock_duration.as_millis() as i64;
        let sid = match requested {
            Some(sid) => {
                if st.session_locks.get(&(entity.to_string(), sid.clone())).is_some_and(|(k, t)| *k != link && *t > now) {
                    return Err(AmqpError::new("com.microsoft:session-cannot-be-locked", format!("The requested session '{}' cannot be accepted. It may be locked by another receiver.", sid)));
                }
                sid
            }
            None => {
                let locked = |sid: &str| st.session_locks.get(&(entity.to_string(), sid.to_string())).is_some_and(|(_, t)| *t > now);
                let candidate = st.queues.get(entity).and_then(|q| q.messages.iter().filter_map(|s| s.msg.properties.as_ref().and_then(|p| p.group_id.clone())).find(|g| !locked(g)));
                match candidate {
                    Some(sid) => sid,
                    None => return Err(AmqpError::new("com.microsoft:timeout", "No unlocked session with messages is available")),
                }
            }
        };
        st.session_locks.insert((entity.to_string(), sid.clone()), (link, until));
        Ok((sid, until))
    }

    /// The message as delivered: header delivery-count, Azure annotations.
    fn decorate(&self, queue: &str, s: &Stored, locked_until: Option<i64>) -> Message {
        let mut m = s.msg.clone();
        if s.delivery_count > 0 || m.header.is_some() {
            let h = m.header_mut();
            h.delivery_count = s.delivery_count;
            h.first_acquirer = s.delivery_count == 0;
        }
        if self.azure() && !queue.starts_with('#') {
            m.set_annotation("x-opt-sequence-number", AmqpValue::Long(s.seq));
            m.set_annotation("x-opt-enqueued-time", AmqpValue::Timestamp(s.enqueued));
            if let Some(l) = locked_until {
                m.set_annotation("x-opt-locked-until", AmqpValue::Timestamp(l));
            }
        }
        m
    }

    /// Apply a disposition outcome to an inflight message.
    fn settle(&self, st: &mut BrokerState, mut inf: Inflight, state: DeliveryState) {
        match state {
            DeliveryState::Accepted => {}
            DeliveryState::Released => {
                if let Some(q) = st.queues.get_mut(&inf.queue) {
                    q.messages.push_front(inf.stored);
                }
            }
            DeliveryState::Modified { undeliverable_here: true, delivery_failed: false, .. } if self.azure() && !inf.queue.starts_with('#') => {
                // Service Bus defer: kept aside, received by sequence number
                st.deferred.entry(inf.queue.clone()).or_default().push(inf.stored);
            }
            DeliveryState::Modified { delivery_failed, message_annotations, .. } => {
                // Service Bus counts every abandon as a delivery attempt
                if delivery_failed || self.azure() {
                    inf.stored.delivery_count += 1;
                }
                for (k, v) in message_annotations {
                    if let Some(k) = k.as_str() {
                        inf.stored.msg.set_annotation(k, v);
                    }
                }
                if self.azure() && inf.stored.delivery_count >= self.max_delivery_count {
                    self.dead_letter(st, inf, "MaxDeliveryCountExceeded", "Message could not be consumed after the maximum delivery attempts.");
                } else if let Some(q) = st.queues.get_mut(&inf.queue) {
                    q.messages.push_front(inf.stored);
                }
            }
            DeliveryState::Rejected(err) => {
                if self.azure() && !inf.queue.ends_with(DLQ) {
                    let info = err.as_ref().map(|e| e.info.clone()).unwrap_or_default();
                    let get = |k: &str| info.iter().find(|(a, _)| a.as_str() == Some(k)).and_then(|(_, v)| v.as_str()).map(String::from);
                    let reason = get("DeadLetterReason").unwrap_or_else(|| err.as_ref().and_then(|e| e.description.clone()).unwrap_or_default());
                    let desc = get("DeadLetterErrorDescription").unwrap_or_default();
                    self.dead_letter(st, inf, &reason, &desc);
                }
            }
            DeliveryState::Received { .. } | DeliveryState::Other(_) | DeliveryState::Declared(_) | DeliveryState::Transactional { .. } => {

                if let Some(q) = st.queues.get_mut(&inf.queue) {
                    q.messages.push_front(inf.stored);
                }
            }
        }
    }

    fn dead_letter(&self, st: &mut BrokerState, mut inf: Inflight, reason: &str, desc: &str) {
        inf.stored.msg.set_app_property("DeadLetterReason", AmqpValue::str(reason));
        inf.stored.msg.set_app_property("DeadLetterErrorDescription", AmqpValue::str(desc));
        inf.stored.msg.set_annotation("x-opt-deadletter-source", AmqpValue::str(inf.queue.clone()));
        let dlq = format!("{}/{}", inf.queue, DLQ);
        st.queues.entry(dlq).or_default().messages.push_back(inf.stored);
    }

    fn housekeeping(&self, st: &mut BrokerState) {
        let now = now_millis();
        let mut changed = false;
        if st.scheduled.iter().any(|(t, _, _)| *t <= now) {
            let due: Vec<(i64, String, Stored)> = {
                let (due, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut st.scheduled).into_iter().partition(|(t, _, _)| *t <= now);
                st.scheduled = keep;
                due
            };
            for (_, addr, mut s) in due {
                s.enqueued = now;
                let _ = self.enqueue(st, &addr, s);
            }
            changed = true;
        }
        // expired session locks detach the receiver
        let lost: Vec<((String, String), LinkKey)> = st.session_locks.iter().filter(|(_, (_, t))| *t <= now).map(|(k, (l, _))| (k.clone(), *l)).collect();
        for ((_, sid), k) in lost {
            let err = AmqpError::new("com.microsoft:session-lock-lost", format!("The session lock has expired on the MessageSession. Session: {}", sid));
            Self::write(st, k.0, Frame::amqp(k.1, Performative::Detach(Detach { handle: k.2, closed: true, error: Some(err) })));
            self.remove_link(st, k);
            changed = true;
        }
        let expired_deferred: Vec<[u8; 16]> = st.deferred_locked.iter().filter(|(_, (_, _, t))| *t <= now).map(|(k, _)| *k).collect();
        for t in expired_deferred {
            if let Some((entity, s, _)) = st.deferred_locked.remove(&t) {
                st.deferred.entry(entity).or_default().push(s);
            }
        }
        let expired: Vec<(u64, u16, u32)> = st.inflight.iter().filter(|(_, i)| i.locked_until.is_some_and(|l| l <= now)).map(|(k, _)| *k).collect();
        for k in expired {
            if let Some(mut inf) = st.inflight.remove(&k) {
                inf.stored.delivery_count += 1;
                if self.azure() && inf.stored.delivery_count >= self.max_delivery_count {
                    self.dead_letter(st, inf, "MaxDeliveryCountExceeded", "Lock expired too many times.");
                } else if let Some(q) = st.queues.get_mut(&inf.queue) {
                    q.messages.push_front(inf.stored);
                }
            }
            changed = true;
        }
        if changed {
            self.dispatch(st);
        }
    }

    /// Deliver messages to consumer links with credit (round robin between competing consumers).
    fn dispatch(&self, st: &mut BrokerState) {
        loop {
            let mut keys: Vec<LinkKey> = Vec::new();
            for (cid, c) in &st.conns {
                for (ch, s) in &c.sessions {
                    for (h, l) in &s.links {
                        if l.credit > 0 && !matches!(l.kind, LinkKind::Producer(_)) {
                            keys.push((*cid, *ch, *h));
                        }
                    }
                }
            }
            if keys.is_empty() {
                return;
            }
            keys.sort();
            let n = keys.len();
            let start = st.rr % n;
            keys.rotate_left(start);
            let mut progress = false;
            for key in keys {
                if self.deliver_one(st, key) {
                    st.rr = st.rr.wrapping_add(1);
                    progress = true;
                }
            }
            if !progress {
                return;
            }
        }
    }

    fn deliver_one(&self, st: &mut BrokerState, key: LinkKey) -> bool {
        let (cid, ch, h) = key;
        let Some(l) = st.conns.get(&cid).and_then(|c| c.sessions.get(&ch)).and_then(|s| s.links.get(&h)) else { return false };
        if l.credit == 0 {
            return false;
        }
        let presettle = l.presettle;
        let peek_lock = l.peek_lock;
        let session = l.session.clone();
        let (queue_name, msg, stored) = match l.kind.clone() {
            LinkKind::Producer(_) | LinkKind::Coordinator => return false,
            LinkKind::Log { key: lk, cursor, .. } => {
                let Some(log) = st.logs.get(&lk) else { return false };
                let Some((off, s)) = log.events.get(cursor).cloned() else { return false };
                let mut m = s.msg.clone();
                m.set_annotation("x-opt-offset", AmqpValue::str(off.to_string()));
                m.set_annotation("x-opt-sequence-number", AmqpValue::Long(s.seq));
                m.set_annotation("x-opt-enqueued-time", AmqpValue::Timestamp(s.enqueued));
                if let Some(LinkKind::Log { cursor, .. }) = st.conns.get_mut(&cid).and_then(|c| c.sessions.get_mut(&ch)).and_then(|s| s.links.get_mut(&h)).map(|l| &mut l.kind) {
                    *cursor += 1;
                }
                (lk, m, None)
            }
            LinkKind::Consumer(q) => {
                let selector = l.selector.clone();
                let now = now_millis();
                let Some(queue) = st.queues.get_mut(&q) else { return false };
                // drop expired messages
                queue.messages.retain(|s| {
                    let p = s.msg.properties.as_ref();
                    let abs = p.and_then(|p| p.absolute_expiry_time).filter(|t| *t > 0);
                    let ttl = s.msg.header.as_ref().and_then(|h| h.ttl).map(|t| s.enqueued + t as i64);
                    abs.or(ttl).is_none_or(|t| t > now)
                });
                let in_session = |s: &Stored| match &session {
                    None => true,
                    Some(sid) => s.msg.properties.as_ref().and_then(|p| p.group_id.as_deref()) == Some(sid.as_str()),
                };
                let idx = match &selector {
                    None => queue.messages.iter().position(in_session),
                    Some(sel) => queue.messages.iter().position(|s| in_session(s) && sel.matches(&selector_lookup(s))),
                };
                let Some(idx) = idx else { return false };
                let s = queue.messages.remove(idx).unwrap();
                let locked = if peek_lock && !presettle { Some(now + self.lock_duration.as_millis() as i64) } else { None };
                let m = self.decorate(&q, &s, locked);
                (q, m, Some((s, locked)))
            }
        };
        let azure_tag = self.azure() && stored.is_some() && !presettle && !queue_name.starts_with('#');
        let c = st.conns.get_mut(&cid).unwrap();
        let max_frame = c.max_frame_out;
        let transport = c.transport.clone();
        let s = c.sessions.get_mut(&ch).unwrap();
        let l = s.links.get_mut(&h).unwrap();
        l.credit -= 1;
        l.delivery_count = l.delivery_count.wrapping_add(1);
        let delivery_id = s.next_delivery_id;
        s.next_delivery_id = s.next_delivery_id.wrapping_add(1);
        let mut msg = msg;
        let mut lock_token = None;
        let tag = if azure_tag {
            let u = crate::message::random_uuid();
            msg.set_annotation("x-opt-lock-token", AmqpValue::Uuid(u));
            lock_token = Some(u);
            crate::client::dotnet_guid_bytes(&u).to_vec()
        } else {
            (delivery_id as u64).to_be_bytes().to_vec()
        };
        let payload = msg.encode();
        let mut t = Transfer { handle: h, delivery_id: Some(delivery_id), delivery_tag: Some(tag), message_format: Some(0), settled: Some(presettle), more: true, ..Default::default() };
        let overhead = 8 + Performative::Transfer(t.clone()).to_value().encode().len();
        let chunk = (max_frame as usize).saturating_sub(overhead).max(1);
        let mut chunks: Vec<&[u8]> = payload.chunks(chunk).collect();
        if chunks.is_empty() {
            chunks.push(&[]);
        }
        let n = chunks.len();
        let mut bytes = Vec::with_capacity(payload.len() + 64);
        for (i, c) in chunks.into_iter().enumerate() {
            t.more = i + 1 < n;
            bytes.extend_from_slice(&Frame { channel: ch, body: FrameBody::Amqp(Performative::Transfer(t.clone()), c.to_vec()) }.encode());
            s.next_outgoing_id = s.next_outgoing_id.wrapping_add(1);
        }
        if let Some((stored, locked)) = stored {
            if !presettle {
                st.inflight.insert((cid, ch, delivery_id), Inflight { queue: queue_name, handle: h, stored, locked_until: locked, lock_token });

            }
        }
        let _ = transport.write_bytes(&bytes);
        true
    }
}

/// Lock tokens of a management request (`uuid` array or list, or strings).
fn uuids(v: Option<&AmqpValue>) -> Vec<[u8; 16]> {
    v.and_then(|v| v.as_list())
        .unwrap_or(&[])
        .iter()
        .filter_map(|t| match t {
            AmqpValue::Uuid(u) => Some(*u),
            other => other.as_str().and_then(crate::message::parse_uuid),
        })
        .collect()
}

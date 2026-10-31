//! An embedded MQTT 3.1.1 / 5.0 broker for tests.
//!
//! Supports CONNECT with optional username/password authentication, clean and persistent
//! sessions (queued QoS 1/2 messages for offline persistent sessions), session takeover,
//! keep-alive enforcement, PUBLISH QoS 0/1/2 in both directions, SUBSCRIBE/UNSUBSCRIBE
//! with `+`/`#` wildcards, retained messages (an empty retained payload clears), will
//! messages on abnormal disconnects and MQTT 5 properties passed through to subscribers.
//!
//! ```no_run
//! use camel_kafka_mqtt::testing::MqttBroker;
//! let broker = MqttBroker::start().unwrap();
//! let uri = format!("paho:sensors/+?brokerUrl={}", broker.url());
//! # let _ = uri;
//! broker.stop();
//! ```

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::{BufReader, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use camel_core::error::Result;

use crate::mqtt::packet::*;
use crate::net::{unique_id, TcpServer};

/// Broker settings.
#[derive(Clone, Debug, Default)]
pub struct MqttBrokerConfig {
    /// Port to bind (0 = ephemeral).
    pub port: u16,
    /// Accepted `(username, password)` pairs; empty = anonymous access allowed.
    pub users: Vec<(String, String)>,
}

struct BConn {
    id: u64,
    writer: Mutex<TcpStream>,
    v5: bool,
}

impl BConn {
    fn send(&self, p: &Packet) -> bool {
        let bytes = p.encode(self.v5);
        let mut w = self.writer.lock().unwrap();
        w.write_all(&bytes).and_then(|_| w.flush()).is_ok()
    }
    fn shutdown(&self) {
        let _ = self.writer.lock().unwrap().shutdown(std::net::Shutdown::Both);
    }
}

struct Session {
    /// filter -> subscription options (qos in bits 0-1, v5 flags above)
    subs: BTreeMap<String, u8>,
    conn: Option<Arc<BConn>>,
    persistent: bool,
    queue: VecDeque<Publish>,
    next_pkid: u16,
}

impl Session {
    fn pkid(&mut self) -> u16 {
        self.next_pkid = self.next_pkid.wrapping_add(1);
        if self.next_pkid == 0 {
            self.next_pkid = 1;
        }
        self.next_pkid
    }
}

#[derive(Default)]
struct Inner {
    sessions: HashMap<String, Session>,
    retained: BTreeMap<String, Publish>,
    connects: HashMap<String, usize>,
}

struct State {
    users: Vec<(String, String)>,
    inner: Mutex<Inner>,
    running: AtomicBool,
    conn_ids: AtomicU64,
    published: AtomicUsize,
}

/// The embedded MQTT broker. Stops when dropped.
pub struct MqttBroker {
    state: Arc<State>,
    server: TcpServer,
}

impl MqttBroker {
    /// Start on an ephemeral port, anonymous access.
    pub fn start() -> Result<MqttBroker> {
        Self::start_with(MqttBrokerConfig::default())
    }

    /// Start with settings.
    pub fn start_with(cfg: MqttBrokerConfig) -> Result<MqttBroker> {
        let state = Arc::new(State {
            users: cfg.users,
            inner: Mutex::new(Inner::default()),
            running: AtomicBool::new(true),
            conn_ids: AtomicU64::new(1),
            published: AtomicUsize::new(0),
        });
        let s2 = state.clone();
        let server = TcpServer::bind("127.0.0.1", cfg.port, "mqtt-broker", move |stream| serve(&s2, stream))?;
        Ok(MqttBroker { state, server })
    }

    pub fn port(&self) -> u16 {
        self.server.port()
    }

    /// `tcp://127.0.0.1:<port>`, for the `brokerUrl` option.
    pub fn url(&self) -> String {
        format!("tcp://127.0.0.1:{}", self.port())
    }

    /// The retained payload of a topic.
    pub fn retained(&self, topic: &str) -> Option<Vec<u8>> {
        self.state.inner.lock().unwrap().retained.get(topic).map(|p| p.payload.clone())
    }

    /// Client ids with an open connection.
    pub fn connected_clients(&self) -> Vec<String> {
        let mut v: Vec<String> = self.state.inner.lock().unwrap().sessions.iter().filter(|(_, s)| s.conn.is_some()).map(|(k, _)| k.clone()).collect();
        v.sort();
        v
    }

    /// The topic filters a client is subscribed to.
    pub fn subscriptions(&self, client_id: &str) -> Vec<String> {
        self.state.inner.lock().unwrap().sessions.get(client_id).map(|s| s.subs.keys().cloned().collect()).unwrap_or_default()
    }

    /// Number of accepted connections (CONNECT + CONNACK) of a client id.
    pub fn connect_count(&self, client_id: &str) -> usize {
        self.state.inner.lock().unwrap().connects.get(client_id).copied().unwrap_or(0)
    }

    /// Number of PUBLISH packets received from clients.
    pub fn published_count(&self) -> usize {
        self.state.published.load(Ordering::SeqCst)
    }

    /// Drop a client's connection without DISCONNECT (its will is published).
    pub fn disconnect_client(&self, client_id: &str) -> bool {
        let conn = self.state.inner.lock().unwrap().sessions.get(client_id).and_then(|s| s.conn.clone());
        match conn {
            Some(c) => {
                c.shutdown();
                true
            }
            None => false,
        }
    }

    /// Publish a message from the broker itself (as if a client had published it).
    pub fn publish(&self, topic: &str, payload: &[u8], qos: u8, retain: bool) {
        route(&self.state, Publish { qos, retain, topic: topic.to_string(), payload: payload.to_vec(), ..Default::default() });
    }

    /// Wait until `f` holds (checked every 10ms) or the timeout expires; returns `f()`.
    pub fn wait_until(&self, timeout: Duration, f: impl Fn(&MqttBroker) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if f(self) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        f(self)
    }

    /// Stop the broker and close every connection.
    pub fn stop(&self) {
        self.state.running.store(false, Ordering::SeqCst);
        self.server.stop();
    }
}

impl Drop for MqttBroker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn route(state: &State, p: Publish) {
    let mut out: Vec<(Arc<BConn>, Packet)> = Vec::new();
    {
        let mut inner = state.inner.lock().unwrap();
        if p.retain {
            if p.payload.is_empty() {
                inner.retained.remove(&p.topic);
            } else {
                inner.retained.insert(p.topic.clone(), p.clone());
            }
        }
        for s in inner.sessions.values_mut() {
            let matching: Vec<u8> = s.subs.iter().filter(|(f, _)| topic_matches(f, &p.topic)).map(|(_, o)| *o).collect();
            if matching.is_empty() {
                continue;
            }
            let sub_qos = matching.iter().map(|o| o & 3).max().unwrap_or(0);
            let retain_as_published = matching.iter().any(|o| o & 0x08 != 0);
            let qos = p.qos.min(sub_qos);
            let mut props = p.props.clone();
            props.remove(props::TOPIC_ALIAS);
            let mut m = Publish { dup: false, qos, retain: p.retain && retain_as_published, topic: p.topic.clone(), pkid: 0, payload: p.payload.clone(), props };
            match s.conn.clone() {
                Some(c) => {
                    if qos > 0 {
                        m.pkid = s.pkid();
                    }
                    out.push((c, Packet::Publish(m)));
                }
                None if s.persistent && qos > 0 => s.queue.push_back(m),
                None => {}
            }
        }
    }
    for (c, packet) in out {
        if !c.send(&packet) {
            c.shutdown();
        }
    }
}

fn valid_credentials(state: &State, c: &Connect) -> bool {
    if state.users.is_empty() {
        return true;
    }
    let (Some(u), Some(p)) = (&c.username, &c.password) else { return false };
    state.users.iter().any(|(uu, pp)| uu == u && pp.as_bytes() == p.as_slice())
}

fn serve(state: &Arc<State>, stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let Ok(rs) = stream.try_clone() else { return };
    let mut reader = BufReader::new(rs);
    let connect = match read_packet(&mut reader, false, 1 << 20) {
        Ok(Some(Packet::Connect(c))) => c,
        _ => return,
    };
    let v5 = connect.level >= 5;
    let conn = Arc::new(BConn { id: state.conn_ids.fetch_add(1, Ordering::SeqCst), writer: Mutex::new(stream), v5 });
    let refuse = |code: u8| {
        conn.send(&Packet::ConnAck { session_present: false, code, props: Properties::new() });
        conn.shutdown();
    };
    if !(3..=5).contains(&connect.level) {
        refuse(if v5 { 0x84 } else { 1 });
        return;
    }
    if !valid_credentials(state, &connect) {
        refuse(if v5 { 0x86 } else { 4 });
        return;
    }
    let mut connack_props = Properties::new();
    let client_id = if connect.client_id.is_empty() {
        if !v5 && !connect.clean_session {
            refuse(2);
            return;
        }
        let id = format!("auto-{}", unique_id());
        if v5 {
            connack_props.push(props::ASSIGNED_CLIENT_IDENTIFIER, PropValue::Str(id.clone()));
        }
        id
    } else {
        connect.client_id.clone()
    };
    let persistent = if v5 {
        matches!(connect.props.get(props::SESSION_EXPIRY_INTERVAL), Some(PropValue::U32(e)) if *e > 0)
    } else {
        !connect.clean_session
    };
    // session setup (and takeover)
    let (session_present, queued, old) = {
        let mut inner = state.inner.lock().unwrap();
        let existing = inner.sessions.remove(&client_id);
        let old = existing.as_ref().and_then(|s| s.conn.clone());
        let (mut session, present) = match existing {
            Some(s) if !connect.clean_session => (s, true),
            _ => (Session { subs: BTreeMap::new(), conn: None, persistent, queue: VecDeque::new(), next_pkid: 0 }, false),
        };
        session.persistent = persistent;
        session.conn = Some(conn.clone());
        let mut queued: Vec<Packet> = Vec::new();
        while let Some(mut m) = session.queue.pop_front() {
            m.pkid = session.pkid();
            queued.push(Packet::Publish(m));
        }
        inner.sessions.insert(client_id.clone(), session);
        *inner.connects.entry(client_id.clone()).or_insert(0) += 1;
        (present, queued, old)
    };
    if let Some(o) = old {
        o.shutdown();
    }
    if !conn.send(&Packet::ConnAck { session_present, code: 0, props: connack_props }) {
        return;
    }
    for q in queued {
        conn.send(&q);
    }
    let keep_alive = Duration::from_secs(connect.keep_alive as u64);
    let read_timeout = if keep_alive.is_zero() { None } else { Some(keep_alive + keep_alive / 2) };
    let _ = reader.get_ref().set_read_timeout(read_timeout);
    let mut will = connect.will.clone();
    let mut inbound_qos2: HashSet<u16> = HashSet::new();
    loop {
        if !state.running.load(Ordering::SeqCst) {
            break;
        }
        let packet = match read_packet(&mut reader, v5, 256 * 1024 * 1024) {
            Ok(Some(p)) => p,
            _ => break,
        };
        match packet {
            Packet::Publish(p) => {
                if !valid_topic(&p.topic) {
                    break;
                }
                state.published.fetch_add(1, Ordering::SeqCst);
                match p.qos {
                    0 => route(state, p),
                    1 => {
                        let pkid = p.pkid;
                        route(state, p);
                        conn.send(&Packet::PubAck { pkid, reason: 0, props: Properties::new() });
                    }
                    _ => {
                        let pkid = p.pkid;
                        if inbound_qos2.insert(pkid) {
                            route(state, p);
                        }
                        conn.send(&Packet::PubRec { pkid, reason: 0, props: Properties::new() });
                    }
                }
            }
            Packet::PubRel { pkid, .. } => {
                inbound_qos2.remove(&pkid);
                conn.send(&Packet::PubComp { pkid, reason: 0, props: Properties::new() });
            }
            Packet::PubRec { pkid, .. } => {
                conn.send(&Packet::PubRel { pkid, reason: 0, props: Properties::new() });
            }
            Packet::PubAck { .. } | Packet::PubComp { .. } => {}
            Packet::Subscribe { pkid, filters, .. } => {
                let mut codes = Vec::new();
                let mut retained: Vec<Packet> = Vec::new();
                {
                    let mut inner = state.inner.lock().unwrap();
                    let msgs: Vec<Publish> = inner.retained.values().cloned().collect();
                    let Some(s) = inner.sessions.get_mut(&client_id) else { break };
                    for (f, opts) in filters {
                        if !valid_filter(&f) || opts & 3 == 3 {
                            codes.push(0x80);
                            continue;
                        }
                        let existed = s.subs.insert(f.clone(), opts).is_some();
                        codes.push(opts & 3);
                        let retain_handling = (opts >> 4) & 3;
                        if retain_handling == 2 || (retain_handling == 1 && existed) {
                            continue;
                        }
                        for m in msgs.iter().filter(|m| topic_matches(&f, &m.topic)) {
                            let qos = m.qos.min(opts & 3);
                            let pkid = if qos > 0 { s.pkid() } else { 0 };
                            retained.push(Packet::Publish(Publish { dup: false, qos, retain: true, topic: m.topic.clone(), pkid, payload: m.payload.clone(), props: m.props.clone() }));
                        }
                    }
                }
                conn.send(&Packet::SubAck { pkid, codes, props: Properties::new() });
                for r in retained {
                    conn.send(&r);
                }
            }
            Packet::Unsubscribe { pkid, filters, .. } => {
                let mut codes = Vec::new();
                {
                    let mut inner = state.inner.lock().unwrap();
                    if let Some(s) = inner.sessions.get_mut(&client_id) {
                        for f in &filters {
                            codes.push(if s.subs.remove(f).is_some() { 0x00 } else { 0x11 });
                        }
                    }
                }
                conn.send(&Packet::UnsubAck { pkid, codes, props: Properties::new() });
            }
            Packet::PingReq => {
                conn.send(&Packet::PingResp);
            }
            Packet::Disconnect { reason, .. } => {
                // v5 reason 0x04 = disconnect with will message
                if !(v5 && reason == 0x04) {
                    will = None;
                }
                break;
            }
            _ => break,
        }
    }
    conn.shutdown();
    // session cleanup (unless taken over by a newer connection)
    {
        let mut inner = state.inner.lock().unwrap();
        let mine = inner.sessions.get(&client_id).and_then(|s| s.conn.as_ref()).map(|c| c.id == conn.id).unwrap_or(false);
        if mine {
            let persistent = inner.sessions.get(&client_id).map(|s| s.persistent).unwrap_or(false);
            if persistent {
                if let Some(s) = inner.sessions.get_mut(&client_id) {
                    s.conn = None;
                }
            } else {
                inner.sessions.remove(&client_id);
            }
        }
    }
    if let Some(w) = will {
        if state.running.load(Ordering::SeqCst) {
            route(state, Publish { dup: false, qos: w.qos, retain: w.retain, topic: w.topic, pkid: 0, payload: w.payload, props: w.props });
        }
    }
}

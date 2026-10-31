//! An embedded, in-memory, single-node Kafka broker for tests.
//!
//! It speaks the same API versions as the client in [`crate::kafka::protocol`]:
//! ApiVersions, Metadata (with topic auto-creation), Produce (record batch v2 with CRC32C
//! validation, acks 0/1/-1), Fetch (long polling with `max_wait_ms`), ListOffsets,
//! FindCoordinator, JoinGroup/SyncGroup/Heartbeat/LeaveGroup (a group coordinator with
//! rebalancing, session timeouts and generations), OffsetCommit/OffsetFetch and
//! CreateTopics. Data is kept in memory and lost on stop.
//!
//! Security (for tests of `securityProtocol`): [`KafkaBrokerConfig::tls`] serves TLS
//! (`SSL` / `SASL_SSL`), [`KafkaBrokerConfig::sasl_users`] requires SASL authentication
//! (SaslHandshake v1 + SaslAuthenticate v1 with PLAIN, SCRAM-SHA-256 or SCRAM-SHA-512)
//! before any request other than ApiVersions.
//!
//! ```no_run
//! use camel_kafka_mqtt::testing::KafkaBroker;
//! let broker = KafkaBroker::start().unwrap();
//! broker.create_topic("orders", 3);
//! let uri = format!("kafka:orders?brokers={}", broker.bootstrap());
//! # let _ = uri;
//! broker.stop();
//! ```

use std::collections::{BTreeMap, HashMap};
use std::net::TcpStream;

use camel_tls::net::NetStream;
use camel_tls::sasl::{parse_plain, ScramCredentials, ScramMechanism, ScramServer};
use camel_tls::TlsServerConfig;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use camel_core::error::Result;

use crate::kafka::protocol::*;
use crate::net::{now_millis, unique_id, TcpServer};

const NODE_ID: i32 = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GroupState {
    Empty,
    PreparingRebalance,
    CompletingRebalance,
    Stable,
}

impl GroupState {
    fn name(&self) -> &'static str {
        match self {
            GroupState::Empty => "Empty",
            GroupState::PreparingRebalance => "PreparingRebalance",
            GroupState::CompletingRebalance => "CompletingRebalance",
            GroupState::Stable => "Stable",
        }
    }
}

struct Member {
    session_timeout: Duration,
    rebalance_timeout: Duration,
    protocols: Vec<(String, Vec<u8>)>,
    last_seen: Instant,
    /// Joined in the current rebalance.
    joined: bool,
    /// Generation of the completed join (answer for a waiting JoinGroup).
    join_result: Option<i32>,
    assignment: Vec<u8>,
}

struct Group {
    state: GroupState,
    generation: i32,
    protocol_type: String,
    protocol: String,
    leader: String,
    members: BTreeMap<String, Member>,
    rebalance_deadline: Instant,
    offsets: HashMap<(String, i32), (i64, Option<String>)>,
}

impl Group {
    fn new() -> Group {
        Group {
            state: GroupState::Empty,
            generation: 0,
            protocol_type: String::new(),
            protocol: String::new(),
            leader: String::new(),
            members: BTreeMap::new(),
            rebalance_deadline: Instant::now(),
            offsets: HashMap::new(),
        }
    }

    fn prepare_rebalance(&mut self) {
        if self.members.is_empty() {
            self.state = GroupState::Empty;
            return;
        }
        if self.state != GroupState::PreparingRebalance {
            self.state = GroupState::PreparingRebalance;
            for m in self.members.values_mut() {
                m.joined = false;
            }
            let max = self.members.values().map(|m| m.rebalance_timeout).max().unwrap_or(Duration::from_secs(10));
            self.rebalance_deadline = Instant::now() + max;
        }
    }

    fn try_complete(&mut self, force: bool) -> bool {
        if self.state != GroupState::PreparingRebalance {
            return false;
        }
        let all = self.members.values().all(|m| m.joined);
        if !all && !force {
            return false;
        }
        self.members.retain(|_, m| m.joined);
        if self.members.is_empty() {
            self.state = GroupState::Empty;
            return true;
        }
        self.generation += 1;
        // first protocol of the (first) member that every member supports
        let first = self.members.values().next().unwrap();
        self.protocol = first
            .protocols
            .iter()
            .map(|(n, _)| n.clone())
            .find(|n| self.members.values().all(|m| m.protocols.iter().any(|(p, _)| p == n)))
            .unwrap_or_default();
        if !self.members.contains_key(&self.leader) {
            self.leader = self.members.keys().next().cloned().unwrap_or_default();
        }
        let gen = self.generation;
        for m in self.members.values_mut() {
            m.join_result = Some(gen);
            m.assignment.clear();
        }
        self.state = GroupState::CompletingRebalance;
        true
    }

    fn expire(&mut self) -> bool {
        let now = Instant::now();
        let preparing = self.state == GroupState::PreparingRebalance;
        let before = self.members.len();
        self.members.retain(|_, m| (preparing && m.joined) || now.duration_since(m.last_seen) < m.session_timeout);
        if self.members.len() != before {
            if preparing {
                self.try_complete(false);
            } else {
                self.prepare_rebalance();
            }
            return true;
        }
        false
    }
}

#[derive(Default)]
struct Data {
    topics: BTreeMap<String, Vec<Vec<Record>>>,
    groups: HashMap<String, Group>,
    fetch_requests: HashMap<String, usize>,
    client_fetches: HashMap<String, usize>,
}

struct State {
    tls: Option<Arc<TlsServerConfig>>,
    sasl_users: Vec<(String, String)>,
    /// Successful SASL logins `(mechanism, user)`.
    sasl_logins: Mutex<Vec<(String, String)>>,
    port: Mutex<u16>,
    default_partitions: usize,
    auto_create: bool,
    running: AtomicBool,
    data: Mutex<Data>,
    cond: Condvar,
}

/// The embedded broker. Stops when dropped.
pub struct KafkaBroker {
    state: Arc<State>,
    server: TcpServer,
}

/// Broker settings.
#[derive(Clone, Debug)]
pub struct KafkaBrokerConfig {
    /// Partitions of auto-created topics (default 1).
    pub default_partitions: usize,
    /// Create unknown topics on metadata requests (default true).
    pub auto_create_topics: bool,
    /// Port to bind (default 0 = ephemeral).
    pub port: u16,
    /// Serve TLS with this configuration (`securityProtocol=SSL` / `SASL_SSL`).
    pub tls: Option<TlsServerConfig>,
    /// When non-empty, clients must authenticate with SASL as one of these `(user, password)`.
    pub sasl_users: Vec<(String, String)>,
}

impl Default for KafkaBrokerConfig {
    fn default() -> Self {
        KafkaBrokerConfig { default_partitions: 1, auto_create_topics: true, port: 0, tls: None, sasl_users: Vec::new() }
    }
}

impl KafkaBroker {
    /// Start a broker on an ephemeral port with default settings.
    pub fn start() -> Result<KafkaBroker> {
        Self::start_with(KafkaBrokerConfig::default())
    }

    /// Start a broker whose auto-created topics have `partitions` partitions.
    pub fn start_with_partitions(partitions: usize) -> Result<KafkaBroker> {
        Self::start_with(KafkaBrokerConfig { default_partitions: partitions.max(1), ..Default::default() })
    }

    /// Start a broker with explicit settings.
    pub fn start_with(cfg: KafkaBrokerConfig) -> Result<KafkaBroker> {
        let state = Arc::new(State {
            tls: cfg.tls.map(Arc::new),
            sasl_users: cfg.sasl_users,
            sasl_logins: Mutex::new(Vec::new()),
            port: Mutex::new(0),
            default_partitions: cfg.default_partitions.max(1),
            auto_create: cfg.auto_create_topics,
            running: AtomicBool::new(true),
            data: Mutex::new(Data::default()),
            cond: Condvar::new(),
        });
        let s2 = state.clone();
        let server = TcpServer::bind("127.0.0.1", cfg.port, "kafka-broker", move |stream| serve(&s2, stream))?;
        *state.port.lock().unwrap() = server.port();
        Ok(KafkaBroker { state, server })
    }

    /// The listening port.
    pub fn port(&self) -> u16 {
        self.server.port()
    }

    /// `127.0.0.1:<port>`, for the `brokers` option.
    pub fn bootstrap(&self) -> String {
        format!("127.0.0.1:{}", self.port())
    }

    /// Create a topic (no-op when it exists).
    pub fn create_topic(&self, name: &str, partitions: usize) {
        let mut d = self.state.data.lock().unwrap();
        d.topics.entry(name.to_string()).or_insert_with(|| vec![Vec::new(); partitions.max(1)]);
    }

    /// Topic names.
    pub fn topics(&self) -> Vec<String> {
        self.state.data.lock().unwrap().topics.keys().cloned().collect()
    }

    /// Number of partitions of a topic (None if unknown).
    pub fn partition_count(&self, topic: &str) -> Option<usize> {
        self.state.data.lock().unwrap().topics.get(topic).map(|p| p.len())
    }

    /// Records stored in a partition.
    pub fn records(&self, topic: &str, partition: i32) -> Vec<Record> {
        let d = self.state.data.lock().unwrap();
        d.topics.get(topic).and_then(|p| p.get(partition as usize)).cloned().unwrap_or_default()
    }

    /// Append a record directly (bypassing the network); returns its offset.
    pub fn append(&self, topic: &str, partition: i32, key: Option<&[u8]>, value: &[u8]) -> i64 {
        let mut d = self.state.data.lock().unwrap();
        let parts = d.topics.entry(topic.to_string()).or_insert_with(|| vec![Vec::new(); self.state.default_partitions]);
        let log = &mut parts[partition as usize];
        let offset = log.len() as i64;
        log.push(Record { offset, timestamp: now_millis(), key: key.map(|k| k.to_vec()), value: Some(value.to_vec()), headers: Vec::new() });
        self.state.cond.notify_all();
        offset
    }

    /// The committed offset of a group for a partition.
    pub fn committed_offset(&self, group: &str, topic: &str, partition: i32) -> Option<i64> {
        let d = self.state.data.lock().unwrap();
        d.groups.get(group).and_then(|g| g.offsets.get(&(topic.to_string(), partition))).map(|(o, _)| *o)
    }

    /// The state of a group (`Empty`, `PreparingRebalance`, `CompletingRebalance`, `Stable`).
    pub fn group_state(&self, group: &str) -> Option<String> {
        self.state.data.lock().unwrap().groups.get(group).map(|g| g.state.name().to_string())
    }

    /// Member ids of a group.
    pub fn group_members(&self, group: &str) -> Vec<String> {
        self.state.data.lock().unwrap().groups.get(group).map(|g| g.members.keys().cloned().collect()).unwrap_or_default()
    }

    /// The current generation of a group.
    pub fn group_generation(&self, group: &str) -> Option<i32> {
        self.state.data.lock().unwrap().groups.get(group).map(|g| g.generation)
    }

    /// The partition assignment of each member of a stable group.
    pub fn group_assignment(&self, group: &str) -> Vec<(String, Vec<(String, Vec<i32>)>)> {
        let d = self.state.data.lock().unwrap();
        d.groups
            .get(group)
            .map(|g| g.members.iter().map(|(id, m)| (id.clone(), decode_assignment(&m.assignment).unwrap_or_default())).collect())
            .unwrap_or_default()
    }

    /// Number of fetch requests that included a topic (to know a consumer is polling).
    pub fn fetch_requests(&self, topic: &str) -> usize {
        self.state.data.lock().unwrap().fetch_requests.get(topic).copied().unwrap_or(0)
    }

    /// Number of fetch requests sent by a client id (the consumer threads of a kafka
    /// endpoint use `<clientId>-<thread index>`).
    pub fn fetch_requests_by_client(&self, client_id: &str) -> usize {
        self.state.data.lock().unwrap().client_fetches.get(client_id).copied().unwrap_or(0)
    }

    /// Wait until `f` holds (checked every 10ms) or the timeout expires; returns `f()`.
    pub fn wait_until(&self, timeout: Duration, f: impl Fn(&KafkaBroker) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if f(self) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        f(self)
    }

    /// Successful SASL logins as `(mechanism, user)`.
    pub fn sasl_logins(&self) -> Vec<(String, String)> {
        self.state.sasl_logins.lock().unwrap().clone()
    }

    /// Number of open client connections.
    pub fn connection_count(&self) -> usize {
        self.server.connection_count()
    }

    /// Stop the broker and close all connections.
    pub fn stop(&self) {
        self.state.running.store(false, Ordering::SeqCst);
        {
            let _g = self.state.data.lock().unwrap();
            self.state.cond.notify_all();
        }
        self.server.stop();
    }
}

impl Drop for KafkaBroker {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Per-connection SASL state.
enum Auth {
    /// No SASL configured, or authentication completed.
    Done,
    /// Waiting for SaslHandshake.
    Handshake,
    /// Waiting for SaslAuthenticate with the given mechanism.
    Plain,
    Scram(Box<ScramServer<Box<dyn Fn(&str) -> Option<ScramCredentials> + Send>>>, bool),
}

fn sasl_step(state: &State, auth: &mut Auth, key: i16, body: &[u8]) -> Option<Vec<u8>> {
    let mut d = Decoder::new(body);
    match key {
        api::SASL_HANDSHAKE => {
            let mech = d.string().ok()?.to_ascii_uppercase();
            let supported = vec!["PLAIN".to_string(), "SCRAM-SHA-256".to_string(), "SCRAM-SHA-512".to_string()];
            let next = match mech.as_str() {
                "PLAIN" => Some(Auth::Plain),
                m => ScramMechanism::from_name(m).map(|mech| {
                    let users = state.sasl_users.clone();
                    let lookup: Box<dyn Fn(&str) -> Option<ScramCredentials> + Send> = Box::new(move |u: &str| {
                        users.iter().find(|(n, _)| n == u).map(|(_, p)| ScramCredentials::derive(mech, p, format!("salt-{u}").as_bytes(), 4096))
                    });
                    Auth::Scram(Box::new(ScramServer::new(mech, lookup)), false)
                }),
            };
            let error_code = match (&auth, next) {
                (Auth::Handshake, Some(n)) => {
                    *auth = n;
                    errors::NONE
                }
                (Auth::Handshake, None) => errors::UNSUPPORTED_SASL_MECHANISM,
                _ => errors::ILLEGAL_SASL_STATE,
            };
            Some(SaslHandshakeResponse { error_code, mechanisms: supported }.encode())
        }
        api::SASL_AUTHENTICATE => {
            let token = d.bytes().ok()?;
            let fail = |m: &str| SaslAuthenticateResponse { error_code: errors::SASL_AUTHENTICATION_FAILED, error_message: Some(m.to_string()), auth_bytes: Vec::new(), session_lifetime_ms: 0 };
            let ok = |b: Vec<u8>| SaslAuthenticateResponse { error_code: errors::NONE, error_message: None, auth_bytes: b, session_lifetime_ms: 0 };
            let resp = match auth {
                Auth::Plain => match parse_plain(&token) {
                    Some((_, u, p)) if state.sasl_users.iter().any(|(n, pw)| *n == u && *pw == p) => {
                        state.sasl_logins.lock().unwrap().push(("PLAIN".into(), u));
                        *auth = Auth::Done;
                        ok(Vec::new())
                    }
                    _ => fail("Authentication failed: Invalid username or password"),
                },
                Auth::Scram(server, first_done) => {
                    let text = String::from_utf8_lossy(&token).into_owned();
                    if !*first_done {
                        match server.handle_client_first(&text) {
                            Ok(sf) => {
                                *first_done = true;
                                ok(sf.into_bytes())
                            }
                            Err(e) => fail(&e.to_string()),
                        }
                    } else {
                        match server.handle_client_final(&text) {
                            Ok(fin) => {
                                let user = server.user().unwrap_or("").to_string();
                                state.sasl_logins.lock().unwrap().push(("SCRAM".into(), user));
                                *auth = Auth::Done;
                                ok(fin.into_bytes())
                            }
                            Err(_) => fail("Authentication failed during authentication due to invalid credentials with SASL mechanism SCRAM"),
                        }
                    }
                }
                _ => SaslAuthenticateResponse { error_code: errors::ILLEGAL_SASL_STATE, error_message: Some("SaslAuthenticate before SaslHandshake".into()), auth_bytes: Vec::new(), session_lifetime_ms: 0 },
            };
            let failed = resp.error_code != errors::NONE;
            let out = resp.encode();
            if failed {
                *auth = Auth::Handshake;
            }
            Some(out)
        }
        _ => None,
    }
}

fn serve(state: &Arc<State>, tcp: TcpStream) {
    let mut stream = match &state.tls {
        Some(cfg) => match NetStream::server(tcp, cfg) {
            Ok(s) => s,
            Err(_) => return,
        },
        None => NetStream::plain(tcp),
    };
    let mut auth = if state.sasl_users.is_empty() { Auth::Done } else { Auth::Handshake };
    loop {
        let frame = match read_frame(&mut stream, 64 * 1024 * 1024) {
            Ok(Some(f)) => f,
            _ => break,
        };
        if !state.running.load(Ordering::SeqCst) {
            break;
        }
        let Ok((header, body)) = decode_request_header(&frame) else { break };
        let expected = api::VERSIONS.iter().find(|(k, _)| *k == header.api_key).map(|(_, v)| *v);
        let response = if header.api_key == api::API_VERSIONS {
            let error_code = if header.api_version == 0 { errors::NONE } else { errors::UNSUPPORTED_VERSION };
            Some(ApiVersionsResponse { error_code, apis: api::VERSIONS.iter().map(|(k, v)| (*k, *v, *v)).collect() }.encode())
        } else if expected != Some(header.api_version) {
            break; // unsupported API or version: Kafka closes the connection
        } else if header.api_key == api::SASL_HANDSHAKE || header.api_key == api::SASL_AUTHENTICATE {
            match sasl_step(state, &mut auth, header.api_key, body) {
                Some(r) => Some(r),
                None => break,
            }
        } else if !matches!(auth, Auth::Done) {
            break; // unauthenticated request: Kafka closes the connection
        } else {
            match handle(state, header.api_key, body, header.client_id.as_deref().unwrap_or("")) {
                Ok(r) => r,
                Err(_) => break,
            }
        };
        if let Some(resp) = response {
            if write_frame(&mut stream, &encode_response(header.correlation_id, &resp)).is_err() {
                break;
            }
        }
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

fn valid_topic_name(t: &str) -> bool {
    !t.is_empty() && t.len() <= 249 && t != "." && t != ".." && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

fn wait<'a>(state: &State, g: MutexGuard<'a, Data>, d: Duration) -> MutexGuard<'a, Data> {
    state.cond.wait_timeout(g, d).unwrap().0
}

fn handle(state: &Arc<State>, key: i16, body: &[u8], client_id: &str) -> Result<Option<Vec<u8>>> {
    let port = *state.port.lock().unwrap() as i32;
    Ok(Some(match key {
        api::METADATA => {
            let req = MetadataRequest::decode(body)?;
            let mut d = state.data.lock().unwrap();
            let names: Vec<String> = match &req.topics {
                None => d.topics.keys().cloned().collect(),
                Some(t) => t.clone(),
            };
            let mut topics = Vec::new();
            for name in names {
                if !d.topics.contains_key(&name) && state.auto_create && req.allow_auto_topic_creation && valid_topic_name(&name) {
                    d.topics.insert(name.clone(), vec![Vec::new(); state.default_partitions]);
                }
                match d.topics.get(&name) {
                    Some(parts) => topics.push(TopicMeta {
                        error_code: errors::NONE,
                        name,
                        is_internal: false,
                        partitions: (0..parts.len())
                            .map(|p| PartitionMeta { error_code: 0, partition: p as i32, leader: NODE_ID, replicas: vec![NODE_ID], isr: vec![NODE_ID], offline: vec![] })
                            .collect(),
                    }),
                    None => {
                        let code = if valid_topic_name(&name) { errors::UNKNOWN_TOPIC_OR_PARTITION } else { errors::INVALID_TOPIC_EXCEPTION };
                        topics.push(TopicMeta { error_code: code, name, is_internal: false, partitions: vec![] })
                    }
                }
            }
            MetadataResponse {
                throttle_time_ms: 0,
                brokers: vec![BrokerMeta { node_id: NODE_ID, host: "127.0.0.1".into(), port, rack: None }],
                cluster_id: Some("rust-camel-embedded".into()),
                controller_id: NODE_ID,
                topics,
            }
            .encode()
        }
        api::PRODUCE => {
            let req = ProduceRequest::decode(body)?;
            let mut d = state.data.lock().unwrap();
            let mut out = Vec::new();
            for (topic, parts) in &req.topics {
                let mut presp = Vec::new();
                for (p, batch) in parts {
                    let mut r = ProducePartitionResponse { partition: *p, error_code: 0, base_offset: -1, log_append_time_ms: -1 };
                    match d.topics.get_mut(topic).and_then(|ps| ps.get_mut(*p as usize)) {
                        None => r.error_code = errors::UNKNOWN_TOPIC_OR_PARTITION,
                        Some(log) => match decode_record_batches(batch) {
                            Err(_) => r.error_code = errors::CORRUPT_MESSAGE,
                            Ok(records) => {
                                r.base_offset = log.len() as i64;
                                for mut rec in records {
                                    rec.offset = log.len() as i64;
                                    log.push(rec);
                                }
                            }
                        },
                    }
                    presp.push(r);
                }
                out.push((topic.clone(), presp));
            }
            state.cond.notify_all();
            if req.acks == 0 {
                return Ok(None);
            }
            ProduceResponse { topics: out, throttle_time_ms: 0 }.encode()
        }
        api::FETCH => {
            let req = FetchRequest::decode(body)?;
            let deadline = Instant::now() + Duration::from_millis(req.max_wait_ms.max(0) as u64);
            let mut d = state.data.lock().unwrap();
            for (t, _) in &req.topics {
                *d.fetch_requests.entry(t.clone()).or_insert(0) += 1;
            }
            *d.client_fetches.entry(client_id.to_string()).or_insert(0) += 1;
            loop {
                let available = req.topics.iter().any(|(t, ps)| {
                    ps.iter().any(|(p, off, _)| d.topics.get(t).and_then(|x| x.get(*p as usize)).map(|log| (log.len() as i64) > *off).unwrap_or(true))
                });
                let now = Instant::now();
                if available || now >= deadline || !state.running.load(Ordering::SeqCst) {
                    break;
                }
                d = wait(state, d, (deadline - now).min(Duration::from_millis(100)));
            }
            let mut budget = req.max_bytes.max(1) as usize;
            let mut topics = Vec::new();
            for (t, ps) in &req.topics {
                let mut presp = Vec::new();
                for (p, off, max) in ps {
                    let mut r = FetchPartitionResponse { partition: *p, ..Default::default() };
                    match d.topics.get(t).and_then(|x| x.get(*p as usize)) {
                        None => r.error_code = errors::UNKNOWN_TOPIC_OR_PARTITION,
                        Some(log) => {
                            let hw = log.len() as i64;
                            r.high_watermark = hw;
                            r.last_stable_offset = hw;
                            if *off < 0 || *off > hw {
                                r.error_code = errors::OFFSET_OUT_OF_RANGE;
                            } else {
                                let limit = (*max).max(1) as usize;
                                let mut size = 0usize;
                                let mut recs = Vec::new();
                                for rec in &log[*off as usize..] {
                                    let s = 32 + rec.value.as_ref().map_or(0, |v| v.len()) + rec.key.as_ref().map_or(0, |k| k.len());
                                    if !recs.is_empty() && (size + s > limit || size + s > budget) {
                                        break;
                                    }
                                    size += s;
                                    recs.push(rec.clone());
                                }
                                budget = budget.saturating_sub(size);
                                if !recs.is_empty() {
                                    r.records = encode_record_batch(*off, &recs);
                                }
                            }
                        }
                    }
                    presp.push(r);
                }
                topics.push((t.clone(), presp));
            }
            FetchResponse { throttle_time_ms: 0, topics }.encode()
        }
        api::LIST_OFFSETS => {
            let req = ListOffsetsRequest::decode(body)?;
            let d = state.data.lock().unwrap();
            let mut topics = Vec::new();
            for (t, ps) in &req.topics {
                let mut out = Vec::new();
                for (p, ts) in ps {
                    match d.topics.get(t).and_then(|x| x.get(*p as usize)) {
                        None => out.push((*p, errors::UNKNOWN_TOPIC_OR_PARTITION, -1, -1)),
                        Some(log) => {
                            let off = match *ts {
                                EARLIEST_TIMESTAMP => 0,
                                LATEST_TIMESTAMP => log.len() as i64,
                                ts => log.iter().find(|r| r.timestamp >= ts).map(|r| r.offset).unwrap_or(-1),
                            };
                            out.push((*p, errors::NONE, -1, off));
                        }
                    }
                }
                topics.push((t.clone(), out));
            }
            ListOffsetsResponse { topics }.encode()
        }
        api::FIND_COORDINATOR => {
            let _req = FindCoordinatorRequest::decode(body)?;
            FindCoordinatorResponse { throttle_time_ms: 0, error_code: 0, error_message: None, node_id: NODE_ID, host: "127.0.0.1".into(), port }.encode()
        }
        api::CREATE_TOPICS => {
            let req = CreateTopicsRequest::decode(body)?;
            let mut d = state.data.lock().unwrap();
            let mut out = Vec::new();
            for t in req.topics {
                let n = if t.num_partitions < 0 { state.default_partitions as i32 } else { t.num_partitions };
                if !valid_topic_name(&t.name) {
                    out.push((t.name, errors::INVALID_TOPIC_EXCEPTION, Some("Invalid topic name".to_string())));
                } else if d.topics.contains_key(&t.name) {
                    out.push((t.name.clone(), errors::TOPIC_ALREADY_EXISTS, Some(format!("Topic '{}' already exists.", t.name))));
                } else if n <= 0 {
                    out.push((t.name, errors::INVALID_PARTITIONS, Some("Number of partitions must be larger than 0.".to_string())));
                } else {
                    if !req.validate_only {
                        d.topics.insert(t.name.clone(), vec![Vec::new(); n as usize]);
                    }
                    out.push((t.name, errors::NONE, None));
                }
            }
            CreateTopicsResponse { throttle_time_ms: 0, topics: out }.encode()
        }
        api::JOIN_GROUP => join_group(state, JoinGroupRequest::decode(body)?).encode(),
        api::SYNC_GROUP => sync_group(state, SyncGroupRequest::decode(body)?).encode(),
        api::HEARTBEAT => {
            let req = HeartbeatRequest::decode(body)?;
            let mut d = state.data.lock().unwrap();
            let code = match d.groups.get_mut(&req.group_id) {
                None => errors::UNKNOWN_MEMBER_ID,
                Some(g) => {
                    if g.expire() {
                        state.cond.notify_all();
                    }
                    match g.members.get_mut(&req.member_id) {
                        None => errors::UNKNOWN_MEMBER_ID,
                        Some(m) => {
                            m.last_seen = Instant::now();
                            if req.generation_id != g.generation {
                                errors::ILLEGAL_GENERATION
                            } else if g.state == GroupState::PreparingRebalance {
                                errors::REBALANCE_IN_PROGRESS
                            } else {
                                errors::NONE
                            }
                        }
                    }
                }
            };
            SimpleResponse { throttle_time_ms: 0, error_code: code }.encode()
        }
        api::LEAVE_GROUP => {
            let req = LeaveGroupRequest::decode(body)?;
            let mut d = state.data.lock().unwrap();
            let code = match d.groups.get_mut(&req.group_id).filter(|g| g.members.contains_key(&req.member_id)) {
                Some(g) => {
                    g.members.remove(&req.member_id);
                    if g.state == GroupState::PreparingRebalance {
                        if g.members.is_empty() {
                            g.state = GroupState::Empty;
                        } else {
                            g.try_complete(false);
                        }
                    } else {
                        g.prepare_rebalance();
                    }
                    errors::NONE
                }
                _ => errors::UNKNOWN_MEMBER_ID,
            };
            state.cond.notify_all();
            SimpleResponse { throttle_time_ms: 0, error_code: code }.encode()
        }
        api::OFFSET_COMMIT => {
            let req = OffsetCommitRequest::decode(body)?;
            let mut d = state.data.lock().unwrap();
            let g = d.groups.entry(req.group_id.clone()).or_insert_with(Group::new);
            let code = if req.generation_id < 0 {
                errors::NONE
            } else {
                match g.members.get_mut(&req.member_id) {
                    None => errors::UNKNOWN_MEMBER_ID,
                    Some(_) if req.generation_id != g.generation => errors::ILLEGAL_GENERATION,
                    Some(_) if g.state == GroupState::CompletingRebalance => errors::REBALANCE_IN_PROGRESS,
                    Some(m) => {
                        m.last_seen = Instant::now();
                        errors::NONE
                    }
                }
            };
            let mut topics = Vec::new();
            for (t, ps) in &req.topics {
                let mut out = Vec::new();
                for (p, off, meta) in ps {
                    if code == errors::NONE {
                        g.offsets.insert((t.clone(), *p), (*off, meta.clone()));
                    }
                    out.push((*p, code));
                }
                topics.push((t.clone(), out));
            }
            OffsetCommitResponse { topics }.encode()
        }
        api::OFFSET_FETCH => {
            let req = OffsetFetchRequest::decode(body)?;
            let d = state.data.lock().unwrap();
            let g = d.groups.get(&req.group_id);
            let topics = req
                .topics
                .iter()
                .map(|(t, ps)| {
                    let parts = ps
                        .iter()
                        .map(|p| match g.and_then(|g| g.offsets.get(&(t.clone(), *p))) {
                            Some((off, meta)) => (*p, *off, meta.clone(), errors::NONE),
                            None => (*p, -1, None, errors::NONE),
                        })
                        .collect();
                    (t.clone(), parts)
                })
                .collect();
            OffsetFetchResponse { topics }.encode()
        }
        _ => return Err(camel_core::error::CamelError::illegal_argument("unsupported api")),
    }))
}

fn join_group(state: &Arc<State>, req: JoinGroupRequest) -> JoinGroupResponse {
    let err = |code: i16, member: &str| JoinGroupResponse { error_code: code, generation_id: -1, member_id: member.to_string(), ..Default::default() };
    if req.group_id.is_empty() {
        return err(errors::INVALID_REQUEST, &req.member_id);
    }
    if req.session_timeout_ms < 1 || req.session_timeout_ms > 30 * 60 * 1000 {
        return err(errors::INVALID_SESSION_TIMEOUT, &req.member_id);
    }
    let mut d = state.data.lock().unwrap();
    let member_id;
    {
        let g = d.groups.entry(req.group_id.clone()).or_insert_with(Group::new);
        g.expire();
        if !g.members.is_empty() && g.protocol_type != req.protocol_type {
            return err(errors::INCONSISTENT_GROUP_PROTOCOL, &req.member_id);
        }
        if !req.member_id.is_empty() && !g.members.contains_key(&req.member_id) {
            return err(errors::UNKNOWN_MEMBER_ID, &req.member_id);
        }
        member_id = if req.member_id.is_empty() { format!("member-{}", unique_id()) } else { req.member_id.clone() };
        g.protocol_type = req.protocol_type.clone();
        g.prepare_rebalance_for_join();
        let rebalance_timeout = Duration::from_millis(req.rebalance_timeout_ms.max(req.session_timeout_ms).max(1) as u64);
        let m = g.members.entry(member_id.clone()).or_insert_with(|| Member {
            session_timeout: Duration::from_millis(req.session_timeout_ms as u64),
            rebalance_timeout,
            protocols: Vec::new(),
            last_seen: Instant::now(),
            joined: false,
            join_result: None,
            assignment: Vec::new(),
        });
        m.session_timeout = Duration::from_millis(req.session_timeout_ms as u64);
        m.rebalance_timeout = rebalance_timeout;
        m.protocols = req.protocols.clone();
        m.last_seen = Instant::now();
        m.joined = true;
        m.join_result = None;
        let deadline = Instant::now() + rebalance_timeout;
        if g.rebalance_deadline < deadline && g.members.len() == 1 {
            g.rebalance_deadline = deadline;
        }
        g.try_complete(false);
    }
    state.cond.notify_all();
    loop {
        if !state.running.load(Ordering::SeqCst) {
            return err(errors::COORDINATOR_NOT_AVAILABLE, &member_id);
        }
        let g = d.groups.get_mut(&req.group_id).unwrap();
        if g.expire() {
            state.cond.notify_all();
        }
        let Some(m) = g.members.get(&member_id) else { return err(errors::UNKNOWN_MEMBER_ID, &member_id) };
        if let Some(gen) = m.join_result {
            if gen == g.generation {
                let members = if g.leader == member_id {
                    let proto = g.protocol.clone();
                    g.members
                        .iter()
                        .map(|(id, m)| (id.clone(), m.protocols.iter().find(|(n, _)| *n == proto).map(|(_, md)| md.clone()).unwrap_or_default()))
                        .collect()
                } else {
                    Vec::new()
                };
                return JoinGroupResponse {
                    throttle_time_ms: 0,
                    error_code: errors::NONE,
                    generation_id: gen,
                    protocol_name: g.protocol.clone(),
                    leader: g.leader.clone(),
                    member_id: member_id.clone(),
                    members,
                };
            }
        }
        if g.state == GroupState::PreparingRebalance && Instant::now() >= g.rebalance_deadline {
            g.try_complete(true);
            state.cond.notify_all();
            continue;
        }
        d = wait(state, d, Duration::from_millis(20));
    }
}

impl Group {
    fn prepare_rebalance_for_join(&mut self) {
        if self.state != GroupState::PreparingRebalance {
            // an empty group starts a fresh rebalance; members of a stable group must rejoin
            if self.members.is_empty() {
                self.state = GroupState::PreparingRebalance;
                self.rebalance_deadline = Instant::now();
            } else {
                self.prepare_rebalance();
            }
        }
    }
}

fn sync_group(state: &Arc<State>, req: SyncGroupRequest) -> SyncGroupResponse {
    let resp = |code: i16, a: Vec<u8>| SyncGroupResponse { throttle_time_ms: 0, error_code: code, assignment: a };
    let mut d = state.data.lock().unwrap();
    let deadline;
    {
        let Some(g) = d.groups.get_mut(&req.group_id) else { return resp(errors::UNKNOWN_MEMBER_ID, vec![]) };
        let Some(m) = g.members.get_mut(&req.member_id) else { return resp(errors::UNKNOWN_MEMBER_ID, vec![]) };
        m.last_seen = Instant::now();
        deadline = Instant::now() + m.rebalance_timeout;
        if req.generation_id != g.generation {
            return resp(errors::ILLEGAL_GENERATION, vec![]);
        }
        if g.state == GroupState::PreparingRebalance {
            return resp(errors::REBALANCE_IN_PROGRESS, vec![]);
        }
        if g.state == GroupState::CompletingRebalance && g.leader == req.member_id {
            for (mid, a) in &req.assignments {
                if let Some(m) = g.members.get_mut(mid) {
                    m.assignment = a.clone();
                }
            }
            g.state = GroupState::Stable;
            state.cond.notify_all();
        }
    }
    loop {
        if !state.running.load(Ordering::SeqCst) {
            return resp(errors::COORDINATOR_NOT_AVAILABLE, vec![]);
        }
        let g = d.groups.get_mut(&req.group_id).unwrap();
        let Some(m) = g.members.get_mut(&req.member_id) else { return resp(errors::UNKNOWN_MEMBER_ID, vec![]) };
        m.last_seen = Instant::now();
        if g.generation != req.generation_id {
            return resp(errors::ILLEGAL_GENERATION, vec![]);
        }
        match g.state {
            GroupState::Stable => return resp(errors::NONE, m.assignment.clone()),
            GroupState::PreparingRebalance | GroupState::Empty => return resp(errors::REBALANCE_IN_PROGRESS, vec![]),
            GroupState::CompletingRebalance => {}
        }
        if Instant::now() >= deadline {
            g.prepare_rebalance();
            return resp(errors::REBALANCE_IN_PROGRESS, vec![]);
        }
        d = wait(state, d, Duration::from_millis(20));
    }
}

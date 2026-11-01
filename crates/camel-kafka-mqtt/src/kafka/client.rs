//! A small blocking Kafka client over `std::net`: bootstrap, ApiVersions check, metadata
//! cache, produce, fetch, list offsets, group coordination and offset management, topic
//! creation. A client is not thread safe by itself (wrap it in a `Mutex` to share it).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use camel_tls::camel_support::EndpointTls;
use camel_tls::net::NetStream;
use camel_tls::sasl::{plain_initial_response, ScramClient, ScramMechanism};

use camel_core::error::{CamelError, Result};

use super::protocol::*;
use crate::net::{connect, io_error, split_host_port};

/// Produces the single client message of a custom one-step SASL mechanism for a broker host
/// (e.g. `AWS_MSK_IAM`, registered by `camel-cloud`).
pub type SaslTokenProvider = Arc<dyn Fn(&str) -> Result<Vec<u8>> + Send + Sync>;

/// Builds a [`SaslTokenProvider`] from the endpoint's `saslJaasConfig`.
pub type SaslMechanismFactory = Arc<dyn Fn(&str) -> Result<SaslTokenProvider> + Send + Sync>;

fn sasl_registry() -> &'static Mutex<HashMap<String, SaslMechanismFactory>> {
    static R: OnceLock<Mutex<HashMap<String, SaslMechanismFactory>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register a custom one-step SASL mechanism (`saslMechanism=<name>`): the factory receives
/// the `saslJaasConfig` text and returns the token provider. Re-registering replaces it.
pub fn register_sasl_mechanism(name: &str, factory: SaslMechanismFactory) {
    sasl_registry().lock().unwrap().insert(name.to_ascii_uppercase(), factory);
}

/// The factory of a registered custom SASL mechanism.
pub fn sasl_mechanism(name: &str) -> Option<SaslMechanismFactory> {
    sasl_registry().lock().unwrap().get(&name.to_ascii_uppercase()).cloned()
}

/// SASL mechanism and credentials (`SASL_PLAINTEXT` / `SASL_SSL`).
#[derive(Clone)]
pub struct SaslConfig {
    /// `PLAIN`, `SCRAM-SHA-256`, `SCRAM-SHA-512` or a registered custom mechanism.
    pub mechanism: String,
    pub username: String,
    pub password: String,
    /// Token provider of a custom one-step mechanism (see [`register_sasl_mechanism`]).
    pub token_provider: Option<SaslTokenProvider>,
}

impl std::fmt::Debug for SaslConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SaslConfig").field("mechanism", &self.mechanism).field("username", &self.username).field("password", &"****").field("custom", &self.token_provider.is_some()).finish()
    }
}

impl SaslConfig {
    /// Credentials from a JAAS line such as
    /// `org.apache.kafka.common.security.plain.PlainLoginModule required username="u" password="p";`.
    pub fn from_jaas(mechanism: &str, jaas: &str) -> Result<SaslConfig> {
        let value = |key: &str| -> Option<String> {
            let i = jaas.find(&format!("{}=", key))? + key.len() + 1;
            let rest = &jaas[i..];
            if let Some(r) = rest.strip_prefix('"') {
                r.find('"').map(|e| r[..e].to_string())
            } else {
                Some(rest.split(|c: char| c.is_whitespace() || c == ';').next().unwrap_or("").to_string())
            }
        };
        let username = value("username").ok_or_else(|| CamelError::illegal_argument("saslJaasConfig: missing username=\"...\""))?;
        let password = value("password").ok_or_else(|| CamelError::illegal_argument("saslJaasConfig: missing password=\"...\""))?;
        Ok(SaslConfig { mechanism: mechanism.to_ascii_uppercase(), username, password, token_provider: None })
    }

    /// A custom one-step mechanism.
    pub fn custom(mechanism: &str, provider: SaslTokenProvider) -> SaslConfig {
        SaslConfig { mechanism: mechanism.to_ascii_uppercase(), username: String::new(), password: String::new(), token_provider: Some(provider) }
    }
}

/// Client settings.
#[derive(Clone, Debug)]
pub struct ClientConfig {
    /// Bootstrap brokers `(host, port)`.
    pub bootstrap: Vec<(String, u16)>,
    pub client_id: String,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    /// TLS (`SSL` / `SASL_SSL`).
    pub tls: Option<EndpointTls>,
    /// SASL authentication (`SASL_PLAINTEXT` / `SASL_SSL`).
    pub sasl: Option<SaslConfig>,
    /// Compression of produced record batches.
    pub compression: Compression,
}

impl ClientConfig {
    /// Parse a comma separated `host:port` broker list.
    pub fn new(brokers: &str, client_id: &str) -> Result<ClientConfig> {
        let mut bootstrap = Vec::new();
        for b in brokers.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let b = ["PLAINTEXT://", "SSL://", "SASL_SSL://", "SASL_PLAINTEXT://"].iter().find_map(|p| b.strip_prefix(p)).unwrap_or(b);
            bootstrap.push(split_host_port(b, 9092)?);
        }
        if bootstrap.is_empty() {
            return Err(CamelError::illegal_argument("No Kafka brokers configured (option brokers=host:port)"));
        }
        Ok(ClientConfig {
            bootstrap,
            client_id: client_id.to_string(),
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(30),
            tls: None,
            sasl: None,
            compression: Compression::None,
        })
    }
}

struct Conn {
    stream: NetStream,
    correlation: i32,
}

/// The node to send a request to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Node {
    /// Any broker (a bootstrap broker or a known one).
    Any,
    /// A broker by node id.
    Id(i32),
}

/// Group coordination result of a successful JoinGroup.
pub type JoinResult = JoinGroupResponse;

/// A blocking Kafka client.
pub struct KafkaClient {
    cfg: ClientConfig,
    conns: HashMap<i32, Conn>,
    bootstrap: Option<Conn>,
    brokers: HashMap<i32, (String, u16)>,
    topics: HashMap<String, Vec<PartitionMeta>>,
}

impl KafkaClient {
    pub fn new(cfg: ClientConfig) -> KafkaClient {
        KafkaClient { cfg, conns: HashMap::new(), bootstrap: None, brokers: HashMap::new(), topics: HashMap::new() }
    }

    pub fn config(&self) -> &ClientConfig {
        &self.cfg
    }

    /// Close all connections.
    pub fn close(&mut self) {
        for (_, c) in self.conns.drain() {
            let _ = c.stream.shutdown(std::net::Shutdown::Both);
        }
        if let Some(c) = self.bootstrap.take() {
            let _ = c.stream.shutdown(std::net::Shutdown::Both);
        }
    }

    fn open(&self, host: &str, port: u16) -> Result<Conn> {
        let what = format!("{}:{}", host, port);
        let tcp = connect(host, port, self.cfg.connect_timeout)?;
        let _ = tcp.set_write_timeout(Some(self.cfg.request_timeout));
        let stream = match &self.cfg.tls {
            Some(t) => t.handshake_client(tcp, host).map_err(|e| e.with_cause(CamelError::io(format!("TLS connection to Kafka broker {} failed", what))))?,
            None => NetStream::plain(tcp),
        };
        let mut conn = Conn { stream, correlation: 0 };
        // check that the broker supports the versions used by this client
        let body = Self::roundtrip(&mut conn, &self.cfg, api::API_VERSIONS, &[], self.cfg.request_timeout, &format!("{}:{}", host, port))?;
        let resp = ApiVersionsResponse::decode(&body)?;
        if resp.error_code != errors::NONE {
            return Err(kafka_error(resp.error_code, "ApiVersions"));
        }
        for (key, version) in api::VERSIONS {
            let ok = resp.apis.iter().any(|(k, min, max)| k == key && min <= version && version <= max);
            if !ok && !api::optional(*key) {
                return Err(CamelError::new(
                    "UnsupportedVersionException",
                    format!("Broker {}:{} does not support {} v{}", host, port, api::name(*key), version),
                ));
            }
        }
        if let Some(sasl) = &self.cfg.sasl {
            Self::authenticate(&mut conn, &self.cfg, sasl, host, &what)?;
        }
        Ok(conn)
    }

    /// SaslHandshake v1 + SaslAuthenticate v1 (KIP-152) with PLAIN or SCRAM.
    fn authenticate(conn: &mut Conn, cfg: &ClientConfig, sasl: &SaslConfig, host: &str, what: &str) -> Result<()> {
        let auth_fail = |msg: String| CamelError::new("SaslAuthenticationException", msg);
        let mut e = Encoder::new();
        e.string(&sasl.mechanism);
        let resp = SaslHandshakeResponse::decode(&Self::roundtrip(conn, cfg, api::SASL_HANDSHAKE, &e.buf, cfg.request_timeout, what)?)?;
        if resp.error_code != errors::NONE {
            return Err(auth_fail(format!(
                "Kafka broker {} does not support SASL mechanism {} (enabled: {})",
                what,
                sasl.mechanism,
                resp.mechanisms.join(", ")
            )));
        }
        let mut step = |token: &[u8]| -> Result<Vec<u8>> {
            let mut e = Encoder::new();
            e.bytes(token);
            let r = SaslAuthenticateResponse::decode(&Self::roundtrip(conn, cfg, api::SASL_AUTHENTICATE, &e.buf, cfg.request_timeout, what)?)?;
            if r.error_code != errors::NONE {
                return Err(auth_fail(format!(
                    "Authentication failed for {} at Kafka broker {}: {}",
                    if sasl.username.is_empty() { format!("mechanism {}", sasl.mechanism) } else { format!("user {}", sasl.username) },
                    what,
                    r.error_message.unwrap_or_else(|| errors::name(r.error_code).to_string())
                )));
            }
            Ok(r.auth_bytes)
        };
        if let Some(p) = &sasl.token_provider {
            step(&p(host)?)?;
            return Ok(());
        }
        match sasl.mechanism.as_str() {
            "PLAIN" => {
                step(&plain_initial_response("", &sasl.username, &sasl.password))?;
            }
            m => {
                let mech = ScramMechanism::from_name(m)
                    .ok_or_else(|| CamelError::illegal_argument(format!("Unsupported saslMechanism: {} (PLAIN, SCRAM-SHA-256, SCRAM-SHA-512)", m)))?;
                let mut c = ScramClient::new(mech, &sasl.username, &sasl.password);
                let server_first = step(c.client_first().as_bytes())?;
                let client_final = c.handle_server_first(&String::from_utf8_lossy(&server_first)).map_err(|e| auth_fail(e.to_string()))?;
                let server_final = step(client_final.as_bytes())?;
                c.verify_server_final(&String::from_utf8_lossy(&server_final)).map_err(|e| auth_fail(e.to_string()))?;
            }
        }
        Ok(())
    }

    fn roundtrip(conn: &mut Conn, cfg: &ClientConfig, key: i16, body: &[u8], timeout: Duration, what: &str) -> Result<Vec<u8>> {
        conn.correlation = conn.correlation.wrapping_add(1);
        let id = conn.correlation;
        let header = RequestHeader { api_key: key, api_version: api::version(key), correlation_id: id, client_id: Some(cfg.client_id.clone()) };
        let frame = encode_request(&header, body);
        write_frame(&mut conn.stream, &frame).map_err(|e| io_error(e, what))?;
        let _ = conn.stream.set_read_timeout(Some(timeout));
        let resp = read_frame(&mut conn.stream, 256 * 1024 * 1024)
            .map_err(|e| io_error(e, what))?
            .ok_or_else(|| CamelError::io(format!("Connection closed by Kafka broker {} during {}", what, api::name(key))))?;
        let mut d = Decoder::new(&resp);
        let cid = d.i32()?;
        if cid != id {
            return Err(CamelError::new(KAFKA_EXCEPTION, format!("Correlation id mismatch: expected {} got {}", id, cid)));
        }
        Ok(resp[4..].to_vec())
    }

    fn conn_for(&mut self, node: Node) -> Result<(&mut Conn, String)> {
        match node {
            Node::Id(id) if self.brokers.contains_key(&id) => {
                let (host, port) = self.brokers[&id].clone();
                if !self.conns.contains_key(&id) {
                    let c = self.open(&host, port)?;
                    self.conns.insert(id, c);
                }
                Ok((self.conns.get_mut(&id).unwrap(), format!("{}:{}", host, port)))
            }
            _ => {
                if self.bootstrap.is_none() {
                    let mut last = None;
                    let mut found = None;
                    for (h, p) in self.cfg.bootstrap.clone() {
                        match self.open(&h, p) {
                            Ok(c) => {
                                found = Some(c);
                                break;
                            }
                            Err(e) => last = Some(e),
                        }
                    }
                    match found {
                        Some(c) => self.bootstrap = Some(c),
                        None => return Err(last.unwrap_or_else(|| CamelError::io("No Kafka broker available"))),
                    }
                }
                let what = self.bootstrap.as_ref().and_then(|c| c.stream.peer_addr().ok()).map(|a| a.to_string()).unwrap_or_default();
                Ok((self.bootstrap.as_mut().unwrap(), what))
            }
        }
    }

    fn drop_conn(&mut self, node: Node) {
        let c = match node {
            Node::Id(id) if self.conns.contains_key(&id) => self.conns.remove(&id),
            _ => self.bootstrap.take(),
        };
        if let Some(c) = c {
            let _ = c.stream.shutdown(std::net::Shutdown::Both);
        }
    }

    /// Send a request and return the response body. On an I/O failure the connection is
    /// dropped (and re-opened on the next request).
    pub fn send(&mut self, node: Node, key: i16, body: &[u8], extra_wait: Duration) -> Result<Vec<u8>> {
        let cfg = self.cfg.clone();
        let timeout = cfg.request_timeout + extra_wait;
        let r = {
            let (conn, what) = self.conn_for(node)?;
            Self::roundtrip(conn, &cfg, key, body, timeout, &what)
        };
        if r.is_err() {
            self.drop_conn(node);
        }
        r
    }

    /// Send a request that has no response (Produce with acks=0).
    fn send_no_response(&mut self, node: Node, key: i16, body: &[u8]) -> Result<()> {
        let cfg = self.cfg.clone();
        let r = {
            let (conn, what) = self.conn_for(node)?;
            conn.correlation = conn.correlation.wrapping_add(1);
            let header = RequestHeader { api_key: key, api_version: api::version(key), correlation_id: conn.correlation, client_id: Some(cfg.client_id.clone()) };
            write_frame(&mut conn.stream, &encode_request(&header, body)).map_err(|e| io_error(e, &what))
        };
        if r.is_err() {
            self.drop_conn(node);
        }
        r
    }

    // -----------------------------------------------------------------------
    // metadata
    // -----------------------------------------------------------------------

    /// Fetch metadata (None = all topics) and update the caches.
    pub fn metadata(&mut self, topics: Option<&[String]>, auto_create: bool) -> Result<MetadataResponse> {
        let req = MetadataRequest { topics: topics.map(|t| t.to_vec()), allow_auto_topic_creation: auto_create };
        let body = self.send(Node::Any, api::METADATA, &req.encode(), Duration::ZERO)?;
        let resp = MetadataResponse::decode(&body)?;
        for b in &resp.brokers {
            self.brokers.insert(b.node_id, (b.host.clone(), b.port as u16));
        }
        for t in &resp.topics {
            if t.error_code == errors::NONE && !t.partitions.is_empty() {
                let mut parts = t.partitions.clone();
                parts.sort_by_key(|p| p.partition);
                self.topics.insert(t.name.clone(), parts);
            } else {
                self.topics.remove(&t.name);
            }
        }
        Ok(resp)
    }

    /// The partitions of a topic (from the cache, or fetched; auto-creates when the
    /// broker allows it). Retries briefly while leaders are not yet available.
    pub fn partitions(&mut self, topic: &str, refresh: bool) -> Result<Vec<PartitionMeta>> {
        if !refresh {
            if let Some(p) = self.topics.get(topic) {
                return Ok(p.clone());
            }
        }
        let mut last = errors::UNKNOWN_TOPIC_OR_PARTITION;
        for attempt in 0..10 {
            let resp = self.metadata(Some(&[topic.to_string()]), true)?;
            match resp.topics.iter().find(|t| t.name == topic) {
                Some(t) if t.error_code == errors::NONE && !t.partitions.is_empty() && t.partitions.iter().all(|p| p.leader >= 0) => {
                    return Ok(self.topics.get(topic).cloned().unwrap_or_default());
                }
                Some(t) => last = if t.error_code == errors::NONE { errors::LEADER_NOT_AVAILABLE } else { t.error_code },
                None => {}
            }
            if !errors::is_retriable(last) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20 * (attempt + 1)));
        }
        Err(kafka_error(last, &format!("Metadata for topic {}", topic)))
    }

    /// All topic names known to the cluster (non-internal).
    pub fn list_topics(&mut self) -> Result<Vec<String>> {
        let resp = self.metadata(None, false)?;
        Ok(resp.topics.into_iter().filter(|t| !t.is_internal && t.error_code == errors::NONE).map(|t| t.name).collect())
    }

    fn leader(&mut self, topic: &str, partition: i32) -> Result<i32> {
        let parts = self.partitions(topic, false)?;
        parts
            .iter()
            .find(|p| p.partition == partition)
            .map(|p| p.leader)
            .ok_or_else(|| kafka_error(errors::UNKNOWN_TOPIC_OR_PARTITION, &format!("Partition {}-{}", topic, partition)))
    }

    // -----------------------------------------------------------------------
    // produce / fetch / offsets
    // -----------------------------------------------------------------------

    /// Produce records to one partition. Returns `(base offset, log append time)`;
    /// with `acks == 0` the broker does not answer and `(-1, -1)` is returned.
    pub fn produce(&mut self, topic: &str, partition: i32, records: &[Record], acks: i16, timeout_ms: i32) -> Result<(i64, i64)> {
        let batch = encode_record_batch_with(0, records, self.cfg.compression)?;
        let req = ProduceRequest { transactional_id: None, acks, timeout_ms, topics: vec![(topic.to_string(), vec![(partition, batch)])] };
        let body = req.encode();
        let mut last = None;
        for attempt in 0..4 {
            let leader = self.leader(topic, partition)?;
            if acks == 0 {
                return self.send_no_response(Node::Id(leader), api::PRODUCE, &body).map(|_| (-1, -1));
            }
            let resp = match self.send(Node::Id(leader), api::PRODUCE, &body, Duration::from_millis(timeout_ms.max(0) as u64)) {
                Ok(b) => ProduceResponse::decode(&b)?,
                Err(e) => {
                    last = Some(e);
                    self.topics.remove(topic);
                    continue;
                }
            };
            let p = resp
                .topics
                .iter()
                .find(|(t, _)| t == topic)
                .and_then(|(_, ps)| ps.iter().find(|p| p.partition == partition))
                .cloned()
                .ok_or_else(|| CamelError::new(KAFKA_EXCEPTION, "Produce response without the partition"))?;
            if p.error_code == errors::NONE {
                return Ok((p.base_offset, p.log_append_time_ms));
            }
            let err = kafka_error(p.error_code, &format!("Produce to {}-{}", topic, partition));
            if !errors::is_retriable(p.error_code) {
                return Err(err);
            }
            last = Some(err);
            self.topics.remove(topic);
            std::thread::sleep(Duration::from_millis(20 * (attempt + 1)));
        }
        Err(last.unwrap_or_else(|| CamelError::new(KAFKA_EXCEPTION, "Produce failed")))
    }

    /// Send a fetch request to a broker.
    pub fn fetch(&mut self, node: i32, req: &FetchRequest) -> Result<FetchResponse> {
        let body = self.send(Node::Id(node), api::FETCH, &req.encode(), Duration::from_millis(req.max_wait_ms.max(0) as u64))?;
        FetchResponse::decode(&body)
    }

    /// The leader of every partition of a topic (from the cache).
    pub fn leaders(&mut self, topic: &str) -> Result<HashMap<i32, i32>> {
        Ok(self.partitions(topic, false)?.iter().map(|p| (p.partition, p.leader)).collect())
    }

    /// Forget cached metadata of a topic (after a leadership error).
    pub fn invalidate(&mut self, topic: &str) {
        self.topics.remove(topic);
    }

    /// Look up offsets by timestamp ([`EARLIEST_TIMESTAMP`] / [`LATEST_TIMESTAMP`]) for
    /// partitions of a topic.
    pub fn list_offsets(&mut self, topic: &str, partitions: &[i32], timestamp: i64) -> Result<HashMap<i32, i64>> {
        let leaders = self.leaders(topic)?;
        let mut by_leader: HashMap<i32, Vec<i32>> = HashMap::new();
        for p in partitions {
            let l = *leaders.get(p).ok_or_else(|| kafka_error(errors::UNKNOWN_TOPIC_OR_PARTITION, &format!("Partition {}-{}", topic, p)))?;
            by_leader.entry(l).or_default().push(*p);
        }
        let mut out = HashMap::new();
        for (leader, parts) in by_leader {
            let req = ListOffsetsRequest { replica_id: -1, topics: vec![(topic.to_string(), parts.iter().map(|p| (*p, timestamp)).collect())] };
            let resp = ListOffsetsResponse::decode(&self.send(Node::Id(leader), api::LIST_OFFSETS, &req.encode(), Duration::ZERO)?)?;
            for (_, ps) in resp.topics {
                for (p, err, _ts, off) in ps {
                    if err != errors::NONE {
                        self.invalidate(topic);
                        return Err(kafka_error(err, &format!("ListOffsets {}-{}", topic, p)));
                    }
                    out.insert(p, off);
                }
            }
        }
        Ok(out)
    }

    /// Create topics; existing topics are not an error.
    pub fn create_topics(&mut self, topics: &[(String, i32)]) -> Result<()> {
        let req = CreateTopicsRequest {
            topics: topics.iter().map(|(n, p)| NewTopic { name: n.clone(), num_partitions: *p, replication_factor: 1, configs: Vec::new() }).collect(),
            timeout_ms: 10_000,
            validate_only: false,
        };
        let resp = CreateTopicsResponse::decode(&self.send(Node::Any, api::CREATE_TOPICS, &req.encode(), Duration::from_secs(10))?)?;
        for (t, err, msg) in resp.topics {
            if err != errors::NONE && err != errors::TOPIC_ALREADY_EXISTS {
                return Err(CamelError::new(KAFKA_EXCEPTION, format!("Cannot create topic {}: {} {}", t, errors::name(err), msg.unwrap_or_default())));
            }
            self.topics.remove(&t);
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // groups
    // -----------------------------------------------------------------------

    /// Find the group coordinator; returns its node id.
    pub fn find_coordinator(&mut self, group: &str) -> Result<i32> {
        let req = FindCoordinatorRequest { key: group.to_string(), key_type: 0 };
        let resp = FindCoordinatorResponse::decode(&self.send(Node::Any, api::FIND_COORDINATOR, &req.encode(), Duration::ZERO)?)?;
        if resp.error_code != errors::NONE {
            return Err(kafka_error(resp.error_code, &format!("FindCoordinator for group {}", group)));
        }
        self.brokers.insert(resp.node_id, (resp.host, resp.port as u16));
        Ok(resp.node_id)
    }

    /// JoinGroup (blocks until the rebalance completes on the broker).
    pub fn join_group(&mut self, coordinator: i32, req: &JoinGroupRequest) -> Result<JoinResult> {
        let wait = Duration::from_millis(req.rebalance_timeout_ms.max(0) as u64);
        JoinGroupResponse::decode(&self.send(Node::Id(coordinator), api::JOIN_GROUP, &req.encode(), wait)?)
    }

    /// SyncGroup (followers block until the leader has sent the assignments).
    pub fn sync_group(&mut self, coordinator: i32, req: &SyncGroupRequest, wait: Duration) -> Result<SyncGroupResponse> {
        SyncGroupResponse::decode(&self.send(Node::Id(coordinator), api::SYNC_GROUP, &req.encode(), wait)?)
    }

    /// Heartbeat; returns the error code.
    pub fn heartbeat(&mut self, coordinator: i32, req: &HeartbeatRequest) -> Result<i16> {
        Ok(SimpleResponse::decode(&self.send(Node::Id(coordinator), api::HEARTBEAT, &req.encode(), Duration::ZERO)?)?.error_code)
    }

    /// LeaveGroup; returns the error code.
    pub fn leave_group(&mut self, coordinator: i32, req: &LeaveGroupRequest) -> Result<i16> {
        Ok(SimpleResponse::decode(&self.send(Node::Id(coordinator), api::LEAVE_GROUP, &req.encode(), Duration::ZERO)?)?.error_code)
    }

    /// Commit offsets; returns the first error code (0 when all succeeded).
    pub fn offset_commit(&mut self, coordinator: i32, req: &OffsetCommitRequest) -> Result<i16> {
        let resp = OffsetCommitResponse::decode(&self.send(Node::Id(coordinator), api::OFFSET_COMMIT, &req.encode(), Duration::ZERO)?)?;
        Ok(resp.topics.iter().flat_map(|(_, ps)| ps.iter().map(|(_, e)| *e)).find(|e| *e != errors::NONE).unwrap_or(errors::NONE))
    }

    /// Fetch committed offsets: (topic, partition) -> offset (-1 when none).
    pub fn offset_fetch(&mut self, coordinator: i32, group: &str, topics: &[(String, Vec<i32>)]) -> Result<HashMap<(String, i32), i64>> {
        let req = OffsetFetchRequest { group_id: group.to_string(), topics: topics.to_vec() };
        let resp = OffsetFetchResponse::decode(&self.send(Node::Id(coordinator), api::OFFSET_FETCH, &req.encode(), Duration::ZERO)?)?;
        let mut out = HashMap::new();
        for (t, ps) in resp.topics {
            for (p, off, _meta, err) in ps {
                if err != errors::NONE && err != errors::UNKNOWN_TOPIC_OR_PARTITION {
                    return Err(kafka_error(err, &format!("OffsetFetch {}-{}", t, p)));
                }
                out.insert((t.clone(), p), off);
            }
        }
        Ok(out)
    }
}

impl Drop for KafkaClient {
    fn drop(&mut self) {
        self.close();
    }
}

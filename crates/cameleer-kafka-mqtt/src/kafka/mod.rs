//! `kafka:` component (Camel `camel-kafka`) over a built-in Kafka wire protocol client.
//!
//! `kafka:topic?brokers=host:port[,host:port]&groupId=...&...`
//!
//! - **Producer**: sends the body (bytes as is, other values as UTF-8 text; an array body
//!   sends one record per element) to the topic. The record key comes from header
//!   `kafka.KEY` or option `key`, the partition from header `kafka.PARTITION_KEY` or option
//!   `partitionKey`; otherwise keyed records use Kafka's murmur2 partitioner and unkeyed
//!   records are spread round robin. Header `kafka.OVERRIDE_TOPIC` overrides (and is
//!   removed). Other headers become record headers (strings as UTF-8, integers as 8-byte
//!   longs, bytes as is), except `Camel*`, `org.apache.camel*` and `kafka.*`. After the
//!   send, `kafka.RECORD_META` holds an array of `{topic, partition, offset, timestamp}`
//!   objects and `kafka.PARTITION` / `kafka.OFFSET` the location of the (last) record.
//!   Each exchange waits for its records to be acknowledged (`requestRequiredAcks`). Records
//!   sent while another send is in flight are batched into one produce request per partition
//!   (as Kafka's own producer does), so concurrent senders don't wait in line one record at
//!   a time; order within a partition is the order of the sends.
//! - **Consumer**: `consumersCount` threads, each a member of consumer group `groupId`
//!   (range assignor, heartbeats, rebalancing, committed offsets). Without a `groupId`
//!   every thread reads all partitions without committing. Message headers:
//!   `kafka.TOPIC`, `kafka.PARTITION`, `kafka.OFFSET`, `kafka.KEY`, `kafka.TIMESTAMP`,
//!   `kafka.LAST_POLL_RECORD`, `kafka.LAST_RECORD_BEFORE_COMMIT`, plus record headers (as bytes).
//!   The body is a string (or bytes with a `ByteArrayDeserializer` value deserializer).
//!
//! Options: `brokers` (localhost:9092), `clientId`, `groupId`, `autoOffsetReset`
//! (earliest/latest/none, default latest), `consumersCount` (1), `maxPollRecords` (500),
//! `autoCommitEnable` (true), `autoCommitIntervalMs` (5000), `autoCommitOnStop`
//! (sync/async/none), `allowManualCommit` (false; see [`manual_commit`]), `key`,
//! `partitionKey`, `requestRequiredAcks` (all/-1, 1, 0), `seekTo` (beginning/end),
//! `pollTimeoutMs` (5000), `fetchWaitMaxMs` (500), `topicIsPattern`, `breakOnFirstError`,
//! `heartbeatIntervalMs` (3000), `sessionTimeoutMs` (45000), `maxPollIntervalMs` (300000),
//! `fetchMaxBytes`, `maxPartitionFetchBytes`, `requestTimeoutMs` (30000), `recordMetadata`
//! (true), `keyDeserializer`/`valueDeserializer`, plus accepted-and-ignored tuning options.
//!
//! **Security**: `securityProtocol` `PLAINTEXT` (default), `SSL`, `SASL_PLAINTEXT` or
//! `SASL_SSL` (custom one-step SASL mechanisms such as `AWS_MSK_IAM`, registered by
//! `cameleer-cloud`, plug in through [`client::register_sasl_mechanism`]). TLS trust comes from `sslContextParameters=#bean`,
//! `useGlobalSslContextParameters=true`, `sslTruststoreLocation` (a PEM CA file - Java
//! keystores are not supported), the global `camel.ssl.*` parameters, else the system
//! trust store; `sslEndpointIdentificationAlgorithm=` (empty) disables host name
//! verification, `sslProtocol` restricts the TLS version. SASL: `saslMechanism` `PLAIN`
//! (default), `SCRAM-SHA-256` or `SCRAM-SHA-512` with credentials from `saslJaasConfig`
//! (`... required username="u" password="p";`). GSSAPI/OAUTHBEARER and client
//! certificates (`sslKeystoreLocation`) are not supported.

pub mod client;
mod consumer;
pub mod protocol;

use std::any::Any;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use cameleer_core::context::{CamelContext, WeakContext};
use cameleer_core::error::{kinds, CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use cameleer_core::util::Parameters;
use cameleer_core::value::Value;

use crate::net::{now_millis, unique_id};
use cameleer_tls::camel_support::{endpoint_tls, EndpointTls};
use client::{ClientConfig, KafkaClient, SaslConfig};
use protocol::{partition_for_key, Record};

/// Kafka header names (Camel `KafkaConstants`).
pub mod headers {
    pub const PARTITION_KEY: &str = "kafka.PARTITION_KEY";
    pub const PARTITION: &str = "kafka.PARTITION";
    pub const KEY: &str = "kafka.KEY";
    pub const TOPIC: &str = "kafka.TOPIC";
    pub const OVERRIDE_TOPIC: &str = "kafka.OVERRIDE_TOPIC";
    pub const OFFSET: &str = "kafka.OFFSET";
    pub const HEADERS: &str = "kafka.HEADERS";
    pub const LAST_RECORD_BEFORE_COMMIT: &str = "kafka.LAST_RECORD_BEFORE_COMMIT";
    pub const LAST_POLL_RECORD: &str = "kafka.LAST_POLL_RECORD";
    pub const TIMESTAMP: &str = "kafka.TIMESTAMP";
    pub const OVERRIDE_TIMESTAMP: &str = "kafka.OVERRIDE_TIMESTAMP";
    pub const RECORD_META: &str = "kafka.RECORD_META";
    pub const MANUAL_COMMIT: &str = "CamelKafkaManualCommit";
}

/// What to do when there is no committed offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OffsetReset {
    Earliest,
    Latest,
    None,
}

/// Where to seek on (first) assignment, ignoring committed offsets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeekTo {
    Beginning,
    End,
}

/// When to commit on stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitOnStop {
    Sync,
    Async,
    None,
}

/// Endpoint configuration.
#[derive(Clone, Debug)]
pub struct KafkaConfig {
    pub topic: String,
    pub brokers: String,
    pub client_id: String,
    pub group_id: Option<String>,
    pub auto_offset_reset: OffsetReset,
    pub consumers_count: usize,
    pub max_poll_records: usize,
    pub auto_commit_enable: bool,
    pub auto_commit_interval: Duration,
    pub auto_commit_on_stop: CommitOnStop,
    pub allow_manual_commit: bool,
    pub key: Option<String>,
    pub partition_key: Option<i32>,
    pub request_required_acks: i16,
    pub seek_to: Option<SeekTo>,
    pub poll_timeout: Duration,
    pub fetch_wait_max: Duration,
    pub topic_is_pattern: bool,
    pub break_on_first_error: bool,
    pub heartbeat_interval: Duration,
    pub session_timeout: Duration,
    pub max_poll_interval: Duration,
    pub fetch_max_bytes: i32,
    pub max_partition_fetch_bytes: i32,
    pub request_timeout: Duration,
    pub record_metadata: bool,
    pub key_as_bytes: bool,
    pub value_as_bytes: bool,
    /// TLS for `SSL` / `SASL_SSL`.
    pub tls: Option<EndpointTls>,
    /// SASL for `SASL_PLAINTEXT` / `SASL_SSL`.
    pub sasl: Option<SaslConfig>,
    /// Producer record batch compression (`compressionCodec`).
    pub compression: protocol::Compression,
}

impl KafkaConfig {
    fn client_config(&self, suffix: &str) -> Result<ClientConfig> {
        let mut c = ClientConfig::new(&self.brokers, &format!("{}{}", self.client_id, suffix))?;
        c.request_timeout = self.request_timeout;
        c.tls = self.tls.clone();
        c.sasl = self.sasl.clone();
        c.compression = self.compression;
        Ok(c)
    }
}

/// Parses the security options (`securityProtocol`, `ssl*`, `sasl*`).
fn take_security(ctx: &CamelContext, params: &mut Parameters) -> Result<(Option<EndpointTls>, Option<SaslConfig>)> {
    let proto = params.take_string("securityProtocol", "PLAINTEXT").to_ascii_uppercase();
    let (use_tls, use_sasl) = match proto.as_str() {
        "PLAINTEXT" => (false, false),
        "SSL" => (true, false),
        "SASL_PLAINTEXT" => (false, true),
        "SASL_SSL" => (true, true),
        other => return Err(CamelError::illegal_argument(format!("Invalid securityProtocol: {} (PLAINTEXT, SSL, SASL_PLAINTEXT or SASL_SSL)", other))),
    };
    let mechanism = params.take_string("saslMechanism", "PLAIN").to_ascii_uppercase();
    let jaas = params.take("saslJaasConfig").filter(|s| !s.trim().is_empty());
    params.take("saslKerberosServiceName");
    // key / trust stores: PKCS12, JKS or PEM (sslKeystoreType / sslTruststoreType; detected when absent)
    let stores = cameleer_tls::camel_support::StoreOptions {
        key_store: params.take("sslKeystoreLocation").filter(|s| !s.trim().is_empty()),
        private_key: None,
        key_store_password: params.take("sslKeystorePassword"),
        key_password: params.take("sslKeyPassword"),
        key_store_type: params.take("sslKeystoreType"),
        trust_store: params.take("sslTruststoreLocation").filter(|s| !s.trim().is_empty()),
        trust_store_password: params.take("sslTruststorePassword"),
        trust_store_type: params.take("sslTruststoreType"),
    };
    let endpoint_alg = params.take("sslEndpointAlgorithm").or_else(|| params.take("sslEndpointIdentificationAlgorithm"));
    let ssl_protocol = params.take("sslProtocol").or_else(|| params.take("sslEnabledProtocols"));
    // other ssl*/kerberos/sasl tuning options are accepted and ignored
    let rest: Vec<String> = params.keys().filter(|k| k.starts_with("ssl") && *k != "sslContextParameters" || k.starts_with("kerberos") || k.starts_with("sasl")).cloned().collect();
    for k in rest {
        params.take(&k);
    }
    let tls = match endpoint_tls(ctx, params, "kafka", use_tls, &[])? {
        Some(t) if use_tls => {
            let mut p = (*t.params).clone();
            stores.apply(&mut p);
            if let Some(alg) = &endpoint_alg {
                p.hostname_verification = Some(!alg.trim().is_empty());
            }
            if let Some(v) = ssl_protocol.as_deref().and_then(|s| s.split(',').filter_map(cameleer_tls::params::parse_tls_version).max()) {
                p.min_version = Some(v);
                p.max_version = Some(v);
            }
            Some(EndpointTls::new(Arc::new(p)))
        }
        _ => None,
    };
    let sasl = if use_sasl {
        if let Some(factory) = client::sasl_mechanism(&mechanism) {
            return Ok((tls, Some(SaslConfig::custom(&mechanism, factory(jaas.as_deref().unwrap_or(""))?))));
        }
        if mechanism == "GSSAPI" || mechanism == "OAUTHBEARER" {
            return Err(CamelError::new(kinds::UNSUPPORTED_OPERATION, format!("Kafka saslMechanism {} is not supported by Cameleer (PLAIN, SCRAM-SHA-256, SCRAM-SHA-512)", mechanism)));
        }
        if mechanism != "PLAIN" && cameleer_tls::sasl::ScramMechanism::from_name(&mechanism).is_none() {
            return Err(CamelError::illegal_argument(format!("Unsupported saslMechanism: {} (PLAIN, SCRAM-SHA-256, SCRAM-SHA-512)", mechanism)));
        }
        let jaas = jaas.ok_or_else(|| CamelError::illegal_argument(format!("securityProtocol={} requires saslJaasConfig with username and password", proto)))?;
        Some(SaslConfig::from_jaas(&mechanism, &jaas)?)
    } else {
        None
    };
    Ok((tls, sasl))
}

/// The `kafka` component. Component-level `brokers` can be set with [`KafkaComponent::with_brokers`].
#[derive(Default)]
pub struct KafkaComponent {
    brokers: Option<String>,
}

impl KafkaComponent {
    pub fn new() -> Self {
        Self::default()
    }
    /// Default brokers for endpoints without a `brokers` option.
    pub fn with_brokers(brokers: &str) -> Self {
        KafkaComponent { brokers: Some(brokers.to_string()) }
    }
}

const IGNORED: &[&str] = &[
    "lazyStartProducer",
    "bridgeErrorHandler",
    "exceptionHandler",
    "exchangePattern",
    "synchronous",
    "keySerializer",
    "valueSerializer",
    "lingerMs",
    "retries",
    "retryBackoffMs",
    "retryBackoffMaxMs",
    "bufferMemorySize",
    "producerBatchSize",
    "batchSize",
    "maxRequestSize",
    "sendBufferBytes",
    "receiveBufferBytes",
    "metadataMaxAgeMs",
    "reconnectBackoffMs",
    "reconnectBackoffMaxMs",
    "connectionMaxIdleMs",
    "fetchMinBytes",
    "checkCrcs",
    "consumerRequestTimeoutMs",
    "isolationLevel",
    "groupInstanceId",
    "partitionAssignor",
    "headerFilterStrategy",
    "shutdownTimeout",
    "enableIdempotence",
    "deliveryTimeoutMs",
    "interceptorClasses",
    "maxBlockMs",
    "maxInFlightRequest",
    "partitioner",
    "partitionerIgnoreKeys",
    "pollOnError",
    "workerPool",
    "workerPoolCoreSize",
    "workerPoolMaxSize",
    "useIterator",
    "batching",
    "kafkaHeaderDeserializer",
    "kafkaHeaderSerializer",
    "additionalProperties",
    "specificAvroReader",
    "schemaRegistryURL",
    "commitTimeoutMs",
    "offsetRepository",
    "kafkaClientFactory",
    "kafkaManualCommitFactory",
    "metricReporters",
    "noOfMetricsSample",
    "metricsSampleWindowMs",
    "queueBufferingMaxMessages",
];

impl Component for KafkaComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let topic = crate::net::percent_decode(remaining.trim_matches('/'));
        let (tls, sasl) = take_security(ctx, params)?;
        let compression = protocol::Compression::from_name(&params.take_string("compressionCodec", "none"))?;
        if compression == protocol::Compression::Zstd {
            return Err(CamelError::new(kinds::UNSUPPORTED_OPERATION, "Kafka compressionCodec=zstd is not supported by Cameleer (none, gzip, snappy, lz4)"));
        }
        let auto_offset_reset = match params.take_string("autoOffsetReset", "latest").to_ascii_lowercase().as_str() {
            "earliest" => OffsetReset::Earliest,
            "latest" => OffsetReset::Latest,
            "none" => OffsetReset::None,
            other => return Err(CamelError::illegal_argument(format!("Invalid autoOffsetReset: {} (earliest, latest or none)", other))),
        };
        let seek_to = match params.take("seekTo").map(|s| s.to_ascii_lowercase()) {
            None => None,
            Some(s) if s.is_empty() => None,
            Some(s) if s == "beginning" => Some(SeekTo::Beginning),
            Some(s) if s == "end" => Some(SeekTo::End),
            Some(other) => return Err(CamelError::illegal_argument(format!("Invalid seekTo: {} (beginning or end)", other))),
        };
        let acks = match params.take_string("requestRequiredAcks", "all").to_ascii_lowercase().as_str() {
            "all" | "-1" => -1,
            "1" => 1,
            "0" => 0,
            other => return Err(CamelError::illegal_argument(format!("Invalid requestRequiredAcks: {} (all, -1, 0 or 1)", other))),
        };
        let auto_commit_on_stop = match params.take_string("autoCommitOnStop", "sync").to_ascii_lowercase().as_str() {
            "sync" => CommitOnStop::Sync,
            "async" => CommitOnStop::Async,
            "none" => CommitOnStop::None,
            other => return Err(CamelError::illegal_argument(format!("Invalid autoCommitOnStop: {}", other))),
        };
        let partition_key = match params.take("partitionKey") {
            None => None,
            Some(s) => Some(s.trim().parse::<i32>().map_err(|_| CamelError::illegal_argument(format!("Invalid partitionKey: {}", s)))?),
        };
        let ms = |params: &mut Parameters, k: &str, d: u64| -> Result<Duration> { Ok(Duration::from_millis(params.take_u64(k, d)?)) };
        let bytes_deser = |v: Option<String>| v.map(|s| s.contains("ByteArray") || s.contains("BytesDeserializer")).unwrap_or(false);
        let cfg = KafkaConfig {
            brokers: params.take("brokers").or_else(|| self.brokers.clone()).unwrap_or_else(|| "localhost:9092".to_string()),
            client_id: params.take("clientId").unwrap_or_else(|| format!("camel-kafka-{}", &unique_id()[..8])),
            group_id: params.take("groupId").filter(|g| !g.is_empty()),
            auto_offset_reset,
            consumers_count: params.take_usize("consumersCount", 1)?.max(1),
            max_poll_records: params.take_usize("maxPollRecords", 500)?.max(1),
            auto_commit_enable: params.take_bool("autoCommitEnable", true)?,
            auto_commit_interval: ms(params, "autoCommitIntervalMs", 5000)?,
            auto_commit_on_stop,
            allow_manual_commit: params.take_bool("allowManualCommit", false)?,
            key: params.take("key"),
            partition_key,
            request_required_acks: acks,
            seek_to,
            poll_timeout: ms(params, "pollTimeoutMs", 5000)?,
            fetch_wait_max: ms(params, "fetchWaitMaxMs", 500)?,
            topic_is_pattern: params.take_bool("topicIsPattern", false)?,
            break_on_first_error: params.take_bool("breakOnFirstError", false)?,
            heartbeat_interval: ms(params, "heartbeatIntervalMs", 3000)?,
            session_timeout: ms(params, "sessionTimeoutMs", 45000)?,
            max_poll_interval: ms(params, "maxPollIntervalMs", 300_000)?,
            fetch_max_bytes: params.take_i64("fetchMaxBytes", 52_428_800)?.clamp(1, i32::MAX as i64) as i32,
            max_partition_fetch_bytes: params.take_i64("maxPartitionFetchBytes", 1_048_576)?.clamp(1, i32::MAX as i64) as i32,
            request_timeout: ms(params, "requestTimeoutMs", 30000)?,
            record_metadata: params.take_bool("recordMetadata", true)?,
            key_as_bytes: bytes_deser(params.take("keyDeserializer")),
            value_as_bytes: bytes_deser(params.take("valueDeserializer")),
            tls,
            sasl,
            compression,
            topic,
        };
        for k in IGNORED {
            params.take(k);
        }
        Ok(Arc::new(KafkaEndpoint { uri: uri.to_string(), ctx: ctx.downgrade(), cfg }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A `kafka` endpoint.
pub struct KafkaEndpoint {
    uri: String,
    ctx: WeakContext,
    cfg: KafkaConfig,
}

impl KafkaEndpoint {
    pub fn config(&self) -> &KafkaConfig {
        &self.cfg
    }
}

impl Endpoint for KafkaEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(KafkaProducer {
            cfg: self.cfg.clone(),
            client: Mutex::new(None),
            queue: Mutex::new(Vec::new()),
            round_robin: AtomicUsize::new(0),
            requests: AtomicUsize::new(0),
        }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        if self.cfg.topic.is_empty() {
            return Err(CamelError::illegal_argument("A topic is required for a Kafka consumer"));
        }
        Ok(Arc::new(consumer::KafkaConsumer::new(self.cfg.clone(), self.ctx.clone(), processor, self.uri.clone())))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Whether an exchange header is kept out of Kafka record headers (Camel's
/// `KafkaHeaderFilterStrategy`): `Camel*`, `org.apache.camel*`, `kafka.*`.
pub fn is_filtered_header(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    l.starts_with("camel") || l.starts_with("org.apache.camel") || l.starts_with("kafka.")
}

/// Serialize a header value for a record header (Camel `DefaultKafkaHeaderSerializer`);
/// None for values that cannot be serialized.
pub fn serialize_header(v: &Value) -> Option<Vec<u8>> {
    match v {
        Value::String(s) => Some(s.as_bytes().to_vec()),
        Value::Bytes(b) => Some(b.clone()),
        Value::Int(i) => Some(i.to_be_bytes().to_vec()),
        Value::Float(f) => Some(f.to_be_bytes().to_vec()),
        Value::Bool(b) => Some(b.to_string().into_bytes()),
        _ => None,
    }
}

fn value_bytes(v: &Value) -> Option<Vec<u8>> {
    match v {
        Value::Null => None,
        other => Some(other.to_bytes()),
    }
}

/// Where a send's result is left for its exchange: `(partition, base offset, append time)`.
type SendSlot = Arc<Mutex<Option<Result<(i32, i64, i64)>>>>;

/// One exchange's records, waiting to be sent.
struct PendingSend {
    topic: String,
    key: Option<Vec<u8>>,
    explicit_partition: Option<i32>,
    records: Vec<Record>,
    slot: SendSlot,
}

struct KafkaProducer {
    cfg: KafkaConfig,
    client: Mutex<Option<KafkaClient>>,
    /// Sends queued while the client is busy; the next sender to get the client sends them all.
    queue: Mutex<Vec<PendingSend>>,
    round_robin: AtomicUsize,
    /// Produce requests sent (for tests).
    requests: AtomicUsize,
}

impl KafkaProducer {
    /// Send every queued record: one produce request per partition, records in queue order.
    /// Each send's slot gets its partition and offsets, or the error of its request.
    fn send_queued(&self, client: &mut Option<KafkaClient>, sends: Vec<PendingSend>) {
        // partition of each send, grouped by (topic, partition) in order of first appearance
        let mut groups: Vec<((String, i32), Vec<usize>)> = Vec::new();
        for (i, s) in sends.iter().enumerate() {
            let partition = (|| -> Result<i32> {
                if client.is_none() {
                    *client = Some(KafkaClient::new(self.cfg.client_config("")?));
                }
                let count = client.as_mut().unwrap().partitions(&s.topic, false)?.len();
                Ok(match (s.explicit_partition, &s.key) {
                    (Some(p), _) => p,
                    (None, Some(k)) => partition_for_key(k, count),
                    (None, None) => (self.round_robin.fetch_add(1, Ordering::Relaxed) % count.max(1)) as i32,
                })
            })();
            match partition {
                Ok(p) => match groups.iter_mut().find(|(k, _)| k.0 == s.topic && k.1 == p) {
                    Some((_, members)) => members.push(i),
                    None => groups.push(((s.topic.clone(), p), vec![i])),
                },
                Err(e) => {
                    if let Some(mut c) = client.take() {
                        c.close();
                    }
                    *s.slot.lock().unwrap() = Some(Err(e));
                }
            }
        }
        // one call per topic, covering all its partitions (one request per leader broker)
        let mut topics: Vec<String> = Vec::new();
        for ((t, _), _) in &groups {
            if !topics.contains(t) {
                topics.push(t.clone());
            }
        }
        for topic in topics {
            let members_of: Vec<(i32, &Vec<usize>)> = groups.iter().filter(|((t, _), _)| *t == topic).map(|((_, p), m)| (*p, m)).collect();
            let batches: Vec<(i32, Vec<Record>)> =
                members_of.iter().map(|(p, m)| (*p, m.iter().flat_map(|&i| sends[i].records.iter().cloned()).collect())).collect();
            let connected = match client {
                Some(_) => Ok(()),
                None => self.cfg.client_config("").map(|cfg| *client = Some(KafkaClient::new(cfg))),
            };
            let results: Vec<Result<(i64, i64)>> = match connected {
                Err(e) => batches.iter().map(|_| Err(e.clone())).collect(),
                Ok(()) => {
                    self.requests.fetch_add(1, Ordering::Relaxed);
                    client.as_mut().unwrap().produce_partitions(
                        &topic,
                        &batches,
                        self.cfg.request_required_acks,
                        self.cfg.request_timeout.as_millis() as i32,
                    )
                }
            };
            let mut failed = false;
            for ((partition, members), result) in members_of.iter().zip(results) {
                match result {
                    Ok((base, append_time)) => {
                        let mut next = base;
                        for &i in members.iter() {
                            *sends[i].slot.lock().unwrap() = Some(Ok((*partition, next, append_time)));
                            if next >= 0 {
                                next += sends[i].records.len() as i64;
                            }
                        }
                    }
                    Err(e) => {
                        failed = true;
                        for &i in members.iter() {
                            *sends[i].slot.lock().unwrap() = Some(Err(e.clone()));
                        }
                    }
                }
            }
            if failed {
                if let Some(mut c) = client.take() {
                    c.close();
                }
            }
        }
    }
}

impl Service for KafkaProducer {
    fn stop(&self) -> Result<()> {
        if let Some(mut c) = self.client.lock().unwrap().take() {
            c.close();
        }
        Ok(())
    }
}

impl Processor for KafkaProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let topic = match ex.message.remove_header(headers::OVERRIDE_TOPIC).filter(|v| !v.is_null()) {
            Some(t) => t.to_string(),
            None => self.cfg.topic.clone(),
        };
        if topic.is_empty() {
            return Err(CamelError::illegal_argument("No Kafka topic: set the endpoint topic or header kafka.OVERRIDE_TOPIC"));
        }
        let key: Option<Vec<u8>> = match ex.header(headers::KEY).filter(|v| !v.is_null()) {
            Some(v) => Some(v.to_bytes()),
            None => self.cfg.key.as_ref().map(|k| k.as_bytes().to_vec()),
        };
        let explicit_partition = match ex.header(headers::PARTITION_KEY).filter(|v| !v.is_null()) {
            Some(v) => Some(
                v.as_i64()
                    .or_else(|| v.to_string().trim().parse().ok())
                    .ok_or_else(|| CamelError::illegal_argument(format!("Invalid {} header: {}", headers::PARTITION_KEY, v)))? as i32,
            ),
            None => self.cfg.partition_key,
        };
        let timestamp = ex.header(headers::OVERRIDE_TIMESTAMP).and_then(|v| v.as_i64()).unwrap_or_else(now_millis);
        let record_headers: Vec<(String, Option<Vec<u8>>)> = ex
            .message
            .headers
            .iter()
            .filter(|(k, _)| !is_filtered_header(k))
            .filter_map(|(k, v)| serialize_header(v).map(|b| (k.to_string(), Some(b))))
            .collect();
        let values: Vec<Option<Vec<u8>>> = match ex.body() {
            Value::Array(items) => items.iter().map(value_bytes).collect(),
            other => vec![value_bytes(other)],
        };
        let records: Vec<Record> =
            values.into_iter().map(|value| Record { offset: 0, timestamp, key: key.clone(), value, headers: record_headers.clone() }).collect();
        if records.is_empty() {
            return Ok(());
        }

        // queue the records, then take the client: whoever holds it sends everything queued, so
        // a send that waited for the client usually finds its result already there
        let count = records.len();
        let slot: SendSlot = Arc::new(Mutex::new(None));
        self.queue.lock().unwrap().push(PendingSend { topic: topic.clone(), key, explicit_partition, records, slot: slot.clone() });
        let mut guard = self.client.lock().unwrap();
        if slot.lock().unwrap().is_none() {
            let sends = std::mem::take(&mut *self.queue.lock().unwrap());
            self.send_queued(&mut guard, sends);
        }
        drop(guard);
        let result = slot.lock().unwrap().take().unwrap_or_else(|| Err(CamelError::new(protocol::KAFKA_EXCEPTION, "Kafka send was not attempted")));
        let (partition, base, append_time) = result?;
        let ts = if append_time >= 0 { append_time } else { timestamp };
        if self.cfg.record_metadata {
            let meta: Vec<Value> = (0..count)
                .map(|i| {
                    Value::object([
                        ("topic", Value::String(topic.clone())),
                        ("partition", Value::Int(partition as i64)),
                        ("offset", Value::Int(if base >= 0 { base + i as i64 } else { -1 })),
                        ("timestamp", Value::Int(ts)),
                    ])
                })
                .collect();
            ex.set_header(headers::RECORD_META, Value::Array(meta));
        }
        ex.set_header(headers::PARTITION, partition as i64);
        ex.set_header(headers::OFFSET, if base >= 0 { base + count as i64 - 1 } else { -1 });
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// manual commit
// ---------------------------------------------------------------------------

type PendingCommits = Arc<Mutex<HashMap<(String, i32), i64>>>;

fn manual_registry() -> &'static Mutex<HashMap<String, PendingCommits>> {
    static R: OnceLock<Mutex<HashMap<String, PendingCommits>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Commit the offset of the record carried by an exchange consumed with
/// `allowManualCommit=true` (Camel `KafkaManualCommit.commit()`). The commit is sent by
/// the consumer right after the exchange has been processed.
pub fn manual_commit(ex: &Exchange) -> Result<()> {
    let v = ex
        .header(headers::MANUAL_COMMIT)
        .ok_or_else(|| CamelError::illegal_state(format!("No {} header: enable allowManualCommit on the consumer", headers::MANUAL_COMMIT)))?;
    let consumer = v.get("consumerId").map(|v| v.to_string()).unwrap_or_default();
    let topic = v.get("topic").map(|v| v.to_string()).unwrap_or_default();
    let partition = v.get("partition").and_then(|v| v.as_i64()).unwrap_or(-1) as i32;
    let offset = v.get("offset").and_then(|v| v.as_i64()).unwrap_or(-1);
    let reg = manual_registry().lock().unwrap();
    let pending = reg.get(&consumer).ok_or_else(|| CamelError::illegal_state("The Kafka consumer of this exchange is no longer running"))?;
    pending.lock().unwrap().insert((topic, partition), offset + 1);
    Ok(())
}

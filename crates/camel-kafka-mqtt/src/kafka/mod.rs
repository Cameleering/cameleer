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
//! `securityProtocol` other than `PLAINTEXT`, `saslMechanism` and `saslJaasConfig` fail
//! with `UnsupportedOperationException` (no TLS/SASL in a std-only build).

pub mod client;
mod consumer;
pub mod protocol;

use std::any::Any;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{kinds, CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::value::Value;

use crate::net::{now_millis, unique_id};
use client::{ClientConfig, KafkaClient};
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
}

impl KafkaConfig {
    fn client_config(&self, suffix: &str) -> Result<ClientConfig> {
        let mut c = ClientConfig::new(&self.brokers, &format!("{}{}", self.client_id, suffix))?;
        c.request_timeout = self.request_timeout;
        Ok(c)
    }
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
    "compressionCodec",
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

const UNSUPPORTED_SECURITY: &str = "no TLS/SASL in a std-only build; use securityProtocol=PLAINTEXT";

impl Component for KafkaComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let topic = crate::net::percent_decode(remaining.trim_matches('/'));
        let proto = params.take_string("securityProtocol", "PLAINTEXT");
        if !proto.eq_ignore_ascii_case("PLAINTEXT") {
            return Err(CamelError::new(kinds::UNSUPPORTED_OPERATION, format!("Kafka securityProtocol {} is not supported by rust-camel ({}): {}", proto, UNSUPPORTED_SECURITY, uri)));
        }
        for k in ["saslMechanism", "saslJaasConfig", "saslKerberosServiceName"] {
            if let Some(v) = params.take(k) {
                if !v.is_empty() && !(k == "saslMechanism" && v.eq_ignore_ascii_case("GSSAPI")) {
                    return Err(CamelError::new(kinds::UNSUPPORTED_OPERATION, format!("Kafka option {} is not supported by rust-camel ({})", k, UNSUPPORTED_SECURITY)));
                }
            }
        }
        // ssl*/kerberos options only apply to SSL/SASL protocols: accepted and ignored
        let ssl: Vec<String> = params.keys().filter(|k| k.starts_with("ssl") || k.starts_with("kerberos") || k.starts_with("sasl")).cloned().collect();
        for k in ssl {
            params.take(&k);
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
        Ok(Arc::new(KafkaProducer { cfg: self.cfg.clone(), client: Mutex::new(None), round_robin: AtomicUsize::new(0) }))
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

struct KafkaProducer {
    cfg: KafkaConfig,
    client: Mutex<Option<KafkaClient>>,
    round_robin: AtomicUsize,
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

        let mut guard = self.client.lock().unwrap();
        if guard.is_none() {
            *guard = Some(KafkaClient::new(self.cfg.client_config("")?));
        }
        let client = guard.as_mut().unwrap();
        let result = (|| -> Result<(i32, i64, i64)> {
            let count = client.partitions(&topic, false)?.len();
            let partition = match (explicit_partition, &key) {
                (Some(p), _) => p,
                (None, Some(k)) => partition_for_key(k, count),
                (None, None) => (self.round_robin.fetch_add(1, Ordering::Relaxed) % count.max(1)) as i32,
            };
            let (base, append_time) = client.produce(&topic, partition, &records, self.cfg.request_required_acks, self.cfg.request_timeout.as_millis() as i32)?;
            Ok((partition, base, append_time))
        })();
        let (partition, base, append_time) = match result {
            Ok(r) => r,
            Err(e) => {
                if let Some(mut c) = guard.take() {
                    c.close();
                }
                return Err(e);
            }
        };
        drop(guard);
        let ts = if append_time >= 0 { append_time } else { timestamp };
        if self.cfg.record_metadata {
            let meta: Vec<Value> = (0..records.len())
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
        ex.set_header(headers::OFFSET, if base >= 0 { base + records.len() as i64 - 1 } else { -1 });
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

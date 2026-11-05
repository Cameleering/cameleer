//! The `google-pubsub-lite` component (Camel `camel-google-pubsub-lite`) over the Pub/Sub Lite
//! gRPC API (cameleer-grpc HTTP/2; `google.cloud.pubsublite.v1`).
//!
//! `google-pubsub-lite:projectId:location:destinationName?serviceAccountKey=...`
//!
//! * Producer (destination = topic): the partition count comes from
//!   `AdminService/GetTopicPartitions` (cached); a message with an ordering key
//!   (`CamelGooglePubsubLiteOrderingKey`) goes to `hash(key) % partitions`, others round robin.
//!   Each exchange opens a `PublisherService/Publish` stream (initial request with the topic and
//!   partition, then one `MessagePublishRequest`) and waits for the cursor of the published
//!   message(s): `CamelGooglePubsubLiteMessageId` = `partition:offset` (a list body publishes one
//!   message per element). Attributes from `CamelGooglePubsubLiteAttributes` (map), event time
//!   from `CamelGooglePubsubLiteEventTime` (epoch millis).
//!   Cameleer admin operations (`operation` / `CamelGooglePubsubLiteOperation`):
//!   `createTopic` (`partitionCount`, default 1; `perPartitionBytes` storage),
//!   `createSubscription` (`topic`), `deleteTopic`, `deleteSubscription`, `getTopicPartitions`.
//! * Consumer (destination = subscription): every poll, for each partition of the subscription's
//!   topic, a `SubscriberService/Subscribe` stream starts at the committed cursor (seek
//!   `COMMITTED_CURSOR`), grants flow control (`maxMessagesPerPoll` messages per partition,
//!   `perPartitionBytes` bytes) and reads until the stream idles (`streamIdleTimeout` ms,
//!   default 300). Each message becomes an exchange (body = data bytes, headers
//!   `CamelGooglePubsubLiteMessageId`, `...Attributes` (string values), `...OrderingKey`,
//!   `...PublishTime`, `...Partition`, `...Offset`); with `ackMode=AUTO` (default) the cursor
//!   is committed (`CursorService/StreamingCommitCursor`, offset + 1) after each successfully
//!   processed message; a failure stops the partition for this poll, so the message is
//!   redelivered on the next poll. `ackMode=NONE` never commits.
//!
//! Target: `pubsubEndpoint` / `endpoint` (`host:port`, plaintext for emulators) or
//! `{region}-pubsublite.googleapis.com:443` over TLS (the region of a zone `us-central1-a` is
//! `us-central1`). Authentication: the bearer token of the common Google credentials.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use cameleer_core::component::scheduled::ScheduledPollConfig;
use cameleer_core::context::CamelContext;
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::spi::Processor;
use cameleer_core::value::{ObjectMap, Value};
use cameleer_grpc::grpc::client::GrpcClient;
use cameleer_grpc::grpc::{Code, Metadata};
use cameleer_grpc::proto::DescriptorPool;
use cameleer_tls::TlsClientConfig;

use super::{GcpConfig, CLOUD_PLATFORM_SCOPE};
use crate::endpoint::{operation, process, set_batch, unsupported, CloudService};
use crate::util::{iso8601_millis, kinds, obj, Opts};

/// Header names (Camel `GooglePubsubLiteConstants`).
pub mod headers {
    pub const OPERATION: &str = "CamelGooglePubsubLiteOperation";
    pub const MESSAGE_ID: &str = "CamelGooglePubsubLiteMessageId";
    pub const ATTRIBUTES: &str = "CamelGooglePubsubLiteAttributes";
    pub const ORDERING_KEY: &str = "CamelGooglePubsubLiteOrderingKey";
    pub const PUBLISH_TIME: &str = "CamelGooglePubsubLitePublishTime";
    pub const EVENT_TIME: &str = "CamelGooglePubsubLiteEventTime";
    pub const PARTITION: &str = "CamelGooglePubsubLitePartition";
    pub const OFFSET: &str = "CamelGooglePubsubLiteOffset";
}

/// The subset of the Pub/Sub Lite protos used here.
pub const PUBSUB_LITE_PROTO: &str = r#"
syntax = "proto3";
package google.cloud.pubsublite.v1;
import "google/protobuf/timestamp.proto";

message AttributeValues { repeated bytes values = 1; }
message PubSubMessage {
  bytes key = 1;
  bytes data = 2;
  map<string, AttributeValues> attributes = 3;
  google.protobuf.Timestamp event_time = 4;
}
message Cursor { int64 offset = 1; }
message SequencedMessage {
  Cursor cursor = 1;
  google.protobuf.Timestamp publish_time = 2;
  PubSubMessage message = 3;
  int64 size_bytes = 4;
}

message InitialPublishRequest { string topic = 1; int64 partition = 2; string client_id = 3; }
message InitialPublishResponse {}
message MessagePublishRequest { repeated PubSubMessage messages = 1; int64 first_sequence_number = 2; }
message CursorRange { Cursor start_cursor = 1; int32 start_index = 2; int32 end_index = 3; }
message MessagePublishResponse { repeated CursorRange cursor_ranges = 1; }
message PublishRequest {
  oneof request_type {
    InitialPublishRequest initial_request = 1;
    MessagePublishRequest message_publish_request = 2;
  }
}
message PublishResponse {
  oneof response_type {
    InitialPublishResponse initial_response = 1;
    MessagePublishResponse message_response = 2;
  }
}

message SeekRequest {
  enum NamedTarget { NAMED_TARGET_UNSPECIFIED = 0; HEAD = 1; COMMITTED_CURSOR = 2; }
  oneof target { NamedTarget named_target = 1; Cursor cursor = 2; }
}
message SeekResponse { Cursor cursor = 1; }
message FlowControlRequest { int64 allowed_messages = 1; int64 allowed_bytes = 2; }
message InitialSubscribeRequest { string subscription = 1; int64 partition = 2; SeekRequest initial_location = 4; }
message InitialSubscribeResponse { Cursor cursor = 1; }
message MessageResponse { repeated SequencedMessage messages = 1; }
message SubscribeRequest {
  oneof request {
    InitialSubscribeRequest initial = 1;
    SeekRequest seek = 2;
    FlowControlRequest flow_control = 3;
  }
}
message SubscribeResponse {
  oneof response {
    InitialSubscribeResponse initial = 1;
    SeekResponse seek = 2;
    MessageResponse messages = 3;
  }
}

message InitialCommitCursorRequest { string subscription = 1; int64 partition = 2; }
message InitialCommitCursorResponse {}
message SequencedCommitCursorRequest { Cursor cursor = 1; }
message SequencedCommitCursorResponse { int64 acknowledged_commits = 1; }
message StreamingCommitCursorRequest {
  oneof request {
    InitialCommitCursorRequest initial = 1;
    SequencedCommitCursorRequest commit = 2;
  }
}
message StreamingCommitCursorResponse {
  oneof request {
    InitialCommitCursorResponse initial = 1;
    SequencedCommitCursorResponse commit = 2;
  }
}

message TopicPartitions { int64 partition_count = 1; }
message GetTopicPartitionsRequest { string name = 1; }
message Topic {
  message PartitionConfig { int64 count = 1; int32 scale = 2; }
  message RetentionConfig { int64 per_partition_bytes = 1; }
  string name = 1;
  PartitionConfig partition_config = 2;
  RetentionConfig retention_config = 3;
}
message Subscription { string name = 1; string topic = 2; }
message CreateTopicRequest { string parent = 1; Topic topic = 2; string topic_id = 3; }
message CreateSubscriptionRequest { string parent = 1; Subscription subscription = 2; string subscription_id = 3; }
message GetSubscriptionRequest { string name = 1; }
message DeleteTopicRequest { string name = 1; }
message DeleteSubscriptionRequest { string name = 1; }
message Empty {}

service PublisherService { rpc Publish(stream PublishRequest) returns (stream PublishResponse); }
service SubscriberService { rpc Subscribe(stream SubscribeRequest) returns (stream SubscribeResponse); }
service CursorService { rpc StreamingCommitCursor(stream StreamingCommitCursorRequest) returns (stream StreamingCommitCursorResponse); }
service AdminService {
  rpc CreateTopic(CreateTopicRequest) returns (Topic);
  rpc GetTopicPartitions(GetTopicPartitionsRequest) returns (TopicPartitions);
  rpc DeleteTopic(DeleteTopicRequest) returns (Empty);
  rpc CreateSubscription(CreateSubscriptionRequest) returns (Subscription);
  rpc GetSubscription(GetSubscriptionRequest) returns (Subscription);
  rpc DeleteSubscription(DeleteSubscriptionRequest) returns (Empty);
}
"#;

/// The descriptor pool of [`PUBSUB_LITE_PROTO`].
pub fn pool() -> &'static DescriptorPool {
    static POOL: OnceLock<DescriptorPool> = OnceLock::new();
    POOL.get_or_init(|| DescriptorPool::from_source("google/cloud/pubsublite/v1/pubsublite.proto", PUBSUB_LITE_PROTO).expect("embedded pubsublite.proto parses"))
}

const PKG: &str = "google.cloud.pubsublite.v1";

fn msg(name: &str) -> String {
    format!("{}.{}", PKG, name)
}

fn status_err(st: cameleer_grpc::Status) -> CamelError {
    crate::util::cloud_error(kinds::GOOGLE_API, format!("Pub/Sub Lite call failed: {} ({})", st.message, st.code.name()))
}

/// The Pub/Sub Lite endpoint.
pub struct PubsubLiteService {
    pub gcp: GcpConfig,
    pub project: String,
    pub location: String,
    pub destination: String,
    pub operation: Option<String>,
    pub target: String,
    pub tls: Option<String>,
    pub ack_auto: bool,
    pub max_messages_per_poll: i64,
    pub per_partition_bytes: i64,
    pub partition_count: i64,
    pub topic: Option<String>,
    pub idle: Duration,
    pub scheduler: ScheduledPollConfig,
    partitions: Mutex<Option<i64>>,
    rr: AtomicUsize,
    client: OnceLock<GrpcClient>,
}

/// Factory for the component.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let parts: Vec<&str> = remaining.trim_start_matches("//").split(':').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
        return Err(CamelError::illegal_argument("Expected google-pubsub-lite:projectId:location:destinationName"));
    }
    let explicit = o.get("pubsubEndpoint");
    let gcp = GcpConfig::from_opts(o)?;
    let location = parts[1].to_string();
    let region = match location.rsplit_once('-') {
        Some((r, z)) if z.len() == 1 && z.chars().all(|c| c.is_ascii_lowercase()) => r.to_string(),
        _ => location.clone(),
    };
    let (target, tls) = match explicit.or_else(|| gcp.endpoint.clone()) {
        Some(e) if e.starts_with("https://") => {
            let hp = e.trim_start_matches("https://").trim_end_matches('/').to_string();
            let host = hp.split(':').next().unwrap_or(&hp).to_string();
            (if hp.contains(':') { hp } else { format!("{}:443", hp) }, Some(host))
        }
        Some(e) => (e.trim_start_matches("http://").trim_end_matches('/').to_string(), None),
        None => (format!("{}-pubsublite.googleapis.com:443", region), Some(format!("{}-pubsublite.googleapis.com", region))),
    };
    let ack = o.string("ackMode", "AUTO").to_ascii_uppercase();
    if ack != "AUTO" && ack != "NONE" {
        return Err(CamelError::illegal_argument(format!("Invalid ackMode {} (AUTO or NONE)", ack)));
    }
    o.ignore(&["concurrentConsumers", "loggerId", "serializer", "maxAckExtensionPeriod", "publisherCacheSize", "publisherCacheTimeout", "publisherTerminationTimeout"]);
    Ok(Arc::new(PubsubLiteService {
        project: parts[0].to_string(),
        location,
        destination: parts[2].to_string(),
        operation: o.get("operation"),
        target,
        tls,
        ack_auto: ack == "AUTO",
        max_messages_per_poll: o.i64("maxMessagesPerPoll", 100)?.max(1),
        per_partition_bytes: o.i64("perPartitionBytes", 10 * 1024 * 1024)?.max(1),
        partition_count: o.i64("partitionCount", 1)?.max(1),
        topic: o.get("topic"),
        idle: Duration::from_millis(o.i64("streamIdleTimeout", 300)?.max(20) as u64),
        scheduler: ScheduledPollConfig::from_params(o.params, Duration::from_millis(1000), Duration::from_millis(500))?,
        partitions: Mutex::new(None),
        rr: AtomicUsize::new(0),
        client: OnceLock::new(),
        gcp,
    }))
}

impl PubsubLiteService {
    fn client(&self) -> &GrpcClient {
        self.client.get_or_init(|| match &self.tls {
            None => GrpcClient::plaintext(&self.target),
            Some(name) => GrpcClient::tls(&self.target, name, TlsClientConfig::builder().alpn_protocols(&["h2"]).build()),
        })
    }

    fn metadata(&self, params: &str) -> Result<Metadata> {
        let mut md: Metadata = vec![("x-goog-request-params".into(), params.to_string())];
        if let Some(t) = self.gcp.bearer(CLOUD_PLATFORM_SCOPE)? {
            md.push(("authorization".into(), format!("Bearer {}", t)));
        }
        Ok(md)
    }

    fn parent(&self) -> String {
        format!("projects/{}/locations/{}", self.project, self.location)
    }

    fn topic_path(&self, t: &str) -> String {
        format!("{}/topics/{}", self.parent(), t)
    }

    fn sub_path(&self, s: &str) -> String {
        format!("{}/subscriptions/{}", self.parent(), s)
    }

    fn unary(&self, method: &str, req_type: &str, resp_type: &str, req: &Value) -> Result<Value> {
        let bytes = pool().encode(&msg(req_type), req)?;
        let md = self.metadata(&format!("parent={}", self.parent()))?;
        let (resp, _) = self.client().unary(&format!("/{}.AdminService/{}", PKG, method), &bytes, &md, Some(Duration::from_secs(30))).map_err(status_err)?;
        pool().decode(&msg(resp_type), &resp)
    }

    fn partition_count_of(&self, topic: &str) -> Result<i64> {
        if let Some(n) = *self.partitions.lock().unwrap() {
            return Ok(n);
        }
        let r = self.unary("GetTopicPartitions", "GetTopicPartitionsRequest", "TopicPartitions", &obj([("name", Value::from(self.topic_path(topic)))]))?;
        let n = r.get("partition_count").and_then(|c| c.as_i64()).unwrap_or(1).max(1);
        *self.partitions.lock().unwrap() = Some(n);
        Ok(n)
    }

    fn to_message(&self, ex: &Exchange, data: Vec<u8>) -> Value {
        let mut m = ObjectMap::new();
        if let Some(k) = crate::util::header_str(ex, headers::ORDERING_KEY) {
            m.insert("key".into(), Value::Bytes(k.into_bytes()));
        }
        m.insert("data".into(), Value::Bytes(data));
        if let Some(Value::Object(attrs)) = ex.header(headers::ATTRIBUTES) {
            let a: ObjectMap = attrs.iter().map(|(k, v)| (k.clone(), obj([("values", Value::Array(vec![Value::Bytes(v.to_string().into_bytes())]))]))).collect();
            m.insert("attributes".into(), Value::Object(a));
        }
        if let Some(t) = ex.header(headers::EVENT_TIME).and_then(|t| t.as_i64()) {
            m.insert("event_time".into(), obj([("seconds", Value::Int(t / 1000)), ("nanos", Value::Int((t % 1000) * 1_000_000))]));
        }
        Value::Object(m)
    }

    fn publish(&self, ex: &mut Exchange) -> Result<()> {
        let n = self.partition_count_of(&self.destination)?;
        let partition = match crate::util::header_str(ex, headers::ORDERING_KEY) {
            Some(k) => (crate::util::md5(k.as_bytes()).iter().take(8).fold(0u64, |a, b| (a << 8) | *b as u64) % n as u64) as i64,
            None => (self.rr.fetch_add(1, Ordering::SeqCst) as i64) % n,
        };
        let messages: Vec<Value> = match ex.body() {
            Value::Array(items) => items.iter().map(|i| self.to_message(ex, i.to_bytes())).collect(),
            other => vec![self.to_message(ex, other.to_bytes())],
        };
        let count = messages.len();
        let topic = self.topic_path(&self.destination);
        let md = self.metadata(&format!("partition={}&topic={}", partition, topic))?;
        let mut call = self.client().start_call(&format!("/{}.PublisherService/Publish", PKG), &md, Some(Duration::from_secs(30))).map_err(status_err)?;
        let enc = |v: Value| pool().encode(&msg("PublishRequest"), &v);
        call.send(&enc(obj([("initial_request", obj([("topic", Value::from(topic.clone())), ("partition", Value::Int(partition)), ("client_id", Value::from(crate::util::random_hex(8)))]))]))?).map_err(status_err)?;
        let first = call.recv().map_err(status_err)?.ok_or_else(|| crate::util::cloud_error(kinds::GOOGLE_API, "Publish stream closed before the initial response"))?;
        let _ = pool().decode(&msg("PublishResponse"), &first)?;
        call.send(&enc(obj([("message_publish_request", obj([("messages", Value::Array(messages)), ("first_sequence_number", Value::Int(0))]))]))?).map_err(status_err)?;
        let resp = call.recv().map_err(status_err)?.ok_or_else(|| crate::util::cloud_error(kinds::GOOGLE_API, "Publish stream closed before the publish response"))?;
        let r = pool().decode(&msg("PublishResponse"), &resp)?;
        let _ = call.close_send();
        call.cancel();
        let ranges = r.get("message_response").and_then(|m| m.get("cursor_ranges")).and_then(|c| c.as_array()).cloned().unwrap_or_default();
        let mut ids = Vec::new();
        for cr in ranges {
            let start = cr.get("start_cursor").and_then(|c| c.get("offset")).and_then(|o| o.as_i64()).unwrap_or(0);
            let (a, b) = (cr.get("start_index").and_then(|x| x.as_i64()).unwrap_or(0), cr.get("end_index").and_then(|x| x.as_i64()).unwrap_or(0));
            for i in a..b {
                ids.push(format!("{}:{}", partition, start + (i - a)));
            }
        }
        if ids.len() != count {
            return Err(crate::util::cloud_error(kinds::GOOGLE_API, format!("Publish acknowledged {} of {} messages", ids.len(), count)));
        }
        ex.set_header(headers::PARTITION, partition);
        ex.set_header(headers::MESSAGE_ID, if count == 1 { Value::from(ids[0].clone()) } else { Value::from(ids) });
        Ok(())
    }

    fn commit(&self, sub: &str, partition: i64, offset: i64) -> Result<()> {
        let md = self.metadata(&format!("partition={}&subscription={}", partition, sub))?;
        let mut call = self.client().start_call(&format!("/{}.CursorService/StreamingCommitCursor", PKG), &md, Some(Duration::from_secs(30))).map_err(status_err)?;
        let enc = |v: Value| pool().encode(&msg("StreamingCommitCursorRequest"), &v);
        call.send(&enc(obj([("initial", obj([("subscription", Value::from(sub)), ("partition", Value::Int(partition))]))]))?).map_err(status_err)?;
        call.recv().map_err(status_err)?;
        call.send(&enc(obj([("commit", obj([("cursor", obj([("offset", Value::Int(offset))]))]))]))?).map_err(status_err)?;
        let r = call.recv().map_err(status_err)?.map(|b| pool().decode(&msg("StreamingCommitCursorResponse"), &b)).transpose()?;
        let _ = call.close_send();
        call.cancel();
        let acked = r.and_then(|r| r.get("commit").and_then(|c| c.get("acknowledged_commits")).and_then(|a| a.as_i64())).unwrap_or(0);
        if acked < 1 {
            return Err(crate::util::cloud_error(kinds::GOOGLE_API, format!("Cursor commit of partition {} not acknowledged", partition)));
        }
        Ok(())
    }
}

impl CloudService for PubsubLiteService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "publish");
        match op.as_str() {
            "publish" => self.publish(ex),
            "createTopic" => {
                let topic = obj([("name", Value::from(self.topic_path(&self.destination))), ("partition_config", obj([("count", Value::Int(self.partition_count)), ("scale", Value::Int(1))])), ("retention_config", obj([("per_partition_bytes", Value::Int(self.per_partition_bytes.max(30 * 1024 * 1024 * 1024)))]))]);
                let r = self.unary("CreateTopic", "CreateTopicRequest", "Topic", &obj([("parent", Value::from(self.parent())), ("topic", topic), ("topic_id", Value::from(self.destination.clone()))]))?;
                ex.set_body(r);
                Ok(())
            }
            "createSubscription" => {
                let topic = crate::util::header_str(ex, "CamelGooglePubsubLiteTopic").or_else(|| self.topic.clone()).ok_or_else(|| CamelError::illegal_argument("createSubscription requires the topic option"))?;
                let sub = obj([("name", Value::from(self.sub_path(&self.destination))), ("topic", Value::from(self.topic_path(&topic)))]);
                let r = self.unary("CreateSubscription", "CreateSubscriptionRequest", "Subscription", &obj([("parent", Value::from(self.parent())), ("subscription", sub), ("subscription_id", Value::from(self.destination.clone()))]))?;
                ex.set_body(r);
                Ok(())
            }
            "deleteTopic" => {
                self.unary("DeleteTopic", "DeleteTopicRequest", "Empty", &obj([("name", Value::from(self.topic_path(&self.destination)))]))?;
                Ok(())
            }
            "deleteSubscription" => {
                self.unary("DeleteSubscription", "DeleteSubscriptionRequest", "Empty", &obj([("name", Value::from(self.sub_path(&self.destination)))]))?;
                Ok(())
            }
            "getTopicPartitions" => {
                *self.partitions.lock().unwrap() = None;
                let n = self.partition_count_of(&self.destination)?;
                ex.set_body(n);
                Ok(())
            }
            other => Err(unsupported("google-pubsub-lite", other)),
        }
    }

    fn scheduler(&self) -> Option<&ScheduledPollConfig> {
        Some(&self.scheduler)
    }

    fn poll(&self, ctx: Option<CamelContext>, processor: &dyn Processor, logger: &str) -> Result<usize> {
        let sub = self.sub_path(&self.destination);
        let s = self.unary("GetSubscription", "GetSubscriptionRequest", "Subscription", &obj([("name", Value::from(sub.clone()))]))?;
        let topic = s.get("topic").map(|t| t.to_string()).unwrap_or_default();
        let topic_id = topic.rsplit('/').next().unwrap_or("").to_string();
        let n = self.partition_count_of(&topic_id)?;
        let mut delivered = 0;
        for partition in 0..n {
            let md = self.metadata(&format!("partition={}&subscription={}", partition, sub))?;
            let mut call = match self.client().start_call(&format!("/{}.SubscriberService/Subscribe", PKG), &md, Some(self.idle)) {
                Ok(c) => c,
                Err(st) => return Err(status_err(st)),
            };
            let enc = |v: Value| pool().encode(&msg("SubscribeRequest"), &v);
            call.send(&enc(obj([("initial", obj([("subscription", Value::from(sub.clone())), ("partition", Value::Int(partition)), ("initial_location", obj([("named_target", Value::from("COMMITTED_CURSOR"))]))]))]))?).map_err(status_err)?;
            call.send(&enc(obj([("flow_control", obj([("allowed_messages", Value::Int(self.max_messages_per_poll)), ("allowed_bytes", Value::Int(self.per_partition_bytes))]))]))?).map_err(status_err)?;
            let mut batch: Vec<Value> = Vec::new();
            loop {
                match call.recv() {
                    Ok(Some(b)) => {
                        let r = pool().decode(&msg("SubscribeResponse"), &b)?;
                        if let Some(m) = r.get("messages").and_then(|m| m.get("messages")).and_then(|m| m.as_array()) {
                            batch.extend(m.iter().cloned());
                        }
                        if batch.len() as i64 >= self.max_messages_per_poll {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(st) if st.code == Code::DeadlineExceeded || st.code == Code::Cancelled => break,
                    Err(st) => return Err(status_err(st)),
                }
            }
            call.cancel();
            let total = batch.len();
            for (i, sm) in batch.iter().enumerate() {
                let offset = sm.get("cursor").and_then(|c| c.get("offset")).and_then(|o| o.as_i64()).unwrap_or(0);
                let m = sm.get("message").cloned().unwrap_or_default();
                let mut ex = Exchange::new(ctx.clone());
                ex.set_header(headers::MESSAGE_ID, format!("{}:{}", partition, offset));
                ex.set_header(headers::PARTITION, partition);
                ex.set_header(headers::OFFSET, offset);
                if let Some(k) = m.get("key").map(|k| String::from_utf8_lossy(&k.to_bytes()).to_string()).filter(|k| !k.is_empty()) {
                    ex.set_header(headers::ORDERING_KEY, k);
                }
                let attrs: ObjectMap = m.get("attributes").and_then(|a| a.as_object().cloned()).unwrap_or_default().into_iter().map(|(k, v)| (k, Value::from(v.get("values").and_then(|x| x.as_array()).and_then(|a| a.first()).map(|b| String::from_utf8_lossy(&b.to_bytes()).to_string()).unwrap_or_default()))).collect();
                ex.set_header(headers::ATTRIBUTES, Value::Object(attrs));
                if let Some(t) = sm.get("publish_time") {
                    let ms = t.get("seconds").and_then(|s| s.as_i64()).unwrap_or(0) * 1000 + t.get("nanos").and_then(|n| n.as_i64()).unwrap_or(0) / 1_000_000;
                    ex.set_header(headers::PUBLISH_TIME, iso8601_millis(ms));
                }
                ex.set_body(Value::Bytes(m.get("data").map(|d| d.to_bytes()).unwrap_or_default()));
                set_batch(&mut ex, i, total);
                delivered += 1;
                if !process(logger, processor, &mut ex) {
                    break;
                }
                if self.ack_auto {
                    self.commit(&sub, partition, offset + 1)?;
                }
            }
        }
        Ok(delivered)
    }
}

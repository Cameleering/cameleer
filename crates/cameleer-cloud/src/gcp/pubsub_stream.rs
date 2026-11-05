//! Pub/Sub **streaming pull** over gRPC (cameleer-grpc HTTP/2 client): the default consumer of
//! `google-pubsub` (`synchronousPull=false`, as in Camel).
//!
//! Each of the `concurrentConsumers` streams calls the bidirectional
//! `google.pubsub.v1.Subscriber/StreamingPull` with the bearer token in the `authorization`
//! metadata, sends the initial request (subscription, `ackDeadlineSeconds` stream deadline,
//! flow control `maxOutstandingMessages` / `maxOutstandingBytes`) and processes the received
//! messages in order. Successful exchanges are acknowledged and failed ones nacked (deadline
//! 0, immediate redelivery) on the same stream (`ackMode=AUTO`); `ackMode=NONE` leaves them to
//! the ack deadline. While a message is being processed its deadline is extended every third
//! of `ackDeadlineSeconds`, for at most `maxAckExtensionPeriod` seconds (default 3600). Broken
//! streams are reopened with exponential backoff (100 ms … 5 s); an `UNAUTHENTICATED` status
//! refreshes the token.
//!
//! Exactly-once delivery: when the server announces `subscription_properties.exactly_once_delivery_enabled`,
//! acknowledgements are tracked until the stream's `acknowledge_confirmation` arrives —
//! `temporary_failed_ack_ids` are acknowledged again (at most 5 times), `invalid_ack_ids`
//! (expired leases) are logged (the message is redelivered); the counters are available from
//! [`exactly_once_stats`] per subscription. Dead-letter policies apply to streaming pull like to
//! synchronous pull: nacked messages are forwarded after `maxDeliveryAttempts` deliveries (the
//! `CamelGooglePubsubDeliveryAttempt` header counts them).
//!
//! Target: `pubsubEndpoint` (`host:port`; `https://` = TLS, `http://` or a bare address of an
//! emulator = plaintext), otherwise the `endpoint` / `uriEndpointOverride` host, otherwise
//! `pubsub.googleapis.com:443` over TLS (ALPN `h2`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cameleer_core::context::WeakContext;
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::spi::{Processor, Service};
use cameleer_core::value::{ObjectMap, Value};
use cameleer_grpc::grpc::client::{CallSender, GrpcClient};
use cameleer_grpc::grpc::{Code, Metadata};
use cameleer_grpc::proto::DescriptorPool;
use cameleer_tls::TlsClientConfig;

use super::pubsub::headers;
use super::{GcpConfig, CLOUD_PLATFORM_SCOPE};
use crate::endpoint::{process, set_batch};
use crate::util::{iso8601_millis, random_hex};

/// The gRPC method path.
pub const STREAMING_PULL: &str = "/google.pubsub.v1.Subscriber/StreamingPull";

/// The subset of `google/pubsub/v1/pubsub.proto` used by streaming pull.
pub const PUBSUB_PROTO: &str = r#"
syntax = "proto3";
package google.pubsub.v1;
import "google/protobuf/timestamp.proto";

message PubsubMessage {
  bytes data = 1;
  map<string, string> attributes = 2;
  string message_id = 3;
  google.protobuf.Timestamp publish_time = 4;
  string ordering_key = 5;
}

message ReceivedMessage {
  string ack_id = 1;
  PubsubMessage message = 2;
  int32 delivery_attempt = 3;
}

message StreamingPullRequest {
  string subscription = 1;
  repeated string ack_ids = 2;
  repeated int32 modify_deadline_seconds = 3;
  repeated string modify_deadline_ack_ids = 4;
  int32 stream_ack_deadline_seconds = 5;
  string client_id = 6;
  int64 max_outstanding_messages = 7;
  int64 max_outstanding_bytes = 8;
}

message AcknowledgeConfirmation {
  repeated string ack_ids = 1;
  repeated string invalid_ack_ids = 2;
  repeated string unordered_ack_ids = 3;
  repeated string temporary_failed_ack_ids = 4;
}

message ModifyAckDeadlineConfirmation {
  repeated string ack_ids = 1;
  repeated string invalid_ack_ids = 2;
  repeated string temporary_failed_ack_ids = 3;
}

message SubscriptionProperties {
  bool exactly_once_delivery_enabled = 1;
  bool message_ordering_enabled = 2;
}

message StreamingPullResponse {
  repeated ReceivedMessage received_messages = 1;
  ModifyAckDeadlineConfirmation modify_ack_deadline_confirmation = 3;
  SubscriptionProperties subscription_properties = 4;
  AcknowledgeConfirmation acknowledge_confirmation = 5;
}

service Subscriber {
  rpc StreamingPull(stream StreamingPullRequest) returns (stream StreamingPullResponse);
}
"#;

/// The descriptor pool of [`PUBSUB_PROTO`] (parsed once).
pub fn pool() -> &'static DescriptorPool {
    static POOL: OnceLock<DescriptorPool> = OnceLock::new();
    POOL.get_or_init(|| DescriptorPool::from_source("google/pubsub/v1/pubsub.proto", PUBSUB_PROTO).expect("embedded pubsub.proto parses"))
}

/// Encode a `StreamingPullRequest` map.
pub fn encode_request(v: &Value) -> Result<Vec<u8>> {
    pool().encode("google.pubsub.v1.StreamingPullRequest", v)
}

/// Exactly-once acknowledgement counters of a subscription (confirmed, invalid, retried).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExactlyOnceStats {
    pub confirmed: usize,
    pub invalid: usize,
    pub retried: usize,
}

fn eo_stats() -> &'static Mutex<HashMap<String, ExactlyOnceStats>> {
    static S: OnceLock<Mutex<HashMap<String, ExactlyOnceStats>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The exactly-once acknowledgement counters of `projects/{p}/subscriptions/{s}` (process wide).
pub fn exactly_once_stats(subscription: &str) -> ExactlyOnceStats {
    eo_stats().lock().unwrap().get(subscription).copied().unwrap_or_default()
}

/// Settings of the streaming consumer.
#[derive(Clone)]
pub struct StreamSettings {
    pub gcp: GcpConfig,
    /// `host:port`.
    pub target: String,
    /// TLS server name when using TLS.
    pub tls: Option<String>,
    /// `projects/{p}/subscriptions/{s}`.
    pub subscription: String,
    pub ack_deadline_seconds: i64,
    pub max_outstanding_messages: i64,
    pub max_outstanding_bytes: i64,
    pub max_ack_extension: Duration,
    pub ack_auto: bool,
    pub concurrent_consumers: usize,
}

impl StreamSettings {
    /// Resolve the gRPC target from `pubsubEndpoint` / the endpoint override.
    pub fn resolve_target(pubsub_endpoint: Option<&str>, gcp: &GcpConfig) -> Result<(String, Option<String>)> {
        let split = |s: &str, default_port: u16| -> (String, String) {
            let hostport = s.split('/').next().unwrap_or(s).to_string();
            let host = hostport.rsplit_once(':').map(|(h, _)| h.to_string()).unwrap_or_else(|| hostport.clone());
            let hp = if hostport.contains(':') { hostport } else { format!("{}:{}", hostport, default_port) };
            (hp, host)
        };
        let (spec, overridden) = match (pubsub_endpoint, &gcp.endpoint) {
            (Some(p), _) => (p.to_string(), true),
            (None, Some(e)) => (e.clone(), true),
            (None, None) => ("https://pubsub.googleapis.com:443".to_string(), false),
        };
        if let Some(rest) = spec.strip_prefix("https://") {
            let (hp, host) = split(rest, 443);
            return Ok((hp, Some(host)));
        }
        if let Some(rest) = spec.strip_prefix("http://") {
            return Ok((split(rest, 80).0, None));
        }
        let (hp, host) = split(&spec, 443);
        Ok(if overridden { (hp, None) } else { (hp, Some(host)) })
    }
}

struct Inner {
    settings: StreamSettings,
    processor: Arc<dyn Processor>,
    ctx: WeakContext,
    uri: String,
    running: AtomicBool,
    senders: Mutex<HashMap<u64, CallSender>>,
    next_stream: std::sync::atomic::AtomicU64,
}

/// The streaming pull consumer.
pub struct StreamingPullConsumer {
    inner: Arc<Inner>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl StreamingPullConsumer {
    pub fn new(settings: StreamSettings, ctx: WeakContext, uri: &str, processor: Arc<dyn Processor>) -> StreamingPullConsumer {
        StreamingPullConsumer {
            inner: Arc::new(Inner { settings, processor, ctx, uri: uri.to_string(), running: AtomicBool::new(false), senders: Mutex::new(HashMap::new()), next_stream: std::sync::atomic::AtomicU64::new(0) }),
            threads: Mutex::new(Vec::new()),
        }
    }
}

impl Service for StreamingPullConsumer {
    fn start(&self) -> Result<()> {
        if self.inner.running.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let mut threads = self.threads.lock().unwrap();
        for i in 0..self.inner.settings.concurrent_consumers.max(1) {
            let inner = self.inner.clone();
            let h = std::thread::Builder::new()
                .name(format!("pubsub-stream-{}", i))
                .spawn(move || run(inner))
                .map_err(|e| CamelError::io(format!("Cannot start Pub/Sub streaming thread: {}", e)))?;
            threads.push(h);
        }
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        if !self.inner.running.swap(false, Ordering::SeqCst) {
            return Ok(());
        }
        for (_, s) in self.inner.senders.lock().unwrap().drain() {
            s.cancel();
        }
        for h in self.threads.lock().unwrap().drain(..) {
            let _ = h.join();
        }
        Ok(())
    }
}

impl Drop for StreamingPullConsumer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn client(s: &StreamSettings) -> GrpcClient {
    match &s.tls {
        None => GrpcClient::plaintext(&s.target),
        Some(name) => GrpcClient::tls(&s.target, name, TlsClientConfig::builder().alpn_protocols(&["h2"]).build()),
    }
}

/// Sleep up to `d` while running.
fn nap(inner: &Inner, d: Duration) {
    let end = Instant::now() + d;
    while inner.running.load(Ordering::SeqCst) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10).min(end.saturating_duration_since(Instant::now())));
    }
}

fn run(inner: Arc<Inner>) {
    let s = inner.settings.clone();
    let client = client(&s);
    let client_id = format!("cameleer-{}", random_hex(8));
    let mut backoff = Duration::from_millis(100);
    while inner.running.load(Ordering::SeqCst) {
        match stream_once(&inner, &client, &client_id) {
            Ok(()) => backoff = Duration::from_millis(100),
            Err((code, msg)) => {
                if !inner.running.load(Ordering::SeqCst) {
                    break;
                }
                if code == Code::Unauthenticated {
                    s.gcp.invalidate(CLOUD_PLATFORM_SCOPE);
                }
                cameleer_core::camel_warn!(&inner.uri, "Pub/Sub streaming pull failed ({}): {}; reconnecting in {:?}", code.name(), msg, backoff);
                nap(&inner, backoff);
                backoff = (backoff * 2).min(Duration::from_secs(5));
            }
        }
    }
    client.close();
}

/// One stream; `Ok` when the server ended it cleanly.
fn stream_once(inner: &Arc<Inner>, client: &GrpcClient, client_id: &str) -> std::result::Result<(), (Code, String)> {
    let s = &inner.settings;
    let mut md: Metadata = vec![("x-goog-request-params".into(), format!("subscription={}", s.subscription))];
    match s.gcp.bearer(CLOUD_PLATFORM_SCOPE) {
        Ok(Some(t)) => md.push(("authorization".into(), format!("Bearer {}", t))),
        Ok(None) => {}
        Err(e) => return Err((Code::Unauthenticated, e.to_string())),
    }
    let mut call = client.start_call(STREAMING_PULL, &md, None).map_err(|st| (st.code, st.message))?;
    let sender = call.sender();
    let enc = |v: Value| encode_request(&v).map_err(|e| (Code::Internal, e.to_string()));
    let mut init = ObjectMap::new();
    init.insert("subscription".into(), Value::from(s.subscription.clone()));
    init.insert("stream_ack_deadline_seconds".into(), Value::Int(s.ack_deadline_seconds));
    init.insert("client_id".into(), Value::from(client_id));
    init.insert("max_outstanding_messages".into(), Value::Int(s.max_outstanding_messages));
    init.insert("max_outstanding_bytes".into(), Value::Int(s.max_outstanding_bytes));
    sender.send(&enc(Value::Object(init))?).map_err(|st| (st.code, st.message))?;
    let stream_id = inner.next_stream.fetch_add(1, Ordering::SeqCst);
    inner.senders.lock().unwrap().insert(stream_id, sender.clone());
    if !inner.running.load(Ordering::SeqCst) {
        sender.cancel();
    }
    // lease management: extend the deadline of messages being processed
    let inflight: Arc<Mutex<HashMap<String, Instant>>> = Arc::new(Mutex::new(HashMap::new()));
    let alive = Arc::new(AtomicBool::new(true));
    let lease = {
        let (inflight, alive, sender, inner2) = (inflight.clone(), alive.clone(), sender.clone(), inner.clone());
        let period = Duration::from_millis(((s.ack_deadline_seconds.max(1) * 1000) / 3).max(100) as u64);
        let (deadline, max_ext) = (s.ack_deadline_seconds, s.max_ack_extension);
        std::thread::spawn(move || {
            let mut next = Instant::now() + period;
            while alive.load(Ordering::SeqCst) && inner2.running.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(10));
                if Instant::now() < next {
                    continue;
                }
                next = Instant::now() + period;
                let ids: Vec<String> = inflight.lock().unwrap().iter().filter(|(_, t)| t.elapsed() < max_ext).map(|(id, _)| id.clone()).collect();
                if ids.is_empty() {
                    continue;
                }
                let secs = vec![Value::Int(deadline); ids.len()];
                let req = Value::object([("modify_deadline_ack_ids", Value::from(ids)), ("modify_deadline_seconds", Value::Array(secs))]);
                if let Ok(b) = encode_request(&req) {
                    let _ = sender.send(&b);
                }
            }
        })
    };
    let mut exactly_once = false;
    // exactly-once: ack id -> attempts, awaiting confirmation
    let mut pending: HashMap<String, usize> = HashMap::new();
    let result = loop {
        let msg = match call.recv() {
            Ok(Some(m)) => m,
            Ok(None) => break Ok(()),
            Err(st) => break Err((st.code, st.message)),
        };
        let resp = match pool().decode("google.pubsub.v1.StreamingPullResponse", &msg) {
            Ok(v) => v,
            Err(e) => break Err((Code::Internal, e.to_string())),
        };
        if let Some(p) = resp.get("subscription_properties") {
            exactly_once = p.get("exactly_once_delivery_enabled").is_some_and(|b| b.is_truthy());
        }
        if let Some(c) = resp.get("acknowledge_confirmation") {
            let list = |k: &str| c.get(k).and_then(|a| a.as_array()).map(|a| a.iter().map(|x| x.to_string()).collect::<Vec<_>>()).unwrap_or_default();
            let mut stats = eo_stats().lock().unwrap();
            let st = stats.entry(s.subscription.clone()).or_default();
            for id in list("ack_ids") {
                pending.remove(&id);
                st.confirmed += 1;
            }
            for id in list("invalid_ack_ids") {
                pending.remove(&id);
                st.invalid += 1;
                cameleer_core::camel_warn!(&inner.uri, "Pub/Sub exactly-once acknowledgement of {} failed permanently (lease expired): the message will be redelivered", id);
            }
            let retry: Vec<String> = list("temporary_failed_ack_ids").into_iter().filter(|id| pending.get(id).is_some_and(|n| *n < 5)).collect();
            st.retried += retry.len();
            drop(stats);
            if !retry.is_empty() {
                for id in &retry {
                    *pending.get_mut(id).expect("pending") += 1;
                }
                if let Err(st) = encode_request(&Value::object([("ack_ids", Value::from(retry))])).map_err(|e| cameleer_grpc::Status::new(Code::Internal, e.to_string())).and_then(|b| sender.send(&b)) {
                    cameleer_core::camel_warn!(&inner.uri, "Error re-acknowledging Pub/Sub messages: {}", st);
                }
            }
        }
        let received = resp.get("received_messages").and_then(|r| r.as_array()).cloned().unwrap_or_default();
        let total = received.len();
        for (i, rm) in received.iter().enumerate() {
            let ack_id = rm.get("ack_id").map(|a| a.to_string()).unwrap_or_default();
            inflight.lock().unwrap().insert(ack_id.clone(), Instant::now());
            let mut ex = to_exchange(inner, rm, &ack_id);
            set_batch(&mut ex, i, total);
            let ok = process(&inner.uri, inner.processor.as_ref(), &mut ex);
            inflight.lock().unwrap().remove(&ack_id);
            if s.ack_auto {
                if ok && exactly_once {
                    pending.insert(ack_id.clone(), 1);
                }
                let req = if ok {
                    Value::object([("ack_ids", Value::from(vec![ack_id]))])
                } else {
                    Value::object([("modify_deadline_ack_ids", Value::from(vec![ack_id])), ("modify_deadline_seconds", Value::Array(vec![Value::Int(0)]))])
                };
                if let Err(st) = encode_request(&req).map_err(|e| cameleer_grpc::Status::new(Code::Internal, e.to_string())).and_then(|b| sender.send(&b)) {
                    cameleer_core::camel_warn!(&inner.uri, "Error acknowledging Pub/Sub message: {}", st);
                }
            }
        }
    };
    alive.store(false, Ordering::SeqCst);
    let _ = lease.join();
    sender.cancel();
    inner.senders.lock().unwrap().remove(&stream_id);
    result
}

fn timestamp_text(v: Option<&Value>) -> Value {
    match v {
        Some(t @ Value::Object(_)) => {
            let secs = t.get("seconds").and_then(|s| s.as_i64().or_else(|| s.to_string().parse().ok())).unwrap_or(0);
            let nanos = t.get("nanos").and_then(|n| n.as_i64()).unwrap_or(0);
            Value::from(iso8601_millis(secs * 1000 + nanos / 1_000_000))
        }
        Some(other) => other.clone(),
        None => Value::Null,
    }
}

fn to_exchange(inner: &Inner, rm: &Value, ack_id: &str) -> Exchange {
    let m = rm.get("message").cloned().unwrap_or_default();
    let mut ex = Exchange::new(inner.ctx.upgrade());
    ex.set_body(Value::Bytes(m.get("data").map(|d| d.to_bytes()).unwrap_or_default()));
    ex.set_header(headers::MESSAGE_ID, m.get("message_id").cloned().unwrap_or_default());
    ex.set_header(headers::ACK_ID, ack_id.to_string());
    ex.set_header(headers::PUBLISH_TIME, timestamp_text(m.get("publish_time")));
    ex.set_header(headers::ATTRIBUTES, m.get("attributes").cloned().unwrap_or(Value::Object(ObjectMap::new())));
    if let Some(k) = m.get("ordering_key").filter(|k| !k.to_string().is_empty()) {
        ex.set_header(headers::ORDERING_KEY, k.clone());
    }
    if let Some(d) = rm.get("delivery_attempt").filter(|d| d.as_i64().unwrap_or(0) > 0) {
        ex.set_header(headers::DELIVERY_ATTEMPT, d.clone());
    }
    ex
}

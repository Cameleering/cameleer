//! The `google-pubsub` component (Camel `camel-google-pubsub`) over the Pub/Sub REST API.
//!
//! `google-pubsub://projectId:topicOrSubscription?serviceAccountKey=...`
//!
//! Producer (destination = topic): publishes the body (base64 `data`) with the attributes of
//! the `CamelGooglePubsubAttributes` header (map) and the `CamelGooglePubsubOrderingKey`; sets
//! `CamelGooglePubsubMessageId`. An array body is published as one message per element (the
//! header then holds the list of ids).
//!
//! Consumer (destination = subscription): synchronous `pull` of up to `maxMessagesPerPoll`
//! (default 1) messages; body = data bytes, headers `CamelGooglePubsubMessageId`,
//! `CamelGooglePubsubAttributes`, `CamelGooglePubsubAckId`, `CamelGooglePubsubPublishTime`,
//! `CamelGooglePubsubOrderingKey`, `CamelGooglePubsubDeliveryAttempt`. With `ackMode=AUTO`
//! (default) successful exchanges are acknowledged and failed ones nacked
//! (`modifyAckDeadline` 0); `ackMode=NONE` leaves acknowledgement to the subscription deadline.
//!
//! The consumer above is the **synchronous pull** one (`synchronousPull=true`, scheduled
//! polls). By default (`synchronousPull=false`, as in Camel) the consumer uses gRPC
//! **streaming pull** ([`super::pubsub_stream`]): `concurrentConsumers` streams (default 1),
//! `ackDeadlineSeconds` (stream ack deadline, default 10), `maxOutstandingMessages` (default
//! 1000) / `maxOutstandingBytes` (default 100 MiB) flow control, `maxAckExtensionPeriod`
//! (seconds, default 3600) deadline extension while processing, target `pubsubEndpoint`.
//!
//! Other options: `authenticate` (default true), `endpoint` (emulator `host:port`),
//! `messageOrderingEnabled`.
//!
//! **Dead-letter policy and exactly-once delivery.** Cameleer producer operations
//! (`operation` option / `CamelGooglePubsubOperation` header; default `publish`) manage the
//! resources: `createTopic`, `deleteTopic`, `createSubscription` (destination = subscription,
//! topic from `topic` / `CamelGooglePubsubTopic`; `deadLetterTopic` + `maxDeliveryAttempts`
//! (5..100) set the `deadLetterPolicy`, `enableExactlyOnceDelivery`, `ackDeadlineSeconds`,
//! `messageOrderingEnabled`), `getSubscription`, `deleteSubscription`. Messages delivered
//! `maxDeliveryAttempts` times without acknowledgement are forwarded to the dead-letter topic by
//! the service; consumers see the attempt in `CamelGooglePubsubDeliveryAttempt`. With
//! exactly-once delivery the synchronous pull consumer extends the ack deadline of a message
//! while it is processed (every `ackDeadlineSeconds / 3`, up to `maxAckExtensionPeriod`
//! seconds) and checks every acknowledgement: transient failures
//! (`TRANSIENT_FAILURE_*` in the `ErrorInfo` metadata) are retried, permanent ones
//! (`PERMANENT_FAILURE_INVALID_ACK_ID`, e.g. an expired lease) are logged (the message will be
//! redelivered).

use std::sync::Arc;
use std::time::Duration;

use cameleer_core::component::scheduled::ScheduledPollConfig;
use cameleer_core::context::{CamelContext, WeakContext};
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::spi::{Consumer, Processor};
use cameleer_core::value::{ObjectMap, Value};

use super::GcpConfig;
use crate::endpoint::{process, set_batch, CloudService};
use crate::http::{Request, Url};
use crate::util::{base64_decode, base64_encode, header_str, kinds, obj, parse_json, Opts};

/// Header names (Camel `GooglePubsubConstants`).
pub mod headers {
    pub const MESSAGE_ID: &str = "CamelGooglePubsubMessageId";
    pub const ACK_ID: &str = "CamelGooglePubsubAckId";
    pub const PUBLISH_TIME: &str = "CamelGooglePubsubPublishTime";
    pub const ATTRIBUTES: &str = "CamelGooglePubsubAttributes";
    pub const ORDERING_KEY: &str = "CamelGooglePubsubOrderingKey";
    pub const DELIVERY_ATTEMPT: &str = "CamelGooglePubsubDeliveryAttempt";
    /// Cameleer extensions.
    pub const OPERATION: &str = "CamelGooglePubsubOperation";
    pub const TOPIC: &str = "CamelGooglePubsubTopic";
}

/// The Pub/Sub endpoint configuration and client.
pub struct PubsubService {
    pub gcp: GcpConfig,
    pub project: String,
    pub destination: String,
    pub max_messages_per_poll: i64,
    pub ack_auto: bool,
    pub message_ordering_enabled: bool,
    pub scheduler: ScheduledPollConfig,
    /// Streaming pull settings (`None` = synchronous pull).
    pub streaming: Option<super::pubsub_stream::StreamSettings>,
    pub operation: Option<String>,
    pub topic: Option<String>,
    pub dead_letter_topic: Option<String>,
    pub max_delivery_attempts: Option<i64>,
    pub exactly_once: bool,
    pub ack_deadline_seconds: i64,
    pub max_ack_extension: Duration,
    /// Acknowledgements that failed permanently (exactly-once delivery).
    pub ack_failures: std::sync::atomic::AtomicUsize,
}

/// Factory for the component.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let (project, destination) = remaining
        .split_once(':')
        .filter(|(p, d)| !p.is_empty() && !d.is_empty())
        .ok_or_else(|| CamelError::illegal_argument("Expected google-pubsub://projectId:destinationName"))?;
    let pubsub_endpoint = o.get("pubsubEndpoint");
    let gcp = GcpConfig::from_opts(o)?;
    let ack = o.string("ackMode", "AUTO").to_ascii_uppercase();
    if ack != "AUTO" && ack != "NONE" {
        return Err(CamelError::illegal_argument(format!("Invalid ackMode: {} (AUTO or NONE)", ack)));
    }
    let ack_deadline_seconds = o.i64("ackDeadlineSeconds", 10)?.clamp(1, 600);
    let max_ack_extension = Duration::from_secs(o.i64("maxAckExtensionPeriod", 3600)?.max(0) as u64);
    let streaming = if o.bool("synchronousPull", false)? {
        o.ignore(&["concurrentConsumers", "maxOutstandingMessages", "maxOutstandingBytes"]);
        None
    } else {
        let (target, tls) = super::pubsub_stream::StreamSettings::resolve_target(pubsub_endpoint.as_deref(), &gcp)?;
        Some(super::pubsub_stream::StreamSettings {
            gcp: gcp.clone(),
            target,
            tls,
            subscription: format!("projects/{}/subscriptions/{}", project, destination),
            ack_deadline_seconds,
            max_outstanding_messages: o.i64("maxOutstandingMessages", 1000)?,
            max_outstanding_bytes: o.i64("maxOutstandingBytes", 100 * 1024 * 1024)?,
            max_ack_extension,
            ack_auto: ack == "AUTO",
            concurrent_consumers: o.i64("concurrentConsumers", 1)?.max(1) as usize,
        })
    };
    let s = PubsubService {
        project: project.to_string(),
        destination: destination.to_string(),
        max_messages_per_poll: o.i64("maxMessagesPerPoll", 1)?,
        ack_auto: ack == "AUTO",
        message_ordering_enabled: o.bool("messageOrderingEnabled", false)?,
        scheduler: ScheduledPollConfig::from_params(o.params, Duration::from_millis(1000), Duration::from_millis(500))?,
        gcp,
        streaming,
        operation: o.get("operation"),
        topic: o.get("topic"),
        dead_letter_topic: o.get("deadLetterTopic"),
        max_delivery_attempts: crate::util::opt_i64(o, "maxDeliveryAttempts")?,
        exactly_once: o.bool("enableExactlyOnceDelivery", false)?,
        ack_deadline_seconds,
        max_ack_extension,
        ack_failures: std::sync::atomic::AtomicUsize::new(0),
    };
    if let Some(m) = s.max_delivery_attempts {
        if !(5..=100).contains(&m) {
            return Err(CamelError::illegal_argument(format!("maxDeliveryAttempts must be between 5 and 100: {}", m)));
        }
    }
    Ok(Arc::new(s))
}

impl PubsubService {
    fn url(&self, kind: &str, verb: &str) -> Result<Url> {
        Url::parse(&format!("{}/v1/projects/{}/{}/{}:{}", self.gcp.base("https://pubsub.googleapis.com"), self.project, kind, self.destination, verb))
    }

    fn post(&self, url: Url, body: &Value) -> Result<Value> {
        let req = Request::new("POST", url).header("Content-Type", "application/json").body(body.to_json().into_bytes());
        parse_json(&self.gcp.send(req, kinds::GOOGLE_API)?.body)
    }

    /// Publish messages `(data, attributes, ordering key)`; returns the message ids.
    pub fn publish(&self, messages: &[(Vec<u8>, ObjectMap, Option<String>)]) -> Result<Vec<String>> {
        let msgs: Vec<Value> = messages
            .iter()
            .map(|(d, a, k)| {
                let mut m = ObjectMap::new();
                m.insert("data".to_string(), Value::from(base64_encode(d)));
                if !a.is_empty() {
                    m.insert("attributes".into(), Value::Object(a.iter().map(|(k, v)| (k.clone(), Value::from(v.to_string()))).collect()));
                }
                if let Some(k) = k {
                    m.insert("orderingKey".into(), Value::from(k.clone()));
                }
                Value::Object(m)
            })
            .collect();
        let r = self.post(self.url("topics", "publish")?, &obj([("messages", Value::Array(msgs))]))?;
        Ok(r.get("messageIds").and_then(|i| i.as_array()).map(|a| a.iter().map(|v| v.to_string()).collect()).unwrap_or_default())
    }

    /// Pull messages from the subscription (raw `receivedMessages`).
    pub fn pull(&self, max: i64) -> Result<Vec<Value>> {
        let r = self.post(self.url("subscriptions", "pull")?, &obj([("maxMessages", Value::Int(max.max(1))), ("returnImmediately", Value::Bool(true))]))?;
        Ok(r.get("receivedMessages").and_then(|m| m.as_array()).cloned().unwrap_or_default())
    }

    pub fn acknowledge(&self, ack_ids: &[String]) -> Result<()> {
        self.post(self.url("subscriptions", "acknowledge")?, &obj([("ackIds", Value::from(ack_ids.to_vec()))]))?;
        Ok(())
    }

    /// Acknowledge, checking the per-ack-id result of exactly-once subscriptions: transient
    /// failures are retried (3 attempts), permanent ones returned as an error.
    pub fn acknowledge_checked(&self, ack_ids: &[String]) -> Result<()> {
        let mut pending = ack_ids.to_vec();
        for attempt in 0..3 {
            let req = Request::new("POST", self.url("subscriptions", "acknowledge")?).header("Content-Type", "application/json").body(obj([("ackIds", Value::from(pending.clone()))]).to_json().into_bytes());
            let resp = self.gcp.send_raw(req, super::CLOUD_PLATFORM_SCOPE)?;
            if resp.is_success() {
                return Ok(());
            }
            let v = parse_json(&resp.body).unwrap_or_default();
            let meta: Vec<(String, String)> = v
                .get("error")
                .and_then(|e| e.get("details"))
                .and_then(|d| d.as_array())
                .into_iter()
                .flatten()
                .filter(|d| d.get("reason").map(|r| r.to_string()).as_deref() == Some("EXACTLY_ONCE_ACKID_FAILURE"))
                .flat_map(|d| d.get("metadata").and_then(|m| m.as_object()).cloned().unwrap_or_default())
                .map(|(k, v)| (k, v.to_string()))
                .collect();
            if meta.is_empty() {
                return Err(super::google_error(kinds::GOOGLE_API, &resp));
            }
            let permanent: Vec<&(String, String)> = meta.iter().filter(|(_, r)| r.starts_with("PERMANENT")).collect();
            if !permanent.is_empty() {
                self.ack_failures.fetch_add(permanent.len(), std::sync::atomic::Ordering::SeqCst);
                return Err(crate::util::cloud_error(kinds::GOOGLE_API, format!("Acknowledgement failed permanently (exactly-once delivery): {}", permanent.iter().map(|(k, r)| format!("{}={}", k, r)).collect::<Vec<_>>().join(", "))));
            }
            pending = meta.into_iter().map(|(k, _)| k).collect();
            std::thread::sleep(Duration::from_millis(50 * (attempt + 1)));
        }
        Err(crate::util::cloud_error(kinds::GOOGLE_API, "Acknowledgement still failing after retries (transient exactly-once failures)"))
    }

    /// Number of acknowledgements that failed permanently.
    pub fn permanent_ack_failures(&self) -> usize {
        self.ack_failures.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn admin(&self, method: &str, kind: &str, name: &str, body: Option<&Value>) -> Result<Value> {
        let url = format!("{}/v1/projects/{}/{}/{}", self.gcp.base("https://pubsub.googleapis.com"), self.project, kind, name);
        let mut req = Request::new(method, Url::parse(&url)?);
        if let Some(b) = body {
            req = req.header("Content-Type", "application/json").body(b.to_json().into_bytes());
        }
        let resp = self.gcp.send(req, kinds::GOOGLE_API)?;
        if resp.body.is_empty() {
            Ok(Value::Object(ObjectMap::new()))
        } else {
            parse_json(&resp.body)
        }
    }

    fn full_topic(&self, t: &str) -> String {
        if t.starts_with("projects/") {
            t.to_string()
        } else {
            format!("projects/{}/topics/{}", self.project, t)
        }
    }

    fn admin_op(&self, ex: &mut Exchange, op: &str) -> Result<()> {
        let body = match op {
            "createTopic" => self.admin("PUT", "topics", &self.destination, Some(&obj([])))?,
            "deleteTopic" => self.admin("DELETE", "topics", &self.destination, None)?,
            "getSubscription" => self.admin("GET", "subscriptions", &self.destination, None)?,
            "deleteSubscription" => self.admin("DELETE", "subscriptions", &self.destination, None)?,
            "createSubscription" => {
                let topic = header_str(ex, headers::TOPIC).or_else(|| self.topic.clone()).ok_or_else(|| CamelError::illegal_argument("createSubscription needs the topic option or the CamelGooglePubsubTopic header"))?;
                let mut m = ObjectMap::new();
                m.insert("topic".into(), Value::from(self.full_topic(&topic)));
                m.insert("ackDeadlineSeconds".into(), Value::Int(self.ack_deadline_seconds));
                if self.exactly_once {
                    m.insert("enableExactlyOnceDelivery".into(), Value::Bool(true));
                }
                if self.message_ordering_enabled {
                    m.insert("enableMessageOrdering".into(), Value::Bool(true));
                }
                if let Some(dlt) = &self.dead_letter_topic {
                    let mut p = ObjectMap::new();
                    p.insert("deadLetterTopic".into(), Value::from(self.full_topic(dlt)));
                    p.insert("maxDeliveryAttempts".into(), Value::Int(self.max_delivery_attempts.unwrap_or(5)));
                    m.insert("deadLetterPolicy".into(), Value::Object(p));
                }
                self.admin("PUT", "subscriptions", &self.destination, Some(&Value::Object(m)))?
            }
            other => return Err(crate::endpoint::unsupported("google-pubsub", other)),
        };
        ex.set_body(body);
        Ok(())
    }

    /// Extend the ack deadline of an in-flight message every `ackDeadlineSeconds / 3`.
    fn start_extender(&self, ack_id: &str, logger: &str) -> Option<(std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>)> {
        if self.max_ack_extension.is_zero() {
            return None;
        }
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let period = Duration::from_millis(((self.ack_deadline_seconds as u64 * 1000) / 3).max(100));
        let deadline = self.ack_deadline_seconds;
        let until = std::time::Instant::now() + self.max_ack_extension;
        let (gcp, url, ack, logger) = (self.gcp.clone(), self.url("subscriptions", "modifyAckDeadline").ok()?, ack_id.to_string(), logger.to_string());
        let h = std::thread::spawn(move || {
            while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(period) {
                if std::time::Instant::now() >= until {
                    return;
                }
                let req = Request::new("POST", url.clone()).header("Content-Type", "application/json").body(obj([("ackIds", Value::from(vec![ack.clone()])), ("ackDeadlineSeconds", Value::Int(deadline))]).to_json().into_bytes());
                if let Err(e) = gcp.send(req, kinds::GOOGLE_API) {
                    cameleer_core::camel_warn!(logger.as_str(), "Cannot extend the ack deadline of a Pub/Sub message: {}", e);
                }
            }
        });
        Some((tx, h))
    }

    /// Change the ack deadline (0 = nack).
    pub fn modify_ack_deadline(&self, ack_ids: &[String], seconds: i64) -> Result<()> {
        self.post(self.url("subscriptions", "modifyAckDeadline")?, &obj([("ackIds", Value::from(ack_ids.to_vec())), ("ackDeadlineSeconds", Value::Int(seconds))]))?;
        Ok(())
    }
}

impl CloudService for PubsubService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = crate::endpoint::operation(ex, headers::OPERATION, &self.operation, "publish");
        if op != "publish" {
            return self.admin_op(ex, &op);
        }
        let attrs: ObjectMap = match ex.header(headers::ATTRIBUTES) {
            Some(Value::Object(m)) => m.clone(),
            _ => ObjectMap::new(),
        };
        let key = header_str(ex, headers::ORDERING_KEY);
        let bodies: Vec<Vec<u8>> = match ex.body() {
            Value::Array(a) => a.iter().map(|v| v.to_bytes()).collect(),
            other => vec![other.to_bytes()],
        };
        let is_list = matches!(ex.body(), Value::Array(_));
        let msgs: Vec<_> = bodies.into_iter().map(|b| (b, attrs.clone(), key.clone())).collect();
        let ids = self.publish(&msgs)?;
        if is_list {
            ex.set_header(headers::MESSAGE_ID, Value::from(ids));
        } else {
            ex.set_header(headers::MESSAGE_ID, ids.into_iter().next().unwrap_or_default());
        }
        Ok(())
    }

    fn scheduler(&self) -> Option<&ScheduledPollConfig> {
        Some(&self.scheduler)
    }

    fn custom_consumer(&self, ctx: &WeakContext, uri: &str, processor: &Arc<dyn Processor>) -> Option<Result<Arc<dyn Consumer>>> {
        let s = self.streaming.clone()?;
        Some(Ok(Arc::new(super::pubsub_stream::StreamingPullConsumer::new(s, ctx.clone(), uri, processor.clone()))))
    }

    fn poll(&self, ctx: Option<CamelContext>, processor: &dyn Processor, logger: &str) -> Result<usize> {
        let received = self.pull(self.max_messages_per_poll)?;
        let total = received.len();
        for (i, rm) in received.iter().enumerate() {
            let ack_id = rm.get("ackId").map(|a| a.to_string()).unwrap_or_default();
            let m = rm.get("message").cloned().unwrap_or_default();
            let mut ex = Exchange::new(ctx.clone());
            ex.set_body(Value::Bytes(m.get("data").and_then(|d| base64_decode(&d.to_string())).unwrap_or_default()));
            ex.set_header(headers::MESSAGE_ID, m.get("messageId").cloned().unwrap_or_default());
            ex.set_header(headers::ACK_ID, ack_id.clone());
            ex.set_header(headers::PUBLISH_TIME, m.get("publishTime").cloned().unwrap_or_default());
            ex.set_header(headers::ATTRIBUTES, m.get("attributes").cloned().unwrap_or(Value::Object(ObjectMap::new())));
            if let Some(k) = m.get("orderingKey").filter(|k| !k.to_string().is_empty()) {
                ex.set_header(headers::ORDERING_KEY, k.clone());
            }
            if let Some(d) = rm.get("deliveryAttempt") {
                ex.set_header(headers::DELIVERY_ATTEMPT, d.clone());
            }
            set_batch(&mut ex, i, total);
            let extender = if self.ack_auto { self.start_extender(&ack_id, logger) } else { None };
            let ok = process(logger, processor, &mut ex);
            if let Some((stop, h)) = extender {
                let _ = stop.send(());
                let _ = h.join();
            }
            if self.ack_auto {
                let r = if ok { self.acknowledge_checked(std::slice::from_ref(&ack_id)) } else { self.modify_ack_deadline(std::slice::from_ref(&ack_id), 0) };
                if let Err(e) = r {
                    cameleer_core::camel_warn!(logger, "Error acknowledging Pub/Sub message: {}", e);
                }
            }
        }
        Ok(total)
    }
}

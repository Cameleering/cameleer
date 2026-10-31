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
//! `messageOrderingEnabled`, `maxDeliveryAttempts` (accepted).

use std::sync::Arc;
use std::time::Duration;

use camel_core::component::scheduled::ScheduledPollConfig;
use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::{Consumer, Processor};
use camel_core::value::{ObjectMap, Value};

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
    let streaming = if o.bool("synchronousPull", false)? {
        o.ignore(&["concurrentConsumers", "maxAckExtensionPeriod", "ackDeadlineSeconds", "maxOutstandingMessages", "maxOutstandingBytes"]);
        None
    } else {
        let (target, tls) = super::pubsub_stream::StreamSettings::resolve_target(pubsub_endpoint.as_deref(), &gcp)?;
        Some(super::pubsub_stream::StreamSettings {
            gcp: gcp.clone(),
            target,
            tls,
            subscription: format!("projects/{}/subscriptions/{}", project, destination),
            ack_deadline_seconds: o.i64("ackDeadlineSeconds", 10)?.clamp(1, 600),
            max_outstanding_messages: o.i64("maxOutstandingMessages", 1000)?,
            max_outstanding_bytes: o.i64("maxOutstandingBytes", 100 * 1024 * 1024)?,
            max_ack_extension: Duration::from_secs(o.i64("maxAckExtensionPeriod", 3600)?.max(0) as u64),
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
    };
    o.ignore(&["maxDeliveryAttempts"]);
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

    /// Change the ack deadline (0 = nack).
    pub fn modify_ack_deadline(&self, ack_ids: &[String], seconds: i64) -> Result<()> {
        self.post(self.url("subscriptions", "modifyAckDeadline")?, &obj([("ackIds", Value::from(ack_ids.to_vec())), ("ackDeadlineSeconds", Value::Int(seconds))]))?;
        Ok(())
    }
}

impl CloudService for PubsubService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
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
            let ok = process(logger, processor, &mut ex);
            if self.ack_auto {
                let r = if ok { self.acknowledge(std::slice::from_ref(&ack_id)) } else { self.modify_ack_deadline(std::slice::from_ref(&ack_id), 0) };
                if let Err(e) = r {
                    camel_core::camel_warn!(logger, "Error acknowledging Pub/Sub message: {}", e);
                }
            }
        }
        Ok(total)
    }
}

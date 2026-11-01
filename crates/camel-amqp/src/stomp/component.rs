//! The `stomp` component and the JMS-style `jms` / `activemq` / `amqp-stomp` components, all
//! speaking STOMP 1.2 (ActiveMQ, Artemis and RabbitMQ accept STOMP).
//!
//! ```text
//! stomp:queue:name?brokerURL=tcp://host:61613&login=&passcode=&host=
//! jms:[queue:|topic:|temp-queue:|temp-topic:]name?brokerURL=tcp://host:61613&username=&password=
//!     &concurrentConsumers=1&replyTo=&requestTimeout=20000&selector=&deliveryPersistent=true
//!     &deliveryMode=&timeToLive=-1&priority=4&explicitQosEnabled=false&preserveMessageQos=false
//!     &disableReplyTo=false&transacted=false&acknowledgementModeName=AUTO_ACKNOWLEDGE
//!     &jmsMessageType=Text|Bytes&clientId=&recoveryInterval=5000&prefetchSize=1
//! ```
//!
//! The broker url comes from `brokerURL` (alias `brokerUrl`) or the context properties
//! `camel.component.<scheme>.brokerUrl` / `brokerURL` (also `remoteURI` for `amqp-stomp`), the
//! credentials from `username`/`password` (`login`/`passcode` for `stomp`) or
//! `camel.component.<scheme>.username|password`. Urls such as `tcp://h:p`, `stomp://h:p`,
//! `amqp://h:p` and `failover:(tcp://h:p,...)` are accepted (the first address is used).
//! TLS: `ssl://h:p`, `stomp+ssl://h:p`, `stomps://h:p` (or `sslContextParameters=#bean` /
//! `useGlobalSslContextParameters=true` with any url) connect over TLS; trust comes from
//! the parameters, the global `camel.ssl.*` ones, else the system trust store.
//!
//! **stomp**: `stomp:queue:a` is the STOMP destination `/queue/a`. Consumers subscribe
//! with `ack:auto` and map every STOMP header to a Camel header; producers `SEND` the body
//! with every non-`Camel*` header.
//!
//! **jms / activemq / amqp-stomp**: queues are point-to-point (competing consumers, one STOMP
//! subscription per concurrent consumer with a prefetch window of `prefetchSize`), topics
//! publish-subscribe. Consumers acknowledge after processing (`client-individual`):
//! `AUTO_ACKNOWLEDGE`/`DUPS_OK_ACKNOWLEDGE` always ack, `CLIENT_ACKNOWLEDGE` nacks failed
//! messages (redelivery) and `transacted=true` processes every message in a STOMP
//! transaction (`BEGIN`, `ACK`, `COMMIT`; a failure `ABORT`s so the broker redelivers with
//! `JMSRedelivered=true`). `selector` is a JMS selector evaluated by the broker (it is
//! validated when the endpoint is created). Messages carry the JMS headers `JMSMessageID`,
//! `JMSCorrelationID`, `JMSDestination`, `JMSReplyTo`, `JMSDeliveryMode` (1/2),
//! `JMSPriority`, `JMSTimestamp`, `JMSType`, `JMSRedelivered`, `JMSExpiration`; other
//! properties become headers (keys decoded with Camel's default key format strategy:
//! `_DOT_` and `_HYPHEN_`).
//!
//! A message with a reply-to makes the exchange InOut; the result is sent to the reply-to
//! destination with the request's `JMSCorrelationID` (or its `JMSMessageID`), a failure
//! with the `CamelExceptionKind`/`CamelExceptionMessage` headers. Producers of InOut
//! exchanges (unless `disableReplyTo`) send with a reply-to (a temporary queue, or the
//! fixed `replyTo` queue) and a correlation id, and wait `requestTimeout` (header
//! `CamelJmsRequestTimeout`) for the reply. Producers honour `deliveryPersistent` /
//! `deliveryMode`, `priority`, `timeToLive` (`expires`), `preserveMessageQos`
//! (`JMSPriority`, `JMSDeliveryMode`, `JMSExpiration` headers), `JMSCorrelationID`,
//! `JMSType`, `JMSReplyTo` and the destination override header `CamelJmsDestinationName`.
//! Persistent sends wait for a broker receipt; `transacted` producers send in a transaction.
//!
//! **Subscriptions**: `durableSubscriptionName` (or `subscriptionName` with
//! `subscriptionDurable=true`, plus `clientId`) subscribes with ActiveMQ's
//! `activemq.subscriptionName` and Artemis' `durable-subscription-name` headers, so the
//! broker keeps messages while the route is stopped (stopping only detaches; a durable
//! subscription has a single subscriber). `subscriptionShared=true` with
//! `subscriptionName` makes the concurrent consumers (and other applications using the
//! name) share one subscription (`subscription-shared:true`, `shared-subscription-name`).
//!
//! **Message types**: map bodies are MapMessages and `jmsMessageType=Object` gives
//! ObjectMessages, both as JSON with ActiveMQ's `transformation` header (`jms-map-json` /
//! `jms-object-json`); received transformed messages become maps / values again.
//!
//! **Replies**: `replyToType=Temporary` (default without `replyTo`) uses a temporary queue
//! per producer, `Shared` (default with `replyTo`) reads each reply from the shared queue
//! with a `JMSCorrelationID` selector, `Exclusive` routes the fixed queue's replies by
//! correlation id. `useMessageIDAsCorrelationID=true`: consumers answer with the request's
//! `JMSMessageID` as correlation id; producers wait on a private temporary queue (STOMP
//! brokers assign the message id, so the producer cannot know it in advance).
//!
//! **XA**: STOMP has no two-phase commit, so XA / JTA transaction managers
//! (`transactionManager`) cannot be honoured; the option is accepted and ignored and
//! `transacted` gives local (STOMP `BEGIN`/`COMMIT`) transactions, the usual emulation
//! with idempotent consumers for exactly-once processing.


use std::any::Any;
use std::collections::HashMap;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camel_core::component::support::{BackgroundThreads, StopFlag};
use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{kinds, CamelError, Result};
use camel_core::message::{Exchange, ExchangePattern};
use camel_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::value::Value;

use super::client::{AckMode, StompConfig, StompConnection, Subscription};
use super::frame::Frame;
use super::{jms_destination_name, normalize_destination};
use crate::common::{broker_url_is_tls, now_millis, parse_broker_url, take_ignored, unique_id, COMMON_IGNORED};
use crate::selector::Selector;

/// JMS header names (Camel `JmsConstants`).
pub mod headers {
    pub const JMS_MESSAGE_ID: &str = "JMSMessageID";
    pub const JMS_CORRELATION_ID: &str = "JMSCorrelationID";
    pub const JMS_DESTINATION: &str = "JMSDestination";
    pub const JMS_REPLY_TO: &str = "JMSReplyTo";
    pub const JMS_DELIVERY_MODE: &str = "JMSDeliveryMode";
    pub const JMS_PRIORITY: &str = "JMSPriority";
    pub const JMS_TIMESTAMP: &str = "JMSTimestamp";
    pub const JMS_TYPE: &str = "JMSType";
    pub const JMS_REDELIVERED: &str = "JMSRedelivered";
    pub const JMS_EXPIRATION: &str = "JMSExpiration";
    pub const JMS_DESTINATION_NAME: &str = "CamelJmsDestinationName";
    pub const JMS_REQUEST_TIMEOUT: &str = "CamelJmsRequestTimeout";
    pub const EXCEPTION_KIND: &str = "CamelExceptionKind";
    pub const EXCEPTION_MESSAGE: &str = "CamelExceptionMessage";
}

/// Which component flavour an endpoint follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    /// `stomp`: raw STOMP headers, InOnly.
    Stomp,
    /// `jms`, `activemq`, `amqp-stomp`: JMS semantics.
    Jms,
}

/// JMS acknowledgement mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JmsAck {
    Auto,
    Client,
    DupsOk,
}

/// Forced JMS message type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageType {
    Text,
    Bytes,
    /// MapMessage: JSON with the ActiveMQ `transformation:jms-map-json` header.
    Map,
    /// ObjectMessage: the body serialized as JSON with `transformation:jms-object-json`.
    Object,
}

/// Where InOut replies come back (Camel `replyToType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyToType {
    /// A temporary queue per producer (default without `replyTo`).
    Temporary,
    /// The fixed `replyTo` queue is shared with other clients: each request reads its reply
    /// with a `JMSCorrelationID` selector (default with `replyTo`).
    Shared,
    /// The fixed `replyTo` queue is ours alone: one reader routes replies by correlation id.
    Exclusive,
}

/// STOMP `transformation` header values of ActiveMQ for map / object messages.
pub const TRANSFORMATION_MAP: &str = "jms-map-json";
pub const TRANSFORMATION_OBJECT: &str = "jms-object-json";

/// Resolved endpoint configuration.
#[derive(Clone, Debug)]
pub struct JmsConfig {
    pub flavor: Flavor,
    pub conn: StompConfig,
    /// STOMP destination (`/queue/x`).
    pub destination: String,
    pub concurrent_consumers: usize,
    pub reply_to: Option<String>,
    pub request_timeout: Duration,
    pub selector: Option<String>,
    pub delivery_persistent: bool,
    pub time_to_live: i64,
    pub priority: Option<u8>,
    pub preserve_message_qos: bool,
    pub disable_reply_to: bool,
    pub transacted: bool,
    pub ack: JmsAck,
    pub message_type: Option<MessageType>,
    pub prefetch: usize,
    pub recovery_interval: Duration,
    /// Durable topic subscription name (`durableSubscriptionName`, `subscriptionName` with
    /// `subscriptionDurable=true`); needs `clientId` unless shared.
    pub durable_subscription: Option<String>,
    /// JMS 2.0 shared subscription name (`subscriptionShared=true`).
    pub shared_subscription: Option<String>,
    /// `useMessageIDAsCorrelationID`.
    pub use_message_id_as_correlation_id: bool,
    pub reply_to_type: ReplyToType,
}

/// Camel's default `JmsKeyFormatStrategy`: `.` → `_DOT_`, `-` → `_HYPHEN_`.
pub fn encode_key(k: &str) -> String {
    k.replace('.', "_DOT_").replace('-', "_HYPHEN_")
}

/// Inverse of [`encode_key`].
pub fn decode_key(k: &str) -> String {
    k.replace("_DOT_", ".").replace("_HYPHEN_", "-")
}

fn bool_opt(key: &str, v: Option<String>, default: bool) -> Result<bool> {
    match v.as_deref().map(str::trim) {
        None => Ok(default),
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        Some(o) => Err(CamelError::illegal_argument(format!("Invalid boolean for {}: {}", key, o))),
    }
}

fn num_opt<T: std::str::FromStr>(key: &str, v: Option<String>, default: T) -> Result<T> {
    match v {
        None => Ok(default),
        Some(s) => s.trim().parse().map_err(|_| CamelError::illegal_argument(format!("Invalid number for {}: {}", key, s))),
    }
}

fn ms_opt(key: &str, v: Option<String>, default: Duration) -> Result<Duration> {
    match v {
        None => Ok(default),
        Some(s) => match s.trim().parse::<i64>() {
            Ok(n) => Ok(Duration::from_millis(n.max(0) as u64)),
            Err(_) => camel_core::util::parse_duration(s.trim()).map_err(|_| CamelError::illegal_argument(format!("Invalid duration for {}: {}", key, s))),
        },
    }
}

impl JmsConfig {
    fn from_uri(flavor: Flavor, scheme: &str, ctx: &CamelContext, remaining: &str, params: &mut Parameters) -> Result<JmsConfig> {
        let props = ctx.properties();
        let comp = |k: &str| props.get(&format!("camel.component.{}.{}", scheme, k));
        let url = params
            .take("brokerURL")
            .or_else(|| params.take("brokerUrl"))
            .or_else(|| comp("brokerURL"))
            .or_else(|| comp("brokerUrl"))
            .or_else(|| comp("remoteURI"))
            .or_else(|| comp("connectionFactory.brokerURL"))
            .unwrap_or_else(|| "tcp://localhost:61613".to_string());
        let (host, port) = parse_broker_url(&url, 61613)?;
        let tls = camel_tls::camel_support::endpoint_tls(ctx, params, scheme, broker_url_is_tls(&url), &[])?;
        let (login_key, pass_key) = if flavor == Flavor::Stomp { ("login", "passcode") } else { ("username", "password") };
        let login = params.take(login_key).or_else(|| comp(login_key)).or_else(|| params.take("login")).or_else(|| comp("login"));
        let passcode = params.take(pass_key).or_else(|| comp(pass_key)).or_else(|| params.take("passcode")).or_else(|| comp("passcode"));
        let virtual_host = params.take("host").or_else(|| comp("host"));
        let client_id = params.take("clientId").or_else(|| comp("clientId"));
        let conn = StompConfig { host, port, login, passcode, virtual_host, client_id, tls, ..Default::default() };
        let destination = match flavor {
            Flavor::Stomp => {
                if remaining.starts_with('/') {
                    remaining.to_string()
                } else {
                    format!("/{}", remaining.replace(':', "/"))
                }
            }
            Flavor::Jms => normalize_destination(remaining),
        };
        let name = match flavor {
            Flavor::Jms => destination.splitn(3, '/').nth(2).unwrap_or(""),
            Flavor::Stomp => destination.trim_start_matches('/'),
        };
        if name.is_empty() {
            return Err(CamelError::illegal_argument(format!("Destination name must be specified: {}", remaining)));
        }
        let mut o = |k: &str| params.take(k).or_else(|| comp(k));
        let concurrent_consumers = num_opt("concurrentConsumers", o("concurrentConsumers"), 1usize)?.max(1);
        let reply_to = o("replyTo").filter(|s| !s.is_empty()).map(|s| normalize_destination(&s));
        let request_timeout = ms_opt("requestTimeout", o("requestTimeout"), Duration::from_millis(20_000))?;
        let selector = o("selector").filter(|s| !s.trim().is_empty());
        if let Some(s) = &selector {
            Selector::parse(s)?;
        }
        let mut delivery_persistent = bool_opt("deliveryPersistent", o("deliveryPersistent"), true)?;
        match o("deliveryMode").as_deref().map(str::trim) {
            None => {}
            Some("2") | Some("PERSISTENT") => delivery_persistent = true,
            Some("1") | Some("NON_PERSISTENT") => delivery_persistent = false,
            Some(other) => return Err(CamelError::illegal_argument(format!("Invalid deliveryMode: {}", other))),
        }
        let time_to_live = num_opt("timeToLive", o("timeToLive"), -1i64)?;
        let priority_v = num_opt("priority", o("priority"), -1i32)?;
        if priority_v > 9 {
            return Err(CamelError::illegal_argument(format!("Invalid priority (0-9): {}", priority_v)));
        }
        o("explicitQosEnabled");
        let preserve_message_qos = bool_opt("preserveMessageQos", o("preserveMessageQos"), false)?;
        let disable_reply_to = bool_opt("disableReplyTo", o("disableReplyTo"), false)?;
        let mut transacted = bool_opt("transacted", o("transacted"), false)?;
        let ack_name = o("acknowledgementModeName");
        let ack = match ack_name.as_deref() {
            None | Some("AUTO_ACKNOWLEDGE") => JmsAck::Auto,
            Some("CLIENT_ACKNOWLEDGE") => JmsAck::Client,
            Some("DUPS_OK_ACKNOWLEDGE") => JmsAck::DupsOk,
            Some("SESSION_TRANSACTED") => {
                transacted = true;
                JmsAck::Auto
            }
            Some(other) => return Err(CamelError::illegal_argument(format!("Invalid acknowledgementModeName: {}", other))),
        };
        let ack = match o("acknowledgementMode").map(|s| s.trim().to_string()).as_deref() {
            None => ack,
            Some("0") => {
                transacted = true;
                ack
            }
            Some("1") => JmsAck::Auto,
            Some("2") => JmsAck::Client,
            Some("3") => JmsAck::DupsOk,
            Some(other) => return Err(CamelError::illegal_argument(format!("Invalid acknowledgementMode: {}", other))),
        };
        let message_type = match o("jmsMessageType").as_deref() {
            None => None,
            Some("Text") => Some(MessageType::Text),
            Some("Bytes") => Some(MessageType::Bytes),
            Some("Map") => Some(MessageType::Map),
            Some("Object") => Some(MessageType::Object),
            Some(other) => return Err(CamelError::illegal_argument(format!("Unsupported jmsMessageType (Text, Bytes, Map or Object): {}", other))),
        };
        let prefetch = num_opt("prefetchSize", o("prefetchSize"), 1usize)?;
        let recovery_interval = ms_opt("recoveryInterval", o("recoveryInterval"), Duration::from_millis(5000))?;
        let durable_name = o("durableSubscriptionName").filter(|s| !s.is_empty());
        let sub_name = o("subscriptionName").filter(|s| !s.is_empty());
        let shared = bool_opt("subscriptionShared", o("subscriptionShared"), false)?;
        let durable = bool_opt("subscriptionDurable", o("subscriptionDurable"), false)? || durable_name.is_some();
        let is_topic = destination.starts_with("/topic/");
        let name = durable_name.or(sub_name);
        let durable_subscription = if durable && is_topic { Some(name.clone().unwrap_or_else(|| destination.trim_start_matches("/topic/").to_string())) } else { None };
        let shared_subscription = if shared && is_topic { Some(name.unwrap_or_else(|| destination.trim_start_matches("/topic/").to_string())) } else { None };
        if durable_subscription.is_some() && shared_subscription.is_none() && conn.client_id.is_none() && flavor == Flavor::Jms {
            return Err(CamelError::illegal_argument("A durable (non-shared) subscription requires the clientId option"));
        }
        let use_message_id_as_correlation_id = bool_opt("useMessageIDAsCorrelationID", o("useMessageIDAsCorrelationID"), false)?;
        let reply_to_type = match o("replyToType").as_deref().map(str::trim) {
            None if reply_to.is_some() => ReplyToType::Shared,
            None => ReplyToType::Temporary,
            Some("Temporary") => ReplyToType::Temporary,
            Some("Shared") => ReplyToType::Shared,
            Some("Exclusive") => ReplyToType::Exclusive,
            Some(other) => return Err(CamelError::illegal_argument(format!("Invalid replyToType (Temporary, Shared or Exclusive): {}", other))),
        };
        if reply_to_type != ReplyToType::Temporary && reply_to.is_none() {
            return Err(CamelError::illegal_argument(format!("replyToType={:?} requires replyTo", reply_to_type)));
        }
        take_ignored(params, COMMON_IGNORED);
        take_ignored(
            params,
            &[
                "maxConcurrentConsumers",
                "connectionFactory",
                "receiveTimeout",
                "testConnectionOnStartup",
                "includeSentJMSMessageID",
                "mapJmsMessage",
                "alwaysCopyMessage",
                "asyncConsumer",
                "asyncStartListener",
                "asyncStopListener",
                "cacheLevelName",
                "pubSubNoLocal",
                "headerFilterStrategy",
                "messageConverter",
                "transferException",
                "transferExchange",
                "lazyCreateTransactionManager",
                "transactionManager",
                "replyToDeliveryPersistent",
                "includeAllJMSXProperties",
                "version",
                "customHeaders",
                "artemisStreamingEnabled",
            ],
        );
        Ok(JmsConfig {
            flavor,
            conn,
            destination,
            concurrent_consumers,
            reply_to,
            request_timeout,
            selector,
            delivery_persistent,
            time_to_live,
            priority: if priority_v >= 0 { Some(priority_v as u8) } else { None },
            preserve_message_qos,
            disable_reply_to,
            transacted,
            ack,
            message_type,
            prefetch,
            recovery_interval,
            durable_subscription,
            shared_subscription,
            use_message_id_as_correlation_id,
            reply_to_type,
        })
    }
}

/// The `stomp` / `jms` / `activemq` / `amqp-stomp` component.
pub struct JmsComponent {
    flavor: Flavor,
    scheme: &'static str,
}

impl JmsComponent {
    /// The `stomp` component.
    pub fn stomp() -> Self {
        JmsComponent { flavor: Flavor::Stomp, scheme: "stomp" }
    }
    /// A JMS-style component (`jms`, `activemq`, `amqp-stomp`) over STOMP.
    pub fn jms(scheme: &'static str) -> Self {
        JmsComponent { flavor: Flavor::Jms, scheme }
    }
}

impl Component for JmsComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let cfg = JmsConfig::from_uri(self.flavor, self.scheme, ctx, remaining, params)?;
        Ok(Arc::new(JmsEndpoint { uri: uri.to_string(), ctx: ctx.downgrade(), cfg: Arc::new(cfg) }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A `stomp` / `jms` / `activemq` / `amqp-stomp` endpoint.
pub struct JmsEndpoint {
    uri: String,
    ctx: WeakContext,
    cfg: Arc<JmsConfig>,
}

impl JmsEndpoint {
    pub fn config(&self) -> &JmsConfig {
        &self.cfg
    }
}

impl Endpoint for JmsEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(JmsProducer { cfg: self.cfg.clone(), uri: self.uri.clone(), state: Mutex::new(None) }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        Ok(Arc::new(JmsConsumer { cfg: self.cfg.clone(), uri: self.uri.clone(), ctx: self.ctx.clone(), processor, threads: BackgroundThreads::new() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---------------------------------------------------------------------------
// message mapping
// ---------------------------------------------------------------------------

/// STOMP headers that are not user properties.
const STOMP_RESERVED: &[&str] = &[
    "subscription",
    "ack",
    "message-id",
    "destination",
    "content-length",
    "content-type",
    "persistent",
    "priority",
    "expires",
    "timestamp",
    "redelivered",
    "correlation-id",
    "reply-to",
    "type",
    "transaction",
    "receipt",
    "transformation",
];

fn frame_to_exchange(cfg: &JmsConfig, f: &Frame, ex: &mut Exchange) {
    use headers::*;
    let ct = f.get("content-type").unwrap_or("").to_ascii_lowercase();
    let as_bytes = match cfg.message_type {
        Some(MessageType::Bytes) => true,
        Some(_) => false,
        None => ct.starts_with("application/octet-stream"),
    };
    let transformed = cfg.flavor == Flavor::Jms && matches!(f.get("transformation"), Some(TRANSFORMATION_MAP) | Some(TRANSFORMATION_OBJECT) | Some("jms-json"));
    let body = if transformed {
        // MapMessage / ObjectMessage: back to a structured body
        let text = String::from_utf8_lossy(&f.body).to_string();
        camel_core::json::parse(&text).unwrap_or(Value::String(text))
    } else if as_bytes {
        Value::Bytes(f.body.clone())
    } else {
        match String::from_utf8(f.body.clone()) {
            Ok(s) => Value::String(s),
            Err(e) => Value::Bytes(e.into_bytes()),
        }
    };
    ex.set_body(body);
    if cfg.flavor == Flavor::Stomp {
        for (k, v) in &f.headers {
            ex.set_header(k.clone(), v.clone());
        }
        return;
    }
    let get = |k: &str| f.get(k).map(String::from);
    if let Some(v) = get("message-id") {
        ex.set_header(JMS_MESSAGE_ID, v);
    }
    if let Some(v) = get("destination") {
        ex.set_header(JMS_DESTINATION, jms_destination_name(&v));
    }
    if let Some(v) = get("correlation-id") {
        ex.set_header(JMS_CORRELATION_ID, v);
    }
    if let Some(v) = get("reply-to") {
        ex.set_header(JMS_REPLY_TO, jms_destination_name(&v));
    }
    ex.set_header(JMS_DELIVERY_MODE, if f.get("persistent") == Some("true") { 2i64 } else { 1i64 });
    ex.set_header(JMS_PRIORITY, f.get("priority").and_then(|p| p.parse::<i64>().ok()).unwrap_or(4));
    if let Some(t) = f.get("timestamp").and_then(|p| p.parse::<i64>().ok()) {
        ex.set_header(JMS_TIMESTAMP, t);
    }
    ex.set_header(JMS_EXPIRATION, f.get("expires").and_then(|p| p.parse::<i64>().ok()).unwrap_or(0));
    if let Some(v) = get("type") {
        ex.set_header(JMS_TYPE, v);
    }
    ex.set_header(JMS_REDELIVERED, f.get("redelivered") == Some("true"));
    for (k, v) in &f.headers {
        if STOMP_RESERVED.contains(&k.as_str()) {
            continue;
        }
        ex.set_header(decode_key(k), v.clone());
    }
}

fn header_str(ex: &Exchange, k: &str) -> Option<String> {
    ex.header(k).filter(|v| !v.is_null()).map(|v| v.to_string()).filter(|s| !s.is_empty())
}

/// Body bytes and STOMP headers for an outgoing message.
fn exchange_to_frame(cfg: &JmsConfig, ex: &Exchange, include_reply_to: bool) -> (Vec<(String, String)>, Vec<u8>) {
    use headers::*;
    let mut h: Vec<(String, String)> = Vec::new();
    let (mut body, ct) = match ex.body() {
        Value::Null => (Vec::new(), "text/plain;charset=UTF-8"),
        Value::Bytes(b) => (b.clone(), "application/octet-stream"),
        Value::String(s) => (s.as_bytes().to_vec(), "text/plain;charset=UTF-8"),
        v @ (Value::Object(_) | Value::Array(_)) => (v.to_json().into_bytes(), "application/json"),
        other => (other.to_string().into_bytes(), "text/plain;charset=UTF-8"),
    };
    // JMS: maps are MapMessages (Camel's default mapping), Object forces an ObjectMessage
    let transformation = match (cfg.flavor, cfg.message_type, ex.body()) {
        (Flavor::Jms, Some(MessageType::Object), _) => Some(TRANSFORMATION_OBJECT),
        (Flavor::Jms, Some(MessageType::Map), _) | (Flavor::Jms, None, Value::Object(_)) => Some(TRANSFORMATION_MAP),
        _ => None,
    };
    if transformation.is_some() && !matches!(ex.body(), Value::Object(_) | Value::Array(_)) {
        body = ex.body().to_json().into_bytes();
    }
    let ct = match (cfg.message_type, transformation) {
        (_, Some(_)) => "application/json",
        (Some(MessageType::Bytes), _) => "application/octet-stream",
        (Some(_), _) => "text/plain;charset=UTF-8",
        (None, None) => ct,
    };
    if cfg.flavor == Flavor::Stomp {
        for (k, v) in ex.message.headers.iter() {
            if k.to_ascii_lowercase().starts_with("camel") || v.is_null() || STOMP_RESERVED.contains(&k) && k != "content-type" {
                continue;
            }
            h.push((k.to_string(), v.to_string()));
        }
        if !h.iter().any(|(k, _)| k == "content-type") {
            h.push(("content-type".into(), ct.into()));
        }
        return (h, body);
    }
    h.push(("content-type".into(), ct.into()));
    if let Some(t) = transformation {
        h.push(("transformation".into(), t.into()));
    }
    let mut persistent = cfg.delivery_persistent;
    let mut priority = cfg.priority;
    let mut expires = if cfg.time_to_live > 0 { now_millis() + cfg.time_to_live } else { 0 };
    if cfg.preserve_message_qos {
        match header_str(ex, JMS_DELIVERY_MODE).as_deref() {
            Some("2") | Some("PERSISTENT") => persistent = true,
            Some("1") | Some("NON_PERSISTENT") => persistent = false,
            _ => {}
        }
        if let Some(p) = ex.header(JMS_PRIORITY).and_then(|v| v.as_i64()) {
            priority = Some(p.clamp(0, 9) as u8);
        }
        if let Some(e) = ex.header(JMS_EXPIRATION).and_then(|v| v.as_i64()).filter(|e| *e > 0) {
            expires = e;
        }
    }
    // NON_PERSISTENT is the absence of the header: RabbitMQ reads `persistent` on a SEND to
    // `/queue/x` as the queue's `durable` flag, so `persistent:false` fails against the
    // existing (durable) queue
    if persistent {
        h.push(("persistent".into(), "true".into()));
    }
    if let Some(p) = priority {
        h.push(("priority".into(), p.to_string()));
    }
    if expires > 0 {
        h.push(("expires".into(), expires.to_string()));
    }
    if let Some(c) = header_str(ex, JMS_CORRELATION_ID) {
        h.push(("correlation-id".into(), c));
    }
    if let Some(t) = header_str(ex, JMS_TYPE) {
        h.push(("type".into(), t));
    }
    if include_reply_to {
        if let Some(r) = header_str(ex, JMS_REPLY_TO) {
            h.push(("reply-to".into(), normalize_destination(&r)));
        }
    }
    for (k, v) in ex.message.headers.iter() {
        let lower = k.to_ascii_lowercase();
        if lower.starts_with("camel") || lower.starts_with("jms") || v.is_null() || lower == "content-type" {
            continue;
        }
        h.push((encode_key(k), v.to_string()));
    }
    (h, body)
}

// ---------------------------------------------------------------------------
// producer
// ---------------------------------------------------------------------------

type Pending = Arc<Mutex<HashMap<String, Sender<Frame>>>>;

struct ReplyRouter {
    destination: String,
    pending: Pending,
    stop: Arc<StopFlag>,
    thread: Option<std::thread::JoinHandle<()>>,
}

struct ProducerState {
    conn: Arc<StompConnection>,
    reply: Option<ReplyRouter>,
}

impl ProducerState {
    fn shutdown(mut self) {
        if let Some(mut r) = self.reply.take() {
            r.stop.stop();
            if let Some(t) = r.thread.take() {
                let _ = t.join();
            }
        }
        self.conn.disconnect();
    }
}

struct JmsProducer {
    cfg: Arc<JmsConfig>,
    uri: String,
    state: Mutex<Option<ProducerState>>,
}

impl JmsProducer {
    fn connection(&self, g: &mut Option<ProducerState>) -> Result<Arc<StompConnection>> {
        if g.as_ref().is_some_and(|s| !s.conn.is_open()) {
            if let Some(old) = g.take() {
                old.shutdown();
            }
        }
        if g.is_none() {
            *g = Some(ProducerState { conn: Arc::new(StompConnection::open(&self.cfg.conn)?), reply: None });
        }
        Ok(g.as_ref().unwrap().conn.clone())
    }

    fn reply_router(&self, s: &mut ProducerState) -> Result<(String, Pending)> {
        if s.reply.is_none() {
            let destination = match self.cfg.reply_to_type {
                ReplyToType::Exclusive => self.cfg.reply_to.clone().unwrap_or_default(),
                _ => format!("/temp-queue/camel-reply-{}", unique_id()),
            };
            let sub: Subscription = s.conn.subscribe_reply_queue(&destination)?;
            let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
            let stop = StopFlag::new();
            let (p2, s2) = (pending.clone(), stop.clone());
            let thread = std::thread::Builder::new()
                .name(format!("Camel {} reply router", self.uri))
                .spawn(move || {
                    while !s2.is_stopped() {
                        match sub.messages.recv_timeout(Duration::from_millis(100)) {
                            Ok(f) => {
                                let id = f.get("correlation-id").unwrap_or("").to_string();
                                if let Some(tx) = p2.lock().unwrap().remove(&id) {
                                    let _ = tx.send(f);
                                }
                            }
                            Err(RecvTimeoutError::Timeout) => {}
                            Err(RecvTimeoutError::Disconnected) => break,
                        }
                    }
                })
                .map_err(|e| CamelError::io(e.to_string()))?;
            s.reply = Some(ReplyRouter { destination, pending, stop, thread: Some(thread) });
        }
        let r = s.reply.as_ref().unwrap();
        Ok((r.destination.clone(), r.pending.clone()))
    }

    fn send(&self, exchange: &mut Exchange) -> Result<()> {
        use headers::*;
        let cfg = &self.cfg;
        let destination = match header_str(exchange, JMS_DESTINATION_NAME) {
            Some(d) if cfg.flavor == Flavor::Jms => normalize_destination(&d),
            _ => cfg.destination.clone(),
        };
        let in_out = cfg.flavor == Flavor::Jms && exchange.is_out_capable() && !cfg.disable_reply_to;
        let (mut hdrs, body) = exchange_to_frame(cfg, exchange, !in_out);
        let mut g = self.state.lock().unwrap();
        let conn = match self.connection(&mut g) {
            Ok(c) => c,
            Err(e) => {
                if let Some(old) = g.take() {
                    old.shutdown();
                }
                return Err(e);
            }
        };
        let receipt = cfg.flavor == Flavor::Stomp || cfg.delivery_persistent;
        if !in_out {
            drop(g);
            return self.send_frame(&conn, &destination, &hdrs, &body, receipt);
        }
        let timeout = exchange.header(JMS_REQUEST_TIMEOUT).and_then(|v| v.as_i64()).map(|ms| Duration::from_millis(ms.max(1) as u64)).unwrap_or(cfg.request_timeout);
        let correlation = header_str(exchange, JMS_CORRELATION_ID).unwrap_or_else(unique_id);
        hdrs.retain(|(k, _)| k != "correlation-id" && k != "reply-to");
        let per_request = cfg.use_message_id_as_correlation_id || cfg.reply_to_type == ReplyToType::Shared;
        let reply = if per_request {
            drop(g);
            // a subscription per request: a private temporary queue when the broker assigns
            // the correlating JMSMessageID, else the shared replyTo queue with a selector
            let (reply_dest, selector) = if cfg.use_message_id_as_correlation_id {
                (format!("/temp-queue/camel-reply-{}", unique_id()), None)
            } else {
                hdrs.push(("correlation-id".into(), correlation.clone()));
                (cfg.reply_to.clone().unwrap_or_default(), Some(format!("JMSCorrelationID = '{}'", correlation.replace('\'', "''"))))
            };
            let extra: Vec<(String, String)> = selector.into_iter().map(|s| ("selector".to_string(), s)).collect();
            let sub = if extra.is_empty() { conn.subscribe_reply_queue(&reply_dest)? } else { conn.subscribe(&reply_dest, AckMode::Auto, &extra)? };
            hdrs.push(("reply-to".into(), reply_dest));
            let r = self.send_frame(&conn, &destination, &hdrs, &body, receipt).and_then(|_| sub.messages.recv_timeout(timeout).map_err(|_| CamelError::new(kinds::EXCHANGE_TIMED_OUT, "")));
            let _ = conn.unsubscribe(&sub.id);
            match r {
                Ok(f) => Ok(f),
                Err(e) if e.kind == kinds::EXCHANGE_TIMED_OUT => Err(()),
                Err(e) => return Err(e),
            }
        } else {
            hdrs.push(("correlation-id".into(), correlation.clone()));
            let (reply_dest, pending) = self.reply_router(g.as_mut().unwrap())?;
            hdrs.push(("reply-to".into(), reply_dest));
            let (tx, rx) = mpsc::channel();
            pending.lock().unwrap().insert(correlation.clone(), tx);
            drop(g);
            if let Err(e) = self.send_frame(&conn, &destination, &hdrs, &body, receipt) {
                pending.lock().unwrap().remove(&correlation);
                return Err(e);
            }
            rx.recv_timeout(timeout).map_err(|_| {
                pending.lock().unwrap().remove(&correlation);
            })
        };
        let reply = match reply {
            Ok(r) => r,
            Err(()) => {
                return Err(CamelError::new(
                    kinds::EXCHANGE_TIMED_OUT,
                    format!(
                        "Exchange timed out after waiting: {} millis. Exchange[{}] (no reply received on {} with JMSCorrelationID {})",
                        timeout.as_millis(),
                        exchange.exchange_id,
                        destination,
                        correlation
                    ),
                ));
            }
        };
        if let Some(kind) = reply.get(EXCEPTION_KIND) {
            return Err(CamelError::new(kind, reply.get(EXCEPTION_MESSAGE).unwrap_or("")));
        }
        exchange.message.headers.clear();
        frame_to_exchange(cfg, &reply, exchange);
        Ok(())
    }

    fn send_frame(&self, conn: &StompConnection, destination: &str, hdrs: &[(String, String)], body: &[u8], receipt: bool) -> Result<()> {
        if self.cfg.transacted {
            let tx = format!("tx-{}", unique_id());
            conn.begin(&tx)?;
            if let Err(e) = conn.send(destination, hdrs, body, Some(&tx), false) {
                let _ = conn.abort(&tx);
                return Err(e);
            }
            return conn.commit(&tx);
        }
        conn.send(destination, hdrs, body, None, receipt)
    }
}

impl Service for JmsProducer {
    fn stop(&self) -> Result<()> {
        if let Some(s) = self.state.lock().unwrap().take() {
            s.shutdown();
        }
        Ok(())
    }
}

impl Processor for JmsProducer {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        self.send(exchange)
    }
}

// ---------------------------------------------------------------------------
// consumer
// ---------------------------------------------------------------------------

struct JmsConsumer {
    cfg: Arc<JmsConfig>,
    uri: String,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
    threads: BackgroundThreads,
}

struct Worker {
    cfg: Arc<JmsConfig>,
    uri: String,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
}

impl Worker {
    fn handle(&self, conn: &StompConnection, f: Frame) {
        let cfg = &self.cfg;
        let mut ex = Exchange::new(self.ctx.upgrade());
        frame_to_exchange(cfg, &f, &mut ex);
        let jms = cfg.flavor == Flavor::Jms;
        let reply_to = if jms && !cfg.disable_reply_to { f.get("reply-to").map(String::from) } else { None };
        if reply_to.is_some() {
            ex.pattern = ExchangePattern::InOut;
        }
        let tx = if jms && cfg.transacted {
            let id = format!("tx-{}", unique_id());
            match conn.begin(&id) {
                Ok(()) => Some(id),
                Err(e) => {
                    camel_core::camel_warn!(&self.uri, "Cannot begin transaction: {}", e.message);
                    return;
                }
            }
        } else {
            None
        };
        if let Err(e) = self.processor.process(&mut ex) {
            ex.set_exception(e);
        }
        let failed = ex.exception.is_some();
        if let Some(e) = &ex.exception {
            camel_core::camel_warn!(&self.uri, "Error processing exchange. Exchange[{}]. Caused by: [{} - {}]", ex.exchange_id, e.kind, e.message);
        }
        if let Some(rt) = reply_to {
            let mut out = ex.clone();
            out.exception = None;
            let reply_cfg = JmsConfig { message_type: None, priority: None, time_to_live: -1, preserve_message_qos: false, ..(**cfg).clone() };
            let (mut hdrs, body) = exchange_to_frame(&reply_cfg, &out, false);
            hdrs.retain(|(k, _)| k != "correlation-id");
            let correlation = if cfg.use_message_id_as_correlation_id { f.get("message-id") } else { f.get("correlation-id").or_else(|| f.get("message-id")) }.unwrap_or("").to_string();
            hdrs.push(("correlation-id".into(), correlation));
            if let Some(e) = &ex.exception {
                hdrs.push((headers::EXCEPTION_KIND.into(), e.kind.clone()));
                hdrs.push((headers::EXCEPTION_MESSAGE.into(), e.message.clone()));
            }
            if let Err(e) = conn.send(&normalize_destination(&rt), &hdrs, &body, tx.as_deref(), false) {
                camel_core::camel_warn!(&self.uri, "Cannot send reply to {}: {}", rt, e.message);
            }
        }
        let Some(ack_id) = f.get("ack").map(String::from) else { return };
        let r = match &tx {
            // RabbitMQ (AMQP transaction semantics) keeps a message whose transactional ACK was
            // aborted unacknowledged instead of redelivering it: NACK it so it is requeued
            // (ActiveMQ redelivers by itself, and treats NACK as a poison ack)
            Some(t) => conn.ack(&ack_id, Some(t)).and_then(|_| {
                if !failed {
                    conn.commit(t)
                } else if conn.is_rabbitmq() {
                    conn.abort(t).and_then(|_| conn.nack(&ack_id, None))
                } else {
                    conn.abort(t)
                }
            }),
            None if failed && cfg.ack == JmsAck::Client => conn.nack(&ack_id, None),
            None => conn.ack(&ack_id, None),
        };
        if let Err(e) = r {
            camel_core::camel_warn!(&self.uri, "Cannot acknowledge message {}: {}", ack_id, e.message);
        }
    }
}

fn setup(cfg: &JmsConfig) -> Result<(StompConnection, Vec<Subscription>)> {
    let conn = StompConnection::open(&cfg.conn)?;
    let mut subs = Vec::new();
    // a durable (non-shared) subscription has a single subscriber
    let n = if cfg.durable_subscription.is_some() && cfg.shared_subscription.is_none() { 1 } else { cfg.concurrent_consumers };
    for _ in 0..n {
        let sub = if cfg.flavor == Flavor::Stomp {
            conn.subscribe(&cfg.destination, AckMode::Auto, &[])?
        } else {
            let mut extra = Vec::new();
            if cfg.prefetch > 0 {
                extra.push(("activemq.prefetchSize".to_string(), cfg.prefetch.to_string()));
            }
            if let Some(s) = &cfg.selector {
                extra.push(("selector".to_string(), s.clone()));
            }
            if let Some(name) = &cfg.shared_subscription {
                extra.push(("subscription-shared".to_string(), "true".to_string()));
                extra.push(("shared-subscription-name".to_string(), name.clone()));
            }
            if let Some(name) = &cfg.durable_subscription {
                // ActiveMQ (with the CONNECT client-id) and Artemis header names, plus
                // RabbitMQ's `durable` / `auto-delete` with a stable subscription id (which
                // names the subscription's queue there)
                extra.push(("activemq.subscriptionName".to_string(), name.clone()));
                extra.push(("durable-subscription-name".to_string(), name.clone()));
                if cfg.shared_subscription.is_none() {
                    extra.push(("durable".to_string(), "true".to_string()));
                    extra.push(("auto-delete".to_string(), "false".to_string()));
                    let id = match &cfg.conn.client_id {
                        Some(c) => format!("{}:{}", c, name),
                        None => name.clone(),
                    };
                    subs.push(conn.subscribe_with_id(&cfg.destination, &id, AckMode::ClientIndividual, &extra)?);
                    continue;
                }
            }
            conn.subscribe(&cfg.destination, AckMode::ClientIndividual, &extra)?

        };
        subs.push(sub);
    }
    Ok((conn, subs))
}

impl Service for JmsConsumer {
    fn start(&self) -> Result<()> {
        if self.threads.is_running() {
            return Ok(());
        }
        let worker = Arc::new(Worker { cfg: self.cfg.clone(), uri: self.uri.clone(), ctx: self.ctx.clone(), processor: self.processor.clone() });
        let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
        let uri = self.uri.clone();
        self.threads.spawn(&format!("Camel {} supervisor", self.uri), move |stop| {
            let mut ready = Some(ready_tx);
            while !stop.is_stopped() {
                let (conn, subs) = match setup(&worker.cfg) {
                    Ok(x) => x,
                    Err(e) => {
                        camel_core::camel_warn!(&uri, "Cannot start consumer on {}: {} - retrying in {} ms", uri, e.message, worker.cfg.recovery_interval.as_millis());
                        if let Some(tx) = ready.take() {
                            let _ = tx.send(Err(e));
                        }
                        if stop.sleep(worker.cfg.recovery_interval) {
                            break;
                        }
                        continue;
                    }
                };
                if let Some(tx) = ready.take() {
                    let _ = tx.send(Ok(()));
                }
                let ids: Vec<String> = subs.iter().map(|s| s.id.clone()).collect();
                std::thread::scope(|scope| {
                    for sub in subs {
                        let (w, st, c) = (worker.clone(), stop.clone(), &conn);
                        scope.spawn(move || loop {
                            if st.is_stopped() {
                                break;
                            }
                            match sub.messages.recv_timeout(Duration::from_millis(50)) {
                                Ok(f) => w.handle(c, f),
                                Err(RecvTimeoutError::Timeout) => {}
                                Err(RecvTimeoutError::Disconnected) => break,
                            }
                        });
                    }
                });
                let lost = !stop.is_stopped();
                if !lost {
                    for id in &ids {
                        let _ = conn.unsubscribe(id);
                    }
                }
                conn.disconnect();
                if lost {
                    camel_core::camel_warn!(&uri, "Connection lost on {} - reconnecting in {} ms", uri, worker.cfg.recovery_interval.as_millis());
                    if stop.sleep(worker.cfg.recovery_interval) {
                        break;
                    }
                }
            }
        })?;
        if let Ok(Err(e)) = ready_rx.recv_timeout(self.cfg.conn.connect_timeout + Duration::from_secs(5)) {
            camel_core::camel_warn!(&self.uri, "Consumer {} not connected yet: {}", self.uri, e.message);
        }
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        self.threads.stop();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_format_strategy() {
        assert_eq!(encode_key("my.key-name"), "my_DOT_key_HYPHEN_name");
        assert_eq!(decode_key("my_DOT_key_HYPHEN_name"), "my.key-name");
    }
}

//! The `amqp` component: Camel's AMQP component (JMS semantics over AMQP 1.0, as with
//! Qpid JMS against ActiveMQ Artemis, ActiveMQ classic, Qpid or Azure Service Bus).
//!
//! ```text
//! amqp:[queue:|topic:|temp-queue:]name?remoteAddress=amqp://host:5672&username=&password=
//!     &concurrentConsumers=1&replyTo=&requestTimeout=20000&selector=&deliveryPersistent=true
//!     &deliveryMode=&timeToLive=-1&priority=&preserveMessageQos=false&disableReplyTo=false
//!     &acknowledgementModeName=AUTO_ACKNOWLEDGE&transacted=false&jmsMessageType=Text|Bytes|Map
//!     &clientId=&durableSubscriptionName=&subscriptionDurable=false&prefetchSize=100
//!     &includeAmqpAnnotations=false&recoveryInterval=5000&idleTimeout=60000&saslMechanism=
//! ```
//!
//! The connection comes from `remoteAddress` (aliases `remoteURI`, `brokerURL`,
//! `connectionFactory.remoteURI`) or the component properties
//! `camel.component.amqp.remoteAddress` (and the same aliases, or `host` + `port`), e.g.
//! `amqp://host:5672` or `amqps://host:5671` (TLS; `sslContextParameters=#bean` and
//! `useGlobalSslContextParameters=true` also select TLS), credentials from `username` /
//! `password` (or the url user info).
//!
//! Messages follow the AMQP JMS mapping: `JMSMessageID` = message-id, `JMSCorrelationID` =
//! correlation-id, `JMSDestination` = to, `JMSReplyTo` = reply-to, `JMSType` = subject,
//! `JMSTimestamp` = creation-time, `JMSExpiration` = absolute-expiry-time,
//! `JMSDeliveryMode` = header durable (2/1), `JMSPriority` = header priority,
//! `JMSRedelivered` = delivery-count > 0 or first-acquirer explicitly false, `JMSXDeliveryCount`, `JMSXUserID`, `JMSXGroupID`,
//! `JMSXGroupSeq`; application-properties are the other headers (keys through Camel's
//! default key format strategy `_DOT_` / `_HYPHEN_`); the message type is in the
//! `x-opt-jms-msg-type` annotation (text = amqp-value string, bytes = data section,
//! map = amqp-value map, stream = amqp-sequence) and the destination types in
//! `x-opt-jms-dest` / `x-opt-jms-reply-to`. Nodes are addressed with the `queue` /
//! `topic` terminus capabilities, and with the `queue-prefix` / `topic-prefix` a broker
//! announces in its open frame (ActiveMQ 5: `queue://` / `topic://`, see
//! [`AddressPrefixes`]); selectors use the `apache.org:selector-filter:string`
//! filter. With `includeAmqpAnnotations=true` message annotations become
//! `JMS_AMQP_MA_<key>` headers.
//!
//! Consumers settle after processing: `AUTO_ACKNOWLEDGE`/`DUPS_OK_ACKNOWLEDGE` accept,
//! `CLIENT_ACKNOWLEDGE` accepts successful exchanges and returns failed ones with
//! `modified(delivery-failed)` (redelivered with `JMSRedelivered=true`). `transacted=true`
//! (or `SESSION_TRANSACTED`) uses AMQP local transactions (a coordinator link per
//! connection): every message is acknowledged, and its reply sent, inside a declared
//! transaction that commits on success and rolls back on failure (the broker redelivers);
//! transacted producers send within a transaction too. A message
//! with a reply-to makes the exchange InOut and the reply goes to that address with the
//! request's correlation id (or message id); producers of InOut exchanges send with a
//! reply-to (a dynamic temporary queue, or `replyTo`) and wait `requestTimeout`.
//! `subscriptionShared=true` with `subscriptionName` uses JMS 2.0 shared subscriptions
//! (Qpid JMS link names and the `shared` / `global` capabilities; durable with
//! `subscriptionDurable=true`). `jmsMessageType=Object` sends ObjectMessages as AMQP typed
//! values (`x-opt-jms-msg-type = 1`), so structured bodies round-trip. A
//! [`AmqpConnectionFactory`] bean (`connectionFactory=#name`) supplies the remote URI,
//! credentials, client id and TLS settings; `amqpws://` / `amqpwss://` urls connect over
//! WebSockets.
//! Persistent sends wait for the broker's `accepted` outcome; non-persistent ones are

//! sent pre-settled.

use std::any::Any;
use std::collections::HashMap;
use std::sync::mpsc::{self, Sender as MpscSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cameleer_core::component::support::BackgroundThreads;
use cameleer_core::context::{CamelContext, WeakContext};
use cameleer_core::error::{kinds as core_kinds, CamelError, Result};
use cameleer_core::message::{Exchange, ExchangePattern};
use cameleer_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use cameleer_core::util::Parameters;
use cameleer_core::value::Value;

use crate::client::{Connection, ConnectionOptions, Coordinator, Delivery, LinkOptions, Receiver, Sender, Session};

use crate::codec::AmqpValue;
use crate::message::{id_to_string, Body, Message};
use crate::types::SELECTOR_FILTER;
use crate::types::DeliveryState;
use crate::util::{amqp_error, now_millis, parse_amqp_url, to_amqp, to_amqp_simple, to_camel, unique_id, Opts, COMMON_IGNORED};

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
    pub const JMSX_DELIVERY_COUNT: &str = "JMSXDeliveryCount";
    pub const JMSX_USER_ID: &str = "JMSXUserID";
    pub const JMSX_GROUP_ID: &str = "JMSXGroupID";
    pub const JMSX_GROUP_SEQ: &str = "JMSXGroupSeq";
    pub const JMS_DESTINATION_NAME: &str = "CamelJmsDestinationName";
    pub const JMS_REQUEST_TIMEOUT: &str = "CamelJmsRequestTimeout";
    pub const AMQP_ANNOTATION_PREFIX: &str = "JMS_AMQP_MA_";
    pub const EXCEPTION_KIND: &str = "CamelExceptionKind";
    pub const EXCEPTION_MESSAGE: &str = "CamelExceptionMessage";
}

/// JMS message type annotation values (`x-opt-jms-msg-type`).
pub mod msg_type {
    pub const MESSAGE: i8 = 0;
    pub const OBJECT: i8 = 1;
    pub const MAP: i8 = 2;
    pub const BYTES: i8 = 3;
    pub const STREAM: i8 = 4;
    pub const TEXT: i8 = 5;
}

/// Destination kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DestKind {
    Queue,
    Topic,
    TempQueue,
    TempTopic,
}

impl DestKind {
    fn capability(self) -> &'static str {
        match self {
            DestKind::Queue => "queue",
            DestKind::Topic => "topic",
            DestKind::TempQueue => "temporary-queue",
            DestKind::TempTopic => "temporary-topic",
        }
    }
    fn code(self) -> i8 {
        match self {
            DestKind::Queue => 0,
            DestKind::Topic => 1,
            DestKind::TempQueue => 2,
            DestKind::TempTopic => 3,
        }
    }
}

/// Parse `[queue:|topic:|temp-queue:|temp-topic:]name` (also `queue://name`).
pub fn parse_destination(s: &str) -> (DestKind, String) {
    let s = s.trim();
    for (p, k) in [("temp-queue:", DestKind::TempQueue), ("temp-topic:", DestKind::TempTopic), ("queue:", DestKind::Queue), ("topic:", DestKind::Topic)] {
        if let Some(rest) = s.strip_prefix(p) {
            // `queue://name` is the URL form; a single leading slash belongs to the address
            // (RabbitMQ: `/queue/name`, `/exchange/x/key`, `/topic/key`)
            return (k, rest.strip_prefix("//").unwrap_or(rest).to_string());
        }
    }
    (DestKind::Queue, s.to_string())
}

/// Node address prefixes a broker announces in its open frame (`queue-prefix` /
/// `topic-prefix` connection properties), applied as Qpid JMS does. ActiveMQ 5 announces
/// `queue://` / `topic://`: it ignores the `queue` / `topic` terminus capabilities and takes a
/// bare address for a queue, so without the prefix every topic consumer would be a competing
/// consumer of one queue.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AddressPrefixes {
    pub queue: Option<String>,
    pub topic: Option<String>,
}

impl AddressPrefixes {
    /// The prefixes announced by the peer of a connection.
    pub fn of(conn: &Connection) -> AddressPrefixes {
        let prop = |k: &str| remote_property(conn, k).filter(|v| !v.is_empty());
        AddressPrefixes { queue: prop("queue-prefix"), topic: prop("topic-prefix") }
    }

    fn prefix(&self, kind: DestKind) -> Option<&str> {
        match kind {
            DestKind::Queue => self.queue.as_deref(),
            DestKind::Topic => self.topic.as_deref(),
            _ => None,
        }
    }

    /// The node address of a queue or topic name (temporary destinations, and addresses
    /// that already carry a prefix, are left alone).
    pub fn apply(&self, name: &str, kind: DestKind) -> String {
        match self.prefix(kind) {
            Some(p) if !name.is_empty() && !self.is_prefixed(name) => format!("{}{}", p, name),
            _ => name.to_string(),
        }
    }

    fn is_prefixed(&self, address: &str) -> bool {
        [&self.queue, &self.topic].into_iter().flatten().any(|p| address.starts_with(p.as_str()))
    }

    /// The `JMSDestination` / `JMSReplyTo` header of a node address: a queue's name, or
    /// `topic:name` for an address with the topic prefix (so it can be sent to again).
    pub fn strip(&self, address: &str) -> String {
        if let Some(rest) = self.queue.as_deref().and_then(|p| address.strip_prefix(p)) {
            return rest.to_string();
        }
        if let Some(rest) = self.topic.as_deref().and_then(|p| address.strip_prefix(p)) {
            return format!("topic:{}", rest);
        }
        address.to_string()
    }
}

/// A string property of the peer's open frame.
fn remote_property(conn: &Connection, key: &str) -> Option<String> {
    conn.remote_open()?.properties.iter().find(|(k, _)| k.as_str() == Some(key)).and_then(|(_, v)| v.as_str().map(String::from))
}

/// Whether the peer is ActiveMQ 5 ("classic"; Artemis announces another product name).
fn is_activemq_classic(conn: &Connection) -> bool {
    remote_property(conn, "product").as_deref() == Some("ActiveMQ") && remote_property(conn, "version").is_some_and(|v| v.starts_with("5."))
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
    Map,
    /// ObjectMessage: the body is serialized as an AMQP typed value (Qpid JMS "AMQP typed
    /// encoding", `x-opt-jms-msg-type = 1`), so maps, lists, numbers, bytes... round-trip.
    Object,
}

/// A connection factory bean, the counterpart of Qpid JMS' `JmsConnectionFactory`: bind
/// one in the registry and reference it with `connectionFactory=#name` (or
/// `camel.component.amqp.connectionFactory=#name`). Endpoint options override its fields.
#[derive(Clone, Debug, Default)]
pub struct AmqpConnectionFactory {
    /// `amqp[s]://host:port`, `amqpws[s]://host:port/path` or `failover:(...)`.
    pub remote_uri: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub client_id: Option<String>,
    pub sasl_mechanism: Option<String>,
    pub idle_timeout: Option<Duration>,
    pub ssl_context_parameters: Option<Arc<cameleer_tls::params::SslContextParameters>>,
}

impl AmqpConnectionFactory {
    pub fn new(remote_uri: &str) -> Self {
        AmqpConnectionFactory { remote_uri: remote_uri.to_string(), ..Default::default() }
    }
    pub fn credentials(mut self, username: &str, password: &str) -> Self {
        self.username = Some(username.to_string());
        self.password = Some(password.to_string());
        self
    }
    pub fn client_id(mut self, id: &str) -> Self {
        self.client_id = Some(id.to_string());
        self
    }
    pub fn ssl(mut self, params: Arc<cameleer_tls::params::SslContextParameters>) -> Self {
        self.ssl_context_parameters = Some(params);
        self
    }
}

fn lookup_factory(ctx: &CamelContext, reference: &str) -> Result<AmqpConnectionFactory> {
    let name = reference.trim().trim_start_matches('#').trim_start_matches("bean:");
    if let Some(f) = ctx.registry().lookup::<AmqpConnectionFactory>(name) {
        return Ok((*f).clone());
    }
    if let Some(f) = ctx.registry().lookup::<Arc<AmqpConnectionFactory>>(name) {
        return Ok((**f).clone());
    }
    Err(CamelError::new(core_kinds::NO_SUCH_BEAN, format!("No AmqpConnectionFactory bean named {}", name)))
}

/// Resolved endpoint configuration.
#[derive(Clone, Debug)]
pub struct AmqpConfig {
    pub conn: ConnectionOptions,
    pub kind: DestKind,
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
    pub prefetch: u32,
    pub durable_subscription: Option<String>,
    /// JMS 2.0 shared subscription name (`subscriptionShared=true`).
    pub shared_subscription: Option<String>,
    pub include_amqp_annotations: bool,
    pub recovery_interval: Duration,
}

/// Camel's default `JmsKeyFormatStrategy`: `.` -> `_DOT_`, `-` -> `_HYPHEN_`.
pub fn encode_key(k: &str) -> String {
    k.replace('.', "_DOT_").replace('-', "_HYPHEN_")
}

/// Inverse of [`encode_key`].
pub fn decode_key(k: &str) -> String {
    k.replace("_DOT_", ".").replace("_HYPHEN_", "-")
}

/// Resolve the connection settings of an `amqp` endpoint.
fn connection_options(ctx: &CamelContext, scheme: &str, o: &mut Opts) -> Result<ConnectionOptions> {
    let factory = match o.string("connectionFactory") {
        Some(r) => Some(lookup_factory(ctx, &r)?),
        None => None,
    };
    let url = o
        .string("remoteAddress")
        .or_else(|| o.string("remoteURI"))
        .or_else(|| o.string("brokerURL"))
        .or_else(|| o.string("brokerUrl"))
        .or_else(|| o.string("connectionFactory.remoteURI"))
        .or_else(|| factory.as_ref().map(|f| f.remote_uri.clone()).filter(|u| !u.is_empty()));
    let host = o.string("host");
    let port: Option<u16> = match o.get("port") {
        Some(p) => Some(p.trim().parse().map_err(|_| CamelError::illegal_argument(format!("Invalid port: {}", p)))?),
        None => None,
    };
    let use_ssl = o.bool("useSsl", false)?;
    let u = match url {
        Some(u) => parse_amqp_url(&u)?,
        None => {
            let tls = use_ssl;
            crate::util::AmqpUrl {
                tls,
                host: host.clone().unwrap_or_else(|| "localhost".into()),
                port: port.unwrap_or(if tls { 5671 } else { 5672 }),
                username: None,
                password: None,
                websocket: false,
                ws_path: "/".into(),
            }
        }
    };
    let tls = cameleer_tls::camel_support::endpoint_tls(ctx, o.params, scheme, u.tls || use_ssl, &[])?;
    let tls = match tls {
        Some(t) => Some(t),
        None if factory.as_ref().is_some_and(|f| f.ssl_context_parameters.is_some()) => Some(cameleer_tls::camel_support::EndpointTls::new(factory.as_ref().unwrap().ssl_context_parameters.clone().unwrap())),
        None if ctx.properties().get(&format!("camel.component.{}.sslContextParameters", scheme)).is_some() => {
            let r = ctx.properties().get(&format!("camel.component.{}.sslContextParameters", scheme)).unwrap();
            let p = cameleer_tls::camel_support::lookup_ssl_context_parameters(ctx, r.trim_start_matches('#'))?;
            Some(cameleer_tls::camel_support::EndpointTls::new(p))
        }
        None => None,
    };
    let mut c = ConnectionOptions::new(&u.host, u.port);
    c.tls = tls;
    let f = factory.unwrap_or_default();
    c.username = o.string("username").or(f.username).or(u.username);
    c.password = o.string("password").or(f.password).or(u.password);
    c.sasl_mechanism = o.string("saslMechanism").or(f.sasl_mechanism);
    c.container_id = o.string("clientId").or(f.client_id);
    c.idle_timeout = o.millis("idleTimeout", f.idle_timeout.unwrap_or(Duration::from_secs(60)))?;
    if u.websocket {
        c.websocket_path = Some(u.ws_path.clone());
    }
    c.connect_timeout = o.millis("connectTimeout", Duration::from_secs(10))?;
    c.max_frame_size = o.num("maxFrameSize", 1u32 << 20)?;
    c.hostname = o.string("vhost");
    Ok(c)
}

impl AmqpConfig {
    fn from_uri(scheme: &str, ctx: &CamelContext, remaining: &str, params: &mut Parameters) -> Result<AmqpConfig> {
        let mut o = Opts::new(ctx, scheme, params);
        let conn = connection_options(ctx, scheme, &mut o)?;
        let (kind, destination) = parse_destination(remaining);
        if destination.is_empty() {
            return Err(CamelError::illegal_argument(format!("Destination name must be specified: {}", remaining)));
        }
        let concurrent_consumers = o.num("concurrentConsumers", 1usize)?.max(1);
        o.get("maxConcurrentConsumers");
        let reply_to = o.string("replyTo").map(|s| parse_destination(&s).1);
        let request_timeout = o.millis("requestTimeout", Duration::from_millis(20_000))?;
        let selector = o.string("selector");
        if let Some(s) = &selector {
            cameleer_amqp::selector::Selector::parse(s)?;
        }
        let mut delivery_persistent = o.bool("deliveryPersistent", true)?;
        match o.get("deliveryMode").as_deref().map(str::trim) {
            None => {}
            Some("2") | Some("PERSISTENT") => delivery_persistent = true,
            Some("1") | Some("NON_PERSISTENT") => delivery_persistent = false,
            Some(other) => return Err(CamelError::illegal_argument(format!("Invalid deliveryMode: {}", other))),
        }
        let time_to_live = o.num("timeToLive", -1i64)?;
        let priority_v = o.num("priority", -1i32)?;
        if priority_v > 9 {
            return Err(CamelError::illegal_argument(format!("Invalid priority (0-9): {}", priority_v)));
        }
        o.get("explicitQosEnabled");
        let preserve_message_qos = o.bool("preserveMessageQos", false)?;
        let disable_reply_to = o.bool("disableReplyTo", false)?;
        let mut transacted = o.bool("transacted", false)?;
        let ack = match o.get("acknowledgementModeName").as_deref() {
            None | Some("AUTO_ACKNOWLEDGE") => JmsAck::Auto,
            Some("CLIENT_ACKNOWLEDGE") => JmsAck::Client,
            Some("DUPS_OK_ACKNOWLEDGE") => JmsAck::DupsOk,
            Some("SESSION_TRANSACTED") => {
                transacted = true;
                JmsAck::Auto
            }
            Some(other) => return Err(CamelError::illegal_argument(format!("Invalid acknowledgementModeName: {}", other))),
        };
        let message_type = match o.get("jmsMessageType").as_deref() {
            None => None,
            Some("Text") => Some(MessageType::Text),
            Some("Bytes") => Some(MessageType::Bytes),
            Some("Map") => Some(MessageType::Map),
            Some("Object") => Some(MessageType::Object),
            Some(other) => return Err(CamelError::illegal_argument(format!("Unsupported jmsMessageType (Text, Bytes, Map or Object): {}", other))),
        };
        let prefetch = o.num("prefetchSize", 100u32)?.max(1);
        let durable_name = o.string("durableSubscriptionName");
        let sub_name = o.string("subscriptionName");
        let shared = o.bool("subscriptionShared", false)?;
        let durable = o.bool("subscriptionDurable", false)? || durable_name.is_some() || (sub_name.is_some() && !shared);
        let name = durable_name.or(sub_name);
        let durable_subscription = if durable && kind == DestKind::Topic { Some(name.clone().unwrap_or_else(|| destination.clone())) } else { None };
        let shared_subscription = if shared && kind == DestKind::Topic { Some(name.unwrap_or_else(|| destination.clone())) } else { None };
        let include_amqp_annotations = o.bool("includeAmqpAnnotations", false)?;
        let recovery_interval = o.millis("recoveryInterval", Duration::from_millis(5000))?;
        o.ignore(COMMON_IGNORED);
        o.ignore(&[
            "replyToType",
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
            "useMessageIDAsCorrelationID",
            "replyToDeliveryPersistent",
            "includeAllJMSXProperties",
            "artemisStreamingEnabled",
            "acknowledgementMode",
        ]);
        Ok(AmqpConfig {
            conn,
            kind,
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
            durable_subscription,
            shared_subscription,
            include_amqp_annotations,
            recovery_interval,
        })
    }
}

/// The `amqp` component (JMS over AMQP 1.0).
pub struct AmqpComponent {
    scheme: &'static str,
}

impl AmqpComponent {
    pub fn new(scheme: &'static str) -> Self {
        AmqpComponent { scheme }
    }
}

impl Default for AmqpComponent {
    fn default() -> Self {
        AmqpComponent::new("amqp")
    }
}

impl Component for AmqpComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let cfg = AmqpConfig::from_uri(self.scheme, ctx, remaining, params)?;
        Ok(Arc::new(AmqpEndpoint { uri: uri.to_string(), ctx: ctx.downgrade(), cfg: Arc::new(cfg) }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// An `amqp` endpoint.
pub struct AmqpEndpoint {
    uri: String,
    ctx: WeakContext,
    cfg: Arc<AmqpConfig>,
}

impl AmqpEndpoint {
    pub fn config(&self) -> &AmqpConfig {
        &self.cfg
    }
}

impl Endpoint for AmqpEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(AmqpProducer { cfg: self.cfg.clone(), uri: self.uri.clone(), state: Mutex::new(None) }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        Ok(Arc::new(AmqpConsumer { cfg: self.cfg.clone(), uri: self.uri.clone(), ctx: self.ctx.clone(), processor, threads: BackgroundThreads::new() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---------------------------------------------------------------------------
// message mapping
// ---------------------------------------------------------------------------

fn header_str(ex: &Exchange, k: &str) -> Option<String> {
    ex.header(k).filter(|v| !v.is_null()).map(|v| v.to_string()).filter(|s| !s.is_empty())
}

/// Map an AMQP message to the exchange (AMQP JMS mapping).
pub fn message_to_exchange(cfg: &AmqpConfig, prefixes: &AddressPrefixes, m: &Message, ex: &mut Exchange) {
    use headers::*;
    let p = m.props();
    let text_ct = p.content_type.as_deref().is_some_and(|c| c.starts_with("text/") || c.contains("json") || c.contains("xml"));
    let mut body = match &m.body {
        Body::Empty => Value::Null,
        Body::Value(v @ AmqpValue::Binary(_)) => to_camel(v),
        Body::Value(v) => to_camel(v),
        Body::Data(d) => {
            let b = d.concat();
            if text_ct {
                String::from_utf8(b).map(Value::String).unwrap_or_else(|e| Value::Bytes(e.into_bytes()))
            } else {
                Value::Bytes(b)
            }
        }
        Body::Sequence(s) => Value::Array(s.iter().flatten().map(to_camel).collect()),
    };
    body = match (cfg.message_type, body) {
        (Some(MessageType::Text), Value::Bytes(b)) => String::from_utf8(b).map(Value::String).unwrap_or_else(|e| Value::Bytes(e.into_bytes())),
        (Some(MessageType::Bytes), Value::String(s)) => Value::Bytes(s.into_bytes()),
        (_, b) => b,
    };
    ex.set_body(body);
    if let Some(v) = &p.message_id {
        ex.set_header(JMS_MESSAGE_ID, id_to_string(v));
    }
    if let Some(v) = &p.correlation_id {
        ex.set_header(JMS_CORRELATION_ID, id_to_string(v));
    }
    ex.set_header(JMS_DESTINATION, p.to.as_deref().map(|t| prefixes.strip(t)).unwrap_or_else(|| cfg.destination.clone()));
    if let Some(r) = &p.reply_to {
        ex.set_header(JMS_REPLY_TO, prefixes.strip(r));
    }
    let h = m.header.clone().unwrap_or_default();
    ex.set_header(JMS_DELIVERY_MODE, if h.durable { 2i64 } else { 1i64 });
    ex.set_header(JMS_PRIORITY, h.priority as i64);
    // RabbitMQ signals a redelivery with `first-acquirer = false` (delivery-count stays 0)
    ex.set_header(JMS_REDELIVERED, h.delivery_count > 0 || h.acquired_before);
    ex.set_header(JMSX_DELIVERY_COUNT, h.delivery_count as i64 + 1);
    if let Some(t) = p.creation_time {
        ex.set_header(JMS_TIMESTAMP, t);
    }
    ex.set_header(JMS_EXPIRATION, p.absolute_expiry_time.unwrap_or(0));
    if let Some(t) = &p.subject {
        ex.set_header(JMS_TYPE, t.clone());
    }
    if let Some(u) = &p.user_id {
        ex.set_header(JMSX_USER_ID, String::from_utf8_lossy(u).to_string());
    }
    if let Some(g) = &p.group_id {
        ex.set_header(JMSX_GROUP_ID, g.clone());
    }
    if let Some(g) = p.group_sequence {
        ex.set_header(JMSX_GROUP_SEQ, g as i64);
    }
    for (k, v) in &m.application_properties {
        ex.set_header(decode_key(k), to_camel(v));
    }
    if cfg.include_amqp_annotations {
        for (k, v) in &m.message_annotations {
            if let Some(k) = k.as_str() {
                ex.set_header(format!("{}{}", AMQP_ANNOTATION_PREFIX, k), to_camel(v));
            }
        }
    }
}

/// Build the AMQP message of an exchange (AMQP JMS mapping).
pub fn exchange_to_message(cfg: &AmqpConfig, prefixes: &AddressPrefixes, ex: &Exchange, to: &str, kind: DestKind, include_reply_to: bool) -> Message {
    use headers::*;
    let mut m = Message::default();
    let (body, ty) = match (cfg.message_type, ex.body()) {
        (Some(MessageType::Bytes), v) => (Body::Data(vec![value_bytes(v)]), msg_type::BYTES),
        (Some(MessageType::Text), v) => (Body::Value(AmqpValue::String(v.to_string())), msg_type::TEXT),
        (Some(MessageType::Object), v) => (Body::Value(to_amqp(v)), msg_type::OBJECT),
        (Some(MessageType::Map), Value::Object(_)) | (None, Value::Object(_)) => (Body::Value(to_amqp(ex.body())), msg_type::MAP),
        (_, Value::Null) => (Body::Value(AmqpValue::Null), msg_type::MESSAGE),
        (_, Value::Bytes(b)) => (Body::Data(vec![b.clone()]), msg_type::BYTES),
        (_, Value::Array(a)) => (Body::Sequence(vec![a.iter().map(to_amqp).collect()]), msg_type::STREAM),
        (_, v) => (Body::Value(AmqpValue::String(v.to_string())), msg_type::TEXT),
    };
    m.body = body;
    m.set_annotation("x-opt-jms-msg-type", AmqpValue::Byte(ty));
    m.set_annotation("x-opt-jms-dest", AmqpValue::Byte(kind.code()));
    if ty == msg_type::BYTES {
        m.props_mut().content_type = Some("application/octet-stream".into());
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
    {
        let h = m.header_mut();
        h.durable = persistent;
        h.priority = priority.unwrap_or(4);
        if expires > 0 {
            h.ttl = Some((expires - now_millis()).max(1) as u32);
        }
    }
    let p = m.props_mut();
    p.message_id = Some(AmqpValue::String(format!("ID:{}", unique_id())));
    p.to = Some(prefixes.apply(to, kind));
    p.creation_time = Some(now_millis());
    if expires > 0 {
        p.absolute_expiry_time = Some(expires);
    }
    p.correlation_id = header_str(ex, JMS_CORRELATION_ID).map(AmqpValue::String);
    p.subject = header_str(ex, JMS_TYPE);
    p.group_id = header_str(ex, JMSX_GROUP_ID);
    if include_reply_to {
        if let Some(r) = header_str(ex, JMS_REPLY_TO) {
            let (k, name) = parse_destination(&r);
            p.reply_to = Some(prefixes.apply(&name, k));
            m.set_annotation("x-opt-jms-reply-to", AmqpValue::Byte(k.code()));
        }
    }
    for (k, v) in ex.message.headers.iter() {
        let lower = k.to_ascii_lowercase();
        if lower.starts_with("camel") || lower.starts_with("jms") || v.is_null() {
            continue;
        }
        m.set_app_property(&encode_key(k), to_amqp_simple(v));
    }
    m
}

fn value_bytes(v: &Value) -> Vec<u8> {
    match v {
        Value::Bytes(b) => b.clone(),
        Value::Null => Vec::new(),
        Value::Object(_) | Value::Array(_) => v.to_json().into_bytes(),
        other => other.to_string().into_bytes(),
    }
}

// ---------------------------------------------------------------------------
// producer
// ---------------------------------------------------------------------------

type Pending = Arc<Mutex<HashMap<String, MpscSender<Message>>>>;

struct ReplyRouter {
    address: String,
    pending: Pending,
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

struct ProducerState {
    conn: Connection,
    session: Session,
    prefixes: AddressPrefixes,
    senders: HashMap<String, Arc<Sender>>,
    reply: Option<ReplyRouter>,
    coordinator: Option<Arc<Coordinator>>,
}

impl ProducerState {
    fn shutdown(mut self) {
        if let Some(mut r) = self.reply.take() {
            r.stop.store(true, std::sync::atomic::Ordering::SeqCst);
            if let Some(t) = r.thread.take() {
                let _ = t.join();
            }
        }
        self.conn.close();
    }
}

struct AmqpProducer {
    cfg: Arc<AmqpConfig>,
    uri: String,
    state: Mutex<Option<ProducerState>>,
}

fn capability_opts(address: &str, kind: DestKind) -> LinkOptions {
    LinkOptions::new(address).capability(kind.capability())
}

impl AmqpProducer {
    fn state<'a>(&self, g: &'a mut Option<ProducerState>) -> Result<&'a mut ProducerState> {
        if g.as_ref().is_some_and(|s| !s.conn.is_open()) {
            if let Some(old) = g.take() {
                old.shutdown();
            }
        }
        if g.is_none() {
            let conn = Connection::open(&self.cfg.conn)?;
            let session = conn.session()?;
            let prefixes = AddressPrefixes::of(&conn);
            *g = Some(ProducerState { conn, session, prefixes, senders: HashMap::new(), reply: None, coordinator: None });
        }
        Ok(g.as_mut().unwrap())
    }

    fn sender(s: &mut ProducerState, address: &str, kind: DestKind) -> Result<Arc<Sender>> {
        if let Some(x) = s.senders.get(address) {
            return Ok(x.clone());
        }
        let snd = Arc::new(s.session.sender(capability_opts(&s.prefixes.apply(address, kind), kind))?);
        s.senders.insert(address.to_string(), snd.clone());
        Ok(snd)
    }

    fn reply_router(&self, s: &mut ProducerState) -> Result<(String, Pending)> {
        if s.reply.is_none() {
            let receiver: Receiver = match &self.cfg.reply_to {
                Some(q) => s.session.receiver(capability_opts(&s.prefixes.apply(q, DestKind::Queue), DestKind::Queue).credit(self.cfg.prefetch))?,
                None => s.session.receiver(LinkOptions::dynamic().capability("temporary-queue").credit(self.cfg.prefetch))?,
            };
            let address = receiver.address().map(String::from).or_else(|| self.cfg.reply_to.as_deref().map(|q| s.prefixes.apply(q, DestKind::Queue))).unwrap_or_default();
            let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (p2, s2) = (pending.clone(), stop.clone());
            let thread = std::thread::Builder::new()
                .name(format!("Camel {} reply router", self.uri))
                .spawn(move || {
                    while !s2.load(std::sync::atomic::Ordering::SeqCst) {
                        match receiver.receive(Duration::from_millis(100)) {
                            Ok(Some(d)) => {
                                let _ = receiver.accept(&d);
                                let id = d.message.properties.as_ref().and_then(|p| p.correlation_id.as_ref()).map(id_to_string).unwrap_or_default();
                                if let Some(tx) = p2.lock().unwrap().remove(&id) {
                                    let _ = tx.send(d.message);
                                }
                            }
                            Ok(None) => {}
                            Err(_) => break,
                        }
                    }
                    receiver.close();
                })
                .map_err(|e| CamelError::io(e.to_string()))?;
            s.reply = Some(ReplyRouter { address, pending, stop, thread: Some(thread) });
        }
        let r = s.reply.as_ref().unwrap();
        Ok((r.address.clone(), r.pending.clone()))
    }

    fn send(&self, exchange: &mut Exchange) -> Result<()> {
        use headers::*;
        let cfg = &self.cfg;
        let (kind, address) = match header_str(exchange, JMS_DESTINATION_NAME) {
            Some(d) => parse_destination(&d),
            None => (cfg.kind, cfg.destination.clone()),
        };
        let in_out = exchange.is_out_capable() && !cfg.disable_reply_to;
        let mut g = self.state.lock().unwrap();
        let mut prefixes = AddressPrefixes::default();
        let result = (|| {
            let s = self.state(&mut g)?;
            prefixes = s.prefixes.clone();
            let snd = Self::sender(s, &address, kind)?;
            if cfg.transacted && s.coordinator.is_none() {
                s.coordinator = Some(Arc::new(s.session.coordinator()?));
            }
            let coord = s.coordinator.clone();
            if !in_out {
                return Ok((snd, coord, None));
            }
            let (reply_addr, pending) = self.reply_router(s)?;
            Ok((snd, coord, Some((reply_addr, pending))))
        })();
        let (snd, coord, reply) = match result {
            Ok(x) => x,
            Err(e) => {
                if let Some(old) = g.take() {
                    old.shutdown();
                }
                return Err(e);
            }
        };
        drop(g);
        let mut msg = exchange_to_message(cfg, &prefixes, exchange, &address, kind, !in_out);
        let send = |m: &Message| -> Result<()> {
            if let Some(c) = &coord {
                // a local AMQP transaction around the send
                let id = c.declare()?;
                if let Err(e) = snd.send_accepted_in_txn(m, &id, cfg.request_timeout) {
                    let _ = c.rollback(&id);
                    return Err(e);
                }
                return c.commit(&id);
            }
            if cfg.delivery_persistent {
                match snd.send_with(m, false, cfg.request_timeout)? {
                    None | Some(DeliveryState::Accepted) => Ok(()),
                    Some(DeliveryState::Rejected(Some(e))) => Err(amqp_error(&e, &format!("Message rejected by {}", address))),
                    Some(other) => Err(crate::util::amqp_failure(format!("Message not accepted by {}: {:?}", address, other))),
                }
            } else {
                snd.send_with(m, true, cfg.request_timeout).map(|_| ())
            }
        };
        let Some((reply_addr, pending)) = reply else {
            return send(&msg);
        };
        let timeout = exchange.header(JMS_REQUEST_TIMEOUT).and_then(|v| v.as_i64()).map(|ms| Duration::from_millis(ms.max(1) as u64)).unwrap_or(cfg.request_timeout);
        let correlation = header_str(exchange, JMS_CORRELATION_ID).unwrap_or_else(unique_id);
        {
            let p = msg.props_mut();
            p.correlation_id = Some(AmqpValue::String(correlation.clone()));
            p.reply_to = Some(reply_addr.clone());
        }
        msg.set_annotation("x-opt-jms-reply-to", AmqpValue::Byte(if cfg.reply_to.is_some() { 0 } else { 2 }));
        let (tx, rx) = mpsc::channel();
        pending.lock().unwrap().insert(correlation.clone(), tx);
        if let Err(e) = send(&msg) {
            pending.lock().unwrap().remove(&correlation);
            return Err(e);
        }
        let reply = match rx.recv_timeout(timeout) {
            Ok(r) => r,
            Err(_) => {
                pending.lock().unwrap().remove(&correlation);
                return Err(CamelError::new(
                    core_kinds::EXCHANGE_TIMED_OUT,
                    format!(
                        "Exchange timed out after waiting: {} millis. Exchange[{}] (no reply received on {} with JMSCorrelationID {})",
                        timeout.as_millis(),
                        exchange.exchange_id,
                        address,
                        correlation
                    ),
                ));
            }
        };
        if let Some(kind) = reply.app_property(EXCEPTION_KIND).and_then(|v| v.as_str()) {
            return Err(CamelError::new(kind, reply.app_property(EXCEPTION_MESSAGE).and_then(|v| v.as_str()).unwrap_or("")));
        }
        exchange.message.headers.clear();
        message_to_exchange(cfg, &prefixes, &reply, exchange);
        Ok(())
    }
}

impl Service for AmqpProducer {
    fn stop(&self) -> Result<()> {
        if let Some(s) = self.state.lock().unwrap().take() {
            s.shutdown();
        }
        Ok(())
    }
}

impl Processor for AmqpProducer {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        self.send(exchange)
    }
}

// ---------------------------------------------------------------------------
// consumer
// ---------------------------------------------------------------------------

struct AmqpConsumer {
    cfg: Arc<AmqpConfig>,
    uri: String,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
    threads: BackgroundThreads,
}

struct Worker {
    cfg: Arc<AmqpConfig>,
    uri: String,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
}

/// The consumer's reply senders: one anonymous-relay link when the broker offers
/// `ANONYMOUS-RELAY` (Artemis, Qpid, the embedded broker), else a link per reply address
/// (RabbitMQ 3.x refuses attaches without a target address).
struct ReplySenders {
    session: Session,
    prefixes: AddressPrefixes,
    anonymous: Option<Sender>,
    by_address: Mutex<HashMap<String, Sender>>,
}

impl ReplySenders {
    fn open(conn: &Connection, session: &Session) -> Result<ReplySenders> {
        let relay = conn.remote_open().is_some_and(|o| o.offered_capabilities.iter().any(|c| c == "ANONYMOUS-RELAY"));
        let anonymous = if relay {
            let mut anon = LinkOptions::new("");
            anon.address = None;
            Some(session.sender(anon)?)
        } else {
            None
        };
        Ok(ReplySenders { session: session.clone(), prefixes: AddressPrefixes::of(conn), anonymous, by_address: Mutex::new(HashMap::new()) })
    }

    fn send(&self, m: &Message, address: &str, kind: DestKind, txn: Option<&Vec<u8>>, timeout: Duration) -> Result<()> {
        let go = |s: &Sender| match txn {
            Some(id) => s.send_accepted_in_txn(m, id, timeout),
            None => s.send_with(m, false, timeout).map(|_| ()),
        };
        if let Some(a) = &self.anonymous {
            return go(a);
        }
        let mut map = self.by_address.lock().unwrap();
        if !map.contains_key(address) {
            let s = self.session.sender(capability_opts(&self.prefixes.apply(address, kind), kind))?;
            map.insert(address.to_string(), s);
        }
        let r = go(&map[address]);
        if r.is_err() {
            map.remove(address);
        }
        r
    }
}

impl Worker {
    fn handle(&self, rx: &Receiver, replies: &ReplySenders, coord: Option<&Coordinator>, d: Delivery) {
        let cfg = &self.cfg;
        let txn = match coord {
            Some(c) => match c.declare() {
                Ok(id) => Some(id),
                Err(e) => {
                    cameleer_core::camel_warn!(&self.uri, "Cannot begin a transaction: {} - releasing the message", e.message);
                    let _ = rx.release(&d);
                    return;
                }
            },
            None => None,
        };
        let mut ex = Exchange::new(self.ctx.upgrade());
        message_to_exchange(cfg, &replies.prefixes, &d.message, &mut ex);
        let reply_to = if cfg.disable_reply_to { None } else { d.message.properties.as_ref().and_then(|p| p.reply_to.clone()) };
        if reply_to.is_some() {
            ex.pattern = ExchangePattern::InOut;
        }
        if let Err(e) = self.processor.process(&mut ex) {
            ex.set_exception(e);
        }
        let failed = ex.exception.is_some();
        if let Some(e) = &ex.exception {
            cameleer_core::camel_warn!(&self.uri, "Error processing exchange. Exchange[{}]. Caused by: [{} - {}]", ex.exchange_id, e.kind, e.message);
        }
        if let Some(rt) = reply_to {
            let mut out = ex.clone();
            out.exception = None;
            let reply_cfg = AmqpConfig { message_type: None, priority: None, time_to_live: -1, preserve_message_qos: false, ..(**cfg).clone() };
            let kind = match d.message.annotation("x-opt-jms-reply-to").and_then(|v| v.as_i64()) {
                Some(1) => DestKind::Topic,
                Some(2) => DestKind::TempQueue,
                Some(3) => DestKind::TempTopic,
                _ => DestKind::Queue,
            };
            let mut m = exchange_to_message(&reply_cfg, &replies.prefixes, &out, &rt, kind, false);
            let p = d.message.props();
            m.props_mut().correlation_id = p.correlation_id.clone().or(p.message_id.clone());
            if let Some(e) = &ex.exception {
                m.set_app_property(headers::EXCEPTION_KIND, AmqpValue::str(e.kind.clone()));
                m.set_app_property(headers::EXCEPTION_MESSAGE, AmqpValue::str(e.message.clone()));
            }
            let sent = replies.send(&m, &rt, kind, txn.as_ref(), cfg.request_timeout);
            if let Err(e) = sent {
                cameleer_core::camel_warn!(&self.uri, "Cannot send reply to {}: {}", rt, e.message);
            }
        }
        if let (Some(c), Some(id)) = (coord, &txn) {
            // the acknowledgement (and any reply) commit together; a failure rolls back so
            // the broker redelivers the message (JMSRedelivered=true)
            let r = rx.settle_in_txn(&d, id, DeliveryState::Accepted).and_then(|_| c.discharge(id, failed));
            if let Err(e) = r {
                cameleer_core::camel_warn!(&self.uri, "Cannot complete the transaction: {}", e.message);
            }
            return;
        }
        let r = if failed && cfg.ack == JmsAck::Client { rx.modify(&d, true, false, vec![]) } else { rx.accept(&d) };
        if let Err(e) = r {
            cameleer_core::camel_warn!(&self.uri, "Cannot settle message: {}", e.message);
        }
    }
}

fn consumer_link(cfg: &AmqpConfig, prefixes: &AddressPrefixes, index: usize) -> LinkOptions {
    let mut o = capability_opts(&prefixes.apply(&cfg.destination, cfg.kind), cfg.kind).credit(cfg.prefetch);
    if let Some(s) = &cfg.selector {
        o = o.filter("jms-selector", SELECTOR_FILTER, AmqpValue::str(s.clone()));
    }
    if let Some(sub) = &cfg.shared_subscription {
        // Qpid JMS link naming: `name`, `name|2`... (durable), `name|volatileN` (non-durable),
        // `|global` variants without a client id
        let global = cfg.conn.container_id.is_none();
        let durable = cfg.durable_subscription.is_some();
        let name = match (durable, global) {
            (true, false) if index == 0 => sub.clone(),
            (true, false) => format!("{}|{}", sub, index + 1),
            (true, true) if index == 0 => format!("{}|global", sub),
            (true, true) => format!("{}|global{}", sub, index + 1),
            (false, false) => format!("{}|volatile{}", sub, index + 1),
            (false, true) => format!("{}|global-volatile{}", sub, index + 1),
        };
        o = o.name(&name).capability("shared");
        if global {
            o = o.capability("global");
        }
        if durable {
            o.durable = 2;
            o.expiry_policy = Some("never".into());
        }
    } else if let Some(sub) = &cfg.durable_subscription {
        o = o.name(&if index == 0 { sub.clone() } else { format!("{}-{}", sub, index) });
        o.durable = 2;
        o.expiry_policy = Some("never".into());
    }
    o
}

impl Service for AmqpConsumer {
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
                let setup = (|| -> Result<(Connection, Session, Vec<Receiver>, ReplySenders, Option<Coordinator>)> {
                    let conn = Connection::open(&worker.cfg.conn)?;
                    if worker.cfg.shared_subscription.is_some() && is_activemq_classic(&conn) {
                        // ActiveMQ 5 has no JMS 2.0 shared subscriptions (its JMS client refuses
                        // shared consumers too); the `shared` capability would be ignored
                        let version = remote_property(&conn, "version").unwrap_or_default();
                        return Err(CamelError::new(
                            core_kinds::UNSUPPORTED_OPERATION,
                            format!("ActiveMQ {} has no JMS 2.0 shared subscriptions (subscriptionShared); use a virtual topic: publish to topic VirtualTopic.<name> and consume queue Consumer.<subscription>.VirtualTopic.<name>", version),
                        ));
                    }
                    let session = conn.session()?;
                    let prefixes = AddressPrefixes::of(&conn);
                    let mut rxs = Vec::new();
                    for i in 0..worker.cfg.concurrent_consumers {
                        rxs.push(session.receiver(consumer_link(&worker.cfg, &prefixes, i))?);
                    }
                    let replies = ReplySenders::open(&conn, &session)?;
                    let coord = if worker.cfg.transacted { Some(session.coordinator()?) } else { None };
                    Ok((conn, session, rxs, replies, coord))
                })();
                let (conn, _session, rxs, replies, coord) = match setup {
                    Ok(x) => x,
                    Err(e) if e.kind == core_kinds::UNSUPPORTED_OPERATION => {
                        // a configuration the broker cannot honour: no point in retrying
                        cameleer_core::camel_warn!(&uri, "Cannot start consumer on {}: {}", uri, e.message);
                        if let Some(tx) = ready.take() {
                            let _ = tx.send(Err(e));
                        }
                        break;
                    }
                    Err(e) => {
                        cameleer_core::camel_warn!(&uri, "Cannot start consumer on {}: {} - retrying in {} ms", uri, e.message, worker.cfg.recovery_interval.as_millis());
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
                std::thread::scope(|scope| {
                    for rx in &rxs {
                        let (w, st, replies, coord) = (worker.clone(), stop.clone(), &replies, coord.as_ref());
                        scope.spawn(move || loop {
                            if st.is_stopped() {
                                break;
                            }
                            match rx.receive(Duration::from_millis(50)) {
                                Ok(Some(d)) => w.handle(rx, replies, coord, d),
                                Ok(None) => {}
                                Err(_) => break,
                            }
                        });
                    }
                });
                let lost = !stop.is_stopped();
                if !lost {
                    if let Some(c) = &coord {
                        c.close();
                    }
                    for rx in &rxs {
                        // durable subscriptions survive: detach without closing them
                        if worker.cfg.durable_subscription.is_some() {
                            rx.detach();
                        } else {
                            rx.close();
                        }
                    }
                }
                conn.close();
                if lost {
                    cameleer_core::camel_warn!(&uri, "Connection lost on {} - reconnecting in {} ms", uri, worker.cfg.recovery_interval.as_millis());
                    if stop.sleep(worker.cfg.recovery_interval) {
                        break;
                    }
                }
            }
        })?;
        if let Ok(Err(e)) = ready_rx.recv_timeout(self.cfg.conn.connect_timeout + Duration::from_secs(5)) {
            if e.kind == core_kinds::UNSUPPORTED_OPERATION {
                self.threads.stop();
                return Err(e);
            }
            cameleer_core::camel_warn!(&self.uri, "Consumer {} not connected yet: {}", self.uri, e.message);
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
    fn destinations_and_keys() {
        assert_eq!(parse_destination("queue:a"), (DestKind::Queue, "a".into()));
        assert_eq!(parse_destination("topic:t"), (DestKind::Topic, "t".into()));
        assert_eq!(parse_destination("temp-queue:x"), (DestKind::TempQueue, "x".into()));
        assert_eq!(parse_destination("plain"), (DestKind::Queue, "plain".into()));
        assert_eq!(parse_destination("queue://a"), (DestKind::Queue, "a".into()));
        // RabbitMQ AMQP 1.0 addresses keep their leading slash (found against a real broker)
        assert_eq!(parse_destination("queue:/queue/a"), (DestKind::Queue, "/queue/a".into()));
        assert_eq!(parse_destination("topic:/exchange/amq.topic/k"), (DestKind::Topic, "/exchange/amq.topic/k".into()));
        assert_eq!(encode_key("a.b-c"), "a_DOT_b_HYPHEN_c");
        assert_eq!(decode_key("a_DOT_b_HYPHEN_c"), "a.b-c");
    }

    #[test]
    fn broker_address_prefixes() {
        // ActiveMQ 5 announces `queue-prefix` / `topic-prefix` (found against 5.17.6)
        let p = AddressPrefixes { queue: Some("queue://".into()), topic: Some("topic://".into()) };
        assert_eq!(p.apply("a", DestKind::Queue), "queue://a");
        assert_eq!(p.apply("t", DestKind::Topic), "topic://t");
        assert_eq!(p.apply("queue://a", DestKind::Queue), "queue://a");
        assert_eq!(p.apply("temp-queue://ID:x", DestKind::TempQueue), "temp-queue://ID:x");
        assert_eq!(p.strip("queue://a"), "a");
        assert_eq!(p.strip("topic://t"), "topic:t");
        assert_eq!(p.strip("temp-queue://ID:x"), "temp-queue://ID:x");
        let none = AddressPrefixes::default();
        assert_eq!(none.apply("a", DestKind::Topic), "a");
        assert_eq!(none.strip("queue://a"), "queue://a");
    }
}

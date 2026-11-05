//! The `azure-servicebus` component (Camel `camel-azure-servicebus`) over AMQP 1.0 with
//! SAS tokens put on the `$cbs` node.
//!
//! ```text
//! azure-servicebus:queueOrTopicName?connectionString=Endpoint=sb://ns.servicebus.windows.net/;SharedAccessKeyName=..;SharedAccessKey=..
//!     &serviceBusType=queue|topic&subscriptionName=&receiveMode=PEEK_LOCK|RECEIVE_AND_DELETE
//!     &consumerOperation=receiveMessages|peekMessages&producerOperation=sendMessages|scheduleMessages
//!     &prefetchCount=0&maxConcurrentCalls=1&peekNumMaxMessages=&scheduledEnqueueTime=
//!     &subQueue=NONE|DEAD_LETTER_QUEUE&enableDeadLettering=false&disableAutoComplete=false&binary=false
//! ```
//!
//! The connection string may carry `UseDevelopmentEmulator=true` (plain AMQP, like the
//! Service Bus emulator); otherwise `amqps` on port 5671 (`sslContextParameters` for custom
//! trust). Instead of a connection string, `fullyQualifiedNamespace` + `sharedAccessName` +
//! `sharedAccessKey` (Cameleer extension) work too.
//!
//! **Consumer** (`receiveMessages`): receives from the queue, `topic/subscriptions/name`
//! or their `$deadletterqueue` (`subQueue=DEAD_LETTER_QUEUE`). In `PEEK_LOCK` mode a
//! processed message is completed (accepted); a failed one is abandoned (redelivered
//! with an incremented delivery count until the entity's MaxDeliveryCount dead-letters
//! it) or dead-lettered with `enableDeadLettering=true` (reason `ProcessingError`, the
//! exception as description). `RECEIVE_AND_DELETE` receives pre-settled messages.
//! `peekMessages` polls the `com.microsoft:peek-message` management operation
//! (`peekNumMaxMessages` per poll) without locking.
//!
//! **Producer**: `sendMessages` sends the body (a list body sends one message per element)
//! as a data section; `scheduleMessages` uses `com.microsoft:schedule-message` with the
//! `scheduledEnqueueTime` option / `CamelAzureServiceBusScheduledEnqueueTime` header
//! (epoch millis, or ISO-8601 `yyyy-MM-ddTHH:mm:ss[.SSS]Z`) and returns the sequence numbers in
//! `CamelAzureServiceBusSequenceNumber`.
//!
//! **Sessions**: `sessionEnabled=true` receives from session-enabled entities with the
//! `com.microsoft:session-filter`: each of the `maxConcurrentCalls` workers accepts the
//! next available session (or the `sessionId`), processes its messages in order and
//! releases it after `sessionIdleTimeout` (Cameleer, default 1 s) without messages.
//! Session state through the Cameleer producer operations `getSessionState` /
//! `setSessionState` (`com.microsoft:get-session-state` / `set-session-state`).
//!
//! **Locks**: while a peek-locked message is processed its lock (`com.microsoft:renew-lock`)
//! and the session lock (`com.microsoft:renew-session-lock`) are renewed automatically, for
//! at most `maxAutoLockRenewDuration` (ISO-8601 or millis, default `PT5M`; 0 disables).
//!
//! **Deferral**: a route sets `CamelAzureServiceBusDisposition` (Cameleer: `complete`,
//! `abandon`, `deadLetter`, `defer`) to settle explicitly; deferred messages are fetched
//! by sequence number with the Cameleer producer operation `receiveDeferredMessages`
//! (`com.microsoft:receive-by-sequence-number`; sequence numbers from the
//! `CamelAzureServiceBusSequenceNumber` header or the body).
//!
//! **Transactions**: `transacted=true` (Cameleer) sends the messages of an exchange in
//! one AMQP transaction (coordinator link, declare / discharge).
//!
//! **Credentials and transport**: see [`crate::azure`] (`credentialType=AZURE_IDENTITY` /
//! `TOKEN_CREDENTIAL`, `amqpTransportType=AMQP_WEB_SOCKETS`).
//!
//! **Headers** (consumer): `CamelAzureServiceBusMessageId`, `...CorrelationId`,

//! `...ContentType`, `...Subject`, `...To`, `...ReplyTo`, `...ReplyToSessionId`,
//! `...SessionId`, `...PartitionKey`, `...ApplicationProperties` (map),
//! `...EnqueuedTime` / `...LockedUntil` / `...ExpiresAt` (epoch millis),
//! `...SequenceNumber`, `...DeliveryCount`, `...LockToken`, `...TimeToLive`,
//! `...DeadLetterReason`, `...DeadLetterErrorDescription`, `...DeadLetterSource`.
//! Producer headers: `...ApplicationProperties`, `...MessageId`, `...CorrelationId`,
//! `...ContentType`, `...Subject`, `...To`, `...ReplyTo`, `...SessionId`,
//! `...PartitionKey`, `...TimeToLive`, `...ScheduledEnqueueTime`, `...ProducerOperation`.

use std::any::Any;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cameleer_core::component::support::BackgroundThreads;
use cameleer_core::context::{CamelContext, WeakContext};
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use cameleer_core::util::Parameters;
use cameleer_core::value::{ObjectMap, Value};

use crate::azure::AzureNamespace;
use crate::cbs::RequestResponse;
use crate::client::{Connection, Coordinator, Delivery, LinkOptions, Receiver, Sender, Session};

use crate::codec::AmqpValue;
use crate::message::{id_to_string, Body, Message};
use crate::types::{AmqpError, DeliveryState};
use crate::util::{amqp_failure, now_millis, to_amqp_simple, to_camel, Opts, COMMON_IGNORED};

/// Header names (Camel `ServiceBusConstants`).
pub mod headers {
    pub const APPLICATION_PROPERTIES: &str = "CamelAzureServiceBusApplicationProperties";
    pub const CONTENT_TYPE: &str = "CamelAzureServiceBusContentType";
    pub const CORRELATION_ID: &str = "CamelAzureServiceBusCorrelationId";
    pub const DEAD_LETTER_ERROR_DESCRIPTION: &str = "CamelAzureServiceBusDeadLetterErrorDescription";
    pub const DEAD_LETTER_REASON: &str = "CamelAzureServiceBusDeadLetterReason";
    pub const DEAD_LETTER_SOURCE: &str = "CamelAzureServiceBusDeadLetterSource";
    pub const DELIVERY_COUNT: &str = "CamelAzureServiceBusDeliveryCount";
    pub const ENQUEUED_TIME: &str = "CamelAzureServiceBusEnqueuedTime";
    pub const EXPIRES_AT: &str = "CamelAzureServiceBusExpiresAt";
    pub const LOCK_TOKEN: &str = "CamelAzureServiceBusLockToken";
    pub const LOCKED_UNTIL: &str = "CamelAzureServiceBusLockedUntil";
    pub const MESSAGE_ID: &str = "CamelAzureServiceBusMessageId";
    pub const PARTITION_KEY: &str = "CamelAzureServiceBusPartitionKey";
    pub const REPLY_TO: &str = "CamelAzureServiceBusReplyTo";
    pub const REPLY_TO_SESSION_ID: &str = "CamelAzureServiceBusReplyToSessionId";
    pub const SEQUENCE_NUMBER: &str = "CamelAzureServiceBusSequenceNumber";
    pub const SESSION_ID: &str = "CamelAzureServiceBusSessionId";
    pub const SUBJECT: &str = "CamelAzureServiceBusSubject";
    pub const TIME_TO_LIVE: &str = "CamelAzureServiceBusTimeToLive";
    pub const TO: &str = "CamelAzureServiceBusTo";
    pub const SCHEDULED_ENQUEUE_TIME: &str = "CamelAzureServiceBusScheduledEnqueueTime";
    pub const PRODUCER_OPERATION: &str = "CamelAzureServiceBusProducerOperation";
    /// Cameleer: how the consumer settles the message after processing (`complete`,
    /// `abandon`, `deadLetter`, `defer`), overriding the automatic completion.
    pub const DISPOSITION: &str = "CamelAzureServiceBusDisposition";
    /// Cameleer: session state bytes (`getSessionState` / `setSessionState`).
    pub const SESSION_STATE: &str = "CamelAzureServiceBusSessionState";
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiveMode {
    PeekLock,
    ReceiveAndDelete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProducerOperation {
    SendMessages,
    ScheduleMessages,
    /// Cameleer: receive deferred messages by sequence number (and remove them).
    ReceiveDeferredMessages,
    /// Cameleer: read the state of the session `CamelAzureServiceBusSessionId`.
    GetSessionState,
    /// Cameleer: store the body as the state of the session `CamelAzureServiceBusSessionId`.
    SetSessionState,
}

impl ProducerOperation {
    fn parse(s: &str) -> Result<ProducerOperation> {
        match s.trim() {
            "sendMessages" => Ok(ProducerOperation::SendMessages),
            "scheduleMessages" => Ok(ProducerOperation::ScheduleMessages),
            "receiveDeferredMessages" => Ok(ProducerOperation::ReceiveDeferredMessages),
            "getSessionState" => Ok(ProducerOperation::GetSessionState),
            "setSessionState" => Ok(ProducerOperation::SetSessionState),
            o => Err(CamelError::illegal_argument(format!("Unsupported producerOperation: {}", o))),
        }
    }
}

/// Resolved endpoint configuration.
#[derive(Clone, Debug)]
pub struct ServiceBusConfig {
    pub ns: AzureNamespace,
    pub entity: String,
    pub topic: bool,
    pub subscription: Option<String>,
    pub receive_mode: ReceiveMode,
    pub peek: bool,
    pub producer_operation: ProducerOperation,
    pub prefetch: u32,
    pub concurrent: usize,
    pub peek_max: u32,
    pub scheduled_enqueue_time: Option<i64>,
    pub dead_letter_queue: bool,
    pub enable_dead_lettering: bool,
    pub disable_auto_complete: bool,
    pub binary: bool,
    pub recovery_interval: Duration,
    pub poll_delay: Duration,
    /// Receive from session-enabled entities (`sessionEnabled`).
    pub session_enabled: bool,
    /// The session to lock (`sessionId`); none = the next available session.
    pub session_id: Option<String>,
    /// How long a session receiver waits for messages before it releases the session
    /// and accepts the next one (`sessionIdleTimeout`, Cameleer).
    pub session_idle_timeout: Duration,
    /// Automatic lock renewal while a message is processed (`maxAutoLockRenewDuration`;
    /// zero disables it).
    pub max_auto_lock_renew: Duration,
    /// Send list bodies atomically in one AMQP transaction (`transacted`, Cameleer).
    pub transacted: bool,
}

impl ServiceBusConfig {
    /// The address consumers receive from.
    pub fn receive_address(&self) -> String {
        let base = match (&self.subscription, self.topic) {
            (Some(s), true) => format!("{}/subscriptions/{}", self.entity, s),
            _ => self.entity.clone(),
        };
        if self.dead_letter_queue {
            format!("{}/$deadletterqueue", base)
        } else {
            base
        }
    }
}

/// Parse a scheduled enqueue time: epoch millis or `yyyy-MM-ddTHH:mm:ss[.SSS](Z|+hh:mm)`.
pub fn parse_time(v: &Value) -> Option<i64> {
    match v {
        Value::Int(i) => Some(*i),
        Value::Float(f) => Some(*f as i64),
        Value::String(s) => {
            let s = s.trim();
            if let Ok(n) = s.parse::<i64>() {
                return Some(n);
            }
            parse_iso(s)
        }
        _ => None,
    }
}

fn parse_iso(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || (b[10] != b'T' && b[10] != b' ') {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (n(0..4)?, n(5..7)?, n(8..10)?, n(11..13)?, n(14..16)?, n(17..19)?);
    let mut rest = &s[19..];
    let mut ms = 0;
    if let Some(r) = rest.strip_prefix('.') {
        let digits: String = r.chars().take_while(|c| c.is_ascii_digit()).collect();
        rest = &r[digits.len()..];
        ms = format!("{:0<3}", &digits[..digits.len().min(3)]).parse().unwrap_or(0);
    }
    let offset_min = match rest {
        "" | "Z" => 0,
        r if r.len() == 6 && (r.starts_with('+') || r.starts_with('-')) => {
            let v = r[1..3].parse::<i64>().ok()? * 60 + r[4..6].parse::<i64>().ok()?;
            if r.starts_with('-') {
                -v
            } else {
                v
            }
        }
        _ => return None,
    };
    // days from civil (Howard Hinnant)
    let (y2, m2) = if mo <= 2 { (y - 1, mo + 9) } else { (y, mo - 3) };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let doy = (153 * m2 + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(((days * 86400 + h * 3600 + mi * 60 + se) - offset_min * 60) * 1000 + ms)
}

impl ServiceBusConfig {
    fn from_uri(scheme: &str, ctx: &CamelContext, remaining: &str, params: &mut Parameters) -> Result<ServiceBusConfig> {
        let mut o = Opts::new(ctx, scheme, params);
        let (ns, entity_path) = AzureNamespace::resolve(ctx, scheme, &mut o, None, crate::identity::SERVICE_BUS_SCOPE)?;
        let entity = if remaining.trim().is_empty() { entity_path.unwrap_or_default() } else { remaining.trim().trim_matches('/').to_string() };
        if entity.is_empty() {
            return Err(CamelError::illegal_argument("azure-servicebus: the queue or topic name must be specified"));
        }
        let topic = match o.get("serviceBusType").as_deref().map(str::trim) {
            None | Some("queue") => false,
            Some("topic") => true,
            Some(other) => return Err(CamelError::illegal_argument(format!("Invalid serviceBusType (queue or topic): {}", other))),
        };
        let subscription = o.string("subscriptionName");
        let receive_mode = match o.get("receiveMode").as_deref().map(str::trim) {
            None | Some("PEEK_LOCK") => ReceiveMode::PeekLock,
            Some("RECEIVE_AND_DELETE") => ReceiveMode::ReceiveAndDelete,
            Some(other) => return Err(CamelError::illegal_argument(format!("Invalid receiveMode: {}", other))),
        };
        let peek = match o.get("consumerOperation").as_deref().map(str::trim) {
            None | Some("receiveMessages") => false,
            Some("peekMessages") => true,
            Some(other) => return Err(CamelError::illegal_argument(format!("Unsupported consumerOperation: {}", other))),
        };
        let producer_operation = match o.get("producerOperation") {
            None => ProducerOperation::SendMessages,
            Some(s) => ProducerOperation::parse(&s)?,
        };
        let prefetch = o.num("prefetchCount", 0u32)?;
        let concurrent = o.num("maxConcurrentCalls", 1usize)?.max(1);
        let peek_max = o.num("peekNumMaxMessages", 10u32)?.max(1);
        let scheduled_enqueue_time = match o.string("scheduledEnqueueTime") {
            Some(s) => Some(parse_time(&Value::String(s.clone())).ok_or_else(|| CamelError::illegal_argument(format!("Invalid scheduledEnqueueTime: {}", s)))?),
            None => None,
        };
        let dead_letter_queue = match o.get("subQueue").as_deref().map(str::trim) {
            None | Some("NONE") => false,
            Some("DEAD_LETTER_QUEUE") => true,
            Some(other) => return Err(CamelError::illegal_argument(format!("Unsupported subQueue: {}", other))),
        };
        let enable_dead_lettering = o.bool("enableDeadLettering", false)?;
        let disable_auto_complete = o.bool("disableAutoComplete", false)?;
        let binary = o.bool("binary", false)?;
        let recovery_interval = o.millis("recoveryInterval", Duration::from_millis(5000))?;
        let poll_delay = o.millis("delay", Duration::from_millis(500))?;
        let session_enabled = o.bool("sessionEnabled", false)?;
        let session_id = o.string("sessionId");
        let session_idle_timeout = o.millis("sessionIdleTimeout", Duration::from_millis(1000))?;
        let max_auto_lock_renew = match o.string("maxAutoLockRenewDuration") {
            None => Duration::from_secs(300),
            Some(s) => parse_iso_duration(&s).ok_or_else(|| CamelError::illegal_argument(format!("Invalid maxAutoLockRenewDuration: {}", s)))?,
        };
        let transacted = o.bool("transacted", false)?;
        o.ignore(COMMON_IGNORED);
        o.ignore(&["processorClient", "receiverAsyncClient", "senderAsyncClient", "serviceBusReceiveMode", "serviceBusTransactionContext", "headerFilterStrategy"]);
        Ok(ServiceBusConfig {
            ns,
            entity,
            topic,
            subscription,
            receive_mode,
            peek,
            producer_operation,
            prefetch,
            concurrent,
            peek_max,
            scheduled_enqueue_time,
            dead_letter_queue,
            enable_dead_lettering,
            disable_auto_complete,
            binary,
            recovery_interval,
            poll_delay,
            session_enabled: session_enabled || session_id.is_some(),
            session_id,
            session_idle_timeout,
            max_auto_lock_renew,
            transacted,
        })
    }
}

/// A duration as milliseconds, `5s`-style or ISO-8601 (`PT5M`, `PT30S`).
fn parse_iso_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if let Ok(n) = s.parse::<u64>() {
        return Some(Duration::from_millis(n));
    }
    if let Some(rest) = s.strip_prefix("PT").or_else(|| s.strip_prefix("pt")) {
        let mut total = 0f64;
        let mut num = String::new();
        for c in rest.chars() {
            match c.to_ascii_uppercase() {
                d if d.is_ascii_digit() || d == '.' => num.push(d),
                'H' => total += num.drain(..).collect::<String>().parse::<f64>().ok()? * 3600.0,
                'M' => total += num.drain(..).collect::<String>().parse::<f64>().ok()? * 60.0,
                'S' => total += num.drain(..).collect::<String>().parse::<f64>().ok()?,
                _ => return None,
            }
        }
        return if num.is_empty() { Some(Duration::from_millis((total * 1000.0) as u64)) } else { None };
    }
    cameleer_core::util::parse_duration(s).ok()
}

/// The `azure-servicebus` component.
#[derive(Default)]
pub struct ServiceBusComponent;

impl Component for ServiceBusComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let cfg = ServiceBusConfig::from_uri("azure-servicebus", ctx, remaining, params)?;
        Ok(Arc::new(ServiceBusEndpoint { uri: uri.to_string(), ctx: ctx.downgrade(), cfg: Arc::new(cfg) }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// An `azure-servicebus` endpoint.
pub struct ServiceBusEndpoint {
    uri: String,
    ctx: WeakContext,
    cfg: Arc<ServiceBusConfig>,
}

impl ServiceBusEndpoint {
    pub fn config(&self) -> &ServiceBusConfig {
        &self.cfg
    }
}

impl Endpoint for ServiceBusEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(ServiceBusProducer { cfg: self.cfg.clone(), state: Mutex::new(None) }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        if self.cfg.topic && self.cfg.subscription.is_none() {
            return Err(CamelError::illegal_argument("azure-servicebus: subscriptionName is required to consume from a topic"));
        }
        Ok(Arc::new(ServiceBusConsumer { cfg: self.cfg.clone(), uri: self.uri.clone(), ctx: self.ctx.clone(), processor, threads: BackgroundThreads::new() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---------------------------------------------------------------------------
// mapping
// ---------------------------------------------------------------------------

fn hstr(ex: &Exchange, k: &str) -> Option<String> {
    ex.header(k).filter(|v| !v.is_null()).map(|v| v.to_string()).filter(|s| !s.is_empty())
}

fn body_bytes(v: &Value) -> Vec<u8> {
    match v {
        Value::Bytes(b) => b.clone(),
        Value::Null => Vec::new(),
        Value::Object(_) | Value::Array(_) => v.to_json().into_bytes(),
        other => other.to_string().into_bytes(),
    }
}

/// Build a Service Bus message from an exchange and one body element.
pub fn build_message(ex: &Exchange, body: &Value) -> Message {
    use headers::*;
    let mut m = Message::data(body_bytes(body));
    let p = m.props_mut();
    p.message_id = Some(AmqpValue::String(hstr(ex, MESSAGE_ID).unwrap_or_else(|| crate::message::format_uuid(&crate::message::random_uuid()).replace('-', ""))));
    p.correlation_id = hstr(ex, CORRELATION_ID).map(AmqpValue::String);
    p.content_type = hstr(ex, CONTENT_TYPE);
    p.subject = hstr(ex, SUBJECT);
    p.to = hstr(ex, TO);
    p.reply_to = hstr(ex, REPLY_TO);
    p.group_id = hstr(ex, SESSION_ID);
    if let Some(ttl) = ex.header(TIME_TO_LIVE).and_then(|v| v.as_i64()).filter(|t| *t > 0) {
        m.header_mut().ttl = Some(ttl.min(u32::MAX as i64) as u32);
        m.props_mut().absolute_expiry_time = Some(now_millis() + ttl);
    }
    if let Some(pk) = hstr(ex, PARTITION_KEY) {
        m.set_annotation("x-opt-partition-key", AmqpValue::str(pk));
    }
    if let Some(Value::Object(props)) = ex.header(APPLICATION_PROPERTIES) {
        for (k, v) in props {
            m.set_app_property(k, to_amqp_simple(v));
        }
    }
    m
}

/// Map a received message to exchange headers and body.
pub fn message_to_exchange(m: &Message, binary: bool, lock_token: Option<String>, ex: &mut Exchange) {
    use headers::*;
    let bytes = m.body_bytes();
    let body = match (&m.body, bytes) {
        (Body::Value(v), _) if !matches!(v, AmqpValue::Binary(_) | AmqpValue::String(_)) => to_camel(v),
        (_, Some(b)) if binary => Value::Bytes(b),
        (_, Some(b)) => String::from_utf8(b).map(Value::String).unwrap_or_else(|e| Value::Bytes(e.into_bytes())),
        (_, None) => Value::Null,
    };
    ex.set_body(body);
    let p = m.props();
    if let Some(v) = &p.message_id {
        ex.set_header(MESSAGE_ID, id_to_string(v));
    }
    if let Some(v) = &p.correlation_id {
        ex.set_header(CORRELATION_ID, id_to_string(v));
    }
    let opt = |ex: &mut Exchange, k: &str, v: &Option<String>| {
        if let Some(v) = v {
            ex.set_header(k, v.clone());
        }
    };
    opt(ex, CONTENT_TYPE, &p.content_type);
    opt(ex, SUBJECT, &p.subject);
    opt(ex, TO, &p.to);
    opt(ex, REPLY_TO, &p.reply_to);
    opt(ex, SESSION_ID, &p.group_id);
    opt(ex, REPLY_TO_SESSION_ID, &p.reply_to_group_id);
    if let Some(t) = p.absolute_expiry_time {
        ex.set_header(EXPIRES_AT, t);
    }
    if let Some(ttl) = m.header.as_ref().and_then(|h| h.ttl) {
        ex.set_header(TIME_TO_LIVE, ttl as i64);
    }
    ex.set_header(DELIVERY_COUNT, m.header.as_ref().map(|h| h.delivery_count).unwrap_or(0) as i64);
    if let Some(v) = m.annotation("x-opt-sequence-number").and_then(|v| v.as_i64()) {
        ex.set_header(SEQUENCE_NUMBER, v);
    }
    if let Some(v) = m.annotation("x-opt-enqueued-time").and_then(|v| v.as_i64()) {
        ex.set_header(ENQUEUED_TIME, v);
    }
    if let Some(v) = m.annotation("x-opt-locked-until").and_then(|v| v.as_i64()) {
        ex.set_header(LOCKED_UNTIL, v);
    }
    if let Some(v) = m.annotation("x-opt-partition-key").and_then(|v| v.as_str()) {
        ex.set_header(PARTITION_KEY, v.to_string());
    }
    if let Some(v) = m.annotation("x-opt-deadletter-source").and_then(|v| v.as_str()) {
        ex.set_header(DEAD_LETTER_SOURCE, v.to_string());
    }
    let token = lock_token.or_else(|| match m.annotation("x-opt-lock-token") {
        Some(AmqpValue::Uuid(u)) => Some(crate::message::format_uuid(u)),
        _ => None,
    });
    if let Some(t) = token {
        ex.set_header(LOCK_TOKEN, t);
    }
    let mut props = ObjectMap::new();
    for (k, v) in &m.application_properties {
        match k.as_str() {
            "DeadLetterReason" => ex.set_header(DEAD_LETTER_REASON, to_camel(v)),
            "DeadLetterErrorDescription" => ex.set_header(DEAD_LETTER_ERROR_DESCRIPTION, to_camel(v)),
            _ => {}
        }
        props.insert(k.clone(), to_camel(v));
    }
    ex.set_header(APPLICATION_PROPERTIES, Value::Object(props));
}

// ---------------------------------------------------------------------------
// producer
// ---------------------------------------------------------------------------

struct ProducerState {
    conn: Connection,
    session: Session,
    sender: Option<Sender>,
    management: Option<RequestResponse>,
    coordinator: Option<Coordinator>,
}

struct ServiceBusProducer {
    cfg: Arc<ServiceBusConfig>,
    state: Mutex<Option<ProducerState>>,
}

impl ServiceBusProducer {
    fn with_state<T>(&self, f: impl FnOnce(&ServiceBusConfig, &mut ProducerState) -> Result<T>) -> Result<T> {
        let mut g = self.state.lock().unwrap();
        if g.as_ref().is_some_and(|s| !s.conn.is_open()) {
            if let Some(s) = g.take() {
                s.conn.close();
            }
        }
        if g.is_none() {
            let (conn, session) = self.cfg.ns.connect(&[&self.cfg.entity])?;
            *g = Some(ProducerState { conn, session, sender: None, management: None, coordinator: None });
        }
        let r = f(&self.cfg, g.as_mut().unwrap());
        if r.is_err() && g.as_ref().is_some_and(|s| !s.conn.is_open()) {
            if let Some(s) = g.take() {
                s.conn.close();
            }
        }
        r
    }

    fn messages(ex: &Exchange) -> Vec<Message> {
        match ex.body() {
            Value::Array(items) => items.iter().map(|b| build_message(ex, b)).collect(),
            b => vec![build_message(ex, b)],
        }
    }
}

impl Service for ServiceBusProducer {
    fn stop(&self) -> Result<()> {
        if let Some(s) = self.state.lock().unwrap().take() {
            s.conn.close();
        }
        Ok(())
    }
}

impl ServiceBusProducer {
    /// A management request on the entity; returns the response body.
    fn manage(&self, operation: &str, body: Vec<(AmqpValue, AmqpValue)>) -> Result<AmqpValue> {
        self.with_state(|cfg, s| {
            if s.management.is_none() {
                s.management = Some(cfg.ns.management(&s.session, &format!("{}/$management", cfg.receive_address()))?);
            }
            let mut req = Message { body: Body::Value(AmqpValue::Map(body)), ..Default::default() };
            req.set_app_property("operation", AmqpValue::str(operation));
            let resp = s.management.as_ref().unwrap().request(req)?;
            let (code, desc) = RequestResponse::status(&resp);
            if code != 200 {
                return Err(amqp_failure(format!("{} failed: {} {}", operation, code, desc)));
            }
            Ok(match resp.body {
                Body::Value(v) => v,
                _ => AmqpValue::Null,
            })
        })
    }
}

impl Processor for ServiceBusProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let op = match hstr(ex, headers::PRODUCER_OPERATION) {
            Some(o) => ProducerOperation::parse(&o)?,
            None => self.cfg.producer_operation,
        };
        match op {
            ProducerOperation::ReceiveDeferredMessages => {
                let seqs: Vec<AmqpValue> = match ex.header(headers::SEQUENCE_NUMBER).cloned().unwrap_or_else(|| ex.body().clone()) {
                    Value::Array(a) => a.iter().filter_map(|v| v.as_i64().or_else(|| v.to_string().trim().parse().ok())).map(AmqpValue::Long).collect(),
                    v => v.as_i64().or_else(|| v.to_string().trim().parse().ok()).map(AmqpValue::Long).into_iter().collect(),
                };
                if seqs.is_empty() {
                    return Err(CamelError::illegal_argument("receiveDeferredMessages needs the sequence numbers (CamelAzureServiceBusSequenceNumber header or body)"));
                }
                let body = vec![(AmqpValue::str("sequence-numbers"), AmqpValue::Array(seqs)), (AmqpValue::str("receiver-settle-mode"), AmqpValue::Uint(0))];
                let resp = self.manage("com.microsoft:receive-by-sequence-number", body)?;
                let msgs: Vec<Message> = resp.map_get("messages").and_then(|m| m.as_list()).unwrap_or(&[]).iter().filter_map(|i| i.map_get("message").and_then(|b| b.as_binary()).and_then(|b| Message::decode(b).ok())).collect();
                ex.message.headers.clear();
                if msgs.len() == 1 {
                    message_to_exchange(&msgs[0], self.cfg.binary, None, ex);
                } else {
                    let mut bodies = Vec::new();
                    for m in &msgs {
                        let mut tmp = Exchange::new(None);
                        message_to_exchange(m, self.cfg.binary, None, &mut tmp);
                        bodies.push(tmp.body().clone());
                    }
                    ex.set_body(Value::Array(bodies));
                }
                return Ok(());
            }
            ProducerOperation::GetSessionState | ProducerOperation::SetSessionState => {
                let sid = hstr(ex, headers::SESSION_ID).or_else(|| self.cfg.session_id.clone()).ok_or_else(|| CamelError::illegal_argument("The session id (CamelAzureServiceBusSessionId header or sessionId) is required"))?;
                if op == ProducerOperation::GetSessionState {
                    let resp = self.manage("com.microsoft:get-session-state", vec![(AmqpValue::str("session-id"), AmqpValue::str(sid))])?;
                    let state = resp.map_get("session-state").and_then(|v| v.as_binary()).map(|b| Value::Bytes(b.to_vec())).unwrap_or(Value::Null);
                    ex.set_header(headers::SESSION_STATE, state.clone());
                    ex.set_body(state);
                } else {
                    let state = match ex.body() {
                        Value::Null => AmqpValue::Null,
                        v => AmqpValue::Binary(body_bytes(v)),
                    };
                    self.manage("com.microsoft:set-session-state", vec![(AmqpValue::str("session-id"), AmqpValue::str(sid)), (AmqpValue::str("session-state"), state)])?;
                }
                return Ok(());
            }
            _ => {}
        }
        let msgs = Self::messages(ex);
        match op {
            ProducerOperation::SendMessages => self.with_state(|cfg, s| {
                if s.sender.is_none() {
                    s.sender = Some(s.session.sender(LinkOptions::new(&cfg.entity))?);
                }
                let snd = s.sender.as_ref().unwrap();
                if cfg.transacted {
                    // all messages of the exchange in one AMQP transaction
                    if s.coordinator.is_none() {
                        s.coordinator = Some(s.session.coordinator()?);
                    }
                    let coord = s.coordinator.as_ref().unwrap();
                    let id = coord.declare()?;
                    for m in &msgs {
                        if let Err(e) = snd.send_accepted_in_txn(m, &id, cfg.ns.request_timeout) {
                            let _ = coord.rollback(&id);
                            return Err(e);
                        }
                    }
                    return coord.commit(&id);
                }
                for m in &msgs {
                    snd.send_accepted(m, cfg.ns.request_timeout)?;
                }
                Ok(())
            }),
            ProducerOperation::ScheduleMessages => {
                let when = ex.header(headers::SCHEDULED_ENQUEUE_TIME).and_then(parse_time).or(self.cfg.scheduled_enqueue_time).unwrap_or_else(now_millis);
                let seqs = self.with_state(|cfg, s| {
                    if s.management.is_none() {
                        s.management = Some(cfg.ns.management(&s.session, &format!("{}/$management", cfg.entity))?);
                    }
                    let items: Vec<AmqpValue> = msgs
                        .iter()
                        .map(|m| {
                            let mut m = m.clone();
                            m.set_annotation("x-opt-scheduled-enqueue-time", AmqpValue::Timestamp(when));
                            let id = m.props().message_id.map(|v| id_to_string(&v)).unwrap_or_default();
                            AmqpValue::Map(vec![(AmqpValue::str("message-id"), AmqpValue::str(id)), (AmqpValue::str("message"), AmqpValue::Binary(m.encode()))])
                        })
                        .collect();
                    let mut req = Message { body: Body::Value(AmqpValue::Map(vec![(AmqpValue::str("messages"), AmqpValue::List(items))])), ..Default::default() };
                    req.set_app_property("operation", AmqpValue::str("com.microsoft:schedule-message"));
                    let resp = s.management.as_ref().unwrap().request(req)?;
                    let (code, desc) = RequestResponse::status(&resp);
                    if code != 200 {
                        return Err(amqp_failure(format!("schedule-message failed: {} {}", code, desc)));
                    }
                    let seqs: Vec<i64> = match &resp.body {
                        Body::Value(v) => v.map_get("sequence-numbers").and_then(|s| s.as_list()).unwrap_or(&[]).iter().filter_map(|x| x.as_i64()).collect(),
                        _ => Vec::new(),
                    };
                    Ok(seqs)
                })?;
                let v = if seqs.len() == 1 { Value::Int(seqs[0]) } else { Value::Array(seqs.into_iter().map(Value::Int).collect()) };
                ex.set_header(headers::SEQUENCE_NUMBER, v);
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

// ---------------------------------------------------------------------------
// consumer
// ---------------------------------------------------------------------------

struct ServiceBusConsumer {
    cfg: Arc<ServiceBusConfig>,
    uri: String,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
    threads: BackgroundThreads,
}

fn dead_letter_error(reason: &str, description: &str) -> AmqpError {
    let mut err = AmqpError::new("com.microsoft:dead-letter", "dead-lettered by the consumer");
    err.info = vec![(AmqpValue::sym("DeadLetterReason"), AmqpValue::str(reason)), (AmqpValue::sym("DeadLetterErrorDescription"), AmqpValue::str(description))];
    err
}

fn settle(cfg: &ServiceBusConfig, rx: &Receiver, d: &Delivery, ex: &Exchange) -> Result<()> {
    if d.settled {
        return Ok(());
    }
    // an explicit disposition chosen by the route wins
    if let Some(disp) = hstr(ex, headers::DISPOSITION) {
        return match disp.to_ascii_lowercase().as_str() {
            "complete" | "completed" => rx.accept(d),
            "abandon" | "abandoned" => rx.modify(d, false, false, vec![]),
            "defer" | "deferred" => rx.modify(d, false, true, vec![]),
            "deadletter" | "dead_letter" | "suspended" => {
                let reason = hstr(ex, headers::DEAD_LETTER_REASON).unwrap_or_else(|| "ProcessingError".into());
                let desc = hstr(ex, headers::DEAD_LETTER_ERROR_DESCRIPTION).unwrap_or_default();
                rx.settle(d, DeliveryState::Rejected(Some(dead_letter_error(&reason, &desc))))
            }
            other => Err(CamelError::illegal_argument(format!("Invalid {} (complete, abandon, deadLetter or defer): {}", headers::DISPOSITION, other))),
        };
    }
    if cfg.disable_auto_complete {
        return Ok(());
    }
    match &ex.exception {
        None => rx.accept(d),
        Some(e) if cfg.enable_dead_lettering => rx.settle(d, DeliveryState::Rejected(Some(dead_letter_error("ProcessingError", &format!("{}: {}", e.kind, e.message))))),
        Some(_) => rx.modify(d, false, false, vec![]),
    }
}

/// The lock token of a peek-lock delivery (the delivery tag, .NET GUID layout).
fn lock_token_of(d: &Delivery) -> Option<[u8; 16]> {
    let t: [u8; 16] = d.tag.as_slice().try_into().ok()?;
    Some(crate::client::dotnet_guid_bytes(&t))
}

/// A shared management link of a consumer connection (requests are serialized).
struct Management<'a> {
    cfg: &'a ServiceBusConfig,
    session: &'a Session,
    rr: Mutex<Option<RequestResponse>>,
}

impl Management<'_> {
    fn request(&self, operation: &str, body: Vec<(AmqpValue, AmqpValue)>) -> Result<AmqpValue> {
        let mut g = self.rr.lock().unwrap();
        if g.is_none() {
            *g = Some(self.cfg.ns.management(self.session, &format!("{}/$management", self.cfg.receive_address()))?);
        }
        let mut req = Message { body: Body::Value(AmqpValue::Map(body)), ..Default::default() };
        req.set_app_property("operation", AmqpValue::str(operation));
        let resp = match g.as_ref().unwrap().request(req) {
            Ok(r) => r,
            Err(e) => {
                *g = None;
                return Err(e);
            }
        };
        let (code, desc) = RequestResponse::status(&resp);
        if code != 200 {
            return Err(amqp_failure(format!("{} failed: {} {}", operation, code, desc)));
        }
        Ok(match resp.body {
            Body::Value(v) => v,
            _ => AmqpValue::Null,
        })
    }

    /// `com.microsoft:renew-lock`: the new lock expiry.
    fn renew_lock(&self, token: [u8; 16]) -> Result<i64> {
        let r = self.request("com.microsoft:renew-lock", vec![(AmqpValue::str("lock-tokens"), AmqpValue::Array(vec![AmqpValue::Uuid(token)]))])?;
        r.map_get("expirations").and_then(|e| e.as_list()).and_then(|l| l.first()).and_then(|t| t.as_i64()).ok_or_else(|| amqp_failure("renew-lock: no expiration in the response"))
    }

    /// `com.microsoft:renew-session-lock`: the new session lock expiry.
    fn renew_session_lock(&self, session_id: &str) -> Result<i64> {
        let r = self.request("com.microsoft:renew-session-lock", vec![(AmqpValue::str("session-id"), AmqpValue::str(session_id))])?;
        r.map_get("expiration").and_then(|t| t.as_i64()).ok_or_else(|| amqp_failure("renew-session-lock: no expiration in the response"))
    }
}

/// A locked session of a session receiver.
struct SessionLock {
    id: String,
    locked_until: Mutex<i64>,
}

impl SessionLock {
    /// Renew the session lock when less than half of its remaining time is left since `from`.
    fn renew_if_due(&self, mgmt: &Management, last: &mut i64) -> Result<()> {
        let until = *self.locked_until.lock().unwrap();
        let now = now_millis();
        if now >= *last + (until - *last) / 2 {
            let new_until = mgmt.renew_session_lock(&self.id)?;
            *self.locked_until.lock().unwrap() = new_until;
            *last = now;
        }
        Ok(())
    }
}

impl ServiceBusConsumer {
    /// Process one delivery, renewing its lock (and the session lock) while the route runs.
    fn handle(&self, rx: &Receiver, d: Delivery, mgmt: &Management, session: Option<&SessionLock>) {
        let cfg = &self.cfg;
        let mut ex = Exchange::new(self.ctx.upgrade());
        message_to_exchange(&d.message, cfg.binary, if d.settled { None } else { d.lock_token() }, &mut ex);
        let token = if d.settled { None } else { lock_token_of(&d) };
        let renew = cfg.max_auto_lock_renew > Duration::ZERO && (token.is_some() || session.is_some());
        let done = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|scope| {
            if renew {
                let (done, uri) = (&done, &self.uri);
                let mut locked_until = d.message.annotation("x-opt-locked-until").and_then(|v| v.as_i64()).unwrap_or(0);
                scope.spawn(move || {
                    let started = std::time::Instant::now();
                    let mut last = now_millis();
                    let mut session_last = now_millis();
                    while !done.load(std::sync::atomic::Ordering::SeqCst) && started.elapsed() < cfg.max_auto_lock_renew {
                        std::thread::sleep(Duration::from_millis(20));
                        let now = now_millis();
                        if let Some(t) = token.filter(|_| locked_until > 0 && now >= last + (locked_until - last) / 2) {
                            match mgmt.renew_lock(t) {
                                Ok(until) => {
                                    locked_until = until;
                                    last = now;
                                }
                                Err(e) => {
                                    cameleer_core::camel_warn!(uri, "Cannot renew the message lock: {}", e.message);
                                    locked_until = 0;
                                }
                            }
                        }
                        if let Some(s) = session {
                            if let Err(e) = s.renew_if_due(mgmt, &mut session_last) {
                                cameleer_core::camel_warn!(uri, "Cannot renew the session lock of {}: {}", s.id, e.message);
                            }
                        }
                    }
                });
            }
            if let Err(e) = self.processor.process(&mut ex) {
                ex.set_exception(e);
            }
            done.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        if let Some(e) = &ex.exception {
            cameleer_core::camel_warn!(&self.uri, "Error processing exchange. Exchange[{}]. Caused by: [{} - {}]", ex.exchange_id, e.kind, e.message);
        }
        if let Err(e) = settle(cfg, rx, &d, &ex) {
            cameleer_core::camel_warn!(&self.uri, "Cannot settle message: {}", e.message);
        }
    }

    fn link_options(&self) -> LinkOptions {
        let cfg = &self.cfg;
        let mut o = LinkOptions::new(&cfg.receive_address()).credit(cfg.prefetch.max(1));
        if cfg.receive_mode == ReceiveMode::ReceiveAndDelete {
            o = o.at_most_once();
        }
        if cfg.session_enabled {
            let sid = cfg.session_id.as_ref().map(|s| AmqpValue::str(s.clone())).unwrap_or(AmqpValue::Null);
            o = o.filter(crate::azure::SESSION_FILTER, crate::azure::SESSION_FILTER, sid);
        }
        o
    }

    /// A session worker: lock a session (the configured one or the next available),
    /// process its messages until it stays idle, release it, repeat.
    fn session_worker(&self, session: &Session, mgmt: &Management, stop: &Arc<cameleer_core::component::support::StopFlag>) {
        let cfg = &self.cfg;
        while !stop.is_stopped() && session.is_open() {
            let rx = match session.receiver(self.link_options()) {
                Ok(rx) => rx,
                Err(_) => {
                    if stop.sleep(cfg.poll_delay) {
                        return;
                    }
                    continue;
                }
            };
            let attach = rx.remote_attach();
            let id = attach.source.as_ref().and_then(|s| s.filter.iter().find_map(|(_, v)| (v.descriptor_symbol() == Some(crate::azure::SESSION_FILTER)).then(|| v.undescribed().as_str().map(String::from)).flatten())).or_else(|| cfg.session_id.clone()).unwrap_or_default();
            let until = attach.properties.iter().find(|(k, _)| k.as_str() == Some("com.microsoft:locked-until-utc")).map(|(_, v)| crate::azure::locked_until_millis(v)).unwrap_or(0);
            let lock = SessionLock { id, locked_until: Mutex::new(until) };
            let mut last_renew = now_millis();
            let mut idle_since = std::time::Instant::now();
            loop {
                if stop.is_stopped() {
                    break;
                }
                if cfg.max_auto_lock_renew > Duration::ZERO && until > 0 {
                    let _ = lock.renew_if_due(mgmt, &mut last_renew);
                }
                match rx.receive(Duration::from_millis(50)) {
                    Ok(Some(d)) => {
                        self.handle(&rx, d, mgmt, Some(&lock));
                        idle_since = std::time::Instant::now();
                    }
                    Ok(None) => {
                        if cfg.session_id.is_none() && idle_since.elapsed() >= cfg.session_idle_timeout {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            rx.close();
            if cfg.session_id.is_some() && !stop.is_stopped() && stop.sleep(cfg.poll_delay) {
                return;
            }
        }
    }

    fn receive_loop(&self, stop: &Arc<cameleer_core::component::support::StopFlag>, ready: &mut Option<std::sync::mpsc::Sender<Result<()>>>) {
        let cfg = &self.cfg;
        while !stop.is_stopped() {
            let setup = (|| -> Result<(Connection, Session, Vec<Receiver>)> {
                let entity = match (&cfg.subscription, cfg.topic) {
                    (Some(s), true) => format!("{}/subscriptions/{}", cfg.entity, s),
                    _ => cfg.entity.clone(),
                };
                let (conn, session) = cfg.ns.connect(&[&entity])?;
                let mut rxs = Vec::new();
                if !cfg.session_enabled {
                    for _ in 0..cfg.concurrent {
                        rxs.push(session.receiver(self.link_options())?);
                    }
                }
                Ok((conn, session, rxs))
            })();
            let (conn, session, rxs) = match setup {
                Ok(x) => x,
                Err(e) => {
                    cameleer_core::camel_warn!(&self.uri, "Cannot start consumer on {}: {} - retrying in {} ms", self.uri, e.message, cfg.recovery_interval.as_millis());
                    if let Some(tx) = ready.take() {
                        let _ = tx.send(Err(e));
                    }
                    if stop.sleep(cfg.recovery_interval) {
                        return;
                    }
                    continue;
                }
            };
            if let Some(tx) = ready.take() {
                let _ = tx.send(Ok(()));
            }
            let mgmt = Management { cfg, session: &session, rr: Mutex::new(None) };
            std::thread::scope(|scope| {
                if cfg.session_enabled {
                    for _ in 0..cfg.concurrent {
                        let (st, mgmt, session) = (stop.clone(), &mgmt, &session);
                        scope.spawn(move || self.session_worker(session, mgmt, &st));
                    }
                }
                for rx in &rxs {
                    let (st, mgmt) = (stop.clone(), &mgmt);
                    scope.spawn(move || loop {
                        if st.is_stopped() {
                            break;
                        }
                        match rx.receive(Duration::from_millis(50)) {
                            Ok(Some(d)) => self.handle(rx, d, mgmt, None),
                            Ok(None) => {}
                            Err(_) => break,
                        }
                    });
                }
            });
            let lost = !stop.is_stopped();
            conn.close();
            if lost {
                cameleer_core::camel_warn!(&self.uri, "Connection lost on {} - reconnecting in {} ms", self.uri, cfg.recovery_interval.as_millis());
                if stop.sleep(cfg.recovery_interval) {
                    return;
                }
            }
        }
    }

    fn peek_loop(&self, stop: &Arc<cameleer_core::component::support::StopFlag>, ready: &mut Option<std::sync::mpsc::Sender<Result<()>>>) {
        let cfg = &self.cfg;
        let mut from = 0i64;
        let address = cfg.receive_address();
        while !stop.is_stopped() {
            let setup = (|| -> Result<(Connection, RequestResponse)> {
                let (conn, session) = cfg.ns.connect(&[&address])?;
                let mgmt = cfg.ns.management(&session, &format!("{}/$management", address))?;
                Ok((conn, mgmt))
            })();
            let (conn, mgmt) = match setup {
                Ok(x) => x,
                Err(e) => {
                    if let Some(tx) = ready.take() {
                        let _ = tx.send(Err(e));
                    }
                    if stop.sleep(cfg.recovery_interval) {
                        return;
                    }
                    continue;
                }
            };
            if let Some(tx) = ready.take() {
                let _ = tx.send(Ok(()));
            }
            while !stop.is_stopped() {
                let mut req = Message {
                    body: Body::Value(AmqpValue::Map(vec![
                        (AmqpValue::str("from-sequence-number"), AmqpValue::Long(from)),
                        (AmqpValue::str("message-count"), AmqpValue::Int(cfg.peek_max as i32)),
                    ])),
                    ..Default::default()
                };
                req.set_app_property("operation", AmqpValue::str("com.microsoft:peek-message"));
                let resp = match mgmt.request(req) {
                    Ok(r) => r,
                    Err(_) => break,
                };
                let items: Vec<Message> = match &resp.body {
                    Body::Value(v) => v.map_get("messages").and_then(|m| m.as_list()).unwrap_or(&[]).iter().filter_map(|i| i.map_get("message").and_then(|b| b.as_binary()).and_then(|b| Message::decode(b).ok())).collect(),
                    _ => Vec::new(),
                };
                if items.is_empty() {
                    if stop.sleep(cfg.poll_delay) {
                        break;
                    }
                    continue;
                }
                for m in items {
                    if let Some(seq) = m.annotation("x-opt-sequence-number").and_then(|v| v.as_i64()) {
                        from = from.max(seq + 1);
                    }
                    let mut ex = Exchange::new(self.ctx.upgrade());
                    message_to_exchange(&m, cfg.binary, None, &mut ex);
                    if let Err(e) = self.processor.process(&mut ex) {
                        cameleer_core::camel_warn!(&self.uri, "Error processing exchange. Exchange[{}]. Caused by: [{} - {}]", ex.exchange_id, e.kind, e.message);
                    }
                }
            }
            conn.close();
            if !stop.is_stopped() && stop.sleep(cfg.recovery_interval) {
                return;
            }
        }
    }
}

impl Service for ServiceBusConsumer {
    fn start(&self) -> Result<()> {
        if self.threads.is_running() {
            return Ok(());
        }
        let me = ServiceBusConsumer { cfg: self.cfg.clone(), uri: self.uri.clone(), ctx: self.ctx.clone(), processor: self.processor.clone(), threads: BackgroundThreads::new() };
        let (tx, rx) = std::sync::mpsc::channel::<Result<()>>();
        self.threads.spawn(&format!("Camel {} consumer", self.uri), move |stop| {
            let mut ready = Some(tx);
            if me.cfg.peek {
                me.peek_loop(&stop, &mut ready);
            } else {
                me.receive_loop(&stop, &mut ready);
            }
        })?;
        if let Ok(Err(e)) = rx.recv_timeout(self.cfg.ns.request_timeout + Duration::from_secs(10)) {
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
    fn iso_times() {
        assert_eq!(parse_iso("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso("2024-02-29T12:34:56.789Z"), Some(1_709_210_096_789));
        assert_eq!(parse_iso("2024-02-29T13:34:56.789+01:00"), Some(1_709_210_096_789));
        assert_eq!(parse_time(&Value::Int(5)), Some(5));
        assert!(parse_iso("nope").is_none());
    }
}

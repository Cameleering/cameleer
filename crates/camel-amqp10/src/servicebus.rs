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
//! `sharedAccessKey` (rust-camel extension) work too.
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

use camel_core::component::support::BackgroundThreads;
use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::value::{ObjectMap, Value};

use crate::azure::AzureNamespace;
use crate::cbs::RequestResponse;
use crate::client::{Connection, Delivery, LinkOptions, Receiver, Sender, Session};
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
}

impl ProducerOperation {
    fn parse(s: &str) -> Result<ProducerOperation> {
        match s.trim() {
            "sendMessages" => Ok(ProducerOperation::SendMessages),
            "scheduleMessages" => Ok(ProducerOperation::ScheduleMessages),
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
        let (ns, entity_path) = AzureNamespace::resolve(ctx, scheme, &mut o, None)?;
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
        o.ignore(COMMON_IGNORED);
        o.ignore(&["maxAutoLockRenewDuration", "processorClient", "receiverAsyncClient", "senderAsyncClient", "serviceBusReceiveMode", "sessionEnabled", "serviceBusTransactionContext", "headerFilterStrategy", "sessionId"]);
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
        })
    }
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
            *g = Some(ProducerState { conn, session, sender: None, management: None });
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

impl Processor for ServiceBusProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let op = match hstr(ex, headers::PRODUCER_OPERATION) {
            Some(o) => ProducerOperation::parse(&o)?,
            None => self.cfg.producer_operation,
        };
        let msgs = Self::messages(ex);
        match op {
            ProducerOperation::SendMessages => self.with_state(|cfg, s| {
                if s.sender.is_none() {
                    s.sender = Some(s.session.sender(LinkOptions::new(&cfg.entity))?);
                }
                let snd = s.sender.as_ref().unwrap();
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

fn settle(cfg: &ServiceBusConfig, rx: &Receiver, d: &Delivery, ex: &Exchange) -> Result<()> {
    if d.settled || cfg.disable_auto_complete {
        return Ok(());
    }
    match &ex.exception {
        None => rx.accept(d),
        Some(e) if cfg.enable_dead_lettering => {
            let mut err = AmqpError::new("com.microsoft:dead-letter", "dead-lettered by the consumer");
            err.info = vec![
                (AmqpValue::sym("DeadLetterReason"), AmqpValue::str("ProcessingError")),
                (AmqpValue::sym("DeadLetterErrorDescription"), AmqpValue::str(format!("{}: {}", e.kind, e.message))),
            ];
            rx.settle(d, DeliveryState::Rejected(Some(err)))
        }
        Some(_) => rx.modify(d, false, false, vec![]),
    }
}

impl ServiceBusConsumer {
    fn receive_loop(&self, stop: &Arc<camel_core::component::support::StopFlag>, ready: &mut Option<std::sync::mpsc::Sender<Result<()>>>) {
        let cfg = &self.cfg;
        let address = cfg.receive_address();
        while !stop.is_stopped() {
            let setup = (|| -> Result<(Connection, Vec<Receiver>)> {
                let entity = match (&cfg.subscription, cfg.topic) {
                    (Some(s), true) => format!("{}/subscriptions/{}", cfg.entity, s),
                    _ => cfg.entity.clone(),
                };
                let (conn, session) = cfg.ns.connect(&[&entity])?;
                let mut rxs = Vec::new();
                for _ in 0..cfg.concurrent {
                    let mut o = LinkOptions::new(&address).credit(cfg.prefetch.max(1));
                    if cfg.receive_mode == ReceiveMode::ReceiveAndDelete {
                        o = o.at_most_once();
                    }
                    rxs.push(session.receiver(o)?);
                }
                Ok((conn, rxs))
            })();
            let (conn, rxs) = match setup {
                Ok(x) => x,
                Err(e) => {
                    camel_core::camel_warn!(&self.uri, "Cannot start consumer on {}: {} - retrying in {} ms", self.uri, e.message, cfg.recovery_interval.as_millis());
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
            std::thread::scope(|scope| {
                for rx in &rxs {
                    let st = stop.clone();
                    scope.spawn(move || loop {
                        if st.is_stopped() {
                            break;
                        }
                        match rx.receive(Duration::from_millis(50)) {
                            Ok(Some(d)) => {
                                let mut ex = Exchange::new(self.ctx.upgrade());
                                message_to_exchange(&d.message, cfg.binary, if d.settled { None } else { d.lock_token() }, &mut ex);
                                if let Err(e) = self.processor.process(&mut ex) {
                                    ex.set_exception(e);
                                }
                                if let Some(e) = &ex.exception {
                                    camel_core::camel_warn!(&self.uri, "Error processing exchange. Exchange[{}]. Caused by: [{} - {}]", ex.exchange_id, e.kind, e.message);
                                }
                                if let Err(e) = settle(cfg, rx, &d, &ex) {
                                    camel_core::camel_warn!(&self.uri, "Cannot settle message: {}", e.message);
                                }
                            }
                            Ok(None) => {}
                            Err(_) => break,
                        }
                    });
                }
            });
            let lost = !stop.is_stopped();
            conn.close();
            if lost {
                camel_core::camel_warn!(&self.uri, "Connection lost on {} - reconnecting in {} ms", self.uri, cfg.recovery_interval.as_millis());
                if stop.sleep(cfg.recovery_interval) {
                    return;
                }
            }
        }
    }

    fn peek_loop(&self, stop: &Arc<camel_core::component::support::StopFlag>, ready: &mut Option<std::sync::mpsc::Sender<Result<()>>>) {
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
                        camel_core::camel_warn!(&self.uri, "Error processing exchange. Exchange[{}]. Caused by: [{} - {}]", ex.exchange_id, e.kind, e.message);
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
    fn iso_times() {
        assert_eq!(parse_iso("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso("2024-02-29T12:34:56.789Z"), Some(1_709_210_096_789));
        assert_eq!(parse_iso("2024-02-29T13:34:56.789+01:00"), Some(1_709_210_096_789));
        assert_eq!(parse_time(&Value::Int(5)), Some(5));
        assert!(parse_iso("nope").is_none());
    }
}

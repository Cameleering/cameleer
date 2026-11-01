//! AMQP 1.0 transport (part 2), messaging terminus/outcome (part 3) and SASL (part 5)
//! types: performatives, frames, sources/targets, delivery states and errors.

use crate::codec::*;

/// Protocol header for plain AMQP (`AMQP 0 1.0.0`).
pub const AMQP_HEADER: [u8; 8] = *b"AMQP\x00\x01\x00\x00";
/// Protocol header for the SASL layer (`AMQP 3 1.0.0`).
pub const SASL_HEADER: [u8; 8] = *b"AMQP\x03\x01\x00\x00";
/// Protocol header for the TLS layer (`AMQP 2 1.0.0`, not supported: use `amqps`).
pub const TLS_HEADER: [u8; 8] = *b"AMQP\x02\x01\x00\x00";
/// Descriptor of JMS selector / Event Hubs position source filters.
pub const SELECTOR_FILTER: &str = "apache.org:selector-filter:string";
/// Smallest max-frame-size a peer may advertise.
pub const MIN_MAX_FRAME_SIZE: u32 = 512;

/// Descriptor codes.
pub mod desc {
    pub const OPEN: u64 = 0x10;
    pub const BEGIN: u64 = 0x11;
    pub const ATTACH: u64 = 0x12;
    pub const FLOW: u64 = 0x13;
    pub const TRANSFER: u64 = 0x14;
    pub const DISPOSITION: u64 = 0x15;
    pub const DETACH: u64 = 0x16;
    pub const END: u64 = 0x17;
    pub const CLOSE: u64 = 0x18;
    pub const ERROR: u64 = 0x1d;
    pub const RECEIVED: u64 = 0x23;
    pub const ACCEPTED: u64 = 0x24;
    pub const REJECTED: u64 = 0x25;
    pub const RELEASED: u64 = 0x26;
    pub const MODIFIED: u64 = 0x27;
    pub const SOURCE: u64 = 0x28;
    pub const TARGET: u64 = 0x29;
    pub const DELETE_ON_CLOSE: u64 = 0x2b;
    pub const COORDINATOR: u64 = 0x30;
    pub const DECLARE: u64 = 0x31;
    pub const DISCHARGE: u64 = 0x32;
    pub const DECLARED: u64 = 0x33;
    pub const TRANSACTIONAL_STATE: u64 = 0x34;
    pub const SASL_MECHANISMS: u64 = 0x40;
    pub const SASL_INIT: u64 = 0x41;
    pub const SASL_CHALLENGE: u64 = 0x42;
    pub const SASL_RESPONSE: u64 = 0x43;
    pub const SASL_OUTCOME: u64 = 0x44;
}

/// Standard error conditions.
pub mod cond {
    pub const INTERNAL_ERROR: &str = "amqp:internal-error";
    pub const NOT_FOUND: &str = "amqp:not-found";
    pub const UNAUTHORIZED_ACCESS: &str = "amqp:unauthorized-access";
    pub const DECODE_ERROR: &str = "amqp:decode-error";
    pub const RESOURCE_LIMIT_EXCEEDED: &str = "amqp:resource-limit-exceeded";
    pub const NOT_ALLOWED: &str = "amqp:not-allowed";
    pub const INVALID_FIELD: &str = "amqp:invalid-field";
    pub const NOT_IMPLEMENTED: &str = "amqp:not-implemented";
    pub const PRECONDITION_FAILED: &str = "amqp:precondition-failed";
    pub const CONNECTION_FORCED: &str = "amqp:connection:forced";
    pub const FRAMING_ERROR: &str = "amqp:connection:framing-error";
    pub const WINDOW_VIOLATION: &str = "amqp:session:window-violation";
    pub const DETACH_FORCED: &str = "amqp:link:detach-forced";
    pub const TRANSFER_LIMIT_EXCEEDED: &str = "amqp:link:transfer-limit-exceeded";
    pub const MESSAGE_SIZE_EXCEEDED: &str = "amqp:link:message-size-exceeded";
    pub const STOLEN: &str = "amqp:link:stolen";
    pub const TRANSACTION_UNKNOWN_ID: &str = "amqp:transaction:unknown-id";
    pub const TRANSACTION_ROLLBACK: &str = "amqp:transaction:rollback";
    pub const TRANSACTION_TIMEOUT: &str = "amqp:transaction:timeout";
}

/// An AMQP error (`amqp:error:list`).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct AmqpError {
    pub condition: String,
    pub description: Option<String>,
    pub info: Vec<(AmqpValue, AmqpValue)>,
}

impl AmqpError {
    pub fn new(condition: &str, description: impl Into<String>) -> Self {
        AmqpError { condition: condition.to_string(), description: Some(description.into()), info: Vec::new() }
    }
    pub fn to_value(&self) -> AmqpValue {
        AmqpValue::composite(desc::ERROR, vec![AmqpValue::sym(self.condition.clone()), opt_str(&self.description), map_value(&self.info)])
    }
    pub fn from_value(v: &AmqpValue) -> Option<AmqpError> {
        if v.descriptor_code() != Some(desc::ERROR) {
            return None;
        }
        let f = v.undescribed().as_list().unwrap_or(&[]);
        Some(AmqpError {
            condition: field(f, 0).as_str().unwrap_or("").to_string(),
            description: field(f, 1).as_str().map(String::from),
            info: map_of(field(f, 2)),
        })
    }
}

impl std::fmt::Display for AmqpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.condition)?;
        if let Some(d) = &self.description {
            write!(f, ": {}", d)?;
        }
        Ok(())
    }
}

fn opt_error(v: &AmqpValue) -> Option<AmqpError> {
    AmqpError::from_value(v)
}
fn error_value(e: &Option<AmqpError>) -> AmqpValue {
    e.as_ref().map(|e| e.to_value()).unwrap_or(AmqpValue::Null)
}

/// Link role.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Sender,
    Receiver,
}

impl Role {
    fn value(self) -> AmqpValue {
        AmqpValue::Boolean(self == Role::Receiver)
    }
    fn from(v: &AmqpValue) -> Role {
        if v.as_bool() == Some(true) {
            Role::Receiver
        } else {
            Role::Sender
        }
    }
}

/// Sender settle mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SndSettleMode {
    /// Deliveries are sent unsettled (at-least-once).
    Unsettled = 0,
    /// Deliveries are sent pre-settled (at-most-once).
    Settled = 1,
    Mixed = 2,
}

impl SndSettleMode {
    fn from(v: &AmqpValue) -> SndSettleMode {
        match v.as_u64() {
            Some(0) => SndSettleMode::Unsettled,
            Some(1) => SndSettleMode::Settled,
            _ => SndSettleMode::Mixed,
        }
    }
}

/// Receiver settle mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcvSettleMode {
    /// The receiver settles spontaneously.
    First = 0,
    /// The receiver settles after the sender settled.
    Second = 1,
}

/// `amqp:open:list`.
#[derive(Clone, Debug, PartialEq)]
pub struct Open {
    pub container_id: String,
    pub hostname: Option<String>,
    pub max_frame_size: u32,
    pub channel_max: u16,
    /// Milliseconds (0 = none).
    pub idle_time_out: u32,
    pub offered_capabilities: Vec<String>,
    pub desired_capabilities: Vec<String>,
    pub properties: Vec<(AmqpValue, AmqpValue)>,
}

impl Default for Open {
    fn default() -> Self {
        Open {
            container_id: String::new(),
            hostname: None,
            max_frame_size: u32::MAX,
            channel_max: u16::MAX,
            idle_time_out: 0,
            offered_capabilities: Vec::new(),
            desired_capabilities: Vec::new(),
            properties: Vec::new(),
        }
    }
}

/// `amqp:begin:list`.
#[derive(Clone, Debug, PartialEq)]
pub struct Begin {
    pub remote_channel: Option<u16>,
    pub next_outgoing_id: u32,
    pub incoming_window: u32,
    pub outgoing_window: u32,
    pub handle_max: u32,
    pub properties: Vec<(AmqpValue, AmqpValue)>,
}

/// `amqp:source:list`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Source {
    pub address: Option<String>,
    pub durable: u32,
    pub expiry_policy: Option<String>,
    pub timeout: u32,
    pub dynamic: bool,
    pub dynamic_node_properties: Vec<(AmqpValue, AmqpValue)>,
    pub distribution_mode: Option<String>,
    pub filter: Vec<(AmqpValue, AmqpValue)>,
    pub default_outcome: Option<AmqpValue>,
    pub outcomes: Vec<String>,
    pub capabilities: Vec<String>,
}

impl Source {
    pub fn to_value(&self) -> AmqpValue {
        AmqpValue::composite(
            desc::SOURCE,
            vec![
                opt_str(&self.address),
                if self.durable == 0 { AmqpValue::Null } else { AmqpValue::Uint(self.durable) },
                opt_sym(&self.expiry_policy),
                if self.timeout == 0 { AmqpValue::Null } else { AmqpValue::Uint(self.timeout) },
                if self.dynamic { AmqpValue::Boolean(true) } else { AmqpValue::Null },
                map_value(&self.dynamic_node_properties),
                opt_sym(&self.distribution_mode),
                map_value(&self.filter),
                self.default_outcome.clone().unwrap_or(AmqpValue::Null),
                symbols_value(&self.outcomes),
                symbols_value(&self.capabilities),
            ],
        )
    }
    pub fn from_value(v: &AmqpValue) -> Option<Source> {
        if v.descriptor_code() != Some(desc::SOURCE) {
            return None;
        }
        let f = v.undescribed().as_list().unwrap_or(&[]);
        Some(Source {
            address: field(f, 0).as_str().map(String::from),
            durable: field(f, 1).as_u32().unwrap_or(0),
            expiry_policy: field(f, 2).as_str().map(String::from),
            timeout: field(f, 3).as_u32().unwrap_or(0),
            dynamic: field(f, 4).as_bool().unwrap_or(false),
            dynamic_node_properties: map_of(field(f, 5)),
            distribution_mode: field(f, 6).as_str().map(String::from),
            filter: map_of(field(f, 7)),
            default_outcome: Some(field(f, 8).clone()).filter(|v| !v.is_null()),
            outcomes: symbols(field(f, 9)),
            capabilities: symbols(field(f, 10)),
        })
    }
}

/// `amqp:target:list`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Target {
    /// A transaction coordinator (`amqp:coordinator:list`) instead of a node; only
    /// `capabilities` apply.
    pub coordinator: bool,
    pub address: Option<String>,
    pub durable: u32,
    pub expiry_policy: Option<String>,
    pub timeout: u32,
    pub dynamic: bool,
    pub dynamic_node_properties: Vec<(AmqpValue, AmqpValue)>,
    pub capabilities: Vec<String>,
}

impl Target {
    /// A transaction coordinator target offering local transactions.
    pub fn coordinator() -> Target {
        Target { coordinator: true, capabilities: vec!["amqp:local-transactions".into()], ..Default::default() }
    }
    pub fn to_value(&self) -> AmqpValue {
        if self.coordinator {
            return AmqpValue::composite(desc::COORDINATOR, vec![symbols_value(&self.capabilities)]);
        }
        AmqpValue::composite(
            desc::TARGET,
            vec![
                opt_str(&self.address),
                if self.durable == 0 { AmqpValue::Null } else { AmqpValue::Uint(self.durable) },
                opt_sym(&self.expiry_policy),
                if self.timeout == 0 { AmqpValue::Null } else { AmqpValue::Uint(self.timeout) },
                if self.dynamic { AmqpValue::Boolean(true) } else { AmqpValue::Null },
                map_value(&self.dynamic_node_properties),
                symbols_value(&self.capabilities),
            ],
        )
    }
    pub fn from_value(v: &AmqpValue) -> Option<Target> {
        if v.descriptor_code() == Some(desc::COORDINATOR) {
            let f = v.undescribed().as_list().unwrap_or(&[]);
            return Some(Target { coordinator: true, capabilities: symbols(field(f, 0)), ..Default::default() });
        }
        if v.descriptor_code() != Some(desc::TARGET) {
            return None;
        }
        let f = v.undescribed().as_list().unwrap_or(&[]);
        Some(Target {
            coordinator: false,
            address: field(f, 0).as_str().map(String::from),
            durable: field(f, 1).as_u32().unwrap_or(0),
            expiry_policy: field(f, 2).as_str().map(String::from),
            timeout: field(f, 3).as_u32().unwrap_or(0),
            dynamic: field(f, 4).as_bool().unwrap_or(false),
            dynamic_node_properties: map_of(field(f, 5)),
            capabilities: symbols(field(f, 6)),
        })
    }
}

/// `amqp:attach:list`.
#[derive(Clone, Debug, PartialEq)]
pub struct Attach {
    pub name: String,
    pub handle: u32,
    pub role: Role,
    pub snd_settle_mode: SndSettleMode,
    pub rcv_settle_mode: RcvSettleMode,
    pub source: Option<Source>,
    pub target: Option<Target>,
    pub initial_delivery_count: Option<u32>,
    pub max_message_size: Option<u64>,
    pub offered_capabilities: Vec<String>,
    pub desired_capabilities: Vec<String>,
    pub properties: Vec<(AmqpValue, AmqpValue)>,
}

/// `amqp:flow:list`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Flow {
    pub next_incoming_id: Option<u32>,
    pub incoming_window: u32,
    pub next_outgoing_id: u32,
    pub outgoing_window: u32,
    pub handle: Option<u32>,
    pub delivery_count: Option<u32>,
    pub link_credit: Option<u32>,
    pub available: Option<u32>,
    pub drain: bool,
    pub echo: bool,
    pub properties: Vec<(AmqpValue, AmqpValue)>,
}

/// Delivery state / outcome.
#[derive(Clone, Debug, PartialEq)]
pub enum DeliveryState {
    Received { section_number: u32, section_offset: u64 },
    Accepted,
    Rejected(Option<AmqpError>),
    Released,
    Modified { delivery_failed: bool, undeliverable_here: bool, message_annotations: Vec<(AmqpValue, AmqpValue)> },
    /// `declared` outcome of a transaction declaration (the transaction id).
    Declared(Vec<u8>),
    /// `transactional-state`: a transfer or disposition within a transaction, with the
    /// provisional outcome.
    Transactional { txn_id: Vec<u8>, outcome: Option<Box<DeliveryState>> },
    /// Any other state, kept raw.
    Other(AmqpValue),
}

impl DeliveryState {
    pub fn to_value(&self) -> AmqpValue {
        match self {
            DeliveryState::Received { section_number, section_offset } => {
                AmqpValue::composite(desc::RECEIVED, vec![AmqpValue::Uint(*section_number), AmqpValue::Ulong(*section_offset)])
            }
            DeliveryState::Accepted => AmqpValue::composite(desc::ACCEPTED, vec![]),
            DeliveryState::Rejected(e) => AmqpValue::composite(desc::REJECTED, vec![error_value(e)]),
            DeliveryState::Released => AmqpValue::composite(desc::RELEASED, vec![]),
            DeliveryState::Modified { delivery_failed, undeliverable_here, message_annotations } => AmqpValue::composite(
                desc::MODIFIED,
                vec![AmqpValue::Boolean(*delivery_failed), AmqpValue::Boolean(*undeliverable_here), map_value(message_annotations)],
            ),
            DeliveryState::Declared(id) => AmqpValue::composite(desc::DECLARED, vec![AmqpValue::Binary(id.clone())]),
            DeliveryState::Transactional { txn_id, outcome } => {
                AmqpValue::composite(desc::TRANSACTIONAL_STATE, vec![AmqpValue::Binary(txn_id.clone()), outcome.as_ref().map(|o| o.to_value()).unwrap_or(AmqpValue::Null)])
            }
            DeliveryState::Other(v) => v.clone(),
        }
    }
    pub fn from_value(v: &AmqpValue) -> Option<DeliveryState> {
        if v.is_null() {
            return None;
        }
        let f = v.undescribed().as_list().unwrap_or(&[]);
        Some(match v.descriptor_code() {
            Some(desc::RECEIVED) => DeliveryState::Received { section_number: field(f, 0).as_u32().unwrap_or(0), section_offset: field(f, 1).as_u64().unwrap_or(0) },
            Some(desc::ACCEPTED) => DeliveryState::Accepted,
            Some(desc::REJECTED) => DeliveryState::Rejected(opt_error(field(f, 0))),
            Some(desc::RELEASED) => DeliveryState::Released,
            Some(desc::MODIFIED) => DeliveryState::Modified {
                delivery_failed: field(f, 0).as_bool().unwrap_or(false),
                undeliverable_here: field(f, 1).as_bool().unwrap_or(false),
                message_annotations: map_of(field(f, 2)),
            },
            Some(desc::DECLARED) => DeliveryState::Declared(field(f, 0).as_binary().unwrap_or(&[]).to_vec()),
            Some(desc::TRANSACTIONAL_STATE) => DeliveryState::Transactional {
                txn_id: field(f, 0).as_binary().unwrap_or(&[]).to_vec(),
                outcome: DeliveryState::from_value(field(f, 1)).map(Box::new),
            },
            _ => DeliveryState::Other(v.clone()),
        })
    }
    /// Whether this is a terminal outcome (a transactional state only with its outcome).
    pub fn is_outcome(&self) -> bool {
        match self {
            DeliveryState::Received { .. } => false,
            DeliveryState::Transactional { outcome, .. } => outcome.is_some(),
            _ => true,
        }
    }
    /// The outcome itself: the provisional outcome of a transactional state.
    pub fn outcome(&self) -> &DeliveryState {
        match self {
            DeliveryState::Transactional { outcome: Some(o), .. } => o.outcome(),
            other => other,
        }
    }
}

/// `amqp:transfer:list`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Transfer {
    pub handle: u32,
    pub delivery_id: Option<u32>,
    pub delivery_tag: Option<Vec<u8>>,
    pub message_format: Option<u32>,
    pub settled: Option<bool>,
    pub more: bool,
    pub rcv_settle_mode: Option<RcvSettleMode>,
    pub state: Option<DeliveryState>,
    pub resume: bool,
    pub aborted: bool,
    pub batchable: bool,
}

/// `amqp:disposition:list`.
#[derive(Clone, Debug, PartialEq)]
pub struct Disposition {
    pub role: Role,
    pub first: u32,
    pub last: Option<u32>,
    pub settled: bool,
    pub state: Option<DeliveryState>,
    pub batchable: bool,
}

/// `amqp:detach:list`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Detach {
    pub handle: u32,
    pub closed: bool,
    pub error: Option<AmqpError>,
}

/// A performative (frame body of an AMQP frame).
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum Performative {
    Open(Open),
    Begin(Begin),
    Attach(Attach),
    Flow(Flow),
    Transfer(Transfer),
    Disposition(Disposition),
    Detach(Detach),
    End(Option<AmqpError>),
    Close(Option<AmqpError>),
}

fn bool_or_null(b: bool) -> AmqpValue {
    if b {
        AmqpValue::Boolean(true)
    } else {
        AmqpValue::Null
    }
}

impl Performative {
    pub fn name(&self) -> &'static str {
        match self {
            Performative::Open(_) => "open",
            Performative::Begin(_) => "begin",
            Performative::Attach(_) => "attach",
            Performative::Flow(_) => "flow",
            Performative::Transfer(_) => "transfer",
            Performative::Disposition(_) => "disposition",
            Performative::Detach(_) => "detach",
            Performative::End(_) => "end",
            Performative::Close(_) => "close",
        }
    }

    pub fn to_value(&self) -> AmqpValue {
        use AmqpValue as V;
        match self {
            Performative::Open(o) => V::composite(
                desc::OPEN,
                vec![
                    V::str(o.container_id.clone()),
                    opt_str(&o.hostname),
                    if o.max_frame_size == u32::MAX { V::Null } else { V::Uint(o.max_frame_size) },
                    if o.channel_max == u16::MAX { V::Null } else { V::Ushort(o.channel_max) },
                    if o.idle_time_out == 0 { V::Null } else { V::Uint(o.idle_time_out) },
                    V::Null,
                    V::Null,
                    symbols_value(&o.offered_capabilities),
                    symbols_value(&o.desired_capabilities),
                    map_value(&o.properties),
                ],
            ),
            Performative::Begin(b) => V::composite(
                desc::BEGIN,
                vec![
                    b.remote_channel.map(V::Ushort).unwrap_or(V::Null),
                    V::Uint(b.next_outgoing_id),
                    V::Uint(b.incoming_window),
                    V::Uint(b.outgoing_window),
                    if b.handle_max == u32::MAX { V::Null } else { V::Uint(b.handle_max) },
                    V::Null,
                    V::Null,
                    map_value(&b.properties),
                ],
            ),
            Performative::Attach(a) => V::composite(
                desc::ATTACH,
                vec![
                    V::str(a.name.clone()),
                    V::Uint(a.handle),
                    a.role.value(),
                    V::Ubyte(a.snd_settle_mode as u8),
                    V::Ubyte(a.rcv_settle_mode as u8),
                    a.source.as_ref().map(|s| s.to_value()).unwrap_or(V::Null),
                    a.target.as_ref().map(|t| t.to_value()).unwrap_or(V::Null),
                    V::Null,
                    V::Null,
                    opt_uint(a.initial_delivery_count),
                    a.max_message_size.map(V::Ulong).unwrap_or(V::Null),
                    symbols_value(&a.offered_capabilities),
                    symbols_value(&a.desired_capabilities),
                    map_value(&a.properties),
                ],
            ),
            Performative::Flow(f) => V::composite(
                desc::FLOW,
                vec![
                    opt_uint(f.next_incoming_id),
                    V::Uint(f.incoming_window),
                    V::Uint(f.next_outgoing_id),
                    V::Uint(f.outgoing_window),
                    opt_uint(f.handle),
                    opt_uint(f.delivery_count),
                    opt_uint(f.link_credit),
                    opt_uint(f.available),
                    bool_or_null(f.drain),
                    bool_or_null(f.echo),
                    map_value(&f.properties),
                ],
            ),
            Performative::Transfer(t) => V::composite(
                desc::TRANSFER,
                vec![
                    V::Uint(t.handle),
                    opt_uint(t.delivery_id),
                    t.delivery_tag.clone().map(V::Binary).unwrap_or(V::Null),
                    opt_uint(t.message_format),
                    t.settled.map(V::Boolean).unwrap_or(V::Null),
                    // always explicit so all frames of a delivery have the same size
                    V::Boolean(t.more),
                    t.rcv_settle_mode.map(|m| V::Ubyte(m as u8)).unwrap_or(V::Null),
                    t.state.as_ref().map(|s| s.to_value()).unwrap_or(V::Null),
                    bool_or_null(t.resume),
                    bool_or_null(t.aborted),
                    bool_or_null(t.batchable),
                ],
            ),
            Performative::Disposition(d) => V::composite(
                desc::DISPOSITION,
                vec![
                    d.role.value(),
                    V::Uint(d.first),
                    opt_uint(d.last),
                    V::Boolean(d.settled),
                    d.state.as_ref().map(|s| s.to_value()).unwrap_or(V::Null),
                    bool_or_null(d.batchable),
                ],
            ),
            Performative::Detach(d) => V::composite(desc::DETACH, vec![V::Uint(d.handle), V::Boolean(d.closed), error_value(&d.error)]),
            Performative::End(e) => V::composite(desc::END, vec![error_value(e)]),
            Performative::Close(e) => V::composite(desc::CLOSE, vec![error_value(e)]),
        }
    }

    pub fn from_value(v: &AmqpValue) -> DResult<Performative> {
        let code = v.descriptor_code().ok_or_else(|| DecodeError("frame body is not a described type".into()))?;
        let f: &[AmqpValue] = v.undescribed().as_list().unwrap_or(&[]);
        let req_u32 = |i: usize, what: &str| field(f, i).as_u32().ok_or_else(|| DecodeError(format!("mandatory field {} missing", what)));
        Ok(match code {
            desc::OPEN => Performative::Open(Open {
                container_id: field(f, 0).as_str().ok_or_else(|| DecodeError("open without container-id".into()))?.to_string(),
                hostname: field(f, 1).as_str().map(String::from),
                max_frame_size: field(f, 2).as_u32().unwrap_or(u32::MAX),
                channel_max: field(f, 3).as_u64().map(|v| v.min(65535) as u16).unwrap_or(u16::MAX),
                idle_time_out: field(f, 4).as_u32().unwrap_or(0),
                offered_capabilities: symbols(field(f, 7)),
                desired_capabilities: symbols(field(f, 8)),
                properties: map_of(field(f, 9)),
            }),
            desc::BEGIN => Performative::Begin(Begin {
                remote_channel: field(f, 0).as_u64().map(|v| v as u16),
                next_outgoing_id: req_u32(1, "next-outgoing-id")?,
                incoming_window: req_u32(2, "incoming-window")?,
                outgoing_window: req_u32(3, "outgoing-window")?,
                handle_max: field(f, 4).as_u32().unwrap_or(u32::MAX),
                properties: map_of(field(f, 7)),
            }),
            desc::ATTACH => Performative::Attach(Attach {
                name: field(f, 0).as_str().ok_or_else(|| DecodeError("attach without name".into()))?.to_string(),
                handle: req_u32(1, "handle")?,
                role: Role::from(field(f, 2)),
                snd_settle_mode: SndSettleMode::from(field(f, 3)),
                rcv_settle_mode: if field(f, 4).as_u64() == Some(1) { RcvSettleMode::Second } else { RcvSettleMode::First },
                source: Source::from_value(field(f, 5)),
                target: Target::from_value(field(f, 6)),
                initial_delivery_count: field(f, 9).as_u32(),
                max_message_size: field(f, 10).as_u64(),
                offered_capabilities: symbols(field(f, 11)),
                desired_capabilities: symbols(field(f, 12)),
                properties: map_of(field(f, 13)),
            }),
            desc::FLOW => Performative::Flow(Flow {
                next_incoming_id: field(f, 0).as_u32(),
                incoming_window: req_u32(1, "incoming-window")?,
                next_outgoing_id: req_u32(2, "next-outgoing-id")?,
                outgoing_window: req_u32(3, "outgoing-window")?,
                handle: field(f, 4).as_u32(),
                delivery_count: field(f, 5).as_u32(),
                link_credit: field(f, 6).as_u32(),
                available: field(f, 7).as_u32(),
                drain: field(f, 8).as_bool().unwrap_or(false),
                echo: field(f, 9).as_bool().unwrap_or(false),
                properties: map_of(field(f, 10)),
            }),
            desc::TRANSFER => Performative::Transfer(Transfer {
                handle: req_u32(0, "handle")?,
                delivery_id: field(f, 1).as_u32(),
                delivery_tag: field(f, 2).as_binary().map(|b| b.to_vec()),
                message_format: field(f, 3).as_u32(),
                settled: field(f, 4).as_bool(),
                more: field(f, 5).as_bool().unwrap_or(false),
                rcv_settle_mode: field(f, 6).as_u64().map(|v| if v == 1 { RcvSettleMode::Second } else { RcvSettleMode::First }),
                state: DeliveryState::from_value(field(f, 7)),
                resume: field(f, 8).as_bool().unwrap_or(false),
                aborted: field(f, 9).as_bool().unwrap_or(false),
                batchable: field(f, 10).as_bool().unwrap_or(false),
            }),
            desc::DISPOSITION => Performative::Disposition(Disposition {
                role: Role::from(field(f, 0)),
                first: req_u32(1, "first")?,
                last: field(f, 2).as_u32(),
                settled: field(f, 3).as_bool().unwrap_or(false),
                state: DeliveryState::from_value(field(f, 4)),
                batchable: field(f, 5).as_bool().unwrap_or(false),
            }),
            desc::DETACH => Performative::Detach(Detach { handle: req_u32(0, "handle")?, closed: field(f, 1).as_bool().unwrap_or(false), error: opt_error(field(f, 2)) }),
            desc::END => Performative::End(opt_error(field(f, 0))),
            desc::CLOSE => Performative::Close(opt_error(field(f, 0))),
            other => return Err(DecodeError(format!("unknown performative 0x{:x}", other))),
        })
    }
}

/// SASL frame bodies.
#[derive(Clone, Debug, PartialEq)]
pub enum SaslFrame {
    Mechanisms(Vec<String>),
    Init { mechanism: String, initial_response: Option<Vec<u8>>, hostname: Option<String> },
    Challenge(Vec<u8>),
    Response(Vec<u8>),
    /// code: 0 ok, 1 auth, 2 sys, 3 sys-perm, 4 sys-temp.
    Outcome { code: u8, additional_data: Option<Vec<u8>> },
}

impl SaslFrame {
    pub fn to_value(&self) -> AmqpValue {
        use AmqpValue as V;
        match self {
            SaslFrame::Mechanisms(m) => V::composite(desc::SASL_MECHANISMS, vec![V::Array(m.iter().map(|s| V::sym(s.clone())).collect())]),
            SaslFrame::Init { mechanism, initial_response, hostname } => V::composite(
                desc::SASL_INIT,
                vec![V::sym(mechanism.clone()), initial_response.clone().map(V::Binary).unwrap_or(V::Null), opt_str(hostname)],
            ),
            SaslFrame::Challenge(c) => V::composite(desc::SASL_CHALLENGE, vec![V::Binary(c.clone())]),
            SaslFrame::Response(r) => V::composite(desc::SASL_RESPONSE, vec![V::Binary(r.clone())]),
            SaslFrame::Outcome { code, additional_data } => {
                V::composite(desc::SASL_OUTCOME, vec![V::Ubyte(*code), additional_data.clone().map(V::Binary).unwrap_or(V::Null)])
            }
        }
    }
    pub fn from_value(v: &AmqpValue) -> DResult<SaslFrame> {
        let f: &[AmqpValue] = v.undescribed().as_list().unwrap_or(&[]);
        Ok(match v.descriptor_code() {
            Some(desc::SASL_MECHANISMS) => SaslFrame::Mechanisms(symbols(field(f, 0))),
            Some(desc::SASL_INIT) => SaslFrame::Init {
                mechanism: field(f, 0).as_str().unwrap_or("").to_string(),
                initial_response: field(f, 1).as_binary().map(|b| b.to_vec()),
                hostname: field(f, 2).as_str().map(String::from),
            },
            Some(desc::SASL_CHALLENGE) => SaslFrame::Challenge(field(f, 0).as_binary().unwrap_or(&[]).to_vec()),
            Some(desc::SASL_RESPONSE) => SaslFrame::Response(field(f, 0).as_binary().unwrap_or(&[]).to_vec()),
            Some(desc::SASL_OUTCOME) => SaslFrame::Outcome { code: field(f, 0).as_u64().unwrap_or(1) as u8, additional_data: field(f, 1).as_binary().map(|b| b.to_vec()) },
            _ => return Err(DecodeError("unknown SASL frame".into())),
        })
    }
}

/// A decoded frame.
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum FrameBody {
    /// An empty frame (heartbeat).
    Empty,
    /// An AMQP performative plus the payload following it (transfer frames).
    Amqp(Performative, Vec<u8>),
    Sasl(SaslFrame),
}

/// A frame on a channel.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub channel: u16,
    pub body: FrameBody,
}

/// Frame type octet for AMQP frames.
pub const FRAME_AMQP: u8 = 0;
/// Frame type octet for SASL frames.
pub const FRAME_SASL: u8 = 1;

/// Encode a frame: 8-byte header (size, doff=2, type, channel) + body + payload.
pub fn encode_frame(frame_type: u8, channel: u16, body: Option<&AmqpValue>, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0, 0, 0, 0, 2, frame_type];
    out.extend_from_slice(&channel.to_be_bytes());
    if let Some(b) = body {
        b.encode_to(&mut out);
    }
    out.extend_from_slice(payload);
    let len = out.len() as u32;
    out[..4].copy_from_slice(&len.to_be_bytes());
    out
}

impl Frame {
    /// An AMQP frame without payload.
    pub fn amqp(channel: u16, p: Performative) -> Frame {
        Frame { channel, body: FrameBody::Amqp(p, Vec::new()) }
    }
    /// Encode the frame.
    pub fn encode(&self) -> Vec<u8> {
        match &self.body {
            FrameBody::Empty => encode_frame(FRAME_AMQP, self.channel, None, &[]),
            FrameBody::Amqp(p, payload) => encode_frame(FRAME_AMQP, self.channel, Some(&p.to_value()), payload),
            FrameBody::Sasl(s) => encode_frame(FRAME_SASL, 0, Some(&s.to_value()), &[]),
        }
    }
    /// Decode a frame from its full bytes (header included).
    pub fn decode(bytes: &[u8]) -> DResult<Frame> {
        if bytes.len() < 8 {
            return Err(DecodeError("frame shorter than its header".into()));
        }
        let doff = bytes[4] as usize * 4;
        if doff < 8 || doff > bytes.len() {
            return Err(DecodeError(format!("invalid data offset {}", doff)));
        }
        let channel = u16::from_be_bytes([bytes[6], bytes[7]]);
        let body = &bytes[doff..];
        if body.is_empty() {
            return Ok(Frame { channel, body: FrameBody::Empty });
        }
        let mut d = Decoder::new(body);
        let v = d.read_value()?;
        let body = match bytes[5] {
            FRAME_AMQP => FrameBody::Amqp(Performative::from_value(&v)?, d.rest().to_vec()),
            FRAME_SASL => FrameBody::Sasl(SaslFrame::from_value(&v)?),
            t => return Err(DecodeError(format!("unknown frame type {}", t))),
        };
        Ok(Frame { channel, body })
    }
}

/// Read one frame from `r`. `Ok(None)` on a clean end of stream before a frame starts.
pub fn read_frame(r: &mut impl std::io::Read, max_frame_size: u32) -> std::io::Result<Option<Frame>> {
    let mut hdr = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        match r.read(&mut hdr[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "connection closed inside a frame")),
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let size = u32::from_be_bytes(hdr);
    if size < 8 || size > max_frame_size.max(MIN_MAX_FRAME_SIZE) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("invalid frame size {} (max {})", size, max_frame_size)));
    }
    let mut buf = vec![0u8; size as usize];
    buf[..4].copy_from_slice(&hdr);
    r.read_exact(&mut buf[4..])?;
    Frame::decode(&buf).map(Some).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.0))
}

/// Read an 8-byte protocol header.
pub fn read_protocol_header(r: &mut impl std::io::Read) -> std::io::Result<[u8; 8]> {
    let mut h = [0u8; 8];
    r.read_exact(&mut h)?;
    Ok(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn performatives_round_trip() {
        let attach = Attach {
            name: "l".into(),
            handle: 3,
            role: Role::Receiver,
            snd_settle_mode: SndSettleMode::Settled,
            rcv_settle_mode: RcvSettleMode::First,
            source: Some(Source {
                address: Some("q".into()),
                filter: vec![(AmqpValue::sym("jms-selector"), AmqpValue::Described(Box::new(AmqpValue::sym("apache.org:selector-filter:string")), Box::new(AmqpValue::str("a = 1"))))],
                capabilities: vec!["queue".into()],
                ..Default::default()
            }),
            target: Some(Target::default()),
            initial_delivery_count: None,
            max_message_size: Some(1024),
            offered_capabilities: vec![],
            desired_capabilities: vec![],
            properties: vec![],
        };
        let ps = vec![
            Performative::Open(Open { container_id: "c".into(), hostname: Some("h".into()), max_frame_size: 65536, idle_time_out: 1000, ..Default::default() }),
            Performative::Begin(Begin { remote_channel: Some(1), next_outgoing_id: 1, incoming_window: 10, outgoing_window: 20, handle_max: 7, properties: vec![] }),
            Performative::Attach(attach),
            Performative::Flow(Flow { next_incoming_id: Some(1), incoming_window: 5, next_outgoing_id: 2, outgoing_window: 3, handle: Some(0), delivery_count: Some(0), link_credit: Some(10), drain: true, ..Default::default() }),
            Performative::Transfer(Transfer { handle: 0, delivery_id: Some(5), delivery_tag: Some(vec![1]), message_format: Some(0), settled: Some(false), more: true, ..Default::default() }),
            Performative::Disposition(Disposition { role: Role::Receiver, first: 1, last: Some(4), settled: true, state: Some(DeliveryState::Rejected(Some(AmqpError::new(cond::NOT_ALLOWED, "no")))), batchable: false }),
            Performative::Disposition(Disposition {
                role: Role::Sender,
                first: 1,
                last: None,
                settled: false,
                state: Some(DeliveryState::Modified { delivery_failed: true, undeliverable_here: false, message_annotations: vec![] }),
                batchable: false,
            }),
            Performative::Detach(Detach { handle: 1, closed: true, error: None }),
            Performative::End(None),
            Performative::Close(Some(AmqpError::new(cond::CONNECTION_FORCED, "bye"))),
        ];
        for p in ps {
            let f = Frame { channel: 7, body: FrameBody::Amqp(p.clone(), vec![9, 9]) };
            let bytes = f.encode();
            let back = Frame::decode(&bytes).unwrap();
            assert_eq!(back, f, "{}", p.name());
            let back2 = read_frame(&mut &bytes[..], 1 << 20).unwrap().unwrap();
            assert_eq!(back2, f);
        }
        for s in [
            SaslFrame::Mechanisms(vec!["PLAIN".into(), "ANONYMOUS".into()]),
            SaslFrame::Init { mechanism: "PLAIN".into(), initial_response: Some(b"\0u\0p".to_vec()), hostname: Some("h".into()) },
            SaslFrame::Challenge(vec![1]),
            SaslFrame::Response(vec![2]),
            SaslFrame::Outcome { code: 0, additional_data: None },
        ] {
            let f = Frame { channel: 0, body: FrameBody::Sasl(s) };
            assert_eq!(Frame::decode(&f.encode()).unwrap(), f);
        }
    }

    #[test]
    fn heartbeat_frame_bytes() {
        let f = Frame { channel: 0, body: FrameBody::Empty };
        assert_eq!(f.encode(), [0, 0, 0, 8, 2, 0, 0, 0]);
        assert_eq!(Frame::decode(&[0, 0, 0, 8, 2, 0, 0, 0]).unwrap(), f);
        // close frame with no error: 00 53 18 45
        let c = Frame::amqp(0, Performative::Close(None)).encode();
        assert_eq!(c, [0, 0, 0, 12, 2, 0, 0, 0, 0x00, 0x53, 0x18, 0x45]);
        assert!(read_frame(&mut &[0u8, 0, 0, 4][..], 1024).is_err());
    }
}

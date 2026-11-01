//! AMQP 1.0 client: connection (protocol header negotiation, SASL, open/close, idle
//! timeout + heartbeats, TLS), sessions (begin/end, incoming/outgoing windows) and links
//! (attach/detach, credit based flow control, multi-frame transfers, settlement).
//!
//! ```no_run
//! use camel_amqp10::client::{Connection, ConnectionOptions, LinkOptions};
//! use camel_amqp10::message::Message;
//! use std::time::Duration;
//! let conn = Connection::open(&ConnectionOptions::new("localhost", 5672).credentials("guest", "guest")).unwrap();
//! let session = conn.session().unwrap();
//! let sender = session.sender(LinkOptions::new("orders")).unwrap();
//! sender.send(&Message::text("hello"), Duration::from_secs(5)).unwrap();
//! let receiver = session.receiver(LinkOptions::new("orders").credit(10)).unwrap();
//! let d = receiver.receive(Duration::from_secs(5)).unwrap().unwrap();
//! receiver.accept(&d).unwrap();
//! conn.close();
//! ```

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use camel_core::error::{kinds as core_kinds, CamelError, Result};
use camel_tls::camel_support::EndpointTls;
use camel_tls::net::NetStream;

use crate::codec::AmqpValue;
use crate::message::Message;
use crate::transport::Transport;
use crate::types::*;
use crate::util::{amqp_error, amqp_failure, kinds, register_kinds};
use crate::ws::AmqpStream;

/// The session incoming window we advertise (in transfer frames).
const INCOMING_WINDOW: u32 = 2048;
/// The session outgoing window we advertise.
const OUTGOING_WINDOW: u32 = u32::MAX >> 1;

/// Connection settings.
#[derive(Clone, Debug)]
pub struct ConnectionOptions {
    pub host: String,
    pub port: u16,
    /// TLS (`amqps`) settings.
    pub tls: Option<EndpointTls>,
    pub username: Option<String>,
    pub password: Option<String>,
    /// Forced SASL mechanism (`PLAIN`, `ANONYMOUS`, `EXTERNAL`, `SCRAM-SHA-256`, `SCRAM-SHA-512`).
    pub sasl_mechanism: Option<String>,
    /// Use the SASL layer (default true).
    pub sasl: bool,
    pub container_id: Option<String>,
    /// The `hostname` of the open frame (virtual host); defaults to the host.
    pub hostname: Option<String>,
    /// Our idle timeout (zero = none).
    pub idle_timeout: Duration,
    /// Our max-frame-size.
    pub max_frame_size: u32,
    pub channel_max: u16,
    pub connect_timeout: Duration,
    /// How long to wait for open/begin/attach/close handshakes.
    pub request_timeout: Duration,
    pub properties: Vec<(AmqpValue, AmqpValue)>,
    /// AMQP over WebSockets: the HTTP path of the upgrade request (Azure:
    /// [`crate::ws::AMQP_WS_PATH`]); `None` = AMQP directly on the socket.
    pub websocket_path: Option<String>,
}

impl ConnectionOptions {
    pub fn new(host: &str, port: u16) -> Self {
        ConnectionOptions {
            host: host.to_string(),
            port,
            tls: None,
            username: None,
            password: None,
            sasl_mechanism: None,
            sasl: true,
            container_id: None,
            hostname: None,
            idle_timeout: Duration::from_secs(60),
            max_frame_size: 1 << 20,
            channel_max: 255,
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(20),
            properties: Vec::new(),
            websocket_path: None,
        }
    }
    /// Carry AMQP over a WebSocket opened on `path` (subprotocol `AMQPWSB10`).
    pub fn websocket(mut self, path: &str) -> Self {
        self.websocket_path = Some(path.to_string());
        self
    }
    pub fn credentials(mut self, user: &str, password: &str) -> Self {
        self.username = Some(user.to_string());
        self.password = Some(password.to_string());
        self
    }
    pub fn tls(mut self, tls: EndpointTls) -> Self {
        self.tls = Some(tls);
        self
    }
    pub fn mechanism(mut self, m: &str) -> Self {
        self.sasl_mechanism = Some(m.to_string());
        self
    }
    pub fn idle_timeout(mut self, d: Duration) -> Self {
        self.idle_timeout = d;
        self
    }
    pub fn max_frame_size(mut self, n: u32) -> Self {
        self.max_frame_size = n.max(MIN_MAX_FRAME_SIZE);
        self
    }
    pub fn hostname(mut self, h: &str) -> Self {
        self.hostname = Some(h.to_string());
        self
    }
}

/// Link settings.
#[derive(Clone, Debug)]
pub struct LinkOptions {
    pub name: Option<String>,
    /// Target address (senders) or source address (receivers).
    pub address: Option<String>,
    /// Ask the peer to create a dynamic node (temporary queue).
    pub dynamic: bool,
    /// Capabilities of the remote terminus (`queue`, `topic`, `temporary-queue`...).
    pub capabilities: Vec<String>,
    /// Source filters (receivers).
    pub filter: Vec<(AmqpValue, AmqpValue)>,
    pub snd_settle_mode: SndSettleMode,
    pub rcv_settle_mode: RcvSettleMode,
    /// Receivers: credit window kept topped up automatically (0 = manual [`Receiver::flow`]).
    pub credit: u32,
    /// Terminus durability (0 none, 1 configuration, 2 unsettled-state).
    pub durable: u32,
    pub expiry_policy: Option<String>,
    /// Receivers: our target address (used as the reply-to of request/response nodes).
    pub local_address: Option<String>,
    pub properties: Vec<(AmqpValue, AmqpValue)>,
    pub desired_capabilities: Vec<String>,
    /// Senders: attach to the transaction coordinator instead of a node.
    pub coordinator: bool,
}

impl LinkOptions {
    pub fn new(address: &str) -> Self {
        LinkOptions {
            name: None,
            address: Some(address.to_string()),
            dynamic: false,
            capabilities: Vec::new(),
            filter: Vec::new(),
            snd_settle_mode: SndSettleMode::Unsettled,
            rcv_settle_mode: RcvSettleMode::First,
            credit: 100,
            durable: 0,
            expiry_policy: None,
            local_address: None,
            properties: Vec::new(),
            desired_capabilities: Vec::new(),
            coordinator: false,
        }
    }
    /// The transaction coordinator (`amqp:coordinator:list` target).
    pub fn coordinator() -> Self {
        LinkOptions { address: None, coordinator: true, ..LinkOptions::new("") }
    }
    /// A dynamic (temporary) node.
    pub fn dynamic() -> Self {
        LinkOptions { address: None, dynamic: true, ..LinkOptions::new("") }
    }
    pub fn name(mut self, n: &str) -> Self {
        self.name = Some(n.to_string());
        self
    }
    pub fn credit(mut self, c: u32) -> Self {
        self.credit = c;
        self
    }
    pub fn capability(mut self, c: &str) -> Self {
        self.capabilities.push(c.to_string());
        self
    }
    pub fn filter(mut self, key: &str, descriptor: &str, value: AmqpValue) -> Self {
        self.filter.push((AmqpValue::sym(key), AmqpValue::Described(Box::new(AmqpValue::sym(descriptor)), Box::new(value))));
        self
    }
    /// At-most-once: deliveries are pre-settled.
    pub fn at_most_once(mut self) -> Self {
        self.snd_settle_mode = SndSettleMode::Settled;
        self
    }
    /// At-least-once: deliveries are settled by the receiver.
    pub fn at_least_once(mut self) -> Self {
        self.snd_settle_mode = SndSettleMode::Unsettled;
        self
    }
    pub fn settle_mode(mut self, snd: SndSettleMode, rcv: RcvSettleMode) -> Self {
        self.snd_settle_mode = snd;
        self.rcv_settle_mode = rcv;
        self
    }
    pub fn local_address(mut self, a: &str) -> Self {
        self.local_address = Some(a.to_string());
        self
    }
    pub fn property(mut self, k: &str, v: AmqpValue) -> Self {
        self.properties.push((AmqpValue::sym(k), v));
        self
    }
}

/// A received message.
#[derive(Clone, Debug)]
pub struct Delivery {
    pub delivery_id: u32,
    pub tag: Vec<u8>,
    pub message: Message,
    /// The raw (encoded) message.
    pub raw: Vec<u8>,
    /// Pre-settled by the sender.
    pub settled: bool,
    pub message_format: u32,
}

impl Delivery {
    /// The delivery tag as a lock token (Azure: a 16-byte UUID).
    pub fn lock_token(&self) -> Option<String> {
        let t: [u8; 16] = self.tag.as_slice().try_into().ok()?;
        Some(crate::message::format_uuid(&dotnet_guid_bytes(&t)))
    }
}

/// Reorder the bytes of a .NET `Guid` wire layout (Azure lock tokens) to RFC 4122 order.
pub fn dotnet_guid_bytes(b: &[u8; 16]) -> [u8; 16] {
    [b[3], b[2], b[1], b[0], b[5], b[4], b[7], b[6], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]]
}

// ---------------------------------------------------------------------------
// state
// ---------------------------------------------------------------------------

struct Partial {
    delivery_id: u32,
    tag: Vec<u8>,
    settled: bool,
    format: u32,
    payload: Vec<u8>,
}

struct LinkState {
    name: String,
    role: Role,
    remote_handle: Option<u32>,
    remote_attach: Option<Attach>,
    detached: Option<Option<AmqpError>>,
    detach_sent: bool,
    delivery_count: u32,
    credit: u32,
    /// Sender: the peer's available count; receiver: whether a drain is in progress.
    drain_pending: bool,
    prefetch: u32,
    queue: VecDeque<Delivery>,
    partial: Option<Partial>,
    snd_settle_mode: SndSettleMode,
    /// Receivers in `rcv-settle-mode = second`: dispositions wait for the sender to settle.
    rcv_second: bool,
    next_tag: u64,
}

struct SessionState {
    remote_channel: Option<u16>,
    ended: Option<Option<AmqpError>>,
    end_sent: bool,
    next_outgoing_id: u32,
    remote_incoming_window: u32,
    next_incoming_id: u32,
    incoming_window: u32,
    links: BTreeMap<u32, LinkState>,
    by_remote_handle: HashMap<u32, u32>,
    next_handle: u32,
    next_delivery_id: u32,
    /// Unsettled deliveries we sent: delivery id -> outcome once settled by the peer.
    outcomes: HashMap<u32, Option<DeliveryState>>,
    /// Deliveries we received that the sender settled after our (unsettled) disposition.
    peer_settled: HashSet<u32>,
}

enum Status {
    Opening,
    Open,
    Closed(Option<AmqpError>, String),
}

struct State {
    status: Status,
    close_sent: bool,
    remote_open: Option<Open>,
    max_frame_out: u32,
    sessions: BTreeMap<u16, SessionState>,
    remote_channels: HashMap<u16, u16>,
    channel_max: u16,
}

struct Shared {
    transport: Arc<Transport>,
    state: Mutex<State>,
    cond: Condvar,
    opts: ConnectionOptions,
    container_id: String,
}

/// Frames the reader wants to send once the state lock is released.
#[allow(clippy::large_enum_variant)]
enum Out {
    Frame(Frame),
    /// A flow frame built from current state (session-level, or for a link handle).
    Flow { channel: u16, handle: Option<u32>, echo: bool },
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Write frames built from the state with writer and state locks held (in that order).
    fn write_with(&self, build: impl FnOnce(&mut State) -> Result<Vec<u8>>) -> Result<()> {
        let mut w = self.transport.lock_writer();
        let bytes = {
            let mut st = self.lock();
            build(&mut st)?
        };
        if bytes.is_empty() {
            return Ok(());
        }
        self.transport.write_locked(&mut w, &bytes).map_err(|e| self.io_err(e))
    }

    fn io_err(&self, e: std::io::Error) -> CamelError {
        if let Status::Closed(Some(err), _) = &self.lock().status {
            return amqp_error(err, "Connection closed");
        }
        camel_tls::camel_support::io_to_camel(e, &format!("{}:{}", self.opts.host, self.opts.port))
    }

    fn send_outs(&self, outs: Vec<Out>) {
        for o in outs {
            let r = self.write_with(|st| {
                Ok(match o {
                    Out::Frame(f) => f.encode(),
                    Out::Flow { channel, handle, echo } => flow_frame(st, channel, handle, false, echo).map(|f| f.encode()).unwrap_or_default(),
                })
            });
            if r.is_err() {
                break;
            }
        }
    }

    fn check_open(st: &State) -> Result<()> {
        match &st.status {
            Status::Closed(Some(e), _) => Err(amqp_error(e, "Connection closed by peer")),
            Status::Closed(None, msg) => Err(amqp_failure(format!("Connection closed: {}", msg))),
            _ => Ok(()),
        }
    }

    fn wait<'a>(&self, mut st: MutexGuard<'a, State>, deadline: Instant, what: &str, mut done: impl FnMut(&State) -> Result<bool>) -> Result<MutexGuard<'a, State>> {
        loop {
            if done(&st)? {
                return Ok(st);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(CamelError::new(core_kinds::TIMEOUT, format!("Timed out waiting for {}", what)));
            }
            st = self.cond.wait_timeout(st, (deadline - now).min(Duration::from_millis(200))).unwrap_or_else(|p| p.into_inner()).0;
        }
    }

    // ------------------------------------------------------------------ reader

    fn handle_frame(&self, f: Frame) -> std::result::Result<(), AmqpError> {
        let FrameBody::Amqp(p, payload) = f.body else {
            return match f.body {
                FrameBody::Empty => Ok(()),
                _ => Err(AmqpError::new(cond::FRAMING_ERROR, "unexpected SASL frame")),
            };
        };
        let mut outs = Vec::new();
        {
            let mut st = self.lock();
            self.process(&mut st, f.channel, p, payload, &mut outs)?;
        }
        self.cond.notify_all();
        self.send_outs(outs);
        Ok(())
    }

    fn process(&self, st: &mut State, channel: u16, p: Performative, payload: Vec<u8>, outs: &mut Vec<Out>) -> std::result::Result<(), AmqpError> {
        match p {
            Performative::Open(o) => {
                st.max_frame_out = o.max_frame_size.max(MIN_MAX_FRAME_SIZE);
                st.channel_max = st.channel_max.min(o.channel_max);
                self.transport.remote_idle_ms.store(o.idle_time_out as u64, Ordering::Relaxed);
                st.remote_open = Some(o);
                if matches!(st.status, Status::Opening) {
                    st.status = Status::Open;
                }
            }
            Performative::Begin(b) => {
                let local = b.remote_channel.ok_or_else(|| AmqpError::new(cond::NOT_IMPLEMENTED, "peer-initiated sessions are not supported"))?;
                let s = st.sessions.get_mut(&local).ok_or_else(|| AmqpError::new(cond::INVALID_FIELD, format!("begin for unknown channel {}", local)))?;
                s.remote_channel = Some(channel);
                s.next_incoming_id = b.next_outgoing_id;
                s.remote_incoming_window = b.incoming_window;
                st.remote_channels.insert(channel, local);
            }
            Performative::End(e) => {
                if let Some(local) = st.remote_channels.remove(&channel) {
                    if let Some(s) = st.sessions.get_mut(&local) {
                        s.ended = Some(e);
                        for l in s.links.values_mut() {
                            if l.detached.is_none() {
                                l.detached = Some(s.ended.clone().flatten());
                            }
                        }
                        if !s.end_sent {
                            s.end_sent = true;
                            outs.push(Out::Frame(Frame::amqp(local, Performative::End(None))));
                        }
                    }
                }
            }
            Performative::Close(e) => {
                st.status = Status::Closed(e, "closed by peer".into());
                if !st.close_sent {
                    st.close_sent = true;
                    outs.push(Out::Frame(Frame::amqp(0, Performative::Close(None))));
                }
            }
            other => {
                let local = *st.remote_channels.get(&channel).ok_or_else(|| AmqpError::new(cond::INVALID_FIELD, format!("frame on unknown channel {}", channel)))?;
                let s = st.sessions.get_mut(&local).ok_or_else(|| AmqpError::new(cond::INTERNAL_ERROR, "session state missing"))?;
                process_session(s, local, other, payload, outs)?;
            }
        }
        Ok(())
    }

    fn fail(&self, err: Option<AmqpError>, msg: &str) {
        {
            let mut st = self.lock();
            if !matches!(st.status, Status::Closed(..)) {
                st.status = Status::Closed(err, msg.to_string());
            }
        }
        self.transport.close();
        self.cond.notify_all();
    }
}

fn process_session(s: &mut SessionState, local: u16, p: Performative, payload: Vec<u8>, outs: &mut Vec<Out>) -> std::result::Result<(), AmqpError> {
    match p {
        Performative::Attach(a) => {
            let found = s.links.iter_mut().find(|(_, l)| l.name == a.name && l.role != a.role && l.remote_handle.is_none());
            match found {
                Some((h, l)) => {
                    let h = *h;
                    l.remote_handle = Some(a.handle);
                    if l.role == Role::Receiver {
                        l.delivery_count = a.initial_delivery_count.unwrap_or(0);
                        l.rcv_second = l.rcv_second && a.rcv_settle_mode == RcvSettleMode::Second;
                    } else {
                        l.snd_settle_mode = a.snd_settle_mode;
                    }
                    l.remote_attach = Some(a.clone());
                    s.by_remote_handle.insert(a.handle, h);
                }
                None => {
                    // peer-initiated links are refused
                    let h = s.next_handle;
                    s.next_handle += 1;
                    let role = if a.role == Role::Sender { Role::Receiver } else { Role::Sender };
                    let attach = Attach {
                        name: a.name.clone(),
                        handle: h,
                        role,
                        snd_settle_mode: SndSettleMode::Mixed,
                        rcv_settle_mode: RcvSettleMode::First,
                        source: None,
                        target: None,
                        initial_delivery_count: if role == Role::Sender { Some(0) } else { None },
                        max_message_size: None,
                        offered_capabilities: vec![],
                        desired_capabilities: vec![],
                        properties: vec![],
                    };
                    outs.push(Out::Frame(Frame::amqp(local, Performative::Attach(attach))));
                    outs.push(Out::Frame(Frame::amqp(
                        local,
                        Performative::Detach(Detach { handle: h, closed: true, error: Some(AmqpError::new(cond::NOT_IMPLEMENTED, "peer-initiated links are not supported")) }),
                    )));
                }
            }
        }
        Performative::Detach(d) => {
            if let Some(h) = s.by_remote_handle.remove(&d.handle) {
                if let Some(l) = s.links.get_mut(&h) {
                    l.detached = Some(d.error.clone());
                    if !l.detach_sent {
                        l.detach_sent = true;
                        outs.push(Out::Frame(Frame::amqp(local, Performative::Detach(Detach { handle: h, closed: d.closed, error: None }))));
                    }
                }
            }
        }
        Performative::Flow(f) => {
            let base = f.next_incoming_id.unwrap_or(0);
            s.remote_incoming_window = base.wrapping_add(f.incoming_window).wrapping_sub(s.next_outgoing_id);
            if let Some(rh) = f.handle {
                let h = *s.by_remote_handle.get(&rh).ok_or_else(|| AmqpError::new(cond::INVALID_FIELD, format!("flow for unknown handle {}", rh)))?;
                let l = s.links.get_mut(&h).unwrap();
                match l.role {
                    Role::Sender => {
                        let dc = f.delivery_count.unwrap_or(l.delivery_count);
                        let credit = f.link_credit.unwrap_or(0);
                        l.credit = dc.wrapping_add(credit).wrapping_sub(l.delivery_count);
                        if l.credit > (u32::MAX >> 1) {
                            l.credit = 0;
                        }
                        if f.drain && l.credit > 0 {
                            // nothing is queued: consume the credit and report back
                            l.delivery_count = l.delivery_count.wrapping_add(l.credit);
                            l.credit = 0;
                            outs.push(Out::Flow { channel: local, handle: Some(h), echo: false });
                        } else if f.echo {
                            outs.push(Out::Flow { channel: local, handle: Some(h), echo: false });
                        }
                    }
                    Role::Receiver => {
                        if let Some(dc) = f.delivery_count {
                            let advanced = dc.wrapping_sub(l.delivery_count);
                            if advanced <= l.credit {
                                l.credit -= advanced;
                                l.delivery_count = dc;
                            }
                        }
                        if l.drain_pending && (f.link_credit == Some(0) || l.credit == 0) {
                            l.drain_pending = false;
                            l.credit = 0;
                        }
                        if f.echo {
                            outs.push(Out::Flow { channel: local, handle: Some(h), echo: false });
                        }
                    }
                }
            } else if f.echo {
                outs.push(Out::Flow { channel: local, handle: None, echo: false });
            }
        }
        Performative::Transfer(t) => {
            s.next_incoming_id = s.next_incoming_id.wrapping_add(1);
            s.incoming_window = s.incoming_window.saturating_sub(1);
            if s.incoming_window < INCOMING_WINDOW / 2 {
                s.incoming_window = INCOMING_WINDOW;
                outs.push(Out::Flow { channel: local, handle: None, echo: false });
            }
            let h = *s.by_remote_handle.get(&t.handle).ok_or_else(|| AmqpError::new(cond::INVALID_FIELD, format!("transfer for unknown handle {}", t.handle)))?;
            let l = s.links.get_mut(&h).unwrap();
            if l.role != Role::Receiver {
                return Err(AmqpError::new(cond::INVALID_FIELD, "transfer on a sender link"));
            }
            if l.partial.is_none() {
                let id = t.delivery_id.ok_or_else(|| AmqpError::new(cond::INVALID_FIELD, "first transfer without delivery-id"))?;
                l.partial = Some(Partial { delivery_id: id, tag: t.delivery_tag.clone().unwrap_or_default(), settled: false, format: t.message_format.unwrap_or(0), payload: Vec::new() });
                l.delivery_count = l.delivery_count.wrapping_add(1);
                l.credit = l.credit.saturating_sub(1);
            }
            let p = l.partial.as_mut().unwrap();
            p.payload.extend_from_slice(&payload);
            p.settled |= t.settled.unwrap_or(false);
            if t.aborted {
                l.partial = None;
            } else if !t.more {
                let p = l.partial.take().unwrap();
                let message = Message::decode(&p.payload).map_err(|e| AmqpError::new(cond::DECODE_ERROR, e.0))?;
                l.queue.push_back(Delivery { delivery_id: p.delivery_id, tag: p.tag, message, raw: p.payload, settled: p.settled, message_format: p.format });
            }
        }
        Performative::Disposition(d) => {
            let last = d.last.unwrap_or(d.first);
            if d.role == Role::Receiver {
                let mut id = d.first;
                loop {
                    if let Some(o) = s.outcomes.get_mut(&id) {
                        if d.settled || d.state.as_ref().is_some_and(|x| x.is_outcome()) {
                            *o = Some(d.state.clone().unwrap_or(DeliveryState::Accepted));
                        }
                    }
                    if id == last {
                        break;
                    }
                    id = id.wrapping_add(1);
                }
                if !d.settled {
                    outs.push(Out::Frame(Frame::amqp(local, Performative::Disposition(Disposition { role: Role::Sender, first: d.first, last: d.last, settled: true, state: d.state, batchable: false }))));
                }
            } else if d.settled {
                // the sender settled deliveries we received (rcv-settle-mode second)
                let mut id = d.first;
                loop {
                    s.peer_settled.insert(id);
                    if id == last {
                        break;
                    }
                    id = id.wrapping_add(1);
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn flow_frame(st: &State, channel: u16, handle: Option<u32>, drain: bool, echo: bool) -> Option<Frame> {
    let s = st.sessions.get(&channel)?;
    if s.end_sent || s.ended.is_some() {
        return None;
    }
    let mut f = Flow {
        next_incoming_id: s.remote_channel.map(|_| s.next_incoming_id),
        incoming_window: s.incoming_window,
        next_outgoing_id: s.next_outgoing_id,
        outgoing_window: OUTGOING_WINDOW,
        echo,
        ..Default::default()
    };
    if let Some(h) = handle {
        let l = s.links.get(&h)?;
        if l.detach_sent || l.detached.is_some() {
            return None;
        }
        f.handle = Some(h);
        f.delivery_count = Some(l.delivery_count);
        f.link_credit = Some(l.credit);
        f.drain = drain;
        if l.role == Role::Sender {
            f.available = Some(0);
        }
    }
    Some(Frame::amqp(channel, Performative::Flow(f)))
}

// ---------------------------------------------------------------------------
// connection
// ---------------------------------------------------------------------------

/// An AMQP 1.0 connection. Closed on drop.
pub struct Connection {
    shared: Arc<Shared>,
    reader: Mutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Connection({}:{})", self.shared.opts.host, self.shared.opts.port)
    }
}

fn sasl_negotiate(stream: &mut AmqpStream, opts: &ConnectionOptions) -> Result<()> {
    use std::io::Write;
    let what = format!("{}:{}", opts.host, opts.port);
    let io = |e: std::io::Error| camel_tls::camel_support::io_to_camel(e, &what);
    stream.write_all(&SASL_HEADER).map_err(io)?;
    let h = read_protocol_header(stream).map_err(io)?;
    if h != SASL_HEADER {
        return Err(amqp_failure(format!("Peer {} does not support the SASL layer (header {:?})", what, h)));
    }
    let next = |s: &mut AmqpStream| -> Result<SaslFrame> {
        match read_frame(s, 65536).map_err(io)? {
            Some(Frame { body: FrameBody::Sasl(f), .. }) => Ok(f),
            Some(_) => Err(amqp_failure("Unexpected frame during SASL negotiation")),
            None => Err(amqp_failure(format!("Connection closed during SASL negotiation with {}", what))),
        }
    };
    let offered = match next(stream)? {
        SaslFrame::Mechanisms(m) => m,
        _ => return Err(amqp_failure("Expected sasl-mechanisms")),
    };
    let has = |m: &str| offered.iter().any(|o| o.eq_ignore_ascii_case(m));
    let mech = match &opts.sasl_mechanism {
        Some(m) => {
            if !has(m) {
                register_kinds();
                return Err(CamelError::new(kinds::AUTHENTICATION_FAILURE, format!("SASL mechanism {} not offered by {} (offered: {})", m, what, offered.join(", "))));
            }
            m.to_ascii_uppercase()
        }
        None if opts.username.is_some() => ["SCRAM-SHA-512", "SCRAM-SHA-256", "PLAIN"]
            .into_iter()
            .find(|m| has(m))
            .map(String::from)
            .ok_or_else(|| CamelError::new(kinds::AUTHENTICATION_FAILURE, format!("No supported SASL mechanism offered by {}: {}", what, offered.join(", "))))?,
        None => ["ANONYMOUS", "EXTERNAL"]
            .into_iter()
            .find(|m| has(m))
            .map(String::from)
            .ok_or_else(|| CamelError::new(kinds::AUTHENTICATION_FAILURE, format!("Credentials required by {} (offered: {})", what, offered.join(", "))))?,
    };
    let user = opts.username.clone().unwrap_or_default();
    let pass = opts.password.clone().unwrap_or_default();
    let mut scram = camel_tls::sasl::ScramMechanism::from_name(&mech).map(|m| camel_tls::sasl::ScramClient::new(m, &user, &pass));
    let initial = match mech.as_str() {
        "PLAIN" => Some(camel_tls::sasl::plain_initial_response("", &user, &pass)),
        "ANONYMOUS" => Some(user.clone().into_bytes()),
        "EXTERNAL" => Some(Vec::new()),
        _ => scram.as_mut().map(|s| s.client_first().into_bytes()),
    };
    let hostname = opts.hostname.clone().or_else(|| Some(opts.host.clone()));
    let send = |s: &mut AmqpStream, f: SaslFrame| s.write_all(&Frame { channel: 0, body: FrameBody::Sasl(f) }.encode()).map_err(io);
    send(stream, SaslFrame::Init { mechanism: mech.clone(), initial_response: initial, hostname })?;
    loop {
        match next(stream)? {
            SaslFrame::Challenge(c) => {
                let resp = match scram.as_mut() {
                    Some(s) => {
                        let text = String::from_utf8_lossy(&c).to_string();
                        s.handle_server_first(&text).map_err(|e| CamelError::new(kinds::AUTHENTICATION_FAILURE, format!("SCRAM: {}", e)))?.into_bytes()
                    }
                    None => Vec::new(),
                };
                send(stream, SaslFrame::Response(resp))?;
            }
            SaslFrame::Outcome { code, additional_data } => {
                if code != 0 {
                    register_kinds();
                    return Err(CamelError::new(kinds::AUTHENTICATION_FAILURE, format!("SASL {} authentication failed against {} (outcome code {})", mech, what, code)));
                }
                if let (Some(s), Some(data)) = (scram.as_ref(), additional_data) {
                    s.verify_server_final(&String::from_utf8_lossy(&data)).map_err(|e| CamelError::new(kinds::AUTHENTICATION_FAILURE, format!("SCRAM: {}", e)))?;
                }
                return Ok(());
            }
            _ => return Err(amqp_failure("Unexpected SASL frame")),
        }
    }
}

impl Connection {
    /// Connect, negotiate SASL and open the connection.
    pub fn open(opts: &ConnectionOptions) -> Result<Connection> {
        use std::io::Write;
        register_kinds();
        let what = format!("{}:{}", opts.host, opts.port);
        let net = match &opts.tls {
            Some(t) => t.connect(&opts.host, opts.port, opts.connect_timeout)?,
            None => {
                let tcp = camel_tls::net::connect_tcp(&opts.host, opts.port, opts.connect_timeout).map_err(|e| match e.kind() {
                    std::io::ErrorKind::ConnectionRefused => CamelError::new(core_kinds::CONNECT, format!("Connection refused: {}", what)),
                    _ => CamelError::new(core_kinds::CONNECT, format!("{}: {}", e, what)),
                })?;
                NetStream::plain(tcp)
            }
        };
        let _ = net.set_nodelay(true);
        let _ = net.set_read_timeout(Some(opts.request_timeout.max(Duration::from_secs(1))));
        let mut stream = match &opts.websocket_path {
            Some(path) => AmqpStream::websocket_client(net, &opts.host, opts.port, path, crate::ws::AMQP_WS_PROTOCOL).map_err(|e| CamelError::new(core_kinds::CONNECT, format!("WebSocket connection to {} failed: {}", what, e)))?,
            None => AmqpStream::from(net),
        };
        let _ = stream.set_read_timeout(Some(opts.request_timeout.max(Duration::from_secs(1))));
        if opts.sasl {
            sasl_negotiate(&mut stream, opts)?;
        }
        let io = |e: std::io::Error| camel_tls::camel_support::io_to_camel(e, &what);
        stream.write_all(&AMQP_HEADER).map_err(io)?;
        let h = read_protocol_header(&mut stream).map_err(io)?;
        if h != AMQP_HEADER {
            return Err(amqp_failure(format!("Unsupported protocol header from {}: {:?}", what, h)));
        }
        let _ = stream.set_read_timeout(None);
        let transport = Transport::new(stream).map_err(io)?;
        let container_id = opts.container_id.clone().unwrap_or_else(|| format!("camel-{}", crate::util::unique_id()));
        let shared = Arc::new(Shared {
            transport: transport.clone(),
            state: Mutex::new(State {
                status: Status::Opening,
                close_sent: false,
                remote_open: None,
                max_frame_out: MIN_MAX_FRAME_SIZE,
                sessions: BTreeMap::new(),
                remote_channels: HashMap::new(),
                channel_max: opts.channel_max,
            }),
            cond: Condvar::new(),
            opts: opts.clone(),
            container_id: container_id.clone(),
        });
        let open = Open {
            container_id,
            hostname: opts.hostname.clone().or_else(|| Some(opts.host.clone())),
            max_frame_size: opts.max_frame_size,
            channel_max: opts.channel_max,
            idle_time_out: opts.idle_timeout.as_millis().min(u32::MAX as u128) as u32,
            properties: opts.properties.clone(),
            ..Default::default()
        };
        transport.write_frame(&Frame::amqp(0, Performative::Open(open))).map_err(io)?;
        transport.local_idle_ms.store(opts.idle_timeout.as_millis() as u64, Ordering::Relaxed);
        let mut reader = transport.reader().map_err(io)?;
        let s2 = shared.clone();
        let max_in = opts.max_frame_size;
        let handle = std::thread::Builder::new()
            .name(format!("amqp10-reader-{}", what))
            .spawn(move || loop {
                match read_frame(&mut reader, max_in) {
                    Ok(Some(f)) => {
                        s2.transport.touch_read();
                        crate::transport::trace_frame("<-", &f);
                        if let Err(e) = s2.handle_frame(f) {
                            let _ = s2.transport.write_frame(&Frame::amqp(0, Performative::Close(Some(e.clone()))));
                            s2.fail(Some(e), "protocol error");
                            break;
                        }
                        if s2.transport.is_closed() {
                            break;
                        }
                        let closed = matches!(s2.lock().status, Status::Closed(..));
                        if closed {
                            s2.transport.close();
                            s2.cond.notify_all();
                            break;
                        }
                    }
                    Ok(None) => {
                        s2.fail(None, "connection lost");
                        break;
                    }
                    Err(e) => {
                        s2.fail(None, &format!("connection lost: {}", e));
                        break;
                    }
                }
            })
            .map_err(|e| CamelError::io(e.to_string()))?;
        let s3 = shared.clone();
        transport.spawn_heartbeat(&format!("amqp10-heartbeat-{}", what), move || {
            let e = AmqpError::new(cond::RESOURCE_LIMIT_EXCEEDED, "local-idle-timeout expired");
            let _ = s3.transport.write_frame(&Frame::amqp(0, Performative::Close(Some(e))));
            s3.fail(None, "idle timeout expired (no frames received)");
        });
        let conn = Connection { shared: shared.clone(), reader: Mutex::new(Some(handle)) };
        let deadline = Instant::now() + opts.request_timeout;
        let st = shared.lock();
        let r = shared.wait(st, deadline, "open", |st| {
            Shared::check_open(st)?;
            Ok(matches!(st.status, Status::Open))
        });
        drop(r?);
        Ok(conn)
    }

    /// The peer's open frame.
    pub fn remote_open(&self) -> Option<Open> {
        self.shared.lock().remote_open.clone()
    }

    /// Our container id.
    pub fn container_id(&self) -> &str {
        &self.shared.container_id
    }

    /// Whether the connection is (still) open.
    pub fn is_open(&self) -> bool {
        matches!(self.shared.lock().status, Status::Open) && !self.shared.transport.is_closed()
    }

    /// The error that closed the connection, if any.
    pub fn error(&self) -> Option<CamelError> {
        Shared::check_open(&self.shared.lock()).err()
    }

    /// Begin a new session.
    pub fn session(&self) -> Result<Session> {
        let sh = &self.shared;
        let mut channel = 0;
        sh.write_with(|st| {
            Shared::check_open(st)?;
            channel = (0..=st.channel_max).find(|c| !st.sessions.contains_key(c)).ok_or_else(|| amqp_failure("channel-max reached"))?;
            st.sessions.insert(
                channel,
                SessionState {
                    remote_channel: None,
                    ended: None,
                    end_sent: false,
                    next_outgoing_id: 0,
                    remote_incoming_window: 0,
                    next_incoming_id: 0,
                    incoming_window: INCOMING_WINDOW,
                    links: BTreeMap::new(),
                    by_remote_handle: HashMap::new(),
                    next_handle: 0,
                    next_delivery_id: 0,
                    outcomes: HashMap::new(),
                    peer_settled: HashSet::new(),
                },
            );
            let b = Begin { remote_channel: None, next_outgoing_id: 0, incoming_window: INCOMING_WINDOW, outgoing_window: OUTGOING_WINDOW, handle_max: u32::MAX, properties: vec![] };
            Ok(Frame::amqp(channel, Performative::Begin(b)).encode())
        })?;
        let deadline = Instant::now() + sh.opts.request_timeout;
        let st = sh.lock();
        drop(sh.wait(st, deadline, "begin", |st| {
            Shared::check_open(st)?;
            let s = &st.sessions[&channel];
            if let Some(e) = &s.ended {
                return Err(e.as_ref().map(|e| amqp_error(e, "Session ended")).unwrap_or_else(|| amqp_failure("Session ended")));
            }
            Ok(s.remote_channel.is_some())
        })?);
        Ok(Session { shared: sh.clone(), channel })
    }

    /// Close the connection (waits briefly for the peer's close).
    pub fn close(&self) {
        let sh = &self.shared;
        let send = {
            let mut st = sh.lock();
            let s = !st.close_sent && !matches!(st.status, Status::Closed(..));
            st.close_sent = true;
            s
        };
        if send && sh.transport.write_frame(&Frame::amqp(0, Performative::Close(None))).is_ok() {
            let st = sh.lock();
            drop(sh.wait(st, Instant::now() + Duration::from_secs(2), "close", |st| Ok(matches!(st.status, Status::Closed(..)))));
        }
        sh.fail(None, "closed");
        if let Some(h) = self.reader.lock().unwrap().take() {
            if h.thread().id() != std::thread::current().id() {
                let _ = h.join();
            }
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.close();
    }
}

// ---------------------------------------------------------------------------
// session and links
// ---------------------------------------------------------------------------

/// A session on a connection.
#[derive(Clone)]
pub struct Session {
    shared: Arc<Shared>,
    channel: u16,
}

fn check_session(st: &State, channel: u16) -> Result<()> {
    Shared::check_open(st)?;
    let s = st.sessions.get(&channel).ok_or_else(|| amqp_failure("unknown session"))?;
    match &s.ended {
        Some(Some(e)) => Err(amqp_error(e, "Session ended")),
        Some(None) => Err(amqp_failure("Session ended")),
        None if s.end_sent => Err(amqp_failure("Session ended")),
        None => Ok(()),
    }
}

fn check_link(st: &State, channel: u16, handle: u32) -> Result<()> {
    check_session(st, channel)?;
    let l = &st.sessions[&channel].links[&handle];
    match &l.detached {
        Some(Some(e)) => Err(amqp_error(e, &format!("Link {} detached", l.name))),
        Some(None) => Err(amqp_failure(format!("Link {} detached", l.name))),
        None if l.detach_sent => Err(amqp_failure(format!("Link {} closed", l.name))),
        None => Ok(()),
    }
}

impl Session {
    /// The local channel number.
    pub fn channel(&self) -> u16 {
        self.channel
    }

    /// Whether the session (and its connection) is still usable.
    pub fn is_open(&self) -> bool {
        check_session(&self.shared.lock(), self.channel).is_ok() && !self.shared.transport.is_closed()
    }

    /// Attach a link to the transaction coordinator (local transactions).
    pub fn coordinator(&self) -> Result<Coordinator> {
        let sender = self.sender(LinkOptions::coordinator().name(&format!("txn-controller-{}", crate::util::unique_id())))?;
        Ok(Coordinator { sender, timeout: self.shared.opts.request_timeout })
    }

    fn attach(&self, role: Role, opts: &LinkOptions) -> Result<(u32, Attach)> {
        let sh = &self.shared;
        let name = opts.name.clone().unwrap_or_else(|| format!("{}-{}", if role == Role::Sender { "sender" } else { "receiver" }, crate::util::unique_id()));
        let mut handle = 0;
        sh.write_with(|st| {
            check_session(st, self.channel)?;
            let s = st.sessions.get_mut(&self.channel).unwrap();
            handle = s.next_handle;
            s.next_handle += 1;
            s.links.insert(
                handle,
                LinkState {
                    name: name.clone(),
                    role,
                    remote_handle: None,
                    remote_attach: None,
                    detached: None,
                    detach_sent: false,
                    delivery_count: 0,
                    credit: 0,
                    drain_pending: false,
                    prefetch: opts.credit,
                    queue: VecDeque::new(),
                    partial: None,
                    snd_settle_mode: opts.snd_settle_mode,
                    rcv_second: role == Role::Receiver && opts.rcv_settle_mode == RcvSettleMode::Second,
                    next_tag: 0,
                },
            );
            let node_source = Source {
                address: if opts.dynamic { None } else { opts.address.clone() },
                dynamic: opts.dynamic,
                durable: opts.durable,
                expiry_policy: opts.expiry_policy.clone(),
                filter: opts.filter.clone(),
                capabilities: opts.capabilities.clone(),
                ..Default::default()
            };
            let node_target = Target {
                address: if opts.dynamic { None } else { opts.address.clone() },
                dynamic: opts.dynamic,
                durable: opts.durable,
                expiry_policy: opts.expiry_policy.clone(),
                capabilities: opts.capabilities.clone(),
                ..Default::default()
            };
            let node_target = if opts.coordinator { Target::coordinator() } else { node_target };
            let (source, target) = match role {
                Role::Sender => (Source { address: Some(name.clone()), ..Default::default() }, node_target),
                Role::Receiver => (node_source, Target { address: opts.local_address.clone().or_else(|| Some(name.clone())), ..Default::default() }),
            };
            let a = Attach {
                name: name.clone(),
                handle,
                role,
                snd_settle_mode: opts.snd_settle_mode,
                rcv_settle_mode: opts.rcv_settle_mode,
                source: Some(source),
                target: Some(target),
                initial_delivery_count: if role == Role::Sender { Some(0) } else { None },
                max_message_size: None,
                offered_capabilities: vec![],
                desired_capabilities: opts.desired_capabilities.clone(),
                properties: opts.properties.clone(),
            };
            Ok(Frame::amqp(self.channel, Performative::Attach(a)).encode())
        })?;
        let deadline = Instant::now() + sh.opts.request_timeout;
        let st = sh.lock();
        let channel = self.channel;
        let st = sh.wait(st, deadline, "attach", |st| {
            check_session(st, channel)?;
            let l = &st.sessions[&channel].links[&handle];
            if let Some(err) = &l.detached {
                return Err(match err {
                    Some(e) => amqp_error(e, &format!("Link to {} refused", opts.address.as_deref().unwrap_or("(dynamic)"))),
                    None => amqp_failure(format!("Link to {} refused", opts.address.as_deref().unwrap_or("(dynamic)"))),
                });
            }
            Ok(match &l.remote_attach {
                // a null terminus announces a refusal: wait for the detach
                Some(a) => (role == Role::Sender && a.target.is_some()) || (role == Role::Receiver && a.source.is_some()),
                None => false,
            })
        })?;
        let a = st.sessions[&channel].links[&handle].remote_attach.clone().unwrap();
        Ok((handle, a))
    }

    /// Attach a sending link.
    pub fn sender(&self, opts: LinkOptions) -> Result<Sender> {
        let (handle, remote) = self.attach(Role::Sender, &opts)?;
        Ok(Sender { shared: self.shared.clone(), channel: self.channel, handle, remote, address: opts.address.clone() })
    }

    /// Attach a receiving link (with `opts.credit` initial credit).
    pub fn receiver(&self, opts: LinkOptions) -> Result<Receiver> {
        let (handle, remote) = self.attach(Role::Receiver, &opts)?;
        let r = Receiver { shared: self.shared.clone(), channel: self.channel, handle, remote };
        if opts.credit > 0 {
            r.flow(opts.credit)?;
        }
        Ok(r)
    }

    /// End the session.
    pub fn end(&self) {
        let sh = &self.shared;
        let ch = self.channel;
        let r = sh.write_with(|st| {
            let s = match st.sessions.get_mut(&ch) {
                Some(s) if !s.end_sent && s.ended.is_none() => s,
                _ => return Ok(Vec::new()),
            };
            s.end_sent = true;
            Ok(Frame::amqp(ch, Performative::End(None)).encode())
        });
        if r.is_ok() {
            let st = sh.lock();
            drop(sh.wait(st, Instant::now() + Duration::from_secs(2), "end", |st| Ok(st.sessions.get(&ch).is_none_or(|s| s.ended.is_some()) || Shared::check_open(st).is_err())));
        }
        let mut st = sh.lock();
        st.sessions.remove(&ch);
    }
}

fn detach_link(shared: &Arc<Shared>, channel: u16, handle: u32, closed: bool) {
    let r = shared.write_with(|st| {
        let l = match st.sessions.get_mut(&channel).and_then(|s| s.links.get_mut(&handle)) {
            Some(l) if !l.detach_sent && l.detached.is_none() => l,
            _ => return Ok(Vec::new()),
        };
        l.detach_sent = true;
        Ok(Frame::amqp(channel, Performative::Detach(Detach { handle, closed, error: None })).encode())
    });
    if r.is_ok() {
        let st = shared.lock();
        drop(shared.wait(st, Instant::now() + Duration::from_secs(2), "detach", |st| {
            Ok(st.sessions.get(&channel).and_then(|s| s.links.get(&handle)).is_none_or(|l| l.detached.is_some()) || check_session(st, channel).is_err())
        }));
    }
    let mut st = shared.lock();
    if let Some(s) = st.sessions.get_mut(&channel) {
        s.links.remove(&handle);
        s.by_remote_handle.retain(|_, h| *h != handle);
    }
}

/// A sending link.
pub struct Sender {
    shared: Arc<Shared>,
    channel: u16,
    handle: u32,
    remote: Attach,
    address: Option<String>,
}

impl Sender {
    /// The peer's attach (target, settle modes...).
    pub fn remote_attach(&self) -> &Attach {
        &self.remote
    }
    /// The target address.
    pub fn address(&self) -> Option<&str> {
        self.remote.target.as_ref().and_then(|t| t.address.as_deref()).or(self.address.as_deref())
    }
    /// Current link credit.
    pub fn credit(&self) -> u32 {
        let st = self.shared.lock();
        st.sessions.get(&self.channel).and_then(|s| s.links.get(&self.handle)).map(|l| l.credit).unwrap_or(0)
    }

    /// Send a message and wait for its outcome (pre-settled - `None` - when the link is at-most-once).
    pub fn send(&self, msg: &Message, timeout: Duration) -> Result<Option<DeliveryState>> {
        let settled = {
            let st = self.shared.lock();
            st.sessions.get(&self.channel).and_then(|s| s.links.get(&self.handle)).map(|l| l.snd_settle_mode == SndSettleMode::Settled).unwrap_or(false)
        };
        self.send_with(msg, settled, timeout)
    }

    /// Send a message, pre-settled or not; waits for credit (and the outcome when unsettled).
    pub fn send_with(&self, msg: &Message, settled: bool, timeout: Duration) -> Result<Option<DeliveryState>> {
        self.send_raw(&msg.encode(), settled, timeout)
    }

    /// Send an encoded message.
    pub fn send_raw(&self, payload: &[u8], settled: bool, timeout: Duration) -> Result<Option<DeliveryState>> {
        self.send_raw_txn(payload, settled, None, timeout)
    }

    /// Send a message as part of a transaction (unsettled; the outcome is transactional).
    pub fn send_in_txn(&self, msg: &Message, txn_id: &[u8], timeout: Duration) -> Result<Option<DeliveryState>> {
        self.send_raw_txn(&msg.encode(), false, Some(txn_id), timeout)
    }

    /// Send an encoded message, optionally within a transaction.
    pub fn send_raw_txn(&self, payload: &[u8], settled: bool, txn: Option<&[u8]>, timeout: Duration) -> Result<Option<DeliveryState>> {
        self.send_formatted(payload, 0, settled, txn, timeout)
    }

    /// Send several messages as one Azure batch delivery (message format `0x80013700`: an
    /// envelope whose data sections are the encoded messages), as the Azure SDKs send batches.
    pub fn send_azure_batch(&self, msgs: &[Message], timeout: Duration) -> Result<Option<DeliveryState>> {
        let envelope = Message { body: crate::message::Body::Data(msgs.iter().map(|m| m.encode()).collect()), ..Default::default() };
        self.send_formatted(&envelope.encode(), 0x8001_3700, false, None, timeout)
    }

    fn send_formatted(&self, payload: &[u8], format: u32, settled: bool, txn: Option<&[u8]>, timeout: Duration) -> Result<Option<DeliveryState>> {
        let sh = &self.shared;
        let (ch, h) = (self.channel, self.handle);
        let deadline = Instant::now() + timeout;
        let delivery_id;
        loop {
            {
                let st = sh.lock();
                drop(sh.wait(st, deadline, "link credit", |st| {
                    check_link(st, ch, h)?;
                    let s = &st.sessions[&ch];
                    Ok(s.links[&h].credit > 0 && s.remote_incoming_window > 0)
                })?);
            }
            let mut w = sh.transport.lock_writer();
            let mut st = sh.lock();
            check_link(&st, ch, h)?;
            let max_frame = st.max_frame_out;
            let s = st.sessions.get_mut(&ch).unwrap();
            if s.links[&h].credit == 0 || s.remote_incoming_window == 0 {
                continue;
            }
            let id = s.next_delivery_id;
            s.next_delivery_id = s.next_delivery_id.wrapping_add(1);
            let l = s.links.get_mut(&h).unwrap();
            l.credit -= 1;
            l.delivery_count = l.delivery_count.wrapping_add(1);
            let tag = l.next_tag.to_be_bytes().to_vec();
            l.next_tag += 1;
            if !settled {
                s.outcomes.insert(id, None);
            }
            let mut t = Transfer { handle: h, delivery_id: Some(id), delivery_tag: Some(tag), message_format: Some(format), settled: Some(settled), more: true, ..Default::default() };
            t.state = txn.map(|id| DeliveryState::Transactional { txn_id: id.to_vec(), outcome: None });
            let overhead = 8 + Performative::Transfer(t.clone()).to_value().encode().len();
            let chunk = (max_frame as usize).saturating_sub(overhead).max(1);
            let mut bytes = Vec::with_capacity(payload.len() + 64);
            let mut chunks: Vec<&[u8]> = payload.chunks(chunk).collect();
            if chunks.is_empty() {
                chunks.push(&[]);
            }
            let n = chunks.len();
            for (i, c) in chunks.into_iter().enumerate() {
                t.more = i + 1 < n;
                bytes.extend_from_slice(&Frame { channel: ch, body: FrameBody::Amqp(Performative::Transfer(t.clone()), c.to_vec()) }.encode());
                s.next_outgoing_id = s.next_outgoing_id.wrapping_add(1);
                s.remote_incoming_window = s.remote_incoming_window.saturating_sub(1);
            }
            drop(st);
            sh.transport.write_locked(&mut w, &bytes).map_err(|e| sh.io_err(e))?;
            delivery_id = id;
            break;
        }
        if settled {
            return Ok(None);
        }
        let st = sh.lock();
        let r = sh.wait(st, deadline, "delivery outcome", |st| {
            if st.sessions.get(&ch).and_then(|s| s.outcomes.get(&delivery_id)).is_some_and(|o| o.is_some()) {
                return Ok(true);
            }
            check_link(st, ch, h)?;
            Ok(false)
        });
        let mut st = match r {
            Ok(st) => st,
            Err(e) => {
                let mut st = sh.lock();
                if let Some(s) = st.sessions.get_mut(&ch) {
                    s.outcomes.remove(&delivery_id);
                }
                return Err(e);
            }
        };
        Ok(st.sessions.get_mut(&ch).and_then(|s| s.outcomes.remove(&delivery_id)).flatten())
    }

    /// Send and require the `accepted` outcome (rejections become errors).
    pub fn send_accepted(&self, msg: &Message, timeout: Duration) -> Result<()> {
        check_accepted(self.send(msg, timeout)?)
    }

    /// Send within a transaction and require the (provisional) `accepted` outcome.
    pub fn send_accepted_in_txn(&self, msg: &Message, txn_id: &[u8], timeout: Duration) -> Result<()> {
        check_accepted(self.send_in_txn(msg, txn_id, timeout)?)
    }

    /// Detach (close) the link.
    pub fn close(&self) {
        detach_link(&self.shared, self.channel, self.handle, true);
    }

    /// Detach without closing the link (a durable subscription survives).
    pub fn detach(&self) {
        detach_link(&self.shared, self.channel, self.handle, false);
    }
}

fn check_accepted(state: Option<DeliveryState>) -> Result<()> {
    match state.as_ref().map(|s| s.outcome()) {
        None | Some(DeliveryState::Accepted) => Ok(()),
        Some(DeliveryState::Rejected(Some(e))) => Err(amqp_error(e, "Message rejected")),
        Some(other) => Err(amqp_failure(format!("Message not accepted: {:?}", other))),
    }
}

/// A link to a transaction coordinator: declares and discharges local transactions
/// (AMQP 1.0 part 4). Transfers and dispositions join a transaction through
/// [`Sender::send_in_txn`] and [`Receiver::settle_in_txn`].
pub struct Coordinator {
    sender: Sender,
    timeout: Duration,
}

impl Coordinator {
    /// Declare a transaction; returns its id.
    pub fn declare(&self) -> Result<Vec<u8>> {
        let msg = Message { body: crate::message::Body::Value(AmqpValue::composite(desc::DECLARE, vec![])), ..Default::default() };
        match self.sender.send(&msg, self.timeout)? {
            Some(DeliveryState::Declared(id)) => Ok(id),
            Some(DeliveryState::Rejected(Some(e))) => Err(amqp_error(&e, "Transaction declaration refused")),
            other => Err(amqp_failure(format!("Unexpected outcome of a transaction declaration: {:?}", other))),
        }
    }

    /// Commit (`fail = false`) or roll back (`fail = true`) a transaction.
    pub fn discharge(&self, txn_id: &[u8], fail: bool) -> Result<()> {
        let body = AmqpValue::composite(desc::DISCHARGE, vec![AmqpValue::Binary(txn_id.to_vec()), AmqpValue::Boolean(fail)]);
        let msg = Message { body: crate::message::Body::Value(body), ..Default::default() };
        match self.sender.send(&msg, self.timeout)? {
            None | Some(DeliveryState::Accepted) => Ok(()),
            Some(DeliveryState::Rejected(Some(e))) => Err(amqp_error(&e, if fail { "Transaction rollback failed" } else { "Transaction commit failed" })),
            other => Err(amqp_failure(format!("Unexpected outcome of a transaction discharge: {:?}", other))),
        }
    }

    pub fn commit(&self, txn_id: &[u8]) -> Result<()> {
        self.discharge(txn_id, false)
    }

    pub fn rollback(&self, txn_id: &[u8]) -> Result<()> {
        self.discharge(txn_id, true)
    }

    /// Detach the coordinator link (open transactions are rolled back by the peer).
    pub fn close(&self) {
        self.sender.close();
    }
}

/// A receiving link.
pub struct Receiver {
    shared: Arc<Shared>,
    channel: u16,
    handle: u32,
    remote: Attach,
}

impl Receiver {
    /// The peer's attach.
    pub fn remote_attach(&self) -> &Attach {
        &self.remote
    }
    /// The source address (the assigned address of a dynamic node).
    pub fn address(&self) -> Option<&str> {
        self.remote.source.as_ref().and_then(|s| s.address.as_deref())
    }
    /// Current link credit.
    pub fn credit(&self) -> u32 {
        let st = self.shared.lock();
        st.sessions.get(&self.channel).and_then(|s| s.links.get(&self.handle)).map(|l| l.credit).unwrap_or(0)
    }
    /// Number of buffered (prefetched) messages.
    pub fn queued(&self) -> usize {
        let st = self.shared.lock();
        st.sessions.get(&self.channel).and_then(|s| s.links.get(&self.handle)).map(|l| l.queue.len()).unwrap_or(0)
    }

    /// Set the link credit.
    pub fn flow(&self, credit: u32) -> Result<()> {
        let (ch, h) = (self.channel, self.handle);
        self.shared.write_with(|st| {
            check_link(st, ch, h)?;
            st.sessions.get_mut(&ch).unwrap().links.get_mut(&h).unwrap().credit = credit;
            Ok(flow_frame(st, ch, Some(h), false, false).map(|f| f.encode()).unwrap_or_default())
        })
    }

    /// Ask the sender to use up or drop the remaining credit; waits for its answer.
    pub fn drain(&self, timeout: Duration) -> Result<()> {
        let (ch, h) = (self.channel, self.handle);
        let sh = &self.shared;
        sh.write_with(|st| {
            check_link(st, ch, h)?;
            let l = st.sessions.get_mut(&ch).unwrap().links.get_mut(&h).unwrap();
            if l.credit == 0 {
                return Ok(Vec::new());
            }
            l.drain_pending = true;
            Ok(flow_frame(st, ch, Some(h), true, false).map(|f| f.encode()).unwrap_or_default())
        })?;
        let st = sh.lock();
        drop(sh.wait(st, Instant::now() + timeout, "drain", |st| {
            check_link(st, ch, h)?;
            Ok(!st.sessions[&ch].links[&h].drain_pending)
        })?);
        Ok(())
    }

    /// Receive the next message (`None` on timeout). With an automatic credit window the
    /// credit is topped up when half of it was used.
    pub fn receive(&self, timeout: Duration) -> Result<Option<Delivery>> {
        let (ch, h) = (self.channel, self.handle);
        let sh = &self.shared;
        let deadline = Instant::now() + timeout;
        let mut st = sh.lock();
        loop {
            let topup;
            {
                let l = st.sessions.get_mut(&ch).and_then(|s| s.links.get_mut(&h));
                let Some(l) = l else { return Err(amqp_failure("link closed")) };
                if let Some(d) = l.queue.pop_front() {
                    topup = l.prefetch > 0 && l.credit + (l.queue.len() as u32) <= l.prefetch / 2;
                    drop(st);
                    if topup {
                        let _ = sh.write_with(|st| {
                            let l = st.sessions.get_mut(&ch).and_then(|s| s.links.get_mut(&h));
                            let Some(l) = l else { return Ok(Vec::new()) };
                            l.credit = l.prefetch.saturating_sub(l.queue.len() as u32);
                            Ok(flow_frame(st, ch, Some(h), false, false).map(|f| f.encode()).unwrap_or_default())
                        });
                    }
                    return Ok(Some(d));
                }
            }
            check_link(&st, ch, h)?;
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            st = sh.cond.wait_timeout(st, (deadline - now).min(Duration::from_millis(200))).unwrap_or_else(|p| p.into_inner()).0;
        }
    }

    /// Settle a delivery with an outcome. In `rcv-settle-mode = second` the outcome is
    /// sent unsettled and this waits until the sender has settled the delivery.
    pub fn settle(&self, d: &Delivery, state: DeliveryState) -> Result<()> {
        if d.settled {
            return Ok(());
        }
        let (ch, h) = (self.channel, self.handle);
        let mut second = false;
        self.shared.write_with(|st| {
            check_session(st, ch)?;
            let s = st.sessions.get_mut(&ch).unwrap();
            second = s.links.get(&h).is_some_and(|l| l.rcv_second);
            if second {
                s.peer_settled.remove(&d.delivery_id);
            }
            Ok(Frame::amqp(ch, Performative::Disposition(Disposition { role: Role::Receiver, first: d.delivery_id, last: None, settled: !second, state: Some(state), batchable: false })).encode())
        })?;
        if second {
            let sh = &self.shared;
            let st = sh.lock();
            let id = d.delivery_id;
            let mut st = sh.wait(st, Instant::now() + sh.opts.request_timeout, "settlement by the sender", |st| {
                check_session(st, ch)?;
                Ok(st.sessions[&ch].peer_settled.contains(&id))
            })?;
            st.sessions.get_mut(&ch).unwrap().peer_settled.remove(&id);
        }
        Ok(())
    }
    /// Settle a delivery within a transaction: the outcome takes effect when the
    /// transaction commits (a rollback makes the message available again).
    pub fn settle_in_txn(&self, d: &Delivery, txn_id: &[u8], outcome: DeliveryState) -> Result<()> {
        self.settle(d, DeliveryState::Transactional { txn_id: txn_id.to_vec(), outcome: Some(Box::new(outcome)) })
    }
    /// Whether dispositions use `rcv-settle-mode = second` (agreed with the sender).
    pub fn settles_second(&self) -> bool {
        let st = self.shared.lock();
        st.sessions.get(&self.channel).and_then(|s| s.links.get(&self.handle)).is_some_and(|l| l.rcv_second)
    }
    pub fn accept(&self, d: &Delivery) -> Result<()> {
        self.settle(d, DeliveryState::Accepted)
    }
    pub fn reject(&self, d: &Delivery, error: Option<AmqpError>) -> Result<()> {
        self.settle(d, DeliveryState::Rejected(error))
    }
    pub fn release(&self, d: &Delivery) -> Result<()> {
        self.settle(d, DeliveryState::Released)
    }
    pub fn modify(&self, d: &Delivery, delivery_failed: bool, undeliverable_here: bool, annotations: Vec<(AmqpValue, AmqpValue)>) -> Result<()> {
        self.settle(d, DeliveryState::Modified { delivery_failed, undeliverable_here, message_annotations: annotations })
    }

    /// Detach (close) the link.
    pub fn close(&self) {
        detach_link(&self.shared, self.channel, self.handle, true);
    }

    /// Detach without closing the link (a durable subscription survives).
    pub fn detach(&self) {
        detach_link(&self.shared, self.channel, self.handle, false);
    }
}

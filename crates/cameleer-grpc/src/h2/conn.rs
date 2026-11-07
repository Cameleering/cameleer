//! The HTTP/2 connection engine shared by the client and the server: settings exchange,
//! stream state machine, flow control, HPACK contexts, PING, GOAWAY and RST_STREAM.
//!
//! Threading model: one reader thread per connection parses frames and updates the shared
//! state; one writer thread drains an outgoing queue, so the reader never blocks on the
//! socket (avoiding the classic "both peers blocked writing" deadlock). Application
//! threads use [`H2Stream`] handles, which block on a condition variable for incoming
//! data and for flow-control credit.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io::{BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::frame::{flags, frame_type, settings_id, ErrorCode, Frame, ReadError, PREFACE};
use super::transport::Transport;
use crate::hpack::{Decoder, Encoder, HeaderList};

/// Largest legal flow-control window.
pub const MAX_WINDOW: i64 = (1 << 31) - 1;
/// The protocol default initial window (RFC 9113 6.9.2).
pub const DEFAULT_WINDOW: u32 = 65_535;
const MAX_HEADER_BLOCK: usize = 1 << 20;

/// Connection settings for our side.
#[derive(Clone, Debug)]
pub struct H2Config {
    /// SETTINGS_MAX_CONCURRENT_STREAMS we allow the peer to open (server side).
    pub max_concurrent_streams: u32,
    /// SETTINGS_INITIAL_WINDOW_SIZE: per-stream receive window.
    pub initial_window_size: u32,
    /// Connection-level receive window (raised with a WINDOW_UPDATE after the preface).
    pub connection_window_size: u32,
    /// SETTINGS_MAX_FRAME_SIZE we accept.
    pub max_frame_size: u32,
    /// SETTINGS_HEADER_TABLE_SIZE for our HPACK decoder.
    pub header_table_size: u32,
    /// SETTINGS_MAX_HEADER_LIST_SIZE we accept (0 = do not advertise / unlimited).
    pub max_header_list_size: u32,
    /// Timeout for the TCP connect and the TLS/preface handshake.
    pub connect_timeout: Duration,
    /// Socket write timeout.
    pub write_timeout: Duration,
}

impl Default for H2Config {
    fn default() -> Self {
        H2Config {
            max_concurrent_streams: 256,
            initial_window_size: 1 << 20,
            connection_window_size: 4 << 20,
            max_frame_size: 16_384,
            header_table_size: 4096,
            max_header_list_size: 64 * 1024,
            connect_timeout: Duration::from_secs(10),
            write_timeout: Duration::from_secs(30),
        }
    }
}

impl H2Config {
    /// Protocol defaults (64 KiB - 1 windows): useful to exercise flow control.
    pub fn protocol_defaults() -> Self {
        H2Config { initial_window_size: DEFAULT_WINDOW, connection_window_size: DEFAULT_WINDOW, ..H2Config::default() }
    }
}

/// HTTP/2 errors surfaced to applications.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum H2Error {
    /// I/O failure or connection closed.
    Io(String),
    /// A connection error detected locally or reported by the peer.
    Protocol(ErrorCode, String),
    /// The stream was reset (RST_STREAM) with the given code.
    StreamReset(ErrorCode),
    /// The peer sent GOAWAY and the stream was not processed.
    GoAway(ErrorCode),
    /// A blocking operation timed out.
    Timeout,
    /// The connection is closed.
    Closed(String),
}

impl fmt::Display for H2Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            H2Error::Io(m) => write!(f, "I/O error: {}", m),
            H2Error::Protocol(c, m) => write!(f, "HTTP/2 connection error {}: {}", c, m),
            H2Error::StreamReset(c) => write!(f, "HTTP/2 stream reset: {}", c),
            H2Error::GoAway(c) => write!(f, "HTTP/2 GOAWAY received: {}", c),
            H2Error::Timeout => write!(f, "HTTP/2 operation timed out"),
            H2Error::Closed(m) => write!(f, "HTTP/2 connection closed: {}", m),
        }
    }
}

impl std::error::Error for H2Error {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    Client,
    Server,
}

#[derive(Debug)]
struct StreamState {
    headers: VecDeque<HeaderList>,
    data: VecDeque<Vec<u8>>,
    remote_end: bool,
    local_end: bool,
    reset: Option<ErrorCode>,
    send_window: i64,
    recv_window: i64,
    recv_unacked: u32,
}

impl StreamState {
    fn new(send_window: i64, recv_window: i64) -> Self {
        StreamState {
            headers: VecDeque::new(),
            data: VecDeque::new(),
            remote_end: false,
            local_end: false,
            reset: None,
            send_window,
            recv_window,
            recv_unacked: 0,
        }
    }
    fn active(&self) -> bool {
        self.reset.is_none() && !(self.local_end && self.remote_end)
    }
}

#[derive(Debug, Clone, Copy)]
struct PeerSettings {
    initial_window: u32,
    max_frame_size: u32,
    max_concurrent: u32,
}

struct ConnState {
    streams: HashMap<u32, StreamState>,
    peer: PeerSettings,
    send_window: i64,
    recv_window: i64,
    recv_unacked: u32,
    next_stream_id: u32,
    last_peer_stream: u32,
    reserved: u32,
    goaway_received: Option<(u32, ErrorCode)>,
    goaway_sent: bool,
    closed: Option<H2Error>,
    pings: HashMap<[u8; 8], bool>,
    settings_acked: bool,
}

impl ConnState {
    fn active_count(&self) -> u32 {
        self.streams.values().filter(|s| s.active()).count() as u32
    }
}

enum Out {
    Bytes(Vec<u8>),
    Close,
}

struct OutState {
    encoder: Encoder,
    tx: Sender<Out>,
    max_frame_size: usize,
}

/// Callback invoked (on the reader thread) for each new peer-initiated stream.
pub(crate) type NewStreamFn = Box<dyn Fn(H2Stream, HeaderList) + Send>;

pub(crate) struct Shared {
    role: Role,
    pub(crate) config: H2Config,
    out: Mutex<OutState>,
    state: Mutex<ConnState>,
    cond: Condvar,
    socket: TcpStream,
    pub(crate) peer_addr: Option<SocketAddr>,
    pub(crate) local_addr: Option<SocketAddr>,
    pub(crate) tls: bool,
    pub(crate) alpn: Option<Vec<u8>>,
    ping_seq: AtomicU64,
}

fn conn_err(code: ErrorCode, msg: impl Into<String>) -> H2Error {
    H2Error::Protocol(code, msg.into())
}

impl Shared {
    /// Performs the connection preface + SETTINGS exchange start and spawns the reader
    /// and writer threads.
    pub(crate) fn start(role: Role, transport: Transport, config: H2Config, on_new_stream: Option<NewStreamFn>) -> Result<Arc<Shared>, H2Error> {
        Self::start_with(role, transport, config, on_new_stream, false)
    }

    /// `client_upgraded`: the client already sent its first request (stream 1) as the
    /// HTTP/1.1 `Upgrade: h2c` request. Servers accept both prior knowledge and the upgrade.
    pub(crate) fn start_with(role: Role, transport: Transport, config: H2Config, on_new_stream: Option<NewStreamFn>, client_upgraded: bool) -> Result<Arc<Shared>, H2Error> {
        let Transport { mut reader, mut writer, socket, alpn, tls, peer_addr, local_addr } = transport;
        let _ = socket.set_write_timeout(Some(config.write_timeout));
        let mut upgraded: Option<super::upgrade::UpgradeRequest> = None;
        if role == Role::Server {
            // read and check the client preface
            let _ = socket.set_read_timeout(Some(config.connect_timeout));
            let mut pre = [0u8; 24];
            read_full(&mut reader, &mut pre).map_err(|e| H2Error::Io(format!("reading connection preface: {}", e)))?;
            if &pre != PREFACE {
                // HTTP/1.1 request with `Upgrade: h2c` (RFC 7540 section 3.2)?
                match super::upgrade::read_upgrade_request(&pre, &mut reader) {
                    Ok(Some(req)) => {
                        writer
                            .write_all(b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: h2c\r\n\r\n")
                            .and_then(|_| writer.flush())
                            .map_err(|e| H2Error::Io(format!("writing 101 response: {}", e)))?;
                        read_full(&mut reader, &mut pre).map_err(|e| H2Error::Io(format!("reading connection preface: {}", e)))?;
                        if &pre != PREFACE {
                            let _ = socket.shutdown(Shutdown::Both);
                            return Err(conn_err(ErrorCode::PROTOCOL_ERROR, "invalid connection preface after the h2c upgrade"));
                        }
                        upgraded = Some(req);
                    }
                    Ok(None) => {
                        let _ = writer.write_all(b"HTTP/1.1 505 HTTP Version Not Supported\r\nConnection: close\r\nContent-Length: 0\r\n\r\n");
                        let _ = socket.shutdown(Shutdown::Both);
                        return Err(conn_err(ErrorCode::PROTOCOL_ERROR, "HTTP/1.1 request without Upgrade: h2c"));
                    }
                    Err(_) => {
                        let _ = socket.shutdown(Shutdown::Both);
                        return Err(conn_err(ErrorCode::PROTOCOL_ERROR, "invalid connection preface (not an HTTP/2 prior-knowledge client?)"));
                    }
                }
            }
            let _ = socket.set_read_timeout(None);
        }
        let (tx, rx) = channel();
        let mut decoder = Decoder::new(config.header_table_size as usize);
        decoder.max_header_list_size = config.max_header_list_size as usize;
        let shared = Arc::new(Shared {
            role,
            out: Mutex::new(OutState { encoder: Encoder::new(), tx, max_frame_size: 16_384 }),
            state: Mutex::new(ConnState {
                streams: HashMap::new(),
                peer: PeerSettings { initial_window: DEFAULT_WINDOW, max_frame_size: 16_384, max_concurrent: u32::MAX },
                send_window: DEFAULT_WINDOW as i64,
                recv_window: config.connection_window_size.max(DEFAULT_WINDOW) as i64,
                recv_unacked: 0,
                next_stream_id: if role == Role::Client { 1 } else { 2 },
                last_peer_stream: 0,
                reserved: 0,
                goaway_received: None,
                goaway_sent: false,
                closed: None,
                pings: HashMap::new(),
                settings_acked: false,
            }),
            cond: Condvar::new(),
            socket,
            peer_addr,
            local_addr,
            tls,
            alpn,
            ping_seq: AtomicU64::new(1),
            config,
        });
        // our preface: (client magic) + SETTINGS (+ WINDOW_UPDATE)
        let mut first = Vec::new();
        if role == Role::Client {
            first.extend_from_slice(PREFACE);
        }
        let c = &shared.config;
        let mut settings = vec![
            (settings_id::HEADER_TABLE_SIZE, c.header_table_size),
            (settings_id::ENABLE_PUSH, 0),
            (settings_id::INITIAL_WINDOW_SIZE, c.initial_window_size.min(MAX_WINDOW as u32)),
            (settings_id::MAX_FRAME_SIZE, c.max_frame_size.clamp(16_384, (1 << 24) - 1)),
        ];
        if role == Role::Server {
            settings.push((settings_id::MAX_CONCURRENT_STREAMS, c.max_concurrent_streams));
        }
        if c.max_header_list_size > 0 {
            settings.push((settings_id::MAX_HEADER_LIST_SIZE, c.max_header_list_size));
        }
        Frame::settings(&settings).encode(&mut first);
        if c.connection_window_size > DEFAULT_WINDOW {
            Frame::window_update(0, c.connection_window_size - DEFAULT_WINDOW).encode(&mut first);
        }
        shared.queue(first)?;
        if client_upgraded {
            // stream 1 carried the upgrade request: its response is read (and ignored) like
            // any other stream; new requests start at 3
            let mut st = shared.lock();
            let mut s = StreamState::new(DEFAULT_WINDOW as i64, shared.config.initial_window_size as i64);
            s.local_end = true;
            st.streams.insert(1, s);
            st.next_stream_id = 3;
        }
        if let Some(req) = upgraded {
            {
                let mut st = shared.lock();
                for (id, v) in &req.settings {
                    match *id {
                        settings_id::INITIAL_WINDOW_SIZE => st.peer.initial_window = *v,
                        settings_id::MAX_FRAME_SIZE => st.peer.max_frame_size = *v,
                        settings_id::MAX_CONCURRENT_STREAMS => st.peer.max_concurrent = *v,
                        _ => {}
                    }
                }
                let mut s = StreamState::new(st.peer.initial_window as i64, shared.config.initial_window_size as i64);
                s.remote_end = true;
                if !req.body.is_empty() {
                    s.data.push_back(req.body.clone());
                }
                st.streams.insert(1, s);
                st.last_peer_stream = 1;
            }
            if let Some(cb) = on_new_stream.as_ref() {
                cb(H2Stream { shared: shared.clone(), id: 1 }, req.headers.clone());
            }
        }

        let w = shared.clone();
        std::thread::Builder::new()
            .name(format!("h2-writer-{:?}", role).to_lowercase())
            .spawn(move || w.writer_loop(writer, rx))
            .map_err(|e| H2Error::Io(e.to_string()))?;
        let r = shared.clone();
        std::thread::Builder::new()
            .name(format!("h2-reader-{:?}", role).to_lowercase())
            .spawn(move || r.reader_loop(reader, decoder, on_new_stream))
            .map_err(|e| H2Error::Io(e.to_string()))?;
        Ok(shared)
    }

    fn lock(&self) -> MutexGuard<'_, ConnState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lock_out(&self) -> MutexGuard<'_, OutState> {
        self.out.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn queue(&self, bytes: Vec<u8>) -> Result<(), H2Error> {
        self.lock_out().tx.send(Out::Bytes(bytes)).map_err(|_| H2Error::Closed("writer stopped".into()))
    }

    fn send_frame(&self, f: Frame) {
        let _ = self.queue(f.to_bytes());
    }

    fn writer_loop(self: Arc<Self>, mut w: Box<dyn Write + Send>, rx: Receiver<Out>) {
        let mut buf = Vec::with_capacity(64 * 1024);
        'outer: while let Ok(msg) = rx.recv() {
            let mut close = false;
            match msg {
                Out::Bytes(b) => buf.extend_from_slice(&b),
                Out::Close => close = true,
            }
            while !close && buf.len() < 256 * 1024 {
                match rx.try_recv() {
                    Ok(Out::Bytes(b)) => buf.extend_from_slice(&b),
                    Ok(Out::Close) => close = true,
                    Err(_) => break,
                }
            }
            if !buf.is_empty() {
                if let Err(e) = w.write_all(&buf).and_then(|_| w.flush()) {
                    self.fail(H2Error::Io(format!("write failed: {}", e)));
                    break 'outer;
                }
                buf.clear();
            }
            if close {
                break;
            }
        }
        let _ = w.flush();
        let _ = self.socket.shutdown(Shutdown::Both);
    }

    /// Marks the connection as failed/closed and wakes everybody.
    fn fail(&self, err: H2Error) {
        let mut st = self.lock();
        if st.closed.is_none() {
            st.closed = Some(err);
        }
        drop(st);
        self.cond.notify_all();
    }

    /// Sends GOAWAY with `code` and closes the connection.
    fn connection_error(&self, code: ErrorCode, msg: &str) {
        let last = self.lock().last_peer_stream;
        self.send_frame(Frame::goaway(last, code, msg.as_bytes()));
        let _ = self.lock_out().tx.send(Out::Close);
        self.fail(conn_err(code, msg));
    }

    fn reader_loop(self: Arc<Self>, reader: Box<dyn Read + Send>, mut decoder: Decoder, on_new_stream: Option<NewStreamFn>) {
        let mut reader = BufReader::with_capacity(64 * 1024, reader);
        let max = self.config.max_frame_size.clamp(16_384, (1 << 24) - 1) as usize;
        // pending header block: (stream id, end_stream, fragments)
        let mut cont: Option<(u32, bool, Vec<u8>)> = None;
        let mut first = true;
        loop {
            let frame = match Frame::read_from(&mut reader, max) {
                Ok(Some(f)) => f,
                Ok(None) => {
                    self.fail(H2Error::Closed("connection closed by peer".into()));
                    break;
                }
                Err(ReadError::Io(e)) => {
                    self.fail(H2Error::Io(e.to_string()));
                    break;
                }
                Err(ReadError::Protocol(code, msg)) => {
                    self.connection_error(code, &msg);
                    break;
                }
            };
            if first {
                first = false;
                if frame.typ != frame_type::SETTINGS || frame.has(flags::ACK) {
                    self.connection_error(ErrorCode::PROTOCOL_ERROR, "first frame must be SETTINGS");
                    break;
                }
            }
            let result = self.handle_frame(frame, &mut cont, &mut decoder, on_new_stream.as_ref());
            if let Err((code, msg)) = result {
                self.connection_error(code, &msg);
                break;
            }
            if self.lock().closed.is_some() {
                break;
            }
        }
        // fail all streams, and stop the writer: it holds the socket, and its channel never
        // disconnects on its own (the writer keeps the shared state that owns the sender), so
        // without this every connection the peer closed would keep a thread and its descriptors
        self.cond.notify_all();
        let _ = self.lock_out().tx.send(Out::Close);
    }

    fn is_idle(&self, st: &ConnState, id: u32) -> bool {
        let ours = (id % 2 == 1) == (self.role == Role::Client);
        if ours {
            id >= st.next_stream_id
        } else {
            id > st.last_peer_stream
        }
    }

    fn handle_frame(
        self: &Arc<Self>,
        f: Frame,
        cont: &mut Option<(u32, bool, Vec<u8>)>,
        decoder: &mut Decoder,
        on_new: Option<&NewStreamFn>,
    ) -> Result<(), (ErrorCode, String)> {
        use frame_type::*;
        let perr = |m: &str| Err((ErrorCode::PROTOCOL_ERROR, m.to_string()));
        if let Some((sid, _, _)) = cont.as_ref() {
            if f.typ != CONTINUATION || f.stream_id != *sid {
                return perr("expected CONTINUATION frame");
            }
        }
        match f.typ {
            DATA => self.on_data(&f),
            HEADERS => {
                if f.stream_id == 0 {
                    return perr("HEADERS on stream 0");
                }
                let frag = f.header_fragment().map_err(|c| (c, "malformed HEADERS".to_string()))?;
                let end_stream = f.has(flags::END_STREAM);
                if f.has(flags::END_HEADERS) {
                    let block = frag.to_vec();
                    self.on_header_block(f.stream_id, end_stream, &block, decoder, on_new)
                } else {
                    *cont = Some((f.stream_id, end_stream, frag.to_vec()));
                    Ok(())
                }
            }
            CONTINUATION => {
                let Some((sid, end_stream, mut block)) = cont.take() else {
                    return perr("CONTINUATION without HEADERS");
                };
                block.extend_from_slice(&f.payload);
                if block.len() > MAX_HEADER_BLOCK {
                    return Err((ErrorCode::ENHANCE_YOUR_CALM, "header block too large".into()));
                }
                if f.has(flags::END_HEADERS) {
                    self.on_header_block(sid, end_stream, &block, decoder, on_new)
                } else {
                    *cont = Some((sid, end_stream, block));
                    Ok(())
                }
            }
            PRIORITY => {
                if f.stream_id == 0 {
                    return perr("PRIORITY on stream 0");
                }
                if f.payload.len() != 5 {
                    self.send_frame(Frame::rst_stream(f.stream_id, ErrorCode::FRAME_SIZE_ERROR));
                }
                Ok(())
            }
            RST_STREAM => {
                if f.stream_id == 0 {
                    return perr("RST_STREAM on stream 0");
                }
                if f.payload.len() != 4 {
                    return Err((ErrorCode::FRAME_SIZE_ERROR, "RST_STREAM length".into()));
                }
                let code = ErrorCode(f.u32_at(0).unwrap_or(0));
                let mut st = self.lock();
                if self.is_idle(&st, f.stream_id) {
                    return perr("RST_STREAM on idle stream");
                }
                if let Some(s) = st.streams.get_mut(&f.stream_id) {
                    s.reset = Some(code);
                }
                drop(st);
                self.cond.notify_all();
                Ok(())
            }
            SETTINGS => self.on_settings(&f, decoder),
            PUSH_PROMISE => perr("PUSH_PROMISE not enabled"),
            PING => {
                if f.stream_id != 0 {
                    return perr("PING on a stream");
                }
                if f.payload.len() != 8 {
                    return Err((ErrorCode::FRAME_SIZE_ERROR, "PING length".into()));
                }
                let mut data = [0u8; 8];
                data.copy_from_slice(&f.payload);
                if f.has(flags::ACK) {
                    let mut st = self.lock();
                    if let Some(v) = st.pings.get_mut(&data) {
                        *v = true;
                    }
                    drop(st);
                    self.cond.notify_all();
                } else {
                    self.send_frame(Frame::ping(data, true));
                }
                Ok(())
            }
            GOAWAY => {
                if f.stream_id != 0 {
                    return perr("GOAWAY on a stream");
                }
                if f.payload.len() < 8 {
                    return Err((ErrorCode::FRAME_SIZE_ERROR, "GOAWAY length".into()));
                }
                let last = f.u32_at(0).unwrap_or(0) & 0x7fff_ffff;
                let code = ErrorCode(f.u32_at(4).unwrap_or(0));
                let mut st = self.lock();
                st.goaway_received = Some((last, code));
                let role = self.role;
                for (id, s) in st.streams.iter_mut() {
                    let ours = (id % 2 == 1) == (role == Role::Client);
                    if ours && *id > last && s.reset.is_none() {
                        s.reset = Some(ErrorCode::REFUSED_STREAM);
                    }
                }
                drop(st);
                self.cond.notify_all();
                Ok(())
            }
            WINDOW_UPDATE => {
                if f.payload.len() != 4 {
                    return Err((ErrorCode::FRAME_SIZE_ERROR, "WINDOW_UPDATE length".into()));
                }
                let inc = (f.u32_at(0).unwrap_or(0) & 0x7fff_ffff) as i64;
                let mut st = self.lock();
                if f.stream_id == 0 {
                    if inc == 0 {
                        return perr("WINDOW_UPDATE with zero increment");
                    }
                    st.send_window += inc;
                    if st.send_window > MAX_WINDOW {
                        return Err((ErrorCode::FLOW_CONTROL_ERROR, "connection window overflow".into()));
                    }
                } else {
                    if self.is_idle(&st, f.stream_id) {
                        return perr("WINDOW_UPDATE on idle stream");
                    }
                    let mut rst = None;
                    if let Some(s) = st.streams.get_mut(&f.stream_id) {
                        if inc == 0 {
                            rst = Some(ErrorCode::PROTOCOL_ERROR);
                        } else {
                            s.send_window += inc;
                            if s.send_window > MAX_WINDOW {
                                rst = Some(ErrorCode::FLOW_CONTROL_ERROR);
                            }
                        }
                        if let Some(c) = rst {
                            s.reset = Some(c);
                        }
                    }
                    if let Some(c) = rst {
                        drop(st);
                        self.send_frame(Frame::rst_stream(f.stream_id, c));
                        self.cond.notify_all();
                        return Ok(());
                    }
                }
                drop(st);
                self.cond.notify_all();
                Ok(())
            }
            _ => Ok(()), // unknown frame types are ignored
        }
    }

    fn on_settings(&self, f: &Frame, decoder: &mut Decoder) -> Result<(), (ErrorCode, String)> {
        if f.stream_id != 0 {
            return Err((ErrorCode::PROTOCOL_ERROR, "SETTINGS on a stream".into()));
        }
        if f.has(flags::ACK) {
            if !f.payload.is_empty() {
                return Err((ErrorCode::FRAME_SIZE_ERROR, "SETTINGS ACK with payload".into()));
            }
            self.lock().settings_acked = true;
            decoder.set_max_allowed(self.config.header_table_size as usize);
            return Ok(());
        }
        let values = f.parse_settings().map_err(|c| (c, "SETTINGS length".to_string()))?;
        let mut table_size = None;
        let mut frame_size = None;
        {
            let mut st = self.lock();
            for (id, v) in values {
                match id {
                    settings_id::HEADER_TABLE_SIZE => table_size = Some(v),
                    settings_id::ENABLE_PUSH => {
                        if v > 1 || (v == 1 && self.role == Role::Client) {
                            return Err((ErrorCode::PROTOCOL_ERROR, "invalid SETTINGS_ENABLE_PUSH".into()));
                        }
                    }
                    settings_id::MAX_CONCURRENT_STREAMS => st.peer.max_concurrent = v,
                    settings_id::INITIAL_WINDOW_SIZE => {
                        if v as i64 > MAX_WINDOW {
                            return Err((ErrorCode::FLOW_CONTROL_ERROR, "SETTINGS_INITIAL_WINDOW_SIZE too large".into()));
                        }
                        let delta = v as i64 - st.peer.initial_window as i64;
                        st.peer.initial_window = v;
                        for s in st.streams.values_mut() {
                            s.send_window += delta;
                            if s.send_window > MAX_WINDOW {
                                return Err((ErrorCode::FLOW_CONTROL_ERROR, "stream window overflow".into()));
                            }
                        }
                    }
                    settings_id::MAX_FRAME_SIZE => {
                        if !(16_384..(1 << 24)).contains(&v) {
                            return Err((ErrorCode::PROTOCOL_ERROR, "invalid SETTINGS_MAX_FRAME_SIZE".into()));
                        }
                        st.peer.max_frame_size = v;
                        frame_size = Some(v);
                    }
                    _ => {}
                }
            }
        }
        {
            let mut out = self.lock_out();
            if let Some(t) = table_size {
                out.encoder.set_max_table_size((t as usize).min(64 * 1024));
            }
            if let Some(m) = frame_size {
                out.max_frame_size = m as usize;
            }
            let _ = out.tx.send(Out::Bytes(Frame::settings_ack().to_bytes()));
        }
        self.cond.notify_all();
        Ok(())
    }

    fn on_data(&self, f: &Frame) -> Result<(), (ErrorCode, String)> {
        if f.stream_id == 0 {
            return Err((ErrorCode::PROTOCOL_ERROR, "DATA on stream 0".into()));
        }
        let (data, padding) = f.unpadded().map_err(|c| (c, "malformed DATA".to_string()))?;
        let len = f.payload.len() as i64;
        let mut st = self.lock();
        if len > st.recv_window {
            return Err((ErrorCode::FLOW_CONTROL_ERROR, "connection receive window exceeded".into()));
        }
        st.recv_window -= len;
        let idle = self.is_idle(&st, f.stream_id);
        let release_conn;
        let mut rst = None;
        match st.streams.get_mut(&f.stream_id) {
            Some(s) if !s.remote_end && s.reset.is_none() => {
                if len > s.recv_window {
                    s.reset = Some(ErrorCode::FLOW_CONTROL_ERROR);
                    rst = Some(ErrorCode::FLOW_CONTROL_ERROR);
                    release_conn = len as u32;
                } else {
                    s.recv_window -= len;
                    if !data.is_empty() {
                        s.data.push_back(data.to_vec());
                    }
                    // padding is consumed immediately
                    s.recv_unacked += padding as u32;
                    release_conn = padding as u32;
                    if f.has(flags::END_STREAM) {
                        s.remote_end = true;
                    }
                }
            }
            Some(s) => {
                release_conn = len as u32;
                if s.reset.is_none() {
                    rst = Some(ErrorCode::STREAM_CLOSED);
                    s.reset = Some(ErrorCode::STREAM_CLOSED);
                }
            }
            None => {
                if idle {
                    return Err((ErrorCode::PROTOCOL_ERROR, "DATA on idle stream".into()));
                }
                release_conn = len as u32;
            }
        }
        let wu = self.release_conn_locked(&mut st, release_conn);
        drop(st);
        if let Some(c) = rst {
            self.send_frame(Frame::rst_stream(f.stream_id, c));
        }
        if let Some(inc) = wu {
            self.send_frame(Frame::window_update(0, inc));
        }
        self.cond.notify_all();
        Ok(())
    }

    /// Accounts consumed connection-level bytes; returns a WINDOW_UPDATE increment when due.
    fn release_conn_locked(&self, st: &mut ConnState, n: u32) -> Option<u32> {
        st.recv_unacked += n;
        let target = self.config.connection_window_size.max(DEFAULT_WINDOW);
        if st.recv_unacked > 0 && st.recv_unacked >= target / 2 {
            let inc = st.recv_unacked;
            st.recv_unacked = 0;
            st.recv_window += inc as i64;
            return Some(inc);
        }
        None
    }

    fn on_header_block(
        self: &Arc<Self>,
        id: u32,
        end_stream: bool,
        block: &[u8],
        decoder: &mut Decoder,
        on_new: Option<&NewStreamFn>,
    ) -> Result<(), (ErrorCode, String)> {
        let headers = decoder.decode(block).map_err(|e| (ErrorCode::COMPRESSION_ERROR, e.to_string()))?;
        let mut st = self.lock();
        if let Some(s) = st.streams.get_mut(&id) {
            if s.remote_end || s.reset.is_some() {
                if s.reset.is_none() {
                    s.reset = Some(ErrorCode::STREAM_CLOSED);
                }
                drop(st);
                self.send_frame(Frame::rst_stream(id, ErrorCode::STREAM_CLOSED));
                self.cond.notify_all();
                return Ok(());
            }
            s.headers.push_back(headers);
            if end_stream {
                s.remote_end = true;
            }
            drop(st);
            self.cond.notify_all();
            return Ok(());
        }
        let peer_initiated = (id % 2 == 1) == (self.role == Role::Server);
        if !peer_initiated {
            if self.is_idle(&st, id) {
                return Err((ErrorCode::PROTOCOL_ERROR, "HEADERS on idle stream".into()));
            }
            return Ok(()); // a stream we already closed
        }
        if self.role == Role::Client {
            return Err((ErrorCode::PROTOCOL_ERROR, "server-initiated stream".into()));
        }
        if id <= st.last_peer_stream {
            return Err((ErrorCode::STREAM_CLOSED, "HEADERS on closed stream".into()));
        }
        st.last_peer_stream = id;
        let refuse = st.goaway_sent || st.closed.is_some() || st.active_count() >= self.config.max_concurrent_streams || on_new.is_none();
        if refuse {
            drop(st);
            self.send_frame(Frame::rst_stream(id, ErrorCode::REFUSED_STREAM));
            return Ok(());
        }
        let malformed = headers.iter().any(|(n, _)| n.bytes().any(|b| b.is_ascii_uppercase()))
            || !headers.iter().any(|(n, _)| n == ":method")
            || (!headers.iter().any(|(n, _)| n == ":path") && !headers.iter().any(|(n, v)| n == ":method" && v == "CONNECT"));
        if malformed {
            drop(st);
            self.send_frame(Frame::rst_stream(id, ErrorCode::PROTOCOL_ERROR));
            return Ok(());
        }
        let mut s = StreamState::new(st.peer.initial_window as i64, self.config.initial_window_size as i64);
        s.remote_end = end_stream;
        st.streams.insert(id, s);
        drop(st);
        if let Some(cb) = on_new {
            cb(H2Stream { shared: self.clone(), id }, headers);
        }
        Ok(())
    }

    // ------------------------------------------------------------------ client API

    /// Opens a new client stream by sending HEADERS. Blocks while the peer's
    /// SETTINGS_MAX_CONCURRENT_STREAMS limit is reached.
    pub(crate) fn open_stream(self: &Arc<Self>, headers: &[(String, String)], end_stream: bool, timeout: Option<Duration>) -> Result<H2Stream, H2Error> {
        let deadline = timeout.map(|t| Instant::now() + t);
        {
            let mut st = self.lock();
            loop {
                if let Some(e) = &st.closed {
                    return Err(e.clone());
                }
                if let Some((_, code)) = st.goaway_received {
                    return Err(H2Error::GoAway(code));
                }
                if st.next_stream_id > MAX_WINDOW as u32 {
                    return Err(H2Error::Closed("stream ids exhausted".into()));
                }
                if st.active_count() + st.reserved < st.peer.max_concurrent {
                    break;
                }
                st = self.wait(st, deadline)?;
            }
            st.reserved += 1;
        }
        let mut out = self.lock_out();
        let id = {
            let mut st = self.lock();
            st.reserved -= 1;
            let id = st.next_stream_id;
            st.next_stream_id += 2;
            let mut s = StreamState::new(st.peer.initial_window as i64, self.config.initial_window_size as i64);
            s.local_end = end_stream;
            st.streams.insert(id, s);
            id
        };
        let block = out.encoder.encode(headers);
        let mut bytes = Vec::new();
        for f in Frame::headers(id, &block, end_stream, out.max_frame_size) {
            f.encode(&mut bytes);
        }
        out.tx.send(Out::Bytes(bytes)).map_err(|_| H2Error::Closed("writer stopped".into()))?;
        drop(out);
        Ok(H2Stream { shared: self.clone(), id })
    }

    fn wait<'a>(&self, st: MutexGuard<'a, ConnState>, deadline: Option<Instant>) -> Result<MutexGuard<'a, ConnState>, H2Error> {
        match deadline {
            None => Ok(self.cond.wait(st).unwrap_or_else(|e| e.into_inner())),
            Some(d) => {
                let now = Instant::now();
                if now >= d {
                    return Err(H2Error::Timeout);
                }
                Ok(self.cond.wait_timeout(st, d - now).unwrap_or_else(|e| e.into_inner()).0)
            }
        }
    }

    /// Sends a PING and waits for the ACK; returns the round-trip time.
    pub(crate) fn ping(&self, timeout: Duration) -> Result<Duration, H2Error> {
        let seq = self.ping_seq.fetch_add(1, Ordering::Relaxed);
        let data = seq.to_be_bytes();
        self.lock().pings.insert(data, false);
        let start = Instant::now();
        self.send_frame(Frame::ping(data, false));
        let deadline = Some(start + timeout);
        let mut st = self.lock();
        loop {
            if st.pings.get(&data) == Some(&true) {
                st.pings.remove(&data);
                return Ok(start.elapsed());
            }
            if let Some(e) = &st.closed {
                return Err(e.clone());
            }
            st = match self.wait(st, deadline) {
                Ok(s) => s,
                Err(e) => {
                    self.lock().pings.remove(&data);
                    return Err(e);
                }
            };
        }
    }

    /// Sends GOAWAY(NO_ERROR) — new streams are refused, existing ones continue.
    pub(crate) fn goaway(&self) {
        let last = {
            let mut st = self.lock();
            if st.goaway_sent {
                return;
            }
            st.goaway_sent = true;
            st.last_peer_stream
        };
        self.send_frame(Frame::goaway(last, ErrorCode::NO_ERROR, b""));
    }

    /// Waits until no stream is active (or the timeout elapses / the connection closes).
    pub(crate) fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = Some(Instant::now() + timeout);
        let mut st = self.lock();
        loop {
            if st.active_count() == 0 || st.closed.is_some() {
                return true;
            }
            st = match self.wait(st, deadline) {
                Ok(s) => s,
                Err(_) => return false,
            };
        }
    }

    /// Sends GOAWAY and closes the connection.
    pub(crate) fn close(&self) {
        self.goaway();
        let _ = self.lock_out().tx.send(Out::Close);
        self.fail(H2Error::Closed("connection closed locally".into()));
    }

    pub(crate) fn is_closed(&self) -> bool {
        let st = self.lock();
        st.closed.is_some() || st.goaway_received.is_some()
    }

    pub(crate) fn active_streams(&self) -> u32 {
        self.lock().active_count()
    }

    pub(crate) fn peer_max_concurrent(&self) -> u32 {
        self.lock().peer.max_concurrent
    }
}

fn read_full(r: &mut Box<dyn Read + Send>, buf: &mut [u8]) -> std::io::Result<()> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(&mut buf[got..]) {
            Ok(0) => return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "connection closed")),
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// A handle to one HTTP/2 stream (request/response exchange).
///
/// Dropping the handle before both directions are closed resets the stream
/// (`CANCEL`, or `NO_ERROR` when our side already finished).
pub struct H2Stream {
    shared: Arc<Shared>,
    id: u32,
}

impl fmt::Debug for H2Stream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "H2Stream({})", self.id)
    }
}

impl H2Stream {
    /// The stream identifier.
    pub fn id(&self) -> u32 {
        self.id
    }

    /// Remote socket address of the connection.
    pub fn peer_addr(&self) -> Option<SocketAddr> {
        self.shared.peer_addr
    }

    /// Whether the connection runs over TLS.
    pub fn is_tls(&self) -> bool {
        self.shared.tls
    }

    /// Local socket address of the connection.
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.shared.local_addr
    }

    fn check(&self, st: &ConnState) -> Result<(), H2Error> {
        if let Some(s) = st.streams.get(&self.id) {
            if let Some(c) = s.reset {
                return Err(H2Error::StreamReset(c));
            }
        } else {
            return Err(H2Error::StreamReset(ErrorCode::STREAM_CLOSED));
        }
        if let Some(e) = &st.closed {
            return Err(e.clone());
        }
        Ok(())
    }

    /// Sends a header block (initial headers, or trailers with `end_stream`).
    pub fn send_headers(&self, headers: &[(String, String)], end_stream: bool) -> Result<(), H2Error> {
        let sh = &self.shared;
        let mut out = sh.lock_out();
        {
            let mut st = sh.lock();
            self.check(&st)?;
            let s = st.streams.get_mut(&self.id).expect("checked");
            if s.local_end {
                return Err(H2Error::StreamReset(ErrorCode::STREAM_CLOSED));
            }
            // mark before the frame can reach the peer, so our view of active streams
            // never lags behind the peer's (SETTINGS_MAX_CONCURRENT_STREAMS accounting)
            if end_stream {
                s.local_end = true;
            }
        }
        let block = out.encoder.encode(headers);
        let mut bytes = Vec::new();
        for f in Frame::headers(self.id, &block, end_stream, out.max_frame_size) {
            f.encode(&mut bytes);
        }
        out.tx.send(Out::Bytes(bytes)).map_err(|_| H2Error::Closed("writer stopped".into()))?;
        drop(out);
        if end_stream {
            sh.cond.notify_all();
        }
        Ok(())
    }

    /// Sends DATA, splitting by the peer's frame size and blocking for flow-control credit.
    pub fn send_data(&self, data: &[u8], end_stream: bool) -> Result<(), H2Error> {
        self.send_data_timeout(data, end_stream, None)
    }

    /// Like [`send_data`](Self::send_data) with a limit on the time spent waiting for credit.
    pub fn send_data_timeout(&self, data: &[u8], end_stream: bool, timeout: Option<Duration>) -> Result<(), H2Error> {
        let sh = &self.shared;
        let deadline = timeout.map(|t| Instant::now() + t);
        if data.is_empty() {
            {
                let mut st = sh.lock();
                self.check(&st)?;
                let s = st.streams.get_mut(&self.id).expect("checked");
                if s.local_end {
                    return Err(H2Error::StreamReset(ErrorCode::STREAM_CLOSED));
                }
                if end_stream {
                    s.local_end = true;
                }
            }
            sh.queue(Frame::data(self.id, &[], end_stream).to_bytes())?;
            if end_stream {
                sh.cond.notify_all();
            }
            return Ok(());
        }
        let mut off = 0;
        while off < data.len() {
            let n = {
                let mut st = sh.lock();
                loop {
                    self.check(&st)?;
                    let s = st.streams.get(&self.id).expect("checked");
                    if s.local_end {
                        return Err(H2Error::StreamReset(ErrorCode::STREAM_CLOSED));
                    }
                    let avail = st.send_window.min(s.send_window).min(st.peer.max_frame_size as i64).min((data.len() - off) as i64);
                    if avail > 0 {
                        st.send_window -= avail;
                        let s = st.streams.get_mut(&self.id).expect("checked");
                        s.send_window -= avail;
                        if end_stream && off + avail as usize == data.len() {
                            s.local_end = true;
                        }
                        break avail as usize;
                    }
                    st = sh.wait(st, deadline)?;
                }
            };
            let last = off + n == data.len();
            sh.queue(Frame::data(self.id, &data[off..off + n], end_stream && last).to_bytes())?;
            off += n;
        }
        if end_stream {
            sh.cond.notify_all();
        }
        Ok(())
    }

    /// Sends trailers (a final header block with END_STREAM).
    pub fn send_trailers(&self, trailers: &[(String, String)]) -> Result<(), H2Error> {
        self.send_headers(trailers, true)
    }

    /// Waits for the next received header block (initial headers, then trailers).
    /// `Ok(None)` when the peer ended the stream without (more) headers.
    pub fn recv_headers(&self, timeout: Option<Duration>) -> Result<Option<HeaderList>, H2Error> {
        let sh = &self.shared;
        let deadline = timeout.map(|t| Instant::now() + t);
        let mut st = sh.lock();
        loop {
            let s = st.streams.get_mut(&self.id).ok_or(H2Error::StreamReset(ErrorCode::STREAM_CLOSED))?;
            if let Some(h) = s.headers.pop_front() {
                return Ok(Some(h));
            }
            if let Some(c) = s.reset {
                return Err(H2Error::StreamReset(c));
            }
            if s.remote_end {
                return Ok(None);
            }
            if let Some(e) = &st.closed {
                return Err(e.clone());
            }
            st = sh.wait(st, deadline)?;
        }
    }

    /// Waits for the next chunk of DATA. `Ok(None)` at the end of the stream.
    pub fn recv_data(&self, timeout: Option<Duration>) -> Result<Option<Vec<u8>>, H2Error> {
        let sh = &self.shared;
        let deadline = timeout.map(|t| Instant::now() + t);
        let mut st = sh.lock();
        loop {
            let s = st.streams.get_mut(&self.id).ok_or(H2Error::StreamReset(ErrorCode::STREAM_CLOSED))?;
            if let Some(c) = s.reset {
                return Err(H2Error::StreamReset(c));
            }
            if let Some(chunk) = s.data.pop_front() {
                let n = chunk.len() as u32;
                s.recv_unacked += n;
                let mut stream_inc = None;
                let target = sh.config.initial_window_size.max(1);
                if !s.remote_end && s.recv_unacked >= target / 2 {
                    let inc = s.recv_unacked;
                    s.recv_unacked = 0;
                    s.recv_window += inc as i64;
                    stream_inc = Some(inc);
                }
                let conn_inc = sh.release_conn_locked(&mut st, n);
                drop(st);
                if let Some(i) = stream_inc {
                    sh.send_frame(Frame::window_update(self.id, i));
                }
                if let Some(i) = conn_inc {
                    sh.send_frame(Frame::window_update(0, i));
                }
                return Ok(Some(chunk));
            }
            if s.remote_end {
                return Ok(None);
            }
            if let Some(e) = &st.closed {
                return Err(e.clone());
            }
            st = sh.wait(st, deadline)?;
        }
    }

    /// Reads all remaining DATA.
    pub fn read_body(&self, timeout: Option<Duration>) -> Result<Vec<u8>, H2Error> {
        let deadline = timeout.map(|t| Instant::now() + t);
        let mut body = Vec::new();
        loop {
            let left = match deadline {
                Some(d) => Some(d.checked_duration_since(Instant::now()).ok_or(H2Error::Timeout)?),
                None => None,
            };
            match self.recv_data(left)? {
                Some(c) => body.extend_from_slice(&c),
                None => return Ok(body),
            }
        }
    }

    /// Resets the stream with `code`.
    pub fn reset(&self, code: ErrorCode) {
        let mut st = self.shared.lock();
        let send = match st.streams.get_mut(&self.id) {
            Some(s) if s.reset.is_none() => {
                s.reset = Some(code);
                true
            }
            _ => false,
        };
        drop(st);
        if send {
            self.shared.send_frame(Frame::rst_stream(self.id, code));
        }
        self.shared.cond.notify_all();
    }

    /// Whether the peer finished sending (END_STREAM received).
    pub fn is_remote_closed(&self) -> bool {
        self.shared.lock().streams.get(&self.id).map(|s| s.remote_end || s.reset.is_some()).unwrap_or(true)
    }

    /// The reset code, if the stream was reset by either side.
    pub fn reset_code(&self) -> Option<ErrorCode> {
        self.shared.lock().streams.get(&self.id).and_then(|s| s.reset)
    }
}

impl Drop for H2Stream {
    fn drop(&mut self) {
        let sh = &self.shared;
        let mut st = sh.lock();
        let Some(s) = st.streams.remove(&self.id) else {
            return;
        };
        let unread: u32 = s.data.iter().map(|d| d.len() as u32).sum();
        let conn_inc = sh.release_conn_locked(&mut st, unread);
        let closed = st.closed.is_some();
        drop(st);
        if !closed && s.reset.is_none() && !(s.local_end && s.remote_end) {
            let code = if s.local_end { ErrorCode::NO_ERROR } else { ErrorCode::CANCEL };
            sh.send_frame(Frame::rst_stream(self.id, code));
        }
        if let Some(i) = conn_inc {
            sh.send_frame(Frame::window_update(0, i));
        }
        sh.cond.notify_all();
    }
}

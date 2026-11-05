//! A blocking STOMP 1.2 client on `std::net`.
//!
//! `CONNECT` with `login`/`passcode`, virtual `host` and heart-beat negotiation, `SEND`,
//! `SUBSCRIBE` (`auto` / `client` / `client-individual` acks, extra headers such as
//! `selector` or `activemq.prefetchSize`), `UNSUBSCRIBE`, `ACK` / `NACK`, transactions
//! (`BEGIN` / `COMMIT` / `ABORT`), receipts (synchronous operations) and `DISCONNECT`.
//! A reader thread routes `MESSAGE` frames to per-subscription channels; an `ERROR`
//! frame closes the connection with that error.

use std::collections::HashMap;
use std::io::{BufReader, Write};
use std::net::Shutdown;

use cameleer_tls::camel_support::EndpointTls;
use cameleer_tls::net::NetStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use cameleer_core::component::support::StopFlag;
use cameleer_core::error::{kinds, CamelError, Result};

use super::frame::{read_frame, Frame};
use crate::common::{connect, io_error, unique_id};

/// Exception kind of a STOMP `ERROR` frame.
pub const STOMP_ERROR: &str = "StompException";
/// Exception kind of a refused login.
pub const AUTHENTICATION_FAILURE: &str = "AuthenticationFailureException";
/// Largest accepted frame body.
pub const MAX_BODY: usize = 64 * 1024 * 1024;

/// Connection settings.
#[derive(Clone, Debug)]
pub struct StompConfig {
    pub host: String,
    pub port: u16,
    pub login: Option<String>,
    pub passcode: Option<String>,
    /// The `host` header (virtual host); defaults to the broker host name.
    pub virtual_host: Option<String>,
    /// Heart-beats this client can send (ms, 0 = none).
    pub heartbeat_send: u64,
    /// Heart-beats this client wants to receive (ms, 0 = none).
    pub heartbeat_receive: u64,
    pub connect_timeout: Duration,
    /// How long receipts are waited for.
    pub receipt_timeout: Duration,
    /// Optional `client-id` header (ActiveMQ).
    pub client_id: Option<String>,
    /// TLS (`ssl://`, `stomp+ssl://` broker urls).
    pub tls: Option<EndpointTls>,
}

impl Default for StompConfig {
    fn default() -> Self {
        StompConfig {
            host: "localhost".into(),
            port: 61613,
            login: None,
            passcode: None,
            virtual_host: None,
            heartbeat_send: 0,
            heartbeat_receive: 0,
            connect_timeout: Duration::from_secs(30),
            receipt_timeout: Duration::from_secs(10),
            client_id: None,
            tls: None,
        }
    }
}

/// Subscription acknowledgement mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AckMode {
    Auto,
    Client,
    ClientIndividual,
}

impl AckMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            AckMode::Auto => "auto",
            AckMode::Client => "client",
            AckMode::ClientIndividual => "client-individual",
        }
    }
    pub fn parse(s: &str) -> Option<AckMode> {
        match s {
            "auto" => Some(AckMode::Auto),
            "client" => Some(AckMode::Client),
            "client-individual" => Some(AckMode::ClientIndividual),
            _ => None,
        }
    }
}

struct Inner {
    writer: Mutex<NetStream>,
    subs: Mutex<HashMap<String, Sender<Frame>>>,
    receipts: Mutex<HashMap<String, Sender<Frame>>>,
    closed: Mutex<Option<CamelError>>,
    open: AtomicBool,
    stop: Arc<StopFlag>,
    threads: Mutex<Vec<JoinHandle<()>>>,
    ids: AtomicU64,
    receipt_timeout: Duration,
}

impl Inner {
    fn write(&self, f: &Frame) -> Result<()> {
        if !self.open.load(Ordering::SeqCst) {
            return Err(self.closed_error());
        }
        let buf = f.encode();
        let mut w = self.writer.lock().unwrap();
        w.write_all(&buf).and_then(|_| w.flush()).map_err(|e| io_error(e, "STOMP connection"))
    }
    fn closed_error(&self) -> CamelError {
        self.closed.lock().unwrap().clone().unwrap_or_else(|| CamelError::io("STOMP connection is closed"))
    }
    fn mark_closed(&self, e: CamelError) {
        {
            let mut c = self.closed.lock().unwrap();
            if c.is_none() {
                *c = Some(e);
            }
        }
        self.open.store(false, Ordering::SeqCst);
        self.subs.lock().unwrap().clear();
        self.receipts.lock().unwrap().clear();
        self.stop.stop();
    }
}

fn error_frame_to_error(f: &Frame) -> CamelError {
    let msg = f.get("message").unwrap_or("").to_string();
    let body = f.body_string();
    // ActiveMQ repeats the message in the body
    let text = if body.trim().is_empty() || body.trim() == msg.trim() { msg.clone() } else { format!("{}: {}", msg, body.trim()) };
    // Artemis puts the reason in `message`; ActiveMQ 5 says "User name [x] or password is
    // invalid."; RabbitMQ sends `message:Bad CONNECT` with "Access refused for user 'x'" in
    // the body
    let lower = text.to_ascii_lowercase();
    let auth = ["auth", "login", "access refused", "password is invalid", "user name ["].iter().any(|k| lower.contains(k));
    let kind = if auth { AUTHENTICATION_FAILURE } else { STOMP_ERROR };
    CamelError::new(kind, text)
}

/// A subscription: its id and the channel of `MESSAGE` frames.
pub struct Subscription {
    pub id: String,
    pub messages: Receiver<Frame>,
}

/// An open STOMP connection. Dropping it disconnects.
pub struct StompConnection {
    inner: Arc<Inner>,
    session: String,
    server: String,
    version: String,
}

impl StompConnection {
    /// Connect (`CONNECT` / `CONNECTED`).
    pub fn open(cfg: &StompConfig) -> Result<StompConnection> {
        let tcp = connect(&cfg.host, cfg.port, cfg.connect_timeout)?;
        let what = format!("{}://{}:{}", if cfg.tls.is_some() { "stomp+ssl" } else { "stomp" }, cfg.host, cfg.port);
        let stream = match &cfg.tls {
            Some(t) => {
                let _ = tcp.set_read_timeout(Some(cfg.connect_timeout.max(Duration::from_millis(100))));
                t.handshake_client(tcp, &cfg.host)?
            }
            None => NetStream::plain(tcp),
        };
        let ioe = |e: std::io::Error| io_error(e, &what);
        stream.set_read_timeout(Some(cfg.connect_timeout.max(Duration::from_millis(100)))).map_err(ioe)?;
        let mut reader = BufReader::new(stream.try_clone().map_err(ioe)?);
        let mut connect_frame = Frame::new("CONNECT")
            .header("accept-version", "1.2")
            .header("host", cfg.virtual_host.clone().unwrap_or_else(|| cfg.host.clone()))
            .header("heart-beat", format!("{},{}", cfg.heartbeat_send, cfg.heartbeat_receive));
        if let Some(l) = &cfg.login {
            connect_frame = connect_frame.header("login", l.clone());
        }
        if let Some(p) = &cfg.passcode {
            connect_frame = connect_frame.header("passcode", p.clone());
        }
        if let Some(c) = &cfg.client_id {
            connect_frame = connect_frame.header("client-id", c.clone());
        }
        (&stream).write_all(&connect_frame.encode()).map_err(ioe)?;
        let reply = read_frame(&mut reader, MAX_BODY).map_err(ioe)?.ok_or_else(|| CamelError::new(kinds::CONNECT, format!("Connection closed by STOMP broker {}", what)))?;
        if reply.command == "ERROR" {
            return Err(error_frame_to_error(&reply));
        }
        if reply.command != "CONNECTED" {
            return Err(CamelError::io(format!("Expected CONNECTED, got {}", reply.command)));
        }
        let (sx, sy) = reply
            .get("heart-beat")
            .and_then(|h| h.split_once(','))
            .map(|(a, b)| (a.trim().parse::<u64>().unwrap_or(0), b.trim().parse::<u64>().unwrap_or(0)))
            .unwrap_or((0, 0));
        let send_every = if cfg.heartbeat_send > 0 && sy > 0 { cfg.heartbeat_send.max(sy) } else { 0 };
        let expect_every = if cfg.heartbeat_receive > 0 && sx > 0 { cfg.heartbeat_receive.max(sx) } else { 0 };
        stream.set_read_timeout(if expect_every > 0 { Some(Duration::from_millis(expect_every * 2 + 500)) } else { None }).map_err(ioe)?;
        let inner = Arc::new(Inner {
            writer: Mutex::new(stream.try_clone().map_err(ioe)?),
            subs: Mutex::new(HashMap::new()),
            receipts: Mutex::new(HashMap::new()),
            closed: Mutex::new(None),
            open: AtomicBool::new(true),
            stop: StopFlag::new(),
            threads: Mutex::new(Vec::new()),
            ids: AtomicU64::new(1),
            receipt_timeout: cfg.receipt_timeout,
        });
        let i2 = inner.clone();
        let t = std::thread::Builder::new()
            .name(format!("stomp-reader-{}:{}", cfg.host, cfg.port))
            .spawn(move || {
                let reason = loop {
                    match read_frame(&mut reader, MAX_BODY) {
                        Ok(Some(f)) => match f.command.as_str() {
                            "MESSAGE" => {
                                let id = f.get("subscription").unwrap_or("").to_string();
                                let tx = i2.subs.lock().unwrap().get(&id).cloned();
                                if let Some(tx) = tx {
                                    let _ = tx.send(f);
                                }
                            }
                            "RECEIPT" => {
                                let id = f.get("receipt-id").unwrap_or("").to_string();
                                if let Some(tx) = i2.receipts.lock().unwrap().remove(&id) {
                                    let _ = tx.send(f);
                                }
                            }
                            "ERROR" => {
                                // the broker closes the connection after an ERROR: mark it
                                // closed before waking a receipt waiter
                                let waiter = f.get("receipt-id").and_then(|id| i2.receipts.lock().unwrap().remove(id));
                                let err = error_frame_to_error(&f);
                                i2.mark_closed(err.clone());
                                if let Some(tx) = waiter {
                                    let _ = tx.send(f.clone());
                                }
                                break err;
                            }
                            _ => {}
                        },
                        Ok(None) => break CamelError::io("STOMP connection closed by the broker"),
                        Err(e) => {
                            break match e.kind() {
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => CamelError::io("Missed heart-beats from the STOMP broker; connection is dead"),
                                _ => CamelError::io(format!("STOMP connection lost: {}", e)),
                            }
                        }
                    }
                };
                i2.mark_closed(reason);
            })
            .map_err(|e| CamelError::io(e.to_string()))?;
        inner.threads.lock().unwrap().push(t);
        if send_every > 0 {
            let i3 = inner.clone();
            let t = std::thread::Builder::new()
                .name("stomp-heartbeat".into())
                .spawn(move || {
                    let stop = i3.stop.clone();
                    while !stop.sleep(Duration::from_millis(send_every)) {
                        let mut w = i3.writer.lock().unwrap();
                        if w.write_all(b"\n").and_then(|_| w.flush()).is_err() {
                            break;
                        }
                    }
                })
                .map_err(|e| CamelError::io(e.to_string()))?;
            inner.threads.lock().unwrap().push(t);
        }
        Ok(StompConnection {
            inner,
            session: reply.get("session").unwrap_or("").to_string(),
            server: reply.get("server").unwrap_or("").to_string(),
            version: reply.get("version").unwrap_or("1.0").to_string(),
        })
    }

    pub fn is_open(&self) -> bool {
        self.inner.open.load(Ordering::SeqCst)
    }
    pub fn close_reason(&self) -> Option<CamelError> {
        self.inner.closed.lock().unwrap().clone()
    }
    pub fn session(&self) -> &str {
        &self.session
    }
    pub fn server(&self) -> &str {
        &self.server
    }
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Send a frame; with `receipt` wait for the broker's `RECEIPT`.
    pub fn send_frame(&self, mut f: Frame, receipt: bool) -> Result<()> {
        if !receipt {
            return self.inner.write(&f);
        }
        let id = format!("rcpt-{}", self.inner.ids.fetch_add(1, Ordering::Relaxed));
        f.set("receipt", id.clone());
        let (tx, rx) = mpsc::channel();
        self.inner.receipts.lock().unwrap().insert(id.clone(), tx);
        if let Err(e) = self.inner.write(&f) {
            self.inner.receipts.lock().unwrap().remove(&id);
            return Err(e);
        }
        match rx.recv_timeout(self.inner.receipt_timeout) {
            Ok(r) if r.command == "RECEIPT" => Ok(()),
            Ok(r) => Err(error_frame_to_error(&r)),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(self.inner.closed_error()),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.inner.receipts.lock().unwrap().remove(&id);
                Err(CamelError::new(kinds::TIMEOUT, format!("No RECEIPT for {} within {} ms", f.command, self.inner.receipt_timeout.as_millis())))
            }
        }
    }

    /// `SEND` a message to a destination (extra headers, optional transaction).
    pub fn send(&self, destination: &str, headers: &[(String, String)], body: &[u8], transaction: Option<&str>, receipt: bool) -> Result<()> {
        let mut f = Frame::new("SEND").header("destination", destination);
        for (k, v) in headers {
            if k != "destination" && k != "receipt" && k != "transaction" && k != "content-length" {
                f.headers.push((k.clone(), v.clone()));
            }
        }
        if let Some(tx) = transaction {
            f.set("transaction", tx);
        }
        f.body = body.to_vec();
        self.send_frame(f, receipt)
    }

    /// Whether the broker is RabbitMQ (its STOMP adapter has its own temporary queue and
    /// durable subscription conventions).
    pub fn is_rabbitmq(&self) -> bool {
        self.server.starts_with("RabbitMQ")
    }

    /// Whether the broker is Apache ActiveMQ 5 ("Classic", `server:ActiveMQ/5.x`): an aborted
    /// transactional `ACK` only restores the pending acknowledgement and a `NACK` is a poison
    /// ack (dead letter queue), so redelivery is up to the client, as with its JMS client.
    pub fn is_activemq_classic(&self) -> bool {
        self.server.starts_with("ActiveMQ/")
    }

    /// Receive the replies sent to a `/temp-queue/...` reply destination. RabbitMQ refuses
    /// `SUBSCRIBE` to temporary queues: it creates one when a `SEND` names it in `reply-to`
    /// and delivers the replies with `subscription` set to that destination, so only a local
    /// route is registered there. Other brokers get a normal (auto-ack) subscription.
    pub fn subscribe_reply_queue(&self, destination: &str) -> Result<Subscription> {
        if self.is_rabbitmq() && destination.starts_with("/temp-queue/") {
            let (tx, rx) = mpsc::channel();
            self.inner.subs.lock().unwrap().insert(destination.to_string(), tx);
            return Ok(Subscription { id: destination.to_string(), messages: rx });
        }
        self.subscribe(destination, AckMode::Auto, &[])
    }

    /// `SUBSCRIBE` (synchronously, with a receipt).
    pub fn subscribe(&self, destination: &str, ack: AckMode, extra: &[(String, String)]) -> Result<Subscription> {
        let id = format!("sub-{}-{}", self.inner.ids.fetch_add(1, Ordering::Relaxed), unique_id());
        self.subscribe_with_id(destination, &id, ack, extra)
    }

    /// `SUBSCRIBE` with a given subscription id (stable ids name RabbitMQ durable subscriptions).
    pub fn subscribe_with_id(&self, destination: &str, id: &str, ack: AckMode, extra: &[(String, String)]) -> Result<Subscription> {
        let id = id.to_string();
        let (tx, rx) = mpsc::channel();
        self.inner.subs.lock().unwrap().insert(id.clone(), tx);
        let mut f = Frame::new("SUBSCRIBE").header("destination", destination).header("id", id.clone()).header("ack", ack.as_str());
        for (k, v) in extra {
            f.headers.push((k.clone(), v.clone()));
        }
        if let Err(e) = self.send_frame(f, true) {
            self.inner.subs.lock().unwrap().remove(&id);
            return Err(e);
        }
        Ok(Subscription { id, messages: rx })
    }

    /// `UNSUBSCRIBE`.
    pub fn unsubscribe(&self, id: &str) -> Result<()> {
        if self.is_rabbitmq() && id.starts_with("/temp-queue/") {
            // a local route registered by `subscribe_reply_queue`
            self.inner.subs.lock().unwrap().remove(id);
            return Ok(());
        }
        let r = self.send_frame(Frame::new("UNSUBSCRIBE").header("id", id), true);
        self.inner.subs.lock().unwrap().remove(id);
        r
    }

    /// `ACK` a message (by its `ack` header).
    pub fn ack(&self, ack_id: &str, transaction: Option<&str>) -> Result<()> {
        let mut f = Frame::new("ACK").header("id", ack_id);
        if let Some(t) = transaction {
            f.set("transaction", t);
        }
        self.inner.write(&f)
    }

    /// `NACK` a message.
    pub fn nack(&self, ack_id: &str, transaction: Option<&str>) -> Result<()> {
        let mut f = Frame::new("NACK").header("id", ack_id);
        if let Some(t) = transaction {
            f.set("transaction", t);
        }
        self.inner.write(&f)
    }

    /// `BEGIN` a transaction.
    pub fn begin(&self, tx: &str) -> Result<()> {
        self.inner.write(&Frame::new("BEGIN").header("transaction", tx))
    }
    /// `COMMIT` a transaction (waits for the receipt).
    pub fn commit(&self, tx: &str) -> Result<()> {
        self.send_frame(Frame::new("COMMIT").header("transaction", tx), true)
    }
    /// `ABORT` a transaction (waits for the receipt).
    pub fn abort(&self, tx: &str) -> Result<()> {
        self.send_frame(Frame::new("ABORT").header("transaction", tx), true)
    }

    /// `DISCONNECT` gracefully and close the socket.
    pub fn disconnect(&self) {
        if self.is_open() {
            let _ = self.send_frame(Frame::new("DISCONNECT"), true);
        }
        self.inner.mark_closed(CamelError::io("STOMP connection closed by application"));
        let _ = self.inner.writer.lock().unwrap().shutdown(Shutdown::Both);
        let me = std::thread::current().id();
        let threads: Vec<JoinHandle<()>> = std::mem::take(&mut *self.inner.threads.lock().unwrap());
        for t in threads {
            if t.thread().id() != me {
                let _ = t.join();
            }
        }
    }
}

impl Drop for StompConnection {
    fn drop(&mut self) {
        self.disconnect();
    }
}

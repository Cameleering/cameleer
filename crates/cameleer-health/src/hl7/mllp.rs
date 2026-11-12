//! `mllp:` component (Camel `camel-mllp`): HL7 v2 over TCP with MLLP framing
//! (`<VT> message <FS><CR>`).
//!
//! `mllp://host:port[?options]`
//!
//! - **Consumer**: a TCP server. Each framed message is routed (body `String`, or bytes
//!   with `stringPayload=false`) with the `CamelMllp*` headers taken from `MSH`. With
//!   `autoAck=true` (default) an HL7 acknowledgement is generated and returned:
//!   `MSA|AA|<MSH-10>` on success, `AE` when the exchange failed. A route can provide its
//!   own acknowledgement with header `CamelMllpAcknowledgement` (bytes) /
//!   `CamelMllpAcknowledgementString`, or force the code with
//!   `CamelMllpAcknowledgementType`.
//! - **Producer**: sends the body framed and waits for the acknowledgement
//!   (`receiveTimeout`, 15s). Sets `CamelMllpAcknowledgement`,
//!   `CamelMllpAcknowledgementString` and `CamelMllpAcknowledgementType`; an `AE`/`AR`
//!   (`CE`/`CR`) acknowledgement fails the exchange with
//!   `MllpApplicationErrorAcknowledgementException` /
//!   `MllpApplicationRejectAcknowledgementException` (`...CommitError...`/`...CommitReject...`),
//!   no acknowledgement with `MllpAcknowledgementTimeoutException`. The connection is kept
//!   open between exchanges.
//!
//! Options: `autoAck` (true), `hl7Headers` (true), `stringPayload` (true),
//! `validatePayload` (false), `charsetName` (UTF-8, or ISO-8859-1), `receiveTimeout`
//! (15s), `readTimeout` (5s, idle wait for the rest of a frame), `connectTimeout` (30s),
//! `idleTimeout` (consumer: close idle connections, 0 = never), `keepAlive`,
//! `tcpNoDelay` (true), plus accepted-and-ignored tuning options.
//!
//! TLS: `sslContextParameters=#bean` (or `useGlobalSslContextParameters=true`) makes the
//! consumer accept TLS connections (the bean needs key material) and the producer connect
//! with TLS (verifying the server certificate against the bean's trust store).

use std::any::Any;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cameleer_core::context::{CamelContext, WeakContext};
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::{Exchange, ExchangePattern};
use cameleer_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use cameleer_core::util::Parameters;
use cameleer_core::value::Value;
use cameleer_tls::camel_support::{endpoint_tls, EndpointTls};
use cameleer_tls::net::NetStream;

use cameleer_tls::net::{connect_stream, io_error, split_host_port, take_ignored, TcpServer, COMMON_IGNORED};
use super::Hl7Message;

/// Start of block (VT).
pub const START_OF_BLOCK: u8 = 0x0b;
/// End of block (FS).
pub const END_OF_BLOCK: u8 = 0x1c;
/// End of data (CR).
pub const END_OF_DATA: u8 = 0x0d;

/// MLLP header names (Camel `MllpConstants`).
pub mod headers {
    pub const LOCAL_ADDRESS: &str = "CamelMllpLocalAddress";
    pub const REMOTE_ADDRESS: &str = "CamelMllpRemoteAddress";
    pub const ACKNOWLEDGEMENT: &str = "CamelMllpAcknowledgement";
    pub const ACKNOWLEDGEMENT_STRING: &str = "CamelMllpAcknowledgementString";
    pub const ACKNOWLEDGEMENT_TYPE: &str = "CamelMllpAcknowledgementType";
    pub const ACKNOWLEDGEMENT_EXCEPTION: &str = "CamelMllpAcknowledgementException";
    pub const AUTO_ACKNOWLEDGE: &str = "CamelMllpAutoAcknowledge";
    pub const CHARSET: &str = "CamelMllpCharset";
    pub const SENDING_APPLICATION: &str = "CamelMllpSendingApplication";
    pub const SENDING_FACILITY: &str = "CamelMllpSendingFacility";
    pub const RECEIVING_APPLICATION: &str = "CamelMllpReceivingApplication";
    pub const RECEIVING_FACILITY: &str = "CamelMllpReceivingFacility";
    pub const TIMESTAMP: &str = "CamelMllpTimestamp";
    pub const SECURITY: &str = "CamelMllpSecurity";
    pub const MESSAGE_TYPE: &str = "CamelMllpMessageType";
    pub const EVENT_TYPE: &str = "CamelMllpEventType";
    pub const TRIGGER_EVENT: &str = "CamelMllpTriggerEvent";
    pub const MESSAGE_CONTROL_ID: &str = "CamelMllpMessageControlId";
    pub const PROCESSING_ID: &str = "CamelMllpProcessingId";
    pub const VERSION_ID: &str = "CamelMllpVersionId";
}

/// Exception kinds.
pub mod errors {
    pub const MLLP_EXCEPTION: &str = "MllpException";
    pub const APPLICATION_ERROR: &str = "MllpApplicationErrorAcknowledgementException";
    pub const APPLICATION_REJECT: &str = "MllpApplicationRejectAcknowledgementException";
    pub const COMMIT_ERROR: &str = "MllpCommitErrorAcknowledgementException";
    pub const COMMIT_REJECT: &str = "MllpCommitRejectAcknowledgementException";
    pub const ACK_TIMEOUT: &str = "MllpAcknowledgementTimeoutException";
    pub const INVALID_ACK: &str = "MllpInvalidAcknowledgementException";
    pub const INVALID_MESSAGE: &str = "MllpInvalidMessageException";
}

/// Frame a payload: `<VT>payload<FS><CR>`.
pub fn frame(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 3);
    out.push(START_OF_BLOCK);
    out.extend_from_slice(payload);
    out.push(END_OF_BLOCK);
    out.push(END_OF_DATA);
    out
}

/// Read one MLLP frame; bytes before the start of block are discarded. `Ok(None)` on EOF.
pub fn read_frame<R: BufRead>(r: &mut R, max: usize) -> std::io::Result<Option<Vec<u8>>> {
    // skip out-of-band data up to the start of block
    let mut junk = Vec::new();
    if r.read_until(START_OF_BLOCK, &mut junk)? == 0 || junk.last() != Some(&START_OF_BLOCK) {
        return Ok(None);
    }
    let mut buf = Vec::new();
    let n = Read::take(&mut *r, (max + 2) as u64).read_until(END_OF_BLOCK, &mut buf)?;
    if n == 0 || buf.last() != Some(&END_OF_BLOCK) {
        if buf.len() > max {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "MLLP frame too large"));
        }
        return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "connection closed inside an MLLP frame"));
    }
    buf.pop();
    // expect the trailing CR (tolerate its absence)
    let next = r.fill_buf()?;
    if next.first() == Some(&END_OF_DATA) {
        r.consume(1);
    }
    Ok(Some(buf))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Charset {
    Utf8,
    Latin1,
}

impl Charset {
    fn decode(&self, b: &[u8]) -> String {
        match self {
            Charset::Latin1 => b.iter().map(|&c| c as char).collect(),
            Charset::Utf8 => String::from_utf8_lossy(b).into_owned(),
        }
    }
    fn encode(&self, s: &str) -> Vec<u8> {
        match self {
            Charset::Latin1 => s.chars().map(|c| if (c as u32) < 256 { c as u8 } else { b'?' }).collect(),
            Charset::Utf8 => s.as_bytes().to_vec(),
        }
    }
}

/// Endpoint configuration.
#[derive(Clone, Debug)]
pub struct MllpConfig {
    pub host: String,
    pub port: u16,
    pub auto_ack: bool,
    pub hl7_headers: bool,
    pub string_payload: bool,
    pub validate_payload: bool,
    charset: Charset,
    pub receive_timeout: Duration,
    pub read_timeout: Duration,
    pub connect_timeout: Duration,
    pub idle_timeout: Duration,
    pub keep_alive: bool,
    pub tcp_no_delay: bool,
    pub max_frame_size: usize,
    /// TLS settings (`sslContextParameters`).
    pub tls: Option<EndpointTls>,
}

/// The `mllp` component.
#[derive(Default)]
pub struct MllpComponent;

impl Component for MllpComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let (host, port) = split_host_port(remaining.trim_end_matches('/'), 0)?;
        let charset = match params.take("charsetName").map(|s| s.to_ascii_uppercase().replace('_', "-")) {
            None => Charset::Utf8,
            Some(c) if c == "UTF-8" || c == "UTF8" => Charset::Utf8,
            Some(c) if c == "ISO-8859-1" || c == "LATIN1" || c == "US-ASCII" => Charset::Latin1,
            Some(c) => return Err(CamelError::illegal_argument(format!("Unsupported charsetName: {}", c))),
        };
        let tls = endpoint_tls(ctx, params, "mllp", false, &[])?;
        let cfg = MllpConfig {
            tls,
            host,
            port,
            auto_ack: params.take_bool("autoAck", true)?,
            hl7_headers: params.take_bool("hl7Headers", true)?,
            string_payload: params.take_bool("stringPayload", true)?,
            validate_payload: params.take_bool("validatePayload", false)?,
            charset,
            receive_timeout: params.take_duration("receiveTimeout", Duration::from_secs(15))?,
            read_timeout: params.take_duration("readTimeout", Duration::from_secs(5))?,
            connect_timeout: params.take_duration("connectTimeout", Duration::from_secs(30))?,
            idle_timeout: params.take_duration("idleTimeout", Duration::ZERO)?,
            keep_alive: params.take_bool("keepAlive", true)?,
            tcp_no_delay: params.take_bool("tcpNoDelay", true)?,
            max_frame_size: params.take_usize("maxBufferSize", 1024 * 1024 * 1024)?,
        };
        take_ignored(params, COMMON_IGNORED);
        take_ignored(
            params,
            &[
                "requireEndOfData",
                "bindTimeout",
                "bindRetryInterval",
                "lenientBind",
                "maxConcurrentConsumers",
                "acceptTimeout",
                "backlog",
                "bufferWrites",
                "minBufferSize",
                "receiveBufferSize",
                "sendBufferSize",
                "reuseAddress",
                "idleTimeoutStrategy",
                "connectionStrategy",
                "keepAlive",
                "convertLFtoCR",
                "hl7AutoAcknowledgement",
            ],
        );
        Ok(Arc::new(MllpEndpoint { uri: uri.to_string(), ctx: ctx.downgrade(), cfg, port: Arc::new(Mutex::new(None)) }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// An `mllp` endpoint.
pub struct MllpEndpoint {
    uri: String,
    ctx: WeakContext,
    cfg: MllpConfig,
    port: Arc<Mutex<Option<u16>>>,
}

impl MllpEndpoint {
    /// The bound port while a consumer is running.
    pub fn local_port(&self) -> Option<u16> {
        *self.port.lock().unwrap()
    }
    pub fn config(&self) -> &MllpConfig {
        &self.cfg
    }
}

impl Endpoint for MllpEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(MllpProducer { cfg: self.cfg.clone(), conn: Mutex::new(None) }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        Ok(Arc::new(MllpConsumer {
            cfg: self.cfg.clone(),
            ctx: self.ctx.clone(),
            processor,
            logger: self.uri.clone(),
            port: self.port.clone(),
            server: Mutex::new(None),
        }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn set_msh_headers(ex: &mut Exchange, m: &Hl7Message) {
    use headers::*;
    let pairs = [
        (SENDING_APPLICATION, "MSH-3"),
        (SENDING_FACILITY, "MSH-4"),
        (RECEIVING_APPLICATION, "MSH-5"),
        (RECEIVING_FACILITY, "MSH-6"),
        (TIMESTAMP, "MSH-7"),
        (SECURITY, "MSH-8"),
        (EVENT_TYPE, "MSH-9-1"),
        (TRIGGER_EVENT, "MSH-9-2"),
        (MESSAGE_CONTROL_ID, "MSH-10"),
        (PROCESSING_ID, "MSH-11"),
        (VERSION_ID, "MSH-12"),
    ];
    for (h, p) in pairs {
        ex.set_header(h, m.get(p).unwrap_or_default());
    }
    ex.set_header(MESSAGE_TYPE, m.message_type());
}

/// Find the `MSA-1` acknowledgement code of an ACK message.
pub fn acknowledgement_code(ack: &str) -> Option<String> {
    Hl7Message::parse(ack).ok().and_then(|m| m.get("MSA-1")).filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------------------
// consumer
// ---------------------------------------------------------------------------

struct MllpConsumer {
    cfg: MllpConfig,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
    logger: String,
    port: Arc<Mutex<Option<u16>>>,
    server: Mutex<Option<TcpServer>>,
}

struct Handler {
    cfg: MllpConfig,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
    logger: String,
}

impl Handler {
    fn serve(&self, tcp: TcpStream) {
        let _ = tcp.set_nodelay(self.cfg.tcp_no_delay);
        let stream = match &self.cfg.tls {
            Some(t) => match t.accept(tcp, "mllp consumer") {
                Ok(s) => s,
                Err(e) => {
                    cameleer_core::camel_debug!(&self.logger, "MLLP TLS handshake failed: {}", e);
                    return;
                }
            },
            None => NetStream::plain(tcp),
        };
        if !self.cfg.idle_timeout.is_zero() {
            let _ = stream.set_read_timeout(Some(self.cfg.idle_timeout));
        }
        let remote = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
        let local = stream.local_addr().map(|a| a.to_string()).unwrap_or_default();
        let Ok(rs) = stream.try_clone() else { return };
        let mut reader = BufReader::new(rs);
        let mut w = &stream;
        loop {
            let data = match read_frame(&mut reader, self.cfg.max_frame_size) {
                Ok(Some(d)) => d,
                Ok(None) => break,
                Err(e) => {
                    cameleer_core::camel_debug!(&self.logger, "MLLP connection {} closed: {}", remote, e);
                    break;
                }
            };
            let reply = self.process(&data, &remote, &local);
            if let Some(ack) = reply {
                if w.write_all(&frame(&ack)).and_then(|_| w.flush()).is_err() {
                    break;
                }
            }
        }
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }

    fn process(&self, data: &[u8], remote: &str, local: &str) -> Option<Vec<u8>> {
        let text = self.cfg.charset.decode(data);
        let mut ex = Exchange::new(self.ctx.upgrade());
        ex.pattern = ExchangePattern::InOut;
        ex.set_body(if self.cfg.string_payload { Value::String(text.clone()) } else { Value::Bytes(data.to_vec()) });
        ex.set_header(headers::REMOTE_ADDRESS, remote.to_string());
        ex.set_header(headers::LOCAL_ADDRESS, local.to_string());
        ex.set_header(headers::AUTO_ACKNOWLEDGE, self.cfg.auto_ack);
        let parsed = Hl7Message::parse(&text);
        match &parsed {
            Ok(m) if self.cfg.hl7_headers => set_msh_headers(&mut ex, m),
            Err(e) if self.cfg.validate_payload => ex.set_exception(CamelError::new(errors::INVALID_MESSAGE, e.message.clone())),
            _ => {}
        }
        if ex.exception.is_none() {
            if let Err(e) = self.processor.process(&mut ex) {
                ex.set_exception(e);
            }
        }
        if let Some(e) = &ex.exception {
            cameleer_core::camel_warn!(&self.logger, "Error processing MLLP exchange {}: {} - {}", ex.exchange_id, e.kind, e.message);
        }
        // an explicit acknowledgement from the route wins
        if let Some(v) = ex.header(headers::ACKNOWLEDGEMENT).filter(|v| !v.is_null()) {
            return Some(match v {
                Value::Bytes(b) => b.clone(),
                other => self.cfg.charset.encode(&other.to_string()),
            });
        }
        if let Some(v) = ex.header(headers::ACKNOWLEDGEMENT_STRING).filter(|v| !v.is_null()) {
            return Some(self.cfg.charset.encode(&v.to_string()));
        }
        if !self.cfg.auto_ack {
            return None;
        }
        let forced = ex.header(headers::ACKNOWLEDGEMENT_TYPE).map(|v| v.to_string()).filter(|s| !s.is_empty());
        let (code, text_msg) = match (&forced, &ex.exception) {
            (Some(c), _) => (c.clone(), None),
            (None, Some(e)) => ("AE".to_string(), Some(format!("{}: {}", e.kind, e.message))),
            (None, None) => ("AA".to_string(), None),
        };
        match parsed {
            Ok(m) => Some(self.cfg.charset.encode(&m.acknowledgement(&code, text_msg.as_deref()))),
            Err(_) => {
                // cannot build an HL7 ACK for a non-HL7 payload: answer with a minimal AR
                Some(b"MSH|^~\\&|||||||ACK||P|2.5\rMSA|AR|\r".to_vec())
            }
        }
    }
}

impl Service for MllpConsumer {
    fn start(&self) -> Result<()> {
        let mut g = self.server.lock().unwrap();
        if g.is_some() {
            return Ok(());
        }
        let h = Arc::new(Handler { cfg: self.cfg.clone(), ctx: self.ctx.clone(), processor: self.processor.clone(), logger: self.logger.clone() });
        let s = TcpServer::bind(&self.cfg.host, self.cfg.port, "mllp", move |stream| h.serve(stream))?;
        cameleer_core::camel_info!("mllp", "MLLP consumer listening on {}", s.local_addr());
        *self.port.lock().unwrap() = Some(s.port());
        *g = Some(s);
        Ok(())
    }
    fn stop(&self) -> Result<()> {
        if let Some(s) = self.server.lock().unwrap().take() {
            s.stop();
        }
        *self.port.lock().unwrap() = None;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// producer
// ---------------------------------------------------------------------------

struct Conn {
    stream: NetStream,
    reader: BufReader<NetStream>,
}

struct MllpProducer {
    cfg: MllpConfig,
    conn: Mutex<Option<Conn>>,
}

impl Service for MllpProducer {
    fn stop(&self) -> Result<()> {
        if let Some(c) = self.conn.lock().unwrap().take() {
            let _ = c.stream.shutdown(std::net::Shutdown::Both);
        }
        Ok(())
    }
}

impl MllpProducer {
    fn open(&self) -> Result<Conn> {
        let host = if self.cfg.host == "0.0.0.0" { "127.0.0.1" } else { &self.cfg.host };
        let stream = connect_stream(host, self.cfg.port, self.cfg.connect_timeout, self.cfg.tls.as_ref())?;
        let _ = stream.set_nodelay(self.cfg.tcp_no_delay);
        let reader = BufReader::new(stream.try_clone().map_err(|e| io_error(e, host))?);
        Ok(Conn { stream, reader })
    }

    fn exchange_ack(&self, conn: &mut Conn, payload: &[u8]) -> std::io::Result<Option<Vec<u8>>> {
        (&conn.stream).write_all(&frame(payload))?;
        (&conn.stream).flush()?;
        let t = if self.cfg.receive_timeout.is_zero() { None } else { Some(self.cfg.receive_timeout) };
        conn.stream.set_read_timeout(t)?;
        read_frame(&mut conn.reader, self.cfg.max_frame_size)
    }
}

impl Processor for MllpProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let payload = match ex.body() {
            Value::Bytes(b) => b.clone(),
            Value::Null => return Err(CamelError::new(errors::MLLP_EXCEPTION, "Cannot send a null body over MLLP")),
            other => self.cfg.charset.encode(&other.to_string()),
        };
        let what = format!("mllp://{}:{}", self.cfg.host, self.cfg.port);
        let mut guard = self.conn.lock().unwrap();
        let mut attempt = 0;
        let ack = loop {
            attempt += 1;
            if guard.is_none() {
                *guard = Some(self.open()?);
            }
            let conn = guard.as_mut().unwrap();
            if let (Ok(l), Ok(r)) = (conn.stream.local_addr(), conn.stream.peer_addr()) {
                ex.set_header(headers::LOCAL_ADDRESS, l.to_string());
                ex.set_header(headers::REMOTE_ADDRESS, r.to_string());
            }
            match self.exchange_ack(conn, &payload) {
                Ok(Some(a)) => break a,
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                    *guard = None;
                    return Err(CamelError::new(
                        errors::ACK_TIMEOUT,
                        format!("Timeout waiting for MLLP acknowledgement from {} after {} millis", what, self.cfg.receive_timeout.as_millis()),
                    ));
                }
                // a stale pooled connection: reconnect once
                Ok(None) | Err(_) if attempt == 1 => {
                    *guard = None;
                }
                Ok(None) => {
                    *guard = None;
                    return Err(CamelError::new(errors::ACK_TIMEOUT, format!("Connection closed by {} before an acknowledgement was received", what)));
                }
                Err(e) => {
                    *guard = None;
                    return Err(io_error(e, &what));
                }
            }
        };
        drop(guard);
        let ack_text = self.cfg.charset.decode(&ack);
        ex.set_header(headers::ACKNOWLEDGEMENT, Value::Bytes(ack));
        ex.set_header(headers::ACKNOWLEDGEMENT_STRING, ack_text.clone());
        let code = acknowledgement_code(&ack_text);
        ex.set_header(headers::ACKNOWLEDGEMENT_TYPE, code.clone().unwrap_or_default());
        let kind = match code.as_deref() {
            Some("AA") | Some("CA") => return Ok(()),
            Some("AE") => errors::APPLICATION_ERROR,
            Some("AR") => errors::APPLICATION_REJECT,
            Some("CE") => errors::COMMIT_ERROR,
            Some("CR") => errors::COMMIT_REJECT,
            _ => errors::INVALID_ACK,
        };
        Err(CamelError::new(kind, format!("{} acknowledgement received from {}: {}", code.unwrap_or_else(|| "Invalid".into()), what, ack_text.replace('\r', "\\r"))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing() {
        let f = frame(b"MSH|x");
        assert_eq!(f[0], START_OF_BLOCK);
        assert_eq!(&f[f.len() - 2..], &[END_OF_BLOCK, END_OF_DATA]);
        let mut data = b"junk".to_vec();
        data.extend(frame(b"one"));
        data.extend(frame(b"two"));
        let mut r = BufReader::new(&data[..]);
        assert_eq!(read_frame(&mut r, 100).unwrap(), Some(b"one".to_vec()));
        assert_eq!(read_frame(&mut r, 100).unwrap(), Some(b"two".to_vec()));
        assert_eq!(read_frame(&mut r, 100).unwrap(), None);
        let mut r = BufReader::new(&[START_OF_BLOCK, b'a'][..]);
        assert!(read_frame(&mut r, 100).is_err());
    }
}

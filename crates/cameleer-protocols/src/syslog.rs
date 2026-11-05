//! Syslog: RFC 5424 / RFC 3164 parsing and formatting, the `syslog` data format and a
//! `syslog:udp://host:port` (or `syslog:tcp://`) component.
//!
//! Parsed messages are objects with the fields `format` (`RFC5424`/`RFC3164`),
//! `priority`, `facility` (name, e.g. `AUTH`), `facilityCode`, `severity` (`CRIT`...),
//! `severityCode`, `version` (5424), `timestamp` (as in the message), `timestampMillis`
//! (when parseable), `hostname`, `appName`, `procId`, `msgId`, `structuredData`
//! (`{"sdId": {"param": "value"}}`), `logMessage`. Nil values (`-`) are null.
//!
//! The data format and the consumer set the headers `CamelSyslogFacility`,
//! `CamelSyslogSeverity`, `CamelSyslogHostname`, `CamelSyslogTimestamp` (plus
//! `CamelSyslogRemoteAddress`/`CamelSyslogLocalAddress` on the consumer).
//!
//! Component options: `unmarshal` (true: body is the parsed object, false: the raw text),
//! `charset` accepted. TCP accepts octet-counted (RFC 6587) and LF terminated frames;
//! the producer sends one datagram (UDP) or an LF terminated line (TCP).
//!
//! TLS (RFC 5425): `syslog:tls://host:port` (default port 6514), or `syslog:tcp://...` with
//! `ssl=true` / `sslContextParameters=#bean`. The consumer needs key material in the
//! parameters (or the global `camel.ssl.*` ones); over TLS the producer uses
//! octet-counted framing.

use std::any::Any;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use cameleer_core::component::support::process_consumer_exchange;
use cameleer_core::context::{CamelContext, WeakContext};
use cameleer_core::dataformat::DataFormat;
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use cameleer_core::util::Parameters;
use cameleer_core::util_time::{civil_from_epoch, epoch_from_civil, now_millis};
use cameleer_core::value::{ObjectMap, Value};
use cameleer_tls::camel_support::{endpoint_tls, EndpointTls};
use cameleer_tls::net::NetStream;

use crate::common::{connect_stream, host_port, io_error, split_host_port, take_ignored, TcpServer, COMMON_IGNORED};
use crate::ftp::listing::{month_name, month_number};

/// Header names (Camel `SyslogConstants`).
pub mod headers {
    pub const FACILITY: &str = "CamelSyslogFacility";
    pub const SEVERITY: &str = "CamelSyslogSeverity";
    pub const HOSTNAME: &str = "CamelSyslogHostname";
    pub const TIMESTAMP: &str = "CamelSyslogTimestamp";
    pub const LOCAL_ADDRESS: &str = "CamelSyslogLocalAddress";
    pub const REMOTE_ADDRESS: &str = "CamelSyslogRemoteAddress";
}

/// Facility names by code (Camel `SyslogFacility`).
pub const FACILITIES: [&str; 24] = [
    "KERN", "USER", "MAIL", "DAEMON", "AUTH", "SYSLOG", "LPR", "NEWS", "UUCP", "CRON", "AUTHPRIV", "FTP", "NTP", "AUDIT", "ALERT", "CLOCK", "LOCAL0",
    "LOCAL1", "LOCAL2", "LOCAL3", "LOCAL4", "LOCAL5", "LOCAL6", "LOCAL7",
];
/// Severity names by code (Camel `SyslogSeverity`).
pub const SEVERITIES: [&str; 8] = ["EMERG", "ALERT", "CRIT", "ERR", "WARNING", "NOTICE", "INFO", "DEBUG"];

/// Exception kind for unparseable messages.
pub const SYSLOG_PARSE_EXCEPTION: &str = "ParseException";

fn parse_err(msg: impl Into<String>) -> CamelError {
    CamelError::new(SYSLOG_PARSE_EXCEPTION, msg)
}

fn nil(s: &str) -> Value {
    if s == "-" {
        Value::Null
    } else {
        Value::String(s.to_string())
    }
}

/// Parse an RFC 3339 timestamp (`2003-08-24T05:14:15.000003-07:00`) to epoch millis.
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || (b[10] != b'T' && b[10] != b't') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let n = |a: usize, z: usize| s.get(a..z)?.parse::<u32>().ok();
    let mut secs = epoch_from_civil(s[0..4].parse().ok()?, n(5, 7)?, n(8, 10)?, n(11, 13)?, n(14, 16)?, n(17, 19)?);
    let mut rest = &s[19..];
    let mut ms = 0i64;
    if let Some(f) = rest.strip_prefix('.') {
        let digits: String = f.chars().take_while(|c| c.is_ascii_digit()).collect();
        ms = format!("{:0<3}", &digits[..digits.len().min(3)]).parse().ok()?;
        rest = &f[digits.len()..];
    }
    if rest != "Z" && rest != "z" {
        let sign = match rest.as_bytes().first()? {
            b'+' => 1,
            b'-' => -1,
            _ => return None,
        };
        let (h, m) = rest[1..].split_once(':')?;
        secs -= sign * (h.parse::<i64>().ok()? * 3600 + m.parse::<i64>().ok()? * 60);
    }
    Some(secs * 1000 + ms)
}

fn format_rfc3339(millis: i64) -> String {
    let (y, mo, d, h, mi, s) = civil_from_epoch(millis.div_euclid(1000));
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", y, mo, d, h, mi, s, millis.rem_euclid(1000))
}

/// Parse RFC 5424 structured data: `[id k="v"][id2 ...]` or `-`. Returns (data, rest).
fn parse_structured_data(s: &str) -> Result<(Value, &str)> {
    if let Some(rest) = s.strip_prefix('-') {
        return Ok((Value::Null, rest));
    }
    let mut map = ObjectMap::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i] == b'[' {
        i += 1;
        let start = i;
        while i < b.len() && b[i] != b' ' && b[i] != b']' {
            i += 1;
        }
        let id = s[start..i].to_string();
        let mut params = ObjectMap::new();
        loop {
            while i < b.len() && b[i] == b' ' {
                i += 1;
            }
            if i >= b.len() {
                return Err(parse_err("Unterminated structured data"));
            }
            if b[i] == b']' {
                i += 1;
                break;
            }
            let ks = i;
            while i < b.len() && b[i] != b'=' {
                i += 1;
            }
            let key = s[ks..i].to_string();
            i += 1;
            if b.get(i) != Some(&b'"') {
                return Err(parse_err("Structured data parameter value must be quoted"));
            }
            i += 1;
            let mut val = String::new();
            loop {
                let c = s[i..].chars().next().ok_or_else(|| parse_err("Unterminated parameter value"))?;
                i += c.len_utf8();
                match c {
                    '\\' => {
                        let n = s[i..].chars().next().ok_or_else(|| parse_err("Unterminated parameter value"))?;
                        if !matches!(n, '"' | '\\' | ']') {
                            val.push('\\');
                        }
                        val.push(n);
                        i += n.len_utf8();
                    }
                    '"' => break,
                    c => val.push(c),
                }
            }
            params.insert(key, Value::String(val));
        }
        map.insert(id, Value::Object(params));
    }
    if map.is_empty() {
        return Err(parse_err("Invalid structured data"));
    }
    Ok((Value::Object(map), &s[i..]))
}

/// Parse a syslog message (RFC 5424 if a version follows the PRI, else RFC 3164).
pub fn parse(text: &str) -> Result<Value> {
    let text = text.trim_end_matches(['\r', '\n', '\0']);
    let rest = text.strip_prefix('<').ok_or_else(|| parse_err(format!("Syslog message must start with <PRI>: {}", text)))?;
    let end = rest.find('>').filter(|&e| (1..=3).contains(&e)).ok_or_else(|| parse_err("Invalid PRI"))?;
    let pri: u32 = rest[..end].parse().map_err(|_| parse_err("Invalid PRI"))?;
    if pri > 191 {
        return Err(parse_err(format!("Invalid PRI: {}", pri)));
    }
    let rest = &rest[end + 1..];
    let mut m = ObjectMap::new();
    let (fac, sev) = ((pri / 8) as usize, (pri % 8) as usize);
    m.insert("priority".to_string(), Value::Int(pri as i64));
    m.insert("facility".to_string(), Value::String(FACILITIES[fac].to_string()));
    m.insert("facilityCode".to_string(), Value::Int(fac as i64));
    m.insert("severity".to_string(), Value::String(SEVERITIES[sev].to_string()));
    m.insert("severityCode".to_string(), Value::Int(sev as i64));
    let version: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !version.is_empty() && rest[version.len()..].starts_with(' ') {
        // RFC 5424: VERSION SP TIMESTAMP SP HOSTNAME SP APP-NAME SP PROCID SP MSGID SP SD [SP MSG]
        let mut parts = rest[version.len() + 1..].splitn(6, ' ');
        let mut next = |what: &str| parts.next().ok_or_else(|| parse_err(format!("Missing {} in RFC 5424 message", what)));
        let ts = next("TIMESTAMP")?;
        let host = next("HOSTNAME")?;
        let app = next("APP-NAME")?;
        let proc_id = next("PROCID")?;
        let msg_id = next("MSGID")?;
        let tail = parts.next().unwrap_or("-");
        let (sd, after) = parse_structured_data(tail)?;
        let msg = after.strip_prefix(' ').unwrap_or(after);
        let msg = msg.strip_prefix('\u{feff}').unwrap_or(msg);
        m.insert("format".into(), Value::String("RFC5424".into()));
        m.insert("version".into(), Value::Int(version.parse().unwrap_or(1)));
        m.insert("timestamp".into(), nil(ts));
        if let Some(ms) = parse_rfc3339(ts) {
            m.insert("timestampMillis".into(), Value::Int(ms));
        }
        m.insert("hostname".into(), nil(host));
        m.insert("appName".into(), nil(app));
        m.insert("procId".into(), nil(proc_id));
        m.insert("msgId".into(), nil(msg_id));
        m.insert("structuredData".into(), sd);
        m.insert("logMessage".into(), if msg.is_empty() { Value::Null } else { Value::String(msg.to_string()) });
        return Ok(Value::Object(m));
    }
    // RFC 3164: "Mmm dd hh:mm:ss HOSTNAME TAG[pid]: MSG"
    m.insert("format".into(), Value::String("RFC3164".into()));
    let mut msg_part = rest;
    if rest.len() >= 15 && month_number(&rest[..3]).is_some() && rest.as_bytes()[3] == b' ' && rest.as_bytes()[9] == b':' {
        let ts = &rest[..15];
        m.insert("timestamp".into(), Value::String(ts.to_string()));
        let day: u32 = ts[4..6].trim().parse().unwrap_or(1);
        let hms: Vec<u32> = ts[7..15].split(':').filter_map(|x| x.parse().ok()).collect();
        if hms.len() == 3 {
            let (y, ..) = civil_from_epoch(now_millis() / 1000);
            let secs = epoch_from_civil(y, month_number(&ts[..3]).unwrap_or(1), day, hms[0], hms[1], hms[2]);
            m.insert("timestampMillis".into(), Value::Int(secs * 1000));
        }
        msg_part = rest[15..].trim_start_matches(' ');
        if let Some((host, after)) = msg_part.split_once(' ') {
            m.insert("hostname".into(), Value::String(host.to_string()));
            msg_part = after;
        }
    }
    // TAG: alphanumeric (up to 32 chars), optionally [pid], terminated by ':'
    let tag_len = msg_part.find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '/')).unwrap_or(0);
    let after_tag = &msg_part[tag_len..];
    if tag_len > 0 && tag_len <= 32 && (after_tag.starts_with(':') || after_tag.starts_with('[')) {
        let tag = &msg_part[..tag_len];
        let mut after = after_tag;
        if let Some(r) = after.strip_prefix('[') {
            if let Some(close) = r.find(']') {
                m.insert("procId".into(), Value::String(r[..close].to_string()));
                after = &r[close + 1..];
            }
        }
        if let Some(r) = after.strip_prefix(':') {
            m.insert("appName".into(), Value::String(tag.to_string()));
            m.insert("tag".into(), Value::String(tag.to_string()));
            msg_part = r.strip_prefix(' ').unwrap_or(r);
        } else {
            m.remove("procId");
        }
    }
    m.insert("logMessage".into(), Value::String(msg_part.to_string()));
    Ok(Value::Object(m))
}

fn code_of(v: &Value, name_key: &str, code_key: &str, names: &[&str], default: usize) -> usize {
    if let Some(c) = v.get(code_key).and_then(|c| c.as_i64()) {
        return (c.max(0) as usize).min(names.len() - 1);
    }
    match v.get(name_key) {
        Some(Value::Int(i)) => (*i).clamp(0, names.len() as i64 - 1) as usize,
        Some(n) => names.iter().position(|x| x.eq_ignore_ascii_case(&n.to_string())).unwrap_or(default),
        None => default,
    }
}

/// Format a message object (see [`parse`]) as RFC 5424, or RFC 3164 when
/// `format` is `RFC3164`. Missing fields use defaults (facility USER, severity INFO, now).
pub fn format(v: &Value) -> String {
    let fac = code_of(v, "facility", "facilityCode", &FACILITIES, 1);
    let sev = code_of(v, "severity", "severityCode", &SEVERITIES, 6);
    let pri = fac * 8 + sev;
    let s = |k: &str| v.get(k).filter(|x| !x.is_null()).map(|x| x.to_string());
    let msg = s("logMessage").unwrap_or_default();
    if s("format").map(|f| f.eq_ignore_ascii_case("RFC3164")).unwrap_or(false) {
        let ts = s("timestamp").unwrap_or_else(|| {
            let (_, mo, d, h, mi, sec) = civil_from_epoch(v.get("timestampMillis").and_then(|t| t.as_i64()).unwrap_or_else(now_millis) / 1000);
            format!("{} {:>2} {:02}:{:02}:{:02}", month_name(mo), d, h, mi, sec)
        });
        let host = s("hostname").unwrap_or_else(|| "localhost".into());
        let tag = match (s("appName").or_else(|| s("tag")), s("procId")) {
            (Some(a), Some(p)) => format!("{}[{}]: ", a, p),
            (Some(a), None) => format!("{}: ", a),
            _ => String::new(),
        };
        return format!("<{}>{} {} {}{}", pri, ts, host, tag, msg);
    }
    let ts = s("timestamp").unwrap_or_else(|| format_rfc3339(v.get("timestampMillis").and_then(|t| t.as_i64()).unwrap_or_else(now_millis)));
    let dash = |k: &str| s(k).filter(|x| !x.is_empty()).unwrap_or_else(|| "-".into());
    let sd = match v.get("structuredData") {
        Some(Value::Object(map)) if !map.is_empty() => map
            .iter()
            .map(|(id, params)| {
                let ps: String = params
                    .as_object()
                    .map(|p| p.iter().map(|(k, v)| format!(" {}=\"{}\"", k, v.to_string().replace('\\', "\\\\").replace('"', "\\\"").replace(']', "\\]"))).collect())
                    .unwrap_or_default();
                format!("[{}{}]", id, ps)
            })
            .collect::<String>(),
        _ => "-".into(),
    };
    let version = v.get("version").and_then(|x| x.as_i64()).unwrap_or(1);
    let mut out = format!("<{}>{} {} {} {} {} {} {}", pri, version, ts, dash("hostname"), dash("appName"), dash("procId"), dash("msgId"), sd);
    if !msg.is_empty() {
        out.push(' ');
        out.push_str(&msg);
    }
    out
}

fn set_headers(ex: &mut Exchange, v: &Value) {
    let g = |k: &str| v.get(k).cloned().unwrap_or(Value::Null);
    ex.set_header(headers::FACILITY, g("facility"));
    ex.set_header(headers::SEVERITY, g("severity"));
    ex.set_header(headers::HOSTNAME, g("hostname"));
    ex.set_header(headers::TIMESTAMP, g("timestamp"));
}

/// The `syslog` data format: unmarshal text to the message object (and headers),
/// marshal an object to RFC 5424 (or 3164) text.
#[derive(Clone, Debug, Default)]
pub struct SyslogDataFormat;

impl DataFormat for SyslogDataFormat {
    fn name(&self) -> &str {
        "syslog"
    }
    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        let text = match ex.body() {
            Value::Object(_) => format(ex.body()),
            other => other.to_string(),
        };
        ex.set_body(text);
        Ok(())
    }
    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        let text = match ex.body() {
            Value::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
            other => other.to_string(),
        };
        let v = parse(&text)?;
        set_headers(ex, &v);
        ex.set_body(v);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// component
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Transport {
    Udp,
    Tcp,
}

#[derive(Clone, Debug)]
struct SyslogConfig {
    transport: Transport,
    host: String,
    port: u16,
    unmarshal: bool,
    tls: Option<EndpointTls>,
}

/// The `syslog` component.
#[derive(Default)]
pub struct SyslogComponent;

impl Component for SyslogComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let (transport, rest) = if let Some(r) = remaining.strip_prefix("udp://").or_else(|| remaining.strip_prefix("udp:")) {
            (Transport::Udp, r)
        } else if let Some(r) = remaining.strip_prefix("tcp://").or_else(|| remaining.strip_prefix("tcp:")) {
            (Transport::Tcp, r)
        } else if let Some(r) = remaining.strip_prefix("tls://").or_else(|| remaining.strip_prefix("tls:")) {
            (Transport::Tcp, r)
        } else {
            (Transport::Udp, remaining)
        };
        let tls_scheme = remaining.starts_with("tls:");
        let (host, port) = split_host_port(rest.trim_end_matches('/'), if tls_scheme { 6514 } else { 514 })?;
        let tls = endpoint_tls(ctx, params, "syslog", tls_scheme, &["ssl"])?;
        if tls.is_some() && transport == Transport::Udp {
            return Err(CamelError::illegal_argument("syslog over TLS requires the tcp or tls transport"));
        }
        let cfg = SyslogConfig { transport, host, port, unmarshal: params.take_bool("unmarshal", true)?, tls };
        take_ignored(params, COMMON_IGNORED);
        take_ignored(params, &["charset", "sync", "textline", "delimiter", "receiveBufferSize", "sendBufferSize"]);
        Ok(Arc::new(SyslogEndpoint { uri: uri.to_string(), ctx: ctx.downgrade(), cfg, port: Arc::new(Mutex::new(None)) }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A `syslog` endpoint.
pub struct SyslogEndpoint {
    uri: String,
    ctx: WeakContext,
    cfg: SyslogConfig,
    port: Arc<Mutex<Option<u16>>>,
}

impl SyslogEndpoint {
    /// The bound port while a consumer is running.
    pub fn local_port(&self) -> Option<u16> {
        *self.port.lock().unwrap()
    }
}

impl Endpoint for SyslogEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(SyslogProducer { cfg: self.cfg.clone() }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        Ok(Arc::new(SyslogConsumer {
            cfg: self.cfg.clone(),
            ctx: self.ctx.clone(),
            processor,
            logger: self.uri.clone(),
            port: self.port.clone(),
            state: Mutex::new(None),
        }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct Router {
    cfg: SyslogConfig,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
    logger: String,
}

impl Router {
    fn route(&self, data: &[u8], remote: Option<SocketAddr>, local: Option<SocketAddr>) {
        let text = String::from_utf8_lossy(data).trim_end_matches(['\r', '\n', '\0']).to_string();
        if text.is_empty() {
            return;
        }
        let mut ex = Exchange::new(self.ctx.upgrade());
        if let Some(r) = remote {
            ex.set_header(headers::REMOTE_ADDRESS, r.to_string());
        }
        if let Some(l) = local {
            ex.set_header(headers::LOCAL_ADDRESS, l.to_string());
        }
        match parse(&text) {
            Ok(v) => {
                set_headers(&mut ex, &v);
                ex.set_body(if self.cfg.unmarshal { v } else { Value::String(text) });
            }
            Err(e) if self.cfg.unmarshal => {
                ex.set_body(text);
                ex.set_exception(e);
            }
            Err(_) => ex.set_body(text),
        }
        if ex.exception.is_some() {
            if let Some(e) = &ex.exception {
                cameleer_core::camel_warn!(&self.logger, "Invalid syslog message: {}", e.message);
            }
            return;
        }
        process_consumer_exchange(&self.logger, self.processor.as_ref(), &mut ex);
    }

    /// Serve a TCP connection: octet-counted (`LEN SP MSG`) or LF terminated frames.
    fn serve_tcp(&self, tcp: TcpStream) {
        let remote = tcp.peer_addr().ok();
        let local = tcp.local_addr().ok();
        let stream = match &self.cfg.tls {
            Some(t) => match t.accept(tcp, "syslog consumer") {
                Ok(s) => s,
                Err(e) => {
                    cameleer_core::camel_debug!(&self.logger, "syslog TLS handshake failed: {}", e);
                    return;
                }
            },
            None => NetStream::plain(tcp),
        };
        let mut r = BufReader::new(&stream);
        loop {
            let first = match r.fill_buf() {
                Ok(b) if !b.is_empty() => b[0],
                _ => break,
            };
            if first.is_ascii_digit() {
                let mut len = Vec::new();
                if r.read_until(b' ', &mut len).is_err() {
                    break;
                }
                let n: usize = String::from_utf8_lossy(&len).trim().parse().unwrap_or(0);
                let mut buf = vec![0u8; n.min(1024 * 1024)];
                if r.read_exact(&mut buf).is_err() {
                    break;
                }
                self.route(&buf, remote, local);
            } else {
                let mut buf = Vec::new();
                match r.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => self.route(&buf, remote, local),
                }
            }
        }
    }
}

enum Running {
    Udp(Arc<AtomicBool>, Option<JoinHandle<()>>),
    Tcp(TcpServer),
}

struct SyslogConsumer {
    cfg: SyslogConfig,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
    logger: String,
    port: Arc<Mutex<Option<u16>>>,
    state: Mutex<Option<Running>>,
}

impl Service for SyslogConsumer {
    fn start(&self) -> Result<()> {
        let mut g = self.state.lock().unwrap();
        if g.is_some() {
            return Ok(());
        }
        let router = Arc::new(Router { cfg: self.cfg.clone(), ctx: self.ctx.clone(), processor: self.processor.clone(), logger: self.logger.clone() });
        let addr = host_port(&self.cfg.host, self.cfg.port);
        let running = match self.cfg.transport {
            Transport::Udp => {
                let socket = UdpSocket::bind(&addr).map_err(|e| CamelError::io(format!("Failed to bind syslog udp consumer on {}: {}", addr, e)))?;
                let local = socket.local_addr().map_err(|e| CamelError::io(e.to_string()))?;
                let _ = socket.set_read_timeout(Some(Duration::from_millis(50)));
                *self.port.lock().unwrap() = Some(local.port());
                let run = Arc::new(AtomicBool::new(true));
                let r2 = run.clone();
                let h = std::thread::Builder::new()
                    .name(format!("syslog-udp-{}", local.port()))
                    .spawn(move || {
                        let mut buf = vec![0u8; 65536];
                        while r2.load(Ordering::SeqCst) {
                            if let Ok((n, from)) = socket.recv_from(&mut buf) {
                                router.route(&buf[..n], Some(from), Some(local));
                            }
                        }
                    })
                    .map_err(|e| CamelError::io(e.to_string()))?;
                Running::Udp(run, Some(h))
            }
            Transport::Tcp => {
                let s = TcpServer::bind(&self.cfg.host, self.cfg.port, "syslog", move |stream| router.serve_tcp(stream))?;
                *self.port.lock().unwrap() = Some(s.port());
                Running::Tcp(s)
            }
        };
        *g = Some(running);
        Ok(())
    }
    fn stop(&self) -> Result<()> {
        match self.state.lock().unwrap().take() {
            Some(Running::Udp(run, h)) => {
                run.store(false, Ordering::SeqCst);
                if let Some(h) = h {
                    let _ = h.join();
                }
            }
            Some(Running::Tcp(s)) => s.stop(),
            None => {}
        }
        *self.port.lock().unwrap() = None;
        Ok(())
    }
}

struct SyslogProducer {
    cfg: SyslogConfig,
}

impl Service for SyslogProducer {}

impl Processor for SyslogProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let text = match ex.body() {
            Value::Object(_) => format(ex.body()),
            Value::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
            other => other.to_string(),
        };
        let host = if self.cfg.host == "0.0.0.0" { "127.0.0.1" } else { &self.cfg.host };
        let what = host_port(host, self.cfg.port);
        match self.cfg.transport {
            Transport::Udp => {
                use std::net::ToSocketAddrs;
                let target = (host, self.cfg.port).to_socket_addrs().map_err(|e| io_error(e, &what))?.next().ok_or_else(|| CamelError::io(format!("Unknown host {}", host)))?;
                let socket = UdpSocket::bind(if target.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" }).map_err(|e| io_error(e, &what))?;
                socket.send_to(text.as_bytes(), target).map_err(|e| io_error(e, &what))?;
            }
            Transport::Tcp => {
                let mut s = connect_stream(host, self.cfg.port, Duration::from_secs(10), self.cfg.tls.as_ref())?;
                let frame = if s.is_tls() { format!("{} {}", text.len(), text) } else { format!("{}\n", text) };
                s.write_all(frame.as_bytes()).map_err(|e| io_error(e, &what))?;
                let _ = s.shutdown(std::net::Shutdown::Both);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(v: &Value, k: &str) -> Value {
        v.get(k).cloned().unwrap_or(Value::Null)
    }

    #[test]
    fn rfc5424_examples() {
        // RFC 5424 section 6.5, example 1 (with BOM)
        let v = parse("<34>1 2003-10-11T22:14:15.003Z mymachine.example.com su - ID47 - \u{feff}'su root' failed for lonvick on /dev/pts/8").unwrap();
        assert_eq!(get(&v, "facility"), Value::from("AUTH"));
        assert_eq!(get(&v, "severity"), Value::from("CRIT"));
        assert_eq!(get(&v, "version"), Value::Int(1));
        assert_eq!(get(&v, "hostname"), Value::from("mymachine.example.com"));
        assert_eq!(get(&v, "appName"), Value::from("su"));
        assert_eq!(get(&v, "procId"), Value::Null);
        assert_eq!(get(&v, "msgId"), Value::from("ID47"));
        assert_eq!(get(&v, "structuredData"), Value::Null);
        assert_eq!(get(&v, "logMessage"), Value::from("'su root' failed for lonvick on /dev/pts/8"));
        assert_eq!(get(&v, "timestampMillis"), Value::Int(epoch_from_civil(2003, 10, 11, 22, 14, 15) * 1000 + 3));
        // example 2: time zone offset, procid
        let v = parse("<165>1 2003-08-24T05:14:15.000003-07:00 192.0.2.1 myproc 8710 - - %% It's time to make the do-nuts.").unwrap();
        assert_eq!(get(&v, "facility"), Value::from("LOCAL4"));
        assert_eq!(get(&v, "severity"), Value::from("NOTICE"));
        assert_eq!(get(&v, "procId"), Value::from("8710"));
        assert_eq!(get(&v, "msgId"), Value::Null);
        assert_eq!(get(&v, "timestampMillis"), Value::Int(epoch_from_civil(2003, 8, 24, 12, 14, 15) * 1000));
        assert_eq!(get(&v, "logMessage"), Value::from("%% It's time to make the do-nuts."));
        // example 3: structured data
        let v = parse("<165>1 2003-10-11T22:14:15.003Z mymachine.example.com evntslog - ID47 [exampleSDID@32473 iut=\"3\" eventSource=\"Application\" eventID=\"1011\"] \u{feff}An application event log entry...").unwrap();
        let sd = get(&v, "structuredData");
        assert_eq!(sd.get("exampleSDID@32473").and_then(|p| p.get("eventSource")), Some(&Value::from("Application")));
        assert_eq!(get(&v, "logMessage"), Value::from("An application event log entry..."));
        // example 4: two SD elements, no message
        let v = parse("<165>1 2003-10-11T22:14:15.003Z mymachine.example.com evntslog - ID47 [exampleSDID@32473 iut=\"3\" eventSource=\"Application\" eventID=\"1011\"][examplePriority@32473 class=\"high\"]").unwrap();
        assert_eq!(get(&v, "structuredData").get("examplePriority@32473").and_then(|p| p.get("class")), Some(&Value::from("high")));
        assert_eq!(get(&v, "logMessage"), Value::Null);
        // escapes in parameter values
        let v = parse("<14>1 - - - - - [x a=\"q\\\"b\\]c\\\\d\"] m").unwrap();
        assert_eq!(get(&v, "structuredData").get("x").and_then(|p| p.get("a")), Some(&Value::from("q\"b]c\\d")));
        assert!(parse("<14>1 - - - - - [x a=b] m").is_err());
        assert!(parse("no pri").is_err());
        assert!(parse("<999>1 - - - - - -").is_err());
    }

    #[test]
    fn rfc3164_examples() {
        let v = parse("<34>Oct 11 22:14:15 mymachine su: 'su root' failed for lonvick on /dev/pts/8").unwrap();
        assert_eq!(get(&v, "format"), Value::from("RFC3164"));
        assert_eq!(get(&v, "facility"), Value::from("AUTH"));
        assert_eq!(get(&v, "severity"), Value::from("CRIT"));
        assert_eq!(get(&v, "timestamp"), Value::from("Oct 11 22:14:15"));
        assert_eq!(get(&v, "hostname"), Value::from("mymachine"));
        assert_eq!(get(&v, "appName"), Value::from("su"));
        assert_eq!(get(&v, "logMessage"), Value::from("'su root' failed for lonvick on /dev/pts/8"));
        let v = parse("<13>Feb  5 17:32:18 10.0.0.99 Use the BFG!").unwrap();
        assert_eq!(get(&v, "facility"), Value::from("USER"));
        assert_eq!(get(&v, "severity"), Value::from("NOTICE"));
        assert_eq!(get(&v, "hostname"), Value::from("10.0.0.99"));
        assert_eq!(get(&v, "appName"), Value::Null);
        assert_eq!(get(&v, "logMessage"), Value::from("Use the BFG!"));
        let v = parse("<30>Jan  1 00:00:01 host sshd[1234]: Accepted publickey").unwrap();
        assert_eq!((get(&v, "appName"), get(&v, "procId")), (Value::from("sshd"), Value::from("1234")));
        assert_eq!(get(&v, "severity"), Value::from("INFO"));
        assert_eq!(get(&v, "facility"), Value::from("DAEMON"));
    }

    #[test]
    fn format_round_trip() {
        for text in [
            "<165>1 2003-10-11T22:14:15.003Z mymachine.example.com evntslog - ID47 [exampleSDID@32473 eventID=\"1011\" eventSource=\"Application\" iut=\"3\"] An application event",
            "<34>1 2003-10-11T22:14:15.003Z mymachine.example.com su - ID47 - hello",
            "<34>Oct 11 22:14:15 mymachine su: 'su root' failed",
            "<30>Jan  1 00:00:01 host sshd[1234]: Accepted",
        ] {
            assert_eq!(format(&parse(text).unwrap()), text);
        }
        let v = Value::object([("facility", Value::from("LOCAL0")), ("severity", Value::from("ERR")), ("hostname", Value::from("h")), ("appName", Value::from("app")), ("logMessage", Value::from("boom"))]);
        let t = format(&v);
        assert!(t.starts_with("<131>1 "), "{}", t);
        assert!(t.ends_with(" h app - - - boom"), "{}", t);
    }
}

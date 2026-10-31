//! AMQP 0-9-1 wire format: frames, method arguments (with bit packing), field tables
//! and the `basic` content header properties.
//!
//! Methods are represented generically as `(class-id, method-id, args)`; the argument
//! types of every method used by the client and the embedded broker are listed in
//! [`schema`] so that incoming frames can be decoded.

use std::io::{self, Read, Write};

use camel_core::value::{ObjectMap, Value};

/// The protocol header sent by a client when it connects.
pub const PROTOCOL_HEADER: &[u8; 8] = b"AMQP\x00\x00\x09\x01";
/// Frame types.
pub const FRAME_METHOD: u8 = 1;
pub const FRAME_HEADER: u8 = 2;
pub const FRAME_BODY: u8 = 3;
pub const FRAME_HEARTBEAT: u8 = 8;
/// Frame terminator octet.
pub const FRAME_END: u8 = 0xCE;
/// Smallest frame size every peer must accept.
pub const FRAME_MIN_SIZE: u32 = 4096;

/// A field table (`headers`, `arguments`, server/client properties).
pub type Table = ObjectMap;

/// Class/method ids of the methods used by rust-camel.
pub mod m {
    pub const CONNECTION_START: (u16, u16) = (10, 10);
    pub const CONNECTION_START_OK: (u16, u16) = (10, 11);
    pub const CONNECTION_TUNE: (u16, u16) = (10, 30);
    pub const CONNECTION_TUNE_OK: (u16, u16) = (10, 31);
    pub const CONNECTION_OPEN: (u16, u16) = (10, 40);
    pub const CONNECTION_OPEN_OK: (u16, u16) = (10, 41);
    pub const CONNECTION_CLOSE: (u16, u16) = (10, 50);
    pub const CONNECTION_CLOSE_OK: (u16, u16) = (10, 51);
    pub const CHANNEL_OPEN: (u16, u16) = (20, 10);
    pub const CHANNEL_OPEN_OK: (u16, u16) = (20, 11);
    pub const CHANNEL_FLOW: (u16, u16) = (20, 20);
    pub const CHANNEL_FLOW_OK: (u16, u16) = (20, 21);
    pub const CHANNEL_CLOSE: (u16, u16) = (20, 40);
    pub const CHANNEL_CLOSE_OK: (u16, u16) = (20, 41);
    pub const EXCHANGE_DECLARE: (u16, u16) = (40, 10);
    pub const EXCHANGE_DECLARE_OK: (u16, u16) = (40, 11);
    pub const EXCHANGE_DELETE: (u16, u16) = (40, 20);
    pub const EXCHANGE_DELETE_OK: (u16, u16) = (40, 21);
    pub const QUEUE_DECLARE: (u16, u16) = (50, 10);
    pub const QUEUE_DECLARE_OK: (u16, u16) = (50, 11);
    pub const QUEUE_BIND: (u16, u16) = (50, 20);
    pub const QUEUE_BIND_OK: (u16, u16) = (50, 21);
    pub const QUEUE_PURGE: (u16, u16) = (50, 30);
    pub const QUEUE_PURGE_OK: (u16, u16) = (50, 31);
    pub const QUEUE_DELETE: (u16, u16) = (50, 40);
    pub const QUEUE_DELETE_OK: (u16, u16) = (50, 41);
    pub const QUEUE_UNBIND: (u16, u16) = (50, 50);
    pub const QUEUE_UNBIND_OK: (u16, u16) = (50, 51);
    pub const BASIC_QOS: (u16, u16) = (60, 10);
    pub const BASIC_QOS_OK: (u16, u16) = (60, 11);
    pub const BASIC_CONSUME: (u16, u16) = (60, 20);
    pub const BASIC_CONSUME_OK: (u16, u16) = (60, 21);
    pub const BASIC_CANCEL: (u16, u16) = (60, 30);
    pub const BASIC_CANCEL_OK: (u16, u16) = (60, 31);
    pub const BASIC_PUBLISH: (u16, u16) = (60, 40);
    pub const BASIC_RETURN: (u16, u16) = (60, 50);
    pub const BASIC_DELIVER: (u16, u16) = (60, 60);
    pub const BASIC_GET: (u16, u16) = (60, 70);
    pub const BASIC_GET_OK: (u16, u16) = (60, 71);
    pub const BASIC_GET_EMPTY: (u16, u16) = (60, 72);
    pub const BASIC_ACK: (u16, u16) = (60, 80);
    pub const BASIC_REJECT: (u16, u16) = (60, 90);
    pub const BASIC_RECOVER_ASYNC: (u16, u16) = (60, 100);
    pub const BASIC_RECOVER: (u16, u16) = (60, 110);
    pub const BASIC_RECOVER_OK: (u16, u16) = (60, 111);
    pub const BASIC_NACK: (u16, u16) = (60, 120);
    pub const CONFIRM_SELECT: (u16, u16) = (85, 10);
    pub const CONFIRM_SELECT_OK: (u16, u16) = (85, 11);
    pub const TX_SELECT: (u16, u16) = (90, 10);
    pub const TX_SELECT_OK: (u16, u16) = (90, 11);
    pub const TX_COMMIT: (u16, u16) = (90, 20);
    pub const TX_COMMIT_OK: (u16, u16) = (90, 21);
    pub const TX_ROLLBACK: (u16, u16) = (90, 30);
    pub const TX_ROLLBACK_OK: (u16, u16) = (90, 31);
}

/// Reply codes.
pub mod reply {
    pub const SUCCESS: u16 = 200;
    pub const NO_ROUTE: u16 = 312;
    pub const ACCESS_REFUSED: u16 = 403;
    pub const NOT_FOUND: u16 = 404;
    pub const RESOURCE_LOCKED: u16 = 405;
    pub const PRECONDITION_FAILED: u16 = 406;
    pub const FRAME_ERROR: u16 = 501;
    pub const COMMAND_INVALID: u16 = 503;
    pub const CHANNEL_ERROR: u16 = 504;
    pub const NOT_IMPLEMENTED: u16 = 540;
}

/// A method argument.
#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    Octet(u8),
    Short(u16),
    Long(u32),
    LongLong(u64),
    ShortStr(String),
    LongStr(Vec<u8>),
    Bit(bool),
    Table(Table),
}

#[derive(Clone, Copy, Debug)]
enum T {
    O,
    S,
    L,
    LL,
    SS,
    LS,
    B,
    Tb,
}

/// Argument types of a method (None for methods rust-camel does not know).
fn schema(class: u16, method: u16) -> Option<&'static [T]> {
    use T::*;
    Some(match (class, method) {
        (10, 10) => &[O, O, Tb, LS, LS],
        (10, 11) => &[Tb, SS, LS, SS],
        (10, 20) | (10, 21) => &[LS],
        (10, 30) | (10, 31) => &[S, L, S],
        (10, 40) => &[SS, SS, B],
        (10, 41) => &[SS],
        (10, 50) | (20, 40) => &[S, SS, S, S],
        (10, 51) | (20, 41) => &[],
        (20, 10) => &[SS],
        (20, 11) => &[LS],
        (20, 20) | (20, 21) => &[B],
        (40, 10) => &[S, SS, SS, B, B, B, B, B, Tb],
        (40, 11) | (40, 21) => &[],
        (40, 20) => &[S, SS, B, B],
        (50, 10) => &[S, SS, B, B, B, B, B, Tb],
        (50, 11) => &[SS, L, L],
        (50, 20) => &[S, SS, SS, SS, B, Tb],
        (50, 21) | (50, 51) => &[],
        (50, 30) => &[S, SS, B],
        (50, 31) | (50, 41) => &[L],
        (50, 40) => &[S, SS, B, B, B],
        (50, 50) => &[S, SS, SS, SS, Tb],
        (60, 10) => &[L, S, B],
        (60, 11) | (60, 111) => &[],
        (60, 20) => &[S, SS, SS, B, B, B, B, Tb],
        (60, 21) | (60, 31) => &[SS],
        (60, 30) => &[SS, B],
        (60, 40) => &[S, SS, SS, B, B],
        (60, 50) => &[S, SS, SS, SS],
        (60, 60) => &[SS, LL, B, SS, SS],
        (60, 70) => &[S, SS, B],
        (60, 71) => &[LL, B, SS, SS, L],
        (60, 72) => &[SS],
        (60, 80) | (60, 90) => &[LL, B],
        (60, 100) | (60, 110) => &[B],
        (60, 120) => &[LL, B, B],
        (85, 10) => &[B],
        (85, 11) => &[],
        (90, 10) | (90, 11) | (90, 20) | (90, 21) | (90, 30) | (90, 31) => &[],
        _ => return None,
    })
}

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// A decoded method frame payload.
#[derive(Clone, Debug, PartialEq)]
pub struct Method {
    pub class: u16,
    pub method: u16,
    pub args: Vec<Arg>,
}

impl Method {
    pub fn new(id: (u16, u16), args: Vec<Arg>) -> Method {
        Method { class: id.0, method: id.1, args }
    }
    /// Whether this is the method `id`.
    pub fn is(&self, id: (u16, u16)) -> bool {
        (self.class, self.method) == id
    }
    pub fn id(&self) -> (u16, u16) {
        (self.class, self.method)
    }
    /// Human readable name for error messages.
    pub fn name(&self) -> String {
        let class = match self.class {
            10 => "connection",
            20 => "channel",
            40 => "exchange",
            50 => "queue",
            60 => "basic",
            85 => "confirm",
            90 => "tx",
            _ => "unknown",
        };
        format!("{}.{}", class, self.method)
    }

    /// Encode as a method frame payload (consecutive bits are packed into octets).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64);
        out.extend_from_slice(&self.class.to_be_bytes());
        out.extend_from_slice(&self.method.to_be_bytes());
        let mut bits: Vec<bool> = Vec::new();
        let flush = |bits: &mut Vec<bool>, out: &mut Vec<u8>| {
            for chunk in bits.chunks(8) {
                let mut b = 0u8;
                for (i, v) in chunk.iter().enumerate() {
                    if *v {
                        b |= 1 << i;
                    }
                }
                out.push(b);
            }
            bits.clear();
        };
        for a in &self.args {
            if let Arg::Bit(v) = a {
                bits.push(*v);
                continue;
            }
            flush(&mut bits, &mut out);
            match a {
                Arg::Octet(v) => out.push(*v),
                Arg::Short(v) => out.extend_from_slice(&v.to_be_bytes()),
                Arg::Long(v) => out.extend_from_slice(&v.to_be_bytes()),
                Arg::LongLong(v) => out.extend_from_slice(&v.to_be_bytes()),
                Arg::ShortStr(s) => put_short_str(&mut out, s),
                Arg::LongStr(b) => put_long_bytes(&mut out, b),
                Arg::Table(t) => put_table(&mut out, t),
                Arg::Bit(_) => unreachable!(),
            }
        }
        flush(&mut bits, &mut out);
        out
    }

    /// Decode a method frame payload.
    pub fn decode(payload: &[u8]) -> io::Result<Method> {
        let mut r = Rd::new(payload);
        let class = r.u16()?;
        let method = r.u16()?;
        let types = schema(class, method).ok_or_else(|| invalid(format!("unknown method {}.{}", class, method)))?;
        let mut args = Vec::with_capacity(types.len());
        let mut bit_byte = 0u8;
        let mut bit_pos = 8u8;
        for t in types {
            if let T::B = t {
                if bit_pos >= 8 {
                    bit_byte = r.u8()?;
                    bit_pos = 0;
                }
                args.push(Arg::Bit(bit_byte & (1 << bit_pos) != 0));
                bit_pos += 1;
                continue;
            }
            bit_pos = 8;
            args.push(match t {
                T::O => Arg::Octet(r.u8()?),
                T::S => Arg::Short(r.u16()?),
                T::L => Arg::Long(r.u32()?),
                T::LL => Arg::LongLong(r.u64()?),
                T::SS => Arg::ShortStr(r.short_str()?),
                T::LS => Arg::LongStr(r.long_bytes()?),
                T::Tb => Arg::Table(r.table()?),
                T::B => unreachable!(),
            });
        }
        Ok(Method { class, method, args })
    }

    fn arg(&self, i: usize) -> Option<&Arg> {
        self.args.get(i)
    }
    /// Argument `i` as a string (short or long string); empty if absent.
    pub fn str(&self, i: usize) -> String {
        match self.arg(i) {
            Some(Arg::ShortStr(s)) => s.clone(),
            Some(Arg::LongStr(b)) => String::from_utf8_lossy(b).into_owned(),
            _ => String::new(),
        }
    }
    /// Argument `i` as raw bytes (long string).
    pub fn bytes(&self, i: usize) -> Vec<u8> {
        match self.arg(i) {
            Some(Arg::LongStr(b)) => b.clone(),
            Some(Arg::ShortStr(s)) => s.as_bytes().to_vec(),
            _ => Vec::new(),
        }
    }
    /// Argument `i` as an unsigned number.
    pub fn num(&self, i: usize) -> u64 {
        match self.arg(i) {
            Some(Arg::Octet(v)) => *v as u64,
            Some(Arg::Short(v)) => *v as u64,
            Some(Arg::Long(v)) => *v as u64,
            Some(Arg::LongLong(v)) => *v,
            _ => 0,
        }
    }
    pub fn bit(&self, i: usize) -> bool {
        matches!(self.arg(i), Some(Arg::Bit(true)))
    }
    pub fn table(&self, i: usize) -> Table {
        match self.arg(i) {
            Some(Arg::Table(t)) => t.clone(),
            _ => Table::new(),
        }
    }
}

/// `basic` content header properties.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BasicProperties {
    pub content_type: Option<String>,
    pub content_encoding: Option<String>,
    pub headers: Option<Table>,
    /// 1 = non-persistent, 2 = persistent.
    pub delivery_mode: Option<u8>,
    pub priority: Option<u8>,
    pub correlation_id: Option<String>,
    pub reply_to: Option<String>,
    /// Per-message TTL in milliseconds (as a string, like RabbitMQ).
    pub expiration: Option<String>,
    pub message_id: Option<String>,
    /// Seconds since the epoch.
    pub timestamp: Option<u64>,
    /// The `type` property.
    pub kind: Option<String>,
    pub user_id: Option<String>,
    pub app_id: Option<String>,
    pub cluster_id: Option<String>,
}

/// Encode a content header frame payload.
pub fn encode_content_header(class: u16, body_size: u64, p: &BasicProperties) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);
    out.extend_from_slice(&class.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&body_size.to_be_bytes());
    let mut flags: u16 = 0;
    let mut props = Vec::new();
    let ss = |bit: u16, v: &Option<String>, flags: &mut u16, props: &mut Vec<u8>| {
        if let Some(s) = v {
            *flags |= 1 << bit;
            put_short_str(props, s);
        }
    };
    ss(15, &p.content_type, &mut flags, &mut props);
    ss(14, &p.content_encoding, &mut flags, &mut props);
    if let Some(h) = &p.headers {
        flags |= 1 << 13;
        put_table(&mut props, h);
    }
    if let Some(v) = p.delivery_mode {
        flags |= 1 << 12;
        props.push(v);
    }
    if let Some(v) = p.priority {
        flags |= 1 << 11;
        props.push(v);
    }
    ss(10, &p.correlation_id, &mut flags, &mut props);
    ss(9, &p.reply_to, &mut flags, &mut props);
    ss(8, &p.expiration, &mut flags, &mut props);
    ss(7, &p.message_id, &mut flags, &mut props);
    if let Some(v) = p.timestamp {
        flags |= 1 << 6;
        props.extend_from_slice(&v.to_be_bytes());
    }
    ss(5, &p.kind, &mut flags, &mut props);
    ss(4, &p.user_id, &mut flags, &mut props);
    ss(3, &p.app_id, &mut flags, &mut props);
    ss(2, &p.cluster_id, &mut flags, &mut props);
    out.extend_from_slice(&flags.to_be_bytes());
    out.extend_from_slice(&props);
    out
}

/// Decode a content header frame payload into `(class, body_size, properties)`.
pub fn decode_content_header(payload: &[u8]) -> io::Result<(u16, u64, BasicProperties)> {
    let mut r = Rd::new(payload);
    let class = r.u16()?;
    let _weight = r.u16()?;
    let size = r.u64()?;
    let flags = r.u16()?;
    let has = |bit: u16| flags & (1 << bit) != 0;
    let mut p = BasicProperties::default();
    let ss = |bit: u16, r: &mut Rd| -> io::Result<Option<String>> { if has(bit) { Ok(Some(r.short_str()?)) } else { Ok(None) } };
    p.content_type = ss(15, &mut r)?;
    p.content_encoding = ss(14, &mut r)?;
    if has(13) {
        p.headers = Some(r.table()?);
    }
    if has(12) {
        p.delivery_mode = Some(r.u8()?);
    }
    if has(11) {
        p.priority = Some(r.u8()?);
    }
    p.correlation_id = ss(10, &mut r)?;
    p.reply_to = ss(9, &mut r)?;
    p.expiration = ss(8, &mut r)?;
    p.message_id = ss(7, &mut r)?;
    if has(6) {
        p.timestamp = Some(r.u64()?);
    }
    p.kind = ss(5, &mut r)?;
    p.user_id = ss(4, &mut r)?;
    p.app_id = ss(3, &mut r)?;
    p.cluster_id = ss(2, &mut r)?;
    Ok((class, size, p))
}

/// A raw frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub kind: u8,
    pub channel: u16,
    pub payload: Vec<u8>,
}

/// Read one frame. `max` is the negotiated frame-max (0 = unlimited).
pub fn read_frame<R: Read>(r: &mut R, max: u32) -> io::Result<Frame> {
    let mut head = [0u8; 7];
    r.read_exact(&mut head)?;
    let kind = head[0];
    let channel = u16::from_be_bytes([head[1], head[2]]);
    let size = u32::from_be_bytes([head[3], head[4], head[5], head[6]]);
    if max != 0 && size > max.max(FRAME_MIN_SIZE) {
        return Err(invalid(format!("frame of {} bytes exceeds frame-max {}", size, max)));
    }
    let mut payload = vec![0u8; size as usize];
    r.read_exact(&mut payload)?;
    let mut end = [0u8; 1];
    r.read_exact(&mut end)?;
    if end[0] != FRAME_END {
        return Err(invalid("missing frame-end octet"));
    }
    Ok(Frame { kind, channel, payload })
}

/// Serialize a frame into `out`.
pub fn put_frame(out: &mut Vec<u8>, kind: u8, channel: u16, payload: &[u8]) {
    out.push(kind);
    out.extend_from_slice(&channel.to_be_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out.push(FRAME_END);
}

/// Serialize a complete message (method + content header + body frames split at
/// `frame_max`) into one buffer, so it can be written atomically.
pub fn put_content(out: &mut Vec<u8>, channel: u16, method: &Method, props: &BasicProperties, body: &[u8], frame_max: u32) {
    put_frame(out, FRAME_METHOD, channel, &method.encode());
    put_frame(out, FRAME_HEADER, channel, &encode_content_header(method.class, body.len() as u64, props));
    let chunk = if frame_max == 0 { usize::MAX } else { (frame_max as usize).saturating_sub(8).max(1) };
    for part in body.chunks(chunk) {
        put_frame(out, FRAME_BODY, channel, part);
    }
}

/// Write a method frame.
pub fn write_method<W: Write>(w: &mut W, channel: u16, method: &Method) -> io::Result<()> {
    let mut out = Vec::new();
    put_frame(&mut out, FRAME_METHOD, channel, &method.encode());
    w.write_all(&out)?;
    w.flush()
}

/// Write a heartbeat frame.
pub fn write_heartbeat<W: Write>(w: &mut W) -> io::Result<()> {
    w.write_all(&[FRAME_HEARTBEAT, 0, 0, 0, 0, 0, 0, FRAME_END])?;
    w.flush()
}

fn put_short_str(out: &mut Vec<u8>, s: &str) {
    let b = s.as_bytes();
    let n = b.len().min(255);
    out.push(n as u8);
    out.extend_from_slice(&b[..n]);
}

fn put_long_bytes(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(&(b.len() as u32).to_be_bytes());
    out.extend_from_slice(b);
}

/// Encode a field table (with its length prefix).
pub fn put_table(out: &mut Vec<u8>, t: &Table) {
    let mut body = Vec::new();
    for (k, v) in t {
        put_short_str(&mut body, k);
        put_field_value(&mut body, v);
    }
    put_long_bytes(out, &body);
}

fn put_field_value(out: &mut Vec<u8>, v: &Value) {
    match v {
        Value::Null => out.push(b'V'),
        Value::Bool(b) => {
            out.push(b't');
            out.push(*b as u8);
        }
        Value::Int(i) => {
            if let Ok(x) = i32::try_from(*i) {
                out.push(b'I');
                out.extend_from_slice(&x.to_be_bytes());
            } else {
                out.push(b'l');
                out.extend_from_slice(&i.to_be_bytes());
            }
        }
        Value::Float(f) => {
            out.push(b'd');
            out.extend_from_slice(&f.to_be_bytes());
        }
        Value::String(s) => {
            out.push(b'S');
            put_long_bytes(out, s.as_bytes());
        }
        Value::Bytes(b) => {
            out.push(b'x');
            put_long_bytes(out, b);
        }
        Value::Array(items) => {
            out.push(b'A');
            let mut body = Vec::new();
            for i in items {
                put_field_value(&mut body, i);
            }
            put_long_bytes(out, &body);
        }
        Value::Object(map) => {
            out.push(b'F');
            put_table(out, map);
        }
    }
}

/// A cursor over a byte slice.
struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Rd<'a> {
    fn new(b: &'a [u8]) -> Self {
        Rd { b, p: 0 }
    }
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        if self.p + n > self.b.len() {
            return Err(invalid("truncated AMQP frame"));
        }
        let s = &self.b[self.p..self.p + n];
        self.p += n;
        Ok(s)
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> io::Result<u16> {
        let s = self.take(2)?;
        Ok(u16::from_be_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> io::Result<u32> {
        let s = self.take(4)?;
        Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn u64(&mut self) -> io::Result<u64> {
        let s = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(s);
        Ok(u64::from_be_bytes(a))
    }
    fn short_str(&mut self) -> io::Result<String> {
        let n = self.u8()? as usize;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
    fn long_bytes(&mut self) -> io::Result<Vec<u8>> {
        let n = self.u32()? as usize;
        Ok(self.take(n)?.to_vec())
    }
    fn table(&mut self) -> io::Result<Table> {
        let n = self.u32()? as usize;
        let mut sub = Rd::new(self.take(n)?);
        let mut t = Table::new();
        while sub.p < sub.b.len() {
            let k = sub.short_str()?;
            let v = sub.field_value()?;
            t.insert(k, v);
        }
        Ok(t)
    }
    fn field_value(&mut self) -> io::Result<Value> {
        let t = self.u8()?;
        Ok(match t {
            b't' => Value::Bool(self.u8()? != 0),
            b'b' => Value::Int(self.u8()? as i8 as i64),
            b'B' => Value::Int(self.u8()? as i64),
            b'U' | b's' => Value::Int(self.u16()? as i16 as i64),
            b'u' => Value::Int(self.u16()? as i64),
            b'I' => Value::Int(self.u32()? as i32 as i64),
            b'i' => Value::Int(self.u32()? as i64),
            b'L' | b'l' => Value::Int(self.u64()? as i64),
            b'T' => Value::Int(self.u64()? as i64),
            b'f' => Value::Float(f32::from_bits(self.u32()?) as f64),
            b'd' => Value::Float(f64::from_bits(self.u64()?)),
            b'D' => {
                let scale = self.u8()? as i32;
                let v = self.u32()? as i32 as f64;
                Value::Float(v / 10f64.powi(scale))
            }
            b'S' => {
                let b = self.long_bytes()?;
                match String::from_utf8(b) {
                    Ok(s) => Value::String(s),
                    Err(e) => Value::Bytes(e.into_bytes()),
                }
            }
            b'x' => Value::Bytes(self.long_bytes()?),
            b'A' => {
                let n = self.u32()? as usize;
                let mut sub = Rd::new(self.take(n)?);
                let mut v = Vec::new();
                while sub.p < sub.b.len() {
                    v.push(sub.field_value()?);
                }
                Value::Array(v)
            }
            b'F' => Value::Object(self.table()?),
            b'V' => Value::Null,
            other => return Err(invalid(format!("unknown field value type '{}'", other as char))),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_round_trip_with_bits() {
        let mut args = Table::new();
        args.insert("x-dead-letter-exchange".into(), Value::from("dlx"));
        args.insert("x-max".into(), Value::Int(5_000_000_000));
        args.insert("nested".into(), Value::object([("a", Value::Bool(true)), ("b", Value::Array(vec![Value::Int(1), Value::Null]))]));
        let m = Method::new(
            m::QUEUE_DECLARE,
            vec![
                Arg::Short(0),
                Arg::ShortStr("orders".into()),
                Arg::Bit(false),
                Arg::Bit(true),
                Arg::Bit(false),
                Arg::Bit(true),
                Arg::Bit(false),
                Arg::Table(args.clone()),
            ],
        );
        let enc = m.encode();
        let d = Method::decode(&enc).unwrap();
        assert_eq!(d, m);
        assert_eq!(d.str(1), "orders");
        assert!(d.bit(3) && d.bit(5) && !d.bit(2));
        assert_eq!(d.table(7), args);
        // nine bits span two octets
        let ack = Method::new(m::BASIC_NACK, vec![Arg::LongLong(7), Arg::Bit(true), Arg::Bit(true)]);
        assert_eq!(Method::decode(&ack.encode()).unwrap(), ack);
    }

    #[test]
    fn content_header_round_trip() {
        let mut h = Table::new();
        h.insert("k".into(), Value::from("v"));
        h.insert("f".into(), Value::Float(1.5));
        h.insert("bin".into(), Value::Bytes(vec![0, 1, 255]));
        let p = BasicProperties {
            content_type: Some("text/plain".into()),
            headers: Some(h),
            delivery_mode: Some(2),
            priority: Some(5),
            correlation_id: Some("c1".into()),
            reply_to: Some("amq.gen-1".into()),
            expiration: Some("1000".into()),
            message_id: Some("m1".into()),
            timestamp: Some(1_700_000_000),
            kind: Some("order".into()),
            user_id: Some("guest".into()),
            app_id: Some("app".into()),
            cluster_id: Some("cl".into()),
            content_encoding: Some("utf-8".into()),
        };
        let enc = encode_content_header(60, 42, &p);
        assert_eq!(decode_content_header(&enc).unwrap(), (60, 42, p));
        let empty = encode_content_header(60, 0, &BasicProperties::default());
        assert_eq!(decode_content_header(&empty).unwrap().2, BasicProperties::default());
    }

    #[test]
    fn frames_split_body() {
        let mut out = Vec::new();
        let method = Method::new(m::BASIC_PUBLISH, vec![Arg::Short(0), Arg::ShortStr("ex".into()), Arg::ShortStr("rk".into()), Arg::Bit(false), Arg::Bit(false)]);
        let body = vec![7u8; 10_000];
        put_content(&mut out, 3, &method, &BasicProperties::default(), &body, 4096);
        let mut r = &out[..];
        let f = read_frame(&mut r, 4096).unwrap();
        assert_eq!((f.kind, f.channel), (FRAME_METHOD, 3));
        assert_eq!(read_frame(&mut r, 4096).unwrap().kind, FRAME_HEADER);
        let mut total = 0;
        let mut frames = 0;
        while !r.is_empty() {
            let f = read_frame(&mut r, 4096).unwrap();
            assert_eq!(f.kind, FRAME_BODY);
            total += f.payload.len();
            frames += 1;
        }
        assert_eq!((total, frames), (10_000, 3));
    }
}

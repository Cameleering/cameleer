//! PostgreSQL frontend/backend protocol v3 message framing and value encoding.

use std::io::{self, Read, Write};

use cameleer_core::value::Value;

use crate::datasource::ColumnType;
use crate::engine::eval::text_of;

/// Protocol version 3.0.
pub const PROTOCOL_V3: i32 = 196608;
/// SSLRequest code.
pub const SSL_REQUEST: i32 = 80877103;
/// CancelRequest code.
pub const CANCEL_REQUEST: i32 = 80877102;
/// GSSENCRequest code.
pub const GSSENC_REQUEST: i32 = 80877104;

/// Type OIDs.
pub mod oid {
    pub const BOOL: i32 = 16;
    pub const BYTEA: i32 = 17;
    pub const CHAR: i32 = 18;
    pub const NAME: i32 = 19;
    pub const INT8: i32 = 20;
    pub const INT2: i32 = 21;
    pub const INT4: i32 = 23;
    pub const TEXT: i32 = 25;
    pub const OID: i32 = 26;
    pub const JSON: i32 = 114;
    pub const FLOAT4: i32 = 700;
    pub const FLOAT8: i32 = 701;
    pub const UNKNOWN: i32 = 705;
    pub const BPCHAR: i32 = 1042;
    pub const VARCHAR: i32 = 1043;
    pub const DATE: i32 = 1082;
    pub const TIME: i32 = 1083;
    pub const TIMESTAMP: i32 = 1114;
    pub const TIMESTAMPTZ: i32 = 1184;
    pub const NUMERIC: i32 = 1700;
    pub const UUID: i32 = 2950;
    pub const JSONB: i32 = 3802;
}

/// Build a tagged message.
pub fn message(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 5);
    out.push(tag);
    out.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
    out.extend_from_slice(body);
    out
}

/// Build an untagged (startup-style) message.
pub fn untagged(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 4);
    out.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
    out.extend_from_slice(body);
    out
}

pub fn put_cstr(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(s.as_bytes());
    buf.push(0);
}
pub fn put_i16(buf: &mut Vec<u8>, v: i16) {
    buf.extend_from_slice(&v.to_be_bytes());
}
pub fn put_i32(buf: &mut Vec<u8>, v: i32) {
    buf.extend_from_slice(&v.to_be_bytes());
}

const MAX_MESSAGE: usize = 256 * 1024 * 1024;

/// Read one tagged message: (tag, body).
pub fn read_message(r: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    let mut head = [0u8; 5];
    r.read_exact(&mut head)?;
    let len = i32::from_be_bytes([head[1], head[2], head[3], head[4]]);
    if len < 4 || len as usize > MAX_MESSAGE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("invalid message length {}", len)));
    }
    let mut body = vec![0u8; len as usize - 4];
    r.read_exact(&mut body)?;
    Ok((head[0], body))
}

/// Read one untagged (startup) message body.
pub fn read_untagged(r: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut head = [0u8; 4];
    r.read_exact(&mut head)?;
    let len = i32::from_be_bytes(head);
    if !(8..=10_000).contains(&len) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("invalid startup packet length {}", len)));
    }
    let mut body = vec![0u8; len as usize - 4];
    r.read_exact(&mut body)?;
    Ok(body)
}

/// Write bytes and flush.
pub fn send(w: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    w.write_all(bytes)?;
    w.flush()
}

/// A cursor over a message body.
pub struct Buf<'a> {
    data: &'a [u8],
    pos: usize,
}

fn short() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "message too short")
}

impl<'a> Buf<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Buf { data, pos: 0 }
    }
    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }
    pub fn u8(&mut self) -> io::Result<u8> {
        let b = *self.data.get(self.pos).ok_or_else(short)?;
        self.pos += 1;
        Ok(b)
    }
    pub fn i16(&mut self) -> io::Result<i16> {
        let b = self.bytes(2)?;
        Ok(i16::from_be_bytes([b[0], b[1]]))
    }
    pub fn i32(&mut self) -> io::Result<i32> {
        let b = self.bytes(4)?;
        Ok(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    pub fn bytes(&mut self, n: usize) -> io::Result<&'a [u8]> {
        if self.pos + n > self.data.len() {
            return Err(short());
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    pub fn rest(&mut self) -> &'a [u8] {
        let s = &self.data[self.pos..];
        self.pos = self.data.len();
        s
    }
    pub fn cstr(&mut self) -> io::Result<String> {
        let start = self.pos;
        while self.pos < self.data.len() && self.data[self.pos] != 0 {
            self.pos += 1;
        }
        if self.pos >= self.data.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "unterminated string"));
        }
        let s = String::from_utf8_lossy(&self.data[start..self.pos]).into_owned();
        self.pos += 1;
        Ok(s)
    }
}

/// Parse ErrorResponse / NoticeResponse fields into (code, message, detail).
pub fn parse_error_fields(body: &[u8]) -> (String, String, Option<String>) {
    let mut b = Buf::new(body);
    let (mut code, mut msg, mut detail) = ("XX000".to_string(), String::new(), None);
    while let Ok(t) = b.u8() {
        if t == 0 {
            break;
        }
        let Ok(v) = b.cstr() else { break };
        match t {
            b'C' => code = v,
            b'M' => msg = v,
            b'D' => detail = Some(v),
            _ => {}
        }
    }
    (code, msg, detail)
}

/// Build an ErrorResponse body.
pub fn error_body(severity: &str, code: &str, message: &str) -> Vec<u8> {
    let mut b = Vec::new();
    for (t, v) in [(b'S', severity), (b'V', severity), (b'C', code), (b'M', message)] {
        b.push(t);
        put_cstr(&mut b, v);
    }
    b.push(0);
    b
}

/// Map a column type to a type OID (server side).
pub fn oid_of(t: ColumnType) -> i32 {
    match t {
        ColumnType::Integer => oid::INT4,
        ColumnType::BigInt => oid::INT8,
        ColumnType::Text => oid::TEXT,
        ColumnType::Boolean => oid::BOOL,
        ColumnType::Double => oid::FLOAT8,
        ColumnType::Decimal => oid::NUMERIC,
        ColumnType::Timestamp => oid::TIMESTAMP,
        ColumnType::Bytes => oid::BYTEA,
        ColumnType::Unknown => oid::TEXT,
    }
}

/// Map a type OID to a column type (client side).
pub fn column_type_of(o: i32) -> ColumnType {
    match o {
        oid::INT2 | oid::INT4 | oid::OID => ColumnType::Integer,
        oid::INT8 => ColumnType::BigInt,
        oid::BOOL => ColumnType::Boolean,
        oid::FLOAT4 | oid::FLOAT8 => ColumnType::Double,
        oid::NUMERIC => ColumnType::Decimal,
        oid::TIMESTAMP | oid::TIMESTAMPTZ | oid::DATE | oid::TIME => ColumnType::Timestamp,
        oid::BYTEA => ColumnType::Bytes,
        _ => ColumnType::Text,
    }
}

/// Encode a value in text format (`None` = SQL NULL).
pub fn encode_text(v: &Value) -> Option<Vec<u8>> {
    match v {
        Value::Null => None,
        Value::Bool(b) => Some(if *b { b"t".to_vec() } else { b"f".to_vec() }),
        Value::Bytes(b) => {
            let mut s = String::from("\\x");
            for x in b {
                s.push_str(&format!("{:02x}", x));
            }
            Some(s.into_bytes())
        }
        other => Some(text_of(other).into_bytes()),
    }
}

/// Type OID to declare for a parameter value (0 = let the server infer).
pub fn param_oid(v: &Value) -> i32 {
    match v {
        Value::Int(_) => oid::INT8,
        Value::Float(_) => oid::FLOAT8,
        Value::Bool(_) => oid::BOOL,
        Value::Bytes(_) => oid::BYTEA,
        _ => 0,
    }
}

/// Decode a text-format value by type OID.
pub fn decode_text(o: i32, raw: &[u8]) -> Value {
    let s = String::from_utf8_lossy(raw);
    match o {
        oid::INT2 | oid::INT4 | oid::INT8 | oid::OID => s.trim().parse::<i64>().map(Value::Int).unwrap_or_else(|_| Value::String(s.into_owned())),
        oid::FLOAT4 | oid::FLOAT8 | oid::NUMERIC => match s.trim() {
            "NaN" => Value::Float(f64::NAN),
            "Infinity" => Value::Float(f64::INFINITY),
            "-Infinity" => Value::Float(f64::NEG_INFINITY),
            t => t.parse::<f64>().map(Value::Float).unwrap_or_else(|_| Value::String(s.into_owned())),
        },
        oid::BOOL => Value::Bool(matches!(s.trim(), "t" | "true" | "1")),
        oid::BYTEA => match s.strip_prefix("\\x") {
            Some(h) if h.len() % 2 == 0 => (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).ok()).collect::<Option<Vec<u8>>>().map(Value::Bytes).unwrap_or_else(|| Value::Bytes(raw.to_vec())),
            _ => Value::Bytes(raw.to_vec()),
        },
        _ => Value::String(s.into_owned()),
    }
}

/// Decode a parameter sent by a client with a declared OID (0 = keep as text).
pub fn decode_param(o: i32, raw: &[u8]) -> Value {
    match o {
        0 | oid::TEXT | oid::VARCHAR | oid::UNKNOWN | oid::BPCHAR => Value::String(String::from_utf8_lossy(raw).into_owned()),
        other => decode_text(other, raw),
    }
}

/// Affected-row count of a CommandComplete tag (`INSERT 0 3` → 3).
pub fn tag_count(tag: &str) -> i64 {
    tag.rsplit(' ').next().and_then(|n| n.parse().ok()).unwrap_or(0)
}

/// Rewrite `?` placeholders (outside quotes/comments) to `$1..$n`.
pub fn question_to_dollar(sql: &str) -> String {
    let chars: Vec<char> = sql.chars().collect();
    let mut out = String::with_capacity(sql.len() + 8);
    let mut n = 0;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\'' | '"' => {
                out.push(c);
                i += 1;
                while i < chars.len() {
                    out.push(chars[i]);
                    if chars[i] == c {
                        if chars.get(i + 1) == Some(&c) {
                            out.push(c);
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            '-' if chars.get(i + 1) == Some(&'-') => {
                while i < chars.len() && chars[i] != '\n' {
                    out.push(chars[i]);
                    i += 1;
                }
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                out.push_str("/*");
                i += 2;
                while i < chars.len() {
                    if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                        out.push_str("*/");
                        i += 2;
                        break;
                    }
                    out.push(chars[i]);
                    i += 1;
                }
            }
            '?' => {
                n += 1;
                out.push_str(&format!("${}", n));
                i += 1;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_params() {
        assert_eq!(question_to_dollar("select ? , '?', \"a?\" from t where x = ? -- ?\n and y = ?"), "select $1 , '?', \"a?\" from t where x = $2 -- ?\n and y = $3");
        assert_eq!(tag_count("INSERT 0 3"), 3);
        assert_eq!(tag_count("UPDATE 2"), 2);
        assert_eq!(tag_count("CREATE TABLE"), 0);
    }

    #[test]
    fn values() {
        assert_eq!(decode_text(oid::INT4, b"42"), Value::Int(42));
        assert_eq!(decode_text(oid::BOOL, b"t"), Value::Bool(true));
        assert_eq!(decode_text(oid::NUMERIC, b"1.50"), Value::Float(1.5));
        assert_eq!(decode_text(oid::BYTEA, b"\\x6869"), Value::Bytes(b"hi".to_vec()));
        assert_eq!(encode_text(&Value::Bytes(b"hi".to_vec())), Some(b"\\x6869".to_vec()));
        assert_eq!(encode_text(&Value::Null), None);
        let mut b = Vec::new();
        put_cstr(&mut b, "abc");
        put_i32(&mut b, 7);
        let mut r = Buf::new(&b);
        assert_eq!(r.cstr().unwrap(), "abc");
        assert_eq!(r.i32().unwrap(), 7);
        assert_eq!(r.remaining(), 0);
    }
}

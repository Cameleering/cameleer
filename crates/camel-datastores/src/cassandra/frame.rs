//! CQL native protocol v4: frames, notation types (`[int]`, `[string]`, `[bytes]`,
//! `[string map]`, `[option]`...), column types and value codecs, consistency levels and
//! query parameters.

use std::io::{self, Read};

use camel_core::error::{CamelError, Result};
use camel_core::value::{ObjectMap, Value};

use crate::mongodb::bson::{format_iso_date, parse_iso_date};
use crate::util::{hex, unhex};

/// Protocol version 4.
pub const VERSION: u8 = 4;
/// Direction bit of response frames.
pub const RESPONSE: u8 = 0x80;
/// Maximum frame body length (256 MB, the protocol limit).
pub const MAX_BODY: usize = 256 * 1024 * 1024;

/// Opcodes.
pub mod opcode {
    pub const ERROR: u8 = 0x00;
    pub const STARTUP: u8 = 0x01;
    pub const READY: u8 = 0x02;
    pub const AUTHENTICATE: u8 = 0x03;
    pub const OPTIONS: u8 = 0x05;
    pub const SUPPORTED: u8 = 0x06;
    pub const QUERY: u8 = 0x07;
    pub const RESULT: u8 = 0x08;
    pub const PREPARE: u8 = 0x09;
    pub const EXECUTE: u8 = 0x0A;
    pub const REGISTER: u8 = 0x0B;
    pub const EVENT: u8 = 0x0C;
    pub const BATCH: u8 = 0x0D;
    pub const AUTH_CHALLENGE: u8 = 0x0E;
    pub const AUTH_RESPONSE: u8 = 0x0F;
    pub const AUTH_SUCCESS: u8 = 0x10;
}

/// Error codes.
pub mod error_code {
    pub const SERVER_ERROR: i32 = 0x0000;
    pub const PROTOCOL_ERROR: i32 = 0x000A;
    pub const BAD_CREDENTIALS: i32 = 0x0100;
    pub const UNAVAILABLE: i32 = 0x1000;
    pub const SYNTAX_ERROR: i32 = 0x2000;
    pub const UNAUTHORIZED: i32 = 0x2100;
    pub const INVALID: i32 = 0x2200;
    pub const CONFIG_ERROR: i32 = 0x2300;
    pub const ALREADY_EXISTS: i32 = 0x2400;
    pub const UNPREPARED: i32 = 0x2500;
}

/// Result kinds.
pub mod result_kind {
    pub const VOID: i32 = 1;
    pub const ROWS: i32 = 2;
    pub const SET_KEYSPACE: i32 = 3;
    pub const PREPARED: i32 = 4;
    pub const SCHEMA_CHANGE: i32 = 5;
}

/// A frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub version: u8,
    pub flags: u8,
    pub stream: i16,
    pub opcode: u8,
    pub body: Vec<u8>,
}

impl Frame {
    pub fn request(stream: i16, opcode: u8, body: Vec<u8>) -> Frame {
        Frame { version: VERSION, flags: 0, stream, opcode, body }
    }

    pub fn response(stream: i16, opcode: u8, body: Vec<u8>) -> Frame {
        Frame { version: VERSION | RESPONSE, flags: 0, stream, opcode, body }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(9 + self.body.len());
        out.push(self.version);
        out.push(self.flags);
        out.extend_from_slice(&self.stream.to_be_bytes());
        out.push(self.opcode);
        out.extend_from_slice(&(self.body.len() as u32).to_be_bytes());
        out.extend_from_slice(&self.body);
        out
    }
}

/// Frame flag: the body is compressed.
pub const FLAG_COMPRESSION: u8 = 0x01;

/// Frame body compression negotiated with `STARTUP {COMPRESSION}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    Lz4,
    Snappy,
}

impl Compression {
    pub fn parse(s: &str) -> Option<Compression> {
        match s.trim().to_ascii_lowercase().as_str() {
            "lz4" => Some(Compression::Lz4),
            "snappy" => Some(Compression::Snappy),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Compression::Lz4 => "lz4",
            Compression::Snappy => "snappy",
        }
    }

    /// Compresses a frame body (LZ4: 4-byte big-endian uncompressed length + block).
    pub fn compress(self, body: &[u8]) -> Vec<u8> {
        match self {
            Compression::Lz4 => {
                let mut out = (body.len() as u32).to_be_bytes().to_vec();
                out.extend(crate::compress::lz4_compress(body));
                out
            }
            Compression::Snappy => crate::compress::snappy_compress(body),
        }
    }

    /// Decompresses a frame body.
    pub fn decompress(self, body: &[u8]) -> Result<Vec<u8>> {
        match self {
            Compression::Lz4 => {
                if body.len() < 4 {
                    return Err(proto_err("invalid lz4 frame body"));
                }
                let n = u32::from_be_bytes([body[0], body[1], body[2], body[3]]) as usize;
                if n > MAX_BODY {
                    return Err(proto_err("lz4 frame body too large"));
                }
                let out = crate::compress::lz4_decompress(&body[4..], n)?;
                if out.len() != n {
                    return Err(proto_err("lz4 frame body length mismatch"));
                }
                Ok(out)
            }
            Compression::Snappy => crate::compress::snappy_decompress(body, MAX_BODY),
        }
    }
}

impl Frame {
    /// Compresses the body (sets the compression flag).
    pub fn compressed(mut self, c: Option<Compression>) -> Frame {
        if let Some(c) = c {
            self.body = c.compress(&self.body);
            self.flags |= FLAG_COMPRESSION;
        }
        self
    }

    /// Decompresses a flagged body.
    pub fn decompressed(mut self, c: Option<Compression>) -> Result<Frame> {
        if self.flags & FLAG_COMPRESSION != 0 {
            let c = c.ok_or_else(|| proto_err("compressed frame without negotiated compression"))?;
            self.body = c.decompress(&self.body)?;
            self.flags &= !FLAG_COMPRESSION;
        }
        Ok(self)
    }
}

/// Writes a signed vint (zig-zag + Cassandra's unsigned vint: leading one bits of the first
/// byte count the extra bytes).
pub fn write_vint(out: &mut Vec<u8>, v: i64) {
    let u = ((v << 1) ^ (v >> 63)) as u64;
    for n in 0..9usize {
        if n == 8 || u < 1u64 << (7 * (n + 1)) {
            if n == 8 {
                out.push(0xFF);
                out.extend_from_slice(&u.to_be_bytes());
            } else {
                let first = ((0xFFu16 << (8 - n)) as u8) | (u >> (8 * n)) as u8;
                out.push(first);
                for k in (0..n).rev() {
                    out.push((u >> (8 * k)) as u8);
                }
            }
            return;
        }
    }
}

/// Reads a signed vint.
pub fn read_vint(b: &[u8], p: &mut usize) -> Option<i64> {
    let first = *b.get(*p)?;
    *p += 1;
    let n = first.leading_ones() as usize;
    let mut u: u64 = if n >= 8 { 0 } else { (first & (0xFF >> n)) as u64 };
    for _ in 0..n {
        u = (u << 8) | *b.get(*p)? as u64;
        *p += 1;
    }
    Some(((u >> 1) as i64) ^ -((u & 1) as i64))
}

const NANOS_PER: [(i64, &str); 6] = [(3_600_000_000_000, "h"), (60_000_000_000, "m"), (1_000_000_000, "s"), (1_000_000, "ms"), (1_000, "us"), (1, "ns")];

/// Cassandra's duration text (`1y2mo3d4h5m6s7ms8us9ns`).
pub fn format_duration(months: i32, days: i32, nanos: i64) -> String {
    if months == 0 && days == 0 && nanos == 0 {
        return "0s".into();
    }
    let neg = months < 0 || days < 0 || nanos < 0;
    let (mut m, d, mut n) = (months.unsigned_abs() as i64, days.unsigned_abs() as i64, nanos.unsigned_abs() as i64);
    let mut s = String::from(if neg { "-" } else { "" });
    if m >= 12 {
        s.push_str(&format!("{}y", m / 12));
        m %= 12;
    }
    if m > 0 {
        s.push_str(&format!("{}mo", m));
    }
    if d > 0 {
        s.push_str(&format!("{}d", d));
    }
    for (per, unit) in NANOS_PER {
        if n >= per {
            s.push_str(&format!("{}{}", n / per, unit));
            n %= per;
        }
    }
    s
}

/// Parses a duration: Cassandra text (`1y2mo3w4d5h6m7s8ms9us10ns`), ISO 8601 (`P1Y2M3DT4H`,
/// `P2W`) or an object `{months, days, nanoseconds}`.
pub fn parse_duration_value(v: &Value) -> Option<(i32, i32, i64)> {
    if let Value::Object(o) = v {
        let g = |k: &str| o.get(k).and_then(|x| x.as_i64()).unwrap_or(0);
        return Some((g("months") as i32, g("days") as i32, g("nanoseconds").max(g("nanos"))));
    }
    let text = v.to_string();
    let t = text.trim();
    let (neg, t) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t),
    };
    let (mut months, mut days, mut nanos) = (0i64, 0i64, 0i64);
    let upper = t.to_ascii_uppercase();
    if let Some(iso) = upper.strip_prefix('P') {
        let (date, time) = iso.split_once('T').unwrap_or((iso, ""));
        let mut num = String::new();
        for c in date.chars() {
            if c.is_ascii_digit() {
                num.push(c);
                continue;
            }
            let n: i64 = num.parse().ok()?;
            num.clear();
            match c {
                'Y' => months += n * 12,
                'M' => months += n,
                'W' => days += n * 7,
                'D' => days += n,
                _ => return None,
            }
        }
        for c in time.chars() {
            if c.is_ascii_digit() {
                num.push(c);
                continue;
            }
            let n: i64 = num.parse().ok()?;
            num.clear();
            match c {
                'H' => nanos += n * 3_600_000_000_000,
                'M' => nanos += n * 60_000_000_000,
                'S' => nanos += n * 1_000_000_000,
                _ => return None,
            }
        }
        if !num.is_empty() {
            return None;
        }
    } else {
        let lower = t.to_ascii_lowercase();
        let b = lower.as_bytes();
        let mut i = 0;
        if b.is_empty() {
            return None;
        }
        while i < b.len() {
            let start = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            let n: i64 = lower[start..i].parse().ok()?;
            let ustart = i;
            while i < b.len() && b[i].is_ascii_alphabetic() || (i < b.len() && b[i] == 0xC2) {
                i += 1;
            }
            match &lower[ustart..i] {
                "y" => months += n * 12,
                "mo" => months += n,
                "w" => days += n * 7,
                "d" => days += n,
                "h" => nanos += n * 3_600_000_000_000,
                "m" => nanos += n * 60_000_000_000,
                "s" => nanos += n * 1_000_000_000,
                "ms" => nanos += n * 1_000_000,
                "us" | "µs" => nanos += n * 1_000,
                "ns" => nanos += n,
                _ => return None,
            }
        }
    }
    let sign = if neg { -1 } else { 1 };
    Some(((months * sign) as i32, (days * sign) as i32, nanos * sign))
}

/// Cassandra's Murmur3Partitioner token of a serialized partition key
/// (`MurmurHash.hash3_x64_128` with signed tail bytes, first half; `Long.MIN_VALUE` → `MAX`).
pub fn murmur3_token(key: &[u8]) -> i64 {
    fn fmix(mut k: u64) -> u64 {
        k ^= k >> 33;
        k = k.wrapping_mul(0xff51afd7ed558ccd);
        k ^= k >> 33;
        k = k.wrapping_mul(0xc4ceb9fe1a85ec53);
        k ^ (k >> 33)
    }
    let (c1, c2) = (0x87c37b91114253d5u64, 0x4cf5ad432745937fu64);
    let (mut h1, mut h2) = (0u64, 0u64);
    let nblocks = key.len() / 16;
    for i in 0..nblocks {
        let b = &key[i * 16..i * 16 + 16];
        let mut k1 = u64::from_le_bytes(b[..8].try_into().unwrap());
        let mut k2 = u64::from_le_bytes(b[8..].try_into().unwrap());
        k1 = k1.wrapping_mul(c1).rotate_left(31).wrapping_mul(c2);
        h1 ^= k1;
        h1 = h1.rotate_left(27).wrapping_add(h2).wrapping_mul(5).wrapping_add(0x52dce729);
        k2 = k2.wrapping_mul(c2).rotate_left(33).wrapping_mul(c1);
        h2 ^= k2;
        h2 = h2.rotate_left(31).wrapping_add(h1).wrapping_mul(5).wrapping_add(0x38495ab5);
    }
    let tail = &key[nblocks * 16..];
    let (mut k1, mut k2) = (0u64, 0u64);
    // Cassandra sign-extends the tail bytes (`(long) key.get(i)`)
    let sb = |i: usize| tail[i] as i8 as i64 as u64;
    for i in (8..tail.len()).rev() {
        k2 ^= sb(i) << ((i - 8) * 8);
    }
    if tail.len() > 8 {
        k2 = k2.wrapping_mul(c2).rotate_left(33).wrapping_mul(c1);
        h2 ^= k2;
    }
    for i in (0..tail.len().min(8)).rev() {
        k1 ^= sb(i) << (i * 8);
    }
    if !tail.is_empty() {
        k1 = k1.wrapping_mul(c1).rotate_left(31).wrapping_mul(c2);
        h1 ^= k1;
    }
    h1 ^= key.len() as u64;
    h2 ^= key.len() as u64;
    h1 = h1.wrapping_add(h2);
    h2 = h2.wrapping_add(h1);
    h1 = fmix(h1);
    h2 = fmix(h2);
    h1 = h1.wrapping_add(h2);
    let t = h1 as i64;
    if t == i64::MIN {
        i64::MAX
    } else {
        t
    }
}

/// The routing key of encoded partition key components (composite keys:
/// `[short len][bytes][0]` per component).
pub fn routing_key(components: &[&[u8]]) -> Vec<u8> {
    if components.len() == 1 {
        return components[0].to_vec();
    }
    let mut out = Vec::new();
    for c in components {
        out.extend_from_slice(&(c.len() as u16).to_be_bytes());
        out.extend_from_slice(c);
        out.push(0);
    }
    out
}

/// Reads a frame.
pub fn read_frame<R: Read>(r: &mut R) -> io::Result<Frame> {
    let mut h = [0u8; 9];
    r.read_exact(&mut h)?;
    let len = u32::from_be_bytes([h[5], h[6], h[7], h[8]]) as usize;
    if len > MAX_BODY {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    Ok(Frame { version: h[0], flags: h[1], stream: i16::from_be_bytes([h[2], h[3]]), opcode: h[4], body })
}

fn proto_err(m: impl Into<String>) -> CamelError {
    CamelError::new(crate::kinds::CASSANDRA_DRIVER, format!("CQL protocol error: {}", m.into()))
}

/// Body writer.
#[derive(Default)]
pub struct Writer(pub Vec<u8>);

impl Writer {
    pub fn new() -> Writer {
        Writer(Vec::new())
    }
    pub fn byte(&mut self, b: u8) -> &mut Self {
        self.0.push(b);
        self
    }
    pub fn short(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn int(&mut self, v: i32) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn long(&mut self, v: i64) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn string(&mut self, s: &str) -> &mut Self {
        self.short(s.len() as u16);
        self.0.extend_from_slice(s.as_bytes());
        self
    }
    pub fn long_string(&mut self, s: &str) -> &mut Self {
        self.int(s.len() as i32);
        self.0.extend_from_slice(s.as_bytes());
        self
    }
    pub fn bytes(&mut self, b: Option<&[u8]>) -> &mut Self {
        match b {
            Some(b) => {
                self.int(b.len() as i32);
                self.0.extend_from_slice(b);
            }
            None => {
                self.int(-1);
            }
        }
        self
    }
    pub fn short_bytes(&mut self, b: &[u8]) -> &mut Self {
        self.short(b.len() as u16);
        self.0.extend_from_slice(b);
        self
    }
    pub fn string_map(&mut self, m: &[(&str, &str)]) -> &mut Self {
        self.short(m.len() as u16);
        for (k, v) in m {
            self.string(k);
            self.string(v);
        }
        self
    }
    pub fn string_list(&mut self, l: &[&str]) -> &mut Self {
        self.short(l.len() as u16);
        for s in l {
            self.string(s);
        }
        self
    }
    pub fn cql_type(&mut self, t: &CqlType) -> &mut Self {
        write_type(t, self);
        self
    }
}

/// Body reader.
pub struct Reader<'a> {
    b: &'a [u8],
    pub pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(b: &'a [u8]) -> Reader<'a> {
        Reader { b, pos: 0 }
    }
    pub fn remaining(&self) -> usize {
        self.b.len() - self.pos
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.b.len() {
            return Err(proto_err("unexpected end of frame body"));
        }
        let s = &self.b[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    pub fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn short(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn int(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn long(&mut self) -> Result<i64> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub fn string(&mut self) -> Result<String> {
        let n = self.short()? as usize;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
    pub fn long_string(&mut self) -> Result<String> {
        let n = self.int()?;
        if n < 0 {
            return Err(proto_err("negative long string length"));
        }
        Ok(String::from_utf8_lossy(self.take(n as usize)?).into_owned())
    }
    pub fn bytes(&mut self) -> Result<Option<Vec<u8>>> {
        let n = self.int()?;
        if n < 0 {
            return Ok(None);
        }
        Ok(Some(self.take(n as usize)?.to_vec()))
    }
    pub fn short_bytes(&mut self) -> Result<Vec<u8>> {
        let n = self.short()? as usize;
        Ok(self.take(n)?.to_vec())
    }
    pub fn string_map(&mut self) -> Result<Vec<(String, String)>> {
        let n = self.short()?;
        (0..n).map(|_| Ok((self.string()?, self.string()?))).collect()
    }
    pub fn string_list(&mut self) -> Result<Vec<String>> {
        let n = self.short()?;
        (0..n).map(|_| self.string()).collect()
    }
    pub fn string_multimap(&mut self) -> Result<Vec<(String, Vec<String>)>> {
        let n = self.short()?;
        (0..n).map(|_| Ok((self.string()?, self.string_list()?))).collect()
    }
    pub fn cql_type(&mut self) -> Result<CqlType> {
        read_type(self)
    }
}

/// Consistency levels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Consistency {
    Any = 0,
    One = 1,
    Two = 2,
    Three = 3,
    Quorum = 4,
    All = 5,
    LocalQuorum = 6,
    EachQuorum = 7,
    Serial = 8,
    LocalSerial = 9,
    LocalOne = 10,
}

impl Consistency {
    pub fn parse(s: &str) -> Result<Consistency> {
        Ok(match s.to_ascii_uppercase().as_str() {
            "ANY" => Consistency::Any,
            "ONE" => Consistency::One,
            "TWO" => Consistency::Two,
            "THREE" => Consistency::Three,
            "QUORUM" => Consistency::Quorum,
            "ALL" => Consistency::All,
            "LOCAL_QUORUM" => Consistency::LocalQuorum,
            "EACH_QUORUM" => Consistency::EachQuorum,
            "SERIAL" => Consistency::Serial,
            "LOCAL_SERIAL" => Consistency::LocalSerial,
            "LOCAL_ONE" => Consistency::LocalOne,
            other => return Err(CamelError::illegal_argument(format!("Unknown consistency level: {}", other))),
        })
    }
    pub fn from_code(c: u16) -> Option<Consistency> {
        use Consistency::*;
        [Any, One, Two, Three, Quorum, All, LocalQuorum, EachQuorum, Serial, LocalSerial, LocalOne].into_iter().find(|x| *x as u16 == c)
    }
    pub fn name(self) -> &'static str {
        match self {
            Consistency::Any => "ANY",
            Consistency::One => "ONE",
            Consistency::Two => "TWO",
            Consistency::Three => "THREE",
            Consistency::Quorum => "QUORUM",
            Consistency::All => "ALL",
            Consistency::LocalQuorum => "LOCAL_QUORUM",
            Consistency::EachQuorum => "EACH_QUORUM",
            Consistency::Serial => "SERIAL",
            Consistency::LocalSerial => "LOCAL_SERIAL",
            Consistency::LocalOne => "LOCAL_ONE",
        }
    }
}

/// CQL column types.
#[derive(Clone, Debug, PartialEq)]
pub enum CqlType {
    Custom(String),
    Ascii,
    Bigint,
    Blob,
    Boolean,
    Counter,
    Decimal,
    Double,
    Float,
    Int,
    Timestamp,
    Uuid,
    Varchar,
    Varint,
    Timeuuid,
    Inet,
    Date,
    Time,
    Smallint,
    Tinyint,
    Duration,
    List(Box<CqlType>),
    Map(Box<CqlType>, Box<CqlType>),
    Set(Box<CqlType>),
    Udt { keyspace: String, name: String, fields: Vec<(String, CqlType)> },
    Tuple(Vec<CqlType>),
}

impl CqlType {
    /// Parses a CQL type name (`int`, `text`, `list<int>`, `map<text, int>`, `frozen<...>`).
    pub fn parse(s: &str) -> Option<CqlType> {
        let s = s.trim();
        let lower = s.to_ascii_lowercase();
        if let Some(inner) = lower.strip_suffix('>') {
            let (outer, args) = inner.split_once('<')?;
            let parts = split_type_args(args);
            return Some(match outer.trim() {
                "list" => CqlType::List(Box::new(CqlType::parse(parts.first()?)?)),
                "set" => CqlType::Set(Box::new(CqlType::parse(parts.first()?)?)),
                "map" => CqlType::Map(Box::new(CqlType::parse(parts.first()?)?), Box::new(CqlType::parse(parts.get(1)?)?)),
                "frozen" => CqlType::parse(parts.first()?)?,
                "tuple" => CqlType::Tuple(parts.iter().map(|p| CqlType::parse(p)).collect::<Option<_>>()?),
                _ => return None,
            });
        }
        Some(match lower.as_str() {
            "ascii" => CqlType::Ascii,
            "bigint" => CqlType::Bigint,
            "blob" => CqlType::Blob,
            "boolean" => CqlType::Boolean,
            "counter" => CqlType::Counter,
            "decimal" => CqlType::Decimal,
            "double" => CqlType::Double,
            "float" => CqlType::Float,
            "int" => CqlType::Int,
            "timestamp" => CqlType::Timestamp,
            "uuid" => CqlType::Uuid,
            "text" | "varchar" => CqlType::Varchar,
            "varint" => CqlType::Varint,
            "timeuuid" => CqlType::Timeuuid,
            "inet" => CqlType::Inet,
            "date" => CqlType::Date,
            "time" => CqlType::Time,
            "smallint" => CqlType::Smallint,
            "tinyint" => CqlType::Tinyint,
            "duration" => CqlType::Duration,
            _ => return None,
        })
    }

    /// The CQL name.
    pub fn name(&self) -> String {
        match self {
            CqlType::Custom(c) => format!("'{}'", c),
            CqlType::Ascii => "ascii".into(),
            CqlType::Bigint => "bigint".into(),
            CqlType::Blob => "blob".into(),
            CqlType::Boolean => "boolean".into(),
            CqlType::Counter => "counter".into(),
            CqlType::Decimal => "decimal".into(),
            CqlType::Double => "double".into(),
            CqlType::Float => "float".into(),
            CqlType::Int => "int".into(),
            CqlType::Timestamp => "timestamp".into(),
            CqlType::Uuid => "uuid".into(),
            CqlType::Varchar => "text".into(),
            CqlType::Varint => "varint".into(),
            CqlType::Timeuuid => "timeuuid".into(),
            CqlType::Inet => "inet".into(),
            CqlType::Date => "date".into(),
            CqlType::Time => "time".into(),
            CqlType::Smallint => "smallint".into(),
            CqlType::Tinyint => "tinyint".into(),
            CqlType::Duration => "duration".into(),
            CqlType::List(t) => format!("list<{}>", t.name()),
            CqlType::Set(t) => format!("set<{}>", t.name()),
            CqlType::Map(k, v) => format!("map<{}, {}>", k.name(), v.name()),
            CqlType::Udt { name, .. } => name.clone(),
            CqlType::Tuple(ts) => format!("tuple<{}>", ts.iter().map(|t| t.name()).collect::<Vec<_>>().join(", ")),
        }
    }

    /// The type used to encode a Camel value when no metadata is known.
    pub fn infer(v: &Value) -> CqlType {
        match v {
            Value::Bool(_) => CqlType::Boolean,
            Value::Int(_) => CqlType::Bigint,
            Value::Float(_) => CqlType::Double,
            Value::Bytes(_) => CqlType::Blob,
            Value::Array(a) => CqlType::List(Box::new(a.first().map(CqlType::infer).unwrap_or(CqlType::Varchar))),
            Value::Object(o) => CqlType::Map(Box::new(CqlType::Varchar), Box::new(o.values().next().map(CqlType::infer).unwrap_or(CqlType::Varchar))),
            _ => CqlType::Varchar,
        }
    }
}

fn split_type_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '<' => {
                depth += 1;
                cur.push(c)
            }
            '>' => {
                depth -= 1;
                cur.push(c)
            }
            ',' if depth == 0 => out.push(std::mem::take(&mut cur).trim().to_string()),
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// Writes an `[option]` type.
pub fn write_type(t: &CqlType, w: &mut Writer) {
    let id: u16 = match t {
        CqlType::Custom(c) => {
            w.short(0);
            w.string(c);
            return;
        }
        CqlType::Ascii => 0x01,
        CqlType::Bigint => 0x02,
        CqlType::Blob => 0x03,
        CqlType::Boolean => 0x04,
        CqlType::Counter => 0x05,
        CqlType::Decimal => 0x06,
        CqlType::Double => 0x07,
        CqlType::Float => 0x08,
        CqlType::Int => 0x09,
        CqlType::Timestamp => 0x0B,
        CqlType::Uuid => 0x0C,
        CqlType::Varchar => 0x0D,
        CqlType::Varint => 0x0E,
        CqlType::Timeuuid => 0x0F,
        CqlType::Inet => 0x10,
        CqlType::Date => 0x11,
        CqlType::Time => 0x12,
        CqlType::Smallint => 0x13,
        CqlType::Tinyint => 0x14,
        CqlType::Duration => 0x15,
        CqlType::List(e) => {
            w.short(0x20);
            write_type(e, w);
            return;
        }
        CqlType::Map(k, v) => {
            w.short(0x21);
            write_type(k, w);
            write_type(v, w);
            return;
        }
        CqlType::Set(e) => {
            w.short(0x22);
            write_type(e, w);
            return;
        }
        CqlType::Udt { keyspace, name, fields } => {
            w.short(0x30);
            w.string(keyspace);
            w.string(name);
            w.short(fields.len() as u16);
            for (n, t) in fields {
                w.string(n);
                write_type(t, w);
            }
            return;
        }
        CqlType::Tuple(ts) => {
            w.short(0x31);
            w.short(ts.len() as u16);
            for t in ts {
                write_type(t, w);
            }
            return;
        }
    };
    w.short(id);
}

/// Reads an `[option]` type.
pub fn read_type(r: &mut Reader) -> Result<CqlType> {
    Ok(match r.short()? {
        0x00 => CqlType::Custom(r.string()?),
        0x01 => CqlType::Ascii,
        0x02 => CqlType::Bigint,
        0x03 => CqlType::Blob,
        0x04 => CqlType::Boolean,
        0x05 => CqlType::Counter,
        0x06 => CqlType::Decimal,
        0x07 => CqlType::Double,
        0x08 => CqlType::Float,
        0x09 => CqlType::Int,
        0x0B => CqlType::Timestamp,
        0x0C => CqlType::Uuid,
        0x0D => CqlType::Varchar,
        0x0E => CqlType::Varint,
        0x0F => CqlType::Timeuuid,
        0x10 => CqlType::Inet,
        0x11 => CqlType::Date,
        0x12 => CqlType::Time,
        0x13 => CqlType::Smallint,
        0x14 => CqlType::Tinyint,
        0x15 => CqlType::Duration,
        0x20 => CqlType::List(Box::new(read_type(r)?)),
        0x21 => CqlType::Map(Box::new(read_type(r)?), Box::new(read_type(r)?)),
        0x22 => CqlType::Set(Box::new(read_type(r)?)),
        0x30 => {
            let keyspace = r.string()?;
            let name = r.string()?;
            let n = r.short()?;
            let fields = (0..n).map(|_| Ok((r.string()?, read_type(r)?))).collect::<Result<_>>()?;
            CqlType::Udt { keyspace, name, fields }
        }
        0x31 => {
            let n = r.short()?;
            CqlType::Tuple((0..n).map(|_| read_type(r)).collect::<Result<_>>()?)
        }
        other => return Err(proto_err(format!("unknown type id 0x{:04x}", other))),
    })
}

fn conv_err(t: &CqlType, v: &Value) -> CamelError {
    CamelError::new(crate::kinds::CASSANDRA_INVALID_QUERY, format!("Cannot encode {} value {:?} as {}", v.type_name(), v, t.name()))
}

fn int_of(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

/// Formats 16 bytes as a UUID.
pub fn format_uuid(b: &[u8]) -> String {
    let h = hex(b);
    if h.len() != 32 {
        return h;
    }
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// Parses a UUID (with or without dashes).
pub fn parse_uuid(s: &str) -> Option<[u8; 16]> {
    let h: String = s.chars().filter(|c| *c != '-').collect();
    unhex(&h)?.try_into().ok()
}

/// A random (version 4) UUID.
pub fn random_uuid() -> String {
    let mut b = [0u8; 16];
    camel_tls::crypto::rng::fill(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format_uuid(&b)
}

/// A time-based (version 1) UUID for the current time.
pub fn time_uuid() -> String {
    let ts = (crate::util::now_millis() as u64 * 10_000 + crate::util::next_id() % 10_000) + 0x01B2_1DD2_1381_4000;
    let mut b = [0u8; 16];
    b[0..4].copy_from_slice(&(ts as u32).to_be_bytes());
    b[4..6].copy_from_slice(&((ts >> 32) as u16).to_be_bytes());
    b[6..8].copy_from_slice(&((((ts >> 48) as u16) & 0x0fff) | 0x1000).to_be_bytes());
    camel_tls::crypto::rng::fill(&mut b[8..]);
    b[8] = (b[8] & 0x3f) | 0x80;
    format_uuid(&b)
}

const DATE_CENTER: i64 = 1 << 31;

fn days_to_date(days: i64) -> String {
    let iso = format_iso_date(days * 86_400_000);
    iso[..10].to_string()
}

fn date_to_days(s: &str) -> Option<i64> {
    parse_iso_date(&format!("{}T00:00:00Z", s.trim())).map(|ms| ms.div_euclid(86_400_000))
}

/// Timestamp value as epoch milliseconds (numbers or ISO-8601 text).
pub fn timestamp_millis(v: &Value) -> Option<i64> {
    match v {
        Value::Int(i) => Some(*i),
        Value::Float(f) => Some(*f as i64),
        Value::String(s) => s.trim().parse().ok().or_else(|| parse_iso_date(s.trim()).or_else(|| parse_iso_date(&format!("{}T00:00:00Z", s.trim())))),
        _ => None,
    }
}

fn varint_bytes(i: i128) -> Vec<u8> {
    let b = i.to_be_bytes();
    let mut start = 0;
    while start < 15 && ((b[start] == 0 && b[start + 1] & 0x80 == 0) || (b[start] == 0xff && b[start + 1] & 0x80 != 0)) {
        start += 1;
    }
    b[start..].to_vec()
}

fn varint_value(b: &[u8]) -> i128 {
    let mut v: i128 = if b.first().is_some_and(|x| x & 0x80 != 0) { -1 } else { 0 };
    for x in b.iter().take(16) {
        v = (v << 8) | *x as i128;
    }
    v
}

/// Encodes a Camel value for a column type (`None` = null).
pub fn encode_value(t: &CqlType, v: &Value) -> Result<Option<Vec<u8>>> {
    if v.is_null() {
        return Ok(None);
    }
    let e = || conv_err(t, v);
    Ok(Some(match t {
        CqlType::Ascii | CqlType::Varchar | CqlType::Custom(_) => match v {
            Value::Bytes(b) => b.clone(),
            other => other.to_string().into_bytes(),
        },
        CqlType::Bigint | CqlType::Counter => int_of(v).ok_or_else(e)?.to_be_bytes().to_vec(),
        CqlType::Int => (int_of(v).ok_or_else(e)? as i32).to_be_bytes().to_vec(),
        CqlType::Smallint => (int_of(v).ok_or_else(e)? as i16).to_be_bytes().to_vec(),
        CqlType::Tinyint => vec![int_of(v).ok_or_else(e)? as i8 as u8],
        CqlType::Blob => match v {
            Value::Bytes(b) => b.clone(),
            Value::String(s) => match s.strip_prefix("0x") {
                Some(h) => unhex(h).ok_or_else(e)?,
                None => s.as_bytes().to_vec(),
            },
            _ => return Err(e()),
        },
        CqlType::Boolean => vec![match v {
            Value::Bool(b) => *b as u8,
            other => other.to_string().eq_ignore_ascii_case("true") as u8,
        }],
        CqlType::Double => v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).ok_or_else(e)?.to_be_bytes().to_vec(),
        CqlType::Float => (v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).ok_or_else(e)? as f32).to_be_bytes().to_vec(),
        CqlType::Timestamp => timestamp_millis(v).ok_or_else(e)?.to_be_bytes().to_vec(),
        CqlType::Uuid | CqlType::Timeuuid => parse_uuid(&v.to_string()).ok_or_else(e)?.to_vec(),
        CqlType::Varint => varint_bytes(v.as_i64().map(|i| i as i128).or_else(|| v.to_string().trim().parse().ok()).ok_or_else(e)?),
        CqlType::Decimal => {
            let s = v.to_string();
            let s = s.trim();
            let (neg, digits) = match s.strip_prefix('-') {
                Some(r) => (true, r),
                None => (false, s),
            };
            let (int, frac) = digits.split_once('.').unwrap_or((digits, ""));
            let unscaled: i128 = format!("{}{}", int, frac).parse().map_err(|_| e())?;
            let mut out = (frac.len() as i32).to_be_bytes().to_vec();
            out.extend(varint_bytes(if neg { -unscaled } else { unscaled }));
            out
        }
        CqlType::Inet => {
            let ip: std::net::IpAddr = v.to_string().parse().map_err(|_| e())?;
            match ip {
                std::net::IpAddr::V4(a) => a.octets().to_vec(),
                std::net::IpAddr::V6(a) => a.octets().to_vec(),
            }
        }
        CqlType::Date => {
            let days = match v {
                Value::Int(d) => *d,
                other => date_to_days(&other.to_string()).ok_or_else(e)?,
            };
            ((days + DATE_CENTER) as u32).to_be_bytes().to_vec()
        }
        CqlType::Time => int_of(v).ok_or_else(e)?.to_be_bytes().to_vec(),
        CqlType::Duration => {
            let (months, days, nanos) = parse_duration_value(v).ok_or_else(e)?;
            let mut out = Vec::new();
            write_vint(&mut out, months as i64);
            write_vint(&mut out, days as i64);
            write_vint(&mut out, nanos);
            out
        }
        CqlType::List(et) | CqlType::Set(et) => {
            let items = match v {
                Value::Array(a) => a.clone(),
                other => vec![other.clone()],
            };
            let mut w = Writer::new();
            w.int(items.len() as i32);
            for it in &items {
                w.bytes(encode_value(et, it)?.as_deref());
            }
            w.0
        }
        CqlType::Map(kt, vt) => {
            let o = v.as_object().ok_or_else(e)?;
            let mut w = Writer::new();
            w.int(o.len() as i32);
            for (k, x) in o {
                w.bytes(encode_value(kt, &Value::from(k.as_str()))?.as_deref());
                w.bytes(encode_value(vt, x)?.as_deref());
            }
            w.0
        }
        CqlType::Tuple(ts) => {
            let a = v.as_array().ok_or_else(e)?;
            let mut w = Writer::new();
            for (i, t2) in ts.iter().enumerate() {
                w.bytes(encode_value(t2, a.get(i).unwrap_or(&Value::Null))?.as_deref());
            }
            w.0
        }
        CqlType::Udt { fields, .. } => {
            let mut w = Writer::new();
            for (n, t2) in fields {
                w.bytes(encode_value(t2, v.get(n).unwrap_or(&Value::Null))?.as_deref());
            }
            w.0
        }
    }))
}

fn be_int(b: &[u8]) -> i64 {
    let mut v: i64 = if b.first().is_some_and(|x| x & 0x80 != 0) { -1 } else { 0 };
    for x in b.iter().take(8) {
        v = (v << 8) | *x as i64;
    }
    v
}

/// Decodes a value of a column type (timestamps become ISO-8601 text, uuids text,
/// dates `YYYY-MM-DD`, decimals/large varints text, collections arrays/objects).
pub fn decode_value(t: &CqlType, b: Option<&[u8]>) -> Result<Value> {
    let Some(b) = b else { return Ok(Value::Null) };
    Ok(match t {
        CqlType::Ascii | CqlType::Varchar | CqlType::Custom(_) => Value::String(String::from_utf8_lossy(b).into_owned()),
        // integers are decoded leniently from any big-endian length
        CqlType::Bigint | CqlType::Counter | CqlType::Int | CqlType::Smallint | CqlType::Tinyint | CqlType::Time => {
            if b.is_empty() {
                Value::Null
            } else {
                Value::Int(be_int(b))
            }
        }
        CqlType::Blob => Value::Bytes(b.to_vec()),
        CqlType::Boolean => Value::Bool(b.first().is_some_and(|x| *x != 0)),
        CqlType::Double => match b.len() {
            8 => Value::Float(f64::from_be_bytes(b.try_into().unwrap())),
            4 => Value::Float(f32::from_be_bytes(b.try_into().unwrap()) as f64),
            _ => return Err(proto_err("invalid double")),
        },
        CqlType::Float => match b.len() {
            4 => Value::Float(f32::from_be_bytes(b.try_into().unwrap()) as f64),
            8 => Value::Float(f64::from_be_bytes(b.try_into().unwrap())),
            _ => return Err(proto_err("invalid float")),
        },
        CqlType::Timestamp => Value::String(format_iso_date(be_int(b))),
        CqlType::Uuid | CqlType::Timeuuid => Value::String(format_uuid(b)),
        CqlType::Varint => {
            let v = varint_value(b);
            if v >= i64::MIN as i128 && v <= i64::MAX as i128 {
                Value::Int(v as i64)
            } else {
                Value::String(v.to_string())
            }
        }
        CqlType::Decimal => {
            if b.len() < 4 {
                return Err(proto_err("invalid decimal"));
            }
            let scale = i32::from_be_bytes(b[..4].try_into().unwrap());
            let unscaled = varint_value(&b[4..]);
            let neg = unscaled < 0;
            let digits = unscaled.unsigned_abs().to_string();
            let s = if scale <= 0 {
                format!("{}{}", digits, "0".repeat((-scale) as usize))
            } else {
                let scale = scale as usize;
                let padded = format!("{:0>width$}", digits, width = scale + 1);
                format!("{}.{}", &padded[..padded.len() - scale], &padded[padded.len() - scale..])
            };
            Value::String(if neg { format!("-{}", s) } else { s })
        }
        CqlType::Inet => match b.len() {
            4 => Value::String(std::net::Ipv4Addr::new(b[0], b[1], b[2], b[3]).to_string()),
            16 => Value::String(std::net::Ipv6Addr::from(<[u8; 16]>::try_from(b).unwrap()).to_string()),
            _ => return Err(proto_err("invalid inet")),
        },
        CqlType::Date => {
            if b.len() != 4 {
                return Err(proto_err("invalid date"));
            }
            Value::String(days_to_date(u32::from_be_bytes(b.try_into().unwrap()) as i64 - DATE_CENTER))
        }
        CqlType::Duration => {
            let mut p = 0;
            let months = read_vint(b, &mut p).ok_or_else(|| proto_err("invalid duration"))?;
            let days = read_vint(b, &mut p).ok_or_else(|| proto_err("invalid duration"))?;
            let nanos = read_vint(b, &mut p).ok_or_else(|| proto_err("invalid duration"))?;
            Value::String(format_duration(months as i32, days as i32, nanos))
        }
        CqlType::List(et) | CqlType::Set(et) => {
            let mut r = Reader::new(b);
            let n = r.int()?;
            Value::Array((0..n.max(0)).map(|_| decode_value(et, r.bytes()?.as_deref())).collect::<Result<_>>()?)
        }
        CqlType::Map(kt, vt) => {
            let mut r = Reader::new(b);
            let n = r.int()?;
            let mut m = ObjectMap::new();
            for _ in 0..n.max(0) {
                let k = decode_value(kt, r.bytes()?.as_deref())?;
                let v = decode_value(vt, r.bytes()?.as_deref())?;
                m.insert(k.to_string(), v);
            }
            Value::Object(m)
        }
        CqlType::Tuple(ts) => {
            let mut r = Reader::new(b);
            let mut out = Vec::new();
            for t2 in ts {
                out.push(if r.remaining() > 0 { decode_value(t2, r.bytes()?.as_deref())? } else { Value::Null });
            }
            Value::Array(out)
        }
        CqlType::Udt { fields, .. } => {
            let mut r = Reader::new(b);
            let mut m = ObjectMap::new();
            for (n, t2) in fields {
                let v = if r.remaining() > 0 { decode_value(t2, r.bytes()?.as_deref())? } else { Value::Null };
                m.insert(n.clone(), v);
            }
            Value::Object(m)
        }
    })
}

/// Column metadata.
#[derive(Clone, Debug, PartialEq)]
pub struct ColumnSpec {
    pub keyspace: String,
    pub table: String,
    pub name: String,
    pub cql_type: CqlType,
}

/// Query parameter flags.
pub mod query_flags {
    pub const VALUES: u8 = 0x01;
    pub const SKIP_METADATA: u8 = 0x02;
    pub const PAGE_SIZE: u8 = 0x04;
    pub const PAGING_STATE: u8 = 0x08;
    pub const SERIAL_CONSISTENCY: u8 = 0x10;
    pub const DEFAULT_TIMESTAMP: u8 = 0x20;
    pub const NAMES_FOR_VALUES: u8 = 0x40;
}

/// `<query_parameters>` of QUERY and EXECUTE.
#[derive(Clone, Debug, PartialEq)]
pub struct QueryParams {
    pub consistency: Consistency,
    pub values: Vec<Option<Vec<u8>>>,
    pub skip_metadata: bool,
    pub page_size: Option<i32>,
    pub paging_state: Option<Vec<u8>>,
    pub serial_consistency: Option<Consistency>,
    pub timestamp: Option<i64>,
}

impl QueryParams {
    pub fn new(consistency: Consistency) -> QueryParams {
        QueryParams { consistency, values: Vec::new(), skip_metadata: false, page_size: None, paging_state: None, serial_consistency: None, timestamp: None }
    }

    pub fn write(&self, w: &mut Writer) {
        w.short(self.consistency as u16);
        let mut flags = 0u8;
        if !self.values.is_empty() {
            flags |= query_flags::VALUES;
        }
        if self.skip_metadata {
            flags |= query_flags::SKIP_METADATA;
        }
        if self.page_size.is_some() {
            flags |= query_flags::PAGE_SIZE;
        }
        if self.paging_state.is_some() {
            flags |= query_flags::PAGING_STATE;
        }
        if self.serial_consistency.is_some() {
            flags |= query_flags::SERIAL_CONSISTENCY;
        }
        if self.timestamp.is_some() {
            flags |= query_flags::DEFAULT_TIMESTAMP;
        }
        w.byte(flags);
        if !self.values.is_empty() {
            w.short(self.values.len() as u16);
            for v in &self.values {
                w.bytes(v.as_deref());
            }
        }
        if let Some(p) = self.page_size {
            w.int(p);
        }
        if let Some(s) = &self.paging_state {
            w.bytes(Some(s));
        }
        if let Some(c) = self.serial_consistency {
            w.short(c as u16);
        }
        if let Some(t) = self.timestamp {
            w.long(t);
        }
    }

    pub fn read(r: &mut Reader) -> Result<QueryParams> {
        let c = r.short()?;
        let consistency = Consistency::from_code(c).ok_or_else(|| proto_err(format!("unknown consistency {}", c)))?;
        let flags = r.byte()?;
        let mut p = QueryParams::new(consistency);
        if flags & query_flags::VALUES != 0 {
            let n = r.short()?;
            for _ in 0..n {
                if flags & query_flags::NAMES_FOR_VALUES != 0 {
                    r.string()?;
                }
                p.values.push(r.bytes()?);
            }
        }
        p.skip_metadata = flags & query_flags::SKIP_METADATA != 0;
        if flags & query_flags::PAGE_SIZE != 0 {
            p.page_size = Some(r.int()?);
        }
        if flags & query_flags::PAGING_STATE != 0 {
            p.paging_state = r.bytes()?;
        }
        if flags & query_flags::SERIAL_CONSISTENCY != 0 {
            p.serial_consistency = Consistency::from_code(r.short()?);
        }
        if flags & query_flags::DEFAULT_TIMESTAMP != 0 {
            p.timestamp = Some(r.long()?);
        }
        Ok(p)
    }
}

/// Rows metadata flags.
pub mod rows_flags {
    pub const GLOBAL_TABLES_SPEC: i32 = 0x0001;
    pub const HAS_MORE_PAGES: i32 = 0x0002;
    pub const NO_METADATA: i32 = 0x0004;
}

/// Writes `<metadata>` of a Rows result (global table spec when all columns share it).
pub fn write_rows_metadata(w: &mut Writer, cols: &[ColumnSpec], paging_state: Option<&[u8]>, no_metadata: bool) {
    let global = cols.first().map(|c| (c.keyspace.clone(), c.table.clone()));
    let all_same = cols.iter().all(|c| Some((c.keyspace.clone(), c.table.clone())) == global);
    let mut flags = 0;
    if no_metadata {
        flags |= rows_flags::NO_METADATA;
    } else if all_same && global.is_some() {
        flags |= rows_flags::GLOBAL_TABLES_SPEC;
    }
    if paging_state.is_some() {
        flags |= rows_flags::HAS_MORE_PAGES;
    }
    w.int(flags);
    w.int(cols.len() as i32);
    if let Some(p) = paging_state {
        w.bytes(Some(p));
    }
    if no_metadata {
        return;
    }
    if flags & rows_flags::GLOBAL_TABLES_SPEC != 0 {
        let (k, t) = global.unwrap();
        w.string(&k);
        w.string(&t);
    }
    for c in cols {
        if flags & rows_flags::GLOBAL_TABLES_SPEC == 0 {
            w.string(&c.keyspace);
            w.string(&c.table);
        }
        w.string(&c.name);
        write_type(&c.cql_type, w);
    }
}

/// Reads `<metadata>`: (columns, paging state, column count).
pub fn read_rows_metadata(r: &mut Reader) -> Result<(Vec<ColumnSpec>, Option<Vec<u8>>, usize)> {
    let flags = r.int()?;
    let count = r.int()?.max(0) as usize;
    let paging = if flags & rows_flags::HAS_MORE_PAGES != 0 { r.bytes()? } else { None };
    if flags & rows_flags::NO_METADATA != 0 {
        return Ok((Vec::new(), paging, count));
    }
    let global = if flags & rows_flags::GLOBAL_TABLES_SPEC != 0 { Some((r.string()?, r.string()?)) } else { None };
    let mut cols = Vec::with_capacity(count);
    for _ in 0..count {
        let (keyspace, table) = match &global {
            Some((k, t)) => (k.clone(), t.clone()),
            None => (r.string()?, r.string()?),
        };
        let name = r.string()?;
        let cql_type = read_type(r)?;
        cols.push(ColumnSpec { keyspace, table, name, cql_type });
    }
    Ok((cols, paging, count))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_frame_known_bytes() {
        let mut w = Writer::new();
        w.string_map(&[("CQL_VERSION", "3.0.0")]);
        let f = Frame::request(1, opcode::STARTUP, w.0);
        let b = f.encode();
        let expected: &[u8] = b"\x04\x00\x00\x01\x01\x00\x00\x00\x16\x00\x01\x00\x0bCQL_VERSION\x00\x053.0.0";
        assert_eq!(b, expected);
        assert_eq!(read_frame(&mut &b[..]).unwrap(), f);
    }

    #[test]
    fn query_frame_known_bytes() {
        let mut w = Writer::new();
        w.long_string("SELECT 1");
        let mut p = QueryParams::new(Consistency::Quorum);
        p.values.push(Some(vec![0, 0, 0, 7]));
        p.page_size = Some(100);
        p.write(&mut w);
        assert_eq!(w.0, b"\x00\x00\x00\x08SELECT 1\x00\x04\x05\x00\x01\x00\x00\x00\x04\x00\x00\x00\x07\x00\x00\x00\x64");
        let mut r = Reader::new(&w.0);
        assert_eq!(r.long_string().unwrap(), "SELECT 1");
        assert_eq!(QueryParams::read(&mut r).unwrap(), p);
    }

    #[test]
    fn value_codecs() {
        let rt = |t: CqlType, v: Value| {
            let b = encode_value(&t, &v).unwrap();
            decode_value(&t, b.as_deref()).unwrap()
        };
        assert_eq!(encode_value(&CqlType::Int, &Value::Int(1)).unwrap().unwrap(), vec![0, 0, 0, 1]);
        assert_eq!(rt(CqlType::Int, Value::Int(-5)), Value::Int(-5));
        assert_eq!(rt(CqlType::Bigint, Value::Int(1 << 40)), Value::Int(1 << 40));
        assert_eq!(rt(CqlType::Smallint, Value::Int(-300)), Value::Int(-300));
        assert_eq!(rt(CqlType::Tinyint, Value::Int(-3)), Value::Int(-3));
        assert_eq!(rt(CqlType::Varchar, Value::from("héllo")), Value::from("héllo"));
        assert_eq!(rt(CqlType::Boolean, Value::Bool(true)), Value::Bool(true));
        assert_eq!(rt(CqlType::Double, Value::Float(2.5)), Value::Float(2.5));
        assert_eq!(rt(CqlType::Float, Value::Float(1.5)), Value::Float(1.5));
        assert_eq!(rt(CqlType::Timestamp, Value::Int(1_700_000_000_123)), Value::from("2023-11-14T22:13:20.123Z"));
        assert_eq!(rt(CqlType::Timestamp, Value::from("2023-11-14T22:13:20.123Z")), Value::from("2023-11-14T22:13:20.123Z"));
        let u = "123e4567-e89b-12d3-a456-426614174000";
        assert_eq!(rt(CqlType::Uuid, Value::from(u)), Value::from(u));
        assert_eq!(rt(CqlType::Varint, Value::Int(-129)), Value::Int(-129));
        assert_eq!(encode_value(&CqlType::Varint, &Value::Int(128)).unwrap().unwrap(), vec![0, 128]);
        assert_eq!(rt(CqlType::Decimal, Value::from("-12.345")), Value::from("-12.345"));
        assert_eq!(rt(CqlType::Decimal, Value::from("0.05")), Value::from("0.05"));
        assert_eq!(rt(CqlType::Inet, Value::from("10.0.0.1")), Value::from("10.0.0.1"));
        assert_eq!(rt(CqlType::Date, Value::from("2024-02-29")), Value::from("2024-02-29"));
        assert_eq!(encode_value(&CqlType::Date, &Value::from("1970-01-01")).unwrap().unwrap(), vec![0x80, 0, 0, 0]);
        assert_eq!(rt(CqlType::Blob, Value::Bytes(vec![1, 2])), Value::Bytes(vec![1, 2]));
        assert_eq!(rt(CqlType::List(Box::new(CqlType::Int)), Value::Array(vec![1.into(), 2.into()])), Value::Array(vec![1.into(), 2.into()]));
        assert_eq!(rt(CqlType::Map(Box::new(CqlType::Varchar), Box::new(CqlType::Int)), Value::object([("a", 1)])), Value::object([("a", 1)]));
        assert_eq!(rt(CqlType::Tuple(vec![CqlType::Int, CqlType::Varchar]), Value::Array(vec![1.into(), "x".into()])), Value::Array(vec![1.into(), "x".into()]));
        assert_eq!(encode_value(&CqlType::Int, &Value::Null).unwrap(), None);
        assert!(encode_value(&CqlType::Int, &Value::from("x")).is_err());
    }

    #[test]
    fn types_and_metadata() {
        for t in ["int", "text", "list<int>", "map<text, bigint>", "set<uuid>", "tuple<int, text>"] {
            let ct = CqlType::parse(t).unwrap();
            assert_eq!(ct.name(), t);
            let mut w = Writer::new();
            write_type(&ct, &mut w);
            assert_eq!(read_type(&mut Reader::new(&w.0)).unwrap(), ct);
        }
        assert_eq!(CqlType::parse("frozen<list<int>>"), Some(CqlType::List(Box::new(CqlType::Int))));
        let cols = vec![
            ColumnSpec { keyspace: "ks".into(), table: "t".into(), name: "id".into(), cql_type: CqlType::Int },
            ColumnSpec { keyspace: "ks".into(), table: "t".into(), name: "v".into(), cql_type: CqlType::Varchar },
        ];
        let mut w = Writer::new();
        write_rows_metadata(&mut w, &cols, Some(&[1, 2]), false);
        let (back, paging, n) = read_rows_metadata(&mut Reader::new(&w.0)).unwrap();
        assert_eq!((back, paging, n), (cols, Some(vec![1, 2]), 2));
        assert_eq!(Consistency::parse("local_quorum").unwrap(), Consistency::LocalQuorum);
        assert_eq!(Consistency::from_code(10), Some(Consistency::LocalOne));
        assert!(parse_uuid(&random_uuid()).is_some());
        assert_eq!(&time_uuid()[14..15], "1");
    }
}

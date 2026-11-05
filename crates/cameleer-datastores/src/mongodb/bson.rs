//! BSON codec (<https://bsonspec.org>): ordered documents, all common types including
//! ObjectId, UTC datetimes, Decimal128 (string conversion), binary, regex and timestamps,
//! plus conversion to/from Camel values using MongoDB Extended JSON (relaxed) for the
//! types JSON lacks (`{"$oid": ..}`, `{"$date": ..}`, `{"$numberDecimal": ..}`, ...).

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};
use std::sync::OnceLock;

use cameleer_core::error::{CamelError, Result};
use cameleer_core::value::Value;
use cameleer_tls::pem::{base64_decode, base64_encode};

use crate::util::{hex, now_millis, unhex};

/// A 12-byte ObjectId.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ObjectId(pub [u8; 12]);

impl ObjectId {
    /// A new id: 4-byte timestamp, 5 random bytes (per process), 3-byte counter.
    pub fn new() -> ObjectId {
        static RANDOM: OnceLock<[u8; 5]> = OnceLock::new();
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let r = RANDOM.get_or_init(|| {
            let mut b = [0u8; 5];
            cameleer_tls::crypto::rng::fill(&mut b);
            b
        });
        let c = COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
        let mut id = [0u8; 12];
        id[..4].copy_from_slice(&((now_millis() / 1000) as u32).to_be_bytes());
        id[4..9].copy_from_slice(r);
        id[9..].copy_from_slice(&c.to_be_bytes()[1..]);
        ObjectId(id)
    }

    /// Parses 24 hex characters.
    pub fn parse(s: &str) -> Option<ObjectId> {
        let b = unhex(s)?;
        let arr: [u8; 12] = b.try_into().ok()?;
        Some(ObjectId(arr))
    }

    pub fn to_hex(&self) -> String {
        hex(&self.0)
    }

    /// Creation time (seconds since the epoch).
    pub fn timestamp(&self) -> u32 {
        u32::from_be_bytes([self.0[0], self.0[1], self.0[2], self.0[3]])
    }
}

impl Default for ObjectId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// IEEE 754-2008 decimal128 (BID encoding), stored as raw bytes (little endian).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Decimal128(pub [u8; 16]);

const DEC_EXP_BIAS: i32 = 6176;

impl Decimal128 {
    /// Parses `[-]digits[.digits][E[+-]digits]`, `Infinity`, `NaN` (at most 34 significant digits).
    pub fn parse(s: &str) -> Option<Decimal128> {
        let t = s.trim();
        let (neg, body) = match t.strip_prefix('-') {
            Some(r) => (true, r),
            None => (false, t.strip_prefix('+').unwrap_or(t)),
        };
        let lower = body.to_ascii_lowercase();
        if lower == "infinity" || lower == "inf" {
            let mut hi: u64 = 0x7800_0000_0000_0000;
            if neg {
                hi |= 1 << 63;
            }
            return Some(Self::from_parts(hi, 0));
        }
        if lower == "nan" {
            return Some(Self::from_parts(0x7c00_0000_0000_0000, 0));
        }
        let (mant, exp_part) = match lower.find('e') {
            Some(i) => (&lower[..i], Some(&lower[i + 1..])),
            None => (lower.as_str(), None),
        };
        let mut exp: i32 = match exp_part {
            Some(e) => e.parse().ok()?,
            None => 0,
        };
        let (int, frac) = match mant.find('.') {
            Some(i) => (&mant[..i], &mant[i + 1..]),
            None => (mant, ""),
        };
        if int.is_empty() && frac.is_empty() {
            return None;
        }
        let digits: String = format!("{}{}", int, frac);
        if !digits.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        exp -= frac.len() as i32;
        let trimmed = digits.trim_start_matches('0');
        if trimmed.len() > 34 {
            return None;
        }
        let coef: u128 = if trimmed.is_empty() { 0 } else { trimmed.parse().ok()? };
        let biased = exp + DEC_EXP_BIAS;
        if !(0..=12287).contains(&biased) {
            return None;
        }
        let mut hi = ((biased as u64) << 49) | ((coef >> 64) as u64 & 0x1_ffff_ffff_ffff);
        if neg {
            hi |= 1 << 63;
        }
        Some(Self::from_parts(hi, coef as u64))
    }

    fn from_parts(hi: u64, lo: u64) -> Decimal128 {
        let mut b = [0u8; 16];
        b[..8].copy_from_slice(&lo.to_le_bytes());
        b[8..].copy_from_slice(&hi.to_le_bytes());
        Decimal128(b)
    }

    fn parts(&self) -> (u64, u64) {
        let lo = u64::from_le_bytes(self.0[..8].try_into().unwrap());
        let hi = u64::from_le_bytes(self.0[8..].try_into().unwrap());
        (hi, lo)
    }

    /// Approximate value as f64.
    pub fn to_f64(&self) -> f64 {
        self.to_string().parse().unwrap_or(f64::NAN)
    }
}

impl fmt::Display for Decimal128 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (hi, lo) = self.parts();
        let neg = hi >> 63 == 1;
        let comb = (hi >> 58) & 0x1f;
        if comb == 0x1f {
            return f.write_str("NaN");
        }
        if comb == 0x1e {
            return f.write_str(if neg { "-Infinity" } else { "Infinity" });
        }
        let (exp, coef) = if (hi >> 61) & 0b11 == 0b11 {
            // non-canonical large coefficient: treated as zero
            (((hi >> 47) & 0x3fff) as i32 - DEC_EXP_BIAS, 0u128)
        } else {
            (((hi >> 49) & 0x3fff) as i32 - DEC_EXP_BIAS, (((hi & 0x1_ffff_ffff_ffff) as u128) << 64) | lo as u128)
        };
        let coef = if coef > 9_999_999_999_999_999_999_999_999_999_999_999u128 { 0 } else { coef };
        let digits = coef.to_string();
        let adjusted = exp + digits.len() as i32 - 1;
        let mut s = String::new();
        if neg {
            s.push('-');
        }
        if exp <= 0 && adjusted >= -6 {
            if exp == 0 {
                s.push_str(&digits);
            } else {
                let point = digits.len() as i32 + exp;
                if point > 0 {
                    s.push_str(&digits[..point as usize]);
                    s.push('.');
                    s.push_str(&digits[point as usize..]);
                } else {
                    s.push_str("0.");
                    s.push_str(&"0".repeat((-point) as usize));
                    s.push_str(&digits);
                }
            }
        } else {
            s.push_str(&digits[..1]);
            if digits.len() > 1 {
                s.push('.');
                s.push_str(&digits[1..]);
            }
            s.push('E');
            if adjusted >= 0 {
                s.push('+');
            }
            s.push_str(&adjusted.to_string());
        }
        f.write_str(&s)
    }
}

/// A BSON value.
#[derive(Clone, Debug, PartialEq)]
pub enum Bson {
    Double(f64),
    String(String),
    Document(Document),
    Array(Vec<Bson>),
    /// (subtype, bytes)
    Binary(u8, Vec<u8>),
    Undefined,
    ObjectId(ObjectId),
    Boolean(bool),
    /// Milliseconds since the Unix epoch.
    DateTime(i64),
    Null,
    /// (pattern, options)
    Regex(String, String),
    JavaScript(String),
    Symbol(String),
    Int32(i32),
    /// (time, increment)
    Timestamp(u32, u32),
    Int64(i64),
    Decimal128(Decimal128),
    MinKey,
    MaxKey,
}

/// An ordered BSON document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Document(pub Vec<(String, Bson)>);

impl Document {
    pub fn new() -> Document {
        Document(Vec::new())
    }

    /// Builder-style insert.
    pub fn with(mut self, k: impl Into<String>, v: impl Into<Bson>) -> Document {
        self.insert(k, v);
        self
    }

    /// Inserts or replaces (keeping the position of an existing key).
    pub fn insert(&mut self, k: impl Into<String>, v: impl Into<Bson>) {
        let k = k.into();
        let v = v.into();
        match self.0.iter_mut().find(|(n, _)| *n == k) {
            Some(slot) => slot.1 = v,
            None => self.0.push((k, v)),
        }
    }

    /// Inserts at the front (or replaces).
    pub fn insert_first(&mut self, k: impl Into<String>, v: impl Into<Bson>) {
        let k = k.into();
        self.remove(&k);
        self.0.insert(0, (k, v.into()));
    }

    pub fn get(&self, k: &str) -> Option<&Bson> {
        self.0.iter().find(|(n, _)| n == k).map(|(_, v)| v)
    }

    pub fn get_mut(&mut self, k: &str) -> Option<&mut Bson> {
        self.0.iter_mut().find(|(n, _)| n == k).map(|(_, v)| v)
    }

    pub fn remove(&mut self, k: &str) -> Option<Bson> {
        let i = self.0.iter().position(|(n, _)| n == k)?;
        Some(self.0.remove(i).1)
    }

    pub fn contains_key(&self, k: &str) -> bool {
        self.get(k).is_some()
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|(k, _)| k.as_str())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &Bson)> {
        self.0.iter().map(|(k, v)| (k, v))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get_str(&self, k: &str) -> Option<&str> {
        match self.get(k) {
            Some(Bson::String(s)) => Some(s),
            _ => None,
        }
    }

    pub fn get_i64(&self, k: &str) -> Option<i64> {
        self.get(k).and_then(Bson::as_i64)
    }

    pub fn get_f64(&self, k: &str) -> Option<f64> {
        self.get(k).and_then(Bson::as_f64)
    }

    pub fn get_bool(&self, k: &str) -> Option<bool> {
        match self.get(k) {
            Some(Bson::Boolean(b)) => Some(*b),
            Some(other) => other.as_f64().map(|f| f != 0.0),
            None => None,
        }
    }

    pub fn get_doc(&self, k: &str) -> Option<&Document> {
        match self.get(k) {
            Some(Bson::Document(d)) => Some(d),
            _ => None,
        }
    }

    pub fn get_array(&self, k: &str) -> Option<&Vec<Bson>> {
        match self.get(k) {
            Some(Bson::Array(a)) => Some(a),
            _ => None,
        }
    }

    /// Encodes the document.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        encode_document(self, &mut out);
        out
    }

    /// Decodes one document from the start of `b` (trailing bytes are an error).
    pub fn from_bytes(b: &[u8]) -> Result<Document> {
        let (d, n) = decode_document(b)?;
        if n != b.len() {
            return Err(bson_err("trailing bytes after document"));
        }
        Ok(d)
    }

    /// Converts to a Camel object (keys sorted by `Value::Object`).
    pub fn to_value(&self) -> Value {
        Bson::Document(self.clone()).to_value()
    }

    /// Converts a Camel object (or JSON text) to a document.
    pub fn from_value(v: &Value) -> Result<Document> {
        match Bson::from_value(v) {
            Bson::Document(d) => Ok(d),
            other => Err(CamelError::new(cameleer_core::error::kinds::TYPE_CONVERSION, format!("Cannot convert {:?} to a BSON document", other))),
        }
    }

    /// Relaxed Extended JSON text, keeping the key order.
    pub fn to_json(&self) -> String {
        let mut s = String::new();
        Bson::Document(self.clone()).write_json(&mut s);
        s
    }

    /// Parses JSON (Extended JSON accepted), keeping the key order.
    pub fn parse_json(text: &str) -> Result<Document> {
        match parse_json(text)? {
            Bson::Document(d) => Ok(d),
            _ => Err(CamelError::new(cameleer_core::error::kinds::TYPE_CONVERSION, format!("JSON is not an object: {}", text))),
        }
    }
}

impl From<f64> for Bson {
    fn from(v: f64) -> Self {
        Bson::Double(v)
    }
}
impl From<i32> for Bson {
    fn from(v: i32) -> Self {
        Bson::Int32(v)
    }
}
impl From<i64> for Bson {
    fn from(v: i64) -> Self {
        Bson::Int64(v)
    }
}
impl From<bool> for Bson {
    fn from(v: bool) -> Self {
        Bson::Boolean(v)
    }
}
impl From<&str> for Bson {
    fn from(v: &str) -> Self {
        Bson::String(v.to_string())
    }
}
impl From<String> for Bson {
    fn from(v: String) -> Self {
        Bson::String(v)
    }
}
impl From<Document> for Bson {
    fn from(v: Document) -> Self {
        Bson::Document(v)
    }
}
impl From<Vec<Bson>> for Bson {
    fn from(v: Vec<Bson>) -> Self {
        Bson::Array(v)
    }
}
impl From<ObjectId> for Bson {
    fn from(v: ObjectId) -> Self {
        Bson::ObjectId(v)
    }
}

fn bson_err(m: impl Into<String>) -> CamelError {
    CamelError::new(crate::kinds::MONGO, format!("Invalid BSON: {}", m.into()))
}

impl Bson {
    /// The element type byte.
    pub fn element_type(&self) -> u8 {
        match self {
            Bson::Double(_) => 0x01,
            Bson::String(_) => 0x02,
            Bson::Document(_) => 0x03,
            Bson::Array(_) => 0x04,
            Bson::Binary(..) => 0x05,
            Bson::Undefined => 0x06,
            Bson::ObjectId(_) => 0x07,
            Bson::Boolean(_) => 0x08,
            Bson::DateTime(_) => 0x09,
            Bson::Null => 0x0A,
            Bson::Regex(..) => 0x0B,
            Bson::JavaScript(_) => 0x0D,
            Bson::Symbol(_) => 0x0E,
            Bson::Int32(_) => 0x10,
            Bson::Timestamp(..) => 0x11,
            Bson::Int64(_) => 0x12,
            Bson::Decimal128(_) => 0x13,
            Bson::MinKey => 0xFF,
            Bson::MaxKey => 0x7F,
        }
    }

    /// The `$type` alias name.
    pub fn type_name(&self) -> &'static str {
        match self {
            Bson::Double(_) => "double",
            Bson::String(_) => "string",
            Bson::Document(_) => "object",
            Bson::Array(_) => "array",
            Bson::Binary(..) => "binData",
            Bson::Undefined => "undefined",
            Bson::ObjectId(_) => "objectId",
            Bson::Boolean(_) => "bool",
            Bson::DateTime(_) => "date",
            Bson::Null => "null",
            Bson::Regex(..) => "regex",
            Bson::JavaScript(_) => "javascript",
            Bson::Symbol(_) => "symbol",
            Bson::Int32(_) => "int",
            Bson::Timestamp(..) => "timestamp",
            Bson::Int64(_) => "long",
            Bson::Decimal128(_) => "decimal",
            Bson::MinKey => "minKey",
            Bson::MaxKey => "maxKey",
        }
    }

    pub fn is_number(&self) -> bool {
        matches!(self, Bson::Double(_) | Bson::Int32(_) | Bson::Int64(_) | Bson::Decimal128(_))
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Bson::Double(d) => Some(*d),
            Bson::Int32(i) => Some(*i as f64),
            Bson::Int64(i) => Some(*i as f64),
            Bson::Decimal128(d) => Some(d.to_f64()),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Bson::Int32(i) => Some(*i as i64),
            Bson::Int64(i) => Some(*i),
            Bson::Double(d) if d.fract() == 0.0 => Some(*d as i64),
            Bson::Decimal128(d) => d.to_string().parse().ok(),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Bson::String(s) | Bson::Symbol(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_document(&self) -> Option<&Document> {
        match self {
            Bson::Document(d) => Some(d),
            _ => None,
        }
    }

    /// Integer result of arithmetic keeps int32 when it fits.
    pub fn from_i64_compact(i: i64) -> Bson {
        if i >= i32::MIN as i64 && i <= i32::MAX as i64 {
            Bson::Int32(i as i32)
        } else {
            Bson::Int64(i)
        }
    }

    /// Canonical type order for comparisons (MongoDB sort order).
    fn type_rank(&self) -> u8 {
        match self {
            Bson::MinKey => 0,
            Bson::Undefined | Bson::Null => 1,
            Bson::Double(_) | Bson::Int32(_) | Bson::Int64(_) | Bson::Decimal128(_) => 2,
            Bson::String(_) | Bson::Symbol(_) => 3,
            Bson::Document(_) => 4,
            Bson::Array(_) => 5,
            Bson::Binary(..) => 6,
            Bson::ObjectId(_) => 7,
            Bson::Boolean(_) => 8,
            Bson::DateTime(_) => 9,
            Bson::Timestamp(..) => 10,
            Bson::Regex(..) => 11,
            Bson::JavaScript(_) => 12,
            Bson::MaxKey => 13,
        }
    }

    /// Total order following MongoDB's comparison/sort order.
    pub fn cmp_bson(&self, other: &Bson) -> Ordering {
        let (ra, rb) = (self.type_rank(), other.type_rank());
        if ra != rb {
            return ra.cmp(&rb);
        }
        match (self, other) {
            (a, b) if a.is_number() => a.as_f64().unwrap_or(0.0).partial_cmp(&b.as_f64().unwrap_or(0.0)).unwrap_or(Ordering::Equal),
            (a, b) if a.as_str().is_some() => a.as_str().cmp(&b.as_str()),
            (Bson::Document(a), Bson::Document(b)) => {
                for ((ka, va), (kb, vb)) in a.0.iter().zip(b.0.iter()) {
                    let c = va.type_rank().cmp(&vb.type_rank()).then_with(|| ka.cmp(kb)).then_with(|| va.cmp_bson(vb));
                    if c != Ordering::Equal {
                        return c;
                    }
                }
                a.len().cmp(&b.len())
            }
            (Bson::Array(a), Bson::Array(b)) => {
                for (x, y) in a.iter().zip(b.iter()) {
                    let c = x.cmp_bson(y);
                    if c != Ordering::Equal {
                        return c;
                    }
                }
                a.len().cmp(&b.len())
            }
            (Bson::Binary(sa, a), Bson::Binary(sb, b)) => a.len().cmp(&b.len()).then(sa.cmp(sb)).then_with(|| a.cmp(b)),
            (Bson::ObjectId(a), Bson::ObjectId(b)) => a.cmp(b),
            (Bson::Boolean(a), Bson::Boolean(b)) => a.cmp(b),
            (Bson::DateTime(a), Bson::DateTime(b)) => a.cmp(b),
            (Bson::Timestamp(a, ai), Bson::Timestamp(b, bi)) => (a, ai).cmp(&(b, bi)),
            (Bson::Regex(a, ao), Bson::Regex(b, bo)) => (a, ao).cmp(&(b, bo)),
            (Bson::JavaScript(a), Bson::JavaScript(b)) => a.cmp(b),
            _ => Ordering::Equal,
        }
    }

    /// Equality as used by queries (numbers compare by value).
    pub fn eq_bson(&self, other: &Bson) -> bool {
        self.type_rank() == other.type_rank() && self.cmp_bson(other) == Ordering::Equal
    }

    /// Converts to a Camel value (Relaxed Extended JSON for non-JSON types).
    pub fn to_value(&self) -> Value {
        match self {
            Bson::Double(d) => Value::Float(*d),
            Bson::String(s) | Bson::Symbol(s) => Value::String(s.clone()),
            Bson::Document(d) => Value::Object(d.0.iter().map(|(k, v)| (k.clone(), v.to_value())).collect()),
            Bson::Array(a) => Value::Array(a.iter().map(Bson::to_value).collect()),
            Bson::Binary(0, b) => Value::Bytes(b.clone()),
            Bson::Binary(st, b) => Value::object([("$binary", Value::object([("base64", Value::String(base64_encode(b))), ("subType", Value::String(format!("{:02x}", st)))]))]),
            Bson::Undefined => Value::object([("$undefined", true)]),
            Bson::ObjectId(o) => Value::object([("$oid", o.to_hex())]),
            Bson::Boolean(b) => Value::Bool(*b),
            Bson::DateTime(ms) => Value::object([("$date", Value::object([("$numberLong", ms.to_string())]))]),
            Bson::Null => Value::Null,
            Bson::Regex(p, o) => Value::object([("$regularExpression", Value::object([("pattern", p.as_str()), ("options", o.as_str())]))]),
            Bson::JavaScript(c) => Value::object([("$code", c.as_str())]),
            Bson::Int32(i) => Value::Int(*i as i64),
            Bson::Int64(i) => Value::Int(*i),
            Bson::Timestamp(t, i) => Value::object([("$timestamp", Value::object([("t", *t as i64), ("i", *i as i64)]))]),
            Bson::Decimal128(d) => Value::object([("$numberDecimal", d.to_string())]),
            Bson::MinKey => Value::object([("$minKey", 1)]),
            Bson::MaxKey => Value::object([("$maxKey", 1)]),
        }
    }

    /// Converts a Camel value: objects with a single `$oid`/`$date`/`$numberDecimal`/
    /// `$numberLong`/`$numberInt`/`$numberDouble`/`$binary`/`$regularExpression`/`$timestamp`/
    /// `$minKey`/`$maxKey`/`$code` key become the matching BSON type; integers become
    /// int32 when they fit, else int64; bytes become binary subtype 0.
    pub fn from_value(v: &Value) -> Bson {
        match v {
            Value::Null => Bson::Null,
            Value::Bool(b) => Bson::Boolean(*b),
            Value::Int(i) => Bson::from_i64_compact(*i),
            Value::Float(f) => Bson::Double(*f),
            Value::String(s) => Bson::String(s.clone()),
            Value::Bytes(b) => Bson::Binary(0, b.clone()),
            Value::Array(a) => Bson::Array(a.iter().map(Bson::from_value).collect()),
            Value::Object(o) => {
                if o.len() == 1 {
                    if let Some(special) = extended(o.iter().next().map(|(k, v)| (k.as_str(), v)).unwrap()) {
                        return special;
                    }
                }
                // keep `_id` first like the drivers do
                let mut d = Document::new();
                if let Some(id) = o.get("_id") {
                    d.insert("_id", Bson::from_value(id));
                }
                for (k, v) in o {
                    if k != "_id" {
                        d.0.push((k.clone(), Bson::from_value(v)));
                    }
                }
                Bson::Document(d)
            }
        }
    }

    /// Appends Relaxed Extended JSON.
    pub fn write_json(&self, out: &mut String) {
        match self {
            Bson::Document(d) => {
                out.push('{');
                for (i, (k, v)) in d.0.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    cameleer_core::value::json_escape(k, out);
                    out.push(':');
                    v.write_json(out);
                }
                out.push('}');
            }
            Bson::Array(a) => {
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write_json(out);
                }
                out.push(']');
            }
            other => out.push_str(&other.to_value().to_json()),
        }
    }
}

fn ext_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Int(i) => Some(*i),
        Value::Float(f) => Some(*f as i64),
        Value::String(s) => s.parse().ok(),
        Value::Object(o) => o.get("$numberLong").and_then(ext_i64),
        _ => None,
    }
}

fn extended((k, v): (&str, &Value)) -> Option<Bson> {
    Some(match k {
        "$oid" => Bson::ObjectId(ObjectId::parse(v.as_str()?)?),
        "$date" => match v {
            Value::String(s) => Bson::DateTime(parse_iso_date(s)?),
            other => Bson::DateTime(ext_i64(other)?),
        },
        "$numberDecimal" => Bson::Decimal128(Decimal128::parse(&v.to_string())?),
        "$numberLong" => Bson::Int64(ext_i64(v)?),
        "$numberInt" => Bson::Int32(v.to_string().parse().ok()?),
        "$numberDouble" => Bson::Double(match v.to_string().as_str() {
            "Infinity" => f64::INFINITY,
            "-Infinity" => f64::NEG_INFINITY,
            "NaN" => f64::NAN,
            s => s.parse().ok()?,
        }),
        "$binary" => {
            let o = v.as_object()?;
            let b = base64_decode(o.get("base64")?.as_str()?)?;
            let st = u8::from_str_radix(o.get("subType").map(|s| s.to_string()).as_deref().unwrap_or("00"), 16).ok()?;
            Bson::Binary(st, b)
        }
        "$regularExpression" => {
            let o = v.as_object()?;
            Bson::Regex(o.get("pattern")?.to_string(), o.get("options").map(|s| s.to_string()).unwrap_or_default())
        }
        "$timestamp" => {
            let o = v.as_object()?;
            Bson::Timestamp(o.get("t")?.as_i64()? as u32, o.get("i")?.as_i64()? as u32)
        }
        "$minKey" => Bson::MinKey,
        "$maxKey" => Bson::MaxKey,
        "$undefined" => Bson::Undefined,
        "$code" => Bson::JavaScript(v.as_str()?.to_string()),
        _ => return None,
    })
}

/// Parses `YYYY-MM-DDTHH:MM:SS[.fff](Z|±HH:MM)` to epoch milliseconds.
pub fn parse_iso_date(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> { s.get(r)?.parse().ok() };
    let (y, mo, d, h, mi, sec) = (num(0..4)?, num(5..7)?, num(8..10)?, num(11..13)?, num(14..16)?, num(17..19)?);
    let mut i = 19;
    let mut ms = 0i64;
    if b.get(i) == Some(&b'.') {
        let start = i + 1;
        i = start;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        let frac = &s[start..i];
        let padded = format!("{:0<3}", &frac[..frac.len().min(3)]);
        ms = padded.parse().ok()?;
    }
    let offset_min = match s.get(i..) {
        Some("Z") | Some("") | None => 0,
        Some(tz) => {
            let sign = if tz.starts_with('-') { -1 } else { 1 };
            let t = &tz[1..];
            let (hh, mm) = (t.get(0..2)?.parse::<i64>().ok()?, t.get(t.len().saturating_sub(2)..)?.parse::<i64>().ok()?);
            sign * (hh * 60 + mm)
        }
    };
    let days = days_from_civil(y, mo, d);
    Some(((days * 86400 + h * 3600 + mi * 60 + sec) - offset_min * 60) * 1000 + ms)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Formats epoch milliseconds as ISO-8601 UTC.
pub fn format_iso_date(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", y, m, d, rem / 3600, rem % 3600 / 60, rem % 60, millis)
}

// ---------------------------------------------------------------------------
// binary codec
// ---------------------------------------------------------------------------

fn cstring(out: &mut Vec<u8>, s: &str) {
    out.extend(s.bytes().filter(|b| *b != 0));
    out.push(0);
}

fn string(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&((s.len() + 1) as i32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
    out.push(0);
}

/// Appends an encoded document.
pub fn encode_document(d: &Document, out: &mut Vec<u8>) {
    let start = out.len();
    out.extend_from_slice(&[0, 0, 0, 0]);
    for (k, v) in &d.0 {
        encode_element(k, v, out);
    }
    out.push(0);
    let len = (out.len() - start) as i32;
    out[start..start + 4].copy_from_slice(&len.to_le_bytes());
}

fn encode_element(k: &str, v: &Bson, out: &mut Vec<u8>) {
    out.push(v.element_type());
    cstring(out, k);
    match v {
        Bson::Double(d) => out.extend_from_slice(&d.to_le_bytes()),
        Bson::String(s) | Bson::JavaScript(s) | Bson::Symbol(s) => string(out, s),
        Bson::Document(d) => encode_document(d, out),
        Bson::Array(a) => {
            let d = Document(a.iter().enumerate().map(|(i, v)| (i.to_string(), v.clone())).collect());
            encode_document(&d, out)
        }
        Bson::Binary(st, b) => {
            out.extend_from_slice(&(b.len() as i32).to_le_bytes());
            out.push(*st);
            out.extend_from_slice(b);
        }
        Bson::Undefined | Bson::Null | Bson::MinKey | Bson::MaxKey => {}
        Bson::ObjectId(o) => out.extend_from_slice(&o.0),
        Bson::Boolean(b) => out.push(*b as u8),
        Bson::DateTime(ms) => out.extend_from_slice(&ms.to_le_bytes()),
        Bson::Regex(p, o) => {
            cstring(out, p);
            let mut opts: Vec<char> = o.chars().collect();
            opts.sort_unstable();
            cstring(out, &opts.into_iter().collect::<String>());
        }
        Bson::Int32(i) => out.extend_from_slice(&i.to_le_bytes()),
        Bson::Timestamp(t, i) => {
            out.extend_from_slice(&i.to_le_bytes());
            out.extend_from_slice(&t.to_le_bytes());
        }
        Bson::Int64(i) => out.extend_from_slice(&i.to_le_bytes()),
        Bson::Decimal128(d) => out.extend_from_slice(&d.0),
    }
}

struct Cursor<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.b.len() {
            return Err(bson_err("unexpected end of data"));
        }
        let s = &self.b[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn cstring(&mut self) -> Result<String> {
        let end = self.b[self.pos..].iter().position(|c| *c == 0).ok_or_else(|| bson_err("unterminated cstring"))?;
        let s = String::from_utf8(self.b[self.pos..self.pos + end].to_vec()).map_err(|_| bson_err("invalid UTF-8 in key"))?;
        self.pos += end + 1;
        Ok(s)
    }
    fn string(&mut self) -> Result<String> {
        let len = self.i32()?;
        if len < 1 {
            return Err(bson_err("invalid string length"));
        }
        let b = self.take(len as usize)?;
        if b[b.len() - 1] != 0 {
            return Err(bson_err("string not null-terminated"));
        }
        String::from_utf8(b[..b.len() - 1].to_vec()).map_err(|_| bson_err("invalid UTF-8 string"))
    }
}

/// Decodes a document at the start of `b`, returning it and the number of bytes used.
pub fn decode_document(b: &[u8]) -> Result<(Document, usize)> {
    if b.len() < 5 {
        return Err(bson_err("document too short"));
    }
    let len = i32::from_le_bytes(b[..4].try_into().unwrap());
    if len < 5 || len as usize > b.len() {
        return Err(bson_err(format!("invalid document length {}", len)));
    }
    let body = &b[..len as usize];
    if body[body.len() - 1] != 0 {
        return Err(bson_err("document not null-terminated"));
    }
    let mut c = Cursor { b: &body[..body.len() - 1], pos: 4 };
    let mut d = Document::new();
    while c.pos < c.b.len() {
        let t = c.u8()?;
        let k = c.cstring()?;
        let v = match t {
            0x01 => Bson::Double(f64::from_le_bytes(c.take(8)?.try_into().unwrap())),
            0x02 => Bson::String(c.string()?),
            0x03 | 0x04 => {
                let (sub, n) = decode_document(&c.b[c.pos..]).map_err(|_| bson_err("invalid embedded document"))?;
                c.pos += n;
                if t == 0x03 {
                    Bson::Document(sub)
                } else {
                    Bson::Array(sub.0.into_iter().map(|(_, v)| v).collect())
                }
            }
            0x05 => {
                let n = c.i32()?;
                if n < 0 {
                    return Err(bson_err("negative binary length"));
                }
                let st = c.u8()?;
                let mut data = c.take(n as usize)?.to_vec();
                if st == 0x02 && data.len() >= 4 {
                    data.drain(..4); // old binary subtype: inner length prefix
                }
                Bson::Binary(st, data)
            }
            0x06 => Bson::Undefined,
            0x07 => Bson::ObjectId(ObjectId(c.take(12)?.try_into().unwrap())),
            0x08 => Bson::Boolean(c.u8()? != 0),
            0x09 => Bson::DateTime(c.i64()?),
            0x0A => Bson::Null,
            0x0B => Bson::Regex(c.cstring()?, c.cstring()?),
            0x0C => {
                let ns = c.string()?;
                let id = ObjectId(c.take(12)?.try_into().unwrap());
                Bson::Document(Document::new().with("$ref", ns).with("$id", id))
            }
            0x0D => Bson::JavaScript(c.string()?),
            0x0E => Bson::Symbol(c.string()?),
            0x0F => {
                let _total = c.i32()?;
                let code = c.string()?;
                let (scope, n) = decode_document(&c.b[c.pos..])?;
                c.pos += n;
                Bson::Document(Document::new().with("$code", code).with("$scope", scope))
            }
            0x10 => Bson::Int32(c.i32()?),
            0x11 => {
                let inc = c.u32()?;
                let time = c.u32()?;
                Bson::Timestamp(time, inc)
            }
            0x12 => Bson::Int64(c.i64()?),
            0x13 => Bson::Decimal128(Decimal128(c.take(16)?.try_into().unwrap())),
            0xFF => Bson::MinKey,
            0x7F => Bson::MaxKey,
            other => return Err(bson_err(format!("unknown element type 0x{:02x}", other))),
        };
        d.0.push((k, v));
    }
    Ok((d, len as usize))
}

// ---------------------------------------------------------------------------
// order-preserving JSON parser (Extended JSON aware)
// ---------------------------------------------------------------------------

/// Parses JSON into BSON keeping the key order (`Value` would sort keys).
pub fn parse_json(text: &str) -> Result<Bson> {
    let mut p = JsonParser { s: text.as_bytes(), i: 0 };
    p.ws();
    let v = p.value()?;
    p.ws();
    if p.i != p.s.len() {
        return Err(p.err("trailing characters"));
    }
    Ok(v)
}

struct JsonParser<'a> {
    s: &'a [u8],
    i: usize,
}

impl JsonParser<'_> {
    fn err(&self, m: &str) -> CamelError {
        CamelError::new(cameleer_core::error::kinds::PARSE, format!("Invalid JSON at {}: {}", self.i, m))
    }
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn value(&mut self) -> Result<Bson> {
        match self.s.get(self.i) {
            Some(b'{') => {
                self.i += 1;
                let mut d = Document::new();
                self.ws();
                if self.s.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Bson::Document(d));
                }
                loop {
                    self.ws();
                    let k = match self.s.get(self.i) {
                        Some(b'"') | Some(b'\'') => self.string()?,
                        _ => self.bare_word()?,
                    };
                    self.ws();
                    if self.s.get(self.i) != Some(&b':') {
                        return Err(self.err("expected ':'"));
                    }
                    self.i += 1;
                    self.ws();
                    let v = self.value()?;
                    d.0.push((k, v));
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            break;
                        }
                        _ => return Err(self.err("expected ',' or '}'")),
                    }
                }
                if d.len() == 1 {
                    let (k, v) = &d.0[0];
                    if k.starts_with('$') {
                        if let Some(b) = extended((k, &v.to_value())) {
                            return Ok(b);
                        }
                    }
                }
                Ok(Bson::Document(d))
            }
            Some(b'[') => {
                self.i += 1;
                let mut a = Vec::new();
                self.ws();
                if self.s.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Bson::Array(a));
                }
                loop {
                    self.ws();
                    a.push(self.value()?);
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            break;
                        }
                        _ => return Err(self.err("expected ',' or ']'")),
                    }
                }
                Ok(Bson::Array(a))
            }
            Some(b'"') | Some(b'\'') => Ok(Bson::String(self.string()?)),
            Some(c) if *c == b'-' || c.is_ascii_digit() => self.number(),
            Some(_) => {
                let w = self.bare_word()?;
                match w.as_str() {
                    "true" => Ok(Bson::Boolean(true)),
                    "false" => Ok(Bson::Boolean(false)),
                    "null" => Ok(Bson::Null),
                    "ObjectId" | "ISODate" | "NumberLong" | "NumberInt" | "NumberDecimal" => self.shell_call(&w),
                    _ => Err(self.err("unexpected token")),
                }
            }
            None => Err(self.err("unexpected end")),
        }
    }
    /// Shell helpers: `ObjectId("..")`, `ISODate("..")`, `NumberLong(1)`...
    fn shell_call(&mut self, name: &str) -> Result<Bson> {
        self.ws();
        if self.s.get(self.i) != Some(&b'(') {
            return Err(self.err("expected '('"));
        }
        self.i += 1;
        self.ws();
        let arg = self.value()?;
        self.ws();
        if self.s.get(self.i) != Some(&b')') {
            return Err(self.err("expected ')'"));
        }
        self.i += 1;
        let text = match &arg {
            Bson::String(s) => s.clone(),
            other => other.to_value().to_string(),
        };
        let r = match name {
            "ObjectId" => ObjectId::parse(&text).map(Bson::ObjectId),
            "ISODate" => parse_iso_date(&text).map(Bson::DateTime),
            "NumberLong" => text.parse().ok().map(Bson::Int64),
            "NumberInt" => text.parse().ok().map(Bson::Int32),
            _ => Decimal128::parse(&text).map(Bson::Decimal128),
        };
        r.ok_or_else(|| self.err("invalid argument"))
    }
    fn bare_word(&mut self) -> Result<String> {
        let start = self.i;
        while self.i < self.s.len() && (self.s[self.i].is_ascii_alphanumeric() || self.s[self.i] == b'_' || self.s[self.i] == b'$' || self.s[self.i] == b'.') {
            self.i += 1;
        }
        if start == self.i {
            return Err(self.err("unexpected character"));
        }
        Ok(String::from_utf8_lossy(&self.s[start..self.i]).into_owned())
    }
    fn number(&mut self) -> Result<Bson> {
        let start = self.i;
        if self.s[self.i] == b'-' {
            self.i += 1;
        }
        let mut float = false;
        while self.i < self.s.len() {
            match self.s[self.i] {
                b'0'..=b'9' => {}
                b'.' | b'e' | b'E' | b'+' | b'-' => float = true,
                _ => break,
            }
            self.i += 1;
        }
        let t = std::str::from_utf8(&self.s[start..self.i]).unwrap_or("");
        if !float {
            if let Ok(i) = t.parse::<i64>() {
                return Ok(Bson::from_i64_compact(i));
            }
        }
        t.parse::<f64>().map(Bson::Double).map_err(|_| self.err("invalid number"))
    }
    fn string(&mut self) -> Result<String> {
        let q = self.s[self.i];
        self.i += 1;
        let mut out = Vec::new();
        loop {
            let Some(&c) = self.s.get(self.i) else { return Err(self.err("unterminated string")) };
            self.i += 1;
            if c == q {
                break;
            }
            if c == b'\\' {
                let Some(&e) = self.s.get(self.i) else { return Err(self.err("bad escape")) };
                self.i += 1;
                match e {
                    b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    b'r' => out.push(b'\r'),
                    b'b' => out.push(8),
                    b'f' => out.push(12),
                    b'u' => {
                        let hexs = std::str::from_utf8(self.s.get(self.i..self.i + 4).ok_or_else(|| self.err("bad \\u"))?).unwrap_or("");
                        let mut cp = u32::from_str_radix(hexs, 16).map_err(|_| self.err("bad \\u"))?;
                        self.i += 4;
                        if (0xD800..0xDC00).contains(&cp) && self.s.get(self.i) == Some(&b'\\') && self.s.get(self.i + 1) == Some(&b'u') {
                            let lo = std::str::from_utf8(self.s.get(self.i + 2..self.i + 6).unwrap_or_default()).unwrap_or("");
                            if let Ok(l) = u32::from_str_radix(lo, 16) {
                                cp = 0x10000 + ((cp - 0xD800) << 10) + (l - 0xDC00);
                                self.i += 6;
                            }
                        }
                        let ch = char::from_u32(cp).unwrap_or('\u{fffd}');
                        let mut buf = [0u8; 4];
                        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                    }
                    other => out.push(other),
                }
            } else {
                out.push(c);
            }
        }
        String::from_utf8(out).map_err(|_| self.err("invalid UTF-8"))
    }
}

/// Converts a JSON-like value or JSON text into a document.
pub fn to_document(v: &Value) -> Result<Document> {
    match v {
        Value::String(s) => Document::parse_json(s),
        Value::Bytes(b) => Document::parse_json(&String::from_utf8_lossy(b)),
        Value::Null => Ok(Document::new()),
        other => Document::from_value(other),
    }
}

/// Map view of a document (for simple lookups by name).
pub fn to_map(d: &Document) -> BTreeMap<String, Bson> {
    d.0.iter().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_bytes() {
        // {"hello": "world"} from the BSON spec
        let d = Document::new().with("hello", "world");
        assert_eq!(d.to_bytes(), b"\x16\x00\x00\x00\x02hello\x00\x06\x00\x00\x00world\x00\x00");
        // {"BSON": ["awesome", 5.05, 1986]}
        let d = Document::new().with("BSON", vec![Bson::from("awesome"), Bson::Double(5.05), Bson::Int32(1986)]);
        let expected: &[u8] = b"\x31\x00\x00\x00\x04BSON\x00\x26\x00\x00\x00\x020\x00\x08\x00\x00\x00awesome\x00\x011\x00\x33\x33\x33\x33\x33\x33\x14\x40\x102\x00\xc2\x07\x00\x00\x00\x00";
        assert_eq!(d.to_bytes(), expected);
        assert_eq!(Document::from_bytes(expected).unwrap(), d);
    }

    #[test]
    fn round_trip_all_types() {
        let oid = ObjectId::new();
        let d = Document::new()
            .with("_id", oid)
            .with("d", 1.5)
            .with("i", 7)
            .with("l", 1i64 << 40)
            .with("b", true)
            .with("n", Bson::Null)
            .with("dt", Bson::DateTime(1_700_000_000_123))
            .with("bin", Bson::Binary(4, vec![1, 2, 3]))
            .with("re", Bson::Regex("^a".into(), "i".into()))
            .with("ts", Bson::Timestamp(5, 6))
            .with("dec", Bson::Decimal128(Decimal128::parse("12.345").unwrap()))
            .with("sub", Document::new().with("x", "y"))
            .with("min", Bson::MinKey)
            .with("max", Bson::MaxKey)
            .with("js", Bson::JavaScript("1+1".into()));
        assert_eq!(Document::from_bytes(&d.to_bytes()).unwrap(), d);
        // through Camel values and extended JSON
        let v = d.to_value();
        assert_eq!(v.get("_id").unwrap().get("$oid").unwrap().as_str().unwrap(), oid.to_hex());
        let back = Document::from_value(&v).unwrap();
        for (k, val) in d.iter() {
            assert!(back.get(k).unwrap().eq_bson(val), "{} {:?} vs {:?}", k, val, back.get(k));
        }
        let json = d.to_json();
        let parsed = Document::parse_json(&json).unwrap();
        assert_eq!(parsed.keys().collect::<Vec<_>>(), d.keys().collect::<Vec<_>>());
        assert_eq!(parsed.get("dec"), d.get("dec"));
        assert!(Document::from_bytes(&[5, 0, 0, 0, 1]).is_err());
        assert!(Document::from_bytes(&[3, 0, 0]).is_err());
    }

    #[test]
    fn decimal128() {
        for s in ["0", "1", "-1", "12.345", "0.001", "1.000", "1E+3", "-1.23E-10", "9999999999999999999999999999999999", "Infinity", "-Infinity", "NaN"] {
            let d = Decimal128::parse(s).unwrap();
            assert_eq!(d.to_string(), s, "{}", s);
        }
        // spec vector: "1" = 0x3040000000000000 0000000000000001
        let one = Decimal128::parse("1").unwrap();
        assert_eq!(one.0, [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x40, 0x30]);
        assert_eq!(Decimal128::parse("1000").unwrap().to_string(), "1000");
        assert!((Decimal128::parse("2.5").unwrap().to_f64() - 2.5).abs() < 1e-12);
        assert!(Decimal128::parse("abc").is_none());
    }

    #[test]
    fn dates_and_shell_syntax() {
        assert_eq!(parse_iso_date("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso_date("2023-11-14T22:13:20.123Z"), Some(1_700_000_000_123));
        assert_eq!(format_iso_date(1_700_000_000_123), "2023-11-14T22:13:20.123Z");
        assert_eq!(parse_iso_date("2023-11-14T23:13:20+01:00"), Some(1_700_000_000_000));
        let d = Document::parse_json("{ _id: ObjectId(\"5f1d7f8e9b1e8a3c4d5e6f70\"), when: ISODate(\"1970-01-01T00:00:01Z\"), n: NumberLong(5), 'q': {\"$gt\": 1} }").unwrap();
        assert_eq!(d.get("_id"), Some(&Bson::ObjectId(ObjectId::parse("5f1d7f8e9b1e8a3c4d5e6f70").unwrap())));
        assert_eq!(d.get("when"), Some(&Bson::DateTime(1000)));
        assert_eq!(d.get("n"), Some(&Bson::Int64(5)));
        assert_eq!(d.get_doc("q").unwrap().get("$gt"), Some(&Bson::Int32(1)));
        let d = Document::parse_json("{\"b\":1,\"a\":{\"$date\":\"1970-01-01T00:00:00.5Z\"}}").unwrap();
        assert_eq!(d.keys().collect::<Vec<_>>(), vec!["b", "a"]);
        assert_eq!(d.get("a"), Some(&Bson::DateTime(500)));
    }

    #[test]
    fn ordering() {
        assert_eq!(Bson::Int32(1).cmp_bson(&Bson::Double(1.0)), Ordering::Equal);
        assert_eq!(Bson::Null.cmp_bson(&Bson::Int32(0)), Ordering::Less);
        assert_eq!(Bson::from("a").cmp_bson(&Bson::Int32(5)), Ordering::Greater);
        assert!(Bson::Int64(3).eq_bson(&Bson::Decimal128(Decimal128::parse("3").unwrap())));
    }
}

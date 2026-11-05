//! `cbor` data format (RFC 8949): `Value` <-> CBOR bytes.
//!
//! Encoding uses preferred serialization (shortest integer heads, shortest float that
//! represents the value exactly - half, single or double precision). Decoding supports
//! all major types, indefinite lengths, bignums (tags 2/3, as `Int` when they fit, else
//! as a decimal string), and simple values; other tags are dropped (the tagged item is
//! returned). Map keys that are not text are rendered as text. Options accepted as Camel
//! does (`unmarshalType`, `useList`, ... are ignored).

use std::sync::Arc;

use cameleer_core::dataformat::DataFormat;
use cameleer_core::error::Result;
use cameleer_core::message::Exchange;
use cameleer_core::value::ObjectMap;
use cameleer_core::Value;

use crate::{body_bytes, df_err, Opts};

fn head(out: &mut Vec<u8>, major: u8, v: u64) {
    let m = major << 5;
    if v < 24 {
        out.push(m | v as u8);
    } else if v <= 0xff {
        out.push(m | 24);
        out.push(v as u8);
    } else if v <= 0xffff {
        out.push(m | 25);
        out.extend_from_slice(&(v as u16).to_be_bytes());
    } else if v <= 0xffff_ffff {
        out.push(m | 26);
        out.extend_from_slice(&(v as u32).to_be_bytes());
    } else {
        out.push(m | 27);
        out.extend_from_slice(&v.to_be_bytes());
    }
}

/// f64 -> IEEE half if exactly representable.
fn to_half(f: f64) -> Option<u16> {
    if f.is_nan() {
        return Some(0x7e00);
    }
    if f.is_infinite() {
        return Some(if f > 0.0 { 0x7c00 } else { 0xfc00 });
    }
    let fl = f as f32;
    if fl as f64 != f {
        return None;
    }
    let bits = fl.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xff) as i32;
    let mant = bits & 0x7f_ffff;
    if exp == 0 && mant == 0 {
        return Some(sign);
    }
    let e = exp - 127;
    if (-14..=15).contains(&e) {
        if mant & 0x1fff != 0 {
            return None;
        }
        return Some(sign | (((e + 15) as u16) << 10) | (mant >> 13) as u16);
    }
    if (-24..-14).contains(&e) {
        // subnormal half
        let full = mant | 0x80_0000;
        let shift = (-14 - e) as u32 + 13;
        if full & ((1 << shift) - 1) != 0 {
            return None;
        }
        return Some(sign | (full >> shift) as u16);
    }
    None
}

fn from_half(h: u16) -> f64 {
    let exp = (h >> 10) & 0x1f;
    let mant = (h & 0x3ff) as f64;
    let v = if exp == 0 {
        mant * 2f64.powi(-24)
    } else if exp != 31 {
        (mant + 1024.0) * 2f64.powi(exp as i32 - 25)
    } else if mant == 0.0 {
        f64::INFINITY
    } else {
        f64::NAN
    };
    if h & 0x8000 != 0 {
        -v
    } else {
        v
    }
}

/// Encode a value as CBOR.
pub fn encode(v: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    enc(v, &mut out);
    out
}

fn enc(v: &Value, out: &mut Vec<u8>) {
    match v {
        Value::Null => out.push(0xf6),
        Value::Bool(false) => out.push(0xf4),
        Value::Bool(true) => out.push(0xf5),
        Value::Int(i) => {
            if *i >= 0 {
                head(out, 0, *i as u64)
            } else {
                head(out, 1, (-1 - *i) as u64)
            }
        }
        Value::Float(f) => {
            if let Some(h) = to_half(*f) {
                out.push(0xf9);
                out.extend_from_slice(&h.to_be_bytes());
            } else if (*f as f32) as f64 == *f {
                out.push(0xfa);
                out.extend_from_slice(&(*f as f32).to_bits().to_be_bytes());
            } else {
                out.push(0xfb);
                out.extend_from_slice(&f.to_bits().to_be_bytes());
            }
        }
        Value::Bytes(b) => {
            head(out, 2, b.len() as u64);
            out.extend_from_slice(b);
        }
        Value::String(s) => {
            head(out, 3, s.len() as u64);
            out.extend_from_slice(s.as_bytes());
        }
        Value::Array(a) => {
            head(out, 4, a.len() as u64);
            for x in a {
                enc(x, out);
            }
        }
        Value::Object(m) => {
            head(out, 5, m.len() as u64);
            for (k, x) in m.iter() {
                head(out, 3, k.len() as u64);
                out.extend_from_slice(k.as_bytes());
                enc(x, out);
            }
        }
    }
}

/// Decode one CBOR item (trailing bytes are an error).
pub fn decode(data: &[u8]) -> Result<Value> {
    let mut d = Decoder { data, pos: 0, depth: 0 };
    let v = d.item()?;
    if d.pos != data.len() {
        return Err(df_err("Trailing bytes after CBOR item"));
    }
    Ok(v)
}

/// Decode a sequence of CBOR items (RFC 8742).
pub fn decode_sequence(data: &[u8]) -> Result<Vec<Value>> {
    let mut d = Decoder { data, pos: 0, depth: 0 };
    let mut out = Vec::new();
    while d.pos < data.len() {
        out.push(d.item()?);
    }
    Ok(out)
}

struct Decoder<'a> {
    data: &'a [u8],
    pos: usize,
    depth: usize,
}

enum Item {
    Val(Value),
    Break,
}

impl<'a> Decoder<'a> {
    fn byte(&mut self) -> Result<u8> {
        let b = *self.data.get(self.pos).ok_or_else(|| df_err("Unexpected end of CBOR data"))?;
        self.pos += 1;
        Ok(b)
    }
    fn take(&mut self, n: u64) -> Result<&'a [u8]> {
        let n = usize::try_from(n).map_err(|_| df_err("CBOR length too large"))?;
        if self.data.len() - self.pos < n {
            return Err(df_err("Unexpected end of CBOR data"));
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn arg(&mut self, info: u8) -> Result<Option<u64>> {
        Ok(Some(match info {
            0..=23 => info as u64,
            24 => self.byte()? as u64,
            25 => u16::from_be_bytes(self.take(2)?.try_into().unwrap()) as u64,
            26 => u32::from_be_bytes(self.take(4)?.try_into().unwrap()) as u64,
            27 => u64::from_be_bytes(self.take(8)?.try_into().unwrap()),
            31 => return Ok(None),
            _ => return Err(df_err("Invalid CBOR additional information")),
        }))
    }
    fn item(&mut self) -> Result<Value> {
        match self.any()? {
            Item::Val(v) => Ok(v),
            Item::Break => Err(df_err("Unexpected CBOR break")),
        }
    }
    fn chunks(&mut self, major: u8) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        loop {
            let b = self.byte()?;
            if b == 0xff {
                return Ok(out);
            }
            if b >> 5 != major {
                return Err(df_err("Invalid chunk in indefinite-length CBOR string"));
            }
            let n = self.arg(b & 31)?.ok_or_else(|| df_err("Nested indefinite CBOR string"))?;
            out.extend_from_slice(self.take(n)?);
        }
    }
    fn any(&mut self) -> Result<Item> {
        let b = self.byte()?;
        let major = b >> 5;
        let info = b & 31;
        if major == 7 {
            return Ok(Item::Val(match info {
                20 => Value::Bool(false),
                21 => Value::Bool(true),
                22 | 23 => Value::Null,
                25 => Value::Float(from_half(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))),
                26 => Value::Float(f32::from_bits(u32::from_be_bytes(self.take(4)?.try_into().unwrap())) as f64),
                27 => Value::Float(f64::from_bits(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))),
                31 => return Ok(Item::Break),
                24 => Value::Int(self.byte()? as i64),
                0..=19 => Value::Int(info as i64),
                _ => return Err(df_err("Invalid CBOR simple value")),
            }));
        }
        let arg = self.arg(info)?;
        self.depth += 1;
        if self.depth > 512 {
            return Err(df_err("CBOR nesting too deep"));
        }
        let v = match (major, arg) {
            (0, Some(n)) => int_or_big(n as u128, false),
            (1, Some(n)) => int_or_big(n as u128, true),
            (2, Some(n)) => Value::Bytes(self.take(n)?.to_vec()),
            (2, None) => Value::Bytes(self.chunks(2)?),
            (3, Some(n)) => Value::String(String::from_utf8(self.take(n)?.to_vec()).map_err(|_| df_err("Invalid UTF-8 in CBOR text"))?),
            (3, None) => Value::String(String::from_utf8(self.chunks(3)?).map_err(|_| df_err("Invalid UTF-8 in CBOR text"))?),
            (4, n) => {
                let mut a = Vec::new();
                match n {
                    Some(n) => {
                        for _ in 0..n {
                            a.push(self.item()?);
                        }
                    }
                    None => loop {
                        match self.any()? {
                            Item::Val(v) => a.push(v),
                            Item::Break => break,
                        }
                    },
                }
                Value::Array(a)
            }
            (5, n) => {
                let mut m = ObjectMap::new();
                let mut i = 0u64;
                loop {
                    if let Some(n) = n {
                        if i == n {
                            break;
                        }
                    }
                    let k = match self.any()? {
                        Item::Val(v) => v,
                        Item::Break if n.is_none() => break,
                        Item::Break => return Err(df_err("Unexpected CBOR break")),
                    };
                    let v = self.item()?;
                    let key = match k {
                        Value::String(s) => s,
                        other => other.to_string(),
                    };
                    m.insert(key, v);
                    i += 1;
                }
                Value::Object(m)
            }
            (6, Some(tag)) => {
                let inner = self.item()?;
                match (tag, inner) {
                    (2, Value::Bytes(b)) => bignum(&b, false),
                    (3, Value::Bytes(b)) => bignum(&b, true),
                    (_, v) => v,
                }
            }
            _ => return Err(df_err("Invalid CBOR item")),
        };
        self.depth -= 1;
        Ok(Item::Val(v))
    }
}

fn int_or_big(n: u128, neg: bool) -> Value {
    if neg {
        let v = -1i128 - n as i128;
        if v >= i64::MIN as i128 {
            Value::Int(v as i64)
        } else {
            Value::String(v.to_string())
        }
    } else if n <= i64::MAX as u128 {
        Value::Int(n as i64)
    } else {
        Value::String(n.to_string())
    }
}

fn bignum(b: &[u8], neg: bool) -> Value {
    // decimal conversion of an arbitrary big-endian unsigned integer
    let mut digits: Vec<u8> = vec![0]; // little-endian base 10
    for byte in b {
        let mut carry = *byte as u32;
        for d in digits.iter_mut() {
            let x = *d as u32 * 256 + carry;
            *d = (x % 10) as u8;
            carry = x / 10;
        }
        while carry > 0 {
            digits.push((carry % 10) as u8);
            carry /= 10;
        }
    }
    let s: String = digits.iter().rev().map(|d| (b'0' + d) as char).collect::<String>();
    let s = s.trim_start_matches('0');
    let s = if s.is_empty() { "0" } else { s };
    if neg {
        // -1 - n
        match s.parse::<i128>() {
            Ok(n) if -1 - n >= i64::MIN as i128 => Value::Int((-1 - n) as i64),
            Ok(n) => Value::String((-1 - n).to_string()),
            Err(_) => Value::String(format!("-(1+{})", s)),
        }
    } else {
        match s.parse::<i64>() {
            Ok(n) => Value::Int(n),
            Err(_) => Value::String(s.to_string()),
        }
    }
}

/// The `cbor` data format.
pub struct CborDataFormat;

impl DataFormat for CborDataFormat {
    fn name(&self) -> &str {
        "cbor"
    }
    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        ex.message.body = Value::Bytes(encode(&ex.message.body));
        Ok(())
    }
    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        let b = body_bytes(&ex.message.body);
        ex.message.body = if b.is_empty() { Value::Null } else { decode(&b)? };
        Ok(())
    }
}

pub(crate) fn create(_o: &Opts) -> Result<Arc<dyn DataFormat>> {
    Ok(Arc::new(CborDataFormat))
}

//! The AMQP 1.0 type system (OASIS AMQP 1.0, part 1): every primitive type, described
//! types and the compound types list / map / array, with their binary encodings.
//!
//! Encoding always picks the most compact form (`uint0`, `smalluint`, `list0`, `list8`,
//! `str8`...); decoding accepts every form. Arrays are encoded with one element
//! constructor (the widest form needed by the elements).

use std::fmt;

/// Codec errors (truncated input, unknown constructor, invalid UTF-8...).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodeError(pub String);

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AMQP decode error: {}", self.0)
    }
}

impl std::error::Error for DecodeError {}

/// Codec result.
pub type DResult<T> = std::result::Result<T, DecodeError>;

fn err<T>(msg: impl Into<String>) -> DResult<T> {
    Err(DecodeError(msg.into()))
}

/// An AMQP 1.0 value.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum AmqpValue {
    #[default]
    Null,
    Boolean(bool),
    Ubyte(u8),
    Ushort(u16),
    Uint(u32),
    Ulong(u64),
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    /// IEEE 754-2008 decimal32 (raw bits).
    Decimal32(u32),
    /// IEEE 754-2008 decimal64 (raw bits).
    Decimal64(u64),
    /// IEEE 754-2008 decimal128 (raw bits).
    Decimal128(u128),
    Char(char),
    /// Milliseconds since the Unix epoch.
    Timestamp(i64),
    Uuid([u8; 16]),
    Binary(Vec<u8>),
    String(String),
    Symbol(String),
    List(Vec<AmqpValue>),
    /// A map keeps its insertion order (keys may be of any type).
    Map(Vec<(AmqpValue, AmqpValue)>),
    /// A homogeneous array.
    Array(Vec<AmqpValue>),
    /// A described value: `(descriptor, value)`.
    Described(Box<AmqpValue>, Box<AmqpValue>),
}

impl AmqpValue {
    /// `symbol` shorthand.
    pub fn sym(s: impl Into<String>) -> AmqpValue {
        AmqpValue::Symbol(s.into())
    }
    /// `string` shorthand.
    pub fn str(s: impl Into<String>) -> AmqpValue {
        AmqpValue::String(s.into())
    }
    /// A described value with a numeric (`ulong`) descriptor.
    pub fn described(code: u64, value: AmqpValue) -> AmqpValue {
        AmqpValue::Described(Box::new(AmqpValue::Ulong(code)), Box::new(value))
    }
    /// A described list (composite type) with trailing nulls removed.
    pub fn composite(code: u64, mut fields: Vec<AmqpValue>) -> AmqpValue {
        while matches!(fields.last(), Some(AmqpValue::Null)) {
            fields.pop();
        }
        AmqpValue::described(code, AmqpValue::List(fields))
    }
    pub fn is_null(&self) -> bool {
        matches!(self, AmqpValue::Null)
    }
    /// The numeric descriptor of a described value.
    pub fn descriptor_code(&self) -> Option<u64> {
        match self {
            AmqpValue::Described(d, _) => match &**d {
                AmqpValue::Ulong(c) => Some(*c),
                AmqpValue::Symbol(s) => symbolic_descriptor_code(s),
                _ => None,
            },
            _ => None,
        }
    }
    /// The symbolic descriptor of a described value.
    pub fn descriptor_symbol(&self) -> Option<&str> {
        match self {
            AmqpValue::Described(d, _) => match &**d {
                AmqpValue::Symbol(s) => Some(s),
                _ => None,
            },
            _ => None,
        }
    }
    /// The value of a described value (or `self`).
    pub fn undescribed(&self) -> &AmqpValue {
        match self {
            AmqpValue::Described(_, v) => v,
            v => v,
        }
    }
    /// Text of a string or symbol.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            AmqpValue::String(s) | AmqpValue::Symbol(s) => Some(s),
            _ => None,
        }
    }
    /// Any integer (or timestamp) as i64.
    pub fn as_i64(&self) -> Option<i64> {
        Some(match *self {
            AmqpValue::Ubyte(v) => v as i64,
            AmqpValue::Ushort(v) => v as i64,
            AmqpValue::Uint(v) => v as i64,
            AmqpValue::Ulong(v) => v as i64,
            AmqpValue::Byte(v) => v as i64,
            AmqpValue::Short(v) => v as i64,
            AmqpValue::Int(v) => v as i64,
            AmqpValue::Long(v) => v,
            AmqpValue::Timestamp(v) => v,
            _ => return None,
        })
    }
    /// Any non-negative integer as u64.
    pub fn as_u64(&self) -> Option<u64> {
        self.as_i64().filter(|v| *v >= 0).map(|v| v as u64).or(match self {
            AmqpValue::Ulong(v) => Some(*v),
            _ => None,
        })
    }
    pub fn as_u32(&self) -> Option<u32> {
        self.as_u64().map(|v| v.min(u32::MAX as u64) as u32)
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            AmqpValue::Boolean(b) => Some(*b),
            _ => None,
        }
    }
    pub fn as_binary(&self) -> Option<&[u8]> {
        match self {
            AmqpValue::Binary(b) => Some(b),
            _ => None,
        }
    }
    pub fn as_list(&self) -> Option<&[AmqpValue]> {
        match self {
            AmqpValue::List(l) | AmqpValue::Array(l) => Some(l),
            _ => None,
        }
    }
    pub fn as_map(&self) -> Option<&[(AmqpValue, AmqpValue)]> {
        match self {
            AmqpValue::Map(m) => Some(m),
            _ => None,
        }
    }
    /// Look a key up in a map value (keys compared as strings/symbols when `key` is text).
    pub fn map_get(&self, key: &str) -> Option<&AmqpValue> {
        self.as_map()?.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v)
    }

    /// Encode to bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_to(&mut out);
        out
    }

    /// Append the encoding to `out`.
    pub fn encode_to(&self, out: &mut Vec<u8>) {
        use AmqpValue::*;
        match self {
            Null => out.push(0x40),
            Boolean(b) => out.push(if *b { 0x41 } else { 0x42 }),
            Ubyte(v) => {
                out.push(0x50);
                out.push(*v);
            }
            Ushort(v) => {
                out.push(0x60);
                out.extend_from_slice(&v.to_be_bytes());
            }
            Uint(0) => out.push(0x43),
            Uint(v) if *v < 256 => {
                out.push(0x52);
                out.push(*v as u8);
            }
            Uint(v) => {
                out.push(0x70);
                out.extend_from_slice(&v.to_be_bytes());
            }
            Ulong(0) => out.push(0x44),
            Ulong(v) if *v < 256 => {
                out.push(0x53);
                out.push(*v as u8);
            }
            Ulong(v) => {
                out.push(0x80);
                out.extend_from_slice(&v.to_be_bytes());
            }
            Byte(v) => {
                out.push(0x51);
                out.push(*v as u8);
            }
            Short(v) => {
                out.push(0x61);
                out.extend_from_slice(&v.to_be_bytes());
            }
            Int(v) if (-128..=127).contains(v) => {
                out.push(0x54);
                out.push(*v as i8 as u8);
            }
            Int(v) => {
                out.push(0x71);
                out.extend_from_slice(&v.to_be_bytes());
            }
            Long(v) if (-128..=127).contains(v) => {
                out.push(0x55);
                out.push(*v as i8 as u8);
            }
            Long(v) => {
                out.push(0x81);
                out.extend_from_slice(&v.to_be_bytes());
            }
            Float(v) => {
                out.push(0x72);
                out.extend_from_slice(&v.to_bits().to_be_bytes());
            }
            Double(v) => {
                out.push(0x82);
                out.extend_from_slice(&v.to_bits().to_be_bytes());
            }
            Decimal32(v) => {
                out.push(0x74);
                out.extend_from_slice(&v.to_be_bytes());
            }
            Decimal64(v) => {
                out.push(0x84);
                out.extend_from_slice(&v.to_be_bytes());
            }
            Decimal128(v) => {
                out.push(0x94);
                out.extend_from_slice(&v.to_be_bytes());
            }
            Char(c) => {
                out.push(0x73);
                out.extend_from_slice(&(*c as u32).to_be_bytes());
            }
            Timestamp(v) => {
                out.push(0x83);
                out.extend_from_slice(&v.to_be_bytes());
            }
            Uuid(u) => {
                out.push(0x98);
                out.extend_from_slice(u);
            }
            Binary(b) => var(out, 0xa0, 0xb0, b),
            String(s) => var(out, 0xa1, 0xb1, s.as_bytes()),
            Symbol(s) => var(out, 0xa3, 0xb3, s.as_bytes()),
            List(items) if items.is_empty() => out.push(0x45),
            List(items) => {
                let mut body = Vec::new();
                for i in items {
                    i.encode_to(&mut body);
                }
                compound(out, 0xc0, 0xd0, items.len(), &body);
            }
            Map(pairs) => {
                let mut body = Vec::new();
                for (k, v) in pairs {
                    k.encode_to(&mut body);
                    v.encode_to(&mut body);
                }
                compound(out, 0xc1, 0xd1, pairs.len() * 2, &body);
            }
            Array(items) => {
                let ctor = array_constructor(items);
                let mut body = ctor.bytes.clone();
                for i in items {
                    encode_element(i, &ctor, &mut body);
                }
                compound(out, 0xe0, 0xf0, items.len(), &body);
            }
            Described(d, v) => {
                out.push(0x00);
                d.encode_to(out);
                v.encode_to(out);
            }
        }
    }

    /// Decode one value from the start of `buf`; returns the value and the bytes used.
    pub fn decode(buf: &[u8]) -> DResult<(AmqpValue, usize)> {
        let mut d = Decoder::new(buf);
        let v = d.read_value()?;
        Ok((v, d.pos))
    }
}

/// Codes of the symbolic descriptors this crate knows (`amqp:open:list`...).
fn symbolic_descriptor_code(s: &str) -> Option<u64> {
    Some(match s {
        "amqp:open:list" => 0x10,
        "amqp:begin:list" => 0x11,
        "amqp:attach:list" => 0x12,
        "amqp:flow:list" => 0x13,
        "amqp:transfer:list" => 0x14,
        "amqp:disposition:list" => 0x15,
        "amqp:detach:list" => 0x16,
        "amqp:end:list" => 0x17,
        "amqp:close:list" => 0x18,
        "amqp:error:list" => 0x1d,
        "amqp:received:list" => 0x23,
        "amqp:accepted:list" => 0x24,
        "amqp:rejected:list" => 0x25,
        "amqp:released:list" => 0x26,
        "amqp:modified:list" => 0x27,
        "amqp:source:list" => 0x28,
        "amqp:target:list" => 0x29,
        "amqp:sasl-mechanisms:list" => 0x40,
        "amqp:sasl-init:list" => 0x41,
        "amqp:sasl-challenge:list" => 0x42,
        "amqp:sasl-response:list" => 0x43,
        "amqp:sasl-outcome:list" => 0x44,
        "amqp:header:list" => 0x70,
        "amqp:delivery-annotations:map" => 0x71,
        "amqp:message-annotations:map" => 0x72,
        "amqp:properties:list" => 0x73,
        "amqp:application-properties:map" => 0x74,
        "amqp:data:binary" => 0x75,
        "amqp:amqp-sequence:list" => 0x76,
        "amqp:amqp-value:*" => 0x77,
        "amqp:footer:map" => 0x78,
        _ => return None,
    })
}

fn var(out: &mut Vec<u8>, small: u8, large: u8, data: &[u8]) {
    if data.len() < 256 {
        out.push(small);
        out.push(data.len() as u8);
    } else {
        out.push(large);
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    }
    out.extend_from_slice(data);
}

fn compound(out: &mut Vec<u8>, small: u8, large: u8, count: usize, body: &[u8]) {
    if body.len() + 1 < 256 && count < 256 {
        out.push(small);
        out.push((body.len() + 1) as u8);
        out.push(count as u8);
    } else {
        out.push(large);
        out.extend_from_slice(&((body.len() + 4) as u32).to_be_bytes());
        out.extend_from_slice(&(count as u32).to_be_bytes());
    }
    out.extend_from_slice(body);
}

/// The element constructor of an array: its bytes, the primitive code, and for
/// described elements the inner constructor.
#[derive(Clone, Debug)]
struct ArrayCtor {
    bytes: Vec<u8>,
    code: u8,
    inner: Option<Box<ArrayCtor>>,
}

fn array_constructor(items: &[AmqpValue]) -> ArrayCtor {
    use AmqpValue::*;
    let Some(first) = items.first() else {
        return ArrayCtor { bytes: vec![0xa3], code: 0xa3, inner: None };
    };
    let all_small = |f: &dyn Fn(&AmqpValue) -> usize| items.iter().all(|i| f(i) < 256);
    let code = match first {
        Null => 0x40,
        Boolean(_) => 0x56,
        Ubyte(_) => 0x50,
        Ushort(_) => 0x60,
        Uint(_) => 0x70,
        Ulong(_) => 0x80,
        Byte(_) => 0x51,
        Short(_) => 0x61,
        Int(_) => 0x71,
        Long(_) => 0x81,
        Float(_) => 0x72,
        Double(_) => 0x82,
        Decimal32(_) => 0x74,
        Decimal64(_) => 0x84,
        Decimal128(_) => 0x94,
        Char(_) => 0x73,
        Timestamp(_) => 0x83,
        Uuid(_) => 0x98,
        Binary(_) => {
            if all_small(&|i| i.as_binary().map(|b| b.len()).unwrap_or(0)) {
                0xa0
            } else {
                0xb0
            }
        }
        String(_) => {
            if all_small(&|i| i.as_str().map(|b| b.len()).unwrap_or(0)) {
                0xa1
            } else {
                0xb1
            }
        }
        Symbol(_) => {
            if all_small(&|i| i.as_str().map(|b| b.len()).unwrap_or(0)) {
                0xa3
            } else {
                0xb3
            }
        }
        List(_) => 0xd0,
        Map(_) => 0xd1,
        Array(_) => 0xf0,
        Described(d, _) => {
            let inner: Vec<AmqpValue> = items.iter().map(|i| i.undescribed().clone()).collect();
            let ic = array_constructor(&inner);
            let mut bytes = vec![0x00];
            d.encode_to(&mut bytes);
            bytes.extend_from_slice(&ic.bytes);
            return ArrayCtor { bytes, code: 0x00, inner: Some(Box::new(ic)) };
        }
    };
    ArrayCtor { bytes: vec![code], code, inner: None }
}

fn encode_element(v: &AmqpValue, ctor: &ArrayCtor, out: &mut Vec<u8>) {
    use AmqpValue::*;
    if let Some(inner) = &ctor.inner {
        encode_element(v.undescribed(), inner, out);
        return;
    }
    match (ctor.code, v) {
        (0x56, Boolean(b)) => out.push(*b as u8),
        (0xa0, Binary(b)) => {
            out.push(b.len() as u8);
            out.extend_from_slice(b);
        }
        (0xb0, Binary(b)) => {
            out.extend_from_slice(&(b.len() as u32).to_be_bytes());
            out.extend_from_slice(b);
        }
        (0xa1 | 0xa3, String(s) | Symbol(s)) => {
            out.push(s.len() as u8);
            out.extend_from_slice(s.as_bytes());
        }
        (0xb1 | 0xb3, String(s) | Symbol(s)) => {
            out.extend_from_slice(&(s.len() as u32).to_be_bytes());
            out.extend_from_slice(s.as_bytes());
        }
        (0xd0, List(items)) => {
            let mut body = Vec::new();
            for i in items {
                i.encode_to(&mut body);
            }
            out.extend_from_slice(&((body.len() + 4) as u32).to_be_bytes());
            out.extend_from_slice(&(items.len() as u32).to_be_bytes());
            out.extend_from_slice(&body);
        }
        (0xd1, Map(pairs)) => {
            let mut body = Vec::new();
            for (k, v) in pairs {
                k.encode_to(&mut body);
                v.encode_to(&mut body);
            }
            out.extend_from_slice(&((body.len() + 4) as u32).to_be_bytes());
            out.extend_from_slice(&((pairs.len() * 2) as u32).to_be_bytes());
            out.extend_from_slice(&body);
        }
        (0xf0, Array(items)) => {
            let c = array_constructor(items);
            let mut body = c.bytes.clone();
            for i in items {
                encode_element(i, &c, &mut body);
            }
            out.extend_from_slice(&((body.len() + 4) as u32).to_be_bytes());
            out.extend_from_slice(&(items.len() as u32).to_be_bytes());
            out.extend_from_slice(&body);
        }
        (0x40, _) => {}
        (_, v) => {
            // fixed-width primitives: the full encoding minus the constructor byte,
            // widened to the array's (wide) code
            let widened = match v {
                Uint(x) => Uint(*x).wide(),
                Ulong(x) => Ulong(*x).wide(),
                Int(x) => Int(*x).wide(),
                Long(x) => Long(*x).wide(),
                other => other.encode(),
            };
            out.extend_from_slice(&widened[1..]);
        }
    }
}

impl AmqpValue {
    /// Wide (fixed-size) encoding of integer types.
    fn wide(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            AmqpValue::Uint(v) => {
                out.push(0x70);
                out.extend_from_slice(&v.to_be_bytes());
            }
            AmqpValue::Ulong(v) => {
                out.push(0x80);
                out.extend_from_slice(&v.to_be_bytes());
            }
            AmqpValue::Int(v) => {
                out.push(0x71);
                out.extend_from_slice(&v.to_be_bytes());
            }
            AmqpValue::Long(v) => {
                out.push(0x81);
                out.extend_from_slice(&v.to_be_bytes());
            }
            other => return other.encode(),
        }
        out
    }
}

/// A constructor read from the wire.
enum Ctor {
    Primitive(u8),
    Described(AmqpValue, Box<Ctor>),
}

/// A streaming decoder over a byte slice.
pub struct Decoder<'a> {
    buf: &'a [u8],
    /// Current read position.
    pub pos: usize,
}

impl<'a> Decoder<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Decoder { buf, pos: 0 }
    }
    /// Whether all input was consumed.
    pub fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }
    /// The unread input.
    pub fn rest(&self) -> &'a [u8] {
        &self.buf[self.pos.min(self.buf.len())..]
    }

    fn take(&mut self, n: usize) -> DResult<&'a [u8]> {
        if self.buf.len() - self.pos.min(self.buf.len()) < n {
            return err(format!("truncated input (need {} bytes at offset {})", n, self.pos));
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u8(&mut self) -> DResult<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> DResult<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> DResult<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> DResult<u64> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn read_ctor(&mut self) -> DResult<Ctor> {
        let code = self.u8()?;
        if code == 0x00 {
            let descriptor = self.read_value()?;
            let inner = self.read_ctor()?;
            return Ok(Ctor::Described(descriptor, Box::new(inner)));
        }
        Ok(Ctor::Primitive(code))
    }

    /// Read one value.
    pub fn read_value(&mut self) -> DResult<AmqpValue> {
        let c = self.read_ctor()?;
        self.read_body(&c)
    }

    fn read_body(&mut self, c: &Ctor) -> DResult<AmqpValue> {
        match c {
            Ctor::Described(d, inner) => Ok(AmqpValue::Described(Box::new(d.clone()), Box::new(self.read_body(inner)?))),
            Ctor::Primitive(code) => self.read_primitive(*code),
        }
    }

    fn string(&mut self, n: usize) -> DResult<String> {
        let b = self.take(n)?;
        String::from_utf8(b.to_vec()).map_err(|_| DecodeError("invalid UTF-8 in string".into()))
    }

    fn read_primitive(&mut self, code: u8) -> DResult<AmqpValue> {
        use AmqpValue::*;
        Ok(match code {
            0x40 => Null,
            0x41 => Boolean(true),
            0x42 => Boolean(false),
            0x56 => match self.u8()? {
                0 => Boolean(false),
                1 => Boolean(true),
                b => return err(format!("invalid boolean {}", b)),
            },
            0x50 => Ubyte(self.u8()?),
            0x60 => Ushort(self.u16()?),
            0x70 => Uint(self.u32()?),
            0x52 => Uint(self.u8()? as u32),
            0x43 => Uint(0),
            0x80 => Ulong(self.u64()?),
            0x53 => Ulong(self.u8()? as u64),
            0x44 => Ulong(0),
            0x51 => Byte(self.u8()? as i8),
            0x61 => Short(self.u16()? as i16),
            0x71 => Int(self.u32()? as i32),
            0x54 => Int(self.u8()? as i8 as i32),
            0x81 => Long(self.u64()? as i64),
            0x55 => Long(self.u8()? as i8 as i64),
            0x72 => Float(f32::from_bits(self.u32()?)),
            0x82 => Double(f64::from_bits(self.u64()?)),
            0x74 => Decimal32(self.u32()?),
            0x84 => Decimal64(self.u64()?),
            0x94 => Decimal128(u128::from_be_bytes(self.take(16)?.try_into().unwrap())),
            0x73 => Char(char::from_u32(self.u32()?).ok_or_else(|| DecodeError("invalid char".into()))?),
            0x83 => Timestamp(self.u64()? as i64),
            0x98 => Uuid(self.take(16)?.try_into().unwrap()),
            0xa0 => {
                let n = self.u8()? as usize;
                Binary(self.take(n)?.to_vec())
            }
            0xb0 => {
                let n = self.u32()? as usize;
                Binary(self.take(n)?.to_vec())
            }
            0xa1 => {
                let n = self.u8()? as usize;
                String(self.string(n)?)
            }
            0xb1 => {
                let n = self.u32()? as usize;
                String(self.string(n)?)
            }
            0xa3 => {
                let n = self.u8()? as usize;
                Symbol(self.string(n)?)
            }
            0xb3 => {
                let n = self.u32()? as usize;
                Symbol(self.string(n)?)
            }
            0x45 => List(Vec::new()),
            0xc0 | 0xd0 => {
                let (size, count) = if code == 0xc0 { (self.u8()? as usize, self.u8()? as usize) } else { (self.u32()? as usize, self.u32()? as usize) };
                let end = self.compound_end(code, size)?;
                let mut items = Vec::with_capacity(count.min(1024));
                for _ in 0..count {
                    items.push(self.read_value()?);
                }
                self.check_end(end)?;
                List(items)
            }
            0xc1 | 0xd1 => {
                let (size, count) = if code == 0xc1 { (self.u8()? as usize, self.u8()? as usize) } else { (self.u32()? as usize, self.u32()? as usize) };
                if count % 2 != 0 {
                    return err("map with an odd number of elements");
                }
                let end = self.compound_end(code, size)?;
                let mut pairs = Vec::with_capacity((count / 2).min(1024));
                for _ in 0..count / 2 {
                    let k = self.read_value()?;
                    let v = self.read_value()?;
                    pairs.push((k, v));
                }
                self.check_end(end)?;
                Map(pairs)
            }
            0xe0 | 0xf0 => {
                let (size, count) = if code == 0xe0 { (self.u8()? as usize, self.u8()? as usize) } else { (self.u32()? as usize, self.u32()? as usize) };
                let end = self.compound_end(code, size)?;
                let ctor = self.read_ctor()?;
                let mut items = Vec::with_capacity(count.min(1024));
                for _ in 0..count {
                    items.push(self.read_body(&ctor)?);
                }
                self.check_end(end)?;
                Array(items)
            }
            other => return err(format!("unknown constructor 0x{:02x}", other)),
        })
    }

    /// End position of a compound whose size field was just read (the size counts the
    /// count field, already consumed).
    fn compound_end(&self, code: u8, size: usize) -> DResult<usize> {
        let count_width = if code & 0xf0 == 0xc0 || code & 0xf0 == 0xe0 { 1 } else { 4 };
        if size < count_width {
            return err("compound size smaller than its count field");
        }
        let end = self.pos + size - count_width;
        if end > self.buf.len() {
            return err("truncated compound");
        }
        Ok(end)
    }

    fn check_end(&mut self, end: usize) -> DResult<()> {
        if self.pos != end {
            return err(format!("compound size mismatch (at {}, expected {})", self.pos, end));
        }
        Ok(())
    }
}

/// Read a field of a composite list: missing fields are `Null`.
pub fn field(fields: &[AmqpValue], i: usize) -> &AmqpValue {
    static NULL: AmqpValue = AmqpValue::Null;
    fields.get(i).unwrap_or(&NULL)
}

/// A "multiple" symbol field: a single symbol or an array/list of symbols.
pub fn symbols(v: &AmqpValue) -> Vec<String> {
    match v {
        AmqpValue::Symbol(s) | AmqpValue::String(s) => vec![s.clone()],
        AmqpValue::Array(a) | AmqpValue::List(a) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        _ => Vec::new(),
    }
}

/// Encode a "multiple" symbol field (null when empty, an array of symbols otherwise).
pub fn symbols_value(v: &[String]) -> AmqpValue {
    if v.is_empty() {
        AmqpValue::Null
    } else {
        AmqpValue::Array(v.iter().map(|s| AmqpValue::sym(s.clone())).collect())
    }
}

/// Helpers converting optional values to AMQP values.
pub fn opt_uint(v: Option<u32>) -> AmqpValue {
    v.map(AmqpValue::Uint).unwrap_or(AmqpValue::Null)
}
pub fn opt_str(v: &Option<String>) -> AmqpValue {
    v.clone().map(AmqpValue::String).unwrap_or(AmqpValue::Null)
}
pub fn opt_sym(v: &Option<String>) -> AmqpValue {
    v.clone().map(AmqpValue::Symbol).unwrap_or(AmqpValue::Null)
}
pub fn map_value(m: &[(AmqpValue, AmqpValue)]) -> AmqpValue {
    if m.is_empty() {
        AmqpValue::Null
    } else {
        AmqpValue::Map(m.to_vec())
    }
}
pub fn map_of(v: &AmqpValue) -> Vec<(AmqpValue, AmqpValue)> {
    v.as_map().map(|m| m.to_vec()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use AmqpValue::*;

    fn str(s: impl Into<std::string::String>) -> AmqpValue {
        AmqpValue::str(s)
    }
    fn sym(s: impl Into<std::string::String>) -> AmqpValue {
        AmqpValue::sym(s)
    }

    fn rt(v: AmqpValue) {
        let bytes = v.encode();
        let (back, used) = AmqpValue::decode(&bytes).unwrap();
        assert_eq!(used, bytes.len(), "{:?}", v);
        assert_eq!(back, v);
    }

    #[test]
    fn known_encodings() {
        assert_eq!(Null.encode(), [0x40]);
        assert_eq!(Boolean(true).encode(), [0x41]);
        assert_eq!(Boolean(false).encode(), [0x42]);
        assert_eq!(Uint(0).encode(), [0x43]);
        assert_eq!(Uint(7).encode(), [0x52, 7]);
        assert_eq!(Uint(256).encode(), [0x70, 0, 0, 1, 0]);
        assert_eq!(Ulong(0).encode(), [0x44]);
        assert_eq!(Ulong(0x10).encode(), [0x53, 0x10]);
        assert_eq!(Int(-1).encode(), [0x54, 0xff]);
        assert_eq!(Long(1000).encode(), [0x81, 0, 0, 0, 0, 0, 0, 0x03, 0xe8]);
        assert_eq!(str("ab").encode(), [0xa1, 2, b'a', b'b']);
        assert_eq!(sym("x").encode(), [0xa3, 1, b'x']);
        assert_eq!(Binary(vec![1, 2]).encode(), [0xa0, 2, 1, 2]);
        assert_eq!(List(vec![]).encode(), [0x45]);
        assert_eq!(List(vec![Uint(1), Null]).encode(), [0xc0, 4, 2, 0x52, 1, 0x40]);
        assert_eq!(Map(vec![(sym("a"), Int(1))]).encode(), [0xc1, 6, 2, 0xa3, 1, b'a', 0x54, 1]);
        assert_eq!(Array(vec![sym("a"), sym("bc")]).encode(), [0xe0, 7, 2, 0xa3, 1, b'a', 2, b'b', b'c']);
        assert_eq!(Array(vec![Uint(1), Uint(2)]).encode(), [0xe0, 10, 2, 0x70, 0, 0, 0, 1, 0, 0, 0, 2]);
        assert_eq!(Timestamp(1).encode(), [0x83, 0, 0, 0, 0, 0, 0, 0, 1]);
        assert_eq!(Char('A').encode(), [0x73, 0, 0, 0, 0x41]);
        // an empty open performative: described list0 with descriptor smallulong 0x10
        assert_eq!(AmqpValue::composite(0x10, vec![Null, Null]).encode(), [0x00, 0x53, 0x10, 0x45]);
        // open(container-id="c")
        assert_eq!(AmqpValue::composite(0x10, vec![str("c")]).encode(), [0x00, 0x53, 0x10, 0xc0, 4, 1, 0xa1, 1, b'c']);
    }

    #[test]
    fn decodes_every_form() {
        let cases: Vec<(&[u8], AmqpValue)> = vec![
            (&[0x56, 1], Boolean(true)),
            (&[0x50, 9], Ubyte(9)),
            (&[0x60, 1, 0], Ushort(256)),
            (&[0x70, 0, 0, 0, 5], Uint(5)),
            (&[0x80, 0, 0, 0, 0, 0, 0, 0, 5], Ulong(5)),
            (&[0x51, 0xfe], Byte(-2)),
            (&[0x61, 0xff, 0xfe], Short(-2)),
            (&[0x71, 0, 0, 0, 5], Int(5)),
            (&[0x55, 0xfb], Long(-5)),
            (&[0xb1, 0, 0, 0, 1, b'z'], str("z")),
            (&[0xb3, 0, 0, 0, 1, b'z'], sym("z")),
            (&[0xb0, 0, 0, 0, 1, 7], Binary(vec![7])),
            (&[0xd0, 0, 0, 0, 5, 0, 0, 0, 1, 0x40], List(vec![Null])),
            (&[0xd1, 0, 0, 0, 6, 0, 0, 0, 2, 0x40, 0x41], Map(vec![(Null, Boolean(true))])),
            (&[0xf0, 0, 0, 0, 7, 0, 0, 0, 2, 0x56, 1, 0], Array(vec![Boolean(true), Boolean(false)])),
            (&[0x00, 0xa3, 1, b'd', 0x40], Described(Box::new(sym("d")), Box::new(Null))),
        ];
        for (bytes, v) in cases {
            let (got, used) = AmqpValue::decode(bytes).unwrap();
            assert_eq!(got, v);
            assert_eq!(used, bytes.len());
        }
        assert!(AmqpValue::decode(&[0xa1, 5, b'a']).is_err());
        assert!(AmqpValue::decode(&[0xff]).is_err());
        assert!(AmqpValue::decode(&[0xc0, 3, 1, 0x40]).is_err());
    }

    #[test]
    fn round_trips() {
        let big = "x".repeat(300);
        for v in [
            Null,
            Boolean(false),
            Ubyte(255),
            Ushort(65535),
            Uint(u32::MAX),
            Ulong(u64::MAX),
            Byte(-128),
            Short(i16::MIN),
            Int(i32::MIN),
            Int(100),
            Long(i64::MIN),
            Float(1.5),
            Double(-2.25),
            Decimal32(0x1234_5678),
            Decimal64(42),
            Decimal128(u128::MAX - 3),
            Char('é'),
            Timestamp(1_700_000_000_000),
            Uuid([7; 16]),
            Binary(vec![0; 300]),
            str(big.clone()),
            sym(big.clone()),
            List((0..300).map(Int).collect()),
            Map(vec![(str("k"), List(vec![Null, str("v")])), (Ulong(1), Map(vec![]))]),
            Array(vec![str("a"), str(big.clone())]),
            Array(vec![Long(1), Long(-1_000_000_000_000)]),
            Array(vec![Int(1), Int(-70000)]),
            Array(vec![Ulong(1), Ulong(1 << 40)]),
            Array(vec![Binary(vec![1]), Binary(vec![2; 400])]),
            Array(vec![List(vec![Int(1)]), List(vec![])]),
            Array(vec![Map(vec![(sym("a"), Null)])]),
            Array(vec![Array(vec![Uint(3)])]),
            Array(vec![Uuid([1; 16]), Uuid([2; 16])]),
            Array(vec![Double(1.0), Double(2.0)]),
            Array(vec![Boolean(true)]),
            Array(vec![AmqpValue::described(5, str("a")), AmqpValue::described(5, str("b"))]),
            AmqpValue::composite(0x12, vec![str("link"), Uint(0), Boolean(true)]),
        ] {
            rt(v);
        }
    }
}

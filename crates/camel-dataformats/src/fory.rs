//! `fory` data format (Apache Fory, formerly Fury): `Value` <-> Fory **xlang** binary.
//!
//! Implements the cross-language serialization protocol of the Fory specification for
//! dynamic values:
//!
//! * protocol edition (`xlangSpec`): `CURRENT` (default) follows the current xlang
//!   specification — one header byte (bit 0 xlang, bit 1 out-of-band), the renumbered internal
//!   type id table (`BOOL` 1 .. `TAGGED_INT64` 8, `UINT8`..`TAGGED_UINT64` 9-15, `FLOAT8` 16,
//!   `FLOAT16` 17, `BFLOAT16` 18, `FLOAT32` 19, `FLOAT64` 20, `STRING` 21, `LIST` 22, `SET` 23,
//!   `MAP` 24, `ENUM` 25 .. `NAMED_EXT` 32, unions 33-35, `NONE` 36, `DURATION` 37,
//!   `TIMESTAMP` 38, `DATE` 39, `DECIMAL` 40, `BINARY` 41, arrays 42-56) and numeric user type
//!   ids written as a separate varuint32 after the internal id. `LEGACY` writes the Fory
//!   0.10/0.11 layout described below. Readers detect the edition from the header byte.
//! * legacy header: bitmap byte (bit 0 null root, bit 1 little endian, bit 2 xlang, bit 3
//!   out-of-band) + language byte (written as `RUST` = 6). On read a leading legacy magic number
//!   (`0x62d4`, Fury <= 0.10) is accepted and skipped; non-xlang (Java native) payloads are
//!   rejected.
//! * reference meta: `NULL_FLAG` (-3), `REF_FLAG` (-2, followed by a varuint ref id),
//!   `NOT_NULL_VALUE_FLAG` (-1) and `REF_VALUE_FLAG` (0). With `refTracking=true` lists, maps,
//!   structs and binaries are written with `REF_VALUE_FLAG` (and identical strings/containers
//!   occurring again are written as back references); reading resolves `REF_FLAG`s.
//! * legacy type ids (varuint32, `(user_id << 8) | internal_id`): `BOOL` 1, `INT8` 2, `INT16` 3, `INT32` 4, `VAR_INT32` 5, `INT64` 6,
//!   `VAR_INT64` 7, `SLI_INT64` 8, `FLOAT16` 9, `FLOAT32` 10, `FLOAT64` 11, `STRING` 12,
//!   `ENUM` 13, `NAMED_ENUM` 14, `STRUCT` 15, `NAMED_STRUCT` 17, `LIST` 21, `SET` 22, `MAP` 23,
//!   `DURATION` 24, `TIMESTAMP` 25, `LOCAL_DATE` 26, `BINARY` 28, primitive arrays 30..37.
//! * encodings: LEB128 varuint32, zigzag varint32/64, the 9-byte varuint64 (8 groups of 7 bits
//!   and a final full byte), `SLI_INT64`; strings with a `(byteLength << 2) | coder` header
//!   (coder 0 LATIN1, 1 UTF16, 2 UTF8: Latin-1 is written when every char fits, UTF-8 otherwise);
//!   lists with an elements header (track ref / has null / declared type / same type);
//!   maps in chunks (`KV header` + chunk size <= 255, null keys/values in single-entry chunks).
//! * meta strings for namespaces / type names: UTF8, LOWER_SPECIAL (5 bit),
//!   LOWER_UPPER_DIGIT_SPECIAL (6 bit), FIRST_TO_LOWER_SPECIAL and ALL_TO_LOWER_SPECIAL, with the
//!   murmur3 x64-128 hash header for strings longer than 16 bytes and meta string back references.
//!
//! `Value` mapping: `Null` ⇄ null flag, `Bool` ⇄ BOOL, `Int` → VAR_INT64 (all integer types
//! read as `Int`), `Float` → FLOAT64 (FLOAT16/32 read as `Float`), `String` ⇄ STRING,
//! `Bytes` ⇄ BINARY, `Array` ⇄ LIST (SET and primitive arrays read as arrays), `Object` ⇄ MAP,
//! except objects carrying a `@type` key (`"namespace.TypeName"`): they are written as a
//! `NAMED_STRUCT` whose fields (every key but `@type`) are written in snake_case-sorted order,
//! each as a nullable, typed value. As the schema-consistent struct layout carries no field
//! names, reading a struct requires its field list: option `structTypes`
//! (`example.Person=name,age;example.Address=city`). ENUM reads its ordinal, LOCAL_DATE an
//! ISO date string, TIMESTAMP / DURATION their integer counts.
//!
//! Camel options: `requireClassRegistration` (default false here: with `true`, unmarshalling a
//! struct not listed in `structTypes` fails, without it fails too as the field names are
//! unknown — the option mostly matters for marshalling, where `true` requires every `@type` to
//! be listed), `threadSafe` (ignored: the format is stateless), `allowAutoWiredFory` (accepted;
//! there is no Java `Fory` bean to wire), `unmarshalType` (accepted, ignored), plus
//! `refTracking` (default false).
//!
//! Compatible mode (`compatible=true` or `compatibleMode=COMPATIBLE`): `@type` objects are
//! written as `NAMED_COMPATIBLE_STRUCT` followed by a shared TypeDef (meta share marker
//! `(index << 1) | flag`: a new definition — 8-byte global header with body size and murmur3
//! hash, struct meta header, namespace/type name, field infos with name encoding, size,
//! nullable flag and field type — or a reference to an earlier one); field values follow in
//! definition order (primitives with a null flag + raw data, others as ref values). Readers
//! match fields by the names in the TypeDef, so no `structTypes` is needed.
//! `DECIMAL`: objects `{"@decimal": "123.45"}` are written with the scale / small-or-big
//! unscaled encoding; decimals read as their plain string. Extension types: objects
//! `{"@ext": "ns.Name", ...}` are written as `NAMED_EXT` by an [`ExtSerializer`] registered in
//! [`ForyConfig::extensions`] (Rust API), which also reads them back.
//!
//! Deviations from the Java implementation: only the xlang protocol is implemented — the Java
//! native mode (class names, JDK serializers, `Externalizable`, ...) has no meaning for
//! dynamic values; out-of-band buffers are rejected; `FLOAT8` and union types (current spec) are
//! not supported; numeric ids of registered (unnamed) structs are read through `structTypes`
//! entries named `#id`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use camel_core::dataformat::DataFormat;
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::ObjectMap;
use camel_core::Value;

use crate::{body_bytes, df_err, Opts};

/// Reference flags.
pub const NULL_FLAG: i8 = -3;
pub const REF_FLAG: i8 = -2;
pub const NOT_NULL_VALUE_FLAG: i8 = -1;
pub const REF_VALUE_FLAG: i8 = 0;

/// Xlang type ids.
pub mod types {
    pub const BOOL: u32 = 1;
    pub const INT8: u32 = 2;
    pub const INT16: u32 = 3;
    pub const INT32: u32 = 4;
    pub const VAR_INT32: u32 = 5;
    pub const INT64: u32 = 6;
    pub const VAR_INT64: u32 = 7;
    pub const SLI_INT64: u32 = 8;
    pub const FLOAT16: u32 = 9;
    pub const FLOAT32: u32 = 10;
    pub const FLOAT64: u32 = 11;
    pub const STRING: u32 = 12;
    pub const ENUM: u32 = 13;
    pub const NAMED_ENUM: u32 = 14;
    pub const STRUCT: u32 = 15;
    pub const COMPATIBLE_STRUCT: u32 = 16;
    pub const NAMED_STRUCT: u32 = 17;
    pub const NAMED_COMPATIBLE_STRUCT: u32 = 18;
    pub const EXT: u32 = 19;
    pub const NAMED_EXT: u32 = 20;
    pub const LIST: u32 = 21;
    pub const SET: u32 = 22;
    pub const MAP: u32 = 23;
    pub const DURATION: u32 = 24;
    pub const TIMESTAMP: u32 = 25;
    pub const LOCAL_DATE: u32 = 26;
    pub const DECIMAL: u32 = 27;
    pub const BINARY: u32 = 28;
    pub const ARRAY: u32 = 29;
    pub const BOOL_ARRAY: u32 = 30;
    pub const INT8_ARRAY: u32 = 31;
    pub const INT16_ARRAY: u32 = 32;
    pub const INT32_ARRAY: u32 = 33;
    pub const INT64_ARRAY: u32 = 34;
    pub const FLOAT16_ARRAY: u32 = 35;
    pub const FLOAT32_ARRAY: u32 = 36;
    pub const FLOAT64_ARRAY: u32 = 37;
    // types only present in the current xlang spec (canonical ids local to this crate; the
    // wire ids come from [`super::XlangSpec`])
    pub const UINT8: u32 = 64;
    pub const UINT16: u32 = 65;
    pub const UINT32: u32 = 66;
    pub const VAR_UINT32: u32 = 67;
    pub const UINT64: u32 = 68;
    pub const VAR_UINT64: u32 = 69;
    pub const TAGGED_UINT64: u32 = 70;
    pub const FLOAT8: u32 = 71;
    pub const BFLOAT16: u32 = 72;
    pub const UNION: u32 = 73;
    pub const TYPED_UNION: u32 = 74;
    pub const NAMED_UNION: u32 = 75;
    pub const NONE: u32 = 76;
    pub const UINT8_ARRAY: u32 = 77;
    pub const UINT16_ARRAY: u32 = 78;
    pub const UINT32_ARRAY: u32 = 79;
    pub const UINT64_ARRAY: u32 = 80;
    pub const FLOAT8_ARRAY: u32 = 81;
    pub const BFLOAT16_ARRAY: u32 = 82;
}

/// Which edition of the xlang protocol is written (`xlangSpec`): the current specification
/// (one-byte header with the xlang / out-of-band bits, the renumbered internal type id table
/// incl. unsigned and bfloat16 types, user type ids written after the internal id) or the
/// Fory 0.10/0.11 one (bitmap + language byte, `(user_id << 8) | internal_id`). Readers detect
/// the edition from the header.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum XlangSpec {
    #[default]
    Current,
    Legacy,
}

/// (canonical id, id in the current spec table)
const CURRENT_IDS: [(u32, u32); 57] = [
    (types::BOOL, 1),
    (types::INT8, 2),
    (types::INT16, 3),
    (types::INT32, 4),
    (types::VAR_INT32, 5),
    (types::INT64, 6),
    (types::VAR_INT64, 7),
    (types::SLI_INT64, 8),
    (types::UINT8, 9),
    (types::UINT16, 10),
    (types::UINT32, 11),
    (types::VAR_UINT32, 12),
    (types::UINT64, 13),
    (types::VAR_UINT64, 14),
    (types::TAGGED_UINT64, 15),
    (types::FLOAT8, 16),
    (types::FLOAT16, 17),
    (types::BFLOAT16, 18),
    (types::FLOAT32, 19),
    (types::FLOAT64, 20),
    (types::STRING, 21),
    (types::LIST, 22),
    (types::SET, 23),
    (types::MAP, 24),
    (types::ENUM, 25),
    (types::NAMED_ENUM, 26),
    (types::STRUCT, 27),
    (types::COMPATIBLE_STRUCT, 28),
    (types::NAMED_STRUCT, 29),
    (types::NAMED_COMPATIBLE_STRUCT, 30),
    (types::EXT, 31),
    (types::NAMED_EXT, 32),
    (types::UNION, 33),
    (types::TYPED_UNION, 34),
    (types::NAMED_UNION, 35),
    (types::NONE, 36),
    (types::DURATION, 37),
    (types::TIMESTAMP, 38),
    (types::LOCAL_DATE, 39),
    (types::DECIMAL, 40),
    (types::BINARY, 41),
    (types::ARRAY, 42),
    (types::BOOL_ARRAY, 43),
    (types::INT8_ARRAY, 44),
    (types::INT16_ARRAY, 45),
    (types::INT32_ARRAY, 46),
    (types::INT64_ARRAY, 47),
    (types::UINT8_ARRAY, 48),
    (types::UINT16_ARRAY, 49),
    (types::UINT32_ARRAY, 50),
    (types::UINT64_ARRAY, 51),
    (types::FLOAT8_ARRAY, 52),
    (types::FLOAT16_ARRAY, 53),
    (types::BFLOAT16_ARRAY, 54),
    (types::FLOAT32_ARRAY, 55),
    (types::FLOAT64_ARRAY, 56),
    (0, 0),
];

impl XlangSpec {
    /// Parses `CURRENT` / `LEGACY` (also `0.10`, `0.11`).
    pub fn parse(s: &str) -> Option<XlangSpec> {
        match s.trim().to_ascii_uppercase().as_str() {
            "CURRENT" | "LATEST" | "" => Some(XlangSpec::Current),
            "LEGACY" | "0.10" | "0.11" | "V0_11" => Some(XlangSpec::Legacy),
            _ => None,
        }
    }

    /// Wire id of a canonical internal type id.
    pub fn to_wire(self, canonical: u32) -> u32 {
        match self {
            XlangSpec::Legacy => canonical,
            XlangSpec::Current => CURRENT_IDS.iter().find(|(c, _)| *c == canonical).map(|(_, w)| *w).unwrap_or(canonical),
        }
    }

    /// Canonical id of a wire internal type id.
    pub fn from_wire(self, wire: u32) -> Option<u32> {
        match self {
            XlangSpec::Legacy => (wire < 64).then_some(wire),
            XlangSpec::Current => CURRENT_IDS.iter().find(|(_, w)| *w == wire).map(|(c, _)| *c),
        }
    }
}

/// Writer language id (Fory `Language.RUST`).
pub const LANGUAGE_RUST: u8 = 6;
const LEGACY_MAGIC: [u8; 2] = [0xd4, 0x62];

// list element header bits
const LIST_TRACKING_REF: u8 = 0b1;
const LIST_HAS_NULL: u8 = 0b10;
const LIST_DECL_TYPE: u8 = 0b100;
const LIST_SAME_TYPE: u8 = 0b1000;
// map KV header bits
const KEY_TRACKING_REF: u8 = 0b1;
const KEY_HAS_NULL: u8 = 0b10;
const KEY_DECL_TYPE: u8 = 0b100;
const VALUE_TRACKING_REF: u8 = 0b1000;
const VALUE_HAS_NULL: u8 = 0b1_0000;
const VALUE_DECL_TYPE: u8 = 0b10_0000;
const MAX_CHUNK: usize = 255;

// ----------------------------------------------------------------------------- varints

/// LEB128 unsigned 32-bit varint.
pub fn write_varuint32(out: &mut Vec<u8>, mut v: u32) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Zigzag signed 32-bit varint.
pub fn write_varint32(out: &mut Vec<u8>, v: i32) {
    write_varuint32(out, ((v << 1) ^ (v >> 31)) as u32)
}

/// Fory unsigned 64-bit varint: up to 8 groups of 7 bits, then one full byte (max 9 bytes).
pub fn write_varuint64(out: &mut Vec<u8>, mut v: u64) {
    for _ in 0..8 {
        if v < 0x80 {
            out.push(v as u8);
            return;
        }
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Zigzag signed 64-bit varint.
pub fn write_varint64(out: &mut Vec<u8>, v: i64) {
    write_varuint64(out, ((v << 1) ^ (v >> 63)) as u64)
}

/// A byte reader over a Fory buffer.
pub struct Reader<'a> {
    b: &'a [u8],
    pub pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(b: &'a [u8]) -> Reader<'a> {
        Reader { b, pos: 0 }
    }
    fn eof() -> CamelError {
        df_err("Truncated Fory data")
    }
    pub fn u8(&mut self) -> Result<u8> {
        let v = *self.b.get(self.pos).ok_or_else(Self::eof)?;
        self.pos += 1;
        Ok(v)
    }
    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.b.len() {
            return Err(Self::eof());
        }
        let s = &self.b[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn arr<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut a = [0u8; N];
        a.copy_from_slice(self.take(N)?);
        Ok(a)
    }
    pub fn varuint32(&mut self) -> Result<u32> {
        let mut r: u32 = 0;
        for i in 0..5 {
            let b = self.u8()?;
            r |= ((b & 0x7f) as u32) << (7 * i);
            if b & 0x80 == 0 {
                return Ok(r);
            }
        }
        Err(df_err("Malformed Fory varuint32"))
    }
    pub fn varint32(&mut self) -> Result<i32> {
        let u = self.varuint32()?;
        Ok(((u >> 1) as i32) ^ -((u & 1) as i32))
    }
    pub fn varuint64(&mut self) -> Result<u64> {
        let mut r: u64 = 0;
        for i in 0..8 {
            let b = self.u8()?;
            r |= ((b & 0x7f) as u64) << (7 * i);
            if b & 0x80 == 0 {
                return Ok(r);
            }
        }
        let b = self.u8()?;
        Ok(r | ((b as u64) << 56))
    }
    pub fn varint64(&mut self) -> Result<i64> {
        let u = self.varuint64()?;
        Ok(((u >> 1) as i64) ^ -((u & 1) as i64))
    }
    fn sli_int64(&mut self) -> Result<i64> {
        let first = self.b.get(self.pos).copied().ok_or_else(Self::eof)?;
        if first & 1 == 0 {
            Ok((i32::from_le_bytes(self.arr::<4>()?) >> 1) as i64)
        } else {
            self.pos += 1;
            Ok(i64::from_le_bytes(self.arr::<8>()?))
        }
    }
}

/// IEEE half -> f64.
fn half_to_f64(h: u16) -> f64 {
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

// ----------------------------------------------------------------------------- meta strings

/// Meta string encodings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetaEncoding {
    Utf8 = 0,
    LowerSpecial = 1,
    LowerUpperDigitSpecial = 2,
    FirstToLowerSpecial = 3,
    AllToLowerSpecial = 4,
}

fn lower_special_code(c: char) -> Option<u8> {
    match c {
        'a'..='z' => Some(c as u8 - b'a'),
        '.' => Some(26),
        '_' => Some(27),
        '$' => Some(28),
        '|' => Some(29),
        _ => None,
    }
}

fn lower_special_char(v: u8) -> Result<char> {
    match v {
        0..=25 => Ok((b'a' + v) as char),
        26 => Ok('.'),
        27 => Ok('_'),
        28 => Ok('$'),
        29 => Ok('|'),
        _ => Err(df_err(format!("Invalid LOWER_SPECIAL code {}", v))),
    }
}

fn lud_code(c: char) -> Option<u8> {
    match c {
        'a'..='z' => Some(c as u8 - b'a'),
        'A'..='Z' => Some(c as u8 - b'A' + 26),
        '0'..='9' => Some(c as u8 - b'0' + 52),
        '.' => Some(62),
        '_' => Some(63),
        _ => None,
    }
}

fn lud_char(v: u8) -> char {
    match v {
        0..=25 => (b'a' + v) as char,
        26..=51 => (b'A' + v - 26) as char,
        52..=61 => (b'0' + v - 52) as char,
        62 => '.',
        _ => '_',
    }
}

/// Pack `codes` of `bits` bits MSB first after a leading "strip last char" flag bit.
fn pack_bits(codes: &[u8], bits: usize) -> Vec<u8> {
    let total = 1 + codes.len() * bits;
    let nbytes = total.div_ceil(8);
    let strip = nbytes * 8 - total >= bits;
    let mut out = vec![0u8; nbytes];
    let mut pos = 0usize;
    let mut put = |v: u32, n: usize, out: &mut Vec<u8>| {
        for i in (0..n).rev() {
            if (v >> i) & 1 == 1 {
                out[pos / 8] |= 0x80 >> (pos % 8);
            }
            pos += 1;
        }
    };
    put(strip as u32, 1, &mut out);
    for c in codes {
        put(*c as u32, bits, &mut out);
    }
    out
}

fn unpack_bits(data: &[u8], bits: usize) -> Vec<u8> {
    if data.is_empty() {
        return vec![];
    }
    let total = data.len() * 8;
    let get = |pos: usize| (data[pos / 8] >> (7 - pos % 8)) & 1;
    let strip = get(0) == 1;
    let mut codes = Vec::new();
    let mut pos = 1;
    while pos + bits <= total {
        let mut v = 0u8;
        for i in 0..bits {
            v = (v << 1) | get(pos + i);
        }
        codes.push(v);
        pos += bits;
    }
    if strip {
        codes.pop();
    }
    codes
}

/// Encode a meta string, choosing the most compact encoding.
pub fn encode_meta_string(s: &str) -> (MetaEncoding, Vec<u8>) {
    if s.is_empty() {
        return (MetaEncoding::Utf8, vec![]);
    }
    if s.chars().all(|c| lower_special_code(c).is_some()) {
        let codes: Vec<u8> = s.chars().map(|c| lower_special_code(c).unwrap()).collect();
        return (MetaEncoding::LowerSpecial, pack_bits(&codes, 5));
    }
    let mut chars = s.chars();
    let first = chars.next().unwrap();
    let rest: String = chars.collect();
    if first.is_ascii_uppercase() && rest.chars().all(|c| lower_special_code(c).is_some()) {
        let mut codes = vec![first.to_ascii_lowercase() as u8 - b'a'];
        codes.extend(rest.chars().map(|c| lower_special_code(c).unwrap()));
        return (MetaEncoding::FirstToLowerSpecial, pack_bits(&codes, 5));
    }
    if s.chars().all(|c| lud_code(c).is_some()) {
        let upper = s.chars().filter(|c| c.is_ascii_uppercase()).count();
        let digits = s.chars().filter(|c| c.is_ascii_digit()).count();
        // ALL_TO_LOWER_SPECIAL escapes uppercase letters with '|': cheaper with few of them
        if digits == 0 && (s.len() + upper) * 5 < s.len() * 6 {
            let mut codes = Vec::new();
            for c in s.chars() {
                if c.is_ascii_uppercase() {
                    codes.push(29);
                    codes.push(c.to_ascii_lowercase() as u8 - b'a');
                } else {
                    codes.push(lower_special_code(c).unwrap());
                }
            }
            return (MetaEncoding::AllToLowerSpecial, pack_bits(&codes, 5));
        }
        let codes: Vec<u8> = s.chars().map(|c| lud_code(c).unwrap()).collect();
        return (MetaEncoding::LowerUpperDigitSpecial, pack_bits(&codes, 6));
    }
    (MetaEncoding::Utf8, s.as_bytes().to_vec())
}

/// Decode meta string bytes.
pub fn decode_meta_string(enc: u8, data: &[u8]) -> Result<String> {
    match enc {
        0 => String::from_utf8(data.to_vec()).map_err(|_| df_err("Invalid UTF-8 meta string")),
        1 => unpack_bits(data, 5).into_iter().map(lower_special_char).collect(),
        2 => Ok(unpack_bits(data, 6).into_iter().map(lud_char).collect()),
        3 => {
            let s: String = unpack_bits(data, 5).into_iter().map(lower_special_char).collect::<Result<String>>()?;
            let mut c = s.chars();
            Ok(match c.next() {
                Some(f) => f.to_ascii_uppercase().to_string() + c.as_str(),
                None => s,
            })
        }
        4 => {
            let s: String = unpack_bits(data, 5).into_iter().map(lower_special_char).collect::<Result<String>>()?;
            let mut out = String::new();
            let mut up = false;
            for c in s.chars() {
                if c == '|' && !up {
                    up = true;
                } else {
                    out.push(if up { c.to_ascii_uppercase() } else { c });
                    up = false;
                }
            }
            Ok(out)
        }
        e => Err(df_err(format!("Unknown meta string encoding {}", e))),
    }
}

/// MurmurHash3 x64 128 (first 64 bits), as used for Fory meta string hashes.
pub fn murmur3_x64_128(data: &[u8], seed: u64) -> (u64, u64) {
    const C1: u64 = 0x87c3_7b91_1142_53d5;
    const C2: u64 = 0x4cf5_ad43_2745_937f;
    let fmix = |mut k: u64| {
        k ^= k >> 33;
        k = k.wrapping_mul(0xff51_afd7_ed55_8ccd);
        k ^= k >> 33;
        k = k.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
        k ^ (k >> 33)
    };
    let (mut h1, mut h2) = (seed, seed);
    let nblocks = data.len() / 16;
    for i in 0..nblocks {
        let mut k1 = u64::from_le_bytes(data[i * 16..i * 16 + 8].try_into().unwrap());
        let mut k2 = u64::from_le_bytes(data[i * 16 + 8..i * 16 + 16].try_into().unwrap());
        k1 = k1.wrapping_mul(C1).rotate_left(31).wrapping_mul(C2);
        h1 ^= k1;
        h1 = h1.rotate_left(27).wrapping_add(h2).wrapping_mul(5).wrapping_add(0x52dc_e729);
        k2 = k2.wrapping_mul(C2).rotate_left(33).wrapping_mul(C1);
        h2 ^= k2;
        h2 = h2.rotate_left(31).wrapping_add(h1).wrapping_mul(5).wrapping_add(0x3849_5ab5);
    }
    let tail = &data[nblocks * 16..];
    let (mut k1, mut k2) = (0u64, 0u64);
    for i in (0..tail.len()).rev() {
        if i >= 8 {
            k2 ^= (tail[i] as u64) << ((i - 8) * 8);
        } else {
            k1 ^= (tail[i] as u64) << (i * 8);
        }
    }
    if tail.len() > 8 {
        k2 = k2.wrapping_mul(C2).rotate_left(33).wrapping_mul(C1);
        h2 ^= k2;
    }
    if !tail.is_empty() {
        k1 = k1.wrapping_mul(C1).rotate_left(31).wrapping_mul(C2);
        h1 ^= k1;
    }
    let len = data.len() as u64;
    h1 ^= len;
    h2 ^= len;
    h1 = h1.wrapping_add(h2);
    h2 = h2.wrapping_add(h1);
    h1 = fmix(h1);
    h2 = fmix(h2);
    h1 = h1.wrapping_add(h2);
    h2 = h2.wrapping_add(h1);
    (h1, h2)
}

fn meta_hash(data: &[u8], enc: u8) -> u64 {
    let h = murmur3_x64_128(data, 47).0 as i64;
    let h = h.unsigned_abs();
    (h & 0xffff_ffff_ffff_ff00) | enc as u64
}

// ----------------------------------------------------------------------------- writer

/// Fory xlang configuration.
#[derive(Clone, Debug, Default)]
pub struct ForyConfig {
    /// Write containers with ref flags and back references.
    pub ref_tracking: bool,
    /// Every `@type` written / read must be listed in `struct_types`.
    pub require_class_registration: bool,
    /// Struct field names by full type name (`ns.Name`, or `#id` for numeric ids).
    pub struct_types: BTreeMap<String, Vec<String>>,
    /// Compatible mode: structs are written as `NAMED_COMPATIBLE_STRUCT` with a shared
    /// TypeDef (field names and types), so readers need no `structTypes`.
    pub compatible: bool,
    /// Extension type serializers by full type name (`NAMED_EXT`).
    pub extensions: BTreeMap<String, ExtSerializer>,
    /// Protocol edition written (`xlangSpec`, default the current spec).
    pub spec: XlangSpec,
}

/// A user serializer for an extension type: values `{"@ext": "ns.Name", ...}` are written
/// as `NAMED_EXT` with the bytes of `write`; `read` decodes them back (it must consume
/// exactly what `write` produced).
#[derive(Clone)]
pub struct ExtSerializer {
    pub write: Arc<dyn Fn(&Value, &mut Vec<u8>) -> Result<()> + Send + Sync>,
    pub read: Arc<dyn Fn(&mut Reader) -> Result<Value> + Send + Sync>,
}

impl std::fmt::Debug for ExtSerializer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ExtSerializer")
    }
}

impl ExtSerializer {
    pub fn new(write: impl Fn(&Value, &mut Vec<u8>) -> Result<()> + Send + Sync + 'static, read: impl Fn(&mut Reader) -> Result<Value> + Send + Sync + 'static) -> ExtSerializer {
        ExtSerializer { write: Arc::new(write), read: Arc::new(read) }
    }
}

struct Writer<'c> {
    cfg: &'c ForyConfig,
    out: Vec<u8>,
    meta_strings: HashMap<Vec<u8>, u32>,
    refs: Vec<Value>,
    type_defs: HashMap<Vec<u8>, u32>,
}

// ----------------------------------------------------------------------------- DECIMAL

/// Parses a plain decimal string into (negative, magnitude digits, scale).
fn parse_decimal(s: &str) -> Result<(bool, Vec<u8>, i32)> {
    let t = s.trim();
    let (neg, t) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let (mant, exp) = match t.find(['e', 'E']) {
        Some(i) => (&t[..i], t[i + 1..].parse::<i32>().map_err(|_| df_err(format!("Invalid decimal {}", s)))?),
        None => (t, 0),
    };
    let (ip, fp) = mant.split_once('.').unwrap_or((mant, ""));
    if ip.is_empty() && fp.is_empty() || !ip.chars().chain(fp.chars()).all(|c| c.is_ascii_digit()) {
        return Err(df_err(format!("Invalid decimal {}", s)));
    }
    let digits: Vec<u8> = ip.bytes().chain(fp.bytes()).map(|b| b - b'0').collect();
    Ok((neg, digits, fp.len() as i32 - exp))
}

/// Decimal digits (most significant first) to minimal little-endian magnitude bytes.
fn digits_to_le(digits: &[u8]) -> Vec<u8> {
    let mut d: Vec<u32> = digits.iter().map(|x| *x as u32).collect();
    let mut out = Vec::new();
    while d.iter().any(|x| *x != 0) {
        let mut rem = 0u32;
        let mut next = Vec::with_capacity(d.len());
        for x in &d {
            let cur = rem * 10 + x;
            next.push(cur / 256);
            rem = cur % 256;
        }
        out.push(rem as u8);
        let nz = next.iter().position(|x| *x != 0).unwrap_or(next.len());
        d = next[nz..].to_vec();
    }
    out
}

fn le_to_digits(le: &[u8]) -> String {
    let mut digits: Vec<u32> = vec![0];
    for &b in le.iter().rev() {
        let mut carry = b as u32;
        for d in digits.iter_mut() {
            let v = *d * 256 + carry;
            *d = v % 10;
            carry = v / 10;
        }
        while carry > 0 {
            digits.push(carry % 10);
            carry /= 10;
        }
    }
    digits.iter().rev().map(|d| char::from(b'0' + *d as u8)).collect()
}

/// Writes a DECIMAL (`scale` varint32, unscaled header varuint64, optional magnitude).
pub fn write_decimal(out: &mut Vec<u8>, text: &str) -> Result<()> {
    let (neg, digits, scale) = parse_decimal(text)?;
    write_varint32(out, scale);
    let mag = digits_to_le(&digits);
    let small = mag.len() <= 8 && {
        let mut b = [0u8; 8];
        b[..mag.len()].copy_from_slice(&mag);
        u64::from_le_bytes(b) < (1u64 << 62)
    };
    if small {
        let mut b = [0u8; 8];
        b[..mag.len()].copy_from_slice(&mag);
        let u = u64::from_le_bytes(b) as i64;
        let v = if neg { -u } else { u };
        let zz = ((v << 1) ^ (v >> 63)) as u64;
        write_varuint64(out, zz << 1);
    } else {
        write_varuint64(out, ((((mag.len() as u64) << 1) | neg as u64) << 1) | 1);
        out.extend_from_slice(&mag);
    }
    Ok(())
}

fn format_decimal(neg: bool, digits: String, scale: i32) -> String {
    let digits = digits.trim_start_matches('0').to_string();
    let digits = if digits.is_empty() { "0".to_string() } else { digits };
    let neg = neg && digits != "0";
    let body = if scale <= 0 {
        format!("{}{}", digits, "0".repeat((-scale) as usize))
    } else {
        let sc = scale as usize;
        let padded = if digits.len() <= sc { format!("{}{}", "0".repeat(sc + 1 - digits.len()), digits) } else { digits };
        format!("{}.{}", &padded[..padded.len() - sc], &padded[padded.len() - sc..])
    };
    if neg {
        format!("-{}", body)
    } else {
        body
    }
}

/// Reads a DECIMAL as its plain string form.
pub fn read_decimal(r: &mut Reader) -> Result<String> {
    let scale = r.varint32()?;
    if !(-10000..=10000).contains(&scale) {
        return Err(df_err(format!("Decimal scale {} out of range", scale)));
    }
    let h = r.varuint64()?;
    if h & 1 == 0 {
        let zz = h >> 1;
        let v = ((zz >> 1) as i64) ^ -((zz & 1) as i64);
        return Ok(format_decimal(v < 0, v.unsigned_abs().to_string(), scale));
    }
    let meta = h >> 1;
    let len = (meta >> 1) as usize;
    if len == 0 || len > 10000 {
        return Err(df_err("Invalid decimal magnitude length"));
    }
    let mag = r.take(len)?;
    Ok(format_decimal(meta & 1 == 1, le_to_digits(mag), scale))
}

// ----------------------------------------------------------------------------- TypeDef

/// A struct type definition (compatible mode).
#[derive(Clone, Debug, PartialEq)]
pub struct TypeDef {
    pub name: String,
    /// (field name, type id, nullable)
    pub fields: Vec<(String, u32, bool)>,
}

fn is_primitive_field(t: u32) -> bool {
    use types::*;
    matches!(t, BOOL | INT8 | INT16 | INT32 | VAR_INT32 | INT64 | VAR_INT64 | SLI_INT64 | FLOAT16 | FLOAT32 | FLOAT64 | UINT8 | UINT16 | UINT32 | VAR_UINT32 | UINT64 | VAR_UINT64 | TAGGED_UINT64 | BFLOAT16)
}

fn inline_meta_string(out: &mut Vec<u8>, s: &str) {
    let (enc, data) = encode_meta_string(s);
    write_varuint32(out, (data.len() as u32) << 1);
    if data.len() > 16 {
        out.extend_from_slice(&meta_hash(&data, enc as u8).to_le_bytes());
    } else {
        out.push(enc as u8);
    }
    out.extend_from_slice(&data);
}

fn read_inline_meta_string(r: &mut Reader) -> Result<String> {
    let h = r.varuint32()?;
    let len = (h >> 1) as usize;
    let enc = if len > 16 { (u64::from_le_bytes(r.arr::<8>()?) & 0xff) as u8 } else { r.u8()? };
    decode_meta_string(enc, r.take(len)?)
}

/// Encodes a TypeDef: 8-byte global header (size, murmur3 hash), meta header, type spec,
/// field infos.
pub fn encode_type_def(td: &TypeDef) -> Vec<u8> {
    encode_type_def_spec(td, XlangSpec::Legacy)
}

/// [`encode_type_def`] with the field type ids of a protocol edition.
pub fn encode_type_def_spec(td: &TypeDef, spec: XlangSpec) -> Vec<u8> {
    let mut body = Vec::new();
    let n = td.fields.len();
    body.push(0x80 | 0x40 | 0x20 | (n.min(31) as u8));
    if n >= 31 {
        write_varuint32(&mut body, (n - 31) as u32);
    }
    let (ns, name) = split_type_name(&td.name);
    inline_meta_string(&mut body, ns);
    inline_meta_string(&mut body, name);
    for (fname, t, nullable) in &td.fields {
        // name: LOWER_UPPER_DIGIT_SPECIAL when possible, else UTF-8
        let (enc, data) = if !fname.is_empty() && fname.chars().all(|c| lud_code(c).is_some()) {
            (2u8, pack_bits(&fname.chars().map(|c| lud_code(c).unwrap()).collect::<Vec<_>>(), 6))
        } else {
            (0u8, fname.as_bytes().to_vec())
        };
        let size = data.len().max(1) - 1;
        body.push((enc << 6) | ((size.min(15) as u8) << 2) | ((*nullable as u8) << 1));
        if size >= 15 {
            write_varuint32(&mut body, (size - 15) as u32);
        }
        write_varuint32(&mut body, spec.to_wire(*t));
        match *t {
            types::LIST | types::SET => write_varuint32(&mut body, 2),
            types::MAP => {
                write_varuint32(&mut body, 2);
                write_varuint32(&mut body, 2);
            }
            _ => {}
        }
        body.extend_from_slice(&data);
    }
    let size_bits = body.len().min(0xff) as u64;
    let hash = murmur3_x64_128(&body, 47).0;
    let header = (hash << 12) | size_bits;
    let mut out = header.to_le_bytes().to_vec();
    if body.len() >= 0xff {
        write_varuint32(&mut out, (body.len() - 0xff) as u32);
    }
    out.extend(body);
    out
}

/// Decodes a TypeDef written by [`encode_type_def`] (or another Fory implementation).
pub fn decode_type_def(r: &mut Reader) -> Result<TypeDef> {
    decode_type_def_spec(r, XlangSpec::Legacy)
}

/// [`decode_type_def`] for a protocol edition.
pub fn decode_type_def_spec(r: &mut Reader, spec: XlangSpec) -> Result<TypeDef> {
    let header = u64::from_le_bytes(r.arr::<8>()?);
    if header & 0x100 != 0 {
        return Err(df_err("Compressed Fory type meta is not supported"));
    }
    let mut size = (header & 0xff) as usize;
    if size == 0xff {
        size += r.varuint32()? as usize;
    }
    let body = r.take(size)?;
    let mut br = Reader::new(body);
    let mh = br.u8()?;
    if mh & 0x80 == 0 {
        return Err(df_err("Only struct type definitions are supported"));
    }
    let mut n = (mh & 0x1f) as usize;
    if n == 31 {
        n += br.varuint32()? as usize;
    }
    let name = if mh & 0x20 != 0 {
        let ns = read_inline_meta_string(&mut br)?;
        let name = read_inline_meta_string(&mut br)?;
        if ns.is_empty() {
            name
        } else {
            format!("{}.{}", ns, name)
        }
    } else {
        format!("#{}", br.varuint32()?)
    };
    let mut fields = Vec::with_capacity(n.min(1024));
    for _ in 0..n {
        let fh = br.u8()?;
        let enc = fh >> 6;
        let mut size = ((fh >> 2) & 0x0f) as usize;
        if size == 15 {
            size += br.varuint32()? as usize;
        }
        let t = br.varuint32()?;
        let t = spec.from_wire(t & 0xff).unwrap_or(t & 0xff);
        match t {
            types::LIST | types::SET => {
                br.varuint32()?;
            }
            types::MAP => {
                br.varuint32()?;
                br.varuint32()?;
            }
            _ => {}
        }
        let fname = match enc {
            3 => format!("#{}", size),
            0 => decode_meta_string(0, br.take(size + 1)?)?,
            1 => decode_meta_string(4, br.take(size + 1)?)?,
            _ => decode_meta_string(2, br.take(size + 1)?)?,
        };
        fields.push((fname, t, fh & 0b10 != 0));
    }
    Ok(TypeDef { name, fields })
}

fn field_type_id(v: &Value) -> u32 {
    match v {
        Value::Null => 0,
        other => Writer::type_id_of(other, false),
    }
}

fn snake(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Field order of a struct: snake_case names sorted.
pub fn sorted_fields<'a>(names: impl IntoIterator<Item = &'a String>) -> Vec<String> {
    let mut v: Vec<String> = names.into_iter().cloned().collect();
    v.sort_by_key(|a| snake(a));
    v
}

fn split_type_name(full: &str) -> (&str, &str) {
    match full.rfind('.') {
        Some(i) => (&full[..i], &full[i + 1..]),
        None => ("", full),
    }
}

impl Writer<'_> {
    fn write_meta_string(&mut self, s: &str) {
        let (enc, data) = encode_meta_string(s);
        let mut key = data.clone();
        key.push(enc as u8);
        if let Some(id) = self.meta_strings.get(&key) {
            write_varuint32(&mut self.out, ((id + 1) << 1) | 1);
            return;
        }
        let id = self.meta_strings.len() as u32;
        self.meta_strings.insert(key, id);
        write_varuint32(&mut self.out, (data.len() as u32) << 1);
        if data.len() > 16 {
            self.out.extend_from_slice(&meta_hash(&data, enc as u8).to_le_bytes());
        } else {
            self.out.push(enc as u8);
        }
        self.out.extend_from_slice(&data);
    }

    fn type_id(&self, v: &Value) -> u32 {
        Self::type_id_of(v, self.cfg.compatible)
    }

    fn type_id_of(v: &Value, compatible: bool) -> u32 {
        match v {
            Value::Object(m) if m.len() == 1 && m.get("@decimal").is_some() => types::DECIMAL,
            Value::Object(m) if m.get("@ext").and_then(|t| t.as_str()).is_some() => types::NAMED_EXT,
            Value::Object(m) if compatible && m.get("@type").and_then(|t| t.as_str()).is_some() => types::NAMED_COMPATIBLE_STRUCT,
            Value::Null => 0,
            Value::Bool(_) => types::BOOL,
            Value::Int(_) => types::VAR_INT64,
            Value::Float(_) => types::FLOAT64,
            Value::String(_) => types::STRING,
            Value::Bytes(_) => types::BINARY,
            Value::Array(_) => types::LIST,
            Value::Object(m) if m.get("@type").and_then(|t| t.as_str()).is_some() => types::NAMED_STRUCT,
            Value::Object(_) => types::MAP,
        }
    }

    /// Type info: id, plus namespace/type name meta strings for named types.
    fn write_type(&mut self, v: &Value) -> Result<()> {
        let id = self.type_id(v);
        write_varuint32(&mut self.out, self.cfg.spec.to_wire(id));
        if id == types::NAMED_EXT {
            let full = v.get("@ext").and_then(|t| t.as_str()).unwrap_or("").to_string();
            if !self.cfg.extensions.contains_key(&full) {
                return Err(df_err(format!("No Fory extension serializer registered for {}", full)));
            }
            let (ns, name) = split_type_name(&full);
            self.write_meta_string(ns);
            self.write_meta_string(name);
        }
        if id == types::NAMED_COMPATIBLE_STRUCT {
            let full = v.get("@type").and_then(|t| t.as_str()).unwrap_or("").to_string();
            if self.cfg.require_class_registration && !self.cfg.struct_types.contains_key(&full) {
                return Err(df_err(format!("Class {} is not registered (requireClassRegistration=true): add it to structTypes", full)));
            }
            let td = Self::type_def_of(v);
            let enc = encode_type_def_spec(&td, self.cfg.spec);
            match self.type_defs.get(&enc) {
                Some(i) => write_varuint32(&mut self.out, (i << 1) | 1),
                None => {
                    let i = self.type_defs.len() as u32;
                    write_varuint32(&mut self.out, i << 1);
                    self.out.extend_from_slice(&enc);
                    self.type_defs.insert(enc, i);
                }
            }
        }
        if id == types::NAMED_STRUCT {
            let full = v.get("@type").and_then(|t| t.as_str()).unwrap_or("").to_string();
            if self.cfg.require_class_registration && !self.cfg.struct_types.contains_key(&full) {
                return Err(df_err(format!("Class {} is not registered (requireClassRegistration=true): add it to structTypes", full)));
            }
            let (ns, name) = split_type_name(&full);
            self.write_meta_string(ns);
            self.write_meta_string(name);
        }
        Ok(())
    }

    fn type_def_of(v: &Value) -> TypeDef {
        let m = v.as_object().unwrap();
        let names: Vec<&String> = m.keys().filter(|k| k.as_str() != "@type").collect();
        let fields = sorted_fields(names).into_iter().map(|f| {
            let t = field_type_id(&m[&f]);
            (f, t, true)
        });
        TypeDef { name: v.get("@type").and_then(|t| t.as_str()).unwrap_or("").to_string(), fields: fields.collect() }
    }

    fn tracks(&self, v: &Value) -> bool {
        self.cfg.ref_tracking && matches!(v, Value::Array(_) | Value::Object(_) | Value::Bytes(_) | Value::String(_))
    }

    /// Ref/null flag, then (when not a back reference) type info and data.
    fn write_ref_value(&mut self, v: &Value) -> Result<()> {
        if v.is_null() {
            self.out.push(NULL_FLAG as u8);
            return Ok(());
        }
        if self.tracks(v) {
            if let Some(i) = self.refs.iter().position(|r| r.eq_ordered_deep(v)) {
                self.out.push(REF_FLAG as u8);
                write_varuint32(&mut self.out, i as u32);
                return Ok(());
            }
            self.refs.push(v.clone());
            self.out.push(REF_VALUE_FLAG as u8);
        } else {
            self.out.push(NOT_NULL_VALUE_FLAG as u8);
        }
        self.write_type(v)?;
        self.write_data(v)
    }

    fn write_string(&mut self, s: &str) {
        if s.chars().all(|c| (c as u32) < 256) {
            let bytes: Vec<u8> = s.chars().map(|c| c as u8).collect();
            write_varuint64(&mut self.out, ((bytes.len() as u64) << 2) | 0);
            self.out.extend_from_slice(&bytes);
        } else {
            write_varuint64(&mut self.out, ((s.len() as u64) << 2) | 2);
            self.out.extend_from_slice(s.as_bytes());
        }
    }

    fn write_data(&mut self, v: &Value) -> Result<()> {
        match v {
            Value::Null => {}
            Value::Bool(b) => self.out.push(*b as u8),
            Value::Int(i) => write_varint64(&mut self.out, *i),
            Value::Float(f) => self.out.extend_from_slice(&f.to_le_bytes()),
            Value::String(s) => self.write_string(s),
            Value::Bytes(b) => {
                write_varuint32(&mut self.out, b.len() as u32);
                self.out.extend_from_slice(b);
            }
            Value::Array(a) => self.write_list(a)?,
            Value::Object(m) if self.type_id(v) == types::DECIMAL => write_decimal(&mut self.out, &m["@decimal"].to_string())?,
            Value::Object(m) if self.type_id(v) == types::NAMED_EXT => {
                let ext = self.cfg.extensions[&m["@ext"].to_string()].clone();
                (ext.write)(v, &mut self.out)?;
            }
            Value::Object(m) if self.type_id(v) == types::NAMED_COMPATIBLE_STRUCT => {
                for (f, t, _) in Self::type_def_of(v).fields {
                    let fv = &m[&f];
                    if is_primitive_field(t) {
                        self.out.push(NOT_NULL_VALUE_FLAG as u8);
                        self.write_data(fv)?;
                    } else {
                        self.write_ref_value(fv)?;
                    }
                }
            }
            Value::Object(m) if self.type_id(v) == types::NAMED_STRUCT => {
                let names: Vec<&String> = m.keys().filter(|k| k.as_str() != "@type").collect();
                for f in sorted_fields(names) {
                    self.write_ref_value(&m[&f])?;
                }
            }
            Value::Object(m) => self.write_map(m)?,
        }
        Ok(())
    }

    fn write_list(&mut self, a: &[Value]) -> Result<()> {
        write_varuint32(&mut self.out, a.len() as u32);
        if a.is_empty() {
            return Ok(());
        }
        let has_null = a.iter().any(|v| v.is_null());
        let non_null: Vec<&Value> = a.iter().filter(|v| !v.is_null()).collect();
        let first_type = non_null.first().map(|v| self.type_id(v));
        // named structs of different names are not "the same type"
        let same = first_type.is_some()
            && non_null.iter().all(|v| Some(self.type_id(v)) == first_type && v.get("@type") == non_null[0].get("@type"));
        let track = self.cfg.ref_tracking && non_null.iter().any(|v| self.tracks(v));
        let mut header = 0u8;
        if track {
            header |= LIST_TRACKING_REF;
        }
        if has_null {
            header |= LIST_HAS_NULL;
        }
        if same {
            header |= LIST_SAME_TYPE;
        }
        self.out.push(header);
        if same {
            self.write_type(non_null[0])?;
        }
        for v in a {
            if track {
                if same && !v.is_null() {
                    // ref flag then data only
                    if let Some(i) = self.refs.iter().position(|r| r.eq_ordered_deep(v)) {
                        self.out.push(REF_FLAG as u8);
                        write_varuint32(&mut self.out, i as u32);
                        continue;
                    }
                    self.refs.push(v.clone());
                    self.out.push(REF_VALUE_FLAG as u8);
                    self.write_data(v)?;
                } else if same {
                    self.out.push(NULL_FLAG as u8);
                } else {
                    self.write_ref_value(v)?;
                }
            } else if same {
                if has_null {
                    if v.is_null() {
                        self.out.push(NULL_FLAG as u8);
                        continue;
                    }
                    self.out.push(NOT_NULL_VALUE_FLAG as u8);
                }
                self.write_data(v)?;
            } else if has_null {
                self.write_ref_value(v)?;
            } else {
                self.write_type(v)?;
                self.write_data(v)?;
            }
        }
        Ok(())
    }

    fn write_map(&mut self, m: &ObjectMap) -> Result<()> {
        write_varuint32(&mut self.out, m.len() as u32);
        let entries: Vec<(&String, &Value)> = m.iter().collect();
        let mut i = 0;
        while i < entries.len() {
            let (k, v) = entries[i];
            if v.is_null() {
                // single-entry chunk without chunk size: value null
                self.out.push(VALUE_HAS_NULL);
                self.write_type(&Value::String(String::new()))?;
                self.write_string(k);
                i += 1;
                continue;
            }
            // collect a chunk of entries whose values share the type
            let vt = self.type_id(v);
            let mut j = i;
            while j < entries.len() && j - i < MAX_CHUNK && !entries[j].1.is_null() && self.type_id(entries[j].1) == vt && entries[j].1.get("@type") == v.get("@type") {
                j += 1;
            }
            let track_v = self.tracks(v);
            let mut header = 0u8;
            if track_v {
                header |= VALUE_TRACKING_REF;
            }
            self.out.push(header);
            self.out.push((j - i) as u8);
            write_varuint32(&mut self.out, self.cfg.spec.to_wire(types::STRING));
            self.write_type(v)?;
            for (k, v) in &entries[i..j] {
                self.write_string(k);
                if track_v {
                    if let Some(pos) = self.refs.iter().position(|r| r.eq_ordered_deep(v)) {
                        self.out.push(REF_FLAG as u8);
                        write_varuint32(&mut self.out, pos as u32);
                        continue;
                    }
                    self.refs.push((*v).clone());
                    self.out.push(REF_VALUE_FLAG as u8);
                }
                self.write_data(v)?;
            }
            i = j;
        }
        Ok(())
    }
}

trait DeepEq {
    fn eq_ordered_deep(&self, other: &Value) -> bool;
}

impl DeepEq for Value {
    fn eq_ordered_deep(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Object(a), Value::Object(b)) => a.len() == b.len() && a.iter().zip(b.iter()).all(|((ka, va), (kb, vb))| ka == kb && va.eq_ordered_deep(vb)),
            (Value::Array(a), Value::Array(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.eq_ordered_deep(y)),
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a.to_bits() == b.to_bits(),
            (Value::Int(_), _) | (Value::Float(_), _) | (_, Value::Int(_)) | (_, Value::Float(_)) => false,
            (a, b) => a == b,
        }
    }
}

/// Serialize a value (header + root reference + type + data).
pub fn serialize(v: &Value, cfg: &ForyConfig) -> Result<Vec<u8>> {
    let mut w = Writer { cfg, out: Vec::new(), meta_strings: HashMap::new(), refs: Vec::new(), type_defs: HashMap::new() };
    if cfg.spec == XlangSpec::Current {
        // current spec: bitmap with the xlang flag only; a null root is a NULL_FLAG
        w.out.push(0b01);
        if v.is_null() {
            w.out.push(NULL_FLAG as u8);
            return Ok(w.out);
        }
        w.write_ref_value(v)?;
        return Ok(w.out);
    }
    let mut bitmap = 0b0110u8; // little endian + xlang
    if v.is_null() {
        bitmap |= 1;
    }
    w.out.push(bitmap);
    w.out.push(LANGUAGE_RUST);
    if v.is_null() {
        return Ok(w.out);
    }
    w.write_ref_value(v)?;
    Ok(w.out)
}

// ----------------------------------------------------------------------------- reader

#[derive(Clone, Debug)]
enum TypeInfo {
    Id(u32),
    Named(u32, String),
    Def(u32, Arc<TypeDef>),
}

impl TypeInfo {
    fn id(&self) -> u32 {
        match self {
            TypeInfo::Id(i) | TypeInfo::Named(i, _) | TypeInfo::Def(i, _) => *i & 0xff,
        }
    }
}

struct Decoder<'a, 'c> {
    r: Reader<'a>,
    cfg: &'c ForyConfig,
    meta_strings: Vec<String>,
    refs: Vec<Value>,
    depth: usize,
    type_defs: Vec<Arc<TypeDef>>,
    spec: XlangSpec,
}

impl Decoder<'_, '_> {
    fn read_meta_string(&mut self) -> Result<String> {
        let h = self.r.varuint32()?;
        if h & 1 == 1 {
            let id = (h >> 1) as usize;
            return self.meta_strings.get(id.wrapping_sub(1)).cloned().ok_or_else(|| df_err(format!("Unknown meta string reference {}", id)));
        }
        let len = (h >> 1) as usize;
        let enc = if len > 16 { (u64::from_le_bytes(self.r.arr::<8>()?) & 0xff) as u8 } else { self.r.u8()? };
        let data = self.r.take(len)?;
        let s = decode_meta_string(enc, data)?;
        self.meta_strings.push(s.clone());
        Ok(s)
    }

    fn read_type(&mut self) -> Result<TypeInfo> {
        let raw = self.r.varuint32()?;
        let id = match self.spec {
            XlangSpec::Legacy => raw,
            XlangSpec::Current => {
                let canon = self.spec.from_wire(raw).ok_or_else(|| df_err(format!("Unknown Fory xlang type id {}", raw)))?;
                if matches!(canon, types::ENUM | types::STRUCT | types::COMPATIBLE_STRUCT | types::EXT | types::TYPED_UNION) {
                    // registered by numeric id: the user type id follows as a varuint32
                    let user = self.r.varuint32()?;
                    canon | (user << 8)
                } else {
                    canon
                }
            }
        };
        match id & 0xff {
            types::NAMED_COMPATIBLE_STRUCT | types::COMPATIBLE_STRUCT => {
                let marker = self.r.varuint32()?;
                let td = if marker & 1 == 1 {
                    self.type_defs.get((marker >> 1) as usize).cloned().ok_or_else(|| df_err(format!("Unknown Fory type meta index {}", marker >> 1)))?
                } else {
                    let td = Arc::new(decode_type_def_spec(&mut self.r, self.spec)?);
                    self.type_defs.push(td.clone());
                    td
                };
                Ok(TypeInfo::Def(id, td))
            }
            types::NAMED_STRUCT | types::NAMED_ENUM | types::NAMED_EXT => {
                let ns = self.read_meta_string()?;
                let name = self.read_meta_string()?;
                Ok(TypeInfo::Named(id, if ns.is_empty() { name } else { format!("{}.{}", ns, name) }))
            }
            _ => Ok(TypeInfo::Id(id)),
        }
    }

    /// Ref flag + type + data.
    fn read_ref_value(&mut self) -> Result<Value> {
        let flag = self.r.u8()? as i8;
        match flag {
            NULL_FLAG => Ok(Value::Null),
            REF_FLAG => {
                let id = self.r.varuint32()? as usize;
                self.refs.get(id).cloned().ok_or_else(|| df_err(format!("Unknown Fory reference id {}", id)))
            }
            NOT_NULL_VALUE_FLAG => {
                let t = self.read_type()?;
                self.read_data(&t)
            }
            REF_VALUE_FLAG => {
                let slot = self.refs.len();
                self.refs.push(Value::Null);
                let t = self.read_type()?;
                let v = self.read_data(&t)?;
                self.refs[slot] = v.clone();
                Ok(v)
            }
            f => Err(df_err(format!("Invalid Fory reference flag {}", f))),
        }
    }

    fn read_string(&mut self) -> Result<String> {
        let h = self.r.varuint64()?;
        let len = (h >> 2) as usize;
        let data = self.r.take(len)?;
        match h & 3 {
            0 => Ok(data.iter().map(|b| *b as char).collect()),
            1 => {
                if len % 2 != 0 {
                    return Err(df_err("Odd UTF-16 string length"));
                }
                let units: Vec<u16> = data.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                String::from_utf16(&units).map_err(|_| df_err("Invalid UTF-16 string"))
            }
            2 => String::from_utf8(data.to_vec()).map_err(|_| df_err("Invalid UTF-8 string")),
            c => Err(df_err(format!("Unknown string coder {}", c))),
        }
    }

    fn prim_array(&mut self, width: usize, f: impl Fn(&[u8]) -> Value) -> Result<Value> {
        let n = self.r.varuint32()? as usize;
        if n % width != 0 {
            return Err(df_err("Primitive array size is not a multiple of its element size"));
        }
        let data = self.r.take(n)?;
        Ok(Value::Array(data.chunks(width).map(f).collect()))
    }

    fn read_data(&mut self, t: &TypeInfo) -> Result<Value> {
        self.depth += 1;
        if self.depth > 512 {
            return Err(df_err("Fory data nested too deeply"));
        }
        let v = self.read_data_inner(t)?;
        self.depth -= 1;
        Ok(v)
    }

    fn read_data_inner(&mut self, t: &TypeInfo) -> Result<Value> {
        use types::*;
        let r = &mut self.r;
        Ok(match t.id() {
            BOOL => Value::Bool(r.u8()? != 0),
            INT8 => Value::Int(r.u8()? as i8 as i64),
            INT16 => Value::Int(i16::from_le_bytes(r.arr::<2>()?) as i64),
            INT32 => Value::Int(i32::from_le_bytes(r.arr::<4>()?) as i64),
            VAR_INT32 => Value::Int(r.varint32()? as i64),
            INT64 => Value::Int(i64::from_le_bytes(r.arr::<8>()?)),
            VAR_INT64 => Value::Int(r.varint64()?),
            SLI_INT64 => Value::Int(r.sli_int64()?),
            FLOAT16 => Value::Float(half_to_f64(u16::from_le_bytes(r.arr::<2>()?))),
            FLOAT32 => Value::Float(f32::from_le_bytes(r.arr::<4>()?) as f64),
            FLOAT64 => Value::Float(f64::from_le_bytes(r.arr::<8>()?)),
            STRING => Value::String(self.read_string()?),
            ENUM | NAMED_ENUM => Value::Int(r.varuint32()? as i64),
            DURATION => {
                let secs = r.varint64()?;
                let nanos = i32::from_le_bytes(r.arr::<4>()?);
                Value::Float(secs as f64 + nanos as f64 / 1e9)
            }
            TIMESTAMP => Value::Int(i64::from_le_bytes(r.arr::<8>()?)),
            LOCAL_DATE => {
                let days = i32::from_le_bytes(r.arr::<4>()?) as i64;
                let (y, m, d, _, _, _) = camel_core::util_time::civil_from_epoch(days * 86400);
                Value::String(format!("{:04}-{:02}-{:02}", y, m, d))
            }
            BINARY => {
                let n = r.varuint32()? as usize;
                Value::Bytes(r.take(n)?.to_vec())
            }
            BOOL_ARRAY => self.prim_array(1, |c| Value::Bool(c[0] != 0))?,
            INT8_ARRAY => self.prim_array(1, |c| Value::Int(c[0] as i8 as i64))?,
            INT16_ARRAY => self.prim_array(2, |c| Value::Int(i16::from_le_bytes([c[0], c[1]]) as i64))?,
            INT32_ARRAY => self.prim_array(4, |c| Value::Int(i32::from_le_bytes(c.try_into().unwrap()) as i64))?,
            INT64_ARRAY => self.prim_array(8, |c| Value::Int(i64::from_le_bytes(c.try_into().unwrap())))?,
            FLOAT16_ARRAY => self.prim_array(2, |c| Value::Float(half_to_f64(u16::from_le_bytes([c[0], c[1]]))))?,
            FLOAT32_ARRAY => self.prim_array(4, |c| Value::Float(f32::from_le_bytes(c.try_into().unwrap()) as f64))?,
            FLOAT64_ARRAY => self.prim_array(8, |c| Value::Float(f64::from_le_bytes(c.try_into().unwrap())))?,
            UINT8 => Value::Int(r.u8()? as i64),
            UINT16 => Value::Int(u16::from_le_bytes(r.arr::<2>()?) as i64),
            UINT32 => Value::Int(u32::from_le_bytes(r.arr::<4>()?) as i64),
            VAR_UINT32 => Value::Int(r.varuint32()? as i64),
            UINT64 => u64_value(u64::from_le_bytes(r.arr::<8>()?)),
            VAR_UINT64 => u64_value(r.varuint64()?),
            TAGGED_UINT64 => u64_value(r.sli_int64()? as u64),
            BFLOAT16 => Value::Float(f32::from_bits((u16::from_le_bytes(r.arr::<2>()?) as u32) << 16) as f64),
            NONE => Value::Null,
            UINT8_ARRAY => self.prim_array(1, |c| Value::Int(c[0] as i64))?,
            UINT16_ARRAY => self.prim_array(2, |c| Value::Int(u16::from_le_bytes([c[0], c[1]]) as i64))?,
            UINT32_ARRAY => self.prim_array(4, |c| Value::Int(u32::from_le_bytes(c.try_into().unwrap()) as i64))?,
            UINT64_ARRAY => self.prim_array(8, |c| u64_value(u64::from_le_bytes(c.try_into().unwrap())))?,
            BFLOAT16_ARRAY => self.prim_array(2, |c| Value::Float(f32::from_bits((u16::from_le_bytes([c[0], c[1]]) as u32) << 16) as f64))?,
            DECIMAL => Value::String(read_decimal(r)?),
            NAMED_EXT | EXT => {
                let full = match t {
                    TypeInfo::Named(_, n) => n.clone(),
                    _ => format!("#{}", t_raw(t) >> 8),
                };
                let ext = self.cfg.extensions.get(&full).cloned().ok_or_else(|| df_err(format!("No Fory extension serializer registered for {}", full)))?;
                (ext.read)(&mut self.r)?
            }
            COMPATIBLE_STRUCT | NAMED_COMPATIBLE_STRUCT => {
                let TypeInfo::Def(_, td) = t else { return Err(df_err("Missing Fory type meta")) };
                let mut m = ObjectMap::new();
                m.insert("@type".to_string(), Value::String(td.name.clone()));
                for (f, ft, nullable) in td.fields.iter() {
                    let v = if is_primitive_field(*ft) {
                        if *nullable && self.r.u8()? as i8 == NULL_FLAG {
                            Value::Null
                        } else {
                            self.read_data(&TypeInfo::Id(*ft))?
                        }
                    } else {
                        self.read_ref_value()?
                    };
                    m.insert(f.clone(), v);
                }
                Value::Object(m)
            }
            LIST | SET | ARRAY => self.read_list()?,
            MAP => self.read_map()?,
            STRUCT | NAMED_STRUCT => {
                let full = match t {
                    TypeInfo::Named(_, n) => n.clone(),
                    TypeInfo::Id(id) | TypeInfo::Def(id, _) => format!("#{}", id >> 8),
                };
                let fields = self
                    .cfg
                    .struct_types
                    .get(&full)
                    .cloned()
                    .ok_or_else(|| df_err(format!("Cannot read Fory struct {}: its field names are unknown, declare them with the structTypes option", full)))?;
                let mut m = ObjectMap::new();
                m.insert("@type".to_string(), Value::String(full));
                for f in sorted_fields(fields.iter()) {
                    let v = self.read_ref_value()?;
                    m.insert(f, v);
                }
                Value::Object(m)
            }
            other => return Err(df_err(format!("Unsupported Fory xlang type id {}", other))),
        })
    }

    fn read_list(&mut self) -> Result<Value> {
        let n = self.r.varuint32()? as usize;
        if n == 0 {
            return Ok(Value::Array(vec![]));
        }
        let header = self.r.u8()?;
        let same = header & LIST_SAME_TYPE != 0;
        let decl = header & LIST_DECL_TYPE != 0;
        if decl && !same {
            return Err(df_err("List with a declared element type cannot be read without a schema"));
        }
        let elem_type = if same && !decl { Some(self.read_type()?) } else { None };
        if decl {
            return Err(df_err("List with a declared element type cannot be read without a schema"));
        }
        let track = header & LIST_TRACKING_REF != 0;
        let has_null = header & LIST_HAS_NULL != 0;
        let mut out = Vec::with_capacity(n.min(4096));
        for _ in 0..n {
            let v = match (&elem_type, track) {
                (Some(t), true) => {
                    let t = t.clone();
                    match self.r.u8()? as i8 {
                        NULL_FLAG => Value::Null,
                        REF_FLAG => {
                            let id = self.r.varuint32()? as usize;
                            self.refs.get(id).cloned().ok_or_else(|| df_err(format!("Unknown Fory reference id {}", id)))?
                        }
                        NOT_NULL_VALUE_FLAG => self.read_data(&t)?,
                        _ => {
                            let slot = self.refs.len();
                            self.refs.push(Value::Null);
                            let v = self.read_data(&t)?;
                            self.refs[slot] = v.clone();
                            v
                        }
                    }
                }
                (Some(t), false) => {
                    let t = t.clone();
                    if has_null && self.r.u8()? as i8 == NULL_FLAG {
                        Value::Null
                    } else {
                        self.read_data(&t)?
                    }
                }
                (None, _) if track || has_null => self.read_ref_value()?,
                (None, _) => {
                    let t = self.read_type()?;
                    self.read_data(&t)?
                }
            };
            out.push(v);
        }
        Ok(Value::Array(out))
    }

    fn read_map(&mut self) -> Result<Value> {
        let total = self.r.varuint32()? as usize;
        let mut m = ObjectMap::new();
        let key_text = |v: Value| match v {
            Value::String(s) => s,
            other => other.to_string(),
        };
        while m.len() < total {
            let header = self.r.u8()?;
            let key_null = header & KEY_HAS_NULL != 0;
            let value_null = header & VALUE_HAS_NULL != 0;
            if key_null || value_null {
                // a single entry with a null key and/or value, no chunk size
                let key = if key_null {
                    Value::Null
                } else if header & KEY_DECL_TYPE != 0 {
                    return Err(df_err("Map with declared key type cannot be read without a schema"));
                } else if header & KEY_TRACKING_REF != 0 {
                    self.read_ref_value()?
                } else {
                    let t = self.read_type()?;
                    self.read_data(&t)?
                };
                let value = if value_null {
                    Value::Null
                } else if header & VALUE_DECL_TYPE != 0 {
                    return Err(df_err("Map with declared value type cannot be read without a schema"));
                } else if header & VALUE_TRACKING_REF != 0 {
                    self.read_ref_value()?
                } else {
                    let t = self.read_type()?;
                    self.read_data(&t)?
                };
                m.insert(key_text(key), value);
                continue;
            }
            if header & (KEY_DECL_TYPE | VALUE_DECL_TYPE) != 0 {
                return Err(df_err("Map with declared key/value types cannot be read without a schema"));
            }
            let size = self.r.u8()? as usize;
            if size == 0 {
                return Err(df_err("Empty Fory map chunk"));
            }
            let kt = self.read_type()?;
            let vt = self.read_type()?;
            for _ in 0..size {
                let k = self.read_tracked(&kt, header & KEY_TRACKING_REF != 0)?;
                let v = self.read_tracked(&vt, header & VALUE_TRACKING_REF != 0)?;
                m.insert(key_text(k), v);
            }
        }
        Ok(Value::Object(m))
    }

    fn read_tracked(&mut self, t: &TypeInfo, track: bool) -> Result<Value> {
        if !track {
            return self.read_data(t);
        }
        match self.r.u8()? as i8 {
            NULL_FLAG => Ok(Value::Null),
            REF_FLAG => {
                let id = self.r.varuint32()? as usize;
                self.refs.get(id).cloned().ok_or_else(|| df_err(format!("Unknown Fory reference id {}", id)))
            }
            NOT_NULL_VALUE_FLAG => self.read_data(t),
            _ => {
                let slot = self.refs.len();
                self.refs.push(Value::Null);
                let v = self.read_data(t)?;
                self.refs[slot] = v.clone();
                Ok(v)
            }
        }
    }
}

/// Unsigned 64-bit values above `i64::MAX` are returned as floats.
fn u64_value(v: u64) -> Value {
    i64::try_from(v).map(Value::Int).unwrap_or(Value::Float(v as f64))
}

fn t_raw(t: &TypeInfo) -> u32 {
    match t {
        TypeInfo::Id(i) | TypeInfo::Named(i, _) | TypeInfo::Def(i, _) => *i,
    }
}

/// Deserialize Fory xlang bytes.
pub fn deserialize(b: &[u8], cfg: &ForyConfig) -> Result<Value> {
    let mut r = Reader::new(b);
    if b.len() >= 2 && b[..2] == LEGACY_MAGIC {
        r.pos = 2;
    }
    let bitmap = r.u8()?;
    // the current spec only defines bits 0 (xlang) and 1 (out-of-band); Fory 0.10/0.11
    // headers always carry bit 2 (xlang) and a language byte
    let legacy = r.pos > 1 || bitmap & 0xfc != 0;
    if !legacy {
        if bitmap & 0b01 == 0 {
            return Err(df_err("Not a Fory xlang payload (Java native serialization is not supported)"));
        }
        if bitmap & 0b10 != 0 {
            return Err(df_err("Fory out-of-band buffers are not supported"));
        }
        let mut d = Decoder { r, cfg, meta_strings: Vec::new(), refs: Vec::new(), depth: 0, type_defs: Vec::new(), spec: XlangSpec::Current };
        return d.read_ref_value();
    }
    if bitmap & 0b0100 == 0 {
        return Err(df_err("Not a Fory xlang payload (Java native serialization is not supported)"));
    }
    if bitmap & 0b0010 == 0 {
        return Err(df_err("Big endian Fory payloads are not supported"));
    }
    if bitmap & 0b1000 != 0 {
        return Err(df_err("Fory out-of-band buffers are not supported"));
    }
    let _language = r.u8()?;
    if bitmap & 1 != 0 {
        return Ok(Value::Null);
    }
    let mut d = Decoder { r, cfg, meta_strings: Vec::new(), refs: Vec::new(), depth: 0, type_defs: Vec::new(), spec: XlangSpec::Legacy };
    d.read_ref_value()
}

// ----------------------------------------------------------------------------- data format

/// The `fory` data format.
pub struct ForyDataFormat {
    pub config: ForyConfig,
}

impl DataFormat for ForyDataFormat {
    fn name(&self) -> &str {
        "fory"
    }
    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        ex.message.body = Value::Bytes(serialize(&ex.message.body, &self.config)?);
        Ok(())
    }
    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        let b = body_bytes(&ex.message.body);
        ex.message.body = deserialize(&b, &self.config)?;
        Ok(())
    }
}

/// Parse `structTypes` (`ns.A=f1,f2;ns.B=g`).
pub fn parse_struct_types(s: &str) -> Result<BTreeMap<String, Vec<String>>> {
    let mut m = BTreeMap::new();
    for part in s.split(';').map(str::trim).filter(|p| !p.is_empty()) {
        let (name, fields) = part.split_once('=').ok_or_else(|| CamelError::illegal_argument(format!("Invalid structTypes entry (expected name=field1,field2): {}", part)))?;
        m.insert(name.trim().to_string(), fields.split(',').map(|f| f.trim().to_string()).filter(|f| !f.is_empty()).collect());
    }
    Ok(m)
}

pub(crate) fn create(o: &Opts) -> Result<Arc<dyn DataFormat>> {
    let config = ForyConfig {
        ref_tracking: o.bool(&["refTracking", "trackingRef"], false)?,
        require_class_registration: o.bool(&["requireClassRegistration"], false)?,
        struct_types: parse_struct_types(&o.string(&["structTypes"], ""))?,
        compatible: o.bool(&["compatible"], false)? || o.string(&["compatibleMode"], "").eq_ignore_ascii_case("COMPATIBLE"),
        extensions: BTreeMap::new(),
        spec: match o.get(&["xlangSpec", "typeIdTable"]) {
            Some(v) => XlangSpec::parse(v).ok_or_else(|| CamelError::illegal_argument(format!("Invalid xlangSpec: {} (CURRENT or LEGACY)", v)))?,
            None => XlangSpec::Current,
        },
    };
    // accepted Camel options without effect here
    let _ = (o.bool(&["threadSafe"], true)?, o.bool(&["allowAutoWiredFory"], true)?, o.get(&["unmarshalType"]));
    Ok(Arc::new(ForyDataFormat { config }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{:02x}", x)).collect()
    }

    #[test]
    fn varints() {
        let mut o = Vec::new();
        write_varuint32(&mut o, 300);
        assert_eq!(hex(&o), "ac02");
        o.clear();
        write_varint32(&mut o, -1);
        assert_eq!(hex(&o), "01");
        o.clear();
        write_varint64(&mut o, i64::MIN);
        assert_eq!(o.len(), 9);
        assert_eq!(Reader::new(&o).varint64().unwrap(), i64::MIN);
        o.clear();
        write_varuint64(&mut o, u64::MAX);
        assert_eq!(hex(&o), "ffffffffffffffffff");
        assert_eq!(Reader::new(&o).varuint64().unwrap(), u64::MAX);
        for v in [0i64, 1, -1, 63, -64, 64, 1 << 40, -(1 << 50), i64::MAX] {
            let mut o = Vec::new();
            write_varint64(&mut o, v);
            assert_eq!(Reader::new(&o).varint64().unwrap(), v);
        }
        // SLI: small values as 4-byte int << 1, big ones flagged
        assert_eq!(Reader::new(&[0x54, 0, 0, 0]).sli_int64().unwrap(), 42);
        let mut big = vec![1u8];
        big.extend_from_slice(&(1i64 << 40).to_le_bytes());
        assert_eq!(Reader::new(&big).sli_int64().unwrap(), 1 << 40);
    }

    #[test]
    fn exact_bytes_current_spec() {
        let cfg = ForyConfig::default();
        assert_eq!(cfg.spec, XlangSpec::Current);
        // header 01 (xlang), NOT_NULL ff, STRING 21, (3<<2|latin1) 0c, "abc"
        assert_eq!(hex(&serialize(&Value::from("abc"), &cfg).unwrap()), "01ff150c616263");
        assert_eq!(hex(&serialize(&Value::Int(-2), &cfg).unwrap()), "01ff0703");
        assert_eq!(hex(&serialize(&Value::Float(0.5), &cfg).unwrap()), format!("01ff14{}", hex(&0.5f64.to_le_bytes())));
        assert_eq!(hex(&serialize(&Value::Null, &cfg).unwrap()), "01fd");
        // LIST 22, MAP 24 with STRING 21 keys
        assert_eq!(hex(&serialize(&Value::from(vec![Value::Int(1), Value::Int(2)]), &cfg).unwrap()), "01ff1602080702 04".replace(' ', ""));
        let m = Value::object([("a", Value::Int(1))]);
        assert_eq!(hex(&serialize(&m, &cfg).unwrap()), "01ff18010001150704 6102".replace(' ', ""));
        assert_eq!(deserialize(&serialize(&m, &cfg).unwrap(), &cfg).unwrap(), m);
        // current-only types: UINT8 9, UINT64 13, BFLOAT16 18, NONE 36, UINT16_ARRAY 49
        assert_eq!(deserialize(&[0x01, 0xff, 9, 200], &cfg).unwrap(), Value::Int(200));
        let mut b = vec![0x01, 0xff, 13];
        b.extend_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(deserialize(&b, &cfg).unwrap(), Value::Float(u64::MAX as f64));
        assert_eq!(deserialize(&[0x01, 0xff, 18, 0xc0, 0x3f], &cfg).unwrap(), Value::Float(1.5));
        assert_eq!(deserialize(&[0x01, 0xff, 36], &cfg).unwrap(), Value::Null);
        assert_eq!(deserialize(&[0x01, 0xff, 49, 4, 1, 0, 0xff, 0xff], &cfg).unwrap(), Value::from(vec![Value::Int(1), Value::Int(65535)]));
        // STRUCT 27 registered by id: the user type id follows as its own varuint32
        let cfg_s = ForyConfig { struct_types: parse_struct_types("#300=a").unwrap(), ..Default::default() };
        let v = deserialize(&[0x01, 0xff, 27, 0xac, 0x02, 0xff, 7, 2], &cfg_s).unwrap();
        assert_eq!(v, Value::object([("@type", Value::from("#300")), ("a", Value::Int(1))]));
        // out-of-band / non-xlang headers are rejected; legacy headers still decode
        assert!(deserialize(&[0x03, 0xff, 9, 1], &cfg).is_err());
        assert!(deserialize(&[0x00, 0xff, 9, 1], &cfg).is_err());
        assert_eq!(deserialize(&[0x06, 0x06, 0xff, 0x07, 0x03], &cfg).unwrap(), Value::Int(-2));
        // compatible structs round trip with the current TypeDef field ids
        let cc = ForyConfig { compatible: true, ..Default::default() };
        let p = Value::object([("@type", Value::from("example.P")), ("name", Value::from("x")), ("age", Value::Int(3))]);
        assert_eq!(deserialize(&serialize(&p, &cc).unwrap(), &cc).unwrap(), p);
        assert_eq!(XlangSpec::parse("0.11"), Some(XlangSpec::Legacy));
        assert_eq!(XlangSpec::Current.to_wire(types::LOCAL_DATE), 39);
        assert_eq!(XlangSpec::Current.from_wire(41), Some(types::BINARY));
    }

    #[test]
    fn exact_bytes_of_simple_values() {
        let cfg = ForyConfig { spec: XlangSpec::Legacy, ..Default::default() };
        // header 06 (LE | xlang), language 06, NOT_NULL ff, STRING 0c, (3<<2|latin1) 0c, "abc"
        assert_eq!(hex(&serialize(&Value::from("abc"), &cfg).unwrap()), "0606ff0c0c616263");
        // UTF-8 for non latin-1
        assert_eq!(hex(&serialize(&Value::from("€"), &cfg).unwrap()), "0606ff0c0ee282ac");
        // var int64 zigzag: -2 -> 3
        assert_eq!(hex(&serialize(&Value::Int(-2), &cfg).unwrap()), "0606ff0703");
        assert_eq!(hex(&serialize(&Value::Bool(true), &cfg).unwrap()), "0606ff0101");
        assert_eq!(hex(&serialize(&Value::Null, &cfg).unwrap()), "0706");
        // list of ints: len 2, header SAME_TYPE (8), elem type VAR_INT64, 1 -> 02, 2 -> 04
        assert_eq!(hex(&serialize(&Value::from(vec![Value::Int(1), Value::Int(2)]), &cfg).unwrap()), "0606ff1502080702 04".replace(' ', ""));
        // map {"a": 1}: size 1, chunk header 0, size 1, key STRING, value VAR_INT64, "a", 1
        let m = Value::object([("a", Value::Int(1))]);
        assert_eq!(hex(&serialize(&m, &cfg).unwrap()), "0606ff170100010c07046102");
    }

    #[test]
    fn decode_handwritten_xlang_payloads() {
        let cfg = ForyConfig::default();
        // legacy magic + header, INT32 fixed 7
        let b = [0xd4, 0x62, 0x06, 0x01, 0xff, 0x04, 7, 0, 0, 0];
        assert_eq!(deserialize(&b, &cfg).unwrap(), Value::Int(7));
        // UTF-16 string "hi"
        let b = [0x06, 0x00, 0xff, 0x0c, (4 << 2) | 1, b'h', 0, b'i', 0];
        assert_eq!(deserialize(&b, &cfg).unwrap(), Value::from("hi"));
        // FLOAT32 1.5, INT16_ARRAY [1, -1], LOCAL_DATE 1
        let mut b = vec![0x06, 0x00, 0xff, 0x0a];
        b.extend_from_slice(&1.5f32.to_le_bytes());
        assert_eq!(deserialize(&b, &cfg).unwrap(), Value::Float(1.5));
        let b = [0x06, 0x00, 0xff, 32, 4, 1, 0, 0xff, 0xff];
        assert_eq!(deserialize(&b, &cfg).unwrap(), Value::from(vec![Value::Int(1), Value::Int(-1)]));
        let b = [0x06, 0x00, 0xff, 26, 1, 0, 0, 0];
        assert_eq!(deserialize(&b, &cfg).unwrap(), Value::from("1970-01-02"));
        // list with nulls of different types (no same-type flag): header HAS_NULL
        let b = [0x06, 0x00, 0xff, 21, 3, 0b10, 0xff, 0x01, 0x01, 0xfd, 0xff, 0x0c, 0x04, b'x'];
        assert_eq!(deserialize(&b, &cfg).unwrap(), Value::from(vec![Value::Bool(true), Value::Null, Value::from("x")]));
        // java native payload rejected
        assert!(deserialize(&[0x02, 0x01, 0xff], &cfg).is_err());
        // references: list [ref-value "ab", ref -> 1]: tracking ref + same type
        let b = [0x06, 0x00, 0x00, 21, 2, 0b1001, 0x0c, 0x00, 0x08, b'a', b'b', 0xfe, 0x01];
        assert_eq!(deserialize(&b, &cfg).unwrap(), Value::from(vec!["ab", "ab"]));
    }

    #[test]
    fn meta_strings() {
        for s in ["example", "Person", "org.apache.fory", "camelCaseName", "with_digits123", "x-y", "MyHTTPServerHandlerFactory.v2", ""] {
            let (enc, data) = encode_meta_string(s);
            assert_eq!(decode_meta_string(enc as u8, &data).unwrap(), s, "{:?}", enc);
        }
        assert_eq!(encode_meta_string("example").0, MetaEncoding::LowerSpecial);
        assert_eq!(encode_meta_string("Person").0, MetaEncoding::FirstToLowerSpecial);
        assert_eq!(encode_meta_string("Abc123").0, MetaEncoding::LowerUpperDigitSpecial);
        // "abc" in LOWER_SPECIAL: flag 0, a=00000 b=00001 c=00010 -> 0000 0000 0010 0010 -> 00 22
        assert_eq!(hex(&encode_meta_string("abc").1), "0022");
        // murmur3 x64-128 reference vector ("hello", seed 0)
        assert_eq!(murmur3_x64_128(b"hello", 0).0, 0xcbd8a7b341bd9b02);
    }

    #[test]
    fn round_trips_and_structs() {
        let mut cfg = ForyConfig { ref_tracking: false, ..Default::default() };
        cfg.struct_types = parse_struct_types("example.Person=name,age,tags,address; example.Address=city").unwrap();
        let person = Value::object([
            ("@type", Value::from("example.Person")),
            ("name", Value::from("Ann")),
            ("age", Value::Int(42)),
            ("tags", Value::from(vec![Value::from("a"), Value::Null])),
            ("address", Value::object([("@type", Value::from("example.Address")), ("city", Value::from("Zürich"))])),
        ]);
        let v = Value::object([
            ("people", Value::Array(vec![person.clone(), person.clone()])),
            ("raw", Value::Bytes(vec![0, 1, 2])),
            ("pi", Value::Float(3.25)),
            ("none", Value::Null),
            ("mixed", Value::Array(vec![Value::Int(1), Value::from("two"), Value::Bool(false)])),
            ("big", Value::Int(i64::MAX)),
            ("long", Value::from("x".repeat(300))),
        ]);
        for track in [false, true] {
            cfg.ref_tracking = track;
            let b = serialize(&v, &cfg).unwrap();
            assert_eq!(deserialize(&b, &cfg).unwrap(), v, "track={}", track);
            if track {
                // the second person is a back reference
                assert!(b.contains(&(REF_FLAG as u8)));
            }
        }
        // a struct without field declarations cannot be read
        let b = serialize(&person, &cfg).unwrap();
        assert!(deserialize(&b, &ForyConfig::default()).unwrap_err().message.contains("structTypes"));
        // requireClassRegistration on write
        let strict = ForyConfig { require_class_registration: true, ..Default::default() };
        assert!(serialize(&person, &strict).is_err());
        // namespace meta string is shared between the two struct types (back reference)
        let ns = encode_meta_string("example").1;
        assert_eq!(b.windows(ns.len()).filter(|w| *w == ns.as_slice()).count(), 1);
    }

    #[test]
    fn long_meta_string_uses_hash() {
        let cfg = ForyConfig { struct_types: parse_struct_types("org.apache.camel.component.fory.model.VeryLongName=x").unwrap(), ..Default::default() };
        let v = Value::object([("@type", Value::from("org.apache.camel.component.fory.model.VeryLongName")), ("x", Value::Int(1))]);
        let b = serialize(&v, &cfg).unwrap();
        assert_eq!(deserialize(&b, &cfg).unwrap(), v);
    }

    #[test]
    fn decimals() {
        let cases = ["0", "1", "-1", "123.45", "-0.001", "4611686018427387903", "4611686018427387904", "-123456789012345678901234567890.12345", "1E+3", "100"];
        let want = ["0", "1", "-1", "123.45", "-0.001", "4611686018427387903", "4611686018427387904", "-123456789012345678901234567890.12345", "1000", "100"];
        for (c, w) in cases.iter().zip(want) {
            let mut o = Vec::new();
            write_decimal(&mut o, c).unwrap();
            assert_eq!(read_decimal(&mut Reader::new(&o)).unwrap(), w, "{}", c);
        }
        // 123.45: scale 2 (zigzag 4), unscaled 12345 small: zigzag 24690 << 1 = 49380
        let mut o = Vec::new();
        write_decimal(&mut o, "123.45").unwrap();
        let mut e = vec![4u8];
        write_varuint64(&mut e, 49380);
        assert_eq!(o, e);
        // big encoding: 2^62 -> len 8, sign 0: header ((8<<1)<<1)|1 = 33, magnitude LE
        let mut o = Vec::new();
        write_decimal(&mut o, "4611686018427387904").unwrap();
        assert_eq!(hex(&o), "00210000000000000040");
        let cfg = ForyConfig::default();
        let v = Value::object([("@decimal", Value::from("-9.99"))]);
        assert_eq!(deserialize(&serialize(&v, &cfg).unwrap(), &cfg).unwrap(), Value::from("-9.99"));
        assert!(write_decimal(&mut Vec::new(), "1.2.3").is_err());
    }

    #[test]
    fn compatible_structs_need_no_struct_types() {
        let cfg = ForyConfig { compatible: true, ..Default::default() };
        let person = Value::object([
            ("@type", Value::from("example.Person")),
            ("name", Value::from("Ann")),
            ("age", Value::Int(42)),
            ("score", Value::Float(1.5)),
            ("active", Value::Bool(true)),
            ("tags", Value::from(vec![Value::from("a")])),
            ("attrs", Value::object([("k", Value::from("v"))])),
            ("nick", Value::Null),
            ("address", Value::object([("@type", Value::from("example.Address")), ("city", Value::from("Bern"))])),
        ]);
        let v = Value::Array(vec![person.clone(), person.clone()]);
        let b = serialize(&v, &cfg).unwrap();
        // the second struct references the shared type definition
        let back = deserialize(&b, &ForyConfig::default()).unwrap();
        let fields = |v: &Value| {
            let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
            k.sort();
            k
        };
        for p in back.as_array().unwrap() {
            assert_eq!(fields(p), fields(&person));
            for (k, x) in person.as_object().unwrap().iter() {
                assert_eq!(p.get(k), Some(x), "{}", k);
            }
        }
        let td = TypeDef { name: "example.Person".into(), fields: vec![("a".into(), types::VAR_INT64, true), ("long_field_name_over_sixteen".into(), types::LIST, false)] };
        let enc = encode_type_def(&td);
        assert_eq!(decode_type_def(&mut Reader::new(&enc)).unwrap(), td);
        assert_eq!(u64::from_le_bytes(enc[..8].try_into().unwrap()) & 0xff, (enc.len() - 8) as u64);
        // many fields (escape in the meta header and the size byte)
        let many = TypeDef { name: "x.Y".into(), fields: (0..40).map(|i| (format!("field_with_a_long_name_{}", i), types::STRING, true)).collect() };
        let enc = encode_type_def(&many);
        assert_eq!(decode_type_def(&mut Reader::new(&enc)).unwrap(), many);
    }

    #[test]
    fn extension_types() {
        let mut cfg = ForyConfig::default();
        cfg.extensions.insert(
            "geo.Point".into(),
            ExtSerializer::new(
                |v, out| {
                    out.extend_from_slice(&v.get("x").and_then(|x| x.as_f64()).unwrap_or(0.0).to_le_bytes());
                    out.extend_from_slice(&v.get("y").and_then(|x| x.as_f64()).unwrap_or(0.0).to_le_bytes());
                    Ok(())
                },
                |r| {
                    let x = f64::from_le_bytes(r.take(8)?.try_into().unwrap());
                    let y = f64::from_le_bytes(r.take(8)?.try_into().unwrap());
                    Ok(Value::object([("@ext", Value::from("geo.Point")), ("x", Value::Float(x)), ("y", Value::Float(y))]))
                },
            ),
        );
        let p = Value::object([("@ext", Value::from("geo.Point")), ("x", Value::Float(1.0)), ("y", Value::Float(-2.5))]);
        let v = Value::object([("p", p.clone()), ("n", Value::Int(1))]);
        let b = serialize(&v, &cfg).unwrap();
        assert_eq!(deserialize(&b, &cfg).unwrap(), v);
        assert!(deserialize(&b, &ForyConfig::default()).is_err());
        assert!(serialize(&p, &ForyConfig::default()).is_err());
    }
}

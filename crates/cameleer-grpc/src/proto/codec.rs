//! Dynamic protobuf wire-format codec: [`Value`] <-> bytes, driven by descriptors.
//!
//! Native value mapping:
//!
//! | protobuf | `Value` |
//! |---|---|
//! | message | `Object` keyed by field name (the JSON name is accepted on input) |
//! | int32/sint32/sfixed32/uint32/fixed32, int64/sint64/sfixed64 | `Int` |
//! | uint64/fixed64 | `Int` (or a decimal `String` above `i64::MAX`) |
//! | float/double | `Float` |
//! | bool | `Bool` |
//! | string | `String` |
//! | bytes | `Bytes` (a `String` input is encoded as its UTF-8 bytes) |
//! | enum | `String` value name (unknown numbers decode as `Int`; `Int` accepted on input) |
//! | repeated | `Array` |
//! | map<K,V> | `Object` (keys as strings) |
//! | `google.protobuf.Struct` / `Value` / `ListValue` | `Object` / any value / `Array` |
//! | wrappers (`google.protobuf.StringValue`...) | the wrapped scalar |
//! | `google.protobuf.Timestamp` / `Duration` | `{seconds, nanos}` (RFC 3339 / `"1.5s"` strings accepted on input) |
//!
//! Encoding follows proto3 rules: implicit-presence fields holding the default value are
//! not written, repeated scalars are packed (unless `[packed=false]`), fields are written
//! in field-number order. Decoding accepts packed and unpacked repeated scalars, merges
//! repeated occurrences of singular message fields, honours `oneof` (last one wins) and
//! skips unknown fields. With [`DecodeOptions::include_defaults`] (default) absent
//! implicit-presence scalars, repeated and map fields are filled with their defaults.

use std::borrow::Cow;
use std::collections::HashMap;

use cameleer_core::error::{CamelError, Result};
use cameleer_core::value::{ObjectMap, Value};

use super::wkt::is_wrapper;
use super::{DescriptorPool, FieldDescriptor, FieldType, MessageDescriptor};

const MAX_DEPTH: usize = 100;

/// Decoding options.
#[derive(Clone, Debug)]
pub struct DecodeOptions {
    /// Fill absent implicit-presence scalars / repeated / map fields with defaults.
    pub include_defaults: bool,
}

impl Default for DecodeOptions {
    fn default() -> Self {
        DecodeOptions { include_defaults: true }
    }
}

fn err(msg: impl Into<String>) -> CamelError {
    CamelError::new("InvalidProtocolBufferException", msg.into())
}

fn arg_err(msg: impl Into<String>) -> CamelError {
    CamelError::illegal_argument(msg.into())
}

// ---------------------------------------------------------------------------
// wire primitives
// ---------------------------------------------------------------------------

/// Appends a base-128 varint.
pub fn put_varint(mut v: u64, out: &mut Vec<u8>) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Reads a varint at `*pos`.
pub fn get_varint(buf: &[u8], pos: &mut usize) -> Result<u64> {
    let mut v: u64 = 0;
    for i in 0..10 {
        let b = *buf.get(*pos).ok_or_else(|| err("truncated varint"))?;
        *pos += 1;
        v |= ((b & 0x7f) as u64) << (7 * i);
        if b & 0x80 == 0 {
            return Ok(v);
        }
    }
    Err(err("malformed varint"))
}

fn put_tag(num: u32, wt: u8, out: &mut Vec<u8>) {
    put_varint(((num as u64) << 3) | wt as u64, out);
}

fn zigzag32(v: i32) -> u64 {
    ((v << 1) ^ (v >> 31)) as u32 as u64
}
fn zigzag64(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}
fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

fn get_bytes<'a>(buf: &'a [u8], pos: &mut usize) -> Result<&'a [u8]> {
    let len = get_varint(buf, pos)? as usize;
    let end = pos.checked_add(len).filter(|e| *e <= buf.len()).ok_or_else(|| err("truncated length-delimited field"))?;
    let s = &buf[*pos..end];
    *pos = end;
    Ok(s)
}

fn get_fixed<const N: usize>(buf: &[u8], pos: &mut usize) -> Result<[u8; N]> {
    let s = buf.get(*pos..*pos + N).ok_or_else(|| err("truncated fixed-width field"))?;
    *pos += N;
    let mut a = [0u8; N];
    a.copy_from_slice(s);
    Ok(a)
}

fn skip_field(buf: &[u8], pos: &mut usize, wt: u8, depth: usize) -> Result<()> {
    match wt {
        0 => {
            get_varint(buf, pos)?;
        }
        1 => {
            get_fixed::<8>(buf, pos)?;
        }
        2 => {
            get_bytes(buf, pos)?;
        }
        5 => {
            get_fixed::<4>(buf, pos)?;
        }
        3 => {
            // group: skip until the matching end-group
            if depth > MAX_DEPTH {
                return Err(err("groups nested too deeply"));
            }
            loop {
                let tag = get_varint(buf, pos)?;
                let w = (tag & 7) as u8;
                if w == 4 {
                    return Ok(());
                }
                skip_field(buf, pos, w, depth + 1)?;
            }
        }
        _ => return Err(err(format!("invalid wire type {}", wt))),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// value conversions
// ---------------------------------------------------------------------------

fn to_i64(v: &Value, f: &str) -> Result<i64> {
    match v {
        Value::Int(i) => Ok(*i),
        Value::Float(x) if x.fract() == 0.0 && x.is_finite() => Ok(*x as i64),
        Value::Bool(b) => Ok(*b as i64),
        Value::String(s) => s.trim().parse::<i64>().or_else(|_| s.trim().parse::<f64>().map_err(|_| ()).and_then(|x| if x.fract() == 0.0 { Ok(x as i64) } else { Err(()) })).map_err(|_| arg_err(format!("Field {}: cannot convert \"{}\" to an integer", f, s))),
        other => Err(arg_err(format!("Field {}: expected an integer but got {}", f, other.type_name()))),
    }
}

fn to_u64(v: &Value, f: &str) -> Result<u64> {
    match v {
        Value::String(s) => s.trim().parse::<u64>().map_err(|_| arg_err(format!("Field {}: cannot convert \"{}\" to an unsigned integer", f, s))),
        other => {
            let i = to_i64(other, f)?;
            if i < 0 {
                return Err(arg_err(format!("Field {}: negative value {} for an unsigned field", f, i)));
            }
            Ok(i as u64)
        }
    }
}

fn to_f64(v: &Value, f: &str) -> Result<f64> {
    match v {
        Value::Float(x) => Ok(*x),
        Value::Int(i) => Ok(*i as f64),
        Value::String(s) => match s.trim() {
            "NaN" => Ok(f64::NAN),
            "Infinity" => Ok(f64::INFINITY),
            "-Infinity" => Ok(f64::NEG_INFINITY),
            t => t.parse::<f64>().map_err(|_| arg_err(format!("Field {}: cannot convert \"{}\" to a number", f, s))),
        },
        other => Err(arg_err(format!("Field {}: expected a number but got {}", f, other.type_name()))),
    }
}

fn to_bool(v: &Value, f: &str) -> Result<bool> {
    match v {
        Value::Bool(b) => Ok(*b),
        Value::Int(i) => Ok(*i != 0),
        Value::String(s) if s.eq_ignore_ascii_case("true") => Ok(true),
        Value::String(s) if s.eq_ignore_ascii_case("false") => Ok(false),
        other => Err(arg_err(format!("Field {}: expected a boolean but got {}", f, other.type_name()))),
    }
}

fn to_string(v: &Value, f: &str) -> Result<String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Int(_) | Value::Float(_) | Value::Bool(_) => Ok(v.to_string()),
        Value::Bytes(b) => String::from_utf8(b.clone()).map_err(|_| arg_err(format!("Field {}: bytes are not valid UTF-8", f))),
        other => Err(arg_err(format!("Field {}: expected a string but got {}", f, other.type_name()))),
    }
}

fn to_bytes(v: &Value, f: &str) -> Result<Vec<u8>> {
    match v {
        Value::Bytes(b) => Ok(b.clone()),
        Value::String(s) => Ok(s.as_bytes().to_vec()),
        Value::Array(a) => a.iter().map(|x| to_i64(x, f).map(|i| i as u8)).collect(),
        other => Err(arg_err(format!("Field {}: expected bytes but got {}", f, other.type_name()))),
    }
}

fn check_range(i: i64, lo: i64, hi: i64, f: &str) -> Result<i64> {
    if i < lo || i > hi {
        return Err(arg_err(format!("Field {}: value {} out of range", f, i)));
    }
    Ok(i)
}

fn enum_number(pool: &DescriptorPool, type_name: &str, v: &Value, f: &str) -> Result<i32> {
    let e = pool.enum_type(type_name).ok_or_else(|| arg_err(format!("Unknown enum type {}", type_name)))?;
    match v {
        Value::String(s) => match e.number_of(s) {
            Some(n) => Ok(n),
            None => s.trim().parse::<i32>().map_err(|_| arg_err(format!("Field {}: unknown value \"{}\" for enum {}", f, s, type_name))),
        },
        other => Ok(check_range(to_i64(other, f)?, i32::MIN as i64, i32::MAX as i64, f)? as i32),
    }
}

fn is_default(pool: &DescriptorPool, fd: &FieldDescriptor, v: &Value) -> bool {
    match fd.field_type {
        FieldType::String => v.as_str() == Some(""),
        FieldType::Bytes => matches!(v, Value::Bytes(b) if b.is_empty()) || v.as_str() == Some(""),
        FieldType::Bool => to_bool(v, "").map(|b| !b).unwrap_or(false),
        FieldType::Float | FieldType::Double => to_f64(v, "").map(|x| x == 0.0 && x.is_sign_positive()).unwrap_or(false),
        FieldType::Enum => enum_number(pool, &fd.type_name, v, "").map(|n| n == 0).unwrap_or(false),
        FieldType::Message => false,
        FieldType::Uint64 | FieldType::Fixed64 => to_u64(v, "").map(|n| n == 0).unwrap_or(false),
        _ => to_i64(v, "").map(|n| n == 0).unwrap_or(false),
    }
}

// ---------------------------------------------------------------------------
// well-known type adaptation
// ---------------------------------------------------------------------------

/// Native value -> message-shaped object for well-known types.
fn wkt_in<'a>(md: &MessageDescriptor, v: &'a Value) -> Result<Cow<'a, Value>> {
    let name = md.full_name.as_str();
    Ok(match name {
        "google.protobuf.Struct" => match v {
            Value::Object(_) => Cow::Owned(Value::object([("fields", v.clone())])),
            Value::Null => Cow::Owned(Value::Object(ObjectMap::new())),
            other => return Err(arg_err(format!("google.protobuf.Struct expects an object but got {}", other.type_name()))),
        },
        "google.protobuf.ListValue" => match v {
            Value::Array(_) => Cow::Owned(Value::object([("values", v.clone())])),
            other => return Err(arg_err(format!("google.protobuf.ListValue expects an array but got {}", other.type_name()))),
        },
        "google.protobuf.Value" => Cow::Owned(match v {
            Value::Null => Value::object([("null_value", Value::from("NULL_VALUE"))]),
            Value::Bool(b) => Value::object([("bool_value", Value::Bool(*b))]),
            Value::Int(i) => Value::object([("number_value", Value::Float(*i as f64))]),
            Value::Float(x) => Value::object([("number_value", Value::Float(*x))]),
            Value::String(s) => Value::object([("string_value", Value::String(s.clone()))]),
            Value::Bytes(b) => Value::object([("string_value", Value::String(String::from_utf8_lossy(b).into_owned()))]),
            Value::Array(_) => Value::object([("list_value", v.clone())]),
            Value::Object(_) => Value::object([("struct_value", v.clone())]),
        }),
        n if is_wrapper(n) => match v {
            Value::Object(m) if m.len() == 1 && m.contains_key("value") => Cow::Borrowed(v),
            Value::Null => Cow::Owned(Value::Object(ObjectMap::new())),
            other => Cow::Owned(Value::object([("value", other.clone())])),
        },
        "google.protobuf.Timestamp" => match v {
            Value::String(s) => {
                let (secs, nanos) = super::json::parse_timestamp(s).ok_or_else(|| arg_err(format!("Invalid RFC 3339 timestamp: {}", s)))?;
                Cow::Owned(Value::object([("seconds", Value::Int(secs)), ("nanos", Value::Int(nanos as i64))]))
            }
            _ => Cow::Borrowed(v),
        },
        "google.protobuf.Duration" => match v {
            Value::String(s) => {
                let (secs, nanos) = super::json::parse_duration(s).ok_or_else(|| arg_err(format!("Invalid duration: {}", s)))?;
                Cow::Owned(Value::object([("seconds", Value::Int(secs)), ("nanos", Value::Int(nanos as i64))]))
            }
            _ => Cow::Borrowed(v),
        },
        _ => Cow::Borrowed(v),
    })
}

/// Decoded message object -> native value for well-known types.
fn wkt_out(md: &MessageDescriptor, v: Value) -> Value {
    let name = md.full_name.as_str();
    let mut obj = match v {
        Value::Object(m) => m,
        other => return other,
    };
    match name {
        "google.protobuf.Struct" => obj.remove("fields").unwrap_or(Value::Object(ObjectMap::new())),
        "google.protobuf.ListValue" => obj.remove("values").unwrap_or(Value::Array(Vec::new())),
        "google.protobuf.Value" => {
            if let Some(x) = obj.remove("number_value") {
                match x {
                    Value::Float(f) if f.fract() == 0.0 && f.abs() < 9.007_199_254_740_992e15 => Value::Int(f as i64),
                    other => other,
                }
            } else if let Some(x) = obj.remove("string_value").or_else(|| obj.remove("bool_value")) {
                x
            } else if let Some(x) = obj.remove("struct_value").or_else(|| obj.remove("list_value")) {
                x
            } else {
                Value::Null
            }
        }
        n if is_wrapper(n) => obj.remove("value").unwrap_or(Value::Null),
        _ => Value::Object(obj),
    }
}

// ---------------------------------------------------------------------------
// encoding
// ---------------------------------------------------------------------------

/// Encodes `value` as message `md`.
pub fn encode_message(pool: &DescriptorPool, md: &MessageDescriptor, value: &Value) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    encode_into(pool, md, value, &mut out, 0)?;
    Ok(out)
}

fn encode_into(pool: &DescriptorPool, md: &MessageDescriptor, value: &Value, out: &mut Vec<u8>, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(arg_err("message nested too deeply"));
    }
    let value = wkt_in(md, value)?;
    let empty = ObjectMap::new();
    let obj = match value.as_ref() {
        Value::Object(m) => m,
        Value::Null => &empty,
        other => return Err(arg_err(format!("Message {} expects an object but got {}", md.full_name, other.type_name()))),
    };
    for k in obj.keys() {
        if md.field(k).is_none() {
            return Err(arg_err(format!("Message {} has no field named \"{}\"", md.full_name, k)));
        }
    }
    let mut fields: Vec<&FieldDescriptor> = md.fields.iter().collect();
    fields.sort_by_key(|f| f.number);
    for fd in fields {
        let v = match obj.get(&fd.name).or_else(|| obj.get(&fd.json_name)) {
            None | Some(Value::Null) => continue,
            Some(v) => v,
        };
        let fname = format!("{}.{}", md.name, fd.name);
        if fd.is_map {
            let entry = pool.message(&fd.type_name).ok_or_else(|| arg_err(format!("Unknown map entry type {}", fd.type_name)))?;
            let m = v.as_object().ok_or_else(|| arg_err(format!("Field {}: map expects an object but got {}", fname, v.type_name())))?;
            let (kf, vf) = (&entry.fields[0], &entry.fields[1]);
            for (k, val) in m {
                let mut buf = Vec::new();
                encode_field_value(pool, kf, &Value::String(k.clone()), &mut buf, &fname, depth)?;
                if !val.is_null() {
                    encode_field_value(pool, vf, val, &mut buf, &fname, depth)?;
                }
                put_tag(fd.number, 2, out);
                put_varint(buf.len() as u64, out);
                out.extend_from_slice(&buf);
            }
        } else if fd.is_repeated() {
            let items: Cow<'_, [Value]> = match v {
                Value::Array(a) => Cow::Borrowed(a.as_slice()),
                other => Cow::Owned(vec![other.clone()]),
            };
            if fd.packed == Some(true) && fd.field_type.is_packable() {
                if items.is_empty() {
                    continue;
                }
                let mut buf = Vec::new();
                for it in items.iter() {
                    encode_scalar(pool, fd, it, &mut buf, &fname)?;
                }
                put_tag(fd.number, 2, out);
                put_varint(buf.len() as u64, out);
                out.extend_from_slice(&buf);
            } else {
                for it in items.iter() {
                    encode_field_value(pool, fd, it, out, &fname, depth)?;
                }
            }
        } else {
            if !fd.has_presence() && is_default(pool, fd, v) {
                continue;
            }
            encode_field_value(pool, fd, v, out, &fname, depth)?;
        }
    }
    Ok(())
}

/// Tag + value of one (non-packed) field occurrence.
fn encode_field_value(pool: &DescriptorPool, fd: &FieldDescriptor, v: &Value, out: &mut Vec<u8>, fname: &str, depth: usize) -> Result<()> {
    put_tag(fd.number, fd.field_type.wire_type(), out);
    match fd.field_type {
        FieldType::Message => {
            let md = pool.message(&fd.type_name).ok_or_else(|| arg_err(format!("Unknown message type {}", fd.type_name)))?;
            let mut buf = Vec::new();
            encode_into(pool, &md, v, &mut buf, depth + 1)?;
            put_varint(buf.len() as u64, out);
            out.extend_from_slice(&buf);
        }
        FieldType::String => {
            let s = to_string(v, fname)?;
            put_varint(s.len() as u64, out);
            out.extend_from_slice(s.as_bytes());
        }
        FieldType::Bytes => {
            let b = to_bytes(v, fname)?;
            put_varint(b.len() as u64, out);
            out.extend_from_slice(&b);
        }
        _ => encode_scalar(pool, fd, v, out, fname)?,
    }
    Ok(())
}

/// The payload of a numeric scalar (no tag).
fn encode_scalar(pool: &DescriptorPool, fd: &FieldDescriptor, v: &Value, out: &mut Vec<u8>, f: &str) -> Result<()> {
    const I32: (i64, i64) = (i32::MIN as i64, i32::MAX as i64);
    match fd.field_type {
        FieldType::Int32 => put_varint(check_range(to_i64(v, f)?, I32.0, I32.1, f)? as u64, out),
        FieldType::Int64 => put_varint(to_i64(v, f)? as u64, out),
        FieldType::Uint32 => put_varint(check_range(to_u64(v, f)? as i64, 0, u32::MAX as i64, f)? as u64, out),
        FieldType::Uint64 => put_varint(to_u64(v, f)?, out),
        FieldType::Sint32 => put_varint(zigzag32(check_range(to_i64(v, f)?, I32.0, I32.1, f)? as i32), out),
        FieldType::Sint64 => put_varint(zigzag64(to_i64(v, f)?), out),
        FieldType::Bool => put_varint(to_bool(v, f)? as u64, out),
        FieldType::Enum => put_varint(enum_number(pool, &fd.type_name, v, f)? as i64 as u64, out),
        FieldType::Fixed32 => out.extend_from_slice(&(check_range(to_u64(v, f)? as i64, 0, u32::MAX as i64, f)? as u32).to_le_bytes()),
        FieldType::Sfixed32 => out.extend_from_slice(&(check_range(to_i64(v, f)?, I32.0, I32.1, f)? as i32).to_le_bytes()),
        FieldType::Float => out.extend_from_slice(&(to_f64(v, f)? as f32).to_le_bytes()),
        FieldType::Fixed64 => out.extend_from_slice(&to_u64(v, f)?.to_le_bytes()),
        FieldType::Sfixed64 => out.extend_from_slice(&to_i64(v, f)?.to_le_bytes()),
        FieldType::Double => out.extend_from_slice(&to_f64(v, f)?.to_le_bytes()),
        FieldType::String | FieldType::Bytes | FieldType::Message => return Err(arg_err(format!("Field {}: not a packable type", f))),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// decoding
// ---------------------------------------------------------------------------

/// Decodes message `md` from `bytes`.
pub fn decode_message(pool: &DescriptorPool, md: &MessageDescriptor, bytes: &[u8], opts: &DecodeOptions) -> Result<Value> {
    decode_depth(pool, md, bytes, opts, 0)
}

fn decode_depth(pool: &DescriptorPool, md: &MessageDescriptor, bytes: &[u8], opts: &DecodeOptions, depth: usize) -> Result<Value> {
    if depth > MAX_DEPTH {
        return Err(err("message nested too deeply"));
    }
    let mut map = ObjectMap::new();
    // singular message fields: concatenated encodings (merge semantics)
    let mut pending: HashMap<u32, Vec<u8>> = HashMap::new();
    let mut pos = 0;
    while pos < bytes.len() {
        let tag = get_varint(bytes, &mut pos)?;
        let num = (tag >> 3) as u32;
        let wt = (tag & 7) as u8;
        if num == 0 {
            return Err(err("invalid field number 0"));
        }
        let Some(fd) = md.field_by_number(num) else {
            skip_field(bytes, &mut pos, wt, depth)?;
            continue;
        };
        if let Some(o) = fd.oneof_index {
            for other in md.fields.iter().filter(|f| f.oneof_index == Some(o) && f.number != num) {
                map.remove(&other.name);
                pending.remove(&other.number);
            }
        }
        if fd.is_map {
            if wt != 2 {
                return Err(err(format!("wire type mismatch for map field {}", fd.name)));
            }
            let entry_bytes = get_bytes(bytes, &mut pos)?;
            let entry = pool.message(&fd.type_name).ok_or_else(|| err(format!("unknown map entry type {}", fd.type_name)))?;
            let ev = decode_depth(pool, &entry, entry_bytes, &DecodeOptions { include_defaults: true }, depth + 1)?;
            let (k, v) = match ev {
                Value::Object(mut m) => (m.remove("key").unwrap_or(Value::Null), m.remove("value").unwrap_or(Value::Null)),
                _ => (Value::Null, Value::Null),
            };
            let v = if v.is_null() && entry.fields[1].field_type == FieldType::Message {
                let vmd = pool.message(&entry.fields[1].type_name).ok_or_else(|| err("unknown map value type"))?;
                decode_depth(pool, &vmd, &[], opts, depth + 1)?
            } else {
                v
            };
            let slot = map.entry(fd.name.clone()).or_insert_with(|| Value::Object(ObjectMap::new()));
            if let Value::Object(m) = slot {
                m.insert(k.to_string(), v);
            }
            continue;
        }
        if fd.is_repeated() {
            let slot = map.entry(fd.name.clone()).or_insert_with(|| Value::Array(Vec::new()));
            let mut items = Vec::new();
            if wt == 2 && fd.field_type.is_packable() {
                let packed = get_bytes(bytes, &mut pos)?;
                let mut p = 0;
                while p < packed.len() {
                    items.push(decode_scalar(pool, fd, packed, &mut p, fd.field_type.wire_type())?);
                }
            } else {
                items.push(decode_value(pool, fd, bytes, &mut pos, wt, opts, depth)?);
            }
            if let Value::Array(a) = slot {
                a.extend(items);
            }
            continue;
        }
        if fd.field_type == FieldType::Message {
            if wt != 2 {
                return Err(err(format!("wire type mismatch for field {}", fd.name)));
            }
            let b = get_bytes(bytes, &mut pos)?;
            pending.entry(num).or_default().extend_from_slice(b);
            map.insert(fd.name.clone(), Value::Null); // placeholder, keeps oneof bookkeeping simple
            continue;
        }
        let v = decode_value(pool, fd, bytes, &mut pos, wt, opts, depth)?;
        map.insert(fd.name.clone(), v);
    }
    for (num, b) in pending {
        let fd = md.field_by_number(num).expect("known field");
        let sub = pool.message(&fd.type_name).ok_or_else(|| err(format!("unknown message type {}", fd.type_name)))?;
        map.insert(fd.name.clone(), decode_depth(pool, &sub, &b, opts, depth + 1)?);
    }
    if opts.include_defaults {
        for fd in &md.fields {
            if map.contains_key(&fd.name) {
                continue;
            }
            if fd.is_map {
                map.insert(fd.name.clone(), Value::Object(ObjectMap::new()));
            } else if fd.is_repeated() {
                map.insert(fd.name.clone(), Value::Array(Vec::new()));
            } else if !fd.has_presence() {
                map.insert(fd.name.clone(), default_value(pool, fd));
            }
        }
    }
    // Fields come out in declaration order (like protobuf-java's `getAllFields` / `JsonFormat`),
    // whatever their order on the wire.
    let mut ordered = ObjectMap::with_capacity(map.len());
    for fd in &md.fields {
        if let Some(v) = map.remove(&fd.name) {
            ordered.insert(fd.name.clone(), v);
        }
    }
    Ok(wkt_out(md, Value::Object(ordered)))
}

/// The default value of an implicit-presence scalar field.
pub fn default_value(pool: &DescriptorPool, fd: &FieldDescriptor) -> Value {
    match fd.field_type {
        FieldType::String => Value::String(String::new()),
        FieldType::Bytes => Value::Bytes(Vec::new()),
        FieldType::Bool => Value::Bool(false),
        FieldType::Float | FieldType::Double => Value::Float(0.0),
        FieldType::Enum => pool.enum_type(&fd.type_name).map(|e| Value::String(e.default_value().0)).unwrap_or(Value::Int(0)),
        FieldType::Message => Value::Null,
        _ => Value::Int(0),
    }
}

fn decode_value(pool: &DescriptorPool, fd: &FieldDescriptor, buf: &[u8], pos: &mut usize, wt: u8, opts: &DecodeOptions, depth: usize) -> Result<Value> {
    let expected = fd.field_type.wire_type();
    if wt != expected {
        return Err(err(format!("wire type mismatch for field {} (got {}, expected {})", fd.name, wt, expected)));
    }
    match fd.field_type {
        FieldType::Message => {
            let b = get_bytes(buf, pos)?;
            let md = pool.message(&fd.type_name).ok_or_else(|| err(format!("unknown message type {}", fd.type_name)))?;
            decode_depth(pool, &md, b, opts, depth + 1)
        }
        FieldType::String => {
            let b = get_bytes(buf, pos)?;
            String::from_utf8(b.to_vec()).map(Value::String).map_err(|_| err(format!("field {} contains invalid UTF-8", fd.name)))
        }
        FieldType::Bytes => Ok(Value::Bytes(get_bytes(buf, pos)?.to_vec())),
        _ => decode_scalar(pool, fd, buf, pos, wt),
    }
}

fn decode_scalar(pool: &DescriptorPool, fd: &FieldDescriptor, buf: &[u8], pos: &mut usize, wt: u8) -> Result<Value> {
    if wt != fd.field_type.wire_type() {
        return Err(err(format!("wire type mismatch for field {}", fd.name)));
    }
    let u64_value = |u: u64| if u > i64::MAX as u64 { Value::String(u.to_string()) } else { Value::Int(u as i64) };
    Ok(match fd.field_type {
        FieldType::Int32 => Value::Int(get_varint(buf, pos)? as i32 as i64),
        FieldType::Int64 => Value::Int(get_varint(buf, pos)? as i64),
        FieldType::Uint32 => Value::Int(get_varint(buf, pos)? as u32 as i64),
        FieldType::Uint64 => u64_value(get_varint(buf, pos)?),
        FieldType::Sint32 => Value::Int(unzigzag(get_varint(buf, pos)? as u32 as u64) as i32 as i64),
        FieldType::Sint64 => Value::Int(unzigzag(get_varint(buf, pos)?)),
        FieldType::Bool => Value::Bool(get_varint(buf, pos)? != 0),
        FieldType::Enum => {
            let n = get_varint(buf, pos)? as i32;
            match pool.enum_type(&fd.type_name).and_then(|e| e.name_of(n).map(|s| s.to_string())) {
                Some(name) => Value::String(name),
                None => Value::Int(n as i64),
            }
        }
        FieldType::Fixed32 => Value::Int(u32::from_le_bytes(get_fixed::<4>(buf, pos)?) as i64),
        FieldType::Sfixed32 => Value::Int(i32::from_le_bytes(get_fixed::<4>(buf, pos)?) as i64),
        FieldType::Float => Value::Float(f32::from_le_bytes(get_fixed::<4>(buf, pos)?) as f64),
        FieldType::Fixed64 => u64_value(u64::from_le_bytes(get_fixed::<8>(buf, pos)?)),
        FieldType::Sfixed64 => Value::Int(i64::from_le_bytes(get_fixed::<8>(buf, pos)?)),
        FieldType::Double => Value::Float(f64::from_le_bytes(get_fixed::<8>(buf, pos)?)),
        FieldType::String | FieldType::Bytes | FieldType::Message => return Err(err("not a scalar")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_and_zigzag() {
        let mut out = Vec::new();
        put_varint(150, &mut out);
        assert_eq!(out, [0x96, 0x01]);
        let mut p = 0;
        assert_eq!(get_varint(&out, &mut p).unwrap(), 150);
        out.clear();
        put_varint(-1i64 as u64, &mut out);
        assert_eq!(out.len(), 10);
        assert_eq!(zigzag32(0), 0);
        assert_eq!(zigzag32(-1), 1);
        assert_eq!(zigzag32(1), 2);
        assert_eq!(zigzag32(-2), 3);
        assert_eq!(zigzag32(i32::MAX), 0xffff_fffe);
        assert_eq!(zigzag32(i32::MIN), 0xffff_ffff);
        assert_eq!(unzigzag(3), -2);
        assert_eq!(unzigzag(zigzag64(i64::MIN)), i64::MIN);
        let mut p = 0;
        assert!(get_varint(&[0x80, 0x80], &mut p).is_err());
    }
}

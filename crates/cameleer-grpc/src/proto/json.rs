//! proto3 JSON mapping between native values (see [`super::codec`]) and JSON values:
//! lowerCamelCase field names (`json_name`), 64-bit integers as strings, bytes as base64,
//! enums by name, `Timestamp` as RFC 3339, `Duration` as `"1.5s"`, wrappers as plain
//! scalars, `Struct`/`Value`/`ListValue` as plain JSON, NaN/Infinity as strings.
//! On input both the original field names and the JSON names are accepted.


use cameleer_core::dataformat::base64;
use cameleer_core::error::{CamelError, Result};
use cameleer_core::util_time::{civil_from_epoch, epoch_from_civil};
use cameleer_core::value::{ObjectMap, Value};

use super::wkt::is_wrapper;
use super::{DescriptorPool, FieldDescriptor, FieldType, MessageDescriptor};

fn arg_err(msg: impl Into<String>) -> CamelError {
    CamelError::illegal_argument(msg.into())
}

/// Formats seconds + nanos as RFC 3339 UTC (`1972-01-01T10:00:20.021Z`).
pub fn format_timestamp(secs: i64, nanos: i32) -> String {
    let (y, mo, d, h, mi, s) = civil_from_epoch(secs);
    let mut out = format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", y, mo, d, h, mi, s);
    out.push_str(&frac(nanos));
    out.push('Z');
    out
}

fn frac(nanos: i32) -> String {
    let n = nanos.unsigned_abs();
    if n == 0 {
        String::new()
    } else if n % 1_000_000 == 0 {
        format!(".{:03}", n / 1_000_000)
    } else if n % 1_000 == 0 {
        format!(".{:06}", n / 1_000)
    } else {
        format!(".{:09}", n)
    }
}

/// Parses an RFC 3339 timestamp into (seconds, nanos).
pub fn parse_timestamp(s: &str) -> Option<(i64, i32)> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !(b[10] == b'T' || b[10] == b't' || b[10] == b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |a: usize, z: usize| s.get(a..z)?.parse::<u32>().ok();
    let (y, mo, d, h, mi, sec) = (num(0, 4)? as i64, num(5, 7)?, num(8, 10)?, num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut i = 19;
    let mut nanos: i64 = 0;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        let digits = &s[start..i];
        if digits.is_empty() || digits.len() > 9 {
            return None;
        }
        nanos = digits.parse::<i64>().ok()? * 10i64.pow(9 - digits.len() as u32);
    }
    let rest = &s[i..];
    let offset: i64 = if rest.eq_ignore_ascii_case("z") {
        0
    } else if rest.len() == 6 && (rest.starts_with('+') || rest.starts_with('-')) && &rest[3..4] == ":" {
        let oh: i64 = rest[1..3].parse().ok()?;
        let om: i64 = rest[4..6].parse().ok()?;
        let o = oh * 3600 + om * 60;
        if rest.starts_with('-') {
            -o
        } else {
            o
        }
    } else {
        return None;
    };
    Some((epoch_from_civil(y, mo, d, h, mi, sec) - offset, nanos as i32))
}

/// Formats a duration as `"<seconds>[.fraction]s"`.
pub fn format_duration(secs: i64, nanos: i32) -> String {
    let neg = secs < 0 || nanos < 0;
    format!("{}{}{}s", if neg { "-" } else { "" }, secs.unsigned_abs(), frac(nanos))
}

/// Parses `"1.5s"` / `"-0.001s"` into (seconds, nanos).
pub fn parse_duration(s: &str) -> Option<(i64, i32)> {
    let t = s.trim().strip_suffix('s')?;
    let (neg, t) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t),
    };
    let (int, fr) = match t.split_once('.') {
        Some((a, b)) => (a, b),
        None => (t, ""),
    };
    let secs: i64 = int.parse().ok()?;
    let nanos: i64 = if fr.is_empty() {
        0
    } else {
        if fr.len() > 9 || !fr.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        fr.parse::<i64>().ok()? * 10i64.pow(9 - fr.len() as u32)
    };
    Some(if neg { (-secs, -(nanos as i32)) } else { (secs, nanos as i32) })
}

fn int_of(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Int(i)) => *i,
        Some(Value::String(s)) => s.parse().unwrap_or(0),
        Some(Value::Float(f)) => *f as i64,
        _ => 0,
    }
}

fn float_json(x: f64) -> Value {
    if x.is_nan() {
        Value::from("NaN")
    } else if x.is_infinite() {
        Value::from(if x > 0.0 { "Infinity" } else { "-Infinity" })
    } else {
        Value::Float(x)
    }
}

/// Native -> JSON mapping for message `md`.
pub fn to_json(pool: &DescriptorPool, md: &MessageDescriptor, v: &Value) -> Result<Value> {
    let name = md.full_name.as_str();
    match name {
        "google.protobuf.Timestamp" => {
            if let Value::String(_) = v {
                return Ok(v.clone());
            }
            return Ok(Value::String(format_timestamp(int_of(v.get("seconds")), int_of(v.get("nanos")) as i32)));
        }
        "google.protobuf.Duration" => {
            if let Value::String(_) = v {
                return Ok(v.clone());
            }
            return Ok(Value::String(format_duration(int_of(v.get("seconds")), int_of(v.get("nanos")) as i32)));
        }
        "google.protobuf.Struct" | "google.protobuf.Value" | "google.protobuf.ListValue" => return Ok(v.clone()),
        n if is_wrapper(n) => {
            let inner = match v {
                Value::Object(m) if m.len() == 1 && m.contains_key("value") => m["value"].clone(),
                other => other.clone(),
            };
            return scalar_to_json(pool, &md.fields[0], &inner);
        }
        _ => {}
    }
    let obj = match v {
        Value::Object(m) => m,
        Value::Null => return Ok(Value::Null),
        other => return Err(arg_err(format!("Message {} expects an object but got {}", md.full_name, other.type_name()))),
    };
    let mut out = ObjectMap::new();
    for (k, val) in obj {
        let fd = md.field(k).ok_or_else(|| arg_err(format!("Message {} has no field named \"{}\"", md.full_name, k)))?;
        if val.is_null() {
            continue;
        }
        out.insert(fd.json_name.clone(), field_to_json(pool, fd, val)?);
    }
    Ok(Value::Object(out))
}

fn field_to_json(pool: &DescriptorPool, fd: &FieldDescriptor, v: &Value) -> Result<Value> {
    if fd.is_map {
        let entry = pool.message(&fd.type_name).ok_or_else(|| arg_err("unknown map entry"))?;
        let m = v.as_object().ok_or_else(|| arg_err(format!("Field {}: map expects an object", fd.name)))?;
        let mut out = ObjectMap::new();
        for (k, x) in m {
            out.insert(k.clone(), scalar_to_json(pool, &entry.fields[1], x)?);
        }
        return Ok(Value::Object(out));
    }
    if fd.is_repeated() {
        return match v {
            Value::Array(a) => Ok(Value::Array(a.iter().map(|x| scalar_to_json(pool, fd, x)).collect::<Result<_>>()?)),
            other => Ok(Value::Array(vec![scalar_to_json(pool, fd, other)?])),
        };
    }
    scalar_to_json(pool, fd, v)
}

fn scalar_to_json(pool: &DescriptorPool, fd: &FieldDescriptor, v: &Value) -> Result<Value> {
    Ok(match fd.field_type {
        FieldType::Message => {
            let md = pool.message(&fd.type_name).ok_or_else(|| arg_err(format!("unknown message type {}", fd.type_name)))?;
            to_json(pool, &md, v)?
        }
        FieldType::Int64 | FieldType::Sint64 | FieldType::Sfixed64 | FieldType::Uint64 | FieldType::Fixed64 => match v {
            Value::String(_) => v.clone(),
            other => Value::String(other.to_string()),
        },
        FieldType::Bytes => match v {
            Value::Bytes(b) => Value::String(base64::encode(b, false)),
            Value::String(s) => Value::String(base64::encode(s.as_bytes(), false)),
            other => other.clone(),
        },
        FieldType::Float | FieldType::Double => match v {
            Value::Float(x) => float_json(*x),
            other => other.clone(),
        },
        FieldType::Enum => match v {
            Value::Int(n) => pool.enum_type(&fd.type_name).and_then(|e| e.name_of(*n as i32).map(Value::from)).unwrap_or(Value::Int(*n)),
            other => other.clone(),
        },
        _ => v.clone(),
    })
}

/// JSON -> native mapping for message `md`.
pub fn from_json(pool: &DescriptorPool, md: &MessageDescriptor, v: &Value) -> Result<Value> {
    let name = md.full_name.as_str();
    match name {
        "google.protobuf.Timestamp" => {
            return match v {
                Value::String(s) => {
                    let (secs, nanos) = parse_timestamp(s).ok_or_else(|| arg_err(format!("Invalid RFC 3339 timestamp: {}", s)))?;
                    Ok(Value::object([("seconds", Value::Int(secs)), ("nanos", Value::Int(nanos as i64))]))
                }
                other => Ok(other.clone()),
            }
        }
        "google.protobuf.Duration" => {
            return match v {
                Value::String(s) => {
                    let (secs, nanos) = parse_duration(s).ok_or_else(|| arg_err(format!("Invalid duration: {}", s)))?;
                    Ok(Value::object([("seconds", Value::Int(secs)), ("nanos", Value::Int(nanos as i64))]))
                }
                other => Ok(other.clone()),
            }
        }
        "google.protobuf.Struct" | "google.protobuf.Value" | "google.protobuf.ListValue" => return Ok(v.clone()),
        n if is_wrapper(n) => return scalar_from_json(pool, &md.fields[0], v),
        _ => {}
    }
    let obj = match v {
        Value::Object(m) => m,
        Value::Null => return Ok(Value::Null),
        other => return Err(arg_err(format!("Message {} expects a JSON object but got {}", md.full_name, other.type_name()))),
    };
    let mut out = ObjectMap::new();
    for (k, val) in obj {
        let fd = md.field(k).ok_or_else(|| arg_err(format!("Message {} has no field named \"{}\"", md.full_name, k)))?;
        if val.is_null() {
            continue;
        }
        let nv = if fd.is_map {
            let entry = pool.message(&fd.type_name).ok_or_else(|| arg_err("unknown map entry"))?;
            let m = val.as_object().ok_or_else(|| arg_err(format!("Field {}: map expects an object", fd.name)))?;
            let mut o = ObjectMap::new();
            for (mk, x) in m {
                o.insert(mk.clone(), scalar_from_json(pool, &entry.fields[1], x)?);
            }
            Value::Object(o)
        } else if fd.is_repeated() {
            match val {
                Value::Array(a) => Value::Array(a.iter().map(|x| scalar_from_json(pool, fd, x)).collect::<Result<_>>()?),
                other => Value::Array(vec![scalar_from_json(pool, fd, other)?]),
            }
        } else {
            scalar_from_json(pool, fd, val)?
        };
        out.insert(fd.name.clone(), nv);
    }
    Ok(Value::Object(out))
}

fn scalar_from_json(pool: &DescriptorPool, fd: &FieldDescriptor, v: &Value) -> Result<Value> {
    Ok(match fd.field_type {
        FieldType::Message => {
            let md = pool.message(&fd.type_name).ok_or_else(|| arg_err(format!("unknown message type {}", fd.type_name)))?;
            from_json(pool, &md, v)?
        }
        FieldType::Bytes => match v {
            Value::String(s) => {
                let std = s.replace('-', "+").replace('_', "/");
                Value::Bytes(base64::decode(&std).map_err(|e| arg_err(format!("Field {}: invalid base64: {}", fd.name, e.message)))?)
            }
            other => other.clone(),
        },
        FieldType::Float | FieldType::Double => match v {
            Value::String(s) => match s.as_str() {
                "NaN" => Value::Float(f64::NAN),
                "Infinity" => Value::Float(f64::INFINITY),
                "-Infinity" => Value::Float(f64::NEG_INFINITY),
                t => Value::Float(t.parse::<f64>().map_err(|_| arg_err(format!("Field {}: invalid number {}", fd.name, t)))?),
            },
            Value::Int(i) => Value::Float(*i as f64),
            other => other.clone(),
        },
        FieldType::Uint64 | FieldType::Fixed64 => match v {
            Value::String(s) => match s.parse::<i64>() {
                Ok(i) => Value::Int(i),
                Err(_) => {
                    s.parse::<u64>().map_err(|_| arg_err(format!("Field {}: invalid integer {}", fd.name, s)))?;
                    Value::String(s.clone())
                }
            },
            other => other.clone(),
        },
        FieldType::Enum | FieldType::String | FieldType::Bool => v.clone(),
        _ => match v {
            Value::String(s) => Value::Int(s.trim().parse::<i64>().map_err(|_| arg_err(format!("Field {}: invalid integer {}", fd.name, s)))?),
            Value::Float(x) if x.fract() == 0.0 => Value::Int(*x as i64),
            other => other.clone(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_and_durations() {
        assert_eq!(format_timestamp(0, 0), "1970-01-01T00:00:00Z");
        assert_eq!(format_timestamp(1_700_000_000, 21_000_000), "2023-11-14T22:13:20.021Z");
        assert_eq!(parse_timestamp("2023-11-14T22:13:20.021Z"), Some((1_700_000_000, 21_000_000)));
        assert_eq!(parse_timestamp("2023-11-14T23:13:20+01:00"), Some((1_700_000_000, 0)));
        assert_eq!(parse_timestamp("1970-01-01T00:00:00.000000001Z"), Some((0, 1)));
        assert!(parse_timestamp("yesterday").is_none());
        assert_eq!(format_duration(1, 500_000_000), "1.500s");
        assert_eq!(format_duration(0, -1_000), "-0.000001s");
        assert_eq!(parse_duration("1.5s"), Some((1, 500_000_000)));
        assert_eq!(parse_duration("-3s"), Some((-3, 0)));
        assert!(parse_duration("3m").is_none());
    }
}

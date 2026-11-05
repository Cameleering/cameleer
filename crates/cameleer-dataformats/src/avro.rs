//! `avro` data format (Apache Avro 1.11 specification).
//!
//! * JSON schema parser: primitives, `record` (`error`), `enum`, `array`, `map`, unions,
//!   `fixed`, named type references with namespaces and aliases, field defaults, and the
//!   logical types `date`, `time-millis`, `time-micros`, `timestamp-millis`,
//!   `timestamp-micros`, `local-timestamp-millis/micros`, `decimal` (bytes or fixed), `uuid`
//!   and `duration`.
//! * Binary encoding (zigzag varints, length-prefixed bytes/strings, blocked arrays/maps),
//!   Avro JSON encoding, and Object Container Files (`null` and `deflate` codecs).
//!
//! Generic values map to `Value`: records and maps are objects, enums strings, `bytes` and
//! `fixed` bytes, unions the value of the selected branch (in the JSON encoding, non-null
//! branches are wrapped as `{"type": value}`). With `logicalTypeConversions` (default true)
//! dates/times decode to ISO-8601 strings (`2024-01-31`, `2024-01-31T10:00:00.000Z`),
//! decimals to decimal strings and durations to `{months, days, millis}`; encoding accepts
//! both these forms and the raw numbers/bytes.
//!
//! Options (Camel's `avro` data format plus extensions):
//! * `instanceClassName`: the full name of the schema's type to use (selects a named type
//!   in `schema`/`schemaFile`), or a registry bean holding the schema (JSON text or [`AvroSchema`]).
//! * `schema` (inline JSON), `schemaFile` / `schemaLocation` (`.avsc` file).
//! * `encoding`: `binary` (default, a bare datum like Camel), `json`, or `container`
//!   (Object Container File; unmarshal reads the writer schema from the file header and
//!   needs no schema option). `codec` (`null`/`deflate`) and `syncInterval` for containers.
//! * `contentTypeHeader` (default true): sets `Content-Type` (`avro/binary`, `application/json`
//!   or `avro/binary` for containers).
//! * `avroJackson` (`avro-jackson`): the same with `library=Jackson` semantics - marshal
//!   accepts JSON text (or values), unmarshal returns JSON text when `jsonView=true`
//!   (default for avroJackson: false, values).

use std::collections::HashMap;
use std::sync::Arc;

use cameleer_core::dataformat::compress::{deflate, inflate};
use cameleer_core::dataformat::DataFormat;
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::props::CONTENT_TYPE;
use cameleer_core::message::Exchange;
use cameleer_core::value::ObjectMap;
use cameleer_core::Value;

use crate::{body_bytes, body_text, df_err, Opts};

fn avro_err(msg: impl Into<String>) -> CamelError {
    CamelError::new("AvroRuntimeException", msg)
}

// ---------------------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------------------

/// A logical type annotation.
#[derive(Clone, Debug, PartialEq)]
pub enum Logical {
    Date,
    TimeMillis,
    TimeMicros,
    TimestampMillis,
    TimestampMicros,
    LocalTimestampMillis,
    LocalTimestampMicros,
    Decimal { precision: u32, scale: u32 },
    Uuid,
    Duration,
}

/// A record field.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub name: String,
    pub schema: Schema,
    pub default: Option<Value>,
    pub aliases: Vec<String>,
    pub doc: Option<String>,
}

/// A named type definition.
#[derive(Clone, Debug, PartialEq)]
pub enum NamedKind {
    Record { fields: Vec<Field>, error: bool },
    Enum { symbols: Vec<String>, default: Option<String> },
    Fixed { size: usize },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Named {
    /// Full name (`namespace.name`).
    pub name: String,
    pub aliases: Vec<String>,
    pub doc: Option<String>,
    pub kind: NamedKind,
    pub logical: Option<Logical>,
}

/// A schema node. Named types are stored once in [`AvroSchema::named`] and referenced by index.
#[derive(Clone, Debug, PartialEq)]
pub enum Schema {
    Null,
    Boolean,
    Int,
    Long,
    Float,
    Double,
    Bytes,
    String,
    Array(Box<Schema>),
    Map(Box<Schema>),
    Union(Vec<Schema>),
    Named(usize),
    Logical(Box<Schema>, Logical),
}

/// A parsed schema with its named types.
#[derive(Clone, Debug, PartialEq)]
pub struct AvroSchema {
    pub root: Schema,
    pub named: Vec<Named>,
}

struct Parser {
    named: Vec<Named>,
    index: HashMap<String, usize>,
}

fn full_name(name: &str, ns: Option<&str>) -> String {
    if name.contains('.') {
        name.to_string()
    } else {
        match ns {
            Some(n) if !n.is_empty() => format!("{}.{}", n, name),
            _ => name.to_string(),
        }
    }
}

fn namespace_of(full: &str) -> Option<String> {
    full.rsplit_once('.').map(|(n, _)| n.to_string())
}

fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.split('.').all(|p| {
            let mut c = p.chars();
            matches!(c.next(), Some(f) if f.is_ascii_alphabetic() || f == '_') && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
        })
}

impl Parser {
    fn parse(&mut self, v: &Value, ns: Option<&str>) -> Result<Schema> {
        match v {
            Value::String(s) => self.by_name(s, ns),
            Value::Array(items) => {
                let mut branches = Vec::new();
                for i in items {
                    let b = self.parse(i, ns)?;
                    if matches!(b, Schema::Union(_)) {
                        return Err(avro_err("Unions may not immediately contain other unions"));
                    }
                    branches.push(b);
                }
                Ok(Schema::Union(branches))
            }
            Value::Object(o) => {
                let t = o.get("type").ok_or_else(|| avro_err(format!("No type: {}", v.to_json())))?;
                let base = match t.as_str() {
                    Some("record") | Some("error") => self.named_type(o, ns, "record")?,
                    Some("enum") => self.named_type(o, ns, "enum")?,
                    Some("fixed") => self.named_type(o, ns, "fixed")?,
                    Some("array") => Schema::Array(Box::new(self.parse(o.get("items").ok_or_else(|| avro_err("Array has no items type"))?, ns)?)),
                    Some("map") => Schema::Map(Box::new(self.parse(o.get("values").ok_or_else(|| avro_err("Map has no values type"))?, ns)?)),
                    _ => self.parse(t, ns)?,
                };
                Ok(match o.get("logicalType").and_then(|l| l.as_str()) {
                    Some(l) => self.logical(base, l, o),
                    None => base,
                })
            }
            other => Err(avro_err(format!("Invalid schema: {}", other.to_json()))),
        }
    }

    fn logical(&mut self, base: Schema, l: &str, o: &ObjectMap) -> Schema {
        let is = |s: &Schema| -> &'static str {
            match s {
                Schema::Int => "int",
                Schema::Long => "long",
                Schema::Bytes => "bytes",
                Schema::String => "string",
                Schema::Named(_) => "fixed",
                _ => "",
            }
        };
        let fixed_size = |p: &Parser, s: &Schema| -> Option<usize> {
            match s {
                Schema::Named(i) => match &p.named[*i].kind {
                    NamedKind::Fixed { size } => Some(*size),
                    _ => None,
                },
                _ => None,
            }
        };
        let lt = match (l, is(&base)) {
            ("date", "int") => Logical::Date,
            ("time-millis", "int") => Logical::TimeMillis,
            ("time-micros", "long") => Logical::TimeMicros,
            ("timestamp-millis", "long") => Logical::TimestampMillis,
            ("timestamp-micros", "long") => Logical::TimestampMicros,
            ("local-timestamp-millis", "long") => Logical::LocalTimestampMillis,
            ("local-timestamp-micros", "long") => Logical::LocalTimestampMicros,
            ("uuid", "string") => Logical::Uuid,
            ("uuid", "fixed") if fixed_size(self, &base) == Some(16) => Logical::Uuid,
            ("duration", "fixed") if fixed_size(self, &base) == Some(12) => Logical::Duration,
            ("decimal", "bytes") | ("decimal", "fixed") => {
                let precision = o.get("precision").and_then(|p| p.as_i64()).unwrap_or(0);
                let scale = o.get("scale").and_then(|p| p.as_i64()).unwrap_or(0);
                if precision <= 0 || scale < 0 || scale > precision {
                    return base; // invalid logical types are ignored (spec)
                }
                Logical::Decimal { precision: precision as u32, scale: scale as u32 }
            }
            _ => return base, // unknown logical types are ignored
        };
        if let Schema::Named(i) = &base {
            self.named[*i].logical = Some(lt.clone());
        }
        Schema::Logical(Box::new(base), lt)
    }

    fn by_name(&self, s: &str, ns: Option<&str>) -> Result<Schema> {
        Ok(match s {
            "null" => Schema::Null,
            "boolean" => Schema::Boolean,
            "int" => Schema::Int,
            "long" => Schema::Long,
            "float" => Schema::Float,
            "double" => Schema::Double,
            "bytes" => Schema::Bytes,
            "string" => Schema::String,
            name => {
                let full = full_name(name, ns);
                match self.index.get(&full).or_else(|| self.index.get(name)) {
                    Some(i) => Schema::Named(*i),
                    None => return Err(avro_err(format!("Undefined name: \"{}\"", full))),
                }
            }
        })
    }

    fn named_type(&mut self, o: &ObjectMap, ns: Option<&str>, kind: &str) -> Result<Schema> {
        let name = o.get("name").and_then(|n| n.as_str()).ok_or_else(|| avro_err(format!("No name in schema: {}", Value::Object(o.clone()).to_json())))?;
        let ns_opt = o.get("namespace").and_then(|n| n.as_str()).map(String::from);
        let effective_ns = if name.contains('.') { namespace_of(name) } else { ns_opt.or_else(|| ns.map(String::from)) };
        let full = full_name(name, effective_ns.as_deref());
        if !valid_name(&full) {
            return Err(avro_err(format!("Illegal name: {}", full)));
        }
        if self.index.contains_key(&full) {
            return Err(avro_err(format!("Can't redefine: {}", full)));
        }
        let aliases: Vec<String> = o
            .get("aliases")
            .and_then(|a| a.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str()).map(|x| full_name(x, effective_ns.as_deref())).collect())
            .unwrap_or_default();
        let doc = o.get("doc").and_then(|d| d.as_str()).map(String::from);
        let idx = self.named.len();
        // placeholder first so recursive references resolve
        self.named.push(Named { name: full.clone(), aliases: aliases.clone(), doc, kind: NamedKind::Fixed { size: 0 }, logical: None });
        self.index.insert(full.clone(), idx);
        for a in &aliases {
            self.index.entry(a.clone()).or_insert(idx);
        }
        let k = match kind {
            "record" => {
                let fs = o.get("fields").and_then(|f| f.as_array()).ok_or_else(|| avro_err(format!("Record has no fields: {}", full)))?;
                let mut fields = Vec::new();
                for f in fs {
                    let fname = f.get("name").and_then(|n| n.as_str()).ok_or_else(|| avro_err("No field name"))?.to_string();
                    if fields.iter().any(|x: &Field| x.name == fname) {
                        return Err(avro_err(format!("Duplicate field {} in record {}", fname, full)));
                    }
                    let ft = f.get("type").ok_or_else(|| avro_err(format!("No field type: {}", fname)))?;
                    let schema = self.parse(ft, effective_ns.as_deref())?;
                    fields.push(Field {
                        name: fname,
                        schema,
                        default: f.get("default").cloned(),
                        aliases: f.get("aliases").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).map(String::from).collect()).unwrap_or_default(),
                        doc: f.get("doc").and_then(|d| d.as_str()).map(String::from),
                    });
                }
                NamedKind::Record { fields, error: o.get("type").and_then(|t| t.as_str()) == Some("error") }
            }
            "enum" => {
                let symbols: Vec<String> = o
                    .get("symbols")
                    .and_then(|s| s.as_array())
                    .ok_or_else(|| avro_err(format!("Enum has no symbols: {}", full)))?
                    .iter()
                    .map(|s| s.as_str().map(String::from).ok_or_else(|| avro_err("Enum symbols must be strings")))
                    .collect::<Result<_>>()?;
                NamedKind::Enum { symbols, default: o.get("default").and_then(|d| d.as_str()).map(String::from) }
            }
            _ => {
                let size = o.get("size").and_then(|s| s.as_i64()).filter(|s| *s >= 0).ok_or_else(|| avro_err(format!("Invalid fixed size: {}", full)))?;
                NamedKind::Fixed { size: size as usize }
            }
        };
        self.named[idx].kind = k;
        Ok(Schema::Named(idx))
    }
}

impl AvroSchema {
    /// Parse a schema from its JSON text.
    pub fn parse(text: &str) -> Result<AvroSchema> {
        let t = text.trim();
        let v = if t.starts_with('{') || t.starts_with('[') || t.starts_with('"') {
            cameleer_core::json::parse(t).map_err(|e| avro_err(format!("Invalid schema JSON: {}", e)))?
        } else {
            Value::String(t.to_string())
        };
        Self::from_value(&v)
    }

    /// Parse a schema from a JSON value.
    pub fn from_value(v: &Value) -> Result<AvroSchema> {
        let mut p = Parser { named: Vec::new(), index: HashMap::new() };
        let root = p.parse(v, None)?;
        Ok(AvroSchema { root, named: p.named })
    }

    /// The same schema with a named type as root (`instanceClassName`).
    pub fn select(&self, full_name: &str) -> Result<AvroSchema> {
        let i = self
            .named
            .iter()
            .position(|n| n.name == full_name || n.name.rsplit('.').next() == Some(full_name) || n.aliases.iter().any(|a| a == full_name))
            .ok_or_else(|| avro_err(format!("Type {} not found in schema", full_name)))?;
        Ok(AvroSchema { root: Schema::Named(i), named: self.named.clone() })
    }

    /// Serialize the schema to JSON (named types defined at first use).
    pub fn to_json(&self) -> String {
        let mut seen = vec![false; self.named.len()];
        self.schema_value(&self.root, &mut seen, None).to_json()
    }

    fn schema_value(&self, s: &Schema, seen: &mut Vec<bool>, ns: Option<&str>) -> Value {
        match s {
            Schema::Null => "null".into(),
            Schema::Boolean => "boolean".into(),
            Schema::Int => "int".into(),
            Schema::Long => "long".into(),
            Schema::Float => "float".into(),
            Schema::Double => "double".into(),
            Schema::Bytes => "bytes".into(),
            Schema::String => "string".into(),
            Schema::Array(i) => Value::object([("type", Value::from("array")), ("items", self.schema_value(i, seen, ns))]),
            Schema::Map(i) => Value::object([("type", Value::from("map")), ("values", self.schema_value(i, seen, ns))]),
            Schema::Union(b) => Value::Array(b.iter().map(|x| self.schema_value(x, seen, ns)).collect()),
            Schema::Logical(base, lt) => {
                let mut b = self.schema_value(base, seen, ns);
                let mut o = match b {
                    Value::Object(ref mut o) => std::mem::take(o),
                    other => {
                        let mut o = ObjectMap::new();
                        o.insert("type".into(), other);
                        o
                    }
                };
                if !o.contains_key("logicalType") {
                    add_logical(&mut o, lt);
                }
                Value::Object(o)
            }
            Schema::Named(i) => {
                let n = &self.named[*i];
                if seen[*i] {
                    return Value::String(n.name.clone());
                }
                seen[*i] = true;
                let mut o = ObjectMap::new();
                let (nns, short) = match n.name.rsplit_once('.') {
                    Some((a, b)) => (Some(a.to_string()), b.to_string()),
                    None => (None, n.name.clone()),
                };
                match &n.kind {
                    NamedKind::Record { error, .. } => o.insert("type".into(), (if *error { "error" } else { "record" }).into()),
                    NamedKind::Enum { .. } => o.insert("type".into(), "enum".into()),
                    NamedKind::Fixed { .. } => o.insert("type".into(), "fixed".into()),
                };
                o.insert("name".into(), short.into());
                if nns.as_deref() != ns {
                    o.insert("namespace".into(), nns.clone().unwrap_or_default().into());
                }
                if let Some(d) = &n.doc {
                    o.insert("doc".into(), d.clone().into());
                }
                if !n.aliases.is_empty() {
                    o.insert("aliases".into(), Value::Array(n.aliases.iter().map(|a| a.clone().into()).collect()));
                }
                match &n.kind {
                    NamedKind::Record { fields, .. } => {
                        let fs = fields
                            .iter()
                            .map(|f| {
                                let mut fo = ObjectMap::new();
                                fo.insert("name".into(), f.name.clone().into());
                                fo.insert("type".into(), self.schema_value(&f.schema, seen, nns.as_deref()));
                                if let Some(d) = &f.default {
                                    fo.insert("default".into(), d.clone());
                                }
                                Value::Object(fo)
                            })
                            .collect();
                        o.insert("fields".into(), Value::Array(fs));
                    }
                    NamedKind::Enum { symbols, default } => {
                        o.insert("symbols".into(), Value::Array(symbols.iter().map(|s| s.clone().into()).collect()));
                        if let Some(d) = default {
                            o.insert("default".into(), d.clone().into());
                        }
                    }
                    NamedKind::Fixed { size } => {
                        o.insert("size".into(), Value::Int(*size as i64));
                    }
                }
                if let Some(lt) = &n.logical {
                    add_logical(&mut o, lt);
                }
                Value::Object(o)
            }
        }
    }

}

fn add_logical(o: &mut ObjectMap, lt: &Logical) {
    let name = match lt {
        Logical::Date => "date",
        Logical::TimeMillis => "time-millis",
        Logical::TimeMicros => "time-micros",
        Logical::TimestampMillis => "timestamp-millis",
        Logical::TimestampMicros => "timestamp-micros",
        Logical::LocalTimestampMillis => "local-timestamp-millis",
        Logical::LocalTimestampMicros => "local-timestamp-micros",
        Logical::Decimal { precision, scale } => {
            o.insert("logicalType".into(), "decimal".into());
            o.insert("precision".into(), Value::Int(*precision as i64));
            o.insert("scale".into(), Value::Int(*scale as i64));
            return;
        }
        Logical::Uuid => "uuid",
        Logical::Duration => "duration",
    };
    o.insert("logicalType".into(), name.into());
}

// ---------------------------------------------------------------------------------------
// Logical type conversions
// ---------------------------------------------------------------------------------------

use cameleer_core::util_time::{civil_from_epoch, epoch_from_civil};

fn format_date(days: i64) -> String {
    let (y, m, d, ..) = civil_from_epoch(days * 86_400);
    format!("{:04}-{:02}-{:02}", y, m, d)
}

fn parse_date(s: &str) -> Option<i64> {
    let p: Vec<&str> = s.split('-').collect();
    if p.len() != 3 {
        return None;
    }
    let (y, m, d) = (p[0].parse().ok()?, p[1].parse().ok()?, p[2].parse().ok()?);
    Some(epoch_from_civil(y, m, d, 0, 0, 0) / 86_400)
}

fn format_time(units: i64, per_sec: i64) -> String {
    let secs = units.div_euclid(per_sec);
    let frac = units.rem_euclid(per_sec);
    let (h, mi, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if per_sec == 1000 {
        format!("{:02}:{:02}:{:02}.{:03}", h, mi, s, frac)
    } else {
        format!("{:02}:{:02}:{:02}.{:06}", h, mi, s, frac)
    }
}

/// Parse `HH:MM[:SS[.fff]]` into units (per_sec per second).
fn parse_time(s: &str, per_sec: i64) -> Option<i64> {
    let (hms, frac) = match s.split_once('.') {
        Some((a, b)) => (a, b),
        None => (s, ""),
    };
    let p: Vec<&str> = hms.split(':').collect();
    if p.len() < 2 || p.len() > 3 {
        return None;
    }
    let h: i64 = p[0].parse().ok()?;
    let mi: i64 = p[1].parse().ok()?;
    let sec: i64 = if p.len() == 3 { p[2].parse().ok()? } else { 0 };
    let digits = if per_sec == 1000 { 3 } else { 6 };
    let mut f: String = frac.chars().take_while(|c| c.is_ascii_digit()).take(digits).collect();
    while f.len() < digits {
        f.push('0');
    }
    Some((h * 3600 + mi * 60 + sec) * per_sec + f.parse::<i64>().ok()?)
}

fn format_timestamp(units: i64, per_sec: i64, zulu: bool) -> String {
    let secs = units.div_euclid(per_sec);
    let (date_part, time_part) = (secs.div_euclid(86_400), units - secs.div_euclid(86_400) * 86_400 * per_sec);
    format!("{}T{}{}", format_date(date_part), format_time(time_part, per_sec), if zulu { "Z" } else { "" })
}

fn parse_timestamp(s: &str, per_sec: i64) -> Option<i64> {
    let (d, t) = s.split_once(['T', ' '])?;
    let days = parse_date(d)?;
    // strip zone: Z or +hh:mm / -hh:mm
    let (t, offset_secs) = if let Some(x) = t.strip_suffix('Z') {
        (x, 0)
    } else if let Some(pos) = t.rfind(['+', '-']).filter(|p| *p >= 5) {
        let (a, z) = t.split_at(pos);
        let sign = if z.starts_with('-') { -1 } else { 1 };
        let zz = &z[1..];
        let (zh, zm) = zz.split_once(':').unwrap_or((zz, "0"));
        (a, sign * (zh.parse::<i64>().ok()? * 3600 + zm.parse::<i64>().ok()? * 60))
    } else {
        (t, 0)
    };
    Some(days * 86_400 * per_sec + parse_time(t, per_sec)? - offset_secs * per_sec)
}

/// Format an unscaled two's complement big-endian integer with `scale`.
fn format_decimal(bytes: &[u8], scale: u32) -> Result<String> {
    if bytes.len() > 16 {
        return Err(avro_err("Decimal too large (max 16 bytes supported)"));
    }
    let mut v: i128 = if !bytes.is_empty() && bytes[0] & 0x80 != 0 { -1 } else { 0 };
    for b in bytes {
        v = (v << 8) | *b as i128;
    }
    let neg = v < 0;
    let digits = v.unsigned_abs().to_string();
    let scale = scale as usize;
    let s = if scale == 0 {
        digits
    } else {
        let padded = format!("{:0>w$}", digits, w = scale + 1);
        let (a, b) = padded.split_at(padded.len() - scale);
        format!("{}.{}", a, b)
    };
    Ok(if neg { format!("-{}", s) } else { s })
}

fn parse_decimal(v: &Value, scale: u32, len: Option<usize>) -> Result<Vec<u8>> {
    let text = match v {
        Value::String(s) => s.trim().to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => format!("{:.*}", scale as usize, f),
        Value::Bytes(b) => return Ok(b.clone()),
        other => return Err(avro_err(format!("Cannot encode {} as decimal", other.type_name()))),
    };
    let (neg, t) = match text.strip_prefix('-') {
        Some(r) => (true, r.to_string()),
        None => (false, text.trim_start_matches('+').to_string()),
    };
    let (int_part, frac) = t.split_once('.').unwrap_or((&t, ""));
    if frac.len() > scale as usize {
        return Err(avro_err(format!("Cannot encode decimal with scale {} as scale {} without rounding", frac.len(), scale)));
    }
    let digits = format!("{}{:0<w$}", int_part, frac, w = scale as usize);
    let mut n: i128 = digits.parse().map_err(|_| avro_err(format!("Invalid decimal: {}", text)))?;
    if neg {
        n = -n;
    }
    let full = n.to_be_bytes();
    let out = match len {
        Some(l) => {
            if l < 16 {
                full[16 - l..].to_vec()
            } else {
                let mut o = vec![if n < 0 { 0xff } else { 0 }; l - 16];
                o.extend_from_slice(&full);
                o
            }
        }
        None => {
            // minimal two's complement
            let mut i = 0;
            while i < 15 && ((full[i] == 0 && full[i + 1] & 0x80 == 0) || (full[i] == 0xff && full[i + 1] & 0x80 != 0)) {
                i += 1;
            }
            full[i..].to_vec()
        }
    };
    Ok(out)
}

fn format_uuid(b: &[u8]) -> String {
    let h: String = b.iter().map(|x| format!("{:02x}", x)).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

// ---------------------------------------------------------------------------------------
// Binary encoding
// ---------------------------------------------------------------------------------------

/// Write a zigzag varint long.
pub fn write_long(out: &mut Vec<u8>, v: i64) {
    let mut n = ((v << 1) ^ (v >> 63)) as u64;
    while n >= 0x80 {
        out.push((n as u8 & 0x7f) | 0x80);
        n >>= 7;
    }
    out.push(n as u8);
}

/// Read a zigzag varint long.
pub fn read_long(data: &[u8], pos: &mut usize) -> Result<i64> {
    let mut n: u64 = 0;
    let mut shift = 0;
    loop {
        let b = *data.get(*pos).ok_or_else(|| avro_err("Unexpected end of Avro data"))?;
        *pos += 1;
        if shift >= 64 {
            return Err(avro_err("Invalid Avro varint"));
        }
        n |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    Ok(((n >> 1) as i64) ^ -((n & 1) as i64))
}

fn write_bytes(out: &mut Vec<u8>, b: &[u8]) {
    write_long(out, b.len() as i64);
    out.extend_from_slice(b);
}

fn read_n<'a>(data: &'a [u8], pos: &mut usize, n: usize) -> Result<&'a [u8]> {
    if data.len() - *pos < n {
        return Err(avro_err("Unexpected end of Avro data"));
    }
    let s = &data[*pos..*pos + n];
    *pos += n;
    Ok(s)
}

fn read_len(data: &[u8], pos: &mut usize) -> Result<usize> {
    let n = read_long(data, pos)?;
    if n < 0 || n as usize > data.len() - *pos {
        return Err(avro_err(format!("Malformed data. Length is negative or too large: {}", n)));
    }
    Ok(n as usize)
}

/// Options for value conversion.
#[derive(Clone, Copy, Debug)]
pub struct Conv {
    /// Convert logical types to readable forms when decoding.
    pub logical: bool,
}

impl Default for Conv {
    fn default() -> Self {
        Conv { logical: true }
    }
}

fn type_mismatch(s: &AvroSchema, schema: &Schema, v: &Value) -> CamelError {
    avro_err(format!("Value {} ({}) does not match schema {}", truncate(&v.to_json()), v.type_name(), AvroSchema { root: schema.clone(), named: s.named.clone() }.to_json()))
}

fn truncate(s: &str) -> String {
    if s.len() > 80 {
        format!("{}...", &s[..s.char_indices().take_while(|(i, _)| *i < 80).last().map(|(i, c)| i + c.len_utf8()).unwrap_or(0)])
    } else {
        s.to_string()
    }
}

/// Union branch selection for a generic value.
fn select_branch(s: &AvroSchema, branches: &[Schema], v: &Value) -> Option<(usize, Value)> {
    // explicit JSON-encoding wrapper {"type": value}
    if let Value::Object(o) = v {
        if o.len() == 1 {
            let (k, inner) = o.iter().next().unwrap();
            for (i, b) in branches.iter().enumerate() {
                if branch_name(s, b) == *k || branch_name(s, b).rsplit('.').next() == Some(k.as_str()) {
                    return Some((i, inner.clone()));
                }
            }
        }
    }
    for (i, b) in branches.iter().enumerate() {
        if matches_exact(s, b, v) {
            return Some((i, v.clone()));
        }
    }
    for (i, b) in branches.iter().enumerate() {
        if matches_loose(s, b, v) {
            return Some((i, v.clone()));
        }
    }
    None
}

/// The name of a union branch (JSON encoding).
pub fn branch_name(s: &AvroSchema, b: &Schema) -> String {
    match b {
        Schema::Null => "null".into(),
        Schema::Boolean => "boolean".into(),
        Schema::Int => "int".into(),
        Schema::Long => "long".into(),
        Schema::Float => "float".into(),
        Schema::Double => "double".into(),
        Schema::Bytes => "bytes".into(),
        Schema::String => "string".into(),
        Schema::Array(_) => "array".into(),
        Schema::Map(_) => "map".into(),
        Schema::Union(_) => "union".into(),
        Schema::Named(i) => s.named[*i].name.clone(),
        Schema::Logical(b, _) => branch_name(s, b),
    }
}

fn matches_exact(s: &AvroSchema, b: &Schema, v: &Value) -> bool {
    match (b, v) {
        (Schema::Null, Value::Null) => true,
        (Schema::Boolean, Value::Bool(_)) => true,
        (Schema::Int, Value::Int(i)) => i32::try_from(*i).is_ok(),
        (Schema::Long, Value::Int(_)) => true,
        (Schema::Double, Value::Float(_)) => true,
        (Schema::String, Value::String(_)) => true,
        (Schema::Bytes, Value::Bytes(_)) => true,
        (Schema::Array(_), Value::Array(_)) => true,
        (Schema::Logical(base, lt), v) => match (lt, v) {
            (Logical::Decimal { .. }, Value::String(t)) => t.parse::<f64>().is_ok(),
            (Logical::Date, Value::String(t)) => parse_date(t).is_some(),
            (Logical::TimestampMillis | Logical::TimestampMicros | Logical::LocalTimestampMillis | Logical::LocalTimestampMicros, Value::String(t)) => parse_timestamp(t, 1000).is_some(),
            (Logical::TimeMillis | Logical::TimeMicros, Value::String(t)) => parse_time(t, 1000).is_some(),
            _ => matches_exact(s, base, v),
        },
        (Schema::Named(i), v) => match (&s.named[*i].kind, v) {
            (NamedKind::Record { fields, .. }, Value::Object(o)) => o.keys().all(|k| fields.iter().any(|f| &f.name == k)) && fields.iter().all(|f| o.contains_key(&f.name) || f.default.is_some()),
            (NamedKind::Enum { symbols, .. }, Value::String(x)) => symbols.contains(x),
            (NamedKind::Fixed { size }, Value::Bytes(b)) => b.len() == *size,
            _ => false,
        },
        _ => false,
    }
}

fn matches_loose(s: &AvroSchema, b: &Schema, v: &Value) -> bool {
    match (b, v) {
        (Schema::Float | Schema::Double, Value::Int(_)) => true,
        (Schema::Float, Value::Float(_)) => true,
        (Schema::Map(_), Value::Object(_)) => true,
        (Schema::Bytes, Value::String(_)) => true,
        (Schema::Logical(base, _), v) => matches_loose(s, base, v),
        (Schema::Named(i), Value::Object(_)) => matches!(s.named[*i].kind, NamedKind::Record { .. }),
        _ => false,
    }
}

/// Encode a value with a schema (binary encoding).
pub fn encode_binary(s: &AvroSchema, v: &Value) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    enc(s, &s.root, v, &mut out)?;
    Ok(out)
}

fn as_long(s: &AvroSchema, schema: &Schema, v: &Value) -> Result<i64> {
    match v {
        Value::Int(i) => Ok(*i),
        Value::Float(f) if f.fract() == 0.0 => Ok(*f as i64),
        Value::String(t) => t.trim().parse().map_err(|_| type_mismatch(s, schema, v)),
        _ => Err(type_mismatch(s, schema, v)),
    }
}

fn enc(s: &AvroSchema, schema: &Schema, v: &Value, out: &mut Vec<u8>) -> Result<()> {
    match schema {
        Schema::Null => {
            if !v.is_null() {
                return Err(type_mismatch(s, schema, v));
            }
        }
        Schema::Boolean => match v {
            Value::Bool(b) => out.push(*b as u8),
            Value::String(t) if t == "true" || t == "false" => out.push((t == "true") as u8),
            _ => return Err(type_mismatch(s, schema, v)),
        },
        Schema::Int => {
            let n = as_long(s, schema, v)?;
            if i32::try_from(n).is_err() {
                return Err(type_mismatch(s, schema, v));
            }
            write_long(out, n)
        }
        Schema::Long => write_long(out, as_long(s, schema, v)?),
        Schema::Float | Schema::Double => {
            let f = match v {
                Value::Float(f) => *f,
                Value::Int(i) => *i as f64,
                Value::String(t) => match t.as_str() {
                    "NaN" => f64::NAN,
                    "Infinity" => f64::INFINITY,
                    "-Infinity" => f64::NEG_INFINITY,
                    _ => t.parse().map_err(|_| type_mismatch(s, schema, v))?,
                },
                _ => return Err(type_mismatch(s, schema, v)),
            };
            if *schema == Schema::Float {
                out.extend_from_slice(&(f as f32).to_le_bytes());
            } else {
                out.extend_from_slice(&f.to_le_bytes());
            }
        }
        Schema::Bytes => match v {
            Value::Bytes(b) => write_bytes(out, b),
            Value::String(t) => write_bytes(out, t.as_bytes()),
            _ => return Err(type_mismatch(s, schema, v)),
        },
        Schema::String => match v {
            Value::String(t) => write_bytes(out, t.as_bytes()),
            Value::Bytes(b) => write_bytes(out, b),
            Value::Int(_) | Value::Float(_) | Value::Bool(_) => write_bytes(out, v.to_string().as_bytes()),
            _ => return Err(type_mismatch(s, schema, v)),
        },
        Schema::Array(items) => {
            let a = v.as_array().ok_or_else(|| type_mismatch(s, schema, v))?;
            if !a.is_empty() {
                write_long(out, a.len() as i64);
                for x in a {
                    enc(s, items, x, out)?;
                }
            }
            write_long(out, 0);
        }
        Schema::Map(values) => {
            let m = v.as_object().ok_or_else(|| type_mismatch(s, schema, v))?;
            if !m.is_empty() {
                write_long(out, m.len() as i64);
                for (k, x) in m.iter() {
                    write_bytes(out, k.as_bytes());
                    enc(s, values, x, out)?;
                }
            }
            write_long(out, 0);
        }
        Schema::Union(branches) => {
            let (i, inner) = select_branch(s, branches, v).ok_or_else(|| avro_err(format!("Not in union {}: {}", AvroSchema { root: schema.clone(), named: s.named.clone() }.to_json(), truncate(&v.to_json()))))?;
            write_long(out, i as i64);
            enc(s, &branches[i], &inner, out)?;
        }
        Schema::Logical(base, lt) => enc_logical(s, base, lt, v, out)?,
        Schema::Named(i) => match &s.named[*i].kind {
            NamedKind::Record { fields, .. } => {
                let o = v.as_object().ok_or_else(|| type_mismatch(s, schema, v))?;
                for f in fields {
                    let fv = o.get(&f.name).or_else(|| f.aliases.iter().find_map(|a| o.get(a)));
                    match fv {
                        Some(x) => enc(s, &f.schema, x, out).map_err(|e| avro_err(format!("{} in field {}", e.message, f.name)))?,
                        None => match &f.default {
                            Some(d) => {
                                let d = default_value(s, &f.schema, d);
                                enc(s, &f.schema, &d, out)?
                            }
                            None if matches!(&f.schema, Schema::Union(b) if b.contains(&Schema::Null)) => enc(s, &f.schema, &Value::Null, out)?,
                            None => return Err(avro_err(format!("Field {} type:{} pos:{} not set and has no default value", f.name, branch_name(s, &f.schema).to_uppercase(), fields.iter().position(|x| x.name == f.name).unwrap_or(0)))),
                        },
                    }
                }
            }
            NamedKind::Enum { symbols, default } => {
                let sym = v.as_str().ok_or_else(|| type_mismatch(s, schema, v))?;
                let idx = symbols.iter().position(|x| x == sym).or_else(|| default.as_ref().and_then(|d| symbols.iter().position(|x| x == d)));
                write_long(out, idx.ok_or_else(|| avro_err(format!("Not an enum symbol: {}", sym)))? as i64);
            }
            NamedKind::Fixed { size } => {
                let b = match v {
                    Value::Bytes(b) => b.clone(),
                    Value::String(t) => t.chars().map(|c| c as u32 as u8).collect(),
                    _ => return Err(type_mismatch(s, schema, v)),
                };
                if b.len() != *size {
                    return Err(avro_err(format!("Fixed size mismatch: expected {} bytes, got {}", size, b.len())));
                }
                out.extend_from_slice(&b);
            }
        },
    }
    Ok(())
}

/// A field default (JSON) as a generic value: union defaults use the first branch, bytes
/// and fixed defaults are strings with code points 0-255.
fn default_value(s: &AvroSchema, schema: &Schema, d: &Value) -> Value {
    match schema {
        Schema::Union(b) if !b.is_empty() => default_value(s, &b[0], d),
        Schema::Bytes => match d {
            Value::String(t) => Value::Bytes(t.chars().map(|c| c as u32 as u8).collect()),
            o => o.clone(),
        },
        Schema::Named(i) => match (&s.named[*i].kind, d) {
            (NamedKind::Fixed { .. }, Value::String(t)) => Value::Bytes(t.chars().map(|c| c as u32 as u8).collect()),
            _ => d.clone(),
        },
        _ => d.clone(),
    }
}

fn enc_logical(s: &AvroSchema, base: &Schema, lt: &Logical, v: &Value, out: &mut Vec<u8>) -> Result<()> {
    let conv = match (lt, v) {
        (Logical::Date, Value::String(t)) => Value::Int(parse_date(t).ok_or_else(|| avro_err(format!("Invalid date: {}", t)))?),
        (Logical::TimeMillis, Value::String(t)) => Value::Int(parse_time(t, 1000).ok_or_else(|| avro_err(format!("Invalid time: {}", t)))?),
        (Logical::TimeMicros, Value::String(t)) => Value::Int(parse_time(t, 1_000_000).ok_or_else(|| avro_err(format!("Invalid time: {}", t)))?),
        (Logical::TimestampMillis | Logical::LocalTimestampMillis, Value::String(t)) => Value::Int(parse_timestamp(t, 1000).ok_or_else(|| avro_err(format!("Invalid timestamp: {}", t)))?),
        (Logical::TimestampMicros | Logical::LocalTimestampMicros, Value::String(t)) => Value::Int(parse_timestamp(t, 1_000_000).ok_or_else(|| avro_err(format!("Invalid timestamp: {}", t)))?),
        (Logical::Decimal { scale, .. }, v) if !matches!(v, Value::Bytes(_)) => {
            let size = match base {
                Schema::Named(i) => match s.named[*i].kind {
                    NamedKind::Fixed { size } => Some(size),
                    _ => None,
                },
                _ => None,
            };
            Value::Bytes(parse_decimal(v, *scale, size)?)
        }
        (Logical::Uuid, Value::String(t)) if matches!(base, Schema::Named(_)) => {
            let h: String = t.chars().filter(|c| *c != '-').collect();
            Value::Bytes(cameleer_tls::util::try_unhex(&h).filter(|b| b.len() == 16).ok_or_else(|| avro_err(format!("Invalid UUID: {}", t)))?)
        }
        (Logical::Duration, Value::Object(o)) => {
            let g = |k: &str| o.get(k).and_then(|x| x.as_i64()).unwrap_or(0) as u32;
            let mut b = Vec::new();
            b.extend_from_slice(&g("months").to_le_bytes());
            b.extend_from_slice(&g("days").to_le_bytes());
            b.extend_from_slice(&g("millis").to_le_bytes());
            Value::Bytes(b)
        }
        _ => v.clone(),
    };
    enc(s, base, &conv, out)
}

/// Decode a binary datum.
pub fn decode_binary(s: &AvroSchema, data: &[u8], conv: Conv) -> Result<Value> {
    let mut pos = 0;
    let v = dec(s, &s.root, data, &mut pos, conv)?;
    Ok(v)
}

fn dec(s: &AvroSchema, schema: &Schema, data: &[u8], pos: &mut usize, conv: Conv) -> Result<Value> {
    Ok(match schema {
        Schema::Null => Value::Null,
        Schema::Boolean => {
            let b = read_n(data, pos, 1)?[0];
            match b {
                0 => Value::Bool(false),
                1 => Value::Bool(true),
                _ => return Err(avro_err(format!("Invalid boolean byte: {}", b))),
            }
        }
        Schema::Int | Schema::Long => Value::Int(read_long(data, pos)?),
        Schema::Float => Value::Float(f32::from_le_bytes(read_n(data, pos, 4)?.try_into().unwrap()) as f64),
        Schema::Double => Value::Float(f64::from_le_bytes(read_n(data, pos, 8)?.try_into().unwrap())),
        Schema::Bytes => {
            let n = read_len(data, pos)?;
            Value::Bytes(read_n(data, pos, n)?.to_vec())
        }
        Schema::String => {
            let n = read_len(data, pos)?;
            Value::String(String::from_utf8(read_n(data, pos, n)?.to_vec()).map_err(|_| avro_err("Invalid UTF-8 string"))?)
        }
        Schema::Array(items) => {
            let mut a = Vec::new();
            loop {
                let mut n = read_long(data, pos)?;
                if n == 0 {
                    break;
                }
                if n < 0 {
                    n = -n;
                    read_long(data, pos)?; // block size in bytes
                }
                for _ in 0..n {
                    a.push(dec(s, items, data, pos, conv)?);
                }
            }
            Value::Array(a)
        }
        Schema::Map(values) => {
            let mut m = ObjectMap::new();
            loop {
                let mut n = read_long(data, pos)?;
                if n == 0 {
                    break;
                }
                if n < 0 {
                    n = -n;
                    read_long(data, pos)?;
                }
                for _ in 0..n {
                    let kl = read_len(data, pos)?;
                    let k = String::from_utf8(read_n(data, pos, kl)?.to_vec()).map_err(|_| avro_err("Invalid UTF-8 map key"))?;
                    let v = dec(s, values, data, pos, conv)?;
                    m.insert(k, v);
                }
            }
            Value::Object(m)
        }
        Schema::Union(branches) => {
            let i = read_long(data, pos)?;
            let b = usize::try_from(i).ok().and_then(|i| branches.get(i)).ok_or_else(|| avro_err(format!("Union index out of range: {}", i)))?;
            dec(s, b, data, pos, conv)?
        }
        Schema::Logical(base, lt) => {
            let raw = dec(s, base, data, pos, conv)?;
            if conv.logical {
                logical_to_value(lt, raw)?
            } else {
                raw
            }
        }
        Schema::Named(i) => match &s.named[*i].kind {
            NamedKind::Record { fields, .. } => {
                let mut m = ObjectMap::new();
                for f in fields {
                    let v = dec(s, &f.schema, data, pos, conv)?;
                    m.insert(f.name.clone(), v);
                }
                Value::Object(m)
            }
            NamedKind::Enum { symbols, .. } => {
                let i = read_long(data, pos)?;
                Value::String(usize::try_from(i).ok().and_then(|i| symbols.get(i)).ok_or_else(|| avro_err(format!("Enum index out of range: {}", i)))?.clone())
            }
            NamedKind::Fixed { size } => Value::Bytes(read_n(data, pos, *size)?.to_vec()),
        },
    })
}

fn logical_to_value(lt: &Logical, raw: Value) -> Result<Value> {
    Ok(match (lt, &raw) {
        (Logical::Date, Value::Int(d)) => Value::String(format_date(*d)),
        (Logical::TimeMillis, Value::Int(t)) => Value::String(format_time(*t, 1000)),
        (Logical::TimeMicros, Value::Int(t)) => Value::String(format_time(*t, 1_000_000)),
        (Logical::TimestampMillis, Value::Int(t)) => Value::String(format_timestamp(*t, 1000, true)),
        (Logical::TimestampMicros, Value::Int(t)) => Value::String(format_timestamp(*t, 1_000_000, true)),
        (Logical::LocalTimestampMillis, Value::Int(t)) => Value::String(format_timestamp(*t, 1000, false)),
        (Logical::LocalTimestampMicros, Value::Int(t)) => Value::String(format_timestamp(*t, 1_000_000, false)),
        (Logical::Decimal { scale, .. }, Value::Bytes(b)) => Value::String(format_decimal(b, *scale)?),
        (Logical::Uuid, Value::Bytes(b)) if b.len() == 16 => Value::String(format_uuid(b)),
        (Logical::Duration, Value::Bytes(b)) if b.len() == 12 => {
            let g = |i: usize| Value::Int(u32::from_le_bytes(b[i..i + 4].try_into().unwrap()) as i64);
            Value::object([("months", g(0)), ("days", g(4)), ("millis", g(8))])
        }
        _ => raw,
    })
}

// ---------------------------------------------------------------------------------------
// JSON encoding
// ---------------------------------------------------------------------------------------

fn bytes_to_json_string(b: &[u8]) -> Value {
    Value::String(b.iter().map(|x| *x as char).collect())
}

fn json_string_to_bytes(t: &str) -> Result<Vec<u8>> {
    t.chars().map(|c| u8::try_from(c as u32).map_err(|_| avro_err("Bytes JSON string has a code point > 255"))).collect()
}

/// Convert a generic value to the Avro JSON encoding value.
pub fn to_json_value(s: &AvroSchema, schema: &Schema, v: &Value) -> Result<Value> {
    Ok(match schema {
        Schema::Bytes => match v {
            Value::Bytes(b) => bytes_to_json_string(b),
            other => other.clone(),
        },
        Schema::Float | Schema::Double => match v {
            Value::Float(f) if f.is_nan() => "NaN".into(),
            Value::Float(f) if f.is_infinite() => (if *f > 0.0 { "Infinity" } else { "-Infinity" }).into(),
            Value::Int(i) => Value::Float(*i as f64),
            other => other.clone(),
        },
        Schema::Array(items) => Value::Array(v.as_array().ok_or_else(|| type_mismatch(s, schema, v))?.iter().map(|x| to_json_value(s, items, x)).collect::<Result<_>>()?),
        Schema::Map(values) => {
            let mut m = ObjectMap::new();
            for (k, x) in v.as_object().ok_or_else(|| type_mismatch(s, schema, v))?.iter() {
                m.insert(k.clone(), to_json_value(s, values, x)?);
            }
            Value::Object(m)
        }
        Schema::Union(branches) => {
            let (i, inner) = select_branch(s, branches, v).ok_or_else(|| avro_err(format!("Not in union: {}", truncate(&v.to_json()))))?;
            if branches[i] == Schema::Null {
                Value::Null
            } else {
                let mut m = ObjectMap::new();
                m.insert(branch_name(s, &branches[i]), to_json_value(s, &branches[i], &inner)?);
                Value::Object(m)
            }
        }
        Schema::Logical(base, _) => {
            // logical values are written in their raw encoding
            let mut raw = Vec::new();
            enc(s, schema, v, &mut raw)?;
            let mut p = 0;
            let rv = dec(s, base, &raw, &mut p, Conv { logical: false })?;
            to_json_value(s, base, &rv)?
        }
        Schema::Named(i) => match &s.named[*i].kind {
            NamedKind::Record { fields, .. } => {
                let o = v.as_object().ok_or_else(|| type_mismatch(s, schema, v))?;
                let mut m = ObjectMap::new();
                for f in fields {
                    let fv = match o.get(&f.name) {
                        Some(x) => x.clone(),
                        None => f.default.as_ref().map(|d| default_value(s, &f.schema, d)).unwrap_or(Value::Null),
                    };
                    m.insert(f.name.clone(), to_json_value(s, &f.schema, &fv)?);
                }
                Value::Object(m)
            }
            NamedKind::Fixed { .. } => match v {
                Value::Bytes(b) => bytes_to_json_string(b),
                other => other.clone(),
            },
            NamedKind::Enum { .. } => v.clone(),
        },
        _ => v.clone(),
    })
}

/// Convert an Avro JSON encoding value to a generic value.
pub fn from_json_value(s: &AvroSchema, schema: &Schema, v: &Value, conv: Conv) -> Result<Value> {
    let out = match schema {
        Schema::Bytes => match v {
            Value::String(t) => Value::Bytes(json_string_to_bytes(t)?),
            other => other.clone(),
        },
        Schema::Float | Schema::Double => match v {
            Value::String(t) if t == "NaN" => Value::Float(f64::NAN),
            Value::String(t) if t == "Infinity" => Value::Float(f64::INFINITY),
            Value::String(t) if t == "-Infinity" => Value::Float(f64::NEG_INFINITY),
            Value::Int(i) => Value::Float(*i as f64),
            other => other.clone(),
        },
        Schema::Array(items) => Value::Array(v.as_array().ok_or_else(|| type_mismatch(s, schema, v))?.iter().map(|x| from_json_value(s, items, x, conv)).collect::<Result<_>>()?),
        Schema::Map(values) => {
            let mut m = ObjectMap::new();
            for (k, x) in v.as_object().ok_or_else(|| type_mismatch(s, schema, v))?.iter() {
                m.insert(k.clone(), from_json_value(s, values, x, conv)?);
            }
            Value::Object(m)
        }
        Schema::Union(branches) => {
            if v.is_null() {
                if !branches.contains(&Schema::Null) {
                    return Err(type_mismatch(s, schema, v));
                }
                Value::Null
            } else {
                let o = v.as_object().filter(|o| o.len() == 1).ok_or_else(|| avro_err(format!("Expected a union wrapper object: {}", truncate(&v.to_json()))))?;
                let (k, inner) = o.iter().next().unwrap();
                let b = branches.iter().find(|b| branch_name(s, b) == *k).ok_or_else(|| avro_err(format!("Unknown union branch: {}", k)))?;
                from_json_value(s, b, inner, conv)?
            }
        }
        Schema::Logical(base, lt) => {
            let raw = from_json_value(s, base, v, conv)?;
            if conv.logical {
                logical_to_value(lt, raw)?
            } else {
                raw
            }
        }
        Schema::Named(i) => match &s.named[*i].kind {
            NamedKind::Record { fields, .. } => {
                let o = v.as_object().ok_or_else(|| type_mismatch(s, schema, v))?;
                let mut m = ObjectMap::new();
                for f in fields {
                    let fv = match o.get(&f.name) {
                        Some(x) => from_json_value(s, &f.schema, x, conv)?,
                        None => match &f.default {
                            Some(d) => default_value(s, &f.schema, d),
                            None => return Err(avro_err(format!("Expected field name not found: {}", f.name))),
                        },
                    };
                    m.insert(f.name.clone(), fv);
                }
                Value::Object(m)
            }
            NamedKind::Fixed { .. } => match v {
                Value::String(t) => Value::Bytes(json_string_to_bytes(t)?),
                other => other.clone(),
            },
            NamedKind::Enum { .. } => v.clone(),
        },
        _ => v.clone(),
    };
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// Object container files
// ---------------------------------------------------------------------------------------

/// Object container file codec.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Null,
    Deflate,
}

/// Write an object container file with the given records.
pub fn write_container(s: &AvroSchema, records: &[Value], codec: Codec, sync_interval: usize) -> Result<Vec<u8>> {
    let mut out = b"Obj\x01".to_vec();
    let mut meta = ObjectMap::new();
    meta.insert("avro.schema".into(), Value::Bytes(s.to_json().into_bytes()));
    meta.insert("avro.codec".into(), Value::Bytes(if codec == Codec::Deflate { b"deflate".to_vec() } else { b"null".to_vec() }));
    write_long(&mut out, meta.len() as i64);
    for (k, v) in meta.iter() {
        write_bytes(&mut out, k.as_bytes());
        write_bytes(&mut out, &v.to_bytes());
    }
    write_long(&mut out, 0);
    let sync = cameleer_tls::crypto::rng::random_bytes(16);
    out.extend_from_slice(&sync);
    let mut block = Vec::new();
    let mut count = 0i64;
    let flush = |out: &mut Vec<u8>, block: &mut Vec<u8>, count: &mut i64| {
        if *count == 0 {
            return;
        }
        let data = if codec == Codec::Deflate { deflate(block, 6) } else { std::mem::take(block) };
        write_long(out, *count);
        write_long(out, data.len() as i64);
        out.extend_from_slice(&data);
        out.extend_from_slice(&sync);
        block.clear();
        *count = 0;
    };
    for r in records {
        enc(s, &s.root, r, &mut block)?;
        count += 1;
        if block.len() >= sync_interval.max(1) {
            flush(&mut out, &mut block, &mut count);
        }
    }
    flush(&mut out, &mut block, &mut count);
    Ok(out)
}

/// A read container file: writer schema, metadata and records.
pub struct Container {
    pub schema: AvroSchema,
    pub metadata: ObjectMap,
    pub records: Vec<Value>,
}

/// Whether bytes start with the container file magic.
pub fn is_container(data: &[u8]) -> bool {
    data.starts_with(b"Obj\x01")
}

/// Read an object container file.
pub fn read_container(data: &[u8], conv: Conv) -> Result<Container> {
    if !is_container(data) {
        return Err(avro_err("Not an Avro data file"));
    }
    let mut pos = 4;
    let mut meta = ObjectMap::new();
    loop {
        let mut n = read_long(data, &mut pos)?;
        if n == 0 {
            break;
        }
        if n < 0 {
            n = -n;
            read_long(data, &mut pos)?;
        }
        for _ in 0..n {
            let kl = read_len(data, &mut pos)?;
            let k = String::from_utf8_lossy(read_n(data, &mut pos, kl)?).to_string();
            let vl = read_len(data, &mut pos)?;
            meta.insert(k, Value::Bytes(read_n(data, &mut pos, vl)?.to_vec()));
        }
    }
    let sync = read_n(data, &mut pos, 16)?.to_vec();
    let schema_text = meta.get("avro.schema").map(|v| String::from_utf8_lossy(&v.to_bytes()).to_string()).ok_or_else(|| avro_err("Container file has no avro.schema"))?;
    let schema = AvroSchema::parse(&schema_text)?;
    let codec = meta.get("avro.codec").map(|v| String::from_utf8_lossy(&v.to_bytes()).to_string()).unwrap_or_else(|| "null".into());
    let mut records = Vec::new();
    while pos < data.len() {
        let count = read_long(data, &mut pos)?;
        let size = read_len(data, &mut pos)?;
        let block = read_n(data, &mut pos, size)?;
        let block = match codec.as_str() {
            "null" => block.to_vec(),
            "deflate" => inflate(block)?,
            other => return Err(avro_err(format!("Unsupported codec: {}", other))),
        };
        let mut bp = 0;
        for _ in 0..count {
            records.push(dec(&schema, &schema.root, &block, &mut bp, conv)?);
        }
        if read_n(data, &mut pos, 16)? != &sync[..] {
            return Err(avro_err("Invalid sync marker"));
        }
    }
    let mut m = ObjectMap::new();
    for (k, v) in meta.iter() {
        if !k.starts_with("avro.") {
            m.insert(k.clone(), Value::String(String::from_utf8_lossy(&v.to_bytes()).to_string()));
        }
    }
    Ok(Container { schema, metadata: m, records })
}

// ---------------------------------------------------------------------------------------
// Data format
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Binary,
    Json,
    Container,
}

/// The `avro` data format.
pub struct AvroDataFormat {
    pub schema: Option<AvroSchema>,
    /// Registry bean / type name resolved at runtime.
    pub instance_class_name: Option<String>,
    pub encoding: Encoding,
    pub codec: Codec,
    pub sync_interval: usize,
    pub content_type_header: bool,
    pub conv: Conv,
    /// Jackson flavour: JSON text input on marshal.
    pub jackson: bool,
    /// Return JSON text from unmarshal.
    pub json_view: bool,
}

impl AvroDataFormat {
    /// An `avro` data format for a schema (binary encoding).
    pub fn new(schema: AvroSchema) -> AvroDataFormat {
        AvroDataFormat {
            schema: Some(schema),
            instance_class_name: None,
            encoding: Encoding::Binary,
            codec: Codec::Null,
            sync_interval: 64_000,
            content_type_header: true,
            conv: Conv::default(),
            jackson: false,
            json_view: false,
        }
    }

    fn schema(&self, ex: &Exchange) -> Result<AvroSchema> {
        if let Some(s) = &self.schema {
            return Ok(s.clone());
        }
        if let Some(name) = &self.instance_class_name {
            if let Ok(ctx) = ex.context() {
                let r = ctx.registry();
                if let Some(s) = r.lookup::<AvroSchema>(name) {
                    return Ok((*s).clone());
                }
                if let Some(s) = r.lookup::<String>(name) {
                    return AvroSchema::parse(&s);
                }
            }
            return Err(avro_err(format!("No schema found for instanceClassName {} (set schema or schemaFile, or bind the schema in the registry)", name)));
        }
        Err(avro_err("The avro data format requires a schema (schema, schemaFile or instanceClassName)"))
    }

    fn input_value(&self, v: &Value) -> Result<Value> {
        match v {
            Value::String(_) | Value::Bytes(_) if self.jackson => {
                let t = body_text(v)?;
                cameleer_core::json::parse(&t)
            }
            other => Ok(other.clone()),
        }
    }

    fn output(&self, v: Value) -> Value {
        if self.json_view {
            Value::String(v.to_json())
        } else {
            v
        }
    }
}

impl DataFormat for AvroDataFormat {
    fn name(&self) -> &str {
        "avro"
    }
    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        let s = self.schema(ex)?;
        let v = self.input_value(&ex.message.body)?;
        let (body, ct) = match self.encoding {
            Encoding::Binary => (Value::Bytes(encode_binary(&s, &v)?), "avro/binary"),
            Encoding::Json => (Value::String(to_json_value(&s, &s.root, &v)?.to_json()), "application/json"),
            Encoding::Container => {
                let records = match (&s.root, &v) {
                    (Schema::Array(_), _) => vec![v],
                    (_, Value::Array(a)) => a.clone(),
                    _ => vec![v],
                };
                (Value::Bytes(write_container(&s, &records, self.codec, self.sync_interval)?), "avro/binary")
            }
        };
        ex.message.body = body;
        if self.content_type_header {
            ex.message.headers.set(CONTENT_TYPE, ct);
        }
        Ok(())
    }
    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        let data = body_bytes(&ex.message.body);
        let v = if self.encoding == Encoding::Container || (self.schema.is_none() && self.instance_class_name.is_none() && is_container(&data)) {
            let c = read_container(&data, self.conv)?;
            Value::Array(c.records)
        } else {
            let s = self.schema(ex)?;
            match self.encoding {
                Encoding::Json => {
                    let j = cameleer_core::json::parse(&body_text(&ex.message.body)?)?;
                    from_json_value(&s, &s.root, &j, self.conv)?
                }
                _ => decode_binary(&s, &data, self.conv)?,
            }
        };
        ex.message.body = self.output(v);
        Ok(())
    }
}

pub(crate) fn create_jackson(o: &Opts) -> Result<Arc<dyn DataFormat>> {
    let mut o = o.clone();
    o.0.insert("library".into(), "Jackson".into());
    create(&o)
}

pub(crate) fn create(o: &Opts) -> Result<Arc<dyn DataFormat>> {
    let jackson = o.get(&["library"]).map(|l| l.eq_ignore_ascii_case("jackson")).unwrap_or(false);
    let mut text = o.get(&["schema"]).map(String::from);
    if text.is_none() {
        if let Some(f) = o.get(&["schemaFile", "schemaLocation", "schemaResolver"]) {
            text = Some(String::from_utf8(crate::read_resource(f)?).map_err(|_| df_err("Schema file is not UTF-8"))?);
        }
    }
    let icn = o.get(&["instanceClassName", "instanceClass", "unmarshalTypeName", "unmarshalType"]).map(String::from);
    let schema = match &text {
        Some(t) => {
            let s = AvroSchema::parse(t)?;
            Some(match &icn {
                Some(n) if matches!(s.root, Schema::Union(_)) || s.named.iter().any(|x| &x.name == n) => s.select(n)?,
                _ => s,
            })
        }
        None => None,
    };
    let encoding = match o.get(&["encoding"]).map(|e| e.to_ascii_lowercase()) {
        None => Encoding::Binary,
        Some(e) if e == "binary" => Encoding::Binary,
        Some(e) if e == "json" => Encoding::Json,
        Some(e) if e == "container" || e == "file" || e == "datafile" => Encoding::Container,
        Some(e) => return Err(CamelError::illegal_argument(format!("Unknown avro encoding: {}", e))),
    };
    let codec = match o.get(&["codec"]).map(|e| e.to_ascii_lowercase()) {
        None => Codec::Null,
        Some(c) if c == "null" => Codec::Null,
        Some(c) if c == "deflate" => Codec::Deflate,
        Some(c) => return Err(CamelError::illegal_argument(format!("Unsupported avro codec: {}", c))),
    };
    Ok(Arc::new(AvroDataFormat {
        instance_class_name: if schema.is_none() { icn } else { None },
        schema,
        encoding,
        codec,
        sync_interval: o.num(&["syncInterval"], 64_000)?.max(1) as usize,
        content_type_header: o.bool(&["contentTypeHeader"], true)?,
        conv: Conv { logical: o.bool(&["logicalTypeConversions"], true)? },
        jackson,
        json_view: o.bool(&["jsonView", "unmarshalToJson"], false)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cameleer_core::json;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{:02x}", x)).collect()
    }

    #[test]
    fn zigzag_spec_examples() {
        // spec: 0 -> 00, -1 -> 01, 1 -> 02, -2 -> 03, 2 -> 04, -64 -> 7f, 64 -> 80 01
        for (v, h) in [(0, "00"), (-1, "01"), (1, "02"), (-2, "03"), (2, "04"), (-64, "7f"), (64, "8001"), (i64::MIN, "ffffffffffffffffff01"), (i64::MAX, "feffffffffffffffff01")] {
            let mut o = Vec::new();
            write_long(&mut o, v);
            assert_eq!(hex(&o), h, "{}", v);
            let mut p = 0;
            assert_eq!(read_long(&o, &mut p).unwrap(), v);
        }
    }

    #[test]
    fn spec_examples() {
        // string "foo" -> 06 66 6f 6f
        let s = AvroSchema::parse("\"string\"").unwrap();
        assert_eq!(hex(&encode_binary(&s, &"foo".into()).unwrap()), "06666f6f");
        // array of longs [3, 27] -> 04 06 36 00
        let s = AvroSchema::parse(r#"{"type":"array","items":"long"}"#).unwrap();
        assert_eq!(hex(&encode_binary(&s, &json::parse("[3,27]").unwrap()).unwrap()), "04063600");
        // union ["null","string"]: null -> 00, "a" -> 02 02 61
        let s = AvroSchema::parse(r#"["null","string"]"#).unwrap();
        assert_eq!(hex(&encode_binary(&s, &Value::Null).unwrap()), "00");
        assert_eq!(hex(&encode_binary(&s, &"a".into()).unwrap()), "020261");
        // record test {a: long 27, b: string "foo"} -> 36 06 66 6f 6f
        let s = AvroSchema::parse(r#"{"type":"record","name":"test","fields":[{"name":"a","type":"long"},{"name":"b","type":"string"}]}"#).unwrap();
        let v = json::parse(r#"{"a":27,"b":"foo"}"#).unwrap();
        let b = encode_binary(&s, &v).unwrap();
        assert_eq!(hex(&b), "3606666f6f");
        assert_eq!(decode_binary(&s, &b, Conv::default()).unwrap(), v);
    }

    #[test]
    fn complex_schema_round_trip() {
        let schema = r#"{"type":"record","name":"User","namespace":"example.avro","fields":[
            {"name":"name","type":"string"},
            {"name":"favorite_number","type":["int","null"]},
            {"name":"color","type":{"type":"enum","name":"Color","symbols":["RED","GREEN"]}},
            {"name":"tags","type":{"type":"map","values":"double"}},
            {"name":"hash","type":{"type":"fixed","name":"MD5","size":4}},
            {"name":"born","type":{"type":"int","logicalType":"date"}},
            {"name":"ts","type":{"type":"long","logicalType":"timestamp-millis"}},
            {"name":"price","type":{"type":"bytes","logicalType":"decimal","precision":9,"scale":2}},
            {"name":"id","type":{"type":"string","logicalType":"uuid"}},
            {"name":"next","type":["null","User"],"default":null},
            {"name":"other","type":"Color","default":"GREEN"}
        ]}"#;
        let s = AvroSchema::parse(schema).unwrap();
        let v = json::parse(r#"{"name":"Ann","favorite_number":7,"color":"GREEN","tags":{"x":1.5},"hash":null,"born":"2024-02-29","ts":"2024-01-31T10:20:30.123Z","price":"-12.34","id":"123e4567-e89b-12d3-a456-426614174000","next":{"name":"Bob","favorite_number":null,"color":"RED","tags":{},"hash":null,"born":"1970-01-01","ts":"1970-01-01T00:00:00.000Z","price":"0.05","id":"x","next":null,"other":"RED"},"other":"GREEN"}"#).unwrap();
        let mut v = v;
        v.as_object_mut().unwrap().insert("hash".into(), Value::Bytes(vec![1, 2, 3, 4]));
        v.as_object_mut().unwrap().get_mut("next").unwrap().as_object_mut().unwrap().insert("hash".into(), Value::Bytes(vec![0; 4]));
        let b = encode_binary(&s, &v).unwrap();
        let back = decode_binary(&s, &b, Conv::default()).unwrap();
        assert_eq!(back, v);
        // raw values when conversions are off
        let raw = decode_binary(&s, &b, Conv { logical: false }).unwrap();
        assert_eq!(raw.get("born"), Some(&Value::Int(19782)));
        assert_eq!(raw.get("ts"), Some(&Value::Int(1706696430123)));
        assert_eq!(raw.get("price"), Some(&Value::Bytes(vec![0xfb, 0x2e])));
        // JSON encoding
        let j = to_json_value(&s, &s.root, &v).unwrap();
        assert_eq!(j.get("favorite_number").unwrap().to_json(), r#"{"int":7}"#);
        assert_eq!(j.get("born"), Some(&Value::Int(19782)));
        assert_eq!(j.get("next").unwrap().get("example.avro.User").unwrap().get("next"), Some(&Value::Null));
        assert_eq!(from_json_value(&s, &s.root, &j, Conv::default()).unwrap(), v);
        // schema serialization re-parses to the same schema
        assert_eq!(AvroSchema::parse(&s.to_json()).unwrap(), s);
        // defaults
        let mut partial = v.clone();
        partial.as_object_mut().unwrap().remove("other");
        partial.as_object_mut().unwrap().remove("next");
        let back = decode_binary(&s, &encode_binary(&s, &partial).unwrap(), Conv::default()).unwrap();
        assert_eq!(back.get("other"), Some(&Value::from("GREEN")));
    }

    #[test]
    fn container_round_trip() {
        let s = AvroSchema::parse(r#"{"type":"record","name":"R","fields":[{"name":"i","type":"int"},{"name":"s","type":"string"}]}"#).unwrap();
        let records: Vec<Value> = (0..500).map(|i| Value::object([("i", Value::Int(i)), ("s", Value::from(format!("row {}", i)))])).collect();
        for codec in [Codec::Null, Codec::Deflate] {
            let f = write_container(&s, &records, codec, 1000).unwrap();
            let c = read_container(&f, Conv::default()).unwrap();
            assert_eq!(c.records, records);
            assert_eq!(c.schema, s);
        }
    }

    #[test]
    fn errors() {
        assert!(AvroSchema::parse(r#"{"type":"record","name":"R","fields":[{"name":"x","type":"Nope"}]}"#).is_err());
        let s = AvroSchema::parse(r#"{"type":"record","name":"R","fields":[{"name":"i","type":"int"}]}"#).unwrap();
        assert!(encode_binary(&s, &json::parse(r#"{}"#).unwrap()).is_err());
        assert!(encode_binary(&s, &json::parse(r#"{"i":"x"}"#).unwrap()).is_err());
        assert!(decode_binary(&s, &[0x80], Conv::default()).is_err());
    }
}

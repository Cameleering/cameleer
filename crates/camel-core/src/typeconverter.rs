//! The pluggable type converter registry (Camel `TypeConverterRegistry`).
//!
//! Message bodies are dynamic [`Value`]s, so a "type" is a (Java-like) name: `String`,
//! `java.lang.Integer`, `byte[]`, `java.util.Map`, `com.acme.Order`, ... Each context has a
//! registry ([`CamelContext::type_converter`]) used by `convertBodyTo`/`convertHeaderTo`,
//! Simple's `bodyAs`/`headerAs`/`mandatoryBodyAs`, and the `resultType` of languages.
//!
//! Lookup order for `convert(value, "T")`: a converter registered for (value type, `T`),
//! then for (`Object`, `T`); then the built-in converters; then the fallback converters in
//! registration order. No result is a `NoTypeConversionAvailableException` for mandatory
//! conversions. Registering a converter for an existing pair follows the
//! [`TypeConverterExists`] policy (Camel's default: override, logged).
//!
//! The value type of a [`Value`] is `null`, `Boolean`, `Integer`/`Long`, `Double`, `String`,
//! `byte[]`, `List` or `Map`; [`canonical_type`] maps Java class names and aliases onto the
//! names used as registry keys (`java.lang.String` -> `String`, `int` -> `Integer`, ...).
//!
//! Built-in conversions (Camel's core converters, adapted to `Value`): `String` (bytes are
//! decoded with the exchange charset `CamelCharsetName`, default UTF-8), `Character`,
//! `Integer`/`Short`/`Byte` (range checked), `Long`/`BigInteger`, `Double`/`Float`/
//! `BigDecimal`/`Number`, `Boolean`, `byte[]`/`InputStream`/`ByteBuffer`, `Map` (also from
//! JSON text and `java.util.Properties` text), `List`/`Collection`/`Iterator`/`Object[]`
//! (JSON text, comma-separated text, single values), `Set` (de-duplicated), `String[]`,
//! `Date`/`Timestamp`/`Calendar` (epoch millis, from ISO-8601 text), `Instant`/
//! `OffsetDateTime`/`ZonedDateTime` (ISO-8601 UTC text), `LocalDate`/`LocalDateTime`/
//! `LocalTime`, `Duration` (millis, from `PT1M`, `1m30s`, numbers), `UUID`, `URI`/`URL`,
//! `File`/`Path`, `Reader`/`StringBuilder`/XML `Document`/`Source` (text), `JsonNode`,
//! `Object` (unchanged).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use crate::error::{kinds, CamelError, Result};
use crate::message::Exchange;
use crate::value::{ObjectMap, Value};

/// A converter for one (from, to) pair. `Ok(None)` means "cannot convert this value"
/// (the next candidate is tried).
pub trait TypeConverter: Send + Sync {
    fn convert(&self, value: &Value, exchange: Option<&Exchange>) -> Result<Option<Value>>;
}

impl<F> TypeConverter for F
where
    F: Fn(&Value, Option<&Exchange>) -> Result<Option<Value>> + Send + Sync,
{
    fn convert(&self, value: &Value, exchange: Option<&Exchange>) -> Result<Option<Value>> {
        self(value, exchange)
    }
}

/// A converter consulted for any target type (Camel `@FallbackConverter`).
pub trait FallbackTypeConverter: Send + Sync {
    fn convert(&self, target: &str, value: &Value, exchange: Option<&Exchange>) -> Result<Option<Value>>;
}

impl<F> FallbackTypeConverter for F
where
    F: Fn(&str, &Value, Option<&Exchange>) -> Result<Option<Value>> + Send + Sync,
{
    fn convert(&self, target: &str, value: &Value, exchange: Option<&Exchange>) -> Result<Option<Value>> {
        self(target, value, exchange)
    }
}

/// What to do when a converter is added for a pair that already has one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TypeConverterExists {
    #[default]
    Override,
    Ignore,
    Fail,
}

/// Conversion statistics (Camel `TypeConverterRegistry.Statistics`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TypeConverterStatistics {
    pub attempts: u64,
    pub hits: u64,
    pub misses: u64,
    pub failures: u64,
}

/// The registry of type converters.
#[derive(Default)]
pub struct TypeConverterRegistry {
    converters: RwLock<HashMap<(String, String), Arc<dyn TypeConverter>>>,
    fallbacks: RwLock<Vec<Arc<dyn FallbackTypeConverter>>>,
    exists: RwLock<TypeConverterExists>,
    attempts: AtomicU64,
    hits: AtomicU64,
    misses: AtomicU64,
    failures: AtomicU64,
}

/// The registry type name of a value.
pub fn value_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "Boolean",
        Value::Int(i) if i32::try_from(*i).is_ok() => "Integer",
        Value::Int(_) => "Long",
        Value::Float(_) => "Double",
        Value::String(_) => "String",
        Value::Bytes(_) => "byte[]",
        Value::Array(_) => "List",
        Value::Object(_) => "Map",
    }
}

/// Canonical registry name of a type name: Java packages of the JDK are dropped
/// (`java.lang.String` -> `String`, `java.util.LinkedHashMap` -> `Map`), primitives and
/// aliases are mapped (`int` -> `Integer`, `bytes` -> `byte[]`); other names (e.g.
/// `com.acme.Order`) are kept as given.
pub fn canonical_type(name: &str) -> String {
    let n = name.trim();
    let jdk = ["java.", "javax.", "org.w3c.", "org.xml.", "com.fasterxml.jackson.databind.", "org.apache.camel."];
    let short = if jdk.iter().any(|p| n.starts_with(p)) || !n.contains('.') { n.rsplit('.').next().unwrap_or(n) } else { return n.to_string() };
    match short {
        "string" | "str" | "CharSequence" => "String",
        "int" | "integer" | "i32" => "Integer",
        "long" | "i64" => "Long",
        "short" | "i16" => "Short",
        "byte" | "i8" => "Byte",
        "double" | "f64" => "Double",
        "float" | "f32" => "Float",
        "boolean" | "bool" => "Boolean",
        "char" => "Character",
        "number" => "Number",
        "bytes" | "Bytes" | "Vec<u8>" => "byte[]",
        "HashMap" | "LinkedHashMap" | "TreeMap" | "object" | "JsonObject" | "ObjectNode" => "Map",
        "ArrayList" | "LinkedList" | "array" | "JsonArray" | "ArrayNode" => "List",
        "HashSet" | "LinkedHashSet" | "TreeSet" => "Set",
        "json" | "Json" => "JsonNode",
        other => other,
    }
    .to_string()
}

impl TypeConverterRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a converter from values of type `from` (`Object` = any) to `to`.
    pub fn add_converter(&self, from: &str, to: &str, converter: impl TypeConverter + 'static) -> Result<()> {
        self.add_converter_arc(from, to, Arc::new(converter))
    }

    pub fn add_converter_arc(&self, from: &str, to: &str, converter: Arc<dyn TypeConverter>) -> Result<()> {
        let key = (canonical_type(from), canonical_type(to));
        let mut m = self.converters.write().unwrap();
        if m.contains_key(&key) {
            match *self.exists.read().unwrap() {
                TypeConverterExists::Ignore => return Ok(()),
                TypeConverterExists::Fail => {
                    return Err(CamelError::illegal_state(format!("Failed to add type converter because a type converter exists from {} to {}", key.0, key.1)))
                }
                TypeConverterExists::Override => {
                    crate::camel_warn!("TypeConverterRegistry", "Overriding type converter from {} to {}", key.0, key.1)
                }
            }
        }
        m.insert(key, converter);
        Ok(())
    }

    /// Remove the converter for a pair; returns whether one was registered.
    pub fn remove_converter(&self, from: &str, to: &str) -> bool {
        self.converters.write().unwrap().remove(&(canonical_type(from), canonical_type(to))).is_some()
    }

    pub fn has_converter(&self, from: &str, to: &str) -> bool {
        self.converters.read().unwrap().contains_key(&(canonical_type(from), canonical_type(to)))
    }

    /// Registered (from, to) pairs, sorted.
    pub fn converters(&self) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = self.converters.read().unwrap().keys().cloned().collect();
        v.sort();
        v
    }

    pub fn add_fallback_converter(&self, converter: impl FallbackTypeConverter + 'static) {
        self.fallbacks.write().unwrap().push(Arc::new(converter));
    }

    pub fn set_type_converter_exists(&self, e: TypeConverterExists) {
        *self.exists.write().unwrap() = e;
    }

    pub fn statistics(&self) -> TypeConverterStatistics {
        TypeConverterStatistics {
            attempts: self.attempts.load(Ordering::Relaxed),
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            failures: self.failures.load(Ordering::Relaxed),
        }
    }

    pub fn reset_statistics(&self) {
        for c in [&self.attempts, &self.hits, &self.misses, &self.failures] {
            c.store(0, Ordering::Relaxed);
        }
    }

    /// Convert `value` to `target`; `Ok(None)` when no converter applies (Camel's
    /// `tryConvertTo` returning null). A `null` value converts to `null`.
    pub fn try_convert(&self, target: &str, value: &Value, exchange: Option<&Exchange>) -> Result<Option<Value>> {
        self.attempts.fetch_add(1, Ordering::Relaxed);
        let r = self.lookup(target, value, exchange);
        match &r {
            Ok(Some(_)) => self.hits.fetch_add(1, Ordering::Relaxed),
            Ok(None) => self.misses.fetch_add(1, Ordering::Relaxed),
            Err(_) => self.failures.fetch_add(1, Ordering::Relaxed),
        };
        r
    }

    /// Mandatory conversion (Camel `mandatoryConvertTo`).
    pub fn convert(&self, target: &str, value: &Value, exchange: Option<&Exchange>) -> Result<Value> {
        match self.try_convert(target, value, exchange) {
            Ok(Some(v)) => Ok(v),
            Ok(None) => Err(no_converter(value, target)),
            Err(e) => Err(conversion_failed(value, target, e)),
        }
    }

    /// Like [`Self::convert`], but a value without an applicable converter is returned
    /// unchanged (used where Camel would keep the value, e.g. `bodyAs` of unknown classes).
    pub fn convert_lenient(&self, target: &str, value: &Value, exchange: Option<&Exchange>) -> Result<Value> {
        match self.try_convert(target, value, exchange) {
            Ok(Some(v)) => Ok(v),
            Ok(None) => Ok(value.clone()),
            Err(e) => Err(conversion_failed(value, target, e)),
        }
    }

    fn lookup(&self, target: &str, value: &Value, exchange: Option<&Exchange>) -> Result<Option<Value>> {
        let to = canonical_type(target);
        let custom: Vec<Arc<dyn TypeConverter>> = {
            let m = self.converters.read().unwrap();
            let from = value_type(value).to_string();
            let mut v = Vec::new();
            if let Some(c) = m.get(&(from.clone(), to.clone())) {
                v.push(c.clone());
            }
            if from == "Long" {
                if let Some(c) = m.get(&("Integer".to_string(), to.clone())) {
                    v.push(c.clone());
                }
            }
            if let Some(c) = m.get(&("Object".to_string(), to.clone())) {
                v.push(c.clone());
            }
            v
        };
        for c in custom {
            if let Some(r) = c.convert(value, exchange)? {
                return Ok(Some(r));
            }
        }
        if value.is_null() {
            return Ok(Some(Value::Null));
        }
        if let Some(r) = builtin(&to, value, exchange)? {
            return Ok(Some(r));
        }
        let fallbacks: Vec<Arc<dyn FallbackTypeConverter>> = self.fallbacks.read().unwrap().clone();
        for f in fallbacks {
            if let Some(r) = f.convert(&to, value, exchange)? {
                return Ok(Some(r));
            }
        }
        Ok(None)
    }
}

/// The registry of the exchange's context, or a shared registry with only the built-in
/// converters when there is no context.
pub fn registry_for(exchange: Option<&Exchange>) -> Arc<TypeConverterRegistry> {
    match exchange.and_then(|e| e.context.as_ref()) {
        Some(c) => c.type_converter(),
        None => default_registry(),
    }
}

/// A shared registry with only the built-in converters.
pub fn default_registry() -> Arc<TypeConverterRegistry> {
    static R: std::sync::OnceLock<Arc<TypeConverterRegistry>> = std::sync::OnceLock::new();
    R.get_or_init(|| Arc::new(TypeConverterRegistry::new())).clone()
}

/// A converter failed (e.g. `NumberFormatException`): reported as
/// `NoTypeConversionAvailableException` with the failure as cause.
fn conversion_failed(v: &Value, target: &str, e: CamelError) -> CamelError {
    if e.kind == kinds::NO_TYPE_CONVERSION_AVAILABLE || e.kind == kinds::TYPE_CONVERSION {
        return e;
    }
    CamelError::new(
        kinds::NO_TYPE_CONVERSION_AVAILABLE,
        format!("No type converter available to convert from type: {} to the required type: {} with value {} due to {}", value_type(v), target, v, e.message),
    )
    .with_cause(e)
}

fn no_converter(v: &Value, target: &str) -> CamelError {
    CamelError::new(
        kinds::NO_TYPE_CONVERSION_AVAILABLE,
        format!("No type converter available to convert from type: {} to the required type: {} with value {}", value_type(v), target, v),
    )
}

fn text(v: &Value, ex: Option<&Exchange>) -> String {
    match v {
        Value::Bytes(b) => {
            let charset = ex.and_then(|e| e.properties.get("CamelCharsetName")).map(|c| c.to_string().to_ascii_uppercase());
            match charset.as_deref() {
                Some("ISO-8859-1" | "LATIN1" | "US-ASCII" | "ASCII") => b.iter().map(|&c| c as char).collect(),
                _ => String::from_utf8_lossy(b).into_owned(),
            }
        }
        other => other.to_string(),
    }
}

fn int_in(v: &Value, min: i64, max: i64) -> Result<Option<Value>> {
    let n = match v {
        Value::Int(i) => Some(*i),
        Value::Float(f) if f.is_finite() => Some(*f as i64),
        Value::Bool(_) => None,
        Value::String(_) | Value::Bytes(_) => {
            let s = v.as_str().unwrap_or("").trim().to_string();
            if s.is_empty() {
                return Ok(Some(Value::Null));
            }
            match s.parse::<i64>() {
                Ok(n) => Some(n),
                Err(_) => {
                    return Err(CamelError::new(kinds::NUMBER_FORMAT, format!("For input string: \"{}\"", s)));
                }
            }
        }
        _ => None,
    };
    match n {
        Some(n) if n < min || n > max => {
            Err(CamelError::new(kinds::NUMBER_FORMAT, format!("Value out of range. Value:\"{}\"", n)))
        }
        Some(n) => Ok(Some(Value::Int(n))),
        None => Ok(None),
    }
}

fn iso_utc(millis: i64) -> String {
    crate::util_time::format_millis_utc(millis, "yyyy-MM-dd'T'HH:mm:ss.SSS'Z'")
}

fn json_or(v: &Value) -> Option<Value> {
    crate::json::parse(v.as_str()?).ok()
}

fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36 && b.iter().enumerate().all(|(i, c)| if [8, 13, 18, 23].contains(&i) { *c == b'-' } else { c.is_ascii_hexdigit() })
}

/// The built-in conversions (see the module documentation).
fn builtin(to: &str, v: &Value, ex: Option<&Exchange>) -> Result<Option<Value>> {
    Ok(Some(match to {
        "Object" => v.clone(),
        "String" | "StringBuilder" | "StringBuffer" | "Reader" | "StringReader" | "Document" | "Node" | "Element" | "Source"
        | "StreamSource" | "DOMSource" | "SAXSource" | "StringSource" => Value::String(text(v, ex)),
        "Character" => match v {
            Value::Int(i) => Value::String(char::from_u32(*i as u32).map(String::from).unwrap_or_default()),
            other => {
                let s = text(other, ex);
                match s.chars().count() {
                    1 => Value::String(s),
                    _ => return Ok(None),
                }
            }
        },
        "Integer" => return int_in(v, i32::MIN as i64, i32::MAX as i64),
        "Short" => return int_in(v, i16::MIN as i64, i16::MAX as i64),
        "Byte" => return int_in(v, i8::MIN as i64, i8::MAX as i64),
        "Long" | "BigInteger" | "AtomicLong" | "AtomicInteger" => return int_in(v, i64::MIN, i64::MAX),
        "Double" | "Float" | "BigDecimal" => match v {
            Value::Int(i) => Value::Float(*i as f64),
            Value::Float(_) => v.clone(),
            Value::String(_) | Value::Bytes(_) => {
                let s = text(v, ex);
                match s.trim().parse::<f64>() {
                    Ok(f) => Value::Float(f),
                    Err(_) if s.trim().is_empty() => Value::Null,
                    Err(_) => return Err(CamelError::new(kinds::NUMBER_FORMAT, format!("For input string: \"{}\"", s.trim()))),
                }
            }
            _ => return Ok(None),
        },
        "Number" => match v {
            Value::Int(_) | Value::Float(_) => v.clone(),
            Value::String(_) | Value::Bytes(_) => match v.as_i64().filter(|_| !text(v, ex).contains('.')) {
                Some(i) => Value::Int(i),
                None => match v.as_f64() {
                    Some(f) => Value::Float(f),
                    None => return Ok(None),
                },
            },
            _ => return Ok(None),
        },
        "Boolean" | "AtomicBoolean" => match v {
            Value::Bool(_) => v.clone(),
            Value::Int(i) => Value::Bool(*i != 0),
            _ => match v.as_bool() {
                Some(b) => Value::Bool(b),
                None => return Ok(None),
            },
        },
        "byte[]" | "InputStream" | "ByteArrayInputStream" | "ByteBuffer" | "Byte[]" => Value::Bytes(v.to_bytes()),
        "Map" | "Properties" => match v {
            Value::Object(_) => v.clone(),
            Value::String(_) | Value::Bytes(_) => match json_or(v) {
                Some(o @ Value::Object(_)) => o,
                _ if to == "Properties" || text(v, ex).contains('=') => {
                    let mut m = ObjectMap::new();
                    for line in text(v, ex).lines() {
                        let l = line.trim();
                        if l.is_empty() || l.starts_with('#') || l.starts_with('!') {
                            continue;
                        }
                        let (k, val) = l.split_once(['=', ':']).unwrap_or((l, ""));
                        m.insert(k.trim().to_string(), Value::String(val.trim().to_string()));
                    }
                    Value::Object(m)
                }
                _ => return Ok(None),
            },
            _ => return Ok(None),
        },
        "List" | "Collection" | "Iterable" | "Iterator" | "Object[]" | "Set" | "String[]" | "Enumeration" => {
            let items: Vec<Value> = match v {
                Value::Array(a) => a.clone(),
                Value::Object(o) => o.values().cloned().collect(),
                Value::String(_) | Value::Bytes(_) => match json_or(v) {
                    Some(Value::Array(a)) => a,
                    _ => {
                        let s = text(v, ex);
                        if s.contains(',') {
                            s.split(',').map(|p| Value::String(p.trim().to_string())).collect()
                        } else {
                            vec![Value::String(s)]
                        }
                    }
                },
                other => vec![other.clone()],
            };
            Value::Array(match to {
                "Set" => {
                    let mut out: Vec<Value> = Vec::new();
                    for i in items {
                        if !out.iter().any(|o| crate::value::values_equal(o, &i)) {
                            out.push(i);
                        }
                    }
                    out
                }
                "String[]" => items.iter().map(|i| Value::String(i.to_string())).collect(),
                _ => items,
            })
        }
        "Date" | "Timestamp" | "Calendar" | "GregorianCalendar" => match crate::language::support::value_to_millis(v) {
            Some(ms) => Value::Int(ms),
            None => return Ok(None),
        },
        "Instant" | "OffsetDateTime" | "ZonedDateTime" => match crate::language::support::value_to_millis(v) {
            Some(ms) => Value::String(iso_utc(ms)),
            None => return Ok(None),
        },
        "LocalDateTime" => match crate::language::support::value_to_millis(v) {
            Some(ms) => Value::String(crate::util_time::format_millis_utc(ms, "yyyy-MM-dd'T'HH:mm:ss")),
            None => return Ok(None),
        },
        "LocalDate" => match crate::language::support::value_to_millis(v) {
            Some(ms) => Value::String(crate::util_time::format_millis_utc(ms, "yyyy-MM-dd")),
            None => return Ok(None),
        },
        "LocalTime" => match v {
            Value::String(s) if s.len() >= 5 && s.as_bytes()[2] == b':' => v.clone(),
            _ => match crate::language::support::value_to_millis(v) {
                Some(ms) => Value::String(crate::util_time::format_millis_utc(ms, "HH:mm:ss")),
                None => return Ok(None),
            },
        },
        "Duration" => match v {
            Value::Int(_) => v.clone(),
            Value::Float(f) => Value::Int(*f as i64),
            _ => match parse_duration_text(&text(v, ex)) {
                Some(ms) => Value::Int(ms),
                None => return Ok(None),
            },
        },
        "UUID" => match v {
            Value::Bytes(b) if b.len() == 16 => {
                let h: String = b.iter().map(|x| format!("{:02x}", x)).collect();
                Value::String(format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32]))
            }
            _ => {
                let s = text(v, ex).trim().to_string();
                if !is_uuid(&s) {
                    return Err(CamelError::illegal_argument(format!("Invalid UUID string: {}", s)));
                }
                Value::String(s.to_ascii_lowercase())
            }
        },
        "URI" | "File" | "Path" => Value::String(text(v, ex)),
        "URL" => {
            let s = text(v, ex);
            if !s.contains("://") && !s.starts_with("file:") && !s.starts_with("mailto:") {
                return Err(CamelError::new("MalformedURLException", format!("no protocol: {}", s)));
            }
            Value::String(s)
        }
        "JsonNode" => match v {
            Value::String(_) | Value::Bytes(_) => match json_or(v) {
                Some(j) => j,
                None => return Ok(None),
            },
            other => other.clone(),
        },
        _ => return Ok(None),
    }))
}

/// Durations: ISO-8601 (`PT1H30M`, `P1D`), `1h30m15s`/`500ms`, or plain millis.
fn parse_duration_text(s: &str) -> Option<i64> {
    let t = s.trim();
    if let Ok(n) = t.parse::<i64>() {
        return Some(n);
    }
    let up = t.to_ascii_uppercase();
    if let Some(rest) = up.strip_prefix('P') {
        let (days, time) = match rest.split_once('T') {
            Some((d, t)) => (d, t),
            None => (rest, ""),
        };
        let mut ms = 0f64;
        if !days.is_empty() {
            ms += days.strip_suffix('D')?.parse::<f64>().ok()? * 86_400_000.0;
        }
        let mut num = String::new();
        for c in time.chars() {
            match c {
                '0'..='9' | '.' | '-' => num.push(c),
                'H' | 'M' | 'S' => {
                    let n: f64 = num.parse().ok()?;
                    num.clear();
                    ms += n * match c {
                        'H' => 3_600_000.0,
                        'M' => 60_000.0,
                        _ => 1000.0,
                    };
                }
                _ => return None,
            }
        }
        return num.is_empty().then_some(ms as i64);
    }
    crate::util::parse_duration(t).ok().map(|d| d.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins() {
        let r = TypeConverterRegistry::new();
        let c = |t: &str, v: Value| r.convert(t, &v, None);
        assert_eq!(c("java.lang.Integer", Value::from("42")).unwrap(), Value::Int(42));
        assert_eq!(c("int", Value::from(4.7)).unwrap(), Value::Int(4));
        assert!(c("Integer", Value::from(3_000_000_000i64)).is_err());
        assert!(c("Byte", Value::from(300)).is_err());
        assert_eq!(c("Double", Value::from(3)).unwrap(), Value::Float(3.0));
        assert_eq!(c("Number", Value::from("7")).unwrap(), Value::Int(7));
        assert_eq!(c("Boolean", Value::from("TRUE")).unwrap(), Value::Bool(true));
        assert_eq!(c("byte[]", Value::from("ab")).unwrap(), Value::Bytes(b"ab".to_vec()));
        assert_eq!(c("String", Value::Bytes(b"hi".to_vec())).unwrap(), Value::from("hi"));
        assert_eq!(c("Character", Value::from(65)).unwrap(), Value::from("A"));
        assert_eq!(c("java.util.Map", Value::from("{\"a\":1}")).unwrap().get("a"), Some(&Value::Int(1)));
        assert_eq!(c("Properties", Value::from("a=1\n# c\nb: two")).unwrap().get("b"), Some(&Value::from("two")));
        assert_eq!(c("List", Value::from("a, b")).unwrap(), Value::from(vec![Value::from("a"), Value::from("b")]));
        assert_eq!(c("Set", Value::from(vec![Value::from(1), Value::from(1), Value::from(2)])).unwrap().as_array().unwrap().len(), 2);
        assert_eq!(c("String[]", Value::from(vec![Value::from(1)])).unwrap(), Value::from(vec![Value::from("1")]));
        assert_eq!(c("java.util.Date", Value::from("1970-01-01T00:00:01Z")).unwrap(), Value::Int(1000));
        assert_eq!(c("Instant", Value::from(0)).unwrap(), Value::from("1970-01-01T00:00:00.000Z"));
        assert_eq!(c("LocalDate", Value::from(86_400_000)).unwrap(), Value::from("1970-01-02"));
        assert_eq!(c("Duration", Value::from("PT1M30S")).unwrap(), Value::Int(90_000));
        assert_eq!(c("Duration", Value::from("2s")).unwrap(), Value::Int(2000));
        assert_eq!(c("UUID", Value::from("123E4567-E89B-12D3-A456-426614174000")).unwrap(), Value::from("123e4567-e89b-12d3-a456-426614174000"));
        assert!(c("UUID", Value::from("nope")).is_err());
        assert!(c("URL", Value::from("nope")).is_err());
        assert_eq!(c("Object", Value::from(1)).unwrap(), Value::from(1));
        assert_eq!(c("String", Value::Null).unwrap(), Value::Null);
        let e = c("com.acme.Order", Value::from(1)).unwrap_err();
        assert_eq!(e.kind, kinds::NO_TYPE_CONVERSION_AVAILABLE);
    }

    #[test]
    fn custom_fallback_exists_policy_and_stats() {
        let r = TypeConverterRegistry::new();
        r.add_converter("String", "com.acme.Order", |v: &Value, _: Option<&Exchange>| {
            Ok(Some(Value::object([("id", Value::from(v.to_string()))])))
        })
        .unwrap();
        assert_eq!(r.convert("com.acme.Order", &Value::from("7"), None).unwrap().get("id"), Some(&Value::from("7")));
        // custom converters take precedence over the built-in ones
        r.add_converter("Object", "String", |v: &Value, _: Option<&Exchange>| Ok(Some(Value::String(format!("<{}>", v))))).unwrap();
        assert_eq!(r.convert("java.lang.String", &Value::from(1), None).unwrap(), Value::from("<1>"));
        r.set_type_converter_exists(TypeConverterExists::Fail);
        assert!(r.add_converter("Object", "String", |_: &Value, _: Option<&Exchange>| Ok(None)).is_err());
        r.set_type_converter_exists(TypeConverterExists::Ignore);
        r.add_converter("Object", "String", |_: &Value, _: Option<&Exchange>| Ok(None)).unwrap();
        assert_eq!(r.convert("String", &Value::from(1), None).unwrap(), Value::from("<1>"));
        r.add_fallback_converter(|to: &str, v: &Value, _: Option<&Exchange>| {
            Ok(if to.starts_with("com.acme.") { Some(Value::object([("fallback", v.clone())])) } else { None })
        });
        assert!(r.convert("com.acme.Customer", &Value::from(1), None).unwrap().get("fallback").is_some());
        assert!(r.try_convert("com.other.X", &Value::from(1), None).unwrap().is_none());
        let s = r.statistics();
        assert!(s.attempts >= 5 && s.hits >= 4 && s.misses == 1);
        assert_eq!(r.converters().len(), 2);
    }
}

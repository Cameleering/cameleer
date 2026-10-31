//! Dynamic value type used for message bodies, headers and exchange properties.
//!
//! Apache Camel bodies/headers are arbitrary Java objects. In rust-camel they are
//! represented by [`Value`], a JSON-like dynamic type that also supports raw bytes.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;

use crate::error::{CamelError, Result};

mod map;
pub use map::{Entry, IntoIter, Iter, IterMut, Keys, ObjectMap, OccupiedEntry, VacantEntry, Values, ValuesMut};

/// Short alias for [`ObjectMap`], the insertion-ordered map behind [`Value::Object`].
pub type Map = ObjectMap;

/// A dynamically typed value.
#[derive(Clone, Debug, Default)]
pub enum Value {
    #[default]
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
    Array(Vec<Value>),
    /// A map with insertion-ordered keys (Camel's `LinkedHashMap`); see [`ObjectMap`].
    Object(ObjectMap),
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        use Value::*;
        match (self, other) {
            (Null, Null) => true,
            (Bool(a), Bool(b)) => a == b,
            (Int(a), Int(b)) => a == b,
            (Float(a), Float(b)) => a == b,
            (Int(a), Float(b)) | (Float(b), Int(a)) => (*a as f64) == *b,
            (String(a), String(b)) => a == b,
            (Bytes(a), Bytes(b)) => a == b,
            (String(a), Bytes(b)) | (Bytes(b), String(a)) => a.as_bytes() == b.as_slice(),
            (Array(a), Array(b)) => a == b,
            (Object(a), Object(b)) => a == b,
            _ => false,
        }
    }
}

impl Value {
    /// Name of the type, useful in error messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Int(_) => "integer",
            Value::Float(_) => "float",
            Value::String(_) => "string",
            Value::Bytes(_) => "bytes",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        }
    }

    /// The Java class name Apache Camel would report for this value (`${bodyType}`, the log
    /// component's `BodyType:`): `java.lang.String`, `java.lang.Integer` (`java.lang.Long` beyond
    /// the 32-bit range), `java.lang.Double`, `java.lang.Boolean`, `byte[]`,
    /// `java.util.ArrayList` and `java.util.LinkedHashMap` (the classes Camel's JSON support
    /// produces for lists and maps). `None` for null.
    pub fn java_type_name(&self) -> Option<&'static str> {
        Some(match self {
            Value::Null => return None,
            Value::Bool(_) => "java.lang.Boolean",
            Value::Int(i) if i32::try_from(*i).is_ok() => "java.lang.Integer",
            Value::Int(_) => "java.lang.Long",
            Value::Float(_) => "java.lang.Double",
            Value::String(_) => "java.lang.String",
            Value::Bytes(_) => "byte[]",
            Value::Array(_) => "java.util.ArrayList",
            Value::Object(_) => "java.util.LinkedHashMap",
        })
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Borrow as `&str` if this is a string (or valid UTF-8 bytes).
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            Value::Bytes(b) => std::str::from_utf8(b).ok(),
            _ => None,
        }
    }

    /// Convert to i64, parsing strings and truncating floats.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Float(f) => Some(*f as i64),
            Value::Bool(b) => Some(*b as i64),
            Value::String(_) | Value::Bytes(_) => {
                let s = self.as_str()?.trim();
                s.parse::<i64>().ok().or_else(|| s.parse::<f64>().ok().filter(|f| f.fract() == 0.0).map(|f| f as i64))
            }
            _ => None,
        }
    }

    /// Convert to f64, parsing strings.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            Value::String(_) | Value::Bytes(_) => self.as_str()?.trim().parse::<f64>().ok(),
            _ => None,
        }
    }

    /// Convert to bool ("true"/"false" strings are accepted, case-insensitive).
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            Value::String(_) | Value::Bytes(_) => match self.as_str()?.trim().to_ascii_lowercase().as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            },
            _ => None,
        }
    }

    /// Camel's `ObjectHelper.evaluateValuePredicate`: the result of an expression used as a
    /// predicate. Exactly as in Camel 4:
    ///
    /// - a boolean is its value,
    /// - a string is `true`/`false` when it is (trimmed, case-insensitive) `"true"`/`"false"`,
    ///   any other string (including `""`) is true because it is not null,
    /// - a collection (array) is true when not empty,
    /// - null is false and every other value (numbers including `0`, maps, byte arrays) is true.
    pub fn is_predicate_true(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::String(s) => !s.trim().eq_ignore_ascii_case("false"),
            Value::Array(a) => !a.is_empty(),
            _ => true,
        }
    }

    /// Lenient boolean conversion used for flag headers/properties (such as
    /// `CamelRouteStop` or `CamelHttpChunked`): null, false, empty string, `"false"`, `0`, empty
    /// array/object are false. This is **not** Camel's predicate semantics; use
    /// [`Value::is_predicate_true`] for the result of an expression used as a predicate.
    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Int(i) => *i != 0,
            Value::Float(f) => *f != 0.0,
            Value::String(s) => !s.is_empty() && !s.eq_ignore_ascii_case("false"),
            Value::Bytes(b) => !b.is_empty(),
            Value::Array(a) => !a.is_empty(),
            Value::Object(o) => !o.is_empty(),
        }
    }

    /// Render the value as bytes (strings as UTF-8, structured values as JSON).
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Value::Bytes(b) => b.clone(),
            Value::Null => Vec::new(),
            other => other.to_string().into_bytes(),
        }
    }

    pub fn as_array(&self) -> Option<&Vec<Value>> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&ObjectMap> {
        match self {
            Value::Object(o) => Some(o),
            _ => None,
        }
    }

    pub fn as_object_mut(&mut self) -> Option<&mut ObjectMap> {
        match self {
            Value::Object(o) => Some(o),
            _ => None,
        }
    }

    /// Look up a key in an object value.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(o) => o.get(key),
            Value::Array(a) => key.parse::<usize>().ok().and_then(|i| a.get(i)),
            _ => None,
        }
    }

    /// Length of string, bytes, array or object; None otherwise.
    pub fn len(&self) -> Option<usize> {
        match self {
            Value::String(s) => Some(s.chars().count()),
            Value::Bytes(b) => Some(b.len()),
            Value::Array(a) => Some(a.len()),
            Value::Object(o) => Some(o.len()),
            Value::Null => Some(0),
            _ => None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len().map(|l| l == 0).unwrap_or(false)
    }

    /// Serialize to a compact JSON string.
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        write_json(self, &mut out, None, 0);
        out
    }

    /// Serialize to a pretty-printed JSON string (2-space indentation).
    pub fn to_json_pretty(&self) -> String {
        let mut out = String::new();
        write_json(self, &mut out, Some(2), 0);
        out
    }

    /// Convert this value into a target Rust type.
    pub fn convert<T: FromValue>(&self) -> Result<T> {
        T::from_value(self)
    }

    /// Build an object value from key/value pairs.
    pub fn object<K: Into<String>, V: Into<Value>, I: IntoIterator<Item = (K, V)>>(items: I) -> Value {
        Value::Object(items.into_iter().map(|(k, v)| (k.into(), v.into())).collect())
    }
}

/// Escape a string for inclusion in JSON.
pub fn json_escape(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn fmt_float(f: f64) -> String {
    if f.is_finite() {
        if f.fract() == 0.0 && f.abs() < 1e16 {
            format!("{:.1}", f)
        } else {
            format!("{}", f)
        }
    } else {
        "null".to_string()
    }
}

fn write_json(v: &Value, out: &mut String, indent: Option<usize>, level: usize) {
    let nl = |out: &mut String, lvl: usize| {
        if let Some(n) = indent {
            out.push('\n');
            for _ in 0..(n * lvl) {
                out.push(' ');
            }
        }
    };
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(i) => out.push_str(&i.to_string()),
        Value::Float(f) => out.push_str(&fmt_float(*f)),
        Value::String(s) => json_escape(s, out),
        Value::Bytes(b) => json_escape(&String::from_utf8_lossy(b), out),
        Value::Array(a) => {
            out.push('[');
            for (i, item) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                nl(out, level + 1);
                write_json(item, out, indent, level + 1);
            }
            if !a.is_empty() {
                nl(out, level);
            }
            out.push(']');
        }
        Value::Object(o) => {
            out.push('{');
            for (i, (k, item)) in o.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                nl(out, level + 1);
                json_escape(k, out);
                out.push(':');
                if indent.is_some() {
                    out.push(' ');
                }
                write_json(item, out, indent, level + 1);
            }
            if !o.is_empty() {
                nl(out, level);
            }
            out.push('}');
        }
    }
}

impl fmt::Display for Value {
    /// Strings render raw (no quotes), null renders as empty string,
    /// arrays/objects render as JSON.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => Ok(()),
            Value::Bool(b) => write!(f, "{}", b),
            Value::Int(i) => write!(f, "{}", i),
            Value::Float(x) => {
                if x.fract() == 0.0 && x.abs() < 1e16 {
                    write!(f, "{:.1}", x)
                } else {
                    write!(f, "{}", x)
                }
            }
            Value::String(s) => f.write_str(s),
            Value::Bytes(b) => f.write_str(&String::from_utf8_lossy(b)),
            Value::Array(_) | Value::Object(_) => f.write_str(&self.to_json()),
        }
    }
}

/// Compare two values the way Camel's simple language / predicates do:
/// numeric comparison when both sides are numeric (or numeric strings),
/// otherwise string comparison. Returns None if values are incomparable (null vs non-null).
pub fn compare_values(a: &Value, b: &Value) -> Option<Ordering> {
    match (a, b) {
        (Value::Null, Value::Null) => Some(Ordering::Equal),
        (Value::Null, _) | (_, Value::Null) => None,
        (Value::Bool(x), Value::Bool(y)) => Some(x.cmp(y)),
        (Value::Bool(x), other) | (other, Value::Bool(x)) if other.as_bool().is_some() => {
            let y = other.as_bool().unwrap();
            if matches!(a, Value::Bool(_)) { Some(x.cmp(&y)) } else { Some(y.cmp(x)) }
        }
        (Value::Int(x), Value::Int(y)) => Some(x.cmp(y)),
        _ => {
            let numeric_a = matches!(a, Value::Int(_) | Value::Float(_));
            let numeric_b = matches!(b, Value::Int(_) | Value::Float(_));
            if numeric_a || numeric_b {
                if let (Some(x), Some(y)) = (a.as_f64(), b.as_f64()) {
                    return x.partial_cmp(&y);
                }
            }
            match (a, b) {
                (Value::Array(x), Value::Array(y)) => {
                    if x == y { Some(Ordering::Equal) } else { None }
                }
                (Value::Object(x), Value::Object(y)) => {
                    if x == y { Some(Ordering::Equal) } else { None }
                }
                _ => Some(a.to_string().cmp(&b.to_string())),
            }
        }
    }
}

/// Equality with type coercion (e.g. `Int(5) == String("5")`).
pub fn values_equal(a: &Value, b: &Value) -> bool {
    if a == b {
        return true;
    }
    matches!(compare_values(a, b), Some(Ordering::Equal))
}

// ---------------------------------------------------------------------------
// Conversions into Value
// ---------------------------------------------------------------------------

macro_rules! from_int {
    ($($t:ty),*) => { $( impl From<$t> for Value { fn from(v: $t) -> Self { Value::Int(v as i64) } } )* };
}
from_int!(i8, i16, i32, i64, u8, u16, u32, u64, usize, isize);

impl From<f32> for Value {
    fn from(v: f32) -> Self {
        Value::Float(v as f64)
    }
}
impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::Float(v)
    }
}
impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}
impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::String(v.to_string())
    }
}
impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::String(v)
    }
}
impl From<&String> for Value {
    fn from(v: &String) -> Self {
        Value::String(v.clone())
    }
}
impl From<Vec<u8>> for Value {
    fn from(v: Vec<u8>) -> Self {
        Value::Bytes(v)
    }
}
impl From<&[u8]> for Value {
    fn from(v: &[u8]) -> Self {
        Value::Bytes(v.to_vec())
    }
}
impl From<Vec<Value>> for Value {
    fn from(v: Vec<Value>) -> Self {
        Value::Array(v)
    }
}
impl From<Vec<String>> for Value {
    fn from(v: Vec<String>) -> Self {
        Value::Array(v.into_iter().map(Value::String).collect())
    }
}
impl From<Vec<&str>> for Value {
    fn from(v: Vec<&str>) -> Self {
        Value::Array(v.into_iter().map(Value::from).collect())
    }
}
impl From<ObjectMap> for Value {
    fn from(v: ObjectMap) -> Self {
        Value::Object(v)
    }
}
impl From<BTreeMap<String, Value>> for Value {
    /// The entries keep the map's (sorted) iteration order.
    fn from(v: BTreeMap<String, Value>) -> Self {
        Value::Object(v.into())
    }
}
impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Self {
        v.map(Into::into).unwrap_or(Value::Null)
    }
}
impl From<()> for Value {
    fn from(_: ()) -> Self {
        Value::Null
    }
}

// ---------------------------------------------------------------------------
// Conversions out of Value (type converter)
// ---------------------------------------------------------------------------

/// Types that can be produced from a [`Value`] (Camel's TypeConverter).
pub trait FromValue: Sized {
    fn from_value(v: &Value) -> Result<Self>;
}

fn conv_err(v: &Value, target: &str) -> CamelError {
    CamelError::new(
        crate::error::kinds::NO_TYPE_CONVERSION_AVAILABLE,
        format!("No type converter available to convert from type: {} to the required type: {} with value {}", v.type_name(), target, v),
    )
}

impl FromValue for Value {
    fn from_value(v: &Value) -> Result<Self> {
        Ok(v.clone())
    }
}
impl FromValue for String {
    fn from_value(v: &Value) -> Result<Self> {
        Ok(v.to_string())
    }
}
impl FromValue for Vec<u8> {
    fn from_value(v: &Value) -> Result<Self> {
        Ok(v.to_bytes())
    }
}
impl FromValue for bool {
    fn from_value(v: &Value) -> Result<Self> {
        v.as_bool().ok_or_else(|| conv_err(v, "bool"))
    }
}
impl FromValue for f64 {
    fn from_value(v: &Value) -> Result<Self> {
        v.as_f64().ok_or_else(|| conv_err(v, "f64"))
    }
}
impl FromValue for f32 {
    fn from_value(v: &Value) -> Result<Self> {
        v.as_f64().map(|f| f as f32).ok_or_else(|| conv_err(v, "f32"))
    }
}
macro_rules! from_value_int {
    ($($t:ty),*) => { $(
        impl FromValue for $t {
            fn from_value(v: &Value) -> Result<Self> {
                v.as_i64().and_then(|i| <$t>::try_from(i).ok()).ok_or_else(|| conv_err(v, stringify!($t)))
            }
        }
    )* };
}
from_value_int!(i8, i16, i32, i64, u8, u16, u32, u64, usize, isize);

impl<T: FromValue> FromValue for Option<T> {
    fn from_value(v: &Value) -> Result<Self> {
        if v.is_null() {
            Ok(None)
        } else {
            T::from_value(v).map(Some)
        }
    }
}
macro_rules! from_value_vec {
    ($($t:ty),*) => { $(
        impl FromValue for Vec<$t> {
            fn from_value(v: &Value) -> Result<Self> {
                match v {
                    Value::Array(a) => a.iter().map(<$t>::from_value).collect(),
                    Value::Null => Ok(Vec::new()),
                    other => Ok(vec![<$t>::from_value(other)?]),
                }
            }
        }
    )* };
}
from_value_vec!(Value, String, i64, i32, f64, bool);

impl FromValue for ObjectMap {
    fn from_value(v: &Value) -> Result<Self> {
        match v {
            Value::Object(o) => Ok(o.clone()),
            Value::Null => Ok(ObjectMap::new()),
            other => Err(conv_err(other, "map")),
        }
    }
}
impl FromValue for BTreeMap<String, Value> {
    fn from_value(v: &Value) -> Result<Self> {
        match v {
            Value::Object(o) => Ok(o.clone().into()),
            Value::Null => Ok(BTreeMap::new()),
            other => Err(conv_err(other, "map")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_and_json() {
        assert_eq!(Value::from("hi").to_string(), "hi");
        assert_eq!(Value::Null.to_string(), "");
        let o = Value::object([("a", Value::from(1)), ("b", Value::from(vec!["x", "y"]))]);
        assert_eq!(o.to_json(), r#"{"a":1,"b":["x","y"]}"#);
        assert_eq!(Value::from("a\"b\n").to_json(), r#""a\"b\n""#);
    }

    #[test]
    fn objects_keep_insertion_order() {
        let o = Value::object([("b", 1), ("a", 2), ("c", 3)]);
        assert_eq!(o.to_json(), r#"{"b":1,"a":2,"c":3}"#);
        assert_eq!(o, Value::object([("a", 2), ("c", 3), ("b", 1)]), "Map.equals ignores order");
        let parsed = crate::json::parse(r#"{"b":1,"a":{"z":true,"y":null}}"#).unwrap();
        assert_eq!(parsed.to_json(), r#"{"b":1,"a":{"z":true,"y":null}}"#);
    }

    #[test]
    fn conversions() {
        assert_eq!(Value::from("42").convert::<i64>().unwrap(), 42);
        assert_eq!(Value::from(3.0).convert::<i32>().unwrap(), 3);
        assert!(Value::from("true").convert::<bool>().unwrap());
        assert!(Value::from("x").convert::<i64>().is_err());
    }

    #[test]
    fn comparisons() {
        assert!(values_equal(&Value::from(5), &Value::from("5")));
        assert_eq!(compare_values(&Value::from("10"), &Value::from(9)), Some(Ordering::Greater));
        assert_eq!(compare_values(&Value::from("b"), &Value::from("a")), Some(Ordering::Greater));
    }
}

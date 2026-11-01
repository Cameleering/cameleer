//! The XQuery data model: items, atomic values, sequence types, casting.

use std::cmp::Ordering;
use std::fmt;
use std::rc::Rc;

use crate::xpath::XNode;

pub use super::decimal::Decimal;

/// An XQuery error with its W3C error code (`XPTY0004`, `FOAR0001`, ...).
#[derive(Clone, Debug, PartialEq)]
pub struct XqError {
    pub code: String,
    pub message: String,
    /// Static (compile time) error.
    pub syntax: bool,
}

impl XqError {
    pub fn new(code: &str, message: impl Into<String>) -> XqError {
        XqError { code: code.to_string(), message: message.into(), syntax: false }
    }
    pub fn syntax(message: impl Into<String>) -> XqError {
        XqError { code: "XPST0003".into(), message: message.into(), syntax: true }
    }
    pub fn static_err(code: &str, message: impl Into<String>) -> XqError {
        XqError { code: code.into(), message: message.into(), syntax: true }
    }
    pub fn type_err(message: impl Into<String>) -> XqError {
        XqError::new("XPTY0004", message)
    }
}

impl fmt::Display for XqError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for XqError {}

/// Result type of the XQuery engine.
pub type XqResult<T> = Result<T, XqError>;

/// Atomic types supported by the engine (integer subtypes map to `Integer`, `xs:float` is
/// computed as a double).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtomicType {
    AnyAtomic,
    Untyped,
    String,
    AnyUri,
    Numeric,
    Integer,
    /// A restricted integer type (`nonNegativeInteger`...) with its bounds.
    IntegerRange(i64, i64),
    Decimal,
    Double,
    Float,
    Boolean,
    DateTime,
    Date,
    Time,
    Duration,
    QName,
}

impl AtomicType {
    /// The type for an `xs:` local name.
    pub fn from_local(n: &str) -> Option<AtomicType> {
        Some(match n {
            "anyAtomicType" => AtomicType::AnyAtomic,
            "untypedAtomic" => AtomicType::Untyped,
            "string" | "normalizedString" | "token" | "language" | "NMTOKEN" | "Name" | "NCName" | "ID" | "IDREF" | "ENTITY" => AtomicType::String,
            "anyURI" => AtomicType::AnyUri,
            "numeric" => AtomicType::Numeric,
            "integer" | "long" => AtomicType::Integer,
            "int" => AtomicType::IntegerRange(i32::MIN as i64, i32::MAX as i64),
            "short" => AtomicType::IntegerRange(i16::MIN as i64, i16::MAX as i64),
            "byte" => AtomicType::IntegerRange(i8::MIN as i64, i8::MAX as i64),
            "nonNegativeInteger" | "unsignedLong" => AtomicType::IntegerRange(0, i64::MAX),
            "positiveInteger" => AtomicType::IntegerRange(1, i64::MAX),
            "nonPositiveInteger" => AtomicType::IntegerRange(i64::MIN, 0),
            "negativeInteger" => AtomicType::IntegerRange(i64::MIN, -1),
            "unsignedInt" => AtomicType::IntegerRange(0, u32::MAX as i64),
            "unsignedShort" => AtomicType::IntegerRange(0, u16::MAX as i64),
            "unsignedByte" => AtomicType::IntegerRange(0, u8::MAX as i64),
            "decimal" => AtomicType::Decimal,
            "double" => AtomicType::Double,
            "float" => AtomicType::Float,
            "boolean" => AtomicType::Boolean,
            "dateTime" | "dateTimeStamp" => AtomicType::DateTime,
            "date" => AtomicType::Date,
            "time" => AtomicType::Time,
            "duration" | "dayTimeDuration" | "yearMonthDuration" => AtomicType::Duration,
            "QName" => AtomicType::QName,
            _ => return None,
        })
    }

    pub fn name(&self) -> &'static str {
        match self {
            AtomicType::AnyAtomic => "xs:anyAtomicType",
            AtomicType::Untyped => "xs:untypedAtomic",
            AtomicType::String => "xs:string",
            AtomicType::AnyUri => "xs:anyURI",
            AtomicType::Numeric => "xs:numeric",
            AtomicType::Integer | AtomicType::IntegerRange(..) => "xs:integer",
            AtomicType::Decimal => "xs:decimal",
            AtomicType::Double => "xs:double",
            AtomicType::Float => "xs:float",
            AtomicType::Boolean => "xs:boolean",
            AtomicType::DateTime => "xs:dateTime",
            AtomicType::Date => "xs:date",
            AtomicType::Time => "xs:time",
            AtomicType::Duration => "xs:duration",
            AtomicType::QName => "xs:QName",
        }
    }
}

/// An atomic value.
#[derive(Clone, Debug, PartialEq)]
pub enum Atomic {
    Untyped(String),
    Str(String),
    AnyUri(String),
    Int(i64),
    Dec(Decimal),
    Dbl(f64),
    Flt(f64),
    Bool(bool),
    DateTime(String),
    Date(String),
    Time(String),
    Duration(String),
    QName(String, String, String),
}

impl Atomic {
    /// The dynamic type.
    pub fn type_of(&self) -> AtomicType {
        match self {
            Atomic::Untyped(_) => AtomicType::Untyped,
            Atomic::Str(_) => AtomicType::String,
            Atomic::AnyUri(_) => AtomicType::AnyUri,
            Atomic::Int(_) => AtomicType::Integer,
            Atomic::Dec(_) => AtomicType::Decimal,
            Atomic::Dbl(_) => AtomicType::Double,
            Atomic::Flt(_) => AtomicType::Float,
            Atomic::Bool(_) => AtomicType::Boolean,
            Atomic::DateTime(_) => AtomicType::DateTime,
            Atomic::Date(_) => AtomicType::Date,
            Atomic::Time(_) => AtomicType::Time,
            Atomic::Duration(_) => AtomicType::Duration,
            Atomic::QName(..) => AtomicType::QName,
        }
    }

    /// The exact decimal value of an `xs:integer` / `xs:decimal`.
    pub fn as_decimal(&self) -> Option<Decimal> {
        match self {
            Atomic::Int(i) => Some(Decimal::from_i64(*i)),
            Atomic::Dec(d) => Some(d.clone()),
            _ => None,
        }
    }

    pub fn is_numeric(&self) -> bool {
        matches!(self, Atomic::Int(_) | Atomic::Dec(_) | Atomic::Dbl(_) | Atomic::Flt(_))
    }

    pub fn is_stringlike(&self) -> bool {
        matches!(self, Atomic::Untyped(_) | Atomic::Str(_) | Atomic::AnyUri(_))
    }

    /// Numeric value as f64 (NaN for non-numerics).
    pub fn as_f64(&self) -> f64 {
        match self {
            Atomic::Int(i) => *i as f64,
            Atomic::Dec(d) => d.to_f64(),
            Atomic::Dbl(f) | Atomic::Flt(f) => *f,
            Atomic::Bool(b) => {
                if *b {
                    1.0
                } else {
                    0.0
                }
            }
            other => parse_double(&other.to_string()).unwrap_or(f64::NAN),
        }
    }

    /// Whether the value is an instance of a type.
    pub fn instance_of(&self, t: AtomicType) -> bool {
        let me = self.type_of();
        match t {
            AtomicType::AnyAtomic => true,
            AtomicType::Numeric => self.is_numeric(),
            AtomicType::Decimal => matches!(self, Atomic::Int(_) | Atomic::Dec(_)),
            AtomicType::IntegerRange(lo, hi) => matches!(self, Atomic::Int(i) if *i >= lo && *i <= hi),
            other => me == other,
        }
    }
}

/// Canonical decimal formatting (no exponent).
pub fn format_decimal(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.fract() == 0.0 && f.abs() < 1e18 {
        return format!("{}", f as i64);
    }
    // decimals are computed as doubles: round to 15 significant digits (0.1 + 0.2 = 0.3)
    let f: f64 = format!("{:.14e}", f).parse().unwrap_or(f);
    if f.fract() == 0.0 && f.abs() < 1e18 {
        return format!("{}", f as i64);
    }
    let s = format!("{}", f);
    if s.contains('e') || s.contains('E') {
        // expand exponent
        let s = format!("{:.20}", f);
        let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
        return s;
    }
    s
}

/// Canonical `xs:double` formatting (XPath 2.0 rules).
pub fn format_double(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "INF".into() } else { "-INF".into() };
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    let a = f.abs();
    if (1e-6..1e6).contains(&a) {
        return format_decimal(f);
    }
    // exponent form: mantissa with at least one fractional digit
    let s = format!("{:E}", f);
    let (m, e) = s.split_once('E').unwrap_or((&s, "0"));
    let m = if m.contains('.') { m.to_string() } else { format!("{}.0", m) };
    format!("{}E{}", m, e)
}

impl fmt::Display for Atomic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Atomic::Untyped(s) | Atomic::Str(s) | Atomic::AnyUri(s) => f.write_str(s),
            Atomic::Int(i) => write!(f, "{}", i),
            Atomic::Dec(d) => write!(f, "{}", d),
            Atomic::Dbl(d) | Atomic::Flt(d) => f.write_str(&format_double(*d)),
            Atomic::Bool(b) => write!(f, "{}", b),
            Atomic::DateTime(s) | Atomic::Date(s) | Atomic::Time(s) | Atomic::Duration(s) => f.write_str(s),
            Atomic::QName(p, l, _) => {
                if p.is_empty() {
                    f.write_str(l)
                } else {
                    write!(f, "{}:{}", p, l)
                }
            }
        }
    }
}

/// An item: a node, an atomic value, or (XQuery 3.1) a function, map or array.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Node(XNode),
    Atomic(Atomic),
    Func(Rc<FuncItem>),
    Map(Rc<XqMap>),
    Array(Rc<Vec<Vec<Item>>>),
}

/// A function item.
#[derive(Debug)]
pub enum FuncItem {
    /// A named function reference (`local:f#2`, `fn:upper-case#1`).
    Named { name: super::ast::QN, arity: usize },
    /// An inline function with its captured variables.
    Inline { params: Vec<(super::ast::QN, Option<SeqType>)>, ret: Option<SeqType>, body: std::sync::Arc<super::ast::Expr>, closure: Vec<(super::ast::QN, Rc<Vec<Item>>)> },
    /// A partial application (`f(?, 2)`): `None` arguments are the placeholders.
    Partial { base: Item, args: Vec<Option<Vec<Item>>> },
}

impl FuncItem {
    pub fn arity(&self) -> usize {
        match self {
            FuncItem::Named { arity, .. } => *arity,
            FuncItem::Inline { params, .. } => params.len(),
            FuncItem::Partial { args, .. } => args.iter().filter(|a| a.is_none()).count(),
        }
    }
}

impl PartialEq for FuncItem {
    fn eq(&self, other: &FuncItem) -> bool {
        std::ptr::eq(self, other)
    }
}

/// An XQuery 3.1 map (insertion-ordered entries, keys compared with `op:same-key`).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct XqMap {
    pub entries: Vec<(Atomic, Vec<Item>)>,
}

/// `op:same-key`: string-like keys compare as strings, numerics by value.
pub fn same_key(a: &Atomic, b: &Atomic) -> bool {
    if a.is_stringlike() && b.is_stringlike() {
        return a.to_string() == b.to_string();
    }
    if a.is_numeric() && b.is_numeric() {
        let (x, y) = (a.as_f64(), b.as_f64());
        return x == y || (x.is_nan() && y.is_nan());
    }
    if a.type_of() != b.type_of() {
        return false;
    }
    matches!(compare_atomic(a, b), Ok(Some(Ordering::Equal)))
}

impl XqMap {
    pub fn get(&self, k: &Atomic) -> Option<&Vec<Item>> {
        self.entries.iter().find(|(x, _)| same_key(x, k)).map(|(_, v)| v)
    }
    pub fn put(&mut self, k: Atomic, v: Vec<Item>) {
        match self.entries.iter_mut().find(|(x, _)| same_key(x, &k)) {
            Some(e) => e.1 = v,
            None => self.entries.push((k, v)),
        }
    }
    pub fn remove(&mut self, k: &Atomic) {
        self.entries.retain(|(x, _)| !same_key(x, k));
    }
}

impl Item {
    pub fn str(s: impl Into<String>) -> Item {
        Item::Atomic(Atomic::Str(s.into()))
    }
    pub fn int(i: i64) -> Item {
        Item::Atomic(Atomic::Int(i))
    }
    pub fn bool(b: bool) -> Item {
        Item::Atomic(Atomic::Bool(b))
    }
    pub fn dbl(f: f64) -> Item {
        Item::Atomic(Atomic::Dbl(f))
    }
}

/// Kinds of items in sequence types.
#[derive(Clone, Debug, PartialEq)]
pub enum ItemType {
    Item,
    AnyNode,
    Document(Option<Box<ItemType>>),
    /// `element()`, `element(name)` (None uri/local = wildcard).
    Element(Option<(String, String)>),
    Attribute(Option<(String, String)>),
    Text,
    Comment,
    Pi(Option<String>),
    Atomic(AtomicType),
    /// `empty-sequence()`
    Empty,
    /// `function(*)` / `function(T, ...) as R` (signatures are not checked).
    Function,
    /// `map(*)` / `map(K, V)`.
    Map,
    /// `array(*)` / `array(T)`.
    Array,
}

/// Occurrence indicators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Occ {
    One,
    Opt,
    Star,
    Plus,
}

/// A sequence type (`xs:integer*`, `element(a)?`, `empty-sequence()`).
#[derive(Clone, Debug, PartialEq)]
pub struct SeqType {
    pub item: ItemType,
    pub occ: Occ,
}

impl SeqType {
    pub fn describe(&self) -> String {
        let base = match &self.item {
            ItemType::Item => "item()".to_string(),
            ItemType::AnyNode => "node()".into(),
            ItemType::Document(_) => "document-node()".into(),
            ItemType::Element(None) => "element()".into(),
            ItemType::Element(Some((_, l))) => format!("element({})", l),
            ItemType::Attribute(None) => "attribute()".into(),
            ItemType::Attribute(Some((_, l))) => format!("attribute({})", l),
            ItemType::Text => "text()".into(),
            ItemType::Comment => "comment()".into(),
            ItemType::Pi(_) => "processing-instruction()".into(),
            ItemType::Atomic(t) => t.name().into(),
            ItemType::Empty => return "empty-sequence()".into(),
            ItemType::Function => "function(*)".into(),
            ItemType::Map => "map(*)".into(),
            ItemType::Array => "array(*)".into(),
        };
        let o = match self.occ {
            Occ::One => "",
            Occ::Opt => "?",
            Occ::Star => "*",
            Occ::Plus => "+",
        };
        format!("{}{}", base, o)
    }
}

/// Parse an `xs:double` lexical form (also `INF`, `-INF`, `NaN`).
pub fn parse_double(s: &str) -> Option<f64> {
    let t = s.trim();
    match t {
        "INF" | "+INF" => return Some(f64::INFINITY),
        "-INF" => return Some(f64::NEG_INFINITY),
        "NaN" => return Some(f64::NAN),
        _ => {}
    }
    if t.is_empty() || !t.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E')) {
        return None;
    }
    t.parse::<f64>().ok()
}

fn parse_decimal(s: &str) -> Option<Decimal> {
    let t = s.trim();
    let body = t.strip_prefix(['-', '+']).unwrap_or(t);
    if body.is_empty() || body == "." || !body.chars().all(|c| c.is_ascii_digit() || c == '.') || body.matches('.').count() > 1 {
        return None;
    }
    Decimal::parse(t)
}

fn parse_integer(s: &str) -> Option<i64> {
    let t = s.trim();
    let body = t.strip_prefix(['-', '+']).unwrap_or(t);
    if body.is_empty() || !body.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    t.parse::<i64>().ok()
}

fn digits(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|b| b.is_ascii_digit())
}

fn valid_tz(s: &str) -> bool {
    s.is_empty() || s == "Z" || (s.len() == 6 && (s.starts_with('+') || s.starts_with('-')) && digits(&s[1..3], 2) && &s[3..4] == ":" && digits(&s[4..6], 2))
}

fn split_tz(s: &str) -> (&str, &str) {
    if let Some(stripped) = s.strip_suffix('Z') {
        return (stripped, "Z");
    }
    if s.len() > 6 {
        let (a, b) = s.split_at(s.len() - 6);
        if (b.starts_with('+') || b.starts_with('-')) && &b[3..4] == ":" {
            return (a, b);
        }
    }
    (s, "")
}

fn valid_date(s: &str) -> bool {
    let (d, tz) = split_tz(s);
    let parts: Vec<&str> = d.trim_start_matches('-').splitn(3, '-').collect();
    if parts.len() != 3 || parts[0].len() < 4 || !parts[0].bytes().all(|b| b.is_ascii_digit()) || !digits(parts[1], 2) || !digits(parts[2], 2) {
        return false;
    }
    let m: u32 = parts[1].parse().unwrap_or(0);
    let day: u32 = parts[2].parse().unwrap_or(0);
    (1..=12).contains(&m) && (1..=31).contains(&day) && valid_tz(tz)
}

fn valid_time(s: &str) -> bool {
    let (t, tz) = split_tz(s);
    let (hms, frac) = t.split_once('.').unwrap_or((t, ""));
    let p: Vec<&str> = hms.split(':').collect();
    p.len() == 3
        && p.iter().all(|x| digits(x, 2))
        && p[0].parse::<u32>().unwrap_or(99) <= 24
        && p[1].parse::<u32>().unwrap_or(99) < 60
        && p[2].parse::<u32>().unwrap_or(99) < 60
        && frac.bytes().all(|b| b.is_ascii_digit())
        && valid_tz(tz)
}

fn valid_datetime(s: &str) -> bool {
    match s.split_once('T') {
        Some((d, t)) => valid_date(d) && valid_time(t),
        None => false,
    }
}

fn valid_duration(s: &str) -> bool {
    let t = s.strip_prefix('-').unwrap_or(s);
    t.starts_with('P') && t.len() > 1 && t[1..].chars().all(|c| c.is_ascii_digit() || matches!(c, 'Y' | 'M' | 'D' | 'T' | 'H' | 'S' | '.'))
}

/// Cast an atomic value to a type (`cast as`, constructor functions).
pub fn cast(a: &Atomic, t: AtomicType) -> XqResult<Atomic> {
    let s = a.to_string();
    let bad = || XqError::new("FORG0001", format!("Cannot convert '{}' to {}", s, t.name()));
    let ws = s.trim();
    Ok(match t {
        AtomicType::AnyAtomic => a.clone(),
        AtomicType::Untyped => Atomic::Untyped(s.clone()),
        AtomicType::String => Atomic::Str(s.clone()),
        AtomicType::AnyUri => Atomic::AnyUri(ws.to_string()),
        AtomicType::Numeric | AtomicType::Double | AtomicType::Float => {
            let f = match a {
                Atomic::Bool(b) => {
                    if *b {
                        1.0
                    } else {
                        0.0
                    }
                }
                x if x.is_numeric() => x.as_f64(),
                x if x.is_stringlike() => parse_double(ws).ok_or_else(bad)?,
                _ => return Err(XqError::type_err(format!("Cannot cast {} to {}", a.type_of().name(), t.name()))),
            };
            if t == AtomicType::Float {
                Atomic::Flt(f as f32 as f64)
            } else {
                Atomic::Dbl(f)
            }
        }
        AtomicType::Decimal => match a {
            Atomic::Int(i) => Atomic::Dec(Decimal::from_i64(*i)),
            Atomic::Dec(d) => Atomic::Dec(d.clone()),
            Atomic::Dbl(d) | Atomic::Flt(d) => match Decimal::from_f64(*d) {
                Some(x) => Atomic::Dec(x),
                None => return Err(XqError::new("FOCA0002", format!("Cannot convert {} to xs:decimal", s))),
            },
            Atomic::Bool(b) => Atomic::Dec(Decimal::from_i64(*b as i64)),
            x if x.is_stringlike() => Atomic::Dec(parse_decimal(ws).ok_or_else(bad)?),
            _ => return Err(XqError::type_err(format!("Cannot cast {} to xs:decimal", a.type_of().name()))),
        },
        AtomicType::Integer | AtomicType::IntegerRange(..) => {
            let i = match a {
                Atomic::Int(i) => *i,
                Atomic::Dec(d) => d.trunc().to_i64().ok_or_else(|| XqError::new("FOCA0003", format!("Value {} is too large for xs:integer", s)))?,
                Atomic::Dbl(d) | Atomic::Flt(d) => {
                    if d.is_nan() || d.is_infinite() {
                        return Err(XqError::new("FOCA0002", format!("Cannot convert {} to xs:integer", s)));
                    }
                    d.trunc() as i64
                }
                Atomic::Bool(b) => *b as i64,
                x if x.is_stringlike() => parse_integer(ws).ok_or_else(bad)?,
                _ => return Err(XqError::type_err(format!("Cannot cast {} to xs:integer", a.type_of().name()))),
            };
            if let AtomicType::IntegerRange(lo, hi) = t {
                if i < lo || i > hi {
                    return Err(bad());
                }
            }
            Atomic::Int(i)
        }
        AtomicType::Boolean => match a {
            Atomic::Bool(b) => Atomic::Bool(*b),
            x if x.is_numeric() => {
                let f = x.as_f64();
                Atomic::Bool(f != 0.0 && !f.is_nan())
            }
            x if x.is_stringlike() => match ws {
                "true" | "1" => Atomic::Bool(true),
                "false" | "0" => Atomic::Bool(false),
                _ => return Err(bad()),
            },
            _ => return Err(XqError::type_err(format!("Cannot cast {} to xs:boolean", a.type_of().name()))),
        },
        AtomicType::DateTime => match a {
            Atomic::DateTime(s) => Atomic::DateTime(s.clone()),
            Atomic::Date(d) => {
                let (day, tz) = split_tz(d);
                Atomic::DateTime(format!("{}T00:00:00{}", day, tz))
            }
            x if x.is_stringlike() && valid_datetime(ws) => Atomic::DateTime(ws.to_string()),
            _ => return Err(bad()),
        },
        AtomicType::Date => match a {
            Atomic::Date(s) => Atomic::Date(s.clone()),
            Atomic::DateTime(dt) => {
                let (body, tz) = split_tz(dt);
                Atomic::Date(format!("{}{}", body.split('T').next().unwrap_or(""), tz))
            }
            x if x.is_stringlike() && valid_date(ws) => Atomic::Date(ws.to_string()),
            _ => return Err(bad()),
        },
        AtomicType::Time => match a {
            Atomic::Time(s) => Atomic::Time(s.clone()),
            Atomic::DateTime(dt) => Atomic::Time(dt.split_once('T').map(|x| x.1).unwrap_or("").to_string()),
            x if x.is_stringlike() && valid_time(ws) => Atomic::Time(ws.to_string()),
            _ => return Err(bad()),
        },
        AtomicType::Duration => match a {
            Atomic::Duration(s) => Atomic::Duration(s.clone()),
            x if x.is_stringlike() && valid_duration(ws) => Atomic::Duration(super::datetime::Duration::parse(ws).map_err(|_| bad())?.format()),
            _ => return Err(bad()),
        },
        AtomicType::QName => match a {
            Atomic::QName(..) => a.clone(),
            _ => return Err(XqError::type_err("Casting to xs:QName is not supported")),
        },
    })
}

/// Value comparison of two atomics (`eq`, `lt`...): `None` when incomparable.
pub fn compare_atomic(a: &Atomic, b: &Atomic) -> XqResult<Option<Ordering>> {
    if a.is_numeric() && b.is_numeric() {
        if let (Atomic::Int(x), Atomic::Int(y)) = (a, b) {
            return Ok(Some(x.cmp(y)));
        }
        if let (Some(x), Some(y)) = (a.as_decimal(), b.as_decimal()) {
            return Ok(Some(x.cmp(&y)));
        }
        return Ok(a.as_f64().partial_cmp(&b.as_f64()));
    }
    if a.is_stringlike() && b.is_stringlike() {
        return Ok(Some(a.to_string().cmp(&b.to_string())));
    }
    match (a, b) {
        (Atomic::Bool(x), Atomic::Bool(y)) => Ok(Some(x.cmp(y))),
        (Atomic::DateTime(x), Atomic::DateTime(y)) => Ok(Some(super::datetime::compare(a, b).unwrap_or_else(|| datetime_key(x).cmp(&datetime_key(y))))),
        (Atomic::Date(x), Atomic::Date(y)) | (Atomic::Time(x), Atomic::Time(y)) | (Atomic::Duration(x), Atomic::Duration(y)) => Ok(Some(super::datetime::compare(a, b).unwrap_or_else(|| x.cmp(y)))),
        (Atomic::QName(_, l1, u1), Atomic::QName(_, l2, u2)) => Ok(if l1 == l2 && u1 == u2 { Some(Ordering::Equal) } else { Some(Ordering::Less) }),
        _ => Err(XqError::type_err(format!("Cannot compare {} with {}", a.type_of().name(), b.type_of().name()))),
    }
}

/// Normalize a dateTime to UTC for comparison when it has a timezone.
fn datetime_key(s: &str) -> String {
    let (body, tz) = split_tz(s);
    if tz.is_empty() || tz == "Z" {
        return body.to_string();
    }
    let sign = if tz.starts_with('-') { -1 } else { 1 };
    let oh: i64 = tz[1..3].parse().unwrap_or(0);
    let om: i64 = tz[4..6].parse().unwrap_or(0);
    let Some((d, t)) = body.split_once('T') else { return body.to_string() };
    let dp: Vec<i64> = d.split('-').filter_map(|x| x.parse().ok()).collect();
    let (hms, frac) = t.split_once('.').map(|(a, b)| (a, format!(".{}", b))).unwrap_or((t, String::new()));
    let tp: Vec<i64> = hms.split(':').filter_map(|x| x.parse().ok()).collect();
    if dp.len() != 3 || tp.len() != 3 {
        return body.to_string();
    }
    let secs = camel_core::util_time::epoch_from_civil(dp[0], dp[1] as u32, dp[2] as u32, tp[0] as u32, tp[1] as u32, tp[2] as u32) - sign * (oh * 3600 + om * 60);
    let (y, mo, da, h, mi, se) = camel_core::util_time::civil_from_epoch(secs);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{}", y, mo, da, h, mi, se, frac)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting_and_casts() {
        assert_eq!(format_double(1e7), "1.0E7");
        assert_eq!(format_double(0.5), "0.5");
        assert_eq!(format_double(123.0), "123");
        assert_eq!(format_decimal(2.5), "2.5");
        assert_eq!(cast(&Atomic::Str("12".into()), AtomicType::Integer).unwrap(), Atomic::Int(12));
        assert!(cast(&Atomic::Str("x".into()), AtomicType::Integer).is_err());
        assert!(cast(&Atomic::Str("2024-02-30".into()), AtomicType::Date).is_ok());
        assert!(cast(&Atomic::Str("2024-13-01".into()), AtomicType::Date).is_err());
        assert!(cast(&Atomic::Str("-1".into()), AtomicType::from_local("positiveInteger").unwrap()).is_err());
        assert_eq!(
            compare_atomic(&Atomic::DateTime("2024-01-01T10:00:00+02:00".into()), &Atomic::DateTime("2024-01-01T08:00:00Z".into())).unwrap(),
            Some(Ordering::Equal)
        );
    }
}

//! Shared helpers for the expression languages: type conversion by (Java-like) type
//! name, Camel predicate truthiness, iteration of values, random numbers, UUIDs,
//! hashing (SHA-256/SHA-1/MD5), date formatting/parsing with time zones and
//! registry beans ([`BeanMethod`]).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::context::{CamelContext, Registry};
use crate::error::{CamelError, Result};
use crate::message::Exchange;
use crate::value::{ObjectMap, Value};

// ---------------------------------------------------------------------------
// Bean methods
// ---------------------------------------------------------------------------

/// A "bean method" bound in the registry, invoked by the `bean`/`method` languages and
/// the Simple `${bean:name}` / `${bean:name.method}` functions.
///
/// Bind with [`bind_bean_method`] using either the bean name (`"greeter"`) or
/// `"bean.method"` (`"greeter.hello"`).
#[derive(Clone)]
pub struct BeanMethod(pub Arc<dyn Fn(&Exchange) -> Result<Value> + Send + Sync>);

impl BeanMethod {
    pub fn new(f: impl Fn(&Exchange) -> Result<Value> + Send + Sync + 'static) -> Self {
        BeanMethod(Arc::new(f))
    }
    pub fn invoke(&self, ex: &Exchange) -> Result<Value> {
        (self.0)(ex)
    }
}

/// Bind a bean method in the registry under `name` (`bean` or `bean.method`).
pub fn bind_bean_method(registry: &Registry, name: impl Into<String>, f: impl Fn(&Exchange) -> Result<Value> + Send + Sync + 'static) {
    registry.bind(name, BeanMethod::new(f));
}

/// Invoke bean `name` with optional `method`. Lookup order: `name.method`, `name::method`,
/// `name`; a registered processor is invoked on a copy of the exchange and its body returned.
pub fn invoke_bean(ctx: &CamelContext, name: &str, method: Option<&str>, ex: &Exchange) -> Result<Value> {
    let reg = ctx.registry();
    let mut candidates = Vec::new();
    if let Some(m) = method.filter(|m| !m.is_empty()) {
        // strip parameter list `hello(${body})` -> `hello`
        let m = m.split('(').next().unwrap_or(m);
        candidates.push(format!("{}.{}", name, m));
        candidates.push(format!("{}::{}", name, m));
    }
    candidates.push(name.to_string());
    for c in &candidates {
        if let Some(b) = reg.lookup::<BeanMethod>(c) {
            return b.invoke(ex);
        }
    }
    for c in &candidates {
        if let Some(p) = reg.lookup_processor(c) {
            let mut copy = ex.clone();
            p.process(&mut copy)?;
            if let Some(e) = copy.exception {
                return Err(e);
            }
            return Ok(copy.message.body);
        }
        if let Some(e) = reg.lookup::<Arc<dyn crate::spi::Expression>>(c) {
            return e.evaluate(ex);
        }
    }
    Err(CamelError::no_such_bean(name))
}

/// Look up a registry bean as a value (`ref:` function): supports `Value`, `String`,
/// `&'static str`, integers, floats, bools, expressions (evaluated) and bean methods.
pub fn lookup_ref_value(ctx: &CamelContext, name: &str, ex: &Exchange) -> Result<Value> {
    let reg = ctx.registry();
    if let Some(v) = reg.lookup::<Value>(name) {
        return Ok((*v).clone());
    }
    if let Some(v) = reg.lookup::<String>(name) {
        return Ok(Value::String((*v).clone()));
    }
    if let Some(v) = reg.lookup::<&'static str>(name) {
        return Ok(Value::from(*v));
    }
    if let Some(v) = reg.lookup::<i64>(name) {
        return Ok(Value::Int(*v));
    }
    if let Some(v) = reg.lookup::<i32>(name) {
        return Ok(Value::Int(*v as i64));
    }
    if let Some(v) = reg.lookup::<f64>(name) {
        return Ok(Value::Float(*v));
    }
    if let Some(v) = reg.lookup::<bool>(name) {
        return Ok(Value::Bool(*v));
    }
    if let Some(e) = reg.lookup::<Arc<dyn crate::spi::Expression>>(name) {
        return e.evaluate(ex);
    }
    if let Some(b) = reg.lookup::<BeanMethod>(name) {
        return b.invoke(ex);
    }
    if reg.contains(name) {
        return Ok(Value::String(name.to_string()));
    }
    Ok(Value::Null)
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

fn short_type(t: &str) -> String {
    let t = t.trim();
    let t = t.rsplit('.').next().unwrap_or(t);
    t.to_string()
}

/// Convert a value to a (Java-like) type name with the built-in converters (see
/// [`crate::typeconverter`]). Types without a converter return the value unchanged.
pub fn convert_to_type(v: &Value, type_name: &str) -> Result<Value> {
    convert_to_type_ex(None, v, type_name)
}

/// Convert with the type converter registry of the exchange's context (custom converters
/// included). Types without a converter return the value unchanged.
pub fn convert_to_type_ex(ex: Option<&Exchange>, v: &Value, type_name: &str) -> Result<Value> {
    crate::typeconverter::registry_for(ex).convert_lenient(type_name, v, ex)
}

/// Whether `v` is an instance of the (Java-like) type name (used by the `is` operator).
pub fn is_instance_of(v: &Value, type_name: &str) -> bool {
    let t = short_type(type_name);
    match t.as_str() {
        "Object" => !v.is_null(),
        "String" | "CharSequence" | "string" => matches!(v, Value::String(_)),
        "Integer" | "int" | "Long" | "long" | "Short" | "Byte" | "integer" => matches!(v, Value::Int(_)),
        "Double" | "double" | "Float" | "float" | "BigDecimal" => matches!(v, Value::Float(_)),
        "Number" | "number" => matches!(v, Value::Int(_) | Value::Float(_)),
        "Boolean" | "boolean" => matches!(v, Value::Bool(_)),
        "byte[]" | "bytes" => matches!(v, Value::Bytes(_)),
        "List" | "ArrayList" | "Collection" | "Iterable" | "Object[]" | "array" => matches!(v, Value::Array(_)),
        "Map" | "HashMap" | "LinkedHashMap" | "object" => matches!(v, Value::Object(_)),
        "null" => v.is_null(),
        other => v.type_name().eq_ignore_ascii_case(other),
    }
}

/// The value of a static field for the Simple `${type:...}` function. Supports the constants
/// of `org.apache.camel.Exchange` (e.g. `org.apache.camel.Exchange.FILE_NAME` ->
/// `CamelFileName`; `Exchange.FILE_NAME` is accepted too) and the `MIN_VALUE`/`MAX_VALUE` of
/// `java.lang.Integer`/`Long`, `java.lang.Boolean.TRUE`/`FALSE`.
pub fn type_constant(name: &str) -> Option<Value> {
    let (class, field) = name.rsplit_once('.')?;
    match class {
        "org.apache.camel.Exchange" | "Exchange" => exchange_constant(field).map(Value::from),
        "java.lang.Integer" | "Integer" => match field {
            "MAX_VALUE" => Some(Value::Int(i32::MAX as i64)),
            "MIN_VALUE" => Some(Value::Int(i32::MIN as i64)),
            _ => None,
        },
        "java.lang.Long" | "Long" => match field {
            "MAX_VALUE" => Some(Value::Int(i64::MAX)),
            "MIN_VALUE" => Some(Value::Int(i64::MIN)),
            _ => None,
        },
        "java.lang.Boolean" | "Boolean" => match field {
            "TRUE" => Some(Value::Bool(true)),
            "FALSE" => Some(Value::Bool(false)),
            _ => None,
        },
        _ => None,
    }
}

/// The string value of an `org.apache.camel.Exchange` constant: `FILE_NAME` -> `CamelFileName`.
/// Most constants follow that naming rule; the exceptions are listed explicitly.
pub fn exchange_constant(field: &str) -> Option<String> {
    if field.is_empty() || !field.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') {
        return None;
    }
    let special = match field {
        "CONTENT_TYPE" => Some("Content-Type"),
        "CONTENT_ENCODING" => Some("Content-Encoding"),
        "CONTENT_LENGTH" => Some("Content-Length"),
        "TRANSFER_ENCODING" => Some("Transfer-Encoding"),
        "BREADCRUMB_ID" => Some("breadcrumbId"),
        "DEFAULT_CHARSET_PROPERTY" => Some("org.apache.camel.default.charset"),
        "ERRORHANDLER_HANDLED" => Some("CamelErrorHandlerHandled"),
        "ERRORHANDLER_BRIDGE" => Some("CamelErrorHandlerBridge"),
        "ERRORHANDLER_CIRCUIT_DETECTED" => Some("CamelFErrorHandlerCircuitDetected"),
        "EXCEPTION_HANDLED" => Some("CamelExceptionHandled"),
        "HTTP_SERVLET_REQUEST" => Some("CamelHttpServletRequest"),
        "HTTP_SERVLET_RESPONSE" => Some("CamelHttpServletResponse"),
        "OVERRULE_FILE_NAME" => Some("CamelOverruleFileName"),
        "SAXPARSER_FACTORY" => Some("CamelSAXParserFactory"),
        "XSLT_FILE_NAME" => Some("CamelXsltFileName"),
        "XSLT_ERROR" => Some("CamelXsltError"),
        "XSLT_FATAL_ERROR" => Some("CamelXsltFatalError"),
        "XSLT_WARNING" => Some("CamelXsltWarning"),
        "SOAP_ACTION" => Some("CamelSoapAction"),
        "AUTHENTICATION" => Some("CamelAuthentication"),
        "DUPLICATE_MESSAGE" => Some("CamelDuplicateMessage"),
        "OFFSET" => Some("CamelOffset"),
        "FILTER_NON_XML_CHARS" => Some("CamelFilterNonXmlChars"),
        _ => None,
    };
    if let Some(s) = special {
        return Some(s.to_string());
    }
    let mut out = String::from("Camel");
    for part in field.split('_').filter(|p| !p.is_empty()) {
        let mut cs = part.chars();
        if let Some(first) = cs.next() {
            out.push(first);
            out.push_str(&cs.as_str().to_ascii_lowercase());
        }
    }
    Some(out)
}

/// Camel's `ObjectHelper.evaluateValuePredicate` (see [`Value::is_predicate_true`]): null,
/// `false`, `"false"` and empty collections are false; any other value (including `0` and
/// `""`) is true.
pub fn value_predicate(v: &Value) -> bool {
    v.is_predicate_true()
}

/// Iterate a value like Camel's `ObjectHelper.createIterator`: arrays item by item,
/// objects by values, strings split on commas, null as empty, others as one item.
pub fn iterate(v: &Value) -> Vec<Value> {
    match v {
        Value::Null => vec![],
        Value::Array(a) => a.clone(),
        Value::Object(o) => o.values().cloned().collect(),
        Value::String(s) => {
            if s.is_empty() {
                vec![]
            } else {
                s.split(',').map(|p| Value::String(p.trim().to_string())).collect()
            }
        }
        other => vec![other.clone()],
    }
}

// ---------------------------------------------------------------------------
// Random / UUID
// ---------------------------------------------------------------------------

static RNG_STATE: AtomicU64 = AtomicU64::new(0);

/// A pseudo random u64 (splitmix64 over a time/counter seed). Not cryptographically secure.
pub fn random_u64() -> u64 {
    let mut s = RNG_STATE.load(Ordering::Relaxed);
    if s == 0 {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1);
        s = t ^ ((std::process::id() as u64) << 32) ^ 0x9E37_79B9_7F4A_7C15;
    }
    let next = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
    RNG_STATE.store(next, Ordering::Relaxed);
    let mut z = next;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Random integer in `[min, max)`.
pub fn random_range(min: i64, max: i64) -> i64 {
    if max <= min {
        return min;
    }
    let span = (max - min) as u64;
    min + (random_u64() % span) as i64
}

/// A random (version 4) UUID string.
pub fn uuid_v4() -> String {
    let a = random_u64();
    let b = random_u64();
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&a.to_be_bytes());
    bytes[8..].copy_from_slice(&b.to_be_bytes());
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let h = to_hex(&bytes);
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// Lowercase hex encoding.
pub fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 15) as usize] as char);
    }
    s
}

// ---------------------------------------------------------------------------
// Hashing
// ---------------------------------------------------------------------------

/// SHA-256 digest.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01,
        0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
        0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
        0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08,
        0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
        0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut msg = data.to_vec();
    let bitlen = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bitlen.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v[7] = v[6];
            v[6] = v[5];
            v[5] = v[4];
            v[4] = v[3].wrapping_add(t1);
            v[3] = v[2];
            v[2] = v[1];
            v[1] = v[0];
            v[0] = t1.wrapping_add(t2);
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    let mut out = [0u8; 32];
    for i in 0..8 {
        out[4 * i..4 * i + 4].copy_from_slice(&h[i].to_be_bytes());
    }
    out
}

/// SHA-1 digest.
pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    let bitlen = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bitlen.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for i in 0..5 {
        out[4 * i..4 * i + 4].copy_from_slice(&h[i].to_be_bytes());
    }
    out
}

/// MD5 digest.
pub fn md5(data: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23,
        4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64).map(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32).collect();
    let mut a0: u32 = 0x67452301;
    let mut b0: u32 = 0xefcdab89;
    let mut c0: u32 = 0x98badcfe;
    let mut d0: u32 = 0x10325476;
    let mut msg = data.to_vec();
    let bitlen = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bitlen.to_le_bytes());
    for chunk in msg.chunks(64) {
        let m: Vec<u32> = (0..16).map(|i| u32::from_le_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]])).collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | (!b & d), i),
                16..=31 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f2 = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f2.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    out
}

/// Hex digest by algorithm name (`SHA-256` (default), `SHA-1`, `MD5`).
pub fn hash_hex(data: &[u8], algorithm: &str) -> Result<String> {
    let a = algorithm.to_ascii_uppercase().replace(['-', '_'], "");
    Ok(match a.as_str() {
        "" | "SHA256" => to_hex(&sha256(data)),
        "SHA1" | "SHA" => to_hex(&sha1(data)),
        "MD5" => to_hex(&md5(data)),
        _ => return Err(CamelError::illegal_argument(format!("Unsupported hash algorithm: {}", algorithm))),
    })
}

// ---------------------------------------------------------------------------
// Dates
// ---------------------------------------------------------------------------

/// Parse a time zone id into an offset in minutes. Supports `UTC`, `GMT`, `Z`,
/// `GMT+2`, `GMT+02:00`, `UTC-5`, `+0100`, `+01:00`. Unknown region ids are treated as UTC.
pub fn parse_timezone(tz: &str) -> i64 {
    let t = tz.trim();
    let rest = t.strip_prefix("GMT").or_else(|| t.strip_prefix("UTC")).unwrap_or(t);
    if rest.is_empty() || rest == "Z" {
        return 0;
    }
    let (sign, digits) = match rest.chars().next() {
        Some('+') => (1, &rest[1..]),
        Some('-') => (-1, &rest[1..]),
        _ => return 0,
    };
    let digits: String = digits.chars().filter(|c| c.is_ascii_digit()).collect();
    let (h, m) = match digits.len() {
        1 | 2 => (digits.parse::<i64>().unwrap_or(0), 0),
        3 => (digits[..1].parse::<i64>().unwrap_or(0), digits[1..].parse::<i64>().unwrap_or(0)),
        4 => (digits[..2].parse::<i64>().unwrap_or(0), digits[2..].parse::<i64>().unwrap_or(0)),
        _ => (0, 0),
    };
    sign * (h * 60 + m)
}

/// Format epoch millis using a Java `SimpleDateFormat` pattern in the time zone given as
/// an offset in minutes (see [`crate::util_time::format_in_zone`] for named zones).
pub fn format_date(millis: i64, pattern: &str, offset_min: i64) -> String {
    let tz = if offset_min == 0 {
        crate::tz::TimeZone::utc()
    } else {
        let a = offset_min.abs();
        crate::tz::TimeZone::fixed(&format!("GMT{}{:02}:{:02}", if offset_min < 0 { '-' } else { '+' }, a / 60, a % 60), (offset_min * 60) as i32)
    };
    crate::util_time::format_in_zone(millis, pattern, &tz)
}

/// Parse a date/time string into epoch millis. Accepts ISO-8601 forms
/// (`yyyy-MM-dd`, `yyyy-MM-ddTHH:mm[:ss[.SSS]][Z|±hh:mm]`, space instead of `T`) and
/// plain integers (epoch millis).
pub fn parse_date(s: &str) -> Option<i64> {
    let t = s.trim();
    if let Ok(n) = t.parse::<i64>() {
        return Some(n);
    }
    let b = t.as_bytes();
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let y: i64 = t[0..4].parse().ok()?;
    let mo: u32 = t[5..7].parse().ok()?;
    let d: u32 = t[8..10].parse().ok()?;
    let (mut h, mut mi, mut sec, mut ms) = (0u32, 0u32, 0u32, 0i64);
    let mut rest = &t[10..];
    if rest.starts_with('T') || rest.starts_with(' ') {
        rest = &rest[1..];
        if rest.len() < 5 {
            return None;
        }
        h = rest[0..2].parse().ok()?;
        mi = rest[3..5].parse().ok()?;
        rest = &rest[5..];
        if rest.starts_with(':') && rest.len() >= 3 {
            sec = rest[1..3].parse().ok()?;
            rest = &rest[3..];
            if rest.starts_with('.') || rest.starts_with(',') {
                let digits: String = rest[1..].chars().take_while(|c| c.is_ascii_digit()).collect();
                let frac = format!("{:0<3}", &digits[..digits.len().min(3)]);
                ms = frac.parse().ok()?;
                rest = &rest[1 + digits.len()..];
            }
        }
    }
    let offset = if rest.is_empty() { 0 } else { parse_timezone(rest) };
    Some((crate::util_time::epoch_from_civil(y, mo, d, h, mi, sec) - offset * 60) * 1000 + ms)
}

/// Convert a value to epoch millis (integers are millis, strings are parsed).
pub fn value_to_millis(v: &Value) -> Option<i64> {
    match v {
        Value::Int(i) => Some(*i),
        Value::Float(f) => Some(*f as i64),
        Value::String(_) | Value::Bytes(_) => parse_date(v.as_str()?),
        _ => None,
    }
}

/// Parse an offset expression like `-1h`, `+30m`, `+1d-2h`, `+500ms` into millis.
pub fn parse_offset(s: &str) -> Option<i64> {
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut total = 0i64;
    while i < b.len() {
        let sign = match b[i] {
            '+' => 1,
            '-' => -1,
            _ => return None,
        };
        i += 1;
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        let n: i64 = b[start..i].iter().collect::<String>().parse().ok()?;
        let ustart = i;
        while i < b.len() && b[i].is_ascii_alphabetic() {
            i += 1;
        }
        let unit: String = b[ustart..i].iter().collect();
        let mult = match unit.as_str() {
            "ms" => 1,
            "s" | "" => 1000,
            "m" => 60_000,
            "h" => 3_600_000,
            "d" => 86_400_000,
            "w" => 7 * 86_400_000,
            _ => return None,
        };
        total += sign * n * mult;
    }
    Some(total)
}

/// Object map from pairs (helper).
pub fn obj(pairs: Vec<(&str, Value)>) -> Value {
    Value::Object(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect::<ObjectMap>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes() {
        assert_eq!(to_hex(&sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(to_hex(&sha256(b"")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(to_hex(&sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(to_hex(&md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(to_hex(&md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
    }

    #[test]
    fn dates() {
        let ms = parse_date("2024-02-29T13:45:10.007Z").unwrap();
        assert_eq!(format_date(ms, "yyyy-MM-dd'T'HH:mm:ss.SSS", 0), "2024-02-29T13:45:10.007");
        assert_eq!(format_date(ms, "EEE, dd MMM yyyy hh:mm a", 0), "Thu, 29 Feb 2024 01:45 PM");
        assert_eq!(format_date(ms, "yyyyMMdd HH:mm XXX", 120), "20240229 15:45 +02:00");
        assert_eq!(format_date(ms, "D", 0), "60");
        assert_eq!(parse_date("2024-02-29T15:45:10.007+02:00"), Some(ms));
        assert_eq!(parse_timezone("GMT-05:30"), -330);
        assert_eq!(parse_offset("+1h-30m"), Some(1_800_000));
        let u = uuid_v4();
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
    }

    #[test]
    fn conversions() {
        assert_eq!(convert_to_type(&Value::from("42"), "java.lang.Integer").unwrap(), Value::Int(42));
        assert_eq!(convert_to_type(&Value::from(3), "Double").unwrap(), Value::Float(3.0));
        assert!(convert_to_type(&Value::from("x"), "int").is_err());
        assert_eq!(convert_to_type(&Value::from("{\"a\":1}"), "Map").unwrap().get("a"), Some(&Value::Int(1)));
        assert!(is_instance_of(&Value::from("x"), "String"));
        assert!(is_instance_of(&Value::from(1), "Number"));
        assert!(!value_predicate(&Value::from("false")));
        assert!(!value_predicate(&Value::from(" FALSE ")));
        assert!(!value_predicate(&Value::Null));
        assert!(!value_predicate(&Value::Bool(false)));
        assert!(!value_predicate(&Value::Array(vec![])));
        assert!(value_predicate(&Value::from(" True")));
        assert!(value_predicate(&Value::from(0)));
        assert!(value_predicate(&Value::from(0.0)));
        assert!(value_predicate(&Value::from("")));
        assert!(value_predicate(&Value::from("no")));
        assert!(value_predicate(&Value::Bytes(vec![])));
        assert!(value_predicate(&Value::Object(Default::default())));
        assert!(value_predicate(&Value::from(vec![0])));
    }
}

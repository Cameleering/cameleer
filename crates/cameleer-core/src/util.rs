//! Utilities: URI parsing, endpoint parameters, durations, pattern matching.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::error::{CamelError, Result};

/// Match a name against a Camel-style pattern:
/// - exact match (case-insensitive)
/// - wildcard: `foo*` (prefix) or `*` (everything)
/// - regular expression (if the pattern compiles and matches the whole name)
pub fn matches_pattern(name: &str, pattern: &str) -> bool {
    if pattern == "*" || name.eq_ignore_ascii_case(pattern) {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        if !prefix.contains('*') && name.to_ascii_lowercase().starts_with(&prefix.to_ascii_lowercase()) {
            return true;
        }
    }
    if pattern.contains('*')
        && wildcard_match(&name.to_ascii_lowercase(), &pattern.to_ascii_lowercase()) {
            return true;
        }
    match crate::regex::Regex::new(pattern) {
        Ok(re) => re.is_full_match(name),
        Err(_) => false,
    }
}

/// Glob-style wildcard match where `*` matches any sequence and `?` any single char.
pub fn wildcard_match(text: &str, pattern: &str) -> bool {
    let t: Vec<char> = text.chars().collect();
    let p: Vec<char> = pattern.chars().collect();
    let (mut ti, mut pi) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            ti += 1;
            pi += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Parse a duration: plain number = millis, or with unit suffix `ms`, `s`, `m`, `h`, `d`,
/// or ISO-ish combos like `1m30s`. Also accepts `PT5S` style.
pub fn parse_duration(s: &str) -> Result<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return Err(CamelError::illegal_argument("Empty duration"));
    }
    if let Ok(ms) = s.parse::<u64>() {
        return Ok(Duration::from_millis(ms));
    }
    let lower = s.to_ascii_lowercase();
    let body = lower.strip_prefix("pt").unwrap_or(&lower);
    let mut total_ms: f64 = 0.0;
    let mut num = String::new();
    let chars: Vec<char> = body.chars().collect();
    let mut i = 0;
    let mut any = false;
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_digit() || c == '.' {
            num.push(c);
            i += 1;
            continue;
        }
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        let mut unit = String::new();
        while i < chars.len() && chars[i].is_ascii_alphabetic() {
            unit.push(chars[i]);
            i += 1;
        }
        let n: f64 = num.parse().map_err(|_| CamelError::illegal_argument(format!("Invalid duration: {}", s)))?;
        num.clear();
        let mult = match unit.as_str() {
            "ms" | "millis" | "milliseconds" => 1.0,
            "s" | "sec" | "secs" | "second" | "seconds" => 1000.0,
            "m" | "min" | "mins" | "minute" | "minutes" => 60_000.0,
            "h" | "hour" | "hours" => 3_600_000.0,
            "d" | "day" | "days" => 86_400_000.0,
            _ => return Err(CamelError::illegal_argument(format!("Invalid duration unit '{}' in: {}", unit, s))),
        };
        total_ms += n * mult;
        any = true;
    }
    if !num.is_empty() {
        // trailing number without unit -> millis
        total_ms += num.parse::<f64>().map_err(|_| CamelError::illegal_argument(format!("Invalid duration: {}", s)))?;
        any = true;
    }
    if !any {
        return Err(CamelError::illegal_argument(format!("Invalid duration: {}", s)));
    }
    Ok(Duration::from_millis(total_ms as u64))
}

/// Percent-decode a URI component (`+` is decoded as space).
pub fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let h = std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
                match h {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Encode like `java.net.URLEncoder` (UTF-8): alphanumerics and `.-*_` are kept, a space
/// becomes `+`, everything else is percent-encoded.
pub fn form_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'*' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// Percent-encode a URI component.
pub fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// Parse a query string `a=1&b=2` into a map. Supports Camel's `RAW(...)` values.
pub fn parse_query(q: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    if q.is_empty() {
        return map;
    }
    // split on '&' but not inside RAW(...)
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut depth = 0;
    let chars: Vec<char> = q.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if depth == 0 && c == '&' {
            parts.push(std::mem::take(&mut cur));
        } else {
            if c == '(' && cur.ends_with("RAW") {
                depth += 1;
            } else if c == ')' && depth > 0 {
                depth -= 1;
            }
            cur.push(c);
        }
        i += 1;
    }
    if !cur.is_empty() {
        parts.push(cur);
    }
    for p in parts {
        if p.is_empty() {
            continue;
        }
        let (k, v) = match p.find('=') {
            Some(i) => (&p[..i], &p[i + 1..]),
            None => (p.as_str(), ""),
        };
        let v = if let Some(raw) = v.strip_prefix("RAW(").and_then(|r| r.strip_suffix(')')) {
            raw.to_string()
        } else if let Some(raw) = v.strip_prefix("RAW{").and_then(|r| r.strip_suffix('}')) {
            raw.to_string()
        } else {
            url_decode(v)
        };
        map.insert(url_decode(k), v);
    }
    map
}

/// A parsed endpoint URI: `scheme:path?query`.
#[derive(Clone, Debug, PartialEq)]
pub struct EndpointUri {
    pub scheme: String,
    /// Remaining part after `scheme:` (and after `//` if present), without the query.
    pub path: String,
    pub params: BTreeMap<String, String>,
}

impl EndpointUri {
    pub fn parse(uri: &str) -> Result<EndpointUri> {
        let uri = uri.trim();
        let colon = uri
            .find(':')
            .ok_or_else(|| CamelError::resolve_endpoint_failed(uri, "URI must be in the form scheme:path"))?;
        let scheme = uri[..colon].to_string();
        if scheme.is_empty() {
            return Err(CamelError::resolve_endpoint_failed(uri, "Missing scheme"));
        }
        let mut rest = &uri[colon + 1..];
        if let Some(r) = rest.strip_prefix("//") {
            rest = r;
        }
        let (path, query) = match find_query_start(rest) {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, ""),
        };
        Ok(EndpointUri { scheme, path: path.to_string(), params: parse_query(query) })
    }

    /// Canonical string form, like Camel's `URISupport.normalizeUri`:
    /// `scheme://path?sorted-params`, with keys and values encoded like `java.net.URLEncoder`
    /// (`mock:result` -> `mock://result`, `seda:foo?size=10&blockWhenFull=true` ->
    /// `seda://foo?blockWhenFull=true&size=10`). Endpoint uris (`Endpoint::uri`) and the
    /// endpoint properties (`CamelToEndpoint`...) use this form.
    pub fn normalized(&self) -> String {
        let mut s = format!("{}://{}", self.scheme, self.path);
        if !self.params.is_empty() {
            s.push('?');
            let q: Vec<String> = self.params.iter().map(|(k, v)| format!("{}={}", form_encode(k), form_encode(v))).collect();
            s.push_str(&q.join("&"));
        }
        s
    }
}

/// Find the `?` that starts the query string, ignoring `?` inside RAW(...).
fn find_query_start(s: &str) -> Option<usize> {
    s.find('?')
}

/// Normalize an endpoint uri like Camel's `URISupport.normalizeUri` (see
/// [`EndpointUri::normalized`]) so that equivalent uris map to the same endpoint.
pub fn normalize_uri(uri: &str) -> Result<String> {
    Ok(EndpointUri::parse(uri)?.normalized())
}

/// Endpoint parameters (query options). Components consume the options they
/// understand with the `take_*` methods; any leftovers are reported as errors.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Parameters {
    map: BTreeMap<String, String>,
}

impl Parameters {
    pub fn new(map: BTreeMap<String, String>) -> Self {
        Parameters { map }
    }
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
    pub fn get(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(|s| s.as_str())
    }
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.map.insert(key.into(), value.into());
    }
    pub fn take(&mut self, key: &str) -> Option<String> {
        self.map.remove(key)
    }
    pub fn take_string(&mut self, key: &str, default: &str) -> String {
        self.take(key).unwrap_or_else(|| default.to_string())
    }
    pub fn take_bool(&mut self, key: &str, default: bool) -> Result<bool> {
        match self.take(key) {
            None => Ok(default),
            Some(v) => match v.to_ascii_lowercase().as_str() {
                "true" | "" => Ok(true),
                "false" => Ok(false),
                _ => Err(CamelError::illegal_argument(format!("Invalid boolean value for option {}: {}", key, v))),
            },
        }
    }
    pub fn take_i64(&mut self, key: &str, default: i64) -> Result<i64> {
        match self.take(key) {
            None => Ok(default),
            Some(v) => v
                .trim()
                .parse()
                .map_err(|_| CamelError::illegal_argument(format!("Invalid number value for option {}: {}", key, v))),
        }
    }
    pub fn take_u64(&mut self, key: &str, default: u64) -> Result<u64> {
        match self.take(key) {
            None => Ok(default),
            Some(v) => v
                .trim()
                .parse()
                .map_err(|_| CamelError::illegal_argument(format!("Invalid number value for option {}: {}", key, v))),
        }
    }
    pub fn take_usize(&mut self, key: &str, default: usize) -> Result<usize> {
        Ok(self.take_u64(key, default as u64)? as usize)
    }
    pub fn take_f64(&mut self, key: &str, default: f64) -> Result<f64> {
        match self.take(key) {
            None => Ok(default),
            Some(v) => v
                .trim()
                .parse()
                .map_err(|_| CamelError::illegal_argument(format!("Invalid number value for option {}: {}", key, v))),
        }
    }
    /// Take a duration option (millis or with unit suffix).
    pub fn take_duration(&mut self, key: &str, default: Duration) -> Result<Duration> {
        match self.take(key) {
            None => Ok(default),
            Some(v) => parse_duration(&v),
        }
    }
    /// Take all options with the given prefix (prefix stripped), e.g. `headers.` or `scheduler.`.
    pub fn take_prefixed(&mut self, prefix: &str) -> BTreeMap<String, String> {
        let keys: Vec<String> = self.map.keys().filter(|k| k.starts_with(prefix)).cloned().collect();
        keys.into_iter().map(|k| (k[prefix.len()..].to_string(), self.map.remove(&k).unwrap())).collect()
    }
    /// Take all remaining parameters.
    pub fn take_all(&mut self) -> BTreeMap<String, String> {
        std::mem::take(&mut self.map)
    }
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.map.keys()
    }
    pub fn as_map(&self) -> &BTreeMap<String, String> {
        &self.map
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_parsing() {
        let u = EndpointUri::parse("timer://foo?period=1000&delay=5").unwrap();
        assert_eq!(u.scheme, "timer");
        assert_eq!(u.path, "foo");
        assert_eq!(u.params.get("period").unwrap(), "1000");
        assert_eq!(normalize_uri("timer:foo?period=1000&delay=5").unwrap(), normalize_uri("timer://foo?delay=5&period=1000").unwrap());
        let u = EndpointUri::parse("http:host?q=RAW(a&b+c)").unwrap();
        assert_eq!(u.params.get("q").unwrap(), "a&b+c");
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("1000").unwrap(), Duration::from_millis(1000));
        assert_eq!(parse_duration("5s").unwrap(), Duration::from_secs(5));
        assert_eq!(parse_duration("1m30s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("PT2S").unwrap(), Duration::from_secs(2));
        assert_eq!(parse_duration("250ms").unwrap(), Duration::from_millis(250));
    }

    #[test]
    fn patterns() {
        assert!(matches_pattern("CamelFileName", "Camel*"));
        assert!(matches_pattern("foo", "FOO"));
        assert!(!matches_pattern("bar", "foo*"));
        assert!(wildcard_match("abcdef", "a*d?f"));
    }
}

//! Shared helpers: hashing (SHA-256, HMAC, MD5), encodings (hex, base64, percent),
//! timestamps, small XML/JSON helpers, endpoint option lookup and exception kinds.

use std::sync::Once;
use std::time::{SystemTime, UNIX_EPOCH};

use camel_core::context::CamelContext;
use camel_core::error::{CamelError, Result};
use camel_core::language::xml::{local_name, Document, NodeKind};
use camel_core::message::Exchange;
use camel_core::util::Parameters;
use camel_core::util_time::{civil_from_epoch, epoch_from_civil};
use camel_core::value::{ObjectMap, Value};
use camel_tls::crypto::hash::{hmac, Digest, Sha256};

// ---------------------------------------------------------------------------
// exception kinds
// ---------------------------------------------------------------------------

/// Exception kinds raised by the cloud components (Java SDK exception class names).
pub mod kinds {
    /// Root of all cloud SDK failures (`software.amazon.awssdk.core.exception.SdkException`).
    pub const SDK: &str = "SdkException";
    /// Any AWS service error response (`AwsServiceException`).
    pub const AWS_SERVICE: &str = "AwsServiceException";
    pub const S3: &str = "S3Exception";
    pub const SQS: &str = "SqsException";
    pub const SNS: &str = "SnsException";
    pub const DYNAMODB: &str = "DynamoDbException";
    pub const LAMBDA: &str = "LambdaException";
    pub const KINESIS: &str = "KinesisException";
    pub const FIREHOSE: &str = "FirehoseException";
    pub const EVENTBRIDGE: &str = "EventBridgeException";
    pub const SECRETS_MANAGER: &str = "SecretsManagerException";
    pub const SES: &str = "SesException";
    pub const CLOUDWATCH: &str = "CloudWatchException";
    pub const STS: &str = "StsException";
    pub const SFN: &str = "SfnException";
    pub const IAM: &str = "IamException";
    pub const EC2: &str = "Ec2Exception";
    /// Client-side configuration / credential problems (`SdkClientException`).
    pub const SDK_CLIENT: &str = "SdkClientException";
    /// Azure `com.azure.core.exception.HttpResponseException` family.
    pub const AZURE_HTTP: &str = "HttpResponseException";
    pub const BLOB_STORAGE: &str = "BlobStorageException";
    pub const QUEUE_STORAGE: &str = "QueueStorageException";
    pub const DATALAKE_STORAGE: &str = "DataLakeStorageException";
    /// Key Vault errors (`com.azure.core.exception.ResourceNotFoundException` etc. map here).
    pub const KEY_VAULT: &str = "KeyVaultErrorException";
    pub const COSMOS: &str = "CosmosException";
    /// Azure AD token acquisition failure (`com.azure.core.exception.ClientAuthenticationException`).
    pub const AZURE_AUTH: &str = "ClientAuthenticationException";
    /// Google `com.google.cloud.BaseServiceException` family.
    pub const GOOGLE_SERVICE: &str = "BaseServiceException";
    pub const GOOGLE_STORAGE: &str = "StorageException";
    pub const GOOGLE_API: &str = "ApiException";
    pub const BIGQUERY: &str = "BigQueryException";
    pub const FIRESTORE: &str = "FirestoreException";
    /// Google API client errors (`com.google.api.client.googleapis.json.GoogleJsonResponseException`).
    pub const GOOGLE_JSON: &str = "GoogleJsonResponseException";
    /// OAuth2 token exchange failure.
    pub const GOOGLE_AUTH: &str = "GoogleAuthException";
}

/// Register the exception hierarchy once (so `onException(AwsServiceException)` catches `S3Exception`).
pub fn ensure_kinds() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        use camel_core::error::register_kind_parent as p;
        p(kinds::SDK, camel_core::error::kinds::RUNTIME);
        p(kinds::SDK_CLIENT, kinds::SDK);
        p(kinds::AWS_SERVICE, kinds::SDK);
        for k in [
            kinds::S3,
            kinds::SQS,
            kinds::SNS,
            kinds::DYNAMODB,
            kinds::LAMBDA,
            kinds::KINESIS,
            kinds::FIREHOSE,
            kinds::EVENTBRIDGE,
            kinds::SECRETS_MANAGER,
            kinds::SES,
            kinds::CLOUDWATCH,
            kinds::STS,
            kinds::SFN,
            kinds::IAM,
            kinds::EC2,
        ] {
            p(k, kinds::AWS_SERVICE);
        }
        p(kinds::AZURE_HTTP, camel_core::error::kinds::RUNTIME);
        p(kinds::BLOB_STORAGE, kinds::AZURE_HTTP);
        p(kinds::QUEUE_STORAGE, kinds::AZURE_HTTP);
        p(kinds::DATALAKE_STORAGE, kinds::AZURE_HTTP);
        p(kinds::KEY_VAULT, kinds::AZURE_HTTP);
        p(kinds::COSMOS, kinds::AZURE_HTTP);
        p(kinds::AZURE_AUTH, kinds::AZURE_HTTP);
        p(kinds::GOOGLE_SERVICE, camel_core::error::kinds::RUNTIME);
        p(kinds::GOOGLE_STORAGE, kinds::GOOGLE_SERVICE);
        p(kinds::GOOGLE_API, kinds::GOOGLE_SERVICE);
        p(kinds::BIGQUERY, kinds::GOOGLE_SERVICE);
        p(kinds::FIRESTORE, kinds::GOOGLE_SERVICE);
        p(kinds::GOOGLE_JSON, kinds::GOOGLE_SERVICE);
        p(kinds::GOOGLE_AUTH, camel_core::error::kinds::IO);
    });
}

/// Build an error of a cloud kind (ensuring the hierarchy is registered).
pub fn cloud_error(kind: &str, message: impl Into<String>) -> CamelError {
    ensure_kinds();
    CamelError::new(kind, message)
}

// ---------------------------------------------------------------------------
// hashing and encodings
// ---------------------------------------------------------------------------

/// Lower-case hex.
pub fn hex(data: &[u8]) -> String {
    camel_tls::util::hex(data)
}

/// SHA-256 digest.
pub fn sha256(data: &[u8]) -> Vec<u8> {
    Sha256::digest(data)
}

/// Hex SHA-256 digest.
pub fn sha256_hex(data: &[u8]) -> String {
    hex(&sha256(data))
}

/// HMAC-SHA256.
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    hmac::<Sha256>(key, data)
}

/// Constant-time comparison.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    camel_tls::util::ct_eq(a, b)
}

/// MD5 (RFC 1321) - used for S3 ETags and SQS body digests only, never for security.
pub fn md5(data: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4,
        11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64).map(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32).collect();
    let (mut a0, mut b0, mut c0, mut d0) = (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64).wrapping_mul(8)).to_le_bytes());
    for chunk in msg.chunks(64) {
        let m: Vec<u32> = chunk.chunks(4).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]])).collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
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
    for (i, w) in [a0, b0, c0, d0].iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
    }
    out
}

/// Hex MD5 digest.
pub fn md5_hex(data: &[u8]) -> String {
    hex(&md5(data))
}

/// Standard base64 with padding.
pub fn base64_encode(data: &[u8]) -> String {
    camel_tls::pem::base64_encode(data)
}

/// Standard base64 decoding (whitespace ignored); also accepts the URL-safe alphabet.
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let normalized: String = s.chars().map(|c| match c {
        '-' => '+',
        '_' => '/',
        c => c,
    }).collect();
    camel_tls::pem::base64_decode(&normalized)
}

/// URL-safe base64 without padding (JWT).
pub fn base64url_encode(data: &[u8]) -> String {
    base64_encode(data).trim_end_matches('=').replace('+', "-").replace('/', "_")
}

/// Percent-encode everything except the RFC 3986 unreserved characters
/// (`A-Z a-z 0-9 - _ . ~`); `/` is kept when `keep_slash`.
pub fn uri_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b'/' if keep_slash => out.push('/'),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// Percent-decode (`+` is kept literally unless `plus_as_space`).
pub fn percent_decode(s: &str, plus_as_space: bool) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                        continue;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b'+' if plus_as_space => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Encode `k=v&...` with [`uri_encode`] (form bodies and query strings).
pub fn encode_form(pairs: &[(String, String)]) -> String {
    pairs.iter().map(|(k, v)| format!("{}={}", uri_encode(k, false), uri_encode(v, false))).collect::<Vec<_>>().join("&")
}

/// Parse a raw query / form body into decoded pairs (order preserved).
pub fn parse_form(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (percent_decode(k, true), percent_decode(v, true)),
            None => (percent_decode(p, true), String::new()),
        })
        .collect()
}

/// Random lower-case hex string of `n` bytes.
pub fn random_hex(n: usize) -> String {
    hex(&camel_tls::crypto::rng::random_bytes(n))
}

/// Random UUID (version 4) string.
pub fn uuid() -> String {
    let mut b = camel_tls::crypto::rng::random_bytes(16);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex(&b);
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

// ---------------------------------------------------------------------------
// time
// ---------------------------------------------------------------------------

/// Seconds since the epoch.
pub fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Milliseconds since the epoch.
pub fn now_millis() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// `YYYYMMDDTHHMMSSZ` (SigV4 `X-Amz-Date`).
pub fn amz_date(secs: i64) -> String {
    let (y, mo, d, h, mi, s) = civil_from_epoch(secs);
    format!("{:04}{:02}{:02}T{:02}{:02}{:02}Z", y, mo, d, h, mi, s)
}

/// RFC 1123 date (`Fri, 24 May 2013 00:00:00 GMT`).
pub fn http_date(secs: i64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let (y, mo, d, h, mi, s) = civil_from_epoch(secs);
    let wd = DAYS[(secs.div_euclid(86400)).rem_euclid(7) as usize];
    format!("{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT", wd, d, MONTHS[(mo - 1) as usize], y, h, mi, s)
}

/// ISO 8601 UTC with milliseconds (`2013-05-24T00:00:00.000Z`).
pub fn iso8601_millis(millis: i64) -> String {
    let (y, mo, d, h, mi, s) = civil_from_epoch(millis.div_euclid(1000));
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", y, mo, d, h, mi, s, millis.rem_euclid(1000))
}

/// ISO 8601 UTC without fraction (`2013-05-24T00:00:00Z`).
pub fn iso8601(secs: i64) -> String {
    let (y, mo, d, h, mi, s) = civil_from_epoch(secs);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d, h, mi, s)
}

/// Parse `YYYY-MM-DDTHH:MM:SS[.fff]Z` (or `YYYYMMDDTHHMMSSZ`) into epoch seconds.
pub fn parse_iso8601(s: &str) -> Option<i64> {
    let digits: String = s.chars().take_while(|c| *c != '.' && *c != 'Z' && *c != '+').filter(|c| c.is_ascii_digit()).collect();
    if digits.len() < 14 {
        if digits.len() == 8 {
            return Some(epoch_from_civil(digits[0..4].parse().ok()?, digits[4..6].parse().ok()?, digits[6..8].parse().ok()?, 0, 0, 0));
        }
        return None;
    }
    let n = |a: usize, b: usize| digits[a..b].parse::<u32>().ok();
    Some(epoch_from_civil(digits[0..4].parse().ok()?, n(4, 6)?, n(6, 8)?, n(8, 10)?, n(10, 12)?, n(12, 14)?))
}

/// Parse an RFC 1123 date into epoch seconds.
pub fn parse_http_date(s: &str) -> Option<i64> {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 5 {
        return None;
    }
    let d: u32 = parts[1].parse().ok()?;
    let mo = MONTHS.iter().position(|m| *m == parts[2])? as u32 + 1;
    let y: i64 = parts[3].parse().ok()?;
    let t: Vec<u32> = parts[4].split(':').filter_map(|x| x.parse().ok()).collect();
    if t.len() != 3 {
        return None;
    }
    Some(epoch_from_civil(y, mo, d, t[0], t[1], t[2]))
}

// ---------------------------------------------------------------------------
// XML
// ---------------------------------------------------------------------------

/// Escape text for XML content/attributes.
pub fn xml_escape(s: &str) -> String {
    camel_core::language::xml::escape_attr(s)
}

/// A parsed XML document with namespace-agnostic (local name) navigation helpers.
pub struct Xml {
    pub doc: Document,
}

impl Xml {
    pub fn parse(text: &str) -> Result<Xml> {
        Ok(Xml { doc: Document::parse(text)? })
    }
    /// Root element index.
    pub fn root(&self) -> usize {
        self.doc.root_element().unwrap_or(0)
    }
    /// Local name of an element.
    pub fn name(&self, i: usize) -> &str {
        local_name(self.doc.name(i))
    }
    /// Child elements with the given local name.
    pub fn children(&self, i: usize, name: &str) -> Vec<usize> {
        self.doc.element_children(i).into_iter().filter(|&c| local_name(self.doc.name(c)) == name).collect()
    }
    /// First child element with the given local name.
    pub fn child(&self, i: usize, name: &str) -> Option<usize> {
        self.children(i, name).into_iter().next()
    }
    /// Text of the first child element with the given local name.
    pub fn child_text(&self, i: usize, name: &str) -> Option<String> {
        self.child(i, name).map(|c| self.doc.text_content(c))
    }
    /// Text content of an element.
    pub fn text(&self, i: usize) -> String {
        self.doc.text_content(i)
    }
    /// All descendant elements (document order) with the given local name.
    pub fn descendants(&self, name: &str) -> Vec<usize> {
        (0..self.doc.nodes.len())
            .filter(|&i| matches!(&self.doc.nodes[i].kind, NodeKind::Element { name: n, .. } if local_name(n) == name))
            .collect()
    }
    /// Text of the first descendant with the given local name.
    pub fn find_text(&self, name: &str) -> Option<String> {
        self.descendants(name).into_iter().next().map(|i| self.text(i))
    }
}

// ---------------------------------------------------------------------------
// endpoint options
// ---------------------------------------------------------------------------

/// Endpoint option lookup: the uri parameter first, then the component level property
/// `camel.component.<scheme>.<option>`.
pub struct Opts<'a> {
    pub params: &'a mut Parameters,
    ctx: &'a CamelContext,
    scheme: String,
}

fn strip_raw(v: String) -> String {
    if let Some(r) = v.strip_prefix("RAW(").and_then(|r| r.strip_suffix(')')) {
        return r.to_string();
    }
    if let Some(r) = v.strip_prefix("RAW{").and_then(|r| r.strip_suffix('}')) {
        return r.to_string();
    }
    v
}

impl<'a> Opts<'a> {
    pub fn new(params: &'a mut Parameters, ctx: &'a CamelContext, scheme: &str) -> Self {
        Opts { params, ctx, scheme: scheme.to_string() }
    }
    /// Option value (uri, then component property). Empty strings count as absent.
    pub fn get(&mut self, key: &str) -> Option<String> {
        self.params
            .take(key)
            .or_else(|| self.ctx.properties().get(&format!("camel.component.{}.{}", self.scheme, key)))
            .map(strip_raw)
            .filter(|s| !s.is_empty())
    }
    pub fn string(&mut self, key: &str, default: &str) -> String {
        self.get(key).unwrap_or_else(|| default.to_string())
    }
    pub fn bool(&mut self, key: &str, default: bool) -> Result<bool> {
        match self.get(key) {
            None => Ok(default),
            Some(v) => match v.to_ascii_lowercase().as_str() {
                "true" => Ok(true),
                "false" => Ok(false),
                _ => Err(CamelError::illegal_argument(format!("Invalid boolean value for option {}: {}", key, v))),
            },
        }
    }
    pub fn i64(&mut self, key: &str, default: i64) -> Result<i64> {
        match self.get(key) {
            None => Ok(default),
            Some(v) => v.trim().parse().map_err(|_| CamelError::illegal_argument(format!("Invalid number for option {}: {}", key, v))),
        }
    }
    /// Remove options that are accepted but have no effect.
    pub fn ignore(&mut self, keys: &[&str]) {
        for k in keys {
            self.params.take(k);
        }
    }
    pub fn context(&self) -> &CamelContext {
        self.ctx
    }
}

// ---------------------------------------------------------------------------
// exchange helpers
// ---------------------------------------------------------------------------

/// Non-null header rendered as a string.
pub fn header_str(ex: &Exchange, name: &str) -> Option<String> {
    ex.header(name).filter(|v| !v.is_null()).map(|v| v.to_string()).filter(|s| !s.is_empty())
}

/// Headers that are user (non `Camel*`) scalars, for message attributes.
pub fn user_headers(ex: &Exchange) -> Vec<(String, Value)> {
    ex.message
        .headers
        .iter()
        .filter(|(k, v)| !k.to_ascii_lowercase().starts_with("camel") && matches!(v, Value::String(_) | Value::Int(_) | Value::Float(_) | Value::Bool(_)))
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

/// A list from a value: an array's elements, or a comma separated string.
pub fn str_list(v: &Value) -> Vec<String> {
    match v {
        Value::Array(a) => a.iter().map(|x| x.to_string()).filter(|x| !x.is_empty()).collect(),
        Value::Null => Vec::new(),
        other => other.to_string().split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect(),
    }
}

/// A header as a list (array or comma separated string); empty when absent.
pub fn header_list(ex: &Exchange, name: &str) -> Vec<String> {
    ex.header(name).map(str_list).unwrap_or_default()
}

/// An optional numeric endpoint option.
pub fn opt_i64(o: &mut Opts, key: &str) -> Result<Option<i64>> {
    o.get(key).map(|v| v.trim().parse::<i64>().map_err(|_| CamelError::illegal_argument(format!("Invalid number for option {}: {}", key, v)))).transpose()
}

/// Header as i64 (number or numeric string).
pub fn header_i64(ex: &Exchange, name: &str) -> Option<i64> {
    ex.header(name).and_then(|v| v.as_i64().or_else(|| v.to_string().trim().parse().ok()))
}

/// Build an object value from string pairs.
pub fn string_map(pairs: impl IntoIterator<Item = (String, String)>) -> Value {
    Value::Object(pairs.into_iter().map(|(k, v)| (k, Value::String(v))).collect::<ObjectMap>())
}

/// JSON object from pairs.
pub fn obj<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    Value::Object(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

/// Parse a JSON body (empty body = empty object).
pub fn parse_json(body: &[u8]) -> Result<Value> {
    let text = String::from_utf8_lossy(body);
    if text.trim().is_empty() {
        return Ok(Value::Object(ObjectMap::new()));
    }
    camel_core::json::parse(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_known_answers() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"The quick brown fox jumps over the lazy dog"), "9e107d9d372bb6826bd81d3542a419d6");
    }

    #[test]
    fn dates() {
        // 2013-05-24T00:00:00Z
        let t = epoch_from_civil(2013, 5, 24, 0, 0, 0);
        assert_eq!(amz_date(t), "20130524T000000Z");
        assert_eq!(http_date(t), "Fri, 24 May 2013 00:00:00 GMT");
        assert_eq!(parse_http_date("Fri, 24 May 2013 00:00:00 GMT"), Some(t));
        assert_eq!(parse_iso8601("2013-05-24T00:00:00.000Z"), Some(t));
        assert_eq!(parse_iso8601("20130524T000000Z"), Some(t));
        assert_eq!(iso8601_millis(t * 1000 + 5), "2013-05-24T00:00:00.005Z");
    }

    #[test]
    fn encodings() {
        assert_eq!(uri_encode("a b/c~$", true), "a%20b/c~%24");
        assert_eq!(uri_encode("a b/c", false), "a%20b%2Fc");
        assert_eq!(percent_decode("a%20b+c%2", true), "a b c%2");
        assert_eq!(base64url_encode(&[0xfb, 0xff]), "-_8");
        assert_eq!(base64_decode("-_8"), Some(vec![0xfb, 0xff]));
        assert_eq!(uuid().len(), 36);
    }
}

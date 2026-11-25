//! An embedded Alibaba Cloud Simple Log Service emulator ([`SlsMock`]).
//!
//! It **verifies every request**: the `Date` header (within 15 minutes), `x-log-apiversion`,
//! `Content-MD5` against the body, `x-log-bodyrawsize` against the decompressed body, and the
//! signature, V1 (`LOG ak:sig`, HMAC-SHA1) or V4 (`SLS4-HMAC-SHA256`, with
//! `x-log-content-sha256`), recomputed with the secret of the AccessKey. STS tokens
//! ([`SlsMock::add_sts_token`]) must come with their temporary AccessKey. The project is the first
//! label of the `Host` header, as on the service.
//!
//! Served: `PutLogs` (`POST /logstores/{name}/shards/lb`, protobuf `LogGroup`, `lz4` or `deflate`
//! or uncompressed; not `zstd`), `GetLogs` (`POST /logstores/{name}/logs` as in the 2020-12-30 API,
//! and the classic `GET /logstores/{name}?type=log`, with a small query language: `*`,
//! words, `key: value`, `and` / `or` / `not`) and `ListLogStores` (`GET /logstores`), plus
//! `POST /logstores` (CreateLogstore) for clients that create their Logstores first. Errors are
//! the service's JSON (`{"errorCode", "errorMessage"}`) with `x-log-requestid`.

use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex};

use cameleer_core::dataformat::codecs::lz4_decompress;
use cameleer_core::dataformat::compress::zlib_decompress;
use cameleer_core::value::{ObjectMap, Value};

use super::{Handler, MockServer};
use crate::alibaba::sls_proto::LogGroup;
use crate::alibaba::sls_sign::{authorization_v1, content_md5, signature_v4};
use crate::http::{Response, ServerRequest};
use crate::util::{ct_eq, now_secs, parse_form, parse_http_date, parse_iso8601, percent_decode, random_hex, sha256_hex};

/// The AccessKey ID the mock accepts.
pub const ACCESS_KEY_ID: &str = "LTAI5tCameleerTestKey";
/// Its secret.
pub const ACCESS_KEY_SECRET: &str = "cameleer-sls-test-secret";
/// The region of [`ENDPOINT`].
pub const REGION: &str = "cn-hangzhou";
/// The service endpoint named in the `Host` header.
pub const ENDPOINT: &str = "cn-hangzhou.log.aliyuncs.com";

/// One stored log.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredLog {
    pub time: u32,
    pub topic: String,
    pub source: String,
    pub tags: Vec<(String, String)>,
    pub contents: Vec<(String, String)>,
}

/// A Logstore.
#[derive(Clone, Debug, Default)]
pub struct LogStore {
    pub mode: String,
    pub telemetry_type: String,
    pub logs: Vec<StoredLog>,
}

#[derive(Default)]
struct State {
    keys: BTreeMap<String, String>,
    /// STS token -> its AccessKey ID.
    tokens: BTreeMap<String, String>,
    projects: BTreeMap<String, BTreeMap<String, LogStore>>,
    /// Every accepted request: (method, path, signature version, compress type).
    requests: Vec<(String, String, String, String)>,
}

/// The Simple Log Service emulator.
pub struct SlsMock {
    server: MockServer,
    state: Arc<Mutex<State>>,
}

fn error(status: u16, code: &str, message: &str) -> Response {
    let body = Value::Object(ObjectMap::from_iter([("errorCode".to_string(), Value::from(code)), ("errorMessage".to_string(), Value::from(message))]));
    Response::json(status, body.to_json()).with_header("x-log-requestid", random_hex(12).to_ascii_uppercase())
}

fn ok(body: String) -> Response {
    Response::json(200, body).with_header("x-log-requestid", random_hex(12).to_ascii_uppercase())
}

impl SlsMock {
    /// Start on plain HTTP.
    pub fn start() -> io::Result<SlsMock> {
        Self::start_with(false)
    }

    /// Start on HTTPS with a generated self-signed certificate ([`SlsMock::cert_pem`]).
    pub fn start_tls() -> io::Result<SlsMock> {
        Self::start_with(true)
    }

    fn start_with(tls: bool) -> io::Result<SlsMock> {
        let mut st = State::default();
        st.keys.insert(ACCESS_KEY_ID.into(), ACCESS_KEY_SECRET.into());
        let state = Arc::new(Mutex::new(st));
        let s = state.clone();
        let handler: Handler = Arc::new(move |req: &ServerRequest| handle(&s, req));
        let server = if tls { MockServer::start_tls(handler)? } else { MockServer::start(handler)? };
        Ok(SlsMock { server, state })
    }

    /// The base URL (`http://127.0.0.1:port`, or `https://localhost:port`).
    pub fn url(&self) -> String {
        self.server.url()
    }

    /// The certificate of a TLS mock.
    pub fn cert_pem(&self) -> Option<String> {
        self.server.cert_pem.clone()
    }

    /// Endpoint options pointing the component at this mock, with the test AccessKey.
    pub fn endpoint_options(&self) -> String {
        format!("endpoint={}&accessKey=RAW({})&secretKey=RAW({})&uriEndpointOverride={}&overrideEndpoint=true", ENDPOINT, ACCESS_KEY_ID, ACCESS_KEY_SECRET, self.url())
    }

    /// Accept another AccessKey pair.
    pub fn add_access_key(&self, id: &str, secret: &str) {
        self.state.lock().unwrap().keys.insert(id.into(), secret.into());
    }

    /// Accept an STS token for a temporary AccessKey pair.
    pub fn add_sts_token(&self, id: &str, secret: &str, token: &str) {
        let mut st = self.state.lock().unwrap();
        st.keys.insert(id.into(), secret.into());
        st.tokens.insert(token.into(), id.into());
    }

    /// Create a project with standard log Logstores.
    pub fn create_project(&self, project: &str, log_stores: &[&str]) {
        for s in log_stores {
            self.create_log_store(project, s, "standard", "None");
        }
        self.state.lock().unwrap().projects.entry(project.into()).or_default();
    }

    /// Create a Logstore.
    pub fn create_log_store(&self, project: &str, name: &str, mode: &str, telemetry_type: &str) {
        self.state.lock().unwrap().projects.entry(project.into()).or_default().insert(name.into(), LogStore { mode: mode.into(), telemetry_type: telemetry_type.into(), logs: Vec::new() });
    }

    /// Store logs directly (for queries).
    pub fn add_logs(&self, project: &str, log_store: &str, logs: Vec<StoredLog>) {
        self.state.lock().unwrap().projects.entry(project.into()).or_default().entry(log_store.into()).or_default().logs.extend(logs);
    }

    /// The logs of a Logstore.
    pub fn logs(&self, project: &str, log_store: &str) -> Vec<StoredLog> {
        self.state.lock().unwrap().projects.get(project).and_then(|p| p.get(log_store)).map(|s| s.logs.clone()).unwrap_or_default()
    }

    /// Every accepted request: (method, path, signature version `v1` / `v4`, compress type).
    pub fn requests(&self) -> Vec<(String, String, String, String)> {
        self.state.lock().unwrap().requests.clone()
    }
}

/// Check the authentication of a request; the signature version on success.
fn authenticate(st: &State, req: &ServerRequest, path: &str, query: &[(String, String)]) -> Result<&'static str, Response> {
    if req.header("x-log-apiversion") != Some("0.6.0") {
        return Err(error(400, "InvalidAPIVersion", "x-log-apiversion must be 0.6.0"));
    }
    let md5 = req.header("Content-MD5");
    if !req.body.is_empty() && md5.map(|m| m != content_md5(&req.body)).unwrap_or(true) {
        return Err(error(400, "InvalidContentMD5", "Content-MD5 does not match the body"));
    }
    let auth = req.header("Authorization").ok_or_else(|| error(401, "Unauthorized", "the Authorization header is missing"))?;
    let headers: Vec<(String, String)> = req.headers.clone();
    let now = now_secs();
    let fresh = |t: Option<i64>| t.map(|t| (now - t).abs() <= 900).unwrap_or(false);
    let (version, key_id, given) = if let Some(rest) = auth.strip_prefix("LOG ") {
        if !fresh(req.header("Date").and_then(parse_http_date)) {
            return Err(error(400, "RequestTimeTooSkewed", "the Date header is missing or more than 15 minutes off"));
        }
        if !matches!(req.header("x-log-signaturemethod"), Some("hmac-sha1") | Some("hmac-sha256")) {
            return Err(error(400, "InvalidSignatureMethod", "x-log-signaturemethod must be hmac-sha1 or hmac-sha256"));
        }
        let (id, _) = rest.split_once(':').ok_or_else(|| error(401, "Unauthorized", "malformed Authorization"))?;
        ("v1", id.to_string(), auth.to_string())
    } else if let Some(rest) = auth.strip_prefix("SLS4-HMAC-SHA256 ") {
        if !fresh(req.header("x-log-date").and_then(parse_iso8601)) {
            return Err(error(400, "RequestTimeTooSkewed", "x-log-date is missing or more than 15 minutes off"));
        }
        if req.header("x-log-content-sha256") != Some(sha256_hex(&req.body).as_str()) {
            return Err(error(400, "InvalidContentSha256", "x-log-content-sha256 does not match the body"));
        }
        // the SDKs' `Credential=..,Signature=..` and the documented `Credential=.., SignedHeaders=.., Signature=..`
        let field = |name: &str| rest.split(',').find_map(|p| p.trim().strip_prefix(name)).map(|v| v.trim().to_string());
        let cred = field("Credential=").ok_or_else(|| error(401, "Unauthorized", "malformed Authorization"))?;
        let sig = field("Signature=").ok_or_else(|| error(401, "Unauthorized", "malformed Authorization"))?;
        ("v4", cred.split('/').next().unwrap_or("").to_string(), format!("{}|{}", cred, sig))
    } else {
        return Err(error(401, "Unauthorized", "unknown authorization scheme"));
    };
    let secret = st.keys.get(&key_id).ok_or_else(|| error(401, "Unauthorized", "the AccessKey ID does not exist"))?;
    let token = req.header("x-acs-security-token");
    let token_key = st.tokens.iter().find(|(_, k)| **k == key_id).map(|(t, _)| t.as_str());
    match (token, token_key) {
        (Some(t), _) if st.tokens.get(t) != Some(&key_id) => return Err(error(401, "SecurityTokenExpired", "the security token is not valid for this AccessKey")),
        (None, Some(_)) => return Err(error(401, "Unauthorized", "a temporary AccessKey needs its security token")),
        _ => {}
    }
    let expected = if version == "v1" {
        let signed = authorization_v1(&key_id, secret, &req.method, path, query, &headers);
        // the Python SDK copies Date into x-log-date after signing (for proxies that drop Date)
        if !ct_eq(signed.as_bytes(), given.as_bytes()) && req.header("x-log-date").is_some() {
            let without: Vec<(String, String)> = headers.iter().filter(|(k, _)| !k.eq_ignore_ascii_case("x-log-date")).cloned().collect();
            authorization_v1(&key_id, secret, &req.method, path, query, &without)
        } else {
            signed
        }
    } else {
        // the canonical URI is the path as sent
        let (cred, _) = given.split_once('|').unwrap_or_default();
        let parts: Vec<&str> = cred.split('/').collect();
        let (date, region) = (parts.get(1).copied().unwrap_or(""), parts.get(2).copied().unwrap_or(""));
        if parts.len() != 5 || parts[3] != "sls" || parts[4] != "aliyun_v4_request" || !req.header("x-log-date").unwrap_or("").starts_with(date) {
            return Err(error(401, "Unauthorized", "malformed credential scope"));
        }
        format!("{}|{}", cred, signature_v4(secret, region, &req.method, &req.path, query, &headers))
    };
    if !ct_eq(expected.as_bytes(), given.as_bytes()) {
        return Err(error(401, "SignatureNotMatch", "the signature does not match"));
    }
    Ok(version)
}

fn handle(state: &Arc<Mutex<State>>, req: &ServerRequest) -> Response {
    let path = percent_decode(&req.path, false);
    let query = parse_form(&req.query);
    let mut st = state.lock().unwrap();
    let version = match authenticate(&st, req, &path, &query) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let host = req.header("Host").unwrap_or("");
    let project = host.split('.').next().unwrap_or("").to_string();
    if !host.contains('.') || !st.projects.contains_key(&project) {
        return error(404, "ProjectNotExist", &format!("Project {} does not exist", project));
    }
    let compress = req.header("x-log-compresstype").unwrap_or("").to_string();
    st.requests.push((req.method.clone(), path.clone(), version.to_string(), compress.clone()));
    let q = |k: &str| query.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let stores = st.projects.get_mut(&project).unwrap();
    match (req.method.as_str(), segments.as_slice()) {
        ("POST", ["logstores", name, "shards", "lb"]) => {
            let Some(store) = stores.get_mut(*name) else { return error(404, "LogStoreNotExist", &format!("logstore {} does not exist", name)) };
            if req.header("Content-Type") != Some("application/x-protobuf") {
                return error(400, "InvalidContentType", "PutLogs takes application/x-protobuf");
            }
            let Some(raw_size) = req.header("x-log-bodyrawsize").and_then(|s| s.trim().parse::<usize>().ok()) else { return error(400, "InvalidBodyRawSize", "x-log-bodyrawsize is required") };
            let raw = match compress.as_str() {
                "lz4" => lz4_decompress(&req.body, raw_size),
                "deflate" => zlib_decompress(&req.body),
                "" => Ok(req.body.clone()),
                "zstd" => return error(400, "InvalidCompressType", "this emulator does not decompress zstd"),
                other => return error(400, "InvalidCompressType", &format!("compress type {} is not supported", other)),
            };
            let Ok(raw) = raw else { return error(400, "InvalidCompressType", "the body does not decompress") };
            if raw.len() != raw_size {
                return error(400, "InvalidBodyRawSize", "x-log-bodyrawsize does not match the body");
            }
            let group = match LogGroup::decode(&raw) {
                Ok(g) => g,
                Err(e) => return error(400, "PostBodyInvalid", &e.message),
            };
            if group.logs.is_empty() {
                return error(400, "PostBodyInvalid", "the log group has no logs");
            }
            for l in group.logs {
                store.logs.push(StoredLog { time: l.time, topic: group.topic.clone().unwrap_or_default(), source: group.source.clone().unwrap_or_default(), tags: group.tags.clone(), contents: l.contents });
            }
            Response::new(200).with_header("x-log-requestid", random_hex(12).to_ascii_uppercase())
        }
        // GetLogs of the 2020-12-30 API (JSON body, `{meta, data}`) and the classic form (query
        // parameters, a JSON array with the meta in headers)
        ("POST", ["logstores", name, "logs"]) | ("GET", ["logstores", name]) => {
            let Some(store) = stores.get(*name) else { return error(404, "LogStoreNotExist", &format!("logstore {} does not exist", name)) };
            let classic = req.method == "GET";
            let body = if classic { Value::Null } else { crate::util::parse_json(&req.body).unwrap_or(Value::Null) };
            let p = |k: &str| if classic { q(k) } else { body.get(k).filter(|v| !v.is_null()).map(|v| v.to_string()) };
            if classic && q("type").as_deref() != Some("log") {
                return error(400, "ParameterInvalid", "type must be log");
            }
            let (Some(from), Some(to)) = (p("from").and_then(|v| v.parse::<i64>().ok()), p("to").and_then(|v| v.parse::<i64>().ok())) else {
                return error(400, "ParameterInvalid", "from and to are required");
            };
            let query = p("query").unwrap_or_default();
            if query.contains('|') {
                return error(400, "ParameterInvalid", "analytic statements are not supported by this emulator");
            }
            let topic = p("topic");
            let line = p("line").and_then(|v| v.parse::<usize>().ok()).unwrap_or(100).min(100);
            let offset = p("offset").and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
            let mut hits: Vec<&StoredLog> = store.logs.iter().filter(|l| (l.time as i64) >= from && (l.time as i64) < to && topic.as_ref().map(|t| *t == l.topic).unwrap_or(true) && matches(&query, l)).collect();
            hits.sort_by_key(|l| l.time);
            if p("reverse").as_deref() == Some("true") {
                hits.reverse();
            }
            let count = hits.len();
            let page: Vec<Value> = hits.into_iter().skip(offset).take(line).map(log_json).collect();
            let n = page.len();
            if classic {
                return ok(Value::Array(page).to_json()).with_header("x-log-progress", "Complete").with_header("x-log-count", n.to_string()).with_header("x-log-processed-rows", count.to_string()).with_header("x-log-elapsed-millisecond", "1");
            }
            let meta = ObjectMap::from_iter([
                ("progress".to_string(), Value::from("Complete")),
                ("count".to_string(), Value::Int(n as i64)),
                ("processedRows".to_string(), Value::Int(count as i64)),
                ("elapsedMillisecond".to_string(), Value::Int(1)),
                ("hasSQL".to_string(), Value::Bool(false)),
                ("isAccurate".to_string(), Value::Bool(true)),
                ("mode".to_string(), Value::Int(0)),
            ]);
            ok(Value::Object(ObjectMap::from_iter([("meta".to_string(), Value::Object(meta)), ("data".to_string(), Value::Array(page))])).to_json())
        }
        ("GET", ["logstores"]) => {
            let offset = q("offset").and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
            let size = q("size").and_then(|v| v.parse::<usize>().ok()).unwrap_or(200).min(500);
            let name = q("logstoreName").unwrap_or_default();
            let mode = q("mode");
            let telemetry = q("telemetryType");
            let all: Vec<&String> = stores.iter().filter(|(n, s)| n.contains(name.as_str()) && mode.as_ref().map(|m| *m == s.mode).unwrap_or(true) && telemetry.as_ref().map(|t| *t == s.telemetry_type).unwrap_or(true)).map(|(n, _)| n).collect();
            let page: Vec<Value> = all.iter().skip(offset).take(size).map(|n| Value::from(n.as_str())).collect();
            let body = Value::Object(ObjectMap::from_iter([("count".to_string(), Value::Int(page.len() as i64)), ("total".to_string(), Value::Int(all.len() as i64)), ("logstores".to_string(), Value::Array(page))]));
            ok(body.to_json())
        }
        ("POST", ["logstores"]) => {
            let v = crate::util::parse_json(&req.body).unwrap_or(Value::Null);
            let Some(name) = v.get("logstoreName").map(|n| n.to_string()) else { return error(400, "ParameterInvalid", "logstoreName is required") };
            if stores.contains_key(&name) {
                return error(400, "LogStoreAlreadyExist", &format!("logstore {} already exists", name));
            }
            let mode = v.get("mode").map(|m| m.to_string()).unwrap_or_else(|| "standard".into());
            let telemetry = v.get("telemetryType").map(|m| m.to_string()).filter(|t| !t.is_empty()).unwrap_or_else(|| "None".into());
            stores.insert(name, LogStore { mode, telemetry_type: telemetry, logs: Vec::new() });
            Response::new(200).with_header("x-log-requestid", random_hex(12).to_ascii_uppercase())
        }
        _ => error(404, "InvalidResource", &format!("{} {} is not served by this emulator", req.method, path)),
    }
}

/// A stored log as GetLogs returns it.
fn log_json(l: &StoredLog) -> Value {
    let mut m = ObjectMap::new();
    m.insert("__time__".into(), Value::from(l.time.to_string()));
    m.insert("__topic__".into(), Value::from(l.topic.clone()));
    m.insert("__source__".into(), Value::from(l.source.clone()));
    for (k, v) in &l.tags {
        m.insert(format!("__tag__:{}", k), Value::from(v.clone()));
    }
    for (k, v) in &l.contents {
        m.insert(k.clone(), Value::from(v.clone()));
    }
    Value::Object(m)
}

/// The search part of a query: `*`, words, `key: value` (or `key:value`), joined by `and`, `or`,
/// `not`. Words match any content value (case-insensitive, as a whole token).
fn matches(query: &str, l: &StoredLog) -> bool {
    let q = query.trim();
    if q.is_empty() || q == "*" {
        return true;
    }
    // `a or b`: any alternative; each alternative: every term (`and` implied)
    q.split(" or ").any(|alt| {
        let mut negate = false;
        let mut ok = true;
        let mut tokens = alt.split_whitespace().peekable();
        while let Some(t) = tokens.next() {
            match t.to_ascii_lowercase().as_str() {
                "and" => continue,
                "not" => {
                    negate = true;
                    continue;
                }
                _ => {}
            }
            let (key, value) = match t.strip_suffix(':') {
                Some(k) => (Some(k), tokens.next().unwrap_or("").to_string()),
                None => match t.split_once(':') {
                    Some((k, v)) => (Some(k), v.to_string()),
                    None => (None, t.to_string()),
                },
            };
            let value = value.trim_matches('"').to_ascii_lowercase();
            let hit = match key {
                Some(k) => l.contents.iter().chain(l.tags.iter()).any(|(n, v)| n == k && token_match(v, &value)),
                None => l.contents.iter().any(|(_, v)| token_match(v, &value)),
            };
            ok &= hit != negate;
            negate = false;
        }
        ok
    })
}

fn token_match(text: &str, word: &str) -> bool {
    if let Some(prefix) = word.strip_suffix('*') {
        return text.to_ascii_lowercase().split(|c: char| !c.is_alphanumeric() && c != '_').any(|t| t.starts_with(prefix));
    }
    text.to_ascii_lowercase().split(|c: char| !c.is_alphanumeric() && c != '_' && c != '.').any(|t| t == word)
}

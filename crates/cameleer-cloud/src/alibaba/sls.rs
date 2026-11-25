//! The `alibaba-sls` component (Camel `camel-alibaba-sls`, producer) over the Alibaba Cloud
//! Simple Log Service REST API.
//!
//! `alibaba-sls:operation?project=...&logStoreName=...&region=...&accessKey=...&secretKey=...`
//!
//! Operations (path, or `CamelAlibabaSlsOperation`):
//! * `putLogs`: `POST /logstores/{logStore}/shards/lb` with the body as a protobuf `LogGroup`
//!   ([`super::sls_proto::LogGroup::from_value`] says which bodies are accepted), compressed with
//!   `lz4` (default), `deflate` or not at all (`compressType`).
//! * `getLogs`: `POST /logstores/{logStore}/logs` with the JSON `{from, to, query, topic, line,
//!   offset, reverse}` (the 2020-12-30 GetLogs API of Camel's SDK); body = `{statusCode, headers,
//!   body: {meta, data: [log, ...]}}`.
//! * `listLogStores`: `GET /logstores?offset&size&logstoreName&mode&telemetryType`; body =
//!   `{statusCode, headers, logstores: [...], count, total}`.
//!
//! The request goes to `https://{project}.{endpoint}` (`endpoint`, else
//! `{region}.log.aliyuncs.com`), signed with the AccessKey pair (`accessKey` / `secretKey`, else
//! `ALIBABA_CLOUD_ACCESS_KEY_ID` / `ALIBABA_CLOUD_ACCESS_KEY_SECRET`) and the optional STS token
//! (`securityToken` / `ALIBABA_CLOUD_SECURITY_TOKEN`, sent as `x-acs-security-token`), with the SLS
//! signature V1 (default; HMAC-SHA256 as Camel's `sls20201230` SDK, or `signatureMethod=hmac-sha1`)
//! or V4 (`signatureVersion=v4`).

use std::sync::Arc;

use cameleer_core::dataformat::codecs::lz4_compress;
use cameleer_core::dataformat::compress::zlib_compress;
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::value::{ObjectMap, Value};

use super::sls_proto::LogGroup;
use super::sls_sign::{authorization_v1, authorization_v4, content_md5};
use crate::endpoint::{unsupported, CloudService};
use crate::http::{HttpClient, Proxy, Request, Response, Url};
use crate::util::{amz_date, cloud_error, header_str, http_date, kinds, now_secs, opt_i64, parse_json, sha256_hex, uri_encode, Opts};

/// Header names (Camel `AlibabaSlsHeaders`).
pub mod headers {
    pub const OPERATION: &str = "CamelAlibabaSlsOperation";
    pub const PROJECT: &str = "CamelAlibabaSlsProject";
    pub const LOG_STORE_NAME: &str = "CamelAlibabaSlsLogStoreName";
    pub const QUERY: &str = "CamelAlibabaSlsQuery";
    pub const FROM: &str = "CamelAlibabaSlsFrom";
    pub const TO: &str = "CamelAlibabaSlsTo";
    pub const LINE: &str = "CamelAlibabaSlsLine";
    pub const OFFSET: &str = "CamelAlibabaSlsOffset";
    pub const TOPIC: &str = "CamelAlibabaSlsTopic";
    pub const REVERSE: &str = "CamelAlibabaSlsReverse";
    pub const LOGSTORE_NAME: &str = "CamelAlibabaSlsListLogStoreName";
    pub const MODE: &str = "CamelAlibabaSlsMode";
    pub const SIZE: &str = "CamelAlibabaSlsSize";
    pub const LIST_OFFSET: &str = "CamelAlibabaSlsListOffset";
    pub const TELEMETRY_TYPE: &str = "CamelAlibabaSlsTelemetryType";
    pub const REQUEST_ID: &str = "CamelAlibabaSlsRequestId";
    pub const STATUS_CODE: &str = "CamelAlibabaSlsStatusCode";
}

/// The operations (Camel `AlibabaSlsOperations`).
pub const OPERATIONS: &[&str] = &["putLogs", "getLogs", "listLogStores"];

/// The SLS API version sent in `x-log-apiversion`.
pub const API_VERSION: &str = "0.6.0";

/// Body compression of `putLogs`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    Lz4,
    Deflate,
    None,
}

/// The signature scheme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignatureVersion {
    V1,
    V4,
}

/// An AccessKey pair with an optional STS token.
#[derive(Clone)]
pub struct SlsCredentials {
    pub access_key: String,
    pub secret_key: String,
    pub security_token: Option<String>,
}

impl std::fmt::Debug for SlsCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlsCredentials").field("access_key", &self.access_key).field("secret_key", &"***").field("security_token", &self.security_token.as_ref().map(|_| "***")).finish()
    }
}

/// The endpoint configuration and client.
pub struct SlsService {
    pub operation: Option<String>,
    pub project: Option<String>,
    pub log_store: Option<String>,
    /// `scheme://host[:port]` of the service (without the project).
    pub base: String,
    /// The local emulator to connect to instead (`uriEndpointOverride` + `overrideEndpoint=true`);
    /// the `Host` header still names `{project}.{endpoint}`.
    pub connect: Option<String>,
    pub region: Option<String>,
    /// `None` when no AccessKey pair is configured (the request then fails).
    pub credentials: Option<SlsCredentials>,
    pub signature: SignatureVersion,
    /// The HMAC of V1: `hmac-sha256` (default) or `hmac-sha1`.
    pub signature_method: String,
    pub compression: Compression,
    pub query: Option<String>,
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub line: Option<i64>,
    pub offset: Option<i64>,
    pub topic: Option<String>,
    pub reverse: Option<bool>,
    client: HttpClient,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// The region of an SLS endpoint host (`cn-hangzhou.log.aliyuncs.com`, `cn-hangzhou-intranet...`).
pub fn region_of_endpoint(host: &str) -> Option<String> {
    let first = host.split('.').next()?;
    let r = first.trim_end_matches("-intranet").trim_end_matches("-share");
    (host.contains(".log.aliyuncs.com") && !r.is_empty()).then(|| r.to_string())
}

/// Factory for the component.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let op = Some(remaining.to_string()).filter(|r| !r.is_empty()).or_else(|| o.get("operation"));
    if let Some(op) = &op {
        if !OPERATIONS.contains(&op.as_str()) {
            return Err(unsupported("Alibaba SLS", op));
        }
    }
    if o.get("serviceKeys").is_some() {
        return Err(CamelError::illegal_argument("alibaba-sls: serviceKeys is a Java object of Camel's Alibaba SDK support; set accessKey and secretKey instead"));
    }
    o.ignore(&["slsClient", "lazyStartProducer", "autowiredEnabled", "healthCheckConsumerEnabled", "healthCheckProducerEnabled"]);
    let region = o.get("region");
    let endpoint = o.get("endpoint");
    let base = match (&endpoint, &region) {
        (Some(e), _) => {
            let e = e.trim_end_matches('/');
            if e.contains("://") {
                e.to_string()
            } else {
                format!("https://{}", e)
            }
        }
        (None, Some(r)) => format!("https://{}.log.aliyuncs.com", r),
        (None, None) => return Err(CamelError::illegal_argument("alibaba-sls: set region (for example cn-hangzhou) or endpoint (for example cn-hangzhou.log.aliyuncs.com)")),
    };
    let host = Url::parse(&base)?.host;
    let region = region.or_else(|| region_of_endpoint(&host));
    let connect = match (o.get("uriEndpointOverride"), o.bool("overrideEndpoint", false)?) {
        (Some(u), true) => Some(u.trim_end_matches('/').to_string()),
        _ => None,
    };
    let access_key = o.get("accessKey").or_else(|| env("ALIBABA_CLOUD_ACCESS_KEY_ID"));
    let secret_key = o.get("secretKey").or_else(|| env("ALIBABA_CLOUD_ACCESS_KEY_SECRET"));
    let security_token = o.get("securityToken").or_else(|| env("ALIBABA_CLOUD_SECURITY_TOKEN"));
    // checked on the first request, as Camel creates its client on the first message
    let credentials = match (access_key, secret_key) {
        (Some(access_key), Some(secret_key)) => Some(SlsCredentials { access_key, secret_key, security_token }),
        _ => None,
    };
    let signature = match o.string("signatureVersion", "v1").to_ascii_lowercase().as_str() {
        "v1" | "1" => SignatureVersion::V1,
        "v4" | "4" => SignatureVersion::V4,
        other => return Err(CamelError::illegal_argument(format!("alibaba-sls: signatureVersion must be v1 or v4: {}", other))),
    };
    let signature_method = o.string("signatureMethod", "hmac-sha256").to_ascii_lowercase();
    if signature_method != "hmac-sha256" && signature_method != "hmac-sha1" {
        return Err(CamelError::illegal_argument(format!("alibaba-sls: signatureMethod must be hmac-sha256 or hmac-sha1: {}", signature_method)));
    }
    if signature == SignatureVersion::V4 && region.is_none() {
        return Err(CamelError::illegal_argument("alibaba-sls: signatureVersion=v4 needs the region (set region)"));
    }
    let compression = match o.string("compressType", "lz4").to_ascii_lowercase().as_str() {
        "lz4" => Compression::Lz4,
        "deflate" => Compression::Deflate,
        "none" | "" => Compression::None,
        other => return Err(CamelError::illegal_argument(format!("alibaba-sls: compressType must be lz4, deflate or none: {}", other))),
    };
    let proxy = match o.get("proxyHost") {
        Some(h) => Proxy::Explicit(format!("http://{}:{}", h, o.string("proxyPort", "3128"))),
        None => {
            o.ignore(&["proxyPort"]);
            Proxy::Environment
        }
    };
    let client = HttpClient::new().with_proxy(proxy).with_tls(o.tls()?);
    let reverse = o.get("reverse").map(|v| v.eq_ignore_ascii_case("true"));
    Ok(Arc::new(SlsService {
        operation: op,
        project: o.get("project"),
        log_store: o.get("logStoreName"),
        base,
        connect,
        region,
        credentials,
        signature,
        signature_method,
        compression,
        query: o.get("query"),
        from: opt_i64(o, "from")?,
        to: opt_i64(o, "to")?,
        line: opt_i64(o, "line")?,
        offset: opt_i64(o, "offset")?,
        topic: o.get("topic"),
        reverse,
        client,
    }))
}

/// Header, then exchange property, then the endpoint option.
fn pick(ex: &Exchange, name: &str, configured: &Option<String>) -> Option<String> {
    header_str(ex, name).or_else(|| ex.property(name).filter(|v| !v.is_null()).map(|v| v.to_string()).filter(|s| !s.is_empty())).or_else(|| configured.clone())
}

fn pick_i64(ex: &Exchange, name: &str, configured: Option<i64>) -> Result<Option<i64>> {
    match pick(ex, name, &None) {
        Some(v) => v.trim().parse().map(Some).map_err(|_| CamelError::illegal_argument(format!("{} must be a number: {}", name, v))),
        None => Ok(configured),
    }
}

fn sls_error(resp: &Response) -> CamelError {
    let v = parse_json(&resp.body).unwrap_or(Value::Null);
    let code = v.get("errorCode").map(|c| c.to_string()).unwrap_or_else(|| format!("HTTP {}", resp.status));
    let msg = v.get("errorMessage").map(|c| c.to_string()).unwrap_or_else(|| resp.text());
    let id = resp.header("x-log-requestid").unwrap_or("");
    cloud_error(kinds::ALIBABA_TEA, format!("code: {}, {} (status: {}, request id: {})", code, msg, resp.status, id))
}

impl SlsService {
    /// Sign and send one request; the response when it is a success.
    pub fn send(&self, method: &str, project: &str, path: &str, query: &[(String, String)], content_type: Option<&str>, body: Vec<u8>, raw_size: usize, extra: &[(&str, String)]) -> Result<Response> {
        let base = Url::parse(&self.base)?;
        let host = format!("{}.{}", project, base.host_header());
        let qs = query.iter().map(|(k, v)| format!("{}={}", uri_encode(k, false), uri_encode(v, false))).collect::<Vec<_>>().join("&");
        let encoded_path = uri_encode(path, true);
        let target = format!("{}{}{}", encoded_path, if qs.is_empty() { "" } else { "?" }, qs);
        let url = match &self.connect {
            Some(c) => format!("{}{}", c, target),
            None => format!("{}://{}{}", base.scheme, host, target),
        };
        let now = now_secs();
        let mut h: Vec<(String, String)> = vec![
            ("Host".into(), host),
            ("Date".into(), http_date(now)),
            ("x-log-apiversion".into(), API_VERSION.into()),
            ("x-log-bodyrawsize".into(), raw_size.to_string()),
            ("Content-Length".into(), body.len().to_string()),
            ("User-Agent".into(), format!("cameleer-alibaba-sls/{}", env!("CARGO_PKG_VERSION"))),
        ];
        if let Some(ct) = content_type {
            h.push(("Content-Type".into(), ct.into()));
        }
        if !body.is_empty() {
            h.push(("Content-MD5".into(), content_md5(&body)));
        }
        if let Some(t) = self.credentials.as_ref().and_then(|c| c.security_token.as_ref()) {
            h.push(("x-acs-security-token".into(), t.clone()));
        }
        for (k, v) in extra {
            h.push((k.to_string(), v.clone()));
        }
        let c = self.credentials.as_ref().ok_or_else(|| CamelError::illegal_argument("alibaba-sls: accessKey and secretKey are required (or ALIBABA_CLOUD_ACCESS_KEY_ID and ALIBABA_CLOUD_ACCESS_KEY_SECRET)"))?;
        let auth = match self.signature {
            SignatureVersion::V1 => {
                h.push(("x-log-signaturemethod".into(), self.signature_method.clone()));
                authorization_v1(&c.access_key, &c.secret_key, method, path, query, &h)
            }
            SignatureVersion::V4 => {
                h.push(("x-log-date".into(), amz_date(now)));
                h.push(("x-log-content-sha256".into(), sha256_hex(&body)));
                authorization_v4(&c.access_key, &c.secret_key, self.region.as_deref().unwrap_or_default(), method, &encoded_path, query, &h)
            }
        };
        h.push(("Authorization".into(), auth));
        let mut req = Request::new(method, Url::parse(&url)?);
        for (k, v) in h {
            req = req.header(&k, v);
        }
        let resp = self.client.send(&req.body(body)).map_err(|e| cloud_error(kinds::ALIBABA_TEA, format!("SLS request failed: {}", e.message)))?;
        if !resp.is_success() {
            return Err(sls_error(&resp));
        }
        Ok(resp)
    }

    /// Write a log group to a Logstore (`PutLogs`).
    pub fn put_logs(&self, project: &str, log_store: &str, group: &LogGroup) -> Result<Response> {
        let raw = group.encode();
        let (body, comp) = match self.compression {
            Compression::Lz4 => (lz4_compress(&raw), Some("lz4")),
            Compression::Deflate => (zlib_compress(&raw, 6), Some("deflate")),
            Compression::None => (raw.clone(), None),
        };
        let extra: Vec<(&str, String)> = comp.map(|c| ("x-log-compresstype", c.to_string())).into_iter().collect();
        self.send("POST", project, &format!("/logstores/{}/shards/lb", log_store), &[], Some("application/x-protobuf"), body, raw.len(), &extra)
    }

    fn project(&self, ex: &Exchange) -> Result<String> {
        pick(ex, headers::PROJECT, &self.project).ok_or_else(|| CamelError::illegal_argument("alibaba-sls: the project is required (project option or CamelAlibabaSlsProject)"))
    }

    fn log_store(&self, ex: &Exchange) -> Result<String> {
        pick(ex, headers::LOG_STORE_NAME, &self.log_store).ok_or_else(|| CamelError::illegal_argument("alibaba-sls: the Logstore is required (logStoreName option or CamelAlibabaSlsLogStoreName)"))
    }
}

fn headers_value(resp: &Response) -> Value {
    Value::Object(resp.headers.iter().map(|(k, v)| (k.to_ascii_lowercase(), Value::from(v.clone()))).collect::<ObjectMap>())
}

fn result(resp: &Response) -> ObjectMap {
    let mut m = ObjectMap::new();
    m.insert("statusCode".into(), Value::Int(resp.status as i64));
    m.insert("headers".into(), headers_value(resp));
    m
}

fn set_out(ex: &mut Exchange, resp: &Response, body: ObjectMap) {
    if let Some(id) = resp.header("x-log-requestid") {
        ex.set_header(headers::REQUEST_ID, id.to_string());
    }
    ex.set_header(headers::STATUS_CODE, resp.status as i64);
    ex.set_body(Value::Object(body));
}

impl CloudService for SlsService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        // the header, then the exchange property, then the endpoint path
        let op = pick(ex, headers::OPERATION, &self.operation).unwrap_or_default();
        match op.as_str() {
            "putLogs" => {
                let (project, store) = (self.project(ex)?, self.log_store(ex)?);
                let group = LogGroup::from_value(ex.body(), now_secs().clamp(0, u32::MAX as i64) as u32)?;
                let resp = self.put_logs(&project, &store, &group)?;
                let out = result(&resp);
                set_out(ex, &resp, out);
            }
            "getLogs" => {
                let (project, store) = (self.project(ex)?, self.log_store(ex)?);
                let from = pick_i64(ex, headers::FROM, self.from)?.ok_or_else(|| CamelError::illegal_argument("alibaba-sls getLogs: the start time is required (from option or CamelAlibabaSlsFrom, Unix seconds)"))?;
                let to = pick_i64(ex, headers::TO, self.to)?.ok_or_else(|| CamelError::illegal_argument("alibaba-sls getLogs: the end time is required (to option or CamelAlibabaSlsTo, Unix seconds)"))?;
                let mut req = ObjectMap::new();
                req.insert("from".into(), Value::Int(from));
                req.insert("to".into(), Value::Int(to));
                if let Some(v) = pick(ex, headers::QUERY, &self.query) {
                    req.insert("query".into(), Value::from(v));
                }
                if let Some(v) = pick(ex, headers::TOPIC, &self.topic) {
                    req.insert("topic".into(), Value::from(v));
                }
                if let Some(v) = pick_i64(ex, headers::LINE, self.line)? {
                    req.insert("line".into(), Value::Int(v));
                }
                if let Some(v) = pick_i64(ex, headers::OFFSET, self.offset)? {
                    req.insert("offset".into(), Value::Int(v));
                }
                let reverse = pick(ex, headers::REVERSE, &None).map(|v| v.eq_ignore_ascii_case("true")).or(self.reverse);
                if let Some(v) = reverse {
                    req.insert("reverse".into(), Value::Bool(v));
                }
                let json = Value::Object(req).to_json().into_bytes();
                let n = json.len();
                let resp = self.send("POST", &project, &format!("/logstores/{}/logs", store), &[], Some("application/json"), json, n, &[])?;
                let mut out = result(&resp);
                out.insert("body".into(), parse_json(&resp.body)?);
                set_out(ex, &resp, out);
            }
            "listLogStores" => {
                let project = self.project(ex)?;
                let mut q: Vec<(String, String)> = Vec::new();
                if let Some(v) = pick_i64(ex, headers::LIST_OFFSET, None)? {
                    q.push(("offset".into(), v.to_string()));
                }
                if let Some(v) = pick_i64(ex, headers::SIZE, None)? {
                    q.push(("size".into(), v.to_string()));
                }
                for (name, key) in [(headers::LOGSTORE_NAME, "logstoreName"), (headers::MODE, "mode"), (headers::TELEMETRY_TYPE, "telemetryType")] {
                    if let Some(v) = pick(ex, name, &None) {
                        q.push((key.into(), v));
                    }
                }
                let resp = self.send("GET", &project, "/logstores", &q, None, Vec::new(), 0, &[])?;
                let v = parse_json(&resp.body)?;
                let mut out = result(&resp);
                out.insert("logstores".into(), v.get("logstores").cloned().unwrap_or(Value::Array(Vec::new())));
                out.insert("count".into(), v.get("count").cloned().unwrap_or(Value::Int(0)));
                out.insert("total".into(), v.get("total").cloned().unwrap_or(Value::Int(0)));
                set_out(ex, &resp, out);
            }
            "" => return Err(CamelError::illegal_argument("alibaba-sls: the operation is required (alibaba-sls:putLogs, getLogs or listLogStores, or CamelAlibabaSlsOperation)")),
            other => return Err(unsupported("Alibaba SLS", other)),
        }
        Ok(())
    }
}

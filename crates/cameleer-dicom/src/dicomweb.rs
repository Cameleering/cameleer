//! The `dicomweb:` component: DICOMweb (PS3.18) QIDO-RS, WADO-RS and STOW-RS.
//!
//! - **Producer** (client): `dicomweb:http://host:port/dicom-web?operation=stow|qido|wado|metadata`.
//!   - `stow` (default): POSTs the body (Part 10 bytes, a Value tree, or an array of them) as
//!     `multipart/related; type="application/dicom"` to `/studies` (or
//!     `/studies/{CamelDicomStudyInstanceUid}`); the body becomes the response (a Value tree of
//!     the DICOM JSON answer). Failed instances raise `DicomWebException` unless
//!     `failOnPartialSuccess=false`.
//!   - `qido`: searches `/studies`, `/series` or `/instances` (`queryLevel`), within a study or
//!     series when `CamelDicomStudyInstanceUid` / `CamelDicomSeriesInstanceUid` are set; query
//!     keys come from the body (a map of keywords or tags, or a Value tree) and the `CamelDicomQuery`
//!     header; the result is an array of Value trees.
//!   - `wado`: retrieves the study, series or instance named by the UID headers (or the body's
//!     UIDs); the result is an array of Part 10 bytes.
//!   - `metadata`: the same as `wado` for `/metadata`; the result is an array of Value trees.
//!
//!   Authentication: `bearerToken` (also from a property placeholder or vault reference) or
//!   `username`/`password` (Basic), or a `CamelDicomWebAuthorization` header; TLS for `https`
//!   with `sslContextParameters`.
//! - **Consumer** (server): `dicomweb:/dicom-web` serves the DICOMweb resources on the shared HTTP
//!   server (the one `platform-http` uses: `camel.component.dicomweb.port`, `camel.server.port`,
//!   default 8080; or `port=`). Each stored instance (STOW-RS), each search (QIDO-RS) and each
//!   retrieval (WADO-RS, metadata, bulk data) becomes an exchange, shaped like the `dicom:` SCP:
//!   the route stores the instance (body Part 10 bytes), or answers the identifier (a Value
//!   tree) with the matches (an array of Value trees) or the instances (an array of Part 10
//!   bytes). The same route can serve both a `dicom:` and a `dicomweb:` consumer.

use std::any::Any;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cameleer_core::context::{CamelContext, WeakContext};
use cameleer_core::dataformat::base64;
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::{Exchange, ExchangePattern};
use cameleer_core::multipart::{self, Part};
use cameleer_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use cameleer_core::util::Parameters;
use cameleer_core::value::{ObjectMap, Value};
use cameleer_http::client::HttpClient;
use cameleer_http::http::{parse_query_string, HttpRequest, HttpResponse};
use cameleer_http::registry::{registry, HttpRouteSpec, Registration};
use cameleer_http::server::ServerConfig;

use crate::charset::CharacterSet;
use crate::codec::{DicomFile, ReadOptions};
use crate::dataformat::{body_bytes, json_model_to_tree, transcode, DicomJsonDataFormat};
use crate::headers;
use crate::model;
use crate::object::{DataSet, Element};
use crate::tag::{self, tags};
use crate::vr::Vr;

/// Exception kind for DICOMweb errors (HTTP failures and failed instances).
pub const DICOMWEB_EXCEPTION: &str = "DicomWebException";

fn werr(msg: impl Into<String>) -> CamelError {
    CamelError::new(DICOMWEB_EXCEPTION, msg)
}

/// The DICOM media types.
pub const APPLICATION_DICOM: &str = "application/dicom";
pub const APPLICATION_DICOM_JSON: &str = "application/dicom+json";

/// The `dicomweb` component.
#[derive(Default)]
pub struct DicomWebComponent;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Stow,
    Qido,
    Wado,
    Metadata,
}

fn parse_op(s: &str) -> Result<Op> {
    match s.trim().to_ascii_lowercase().as_str() {
        "stow" | "stow-rs" | "store" => Ok(Op::Stow),
        "qido" | "qido-rs" | "search" | "find" => Ok(Op::Qido),
        "wado" | "wado-rs" | "retrieve" => Ok(Op::Wado),
        "metadata" => Ok(Op::Metadata),
        other => Err(CamelError::illegal_argument(format!("Unknown dicomweb operation '{}' (stow, qido, wado, metadata)", other))),
    }
}

/// Client settings.
#[derive(Clone)]
pub struct ClientConfig {
    pub base: String,
    op: Op,
    /// `studies`, `series` or `instances`.
    pub query_level: String,
    pub query: Vec<(String, String)>,
    pub transfer_syntax: String,
    pub authorization: Option<String>,
    pub timeout: Duration,
    pub fail_on_partial_success: bool,
    tls: Option<cameleer_tls::camel_support::EndpointTls>,
}

/// Server settings.
#[derive(Clone)]
pub struct ServerSettings {
    pub host: String,
    pub port: u16,
    pub path: String,
    pub server_config: ServerConfig,
    pub bulk_data_threshold: usize,
}

enum Mode {
    Client(ClientConfig),
    Server(ServerSettings),
}

impl Component for DicomWebComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, p: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let is_client = remaining.starts_with("http://") || remaining.starts_with("https://");
        let mode = if is_client {
            let base = remaining.trim_end_matches('/').to_string();
            let tls = cameleer_tls::camel_support::endpoint_tls(ctx, p, "dicomweb", false, &[])?;
            let authorization = match (p.take("bearerToken"), p.take("username")) {
                (Some(t), _) if !t.is_empty() => Some(format!("Bearer {}", t)),
                (_, Some(u)) => Some(format!("Basic {}", base64::encode(format!("{}:{}", u, p.take("password").unwrap_or_default()).as_bytes(), false))),
                _ => None,
            };
            let mut query = Vec::new();
            for k in ["limit", "offset", "fuzzymatching", "includefield"] {
                if let Some(v) = p.take(k) {
                    for part in if k == "includefield" { v.split(',').map(str::to_string).collect::<Vec<_>>() } else { vec![v] } {
                        query.push((k.to_string(), part.trim().to_string()));
                    }
                }
            }
            Mode::Client(ClientConfig {
                base,
                op: parse_op(&p.take_string("operation", "stow"))?,
                query_level: normalize_level(&p.take_string("queryLevel", "studies"))?,
                query,
                transfer_syntax: p.take_string("transferSyntax", "*"),
                authorization,
                timeout: p.take_duration("timeout", Duration::from_secs(60))?,
                fail_on_partial_success: p.take_bool("failOnPartialSuccess", true)?,
                tls,
            })
        } else {
            let ssl = cameleer_tls::camel_support::take_endpoint_ssl(ctx, p, "dicomweb")?;
            let ssl_flag = p.take_bool("ssl", false)?;
            let client_auth = cameleer_tls::camel_support::take_client_auth(p)?;
            let mut server_config = ServerConfig::default();
            if ssl_flag || ssl.is_some() {
                server_config.tls = Some(Arc::new(cameleer_tls::camel_support::server_config_with_client_auth(ctx, ssl.as_ref(), uri, client_auth)?));
            }
            let port = match p.take("port") {
                Some(v) => v.trim().parse::<u16>().map_err(|_| CamelError::illegal_argument(format!("Invalid port: {}", v)))?,
                None => default_port(ctx)?,
            };
            let host = p.take("host").or_else(|| ctx.properties().get("camel.server.host")).unwrap_or_else(|| "0.0.0.0".into());
            let path = format!("/{}", remaining.trim_matches('/'));
            Mode::Server(ServerSettings { host, port, path, server_config, bulk_data_threshold: p.take_usize("bulkDataThreshold", 1024)? })
        };
        Ok(Arc::new(DicomWebEndpoint { uri: uri.to_string(), ctx: ctx.downgrade(), mode: Arc::new(mode), bound: Arc::new(Mutex::new(None)) }))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn default_port(ctx: &CamelContext) -> Result<u16> {
    for key in ["camel.component.dicomweb.port", "camel.server.port"] {
        if let Some(v) = ctx.properties().get(key) {
            return v.trim().parse().map_err(|_| CamelError::illegal_argument(format!("Invalid port in property {}: {}", key, v)));
        }
    }
    Ok(8080)
}

fn normalize_level(s: &str) -> Result<String> {
    Ok(match s.trim().to_ascii_lowercase().as_str() {
        "studies" | "study" => "studies",
        "series" => "series",
        "instances" | "instance" | "image" => "instances",
        other => return Err(CamelError::illegal_argument(format!("queryLevel must be studies, series or instances, not {}", other))),
    }
    .to_string())
}

/// A `dicomweb` endpoint.
pub struct DicomWebEndpoint {
    uri: String,
    ctx: WeakContext,
    mode: Arc<Mode>,
    bound: Arc<Mutex<Option<Registration>>>,
}

impl DicomWebEndpoint {
    /// The bound port while a consumer runs.
    pub fn local_port(&self) -> Option<u16> {
        self.bound.lock().unwrap().as_ref().map(|r| r.port)
    }
}

impl Endpoint for DicomWebEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        match self.mode.as_ref() {
            Mode::Client(c) => Ok(Arc::new(DicomWebProducer { cfg: c.clone() })),
            Mode::Server(_) => Err(CamelError::illegal_argument(format!("{}: a dicomweb producer needs an http:// or https:// base URL", self.uri))),
        }
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        match self.mode.as_ref() {
            Mode::Server(s) => Ok(Arc::new(DicomWebConsumer { settings: s.clone(), ctx: self.ctx.clone(), processor, bound: self.bound.clone() })),
            Mode::Client(_) => Err(CamelError::illegal_argument(format!("{}: a dicomweb consumer takes a path such as dicomweb:/dicom-web", self.uri))),
        }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---------------------------------------------------------------------------------------
// Shared helpers

/// A STOW-RS request body for instances.
pub fn stow_body(files: &[Vec<u8>]) -> (String, Vec<u8>) {
    let b = multipart::new_boundary();
    let parts: Vec<Part> = files.iter().map(|f| Part::new(APPLICATION_DICOM, f.clone())).collect();
    (format!("multipart/related; type=\"{}\"; boundary={}", APPLICATION_DICOM, b), multipart::build(&parts, &b))
}

/// The parts of a multipart response or request.
pub fn split_multipart(content_type: &str, body: &[u8]) -> Result<Vec<Part>> {
    let b = multipart::boundary(content_type).ok_or_else(|| werr(format!("Expected a multipart body, got Content-Type '{}'", content_type)))?;
    multipart::parse(body, &b)
}

/// Query parameters to an identifier: keywords or tags with values, `includefield` names as
/// empty attributes (`all` is ignored), the level's unique key always present.
pub fn query_to_identifier(params: &[(String, String)], level: &str) -> Result<DataSet> {
    let mut ds = DataSet::new();
    let cs = CharacterSet::from_value("ISO_IR 192");
    for (k, v) in params {
        match k.as_str() {
            "limit" | "offset" | "fuzzymatching" => continue,
            "includefield" => {
                for f in v.split(',').map(str::trim).filter(|f| !f.is_empty() && *f != "all") {
                    if let Some(t) = model::key_tag(f) {
                        if !ds.contains(t) {
                            ds.put_empty(t);
                        }
                    }
                }
            }
            key => {
                // sequence paths (ReferencedStudySequence.ReferencedSOPInstanceUID) are not
                // supported as query keys; unknown keys are refused
                let t = model::key_tag(key).ok_or_else(|| werr(format!("Unknown query key: {}", key)))?;
                let vr = model::default_vr(t);
                ds.insert(Element::strings(t, vr, &[v.as_str()], &cs)?);
            }
        }
    }
    if ds.iter().any(|e| e.vr.uses_charset() && e.raw().is_some_and(|b| !b.is_ascii())) {
        ds.insert(Element::string(tags::SPECIFIC_CHARACTER_SET, Vr::CS, "ISO_IR 192"));
    }
    let key = match level {
        "SERIES" => tags::SERIES_INSTANCE_UID,
        "IMAGE" => tags::SOP_INSTANCE_UID,
        _ => tags::STUDY_INSTANCE_UID,
    };
    if !ds.contains(key) {
        ds.put_empty(key);
    }
    ds.insert(Element::string(tags::QUERY_RETRIEVE_LEVEL, Vr::CS, level));
    Ok(ds)
}

fn json_array_to_trees(text: &[u8]) -> Result<Vec<Value>> {
    if text.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(Vec::new());
    }
    match cameleer_core::json::parse(&String::from_utf8_lossy(text))? {
        Value::Array(a) => a.iter().map(json_model_to_tree).collect(),
        other => Ok(vec![json_model_to_tree(&other)?]),
    }
}

// ---------------------------------------------------------------------------------------
// Producer

struct DicomWebProducer {
    cfg: ClientConfig,
}

impl Service for DicomWebProducer {}

fn header_str(ex: &Exchange, name: &str) -> Option<String> {
    ex.header(name).map(|v| v.to_string()).filter(|s| !s.is_empty())
}

impl DicomWebProducer {
    fn client(&self) -> Result<HttpClient> {
        let mut c = HttpClient::new();
        c.read_timeout = self.cfg.timeout;
        c.limits.max_body_bytes = 4 << 30;
        if let Some(t) = &self.cfg.tls {
            c.tls = Some(t.client_config()?);
        }
        Ok(c)
    }

    fn send(&self, ex: &Exchange, method: &str, url: &str, accept: &str, content_type: Option<&str>, body: Option<&[u8]>) -> Result<cameleer_http::http::HttpResponse> {
        let auth = header_str(ex, "CamelDicomWebAuthorization").or_else(|| self.cfg.authorization.clone());
        let mut h: Vec<(&str, &str)> = vec![("Accept", accept)];
        if let Some(ct) = content_type {
            h.push(("Content-Type", ct));
        }
        if let Some(a) = &auth {
            h.push(("Authorization", a.as_str()));
        }
        self.client()?.request(method, url, &h, body, Some(self.cfg.timeout))
    }

    fn uids(&self, ex: &Exchange) -> (Option<String>, Option<String>, Option<String>) {
        let from_body = |t: u32| -> Option<String> {
            match ex.body() {
                Value::Object(_) => model::value_to_dataset(ex.body(), &CharacterSet::default()).ok().and_then(|d| d.string(t)).filter(|s| !s.is_empty()),
                _ => None,
            }
        };
        (
            header_str(ex, headers::STUDY_INSTANCE_UID).or_else(|| from_body(tags::STUDY_INSTANCE_UID)),
            header_str(ex, headers::SERIES_INSTANCE_UID).or_else(|| from_body(tags::SERIES_INSTANCE_UID)),
            header_str(ex, headers::SOP_INSTANCE_UID).or_else(|| from_body(tags::SOP_INSTANCE_UID)),
        )
    }
}

fn check_http(op: &str, r: &cameleer_http::http::HttpResponse) -> Result<()> {
    if r.status >= 400 {
        let text = String::from_utf8_lossy(&r.body);
        let mut t: String = text.chars().take(300).collect();
        if text.len() > 300 {
            t.push_str("...");
        }
        return Err(werr(format!("{} failed with HTTP {}: {}", op, r.status, t.trim())));
    }
    Ok(())
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'*' | b',' => (b as char).to_string(),
            _ => format!("%{:02X}", b),
        })
        .collect()
}

impl Processor for DicomWebProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let op = match header_str(ex, headers::DICOMWEB_OPERATION) {
            Some(o) => parse_op(&o)?,
            None => self.cfg.op,
        };
        let base = &self.cfg.base;
        match op {
            Op::Stow => {
                let files = match ex.body() {
                    Value::Array(a) => a.iter().map(body_bytes).collect::<Result<Vec<_>>>()?,
                    other => vec![body_bytes(other)?],
                };
                let url = match header_str(ex, headers::STUDY_INSTANCE_UID) {
                    Some(s) => format!("{}/studies/{}", base, enc(&s)),
                    None => format!("{}/studies", base),
                };
                let (ct, body) = stow_body(&files);
                let r = self.send(ex, "POST", &url, APPLICATION_DICOM_JSON, Some(&ct), Some(&body))?;
                ex.set_header(headers::HTTP_STATUS, r.status as i64);
                let trees = if r.body.is_empty() { Vec::new() } else { json_array_to_trees(&r.body).unwrap_or_default() };
                let answer = trees.into_iter().next().unwrap_or(Value::Object(ObjectMap::new()));
                let count = |t: &str| answer.get(t).and_then(|e| e.get("Value")).and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
                let (ok, failed) = (count("00081199"), count("00081198"));
                ex.set_header(headers::COMPLETED, ok as i64);
                ex.set_header(headers::FAILED, failed as i64);
                if r.status >= 400 || (failed > 0 && self.cfg.fail_on_partial_success) {
                    let reasons: Vec<String> = answer
                        .get("00081198")
                        .and_then(|e| e.get("Value"))
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .map(|it| {
                                    let uid = it.get("00081155").and_then(|e| e.get("Value")).and_then(|v| v.as_array()).and_then(|v| v.first()).map(|v| v.to_string()).unwrap_or_default();
                                    let why = it.get("00081197").and_then(|e| e.get("Value")).and_then(|v| v.as_array()).and_then(|v| v.first()).and_then(|v| v.as_i64()).unwrap_or(0);
                                    format!("{} (reason 0x{:04X})", uid, why)
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    check_http("STOW-RS", &r)?;
                    return Err(werr(format!("STOW-RS: {} of {} instances failed: {}", failed, ok + failed, reasons.join(", "))));
                }
                ex.set_body(answer);
                Ok(())
            }
            Op::Qido => {
                let (study, series, _) = (header_str(ex, headers::STUDY_INSTANCE_UID), header_str(ex, headers::SERIES_INSTANCE_UID), ());
                let level = self.cfg.query_level.as_str();
                let path = match (level, &study, &series) {
                    ("series", Some(s), _) => format!("/studies/{}/series", enc(s)),
                    ("instances", Some(s), Some(se)) => format!("/studies/{}/series/{}/instances", enc(s), enc(se)),
                    ("instances", Some(s), None) => format!("/studies/{}/instances", enc(s)),
                    (l, _, _) => format!("/{}", l),
                };
                let mut q: Vec<(String, String)> = Vec::new();
                // keys from the body (a keyword/tag map or a Value tree)
                if let Value::Object(_) = ex.body() {
                    let ds = model::value_to_dataset(ex.body(), &CharacterSet::default())?;
                    let cs = ds.charset();
                    for e in ds.iter() {
                        if e.tag == tags::QUERY_RETRIEVE_LEVEL || e.tag == tags::SPECIFIC_CHARACTER_SET {
                            continue;
                        }
                        let key = crate::dict::lookup(e.tag).map(|d| d.keyword.to_string()).filter(|k| !k.is_empty()).unwrap_or_else(|| tag::format_hex(e.tag));
                        let v = e.to_string_value(&cs);
                        if v.is_empty() {
                            q.push(("includefield".into(), key));
                        } else {
                            q.push((key, v));
                        }
                    }
                }
                if let Some(Value::Object(m)) = ex.header(headers::QUERY) {
                    for (k, v) in m.iter() {
                        q.push((k.clone(), v.to_string()));
                    }
                }
                q.extend(self.cfg.query.iter().cloned());
                let qs: Vec<String> = q.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect();
                let url = if qs.is_empty() { format!("{}{}", base, path) } else { format!("{}{}?{}", base, path, qs.join("&")) };
                let r = self.send(ex, "GET", &url, APPLICATION_DICOM_JSON, None, None)?;
                ex.set_header(headers::HTTP_STATUS, r.status as i64);
                check_http("QIDO-RS", &r)?;
                let trees = json_array_to_trees(&r.body)?;
                ex.set_header(headers::RESULT_COUNT, trees.len() as i64);
                ex.set_body(Value::Array(trees));
                Ok(())
            }
            Op::Wado | Op::Metadata => {
                let (study, series, inst) = self.uids(ex);
                let study = study.ok_or_else(|| werr("WADO-RS needs a Study Instance UID (CamelDicomStudyInstanceUid or in the body)"))?;
                let mut path = format!("/studies/{}", enc(&study));
                if let Some(s) = &series {
                    path.push_str(&format!("/series/{}", enc(s)));
                    if let Some(i) = &inst {
                        path.push_str(&format!("/instances/{}", enc(i)));
                    }
                }
                if op == Op::Metadata {
                    let r = self.send(ex, "GET", &format!("{}{}/metadata", base, path), APPLICATION_DICOM_JSON, None, None)?;
                    ex.set_header(headers::HTTP_STATUS, r.status as i64);
                    check_http("WADO-RS metadata", &r)?;
                    let trees = json_array_to_trees(&r.body)?;
                    ex.set_header(headers::RESULT_COUNT, trees.len() as i64);
                    ex.set_body(Value::Array(trees));
                    return Ok(());
                }
                let accept = format!("multipart/related; type=\"{}\"; transfer-syntax={}", APPLICATION_DICOM, self.cfg.transfer_syntax);
                let r = self.send(ex, "GET", &format!("{}{}", base, path), &accept, None, None)?;
                ex.set_header(headers::HTTP_STATUS, r.status as i64);
                check_http("WADO-RS", &r)?;
                let ct = r.headers.get("Content-Type").unwrap_or("").to_string();
                let files: Vec<Value> = if ct.to_ascii_lowercase().starts_with("multipart/") {
                    split_multipart(&ct, &r.body)?.into_iter().map(|p| Value::Bytes(p.body)).collect()
                } else {
                    vec![Value::Bytes(r.body)]
                };
                ex.set_header(headers::RESULT_COUNT, files.len() as i64);
                ex.set_body(Value::Array(files));
                Ok(())
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// Consumer

struct DicomWebConsumer {
    settings: ServerSettings,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
    bound: Arc<Mutex<Option<Registration>>>,
}

struct Server {
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
    base: String,
    bulk_data_threshold: usize,
}

fn json_response(status: u16, v: &Value) -> HttpResponse {
    let mut r = HttpResponse::new(status).with_body(v.to_json().into_bytes());
    r.headers.set("Content-Type", APPLICATION_DICOM_JSON);
    r
}

fn error_response(status: u16, msg: &str) -> HttpResponse {
    HttpResponse::text(status, msg.to_string())
}

fn uid_seq_item(class: &str, inst: &str, extra: Vec<Element>) -> DataSet {
    let mut d = DataSet::new();
    d.insert(crate::dimse::ui(tags::REFERENCED_SOP_CLASS_UID, class));
    d.insert(crate::dimse::ui(tags::REFERENCED_SOP_INSTANCE_UID, inst));
    for e in extra {
        d.insert(e);
    }
    d
}

impl Server {
    fn exchange(&self, req: &HttpRequest, op: &str, body: Value) -> Exchange {
        let mut ex = Exchange::new(self.ctx.upgrade());
        ex.pattern = ExchangePattern::InOut;
        ex.set_body(body);
        ex.set_header(headers::DICOMWEB_OPERATION, op);
        ex.set_header(headers::COMMAND, match op {
            "stow" => "STOW-RS",
            "qido" => "QIDO-RS",
            _ => "WADO-RS",
        });
        if let Some(a) = req.remote_addr {
            ex.set_header(headers::REMOTE_ADDRESS, a.to_string());
        }
        for h in ["Authorization", "X-Forwarded-For"] {
            if let Some(v) = req.headers.get(h) {
                ex.set_header(h, v.to_string());
            }
        }
        ex
    }

    fn run(&self, ex: &mut Exchange) {
        if let Err(e) = self.processor.process(ex) {
            ex.set_exception(e);
        }
    }

    fn handle(&self, req: &mut HttpRequest) -> HttpResponse {
        let rel = req.path.strip_prefix(&self.base).unwrap_or(&req.path).trim_matches('/').to_string();
        let seg: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
        let query = req.raw_query.as_deref().map(parse_query_string).unwrap_or_default();
        match (req.method.as_str(), seg.as_slice()) {
            ("POST", ["studies"]) => self.stow(req, None),
            ("POST", ["studies", s]) => self.stow(req, Some(s.to_string())),
            ("GET" | "HEAD", ["studies"]) => self.qido(&query, "STUDY", &[]),
            ("GET" | "HEAD", ["series"]) => self.qido(&query, "SERIES", &[]),
            ("GET" | "HEAD", ["instances"]) => self.qido(&query, "IMAGE", &[]),
            ("GET" | "HEAD", ["studies", s, "series"]) => self.qido(&query, "SERIES", &[(tags::STUDY_INSTANCE_UID, s)]),
            ("GET" | "HEAD", ["studies", s, "instances"]) => self.qido(&query, "IMAGE", &[(tags::STUDY_INSTANCE_UID, s)]),
            ("GET" | "HEAD", ["studies", s, "series", se, "instances"]) => self.qido(&query, "IMAGE", &[(tags::STUDY_INSTANCE_UID, s), (tags::SERIES_INSTANCE_UID, se)]),
            ("GET" | "HEAD", ["studies", s]) => self.wado(req, &[s], false),
            ("GET" | "HEAD", ["studies", s, "series", se]) => self.wado(req, &[s, se], false),
            ("GET" | "HEAD", ["studies", s, "series", se, "instances", i]) => self.wado(req, &[s, se, i], false),
            ("GET" | "HEAD", ["studies", s, "metadata"]) => self.wado(req, &[s], true),
            ("GET" | "HEAD", ["studies", s, "series", se, "metadata"]) => self.wado(req, &[s, se], true),
            ("GET" | "HEAD", ["studies", s, "series", se, "instances", i, "metadata"]) => self.wado(req, &[s, se, i], true),
            ("GET" | "HEAD", ["studies", s, "series", se, "instances", i, "bulkdata", t]) => self.bulkdata(req, &[s, se, i], t),
            ("GET", [.., "frames", _]) | ("GET", [.., "rendered"]) | ("GET", [.., "thumbnail"]) => error_response(501, "Frames, rendered and thumbnail resources need pixel codecs, which this server does not have"),
            (_, _) => error_response(404, &format!("No DICOMweb resource at {}", req.path)),
        }
    }

    fn stow(&self, req: &HttpRequest, study: Option<String>) -> HttpResponse {
        let ct = req.headers.get("Content-Type").unwrap_or("").to_string();
        let mt = ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        let files: Vec<Vec<u8>> = if mt == "multipart/related" {
            match split_multipart(&ct, &req.body) {
                Ok(parts) => {
                    if parts.iter().any(|p| p.media_type().is_some_and(|m| m != APPLICATION_DICOM)) {
                        return error_response(415, "Only application/dicom parts are accepted (JSON metadata with bulk data is not supported)");
                    }
                    parts.into_iter().map(|p| p.body).collect()
                }
                Err(e) => return error_response(400, &e.message),
            }
        } else if mt == APPLICATION_DICOM {
            vec![req.body.clone()]
        } else {
            return error_response(415, &format!("STOW-RS takes multipart/related; type=\"application/dicom\", not {}", ct));
        };
        let mut referenced = Vec::new();
        let mut failed = Vec::new();
        for f in files {
            let parsed = DicomFile::parse(&f, ReadOptions { stop_before_pixel_data: true });
            let (class, inst, st_uid, se_uid) = match &parsed {
                Ok(p) => (
                    p.dataset.sop_class_uid().unwrap_or_default(),
                    p.dataset.sop_instance_uid().unwrap_or_default(),
                    p.dataset.string(tags::STUDY_INSTANCE_UID).unwrap_or_default(),
                    p.dataset.string(tags::SERIES_INSTANCE_UID).unwrap_or_default(),
                ),
                Err(_) => (String::new(), String::new(), String::new(), String::new()),
            };
            let reason = if parsed.is_err() {
                Some(0xC000u16)
            } else if study.as_deref().is_some_and(|s| s != st_uid) {
                Some(0xA900) // the instance does not belong to the study in the URL
            } else {
                let mut ex = self.exchange(req, "stow", Value::Bytes(f));
                ex.set_header(headers::SOP_CLASS_UID, class.clone());
                ex.set_header(headers::SOP_INSTANCE_UID, inst.clone());
                ex.set_header(headers::STUDY_INSTANCE_UID, st_uid.clone());
                self.run(&mut ex);
                match (&ex.exception, ex.header(headers::STATUS).and_then(|v| v.as_i64())) {
                    (_, Some(s)) if crate::dimse::status_class(s as u16) == crate::dimse::StatusClass::Failure => Some(s as u16),
                    (Some(_), _) => Some(0xA700),
                    _ => None,
                }
            };
            match reason {
                None => {
                    let url = format!("{}/studies/{}/series/{}/instances/{}", self.base, st_uid, se_uid, inst);
                    referenced.push(uid_seq_item(&class, &inst, vec![Element::string(tags::RETRIEVE_URL, Vr::UR, &url)]));
                }
                Some(r) => failed.push(uid_seq_item(&class, &inst, vec![Element::us(tags::FAILURE_REASON, r)])),
            }
        }
        let mut resp = DataSet::new();
        if !failed.is_empty() {
            resp.insert(Element::sequence(tags::FAILED_SOP_SEQUENCE, failed.clone()));
        }
        if !referenced.is_empty() {
            resp.insert(Element::sequence(tags::REFERENCED_SOP_SEQUENCE, referenced.clone()));
        }
        if let Some(s) = &study {
            resp.insert(Element::string(tags::RETRIEVE_URL, Vr::UR, &format!("{}/studies/{}", self.base, s)));
        }
        let status = match (referenced.is_empty(), failed.is_empty()) {
            (_, true) => 200,
            (false, false) => 202,
            (true, false) => 409,
        };
        let df = DicomJsonDataFormat::default();
        json_response(status, &df.to_json_model(&model::dataset_to_value(&resp, &CharacterSet::default())))
    }

    fn qido(&self, query: &[(String, String)], level: &str, fixed: &[(u32, &&str)]) -> HttpResponse {
        let mut q: Vec<(String, String)> = query.to_vec();
        for (t, v) in fixed {
            q.push((tag::format_hex(*t), v.to_string()));
        }
        let ident = match query_to_identifier(&q, level) {
            Ok(d) => d,
            Err(e) => return error_response(400, &e.message),
        };
        let fake = HttpRequest::new("GET", "/");
        let mut ex = self.exchange(&fake, "qido", model::dataset_to_value(&ident, &CharacterSet::default()));
        ex.set_header(headers::QUERY_LEVEL, level);
        let mut qm = ObjectMap::new();
        for (k, v) in query {
            qm.insert(k.clone(), Value::String(v.clone()));
        }
        ex.set_header(headers::QUERY, Value::Object(qm));
        self.run(&mut ex);
        if let Some(e) = &ex.exception {
            return error_response(500, &format!("{}: {}", e.kind, e.message));
        }
        let items: Vec<Value> = match ex.body() {
            Value::Null => Vec::new(),
            Value::Array(a) => a.clone(),
            other => vec![other.clone()],
        };
        let mut out = Vec::new();
        for it in items {
            let ds = match &it {
                Value::Bytes(b) => DicomFile::parse(b, ReadOptions { stop_before_pixel_data: true }).map(|f| f.dataset),
                other => model::value_to_dataset(other, &CharacterSet::default()),
            };
            match ds {
                Ok(mut d) => {
                    d.remove(tags::QUERY_RETRIEVE_LEVEL);
                    out.push(d)
                }
                Err(e) => return error_response(500, &e.message),
            }
        }
        let num = |k: &str| query.iter().find(|(n, _)| n == k).and_then(|(_, v)| v.parse::<usize>().ok());
        let offset = num("offset").unwrap_or(0);
        let limit = num("limit").unwrap_or(usize::MAX);
        let df = DicomJsonDataFormat { bulk_data_threshold: Some(self.bulk_data_threshold), ..Default::default() };
        let arr: Vec<Value> = out.iter().skip(offset).take(limit).map(|d| df.to_json_model(&model::dataset_to_value(d, &CharacterSet::default()))).collect();
        json_response(200, &Value::Array(arr))
    }

    fn retrieve(&self, req: &HttpRequest, uids: &[&&str], op: &str) -> std::result::Result<Vec<Vec<u8>>, HttpResponse> {
        let mut ident = DataSet::new();
        let keys = [tags::STUDY_INSTANCE_UID, tags::SERIES_INSTANCE_UID, tags::SOP_INSTANCE_UID];
        for (k, u) in keys.iter().zip(uids) {
            ident.insert(crate::dimse::ui(*k, u));
        }
        let level = ["STUDY", "SERIES", "IMAGE"][uids.len() - 1];
        ident.insert(Element::string(tags::QUERY_RETRIEVE_LEVEL, Vr::CS, level));
        let mut ex = self.exchange(req, op, model::dataset_to_value(&ident, &CharacterSet::default()));
        ex.set_header(headers::QUERY_LEVEL, level);
        ex.set_header(headers::STUDY_INSTANCE_UID, uids[0].to_string());
        if uids.len() > 1 {
            ex.set_header(headers::SERIES_INSTANCE_UID, uids[1].to_string());
        }
        if uids.len() > 2 {
            ex.set_header(headers::SOP_INSTANCE_UID, uids[2].to_string());
        }
        self.run(&mut ex);
        if let Some(e) = &ex.exception {
            return Err(error_response(500, &format!("{}: {}", e.kind, e.message)));
        }
        let items: Vec<Value> = match ex.body() {
            Value::Null => Vec::new(),
            Value::Array(a) => a.clone(),
            other => vec![other.clone()],
        };
        let files = items.iter().map(body_bytes).collect::<Result<Vec<_>>>().map_err(|e| error_response(500, &e.message))?;
        if files.is_empty() {
            return Err(error_response(404, "No such study, series or instance"));
        }
        Ok(files)
    }

    fn wado(&self, req: &HttpRequest, uids: &[&&str], metadata: bool) -> HttpResponse {
        let files = match self.retrieve(req, uids, if metadata { "metadata" } else { "wado" }) {
            Ok(f) => f,
            Err(r) => return r,
        };
        if metadata {
            let mut arr = Vec::new();
            for f in &files {
                let Ok(file) = DicomFile::parse(f, ReadOptions::default()) else { continue };
                let (s, se, i) = (file.dataset.string(tags::STUDY_INSTANCE_UID).unwrap_or_default(), file.dataset.string(tags::SERIES_INSTANCE_UID).unwrap_or_default(), file.dataset.sop_instance_uid().unwrap_or_default());
                let df = DicomJsonDataFormat {
                    bulk_data_threshold: Some(self.bulk_data_threshold),
                    bulk_data_uri: Some(format!("{}/studies/{}/series/{}/instances/{}/bulkdata/{{tag}}", self.base, s, se, i)),
                    ..Default::default()
                };
                arr.push(df.to_json_model(&model::file_to_value(&file)));
            }
            return json_response(200, &Value::Array(arr));
        }
        // Accept: multipart/related; type="application/dicom"; transfer-syntax=...
        let accept = req.headers.get("Accept").unwrap_or("").to_string();
        if !accept.is_empty() && !accept.contains("*/*") && !accept.to_ascii_lowercase().contains("multipart/related") && !accept.to_ascii_lowercase().contains(APPLICATION_DICOM) {
            return error_response(406, "WADO-RS answers multipart/related; type=\"application/dicom\"");
        }
        let wanted = multipart::param(&accept, "transfer-syntax").filter(|t| t != "*");
        let mut parts = Vec::new();
        for f in files {
            let p = match &wanted {
                Some(t) => {
                    let mut file = match DicomFile::parse(&f, ReadOptions::default()) {
                        Ok(x) => x,
                        Err(e) => return error_response(500, &e.message),
                    };
                    if let Err(e) = transcode(&mut file, t) {
                        return error_response(406, &e.message);
                    }
                    match file.to_bytes() {
                        Ok(b) => b,
                        Err(e) => return error_response(500, &e.message),
                    }
                }
                None => f,
            };
            let ts = DicomFile::parse(&p, ReadOptions { stop_before_pixel_data: true }).map(|x| x.transfer_syntax_uid()).unwrap_or_default();
            parts.push(Part::new(&format!("{}; transfer-syntax={}", APPLICATION_DICOM, ts), p));
        }
        let b = multipart::new_boundary();
        let mut r = HttpResponse::new(200).with_body(multipart::build(&parts, &b));
        r.headers.set("Content-Type", format!("multipart/related; type=\"{}\"; boundary={}", APPLICATION_DICOM, b));
        r
    }

    fn bulkdata(&self, req: &HttpRequest, uids: &[&&str], t: &str) -> HttpResponse {
        let Some(tg) = tag::try_parse(t) else { return error_response(404, "Not a tag") };
        let files = match self.retrieve(req, uids, "wado") {
            Ok(f) => f,
            Err(r) => return r,
        };
        let Ok(file) = DicomFile::parse(&files[0], ReadOptions::default()) else { return error_response(500, "Cannot parse the instance") };
        let Some(el) = file.dataset.get(tg) else { return error_response(404, "No such attribute") };
        let bytes = match &el.value {
            crate::object::Value::Bytes(b) => b.clone(),
            crate::object::Value::Fragments(fr) => fr.iter().skip(1).flatten().copied().collect(),
            _ => return error_response(404, "Not a bulk data attribute"),
        };
        let b = multipart::new_boundary();
        let mut r = HttpResponse::new(200).with_body(multipart::build(&[Part::new("application/octet-stream", bytes)], &b));
        r.headers.set("Content-Type", format!("multipart/related; type=\"application/octet-stream\"; boundary={}", b));
        r
    }
}

impl Service for DicomWebConsumer {
    fn start(&self) -> Result<()> {
        let mut g = self.bound.lock().unwrap();
        if g.is_some() {
            return Ok(());
        }
        let s = &self.settings;
        let server = Arc::new(Server { ctx: self.ctx.clone(), processor: self.processor.clone(), base: s.path.trim_end_matches('/').to_string(), bulk_data_threshold: s.bulk_data_threshold });
        let spec = HttpRouteSpec { path: s.path.clone(), methods: None, match_on_uri_prefix: true, consumes: None, produces: None };
        let handler: cameleer_http::registry::RouteHandler = Arc::new(move |req: &mut HttpRequest, _params: Vec<(String, String)>| server.handle(req));
        let reg = registry().register(&s.host, s.port, &s.server_config, spec, handler)?;
        cameleer_core::camel_info!("dicomweb", "DICOMweb server on port {} at {}", reg.port, s.path);
        *g = Some(reg);
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        if let Some(r) = self.bound.lock().unwrap().take() {
            registry().unregister(&r);
        }
        Ok(())
    }
}

/// The bound port of a started `dicomweb:` consumer.
pub fn local_port(ctx: &CamelContext, uri: &str) -> Option<u16> {
    let ep = ctx.get_endpoint(uri).ok()?;
    ep.as_any().downcast_ref::<DicomWebEndpoint>().and_then(|e| e.local_port())
}

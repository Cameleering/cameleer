//! Elasticsearch / OpenSearch: the Camel `elasticsearch` (and `opensearch`) component over
//! the REST API, plus an embedded test server.
//!
//! `elasticsearch://clusterName?hostAddresses=host:9200&operation=Index&indexName=tweets`
//!
//! Operations (`operation`, header `operation` overrides): `Index` (body = document, id from
//! header `indexId`, returns the id), `Update` (partial document, wrapped in `{"doc": ...}`
//! unless it already has `doc`/`script`/`upsert`; returns the id), `Bulk` (array of documents
//! or of `{"index"|"create"|"update"|"delete": {...}}` actions; returns the item results),
//! `GetById` (body or `indexId` = id), `MultiGet` (array of ids), `Delete` (returns the
//! result, e.g. `deleted`), `DeleteIndex` (returns acknowledged), `Exists` (index exists),
//! `Search` (object → query, wrapped in `{"query": ...}` unless it has `query`; JSON text = full
//! request), `MultiSearch` (array of search requests), `Ping`.
//! With `enableDocumentOnlyMode=true`, `GetById`/`MultiGet`/`Search`/`MultiSearch` return
//! the `_source` documents only; otherwise the response objects (`Search` returns the
//! `hits` object). Headers: `indexName`, `indexId`, `size`, `from`, `operation`.
//!
//! Options: `hostAddresses` (comma list, tried in order), `indexName`, `from`, `size`,
//! `enableSSL` (HTTPS; trust from `sslContextParameters=#bean`, the global `camel.ssl.*`
//! parameters, else the system trust store), `user`/`password` (basic auth), `apiKey`
//! (`Authorization: ApiKey ...`), `connectionTimeout`, `socketTimeout`,
//! `enableDocumentOnlyMode`; accepted and ignored: `maxRetryTimeout`,
//! `certificatePath` (use `sslContextParameters`).
//!
//! Scrolling: `useScroll=true` (with `scrollKeepAliveMs`, default 60000) runs `Search` as a
//! scroll search, fetching every page with `_search/scroll` and clearing the scroll
//! context; the body becomes all hits (or documents) and the exchange property
//! `CamelElasticsearchScrollQueryCount` the number of scroll requests.
//!
//! Sniffing: `enableSniffing=true` replaces `hostAddresses` by the nodes reported by
//! `GET /_nodes/http` (their `http.publish_address`) on first use, every `sniffInterval`
//! ms (default 300000) and `sniffAfterFailureDelay` ms (default 60000) after a connection
//! failure.
//!
//! Typed documents: `documentClass` (option or header) names an [`EsDocumentClass`]: a
//! `#bean`, or an inline field list `name:string,year:int,price:double,active:boolean,...`
//! (types `string`, `int`/`long`, `double`/`float`, `boolean`, `object`, `array`). The
//! `_source` documents returned by `GetById`, `MultiGet`, `Search` and `MultiSearch` are
//! mapped to it: listed fields only, values coerced to the declared types (missing = null).
//! Java class names such as `java.util.Map` / `ObjectNode` keep the generic documents.

pub mod server;

use std::any::Any;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use camel_core::context::CamelContext;
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::{Component, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::value::Value;
use camel_http::client::HttpClient;
use camel_http::http::HttpResponse;

use crate::kinds;
use crate::util::{body_to_json_value, header_i64, header_str};

/// Header names (Camel `ElasticsearchConstants`).
pub mod headers {
    pub const OPERATION: &str = "operation";
    pub const INDEX_NAME: &str = "indexName";
    pub const INDEX_ID: &str = "indexId";
    pub const SIZE: &str = "size";
    pub const FROM: &str = "from";
    pub const DOCUMENT_CLASS: &str = "documentClass";
    pub const WAIT_FOR_ACTIVE_SHARDS: &str = "waitForActiveShards";
}

/// Exchange property with the number of scroll requests of a `useScroll` search.
pub const SCROLL_QUERY_COUNT: &str = "CamelElasticsearchScrollQueryCount";

/// A field type of an [`EsDocumentClass`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    String,
    Long,
    Double,
    Boolean,
    Object,
    Array,
}

/// A typed document shape (the `documentClass` of Camel's component).
#[derive(Clone, Debug, PartialEq)]
pub struct EsDocumentClass {
    pub fields: Vec<(String, FieldKind)>,
}

impl EsDocumentClass {
    /// Parses `name:type,...`.
    pub fn parse(spec: &str) -> Result<EsDocumentClass> {
        let mut fields = Vec::new();
        for part in spec.split(',').map(|p| p.trim()).filter(|p| !p.is_empty()) {
            let (name, ty) = part.split_once(':').unwrap_or((part, "string"));
            let kind = match ty.trim().to_ascii_lowercase().as_str() {
                "string" | "text" | "keyword" | "date" => FieldKind::String,
                "int" | "integer" | "long" | "short" => FieldKind::Long,
                "double" | "float" | "number" => FieldKind::Double,
                "boolean" | "bool" => FieldKind::Boolean,
                "object" | "map" => FieldKind::Object,
                "array" | "list" => FieldKind::Array,
                other => return Err(CamelError::illegal_argument(format!("elasticsearch documentClass: unknown field type '{}'", other))),
            };
            fields.push((name.trim().to_string(), kind));
        }
        if fields.is_empty() {
            return Err(CamelError::illegal_argument("elasticsearch documentClass: no fields"));
        }
        Ok(EsDocumentClass { fields })
    }

    /// Maps a `_source` document to this shape.
    pub fn map(&self, src: &Value) -> Value {
        let out: Vec<(String, Value)> = self.fields.iter().map(|(name, kind)| (name.clone(), coerce(src.get(name).cloned().unwrap_or(Value::Null), *kind))).collect();
        Value::object(out)
    }
}

fn coerce(v: Value, kind: FieldKind) -> Value {
    if v.is_null() {
        return v;
    }
    match kind {
        FieldKind::String => match v {
            Value::String(_) => v,
            Value::Array(_) | Value::Object(_) => Value::String(v.to_json()),
            other => Value::String(other.to_string()),
        },
        FieldKind::Long => match &v {
            Value::Int(_) => v,
            Value::Float(f) => Value::Int(*f as i64),
            Value::Bool(b) => Value::Int(*b as i64),
            other => other.to_string().trim().parse::<i64>().ok().or_else(|| other.to_string().trim().parse::<f64>().ok().map(|f| f as i64)).map(Value::Int).unwrap_or(Value::Null),
        },
        FieldKind::Double => match &v {
            Value::Float(_) => v,
            Value::Int(i) => Value::Float(*i as f64),
            other => other.to_string().trim().parse::<f64>().map(Value::Float).unwrap_or(Value::Null),
        },
        FieldKind::Boolean => match &v {
            Value::Bool(_) => v,
            Value::Int(i) => Value::Bool(*i != 0),
            other => Value::Bool(other.to_string().eq_ignore_ascii_case("true")),
        },
        FieldKind::Object => match v {
            Value::Object(_) => v,
            Value::String(s) => camel_core::json::parse(&s).unwrap_or(Value::String(s)),
            other => other,
        },
        FieldKind::Array => match v {
            Value::Array(_) => v,
            other => Value::Array(vec![other]),
        },
    }
}

/// Generic document classes (no mapping).
fn is_generic_class(name: &str) -> bool {
    let simple = name.rsplit('.').next().unwrap_or(name);
    matches!(simple, "Map" | "HashMap" | "LinkedHashMap" | "ObjectNode" | "JsonNode" | "Object" | "String")
}

/// Sniffing state.
struct Sniffer {
    enabled: bool,
    interval: Duration,
    after_failure: Duration,
    next: Mutex<Option<Instant>>,
}

/// Operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EsOperation {
    Index,
    Update,
    Bulk,
    GetById,
    MultiGet,
    MultiSearch,
    Delete,
    DeleteIndex,
    Search,
    Exists,
    Ping,
}

impl EsOperation {
    pub fn parse(s: &str) -> Result<EsOperation> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "index" => EsOperation::Index,
            "update" => EsOperation::Update,
            "bulk" | "bulkindex" => EsOperation::Bulk,
            "getbyid" => EsOperation::GetById,
            "multiget" => EsOperation::MultiGet,
            "multisearch" => EsOperation::MultiSearch,
            "delete" => EsOperation::Delete,
            "deleteindex" => EsOperation::DeleteIndex,
            "search" => EsOperation::Search,
            "exists" => EsOperation::Exists,
            "ping" => EsOperation::Ping,
            _ => return Err(CamelError::illegal_argument(format!("Unknown elasticsearch operation: {}", s))),
        })
    }
}

/// The `elasticsearch` / `opensearch` component.
pub struct ElasticsearchComponent {
    scheme: String,
}

impl ElasticsearchComponent {
    pub fn new(scheme: &str) -> Self {
        ElasticsearchComponent { scheme: scheme.to_string() }
    }
}

/// Endpoint configuration.
#[derive(Clone)]
pub struct EsConfig {
    pub cluster_name: String,
    /// Current hosts (replaced by sniffing).
    pub hosts: Arc<Mutex<Vec<String>>>,
    pub https: bool,
    pub operation: Option<EsOperation>,
    pub index_name: Option<String>,
    pub from: Option<i64>,
    pub size: Option<i64>,
    pub authorization: Option<String>,
    pub document_only: bool,
    pub client: HttpClient,
    pub document_class: Option<String>,
    pub use_scroll: bool,
    pub scroll_keep_alive_ms: u64,
    sniffer: Arc<Sniffer>,
}

const IGNORED: &[&str] = &["lazyStartProducer", "maxRetryTimeout", "certificatePath", "waitForActiveShards", "connectionManagerMaxConnections"];

impl Component for ElasticsearchComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let hosts: Vec<String> = params.take("hostAddresses").unwrap_or_else(|| "localhost:9200".into()).split(',').map(|h| h.trim().to_string()).filter(|h| !h.is_empty()).collect();
        let explicit = camel_tls::camel_support::take_endpoint_ssl(ctx, params, &self.scheme)?;
        let https = params.take_bool("enableSSL", false)? || explicit.is_some();
        let mut client = HttpClient::new();
        client.connect_timeout = params.take_duration("connectionTimeout", Duration::from_secs(30))?;
        client.read_timeout = params.take_duration("socketTimeout", Duration::from_secs(30))?;
        if https {
            let p = camel_tls::camel_support::effective_client_params(ctx, explicit.as_ref())?;
            client.tls = Some(Arc::new(p.client_config().map_err(|e| CamelError::illegal_argument(format!("TLS client configuration: {}", e)))?));
            client.tls_server_name = p.server_name.clone();
        }
        let user = params.take("user");
        let password = params.take("password");
        let api_key = params.take("apiKey");
        let authorization = match (api_key, user) {
            (Some(k), _) => Some(format!("ApiKey {}", k)),
            (None, Some(u)) => Some(format!("Basic {}", camel_tls::pem::base64_encode(format!("{}:{}", u, password.unwrap_or_default()).as_bytes()))),
            _ => None,
        };
        let operation = match params.take("operation") {
            Some(o) => Some(EsOperation::parse(&o)?),
            None => None,
        };
        let sniffer = Arc::new(Sniffer {
            enabled: params.take_bool("enableSniffing", false)?,
            interval: Duration::from_millis({
                let legacy = params.take_u64("snifferInterval", 300_000)?;
                params.take_u64("sniffInterval", legacy)?
            }),
            after_failure: Duration::from_millis(params.take_u64("sniffAfterFailureDelay", 60_000)?),
            next: Mutex::new(None),
        });
        let document_class = params.take("documentClass").filter(|s| !s.is_empty());
        if let Some(dc) = &document_class {
            resolve_document_class(ctx, dc)?;
        }
        let cfg = EsConfig {
            cluster_name: remaining.to_string(),
            hosts: Arc::new(Mutex::new(hosts)),
            document_class,
            use_scroll: params.take_bool("useScroll", false)?,
            scroll_keep_alive_ms: params.take_u64("scrollKeepAliveMs", 60_000)?,
            sniffer,
            https,
            operation,
            index_name: params.take("indexName"),
            from: params.take("from").map(|v| v.parse()).transpose().map_err(|_| CamelError::illegal_argument("Invalid from"))?,
            size: params.take("size").map(|v| v.parse()).transpose().map_err(|_| CamelError::illegal_argument("Invalid size"))?,
            authorization,
            document_only: params.take_bool("enableDocumentOnlyMode", false)?,
            client,
        };
        for k in IGNORED {
            params.take(k);
        }
        Ok(Arc::new(EsEndpoint { uri: uri.to_string(), cfg, ctx: ctx.downgrade() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// An `elasticsearch` endpoint (producer only).
pub struct EsEndpoint {
    uri: String,
    cfg: EsConfig,
    ctx: camel_core::context::WeakContext,
}

/// Resolves a `documentClass` value (`#bean`, inline spec, or a generic Java class → None).
fn resolve_document_class(ctx: &CamelContext, name: &str) -> Result<Option<EsDocumentClass>> {
    if let Some(bean) = name.strip_prefix('#') {
        let b = ctx.registry().lookup::<EsDocumentClass>(bean).ok_or_else(|| CamelError::no_such_bean(bean))?;
        return Ok(Some((*b).clone()));
    }
    if is_generic_class(name) {
        return Ok(None);
    }
    if !name.contains(':') {
        if let Some(b) = ctx.registry().lookup::<EsDocumentClass>(name) {
            return Ok(Some((*b).clone()));
        }
        return Err(CamelError::illegal_argument(format!("elasticsearch: unknown documentClass '{}' (bind an EsDocumentClass bean or use name:type,...)", name)));
    }
    EsDocumentClass::parse(name).map(Some)
}

impl EsEndpoint {
    pub fn config(&self) -> &EsConfig {
        &self.cfg
    }
}

impl Endpoint for EsEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(EsProducer { cfg: self.cfg.clone(), ctx: self.ctx.clone() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn es_error(resp: &HttpResponse, what: &str) -> CamelError {
    let body = String::from_utf8_lossy(&resp.body).into_owned();
    let reason = camel_core::json::parse(&body).ok().and_then(|v| v.get("error").map(|e| e.get("reason").map(|r| r.to_string()).unwrap_or_else(|| e.to_string()))).unwrap_or(body);
    CamelError::new(kinds::ELASTICSEARCH, format!("{} failed with status {}: {}", what, resp.status, reason))
}

fn map_source(doc: &mut Value, dc: &EsDocumentClass) {
    if let Some(o) = doc.as_object_mut() {
        if let Some(src) = o.get("_source").cloned() {
            o.insert("_source".into(), dc.map(&src));
        }
    }
}

fn enc(s: &str) -> String {
    camel_core::util::url_encode(s)
}

struct EsProducer {
    cfg: EsConfig,
    ctx: camel_core::context::WeakContext,
}

impl EsProducer {
    /// Refreshes the host list from `GET /_nodes/http` when sniffing is due.
    fn sniff_if_due(&self) {
        let sn = &self.cfg.sniffer;
        if !sn.enabled {
            return;
        }
        let due = sn.next.lock().unwrap().map(|t| Instant::now() >= t).unwrap_or(true);
        if !due {
            return;
        }
        *sn.next.lock().unwrap() = Some(Instant::now() + sn.interval);
        let current = self.cfg.hosts.lock().unwrap().clone();
        for host in &current {
            let url = format!("{}://{}/_nodes/http", if self.cfg.https { "https" } else { "http" }, host);
            let mut headers: Vec<(&str, &str)> = vec![("Accept", "application/json")];
            if let Some(a) = &self.cfg.authorization {
                headers.push(("Authorization", a.as_str()));
            }
            let Ok(r) = self.cfg.client.request("GET", &url, &headers, None, None) else { continue };
            if r.status != 200 {
                continue;
            }
            let Ok(v) = camel_core::json::parse(&String::from_utf8_lossy(&r.body)) else { continue };
            let found: Vec<String> = v
                .get("nodes")
                .and_then(|n| n.as_object())
                .map(|o| {
                    o.values()
                        .filter_map(|n| n.get("http").and_then(|h| h.get("publish_address")).map(|a| a.to_string()))
                        // `hostname/ip:port` publish addresses
                        .map(|a| match a.split_once('/') {
                            Some((_, ip)) => ip.to_string(),
                            None => a,
                        })
                        .collect()
                })
                .unwrap_or_default();
            if !found.is_empty() {
                camel_core::camel_debug!("elasticsearch", "Sniffed nodes {:?}", found);
                *self.cfg.hosts.lock().unwrap() = found;
            }
            return;
        }
    }

    fn document_class(&self, ex: &Exchange) -> Result<Option<EsDocumentClass>> {
        let name = header_str(ex, headers::DOCUMENT_CLASS).or_else(|| self.cfg.document_class.clone());
        match (name, self.ctx.upgrade()) {
            (Some(n), Some(ctx)) => resolve_document_class(&ctx, &n),
            (Some(n), None) if n.contains(':') => EsDocumentClass::parse(&n).map(Some),
            _ => Ok(None),
        }
    }

    fn request(&self, method: &str, path: &str, body: Option<(&str, Vec<u8>)>) -> Result<HttpResponse> {
        self.sniff_if_due();
        let mut last = None;
        let hosts = self.cfg.hosts.lock().unwrap().clone();
        for host in &hosts {
            let url = format!("{}://{}{}", if self.cfg.https { "https" } else { "http" }, host, path);
            let mut headers: Vec<(&str, &str)> = vec![("Accept", "application/json")];
            if let Some(a) = &self.cfg.authorization {
                headers.push(("Authorization", a.as_str()));
            }
            if let Some((ct, _)) = &body {
                headers.push(("Content-Type", ct));
            }
            match self.cfg.client.request(method, &url, &headers, body.as_ref().map(|(_, b)| b.as_slice()), None) {
                Ok(r) => return Ok(r),
                Err(e) if e.is_a(camel_core::error::kinds::CONNECT) => {
                    if self.cfg.sniffer.enabled {
                        *self.cfg.sniffer.next.lock().unwrap() = Some(Instant::now() + self.cfg.sniffer.after_failure);
                    }
                    last = Some(e)
                }
                Err(e) => return Err(e),
            }
        }
        Err(last.unwrap_or_else(|| CamelError::illegal_argument("no elasticsearch host")))
    }

    fn json_request(&self, method: &str, path: &str, body: Option<&Value>, what: &str, ok_404: bool) -> Result<(u16, Value)> {
        let r = self.request(method, path, body.map(|b| ("application/json", b.to_json().into_bytes())))?;
        if !(200..300).contains(&r.status) && !(ok_404 && r.status == 404) {
            return Err(es_error(&r, what));
        }
        let v = if r.body.is_empty() { Value::Null } else { camel_core::json::parse(&String::from_utf8_lossy(&r.body))? };
        Ok((r.status, v))
    }

    fn index(&self, ex: &Exchange) -> Result<String> {
        header_str(ex, headers::INDEX_NAME).or_else(|| self.cfg.index_name.clone()).ok_or_else(|| CamelError::illegal_argument("elasticsearch: indexName is not set"))
    }

    fn id(&self, ex: &Exchange, body_fallback: bool) -> Result<String> {
        if let Some(id) = header_str(ex, headers::INDEX_ID) {
            return Ok(id);
        }
        if body_fallback && !ex.body().is_null() {
            return Ok(ex.body().to_string());
        }
        Err(CamelError::illegal_argument("elasticsearch: no document id (indexId header or body)"))
    }

    fn search_request(&self, ex: &Exchange, body: &Value) -> Result<Value> {
        let mut req = match body {
            Value::String(_) | Value::Bytes(_) => body_to_json_value(body)?,
            Value::Object(o) if o.contains_key("query") => body.clone(),
            Value::Object(_) => Value::object([("query", body.clone())]),
            Value::Null => Value::object([("query", Value::object([("match_all", Value::object(Vec::<(String, Value)>::new()))]))]),
            other => return Err(CamelError::illegal_argument(format!("Cannot build a search request from {}", other.type_name()))),
        };
        let size = header_i64(ex, headers::SIZE)?.or(self.cfg.size);
        let from = header_i64(ex, headers::FROM)?.or(self.cfg.from);
        if let Some(o) = req.as_object_mut() {
            if let Some(s) = size {
                o.insert("size".into(), Value::Int(s));
            }
            if let Some(f) = from {
                o.insert("from".into(), Value::Int(f));
            }
        }
        Ok(req)
    }

    fn hits_result(&self, resp: &Value, dc: Option<&EsDocumentClass>) -> Value {
        let mut hits = resp.get("hits").cloned().unwrap_or(Value::Null);
        if let Some(dc) = dc {
            if let Some(list) = hits.as_object_mut().and_then(|o| o.get_mut("hits")).and_then(|h| match h {
                Value::Array(a) => Some(a),
                _ => None,
            }) {
                for h in list.iter_mut() {
                    map_source(h, dc);
                }
            }
        }
        if self.cfg.document_only {
            Value::Array(hits.get("hits").and_then(|h| h.as_array()).map(|a| a.iter().filter_map(|h| h.get("_source").cloned()).collect()).unwrap_or_default())
        } else {
            hits
        }
    }
}

impl Service for EsProducer {}

impl Processor for EsProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let op = match header_str(ex, headers::OPERATION) {
            Some(o) => EsOperation::parse(&o)?,
            None => self.cfg.operation.ok_or_else(|| CamelError::illegal_argument("elasticsearch: operation is not set"))?,
        };
        let dc = self.document_class(ex)?;
        let result = match op {
            EsOperation::Ping => {
                let r = self.request("HEAD", "/", None);
                Value::Bool(matches!(r, Ok(resp) if resp.status == 200))
            }
            EsOperation::Exists => {
                let idx = self.index(ex)?;
                let r = self.request("HEAD", &format!("/{}", enc(&idx)), None)?;
                Value::Bool(r.status == 200)
            }
            EsOperation::DeleteIndex => {
                let idx = self.index(ex)?;
                let (_, v) = self.json_request("DELETE", &format!("/{}", enc(&idx)), None, "DeleteIndex", false)?;
                v.get("acknowledged").cloned().unwrap_or(Value::Bool(false))
            }
            EsOperation::Index => {
                let idx = self.index(ex)?;
                let doc = body_to_json_value(ex.body())?;
                let path = match header_str(ex, headers::INDEX_ID) {
                    Some(id) => format!("/{}/_doc/{}", enc(&idx), enc(&id)),
                    None => format!("/{}/_doc", enc(&idx)),
                };
                let method = if header_str(ex, headers::INDEX_ID).is_some() { "PUT" } else { "POST" };
                let (_, v) = self.json_request(method, &path, Some(&doc), "Index", false)?;
                v.get("_id").cloned().unwrap_or(Value::Null)
            }
            EsOperation::Update => {
                let idx = self.index(ex)?;
                let id = self.id(ex, false)?;
                let doc = body_to_json_value(ex.body())?;
                let body = match &doc {
                    Value::Object(o) if o.contains_key("doc") || o.contains_key("script") || o.contains_key("upsert") => doc.clone(),
                    _ => Value::object([("doc", doc.clone())]),
                };
                let (_, v) = self.json_request("POST", &format!("/{}/_update/{}", enc(&idx), enc(&id)), Some(&body), "Update", false)?;
                v.get("_id").cloned().unwrap_or(Value::Null)
            }
            EsOperation::GetById => {
                let idx = self.index(ex)?;
                let id = self.id(ex, true)?;
                let (_, mut v) = self.json_request("GET", &format!("/{}/_doc/{}", enc(&idx), enc(&id)), None, "GetById", true)?;
                if let Some(dc) = &dc {
                    map_source(&mut v, dc);
                }
                if v.get("error").is_some() {
                    return Err(CamelError::new(kinds::ELASTICSEARCH, format!("GetById failed: {}", v.get("error").map(|e| e.to_json()).unwrap_or_default())));
                }
                if self.cfg.document_only {
                    v.get("_source").cloned().unwrap_or(Value::Null)
                } else {
                    v
                }
            }
            EsOperation::MultiGet => {
                let idx = self.index(ex)?;
                let ids = match body_to_json_value(ex.body())? {
                    Value::Array(a) => a,
                    other => vec![other],
                };
                let docs: Vec<Value> = ids
                    .iter()
                    .map(|i| match i {
                        Value::Object(_) => i.clone(),
                        other => Value::object([("_index", Value::from(idx.as_str())), ("_id", Value::from(other.to_string()))]),
                    })
                    .collect();
                let (_, v) = self.json_request("POST", "/_mget", Some(&Value::object([("docs", Value::Array(docs))])), "MultiGet", false)?;
                let mut items = v.get("docs").and_then(|d| d.as_array()).cloned().unwrap_or_default();
                if let Some(dc) = &dc {
                    items.iter_mut().for_each(|d| map_source(d, dc));
                }
                if self.cfg.document_only {
                    Value::Array(items.iter().map(|d| d.get("_source").cloned().unwrap_or(Value::Null)).collect())
                } else {
                    Value::Array(items)
                }
            }
            EsOperation::Delete => {
                let idx = self.index(ex)?;
                let id = self.id(ex, true)?;
                let (_, v) = self.json_request("DELETE", &format!("/{}/_doc/{}", enc(&idx), enc(&id)), None, "Delete", true)?;
                if v.get("error").is_some() {
                    return Err(CamelError::new(kinds::ELASTICSEARCH, format!("Delete failed: {}", v.to_json())));
                }
                v.get("result").cloned().unwrap_or(Value::Null)
            }
            EsOperation::Search => {
                let req = self.search_request(ex, &ex.body().clone())?;
                let path = match header_str(ex, headers::INDEX_NAME).or_else(|| self.cfg.index_name.clone()) {
                    Some(i) => format!("/{}/_search", enc(&i)),
                    None => "/_search".into(),
                };
                if self.cfg.use_scroll {
                    let keep = format!("{}ms", self.cfg.scroll_keep_alive_ms);
                    let (_, mut v) = self.json_request("POST", &format!("{}?scroll={}", path, keep), Some(&req), "Search", false)?;
                    let mut all: Vec<Value> = Vec::new();
                    let mut queries = 1i64;
                    let mut scroll_id = v.get("_scroll_id").map(|s| s.to_string());
                    loop {
                        let page = v.get("hits").and_then(|h| h.get("hits")).and_then(|h| h.as_array()).cloned().unwrap_or_default();
                        if page.is_empty() {
                            break;
                        }
                        all.extend(page);
                        let Some(id) = scroll_id.clone() else { break };
                        let (_, next) = self.json_request("POST", "/_search/scroll", Some(&Value::object([("scroll", Value::from(keep.as_str())), ("scroll_id", Value::from(id))])), "Scroll", false)?;
                        queries += 1;
                        scroll_id = next.get("_scroll_id").map(|s| s.to_string()).or(scroll_id);
                        v = next;
                    }
                    if let Some(id) = scroll_id {
                        let _ = self.json_request("DELETE", "/_search/scroll", Some(&Value::object([("scroll_id", Value::Array(vec![Value::from(id)]))])), "ClearScroll", true);
                    }
                    ex.set_property(SCROLL_QUERY_COUNT, Value::Int(queries));
                    let resp = Value::object([("hits", Value::object([("hits", Value::Array(all))]))]);
                    let r = self.hits_result(&resp, dc.as_ref());
                    if self.cfg.document_only { r } else { r.get("hits").cloned().unwrap_or(Value::Array(vec![])) }
                } else {
                    let (_, v) = self.json_request("POST", &path, Some(&req), "Search", false)?;
                    self.hits_result(&v, dc.as_ref())
                }
            }
            EsOperation::MultiSearch => {
                let reqs = match body_to_json_value(ex.body())? {
                    Value::Array(a) => a,
                    other => vec![other],
                };
                let default_index = header_str(ex, headers::INDEX_NAME).or_else(|| self.cfg.index_name.clone());
                let mut nd = String::new();
                for r in &reqs {
                    let (idx, body) = match r.get("index") {
                        Some(Value::String(i)) if r.get("body").is_some() => (Some(i.clone()), r.get("body").cloned().unwrap_or(Value::Null)),
                        _ => (default_index.clone(), r.clone()),
                    };
                    let header = match idx {
                        Some(i) => Value::object([("index", i)]),
                        None => Value::object(Vec::<(String, Value)>::new()),
                    };
                    nd.push_str(&header.to_json());
                    nd.push('\n');
                    nd.push_str(&self.search_request(ex, &body)?.to_json());
                    nd.push('\n');
                }
                let r = self.request("POST", "/_msearch", Some(("application/x-ndjson", nd.into_bytes())))?;
                if !(200..300).contains(&r.status) {
                    return Err(es_error(&r, "MultiSearch"));
                }
                let v = camel_core::json::parse(&String::from_utf8_lossy(&r.body))?;
                Value::Array(v.get("responses").and_then(|r| r.as_array()).map(|a| a.iter().map(|resp| self.hits_result(resp, dc.as_ref())).collect()).unwrap_or_default())
            }
            EsOperation::Bulk => {
                let idx = self.index(ex).ok();
                let items = match body_to_json_value(ex.body())? {
                    Value::Array(a) => a,
                    other => vec![other],
                };
                let mut nd = String::new();
                for it in &items {
                    let action = it.as_object().filter(|o| o.len() <= 2).and_then(|o| o.iter().find(|(k, _)| matches!(k.as_str(), "index" | "create" | "update" | "delete")).map(|(k, v)| (k.clone(), v.clone())));
                    match action {
                        Some((op, meta)) => {
                            let mut meta = meta.clone();
                            if let (Some(i), Some(m)) = (&idx, meta.as_object_mut()) {
                                m.entry("_index".to_string()).or_insert_with(|| Value::from(i.as_str()));
                            }
                            nd.push_str(&Value::object([(op.as_str(), meta)]).to_json());
                            nd.push('\n');
                            if op != "delete" {
                                let doc = it.get("doc").or_else(|| it.get("document")).or_else(|| it.get("source")).cloned().unwrap_or(Value::Null);
                                let payload = if op == "update" && doc.get("doc").is_none() { Value::object([("doc", doc)]) } else { doc };
                                nd.push_str(&payload.to_json());
                                nd.push('\n');
                            }
                        }
                        None => {
                            let meta = match &idx {
                                Some(i) => Value::object([("_index", i.as_str())]),
                                None => return Err(CamelError::illegal_argument("elasticsearch Bulk: indexName is not set")),
                            };
                            nd.push_str(&Value::object([("index", meta)]).to_json());
                            nd.push('\n');
                            nd.push_str(&it.to_json());
                            nd.push('\n');
                        }
                    }
                }
                let r = self.request("POST", "/_bulk", Some(("application/x-ndjson", nd.into_bytes())))?;
                if !(200..300).contains(&r.status) {
                    return Err(es_error(&r, "Bulk"));
                }
                let v = camel_core::json::parse(&String::from_utf8_lossy(&r.body))?;
                Value::Array(v.get("items").and_then(|i| i.as_array()).map(|a| a.iter().filter_map(|it| it.as_object().and_then(|o| o.values().next().cloned())).collect()).unwrap_or_default())
            }
        };
        ex.set_body(result);
        Ok(())
    }
}

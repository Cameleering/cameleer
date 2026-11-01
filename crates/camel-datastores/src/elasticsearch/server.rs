//! Embedded Elasticsearch/OpenSearch server for tests: REST/JSON over HTTP(S) with an
//! in-memory inverted index. Supports document CRUD (`_doc`, `_create`, `_update`),
//! `_bulk`, `_search` / `_msearch` / `_count` / `_mget`, index create/delete/exists,
//! queries `match_all`, `match_none`, `match` (`operator`), `match_phrase`, `multi_match`,
//! `term`, `terms`, `range`, `exists`, `ids`, `prefix`, `wildcard`, `bool`
//! (`must`/`filter`/`should`/`must_not`/`minimum_should_match`), `sort`, `from`/`size`,
//! `_source` filtering, scroll searches (`?scroll=` + `_search/scroll`, clear scroll),
//! node info for sniffing (`GET /_nodes/http`, addresses from
//! [`ElasticsearchServer::set_nodes`]) and basic or API-key authentication.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;
use std::sync::{Arc, Mutex};

use camel_core::value::Value;
use camel_http::http::{HttpRequest, HttpResponse};
use camel_http::server::{HttpServer, ServerConfig};
use camel_tls::TlsServerConfig;

/// Server settings.
#[derive(Clone, Default)]
pub struct ElasticsearchServerConfig {
    /// Basic authentication (user, password).
    pub basic_auth: Option<(String, String)>,
    /// Accepted API key (the encoded value sent as `Authorization: ApiKey <value>`).
    pub api_key: Option<String>,
    pub tls: Option<Arc<TlsServerConfig>>,
    /// `version.distribution` reported by `GET /` (`opensearch` or empty for Elasticsearch).
    pub distribution: Option<String>,
}

#[derive(Clone)]
struct Doc {
    source: Value,
    version: i64,
    seq_no: i64,
}

#[derive(Default)]
struct Index {
    docs: BTreeMap<String, Doc>,
    /// field → token → doc id → term frequency
    inverted: HashMap<String, HashMap<String, BTreeMap<String, u32>>>,
    seq: i64,
}

/// Lower-cased alphanumeric tokens (the standard analyzer, roughly).
pub fn analyze(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty()).map(|t| t.to_lowercase()).collect()
}

/// Flattens a document into (dotted field, leaf value) pairs (arrays expanded).
fn flatten(prefix: &str, v: &Value, out: &mut Vec<(String, Value)>) {
    match v {
        Value::Object(o) => {
            for (k, x) in o {
                let p = if prefix.is_empty() { k.clone() } else { format!("{}.{}", prefix, k) };
                flatten(&p, x, out);
            }
        }
        Value::Array(a) => {
            for x in a {
                flatten(prefix, x, out);
            }
        }
        other => out.push((prefix.to_string(), other.clone())),
    }
}

fn field_values(src: &Value, field: &str) -> Vec<Value> {
    let field = field.strip_suffix(".keyword").unwrap_or(field);
    let mut all = Vec::new();
    flatten("", src, &mut all);
    all.into_iter().filter(|(k, _)| k == field).map(|(_, v)| v).collect()
}

impl Index {
    fn add_terms(&mut self, id: &str, src: &Value) {
        let mut fields = Vec::new();
        flatten("", src, &mut fields);
        for (f, v) in fields {
            let tokens = match &v {
                Value::String(s) => analyze(s),
                Value::Null => continue,
                other => vec![other.to_string().to_lowercase()],
            };
            let map = self.inverted.entry(f).or_default();
            for t in tokens {
                *map.entry(t).or_default().entry(id.to_string()).or_insert(0) += 1;
            }
        }
    }

    fn remove_terms(&mut self, id: &str) {
        for map in self.inverted.values_mut() {
            for postings in map.values_mut() {
                postings.remove(id);
            }
            map.retain(|_, p| !p.is_empty());
        }
    }

    fn put(&mut self, id: &str, source: Value) -> (i64, bool) {
        let created = !self.docs.contains_key(id);
        if !created {
            self.remove_terms(id);
        }
        self.seq += 1;
        let version = self.docs.get(id).map(|d| d.version + 1).unwrap_or(1);
        self.add_terms(id, &source);
        self.docs.insert(id.to_string(), Doc { source, version, seq_no: self.seq });
        (version, created)
    }

    fn delete(&mut self, id: &str) -> Option<Doc> {
        let d = self.docs.remove(id)?;
        self.remove_terms(id);
        self.seq += 1;
        Some(d)
    }

    /// Docs containing `token` in `field` with their term frequency.
    fn postings(&self, field: &str, token: &str) -> BTreeMap<String, u32> {
        self.inverted.get(field).and_then(|m| m.get(token)).cloned().unwrap_or_default()
    }

    fn idf(&self, field: &str, token: &str) -> f64 {
        let df = self.postings(field, token).len() as f64;
        (1.0 + (self.docs.len() as f64 - df + 0.5) / (df + 0.5)).ln()
    }
}

struct Shared {
    cfg: ElasticsearchServerConfig,
    indices: Mutex<BTreeMap<String, Index>>,
    requests: Mutex<Vec<String>>,
    auto_id: Mutex<u64>,
    /// scroll id → (remaining hits, page size, total)
    scrolls: Mutex<HashMap<String, (Vec<Value>, usize, usize)>>,
    /// `publish_address`es reported by `GET /_nodes/http` (empty = this server only)
    nodes: Mutex<Vec<String>>,
    self_address: Mutex<String>,
}

/// A running embedded Elasticsearch server (stopped on drop).
pub struct ElasticsearchServer {
    shared: Arc<Shared>,
    http: HttpServer,
}

impl ElasticsearchServer {
    pub fn start() -> io::Result<ElasticsearchServer> {
        Self::start_with(ElasticsearchServerConfig::default())
    }

    pub fn start_with(cfg: ElasticsearchServerConfig) -> io::Result<ElasticsearchServer> {
        let tls = cfg.tls.clone();
        let shared = Arc::new(Shared {
            cfg,
            indices: Mutex::new(BTreeMap::new()),
            requests: Mutex::new(Vec::new()),
            auto_id: Mutex::new(0),
            scrolls: Mutex::new(HashMap::new()),
            nodes: Mutex::new(Vec::new()),
            self_address: Mutex::new(String::new()),
        });
        let sh = shared.clone();
        let config = ServerConfig { name: "es-server".into(), pool_size: 8, tls, ..ServerConfig::default() };
        let http = HttpServer::bind("127.0.0.1:0", config, move |req: &mut HttpRequest| handle(&sh, req))?;
        *shared.self_address.lock().unwrap() = format!("127.0.0.1:{}", http.port());
        Ok(ElasticsearchServer { shared, http })
    }

    /// The cluster nodes (`host:port`) reported by `GET /_nodes/http`.
    pub fn set_nodes(&self, nodes: Vec<String>) {
        *self.shared.nodes.lock().unwrap() = nodes;
    }

    /// Number of open scroll contexts.
    pub fn open_scrolls(&self) -> usize {
        self.shared.scrolls.lock().unwrap().len()
    }

    pub fn port(&self) -> u16 {
        self.http.port()
    }

    /// `127.0.0.1:port`.
    pub fn address(&self) -> String {
        format!("127.0.0.1:{}", self.port())
    }

    /// `METHOD /path` of the requests received.
    pub fn requests(&self) -> Vec<String> {
        self.shared.requests.lock().unwrap().clone()
    }

    /// The source of a document.
    pub fn document(&self, index: &str, id: &str) -> Option<Value> {
        self.shared.indices.lock().unwrap().get(index).and_then(|i| i.docs.get(id)).map(|d| d.source.clone())
    }

    /// Number of documents in an index.
    pub fn count(&self, index: &str) -> usize {
        self.shared.indices.lock().unwrap().get(index).map(|i| i.docs.len()).unwrap_or(0)
    }

    pub fn index_names(&self) -> Vec<String> {
        self.shared.indices.lock().unwrap().keys().cloned().collect()
    }

    pub fn stop(&self) {
        self.http.stop();
    }
}

fn json(status: u16, v: Value) -> HttpResponse {
    HttpResponse::json(status, v.to_json())
}

fn error(status: u16, kind: &str, reason: &str) -> HttpResponse {
    json(
        status,
        Value::object([
            ("error", Value::object([("root_cause", Value::Array(vec![Value::object([("type", kind), ("reason", reason)])])), ("type", Value::from(kind)), ("reason", Value::from(reason))])),
            ("status", Value::Int(status as i64)),
        ]),
    )
}

fn not_found_index(name: &str) -> HttpResponse {
    error(404, "index_not_found_exception", &format!("no such index [{}]", name))
}

fn authorized(cfg: &ElasticsearchServerConfig, req: &HttpRequest) -> bool {
    if cfg.basic_auth.is_none() && cfg.api_key.is_none() {
        return true;
    }
    let Some(h) = req.headers.get("Authorization") else { return false };
    if let (Some((u, p)), Some(b)) = (&cfg.basic_auth, h.strip_prefix("Basic ")) {
        if camel_tls::pem::base64_decode(b.trim()).is_some_and(|d| d == format!("{}:{}", u, p).as_bytes()) {
            return true;
        }
    }
    if let (Some(k), Some(v)) = (&cfg.api_key, h.strip_prefix("ApiKey ")) {
        return v.trim() == k;
    }
    false
}

fn body_json(req: &HttpRequest) -> Result<Value, HttpResponse> {
    if req.body.is_empty() {
        return Ok(Value::Null);
    }
    camel_core::json::parse(&req.body_text()).map_err(|e| error(400, "parse_exception", &e.message))
}

fn ndjson(req: &HttpRequest) -> Result<Vec<Value>, HttpResponse> {
    req.body_text().lines().filter(|l| !l.trim().is_empty()).map(|l| camel_core::json::parse(l).map_err(|e| error(400, "parse_exception", &e.message))).collect()
}

fn handle(sh: &Shared, req: &mut HttpRequest) -> HttpResponse {
    sh.requests.lock().unwrap().push(format!("{} {}", req.method, req.path));
    if !authorized(&sh.cfg, req) {
        return error(401, "security_exception", "missing authentication credentials for REST request").with_header("WWW-Authenticate", "Basic realm=\"security\" charset=\"UTF-8\"");
    }
    let segs: Vec<String> = req.path.split('/').filter(|s| !s.is_empty()).map(String::from).collect();
    let s: Vec<&str> = segs.iter().map(|x| x.as_str()).collect();
    let m = req.method.as_str();
    match (m, s.as_slice()) {
        ("GET", ["_nodes", ..]) => {
            let mut nodes = sh.nodes.lock().unwrap().clone();
            if nodes.is_empty() {
                nodes.push(sh.self_address.lock().unwrap().clone());
            }
            let map: Vec<(String, Value)> = nodes
                .iter()
                .enumerate()
                .map(|(i, a)| (format!("node-{}", i), Value::object([("name", Value::from(format!("node-{}", i))), ("roles", Value::Array(vec!["data".into(), "master".into()])), ("http", Value::object([("publish_address", Value::from(a.as_str())), ("bound_address", Value::Array(vec![Value::from(a.as_str())]))]))])))
                .collect();
            json(200, Value::object([("_nodes", Value::object([("total", Value::Int(nodes.len() as i64)), ("successful", Value::Int(nodes.len() as i64)), ("failed", Value::Int(0))])), ("cluster_name", Value::from("embedded-cluster")), ("nodes", Value::object(map))]))
        }
        ("GET" | "POST", ["_search", "scroll"]) | ("GET" | "POST", ["_search", "scroll", _]) => {
            let body = body_json(req).unwrap_or(Value::Null);
            let id = body.get("scroll_id").map(|v| v.to_string()).or_else(|| s.get(2).map(|x| x.to_string())).or_else(|| req.query_param("scroll_id")).unwrap_or_default();
            let mut scrolls = sh.scrolls.lock().unwrap();
            let Some((rest, size, total)) = scrolls.get_mut(&id) else { return error(404, "search_context_missing_exception", &format!("No search context found for id [{}]", id)) };
            let n = (*size).min(rest.len());
            let page: Vec<Value> = rest.drain(..n).collect();
            let total = *total;
            json(200, Value::object([("_scroll_id", Value::from(id)), ("took", Value::Int(1)), ("timed_out", Value::Bool(false)), ("hits", Value::object([("total", Value::object([("value", Value::Int(total as i64)), ("relation", Value::from("eq"))])), ("hits", Value::Array(page))]))]))
        }
        ("DELETE", ["_search", "scroll", ..]) => {
            let body = body_json(req).unwrap_or(Value::Null);
            let ids: Vec<String> = match body.get("scroll_id") {
                Some(Value::Array(a)) => a.iter().map(|v| v.to_string()).collect(),
                Some(v) => vec![v.to_string()],
                None => s.get(2).map(|x| x.split(',').map(String::from).collect()).unwrap_or_default(),
            };
            let mut scrolls = sh.scrolls.lock().unwrap();
            let freed = if ids.iter().any(|i| i == "_all") {
                let n = scrolls.len();
                scrolls.clear();
                n
            } else {
                ids.iter().filter(|i| scrolls.remove(*i).is_some()).count()
            };
            json(200, Value::object([("succeeded", Value::Bool(true)), ("num_freed", Value::Int(freed as i64))]))
        }
        ("GET" | "HEAD", []) => {
            let mut version = Value::object([("number", "8.11.0"), ("lucene_version", "9.8.0")]);
            if let (Some(d), Some(o)) = (&sh.cfg.distribution, version.as_object_mut()) {
                o.insert("distribution".into(), Value::from(d.as_str()));
                o.insert("number".into(), Value::from("2.11.0"));
            }
            json(200, Value::object([("name", Value::from("embedded")), ("cluster_name", Value::from("embedded-cluster")), ("version", version), ("tagline", Value::from("You Know, for Search"))]))
        }
        ("POST", ["_bulk"]) => bulk(sh, req, None),
        ("POST", [idx, "_bulk"]) => bulk(sh, req, Some(idx)),
        ("GET" | "POST", ["_msearch"]) => msearch(sh, req, None),
        ("GET" | "POST", [idx, "_msearch"]) => msearch(sh, req, Some(idx)),
        ("GET" | "POST", ["_mget"]) => mget(sh, req, None),
        ("GET" | "POST", [idx, "_mget"]) => mget(sh, req, Some(idx)),
        ("GET" | "POST", ["_search"]) => match body_json(req) {
            Ok(b) => search_all(sh, None, &b, req),
            Err(e) => e,
        },
        ("GET" | "POST", [idx, "_search"]) => match body_json(req) {
            Ok(b) => search_all(sh, Some(idx), &b, req),
            Err(e) => e,
        },
        ("GET" | "POST", [idx, "_count"]) => {
            let b = match body_json(req) {
                Ok(b) => b,
                Err(e) => return e,
            };
            let indices = sh.indices.lock().unwrap();
            let Some(i) = indices.get(*idx) else { return not_found_index(idx) };
            match run_query(i, b.get("query").unwrap_or(&Value::object([("match_all", Value::object(Vec::<(String, Value)>::new()))]))) {
                Ok(hits) => json(200, Value::object([("count", hits.len() as i64)])),
                Err(e) => error(400, "parsing_exception", &e),
            }
        }
        ("POST", [_, "_refresh"]) | ("POST", ["_refresh"]) => json(200, Value::object([("_shards", Value::object([("total", 1), ("successful", 1), ("failed", 0)]))])),
        ("HEAD", [idx]) => {
            if sh.indices.lock().unwrap().contains_key(*idx) {
                HttpResponse::new(200)
            } else {
                HttpResponse::new(404)
            }
        }
        ("GET", [idx]) => {
            let indices = sh.indices.lock().unwrap();
            if !indices.contains_key(*idx) {
                return not_found_index(idx);
            }
            json(200, Value::object([(idx.to_string(), Value::object([("mappings", Value::object(Vec::<(String, Value)>::new())), ("settings", Value::object(Vec::<(String, Value)>::new()))]))]))
        }
        ("PUT", [idx]) => {
            let mut indices = sh.indices.lock().unwrap();
            if indices.contains_key(*idx) {
                return error(400, "resource_already_exists_exception", &format!("index [{}] already exists", idx));
            }
            indices.insert(idx.to_string(), Index::default());
            json(200, Value::object([("acknowledged", Value::Bool(true)), ("shards_acknowledged", Value::Bool(true)), ("index", Value::from(*idx))]))
        }
        ("DELETE", [idx]) => {
            let mut indices = sh.indices.lock().unwrap();
            if indices.remove(*idx).is_none() {
                return not_found_index(idx);
            }
            json(200, Value::object([("acknowledged", true)]))
        }
        ("PUT" | "POST", [idx, "_doc"]) | ("PUT" | "POST", [idx, "_doc", _]) | ("PUT" | "POST", [idx, "_create", _]) => {
            let src = match body_json(req) {
                Ok(Value::Object(o)) => Value::Object(o),
                Ok(_) => return error(400, "mapper_parsing_exception", "failed to parse, document is empty or not an object"),
                Err(e) => return e,
            };
            let create = s[1] == "_create" || req.query_param("op_type").as_deref() == Some("create");
            let id = match s.get(2) {
                Some(id) => id.to_string(),
                None => auto_id(sh),
            };
            let mut indices = sh.indices.lock().unwrap();
            let i = indices.entry(idx.to_string()).or_default();
            if create && i.docs.contains_key(&id) {
                return error(409, "version_conflict_engine_exception", &format!("[{}]: version conflict, document already exists", id));
            }
            let (version, created) = i.put(&id, src);
            json(if created { 201 } else { 200 }, write_result(idx, &id, version, if created { "created" } else { "updated" }, i.seq))
        }
        ("GET" | "HEAD", [idx, "_doc", id]) | ("GET", [idx, "_source", id]) => {
            let indices = sh.indices.lock().unwrap();
            let Some(i) = indices.get(*idx) else { return if m == "HEAD" { HttpResponse::new(404) } else { not_found_index(idx) } };
            match i.docs.get(*id) {
                Some(d) if s[1] == "_source" => json(200, d.source.clone()),
                Some(d) => {
                    if m == "HEAD" {
                        return HttpResponse::new(200);
                    }
                    json(200, get_result(idx, id, Some(d)))
                }
                None => {
                    if m == "HEAD" {
                        HttpResponse::new(404)
                    } else {
                        json(404, get_result(idx, id, None))
                    }
                }
            }
        }
        ("DELETE", [idx, "_doc", id]) => {
            let mut indices = sh.indices.lock().unwrap();
            let Some(i) = indices.get_mut(*idx) else { return not_found_index(idx) };
            match i.delete(id) {
                Some(d) => json(200, write_result(idx, id, d.version + 1, "deleted", i.seq)),
                None => json(404, write_result(idx, id, 1, "not_found", i.seq)),
            }
        }
        ("POST", [idx, "_update", id]) => {
            let b = match body_json(req) {
                Ok(b) => b,
                Err(e) => return e,
            };
            let mut indices = sh.indices.lock().unwrap();
            let i = indices.entry(idx.to_string()).or_default();
            match update_doc(i, id, &b) {
                Ok((version, result)) => json(if result == "created" { 201 } else { 200 }, write_result(idx, id, version, result, i.seq)),
                Err(r) => r,
            }
        }
        _ => error(405, "illegal_argument_exception", &format!("Incorrect HTTP method for uri [{}] and method [{}]", req.path, m)),
    }
}

fn auto_id(sh: &Shared) -> String {
    let mut n = sh.auto_id.lock().unwrap();
    *n += 1;
    format!("auto{:010}{}", *n, crate::util::next_id())
}

fn write_result(idx: &str, id: &str, version: i64, result: &str, seq: i64) -> Value {
    Value::object([
        ("_index", Value::from(idx)),
        ("_id", Value::from(id)),
        ("_version", Value::Int(version)),
        ("result", Value::from(result)),
        ("_shards", Value::object([("total", 1), ("successful", 1), ("failed", 0)])),
        ("_seq_no", Value::Int(seq)),
        ("_primary_term", Value::Int(1)),
    ])
}

fn get_result(idx: &str, id: &str, d: Option<&Doc>) -> Value {
    match d {
        Some(d) => Value::object([
            ("_index", Value::from(idx)),
            ("_id", Value::from(id)),
            ("_version", Value::Int(d.version)),
            ("_seq_no", Value::Int(d.seq_no)),
            ("_primary_term", Value::Int(1)),
            ("found", Value::Bool(true)),
            ("_source", d.source.clone()),
        ]),
        None => Value::object([("_index", Value::from(idx)), ("_id", Value::from(id)), ("found", Value::Bool(false))]),
    }
}

fn merge(target: &mut Value, patch: &Value) {
    match (target, patch) {
        (Value::Object(t), Value::Object(p)) => {
            for (k, v) in p {
                match t.get_mut(k) {
                    Some(existing) if matches!(existing, Value::Object(_)) && matches!(v, Value::Object(_)) => merge(existing, v),
                    _ => {
                        t.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (t, p) => *t = p.clone(),
    }
}

fn update_doc(i: &mut Index, id: &str, b: &Value) -> Result<(i64, &'static str), HttpResponse> {
    let doc = b.get("doc");
    let upsert = b.get("upsert");
    let doc_as_upsert = b.get("doc_as_upsert").is_some_and(|v| v.is_truthy());
    match i.docs.get(id).cloned() {
        Some(existing) => {
            let Some(patch) = doc else { return Err(error(400, "action_request_validation_exception", "Validation Failed: 1: script or doc is missing;")) };
            let mut src = existing.source.clone();
            merge(&mut src, patch);
            if src == existing.source {
                return Ok((existing.version, "noop"));
            }
            let (v, _) = i.put(id, src);
            Ok((v, "updated"))
        }
        None => {
            let src = match (upsert, doc) {
                (Some(u), _) => u.clone(),
                (None, Some(d)) if doc_as_upsert => d.clone(),
                _ => return Err(error(404, "document_missing_exception", &format!("[{}]: document missing", id))),
            };
            let (v, _) = i.put(id, src);
            Ok((v, "created"))
        }
    }
}

fn bulk(sh: &Shared, req: &HttpRequest, default_index: Option<&str>) -> HttpResponse {
    let lines = match ndjson(req) {
        Ok(l) => l,
        Err(e) => return e,
    };
    let mut items = Vec::new();
    let mut errors = false;
    let mut it = lines.into_iter();
    while let Some(action) = it.next() {
        let Some((op, meta)) = action.as_object().and_then(|o| o.iter().next()).map(|(k, v)| (k.clone(), v.clone())) else {
            return error(400, "illegal_argument_exception", "Malformed action/metadata line");
        };
        let idx = meta.get("_index").map(|v| v.to_string()).or_else(|| default_index.map(String::from)).unwrap_or_default();
        let id = meta.get("_id").map(|v| v.to_string()).unwrap_or_else(|| auto_id(sh));
        let mut indices = sh.indices.lock().unwrap();
        let i = indices.entry(idx.clone()).or_default();
        let (status, result, version, err): (u16, &str, i64, Option<String>) = match op.as_str() {
            "index" | "create" => {
                let src = it.next().unwrap_or(Value::Null);
                if op == "create" && i.docs.contains_key(&id) {
                    (409, "", 0, Some(format!("[{}]: version conflict, document already exists", id)))
                } else {
                    let (v, created) = i.put(&id, src);
                    (if created { 201 } else { 200 }, if created { "created" } else { "updated" }, v, None)
                }
            }
            "update" => {
                let b = it.next().unwrap_or(Value::Null);
                match update_doc(i, &id, &b) {
                    Ok((v, r)) => (200, r, v, None),
                    Err(_) => (404, "", 0, Some(format!("[{}]: document missing", id))),
                }
            }
            "delete" => match i.delete(&id) {
                Some(d) => (200, "deleted", d.version + 1, None),
                None => (404, "not_found", 1, None),
            },
            other => return error(400, "illegal_argument_exception", &format!("Malformed action/metadata line, expected one of [create, delete, index, update] but found [{}]", other)),
        };
        let mut item = Value::object([("_index", Value::from(idx.as_str())), ("_id", Value::from(id.as_str())), ("status", Value::Int(status as i64))]);
        let o = item.as_object_mut().unwrap();
        match err {
            Some(reason) => {
                errors = true;
                o.insert("error".into(), Value::object([("type", if status == 409 { "version_conflict_engine_exception" } else { "document_missing_exception" }), ("reason", reason.as_str())]));
            }
            None => {
                o.insert("_version".into(), Value::Int(version));
                o.insert("result".into(), Value::from(result));
            }
        }
        items.push(Value::object([(op, item)]));
    }
    json(200, Value::object([("took", Value::Int(1)), ("errors", Value::Bool(errors)), ("items", Value::Array(items))]))
}

fn mget(sh: &Shared, req: &HttpRequest, default_index: Option<&str>) -> HttpResponse {
    let b = match body_json(req) {
        Ok(b) => b,
        Err(e) => return e,
    };
    let mut wanted: Vec<(String, String)> = Vec::new();
    if let Some(Value::Array(ids)) = b.get("ids") {
        for id in ids {
            wanted.push((default_index.unwrap_or("").to_string(), id.to_string()));
        }
    }
    if let Some(Value::Array(docs)) = b.get("docs") {
        for d in docs {
            let idx = d.get("_index").map(|v| v.to_string()).or_else(|| default_index.map(String::from)).unwrap_or_default();
            wanted.push((idx, d.get("_id").map(|v| v.to_string()).unwrap_or_default()));
        }
    }
    let indices = sh.indices.lock().unwrap();
    let docs: Vec<Value> = wanted.iter().map(|(idx, id)| get_result(idx, id, indices.get(idx).and_then(|i| i.docs.get(id)))).collect();
    json(200, Value::object([("docs", Value::Array(docs))]))
}

fn msearch(sh: &Shared, req: &HttpRequest, default_index: Option<&str>) -> HttpResponse {
    let lines = match ndjson(req) {
        Ok(l) => l,
        Err(e) => return e,
    };
    let mut responses = Vec::new();
    for pair in lines.chunks(2) {
        let header = &pair[0];
        let body = pair.get(1).cloned().unwrap_or(Value::Null);
        let idx = header.get("index").map(|v| v.to_string()).or_else(|| default_index.map(String::from));
        let resp = search_all(sh, idx.as_deref(), &body, req);
        let mut v = camel_core::json::parse(&String::from_utf8_lossy(&resp.body)).unwrap_or(Value::Null);
        if let Some(o) = v.as_object_mut() {
            o.insert("status".into(), Value::Int(resp.status as i64));
        }
        responses.push(v);
    }
    json(200, Value::object([("took", Value::Int(1)), ("responses", Value::Array(responses))]))
}

type Hits = BTreeMap<String, f64>;

fn as_obj(v: &Value) -> Option<(&str, &Value)> {
    v.as_object().filter(|o| o.len() == 1).and_then(|o| o.iter().next()).map(|(k, v)| (k.as_str(), v))
}

/// Evaluates a query: matching doc ids with scores.
fn run_query(i: &Index, q: &Value) -> Result<Hits, String> {
    let all = || -> Hits { i.docs.keys().map(|k| (k.clone(), 1.0)).collect() };
    let (kind, body) = as_obj(q).ok_or_else(|| "query malformed, must start with start_object".to_string())?;
    let field_spec = |body: &Value| -> Result<(String, Value), String> { as_obj(body).map(|(f, v)| (f.to_string(), v.clone())).ok_or_else(|| format!("[{}] query malformed", kind)) };
    Ok(match kind {
        "match_all" => all(),
        "match_none" => Hits::new(),
        "match" | "match_phrase" => {
            let (field, spec) = field_spec(body)?;
            let (text, operator) = match &spec {
                Value::Object(o) => (o.get("query").map(|v| v.to_string()).unwrap_or_default(), o.get("operator").map(|v| v.to_string().to_lowercase()).unwrap_or_else(|| "or".into())),
                other => (other.to_string(), "or".into()),
            };
            match_text(i, &[field], &text, kind == "match_phrase" || operator == "and", kind == "match_phrase")
        }
        "multi_match" => {
            let text = body.get("query").map(|v| v.to_string()).unwrap_or_default();
            let fields: Vec<String> = match body.get("fields") {
                Some(Value::Array(a)) => a.iter().map(|f| f.to_string().split('^').next().unwrap_or("").to_string()).collect(),
                _ => i.inverted.keys().cloned().collect(),
            };
            let and = body.get("operator").is_some_and(|o| o.to_string().eq_ignore_ascii_case("and"));
            match_text(i, &fields, &text, and, false)
        }
        "term" | "terms" => {
            let (field, spec) = field_spec(&Value::Object(body.as_object().map(|o| o.iter().filter(|(k, _)| k.as_str() != "boost").map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default()))?;
            let values: Vec<Value> = match (&spec, kind) {
                (Value::Array(a), "terms") => a.clone(),
                (Value::Object(o), "term") => vec![o.get("value").cloned().unwrap_or(Value::Null)],
                (other, _) => vec![other.clone()],
            };
            let mut hits = Hits::new();
            for want in values {
                if let Some(raw) = field.strip_suffix(".keyword") {
                    for (id, d) in &i.docs {
                        if field_values(&d.source, raw).iter().any(|v| v.to_string() == want.to_string()) {
                            hits.insert(id.clone(), 1.0);
                        }
                    }
                } else {
                    let token = want.to_string().to_lowercase();
                    for (id, _) in i.postings(&field, &token) {
                        hits.insert(id, 1.0);
                    }
                    // exact match for non-analyzed values (numbers, booleans, whole strings)
                    for (id, d) in &i.docs {
                        if field_values(&d.source, &field).iter().any(|v| *v == want) {
                            hits.insert(id.clone(), 1.0);
                        }
                    }
                }
            }
            hits
        }
        "range" => {
            let (field, spec) = field_spec(body)?;
            let mut hits = Hits::new();
            for (id, d) in &i.docs {
                let ok = field_values(&d.source, &field).iter().any(|v| {
                    [("gt", std::cmp::Ordering::Greater, false), ("gte", std::cmp::Ordering::Greater, true), ("lt", std::cmp::Ordering::Less, false), ("lte", std::cmp::Ordering::Less, true)].iter().all(|(op, want, eq)| match spec.get(op) {
                        None => true,
                        Some(bound) => match camel_core::value::compare_values(v, bound) {
                            Some(o) => o == *want || (*eq && o == std::cmp::Ordering::Equal),
                            None => false,
                        },
                    })
                });
                if ok {
                    hits.insert(id.clone(), 1.0);
                }
            }
            hits
        }
        "exists" => {
            let field = body.get("field").map(|v| v.to_string()).unwrap_or_default();
            i.docs.iter().filter(|(_, d)| field_values(&d.source, &field).iter().any(|v| !v.is_null())).map(|(k, _)| (k.clone(), 1.0)).collect()
        }
        "ids" => {
            let ids: BTreeSet<String> = match body.get("values") {
                Some(Value::Array(a)) => a.iter().map(|v| v.to_string()).collect(),
                _ => BTreeSet::new(),
            };
            i.docs.keys().filter(|k| ids.contains(*k)).map(|k| (k.clone(), 1.0)).collect()
        }
        "prefix" | "wildcard" => {
            let (field, spec) = field_spec(body)?;
            let pat = match &spec {
                Value::Object(o) => o.get("value").map(|v| v.to_string()).unwrap_or_default(),
                other => other.to_string(),
            };
            let pat = if kind == "prefix" { format!("{}*", pat) } else { pat };
            let field = field.strip_suffix(".keyword").unwrap_or(&field).to_string();
            let mut hits = Hits::new();
            if let Some(m) = i.inverted.get(&field) {
                for (tok, postings) in m {
                    if camel_core::util::wildcard_match(tok, &pat.to_lowercase()) {
                        for id in postings.keys() {
                            hits.insert(id.clone(), 1.0);
                        }
                    }
                }
            }
            for (id, d) in &i.docs {
                if field_values(&d.source, &field).iter().any(|v| camel_core::util::wildcard_match(&v.to_string(), &pat)) {
                    hits.insert(id.clone(), 1.0);
                }
            }
            hits
        }
        "bool" => {
            let clauses = |name: &str| -> Vec<Value> {
                match body.get(name) {
                    Some(Value::Array(a)) => a.clone(),
                    Some(v @ Value::Object(_)) => vec![v.clone()],
                    _ => vec![],
                }
            };
            let (must, filter, should, must_not) = (clauses("must"), clauses("filter"), clauses("should"), clauses("must_not"));
            let mut result: Option<Hits> = None;
            for (c, scoring) in must.iter().map(|c| (c, true)).chain(filter.iter().map(|c| (c, false))) {
                let h = run_query(i, c)?;
                result = Some(match result {
                    None => h.into_iter().map(|(k, s)| (k, if scoring { s } else { 0.0 })).collect(),
                    Some(r) => r.into_iter().filter_map(|(k, s)| h.get(&k).map(|s2| (k, s + if scoring { *s2 } else { 0.0 }))).collect(),
                });
            }
            let min_should = body.get("minimum_should_match").and_then(|v| v.as_i64().or_else(|| v.to_string().parse().ok())).unwrap_or(if must.is_empty() && filter.is_empty() && !should.is_empty() { 1 } else { 0 });
            if !should.is_empty() {
                let results: Vec<Hits> = should.iter().map(|c| run_query(i, c)).collect::<Result<_, _>>()?;
                let base = result.unwrap_or_else(|| all().into_iter().map(|(k, _)| (k, 0.0)).collect());
                result = Some(
                    base.into_iter()
                        .filter_map(|(k, s)| {
                            let matched: Vec<f64> = results.iter().filter_map(|h| h.get(&k).copied()).collect();
                            if (matched.len() as i64) < min_should {
                                None
                            } else {
                                Some((k, s + matched.iter().sum::<f64>()))
                            }
                        })
                        .collect(),
                );
            }
            let mut r = result.unwrap_or_else(|| all().into_iter().map(|(k, _)| (k, 0.0)).collect());
            for c in &must_not {
                let h = run_query(i, c)?;
                r.retain(|k, _| !h.contains_key(k));
            }
            if must.is_empty() && should.is_empty() {
                for s in r.values_mut() {
                    *s = 0.0;
                }
            }
            r
        }
        other => return Err(format!("unknown query [{}]", other)),
    })
}

fn match_text(i: &Index, fields: &[String], text: &str, and: bool, phrase: bool) -> Hits {
    let tokens = analyze(text);
    let mut hits = Hits::new();
    if tokens.is_empty() {
        return hits;
    }
    for field in fields {
        let mut per_doc: BTreeMap<String, (usize, f64)> = BTreeMap::new();
        for t in &tokens {
            let idf = i.idf(field, t);
            for (id, tf) in i.postings(field, t) {
                let e = per_doc.entry(id).or_insert((0, 0.0));
                e.0 += 1;
                e.1 += (tf as f64).sqrt() * idf;
            }
        }
        for (id, (n, score)) in per_doc {
            if and && n < tokens.len() {
                continue;
            }
            if phrase {
                let Some(d) = i.docs.get(&id) else { continue };
                let ok = field_values(&d.source, field).iter().any(|v| {
                    let toks = analyze(&v.to_string());
                    toks.windows(tokens.len()).any(|w| w == tokens.as_slice())
                });
                if !ok {
                    continue;
                }
            }
            let e = hits.entry(id).or_insert(0.0);
            *e = e.max(score);
        }
    }
    hits
}

fn source_filter(src: &Value, spec: Option<&Value>) -> Option<Value> {
    match spec {
        None | Some(Value::Bool(true)) => Some(src.clone()),
        Some(Value::Bool(false)) => None,
        Some(spec) => {
            let includes: Vec<String> = match spec {
                Value::Array(a) => a.iter().map(|v| v.to_string()).collect(),
                Value::String(s) => vec![s.clone()],
                Value::Object(o) => match o.get("includes") {
                    Some(Value::Array(a)) => a.iter().map(|v| v.to_string()).collect(),
                    _ => return Some(src.clone()),
                },
                _ => return Some(src.clone()),
            };
            let o = src.as_object()?;
            Some(Value::Object(o.iter().filter(|(k, _)| includes.iter().any(|p| camel_core::util::wildcard_match(k, p))).map(|(k, v)| (k.clone(), v.clone())).collect()))
        }
    }
}

fn search_all(sh: &Shared, index: Option<&str>, body: &Value, req: &HttpRequest) -> HttpResponse {
    let indices = sh.indices.lock().unwrap();
    let names: Vec<String> = match index {
        Some(spec) if spec != "_all" && spec != "*" => {
            let mut v = Vec::new();
            for n in spec.split(',') {
                if n.contains('*') {
                    v.extend(indices.keys().filter(|k| camel_core::util::wildcard_match(k, n)).cloned());
                } else if indices.contains_key(n) {
                    v.push(n.to_string());
                } else {
                    return not_found_index(n);
                }
            }
            v
        }
        _ => indices.keys().cloned().collect(),
    };
    let default_query = Value::object([("match_all", Value::object(Vec::<(String, Value)>::new()))]);
    let q = body.get("query").unwrap_or(&default_query);
    let mut hits: Vec<(String, String, f64, Value)> = Vec::new();
    for n in &names {
        let i = &indices[n];
        match run_query(i, q) {
            Ok(h) => {
                for (id, score) in h {
                    hits.push((n.clone(), id.clone(), score, i.docs[&id].source.clone()));
                }
            }
            Err(e) => return error(400, "parsing_exception", &e),
        }
    }
    // sorting: explicit sort fields, else score desc then id
    let sort_spec: Vec<(String, bool)> = match body.get("sort") {
        Some(Value::Array(a)) => a
            .iter()
            .map(|s| match s {
                Value::String(f) => (f.clone(), false),
                Value::Object(o) => {
                    let (f, spec) = o.iter().next().map(|(k, v)| (k.clone(), v.clone())).unwrap_or_default();
                    let desc = match &spec {
                        Value::String(d) => d.eq_ignore_ascii_case("desc"),
                        Value::Object(so) => so.get("order").is_some_and(|d| d.to_string().eq_ignore_ascii_case("desc")),
                        _ => false,
                    };
                    (f, desc)
                }
                _ => (String::new(), false),
            })
            .collect(),
        Some(Value::String(f)) => vec![(f.clone(), false)],
        Some(Value::Object(o)) => o.iter().map(|(f, d)| (f.clone(), d.to_string().eq_ignore_ascii_case("desc") || d.get("order").is_some_and(|x| x.to_string().eq_ignore_ascii_case("desc")))).collect(),
        _ => vec![],
    };
    hits.sort_by(|a, b| {
        for (f, desc) in &sort_spec {
            let o = if f == "_score" {
                b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal).reverse()
            } else {
                let va = field_values(&a.3, f).into_iter().next().unwrap_or(Value::Null);
                let vb = field_values(&b.3, f).into_iter().next().unwrap_or(Value::Null);
                camel_core::value::compare_values(&va, &vb).unwrap_or(std::cmp::Ordering::Equal)
            };
            let o = if *desc { o.reverse() } else { o };
            if o != std::cmp::Ordering::Equal {
                return o;
            }
        }
        if sort_spec.is_empty() {
            b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.1.cmp(&b.1))
        } else {
            std::cmp::Ordering::Equal
        }
    });
    let total = hits.len();
    let qp = |k: &str| req.query_param(k).and_then(|v| v.parse::<i64>().ok());
    let from = body.get("from").and_then(|v| v.as_i64()).or_else(|| qp("from")).unwrap_or(0).max(0) as usize;
    let size = body.get("size").and_then(|v| v.as_i64()).or_else(|| qp("size")).unwrap_or(10).max(0) as usize;
    let max_score = hits.iter().map(|h| h.2).fold(None, |m: Option<f64>, s| Some(m.map_or(s, |m| m.max(s))));
    let src_spec = body.get("_source");
    let scroll = req.query_param("scroll").is_some();
    let mut all: Vec<Value> = hits
        .into_iter()
        .skip(from)
        .take(if scroll { usize::MAX } else { size })
        .map(|(idx, id, score, src)| {
            let mut h = Value::object([("_index", Value::from(idx)), ("_id", Value::from(id)), ("_score", Value::Float(score))]);
            if let (Some(s), Some(o)) = (source_filter(&src, src_spec), h.as_object_mut()) {
                o.insert("_source".into(), s);
            }
            h
        })
        .collect();
    let rest = all.split_off(size.min(all.len()));
    let page = all;
    let mut scroll_id = None;
    if scroll {
        let id = format!("scroll-{}", crate::util::next_id());
        sh.scrolls.lock().unwrap().insert(id.clone(), (rest, size.max(1), total));
        scroll_id = Some(id);
    }
    let mut out = Value::object([
            ("took", Value::Int(1)),
            ("timed_out", Value::Bool(false)),
            ("_shards", Value::object([("total", 1), ("successful", 1), ("skipped", 0), ("failed", 0)])),
            (
                "hits",
                Value::object([
                    ("total", Value::object([("value", Value::Int(total as i64)), ("relation", Value::from("eq"))])),
                    ("max_score", max_score.map(Value::Float).unwrap_or(Value::Null)),
                    ("hits", Value::Array(page)),
                ]),
            ),
        ]);
    if let (Some(id), Some(o)) = (scroll_id, out.as_object_mut()) {
        o.insert("_scroll_id".into(), Value::from(id));
    }
    json(200, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyzer_and_index() {
        assert_eq!(analyze("Hello, World! foo-bar 42"), vec!["hello", "world", "foo", "bar", "42"]);
        let mut i = Index::default();
        i.put("1", camel_core::json::parse(r#"{"title":"Quick brown fox","n":5,"tags":["a","b"]}"#).unwrap());
        i.put("2", camel_core::json::parse(r#"{"title":"Lazy brown dog","n":10}"#).unwrap());
        let q = |s: &str| run_query(&i, &camel_core::json::parse(s).unwrap()).unwrap();
        assert_eq!(q(r#"{"match":{"title":"brown"}}"#).len(), 2);
        assert_eq!(q(r#"{"match":{"title":{"query":"brown fox","operator":"and"}}}"#).keys().collect::<Vec<_>>(), vec!["1"]);
        assert_eq!(q(r#"{"match_phrase":{"title":"brown dog"}}"#).keys().collect::<Vec<_>>(), vec!["2"]);
        assert_eq!(q(r#"{"term":{"tags":"b"}}"#).len(), 1);
        assert_eq!(q(r#"{"term":{"title.keyword":"Lazy brown dog"}}"#).len(), 1);
        assert_eq!(q(r#"{"range":{"n":{"gt":5,"lte":10}}}"#).keys().collect::<Vec<_>>(), vec!["2"]);
        assert_eq!(q(r#"{"bool":{"must":[{"match":{"title":"brown"}}],"must_not":[{"term":{"n":5}}]}}"#).keys().collect::<Vec<_>>(), vec!["2"]);
        assert_eq!(q(r#"{"bool":{"should":[{"term":{"n":5}},{"term":{"n":10}}],"minimum_should_match":1}}"#).len(), 2);
        assert_eq!(q(r#"{"prefix":{"title":"qui"}}"#).len(), 1);
        i.delete("1");
        assert_eq!(run_query(&i, &camel_core::json::parse(r#"{"match":{"title":"fox"}}"#).unwrap()).unwrap().len(), 0);
        assert!(run_query(&i, &camel_core::json::parse(r#"{"nope":{}}"#).unwrap()).is_err());
    }
}

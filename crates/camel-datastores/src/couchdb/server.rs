//! Embedded CouchDB server for tests: databases, documents with MVCC revisions and
//! conflicts, `_all_docs`, the `_changes` feed (`normal` and `longpoll`, `include_docs`,
//! `since=now`) and basic authentication.

use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use camel_core::value::Value;
use camel_http::http::{HttpRequest, HttpResponse};
use camel_http::server::{HttpServer, ServerConfig};
use camel_tls::crypto::hash::HashAlg;

use crate::util::hex;

/// Server settings.
#[derive(Clone, Default)]
pub struct CouchDbServerConfig {
    /// Basic authentication (user, password); `None` = admin party.
    pub credentials: Option<(String, String)>,
}

#[derive(Clone)]
struct DocEntry {
    rev_num: u64,
    rev: String,
    body: Value,
    deleted: bool,
    seq: u64,
}

#[derive(Default)]
struct Db {
    docs: BTreeMap<String, DocEntry>,
    seq: u64,
}

struct Shared {
    cfg: CouchDbServerConfig,
    dbs: Mutex<BTreeMap<String, Db>>,
    cv: Condvar,
    log: Mutex<Vec<String>>,
}

/// A running embedded CouchDB server (stopped on drop).
pub struct CouchDbServer {
    shared: Arc<Shared>,
    http: HttpServer,
}

impl CouchDbServer {
    pub fn start() -> io::Result<CouchDbServer> {
        Self::start_with(CouchDbServerConfig::default())
    }

    pub fn start_with(cfg: CouchDbServerConfig) -> io::Result<CouchDbServer> {
        let shared = Arc::new(Shared { cfg, dbs: Mutex::new(BTreeMap::new()), cv: Condvar::new(), log: Mutex::new(Vec::new()) });
        let sh = shared.clone();
        let config = ServerConfig { name: "couchdb-server".into(), pool_size: 16, ..ServerConfig::default() };
        let http = HttpServer::bind("127.0.0.1:0", config, move |req: &mut HttpRequest| handle(&sh, req))?;
        Ok(CouchDbServer { shared, http })
    }

    pub fn port(&self) -> u16 {
        self.http.port()
    }

    /// `http://127.0.0.1:port`.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port())
    }

    /// A live document (with `_id`/`_rev`).
    pub fn document(&self, db: &str, id: &str) -> Option<Value> {
        let dbs = self.shared.dbs.lock().unwrap();
        dbs.get(db).and_then(|d| d.docs.get(id)).filter(|e| !e.deleted).map(|e| with_meta(id, e))
    }

    /// Whether a database exists.
    pub fn has_database(&self, db: &str) -> bool {
        self.shared.dbs.lock().unwrap().contains_key(db)
    }

    /// Number of live documents.
    pub fn doc_count(&self, db: &str) -> usize {
        self.shared.dbs.lock().unwrap().get(db).map(|d| d.docs.values().filter(|e| !e.deleted).count()).unwrap_or(0)
    }

    /// Number of received requests whose `METHOD path?query` contains `needle`.
    pub fn requests_containing(&self, needle: &str) -> usize {
        self.shared.log.lock().unwrap().iter().filter(|r| r.contains(needle)).count()
    }

    pub fn stop(&self) {
        self.http.stop();
    }
}

fn with_meta(id: &str, e: &DocEntry) -> Value {
    let mut m = e.body.as_object().cloned().unwrap_or_default();
    m.insert("_id".into(), Value::from(id));
    m.insert("_rev".into(), Value::from(e.rev.as_str()));
    if e.deleted {
        m.insert("_deleted".into(), Value::Bool(true));
    }
    Value::Object(m)
}

fn json(status: u16, v: Value) -> HttpResponse {
    HttpResponse::json(status, v.to_json())
}

fn err(status: u16, error: &str, reason: &str) -> HttpResponse {
    json(status, Value::object([("error", error), ("reason", reason)]))
}

fn seq_str(n: u64) -> String {
    format!("{}-embedded", n)
}

fn parse_seq(s: &str, current: u64) -> u64 {
    if s == "now" {
        return current;
    }
    s.split('-').next().and_then(|n| n.parse().ok()).unwrap_or(0)
}

fn authorized(cfg: &CouchDbServerConfig, req: &HttpRequest) -> bool {
    let Some((u, p)) = &cfg.credentials else { return true };
    req.headers
        .get("Authorization")
        .and_then(|h| h.strip_prefix("Basic "))
        .and_then(|b| camel_tls::pem::base64_decode(b.trim()))
        .is_some_and(|d| d == format!("{}:{}", u, p).as_bytes())
}

fn new_rev(n: u64, body: &Value) -> String {
    let h = HashAlg::Sha256.hash(format!("{}{}", n, body.to_json()).as_bytes());
    format!("{}-{}", n, &hex(&h)[..32])
}

fn handle(sh: &Shared, req: &mut HttpRequest) -> HttpResponse {
    let q: Vec<String> = req.query_params().into_iter().map(|(k, v)| format!("{}={}", k, v)).collect();
    sh.log.lock().unwrap().push(format!("{} {}?{}", req.method, req.path, q.join("&")));
    if !authorized(&sh.cfg, req) {
        return err(401, "unauthorized", "Name or password is incorrect.");
    }
    let segs: Vec<String> = req.path.split('/').filter(|s| !s.is_empty()).map(String::from).collect();
    let s: Vec<&str> = segs.iter().map(|x| x.as_str()).collect();
    let m = req.method.clone();
    match (m.as_str(), s.as_slice()) {
        ("GET", []) => json(200, Value::object([("couchdb", "Welcome"), ("version", "3.3.3"), ("vendor", "rust-camel embedded")])),
        ("GET", ["_all_dbs"]) => json(200, Value::Array(sh.dbs.lock().unwrap().keys().map(|k| Value::from(k.as_str())).collect())),
        ("PUT", [db]) => {
            let mut dbs = sh.dbs.lock().unwrap();
            if dbs.contains_key(*db) {
                return err(412, "file_exists", "The database could not be created, the file already exists.");
            }
            dbs.insert(db.to_string(), Db::default());
            json(201, Value::object([("ok", true)]))
        }
        ("GET" | "HEAD", [db]) => {
            let dbs = sh.dbs.lock().unwrap();
            let Some(d) = dbs.get(*db) else { return err(404, "not_found", "Database does not exist.") };
            json(200, Value::object([("db_name", Value::from(*db)), ("doc_count", Value::Int(d.docs.values().filter(|e| !e.deleted).count() as i64)), ("doc_del_count", Value::Int(d.docs.values().filter(|e| e.deleted).count() as i64)), ("update_seq", Value::from(seq_str(d.seq)))]))
        }
        ("DELETE", [db]) => {
            if sh.dbs.lock().unwrap().remove(*db).is_none() {
                return err(404, "not_found", "Database does not exist.");
            }
            json(200, Value::object([("ok", true)]))
        }
        ("GET", [db, "_changes"]) | ("POST", [db, "_changes"]) => changes(sh, db, req),
        ("GET", [db, "_all_docs"]) => {
            let dbs = sh.dbs.lock().unwrap();
            let Some(d) = dbs.get(*db) else { return err(404, "not_found", "Database does not exist.") };
            let include = req.query_param("include_docs").as_deref() == Some("true");
            let rows: Vec<Value> = d
                .docs
                .iter()
                .filter(|(_, e)| !e.deleted)
                .map(|(id, e)| {
                    let mut r = Value::object([("id", Value::from(id.as_str())), ("key", Value::from(id.as_str())), ("value", Value::object([("rev", e.rev.as_str())]))]);
                    if include {
                        r.as_object_mut().unwrap().insert("doc".into(), with_meta(id, e));
                    }
                    r
                })
                .collect();
            json(200, Value::object([("total_rows", Value::Int(rows.len() as i64)), ("offset", Value::Int(0)), ("rows", Value::Array(rows))]))
        }
        ("POST", [db, "_bulk_docs"]) => {
            let body = camel_core::json::parse(&req.body_text()).unwrap_or(Value::Null);
            let Some(docs) = body.get("docs").and_then(|d| d.as_array()).cloned() else { return err(400, "bad_request", "POST body must include `docs` parameter.") };
            let mut out = Vec::new();
            for doc in docs {
                if !matches!(doc, Value::Object(_)) {
                    out.push(Value::object([("error", "bad_request"), ("reason", "Document must be a JSON object")]));
                    continue;
                }
                let id = doc.get("_id").map(|v| v.to_string()).unwrap_or_else(|| crate::cassandra::frame::random_uuid().replace('-', ""));
                let r = save(sh, db, &id, doc, None);
                let v = camel_core::json::parse(&String::from_utf8_lossy(&r.body)).unwrap_or(Value::Null);
                out.push(if r.status == 201 {
                    Value::object([("ok", Value::Bool(true)), ("id", Value::from(id)), ("rev", v.get("rev").cloned().unwrap_or(Value::Null))])
                } else {
                    Value::object([("id", Value::from(id)), ("error", v.get("error").cloned().unwrap_or(Value::Null)), ("reason", v.get("reason").cloned().unwrap_or(Value::Null))])
                });
            }
            json(201, Value::Array(out))
        }
        ("POST", [db]) => {
            let body = match camel_core::json::parse(&req.body_text()) {
                Ok(v @ Value::Object(_)) => v,
                _ => return err(400, "bad_request", "Document must be a JSON object"),
            };
            let id = body.get("_id").map(|v| v.to_string()).unwrap_or_else(|| crate::cassandra::frame::random_uuid().replace('-', ""));
            save(sh, db, &id, body, None)
        }
        ("PUT", [db, id]) => {
            let body = match camel_core::json::parse(&req.body_text()) {
                Ok(v @ Value::Object(_)) => v,
                _ => return err(400, "bad_request", "Document must be a JSON object"),
            };
            let rev = req.query_param("rev");
            save(sh, db, id, body, rev)
        }
        ("GET" | "HEAD", [db, id]) => {
            let dbs = sh.dbs.lock().unwrap();
            let Some(d) = dbs.get(*db) else { return err(404, "not_found", "Database does not exist.") };
            match d.docs.get(*id) {
                Some(e) if !e.deleted => json(200, with_meta(id, e)).with_header("ETag", &format!("\"{}\"", e.rev)),
                Some(_) => err(404, "not_found", "deleted"),
                None => err(404, "not_found", "missing"),
            }
        }
        ("DELETE", [db, id]) => {
            let rev = req.query_param("rev").or_else(|| req.headers.get("If-Match").map(|r| r.trim_matches('"').to_string()));
            let mut dbs = sh.dbs.lock().unwrap();
            let Some(d) = dbs.get_mut(*db) else { return err(404, "not_found", "Database does not exist.") };
            let Some(e) = d.docs.get(*id).cloned().filter(|e| !e.deleted) else { return err(404, "not_found", "missing") };
            if rev.as_deref() != Some(e.rev.as_str()) {
                return err(409, "conflict", "Document update conflict.");
            }
            d.seq += 1;
            let n = e.rev_num + 1;
            let r = new_rev(n, &Value::Null);
            d.docs.insert(id.to_string(), DocEntry { rev_num: n, rev: r.clone(), body: Value::object(Vec::<(String, Value)>::new()), deleted: true, seq: d.seq });
            drop(dbs);
            sh.cv.notify_all();
            json(200, Value::object([("ok", Value::Bool(true)), ("id", Value::from(*id)), ("rev", Value::from(r))]))
        }
        _ => err(405, "method_not_allowed", &format!("Only GET,HEAD,POST,PUT,DELETE allowed for {}", req.path)),
    }
}

fn save(sh: &Shared, db: &str, id: &str, mut body: Value, query_rev: Option<String>) -> HttpResponse {
    let mut dbs = sh.dbs.lock().unwrap();
    let Some(d) = dbs.get_mut(db) else { return err(404, "not_found", "Database does not exist.") };
    let o = body.as_object_mut().unwrap();
    let rev = o.remove("_rev").map(|v| v.to_string()).or(query_rev);
    o.remove("_id");
    let deleted = o.remove("_deleted").is_some_and(|v| v.is_truthy());
    let existing = d.docs.get(id).cloned();
    let n = match &existing {
        Some(e) if !e.deleted => {
            if rev.as_deref() != Some(e.rev.as_str()) {
                return err(409, "conflict", "Document update conflict.");
            }
            e.rev_num + 1
        }
        Some(e) => e.rev_num + 1,
        None => 1,
    };
    d.seq += 1;
    let r = new_rev(n, &body);
    d.docs.insert(id.to_string(), DocEntry { rev_num: n, rev: r.clone(), body, deleted, seq: d.seq });
    drop(dbs);
    sh.cv.notify_all();
    json(201, Value::object([("ok", Value::Bool(true)), ("id", Value::from(id)), ("rev", Value::from(r))]))
}

fn changes(sh: &Shared, db: &str, req: &HttpRequest) -> HttpResponse {
    let include_docs = req.query_param("include_docs").as_deref() == Some("true");
    let longpoll = req.query_param("feed").as_deref() == Some("longpoll");
    let timeout = Duration::from_millis(req.query_param("timeout").and_then(|t| t.parse().ok()).unwrap_or(60_000).min(60_000));
    let limit = req.query_param("limit").and_then(|l| l.parse::<usize>().ok()).unwrap_or(usize::MAX);
    let deadline = Instant::now() + timeout;
    let mut dbs = sh.dbs.lock().unwrap();
    let Some(cur) = dbs.get(db).map(|d| d.seq) else { return err(404, "not_found", "Database does not exist.") };
    let since = parse_seq(&req.query_param("since").unwrap_or_else(|| "0".into()), cur);
    loop {
        let Some(d) = dbs.get(db) else { return err(404, "not_found", "Database does not exist.") };
        let mut entries: Vec<(&String, &DocEntry)> = d.docs.iter().filter(|(_, e)| e.seq > since).collect();
        entries.sort_by_key(|(_, e)| e.seq);
        entries.truncate(limit);
        let now = Instant::now();
        if !entries.is_empty() || !longpoll || now >= deadline {
            let last = entries.last().map(|(_, e)| e.seq).unwrap_or(since);
            let results: Vec<Value> = entries
                .iter()
                .map(|(id, e)| {
                    let mut r = Value::object([("seq", Value::from(seq_str(e.seq))), ("id", Value::from(id.as_str())), ("changes", Value::Array(vec![Value::object([("rev", e.rev.as_str())])]))]);
                    let o = r.as_object_mut().unwrap();
                    if e.deleted {
                        o.insert("deleted".into(), Value::Bool(true));
                    }
                    if include_docs {
                        o.insert("doc".into(), with_meta(id, e));
                    }
                    r
                })
                .collect();
            return json(200, Value::object([("results", Value::Array(results)), ("last_seq", Value::from(seq_str(last))), ("pending", Value::Int(0))]));
        }
        dbs = sh.cv.wait_timeout(dbs, (deadline - now).min(Duration::from_millis(100))).unwrap().0;
    }
}

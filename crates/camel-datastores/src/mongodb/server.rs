//! Embedded MongoDB server for tests (in memory, `OP_MSG` + legacy `OP_QUERY` handshake):
//! `hello`, SCRAM-SHA-256 authentication, TLS, CRUD commands (`find`, `getMore`,
//! `insert`, `update`, `delete`, `count`, `distinct`, `aggregate`, `findAndModify`),
//! capped collections with tailable/awaitData cursors, change streams
//! (`$changeStream`), and admin commands (`create`, `drop`, `listCollections`,
//! `listDatabases`, `dbStats`, `collStats`, `dropDatabase`, `buildInfo`, `ping`).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{self, BufReader, Write};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use camel_tls::net::NetStream;
use camel_tls::sasl::{ScramCredentials, ScramMechanism, ScramServer};
use camel_tls::TlsServerConfig;

use super::bson::{Bson, Document, ObjectId};
use super::query::{self, QueryError};
use super::wire::{decode_op_msg, decode_op_query, encode_op_msg, encode_op_reply, read_message, OpMsg, FLAG_MORE_TO_COME, OP_MSG, OP_QUERY};
use crate::util::{now_millis, TcpServer};

/// Server settings.
#[derive(Clone, Default)]
pub struct MongoServerConfig {
    /// Users (name → password) accepted with SCRAM-SHA-256 (any auth database).
    /// When empty, authentication is not required.
    pub users: BTreeMap<String, String>,
    pub tls: Option<Arc<TlsServerConfig>>,
}

struct Collection {
    docs: Vec<(u64, Document)>,
    /// (max bytes, max documents)
    capped: Option<(i64, Option<i64>)>,
}

impl Collection {
    fn new() -> Collection {
        Collection { docs: Vec::new(), capped: None }
    }
}

#[derive(Clone)]
struct ChangeEvent {
    seq: u64,
    db: String,
    coll: String,
    op: &'static str,
    key: Bson,
    full: Option<Document>,
    update: Option<Document>,
}

enum CursorKind {
    Static(VecDeque<Document>),
    Tailable { filter: Document, last_seq: u64 },
    ChangeStream { all_colls: bool, pipeline: Vec<Bson>, next_event: u64, lookup: bool },
}

struct ServerCursor {
    db: String,
    coll: String,
    kind: CursorKind,
}

#[derive(Default)]
struct Store {
    dbs: BTreeMap<String, BTreeMap<String, Collection>>,
    cursors: HashMap<i64, ServerCursor>,
    oplog: Vec<ChangeEvent>,
    seq: u64,
}

struct Shared {
    creds: HashMap<String, ScramCredentials>,
    auth_required: bool,
    store: Mutex<Store>,
    cv: Condvar,
    next_cursor: AtomicI64,
    commands: Mutex<Vec<String>>,
}

/// A running embedded MongoDB server (stopped on drop).
pub struct MongoServer {
    shared: Arc<Shared>,
    tcp: TcpServer,
}

type R = std::result::Result<Document, QueryError>;

fn err(msg: impl Into<String>, code: i32) -> QueryError {
    QueryError(msg.into(), code)
}

fn code_name(code: i32) -> &'static str {
    match code {
        2 => "BadValue",
        9 => "FailedToParse",
        13 => "Unauthorized",
        18 => "AuthenticationFailed",
        26 => "NamespaceNotFound",
        43 => "CursorNotFound",
        48 => "NamespaceExists",
        59 => "CommandNotFound",
        66 => "ImmutableField",
        11000 => "DuplicateKey",
        _ => "Location",
    }
}

fn error_doc(e: &QueryError) -> Document {
    Document::new().with("ok", 0.0).with("errmsg", e.0.as_str()).with("code", e.1).with("codeName", code_name(e.1))
}

impl MongoServer {
    /// Starts a server without authentication.
    pub fn start() -> io::Result<MongoServer> {
        Self::start_with(MongoServerConfig::default())
    }

    pub fn start_with(cfg: MongoServerConfig) -> io::Result<MongoServer> {
        let creds = cfg
            .users
            .iter()
            .map(|(u, p)| (u.clone(), ScramCredentials::derive(ScramMechanism::Sha256, p, format!("salt-{}", u).as_bytes(), 4096)))
            .collect();
        let shared = Arc::new(Shared {
            creds,
            auth_required: !cfg.users.is_empty(),
            store: Mutex::new(Store::default()),
            cv: Condvar::new(),
            next_cursor: AtomicI64::new(1000),
            commands: Mutex::new(Vec::new()),
        });
        let sh = shared.clone();
        let tcp = TcpServer::start("mongo-server", cfg.tls.clone(), move |s| {
            let _ = serve(&sh, s);
        })?;
        Ok(MongoServer { shared, tcp })
    }

    pub fn port(&self) -> u16 {
        self.tcp.port()
    }

    /// `mongodb://127.0.0.1:port` (add credentials/options as needed).
    pub fn connection_string(&self) -> String {
        format!("mongodb://127.0.0.1:{}", self.port())
    }

    /// Names of the commands received so far (in order).
    pub fn commands(&self) -> Vec<String> {
        self.shared.commands.lock().unwrap().clone()
    }

    /// Number of open cursors.
    pub fn open_cursors(&self) -> usize {
        self.shared.store.lock().unwrap().cursors.len()
    }

    /// Documents of a collection (insertion order).
    pub fn documents(&self, db: &str, coll: &str) -> Vec<Document> {
        let st = self.shared.store.lock().unwrap();
        st.dbs.get(db).and_then(|d| d.get(coll)).map(|c| c.docs.iter().map(|(_, d)| d.clone()).collect()).unwrap_or_default()
    }

    /// Inserts documents directly.
    pub fn insert(&self, db: &str, coll: &str, docs: Vec<Document>) {
        let mut st = self.shared.store.lock().unwrap();
        for d in docs {
            let _ = insert_doc(&mut st, db, coll, d);
        }
        drop(st);
        self.shared.cv.notify_all();
    }

    /// Number of open client connections.
    pub fn connection_count(&self) -> usize {
        self.tcp.connection_count()
    }

    pub fn stop(&self) {
        self.tcp.stop();
    }
}

struct Conn {
    authed: bool,
    scram: Option<ScramServer<Box<dyn Fn(&str) -> Option<ScramCredentials> + Send>>>,
}

fn serve(sh: &Arc<Shared>, stream: NetStream) -> io::Result<()> {
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let mut conn = Conn { authed: !sh.auth_required, scram: None };
    let mut reply_id = 1i32;
    loop {
        let (h, body) = read_message(&mut reader)?;
        reply_id += 1;
        match h.op_code {
            OP_MSG => {
                let msg = match decode_op_msg(&body) {
                    Ok(m) => m,
                    Err(e) => {
                        let r = error_doc(&err(e.message, 2));
                        writer.write_all(&encode_op_msg(reply_id, h.request_id, &OpMsg::new(r)))?;
                        continue;
                    }
                };
                let cmd = msg.merged();
                let reply = dispatch(sh, &mut conn, cmd);
                if msg.flags & FLAG_MORE_TO_COME == 0 {
                    writer.write_all(&encode_op_msg(reply_id, h.request_id, &OpMsg::new(reply)))?;
                    writer.flush()?;
                }
            }
            OP_QUERY => {
                let reply = match decode_op_query(&body) {
                    Ok(q) => {
                        let db = q.collection.split('.').next().unwrap_or("admin").to_string();
                        let mut cmd = q.query.get_doc("$query").cloned().unwrap_or(q.query);
                        cmd.insert("$db", db);
                        dispatch(sh, &mut conn, cmd)
                    }
                    Err(e) => error_doc(&err(e.message, 2)),
                };
                writer.write_all(&encode_op_reply(reply_id, h.request_id, &[reply]))?;
                writer.flush()?;
            }
            other => {
                let r = error_doc(&err(format!("Unsupported opcode {}", other), 2));
                writer.write_all(&encode_op_msg(reply_id, h.request_id, &OpMsg::new(r)))?;
            }
        }
    }
}

fn dispatch(sh: &Shared, conn: &mut Conn, cmd: Document) -> Document {
    let Some(name) = cmd.keys().next().map(String::from) else { return error_doc(&err("empty command", 9)) };
    sh.commands.lock().unwrap().push(name.clone());
    let db = cmd.get_str("$db").unwrap_or("admin").to_string();
    let open = matches!(name.as_str(), "hello" | "isMaster" | "ismaster" | "ping" | "buildInfo" | "buildinfo" | "saslStart" | "saslContinue" | "endSessions" | "whatsmyuri");
    if !conn.authed && !open {
        return error_doc(&err(format!("command {} requires authentication", name), 13));
    }
    let r = match name.as_str() {
        "hello" | "isMaster" | "ismaster" => Ok(hello(sh, &cmd, &name)),
        "ping" | "endSessions" | "createIndexes" | "getLastError" | "whatsmyuri" => Ok(Document::new()),
        "buildInfo" | "buildinfo" => Ok(Document::new().with("version", "7.0.0").with("versionArray", vec![Bson::Int32(7), Bson::Int32(0), Bson::Int32(0), Bson::Int32(0)])),
        "saslStart" | "saslContinue" => sasl(sh, conn, &cmd, &name),
        _ => data_command(sh, &db, &name, &cmd),
    };
    match r {
        Ok(mut d) => {
            d.insert("ok", 1.0);
            d
        }
        Err(e) => error_doc(&e),
    }
}

fn hello(sh: &Shared, cmd: &Document, name: &str) -> Document {
    let mut d = Document::new()
        .with(if name == "hello" { "isWritablePrimary" } else { "ismaster" }, true)
        .with("topologyVersion", Document::new().with("processId", ObjectId::new()).with("counter", Bson::Int64(0)))
        .with("maxBsonObjectSize", 16 * 1024 * 1024)
        .with("maxMessageSizeBytes", 48_000_000)
        .with("maxWriteBatchSize", 100_000)
        .with("localTime", Bson::DateTime(now_millis()))
        .with("logicalSessionTimeoutMinutes", 30)
        .with("connectionId", 1)
        .with("minWireVersion", 0)
        .with("maxWireVersion", 21)
        .with("readOnly", false);
    if let Some(u) = cmd.get_str("saslSupportedMechs") {
        let user = u.split_once('.').map(|(_, u)| u).unwrap_or(u);
        if sh.creds.contains_key(user) {
            d.insert("saslSupportedMechs", vec![Bson::from("SCRAM-SHA-256")]);
        }
    }
    d
}

fn payload(cmd: &Document) -> String {
    match cmd.get("payload") {
        Some(Bson::Binary(_, b)) => String::from_utf8_lossy(b).into_owned(),
        Some(Bson::String(s)) => s.clone(),
        _ => String::new(),
    }
}

fn sasl(sh: &Shared, conn: &mut Conn, cmd: &Document, name: &str) -> R {
    let fail = || err("Authentication failed.", 18);
    if name == "saslStart" {
        if cmd.get_str("mechanism") != Some("SCRAM-SHA-256") {
            return Err(err(format!("Received authentication for mechanism {} which is not enabled", cmd.get_str("mechanism").unwrap_or("")), 334));
        }
        let creds = sh.creds.clone();
        let lookup: Box<dyn Fn(&str) -> Option<ScramCredentials> + Send> = Box::new(move |u: &str| creds.get(u).cloned());
        let mut s = ScramServer::new(ScramMechanism::Sha256, lookup);
        let first = s.handle_client_first(&payload(cmd)).map_err(|_| fail())?;
        conn.scram = Some(s);
        return Ok(Document::new().with("conversationId", 1).with("done", false).with("payload", Bson::Binary(0, first.into_bytes())));
    }
    if conn.authed && conn.scram.is_none() {
        return Ok(Document::new().with("conversationId", 1).with("done", true).with("payload", Bson::Binary(0, Vec::new())));
    }
    let mut s = conn.scram.take().ok_or_else(|| err("No SASL session state found", 17))?;
    let fin = s.handle_client_final(&payload(cmd)).map_err(|_| fail())?;
    conn.authed = true;
    Ok(Document::new().with("conversationId", 1).with("done", true).with("payload", Bson::Binary(0, fin.into_bytes())))
}

fn coll_name(cmd: &Document, name: &str) -> std::result::Result<String, QueryError> {
    match cmd.get(name) {
        Some(Bson::String(s)) if !s.is_empty() => Ok(s.clone()),
        _ => Err(err(format!("collection name has invalid type for {}", name), 73)),
    }
}

fn doc_size(d: &Document) -> i64 {
    d.to_bytes().len() as i64
}

fn insert_doc(st: &mut Store, db: &str, coll: &str, mut d: Document) -> std::result::Result<(), QueryError> {
    if !d.contains_key("_id") {
        d.insert_first("_id", ObjectId::new());
    }
    let id = d.get("_id").cloned().unwrap();
    st.seq += 1;
    let seq = st.seq;
    let c = st.dbs.entry(db.to_string()).or_default().entry(coll.to_string()).or_insert_with(Collection::new);
    if c.docs.iter().any(|(_, x)| x.get("_id").is_some_and(|v| v.eq_bson(&id))) {
        return Err(err(format!("E11000 duplicate key error collection: {}.{} index: _id_ dup key: {{ _id: {} }}", db, coll, id.to_value().to_json()), 11000));
    }
    c.docs.push((seq, d.clone()));
    if let Some((size, max)) = c.capped {
        while max.is_some_and(|m| c.docs.len() as i64 > m) || (c.docs.len() > 1 && c.docs.iter().map(|(_, d)| doc_size(d)).sum::<i64>() > size) {
            c.docs.remove(0);
        }
    }
    st.oplog.push(ChangeEvent { seq, db: db.into(), coll: coll.into(), op: "insert", key: id, full: Some(d), update: None });
    Ok(())
}

fn batch_size(cmd: &Document, key: &str, default: usize) -> usize {
    match cmd.get(key) {
        Some(Bson::Document(c)) => c.get_i64("batchSize").map(|b| b.max(0) as usize).unwrap_or(default),
        Some(v) => v.as_i64().map(|b| if b <= 0 { default } else { b as usize }).unwrap_or(default),
        None => default,
    }
}

fn cursor_reply(id: i64, ns: &str, field: &str, docs: Vec<Document>, extra: Option<(&str, Bson)>) -> Document {
    let mut c = Document::new().with("id", Bson::Int64(id)).with("ns", ns).with(field, Bson::Array(docs.into_iter().map(Bson::Document).collect()));
    if let Some((k, v)) = extra {
        c.insert(k, v);
    }
    Document::new().with("cursor", c)
}

fn new_static_cursor(sh: &Shared, st: &mut Store, db: &str, coll: &str, mut docs: Vec<Document>, first: usize, single: bool) -> Document {
    let ns = format!("{}.{}", db, coll);
    if single {
        docs.truncate(first);
        return cursor_reply(0, &ns, "firstBatch", docs, None);
    }
    if docs.len() <= first {
        return cursor_reply(0, &ns, "firstBatch", docs, None);
    }
    let rest: VecDeque<Document> = docs.split_off(first).into();
    let id = sh.next_cursor.fetch_add(1, Ordering::SeqCst);
    st.cursors.insert(id, ServerCursor { db: db.into(), coll: coll.into(), kind: CursorKind::Static(rest) });
    cursor_reply(id, &ns, "firstBatch", docs, None)
}

fn matching(st: &Store, db: &str, coll: &str, filter: &Document) -> std::result::Result<Vec<(u64, Document)>, QueryError> {
    let mut out = Vec::new();
    if let Some(c) = st.dbs.get(db).and_then(|d| d.get(coll)) {
        for (s, d) in &c.docs {
            if query::matches(d, filter)? {
                out.push((*s, d.clone()));
            }
        }
    }
    Ok(out)
}

fn token(seq: u64) -> Document {
    Document::new().with("_data", format!("{:016x}", seq))
}

fn change_doc(e: &ChangeEvent, st: &Store, lookup: bool) -> Document {
    let mut d = Document::new()
        .with("_id", token(e.seq))
        .with("operationType", e.op)
        .with("clusterTime", Bson::Timestamp((now_millis() / 1000) as u32, e.seq as u32))
        .with("wallTime", Bson::DateTime(now_millis()))
        .with("ns", Document::new().with("db", e.db.as_str()).with("coll", e.coll.as_str()))
        .with("documentKey", Document::new().with("_id", e.key.clone()));
    match e.op {
        "insert" | "replace" => {
            if let Some(f) = &e.full {
                d.insert("fullDocument", f.clone());
            }
        }
        "update" => {
            if let Some(u) = &e.update {
                d.insert("updateDescription", u.clone());
            }
            if lookup {
                let cur = st.dbs.get(&e.db).and_then(|x| x.get(&e.coll)).and_then(|c| c.docs.iter().find(|(_, x)| x.get("_id").is_some_and(|v| v.eq_bson(&e.key)))).map(|(_, x)| x.clone());
                d.insert("fullDocument", cur.map(Bson::Document).unwrap_or(Bson::Null));
            }
        }
        _ => {}
    }
    d
}

/// Collects new results of a tailable/change-stream cursor (None = cursor gone).
fn poll_cursor(st: &mut Store, id: i64, limit: usize) -> std::result::Result<Option<(Vec<Document>, Option<Document>)>, QueryError> {
    let Some(cur) = st.cursors.get(&id) else { return Ok(None) };
    let (db, coll) = (cur.db.clone(), cur.coll.clone());
    match &cur.kind {
        CursorKind::Static(_) => Ok(Some((Vec::new(), None))),
        CursorKind::Tailable { filter, last_seq } => {
            let (filter, last) = (filter.clone(), *last_seq);
            let mut out = Vec::new();
            let mut new_last = last;
            if let Some(c) = st.dbs.get(&db).and_then(|d| d.get(&coll)) {
                for (s, d) in c.docs.iter().filter(|(s, _)| *s > last) {
                    if out.len() >= limit {
                        break;
                    }
                    new_last = *s;
                    if query::matches(d, &filter)? {
                        out.push(d.clone());
                    }
                }
            }
            if let Some(ServerCursor { kind: CursorKind::Tailable { last_seq, .. }, .. }) = st.cursors.get_mut(&id) {
                *last_seq = new_last;
            }
            Ok(Some((out, None)))
        }
        CursorKind::ChangeStream { all_colls, pipeline, next_event, lookup } => {
            let (all, pipeline, start, lookup) = (*all_colls, pipeline.clone(), *next_event, *lookup);
            let mut events = Vec::new();
            let mut last = start;
            for e in st.oplog.iter().filter(|e| e.seq >= start && e.db == db && (all || e.coll == coll)) {
                if events.len() >= limit {
                    break;
                }
                events.push(change_doc(e, st, lookup));
                last = e.seq + 1;
            }
            if events.is_empty() {
                last = st.seq + 1;
            }
            let out = query::aggregate(events, &pipeline)?;
            if let Some(ServerCursor { kind: CursorKind::ChangeStream { next_event, .. }, .. }) = st.cursors.get_mut(&id) {
                *next_event = last;
            }
            Ok(Some((out, Some(token(last.saturating_sub(1))))))
        }
    }
}

fn diff(old: &Document, new: &Document) -> Document {
    let mut updated = Document::new();
    for (k, v) in new.iter() {
        if old.get(k).is_none_or(|o| !o.eq_bson(v)) {
            updated.insert(k.clone(), v.clone());
        }
    }
    let removed: Vec<Bson> = old.keys().filter(|k| !new.contains_key(k)).map(Bson::from).collect();
    Document::new().with("updatedFields", updated).with("removedFields", Bson::Array(removed))
}

fn data_command(sh: &Shared, db: &str, name: &str, cmd: &Document) -> R {
    let mut st = sh.store.lock().unwrap();
    match name {
        "find" => {
            let coll = coll_name(cmd, "find")?;
            let filter = cmd.get_doc("filter").cloned().unwrap_or_default();
            if cmd.get_bool("tailable").unwrap_or(false) {
                let capped = st.dbs.get(db).and_then(|d| d.get(&coll)).map(|c| c.capped.is_some());
                if capped != Some(true) {
                    return Err(err(format!("error processing query: ns={}.{} tailable cursor requested on non capped collection", db, coll), 2));
                }
                let found = matching(&st, db, &coll, &filter)?;
                let last = st.dbs.get(db).and_then(|d| d.get(&coll)).and_then(|c| c.docs.last()).map(|(s, _)| *s).unwrap_or(0);
                let id = sh.next_cursor.fetch_add(1, Ordering::SeqCst);
                st.cursors.insert(id, ServerCursor { db: db.into(), coll: coll.clone(), kind: CursorKind::Tailable { filter, last_seq: last } });
                return Ok(cursor_reply(id, &format!("{}.{}", db, coll), "firstBatch", found.into_iter().map(|(_, d)| d).collect(), None));
            }
            let mut docs: Vec<Document> = matching(&st, db, &coll, &filter)?.into_iter().map(|(_, d)| d).collect();
            if let Some(s) = cmd.get_doc("sort").filter(|s| !s.is_empty()) {
                query::sort_docs(&mut docs, s);
            }
            let skip = cmd.get_i64("skip").unwrap_or(0).max(0) as usize;
            let mut docs: Vec<Document> = docs.into_iter().skip(skip).collect();
            let limit = cmd.get_i64("limit").unwrap_or(0);
            if limit != 0 {
                docs.truncate(limit.unsigned_abs() as usize);
            }
            if let Some(p) = cmd.get_doc("projection").filter(|p| !p.is_empty()) {
                docs = docs.iter().map(|d| query::project(d, p)).collect::<std::result::Result<_, _>>()?;
            }
            let first = batch_size(cmd, "batchSize", 101);
            let single = cmd.get_bool("singleBatch").unwrap_or(false) || limit < 0;
            Ok(new_static_cursor(sh, &mut st, db, &coll, docs, first, single))
        }
        "getMore" => {
            let id = cmd.get_i64("getMore").ok_or_else(|| err("getMore needs a cursor id", 2))?;
            let coll = cmd.get_str("collection").unwrap_or("").to_string();
            let ns = format!("{}.{}", db, coll);
            let limit = batch_size(cmd, "batchSize", usize::MAX);
            let static_batch = match st.cursors.get_mut(&id) {
                None => return Err(err(format!("cursor id {} not found", id), 43)),
                Some(ServerCursor { kind: CursorKind::Static(q), .. }) => {
                    let n = limit.min(q.len());
                    let batch: Vec<Document> = q.drain(..n).collect();
                    let done = q.is_empty();
                    Some((batch, done))
                }
                Some(_) => None,
            };
            if let Some((batch, done)) = static_batch {
                if done {
                    st.cursors.remove(&id);
                }
                return Ok(cursor_reply(if done { 0 } else { id }, &ns, "nextBatch", batch, None));
            }
            let wait = Duration::from_millis(cmd.get_i64("maxTimeMS").unwrap_or(1000).clamp(0, 60_000) as u64);
            let deadline = Instant::now() + wait;
            loop {
                let Some((docs, resume)) = poll_cursor(&mut st, id, limit)? else { return Err(err(format!("cursor id {} not found", id), 43)) };
                let now = Instant::now();
                if !docs.is_empty() || now >= deadline {
                    let extra = resume.map(|t| ("postBatchResumeToken", Bson::Document(t)));
                    return Ok(cursor_reply(id, &ns, "nextBatch", docs, extra));
                }
                st = wait_on(sh, st, deadline - now);
            }
        }
        "killCursors" => {
            let ids = cmd.get_array("cursors").cloned().unwrap_or_default();
            let mut killed = Vec::new();
            let mut missing = Vec::new();
            for i in ids {
                if let Some(id) = i.as_i64() {
                    if st.cursors.remove(&id).is_some() {
                        killed.push(Bson::Int64(id));
                    } else {
                        missing.push(Bson::Int64(id));
                    }
                }
            }
            Ok(Document::new().with("cursorsKilled", killed).with("cursorsNotFound", missing).with("cursorsAlive", Bson::Array(vec![])).with("cursorsUnknown", Bson::Array(vec![])))
        }
        "insert" => {
            let coll = coll_name(cmd, "insert")?;
            let docs = cmd.get_array("documents").cloned().unwrap_or_default();
            let ordered = cmd.get_bool("ordered").unwrap_or(true);
            let mut n = 0;
            let mut errors = Vec::new();
            for (i, d) in docs.into_iter().enumerate() {
                let Bson::Document(d) = d else { continue };
                match insert_doc(&mut st, db, &coll, d) {
                    Ok(()) => n += 1,
                    Err(e) => {
                        errors.push(Bson::Document(Document::new().with("index", i as i32).with("code", e.1).with("errmsg", e.0)));
                        if ordered {
                            break;
                        }
                    }
                }
            }
            drop(st);
            sh.cv.notify_all();
            let mut r = Document::new().with("n", n);
            if !errors.is_empty() {
                r.insert("writeErrors", errors);
            }
            Ok(r)
        }
        "update" => {
            let coll = coll_name(cmd, "update")?;
            let updates = cmd.get_array("updates").cloned().unwrap_or_default();
            let (mut n, mut modified) = (0i64, 0i64);
            let mut upserted = Vec::new();
            let mut errors = Vec::new();
            for (i, u) in updates.iter().enumerate() {
                let Some(u) = u.as_document() else { continue };
                match update_one_stmt(&mut st, db, &coll, u) {
                    Ok((matched, m, up)) => {
                        n += matched;
                        modified += m;
                        if let Some(id) = up {
                            n += 1;
                            upserted.push(Bson::Document(Document::new().with("index", i as i32).with("_id", id)));
                        }
                    }
                    Err(e) => {
                        errors.push(Bson::Document(Document::new().with("index", i as i32).with("code", e.1).with("errmsg", e.0)));
                        break;
                    }
                }
            }
            drop(st);
            sh.cv.notify_all();
            let mut r = Document::new().with("n", Bson::from_i64_compact(n)).with("nModified", Bson::from_i64_compact(modified));
            if !upserted.is_empty() {
                r.insert("upserted", upserted);
            }
            if !errors.is_empty() {
                r.insert("writeErrors", errors);
            }
            Ok(r)
        }
        "delete" => {
            let coll = coll_name(cmd, "delete")?;
            let deletes = cmd.get_array("deletes").cloned().unwrap_or_default();
            let mut n = 0i64;
            for d in deletes {
                let Some(d) = d.as_document() else { continue };
                let q = d.get_doc("q").cloned().unwrap_or_default();
                let limit = d.get_i64("limit").unwrap_or(0);
                let victims: Vec<u64> = {
                    let m = matching(&st, db, &coll, &q)?;
                    let take = if limit == 1 { 1 } else { usize::MAX };
                    m.into_iter().take(take).map(|(s, _)| s).collect()
                };
                let mut removed = Vec::new();
                if let Some(c) = st.dbs.get_mut(db).and_then(|x| x.get_mut(&coll)) {
                    if c.capped.is_some() && !victims.is_empty() {
                        return Err(err("cannot remove from a capped collection", 20));
                    }
                    c.docs.retain(|(s, d)| {
                        if victims.contains(s) {
                            removed.push(d.get("_id").cloned().unwrap_or(Bson::Null));
                            false
                        } else {
                            true
                        }
                    });
                }
                for key in removed {
                    n += 1;
                    st.seq += 1;
                    let seq = st.seq;
                    st.oplog.push(ChangeEvent { seq, db: db.into(), coll: coll.clone(), op: "delete", key, full: None, update: None });
                }
            }
            drop(st);
            sh.cv.notify_all();
            Ok(Document::new().with("n", Bson::from_i64_compact(n)))
        }
        "findAndModify" | "findandmodify" => {
            let coll = cmd.get_str(name).unwrap_or("").to_string();
            let q = cmd.get_doc("query").cloned().unwrap_or_default();
            let mut found: Vec<Document> = matching(&st, db, &coll, &q)?.into_iter().map(|(_, d)| d).collect();
            if let Some(s) = cmd.get_doc("sort") {
                query::sort_docs(&mut found, s);
            }
            let target = found.into_iter().next();
            let want_new = cmd.get_bool("new").unwrap_or(false);
            if cmd.get_bool("remove").unwrap_or(false) {
                if let Some(t) = &target {
                    let id = t.get("_id").cloned().unwrap_or(Bson::Null);
                    if let Some(c) = st.dbs.get_mut(db).and_then(|x| x.get_mut(&coll)) {
                        c.docs.retain(|(_, d)| !d.get("_id").is_some_and(|v| v.eq_bson(&id)));
                    }
                }
                return Ok(Document::new().with("value", target.map(Bson::Document).unwrap_or(Bson::Null)));
            }
            let mut stmt = Document::new().with("q", match &target {
                Some(t) => Document::new().with("_id", t.get("_id").cloned().unwrap_or(Bson::Null)),
                None => q.clone(),
            });
            stmt.insert("u", cmd.get_doc("update").cloned().unwrap_or_default());
            stmt.insert("upsert", cmd.get_bool("upsert").unwrap_or(false));
            let (_, _, up) = update_one_stmt(&mut st, db, &coll, &stmt)?;
            let after = match (&target, &up) {
                (Some(t), _) => {
                    let id = t.get("_id").cloned().unwrap_or(Bson::Null);
                    matching(&st, db, &coll, &Document::new().with("_id", id))?.into_iter().next().map(|(_, d)| d)
                }
                (None, Some(id)) => matching(&st, db, &coll, &Document::new().with("_id", id.clone()))?.into_iter().next().map(|(_, d)| d),
                _ => None,
            };
            drop(st);
            sh.cv.notify_all();
            let value = if want_new { after } else { target };
            Ok(Document::new().with("value", value.map(Bson::Document).unwrap_or(Bson::Null)))
        }
        "count" => {
            let coll = coll_name(cmd, "count")?;
            let q = cmd.get_doc("query").cloned().unwrap_or_default();
            let mut n = matching(&st, db, &coll, &q)?.len() as i64;
            n = (n - cmd.get_i64("skip").unwrap_or(0)).max(0);
            if let Some(l) = cmd.get_i64("limit").filter(|l| *l > 0) {
                n = n.min(l);
            }
            Ok(Document::new().with("n", Bson::from_i64_compact(n)))
        }
        "distinct" => {
            let coll = coll_name(cmd, "distinct")?;
            let key = cmd.get_str("key").unwrap_or("").to_string();
            let q = cmd.get_doc("query").cloned().unwrap_or_default();
            let mut values: Vec<Bson> = Vec::new();
            for (_, d) in matching(&st, db, &coll, &q)? {
                for v in query::path_values(&d, &key) {
                    let items = match v {
                        Bson::Array(a) => a.clone(),
                        other => vec![other.clone()],
                    };
                    for it in items {
                        if !values.iter().any(|x| x.eq_bson(&it)) {
                            values.push(it);
                        }
                    }
                }
            }
            values.sort_by(|a, b| a.cmp_bson(b));
            Ok(Document::new().with("values", values))
        }
        "aggregate" => {
            let all_colls = matches!(cmd.get("aggregate"), Some(Bson::Int32(1)) | Some(Bson::Int64(1)) | Some(Bson::Double(_)));
            let coll = if all_colls { String::new() } else { coll_name(cmd, "aggregate")? };
            let pipeline = cmd.get_array("pipeline").cloned().unwrap_or_default();
            let first_batch = batch_size(cmd, "cursor", 101);
            if let Some(cs) = pipeline.first().and_then(|s| s.as_document()).and_then(|s| s.get_doc("$changeStream")).cloned() {
                let lookup = cs.get_str("fullDocument").is_some_and(|f| f == "updateLookup" || f == "whenAvailable" || f == "required");
                let resume = cs.get_doc("resumeAfter").or_else(|| cs.get_doc("startAfter")).and_then(|t| t.get_str("_data")).and_then(|h| u64::from_str_radix(h, 16).ok());
                let next_event = match resume {
                    Some(s) => s + 1,
                    None => st.seq + 1,
                };
                let id = sh.next_cursor.fetch_add(1, Ordering::SeqCst);
                st.cursors.insert(id, ServerCursor { db: db.into(), coll: coll.clone(), kind: CursorKind::ChangeStream { all_colls, pipeline: pipeline[1..].to_vec(), next_event, lookup } });
                let ns = if all_colls { format!("{}.$cmd.aggregate", db) } else { format!("{}.{}", db, coll) };
                let (docs, tok) = poll_cursor(&mut st, id, first_batch)?.unwrap_or_default();
                return Ok(cursor_reply(id, &ns, "firstBatch", docs, tok.map(|t| ("postBatchResumeToken", Bson::Document(t)))));
            }
            let docs: Vec<Document> = matching(&st, db, &coll, &Document::new())?.into_iter().map(|(_, d)| d).collect();
            let out = query::aggregate(docs, &pipeline)?;
            Ok(new_static_cursor(sh, &mut st, db, &coll, out, first_batch, false))
        }
        "create" => {
            let coll = coll_name(cmd, "create")?;
            let dbm = st.dbs.entry(db.to_string()).or_default();
            if dbm.contains_key(&coll) {
                return Err(err(format!("Collection {}.{} already exists.", db, coll), 48));
            }
            let mut c = Collection::new();
            if cmd.get_bool("capped").unwrap_or(false) {
                let size = cmd.get_i64("size").ok_or_else(|| err("the 'size' field is required when 'capped' is true", 72))?;
                c.capped = Some((size.max(4096), cmd.get_i64("max").filter(|m| *m > 0)));
            }
            dbm.insert(coll, c);
            Ok(Document::new())
        }
        "drop" => {
            let coll = coll_name(cmd, "drop")?;
            let removed = st.dbs.get_mut(db).and_then(|d| d.remove(&coll));
            match removed {
                Some(c) => Ok(Document::new().with("nIndexesWas", 1).with("ns", format!("{}.{}", db, coll)).with("n", c.docs.len() as i32)),
                None => Err(err("ns not found", 26)),
            }
        }
        "dropDatabase" => {
            st.dbs.remove(db);
            Ok(Document::new().with("dropped", db))
        }
        "listCollections" => {
            let filter = cmd.get_doc("filter").cloned().unwrap_or_default();
            let mut out = Vec::new();
            if let Some(d) = st.dbs.get(db) {
                for (name, c) in d {
                    let mut opts = Document::new();
                    if let Some((size, max)) = c.capped {
                        opts.insert("capped", true);
                        opts.insert("size", Bson::Int64(size));
                        if let Some(m) = max {
                            opts.insert("max", Bson::Int64(m));
                        }
                    }
                    let info = Document::new().with("name", name.as_str()).with("type", "collection").with("options", opts).with("info", Document::new().with("readOnly", false));
                    if query::matches(&info, &filter)? {
                        out.push(info);
                    }
                }
            }
            Ok(cursor_reply(0, &format!("{}.$cmd.listCollections", db), "firstBatch", out, None))
        }
        "listDatabases" => {
            let dbs: Vec<Bson> = st
                .dbs
                .iter()
                .map(|(n, colls)| Bson::Document(Document::new().with("name", n.as_str()).with("sizeOnDisk", Bson::Int64(colls.values().flat_map(|c| c.docs.iter()).map(|(_, d)| doc_size(d)).sum())).with("empty", colls.is_empty())))
                .collect();
            Ok(Document::new().with("databases", dbs).with("totalSize", Bson::Int64(0)))
        }
        "dbStats" | "dbstats" => {
            let colls = st.dbs.get(db);
            let objects: i64 = colls.map(|c| c.values().map(|x| x.docs.len() as i64).sum()).unwrap_or(0);
            let size: i64 = colls.map(|c| c.values().flat_map(|x| x.docs.iter()).map(|(_, d)| doc_size(d)).sum()).unwrap_or(0);
            Ok(Document::new()
                .with("db", db)
                .with("collections", colls.map(|c| c.len() as i32).unwrap_or(0))
                .with("views", 0)
                .with("objects", Bson::Int64(objects))
                .with("avgObjSize", if objects > 0 { size as f64 / objects as f64 } else { 0.0 })
                .with("dataSize", Bson::Int64(size))
                .with("storageSize", Bson::Int64(size))
                .with("indexes", colls.map(|c| c.len() as i32).unwrap_or(0)))
        }
        "collStats" | "collstats" => {
            let coll = coll_name(cmd, name)?;
            let c = st.dbs.get(db).and_then(|d| d.get(&coll)).ok_or_else(|| err(format!("Collection [{}.{}] not found.", db, coll), 26))?;
            let size: i64 = c.docs.iter().map(|(_, d)| doc_size(d)).sum();
            Ok(Document::new().with("ns", format!("{}.{}", db, coll)).with("count", Bson::Int64(c.docs.len() as i64)).with("size", Bson::Int64(size)).with("capped", c.capped.is_some()).with("nindexes", 1))
        }
        other => Err(err(format!("no such command: '{}'", other), 59)),
    }
}

fn wait_on<'a>(sh: &'a Shared, g: MutexGuard<'a, Store>, d: Duration) -> MutexGuard<'a, Store> {
    sh.cv.wait_timeout(g, d.min(Duration::from_millis(100))).unwrap().0
}

/// Applies one update statement; returns (matched, modified, upserted id).
fn update_one_stmt(st: &mut Store, db: &str, coll: &str, u: &Document) -> std::result::Result<(i64, i64, Option<Bson>), QueryError> {
    let q = u.get_doc("q").cloned().unwrap_or_default();
    let upd = match u.get("u") {
        Some(Bson::Document(d)) => d.clone(),
        Some(Bson::Array(_)) => return Err(err("pipeline-style updates are not supported by the embedded server", 2)),
        _ => return Err(err("update must be a document", 9)),
    };
    let multi = u.get_bool("multi").unwrap_or(false);
    let upsert = u.get_bool("upsert").unwrap_or(false);
    let ops = query::is_operator_update(&upd);
    if multi && !ops {
        return Err(err("multi update is not supported for replacement-style update", 9));
    }
    let targets: Vec<u64> = matching(st, db, coll, &q)?.into_iter().take(if multi { usize::MAX } else { 1 }).map(|(s, _)| s).collect();
    if targets.is_empty() {
        if !upsert {
            return Ok((0, 0, None));
        }
        let mut d = if ops { query::upsert_base(&q) } else { upd.clone() };
        if ops {
            query::apply_update(&mut d, &upd, true)?;
        } else if let Some(id) = query::upsert_base(&q).get("_id") {
            if !d.contains_key("_id") {
                d.insert_first("_id", id.clone());
            }
        }
        if !d.contains_key("_id") {
            d.insert_first("_id", ObjectId::new());
        }
        let id = d.get("_id").cloned().unwrap();
        insert_doc(st, db, coll, d)?;
        return Ok((0, 0, Some(id)));
    }
    let mut modified = 0;
    let mut events = Vec::new();
    {
        let c = st.dbs.get_mut(db).and_then(|x| x.get_mut(coll)).unwrap();
        for (s, d) in c.docs.iter_mut().filter(|(s, _)| targets.contains(s)) {
            let old = d.clone();
            let mut new = if ops {
                let mut n = d.clone();
                query::apply_update(&mut n, &upd, false)?;
                n
            } else {
                let mut n = upd.clone();
                if let Some(id) = n.get("_id") {
                    if !old.get("_id").is_some_and(|o| o.eq_bson(id)) {
                        return Err(err("After applying the update, the (immutable) field '_id' was found to have been altered", 66));
                    }
                }
                n.insert_first("_id", old.get("_id").cloned().unwrap_or(Bson::Null));
                n
            };
            if new != old {
                if c.capped.is_some() && doc_size(&new) != doc_size(&old) {
                    return Err(err("Cannot change the size of a document in a capped collection", 10003));
                }
                modified += 1;
                let key = old.get("_id").cloned().unwrap_or(Bson::Null);
                events.push((key, if ops { None } else { Some(new.clone()) }, diff(&old, &new)));
                std::mem::swap(d, &mut new);
                let _ = s;
            }
        }
    }
    for (key, full, desc) in events {
        st.seq += 1;
        let seq = st.seq;
        let op = if full.is_some() { "replace" } else { "update" };
        st.oplog.push(ChangeEvent { seq, db: db.into(), coll: coll.into(), op, key, full, update: if op == "update" { Some(desc) } else { None } });
    }
    Ok((targets.len() as i64, modified, None))
}

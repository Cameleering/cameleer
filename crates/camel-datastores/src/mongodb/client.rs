//! MongoDB client: connection strings, `hello` handshake, SCRAM-SHA-256 authentication,
//! TLS, commands over `OP_MSG`, cursors (incl. tailable/awaitData and change streams)
//! and a small connection pool.

use std::io::{BufReader, Write};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use camel_core::error::{CamelError, Result};
use camel_tls::camel_support::EndpointTls;
use camel_tls::net::NetStream;
use camel_tls::sasl::{ScramClient, ScramMechanism};

use super::bson::{Bson, Document};
use super::wire::{decode_op_msg, encode_op_msg, read_message, OpMsg, OP_MSG};
use crate::kinds;
use crate::util::{percent_decode, split_host_port};

/// Client settings (from a connection string or code).
#[derive(Clone, Debug)]
pub struct MongoClientOptions {
    pub hosts: Vec<(String, u16)>,
    pub username: Option<String>,
    pub password: Option<String>,
    /// Database holding the credentials (default: the connection string database, else `admin`).
    pub auth_source: Option<String>,
    /// Only `SCRAM-SHA-256` is supported.
    pub auth_mechanism: Option<String>,
    /// Database from the connection string path.
    pub default_database: Option<String>,
    pub tls: Option<EndpointTls>,
    /// `tls=true` in the connection string (TLS settings resolved by the endpoint when not given).
    pub tls_requested: bool,
    pub tls_insecure: bool,
    pub connect_timeout: Duration,
    pub socket_timeout: Option<Duration>,
    pub app_name: Option<String>,
}

impl Default for MongoClientOptions {
    fn default() -> Self {
        MongoClientOptions {
            hosts: vec![("localhost".into(), 27017)],
            username: None,
            password: None,
            auth_source: None,
            auth_mechanism: None,
            default_database: None,
            tls: None,
            tls_requested: false,
            tls_insecure: false,
            connect_timeout: Duration::from_secs(10),
            socket_timeout: Some(Duration::from_secs(60)),
            app_name: None,
        }
    }
}

impl MongoClientOptions {
    /// Parses `mongodb://[user:pass@]host[:port][,host...][/db][?options]`.
    pub fn parse(uri: &str) -> Result<MongoClientOptions> {
        let rest = uri
            .strip_prefix("mongodb://")
            .ok_or_else(|| CamelError::illegal_argument(format!("Unsupported MongoDB connection string (mongodb:// expected; mongodb+srv:// needs DNS SRV lookups): {}", uri)))?;
        let mut o = MongoClientOptions::default();
        let (main, query) = match rest.split_once('?') {
            Some((m, q)) => (m, Some(q)),
            None => (rest, None),
        };
        let (creds, hostpart) = match main.rfind('@') {
            Some(i) => (Some(&main[..i]), &main[i + 1..]),
            None => (None, main),
        };
        if let Some(c) = creds {
            let (u, p) = match c.split_once(':') {
                Some((u, p)) => (u, Some(p)),
                None => (c, None),
            };
            o.username = Some(percent_decode(u));
            o.password = p.map(percent_decode);
        }
        let (hosts, db) = match hostpart.split_once('/') {
            Some((h, d)) => (h, Some(d)),
            None => (hostpart, None),
        };
        o.hosts = hosts.split(',').filter(|h| !h.is_empty()).map(|h| split_host_port(h, 27017)).collect::<Result<_>>()?;
        if o.hosts.is_empty() {
            return Err(CamelError::illegal_argument(format!("No host in MongoDB connection string: {}", uri)));
        }
        o.default_database = db.filter(|d| !d.is_empty()).map(percent_decode);
        for pair in query.unwrap_or("").split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let v = percent_decode(v);
            let truthy = v.eq_ignore_ascii_case("true");
            match k.to_ascii_lowercase().as_str() {
                "authsource" => o.auth_source = Some(v),
                "authmechanism" => o.auth_mechanism = Some(v),
                "tls" | "ssl" => o.tls_requested = truthy,
                "tlsinsecure" | "tlsallowinvalidcertificates" | "sslinvalidhostnameallowed" => o.tls_insecure = truthy,
                "connecttimeoutms" => o.connect_timeout = Duration::from_millis(v.parse().unwrap_or(10_000)),
                "sockettimeoutms" => {
                    let ms: u64 = v.parse().unwrap_or(0);
                    o.socket_timeout = if ms == 0 { None } else { Some(Duration::from_millis(ms)) };
                }
                "appname" => o.app_name = Some(v),
                _ => {} // replicaSet, w, readPreference, maxPoolSize, ...: accepted
            }
        }
        if let Some(m) = &o.auth_mechanism {
            if !m.eq_ignore_ascii_case("SCRAM-SHA-256") {
                return Err(CamelError::new(kinds::MONGO_SECURITY, format!("Unsupported authMechanism {} (only SCRAM-SHA-256 is implemented)", m)));
            }
        }
        Ok(o)
    }
}

/// Builds the exception for a failed command reply.
pub fn command_error(reply: &Document) -> CamelError {
    let code = reply.get_i64("code").unwrap_or(0);
    let name = reply.get_str("codeName").unwrap_or("");
    let msg = reply.get_str("errmsg").unwrap_or("unknown error");
    let kind = if code == 18 || code == 13 && msg.contains("uthenticat") { kinds::MONGO_SECURITY } else { kinds::MONGO_COMMAND };
    CamelError::new(kind, format!("Command failed with error {} ({}): '{}'", code, name, msg))
}

fn write_errors(reply: &Document) -> Option<CamelError> {
    let errs = reply.get_array("writeErrors")?;
    let first = errs.first()?.as_document()?;
    Some(CamelError::new(kinds::MONGO_WRITE, format!("Write operation error: {} ({})", first.get_str("errmsg").unwrap_or(""), first.get_i64("code").unwrap_or(0))))
}

static REQUEST_ID: AtomicI32 = AtomicI32::new(1);

/// One connection to a server.
pub struct MongoConnection {
    stream: NetStream,
    reader: BufReader<NetStream>,
    broken: bool,
    what: String,
    /// The `hello` reply.
    pub hello: Document,
}

impl MongoConnection {
    /// Connects to the first reachable host, performs the handshake and authenticates.
    pub fn connect(o: &MongoClientOptions) -> Result<MongoConnection> {
        let mut last = None;
        for (host, port) in &o.hosts {
            match Self::connect_host(o, host, *port) {
                Ok(c) => return Ok(c),
                Err(e) if e.kind == kinds::MONGO_SECURITY || e.kind.starts_with("SSL") => return Err(e),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| CamelError::illegal_argument("no MongoDB host")))
    }

    fn connect_host(o: &MongoClientOptions, host: &str, port: u16) -> Result<MongoConnection> {
        let what = format!("{}:{}", host, port);
        let stream = match &o.tls {
            Some(t) => t.connect(host, port, o.connect_timeout)?,
            None => NetStream::plain(camel_tls::net::connect_tcp(host, port, o.connect_timeout).map_err(|e| {
                CamelError::new(camel_core::error::kinds::CONNECT, format!("Timed out or failed connecting to MongoDB {}: {}", what, e))
            })?),
        };
        let _ = stream.set_nodelay(true);
        stream.set_read_timeout(o.socket_timeout).map_err(|e| CamelError::io(e.to_string()))?;
        let reader = BufReader::new(stream.try_clone().map_err(|e| CamelError::io(e.to_string()))?);
        let mut c = MongoConnection { stream, reader, broken: false, what, hello: Document::new() };
        let mut hello = Document::new().with("hello", 1).with("$db", "admin");
        let mut client = Document::new().with("driver", Document::new().with("name", "rust-camel").with("version", env!("CARGO_PKG_VERSION")));
        client.insert("os", Document::new().with("type", std::env::consts::OS));
        if let Some(a) = &o.app_name {
            client.insert_first("application", Document::new().with("name", a.as_str()));
        }
        hello.insert("client", client);
        c.hello = c.command(hello)?;
        if let Some(user) = &o.username {
            let source = o.auth_source.clone().or_else(|| o.default_database.clone()).unwrap_or_else(|| "admin".into());
            c.authenticate(&source, user, o.password.as_deref().unwrap_or("")).map_err(|e| {
                CamelError::new(kinds::MONGO_SECURITY, format!("Exception authenticating MongoCredential{{mechanism=SCRAM-SHA-256, userName='{}', source='{}'}}: {}", user, source, e.message))
            })?;
        }
        Ok(c)
    }

    fn authenticate(&mut self, source: &str, user: &str, password: &str) -> Result<()> {
        let mut scram = ScramClient::new(ScramMechanism::Sha256, user, password);
        let first = scram.client_first();
        let r = self.command(
            Document::new()
                .with("saslStart", 1)
                .with("mechanism", "SCRAM-SHA-256")
                .with("payload", Bson::Binary(0, first.into_bytes()))
                .with("autoAuthorize", 1)
                .with("options", Document::new().with("skipEmptyExchange", true))
                .with("$db", source),
        )?;
        let conv = r.get("conversationId").cloned().unwrap_or(Bson::Int32(1));
        let payload = |d: &Document| match d.get("payload") {
            Some(Bson::Binary(_, b)) => String::from_utf8_lossy(b).into_owned(),
            Some(Bson::String(s)) => s.clone(),
            _ => String::new(),
        };
        let fin = scram.handle_server_first(&payload(&r)).map_err(|e| CamelError::new(kinds::MONGO_SECURITY, e.to_string()))?;
        let r2 = self.command(Document::new().with("saslContinue", 1).with("conversationId", conv.clone()).with("payload", Bson::Binary(0, fin.into_bytes())).with("$db", source))?;
        scram.verify_server_final(&payload(&r2)).map_err(|e| CamelError::new(kinds::MONGO_SECURITY, e.to_string()))?;
        if !r2.get_bool("done").unwrap_or(false) {
            let r3 = self.command(Document::new().with("saslContinue", 1).with("conversationId", conv).with("payload", Bson::Binary(0, Vec::new())).with("$db", source))?;
            if !r3.get_bool("done").unwrap_or(false) {
                return Err(CamelError::new(kinds::MONGO_SECURITY, "SASL conversation did not complete"));
            }
        }
        Ok(())
    }

    pub fn is_broken(&self) -> bool {
        self.broken
    }

    /// A clone of the socket (shut it down to interrupt a blocked read from another thread).
    pub fn closer(&self) -> Option<NetStream> {
        self.stream.try_clone().ok()
    }

    /// Sets the socket read timeout.
    pub fn set_read_timeout(&self, d: Option<Duration>) {
        let _ = self.stream.set_read_timeout(d);
    }

    /// Sends a message and reads the reply (raw, `ok` not checked).
    pub fn send(&mut self, msg: &OpMsg) -> Result<Document> {
        let id = REQUEST_ID.fetch_add(1, Ordering::Relaxed);
        let bytes = encode_op_msg(id, 0, msg);
        let io = |e: std::io::Error, c: &mut MongoConnection| {
            c.broken = true;
            if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) {
                CamelError::new(camel_core::error::kinds::SOCKET_TIMEOUT, format!("Timeout while receiving message from {}", c.what))
            } else {
                camel_tls::camel_support::io_to_camel(e, &format!("mongodb {}", c.what))
            }
        };
        if let Err(e) = self.stream.write_all(&bytes).and_then(|_| self.stream.flush()) {
            return Err(io(e, self));
        }
        loop {
            let (h, body) = match read_message(&mut self.reader) {
                Ok(m) => m,
                Err(e) => return Err(io(e, self)),
            };
            if h.op_code != OP_MSG {
                self.broken = true;
                return Err(CamelError::new(kinds::MONGO, format!("Unexpected reply opcode {}", h.op_code)));
            }
            if h.response_to != id {
                continue;
            }
            return Ok(decode_op_msg(&body)?.body);
        }
    }

    /// Runs a command (its `$db` field must be set) and checks `ok`.
    pub fn command(&mut self, cmd: Document) -> Result<Document> {
        self.command_msg(&OpMsg::new(cmd))
    }

    /// Runs a command message (with document sequences) and checks `ok`.
    pub fn command_msg(&mut self, msg: &OpMsg) -> Result<Document> {
        let r = self.send(msg)?;
        if r.get_f64("ok").unwrap_or(0.0) != 1.0 {
            return Err(command_error(&r));
        }
        Ok(r)
    }
}

/// Result of an update.
#[derive(Clone, Debug, PartialEq)]
pub struct UpdateResult {
    pub matched: i64,
    pub modified: i64,
    pub upserted_id: Option<Bson>,
}

/// An open cursor (first batch + id).
#[derive(Clone, Debug)]
pub struct CursorBatch {
    pub id: i64,
    pub ns: String,
    pub docs: Vec<Document>,
    /// Change streams: the resume token after this batch.
    pub post_batch_resume_token: Option<Document>,
}

fn parse_cursor(r: &Document) -> Result<CursorBatch> {
    let c = r.get_doc("cursor").ok_or_else(|| CamelError::new(kinds::MONGO, "reply without cursor"))?;
    let docs = c
        .get_array("firstBatch")
        .or_else(|| c.get_array("nextBatch"))
        .map(|a| a.iter().filter_map(|d| d.as_document().cloned()).collect())
        .unwrap_or_default();
    Ok(CursorBatch { id: c.get_i64("id").unwrap_or(0), ns: c.get_str("ns").unwrap_or("").to_string(), docs, post_batch_resume_token: c.get_doc("postBatchResumeToken").cloned() })
}

/// Options of a `find`.
#[derive(Clone, Debug, Default)]
pub struct FindOptions {
    pub projection: Option<Document>,
    pub sort: Option<Document>,
    pub limit: Option<i64>,
    pub skip: Option<i64>,
    pub batch_size: Option<i64>,
    pub tailable: bool,
    pub await_data: bool,
    pub max_await_ms: Option<i64>,
}

/// A pooled MongoDB client (bind it as a bean and reference it from `mongodb:<bean>`).
pub struct MongoClient {
    opts: MongoClientOptions,
    pool: Mutex<Vec<MongoConnection>>,
}

impl MongoClient {
    pub fn new(opts: MongoClientOptions) -> MongoClient {
        MongoClient { opts, pool: Mutex::new(Vec::new()) }
    }

    /// Client for a connection string.
    pub fn parse(uri: &str) -> Result<MongoClient> {
        Ok(Self::new(MongoClientOptions::parse(uri)?))
    }

    pub fn options(&self) -> &MongoClientOptions {
        &self.opts
    }

    /// A dedicated (unpooled) connection, e.g. for long-running cursors.
    pub fn connect(&self) -> Result<MongoConnection> {
        MongoConnection::connect(&self.opts)
    }

    /// Runs `f` with a pooled connection.
    pub fn with_connection<T>(&self, f: impl FnOnce(&mut MongoConnection) -> Result<T>) -> Result<T> {
        let pooled = self.pool.lock().unwrap().pop();
        let mut c = match pooled {
            Some(c) => c,
            None => self.connect()?,
        };
        let r = f(&mut c);
        if !c.is_broken() {
            let mut p = self.pool.lock().unwrap();
            if p.len() < 8 {
                p.push(c);
            }
        }
        r
    }

    /// Runs a command against `db`.
    pub fn run_command(&self, db: &str, mut cmd: Document) -> Result<Document> {
        cmd.insert("$db", db);
        self.with_connection(|c| c.command(cmd))
    }

    /// `find` returning all documents (follows the cursor with `getMore`).
    pub fn find(&self, db: &str, coll: &str, filter: Document, o: &FindOptions) -> Result<Vec<Document>> {
        let first = self.find_cursor(db, coll, filter, o)?;
        self.drain(db, coll, first, o.batch_size)
    }

    fn find_cmd(coll: &str, filter: Document, o: &FindOptions) -> Document {
        let mut cmd = Document::new().with("find", coll).with("filter", filter);
        if let Some(p) = &o.projection {
            cmd.insert("projection", p.clone());
        }
        if let Some(s) = &o.sort {
            cmd.insert("sort", s.clone());
        }
        if let Some(l) = o.limit.filter(|l| *l != 0) {
            cmd.insert("limit", Bson::Int64(l.abs()));
            if l < 0 {
                cmd.insert("singleBatch", true);
            }
        }
        if let Some(s) = o.skip.filter(|s| *s > 0) {
            cmd.insert("skip", Bson::Int64(s));
        }
        if let Some(b) = o.batch_size.filter(|b| *b > 0) {
            cmd.insert("batchSize", Bson::Int64(b));
        }
        if o.tailable {
            cmd.insert("tailable", true);
        }
        if o.await_data {
            cmd.insert("awaitData", true);
        }
        cmd
    }

    /// `find` returning the first batch (cursor left open).
    pub fn find_cursor(&self, db: &str, coll: &str, filter: Document, o: &FindOptions) -> Result<CursorBatch> {
        parse_cursor(&self.run_command(db, Self::find_cmd(coll, filter, o))?)
    }

    /// Fetches the next batch of a cursor.
    pub fn get_more(&self, db: &str, coll: &str, id: i64, batch_size: Option<i64>, max_await_ms: Option<i64>) -> Result<CursorBatch> {
        self.with_connection(|c| get_more_on(c, db, coll, id, batch_size, max_await_ms))
    }

    fn drain(&self, db: &str, coll: &str, mut batch: CursorBatch, batch_size: Option<i64>) -> Result<Vec<Document>> {
        let mut out = std::mem::take(&mut batch.docs);
        while batch.id != 0 {
            batch = self.get_more(db, coll, batch.id, batch_size, None)?;
            out.append(&mut batch.docs);
        }
        Ok(out)
    }

    /// Closes server cursors.
    pub fn kill_cursors(&self, db: &str, coll: &str, ids: &[i64]) -> Result<()> {
        self.run_command(db, Document::new().with("killCursors", coll).with("cursors", Bson::Array(ids.iter().map(|i| Bson::Int64(*i)).collect()))).map(|_| ())
    }

    /// Inserts documents (an `_id` ObjectId is added when missing); returns the documents as inserted.
    pub fn insert_many(&self, db: &str, coll: &str, mut docs: Vec<Document>, ordered: bool) -> Result<Vec<Document>> {
        for d in docs.iter_mut() {
            if !d.contains_key("_id") {
                d.insert_first("_id", super::bson::ObjectId::new());
            }
        }
        let mut msg = OpMsg::new(Document::new().with("insert", coll).with("ordered", ordered).with("$db", db));
        msg.sequences.push(("documents".into(), docs.clone()));
        let r = self.with_connection(|c| c.command_msg(&msg))?;
        if let Some(e) = write_errors(&r) {
            return Err(e);
        }
        Ok(docs)
    }

    /// `update` with one statement.
    pub fn update(&self, db: &str, coll: &str, filter: Document, update: Document, multi: bool, upsert: bool) -> Result<UpdateResult> {
        let stmt = Document::new().with("q", filter).with("u", update).with("multi", multi).with("upsert", upsert);
        let mut msg = OpMsg::new(Document::new().with("update", coll).with("ordered", true).with("$db", db));
        msg.sequences.push(("updates".into(), vec![stmt]));
        let r = self.with_connection(|c| c.command_msg(&msg))?;
        if let Some(e) = write_errors(&r) {
            return Err(e);
        }
        let upserted = r.get_array("upserted").and_then(|a| a.first()).and_then(|u| u.as_document()).and_then(|u| u.get("_id").cloned());
        let n = r.get_i64("n").unwrap_or(0);
        Ok(UpdateResult { matched: if upserted.is_some() { 0 } else { n }, modified: r.get_i64("nModified").unwrap_or(0), upserted_id: upserted })
    }

    /// `delete` (limit 0 = all matching, 1 = one).
    pub fn delete(&self, db: &str, coll: &str, filter: Document, limit: i32) -> Result<i64> {
        let mut msg = OpMsg::new(Document::new().with("delete", coll).with("ordered", true).with("$db", db));
        msg.sequences.push(("deletes".into(), vec![Document::new().with("q", filter).with("limit", limit)]));
        let r = self.with_connection(|c| c.command_msg(&msg))?;
        if let Some(e) = write_errors(&r) {
            return Err(e);
        }
        Ok(r.get_i64("n").unwrap_or(0))
    }

    /// Counts matching documents.
    pub fn count(&self, db: &str, coll: &str, filter: Document) -> Result<i64> {
        Ok(self.run_command(db, Document::new().with("count", coll).with("query", filter))?.get_i64("n").unwrap_or(0))
    }

    /// Runs an aggregation and returns all results.
    pub fn aggregate(&self, db: &str, coll: &str, pipeline: Vec<Bson>, allow_disk_use: bool, batch_size: Option<i64>) -> Result<Vec<Document>> {
        let mut cursor = Document::new();
        if let Some(b) = batch_size.filter(|b| *b > 0) {
            cursor.insert("batchSize", Bson::Int64(b));
        }
        let mut cmd = Document::new().with("aggregate", coll).with("pipeline", Bson::Array(pipeline)).with("cursor", cursor);
        if allow_disk_use {
            cmd.insert("allowDiskUse", true);
        }
        let first = parse_cursor(&self.run_command(db, cmd)?)?;
        self.drain(db, coll, first, batch_size)
    }

    /// Distinct values of a field.
    pub fn distinct(&self, db: &str, coll: &str, key: &str, filter: Document) -> Result<Vec<Bson>> {
        let r = self.run_command(db, Document::new().with("distinct", coll).with("key", key).with("query", filter))?;
        Ok(r.get_array("values").cloned().unwrap_or_default())
    }

    /// Creates a collection (`capped` with `size`/`max` for tailable cursors).
    pub fn create_collection(&self, db: &str, coll: &str, capped: Option<(i64, Option<i64>)>) -> Result<()> {
        let mut cmd = Document::new().with("create", coll);
        if let Some((size, max)) = capped {
            cmd.insert("capped", true);
            cmd.insert("size", Bson::Int64(size));
            if let Some(m) = max {
                cmd.insert("max", Bson::Int64(m));
            }
        }
        self.run_command(db, cmd).map(|_| ())
    }

    /// Names of the collections of a database.
    pub fn list_collection_names(&self, db: &str) -> Result<Vec<String>> {
        let first = parse_cursor(&self.run_command(db, Document::new().with("listCollections", 1).with("nameOnly", true))?)?;
        Ok(first.docs.iter().filter_map(|d| d.get_str("name").map(String::from)).collect())
    }
}

/// `getMore` on a given connection.
pub fn get_more_on(c: &mut MongoConnection, db: &str, coll: &str, id: i64, batch_size: Option<i64>, max_await_ms: Option<i64>) -> Result<CursorBatch> {
    let mut cmd = Document::new().with("getMore", Bson::Int64(id)).with("collection", coll);
    if let Some(b) = batch_size.filter(|b| *b > 0) {
        cmd.insert("batchSize", Bson::Int64(b));
    }
    if let Some(m) = max_await_ms {
        cmd.insert("maxTimeMS", Bson::Int64(m));
    }
    cmd.insert("$db", db);
    parse_cursor(&c.command(cmd)?)
}

/// Runs a cursor-returning command on a given connection.
pub fn open_cursor_on(c: &mut MongoConnection, db: &str, mut cmd: Document) -> Result<CursorBatch> {
    cmd.insert("$db", db);
    parse_cursor(&c.command(cmd)?)
}

/// The `find` command document for [`FindOptions`] (for dedicated-connection cursors).
pub fn find_command(coll: &str, filter: Document, o: &FindOptions) -> Document {
    MongoClient::find_cmd(coll, filter, o)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_strings() {
        let o = MongoClientOptions::parse("mongodb://us%40r:p%3Ass@h1:1,h2/mydb?authSource=admin&tls=true&socketTimeoutMS=0&appName=x").unwrap();
        assert_eq!(o.hosts, vec![("h1".to_string(), 1), ("h2".to_string(), 27017)]);
        assert_eq!(o.username.as_deref(), Some("us@r"));
        assert_eq!(o.password.as_deref(), Some("p:ss"));
        assert_eq!(o.default_database.as_deref(), Some("mydb"));
        assert_eq!(o.auth_source.as_deref(), Some("admin"));
        assert!(o.tls_requested);
        assert_eq!(o.socket_timeout, None);
        assert!(MongoClientOptions::parse("mongodb+srv://x").is_err());
        assert!(MongoClientOptions::parse("mongodb://h/?authMechanism=MONGODB-X509").is_err());
        let o = MongoClientOptions::parse("mongodb://localhost").unwrap();
        assert_eq!(o.hosts, vec![("localhost".to_string(), 27017)]);
    }
}

//! MongoDB client: connection strings (`mongodb://` and `mongodb+srv://` with DNS SRV/TXT
//! lookups), `hello` handshake, replica-set discovery (connects to the primary named by
//! `hello`, remembers the members, re-discovers after `NotWritablePrimary`), SCRAM-SHA-1 /
//! SCRAM-SHA-256 / MONGODB-X509 authentication, TLS, commands over `OP_MSG`, cursors
//! (incl. tailable/awaitData and change streams) and a small connection pool.

use std::io::{BufReader, Write};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use cameleer_core::error::{CamelError, Result};
use cameleer_tls::camel_support::EndpointTls;
use cameleer_tls::net::NetStream;
use cameleer_tls::sasl::{ScramClient, ScramMechanism};

use crate::dns;

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
    /// `SCRAM-SHA-256` (default), `SCRAM-SHA-1` or `MONGODB-X509`.
    pub auth_mechanism: Option<String>,
    /// `replicaSet`: the expected replica set name.
    pub replica_set: Option<String>,
    /// `directConnection=true`: no replica-set discovery.
    pub direct_connection: bool,
    /// The `mongodb+srv` host name (hosts come from its SRV records).
    pub srv_host: Option<String>,
    /// DNS server for `mongodb+srv` lookups (`dnsServer` option; default from the system).
    pub dns_server: Option<String>,
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
            replica_set: None,
            direct_connection: false,
            srv_host: None,
            dns_server: None,
        }
    }
}

/// Seed list of a `mongodb+srv` host: SRV records of `_<service>._tcp.<host>` (targets must
/// share the parent domain of the host) and the URI options of its TXT record.
pub fn resolve_srv(host: &str, service: &str, dns_server: Option<&str>, timeout: Duration) -> Result<(Vec<(String, u16)>, Option<String>)> {
    let server = dns_server.map(String::from).unwrap_or_else(dns::default_server);
    let records = dns::lookup_srv(&server, &format!("_{}._tcp.{}", service, host), timeout)?;
    if records.is_empty() {
        return Err(CamelError::new(kinds::MONGO_CONFIGURATION, format!("No SRV records for _{}._tcp.{}", service, host)));
    }
    let parent = match host.split_once('.') {
        Some((_, p)) if p.contains('.') => p.to_ascii_lowercase(),
        _ => return Err(CamelError::new(kinds::MONGO_CONFIGURATION, format!("mongodb+srv host '{}' must have at least three parts (host.domain.tld)", host))),
    };
    let mut hosts = Vec::new();
    for r in records {
        let target = r.target.trim_end_matches('.').to_ascii_lowercase();
        if !target.ends_with(&format!(".{}", parent)) {
            return Err(CamelError::new(kinds::MONGO_CONFIGURATION, format!("SRV record target '{}' is not in the domain of '{}'", target, host)));
        }
        // with an explicit DNS server, resolve the targets through it too (private zones)
        let mut addr = target.clone();
        if dns_server.is_some() {
            if let Ok(msg) = dns::query(&server, &target, dns::TYPE_A, timeout) {
                if let Some(ip) = first_a_record(&msg) {
                    addr = ip;
                }
            }
        }
        hosts.push((addr, r.port));
    }
    let txt = dns::lookup_txt(&server, host, timeout)?;
    if txt.len() > 1 {
        return Err(CamelError::new(kinds::MONGO_CONFIGURATION, format!("Multiple TXT records for {}", host)));
    }
    Ok((hosts, txt.into_iter().next()))
}

fn first_a_record(msg: &[u8]) -> Option<String> {
    // the last 4 bytes of a single-answer A reply hold the address
    if msg.len() < 16 || u16::from_be_bytes([msg[6], msg[7]]) == 0 {
        return None;
    }
    let ip = &msg[msg.len() - 4..];
    Some(format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]))
}

impl MongoClientOptions {
    /// Parses `mongodb://[user:pass@]host[:port][,host...][/db][?options]`.
    pub fn parse(uri: &str) -> Result<MongoClientOptions> {
        let (srv, rest) = match uri.strip_prefix("mongodb+srv://") {
            Some(r) => (true, r),
            None => (false, uri.strip_prefix("mongodb://").ok_or_else(|| CamelError::illegal_argument(format!("Unsupported MongoDB connection string (mongodb:// or mongodb+srv:// expected): {}", uri)))?),
        };
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
        let user_query: Vec<String> = query.unwrap_or("").split('&').filter(|p| !p.is_empty()).map(String::from).collect();
        let mut pairs: Vec<String> = Vec::new();
        if srv {
            if hosts.contains(',') || hosts.contains(':') {
                return Err(CamelError::illegal_argument(format!("A mongodb+srv connection string takes one host name without port: {}", uri)));
            }
            o.tls_requested = true;
            let host = hosts.to_string();
            let dns_server = user_query.iter().find_map(|p| p.split_once('=').filter(|(k, _)| k.eq_ignore_ascii_case("dnsServer")).map(|(_, v)| percent_decode(v)));
            let service = user_query.iter().find_map(|p| p.split_once('=').filter(|(k, _)| k.eq_ignore_ascii_case("srvServiceName")).map(|(_, v)| percent_decode(v))).unwrap_or_else(|| "mongodb".into());
            let max_hosts: usize = user_query.iter().find_map(|p| p.split_once('=').filter(|(k, _)| k.eq_ignore_ascii_case("srvMaxHosts")).and_then(|(_, v)| v.parse().ok())).unwrap_or(0);
            let (mut seeds, txt) = resolve_srv(&host, &service, dns_server.as_deref(), o.connect_timeout)?;
            if max_hosts > 0 {
                seeds.truncate(max_hosts);
            }
            o.hosts = seeds;
            o.srv_host = Some(host);
            o.dns_server = dns_server;
            // TXT options first (only authSource / replicaSet / loadBalanced are allowed), user options override
            if let Some(t) = txt {
                for p in t.split('&').filter(|p| !p.is_empty()) {
                    let k = p.split_once('=').map(|(k, _)| k).unwrap_or(p).to_ascii_lowercase();
                    if !matches!(k.as_str(), "authsource" | "replicaset" | "loadbalanced") {
                        return Err(CamelError::new(kinds::MONGO_CONFIGURATION, format!("TXT record option '{}' is not allowed", k)));
                    }
                    pairs.push(p.to_string());
                }
            }
        }
        pairs.extend(user_query);
        for pair in pairs.iter().map(|s| s.as_str()) {
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
                "replicaset" => o.replica_set = Some(v),
                "directconnection" => o.direct_connection = truthy,
                "dnsserver" => o.dns_server = Some(v),
                _ => {} // w, readPreference, maxPoolSize, srvServiceName, ...: accepted
            }
        }
        if let Some(m) = &o.auth_mechanism {
            let m = m.to_ascii_uppercase();
            if !matches!(m.as_str(), "SCRAM-SHA-256" | "SCRAM-SHA-1" | "MONGODB-X509") {
                return Err(CamelError::new(kinds::MONGO_SECURITY, format!("Unsupported authMechanism {} (SCRAM-SHA-256, SCRAM-SHA-1 or MONGODB-X509)", m)));
            }
            if m == "MONGODB-X509" && o.password.is_some() {
                return Err(CamelError::new(kinds::MONGO_SECURITY, "MONGODB-X509 does not take a password"));
            }
            o.auth_mechanism = Some(m);
        }
        if srv && o.direct_connection {
            return Err(CamelError::illegal_argument("directConnection=true is not allowed with mongodb+srv"));
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

/// MongoDB's legacy password digest `hex(md5(user + ":mongo:" + password))` (SCRAM-SHA-1).
pub fn mongo_password_digest(user: &str, password: &str) -> String {
    use cameleer_tls::crypto::hash::Digest;
    let d = cameleer_tls::crypto::md5::Md5::digest(format!("{}:mongo:{}", user, password).as_bytes());
    crate::util::hex(&d)
}

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
        let mut candidates: Vec<(String, u16)> = o.hosts.clone();
        let mut i = 0;
        while i < candidates.len() && i < 64 {
            let (host, port) = candidates[i].clone();
            i += 1;
            match Self::connect_host(o, &host, port) {
                Ok(c) => {
                    let set = c.hello.get_str("setName").map(String::from);
                    if let (Some(want), Some(have)) = (&o.replica_set, &set) {
                        if want != have {
                            last = Some(CamelError::new(kinds::MONGO, format!("{}:{} belongs to replica set '{}' instead of '{}'", host, port, have, want)));
                            continue;
                        }
                    }
                    if o.direct_connection || (set.is_none() && o.replica_set.is_none()) || c.is_writable_primary() {
                        return Ok(c);
                    }
                    // a secondary: go to the primary it names, then the other members
                    let mut more: Vec<(String, u16)> = Vec::new();
                    if let Some(p) = c.hello.get_str("primary") {
                        more.push(split_host_port(p, 27017)?);
                    }
                    for h in c.members() {
                        more.push(h);
                    }
                    for m in more {
                        if !candidates.contains(&m) {
                            candidates.insert(i.min(candidates.len()), m);
                        }
                    }
                    last = Some(CamelError::new(kinds::MONGO, format!("{}:{} is not the replica set primary", host, port)));
                }
                Err(e) if e.kind == kinds::MONGO_SECURITY || e.kind.starts_with("SSL") => return Err(e),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| CamelError::illegal_argument("no MongoDB host")))
    }

    /// Whether the server is a writable primary (or a standalone).
    pub fn is_writable_primary(&self) -> bool {
        self.hello.get_bool("isWritablePrimary").or_else(|| self.hello.get_bool("ismaster")).unwrap_or(false)
    }

    /// Replica set members announced by `hello` (`hosts` + `passives`).
    pub fn members(&self) -> Vec<(String, u16)> {
        let mut out = Vec::new();
        for key in ["hosts", "passives"] {
            for h in self.hello.get_array(key).cloned().unwrap_or_default() {
                if let Some(Ok(hp)) = h.as_str().map(|s| split_host_port(s, 27017)) {
                    out.push(hp);
                }
            }
        }
        out
    }

    fn connect_host(o: &MongoClientOptions, host: &str, port: u16) -> Result<MongoConnection> {
        let what = format!("{}:{}", host, port);
        let stream = match &o.tls {
            Some(t) => t.connect(host, port, o.connect_timeout)?,
            None => NetStream::plain(cameleer_tls::net::connect_tcp(host, port, o.connect_timeout).map_err(|e| {
                CamelError::new(cameleer_core::error::kinds::CONNECT, format!("Timed out or failed connecting to MongoDB {}: {}", what, e))
            })?),
        };
        let _ = stream.set_nodelay(true);
        stream.set_read_timeout(o.socket_timeout).map_err(|e| CamelError::io(e.to_string()))?;
        let reader = BufReader::new(stream.try_clone().map_err(|e| CamelError::io(e.to_string()))?);
        let mut c = MongoConnection { stream, reader, broken: false, what, hello: Document::new() };
        let mut hello = Document::new().with("hello", 1).with("$db", "admin");
        let mut client = Document::new().with("driver", Document::new().with("name", "Cameleer").with("version", env!("CARGO_PKG_VERSION")));
        client.insert("os", Document::new().with("type", std::env::consts::OS));
        if let Some(a) = &o.app_name {
            client.insert_first("application", Document::new().with("name", a.as_str()));
        }
        hello.insert("client", client);
        c.hello = c.command(hello)?;
        let mech = o.auth_mechanism.clone().unwrap_or_else(|| "SCRAM-SHA-256".into());
        if mech == "MONGODB-X509" {
            let mut cmd = Document::new().with("authenticate", 1).with("mechanism", "MONGODB-X509");
            if let Some(u) = &o.username {
                cmd.insert("user", u.as_str());
            }
            cmd.insert("$db", "$external");
            c.command(cmd).map_err(|e| CamelError::new(kinds::MONGO_SECURITY, format!("Exception authenticating MongoCredential{{mechanism=MONGODB-X509, userName='{}', source='$external'}}: {}", o.username.as_deref().unwrap_or(""), e.message)))?;
        } else if let Some(user) = &o.username {
            let source = o.auth_source.clone().or_else(|| o.default_database.clone()).unwrap_or_else(|| "admin".into());
            let sha1 = mech == "SCRAM-SHA-1";
            c.authenticate(&source, user, o.password.as_deref().unwrap_or(""), sha1).map_err(|e| {
                CamelError::new(kinds::MONGO_SECURITY, format!("Exception authenticating MongoCredential{{mechanism={}, userName='{}', source='{}'}}: {}", mech, user, source, e.message))
            })?;
        }
        Ok(c)
    }

    fn authenticate(&mut self, source: &str, user: &str, password: &str, sha1: bool) -> Result<()> {
        // SCRAM-SHA-1 hashes the password as hex(md5("user:mongo:password")) (no SASLprep)
        let (mech, secret) = if sha1 { (ScramMechanism::Sha1, mongo_password_digest(user, password)) } else { (ScramMechanism::Sha256, password.to_string()) };
        let mut scram = ScramClient::new(mech, user, &secret);
        let first = scram.client_first();
        let r = self.command(
            Document::new()
                .with("saslStart", 1)
                .with("mechanism", mech.name())
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
                CamelError::new(cameleer_core::error::kinds::SOCKET_TIMEOUT, format!("Timeout while receiving message from {}", c.what))
            } else {
                cameleer_tls::camel_support::io_to_camel(e, &format!("mongodb {}", c.what))
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

/// Result of [`MongoClient::bulk_write`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BulkWriteResult {
    pub inserted: i64,
    pub matched: i64,
    pub modified: i64,
    pub deleted: i64,
    /// `(index in the request list, _id)` of upserted documents.
    pub upserts: Vec<(i64, Bson)>,
    /// `(index, code, message)` of failed writes (unordered bulks continue past them).
    pub errors: Vec<(i64, i64, String)>,
}

/// One bulk write model.
#[derive(Clone, Debug)]
pub enum WriteModel {
    InsertOne(Document),
    UpdateOne { filter: Document, update: Document, upsert: bool },
    UpdateMany { filter: Document, update: Document, upsert: bool },
    ReplaceOne { filter: Document, replacement: Document, upsert: bool },
    DeleteOne(Document),
    DeleteMany(Document),
}

impl WriteModel {
    /// Parses `{"insertOne": {"document": ..}}`, `{"updateOne": {"filter", "update", "upsert"}}`,
    /// `updateMany`, `{"replaceOne": {"filter", "replacement", "upsert"}}`, `{"deleteOne":
    /// {"filter"}}`, `deleteMany`; any other document is an `insertOne`.
    pub fn from_document(d: &Document) -> Result<WriteModel> {
        let bad = |m: &str| CamelError::new(kinds::MONGO, format!("Invalid bulk write model: {}", m));
        if d.keys().count() == 1 {
            let k = d.keys().next().unwrap_or("").to_string();
            if let Some(spec) = d.get_doc(&k) {
                let filter = || spec.get_doc("filter").cloned().unwrap_or_default();
                let upsert = spec.get_bool("upsert").unwrap_or(false);
                return Ok(match k.as_str() {
                    "insertOne" => WriteModel::InsertOne(spec.get_doc("document").cloned().ok_or_else(|| bad("insertOne needs document"))?),
                    "updateOne" => WriteModel::UpdateOne { filter: filter(), update: spec.get_doc("update").cloned().ok_or_else(|| bad("updateOne needs update"))?, upsert },
                    "updateMany" => WriteModel::UpdateMany { filter: filter(), update: spec.get_doc("update").cloned().ok_or_else(|| bad("updateMany needs update"))?, upsert },
                    "replaceOne" => WriteModel::ReplaceOne { filter: filter(), replacement: spec.get_doc("replacement").cloned().ok_or_else(|| bad("replaceOne needs replacement"))?, upsert },
                    "deleteOne" => WriteModel::DeleteOne(filter()),
                    "deleteMany" => WriteModel::DeleteMany(filter()),
                    _ => WriteModel::InsertOne(d.clone()),
                });
            }
        }
        Ok(WriteModel::InsertOne(d.clone()))
    }

    fn kind(&self) -> u8 {
        match self {
            WriteModel::InsertOne(_) => 0,
            WriteModel::UpdateOne { .. } | WriteModel::UpdateMany { .. } | WriteModel::ReplaceOne { .. } => 1,
            _ => 2,
        }
    }
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
    /// Replica set members learnt from `hello` (tried after the seeds).
    discovered: Mutex<Vec<(String, u16)>>,
}

fn is_not_primary(e: &CamelError) -> bool {
    e.message.contains("NotWritablePrimary") || e.message.contains("NotPrimary") || e.message.contains("not master")
}

impl MongoClient {
    pub fn new(opts: MongoClientOptions) -> MongoClient {
        MongoClient { opts, pool: Mutex::new(Vec::new()), discovered: Mutex::new(Vec::new()) }
    }

    /// Replica set members discovered so far.
    pub fn discovered_members(&self) -> Vec<(String, u16)> {
        self.discovered.lock().unwrap().clone()
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
        let mut o = self.opts.clone();
        for m in self.discovered.lock().unwrap().iter() {
            if !o.hosts.contains(m) {
                o.hosts.push(m.clone());
            }
        }
        let c = MongoConnection::connect(&o)?;
        let members = c.members();
        if !members.is_empty() {
            *self.discovered.lock().unwrap() = members;
        }
        Ok(c)
    }

    /// Runs `f` with a pooled connection (after a `NotWritablePrimary` error the pool is
    /// dropped and `f` retried once on the newly discovered primary).
    pub fn with_connection<T>(&self, f: impl Fn(&mut MongoConnection) -> Result<T>) -> Result<T> {
        for attempt in 0..2 {
            let pooled = self.pool.lock().unwrap().pop();
            let mut c = match pooled {
                Some(c) => c,
                None => self.connect()?,
            };
            let r = f(&mut c);
            match &r {
                Err(e) if attempt == 0 && is_not_primary(e) => {
                    self.pool.lock().unwrap().clear();
                    continue;
                }
                _ => {}
            }
            if !c.is_broken() {
                let mut p = self.pool.lock().unwrap();
                if p.len() < 8 {
                    p.push(c);
                }
            }
            return r;
        }
        unreachable!()
    }

    /// Runs a command against `db`.
    pub fn run_command(&self, db: &str, mut cmd: Document) -> Result<Document> {
        cmd.insert("$db", db);
        self.with_connection(|c| c.command(cmd.clone()))
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

    /// Runs write models as `insert` / `update` / `delete` commands, grouping consecutive
    /// models of the same kind. Ordered bulks stop at the first error.
    pub fn bulk_write(&self, db: &str, coll: &str, models: Vec<WriteModel>, ordered: bool) -> Result<BulkWriteResult> {
        let mut res = BulkWriteResult::default();
        let mut i = 0;
        while i < models.len() {
            let kind = models[i].kind();
            let start = i;
            while i < models.len() && models[i].kind() == kind {
                i += 1;
            }
            let group = &models[start..i];
            let (cmd, seq, docs): (&str, &str, Vec<Document>) = match kind {
                0 => (
                    "insert",
                    "documents",
                    group
                        .iter()
                        .map(|m| match m {
                            WriteModel::InsertOne(d) => {
                                let mut d = d.clone();
                                if !d.contains_key("_id") {
                                    d.insert_first("_id", super::bson::ObjectId::new());
                                }
                                d
                            }
                            _ => unreachable!(),
                        })
                        .collect(),
                ),
                1 => (
                    "update",
                    "updates",
                    group
                        .iter()
                        .map(|m| match m {
                            WriteModel::UpdateOne { filter, update, upsert } => Document::new().with("q", filter.clone()).with("u", update.clone()).with("multi", false).with("upsert", *upsert),
                            WriteModel::UpdateMany { filter, update, upsert } => Document::new().with("q", filter.clone()).with("u", update.clone()).with("multi", true).with("upsert", *upsert),
                            WriteModel::ReplaceOne { filter, replacement, upsert } => Document::new().with("q", filter.clone()).with("u", replacement.clone()).with("multi", false).with("upsert", *upsert),
                            _ => unreachable!(),
                        })
                        .collect(),
                ),
                _ => (
                    "delete",
                    "deletes",
                    group
                        .iter()
                        .map(|m| match m {
                            WriteModel::DeleteOne(f) => Document::new().with("q", f.clone()).with("limit", 1),
                            WriteModel::DeleteMany(f) => Document::new().with("q", f.clone()).with("limit", 0),
                            _ => unreachable!(),
                        })
                        .collect(),
                ),
            };
            let mut msg = OpMsg::new(Document::new().with(cmd, coll).with("ordered", ordered).with("$db", db));
            msg.sequences.push((seq.into(), docs));
            let r = self.with_connection(|c| c.command_msg(&msg))?;
            let n = r.get_i64("n").unwrap_or(0);
            let upserted: Vec<(i64, Bson)> = r.get_array("upserted").map(|a| a.iter().filter_map(|u| u.as_document()).map(|u| (start as i64 + u.get_i64("index").unwrap_or(0), u.get("_id").cloned().unwrap_or(Bson::Null))).collect()).unwrap_or_default();
            match kind {
                0 => res.inserted += n,
                1 => {
                    res.matched += n - upserted.len() as i64;
                    res.modified += r.get_i64("nModified").unwrap_or(0);
                }
                _ => res.deleted += n,
            }
            res.upserts.extend(upserted);
            if let Some(errs) = r.get_array("writeErrors") {
                for e in errs.iter().filter_map(|e| e.as_document()) {
                    res.errors.push((start as i64 + e.get_i64("index").unwrap_or(0), e.get_i64("code").unwrap_or(0), e.get_str("errmsg").unwrap_or("").to_string()));
                }
                if ordered && !errs.is_empty() {
                    break;
                }
            }
        }
        Ok(res)
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
        assert!(MongoClientOptions::parse("mongodb+srv://a:1").is_err());
        assert!(MongoClientOptions::parse("mongodb://h/?authMechanism=GSSAPI").is_err());
        assert!(MongoClientOptions::parse("mongodb://u:p@h/?authMechanism=MONGODB-X509").is_err());
        let o = MongoClientOptions::parse("mongodb://h/?authMechanism=scram-sha-1&replicaSet=rs0&directConnection=true").unwrap();
        assert_eq!(o.auth_mechanism.as_deref(), Some("SCRAM-SHA-1"));
        assert_eq!(o.replica_set.as_deref(), Some("rs0"));
        assert!(o.direct_connection);
        // driver spec example: user "user", password "pencil"
        assert_eq!(mongo_password_digest("user", "pencil"), "1c33006ec1ffd90f9cadcbcc0e118200");
        let o = MongoClientOptions::parse("mongodb://localhost").unwrap();
        assert_eq!(o.hosts, vec![("localhost".to_string(), 27017)]);
    }
}

//! Test helpers: an embedded PostgreSQL wire-protocol server backed by the rustsql engine.
//!
//! [`PgTestServer`] listens on an ephemeral port and speaks the subset of protocol v3 that
//! the [`crate::pg`] client uses: startup (SSL requests are declined, or accepted with
//! [`PgTestServer::start_tls`]), authentication (trust,
//! cleartext, MD5, SCRAM-SHA-256), simple query (multi-statement), extended query
//! (Parse/Bind/Describe/Execute/Sync/Close/Flush) with text parameters, and transactions.
//!
//! ```no_run
//! use cameleer_sql::testing::{PgAuth, PgTestServer};
//! let server = PgTestServer::start("shop", PgAuth::ScramSha256, &[("camel", "secret")]).unwrap();
//! let url = server.url("camel", "secret"); // jdbc:postgresql://127.0.0.1:<port>/shop?user=camel&password=secret
//! ```

use std::collections::{BTreeMap, HashMap};
use std::io::{self, BufReader};
use std::net::{TcpListener, TcpStream};

use cameleer_tls::net::NetStream;
use cameleer_tls::TlsServerConfig;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use cameleer_core::value::Value;

use crate::datasource::{ExecuteOptions, QueryResult};
use crate::engine::ast::Statement;
use crate::engine::{parser, EmbeddedConnection, SqlErr};
use crate::pg::crypto::{b64, b64_decode, hmac_sha256, md5_password, nonce, sha256, xor32, ScramKeys};
use crate::pg::protocol::*;

/// Authentication method required by the test server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PgAuth {
    Trust,
    Cleartext,
    Md5,
    ScramSha256,
}

struct UserEntry {
    password: String,
    salt: Vec<u8>,
    keys: ScramKeys,
}

struct Shared {
    tls: Option<Arc<TlsServerConfig>>,
    /// Whether the TLS server refuses plain (non-SSL) sessions.
    require_tls: bool,
    db: String,
    auth: PgAuth,
    iterations: u32,
    users: HashMap<String, UserEntry>,
    stop: AtomicBool,
    connections: AtomicUsize,
    auth_failures: AtomicUsize,
    queries: Mutex<Vec<String>>,
}

/// An embedded PostgreSQL-protocol server for tests.
pub struct PgTestServer {
    shared: Arc<Shared>,
    port: u16,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl PgTestServer {
    /// Start a server on `127.0.0.1:0` serving the rustsql database `db` (SCRAM uses 4096 iterations).
    pub fn start(db: &str, auth: PgAuth, users: &[(&str, &str)]) -> io::Result<PgTestServer> {
        Self::start_with_iterations(db, auth, users, 4096)
    }

    /// Like [`PgTestServer::start`] with a custom SCRAM iteration count.
    pub fn start_with_iterations(db: &str, auth: PgAuth, users: &[(&str, &str)], iterations: u32) -> io::Result<PgTestServer> {
        Self::start_inner(db, auth, users, iterations, None)
    }

    /// Like [`PgTestServer::start`], accepting SSLRequest with the given TLS configuration
    /// (and refusing sessions that did not negotiate TLS, like `hostssl` in pg_hba.conf).
    pub fn start_tls(db: &str, auth: PgAuth, users: &[(&str, &str)], tls: TlsServerConfig) -> io::Result<PgTestServer> {
        Self::start_inner(db, auth, users, 4096, Some(Arc::new(tls)))
    }

    fn start_inner(db: &str, auth: PgAuth, users: &[(&str, &str)], iterations: u32, tls: Option<Arc<TlsServerConfig>>) -> io::Result<PgTestServer> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let users = users
            .iter()
            .map(|(u, p)| {
                let salt = nonce(16).into_bytes();
                let keys = ScramKeys::derive(p, &salt, iterations);
                (u.to_string(), UserEntry { password: p.to_string(), salt, keys })
            })
            .collect();
        let shared = Arc::new(Shared {
            require_tls: tls.is_some(),
            tls,
            db: db.to_string(),
            auth,
            iterations,
            users,
            stop: AtomicBool::new(false),
            connections: AtomicUsize::new(0),
            auth_failures: AtomicUsize::new(0),
            queries: Mutex::new(Vec::new()),
        });
        let s2 = shared.clone();
        let handle = std::thread::Builder::new().name(format!("pg-test-server-{}", port)).spawn(move || {
            for stream in listener.incoming() {
                if s2.stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                s2.connections.fetch_add(1, Ordering::SeqCst);
                let s3 = s2.clone();
                let _ = std::thread::Builder::new().name("pg-test-session".into()).spawn(move || {
                    let _ = Session::run(s3, stream);
                });
            }
        })?;
        Ok(PgTestServer { shared, port, handle: Mutex::new(Some(handle)) })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The database name served.
    pub fn database(&self) -> &str {
        &self.shared.db
    }

    /// A `jdbc:postgresql://` url for this server.
    pub fn url(&self, user: &str, password: &str) -> String {
        format!("jdbc:postgresql://127.0.0.1:{}/{}?user={}&password={}", self.port, self.shared.db, user, password)
    }

    /// Number of accepted connections.
    pub fn connection_count(&self) -> usize {
        self.shared.connections.load(Ordering::SeqCst)
    }

    /// Number of failed authentications.
    pub fn auth_failures(&self) -> usize {
        self.shared.auth_failures.load(Ordering::SeqCst)
    }

    /// SQL texts received (simple queries and Parse messages).
    pub fn received_queries(&self) -> Vec<String> {
        self.shared.queries.lock().unwrap().clone()
    }

    /// Stop accepting connections.
    pub fn stop(&self) {
        if self.shared.stop.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(h) = self.handle.lock().unwrap().take() {
            let _ = h.join();
        }
    }
}

impl Drop for PgTestServer {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Portal {
    stmt: Option<Statement>,
    params: Vec<Value>,
    result: Option<QueryResult>,
}

struct Session {
    shared: Arc<Shared>,
    reader: BufReader<NetStream>,
    writer: NetStream,
    conn: EmbeddedConnection,
    statements: HashMap<String, (Option<Statement>, Vec<i32>)>,
    portals: HashMap<String, Portal>,
    out: Vec<u8>,
}

fn sql_state(e: &SqlErr) -> &'static str {
    e.code
}

impl Session {
    fn run(shared: Arc<Shared>, stream: TcpStream) -> io::Result<()> {
        let _ = stream.set_nodelay(true);
        let stream = NetStream::plain(stream);
        let writer = stream.try_clone()?;
        let db = shared.db.clone();
        let mut s = Session {
            shared,
            reader: BufReader::new(stream),
            writer,
            conn: EmbeddedConnection::open(&db),
            statements: HashMap::new(),
            portals: HashMap::new(),
            out: Vec::new(),
        };
        if s.shared.stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        if !s.startup()? {
            return Ok(());
        }
        s.serve()
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.out.is_empty() {
            let out = std::mem::take(&mut self.out);
            send(&mut self.writer, &out)?;
        }
        Ok(())
    }

    fn push(&mut self, tag: u8, body: &[u8]) {
        self.out.extend(message(tag, body));
    }

    fn fatal(&mut self, code: &str, msg: &str) -> io::Result<bool> {
        self.push(b'E', &error_body("FATAL", code, msg));
        self.flush()?;
        Ok(false)
    }

    fn auth_failed(&mut self, user: &str) -> io::Result<bool> {
        self.shared.auth_failures.fetch_add(1, Ordering::SeqCst);
        self.fatal("28P01", &format!("password authentication failed for user \"{}\"", user))
    }

    fn read_password_message(&mut self) -> io::Result<Vec<u8>> {
        let (tag, body) = read_message(&mut self.reader)?;
        if tag != b'p' {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "expected password message"));
        }
        Ok(body)
    }

    fn startup(&mut self) -> io::Result<bool> {
        let body = loop {
            let body = read_untagged(&mut self.reader)?;
            let code = Buf::new(&body).i32()?;
            match code {
                SSL_REQUEST if self.shared.tls.is_some() && !self.writer.is_tls() => {
                    send(&mut self.writer, b"S")?;
                    let cfg = self.shared.tls.clone().expect("tls");
                    let plain = self.writer.try_clone()?;
                    let secured = plain.start_tls_server(&cfg)?;
                    self.reader = BufReader::new(secured.try_clone()?);
                    self.writer = secured;
                }
                SSL_REQUEST | GSSENC_REQUEST => {
                    send(&mut self.writer, b"N")?;
                }
                CANCEL_REQUEST => return Ok(false),
                PROTOCOL_V3 if self.shared.require_tls && !self.writer.is_tls() => {
                    return self.fatal("28000", "no pg_hba.conf entry for host: SSL off");
                }
                PROTOCOL_V3 => break body,
                other => return self.fatal("0A000", &format!("unsupported frontend protocol {}", other)),
            }
        };
        let mut b = Buf::new(&body[4..]);
        let mut params = BTreeMap::new();
        loop {
            let k = b.cstr()?;
            if k.is_empty() {
                break;
            }
            let v = b.cstr()?;
            params.insert(k, v);
        }
        let user = params.get("user").cloned().unwrap_or_default();
        let database = params.get("database").cloned().unwrap_or_else(|| user.clone());
        if database != self.shared.db {
            return self.fatal("3D000", &format!("database \"{}\" does not exist", database));
        }
        let auth = self.shared.auth;
        if auth != PgAuth::Trust {
            let Some((password, salt, stored_key, server_key)) =
                self.shared.users.get(&user).map(|u| (u.password.clone(), u.salt.clone(), u.keys.stored_key, u.keys.server_key))
            else {
                // still run the exchange shape for the method, then fail
                return self.auth_failed(&user);
            };
            match auth {
                PgAuth::Cleartext => {
                    let mut r = Vec::new();
                    put_i32(&mut r, 3);
                    self.push(b'R', &r);
                    self.flush()?;
                    let body = self.read_password_message()?;
                    if Buf::new(&body).cstr()? != password {
                        return self.auth_failed(&user);
                    }
                }
                PgAuth::Md5 => {
                    let salt4: Vec<u8> = nonce(4).into_bytes();
                    let mut r = Vec::new();
                    put_i32(&mut r, 5);
                    r.extend_from_slice(&salt4);
                    self.push(b'R', &r);
                    self.flush()?;
                    let body = self.read_password_message()?;
                    if Buf::new(&body).cstr()? != md5_password(&user, &password, &salt4) {
                        return self.auth_failed(&user);
                    }
                }
                PgAuth::ScramSha256 => {
                    let mut r = Vec::new();
                    put_i32(&mut r, 10);
                    put_cstr(&mut r, "SCRAM-SHA-256");
                    r.push(0);
                    self.push(b'R', &r);
                    self.flush()?;
                    let body = self.read_password_message()?;
                    let mut b = Buf::new(&body);
                    let mech = b.cstr()?;
                    if mech != "SCRAM-SHA-256" {
                        return self.fatal("28000", &format!("unsupported SASL mechanism {}", mech));
                    }
                    let len = b.i32()?;
                    let first = String::from_utf8_lossy(b.bytes(len.max(0) as usize)?).into_owned();
                    let Some(bare) = first.strip_prefix("n,,").or_else(|| first.strip_prefix("y,,")) else {
                        return self.fatal("28000", "channel binding is not supported");
                    };
                    let bare = bare.to_string();
                    let Some(cnonce) = bare.split(',').find_map(|p| p.strip_prefix("r=")) else {
                        return self.fatal("08P01", "invalid SCRAM client-first-message");
                    };
                    let snonce = format!("{}{}", cnonce, nonce(18));
                    let server_first = format!("r={},s={},i={}", snonce, b64(&salt), self.shared.iterations);
                    let mut r = Vec::new();
                    put_i32(&mut r, 11);
                    r.extend_from_slice(server_first.as_bytes());
                    self.push(b'R', &r);
                    self.flush()?;
                    let body = self.read_password_message()?;
                    let fin = String::from_utf8_lossy(&body).into_owned();
                    let Some(pidx) = fin.rfind(",p=") else {
                        return self.fatal("08P01", "invalid SCRAM client-final-message");
                    };
                    let without_proof = &fin[..pidx];
                    let proof = b64_decode(&fin[pidx + 3..]).unwrap_or_default();
                    let ok_nonce = without_proof.split(',').any(|p| p == format!("r={}", snonce));
                    let ok_cb = without_proof.split(',').any(|p| p == "c=biws" || p == "c=eSws");
                    let auth_msg = format!("{},{},{}", bare, server_first, without_proof);
                    let sig = hmac_sha256(&stored_key, auth_msg.as_bytes());
                    let valid = proof.len() == 32 && {
                        let mut p = [0u8; 32];
                        p.copy_from_slice(&proof);
                        sha256(&xor32(&p, &sig)) == stored_key
                    };
                    if !(ok_nonce && ok_cb && valid) {
                        return self.auth_failed(&user);
                    }
                    let mut r = Vec::new();
                    put_i32(&mut r, 12);
                    r.extend_from_slice(format!("v={}", b64(&hmac_sha256(&server_key, auth_msg.as_bytes()))).as_bytes());
                    self.push(b'R', &r);
                }
                PgAuth::Trust => {}
            }
        }
        let mut r = Vec::new();
        put_i32(&mut r, 0);
        self.push(b'R', &r);
        for (k, v) in [
            ("server_version", "14.0 (rustsql)"),
            ("server_encoding", "UTF8"),
            ("client_encoding", "UTF8"),
            ("DateStyle", "ISO, MDY"),
            ("integer_datetimes", "on"),
            ("standard_conforming_strings", "on"),
            ("TimeZone", "UTC"),
        ] {
            let mut p = Vec::new();
            put_cstr(&mut p, k);
            put_cstr(&mut p, v);
            self.push(b'S', &p);
        }
        let mut k = Vec::new();
        put_i32(&mut k, std::process::id() as i32);
        put_i32(&mut k, 12345);
        self.push(b'K', &k);
        self.ready();
        self.flush()?;
        Ok(true)
    }

    fn ready(&mut self) {
        let st = self.conn.transaction_status();
        self.push(b'Z', &[st]);
    }

    fn send_error(&mut self, e: &SqlErr) {
        self.push(b'E', &error_body("ERROR", sql_state(e), &e.message));
    }

    fn row_description(&mut self, r: &QueryResult) {
        let mut b = Vec::new();
        put_i16(&mut b, r.columns.len() as i16);
        for (i, c) in r.columns.iter().enumerate() {
            put_cstr(&mut b, c);
            put_i32(&mut b, 0);
            put_i16(&mut b, 0);
            put_i32(&mut b, oid_of(r.column_types.get(i).copied().unwrap_or(crate::datasource::ColumnType::Text)));
            put_i16(&mut b, -1);
            put_i32(&mut b, -1);
            put_i16(&mut b, 0);
        }
        self.push(b'T', &b);
    }

    fn data_rows(&mut self, r: &QueryResult) {
        for row in &r.rows {
            let mut b = Vec::new();
            put_i16(&mut b, row.len() as i16);
            for v in row {
                match encode_text(v) {
                    None => put_i32(&mut b, -1),
                    Some(bytes) => {
                        put_i32(&mut b, bytes.len() as i32);
                        b.extend_from_slice(&bytes);
                    }
                }
            }
            self.push(b'D', &b);
        }
    }

    fn complete(&mut self, r: &QueryResult) {
        let tag = match r.command.as_str() {
            "INSERT" => format!("INSERT 0 {}", r.update_count),
            "SELECT" => format!("SELECT {}", r.rows.len()),
            "UPDATE" | "DELETE" => format!("{} {}", r.command, r.update_count),
            "" => "SELECT 0".to_string(),
            other => other.to_string(),
        };
        let mut b = Vec::new();
        put_cstr(&mut b, &tag);
        self.push(b'C', &b);
    }

    fn exec(&mut self, stmt: &Statement, params: &[Value]) -> Result<QueryResult, SqlErr> {
        self.conn.execute_parsed(stmt, params, None, &ExecuteOptions::default())
    }

    fn simple_query(&mut self, sql: &str) {
        self.shared.queries.lock().unwrap().push(sql.to_string());
        if sql.trim().trim_matches(';').trim().is_empty() {
            self.push(b'I', &[]);
            return;
        }
        let stmts = match parser::parse_statements(sql) {
            Ok(s) => s,
            Err(e) => {
                // a parse error inside a transaction aborts it
                self.conn.mark_failed();
                self.send_error(&e);
                return;
            }
        };
        for s in &stmts {
            match self.exec(s, &[]) {
                Ok(r) => {
                    if r.is_query {
                        self.row_description(&r);
                        self.data_rows(&r);
                    }
                    self.complete(&r);
                }
                Err(e) => {
                    self.send_error(&e);
                    return;
                }
            }
        }
    }

    fn serve(&mut self) -> io::Result<()> {
        let mut skipping = false;
        loop {
            let (tag, body) = match read_message(&mut self.reader) {
                Ok(m) => m,
                Err(_) => return Ok(()),
            };
            if skipping && tag != b'S' && tag != b'X' {
                continue;
            }
            let mut b = Buf::new(&body);
            match tag {
                b'Q' => {
                    let sql = b.cstr()?;
                    self.simple_query(&sql);
                    self.ready();
                    self.flush()?;
                }
                b'P' => {
                    let name = b.cstr()?;
                    let sql = b.cstr()?;
                    let n = b.i16()?.max(0) as usize;
                    let mut oids = Vec::with_capacity(n);
                    for _ in 0..n {
                        oids.push(b.i32()?);
                    }
                    self.shared.queries.lock().unwrap().push(sql.clone());
                    match parser::parse_statements(&sql) {
                        Ok(mut v) if v.len() <= 1 => {
                            self.statements.insert(name, (v.pop(), oids));
                            self.push(b'1', &[]);
                        }
                        Ok(_) => {
                            self.send_error(&SqlErr::syntax("cannot insert multiple commands into a prepared statement"));
                            skipping = true;
                        }
                        Err(e) => {
                            self.send_error(&e);
                            skipping = true;
                        }
                    }
                }
                b'B' => {
                    let portal = b.cstr()?;
                    let stmt_name = b.cstr()?;
                    let nf = b.i16()?.max(0) as usize;
                    let mut formats = Vec::with_capacity(nf);
                    for _ in 0..nf {
                        formats.push(b.i16()?);
                    }
                    let np = b.i16()?.max(0) as usize;
                    let Some((stmt, oids)) = self.statements.get(&stmt_name).map(|(s, o)| (s.clone(), o.clone())) else {
                        self.send_error(&SqlErr::new("26000", format!("prepared statement \"{}\" does not exist", stmt_name)));
                        skipping = true;
                        continue;
                    };
                    let mut params = Vec::with_capacity(np);
                    let mut bad = false;
                    for i in 0..np {
                        let len = b.i32()?;
                        let fmt = if formats.len() == 1 { formats[0] } else { formats.get(i).copied().unwrap_or(0) };
                        if fmt != 0 {
                            bad = true;
                        }
                        if len < 0 {
                            params.push(Value::Null);
                        } else {
                            let raw = b.bytes(len as usize)?;
                            params.push(decode_param(oids.get(i).copied().unwrap_or(0), raw));
                        }
                    }
                    if bad {
                        self.send_error(&SqlErr::new("0A000", "binary parameter format is not supported"));
                        skipping = true;
                        continue;
                    }
                    self.portals.insert(portal, Portal { stmt, params, result: None });
                    self.push(b'2', &[]);
                }
                b'D' => {
                    let kind = b.u8()?;
                    let name = b.cstr()?;
                    if kind == b'S' {
                        let oids = self.statements.get(&name).map(|(_, o)| o.clone()).unwrap_or_default();
                        let mut p = Vec::new();
                        put_i16(&mut p, oids.len() as i16);
                        for o in oids {
                            put_i32(&mut p, o);
                        }
                        self.push(b't', &p);
                        self.push(b'n', &[]);
                        continue;
                    }
                    match self.run_portal(&name) {
                        Ok(Some(r)) if r.is_query => self.row_description(&r),
                        Ok(_) => self.push(b'n', &[]),
                        Err(e) => {
                            self.send_error(&e);
                            skipping = true;
                        }
                    }
                }
                b'E' => {
                    let name = b.cstr()?;
                    let max = b.i32()?;
                    match self.run_portal(&name) {
                        Ok(Some(mut r)) => {
                            if max > 0 && r.rows.len() > max as usize {
                                r.rows.truncate(max as usize);
                            }
                            if r.is_query {
                                self.data_rows(&r);
                            }
                            self.complete(&r);
                        }
                        Ok(None) => self.push(b'I', &[]),
                        Err(e) => {
                            self.send_error(&e);
                            skipping = true;
                        }
                    }
                }
                b'S' => {
                    skipping = false;
                    self.ready();
                    self.flush()?;
                }
                b'C' => {
                    let kind = b.u8()?;
                    let name = b.cstr()?;
                    if kind == b'S' {
                        self.statements.remove(&name);
                    } else {
                        self.portals.remove(&name);
                    }
                    self.push(b'3', &[]);
                }
                b'H' => self.flush()?,
                b'X' => return Ok(()),
                other => {
                    self.send_error(&SqlErr::new("08P01", format!("unsupported message type '{}'", other as char)));
                    self.ready();
                    self.flush()?;
                }
            }
        }
    }

    /// Execute a portal once (results are cached for Describe + Execute).
    fn run_portal(&mut self, name: &str) -> Result<Option<QueryResult>, SqlErr> {
        let Some(p) = self.portals.get(name) else {
            return Err(SqlErr::new("34000", format!("portal \"{}\" does not exist", name)));
        };
        if let Some(r) = &p.result {
            return Ok(Some(r.clone()));
        }
        let Some(stmt) = p.stmt.clone() else {
            return Ok(None);
        };
        let params = p.params.clone();
        let r = self.exec(&stmt, &params)?;
        if let Some(p) = self.portals.get_mut(name) {
            p.result = Some(r.clone());
        }
        Ok(Some(r))
    }
}

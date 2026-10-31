//! PostgreSQL wire protocol (v3) client implementing [`Connection`].

use std::collections::BTreeMap;
use std::io::{BufReader, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camel_core::error::{CamelError, Result};
use camel_core::util::{parse_query, url_decode};
use camel_core::value::Value;

use super::crypto::{b64, b64_decode, hmac_sha256, md5_password, nonce, xor32, ScramKeys};
use super::protocol::*;
use crate::datasource::{sql_error, Connection, DataSource, ExecuteOptions, QueryResult};

/// Connection settings.
#[derive(Clone, Debug)]
pub struct PgConfig {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
    pub password: Option<String>,
    pub connect_timeout: Duration,
    /// Socket read timeout (`socketTimeout`, seconds in the url; 0 = none).
    pub read_timeout: Option<Duration>,
    pub application_name: String,
    /// Maximum idle connections kept by [`PgDataSource`].
    pub pool_size: usize,
}

impl Default for PgConfig {
    fn default() -> Self {
        PgConfig {
            host: "localhost".into(),
            port: 5432,
            database: "postgres".into(),
            user: "postgres".into(),
            password: None,
            connect_timeout: Duration::from_secs(10),
            read_timeout: Some(Duration::from_secs(60)),
            application_name: "rust-camel".into(),
            pool_size: 8,
        }
    }
}

impl PgConfig {
    /// Parse `[jdbc:]postgresql://[user[:password]@]host[:port][/database][?user=&password=&connectTimeout=&socketTimeout=&ApplicationName=&poolSize=]`.
    pub fn from_url(url: &str) -> Result<Self> {
        let u = url.trim();
        let u = u.strip_prefix("jdbc:").unwrap_or(u);
        let rest = u
            .strip_prefix("postgresql://")
            .or_else(|| u.strip_prefix("postgres://"))
            .ok_or_else(|| CamelError::illegal_argument(format!("Not a postgresql url: {}", url)))?;
        let (main, query) = match rest.find('?') {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, ""),
        };
        let mut cfg = PgConfig::default();
        let (auth, hostpart) = match main.rfind('@') {
            Some(i) => (Some(&main[..i]), &main[i + 1..]),
            None => (None, main),
        };
        if let Some(a) = auth {
            match a.find(':') {
                Some(i) => {
                    cfg.user = url_decode(&a[..i]);
                    cfg.password = Some(url_decode(&a[i + 1..]));
                }
                None => cfg.user = url_decode(a),
            }
        }
        let (hp, db) = match hostpart.find('/') {
            Some(i) => (&hostpart[..i], &hostpart[i + 1..]),
            None => (hostpart, ""),
        };
        if !db.is_empty() {
            cfg.database = url_decode(db);
        }
        if hp.starts_with('[') {
            let end = hp.find(']').ok_or_else(|| CamelError::illegal_argument(format!("Invalid host in url: {}", url)))?;
            cfg.host = hp[1..end].to_string();
            if let Some(p) = hp[end + 1..].strip_prefix(':') {
                cfg.port = p.parse().map_err(|_| CamelError::illegal_argument(format!("Invalid port in url: {}", url)))?;
            }
        } else if let Some(i) = hp.rfind(':') {
            cfg.host = hp[..i].to_string();
            cfg.port = hp[i + 1..].parse().map_err(|_| CamelError::illegal_argument(format!("Invalid port in url: {}", url)))?;
        } else if !hp.is_empty() {
            cfg.host = hp.to_string();
        }
        for (k, v) in parse_query(query) {
            match k.as_str() {
                "user" => cfg.user = v,
                "password" => cfg.password = Some(v),
                "connectTimeout" => cfg.connect_timeout = Duration::from_secs(v.parse().unwrap_or(10)),
                "socketTimeout" => {
                    let s: u64 = v.parse().unwrap_or(0);
                    cfg.read_timeout = if s == 0 { None } else { Some(Duration::from_secs(s)) };
                }
                "ApplicationName" | "applicationName" => cfg.application_name = v,
                "poolSize" => cfg.pool_size = v.parse().unwrap_or(8),
                "sslmode" | "ssl" if v != "disable" && v != "false" && v != "allow" && v != "prefer" => {
                    return Err(CamelError::new(
                        camel_core::error::kinds::UNSUPPORTED_OPERATION,
                        format!("SSL connections are not supported by rust-camel (std only): {}", url),
                    ));
                }
                _ => {}
            }
        }
        Ok(cfg)
    }

    /// The url form (without password).
    pub fn url(&self) -> String {
        format!("jdbc:postgresql://{}:{}/{}?user={}", self.host, self.port, self.database, self.user)
    }
}

fn io_err(e: std::io::Error) -> CamelError {
    sql_error("08006", format!("I/O error communicating with PostgreSQL server: {}", e))
}

/// A connection to a PostgreSQL server.
pub struct PgConnection {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    status: u8,
    broken: bool,
    server_params: BTreeMap<String, String>,
}

struct Pending {
    result: QueryResult,
    oids: Vec<i32>,
}

impl PgConnection {
    /// Connect and authenticate.
    pub fn connect(cfg: &PgConfig) -> Result<PgConnection> {
        let addrs: Vec<_> = (cfg.host.as_str(), cfg.port)
            .to_socket_addrs()
            .map_err(|e| sql_error("08001", format!("Cannot resolve {}:{}: {}", cfg.host, cfg.port, e)))?
            .collect();
        let mut last = None;
        let mut stream = None;
        for a in addrs {
            match TcpStream::connect_timeout(&a, cfg.connect_timeout) {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(e) => last = Some(e),
            }
        }
        let stream = stream.ok_or_else(|| {
            sql_error(
                "08001",
                format!("Connection to {}:{} refused: {}", cfg.host, cfg.port, last.map(|e| e.to_string()).unwrap_or_else(|| "no address".into())),
            )
        })?;
        let _ = stream.set_nodelay(true);
        stream.set_read_timeout(cfg.read_timeout).map_err(io_err)?;
        let writer = stream.try_clone().map_err(io_err)?;
        let mut c = PgConnection { reader: BufReader::new(stream), writer, status: b'I', broken: false, server_params: BTreeMap::new() };
        c.startup(cfg)?;
        Ok(c)
    }

    /// Server parameters reported at startup (`server_version`, ...).
    pub fn server_parameters(&self) -> &BTreeMap<String, String> {
        &self.server_params
    }

    fn send(&mut self, bytes: &[u8]) -> Result<()> {
        if let Err(e) = send(&mut self.writer, bytes) {
            self.broken = true;
            return Err(io_err(e));
        }
        Ok(())
    }

    fn recv(&mut self) -> Result<(u8, Vec<u8>)> {
        match read_message(&mut self.reader) {
            Ok(m) => Ok(m),
            Err(e) => {
                self.broken = true;
                Err(io_err(e))
            }
        }
    }

    fn server_error(body: &[u8]) -> CamelError {
        let (code, msg, detail) = parse_error_fields(body);
        let msg = match detail {
            Some(d) if !msg.contains(&d) => format!("{}: {}", msg, d),
            _ => msg,
        };
        sql_error(&code, msg)
    }

    fn startup(&mut self, cfg: &PgConfig) -> Result<()> {
        let mut body = Vec::new();
        put_i32(&mut body, PROTOCOL_V3);
        for (k, v) in [("user", cfg.user.as_str()), ("database", cfg.database.as_str()), ("application_name", cfg.application_name.as_str()), ("client_encoding", "UTF8")] {
            put_cstr(&mut body, k);
            put_cstr(&mut body, v);
        }
        body.push(0);
        self.send(&untagged(&body))?;
        let password = || cfg.password.clone().ok_or_else(|| sql_error("28P01", format!("The server requested password-based authentication, but no password was provided for user \"{}\"", cfg.user)));
        let mut scram: Option<(String, String)> = None; // (client nonce, client-first-bare)
        let mut scram_expect: Option<[u8; 32]> = None;
        loop {
            let (tag, body) = self.recv()?;
            let mut b = Buf::new(&body);
            match tag {
                b'R' => {
                    let code = b.i32().map_err(io_err)?;
                    match code {
                        0 => {}
                        3 => {
                            let mut m = Vec::new();
                            put_cstr(&mut m, &password()?);
                            self.send(&message(b'p', &m))?;
                        }
                        5 => {
                            let salt = b.bytes(4).map_err(io_err)?.to_vec();
                            let mut m = Vec::new();
                            put_cstr(&mut m, &md5_password(&cfg.user, &password()?, &salt));
                            self.send(&message(b'p', &m))?;
                        }
                        10 => {
                            let mut mechs = Vec::new();
                            while let Ok(s) = b.cstr() {
                                if s.is_empty() {
                                    break;
                                }
                                mechs.push(s);
                            }
                            if !mechs.iter().any(|m| m == "SCRAM-SHA-256") {
                                return Err(sql_error("28000", format!("None of the server's SASL authentication mechanisms are supported: {:?}", mechs)));
                            }
                            password()?;
                            let cn = nonce(24);
                            let bare = format!("n=,r={}", cn);
                            let first = format!("n,,{}", bare);
                            let mut m = Vec::new();
                            put_cstr(&mut m, "SCRAM-SHA-256");
                            put_i32(&mut m, first.len() as i32);
                            m.extend_from_slice(first.as_bytes());
                            self.send(&message(b'p', &m))?;
                            scram = Some((cn, bare));
                        }
                        11 => {
                            let (cn, bare) = scram.take().ok_or_else(|| sql_error("08P01", "unexpected SASLContinue"))?;
                            let server_first = String::from_utf8_lossy(b.rest()).into_owned();
                            let mut r = None;
                            let mut s = None;
                            let mut it = None;
                            for part in server_first.split(',') {
                                if let Some(v) = part.strip_prefix("r=") {
                                    r = Some(v.to_string());
                                } else if let Some(v) = part.strip_prefix("s=") {
                                    s = Some(v.to_string());
                                } else if let Some(v) = part.strip_prefix("i=") {
                                    it = v.parse::<u32>().ok();
                                }
                            }
                            let (r, s, it) = match (r, s, it) {
                                (Some(r), Some(s), Some(i)) => (r, s, i),
                                _ => return Err(sql_error("08P01", format!("invalid SCRAM server-first-message: {}", server_first))),
                            };
                            if !r.starts_with(&cn) {
                                return Err(sql_error("08P01", "SCRAM server nonce does not start with the client nonce"));
                            }
                            let salt = b64_decode(&s).ok_or_else(|| sql_error("08P01", "invalid SCRAM salt"))?;
                            let keys = ScramKeys::derive(&password()?, &salt, it);
                            let without_proof = format!("c=biws,r={}", r);
                            let auth = format!("{},{},{}", bare, server_first, without_proof);
                            let sig = hmac_sha256(&keys.stored_key, auth.as_bytes());
                            let proof = xor32(&keys.client_key, &sig);
                            scram_expect = Some(hmac_sha256(&keys.server_key, auth.as_bytes()));
                            let fin = format!("{},p={}", without_proof, b64(&proof));
                            self.send(&message(b'p', fin.as_bytes()))?;
                        }
                        12 => {
                            let data = String::from_utf8_lossy(b.rest()).into_owned();
                            let expected = scram_expect.take().ok_or_else(|| sql_error("08P01", "unexpected SASLFinal"))?;
                            let v = data.split(',').find_map(|p| p.strip_prefix("v=")).and_then(b64_decode);
                            if v.as_deref() != Some(&expected[..]) {
                                return Err(sql_error("28000", "SCRAM server signature verification failed"));
                            }
                        }
                        other => return Err(sql_error("28000", format!("Authentication method {} is not supported", other))),
                    }
                }
                b'S' => {
                    let k = b.cstr().map_err(io_err)?;
                    let v = b.cstr().map_err(io_err)?;
                    self.server_params.insert(k, v);
                }
                b'K' | b'N' => {}
                b'E' => {
                    self.broken = true;
                    return Err(Self::server_error(&body));
                }
                b'Z' => {
                    self.status = b.u8().unwrap_or(b'I');
                    return Ok(());
                }
                other => return Err(sql_error("08P01", format!("unexpected message '{}' during startup", other as char))),
            }
        }
    }

    /// Read responses until ReadyForQuery. Returns the result of the last statement.
    fn read_results(&mut self) -> Result<QueryResult> {
        let mut cur = Pending { result: QueryResult::default(), oids: Vec::new() };
        let mut last: Option<QueryResult> = None;
        let mut error: Option<CamelError> = None;
        loop {
            let (tag, body) = self.recv()?;
            let mut b = Buf::new(&body);
            match tag {
                b'1' | b'2' | b'3' | b'N' | b'S' | b't' | b'A' => {}
                b'n' => {}
                b'T' => {
                    let n = b.i16().map_err(io_err)?.max(0) as usize;
                    cur = Pending { result: QueryResult { is_query: true, ..Default::default() }, oids: Vec::with_capacity(n) };
                    for _ in 0..n {
                        let name = b.cstr().map_err(io_err)?;
                        b.bytes(6).map_err(io_err)?;
                        let o = b.i32().map_err(io_err)?;
                        b.bytes(8).map_err(io_err)?;
                        cur.result.columns.push(name);
                        cur.result.column_types.push(column_type_of(o));
                        cur.oids.push(o);
                    }
                }
                b'D' => {
                    let n = b.i16().map_err(io_err)?.max(0) as usize;
                    let mut row = Vec::with_capacity(n);
                    for i in 0..n {
                        let len = b.i32().map_err(io_err)?;
                        if len < 0 {
                            row.push(Value::Null);
                        } else {
                            let raw = b.bytes(len as usize).map_err(io_err)?;
                            row.push(decode_text(cur.oids.get(i).copied().unwrap_or(oid::TEXT), raw));
                        }
                    }
                    cur.result.rows.push(row);
                }
                b'C' | b's' => {
                    let mut r = std::mem::take(&mut cur.result);
                    if tag == b'C' {
                        let t = b.cstr().map_err(io_err)?;
                        r.update_count = if r.is_query && !t.starts_with("SELECT") && !t.starts_with("INSERT") && !t.starts_with("UPDATE") && !t.starts_with("DELETE") {
                            r.rows.len() as i64
                        } else {
                            tag_count(&t)
                        };
                        r.command = t.split(' ').next().unwrap_or("").to_string();
                        if t.starts_with("CREATE TABLE") || t.starts_with("DROP TABLE") {
                            r.command = t.clone();
                        }
                    } else {
                        r.update_count = r.rows.len() as i64;
                        r.command = "SELECT".into();
                    }
                    cur.oids.clear();
                    last = Some(r);
                }
                b'I' => last = Some(QueryResult::default()),
                b'E' => {
                    if error.is_none() {
                        error = Some(Self::server_error(&body));
                    }
                }
                b'Z' => {
                    self.status = b.u8().unwrap_or(b'I');
                    if let Some(e) = error {
                        return Err(e);
                    }
                    return Ok(last.unwrap_or_default());
                }
                other => {
                    self.broken = true;
                    return Err(sql_error("08P01", format!("unexpected message '{}' from server", other as char)));
                }
            }
        }
    }

    /// Run a simple query (may contain several statements).
    pub fn simple_query(&mut self, sql: &str) -> Result<QueryResult> {
        let mut m = Vec::new();
        put_cstr(&mut m, sql);
        self.send(&message(b'Q', &m))?;
        self.read_results()
    }

    /// Run an extended query (Parse/Bind/Describe/Execute/Sync) with text parameters.
    pub fn extended_query(&mut self, sql: &str, params: &[Value], max_rows: usize) -> Result<QueryResult> {
        let mut out = Vec::new();
        let mut p = Vec::new();
        put_cstr(&mut p, "");
        put_cstr(&mut p, sql);
        put_i16(&mut p, params.len() as i16);
        for v in params {
            put_i32(&mut p, param_oid(v));
        }
        out.extend(message(b'P', &p));
        let mut b = Vec::new();
        put_cstr(&mut b, "");
        put_cstr(&mut b, "");
        put_i16(&mut b, 0);
        put_i16(&mut b, params.len() as i16);
        for v in params {
            match encode_text(v) {
                None => put_i32(&mut b, -1),
                Some(bytes) => {
                    put_i32(&mut b, bytes.len() as i32);
                    b.extend_from_slice(&bytes);
                }
            }
        }
        put_i16(&mut b, 0);
        out.extend(message(b'B', &b));
        out.extend(message(b'D', b"P\0"));
        let mut e = Vec::new();
        put_cstr(&mut e, "");
        put_i32(&mut e, max_rows.min(i32::MAX as usize) as i32);
        out.extend(message(b'E', &e));
        out.extend(message(b'S', &[]));
        self.send(&out)?;
        self.read_results()
    }

    /// Transaction status byte from the last ReadyForQuery (`I`, `T`, `E`).
    pub fn transaction_status(&self) -> u8 {
        self.status
    }
}

fn wants_returning(sql: &str) -> bool {
    let t = sql.trim_start().to_ascii_lowercase();
    t.starts_with("insert") && !t.contains("returning") && !t.trim_end().trim_end_matches(';').contains(';')
}

impl Connection for PgConnection {
    fn execute_with(&mut self, sql: &str, params: &[Value], options: &ExecuteOptions) -> Result<QueryResult> {
        let keys = options.return_generated_keys && wants_returning(sql);
        let sql_owned;
        let mut sql = sql;
        if keys {
            sql_owned = format!("{} RETURNING *", sql.trim_end().trim_end_matches(';'));
            sql = &sql_owned;
        }
        let mut r = if params.is_empty() && !sql.contains('?') {
            let mut r = self.simple_query(sql)?;
            if options.max_rows > 0 {
                r.rows.truncate(options.max_rows);
            }
            r
        } else {
            let converted = if sql.contains('?') { question_to_dollar(sql) } else { sql.to_string() };
            self.extended_query(&converted, params, options.max_rows)?
        };
        if keys {
            r.generated_keys = r.rows_as_maps();
            r.rows.clear();
            r.columns.clear();
            r.column_types.clear();
            r.is_query = false;
        }
        Ok(r)
    }
    fn begin(&mut self) -> Result<()> {
        self.simple_query("BEGIN").map(|_| ())
    }
    fn commit(&mut self) -> Result<()> {
        let failed = self.status == b'E';
        let r = self.simple_query("COMMIT")?;
        if failed || r.command == "ROLLBACK" {
            return Err(sql_error("40000", "transaction was aborted by an earlier error and has been rolled back"));
        }
        Ok(())
    }
    fn rollback(&mut self) -> Result<()> {
        self.simple_query("ROLLBACK").map(|_| ())
    }
    fn in_transaction(&self) -> bool {
        self.status != b'I'
    }
    fn is_valid(&self) -> bool {
        !self.broken
    }
}

impl Drop for PgConnection {
    fn drop(&mut self) {
        if !self.broken {
            let _ = self.writer.write_all(&message(b'X', &[]));
        }
    }
}

struct Pool {
    cfg: PgConfig,
    idle: Mutex<Vec<PgConnection>>,
}

/// A data source for a PostgreSQL server with a small idle-connection pool.
#[derive(Clone)]
pub struct PgDataSource {
    pool: Arc<Pool>,
}

impl PgDataSource {
    pub fn new(cfg: PgConfig) -> Self {
        PgDataSource { pool: Arc::new(Pool { cfg, idle: Mutex::new(Vec::new()) }) }
    }
    /// Create from a `jdbc:postgresql://` url.
    pub fn from_url(url: &str) -> Result<Self> {
        Ok(Self::new(PgConfig::from_url(url)?))
    }
    pub fn config(&self) -> &PgConfig {
        &self.pool.cfg
    }
    /// Number of idle pooled connections.
    pub fn idle_connections(&self) -> usize {
        self.pool.idle.lock().unwrap().len()
    }
    /// Close all idle connections.
    pub fn close(&self) {
        self.pool.idle.lock().unwrap().clear();
    }
}

/// A pooled connection: returned to the pool when dropped (rolled back if needed).
pub struct PooledConnection {
    conn: Option<PgConnection>,
    pool: Arc<Pool>,
}

impl PooledConnection {
    fn inner(&mut self) -> &mut PgConnection {
        self.conn.as_mut().expect("connection")
    }
}

impl Drop for PooledConnection {
    fn drop(&mut self) {
        if let Some(mut c) = self.conn.take() {
            if c.broken {
                return;
            }
            if c.status != b'I' && c.simple_query("ROLLBACK").is_err() {
                return;
            }
            let mut idle = self.pool.idle.lock().unwrap();
            if idle.len() < self.pool.cfg.pool_size {
                idle.push(c);
            }
        }
    }
}

impl Connection for PooledConnection {
    fn execute_with(&mut self, sql: &str, params: &[Value], options: &ExecuteOptions) -> Result<QueryResult> {
        self.inner().execute_with(sql, params, options)
    }
    fn begin(&mut self) -> Result<()> {
        self.inner().begin()
    }
    fn commit(&mut self) -> Result<()> {
        self.inner().commit()
    }
    fn rollback(&mut self) -> Result<()> {
        self.inner().rollback()
    }
    fn in_transaction(&self) -> bool {
        self.conn.as_ref().is_some_and(|c| c.in_transaction())
    }
    fn is_valid(&self) -> bool {
        self.conn.as_ref().is_some_and(|c| c.is_valid())
    }
}

impl DataSource for PgDataSource {
    fn get_connection(&self) -> Result<Box<dyn Connection>> {
        let pooled = self.pool.idle.lock().unwrap().pop();
        let conn = match pooled {
            Some(c) if c.is_valid() => c,
            _ => PgConnection::connect(&self.pool.cfg)?,
        };
        Ok(Box::new(PooledConnection { conn: Some(conn), pool: self.pool.clone() }))
    }
    fn description(&self) -> String {
        self.pool.cfg.url()
    }
    fn key(&self) -> String {
        format!("{}#{:x}", self.pool.cfg.url(), Arc::as_ptr(&self.pool) as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_urls() {
        let c = PgConfig::from_url("jdbc:postgresql://db.example:5433/shop?user=camel&password=s3cret&socketTimeout=5").unwrap();
        assert_eq!((c.host.as_str(), c.port, c.database.as_str(), c.user.as_str()), ("db.example", 5433, "shop", "camel"));
        assert_eq!(c.password.as_deref(), Some("s3cret"));
        assert_eq!(c.read_timeout, Some(Duration::from_secs(5)));
        let c = PgConfig::from_url("postgresql://bob:pw%40x@localhost/test").unwrap();
        assert_eq!((c.user.as_str(), c.password.as_deref(), c.port, c.database.as_str()), ("bob", Some("pw@x"), 5432, "test"));
        assert!(PgConfig::from_url("jdbc:postgresql://h/db?sslmode=require").is_err());
        assert!(PgConfig::from_url("mysql://h/db").is_err());
        assert!(wants_returning("insert into t values (1)"));
        assert!(!wants_returning("insert into t values (1) returning id"));
        assert!(!wants_returning("update t set a = 1"));
    }

    #[test]
    fn refused_connection_is_sql_exception() {
        // bind then drop to get a (very likely) closed port
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let cfg = PgConfig { port, host: "127.0.0.1".into(), connect_timeout: Duration::from_secs(2), ..Default::default() };
        let e = PgConnection::connect(&cfg).err().unwrap();
        assert_eq!(e.kind, "SQLNonTransientConnectionException");
    }
}

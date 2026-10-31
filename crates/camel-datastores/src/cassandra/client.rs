//! CQL client: STARTUP, PasswordAuthenticator (SASL PLAIN), QUERY, PREPARE/EXECUTE
//! (with re-prepare on `Unprepared`), result decoding and paging.

use std::collections::HashMap;
use std::io::{BufReader, Write};
use std::sync::Mutex;
use std::time::Duration;

use camel_core::error::{CamelError, Result};
use camel_core::value::Value;
use camel_tls::camel_support::EndpointTls;
use camel_tls::net::NetStream;

use super::frame::{decode_value, encode_value, error_code, opcode, read_frame, read_rows_metadata, result_kind, ColumnSpec, Consistency, CqlType, Frame, QueryParams, Reader, Writer};
use crate::kinds;

/// Connection settings.
#[derive(Clone, Debug)]
pub struct CqlConfig {
    /// Contact points (tried in order).
    pub hosts: Vec<(String, u16)>,
    pub keyspace: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub tls: Option<EndpointTls>,
    pub connect_timeout: Duration,
    pub read_timeout: Option<Duration>,
}

impl CqlConfig {
    pub fn new(host: &str, port: u16) -> CqlConfig {
        CqlConfig { hosts: vec![(host.to_string(), port)], keyspace: None, username: None, password: None, tls: None, connect_timeout: Duration::from_secs(10), read_timeout: Some(Duration::from_secs(30)) }
    }
}

/// A prepared statement.
#[derive(Clone, Debug)]
pub struct Prepared {
    pub id: Vec<u8>,
    pub query: String,
    pub bind: Vec<ColumnSpec>,
    pub result: Vec<ColumnSpec>,
}

/// A page of rows.
#[derive(Clone, Debug, Default)]
pub struct Rows {
    pub columns: Vec<ColumnSpec>,
    pub rows: Vec<Vec<Value>>,
    pub paging_state: Option<Vec<u8>>,
}

impl Rows {
    /// Rows as objects (column name → value).
    pub fn to_objects(&self) -> Vec<Value> {
        self.rows.iter().map(|r| Value::Object(self.columns.iter().zip(r.iter()).map(|(c, v)| (c.name.clone(), v.clone())).collect())).collect()
    }
}

/// A decoded RESULT.
#[derive(Clone, Debug)]
pub enum CqlResult {
    Void,
    Rows(Rows),
    SetKeyspace(String),
    Prepared(Prepared),
    SchemaChange { change: String, target: String, keyspace: String, name: String },
}

/// Turns an ERROR frame body into an exception.
pub fn error_from_body(body: &[u8]) -> CamelError {
    let mut r = Reader::new(body);
    let code = r.int().unwrap_or(0);
    let msg = r.string().unwrap_or_default();
    let kind = match code {
        error_code::BAD_CREDENTIALS => kinds::CASSANDRA_AUTHENTICATION,
        error_code::SYNTAX_ERROR => kinds::CASSANDRA_SYNTAX,
        error_code::INVALID | error_code::ALREADY_EXISTS | error_code::CONFIG_ERROR | error_code::UNAUTHORIZED => kinds::CASSANDRA_INVALID_QUERY,
        _ => kinds::CASSANDRA_DRIVER,
    };
    let mut e = CamelError::new(kind, msg);
    e.message = format!("{} (error code 0x{:04x})", e.message, code);
    e
}

fn is_unprepared(e: &CamelError) -> bool {
    e.message.ends_with("(error code 0x2500)")
}

/// One connection.
pub struct CqlConnection {
    stream: NetStream,
    reader: BufReader<NetStream>,
    next_stream: i16,
    what: String,
    broken: bool,
}

impl CqlConnection {
    /// Connects to the first reachable contact point, authenticates and `USE`s the keyspace.
    pub fn connect(cfg: &CqlConfig) -> Result<CqlConnection> {
        let mut last = None;
        for (h, p) in &cfg.hosts {
            match Self::connect_host(cfg, h, *p) {
                Ok(c) => return Ok(c),
                Err(e) if e.kind == kinds::CASSANDRA_AUTHENTICATION || e.kind.starts_with("SSL") || e.kind == kinds::CASSANDRA_INVALID_QUERY => return Err(e),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| CamelError::illegal_argument("no Cassandra contact point")))
    }

    fn connect_host(cfg: &CqlConfig, host: &str, port: u16) -> Result<CqlConnection> {
        let what = format!("{}:{}", host, port);
        let stream = match &cfg.tls {
            Some(t) => t.connect(host, port, cfg.connect_timeout)?,
            None => NetStream::plain(camel_tls::net::connect_tcp(host, port, cfg.connect_timeout).map_err(|e| {
                CamelError::new(kinds::CASSANDRA_DRIVER, format!("Could not reach any contact point ({}): {}", what, e))
            })?),
        };
        let _ = stream.set_nodelay(true);
        stream.set_read_timeout(cfg.read_timeout).map_err(|e| CamelError::io(e.to_string()))?;
        let reader = BufReader::new(stream.try_clone().map_err(|e| CamelError::io(e.to_string()))?);
        let mut c = CqlConnection { stream, reader, next_stream: 0, what, broken: false };
        let mut w = Writer::new();
        w.string_map(&[("CQL_VERSION", "3.0.0"), ("DRIVER_NAME", "rust-camel")]);
        let f = c.round_trip(opcode::STARTUP, w.0)?;
        match f.opcode {
            opcode::READY => {}
            opcode::AUTHENTICATE => {
                let (Some(u), p) = (&cfg.username, cfg.password.as_deref().unwrap_or("")) else {
                    return Err(CamelError::new(kinds::CASSANDRA_AUTHENTICATION, format!("Authentication error on host {}: Host requires authentication, but no authenticator found in Cluster configuration", c.what)));
                };
                let mut w = Writer::new();
                w.bytes(Some(&camel_tls::sasl::plain_initial_response("", u, p)));
                let mut f = c.round_trip(opcode::AUTH_RESPONSE, w.0)?;
                while f.opcode == opcode::AUTH_CHALLENGE {
                    let mut w = Writer::new();
                    w.bytes(Some(&[]));
                    f = c.round_trip(opcode::AUTH_RESPONSE, w.0)?;
                }
                if f.opcode != opcode::AUTH_SUCCESS {
                    return Err(CamelError::new(kinds::CASSANDRA_AUTHENTICATION, format!("unexpected opcode {} during authentication", f.opcode)));
                }
            }
            other => return Err(CamelError::new(kinds::CASSANDRA_DRIVER, format!("unexpected opcode {} after STARTUP", other))),
        }
        if let Some(ks) = &cfg.keyspace {
            c.query(&format!("USE {}", quote_ident(ks)), QueryParams::new(Consistency::One))?;
        }
        Ok(c)
    }

    pub fn is_broken(&self) -> bool {
        self.broken
    }

    /// Sends a request and reads its response; ERROR frames become exceptions.
    pub fn round_trip(&mut self, op: u8, body: Vec<u8>) -> Result<Frame> {
        self.next_stream = (self.next_stream + 1) & 0x7fff;
        let stream_id = self.next_stream;
        let io = |e: std::io::Error, c: &mut CqlConnection| {
            c.broken = true;
            if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) {
                CamelError::new(kinds::CASSANDRA_DRIVER, format!("Query timed out after waiting on {}", c.what))
            } else {
                camel_tls::camel_support::io_to_camel(e, &format!("cql {}", c.what))
            }
        };
        if let Err(e) = self.stream.write_all(&Frame::request(stream_id, op, body).encode()).and_then(|_| self.stream.flush()) {
            return Err(io(e, self));
        }
        loop {
            let f = match read_frame(&mut self.reader) {
                Ok(f) => f,
                Err(e) => return Err(io(e, self)),
            };
            if f.stream != stream_id {
                continue; // events (stream -1) or stale responses
            }
            if f.opcode == opcode::ERROR {
                return Err(error_from_body(&f.body));
            }
            return Ok(f);
        }
    }

    /// Runs a QUERY.
    pub fn query(&mut self, cql: &str, params: QueryParams) -> Result<CqlResult> {
        let mut w = Writer::new();
        w.long_string(cql);
        params.write(&mut w);
        let f = self.round_trip(opcode::QUERY, w.0)?;
        decode_result(&f.body, &[])
    }

    /// PREPAREs a statement.
    pub fn prepare(&mut self, cql: &str) -> Result<Prepared> {
        let mut w = Writer::new();
        w.long_string(cql);
        let f = self.round_trip(opcode::PREPARE, w.0)?;
        match decode_result(&f.body, &[])? {
            CqlResult::Prepared(mut p) => {
                p.query = cql.to_string();
                Ok(p)
            }
            other => Err(CamelError::new(kinds::CASSANDRA_DRIVER, format!("unexpected PREPARE result {:?}", other))),
        }
    }

    /// EXECUTEs a prepared statement (values already encoded in `params`).
    pub fn execute(&mut self, p: &Prepared, params: QueryParams) -> Result<CqlResult> {
        let mut w = Writer::new();
        w.short_bytes(&p.id);
        params.write(&mut w);
        let f = self.round_trip(opcode::EXECUTE, w.0)?;
        decode_result(&f.body, &p.result)
    }
}

/// Double-quotes an identifier when needed.
pub fn quote_ident(s: &str) -> String {
    if s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') && !s.is_empty() {
        s.to_string()
    } else {
        format!("\"{}\"", s.replace('"', "\"\""))
    }
}

fn decode_rows(r: &mut Reader, known: &[ColumnSpec]) -> Result<Rows> {
    let (mut cols, paging, count) = read_rows_metadata(r)?;
    if cols.is_empty() && count > 0 {
        cols = known.to_vec();
    }
    let n = r.int()?.max(0) as usize;
    let mut rows = Vec::with_capacity(n);
    for _ in 0..n {
        let mut row = Vec::with_capacity(count);
        for i in 0..count {
            let t = cols.get(i).map(|c| c.cql_type.clone()).unwrap_or(CqlType::Blob);
            row.push(decode_value(&t, r.bytes()?.as_deref())?);
        }
        rows.push(row);
    }
    Ok(Rows { columns: cols, rows, paging_state: paging })
}

/// Decodes a RESULT body.
pub fn decode_result(body: &[u8], known: &[ColumnSpec]) -> Result<CqlResult> {
    let mut r = Reader::new(body);
    Ok(match r.int()? {
        result_kind::VOID => CqlResult::Void,
        result_kind::ROWS => CqlResult::Rows(decode_rows(&mut r, known)?),
        result_kind::SET_KEYSPACE => CqlResult::SetKeyspace(r.string()?),
        result_kind::PREPARED => {
            let id = r.short_bytes()?;
            // bind metadata: flags, column count, pk count + indexes, specs
            let flags = r.int()?;
            let count = r.int()?.max(0) as usize;
            let pk = r.int()?.max(0);
            for _ in 0..pk {
                r.short()?;
            }
            let global = if flags & 1 != 0 { Some((r.string()?, r.string()?)) } else { None };
            let mut bind = Vec::with_capacity(count);
            for _ in 0..count {
                let (keyspace, table) = match &global {
                    Some((k, t)) => (k.clone(), t.clone()),
                    None => (r.string()?, r.string()?),
                };
                let name = r.string()?;
                bind.push(ColumnSpec { keyspace, table, name, cql_type: r.cql_type()? });
            }
            let (result, _, _) = read_rows_metadata(&mut r)?;
            CqlResult::Prepared(Prepared { id, query: String::new(), bind, result })
        }
        result_kind::SCHEMA_CHANGE => {
            let change = r.string()?;
            let target = r.string()?;
            let keyspace = r.string()?;
            let name = if target != "KEYSPACE" && r.remaining() > 0 { r.string()? } else { String::new() };
            CqlResult::SchemaChange { change, target, keyspace, name }
        }
        other => return Err(CamelError::new(kinds::CASSANDRA_DRIVER, format!("unknown result kind {}", other))),
    })
}

/// Execution options.
#[derive(Clone, Debug)]
pub struct ExecOptions {
    pub consistency: Consistency,
    pub serial_consistency: Option<Consistency>,
    pub fetch_size: i32,
    /// Stop paging once this many rows were read.
    pub limit: Option<usize>,
    pub prepare: bool,
}

impl Default for ExecOptions {
    fn default() -> Self {
        ExecOptions { consistency: Consistency::LocalOne, serial_consistency: None, fetch_size: 5000, limit: None, prepare: true }
    }
}

/// A session (one lazily opened connection, reconnected after I/O errors, plus a
/// prepared-statement cache). Bind it as a bean and use `cql:bean:<name>`.
pub struct CqlSession {
    cfg: CqlConfig,
    conn: Mutex<Option<CqlConnection>>,
    prepared: Mutex<HashMap<String, Prepared>>,
}

impl CqlSession {
    pub fn new(cfg: CqlConfig) -> CqlSession {
        CqlSession { cfg, conn: Mutex::new(None), prepared: Mutex::new(HashMap::new()) }
    }

    pub fn config(&self) -> &CqlConfig {
        &self.cfg
    }

    fn with_conn<T>(&self, f: impl FnOnce(&mut CqlConnection) -> Result<T>) -> Result<T> {
        let mut g = self.conn.lock().unwrap();
        if g.as_ref().is_none_or(|c| c.is_broken()) {
            *g = Some(CqlConnection::connect(&self.cfg)?);
            self.prepared.lock().unwrap().clear();
        }
        let c = g.as_mut().unwrap();
        let r = f(c);
        if c.is_broken() {
            *g = None;
        }
        r
    }

    /// Runs a statement and returns the rows (all pages up to `limit`).
    pub fn execute(&self, cql: &str, values: &[Value], o: &ExecOptions) -> Result<Rows> {
        self.with_conn(|c| {
            let mut out = Rows::default();
            let mut paging: Option<Vec<u8>> = None;
            let prepared = if o.prepare {
                let cached = self.prepared.lock().unwrap().get(cql).cloned();
                Some(match cached {
                    Some(p) => p,
                    None => {
                        let p = c.prepare(cql)?;
                        self.prepared.lock().unwrap().insert(cql.to_string(), p.clone());
                        p
                    }
                })
            } else {
                None
            };
            loop {
                let mut params = QueryParams::new(o.consistency);
                params.serial_consistency = o.serial_consistency;
                params.page_size = Some(o.fetch_size.max(1));
                params.paging_state = paging.take();
                let res = match &prepared {
                    Some(p) => {
                        if values.len() != p.bind.len() {
                            return Err(CamelError::new(kinds::CASSANDRA_INVALID_QUERY, format!("Invalid amount of bind variables: expected {}, got {}", p.bind.len(), values.len())));
                        }
                        for (spec, v) in p.bind.iter().zip(values) {
                            params.values.push(encode_value(&spec.cql_type, v)?);
                        }
                        match c.execute(p, params.clone()) {
                            Err(e) if is_unprepared(&e) => {
                                let np = c.prepare(cql)?;
                                self.prepared.lock().unwrap().insert(cql.to_string(), np.clone());
                                c.execute(&np, params)?
                            }
                            r => r?,
                        }
                    }
                    None => {
                        for v in values {
                            params.values.push(encode_value(&CqlType::infer(v), v)?);
                        }
                        c.query(cql, params)?
                    }
                };
                match res {
                    CqlResult::Rows(r) => {
                        out.columns = r.columns;
                        out.rows.extend(r.rows);
                        if let Some(l) = o.limit {
                            if out.rows.len() >= l {
                                out.rows.truncate(l);
                                break;
                            }
                        }
                        match r.paging_state {
                            Some(p) => paging = Some(p),
                            None => break,
                        }
                    }
                    _ => break,
                }
            }
            Ok(out)
        })
    }

    /// Closes the connection.
    pub fn close(&self) {
        if let Some(c) = self.conn.lock().unwrap().take() {
            let _ = c.stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

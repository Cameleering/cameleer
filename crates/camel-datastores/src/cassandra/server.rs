//! Embedded Cassandra server for tests: CQL native protocol v4 (`OPTIONS`, `STARTUP`,
//! PasswordAuthenticator via `AUTH_RESPONSE`, `QUERY`, `PREPARE`, `EXECUTE`, `BATCH`,
//! `REGISTER`), result paging and TLS, backed by the in-memory [`engine`](super::engine).

use std::collections::{BTreeMap, HashMap};
use std::io::{self, BufReader, Write};
use std::sync::{Arc, Mutex};

use camel_core::value::Value;
use camel_tls::crypto::hash::HashAlg;
use camel_tls::net::NetStream;
use camel_tls::TlsServerConfig;

use super::engine::{self, CqlError, Outcome, Schema};
use super::frame::{decode_value, encode_value, error_code, opcode, read_frame, result_kind, write_rows_metadata, ColumnSpec, Consistency, Frame, QueryParams, Reader, Writer, VERSION};
use crate::util::TcpServer;

/// Server settings.
#[derive(Clone, Default)]
pub struct CassandraServerConfig {
    /// Users (name → password); when not empty the PasswordAuthenticator is required.
    pub users: BTreeMap<String, String>,
    pub tls: Option<Arc<TlsServerConfig>>,
}

struct Shared {
    cfg: CassandraServerConfig,
    schema: Mutex<Schema>,
    prepared: Mutex<HashMap<Vec<u8>, (String, Option<String>)>>,
    consistencies: Mutex<Vec<Consistency>>,
    queries: Mutex<Vec<String>>,
}

/// A running embedded Cassandra server (stopped on drop).
pub struct CassandraServer {
    shared: Arc<Shared>,
    tcp: TcpServer,
}

/// `schema_version` reported by every node (drivers wait for schema agreement on it).
const SCHEMA_VERSION: &str = "5c4e6d61-0000-4000-8000-000000000001";

impl CassandraServer {
    pub fn start() -> io::Result<CassandraServer> {
        Self::start_with(CassandraServerConfig::default())
    }

    pub fn start_with(cfg: CassandraServerConfig) -> io::Result<CassandraServer> {
        let tls = cfg.tls.clone();
        let mut schema = Schema::default();
        for q in [
            "CREATE KEYSPACE system WITH replication = {'class': 'LocalStrategy'}",
            "CREATE TABLE system.local (key text PRIMARY KEY, bootstrapped text, broadcast_address inet, broadcast_port int, cluster_name text, cql_version text, data_center text, host_id uuid, listen_address inet, listen_port int, native_protocol_version text, partitioner text, rack text, release_version text, rpc_address inet, rpc_port int, schema_version uuid, tokens set<text>)",
            "CREATE TABLE system.peers_v2 (peer inet, peer_port int, data_center text, host_id uuid, native_address inet, native_port int, preferred_ip inet, preferred_port int, rack text, release_version text, schema_version uuid, tokens set<text>, PRIMARY KEY ((peer), peer_port))",
            "CREATE TABLE system.peers (peer inet PRIMARY KEY, data_center text, host_id uuid, preferred_ip inet, rack text, release_version text, rpc_address inet, schema_version uuid, tokens set<text>)",
            "INSERT INTO system.local (key, bootstrapped, broadcast_address, broadcast_port, cluster_name, cql_version, data_center, host_id, listen_address, listen_port, native_protocol_version, partitioner, rack, release_version, rpc_address, rpc_port, schema_version) VALUES ('local', 'COMPLETED', '127.0.0.1', 7000, 'Embedded Cluster', '3.4.6', 'datacenter1', 6c6f6361-6c00-4000-8000-000000000001, '127.0.0.1', 7000, '4', 'org.apache.cassandra.dht.Murmur3Partitioner', 'rack1', '4.1.0', '127.0.0.1', 9042, 5c4e6d61-0000-4000-8000-000000000001)",
        ] {
            let (st, _) = engine::parse(q).expect("bootstrap statement");
            schema.execute(&st, &[], None).expect("bootstrap");
        }
        for q in engine::SYSTEM_SCHEMA_DDL {
            let (st, _) = engine::parse(q).expect("system_schema statement");
            schema.execute(&st, &[], None).expect("system_schema");
        }
        let shared = Arc::new(Shared { cfg, schema: Mutex::new(schema), prepared: Mutex::new(HashMap::new()), consistencies: Mutex::new(Vec::new()), queries: Mutex::new(Vec::new()) });
        let sh = shared.clone();
        let tcp = TcpServer::start("cql-server", tls, move |s| {
            let _ = serve(&sh, s);
        })?;
        Ok(CassandraServer { shared, tcp })
    }

    /// Sets the token ring: this node's tokens and the peers `(host, native port, tokens)`
    /// reported in `system.local` / `system.peers_v2` (for token-aware clients).
    pub fn set_ring(&self, tokens: &[i64], peers: &[(String, u16, Vec<i64>)]) {
        let set = |ts: &[i64]| format!("{{{}}}", ts.iter().map(|t| format!("'{}'", t)).collect::<Vec<_>>().join(", "));
        let _ = self.execute(&format!("UPDATE system.local SET tokens = {} WHERE key = 'local'", set(tokens)));
        let _ = self.execute("TRUNCATE system.peers_v2");
        let _ = self.execute("TRUNCATE system.peers");
        for (i, (h, p, ts)) in peers.iter().enumerate() {
            let host_id = format!("6c6f6361-6c00-4000-8000-{:012x}", i + 2);
            let common = format!("data_center, rack, release_version, schema_version, host_id, tokens) VALUES ('datacenter1', 'rack1', '4.1.0', {}, {}, {})", SCHEMA_VERSION, host_id, set(ts));
            let _ = self.execute(&format!("INSERT INTO system.peers_v2 (peer, peer_port, native_address, native_port, {}", common).replacen("VALUES (", &format!("VALUES ('{}', 7000, '{}', {}, ", h, h, p), 1));
            let _ = self.execute(&format!("INSERT INTO system.peers (peer, rpc_address, {}", common).replacen("VALUES (", &format!("VALUES ('{}', '{}', ", h, h), 1));
        }
    }

    pub fn port(&self) -> u16 {
        self.tcp.port()
    }

    /// Runs a CQL statement directly (no keyspace selected).
    pub fn execute(&self, cql: &str) -> Result<Outcome, CqlError> {
        let (st, _) = engine::parse(cql)?;
        self.shared.schema.lock().unwrap().execute(&st, &[], None)
    }

    /// Consistency levels of the QUERY/EXECUTE requests received so far.
    pub fn consistencies(&self) -> Vec<Consistency> {
        self.shared.consistencies.lock().unwrap().clone()
    }

    /// Statements received (QUERY and PREPARE).
    pub fn queries(&self) -> Vec<String> {
        self.shared.queries.lock().unwrap().clone()
    }

    pub fn connection_count(&self) -> usize {
        self.tcp.connection_count()
    }

    pub fn stop(&self) {
        self.tcp.stop();
    }
}

struct Conn {
    started: bool,
    authed: bool,
    keyspace: Option<String>,
    compression: Option<super::frame::Compression>,
}

fn error_frame(stream: i16, e: &CqlError, extra: Option<&[u8]>) -> Frame {
    let mut w = Writer::new();
    w.int(e.0);
    w.string(&e.1);
    match e.0 {
        error_code::ALREADY_EXISTS => {
            w.string("");
            w.string("");
        }
        error_code::UNPREPARED => {
            w.short_bytes(extra.unwrap_or_default());
        }
        _ => {}
    }
    Frame::response(stream, opcode::ERROR, w.0)
}

fn serve(sh: &Shared, stream: NetStream) -> io::Result<()> {
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let mut conn = Conn { started: false, authed: sh.cfg.users.is_empty(), keyspace: None, compression: None };
    loop {
        let f = read_frame(&mut reader)?;
        let sid = f.stream;
        let f = match f.decompressed(conn.compression) {
            Ok(f) => f,
            Err(e) => {
                let mut fr = error_frame(sid, &CqlError(error_code::PROTOCOL_ERROR, e.message), None);
                fr = fr.compressed(conn.compression);
                writer.write_all(&fr.encode())?;
                writer.flush()?;
                continue;
            }
        };
        let startup = f.opcode == opcode::STARTUP;
        let resp = if f.version & 0x7f != VERSION {
            let e = CqlError(error_code::PROTOCOL_ERROR, format!("Invalid or unsupported protocol version ({}); supported versions are (4/v4)", f.version & 0x7f));
            let mut fr = error_frame(f.stream, &e, None);
            fr.version = (f.version & 0x7f) | 0x80;
            fr
        } else {
            match handle(sh, &mut conn, &f) {
                Ok(fr) => fr,
                Err((e, extra)) => error_frame(f.stream, &e, extra.as_deref()),
            }
        };
        // the READY / AUTHENTICATE answer to STARTUP is not compressed yet
        let resp = if startup { resp } else { resp.compressed(conn.compression) };
        writer.write_all(&resp.encode())?;
        writer.flush()?;
    }
}

type HResult = Result<Frame, (CqlError, Option<Vec<u8>>)>;

fn perr(m: &str) -> (CqlError, Option<Vec<u8>>) {
    (CqlError(error_code::PROTOCOL_ERROR, m.to_string()), None)
}

fn handle(sh: &Shared, conn: &mut Conn, f: &Frame) -> HResult {
    let mut r = Reader::new(&f.body);
    let pe = |e: camel_core::error::CamelError| perr(&e.message);
    match f.opcode {
        opcode::OPTIONS => {
            let mut w = Writer::new();
            w.short(3);
            w.string("CQL_VERSION");
            w.string_list(&["3.4.6"]);
            w.string("COMPRESSION");
            w.string_list(&["lz4", "snappy"]);
            w.string("PROTOCOL_VERSIONS");
            w.string_list(&["4/v4"]);
            Ok(Frame::response(f.stream, opcode::SUPPORTED, w.0))
        }
        opcode::STARTUP => {
            let opts = r.string_map().map_err(pe)?;
            if let Some((_, c)) = opts.iter().find(|(k, _)| k.eq_ignore_ascii_case("COMPRESSION")) {
                conn.compression = Some(super::frame::Compression::parse(c).ok_or_else(|| perr(&format!("Unknown compression algorithm: {}", c)))?);
            }
            conn.started = true;
            if sh.cfg.users.is_empty() {
                Ok(Frame::response(f.stream, opcode::READY, Vec::new()))
            } else {
                let mut w = Writer::new();
                w.string("org.apache.cassandra.auth.PasswordAuthenticator");
                Ok(Frame::response(f.stream, opcode::AUTHENTICATE, w.0))
            }
        }
        opcode::AUTH_RESPONSE => {
            let token = r.bytes().map_err(pe)?.unwrap_or_default();
            let ok = camel_tls::sasl::parse_plain(&token).is_some_and(|(_, u, p)| sh.cfg.users.get(&u) == Some(&p));
            if !ok {
                let user = camel_tls::sasl::parse_plain(&token).map(|(_, u, _)| u).unwrap_or_default();
                return Err((CqlError(error_code::BAD_CREDENTIALS, format!("Provided username {} and/or password are incorrect", user)), None));
            }
            conn.authed = true;
            let mut w = Writer::new();
            w.bytes(None);
            Ok(Frame::response(f.stream, opcode::AUTH_SUCCESS, w.0))
        }
        _ if !conn.started => Err(perr("Unexpected message, expecting STARTUP or OPTIONS")),
        _ if !conn.authed => Err((CqlError(error_code::UNAUTHORIZED, "You have not logged in".into()), None)),
        opcode::REGISTER => Ok(Frame::response(f.stream, opcode::READY, Vec::new())),
        opcode::QUERY => {
            let q = r.long_string().map_err(pe)?;
            let params = QueryParams::read(&mut r).map_err(pe)?;
            sh.queries.lock().unwrap().push(q.clone());
            run(sh, conn, f.stream, &q, &params)
        }
        opcode::PREPARE => {
            let q = r.long_string().map_err(pe)?;
            sh.queries.lock().unwrap().push(q.clone());
            let (st, n) = engine::parse(&q).map_err(|e| (e, None))?;
            let (binds, result) = sh.schema.lock().unwrap().prepare(&st, n, conn.keyspace.as_deref()).map_err(|e| (e, None))?;
            let id = HashAlg::Sha256.hash(format!("{}\0{}", conn.keyspace.as_deref().unwrap_or(""), q).as_bytes())[..16].to_vec();
            sh.prepared.lock().unwrap().insert(id.clone(), (q, conn.keyspace.clone()));
            let mut w = Writer::new();
            w.int(result_kind::PREPARED);
            w.short_bytes(&id);
            w.int(0); // flags: no global table spec
            w.int(binds.len() as i32);
            // partition key bind indexes (for token-aware routing)
            let pk: Vec<u16> = match binds.first() {
                Some(b0) => {
                    let cols = sh.schema.lock().unwrap().partition_key(&b0.keyspace, &b0.table);
                    let idx: Vec<Option<u16>> = cols.iter().map(|c| binds.iter().position(|b| &b.name == c && b.table == b0.table).map(|i| i as u16)).collect();
                    if !idx.is_empty() && idx.iter().all(|i| i.is_some()) { idx.into_iter().flatten().collect() } else { Vec::new() }
                }
                None => Vec::new(),
            };
            w.int(pk.len() as i32);
            for i in &pk {
                w.short(*i);
            }
            for b in &binds {
                w.string(&b.keyspace);
                w.string(&b.table);
                w.string(&b.name);
                w.cql_type(&b.cql_type);
            }
            write_rows_metadata(&mut w, &result, None, result.is_empty());
            Ok(Frame::response(f.stream, opcode::RESULT, w.0))
        }
        opcode::EXECUTE => {
            let id = r.short_bytes().map_err(pe)?;
            let params = QueryParams::read(&mut r).map_err(pe)?;
            let found = sh.prepared.lock().unwrap().get(&id).cloned();
            let Some((q, ks)) = found else {
                return Err((CqlError(error_code::UNPREPARED, format!("Prepared query with ID {} not found (either the query was not prepared on this host (maybe the host has been restarted?) or you have prepared too many queries and it has been evicted from the internal cache)", crate::util::hex(&id))), Some(id)));
            };
            // run in the keyspace the statement was prepared in
            let original = conn.keyspace.clone();
            if ks.is_some() {
                conn.keyspace = ks;
            }
            let r = run(sh, conn, f.stream, &q, &params);
            if !matches!(&r, Ok(fr) if is_set_keyspace(fr)) {
                conn.keyspace = original;
            }
            r
        }
        opcode::BATCH => {
            let _kind = r.byte().map_err(pe)?;
            let n = r.short().map_err(pe)?;
            let mut stmts = Vec::new();
            for _ in 0..n {
                let k = r.byte().map_err(pe)?;
                let q = if k == 0 {
                    r.long_string().map_err(pe)?
                } else {
                    let id = r.short_bytes().map_err(pe)?;
                    match sh.prepared.lock().unwrap().get(&id) {
                        Some((q, _)) => q.clone(),
                        None => return Err((CqlError(error_code::UNPREPARED, "Prepared query not found".into()), Some(id))),
                    }
                };
                let nv = r.short().map_err(pe)?;
                let vals = (0..nv).map(|_| r.bytes()).collect::<camel_core::error::Result<Vec<_>>>().map_err(pe)?;
                stmts.push((q, vals));
            }
            let c = r.short().map_err(pe)?;
            if let Some(c) = Consistency::from_code(c) {
                sh.consistencies.lock().unwrap().push(c);
            }
            for (q, vals) in stmts {
                let mut p = QueryParams::new(Consistency::One);
                p.values = vals;
                run(sh, conn, f.stream, &q, &p)?;
            }
            let mut w = Writer::new();
            w.int(result_kind::VOID);
            Ok(Frame::response(f.stream, opcode::RESULT, w.0))
        }
        other => Err(perr(&format!("Unknown opcode {}", other))),
    }
}

fn is_set_keyspace(f: &Frame) -> bool {
    f.body.len() >= 4 && i32::from_be_bytes(f.body[..4].try_into().unwrap()) == result_kind::SET_KEYSPACE
}

fn run(sh: &Shared, conn: &mut Conn, stream: i16, q: &str, params: &QueryParams) -> HResult {
    sh.consistencies.lock().unwrap().push(params.consistency);
    let (st, n) = engine::parse(q).map_err(|e| (e, None))?;
    let mut schema = sh.schema.lock().unwrap();
    if q.to_ascii_lowercase().contains("system_schema") {
        schema.refresh_system_schema();
    }
    let ks = conn.keyspace.clone();
    let mut binds = Vec::new();
    if n > 0 || !params.values.is_empty() {
        if params.values.len() != n {
            return Err((CqlError(error_code::INVALID, format!("There were {} markers(?) in CQL but {} bound variables", n, params.values.len())), None));
        }
        let (specs, _) = schema.prepare(&st, n, ks.as_deref()).map_err(|e| (e, None))?;
        for (spec, v) in specs.iter().zip(&params.values) {
            binds.push(decode_value(&spec.cql_type, v.as_deref()).map_err(|e| (CqlError(error_code::INVALID, e.message), None))?);
        }
    }
    let outcome = schema.execute(&st, &binds, ks.as_deref()).map_err(|e| (e, None))?;
    drop(schema);
    let mut w = Writer::new();
    match outcome {
        Outcome::Void => {
            w.int(result_kind::VOID);
        }
        Outcome::SetKeyspace(k) => {
            w.int(result_kind::SET_KEYSPACE);
            w.string(&k);
            conn.keyspace = Some(k);
        }
        Outcome::SchemaChange { change, target, keyspace, name } => {
            w.int(result_kind::SCHEMA_CHANGE);
            w.string(change);
            w.string(target);
            w.string(&keyspace);
            if target != "KEYSPACE" {
                w.string(&name);
            }
        }
        Outcome::Rows { cols, rows } => encode_rows(&mut w, &cols, rows, params).map_err(|e| (e, None))?,
    }
    Ok(Frame::response(stream, opcode::RESULT, w.0))
}

fn encode_rows(w: &mut Writer, cols: &[ColumnSpec], rows: Vec<Vec<Value>>, params: &QueryParams) -> Result<(), CqlError> {
    let offset = params.paging_state.as_ref().filter(|p| p.len() == 4).map(|p| u32::from_be_bytes(p[..4].try_into().unwrap()) as usize).unwrap_or(0);
    let page = params.page_size.filter(|p| *p > 0).map(|p| p as usize).unwrap_or(usize::MAX);
    let end = offset.saturating_add(page).min(rows.len());
    let more = end < rows.len();
    let paging = if more { Some((end as u32).to_be_bytes().to_vec()) } else { None };
    w.int(result_kind::ROWS);
    write_rows_metadata(w, cols, paging.as_deref(), params.skip_metadata);
    let slice = if offset < rows.len() { &rows[offset..end] } else { &[] };
    w.int(slice.len() as i32);
    for row in slice {
        for (c, v) in cols.iter().zip(row) {
            let b = encode_value(&c.cql_type, v).map_err(|e| CqlError(error_code::SERVER_ERROR, e.message))?;
            w.bytes(b.as_deref());
        }
    }
    Ok(())
}

//! CQL frame compression, durations, UDTs / secondary indexes and token-aware routing.

use cameleer_core::prelude::*;
use cameleer_datastores::cassandra::frame::{format_duration, murmur3_token, parse_duration_value, read_vint, write_vint};
use cameleer_datastores::cassandra::{CqlConfig, CqlSession, ExecOptions};
use cameleer_datastores::testing::CassandraServer;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_datastores::register(&ctx);
    ctx
}

#[test]
fn murmur3_tokens_and_vints() {
    // SELECT token(id) FROM t WHERE id = 1 (int key) on a real cluster
    assert_eq!(murmur3_token(&1i32.to_be_bytes()), -4069959284402364209);
    for v in [0i64, 1, -1, 63, -64, 64, 1 << 20, -(1 << 40), i64::MAX, i64::MIN] {
        let mut b = Vec::new();
        write_vint(&mut b, v);
        let mut p = 0;
        assert_eq!(read_vint(&b, &mut p), Some(v), "{}", v);
        assert_eq!(p, b.len());
    }
    assert_eq!(format_duration(14, 3, 3_723_000_000_123), "1y2mo3d1h2m3s123ns");
    assert_eq!(parse_duration_value(&Value::from("1y2mo3d1h2m3s123ns")), Some((14, 3, 3_723_000_000_123)));
    assert_eq!(parse_duration_value(&Value::from("P1Y2M3DT1H2M3S")), Some((14, 3, 3_723_000_000_000)));
    assert_eq!(parse_duration_value(&Value::from("-2w")), Some((0, -14, 0)));
    assert_eq!(parse_duration_value(&Value::from("3x")), None);
}

#[test]
fn compressed_sessions() {
    let server = CassandraServer::start().unwrap();
    server.execute("CREATE KEYSPACE ks WITH replication = {'class': 'SimpleStrategy', 'replication_factor': 1}").unwrap();
    server.execute("CREATE TABLE ks.docs (id int PRIMARY KEY, body text, d duration)").unwrap();
    let ctx = context();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for comp in ["lz4", "snappy"] {
        let base = format!("cql://127.0.0.1:{}/ks", server.port());
        let text = format!("{} ", comp).repeat(2000);
        let insert = format!("{}?cql=INSERT INTO docs (id, body, d) VALUES (?, ?, ?)&compression={}", base, comp);
        let id = if comp == "lz4" { 1 } else { 2 };
        t.send_body(&insert, Value::Array(vec![Value::Int(id), Value::from(text.clone()), Value::from("1mo2d3h")])).unwrap();
        let rows = t.request_body(&format!("{}?cql=SELECT body, d FROM docs WHERE id = ?&compression={}", base, comp), Value::Int(id)).unwrap();
        assert_eq!(rows.as_array().unwrap()[0].get("body"), Some(&Value::from(text)));
        assert_eq!(rows.as_array().unwrap()[0].get("d"), Some(&Value::from("1mo2d3h")));
    }
    assert!(ctx.get_endpoint(&format!("cql://127.0.0.1:{}/ks?cql=SELECT * FROM docs&compression=zstd", server.port())).is_err());
}

#[test]
fn udts_and_secondary_indexes() {
    let server = CassandraServer::start().unwrap();
    let session = CqlSession::new(CqlConfig::new("127.0.0.1", server.port()));
    let o = ExecOptions::default();
    let run = |q: &str, v: &[Value]| session.execute(q, v, &o);
    let plain = ExecOptions { prepare: false, ..ExecOptions::default() };
    session.execute("CREATE KEYSPACE ks WITH replication = {'class': 'SimpleStrategy', 'replication_factor': 1}", &[], &plain).unwrap();
    session.execute("CREATE TYPE ks.address (street text, zip int)", &[], &plain).unwrap();
    assert!(session.execute("CREATE TYPE ks.address (x int)", &[], &plain).is_err());
    session.execute("CREATE TYPE IF NOT EXISTS ks.address (x int)", &[], &plain).unwrap();
    session.execute("CREATE TABLE ks.people (id int PRIMARY KEY, name text, city text, home frozen<address>, others list<frozen<address>>)", &[], &plain).unwrap();
    assert!(session.execute("CREATE TABLE ks.bad (id int PRIMARY KEY, a frozen<nosuchtype>)", &[], &plain).is_err());
    session.execute("INSERT INTO ks.people (id, name, city, home) VALUES (1, 'ada', 'paris', {street: 'Rue 1', zip: 75001})", &[], &plain).unwrap();
    run("INSERT INTO ks.people (id, name, city, home, others) VALUES (?, ?, ?, ?, ?)", &[Value::Int(2), "bob".into(), "rome".into(), Value::object([("street", Value::from("Via 2")), ("zip", Value::Int(100))]), Value::Array(vec![Value::object([("street", Value::from("Alt")), ("zip", Value::Int(1))])])]).unwrap();
    let rows = run("SELECT home, others FROM ks.people WHERE id = ?", &[Value::Int(2)]).unwrap().to_objects();
    assert_eq!(rows[0].get("home"), Some(&Value::object([("street", Value::from("Via 2")), ("zip", Value::Int(100))])));
    assert_eq!(rows[0].get("others").and_then(|o| o.as_array()).map(|a| a.len()), Some(1));
    let rows = run("SELECT home FROM ks.people WHERE id = ?", &[Value::Int(1)]).unwrap().to_objects();
    assert_eq!(rows[0].get("home").and_then(|h| h.get("zip")), Some(&Value::Int(75001)));
    assert!(session.execute("INSERT INTO ks.people (id, home) VALUES (3, {nope: 1})", &[], &plain).is_err());
    // filtering on a regular column needs ALLOW FILTERING until it is indexed
    assert!(run("SELECT name FROM ks.people WHERE city = ?", &["rome".into()]).is_err());
    session.execute("CREATE INDEX people_city ON ks.people (city)", &[], &plain).unwrap();
    assert!(session.execute("CREATE INDEX ON ks.people (city)", &[], &plain).is_err());
    session.execute("CREATE INDEX IF NOT EXISTS ON ks.people (city)", &[], &plain).unwrap();
    let rows = run("SELECT name FROM ks.people WHERE city = ?", &["rome".into()]).unwrap().to_objects();
    assert_eq!(rows, vec![Value::object([("name", Value::from("bob"))])]);
    assert!(session.execute("CREATE INDEX ON ks.people (id)", &[], &plain).is_err());
}

#[test]
fn token_aware_routing() {
    let a = CassandraServer::start().unwrap();
    let b = CassandraServer::start().unwrap();
    for s in [&a, &b] {
        s.execute("CREATE KEYSPACE ks WITH replication = {'class': 'SimpleStrategy', 'replication_factor': 1}").unwrap();
        s.execute("CREATE TABLE ks.kv (k int PRIMARY KEY, v text)").unwrap();
    }
    // a owns (MIN, 0], b owns (0, MAX] — tokens 0 and i64::MAX
    a.set_ring(&[0], &[("127.0.0.1".to_string(), b.port(), vec![i64::MAX])]);
    b.set_ring(&[i64::MAX], &[("127.0.0.1".to_string(), a.port(), vec![0])]);
    let session = CqlSession::new(CqlConfig::new("127.0.0.1", a.port()));
    let ring = session.token_ring().unwrap();
    assert_eq!(ring.len(), 2);
    let o = ExecOptions::default();
    let mut expect_a = 0;
    for k in 0..40 {
        session.execute("INSERT INTO ks.kv (k, v) VALUES (?, ?)", &[Value::Int(k), Value::from(format!("v{}", k))], &o).unwrap();
        if murmur3_token(&(k as i32).to_be_bytes()) <= 0 {
            expect_a += 1;
        }
    }
    let count = |s: &CassandraServer| match s.execute("SELECT COUNT(*) FROM ks.kv").unwrap() {
        cameleer_datastores::cassandra::engine::Outcome::Rows { rows, .. } => rows[0][0].as_i64().unwrap(),
        _ => -1,
    };
    assert_eq!(count(&a), expect_a);
    assert_eq!(count(&b), 40 - expect_a);
    assert!(expect_a > 0 && expect_a < 40);
    // reads are routed as well
    let r = session.execute("SELECT v FROM ks.kv WHERE k = ?", &[Value::Int(7)], &o).unwrap().to_objects();
    assert_eq!(r[0].get("v"), Some(&Value::from("v7")));
    // without token awareness everything goes to the contact point
    let mut cfg = CqlConfig::new("127.0.0.1", a.port());
    cfg.token_aware = false;
    let plain = CqlSession::new(cfg);
    plain.execute("INSERT INTO ks.kv (k, v) VALUES (?, ?)", &[Value::Int(1000), Value::from("x")], &o).unwrap();
    assert_eq!(count(&a) + count(&b), 41);
}

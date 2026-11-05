//! Live interop: Cameleer `sql`, `jdbc`, `sql-stored`, JDBC repositories and transactions against
//! a real PostgreSQL 16 server (`scripts/live-interop.sh`). Runs only with `CAMEL_LIVE_INTEROP=1`.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use cameleer_core::prelude::*;
use cameleer_sql::pg::{PgConfig, PgConnection};
use cameleer_sql::{transaction_policy, Connection, DataSource, JdbcAggregationRepository, JdbcIdempotentRepository, PgDataSource};
use common::*;

const PORT: u16 = 15432;
const PW: &str = "camelpw";

fn live() -> bool {
    let on = std::env::var("CAMEL_LIVE_INTEROP").map(|v| v == "1").unwrap_or(false);
    if !on {
        eprintln!("skipped: set CAMEL_LIVE_INTEROP=1 and run scripts/live-interop.sh start");
    }
    on
}

fn ca() -> String {
    format!("{}/certs/ca.pem", std::env::var("CAMEL_INTEROP_DIR").unwrap_or_else(|_| "/var/lib/camel-interop".into()))
}

fn url(db: &str, extra: &str) -> String {
    format!("jdbc:postgresql://127.0.0.1:{}/{}?user=camel&password={}{}", PORT, db, PW, extra)
}

/// A fresh database owned by `camel`, dropped when the guard goes away.
struct Db(String);

impl Db {
    fn new(prefix: &str) -> Db {
        let name = unique(&format!("live_{}", prefix));
        let mut admin = PgConnection::connect(&PgConfig::from_url(&format!("jdbc:postgresql://127.0.0.1:{}/postgres?user=postgres&password={}", PORT, PW)).unwrap()).unwrap_or_else(|e| panic!("connect as postgres: {}", e));
        admin.execute(&format!("CREATE DATABASE {} OWNER camel", name), &[]).unwrap();
        Db(name)
    }
    fn ds(&self) -> Arc<dyn DataSource> {
        Arc::new(PgDataSource::from_url(&url(&self.0, "")).unwrap())
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        if let Ok(mut admin) = PgConnection::connect(&PgConfig::from_url(&format!("jdbc:postgresql://127.0.0.1:{}/postgres?user=postgres&password={}", PORT, PW)).unwrap()) {
            let _ = admin.execute(&format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", self.0), &[]);
        }
    }
}

#[test]
fn live_postgres_sql_and_jdbc_producers() {
    if !live() {
        return;
    }
    let db = Db::new("producer");
    let ds = db.ds();
    let ctx = context_with(ds.clone());
    producer_suite(&ctx, &ds);
}

#[test]
fn live_postgres_sql_stored_with_plpgsql() {
    if !live() {
        return;
    }
    let db = Db::new("stored");
    let ds = db.ds();
    let ctx = context_with(ds.clone());
    let dir = std::env::temp_dir().join(unique("livestored"));
    std::fs::create_dir_all(&dir).unwrap();
    stored_suite(&ctx, &ds, &dir, true);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn live_postgres_scram_auth_server_version_and_types() {
    if !live() {
        return;
    }
    let db = Db::new("types");
    let mut c = PgConnection::connect(&PgConfig::from_url(&url(&db.0, "")).unwrap()).unwrap();
    let version = c.server_parameters().get("server_version").cloned().unwrap_or_default();
    assert!(version.starts_with("16"), "{}", version);
    assert_eq!(c.server_parameters().get("server_encoding").map(|s| s.as_str()), Some("UTF8"));
    // wrong password: SCRAM failure with SQLSTATE 28P01
    let e = PgConnection::connect(&PgConfig::from_url(&format!("jdbc:postgresql://127.0.0.1:{}/{}?user=camel&password=wrong", PORT, db.0)).unwrap()).err().unwrap();
    assert_eq!(e.kind, "SQLNonTransientConnectionException", "{}", e);
    assert!(e.message.contains("28P01") && e.message.contains("password authentication failed"), "{}", e.message);
    let e = PgConnection::connect(&PgConfig::from_url(&url("no_such_db", "")).unwrap()).err().unwrap();
    assert!(e.message.contains("does not exist"), "{}", e.message);

    c.execute(
        "CREATE TABLE t (id BIGSERIAL PRIMARY KEY, small SMALLINT, i INTEGER, name VARCHAR(20), ch CHAR(3), price NUMERIC(8,2), r REAL, ok BOOLEAN, at TIMESTAMP, d DATE, data BYTEA, doc JSONB, u UUID)",
        &[],
    )
    .unwrap();
    let r = c
        .execute(
            "INSERT INTO t (small, i, name, ch, price, r, ok, at, d, data, doc, u) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
            &[
                Value::Int(7),
                Value::Int(42),
                Value::from("zoë ✓"),
                Value::from("ab"),
                Value::Float(12.5),
                Value::Float(0.25),
                Value::Bool(true),
                Value::from("2024-01-02 03:04:05"),
                Value::from("2024-02-29"),
                Value::Bytes(vec![0, 1, 2, 255]),
                Value::from(r#"{"a": [1, 2]}"#),
                Value::from("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"),
            ],
        )
        .unwrap_or_else(|e| panic!("{}", e));
    assert_eq!(r.rows, vec![vec![Value::Int(1)]]);
    let r = c.execute("SELECT small, i, name, ch, price, r, ok, at, d, data, doc, u FROM t WHERE i = ?", &[Value::Int(42)]).unwrap();
    assert_eq!(
        r.rows[0],
        vec![
            Value::Int(7),
            Value::Int(42),
            Value::from("zoë ✓"),
            Value::from("ab "),
            Value::Float(12.5),
            Value::Float(0.25),
            Value::Bool(true),
            Value::from("2024-01-02 03:04:05"),
            Value::from("2024-02-29"),
            Value::Bytes(vec![0, 1, 2, 255]),
            Value::from(r#"{"a": [1, 2]}"#),
            Value::from("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"),
        ]
    );
    // NULL parameters, text parameters compared with typed columns, LIKE, multi-statement simple query
    let r = c.execute("INSERT INTO t (name, i) VALUES (?, ?)", &[Value::Null, Value::from("9")]).unwrap();
    assert_eq!(r.update_count, 1);
    assert_eq!(c.execute("SELECT COUNT(*) FROM t WHERE name IS NULL AND i = ?", &[Value::from("9")]).unwrap().scalar(), Some(&Value::Int(1)));
    assert_eq!(c.execute("SELECT COUNT(*) FROM t WHERE name LIKE ?", &[Value::from("zo%")]).unwrap().scalar(), Some(&Value::Int(1)));
    let r = c.execute("UPDATE t SET ok = false WHERE i = 9; UPDATE t SET ok = true WHERE i = 9", &[]).unwrap();
    assert_eq!(r.update_count, 1);
    // errors keep their SQLSTATE and the connection stays usable
    let e = c.execute("INSERT INTO t (id) VALUES (1)", &[]).unwrap_err();
    assert_eq!(e.kind, "SQLIntegrityConstraintViolationException", "{}", e);
    let e = c.execute("SELECT 1/0", &[]).unwrap_err();
    assert!(e.message.contains("22012"), "{}", e.message);
    assert_eq!(c.execute("SELECT 1", &[]).unwrap().scalar(), Some(&Value::Int(1)));
    // a large result set across many DataRow messages
    let r = c.execute("SELECT g, md5(g::text) FROM generate_series(1, 5000) g", &[]).unwrap();
    assert_eq!(r.rows.len(), 5000);
    assert_eq!(r.rows[4999][0], Value::Int(5000));
    // notices (RAISE NOTICE) do not disturb the protocol
    c.execute("DO $$ BEGIN RAISE NOTICE 'hello %', 1; END $$", &[]).unwrap();
    assert_eq!(c.execute("SELECT 'still' AS s", &[]).unwrap().scalar(), Some(&Value::from("still")));
}

#[test]
fn live_postgres_tls_sslmode_verify_full() {
    if !live() {
        return;
    }
    let db = Db::new("tls");
    for (host, extra) in [
        ("localhost", "&sslmode=require".to_string()),
        ("localhost", format!("&sslmode=verify-ca&sslrootcert={}", ca())),
        ("localhost", format!("&sslmode=verify-full&sslrootcert={}", ca())),
        ("127.0.0.1", format!("&sslmode=verify-full&sslrootcert={}", ca())),
    ] {
        let u = format!("jdbc:postgresql://{}:{}/{}?user=camel&password={}{}", host, PORT, db.0, PW, extra);
        let mut c = PgConnection::connect(&PgConfig::from_url(&u).unwrap()).unwrap_or_else(|e| panic!("{}: {}", extra, e));
        let r = c.execute("SELECT ssl, version FROM pg_stat_ssl WHERE pid = pg_backend_pid()", &[]).unwrap();
        assert_eq!(r.rows[0][0], Value::Bool(true), "{}", extra);
        let v = r.rows[0][1].to_string();
        assert!(v == "TLSv1.3" || v == "TLSv1.2", "{}", v);
        // a datasource with TLS runs routes too
        let ds: Arc<dyn DataSource> = Arc::new(PgDataSource::from_url(&u).unwrap());
        let ctx = context_with(ds.clone());
        ctx.add_routes(|b| {
            b.from("direct:q").to("sql:select current_user as u, :#x::int + 1 as n?dataSource=#ds&outputType=SelectOne");
        })
        .unwrap();
        ctx.start().unwrap();
        let r = ctx.create_producer_template().request_body_and_header("direct:q", Value::Null, "x", 41).unwrap();
        assert_eq!(r, obj(&[("u", Value::from("camel")), ("n", Value::Int(42))]));
        ctx.stop().unwrap();
    }
    // verify-full against a CA that did not sign the server certificate fails
    let (other_ca, _) = cameleer_tls::testing::write_localhost_pem_files("live-pg-wrong-ca");
    let u = format!("jdbc:postgresql://localhost:{}/{}?user=camel&password={}&sslmode=verify-full&sslrootcert={}", PORT, db.0, PW, other_ca.display());
    assert!(PgConnection::connect(&PgConfig::from_url(&u).unwrap()).is_err());
}

#[test]
fn live_postgres_transactions_and_rollback() {
    if !live() {
        return;
    }
    let db = Db::new("tx");
    let cfg = PgConfig::from_url(&url(&db.0, "")).unwrap();
    let mut a = PgConnection::connect(&cfg).unwrap();
    let mut b = PgConnection::connect(&cfg).unwrap();
    a.execute("CREATE TABLE acc (id INT PRIMARY KEY, bal INT)", &[]).unwrap();
    a.execute("INSERT INTO acc VALUES (1, 100), (2, 0)", &[]).unwrap();
    a.begin().unwrap();
    assert_eq!(a.transaction_status(), b'T');
    a.execute("UPDATE acc SET bal = bal - ? WHERE id = 1", &[Value::Int(30)]).unwrap();
    // isolation: the other session does not see the uncommitted update
    assert_eq!(b.execute("SELECT bal FROM acc WHERE id = 1", &[]).unwrap().scalar(), Some(&Value::Int(100)));
    a.rollback().unwrap();
    assert_eq!(b.execute("SELECT bal FROM acc WHERE id = 1", &[]).unwrap().scalar(), Some(&Value::Int(100)));
    a.begin().unwrap();
    a.execute("UPDATE acc SET bal = bal - 30 WHERE id = 1", &[]).unwrap();
    a.commit().unwrap();
    assert_eq!(b.execute("SELECT bal FROM acc WHERE id = 1", &[]).unwrap().scalar(), Some(&Value::Int(70)));
    a.begin().unwrap();
    assert!(a.execute("INSERT INTO acc VALUES (1, 1)", &[]).is_err());
    assert_eq!(a.transaction_status(), b'E');
    let e = a.execute("SELECT 1", &[]).unwrap_err();
    assert!(e.message.contains("current transaction is aborted"), "{}", e.message);
    assert!(a.commit().is_err());
    assert_eq!(a.transaction_status(), b'I');
    // a dropped connection with an open transaction is rolled back by the server
    {
        let mut c = PgConnection::connect(&cfg).unwrap();
        c.begin().unwrap();
        c.execute("DELETE FROM acc", &[]).unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let n = b.execute("SELECT COUNT(*) FROM acc", &[]).unwrap().scalar().and_then(|v| v.as_i64()).unwrap();
        if n == 2 || Instant::now() > deadline {
            assert_eq!(n, 2);
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    // route-level transactions with the transaction policy
    let ds = db.ds();
    exec(&ds, "CREATE TABLE ledger (id SERIAL PRIMARY KEY, kind VARCHAR(10) NOT NULL)");
    let ctx = context_with(ds.clone());
    let policy = transaction_policy(ds.clone());
    ctx.add_routes(|b| {
        b.from("direct:tx")
            .policy(policy.clone())
            .to("sql:insert into ledger (kind) values (:#kind)?dataSource=#ds")
            .to("sql:insert into ledger (kind) values ('second')?dataSource=#ds")
            .filter(simple("${header.kind} == 'bad'"))
            .throw_exception("IllegalArgumentException", "bad kind")
            .end()
            .end();
        b.from("direct:rollback").policy(policy.clone()).to("sql:insert into ledger (kind) values ('rb')?dataSource=#ds").rollback("forced").end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.request("direct:tx", |ex| ex.set_header("kind", "good")).unwrap();
    assert_eq!(count(&ds, "SELECT COUNT(*) FROM ledger"), 2);
    assert!(t.request("direct:tx", |ex| ex.set_header("kind", "bad")).is_err());
    assert_eq!(count(&ds, "SELECT COUNT(*) FROM ledger"), 2, "both inserts rolled back");
    assert!(t.request("direct:rollback", |_| {}).is_err());
    assert_eq!(count(&ds, "SELECT COUNT(*) FROM ledger WHERE kind = 'rb'"), 0);
    ctx.stop().unwrap();
}

#[test]
fn live_postgres_consumer_and_jdbc_repositories() {
    if !live() {
        return;
    }
    let db = Db::new("repo");
    let ds = db.ds();
    exec(&ds, "CREATE TABLE events (id SERIAL PRIMARY KEY, kind VARCHAR(10), handled BOOLEAN DEFAULT false)");
    exec(&ds, "INSERT INTO events (kind) VALUES ('a'), ('b'), ('a')");
    let ctx = context_with(ds.clone());
    let repo = Arc::new(JdbcIdempotentRepository::new(ds.clone(), "events").unwrap());
    let agg = Arc::new(JdbcAggregationRepository::new(&ctx, ds.clone(), "aggregation").unwrap());
    let agg2 = agg.clone();
    ctx.add_routes(move |b| {
        b.from("sql:select * from events where handled = false order by id?dataSource=#ds&initialDelay=0&delay=20&onConsume=update events set handled = true where id = :#id")
            .idempotent_consumer(simple("${body[kind]}"), repo.clone())
            .to("mock:events")
            .end();
        b.from("direct:agg")
            .aggregate(header("group"), |old: Option<Exchange>, new: Exchange| -> Result<Exchange> {
                Ok(match old {
                    None => new,
                    Some(mut o) => {
                        let body = format!("{}+{}", o.body(), new.body());
                        o.set_body(body);
                        o
                    }
                })
            })
            .completion_size(3)
            .aggregation_repository(agg2.clone())
            .to("mock:aggregated")
            .end();
    })
    .unwrap();
    let mock = ctx.mock_endpoint("mock:events").unwrap();
    mock.expected_message_count(2);
    ctx.start().unwrap();
    mock.assert_is_satisfied().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while count(&ds, "SELECT COUNT(*) FROM events WHERE handled = false") > 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(count(&ds, "SELECT COUNT(*) FROM events WHERE handled = false"), 0);
    assert_eq!(count(&ds, "SELECT COUNT(*) FROM CAMEL_MESSAGEPROCESSED"), 2);
    use cameleer_core::processor::errors::IdempotentRepository;
    let again = JdbcIdempotentRepository::new(ds.clone(), "events").unwrap();
    assert!(again.contains("a") && again.contains("b"));
    assert!(again.add("c"));
    assert!(!again.add("c"));
    assert!(again.remove("c"));

    let aggregated = ctx.mock_endpoint("mock:aggregated").unwrap();
    aggregated.expected_message_count(2);
    let t = ctx.create_producer_template();
    for (g, b) in [("A", "a1"), ("B", "b1"), ("A", "a2"), ("B", "b2"), ("A", "a3"), ("C", "c1"), ("B", "b3")] {
        t.send_body_and_header("direct:agg", b, "group", g).unwrap();
    }
    aggregated.assert_is_satisfied().unwrap();
    let bodies: Vec<String> = aggregated.received_bodies().iter().map(|v| v.to_string()).collect();
    assert_eq!(bodies, vec!["a1+a2+a3", "b1+b2+b3"]);
    use cameleer_core::processor::routing::AggregationRepository;
    assert_eq!(agg.keys(), vec!["C".to_string()]);
    assert_eq!(count(&ds, "SELECT COUNT(*) FROM aggregation"), 1);
    assert_eq!(agg.get("C").unwrap().header("group"), Some(&Value::from("C")));
    ctx.stop().unwrap();
    let ex = agg.remove("C").unwrap();
    assert_eq!(agg.scan().unwrap(), vec![ex.exchange_id.to_string()]);
    assert_eq!(agg.recover(&ex.exchange_id).unwrap().unwrap().body(), &Value::from("c1"));
    agg.confirm(&ex.exchange_id);
    assert!(agg.scan().unwrap().is_empty());
}

#[test]
fn live_postgres_pool_reuses_connections() {
    if !live() {
        return;
    }
    let db = Db::new("pool");
    let ds = PgDataSource::from_url(&url(&db.0, "")).unwrap();
    {
        let mut c = ds.get_connection().unwrap();
        c.execute("CREATE TABLE p (v INT)", &[]).unwrap();
        c.begin().unwrap();
        c.execute("INSERT INTO p VALUES (1)", &[]).unwrap();
    }
    let mut c = ds.get_connection().unwrap();
    assert!(!c.in_transaction(), "a returned connection was rolled back");
    let pid = c.execute("SELECT pg_backend_pid()", &[]).unwrap().scalar().cloned();
    assert_eq!(c.execute("SELECT COUNT(*) FROM p", &[]).unwrap().scalar(), Some(&Value::Int(0)));
    drop(c);
    let mut c = ds.get_connection().unwrap();
    assert_eq!(c.execute("SELECT pg_backend_pid()", &[]).unwrap().scalar().cloned(), pid, "the pooled backend was reused");
    drop(c);
    ds.close();
}

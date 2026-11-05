//! End-to-end tests of the PostgreSQL wire protocol client against the embedded
//! PostgreSQL-protocol server (backed by rustsql).

mod common;

use std::sync::Arc;
use std::time::Duration;

use cameleer_core::prelude::*;
use cameleer_sql::pg::{PgConfig, PgConnection};
use cameleer_sql::testing::{PgAuth, PgTestServer};
use cameleer_sql::{datasource_from_url, transaction_policy, Connection, DataSource, JdbcIdempotentRepository, PgDataSource};
use common::*;

fn server(auth: PgAuth) -> PgTestServer {
    PgTestServer::start(&unique("pgdb"), auth, &[("camel", "s3cret"), ("other", "pw")]).unwrap()
}

fn config(s: &PgTestServer, user: &str, password: &str) -> PgConfig {
    PgConfig::from_url(&s.url(user, password)).unwrap()
}

#[test]
fn producer_suite_over_postgres_with_scram() {
    let s = server(PgAuth::ScramSha256);
    let ds = datasource_from_url(&s.url("camel", "s3cret")).unwrap();
    let ctx = context_with(ds.clone());
    producer_suite(&ctx, &ds);
    // the pool keeps connections: far fewer TCP connections than statements
    assert!(s.connection_count() < 20, "{}", s.connection_count());
    assert!(s.received_queries().iter().any(|q| q.contains("$1")), "extended protocol with numbered parameters");
}

#[test]
fn stored_procedure_suite_over_postgres() {
    let s = server(PgAuth::ScramSha256);
    let ds = datasource_from_url(&s.url("camel", "s3cret")).unwrap();
    let ctx = context_with(ds.clone());
    let dir = std::env::temp_dir().join(unique("pgstored"));
    std::fs::create_dir_all(&dir).unwrap();
    stored_suite(&ctx, &ds, &dir, true);
    std::fs::remove_dir_all(&dir).ok();
    assert!(s.received_queries().iter().any(|q| q.starts_with("CALL ") && q.contains("$1")), "{:?}", s.received_queries());
    assert!(s.received_queries().iter().any(|q| q.starts_with("SELECT * FROM subnumbers_function(CAST($1 AS INTEGER), CAST($2 AS INTEGER))")));
}

#[test]
fn procedures_and_functions_over_the_wire() {
    let s = server(PgAuth::Md5);
    let mut c = PgConnection::connect(&config(&s, "camel", "s3cret")).unwrap();
    // simple query protocol: DDL with dollar quoting, CALL with OUT parameters as a row
    c.execute("CREATE PROCEDURE split_name(IN full_name TEXT, OUT first_name TEXT, OUT last_name TEXT) LANGUAGE plpgsql AS $body$ BEGIN first_name := split_part(full_name, ' ', 1); last_name := split_part(full_name, ' ', 2); END $body$", &[])
        .unwrap_or_else(|e| panic!("{}", e));
    let r = c.execute("CALL split_name('Ada Lovelace', NULL, NULL)", &[]).unwrap();
    assert!(r.is_query);
    assert_eq!(r.columns, vec!["first_name", "last_name"]);
    assert_eq!(r.rows, vec![vec![Value::from("Ada"), Value::from("Lovelace")]]);
    // extended protocol with parameters
    let r = c.execute("CALL split_name(?, NULL, NULL)", &[Value::from("Grace Hopper")]).unwrap();
    assert_eq!(r.rows, vec![vec![Value::from("Grace"), Value::from("Hopper")]]);
    c.execute("CREATE FUNCTION area(w DOUBLE PRECISION, h DOUBLE PRECISION, OUT square DOUBLE PRECISION, OUT perimeter DOUBLE PRECISION) AS $$ BEGIN square := w * h; perimeter := 2 * (w + h); END $$ LANGUAGE plpgsql", &[]).unwrap();
    let r = c.execute("SELECT * FROM area(?, ?)", &[Value::Float(2.0), Value::Float(3.5)]).unwrap();
    assert_eq!(r.columns, vec!["square", "perimeter"]);
    assert_eq!(r.rows, vec![vec![Value::Float(7.0), Value::Float(11.0)]]);
    c.execute("CREATE FUNCTION inc(i INTEGER) RETURNS INTEGER AS $$ SELECT i + 1 $$ LANGUAGE sql", &[]).unwrap();
    let r = c.execute("SELECT inc(41)", &[]).unwrap();
    assert_eq!(r.columns, vec!["inc"]);
    assert_eq!(r.rows, vec![vec![Value::Int(42)]]);
    // errors raised by procedures keep their SQLSTATE
    c.execute("CREATE PROCEDURE fail(IN code TEXT) LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'failed: %', code USING ERRCODE = '22023'; END $$", &[]).unwrap();
    let e = c.execute("CALL fail('x')", &[]).unwrap_err();
    assert!(e.message.contains("failed: x") && e.message.contains("22023"), "{}", e.message);
    // a procedure without OUT parameters completes with the CALL tag
    c.execute("CREATE PROCEDURE noop() LANGUAGE plpgsql AS $$ BEGIN NULL; END $$", &[]).unwrap();
    let r = c.execute("CALL noop()", &[]).unwrap();
    assert!(!r.is_query);
    assert_eq!(r.command, "CALL");
}

#[test]
fn scram_md5_cleartext_and_trust_authentication() {
    for auth in [PgAuth::ScramSha256, PgAuth::Md5, PgAuth::Cleartext, PgAuth::Trust] {
        let s = server(auth);
        let mut c = PgConnection::connect(&config(&s, "camel", "s3cret")).unwrap_or_else(|e| panic!("{:?}: {}", auth, e));
        assert_eq!(c.server_parameters().get("server_encoding").map(|s| s.as_str()), Some("UTF8"));
        let r = c.execute("SELECT 1 + 1 AS two, 'x' || 'y' AS s, TRUE AS b, 1.5 AS f, NULL AS n", &[]).unwrap();
        assert_eq!(r.columns, vec!["two", "s", "b", "f", "n"]);
        assert_eq!(r.rows[0], vec![Value::Int(2), Value::from("xy"), Value::Bool(true), Value::Float(1.5), Value::Null]);
        if auth != PgAuth::Trust {
            let e = PgConnection::connect(&config(&s, "camel", "wrong")).err().unwrap();
            assert_eq!(e.kind, "SQLNonTransientConnectionException", "{:?}", auth);
            assert!(e.message.contains("password authentication failed for user \"camel\""), "{}", e.message);
            assert!(e.message.contains("28P01"));
            assert!(PgConnection::connect(&config(&s, "nobody", "s3cret")).is_err());
            assert!(s.auth_failures() >= 2);
        }
        let mut cfg = config(&s, "camel", "s3cret");
        cfg.database = "nope".into();
        let e = PgConnection::connect(&cfg).err().unwrap();
        assert!(e.message.contains("database \"nope\" does not exist"), "{}", e.message);
    }
}

#[test]
fn simple_and_extended_queries_types_and_errors() {
    let s = server(PgAuth::ScramSha256);
    let mut c = PgConnection::connect(&config(&s, "camel", "s3cret")).unwrap();
    let r = c
        .execute(
            "CREATE TABLE t (id BIGSERIAL PRIMARY KEY, name VARCHAR(20), price NUMERIC(8,2), ok BOOLEAN, at TIMESTAMP, data BYTEA); INSERT INTO t (name, price, ok, at) VALUES ('a', 1.25, true, '2024-01-02 03:04:05')",
            &[],
        )
        .unwrap();
    assert_eq!(r.update_count, 1);
    let r = c.execute("INSERT INTO t (name, price, ok, data) VALUES (?, ?, ?, ?)", &[Value::from("b"), Value::Float(2.5), Value::Bool(false), Value::Bytes(vec![1, 2, 255])]).unwrap();
    assert_eq!(r.update_count, 1);
    assert!(!r.is_query);
    let r = c.execute("SELECT id, name, price, ok, at, data FROM t WHERE id >= $1 ORDER BY id", &[Value::Int(1)]).unwrap();
    assert_eq!(r.rows.len(), 2);
    assert_eq!(r.rows[0], vec![Value::Int(1), Value::from("a"), Value::Float(1.25), Value::Bool(true), Value::from("2024-01-02 03:04:05"), Value::Null]);
    assert_eq!(r.rows[1][5], Value::Bytes(vec![1, 2, 255]));
    assert_eq!(r.column_types[0], cameleer_sql::ColumnType::BigInt);
    assert_eq!(r.column_types[3], cameleer_sql::ColumnType::Boolean);
    // NULL parameters and RETURNING
    let r = c.execute("INSERT INTO t (name, price) VALUES (?, ?) RETURNING id, name", &[Value::from("c"), Value::Null]).unwrap();
    assert!(r.is_query);
    assert_eq!(r.rows, vec![vec![Value::Int(3), Value::from("c")]]);
    // update / delete counts
    assert_eq!(c.execute("UPDATE t SET ok = ? WHERE ok IS NULL OR ok = false", &[Value::Bool(true)]).unwrap().update_count, 2);
    assert_eq!(c.execute("DELETE FROM t WHERE id = 3", &[]).unwrap().update_count, 1);
    // generated keys via RETURNING *
    let r = c.execute_with("INSERT INTO t (name) VALUES (?)", &[Value::from("k")], &cameleer_sql::ExecuteOptions { return_generated_keys: true, max_rows: 0 }).unwrap();
    assert_eq!(r.generated_keys.len(), 1);
    assert_eq!(r.generated_keys[0].get("id"), Some(&Value::Int(4)));
    // errors map to SQLException kinds, and the connection stays usable
    let e = c.execute("SELECT * FROM missing", &[]).unwrap_err();
    assert_eq!(e.kind, "SQLSyntaxErrorException");
    assert!(e.message.contains("relation \"missing\" does not exist [SQLState: 42P01]"), "{}", e.message);
    let e = c.execute("INSERT INTO t (id, name) VALUES (?, ?)", &[Value::Int(1), Value::from("dup")]).unwrap_err();
    assert_eq!(e.kind, "SQLIntegrityConstraintViolationException");
    let e = c.execute("SELEC 1", &[]).unwrap_err();
    assert!(e.message.contains("syntax error at or near \"SELEC\""), "{}", e.message);
    let e = c.execute("SELECT ? + 1", &[]).unwrap_err();
    assert!(e.message.contains("No value specified for parameter 1"), "{}", e.message);
    assert_eq!(c.execute("SELECT COUNT(*) FROM t", &[]).unwrap().scalar(), Some(&Value::Int(3)));
    // max rows
    let r = c.execute_with("SELECT * FROM t", &[], &cameleer_sql::ExecuteOptions { max_rows: 2, ..Default::default() }).unwrap();
    assert_eq!(r.rows.len(), 2);
    let r = c.execute_with("SELECT * FROM t WHERE id > ?", &[Value::Int(0)], &cameleer_sql::ExecuteOptions { max_rows: 1, ..Default::default() }).unwrap();
    assert_eq!(r.rows.len(), 1);
}

#[test]
fn transactions_over_the_wire() {
    let s = server(PgAuth::ScramSha256);
    let cfg = config(&s, "camel", "s3cret");
    let mut a = PgConnection::connect(&cfg).unwrap();
    let mut b = PgConnection::connect(&cfg).unwrap();
    a.execute("CREATE TABLE acc (id INT PRIMARY KEY, bal INT)", &[]).unwrap();
    a.execute("INSERT INTO acc VALUES (1, 100), (2, 0)", &[]).unwrap();
    a.begin().unwrap();
    assert!(a.in_transaction());
    assert_eq!(a.transaction_status(), b'T');
    a.execute("UPDATE acc SET bal = bal - ? WHERE id = 1", &[Value::Int(30)]).unwrap();
    a.execute("UPDATE acc SET bal = bal + ? WHERE id = 2", &[Value::Int(30)]).unwrap();
    a.rollback().unwrap();
    assert!(!a.in_transaction());
    assert_eq!(b.execute("SELECT bal FROM acc WHERE id = 1", &[]).unwrap().scalar(), Some(&Value::Int(100)));
    a.begin().unwrap();
    a.execute("UPDATE acc SET bal = bal - 30 WHERE id = 1", &[]).unwrap();
    a.commit().unwrap();
    assert_eq!(b.execute("SELECT bal FROM acc WHERE id = 1", &[]).unwrap().scalar(), Some(&Value::Int(70)));
    // an error aborts the transaction (status E); commit reports the rollback
    a.begin().unwrap();
    a.execute("UPDATE acc SET bal = 0 WHERE id = 1", &[]).unwrap();
    assert!(a.execute("INSERT INTO acc VALUES (1, 1)", &[]).is_err());
    assert_eq!(a.transaction_status(), b'E');
    let e = a.execute("SELECT 1", &[]).unwrap_err();
    assert!(e.message.contains("current transaction is aborted"), "{}", e.message);
    assert!(a.commit().is_err());
    assert_eq!(b.execute("SELECT bal FROM acc WHERE id = 1", &[]).unwrap().scalar(), Some(&Value::Int(70)));
    // a dropped connection with an open transaction is rolled back by the server
    {
        let mut c = PgConnection::connect(&cfg).unwrap();
        c.begin().unwrap();
        c.execute("DELETE FROM acc", &[]).unwrap();
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        // the server notices the closed socket asynchronously
        let n = b.execute("SELECT COUNT(*) FROM acc", &[]).unwrap().scalar().and_then(|v| v.as_i64()).unwrap();
        if n == 2 || std::time::Instant::now() > deadline {
            assert_eq!(n, 2);
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn pooled_datasource_rolls_back_returned_connections() {
    let s = server(PgAuth::Md5);
    let ds = PgDataSource::from_url(&s.url("camel", "s3cret")).unwrap();
    {
        let mut c = ds.get_connection().unwrap();
        c.execute("CREATE TABLE p (v INT)", &[]).unwrap();
        c.begin().unwrap();
        c.execute("INSERT INTO p VALUES (1)", &[]).unwrap();
    }
    assert_eq!(ds.idle_connections(), 1);
    let mut c = ds.get_connection().unwrap();
    assert!(!c.in_transaction());
    assert_eq!(c.execute("SELECT COUNT(*) FROM p", &[]).unwrap().scalar(), Some(&Value::Int(0)));
    drop(c);
    assert_eq!(s.connection_count(), 1, "the pooled connection was reused");
    ds.close();
    assert_eq!(ds.idle_connections(), 0);
}

#[test]
fn routes_over_postgres_consumer_transactions_and_idempotency() {
    let s = server(PgAuth::ScramSha256);
    let ds: Arc<dyn DataSource> = Arc::new(PgDataSource::from_url(&s.url("camel", "s3cret")).unwrap());
    exec(&ds, "CREATE TABLE events (id SERIAL PRIMARY KEY, kind VARCHAR(10), handled BOOLEAN DEFAULT false)");
    exec(&ds, "INSERT INTO events (kind) VALUES ('a'), ('b'), ('a')");
    let ctx = context_with(ds.clone());
    let repo = Arc::new(JdbcIdempotentRepository::new(ds.clone(), "events").unwrap());
    let policy = transaction_policy(ds.clone());
    ctx.add_routes(|b| {
        b.from("sql:select * from events where handled = false order by id?dataSource=#ds&initialDelay=0&delay=10&onConsume=update events set handled = true where id = :#id")
            .idempotent_consumer(simple("${body[kind]}"), repo.clone())
            .to("mock:events")
            .end();
        b.from("direct:tx")
            .policy(policy.clone())
            .to("sql:insert into events (kind) values (:#kind)?dataSource=#ds")
            .filter(simple("${header.kind} == 'bad'"))
            .throw_exception("IllegalArgumentException", "bad kind")
            .end()
            .end();
    })
    .unwrap();
    let mock = ctx.mock_endpoint("mock:events").unwrap();
    mock.expected_message_count(2);
    ctx.start().unwrap();
    mock.assert_is_satisfied().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while count(&ds, "SELECT COUNT(*) FROM events WHERE handled = false") > 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(count(&ds, "SELECT COUNT(*) FROM events WHERE handled = false"), 0);
    assert_eq!(count(&ds, "SELECT COUNT(*) FROM CAMEL_MESSAGEPROCESSED"), 2);
    let t = ctx.create_producer_template();
    let before = count(&ds, "SELECT COUNT(*) FROM events");
    assert!(t.request("direct:tx", |ex| ex.set_header("kind", "bad")).is_err());
    assert_eq!(count(&ds, "SELECT COUNT(*) FROM events"), before, "rolled back over the wire");
    ctx.stop().unwrap();
}

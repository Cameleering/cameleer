use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use camel_core::prelude::*;
use camel_datastores::cassandra::{headers as h, Consistency, CqlConfig, CqlSession, ExecOptions};
use camel_datastores::testing::{CassandraServer, CassandraServerConfig};

const WAIT: Duration = Duration::from_secs(10);

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    camel_datastores::register(&ctx);
    ctx
}

fn setup(server: &CassandraServer) {
    server.execute("CREATE KEYSPACE shop WITH replication = {'class': 'SimpleStrategy', 'replication_factor': 1}").unwrap();
    server.execute("CREATE TABLE shop.users (id int PRIMARY KEY, name text, email text, created timestamp, uid uuid, score double, tags list<text>)").unwrap();
    server.execute("CREATE TABLE shop.events (day text, seq int, msg text, PRIMARY KEY ((day), seq))").unwrap();
}

#[test]
fn producer_crud_and_conversion_strategies() {
    let server = CassandraServer::start().unwrap();
    setup(&server);
    let ctx = context();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let base = format!("cql://127.0.0.1:{}/shop", server.port());
    let insert = format!("{}?cql=INSERT INTO users (id, name, email, created, uid, score, tags) VALUES (?, ?, ?, ?, ?, ?, ?)&consistencyLevel=QUORUM", base);
    for (id, name) in [(1, "ada"), (2, "bob"), (3, "cy")] {
        let body = Value::Array(vec![
            Value::Int(id),
            name.into(),
            format!("{}@example.com", name).into(),
            Value::Int(1_700_000_000_000 + id),
            "123e4567-e89b-12d3-a456-42661417400a".into(),
            Value::Float(id as f64 * 1.5),
            Value::Array(vec!["x".into(), name.into()]),
        ]);
        t.send_body(&insert, body).unwrap();
    }
    assert!(server.consistencies().contains(&Consistency::Quorum));

    // ALL (default) with a single bind value
    let rows = t.request_body(&format!("{}?cql=SELECT id, name, created, uid, score, tags FROM users WHERE id = ?", base), Value::Int(2)).unwrap();
    assert_eq!(
        rows,
        Value::Array(vec![Value::object([
            ("id", Value::Int(2)),
            ("name", Value::from("bob")),
            ("created", Value::from("2023-11-14T22:13:20.002Z")),
            ("uid", Value::from("123e4567-e89b-12d3-a456-42661417400a")),
            ("score", Value::Float(3.0)),
            ("tags", Value::Array(vec!["x".into(), "bob".into()])),
        ])])
    );
    // ONE
    let one = t.request_body(&format!("{}?cql=SELECT name FROM users WHERE id IN (1, 3)&resultSetConversionStrategy=ONE", base), Value::Null).unwrap();
    assert_eq!(one, Value::object([("name", "ada")]));
    let none = t.request_body(&format!("{}?cql=SELECT name FROM users WHERE id = 99&resultSetConversionStrategy=ONE", base), Value::Null).unwrap();
    assert_eq!(none, Value::Null);
    // LIMIT_n with paging (fetchSize 1 forces several pages)
    let limited = t.request_body(&format!("{}?cql=SELECT id FROM users&resultSetConversionStrategy=LIMIT_2&fetchSize=1", base), Value::Null).unwrap();
    assert_eq!(limited.as_array().unwrap().len(), 2);
    let all = t.request_body(&format!("{}?cql=SELECT id FROM users&fetchSize=1", base), Value::Null).unwrap();
    assert_eq!(all.as_array().unwrap().len(), 3);

    // UPDATE / DELETE through the CamelCqlQuery header
    let generic = format!("{}?cql=SELECT * FROM users", base);
    t.request_body_and_header(&generic, Value::Array(vec!["ADA".into(), Value::Int(1)]), h::CQL_QUERY, "UPDATE users SET name = ? WHERE id = ?").unwrap();
    t.request_body_and_header(&generic, Value::Int(3), h::CQL_QUERY, "DELETE FROM users WHERE id = ?").unwrap();
    let names = t.request_body(&format!("{}?cql=SELECT name FROM users", base), Value::Null).unwrap();
    let mut names: Vec<String> = names.as_array().unwrap().iter().map(|r| r.get("name").unwrap().to_string()).collect();
    names.sort();
    assert_eq!(names, vec!["ADA", "bob"]);

    // clustering + ORDER BY, unprepared statements with literals
    let plain = format!("{}?prepareStatements=false&cql=INSERT INTO events (day, seq, msg) VALUES ('mon', 1, 'first')", base);
    t.send_body(&plain, Value::Null).unwrap();
    t.request_body_and_header(&format!("{}?prepareStatements=false&cql=x", base), Value::Array(vec!["mon".into(), Value::Int(2), "second".into()]), h::CQL_QUERY, "INSERT INTO events (day, seq, msg) VALUES (?, ?, ?)").unwrap();
    let ev = t.request_body(&format!("{}?cql=SELECT seq, msg FROM events WHERE day = 'mon' ORDER BY seq DESC", base), Value::Null).unwrap();
    assert_eq!(ev, Value::Array(vec![Value::object([("seq", Value::Int(2)), ("msg", Value::from("second"))]), Value::object([("seq", Value::Int(1)), ("msg", Value::from("first"))])]));

    // errors
    let err = t.request_body(&format!("{}?cql=SELECT * FROM users WHERE name = 'bob'", base), Value::Null).unwrap_err();
    assert!(err.root_cause().is_a("InvalidQueryException"), "{:?}", err);
    assert!(err.root_cause().message.contains("ALLOW FILTERING"));
    let err = t.request_body(&format!("{}?cql=SELEKT 1", base), Value::Null).unwrap_err();
    assert!(err.root_cause().is_a("SyntaxError") && err.root_cause().is_a("DriverException"), "{:?}", err);
    let err = t.request_body(&format!("{}?cql=SELECT * FROM users WHERE id = ?", base), Value::Null).unwrap_err();
    assert!(err.root_cause().message.contains("bind variables"), "{:?}", err);
    assert!(ctx.get_endpoint(&format!("{}?cql=x&resultSetConversionStrategy=SOME", base)).is_err());
    assert!(ctx.get_endpoint(&format!("{}?cql=x&consistencyLevel=MOST", base)).is_err());
    ctx.stop().unwrap();
}

#[test]
fn polling_consumer() {
    let server = CassandraServer::start().unwrap();
    setup(&server);
    server.execute("INSERT INTO shop.users (id, name) VALUES (1, 'ada')").unwrap();
    let ctx = context();
    let uri = format!("cql://127.0.0.1:{}/shop?cql=SELECT id, name FROM users&initialDelay=0&delay=50&resultSetConversionStrategy=ALL", server.port());
    ctx.add_routes(|r| {
        r.from(&uri).to("mock:poll");
    })
    .unwrap();
    let mock = ctx.mock_endpoint("mock:poll").unwrap();
    mock.expected_minimum_message_count(2);
    ctx.start().unwrap();
    mock.assert_is_satisfied_within(WAIT).unwrap();
    assert_eq!(mock.received_exchanges()[0].body(), &Value::Array(vec![Value::object([("id", Value::Int(1)), ("name", Value::from("ada"))])]));
    ctx.stop().unwrap();
}

#[test]
fn password_authenticator() {
    let mut users = BTreeMap::new();
    users.insert("cassandra".to_string(), "secret".to_string());
    let server = CassandraServer::start_with(CassandraServerConfig { users, tls: None }).unwrap();
    setup(&server);
    let ctx = context();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let ok = format!("cql://127.0.0.1:{}/shop?cql=SELECT * FROM system.local&username=cassandra&password=secret&resultSetConversionStrategy=ONE", server.port());
    let local = t.request_body(&ok, Value::Null).unwrap();
    assert_eq!(local.get("release_version"), Some(&Value::from("4.1.0")));
    let bad = format!("cql://127.0.0.1:{}/shop?cql=SELECT * FROM users&username=cassandra&password=nope", server.port());
    let err = t.request_body(&bad, Value::Null).unwrap_err();
    assert!(err.root_cause().is_a("AuthenticationException"), "{:?}", err);
    assert!(err.root_cause().message.contains("incorrect"));
    let anon = format!("cql://127.0.0.1:{}/shop?cql=SELECT * FROM users", server.port());
    let err = t.request_body(&anon, Value::Null).unwrap_err();
    assert!(err.root_cause().is_a("AuthenticationException"), "{:?}", err);
    ctx.stop().unwrap();
}

#[test]
fn session_bean_tls_and_multiple_contact_points() {
    let tls = Arc::new(camel_tls::testing::server_params().server_config().unwrap());
    let server = CassandraServer::start_with(CassandraServerConfig { tls: Some(tls), ..Default::default() }).unwrap();
    setup(&server);
    let ctx = context();
    camel_tls::testing::bind_test_beans(&ctx);
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    // first contact point refuses connections, the second one is used
    let uri = format!("cql://localhost:1,localhost:{}/shop?cql=INSERT INTO users (id, name) VALUES (?, ?)&sslContextParameters=#ssl", server.port());
    t.send_body(&uri, Value::Array(vec![Value::Int(7), "tls".into()])).unwrap();
    let untrusted = format!("cql://localhost:{}/shop?cql=SELECT * FROM users&sslContextParameters=#sslUntrusted", server.port());
    let err = t.request_body(&untrusted, Value::Null).unwrap_err();
    assert!(err.root_cause().is_a("IOException"), "{:?}", err);
    // session bean
    let mut cfg = CqlConfig::new("localhost", server.port());
    cfg.keyspace = Some("shop".into());
    cfg.tls = Some(camel_tls::camel_support::EndpointTls::new(Arc::new(camel_tls::testing::client_params())));
    ctx.registry().bind("session", CqlSession::new(cfg));
    let v = t.request_body("cql:bean:session?cql=SELECT name FROM users WHERE id = 7&resultSetConversionStrategy=ONE", Value::Null).unwrap();
    assert_eq!(v, Value::object([("name", "tls")]));
    ctx.stop().unwrap();
}

#[test]
fn client_paging_with_prepared_statements() {
    let server = CassandraServer::start().unwrap();
    setup(&server);
    let mut cfg = CqlConfig::new("127.0.0.1", server.port());
    cfg.keyspace = Some("shop".into());
    let session = CqlSession::new(cfg);
    let o = ExecOptions { fetch_size: 3, ..Default::default() };
    for i in 0..10 {
        session.execute("INSERT INTO events (day, seq, msg) VALUES (?, ?, ?)", &[Value::from("d"), Value::Int(i), Value::from(format!("m{}", i))], &o).unwrap();
    }
    let rows = session.execute("SELECT seq FROM events WHERE day = ?", &[Value::from("d")], &o).unwrap();
    assert_eq!(rows.rows.len(), 10);
    assert_eq!(rows.columns[0].name, "seq");
    let pages = server.queries().iter().filter(|q| q.starts_with("SELECT seq")).count();
    assert_eq!(pages, 1, "prepared once, paged with EXECUTE");
    let rows = session.execute("SELECT COUNT(*) FROM events WHERE day = 'd'", &[], &o).unwrap();
    assert_eq!(rows.rows, vec![vec![Value::Int(10)]]);
    session.close();
}

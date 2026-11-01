//! Official-client cross-verification of the embedded MongoDB, Cassandra, Elasticsearch and
//! Redis servers: pymongo, the DataStax Python driver, elasticsearch-py and redis-py run
//! against them (plain, TLS, authenticated), and exchange data with rust-camel's components.
//! Runs only with `CAMEL_OFFICIAL_CLIENTS=1` (see `scripts/official-clients.sh`).

use std::collections::BTreeMap;
use std::sync::Arc;

use camel_core::prelude::*;
use camel_datastores::testing::*;
use camel_tls::testing::official::{self, python_script, Run};

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    camel_datastores::register(&ctx);
    ctx
}

fn users() -> BTreeMap<String, String> {
    [("alice".to_string(), "alice-pw".to_string())].into_iter().collect()
}

fn tls() -> Arc<camel_tls::TlsServerConfig> {
    Arc::new(official::server_params().server_config().unwrap())
}

fn py(script: &str, args: &[&str]) -> Run {
    python_script(script, args, &[])
}

// ---------------------------------------------------------------------------
// MongoDB / pymongo
// ---------------------------------------------------------------------------

#[test]
fn pymongo_crud_aggregation_and_change_streams() {
    if !official::enabled() {
        return;
    }
    let server = MongoServer::start().unwrap();
    let uri = format!("mongodb://127.0.0.1:{}/", server.port());
    let r = py("mongo_pymongo.py", &[&uri, "crud"]).ok();
    let expect = [
        ("inserted-one:", "1"),
        ("inserted-many:", "3"),
        ("duplicate:", "E11000"),
        ("find-one:", "ada 36 1815 0001ff 1.5"),
        ("find-sorted:", "grace,alan,ada"),
        ("find-range:", "edsger"),
        ("count:", "1"),
        ("estimated:", "4"),
        ("distinct:", "code,math,navy"),
        ("update-one:", "1 1"),
        ("update-many:", "2 2"),
        ("upsert:", "True"),
        ("replace:", "1"),
        ("find-and-modify:", "86"),
        ("bulk:", "1 2 2 1"),
        ("index:", "name_1"),
        ("indexes:", "_id_,name_1"),
        ("delete-many:", "1"),
        ("cursor:", "250"),
        ("collections:", "many,people"),
        ("databases-has-official:", "True"),
        ("dropped:", "True"),
    ];
    for (k, v) in expect {
        assert_eq!(r.value(k), v, "{}\n{}", k, r.stdout);
    }
    // the unique index pymongo created is enforced
    assert_eq!(server.documents("official", "people").len(), 4);
    let r = py("mongo_pymongo.py", &[&uri, "aggregate"]).ok();
    assert_eq!(r.value("group:"), "carrot=10/5.0/1;apple=8/12.0/2;pear=2/4.0/1");
    assert_eq!(r.value("unwind:"), "fruit=3;red=2;veg=1");
    assert_eq!(r.value("project:"), "apple=True;apple=False");
    assert_eq!(r.value("count:"), "3");
    let r = py("mongo_pymongo.py", &[&uri, "watch"]).ok();
    assert_eq!(r.value("events:"), "insert,update,delete");
    assert_eq!(r.value("update-fields:"), "{'v': 2}");
    assert_eq!(r.value("full-document:"), "2");
    assert_eq!(r.value("resume-token:"), "True");
    assert!(server.commands().iter().any(|c| c == "getMore"));
}

#[test]
fn pymongo_scram_tls_and_x509() {
    if !official::enabled() {
        return;
    }
    let pem = official::pem_files("pymongo");
    let ca = pem.ca.display().to_string();
    let server = MongoServer::start_with(MongoServerConfig { users: users(), tls: Some(tls()), ..Default::default() }).unwrap();
    for mech in ["SCRAM-SHA-256", "SCRAM-SHA-1"] {
        let uri = format!("mongodb://alice:alice-pw@localhost:{}/?tls=true&tlsCAFile={}&authMechanism={}&authSource=admin", server.port(), ca, mech);
        let r = py("mongo_pymongo.py", &[&uri, "aggregate"]).ok();
        assert_eq!(r.value("count:"), "3", "{}", mech);
    }
    let bad = format!("mongodb://alice:wrong@localhost:{}/?tls=true&tlsCAFile={}", server.port(), ca);
    let r = py("mongo_pymongo.py", &[&bad, "auth-fail"]).ok();
    assert!(r.value("auth-failed:").starts_with("18 "), "{}", r.stdout);
    // pymongo verifies the host name: the certificate has no SAN for 127.0.0.2
    let wrong_host = format!("mongodb://alice:alice-pw@127.0.0.2:{}/?tls=true&tlsCAFile={}&serverSelectionTimeoutMS=3000", server.port(), ca);
    assert_ne!(py("mongo_pymongo.py", &[&wrong_host, "auth-fail"]).status, 0);

    // MONGODB-X509 with a client certificate
    let combined = pem.dir.join("client-combined.pem");
    std::fs::write(&combined, format!("{}{}", std::fs::read_to_string(&pem.client_cert).unwrap(), std::fs::read_to_string(&pem.client_key).unwrap())).unwrap();
    let mtls = Arc::new(camel_tls::testing::mtls_server_params(camel_tls::ClientAuth::Want).server_config().unwrap());
    let x509 = MongoServer::start_with(MongoServerConfig { tls: Some(mtls), x509_users: vec!["CN=camel-client".into()], ..Default::default() }).unwrap();
    let uri = format!("mongodb://localhost:{}/?tls=true&tlsCAFile={}&tlsCertificateKeyFile={}&authMechanism=MONGODB-X509", x509.port(), ca, combined.display());
    let r = py("mongo_pymongo.py", &[&uri, "whoami"]).ok();
    assert_eq!(r.value("x509-insert:"), "1");
    let _ = std::fs::remove_dir_all(&pem.dir);
}

#[test]
fn pymongo_and_rust_camel_share_documents() {
    if !official::enabled() {
        return;
    }
    let server = MongoServer::start().unwrap();
    let uri = format!("mongodb://127.0.0.1:{}/", server.port());
    py("mongo_pymongo.py", &[&uri, "seed"]).ok();
    let ctx = context();
    ctx.registry().bind("db", camel_datastores::mongodb::MongoClient::parse(&server.connection_string()).unwrap());
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let found = t.request_body("mongodb:db?database=camel&collection=fromPymongo&operation=findAll", Value::Null).unwrap();
    let docs = found.as_array().unwrap();
    assert_eq!(docs.len(), 3);
    assert_eq!(docs[1].get("_id"), Some(&Value::from("p1")));
    assert_eq!(docs[1].get("n").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(docs[2].get("i64").and_then(|v| v.as_i64()), Some((1i64 << 40) + 2));
    for k in ["when", "bin", "oid", "dec"] {
        assert!(docs[0].get(k).is_some(), "{} decoded: {:?}", k, docs[0]);
    }
    for i in 0..3 {
        t.request_body("mongodb:db?database=camel&collection=fromCamel&operation=insert", format!(r#"{{"name":"camel-{}","n":{},"nested":{{"ok":true}}}}"#, i, i)).unwrap();
    }
    let r = py("mongo_pymongo.py", &[&uri, "check"]).ok();
    assert_eq!(r.values("doc:"), vec!["camel-0 0 int True", "camel-1 1 int True", "camel-2 2 int True"]);
    ctx.stop().unwrap();
}

// ---------------------------------------------------------------------------
// Cassandra / DataStax python driver
// ---------------------------------------------------------------------------

fn check_cql(r: &Run) {
    assert_eq!(r.value("prepared-columns:"), "12");
    assert_eq!(r.value("row:"), "user7 u7@example.com 10.5 False 2024-05-17T10:30:00 12345678-1234-5678-1234-567812345678 a,b7 7 1099511627783 07ff 12.07");
    assert_eq!(r.value("prepared-select:"), "user3");
    assert_eq!(r.value("paging:"), "25 rows 3 pages");
    assert_eq!(r.value("auto-paging:"), "25");
    assert_eq!(r.value("batch:"), "100,101,102");
    assert_eq!(r.value("count:"), "24");
    assert_eq!(r.value("invalid:"), "InvalidRequest");
    assert_eq!(r.value("metadata-keyspace:"), "True");
    assert_eq!(r.value("release-version:"), "4.1.0");
}

#[test]
fn cassandra_driver_protocol_v4() {
    if !official::enabled() {
        return;
    }
    let server = CassandraServer::start().unwrap();
    let port = server.port().to_string();
    let r = py("cassandra_driver.py", &[&port, "cql"]).ok();
    check_cql(&r);
    assert_eq!(r.value("compressor:"), "none");
    // frame compression negotiated through STARTUP (python-lz4 / python-snappy)
    let r = py("cassandra_driver.py", &[&port, "cql", "", "", "", "lz4"]).ok();
    check_cql(&r);
    assert_eq!(r.value("compressor:"), "lz4_compress");
    let r = py("cassandra_driver.py", &[&port, "cql", "", "", "", "snappy"]).ok();
    assert_eq!(r.value("compressor:"), "compress");
    assert!(server.queries().iter().any(|q| q.contains("system_schema.columns")));
}

#[test]
fn cassandra_driver_auth_tls_and_rust_camel() {
    if !official::enabled() {
        return;
    }
    let pem = official::pem_files("cassandra");
    let ca = pem.ca.display().to_string();
    let server = CassandraServer::start_with(CassandraServerConfig { users: users(), tls: Some(tls()) }).unwrap();
    let port = server.port().to_string();
    let r = py("cassandra_driver.py", &[&port, "cql", "alice", "alice-pw", &ca, "lz4"]).ok();
    check_cql(&r);
    let r = py("cassandra_driver.py", &[&port, "auth-fail", "alice", "wrong", &ca]).ok();
    assert!(r.value("auth-failed:").starts_with("AuthenticationFailed"), "{}", r.stdout);
    // data written by the driver is read by the cql component, and the other way round
    py("cassandra_driver.py", &[&port, "seed", "alice", "alice-pw", &ca]).ok();
    let ctx = context();
    ctx.registry().bind("ssl", official::server_params());
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let base = format!("cql://localhost:{}/camel?username=alice&password=alice-pw&sslContextParameters=#ssl", server.port());
    let rows = t.request_body(&format!("{}&cql=SELECT id, name, n FROM from_driver&resultSetConversionStrategy=ALL", base), Value::Null).unwrap();
    let mut rows = rows.as_array().unwrap().clone();
    rows.sort_by_key(|r| r.get("id").and_then(|v| v.as_i64()));
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[2].get("name"), Some(&Value::from("driver-2")));
    assert_eq!(rows[2].get("n").and_then(|v| v.as_i64()), Some(1_000_000_000_002));
    t.request_body(&format!("{}&cql=CREATE TABLE IF NOT EXISTS from_camel (id int PRIMARY KEY, name text)", base), Value::Null).unwrap();
    for i in 0..2 {
        t.request_body(&format!("{}&cql=INSERT INTO from_camel (id, name) VALUES (?, ?)", base), Value::Array(vec![Value::Int(i), Value::from(format!("camel-{}", i))])).unwrap();
    }
    let r = py("cassandra_driver.py", &[&port, "check", "alice", "alice-pw", &ca]).ok();
    assert_eq!(r.values("row:"), vec!["0 camel-0", "1 camel-1"]);
    ctx.stop().unwrap();
    let _ = std::fs::remove_dir_all(&pem.dir);
}

// ---------------------------------------------------------------------------
// Elasticsearch / elasticsearch-py
// ---------------------------------------------------------------------------

fn check_es(r: &Run) {
    let expect = [
        ("info:", "8.11.0 You Know, for Search"),
        ("created:", "True books"),
        ("exists:", "True"),
        ("indexed:", "created 1"),
        ("auto-id:", "True created"),
        ("get:", "True Dune"),
        ("missing:", "404"),
        ("updated:", "updated 2"),
        ("conflict:", "409"),
        ("match:", "1 Camel in Action"),
        ("term-sorted:", "Dune,Hyperion"),
        ("bool-range:", "2 Camel in Action"),
        ("aggs:", "tech=3 scifi=2 avg=636.4"),
        ("count:", "3"),
        ("bulk:", "50 0"),
        ("mget:", "True,True,False"),
        ("scan:", "50"),
        ("delete-by-query:", "50"),
        ("deleted:", "deleted"),
        ("final-count:", "5"),
        ("index-deleted:", "True"),
    ];
    for (k, v) in expect {
        assert_eq!(r.value(k), v, "{}\n{}", k, r.stdout);
    }
}

#[test]
fn elasticsearch_py_api() {
    if !official::enabled() {
        return;
    }
    let server = ElasticsearchServer::start().unwrap();
    let r = py("es_client.py", &[&format!("http://{}", server.address()), "api"]).ok();
    check_es(&r);

    let pem = official::pem_files("es");
    let ca = pem.ca.display().to_string();
    let secure = ElasticsearchServer::start_with(ElasticsearchServerConfig { basic_auth: Some(("alice".into(), "alice-pw".into())), tls: Some(tls()), ..Default::default() }).unwrap();
    let url = format!("https://localhost:{}", secure.port());
    check_es(&py("es_client.py", &[&url, "api", "alice", "alice-pw", &ca]).ok());
    assert_eq!(py("es_client.py", &[&url, "auth-fail", "alice", "nope", &ca]).ok().value("auth-failed:"), "401");

    // documents indexed by the python client are searched by the elasticsearch component
    py("es_client.py", &[&url, "seed", "alice", "alice-pw", &ca]).ok();
    let ctx = context();
    ctx.registry().bind("ssl", official::server_params());
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let base = format!("elasticsearch://c?hostAddresses=localhost:{}&sslContextParameters=#ssl&user=alice&password=alice-pw", secure.port());
    let doc = t.request_body(&format!("{}&indexName=from-python&operation=GetById&enableDocumentOnlyMode=true", base), "py1").unwrap();
    assert_eq!(doc.get("name"), Some(&Value::from("python-1")));
    for i in 0..2 {
        let body = camel_core::json::parse(&format!(r#"{{"name":"camel-{}","n":{}}}"#, i, i)).unwrap();
        t.request_body_and_header(&format!("{}&indexName=from-camel&operation=Index", base), body, "indexId", format!("c{}", i)).unwrap();
    }
    let r = py("es_client.py", &[&url, "check", "alice", "alice-pw", &ca]).ok();
    assert_eq!(r.values("doc:"), vec!["c0 camel-0", "c1 camel-1"]);
    ctx.stop().unwrap();
    let _ = std::fs::remove_dir_all(&pem.dir);
}

// ---------------------------------------------------------------------------
// Redis / redis-py
// ---------------------------------------------------------------------------

fn check_redis(r: &Run) {
    let expect = [
        ("ping:", "True"),
        ("get:", "v1"),
        ("setnx:", "None"),
        ("ttl:", "True"),
        ("pttl:", "True"),
        ("incr:", "1 11 1.5"),
        ("mget:", "['v1', None, 'x']"),
        ("binary:", "000102ff0d0a"),
        ("append:", "7 v1-m 7"),
        ("hash:", "[('a', '1'), ('b', '2')] 6 False"),
        ("list:", "['z', 'a', 'b', 'c'] z c 2"),
        ("blpop:", "(b'l', b'a')"),
        ("set:", "['x', 'y', 'z'] True 3"),
        ("sinter:", "['y']"),
        ("zset:", "[('bob', 1.0), ('carol', 2.0), ('alice', 3.0)] 3.0 1"),
        ("zrangebyscore:", "['carol', 'alice'] 11.0"),
        ("exists:", "2 hash zset"),
        ("scan:", "bin,f,h,l,n,p,s,set,set2,t,z"),
        ("keys-pattern:", "['s', 'set', 'set2']"),
        ("rename:", "True 0"),
        ("delete:", "1"),
        ("wrongtype:", "WRONGTYPE"),
        ("multi:", "[True, 1, 'a']"),
        ("pipeline:", "100"),
        ("watch:", "aborted"),
        ("pubsub:", "1 1 chan:hello,news.tech:rust"),
    ];
    for (k, v) in expect {
        assert_eq!(r.value(k), v, "{}\n{}", k, r.stdout);
    }
}

#[test]
fn redis_py_against_embedded_redis() {
    if !official::enabled() {
        return;
    }
    let server = RedisServer::start().unwrap();
    let port = server.port().to_string();
    check_redis(&py("redis_py.py", &["127.0.0.1", &port, "api"]).ok());

    let pem = official::pem_files("redis");
    let ca = pem.ca.display().to_string();
    let secure = RedisServer::start_with(RedisServerConfig { password: Some("default-pw".into()), users: users(), tls: Some(tls()) }).unwrap();
    let sport = secure.port().to_string();
    check_redis(&py("redis_py.py", &["localhost", &sport, "api", "default-pw", &ca]).ok());
    check_redis(&py("redis_py.py", &["localhost", &sport, "api", "alice-pw", &ca, "alice"]).ok());
    assert!(py("redis_py.py", &["localhost", &sport, "auth-fail", "wrong", &ca]).ok().value("auth-failed:").contains("WRONGPASS"));
    assert!(py("redis_py.py", &["localhost", &sport, "auth-fail", "", &ca]).ok().value("auth-failed:").contains("Authentication required"));

    // keys written by redis-py are read by spring-redis and the other way round
    py("redis_py.py", &["127.0.0.1", &port, "seed"]).ok();
    let ctx = context();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let uri = format!("spring-redis://{}", server.address());
    let get = |cmd: &str, key: &str| {
        let ex = t.send_with(&uri, ExchangePattern::InOut, |ex| {
            ex.set_header("CamelRedis.Command", cmd);
            ex.set_header("CamelRedis.Key", key);
        });
        assert!(ex.exception.is_none(), "{:?}", ex.exception);
        ex.body().clone()
    };
    assert_eq!(get("GET", "py:string"), Value::from("from-python"));
    assert_eq!(get("HGETALL", "py:hash"), Value::object([("k1", "v1"), ("k2", "v2")]));
    let send = |cmd: &str, key: &str, value: &str| {
        let ex = t.send_with(&uri, ExchangePattern::InOut, |ex| {
            ex.set_header("CamelRedis.Command", cmd);
            ex.set_header("CamelRedis.Key", key);
            ex.set_header("CamelRedis.Value", value);
        });
        assert!(ex.exception.is_none(), "{:?}", ex.exception);
    };
    send("SET", "camel:string", "from-camel");
    send("RPUSH", "camel:list", "x");
    send("RPUSH", "camel:list", "y");
    let r = py("redis_py.py", &["127.0.0.1", &port, "check"]).ok();
    assert_eq!(r.value("camel-string:"), "from-camel");
    assert_eq!(r.value("camel-list:"), "['x', 'y']");
    ctx.stop().unwrap();
    let _ = std::fs::remove_dir_all(&pem.dir);
}

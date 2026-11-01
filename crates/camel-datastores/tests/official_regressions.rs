//! Hermetic regressions for the embedded-server gaps found with the official clients
//! (`tests/official_datastores.rs`): MongoDB indexes (pymongo `create_index` /
//! `list_indexes`), Cassandra `system_schema` and peer metadata (DataStax driver schema
//! metadata and schema agreement), Elasticsearch product header / aggregations /
//! delete-by-query / scroll `_shards` (elasticsearch-py), Redis SCAN and WATCH (redis-py).

use std::io::{Read, Write};
use std::net::TcpStream;

use camel_core::value::Value;
use camel_datastores::cassandra::engine::Outcome;
use camel_datastores::cassandra::{CqlConfig, CqlSession, ExecOptions};
use camel_datastores::mongodb::{Bson, Document, MongoClient};
use camel_datastores::redis::{RedisConfig, RedisConnection, Resp};
use camel_datastores::testing::*;

#[test]
fn mongo_indexes_are_listed_dropped_and_unique() {
    let server = MongoServer::start().unwrap();
    let client = MongoClient::parse(&server.connection_string()).unwrap();
    let mut c = client.connect().unwrap();
    let cmd = |d: Document| d.with("$db", "idx");
    c.command(cmd(Document::new().with("insert", "people").with("documents", vec![Bson::Document(Document::new().with("name", "ada"))]))).unwrap();
    let r = c
        .command(cmd(Document::new().with("createIndexes", "people").with(
            "indexes",
            vec![Bson::Document(Document::new().with("key", Document::new().with("name", 1)).with("unique", true)), Bson::Document(Document::new().with("key", Document::new().with("a", 1).with("b", -1)).with("name", "ab"))],
        )))
        .unwrap();
    assert_eq!(r.get_i64("numIndexesBefore"), Some(1));
    assert_eq!(r.get_i64("numIndexesAfter"), Some(3));
    let r = c.command(cmd(Document::new().with("listIndexes", "people"))).unwrap();
    let names: Vec<String> = r.get_doc("cursor").unwrap().get_array("firstBatch").unwrap().iter().map(|d| d.as_document().unwrap().get_str("name").unwrap().to_string()).collect();
    assert_eq!(names, vec!["_id_", "name_1", "ab"]);
    // the unique index rejects a duplicate name
    let r = c.command(cmd(Document::new().with("insert", "people").with("documents", vec![Bson::Document(Document::new().with("name", "ada"))]))).unwrap();
    let errs = r.get_array("writeErrors").expect("duplicate refused");
    assert_eq!(errs[0].as_document().unwrap().get_i64("code"), Some(11000));
    assert!(errs[0].as_document().unwrap().get_str("errmsg").unwrap().contains("index: name_1"));
    // building a unique index over existing duplicates fails
    c.command(cmd(Document::new().with("insert", "dups").with("documents", vec![Bson::Document(Document::new().with("x", 1)), Bson::Document(Document::new().with("x", 1))]))).unwrap();
    assert!(c.command(cmd(Document::new().with("createIndexes", "dups").with("indexes", vec![Bson::Document(Document::new().with("key", Document::new().with("x", 1)).with("unique", true))]))).is_err());
    let r = c.command(cmd(Document::new().with("dropIndexes", "people").with("index", "ab"))).unwrap();
    assert_eq!(r.get_i64("nIndexesWas"), Some(3));
    assert!(c.command(cmd(Document::new().with("dropIndexes", "people").with("index", "_id_"))).is_err());
    c.command(cmd(Document::new().with("dropIndexes", "people").with("index", "*"))).unwrap();
    assert!(c.command(cmd(Document::new().with("listIndexes", "nope"))).is_err());
}

fn rows(o: Outcome) -> Vec<Vec<Value>> {
    match o {
        Outcome::Rows { rows, .. } => rows,
        other => panic!("{:?}", other),
    }
}

struct Cql(CqlSession);

impl Cql {
    fn query_rows(&self, q: &str) -> camel_core::error::Result<Vec<Vec<Value>>> {
        Ok(self.0.execute(q, &[], &ExecOptions { prepare: false, ..Default::default() })?.rows)
    }
}

#[test]
fn cassandra_system_schema_and_peer_metadata() {
    let server = CassandraServer::start().unwrap();
    server.execute("CREATE KEYSPACE shop WITH replication = {'class': 'SimpleStrategy', 'replication_factor': 1}").unwrap();
    server.execute("CREATE TYPE shop.addr (street text, zip int)").unwrap();
    server.execute("CREATE TABLE shop.orders (cust int, day int, amount double, tags list<text>, PRIMARY KEY ((cust), day))").unwrap();
    server.execute("CREATE INDEX ON shop.orders (amount)").unwrap();
    let _ = rows(server.execute("SELECT * FROM system.local").unwrap());
    let client = Cql(CqlSession::new(CqlConfig::new("127.0.0.1", server.port())));
    let cols = client.query_rows("SELECT column_name, kind, position, type FROM system_schema.columns WHERE keyspace_name = 'shop' AND table_name = 'orders'").unwrap();
    let mut got: Vec<String> = cols.iter().map(|r| r.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(" ")).collect();
    got.sort();
    assert_eq!(got, vec!["amount regular -1 double", "cust partition_key 0 int", "day clustering 0 int", "tags regular -1 list<text>"]);
    let ks = client.query_rows("SELECT keyspace_name, replication FROM system_schema.keyspaces WHERE keyspace_name = 'shop'").unwrap();
    assert_eq!(ks.len(), 1);
    assert!(ks[0][1].to_string().contains("SimpleStrategy"));
    let tables = client.query_rows("SELECT table_name, id FROM system_schema.tables WHERE keyspace_name = 'shop'").unwrap();
    assert_eq!(tables.len(), 1);
    let types = client.query_rows("SELECT type_name, field_names, field_types FROM system_schema.types WHERE keyspace_name = 'shop'").unwrap();
    assert_eq!(types[0][0], Value::from("addr"));
    let idx = client.query_rows("SELECT index_name, options FROM system_schema.indexes WHERE keyspace_name = 'shop'").unwrap();
    assert_eq!(idx.len(), 1);
    for t in ["functions", "aggregates", "triggers", "views"] {
        assert!(client.query_rows(&format!("SELECT * FROM system_schema.{}", t)).unwrap().is_empty());
    }
    // schema agreement columns and the v1 peers table
    let local = client.query_rows("SELECT host_id, schema_version, rpc_address FROM system.local WHERE key = 'local'").unwrap();
    assert_eq!(local.len(), 1);
    server.set_ring(&[1], &[("127.0.0.2".into(), 9042, vec![2])]);
    let peers = client.query_rows("SELECT peer, schema_version, host_id FROM system.peers").unwrap();
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0][1], local[0][1], "same schema version on every node");
    assert_eq!(client.query_rows("SELECT peer, schema_version FROM system.peers_v2").unwrap().len(), 1);
}

fn http(port: u16, method: &str, path: &str, body: &str) -> (String, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(s, "{} {} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", method, path, body.len(), body).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let (head, body) = out.split_once("\r\n\r\n").unwrap();
    (head.to_string(), body.to_string())
}

#[test]
fn elasticsearch_product_header_aggregations_and_delete_by_query() {
    let server = ElasticsearchServer::start().unwrap();
    let p = server.port();
    let (head, body) = http(p, "GET", "/", "");
    assert!(head.contains("X-Elastic-Product: Elasticsearch"), "{}", head);
    assert!(body.contains("\"build_flavor\":\"default\""));
    for (i, (g, n)) in [("a", 1), ("b", 5), ("a", 3), ("c", 7)].iter().enumerate() {
        http(p, "PUT", &format!("/agg/_doc/{}", i), &format!(r#"{{"g":"{}","n":{}}}"#, g, n));
    }
    http(p, "POST", "/agg/_refresh", "");
    let (_, body) = http(p, "POST", "/agg/_search", r#"{"size":0,"aggs":{"groups":{"terms":{"field":"g"},"aggs":{"total":{"sum":{"field":"n"}}}},"avg_n":{"avg":{"field":"n"}},"st":{"stats":{"field":"n"}},"card":{"cardinality":{"field":"g"}}}}"#);
    let v = camel_core::json::parse(&body).unwrap();
    let aggs = v.get("aggregations").unwrap();
    let buckets = aggs.get("groups").unwrap().get("buckets").unwrap().as_array().unwrap();
    assert_eq!(buckets[0].get("key"), Some(&Value::from("a")));
    assert_eq!(buckets[0].get("doc_count").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(buckets[0].get("total").unwrap().get("value").and_then(|v| v.as_f64()), Some(4.0));
    assert_eq!(aggs.get("avg_n").unwrap().get("value").and_then(|v| v.as_f64()), Some(4.0));
    assert_eq!(aggs.get("st").unwrap().get("max").and_then(|v| v.as_f64()), Some(7.0));
    assert_eq!(aggs.get("card").unwrap().get("value").and_then(|v| v.as_i64()), Some(3));
    let (head, _) = http(p, "POST", "/agg/_search", r#"{"aggs":{"x":{"nope":{}}}}"#);
    assert!(head.starts_with("HTTP/1.1 400"));
    // scroll continuation pages carry _shards (elasticsearch-py's scan helper reads it)
    let (_, body) = http(p, "POST", "/agg/_search?scroll=1m", r#"{"size":1}"#);
    let id = camel_core::json::parse(&body).unwrap().get("_scroll_id").unwrap().to_string();
    let (_, body) = http(p, "POST", "/_search/scroll", &format!(r#"{{"scroll_id":"{}"}}"#, id));
    assert!(camel_core::json::parse(&body).unwrap().get("_shards").is_some());
    let (_, body) = http(p, "POST", "/agg/_delete_by_query", r#"{"query":{"term":{"g":"a"}}}"#);
    assert_eq!(camel_core::json::parse(&body).unwrap().get("deleted").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(server.count("agg"), 2);
    // OpenSearch does not send the Elastic product header
    let os = ElasticsearchServer::start_with(ElasticsearchServerConfig { distribution: Some("opensearch".into()), ..Default::default() }).unwrap();
    assert!(!http(os.port(), "GET", "/", "").0.contains("X-Elastic-Product"));
}

fn cmd(c: &mut RedisConnection, args: &[&str]) -> Resp {
    c.command(&args.iter().map(|a| a.as_bytes().to_vec()).collect::<Vec<_>>()).unwrap()
}

fn strings(r: &Resp) -> Vec<String> {
    r.items().unwrap().iter().map(|x| String::from_utf8_lossy(x.as_bytes().unwrap()).into_owned()).collect()
}

#[test]
fn redis_scan_family_and_watch() {
    let server = RedisServer::start().unwrap();
    let cfg = RedisConfig::new("127.0.0.1", server.port());
    let mut c = RedisConnection::connect(&cfg).unwrap();
    for i in 0..25 {
        cmd(&mut c, &["SET", &format!("k{:02}", i), "v"]);
    }
    cmd(&mut c, &["HSET", "h", "f1", "1", "f2", "2"]);
    cmd(&mut c, &["SADD", "s", "a", "b"]);
    cmd(&mut c, &["ZADD", "z", "2", "two", "1", "one"]);
    // a full SCAN iteration returns every key exactly once
    let (mut cursor, mut seen) = ("0".to_string(), Vec::new());
    loop {
        let r = cmd(&mut c, &["SCAN", &cursor, "MATCH", "k*", "COUNT", "7"]);
        let items = r.items().unwrap();
        cursor = String::from_utf8_lossy(items[0].as_bytes().unwrap()).into_owned();
        seen.extend(strings(&items[1]));
        if cursor == "0" {
            break;
        }
    }
    seen.sort();
    assert_eq!(seen.len(), 25);
    seen.dedup();
    assert_eq!(seen.len(), 25);
    let r = cmd(&mut c, &["SCAN", "0", "TYPE", "hash", "COUNT", "100"]);
    assert_eq!(strings(&r.items().unwrap()[1]), vec!["h"]);
    assert_eq!(strings(&cmd(&mut c, &["HSCAN", "h", "0"]).items().unwrap()[1]), vec!["f1", "1", "f2", "2"]);
    assert_eq!(strings(&cmd(&mut c, &["SSCAN", "s", "0", "MATCH", "a"]).items().unwrap()[1]), vec!["a"]);
    assert_eq!(strings(&cmd(&mut c, &["ZSCAN", "z", "0"]).items().unwrap()[1]), vec!["one", "1", "two", "2"]);
    let raw = |c: &mut RedisConnection, a: &[&str]| c.command(&a.iter().map(|x| x.as_bytes().to_vec()).collect::<Vec<_>>());
    assert!(raw(&mut c, &["HSCAN", "s", "0"]).unwrap_err().message.starts_with("WRONGTYPE"));
    assert!(raw(&mut c, &["SCAN", "x"]).is_err());

    // WATCH: a write from another connection aborts EXEC (null array); untouched keys commit
    let mut other = RedisConnection::connect(&cfg).unwrap();
    cmd(&mut c, &["WATCH", "w"]);
    cmd(&mut other, &["SET", "w", "theirs"]);
    cmd(&mut c, &["MULTI"]);
    cmd(&mut c, &["SET", "w", "mine"]);
    assert_eq!(cmd(&mut c, &["EXEC"]), Resp::Array(None));
    assert_eq!(cmd(&mut c, &["GET", "w"]), Resp::Bulk(Some(b"theirs".to_vec())));
    cmd(&mut c, &["WATCH", "w"]);
    cmd(&mut other, &["SET", "unrelated", "x"]);
    cmd(&mut c, &["MULTI"]);
    cmd(&mut c, &["SET", "w", "mine"]);
    assert_eq!(cmd(&mut c, &["EXEC"]).items().unwrap().len(), 1);
    // FLUSHDB counts as a modification of every key; UNWATCH forgets
    cmd(&mut c, &["WATCH", "w"]);
    cmd(&mut other, &["FLUSHDB"]);
    cmd(&mut c, &["MULTI"]);
    cmd(&mut c, &["SET", "w", "again"]);
    assert_eq!(cmd(&mut c, &["EXEC"]), Resp::Array(None));
    cmd(&mut c, &["WATCH", "w"]);
    cmd(&mut other, &["SET", "w", "x"]);
    cmd(&mut c, &["UNWATCH"]);
    cmd(&mut c, &["MULTI"]);
    cmd(&mut c, &["SET", "w", "final"]);
    assert_eq!(cmd(&mut c, &["EXEC"]).items().unwrap().len(), 1);
}

//! MongoDB bulkWrite, SCRAM-SHA-1, X.509, mongodb+srv, replica-set discovery and GridFS.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use cameleer_core::prelude::*;
use cameleer_datastores::mongodb::{headers as h, Document, MongoClient, MongoClientOptions};
use cameleer_datastores::testing::{DnsRecord, DnsServer, MongoServer, MongoServerConfig, SrvRecord};

fn context(server: &MongoServer) -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_datastores::register(&ctx);
    ctx.registry().bind("myDb", MongoClient::parse(&server.connection_string()).unwrap());
    ctx
}

fn json(s: &str) -> Value {
    cameleer_core::json::parse(s).unwrap()
}

#[test]
fn bulk_write_models() {
    let server = MongoServer::start().unwrap();
    let ctx = context(&server);
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let uri = "mongodb:myDb?database=shop&collection=items&operation=bulkWrite";
    let body = json(
        r#"[
        {"insertOne": {"document": {"_id": 1, "name": "a", "qty": 1}}},
        {"insertOne": {"document": {"_id": 2, "name": "b", "qty": 2}}},
        {"_id": 3, "name": "c", "qty": 3},
        {"updateOne": {"filter": {"_id": 1}, "update": {"$set": {"qty": 10}}}},
        {"updateMany": {"filter": {"qty": {"$lt": 5}}, "update": {"$inc": {"qty": 100}}}},
        {"replaceOne": {"filter": {"_id": 9}, "replacement": {"name": "z"}, "upsert": true}},
        {"deleteOne": {"filter": {"_id": 3}}}
    ]"#,
    );
    let ex = t.send_with(uri, ExchangePattern::InOut, |ex| ex.set_body(body.clone()));
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
    let r = ex.message.body.clone();
    assert_eq!(r.get("insertedCount"), Some(&Value::Int(3)));
    assert_eq!(r.get("matchedCount"), Some(&Value::Int(3)));
    assert_eq!(r.get("modifiedCount"), Some(&Value::Int(3)));
    assert_eq!(r.get("deletedCount"), Some(&Value::Int(1)));
    assert_eq!(r.get("upsertedCount"), Some(&Value::Int(1)));
    assert_eq!(r.get("upserts").and_then(|u| u.as_array()).and_then(|a| a[0].get("index")), Some(&Value::Int(5)));
    let docs = server.documents("shop", "items");
    assert_eq!(docs.len(), 3);
    assert_eq!(docs.iter().find(|d| d.get_i64("_id") == Some(1)).and_then(|d| d.get_i64("qty")), Some(10));
    assert_eq!(docs.iter().find(|d| d.get_i64("_id") == Some(2)).and_then(|d| d.get_i64("qty")), Some(102));
    // ordered bulk stops at the duplicate key; unordered continues
    let dup = json(r#"[{"_id": 2}, {"_id": 50}]"#);
    let e = t.request_body(uri, dup.clone()).unwrap_err();
    assert!(e.root_cause().is_a("MongoBulkWriteException"), "{:?}", e);
    assert!(server.documents("shop", "items").iter().all(|d| d.get_i64("_id") != Some(50)));
    let dup2 = json(r#"[{"insertOne": {"document": {"_id": 2}}}, {"insertOne": {"document": {"_id": 51}}}]"#);
    let ex = t.send_with(uri, ExchangePattern::InOut, |ex| {
        ex.set_body(dup2.clone());
        ex.set_header(h::BULK_ORDERED, false);
    });
    assert!(ex.exception.is_some());
    assert!(server.documents("shop", "items").iter().any(|d| d.get_i64("_id") == Some(51)));
}

#[test]
fn scram_sha1_authentication() {
    let mut users = BTreeMap::new();
    users.insert("legacy".to_string(), "pa:ss".to_string());
    let server = MongoServer::start_with(MongoServerConfig { users, ..Default::default() }).unwrap();
    let client = MongoClient::parse(&format!("mongodb://legacy:pa%3Ass@127.0.0.1:{}/?authMechanism=SCRAM-SHA-1&authSource=admin", server.port())).unwrap();
    client.insert_many("d", "c", vec![Document::new().with("a", 1)], true).unwrap();
    assert_eq!(client.count("d", "c", Document::new()).unwrap(), 1);
    let bad = MongoClient::parse(&format!("mongodb://legacy:nope@127.0.0.1:{}/?authMechanism=SCRAM-SHA-1", server.port())).unwrap();
    let e = bad.count("d", "c", Document::new()).unwrap_err();
    assert!(e.kind == "MongoSecurityException" && e.message.contains("SCRAM-SHA-1"), "{:?}", e);
}

#[test]
fn x509_authentication() {
    let tls = Arc::new(cameleer_tls::testing::mtls_server_params(cameleer_tls::ClientAuth::Need).server_config().unwrap());
    let server = MongoServer::start_with(MongoServerConfig { tls: Some(tls), x509_users: vec!["CN=camel-client".into()], ..Default::default() }).unwrap();
    let ctx = CamelContext::new();
    cameleer_datastores::register(&ctx);
    cameleer_tls::testing::bind_mtls_beans(&ctx);
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let cs = format!("mongodb://localhost:{}/?tls=true&authMechanism=MONGODB-X509", server.port());
    let good = format!("mongodb:x?connectionUriString={}&sslContextParameters=#sslMtlsClient&database=d&collection=c&operation=insert", cameleer_core::util::url_encode(&cs));
    t.send_body(&good, r#"{"x509":true}"#).unwrap();
    assert_eq!(server.documents("d", "c").len(), 1);
    assert!(server.commands().contains(&"authenticate".to_string()));
    // explicit user name must match the certificate subject
    let named = format!("mongodb://CN%3Dsomeone@localhost:{}/?tls=true&authMechanism=MONGODB-X509", server.port());
    let wrong = format!("mongodb:x?connectionUriString={}&sslContextParameters=#sslMtlsClient&database=d&collection=c&operation=count", cameleer_core::util::url_encode(&named));
    let e = t.request_body(&wrong, Value::Null).unwrap_err();
    assert!(e.root_cause().is_a("MongoSecurityException"), "{:?}", e);
    ctx.stop().unwrap();
}

#[test]
fn mongodb_srv_connection_string() {
    let server = MongoServer::start().unwrap();
    let dns = DnsServer::start().unwrap();
    dns.add("_mongodb._tcp.cluster0.example.com", DnsRecord::Srv(SrvRecord { priority: 0, weight: 0, port: server.port(), target: "node1.example.com".into() }));
    dns.add("node1.example.com", DnsRecord::A([127, 0, 0, 1]));
    dns.add("cluster0.example.com", DnsRecord::Txt("authSource=admin".into()));
    let cs = format!("mongodb+srv://cluster0.example.com/shop?tls=false&dnsServer={}", dns.address());
    let o = MongoClientOptions::parse(&cs).unwrap();
    assert_eq!(o.hosts, vec![("127.0.0.1".to_string(), server.port())]);
    assert_eq!(o.auth_source.as_deref(), Some("admin"));
    assert_eq!(o.srv_host.as_deref(), Some("cluster0.example.com"));
    let client = MongoClient::new(o);
    client.insert_many("shop", "c", vec![Document::new().with("srv", true)], true).unwrap();
    assert_eq!(server.documents("shop", "c").len(), 1);
    // tls defaults to true for mongodb+srv
    let o = MongoClientOptions::parse(&format!("mongodb+srv://cluster0.example.com/?dnsServer={}", dns.address())).unwrap();
    assert!(o.tls_requested);
    // targets outside the domain and forbidden TXT options are rejected
    dns.add("_mongodb._tcp.evil.example.org", DnsRecord::Srv(SrvRecord { priority: 0, weight: 0, port: 1, target: "attacker.com".into() }));
    assert!(MongoClientOptions::parse(&format!("mongodb+srv://evil.example.org/?dnsServer={}", dns.address())).is_err());
    dns.add("_mongodb._tcp.bad.example.com", DnsRecord::Srv(SrvRecord { priority: 0, weight: 0, port: 1, target: "n.example.com".into() }));
    dns.add("bad.example.com", DnsRecord::Txt("ssl=false".into()));
    assert!(MongoClientOptions::parse(&format!("mongodb+srv://bad.example.com/?dnsServer={}", dns.address())).is_err());
    assert!(MongoClientOptions::parse(&format!("mongodb+srv://missing.example.com/?dnsServer={}", dns.address())).is_err());
    // through the component
    let ctx = CamelContext::new();
    cameleer_datastores::register(&ctx);
    ctx.start().unwrap();
    let uri = format!("mongodb:x?connectionUriString={}&database=shop&collection=c&operation=count", cameleer_core::util::url_encode(&cs));
    assert_eq!(ctx.create_producer_template().request_body(&uri, Value::Null).unwrap(), Value::Int(1));
}

#[test]
fn replica_set_discovery_and_failover() {
    let a = MongoServer::start().unwrap();
    let b = MongoServer::start().unwrap();
    let members = vec![a.address(), b.address()];
    a.set_replica_set("rs0", members.clone(), Some(b.address()));
    b.set_replica_set("rs0", members.clone(), Some(b.address()));
    // seed list names only the secondary: the client follows `primary`
    let client = MongoClient::parse(&format!("mongodb://{}/?replicaSet=rs0", a.address())).unwrap();
    client.insert_many("d", "c", vec![Document::new().with("n", 1)], true).unwrap();
    assert_eq!(b.documents("d", "c").len(), 1);
    assert_eq!(a.documents("d", "c").len(), 0);
    assert_eq!(client.discovered_members().len(), 2);
    // failover: a becomes primary, the pooled connection to b gets NotWritablePrimary
    a.set_replica_set("rs0", members.clone(), Some(a.address()));
    b.set_replica_set("rs0", members.clone(), Some(a.address()));
    client.insert_many("d", "c", vec![Document::new().with("n", 2)], true).unwrap();
    assert_eq!(a.documents("d", "c").len(), 1);
    // wrong replica set name
    let wrong = MongoClient::parse(&format!("mongodb://{}/?replicaSet=other", a.address())).unwrap();
    assert!(wrong.count("d", "c", Document::new()).is_err());
    // directConnection talks to the named member only (writes refused on a secondary)
    let direct = MongoClient::parse(&format!("mongodb://{}/?directConnection=true", b.address())).unwrap();
    assert!(direct.insert_many("d", "c", vec![Document::new()], true).is_err());
}

#[test]
fn gridfs_producer_and_consumer() {
    let server = MongoServer::start().unwrap();
    let ctx = context(&server);
    ctx.add_routes(|r| {
        r.from("mongodb-gridfs:myDb?database=files&bucket=docs&delay=50&initialDelay=0").to("mock:files");
        r.from("mongodb-gridfs:myDb?database=files&bucket=attr&queryStrategy=FileAttribute&delay=50&initialDelay=0").to("mock:attr");
    })
    .unwrap();
    ctx.start().unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let t = ctx.create_producer_template();
    let base = "mongodb-gridfs:myDb?database=files&bucket=docs";
    let data: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
    let ex = t.send_with(&format!("{}&operation=create", base), ExchangePattern::InOut, |ex| {
        ex.set_body(Value::Bytes(data.clone()));
        ex.set_header("CamelFileName", "data.bin");
        ex.set_header("gridfs.chunksize", 300);
        ex.set_header("Content-Type", "application/octet-stream");
        ex.set_header("gridfs.metadata", r#"{"owner":"ann"}"#);
    });
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
    let id = ex.message.headers.get("gridfs.fileid").unwrap().to_string();
    assert_eq!(server.documents("files", "docs.chunks").len(), 4);
    let files = server.documents("files", "docs.files");
    assert_eq!(files[0].get_i64("length"), Some(1000));
    assert_eq!(files[0].get_i64("chunkSize"), Some(300));
    // findOne by name and by id
    let f = t.send_with(&format!("{}&operation=findOne", base), ExchangePattern::InOut, |ex| ex.set_header("CamelFileName", "data.bin"));
    assert_eq!(f.message.body, Value::Bytes(data.clone()));
    assert_eq!(f.message.headers.get("CamelFileLength"), Some(&Value::Int(1000)));
    assert_eq!(f.message.headers.get("Content-Type"), Some(&Value::from("application/octet-stream")));
    assert!(f.message.headers.get("gridfs.metadata").unwrap().to_string().contains("ann"));
    let f2 = t.send_with(&format!("{}&operation=findOne", base), ExchangePattern::InOut, |ex| ex.set_header("gridfs.objectid", id.clone()));
    assert_eq!(f2.message.body, Value::Bytes(data.clone()));
    t.send_with(&format!("{}&operation=create", base), ExchangePattern::InOut, |ex| {
        ex.set_body("hello");
        ex.set_header("CamelFileName", "hello.txt");
    });
    assert_eq!(t.request_body(&format!("{}&operation=count", base), Value::Null).unwrap(), Value::Int(2));
    let list = t.request_body(&format!("{}&operation=listAll", base), Value::Null).unwrap().to_string();
    assert!(list.contains(&format!("data.bin\t{}\n", id)) && list.contains("hello.txt\t"), "{}", list);
    // the consumer received both files
    let m = ctx.mock_endpoint("mock:files").unwrap();
    m.expected_message_count(2);
    m.assert_is_satisfied_within(Duration::from_secs(10)).unwrap();
    let got = m.received_exchanges();
    assert_eq!(got[0].message.headers.get("CamelFileName"), Some(&Value::from("data.bin")));
    assert_eq!(got[1].message.body, Value::Bytes(b"hello".to_vec()));
    // remove
    t.send_with(&format!("{}&operation=remove", base), ExchangePattern::InOut, |ex| ex.set_header("CamelFileName", "data.bin"));
    assert_eq!(t.request_body(&format!("{}&operation=count", base), Value::Null).unwrap(), Value::Int(1));
    assert_eq!(server.documents("files", "docs.chunks").len(), 1);
    // FileAttribute strategy marks processed files
    t.send_with("mongodb-gridfs:myDb?database=files&bucket=attr", ExchangePattern::InOut, |ex| {
        ex.set_body("x");
        ex.set_header("CamelFileName", "a.txt");
    });
    let ma = ctx.mock_endpoint("mock:attr").unwrap();
    ma.expected_message_count(1);
    ma.assert_is_satisfied_within(Duration::from_secs(10)).unwrap();
    let mut done = false;
    for _ in 0..200 {
        let f = server.documents("files", "attr.files");
        if f[0].get_doc("metadata").and_then(|m| m.get_str("camel-processed")) == Some("done") {
            done = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(done);
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(ma.received_counter(), 1);
    ctx.stop().unwrap();
}

/// Regression: the TimeStamp consumer asked for `uploadDate > last seen`, so a file stored
/// in the same millisecond as the last one it routed was skipped forever.
#[test]
fn gridfs_consumer_gets_files_stored_in_the_same_millisecond() {
    use cameleer_datastores::mongodb::bson::{Bson, ObjectId};
    let server = MongoServer::start().unwrap();
    let ctx = context(&server);
    ctx.add_routes(|r| {
        r.from("mongodb-gridfs:myDb?database=files&bucket=burst&delay=20&initialDelay=0").to("mock:burst");
    })
    .unwrap();
    ctx.start().unwrap();
    let client = MongoClient::parse(&server.connection_string()).unwrap();
    // one upload date for both files, after the consumer's start time
    let when = cameleer_core::util_time::now_millis() + 60_000;
    let store = |name: &str| {
        let id = Bson::ObjectId(ObjectId::new());
        let file = Document::new()
            .with("_id", id.clone())
            .with("filename", name)
            .with("length", Bson::Int64(1))
            .with("chunkSize", Bson::Int32(255))
            .with("uploadDate", Bson::DateTime(when));
        let chunk = Document::new().with("files_id", id).with("n", Bson::Int32(0)).with("data", Bson::Binary(0, b"x".to_vec()));
        client.insert_many("files", "burst.chunks", vec![chunk], true).unwrap();
        client.insert_many("files", "burst.files", vec![file], true).unwrap();
    };
    let m = ctx.mock_endpoint("mock:burst").unwrap();
    store("first.txt");
    m.expected_message_count(1);
    m.assert_is_satisfied_within(Duration::from_secs(10)).unwrap();
    store("second.txt");
    m.expected_message_count(2);
    m.assert_is_satisfied_within(Duration::from_secs(10)).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let names: Vec<String> = m.received_exchanges().iter().map(|e| e.message.headers.get("CamelFileName").unwrap().to_string()).collect();
    assert_eq!(names, vec!["first.txt".to_string(), "second.txt".to_string()], "each file exactly once");
    ctx.stop().unwrap();
}

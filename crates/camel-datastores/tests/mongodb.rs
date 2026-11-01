use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use camel_core::prelude::*;
use camel_datastores::mongodb::{headers as h, Bson, Document, MongoClient};
use camel_datastores::testing::{MongoServer, MongoServerConfig};

const WAIT: Duration = Duration::from_secs(10);

fn context(server: &MongoServer) -> CamelContext {
    let ctx = CamelContext::new();
    camel_datastores::register(&ctx);
    ctx.registry().bind("myDb", MongoClient::parse(&server.connection_string()).unwrap());
    ctx
}

fn uri(op: &str) -> String {
    format!("mongodb:myDb?database=shop&collection=items&operation={}", op)
}

fn req(ctx: &CamelContext, op: &str, body: impl Into<Value>, headers: Vec<(&str, Value)>) -> Result<Exchange> {
    let t = ctx.create_producer_template();
    let body = body.into();
    let ex = t.send_with(&uri(op), ExchangePattern::InOut, |ex| {
        ex.set_body(body);
        for (k, v) in headers {
            ex.set_header(k, v);
        }
    });
    match ex.exception.clone() {
        Some(e) => Err(e),
        None => Ok(ex),
    }
}

fn wait_for(f: impl Fn() -> bool) -> bool {
    let end = std::time::Instant::now() + WAIT;
    while std::time::Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    f()
}

fn names(v: &Value) -> Vec<String> {
    v.as_array().unwrap().iter().map(|d| d.get("name").unwrap().to_string()).collect()
}

#[test]
fn crud_operations() {
    let server = MongoServer::start().unwrap();
    let ctx = context(&server);
    ctx.start().unwrap();

    // insert single (JSON text) and many (array)
    let ex = req(&ctx, "insert", r#"{"name":"apple","qty":5,"price":1.5,"tags":["fruit","red"]}"#, vec![]).unwrap();
    let oid = ex.header(h::OID).cloned().unwrap();
    assert!(oid.get("$oid").is_some(), "{:?}", oid);
    assert_eq!(ex.body().get("name"), Some(&Value::from("apple")));
    let many = Value::Array(vec![
        Value::object([("name", Value::from("banana")), ("qty", Value::Int(12)), ("price", Value::Float(0.5)), ("tags", Value::Array(vec!["fruit".into()]))]),
        Value::object([("name", Value::from("carrot")), ("qty", Value::Int(0)), ("price", Value::Float(0.25)), ("tags", Value::Array(vec!["veg".into()]))]),
        Value::object([("_id", Value::from("custom-id")), ("name", Value::from("date")), ("qty", Value::Int(7)), ("price", Value::Float(3.0))]),
    ]);
    let ex = req(&ctx, "insert", many, vec![]).unwrap();
    assert_eq!(ex.header(h::OID).unwrap().as_array().unwrap().len(), 3);
    assert_eq!(server.documents("shop", "items").len(), 4);
    // duplicate key
    let err = req(&ctx, "insert", Value::object([("_id", "custom-id")]), vec![]).unwrap_err();
    assert!(err.is_a("MongoWriteException") && err.is_a("MongoException"), "{:?}", err);
    assert!(err.message.contains("E11000"));

    // findById (ObjectId from the OID header, and a string id)
    let ex = req(&ctx, "findById", oid.clone(), vec![]).unwrap();
    assert_eq!(ex.body().get("name"), Some(&Value::from("apple")));
    assert_eq!(ex.header(h::RESULT_TOTAL_SIZE), Some(&Value::Int(1)));
    let ex = req(&ctx, "findById", "custom-id", vec![(h::FIELDS_PROJECTION, Value::from(r#"{"name":1,"_id":0}"#))]).unwrap();
    assert_eq!(ex.body(), &Value::object([("name", "date")]));
    assert_eq!(req(&ctx, "findById", "nope", vec![]).unwrap().body(), &Value::Null);

    // findOneByQuery with sort
    let ex = req(&ctx, "findOneByQuery", r#"{"tags":"fruit"}"#, vec![(h::SORT_BY, Value::from(r#"{"qty":-1}"#))]).unwrap();
    assert_eq!(ex.body().get("name"), Some(&Value::from("banana")));

    // findAll with criteria header, sort, limit, skip, projection
    let ex = req(&ctx, "findAll", Value::Null, vec![(h::CRITERIA, Value::from(r#"{"qty":{"$gte":5}}"#)), (h::SORT_BY, Value::from(r#"{"name":1}"#))]).unwrap();
    assert_eq!(names(ex.body()), vec!["apple", "banana", "date"]);
    assert_eq!(ex.header(h::RESULT_TOTAL_SIZE), Some(&Value::Int(3)));
    let ex = req(&ctx, "findAll", Value::Null, vec![(h::SORT_BY, Value::from(r#"{"price":-1}"#)), (h::LIMIT, 2.into()), (h::NUM_TO_SKIP, 1.into())]).unwrap();
    assert_eq!(names(ex.body()), vec!["apple", "banana"]);
    assert_eq!(ex.header(h::RESULT_PAGE_SIZE), Some(&Value::Int(2)));
    let ex = req(&ctx, "findAll", r#"{"$or":[{"name":{"$regex":"^c"}},{"price":{"$gt":2}}]}"#, vec![(h::FIELDS_PROJECTION, Value::from(r#"{"name":1}"#)), (h::SORT_BY, Value::from(r#"{"name":1}"#))]).unwrap();
    assert_eq!(names(ex.body()), vec!["carrot", "date"]);
    assert!(ex.body().as_array().unwrap()[0].get("qty").is_none());
    // batching through getMore
    let ex = req(&ctx, "findAll", Value::Null, vec![(h::BATCH_SIZE, 1.into())]).unwrap();
    assert_eq!(ex.body().as_array().unwrap().len(), 4);
    assert!(server.commands().iter().filter(|c| *c == "getMore").count() >= 3);

    // count
    assert_eq!(req(&ctx, "count", r#"{"tags":{"$in":["veg","red"]}}"#, vec![]).unwrap().body(), &Value::Int(2));
    assert_eq!(req(&ctx, "count", Value::Null, vec![]).unwrap().body(), &Value::Int(4));

    // findDistinct
    let ex = req(&ctx, "findDistinct", Value::Null, vec![(h::DISTINCT_QUERY_FIELD, "tags".into())]).unwrap();
    assert_eq!(ex.body(), &Value::Array(vec!["fruit".into(), "red".into(), "veg".into()]));

    // update: [criteria, update] body, multi
    let ex = req(&ctx, "update", r#"[{"tags":"fruit"},{"$inc":{"qty":1},"$set":{"onSale":true}}]"#, vec![(h::MULTI_UPDATE, true.into())]).unwrap();
    assert_eq!(ex.header(h::RECORDS_AFFECTED), Some(&Value::Int(2)));
    assert_eq!(ex.header(h::RECORDS_MATCHED), Some(&Value::Int(2)));
    assert_eq!(ex.body().get("modifiedCount"), Some(&Value::Int(2)));
    // update with criteria header + upsert
    let ex = req(&ctx, "update", r#"{"$set":{"qty":1},"$push":{"tags":"new"}}"#, vec![(h::CRITERIA, Value::from(r#"{"name":"elderberry"}"#)), (h::UPSERT, true.into())]).unwrap();
    assert!(ex.body().get("upsertedId").is_some());
    let ex = req(&ctx, "findOneByQuery", r#"{"name":"elderberry"}"#, vec![]).unwrap();
    assert_eq!(ex.body().get("tags"), Some(&Value::Array(vec!["new".into()])));
    // $unset
    req(&ctx, "update", r#"[{"name":"apple"},{"$unset":{"onSale":""}}]"#, vec![]).unwrap();
    let ex = req(&ctx, "findById", oid.clone(), vec![]).unwrap();
    assert_eq!(ex.body().get("qty"), Some(&Value::Int(6)));
    assert!(ex.body().get("onSale").is_none());
    // invalid update
    let err = req(&ctx, "update", r#"[{"name":"apple"},{"$bogus":{"a":1}}]"#, vec![]).unwrap_err();
    assert!(err.is_a("MongoException"), "{:?}", err);

    // save (replace by _id, insert without)
    let ex = req(&ctx, "save", r#"{"_id":"custom-id","name":"date","qty":70}"#, vec![]).unwrap();
    assert_eq!(ex.body().get("matchedCount"), Some(&Value::Int(1)));
    assert_eq!(ex.header(h::OID), Some(&Value::from("custom-id")));
    let ex = req(&ctx, "findById", "custom-id", vec![]).unwrap();
    assert_eq!(ex.body(), &Value::object([("_id", Value::from("custom-id")), ("name", Value::from("date")), ("qty", Value::Int(70))]));
    let ex = req(&ctx, "save", r#"{"name":"fig"}"#, vec![]).unwrap();
    assert!(ex.header(h::OID).unwrap().get("$oid").is_some());
    assert!(ex.body().get("upsertedId").is_some());

    // aggregate
    let ex = req(&ctx, "aggregate", r#"[{"$match":{"qty":{"$gt":0}}},{"$unwind":"$tags"},{"$group":{"_id":"$tags","n":{"$sum":1},"qty":{"$sum":"$qty"}}},{"$sort":{"_id":1}}]"#, vec![]).unwrap();
    let rows = ex.body().as_array().unwrap();
    assert_eq!(rows[0], Value::object([("_id", Value::from("fruit")), ("n", Value::Int(2)), ("qty", Value::Int(19))]));
    let ex = req(&ctx, "aggregate", r#"[{"$count":"total"}]"#, vec![]).unwrap();
    assert_eq!(ex.body(), &Value::Array(vec![Value::object([("total", 6)])]));

    // remove
    let ex = req(&ctx, "remove", r#"{"qty":{"$lt":2}}"#, vec![]).unwrap();
    assert_eq!(ex.header(h::RECORDS_AFFECTED), Some(&Value::Int(2)));
    assert_eq!(ex.body(), &Value::object([("deletedCount", 2)]));

    // stats and commands
    let ex = req(&ctx, "getDbStats", Value::Null, vec![]).unwrap();
    assert_eq!(ex.body().get("db"), Some(&Value::from("shop")));
    assert_eq!(ex.body().get("objects"), Some(&Value::Int(4)));
    let ex = req(&ctx, "getColStats", Value::Null, vec![]).unwrap();
    assert_eq!(ex.body().get("count"), Some(&Value::Int(4)));
    let ex = req(&ctx, "command", r#"{"ping":1}"#, vec![]).unwrap();
    assert_eq!(ex.body().get("ok"), Some(&Value::Float(1.0)));
    let err = req(&ctx, "command", r#"{"nosuchcmd":1}"#, vec![]).unwrap_err();
    assert!(err.is_a("MongoCommandException"), "{:?}", err);
    assert!(err.message.contains("59"));
    // operation header override
    let ex = req(&ctx, "findAll", Value::Null, vec![(h::OPERATION, "count".into())]).unwrap();
    assert_eq!(ex.body(), &Value::Int(4));
    ctx.stop().unwrap();
}

#[test]
fn dynamic_db_extended_json_and_endpoint_options() {
    let server = MongoServer::start().unwrap();
    let ctx = context(&server);
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let dynamic = "mongodb:myDb?database=a&collection=c&operation=insert&dynamicity=true";
    t.send_body_and_headers(dynamic, r#"{"when":{"$date":"2024-01-02T03:04:05Z"},"price":{"$numberDecimal":"9.99"},"n":NumberLong(5)}"#, vec![(h::DATABASE, "other".into()), (h::COLLECTION, "dyn".into())]).unwrap();
    let docs = server.documents("other", "dyn");
    assert_eq!(docs.len(), 1);
    assert!(matches!(docs[0].get("when"), Some(Bson::DateTime(_))));
    assert!(matches!(docs[0].get("price"), Some(Bson::Decimal128(_))));
    assert_eq!(docs[0].get("n"), Some(&Bson::Int64(5)));
    let found = t.request_body("mongodb:myDb?database=other&collection=dyn&operation=findOneByQuery", Value::Null).unwrap();
    assert_eq!(found.get("price"), Some(&Value::object([("$numberDecimal", "9.99")])));
    assert_eq!(found.get("when").unwrap().get("$date").unwrap().get("$numberLong").unwrap().as_str(), Some("1704164645000"));
    // without dynamicity the headers are ignored
    t.send_body_and_headers("mongodb:myDb?database=a&collection=c&operation=insert", r#"{"x":1}"#, vec![(h::DATABASE, "other".into())]).unwrap();
    assert_eq!(server.documents("a", "c").len(), 1);
    // writeResultAsHeader
    let ex = t.send_with("mongodb:myDb?database=a&collection=c&operation=remove&writeResultAsHeader=true", ExchangePattern::InOut, |ex| ex.set_body(r#"{"x":1}"#));
    assert_eq!(ex.body(), &Value::from(r#"{"x":1}"#));
    assert_eq!(ex.header(h::WRITE_RESULT), Some(&Value::object([("deletedCount", 1)])));
    // connectionUriString
    let cs = format!("mongodb:x?connectionUriString={}/a&collection=c&operation=count", server.connection_string());
    assert_eq!(t.request_body(&cs, Value::Null).unwrap(), Value::Int(0));
    // unknown bean / operation
    assert!(ctx.get_endpoint("mongodb:nope?database=a&collection=c&operation=count").is_err());
    assert!(ctx.get_endpoint("mongodb:myDb?database=a&collection=c&operation=explode").is_err());
    ctx.stop().unwrap();
}

#[test]
fn create_collection_false() {
    let server = MongoServer::start().unwrap();
    let ctx = context(&server);
    ctx.add_routes(|r| {
        r.from("direct:x").to("mongodb:myDb?database=shop&collection=missing&operation=findAll&createCollection=false");
    })
    .unwrap();
    let e = ctx.start().unwrap_err();
    assert!(format!("{:?}", e).contains("createCollection is false"), "{:?}", e);
}

#[test]
fn scram_authentication() {
    let mut users = BTreeMap::new();
    users.insert("admin".to_string(), "s3cret".to_string());
    let server = MongoServer::start_with(MongoServerConfig { users, ..Default::default() }).unwrap();
    let ctx = CamelContext::new();
    camel_datastores::register(&ctx);
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let good = format!("mongodb:x?connectionUriString=mongodb://admin:s3cret@127.0.0.1:{}/?authSource=admin&database=d&collection=c&operation=insert", server.port());
    t.send_body(&good, r#"{"a":1}"#).unwrap();
    assert_eq!(server.documents("d", "c").len(), 1);
    assert!(server.commands().contains(&"saslContinue".to_string()));
    // hosts/username/password options
    let opts = format!("mongodb:x?hosts=127.0.0.1:{}&username=admin&password=s3cret&authSource=admin&database=d&collection=c&operation=count", server.port());
    assert_eq!(t.request_body(&opts, Value::Null).unwrap(), Value::Int(1));
    // wrong password
    let bad = format!("mongodb:x?connectionUriString=mongodb://admin:wrong@127.0.0.1:{}/&database=d&collection=c&operation=count", server.port());
    let err = t.request_body(&bad, Value::Null).unwrap_err();
    assert!(err.root_cause().is_a("MongoSecurityException"), "{:?}", err);
    // no credentials
    let none = format!("mongodb:x?connectionUriString=mongodb://127.0.0.1:{}&database=d&collection=c&operation=count", server.port());
    let err = t.request_body(&none, Value::Null).unwrap_err();
    assert!(err.root_cause().message.contains("requires authentication"), "{:?}", err);
    ctx.stop().unwrap();
}

#[test]
fn tls_connection() {
    let tls = Arc::new(camel_tls::testing::server_params().server_config().unwrap());
    let server = MongoServer::start_with(MongoServerConfig { tls: Some(tls), ..Default::default() }).unwrap();
    let ctx = CamelContext::new();
    camel_datastores::register(&ctx);
    camel_tls::testing::bind_test_beans(&ctx);
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let base = format!("mongodb://localhost:{}/?tls=true", server.port());
    let good = format!("mongodb:x?connectionUriString={}&sslContextParameters=#ssl&database=d&collection=c&operation=insert", camel_core::util::url_encode(&base));
    t.send_body(&good, r#"{"secure":true}"#).unwrap();
    assert_eq!(server.documents("d", "c").len(), 1);
    let bad = format!("mongodb:x?connectionUriString={}&sslContextParameters=#sslUntrusted&database=d&collection=c&operation=count", camel_core::util::url_encode(&base));
    let err = t.request_body(&bad, Value::Null).unwrap_err();
    assert!(err.root_cause().is_a("IOException"), "{:?}", err);
    let insecure = format!("mongodb://localhost:{}/?tls=true&tlsInsecure=true", server.port());
    let ok = format!("mongodb:x?connectionUriString={}&sslContextParameters=#sslUntrusted&database=d&collection=c&operation=count", camel_core::util::url_encode(&insecure));
    assert_eq!(t.request_body(&ok, Value::Null).unwrap(), Value::Int(1));
    ctx.stop().unwrap();
}

#[test]
fn tailable_consumer() {
    let server = MongoServer::start().unwrap();
    let client = MongoClient::parse(&server.connection_string()).unwrap();
    client.create_collection("logs", "events", Some((1_000_000, Some(100)))).unwrap();
    client.insert_many("logs", "events", vec![Document::new().with("seq", 1).with("msg", "before")], true).unwrap();
    let ctx = context(&server);
    ctx.add_routes(|r| {
        r.from("mongodb:myDb?database=logs&collection=events&consumerType=tailable&tailTrackIncreasingField=seq&cursorRegenerationDelay=100").to("mock:tail");
    })
    .unwrap();
    let mock = ctx.mock_endpoint("mock:tail").unwrap();
    ctx.start().unwrap();
    mock.reset();
    mock.expected_message_count(4);
    let t = ctx.create_producer_template();
    for i in 2..5 {
        t.send_body("mongodb:myDb?database=logs&collection=events&operation=insert", format!(r#"{{"seq":{},"msg":"m{}"}}"#, i, i)).unwrap();
    }
    mock.assert_is_satisfied_within(WAIT).unwrap();
    let seqs: Vec<i64> = mock.received_bodies().iter().map(|b| b.get("seq").unwrap().as_i64().unwrap()).collect();
    assert_eq!(seqs, vec![1, 2, 3, 4]);
    ctx.stop().unwrap();
    // tailable cursors need a capped collection
    let err = client.find("logs", "plain", Document::new(), &camel_datastores::mongodb::FindOptions { tailable: true, ..Default::default() }).unwrap_err();
    assert!(err.message.contains("non capped"), "{}", err);
}

#[test]
fn change_stream_consumer() {
    let server = MongoServer::start().unwrap();
    let ctx = context(&server);
    ctx.add_routes(|r| {
        r.from("mongodb:myDb?database=shop&collection=orders&consumerType=changeStreams").to("mock:all");
        r.from("mongodb:myDb?database=shop&collection=orders&consumerType=changeStreams&fullDocument=updateLookup&streamFilter={\"operationType\":{\"$in\":[\"update\",\"delete\"]}}").to("mock:filtered");
    })
    .unwrap();
    ctx.start().unwrap();
    let all = ctx.mock_endpoint("mock:all").unwrap();
    all.expected_message_count(3);
    let filtered = ctx.mock_endpoint("mock:filtered").unwrap();
    filtered.expected_message_count(2);
    let t = ctx.create_producer_template();
    let base = "mongodb:myDb?database=shop&collection=orders&operation=";
    t.send_body(&format!("{}insert", base), r#"{"_id":1,"status":"new"}"#).unwrap();
    t.send_body(&format!("{}update", base), r#"[{"_id":1},{"$set":{"status":"paid"}}]"#).unwrap();
    // the lookup happens when the event is read: wait before deleting the document
    assert!(wait_for(|| filtered.received_counter() == 1));
    t.send_body(&format!("{}remove", base), r#"{"_id":1}"#).unwrap();
    all.assert_is_satisfied_within(WAIT).unwrap();
    filtered.assert_is_satisfied_within(WAIT).unwrap();
    let ops: Vec<String> = all.received_exchanges().iter().map(|e| e.header(h::STREAM_OPERATION_TYPE).unwrap().to_string()).collect();
    assert_eq!(ops, vec!["insert", "update", "delete"]);
    let first = &all.received_exchanges()[0];
    assert_eq!(first.body(), &Value::object([("_id", Value::Int(1)), ("status", Value::from("new"))]));
    assert_eq!(first.header(h::MONGO_ID), Some(&Value::Int(1)));
    let f = filtered.received_exchanges();
    assert_eq!(f[0].header(h::STREAM_OPERATION_TYPE), Some(&Value::from("update")));
    assert_eq!(f[0].body().get("status"), Some(&Value::from("paid")), "updateLookup gives the full document");
    assert_eq!(f[1].body(), &Value::Null);
    ctx.stop().unwrap();
}

#[test]
fn legacy_op_query_handshake() {
    use camel_datastores::mongodb::wire;
    use std::io::Write;
    let server = MongoServer::start().unwrap();
    let mut s = std::net::TcpStream::connect(("127.0.0.1", server.port())).unwrap();
    s.write_all(&wire::encode_op_query(1, "admin.$cmd", &Document::new().with("isMaster", 1))).unwrap();
    let (hdr, body) = wire::read_message(&mut s).unwrap();
    assert_eq!((hdr.op_code, hdr.response_to), (wire::OP_REPLY, 1));
    let docs = wire::decode_op_reply(&body).unwrap();
    assert_eq!(docs[0].get("ismaster"), Some(&Bson::Boolean(true)));
    assert_eq!(docs[0].get_f64("ok"), Some(1.0));
}

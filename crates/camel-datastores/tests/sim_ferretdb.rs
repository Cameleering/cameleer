//! Third-party simulator: the `mongodb` and `mongodb-gridfs` components against FerretDB 1.24
//! (a MongoDB wire-protocol implementation on PostgreSQL; `scripts/simulators.sh`: port 27117,
//! TLS on 27118, backed by the live-interop PostgreSQL 16, SCRAM user `camel` / `camelpw` in
//! `admin`). Runs only with `CAMEL_SIMULATORS=1`.

use std::time::Duration;

use camel_core::prelude::*;
use camel_datastores::mongodb::{headers as h, MongoClient};
use camel_tls::testing::{live, sim};

const URI: &str = "mongodb://camel:camelpw@127.0.0.1:27117/?authSource=admin";
const WAIT: Duration = Duration::from_secs(20);

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    camel_datastores::register(&ctx);
    live::bind_beans(&ctx);
    ctx.registry().bind("ferret", MongoClient::parse(URI).unwrap());
    ctx
}

fn db() -> String {
    sim::unique("shop").replace('-', "_")
}

fn req(ctx: &CamelContext, uri: &str, body: impl Into<Value>, headers: Vec<(&str, Value)>) -> Result<Exchange> {
    let body = body.into();
    let ex = ctx.create_producer_template().send_with(uri, ExchangePattern::InOut, |ex| {
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

fn names(v: &Value) -> Vec<String> {
    v.as_array().unwrap().iter().map(|d| d.get("name").unwrap().to_string()).collect()
}

fn json(s: &str) -> Value {
    camel_core::json::parse(s).unwrap()
}

#[test]
fn sim_ferretdb_crud_queries_updates_aggregation() {
    if !sim::enabled() {
        return;
    }
    let ctx = context();
    ctx.start().unwrap();
    let d = db();
    let u = |op: &str| format!("mongodb:ferret?database={}&collection=items&operation={}", d, op);
    let r = |op: &str, body: Value, headers: Vec<(&str, Value)>| req(&ctx, &u(op), body, headers).unwrap_or_else(|e| panic!("{}: {}", op, e));

    let ex = r("insert", Value::from(r#"{"name":"apple","qty":5,"price":1.5,"tags":["fruit","red"]}"#), vec![]);
    let oid = ex.header(h::OID).cloned().unwrap();
    assert!(oid.get("$oid").is_some(), "{:?}", oid);
    let many = Value::Array(vec![
        Value::object([("name", Value::from("banana")), ("qty", Value::Int(12)), ("price", Value::Float(0.5)), ("tags", Value::Array(vec!["fruit".into()]))]),
        Value::object([("name", Value::from("carrot")), ("qty", Value::Int(0)), ("price", Value::Float(0.25)), ("tags", Value::Array(vec!["veg".into()]))]),
        Value::object([("_id", Value::from("custom-id")), ("name", Value::from("date")), ("qty", Value::Int(7)), ("price", Value::Float(3.0))]),
    ]);
    assert_eq!(r("insert", many, vec![]).header(h::OID).unwrap().as_array().unwrap().len(), 3);
    // duplicate _id as reported by FerretDB
    let err = req(&ctx, &u("insert"), Value::object([("_id", "custom-id")]), vec![]).unwrap_err();
    assert!(err.is_a("MongoWriteException") && err.message.contains("E11000"), "{:?}", err);

    assert_eq!(r("findById", oid.clone(), vec![]).body().get("name"), Some(&Value::from("apple")));
    assert_eq!(r("findById", Value::from("custom-id"), vec![(h::FIELDS_PROJECTION, Value::from(r#"{"name":1,"_id":0}"#))]).body(), &Value::object([("name", "date")]));
    assert_eq!(r("findById", Value::from("nope"), vec![]).body(), &Value::Null);
    assert_eq!(r("findOneByQuery", Value::from(r#"{"tags":"fruit"}"#), vec![(h::SORT_BY, Value::from(r#"{"qty":-1}"#))]).body().get("name"), Some(&Value::from("banana")));
    let ex = r("findAll", Value::Null, vec![(h::CRITERIA, Value::from(r#"{"qty":{"$gte":5}}"#)), (h::SORT_BY, Value::from(r#"{"name":1}"#))]);
    assert_eq!(names(ex.body()), vec!["apple", "banana", "date"]);
    let ex = r("findAll", Value::Null, vec![(h::SORT_BY, Value::from(r#"{"price":-1}"#)), (h::LIMIT, 2.into()), (h::NUM_TO_SKIP, 1.into())]);
    assert_eq!(names(ex.body()), vec!["apple", "banana"]);
    let ex = r("findAll", Value::from(r#"{"$or":[{"name":{"$regex":"^c"}},{"price":{"$gt":2}}]}"#), vec![(h::FIELDS_PROJECTION, Value::from(r#"{"name":1}"#)), (h::SORT_BY, Value::from(r#"{"name":1}"#))]);
    assert_eq!(names(ex.body()), vec!["carrot", "date"]);
    assert!(ex.body().as_array().unwrap()[0].get("qty").is_none());
    // cursors with getMore
    assert_eq!(r("findAll", Value::Null, vec![(h::BATCH_SIZE, 1.into())]).body().as_array().unwrap().len(), 4);
    assert_eq!(r("count", Value::from(r#"{"tags":{"$in":["veg","red"]}}"#), vec![]).body(), &Value::Int(2));
    assert_eq!(r("count", Value::Null, vec![]).body(), &Value::Int(4));
    let mut distinct: Vec<String> = r("findDistinct", Value::Null, vec![(h::DISTINCT_QUERY_FIELD, "tags".into())]).body().as_array().unwrap().iter().map(|v| v.to_string()).collect();
    distinct.sort();
    assert_eq!(distinct, vec!["fruit", "red", "veg"]);
    // updates
    let ex = r("update", Value::from(r#"[{"tags":"fruit"},{"$inc":{"qty":1},"$set":{"onSale":true}}]"#), vec![(h::MULTI_UPDATE, true.into())]);
    assert_eq!(ex.header(h::RECORDS_AFFECTED), Some(&Value::Int(2)));
    assert_eq!(ex.header(h::RECORDS_MATCHED), Some(&Value::Int(2)));
    let ex = r("update", Value::from(r#"{"$set":{"qty":1},"$push":{"tags":"new"}}"#), vec![(h::CRITERIA, Value::from(r#"{"name":"elderberry"}"#)), (h::UPSERT, true.into())]);
    assert!(ex.body().get("upsertedId").is_some(), "{:?}", ex.body());
    assert_eq!(r("findOneByQuery", Value::from(r#"{"name":"elderberry"}"#), vec![]).body().get("tags"), Some(&Value::Array(vec!["new".into()])));
    r("update", Value::from(r#"[{"name":"apple"},{"$unset":{"onSale":""}}]"#), vec![]);
    let apple = r("findById", oid.clone(), vec![]);
    assert_eq!(apple.body().get("qty"), Some(&Value::Int(6)));
    assert!(apple.body().get("onSale").is_none());
    let err = req(&ctx, &u("update"), Value::from(r#"[{"name":"apple"},{"$bogus":{"a":1}}]"#), vec![]).unwrap_err();
    assert!(err.is_a("MongoException"), "{:?}", err);
    // save
    assert_eq!(r("save", Value::from(r#"{"_id":"custom-id","name":"date","qty":70}"#), vec![]).body().get("matchedCount"), Some(&Value::Int(1)));
    assert_eq!(r("findById", Value::from("custom-id"), vec![]).body().get("qty"), Some(&Value::Int(70)));
    // aggregation pipeline
    // (FerretDB 1.24 sums field paths to 0 after $unwind - the same with pymongo - so the
    // sum is checked on a pipeline without $unwind)
    let ex = r("aggregate", Value::from(r#"[{"$match":{"qty":{"$gt":0}}},{"$unwind":"$tags"},{"$group":{"_id":"$tags","n":{"$sum":1}}},{"$sort":{"_id":1}}]"#), vec![]);
    assert_eq!(ex.body().as_array().unwrap()[0], Value::object([("_id", Value::from("fruit")), ("n", Value::Int(2))]));
    let ex = r("aggregate", Value::from(r#"[{"$match":{"tags":"fruit"}},{"$group":{"_id":null,"qty":{"$sum":"$qty"}}}]"#), vec![]);
    assert_eq!(ex.body().as_array().unwrap()[0], Value::object([("_id", Value::Null), ("qty", Value::Int(19))]));
    assert_eq!(r("aggregate", Value::from(r#"[{"$count":"total"}]"#), vec![]).body(), &Value::Array(vec![Value::object([("total", 5)])]));
    // remove, stats, commands
    let ex = r("remove", Value::from(r#"{"qty":{"$lt":2}}"#), vec![]);
    assert_eq!(ex.header(h::RECORDS_AFFECTED), Some(&Value::Int(2)));
    assert_eq!(r("getDbStats", Value::Null, vec![]).body().get("db"), Some(&Value::from(d.clone())));
    assert_eq!(r("getColStats", Value::Null, vec![]).body().get("count").and_then(|v| v.as_i64()), Some(3));
    assert!(r("command", Value::from(r#"{"ping":1}"#), vec![]).body().get("ok").is_some());
    let err = req(&ctx, &u("command"), Value::from(r#"{"nosuchcmd":1}"#), vec![]).unwrap_err();
    assert!(err.is_a("MongoCommandException"), "{:?}", err);
    // Extended JSON types round trip through FerretDB
    let x = format!("mongodb:ferret?database={}&collection=types&operation=insert", d);
    req(&ctx, &x, r#"{"_id":1,"when":{"$date":"2024-01-02T03:04:05Z"},"n":NumberLong(5),"bin":{"$binary":{"base64":"AAEC","subType":"00"}}}"#, vec![]).unwrap();
    let found = req(&ctx, &format!("mongodb:ferret?database={}&collection=types&operation=findById", d), 1, vec![]).unwrap();
    assert_eq!(found.body().get("when").unwrap().get("$date").unwrap().get("$numberLong").unwrap().as_str(), Some("1704164645000"));
    assert_eq!(found.body().get("n"), Some(&Value::Int(5)));
    ctx.stop().unwrap();
}

#[test]
fn sim_ferretdb_bulk_write_and_dynamic_collections() {
    if !sim::enabled() {
        return;
    }
    let ctx = context();
    ctx.start().unwrap();
    let d = db();
    let uri = format!("mongodb:ferret?database={}&collection=items&operation=bulkWrite", d);
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
    let r = req(&ctx, &uri, body, vec![]).unwrap();
    let r = r.body();
    assert_eq!(r.get("insertedCount"), Some(&Value::Int(3)));
    assert_eq!(r.get("matchedCount"), Some(&Value::Int(3)));
    assert_eq!(r.get("modifiedCount"), Some(&Value::Int(3)));
    assert_eq!(r.get("deletedCount"), Some(&Value::Int(1)));
    assert_eq!(r.get("upsertedCount"), Some(&Value::Int(1)));
    let all = req(&ctx, &format!("mongodb:ferret?database={}&collection=items&operation=findAll", d), Value::Null, vec![(h::SORT_BY, Value::from(r#"{"_id":1}"#))]).unwrap();
    let qty: Vec<Option<i64>> = all.body().as_array().unwrap().iter().map(|d| d.get("qty").and_then(|q| q.as_i64())).collect();
    assert_eq!(qty, vec![Some(10), Some(102), None]);
    // ordered bulk stops at the duplicate, unordered continues
    let e = req(&ctx, &uri, json(r#"[{"_id": 2}, {"_id": 50}]"#), vec![]).unwrap_err();
    assert!(e.root_cause().is_a("MongoBulkWriteException"), "{:?}", e);
    assert_eq!(req(&ctx, &format!("mongodb:ferret?database={}&collection=items&operation=count", d), r#"{"_id":50}"#, vec![]).unwrap().body(), &Value::Int(0));
    let e = req(&ctx, &uri, json(r#"[{"insertOne": {"document": {"_id": 2}}}, {"insertOne": {"document": {"_id": 51}}}]"#), vec![(h::BULK_ORDERED, false.into())]);
    assert!(e.is_err());
    assert_eq!(req(&ctx, &format!("mongodb:ferret?database={}&collection=items&operation=count", d), r#"{"_id":51}"#, vec![]).unwrap().body(), &Value::Int(1));
    // dynamicity
    let dynamic = format!("mongodb:ferret?database={}&collection=c&operation=insert&dynamicity=true", d);
    req(&ctx, &dynamic, r#"{"x":1}"#, vec![(h::COLLECTION, "dyn".into())]).unwrap();
    assert_eq!(req(&ctx, &format!("mongodb:ferret?database={}&collection=dyn&operation=count", d), Value::Null, vec![]).unwrap().body(), &Value::Int(1));
    // createCollection=false on a missing collection
    let ctx2 = context();
    let d2 = d.clone();
    ctx2.add_routes(move |r| {
        r.from("direct:x").to(&format!("mongodb:ferret?database={}&collection=missing&operation=findAll&createCollection=false", d2));
    })
    .unwrap();
    assert!(ctx2.start().is_err());
    ctx.stop().unwrap();
}

#[test]
fn sim_ferretdb_authentication_and_tls() {
    if !sim::enabled() {
        return;
    }
    let ctx = context();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let d = db();
    for mech in ["SCRAM-SHA-256", "SCRAM-SHA-1"] {
        let uri = format!("mongodb:x?connectionUriString={}&database={}&collection=c&operation=insert", camel_core::util::url_encode(&format!("mongodb://camel:camelpw@127.0.0.1:27117/?authSource=admin&authMechanism={}", mech)), d);
        t.send_body(&uri, format!(r#"{{"mech":"{}"}}"#, mech)).unwrap_or_else(|e| panic!("{}: {}", mech, e));
    }
    let opts = format!("mongodb:x?hosts=127.0.0.1:27117&username=camel&password=camelpw&authSource=admin&database={}&collection=c&operation=count", d);
    assert_eq!(t.request_body(&opts, Value::Null).unwrap(), Value::Int(2));
    let bad = format!("mongodb:x?connectionUriString={}&database={}&collection=c&operation=count", camel_core::util::url_encode("mongodb://camel:wrong@127.0.0.1:27117/?authSource=admin"), d);
    let err = t.request_body(&bad, Value::Null).unwrap_err();
    assert!(err.root_cause().is_a("MongoSecurityException"), "{:?}", err);
    // TLS: verified against the interop CA by host name; untrusted CA refused
    let base = "mongodb://camel:camelpw@localhost:27118/?authSource=admin&tls=true";
    let good = format!("mongodb:x?connectionUriString={}&sslContextParameters=#liveSsl&database={}&collection=c&operation=insert", camel_core::util::url_encode(base), d);
    t.send_body(&good, r#"{"secure":true}"#).unwrap();
    let count = format!("mongodb:x?connectionUriString={}&sslContextParameters=#liveSsl&database={}&collection=c&operation=count", camel_core::util::url_encode(base), d);
    assert_eq!(t.request_body(&count, Value::Null).unwrap(), Value::Int(3));
    let untrusted = format!("mongodb:x?connectionUriString={}&sslContextParameters=#liveUntrusted&database={}&collection=c&operation=count", camel_core::util::url_encode(base), d);
    let err = t.request_body(&untrusted, Value::Null).unwrap_err();
    assert!(err.root_cause().is_a("IOException"), "{:?}", err);
    ctx.stop().unwrap();
}

#[test]
fn sim_ferretdb_tailable_consumer_on_capped_collection() {
    if !sim::enabled() {
        return;
    }
    let d = db();
    let client = MongoClient::parse(URI).unwrap();
    client.create_collection(&d, "events", Some((1_000_000, Some(100)))).unwrap();
    client.insert_many(&d, "events", vec![camel_datastores::mongodb::Document::new().with("seq", 1).with("msg", "before")], true).unwrap();
    let ctx = context();
    let d2 = d.clone();
    ctx.add_routes(move |r| {
        r.from(&format!("mongodb:ferret?database={}&collection=events&consumerType=tailable&tailTrackIncreasingField=seq&cursorRegenerationDelay=200", d2)).to("mock:tail");
    })
    .unwrap();
    let mock = ctx.mock_endpoint("mock:tail").unwrap();
    mock.expected_message_count(4);
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for i in 2..5 {
        t.send_body(&format!("mongodb:ferret?database={}&collection=events&operation=insert", d), format!(r#"{{"seq":{},"msg":"m{}"}}"#, i, i)).unwrap();
    }
    mock.assert_is_satisfied_within(WAIT).unwrap();
    let seqs: Vec<i64> = mock.received_bodies().iter().map(|b| b.get("seq").unwrap().as_i64().unwrap()).collect();
    assert_eq!(seqs, vec![1, 2, 3, 4]);
    ctx.stop().unwrap();
}

#[test]
fn sim_ferretdb_gridfs_producer_and_consumer() {
    if !sim::enabled() {
        return;
    }
    let d = db();
    let ctx = context();
    let d2 = d.clone();
    ctx.add_routes(move |r| {
        r.from(&format!("mongodb-gridfs:ferret?database={}&bucket=docs&delay=100&initialDelay=0", d2)).to("mock:files");
    })
    .unwrap();
    let files = ctx.mock_endpoint("mock:files").unwrap();
    files.expected_message_count(1);
    ctx.start().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let t = ctx.create_producer_template();
    let data: Vec<u8> = (0..600_000u32).map(|i| (i % 249) as u8).collect();
    let base = format!("mongodb-gridfs:ferret?database={}&bucket=docs", d);
    t.send_body_and_header(&format!("{}&operation=create", base), Value::Bytes(data.clone()), "CamelFileName", "big.bin").unwrap();
    files.assert_is_satisfied_within(WAIT).unwrap();
    assert_eq!(files.received_exchanges()[0].body_as::<Vec<u8>>().unwrap(), data);
    let got = t.request_body_and_header(&format!("{}&operation=findOne", base), "", "CamelFileName", "big.bin").unwrap();
    assert_eq!(got, Value::Bytes(data));
    assert_eq!(t.request_body(&format!("{}&operation=count", base), Value::Null).unwrap(), Value::Int(1));
    assert!(t.request_body(&format!("{}&operation=listAll", base), Value::Null).unwrap().to_string().contains("big.bin\t"));
    t.request_body_and_header(&format!("{}&operation=remove", base), "", "CamelFileName", "big.bin").unwrap();
    assert_eq!(t.request_body(&format!("{}&operation=count", base), Value::Null).unwrap(), Value::Int(0));
    ctx.stop().unwrap();
}

use std::time::Duration;

use camel_core::prelude::*;
use camel_datastores::couchdb::headers as h;
use camel_datastores::testing::{CouchDbServer, CouchDbServerConfig};

const WAIT: Duration = Duration::from_secs(10);

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    camel_datastores::register(&ctx);
    ctx
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

fn send(ctx: &CamelContext, uri: &str, body: impl Into<Value>, headers: Vec<(&str, Value)>) -> Result<Exchange> {
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

#[test]
fn producer_save_get_delete() {
    let server = CouchDbServer::start().unwrap();
    let ctx = context();
    ctx.start().unwrap();
    let uri = format!("couchdb:{}/inventory?createDatabase=true", server.url());

    // save without _id (POST) and with _id (PUT)
    let ex = send(&ctx, &uri, r#"{"item":"apple","qty":3}"#, vec![]).unwrap();
    let generated = ex.header(h::ID).unwrap().to_string();
    assert!(!generated.is_empty());
    assert!(ex.header(h::REV).unwrap().to_string().starts_with("1-"));
    assert!(server.has_database("inventory"));
    let ex = send(&ctx, &uri, Value::object([("_id", Value::from("pear")), ("qty", Value::Int(1))]), vec![]).unwrap();
    assert_eq!(ex.header(h::ID), Some(&Value::from("pear")));
    let rev1 = ex.header(h::REV).unwrap().to_string();

    // update with the current revision, conflict with a stale one
    let ex = send(&ctx, &uri, Value::object([("_id", Value::from("pear")), ("_rev", Value::from(rev1.as_str())), ("qty", Value::Int(2))]), vec![]).unwrap();
    let rev2 = ex.header(h::REV).unwrap().to_string();
    assert!(rev2.starts_with("2-"));
    let err = send(&ctx, &uri, Value::object([("_id", Value::from("pear")), ("_rev", Value::from(rev1.as_str())), ("qty", Value::Int(9))]), vec![]).unwrap_err();
    assert!(err.is_a("DocumentConflictException") && err.is_a("CouchDbException"), "{:?}", err);

    // get
    let ex = send(&ctx, &uri, Value::Null, vec![(h::METHOD, "GET".into()), (h::ID, "pear".into())]).unwrap();
    assert_eq!(ex.body().get("qty"), Some(&Value::Int(2)));
    assert_eq!(ex.body().get("_rev"), Some(&Value::from(rev2.as_str())));
    let err = send(&ctx, &uri, Value::Null, vec![(h::METHOD, "GET".into()), (h::ID, "nope".into())]).unwrap_err();
    assert!(err.is_a("NoDocumentException"), "{:?}", err);

    // delete with the document as body, then via headers
    let doc = ex.body().clone();
    send(&ctx, &uri, doc, vec![(h::METHOD, "DELETE".into())]).unwrap();
    assert!(server.document("inventory", "pear").is_none());
    let rev = server.document("inventory", &generated).unwrap().get("_rev").unwrap().to_string();
    send(&ctx, &uri, Value::Null, vec![(h::METHOD, "DELETE".into()), (h::ID, generated.clone().into()), (h::REV, rev.into())]).unwrap();
    assert_eq!(server.doc_count("inventory"), 0);
    // invalid body / uri
    assert!(send(&ctx, &uri, "[1,2]", vec![]).is_err());
    assert!(ctx.get_endpoint(&format!("couchdb:{}", server.url())).is_err());
    assert!(ctx.get_endpoint("couchdb:ftp://x/db").is_err());
    ctx.stop().unwrap();
}

#[test]
fn changes_feed_consumer() {
    let server = CouchDbServer::start().unwrap();
    let ctx = context();
    let base = format!("couchdb:{}/events?createDatabase=true", server.url());
    // a document created before the consumer starts is not delivered
    ctx.start().unwrap();
    send(&ctx, &base, r#"{"_id":"old","v":0}"#, vec![]).unwrap();
    ctx.add_routes(|r| {
        r.from(&format!("couchdb:{}/events?heartbeat=200", server.url())).to("mock:changes");
        r.from(&format!("couchdb:{}/events?heartbeat=200&deletes=false", server.url())).to("mock:updates");
    })
    .unwrap();
    let all = ctx.mock_endpoint("mock:changes").unwrap();
    all.expected_message_count(3);
    let updates = ctx.mock_endpoint("mock:updates").unwrap();
    updates.expected_message_count(2);
    // like CouchDB, the feed only reports the latest revision of a document: wait for each
    // change to be consumed before making the next one
    let ex = send(&ctx, &base, r#"{"_id":"a","v":1}"#, vec![]).unwrap();
    assert!(wait_for(|| all.received_counter() == 1 && updates.received_counter() == 1));
    let rev = ex.header(h::REV).unwrap().to_string();
    send(&ctx, &base, Value::object([("_id", Value::from("a")), ("_rev", Value::from(rev.as_str())), ("v", Value::Int(2))]), vec![]).unwrap();
    assert!(wait_for(|| all.received_counter() == 2 && updates.received_counter() == 2));
    let doc = server.document("events", "a").unwrap();
    send(&ctx, &base, doc, vec![(h::METHOD, "DELETE".into())]).unwrap();
    all.assert_is_satisfied_within(WAIT).unwrap();
    updates.assert_is_satisfied_within(WAIT).unwrap();
    let ex = all.received_exchanges();
    let methods: Vec<String> = ex.iter().map(|e| e.header(h::METHOD).unwrap().to_string()).collect();
    assert_eq!(methods, vec!["UPDATE", "UPDATE", "DELETE"]);
    assert_eq!(ex[0].body().get("v"), Some(&Value::Int(1)));
    assert_eq!(ex[1].body().get("v"), Some(&Value::Int(2)));
    assert_eq!(ex[0].header(h::DATABASE), Some(&Value::from("events")));
    assert_eq!(ex[0].header(h::ID), Some(&Value::from("a")));
    assert!(ex[0].header(h::SEQ).is_some());
    assert_eq!(ex[2].body().get("_deleted"), Some(&Value::Bool(true)));
    assert!(ex.iter().all(|e| e.header(h::ID) != Some(&Value::from("old"))));
    ctx.stop().unwrap();
}

#[test]
fn basic_authentication() {
    let server = CouchDbServer::start_with(CouchDbServerConfig { credentials: Some(("admin".into(), "pw".into())) }).unwrap();
    let ctx = context();
    ctx.start().unwrap();
    let ok = format!("couchdb:{}/secure?createDatabase=true&username=admin&password=pw", server.url());
    send(&ctx, &ok, r#"{"_id":"x"}"#, vec![]).unwrap();
    assert!(server.document("secure", "x").is_some());
    let bad = format!("couchdb:{}/secure?username=admin&password=wrong", server.url());
    let err = send(&ctx, &bad, r#"{"_id":"y"}"#, vec![]).unwrap_err();
    assert!(err.is_a("CouchDbException") && err.message.contains("401"), "{:?}", err);
    ctx.stop().unwrap();
}

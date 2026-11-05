//! CouchDB bulk saves and maxMessagesPerPoll.

use std::time::Duration;

use cameleer_core::prelude::*;
use cameleer_datastores::couchdb::headers as h;
use cameleer_datastores::testing::CouchDbServer;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_datastores::register(&ctx);
    ctx
}

#[test]
fn bulk_save_and_limited_changes_feed() {
    let server = CouchDbServer::start().unwrap();
    let ctx = context();
    let uri = format!("couchdb:{}/bulk?createDatabase=true", server.url());
    let consumer = format!("couchdb:{}/bulk?createDatabase=true&maxMessagesPerPoll=2&heartbeat=50", server.url());
    ctx.add_routes(move |b| {
        b.from(&consumer).to("mock:changes");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let docs = Value::Array((0..5).map(|i| Value::object([("_id", Value::from(format!("d{}", i))), ("n", Value::Int(i))])).collect());
    let ex = t.send_with(&uri, ExchangePattern::InOut, |ex| ex.set_body(docs.clone()));
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
    let res = ex.message.body.as_array().unwrap().clone();
    assert_eq!(res.len(), 5);
    assert!(res.iter().all(|r| r.get("ok") == Some(&Value::Bool(true))));
    assert_eq!(ex.message.headers.get(h::ID).and_then(|i| i.as_array()).map(|a| a.len()), Some(5));
    // saving again without revisions conflicts per document
    let again = t.request_body(&uri, Value::Array(vec![Value::object([("_id", "d0")]), Value::object([("_id", "new")])])).unwrap();
    let again = again.as_array().unwrap();
    assert_eq!(again[0].get("error"), Some(&Value::from("conflict")));
    assert_eq!(again[1].get("ok"), Some(&Value::Bool(true)));
    let m = ctx.mock_endpoint("mock:changes").unwrap();
    m.expected_message_count(6);
    m.assert_is_satisfied_within(Duration::from_secs(10)).unwrap();
    let ids: Vec<String> = m.received_exchanges().iter().map(|e| e.message.headers.get(h::ID).unwrap().to_string()).collect();
    assert_eq!(ids, vec!["d0", "d1", "d2", "d3", "d4", "new"]);
    ctx.stop().unwrap();
    assert!(server.requests_containing("limit=2") > 0);
}

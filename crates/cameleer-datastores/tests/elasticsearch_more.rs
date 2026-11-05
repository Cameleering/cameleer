//! Elasticsearch scroll searches, sniffing and typed document classes.

use cameleer_core::prelude::*;
use cameleer_datastores::elasticsearch::{headers as h, EsDocumentClass, SCROLL_QUERY_COUNT};
use cameleer_datastores::testing::ElasticsearchServer;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_datastores::register(&ctx);
    ctx
}

fn json(s: &str) -> Value {
    cameleer_core::json::parse(s).unwrap()
}

#[test]
fn scroll_search_fetches_every_page() {
    let server = ElasticsearchServer::start().unwrap();
    let ctx = context();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let base = format!("elasticsearch://local?hostAddresses={}&indexName=logs", server.address());
    let docs: Vec<Value> = (0..25).map(|i| Value::object([("n", Value::Int(i)), ("msg", Value::from(format!("line {}", i)))])).collect();
    t.request_body(&format!("{}&operation=Bulk", base), Value::Array(docs)).unwrap();
    let ex = t
        .request(&format!("{}&operation=Search&useScroll=true&scrollKeepAliveMs=30000&size=10&enableDocumentOnlyMode=true", base), |ex| {
            ex.set_body(json(r#"{"query":{"match_all":{}},"sort":["n"]}"#));
        })
        .unwrap();
    let all = ex.body().as_array().unwrap().clone();
    assert_eq!(all.len(), 25);
    assert_eq!(all[24].get("n"), Some(&Value::Int(24)));
    // 3 pages of 10/10/5 + the empty page
    assert_eq!(ex.properties.get(SCROLL_QUERY_COUNT), Some(&Value::Int(4)));
    assert_eq!(server.open_scrolls(), 0, "scroll context cleared");
    assert!(server.requests().iter().any(|r| r == "POST /_search/scroll"));
    // without document-only mode the hits are returned
    let hits = t.request_body(&format!("{}&operation=Search&useScroll=true&size=7", base), Value::Null).unwrap();
    assert_eq!(hits.as_array().unwrap().len(), 25);
    assert!(hits.as_array().unwrap()[0].get("_id").is_some());
}

#[test]
fn sniffing_replaces_host_addresses() {
    let a = ElasticsearchServer::start().unwrap();
    let b = ElasticsearchServer::start().unwrap();
    // node a reports only node b (with a hostname/ip publish address)
    a.set_nodes(vec![format!("localhost/{}", b.address())]);
    let ctx = context();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let uri = format!("elasticsearch://local?hostAddresses={}&indexName=x&operation=Index&enableSniffing=true", a.address());
    t.request_body_and_header(&uri, json(r#"{"k":1}"#), h::INDEX_ID, "1").unwrap();
    assert_eq!(b.count("x"), 1);
    assert_eq!(a.count("x"), 0);
    assert!(a.requests().iter().any(|r| r == "GET /_nodes/http"));
    // without sniffing the configured host is used
    let plain = format!("elasticsearch://local?hostAddresses={}&indexName=x&operation=Index", a.address());
    t.request_body_and_header(&plain, json(r#"{"k":2}"#), h::INDEX_ID, "2").unwrap();
    assert_eq!(a.count("x"), 1);
}

#[test]
fn typed_document_class() {
    let server = ElasticsearchServer::start().unwrap();
    let ctx = context();
    ctx.registry().bind("book", EsDocumentClass::parse("title:string,year:int,price:double,available:boolean").unwrap());
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let base = format!("elasticsearch://local?hostAddresses={}&indexName=books", server.address());
    t.request_body_and_header(&format!("{}&operation=Index", base), json(r#"{"title":"Rust","year":"2018","price":39,"available":"true","extra":"dropped"}"#), h::INDEX_ID, "1").unwrap();
    let typed = format!("{}&documentClass=#book&enableDocumentOnlyMode=true", base);
    let doc = t.request_body(&format!("{}&operation=GetById", typed), "1").unwrap();
    assert_eq!(doc, Value::object([("title", Value::from("Rust")), ("year", Value::Int(2018)), ("price", Value::Float(39.0)), ("available", Value::Bool(true))]));
    let found = t.request_body(&format!("{}&operation=Search", typed), Value::Null).unwrap();
    assert_eq!(found.as_array().unwrap()[0].get("year"), Some(&Value::Int(2018)));
    let many = t.request_body(&format!("{}&operation=MultiGet", typed), Value::Array(vec!["1".into()])).unwrap();
    assert_eq!(many.as_array().unwrap()[0].get("extra"), None);
    // inline spec via header, full response keeps the envelope
    let r = t.request_body_and_header(&format!("{}&operation=GetById", base), "1", h::DOCUMENT_CLASS, "year:string").unwrap();
    assert_eq!(r.get("_source"), Some(&Value::object([("year", Value::from("2018"))])));
    // generic class names keep the document
    let r = t.request_body(&format!("{}&operation=GetById&documentClass=java.util.Map&enableDocumentOnlyMode=true", base), "1").unwrap();
    assert_eq!(r.get("extra"), Some(&Value::from("dropped")));
    assert!(ctx.get_endpoint(&format!("{}&operation=GetById&documentClass=com.acme.Unknown", base)).is_err());
}

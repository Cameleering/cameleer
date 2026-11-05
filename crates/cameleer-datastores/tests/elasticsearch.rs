use std::sync::Arc;

use cameleer_core::prelude::*;
use cameleer_datastores::elasticsearch::headers as h;
use cameleer_datastores::testing::{ElasticsearchServer, ElasticsearchServerConfig};

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_datastores::register(&ctx);
    ctx
}

fn json(s: &str) -> Value {
    cameleer_core::json::parse(s).unwrap()
}

#[test]
fn document_operations_and_search() {
    let server = ElasticsearchServer::start().unwrap();
    let ctx = context();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let base = format!("elasticsearch://local?hostAddresses={}&indexName=books", server.address());
    let op = |o: &str| format!("{}&operation={}", base, o);

    // Index with and without id
    let id = t.request_body_and_header(&op("Index"), json(r#"{"title":"The Rust Programming Language","year":2018,"tags":["rust","systems"]}"#), h::INDEX_ID, "1").unwrap();
    assert_eq!(id, Value::from("1"));
    let auto = t.request_body(&op("Index"), r#"{"title":"Programming Rust","year":2017,"tags":["rust"]}"#).unwrap();
    assert!(!auto.to_string().is_empty());
    t.request_body_and_header(&op("Index"), json(r#"{"title":"Learning Python","year":2013,"tags":["python"]}"#), h::INDEX_ID, "3").unwrap();
    assert_eq!(server.count("books"), 3);

    // GetById (full response and document-only mode)
    let r = t.request_body(&op("GetById"), "1").unwrap();
    assert_eq!(r.get("found"), Some(&Value::Bool(true)));
    assert_eq!(r.get("_source").unwrap().get("year"), Some(&Value::Int(2018)));
    let doc = t.request_body(&format!("{}&enableDocumentOnlyMode=true", op("GetById")), "3").unwrap();
    assert_eq!(doc.get("title"), Some(&Value::from("Learning Python")));
    let missing = t.request_body(&op("GetById"), "nope").unwrap();
    assert_eq!(missing.get("found"), Some(&Value::Bool(false)));

    // Update (partial doc)
    let id = t.request_body_and_header(&op("Update"), json(r#"{"year":2019,"edition":2}"#), h::INDEX_ID, "1").unwrap();
    assert_eq!(id, Value::from("1"));
    let src = server.document("books", "1").unwrap();
    assert_eq!(src.get("year"), Some(&Value::Int(2019)));
    assert_eq!(src.get("title"), Some(&Value::from("The Rust Programming Language")));

    // Search: match / term / bool / range, document-only and hits object
    let docs_uri = format!("{}&enableDocumentOnlyMode=true", op("Search"));
    let titles = |v: &Value| -> Vec<String> { v.as_array().unwrap().iter().map(|d| d.get("title").unwrap().to_string()).collect() };
    let r = t.request_body(&docs_uri, json(r#"{"match":{"title":"rust"}}"#)).unwrap();
    assert_eq!(r.as_array().unwrap().len(), 2);
    let r = t.request_body(&docs_uri, json(r#"{"term":{"tags":"python"}}"#)).unwrap();
    assert_eq!(titles(&r), vec!["Learning Python"]);
    let r = t.request_body(&docs_uri, json(r#"{"bool":{"must":[{"match":{"tags":"rust"}}],"filter":[{"range":{"year":{"gte":2018}}}]}}"#)).unwrap();
    assert_eq!(titles(&r), vec!["The Rust Programming Language"]);
    let r = t.request_body(&docs_uri, r#"{"query":{"match_all":{}},"sort":[{"year":{"order":"asc"}}],"size":2}"#).unwrap();
    assert_eq!(titles(&r), vec!["Learning Python", "Programming Rust"]);
    let r = t.request_body_and_headers(&docs_uri, Value::Null, vec![(h::SIZE, 1.into()), (h::FROM, 1.into())]).unwrap();
    assert_eq!(r.as_array().unwrap().len(), 1);
    let hits = t.request_body(&op("Search"), json(r#"{"match":{"title":{"query":"rust programming","operator":"and"}}}"#)).unwrap();
    assert_eq!(hits.get("total").unwrap().get("value"), Some(&Value::Int(2)));
    assert!(hits.get("hits").unwrap().as_array().unwrap()[0].get("_score").is_some());

    // MultiSearch
    let r = t.request_body(&format!("{}&enableDocumentOnlyMode=true", op("MultiSearch")), json(r#"[{"query":{"term":{"tags":"rust"}}},{"query":{"match":{"title":"python"}}}]"#)).unwrap();
    let r = r.as_array().unwrap();
    assert_eq!((r[0].as_array().unwrap().len(), r[1].as_array().unwrap().len()), (2, 1));

    // MultiGet
    let r = t.request_body(&format!("{}&enableDocumentOnlyMode=true", op("MultiGet")), json(r#"["1","3","x"]"#)).unwrap();
    assert_eq!(r.as_array().unwrap().len(), 3);
    assert_eq!(r.as_array().unwrap()[2], Value::Null);

    // Delete, Exists, DeleteIndex, Ping
    assert_eq!(t.request_body(&op("Delete"), "3").unwrap(), Value::from("deleted"));
    assert_eq!(t.request_body(&op("Delete"), "3").unwrap(), Value::from("not_found"));
    assert_eq!(t.request_body(&op("Exists"), Value::Null).unwrap(), Value::Bool(true));
    assert_eq!(t.request_body(&op("Ping"), Value::Null).unwrap(), Value::Bool(true));
    assert_eq!(t.request_body(&op("DeleteIndex"), Value::Null).unwrap(), Value::Bool(true));
    assert_eq!(t.request_body(&op("Exists"), Value::Null).unwrap(), Value::Bool(false));
    let err = t.request_body(&op("DeleteIndex"), Value::Null).unwrap_err();
    assert!(err.root_cause().is_a("ElasticsearchException"), "{:?}", err);
    assert!(err.root_cause().message.contains("no such index"));
    // operation header override + indexName header
    t.request_body_and_headers(&op("Ping"), json(r#"{"a":1}"#), vec![(h::OPERATION, "Index".into()), (h::INDEX_NAME, "other".into()), (h::INDEX_ID, "z".into())]).unwrap();
    assert!(server.document("other", "z").is_some());
    ctx.stop().unwrap();
}

#[test]
fn bulk_and_opensearch_alias() {
    let server = ElasticsearchServer::start_with(ElasticsearchServerConfig { distribution: Some("opensearch".into()), ..Default::default() }).unwrap();
    let ctx = context();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let uri = format!("opensearch://cluster?hostAddresses=127.0.0.1:1,{}&indexName=logs&operation=Bulk", server.address());
    let r = t.request_body(&uri, json(r#"[{"msg":"one"},{"msg":"two"},{"index":{"_id":"k"},"doc":{"msg":"three"}}]"#)).unwrap();
    let items = r.as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[2].get("_id"), Some(&Value::from("k")));
    assert_eq!(items[0].get("status"), Some(&Value::Int(201)));
    assert_eq!(server.count("logs"), 3);
    let r = t.request_body(&uri, json(r#"[{"delete":{"_id":"k"}},{"update":{"_id":"missing"},"doc":{"x":1}}]"#)).unwrap();
    let items = r.as_array().unwrap();
    assert_eq!(items[0].get("result"), Some(&Value::from("deleted")));
    assert!(items[1].get("error").is_some());
    assert_eq!(server.count("logs"), 2);
    let found = t.request_body(&format!("opensearch://c?hostAddresses={}&indexName=logs&operation=Search&enableDocumentOnlyMode=true", server.address()), json(r#"{"match":{"msg":"two"}}"#)).unwrap();
    assert_eq!(found, Value::Array(vec![Value::object([("msg", "two")])]));
    assert!(server.requests().contains(&"POST /_bulk".to_string()));
    ctx.stop().unwrap();
}

#[test]
fn authentication_and_tls() {
    let tls = Arc::new(cameleer_tls::testing::server_params().server_config().unwrap());
    let server = ElasticsearchServer::start_with(ElasticsearchServerConfig { basic_auth: Some(("elastic".into(), "changeme".into())), api_key: Some("a2V5OnNlY3JldA==".into()), tls: Some(tls), distribution: None }).unwrap();
    let ctx = context();
    cameleer_tls::testing::bind_test_beans(&ctx);
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let base = format!("elasticsearch://c?hostAddresses=localhost:{}&indexName=s&operation=Index&sslContextParameters=#ssl", server.port());
    t.request_body(&format!("{}&user=elastic&password=changeme", base), r#"{"a":1}"#).unwrap();
    t.request_body(&format!("{}&apiKey=a2V5OnNlY3JldA==", base), r#"{"a":2}"#).unwrap();
    assert_eq!(server.count("s"), 2);
    let err = t.request_body(&format!("{}&user=elastic&password=wrong", base), r#"{"a":3}"#).unwrap_err();
    assert!(err.root_cause().message.contains("401"), "{:?}", err);
    let err = t.request_body(&base, r#"{"a":3}"#).unwrap_err();
    assert!(err.root_cause().is_a("ElasticsearchException"));
    // untrusted certificate
    let bad = format!("elasticsearch://c?hostAddresses=localhost:{}&indexName=s&operation=Index&sslContextParameters=#sslUntrusted&user=elastic&password=changeme", server.port());
    let err = t.request_body(&bad, r#"{"a":4}"#).unwrap_err();
    assert!(err.root_cause().is_a("IOException"), "{:?}", err);
    ctx.stop().unwrap();
}

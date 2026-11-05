//! JSONPath language tests.

use cameleer_core::language::jsonpath::{query, JsonPath, PATH_NOT_FOUND};
use cameleer_core::language::{jsonpath, jsonpath_as, LanguageOptions};
use cameleer_core::prelude::*;

const STORE: &str = r#"{
  "store": {
    "book": [
      {"category": "reference", "author": "Nigel Rees", "title": "Sayings of the Century", "price": 8.95},
      {"category": "fiction", "author": "Evelyn Waugh", "title": "Sword of Honour", "price": 12.99},
      {"category": "fiction", "author": "Herman Melville", "title": "Moby Dick", "isbn": "0-553-21311-3", "price": 8.99},
      {"category": "fiction", "author": "J. R. R. Tolkien", "title": "The Lord of the Rings", "isbn": "0-395-19395-8", "price": 22.99}
    ],
    "bicycle": {"color": "red", "price": 19.95}
  },
  "expensive": 10
}"#;

fn doc() -> Value {
    cameleer_core::json::parse(STORE).unwrap()
}

fn strs(v: &[&str]) -> Value {
    Value::from(v.to_vec())
}

#[test]
fn jsonpath_table() {
    let d = doc();
    let cases: Vec<(&str, Value)> = vec![
        ("$.store.bicycle.color", Value::from("red")),
        ("$['store']['bicycle']['color']", Value::from("red")),
        ("$[\"store\"].bicycle[\"price\"]", Value::Float(19.95)),
        ("store.bicycle.color", Value::from("red")),
        ("$.store.book[0].author", Value::from("Nigel Rees")),
        ("$.store.book[-1].title", Value::from("The Lord of the Rings")),
        ("$.expensive", Value::Int(10)),
        ("$.store.book[*].author", strs(&["Nigel Rees", "Evelyn Waugh", "Herman Melville", "J. R. R. Tolkien"])),
        ("$..author", strs(&["Nigel Rees", "Evelyn Waugh", "Herman Melville", "J. R. R. Tolkien"])),
        ("$.store.book[0,2].title", strs(&["Sayings of the Century", "Moby Dick"])),
        ("$.store.book[1:3].title", strs(&["Sword of Honour", "Moby Dick"])),
        ("$.store.book[:2].category", strs(&["reference", "fiction"])),
        ("$.store.book[-2:].title", strs(&["Moby Dick", "The Lord of the Rings"])),
        ("$.store.book[?(@.isbn)].title", strs(&["Moby Dick", "The Lord of the Rings"])),
        ("$.store.book[?(!@.isbn)].title", strs(&["Sayings of the Century", "Sword of Honour"])),
        ("$.store.book[?(@.price < 10)].title", strs(&["Sayings of the Century", "Moby Dick"])),
        ("$.store.book[?(@.price <= 8.95)].title", strs(&["Sayings of the Century"])),
        ("$.store.book[?(@.price > 20)].title", strs(&["The Lord of the Rings"])),
        ("$.store.book[?(@.price >= 12.99)].title", strs(&["Sword of Honour", "The Lord of the Rings"])),
        ("$.store.book[?(@.category == 'reference')].author", strs(&["Nigel Rees"])),
        ("$.store.book[?(@.category != 'fiction')].author", strs(&["Nigel Rees"])),
        ("$.store.book[?(@.price < 10 && @.category == 'fiction')].title", strs(&["Moby Dick"])),
        ("$.store.book[?(@.price > 20 || @.category == 'reference')].title", strs(&["Sayings of the Century", "The Lord of the Rings"])),
        ("$.store.book[?(@.author =~ /.*Melville/)].title", strs(&["Moby Dick"])),
        ("$.store.book[?(@.author =~ /.*melville/i)].title", strs(&["Moby Dick"])),
        ("$.store.book[?(@.price > $.expensive)].title", strs(&["Sword of Honour", "The Lord of the Rings"])),
        ("$.store.book[?(@.category in ['reference','x'])].title", strs(&["Sayings of the Century"])),
        ("$.store.book[?(@.category nin ['fiction'])].title", strs(&["Sayings of the Century"])),
        ("$.store.book[?((@.price < 9) && (@.isbn))].title", strs(&["Moby Dick"])),
        ("$..book[?(@.price < 9)].price", Value::from(vec![Value::Float(8.95), Value::Float(8.99)])),
        ("$..bicycle.color", strs(&["red"])),
        ("$.store.book.length()", Value::Int(4)),
        ("$.store.book[*].price.min()", Value::Float(8.95)),
        ("$.store.book[*].price.max()", Value::Float(22.99)),
        ("$.store.bicycle.keys()", strs(&["color", "price"])),
        ("$.store.book[?(@.nope == 'x')]", Value::Array(vec![])),
        ("$..nope", Value::Array(vec![])),
    ];
    for (path, expected) in cases {
        assert_eq!(query(&d, path).unwrap_or_else(|e| panic!("{}: {}", path, e)), expected, "path: {}", path);
    }
    // wildcard on object, recursive descent counts
    assert_eq!(query(&d, "$.store.*").unwrap().as_array().unwrap().len(), 2);
    assert_eq!(query(&d, "$..price").unwrap().as_array().unwrap().len(), 5);
    assert_eq!(query(&d, "$").unwrap(), d);
    assert!(JsonPath::compile("$.store.book[0]").unwrap().is_definite());
    assert!(!JsonPath::compile("$..book").unwrap().is_definite());
}

#[test]
fn jsonpath_errors() {
    let d = doc();
    assert_eq!(query(&d, "$.store.missing").unwrap_err().kind, PATH_NOT_FOUND);
    assert_eq!(query(&d, "$.store.book[10]").unwrap_err().kind, PATH_NOT_FOUND);
    for bad in ["$.store[", "$.store.book[?(@.price <)]", "$..", "$.a b"] {
        let e = JsonPath::compile(bad).err().unwrap_or_else(|| panic!("expected error for {}", bad));
        assert_eq!(e.kind, kinds::EXPRESSION_ILLEGAL_SYNTAX, "{}", bad);
    }
    // string input is parsed; invalid JSON fails
    assert_eq!(query(&Value::from(r#"{"a":1}"#), "$.a").unwrap(), Value::Int(1));
    assert!(query(&Value::from("not json"), "$.a").is_err());
}

#[test]
fn jsonpath_in_routes() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:in")
            .set_header("color", jsonpath("$.store.bicycle.color"))
            .set_header("count", jsonpath_as("$.store.book.length()", "String"))
            .set_header("missing", jsonpath("$.nope").with_option("suppressExceptions", "true"))
            .set_header("cheap", jsonpath("$.store.book[?(@.price < 9)].title"))
            .set_body(jsonpath("$.store.book[0]"))
            .to("mock:out");
        b.from("direct:header").set_body(jsonpath("$.x").with_option("headerName", "data")).to("mock:header");
        b.from("direct:yaml-style").set_body(language("jsonpath", "$.a").with_option("unpackArray", "true")).to("mock:unpack");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let out = ctx.mock_endpoint("mock:out").unwrap();
    out.expected_message_count(1);
    out.expected_header_received("color", "red");
    out.expected_header_received("count", "4");
    t.send_body("direct:in", STORE).unwrap();
    out.assert_is_satisfied().unwrap();
    let ex = &out.received_exchanges()[0];
    assert_eq!(ex.header("missing"), Some(&Value::Null));
    assert_eq!(ex.header("cheap").unwrap().as_array().unwrap().len(), 2);
    assert_eq!(ex.body().get("author"), Some(&Value::from("Nigel Rees")));

    // missing definite path fails the exchange
    let err = t.send_body("direct:in", r#"{"store":{"book":[]}}"#).unwrap_err();
    assert!(err.to_string().contains(PATH_NOT_FOUND), "{}", err);

    let p = jsonpath("$.store.book[?(@.price > 20)]").def.resolve_predicate(&ctx).unwrap();
    let mut ex = ctx.create_exchange();
    ex.set_body(STORE);
    assert!(p.matches(&ex).unwrap());
    ex.set_body(r#"{"store":{"book":[{"price":1}]}}"#);
    assert!(!p.matches(&ex).unwrap());
    let p = jsonpath("$.nothing.here").def.resolve_predicate(&ctx).unwrap();
    assert!(!p.matches(&ex).unwrap());

    let h = ctx.mock_endpoint("mock:header").unwrap();
    h.expected_bodies_received(vec![Value::Int(5)]);
    t.send_body_and_header("direct:header", "ignored", "data", cameleer_core::json::parse(r#"{"x":5}"#).unwrap()).unwrap();
    h.assert_is_satisfied().unwrap();

    let u = ctx.mock_endpoint("mock:unpack").unwrap();
    u.expected_bodies_received(vec![Value::from("only")]);
    t.send_body("direct:yaml-style", r#"{"a":["only"]}"#).unwrap();
    u.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
}

//! json-validator and validator (XML well-formedness) components.

use std::path::PathBuf;

use cameleer_core::error::kind_is_a;
use cameleer_core::prelude::*;

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("camel-val-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

const SCHEMA: &str = r##"{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "type": "object",
  "required": ["id", "name", "price"],
  "properties": {
    "id": {"type": "integer"},
    "name": {"type": "string", "minLength": 1},
    "price": {"type": "number", "exclusiveMinimum": 0},
    "tags": {"type": "array", "items": {"$ref": "#/definitions/tag"}, "uniqueItems": true}
  },
  "additionalProperties": false,
  "definitions": {"tag": {"type": "string", "enum": ["a", "b"]}}
}"##;

#[test]
fn json_validator_valid_and_invalid() {
    let dir = temp_dir("json");
    std::fs::write(dir.join("schema.json"), SCHEMA).unwrap();
    let uri = format!("json-validator:{}", dir.join("schema.json").display());
    let ctx = CamelContext::new();
    let u = uri.clone();
    ctx.add_routes(move |b| {
        b.from("direct:validate")
            .do_try()
            .to(u.as_str())
            .to("mock:valid")
            .do_catch(&["ValidationException"])
            .to("mock:invalid")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:validate", r#"{"id": 1, "name": "Pen", "price": 1.5, "tags": ["a"]}"#).unwrap();
    // structured bodies are validated directly
    t.send_body("direct:validate", cameleer_core::json::parse(r#"{"id": 2, "name": "Ink", "price": 3}"#).unwrap()).unwrap();
    t.send_body("direct:validate", r#"{"id": "x", "price": 0, "tags": ["a", "a", "z"], "extra": true}"#).unwrap();
    let valid = ctx.mock_endpoint("mock:valid").unwrap();
    let invalid = ctx.mock_endpoint("mock:invalid").unwrap();
    assert_eq!(valid.received_counter(), 2);
    assert_eq!(invalid.received_counter(), 1);
    let ex = invalid.exchange(0).unwrap();
    let caught = ex.property("CamelExceptionCaught").expect("caught exception");
    let msg = caught.to_string();
    for expected in [
        "$.name: is missing but it is required",
        "$.id: string found, integer expected",
        "$.price: must have an exclusive minimum value of 0",
        "$.tags: the items in the array must be unique",
        "$.tags[2]: does not have a value in the enumeration [a, b]",
        "$.extra: is not defined in the schema and the schema does not allow additional properties",
    ] {
        assert!(msg.contains(expected), "missing '{}' in {}", expected, msg);
    }
    assert!(msg.contains("JSON validation error with 6 errors"), "{}", msg);
    let errors = ex.property("CamelJsonValidationErrors").unwrap().as_array().unwrap().len();
    assert_eq!(errors, 6);
    ctx.stop().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn json_validator_exception_kinds_and_options() {
    let dir = temp_dir("json-kinds");
    std::fs::write(dir.join("s.json"), r#"{"type":"object","required":["a"]}"#).unwrap();
    let base = format!("json-validator:file:{}", dir.join("s.json").display());
    let ctx = CamelContext::new();
    let t = ctx.create_producer_template();
    let err = t.request_body(&base, r#"{"b":1}"#).unwrap_err();
    let cause = err.cause.as_deref().unwrap();
    assert_eq!(cause.kind, "JsonValidationException");
    assert!(kind_is_a(&cause.kind, "ValidationException"));
    // null body
    let err = t.request_body(&base, Value::Null).unwrap_err();
    assert_eq!(err.cause.as_deref().unwrap().kind, "NoJsonBodyValidationException");
    assert!(t.request_body(&format!("{}?failOnNullBody=false", base), Value::Null).is_ok());
    // invalid JSON text
    let err = t.request_body(&base, "{not json").unwrap_err();
    assert_eq!(err.cause.as_deref().unwrap().kind, "JsonValidationException");
    // header validation
    let hdr = format!("{}?headerName=payload", base);
    assert!(t.request_body_and_header(&hdr, "ignored", "payload", r#"{"a":1}"#).is_ok());
    let err = t.request_body(&hdr, "x").unwrap_err();
    assert_eq!(err.cause.as_deref().unwrap().kind, "NoJsonHeaderValidationException");
    // unknown option fails endpoint creation
    assert!(ctx.get_endpoint(&format!("{}?bogus=1", base)).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn xml_validator_checks_well_formedness_and_root() {
    let dir = temp_dir("xml");
    std::fs::write(
        dir.join("order.xsd"),
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="order"/></xs:schema>"#,
    )
    .unwrap();
    let uri = format!("validator:{}", dir.join("order.xsd").display());
    let ctx = CamelContext::new();
    let t = ctx.create_producer_template();
    assert!(t.request_body(&uri, "<order><id>1</id></order>").is_ok());
    let err = t.request_body(&uri, "<order><id>1</order>").unwrap_err();
    let cause = err.cause.as_deref().unwrap();
    assert_eq!(cause.kind, "SchemaValidationException");
    assert!(kind_is_a(&cause.kind, "ValidationException"));
    let err = t.request_body(&uri, "<invoice/>").unwrap_err();
    assert!(err.cause.as_deref().unwrap().message.contains("Cannot find the declaration of element 'invoice'"));
    let err = t.request_body(&uri, Value::Null).unwrap_err();
    assert_eq!(err.cause.as_deref().unwrap().kind, "NoXmlBodyValidationException");
    let err = t.request_body("validator:missing.xsd", "<a/>").unwrap_err();
    assert_eq!(err.cause.as_deref().unwrap().kind, "FileNotFoundException");
    let _ = std::fs::remove_dir_all(&dir);
}

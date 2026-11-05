//! Transformer / Validator (data types): route contracts, automatic transformation,
//! built-in and custom transformers, transformer chains, validation, `transformDataType`.

use cameleer_core::prelude::*;
use cameleer_core::processor::transformer::{
    data_format_transformer, endpoint_transformer, endpoint_validator, message_data_type, predicate_validator, processor_transformer,
    CamelContextTransformerExt, DataType, MESSAGE_DATA_TYPE,
};

fn json_to_xml(ex: &mut Exchange) -> Result<()> {
    let v = cameleer_core::json::parse(&ex.body_as::<String>()?)?;
    let name = v.get("name").map(|n| n.to_string()).unwrap_or_default();
    ex.set_body(format!("<order><name>{}</name></order>", name));
    Ok(())
}

fn root_kind(e: &CamelError) -> String {
    let mut cur = e;
    while let Some(c) = &cur.cause {
        if cur.kind == "CamelExecutionException" {
            cur = c;
        } else {
            break;
        }
    }
    cur.kind.clone()
}

#[test]
fn automatic_transformation_between_route_input_types() {
    let ctx = CamelContext::new();
    ctx.add_transformer("json:Order", "xml:Order", processor_transformer(json_to_xml));
    ctx.add_routes(|b| {
        b.from("direct:json").input_type("json:Order", false).to("mock:jsonSeen").to("direct:xml").to("mock:back");
        b.from("direct:xml").input_type("xml:Order", false).to("mock:xml");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:json", r#"{"name":"apple"}"#).unwrap();
    let seen = ctx.mock_endpoint("jsonSeen").unwrap().exchange(0).unwrap();
    assert_eq!(seen.body(), &Value::from(r#"{"name":"apple"}"#));
    assert_eq!(message_data_type(&seen), Some(DataType::parse("json:Order")));
    let xml = ctx.mock_endpoint("xml").unwrap().exchange(0).unwrap();
    assert_eq!(xml.body(), &Value::from("<order><name>apple</name></order>"));
    assert_eq!(xml.property(MESSAGE_DATA_TYPE), Some(&Value::from("xml:Order")));
    // the data type travels back with the exchange
    assert_eq!(ctx.mock_endpoint("back").unwrap().exchange(0).unwrap().property(MESSAGE_DATA_TYPE), Some(&Value::from("xml:Order")));
}

#[test]
fn output_type_is_applied_when_the_route_completes() {
    let ctx = CamelContext::new();
    ctx.add_transformer("json:Order", "xml:Order", processor_transformer(json_to_xml));
    ctx.add_routes(|b| {
        b.from("direct:start").input_type("json:Order", false).output_type("xml:Order", false).to("mock:inside");
        b.from("direct:obj").output_type("application-json", false).set_body(constant(Value::object([("a", 1)])));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let r = t.request_body("direct:start", r#"{"name":"pear"}"#).unwrap();
    assert_eq!(r, Value::from("<order><name>pear</name></order>"));
    assert_eq!(ctx.mock_endpoint("inside").unwrap().received_bodies(), vec![Value::from(r#"{"name":"pear"}"#)]);
    let ex = t.request("direct:obj", |_| {}).unwrap();
    assert_eq!(ex.body(), &Value::from(r#"{"a":1}"#));
    assert_eq!(ex.header("Content-Type"), Some(&Value::from("application/json")));
}

#[test]
fn data_format_transformer_marshals_and_unmarshals() {
    let ctx = CamelContext::new();
    ctx.add_transformer("json:Order", "java:Order", data_format_transformer(DataFormatDefinition::json()));
    ctx.add_transformer("java:Order", "json:Order", data_format_transformer(DataFormatDefinition::json()));
    ctx.add_routes(|b| {
        b.from("direct:in").input_type("json:Order", false).to("direct:java");
        b.from("direct:java").input_type("java:Order", false).to("mock:java").output_type("json:Order", false);
    })
    .unwrap();
    ctx.start().unwrap();
    let r = ctx.create_producer_template().request_body("direct:in", r#"{"id":5}"#).unwrap();
    let java = ctx.mock_endpoint("java").unwrap().exchange(0).unwrap();
    assert_eq!(java.body().get("id"), Some(&Value::from(5)));
    assert_eq!(r, Value::from(r#"{"id":5}"#));
}

#[test]
fn endpoint_transformer_and_transformer_chain() {
    let ctx = CamelContext::new();
    ctx.add_transformer("csv:Order", "java:Order", endpoint_transformer("direct:csvToJava"));
    ctx.add_transformer("java:Order", "xml:Order", processor_transformer(|ex: &mut Exchange| {
        let parts: Vec<String> = ex.body_as()?;
        ex.set_body(format!("<order>{}</order>", parts.join("|")));
        Ok(())
    }));
    ctx.add_routes(|b| {
        b.from("direct:csvToJava").process(|ex: &mut Exchange| {
            let s: String = ex.body_as()?;
            ex.set_body(Value::from(s.split(',').map(|p| p.to_string()).collect::<Vec<String>>()));
            Ok(())
        });
        b.from("direct:csv").input_type("csv:Order", false).to("direct:xml");
        b.from("direct:xml").input_type("xml:Order", false).to("mock:xml");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:csv", "a,b").unwrap();
    assert_eq!(ctx.mock_endpoint("xml").unwrap().received_bodies(), vec![Value::from("<order>a|b</order>")]);
}

#[test]
fn validation_failures_raise_validation_exception() {
    let ctx = CamelContext::new();
    ctx.add_validator("xml:Order", predicate_validator(body().contains("<order>")));
    ctx.add_validator("json", endpoint_validator("direct:checkJson"));
    ctx.add_routes(|b| {
        b.from("direct:xml").input_type("xml:Order", true).to("mock:xml");
        b.from("direct:noValidator").input_type("csv:Order", true).to("mock:none");
        b.from("direct:json").input_type("json:Order", true).to("mock:json");
        b.from("direct:checkJson").process(|ex: &mut Exchange| {
            cameleer_core::json::parse(&ex.body_as::<String>()?)?;
            Ok(())
        });
        b.from("direct:out").output_type("xml:Order", true).set_body(constant("nope"));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:xml", "<order>1</order>").unwrap();
    assert_eq!(ctx.mock_endpoint("xml").unwrap().received_counter(), 1);
    let e = t.send_body("direct:xml", "plain").unwrap_err();
    assert_eq!(root_kind(&e), kinds::VALIDATION, "{}", e);
    assert_eq!(ctx.mock_endpoint("xml").unwrap().received_counter(), 1);

    let e = t.send_body("direct:noValidator", "a,b").unwrap_err();
    assert_eq!(root_kind(&e), kinds::VALIDATION);
    assert!(format!("{}", e).contains("No Validator found"), "{}", e);

    t.send_body("direct:json", r#"{"ok":true}"#).unwrap();
    let e = t.send_body("direct:json", "{broken").unwrap_err();
    assert_eq!(root_kind(&e), kinds::VALIDATION, "{}", e);
    assert_eq!(ctx.mock_endpoint("json").unwrap().received_counter(), 1);

    let e = t.request_body("direct:out", "x").unwrap_err();
    assert_eq!(root_kind(&e), kinds::VALIDATION, "{}", e);
}

#[test]
fn missing_transformer_between_declared_types_fails() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:a").input_type("json:Order", false).to("direct:b");
        b.from("direct:b").input_type("xml:Order", false).to("mock:b");
    })
    .unwrap();
    ctx.start().unwrap();
    let e = ctx.create_producer_template().send_body("direct:a", "{}").unwrap_err();
    assert!(format!("{}", e).contains("No Transformer found for [from='json:Order', to='xml:Order']"), "{}", e);
    assert_eq!(ctx.mock_endpoint("b").unwrap().received_counter(), 0);
}

#[test]
fn transform_data_type_with_builtins_and_custom_transformers() {
    let ctx = CamelContext::new();
    ctx.add_transformer("json:Order", "xml:Order", processor_transformer(json_to_xml));
    ctx.add_named_transformer("uppercase", processor_transformer(|ex: &mut Exchange| {
        let s: String = ex.body_as()?;
        ex.set_body(s.to_uppercase());
        Ok(())
    }));
    ctx.add_routes(|b| {
        b.from("direct:json").transform_data_type(None, "application-json").to("mock:json");
        b.from("direct:text").transform_data_type(None, "text-plain").to("mock:text");
        b.from("direct:bytes").transform_data_type(None, "application-octet-stream").to("mock:bytes");
        b.from("direct:struct").transform_data_type(None, "application-x-struct").to("mock:struct");
        b.from("direct:order").transform_data_type(Some("json:Order"), "xml:Order").to("mock:order");
        b.from("direct:named").transform_data_type(None, "my:uppercase").to("mock:named");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:json", Value::object([("k", "v")])).unwrap();
    let j = ctx.mock_endpoint("json").unwrap().exchange(0).unwrap();
    assert_eq!(j.body(), &Value::from(r#"{"k":"v"}"#));
    assert_eq!(j.header("Content-Type"), Some(&Value::from("application/json")));
    assert_eq!(j.property(MESSAGE_DATA_TYPE), Some(&Value::from("application-json")));

    t.send_body("direct:text", Value::Bytes(b"hi".to_vec())).unwrap();
    assert_eq!(ctx.mock_endpoint("text").unwrap().received_bodies(), vec![Value::from("hi")]);
    t.send_body("direct:bytes", "hi").unwrap();
    assert_eq!(ctx.mock_endpoint("bytes").unwrap().received_bodies(), vec![Value::Bytes(b"hi".to_vec())]);
    t.send_body("direct:struct", r#"{"n":[1,2]}"#).unwrap();
    assert_eq!(ctx.mock_endpoint("struct").unwrap().exchange(0).unwrap().body().get("n"), Some(&Value::from(vec![Value::from(1), Value::from(2)])));
    t.send_body("direct:order", r#"{"name":"fig"}"#).unwrap();
    assert_eq!(ctx.mock_endpoint("order").unwrap().received_bodies(), vec![Value::from("<order><name>fig</name></order>")]);
    t.send_body("direct:named", "abc").unwrap();
    assert_eq!(ctx.mock_endpoint("named").unwrap().received_bodies(), vec![Value::from("ABC")]);
}

#[test]
fn scheme_transformer_and_java_conversion() {
    let ctx = CamelContext::new();
    ctx.add_scheme_transformer("csv", data_format_transformer(DataFormatDefinition::csv()));
    ctx.add_routes(|b| {
        b.from("direct:csv").input_type("csv:Rows", false).to("direct:java");
        b.from("direct:java").input_type("java:Rows", false).to("mock:rows");
        b.from("direct:str").input_type("java:java.lang.String", false).to("mock:str");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:csv", "a,b\nc,d").unwrap();
    let rows = ctx.mock_endpoint("rows").unwrap().exchange(0).unwrap();
    assert_eq!(rows.body().as_array().map(|a| a.len()), Some(2));
    t.send_body("direct:str", 42).unwrap();
    assert_eq!(ctx.mock_endpoint("str").unwrap().received_bodies(), vec![Value::from("42")]);
}

//! Conformance tests for Exchange / Message semantics (Apache Camel 4 behaviour).

use std::time::Duration;

use camel_core::prelude::*;

#[test]
fn in_out_through_direct_chain_returns_last_body() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:a").set_body(simple("${body}-a")).to("direct:b").set_body(simple("${body}-a2"));
        b.from("direct:b").set_body(simple("${body}-b")).to("direct:c");
        b.from("direct:c").set_body(simple("${body}-c"));
    })
    .unwrap();
    ctx.start().unwrap();
    let reply = ctx.create_producer_template().request_body("direct:a", "x").unwrap();
    assert_eq!(reply, Value::from("x-a-b-c-a2"));
}

#[test]
fn in_only_send_still_sees_modified_exchange() {
    // With direct the same exchange flows through; even InOnly the caller sees the change
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:a").set_header("touched", constant(true)).to("direct:b");
        b.from("direct:b").set_body(constant("changed"));
    })
    .unwrap();
    ctx.start().unwrap();
    let ex = ctx.create_producer_template().send_with("direct:a", ExchangePattern::InOnly, |e| e.set_body("orig"));
    assert_eq!(ex.body(), &Value::from("changed"));
    assert_eq!(ex.header("touched"), Some(&Value::Bool(true)));
    assert_eq!(ex.pattern, ExchangePattern::InOnly);
}

#[test]
fn seda_in_out_waits_for_reply() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").to("seda:work").set_body(simple("${body}!"));
        b.from("seda:work").set_body(simple("Bye ${body}"));
    })
    .unwrap();
    ctx.start().unwrap();
    let reply = ctx.create_producer_template().request_body("direct:start", "World").unwrap();
    assert_eq!(reply, Value::from("Bye World!"));
    ctx.stop().unwrap();
}

#[test]
fn seda_in_only_does_not_wait_and_caller_keeps_its_body() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").to("seda:work").to("mock:after");
        b.from("seda:work").delay(constant(50)).set_body(constant("changed")).to("mock:seda");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "orig").unwrap();
    let after = ctx.mock_endpoint("mock:after").unwrap();
    assert_eq!(after.received_bodies(), vec![Value::from("orig")]);
    let seda = ctx.mock_endpoint("mock:seda").unwrap();
    seda.expected_bodies_received(vec!["changed"]);
    seda.assert_is_satisfied_within(Duration::from_secs(5)).unwrap();
    ctx.stop().unwrap();
}

#[test]
fn seda_in_only_endpoint_option_with_in_out_caller() {
    // to("seda:x?waitForTaskToComplete=Never") never waits even for InOut
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").to("seda:x?waitForTaskToComplete=Never");
        b.from("seda:x").set_body(constant("from seda")).to("mock:x");
    })
    .unwrap();
    ctx.start().unwrap();
    let reply = ctx.create_producer_template().request_body("direct:start", "orig").unwrap();
    assert_eq!(reply, Value::from("orig"));
    ctx.mock_endpoint("mock:x").unwrap().wait_for_messages(1, Duration::from_secs(5));
    ctx.stop().unwrap();
}

#[test]
fn headers_are_case_insensitive_and_preserve_case() {
    let mut m = Message::new("b");
    m.set_header("Content-Type", "text/plain");
    assert_eq!(m.header("content-type"), Some(&Value::from("text/plain")));
    assert_eq!(m.header("CONTENT-TYPE"), Some(&Value::from("text/plain")));
    m.set_header("content-type", "application/json");
    assert_eq!(m.headers.len(), 1);
    assert_eq!(m.header("Content-Type"), Some(&Value::from("application/json")));
    assert!(m.remove_header("CONTENT-type").is_some());
    assert!(m.headers.is_empty());
}

#[test]
fn headers_case_insensitive_in_routes() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").set_header("MyHeader", constant("v")).filter(header("myheader").is_equal_to("v")).to("mock:hit").end();
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "x").unwrap();
    assert_eq!(ctx.mock_endpoint("mock:hit").unwrap().received_counter(), 1);
}

#[test]
fn convert_body_to_string_from_bytes_and_numbers() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:s").convert_body_to("String").to("mock:s");
        b.from("direct:i").convert_body_to("Integer").to("mock:i");
        b.from("direct:bytes").convert_body_to("byte[]").to("mock:bytes");
        b.from("direct:java").convert_body_to("java.lang.String").to("mock:java");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:s", Value::Bytes(b"hello".to_vec())).unwrap();
    t.send_body("direct:s", 42).unwrap();
    t.send_body("direct:s", 1.5).unwrap();
    t.send_body("direct:s", true).unwrap();
    t.send_body("direct:i", "123").unwrap();
    t.send_body("direct:bytes", "abc").unwrap();
    t.send_body("direct:java", 5).unwrap();
    assert_eq!(
        ctx.mock_endpoint("mock:s").unwrap().received_bodies(),
        vec![Value::from("hello"), Value::from("42"), Value::from("1.5"), Value::from("true")]
    );
    assert_eq!(ctx.mock_endpoint("mock:i").unwrap().received_bodies(), vec![Value::Int(123)]);
    assert_eq!(ctx.mock_endpoint("mock:bytes").unwrap().received_bodies(), vec![Value::Bytes(b"abc".to_vec())]);
    assert_eq!(ctx.mock_endpoint("mock:java").unwrap().received_bodies(), vec![Value::from("5")]);
}

#[test]
fn convert_body_to_failure_is_type_conversion_exception() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:i").convert_body_to("Integer");
    })
    .unwrap();
    ctx.start().unwrap();
    let e = ctx.create_producer_template().send_body("direct:i", "abc").unwrap_err();
    assert!(e.is_a(kinds::CAMEL_EXECUTION));
    let cause = e.cause.unwrap();
    assert!(cause.is_a("TypeConversionException") || cause.is_a("InvalidPayloadException") || cause.is_a("NoTypeConversionAvailableException"), "{}", cause);
}

#[test]
fn body_as_type_converter() {
    let ex = Exchange::with_body(None, Value::Bytes(b"12".to_vec()));
    assert_eq!(ex.body_as::<String>().unwrap(), "12");
    assert_eq!(ex.body_as::<i64>().unwrap(), 12);
    let ex = Exchange::with_body(None, "true");
    assert!(ex.body_as::<bool>().unwrap());
}

#[test]
fn correlated_copy_has_new_id_and_correlation_property() {
    let mut ex = Exchange::with_body(None, "b");
    ex.set_header("h", 1);
    ex.set_property("p", 2);
    let c = ex.correlated_copy();
    assert_ne!(c.exchange_id, ex.exchange_id);
    assert_eq!(c.property(props::CORRELATION_ID), Some(&Value::from(ex.exchange_id.clone())));
    assert_eq!(c.header("h"), Some(&Value::Int(1)));
    assert_eq!(c.property("p"), Some(&Value::Int(2)));
    assert_eq!(c.body(), &Value::from("b"));
}

#[test]
fn properties_visible_across_direct_routes_but_not_seda_copies_are_isolated() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:a").set_property("myProp", constant("fromA")).to("direct:b").to("mock:a");
        b.from("direct:b").set_body(simple("${exchangeProperty.myProp}")).set_property("fromB", constant(1));
        b.from("direct:s").set_property("myProp", constant("x")).to("seda:q");
        b.from("seda:q").set_property("myProp", constant("changed in seda")).to("mock:q");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:a", "?").unwrap();
    let a = ctx.mock_endpoint("mock:a").unwrap().exchange(0).unwrap();
    assert_eq!(a.body(), &Value::from("fromA"));
    assert_eq!(a.property("fromB"), Some(&Value::Int(1)));

    // seda: properties are copied to the consumer side
    let ex = t.send_with("direct:s", ExchangePattern::InOnly, |e| e.set_body("x"));
    let q = ctx.mock_endpoint("mock:q").unwrap();
    q.wait_for_messages(1, Duration::from_secs(5));
    assert_eq!(q.exchange(0).unwrap().property("myProp"), Some(&Value::from("changed in seda")));
    assert_eq!(ex.property("myProp"), Some(&Value::from("x")));
    ctx.stop().unwrap();
}

#[test]
fn set_exchange_pattern_and_in_only_to() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").in_only("seda:audit").set_body(constant("reply"));
        b.from("seda:audit").process(|ex: &mut Exchange| {
            ex.set_header("pattern", format!("{:?}", ex.pattern));
            Ok(())
        })
        .to("mock:audit");
    })
    .unwrap();
    ctx.start().unwrap();
    assert_eq!(ctx.create_producer_template().request_body("direct:start", "x").unwrap(), Value::from("reply"));
    let m = ctx.mock_endpoint("mock:audit").unwrap();
    m.wait_for_messages(1, Duration::from_secs(5));
    assert_eq!(m.exchange(0).unwrap().header("pattern"), Some(&Value::from("InOnly")));
    ctx.stop().unwrap();
}

#[test]
fn exchange_has_created_timestamp_and_from_route_id() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").route_id("myRoute").set_body(simple("${routeId}")).to("mock:r");
    })
    .unwrap();
    ctx.start().unwrap();
    let ex = ctx.create_producer_template().send_with("direct:start", ExchangePattern::InOut, |e| e.set_body("x"));
    assert_eq!(ex.body(), &Value::from("myRoute"));
    assert_eq!(ex.from_route_id.as_deref(), Some("myRoute"));
    assert!(ex.created_millis() > 0);
}

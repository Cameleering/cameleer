//! The context's TypeConverterRegistry is used by convertBodyTo and Simple bodyAs.

use camel_core::prelude::*;

#[test]
fn custom_converters_in_routes() {
    let ctx = CamelContext::new();
    ctx.type_converter()
        .add_converter("String", "com.acme.Order", |v: &Value, _: Option<&Exchange>| {
            let (id, qty) = v.to_string().split_once(':').map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or_default();
            Ok(Some(Value::object([("id", Value::from(id)), ("qty", Value::from(qty.parse::<i64>().unwrap_or(0)))])))
        })
        .unwrap();
    ctx.add_routes(|b| {
        b.from("direct:order").convert_body_to("com.acme.Order").set_header("qty", simple("${body[qty]}")).to("mock:order");
        b.from("direct:simple").set_body(simple("${bodyAs(com.acme.Order)}")).to("mock:simple");
        b.from("direct:int").convert_body_to("java.lang.Integer").to("mock:int");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:order", "A1:3").unwrap();
    let ex = ctx.mock_endpoint("mock:order").unwrap().exchange(0).unwrap();
    assert_eq!(ex.body().get("id"), Some(&Value::from("A1")));
    assert_eq!(ex.header("qty").map(|v| v.to_string()), Some("3".into()));
    t.send_body("direct:simple", "B:1").unwrap();
    assert_eq!(ctx.mock_endpoint("mock:simple").unwrap().exchange(0).unwrap().body().get("id"), Some(&Value::from("B")));
    let e = t.send_body("direct:int", "x").unwrap_err();
    assert!(e.chain().any(|c| c.kind == kinds::NO_TYPE_CONVERSION_AVAILABLE), "{}", e);
    let stats = ctx.type_converter().statistics();
    assert!(stats.hits >= 2 && stats.failures >= 1);
    ctx.stop().unwrap();
}

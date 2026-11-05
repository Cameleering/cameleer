//! Route configurations (Camel `routeConfiguration`) for Rust DSL routes and context-wide
//! `onException`.

use cameleer_core::prelude::*;

#[test]
fn route_configuration_without_id_applies_to_all_routes() {
    let ctx = CamelContext::new();
    ctx.add_route_configuration(None, |c| {
        c.on_exception(&["IOException"]).handled(true).set_body(constant("handled globally")).to("mock:global").end();
    })
    .unwrap();
    ctx.add_routes(|b| {
        b.from("direct:a").throw_exception("IOException", "a failed");
        b.from("direct:b").throw_exception("FileNotFoundException", "b failed");
        // route-scoped clauses still win
        b.from("direct:c")
            .on_exception(&["IOException"])
            .handled(true)
            .to("mock:route")
            .end()
            .throw_exception("IOException", "c failed");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    assert_eq!(t.request_body("direct:a", "x").unwrap(), Value::from("handled globally"));
    assert_eq!(t.request_body("direct:b", "x").unwrap(), Value::from("handled globally"));
    t.request_body("direct:c", "x").unwrap();
    assert_eq!(ctx.mock_endpoint("mock:global").unwrap().received_counter(), 2);
    assert_eq!(ctx.mock_endpoint("mock:route").unwrap().received_counter(), 1);
    // other exceptions are not handled
    ctx.add_routes(|b| {
        b.from("direct:d").throw_exception("IllegalArgumentException", "d failed");
    })
    .unwrap();
    assert!(t.request_body("direct:d", "x").is_err());
}

#[test]
fn route_configuration_selected_by_id() {
    let ctx = CamelContext::new();
    ctx.add_route_configuration(Some("retrying"), |c| {
        c.error_handler(dead_letter_channel("mock:dead").maximum_redeliveries(2).redelivery_delay(0));
        c.intercept_send_to_endpoint("mock:out*").to("mock:intercepted").end();
    })
    .unwrap();
    ctx.add_route_configuration(Some("quiet"), |c| {
        c.on_exception(&["Exception"]).handled(true).to("mock:quiet").end();
    })
    .unwrap();
    ctx.add_routes(|b| {
        b.from("direct:retry").route_configuration_id("retrying").to("mock:out").throw_exception("IOException", "boom");
        b.from("direct:both").route_configuration_id("retrying, quiet").throw_exception("IOException", "boom");
        b.from("direct:wild").route_configuration_id("qui*").throw_exception("IOException", "boom");
        b.from("direct:none").to("mock:out").throw_exception("IOException", "boom");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:retry", "r").unwrap();
    let dead = ctx.mock_endpoint("mock:dead").unwrap();
    assert_eq!(dead.received_counter(), 1);
    assert_eq!(dead.exchange(0).unwrap().header(props::REDELIVERY_COUNTER), Some(&Value::Int(2)));
    assert_eq!(ctx.mock_endpoint("mock:intercepted").unwrap().received_counter(), 1);

    t.send_body("direct:both", "b").unwrap();
    t.send_body("direct:wild", "w").unwrap();
    assert_eq!(ctx.mock_endpoint("mock:quiet").unwrap().received_counter(), 2);
    assert_eq!(dead.received_counter(), 1, "the onException clause wins over the dead letter channel");

    // a route without routeConfigurationId only gets configurations without id
    assert!(t.send_body("direct:none", "n").is_err());
    assert_eq!(ctx.mock_endpoint("mock:intercepted").unwrap().received_counter(), 1);
}

#[test]
fn route_configuration_rejects_other_steps() {
    let ctx = CamelContext::new();
    let r = ctx.add_route_configuration(Some("bad"), |c| {
        c.to("mock:x");
    });
    assert!(r.is_err());
}

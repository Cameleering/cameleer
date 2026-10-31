use camel_core::prelude::*;

#[test]
fn direct_to_mock_with_transformations() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .route_id("main")
            .set_header("greeting", constant("Hello"))
            .process(|ex: &mut Exchange| {
                let b: String = ex.body_as()?;
                ex.set_body(b.to_uppercase());
                Ok(())
            })
            .set_body(simple("${header.greeting} ${body}"))
            .to("direct:sub")
            .to("mock:result");
        b.from("direct:sub").set_property("seen", constant(true)).to("log:sub?showHeaders=true");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:result").unwrap();
    mock.expected_bodies_received(vec!["Hello WORLD"]);
    mock.expected_header_received("greeting", "Hello");
    mock.expected_property_received("seen", true);

    let t = ctx.create_producer_template();
    t.send_body("direct:start", "world").unwrap();
    mock.assert_is_satisfied().unwrap();

    let reply = t.request_body("direct:start", "again").unwrap();
    assert_eq!(reply, Value::from("Hello AGAIN"));
    assert_eq!(ctx.route_status("main"), Some(ServiceStatus::Started));
    ctx.stop().unwrap();
    assert_eq!(ctx.route_status("main"), Some(ServiceStatus::Stopped));
}

#[test]
fn failures_propagate_to_template() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:fail")
            .process(|_: &mut Exchange| Err(CamelError::illegal_argument("boom")))
            .to("mock:never");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:never").unwrap();
    mock.expected_message_count(0);
    let err = ctx.create_producer_template().send_body("direct:fail", "x").unwrap_err();
    assert!(err.cause.as_ref().unwrap().is_a("IllegalArgumentException"));
    mock.assert_is_satisfied_within(std::time::Duration::from_millis(50)).unwrap();
}

#[test]
fn unknown_endpoint_option_fails() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:a").to("log:x?nonsense=1");
    })
    .unwrap();
    let e = ctx.start().unwrap_err();
    assert!(e.to_string().contains("nonsense"), "{}", e);
}

#[test]
fn no_direct_consumer() {
    let ctx = CamelContext::new();
    ctx.start().unwrap();
    let e = ctx.create_producer_template().send_body("direct:nobody", "x").unwrap_err();
    assert!(e.cause.unwrap().is_a(kinds::DIRECT_CONSUMER_NOT_AVAILABLE));
}

#[test]
fn stop_eip_and_route_control() {
    let ctx = CamelContext::new();
    ctx.properties().set("target", "mock:end");
    ctx.add_routes(|b| {
        b.from("direct:s").route_id("r1").to("mock:before").stop().to("mock:after");
        b.from("direct:p").route_id("r2").to("{{target}}");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:s", 1).unwrap();
    t.send_body("direct:p", 2).unwrap();
    assert_eq!(ctx.mock_endpoint("before").unwrap().received_counter(), 1);
    assert_eq!(ctx.mock_endpoint("after").unwrap().received_counter(), 0);
    assert_eq!(ctx.mock_endpoint("end").unwrap().received_bodies(), vec![Value::from(2)]);
    ctx.stop_route("r2").unwrap();
    assert!(t.send_body("direct:p", 3).is_err());
    ctx.start_route("r2").unwrap();
    t.send_body("direct:p", 4).unwrap();
    assert!(ctx.remove_route("r2").unwrap() == false);
    ctx.stop_route("r2").unwrap();
    assert!(ctx.remove_route("r2").unwrap());
}

#[test]
fn id_applies_to_last_child_inside_block() {
    let ctx = CamelContext::new();
    let mut b = RouteBuilder::new(ctx.clone());
    b.from("direct:a").filter(constant(true)).id("f1").to("mock:x").id("to-x").to("mock:y").end().to("mock:z").id("to-z");
    let defs = b.into_definitions().unwrap();
    let filter = &defs[0].outputs[0];
    assert_eq!(filter.id(), Some("f1"));
    let kids = filter.outputs();
    assert_eq!(kids[0].id(), Some("to-x"));
    assert_eq!(kids[1].id(), None);
    assert_eq!(defs[0].outputs[1].id(), Some("to-z"));
}

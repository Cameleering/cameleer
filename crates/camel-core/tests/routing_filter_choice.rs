use camel_core::prelude::*;

#[test]
fn filter_routes_only_matching() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .filter(header("foo").is_equal_to("bar"))
            .to("mock:filtered")
            .end()
            .to("mock:all");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body_and_header("direct:start", "a", "foo", "bar").unwrap();
    t.send_body_and_header("direct:start", "b", "foo", "baz").unwrap();
    t.send_body("direct:start", "c").unwrap();
    let filtered = ctx.mock_endpoint("mock:filtered").unwrap();
    assert_eq!(filtered.received_bodies(), vec![Value::from("a")]);
    let all = ctx.mock_endpoint("mock:all").unwrap();
    assert_eq!(all.received_counter(), 3);
    let ex = all.received_exchanges();
    assert_eq!(ex[0].property(props::FILTER_MATCHED), Some(&Value::Bool(true)));
    assert_eq!(ex[1].property(props::FILTER_MATCHED), Some(&Value::Bool(false)));
}

#[test]
fn filter_with_closure_predicate_and_failure() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .filter(pred(|ex| {
                let n: i64 = ex.body_as()?;
                Ok(n > 5)
            }))
            .to("mock:big")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:start", 10).unwrap();
    t.send_body("direct:start", 3).unwrap();
    // predicate failure fails the exchange
    assert!(t.send_body("direct:start", "abc").is_err());
    assert_eq!(ctx.mock_endpoint("mock:big").unwrap().received_bodies(), vec![Value::from(10)]);
}

#[test]
fn choice_when_otherwise() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .choice()
            .when(header("type").is_equal_to("a"))
            .to("mock:a")
            .when(header("type").is_equal_to("b"))
            .to("mock:b")
            .otherwise()
            .to("mock:other")
            .end()
            .to("mock:after");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body_and_header("direct:start", 1, "type", "a").unwrap();
    t.send_body_and_header("direct:start", 2, "type", "b").unwrap();
    t.send_body_and_header("direct:start", 3, "type", "c").unwrap();
    t.send_body("direct:start", 4).unwrap();
    assert_eq!(ctx.mock_endpoint("a").unwrap().received_bodies(), vec![Value::from(1)]);
    assert_eq!(ctx.mock_endpoint("b").unwrap().received_bodies(), vec![Value::from(2)]);
    assert_eq!(ctx.mock_endpoint("other").unwrap().received_bodies(), vec![Value::from(3), Value::from(4)]);
    assert_eq!(ctx.mock_endpoint("after").unwrap().received_counter(), 4);
}

#[test]
fn choice_when_after_nested_blocks() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .choice()
            .when(header("split").is_equal_to(true))
            .split(body())
            .to("mock:part")
            // no end(): when() closes the split
            .when(header("filter").is_equal_to(true))
            .filter(body().is_equal_to("keep"))
            .to("mock:kept")
            .end()
            .to("mock:filter-branch")
            .end_choice()
            .otherwise()
            .to("mock:other")
            .end()
            .to("mock:after");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body_and_header("direct:start", Value::from(vec!["x", "y"]), "split", true).unwrap();
    t.send_body_and_header("direct:start", "keep", "filter", true).unwrap();
    t.send_body_and_header("direct:start", "drop", "filter", true).unwrap();
    t.send_body("direct:start", "o").unwrap();
    assert_eq!(ctx.mock_endpoint("part").unwrap().received_bodies(), vec![Value::from("x"), Value::from("y")]);
    assert_eq!(ctx.mock_endpoint("kept").unwrap().received_bodies(), vec![Value::from("keep")]);
    assert_eq!(ctx.mock_endpoint("filter-branch").unwrap().received_counter(), 2);
    assert_eq!(ctx.mock_endpoint("other").unwrap().received_bodies(), vec![Value::from("o")]);
    assert_eq!(ctx.mock_endpoint("after").unwrap().received_counter(), 4);
}

#[test]
fn nested_choices() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .choice()
            .when(header("a").is_equal_to(1))
            .choice()
            .when(header("b").is_equal_to(1))
            .to("mock:a1b1")
            .otherwise()
            .to("mock:a1")
            .end()
            .otherwise()
            .to("mock:none")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body_and_headers("direct:start", "x", vec![("a", Value::from(1)), ("b", Value::from(1))]).unwrap();
    t.send_body_and_header("direct:start", "y", "a", 1).unwrap();
    t.send_body("direct:start", "z").unwrap();
    assert_eq!(ctx.mock_endpoint("a1b1").unwrap().received_bodies(), vec![Value::from("x")]);
    assert_eq!(ctx.mock_endpoint("a1").unwrap().received_bodies(), vec![Value::from("y")]);
    assert_eq!(ctx.mock_endpoint("none").unwrap().received_bodies(), vec![Value::from("z")]);
}

#[test]
fn choice_precondition_selects_branch_at_startup() {
    let ctx = CamelContext::new();
    ctx.properties().set("mode", "fast");
    ctx.add_routes(|b| {
        b.from("direct:start")
            .choice()
            .precondition()
            .when(simple("{{mode}}").is_equal_to("slow"))
            .to("mock:slow")
            .when(simple("{{mode}}").is_equal_to("fast"))
            .to("mock:fast")
            .otherwise()
            .to("mock:other")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "x").unwrap();
    assert_eq!(ctx.mock_endpoint("fast").unwrap().received_counter(), 1);
    assert_eq!(ctx.mock_endpoint("slow").unwrap().received_counter(), 0);
    let dump = ctx.route("route1").map(|r| r.definition_dump.clone()).unwrap_or_default();
    assert!(dump.contains("choice"), "{}", dump);
}

#[test]
fn when_outside_choice_is_build_error() {
    let ctx = CamelContext::new();
    let r = ctx.add_routes(|b| {
        b.from("direct:start").when(constant(true)).to("mock:x");
    });
    assert!(r.is_err());
}

#[test]
fn pipeline_block() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .multicast()
            .pipeline()
            .set_body(body().append("-1"))
            .to("mock:p1")
            .end()
            .pipeline()
            .set_body(body().append("-2"))
            .to("mock:p2")
            .end()
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "x").unwrap();
    assert_eq!(ctx.mock_endpoint("p1").unwrap().received_bodies(), vec![Value::from("x-1")]);
    assert_eq!(ctx.mock_endpoint("p2").unwrap().received_bodies(), vec![Value::from("x-2")]);
}

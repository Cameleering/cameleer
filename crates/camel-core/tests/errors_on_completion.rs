use camel_core::prelude::*;
use camel_core::processor::errors::*;

#[test]
fn route_scoped_on_completion_modes() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:always").on_completion().to("mock:always").end().to("mock:a");
        b.from("direct:complete").on_completion().on_complete_only().to("mock:complete").end().process(|ex: &mut Exchange| {
            if ex.body().as_str() == Some("fail") {
                Err(CamelError::io("boom"))
            } else {
                Ok(())
            }
        });
        b.from("direct:failure").on_completion().on_failure_only().to("mock:failure").end().process(|ex: &mut Exchange| {
            if ex.body().as_str() == Some("fail") {
                Err(CamelError::io("boom"))
            } else {
                Ok(())
            }
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:always", "x").unwrap();
    t.send_body("direct:complete", "ok").unwrap();
    assert!(t.send_body("direct:complete", "fail").is_err());
    t.send_body("direct:failure", "ok").unwrap();
    assert!(t.send_body("direct:failure", "fail").is_err());

    let always = ctx.mock_endpoint("mock:always").unwrap();
    assert_eq!(always.received_counter(), 1);
    assert_eq!(always.exchange(0).unwrap().property(props::ON_COMPLETION), Some(&Value::Bool(true)));
    assert_eq!(ctx.mock_endpoint("mock:complete").unwrap().received_bodies(), vec![Value::from("ok")]);
    let failure = ctx.mock_endpoint("mock:failure").unwrap();
    assert_eq!(failure.received_bodies(), vec![Value::from("fail")]);
    // the copy has the exception moved to CamelExceptionCaught
    let f = failure.exchange(0).unwrap();
    assert!(f.exception.is_none());
    assert_eq!(caught_exception(&f).unwrap().message, "boom");
}

#[test]
fn on_completion_runs_after_route_on_a_copy() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .on_completion()
            .set_body(constant("changed by onCompletion"))
            .to("mock:done")
            .end()
            .set_body(constant("reply"))
            .to("mock:result");
    })
    .unwrap();
    ctx.start().unwrap();
    let reply = ctx.create_producer_template().request_body("direct:start", "x").unwrap();
    assert_eq!(reply, Value::from("reply"));
    let done = ctx.mock_endpoint("mock:done").unwrap();
    assert_eq!(done.received_bodies(), vec![Value::from("changed by onCompletion")]);
    // ran after the route finished
    let r = ctx.mock_endpoint("mock:result").unwrap().exchange(0).unwrap();
    assert!(r.property(props::ON_COMPLETION).is_none());
}

#[test]
fn mode_before_consumer_changes_reply() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").on_completion().mode_before_consumer().set_body(body().append(" + completion")).end().set_body(constant("reply"));
    })
    .unwrap();
    ctx.start().unwrap();
    let reply = ctx.create_producer_template().request_body("direct:start", "x").unwrap();
    assert_eq!(reply, Value::from("reply + completion"));
}

#[test]
fn on_completion_on_when() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").on_completion().on_when(body().is_equal_to("yes")).to("mock:done").end().to("mock:result");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:start", "yes").unwrap();
    t.send_body("direct:start", "no").unwrap();
    assert_eq!(ctx.mock_endpoint("mock:done").unwrap().received_bodies(), vec![Value::from("yes")]);
}

#[test]
fn builder_scoped_on_completion_runs_once_and_route_scoped_overrides() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.on_completion().to("mock:global");
        b.from("direct:a").to("direct:b").to("mock:a");
        b.from("direct:b").to("mock:b");
        b.from("direct:c").on_completion().to("mock:local").end().to("mock:c");
    })
    .unwrap();
    ctx.add_routes(|b| {
        b.from("direct:other").to("mock:other");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:a", 1).unwrap();
    t.send_body("direct:c", 2).unwrap();
    t.send_body("direct:other", 3).unwrap();
    assert_eq!(ctx.mock_endpoint("mock:global").unwrap().received_bodies(), vec![Value::from(1)]);
    assert_eq!(ctx.mock_endpoint("mock:local").unwrap().received_bodies(), vec![Value::from(2)]);
}

#[test]
fn handled_exception_counts_as_complete() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.error_handler(dead_letter_channel("mock:dead"));
        b.from("direct:start")
            .on_completion()
            .on_complete_only()
            .to("mock:complete")
            .end()
            .on_completion()
            .on_failure_only()
            .to("mock:failed")
            .end()
            .throw_exception("IOException", "x");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", 1).unwrap();
    assert_eq!(ctx.mock_endpoint("mock:complete").unwrap().received_counter(), 1);
    assert_eq!(ctx.mock_endpoint("mock:failed").unwrap().received_counter(), 0);
}

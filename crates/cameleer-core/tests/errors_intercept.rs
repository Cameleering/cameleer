use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use cameleer_core::prelude::*;

#[test]
fn intercept_every_step() {
    let ctx = CamelContext::new();
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    ctx.add_routes(move |b| {
        b.intercept().process(move |_: &mut Exchange| {
            c.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        b.from("direct:start").set_header("a", constant(1)).to("mock:a").to("mock:b");
    })
    .unwrap();
    ctx.add_routes(|b| {
        b.from("direct:other").to("mock:other");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:start", 1).unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 3);
    // other builder not intercepted
    t.send_body("direct:other", 1).unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 3);
}

#[test]
fn intercept_when_and_stop() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.intercept().on_when(body().is_equal_to("stop")).to("mock:intercepted").stop();
        b.from("direct:start").to("mock:a").to("mock:b");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:start", "go").unwrap();
    t.send_body("direct:start", "stop").unwrap();
    assert_eq!(ctx.mock_endpoint("mock:a").unwrap().received_bodies(), vec![Value::from("go")]);
    assert_eq!(ctx.mock_endpoint("mock:b").unwrap().received_bodies(), vec![Value::from("go")]);
    assert_eq!(ctx.mock_endpoint("mock:intercepted").unwrap().received_counter(), 1);
}

#[test]
fn intercept_from_pattern() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.intercept_from("direct:in*").to("mock:intercepted");
        b.from("direct:input1").to("mock:result");
        b.from("direct:other").to("mock:result");
        b.from("direct:in2").to("mock:result");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:input1", 1).unwrap();
    t.send_body("direct:other", 2).unwrap();
    t.send_body("direct:in2", 3).unwrap();
    let i = ctx.mock_endpoint("mock:intercepted").unwrap();
    assert_eq!(i.received_bodies(), vec![Value::from(1), Value::from(3)]);
    assert_eq!(i.exchange(0).unwrap().header(props::INTERCEPTED_ENDPOINT), Some(&Value::from("direct://input1")));
    assert_eq!(ctx.mock_endpoint("mock:result").unwrap().received_counter(), 3);
}

#[test]
fn intercept_from_stop() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.intercept_from("*").on_when(header("block").is_equal_to(true)).to("mock:blocked").stop();
        b.from("direct:start").to("mock:result");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body_and_header("direct:start", 1, "block", true).unwrap();
    t.send_body_and_header("direct:start", 2, "block", false).unwrap();
    assert_eq!(ctx.mock_endpoint("mock:result").unwrap().received_bodies(), vec![Value::from(2)]);
    assert_eq!(ctx.mock_endpoint("mock:blocked").unwrap().received_bodies(), vec![Value::from(1)]);
}

#[test]
fn intercept_send_to_endpoint() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.intercept_send_to_endpoint("mock:fo*").to("mock:detour");
        b.intercept_send_to_endpoint("mock:skip").skip_send_to_original_endpoint().set_body(constant("skipped")).to("mock:instead");
        b.from("direct:start").to("mock:foo").to("mock:bar").to("mock:skip").to_d("mock:${header.dyn}").to("mock:end");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body_and_header("direct:start", "x", "dyn", "four").unwrap();
    let count = |m: &str| ctx.mock_endpoint(m).unwrap().received_counter();
    assert_eq!(count("mock:foo"), 1);
    assert_eq!(count("mock:bar"), 1);
    assert_eq!(count("mock:skip"), 0);
    assert_eq!(count("mock:instead"), 1);
    // to("mock:foo") and toD("mock:four") are intercepted
    assert_eq!(count("mock:detour"), 2);
    let d = ctx.mock_endpoint("mock:detour").unwrap();
    assert_eq!(d.exchange(0).unwrap().header(props::INTERCEPTED_ENDPOINT), Some(&Value::from("mock://foo")));
    assert_eq!(d.exchange(1).unwrap().header(props::INTERCEPTED_ENDPOINT), Some(&Value::from("mock://four")));
    assert_eq!(ctx.mock_endpoint("mock:end").unwrap().received_bodies(), vec![Value::from("skipped")]);
}

#[test]
fn intercept_send_when() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.intercept_send_to_endpoint("mock:target").on_when(body().is_equal_to("secret")).skip_send_to_original_endpoint().to("mock:vault");
        b.from("direct:start").to("mock:target");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:start", "public").unwrap();
    t.send_body("direct:start", "secret").unwrap();
    assert_eq!(ctx.mock_endpoint("mock:target").unwrap().received_bodies(), vec![Value::from("public")]);
    assert_eq!(ctx.mock_endpoint("mock:vault").unwrap().received_bodies(), vec![Value::from("secret")]);
    let target = ctx.mock_endpoint("mock:target").unwrap().exchange(0).unwrap();
    assert!(target.header(props::INTERCEPTED_ENDPOINT).is_none());
}

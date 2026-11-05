use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use cameleer_core::prelude::*;

fn fail_on(body: &'static str, kind: &'static str) -> impl Fn(&mut Exchange) -> Result<()> + Send + Sync {
    move |ex: &mut Exchange| {
        if ex.body().as_str() == Some(body) {
            Err(CamelError::new(kind, format!("failed on {}", body)))
        } else {
            Ok(())
        }
    }
}

fn routes(b: &mut RouteBuilder) {
    b.from("direct:start")
        .do_try()
        .process(fail_on("io", "FileNotFoundException"))
        .process(fail_on("arg", "IllegalArgumentException"))
        .process(fail_on("other", "TimeoutException"))
        .to("mock:try")
        .do_catch(&["IOException"])
        .to("mock:catchIo")
        .do_catch(&["IllegalArgumentException", "IllegalStateException"])
        .to("mock:catchIllegal")
        .do_finally()
        .to("mock:finally")
        .end_do_try()
        .to("mock:result");
}

#[test]
fn try_catch_finally() {
    let ctx = CamelContext::new();
    ctx.add_routes(routes).unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:start", "ok").unwrap();
    t.send_body("direct:start", "io").unwrap();
    t.send_body("direct:start", "arg").unwrap();
    let e = t.send_body("direct:start", "other").unwrap_err();
    assert!(e.cause.unwrap().is_a("TimeoutException"));
    let bodies = |m: &str| ctx.mock_endpoint(m).unwrap().received_bodies();
    assert_eq!(bodies("mock:try"), vec![Value::from("ok")]);
    assert_eq!(bodies("mock:catchIo"), vec![Value::from("io")]);
    assert_eq!(bodies("mock:catchIllegal"), vec![Value::from("arg")]);
    assert_eq!(bodies("mock:finally"), vec![Value::from("ok"), Value::from("io"), Value::from("arg"), Value::from("other")]);
    assert_eq!(bodies("mock:result"), vec![Value::from("ok"), Value::from("io"), Value::from("arg")]);
    let caught = ctx.mock_endpoint("mock:catchIo").unwrap().exchange(0).unwrap();
    assert_eq!(caught_exception(&caught).unwrap().kind, "FileNotFoundException");
    assert!(caught.exception.is_none());
}

#[test]
fn catch_on_when_and_cause_chain() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .do_try()
            .process(|_: &mut Exchange| Err(CamelError::runtime("wrapper").with_cause(CamelError::io("root"))))
            .do_catch(&["IOException"])
            .on_when(header("special").is_equal_to(true))
            .to("mock:special")
            .do_catch(&["IOException"])
            .to("mock:io")
            .end()
            .to("mock:result");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body_and_header("direct:start", 1, "special", true).unwrap();
    t.send_body_and_header("direct:start", 2, "special", false).unwrap();
    assert_eq!(ctx.mock_endpoint("mock:special").unwrap().received_bodies(), vec![Value::from(1)]);
    assert_eq!(ctx.mock_endpoint("mock:io").unwrap().received_bodies(), vec![Value::from(2)]);
    assert_eq!(ctx.mock_endpoint("mock:result").unwrap().received_counter(), 2);
}

#[test]
fn nested_do_try() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .do_try()
            .to("mock:outerTry")
            .do_try()
            .throw_exception("IllegalArgumentException", "inner")
            .do_catch(&["IOException"])
            .to("mock:innerCatch")
            .do_finally()
            .to("mock:innerFinally")
            .end_do_try()
            .to("mock:afterInner")
            .do_catch(&["IllegalArgumentException"])
            .to("mock:outerCatch")
            .throw_exception("IllegalStateException", "from catch")
            .end_do_try()
            .to("mock:result");
    })
    .unwrap();
    ctx.start().unwrap();
    let e = ctx.create_producer_template().send_body("direct:start", 1).unwrap_err();
    assert_eq!(e.cause.unwrap().message, "from catch");
    let count = |m: &str| ctx.mock_endpoint(m).unwrap().received_counter();
    assert_eq!(count("mock:outerTry"), 1);
    assert_eq!(count("mock:innerCatch"), 0);
    assert_eq!(count("mock:innerFinally"), 1);
    assert_eq!(count("mock:afterInner"), 0);
    assert_eq!(count("mock:outerCatch"), 1);
    assert_eq!(count("mock:result"), 0);
}

#[test]
fn error_handler_does_not_interfere_inside_try() {
    let ctx = CamelContext::new();
    let attempts = Arc::new(AtomicUsize::new(0));
    let a = attempts.clone();
    ctx.add_routes(move |b| {
        b.error_handler(dead_letter_channel("mock:dead").maximum_redeliveries(3).redelivery_delay(0));
        b.on_exception(&["IOException"]).handled(true).to("mock:onException");
        b.from("direct:start")
            .do_try()
            .process(move |_: &mut Exchange| {
                a.fetch_add(1, Ordering::SeqCst);
                Err(CamelError::io("x"))
            })
            .do_catch(&["IOException"])
            .to("mock:catch")
            .end_do_try();
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", 1).unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert_eq!(ctx.mock_endpoint("mock:catch").unwrap().received_counter(), 1);
    assert_eq!(ctx.mock_endpoint("mock:dead").unwrap().received_counter(), 0);
    assert_eq!(ctx.mock_endpoint("mock:onException").unwrap().received_counter(), 0);
}

#[test]
fn uncaught_goes_to_error_handler_after_finally() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.error_handler(dead_letter_channel("mock:dead"));
        b.from("direct:start")
            .do_try()
            .throw_exception("IOException", "x")
            .do_catch(&["IllegalArgumentException"])
            .to("mock:catch")
            .do_finally()
            .set_header("finally", constant(true))
            .end_do_try()
            .to("mock:result");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", 1).unwrap();
    let dead = ctx.mock_endpoint("mock:dead").unwrap();
    assert_eq!(dead.received_counter(), 1);
    assert_eq!(dead.exchange(0).unwrap().header("finally"), Some(&Value::Bool(true)));
    assert_eq!(ctx.mock_endpoint("mock:catch").unwrap().received_counter(), 0);
}

#[test]
fn catch_all_and_sub_route_failure() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").do_try().to("direct:sub").do_catch(&[]).to("mock:catch").end().to("mock:result");
        b.from("direct:sub").error_handler(default_error_handler().log_exhausted(false)).throw_exception("MyCustomError", "custom");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", 1).unwrap();
    assert_eq!(ctx.mock_endpoint("mock:catch").unwrap().received_counter(), 1);
    let r = ctx.mock_endpoint("mock:result").unwrap().exchange(0).unwrap();
    assert_eq!(caught_exception(&r).unwrap().kind, "MyCustomError");
    assert!(r.property(props::REDELIVERY_EXHAUSTED).is_none());
}

#[test]
fn finally_error_replaces_and_try_without_catch() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").do_try().to("mock:try").do_finally().throw_exception("IllegalStateException", "finally failed").end_do_try();
    })
    .unwrap();
    ctx.start().unwrap();
    let e = ctx.create_producer_template().send_body("direct:start", 1).unwrap_err();
    assert_eq!(e.cause.unwrap().message, "finally failed");
    assert_eq!(ctx.mock_endpoint("mock:try").unwrap().received_counter(), 1);
}

#[test]
fn do_catch_without_try_is_build_error() {
    let ctx = CamelContext::new();
    assert!(ctx
        .add_routes(|b| {
            b.from("direct:start").do_catch(&["IOException"]).to("mock:x");
        })
        .is_err());
}

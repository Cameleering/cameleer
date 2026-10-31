use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use camel_core::builder::exception_message;
use camel_core::prelude::*;

#[test]
fn handled_stops_routing_and_clears_exception() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.on_exception(&["IOException"]).handled(true).set_body(exception_message().prepend("Error: ")).to("mock:error").end();
        b.from("direct:start").to("mock:before").throw_exception("IOException", "disk gone").to("mock:after");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let reply = t.request_body("direct:start", "x").unwrap();
    assert_eq!(reply, Value::from("Error: disk gone"));
    let err = ctx.mock_endpoint("mock:error").unwrap();
    assert_eq!(err.received_bodies(), vec![Value::from("Error: disk gone")]);
    let e = err.exchange(0).unwrap();
    assert_eq!(e.property(props::EXCEPTION_CAUGHT).unwrap().get("message").unwrap().as_str(), Some("disk gone"));
    assert_eq!(ctx.mock_endpoint("mock:after").unwrap().received_counter(), 0);
    let ex = t.send_with("direct:start", ExchangePattern::InOnly, |ex| ex.set_body("y"));
    assert!(ex.exception.is_none());
    assert_eq!(ex.property(props::ERRORHANDLER_HANDLED), Some(&Value::Bool(true)));
}

#[test]
fn continued_resumes_with_next_step() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.on_exception(&["IllegalArgumentException"]).continued(true).to("mock:continued");
        b.from("direct:start").throw_exception("IllegalArgumentException", "ignore me").set_body(constant("after")).to("mock:after");
    })
    .unwrap();
    ctx.start().unwrap();
    let reply = ctx.create_producer_template().request_body("direct:start", "x").unwrap();
    assert_eq!(reply, Value::from("after"));
    assert_eq!(ctx.mock_endpoint("mock:continued").unwrap().received_counter(), 1);
    let after = ctx.mock_endpoint("mock:after").unwrap().exchange(0).unwrap();
    assert_eq!(caught_exception(&after).unwrap().message, "ignore me");
}

#[test]
fn not_handled_runs_outputs_and_keeps_exception() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.on_exception(&["IOException"]).to("mock:error");
        b.from("direct:start").throw_exception("IOException", "x").to("mock:after");
    })
    .unwrap();
    ctx.start().unwrap();
    let e = ctx.create_producer_template().send_body("direct:start", "x").unwrap_err();
    assert!(e.cause.unwrap().is_a("IOException"));
    assert_eq!(ctx.mock_endpoint("mock:error").unwrap().received_counter(), 1);
    assert_eq!(ctx.mock_endpoint("mock:after").unwrap().received_counter(), 0);
}

#[test]
fn closest_match_wins() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.on_exception(&["Exception"]).handled(true).to("mock:exception");
        b.on_exception(&["IOException"]).handled(true).to("mock:io");
        b.on_exception(&["IllegalArgumentException", "IllegalStateException"]).handled(true).to("mock:illegal");
        b.from("direct:start").process(|ex: &mut Exchange| {
            let kind: String = ex.body_as()?;
            Err(CamelError::new(kind, "x"))
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for k in ["FileNotFoundException", "IOException", "NumberFormatException", "IllegalStateException", "NullPointerException", "ParseException"] {
        t.send_body("direct:start", k).unwrap();
    }
    assert_eq!(ctx.mock_endpoint("mock:io").unwrap().received_bodies(), vec![Value::from("FileNotFoundException"), Value::from("IOException")]);
    assert_eq!(
        ctx.mock_endpoint("mock:illegal").unwrap().received_bodies(),
        vec![Value::from("NumberFormatException"), Value::from("IllegalStateException")]
    );
    assert_eq!(
        ctx.mock_endpoint("mock:exception").unwrap().received_bodies(),
        vec![Value::from("NullPointerException"), Value::from("ParseException")]
    );
}

#[test]
fn cause_chain_matched_from_the_bottom() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.on_exception(&["RuntimeException"]).handled(true).to("mock:runtime");
        b.on_exception(&["IOException"]).handled(true).to("mock:io");
        b.from("direct:start").process(|_: &mut Exchange| Err(CamelError::runtime("wrapper").with_cause(CamelError::new("ConnectException", "refused"))));
        b.from("direct:plain").process(|_: &mut Exchange| Err(CamelError::runtime("plain")));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:start", 1).unwrap();
    t.send_body("direct:plain", 2).unwrap();
    assert_eq!(ctx.mock_endpoint("mock:io").unwrap().received_bodies(), vec![Value::from(1)]);
    assert_eq!(ctx.mock_endpoint("mock:runtime").unwrap().received_bodies(), vec![Value::from(2)]);
    let io = ctx.mock_endpoint("mock:io").unwrap().exchange(0).unwrap();
    // the caught exception is the original (outer) exception
    assert_eq!(caught_exception(&io).unwrap().message, "wrapper");
}

#[test]
fn route_scoped_precedence_and_builder_isolation() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.on_exception(&["IOException"]).handled(true).to("mock:global");
        b.from("direct:scoped").on_exception(&["IOException"]).handled(true).to("mock:local").end().throw_exception("IOException", "a");
        b.from("direct:unscoped").throw_exception("IOException", "b");
    })
    .unwrap();
    ctx.add_routes(|b| {
        b.from("direct:other").throw_exception("IOException", "c");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:scoped", 1).unwrap();
    t.send_body("direct:unscoped", 2).unwrap();
    assert!(t.send_body("direct:other", 3).is_err());
    assert_eq!(ctx.mock_endpoint("mock:local").unwrap().received_bodies(), vec![Value::from(1)]);
    assert_eq!(ctx.mock_endpoint("mock:global").unwrap().received_bodies(), vec![Value::from(2)]);
}

#[test]
fn closer_builder_clause_beats_route_scoped_generic() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.on_exception(&["FileNotFoundException"]).handled(true).to("mock:global");
        b.from("direct:start").on_exception(&["Exception"]).handled(true).to("mock:local").end().throw_exception("FileNotFoundException", "a");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", 1).unwrap();
    assert_eq!(ctx.mock_endpoint("mock:global").unwrap().received_counter(), 1);
    assert_eq!(ctx.mock_endpoint("mock:local").unwrap().received_counter(), 0);
}

#[test]
fn on_when_filters_clauses() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.on_exception(&["IOException"]).on_when(header("vip").is_equal_to(true)).handled(true).to("mock:vip");
        b.on_exception(&["IOException"]).handled(true).to("mock:normal");
        b.from("direct:start").throw_exception("IOException", "x");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body_and_header("direct:start", 1, "vip", true).unwrap();
    t.send_body_and_header("direct:start", 2, "vip", false).unwrap();
    assert_eq!(ctx.mock_endpoint("mock:vip").unwrap().received_bodies(), vec![Value::from(1)]);
    assert_eq!(ctx.mock_endpoint("mock:normal").unwrap().received_bodies(), vec![Value::from(2)]);
}

#[test]
fn clause_redelivery_overrides_error_handler() {
    let ctx = CamelContext::new();
    let io_attempts = Arc::new(AtomicUsize::new(0));
    let other_attempts = Arc::new(AtomicUsize::new(0));
    let (a, o) = (io_attempts.clone(), other_attempts.clone());
    ctx.add_routes(move |b| {
        b.error_handler(dead_letter_channel("mock:dead").maximum_redeliveries(1).redelivery_delay(0));
        b.on_exception(&["IOException"]).maximum_redeliveries(4).redelivery_delay(0).on_redelivery(|ex: &mut Exchange| {
            ex.set_header("clauseRedelivery", true);
            Ok(())
        });
        b.from("direct:io").process(move |_: &mut Exchange| {
            a.fetch_add(1, Ordering::SeqCst);
            Err(CamelError::io("x"))
        });
        b.from("direct:other").process(move |_: &mut Exchange| {
            o.fetch_add(1, Ordering::SeqCst);
            Err(CamelError::illegal_argument("y"))
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    // clause without outputs + DLC: the DLC is the failure processor and handles it
    t.send_body("direct:io", 1).unwrap();
    t.send_body("direct:other", 2).unwrap();
    assert_eq!(io_attempts.load(Ordering::SeqCst), 5);
    assert_eq!(other_attempts.load(Ordering::SeqCst), 2);
    let dead = ctx.mock_endpoint("mock:dead").unwrap();
    assert_eq!(dead.received_counter(), 2);
    assert_eq!(dead.exchange(0).unwrap().header("clauseRedelivery"), Some(&Value::Bool(true)));
    assert_eq!(dead.exchange(0).unwrap().header(props::REDELIVERY_MAX_COUNTER), Some(&Value::from(4)));
}

#[test]
fn clause_handled_false_with_dead_letter_channel() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.error_handler(dead_letter_channel("mock:dead"));
        b.on_exception(&["IOException"]).handled(false).to("mock:io");
        b.from("direct:start").throw_exception("IOException", "x");
    })
    .unwrap();
    ctx.start().unwrap();
    assert!(ctx.create_producer_template().send_body("direct:start", 1).is_err());
    assert_eq!(ctx.mock_endpoint("mock:io").unwrap().received_counter(), 1);
    assert_eq!(ctx.mock_endpoint("mock:dead").unwrap().received_counter(), 0);
}

#[test]
fn handled_predicate() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.on_exception(&["IOException"]).handled(header("handle").is_equal_to("yes")).to("mock:io");
        b.from("direct:start").throw_exception("IOException", "x");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body_and_header("direct:start", 1, "handle", "yes").unwrap();
    assert!(t.send_body_and_header("direct:start", 2, "handle", "no").is_err());
    assert_eq!(ctx.mock_endpoint("mock:io").unwrap().received_counter(), 2);
}

#[test]
fn clause_use_original_message_and_retry_while() {
    let ctx = CamelContext::new();
    let attempts = Arc::new(AtomicUsize::new(0));
    let a = attempts.clone();
    ctx.add_routes(move |b| {
        b.from("direct:start")
            .on_exception(&["IOException"])
            .use_original_message()
            .retry_while(pred(|ex| Ok(ex.header(props::REDELIVERY_COUNTER).and_then(|v| v.as_i64()).unwrap_or(0) < 2)))
            .redelivery_delay(0)
            .handled(true)
            .to("mock:error")
            .end()
            .set_body(constant("modified"))
            .process(move |_: &mut Exchange| {
                a.fetch_add(1, Ordering::SeqCst);
                Err(CamelError::io("x"))
            });
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "original").unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    assert_eq!(ctx.mock_endpoint("mock:error").unwrap().received_bodies(), vec![Value::from("original")]);
}

#[test]
fn clause_outputs_can_use_full_dsl_and_failures_in_clause() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.on_exception(&["IOException"])
            .handled(true)
            .set_header("type", constant("io"))
            .do_try()
            .throw_exception("IllegalStateException", "inner")
            .do_catch(&["IllegalStateException"])
            .to("mock:innerCatch")
            .end_do_try()
            .to("mock:error");
        b.on_exception(&["IllegalArgumentException"]).handled(true).throw_exception("IllegalStateException", "clause failed");
        b.from("direct:start").throw_exception("IOException", "x");
        b.from("direct:broken").throw_exception("IllegalArgumentException", "y");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:start", 1).unwrap();
    assert_eq!(ctx.mock_endpoint("mock:innerCatch").unwrap().received_counter(), 1);
    assert_eq!(ctx.mock_endpoint("mock:error").unwrap().exchange(0).unwrap().header("type"), Some(&Value::from("io")));
    // a failure inside the clause outputs replaces the exception
    let e = t.send_body("direct:broken", 2).unwrap_err();
    assert_eq!(e.cause.unwrap().message, "clause failed");
}

#[test]
fn no_error_handler_ignores_on_exception() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.error_handler(no_error_handler());
        b.on_exception(&["IOException"]).handled(true).to("mock:io");
        b.from("direct:start").throw_exception("IOException", "x");
    })
    .unwrap();
    ctx.start().unwrap();
    assert!(ctx.create_producer_template().send_body("direct:start", 1).is_err());
    assert_eq!(ctx.mock_endpoint("mock:io").unwrap().received_counter(), 0);
}

#[test]
fn misplaced_clause_option_is_a_build_error() {
    let ctx = CamelContext::new();
    let r = ctx.add_routes(|b| {
        b.from("direct:start").handled(true).to("mock:x");
    });
    assert!(r.is_err());
}

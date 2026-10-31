use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use camel_core::prelude::*;
use camel_core::processor::errors::*;

/// A processor failing the first `fail_times` invocations with the given error.
fn flaky(counter: Arc<AtomicUsize>, fail_times: usize, kind: &'static str) -> impl Fn(&mut Exchange) -> Result<()> + Send + Sync {
    move |_ex: &mut Exchange| {
        let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
        if n <= fail_times {
            Err(CamelError::new(kind, format!("attempt {} failed", n)))
        } else {
            Ok(())
        }
    }
}

#[test]
fn default_error_handler_no_redelivery() {
    let ctx = CamelContext::new();
    let attempts = Arc::new(AtomicUsize::new(0));
    let a = attempts.clone();
    ctx.add_routes(move |b| {
        b.from("direct:start").process(flaky(a, 100, "IOException")).to("mock:result");
    })
    .unwrap();
    ctx.start().unwrap();
    let ex = ctx.create_producer_template().send_with("direct:start", ExchangePattern::InOnly, |ex| ex.set_body("x"));
    assert!(ex.exception.as_ref().unwrap().is_a("IOException"));
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert_eq!(ex.property(props::REDELIVERY_EXHAUSTED), Some(&Value::Bool(true)));
    assert_eq!(caught_exception(&ex).unwrap().kind, "IOException");
    assert_eq!(ctx.mock_endpoint("mock:result").unwrap().received_counter(), 0);
}

#[test]
fn redelivery_until_success_sets_headers() {
    let ctx = CamelContext::new();
    let attempts = Arc::new(AtomicUsize::new(0));
    let a = attempts.clone();
    ctx.add_routes(move |b| {
        b.error_handler(default_error_handler().maximum_redeliveries(3).redelivery_delay(0));
        b.from("direct:start").process(flaky(a, 2, "IOException")).to("mock:result");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:result").unwrap();
    mock.expected_message_count(1);
    mock.expected_header_received(props::REDELIVERED, true);
    mock.expected_header_received(props::REDELIVERY_COUNTER, 2);
    mock.expected_header_received(props::REDELIVERY_MAX_COUNTER, 3);
    ctx.create_producer_template().send_body("direct:start", "x").unwrap();
    mock.assert_is_satisfied().unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
}

#[test]
fn redelivery_is_per_step() {
    let ctx = CamelContext::new();
    let before = Arc::new(AtomicUsize::new(0));
    let failing = Arc::new(AtomicUsize::new(0));
    let (b1, f1) = (before.clone(), failing.clone());
    ctx.add_routes(move |b| {
        b.from("direct:start")
            .error_handler(default_error_handler().maximum_redeliveries(2).redelivery_delay(0))
            .process(move |_: &mut Exchange| {
                b1.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .process(flaky(f1, 100, "IOException"));
    })
    .unwrap();
    ctx.start().unwrap();
    assert!(ctx.create_producer_template().send_body("direct:start", "x").is_err());
    assert_eq!(before.load(Ordering::SeqCst), 1);
    assert_eq!(failing.load(Ordering::SeqCst), 3);
}

#[test]
fn dead_letter_channel_moves_failed_exchange() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.error_handler(dead_letter_channel("mock:dead").maximum_redeliveries(2).redelivery_delay(0));
        b.from("direct:start").route_id("main").to("mock:flaky").to("mock:result");
    })
    .unwrap();
    ctx.start().unwrap();
    let flaky = ctx.mock_endpoint("mock:flaky").unwrap();
    flaky.when_any_exchange_received(|_: &mut Exchange| Err(CamelError::io("disk full")));
    let dead = ctx.mock_endpoint("mock:dead").unwrap();
    dead.expected_message_count(1);
    dead.expected_bodies_received(vec!["payload"]);
    dead.expected_header_received(props::REDELIVERY_COUNTER, 2);
    // handled: the caller does not see the exception
    let ex = ctx.create_producer_template().send_with("direct:start", ExchangePattern::InOnly, |ex| ex.set_body("payload"));
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
    dead.assert_is_satisfied().unwrap();
    assert_eq!(flaky.received_counter(), 3);
    assert_eq!(ctx.mock_endpoint("mock:result").unwrap().received_counter(), 0);

    let d = dead.exchange(0).unwrap();
    let caught = d.property(props::EXCEPTION_CAUGHT).unwrap();
    assert_eq!(caught.get("message").unwrap().as_str(), Some("disk full"));
    assert_eq!(caught.get("kind").unwrap().as_str(), Some("IOException"));
    assert_eq!(caught_exception(&d).unwrap(), CamelError::io("disk full"));
    assert_eq!(d.property(props::FAILURE_ENDPOINT).unwrap().as_str(), Some("mock:flaky"));
    assert_eq!(d.property(props::FAILURE_ROUTE_ID).unwrap().as_str(), Some("main"));
    assert_eq!(ex.property(props::ERRORHANDLER_HANDLED), Some(&Value::Bool(true)));
    assert!(ex.property(props::EXCEPTION_CAUGHT).is_some());
}

#[test]
fn dead_letter_channel_to_route_and_original_message() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.error_handler(dead_letter_channel("direct:dead").use_original_message().redelivery_delay(0));
        b.from("direct:start")
            .set_body(constant("changed"))
            .set_header("added", constant(1))
            .throw_exception("IllegalStateException", "bad state");
        b.from("direct:dead").to("mock:dead");
    })
    .unwrap();
    ctx.start().unwrap();
    let dead = ctx.mock_endpoint("mock:dead").unwrap();
    dead.expected_bodies_received(vec!["original"]);
    ctx.create_producer_template().send_body_and_header("direct:start", "original", "keep", "yes").unwrap();
    dead.assert_is_satisfied().unwrap();
    let d = dead.exchange(0).unwrap();
    assert_eq!(d.header("keep"), Some(&Value::from("yes")));
    assert!(d.header("added").is_none());
    assert_eq!(caught_exception(&d).unwrap().message, "bad state");
}

#[test]
fn dead_letter_use_original_body_keeps_headers() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .error_handler(dead_letter_channel("mock:dead").use_original_body())
            .set_body(constant("changed"))
            .set_header("added", constant(1))
            .throw_exception("IOException", "x");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "original").unwrap();
    let d = ctx.mock_endpoint("mock:dead").unwrap().exchange(0).unwrap();
    assert_eq!(d.body(), &Value::from("original"));
    assert_eq!(d.header("added"), Some(&Value::from(1)));
}

#[test]
fn dead_letter_new_exception_handling() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:handled").error_handler(dead_letter_channel("direct:deadFails")).throw_exception("IOException", "first");
        b.from("direct:unhandled")
            .error_handler(dead_letter_channel("direct:deadFails").dead_letter_handle_new_exception(false))
            .throw_exception("IOException", "first");
        b.from("direct:deadFails").error_handler(no_error_handler()).throw_exception("IllegalStateException", "dlc broken");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:handled", "x").unwrap();
    let e = t.send_body("direct:unhandled", "x").unwrap_err();
    assert_eq!(e.cause.unwrap().message, "dlc broken");
}

#[test]
fn retry_while_and_on_redelivery() {
    let ctx = CamelContext::new();
    let attempts = Arc::new(AtomicUsize::new(0));
    let a = attempts.clone();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    ctx.add_routes(move |b| {
        b.error_handler(
            dead_letter_channel("mock:dead")
                .redelivery_delay(0)
                .retry_while(pred(|ex| Ok(ex.header(props::REDELIVERY_COUNTER).and_then(|v| v.as_i64()).unwrap_or(0) < 4)))
                .on_redelivery(move |ex: &mut Exchange| {
                    s.lock().unwrap().push(ex.header(props::REDELIVERY_COUNTER).cloned().unwrap());
                    ex.set_header("onRedelivery", true);
                    Ok(())
                }),
        );
        b.from("direct:start").process(flaky(a, 100, "IOException"));
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "x").unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 5);
    assert_eq!(*seen.lock().unwrap(), vec![Value::from(1), Value::from(2), Value::from(3), Value::from(4)]);
    let d = ctx.mock_endpoint("mock:dead").unwrap().exchange(0).unwrap();
    assert_eq!(d.header("onRedelivery"), Some(&Value::Bool(true)));
    // no max counter header with retryWhile
    assert!(d.header(props::REDELIVERY_MAX_COUNTER).is_none());
}

#[test]
fn on_exception_occurred_and_on_prepare_failure() {
    let ctx = CamelContext::new();
    let occurred = Arc::new(AtomicUsize::new(0));
    let o = occurred.clone();
    ctx.add_routes(move |b| {
        b.error_handler(
            dead_letter_channel("mock:dead")
                .maximum_redeliveries(2)
                .redelivery_delay(0)
                .on_exception_occurred(move |ex: &mut Exchange| {
                    assert!(ex.exception.is_some());
                    o.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
                .on_prepare_failure(|ex: &mut Exchange| {
                    let msg = caught_exception(ex).map(|e| e.message).unwrap_or_default();
                    ex.set_header("failureReason", msg);
                    Ok(())
                }),
        );
        b.from("direct:start").throw_exception("IOException", "boom");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "x").unwrap();
    assert_eq!(occurred.load(Ordering::SeqCst), 3);
    let d = ctx.mock_endpoint("mock:dead").unwrap().exchange(0).unwrap();
    assert_eq!(d.header("failureReason"), Some(&Value::from("boom")));
}

#[test]
fn error_handler_precedence_route_builder_context() {
    let ctx = CamelContext::new();
    ctx.set_error_handler(dead_letter_channel("mock:contextDead"));
    assert!(ctx.context_error_handler().unwrap().is_dead_letter_channel());
    ctx.add_routes(|b| {
        b.from("direct:ctx").throw_exception("IOException", "ctx");
    })
    .unwrap();
    ctx.add_routes(|b| {
        b.error_handler(dead_letter_channel("mock:builderDead"));
        b.from("direct:builder").throw_exception("IOException", "builder");
        b.from("direct:route").error_handler(dead_letter_channel("mock:routeDead")).throw_exception("IOException", "route");
        b.from("direct:none").error_handler(no_error_handler()).throw_exception("IOException", "none");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:ctx", 1).unwrap();
    t.send_body("direct:builder", 2).unwrap();
    t.send_body("direct:route", 3).unwrap();
    assert!(t.send_body("direct:none", 4).is_err());
    assert_eq!(ctx.mock_endpoint("mock:contextDead").unwrap().received_bodies(), vec![Value::from(1)]);
    assert_eq!(ctx.mock_endpoint("mock:builderDead").unwrap().received_bodies(), vec![Value::from(2)]);
    assert_eq!(ctx.mock_endpoint("mock:routeDead").unwrap().received_bodies(), vec![Value::from(3)]);
}

#[test]
fn sub_route_errors() {
    let ctx = CamelContext::new();
    let attempts = Arc::new(AtomicUsize::new(0));
    let a = attempts.clone();
    let attempts2 = Arc::new(AtomicUsize::new(0));
    let a2 = attempts2.clone();
    ctx.add_routes(move |b| {
        b.error_handler(dead_letter_channel("mock:dead").maximum_redeliveries(1).redelivery_delay(0));
        // sub route without error handler: the caller's error handler deals with it
        b.from("direct:a").to("direct:subNoEh").to("mock:a");
        b.from("direct:subNoEh").error_handler(no_error_handler()).process(flaky(a, 100, "IOException"));
        // sub route with default error handler: it handles (exhausts) it, caller does not redeliver
        b.from("direct:b").to("direct:subDefault").to("mock:b");
        b.from("direct:subDefault").error_handler(default_error_handler()).process(flaky(a2, 100, "IOException"));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:a", "a").unwrap();
    // initial + 1 redelivery of the `to(direct:subNoEh)` step
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(ctx.mock_endpoint("mock:dead").unwrap().received_counter(), 1);
    let e = t.send_body("direct:b", "b").unwrap_err();
    assert!(e.cause.unwrap().is_a("IOException"));
    assert_eq!(attempts2.load(Ordering::SeqCst), 1);
    assert_eq!(ctx.mock_endpoint("mock:dead").unwrap().received_counter(), 1);
    assert_eq!(ctx.mock_endpoint("mock:b").unwrap().received_counter(), 0);
}

#[test]
fn exponential_back_off_timing() {
    let ctx = CamelContext::new();
    let times = Arc::new(Mutex::new(Vec::<Instant>::new()));
    let t2 = times.clone();
    ctx.add_routes(move |b| {
        b.from("direct:start")
            .error_handler(
                default_error_handler()
                    .maximum_redeliveries(3)
                    .redelivery_delay(20)
                    .use_exponential_back_off()
                    .back_off_multiplier(2.0)
                    .maximum_redelivery_delay(50)
                    .log_exhausted(false),
            )
            .process(move |_: &mut Exchange| {
                t2.lock().unwrap().push(Instant::now());
                Err(CamelError::io("x"))
            });
    })
    .unwrap();
    ctx.start().unwrap();
    assert!(ctx.create_producer_template().send_body("direct:start", 1).is_err());
    let t = times.lock().unwrap();
    assert_eq!(t.len(), 4);
    let gaps: Vec<u128> = t.windows(2).map(|w| (w[1] - w[0]).as_millis()).collect();
    // 20, 40, 50 (capped)
    assert!(gaps[0] >= 20 && gaps[0] < 200, "{:?}", gaps);
    assert!(gaps[1] >= 40 && gaps[1] < 250, "{:?}", gaps);
    assert!(gaps[2] >= 50 && gaps[2] < 300, "{:?}", gaps);
}

#[test]
fn delay_pattern_and_forever() {
    let ctx = CamelContext::new();
    let attempts = Arc::new(AtomicUsize::new(0));
    let a = attempts.clone();
    ctx.add_routes(move |b| {
        b.from("direct:start")
            .error_handler(default_error_handler().maximum_redeliveries(-1).delay_pattern("1:0;3:5"))
            .process(flaky(a, 5, "IOException"))
            .to("mock:result");
    })
    .unwrap();
    ctx.start().unwrap();
    let start = Instant::now();
    ctx.create_producer_template().send_body("direct:start", 1).unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 6);
    // attempts 3,4,5 wait 5ms each
    assert!(start.elapsed() >= Duration::from_millis(15));
    let m = ctx.mock_endpoint("mock:result").unwrap();
    assert_eq!(m.received_counter(), 1);
    // forever: no max counter header
    assert!(m.exchange(0).unwrap().header(props::REDELIVERY_MAX_COUNTER).is_none());
}

#[test]
fn redelivery_inside_nested_blocks_only_once() {
    let ctx = CamelContext::new();
    let attempts = Arc::new(AtomicUsize::new(0));
    let a = attempts.clone();
    ctx.add_routes(move |b| {
        b.error_handler(dead_letter_channel("mock:dead").maximum_redeliveries(2).redelivery_delay(0));
        b.from("direct:start").step("s1").process(flaky(a, 100, "IOException")).end().to("mock:after");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", 1).unwrap();
    // the inner step is redelivered; the enclosing step's channel does not redeliver again
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    assert_eq!(ctx.mock_endpoint("mock:dead").unwrap().received_counter(), 1);
    assert_eq!(ctx.mock_endpoint("mock:after").unwrap().received_counter(), 0);
}

#[test]
fn logs_exhausted_failure_at_error() {
    let records = Arc::new(Mutex::new(Vec::<String>::new()));
    let r = records.clone();
    camel_core::log::add_sink(move |rec| {
        if rec.level == camel_core::log::Level::Error && rec.message.contains("unique-log-marker") {
            r.lock().unwrap().push(rec.message.clone());
        }
    });
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").throw_exception("IOException", "unique-log-marker");
        b.from("direct:quiet").error_handler(default_error_handler().log_exhausted(false)).throw_exception("IOException", "unique-log-marker quiet");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    assert!(t.send_body("direct:start", 1).is_err());
    assert!(t.send_body("direct:quiet", 1).is_err());
    let recs = records.lock().unwrap();
    assert_eq!(recs.len(), 1, "{:?}", recs);
    assert!(recs[0].contains("Exhausted after delivery attempt: 1"), "{}", recs[0]);
}

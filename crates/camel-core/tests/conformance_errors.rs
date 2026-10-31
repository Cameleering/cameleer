//! Conformance tests for error handling (Apache Camel 4 "Error Handler", "Exception Clause",
//! "Dead Letter Channel", "Try Catch Finally" and "OnCompletion" documentation).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use camel_core::builder::exception_message;
use camel_core::prelude::*;

fn bodies(ctx: &CamelContext, mock: &str) -> Vec<String> {
    ctx.mock_endpoint(mock).unwrap().received_bodies().iter().map(|v| v.to_string()).collect()
}

fn fail_with(kind: &'static str) -> impl Fn(&mut Exchange) -> Result<()> + Send + Sync {
    move |_ex: &mut Exchange| Err(CamelError::new(kind, format!("{} forced", kind)))
}

#[test]
fn exception_hierarchy_matching_closest_parent() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.on_exception(&["Exception"]).handled(true).to("mock:exception");
        b.on_exception(&["IOException"]).handled(true).to("mock:io");
        b.from("direct:fnf").process(fail_with("FileNotFoundException"));
        b.from("direct:connect").process(fail_with("ConnectException"));
        b.from("direct:npe").process(fail_with("NullPointerException"));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:fnf", "fnf").unwrap();
    t.send_body("direct:connect", "connect").unwrap();
    t.send_body("direct:npe", "npe").unwrap();
    assert_eq!(bodies(&ctx, "mock:io"), vec!["fnf", "connect"]);
    assert_eq!(bodies(&ctx, "mock:exception"), vec!["npe"]);
}

#[test]
fn custom_exception_kind_registered_under_parent() {
    camel_core::error::register_kind_parent("OrderFailedException", "IllegalStateException");
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.on_exception(&["IllegalStateException"]).handled(true).set_body(simple("handled: ${exception.message}")).to("mock:handled");
        b.from("direct:start").process(fail_with("OrderFailedException"));
    })
    .unwrap();
    ctx.start().unwrap();
    assert_eq!(ctx.create_producer_template().request_body("direct:start", "x").unwrap(), Value::from("handled: OrderFailedException forced"));
}

#[test]
fn not_handled_is_rethrown_to_producer_template_as_camel_execution_exception() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.on_exception(&["IOException"]).handled(false).to("mock:error");
        b.from("direct:start").process(fail_with("IOException"));
    })
    .unwrap();
    ctx.start().unwrap();
    let e = ctx.create_producer_template().request_body("direct:start", "x").unwrap_err();
    assert_eq!(e.kind, kinds::CAMEL_EXECUTION);
    let cause = e.cause.unwrap();
    assert_eq!(cause.kind, "IOException");
    assert_eq!(cause.message, "IOException forced");
    assert_eq!(ctx.mock_endpoint("mock:error").unwrap().received_counter(), 1);
}

#[test]
fn no_on_exception_default_error_handler_rethrows() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").process(fail_with("IllegalArgumentException")).to("mock:never");
    })
    .unwrap();
    ctx.start().unwrap();
    let ex = ctx.create_producer_template().send_with("direct:start", ExchangePattern::InOut, |e| e.set_body("x"));
    assert_eq!(ex.exception.as_ref().unwrap().kind, "IllegalArgumentException");
    assert_eq!(ctx.mock_endpoint("mock:never").unwrap().received_counter(), 0);
}

#[test]
fn handled_sets_reply_from_on_exception_route() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.on_exception(&["IllegalArgumentException"]).handled(true).set_body(exception_message().prepend("Sorry: "));
        b.from("direct:start").process(fail_with("IllegalArgumentException"));
    })
    .unwrap();
    ctx.start().unwrap();
    assert_eq!(ctx.create_producer_template().request_body("direct:start", "x").unwrap(), Value::from("Sorry: IllegalArgumentException forced"));
}

#[test]
fn continued_resumes_route_and_exception_cleared() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.on_exception(&["IllegalArgumentException"]).continued(true);
        b.from("direct:start").to("mock:a").process(fail_with("IllegalArgumentException")).to("mock:b");
    })
    .unwrap();
    ctx.start().unwrap();
    let ex = ctx.create_producer_template().send_with("direct:start", ExchangePattern::InOut, |e| e.set_body("x"));
    assert!(ex.exception.is_none());
    assert_eq!(bodies(&ctx, "mock:b"), vec!["x"]);
}

#[test]
fn on_exception_with_dead_letter_channel_on_exception_wins() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.error_handler(dead_letter_channel("mock:dead").maximum_redeliveries(2).redelivery_delay(0));
        b.on_exception(&["IOException"]).handled(true).to("mock:io");
        b.from("direct:io").process(fail_with("IOException"));
        b.from("direct:other").process(fail_with("IllegalStateException"));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:io", "io").unwrap();
    // DLC handles the exception, so the caller does not see it
    t.send_body("direct:other", "other").unwrap();
    assert_eq!(bodies(&ctx, "mock:io"), vec!["io"]);
    assert_eq!(bodies(&ctx, "mock:dead"), vec!["other"]);
    let dead = ctx.mock_endpoint("mock:dead").unwrap().exchange(0).unwrap();
    assert_eq!(dead.header(props::REDELIVERY_COUNTER), Some(&Value::Int(2)));
    assert_eq!(dead.header(props::REDELIVERED), Some(&Value::Bool(true)));
    assert_eq!(dead.header(props::REDELIVERY_MAX_COUNTER), Some(&Value::Int(2)));
    assert_eq!(caught_exception(&dead).unwrap().kind, "IllegalStateException");
    assert!(dead.exception.is_none(), "the DLC receives the exchange without exception set");
}

#[test]
fn redelivery_counter_headers_seen_by_processor() {
    let ctx = CamelContext::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    ctx.add_routes(move |b| {
        b.error_handler(default_error_handler().maximum_redeliveries(3).redelivery_delay(0));
        let s3 = s2.clone();
        b.from("direct:start").process(move |ex: &mut Exchange| {
            let c = ex.header(props::REDELIVERY_COUNTER).cloned();
            let mut s = s3.lock().unwrap();
            s.push(c);
            if s.len() < 3 {
                return Err(CamelError::io("flaky"));
            }
            ex.set_body("ok");
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let ex = ctx.create_producer_template().send_with("direct:start", ExchangePattern::InOut, |e| e.set_body("x"));
    assert!(ex.exception.is_none());
    assert_eq!(ex.body(), &Value::from("ok"));
    assert_eq!(*seen.lock().unwrap(), vec![None, Some(Value::Int(1)), Some(Value::Int(2))]);
    assert_eq!(ex.header(props::REDELIVERED), Some(&Value::Bool(true)));
}

#[test]
fn redelivery_exhausted_attempt_count() {
    let ctx = CamelContext::new();
    let count = Arc::new(Mutex::new(0));
    let c2 = count.clone();
    ctx.add_routes(move |b| {
        let c3 = c2.clone();
        b.on_exception(&["IOException"]).maximum_redeliveries(4).redelivery_delay(0).handled(true).to("mock:exhausted");
        b.from("direct:start").process(move |_ex: &mut Exchange| {
            *c3.lock().unwrap() += 1;
            Err(CamelError::io("always"))
        });
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "x").unwrap();
    assert_eq!(*count.lock().unwrap(), 5, "1 delivery + 4 redeliveries");
    let ex = ctx.mock_endpoint("mock:exhausted").unwrap().exchange(0).unwrap();
    assert_eq!(ex.header(props::REDELIVERY_COUNTER), Some(&Value::Int(4)));
}

#[test]
fn redelivery_only_retries_the_failing_processor() {
    // Camel redelivers at the point of failure, not from the start of the route
    let ctx = CamelContext::new();
    let fails = Arc::new(Mutex::new(0));
    let f2 = fails.clone();
    ctx.add_routes(move |b| {
        let f3 = f2.clone();
        b.error_handler(default_error_handler().maximum_redeliveries(2).redelivery_delay(0));
        b.from("direct:start").to("mock:before").process(move |_ex: &mut Exchange| {
            let mut n = f3.lock().unwrap();
            *n += 1;
            if *n < 3 {
                Err(CamelError::io("x"))
            } else {
                Ok(())
            }
        })
        .to("mock:after");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "x").unwrap();
    assert_eq!(ctx.mock_endpoint("mock:before").unwrap().received_counter(), 1);
    assert_eq!(ctx.mock_endpoint("mock:after").unwrap().received_counter(), 1);
}

#[test]
fn on_exception_in_direct_sub_route_handles_and_caller_is_not_rehandled() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:caller")
            .on_exception(&["Exception"])
            .handled(true)
            .to("mock:caller-error")
            .end()
            .to("direct:sub")
            .to("mock:caller-after");
        b.from("direct:sub")
            .on_exception(&["IOException"])
            .handled(true)
            .set_body(constant("handled in sub"))
            .to("mock:sub-error")
            .end()
            .process(fail_with("IOException"))
            .to("mock:sub-after");
    })
    .unwrap();
    ctx.start().unwrap();
    let reply = ctx.create_producer_template().request_body("direct:caller", "x").unwrap();
    assert_eq!(reply, Value::from("handled in sub"));
    assert_eq!(ctx.mock_endpoint("mock:sub-error").unwrap().received_counter(), 1);
    assert_eq!(ctx.mock_endpoint("mock:caller-error").unwrap().received_counter(), 0);
    assert_eq!(ctx.mock_endpoint("mock:sub-after").unwrap().received_counter(), 0);
    // Camel: the exchange is marked as handled (CamelErrorHandlerHandled) so the caller stops too
    assert_eq!(ctx.mock_endpoint("mock:caller-after").unwrap().received_counter(), 0);
}

#[test]
fn sub_route_with_no_error_handler_lets_caller_handle() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:caller").on_exception(&["IOException"]).handled(true).to("mock:caller-error").end().to("direct:sub").to("mock:after");
        b.from("direct:sub").error_handler(no_error_handler()).process(fail_with("IOException"));
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:caller", "x").unwrap();
    assert_eq!(ctx.mock_endpoint("mock:caller-error").unwrap().received_counter(), 1);
    assert_eq!(ctx.mock_endpoint("mock:after").unwrap().received_counter(), 0);
}

#[test]
fn global_on_exception_applies_to_sub_route_where_failure_happened() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.on_exception(&["IOException"]).handled(true).set_body(simple("failed in ${exchangeProperty.CamelFailureRouteId}")).to("mock:error");
        b.from("direct:caller").route_id("caller").to("direct:sub").to("mock:after");
        b.from("direct:sub").route_id("sub").process(fail_with("IOException"));
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:caller", "x").unwrap();
    assert_eq!(bodies(&ctx, "mock:error"), vec!["failed in sub"]);
    assert_eq!(ctx.mock_endpoint("mock:after").unwrap().received_counter(), 0);
}

#[test]
fn on_completion_runs_after_failure() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.on_completion().to("mock:completion");
        b.on_completion().on_failure_only().set_body(simple("failed: ${exception.message}")).to("mock:failure");
        b.on_completion().on_complete_only().to("mock:success");
        b.from("direct:start").choice().when(body().is_equal_to("bad")).throw_exception("IllegalArgumentException", "boom").end().to("mock:result");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:start", "good").unwrap();
    assert!(t.send_body("direct:start", "bad").is_err());
    let all = ctx.mock_endpoint("mock:completion").unwrap();
    assert!(all.wait_for_messages(2, Duration::from_secs(5)));
    let failure = ctx.mock_endpoint("mock:failure").unwrap();
    assert!(failure.wait_for_messages(1, Duration::from_secs(5)));
    assert_eq!(bodies(&ctx, "mock:failure"), vec!["failed: boom"]);
    let success = ctx.mock_endpoint("mock:success").unwrap();
    assert!(success.wait_for_messages(1, Duration::from_secs(5)));
    assert_eq!(bodies(&ctx, "mock:success"), vec!["good"]);
}

#[test]
fn on_completion_does_not_change_reply() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").on_completion().set_body(constant("from completion")).to("mock:completion").end().set_body(constant("reply"));
    })
    .unwrap();
    ctx.start().unwrap();
    assert_eq!(ctx.create_producer_template().request_body("direct:start", "x").unwrap(), Value::from("reply"));
    assert!(ctx.mock_endpoint("mock:completion").unwrap().wait_for_messages(1, Duration::from_secs(5)));
}

#[test]
fn use_original_message_on_exception() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.on_exception(&["IOException"]).handled(true).use_original_message().to("mock:dlq");
        b.from("direct:start").set_body(constant("modified")).set_header("added", constant(1)).process(fail_with("IOException"));
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body_and_header("direct:start", "original", "h", "v").unwrap();
    let ex = ctx.mock_endpoint("mock:dlq").unwrap().exchange(0).unwrap();
    assert_eq!(ex.body(), &Value::from("original"));
    assert_eq!(ex.header("h"), Some(&Value::from("v")));
    assert!(ex.header("added").is_none());
}

#[test]
fn use_original_message_with_dead_letter_channel() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.error_handler(dead_letter_channel("mock:dead").use_original_message().maximum_redeliveries(0));
        b.from("direct:start").transform(constant("changed")).process(fail_with("IOException"));
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "original").unwrap();
    assert_eq!(bodies(&ctx, "mock:dead"), vec!["original"]);
}

#[test]
fn do_try_catch_finally_order() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .do_try()
            .to("mock:try")
            .process(fail_with("IOException"))
            .to("mock:never")
            .do_catch(&["IOException"])
            .to("mock:catch")
            .do_finally()
            .to("mock:finally")
            .end_do_try()
            .to("mock:after");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "x").unwrap();
    for m in ["mock:try", "mock:catch", "mock:finally", "mock:after"] {
        assert_eq!(ctx.mock_endpoint(m).unwrap().received_counter(), 1, "{}", m);
    }
    assert_eq!(ctx.mock_endpoint("mock:never").unwrap().received_counter(), 0);
    let c = ctx.mock_endpoint("mock:catch").unwrap().exchange(0).unwrap();
    assert_eq!(caught_exception(&c).unwrap().kind, "IOException");
}

#[test]
fn do_try_uncaught_runs_finally_and_rethrows_to_route_error_handler() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.on_exception(&["IllegalStateException"]).handled(true).to("mock:handler");
        b.from("direct:start")
            .do_try()
            .process(fail_with("IllegalStateException"))
            .do_catch(&["IOException"])
            .to("mock:catch")
            .do_finally()
            .to("mock:finally")
            .end_do_try()
            .to("mock:after");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "x").unwrap();
    assert_eq!(ctx.mock_endpoint("mock:catch").unwrap().received_counter(), 0);
    assert_eq!(ctx.mock_endpoint("mock:finally").unwrap().received_counter(), 1);
    assert_eq!(ctx.mock_endpoint("mock:handler").unwrap().received_counter(), 1);
    assert_eq!(ctx.mock_endpoint("mock:after").unwrap().received_counter(), 0);
}

#[test]
fn do_try_is_not_subject_to_error_handler_redelivery() {
    // Camel: the error handler does not redeliver inside doTry; doCatch handles immediately
    let ctx = CamelContext::new();
    let count = Arc::new(Mutex::new(0));
    let c2 = count.clone();
    ctx.add_routes(move |b| {
        b.error_handler(default_error_handler().maximum_redeliveries(3).redelivery_delay(0));
        let c3 = c2.clone();
        b.from("direct:start")
            .do_try()
            .process(move |_ex: &mut Exchange| {
                *c3.lock().unwrap() += 1;
                Err(CamelError::io("x"))
            })
            .do_catch(&["IOException"])
            .to("mock:catch")
            .end_do_try();
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "x").unwrap();
    assert_eq!(*count.lock().unwrap(), 1);
    assert_eq!(ctx.mock_endpoint("mock:catch").unwrap().received_counter(), 1);
}

#[test]
fn on_when_filters_exception_clause() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.on_exception(&["IOException"]).on_when(header("vip").is_equal_to(true)).handled(true).to("mock:vip");
        b.on_exception(&["IOException"]).handled(true).to("mock:normal");
        b.from("direct:start").process(fail_with("IOException"));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body_and_header("direct:start", "a", "vip", true).unwrap();
    t.send_body_and_header("direct:start", "b", "vip", false).unwrap();
    assert_eq!(bodies(&ctx, "mock:vip"), vec!["a"]);
    assert_eq!(bodies(&ctx, "mock:normal"), vec!["b"]);
}

#[test]
fn exception_caught_and_failure_endpoint_properties() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.on_exception(&["IOException"]).handled(true).to("mock:error");
        b.from("direct:start").route_id("r1").to("direct:fails");
        b.from("direct:fails").route_id("r2").process(fail_with("IOException"));
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "x").unwrap();
    let e = ctx.mock_endpoint("mock:error").unwrap().exchange(0).unwrap();
    assert!(e.property(props::EXCEPTION_CAUGHT).is_some());
    assert_eq!(e.property(props::FAILURE_ROUTE_ID), Some(&Value::from("r2")));
}

#[test]
fn error_in_split_with_on_exception_handled_continues_with_next_item() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.on_exception(&["IllegalArgumentException"]).handled(true).to("mock:error");
        b.from("direct:start")
            .split(body())
            .process(|ex: &mut Exchange| if ex.body() == &Value::from("bad") { Err(CamelError::illegal_argument("bad")) } else { Ok(()) })
            .to("mock:part")
            .end()
            .to("mock:after");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "a,bad,c").unwrap();
    assert_eq!(bodies(&ctx, "mock:part"), vec!["a", "c"]);
    assert_eq!(bodies(&ctx, "mock:error"), vec!["bad"]);
    assert_eq!(bodies(&ctx, "mock:after"), vec!["a,bad,c"]);
}

#[test]
fn throw_exception_dsl_kind_and_message() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").throw_exception("IllegalArgumentException", "Forced ${body}");
    })
    .unwrap();
    ctx.start().unwrap();
    let e = ctx.create_producer_template().send_body("direct:start", "x").unwrap_err();
    let cause = e.cause.unwrap();
    assert_eq!(cause.kind, "IllegalArgumentException");
    // Camel's throwException message is a Simple expression
    assert_eq!(cause.message, "Forced x");
}

#[test]
fn use_original_message_spans_direct_sub_routes() {
    // Camel 3+: the original message is the one bound to the unit of work, which spans
    // routes connected with direct
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.on_exception(&["IOException"]).handled(true).use_original_message().to("mock:dlq");
        b.from("direct:a").transform(constant("changed in a")).to("direct:b");
        b.from("direct:b").transform(constant("changed in b")).process(fail_with("IOException"));
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:a", "original").unwrap();
    assert_eq!(bodies(&ctx, "mock:dlq"), vec!["original"]);
}

#[test]
fn dead_letter_channel_failure_is_still_handled() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.error_handler(dead_letter_channel("direct:dlc").maximum_redeliveries(0));
        b.from("direct:start").process(fail_with("IOException"));
        b.from("direct:dlc").error_handler(no_error_handler()).to("mock:dlc").process(fail_with("IllegalStateException"));
    })
    .unwrap();
    ctx.start().unwrap();
    // deadLetterHandleNewException=true (default): the new exception is logged and handled
    ctx.create_producer_template().send_body("direct:start", "x").unwrap();
    assert_eq!(ctx.mock_endpoint("mock:dlc").unwrap().received_counter(), 1);
}

#[test]
fn retry_while_controls_redelivery() {
    let ctx = CamelContext::new();
    let count = Arc::new(Mutex::new(0));
    let c2 = count.clone();
    ctx.add_routes(move |b| {
        let c3 = c2.clone();
        b.on_exception(&["IOException"])
            .retry_while(simple("${header.CamelRedeliveryCounter} == null || ${header.CamelRedeliveryCounter} < 2"))
            .redelivery_delay(0)
            .handled(true)
            .to("mock:gaveup");
        b.from("direct:start").process(move |_ex: &mut Exchange| {
            *c3.lock().unwrap() += 1;
            Err(CamelError::io("x"))
        });
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "x").unwrap();
    assert_eq!(*count.lock().unwrap(), 3);
    assert_eq!(ctx.mock_endpoint("mock:gaveup").unwrap().received_counter(), 1);
}

#[test]
fn caller_does_not_redeliver_again_after_sub_route_exhausted() {
    // Camel marks the exchange as exhausted in the sub-route; the caller's error
    // handler does not start another round of redeliveries
    let ctx = CamelContext::new();
    let count = Arc::new(Mutex::new(0));
    let c2 = count.clone();
    ctx.add_routes(move |b| {
        let c3 = c2.clone();
        b.error_handler(default_error_handler().maximum_redeliveries(2).redelivery_delay(0).log_exhausted(false));
        b.from("direct:caller").to("direct:sub").to("mock:after");
        b.from("direct:sub").error_handler(default_error_handler().maximum_redeliveries(1).redelivery_delay(0).log_exhausted(false)).process(
            move |_ex: &mut Exchange| {
                *c3.lock().unwrap() += 1;
                Err(CamelError::io("x"))
            },
        );
    })
    .unwrap();
    ctx.start().unwrap();
    assert!(ctx.create_producer_template().send_body("direct:caller", "x").is_err());
    assert_eq!(*count.lock().unwrap(), 2, "only the sub-route's 1 redelivery applies");
    assert_eq!(ctx.mock_endpoint("mock:after").unwrap().received_counter(), 0);
}

#[test]
fn exception_inside_filter_inside_split_handled_per_item() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.on_exception(&["IllegalArgumentException"]).handled(true).set_body(simple("E${exchangeProperty.CamelSplitIndex}")).to("mock:error");
        b.from("direct:start")
            .split(body())
            .filter(simple("${exchangeProperty.CamelSplitIndex} == 1"))
            .throw_exception("IllegalArgumentException", "second item")
            .end()
            .to("mock:part")
            .end()
            .to("mock:after");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "a,b,c").unwrap();
    assert_eq!(bodies(&ctx, "mock:error"), vec!["E1"]);
    assert_eq!(bodies(&ctx, "mock:part"), vec!["a", "c"]);
    assert_eq!(bodies(&ctx, "mock:after"), vec!["a,b,c"]);
}

#[test]
fn use_original_message_inside_split_is_the_split_part() {
    // Camel: the splitter starts a new unit of work for each split message
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.on_exception(&["IOException"]).handled(true).use_original_message().to("mock:dlq");
        b.from("direct:start")
            .split(body())
            .transform(simple("changed ${body}"))
            .filter(simple("${body} == 'changed b'"))
            .throw_exception("IOException", "bad part")
            .end()
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "a,b,c").unwrap();
    assert_eq!(bodies(&ctx, "mock:dlq"), vec!["b"]);
}

#[test]
fn use_original_message_with_split_share_unit_of_work_is_the_parent_message() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.on_exception(&["IOException"]).handled(true).use_original_message().to("mock:dlq");
        b.from("direct:start")
            .transform(simple("changed ${body}"))
            .split(body())
            .share_unit_of_work()
            .filter(simple("${body} == 'b'"))
            .throw_exception("IOException", "bad part")
            .end()
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "a,b,c").unwrap();
    assert_eq!(bodies(&ctx, "mock:dlq"), vec!["a,b,c"]);
}

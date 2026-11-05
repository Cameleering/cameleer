use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cameleer_core::prelude::*;
use cameleer_core::processor::errors::*;

#[test]
fn throttle_time_window() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").throttle(3, Duration::from_millis(100)).to("mock:result");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let start = Instant::now();
    for i in 0..7 {
        t.send_body("direct:start", i).unwrap();
    }
    let elapsed = start.elapsed();
    // 3 immediately, 3 after ~100ms, 1 after ~200ms
    assert!(elapsed >= Duration::from_millis(190), "{:?}", elapsed);
    assert!(elapsed < Duration::from_millis(1500), "{:?}", elapsed);
    assert_eq!(ctx.mock_endpoint("mock:result").unwrap().received_counter(), 7);
}

#[test]
fn throttle_reject_execution() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").throttle(2, Duration::from_secs(10)).reject_execution(true).to("mock:result");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:start", 1).unwrap();
    t.send_body("direct:start", 2).unwrap();
    let e = t.send_body("direct:start", 3).unwrap_err();
    let cause = e.cause.unwrap();
    assert_eq!(cause.kind, kinds::THROTTLER_REJECTED);
    assert!(cause.is_a(kinds::REJECTED_EXECUTION));
    assert_eq!(ctx.mock_endpoint("mock:result").unwrap().received_counter(), 2);
}

#[test]
fn throttle_per_correlation_key() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").throttle_correlated(1, Duration::from_secs(10), header("key")).reject_execution(true).to("mock:result");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body_and_header("direct:start", 1, "key", "a").unwrap();
    t.send_body_and_header("direct:start", 2, "key", "b").unwrap();
    assert!(t.send_body_and_header("direct:start", 3, "key", "a").is_err());
    assert_eq!(ctx.mock_endpoint("mock:result").unwrap().received_counter(), 2);
}

#[test]
fn delay_eip() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").delay(header("wait")).async_delayed(true).to("mock:result");
        b.from("direct:const").delay(constant("30ms")).to("mock:result");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let start = Instant::now();
    t.send_body_and_header("direct:start", 1, "wait", 50).unwrap();
    assert!(start.elapsed() >= Duration::from_millis(50));
    let start = Instant::now();
    t.send_body("direct:const", 2).unwrap();
    assert!(start.elapsed() >= Duration::from_millis(30));
    assert_eq!(ctx.mock_endpoint("mock:result").unwrap().received_counter(), 2);
}

#[test]
fn sample_period_and_frequency() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:period").sample(Duration::from_millis(60)).to("mock:period");
        b.from("direct:every").sample_every(3).to("mock:every");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for i in 0..5 {
        t.send_body("direct:period", i).unwrap();
    }
    std::thread::sleep(Duration::from_millis(80));
    t.send_body("direct:period", 99).unwrap();
    assert_eq!(ctx.mock_endpoint("mock:period").unwrap().received_bodies(), vec![Value::from(0), Value::from(99)]);
    for i in 1..=7 {
        t.send_body("direct:every", i).unwrap();
    }
    assert_eq!(ctx.mock_endpoint("mock:every").unwrap().received_bodies(), vec![Value::from(3), Value::from(6)]);
}

#[test]
fn idempotent_consumer_skips_duplicates() {
    let ctx = CamelContext::new();
    let repo = memory_idempotent_repository(100);
    let r = repo.clone();
    ctx.add_routes(move |b| {
        b.from("direct:start").idempotent_consumer(header("id"), r).to("mock:result").end().to("mock:after");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for (id, body) in [(1, "a"), (2, "b"), (1, "c"), (3, "d"), (2, "e")] {
        t.send_body_and_header("direct:start", body, "id", id).unwrap();
    }
    assert_eq!(
        ctx.mock_endpoint("mock:result").unwrap().received_bodies(),
        vec![Value::from("a"), Value::from("b"), Value::from("d")]
    );
    let after = ctx.mock_endpoint("mock:after").unwrap();
    assert_eq!(after.received_counter(), 5);
    assert_eq!(after.exchange(2).unwrap().property(props::DUPLICATE_MESSAGE), Some(&Value::Bool(true)));
    assert!(repo.contains("1") && repo.contains("2") && repo.contains("3"));
    // missing id
    let e = t.send_body("direct:start", "noid").unwrap_err();
    assert_eq!(e.cause.unwrap().kind, kinds::NO_MESSAGE_ID);
}

#[test]
fn idempotent_consumer_options() {
    let ctx = CamelContext::new();
    let repo = memory_idempotent_repository(100);
    let (r1, r2) = (repo.clone(), repo.clone());
    ctx.add_routes(move |b| {
        b.from("direct:noskip").idempotent_consumer(header("id"), r1).skip_duplicate(false).to("mock:noskip").end();
        b.from("direct:fail")
            .error_handler(default_error_handler().log_exhausted(false))
            .idempotent_consumer(header("id"), r2)
            .eager(false)
            .completion_eager(true)
            .process(|ex: &mut Exchange| if ex.body().as_str() == Some("bad") { Err(CamelError::io("x")) } else { Ok(()) })
            .to("mock:fail")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body_and_header("direct:noskip", "a", "id", "k1").unwrap();
    t.send_body_and_header("direct:noskip", "b", "id", "k1").unwrap();
    let noskip = ctx.mock_endpoint("mock:noskip").unwrap();
    assert_eq!(noskip.received_counter(), 2);
    assert!(noskip.exchange(0).unwrap().property(props::DUPLICATE_MESSAGE).is_none());
    assert_eq!(noskip.exchange(1).unwrap().property(props::DUPLICATE_MESSAGE), Some(&Value::Bool(true)));

    // failed exchange: key not kept, so a retry with the same id is processed
    assert!(t.send_body_and_header("direct:fail", "bad", "id", "k2").is_err());
    assert!(!repo.contains("k2"));
    t.send_body_and_header("direct:fail", "good", "id", "k2").unwrap();
    assert!(repo.contains("k2"));
    t.send_body_and_header("direct:fail", "good again", "id", "k2").unwrap();
    assert_eq!(ctx.mock_endpoint("mock:fail").unwrap().received_bodies(), vec![Value::from("good")]);
}

#[test]
fn idempotent_remove_on_failure_at_completion() {
    let ctx = CamelContext::new();
    let repo = memory_idempotent_repository(10);
    let r = repo.clone();
    ctx.add_routes(move |b| {
        b.from("direct:start")
            .error_handler(default_error_handler().log_exhausted(false))
            .idempotent_consumer(header("id"), r)
            .to("mock:inside")
            .end()
            .throw_exception("IOException", "later failure");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    assert!(t.send_body_and_header("direct:start", 1, "id", "x").is_err());
    // the whole exchange failed -> key removed at completion
    assert!(!repo.contains("x"));
}

#[test]
fn circuit_breaker_transitions() {
    let ctx = CamelContext::new();
    let fail = Arc::new(Mutex::new(true));
    let calls = Arc::new(AtomicUsize::new(0));
    let (f, c) = (fail.clone(), calls.clone());
    ctx.add_routes(move |b| {
        b.from("direct:start")
            .circuit_breaker()
            .minimum_number_of_calls(2)
            .sliding_window_size(4)
            .failure_rate_threshold(50.0)
            .wait_duration_in_open_state(Duration::from_millis(80))
            .permitted_number_of_calls_in_half_open_state(1)
            .process(move |_: &mut Exchange| {
                c.fetch_add(1, Ordering::SeqCst);
                if *f.lock().unwrap() {
                    Err(CamelError::io("service down"))
                } else {
                    Ok(())
                }
            })
            .set_body(constant("from service"))
            .on_fallback()
            .set_body(constant("fallback"))
            .end()
            .to("mock:result");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let state = |ex: &Exchange| ex.property(props::CIRCUIT_BREAKER_STATE).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let send = || t.send_with("direct:start", ExchangePattern::InOut, |ex| ex.set_body("x"));

    let e1 = send();
    assert!(e1.exception.is_none());
    assert_eq!(e1.body(), &Value::from("fallback"));
    assert_eq!(state(&e1), "CLOSED");
    let e2 = send();
    assert_eq!(state(&e2), "OPEN");
    assert_eq!(e2.property(props::RESPONSE_FROM_FALLBACK), Some(&Value::Bool(true)));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    // open: short circuited without calling the service
    let e3 = send();
    assert_eq!(e3.property(props::RESPONSE_SHORT_CIRCUITED), Some(&Value::Bool(true)));
    assert_eq!(e3.body(), &Value::from("fallback"));
    assert_eq!(caught_exception(&e3).unwrap().kind, kinds::CALL_NOT_PERMITTED);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    // after the wait duration a trial call is let through (half open) and closes the breaker
    std::thread::sleep(Duration::from_millis(100));
    *fail.lock().unwrap() = false;
    let e4 = send();
    assert_eq!(e4.body(), &Value::from("from service"));
    assert_eq!(state(&e4), "CLOSED");
    assert_eq!(e4.property(props::RESPONSE_SUCCESSFUL_EXECUTION), Some(&Value::Bool(true)));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(ctx.mock_endpoint("mock:result").unwrap().received_counter(), 4);
}

#[test]
fn circuit_breaker_without_fallback_and_half_open_failure() {
    let ctx = CamelContext::new();
    let attempts = Arc::new(AtomicUsize::new(0));
    let a = attempts.clone();
    ctx.add_routes(move |b| {
        b.error_handler(default_error_handler().maximum_redeliveries(0).log_exhausted(false));
        b.from("direct:start")
            .circuit_breaker()
            .minimum_number_of_calls(1)
            .wait_duration_in_open_state(Duration::from_millis(50))
            .permitted_number_of_calls_in_half_open_state(1)
            .process(move |_: &mut Exchange| {
                a.fetch_add(1, Ordering::SeqCst);
                Err(CamelError::io("down"))
            })
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let e = t.send_body("direct:start", 1).unwrap_err();
    assert!(e.cause.unwrap().is_a("IOException"));
    let e = t.send_body("direct:start", 2).unwrap_err();
    assert_eq!(e.cause.unwrap().kind, kinds::CALL_NOT_PERMITTED);
    std::thread::sleep(Duration::from_millis(70));
    // half open trial fails -> open again
    let ex = t.send_with("direct:start", ExchangePattern::InOnly, |ex| ex.set_body(3));
    assert!(ex.exception.as_ref().unwrap().is_a("IOException"));
    assert_eq!(ex.property(props::CIRCUIT_BREAKER_STATE), Some(&Value::from("OPEN")));
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[test]
fn circuit_breaker_timeout() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .circuit_breaker()
            .timeout_enabled(true)
            .timeout_duration(Duration::from_millis(30))
            .delay(constant(300))
            .set_body(constant("slow"))
            .on_fallback()
            .set_body(constant("timeout fallback"))
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let start = Instant::now();
    let ex = ctx.create_producer_template().send_with("direct:start", ExchangePattern::InOut, |ex| ex.set_body("x"));
    assert!(start.elapsed() < Duration::from_millis(250));
    assert_eq!(ex.body(), &Value::from("timeout fallback"));
    assert_eq!(ex.property(props::RESPONSE_TIMED_OUT), Some(&Value::Bool(true)));
    assert_eq!(caught_exception(&ex).unwrap().kind, kinds::TIMEOUT);
}

#[test]
fn validate_eip() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").validate(body().is_greater_than(10)).to("mock:result");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:start", 42).unwrap();
    let e = t.send_body("direct:start", 3).unwrap_err().cause.unwrap();
    assert_eq!(e.kind, kinds::PREDICATE_VALIDATION);
    assert!(e.is_a(kinds::VALIDATION));
    assert!(e.message.starts_with("Validation failed for Predicate"));
    assert_eq!(ctx.mock_endpoint("mock:result").unwrap().received_counter(), 1);
}

#[test]
fn rollback_variants() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:rollback").rollback("forced rollback").to("mock:never");
        b.from("direct:mark").mark_rollback_only().to("mock:never");
        b.from("direct:markLast").mark_rollback_only_last().to("mock:never");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let e = t.send_body("direct:rollback", 1).unwrap_err().cause.unwrap();
    assert_eq!(e.kind, kinds::ROLLBACK_EXCHANGE);
    assert_eq!(e.message, "forced rollback");
    let ex = t.send_with("direct:mark", ExchangePattern::InOnly, |_| {});
    assert!(ex.exception.is_none());
    assert!(ex.rollback_only);
    let ex = t.send_with("direct:markLast", ExchangePattern::InOnly, |_| {});
    assert_eq!(ex.property(ROLLBACK_ONLY_LAST), Some(&Value::Bool(true)));
    assert_eq!(ctx.mock_endpoint("mock:never").unwrap().received_counter(), 0);
}

#[test]
fn throw_exception_with_simple_message() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").throw_exception("MyBusinessException", "Invalid order ${header.order}");
    })
    .unwrap();
    ctx.start().unwrap();
    let e = ctx.create_producer_template().send_body_and_header("direct:start", 1, "order", 7).unwrap_err().cause.unwrap();
    assert_eq!(e.kind, "MyBusinessException");
    assert_eq!(e.message, "Invalid order 7");
}

#[test]
fn threads_eip() {
    let ctx = CamelContext::new();
    let names = Arc::new(Mutex::new(Vec::<String>::new()));
    let n = names.clone();
    ctx.add_routes(move |b| {
        b.from("direct:start")
            .threads(2)
            .process(move |ex: &mut Exchange| {
                n.lock().unwrap().push(std::thread::current().name().unwrap_or("").to_string());
                let s: String = ex.body_as()?;
                ex.set_body(s.to_uppercase());
                Ok(())
            })
            .to("mock:result");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    // InOut waits for the result
    assert_eq!(t.request_body("direct:start", "hello").unwrap(), Value::from("HELLO"));
    // InOnly returns immediately and continues asynchronously
    t.send_body("direct:start", "async").unwrap();
    let mock = ctx.mock_endpoint("mock:result").unwrap();
    assert!(mock.wait_for_messages(2, Duration::from_secs(5)));
    assert!(names.lock().unwrap().iter().all(|n| n.starts_with("Threads")), "{:?}", names.lock().unwrap());
    ctx.stop().unwrap();
}

#[test]
fn transacted_with_a_transaction_manager() {
    let ctx = CamelContext::new();
    let commits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let c2 = commits.clone();
    ctx.registry().bind_transaction_manager(
        "tm",
        cameleer_core::transaction::FnTransactionManager::new(
            |_| Ok(Box::new(())),
            move |_| {
                c2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            },
            |_| Ok(()),
        ),
    );
    ctx.add_routes(|b| {
        b.from("direct:start").transacted().to("mock:result");
        b.from("direct:ref").transacted_ref("PROPAGATION_REQUIRES_NEW").to("mock:result");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", 1).unwrap();
    ctx.create_producer_template().send_body("direct:ref", 2).unwrap();
    assert_eq!(ctx.mock_endpoint("mock:result").unwrap().received_counter(), 2);
    assert_eq!(commits.load(std::sync::atomic::Ordering::SeqCst), 2);
}

struct HeaderPolicy(&'static str);

impl Policy for HeaderPolicy {
    fn wrap(&self, _rc: &RouteContext, processor: Arc<dyn Processor>) -> Result<Arc<dyn Processor>> {
        let name = self.0;
        Ok(Arc::new(move |ex: &mut Exchange| {
            ex.set_header("policyBefore", name);
            processor.process(ex)?;
            ex.set_header("policyAfter", name);
            Ok(())
        }))
    }
}

#[test]
fn policy_wraps_block() {
    let ctx = CamelContext::new();
    let p: Arc<dyn Policy> = Arc::new(HeaderPolicy("registry"));
    ctx.registry().bind("myPolicy", p);
    ctx.add_routes(|b| {
        b.from("direct:start").policy(Arc::new(HeaderPolicy("inline"))).to("mock:inside").end().to("mock:after");
        b.from("direct:ref").policy_ref("myPolicy").to("mock:inside").end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:start", 1).unwrap();
    t.send_body("direct:ref", 2).unwrap();
    let inside = ctx.mock_endpoint("mock:inside").unwrap();
    assert_eq!(inside.exchange(0).unwrap().header("policyBefore"), Some(&Value::from("inline")));
    assert!(inside.exchange(0).unwrap().header("policyAfter").is_none());
    assert_eq!(inside.exchange(1).unwrap().header("policyBefore"), Some(&Value::from("registry")));
    let after = ctx.mock_endpoint("mock:after").unwrap().exchange(0).unwrap();
    assert_eq!(after.header("policyAfter"), Some(&Value::from("inline")));
}

#[derive(Default)]
struct CountingPolicy {
    inits: AtomicUsize,
    begins: AtomicUsize,
    dones: AtomicUsize,
    failed: AtomicUsize,
}

impl RoutePolicy for CountingPolicy {
    fn on_init(&self, _route_id: &str) {
        self.inits.fetch_add(1, Ordering::SeqCst);
    }
    fn on_exchange_begin(&self, _route_id: &str, ex: &mut Exchange) {
        self.begins.fetch_add(1, Ordering::SeqCst);
        ex.set_header("policy", "begin");
    }
    fn on_exchange_done(&self, _route_id: &str, ex: &Exchange) {
        self.dones.fetch_add(1, Ordering::SeqCst);
        if ex.is_failed() {
            self.failed.fetch_add(1, Ordering::SeqCst);
        }
    }
}

#[test]
fn route_policy_callbacks() {
    let ctx = CamelContext::new();
    let policy = Arc::new(CountingPolicy::default());
    let p: Arc<dyn RoutePolicy> = policy.clone();
    ctx.registry().bind("counting", p.clone());
    ctx.add_routes(move |b| {
        b.from("direct:start").route_policy(p).to("mock:result");
        b.from("direct:fail").route_policy_ref("counting").error_handler(no_error_handler()).throw_exception("IOException", "x");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:start", 1).unwrap();
    t.send_body("direct:start", 2).unwrap();
    assert!(t.send_body("direct:fail", 3).is_err());
    assert_eq!(policy.inits.load(Ordering::SeqCst), 2);
    assert_eq!(policy.begins.load(Ordering::SeqCst), 3);
    assert_eq!(policy.dones.load(Ordering::SeqCst), 3);
    assert_eq!(policy.failed.load(Ordering::SeqCst), 1);
    assert_eq!(ctx.mock_endpoint("mock:result").unwrap().exchange(0).unwrap().header("policy"), Some(&Value::from("begin")));
}

#[test]
fn throttling_inflight_route_policy_limits_concurrency() {
    let ctx = CamelContext::new();
    let policy = Arc::new(ThrottlingInflightRoutePolicy::new(2).with_resume_percent_of_max(100));
    let p: Arc<dyn RoutePolicy> = policy.clone();
    let current = Arc::new(AtomicUsize::new(0));
    let max_seen = Arc::new(AtomicUsize::new(0));
    let (c, m) = (current.clone(), max_seen.clone());
    ctx.add_routes(move |b| {
        b.from("direct:start").route_policy(p).process(move |_: &mut Exchange| {
            let now = c.fetch_add(1, Ordering::SeqCst) + 1;
            m.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(30));
            c.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let handles: Vec<_> = (0..6).map(|i| t.async_send_body("direct:start", i)).collect();
    for h in handles {
        h.join().unwrap().unwrap();
    }
    assert!(max_seen.load(Ordering::SeqCst) <= 2, "{}", max_seen.load(Ordering::SeqCst));
    assert!(policy.throttled_count() > 0);
    assert_eq!(policy.inflight("route1"), 0);
}

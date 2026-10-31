use std::time::{Duration, Instant};

use camel_core::component::scheduler::SCHEDULER_POLLED_MESSAGES;
use camel_core::prelude::*;

#[test]
fn timer_repeat_count_delivers_exact_number() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("timer:tick?repeatCount=3&period=10&delay=0").to("mock:out");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:out").unwrap();
    mock.expected_message_count(3);
    mock.set_assert_period(Duration::from_millis(60));
    mock.assert_is_satisfied().unwrap();
    let counters: Vec<i64> = mock.received_exchanges().iter().map(|e| e.property("CamelTimerCounter").unwrap().as_i64().unwrap()).collect();
    assert_eq!(counters, vec![1, 2, 3]);
    assert!(mock.received_exchanges().iter().all(|e| e.body().is_null()));
    ctx.stop().unwrap();
}

#[test]
fn timer_include_metadata_sets_headers() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("timer:meta?repeatCount=1&delay=0&includeMetadata=true&period=5").to("mock:meta");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:meta").unwrap();
    mock.expected_message_count(1);
    mock.assert_is_satisfied().unwrap();
    let ex = mock.exchange(0).unwrap();
    assert_eq!(ex.property("CamelTimerName").unwrap(), &Value::from("meta"));
    assert_eq!(ex.property("CamelTimerPeriod").unwrap().as_i64(), Some(5));
    assert!(ex.header("CamelTimerFiredTime").is_some());
    assert_eq!(ex.header("CamelTimerCounter").unwrap().as_i64(), Some(1));
    ctx.stop().unwrap();
}

#[test]
fn timer_without_metadata_has_no_name_property() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("timer:plain?repeatCount=1&delay=0").to("mock:plain");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:plain").unwrap();
    mock.expected_message_count(1);
    mock.assert_is_satisfied().unwrap();
    assert!(mock.exchange(0).unwrap().property("CamelTimerName").is_none());
    ctx.stop().unwrap();
}

#[test]
fn timer_stops_promptly() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("timer:slow?period=60000&delay=60000").to("mock:never");
    })
    .unwrap();
    ctx.start().unwrap();
    let start = Instant::now();
    ctx.stop().unwrap();
    assert!(start.elapsed() < Duration::from_millis(500), "stop took {:?}", start.elapsed());
}

#[test]
fn timer_fixed_rate_and_route_stop_start() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("timer:rate?period=5&delay=0&fixedRate=true").route_id("rate").to("mock:rate");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:rate").unwrap();
    assert!(mock.wait_for_messages(3, Duration::from_secs(5)));
    ctx.stop_route("rate").unwrap();
    let n = mock.received_counter();
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(mock.received_counter(), n, "no messages after stop");
    ctx.start_route("rate").unwrap();
    assert!(mock.wait_for_messages(n + 2, Duration::from_secs(5)));
    ctx.stop().unwrap();
}

#[test]
fn timer_bad_option_fails() {
    let ctx = CamelContext::new();
    assert!(ctx.get_endpoint("timer:x?period=abc").is_err());
    assert!(ctx.get_endpoint("timer:x?nope=1").is_err());
}

#[test]
fn scheduler_repeat_count_and_properties() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("scheduler:sched?initialDelay=0&delay=5&repeatCount=4").to("mock:sched");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:sched").unwrap();
    mock.expected_message_count(4);
    mock.set_assert_period(Duration::from_millis(50));
    mock.assert_is_satisfied().unwrap();
    assert_eq!(mock.exchange(0).unwrap().property("CamelTimerName").unwrap(), &Value::from("sched"));
    ctx.stop().unwrap();
}

#[test]
fn scheduler_greedy_and_backoff() {
    let ctx = CamelContext::new();
    // the route reports "nothing polled" -> after 1 idle poll the scheduler skips 1000 polls
    ctx.add_routes(|b| {
        b.from("scheduler:idle?initialDelay=0&delay=2&backoffMultiplier=1000&backoffIdleThreshold=1")
            .process(|ex: &mut Exchange| {
                ex.set_property(SCHEDULER_POLLED_MESSAGES, false);
                Ok(())
            })
            .to("mock:idle");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:idle").unwrap();
    mock.expected_message_count(1);
    mock.set_assert_period(Duration::from_millis(80));
    mock.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
}

#[test]
fn scheduler_cron_and_unknown_scheduler_rejected() {
    let ctx = CamelContext::new();
    assert!(ctx.get_endpoint("scheduler:c?scheduler.cron=0+0+*+*+*").is_err());
    assert!(ctx.get_endpoint("scheduler:c?scheduler=spring").is_ok());
    assert!(ctx.get_endpoint("scheduler:d?timeUnit=SECONDS&delay=1").is_ok());
}

#[test]
fn scheduler_start_scheduler_false_does_not_fire() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("scheduler:manual?initialDelay=0&delay=1&startScheduler=false").to("mock:manual");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:manual").unwrap();
    mock.expected_message_count(0);
    mock.assert_is_satisfied_within(Duration::from_millis(50)).unwrap();
    ctx.stop().unwrap();
}

//! Asynchronous delays (`asyncDelayed`, `asyncDelayedRedelivery`): the consumer thread of
//! asynchronous consumers (seda, timer) is released while an exchange waits; synchronous
//! callers (direct + ProducerTemplate) still wait.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use camel_core::prelude::*;

fn mock(ctx: &CamelContext, uri: &str) -> MockEndpoint {
    ctx.mock_endpoint(uri).unwrap()
}

#[test]
fn async_delay_releases_the_seda_consumer() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        // delay is asyncDelayed by default, as in Camel
        b.from("seda:async").to("mock:a-before").delay(constant(1000)).to("mock:a-after");
        b.from("seda:sync").to("mock:s-before").delay(constant(1000)).sync_delayed().to("mock:s-after");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let start = Instant::now();
    for i in 0..3 {
        t.send_body("seda:async", i).unwrap();
        t.send_body("seda:sync", i).unwrap();
    }
    // the single seda consumer took all three exchanges without waiting for the delays
    assert!(mock(&ctx, "mock:a-before").wait_for_messages(3, Duration::from_millis(900)), "{:?}", start.elapsed());
    // the synchronous delay blocks the consumer: only the first exchange got through
    assert_eq!(mock(&ctx, "mock:s-before").received_counter(), 1);
    assert!(mock(&ctx, "mock:a-after").wait_for_messages(3, Duration::from_secs(5)));
    // all three delays overlapped (about 1s, not 3s)
    assert!(start.elapsed() < Duration::from_millis(2500), "{:?}", start.elapsed());
    assert!(mock(&ctx, "mock:s-after").wait_for_messages(3, Duration::from_secs(5)));
    assert!(start.elapsed() >= Duration::from_millis(3000), "{:?}", start.elapsed());
    // graceful stop waits for released threads
    ctx.stop().unwrap();
}

#[test]
fn async_delay_still_blocks_synchronous_callers() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").delay(constant(100)).async_delayed(true).set_body(constant("done"));
    })
    .unwrap();
    ctx.start().unwrap();
    let start = Instant::now();
    let r = ctx.create_producer_template().request_body("direct:start", "x").unwrap();
    assert_eq!(r, Value::from("done"));
    assert!(start.elapsed() >= Duration::from_millis(100));
}

#[test]
fn async_delayed_throttle_releases_the_seda_consumer() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("seda:t")
            .to("mock:t-before")
            .throttle(1, Duration::from_millis(300))
            .async_delayed(true)
            .to("mock:t-after");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let start = Instant::now();
    for i in 0..3 {
        t.send_body("seda:t", i).unwrap();
    }
    assert!(mock(&ctx, "mock:t-before").wait_for_messages(3, Duration::from_millis(250)));
    // the throttle still applies: 1 per 300ms
    assert!(mock(&ctx, "mock:t-after").wait_for_messages(3, Duration::from_secs(5)));
    assert!(start.elapsed() >= Duration::from_millis(600), "{:?}", start.elapsed());
    ctx.stop().unwrap();
}

#[test]
fn async_delayed_redelivery_releases_the_seda_consumer() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let ctx = CamelContext::new();
    let a = attempts.clone();
    ctx.add_routes(move |b| {
        b.error_handler(default_error_handler().maximum_redeliveries(1).redelivery_delay(400).async_delayed_redelivery(true));
        let a = a.clone();
        b.from("seda:r")
            .process(move |ex: &mut Exchange| {
                a.fetch_add(1, Ordering::SeqCst);
                if ex.header(props::REDELIVERED).is_none() {
                    return Err(CamelError::io("first attempt fails"));
                }
                Ok(())
            })
            .to("mock:r-done");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let start = Instant::now();
    for i in 0..3 {
        t.send_body("seda:r", i).unwrap();
    }
    // the three first attempts happen at once: the consumer is not blocked by the redelivery delays
    let deadline = Instant::now() + Duration::from_millis(300);
    while attempts.load(Ordering::SeqCst) < 3 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(attempts.load(Ordering::SeqCst), 3, "{:?}", start.elapsed());
    let done = mock(&ctx, "mock:r-done");
    assert!(done.wait_for_messages(3, Duration::from_secs(5)));
    assert!(start.elapsed() < Duration::from_millis(1000), "{:?}", start.elapsed());
    assert!(done.received_exchanges().iter().all(|e| e.header(props::REDELIVERY_COUNTER) == Some(&Value::Int(1))));
    ctx.stop().unwrap();
}

#[test]
fn async_delay_lets_the_timer_keep_its_schedule() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("timer:tick?period=50&repeatCount=4&delay=0").to("mock:tick").delay(constant(500)).to("mock:tock");
    })
    .unwrap();
    ctx.start().unwrap();
    // 4 fires at ~0, 50, 100, 150ms despite the 500ms delay of each exchange
    assert!(mock(&ctx, "mock:tick").wait_for_messages(4, Duration::from_millis(450)));
    assert!(mock(&ctx, "mock:tock").wait_for_messages(4, Duration::from_secs(5)));
    let counters: Vec<i64> =
        mock(&ctx, "mock:tock").received_exchanges().iter().map(|e| e.property(props::TIMER_COUNTER).unwrap().as_i64().unwrap()).collect();
    let mut sorted = counters.clone();
    sorted.sort();
    assert_eq!(sorted, vec![1, 2, 3, 4]);
    ctx.stop().unwrap();
}

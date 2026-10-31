use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use camel_core::component::seda;
use camel_core::prelude::*;

#[test]
fn seda_in_only_async_delivery() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("seda:a").to("mock:a");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:a").unwrap();
    mock.expected_bodies_received(vec!["x", "y", "z"]);
    let t = ctx.create_producer_template();
    for b in ["x", "y", "z"] {
        t.send_body("seda:a", b).unwrap();
    }
    mock.assert_is_satisfied().unwrap();
    // exchanges crossing the queue are correlated copies
    assert!(mock.exchange(0).unwrap().property("CamelCorrelationId").is_some());
    ctx.stop().unwrap();
}

#[test]
fn seda_in_out_request_reply() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("seda:rr")
            .process(|ex: &mut Exchange| {
                let s: String = ex.body_as()?;
                ex.set_body(format!("Bye {}", s));
                ex.set_header("replied", true);
                Ok(())
            });
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    assert_eq!(t.request_body("seda:rr", "World").unwrap(), Value::from("Bye World"));
    let ex = t.request("seda:rr", |e| e.set_body("Camel")).unwrap();
    assert_eq!(ex.header("replied").unwrap(), &Value::Bool(true));
    ctx.stop().unwrap();
}

#[test]
fn seda_in_out_failure_propagates_and_timeout() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("seda:fail").process(|_: &mut Exchange| Err(CamelError::illegal_argument("bad")));
        b.from("seda:slow").process(|_: &mut Exchange| {
            std::thread::sleep(Duration::from_millis(300));
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let err = t.request_body("seda:fail", "x").unwrap_err();
    assert!(err.cause.unwrap().is_a("IllegalArgumentException"));
    let err = t.request_body("seda:slow?timeout=50", "x").unwrap_err();
    assert!(err.cause.unwrap().is_a("ExchangeTimedOutException"));
    // waitForTaskToComplete=Never returns immediately with the original body
    let start = Instant::now();
    assert_eq!(t.request_body("seda:slow?waitForTaskToComplete=Never", "orig").unwrap(), Value::from("orig"));
    assert!(start.elapsed() < Duration::from_millis(250));
    ctx.stop().unwrap();
}

#[test]
fn seda_concurrent_consumers_process_in_parallel() {
    let ctx = CamelContext::new();
    let threads = Arc::new(Mutex::new(HashSet::new()));
    let th = threads.clone();
    ctx.add_routes(move |b| {
        b.from("seda:par?concurrentConsumers=4")
            .process(move |_: &mut Exchange| {
                th.lock().unwrap().insert(std::thread::current().name().unwrap_or("").to_string());
                std::thread::sleep(Duration::from_millis(50));
                Ok(())
            })
            .to("mock:par");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:par").unwrap();
    mock.expected_message_count(8);
    let t = ctx.create_producer_template();
    let start = Instant::now();
    for i in 0..8 {
        t.send_body("seda:par", i).unwrap();
    }
    mock.assert_is_satisfied().unwrap();
    assert!(start.elapsed() < Duration::from_millis(350), "took {:?}", start.elapsed());
    assert!(threads.lock().unwrap().len() >= 2);
    ctx.stop().unwrap();
}

#[test]
fn seda_queue_full_and_queue_size() {
    let ctx = CamelContext::new();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("seda:full?size=2", "a").unwrap();
    t.send_body("seda:full?size=2", "b").unwrap();
    assert_eq!(seda::queue_size(&ctx, "seda:full").unwrap(), 2);
    let err = t.send_body("seda:full?size=2", "c").unwrap_err();
    let cause = err.cause.unwrap();
    assert!(cause.is_a("IllegalStateException"));
    assert_eq!(cause.message, "Queue full");
    // discardWhenFull silently drops
    t.send_body("seda:full?size=2&discardWhenFull=true", "d").unwrap();
    assert_eq!(seda::queue_size(&ctx, "seda:full?size=2").unwrap(), 2);
    // blockWhenFull with offerTimeout fails after the timeout
    let err = t.send_body("seda:full?size=2&blockWhenFull=true&offerTimeout=20", "e").unwrap_err();
    assert!(err.cause.unwrap().message.contains("timeout"));
    // a different explicit size is rejected
    assert!(ctx.get_endpoint("seda:full?size=5").is_err());
    // polling consumer drains the queue
    let ct = ctx.create_consumer_template();
    assert_eq!(ct.receive_body("seda:full", Duration::from_millis(10)).unwrap(), Some(Value::from("a")));
    assert_eq!(ct.receive_body("seda:full", Duration::from_millis(10)).unwrap(), Some(Value::from("b")));
    assert_eq!(ct.receive_no_wait("seda:full").unwrap().map(|e| e.message.body), None);
    ctx.stop().unwrap();
}

#[test]
fn seda_multiple_consumers_pub_sub() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("seda:topic?multipleConsumers=true").to("mock:s1");
        b.from("seda:topic?multipleConsumers=true").to("mock:s2");
    })
    .unwrap();
    ctx.start().unwrap();
    let (m1, m2) = (ctx.mock_endpoint("mock:s1").unwrap(), ctx.mock_endpoint("mock:s2").unwrap());
    m1.expected_bodies_received(vec!["e1", "e2"]);
    m2.expected_bodies_received(vec!["e1", "e2"]);
    let t = ctx.create_producer_template();
    t.send_body("seda:topic?multipleConsumers=true", "e1").unwrap();
    t.send_body("seda:topic?multipleConsumers=true", "e2").unwrap();
    MockEndpoint::assert_all_satisfied(&[&m1, &m2]).unwrap();
    ctx.stop().unwrap();
}

#[test]
fn seda_fail_if_no_consumers_and_purge() {
    let ctx = CamelContext::new();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let err = t.send_body("seda:none?failIfNoConsumers=true", "x").unwrap_err();
    assert!(err.cause.unwrap().kind.contains("SedaConsumerNotAvailable"));
    t.send_body("seda:none?discardIfNoConsumers=true", "x").unwrap();
    assert_eq!(seda::queue_size(&ctx, "seda:none").unwrap(), 0);
    ctx.stop().unwrap();
}

#[test]
fn vm_across_two_contexts() {
    let consumer_ctx = CamelContext::new();
    let producer_ctx = CamelContext::new();
    consumer_ctx
        .add_routes(|b| {
            b.from("vm:shared-xctx").transform(simple("got ${body}")).to("mock:vm");
        })
        .unwrap();
    consumer_ctx.start().unwrap();
    producer_ctx.start().unwrap();
    let mock = consumer_ctx.mock_endpoint("mock:vm").unwrap();
    mock.expected_bodies_received(vec!["got hi"]);
    producer_ctx.create_producer_template().send_body("vm:shared-xctx", "hi").unwrap();
    mock.assert_is_satisfied().unwrap();
    // the exchange is bound to the consumer's context
    assert_eq!(mock.exchange(0).unwrap().context.unwrap().name(), consumer_ctx.name());
    // request-reply across contexts
    assert_eq!(producer_ctx.create_producer_template().request_body("vm:shared-xctx", "x").unwrap(), Value::from("got x"));
    producer_ctx.stop().unwrap();
    consumer_ctx.stop().unwrap();
}

#[test]
fn stub_accepts_any_uri() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("stub:smtp://somehost?user=x&password=y").to("mock:stub");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:stub").unwrap();
    mock.expected_bodies_received(vec!["mail"]);
    ctx.create_producer_template().send_body("stub:smtp://somehost?user=x&password=y", "mail").unwrap();
    mock.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
}

#[test]
fn seda_stop_is_prompt_and_on_completion_handover() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("seda:oc?concurrentConsumers=3&pollTimeout=60000").to("mock:oc");
    })
    .unwrap();
    ctx.start().unwrap();
    let done = Arc::new(Mutex::new(false));
    let d = done.clone();
    let mut ex = ctx.create_exchange();
    ex.set_body("x");
    ex.add_on_completion(move |_| *d.lock().unwrap() = true);
    let ex = ctx.create_producer_template().send("seda:oc", ex);
    assert!(ex.exception.is_none());
    let mock = ctx.mock_endpoint("mock:oc").unwrap();
    assert!(mock.wait_for_messages(1, Duration::from_secs(5)));
    let deadline = Instant::now() + Duration::from_secs(2);
    while !*done.lock().unwrap() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(*done.lock().unwrap(), "on completion handed over to the consumer side");
    let start = Instant::now();
    ctx.stop().unwrap();
    assert!(start.elapsed() < Duration::from_millis(500), "stop took {:?}", start.elapsed());
}

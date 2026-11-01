//! Live interop: the `amqp` component (JMS over AMQP 1.0) against the RabbitMQ 3.12 AMQP 1.0
//! plugin started by `scripts/live-interop.sh`. Runs only with `CAMEL_LIVE_INTEROP=1`.
//!
//! RabbitMQ 3.12 addresses nodes with its own syntax: `/queue/<name>` (declares a durable
//! queue), `/amq/queue/<name>` (existing queue), `/exchange/<x>/<key>` and `/topic/<key>`;
//! it has no JMS selectors.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use camel_core::prelude::*;
use camel_tls::testing::live;

const PORT: u16 = 15673;
const TLS_PORT: u16 = 15674;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    camel_amqp::register(&ctx);
    camel_amqp10::register(&ctx);
    live::bind_beans(&ctx);
    let p = ctx.properties();
    p.set("camel.component.amqp.remoteAddress", format!("amqp://127.0.0.1:{}", PORT));
    p.set("camel.component.amqp.username", "camel");
    p.set("camel.component.amqp.password", live::PASSWORD);
    ctx
}

fn n(name: &str) -> String {
    format!("live-{}-{}", std::process::id(), name)
}

fn delete_queues(queues: &[&str]) {
    use camel_amqp::amqp::client::{AmqpConfig, Connection};
    let conn = Connection::open(&AmqpConfig { host: "127.0.0.1".into(), port: PORT, username: "camel".into(), password: live::PASSWORD.into(), heartbeat: 0, ..Default::default() }).unwrap();
    let ch = conn.channel().unwrap();
    for q in queues {
        let _ = ch.queue_delete(q, false, false);
    }
    conn.close();
}

#[test]
fn live_amqp10_queue_round_trip_and_headers() {
    if !live::enabled() {
        return;
    }
    let q = n("orders");
    let ctx = context();
    let addr = format!("amqp:queue:/queue/{}", q);
    let from = addr.clone();
    ctx.add_routes(move |r| {
        r.from(&from).to("mock:orders");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:orders").unwrap();
    mock.expected_bodies_received(vec!["hello amqp", "second"]);
    let t = ctx.create_producer_template();
    t.send_body_and_headers(
        &format!("{}?priority=7", addr),
        "hello amqp",
        vec![("my.key", Value::from("v1")), ("count", Value::from(3)), ("JMSType", Value::from("order")), ("JMSCorrelationID", Value::from("c-1"))],
    )
    .unwrap_or_else(|e| panic!("{}", e));
    t.send_body(&addr, "second").unwrap();
    mock.assert_is_satisfied_within(Duration::from_secs(10)).unwrap();
    let ex = mock.exchange(0).unwrap();
    let s = |h: &str| ex.header(h).map(|v| v.to_string()).unwrap_or_default();
    assert_eq!(s("my.key"), "v1");
    assert_eq!(ex.header("count").and_then(|v| v.as_i64()), Some(3));
    assert_eq!(s("JMSType"), "order");
    assert_eq!(s("JMSCorrelationID"), "c-1");
    assert_eq!(ex.header("JMSPriority").and_then(|v| v.as_i64()), Some(7));
    assert_eq!(ex.header("JMSDeliveryMode").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(ex.header("JMSRedelivered").and_then(|v| v.as_bool()), Some(false));
    assert!(!s("JMSMessageID").is_empty());
    // bytes and map bodies
    mock.reset();
    mock.expected_message_count(2);
    t.send_body(&addr, Value::Bytes(vec![0, 1, 2, 255])).unwrap();
    t.send_body(&addr, Value::object([("id", Value::Int(1)), ("name", Value::from("x"))])).unwrap();
    mock.assert_is_satisfied_within(Duration::from_secs(10)).unwrap();
    assert_eq!(mock.exchange(0).unwrap().body().clone(), Value::Bytes(vec![0, 1, 2, 255]));
    assert_eq!(mock.exchange(1).unwrap().body().get("name"), Some(&Value::from("x")));
    ctx.stop().unwrap();
    // wrong credentials
    let ctx = context();
    ctx.start().unwrap();
    let ex = ctx.create_producer_template().send(&format!("{}?username=camel&password=wrong", addr), Exchange::with_body(Some(ctx.clone()), "x"));
    assert!(ex.exception.is_some(), "bad credentials must fail");
    ctx.stop().unwrap();
    delete_queues(&[&q]);
}

#[test]
fn live_amqp10_competing_consumers_and_topics() {
    if !live::enabled() {
        return;
    }
    let (work, key) = (n("work"), n("news"));
    let ctx = context();
    let (w, k1, k2) = (work.clone(), key.clone(), key.clone());
    ctx.add_routes(move |r| {
        r.from(&format!("amqp:queue:/queue/{}?concurrentConsumers=3", w)).to("mock:work");
        r.from(&format!("amqp:topic:/topic/{}", k1)).to("mock:sub1");
        r.from(&format!("amqp:topic:/topic/{}", k2)).to("mock:sub2");
    })
    .unwrap();
    ctx.start().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let wm = ctx.mock_endpoint("mock:work").unwrap();
    let bodies: Vec<String> = (0..12).map(|i| format!("job-{}", i)).collect();
    wm.expected_bodies_received_in_any_order(bodies.clone());
    let s1 = ctx.mock_endpoint("mock:sub1").unwrap();
    let s2 = ctx.mock_endpoint("mock:sub2").unwrap();
    s1.expected_bodies_received(vec!["breaking"]);
    s2.expected_bodies_received(vec!["breaking"]);
    let t = ctx.create_producer_template();
    for body in &bodies {
        t.send_body(&format!("amqp:queue:/queue/{}", work), body.as_str()).unwrap();
    }
    t.send_body(&format!("amqp:topic:/topic/{}", key), "breaking").unwrap();
    MockEndpoint::assert_all_satisfied(&[&wm, &s1, &s2]).unwrap();
    ctx.stop().unwrap();
    delete_queues(&[&work]);
}

#[test]
fn live_amqp10_request_reply() {
    if !live::enabled() {
        return;
    }
    let (upper, fails, replies) = (n("upper"), n("fails"), n("replies"));
    let ctx = context();
    let (u, f) = (upper.clone(), fails.clone());
    ctx.add_routes(move |r| {
        r.from(&format!("amqp:queue:/queue/{}", u)).transform(simple("${body.toUpperCase()}")).set_header("replied", constant(true));
        r.from(&format!("amqp:queue:/queue/{}", f)).throw_exception("IllegalStateException", "boom");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    // a fixed reply queue per producer endpoint (RabbitMQ has no selectors, so producers must
    // not compete for one reply queue)
    let (r1, r2, r3) = (format!("{}-1", replies), format!("{}-2", replies), format!("{}-3", replies));
    assert_eq!(t.request_body(&format!("amqp:queue:/queue/{}?replyTo=/queue/{}", upper, r1), "hello").unwrap_or_else(|e| panic!("{}", e)), Value::from("HELLO"));
    let ex = t.request(&format!("amqp:queue:/queue/{}?replyTo=/queue/{}", upper, r1), |ex| ex.set_body("again")).unwrap();
    assert_eq!(ex.body(), &Value::from("AGAIN"));
    assert_eq!(ex.header("replied"), Some(&Value::Bool(true)));
    let err = t.request_body(&format!("amqp:queue:/queue/{}?replyTo=/queue/{}", fails, r2), "x").unwrap_err();
    assert!(err.chain().any(|c| c.kind == "IllegalStateException" && c.message.contains("boom")), "{:?}", err);
    let err = t.request_body(&format!("amqp:queue:/queue/{}?requestTimeout=300&replyTo=/queue/{}", n("nobody"), r3), "z").unwrap_err();
    assert!(err.chain().any(|c| c.kind == "ExchangeTimedOutException"), "{:?}", err);
    // the default reply queue is a dynamic node, which RabbitMQ 3.12 refuses (clear error)
    let err = t.request_body(&format!("amqp:queue:/queue/{}?requestTimeout=5000", upper), "dynamic").unwrap_err();
    assert!(format!("{:?}", err).contains("Dynamic sources not supported"), "{:?}", err);
    ctx.stop().unwrap();
    delete_queues(&[&upper, &fails, &r1, &r2, &r3, &n("nobody")]);
}

#[test]
fn live_amqp10_client_ack_redelivery_and_transactions() {
    if !live::enabled() {
        return;
    }
    let (client, tx) = (n("client"), n("tx"));
    let ctx = context();
    let attempts = Arc::new(AtomicUsize::new(0));
    let a2 = attempts.clone();
    let (c, txq) = (client.clone(), tx.clone());
    let tx_attempts = Arc::new(AtomicUsize::new(0));
    let t2 = tx_attempts.clone();
    ctx.add_routes(move |r| {
        r.from(&format!("amqp:queue:/queue/{}?acknowledgementModeName=CLIENT_ACKNOWLEDGE", c))
            .process(move |_ex: &mut Exchange| if a2.fetch_add(1, Ordering::SeqCst) == 0 { Err(CamelError::illegal_state("first fails")) } else { Ok(()) })
            .to("mock:client");
        r.from(&format!("amqp:queue:/queue/{}?transacted=true", txq))
            .process(move |_ex: &mut Exchange| if t2.fetch_add(1, Ordering::SeqCst) == 0 { Err(CamelError::illegal_state("tx fails")) } else { Ok(()) })
            .to("mock:tx");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let m = ctx.mock_endpoint("mock:client").unwrap();
    m.expected_message_count(1);
    m.expected_header_received("JMSRedelivered", true);
    t.send_body(&format!("amqp:queue:/queue/{}", client), "c").unwrap();
    m.assert_is_satisfied_within(Duration::from_secs(10)).unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    // RabbitMQ 3.12's AMQP 1.0 plugin has no transaction coordinator: the transacted consumer
    // cannot attach one (`amqp:not-implemented`) and never takes the message
    t.send_body(&format!("amqp:queue:/queue/{}", tx), "t").unwrap();
    let txm = ctx.mock_endpoint("mock:tx").unwrap();
    assert!(!txm.wait_for_messages(1, Duration::from_millis(500)));
    assert_eq!(tx_attempts.load(Ordering::SeqCst), 0);
    let ex = t.send(&format!("amqp:queue:/queue/{}?transacted=true", tx), Exchange::with_body(Some(ctx.clone()), "t2"));
    assert!(format!("{:?}", ex.exception).contains("Transactions not supported"), "{:?}", ex.exception);
    ctx.stop().unwrap();
    delete_queues(&[&client, &tx]);
}

#[test]
fn live_amqp10_over_tls() {
    if !live::enabled() {
        return;
    }
    let q = n("secure");
    let ctx = context();
    let addr = format!("amqp:queue:/queue/{}?remoteAddress=amqps://localhost:{}&sslContextParameters=#liveSsl", q, TLS_PORT);
    let from = addr.clone();
    ctx.add_routes(move |r| {
        r.from(&from).to("mock:secure");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:secure").unwrap();
    mock.expected_bodies_received(vec!["over tls"]);
    ctx.create_producer_template().send_body(&addr, "over tls").unwrap();
    mock.assert_is_satisfied_within(Duration::from_secs(10)).unwrap();
    let ex = ctx.create_producer_template().send(&format!("amqp:queue:/queue/{}?remoteAddress=amqps://localhost:{}&sslContextParameters=#liveUntrusted", q, TLS_PORT), Exchange::with_body(Some(ctx.clone()), "x"));
    assert!(ex.exception.is_some());
    ctx.stop().unwrap();
    delete_queues(&[&q]);
}

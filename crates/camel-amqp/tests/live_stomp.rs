//! Live interop: `stomp` and the JMS-style `jms` / `activemq` components against the RabbitMQ
//! STOMP plugin started by `scripts/live-interop.sh`. Runs only with `CAMEL_LIVE_INTEROP=1`.
//!
//! RabbitMQ's STOMP adapter has no JMS selectors (the `selector` header is ignored), so
//! selectors are not exercised here.

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use camel_amqp::amqp::client::{AmqpConfig, Connection, QueueOptions};
use camel_core::prelude::*;
use camel_tls::testing::live;

const PORT: u16 = 16613;
const TLS_PORT: u16 = 16614;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    camel_amqp::register(&ctx);
    live::bind_beans(&ctx);
    for scheme in ["jms", "activemq"] {
        ctx.properties().set(format!("camel.component.{}.brokerUrl", scheme), format!("tcp://127.0.0.1:{}", PORT));
        ctx.properties().set(format!("camel.component.{}.username", scheme), "camel");
        ctx.properties().set(format!("camel.component.{}.password", scheme), live::PASSWORD);
        // RabbitMQ maps the STOMP `host` header to the virtual host
        ctx.properties().set(format!("camel.component.{}.host", scheme), "/");
    }
    ctx
}

fn n(name: &str) -> String {
    format!("live-{}-{}", std::process::id(), name)
}

fn s(v: Option<&Value>) -> String {
    v.map(|v| v.to_string()).unwrap_or_default()
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out waiting for {}", what);
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn admin() -> Connection {
    Connection::open(&AmqpConfig { host: "127.0.0.1".into(), port: 15673, username: "camel".into(), password: live::PASSWORD.into(), heartbeat: 0, ..Default::default() }).unwrap()
}

/// Ready messages in a queue (STOMP `/queue/x` destinations are AMQP queues named `x`).
fn depth(conn: &Connection, queue: &str) -> u32 {
    let ch = conn.channel().unwrap();
    let r = ch.queue_declare(queue, &QueueOptions { passive: true, ..Default::default() }).map(|i| i.message_count).unwrap_or(0);
    let _ = ch.close();
    r
}

fn delete_queues(queues: &[&str]) {
    let conn = admin();
    let ch = conn.channel().unwrap();
    for q in queues {
        let _ = ch.queue_delete(q, false, false);
    }
    conn.close();
}

#[test]
fn live_stomp_component_round_trip_and_auth() {
    if !live::enabled() {
        return;
    }
    let q = n("inbox");
    let ctx = CamelContext::new();
    camel_amqp::register(&ctx);
    let url = format!("brokerURL=tcp://127.0.0.1:{}&login=camel&passcode={}&host=/", PORT, live::PASSWORD);
    let uri = format!("stomp:queue:{}?{}", q, url);
    let from = uri.clone();
    ctx.add_routes(move |b| {
        b.from(&from).to("mock:stomp");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:stomp").unwrap();
    mock.expected_bodies_received(vec!["hello stomp"]);
    mock.expected_header_received("destination", format!("/queue/{}", q));
    mock.expected_header_received("custom", "42");
    ctx.create_producer_template().send_body_and_header(&uri, "hello stomp", "custom", 42).unwrap();
    mock.assert_is_satisfied().unwrap();
    let ex = ctx.create_producer_template().send(&format!("stomp:queue:{}?brokerURL=tcp://127.0.0.1:{}&login=camel&passcode=wrong&host=/", q, PORT), Exchange::with_body(Some(ctx.clone()), "x"));
    assert_eq!(ex.exception.map(|e| e.kind), Some("AuthenticationFailureException".to_string()));
    ctx.stop().unwrap();
    delete_queues(&[&q]);
}

#[test]
fn live_stomp_jms_queues_topics_and_headers() {
    if !live::enabled() {
        return;
    }
    let (orders, shared, headers, prices) = (n("orders"), n("shared"), n("headers"), n("prices"));
    let ctx = context();
    let threads = Arc::new(Mutex::new(HashSet::new()));
    let t2 = threads.clone();
    let (o, sh, h, p) = (orders.clone(), shared.clone(), headers.clone(), prices.clone());
    ctx.add_routes(move |b| {
        b.from(&format!("jms:queue:{}?concurrentConsumers=3", o))
            .process(move |_ex: &mut Exchange| {
                t2.lock().unwrap().insert(format!("{:?}", std::thread::current().id()));
                std::thread::sleep(Duration::from_millis(20));
                Ok(())
            })
            .to("mock:orders");
        b.from(&format!("activemq:queue:{}", sh)).to("mock:a");
        b.from(&format!("jms:queue:{}", sh)).to("mock:b");
        b.from(&format!("jms:queue:{}", h)).to("mock:headers");
        b.from(&format!("jms:topic:{}", p)).to("mock:t1");
        b.from(&format!("activemq:topic:{}", p)).to("mock:t2");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let mock = ctx.mock_endpoint("mock:orders").unwrap();
    let bodies: Vec<String> = (0..9).map(|i| format!("order-{}", i)).collect();
    mock.expected_bodies_received_in_any_order(bodies.clone());
    for b in &bodies {
        t.send_body(&format!("jms:queue:{}", orders), b.as_str()).unwrap();
    }
    mock.assert_is_satisfied().unwrap();
    assert!(threads.lock().unwrap().len() >= 2, "concurrent consumers took part: {:?}", threads.lock().unwrap());
    let (a, b) = (ctx.mock_endpoint("mock:a").unwrap(), ctx.mock_endpoint("mock:b").unwrap());
    for i in 0..10 {
        t.send_body(&format!("jms:{}", shared), format!("s{}", i)).unwrap();
    }
    wait_until("all shared messages consumed", || a.received_counter() + b.received_counter() == 10);
    assert!(a.received_counter() > 0 && b.received_counter() > 0, "both consumers compete");

    // topics fan out to every subscriber
    let (t1, t2m) = (ctx.mock_endpoint("mock:t1").unwrap(), ctx.mock_endpoint("mock:t2").unwrap());
    t1.expected_bodies_received(vec!["p1", "p2"]);
    t2m.expected_bodies_received(vec!["p1", "p2"]);
    t.send_body(&format!("jms:topic:{}", prices), "p1").unwrap();
    t.send_body(&format!("jms:topic:{}", prices), "p2").unwrap();
    MockEndpoint::assert_all_satisfied(&[&t1, &t2m]).unwrap();

    // JMS headers and properties through RabbitMQ
    let hm = ctx.mock_endpoint("mock:headers").unwrap();
    hm.expected_message_count(1);
    t.send_body_and_headers(
        &format!("jms:queue:{}?priority=7&timeToLive=60000&deliveryPersistent=false", headers),
        "payload",
        vec![("JMSCorrelationID", Value::from("corr-9")), ("JMSType", Value::from("OrderCreated")), ("my.dotted-key", Value::from("v")), ("count", Value::Int(3)), ("CamelInternal", Value::from("skip"))],
    )
    .unwrap();
    hm.assert_is_satisfied().unwrap();
    let ex = hm.exchange(0).unwrap();
    assert!(!s(ex.header("JMSMessageID")).is_empty());
    assert_eq!(s(ex.header("JMSCorrelationID")), "corr-9");
    assert_eq!(s(ex.header("JMSType")), "OrderCreated");
    assert_eq!(s(ex.header("JMSDestination")), format!("queue://{}", headers));
    assert_eq!(ex.header("JMSPriority").and_then(|v| v.as_i64()), Some(7));
    assert_eq!(ex.header("JMSDeliveryMode").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(ex.header("JMSRedelivered").and_then(|v| v.as_bool()), Some(false));
    assert!(ex.header("JMSExpiration").and_then(|v| v.as_i64()).unwrap_or(0) > 0, "{:?}", ex.message.headers);
    assert_eq!(s(ex.header("my.dotted-key")), "v");
    assert_eq!(s(ex.header("count")), "3");
    assert!(ex.header("CamelInternal").is_none());
    // bytes bodies (content-length framing with NUL bytes)
    hm.reset();
    hm.expected_message_count(1);
    t.send_body(&format!("jms:queue:{}", headers), Value::Bytes(vec![0, 1, 0, 255])).unwrap();
    hm.assert_is_satisfied().unwrap();
    assert_eq!(hm.exchange(0).unwrap().body().clone(), Value::Bytes(vec![0, 1, 0, 255]));
    ctx.stop().unwrap();
    delete_queues(&[&orders, &shared, &headers]);
}

#[test]
fn live_stomp_jms_request_reply() {
    if !live::enabled() {
        return;
    }
    let (greeter, fails, fixed) = (n("greeter"), n("fails"), n("fixed-replies"));
    let ctx = context();
    let (g, f, fx) = (greeter.clone(), fails.clone(), fixed.clone());
    ctx.add_routes(move |b| {
        b.from(&format!("jms:queue:{}?concurrentConsumers=2", g)).transform(simple("Hello ${body}")).set_header("handled-by", constant("greeter"));
        b.from(&format!("activemq:queue:{}", f)).process(|_ex: &mut Exchange| Err(CamelError::new("InsufficientFundsException", "balance too low")));
        b.from("direct:call").to_with_pattern(ExchangePattern::InOut, &format!("jms:queue:{}?replyTo={}", g, fx)).to("mock:reply");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for i in 0..5 {
        let r = t.request_body(&format!("jms:queue:{}", greeter), format!("Camel{}", i)).unwrap_or_else(|e| panic!("request {}: {}", i, e));
        assert_eq!(r.to_string(), format!("Hello Camel{}", i));
    }
    let ex = t
        .request(&format!("jms:queue:{}", greeter), |ex| {
            ex.set_body("World");
            ex.set_header("JMSCorrelationID", "my-corr");
        })
        .unwrap();
    assert_eq!(ex.body().to_string(), "Hello World");
    assert_eq!(s(ex.header("JMSCorrelationID")), "my-corr");
    assert_eq!(s(ex.header("handled-by")), "greeter");
    let mock = ctx.mock_endpoint("mock:reply").unwrap();
    mock.expected_bodies_received(vec!["Hello Route"]);
    t.send_body("direct:call", "Route").unwrap();
    mock.assert_is_satisfied().unwrap();
    let err = t.request_body(&format!("activemq:queue:{}", fails), "x").unwrap_err();
    let cause = err.cause.as_deref().unwrap_or(&err);
    assert_eq!(cause.kind, "InsufficientFundsException", "{:?}", err);
    let start = Instant::now();
    let err = t.request_body(&format!("jms:queue:{}?requestTimeout=300", n("nobody")), "x").unwrap_err();
    let cause = err.cause.as_deref().unwrap_or(&err);
    assert_eq!(cause.kind, "ExchangeTimedOutException");
    assert!(start.elapsed() < Duration::from_secs(5));
    ctx.stop().unwrap();
    delete_queues(&[&greeter, &fails, &fixed, &n("nobody")]);
}

#[test]
fn live_stomp_jms_transactions_and_client_ack() {
    if !live::enabled() {
        return;
    }
    let (payments, audit, client, auto) = (n("payments"), n("audit"), n("client"), n("auto"));
    let ctx = context();
    let attempts = Arc::new(AtomicUsize::new(0));
    let a2 = attempts.clone();
    let flags = Arc::new(Mutex::new(Vec::new()));
    let f2 = flags.clone();
    let client_attempts = Arc::new(AtomicUsize::new(0));
    let c2 = client_attempts.clone();
    let (p, au, c, at) = (payments.clone(), audit.clone(), client.clone(), auto.clone());
    ctx.add_routes(move |b| {
        b.from(&format!("jms:queue:{}?transacted=true", p))
            .process(move |ex: &mut Exchange| {
                f2.lock().unwrap().push(ex.header("JMSRedelivered").and_then(|v| v.as_bool()).unwrap_or(false));
                if a2.fetch_add(1, Ordering::SeqCst) < 2 {
                    return Err(CamelError::illegal_state("database deadlock"));
                }
                Ok(())
            })
            .to(&format!("jms:queue:{}?transacted=true", au))
            .to("mock:committed");
        b.from(&format!("jms:queue:{}?acknowledgementModeName=CLIENT_ACKNOWLEDGE", c))
            .process(move |_ex: &mut Exchange| {
                if c2.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(CamelError::illegal_state("first time fails"));
                }
                Ok(())
            })
            .to("mock:client");
        b.from(&format!("jms:queue:{}", at)).process(|_ex: &mut Exchange| Err(CamelError::illegal_state("always fails"))).to("mock:never");
    })
    .unwrap();
    ctx.start().unwrap();
    let conn = admin();
    let committed = ctx.mock_endpoint("mock:committed").unwrap();
    committed.expected_bodies_received(vec!["pay 10"]);
    committed.expected_header_received("JMSRedelivered", true);
    let t = ctx.create_producer_template();
    t.send_body(&format!("jms:queue:{}", payments), "pay 10").unwrap();
    committed.assert_is_satisfied().unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    assert_eq!(*flags.lock().unwrap(), vec![false, true, true]);
    wait_until("transaction settled", || depth(&conn, &payments) == 0);
    wait_until("audit message committed", || depth(&conn, &audit) == 1);
    let client_mock = ctx.mock_endpoint("mock:client").unwrap();
    client_mock.expected_header_received("JMSRedelivered", true);
    client_mock.expected_message_count(1);
    t.send_body(&format!("jms:queue:{}", client), "c").unwrap();
    client_mock.assert_is_satisfied().unwrap();
    t.send_body(&format!("jms:queue:{}", auto), "a").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(ctx.mock_endpoint("mock:never").unwrap().received_counter(), 0);
    assert_eq!(depth(&conn, &auto), 0, "AUTO_ACKNOWLEDGE acknowledges failures");
    ctx.stop().unwrap();
    delete_queues(&[&payments, &audit, &client, &auto]);
}

#[test]
fn live_stomp_durable_topic_subscription() {
    if !live::enabled() {
        return;
    }
    let topic = n("news");
    let ctx = context();
    let tp = topic.clone();
    ctx.add_routes(move |b| {
        b.from(&format!("jms:topic:{}?durableSubscriptionName=reader&clientId=live-{}", tp, std::process::id())).route_id("durable").to("mock:durable");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:durable").unwrap();
    mock.expected_bodies_received(vec!["online"]);
    let t = ctx.create_producer_template();
    t.send_body(&format!("jms:topic:{}", topic), "online").unwrap();
    mock.assert_is_satisfied().unwrap();
    // while the route is stopped RabbitMQ keeps the messages in the subscription's queue
    ctx.stop_route("durable").unwrap();
    t.send_body(&format!("jms:topic:{}", topic), "while-offline").unwrap();
    mock.reset();
    mock.expected_bodies_received(vec!["while-offline"]);
    ctx.start_route("durable").unwrap();
    mock.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
}

#[test]
fn live_stomp_over_tls() {
    if !live::enabled() {
        return;
    }
    let q = n("secure");
    let ctx = context();
    let uri = format!("jms:queue:{}?brokerURL=stomp+ssl://localhost:{}&sslContextParameters=#liveSsl", q, TLS_PORT);
    let from = uri.clone();
    ctx.add_routes(move |b| {
        b.from(&from).to("mock:secure");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:secure").unwrap();
    mock.expected_bodies_received(vec!["over tls"]);
    ctx.create_producer_template().send_body(&uri, "over tls").unwrap();
    mock.assert_is_satisfied().unwrap();
    let ex = ctx.create_producer_template().send(&format!("jms:queue:{}?brokerURL=stomp+ssl://localhost:{}&sslContextParameters=#liveUntrusted", q, TLS_PORT), Exchange::with_body(Some(ctx.clone()), "x"));
    assert!(ex.exception.is_some());
    ctx.stop().unwrap();
    delete_queues(&[&q]);
}

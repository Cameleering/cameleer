//! Live interop: `stomp` and the JMS-style `jms` / `activemq` components against a real Apache
//! ActiveMQ 5.17 broker (Ubuntu package) started by `scripts/live-interop.sh`, over its STOMP
//! and STOMP+SSL transport connectors with the simple authentication plugin. Runs only with
//! `CAMEL_LIVE_INTEROP=1`.
//!
//! ActiveMQ is the broker the `activemq` component is named after, so this suite covers what
//! RabbitMQ's STOMP adapter cannot: JMS selectors evaluated by the broker, `/temp-queue/`
//! request/reply, `activemq.subscriptionName` durable subscriptions, ActiveMQ's transaction /
//! redelivery semantics, message priority and expiry.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use camel_amqp::stomp::client::{AckMode, StompConfig, StompConnection};
use camel_core::prelude::*;
use camel_tls::testing::live;

const PORT: u16 = 16615;
const TLS_PORT: u16 = 16616;

fn broker_url() -> String {
    format!("tcp://127.0.0.1:{}", PORT)
}

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    camel_amqp::register(&ctx);
    live::bind_beans(&ctx);
    for scheme in ["jms", "activemq"] {
        // ActiveMQ connection factory style options: fast client-side redelivery
        ctx.properties().set(format!("camel.component.{}.brokerUrl", scheme), format!("{}?jms.redeliveryPolicy.initialRedeliveryDelay=20&jms.redeliveryPolicy.redeliveryDelay=20", broker_url()));
        ctx.properties().set(format!("camel.component.{}.username", scheme), "camel");
        ctx.properties().set(format!("camel.component.{}.password", scheme), live::PASSWORD);
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

fn raw_config() -> StompConfig {
    StompConfig { host: "127.0.0.1".into(), port: PORT, login: Some("camel".into()), passcode: Some(live::PASSWORD.into()), ..Default::default() }
}

/// Consumes (and acknowledges) what is left in a destination: the bodies received until
/// nothing arrives for `quiet`.
fn drain(destination: &str, quiet: Duration) -> Vec<String> {
    let conn = StompConnection::open(&raw_config()).unwrap();
    let sub = conn.subscribe(destination, AckMode::Auto, &[]).unwrap();
    let mut out = Vec::new();
    while let Ok(f) = sub.messages.recv_timeout(quiet) {
        out.push(f.body_string());
    }
    conn.disconnect();
    out
}

#[test]
fn live_activemq_stomp_component_and_authentication() {
    if !live::enabled() {
        return;
    }
    let q = n("inbox");
    let ctx = CamelContext::new();
    camel_amqp::register(&ctx);
    let uri = format!("stomp:queue:{}?brokerURL={}&login=camel&passcode={}", q, broker_url(), live::PASSWORD);
    let from = uri.clone();
    ctx.add_routes(move |b| {
        b.from(&from).to("mock:stomp");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:stomp").unwrap();
    mock.expected_bodies_received(vec!["hello activemq"]);
    mock.expected_header_received("destination", format!("/queue/{}", q));
    mock.expected_header_received("custom", "42");
    ctx.create_producer_template().send_body_and_header(&uri, "hello activemq", "custom", 42).unwrap();
    mock.assert_is_satisfied().unwrap();
    // a wrong password and a missing login are refused by the simple authentication plugin
    for creds in ["&login=camel&passcode=wrong", ""] {
        let ex = ctx.create_producer_template().send(&format!("stomp:queue:{}?brokerURL={}{}", q, broker_url(), creds), Exchange::with_body(Some(ctx.clone()), "x"));
        let e = ex.exception.expect("refused");
        assert_eq!(e.kind, "AuthenticationFailureException", "{:?}", e);
    }
    let jctx = context();
    jctx.properties().set("camel.component.jms.password", "wrong");
    let err = jctx.create_producer_template().send_body(&format!("jms:queue:{}", q), "x").unwrap_err();
    let cause = err.cause.as_deref().unwrap_or(&err);
    assert_eq!(cause.kind, "AuthenticationFailureException", "{:?}", err);
    ctx.stop().unwrap();
}

#[test]
fn live_activemq_queues_topics_headers_and_types() {
    if !live::enabled() {
        return;
    }
    let (shared, headers, prices) = (n("shared"), n("headers"), n("prices"));
    let ctx = context();
    let (sh, h, p) = (shared.clone(), headers.clone(), prices.clone());
    ctx.add_routes(move |b| {
        b.from(&format!("activemq:queue:{}", sh)).to("mock:a");
        b.from(&format!("jms:queue:{}?concurrentConsumers=2", sh)).to("mock:b");
        b.from(&format!("jms:queue:{}", h)).to("mock:headers");
        b.from(&format!("jms:topic:{}", p)).to("mock:t1");
        b.from(&format!("activemq:topic:{}", p)).to("mock:t2");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let (a, b) = (ctx.mock_endpoint("mock:a").unwrap(), ctx.mock_endpoint("mock:b").unwrap());
    for i in 0..12 {
        t.send_body(&format!("jms:{}", shared), format!("s{}", i)).unwrap();
    }
    wait_until("all shared messages consumed", || a.received_counter() + b.received_counter() == 12);
    assert!(a.received_counter() > 0 && b.received_counter() > 0, "competing consumers");

    let (t1, t2) = (ctx.mock_endpoint("mock:t1").unwrap(), ctx.mock_endpoint("mock:t2").unwrap());
    t1.expected_bodies_received(vec!["p1", "p2"]);
    t2.expected_bodies_received(vec!["p1", "p2"]);
    t.send_body(&format!("jms:topic:{}", prices), "p1").unwrap();
    t.send_body(&format!("activemq:topic:{}", prices), "p2").unwrap();
    MockEndpoint::assert_all_satisfied(&[&t1, &t2]).unwrap();
    assert_eq!(s(t1.exchange(0).unwrap().header("JMSDestination")), format!("topic://{}", prices));

    // JMS headers, QoS and properties as ActiveMQ maps them
    let hm = ctx.mock_endpoint("mock:headers").unwrap();
    hm.expected_message_count(2);
    t.send_body_and_headers(
        &format!("jms:queue:{}?priority=7&timeToLive=60000&deliveryPersistent=false", headers),
        "non-persistent",
        vec![("JMSCorrelationID", Value::from("corr-9")), ("JMSType", Value::from("OrderCreated")), ("my.dotted-key", Value::from("v")), ("count", Value::Int(3)), ("CamelInternal", Value::from("skip"))],
    )
    .unwrap();
    t.send_body(&format!("jms:queue:{}", headers), "persistent").unwrap();
    hm.assert_is_satisfied().unwrap();
    let ex = hm.exchange(0).unwrap();
    assert_eq!(ex.body().to_string(), "non-persistent");
    assert!(s(ex.header("JMSMessageID")).starts_with("ID:"), "ActiveMQ message id: {:?}", ex.header("JMSMessageID"));
    assert_eq!(s(ex.header("JMSCorrelationID")), "corr-9");
    assert_eq!(s(ex.header("JMSType")), "OrderCreated");
    assert_eq!(s(ex.header("JMSDestination")), format!("queue://{}", headers));
    assert_eq!(ex.header("JMSPriority").and_then(|v| v.as_i64()), Some(7));
    assert_eq!(ex.header("JMSDeliveryMode").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(ex.header("JMSRedelivered").and_then(|v| v.as_bool()), Some(false));
    let exp = ex.header("JMSExpiration").and_then(|v| v.as_i64()).unwrap_or(0);
    let ts = ex.header("JMSTimestamp").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(ts > 0 && exp - ts > 50_000 && exp - ts <= 60_000, "expiration {} timestamp {}", exp, ts);
    assert_eq!(s(ex.header("my.dotted-key")), "v");
    assert_eq!(s(ex.header("count")), "3");
    assert!(ex.header("CamelInternal").is_none());
    let ex = hm.exchange(1).unwrap();
    assert_eq!(ex.header("JMSDeliveryMode").and_then(|v| v.as_i64()), Some(2), "persistent by default");
    assert_eq!(ex.header("JMSPriority").and_then(|v| v.as_i64()), Some(4), "ActiveMQ default priority");
    assert_eq!(ex.header("JMSExpiration").and_then(|v| v.as_i64()).unwrap_or(0), 0);

    // bytes (content-length framing, NUL bytes) and UTF-8 text
    hm.reset();
    hm.expected_message_count(2);
    t.send_body(&format!("jms:queue:{}", headers), Value::Bytes(vec![0, 1, 0, 255])).unwrap();
    t.send_body(&format!("jms:queue:{}", headers), "grüße ✓").unwrap();
    hm.assert_is_satisfied().unwrap();
    assert_eq!(hm.exchange(0).unwrap().body().clone(), Value::Bytes(vec![0, 1, 0, 255]));
    assert_eq!(hm.exchange(1).unwrap().body().to_string(), "grüße ✓");
    ctx.stop().unwrap();
}

#[test]
fn live_activemq_jms_selectors() {
    if !live::enabled() {
        return;
    }
    let (orders, alerts) = (n("sel-orders"), n("sel-alerts"));
    let ctx = context();
    let (o, a) = (orders.clone(), alerts.clone());
    ctx.add_routes(move |b| {
        b.from(&format!("jms:queue:{}?selector=region = 'EU' AND amount > 100", o)).to("mock:eu-big");
        b.from(&format!("jms:topic:{}?selector=JMSPriority >= 7 OR severity IN ('high', 'critical')", a)).to("mock:urgent");
        b.from(&format!("jms:topic:{}", a)).to("mock:all-alerts");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let eu = ctx.mock_endpoint("mock:eu-big").unwrap();
    eu.expected_bodies_received(vec!["o2", "o4"]);
    let sends: [(&str, &str, i64); 5] = [("o1", "EU", 50), ("o2", "EU", 150), ("o3", "US", 500), ("o4", "EU", 101), ("o5", "APAC", 1000)];
    for (body, region, amount) in sends {
        t.send_body_and_headers(&format!("jms:queue:{}", orders), body, vec![("region", Value::from(region)), ("amount", Value::Int(amount))]).unwrap();
    }
    eu.assert_is_satisfied().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(eu.received_counter(), 2, "the broker filtered the rest");
    let urgent = ctx.mock_endpoint("mock:urgent").unwrap();
    urgent.expected_bodies_received(vec!["by-priority", "by-severity"]);
    let all = ctx.mock_endpoint("mock:all-alerts").unwrap();
    all.expected_message_count(4);
    t.send_body(&format!("jms:topic:{}?priority=8", alerts), "by-priority").unwrap();
    t.send_body_and_header(&format!("jms:topic:{}", alerts), "by-severity", "severity", "critical").unwrap();
    t.send_body_and_header(&format!("jms:topic:{}", alerts), "low", "severity", "low").unwrap();
    t.send_body(&format!("jms:topic:{}", alerts), "plain").unwrap();
    MockEndpoint::assert_all_satisfied(&[&urgent, &all]).unwrap();
    ctx.stop().unwrap();
    // the messages the selector skipped stay on the queue
    let mut left = drain(&format!("/queue/{}", orders), Duration::from_millis(500));
    left.sort();
    assert_eq!(left, vec!["o1", "o3", "o5"]);
}

#[test]
fn live_activemq_request_reply() {
    if !live::enabled() {
        return;
    }
    let (greeter, fails, fixed, exclusive, by_id) = (n("greeter"), n("fails"), n("fixed-replies"), n("excl-replies"), n("by-id"));
    let ctx = context();
    let (g, f, bi) = (greeter.clone(), fails.clone(), by_id.clone());
    ctx.add_routes(move |b| {
        b.from(&format!("jms:queue:{}?concurrentConsumers=3", g)).transform(simple("Hello ${body}")).set_header("handled-by", constant("greeter"));
        b.from(&format!("activemq:queue:{}", f)).process(|_ex: &mut Exchange| Err(CamelError::new("InsufficientFundsException", "balance too low")));
        b.from(&format!("jms:queue:{}?useMessageIDAsCorrelationID=true", bi)).transform(simple("id ${body}"));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    // temporary reply queues (ActiveMQ /temp-queue/), concurrently
    let errors = Arc::new(Mutex::new(Vec::new()));
    let threads: Vec<_> = (0..4)
        .map(|th| {
            let (ctx, g, errors) = (ctx.clone(), greeter.clone(), errors.clone());
            std::thread::spawn(move || {
                let t = ctx.create_producer_template();
                for i in 0..5 {
                    match t.request_body(&format!("jms:queue:{}", g), format!("C{}-{}", th, i)) {
                        Ok(r) if r.to_string() == format!("Hello C{}-{}", th, i) => {}
                        other => errors.lock().unwrap().push(format!("{:?}", other)),
                    }
                }
            })
        })
        .collect();
    for th in threads {
        th.join().unwrap();
    }
    assert!(errors.lock().unwrap().is_empty(), "{:?}", errors.lock().unwrap());
    let ex = t
        .request(&format!("jms:queue:{}", greeter), |ex| {
            ex.set_body("World");
            ex.set_header("JMSCorrelationID", "my-corr");
        })
        .unwrap();
    assert_eq!(ex.body().to_string(), "Hello World");
    assert_eq!(s(ex.header("JMSCorrelationID")), "my-corr");
    assert_eq!(s(ex.header("handled-by")), "greeter");
    // a fixed, shared reply queue: each producer reads its reply with a JMSCorrelationID
    // selector evaluated by ActiveMQ, so two concurrent producers never steal replies
    let threads: Vec<_> = (0..2)
        .map(|th| {
            let (ctx, g, fx, errors) = (ctx.clone(), greeter.clone(), fixed.clone(), errors.clone());
            std::thread::spawn(move || {
                let t = ctx.create_producer_template();
                for i in 0..4 {
                    match t.request_body(&format!("jms:queue:{}?replyTo={}", g, fx), format!("F{}-{}", th, i)) {
                        Ok(r) if r.to_string() == format!("Hello F{}-{}", th, i) => {}
                        other => errors.lock().unwrap().push(format!("{:?}", other)),
                    }
                }
            })
        })
        .collect();
    for th in threads {
        th.join().unwrap();
    }
    assert!(errors.lock().unwrap().is_empty(), "{:?}", errors.lock().unwrap());
    for i in 0..3 {
        let r = t.request_body(&format!("jms:queue:{}?replyTo={}&replyToType=Exclusive", greeter, exclusive), format!("E{}", i)).unwrap();
        assert_eq!(r.to_string(), format!("Hello E{}", i));
    }
    let r = t.request_body(&format!("jms:queue:{}?useMessageIDAsCorrelationID=true", by_id), "x").unwrap();
    assert_eq!(r.to_string(), "id x");
    // remote failure and timeout
    let err = t.request_body(&format!("activemq:queue:{}", fails), "x").unwrap_err();
    let cause = err.cause.as_deref().unwrap_or(&err);
    assert_eq!(cause.kind, "InsufficientFundsException", "{:?}", err);
    assert!(cause.message.contains("balance too low"));
    let start = Instant::now();
    let err = t.request_body(&format!("jms:queue:{}?requestTimeout=300", n("nobody")), "x").unwrap_err();
    let cause = err.cause.as_deref().unwrap_or(&err);
    assert_eq!(cause.kind, "ExchangeTimedOutException");
    assert!(start.elapsed() < Duration::from_secs(5));
    ctx.stop().unwrap();
    // no stray replies were left on the shared queues
    assert!(drain(&format!("/queue/{}", fixed), Duration::from_millis(300)).is_empty());
    assert!(drain(&format!("/queue/{}", exclusive), Duration::from_millis(300)).is_empty());
    let _ = drain(&format!("/queue/{}", n("nobody")), Duration::from_millis(200));
}

#[test]
fn live_activemq_transactions_and_redelivery() {
    if !live::enabled() {
        return;
    }
    let (payments, audit, client, auto, rollback) = (n("payments"), n("audit"), n("client"), n("auto"), n("rollback"));
    let ctx = context();
    let attempts = Arc::new(AtomicUsize::new(0));
    let a2 = attempts.clone();
    let flags = Arc::new(Mutex::new(Vec::new()));
    let f2 = flags.clone();
    let client_attempts = Arc::new(AtomicUsize::new(0));
    let c2 = client_attempts.clone();
    let (p, au, c, at, rb) = (payments.clone(), audit.clone(), client.clone(), auto.clone(), rollback.clone());
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
        // the transactional send is rolled back with the consumed message
        b.from(&format!("jms:queue:{}?transacted=true", rb)).to(&format!("jms:queue:{}?transacted=true", au)).process(|_ex: &mut Exchange| Err(CamelError::illegal_state("always fails")));
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
    let committed = ctx.mock_endpoint("mock:committed").unwrap();
    committed.expected_bodies_received(vec!["pay 10"]);
    committed.expected_header_received("JMSRedelivered", true);
    let t = ctx.create_producer_template();
    t.send_body(&format!("jms:queue:{}", payments), "pay 10").unwrap();
    committed.assert_is_satisfied().unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    assert_eq!(*flags.lock().unwrap(), vec![false, true, true]);

    // a message that always fails: its transactional send is never committed, and ActiveMQ
    // moves it to the dead letter queue after its redelivery policy (6 redeliveries)
    t.send_body(&format!("jms:queue:{}", rollback), "doomed").unwrap();
    let dlq = format!("/queue/DLQ.{}", rollback);
    let start = Instant::now();
    let mut dead = Vec::new();
    while dead.is_empty() && start.elapsed() < Duration::from_secs(15) {
        dead = drain(&dlq, Duration::from_millis(300));
    }
    assert_eq!(dead, vec!["doomed"], "dead-lettered by ActiveMQ");

    let client_mock = ctx.mock_endpoint("mock:client").unwrap();
    client_mock.expected_header_received("JMSRedelivered", true);
    client_mock.expected_message_count(1);
    t.send_body(&format!("jms:queue:{}", client), "c").unwrap();
    client_mock.assert_is_satisfied().unwrap();
    t.send_body(&format!("jms:queue:{}", auto), "a").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(ctx.mock_endpoint("mock:never").unwrap().received_counter(), 0);
    ctx.stop().unwrap();
    assert_eq!(drain(&format!("/queue/{}", audit), Duration::from_millis(300)), vec!["pay 10"], "only the committed send");
    assert!(drain(&format!("/queue/{}", payments), Duration::from_millis(200)).is_empty(), "transaction settled");
    assert!(drain(&format!("/queue/{}", auto), Duration::from_millis(200)).is_empty(), "AUTO_ACKNOWLEDGE acknowledges failures");
    assert!(drain(&format!("/queue/DLQ.{}", auto), Duration::from_millis(200)).is_empty());
}

#[test]
fn live_activemq_durable_subscription() {
    if !live::enabled() {
        return;
    }
    let topic = n("news");
    let ctx = context();
    let tp = topic.clone();
    ctx.add_routes(move |b| {
        b.from(&format!("jms:topic:{}?durableSubscriptionName=reader&clientId=live-{}", tp, std::process::id())).route_id("durable").to("mock:durable");
        b.from(&format!("activemq:topic:{}?subscriptionDurable=true&subscriptionName=other&clientId=live-other-{}&selector=kind = 'keep'", tp, std::process::id()))
            .route_id("durable-selector")
            .to("mock:durable-selector");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:durable").unwrap();
    let sel = ctx.mock_endpoint("mock:durable-selector").unwrap();
    mock.expected_bodies_received(vec!["online"]);
    sel.expected_bodies_received(vec!["online"]);
    let t = ctx.create_producer_template();
    t.send_body_and_header(&format!("jms:topic:{}", topic), "online", "kind", "keep").unwrap();
    MockEndpoint::assert_all_satisfied(&[&mock, &sel]).unwrap();
    // ActiveMQ keeps the messages of the offline durable subscriptions
    ctx.stop_route("durable").unwrap();
    ctx.stop_route("durable-selector").unwrap();
    t.send_body_and_header(&format!("jms:topic:{}", topic), "while-offline-1", "kind", "keep").unwrap();
    t.send_body_and_header(&format!("jms:topic:{}", topic), "while-offline-2", "kind", "drop").unwrap();
    mock.reset();
    sel.reset();
    mock.expected_bodies_received(vec!["while-offline-1", "while-offline-2"]);
    sel.expected_bodies_received(vec!["while-offline-1"]);
    ctx.start_route("durable").unwrap();
    ctx.start_route("durable-selector").unwrap();
    MockEndpoint::assert_all_satisfied(&[&mock, &sel]).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(sel.received_counter(), 1);
    ctx.stop().unwrap();
}

#[test]
fn live_activemq_priority_and_expiry() {
    if !live::enabled() {
        return;
    }
    let (prio, ttl) = (n("prio"), n("ttl"));
    let ctx = context();
    let t = ctx.create_producer_template();
    // ActiveMQ (prioritizedMessages) dispatches waiting messages by priority
    for (body, p) in [("low-1", 1), ("high", 9), ("low-2", 1), ("mid", 5)] {
        t.send_body(&format!("jms:queue:{}?priority={}", prio, p), body).unwrap();
    }
    // expired messages are never delivered (ActiveMQ moves them to the DLQ)
    t.send_body(&format!("jms:queue:{}?timeToLive=200", ttl), "short-lived").unwrap();
    t.send_body(&format!("jms:queue:{}?timeToLive=60000", ttl), "long-lived").unwrap();
    std::thread::sleep(Duration::from_millis(600));
    let (pr, tl) = (prio.clone(), ttl.clone());
    ctx.add_routes(move |b| {
        b.from(&format!("jms:queue:{}?prefetchSize=1", pr)).to("mock:prio");
        b.from(&format!("jms:queue:{}", tl)).to("mock:ttl");
    })
    .unwrap();
    ctx.start().unwrap();
    let pm = ctx.mock_endpoint("mock:prio").unwrap();
    pm.expected_bodies_received(vec!["high", "mid", "low-1", "low-2"]);
    let tm = ctx.mock_endpoint("mock:ttl").unwrap();
    tm.expected_bodies_received(vec!["long-lived"]);
    MockEndpoint::assert_all_satisfied(&[&pm, &tm]).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(tm.received_counter(), 1);
    ctx.stop().unwrap();
}

#[test]
fn live_activemq_stomp_over_tls() {
    if !live::enabled() {
        return;
    }
    let q = n("secure");
    let ctx = context();
    let uri = format!("activemq:queue:{}?brokerURL=ssl://localhost:{}&sslContextParameters=#liveSsl", q, TLS_PORT);
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
    let r = ctx.create_producer_template().request_body(&format!("activemq:queue:{}?brokerURL=stomp+ssl://127.0.0.1:{}&sslContextParameters=#liveSsl&requestTimeout=2000", n("tls-nobody"), TLS_PORT), "x");
    assert_eq!(r.unwrap_err().cause.map(|c| c.kind), Some("ExchangeTimedOutException".to_string()), "TLS with the IP SAN");
    let ex = ctx.create_producer_template().send(&format!("jms:queue:{}?brokerURL=stomp+ssl://localhost:{}&sslContextParameters=#liveUntrusted", q, TLS_PORT), Exchange::with_body(Some(ctx.clone()), "x"));
    assert!(ex.exception.is_some(), "untrusted CA refused");
    ctx.stop().unwrap();
    let _ = drain(&format!("/queue/{}", n("tls-nobody")), Duration::from_millis(200));
}

/// The broker-side semantics rust-camel's JMS emulation relies on (and the embedded broker's
/// ActiveMQ flavour reproduces): an aborted transactional ACK only restores the pending
/// acknowledgement (no redelivery), a NACK is a poison ack (straight to the dead letter
/// queue), and a disconnect redelivers unacknowledged messages with `redelivered:true`.
#[test]
fn live_activemq_raw_stomp_semantics() {
    if !live::enabled() {
        return;
    }
    let (q, dq) = (format!("/queue/{}", n("raw")), format!("/queue/{}", n("raw-nack")));
    let conn = StompConnection::open(&raw_config()).unwrap();
    assert!(conn.server().starts_with("ActiveMQ/5.17"), "{}", conn.server());
    conn.send(&q, &[], b"m1", None, true).unwrap();
    let sub = conn.subscribe(&q, AckMode::ClientIndividual, &[("activemq.prefetchSize".into(), "1".into())]).unwrap();
    let f = sub.messages.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(f.get("redelivered"), None, "only redeliveries carry the header");
    let ack = f.get("ack").unwrap().to_string();
    conn.begin("t1").unwrap();
    conn.ack(&ack, Some("t1")).unwrap();
    conn.abort("t1").unwrap();
    assert!(sub.messages.recv_timeout(Duration::from_millis(500)).is_err(), "ABORT does not redeliver");
    // the restored ack id is valid again
    conn.begin("t2").unwrap();
    conn.ack(&ack, Some("t2")).unwrap();
    conn.commit("t2").unwrap();
    assert!(drain(&q, Duration::from_millis(300)).is_empty());

    conn.send(&dq, &[("persistent".into(), "true".into())], b"m2", None, true).unwrap();
    let sub2 = conn.subscribe(&dq, AckMode::ClientIndividual, &[]).unwrap();
    let f = sub2.messages.recv_timeout(Duration::from_secs(5)).unwrap();
    conn.nack(f.get("ack").unwrap(), None).unwrap();
    assert!(sub2.messages.recv_timeout(Duration::from_millis(500)).is_err(), "NACK does not redeliver");
    conn.disconnect();
    assert_eq!(drain(&format!("/queue/DLQ.{}", n("raw-nack")), Duration::from_millis(500)), vec!["m2"], "NACK is a poison ack");

    // unacknowledged at disconnect: redelivered
    let c1 = StompConnection::open(&raw_config()).unwrap();
    c1.send(&q, &[], b"m3", None, true).unwrap();
    let s1 = c1.subscribe(&q, AckMode::ClientIndividual, &[]).unwrap();
    assert_eq!(s1.messages.recv_timeout(Duration::from_secs(5)).unwrap().body_string(), "m3");
    c1.disconnect();
    let c2 = StompConnection::open(&raw_config()).unwrap();
    let s2 = c2.subscribe(&q, AckMode::Auto, &[]).unwrap();
    let f = s2.messages.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!((f.body_string().as_str(), f.get("redelivered")), ("m3", Some("true")));
    c2.disconnect();
}

#[test]
fn live_activemq_map_and_object_messages() {
    if !live::enabled() {
        return;
    }
    let (maps, objects) = (n("maps"), n("objects"));
    let ctx = context();
    let (m2, o2) = (maps.clone(), objects.clone());
    ctx.add_routes(move |b| {
        b.from(&format!("jms:queue:{}", m2)).to("mock:maps");
        b.from(&format!("jms:queue:{}", o2)).to("mock:objects");
    })
    .unwrap();
    ctx.start().unwrap();
    let mut m = camel_core::value::ObjectMap::new();
    m.insert("id".into(), Value::Int(7));
    m.insert("name".into(), Value::from("widget"));
    let mock = ctx.mock_endpoint("mock:maps").unwrap();
    mock.expected_bodies_received(vec![Value::Object(m.clone())]);
    let t = ctx.create_producer_template();
    t.send_body(&format!("jms:queue:{}", maps), Value::Object(m)).unwrap();
    mock.assert_is_satisfied().unwrap();
    let objs = ctx.mock_endpoint("mock:objects").unwrap();
    objs.expected_bodies_received(vec![Value::Array(vec![Value::Int(1), Value::from("x")])]);
    t.send_body(&format!("jms:queue:{}?jmsMessageType=Object", objects), Value::Array(vec![Value::Int(1), Value::from("x")])).unwrap();
    objs.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
}

/// ActiveMQ connection factory style redelivery options in the broker url; exhausted
/// messages are NACKed, which ActiveMQ dead-letters.
#[test]
fn live_activemq_redelivery_policy() {
    if !live::enabled() {
        return;
    }
    let q = n("policy");
    let ctx = context();
    ctx.properties().set("camel.component.jms.brokerUrl", format!("{}?jms.redeliveryPolicy.maximumRedeliveries=2&jms.redeliveryPolicy.initialRedeliveryDelay=0&jms.redeliveryPolicy.redeliveryDelay=50", broker_url()));
    let counts = Arc::new(Mutex::new(Vec::new()));
    let c2 = counts.clone();
    let qq = q.clone();
    ctx.add_routes(move |b| {
        b.from(&format!("jms:queue:{}?acknowledgementModeName=CLIENT_ACKNOWLEDGE", qq)).process(move |ex: &mut Exchange| {
            c2.lock().unwrap().push((ex.header("JMSRedelivered").and_then(|v| v.as_bool()).unwrap_or(false), ex.header("JMSXDeliveryCount").and_then(|v| v.as_i64()).unwrap_or(1), Instant::now()));
            Err(CamelError::illegal_state("always fails"))
        });
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body(&format!("jms:queue:{}", q), "poison").unwrap();
    let dlq = format!("/queue/DLQ.{}", q);
    let start = Instant::now();
    let mut dead = Vec::new();
    while dead.is_empty() && start.elapsed() < Duration::from_secs(10) {
        dead = drain(&dlq, Duration::from_millis(300));
    }
    assert_eq!(dead, vec!["poison"]);
    let c = counts.lock().unwrap();
    assert_eq!(c.iter().map(|(r, n, _)| (*r, *n)).collect::<Vec<_>>(), vec![(false, 1), (true, 2), (true, 3)]);
    assert!(c[2].2 - c[1].2 >= Duration::from_millis(50), "redeliveryDelay");
    drop(c);
    ctx.stop().unwrap();
}

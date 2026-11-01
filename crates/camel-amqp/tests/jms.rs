//! `stomp`, `jms`, `activemq` and `amqp` components against the embedded STOMP broker.

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use camel_amqp::testing::StompBroker;
use camel_core::prelude::*;

fn context(broker: &StompBroker) -> CamelContext {
    let ctx = CamelContext::new();
    camel_amqp::register(&ctx);
    for scheme in ["jms", "activemq", "amqp-stomp", "stomp"] {
        ctx.properties().set(format!("camel.component.{}.brokerUrl", scheme), broker.url());
    }
    ctx
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < Duration::from_secs(5), "timed out waiting for {}", what);
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn s(v: Option<&Value>) -> String {
    v.map(|v| v.to_string()).unwrap_or_default()
}

#[test]
fn stomp_component_round_trip() {
    let broker = StompBroker::builder().user("guest", "pw").start().unwrap();
    let ctx = CamelContext::new();
    camel_amqp::register(&ctx);
    let url = format!("brokerURL=tcp://127.0.0.1:{}&login=guest&passcode=pw", broker.port());
    let (from, to) = (format!("stomp:queue:inbox?{}", url), format!("stomp:queue:inbox?{}", url));
    ctx.add_routes(move |b| {
        b.from(&from).to("mock:stomp");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:stomp").unwrap();
    mock.expected_bodies_received(vec!["hello stomp"]);
    mock.expected_header_received("destination", "/queue/inbox");
    mock.expected_header_received("custom", "42");
    ctx.create_producer_template().send_body_and_header(&to, "hello stomp", "custom", 42).unwrap();
    mock.assert_is_satisfied().unwrap();
    // bad credentials fail the producer
    let ex = ctx.create_producer_template().send(&format!("stomp:queue:inbox?brokerURL=tcp://127.0.0.1:{}&login=x&passcode=y", broker.port()), Exchange::with_body(Some(ctx.clone()), "x"));
    assert_eq!(ex.exception.unwrap().kind, "AuthenticationFailureException");
    ctx.stop().unwrap();
}

#[test]
fn queue_competing_consumers() {
    let broker = StompBroker::start().unwrap();
    let ctx = context(&broker);
    let threads = Arc::new(Mutex::new(HashSet::new()));
    let t2 = threads.clone();
    ctx.add_routes(move |b| {
        b.from("jms:queue:orders?concurrentConsumers=3")
            .process(move |_ex: &mut Exchange| {
                t2.lock().unwrap().insert(format!("{:?}", std::thread::current().id()));
                std::thread::sleep(Duration::from_millis(10));
                Ok(())
            })
            .to("mock:orders");
        b.from("activemq:queue:shared").to("mock:a");
        b.from("amqp-stomp:queue:shared").to("mock:b");
    })
    .unwrap();
    ctx.start().unwrap();
    assert_eq!(broker.subscription_count("orders"), 3);
    let mock = ctx.mock_endpoint("mock:orders").unwrap();
    let bodies: Vec<String> = (0..9).map(|i| format!("order-{}", i)).collect();
    mock.expected_bodies_received_in_any_order(bodies.clone());
    let t = ctx.create_producer_template();
    for b in &bodies {
        t.send_body("jms:queue:orders", b.as_str()).unwrap();
    }
    mock.assert_is_satisfied().unwrap();
    assert_eq!(threads.lock().unwrap().len(), 3, "all three consumers took part");
    let (a, b) = (ctx.mock_endpoint("mock:a").unwrap(), ctx.mock_endpoint("mock:b").unwrap());
    for i in 0..10 {
        t.send_body("jms:shared", format!("s{}", i)).unwrap();
    }
    wait_until("all shared messages consumed", || a.received_counter() + b.received_counter() == 10);
    assert!(a.received_counter() > 0 && b.received_counter() > 0, "both consumers compete");
    wait_until("acked", || broker.pending_count("orders") == 0 && broker.queue_depth("orders") == 0);
    ctx.stop().unwrap();
}

#[test]
fn topic_fan_out_and_selectors() {
    let broker = StompBroker::start().unwrap();
    let ctx = context(&broker);
    ctx.add_routes(|b| {
        b.from("jms:topic:prices").to("mock:all");
        b.from("jms:topic:prices?selector=symbol = 'ACME' AND price > 100").to("mock:acme");
        b.from("jms:queue:alerts?selector=severity IN ('HIGH', 'CRITICAL') OR JMSPriority >= 8").to("mock:alerts");
        b.from("jms:queue:alerts?selector=severity = 'LOW'").to("mock:low");
    })
    .unwrap();
    ctx.start().unwrap();
    let m = |n: &str| ctx.mock_endpoint(n).unwrap();
    let (all, acme, alerts, low) = (m("mock:all"), m("mock:acme"), m("mock:alerts"), m("mock:low"));
    all.expected_bodies_received(vec!["acme-150", "acme-50", "other-500"]);
    acme.expected_bodies_received(vec!["acme-150"]);
    alerts.expected_bodies_received_in_any_order(vec!["disk full", "urgent"]);
    low.expected_bodies_received(vec!["fyi"]);
    let t = ctx.create_producer_template();
    let quote = |sym: &str, price: i64, body: &str| t.send_body_and_headers("jms:topic:prices", body.to_string(), vec![("symbol", Value::from(sym)), ("price", Value::Int(price))]).unwrap();
    quote("ACME", 150, "acme-150");
    quote("ACME", 50, "acme-50");
    quote("OTHER", 500, "other-500");
    t.send_body_and_header("jms:queue:alerts", "disk full", "severity", "CRITICAL").unwrap();
    t.send_body_and_header("jms:queue:alerts", "fyi", "severity", "LOW").unwrap();
    t.send_body_and_header("jms:queue:alerts?priority=9", "urgent", "severity", "MEDIUM").unwrap();
    t.send_body_and_header("jms:queue:alerts", "unmatched", "severity", "MEDIUM").unwrap();
    MockEndpoint::assert_all_satisfied(&[&all, &acme, &alerts, &low]).unwrap();
    assert_eq!(broker.queue_depth("alerts"), 1, "a message no selector matches stays queued");
    assert_eq!(broker.queue_messages("alerts")[0].body_string(), "unmatched");
    // invalid selectors are rejected at endpoint creation
    let e = ctx.get_endpoint("jms:queue:x?selector=a = ").err().unwrap();
    assert!(e.message.contains("selector"), "{}", e.message);
    ctx.stop().unwrap();
}

#[test]
fn jms_headers_qos_and_key_format() {
    let broker = StompBroker::start().unwrap();
    let ctx = context(&broker);
    ctx.add_routes(|b| {
        b.from("jms:queue:headers").to("mock:headers");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:headers").unwrap();
    mock.expected_message_count(1);
    let t = ctx.create_producer_template();
    t.send_body_and_headers(
        "jms:queue:headers?priority=7&timeToLive=60000&deliveryPersistent=false",
        "payload",
        vec![
            ("JMSCorrelationID", Value::from("corr-9")),
            ("JMSType", Value::from("OrderCreated")),
            ("JMSReplyTo", Value::from("queue:answers")),
            ("my.dotted-key", Value::from("v")),
            ("count", Value::Int(3)),
            ("CamelInternal", Value::from("skip")),
        ],
    )
    .unwrap();
    mock.assert_is_satisfied().unwrap();
    let ex = mock.exchange(0).unwrap();
    assert!(s(ex.header("JMSMessageID")).starts_with("ID:"));
    assert_eq!(s(ex.header("JMSCorrelationID")), "corr-9");
    assert_eq!(s(ex.header("JMSType")), "OrderCreated");
    assert_eq!(s(ex.header("JMSDestination")), "queue://headers");
    assert_eq!(s(ex.header("JMSReplyTo")), "queue://answers");
    assert_eq!(ex.header("JMSPriority").and_then(|v| v.as_i64()), Some(7));
    assert_eq!(ex.header("JMSDeliveryMode").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(ex.header("JMSRedelivered").and_then(|v| v.as_bool()), Some(false));
    assert!(ex.header("JMSTimestamp").and_then(|v| v.as_i64()).unwrap() > 0);
    assert!(ex.header("JMSExpiration").and_then(|v| v.as_i64()).unwrap() > 0);
    assert_eq!(s(ex.header("my.dotted-key")), "v");
    assert_eq!(s(ex.header("count")), "3");
    assert!(ex.header("CamelInternal").is_none());
    assert_eq!(ex.pattern, ExchangePattern::InOut, "a JMSReplyTo makes the consumer exchange InOut (Camel semantics)");
    // ... and the consumer replied to it
    wait_until("reply sent to JMSReplyTo", || broker.queue_depth("answers") == 1);
    let reply = &broker.queue_messages("answers")[0];
    assert_eq!((reply.body_string().as_str(), reply.header("correlation-id")), ("payload", Some("corr-9")));
    ctx.stop().unwrap();
    // bytes bodies and priority ordering of queued messages
    let ctx = context(&broker);
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("jms:queue:prio?priority=1", "low").unwrap();
    t.send_body("jms:queue:prio?priority=9", Value::Bytes(vec![0, 1, 2])).unwrap();
    let msgs = broker.queue_messages("prio");
    assert_eq!(msgs[0].body, vec![0, 1, 2]);
    assert_eq!(msgs[1].body_string(), "low");
    ctx.stop().unwrap();
}

#[test]
fn in_out_request_reply_over_jms_queue() {
    let broker = StompBroker::start().unwrap();
    let ctx = context(&broker);
    ctx.add_routes(|b| {
        b.from("jms:queue:greeter?concurrentConsumers=2").transform(simple("Hello ${body}")).set_header("JMSType", constant("Greeting")).set_header("handled-by", constant("greeter"));
        b.from("activemq:queue:fails").process(|_ex: &mut Exchange| Err(CamelError::new("InsufficientFundsException", "balance too low")));
        b.from("direct:call").to_with_pattern(ExchangePattern::InOut, "jms:queue:greeter?replyTo=fixed.replies").to("mock:reply");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for i in 0..5 {
        assert_eq!(t.request_body("jms:queue:greeter", format!("Camel{}", i)).unwrap().to_string(), format!("Hello Camel{}", i));
    }
    let ex = t.request("jms:queue:greeter", |ex| {
        ex.set_body("World");
        ex.set_header("JMSCorrelationID", "my-corr");
    })
    .unwrap();
    assert_eq!(ex.body().to_string(), "Hello World");
    assert_eq!(s(ex.header("JMSCorrelationID")), "my-corr");
    assert_eq!(s(ex.header("JMSType")), "Greeting");
    assert_eq!(s(ex.header("handled-by")), "greeter");
    // fixed reply queue
    let mock = ctx.mock_endpoint("mock:reply").unwrap();
    mock.expected_bodies_received(vec!["Hello Route"]);
    t.send_body("direct:call", "Route").unwrap();
    mock.assert_is_satisfied().unwrap();
    // remote failure
    let err = t.request_body("activemq:queue:fails", "x").unwrap_err();
    let cause = err.cause.as_deref().unwrap_or(&err);
    assert_eq!(cause.kind, "InsufficientFundsException", "{:?}", err);
    // timeout
    let start = Instant::now();
    let err = t.request_body("jms:queue:nobody?requestTimeout=200", "x").unwrap_err();
    let cause = err.cause.as_deref().unwrap_or(&err);
    assert_eq!(cause.kind, "ExchangeTimedOutException");
    assert!(start.elapsed() < Duration::from_secs(3));
    // disableReplyTo: InOut becomes fire-and-forget
    assert_eq!(t.request_body("jms:queue:sink?disableReplyTo=true", "quiet").unwrap().to_string(), "quiet");
    assert_eq!(broker.queue_depth("sink"), 1);
    ctx.stop().unwrap();
}

#[test]
fn transacted_rollback_redelivers_with_jms_redelivered() {
    let broker = StompBroker::start().unwrap();
    let ctx = context(&broker);
    let attempts = Arc::new(AtomicUsize::new(0));
    let a2 = attempts.clone();
    let flags = Arc::new(Mutex::new(Vec::new()));
    let f2 = flags.clone();
    let client_attempts = Arc::new(AtomicUsize::new(0));
    let c2 = client_attempts.clone();
    ctx.add_routes(move |b| {
        b.from("jms:queue:payments?transacted=true")
            .process(move |ex: &mut Exchange| {
                f2.lock().unwrap().push(ex.header("JMSRedelivered").and_then(|v| v.as_bool()).unwrap_or(false));
                if a2.fetch_add(1, Ordering::SeqCst) < 2 {
                    return Err(CamelError::illegal_state("database deadlock"));
                }
                Ok(())
            })
            .to("jms:queue:audit?transacted=true")
            .to("mock:committed");
        b.from("jms:queue:client?acknowledgementModeName=CLIENT_ACKNOWLEDGE")
            .process(move |_ex: &mut Exchange| {
                if c2.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(CamelError::illegal_state("first time fails"));
                }
                Ok(())
            })
            .to("mock:client");
        b.from("jms:queue:auto").process(|_ex: &mut Exchange| Err(CamelError::illegal_state("always fails"))).to("mock:never");
    })
    .unwrap();
    ctx.start().unwrap();
    let committed = ctx.mock_endpoint("mock:committed").unwrap();
    committed.expected_bodies_received(vec!["pay 10"]);
    committed.expected_header_received("JMSRedelivered", true);
    let t = ctx.create_producer_template();
    t.send_body("jms:queue:payments", "pay 10").unwrap();
    committed.assert_is_satisfied().unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    assert_eq!(*flags.lock().unwrap(), vec![false, true, true]);
    wait_until("transaction settled", || broker.pending_count("payments") == 0 && broker.queue_depth("payments") == 0);
    assert_eq!(broker.queue_depth("audit"), 1);
    // CLIENT_ACKNOWLEDGE: failure => NACK => redelivery
    let client = ctx.mock_endpoint("mock:client").unwrap();
    client.expected_header_received("JMSRedelivered", true);
    client.expected_message_count(1);
    t.send_body("jms:queue:client", "c").unwrap();
    client.assert_is_satisfied().unwrap();
    // AUTO_ACKNOWLEDGE: failures are acknowledged (no redelivery)
    t.send_body("jms:queue:auto", "a").unwrap();
    wait_until("auto message acknowledged", || broker.pending_count("auto") == 0 && broker.queue_depth("auto") == 0);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(ctx.mock_endpoint("mock:never").unwrap().received_counter(), 0);
    assert_eq!(broker.queue_depth("auto"), 0);
    // a transacted producer that never commits: nothing is visible
    ctx.stop().unwrap();
}

#[test]
fn consumer_reconnects_and_options_are_validated() {
    let broker = StompBroker::start().unwrap();
    let ctx = context(&broker);
    ctx.add_routes(|b| {
        b.from("jms:queue:resilient?recoveryInterval=50").to("mock:r");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:r").unwrap();
    mock.expected_bodies_received(vec!["before", "after"]);
    let t = ctx.create_producer_template();
    t.send_body("jms:queue:resilient", "before").unwrap();
    mock.wait_for_messages(1, Duration::from_secs(5));
    wait_until("acked", || broker.pending_count("resilient") == 0);
    broker.drop_connections();
    std::thread::sleep(Duration::from_millis(100));
    wait_until("resubscribed", || broker.subscription_count("resilient") == 1);
    if t.send_body("jms:queue:resilient", "after").is_err() {
        t.send_body("jms:queue:resilient", "after").unwrap();
    }
    mock.assert_is_satisfied().unwrap();
    for bad in ["jms:queue:x?bogus=1", "jms:queue:x?priority=12", "jms:queue:x?acknowledgementModeName=NOPE", "jms:queue:", "jms:queue:x?jmsMessageType=Stream", "jms:topic:t?durableSubscriptionName=d", "jms:queue:x?replyToType=Exclusive"] {
        assert!(ctx.get_endpoint(bad).is_err(), "{}", bad);
    }
    ctx.stop().unwrap();
}

/// Regressions found against a real RabbitMQ (live interop suite), reproduced with the
/// embedded broker's RabbitMQ flavour.
#[test]
fn rabbitmq_flavoured_broker_conventions() {
    let broker = StompBroker::builder().rabbitmq().user("camel", "pw").start().unwrap();
    let ctx = CamelContext::new();
    camel_amqp::register(&ctx);
    for (k, v) in [("brokerUrl", broker.url()), ("username", "camel".into()), ("password", "pw".into()), ("host", "/".into())] {
        ctx.properties().set(format!("camel.component.jms.{}", k), v);
    }
    let attempts = Arc::new(AtomicUsize::new(0));
    let a2 = attempts.clone();
    ctx.add_routes(move |b| {
        b.from("jms:queue:greeter").transform(simple("Hello ${body}"));
        b.from("jms:queue:np").to("mock:np");
        b.from("jms:queue:payments?transacted=true")
            .process(move |_ex: &mut Exchange| if a2.fetch_add(1, Ordering::SeqCst) == 0 { Err(CamelError::illegal_state("deadlock")) } else { Ok(()) })
            .to("mock:committed");
        b.from("jms:topic:news?durableSubscriptionName=reader&clientId=app").route_id("durable").to("mock:durable");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    // a "Bad CONNECT" error whose body says "Access refused" is an authentication failure
    let ex = t.send(&format!("jms:queue:x?brokerURL={}&username=camel&password=wrong&host=/", broker.url()), Exchange::with_body(Some(ctx.clone()), "x"));
    assert_eq!(ex.exception.unwrap().kind, "AuthenticationFailureException");
    // request/reply over RabbitMQ temporary queues (reply-to only, no SUBSCRIBE)
    for i in 0..3 {
        assert_eq!(t.request_body("jms:queue:greeter", format!("R{}", i)).unwrap().to_string(), format!("Hello R{}", i));
    }
    // NON_PERSISTENT sends omit `persistent` (RabbitMQ: `persistent:false` = non-durable queue)
    let np = ctx.mock_endpoint("mock:np").unwrap();
    np.expected_header_received("JMSDeliveryMode", 1i64);
    np.expected_message_count(1);
    t.send_body("jms:queue:np?deliveryPersistent=false", "transient").unwrap();
    np.assert_is_satisfied().unwrap();
    // a failed transacted exchange is redelivered (ABORT + NACK)
    let committed = ctx.mock_endpoint("mock:committed").unwrap();
    committed.expected_header_received("JMSRedelivered", true);
    committed.expected_message_count(1);
    t.send_body("jms:queue:payments", "pay").unwrap();
    committed.assert_is_satisfied().unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    // durable topic subscription (durable:true + stable id)
    let durable = ctx.mock_endpoint("mock:durable").unwrap();
    ctx.stop_route("durable").unwrap();
    t.send_body("jms:topic:news", "while-offline").unwrap();
    durable.expected_bodies_received(vec!["while-offline"]);
    ctx.start_route("durable").unwrap();
    durable.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
    // without host=/ the RabbitMQ vhost check refuses the connection
    let ctx2 = context(&broker);
    ctx2.start().unwrap();
    let ex = ctx2.create_producer_template().send(&format!("jms:queue:x?username=camel&password=pw&brokerURL={}", broker.url()), Exchange::with_body(Some(ctx2.clone()), "x"));
    assert!(ex.exception.unwrap().message.contains("Virtual host"));
}

/// Regressions found against a real Apache ActiveMQ 5.17 (live interop suite), reproduced with
/// the embedded broker's ActiveMQ flavour: ActiveMQ does not redeliver after an aborted
/// transactional ACK and dead-letters NACKed messages, so the consumer redelivers locally
/// (ActiveMQ JMS client redelivery policy), and its login error is an authentication failure.
#[test]
fn activemq_flavoured_broker_conventions() {
    let broker = StompBroker::builder().activemq().user("camel", "pw").start().unwrap();
    let ctx = CamelContext::new();
    camel_amqp::register(&ctx);
    let url = format!("{}?jms.redeliveryPolicy.initialRedeliveryDelay=0&jms.redeliveryPolicy.redeliveryDelay=0", broker.url());
    for (k, v) in [("brokerUrl", url), ("username", "camel".into()), ("password", "pw".into())] {
        ctx.properties().set(format!("camel.component.jms.{}", k), v);
    }
    let attempts = Arc::new(AtomicUsize::new(0));
    let a2 = attempts.clone();
    let flags = Arc::new(Mutex::new(Vec::new()));
    let f2 = flags.clone();
    let client_attempts = Arc::new(AtomicUsize::new(0));
    let c2 = client_attempts.clone();
    let doomed = Arc::new(AtomicUsize::new(0));
    let d2 = doomed.clone();
    ctx.add_routes(move |b| {
        b.from("jms:queue:payments?transacted=true")
            .process(move |ex: &mut Exchange| {
                f2.lock().unwrap().push((ex.header("JMSRedelivered").and_then(|v| v.as_bool()).unwrap_or(false), ex.header("JMSXDeliveryCount").and_then(|v| v.as_i64())));
                // the send joins the consumer's transaction: rolled back with each failure
                Ok(())
            })
            .to("jms:queue:audit")
            .process(move |_ex: &mut Exchange| if a2.fetch_add(1, Ordering::SeqCst) < 2 { Err(CamelError::illegal_state("deadlock")) } else { Ok(()) })
            .to("mock:committed");
        b.from("jms:queue:client?acknowledgementModeName=CLIENT_ACKNOWLEDGE")
            .process(move |_ex: &mut Exchange| if c2.fetch_add(1, Ordering::SeqCst) == 0 { Err(CamelError::illegal_state("first time fails")) } else { Ok(()) })
            .to("mock:client");
        b.from("jms:queue:doomed?transacted=true").process(move |_ex: &mut Exchange| {
            d2.fetch_add(1, Ordering::SeqCst);
            Err(CamelError::illegal_state("always fails"))
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    // ActiveMQ's "User name [x] or password is invalid." is an authentication failure
    let ex = t.send(&format!("jms:queue:x?brokerURL={}&username=camel&password=wrong", broker.url()), Exchange::with_body(Some(ctx.clone()), "x"));
    let e = ex.exception.unwrap();
    assert_eq!(e.kind, "AuthenticationFailureException");
    assert_eq!(e.message, "User name [camel] or password is invalid.");
    // a failed transacted exchange is redelivered by the consumer (the broker would not)
    let committed = ctx.mock_endpoint("mock:committed").unwrap();
    committed.expected_header_received("JMSRedelivered", true);
    committed.expected_message_count(1);
    t.send_body("jms:queue:payments", "pay").unwrap();
    committed.assert_is_satisfied().unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    assert_eq!(*flags.lock().unwrap(), vec![(false, None), (true, Some(2)), (true, Some(3))]);
    wait_until("transaction settled", || broker.pending_count("payments") == 0 && broker.queue_depth("payments") == 0);
    assert_eq!(broker.queue_depth("audit"), 1, "only the committed attempt's send");
    // CLIENT_ACKNOWLEDGE: redelivered by the consumer, not NACKed (a poison ack on ActiveMQ)
    let client = ctx.mock_endpoint("mock:client").unwrap();
    client.expected_header_received("JMSRedelivered", true);
    client.expected_message_count(1);
    t.send_body("jms:queue:client", "c").unwrap();
    client.assert_is_satisfied().unwrap();
    assert_eq!(broker.queue_depth("ActiveMQ.DLQ"), 0);
    // exhausted after 6 redeliveries: NACKed, so ActiveMQ dead-letters it
    t.send_body("jms:queue:doomed", "poison").unwrap();
    wait_until("dead-lettered", || broker.queue_depth("ActiveMQ.DLQ") == 1);
    assert_eq!(doomed.load(Ordering::SeqCst), 7);
    assert_eq!(broker.queue_messages("ActiveMQ.DLQ")[0].header("originalDestination"), Some("/queue/doomed"));
    ctx.stop().unwrap();
}

#[test]
fn redelivery_policy_from_broker_url() {
    use camel_amqp::stomp::component::RedeliveryPolicy;
    let d = RedeliveryPolicy::from_broker_url("tcp://h:61613").unwrap();
    assert_eq!((d.maximum_redeliveries, d.initial_redelivery_delay, d.redelivery_delay), (6, Duration::from_millis(1000), Duration::from_millis(1000)));
    let p = RedeliveryPolicy::from_broker_url("failover:(tcp://a:1,tcp://b:2)?randomize=false&jms.redeliveryPolicy.maximumRedeliveries=-1&jms.redeliveryPolicy.initialRedeliveryDelay=0&jms.redeliveryPolicy.redeliveryDelay=250").unwrap();
    assert_eq!((p.maximum_redeliveries, p.delay(1), p.delay(2)), (-1, Duration::ZERO, Duration::from_millis(250)));
    assert!(p.allows(1_000));
    assert!(d.allows(6) && !d.allows(7));
    assert!(RedeliveryPolicy::from_broker_url("tcp://h:1?jms.redeliveryPolicy.maximumRedeliveries=x").is_err());
}

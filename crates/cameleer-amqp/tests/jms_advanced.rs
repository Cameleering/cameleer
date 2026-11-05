//! JMS over STOMP: durable and shared subscriptions, MapMessage / ObjectMessage,
//! `replyToType`, `useMessageIDAsCorrelationID` and broker persistence across restarts.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cameleer_amqp::testing::StompBroker;
use cameleer_core::prelude::*;

fn context(broker: &StompBroker) -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_amqp::register(&ctx);
    for scheme in ["jms", "activemq", "amqp-stomp"] {
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

#[test]
fn durable_subscription_survives_consumer_downtime() {
    let broker = StompBroker::start().unwrap();
    let ctx = context(&broker);
    ctx.add_routes(|b| {
        b.from("jms:topic:prices?clientId=app1&durableSubscriptionName=sub1").route_id("durable").to("mock:prices");
    })
    .unwrap();
    ctx.start().unwrap();
    wait_until("subscribed", || broker.subscription_count("topic:prices") == 1);
    let mock = ctx.mock_endpoint("mock:prices").unwrap();
    mock.expected_bodies_received(vec!["p1"]);
    let t = ctx.create_producer_template();
    t.send_body("jms:topic:prices", "p1").unwrap();
    mock.assert_is_satisfied().unwrap();
    ctx.stop_route("durable").unwrap();
    wait_until("detached", || broker.subscription_count("topic:prices") == 0);
    t.send_body("jms:topic:prices", "p2").unwrap();
    t.send_body("jms:topic:prices", "p3").unwrap();
    assert_eq!(broker.topic_subscriptions("topic:prices"), vec![("#durable:app1:sub1".to_string(), 2)]);
    mock.reset();
    mock.expected_bodies_received(vec!["p2", "p3"]);
    ctx.start_route("durable").unwrap();
    mock.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
}

#[test]
fn shared_subscription_members_compete() {
    let broker = StompBroker::start().unwrap();
    let ctx = context(&broker);
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    ctx.add_routes(move |b| {
        b.from("jms:topic:feed?subscriptionShared=true&subscriptionName=workers&concurrentConsumers=3").to("mock:shared");
        b.from("jms:topic:feed").process(move |_: &mut Exchange| {
            c.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    wait_until("subscribed", || broker.subscription_count("topic:feed") == 4);
    let mock = ctx.mock_endpoint("mock:shared").unwrap();
    mock.expected_message_count(9);
    let t = ctx.create_producer_template();
    for i in 0..9 {
        t.send_body("jms:topic:feed", format!("f{}", i)).unwrap();
    }
    mock.assert_is_satisfied().unwrap();
    wait_until("plain subscriber", || count.load(Ordering::SeqCst) == 9);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(mock.received_counter(), 9, "each message once within the shared subscription");
    let threads: std::collections::HashSet<String> = mock.received_exchanges().iter().map(|e| e.body().to_string()).collect();
    assert_eq!(threads.len(), 9);
    ctx.stop().unwrap();
    // a non-durable shared subscription ends with its last member
    wait_until("removed", || broker.topic_subscriptions("topic:feed").is_empty());
}

#[test]
fn map_and_object_messages() {
    let broker = StompBroker::start().unwrap();
    let ctx = context(&broker);
    ctx.add_routes(|b| {
        b.from("jms:queue:maps").to("mock:maps");
        b.from("jms:queue:objects").to("mock:objects");
    })
    .unwrap();
    ctx.start().unwrap();
    let mut m = cameleer_core::value::ObjectMap::new();
    m.insert("id".into(), Value::Int(7));
    m.insert("name".into(), Value::from("widget"));
    m.insert("tags".into(), Value::Array(vec![Value::from("a"), Value::Bool(true)]));
    let maps = ctx.mock_endpoint("mock:maps").unwrap();
    maps.expected_bodies_received(vec![Value::Object(m.clone())]);
    let objects = ctx.mock_endpoint("mock:objects").unwrap();
    objects.expected_bodies_received(vec![Value::Array(vec![Value::Int(1), Value::Float(2.5), Value::from("x")])]);
    let t = ctx.create_producer_template();
    t.send_body("jms:queue:maps", Value::Object(m.clone())).unwrap();
    t.send_body("jms:queue:objects?jmsMessageType=Object", Value::Array(vec![Value::Int(1), Value::Float(2.5), Value::from("x")])).unwrap();
    maps.assert_is_satisfied().unwrap();
    objects.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
    // the wire format carries ActiveMQ's transformation header
    t.send_body("jms:queue:wire?jmsMessageType=Map", Value::Object(m)).unwrap();
    let wire = broker.queue_messages("wire");
    assert_eq!(wire[0].header("transformation"), Some("jms-map-json"));
}

#[test]
fn reply_to_types_and_message_id_correlation() {
    let broker = StompBroker::start().unwrap();
    let ctx = context(&broker);
    ctx.add_routes(|b| {
        b.from("jms:queue:svc?concurrentConsumers=2").transform(simple("re:${body}"));
        b.from("jms:queue:svc-mid?useMessageIDAsCorrelationID=true").transform(simple("mid:${body}"));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    // shared reply queue (default with replyTo): concurrent requesters only see their replies
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let t = ctx.create_producer_template();
            std::thread::spawn(move || t.request_body("jms:queue:svc?replyTo=shared.replies&replyToType=Shared", format!("r{}", i)).unwrap().to_string())
        })
        .collect();
    let mut got: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    got.sort();
    assert_eq!(got, vec!["re:r0", "re:r1", "re:r2", "re:r3"]);
    assert_eq!(broker.queue_depth("shared.replies"), 0);
    // exclusive reply queue
    assert_eq!(t.request_body("jms:queue:svc?replyTo=excl.replies&replyToType=Exclusive", "x").unwrap().to_string(), "re:x");
    // the request's JMSMessageID is the reply's JMSCorrelationID
    let ex = t
        .request("jms:queue:svc-mid?useMessageIDAsCorrelationID=true", |ex| {
            ex.set_body("y");
            ex.set_header("JMSCorrelationID", "ignored");
        })
        .unwrap();
    assert_eq!(ex.body().to_string(), "mid:y");
    assert!(ex.header("JMSCorrelationID").unwrap().to_string().starts_with("ID:"), "{:?}", ex.header("JMSCorrelationID"));
    ctx.stop().unwrap();
}

#[test]
fn persistent_messages_survive_broker_restarts() {
    let dir = std::env::temp_dir().join(format!("camel-stomp-journal-{}-{}", std::process::id(), cameleer_amqp::common::unique_id()));
    {
        let broker = StompBroker::builder().persistence(&dir).start().unwrap();
        let ctx = context(&broker);
        let t = ctx.create_producer_template();
        t.send_body("jms:queue:orders", "o1").unwrap();
        t.send_body("jms:queue:orders", "o2").unwrap();
        t.send_body("jms:queue:orders?deliveryPersistent=false", "transient").unwrap();
        // a durable subscription and a message waiting in it
        ctx.add_routes(|b| {
            b.from("jms:topic:news?clientId=c1&durableSubscriptionName=n").route_id("news").to("mock:news");
        })
        .unwrap();
        ctx.start().unwrap();
        wait_until("subscribed", || broker.subscription_count("topic:news") == 1);
        ctx.stop_route("news").unwrap();
        t.send_body("jms:topic:news", "n1").unwrap();
        broker.send("/queue/other", &[("persistent", "true")], b"x");
        ctx.stop().unwrap();
        assert_eq!(broker.queue_depth("orders"), 3);
    }
    let broker = StompBroker::builder().persistence(&dir).start().unwrap();
    assert_eq!(broker.queue_depth("orders"), 2, "non-persistent messages are not recovered");
    assert_eq!(broker.topic_subscriptions("topic:news"), vec![("#durable:c1:n".to_string(), 1)]);
    let ctx = context(&broker);
    let attempts = Arc::new(AtomicUsize::new(0));
    let a2 = attempts.clone();
    ctx.add_routes(move |b| {
        b.from("jms:queue:orders?acknowledgementModeName=CLIENT_ACKNOWLEDGE")
            .process(move |_: &mut Exchange| {
                if a2.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(CamelError::new("IOException", "nack"));
                }
                Ok(())
            })
            .to("mock:orders");
        b.from("jms:topic:news?clientId=c1&durableSubscriptionName=n").to("mock:news");
    })
    .unwrap();
    ctx.start().unwrap();
    let orders = ctx.mock_endpoint("mock:orders").unwrap();
    orders.expected_bodies_received(vec!["o1", "o2"]);
    let news = ctx.mock_endpoint("mock:news").unwrap();
    news.expected_bodies_received(vec!["n1"]);
    orders.assert_is_satisfied().unwrap();
    news.assert_is_satisfied().unwrap();
    assert_eq!(orders.exchange(0).unwrap().header("JMSRedelivered"), Some(&Value::Bool(true)));
    ctx.stop().unwrap();
    drop(broker);
    // a third start: everything was acknowledged, only /queue/other remains
    let broker = StompBroker::builder().persistence(&dir).start().unwrap();
    assert_eq!(broker.queue_depth("orders"), 0);
    assert_eq!(broker.queue_depth("other"), 1);
    // unacknowledged deliveries before a restart come back redelivered
    let ctx = context(&broker);
    let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let r2 = release.clone();
    ctx.add_routes(move |b| {
        b.from("jms:queue:other?acknowledgementModeName=CLIENT_ACKNOWLEDGE&recoveryInterval=50")
            .process(move |_: &mut Exchange| {
                let start = Instant::now();
                while !r2.load(Ordering::SeqCst) && start.elapsed() < Duration::from_secs(10) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Ok(())
            })
            .to("mock:never");
    })
    .unwrap();
    ctx.start().unwrap();
    wait_until("delivered", || broker.pending_count("other") == 1);
    drop(broker);
    let broker = StompBroker::builder().persistence(&dir).start().unwrap();
    let msgs = broker.queue_messages("other");
    assert_eq!(msgs.len(), 1);
    assert!(msgs[0].redelivered);
    drop(broker);
    release.store(true, Ordering::SeqCst);
    ctx.stop().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

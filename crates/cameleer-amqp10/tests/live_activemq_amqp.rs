//! Live interop: the `amqp` component (JMS over AMQP 1.0) against the AMQP transport connector
//! of the Apache ActiveMQ 5.17 broker started by `scripts/live-interop.sh`. Runs only with
//! `CAMEL_LIVE_INTEROP=1`, and only when the broker has its AMQP connector (port 15675):
//! Ubuntu's `activemq` package does not ship the `activemq-amqp` module; `scripts/live-interop.sh
//! install` fetches `activemq-amqp-5.17.6.jar` from Maven Central (SHA-1 pinned) into
//! `$CAMEL_INTEROP_DIR/activemq/lib` (proton-j comes from `libapache-qpid-proton-j-java`).
//! Without it every test prints a skip notice and passes.
//!
//! Covers what RabbitMQ's AMQP 1.0 plugin cannot: JMS selectors
//! (`apache.org:selector-filter:string`), AMQP 1.0 transactions (coordinator), dynamic
//! temporary reply queues for InOut and durable subscriptions, plus AMQPS and SASL PLAIN; and
//! ActiveMQ specifics: `queue://` / `topic://` address prefixes, the JMS mapping through
//! ActiveMQ's JMS transformer, multi-frame messages, redelivery counts and the refusal of
//! JMS 2.0 shared subscriptions. `CAMEL_AMQP10_TRACE=1` (with `--nocapture`) prints every frame.

use std::net::TcpStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use cameleer_core::prelude::*;
use cameleer_tls::testing::live;

const PORT: u16 = 15675;
const TLS_PORT: u16 = 15676;
const WAIT: Duration = Duration::from_secs(10);

fn enabled() -> bool {
    if !live::enabled() {
        return false;
    }
    if TcpStream::connect(("127.0.0.1", PORT)).is_err() {
        eprintln!("skipped: ActiveMQ has no AMQP connector on port {} (the activemq-amqp module is not installed)", PORT);
        return false;
    }
    true
}

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_amqp10::register(&ctx);
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

#[test]
fn live_activemq_amqp_queues_topics_and_sasl() {
    if !enabled() {
        return;
    }
    let (q, topic) = (n("orders"), n("news"));
    let ctx = context();
    let (q2, t1, t2) = (q.clone(), topic.clone(), topic.clone());
    ctx.add_routes(move |r| {
        r.from(&format!("amqp:queue:{}", q2)).to("mock:orders");
        r.from(&format!("amqp:topic:{}", t1)).to("mock:sub1");
        r.from(&format!("amqp:topic:{}", t2)).to("mock:sub2");
    })
    .unwrap();
    ctx.start().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let mock = ctx.mock_endpoint("mock:orders").unwrap();
    mock.expected_bodies_received(vec!["hello activemq"]);
    let t = ctx.create_producer_template();
    t.send_body_and_headers(&format!("amqp:queue:{}?priority=7", q), "hello activemq", vec![("my.key", Value::from("v1")), ("JMSCorrelationID", Value::from("c-1"))]).unwrap();
    mock.assert_is_satisfied_within(WAIT).unwrap();
    let ex = mock.exchange(0).unwrap();
    assert_eq!(ex.header("my.key").map(|v| v.to_string()), Some("v1".into()));
    assert_eq!(ex.header("JMSCorrelationID").map(|v| v.to_string()), Some("c-1".into()));
    assert_eq!(ex.header("JMSPriority").and_then(|v| v.as_i64()), Some(7));
    let (s1, s2) = (ctx.mock_endpoint("mock:sub1").unwrap(), ctx.mock_endpoint("mock:sub2").unwrap());
    s1.expected_bodies_received(vec!["breaking"]);
    s2.expected_bodies_received(vec!["breaking"]);
    t.send_body(&format!("amqp:topic:{}", topic), "breaking").unwrap();
    MockEndpoint::assert_all_satisfied(&[&s1, &s2]).unwrap();
    // SASL PLAIN with a wrong password
    let err = t.send_body(&format!("amqp:queue:{}?username=camel&password=wrong", q), "x").unwrap_err();
    assert!(err.chain().any(|c| c.kind == "AuthenticationFailureException"), "{:?}", err);
    ctx.stop().unwrap();
}

#[test]
fn live_activemq_amqp_selectors() {
    if !enabled() {
        return;
    }
    let q = n("events");
    let ctx = context();
    let q2 = q.clone();
    ctx.add_routes(move |r| {
        r.from(&format!("amqp:queue:{}?selector=region = 'EU' AND amount > 10", q2)).to("mock:eu");
    })
    .unwrap();
    ctx.start().unwrap();
    let eu = ctx.mock_endpoint("mock:eu").unwrap();
    eu.expected_bodies_received(vec!["e2"]);
    let t = ctx.create_producer_template();
    for (body, region, amount) in [("e1", "EU", 5), ("e2", "EU", 50), ("e3", "US", 50)] {
        t.send_body_and_headers(&format!("amqp:queue:{}", q), body, vec![("region", Value::from(region)), ("amount", Value::from(amount))]).unwrap();
    }
    eu.assert_is_satisfied_within(WAIT).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(eu.received_counter(), 1);
    ctx.stop().unwrap();
}

#[test]
fn live_activemq_amqp_request_reply_with_dynamic_reply_queues() {
    if !enabled() {
        return;
    }
    let (upper, fails) = (n("upper"), n("fails"));
    let ctx = context();
    let (u, f) = (upper.clone(), fails.clone());
    ctx.add_routes(move |r| {
        r.from(&format!("amqp:queue:{}", u)).transform(simple("${body.toUpperCase()}"));
        r.from(&format!("amqp:queue:{}", f)).throw_exception("IllegalStateException", "boom");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for i in 0..3 {
        assert_eq!(t.request_body(&format!("amqp:queue:{}", upper), format!("hello{}", i)).unwrap(), Value::from(format!("HELLO{}", i)));
    }
    let err = t.request_body(&format!("amqp:queue:{}", fails), "x").unwrap_err();
    assert!(err.chain().any(|c| c.kind == "IllegalStateException" && c.message.contains("boom")), "{:?}", err);
    let err = t.request_body(&format!("amqp:queue:{}?requestTimeout=300", n("nobody")), "z").unwrap_err();
    assert!(err.chain().any(|c| c.kind == "ExchangeTimedOutException"), "{:?}", err);
    ctx.stop().unwrap();
}

#[test]
fn live_activemq_amqp_transactions() {
    if !enabled() {
        return;
    }
    let q = n("tx-in");
    let ctx = context();
    let attempts = Arc::new(AtomicUsize::new(0));
    let a2 = attempts.clone();
    let q2 = q.clone();
    ctx.add_routes(move |r| {
        r.from(&format!("amqp:queue:{}?transacted=true", q2))
            .process(move |ex: &mut Exchange| {
                if a2.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(CamelError::new("IOException", "roll back"));
                }
                assert_eq!(ex.header("JMSRedelivered"), Some(&Value::Bool(true)));
                Ok(())
            })
            .to("mock:tx");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:tx").unwrap();
    mock.expected_bodies_received(vec!["atomic"]);
    ctx.create_producer_template().send_body(&format!("amqp:queue:{}?transacted=true", q), "atomic").unwrap();
    mock.assert_is_satisfied_within(WAIT).unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    ctx.stop().unwrap();
}

#[test]
fn live_activemq_amqp_durable_subscription() {
    if !enabled() {
        return;
    }
    let topic = n("prices");
    let ctx = context();
    let tp = topic.clone();
    ctx.add_routes(move |r| {
        r.from(&format!("amqp:topic:{}?clientId=live-amqp-{}&durableSubscriptionName=sub1", tp, std::process::id())).route_id("durable").to("mock:prices");
    })
    .unwrap();
    ctx.start().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let mock = ctx.mock_endpoint("mock:prices").unwrap();
    mock.expected_bodies_received(vec!["p1"]);
    let t = ctx.create_producer_template();
    t.send_body(&format!("amqp:topic:{}", topic), "p1").unwrap();
    mock.assert_is_satisfied_within(WAIT).unwrap();
    ctx.stop_route("durable").unwrap();
    t.send_body(&format!("amqp:topic:{}", topic), "p2").unwrap();
    mock.reset();
    mock.expected_bodies_received(vec!["p2"]);
    ctx.start_route("durable").unwrap();
    mock.assert_is_satisfied_within(WAIT).unwrap();
    ctx.stop().unwrap();
}

#[test]
fn live_activemq_amqps() {
    if !enabled() {
        return;
    }
    let q = n("secure");
    let ctx = context();
    let uri = format!("amqp:queue:{}?remoteAddress=amqps://localhost:{}&sslContextParameters=#liveSsl", q, TLS_PORT);
    let from = uri.clone();
    ctx.add_routes(move |r| {
        r.from(&from).to("mock:secure");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:secure").unwrap();
    mock.expected_bodies_received(vec!["tls+sasl"]);
    ctx.create_producer_template().send_body(&uri, "tls+sasl").unwrap();
    mock.assert_is_satisfied_within(WAIT).unwrap();
    ctx.stop().unwrap();
}

/// The AMQP JMS mapping through ActiveMQ's JMS transformer (`transport.transformer=jms`: every
/// message is converted to an ActiveMQ JMS message and back), with message annotations.
#[test]
fn live_activemq_amqp_jms_mapping_round_trip() {
    if !enabled() {
        return;
    }
    let (q, topic) = (n("mapping"), n("mapping-topic"));
    let ctx = context();
    let (q2, t2) = (q.clone(), topic.clone());
    ctx.add_routes(move |r| {
        r.from(&format!("amqp:queue:{}?includeAmqpAnnotations=true", q2)).to("mock:mapping");
        r.from(&format!("amqp:topic:{}", t2)).to("mock:topic");
    })
    .unwrap();
    ctx.start().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let mock = ctx.mock_endpoint("mock:mapping").unwrap();
    mock.expected_message_count(5);
    let t = ctx.create_producer_template();
    let before = wall_clock_millis();
    t.send_body_and_headers(
        &format!("amqp:queue:{}?priority=6&timeToLive=60000", q),
        "text body ü",
        vec![
            ("JMSType", Value::from("order")),
            ("JMSCorrelationID", Value::from("c-9")),
            ("JMSXGroupID", Value::from("group-1")),
            ("JMSReplyTo", Value::from(format!("topic:{}", topic))),
            ("my.int", Value::from(42)),
            ("flag", Value::Bool(true)),
            ("ratio", Value::Float(1.5)),
            ("dashed-key", Value::from("d")),
        ],
    )
    .unwrap();
    t.send_body(&format!("amqp:queue:{}", q), Value::Bytes(vec![0, 1, 2, 255])).unwrap();
    t.send_body(&format!("amqp:queue:{}", q), Value::object([("id", Value::Int(1)), ("name", Value::from("x"))])).unwrap();
    t.send_body(&format!("amqp:queue:{}?deliveryPersistent=false", q), "non-persistent").unwrap();
    t.send_body(&format!("amqp:queue:{}?jmsMessageType=Object", q), Value::object([("n", Value::Int(7))])).unwrap();
    mock.assert_is_satisfied_within(WAIT).unwrap();
    let ex = mock.exchange(0).unwrap();
    let s = |h: &str| ex.header(h).map(|v| v.to_string()).unwrap_or_default();
    assert_eq!(ex.body(), &Value::from("text body ü"));
    assert_eq!(s("JMSType"), "order");
    assert_eq!(s("JMSCorrelationID"), "c-9");
    assert_eq!(s("JMSXGroupID"), "group-1");
    assert_eq!(s("JMSDestination"), q, "queue:// prefix stripped");
    assert_eq!(s("JMSReplyTo"), format!("topic:{}", topic), "topic reply-to keeps its type");
    assert_eq!(ex.header("JMSPriority").and_then(|v| v.as_i64()), Some(6));
    assert_eq!(ex.header("JMSDeliveryMode").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(ex.header("JMSRedelivered").and_then(|v| v.as_bool()), Some(false));
    let ts = ex.header("JMSTimestamp").and_then(|v| v.as_i64()).unwrap();
    assert!(ts >= before - 5_000 && ts <= wall_clock_millis() + 5_000, "JMSTimestamp {}", ts);
    let exp = ex.header("JMSExpiration").and_then(|v| v.as_i64()).unwrap();
    assert!((exp - ts - 60_000).abs() < 5_000, "JMSExpiration - JMSTimestamp ~ timeToLive: {} {}", exp, ts);
    assert!(s("JMSMessageID").starts_with("ID:"), "{}", s("JMSMessageID"));
    assert_eq!(ex.header("my.int").and_then(|v| v.as_i64()), Some(42));
    assert_eq!(ex.header("flag").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(ex.header("ratio"), Some(&Value::Float(1.5)));
    assert_eq!(s("dashed-key"), "d");
    // annotations set by ActiveMQ's outbound transformer
    assert_eq!(ex.header("JMS_AMQP_MA_x-opt-jms-msg-type").and_then(|v| v.as_i64()), Some(5));
    assert_eq!(ex.header("JMS_AMQP_MA_x-opt-jms-dest").and_then(|v| v.as_i64()), Some(0));
    assert_eq!(ex.header("JMS_AMQP_MA_x-opt-jms-reply-to").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(mock.exchange(1).unwrap().body(), &Value::Bytes(vec![0, 1, 2, 255]));
    assert_eq!(mock.exchange(1).unwrap().header("JMS_AMQP_MA_x-opt-jms-msg-type").and_then(|v| v.as_i64()), Some(3));
    let map = mock.exchange(2).unwrap();
    assert_eq!(map.body().get("name"), Some(&Value::from("x")));
    assert_eq!(map.body().get("id").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(map.header("JMS_AMQP_MA_x-opt-jms-msg-type").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(mock.exchange(3).unwrap().header("JMSDeliveryMode").and_then(|v| v.as_i64()), Some(1));
    let obj = mock.exchange(4).unwrap();
    assert_eq!(obj.body().get("n").and_then(|v| v.as_i64()), Some(7), "{:?}", obj.body());
    // the consumer replied to the topic named by JMSReplyTo (the exchange was InOut), and the
    // stripped header names the topic again as a destination
    let sub = ctx.mock_endpoint("mock:topic").unwrap();
    sub.expected_bodies_received_in_any_order(vec!["text body ü", "to the topic"]);
    t.send_body_and_headers("amqp:queue:unused", "to the topic", vec![("CamelJmsDestinationName", Value::from(s("JMSReplyTo")))]).unwrap();
    sub.assert_is_satisfied_within(WAIT).unwrap();
    for i in 0..2 {
        let e = sub.exchange(i).unwrap();
        assert_eq!(e.header("JMSDestination").map(|v| v.to_string()), Some(format!("topic:{}", topic)));
        if e.body() == &Value::from("text body ü") {
            assert_eq!(e.header("JMSCorrelationID").map(|v| v.to_string()), Some("c-9".into()));
        }
    }
    ctx.stop().unwrap();
}

fn wall_clock_millis() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}

/// Messages larger than ActiveMQ's 128 KiB max-frame-size (and than ours), split into many
/// transfer frames in both directions.
#[test]
fn live_activemq_amqp_large_messages() {
    if !enabled() {
        return;
    }
    let q = n("large");
    let ctx = context();
    let q2 = q.clone();
    ctx.add_routes(move |r| {
        r.from(&format!("amqp:queue:{}?maxFrameSize=16384", q2)).to("mock:large");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:large").unwrap();
    mock.expected_message_count(2);
    let bytes: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    let text: String = (0..600_000).map(|i| char::from(b'a' + (i % 26) as u8)).collect();
    let t = ctx.create_producer_template();
    t.send_body(&format!("amqp:queue:{}", q), Value::Bytes(bytes.clone())).unwrap();
    t.send_body(&format!("amqp:queue:{}", q), text.clone()).unwrap();
    mock.assert_is_satisfied_within(Duration::from_secs(30)).unwrap();
    assert!(mock.exchange(0).unwrap().body() == &Value::Bytes(bytes), "3 MB bytes body intact");
    assert!(mock.exchange(1).unwrap().body() == &Value::from(text), "600 KB text body intact");
    ctx.stop().unwrap();
}

/// Redelivery: `CLIENT_ACKNOWLEDGE` returns a failed message with `modified(delivery-failed)`,
/// and a rolled back AMQP transaction returns it to the queue; ActiveMQ redelivers it with a
/// delivery count (`JMSRedelivered=true`, `JMSXDeliveryCount`).
#[test]
fn live_activemq_amqp_redelivery() {
    if !enabled() {
        return;
    }
    let (cq, tq) = (n("client-ack"), n("tx-redeliver"));
    let ctx = context();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (s1, s2) = (seen.clone(), seen.clone());
    let (c2, t2) = (cq.clone(), tq.clone());
    ctx.add_routes(move |r| {
        r.from(&format!("amqp:queue:{}?acknowledgementModeName=CLIENT_ACKNOWLEDGE", c2))
            .process(move |ex: &mut Exchange| {
                let red = ex.header("JMSRedelivered").and_then(|v| v.as_bool()).unwrap_or(false);
                let count = ex.header("JMSXDeliveryCount").and_then(|v| v.as_i64()).unwrap_or(0);
                s1.lock().unwrap().push(("client", red, count));
                if count < 3 {
                    return Err(CamelError::new("IOException", "not yet"));
                }
                Ok(())
            })
            .to("mock:client");
        r.from(&format!("amqp:queue:{}?transacted=true", t2))
            .process(move |ex: &mut Exchange| {
                let red = ex.header("JMSRedelivered").and_then(|v| v.as_bool()).unwrap_or(false);
                let count = ex.header("JMSXDeliveryCount").and_then(|v| v.as_i64()).unwrap_or(0);
                s2.lock().unwrap().push(("tx", red, count));
                if count < 2 {
                    return Err(CamelError::new("IOException", "roll back"));
                }
                Ok(())
            })
            .to("mock:tx");
    })
    .unwrap();
    ctx.start().unwrap();
    let (client, tx) = (ctx.mock_endpoint("mock:client").unwrap(), ctx.mock_endpoint("mock:tx").unwrap());
    client.expected_bodies_received(vec!["c"]);
    tx.expected_bodies_received(vec!["t"]);
    let t = ctx.create_producer_template();
    t.send_body(&format!("amqp:queue:{}", cq), "c").unwrap();
    t.send_body(&format!("amqp:queue:{}", tq), "t").unwrap();
    MockEndpoint::assert_all_satisfied(&[&client, &tx]).unwrap();
    let seen = seen.lock().unwrap().clone();
    let of = |k: &str| seen.iter().filter(|s| s.0 == k).map(|s| (s.1, s.2)).collect::<Vec<_>>();
    assert_eq!(of("client"), vec![(false, 1), (true, 2), (true, 3)], "{:?}", seen);
    assert_eq!(of("tx"), vec![(false, 1), (true, 2)], "{:?}", seen);
    ctx.stop().unwrap();
}

/// ActiveMQ 5 has no JMS 2.0 shared subscriptions (it ignores the `shared` capability, so
/// every member would get every message): refused at route start.
#[test]
fn live_activemq_amqp_shared_subscriptions_refused() {
    if !enabled() {
        return;
    }
    let topic = n("shared");
    let ctx = context();
    ctx.add_routes(move |r| {
        r.from(&format!("amqp:topic:{}?subscriptionShared=true&subscriptionName=workers&concurrentConsumers=2", topic)).to("mock:never");
    })
    .unwrap();
    let err = ctx.start().unwrap_err();
    assert!(err.chain().any(|c| c.kind == "UnsupportedOperationException" && c.message.contains("ActiveMQ 5.17") && c.message.contains("shared subscriptions")), "{:?}", err);
    let _ = ctx.stop();
}

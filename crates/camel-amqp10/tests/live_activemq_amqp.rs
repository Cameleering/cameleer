//! Live interop: the `amqp` component (JMS over AMQP 1.0) against the AMQP transport connector
//! of the Apache ActiveMQ 5.17 broker started by `scripts/live-interop.sh`. Runs only with
//! `CAMEL_LIVE_INTEROP=1`, and only when the broker has its AMQP connector (port 15675):
//! Ubuntu's `activemq` package does not ship the `activemq-amqp` module, so the script enables
//! the connector only when `activemq-amqp-5.17.x.jar` has been put into
//! `$CAMEL_INTEROP_DIR/activemq/lib` (proton-j comes from `libapache-qpid-proton-j-java`).
//! Without it every test prints a skip notice and passes.
//!
//! Covers what RabbitMQ's AMQP 1.0 plugin cannot: JMS selectors
//! (`apache.org:selector-filter:string`), AMQP 1.0 transactions (coordinator), dynamic
//! temporary reply queues for InOut and durable subscriptions, plus AMQPS and SASL PLAIN.

use std::net::TcpStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use camel_core::prelude::*;
use camel_tls::testing::live;

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

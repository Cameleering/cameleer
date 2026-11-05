//! AMQP 1.0 client against the embedded broker: SASL, credit, settlement modes,
//! dispositions, multi-frame messages, selectors, dynamic nodes, heartbeats and TLS.

use std::time::Duration;

use cameleer_amqp10::client::{Connection, ConnectionOptions, LinkOptions};
use cameleer_amqp10::codec::AmqpValue;
use cameleer_amqp10::message::{Body, Message};
use cameleer_amqp10::testing::{Amqp10Broker, SELECTOR_FILTER};
use cameleer_amqp10::types::{AmqpError, DeliveryState};

const T: Duration = Duration::from_secs(5);

fn opts(b: &Amqp10Broker) -> ConnectionOptions {
    ConnectionOptions::new("127.0.0.1", b.port())
}

fn text(d: &cameleer_amqp10::client::Delivery) -> String {
    match &d.message.body {
        Body::Value(v) => v.as_str().unwrap_or("").to_string(),
        other => format!("{:?}", other),
    }
}

#[test]
fn send_receive_at_least_once_with_credit() {
    let b = Amqp10Broker::start().unwrap();
    let conn = Connection::open(&opts(&b)).unwrap();
    assert_eq!(conn.remote_open().unwrap().container_id, "cameleer-amqp10-broker");
    let s = conn.session().unwrap();
    let tx = s.sender(LinkOptions::new("q1")).unwrap();
    for i in 0..5 {
        assert_eq!(tx.send(&Message::text(format!("m{}", i)), T).unwrap(), Some(DeliveryState::Accepted));
    }
    assert_eq!(b.queue_depth("q1"), 5);
    // manual credit: exactly two deliveries
    let rx = s.receiver(LinkOptions::new("q1").credit(0)).unwrap();
    assert!(rx.receive(Duration::from_millis(100)).unwrap().is_none());
    rx.flow(2).unwrap();
    let d1 = rx.receive(T).unwrap().unwrap();
    let d2 = rx.receive(T).unwrap().unwrap();
    assert_eq!((text(&d1), text(&d2)), ("m0".to_string(), "m1".to_string()));
    assert!(!d1.settled);
    assert!(rx.receive(Duration::from_millis(100)).unwrap().is_none());
    assert_eq!(b.inflight_count(), 2);
    rx.accept(&d1).unwrap();
    // released goes back to the head of the queue, modified(delivery-failed) increments delivery-count
    rx.release(&d2).unwrap();
    rx.flow(1).unwrap();
    let again = rx.receive(T).unwrap().unwrap();
    assert_eq!(text(&again), "m1");
    assert_eq!(again.message.header.as_ref().map(|h| h.delivery_count).unwrap_or(0), 0);
    rx.modify(&again, true, false, vec![]).unwrap();
    rx.flow(1).unwrap();
    let third = rx.receive(T).unwrap().unwrap();
    assert_eq!(text(&third), "m1");
    assert_eq!(third.message.header.as_ref().unwrap().delivery_count, 1);
    rx.reject(&third, Some(AmqpError::new("amqp:internal-error", "bad"))).unwrap();
    // drain: the broker has 3 messages, credit 10 -> all three, then credit is used up
    rx.flow(10).unwrap();
    let mut got = Vec::new();
    while let Some(d) = rx.receive(Duration::from_millis(300)).unwrap() {
        rx.accept(&d).unwrap();
        got.push(text(&d));
    }
    assert_eq!(got, vec!["m2", "m3", "m4"]);
    rx.drain(T).unwrap();
    assert_eq!(rx.credit(), 0);
    assert!(b.wait_for(T, |b| b.inflight_count() == 0));
    rx.close();
    tx.close();
    s.end();
    conn.close();
    assert!(b.wait_for(T, |b| b.connection_count() == 0));
}

#[test]
fn at_most_once_and_auto_credit() {
    let b = Amqp10Broker::start().unwrap();
    let conn = Connection::open(&opts(&b)).unwrap();
    let s = conn.session().unwrap();
    let tx = s.sender(LinkOptions::new("fast").at_most_once()).unwrap();
    for i in 0..300 {
        assert_eq!(tx.send(&Message::text(i.to_string()), T).unwrap(), None);
    }
    let rx = s.receiver(LinkOptions::new("fast").at_most_once().credit(20)).unwrap();
    for i in 0..300 {
        let d = rx.receive(T).unwrap().unwrap();
        assert!(d.settled);
        assert_eq!(text(&d), i.to_string());
    }
    assert_eq!(b.inflight_count(), 0);
    assert_eq!(b.queue_depth("fast"), 0);
}

#[test]
fn large_multi_frame_messages_both_ways() {
    let b = Amqp10Broker::builder().max_frame_size(4096).start().unwrap();
    let conn = Connection::open(&opts(&b).max_frame_size(2048)).unwrap();
    let s = conn.session().unwrap();
    let tx = s.sender(LinkOptions::new("big")).unwrap();
    let payload: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let mut m = Message::data(payload.clone());
    m.set_app_property("size", AmqpValue::Long(payload.len() as i64));
    tx.send_accepted(&m, T).unwrap();
    let rx = s.receiver(LinkOptions::new("big").credit(1)).unwrap();
    let d = rx.receive(T).unwrap().unwrap();
    assert_eq!(d.message.body_bytes().unwrap(), payload);
    assert_eq!(d.message.app_property("size"), Some(&AmqpValue::Long(300_000)));
    rx.accept(&d).unwrap();
}

#[test]
fn sasl_mechanisms() {
    let b = Amqp10Broker::builder().user("admin", "s3cret").start().unwrap();
    for mech in ["PLAIN", "SCRAM-SHA-256"] {
        let c = Connection::open(&opts(&b).credentials("admin", "s3cret").mechanism(mech)).unwrap();
        assert!(c.is_open());
    }
    // preferred mechanism with credentials: SCRAM
    Connection::open(&opts(&b).credentials("admin", "s3cret")).unwrap();
    let e = Connection::open(&opts(&b).credentials("admin", "wrong")).unwrap_err();
    assert_eq!(e.kind, "AuthenticationFailureException", "{:?}", e);
    let e = Connection::open(&opts(&b).credentials("admin", "wrong").mechanism("PLAIN")).unwrap_err();
    assert_eq!(e.kind, "AuthenticationFailureException");
    // anonymous is not offered
    let e = Connection::open(&opts(&b)).unwrap_err();
    assert_eq!(e.kind, "AuthenticationFailureException");
    // without the SASL layer the broker refuses
    let mut o = opts(&b);
    o.sasl = false;
    assert!(Connection::open(&o).is_err());
    // anonymous broker: ANONYMOUS, and no SASL layer at all
    let anon = Amqp10Broker::start().unwrap();
    Connection::open(&opts(&anon)).unwrap();
    let mut o = opts(&anon);
    o.sasl = false;
    Connection::open(&o).unwrap();
}

#[test]
fn selectors_topics_and_dynamic_nodes() {
    let b = Amqp10Broker::builder().topic("news").start().unwrap();
    let conn = Connection::open(&opts(&b)).unwrap();
    let s = conn.session().unwrap();
    let tx = s.sender(LinkOptions::new("orders")).unwrap();
    for (i, region) in ["EU", "US", "EU"].iter().enumerate() {
        let mut m = Message::text(format!("o{}", i));
        m.set_app_property("region", AmqpValue::str(*region));
        tx.send_accepted(&m, T).unwrap();
    }
    let eu = s.receiver(LinkOptions::new("orders").filter("jms-selector", SELECTOR_FILTER, AmqpValue::str("region = 'EU'"))).unwrap();
    let a = eu.receive(T).unwrap().unwrap();
    let c = eu.receive(T).unwrap().unwrap();
    assert_eq!((text(&a), text(&c)), ("o0".into(), "o2".into()));
    assert!(eu.receive(Duration::from_millis(100)).unwrap().is_none());
    eu.accept(&a).unwrap();
    eu.accept(&c).unwrap();
    assert_eq!(b.queue_depth("orders"), 1);
    // bad selector refused
    let err = s.receiver(LinkOptions::new("orders").filter("jms-selector", SELECTOR_FILTER, AmqpValue::str("region = "))).err().unwrap();
    assert!(err.message.contains("selector"), "{}", err);

    // topic: every subscriber gets a copy
    let sub1 = s.receiver(LinkOptions::new("news")).unwrap();
    let sub2 = s.receiver(LinkOptions::new("news")).unwrap();
    assert_eq!(b.subscriber_count("news"), 2);
    let pubr = s.sender(LinkOptions::new("news")).unwrap();
    pubr.send_accepted(&Message::text("headline"), T).unwrap();
    for sub in [&sub1, &sub2] {
        let d = sub.receive(T).unwrap().unwrap();
        assert_eq!(text(&d), "headline");
        sub.accept(&d).unwrap();
    }
    sub1.close();
    assert_eq!(b.subscriber_count("news"), 1);

    // dynamic node as a reply queue
    let tmp = s.receiver(LinkOptions::dynamic()).unwrap();
    let addr = tmp.address().unwrap().to_string();
    assert!(addr.starts_with("temp-queue"));
    let reply = s.sender(LinkOptions::new(&addr)).unwrap();
    reply.send_accepted(&Message::text("pong"), T).unwrap();
    assert_eq!(text(&tmp.receive(T).unwrap().unwrap()), "pong");
    tmp.close();
    assert_eq!(b.queue_depth(&addr), 0);
}

#[test]
fn unsettled_deliveries_are_redelivered_after_connection_loss() {
    let b = Amqp10Broker::start().unwrap();
    {
        let conn = Connection::open(&opts(&b)).unwrap();
        let s = conn.session().unwrap();
        s.sender(LinkOptions::new("work")).unwrap().send_accepted(&Message::text("job"), T).unwrap();
        let rx = s.receiver(LinkOptions::new("work")).unwrap();
        let d = rx.receive(T).unwrap().unwrap();
        assert_eq!(text(&d), "job");
        // not settled; connection dropped
    }
    assert!(b.wait_for(T, |b| b.queue_depth("work") == 1));
    let conn = Connection::open(&opts(&b)).unwrap();
    let rx = conn.session().unwrap().receiver(LinkOptions::new("work")).unwrap();
    let d = rx.receive(T).unwrap().unwrap();
    assert_eq!(d.message.header.as_ref().unwrap().delivery_count, 1);
    rx.accept(&d).unwrap();
}

#[test]
fn idle_timeout_and_heartbeats() {
    // the broker sends heartbeats: the connection survives several idle periods
    let b = Amqp10Broker::start().unwrap();
    let conn = Connection::open(&opts(&b).idle_timeout(Duration::from_millis(500))).unwrap();
    std::thread::sleep(Duration::from_millis(1200));
    assert!(conn.is_open());
    conn.session().unwrap();
    // a silent broker: the client closes the connection after its idle timeout
    let silent = Amqp10Broker::builder().heartbeats(false).start().unwrap();
    let conn = Connection::open(&opts(&silent).idle_timeout(Duration::from_millis(200))).unwrap();
    let start = std::time::Instant::now();
    while conn.is_open() && start.elapsed() < T {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!conn.is_open());
    assert!(conn.session().is_err());
    // a broker with an idle timeout is kept alive by the client's heartbeats
    let strict = Amqp10Broker::builder().idle_timeout(Duration::from_millis(500)).start().unwrap();
    let conn = Connection::open(&opts(&strict)).unwrap();
    std::thread::sleep(Duration::from_millis(1200));
    assert!(conn.is_open());
    assert_eq!(strict.connection_count(), 1);
}

#[test]
fn tls_amqps_and_external() {
    let b = Amqp10Broker::builder().tls(cameleer_tls::testing::server_params().server_config().unwrap()).start().unwrap();
    assert!(b.url().starts_with("amqps://"));
    let tls = cameleer_tls::camel_support::EndpointTls::new(std::sync::Arc::new(cameleer_tls::testing::client_params()));
    let conn = Connection::open(&ConnectionOptions::new("localhost", b.port()).tls(tls.clone())).unwrap();
    let s = conn.session().unwrap();
    s.sender(LinkOptions::new("secure")).unwrap().send_accepted(&Message::text("over tls"), T).unwrap();
    let rx = s.receiver(LinkOptions::new("secure")).unwrap();
    assert_eq!(text(&rx.receive(T).unwrap().unwrap()), "over tls");
    Connection::open(&ConnectionOptions::new("localhost", b.port()).tls(tls).mechanism("EXTERNAL")).unwrap();
    // plain AMQP against the TLS port fails, so does an untrusting client
    assert!(Connection::open(&ConnectionOptions::new("127.0.0.1", b.port())).is_err());
    let bad = cameleer_tls::camel_support::EndpointTls::new(std::sync::Arc::new(cameleer_tls::testing::untrusting_client_params()));
    let e = Connection::open(&ConnectionOptions::new("localhost", b.port()).tls(bad)).unwrap_err();
    assert!(e.chain().any(|c| c.is_a("SSLException")), "{:?}", e);
}

#[test]
fn connection_refused_maps_to_connect_exception() {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let e = Connection::open(&ConnectionOptions::new("127.0.0.1", port)).unwrap_err();
    assert_eq!(e.kind, "ConnectException");
}

/// Interop with a real AMQP 1.0 broker (Artemis, Qpid, ...): set `AMQP10_INTEROP_URL`
/// (e.g. `amqp://admin:admin@localhost:5672`) and run with `--ignored`.
#[test]
#[ignore]
fn interop_with_real_broker() {
    let Ok(url) = std::env::var("AMQP10_INTEROP_URL") else { return };
    let u = cameleer_amqp10::util::parse_amqp_url(&url).unwrap();
    let mut o = ConnectionOptions::new(&u.host, u.port);
    o.username = u.username;
    o.password = u.password;
    let conn = Connection::open(&o).unwrap();
    let s = conn.session().unwrap();
    let q = format!("camel-interop-{}", cameleer_amqp10::util::unique_id());
    s.sender(LinkOptions::new(&q).capability("queue")).unwrap().send_accepted(&Message::text("interop"), T).unwrap();
    let rx = s.receiver(LinkOptions::new(&q).capability("queue")).unwrap();
    let d = rx.receive(T).unwrap().unwrap();
    assert_eq!(text(&d), "interop");
    rx.accept(&d).unwrap();
}

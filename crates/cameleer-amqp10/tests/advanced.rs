//! AMQP 1.0 protocol features against the embedded broker: local transactions
//! (coordinator, declare / discharge, transactional transfers and dispositions),
//! `rcv-settle-mode = second`, AMQP over WebSockets, JMS 2.0 shared subscriptions,
//! Event Hubs epochs and Service Bus sessions / lock renewal / deferral at link level.

use std::time::Duration;

use cameleer_amqp10::client::{Connection, ConnectionOptions, LinkOptions};
use cameleer_amqp10::codec::AmqpValue;
use cameleer_amqp10::message::{Body, Message};
use cameleer_amqp10::testing::{Amqp10Broker, EPOCH_PROPERTY, SESSION_FILTER};
use cameleer_amqp10::types::{DeliveryState, RcvSettleMode, SndSettleMode};

const T: Duration = Duration::from_secs(5);

fn opts(b: &Amqp10Broker) -> ConnectionOptions {
    ConnectionOptions::new("127.0.0.1", b.port())
}

fn text(d: &cameleer_amqp10::client::Delivery) -> String {
    match &d.message.body {
        Body::Value(v) => v.as_str().unwrap_or("").to_string(),
        Body::Data(d) => String::from_utf8_lossy(&d.concat()).to_string(),
        other => format!("{:?}", other),
    }
}

#[test]
fn transactional_sends_commit_and_rollback() {
    let b = Amqp10Broker::start().unwrap();
    let conn = Connection::open(&opts(&b)).unwrap();
    let s = conn.session().unwrap();
    let coord = s.coordinator().unwrap();
    let tx = s.sender(LinkOptions::new("txq")).unwrap();
    let id = coord.declare().unwrap();
    assert_eq!(b.open_transactions(), 1);
    let state = tx.send_in_txn(&Message::text("a"), &id, T).unwrap();
    assert!(matches!(state, Some(DeliveryState::Transactional { outcome: Some(ref o), .. }) if **o == DeliveryState::Accepted));
    tx.send_accepted_in_txn(&Message::text("b"), &id, T).unwrap();
    // not visible before the commit
    assert_eq!(b.queue_depth("txq"), 0);
    coord.commit(&id).unwrap();
    assert_eq!(b.queue_depth("txq"), 2);
    let id2 = coord.declare().unwrap();
    tx.send_accepted_in_txn(&Message::text("c"), &id2, T).unwrap();
    coord.rollback(&id2).unwrap();
    assert_eq!(b.queue_depth("txq"), 2);
    assert_eq!(b.transaction_counts(), (1, 1));
    // discharging an unknown transaction is refused
    assert!(coord.commit(b"nope").is_err());
    conn.close();
}

#[test]
fn transactional_acknowledgements() {
    let b = Amqp10Broker::start().unwrap();
    let conn = Connection::open(&opts(&b)).unwrap();
    let s = conn.session().unwrap();
    let tx = s.sender(LinkOptions::new("acks")).unwrap();
    tx.send_accepted(&Message::text("one"), T).unwrap();
    tx.send_accepted(&Message::text("two"), T).unwrap();
    let coord = s.coordinator().unwrap();
    let rx = s.receiver(LinkOptions::new("acks").credit(0)).unwrap();
    rx.flow(1).unwrap();
    // rollback: the acquired message comes back, redelivered
    let id = coord.declare().unwrap();
    let d = rx.receive(T).unwrap().unwrap();
    assert_eq!(text(&d), "one");
    rx.settle_in_txn(&d, &id, DeliveryState::Accepted).unwrap();
    assert!(b.wait_for(T, |b| b.inflight_count() == 0));
    coord.rollback(&id).unwrap();
    rx.flow(1).unwrap();
    let again = rx.receive(T).unwrap().unwrap();
    assert_eq!(text(&again), "one");
    assert_eq!(again.message.header.as_ref().unwrap().delivery_count, 1);
    // commit: consumed and a reply sent atomically
    let id = coord.declare().unwrap();
    rx.settle_in_txn(&again, &id, DeliveryState::Accepted).unwrap();
    let out = s.sender(LinkOptions::new("replies")).unwrap();
    out.send_accepted_in_txn(&Message::text("reply"), &id, T).unwrap();
    assert_eq!(b.queue_depth("replies"), 0);
    coord.commit(&id).unwrap();
    assert_eq!(b.queue_depth("replies"), 1);
    assert!(b.wait_for(T, |b| b.queue_depth("acks") == 1 && b.inflight_count() == 0));
    rx.flow(1).unwrap();
    let d = rx.receive(T).unwrap().unwrap();
    assert_eq!(text(&d), "two");
    // an open transaction rolls back when the connection closes
    let id = coord.declare().unwrap();
    rx.settle_in_txn(&d, &id, DeliveryState::Accepted).unwrap();
    out.send_accepted_in_txn(&Message::text("lost"), &id, T).unwrap();
    conn.close();
    assert!(b.wait_for(T, |b| b.queue_depth("acks") == 1 && b.open_transactions() == 0));
    assert_eq!(b.queue_depth("replies"), 1);
}

#[test]
fn receiver_settle_mode_second() {
    let b = Amqp10Broker::start().unwrap();
    let conn = Connection::open(&opts(&b)).unwrap();
    let s = conn.session().unwrap();
    let tx = s.sender(LinkOptions::new("second").settle_mode(SndSettleMode::Unsettled, RcvSettleMode::Second)).unwrap();
    assert_eq!(tx.send(&Message::text("x"), T).unwrap(), Some(DeliveryState::Accepted));
    let rx = s.receiver(LinkOptions::new("second").settle_mode(SndSettleMode::Unsettled, RcvSettleMode::Second).credit(5)).unwrap();
    assert!(rx.settles_second());
    let d = rx.receive(T).unwrap().unwrap();
    rx.accept(&d).unwrap();
    assert_eq!(b.inflight_count(), 0);
    assert_eq!(b.queue_depth("second"), 0);
    conn.close();
}

#[test]
fn amqp_over_websockets() {
    let b = Amqp10Broker::start().unwrap();
    let conn = Connection::open(&opts(&b).websocket(cameleer_amqp10::ws::AMQP_WS_PATH)).unwrap();
    let s = conn.session().unwrap();
    let tx = s.sender(LinkOptions::new("wsq")).unwrap();
    let big = "w".repeat(200_000);
    tx.send_accepted(&Message::text(big.clone()), T).unwrap();
    let rx = s.receiver(LinkOptions::new("wsq")).unwrap();
    let d = rx.receive(T).unwrap().unwrap();
    assert_eq!(text(&d), big);
    rx.accept(&d).unwrap();
    conn.close();
}

#[test]
fn shared_subscriptions_compete() {
    let b = Amqp10Broker::builder().topic("news").start().unwrap();
    let mut c1 = opts(&b);
    c1.container_id = Some("app".into());
    let conn = Connection::open(&c1).unwrap();
    let s = conn.session().unwrap();
    let shared = |name: &str| {
        let mut o = LinkOptions::new("news").name(name).capability("topic").capability("shared").credit(10);
        o.durable = 2;
        o.expiry_policy = Some("never".into());
        o
    };
    let r1 = s.receiver(shared("sub")).unwrap();
    let r2 = s.receiver(shared("sub|2")).unwrap();
    let plain = s.receiver(LinkOptions::new("news").capability("topic").credit(10)).unwrap();
    let tx = s.sender(LinkOptions::new("news").capability("topic")).unwrap();
    for i in 0..4 {
        tx.send_accepted(&Message::text(format!("n{}", i)), T).unwrap();
    }
    let mut shared_got = Vec::new();
    for _ in 0..4 {
        let d = plain.receive(T).unwrap().unwrap();
        plain.accept(&d).unwrap();
    }
    let deadline = std::time::Instant::now() + T;
    while shared_got.len() < 4 && std::time::Instant::now() < deadline {
        for r in [&r1, &r2] {
            if let Some(d) = r.receive(Duration::from_millis(20)).unwrap() {
                r.accept(&d).unwrap();
                shared_got.push((std::ptr::eq(r, &r1), text(&d)));
            }
        }
    }
    shared_got.sort_by(|a, b| a.1.cmp(&b.1));
    assert_eq!(shared_got.iter().map(|x| x.1.clone()).collect::<Vec<_>>(), vec!["n0", "n1", "n2", "n3"]);
    assert!(shared_got.iter().any(|x| x.0) && shared_got.iter().any(|x| !x.0), "both members get messages: {:?}", shared_got);
    // the durable shared subscription keeps messages while no member is attached
    r1.detach();
    r2.detach();
    tx.send_accepted(&Message::text("later"), T).unwrap();
    let r3 = s.receiver(shared("sub")).unwrap();
    assert_eq!(text(&r3.receive(T).unwrap().unwrap()), "later");
    conn.close();
}

#[test]
fn event_hub_epochs() {
    let b = Amqp10Broker::builder().azure_jwt("tok").event_hub("hub", 2).start().unwrap();
    let conn = Connection::open(&opts(&b).mechanism("ANONYMOUS")).unwrap();
    let s = conn.session().unwrap();
    cameleer_amqp10::cbs::put_token(&s, &format!("sb://127.0.0.1/hub"), "tok", cameleer_amqp10::cbs::JWT_TOKEN_TYPE, T).unwrap();
    let addr = "hub/ConsumerGroups/$Default/Partitions/0";
    let filter = |o: LinkOptions| o.at_most_once().filter("apache.org:selector-filter:string", cameleer_amqp10::types::SELECTOR_FILTER, AmqpValue::str("amqp.annotation.x-opt-offset > '-1'"));
    let r0 = s.receiver(filter(LinkOptions::new(addr)).property(EPOCH_PROPERTY, AmqpValue::Long(1))).unwrap();
    // a lower epoch and a receiver without epoch are refused
    assert!(s.receiver(filter(LinkOptions::new(addr)).property(EPOCH_PROPERTY, AmqpValue::Long(0))).is_err());
    assert!(s.receiver(filter(LinkOptions::new(addr))).is_err());
    assert_eq!(b.partition_receivers("hub", "$Default", 0), vec![Some(1)]);
    // a higher epoch steals the partition
    let r1 = s.receiver(filter(LinkOptions::new(addr)).property(EPOCH_PROPERTY, AmqpValue::Long(2))).unwrap();
    let err = r0.receive(T).unwrap_err();
    assert!(err.message.contains("epoch"), "{}", err.message);
    assert_eq!(b.partition_receivers("hub", "$Default", 0), vec![Some(2)]);
    drop(r1);
    conn.close();
}

#[test]
fn service_bus_sessions_lock_renewal_and_deferral_at_link_level() {
    let b = Amqp10Broker::builder().azure_jwt("tok").session_entity("sq").lock_duration(Duration::from_millis(800)).start().unwrap();
    let conn = Connection::open(&opts(&b).mechanism("ANONYMOUS")).unwrap();
    let s = conn.session().unwrap();
    cameleer_amqp10::cbs::put_token(&s, "sb://127.0.0.1/sq", "tok", cameleer_amqp10::cbs::JWT_TOKEN_TYPE, T).unwrap();
    let tx = s.sender(LinkOptions::new("sq")).unwrap();
    for (sid, body) in [("s1", "a"), ("s2", "b"), ("s1", "c")] {
        let mut m = Message::text(body);
        m.props_mut().group_id = Some(sid.into());
        tx.send_accepted(&m, T).unwrap();
    }
    // a receiver without a session filter is refused on a session entity
    assert!(s.receiver(LinkOptions::new("sq")).is_err());
    let session_link = |sid: Option<&str>| LinkOptions::new("sq").filter(SESSION_FILTER, SESSION_FILTER, sid.map(AmqpValue::str).unwrap_or(AmqpValue::Null)).credit(10);
    let r1 = s.receiver(session_link(Some("s1"))).unwrap();
    assert!(r1.remote_attach().properties.iter().any(|(k, _)| k.as_str() == Some("com.microsoft:locked-until-utc")));
    // the next available session is s2
    let r2 = s.receiver(session_link(None)).unwrap();
    let sid = r2.remote_attach().source.as_ref().unwrap().filter.iter().find_map(|(_, v)| v.undescribed().as_str().map(String::from));
    assert_eq!(sid.as_deref(), Some("s2"));
    // s1 is locked by r1
    assert!(s.receiver(session_link(Some("s1"))).is_err());
    assert_eq!(b.locked_sessions("sq"), vec!["s1", "s2"]);
    let d1 = r1.receive(T).unwrap().unwrap();
    let d2 = r1.receive(T).unwrap().unwrap();
    assert_eq!((text(&d1), text(&d2)), ("a".into(), "c".into()));
    let d3 = r2.receive(T).unwrap().unwrap();
    assert_eq!(text(&d3), "b");
    // management: renew the message lock, defer, receive deferred by sequence number
    let mgmt = cameleer_amqp10::cbs::RequestResponse::open(&s, "sq/$management", T).unwrap();
    let token = d1.lock_token().map(|t| cameleer_amqp10::message::parse_uuid(&t).unwrap()).unwrap();
    let before = b.lock_expirations("sq").into_iter().min().unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let mut req = Message { body: Body::Value(AmqpValue::Map(vec![(AmqpValue::str("lock-tokens"), AmqpValue::Array(vec![AmqpValue::Uuid(token)]))])), ..Default::default() };
    req.set_app_property("operation", AmqpValue::str("com.microsoft:renew-lock"));
    let resp = mgmt.request(req).unwrap();
    assert_eq!(cameleer_amqp10::cbs::RequestResponse::status(&resp).0, 200);
    assert!(b.lock_expirations("sq").into_iter().max().unwrap() > before);
    r1.accept(&d2).unwrap();
    let seq = d1.message.annotation("x-opt-sequence-number").and_then(|v| v.as_i64()).unwrap();
    r1.modify(&d1, false, true, vec![]).unwrap();
    assert!(b.wait_for(T, |b| b.deferred_count("sq") == 1));
    let mut req = Message {
        body: Body::Value(AmqpValue::Map(vec![(AmqpValue::str("sequence-numbers"), AmqpValue::Array(vec![AmqpValue::Long(seq)])), (AmqpValue::str("receiver-settle-mode"), AmqpValue::Uint(0))])),
        ..Default::default()
    };
    req.set_app_property("operation", AmqpValue::str("com.microsoft:receive-by-sequence-number"));
    let resp = mgmt.request(req).unwrap();
    assert_eq!(cameleer_amqp10::cbs::RequestResponse::status(&resp).0, 200);
    assert_eq!(b.deferred_count("sq"), 0);
    // session state
    let mut req = Message { body: Body::Value(AmqpValue::Map(vec![(AmqpValue::str("session-id"), AmqpValue::str("s2")), (AmqpValue::str("session-state"), AmqpValue::Binary(b"step-3".to_vec()))])), ..Default::default() };
    req.set_app_property("operation", AmqpValue::str("com.microsoft:set-session-state"));
    mgmt.request(req).unwrap();
    assert_eq!(b.session_state("sq", "s2").as_deref(), Some(&b"step-3"[..]));
    r2.accept(&d3).unwrap();
    // an expired session lock detaches the receiver
    let err = loop {
        match r2.receive(Duration::from_millis(100)) {
            Err(e) => break e,
            Ok(_) => continue,
        }
    };
    assert!(err.message.contains("session lock"), "{}", err.message);
    conn.close();
}

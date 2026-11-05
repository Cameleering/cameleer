//! AMQP 0-9-1 client against the embedded broker.

use std::time::Duration;

use cameleer_amqp::amqp::client::*;
use cameleer_amqp::amqp::codec::{BasicProperties, Table};
use cameleer_amqp::testing::AmqpBroker;
use cameleer_core::value::Value;

fn cfg(broker: &AmqpBroker) -> AmqpConfig {
    AmqpConfig { host: "127.0.0.1".into(), port: broker.port(), heartbeat: 0, ..Default::default() }
}

fn recv(sub: &Subscription) -> Delivery {
    sub.deliveries.recv_timeout(Duration::from_secs(5)).expect("delivery")
}

fn text(d: &Delivery) -> String {
    String::from_utf8(d.body.clone()).unwrap()
}

#[test]
fn exchange_types_route_messages() {
    let broker = AmqpBroker::start().unwrap();
    let conn = Connection::open(&cfg(&broker)).unwrap();
    let ch = conn.channel().unwrap();
    for (ex, kind) in [("d", "direct"), ("t", "topic"), ("f", "fanout"), ("h", "headers")] {
        ch.exchange_declare(ex, kind, &ExchangeOptions::default()).unwrap();
    }
    let q = |n: &str| ch.queue_declare(n, &QueueOptions::default()).unwrap().name;
    for n in ["d1", "d2", "t1", "t2", "f1", "f2", "h1"] {
        q(n);
    }
    ch.queue_bind("d1", "d", "red", &Table::new()).unwrap();
    ch.queue_bind("d2", "d", "blue", &Table::new()).unwrap();
    ch.queue_bind("t1", "t", "orders.*", &Table::new()).unwrap();
    ch.queue_bind("t2", "t", "orders.#", &Table::new()).unwrap();
    ch.queue_bind("f1", "f", "", &Table::new()).unwrap();
    ch.queue_bind("f2", "f", "ignored", &Table::new()).unwrap();
    let mut hargs = Table::new();
    hargs.insert("x-match".into(), Value::from("all"));
    hargs.insert("format".into(), Value::from("pdf"));
    ch.queue_bind("h1", "h", "", &hargs).unwrap();

    let p = BasicProperties::default();
    ch.basic_publish("d", "red", false, &p, b"r").unwrap();
    ch.basic_publish("t", "orders.eu", false, &p, b"t-eu").unwrap();
    ch.basic_publish("t", "orders.eu.de", false, &p, b"t-de").unwrap();
    ch.basic_publish("f", "whatever", false, &p, b"fan").unwrap();
    let mut h = Table::new();
    h.insert("format".into(), Value::from("pdf"));
    ch.basic_publish("h", "", false, &BasicProperties { headers: Some(h), ..Default::default() }, b"pdf").unwrap();
    ch.basic_publish("h", "", false, &p, b"no-headers").unwrap();
    ch.basic_publish("", "d2", false, &p, b"default-exchange").unwrap();
    // synchronous round trip so all publishes are processed
    ch.queue_declare("d1", &QueueOptions { passive: true, ..Default::default() }).unwrap();
    let depth = |n: &str| broker.queue_depth(n);
    assert_eq!((depth("d1"), depth("d2")), (1, 1));
    assert_eq!((depth("t1"), depth("t2")), (1, 2));
    assert_eq!((depth("f1"), depth("f2")), (1, 1));
    assert_eq!(depth("h1"), 1);
    assert_eq!(broker.exchange_type("t").as_deref(), Some("topic"));
    let got = ch.basic_get("t2", true).unwrap().unwrap();
    assert_eq!((text(&got), got.routing_key.as_str(), got.message_count), ("t-eu".into(), "orders.eu", 1));
    assert!(ch.basic_get("t2", true).unwrap().is_some());
    assert!(ch.basic_get("t2", true).unwrap().is_none());

    // errors close the channel with the broker's reply
    let e = ch.queue_bind("nope", "d", "x", &Table::new()).unwrap_err();
    assert!(e.message.contains("404") && e.message.contains("no queue 'nope'"), "{}", e.message);
    assert!(!ch.is_open());
    let ch2 = conn.channel().unwrap();
    let e = ch2.exchange_declare("d", "fanout", &ExchangeOptions::default()).unwrap_err();
    assert!(e.message.contains("406"), "{}", e.message);
    conn.close();
    broker.stop();
}

#[test]
fn properties_headers_and_large_bodies_survive() {
    let broker = AmqpBroker::builder().frame_max(4096).start().unwrap();
    let conn = Connection::open(&cfg(&broker)).unwrap();
    assert_eq!(conn.frame_max(), 4096);
    let ch = conn.channel().unwrap();
    ch.queue_declare("big", &QueueOptions::default()).unwrap();
    let sub = ch.basic_consume("big", "", true, false, &Table::new()).unwrap();
    let mut headers = Table::new();
    headers.insert("count".into(), Value::Int(3));
    headers.insert("nested".into(), Value::object([("ok", Value::Bool(true))]));
    let props = BasicProperties {
        content_type: Some("application/octet-stream".into()),
        correlation_id: Some("abc".into()),
        priority: Some(7),
        delivery_mode: Some(2),
        headers: Some(headers.clone()),
        ..Default::default()
    };
    let body: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
    ch.basic_publish("", "big", false, &props, &body).unwrap();
    let d = recv(&sub);
    assert_eq!(d.body, body);
    assert_eq!(d.properties, props);
    assert_eq!(d.consumer_tag, sub.consumer_tag);
}

#[test]
fn manual_ack_prefetch_reject_requeue_and_dead_letter() {
    let broker = AmqpBroker::start().unwrap();
    let conn = Connection::open(&cfg(&broker)).unwrap();
    let ch = conn.channel().unwrap();
    ch.exchange_declare("dlx", "fanout", &ExchangeOptions::default()).unwrap();
    ch.queue_declare("dead", &QueueOptions::default()).unwrap();
    ch.queue_bind("dead", "dlx", "", &Table::new()).unwrap();
    let mut args = Table::new();
    args.insert("x-dead-letter-exchange".into(), Value::from("dlx"));
    ch.queue_declare("work", &QueueOptions { arguments: args, ..Default::default() }).unwrap();
    for i in 0..5 {
        ch.basic_publish("", "work", false, &BasicProperties::default(), format!("m{}", i).as_bytes()).unwrap();
    }
    ch.basic_qos(2, false).unwrap();
    let sub = ch.basic_consume("work", "", false, false, &Table::new()).unwrap();
    let a = recv(&sub);
    let b = recv(&sub);
    assert_eq!((text(&a), text(&b)), ("m0".into(), "m1".into()));
    // prefetch limit: no third delivery until something is acked
    assert!(sub.deliveries.recv_timeout(Duration::from_millis(150)).is_err());
    assert_eq!((broker.unacked_count("work"), broker.queue_depth("work")), (2, 3));
    ch.basic_ack(a.delivery_tag, false).unwrap();
    let c = recv(&sub);
    assert_eq!(text(&c), "m2");
    // reject with requeue: redelivered flag set
    ch.basic_reject(b.delivery_tag, true).unwrap();
    let again = recv(&sub);
    assert_eq!(text(&again), "m1");
    assert!(again.redelivered);
    // nack without requeue: dead-lettered with x-death
    ch.basic_nack(again.delivery_tag, false, false).unwrap();
    ch.basic_ack(c.delivery_tag, false).unwrap();
    let ch2 = conn.channel().unwrap();
    let mut dead = None;
    for _ in 0..100 {
        dead = ch2.basic_get("dead", true).unwrap();
        if dead.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let dead = dead.expect("dead-lettered message");
    assert_eq!(text(&dead), "m1");
    let death = dead.properties.headers.as_ref().unwrap().get("x-death").unwrap().as_array().unwrap()[0].clone();
    assert_eq!(death.get("reason").unwrap().to_string(), "rejected");
    assert_eq!(death.get("queue").unwrap().to_string(), "work");
    // closing the channel requeues unacked deliveries
    let d3 = recv(&sub);
    let d4 = recv(&sub);
    assert_eq!((text(&d3), text(&d4)), ("m3".into(), "m4".into()));
    ch.close().unwrap();
    let back = ch2.basic_get("work", false).unwrap().unwrap();
    assert!(back.redelivered);
    assert_eq!(text(&back), "m3");
    ch2.basic_ack(back.delivery_tag, false).unwrap();
    assert_eq!(broker.queue_depth("work"), 1);
}

#[test]
fn confirms_returns_competing_consumers_and_reply_queues() {
    let broker = AmqpBroker::start().unwrap();
    let conn = Connection::open(&cfg(&broker)).unwrap();
    let ch = conn.channel().unwrap();
    ch.confirm_select().unwrap();
    ch.queue_declare("jobs", &QueueOptions::default()).unwrap();
    let c1 = conn.channel().unwrap();
    let c2 = conn.channel().unwrap();
    let s1 = c1.basic_consume("jobs", "", true, false, &Table::new()).unwrap();
    let s2 = c2.basic_consume("jobs", "", true, false, &Table::new()).unwrap();
    for i in 0..10 {
        ch.basic_publish("", "jobs", false, &BasicProperties::default(), format!("{}", i).as_bytes()).unwrap();
    }
    assert!(ch.wait_for_confirms(Duration::from_secs(5)).unwrap());
    let (mut n1, mut n2) = (0, 0);
    for _ in 0..10 {
        if s1.deliveries.recv_timeout(Duration::from_millis(20)).is_ok() {
            n1 += 1;
        }
        if s2.deliveries.recv_timeout(Duration::from_millis(20)).is_ok() {
            n2 += 1;
        }
    }
    assert_eq!((n1, n2), (5, 5), "round robin");
    // mandatory + no route => returned
    ch.basic_publish("", "no-such-queue", true, &BasicProperties::default(), b"lost").unwrap();
    assert!(ch.wait_for_confirms(Duration::from_secs(5)).unwrap());
    let r = ch.take_returned();
    assert_eq!(r.len(), 1);
    assert_eq!((r[0].reply_code, r[0].routing_key.as_str(), r[0].body.as_slice()), (312, "no-such-queue", &b"lost"[..]));
    // server-named exclusive auto-delete queue disappears with its consumer
    let info = c1.queue_declare("", &QueueOptions { exclusive: true, auto_delete: true, ..Default::default() }).unwrap();
    assert!(info.name.starts_with("amq.gen-"));
    let s = c1.basic_consume(&info.name, "", true, false, &Table::new()).unwrap();
    assert!(broker.has_queue(&info.name));
    c1.basic_cancel(&s.consumer_tag).unwrap();
    assert!(!broker.has_queue(&info.name));
    // another connection cannot use an exclusive queue
    let info = c2.queue_declare("", &QueueOptions { exclusive: true, ..Default::default() }).unwrap();
    let other = Connection::open(&cfg(&broker)).unwrap();
    let e = other.channel().unwrap().queue_declare(&info.name, &QueueOptions::default()).unwrap_err();
    assert!(e.message.contains("405"), "{}", e.message);
    conn.close();
    // the broker notices the closed connection asynchronously
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while broker.has_queue(&info.name) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!broker.has_queue(&info.name), "exclusive queue deleted with its connection");
}

#[test]
fn authentication_vhosts_and_heartbeats() {
    let broker = AmqpBroker::builder().user("camel", "s3cret").vhost("prod").heartbeat(1).start().unwrap();
    let bad = AmqpConfig { username: "camel".into(), password: "wrong".into(), ..cfg(&broker) };
    let e = Connection::open(&bad).err().unwrap();
    assert_eq!(e.kind, AUTHENTICATION_FAILURE, "{}", e.message);
    let good = AmqpConfig { username: "camel".into(), password: "s3cret".into(), vhost: "prod".into(), heartbeat: 60, ..cfg(&broker) };
    let e = Connection::open(&AmqpConfig { vhost: "nope".into(), ..good.clone() }).err().unwrap();
    assert!(e.message.contains("530"), "{}", e.message);
    let conn = Connection::open(&good).unwrap();
    assert_eq!(conn.heartbeat(), 1, "the smaller non-zero heartbeat wins");
    let ch = conn.channel().unwrap();
    ch.queue_declare("hb", &QueueOptions::default()).unwrap();
    let start = std::time::Instant::now();
    while broker.heartbeats_received() == 0 && start.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(broker.heartbeats_received() > 0);
    assert!(conn.is_open());
    ch.basic_publish("", "hb", false, &BasicProperties::default(), b"still alive").unwrap();
    assert_eq!(text(&ch.basic_get("hb", true).unwrap().unwrap()), "still alive");
    // broker going away is detected
    broker.drop_connections();
    let start = std::time::Instant::now();
    while conn.is_open() && start.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!conn.is_open());
    assert!(ch.basic_publish("", "hb", false, &BasicProperties::default(), b"x").is_err());
}

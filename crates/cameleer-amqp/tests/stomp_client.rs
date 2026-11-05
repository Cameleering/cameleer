//! STOMP 1.2 client against the embedded STOMP broker.

use std::time::Duration;

use cameleer_amqp::stomp::client::*;
use cameleer_amqp::stomp::frame::Frame;
use cameleer_amqp::testing::StompBroker;

fn cfg(broker: &StompBroker) -> StompConfig {
    StompConfig { host: "127.0.0.1".into(), port: broker.port(), ..Default::default() }
}

fn recv(sub: &Subscription) -> Frame {
    sub.messages.recv_timeout(Duration::from_secs(5)).expect("message")
}

fn none(sub: &Subscription) -> bool {
    sub.messages.recv_timeout(Duration::from_millis(100)).is_err()
}

fn h(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

#[test]
fn queues_topics_and_selectors() {
    let broker = StompBroker::start().unwrap();
    let c = StompConnection::open(&cfg(&broker)).unwrap();
    assert_eq!(c.version(), "1.2");
    let q1 = c.subscribe("/queue/work", AckMode::Auto, &[]).unwrap();
    let q2 = c.subscribe("/queue/work", AckMode::Auto, &[]).unwrap();
    let t1 = c.subscribe("/topic/news", AckMode::Auto, &[]).unwrap();
    let t2 = c.subscribe("/topic/news", AckMode::Auto, &h(&[("selector", "region = 'EU' AND price > 10")])).unwrap();
    for i in 0..4 {
        c.send("/queue/work", &h(&[("n", &i.to_string())]), format!("job{}", i).as_bytes(), None, true).unwrap();
    }
    let a: Vec<String> = (0..2).map(|_| recv(&q1).body_string()).collect();
    let b: Vec<String> = (0..2).map(|_| recv(&q2).body_string()).collect();
    assert_eq!(a, vec!["job0", "job2"], "round robin");
    assert_eq!(b, vec!["job1", "job3"]);
    c.send("/topic/news", &h(&[("region", "EU"), ("price", "12")]), b"eu-12", None, true).unwrap();
    c.send("/topic/news", &h(&[("region", "US"), ("price", "50")]), b"us-50", None, true).unwrap();
    assert_eq!(recv(&t1).body_string(), "eu-12");
    assert_eq!(recv(&t1).body_string(), "us-50");
    let m = recv(&t2);
    assert_eq!(m.body_string(), "eu-12");
    assert_eq!(m.get("region"), Some("EU"));
    assert!(m.get("message-id").unwrap().starts_with("ID:"));
    assert!(none(&t2));
    // topics keep nothing for absent subscribers
    c.unsubscribe(&t1.id).unwrap();
    c.unsubscribe(&t2.id).unwrap();
    c.send("/topic/news", &[], b"lost", None, true).unwrap();
    assert_eq!(broker.queue_depth("/topic/news"), 0);
    // an invalid selector is an ERROR (and the connection is closed)
    let e = c.subscribe("/queue/x", AckMode::Auto, &h(&[("selector", "a = ")])).err().unwrap();
    assert_eq!(e.kind, STOMP_ERROR);
    assert!(!c.is_open());
}

#[test]
fn client_acks_nack_redelivery_prefetch_priority_and_dlq() {
    let broker = StompBroker::builder().max_redeliveries(2).start().unwrap();
    let c = StompConnection::open(&cfg(&broker)).unwrap();
    // priority ordering for queued messages
    c.send("/queue/p", &h(&[("priority", "1")]), b"low", None, true).unwrap();
    c.send("/queue/p", &h(&[("priority", "9")]), b"high", None, true).unwrap();
    c.send("/queue/p", &[], b"normal", None, true).unwrap();
    c.send("/queue/p", &h(&[("expires", "1")]), b"expired", None, true).unwrap();
    assert_eq!(broker.queue_depth("p"), 3, "expired messages are dropped");
    let sub = c.subscribe("/queue/p", AckMode::ClientIndividual, &h(&[("activemq.prefetchSize", "1")])).unwrap();
    let m1 = recv(&sub);
    assert_eq!(m1.body_string(), "high");
    assert!(none(&sub), "prefetch window of 1");
    assert_eq!(broker.pending_count("p"), 1);
    c.ack(m1.get("ack").unwrap(), None).unwrap();
    let m2 = recv(&sub);
    assert_eq!(m2.body_string(), "normal");
    c.nack(m2.get("ack").unwrap(), None).unwrap();
    let again = recv(&sub);
    assert_eq!((again.body_string().as_str(), again.get("redelivered")), ("normal", Some("true")));
    c.nack(again.get("ack").unwrap(), None).unwrap();
    let third = recv(&sub);
    c.nack(third.get("ack").unwrap(), None).unwrap();
    // after max_redeliveries the message goes to the DLQ
    let low = recv(&sub);
    assert_eq!(low.body_string(), "low");
    c.ack(low.get("ack").unwrap(), None).unwrap();
    let dlq = broker.queue_messages("ActiveMQ.DLQ");
    assert_eq!(dlq.len(), 1);
    assert_eq!(dlq[0].body_string(), "normal");
    assert_eq!(dlq[0].header("originalDestination"), Some("/queue/p"));
    // unacked messages are redelivered when the connection goes away
    c.send("/queue/p", &[], b"orphan", None, true).unwrap();
    let o = recv(&sub);
    assert_eq!(o.body_string(), "orphan");
    c.disconnect();
    let c2 = StompConnection::open(&cfg(&broker)).unwrap();
    let s2 = c2.subscribe("/queue/p", AckMode::Client, &[]).unwrap();
    let o2 = recv(&s2);
    assert_eq!((o2.body_string().as_str(), o2.get("redelivered")), ("orphan", Some("true")));
}

#[test]
fn transactions_commit_and_abort() {
    let broker = StompBroker::start().unwrap();
    let c = StompConnection::open(&cfg(&broker)).unwrap();
    c.begin("tx1").unwrap();
    c.send("/queue/t", &[], b"a", Some("tx1"), false).unwrap();
    c.send("/queue/t", &[], b"b", Some("tx1"), false).unwrap();
    c.begin("tx2").unwrap();
    c.send("/queue/t", &[], b"rolled-back", Some("tx2"), false).unwrap();
    c.abort("tx2").unwrap();
    assert_eq!(broker.queue_depth("t"), 0);
    c.commit("tx1").unwrap();
    assert_eq!(broker.queue_depth("t"), 2);
    let sub = c.subscribe("/queue/t", AckMode::ClientIndividual, &[]).unwrap();
    let a = recv(&sub);
    let b = recv(&sub);
    c.begin("tx3").unwrap();
    c.ack(a.get("ack").unwrap(), Some("tx3")).unwrap();
    c.ack(b.get("ack").unwrap(), Some("tx3")).unwrap();
    c.abort("tx3").unwrap();
    let ra = recv(&sub);
    let rb = recv(&sub);
    assert_eq!((ra.body_string().as_str(), ra.get("redelivered")), ("a", Some("true")));
    assert_eq!(rb.body_string(), "b");
    c.begin("tx4").unwrap();
    c.ack(ra.get("ack").unwrap(), Some("tx4")).unwrap();
    c.ack(rb.get("ack").unwrap(), Some("tx4")).unwrap();
    c.commit("tx4").unwrap();
    assert_eq!((broker.pending_count("t"), broker.queue_depth("t")), (0, 0));
    let e = c.commit("unknown").unwrap_err();
    assert!(e.message.contains("Invalid transaction"), "{}", e.message);
}

#[test]
fn login_temp_queues_and_heartbeats() {
    let broker = StompBroker::builder().user("admin", "secret").heartbeat(50, 50).start().unwrap();
    let e = StompConnection::open(&StompConfig { login: Some("admin".into()), passcode: Some("nope".into()), ..cfg(&broker) }).err().unwrap();
    assert_eq!(e.kind, AUTHENTICATION_FAILURE, "{}", e.message);
    let good = StompConfig { login: Some("admin".into()), passcode: Some("secret".into()), heartbeat_send: 50, heartbeat_receive: 50, ..cfg(&broker) };
    let c = StompConnection::open(&good).unwrap();
    let tmp = c.subscribe("/temp-queue/replies-1", AckMode::Auto, &[]).unwrap();
    let before = broker.frames_received();
    std::thread::sleep(Duration::from_millis(700));
    assert!(c.is_open(), "heart-beats keep the connection alive");
    c.send("/temp-queue/replies-1", &[], b"r", None, true).unwrap();
    assert_eq!(recv(&tmp).body_string(), "r");
    assert!(broker.frames_received() > before);
    c.unsubscribe(&tmp.id).unwrap();
    c.send("/temp-queue/replies-1", &[], b"queued", None, true).unwrap();
    assert_eq!(broker.queue_depth("/temp-queue/replies-1"), 1);
    c.disconnect();
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(broker.queue_depth("/temp-queue/replies-1"), 0, "temp queues die with their connection");
    // a dead broker is detected
    let c = StompConnection::open(&good).unwrap();
    broker.drop_connections();
    let start = std::time::Instant::now();
    while c.is_open() && start.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!c.is_open());
}

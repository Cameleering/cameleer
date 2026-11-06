use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use cameleer_core::prelude::*;
use cameleer_kafka_mqtt::kafka::client::{ClientConfig, KafkaClient};
use cameleer_kafka_mqtt::kafka::protocol::partition_for_key;
use cameleer_kafka_mqtt::testing::KafkaBroker;

const WAIT: Duration = Duration::from_secs(10);
/// Fast group coordination for tests.
const FAST: &str = "pollTimeoutMs=100&heartbeatIntervalMs=50&fetchWaitMaxMs=100";

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_kafka_mqtt::register(&ctx);
    ctx
}

fn int(ex: &Exchange, h: &str) -> i64 {
    ex.header(h).and_then(|v| v.as_i64()).unwrap_or_else(|| panic!("missing header {}", h))
}

fn text(ex: &Exchange, h: &str) -> String {
    ex.header(h).map(|v| v.to_string()).unwrap_or_default()
}

#[test]
fn produce_and_consume_with_keys_across_partitions() {
    let broker = KafkaBroker::start().unwrap();
    broker.create_topic("orders", 3);
    let b = broker.bootstrap();
    let ctx = context();
    ctx.add_routes(|r| {
        r.from(&format!("kafka:orders?brokers={}&groupId=g1&autoOffsetReset=earliest&{}", b, FAST)).to("mock:orders");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:orders").unwrap();
    mock.expected_message_count(12);

    let t = ctx.create_producer_template();
    let uri = format!("kafka:orders?brokers={}", b);
    let mut produced: HashMap<String, i64> = HashMap::new();
    for i in 0..12 {
        let key = format!("key-{}", i % 6);
        let ex = t.send_with(&uri, ExchangePattern::InOnly, |ex| {
            ex.set_body(format!("order-{}", i));
            ex.set_header("kafka.KEY", key.clone());
            ex.set_header("tenant", "acme");
            ex.set_header("CamelSecret", "filtered");
        });
        assert!(ex.exception.is_none(), "{:?}", ex.exception);
        let partition = int(&ex, "kafka.PARTITION");
        assert_eq!(partition, partition_for_key(key.as_bytes(), 3) as i64, "murmur2 partitioner");
        // the same key always lands on the same partition
        if let Some(p) = produced.insert(key.clone(), partition) {
            assert_eq!(p, partition);
        }
        let meta = ex.header("kafka.RECORD_META").unwrap().as_array().unwrap().clone();
        assert_eq!(meta.len(), 1);
        assert_eq!(meta[0].get("topic").unwrap().to_string(), "orders");
        assert_eq!(meta[0].get("offset").unwrap().as_i64(), Some(int(&ex, "kafka.OFFSET")));
    }
    mock.assert_is_satisfied_within(WAIT).unwrap();
    let mut per_partition: HashMap<i64, Vec<i64>> = HashMap::new();
    for ex in mock.received_exchanges() {
        assert_eq!(text(&ex, "kafka.TOPIC"), "orders");
        let key = text(&ex, "kafka.KEY");
        let p = int(&ex, "kafka.PARTITION");
        assert_eq!(produced[&key], p);
        assert!(int(&ex, "kafka.TIMESTAMP") > 0);
        assert_eq!(ex.header("tenant").unwrap(), &Value::Bytes(b"acme".to_vec()));
        assert!(ex.header("CamelSecret").is_none(), "Camel headers are not propagated");
        per_partition.entry(p).or_default().push(int(&ex, "kafka.OFFSET"));
    }
    // offsets are consecutive and in order within each partition
    let total: usize = per_partition.values().map(|v| v.len()).sum();
    assert_eq!(total, 12);
    for (p, offsets) in &per_partition {
        let expected: Vec<i64> = (0..offsets.len() as i64).collect();
        assert_eq!(offsets, &expected, "partition {}", p);
        assert_eq!(broker.records("orders", *p as i32).len(), offsets.len());
    }
    assert!(per_partition.len() >= 2, "keys should spread over partitions");
    ctx.stop().unwrap();
    // committed on stop
    for (p, offsets) in &per_partition {
        assert_eq!(broker.committed_offset("g1", "orders", *p as i32), Some(offsets.len() as i64));
    }
    broker.stop();
}

#[test]
fn producer_options_override_topic_partition_key_and_acks() {
    let broker = KafkaBroker::start_with_partitions(4).unwrap();
    let b = broker.bootstrap();
    let ctx = context();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    // auto-created topic with 4 partitions, explicit partitionKey option
    t.send_body(&format!("kafka:events?brokers={}&partitionKey=2&key=fixed", b), "a").unwrap();
    assert_eq!(broker.partition_count("events"), Some(4));
    let recs = broker.records("events", 2);
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].key.as_deref(), Some(&b"fixed"[..]));
    assert_eq!(recs[0].value.as_deref(), Some(&b"a"[..]));
    // header partition key wins; override topic header redirects (and is removed)
    let ex = t.send_with(&format!("kafka:events?brokers={}&partitionKey=2", b), ExchangePattern::InOnly, |ex| {
        ex.set_body("b");
        ex.set_header("kafka.PARTITION_KEY", 3);
        ex.set_header("kafka.OVERRIDE_TOPIC", "other");
    });
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
    assert!(ex.header("kafka.OVERRIDE_TOPIC").is_none());
    assert_eq!(broker.records("other", 3).len(), 1);
    // acks=0: fire and forget; an array body sends one record per element
    t.send_body(&format!("kafka:events?brokers={}&requestRequiredAcks=0&partitionKey=0", b), Value::Array(vec!["x".into(), "y".into()])).unwrap();
    assert!(broker.wait_until(WAIT, |br| br.records("events", 0).len() == 2));
    // unkeyed records are spread round robin
    for i in 0..8 {
        t.send_body(&format!("kafka:spread?brokers={}", b), format!("m{}", i)).unwrap();
    }
    for p in 0..4 {
        assert_eq!(broker.records("spread", p).len(), 2);
    }
    // SASL needs credentials; unsupported mechanisms are rejected clearly (TLS/SASL: tests/tls.rs)
    let err = ctx.get_endpoint(&format!("kafka:events?brokers={}&securityProtocol=SASL_SSL", b)).err().unwrap();
    assert!(err.message.contains("saslJaasConfig"), "{}", err.message);
    let err = ctx.get_endpoint(&format!("kafka:events?brokers={}&securityProtocol=SASL_PLAINTEXT&saslMechanism=GSSAPI", b)).err().unwrap();
    assert!(err.message.contains("not supported"), "{}", err.message);
    assert!(ctx.get_endpoint("kafka:events?bogusOption=1").is_err());
    ctx.stop().unwrap();
}

#[test]
fn consumer_group_of_two_splits_partitions() {
    let broker = KafkaBroker::start().unwrap();
    broker.create_topic("split", 4);
    let b = broker.bootstrap();
    let ctx = context();
    ctx.add_routes(|r| {
        r.from(&format!("kafka:split?brokers={}&groupId=g2&clientId=a&autoOffsetReset=earliest&{}", b, FAST)).to("mock:a");
        r.from(&format!("kafka:split?brokers={}&groupId=g2&clientId=b&autoOffsetReset=earliest&{}", b, FAST)).to("mock:b");
    })
    .unwrap();
    ctx.start().unwrap();
    // wait for a stable group of two members with two partitions each
    assert!(
        broker.wait_until(WAIT, |br| {
            let a = br.group_assignment("g2");
            br.group_state("g2").as_deref() == Some("Stable")
                && a.len() == 2
                && a.iter().all(|(_, parts)| parts.iter().map(|(_, p)| p.len()).sum::<usize>() == 2)
        }),
        "group did not stabilize: {:?} {:?}",
        broker.group_state("g2"),
        broker.group_assignment("g2")
    );
    let t = ctx.create_producer_template();
    for p in 0..4 {
        for i in 0..3 {
            t.send_body_and_header(&format!("kafka:split?brokers={}", b), format!("p{}-{}", p, i), "kafka.PARTITION_KEY", p).unwrap();
        }
    }
    let (a, bm) = (ctx.mock_endpoint("mock:a").unwrap(), ctx.mock_endpoint("mock:b").unwrap());
    assert!(broker.wait_until(WAIT, |_| a.received_counter() + bm.received_counter() >= 12));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(a.received_counter() + bm.received_counter(), 12, "each record is consumed exactly once");
    let parts = |m: &MockEndpoint| m.received_exchanges().iter().map(|e| int(e, "kafka.PARTITION")).collect::<BTreeSet<i64>>();
    let (pa, pb) = (parts(&a), parts(&bm));
    assert_eq!(pa.len(), 2, "{:?}", pa);
    assert_eq!(pb.len(), 2, "{:?}", pb);
    assert!(pa.is_disjoint(&pb));
    assert_eq!(a.received_counter(), 6);
    ctx.stop().unwrap();
    assert!(broker.wait_until(WAIT, |br| br.group_members("g2").is_empty()), "consumers leave the group on stop");
    for p in 0..4 {
        assert_eq!(broker.committed_offset("g2", "split", p), Some(3));
    }
}

#[test]
fn consumers_count_in_one_route_share_the_partitions() {
    let broker = KafkaBroker::start().unwrap();
    broker.create_topic("shared", 2);
    let b = broker.bootstrap();
    let ctx = context();
    ctx.add_routes(|r| {
        r.from(&format!("kafka:shared?brokers={}&groupId=g5&consumersCount=2&autoOffsetReset=earliest&{}", b, FAST)).to("mock:shared");
    })
    .unwrap();
    ctx.start().unwrap();
    assert!(broker.wait_until(WAIT, |br| br.group_state("g5").as_deref() == Some("Stable") && br.group_members("g5").len() == 2
        && br.group_assignment("g5").iter().all(|(_, a)| a.len() == 1)));
    let mock = ctx.mock_endpoint("mock:shared").unwrap();
    mock.expected_message_count(6);
    let t = ctx.create_producer_template();
    for i in 0..6 {
        t.send_body_and_header(&format!("kafka:shared?brokers={}", b), format!("m{}", i), "kafka.KEY", format!("k{}", i)).unwrap();
    }
    mock.assert_is_satisfied_within(WAIT).unwrap();
    ctx.stop().unwrap();
}

#[test]
fn committed_offsets_survive_a_restart() {
    let broker = KafkaBroker::start().unwrap();
    broker.create_topic("resume", 1);
    let b = broker.bootstrap();
    let uri = format!("kafka:resume?brokers={}&groupId=g3&autoOffsetReset=earliest&{}", b, FAST);
    let t_ctx = context();
    t_ctx.start().unwrap();
    let t = t_ctx.create_producer_template();
    for i in 1..=3 {
        t.send_body(&format!("kafka:resume?brokers={}", b), format!("m{}", i)).unwrap();
    }

    let ctx = context();
    ctx.add_routes(|r| {
        r.from(&uri).to("mock:first");
    })
    .unwrap();
    ctx.start().unwrap();
    let first = ctx.mock_endpoint("mock:first").unwrap();
    first.expected_bodies_received(vec!["m1", "m2", "m3"]);
    first.assert_is_satisfied_within(WAIT).unwrap();
    ctx.stop().unwrap();
    assert_eq!(broker.committed_offset("g3", "resume", 0), Some(3));

    for i in 4..=5 {
        t.send_body(&format!("kafka:resume?brokers={}", b), format!("m{}", i)).unwrap();
    }
    let ctx2 = context();
    ctx2.add_routes(|r| {
        r.from(&uri).to("mock:second");
    })
    .unwrap();
    ctx2.start().unwrap();
    let second = ctx2.mock_endpoint("mock:second").unwrap();
    second.expected_bodies_received(vec!["m4", "m5"]);
    second.assert_is_satisfied_within(WAIT).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(second.received_counter(), 2, "already committed records are not consumed again");
    ctx2.stop().unwrap();
    assert_eq!(broker.committed_offset("g3", "resume", 0), Some(5));
    t_ctx.stop().unwrap();
}

#[test]
fn auto_offset_reset_earliest_versus_latest() {
    let broker = KafkaBroker::start().unwrap();
    broker.create_topic("reset", 1);
    broker.append("reset", 0, None, b"old-1");
    broker.append("reset", 0, None, b"old-2");
    let b = broker.bootstrap();
    let ctx = context();
    ctx.add_routes(|r| {
        r.from(&format!("kafka:reset?brokers={}&groupId=early&clientId=early&autoOffsetReset=earliest&{}", b, FAST)).to("mock:early");
        r.from(&format!("kafka:reset?brokers={}&groupId=late&clientId=late&autoOffsetReset=latest&{}", b, FAST)).to("mock:late");
        // no group: reads every partition, seekTo=beginning ignores any position
        r.from(&format!("kafka:reset?brokers={}&clientId=nogroup&seekTo=beginning&{}", b, FAST)).to("mock:nogroup");
    })
    .unwrap();
    ctx.start().unwrap();
    // a consumer resolves its start position before its first fetch
    assert!(broker.wait_until(WAIT, |br| br.fetch_requests_by_client("late-0") > 0 && br.fetch_requests_by_client("early-0") > 0));
    broker.append("reset", 0, None, b"new");
    let early = ctx.mock_endpoint("mock:early").unwrap();
    early.expected_bodies_received(vec!["old-1", "old-2", "new"]);
    let late = ctx.mock_endpoint("mock:late").unwrap();
    late.expected_bodies_received(vec!["new"]);
    let nogroup = ctx.mock_endpoint("mock:nogroup").unwrap();
    nogroup.expected_bodies_received(vec!["old-1", "old-2", "new"]);
    MockEndpoint::assert_all_satisfied(&[&early, &late, &nogroup]).unwrap();
    assert_eq!(int(&late.received_exchanges()[0], "kafka.OFFSET"), 2);
    ctx.stop().unwrap();
    assert_eq!(broker.committed_offset("late", "reset", 0), Some(3));
}

#[test]
fn topic_pattern_break_on_first_error_and_manual_commit() {
    let broker = KafkaBroker::start().unwrap();
    for t in ["metrics-a", "metrics-b", "logs"] {
        broker.create_topic(t, 1);
        broker.append(t, 0, Some(b"k"), format!("{}-1", t).as_bytes());
    }
    broker.create_topic("work", 1);
    for v in ["w1", "bad", "w3"] {
        broker.append("work", 0, None, v.as_bytes());
    }
    broker.create_topic("manual", 1);
    for v in ["x1", "x2", "x3"] {
        broker.append("manual", 0, None, v.as_bytes());
    }
    let b = broker.bootstrap();
    let failures = Arc::new(AtomicUsize::new(0));
    let f2 = failures.clone();
    let ctx = context();
    ctx.add_routes(|r| {
        r.from(&format!("kafka:metrics-.*?topicIsPattern=true&brokers={}&groupId=pat&autoOffsetReset=earliest&{}", b, FAST)).to("mock:pattern");
        r.from(&format!("kafka:work?brokers={}&groupId=w&autoOffsetReset=earliest&breakOnFirstError=true&{}", b, FAST))
            .process(move |ex: &mut Exchange| {
                if ex.body().to_string() == "bad" && f2.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(CamelError::illegal_state("transient failure"));
                }
                Ok(())
            })
            .to("mock:work");
        r.from(&format!("kafka:manual?brokers={}&groupId=m&autoOffsetReset=earliest&autoCommitEnable=false&allowManualCommit=true&{}", b, FAST))
            .process(|ex: &mut Exchange| {
                if ex.body().to_string() != "x3" {
                    cameleer_kafka_mqtt::kafka::manual_commit(ex)?;
                }
                Ok(())
            })
            .to("mock:manual");
    })
    .unwrap();
    ctx.start().unwrap();
    let pattern = ctx.mock_endpoint("mock:pattern").unwrap();
    pattern.expected_bodies_received_in_any_order(vec!["metrics-a-1", "metrics-b-1"]);
    let work = ctx.mock_endpoint("mock:work").unwrap();
    work.expected_bodies_received(vec!["w1", "bad", "w3"]);
    let manual = ctx.mock_endpoint("mock:manual").unwrap();
    manual.expected_bodies_received(vec!["x1", "x2", "x3"]);
    MockEndpoint::assert_all_satisfied(&[&pattern, &work, &manual]).unwrap();
    assert_eq!(failures.load(Ordering::SeqCst), 2, "the failed record was consumed again");
    assert!(manual.received_exchanges()[0].header("CamelKafkaManualCommit").is_some());
    assert_eq!(manual.received_exchanges()[2].header("kafka.LAST_RECORD_BEFORE_COMMIT"), Some(&Value::Bool(true)));
    assert!(broker.wait_until(WAIT, |br| br.committed_offset("m", "manual", 0) == Some(2)));
    ctx.stop().unwrap();
    assert_eq!(broker.committed_offset("m", "manual", 0), Some(2), "only manually committed offsets are stored");
    assert_eq!(broker.committed_offset("w", "work", 0), Some(3));
}

#[test]
fn client_api_create_topics_and_offsets() {
    let broker = KafkaBroker::start().unwrap();
    let mut c = KafkaClient::new(ClientConfig::new(&broker.bootstrap(), "test-client").unwrap());
    c.create_topics(&[("created".to_string(), 5)]).unwrap();
    c.create_topics(&[("created".to_string(), 5)]).unwrap(); // already exists: fine
    assert_eq!(broker.partition_count("created"), Some(5));
    assert_eq!(c.partitions("created", true).unwrap().len(), 5);
    assert!(c.list_topics().unwrap().contains(&"created".to_string()));
    let rec = cameleer_kafka_mqtt::kafka::protocol::Record { timestamp: 1, value: Some(b"v".to_vec()), ..Default::default() };
    let (base, _) = c.produce("created", 4, &[rec.clone(), rec], -1, 1000).unwrap();
    assert_eq!(base, 0);
    let latest = c.list_offsets("created", &[0, 4], cameleer_kafka_mqtt::kafka::protocol::LATEST_TIMESTAMP).unwrap();
    assert_eq!((latest[&0], latest[&4]), (0, 2));
    // a stopped broker gives a connection error
    broker.stop();
    c.close();
    assert!(c.metadata(None, false).is_err());
}

#[test]
fn concurrent_sends_share_produce_requests_and_keep_their_offsets() {
    let broker = KafkaBroker::start_with_partitions(1).unwrap();
    let ctx = context();
    ctx.start().unwrap();
    let uri = format!("kafka:batched?brokers={}", broker.bootstrap());
    let t = ctx.create_producer_template();
    t.send_body(&uri, "warm").unwrap();
    const SENDERS: usize = 16;
    const EACH: usize = 50;
    let senders: Vec<_> = (0..SENDERS)
        .map(|s| {
            let (t, uri) = (t.clone(), uri.clone());
            std::thread::spawn(move || {
                (0..EACH)
                    .map(|i| {
                        let body = format!("s{s}-{i}");
                        let ex = t.send_with(&uri, ExchangePattern::InOnly, |ex: &mut Exchange| ex.set_body(body.clone()));
                        assert!(ex.exception.is_none(), "{:?}", ex.exception);
                        (body, int(&ex, "kafka.OFFSET"))
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let sent: Vec<Vec<(String, i64)>> = senders.into_iter().map(|h| h.join().unwrap()).collect();
    let records = broker.records("batched", 0);
    assert_eq!(records.len(), 1 + SENDERS * EACH);
    for per_sender in &sent {
        // every exchange's offset holds its own record, and one sender's records keep their order
        for (body, offset) in per_sender {
            assert_eq!(records[*offset as usize].value.as_deref(), Some(body.as_bytes()));
        }
        assert!(per_sender.windows(2).all(|w| w[0].1 < w[1].1));
    }
    // sends that waited for the client went out together instead of one request each
    let batches = broker.batch_compression("batched", 0).len();
    assert!(batches < 1 + SENDERS * EACH, "{batches} batches for {} records", 1 + SENDERS * EACH);
    ctx.stop().unwrap();
}

//! Third-party simulator: the `kafka` component against kfake, the fake Kafka cluster of the
//! franz-go client (`scripts/simulators.sh`: three plaintext brokers on 19092-19094, SASL on
//! 19095, TLS on 19096, SASL_SSL on 19097). Runs only with `CAMEL_SIMULATORS=1`.

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use cameleer_core::prelude::*;
use cameleer_kafka_mqtt::kafka::client::{ClientConfig, KafkaClient};
use cameleer_kafka_mqtt::kafka::protocol::{partition_for_key, LATEST_TIMESTAMP};
use cameleer_tls::testing::{live, sim};

const BROKERS: &str = "127.0.0.1:19092,127.0.0.1:19093,127.0.0.1:19094";
const WAIT: Duration = Duration::from_secs(30);
const FAST: &str = "pollTimeoutMs=200&heartbeatIntervalMs=100&fetchWaitMaxMs=200&sessionTimeoutMs=10000";

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_kafka_mqtt::register(&ctx);
    live::bind_beans(&ctx);
    ctx
}

fn int(ex: &Exchange, h: &str) -> i64 {
    ex.header(h).and_then(|v| v.as_i64()).unwrap_or_else(|| panic!("missing header {}", h))
}

fn text(ex: &Exchange, h: &str) -> String {
    ex.header(h).map(|v| v.to_string()).unwrap_or_default()
}

fn client() -> KafkaClient {
    KafkaClient::new(ClientConfig::new(BROKERS, "sim-admin").unwrap())
}

fn create(topic: &str, partitions: i32) {
    client().create_topics(&[(topic.to_string(), partitions)]).unwrap();
}

fn committed(group: &str, topic: &str, partitions: &[i32]) -> HashMap<i32, i64> {
    let mut c = client();
    let coord = c.find_coordinator(group).unwrap();
    c.offset_fetch(coord, group, &[(topic.to_string(), partitions.to_vec())]).unwrap().into_iter().map(|((_, p), o)| (p, o)).collect()
}

fn wait_for(f: impl Fn() -> bool) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < WAIT {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

#[test]
fn sim_kafka_produce_consume_across_brokers_and_partitions() {
    if !sim::enabled() {
        return;
    }
    let topic = sim::unique("orders");
    create(&topic, 3);
    let mut admin = client();
    let leaders = admin.leaders(&topic).unwrap();
    assert_eq!(leaders.len(), 3);
    let group = sim::unique("g");
    let ctx = context();
    ctx.add_routes(|r| {
        r.from(&format!("kafka:{}?brokers={}&groupId={}&autoOffsetReset=earliest&{}", topic, BROKERS, group, FAST)).to("mock:orders");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:orders").unwrap();
    mock.expected_message_count(12);
    let t = ctx.create_producer_template();
    let uri = format!("kafka:{}?brokers={}", topic, BROKERS);
    let mut produced: HashMap<String, i64> = HashMap::new();
    for i in 0..12 {
        let key = format!("key-{}", i % 6);
        let ex = t.send_with(&uri, ExchangePattern::InOnly, |ex| {
            ex.set_body(format!("order-{}", i));
            ex.set_header("kafka.KEY", key.clone());
            ex.set_header("tenant", "acme");
            ex.set_header("attempt", 3i64);
            ex.set_header("CamelSecret", "filtered");
        });
        assert!(ex.exception.is_none(), "{:?}", ex.exception);
        let p = int(&ex, "kafka.PARTITION");
        assert_eq!(p, partition_for_key(key.as_bytes(), 3) as i64, "murmur2 partitioner");
        if let Some(prev) = produced.insert(key, p) {
            assert_eq!(prev, p);
        }
    }
    mock.assert_is_satisfied_within(WAIT).unwrap();
    let mut per_partition: HashMap<i64, Vec<i64>> = HashMap::new();
    for ex in mock.received_exchanges() {
        assert_eq!(text(&ex, "kafka.TOPIC"), topic);
        assert_eq!(produced[&text(&ex, "kafka.KEY")], int(&ex, "kafka.PARTITION"));
        assert!(int(&ex, "kafka.TIMESTAMP") > 0);
        assert_eq!(ex.header("tenant").unwrap(), &Value::Bytes(b"acme".to_vec()));
        assert_eq!(ex.header("attempt").unwrap(), &Value::Bytes(3i64.to_be_bytes().to_vec()));
        assert!(ex.header("CamelSecret").is_none());
        per_partition.entry(int(&ex, "kafka.PARTITION")).or_default().push(int(&ex, "kafka.OFFSET"));
    }
    for offsets in per_partition.values() {
        assert_eq!(offsets, &(0..offsets.len() as i64).collect::<Vec<_>>());
    }
    ctx.stop().unwrap();
    // committed on stop, as kfake reports them
    let c = committed(&group, &topic, &[0, 1, 2]);
    for (p, offsets) in &per_partition {
        assert_eq!(c.get(&(*p as i32)), Some(&(offsets.len() as i64)), "partition {}", p);
    }
    let latest = admin.list_offsets(&topic, &[0, 1, 2], LATEST_TIMESTAMP).unwrap();
    assert_eq!(latest.values().sum::<i64>(), 12);
}

#[test]
fn sim_kafka_producer_options_and_auto_created_topics() {
    if !sim::enabled() {
        return;
    }
    let ctx = context();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let events = sim::unique("events");
    let other = sim::unique("other");
    // auto-created topic (kfake's default: 3 partitions), explicit partition and key
    let ex = t.send_with(&format!("kafka:{}?brokers={}&partitionKey=2&key=fixed", events, BROKERS), ExchangePattern::InOnly, |ex| ex.set_body("a"));
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
    assert_eq!(int(&ex, "kafka.PARTITION"), 2);
    let mut admin = client();
    assert_eq!(admin.partitions(&events, true).unwrap().len(), 3);
    // header partition wins, override topic redirects and is removed
    let ex = t.send_with(&format!("kafka:{}?brokers={}&partitionKey=2", events, BROKERS), ExchangePattern::InOnly, |ex| {
        ex.set_body("b");
        ex.set_header("kafka.PARTITION_KEY", 1);
        ex.set_header("kafka.OVERRIDE_TOPIC", other.clone());
    });
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
    assert!(ex.header("kafka.OVERRIDE_TOPIC").is_none());
    assert_eq!(ex.header("kafka.RECORD_META").unwrap().as_array().unwrap()[0].get("topic").unwrap().to_string(), other);
    assert_eq!(admin.list_offsets(&other, &[1], LATEST_TIMESTAMP).unwrap()[&1], 1);
    // acks=0 and acks=1, array bodies (one record each), unkeyed round robin
    t.send_body(&format!("kafka:{}?brokers={}&requestRequiredAcks=0&partitionKey=0", events, BROKERS), Value::Array(vec!["x".into(), "y".into()])).unwrap();
    assert!(wait_for(|| client().list_offsets(&events, &[0], LATEST_TIMESTAMP).unwrap()[&0] == 2));
    let spread = sim::unique("spread");
    create(&spread, 3);
    for i in 0..6 {
        t.send_body(&format!("kafka:{}?brokers={}&requestRequiredAcks=1", spread, BROKERS), format!("m{}", i)).unwrap();
    }
    let offs = admin.list_offsets(&spread, &[0, 1, 2], LATEST_TIMESTAMP).unwrap();
    assert_eq!((offs[&0], offs[&1], offs[&2]), (2, 2, 2));
    // compression codecs understood by kfake (franz-go's decoders)
    for codec in ["gzip", "snappy", "lz4"] {
        let topic = sim::unique(codec);
        let ctx2 = context();
        ctx2.add_routes(|r| {
            r.from(&format!("kafka:{}?brokers={}&autoOffsetReset=earliest&{}", topic, BROKERS, FAST)).to("mock:c");
        })
        .unwrap();
        create(&topic, 1);
        ctx2.start().unwrap();
        let mock = ctx2.mock_endpoint("mock:c").unwrap();
        mock.expected_bodies_received(vec!["one", "two"]);
        t.send_body(&format!("kafka:{}?brokers={}&compressionCodec={}", topic, BROKERS, codec), Value::Array(vec!["one".into(), "two".into()])).unwrap();
        mock.assert_is_satisfied_within(WAIT).unwrap_or_else(|e| panic!("{}: {}", codec, e));
        ctx2.stop().unwrap();
    }
    ctx.stop().unwrap();
}

#[test]
fn sim_kafka_consumer_group_rebalances_between_routes() {
    if !sim::enabled() {
        return;
    }
    let topic = sim::unique("split");
    create(&topic, 4);
    let group = sim::unique("grp");
    let uri = format!("kafka:{}?brokers={}&groupId={}&autoOffsetReset=earliest&{}", topic, BROKERS, group, FAST);
    let a = context();
    a.add_routes(|r| {
        r.from(&uri).to("mock:a");
    })
    .unwrap();
    a.start().unwrap();
    let b = context();
    b.add_routes(|r| {
        r.from(&uri).to("mock:b");
    })
    .unwrap();
    b.start().unwrap();
    let ma = a.mock_endpoint("mock:a").unwrap();
    let mb = b.mock_endpoint("mock:b").unwrap();
    // let the group settle with both members
    std::thread::sleep(Duration::from_secs(3));
    let t = a.create_producer_template();
    for p in 0..4 {
        for i in 0..3 {
            t.send_body_and_header(&format!("kafka:{}?brokers={}", topic, BROKERS), format!("p{}-{}", p, i), "kafka.PARTITION_KEY", p).unwrap();
        }
    }
    assert!(wait_for(|| ma.received_counter() + mb.received_counter() == 12), "{} + {}", ma.received_counter(), mb.received_counter());
    let parts = |m: &MockEndpoint| m.received_exchanges().iter().map(|e| int(e, "kafka.PARTITION")).collect::<BTreeSet<_>>();
    let (pa, pb) = (parts(&ma), parts(&mb));
    assert!(!pa.is_empty() && !pb.is_empty(), "both members got partitions: {:?} {:?}", pa, pb);
    assert!(pa.is_disjoint(&pb), "{:?} {:?}", pa, pb);
    // one member leaves: the other takes over every partition
    b.stop().unwrap();
    std::thread::sleep(Duration::from_secs(2));
    for p in 0..4 {
        t.send_body_and_header(&format!("kafka:{}?brokers={}", topic, BROKERS), format!("late-{}", p), "kafka.PARTITION_KEY", p).unwrap();
    }
    assert!(wait_for(|| ma.received_exchanges().iter().filter(|e| e.body_as::<String>().unwrap().starts_with("late-")).count() == 4));
    a.stop().unwrap();
    let c = committed(&group, &topic, &[0, 1, 2, 3]);
    assert_eq!(c.values().sum::<i64>(), 16, "{:?}", c);
}

#[test]
fn sim_kafka_committed_offsets_survive_restart_and_manual_commit() {
    if !sim::enabled() {
        return;
    }
    let topic = sim::unique("resume");
    create(&topic, 1);
    let group = sim::unique("g3");
    let uri = format!("kafka:{}?brokers={}&groupId={}&autoOffsetReset=earliest&{}", topic, BROKERS, group, FAST);
    let tctx = context();
    tctx.start().unwrap();
    let t = tctx.create_producer_template();
    for i in 1..=3 {
        t.send_body(&format!("kafka:{}?brokers={}", topic, BROKERS), format!("m{}", i)).unwrap();
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
    assert_eq!(committed(&group, &topic, &[0]).get(&0), Some(&3));
    for i in 4..=5 {
        t.send_body(&format!("kafka:{}?brokers={}", topic, BROKERS), format!("m{}", i)).unwrap();
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
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(second.received_counter(), 2);
    ctx2.stop().unwrap();
    assert_eq!(committed(&group, &topic, &[0]).get(&0), Some(&5));

    // manual commit: only the explicitly committed offset is stored
    let manual = sim::unique("manual");
    create(&manual, 1);
    let mgroup = sim::unique("m");
    let ctx3 = context();
    ctx3.add_routes(|r| {
        r.from(&format!("kafka:{}?brokers={}&groupId={}&autoOffsetReset=earliest&allowManualCommit=true&autoCommitEnable=false&autoCommitOnStop=none&{}", manual, BROKERS, mgroup, FAST))
            .process(|ex: &mut Exchange| {
                if ex.body_as::<String>()? == "c2" {
                    cameleer_kafka_mqtt::kafka::manual_commit(ex)?;
                }
                Ok(())
            })
            .to("mock:manual");
    })
    .unwrap();
    ctx3.start().unwrap();
    let mm = ctx3.mock_endpoint("mock:manual").unwrap();
    mm.expected_message_count(3);
    for i in 1..=3 {
        t.send_body(&format!("kafka:{}?brokers={}", manual, BROKERS), format!("c{}", i)).unwrap();
    }
    mm.assert_is_satisfied_within(WAIT).unwrap();
    assert!(wait_for(|| committed(&mgroup, &manual, &[0]).get(&0) == Some(&2)));
    ctx3.stop().unwrap();
    assert_eq!(committed(&mgroup, &manual, &[0]).get(&0), Some(&2));
    tctx.stop().unwrap();
}

fn secured_round_trip(port: u16, security: &str, extra: &str) {
    let topic = sim::unique("secure");
    let ctx = context();
    let base = format!("brokers=127.0.0.1:{}&securityProtocol={}{}", port, security, extra);
    ctx.add_routes(|r| {
        r.from(&format!("kafka:{}?{}&groupId={}&autoOffsetReset=earliest&{}", topic, base, sim::unique("sg"), FAST)).to("mock:secure");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:secure").unwrap();
    mock.expected_bodies_received(vec!["s1", "s2"]);
    let t = ctx.create_producer_template();
    t.send_body(&format!("kafka:{}?{}", topic, base), "s1").unwrap_or_else(|e| panic!("{} {}: {}", security, extra, e));
    t.send_body(&format!("kafka:{}?{}", topic, base), "s2").unwrap();
    mock.assert_is_satisfied_within(WAIT).unwrap_or_else(|e| panic!("{} {}: {}", security, extra, e));
    ctx.stop().unwrap();
}

fn jaas(user: &str, pass: &str, scram: bool) -> String {
    let module = if scram { "org.apache.kafka.common.security.scram.ScramLoginModule" } else { "org.apache.kafka.common.security.plain.PlainLoginModule" };
    format!("RAW({} required username=\"{}\" password=\"{}\";)", module, user, pass)
}

#[test]
fn sim_kafka_sasl_and_tls() {
    if !sim::enabled() {
        return;
    }
    for mech in ["PLAIN", "SCRAM-SHA-256", "SCRAM-SHA-512"] {
        let j = jaas("camel", sim::PASSWORD, mech != "PLAIN");
        secured_round_trip(19095, "SASL_PLAINTEXT", &format!("&saslMechanism={}&saslJaasConfig={}", mech, j));
        secured_round_trip(19097, "SASL_SSL", &format!("&saslMechanism={}&saslJaasConfig={}&sslContextParameters=#liveSsl", mech, j));
    }
    secured_round_trip(19096, "SSL", "&sslContextParameters=#liveSsl");
    secured_round_trip(19096, "SSL", &format!("&sslTruststoreLocation={}", live::ca_file().display()));

    // wrong passwords and untrusted certificates are refused
    let ctx = context();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for mech in ["PLAIN", "SCRAM-SHA-256"] {
        let uri = format!("kafka:{}?brokers=127.0.0.1:19095&securityProtocol=SASL_PLAINTEXT&saslMechanism={}&saslJaasConfig={}&requestTimeoutMs=5000", sim::unique("x"), mech, jaas("camel", "wrong", mech != "PLAIN"));
        let e = t.send_body(&uri, "x").unwrap_err();
        assert!(e.root_cause().message.to_ascii_lowercase().contains("auth") || e.root_cause().message.contains("SASL"), "{}: {}", mech, e);
    }
    let e = t.send_body(&format!("kafka:{}?brokers=127.0.0.1:19096&securityProtocol=SSL&sslContextParameters=#liveUntrusted&requestTimeoutMs=5000", sim::unique("x")), "x").unwrap_err();
    assert!(format!("{}", e).to_ascii_lowercase().contains("certificate") || format!("{}", e).contains("TLS"), "{}", e);
    // plaintext against the SASL listener is refused by kfake
    let e = t.send_body(&format!("kafka:{}?brokers=127.0.0.1:19095&requestTimeoutMs=5000", sim::unique("x")), "x");
    assert!(e.is_err(), "unauthenticated produce must fail");
    ctx.stop().unwrap();
}

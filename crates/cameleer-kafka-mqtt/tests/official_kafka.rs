//! Official-client cross-verification of the embedded Kafka broker and the `kafka`
//! component: librdkafka (through confluent-kafka-python and kcat) producing and consuming
//! against Cameleer, with compression, headers, consumer groups, SASL and TLS.
//! Runs only with `CAMEL_OFFICIAL_CLIENTS=1` (see `scripts/official-clients.sh`).

use std::collections::BTreeSet;
use std::process::Command;
use std::time::Duration;

use cameleer_core::prelude::*;
use cameleer_kafka_mqtt::kafka::protocol::partition_for_key;
use cameleer_kafka_mqtt::testing::{KafkaBroker, KafkaBrokerConfig};
use cameleer_tls::testing::official::{self, python_script, Run};

const WAIT: Duration = Duration::from_secs(30);
const FAST: &str = "pollTimeoutMs=100&heartbeatIntervalMs=200&fetchWaitMaxMs=100&autoOffsetReset=earliest";

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_kafka_mqtt::register(&ctx);
    ctx
}

fn librdkafka(bootstrap: &str, args: &[&str], env: &[(&str, &str)]) -> Run {
    let mut all = vec![bootstrap];
    all.extend_from_slice(args);
    python_script("kafka_confluent.py", &all, env)
}

fn bytes(ex: &Exchange, h: &str) -> Vec<u8> {
    match ex.header(h) {
        Some(Value::Bytes(b)) => b.clone(),
        Some(v) => v.to_string().into_bytes(),
        None => panic!("missing header {}", h),
    }
}

fn kcat(args: &[&str], stdin: Option<&[u8]>) -> Run {
    let mut cmd = Command::new("kcat");
    cmd.args(args);
    official::run(&mut cmd, stdin, Duration::from_secs(60))
}

/// librdkafka compresses record batches (gzip, snappy, lz4); the broker stores them verbatim
/// and the Cameleer consumer decodes librdkafka's own bytes, headers included.
#[test]
fn librdkafka_compressed_batches_decoded_by_rust_camel() {
    if !official::enabled() {
        return;
    }
    let broker = KafkaBroker::start().unwrap();
    let b = broker.bootstrap();
    let codecs = ["none", "gzip", "snappy", "lz4"];
    for codec in codecs {
        let topic = format!("lib-{}", codec);
        broker.create_topic(&topic, 2);
        let run = librdkafka(&b, &["produce", &topic, "8", codec], &[]).ok();
        assert_eq!(run.value("delivered:"), "8");
        let mut seen = Vec::new();
        for p in 0..2 {
            seen.extend(broker.batch_compression(&topic, p));
        }
        assert!(!seen.is_empty());
        assert!(seen.iter().all(|c| *c == codec), "{} batches stored as {:?}", codec, seen);
    }
    let ctx = context();
    ctx.add_routes(|r| {
        for codec in codecs {
            r.from(&format!("kafka:lib-{}?brokers={}&groupId=camel-{}&{}", codec, b, codec, FAST)).to("mock:lib");
        }
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:lib").unwrap();
    mock.expected_message_count(32);
    mock.assert_is_satisfied_within(WAIT).unwrap();
    let mut per_codec: std::collections::HashMap<String, BTreeSet<String>> = Default::default();
    for ex in mock.received_exchanges() {
        let codec = String::from_utf8(bytes(&ex, "codec")).unwrap();
        let index = String::from_utf8(bytes(&ex, "index")).unwrap();
        assert_eq!(bytes(&ex, "empty"), Vec::<u8>::new());
        assert_eq!(bytes(&ex, "bin"), vec![0, 1, 255]);
        let body = ex.body().to_string();
        assert!(body.starts_with(&format!("msg-{}-{} ", codec, index)), "{}", body);
        assert_eq!(body.len(), 20 * format!("msg-{}-{} ", codec, index).len());
        let i: usize = index.parse().unwrap();
        assert_eq!(ex.header("kafka.KEY").unwrap().to_string(), format!("key-{}", i % 4));
        per_codec.entry(codec).or_default().insert(index);
    }
    for codec in codecs {
        assert_eq!(per_codec[codec].len(), 8, "{}", codec);
    }
    ctx.stop().unwrap();
}

/// zstd needs Produce v7 / Fetch v10 (the broker speaks v3 / v4): librdkafka sees that from
/// ApiVersions and falls back to uncompressed batches.
#[test]
fn librdkafka_zstd_falls_back_to_uncompressed() {
    if !official::enabled() {
        return;
    }
    let broker = KafkaBroker::start().unwrap();
    broker.create_topic("zstd", 1);
    librdkafka(&broker.bootstrap(), &["produce", "zstd", "3", "zstd"], &[]).ok();
    let codecs = broker.batch_compression("zstd", 0);
    assert!(!codecs.is_empty() && codecs.iter().all(|c| *c == "none"), "{:?}", codecs);
    assert_eq!(broker.records("zstd", 0).len(), 3);
}

/// Cameleer's producer compresses (`compressionCodec`) and librdkafka consumes it in a
/// consumer group, committing offsets the broker records.
#[test]
fn rust_camel_compressed_batches_decoded_by_librdkafka() {
    if !official::enabled() {
        return;
    }
    let broker = KafkaBroker::start().unwrap();
    let b = broker.bootstrap();
    let ctx = context();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for codec in ["none", "gzip", "snappy", "lz4"] {
        let topic = format!("camel-{}", codec);
        broker.create_topic(&topic, 1);
        let uri = format!("kafka:{}?brokers={}&compressionCodec={}", topic, b, codec);
        for i in 0..5 {
            let ex = t.send_with(&uri, ExchangePattern::InOnly, |ex| {
                ex.set_body(format!("camel-{}-{} {}", codec, i, "payload ".repeat(50)));
                ex.set_header("kafka.KEY", format!("key-{}", i));
                ex.set_header("codec", codec);
                ex.set_header("index", i.to_string());
            });
            assert!(ex.exception.is_none(), "{:?}", ex.exception);
        }
        assert!(broker.batch_compression(&topic, 0).iter().all(|c| *c == codec));
        let run = librdkafka(&b, &["consume", &topic, &format!("lib-{}", codec), "5"], &[]).ok();
        let records = run.values("record:");
        assert_eq!(records.len(), 5, "{}", run.stdout);
        for (i, r) in records.iter().enumerate() {
            let f: Vec<&str> = r.split(' ').collect();
            assert_eq!(f[1], i.to_string(), "offset");
            assert_eq!(f[2], format!("key-{}", i));
            assert_eq!(f[3], format!("camel-{}-{}", codec, i));
            let hex = |s: &str| s.bytes().map(|b| format!("{:02x}", b)).collect::<String>();
            assert!(f[4].contains(&format!("codec={}", hex(codec))), "{}", r);
            assert!(f[4].contains(&format!("index={}", hex(&i.to_string()))), "{}", r);
        }
        assert_eq!(run.value("committed:"), "0 5");
        assert_eq!(broker.committed_offset(&format!("lib-{}", codec), &topic, 0), Some(5));
    }
    // zstd is refused with a clear error rather than sent uncompressed
    let ex = t.send_with(&format!("kafka:camel-none?brokers={}&compressionCodec=zstd", b), ExchangePattern::InOnly, |ex| ex.set_body("x"));
    assert!(ex.exception.is_some());
    ctx.stop().unwrap();
}

/// librdkafka's `murmur2_random` partitioner (the Java client's default) places keys on the
/// same partitions as Cameleer's partitioner.
#[test]
fn murmur2_partitioning_agrees_with_librdkafka() {
    if !official::enabled() {
        return;
    }
    let broker = KafkaBroker::start().unwrap();
    broker.create_topic("murmur", 5);
    librdkafka(&broker.bootstrap(), &["produce", "murmur", "40", "none"], &[("KAFKA_PARTITIONER", "murmur2_random")]).ok();
    let mut total = 0;
    for p in 0..5 {
        for r in broker.records("murmur", p) {
            let key = r.key.unwrap();
            assert_eq!(partition_for_key(&key, 5), p, "key {}", String::from_utf8_lossy(&key));
            total += 1;
        }
    }
    assert_eq!(total, 40);
}

/// A librdkafka consumer and a Cameleer consumer share one consumer group (JoinGroup /
/// SyncGroup with the `range` assignor's subscription and assignment encodings).
#[test]
fn mixed_consumer_group_with_librdkafka() {
    if !official::enabled() {
        return;
    }
    let broker = KafkaBroker::start().unwrap();
    broker.create_topic("mixed", 4);
    let b = broker.bootstrap();
    let ctx = context();
    ctx.add_routes(|r| {
        r.from(&format!("kafka:mixed?brokers={}&groupId=mixed&sessionTimeoutMs=6000&{}", b, FAST)).to("mock:camel");
    })
    .unwrap();
    ctx.start().unwrap();
    assert!(broker.wait_until(WAIT, |b| b.group_state("mixed").as_deref() == Some("Stable")));
    let run = librdkafka(&b, &["group", "mixed", "mixed", "40", "6"], &[]).ok();
    let assigned: BTreeSet<i32> = run.value("assigned:").split(',').map(|p| p.parse().unwrap()).collect();
    assert_eq!(assigned.len(), 2, "range assignor splits 4 partitions over 2 members: {}", run.stdout);
    let lib_count: usize = run.value("received:").parse().unwrap();
    assert_eq!(lib_count, 20);
    let mock = ctx.mock_endpoint("mock:camel").unwrap();
    mock.expected_minimum_message_count(20);
    mock.assert_is_satisfied_within(WAIT).unwrap();
    let camel_parts: BTreeSet<i32> = mock.received_exchanges().iter().map(|ex| ex.header("kafka.PARTITION").unwrap().as_i64().unwrap() as i32).collect();
    assert!(camel_parts.is_disjoint(&assigned), "camel {:?} vs librdkafka {:?}", camel_parts, assigned);
    assert_eq!(camel_parts.len() + assigned.len(), 4);
    // both members committed their partitions
    for p in 0..4 {
        assert_eq!(broker.committed_offset("mixed", "mixed", p).unwrap_or(-1) >= 0, true, "partition {}", p);
    }
    ctx.stop().unwrap();
}

/// librdkafka's AdminClient creates topics and reads cluster metadata.
#[test]
fn librdkafka_admin_client() {
    if !official::enabled() {
        return;
    }
    let broker = KafkaBroker::start().unwrap();
    let run = librdkafka(&broker.bootstrap(), &["admin", "admin-topic", "6"], &[]).ok();
    assert_eq!(run.value("created:"), "admin-topic");
    assert_eq!(run.value("partitions:"), "6");
    assert_eq!(broker.partition_count("admin-topic"), Some(6));
}

/// kcat (librdkafka's CLI) producing keyed records with headers (snappy) for Cameleer, and
/// consuming what Cameleer produced (lz4), in plain and group mode; metadata listing.
#[test]
fn kcat_round_trip() {
    if !official::enabled() {
        return;
    }
    let broker = KafkaBroker::start().unwrap();
    broker.create_topic("kcat-in", 1);
    broker.create_topic("kcat-out", 1);
    let b = broker.bootstrap();
    let input = b"k1:first line\nk2:second line\nk3:third line\n";
    kcat(&["-P", "-b", &b, "-t", "kcat-in", "-K", ":", "-H", "source=kcat", "-H", "trace=abc", "-z", "snappy", "-X", "linger.ms=100"], Some(input)).ok();
    assert!(broker.batch_compression("kcat-in", 0).iter().all(|c| *c == "snappy"));
    let ctx = context();
    ctx.add_routes(|r| {
        r.from(&format!("kafka:kcat-in?brokers={}&groupId=camel&{}", b, FAST)).to("mock:kcat");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:kcat").unwrap();
    mock.expected_bodies_received(vec!["first line", "second line", "third line"]);
    mock.assert_is_satisfied_within(WAIT).unwrap();
    let ex = &mock.received_exchanges()[1];
    assert_eq!(ex.header("kafka.KEY").unwrap().to_string(), "k2");
    assert_eq!(bytes(ex, "source"), b"kcat");
    assert_eq!(bytes(ex, "trace"), b"abc");

    let t = ctx.create_producer_template();
    for i in 0..3 {
        t.send_with(&format!("kafka:kcat-out?brokers={}&compressionCodec=lz4", b), ExchangePattern::InOnly, |ex| {
            ex.set_body(format!("from camel {}", i));
            ex.set_header("kafka.KEY", format!("ck{}", i));
            ex.set_header("origin", "Cameleer");
        });
    }
    let run = kcat(&["-C", "-b", &b, "-t", "kcat-out", "-o", "beginning", "-e", "-f", "%k|%s|%h|%p|%o\\n"], None).ok();
    let lines: Vec<&str> = run.stdout.lines().filter(|l| l.contains('|')).collect();
    assert_eq!(lines, vec!["ck0|from camel 0|origin=Cameleer|0|0", "ck1|from camel 1|origin=Cameleer|0|1", "ck2|from camel 2|origin=Cameleer|0|2"]);
    // balanced consumer group mode
    let run = kcat(&["-G", "kcat-group", "-b", &b, "-o", "beginning", "-e", "-f", "%s\\n", "-X", "auto.offset.reset=earliest", "kcat-out"], None).ok();
    assert_eq!(run.stdout.lines().filter(|l| l.starts_with("from camel")).count(), 3, "{}", run.stderr);
    assert!(broker.wait_until(WAIT, |b| b.committed_offset("kcat-group", "kcat-out", 0) == Some(3)));
    let run = kcat(&["-L", "-b", &b], None).ok();
    assert!(run.stdout.contains("topic \"kcat-out\" with 1 partitions"), "{}", run.stdout);
    ctx.stop().unwrap();
}

/// SASL PLAIN / SCRAM-SHA-256 / SCRAM-SHA-512 over TLS (verified against the CA and the
/// `localhost` name), wrong passwords, and SASL_PLAINTEXT, with librdkafka.
#[test]
fn librdkafka_sasl_and_tls() {
    if !official::enabled() {
        return;
    }
    let pem = official::pem_files("kafka-sasl");
    let ca = pem.ca.display().to_string();
    let users = vec![("alice".to_string(), "alice-secret".to_string())];
    let tls = official::server_params().server_config().unwrap();
    let broker = KafkaBroker::start_with(KafkaBrokerConfig { tls: Some(tls), sasl_users: users.clone(), ..Default::default() }).unwrap();
    broker.create_topic("secure", 1);
    let b = format!("localhost:{}", broker.port());
    for mech in ["PLAIN", "SCRAM-SHA-256", "SCRAM-SHA-512"] {
        let env = [("KAFKA_SECURITY_PROTOCOL", "SASL_SSL"), ("KAFKA_CA", ca.as_str()), ("KAFKA_SASL_MECHANISM", mech), ("KAFKA_SASL_USERNAME", "alice"), ("KAFKA_SASL_PASSWORD", "alice-secret")];
        let run = librdkafka(&b, &["produce", "secure", "2", "gzip"], &env).ok();
        assert_eq!(run.value("delivered:"), "2", "{}", mech);
        let bad = [("KAFKA_SECURITY_PROTOCOL", "SASL_SSL"), ("KAFKA_CA", ca.as_str()), ("KAFKA_SASL_MECHANISM", mech), ("KAFKA_SASL_USERNAME", "alice"), ("KAFKA_SASL_PASSWORD", "wrong")];
        let run = librdkafka(&b, &["auth-error"], &bad).ok();
        assert!(run.value("auth-error:").contains("_AUTHENTICATION"), "{}: {}", mech, run.stdout);
    }
    let logins = broker.sasl_logins();
    assert!(logins.iter().any(|(m, u)| m == "PLAIN" && u == "alice"));
    assert!(logins.iter().any(|(m, u)| m == "SCRAM" && u == "alice"));
    // consume in a group over SASL_SSL with SCRAM-SHA-512
    let env = [("KAFKA_SECURITY_PROTOCOL", "SASL_SSL"), ("KAFKA_CA", ca.as_str()), ("KAFKA_SASL_MECHANISM", "SCRAM-SHA-512"), ("KAFKA_SASL_USERNAME", "alice"), ("KAFKA_SASL_PASSWORD", "alice-secret")];
    let run = librdkafka(&b, &["consume", "secure", "sec-group", "6"], &env).ok();
    assert_eq!(run.value("consumed:"), "6");
    // the certificate is not valid for another name: hostname verification fails
    let run = librdkafka(&format!("127.0.0.2:{}", broker.port()), &["auth-error"], &env).ok();
    assert!(!run.value("auth-error:").is_empty());

    let plain = KafkaBroker::start_with(KafkaBrokerConfig { sasl_users: users, ..Default::default() }).unwrap();
    let env = [("KAFKA_SECURITY_PROTOCOL", "SASL_PLAINTEXT"), ("KAFKA_SASL_MECHANISM", "SCRAM-SHA-256"), ("KAFKA_SASL_USERNAME", "alice"), ("KAFKA_SASL_PASSWORD", "alice-secret")];
    let run = librdkafka(&plain.bootstrap(), &["produce", "plain", "3", "snappy"], &env).ok();
    assert_eq!(run.value("delivered:"), "3");
    let _ = std::fs::remove_dir_all(&pem.dir);
}

/// Mutual TLS: librdkafka presents a client certificate to a broker that requires one;
/// without one, or with a certificate from an unrelated CA, the handshake fails.
#[test]
fn librdkafka_mutual_tls() {
    if !official::enabled() {
        return;
    }
    let pem = official::pem_files("kafka-mtls");
    let tls = cameleer_tls::testing::mtls_server_params(cameleer_tls::ClientAuth::Need).server_config().unwrap();
    let broker = KafkaBroker::start_with(KafkaBrokerConfig { tls: Some(tls), ..Default::default() }).unwrap();
    broker.create_topic("mtls", 1);
    let b = format!("localhost:{}", broker.port());
    let ca = pem.ca.display().to_string();
    let (cert, key) = (pem.client_cert.display().to_string(), pem.client_key.display().to_string());
    let env = [("KAFKA_SECURITY_PROTOCOL", "SSL"), ("KAFKA_CA", ca.as_str()), ("KAFKA_CERT", cert.as_str()), ("KAFKA_KEY", key.as_str())];
    let run = librdkafka(&b, &["produce", "mtls", "2", "lz4"], &env).ok();
    assert_eq!(run.value("delivered:"), "2");
    let run = librdkafka(&b, &["auth-error"], &env[..2]).ok();
    assert!(run.value("auth-error:").contains("_SSL") || run.value("auth-error:").contains("_TRANSPORT"), "{}", run.stdout);
    let (ucert, ukey) = (pem.untrusted_cert.display().to_string(), pem.untrusted_key.display().to_string());
    let env = [("KAFKA_SECURITY_PROTOCOL", "SSL"), ("KAFKA_CA", ca.as_str()), ("KAFKA_CERT", ucert.as_str()), ("KAFKA_KEY", ukey.as_str())];
    let run = librdkafka(&b, &["auth-error"], &env).ok();
    assert!(!run.value("auth-error:").is_empty());
    assert_eq!(broker.records("mtls", 0).len(), 2);
    let _ = std::fs::remove_dir_all(&pem.dir);
}

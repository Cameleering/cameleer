//! Hermetic regressions for the wire-compatibility fixes found with librdkafka / kcat
//! (`tests/official_kafka.rs`): legacy API versions librdkafka probes for, legacy message
//! sets, SaslHandshake v0, compressed record batches stored verbatim, and the Kafka-style
//! SCRAM nonce check.

use std::io::Write;
use std::net::TcpStream;
use std::time::Duration;

use cameleer_core::prelude::*;
use cameleer_kafka_mqtt::kafka::protocol::*;
use cameleer_kafka_mqtt::testing::{KafkaBroker, KafkaBrokerConfig};

const FAST: &str = "pollTimeoutMs=100&heartbeatIntervalMs=50&fetchWaitMaxMs=100&autoOffsetReset=earliest";

struct Raw {
    s: TcpStream,
    corr: i32,
}

impl Raw {
    fn connect(b: &KafkaBroker) -> Raw {
        let s = TcpStream::connect(("127.0.0.1", b.port())).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        Raw { s, corr: 0 }
    }
    fn call(&mut self, key: i16, version: i16, body: &[u8]) -> Vec<u8> {
        self.corr += 1;
        let frame = encode_request(&RequestHeader { api_key: key, api_version: version, correlation_id: self.corr, client_id: Some("raw".into()) }, body);
        self.s.write_all(&frame).unwrap();
        let resp = read_frame(&mut self.s, 1 << 26).unwrap().expect("response");
        assert_eq!(i32::from_be_bytes(resp[..4].try_into().unwrap()), self.corr);
        resp[4..].to_vec()
    }
}

fn legacy_message(magic: i8, attributes: i8, ts: i64, key: Option<&[u8]>, value: Option<&[u8]>) -> Vec<u8> {
    let mut m = Encoder::new();
    m.i8(magic).i8(attributes);
    if magic >= 1 {
        m.i64(ts);
    }
    m.nullable_bytes(key).nullable_bytes(value);
    let crc = cameleer_core::dataformat::compress::crc32(&m.buf);
    let mut e = Encoder::new();
    e.u32(crc).raw(&m.buf);
    e.buf
}

fn entry(offset: i64, msg: &[u8]) -> Vec<u8> {
    let mut e = Encoder::new();
    e.i64(offset).i32(msg.len() as i32).raw(msg);
    e.buf
}

/// ApiVersions lists the version ranges librdkafka's feature probing needs.
#[test]
fn api_versions_cover_librdkafka_feature_probes() {
    let broker = KafkaBroker::start().unwrap();
    let mut c = Raw::connect(&broker);
    let r = ApiVersionsResponse::decode(&c.call(api::API_VERSIONS, 0, &[])).unwrap();
    let range = |k: i16| r.apis.iter().find(|(key, _, _)| *key == k).map(|(_, a, b)| (*a, *b)).unwrap();
    assert_eq!(range(api::PRODUCE), (0, 3), "gzip/snappy/lz4 need Produce v0, MsgVer1 needs v2");
    assert_eq!(range(api::FETCH), (0, 4));
    for k in [api::FIND_COORDINATOR, api::JOIN_GROUP, api::SYNC_GROUP, api::HEARTBEAT, api::LEAVE_GROUP, api::SASL_HANDSHAKE] {
        assert_eq!(range(k).0, 0, "{} v0", api::name(k));
    }
}

/// FindCoordinator v0 / JoinGroup v0 / SyncGroup v0 / Heartbeat v0 / LeaveGroup v0 layouts.
#[test]
fn group_coordination_v0() {
    let broker = KafkaBroker::start().unwrap();
    let mut c = Raw::connect(&broker);
    let mut e = Encoder::new();
    e.string("g0");
    let r = c.call(api::FIND_COORDINATOR, 0, &e.buf);
    let mut d = Decoder::new(&r);
    assert_eq!(d.i16().unwrap(), 0, "error code first (no throttle time in v0)");
    assert_eq!(d.i32().unwrap(), 0, "node id");
    assert_eq!(d.string().unwrap(), "127.0.0.1");
    assert_eq!(d.i32().unwrap(), broker.port() as i32);
    assert_eq!(d.remaining(), 0);

    // JoinGroup v0: no rebalance timeout in the request, no throttle time in the response
    let mut e = Encoder::new();
    e.string("g0").i32(6000).string("").string("consumer").len(1).string("range").bytes(&[0, 0, 0, 0, 0, 1, 0, 1, b't', 0, 0, 0, 0]);
    let r = c.call(api::JOIN_GROUP, 0, &e.buf);
    let mut d = Decoder::new(&r);
    assert_eq!(d.i16().unwrap(), 0);
    let generation = d.i32().unwrap();
    assert_eq!(d.string().unwrap(), "range");
    let leader = d.string().unwrap();
    let member = d.string().unwrap();
    assert_eq!(leader, member);
    // SyncGroup v0
    let mut e = Encoder::new();
    e.string("g0").i32(generation).string(&member).len(1).string(&member).bytes(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let r = c.call(api::SYNC_GROUP, 0, &e.buf);
    let mut d = Decoder::new(&r);
    assert_eq!(d.i16().unwrap(), 0, "no throttle time in v0");
    assert_eq!(d.bytes().unwrap().len(), 10);
    // Heartbeat v0 / LeaveGroup v0: just the error code
    let mut e = Encoder::new();
    e.string("g0").i32(generation).string(&member);
    assert_eq!(c.call(api::HEARTBEAT, 0, &e.buf), vec![0, 0]);
    let mut e = Encoder::new();
    e.string("g0").string(&member);
    assert_eq!(c.call(api::LEAVE_GROUP, 0, &e.buf), vec![0, 0]);
    assert_eq!(broker.group_members("g0").len(), 0);
}

/// Produce v0 / v2 with legacy message sets (magic 0, magic 1, gzip-compressed wrappers) and
/// Fetch v2 answering with a magic 1 message set.
#[test]
fn legacy_message_sets_produce_and_fetch() {
    let broker = KafkaBroker::start().unwrap();
    broker.create_topic("legacy", 1);
    let mut c = Raw::connect(&broker);
    // v0: magic 0, plain
    let set = entry(0, &legacy_message(0, 0, 0, Some(b"k0"), Some(b"v0")));
    let mut e = Encoder::new();
    e.i16(1).i32(5000).len(1).string("legacy").len(1).i32(0).bytes(&set);
    let r = c.call(api::PRODUCE, 0, &e.buf);
    let mut d = Decoder::new(&r);
    assert_eq!(d.len().unwrap(), 1);
    assert_eq!(d.string().unwrap(), "legacy");
    assert_eq!(d.len().unwrap(), 1);
    assert_eq!((d.i32().unwrap(), d.i16().unwrap(), d.i64().unwrap()), (0, 0, 0));
    assert_eq!(d.remaining(), 0, "v0 response has no throttle time / log append time");
    // v2: a gzip wrapper (magic 1) holding two messages with relative offsets 0 and 1
    let inner = [entry(0, &legacy_message(1, 0, 1000, Some(b"k1"), Some(b"v1"))), entry(1, &legacy_message(1, 0, 1001, None, Some(b"v2")))].concat();
    let wrapper = legacy_message(1, 1, 1001, None, Some(&cameleer_core::dataformat::compress::gzip_compress(&inner, 6)));
    let mut e = Encoder::new();
    e.i16(1).i32(5000).len(1).string("legacy").len(1).i32(0).bytes(&entry(1, &wrapper));
    let r = ProduceResponse::decode(&c.call(api::PRODUCE, 2, &e.buf)).unwrap();
    assert_eq!(r.topics[0].1[0].error_code, 0);
    assert_eq!(r.topics[0].1[0].base_offset, 1);
    let recs = broker.records("legacy", 0);
    assert_eq!(recs.iter().map(|r| (r.offset, r.value.clone().unwrap())).collect::<Vec<_>>(), vec![(0, b"v0".to_vec()), (1, b"v1".to_vec()), (2, b"v2".to_vec())]);
    assert_eq!(recs[1].timestamp, 1000);
    assert_eq!(recs[1].key.as_deref(), Some(&b"k1"[..]));
    // a corrupt CRC is refused
    let mut bad = legacy_message(1, 0, 1, None, Some(b"x"));
    bad[0] ^= 0xFF;
    let mut e = Encoder::new();
    e.i16(1).i32(5000).len(1).string("legacy").len(1).i32(0).bytes(&entry(0, &bad));
    let r = ProduceResponse::decode(&c.call(api::PRODUCE, 2, &e.buf)).unwrap();
    assert_eq!(r.topics[0].1[0].error_code, errors::CORRUPT_MESSAGE);

    // Fetch v2: replica, max wait, min bytes, [topic [partition offset max]] -> message set v1
    let mut e = Encoder::new();
    e.i32(-1).i32(100).i32(1).len(1).string("legacy").len(1).i32(0).i64(1).i32(1 << 20);
    let r = c.call(api::FETCH, 2, &e.buf);
    let mut d = Decoder::new(&r);
    assert_eq!(d.i32().unwrap(), 0, "throttle time");
    assert_eq!(d.len().unwrap(), 1);
    assert_eq!(d.string().unwrap(), "legacy");
    assert_eq!(d.len().unwrap(), 1);
    assert_eq!((d.i32().unwrap(), d.i16().unwrap(), d.i64().unwrap()), (0, 0, 3));
    let set = d.bytes().unwrap();
    assert_eq!(d.remaining(), 0);
    assert_eq!(set[16], 1, "magic 1");
    let fetched = decode_record_batches(&set).unwrap();
    assert_eq!(fetched.iter().map(|r| r.offset).collect::<Vec<_>>(), vec![1, 2]);
    assert_eq!(fetched[0].value.as_deref(), Some(&b"v1"[..]));
}

/// After SaslHandshake v0 the SASL tokens are bare frames (PLAIN), then normal requests follow.
#[test]
fn sasl_handshake_v0_with_bare_tokens() {
    let broker = KafkaBroker::start_with(KafkaBrokerConfig { sasl_users: vec![("u".into(), "pw".into())], ..Default::default() }).unwrap();
    let mut c = Raw::connect(&broker);
    let mut e = Encoder::new();
    e.string("PLAIN");
    let r = SaslHandshakeResponse::decode(&c.call(api::SASL_HANDSHAKE, 0, &e.buf)).unwrap();
    assert_eq!(r.error_code, 0);
    write_frame(&mut c.s, b"\0\0\0\x05\0u\0pw").unwrap();
    assert_eq!(read_frame(&mut c.s, 1024).unwrap().unwrap(), Vec::<u8>::new(), "empty token = success");
    let md = MetadataResponse::decode(&c.call(api::METADATA, 5, &MetadataRequest { topics: Some(vec![]), allow_auto_topic_creation: false }.encode())).unwrap();
    assert_eq!(md.brokers.len(), 1);
    assert_eq!(broker.sasl_logins(), vec![("PLAIN".to_string(), "u".to_string())]);
    // wrong password: the broker closes the connection
    let mut c = Raw::connect(&broker);
    let mut e = Encoder::new();
    e.string("PLAIN");
    c.call(api::SASL_HANDSHAKE, 0, &e.buf);
    write_frame(&mut c.s, b"\0\0\0\x06\0u\0bad").unwrap();
    assert!(read_frame(&mut c.s, 1024).map(|f| f.is_none()).unwrap_or(true));
}

/// Compressed record batches round-trip through the codecs and are rejected when corrupt.
#[test]
fn compressed_record_batches_round_trip() {
    let recs: Vec<Record> = (0..50)
        .map(|i| Record { offset: 0, timestamp: 1_700_000_000_000 + i, key: Some(format!("k{}", i).into_bytes()), value: Some(format!("value {} ", i).repeat(30).into_bytes()), headers: vec![("h".into(), Some(vec![i as u8])), ("n".into(), None)] })
        .collect();
    let plain = encode_record_batch(10, &recs);
    for codec in [Compression::Gzip, Compression::Snappy, Compression::Lz4] {
        let batch = encode_record_batch_with(10, &recs, codec).unwrap();
        assert_eq!(batch[22] & 7, codec.id() as u8);
        assert!(batch.len() < plain.len() / 3, "{:?} compresses", codec);
        let back = decode_record_batches(&batch).unwrap();
        assert_eq!(back.len(), 50);
        for (i, r) in back.iter().enumerate() {
            assert_eq!(r.offset, 10 + i as i64);
            assert_eq!(r.value, recs[i].value);
            assert_eq!(r.headers, recs[i].headers);
            assert_eq!(r.timestamp, recs[i].timestamp);
        }
        let mut corrupt = batch.clone();
        let n = corrupt.len();
        corrupt[n - 3] ^= 0x55;
        assert!(decode_record_batches(&corrupt).is_err(), "CRC32C covers the compressed payload");
    }
    // raw (unframed) snappy, as librdkafka writes it
    let mut raw = Encoder::new();
    for (i, r) in recs.iter().take(2).enumerate() {
        let mut rec = Encoder::new();
        rec.i8(0).varlong(0).varint(i as i32).varbytes(r.key.as_deref()).varbytes(r.value.as_deref()).varint(0);
        raw.varint(rec.buf.len() as i32).raw(&rec.buf);
    }
    let payload = cameleer_core::dataformat::codecs::snappy_compress(&raw.buf);
    let mut body = Encoder::new();
    body.i16(2).i32(1).i64(5).i64(5).i64(-1).i16(-1).i32(-1).i32(2).raw(&payload);
    let mut e = Encoder::new();
    e.i64(0).i32(9 + body.buf.len() as i32).i32(-1).i8(2).u32(crc32c(&body.buf)).raw(&body.buf);
    assert_eq!(decode_record_batches(&e.buf).unwrap().len(), 2);
    assert!(Compression::Zstd.compress(b"x").is_err());
    assert!(Compression::from_name("brotli").is_err());
}

/// `compressionCodec` on the producer; the broker keeps the batches verbatim and a fetch in
/// the middle of a compressed batch returns the whole batch, of which the consumer skips the
/// records before its position.
#[test]
fn producer_compression_and_verbatim_batches() {
    let broker = KafkaBroker::start().unwrap();
    broker.create_topic("zipped", 1);
    let b = broker.bootstrap();
    let ctx = CamelContext::new();
    cameleer_kafka_mqtt::register(&ctx);
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for codec in ["gzip", "snappy", "lz4", "none"] {
        t.send_body(&format!("kafka:zipped?brokers={}&compressionCodec={}", b, codec), format!("{} {}", codec, "x".repeat(500))).unwrap();
    }
    assert_eq!(broker.batch_compression("zipped", 0), vec!["gzip", "snappy", "lz4", "none"]);
    assert!(t.send_body(&format!("kafka:zipped?brokers={}&compressionCodec=zstd", b), "x").is_err());

    // one batch of 5 lz4 records via the client, then consume from offset 2 (inside it)
    let mut client = cameleer_kafka_mqtt::kafka::client::KafkaClient::new(cameleer_kafka_mqtt::kafka::client::ClientConfig { compression: Compression::Lz4, ..cameleer_kafka_mqtt::kafka::client::ClientConfig::new(&b, "t").unwrap() });
    broker.create_topic("mid", 1);
    let recs: Vec<Record> = (0..5).map(|i| Record { timestamp: 1, value: Some(format!("r{}", i).into_bytes()), ..Default::default() }).collect();
    client.produce("mid", 0, &recs, -1, 5000).unwrap();
    assert_eq!(broker.batch_compression("mid", 0), vec!["lz4"]);
    let mut e = Encoder::new();
    e.i32(-1).i32(100).i32(1).i32(1 << 20).i8(0).len(1).string("mid").len(1).i32(0).i64(2).i32(1 << 20);
    let mut c = Raw::connect(&broker);
    let f = FetchResponse::decode(&c.call(api::FETCH, 4, &e.buf)).unwrap();
    let got = decode_record_batches(&f.topics[0].1[0].records).unwrap();
    assert_eq!(got.first().map(|r| r.offset), Some(0), "the stored batch is returned whole");
    // commit offset 2 (a simple commit, generation -1) so the consumer starts inside the batch
    let commit = OffsetCommitRequest { group_id: "mid".into(), generation_id: -1, member_id: String::new(), retention_time_ms: -1, topics: vec![("mid".into(), vec![(0, 2, None)])] };
    assert_eq!(client.offset_commit(0, &commit).unwrap(), errors::NONE);
    ctx.add_routes(|r| {
        r.from(&format!("kafka:mid?brokers={}&groupId=mid&{}", b, FAST)).to("mock:mid");
    })
    .unwrap();
    let mock = ctx.mock_endpoint("mock:mid").unwrap();
    mock.expected_bodies_received(vec!["r2", "r3", "r4"]);
    mock.assert_is_satisfied_within(Duration::from_secs(10)).unwrap();
    ctx.stop().unwrap();
}

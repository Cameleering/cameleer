//! Live interop: `spring-redis`, pub/sub, Redis repositories, AUTH and TLS against a real Redis
//! server (`scripts/live-interop.sh`). Runs only with `CAMEL_LIVE_INTEROP=1`.

use std::sync::Arc;
use std::time::Duration;

use cameleer_core::prelude::*;
use cameleer_datastores::redis::{headers as h, RedisAggregationRepository, RedisClient, RedisConfig, RedisIdempotentRepository, Resp};
use cameleer_tls::testing::live;

const PORT: u16 = 16379;
const TLS_PORT: u16 = 16380;
const WAIT: Duration = Duration::from_secs(10);

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_datastores::register(&ctx);
    live::bind_beans(&ctx);
    ctx
}

/// A client on database `db`, emptied first (each test uses its own database).
fn client(db: i64) -> Arc<RedisClient> {
    let mut cfg = RedisConfig::new("127.0.0.1", PORT);
    cfg.password = Some(live::PASSWORD.into());
    cfg.database = db;
    let c = Arc::new(RedisClient::new(cfg));
    assert_eq!(c.cmd(&["FLUSHDB"]).unwrap(), Resp::ok());
    c
}

fn uri(db: i64) -> String {
    format!("spring-redis://127.0.0.1:{}?password={}&database={}", PORT, live::PASSWORD, db)
}

fn req(ctx: &CamelContext, uri: &str, cmd: &str, headers: Vec<(&str, Value)>) -> Result<Value> {
    let mut hs = vec![(h::COMMAND, Value::from(cmd))];
    hs.extend(headers);
    ctx.create_producer_template().request_body_and_headers(uri, Value::Null, hs)
}

#[test]
fn live_redis_producer_commands() {
    if !live::enabled() {
        return;
    }
    let raw = client(1);
    let ctx = context();
    ctx.start().unwrap();
    let uri = uri(1);
    let r = |cmd: &str, hs: Vec<(&str, Value)>| req(&ctx, &uri, cmd, hs).unwrap_or_else(|e| panic!("{}: {}", cmd, e));

    assert_eq!(r("PING", vec![]), Value::from("PONG"));
    r("SET", vec![(h::KEY, "k1".into()), (h::VALUE, "hello".into())]);
    assert_eq!(r("GET", vec![(h::KEY, "k1".into())]), Value::from("hello"));
    assert_eq!(r("EXISTS", vec![(h::KEY, "k1".into())]), Value::Bool(true));
    assert_eq!(r("EXISTS", vec![(h::KEY, "nope".into())]), Value::Bool(false));
    assert_eq!(r("INCR", vec![(h::KEY, "counter".into())]), Value::Int(1));
    assert_eq!(r("INCRBY", vec![(h::KEY, "counter".into()), (h::VALUE, 10.into())]), Value::Int(11));
    assert_eq!(r("EXPIRE", vec![(h::KEY, "k1".into()), (h::TIMEOUT, 100.into())]), Value::Bool(true));
    let ttl = r("TTL", vec![(h::KEY, "k1".into())]).as_i64().unwrap();
    assert!((99..=100).contains(&ttl), "{}", ttl);
    r("SETEX", vec![(h::KEY, "tmp".into()), (h::VALUE, "x".into()), (h::TIMEOUT, 10.into())]);
    assert_eq!(r("SETNX", vec![(h::KEY, "tmp".into()), (h::VALUE, "y".into())]), Value::Bool(false));
    assert_eq!(r("DEL", vec![(h::KEYS, Value::Array(vec!["k1".into(), "tmp".into(), "zzz".into()]))]), Value::Int(2));
    assert_eq!(r("GET", vec![(h::KEY, "k1".into())]), Value::Null);
    r("MSET", vec![(h::VALUES, Value::object([("a", "1"), ("b", "2")]))]);
    assert_eq!(r("MGET", vec![(h::FIELDS, Value::Array(vec!["a".into(), "b".into(), "c".into()]))]), Value::Array(vec!["1".into(), "2".into(), Value::Null]));
    let mut keys: Vec<String> = r("KEYS", vec![(h::PATTERN, "*".into())]).as_array().unwrap().iter().map(|v| v.to_string()).collect();
    keys.sort();
    assert_eq!(keys, vec!["a", "b", "counter"]);

    // hashes
    r("HSET", vec![(h::KEY, "user:1".into()), (h::FIELD, "name".into()), (h::VALUE, "Ada".into())]);
    r("HSET", vec![(h::KEY, "user:1".into()), (h::FIELD, "age".into()), (h::VALUE, 36.into())]);
    assert_eq!(r("HGET", vec![(h::KEY, "user:1".into()), (h::FIELD, "name".into())]), Value::from("Ada"));
    assert_eq!(r("HGETALL", vec![(h::KEY, "user:1".into())]), Value::object([("age", "36"), ("name", "Ada")]));
    assert_eq!(r("HEXISTS", vec![(h::KEY, "user:1".into()), (h::FIELD, "x".into())]), Value::Bool(false));
    assert_eq!(r("HINCRBY", vec![(h::KEY, "user:1".into()), (h::FIELD, "age".into()), (h::VALUE, 1.into())]), Value::Int(37));
    let mut hk: Vec<String> = r("HKEYS", vec![(h::KEY, "user:1".into())]).as_array().unwrap().iter().map(|v| v.to_string()).collect();
    hk.sort();
    assert_eq!(hk, vec!["age", "name"]);

    // lists
    assert_eq!(r("LPUSH", vec![(h::KEY, "q".into()), (h::VALUE, "a".into())]), Value::Int(1));
    r("LPUSH", vec![(h::KEY, "q".into()), (h::VALUE, "b".into())]);
    r("RPUSH", vec![(h::KEY, "q".into()), (h::VALUE, "c".into())]);
    assert_eq!(r("LRANGE", vec![(h::KEY, "q".into()), (h::START, 0.into()), (h::END, (-1).into())]), Value::Array(vec!["b".into(), "a".into(), "c".into()]));
    assert_eq!(r("RPOP", vec![(h::KEY, "q".into())]), Value::from("c"));
    assert_eq!(r("LLEN", vec![(h::KEY, "q".into())]), Value::Int(2));
    assert_eq!(r("LINDEX", vec![(h::KEY, "q".into()), (h::INDEX, 0.into())]), Value::from("b"));
    assert_eq!(r("BLPOP", vec![(h::KEY, "q".into()), (h::TIMEOUT, 1.into())]), Value::from("b"));

    // sets
    for m in ["x", "y", "x"] {
        r("SADD", vec![(h::KEY, "s".into()), (h::VALUE, m.into())]);
    }
    let mut members: Vec<String> = r("SMEMBERS", vec![(h::KEY, "s".into())]).as_array().unwrap().iter().map(|v| v.to_string()).collect();
    members.sort();
    assert_eq!(members, vec!["x", "y"]);
    assert_eq!(r("SISMEMBER", vec![(h::KEY, "s".into()), (h::VALUE, "y".into())]), Value::Bool(true));
    assert_eq!(r("SCARD", vec![(h::KEY, "s".into())]), Value::Int(2));

    // sorted sets
    assert_eq!(r("ZADD", vec![(h::KEY, "z".into()), (h::VALUE, "bob".into()), (h::SCORE, 2.5.into())]), Value::Bool(true));
    r("ZADD", vec![(h::KEY, "z".into()), (h::VALUE, "amy".into()), (h::SCORE, 1.into())]);
    assert_eq!(r("ZRANGE", vec![(h::KEY, "z".into()), (h::START, 0.into()), (h::END, (-1).into())]), Value::Array(vec!["amy".into(), "bob".into()]));
    assert_eq!(
        r("ZRANGE", vec![(h::KEY, "z".into()), (h::START, 0.into()), (h::END, 0.into()), (h::WITHSCORE, true.into())]),
        Value::Array(vec![Value::object([("value", Value::from("amy")), ("score", Value::Float(1.0))])])
    );
    assert_eq!(r("ZSCORE", vec![(h::KEY, "z".into()), (h::VALUE, "bob".into())]), Value::Float(2.5));
    assert_eq!(r("ZRANK", vec![(h::KEY, "z".into()), (h::VALUE, "bob".into())]), Value::Int(1));
    assert_eq!(r("ZCOUNT", vec![(h::KEY, "z".into()), (h::MIN, 2.into()), (h::MAX, 3.into())]), Value::Int(1));

    // JSON and binary values round-trip byte for byte
    r("SET", vec![(h::KEY, "json".into()), (h::VALUE, Value::object([("a", 1)]))]);
    assert_eq!(raw.cmd(&["GET", "json"]).unwrap(), Resp::bulk("{\"a\":1}"));
    r("SET", vec![(h::KEY, "bin".into()), (h::VALUE, Value::Bytes(vec![0xff, 0, 0x80]))]);
    assert_eq!(r("GET", vec![(h::KEY, "bin".into())]), Value::Bytes(vec![0xff, 0, 0x80]));

    // the real server's WRONGTYPE error
    let err = req(&ctx, &uri, "GET", vec![(h::KEY, "q".into())]).unwrap_err();
    assert!(err.root_cause().is_a("RedisSystemException"), "{:?}", err);
    assert!(err.message.contains("WRONGTYPE"), "{}", err);
    ctx.stop().unwrap();
}

#[test]
fn live_redis_auth_database_and_resp3() {
    if !live::enabled() {
        return;
    }
    let raw = client(2);
    let ctx = context();
    ctx.start().unwrap();
    let err = req(&ctx, &format!("spring-redis://127.0.0.1:{}", PORT), "PING", vec![]).unwrap_err();
    assert!(err.message.contains("NOAUTH"), "{}", err);
    let err = req(&ctx, &format!("spring-redis://127.0.0.1:{}?password=bad", PORT), "PING", vec![]).unwrap_err();
    assert!(err.root_cause().is_a("RedisConnectionFailureException"), "{:?}", err);
    assert!(err.message.contains("WRONGPASS"), "{}", err);
    req(&ctx, &uri(2), "SET", vec![(h::KEY, "k".into()), (h::VALUE, "db2".into())]).unwrap();
    assert_eq!(raw.cmd(&["GET", "k"]).unwrap(), Resp::bulk("db2"));
    // the default user with an explicit user name, over RESP3 (HELLO 3 AUTH default ...)
    let uri3 = format!("spring-redis://127.0.0.1:{}?username=default&password={}&protocol=3&database=2", PORT, live::PASSWORD);
    assert_eq!(req(&ctx, &uri3, "GET", vec![(h::KEY, "k".into())]).unwrap(), Value::from("db2"));
    req(&ctx, &uri3, "HSET", vec![(h::KEY, "hh".into()), (h::FIELD, "f".into()), (h::VALUE, "v".into())]).unwrap();
    assert_eq!(req(&ctx, &uri3, "HGETALL", vec![(h::KEY, "hh".into())]).unwrap(), Value::object([("f", "v")]));
    assert_eq!(req(&ctx, &uri3, "HGETALL", vec![(h::KEY, "none".into())]).unwrap(), Value::object(Vec::<(String, Value)>::new()));
    ctx.stop().unwrap();

    // client: pipelining, MULTI/EXEC, RESP3 types
    let mut cfg = RedisConfig::new("127.0.0.1", PORT);
    cfg.password = Some(live::PASSWORD.into());
    cfg.database = 2;
    cfg.protocol = 3;
    let c = RedisClient::new(cfg);
    let cmds: Vec<Vec<Vec<u8>>> = (0..200).map(|i| vec![b"RPUSH".to_vec(), b"pl".to_vec(), i.to_string().into_bytes()]).collect();
    let replies = c.pipeline(&cmds).unwrap();
    assert_eq!(replies.len(), 200);
    assert_eq!(replies[199], Resp::Int(200));
    assert_eq!(c.cmd(&["HGETALL", "hh"]).unwrap(), Resp::Map(vec![(Resp::bulk("f"), Resp::bulk("v"))]));
    assert_eq!(c.cmd(&["GET", "missing"]).unwrap(), Resp::Null);
    assert_eq!(c.cmd(&["ZINCRBY", "z", "1.5", "m"]).unwrap(), Resp::Double(1.5));
    let tx = c.pipeline(&[vec![b"MULTI".to_vec()], vec![b"INCR".to_vec(), b"c".to_vec()], vec![b"INCR".to_vec(), b"c".to_vec()], vec![b"EXEC".to_vec()]]).unwrap();
    assert_eq!(tx[3], Resp::array(vec![Resp::Int(1), Resp::Int(2)]));
    // a large value (multi-segment reads)
    let big = "x".repeat(1 << 20);
    c.cmd(&["SET", "big", &big]).unwrap();
    assert_eq!(c.cmd(&["STRLEN", "big"]).unwrap(), Resp::Int(1 << 20));
    assert_eq!(c.cmd(&["GET", "big"]).unwrap().as_bytes().map(|b| b.len()), Some(1 << 20));
}

#[test]
fn live_redis_pubsub_consumer() {
    if !live::enabled() {
        return;
    }
    let raw = client(3);
    let ctx = context();
    let base = format!("spring-redis://127.0.0.1:{}?password={}", PORT, live::PASSWORD);
    let (b1, b2) = (base.clone(), base.clone());
    ctx.add_routes(move |r| {
        r.from(&format!("{}&command=SUBSCRIBE&channels=live.news,live.alerts", b1)).to("mock:sub");
        r.from(&format!("{}&command=PSUBSCRIBE&channels=live.sensor.*", b2)).to("mock:psub");
    })
    .unwrap();
    ctx.start().unwrap();
    // wait until the real server reports the subscriptions
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let n = match raw.cmd(&["PUBSUB", "NUMSUB", "live.news", "live.alerts"]).unwrap() {
            Resp::Array(Some(items)) => items.iter().skip(1).step_by(2).filter(|v| **v == Resp::Int(1)).count(),
            _ => 0,
        };
        let p = raw.cmd(&["PUBSUB", "NUMPAT"]).unwrap();
        if n == 2 && p != Resp::Int(0) {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "subscriptions not registered");
        std::thread::sleep(Duration::from_millis(20));
    }
    let sub = ctx.mock_endpoint("mock:sub").unwrap();
    sub.expected_bodies_received(vec!["n1", "a1"]);
    let psub = ctx.mock_endpoint("mock:psub").unwrap();
    psub.expected_bodies_received(vec!["21.5"]);
    psub.expected_header_received(h::CHANNEL, "live.sensor.temp");
    psub.expected_header_received(h::PATTERN, "live.sensor.*");
    let pub_uri = format!("{}&command=PUBLISH", base);
    let t = ctx.create_producer_template();
    t.send_body_and_header(&pub_uri, "n1", h::CHANNEL, "live.news").unwrap();
    t.send_body_and_header(&pub_uri, "a1", h::CHANNEL, "live.alerts").unwrap();
    t.send_body_and_headers(&pub_uri, "ignored", vec![(h::CHANNEL, "live.sensor.temp".into()), (h::MESSAGE, "21.5".into())]).unwrap();
    // a message published by another client (the redis server fans it out)
    sub.assert_is_satisfied_within(WAIT).unwrap();
    psub.assert_is_satisfied_within(WAIT).unwrap();
    assert_eq!(sub.received_exchanges()[0].header(h::CHANNEL), Some(&Value::from("live.news")));
    ctx.stop().unwrap();
}

#[test]
fn live_redis_tls() {
    if !live::enabled() {
        return;
    }
    let ctx = context();
    let tls = format!("spring-redis://localhost:{}?password={}&database=4&sslContextParameters=#liveSsl", TLS_PORT, live::PASSWORD);
    let tls2 = tls.clone();
    ctx.add_routes(move |r| {
        r.from(&format!("{}&channels=live.tls", tls2)).to("mock:tls");
    })
    .unwrap();
    ctx.start().unwrap();
    req(&ctx, &tls, "SET", vec![(h::KEY, "secure".into()), (h::VALUE, "yes".into())]).unwrap();
    assert_eq!(req(&ctx, &tls, "GET", vec![(h::KEY, "secure".into())]).unwrap(), Value::from("yes"));
    let bad = format!("spring-redis://localhost:{}?password={}&sslContextParameters=#liveUntrusted", TLS_PORT, live::PASSWORD);
    let err = req(&ctx, &bad, "GET", vec![(h::KEY, "secure".into())]).unwrap_err();
    assert!(err.root_cause().is_a("IOException"), "{:?}", err);
    let mock = ctx.mock_endpoint("mock:tls").unwrap();
    mock.expected_bodies_received(vec!["encrypted"]);
    // wait until the subscriber (connected over TLS) is registered, then publish over TLS
    let raw = client(4);
    let deadline = std::time::Instant::now() + WAIT;
    while raw.cmd(&["PUBSUB", "NUMSUB", "live.tls"]).unwrap() != Resp::array(vec![Resp::bulk("live.tls"), Resp::Int(1)]) {
        assert!(std::time::Instant::now() < deadline, "TLS subscriber not registered");
        std::thread::sleep(Duration::from_millis(20));
    }
    req(&ctx, &tls, "PUBLISH", vec![(h::CHANNEL, "live.tls".into()), (h::MESSAGE, "encrypted".into())]).unwrap();
    mock.assert_is_satisfied_within(WAIT).unwrap();
    ctx.stop().unwrap();
}

#[test]
fn live_redis_repositories() {
    if !live::enabled() {
        return;
    }
    let c = client(5);
    let repo = Arc::new(RedisIdempotentRepository::new(c.clone(), "orders"));
    let ctx = context();
    let agg = Arc::new(RedisAggregationRepository::new(&ctx, c.clone(), "agg"));
    let (r2, a2) = (repo.clone(), agg.clone());
    ctx.add_routes(move |b| {
        b.from("direct:in").idempotent_consumer(header("id"), r2.clone()).to("mock:out").end();
        b.from("direct:agg")
            .aggregate(header("group"), |old: Option<Exchange>, new: Exchange| -> Result<Exchange> {
                Ok(match old {
                    None => new,
                    Some(mut o) => {
                        let body = format!("{}+{}", o.body(), new.body());
                        o.set_body(body);
                        o
                    }
                })
            })
            .completion_size(3)
            .aggregation_repository(a2.clone())
            .to("mock:aggregated")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:out").unwrap();
    mock.expected_bodies_received(vec!["a", "b"]);
    let t = ctx.create_producer_template();
    for (id, b) in [("1", "a"), ("1", "dup"), ("2", "b"), ("2", "dup")] {
        t.send_body_and_header("direct:in", b, "id", id).unwrap();
    }
    mock.assert_is_satisfied().unwrap();
    assert_eq!(repo.size().unwrap(), 2);
    assert_eq!(c.cmd(&["SISMEMBER", "orders", "1"]).unwrap(), Resp::Int(1));
    use cameleer_core::processor::errors::IdempotentRepository;
    assert!(repo.contains("2"));
    assert!(repo.remove("2"));
    assert!(!repo.contains("2"));

    let aggregated = ctx.mock_endpoint("mock:aggregated").unwrap();
    aggregated.expected_message_count(2);
    for (g, b) in [("A", "a1"), ("B", "b1"), ("A", "a2"), ("B", "b2"), ("A", "a3"), ("C", "c1"), ("B", "b3")] {
        t.send_body_and_header("direct:agg", b, "group", g).unwrap();
    }
    aggregated.assert_is_satisfied().unwrap();
    let bodies: Vec<String> = aggregated.received_bodies().iter().map(|v| v.to_string()).collect();
    assert_eq!(bodies, vec!["a1+a2+a3", "b1+b2+b3"]);
    use cameleer_core::processor::routing::AggregationRepository;
    assert_eq!(agg.keys(), vec!["C".to_string()]);
    assert_eq!(agg.get("C").unwrap().body(), &Value::from("c1"));
    assert!(agg.scan().unwrap().is_empty());
    // a second repository instance (another process in real life) sees the state in Redis
    let again = RedisAggregationRepository::new(&ctx, c.clone(), "agg");
    assert_eq!(again.get("C").unwrap().header("group"), Some(&Value::from("C")));
    repo.clear();
    assert_eq!(repo.size().unwrap(), 0);
    ctx.stop().unwrap();
}

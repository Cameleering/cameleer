use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use cameleer_core::prelude::*;
use cameleer_datastores::redis::{headers as h, RedisAggregationRepository, RedisClient, RedisConfig, RedisIdempotentRepository, Resp};
use cameleer_datastores::testing::{RedisServer, RedisServerConfig};

const WAIT: Duration = Duration::from_secs(10);

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_datastores::register(&ctx);
    ctx
}

fn req(ctx: &CamelContext, uri: &str, cmd: &str, headers: Vec<(&str, Value)>) -> Result<Value> {
    let mut hs = vec![(h::COMMAND, Value::from(cmd))];
    hs.extend(headers);
    ctx.create_producer_template().request_body_and_headers(uri, Value::Null, hs)
}

#[test]
fn producer_commands() {
    let server = RedisServer::start().unwrap();
    let ctx = context();
    ctx.start().unwrap();
    let uri = format!("spring-redis://{}", server.address());
    let r = |cmd: &str, hs: Vec<(&str, Value)>| req(&ctx, &uri, cmd, hs).unwrap();

    // strings + keys
    assert_eq!(r("SET", vec![(h::KEY, "k1".into()), (h::VALUE, "hello".into())]), Value::Null, "SET keeps the body");
    assert_eq!(r("GET", vec![(h::KEY, "k1".into())]), Value::from("hello"));
    assert_eq!(r("EXISTS", vec![(h::KEY, "k1".into())]), Value::Bool(true));
    assert_eq!(r("EXISTS", vec![(h::KEY, "nope".into())]), Value::Bool(false));
    assert_eq!(r("INCR", vec![(h::KEY, "counter".into())]), Value::Int(1));
    assert_eq!(r("INCRBY", vec![(h::KEY, "counter".into()), (h::VALUE, 10.into())]), Value::Int(11));
    assert_eq!(r("EXPIRE", vec![(h::KEY, "k1".into()), (h::TIMEOUT, 100.into())]), Value::Bool(true));
    let ttl = r("TTL", vec![(h::KEY, "k1".into())]).as_i64().unwrap();
    assert!((99..=100).contains(&ttl), "{}", ttl);
    assert_eq!(r("SETEX", vec![(h::KEY, "tmp".into()), (h::VALUE, "x".into()), (h::TIMEOUT, 1.into())]), Value::Null);
    assert_eq!(r("SETNX", vec![(h::KEY, "tmp".into()), (h::VALUE, "y".into())]), Value::Bool(false));
    assert_eq!(r("DEL", vec![(h::KEYS, Value::Array(vec!["k1".into(), "tmp".into(), "zzz".into()]))]), Value::Int(2));
    assert_eq!(r("GET", vec![(h::KEY, "k1".into())]), Value::Null);
    assert_eq!(r("MSET", vec![(h::VALUES, Value::object([("a", "1"), ("b", "2")]))]), Value::Null);
    assert_eq!(r("MGET", vec![(h::FIELDS, Value::Array(vec!["a".into(), "b".into(), "c".into()]))]), Value::Array(vec!["1".into(), "2".into(), Value::Null]));
    assert_eq!(r("KEYS", vec![(h::PATTERN, "*".into())]), Value::Array(vec!["a".into(), "b".into(), "counter".into()]));

    // hashes
    assert_eq!(r("HSET", vec![(h::KEY, "user:1".into()), (h::FIELD, "name".into()), (h::VALUE, "Ada".into())]), Value::Null);
    r("HSET", vec![(h::KEY, "user:1".into()), (h::FIELD, "age".into()), (h::VALUE, 36.into())]);
    assert_eq!(r("HGET", vec![(h::KEY, "user:1".into()), (h::FIELD, "name".into())]), Value::from("Ada"));
    assert_eq!(r("HGETALL", vec![(h::KEY, "user:1".into())]), Value::object([("age", "36"), ("name", "Ada")]));
    assert_eq!(r("HEXISTS", vec![(h::KEY, "user:1".into()), (h::FIELD, "x".into())]), Value::Bool(false));
    assert_eq!(r("HINCRBY", vec![(h::KEY, "user:1".into()), (h::FIELD, "age".into()), (h::VALUE, 1.into())]), Value::Int(37));
    assert_eq!(r("HKEYS", vec![(h::KEY, "user:1".into())]), Value::Array(vec!["age".into(), "name".into()]));

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
    assert_eq!(r("SMEMBERS", vec![(h::KEY, "s".into())]), Value::Array(vec!["x".into(), "y".into()]));
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

    // type errors surface as RedisSystemException
    let err = req(&ctx, &uri, "GET", vec![(h::KEY, "q".into())]).unwrap_err();
    assert!(err.root_cause().is_a("RedisSystemException"), "{:?}", err);
    assert!(err.root_cause().is_a("DataAccessException"));
    assert!(err.message.contains("WRONGTYPE"));
    // missing header
    assert!(req(&ctx, &uri, "GET", vec![]).is_err());
    ctx.stop().unwrap();
}

#[test]
fn endpoint_default_command_and_value_types() {
    let server = RedisServer::start().unwrap();
    let ctx = context();
    ctx.add_routes(|r| {
        r.from("direct:set").to(&format!("spring-redis://{}?command=SET", server.address()));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body_and_headers("direct:set", "body", vec![(h::KEY, "json".into()), (h::VALUE, Value::object([("a", 1)]))]).unwrap();
    t.send_body_and_headers("direct:set", "body", vec![(h::KEY, "bin".into()), (h::VALUE, Value::Bytes(vec![0xff, 0]))]).unwrap();
    assert_eq!(server.exec(&["GET", "json"]), Resp::bulk("{\"a\":1}"));
    assert_eq!(server.exec(&["GET", "bin"]).as_bytes().unwrap(), &[0xff, 0]);
    let v = req(&ctx, &format!("spring-redis://{}", server.address()), "GET", vec![(h::KEY, "bin".into())]).unwrap();
    assert_eq!(v, Value::Bytes(vec![0xff, 0]));
    ctx.stop().unwrap();
}

#[test]
fn consumer_subscribe_and_psubscribe() {
    let server = RedisServer::start().unwrap();
    let ctx = context();
    let addr = server.address();
    ctx.add_routes(|r| {
        r.from(&format!("spring-redis://{}?command=SUBSCRIBE&channels=news,alerts", addr)).to("mock:sub");
        r.from(&format!("spring-redis://{}?command=PSUBSCRIBE&channels=sensor.*", addr)).to("mock:psub");
    })
    .unwrap();
    ctx.start().unwrap();
    assert!(server.wait_until(WAIT, |s| s.subscription_count() == 3));
    let sub = ctx.mock_endpoint("mock:sub").unwrap();
    sub.expected_bodies_received(vec!["n1", "a1"]);
    let psub = ctx.mock_endpoint("mock:psub").unwrap();
    psub.expected_bodies_received(vec!["21.5"]);
    psub.expected_header_received(h::CHANNEL, "sensor.temp");
    psub.expected_header_received(h::PATTERN, "sensor.*");
    let pub_uri = format!("spring-redis://{}?command=PUBLISH", addr);
    let t = ctx.create_producer_template();
    t.send_body_and_header(&pub_uri, "n1", h::CHANNEL, "news").unwrap();
    t.send_body_and_header(&pub_uri, "a1", h::CHANNEL, "alerts").unwrap();
    t.send_body_and_headers(&pub_uri, "ignored", vec![(h::CHANNEL, "sensor.temp".into()), (h::MESSAGE, "21.5".into())]).unwrap();
    sub.assert_is_satisfied_within(WAIT).unwrap();
    psub.assert_is_satisfied_within(WAIT).unwrap();
    assert_eq!(sub.received_exchanges()[0].header(h::CHANNEL), Some(&Value::from("news")));
    ctx.stop().unwrap();
    assert!(server.wait_until(WAIT, |s| s.subscription_count() == 0));
}

#[test]
fn authentication_and_database_selection() {
    let mut users = BTreeMap::new();
    users.insert("app".to_string(), "apppw".to_string());
    let server = RedisServer::start_with(RedisServerConfig { password: Some("secret".into()), users, tls: None }).unwrap();
    let ctx = context();
    ctx.start().unwrap();
    let a = server.address();
    // no password
    let err = req(&ctx, &format!("spring-redis://{}", a), "PING", vec![]).unwrap_err();
    assert!(err.message.contains("NOAUTH"), "{}", err);
    // wrong password
    let err = req(&ctx, &format!("spring-redis://{}?password=bad", a), "PING", vec![]).unwrap_err();
    assert!(err.root_cause().is_a("RedisConnectionFailureException"), "{:?}", err);
    assert!(err.message.contains("WRONGPASS"));
    // right password, database 3
    let uri = format!("spring-redis://{}?password=secret&database=3", a);
    req(&ctx, &uri, "SET", vec![(h::KEY, "k".into()), (h::VALUE, "db3".into())]).unwrap();
    assert_eq!(req(&ctx, &uri, "GET", vec![(h::KEY, "k".into())]).unwrap(), Value::from("db3"));
    assert_eq!(server.exec(&["GET", "k"]), Resp::Bulk(None), "not in db 0");
    assert_eq!(server.exec(&["SELECT", "3"]), Resp::ok());
    // ACL user over RESP3
    let uri3 = format!("spring-redis://{}?username=app&password=apppw&protocol=3&database=3", a);
    assert_eq!(req(&ctx, &uri3, "GET", vec![(h::KEY, "k".into())]).unwrap(), Value::from("db3"));
    assert_eq!(req(&ctx, &uri3, "HGETALL", vec![(h::KEY, "none".into())]).unwrap(), Value::object(Vec::<(String, Value)>::new()));
    let err = req(&ctx, &format!("spring-redis://{}?username=app&password=no&protocol=3", a), "PING", vec![]).unwrap_err();
    assert!(err.root_cause().is_a("RedisConnectionFailureException"));
    ctx.stop().unwrap();
}

#[test]
fn client_pipelining_resp3_and_transactions() {
    let server = RedisServer::start().unwrap();
    let mut cfg = RedisConfig::new("127.0.0.1", server.port());
    cfg.protocol = 3;
    let client = RedisClient::new(cfg);
    let cmds: Vec<Vec<Vec<u8>>> = (0..50).map(|i| vec![b"RPUSH".to_vec(), b"pl".to_vec(), i.to_string().into_bytes()]).collect();
    let replies = client.pipeline(&cmds).unwrap();
    assert_eq!(replies.len(), 50);
    assert_eq!(replies[49], Resp::Int(50));
    assert_eq!(client.cmd(&["HSET", "h", "f", "v"]).unwrap(), Resp::Int(1));
    assert_eq!(client.cmd(&["HGETALL", "h"]).unwrap(), Resp::Map(vec![(Resp::bulk("f"), Resp::bulk("v"))]));
    assert_eq!(client.cmd(&["GET", "missing"]).unwrap(), Resp::Null);
    assert_eq!(client.cmd(&["ZINCRBY", "z", "1.5", "m"]).unwrap(), Resp::Double(1.5));
    let tx = client
        .pipeline(&[vec![b"MULTI".to_vec()], vec![b"INCR".to_vec(), b"c".to_vec()], vec![b"INCR".to_vec(), b"c".to_vec()], vec![b"EXEC".to_vec()]])
        .unwrap();
    assert_eq!(tx[3], Resp::array(vec![Resp::Int(1), Resp::Int(2)]));
    // pub/sub over RESP3 (push messages)
    let mut sub = client.subscribe(&["ch".to_string()], &[]).unwrap();
    assert_eq!(client.publish("ch", b"hi").unwrap(), 1);
    let m = sub.next_message(WAIT).unwrap().unwrap();
    assert_eq!((m.channel.as_str(), m.payload.as_slice()), ("ch", &b"hi"[..]));
    assert!(sub.next_message(Duration::from_millis(50)).unwrap().is_none());
    sub.close();
}

#[test]
fn tls_connection() {
    let tls = Arc::new(cameleer_tls::testing::server_params().server_config().unwrap());
    let server = RedisServer::start_with(RedisServerConfig { tls: Some(tls), ..Default::default() }).unwrap();
    let ctx = context();
    cameleer_tls::testing::bind_test_beans(&ctx);
    ctx.start().unwrap();
    let uri = format!("spring-redis://localhost:{}?sslContextParameters=#ssl", server.port());
    req(&ctx, &uri, "SET", vec![(h::KEY, "secure".into()), (h::VALUE, "yes".into())]).unwrap();
    assert_eq!(req(&ctx, &uri, "GET", vec![(h::KEY, "secure".into())]).unwrap(), Value::from("yes"));
    // untrusted certificate
    let bad = format!("spring-redis://localhost:{}?sslContextParameters=#sslUntrusted", server.port());
    let err = req(&ctx, &bad, "GET", vec![(h::KEY, "secure".into())]).unwrap_err();
    assert!(err.root_cause().is_a("IOException"), "{:?}", err);
    // TLS pub/sub
    ctx.add_routes(|r| {
        r.from(&format!("spring-redis://localhost:{}?sslContextParameters=#ssl&channels=tls", server.port())).to("mock:tls");
    })
    .unwrap();
    let mock = ctx.mock_endpoint("mock:tls").unwrap();
    mock.expected_bodies_received(vec!["encrypted"]);
    assert!(server.wait_until(WAIT, |s| s.subscription_count() == 1));
    req(&ctx, &uri, "PUBLISH", vec![(h::CHANNEL, "tls".into()), (h::MESSAGE, "encrypted".into())]).unwrap();
    mock.assert_is_satisfied_within(WAIT).unwrap();
    ctx.stop().unwrap();
}

#[test]
fn redis_template_bean() {
    let server = RedisServer::start().unwrap();
    let ctx = context();
    ctx.registry().bind("redis", RedisClient::new(RedisConfig::new("127.0.0.1", server.port())));
    ctx.start().unwrap();
    req(&ctx, "spring-redis://ignored:1?redisTemplate=#redis", "SET", vec![(h::KEY, "b".into()), (h::VALUE, "v".into())]).unwrap();
    assert_eq!(server.exec(&["GET", "b"]), Resp::bulk("v"));
    assert!(ctx.get_endpoint("spring-redis://x:1?redisTemplate=#nope").is_err());
}

#[test]
fn idempotent_repository() {
    let server = RedisServer::start().unwrap();
    let client = Arc::new(RedisClient::new(RedisConfig::new("127.0.0.1", server.port())));
    let repo = Arc::new(RedisIdempotentRepository::new(client.clone(), "orders"));
    let ctx = context();
    let r2 = repo.clone();
    ctx.add_routes(move |b| {
        b.from("direct:in").idempotent_consumer(header("id"), r2.clone()).to("mock:out").end();
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
    assert_eq!(server.exec(&["SISMEMBER", "orders", "1"]), Resp::Int(1));
    use cameleer_core::processor::errors::IdempotentRepository;
    assert!(repo.contains("2"));
    assert!(repo.remove("2"));
    assert!(!repo.contains("2"));
    repo.clear();
    assert_eq!(repo.size().unwrap(), 0);
    ctx.stop().unwrap();
}

#[test]
fn aggregation_repository() {
    let server = RedisServer::start().unwrap();
    let client = Arc::new(RedisClient::new(RedisConfig::new("127.0.0.1", server.port())));
    let ctx = context();
    let repo = Arc::new(RedisAggregationRepository::new(&ctx, client, "agg"));
    let r2 = repo.clone();
    ctx.add_routes(move |b| {
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
            .aggregation_repository(r2.clone())
            .to("mock:aggregated")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:aggregated").unwrap();
    mock.expected_message_count(2);
    let t = ctx.create_producer_template();
    for (g, b) in [("A", "a1"), ("B", "b1"), ("A", "a2"), ("B", "b2"), ("A", "a3"), ("C", "c1"), ("B", "b3")] {
        t.send_body_and_header("direct:agg", b, "group", g).unwrap();
    }
    mock.assert_is_satisfied().unwrap();
    let bodies: Vec<String> = mock.received_bodies().iter().map(|v| v.to_string()).collect();
    assert_eq!(bodies, vec!["a1+a2+a3", "b1+b2+b3"]);
    use cameleer_core::processor::routing::AggregationRepository;
    assert_eq!(repo.keys(), vec!["C".to_string()]);
    assert_eq!(repo.get("C").unwrap().body(), &Value::from("c1"));
    assert!(repo.scan().unwrap().is_empty(), "completed exchanges were confirmed");
    ctx.stop().unwrap();
}

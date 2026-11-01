//! spring-redis serializers, geo commands, Sentinel and Cluster basics.

use std::sync::Arc;

use camel_core::prelude::*;
use camel_datastores::redis::{headers as h, RedisClient, RedisConfig, RedisSerializer, RedisSerializerBean, Resp};
use camel_datastores::testing::RedisServer;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    camel_datastores::register(&ctx);
    ctx
}

fn req(ctx: &CamelContext, uri: &str, cmd: &str, headers: Vec<(&str, Value)>) -> Result<Value> {
    let mut hs = vec![(h::COMMAND, Value::from(cmd))];
    hs.extend(headers);
    ctx.create_producer_template().request_body_and_headers(uri, Value::Null, hs)
}

struct Upper;
impl RedisSerializer for Upper {
    fn serialize(&self, v: &Value) -> Vec<u8> {
        v.to_string().to_uppercase().into_bytes()
    }
    fn deserialize(&self, b: &[u8]) -> Value {
        Value::String(format!("<{}>", String::from_utf8_lossy(b)))
    }
}

#[test]
fn serializers() {
    let server = RedisServer::start().unwrap();
    let ctx = context();
    ctx.registry().bind("upper", RedisSerializerBean(Arc::new(Upper)));
    ctx.start().unwrap();
    let base = format!("spring-redis://{}", server.address());
    // json: objects and strings are JSON documents, replies are parsed
    let json = format!("{}?serializer=json", base);
    req(&ctx, &json, "SET", vec![(h::KEY, "doc".into()), (h::VALUE, Value::object([("a", Value::Int(1)), ("b", Value::from("x"))]))]).unwrap();
    req(&ctx, &json, "SET", vec![(h::KEY, "s".into()), (h::VALUE, "text".into())]).unwrap();
    assert_eq!(server.exec(&["GET", "s"]), Resp::bulk("\"text\""));
    assert_eq!(req(&ctx, &json, "GET", vec![(h::KEY, "doc".into())]).unwrap().get("a"), Some(&Value::Int(1)));
    assert_eq!(req(&ctx, &json, "GET", vec![(h::KEY, "s".into())]).unwrap(), Value::from("text"));
    req(&ctx, &json, "RPUSH", vec![(h::KEY, "l".into()), (h::VALUE, Value::Int(5))]).unwrap();
    assert_eq!(req(&ctx, &json, "LRANGE", vec![(h::KEY, "l".into()), (h::START, 0.into()), (h::END, (-1).into())]).unwrap(), Value::Array(vec![Value::Int(5)]));
    req(&ctx, &json, "HMSET", vec![(h::KEY, "hh".into()), (h::VALUES, Value::object([("f", Value::Bool(true))]))]).unwrap();
    assert_eq!(req(&ctx, &json, "HGETALL", vec![(h::KEY, "hh".into())]).unwrap(), Value::object([("f", Value::Bool(true))]));
    // bytes: replies stay binary
    let bytes = format!("{}?serializer=org.springframework.data.redis.serializer.ByteArrayRedisSerializer", base);
    req(&ctx, &bytes, "SET", vec![(h::KEY, "bin".into()), (h::VALUE, Value::Bytes(vec![0, 159, 146, 150]))]).unwrap();
    assert_eq!(req(&ctx, &bytes, "GET", vec![(h::KEY, "bin".into())]).unwrap(), Value::Bytes(vec![0, 159, 146, 150]));
    assert_eq!(req(&ctx, &bytes, "GET", vec![(h::KEY, "s".into())]).unwrap(), Value::Bytes(b"\"text\"".to_vec()));
    // string: always text
    let string = format!("{}?serializer=string", base);
    assert_eq!(req(&ctx, &string, "GET", vec![(h::KEY, "doc".into())]).unwrap(), Value::from("{\"a\":1,\"b\":\"x\"}"));
    // custom bean
    let custom = format!("{}?serializer=#upper", base);
    req(&ctx, &custom, "SET", vec![(h::KEY, "c".into()), (h::VALUE, "abc".into())]).unwrap();
    assert_eq!(server.exec(&["GET", "c"]), Resp::bulk("ABC"));
    assert_eq!(req(&ctx, &custom, "GET", vec![(h::KEY, "c".into())]).unwrap(), Value::from("<ABC>"));
    assert!(ctx.get_endpoint(&format!("{}?serializer=jdk", base)).is_err());
    assert!(ctx.get_endpoint(&format!("{}?serializer=nope", base)).is_err());
}

#[test]
fn geo_commands() {
    let server = RedisServer::start().unwrap();
    let ctx = context();
    ctx.start().unwrap();
    let uri = format!("spring-redis://{}", server.address());
    let r = |cmd: &str, hs: Vec<(&str, Value)>| req(&ctx, &uri, cmd, hs).unwrap();
    assert_eq!(r("GEOADD", vec![(h::KEY, "Sicily".into()), (h::LONGITUDE, Value::Float(13.361389)), (h::LATITUDE, Value::Float(38.115556)), (h::VALUE, "Palermo".into())]), Value::Int(1));
    r("GEOADD", vec![(h::KEY, "Sicily".into()), (h::LONGITUDE, Value::Float(15.087269)), (h::LATITUDE, Value::Float(37.502669)), (h::VALUE, "Catania".into())]);
    // the sorted set holds the same scores as real Redis
    assert_eq!(server.exec(&["ZSCORE", "Sicily", "Palermo"]), Resp::bulk("3479099956230698"));
    let d = r("GEODIST", vec![(h::KEY, "Sicily".into()), (h::VALUES, Value::Array(vec!["Palermo".into(), "Catania".into()]))]).as_f64().unwrap();
    assert!((d - 166274.1516).abs() < 0.001, "{}", d);
    assert_eq!(r("GEOHASH", vec![(h::KEY, "Sicily".into()), (h::VALUES, Value::Array(vec!["Palermo".into(), "Catania".into()]))]), Value::Array(vec!["sqc8b49rny0".into(), "sqdtr74hyu0".into()]));
    let pos = r("GEOPOS", vec![(h::KEY, "Sicily".into()), (h::VALUES, Value::Array(vec!["Palermo".into(), "Nowhere".into()]))]);
    let p = pos.as_array().unwrap();
    assert!((p[0].get("longitude").unwrap().as_f64().unwrap() - 13.361389).abs() < 1e-5);
    assert_eq!(p[1], Value::Null);
    // GEORADIUS 15 37 200 km
    let near = r("GEORADIUS", vec![(h::KEY, "Sicily".into()), (h::LONGITUDE, Value::Int(15)), (h::LATITUDE, Value::Int(37)), (h::RADIUS, Value::Int(200_000))]);
    let near = near.as_array().unwrap();
    assert_eq!(near.len(), 2);
    assert_eq!(near[0].get("name"), Some(&Value::from("Catania")));
    assert!((near[0].get("distance").unwrap().as_f64().unwrap() - 56441.2654).abs() < 0.01);
    let one = r("GEORADIUS", vec![(h::KEY, "Sicily".into()), (h::LONGITUDE, Value::Int(15)), (h::LATITUDE, Value::Int(37)), (h::RADIUS, Value::Int(100_000))]);
    assert_eq!(one.as_array().unwrap().len(), 1);
    let by = r("GEORADIUSBYMEMBER", vec![(h::KEY, "Sicily".into()), (h::VALUE, "Palermo".into()), (h::RADIUS, Value::Int(200_000)), (h::COUNT, Value::Int(1))]);
    assert_eq!(by.as_array().unwrap()[0].get("name"), Some(&Value::from("Palermo")));
    assert_eq!(by.as_array().unwrap()[0].get("distance").unwrap().as_f64(), Some(0.0));
}

#[test]
fn sentinel_master_discovery_and_failover() {
    let master1 = RedisServer::start().unwrap();
    let master2 = RedisServer::start().unwrap();
    let sentinel = RedisServer::start().unwrap();
    sentinel.set_sentinel_master("mymaster", &master1.address());
    let ctx = context();
    ctx.start().unwrap();
    let uri = format!("spring-redis://ignored:1?sentinels=127.0.0.1:1,{}&sentinelMaster=mymaster", sentinel.address());
    req(&ctx, &uri, "SET", vec![(h::KEY, "k".into()), (h::VALUE, "one".into())]).unwrap();
    assert_eq!(master1.exec(&["GET", "k"]), Resp::bulk("one"));
    // fail over: the old master goes away, the sentinel points to the new one
    sentinel.set_sentinel_master("mymaster", &master2.address());
    master1.stop();
    drop(master1);
    let mut ok = false;
    for _ in 0..3 {
        if req(&ctx, &uri, "SET", vec![(h::KEY, "k".into()), (h::VALUE, "two".into())]).is_ok() {
            ok = true;
            break;
        }
    }
    assert!(ok);
    assert_eq!(master2.exec(&["GET", "k"]), Resp::bulk("two"));
    // unknown master
    let bad = format!("spring-redis://ignored:1?sentinels={}&sentinelMaster=other", sentinel.address());
    assert!(req(&ctx, &bad, "GET", vec![(h::KEY, "k".into())]).is_err());
}

#[test]
fn cluster_redirections() {
    let a = RedisServer::start().unwrap();
    let b = RedisServer::start().unwrap();
    a.set_cluster(0, 8191, vec![(8192, 16383, b.address())]);
    b.set_cluster(8192, 16383, vec![(0, 8191, a.address())]);
    // "foo" hashes to slot 12182 (node b), "bar" to 5061 (node a)
    let client = RedisClient::new({
        let mut c = RedisConfig::new("127.0.0.1", a.port());
        c.cluster = true;
        c
    });
    client.cmd(&["SET", "foo", "1"]).unwrap();
    client.cmd(&["SET", "bar", "2"]).unwrap();
    assert_eq!(b.exec(&["GET", "foo"]), Resp::bulk("1"));
    assert_eq!(a.exec(&["GET", "bar"]), Resp::bulk("2"));
    assert_eq!(client.cmd(&["GET", "foo"]).unwrap(), Resp::bulk("1"));
    // without cluster support the redirection is an error
    let plain = RedisClient::new(RedisConfig::new("127.0.0.1", a.port()));
    let e = plain.cmd(&["GET", "foo"]).unwrap_err();
    assert!(e.message.starts_with("MOVED 12182"), "{}", e.message);
    assert_eq!(plain.cmd(&["CLUSTER", "KEYSLOT", "foo"]).unwrap(), Resp::Int(12182));
    // the component option
    let ctx = context();
    ctx.start().unwrap();
    let uri = format!("spring-redis://{}?cluster=true", b.address());
    req(&ctx, &uri, "SET", vec![(h::KEY, "bar".into()), (h::VALUE, "3".into())]).unwrap();
    assert_eq!(req(&ctx, &uri, "GET", vec![(h::KEY, "bar".into())]).unwrap(), Value::from("3"));
    assert_eq!(a.exec(&["GET", "bar"]), Resp::bulk("3"));
}

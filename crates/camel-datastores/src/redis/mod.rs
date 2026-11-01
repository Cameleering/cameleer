//! Redis: RESP2/RESP3 client, the Camel `spring-redis` component, Redis-backed
//! idempotent/aggregation repositories and an embedded test server.
//!
//! `spring-redis://host:port?command=SET&channels=a,b&password=secret`
//!
//! - **Producer**: runs `command` (header `CamelRedis.Command` overrides it) with the
//!   arguments taken from the `CamelRedis.*` headers (`Key`, `Keys`, `Field`, `Fields`,
//!   `Value`, `Values`, `Timeout`, `Start`, `End`, `Index`, `Offset`, `Destination`,
//!   `Channel`, `Message`, `Pivot`, `Position`, `Count`, `Timestamp`, `Pattern`, `Db`,
//!   `Score`, `Min`, `Max`, `Increment`, `WithScore`) like Camel's `RedisProducer`, and
//!   stores the result in the body (commands that return nothing in Spring Data Redis, e.g.
//!   `SET`, `HSET`, `PUBLISH`, keep the body). Values go through the `serializer`
//!   ([`RedisSerializer`]): by default UTF-8 text (bytes as is, arrays/objects as JSON) with
//!   replies as text when valid UTF-8, else bytes; `serializer=string` (Spring's
//!   `StringRedisSerializer`), `json` (`GenericJackson2JsonRedisSerializer`: JSON written,
//!   replies parsed), `bytes` (`ByteArrayRedisSerializer`: replies stay bytes) or a `#bean`
//!   of type [`RedisSerializerBean`]. There is no Java serialization (Camel's
//!   `JdkSerializationRedisSerializer` default). Keys and field names are always text.
//!   Geo commands: `GEOADD` (`Key`, `Longitude`, `Latitude`, `Value`), `GEODIST` (`Key`,
//!   `Values` = two members; meters as float), `GEOHASH` / `GEOPOS` (`Key`, `Values`),
//!   `GEORADIUS` (`Key`, `Longitude`, `Latitude`, `Radius` in meters, optional `Count`) and
//!   `GEORADIUSBYMEMBER` (`Key`, `Value`, `Radius`, `Count`), results as objects `{name,
//!   distance, longitude, latitude}` nearest first.
//! - **Consumer**: `SUBSCRIBE` to `channels` (or `PSUBSCRIBE` when `command=PSUBSCRIBE`); body
//!   = message, headers `CamelRedis.Channel` and `CamelRedis.Pattern`.
//!
//! Options: `command`, `channels`, `password`, `username` (ACL user), `database`,
//! `protocol` (2 or 3: RESP3 via `HELLO 3`), `connectTimeout`, `timeout` (reply timeout),
//! `redisTemplate` / `connectionFactory` (`#bean` of type [`RedisClient`]), TLS with
//! `ssl=true` / `sslContextParameters=#bean` / `useGlobalSslContextParameters=true`,
//! `serializer`; accepted and ignored: `listenerContainer`.
//! Topologies (rust-camel options): `sentinels=host:port,...` with `sentinelMaster=name`
//! resolve the master through Redis Sentinel; `cluster=true` routes commands by key slot
//! and follows `MOVED`/`ASK` redirections (see [`client`]).

pub mod client;
pub mod cluster;
pub mod geo;
pub mod repository;
pub mod resp;
pub mod server;

use std::any::Any;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camel_core::component::support::{process_consumer_exchange, BackgroundThreads};
use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::value::{ObjectMap, Value};

pub use client::{PubSubMessage, RedisClient, RedisConfig, RedisConnection, Subscription};
pub use repository::{RedisAggregationRepository, RedisIdempotentRepository};
pub use resp::Resp;

use crate::util::{header, header_bool, header_str, split_host_port};

/// Header names (Camel `RedisConstants`).
pub mod headers {
    pub const COMMAND: &str = "CamelRedis.Command";
    pub const KEY: &str = "CamelRedis.Key";
    pub const KEYS: &str = "CamelRedis.Keys";
    pub const FIELD: &str = "CamelRedis.Field";
    pub const FIELDS: &str = "CamelRedis.Fields";
    pub const VALUE: &str = "CamelRedis.Value";
    pub const VALUES: &str = "CamelRedis.Values";
    pub const START: &str = "CamelRedis.Start";
    pub const END: &str = "CamelRedis.End";
    pub const TIMEOUT: &str = "CamelRedis.Timeout";
    pub const OFFSET: &str = "CamelRedis.Offset";
    pub const DESTINATION: &str = "CamelRedis.Destination";
    pub const CHANNEL: &str = "CamelRedis.Channel";
    pub const MESSAGE: &str = "CamelRedis.Message";
    pub const INDEX: &str = "CamelRedis.Index";
    pub const POSITION: &str = "CamelRedis.Position";
    pub const PIVOT: &str = "CamelRedis.Pivot";
    pub const COUNT: &str = "CamelRedis.Count";
    pub const TIMESTAMP: &str = "CamelRedis.Timestamp";
    pub const PATTERN: &str = "CamelRedis.Pattern";
    pub const DB: &str = "CamelRedis.Db";
    pub const SCORE: &str = "CamelRedis.Score";
    pub const MIN: &str = "CamelRedis.Min";
    pub const MAX: &str = "CamelRedis.Max";
    pub const INCREMENT: &str = "CamelRedis.Increment";
    pub const WITHSCORE: &str = "CamelRedis.WithScore";
    pub const LONGITUDE: &str = "CamelRedis.Longitude";
    pub const LATITUDE: &str = "CamelRedis.Latitude";
    pub const RADIUS: &str = "CamelRedis.Radius";
}

/// Converts message values to Redis values and back (Spring Data Redis `RedisSerializer`).
pub trait RedisSerializer: Send + Sync {
    fn serialize(&self, v: &Value) -> Vec<u8>;
    fn deserialize(&self, b: &[u8]) -> Value;
}

/// Registry bean wrapping a custom serializer (`serializer=#name`).
#[derive(Clone)]
pub struct RedisSerializerBean(pub Arc<dyn RedisSerializer>);

/// Default: text (bytes as is, arrays/objects as JSON); replies as text when UTF-8.
pub struct TextRedisSerializer;
/// `StringRedisSerializer`: everything as UTF-8 text.
pub struct StringRedisSerializer;
/// `GenericJackson2JsonRedisSerializer`: JSON both ways.
pub struct JsonRedisSerializer;
/// `ByteArrayRedisSerializer`: raw bytes both ways.
pub struct ByteArrayRedisSerializer;

impl RedisSerializer for TextRedisSerializer {
    fn serialize(&self, v: &Value) -> Vec<u8> {
        value_bytes(v)
    }
    fn deserialize(&self, b: &[u8]) -> Value {
        resp::bytes_to_value(b)
    }
}

impl RedisSerializer for StringRedisSerializer {
    fn serialize(&self, v: &Value) -> Vec<u8> {
        match v {
            Value::Bytes(b) => String::from_utf8_lossy(b).into_owned().into_bytes(),
            other => value_bytes(other),
        }
    }
    fn deserialize(&self, b: &[u8]) -> Value {
        Value::String(String::from_utf8_lossy(b).into_owned())
    }
}

impl RedisSerializer for JsonRedisSerializer {
    fn serialize(&self, v: &Value) -> Vec<u8> {
        v.to_json().into_bytes()
    }
    fn deserialize(&self, b: &[u8]) -> Value {
        let text = String::from_utf8_lossy(b);
        camel_core::json::parse(&text).unwrap_or_else(|_| Value::String(text.into_owned()))
    }
}

impl RedisSerializer for ByteArrayRedisSerializer {
    fn serialize(&self, v: &Value) -> Vec<u8> {
        value_bytes(v)
    }
    fn deserialize(&self, b: &[u8]) -> Value {
        Value::Bytes(b.to_vec())
    }
}

/// Resolves the `serializer` option (name, Spring class name or `#bean`).
pub fn serializer_for(ctx: &CamelContext, name: Option<&str>) -> Result<Arc<dyn RedisSerializer>> {
    let Some(n) = name.map(|s| s.trim()).filter(|s| !s.is_empty()) else { return Ok(Arc::new(TextRedisSerializer)) };
    if let Some(bean) = n.strip_prefix('#') {
        let b = ctx.registry().lookup::<RedisSerializerBean>(bean).ok_or_else(|| CamelError::no_such_bean(bean))?;
        return Ok(b.0.clone());
    }
    let simple = n.rsplit('.').next().unwrap_or(n).to_ascii_lowercase();
    Ok(match simple.as_str() {
        "text" | "default" => Arc::new(TextRedisSerializer),
        "string" | "stringredisserializer" => Arc::new(StringRedisSerializer),
        "json" | "jackson2jsonredisserializer" | "genericjackson2jsonredisserializer" | "jackson" => Arc::new(JsonRedisSerializer),
        "bytes" | "byte" | "bytearrayredisserializer" | "raw" => Arc::new(ByteArrayRedisSerializer),
        "jdkserializationredisserializer" | "jdk" => return Err(CamelError::illegal_argument("spring-redis: Java serialization is not supported (use string, json or bytes)")),
        _ => return Err(CamelError::illegal_argument(format!("spring-redis: unknown serializer '{}' (string, json, bytes or #bean)", n))),
    })
}

/// Reply → value, running bulk strings through the serializer.
fn decode(r: &Resp, ser: &dyn RedisSerializer) -> Value {
    match r {
        Resp::Bulk(Some(b)) => ser.deserialize(b),
        Resp::Verbatim(_, b) => ser.deserialize(b),
        Resp::Array(Some(items)) | Resp::Set(items) | Resp::Push(items) => Value::Array(items.iter().map(|i| decode(i, ser)).collect()),
        other => other.to_value(),
    }
}

/// Serializes a Camel value for Redis (text as UTF-8, bytes as is, numbers as text,
/// arrays/objects as JSON).
pub fn value_bytes(v: &Value) -> Vec<u8> {
    match v {
        Value::Bytes(b) => b.clone(),
        Value::Array(_) | Value::Object(_) => v.to_json().into_bytes(),
        Value::Float(f) => resp::format_double(*f).into_bytes(),
        other => other.to_string().into_bytes(),
    }
}

/// The `spring-redis` component.
#[derive(Default)]
pub struct RedisComponent;

/// Endpoint configuration.
#[derive(Clone)]
pub struct RedisEndpointConfig {
    pub command: String,
    pub channels: Vec<String>,
    pub client: Arc<RedisClient>,
    pub serializer: Arc<dyn RedisSerializer>,
}

impl Component for RedisComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let (host, port) = split_host_port(remaining.trim_end_matches('/'), 6379)?;
        let command = params.take("command").unwrap_or_else(|| "SET".to_string()).to_ascii_uppercase();
        let channels: Vec<String> = params.take("channels").map(|c| c.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()).unwrap_or_default();
        let bean = params.take("redisTemplate").or_else(|| params.take("connectionFactory"));
        let tls = camel_tls::camel_support::endpoint_tls(ctx, params, "spring-redis", false, &["ssl"])?;
        let mut cfg = RedisConfig::new(&host, port);
        cfg.password = params.take("password").filter(|p| !p.is_empty());
        cfg.username = params.take("username").filter(|p| !p.is_empty());
        cfg.database = params.take_i64("database", 0)?;
        cfg.protocol = params.take_u64("protocol", 2)? as u8;
        if cfg.protocol != 2 && cfg.protocol != 3 {
            return Err(CamelError::illegal_argument(format!("Invalid Redis protocol {} (2 or 3)", cfg.protocol)));
        }
        cfg.connect_timeout = params.take_duration("connectTimeout", Duration::from_secs(10))?;
        cfg.read_timeout = Some(params.take_duration("timeout", Duration::from_secs(30))?);
        cfg.tls = tls;
        cfg.sentinels = match params.take("sentinels") {
            Some(list) => list.split(',').map(|h| h.trim()).filter(|h| !h.is_empty()).map(|h| split_host_port(h, 26379)).collect::<Result<Vec<_>>>()?,
            None => Vec::new(),
        };
        cfg.master_name = params.take("sentinelMaster").or_else(|| params.take("masterName")).filter(|s| !s.is_empty());
        if !cfg.sentinels.is_empty() && cfg.master_name.is_none() {
            cfg.master_name = Some("mymaster".into());
        }
        cfg.cluster = params.take_bool("cluster", false)?;
        let serializer = serializer_for(ctx, params.take("serializer").as_deref())?;
        for k in ["listenerContainer", "lazyStartProducer", "bridgeErrorHandler", "exceptionHandler", "exchangePattern"] {
            params.take(k);
        }
        let client = match bean {
            Some(b) => {
                let name = b.trim_start_matches('#');
                ctx.registry().lookup::<RedisClient>(name).ok_or_else(|| CamelError::no_such_bean(name))?
            }
            None => Arc::new(RedisClient::new(cfg)),
        };
        Ok(Arc::new(RedisEndpoint { uri: uri.to_string(), ctx: ctx.downgrade(), cfg: RedisEndpointConfig { command, channels, client, serializer } }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A `spring-redis` endpoint.
pub struct RedisEndpoint {
    uri: String,
    ctx: WeakContext,
    cfg: RedisEndpointConfig,
}

impl RedisEndpoint {
    pub fn config(&self) -> &RedisEndpointConfig {
        &self.cfg
    }
}

impl Endpoint for RedisEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(RedisProducer { cfg: self.cfg.clone() }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        if self.cfg.channels.is_empty() {
            return Err(CamelError::illegal_argument("A spring-redis consumer needs the channels option"));
        }
        Ok(Arc::new(RedisConsumer { cfg: self.cfg.clone(), ctx: self.ctx.clone(), processor, logger: self.uri.clone(), threads: BackgroundThreads::new(), closer: Mutex::new(None) }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// What the producer does with a reply.
#[derive(Clone, Copy, PartialEq)]
enum Out {
    /// Keep the body (void in Spring Data Redis).
    Void,
    /// Integer reply as boolean.
    Bool,
    /// Plain conversion.
    Raw,
    /// Flat field/value list or map → object.
    Map,
    /// Text → float.
    Float,
    /// `WITHSCORES` reply → array of `{value, score}`.
    Scored,
    /// `GEOPOS` reply → array of `{longitude, latitude}` (or null).
    GeoPos,
    /// `GEORADIUS ... WITHDIST WITHCOORD` → array of `{name, distance, longitude, latitude}`.
    GeoResults,
}

fn num(r: &Resp) -> Value {
    match r {
        Resp::Double(d) => Value::Float(*d),
        other => other.as_text().and_then(|t| t.parse::<f64>().ok()).map(Value::Float).unwrap_or(Value::Null),
    }
}

struct RedisProducer {
    cfg: RedisEndpointConfig,
}

fn req<'a>(ex: &'a Exchange, name: &str) -> Result<&'a Value> {
    header(ex, name).ok_or_else(|| CamelError::new(camel_core::error::kinds::NO_SUCH_HEADER, format!("No '{}' header available", name)))
}

fn hb(ex: &Exchange, name: &str) -> Result<Vec<u8>> {
    req(ex, name).map(value_bytes)
}

fn list_header(ex: &Exchange, name: &str) -> Result<Vec<Vec<u8>>> {
    match req(ex, name)? {
        Value::Array(a) => Ok(a.iter().map(value_bytes).collect()),
        Value::String(s) if s.contains(',') => Ok(s.split(',').map(|x| x.trim().as_bytes().to_vec()).collect()),
        other => Ok(vec![value_bytes(other)]),
    }
}

fn hs(ex: &Exchange, name: &str) -> Result<Vec<u8>> {
    Ok(req(ex, name)?.to_string().into_bytes())
}

fn to_object(r: &Resp, ser: &dyn RedisSerializer) -> Value {
    let mut m = ObjectMap::new();
    match r {
        Resp::Map(pairs) => {
            for (k, v) in pairs {
                m.insert(k.to_value().to_string(), decode(v, ser));
            }
        }
        _ => {
            let items = r.items().unwrap_or_default();
            for p in items.chunks(2) {
                if p.len() == 2 {
                    m.insert(p[0].to_value().to_string(), decode(&p[1], ser));
                }
            }
        }
    }
    Value::Object(m)
}

fn scored(r: &Resp) -> Value {
    fn score(r: &Resp) -> f64 {
        match r {
            Resp::Double(d) => *d,
            other => other.as_text().and_then(|t| t.parse().ok()).unwrap_or(0.0),
        }
    }
    let items = r.items().unwrap_or_default();
    // RESP3 replies nest [member, score] pairs, RESP2 replies are flat
    let pairs: Vec<&[Resp]> = if !items.is_empty() && items.iter().all(|i| i.items().is_some()) {
        items.iter().filter_map(|p| p.items()).collect()
    } else {
        items.chunks(2).collect()
    };
    Value::Array(pairs.into_iter().filter(|p| p.len() == 2).map(|p| Value::object([("value", p[0].to_value()), ("score", Value::Float(score(&p[1])))])).collect())
}

impl RedisProducer {
    /// Builds the command arguments and the result handling for `cmd`.
    fn build(&self, ex: &Exchange, cmd: &str) -> Result<(Vec<Vec<u8>>, Out)> {
        use headers::*;
        let c = cmd.as_bytes().to_vec();
        let ser = self.cfg.serializer.as_ref();
        let k = || hb(ex, KEY);
        let v = || req(ex, VALUE).map(|x| ser.serialize(x));
        let sv = |name: &str| -> Result<Vec<Vec<u8>>> {
            Ok(match req(ex, name)? {
                Value::Array(a) => a.iter().map(|x| ser.serialize(x)).collect(),
                other => vec![ser.serialize(other)],
            })
        };
        let smap = |name: &str| -> Result<Vec<Vec<u8>>> {
            match req(ex, name)? {
                Value::Object(m) => Ok(m.iter().flat_map(|(k, v)| [k.as_bytes().to_vec(), ser.serialize(v)]).collect()),
                Value::Array(a) => Ok(a.iter().map(value_bytes).collect()),
                other => Err(CamelError::illegal_argument(format!("Header {} must be a map, got {}", name, other.type_name()))),
            }
        };
        let timeout = || hs(ex, TIMEOUT);
        let mut a = vec![c];
        let out = match cmd {
            // keys
            "DEL" => {
                match header(ex, KEYS) {
                    Some(_) => a.extend(list_header(ex, KEYS)?),
                    None => a.push(k()?),
                }
                Out::Raw
            }
            "EXISTS" | "PERSIST" => {
                a.push(k()?);
                Out::Bool
            }
            "EXPIRE" | "PEXPIRE" => {
                a.extend([k()?, timeout()?]);
                Out::Bool
            }
            "EXPIREAT" | "PEXPIREAT" => {
                a.extend([k()?, hs(ex, TIMESTAMP)?]);
                Out::Bool
            }
            "KEYS" => {
                a.push(hb(ex, PATTERN)?);
                Out::Raw
            }
            "MOVE" => {
                a.extend([k()?, hs(ex, DB)?]);
                Out::Bool
            }
            "RANDOMKEY" | "PING" | "DBSIZE" => Out::Raw,
            "RENAME" => {
                a.extend([k()?, v()?]);
                Out::Void
            }
            "RENAMENX" => {
                a.extend([k()?, v()?]);
                Out::Bool
            }
            "SORT" | "TTL" | "PTTL" | "TYPE" | "GET" | "DECR" | "INCR" | "STRLEN" | "HGETALL" | "HKEYS" | "HLEN" | "HVALS" | "LLEN" | "LPOP" | "RPOP" | "SCARD" | "SMEMBERS" | "SPOP" | "SRANDMEMBER" | "ZCARD" | "GETDEL" => {
                a.push(k()?);
                if cmd == "HGETALL" { Out::Map } else { Out::Raw }
            }
            "ECHO" => {
                a.push(v()?);
                Out::Raw
            }
            // strings
            "SET" => {
                a.extend([k()?, v()?]);
                if let Some(t) = header(ex, TIMEOUT) {
                    a.extend([b"EX".to_vec(), t.to_string().into_bytes()]);
                }
                Out::Void
            }
            "SETEX" => {
                a.extend([k()?, timeout()?, v()?]);
                Out::Void
            }
            "SETNX" => {
                a.extend([k()?, v()?]);
                Out::Bool
            }
            "GETSET" | "APPEND" | "DECRBY" | "INCRBY" | "LPUSH" | "RPUSH" | "LPUSHX" | "RPUSHX" | "SADD" | "SREM" | "SISMEMBER" | "ZRANK" | "ZREVRANK" | "ZREM" => {
                a.extend([k()?, v()?]);
                if cmd == "SISMEMBER" { Out::Bool } else { Out::Raw }
            }
            "INCRBYFLOAT" => {
                a.extend([k()?, v()?]);
                Out::Float
            }
            "GETBIT" => {
                a.extend([k()?, hs(ex, OFFSET)?]);
                Out::Bool
            }
            "SETBIT" => {
                let bit = match req(ex, VALUE)? {
                    Value::Bool(b) => if *b { b"1".to_vec() } else { b"0".to_vec() },
                    other => {
                        if other.is_predicate_true() && other.to_string() != "0" { b"1".to_vec() } else { b"0".to_vec() }
                    }
                };
                a.extend([k()?, hs(ex, OFFSET)?, bit]);
                Out::Bool
            }
            "GETRANGE" | "LRANGE" | "LTRIM" | "ZREMRANGEBYRANK" => {
                a.extend([k()?, hs(ex, START)?, hs(ex, END)?]);
                if cmd == "LTRIM" { Out::Void } else { Out::Raw }
            }
            "SETRANGE" => {
                a.extend([k()?, hs(ex, OFFSET)?, v()?]);
                Out::Void
            }
            "MGET" => {
                a.extend(list_header(ex, FIELDS)?);
                Out::Raw
            }
            "MSET" => {
                a.extend(smap(VALUES)?);
                Out::Void
            }
            "MSETNX" => {
                a.extend(smap(VALUES)?);
                Out::Bool
            }
            // hashes
            "HSET" => {
                a.extend([k()?, hb(ex, FIELD)?, v()?]);
                Out::Void
            }
            "HSETNX" => {
                a.extend([k()?, hb(ex, FIELD)?, v()?]);
                Out::Bool
            }
            "HGET" | "HDEL" => {
                a.extend([k()?, hb(ex, FIELD)?]);
                Out::Raw
            }
            "HEXISTS" => {
                a.extend([k()?, hb(ex, FIELD)?]);
                Out::Bool
            }
            "HINCRBY" => {
                a.extend([k()?, hb(ex, FIELD)?, v()?]);
                Out::Raw
            }
            "HMGET" => {
                a.push(k()?);
                a.extend(list_header(ex, FIELDS)?);
                Out::Raw
            }
            "HMSET" => {
                a.push(k()?);
                a.extend(smap(VALUES)?);
                Out::Void
            }
            // lists
            "BLPOP" | "BRPOP" => {
                a.extend([k()?, timeout()?]);
                // Spring returns only the value
                return Ok((a, Out::Raw));
            }
            "BRPOPLPUSH" => {
                a.extend([k()?, hb(ex, DESTINATION)?, timeout()?]);
                Out::Raw
            }
            "RPOPLPUSH" => {
                a.extend([k()?, hb(ex, DESTINATION)?]);
                Out::Raw
            }
            "LINDEX" => {
                a.extend([k()?, hs(ex, INDEX)?]);
                Out::Raw
            }
            "LINSERT" => {
                let pos = header_str(ex, POSITION).unwrap_or_else(|| "BEFORE".into()).to_ascii_uppercase();
                a.extend([k()?, pos.into_bytes(), hb(ex, PIVOT)?, v()?]);
                Out::Raw
            }
            "LREM" => {
                a.extend([k()?, hs(ex, COUNT)?, v()?]);
                Out::Raw
            }
            "LSET" => {
                a.extend([k()?, hs(ex, INDEX)?, v()?]);
                Out::Void
            }
            // sets
            "SDIFF" | "SINTER" | "SUNION" => {
                a.push(k()?);
                a.extend(list_header(ex, KEYS)?);
                Out::Raw
            }
            "SDIFFSTORE" | "SINTERSTORE" | "SUNIONSTORE" => {
                a.push(hb(ex, DESTINATION)?);
                a.push(k()?);
                a.extend(list_header(ex, KEYS)?);
                Out::Void
            }
            "SMOVE" => {
                a.extend([k()?, hb(ex, DESTINATION)?, v()?]);
                Out::Bool
            }
            // sorted sets
            "ZADD" => {
                a.extend([k()?, hs(ex, SCORE)?, v()?]);
                Out::Bool
            }
            "ZCOUNT" | "ZREMRANGEBYSCORE" => {
                let (min, max) = if cmd == "ZCOUNT" { (MIN, MAX) } else { (START, END) };
                a.extend([k()?, hs(ex, min)?, hs(ex, max)?]);
                Out::Raw
            }
            "ZINCRBY" => {
                a.extend([k()?, hs(ex, INCREMENT)?, v()?]);
                Out::Float
            }
            "ZSCORE" => {
                a.extend([k()?, v()?]);
                Out::Float
            }
            "ZRANGE" | "ZREVRANGE" => {
                a.extend([k()?, hs(ex, START)?, hs(ex, END)?]);
                if header_bool(ex, WITHSCORE).unwrap_or(false) {
                    a.push(b"WITHSCORES".to_vec());
                    Out::Scored
                } else {
                    Out::Raw
                }
            }
            "ZRANGEBYSCORE" | "ZREVRANGEBYSCORE" => {
                a.extend([k()?, hs(ex, MIN)?, hs(ex, MAX)?]);
                if cmd == "ZREVRANGEBYSCORE" {
                    a.swap(2, 3);
                }
                Out::Raw
            }
            "ZUNIONSTORE" | "ZINTERSTORE" => {
                let keys = list_header(ex, KEYS)?;
                a.push(hb(ex, DESTINATION)?);
                a.push((keys.len() + 1).to_string().into_bytes());
                a.push(k()?);
                a.extend(keys);
                Out::Void
            }
            // geo
            "GEOADD" => {
                a.extend([k()?, hs(ex, LONGITUDE)?, hs(ex, LATITUDE)?, hb(ex, VALUE)?]);
                Out::Raw
            }
            "GEODIST" => {
                a.push(k()?);
                let members = list_header(ex, VALUES)?;
                if members.len() != 2 {
                    return Err(CamelError::illegal_argument("GEODIST needs two members in the CamelRedis.Values header"));
                }
                a.extend(members);
                a.push(b"m".to_vec());
                Out::Float
            }
            "GEOHASH" | "GEOPOS" => {
                a.push(k()?);
                match header(ex, VALUES) {
                    Some(_) => a.extend(list_header(ex, VALUES)?),
                    None => a.push(hb(ex, VALUE)?),
                }
                if cmd == "GEOPOS" { Out::GeoPos } else { Out::Raw }
            }
            "GEORADIUS" | "GEORADIUSBYMEMBER" => {
                a.push(k()?);
                if cmd == "GEORADIUS" {
                    a.extend([hs(ex, LONGITUDE)?, hs(ex, LATITUDE)?]);
                } else {
                    a.push(hb(ex, VALUE)?);
                }
                a.extend([hs(ex, RADIUS)?, b"m".to_vec(), b"WITHDIST".to_vec(), b"WITHCOORD".to_vec(), b"ASC".to_vec()]);
                if let Some(c) = header(ex, COUNT) {
                    a.extend([b"COUNT".to_vec(), c.to_string().into_bytes()]);
                }
                Out::GeoResults
            }
            // pub/sub
            "PUBLISH" => {
                let msg = match header(ex, MESSAGE) {
                    Some(m) => ser.serialize(m),
                    None => ser.serialize(ex.body()),
                };
                a.extend([hb(ex, CHANNEL)?, msg]);
                Out::Void
            }
            "MULTI" | "EXEC" | "DISCARD" | "UNWATCH" => Out::Raw,
            "WATCH" => {
                a.extend(list_header(ex, KEYS)?);
                Out::Void
            }
            "QUIT" => Out::Void,
            other => {
                // generic command: key (if any) followed by `CamelRedis.Values` (if any)
                if let Some(key) = header(ex, KEY) {
                    a.push(value_bytes(key));
                }
                if let Some(Value::Array(_)) = header(ex, VALUES) {
                    a.extend(sv(VALUES)?);
                } else if header(ex, VALUE).is_some() {
                    a.push(v()?);
                }
                let _ = other;
                Out::Raw
            }
        };
        Ok((a, out))
    }
}

impl Service for RedisProducer {}

impl Processor for RedisProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let cmd = header_str(ex, headers::COMMAND).unwrap_or_else(|| self.cfg.command.clone()).to_ascii_uppercase();
        if matches!(cmd.as_str(), "SUBSCRIBE" | "PSUBSCRIBE") {
            return Err(CamelError::new(camel_core::error::kinds::UNSUPPORTED_OPERATION, format!("{} is only supported by the spring-redis consumer", cmd)));
        }
        let (args, out) = self.build(ex, &cmd)?;
        let reply = self.cfg.client.command(&args)?;
        let value = {
            match out {
                Out::Scored => scored(&reply),
                Out::GeoPos => Value::Array(
                    reply
                        .items()
                        .unwrap_or_default()
                        .iter()
                        .map(|p| match p.items() {
                            Some(c) if c.len() == 2 => Value::object([("longitude", num(&c[0])), ("latitude", num(&c[1]))]),
                            _ => Value::Null,
                        })
                        .collect(),
                ),
                Out::GeoResults => Value::Array(
                    reply
                        .items()
                        .unwrap_or_default()
                        .iter()
                        .filter_map(|h| {
                            let it = h.items()?;
                            let coords = it.get(2).and_then(|c| c.items()).unwrap_or_default();
                            Some(Value::object([
                                ("name", decode(&it[0], self.cfg.serializer.as_ref())),
                                ("distance", it.get(1).map(num).unwrap_or(Value::Null)),
                                ("longitude", coords.first().map(num).unwrap_or(Value::Null)),
                                ("latitude", coords.get(1).map(num).unwrap_or(Value::Null)),
                            ]))
                        })
                        .collect(),
                ),
                Out::Void => return Ok(()),
                Out::Bool => Value::Bool(reply.as_int().map(|i| i != 0).unwrap_or(!reply.is_null())),
                Out::Map => to_object(&reply, self.cfg.serializer.as_ref()),
                Out::Float => match &reply {
                    Resp::Double(d) => Value::Float(*d),
                    r if r.is_null() => Value::Null,
                    r => r.as_text().and_then(|t| t.parse().ok()).map(Value::Float).unwrap_or(Value::Null),
                },
                Out::Raw => {
                    let ser = self.cfg.serializer.as_ref();
                    if matches!(cmd.as_str(), "BLPOP" | "BRPOP") {
                        reply.items().and_then(|i| i.get(1)).map(|v| decode(v, ser)).unwrap_or(Value::Null)
                    } else if matches!(cmd.as_str(), "KEYS" | "RANDOMKEY" | "TYPE" | "PING" | "ECHO" | "GEOHASH") {
                        reply.to_value()
                    } else {
                        decode(&reply, ser)
                    }
                }
            }
        };
        ex.set_body(value);
        Ok(())
    }
}

struct RedisConsumer {
    cfg: RedisEndpointConfig,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
    logger: String,
    threads: BackgroundThreads,
    closer: Mutex<Option<camel_tls::net::NetStream>>,
}

impl Service for RedisConsumer {
    fn start(&self) -> Result<()> {
        let pattern = self.cfg.command == "PSUBSCRIBE";
        let (channels, patterns) = if pattern { (vec![], self.cfg.channels.clone()) } else { (self.cfg.channels.clone(), vec![]) };
        let sub = self.cfg.client.subscribe(&channels, &patterns)?;
        *self.closer.lock().unwrap() = sub.closer().ok();
        let (client, ctx, processor, logger, ser) = (self.cfg.client.clone(), self.ctx.clone(), self.processor.clone(), self.logger.clone(), self.cfg.serializer.clone());
        camel_core::camel_info!("spring-redis", "Subscribed to {:?} on {}:{}", self.cfg.channels, client.config().host, client.config().port);
        self.threads.spawn("spring-redis-consumer", move |stop| {
            let mut sub = Some(sub);
            while !stop.is_stopped() {
                let s = match sub.as_mut() {
                    Some(s) => s,
                    None => match client.subscribe(&channels, &patterns) {
                        Ok(s) => sub.insert(s),
                        Err(e) => {
                            camel_core::camel_warn!("spring-redis", "Resubscribe failed: {}", e);
                            stop.sleep(Duration::from_millis(500));
                            continue;
                        }
                    },
                };
                match s.next_message(Duration::from_millis(200)) {
                    Ok(Some(m)) => {
                        let mut ex = Exchange::new(ctx.upgrade());
                        ex.set_body(ser.deserialize(&m.payload));
                        ex.set_header(headers::CHANNEL, m.channel.clone());
                        ex.set_header(headers::PATTERN, m.pattern.clone().unwrap_or(m.channel));
                        process_consumer_exchange(&logger, processor.as_ref(), &mut ex);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        if stop.is_stopped() {
                            break;
                        }
                        camel_core::camel_warn!("spring-redis", "Subscription error: {}", e);
                        if let Some(s) = sub.take() {
                            s.close();
                        }
                    }
                }
            }
            if let Some(s) = sub.take() {
                s.close();
            }
        })
    }
    fn stop(&self) -> Result<()> {
        if let Some(c) = self.closer.lock().unwrap().take() {
            let _ = c.shutdown(std::net::Shutdown::Both);
        }
        self.threads.stop();
        Ok(())
    }
}

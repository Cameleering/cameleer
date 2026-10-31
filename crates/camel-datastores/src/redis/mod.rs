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
//!   `SET`, `HSET`, `PUBLISH`, keep the body). Values are written as UTF-8 text (bytes as is,
//!   arrays/objects as JSON); replies become text when valid UTF-8, else bytes.
//! - **Consumer**: `SUBSCRIBE` to `channels` (or `PSUBSCRIBE` when `command=PSUBSCRIBE`); body
//!   = message, headers `CamelRedis.Channel` and `CamelRedis.Pattern`.
//!
//! Options: `command`, `channels`, `password`, `username` (ACL user), `database`,
//! `protocol` (2 or 3: RESP3 via `HELLO 3`), `connectTimeout`, `timeout` (reply timeout),
//! `redisTemplate` / `connectionFactory` (`#bean` of type [`RedisClient`]), TLS with
//! `ssl=true` / `sslContextParameters=#bean` / `useGlobalSslContextParameters=true`,
//! accepted and ignored: `serializer`, `listenerContainer`.

pub mod client;
pub mod repository;
pub mod resp;
pub mod server;

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camel_core::component::support::{process_consumer_exchange, BackgroundThreads};
use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::value::Value;

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
        for k in ["serializer", "listenerContainer", "lazyStartProducer", "bridgeErrorHandler", "exceptionHandler", "exchangePattern"] {
            params.take(k);
        }
        let client = match bean {
            Some(b) => {
                let name = b.trim_start_matches('#');
                ctx.registry().lookup::<RedisClient>(name).ok_or_else(|| CamelError::no_such_bean(name))?
            }
            None => Arc::new(RedisClient::new(cfg)),
        };
        Ok(Arc::new(RedisEndpoint { uri: uri.to_string(), ctx: ctx.downgrade(), cfg: RedisEndpointConfig { command, channels, client } }))
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

fn map_header(ex: &Exchange, name: &str) -> Result<Vec<Vec<u8>>> {
    match req(ex, name)? {
        Value::Object(m) => Ok(m.iter().flat_map(|(k, v)| [k.as_bytes().to_vec(), value_bytes(v)]).collect()),
        Value::Array(a) => Ok(a.iter().map(value_bytes).collect()),
        other => Err(CamelError::illegal_argument(format!("Header {} must be a map, got {}", name, other.type_name()))),
    }
}

fn hs(ex: &Exchange, name: &str) -> Result<Vec<u8>> {
    Ok(req(ex, name)?.to_string().into_bytes())
}

fn to_object(r: &Resp) -> Value {
    match r {
        Resp::Map(_) => r.to_value(),
        _ => {
            let items = r.items().unwrap_or_default();
            let mut m = BTreeMap::new();
            for p in items.chunks(2) {
                if p.len() == 2 {
                    m.insert(p[0].to_value().to_string(), p[1].to_value());
                }
            }
            Value::Object(m)
        }
    }
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
        let k = || hb(ex, KEY);
        let v = || hb(ex, VALUE);
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
                a.extend(map_header(ex, VALUES)?);
                Out::Void
            }
            "MSETNX" => {
                a.extend(map_header(ex, VALUES)?);
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
                a.extend(map_header(ex, VALUES)?);
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
            // pub/sub
            "PUBLISH" => {
                let msg = match header(ex, MESSAGE) {
                    Some(m) => value_bytes(m),
                    None => value_bytes(ex.body()),
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
                if let Some(Value::Array(vals)) = header(ex, VALUES) {
                    a.extend(vals.iter().map(value_bytes));
                } else if let Some(val) = header(ex, VALUE) {
                    a.push(value_bytes(val));
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
                Out::Void => return Ok(()),
                Out::Bool => Value::Bool(reply.as_int().map(|i| i != 0).unwrap_or(!reply.is_null())),
                Out::Map => to_object(&reply),
                Out::Float => match &reply {
                    Resp::Double(d) => Value::Float(*d),
                    r if r.is_null() => Value::Null,
                    r => r.as_text().and_then(|t| t.parse().ok()).map(Value::Float).unwrap_or(Value::Null),
                },
                Out::Raw => {
                    if matches!(cmd.as_str(), "BLPOP" | "BRPOP") {
                        reply.items().and_then(|i| i.get(1)).map(|v| v.to_value()).unwrap_or(Value::Null)
                    } else {
                        reply.to_value()
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
        let (client, ctx, processor, logger) = (self.cfg.client.clone(), self.ctx.clone(), self.processor.clone(), self.logger.clone());
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
                        ex.set_body(resp::bytes_to_value(&m.payload));
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

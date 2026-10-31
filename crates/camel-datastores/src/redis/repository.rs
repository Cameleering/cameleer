//! Redis-backed repositories for the idempotent consumer and the aggregator.

use std::sync::Arc;

use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::Result;
use camel_core::message::Exchange;
use camel_core::processor::errors::IdempotentRepository;
use camel_core::processor::routing::AggregationRepository;

use super::client::{args, RedisClient};
use super::resp::Resp;
use crate::util::{exchange_from_json, exchange_to_json};

fn warn<T>(what: &str, r: Result<T>, default: T) -> T {
    r.unwrap_or_else(|e| {
        camel_core::camel_warn!("RedisRepository", "Error during {}: {}", what, e);
        default
    })
}

/// Idempotent repository in a Redis set named after the processor (Camel
/// `RedisIdempotentRepository`: `SADD` / `SISMEMBER` / `SREM` / `DEL`).
pub struct RedisIdempotentRepository {
    client: Arc<RedisClient>,
    processor_name: String,
}

impl RedisIdempotentRepository {
    pub fn new(client: Arc<RedisClient>, processor_name: &str) -> Self {
        RedisIdempotentRepository { client, processor_name: processor_name.to_string() }
    }

    /// Number of stored keys (`SCARD`).
    pub fn size(&self) -> Result<i64> {
        Ok(self.client.cmd(&["SCARD", &self.processor_name])?.as_int().unwrap_or(0))
    }
}

impl IdempotentRepository for RedisIdempotentRepository {
    fn add(&self, key: &str) -> bool {
        warn("add", self.client.cmd(&["SADD", &self.processor_name, key]).map(|r| r.as_int() == Some(1)), false)
    }
    fn contains(&self, key: &str) -> bool {
        warn("contains", self.client.cmd(&["SISMEMBER", &self.processor_name, key]).map(|r| r.as_int() == Some(1)), false)
    }
    fn remove(&self, key: &str) -> bool {
        warn("remove", self.client.cmd(&["SREM", &self.processor_name, key]).map(|r| r.as_int() == Some(1)), false)
    }
    fn clear(&self) {
        warn("clear", self.client.cmd(&["DEL", &self.processor_name]).map(|_| ()), ());
    }
}

/// Aggregation repository in Redis (like Camel's `RedissonAggregationRepository`): the
/// groups in progress live in hash `<name>` (key → exchange JSON) with their insertion
/// order in sorted set `<name>:keys`, completed exchanges in hash `<name>-completed`
/// until confirmed. Exchanges are stored as JSON snapshots (body, headers, properties).
pub struct RedisAggregationRepository {
    client: Arc<RedisClient>,
    name: String,
    ctx: Option<WeakContext>,
}

impl RedisAggregationRepository {
    pub fn new(ctx: &CamelContext, client: Arc<RedisClient>, repository_name: &str) -> Self {
        RedisAggregationRepository { client, name: repository_name.to_string(), ctx: Some(ctx.downgrade()) }
    }

    fn completed(&self) -> String {
        format!("{}-completed", self.name)
    }

    fn keys_set(&self) -> String {
        format!("{}:keys", self.name)
    }

    fn load(&self, hash: &str, field: &str) -> Result<Option<Exchange>> {
        match self.client.cmd(&["HGET", hash, field])?.as_text() {
            Some(json) => Ok(Some(exchange_from_json(&json, self.ctx.as_ref().and_then(|c| c.upgrade()))?)),
            None => Ok(None),
        }
    }

    fn try_add(&self, key: &str, ex: &Exchange) -> Result<Option<Exchange>> {
        let old = self.load(&self.name, key)?;
        let json = exchange_to_json(ex);
        let seq_key = format!("{}:seq", self.name);
        let mut cmds = vec![args(["HSET", self.name.as_str(), key, json.as_str()])];
        if old.is_none() {
            let seq = self.client.cmd(&["INCR", &seq_key])?.as_int().unwrap_or(0);
            cmds.push(args(["ZADD".to_string(), self.keys_set(), seq.to_string(), key.to_string()]));
        }
        for r in self.client.pipeline(&cmds)? {
            if let Some(e) = super::client::reply_error(&r) {
                return Err(e);
            }
        }
        Ok(old)
    }

    fn try_remove(&self, key: &str) -> Result<Option<Exchange>> {
        let Some(ex) = self.load(&self.name, key)? else { return Ok(None) };
        let json = exchange_to_json(&ex);
        let cmds = vec![
            args(["MULTI"]),
            args(["HDEL", self.name.as_str(), key]),
            args(["ZREM".to_string(), self.keys_set(), key.to_string()]),
            args(["HSET".to_string(), self.completed(), ex.exchange_id.clone(), json]),
            args(["EXEC"]),
        ];
        let replies = self.client.pipeline(&cmds)?;
        if let Some(e) = replies.iter().find_map(super::client::reply_error) {
            return Err(e);
        }
        Ok(Some(ex))
    }

    /// Exchange ids of completed exchanges that were not confirmed yet.
    pub fn scan(&self) -> Result<Vec<String>> {
        let r = self.client.cmd(&["HKEYS", &self.completed()])?;
        Ok(r.items().unwrap_or_default().iter().filter_map(Resp::as_text).collect())
    }

    /// Loads a completed (unconfirmed) exchange for recovery.
    pub fn recover(&self, exchange_id: &str) -> Result<Option<Exchange>> {
        self.load(&self.completed(), exchange_id)
    }
}

impl AggregationRepository for RedisAggregationRepository {
    fn add(&self, key: &str, exchange: Exchange) -> Option<Exchange> {
        warn("add", self.try_add(key, &exchange), None)
    }
    fn get(&self, key: &str) -> Option<Exchange> {
        warn("get", self.load(&self.name, key), None)
    }
    fn remove(&self, key: &str) -> Option<Exchange> {
        warn("remove", self.try_remove(key), None)
    }
    fn confirm(&self, exchange_id: &str) {
        warn("confirm", self.client.cmd(&["HDEL", &self.completed(), exchange_id]).map(|_| ()), ());
    }
    fn keys(&self) -> Vec<String> {
        warn("keys", self.client.cmd(&["ZRANGE", &self.keys_set(), "0", "-1"]).map(|r| r.items().unwrap_or_default().iter().filter_map(Resp::as_text).collect()), Vec::new())
    }
}

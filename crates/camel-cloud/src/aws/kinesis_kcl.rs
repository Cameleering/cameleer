//! KCL-style Kinesis consumers for `aws2-kinesis`:
//!
//! * [`LeaseCoordinator`] (`useKclConsumers=true`): the Kinesis Client Library lease
//!   protocol on a DynamoDB **lease table** (named `applicationName`, default
//!   `{stream}-camel`; created on demand with hash key `leaseKey`). Every shard has a lease
//!   item `{leaseKey, checkpoint, leaseOwner, leaseCounter, ownerSwitchesSinceCheckpoint,
//!   parentShardIds}`. Each poll the worker (`workerIdentifier`, default random) creates
//!   missing leases (`attribute_not_exists` condition), renews its own leases (conditional
//!   `leaseCounter` increments; a failed condition means the lease was lost), takes unowned
//!   leases and leases whose counter did not move for `failoverTimeMillis` (default 10 s) up
//!   to its fair share (leases / live workers), and reads its shards from the checkpoint
//!   (`AFTER_SEQUENCE_NUMBER`, or the initial position `iteratorType` for new leases). Child
//!   shards are only read once their parents are checkpointed `SHARD_END`. Successfully
//!   processed records are checkpointed (conditional on still owning the lease); a failed
//!   exchange stops the shard for this poll and the records after the last checkpoint are
//!   delivered again. Leases are released (owner removed) when the consumer stops.
//! * [`FanOutConsumer`] (rust-camel `enhancedFanOut=true`, `consumerName`): registers (or
//!   reuses) a stream consumer (`RegisterStreamConsumer`, waits for `ACTIVE`), keeps one
//!   `SubscribeToShard` HTTP/2 event stream per shard in a background thread (resubscribing
//!   from the continuation sequence number when a subscription ends, following child shards),
//!   and hands the pushed records to the poll.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use camel_core::error::Result;
use camel_core::value::{ObjectMap, Value};

use super::ddb::{marshal_item, to_attribute_value, unmarshal_item};
use super::kinesis::KinesisService;
use super::{error_code, AwsConfig};
use crate::util::{kinds, obj, random_hex};

/// Checkpoint value of a fully consumed (closed) shard.
pub const SHARD_END: &str = "SHARD_END";

/// A lease as read from the lease table.
#[derive(Clone, Debug, PartialEq)]
pub struct Lease {
    pub shard: String,
    pub checkpoint: String,
    pub owner: Option<String>,
    pub counter: i64,
    pub parents: Vec<String>,
}

/// The KCL lease protocol on a DynamoDB table.
pub struct LeaseCoordinator {
    pub aws: AwsConfig,
    pub table: String,
    pub worker: String,
    pub failover: Duration,
    /// lease counter last seen per shard, and when it last changed
    seen: Mutex<HashMap<String, (i64, Instant)>>,
    table_ready: AtomicBool,
}

impl LeaseCoordinator {
    pub fn new(aws: AwsConfig, table: String, worker: String, failover: Duration) -> LeaseCoordinator {
        LeaseCoordinator { aws, table, worker, failover, seen: Mutex::new(HashMap::new()), table_ready: AtomicBool::new(false) }
    }

    fn ddb(&self, op: &str, body: Value) -> Result<Value> {
        self.aws.json_call("dynamodb", "dynamodb", "DynamoDB_20120810", op, &body, "1.0", kinds::DYNAMODB)
    }

    /// Create the lease table when missing.
    pub fn ensure_table(&self) -> Result<()> {
        if self.table_ready.load(Ordering::SeqCst) {
            return Ok(());
        }
        if let Err(e) = self.ddb("DescribeTable", obj([("TableName", Value::from(self.table.clone()))])) {
            if error_code(&e).as_deref() != Some("ResourceNotFoundException") {
                return Err(e);
            }
            let r = self.ddb(
                "CreateTable",
                obj([
                    ("TableName", Value::from(self.table.clone())),
                    ("KeySchema", Value::Array(vec![obj([("AttributeName", Value::from("leaseKey")), ("KeyType", Value::from("HASH"))])])),
                    ("AttributeDefinitions", Value::Array(vec![obj([("AttributeName", Value::from("leaseKey")), ("AttributeType", Value::from("S"))])])),
                    ("BillingMode", Value::from("PAY_PER_REQUEST")),
                ]),
            );
            if let Err(e) = r {
                if error_code(&e).as_deref() != Some("ResourceInUseException") {
                    return Err(e);
                }
            }
        }
        self.table_ready.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// All leases (Scan, all pages).
    pub fn leases(&self) -> Result<Vec<Lease>> {
        let mut out = Vec::new();
        let mut start: Option<Value> = None;
        loop {
            let mut req = ObjectMap::new();
            req.insert("TableName".into(), Value::from(self.table.clone()));
            req.insert("ConsistentRead".into(), Value::Bool(true));
            if let Some(k) = start.take() {
                req.insert("ExclusiveStartKey".into(), k);
            }
            let r = self.ddb("Scan", Value::Object(req))?;
            for item in r.get("Items").and_then(|i| i.as_array()).cloned().unwrap_or_default() {
                let m = unmarshal_item(&item);
                let s = |k: &str| m.get(k).map(|v| v.to_string()).filter(|v| !v.is_empty());
                out.push(Lease {
                    shard: s("leaseKey").unwrap_or_default(),
                    checkpoint: s("checkpoint").unwrap_or_else(|| "TRIM_HORIZON".into()),
                    owner: s("leaseOwner"),
                    counter: m.get("leaseCounter").and_then(|c| c.as_i64().or_else(|| c.to_string().parse().ok())).unwrap_or(0),
                    parents: m.get("parentShardIds").and_then(|p| p.as_array()).map(|a| a.iter().map(|x| x.to_string()).collect()).unwrap_or_default(),
                });
            }
            match r.get("LastEvaluatedKey") {
                Some(k @ Value::Object(m)) if !m.is_empty() => start = Some(k.clone()),
                _ => return Ok(out),
            }
        }
    }

    /// Create a lease for a new shard (no-op when it exists).
    pub fn create_lease(&self, shard: &str, initial: &str, parents: &[String]) -> Result<()> {
        let mut item = ObjectMap::new();
        item.insert("leaseKey".into(), Value::from(shard));
        item.insert("checkpoint".into(), Value::from(initial));
        item.insert("leaseCounter".into(), Value::Int(0));
        item.insert("ownerSwitchesSinceCheckpoint".into(), Value::Int(0));
        if !parents.is_empty() {
            item.insert("parentShardIds".into(), Value::Array(parents.iter().map(|p| Value::from(p.clone())).collect()));
        }
        let r = self.ddb(
            "PutItem",
            obj([("TableName", Value::from(self.table.clone())), ("Item", marshal_item(&Value::Object(item))?), ("ConditionExpression", Value::from("attribute_not_exists(leaseKey)"))]),
        );
        match r {
            Err(e) if error_code(&e).as_deref() == Some("ConditionalCheckFailedException") => Ok(()),
            other => other.map(|_| ()),
        }
    }

    /// Conditional update of a lease; `Ok(false)` when the condition failed.
    fn update(&self, shard: &str, update: &str, condition: &str, values: Vec<(&str, Value)>) -> Result<bool> {
        let mut vals = ObjectMap::new();
        for (k, v) in values {
            vals.insert(k.to_string(), to_attribute_value(&v));
        }
        let mut req = ObjectMap::new();
        req.insert("TableName".into(), Value::from(self.table.clone()));
        req.insert("Key".into(), obj([("leaseKey", obj([("S", Value::from(shard))]))]));
        req.insert("UpdateExpression".into(), Value::from(update));
        req.insert("ConditionExpression".into(), Value::from(condition));
        if !vals.is_empty() {
            req.insert("ExpressionAttributeValues".into(), Value::Object(vals));
        }
        match self.ddb("UpdateItem", Value::Object(req)) {
            Ok(_) => Ok(true),
            Err(e) if error_code(&e).as_deref() == Some("ConditionalCheckFailedException") => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Renew an owned lease.
    pub fn renew(&self, l: &Lease) -> Result<bool> {
        self.update(&l.shard, "SET leaseCounter = leaseCounter + :one", "leaseCounter = :c AND leaseOwner = :me", vec![(":one", Value::Int(1)), (":c", Value::Int(l.counter)), (":me", Value::from(self.worker.clone()))])
    }

    /// Take an unowned / expired lease.
    pub fn take(&self, l: &Lease) -> Result<bool> {
        self.update(
            &l.shard,
            "SET leaseOwner = :me, leaseCounter = leaseCounter + :one, ownerSwitchesSinceCheckpoint = ownerSwitchesSinceCheckpoint + :one",
            "leaseCounter = :c",
            vec![(":one", Value::Int(1)), (":c", Value::Int(l.counter)), (":me", Value::from(self.worker.clone()))],
        )
    }

    /// Record a checkpoint (fails the condition when the lease was lost).
    pub fn checkpoint(&self, shard: &str, checkpoint: &str) -> Result<bool> {
        self.update(shard, "SET checkpoint = :cp, ownerSwitchesSinceCheckpoint = :zero", "leaseOwner = :me", vec![(":cp", Value::from(checkpoint)), (":zero", Value::Int(0)), (":me", Value::from(self.worker.clone()))])
    }

    /// Give up a lease.
    pub fn release(&self, shard: &str) -> Result<bool> {
        self.update(shard, "REMOVE leaseOwner", "leaseOwner = :me", vec![(":me", Value::from(self.worker.clone()))])
    }

    /// Whether a lease owned by somebody else has expired (its counter did not change for
    /// the failover time, as observed by this worker).
    fn expired(&self, l: &Lease) -> bool {
        let mut seen = self.seen.lock().unwrap();
        let e = seen.entry(l.shard.clone()).or_insert((l.counter, Instant::now()));
        if e.0 != l.counter {
            *e = (l.counter, Instant::now());
        }
        e.1.elapsed() >= self.failover
    }

    /// One coordination round: create missing leases for `shards` (`(id, parents)`), renew /
    /// take leases. Returns the leases this worker now owns, the full lease list.
    pub fn coordinate(&self, shards: &[(String, Vec<String>)], initial: &str) -> Result<(Vec<Lease>, Vec<Lease>)> {
        self.ensure_table()?;
        let mut leases = self.leases()?;
        let known: HashSet<String> = leases.iter().map(|l| l.shard.clone()).collect();
        let mut created = false;
        for (id, parents) in shards {
            if !known.contains(id) {
                self.create_lease(id, initial, parents)?;
                created = true;
            }
        }
        if created {
            leases = self.leases()?;
        }
        let mut mine = Vec::new();
        let mut candidates = Vec::new();
        let mut live_workers: HashSet<String> = HashSet::new();
        live_workers.insert(self.worker.clone());
        for l in &leases {
            let expired = l.owner.as_deref() != Some(self.worker.as_str()) && self.expired(l);
            match &l.owner {
                Some(o) if *o == self.worker => {
                    if self.renew(l)? {
                        mine.push(Lease { counter: l.counter + 1, ..l.clone() });
                    }
                }
                Some(o) if !expired => {
                    live_workers.insert(o.clone());
                }
                _ => candidates.push(l.clone()),
            }
        }
        let active = leases.iter().filter(|l| l.checkpoint != SHARD_END).count().max(1);
        let target = active.div_ceil(live_workers.len());
        let mut owned_active = mine.iter().filter(|l| l.checkpoint != SHARD_END).count();
        for l in candidates {
            let finished = l.checkpoint == SHARD_END;
            if !finished && owned_active >= target {
                continue;
            }
            if self.take(&l)? {
                if !finished {
                    owned_active += 1;
                }
                self.seen.lock().unwrap().remove(&l.shard);
                mine.push(Lease { owner: Some(self.worker.clone()), counter: l.counter + 1, ..l });
            }
        }
        Ok((mine, leases))
    }
}

/// A record pushed by an enhanced fan-out subscription.
pub type PushedRecord = (String, Value);

/// Enhanced fan-out: one `SubscribeToShard` subscription thread per shard.
pub struct FanOutConsumer {
    pub consumer_name: String,
    consumer_arn: Mutex<Option<String>>,
    pub queue: Arc<Mutex<VecDeque<PushedRecord>>>,
    running: Arc<AtomicBool>,
    /// shards with a subscription thread (running or finished)
    subscribed: Mutex<HashMap<String, (JoinHandle<()>, Arc<AtomicBool>)>>,
    /// shards that reached their end
    pub ended: Arc<Mutex<HashSet<String>>>,
    pub errors: Arc<Mutex<Vec<String>>>,
}

impl FanOutConsumer {
    pub fn new(consumer_name: String) -> FanOutConsumer {
        FanOutConsumer {
            consumer_name,
            consumer_arn: Mutex::new(None),
            queue: Arc::new(Mutex::new(VecDeque::new())),
            running: Arc::new(AtomicBool::new(true)),
            subscribed: Mutex::new(HashMap::new()),
            ended: Arc::new(Mutex::new(HashSet::new())),
            errors: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Register (or look up) the stream consumer and wait until it is ACTIVE.
    pub fn consumer_arn(&self, svc: &KinesisService) -> Result<String> {
        if let Some(a) = self.consumer_arn.lock().unwrap().clone() {
            return Ok(a);
        }
        let summary = svc.call("DescribeStreamSummary", obj([("StreamName", Value::from(svc.stream.clone()))]))?;
        let stream_arn = summary.get("StreamDescriptionSummary").and_then(|s| s.get("StreamARN")).map(|a| a.to_string()).unwrap_or_default();
        let name = Value::from(self.consumer_name.clone());
        let arn = match svc.call("RegisterStreamConsumer", obj([("StreamARN", Value::from(stream_arn.clone())), ("ConsumerName", name.clone())])) {
            Ok(r) => r.get("Consumer").and_then(|c| c.get("ConsumerARN")).map(|a| a.to_string()).unwrap_or_default(),
            Err(e) if error_code(&e).as_deref() == Some("ResourceInUseException") => String::new(),
            Err(e) => return Err(e),
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let req = if arn.is_empty() { obj([("StreamARN", Value::from(stream_arn.clone())), ("ConsumerName", name.clone())]) } else { obj([("ConsumerARN", Value::from(arn.clone()))]) };
            let d = svc.call("DescribeStreamConsumer", req)?;
            let desc = d.get("ConsumerDescription").cloned().unwrap_or_default();
            if desc.get("ConsumerStatus").map(|s| s.to_string()).as_deref() == Some("ACTIVE") {
                let a = desc.get("ConsumerARN").map(|a| a.to_string()).unwrap_or_default();
                *self.consumer_arn.lock().unwrap() = Some(a.clone());
                return Ok(a);
            }
            if Instant::now() > deadline {
                return Err(crate::util::cloud_error(kinds::KINESIS, format!("Stream consumer {} did not become ACTIVE", self.consumer_name)));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Start subscription threads for shards not subscribed yet (children only after their
    /// parents ended).
    pub fn ensure_subscriptions(&self, svc: &KinesisService, shards: &[(String, Vec<String>)], start: Value) -> Result<()> {
        let arn = self.consumer_arn(svc)?;
        let mut subs = self.subscribed.lock().unwrap();
        for (shard, parents) in shards {
            if subs.contains_key(shard) {
                continue;
            }
            let ended = self.ended.lock().unwrap().clone();
            let known: HashSet<&String> = shards.iter().map(|(s, _)| s).collect();
            if parents.iter().any(|p| known.contains(p) && !ended.contains(p)) {
                continue;
            }
            let stop = Arc::new(AtomicBool::new(false));
            let (svc2, arn2, shard2, queue, ended2, running, stop2, errors, start2) =
                (svc.aws.clone(), arn.clone(), shard.clone(), self.queue.clone(), self.ended.clone(), self.running.clone(), stop.clone(), self.errors.clone(), start.clone());
            let h = std::thread::Builder::new()
                .name(format!("kinesis-efo-{}", shard))
                .spawn(move || subscription_loop(&svc2, &arn2, &shard2, start2, &queue, &ended2, &running, &stop2, &errors))
                .map_err(|e| camel_core::error::CamelError::io(e.to_string()))?;
            subs.insert(shard.clone(), (h, stop));
        }
        Ok(())
    }

    /// Allow subscriptions again after [`FanOutConsumer::stop`].
    pub fn start(&self) {
        self.running.store(true, Ordering::SeqCst);
    }

    /// Stop all subscriptions.
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        for (_, (h, stop)) in self.subscribed.lock().unwrap().drain() {
            stop.store(true, Ordering::SeqCst);
            let _ = h.join();
        }
    }
}

impl Drop for FanOutConsumer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[allow(clippy::too_many_arguments)]
fn subscription_loop(
    aws: &AwsConfig,
    consumer_arn: &str,
    shard: &str,
    start: Value,
    queue: &Mutex<VecDeque<PushedRecord>>,
    ended: &Mutex<HashSet<String>>,
    running: &AtomicBool,
    stop: &AtomicBool,
    errors: &Mutex<Vec<String>>,
) {
    let mut position = start;
    let mut backoff = Duration::from_millis(100);
    let alive = || running.load(Ordering::SeqCst) && !stop.load(Ordering::SeqCst);
    while alive() {
        let mut finished = false;
        let mut continuation: Option<String> = None;
        let req = obj([("ConsumerARN", Value::from(consumer_arn)), ("ShardId", Value::from(shard)), ("StartingPosition", position.clone())]);
        let r = aws.h2_event_stream("kinesis", "kinesis", "Kinesis_20131202.SubscribeToShard", &req, "1.1", kinds::KINESIS, &|| !alive(), &mut |m| {
            if m.event_type() != "SubscribeToShardEvent" {
                return Ok(true);
            }
            let v = m.json()?;
            let records = v.get("Records").and_then(|r| r.as_array()).cloned().unwrap_or_default();
            if !records.is_empty() {
                let mut q = queue.lock().unwrap();
                for rec in records {
                    q.push_back((shard.to_string(), rec));
                }
            }
            match v.get("ContinuationSequenceNumber").map(|c| c.to_string()).filter(|c| !c.is_empty()) {
                Some(c) => continuation = Some(c),
                None => finished = true,
            }
            Ok(!finished)
        });
        if let Some(c) = continuation {
            position = obj([("Type", Value::from("AFTER_SEQUENCE_NUMBER")), ("SequenceNumber", Value::from(c))]);
        }
        if finished {
            ended.lock().unwrap().insert(shard.to_string());
            return;
        }
        match r {
            Ok(()) => backoff = Duration::from_millis(100),
            Err(e) => {
                if !alive() {
                    return;
                }
                errors.lock().unwrap().push(e.to_string());
                let end = Instant::now() + backoff;
                while alive() && Instant::now() < end {
                    std::thread::sleep(Duration::from_millis(10));
                }
                backoff = (backoff * 2).min(Duration::from_secs(5));
            }
        }
    }
}

/// A random worker identifier.
pub fn worker_id() -> String {
    format!("camel-worker-{}", random_hex(8))
}

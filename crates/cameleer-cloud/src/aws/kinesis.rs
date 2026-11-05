//! The `aws2-kinesis` component (Camel `camel-aws2-kinesis`) over the Kinesis Data Streams
//! AWS JSON 1.1 protocol (or its CBOR flavour with `cborEnabled=true`).
//!
//! `aws2-kinesis://streamName?...`
//!
//! Producer: `PutRecord` of the body with the partition key of the
//! `CamelAwsKinesisPartitionKey` header (required) and the optional
//! `CamelAwsKinesisSequenceNumber` (`SequenceNumberForOrdering`); sets
//! `CamelAwsKinesisSequenceNumber` and `CamelAwsKinesisShardId`. An array body is sent with
//! `PutRecords` (each element a record body, or an object `{data, partitionKey}`); the body
//! then becomes the list of `{sequenceNumber, shardId, errorCode}` results and
//! `CamelAwsKinesisFailedRecordCount` is set. Cameleer extension: `operation` /
//! `CamelAwsKinesisOperation` = `putRecord` (default), `putRecords`, `createStream`
//! (`shardCount`), `describeStream`, `listShards`, `deleteStream`.
//!
//! Consumer: lists the shards (`ListShards`) on each poll, keeps one shard iterator per shard
//! (`GetShardIterator` with `iteratorType` = `TRIM_HORIZON` (default), `LATEST`,
//! `AT_SEQUENCE_NUMBER` / `AFTER_SEQUENCE_NUMBER` (`sequenceNumber`) or `AT_TIMESTAMP`
//! (`messageTimestamp`, epoch millis)) and reads up to `maxResultsPerRequest` (default 1)
//! records per shard with `GetRecords`. `shardId` restricts the consumer to one shard. Body =
//! record data (bytes), headers `CamelAwsKinesisSequenceNumber`,
//! `CamelAwsKinesisApproximateArrivalTimestamp`, `CamelAwsKinesisPartitionKey`,
//! `CamelAwsKinesisShardId`, `CamelMessageTimestamp`. A closed shard (no next iterator) is
//! handled per `shardClosed`: `ignore` (default, logged), `silent` or `fail`.
//!
//! * `resumeStrategy=#bean`: a [`KinesisResumeStrategy`] (e.g. [`InMemoryResumeStrategy`])
//!   giving the sequence number to resume each shard after (`AFTER_SEQUENCE_NUMBER`) and
//!   receiving the sequence number of every successfully processed record.
//! * `useKclConsumers=true`: KCL lease-table consumers (`applicationName`,
//!   `workerIdentifier`, `failoverTimeMillis`), see [`super::kinesis_kcl::LeaseCoordinator`].
//!   `kclDisableCloudwatchMetricsExport` is accepted (no metrics are published).
//! * `enhancedFanOut=true` (Cameleer, with `consumerName`, default `camel-{stream}`):
//!   enhanced fan-out `SubscribeToShard` HTTP/2 event streams, see
//!   [`super::kinesis_kcl::FanOutConsumer`].
//! * `cborEnabled=true`: requests and responses in CBOR (`application/x-amz-cbor-1.1`, record
//!   data as byte strings) like the AWS SDK; JSON otherwise (default here).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cameleer_core::component::scheduled::ScheduledPollConfig;
use cameleer_core::context::CamelContext;
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::spi::Processor;
use cameleer_core::value::{ObjectMap, Value};

use super::kinesis_kcl::{worker_id, FanOutConsumer, LeaseCoordinator, SHARD_END};
use super::{service_error, sigv4, AwsConfig};
use crate::endpoint::{operation, process, set_batch, unsupported, CloudService};
use crate::http::{Request, Response};
use crate::util::{base64_decode, base64_encode, cloud_error, header_str, kinds, now_secs, obj, opt_i64, Opts};

/// Header names (Camel `Kinesis2Constants`).
pub mod headers {
    pub const SEQUENCE_NUMBER: &str = "CamelAwsKinesisSequenceNumber";
    pub const APPROX_ARRIVAL_TIME: &str = "CamelAwsKinesisApproximateArrivalTimestamp";
    pub const PARTITION_KEY: &str = "CamelAwsKinesisPartitionKey";
    pub const SHARD_ID: &str = "CamelAwsKinesisShardId";
    pub const MESSAGE_TIMESTAMP: &str = "CamelMessageTimestamp";
    /// Cameleer extensions.
    pub const OPERATION: &str = "CamelAwsKinesisOperation";
    pub const FAILED_RECORD_COUNT: &str = "CamelAwsKinesisFailedRecordCount";
}

/// Where to resume reading a shard (Camel's Kinesis resume strategy / adapter).
pub trait KinesisResumeStrategy: Send + Sync {
    /// The sequence number after which `shard` of `stream` resumes, if known.
    fn resume_sequence(&self, stream: &str, shard: &str) -> Option<String>;
    /// Called after a record was processed successfully.
    fn update_last_offset(&self, stream: &str, shard: &str, sequence_number: &str);
}

/// An in-memory [`KinesisResumeStrategy`] (bind as `Arc<dyn KinesisResumeStrategy>`).
#[derive(Default)]
pub struct InMemoryResumeStrategy {
    offsets: Mutex<HashMap<(String, String), String>>,
}

impl InMemoryResumeStrategy {
    /// Preset the offset of a shard.
    pub fn set(&self, stream: &str, shard: &str, sequence_number: &str) {
        self.offsets.lock().unwrap().insert((stream.into(), shard.into()), sequence_number.into());
    }
    /// The recorded offset of a shard.
    pub fn get(&self, stream: &str, shard: &str) -> Option<String> {
        self.offsets.lock().unwrap().get(&(stream.to_string(), shard.to_string())).cloned()
    }
}

impl KinesisResumeStrategy for InMemoryResumeStrategy {
    fn resume_sequence(&self, stream: &str, shard: &str) -> Option<String> {
        self.get(stream, shard)
    }
    fn update_last_offset(&self, stream: &str, shard: &str, sequence_number: &str) {
        self.set(stream, shard, sequence_number)
    }
}

/// The Kinesis endpoint configuration and client.
pub struct KinesisService {
    pub aws: AwsConfig,
    pub stream: String,
    pub operation: Option<String>,
    pub iterator_type: String,
    pub sequence_number: Option<String>,
    pub message_timestamp: Option<i64>,
    pub shard_id: Option<String>,
    pub max_results_per_request: i64,
    pub shard_closed: String,
    pub shard_count: i64,
    pub cbor: bool,
    pub scheduler: ScheduledPollConfig,
    pub resume: Option<Arc<dyn KinesisResumeStrategy>>,
    pub kcl: Option<LeaseCoordinator>,
    pub fan_out: Option<FanOutConsumer>,
    iterators: Mutex<HashMap<String, Option<String>>>,
}

/// Factory for the component.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let stream = remaining.rsplit('/').next().unwrap_or(remaining).to_string();
    if stream.is_empty() {
        return Err(CamelError::illegal_argument("Stream name must be specified: aws2-kinesis://streamName"));
    }
    let aws = AwsConfig::from_opts(o)?;
    let iterator_type = o.string("iteratorType", "TRIM_HORIZON").to_ascii_uppercase();
    if !matches!(iterator_type.as_str(), "TRIM_HORIZON" | "LATEST" | "AT_SEQUENCE_NUMBER" | "AFTER_SEQUENCE_NUMBER" | "AT_TIMESTAMP") {
        return Err(CamelError::illegal_argument(format!("Invalid iteratorType: {}", iterator_type)));
    }
    let sequence_number = o.get("sequenceNumber");
    if iterator_type.contains("SEQUENCE_NUMBER") && sequence_number.is_none() {
        return Err(CamelError::illegal_argument(format!("iteratorType {} requires the sequenceNumber option", iterator_type)));
    }
    let shard_closed = o.string("shardClosed", "ignore").to_ascii_lowercase();
    if !matches!(shard_closed.as_str(), "ignore" | "fail" | "silent") {
        return Err(CamelError::illegal_argument(format!("Invalid shardClosed: {} (ignore, fail or silent)", shard_closed)));
    }
    let resume = match o.get("resumeStrategy") {
        Some(r) => Some(
            o.context()
                .registry()
                .lookup::<Arc<dyn KinesisResumeStrategy>>(r.trim_start_matches('#'))
                .map(|a| (*a).clone())
                .ok_or_else(|| CamelError::illegal_argument(format!("No KinesisResumeStrategy bean {} in the registry", r)))?,
        ),
        None => None,
    };
    let kcl = if o.bool("useKclConsumers", false)? {
        let table = o.get("applicationName").unwrap_or_else(|| format!("{}-camel", stream));
        let worker = o.get("workerIdentifier").unwrap_or_else(worker_id);
        let failover = Duration::from_millis(o.i64("failoverTimeMillis", 10_000)?.max(0) as u64);
        Some(LeaseCoordinator::new(aws.clone(), table, worker, failover))
    } else {
        o.ignore(&["applicationName", "workerIdentifier", "failoverTimeMillis"]);
        None
    };
    let fan_out = if o.bool("enhancedFanOut", false)? {
        if kcl.is_some() {
            return Err(CamelError::illegal_argument("useKclConsumers and enhancedFanOut cannot be combined"));
        }
        Some(FanOutConsumer::new(o.get("consumerName").unwrap_or_else(|| format!("camel-{}", stream))))
    } else {
        o.ignore(&["consumerName"]);
        None
    };
    let s = KinesisService {
        operation: o.get("operation"),
        message_timestamp: opt_i64(o, "messageTimestamp")?,
        shard_id: o.get("shardId"),
        max_results_per_request: o.i64("maxResultsPerRequest", 1)?.clamp(1, 10000),
        shard_count: o.i64("shardCount", 1)?,
        cbor: o.bool("cborEnabled", false)?,
        scheduler: ScheduledPollConfig::from_params(o.params, Duration::from_millis(1000), Duration::from_millis(500))?,
        iterators: Mutex::new(HashMap::new()),
        iterator_type,
        sequence_number,
        shard_closed,
        stream,
        resume,
        kcl,
        fan_out,
        aws,
    };
    o.ignore(&["asyncClient", "kclDisableCloudwatchMetricsExport", "amazonKinesisClient", "dynamoDbAsyncClient", "cloudWatchAsyncClient"]);
    Ok(Arc::new(s))
}

/// CBOR form of a request (base64 `Data` → byte strings).
fn to_cbor_value(v: &Value) -> Value {
    match v {
        Value::Array(a) => Value::Array(a.iter().map(to_cbor_value).collect()),
        Value::Object(m) => Value::Object(m.iter().map(|(k, v)| (k.clone(), if k == "Data" { Value::Bytes(base64_decode(&v.to_string()).unwrap_or_default()) } else { to_cbor_value(v) })).collect()),
        other => other.clone(),
    }
}

/// JSON form of a CBOR response (byte strings → base64).
fn from_cbor_value(v: Value) -> Value {
    match v {
        Value::Bytes(b) => Value::from(base64_encode(&b)),
        Value::Array(a) => Value::Array(a.into_iter().map(from_cbor_value).collect()),
        Value::Object(m) => Value::Object(m.into_iter().map(|(k, v)| (k, from_cbor_value(v))).collect()),
        other => other,
    }
}

impl KinesisService {
    /// Invoke a Kinesis API action.
    pub fn call(&self, action: &str, body: Value) -> Result<Value> {
        if !self.cbor {
            return self.aws.json_call("kinesis", "kinesis", "Kinesis_20131202", action, &body, "1.1", kinds::KINESIS);
        }
        let mut req = Request::new("POST", self.aws.endpoint("kinesis")?)
            .header("Content-Type", "application/x-amz-cbor-1.1")
            .header("Accept", "application/x-amz-cbor-1.1")
            .header("X-Amz-Target", format!("Kinesis_20131202.{}", action))
            .body(cameleer_dataformats::cbor::encode(&to_cbor_value(&body)));
        sigv4::sign_request(&mut req, &self.aws.credentials()?, &self.aws.region, "kinesis", now_secs());
        let resp = self.aws.http().send(&req)?;
        let v = if resp.body.is_empty() { Value::Object(ObjectMap::new()) } else { from_cbor_value(cameleer_dataformats::cbor::decode(&resp.body).map_err(|e| cloud_error(kinds::KINESIS, format!("Invalid CBOR response: {}", e)))?) };
        if resp.is_success() {
            Ok(v)
        } else {
            let json = Response { status: resp.status, headers: resp.headers.clone(), body: v.to_json().into_bytes() };
            Err(service_error(kinds::KINESIS, "kinesis", &json))
        }
    }

    fn stream_name(&self) -> Value {
        Value::from(self.stream.clone())
    }

    /// The stream's shards (all pages of `ListShards`).
    pub fn list_shards(&self) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let req = match &token {
                Some(t) => obj([("NextToken", Value::from(t.clone()))]),
                None => obj([("StreamName", self.stream_name())]),
            };
            let r = self.call("ListShards", req)?;
            out.extend(r.get("Shards").and_then(|s| s.as_array()).cloned().unwrap_or_default());
            token = r.get("NextToken").map(|t| t.to_string()).filter(|t| !t.is_empty());
            if token.is_none() {
                return Ok(out);
            }
        }
    }

    /// `(shard id, parent shard ids)` of the shards to consume (honouring `shardId`).
    fn shard_parents(&self) -> Result<Vec<(String, Vec<String>)>> {
        let all: Vec<(String, Vec<String>)> = self
            .list_shards()?
            .iter()
            .map(|s| {
                let id = s.get("ShardId").map(|i| i.to_string()).unwrap_or_default();
                let parents = ["ParentShardId", "AdjacentParentShardId"].iter().filter_map(|k| s.get(k).map(|p| p.to_string()).filter(|p| !p.is_empty())).collect();
                (id, parents)
            })
            .collect();
        Ok(match &self.shard_id {
            Some(only) => all.into_iter().filter(|(id, _)| id == only).collect(),
            None => all,
        })
    }

    fn record_entry(v: &Value) -> (Vec<u8>, Option<String>) {
        match v {
            Value::Object(m) if m.contains_key("data") || m.contains_key("Data") => {
                let data = m.get("data").or_else(|| m.get("Data")).cloned().unwrap_or_default();
                let pk = m.get("partitionKey").or_else(|| m.get("PartitionKey")).map(|p| p.to_string());
                (data.to_bytes(), pk)
            }
            other => (other.to_bytes(), None),
        }
    }

    fn put_records(&self, ex: &mut Exchange, items: Vec<Value>) -> Result<()> {
        let default_pk = header_str(ex, headers::PARTITION_KEY);
        let mut records = Vec::new();
        for (i, item) in items.iter().enumerate() {
            let (data, pk) = Self::record_entry(item);
            let pk = pk.or_else(|| default_pk.clone()).unwrap_or_else(|| i.to_string());
            records.push(obj([("Data", Value::from(base64_encode(&data))), ("PartitionKey", Value::from(pk))]));
        }
        let r = self.call("PutRecords", obj([("StreamName", self.stream_name()), ("Records", Value::Array(records))]))?;
        let results: Vec<Value> = r
            .get("Records")
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|e| {
                obj([
                    ("sequenceNumber", e.get("SequenceNumber").cloned().unwrap_or_default()),
                    ("shardId", e.get("ShardId").cloned().unwrap_or_default()),
                    ("errorCode", e.get("ErrorCode").cloned().unwrap_or_default()),
                    ("errorMessage", e.get("ErrorMessage").cloned().unwrap_or_default()),
                ])
            })
            .collect();
        ex.set_header(headers::FAILED_RECORD_COUNT, r.get("FailedRecordCount").and_then(|c| c.as_i64()).unwrap_or(0));
        ex.set_body(Value::Array(results));
        Ok(())
    }

    /// `GetShardIterator` of `kind` (`AFTER_SEQUENCE_NUMBER` etc.).
    fn get_iterator(&self, shard: &str, kind: &str, seq: Option<&str>) -> Result<String> {
        let mut req = ObjectMap::new();
        req.insert("StreamName".to_string(), self.stream_name());
        req.insert("ShardId".to_string(), Value::from(shard));
        req.insert("ShardIteratorType".to_string(), Value::from(kind));
        if let Some(sn) = seq {
            req.insert("StartingSequenceNumber".into(), Value::from(sn));
        }
        if kind == "AT_TIMESTAMP" {
            req.insert("Timestamp".into(), Value::Float(self.message_timestamp.unwrap_or(0) as f64 / 1000.0));
        }
        let r = self.call("GetShardIterator", Value::Object(req))?;
        Ok(r.get("ShardIterator").map(|v| v.to_string()).unwrap_or_default())
    }

    fn iterator_for(&self, shard: &str) -> Result<String> {
        if let Some(seq) = self.resume.as_ref().and_then(|r| r.resume_sequence(&self.stream, shard)) {
            return self.get_iterator(shard, "AFTER_SEQUENCE_NUMBER", Some(&seq));
        }
        let seq = if self.iterator_type.contains("SEQUENCE_NUMBER") { self.sequence_number.as_deref() } else { None };
        self.get_iterator(shard, &self.iterator_type, seq)
    }

    /// The `StartingPosition` of a new enhanced fan-out subscription.
    fn starting_position(&self) -> Value {
        let mut m = ObjectMap::new();
        m.insert("Type".into(), Value::from(self.iterator_type.clone()));
        if self.iterator_type.contains("SEQUENCE_NUMBER") {
            m.insert("SequenceNumber".into(), Value::from(self.sequence_number.clone().unwrap_or_default()));
        }
        if self.iterator_type == "AT_TIMESTAMP" {
            m.insert("Timestamp".into(), Value::Float(self.message_timestamp.unwrap_or(0) as f64 / 1000.0));
        }
        Value::Object(m)
    }

    fn record_exchange(ctx: &Option<CamelContext>, shard: &str, rec: &Value) -> Exchange {
        let mut ex = Exchange::new(ctx.clone());
        let data = rec.get("Data").and_then(|d| base64_decode(&d.to_string())).unwrap_or_default();
        ex.set_body(Value::Bytes(data));
        ex.set_header(headers::SEQUENCE_NUMBER, rec.get("SequenceNumber").cloned().unwrap_or_default());
        ex.set_header(headers::PARTITION_KEY, rec.get("PartitionKey").cloned().unwrap_or_default());
        ex.set_header(headers::SHARD_ID, shard.to_string());
        if let Some(ts) = rec.get("ApproximateArrivalTimestamp").and_then(|t| t.as_f64()) {
            let millis = (ts * 1000.0) as i64;
            ex.set_header(headers::APPROX_ARRIVAL_TIME, millis);
            ex.set_header(headers::MESSAGE_TIMESTAMP, millis);
        }
        ex
    }

    fn closed_shard(&self, shard: &str, logger: &str) -> Result<()> {
        match self.shard_closed.as_str() {
            "fail" => Err(cloud_error(kinds::KINESIS, format!("Shard {} of stream {} is closed", shard, self.stream))),
            "ignore" => {
                cameleer_core::camel_warn!(logger, "Shard {} of stream {} is closed; it will not be read any more", shard, self.stream);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn poll_kcl(&self, kcl: &LeaseCoordinator, ctx: Option<CamelContext>, processor: &dyn Processor, logger: &str) -> Result<usize> {
        let shards = self.shard_parents()?;
        let initial = match self.iterator_type.as_str() {
            "LATEST" => "LATEST",
            "AT_TIMESTAMP" => "AT_TIMESTAMP",
            _ => "TRIM_HORIZON",
        };
        let (mine, all) = kcl.coordinate(&shards, initial)?;
        let ended: HashSet<String> = all.iter().filter(|l| l.checkpoint == SHARD_END).map(|l| l.shard.clone()).collect();
        let known: HashSet<String> = all.iter().map(|l| l.shard.clone()).collect();
        let owned: HashSet<String> = mine.iter().map(|l| l.shard.clone()).collect();
        self.iterators.lock().unwrap().retain(|s, _| owned.contains(s));
        let mut total = 0;
        for lease in mine {
            if lease.checkpoint == SHARD_END || self.shard_id.as_ref().is_some_and(|only| *only != lease.shard) {
                continue;
            }
            if lease.parents.iter().any(|p| known.contains(p) && !ended.contains(p)) {
                continue; // the parent shard is not finished yet
            }
            let cached = self.iterators.lock().unwrap().get(&lease.shard).cloned().flatten();
            let iterator = match cached {
                Some(it) => it,
                None => match lease.checkpoint.as_str() {
                    k @ ("TRIM_HORIZON" | "LATEST" | "AT_TIMESTAMP") => self.get_iterator(&lease.shard, k, None)?,
                    seq => self.get_iterator(&lease.shard, "AFTER_SEQUENCE_NUMBER", Some(seq))?,
                },
            };
            let r = self.call("GetRecords", obj([("ShardIterator", Value::from(iterator)), ("Limit", Value::Int(self.max_results_per_request))]))?;
            let next = r.get("NextShardIterator").map(|n| n.to_string()).filter(|n| !n.is_empty());
            let records = r.get("Records").and_then(|a| a.as_array()).cloned().unwrap_or_default();
            let (mut last, mut failed) = (None, false);
            let n = records.len();
            for (i, rec) in records.iter().enumerate() {
                let mut ex = Self::record_exchange(&ctx, &lease.shard, rec);
                set_batch(&mut ex, i, n);
                total += 1;
                if process(logger, processor, &mut ex) {
                    last = rec.get("SequenceNumber").map(|s| s.to_string());
                } else {
                    failed = true;
                    break;
                }
            }
            let mut still_owned = true;
            if let Some(seq) = &last {
                still_owned = kcl.checkpoint(&lease.shard, seq)?;
            }
            if failed || !still_owned {
                // re-read from the checkpoint next time (or give up a lost lease)
                self.iterators.lock().unwrap().remove(&lease.shard);
                continue;
            }
            match next {
                Some(it) => {
                    self.iterators.lock().unwrap().insert(lease.shard.clone(), Some(it));
                }
                None => {
                    kcl.checkpoint(&lease.shard, SHARD_END)?;
                    self.iterators.lock().unwrap().remove(&lease.shard);
                }
            }
        }
        Ok(total)
    }

    fn poll_fan_out(&self, fo: &FanOutConsumer, ctx: Option<CamelContext>, processor: &dyn Processor, logger: &str) -> Result<usize> {
        let shards = self.shard_parents()?;
        fo.ensure_subscriptions(self, &shards, self.starting_position())?;
        for e in fo.errors.lock().unwrap().drain(..) {
            cameleer_core::camel_warn!(logger, "Kinesis SubscribeToShard failed: {}; resubscribing", e);
        }
        let batch: Vec<(String, Value)> = fo.queue.lock().unwrap().drain(..).collect();
        let total = batch.len();
        for (i, (shard, rec)) in batch.into_iter().enumerate() {
            let mut ex = Self::record_exchange(&ctx, &shard, &rec);
            set_batch(&mut ex, i, total);
            if process(logger, processor, &mut ex) {
                if let (Some(r), Some(seq)) = (&self.resume, rec.get("SequenceNumber")) {
                    r.update_last_offset(&self.stream, &shard, &seq.to_string());
                }
            }
        }
        Ok(total)
    }
}

impl CloudService for KinesisService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let default = if matches!(ex.body(), Value::Array(_)) { "putRecords" } else { "putRecord" };
        let op = operation(ex, headers::OPERATION, &self.operation, default);
        match op.as_str() {
            "putRecord" => {
                let pk = header_str(ex, headers::PARTITION_KEY)
                    .ok_or_else(|| CamelError::illegal_argument("putRecord requires the CamelAwsKinesisPartitionKey header"))?;
                let mut req = ObjectMap::new();
                req.insert("StreamName".to_string(), self.stream_name());
                req.insert("Data".to_string(), Value::from(base64_encode(&ex.body().to_bytes())));
                req.insert("PartitionKey".to_string(), Value::from(pk));
                if let Some(sn) = header_str(ex, headers::SEQUENCE_NUMBER) {
                    req.insert("SequenceNumberForOrdering".into(), Value::from(sn));
                }
                let r = self.call("PutRecord", Value::Object(req))?;
                ex.set_header(headers::SEQUENCE_NUMBER, r.get("SequenceNumber").cloned().unwrap_or_default());
                ex.set_header(headers::SHARD_ID, r.get("ShardId").cloned().unwrap_or_default());
            }
            "putRecords" => {
                let items = match ex.body() {
                    Value::Array(a) => a.clone(),
                    other => vec![other.clone()],
                };
                self.put_records(ex, items)?;
            }
            "createStream" => {
                self.call("CreateStream", obj([("StreamName", self.stream_name()), ("ShardCount", Value::Int(self.shard_count))]))?;
            }
            "describeStream" => {
                let r = self.call("DescribeStream", obj([("StreamName", self.stream_name())]))?;
                ex.set_body(r.get("StreamDescription").cloned().unwrap_or_default());
            }
            "listShards" => {
                let shards = self.list_shards()?;
                ex.set_body(Value::Array(shards));
            }
            "deleteStream" => {
                self.call("DeleteStream", obj([("StreamName", self.stream_name())]))?;
            }
            other => return Err(unsupported("aws2-kinesis", other)),
        }
        Ok(())
    }

    fn scheduler(&self) -> Option<&ScheduledPollConfig> {
        Some(&self.scheduler)
    }

    fn on_start(&self) -> Result<()> {
        if let Some(fo) = &self.fan_out {
            fo.start();
        }
        Ok(())
    }

    fn on_stop(&self) -> Result<()> {
        if let Some(fo) = &self.fan_out {
            fo.stop();
        }
        if let Some(kcl) = &self.kcl {
            // graceful shutdown: give the leases back
            let owned: Vec<String> = self.iterators.lock().unwrap().drain().map(|(s, _)| s).collect();
            if let Ok(leases) = kcl.leases() {
                for l in leases.iter().filter(|l| l.owner.as_deref() == Some(kcl.worker.as_str()) || owned.contains(&l.shard)) {
                    let _ = kcl.release(&l.shard);
                }
            }
        }
        Ok(())
    }

    fn poll(&self, ctx: Option<CamelContext>, processor: &dyn Processor, logger: &str) -> Result<usize> {
        if let Some(kcl) = &self.kcl {
            return self.poll_kcl(kcl, ctx, processor, logger);
        }
        if let Some(fo) = &self.fan_out {
            return self.poll_fan_out(fo, ctx, processor, logger);
        }
        let shards: Vec<String> = match &self.shard_id {
            Some(s) => vec![s.clone()],
            None => self.list_shards()?.iter().filter_map(|s| s.get("ShardId").map(|i| i.to_string())).collect(),
        };
        let mut batch: Vec<(String, Value)> = Vec::new();
        for shard in shards {
            let known = self.iterators.lock().unwrap().get(&shard).cloned();
            let iterator = match known {
                Some(None) => continue, // closed shard, already reported
                Some(Some(it)) => it,
                None => self.iterator_for(&shard)?,
            };
            let r = self.call("GetRecords", obj([("ShardIterator", Value::from(iterator)), ("Limit", Value::Int(self.max_results_per_request))]))?;
            let next = r.get("NextShardIterator").map(|n| n.to_string()).filter(|n| !n.is_empty());
            if next.is_none() {
                self.closed_shard(&shard, logger)?;
            }
            self.iterators.lock().unwrap().insert(shard.clone(), next);
            for rec in r.get("Records").and_then(|a| a.as_array()).cloned().unwrap_or_default() {
                batch.push((shard.clone(), rec));
            }
        }
        let total = batch.len();
        for (i, (shard, rec)) in batch.into_iter().enumerate() {
            let mut ex = Self::record_exchange(&ctx, &shard, &rec);
            set_batch(&mut ex, i, total);
            if process(logger, processor, &mut ex) {
                if let (Some(r), Some(seq)) = (&self.resume, rec.get("SequenceNumber")) {
                    r.update_last_offset(&self.stream, &shard, &seq.to_string());
                }
            }
        }
        Ok(total)
    }
}

//! The `aws2-kinesis` component (Camel `camel-aws2-kinesis`) over the Kinesis Data Streams
//! AWS JSON 1.1 protocol.
//!
//! `aws2-kinesis://streamName?...`
//!
//! Producer: `PutRecord` of the body with the partition key of the
//! `CamelAwsKinesisPartitionKey` header (required) and the optional
//! `CamelAwsKinesisSequenceNumber` (`SequenceNumberForOrdering`); sets
//! `CamelAwsKinesisSequenceNumber` and `CamelAwsKinesisShardId`. An array body is sent with
//! `PutRecords` (each element a record body, or an object `{data, partitionKey}`); the body
//! then becomes the list of `{sequenceNumber, shardId, errorCode}` results and
//! `CamelAwsKinesisFailedRecordCount` is set. rust-camel extension: `operation` /
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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camel_core::component::scheduled::ScheduledPollConfig;
use camel_core::context::CamelContext;
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::Processor;
use camel_core::value::{ObjectMap, Value};

use super::AwsConfig;
use crate::endpoint::{operation, process, set_batch, unsupported, CloudService};
use crate::util::{base64_decode, base64_encode, cloud_error, header_str, kinds, obj, opt_i64, Opts};

/// Header names (Camel `Kinesis2Constants`).
pub mod headers {
    pub const SEQUENCE_NUMBER: &str = "CamelAwsKinesisSequenceNumber";
    pub const APPROX_ARRIVAL_TIME: &str = "CamelAwsKinesisApproximateArrivalTimestamp";
    pub const PARTITION_KEY: &str = "CamelAwsKinesisPartitionKey";
    pub const SHARD_ID: &str = "CamelAwsKinesisShardId";
    pub const MESSAGE_TIMESTAMP: &str = "CamelMessageTimestamp";
    /// rust-camel extensions.
    pub const OPERATION: &str = "CamelAwsKinesisOperation";
    pub const FAILED_RECORD_COUNT: &str = "CamelAwsKinesisFailedRecordCount";
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
    pub scheduler: ScheduledPollConfig,
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
    let s = KinesisService {
        operation: o.get("operation"),
        message_timestamp: opt_i64(o, "messageTimestamp")?,
        shard_id: o.get("shardId"),
        max_results_per_request: o.i64("maxResultsPerRequest", 1)?.clamp(1, 10000),
        shard_count: o.i64("shardCount", 1)?,
        scheduler: ScheduledPollConfig::from_params(o.params, Duration::from_millis(1000), Duration::from_millis(500))?,
        iterators: Mutex::new(HashMap::new()),
        iterator_type,
        sequence_number,
        shard_closed,
        stream,
        aws,
    };
    o.ignore(&["asyncClient", "useKclConsumers", "cborEnabled", "kclDisableCloudwatchMetricsExport", "resumeStrategy", "applicationName"]);
    Ok(Arc::new(s))
}

impl KinesisService {
    /// Invoke a Kinesis API action.
    pub fn call(&self, action: &str, body: Value) -> Result<Value> {
        self.aws.json_call("kinesis", "kinesis", "Kinesis_20131202", action, &body, "1.1", kinds::KINESIS)
    }

    fn stream_name(&self) -> Value {
        Value::from(self.stream.clone())
    }

    /// Ids of the stream's shards (all pages of `ListShards`).
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

    fn iterator_for(&self, shard: &str) -> Result<String> {
        let mut req = ObjectMap::new();
        req.insert("StreamName".to_string(), self.stream_name());
        req.insert("ShardId".to_string(), Value::from(shard));
        req.insert("ShardIteratorType".to_string(), Value::from(self.iterator_type.clone()));
        if let Some(sn) = &self.sequence_number {
            if self.iterator_type.contains("SEQUENCE_NUMBER") {
                req.insert("StartingSequenceNumber".into(), Value::from(sn.clone()));
            }
        }
        if self.iterator_type == "AT_TIMESTAMP" {
            req.insert("Timestamp".into(), Value::Float(self.message_timestamp.unwrap_or(0) as f64 / 1000.0));
        }
        let r = self.call("GetShardIterator", Value::Object(req))?;
        Ok(r.get("ShardIterator").map(|v| v.to_string()).unwrap_or_default())
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

    fn poll(&self, ctx: Option<CamelContext>, processor: &dyn Processor, logger: &str) -> Result<usize> {
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
                match self.shard_closed.as_str() {
                    "fail" => return Err(cloud_error(kinds::KINESIS, format!("Shard {} of stream {} is closed", shard, self.stream))),
                    "ignore" => camel_core::camel_warn!(logger, "Shard {} of stream {} is closed; it will not be read any more", shard, self.stream),
                    _ => {}
                }
            }
            self.iterators.lock().unwrap().insert(shard.clone(), next);
            for rec in r.get("Records").and_then(|a| a.as_array()).cloned().unwrap_or_default() {
                batch.push((shard.clone(), rec));
            }
        }
        let total = batch.len();
        for (i, (shard, rec)) in batch.into_iter().enumerate() {
            let mut ex = Exchange::new(ctx.clone());
            let data = rec.get("Data").and_then(|d| base64_decode(&d.to_string())).unwrap_or_default();
            ex.set_body(Value::Bytes(data));
            ex.set_header(headers::SEQUENCE_NUMBER, rec.get("SequenceNumber").cloned().unwrap_or_default());
            ex.set_header(headers::PARTITION_KEY, rec.get("PartitionKey").cloned().unwrap_or_default());
            ex.set_header(headers::SHARD_ID, shard);
            if let Some(ts) = rec.get("ApproximateArrivalTimestamp").and_then(|t| t.as_f64()) {
                let millis = (ts * 1000.0) as i64;
                ex.set_header(headers::APPROX_ARRIVAL_TIME, millis);
                ex.set_header(headers::MESSAGE_TIMESTAMP, millis);
            }
            set_batch(&mut ex, i, total);
            process(logger, processor, &mut ex);
        }
        Ok(total)
    }
}

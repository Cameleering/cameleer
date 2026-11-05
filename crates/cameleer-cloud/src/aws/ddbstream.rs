//! The `aws2-ddbstream` component (Camel `camel-aws2-ddb`'s stream consumer) over the DynamoDB
//! Streams AWS JSON 1.0 API (`DynamoDBStreams_20120810`, signing name `dynamodb`).
//!
//! `aws2-ddbstream://tableName?streamIteratorType=FROM_LATEST|FROM_START|FROM_SEQUENCE_NUMBER`
//!
//! The table's `LatestStreamArn` comes from `DescribeTable`; every poll runs `DescribeStream`
//! (new child shards after a split are picked up and read from their start), keeps one shard
//! iterator per shard (`GetShardIterator`: `LATEST`, `TRIM_HORIZON` or `AFTER_SEQUENCE_NUMBER`
//! with `sequenceNumberProvider` = a sequence number), reads `GetRecords` (`maxResultsPerRequest`,
//! default 100), forgets closed shards and re-creates expired iterators after the last sequence
//! number seen. Body = the stream record (`eventID`, `eventName` INSERT/MODIFY/REMOVE,
//! `eventSource`, `awsRegion`, `dynamodb` with `Keys` / `NewImage` / `OldImage` unmarshalled to
//! plain maps, `SequenceNumber`, `SizeBytes`, `StreamViewType`); Cameleer headers
//! `CamelAwsDdbStreamEventName`, `CamelAwsDdbStreamSequenceNumber`, `CamelAwsDdbStreamShardId`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cameleer_core::component::scheduled::ScheduledPollConfig;
use cameleer_core::context::CamelContext;
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::spi::Processor;
use cameleer_core::value::{ObjectMap, Value};

use super::ddb::unmarshal_item;
use super::{error_code, AwsConfig, JsonApi};
use crate::endpoint::{process, set_batch, CloudService};
use crate::util::{kinds, obj, Opts};

/// Header names (Cameleer).
pub mod headers {
    pub const EVENT_NAME: &str = "CamelAwsDdbStreamEventName";
    pub const SEQUENCE_NUMBER: &str = "CamelAwsDdbStreamSequenceNumber";
    pub const SHARD_ID: &str = "CamelAwsDdbStreamShardId";
}

/// The DynamoDB Streams API.
pub const STREAMS_API: JsonApi = JsonApi { host: "streams.dynamodb", service: "dynamodb", target: "DynamoDBStreams_20120810", version: "1.0", kind: kinds::DYNAMODB };

#[derive(Default)]
struct State {
    stream_arn: Option<String>,
    /// shard id -> current iterator (None once closed)
    iterators: BTreeMap<String, Option<String>>,
    last_sequence: BTreeMap<String, String>,
    first_poll_done: bool,
}

/// The stream consumer configuration and state.
pub struct DdbStreamService {
    pub aws: AwsConfig,
    pub table: String,
    pub iterator_type: String,
    pub sequence_number: Option<String>,
    pub max_results: i64,
    pub scheduler: ScheduledPollConfig,
    state: Mutex<State>,
}

/// Factory for the component.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    if remaining.is_empty() {
        return Err(CamelError::illegal_argument("Table name must be specified: aws2-ddbstream://tableName"));
    }
    let aws = AwsConfig::from_opts(o)?;
    let iterator_type = o.string("streamIteratorType", "FROM_LATEST").to_ascii_uppercase();
    if !matches!(iterator_type.as_str(), "FROM_LATEST" | "FROM_START" | "FROM_SEQUENCE_NUMBER") {
        return Err(CamelError::illegal_argument(format!("Invalid streamIteratorType: {}", iterator_type)));
    }
    let sequence_number = o.get("sequenceNumberProvider");
    if iterator_type == "FROM_SEQUENCE_NUMBER" && sequence_number.is_none() {
        return Err(CamelError::illegal_argument("streamIteratorType=FROM_SEQUENCE_NUMBER requires sequenceNumberProvider"));
    }
    o.ignore(&["amazonDynamoDbStreamsClient"]);
    Ok(Arc::new(DdbStreamService {
        table: remaining.to_string(),
        iterator_type,
        sequence_number,
        max_results: o.i64("maxResultsPerRequest", 100)?.clamp(1, 1000),
        scheduler: ScheduledPollConfig::from_params(o.params, Duration::from_millis(1000), Duration::from_millis(500))?,
        state: Mutex::new(State::default()),
        aws,
    }))
}

fn unmarshal_record(r: &Value) -> Value {
    let mut out = r.as_object().cloned().unwrap_or_default();
    if let Some(Value::Object(d)) = out.get("dynamodb").cloned() {
        let mut d2 = d.clone();
        for f in ["Keys", "NewImage", "OldImage"] {
            if let Some(v) = d.get(f) {
                d2.insert(f.to_string(), unmarshal_item(v));
            }
        }
        out.insert("dynamodb".into(), Value::Object(d2));
    }
    Value::Object(out)
}

impl DdbStreamService {
    fn stream_arn(&self) -> Result<String> {
        if let Some(a) = self.state.lock().unwrap().stream_arn.clone() {
            return Ok(a);
        }
        let r = self.aws.json_call("dynamodb", "dynamodb", "DynamoDB_20120810", "DescribeTable", &obj([("TableName", Value::from(self.table.clone()))]), "1.0", kinds::DYNAMODB)?;
        let arn = r
            .get("Table")
            .and_then(|t| t.get("LatestStreamArn"))
            .map(|a| a.to_string())
            .filter(|a| !a.is_empty())
            .ok_or_else(|| CamelError::illegal_state(format!("DynamoDB table {} has no stream enabled", self.table)))?;
        self.state.lock().unwrap().stream_arn = Some(arn.clone());
        Ok(arn)
    }

    fn shard_iterator(&self, arn: &str, shard: &str, kind: &str, seq: Option<&str>) -> Result<Option<String>> {
        let mut req = ObjectMap::new();
        req.insert("StreamArn".into(), Value::from(arn));
        req.insert("ShardId".into(), Value::from(shard));
        req.insert("ShardIteratorType".into(), Value::from(kind));
        if let Some(s) = seq {
            req.insert("SequenceNumber".into(), Value::from(s));
        }
        let r = self.aws.api(&STREAMS_API, "GetShardIterator", &Value::Object(req))?;
        Ok(r.get("ShardIterator").map(|i| i.to_string()).filter(|i| !i.is_empty()))
    }
}

impl CloudService for DdbStreamService {
    fn produce(&self, _ex: &mut Exchange) -> Result<()> {
        Err(CamelError::new(cameleer_core::error::kinds::UNSUPPORTED_OPERATION, "aws2-ddbstream only supports consumers"))
    }

    fn scheduler(&self) -> Option<&ScheduledPollConfig> {
        Some(&self.scheduler)
    }

    fn poll(&self, ctx: Option<CamelContext>, processor: &dyn Processor, logger: &str) -> Result<usize> {
        let arn = self.stream_arn()?;
        // shards (paged by ExclusiveStartShardId)
        let mut shards: Vec<String> = Vec::new();
        let mut start: Option<String> = None;
        loop {
            let mut req = ObjectMap::new();
            req.insert("StreamArn".into(), Value::from(arn.clone()));
            if let Some(s) = &start {
                req.insert("ExclusiveStartShardId".into(), Value::from(s.clone()));
            }
            let r = self.aws.api(&STREAMS_API, "DescribeStream", &Value::Object(req))?;
            let d = r.get("StreamDescription").cloned().unwrap_or_default();
            shards.extend(d.get("Shards").and_then(|s| s.as_array()).into_iter().flatten().filter_map(|s| s.get("ShardId").map(|i| i.to_string())));
            start = d.get("LastEvaluatedShardId").map(|s| s.to_string()).filter(|s| !s.is_empty());
            if start.is_none() {
                break;
            }
        }
        let first = !self.state.lock().unwrap().first_poll_done;
        for shard in &shards {
            if self.state.lock().unwrap().iterators.contains_key(shard) {
                continue;
            }
            let (kind, seq) = match (first, self.iterator_type.as_str()) {
                (true, "FROM_LATEST") => ("LATEST", None),
                (true, "FROM_SEQUENCE_NUMBER") => ("AFTER_SEQUENCE_NUMBER", self.sequence_number.as_deref()),
                _ => ("TRIM_HORIZON", None),
            };
            let it = self.shard_iterator(&arn, shard, kind, seq)?;
            self.state.lock().unwrap().iterators.insert(shard.clone(), it);
        }
        self.state.lock().unwrap().first_poll_done = true;
        // read every open shard
        let mut records: Vec<(String, Value)> = Vec::new();
        let open: Vec<(String, String)> = self.state.lock().unwrap().iterators.iter().filter_map(|(s, i)| i.clone().map(|i| (s.clone(), i))).collect();
        for (shard, it) in open {
            let req = obj([("ShardIterator", Value::from(it)), ("Limit", Value::Int(self.max_results))]);
            let r = match self.aws.api(&STREAMS_API, "GetRecords", &req) {
                Ok(r) => r,
                Err(e) if error_code(&e).as_deref() == Some("ExpiredIteratorException") => {
                    let last = self.state.lock().unwrap().last_sequence.get(&shard).cloned();
                    let it = match &last {
                        Some(seq) => self.shard_iterator(&arn, &shard, "AFTER_SEQUENCE_NUMBER", Some(seq))?,
                        None => self.shard_iterator(&arn, &shard, "TRIM_HORIZON", None)?,
                    };
                    self.state.lock().unwrap().iterators.insert(shard.clone(), it);
                    continue;
                }
                Err(e) => return Err(e),
            };
            let next = r.get("NextShardIterator").map(|i| i.to_string()).filter(|i| !i.is_empty());
            for rec in r.get("Records").and_then(|a| a.as_array()).cloned().unwrap_or_default() {
                records.push((shard.clone(), rec));
            }
            self.state.lock().unwrap().iterators.insert(shard, next);
        }
        let total = records.len();
        for (i, (shard, rec)) in records.into_iter().enumerate() {
            let seq = rec.get("dynamodb").and_then(|d| d.get("SequenceNumber")).map(|s| s.to_string()).unwrap_or_default();
            let mut ex = Exchange::new(ctx.clone());
            ex.set_header(headers::EVENT_NAME, rec.get("eventName").cloned().unwrap_or_default());
            ex.set_header(headers::SEQUENCE_NUMBER, seq.clone());
            ex.set_header(headers::SHARD_ID, shard.clone());
            ex.set_body(unmarshal_record(&rec));
            set_batch(&mut ex, i, total);
            process(logger, processor, &mut ex);
            self.state.lock().unwrap().last_sequence.insert(shard, seq);
        }
        Ok(total)
    }
}

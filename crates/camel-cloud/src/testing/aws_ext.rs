//! More emulated AWS services on the [`AwsMock`] port: Kinesis, Firehose, EventBridge,
//! Secrets Manager (AWS JSON 1.1), Step Functions (JSON 1.0) and the Query APIs of SES,
//! CloudWatch, STS, IAM and EC2. Requests are SigV4-verified by the parent handler; the
//! service comes from the credential scope.

use std::collections::BTreeMap;

use camel_core::value::{ObjectMap, Value};

use super::{json_error, request_id, AwsMock, Shared, ACCOUNT};
use crate::http::{Response, ServerRequest};
use crate::util::{base64_decode, base64_encode, iso8601, iso8601_millis, md5, now_millis, now_secs, obj, parse_form, parse_json, random_hex, uuid, xml_escape};

#[derive(Clone, Debug)]
pub(super) struct KRecord {
    pub(super) seq: u128,
    pub(super) data: Vec<u8>,
    pub(super) partition_key: String,
    pub(super) arrival_millis: i64,
}

pub(super) struct KStream {
    pub(super) shards: Vec<Vec<KRecord>>,
    pub(super) closed: Vec<bool>,
    /// (parent, adjacent parent) of each shard.
    pub(super) parents: Vec<(Option<usize>, Option<usize>)>,
    created: i64,
}

fn new_stream(n: usize) -> KStream {
    KStream { shards: (0..n.max(1)).map(|_| Vec::new()).collect(), closed: vec![false; n.max(1)], parents: vec![(None, None); n.max(1)], created: now_secs() }
}

#[derive(Clone)]
struct SecretVersion {
    id: String,
    string: Option<String>,
    binary: Option<Vec<u8>>,
    created: i64,
}

struct Secret {
    arn: String,
    description: String,
    versions: Vec<SecretVersion>,
    deleted: Option<i64>,
    rotation_lambda: Option<String>,
    rotation_enabled: bool,
    created: i64,
    replicas: Vec<String>,
}

struct Rule {
    arn: String,
    pattern: Option<String>,
    state: String,
    targets: Vec<Value>,
}

pub(super) struct Machine {
    pub(super) name: String,
    pub(super) definition: String,
    pub(super) role: String,
    pub(super) created: i64,
}

pub(super) struct Execution {
    pub(super) machine: String,
    pub(super) name: String,
    pub(super) status: String,
    pub(super) input: String,
    pub(super) output: Option<String>,
    pub(super) error: Option<(String, String)>,
    pub(super) start: i64,
    pub(super) stop: Option<i64>,
}

pub(super) struct IamUser {
    pub(super) id: String,
    pub(super) arn: String,
    pub(super) created: i64,
}

/// An email accepted by the emulated SES.
#[derive(Clone, Debug, PartialEq)]
pub struct SentEmail {
    pub message_id: String,
    pub source: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub text: Option<String>,
    pub html: Option<String>,
    /// Decoded raw MIME message (SendRawEmail).
    pub raw: Option<Vec<u8>>,
    pub reply_to: Vec<String>,
}

/// A datum received by the emulated CloudWatch.
#[derive(Clone, Debug, PartialEq)]
pub struct MetricDatum {
    pub namespace: String,
    pub name: String,
    pub value: f64,
    pub unit: String,
    pub timestamp: String,
    pub dimensions: Vec<(String, String)>,
}

pub(super) struct Ec2Instance {
    pub(super) image: String,
    pub(super) itype: String,
    pub(super) state: (i64, String),
    pub(super) launch: i64,
    pub(super) key: String,
    pub(super) ip: String,
    pub(super) monitoring: bool,
    pub(super) tags: BTreeMap<String, String>,
    pub(super) groups: Vec<String>,
    pub(super) subnet: String,
    pub(super) zone: String,
    pub(super) ebs_optimized: bool,
    pub(super) kernel: String,
    pub(super) client_token: String,
}

/// State of the extra services.
#[derive(Default)]
pub(super) struct ExtState {
    pub(super) streams: BTreeMap<String, KStream>,
    seq: u128,
    firehose: BTreeMap<String, Vec<Vec<u8>>>,
    rules: BTreeMap<(String, String), Rule>,
    events: Vec<Value>,
    secrets: BTreeMap<String, Secret>,
    emails: Vec<SentEmail>,
    metrics: Vec<MetricDatum>,
    pub(super) machines: BTreeMap<String, Machine>,
    pub(super) executions: BTreeMap<String, Execution>,
    pub(super) users: BTreeMap<String, IamUser>,
    pub(super) groups: BTreeMap<String, (String, i64, Vec<String>)>,
    pub(super) access_keys: Vec<(String, String, String)>,
    pub(super) instances: BTreeMap<String, Ec2Instance>,
}

// ---------------------------------------------------------------------------
// helpers on the mock
// ---------------------------------------------------------------------------

impl AwsMock {
    // ---- Kinesis
    /// Create a stream with `shards` shards.
    pub fn create_stream(&self, name: &str, shards: usize) {
        self.lock().ext.streams.insert(name.into(), new_stream(shards));
    }
    /// Put a record directly; returns (shard id, sequence number).
    pub fn put_kinesis_record(&self, stream: &str, partition_key: &str, data: &[u8]) -> Option<(String, String)> {
        let mut s = self.lock();
        put_record(&mut s.ext, stream, partition_key, data.to_vec())
    }
    /// (partition key, data) of all records of a stream, by shard then sequence.
    pub fn kinesis_records(&self, stream: &str) -> Vec<(String, Vec<u8>)> {
        self.lock().ext.streams.get(stream).map(|k| k.shards.iter().flatten().map(|r| (r.partition_key.clone(), r.data.clone())).collect()).unwrap_or_default()
    }
    /// Reshard: closes all current shards and adds `n` new open shards.
    pub fn reshard_stream(&self, stream: &str, n: usize) {
        if let Some(k) = self.lock().ext.streams.get_mut(stream) {
            let open: Vec<usize> = (0..k.shards.len()).filter(|i| !k.closed[*i]).collect();
            let m = open.len();
            k.closed.iter_mut().for_each(|c| *c = true);
            k.shards.extend((0..n).map(|_| Vec::new()));
            k.closed.extend((0..n).map(|_| false));
            for j in 0..n {
                let p = (m > 0).then(|| open[j * m / n.max(1)]);
                let adj = (m > n).then(|| open.get(j * m / n.max(1) + 1).copied()).flatten();
                k.parents.push((p, adj));
            }
        }
    }

    // ---- Firehose
    pub fn create_delivery_stream(&self, name: &str) {
        self.lock().ext.firehose.entry(name.into()).or_default();
    }
    /// Records delivered to a delivery stream.
    pub fn firehose_records(&self, name: &str) -> Vec<Vec<u8>> {
        self.lock().ext.firehose.get(name).cloned().unwrap_or_default()
    }

    // ---- EventBridge
    /// Events accepted by `PutEvents` (as JSON objects with `source`, `detail-type`, `detail`...).
    pub fn events(&self) -> Vec<Value> {
        self.lock().ext.events.clone()
    }
    /// Names of the rules of a bus.
    pub fn rule_names(&self, bus: &str) -> Vec<String> {
        self.lock().ext.rules.keys().filter(|(b, _)| b == bus).map(|(_, n)| n.clone()).collect()
    }

    // ---- Secrets Manager
    /// Create (or add a version to) a string secret.
    pub fn put_secret(&self, name: &str, value: &str) {
        let mut s = self.lock();
        let region = s.region.clone();
        let sec = s.ext.secrets.entry(name.into()).or_insert_with(|| new_secret(&region, name, ""));
        sec.versions.push(SecretVersion { id: uuid(), string: Some(value.into()), binary: None, created: now_secs() });
    }
    /// Current value of a string secret (None when absent or deleted).
    pub fn secret(&self, name: &str) -> Option<String> {
        self.lock().ext.secrets.get(name).filter(|s| s.deleted.is_none()).and_then(|s| s.versions.last()).and_then(|v| v.string.clone())
    }
    /// Whether a secret is scheduled for deletion (or removed).
    pub fn secret_deleted(&self, name: &str) -> bool {
        self.lock().ext.secrets.get(name).is_none_or(|s| s.deleted.is_some())
    }
    /// (rotation enabled, rotation lambda ARN) of a secret.
    pub fn secret_rotation(&self, name: &str) -> Option<(bool, Option<String>)> {
        self.lock().ext.secrets.get(name).map(|s| (s.rotation_enabled, s.rotation_lambda.clone()))
    }

    // ---- SES / CloudWatch
    pub fn sent_emails(&self) -> Vec<SentEmail> {
        self.lock().ext.emails.clone()
    }
    pub fn metrics(&self) -> Vec<MetricDatum> {
        self.lock().ext.metrics.clone()
    }

    // ---- IAM / EC2
    pub fn iam_users(&self) -> Vec<String> {
        self.lock().ext.users.keys().cloned().collect()
    }
    /// (instance id, state name) of all EC2 instances.
    pub fn ec2_instances(&self) -> Vec<(String, String)> {
        self.lock().ext.instances.iter().map(|(id, i)| (id.clone(), i.state.1.clone())).collect()
    }
    /// Launch an instance directly; returns its id.
    pub fn run_instance(&self, image: &str, itype: &str) -> String {
        let mut s = self.lock();
        new_instance(&mut s.ext, image, itype, "")
    }
}

fn new_secret(region: &str, name: &str, description: &str) -> Secret {
    Secret {
        arn: format!("arn:aws:secretsmanager:{}:{}:secret:{}-{}", region, ACCOUNT, name, &random_hex(3)),
        description: description.into(),
        versions: vec![],
        deleted: None,
        rotation_lambda: None,
        rotation_enabled: false,
        created: now_secs(),
        replicas: vec![],
    }
}

fn new_instance(ext: &mut ExtState, image: &str, itype: &str, key: &str) -> String {
    let id = format!("i-{}", random_hex(8));
    let n = ext.instances.len() + 10;
    ext.instances.insert(
        id.clone(),
        Ec2Instance {
            image: image.into(),
            itype: itype.into(),
            state: (16, "running".into()),
            launch: now_secs(),
            key: key.into(),
            ip: format!("10.0.0.{}", n),
            monitoring: false,
            tags: BTreeMap::new(),
            groups: vec!["default".into()],
            subnet: "subnet-mock".into(),
            zone: "us-east-1a".into(),
            ebs_optimized: false,
            kernel: String::new(),
            client_token: String::new(),
        },
    );
    id
}

/// Dispatch a verified request for one of the extra services (`None` = not handled here).
pub(super) fn handle(st: &Shared, service: &str, req: &ServerRequest) -> Option<Response> {
    Some(match service {
        "kinesis" => kinesis(st, req),
        "firehose" => firehose(st, req),
        "events" => events(st, req),
        "secretsmanager" => secrets(st, req),
        "states" => states(st, req),
        "ses" | "email" => ses(st, req),
        "monitoring" => cloudwatch(st, req),
        "sts" => sts(st, req),
        "iam" => iam(st, req),
        "ec2" => ec2(st, req),
        _ => return None,
    })
}

fn json_ok(v: Value) -> Response {
    Response::json(200, v.to_json()).with_header("Content-Type", "application/x-amz-json-1.1").with_header("x-amzn-RequestId", request_id())
}

fn target(req: &ServerRequest) -> String {
    req.header("X-Amz-Target").and_then(|t| t.split_once('.')).map(|(_, a)| a.to_string()).unwrap_or_default()
}

fn s_(v: &Value, k: &str) -> String {
    v.get(k).map(|x| x.to_string()).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Kinesis
// ---------------------------------------------------------------------------

pub(super) fn kerr(code: &str, msg: &str) -> Response {
    json_error(400, "com.amazonaws.kinesis.v20131202", code, msg)
}

pub(super) fn shard_id(i: usize) -> String {
    format!("shardId-{:012}", i)
}

fn hash_key(pk: &str) -> u128 {
    md5(pk.as_bytes()).iter().fold(0u128, |a, b| (a << 8) | *b as u128)
}

fn put_record(ext: &mut ExtState, stream: &str, pk: &str, data: Vec<u8>) -> Option<(String, String)> {
    ext.seq += 1;
    let seq = 49_000_000_000_000_000_000u128 + ext.seq;
    let k = ext.streams.get_mut(stream)?;
    // only the open (most recent) shards receive records; closed shards keep their data
    let open = open_shards(k);
    let n = open.len().max(1) as u128;
    let idx = open[(hash_key(pk) % n) as usize];
    k.shards[idx].push(KRecord { seq, data, partition_key: pk.into(), arrival_millis: now_millis() });
    Some((shard_id(idx), seq.to_string()))
}

/// The open shards: after a reshard only the new ones accept records.
fn open_shards(k: &KStream) -> Vec<usize> {
    (0..k.shards.len()).filter(|i| !k.closed[*i]).collect()
}

fn shard_desc(i: usize, n: usize, parents: (Option<usize>, Option<usize>)) -> Value {
    let step = u128::MAX / n as u128;
    let start = step * i as u128;
    let end = if i + 1 == n { u128::MAX } else { step * (i as u128 + 1) - 1 };
    let mut m = ObjectMap::new();
    m.insert("ShardId".into(), Value::from(shard_id(i)));
    if let Some(p) = parents.0 {
        m.insert("ParentShardId".into(), Value::from(shard_id(p)));
    }
    if let Some(p) = parents.1 {
        m.insert("AdjacentParentShardId".into(), Value::from(shard_id(p)));
    }
    m.insert("HashKeyRange".into(), obj([("StartingHashKey", Value::from(start.to_string())), ("EndingHashKey", Value::from(end.to_string()))]));
    m.insert("SequenceNumberRange".into(), obj([("StartingSequenceNumber", Value::from("49000000000000000000"))]));
    Value::Object(m)
}

/// Position of the first record of a shard for a `StartingPosition` / iterator type.
pub(super) fn start_position(recs: &[KRecord], kind: &str, seq: u128, timestamp: f64) -> Option<usize> {
    Some(match kind {
        "TRIM_HORIZON" => 0,
        "LATEST" => recs.len(),
        "AT_SEQUENCE_NUMBER" => recs.iter().position(|r| r.seq >= seq).unwrap_or(recs.len()),
        "AFTER_SEQUENCE_NUMBER" => recs.iter().position(|r| r.seq > seq).unwrap_or(recs.len()),
        "AT_TIMESTAMP" => recs.iter().position(|r| r.arrival_millis as f64 >= timestamp * 1000.0).unwrap_or(recs.len()),
        _ => return None,
    })
}

fn encode_iterator(stream: &str, shard: usize, pos: usize) -> String {
    base64_encode(format!("{}|{}|{}", stream, shard, pos).as_bytes())
}

fn decode_iterator(it: &str) -> Option<(String, usize, usize)> {
    let raw = String::from_utf8(base64_decode(it)?).ok()?;
    let mut p = raw.rsplitn(3, '|');
    let pos = p.next()?.parse().ok()?;
    let shard = p.next()?.parse().ok()?;
    Some((p.next()?.to_string(), shard, pos))
}

pub(super) fn kinesis(st: &Shared, req: &ServerRequest) -> Response {
    let body = parse_json(&req.body).unwrap_or_default();
    let mut s = st.0.lock().unwrap();
    let region = s.region.clone();
    let ext = &mut s.ext;
    let name = s_(&body, "StreamName");
    match target(req).as_str() {
        "CreateStream" => {
            if ext.streams.contains_key(&name) {
                return kerr("ResourceInUseException", &format!("Stream {} under account {} already exists.", name, ACCOUNT));
            }
            let n = body.get("ShardCount").and_then(|c| c.as_i64()).unwrap_or(1).max(1) as usize;
            ext.streams.insert(name, new_stream(n));
            json_ok(obj([]))
        }
        "DeleteStream" => match ext.streams.remove(&name) {
            Some(_) => json_ok(obj([])),
            None => kerr("ResourceNotFoundException", &format!("Stream {} under account {} not found.", name, ACCOUNT)),
        },
        "ListStreams" => json_ok(obj([("StreamNames", Value::from(ext.streams.keys().cloned().collect::<Vec<_>>())), ("HasMoreStreams", Value::Bool(false))])),
        "DescribeStream" | "ListShards" => {
            let Some(k) = ext.streams.get(&name) else { return kerr("ResourceNotFoundException", &format!("Stream {} under account {} not found.", name, ACCOUNT)) };
            let n = k.shards.len();
            let shards: Vec<Value> = (0..n).map(|i| shard_desc(i, n, k.parents.get(i).copied().unwrap_or_default())).collect();
            if target(req) == "ListShards" {
                return json_ok(obj([("Shards", Value::Array(shards))]));
            }
            json_ok(obj([(
                "StreamDescription",
                obj([
                    ("StreamName", Value::from(name.clone())),
                    ("StreamARN", Value::from(format!("arn:aws:kinesis:{}:{}:stream/{}", region, ACCOUNT, name))),
                    ("StreamStatus", Value::from("ACTIVE")),
                    ("Shards", Value::Array(shards)),
                    ("HasMoreShards", Value::Bool(false)),
                    ("RetentionPeriodHours", Value::Int(24)),
                    ("StreamCreationTimestamp", Value::Int(k.created)),
                ]),
            )]))
        }
        "PutRecord" => {
            let data = base64_decode(&s_(&body, "Data")).unwrap_or_default();
            let pk = s_(&body, "PartitionKey");
            if pk.is_empty() {
                return kerr("ValidationException", "1 validation error detected: Value null at 'partitionKey' failed to satisfy constraint: Member must not be null");
            }
            match put_record(ext, &name, &pk, data) {
                Some((shard, seq)) => json_ok(obj([("ShardId", Value::from(shard)), ("SequenceNumber", Value::from(seq))])),
                None => kerr("ResourceNotFoundException", &format!("Stream {} under account {} not found.", name, ACCOUNT)),
            }
        }
        "PutRecords" => {
            if !ext.streams.contains_key(&name) {
                return kerr("ResourceNotFoundException", &format!("Stream {} under account {} not found.", name, ACCOUNT));
            }
            let mut out = Vec::new();
            for r in body.get("Records").and_then(|r| r.as_array()).cloned().unwrap_or_default() {
                let data = base64_decode(&s_(&r, "Data")).unwrap_or_default();
                let (shard, seq) = put_record(ext, &name, &s_(&r, "PartitionKey"), data).unwrap_or_default();
                out.push(obj([("ShardId", Value::from(shard)), ("SequenceNumber", Value::from(seq))]));
            }
            json_ok(obj([("FailedRecordCount", Value::Int(0)), ("Records", Value::Array(out))]))
        }
        "GetShardIterator" => {
            let Some(k) = ext.streams.get(&name) else { return kerr("ResourceNotFoundException", &format!("Stream {} under account {} not found.", name, ACCOUNT)) };
            let shard_name = s_(&body, "ShardId");
            let Some(idx) = (0..k.shards.len()).find(|i| shard_id(*i) == shard_name) else { return kerr("ResourceNotFoundException", &format!("Shard {} in stream {} does not exist", shard_name, name)) };
            let recs = &k.shards[idx];
            let seq: u128 = s_(&body, "StartingSequenceNumber").parse().unwrap_or(0);
            let ts = body.get("Timestamp").and_then(|t| t.as_f64()).unwrap_or(0.0);
            let kind = s_(&body, "ShardIteratorType");
            let Some(pos) = start_position(recs, &kind, seq, ts) else { return kerr("ValidationException", &format!("Invalid ShardIteratorType {}", kind)) };
            json_ok(obj([("ShardIterator", Value::from(encode_iterator(&name, idx, pos)))]))
        }
        "GetRecords" => {
            let Some((stream, idx, pos)) = decode_iterator(&s_(&body, "ShardIterator")) else { return kerr("InvalidArgumentException", "Invalid ShardIterator") };
            let Some(k) = ext.streams.get(&stream) else { return kerr("ResourceNotFoundException", &format!("Stream {} not found.", stream)) };
            let limit = body.get("Limit").and_then(|l| l.as_i64()).unwrap_or(10000).max(1) as usize;
            let recs = &k.shards[idx];
            let end = (pos + limit).min(recs.len());
            let out: Vec<Value> = recs[pos.min(recs.len())..end]
                .iter()
                .map(|r| {
                    obj([
                        ("SequenceNumber", Value::from(r.seq.to_string())),
                        ("ApproximateArrivalTimestamp", Value::Float(r.arrival_millis as f64 / 1000.0)),
                        ("Data", Value::from(base64_encode(&r.data))),
                        ("PartitionKey", Value::from(r.partition_key.clone())),
                    ])
                })
                .collect();
            let mut m = ObjectMap::new();
            m.insert("Records".to_string(), Value::Array(out));
            m.insert("MillisBehindLatest".to_string(), Value::Int(0));
            if !(k.closed[idx] && end >= recs.len()) {
                m.insert("NextShardIterator".to_string(), Value::from(encode_iterator(&stream, idx, end)));
            }
            json_ok(Value::Object(m))
        }
        other => kerr("UnknownOperationException", &format!("Unsupported operation {}", other)),
    }
}

// ---------------------------------------------------------------------------
// Firehose
// ---------------------------------------------------------------------------

fn firehose(st: &Shared, req: &ServerRequest) -> Response {
    let body = parse_json(&req.body).unwrap_or_default();
    let mut s = st.0.lock().unwrap();
    let region = s.region.clone();
    let ext = &mut s.ext;
    let name = s_(&body, "DeliveryStreamName");
    let err = |code: &str, msg: &str| json_error(400, "com.amazonaws.firehose.v20150804", code, msg);
    let not_found = || err("ResourceNotFoundException", &format!("Firehose {} under account {} not found.", name, ACCOUNT));
    let arn = format!("arn:aws:firehose:{}:{}:deliverystream/{}", region, ACCOUNT, name);
    match target(req).as_str() {
        "CreateDeliveryStream" => {
            if ext.firehose.contains_key(&name) {
                return err("ResourceInUseException", &format!("Firehose {} under accountId {} already exists", name, ACCOUNT));
            }
            ext.firehose.insert(name.clone(), Vec::new());
            json_ok(obj([("DeliveryStreamARN", Value::from(arn))]))
        }
        "DeleteDeliveryStream" => match ext.firehose.remove(&name) {
            Some(_) => json_ok(obj([])),
            None => not_found(),
        },
        "DescribeDeliveryStream" => match ext.firehose.get(&name) {
            Some(_) => json_ok(obj([(
                "DeliveryStreamDescription",
                obj([
                    ("DeliveryStreamName", Value::from(name.clone())),
                    ("DeliveryStreamARN", Value::from(arn)),
                    ("DeliveryStreamStatus", Value::from("ACTIVE")),
                    ("DeliveryStreamType", Value::from("DirectPut")),
                ]),
            )])),
            None => not_found(),
        },
        "PutRecord" => {
            let Some(recs) = ext.firehose.get_mut(&name) else { return not_found() };
            recs.push(body.get("Record").and_then(|r| base64_decode(&s_(r, "Data"))).unwrap_or_default());
            json_ok(obj([("RecordId", Value::from(random_hex(16))), ("Encrypted", Value::Bool(false))]))
        }
        "PutRecordBatch" => {
            let Some(recs) = ext.firehose.get_mut(&name) else { return not_found() };
            let mut out = Vec::new();
            for r in body.get("Records").and_then(|r| r.as_array()).cloned().unwrap_or_default() {
                recs.push(base64_decode(&s_(&r, "Data")).unwrap_or_default());
                out.push(obj([("RecordId", Value::from(random_hex(16)))]));
            }
            json_ok(obj([("FailedPutCount", Value::Int(0)), ("Encrypted", Value::Bool(false)), ("RequestResponses", Value::Array(out))]))
        }
        other => err("UnknownOperationException", &format!("Unsupported operation {}", other)),
    }
}

// ---------------------------------------------------------------------------
// EventBridge
// ---------------------------------------------------------------------------

/// Simple EventBridge pattern matching: every pattern key must match the event field
/// (arrays list accepted values; nested objects recurse).
fn pattern_matches(pattern: &Value, event: &Value) -> bool {
    let Some(p) = pattern.as_object() else { return false };
    p.iter().all(|(k, cond)| {
        let v = event.get(k);
        match cond {
            Value::Array(options) => v.is_some_and(|v| match v {
                Value::Array(vals) => vals.iter().any(|x| options.contains(x)),
                x => options.contains(x),
            }),
            Value::Object(_) => v.is_some_and(|v| pattern_matches(cond, v)),
            other => v == Some(other),
        }
    })
}

fn events(st: &Shared, req: &ServerRequest) -> Response {
    let body = parse_json(&req.body).unwrap_or_default();
    let mut guard = st.0.lock().unwrap();
    let s = &mut *guard;
    let region = s.region.clone();
    let err = |code: &str, msg: &str| json_error(400, "com.amazonaws.events", code, msg);
    let bus = Some(s_(&body, "EventBusName")).filter(|b| !b.is_empty()).unwrap_or_else(|| "default".into());
    let bus = bus.rsplit('/').next().unwrap_or(&bus).to_string();
    let rule_name = Some(s_(&body, "Name")).filter(|n| !n.is_empty()).unwrap_or_else(|| s_(&body, "Rule"));
    let key = (bus.clone(), rule_name.clone());
    let not_found = || err("ResourceNotFoundException", &format!("Rule {} does not exist on EventBus {}.", rule_name, bus));
    let rule_doc = |name: &str, r: &Rule| {
        let mut m = ObjectMap::new();
        m.insert("Name".to_string(), Value::from(name));
        m.insert("Arn".to_string(), Value::from(r.arn.clone()));
        m.insert("State".to_string(), Value::from(r.state.clone()));
        m.insert("EventBusName".to_string(), Value::from(bus.clone()));
        if let Some(p) = &r.pattern {
            m.insert("EventPattern".to_string(), Value::from(p.clone()));
        }
        Value::Object(m)
    };
    match target(req).as_str() {
        "PutRule" => {
            let arn = format!("arn:aws:events:{}:{}:rule/{}{}", region, ACCOUNT, if bus == "default" { String::new() } else { format!("{}/", bus) }, rule_name);
            let pattern = body.get("EventPattern").map(|p| p.to_string());
            if let Some(p) = &pattern {
                if parse_json(p.as_bytes()).ok().and_then(|v| v.as_object().cloned()).is_none() {
                    return err("InvalidEventPatternException", "Event pattern is not valid.");
                }
            }
            let state = Some(s_(&body, "State")).filter(|x| !x.is_empty()).unwrap_or_else(|| "ENABLED".into());
            let targets = s.ext.rules.get(&key).map(|r| r.targets.clone()).unwrap_or_default();
            s.ext.rules.insert(key, Rule { arn: arn.clone(), pattern, state, targets });
            json_ok(obj([("RuleArn", Value::from(arn))]))
        }
        "DeleteRule" => {
            match s.ext.rules.get(&key) {
                None => {}
                Some(r) if !r.targets.is_empty() => return err("ValidationException", "Rule can't be deleted since it has targets."),
                Some(_) => {
                    s.ext.rules.remove(&key);
                }
            }
            json_ok(obj([]))
        }
        "EnableRule" | "DisableRule" => match s.ext.rules.get_mut(&key) {
            Some(r) => {
                r.state = if target(req) == "EnableRule" { "ENABLED" } else { "DISABLED" }.into();
                json_ok(obj([]))
            }
            None => not_found(),
        },
        "DescribeRule" => match s.ext.rules.get(&key) {
            Some(r) => json_ok(rule_doc(&rule_name, r)),
            None => not_found(),
        },
        "ListRules" => {
            let prefix = s_(&body, "NamePrefix");
            let rules: Vec<Value> = s.ext.rules.iter().filter(|((b, n), _)| *b == bus && n.starts_with(&prefix)).map(|((_, n), r)| rule_doc(n, r)).collect();
            json_ok(obj([("Rules", Value::Array(rules))]))
        }
        "PutTargets" => {
            let Some(r) = s.ext.rules.get_mut(&key) else { return not_found() };
            for t in body.get("Targets").and_then(|t| t.as_array()).cloned().unwrap_or_default() {
                let id = s_(&t, "Id");
                r.targets.retain(|x| s_(x, "Id") != id);
                r.targets.push(t);
            }
            json_ok(obj([("FailedEntryCount", Value::Int(0)), ("FailedEntries", Value::Array(vec![]))]))
        }
        "RemoveTargets" => {
            let Some(r) = s.ext.rules.get_mut(&key) else { return not_found() };
            let ids: Vec<String> = body.get("Ids").and_then(|i| i.as_array()).map(|a| a.iter().map(|x| x.to_string()).collect()).unwrap_or_default();
            r.targets.retain(|t| !ids.contains(&s_(t, "Id")));
            json_ok(obj([("FailedEntryCount", Value::Int(0)), ("FailedEntries", Value::Array(vec![]))]))
        }
        "ListTargetsByRule" => match s.ext.rules.get(&key) {
            Some(r) => json_ok(obj([("Targets", Value::Array(r.targets.clone()))])),
            None => not_found(),
        },
        "ListRuleNamesByTarget" => {
            let arn = s_(&body, "TargetArn");
            let names: Vec<Value> = s.ext.rules.iter().filter(|((b, _), r)| *b == bus && r.targets.iter().any(|t| s_(t, "Arn") == arn)).map(|((_, n), _)| Value::from(n.clone())).collect();
            json_ok(obj([("RuleNames", Value::Array(names))]))
        }
        "PutEvents" => {
            let mut out = Vec::new();
            let mut deliveries: Vec<(String, String)> = Vec::new();
            for e in body.get("Entries").and_then(|e| e.as_array()).cloned().unwrap_or_default() {
                let id = uuid();
                let ebus = Some(s_(&e, "EventBusName")).filter(|b| !b.is_empty()).unwrap_or_else(|| "default".into());
                let ebus = ebus.rsplit('/').next().unwrap_or(&ebus).to_string();
                let detail = parse_json(s_(&e, "Detail").as_bytes()).unwrap_or_default();
                let event = obj([
                    ("version", Value::from("0")),
                    ("id", Value::from(id.clone())),
                    ("detail-type", Value::from(s_(&e, "DetailType"))),
                    ("source", Value::from(s_(&e, "Source"))),
                    ("account", Value::from(ACCOUNT)),
                    ("time", Value::from(iso8601(now_secs()))),
                    ("region", Value::from(region.clone())),
                    ("resources", e.get("Resources").cloned().unwrap_or(Value::Array(vec![]))),
                    ("detail", detail),
                ]);
                for ((b, _), r) in s.ext.rules.iter().filter(|((b, _), r)| *b == ebus && r.state == "ENABLED") {
                    let _ = b;
                    let matched = r.pattern.as_ref().and_then(|p| parse_json(p.as_bytes()).ok()).is_some_and(|p| pattern_matches(&p, &event));
                    if matched {
                        for t in &r.targets {
                            let arn = s_(t, "Arn");
                            if let Some(q) = arn.strip_prefix("arn:aws:sqs:").and_then(|r| r.rsplit(':').next()) {
                                let payload = t.get("Input").map(|i| i.to_string()).unwrap_or_else(|| event.to_json());
                                deliveries.push((q.to_string(), payload));
                            }
                        }
                    }
                }
                s.ext.events.push(event);
                out.push(obj([("EventId", Value::from(id))]));
            }
            for (q, payload) in deliveries {
                s.queues.entry(q).or_default().messages.push(super::new_msg(payload, Value::Object(ObjectMap::new()), 0));
            }
            st.1.notify_all();
            json_ok(obj([("FailedEntryCount", Value::Int(0)), ("Entries", Value::Array(out))]))
        }
        other => err("UnknownOperationException", &format!("Unsupported operation {}", other)),
    }
}

// ---------------------------------------------------------------------------
// Secrets Manager
// ---------------------------------------------------------------------------

fn secrets(st: &Shared, req: &ServerRequest) -> Response {
    let body = parse_json(&req.body).unwrap_or_default();
    let mut s = st.0.lock().unwrap();
    let region = s.region.clone();
    let ext = &mut s.ext;
    let err = |code: &str, msg: &str| json_error(400, "com.amazonaws.secretsmanager", code, msg);
    let id = Some(s_(&body, "SecretId")).filter(|x| !x.is_empty()).unwrap_or_else(|| s_(&body, "Name"));
    // resolve by name or ARN
    let name = ext.secrets.iter().find(|(n, sec)| **n == id || sec.arn == id).map(|(n, _)| n.clone()).unwrap_or_else(|| id.clone());
    let not_found = || err("ResourceNotFoundException", "Secrets Manager can't find the specified secret.");
    let new_version = |body: &Value| SecretVersion {
        id: Some(s_(body, "ClientRequestToken")).filter(|t| !t.is_empty()).unwrap_or_else(uuid),
        string: body.get("SecretString").filter(|v| !v.is_null()).map(|v| v.to_string()),
        binary: body.get("SecretBinary").filter(|v| !v.is_null()).and_then(|v| base64_decode(&v.to_string())),
        created: now_secs(),
    };
    match target(req).as_str() {
        "CreateSecret" => {
            if ext.secrets.get(&name).is_some_and(|s| s.deleted.is_none()) {
                return err("ResourceExistsException", &format!("The operation failed because the secret {} already exists.", name));
            }
            let mut sec = new_secret(&region, &name, &s_(&body, "Description"));
            let v = new_version(&body);
            let vid = v.id.clone();
            sec.versions.push(v);
            let arn = sec.arn.clone();
            ext.secrets.insert(name.clone(), sec);
            json_ok(obj([("ARN", Value::from(arn)), ("Name", Value::from(name)), ("VersionId", Value::from(vid))]))
        }
        "GetSecretValue" => {
            let Some(sec) = ext.secrets.get(&name) else { return not_found() };
            if sec.deleted.is_some() {
                return err("InvalidRequestException", "You can't perform this operation on the secret because it was marked for deletion.");
            }
            let wanted = s_(&body, "VersionId");
            let v = if wanted.is_empty() { sec.versions.last() } else { sec.versions.iter().find(|v| v.id == wanted) };
            let Some(v) = v else { return err("ResourceNotFoundException", "Secrets Manager can't find the specified secret value for VersionId") };
            let mut m = ObjectMap::new();
            m.insert("ARN".to_string(), Value::from(sec.arn.clone()));
            m.insert("Name".to_string(), Value::from(name.clone()));
            m.insert("VersionId".to_string(), Value::from(v.id.clone()));
            m.insert("VersionStages".to_string(), Value::from(vec!["AWSCURRENT"]));
            m.insert("CreatedDate".to_string(), Value::Int(v.created));
            if let Some(t) = &v.string {
                m.insert("SecretString".into(), Value::from(t.clone()));
            }
            if let Some(b) = &v.binary {
                m.insert("SecretBinary".into(), Value::from(base64_encode(b)));
            }
            json_ok(Value::Object(m))
        }
        "PutSecretValue" => {
            let Some(sec) = ext.secrets.get_mut(&name) else { return not_found() };
            let v = new_version(&body);
            let vid = v.id.clone();
            sec.versions.push(v);
            json_ok(obj([("ARN", Value::from(sec.arn.clone())), ("Name", Value::from(name)), ("VersionId", Value::from(vid))]))
        }
        "DescribeSecret" => {
            let Some(sec) = ext.secrets.get(&name) else { return not_found() };
            let mut m = ObjectMap::new();
            m.insert("ARN".to_string(), Value::from(sec.arn.clone()));
            m.insert("Name".to_string(), Value::from(name.clone()));
            m.insert("Description".to_string(), Value::from(sec.description.clone()));
            m.insert("RotationEnabled".to_string(), Value::Bool(sec.rotation_enabled));
            m.insert("CreatedDate".to_string(), Value::Int(sec.created));
            if let Some(l) = &sec.rotation_lambda {
                m.insert("RotationLambdaARN".into(), Value::from(l.clone()));
            }
            if let Some(d) = sec.deleted {
                m.insert("DeletedDate".into(), Value::Int(d));
            }
            if !sec.replicas.is_empty() {
                m.insert("ReplicationStatus".into(), Value::Array(sec.replicas.iter().map(|r| obj([("Region", Value::from(r.clone())), ("Status", Value::from("InSync"))])).collect()));
            }
            json_ok(Value::Object(m))
        }
        "ListSecrets" => {
            let list: Vec<Value> = ext
                .secrets
                .iter()
                .filter(|(_, s)| s.deleted.is_none())
                .map(|(n, s)| obj([("ARN", Value::from(s.arn.clone())), ("Name", Value::from(n.clone())), ("Description", Value::from(s.description.clone())), ("RotationEnabled", Value::Bool(s.rotation_enabled))]))
                .collect();
            json_ok(obj([("SecretList", Value::Array(list))]))
        }
        "DeleteSecret" => {
            let force = body.get("ForceDeleteWithoutRecovery").is_some_and(|f| f.is_truthy());
            let Some(sec) = ext.secrets.get_mut(&name) else { return not_found() };
            let arn = sec.arn.clone();
            let when = now_secs() + if force { 0 } else { 30 * 86400 };
            if force {
                ext.secrets.remove(&name);
            } else {
                sec.deleted = Some(now_secs());
            }
            json_ok(obj([("ARN", Value::from(arn)), ("Name", Value::from(name)), ("DeletionDate", Value::Int(when))]))
        }
        "RestoreSecret" => {
            let Some(sec) = ext.secrets.get_mut(&name) else { return not_found() };
            sec.deleted = None;
            json_ok(obj([("ARN", Value::from(sec.arn.clone())), ("Name", Value::from(name))]))
        }
        "RotateSecret" => {
            // stub: records the rotation configuration and adds a new version with the current value
            let Some(sec) = ext.secrets.get_mut(&name) else { return not_found() };
            if let Some(l) = body.get("RotationLambdaARN").map(|l| l.to_string()) {
                sec.rotation_lambda = Some(l);
            }
            if sec.rotation_lambda.is_none() {
                return err("InvalidRequestException", "No Lambda rotation function ARN is associated with this secret.");
            }
            sec.rotation_enabled = true;
            let mut v = sec.versions.last().cloned().unwrap_or(SecretVersion { id: String::new(), string: None, binary: None, created: 0 });
            v.id = Some(s_(&body, "ClientRequestToken")).filter(|t| !t.is_empty()).unwrap_or_else(uuid);
            v.created = now_secs();
            let vid = v.id.clone();
            sec.versions.push(v);
            json_ok(obj([("ARN", Value::from(sec.arn.clone())), ("Name", Value::from(name)), ("VersionId", Value::from(vid))]))
        }
        "ReplicateSecretToRegions" => {
            let Some(sec) = ext.secrets.get_mut(&name) else { return not_found() };
            for r in body.get("AddReplicaRegions").and_then(|r| r.as_array()).cloned().unwrap_or_default() {
                sec.replicas.push(s_(&r, "Region"));
            }
            let status: Vec<Value> = sec.replicas.iter().map(|r| obj([("Region", Value::from(r.clone())), ("Status", Value::from("InSync"))])).collect();
            json_ok(obj([("ARN", Value::from(sec.arn.clone())), ("ReplicationStatus", Value::Array(status))]))
        }
        other => err("InvalidRequestException", &format!("Unsupported operation {}", other)),
    }
}

// ---------------------------------------------------------------------------
// Step Functions
// ---------------------------------------------------------------------------

/// Run a definition made of `Pass` / `Succeed` / `Fail` states (others leave it RUNNING).
fn run_machine(definition: &str, input: &str) -> (String, Option<String>, Option<(String, String)>) {
    let Ok(def) = parse_json(definition.as_bytes()) else { return ("FAILED".into(), None, Some(("States.Runtime".into(), "Invalid definition".into()))) };
    let mut data = parse_json(input.as_bytes()).unwrap_or_default();
    let mut current = s_(&def, "StartAt");
    for _ in 0..100 {
        let Some(state) = def.get("States").and_then(|s| s.get(&current)) else { return ("FAILED".into(), None, Some(("States.Runtime".into(), format!("No state {}", current)))) };
        match s_(state, "Type").as_str() {
            "Pass" => {
                if let Some(r) = state.get("Result") {
                    data = r.clone();
                }
                if state.get("End").is_some_and(|e| e.is_truthy()) {
                    return ("SUCCEEDED".into(), Some(data.to_json()), None);
                }
                current = s_(state, "Next");
            }
            "Succeed" => return ("SUCCEEDED".into(), Some(data.to_json()), None),
            "Fail" => return ("FAILED".into(), None, Some((s_(state, "Error"), s_(state, "Cause")))),
            _ => return ("RUNNING".into(), None, None),
        }
    }
    ("FAILED".into(), None, Some(("States.Runtime".into(), "Too many transitions".into())))
}

fn states(st: &Shared, req: &ServerRequest) -> Response {
    let body = parse_json(&req.body).unwrap_or_default();
    let mut s = st.0.lock().unwrap();
    let region = s.region.clone();
    let ext = &mut s.ext;
    let err = |code: &str, msg: &str| json_error(400, "com.amazonaws.swf.service.model", code, msg).with_header("Content-Type", "application/x-amz-json-1.0");
    let ok = |v: Value| Response::json(200, v.to_json()).with_header("Content-Type", "application/x-amz-json-1.0");
    let exec_doc = |arn: &str, e: &Execution| {
        let mut m = ObjectMap::new();
        m.insert("executionArn".to_string(), Value::from(arn));
        m.insert("stateMachineArn".to_string(), Value::from(e.machine.clone()));
        m.insert("name".to_string(), Value::from(e.name.clone()));
        m.insert("status".to_string(), Value::from(e.status.clone()));
        m.insert("startDate".to_string(), Value::Float(e.start as f64 / 1000.0));
        m.insert("input".to_string(), Value::from(e.input.clone()));
        if let Some(o) = &e.output {
            m.insert("output".into(), Value::from(o.clone()));
        }
        if let Some(t) = e.stop {
            m.insert("stopDate".into(), Value::Float(t as f64 / 1000.0));
        }
        if let Some((code, cause)) = &e.error {
            m.insert("error".into(), Value::from(code.clone()));
            m.insert("cause".into(), Value::from(cause.clone()));
        }
        Value::Object(m)
    };
    match target(req).as_str() {
        "CreateStateMachine" => {
            let name = s_(&body, "name");
            let def = s_(&body, "definition");
            if parse_json(def.as_bytes()).ok().and_then(|d| d.get("StartAt").cloned()).is_none() {
                return err("InvalidDefinition", "Invalid State Machine Definition: 'MISSING_START_AT'");
            }
            let arn = format!("arn:aws:states:{}:{}:stateMachine:{}", region, ACCOUNT, name);
            if ext.machines.contains_key(&arn) {
                return err("StateMachineAlreadyExists", &format!("State Machine Already Exists: '{}'", arn));
            }
            ext.machines.insert(arn.clone(), Machine { name, definition: def, role: s_(&body, "roleArn"), created: now_millis() });
            ok(obj([("stateMachineArn", Value::from(arn)), ("creationDate", Value::Float(now_millis() as f64 / 1000.0))]))
        }
        "DeleteStateMachine" => {
            ext.machines.remove(&s_(&body, "stateMachineArn"));
            ok(obj([]))
        }
        "DescribeStateMachine" => match ext.machines.get(&s_(&body, "stateMachineArn")) {
            Some(m) => ok(obj([
                ("stateMachineArn", Value::from(s_(&body, "stateMachineArn"))),
                ("name", Value::from(m.name.clone())),
                ("definition", Value::from(m.definition.clone())),
                ("roleArn", Value::from(m.role.clone())),
                ("status", Value::from("ACTIVE")),
                ("type", Value::from("STANDARD")),
                ("creationDate", Value::Float(m.created as f64 / 1000.0)),
            ])),
            None => err("StateMachineDoesNotExist", "State Machine Does Not Exist"),
        },
        "ListStateMachines" => {
            let list: Vec<Value> = ext.machines.iter().map(|(arn, m)| obj([("stateMachineArn", Value::from(arn.clone())), ("name", Value::from(m.name.clone())), ("type", Value::from("STANDARD"))])).collect();
            ok(obj([("stateMachines", Value::Array(list))]))
        }
        "StartExecution" | "StartSyncExecution" => {
            let machine = s_(&body, "stateMachineArn");
            let Some(m) = ext.machines.get(&machine) else { return err("StateMachineDoesNotExist", &format!("State Machine Does Not Exist: '{}'", machine)) };
            let name = Some(s_(&body, "name")).filter(|n| !n.is_empty()).unwrap_or_else(uuid);
            let input = Some(s_(&body, "input")).filter(|i| !i.is_empty()).unwrap_or_else(|| "{}".into());
            if parse_json(input.as_bytes()).is_err() {
                return err("InvalidExecutionInput", "Invalid execution input: not JSON");
            }
            let arn = format!("arn:aws:states:{}:{}:execution:{}:{}", region, ACCOUNT, m.name, name);
            if ext.executions.contains_key(&arn) {
                return err("ExecutionAlreadyExists", &format!("Execution Already Exists: '{}'", arn));
            }
            let (status, output, error) = run_machine(&m.definition, &input);
            let now = now_millis();
            let done = status != "RUNNING";
            let e = Execution { machine, name, status, input, output, error, start: now, stop: done.then_some(now) };
            let doc = exec_doc(&arn, &e);
            ext.executions.insert(arn.clone(), e);
            if target(req) == "StartSyncExecution" {
                return ok(doc);
            }
            ok(obj([("executionArn", Value::from(arn)), ("startDate", Value::Float(now as f64 / 1000.0))]))
        }
        "DescribeExecution" => match ext.executions.get(&s_(&body, "executionArn")) {
            Some(e) => ok(exec_doc(&s_(&body, "executionArn"), e)),
            None => err("ExecutionDoesNotExist", "Execution Does Not Exist"),
        },
        "StopExecution" => match ext.executions.get_mut(&s_(&body, "executionArn")) {
            Some(e) => {
                if e.status == "RUNNING" {
                    e.status = "ABORTED".into();
                    e.stop = Some(now_millis());
                }
                ok(obj([("stopDate", Value::Float(now_millis() as f64 / 1000.0))]))
            }
            None => err("ExecutionDoesNotExist", "Execution Does Not Exist"),
        },
        "ListExecutions" => {
            let machine = s_(&body, "stateMachineArn");
            let filter = s_(&body, "statusFilter");
            let list: Vec<Value> = ext
                .executions
                .iter()
                .filter(|(_, e)| e.machine == machine && (filter.is_empty() || e.status == filter))
                .map(|(arn, e)| obj([("executionArn", Value::from(arn.clone())), ("name", Value::from(e.name.clone())), ("status", Value::from(e.status.clone()))]))
                .collect();
            ok(obj([("executions", Value::Array(list))]))
        }
        "GetExecutionHistory" => match ext.executions.get(&s_(&body, "executionArn")) {
            Some(e) => {
                let mut events = vec![obj([("id", Value::Int(1)), ("type", Value::from("ExecutionStarted"))])];
                match e.status.as_str() {
                    "SUCCEEDED" => events.push(obj([("id", Value::Int(2)), ("type", Value::from("ExecutionSucceeded"))])),
                    "FAILED" => events.push(obj([("id", Value::Int(2)), ("type", Value::from("ExecutionFailed"))])),
                    "ABORTED" => events.push(obj([("id", Value::Int(2)), ("type", Value::from("ExecutionAborted"))])),
                    _ => {}
                }
                ok(obj([("events", Value::Array(events))]))
            }
            None => err("ExecutionDoesNotExist", "Execution Does Not Exist"),
        },
        other => err("UnknownOperationException", &format!("Unsupported operation {}", other)),
    }
}

// ---------------------------------------------------------------------------
// Query APIs (SES, CloudWatch, STS, IAM, EC2)
// ---------------------------------------------------------------------------

fn form(req: &ServerRequest) -> Vec<(String, String)> {
    let mut p = parse_form(&req.text());
    p.extend(req.query_params());
    p
}

fn get(p: &[(String, String)], k: &str) -> Option<String> {
    p.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone())
}

fn members(p: &[(String, String)], prefix: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 1;
    while let Some(v) = get(p, &format!("{}.member.{}", prefix, i)) {
        out.push(v);
        i += 1;
    }
    out
}

fn query_ok(ns: &str, action: &str, inner: String) -> Response {
    Response::xml(
        200,
        format!("<{0}Response xmlns=\"{1}\"><{0}Result>{2}</{0}Result><ResponseMetadata><RequestId>{3}</RequestId></ResponseMetadata></{0}Response>", action, ns, inner, request_id()),
    )
}

fn query_err(status: u16, ns: &str, code: &str, msg: &str) -> Response {
    Response::xml(
        status,
        format!("<ErrorResponse xmlns=\"{}\"><Error><Type>Sender</Type><Code>{}</Code><Message>{}</Message></Error><RequestId>{}</RequestId></ErrorResponse>", ns, code, xml_escape(msg), request_id()),
    )
}

fn ses(st: &Shared, req: &ServerRequest) -> Response {
    const NS: &str = "http://ses.amazonaws.com/doc/2010-12-01/";
    let p = form(req);
    let action = get(&p, "Action").unwrap_or_default();
    let mut s = st.0.lock().unwrap();
    let id = format!("0100{}-{}-000000", random_hex(8), uuid());
    let mut email = SentEmail {
        message_id: id.clone(),
        source: get(&p, "Source").unwrap_or_default(),
        to: vec![],
        cc: vec![],
        bcc: vec![],
        subject: String::new(),
        text: None,
        html: None,
        raw: None,
        reply_to: members(&p, "ReplyToAddresses"),
    };
    match action.as_str() {
        "SendEmail" => {
            email.to = members(&p, "Destination.ToAddresses");
            email.cc = members(&p, "Destination.CcAddresses");
            email.bcc = members(&p, "Destination.BccAddresses");
            if email.to.is_empty() && email.cc.is_empty() && email.bcc.is_empty() {
                return query_err(400, NS, "InvalidParameterValue", "Missing final '@domain'");
            }
            if email.source.is_empty() || !email.source.contains('@') {
                return query_err(400, NS, "InvalidParameterValue", "Missing or invalid Source");
            }
            email.subject = get(&p, "Message.Subject.Data").unwrap_or_default();
            email.text = get(&p, "Message.Body.Text.Data");
            email.html = get(&p, "Message.Body.Html.Data");
            s.ext.emails.push(email);
            query_ok(NS, "SendEmail", format!("<MessageId>{}</MessageId>", id))
        }
        "SendRawEmail" => {
            let Some(raw) = get(&p, "RawMessage.Data").and_then(|d| base64_decode(&d)) else { return query_err(400, NS, "InvalidParameterValue", "RawMessage.Data is required") };
            email.to = members(&p, "Destinations");
            email.raw = Some(raw);
            s.ext.emails.push(email);
            query_ok(NS, "SendRawEmail", format!("<MessageId>{}</MessageId>", id))
        }
        other => query_err(400, NS, "InvalidAction", &format!("Unsupported action {}", other)),
    }
}

fn cloudwatch(st: &Shared, req: &ServerRequest) -> Response {
    const NS: &str = "http://monitoring.amazonaws.com/doc/2010-08-01/";
    let p = form(req);
    let mut s = st.0.lock().unwrap();
    match get(&p, "Action").unwrap_or_default().as_str() {
        "PutMetricData" => {
            let Some(ns) = get(&p, "Namespace").filter(|n| !n.is_empty()) else { return query_err(400, NS, "MissingParameter", "The parameter Namespace is required.") };
            let mut i = 1;
            while let Some(name) = get(&p, &format!("MetricData.member.{}.MetricName", i)) {
                let m = format!("MetricData.member.{}", i);
                let mut dims = Vec::new();
                let mut j = 1;
                while let Some(dn) = get(&p, &format!("{}.Dimensions.member.{}.Name", m, j)) {
                    dims.push((dn, get(&p, &format!("{}.Dimensions.member.{}.Value", m, j)).unwrap_or_default()));
                    j += 1;
                }
                let value = match get(&p, &format!("{}.Value", m)).map(|v| v.parse::<f64>()) {
                    Some(Ok(v)) => v,
                    _ => return query_err(400, NS, "InvalidParameterValue", "The value for parameter MetricData.member.Value is invalid."),
                };
                s.ext.metrics.push(MetricDatum {
                    namespace: ns.clone(),
                    name,
                    value,
                    unit: get(&p, &format!("{}.Unit", m)).unwrap_or_else(|| "None".into()),
                    timestamp: get(&p, &format!("{}.Timestamp", m)).unwrap_or_default(),
                    dimensions: dims,
                });
                i += 1;
            }
            query_ok(NS, "PutMetricData", String::new())
        }
        other => query_err(400, NS, "InvalidAction", &format!("Unsupported action {}", other)),
    }
}

fn sts(st: &Shared, req: &ServerRequest) -> Response {
    const NS: &str = "https://sts.amazonaws.com/doc/2011-06-15/";
    let p = form(req);
    let action = get(&p, "Action").unwrap_or_default();
    let mut s = st.0.lock().unwrap();
    let duration: i64 = get(&p, "DurationSeconds").and_then(|d| d.parse().ok()).unwrap_or(3600);
    if !(900..=129_600).contains(&duration) {
        return query_err(400, NS, "ValidationError", "1 validation error detected: Value at 'durationSeconds' failed to satisfy constraint");
    }
    let issue = |s: &mut super::State| {
        let ak = format!("ASIA{}", random_hex(8).to_uppercase());
        let sk = random_hex(20);
        let token = base64_encode(format!("FwoGZXIvYXdzE{}", random_hex(24)).as_bytes());
        s.credentials.insert(ak.clone(), sk.clone());
        s.session_tokens.insert(ak.clone(), token.clone());
        format!(
            "<Credentials><AccessKeyId>{}</AccessKeyId><SecretAccessKey>{}</SecretAccessKey><SessionToken>{}</SessionToken><Expiration>{}</Expiration></Credentials>",
            ak,
            sk,
            xml_escape(&token),
            iso8601(now_secs() + duration)
        )
    };
    match action.as_str() {
        "AssumeRole" => {
            let (Some(arn), Some(name)) = (get(&p, "RoleArn"), get(&p, "RoleSessionName")) else { return query_err(400, NS, "ValidationError", "RoleArn and RoleSessionName are required") };
            let Some(role) = arn.strip_prefix(&format!("arn:aws:iam::{}:role/", ACCOUNT)).map(|r| r.to_string()) else {
                return query_err(403, NS, "AccessDenied", &format!("User is not authorized to perform: sts:AssumeRole on resource: {}", arn));
            };
            let creds = issue(&mut s);
            query_ok(
                NS,
                "AssumeRole",
                format!(
                    "{}<AssumedRoleUser><AssumedRoleId>AROA{}:{}</AssumedRoleId><Arn>arn:aws:sts::{}:assumed-role/{}/{}</Arn></AssumedRoleUser>",
                    creds,
                    random_hex(6).to_uppercase(),
                    xml_escape(&name),
                    ACCOUNT,
                    xml_escape(&role),
                    xml_escape(&name)
                ),
            )
        }
        "GetSessionToken" => {
            let creds = issue(&mut s);
            query_ok(NS, "GetSessionToken", creds)
        }
        "GetFederationToken" => {
            let Some(name) = get(&p, "Name") else { return query_err(400, NS, "ValidationError", "Name is required") };
            let creds = issue(&mut s);
            query_ok(
                NS,
                "GetFederationToken",
                format!("{}<FederatedUser><FederatedUserId>{}:{}</FederatedUserId><Arn>arn:aws:sts::{}:federated-user/{}</Arn></FederatedUser>", creds, ACCOUNT, xml_escape(&name), ACCOUNT, xml_escape(&name)),
            )
        }
        "GetCallerIdentity" => query_ok(NS, "GetCallerIdentity", format!("<Arn>arn:aws:iam::{0}:user/mock</Arn><UserId>AIDAMOCK</UserId><Account>{0}</Account>", ACCOUNT)),
        other => query_err(400, NS, "InvalidAction", &format!("Unsupported action {}", other)),
    }
}

fn iam(st: &Shared, req: &ServerRequest) -> Response {
    const NS: &str = "https://iam.amazonaws.com/doc/2010-05-08/";
    let p = form(req);
    let action = get(&p, "Action").unwrap_or_default();
    let mut s = st.0.lock().unwrap();
    let ext = &mut s.ext;
    let user_name = get(&p, "UserName").unwrap_or_default();
    let group_name = get(&p, "GroupName").unwrap_or_default();
    let user_xml = |n: &str, u: &IamUser| format!("<UserName>{}</UserName><UserId>{}</UserId><Arn>{}</Arn><Path>/</Path><CreateDate>{}</CreateDate>", xml_escape(n), u.id, xml_escape(&u.arn), iso8601(u.created));
    let group_xml = |n: &str, g: &(String, i64, Vec<String>)| format!("<GroupName>{}</GroupName><GroupId>{}</GroupId><Arn>arn:aws:iam::{}:group/{}</Arn><Path>/</Path><CreateDate>{}</CreateDate>", xml_escape(n), g.0, ACCOUNT, xml_escape(n), iso8601(g.1));
    let no_user = || query_err(404, NS, "NoSuchEntity", &format!("The user with name {} cannot be found.", user_name));
    match action.as_str() {
        "CreateUser" => {
            if user_name.is_empty() {
                return query_err(400, NS, "ValidationError", "UserName is required");
            }
            if ext.users.contains_key(&user_name) {
                return query_err(409, NS, "EntityAlreadyExists", &format!("User with name {} already exists.", user_name));
            }
            let u = IamUser { id: format!("AIDA{}", random_hex(8).to_uppercase()), arn: format!("arn:aws:iam::{}:user/{}", ACCOUNT, user_name), created: now_secs() };
            let x = user_xml(&user_name, &u);
            ext.users.insert(user_name, u);
            query_ok(NS, "CreateUser", format!("<User>{}</User>", x))
        }
        "GetUser" => match ext.users.get(&user_name) {
            Some(u) => query_ok(NS, "GetUser", format!("<User>{}</User>", user_xml(&user_name, u))),
            None => no_user(),
        },
        "DeleteUser" => {
            if ext.users.remove(&user_name).is_none() {
                return no_user();
            }
            for g in ext.groups.values_mut() {
                g.2.retain(|u| *u != user_name);
            }
            query_ok(NS, "DeleteUser", String::new())
        }
        "ListUsers" => {
            let members: String = ext.users.iter().map(|(n, u)| format!("<member>{}</member>", user_xml(n, u))).collect();
            query_ok(NS, "ListUsers", format!("<Users>{}</Users><IsTruncated>false</IsTruncated>", members))
        }
        "CreateGroup" => {
            if ext.groups.contains_key(&group_name) {
                return query_err(409, NS, "EntityAlreadyExists", &format!("Group with name {} already exists.", group_name));
            }
            let g = (format!("AGPA{}", random_hex(8).to_uppercase()), now_secs(), Vec::new());
            let x = group_xml(&group_name, &g);
            ext.groups.insert(group_name, g);
            query_ok(NS, "CreateGroup", format!("<Group>{}</Group>", x))
        }
        "DeleteGroup" => match ext.groups.remove(&group_name) {
            Some(_) => query_ok(NS, "DeleteGroup", String::new()),
            None => query_err(404, NS, "NoSuchEntity", &format!("The group with name {} cannot be found.", group_name)),
        },
        "ListGroups" => {
            let members: String = ext.groups.iter().map(|(n, g)| format!("<member>{}</member>", group_xml(n, g))).collect();
            query_ok(NS, "ListGroups", format!("<Groups>{}</Groups><IsTruncated>false</IsTruncated>", members))
        }
        "AddUserToGroup" | "RemoveUserFromGroup" => {
            if !ext.users.contains_key(&user_name) {
                return no_user();
            }
            let Some(g) = ext.groups.get_mut(&group_name) else { return query_err(404, NS, "NoSuchEntity", &format!("The group with name {} cannot be found.", group_name)) };
            g.2.retain(|u| *u != user_name);
            if action == "AddUserToGroup" {
                g.2.push(user_name);
            }
            query_ok(NS, &action, String::new())
        }
        "CreateAccessKey" => {
            if !ext.users.contains_key(&user_name) {
                return no_user();
            }
            let ak = format!("AKIA{}", random_hex(8).to_uppercase());
            let sk = random_hex(20);
            ext.access_keys.push((user_name.clone(), ak.clone(), sk.clone()));
            s.credentials.insert(ak.clone(), sk.clone());
            query_ok(
                NS,
                "CreateAccessKey",
                format!("<AccessKey><UserName>{}</UserName><AccessKeyId>{}</AccessKeyId><Status>Active</Status><SecretAccessKey>{}</SecretAccessKey><CreateDate>{}</CreateDate></AccessKey>", xml_escape(&user_name), ak, sk, iso8601(now_secs())),
            )
        }
        "ListAccessKeys" => {
            let members: String = ext
                .access_keys
                .iter()
                .filter(|(u, _, _)| user_name.is_empty() || *u == user_name)
                .map(|(u, ak, _)| format!("<member><UserName>{}</UserName><AccessKeyId>{}</AccessKeyId><Status>Active</Status></member>", xml_escape(u), ak))
                .collect();
            query_ok(NS, "ListAccessKeys", format!("<AccessKeyMetadata>{}</AccessKeyMetadata><IsTruncated>false</IsTruncated>", members))
        }
        "DeleteAccessKey" => {
            let ak = get(&p, "AccessKeyId").unwrap_or_default();
            ext.access_keys.retain(|(_, k, _)| *k != ak);
            s.credentials.remove(&ak);
            query_ok(NS, "DeleteAccessKey", String::new())
        }
        other => query_err(400, NS, "InvalidAction", &format!("Unsupported action {}", other)),
    }
}

fn ec2(st: &Shared, req: &ServerRequest) -> Response {
    const NS: &str = "http://ec2.amazonaws.com/doc/2016-11-15/";
    let p = form(req);
    let action = get(&p, "Action").unwrap_or_default();
    let mut s = st.0.lock().unwrap();
    let ext = &mut s.ext;
    let err = |code: &str, msg: &str| Response::xml(400, format!("<Response><Errors><Error><Code>{}</Code><Message>{}</Message></Error></Errors><RequestID>{}</RequestID></Response>", code, xml_escape(msg), request_id()));
    let mut ids = Vec::new();
    let mut i = 1;
    let id_key = if action.ends_with("Tags") { "ResourceId" } else { "InstanceId" };
    while let Some(v) = get(&p, &format!("{}.{}", id_key, i)) {
        ids.push(v);
        i += 1;
    }
    if let Some(missing) = ids.iter().find(|id| !ext.instances.contains_key(*id)) {
        return err("InvalidInstanceID.NotFound", &format!("The instance ID '{}' does not exist", missing));
    }
    let inst_xml = |id: &str, i: &Ec2Instance| {
        let groups: String = i.groups.iter().map(|g| format!("<item><groupId>sg-{}</groupId><groupName>{}</groupName></item>", xml_escape(g), xml_escape(g))).collect();
        let tags: String = i.tags.iter().map(|(k, v)| format!("<item><key>{}</key><value>{}</value></item>", xml_escape(k), xml_escape(v))).collect();
        format!(
            "<item><instanceId>{}</instanceId><imageId>{}</imageId><instanceState><code>{}</code><name>{}</name></instanceState><privateIpAddress>{}</privateIpAddress><keyName>{}</keyName><instanceType>{}</instanceType><launchTime>{}</launchTime><placement><availabilityZone>{}</availabilityZone></placement><monitoring><state>{}</state></monitoring><subnetId>{}</subnetId><groupSet>{}</groupSet><tagSet>{}</tagSet><ebsOptimized>{}</ebsOptimized><kernelId>{}</kernelId><clientToken>{}</clientToken></item>",
            id,
            xml_escape(&i.image),
            i.state.0,
            i.state.1,
            i.ip,
            xml_escape(&i.key),
            xml_escape(&i.itype),
            iso8601_millis(i.launch * 1000),
            xml_escape(&i.zone),
            if i.monitoring { "enabled" } else { "disabled" },
            xml_escape(&i.subnet),
            groups,
            tags,
            i.ebs_optimized,
            xml_escape(&i.kernel),
            xml_escape(&i.client_token)
        )
    };
    let wrap = |action: &str, inner: String| Response::xml(200, format!("<{0}Response xmlns=\"{1}\"><requestId>{2}</requestId>{3}</{0}Response>", action, NS, request_id(), inner));
    match action.as_str() {
        "DescribeInstances" => {
            let items: String = ext.instances.iter().filter(|(id, _)| ids.is_empty() || ids.contains(id)).map(|(id, i)| inst_xml(id, i)).collect();
            let reservation = if items.is_empty() { String::new() } else { format!("<item><reservationId>r-{}</reservationId><ownerId>{}</ownerId><instancesSet>{}</instancesSet></item>", random_hex(8), ACCOUNT, items) };
            wrap("DescribeInstances", format!("<reservationSet>{}</reservationSet>", reservation))
        }
        "DescribeInstanceStatus" => {
            let items: String = ext
                .instances
                .iter()
                .filter(|(id, i)| (ids.is_empty() || ids.contains(id)) && i.state.1 == "running")
                .map(|(id, i)| format!("<item><instanceId>{}</instanceId><instanceState><code>{}</code><name>{}</name></instanceState></item>", id, i.state.0, i.state.1))
                .collect();
            wrap("DescribeInstanceStatus", format!("<instanceStatusSet>{}</instanceStatusSet>", items))
        }
        "RunInstances" => {
            let image = get(&p, "ImageId").unwrap_or_default();
            if !image.starts_with("ami-") {
                return err("InvalidAMIID.Malformed", &format!("Invalid id: \"{}\" (expecting \"ami-...\")", image));
            }
            let itype = get(&p, "InstanceType").unwrap_or_else(|| "m1.small".into());
            let count: usize = get(&p, "MinCount").and_then(|c| c.parse().ok()).unwrap_or(1).max(1);
            let key = get(&p, "KeyName").unwrap_or_default();
            let token = get(&p, "ClientToken").unwrap_or_default();
            // idempotency: the same client token returns the instances of the first call
            let previous: Vec<String> = if token.is_empty() { vec![] } else { ext.instances.iter().filter(|(_, i)| i.client_token == token).map(|(id, _)| id.clone()).collect() };
            if !previous.is_empty() {
                let items: String = previous.iter().map(|id| inst_xml(id, &ext.instances[id])).collect();
                return wrap("RunInstances", format!("<reservationId>r-{}</reservationId><ownerId>{}</ownerId><instancesSet>{}</instancesSet>", random_hex(8), ACCOUNT, items));
            }
            let collect = |prefix: &str| -> Vec<String> {
                let mut out = Vec::new();
                let mut n = 1;
                while let Some(v) = get(&p, &format!("{}.{}", prefix, n)) {
                    out.push(v);
                    n += 1;
                }
                out
            };
            let mut groups = collect("SecurityGroup");
            groups.extend(collect("SecurityGroupId"));
            let mut tags = BTreeMap::new();
            let mut n = 1;
            while let Some(k) = get(&p, &format!("TagSpecification.1.Tag.{}.Key", n)) {
                tags.insert(k, get(&p, &format!("TagSpecification.1.Tag.{}.Value", n)).unwrap_or_default());
                n += 1;
            }
            let new_ids: Vec<String> = (0..count).map(|_| new_instance(ext, &image, &itype, &key)).collect();
            for id in &new_ids {
                let inst = ext.instances.get_mut(id).expect("just created");
                inst.monitoring = get(&p, "Monitoring.Enabled").as_deref() == Some("true");
                inst.tags = tags.clone();
                if !groups.is_empty() {
                    inst.groups = groups.clone();
                }
                if let Some(sn) = get(&p, "SubnetId") {
                    inst.subnet = sn;
                }
                if let Some(z) = get(&p, "Placement.AvailabilityZone") {
                    inst.zone = z;
                }
                inst.ebs_optimized = get(&p, "EbsOptimized").as_deref() == Some("true");
                inst.kernel = get(&p, "KernelId").unwrap_or_default();
                inst.client_token = token.clone();
            }
            let items: String = new_ids.iter().map(|id| inst_xml(id, &ext.instances[id])).collect();
            wrap("RunInstances", format!("<reservationId>r-{}</reservationId><ownerId>{}</ownerId><instancesSet>{}</instancesSet>", random_hex(8), ACCOUNT, items))
        }
        "StartInstances" | "StopInstances" | "TerminateInstances" | "RebootInstances" => {
            let new_state: (i64, &str) = match action.as_str() {
                "StartInstances" => (16, "running"),
                "StopInstances" => (80, "stopped"),
                "TerminateInstances" => (48, "terminated"),
                _ => (16, "running"),
            };
            let mut items = String::new();
            for id in &ids {
                let inst = ext.instances.get_mut(id).expect("checked above");
                let prev = inst.state.clone();
                inst.state = (new_state.0, new_state.1.to_string());
                items.push_str(&format!(
                    "<item><instanceId>{}</instanceId><currentState><code>{}</code><name>{}</name></currentState><previousState><code>{}</code><name>{}</name></previousState></item>",
                    id, new_state.0, new_state.1, prev.0, prev.1
                ));
            }
            if action == "RebootInstances" {
                return wrap("RebootInstances", "<return>true</return>".into());
            }
            wrap(&action, format!("<instancesSet>{}</instancesSet>", items))
        }
        "MonitorInstances" | "UnmonitorInstances" => {
            let on = action == "MonitorInstances";
            let mut items = String::new();
            for id in &ids {
                let inst = ext.instances.get_mut(id).expect("checked above");
                inst.monitoring = on;
                items.push_str(&format!("<item><instanceId>{}</instanceId><monitoring><state>{}</state></monitoring></item>", id, if on { "pending" } else { "disabling" }));
            }
            wrap(&action, format!("<instancesSet>{}</instancesSet>", items))
        }
        "CreateTags" | "DeleteTags" => {
            let mut tags = Vec::new();
            let mut n = 1;
            while let Some(k) = get(&p, &format!("Tag.{}.Key", n)) {
                tags.push((k, get(&p, &format!("Tag.{}.Value", n))));
                n += 1;
            }
            for id in &ids {
                let inst = ext.instances.get_mut(id).expect("checked above");
                for (k, v) in &tags {
                    if action == "CreateTags" {
                        inst.tags.insert(k.clone(), v.clone().unwrap_or_default());
                    } else if v.as_ref().is_none_or(|v| inst.tags.get(k) == Some(v)) {
                        inst.tags.remove(k);
                    }
                }
            }
            wrap(&action, "<return>true</return>".into())
        }
        other => err("InvalidAction", &format!("The action {} is not valid for this web service.", other)),
    }
}

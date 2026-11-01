//! Further AWS emulation on the [`AwsMock`] port (SigV4 verified by the parent handler):
//!
//! * Bedrock streaming: `InvokeModelWithResponseStream` and `ConverseStream` answer with AWS
//!   event stream frames (CRC32 checked by the client), `ApplyGuardrail`, and the Agent
//!   Runtime `InvokeAgent` event stream (chunks, traces, attributions).
//! * Kinesis enhanced fan-out: RegisterStreamConsumer / DescribeStreamConsumer (CREATING →
//!   ACTIVE) / ListStreamConsumers / DeregisterStreamConsumer, DescribeStreamSummary and
//!   `SubscribeToShard` over HTTP/2 ([`subscribe_to_shard`], event stream with
//!   `initial-response` + `SubscribeToShardEvent`s, continuation sequence numbers, child shards
//!   at shard end, subscription lifetime [`super::AwsMock::set_kinesis_subscription_millis`]),
//!   and the CBOR flavour of the Kinesis protocol (`application/x-amz-cbor-1.1`).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use camel_core::value::{ObjectMap, Value};

use super::ext::{kerr, kinesis, shard_id, start_position};
use super::{request_id, AwsMock, Shared, ACCOUNT};
use crate::aws::eventstream::{encode_all, Message};
use crate::http::{Response, ServerRequest};
use crate::util::{base64_decode, base64_encode, now_secs, obj, parse_json, percent_decode, xml_escape, Xml};

struct StreamConsumer {
    stream_arn: String,
    name: String,
    status: String,
    created: i64,
}

/// State of the services emulated here.
#[derive(Default)]
pub(super) struct GapsState {
    /// (agent id, session id, input) of InvokeAgent calls.
    pub(super) agent_calls: Vec<(String, String, String)>,
    consumers: BTreeMap<String, StreamConsumer>,
    subscription_millis: Option<u64>,
    subscriptions: usize,
    cbor_requests: usize,
    s3: S3Extra,
    web_tokens: Vec<String>,
    web_identity_requests: usize,
    sns: SnsExtra,
    iam: IamExtra,
    sfn: SfnExtra,
    rek: RekExtra,
}

impl AwsMock {
    /// Lifetime of a `SubscribeToShard` subscription (default 5 minutes, like AWS).
    pub fn set_kinesis_subscription_millis(&self, millis: u64) {
        self.lock().gaps.subscription_millis = Some(millis);
    }
    /// Number of `SubscribeToShard` calls served.
    pub fn kinesis_subscriptions(&self) -> usize {
        self.lock().gaps.subscriptions
    }
    /// Number of Kinesis requests received in CBOR.
    pub fn kinesis_cbor_requests(&self) -> usize {
        self.lock().gaps.cbor_requests
    }
    /// Names of the registered enhanced fan-out consumers.
    pub fn kinesis_consumers(&self) -> Vec<String> {
        self.lock().gaps.consumers.values().map(|c| c.name.clone()).collect()
    }
}

pub(super) fn handle(st: &Shared, service: &str, req: &ServerRequest) -> Option<Response> {
    match service {
        "bedrock" => bedrock(st, req),
        "kinesis" => kinesis_more(st, req),
        "s3" => s3_more(st, req),
        "sqs" => sqs_more(st, req),
        "sns" => sns_more(st, req),
        "iam" => iam_more(st, req),
        "states" => states_more(st, req),
        "rekognition" => rekognition_more(st, req),
        _ => None,
    }
}

pub(super) fn s_(v: &Value, k: &str) -> String {
    v.get(k).map(|x| x.to_string()).unwrap_or_default()
}

pub(super) fn body(req: &ServerRequest) -> Value {
    parse_json(&req.body).unwrap_or_default()
}

pub(super) fn segs(req: &ServerRequest) -> Vec<String> {
    req.path.split('?').next().unwrap_or("").split('/').filter(|s| !s.is_empty()).map(|s| percent_decode(s, false)).collect()
}

pub(super) fn ok(v: Value) -> Response {
    Response::json(200, v.to_json()).with_header("x-amzn-RequestId", request_id())
}

/// REST-JSON error (`x-amzn-ErrorType`).
pub(super) fn rest_err(status: u16, code: &str, msg: &str) -> Response {
    Response::json(status, obj([("message", Value::from(msg))]).to_json()).with_header("x-amzn-ErrorType", format!("{}:", code)).with_header("x-amzn-RequestId", request_id())
}

pub(super) fn target(req: &ServerRequest) -> String {
    req.header("X-Amz-Target").and_then(|t| t.rsplit_once('.')).map(|(_, a)| a.to_string()).unwrap_or_default()
}

fn event_stream(messages: &[Message]) -> Response {
    Response::new(200).with_header("Content-Type", "application/vnd.amazon.eventstream").with_header("x-amzn-RequestId", request_id()).with_body(encode_all(messages))
}

// ---------------------------------------------------------------------------
// Bedrock streaming / guardrails / agents
// ---------------------------------------------------------------------------

fn user_text(b: &Value) -> String {
    if let Some(t) = b.get("inputText") {
        return t.to_string();
    }
    if let Some(p) = b.get("prompt") {
        return p.to_string();
    }
    let msgs = b.get("messages").and_then(|m| m.as_array()).cloned().unwrap_or_default();
    msgs.iter()
        .rev()
        .find(|m| s_(m, "role") == "user")
        .map(|m| match m.get("content") {
            Some(Value::Array(blocks)) => blocks.iter().filter_map(|b| b.get("text").map(|t| t.to_string())).collect::<Vec<_>>().join(""),
            Some(other) => other.to_string(),
            None => String::new(),
        })
        .unwrap_or_default()
}

fn chunk(payload: &Value) -> Message {
    Message::event("chunk", &obj([("bytes", Value::from(base64_encode(payload.to_json().as_bytes())))]))
}

/// Split a reply into word chunks (keeping the separating spaces).
fn pieces(reply: &str) -> Vec<String> {
    let words: Vec<&str> = reply.split(' ').collect();
    words.iter().enumerate().map(|(i, w)| if i + 1 < words.len() { format!("{} ", w) } else { w.to_string() }).collect()
}

fn bedrock(st: &Shared, req: &ServerRequest) -> Option<Response> {
    let segs = segs(req);
    let s: Vec<&str> = segs.iter().map(|x| x.as_str()).collect();
    let b = body(req);
    match (req.method.as_str(), s.as_slice()) {
        ("POST", ["model", model, op @ ("invoke-with-response-stream" | "converse-stream")]) => {
            if model.contains("missing") {
                return Some(rest_err(404, "ResourceNotFoundException", &format!("Could not resolve the foundation model from the provided model identifier: {}", model)));
            }
            let text = user_text(&b);
            if text.contains("throttle-me") {
                // a mid-stream exception message
                return Some(event_stream(&[Message::event("messageStart", &obj([("role", Value::from("assistant"))])), Message::exception("throttlingException", "Too many requests, please wait before trying again.")]));
            }
            let reply = format!("Echo: {}", text);
            let parts = pieces(&reply);
            let n = parts.len() as i64;
            let mut msgs = Vec::new();
            if *op == "converse-stream" {
                msgs.push(Message::event("messageStart", &obj([("role", Value::from("assistant"))])));
                for p in &parts {
                    msgs.push(Message::event("contentBlockDelta", &obj([("contentBlockIndex", Value::Int(0)), ("delta", obj([("text", Value::from(p.clone()))]))])));
                }
                msgs.push(Message::event("contentBlockStop", &obj([("contentBlockIndex", Value::Int(0))])));
                msgs.push(Message::event("messageStop", &obj([("stopReason", Value::from("end_turn"))])));
                let inp = text.split_whitespace().count() as i64;
                msgs.push(Message::event("metadata", &obj([("usage", obj([("inputTokens", Value::Int(inp)), ("outputTokens", Value::Int(n)), ("totalTokens", Value::Int(inp + n))])), ("metrics", obj([("latencyMs", Value::Int(1))]))])));
            } else if model.starts_with("anthropic") {
                msgs.push(chunk(&obj([("type", Value::from("message_start")), ("message", obj([("role", Value::from("assistant"))]))])));
                msgs.push(chunk(&obj([("type", Value::from("content_block_start")), ("index", Value::Int(0))])));
                for p in &parts {
                    msgs.push(chunk(&obj([("type", Value::from("content_block_delta")), ("index", Value::Int(0)), ("delta", obj([("type", Value::from("text_delta")), ("text", Value::from(p.clone()))]))])));
                }
                msgs.push(chunk(&obj([("type", Value::from("message_delta")), ("delta", obj([("stop_reason", Value::from("end_turn"))])), ("usage", obj([("output_tokens", Value::Int(n))]))])));
                msgs.push(chunk(&obj([("type", Value::from("message_stop")), ("amazon-bedrock-invocationMetrics", obj([("outputTokenCount", Value::Int(n))]))])));
            } else {
                for (i, p) in parts.iter().enumerate() {
                    let last = i + 1 == parts.len();
                    let mut m = ObjectMap::new();
                    m.insert("outputText".into(), Value::from(p.clone()));
                    m.insert("index".into(), Value::Int(0));
                    m.insert("completionReason".into(), if last { Value::from("FINISH") } else { Value::Null });
                    if last {
                        m.insert("totalOutputTextTokenCount".into(), Value::Int(n));
                    }
                    msgs.push(chunk(&Value::Object(m)));
                }
            }
            Some(event_stream(&msgs))
        }
        ("POST", ["guardrail", id, "version", version, "apply"]) => {
            if id.contains("missing") {
                return Some(rest_err(404, "ResourceNotFoundException", "The guardrail was not found"));
            }
            let text = b.get("content").and_then(|c| c.as_array()).and_then(|a| a.first().cloned()).and_then(|c| c.get("text").map(|t| s_(t, "text"))).unwrap_or_default();
            let blocked = text.to_ascii_lowercase().contains("forbidden");
            let outputs = if blocked { Value::Array(vec![obj([("text", Value::from("Sorry, I cannot help with that."))])]) } else { Value::Array(vec![]) };
            let assessments = if blocked {
                Value::Array(vec![obj([("topicPolicy", obj([("topics", Value::Array(vec![obj([("name", Value::from("Forbidden")), ("type", Value::from("DENY")), ("action", Value::from("BLOCKED"))])]))]))])])
            } else {
                Value::Array(vec![])
            };
            Some(ok(obj([
                ("action", Value::from(if blocked { "GUARDRAIL_INTERVENED" } else { "NONE" })),
                ("outputs", outputs),
                ("assessments", assessments),
                ("guardrail", Value::from(format!("{}:{}", id, version))),
                ("usage", obj([("topicPolicyUnits", Value::Int(1))])),
            ])))
        }
        ("POST", ["agents", agent, "agentAliases", _alias, "sessions", session, "text"]) => {
            if agent.contains("missing") {
                return Some(rest_err(404, "ResourceNotFoundException", &format!("Agent {} not found", agent)));
            }
            let input = s_(&b, "inputText");
            st.0.lock().unwrap().gaps.agent_calls.push((agent.to_string(), session.to_string(), input.clone()));
            let attrs = b.get("sessionState").and_then(|s| s.get("sessionAttributes")).map(|a| a.to_json()).unwrap_or_default();
            let mut msgs = Vec::new();
            if b.get("enableTrace").and_then(|t| t.as_bool()) == Some(true) {
                msgs.push(Message::event("trace", &obj([("agentId", Value::from(agent.to_string())), ("sessionId", Value::from(session.to_string())), ("trace", obj([("orchestrationTrace", obj([("rationale", obj([("text", Value::from(format!("The user asks: {}", input)))]))]))]))])));
            }
            let reply = if attrs.is_empty() { format!("Agent {} answered: {}", agent, input) } else { format!("Agent {} answered: {} {}", agent, input, attrs) };
            let half = reply.len() / 2;
            let split = (0..=half).rev().find(|i| reply.is_char_boundary(*i)).unwrap_or(0);
            let (a, c) = reply.split_at(split);
            msgs.push(Message::event("chunk", &obj([("bytes", Value::from(base64_encode(a.as_bytes())))])));
            msgs.push(Message::event(
                "chunk",
                &obj([("bytes", Value::from(base64_encode(c.as_bytes()))), ("attribution", obj([("citations", Value::Array(vec![obj([("generatedResponsePart", obj([("textResponsePart", obj([("text", Value::from(c))]))]))])]))]))]),
            ));
            Some(event_stream(&msgs).with_header("x-amz-bedrock-agent-session-id", session.to_string()).with_header("x-amzn-bedrock-agent-content-type", "application/json"))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Kinesis: enhanced fan-out consumers, SubscribeToShard, CBOR
// ---------------------------------------------------------------------------

/// CBOR → JSON form of a Kinesis request (byte strings → base64, as in the JSON protocol).
fn cbor_to_json(v: Value) -> Value {
    match v {
        Value::Bytes(b) => Value::from(base64_encode(&b)),
        Value::Array(a) => Value::Array(a.into_iter().map(cbor_to_json).collect()),
        Value::Object(m) => Value::Object(m.into_iter().map(|(k, v)| (k, cbor_to_json(v))).collect()),
        other => other,
    }
}

/// JSON → CBOR form of a Kinesis response (`Data` fields become byte strings).
fn json_to_cbor(v: Value) -> Value {
    match v {
        Value::Array(a) => Value::Array(a.into_iter().map(json_to_cbor).collect()),
        Value::Object(m) => Value::Object(
            m.into_iter()
                .map(|(k, v)| {
                    let v = if k == "Data" { Value::Bytes(base64_decode(&v.to_string()).unwrap_or_default()) } else { json_to_cbor(v) };
                    (k, v)
                })
                .collect(),
        ),
        other => other,
    }
}

fn stream_name_of(arn: &str) -> String {
    arn.split("stream/").nth(1).unwrap_or("").split('/').next().unwrap_or("").to_string()
}

fn consumer_key(s: &super::State, b: &Value) -> Option<String> {
    let arn = s_(b, "ConsumerARN");
    if arn.is_empty() {
        let (sarn, name) = (s_(b, "StreamARN"), s_(b, "ConsumerName"));
        s.gaps.consumers.iter().find(|(_, c)| c.stream_arn == sarn && c.name == name).map(|(k, _)| k.clone())
    } else {
        s.gaps.consumers.contains_key(&arn).then_some(arn)
    }
}

fn kinesis_more(st: &Shared, req: &ServerRequest) -> Option<Response> {
    if req.header("Content-Type").is_some_and(|c| c.contains("cbor")) {
        let Ok(v) = camel_dataformats::cbor::decode(&req.body) else { return Some(kerr("SerializationException", "Invalid CBOR request")) };
        st.0.lock().unwrap().gaps.cbor_requests += 1;
        let mut json_req = req.clone();
        json_req.body = cbor_to_json(v).to_json().into_bytes();
        json_req.headers.retain(|(k, _)| !k.eq_ignore_ascii_case("Content-Type"));
        json_req.headers.push(("Content-Type".into(), "application/x-amz-json-1.1".into()));
        let resp = kinesis_more(st, &json_req).unwrap_or_else(|| kinesis(st, &json_req));
        let v = parse_json(&resp.body).unwrap_or_default();
        let mut out = Response::new(resp.status).with_body(camel_dataformats::cbor::encode(&json_to_cbor(v))).with_header("Content-Type", "application/x-amz-cbor-1.1");
        for (k, v) in &resp.headers {
            if !k.eq_ignore_ascii_case("Content-Type") && !k.eq_ignore_ascii_case("Content-Length") {
                out = out.with_header(k, v.clone());
            }
        }
        return Some(out);
    }
    let b = body(req);
    let mut s = st.0.lock().unwrap();
    let region = s.region.clone();
    let stream_arn = |name: &str| format!("arn:aws:kinesis:{}:{}:stream/{}", region, ACCOUNT, name);
    match target(req).as_str() {
        "DescribeStreamSummary" => {
            let name = s_(&b, "StreamName");
            let Some(k) = s.ext.streams.get(&name) else { return Some(kerr("ResourceNotFoundException", &format!("Stream {} under account {} not found.", name, ACCOUNT))) };
            let open = k.closed.iter().filter(|c| !**c).count() as i64;
            let consumers = s.gaps.consumers.values().filter(|c| stream_name_of(&c.stream_arn) == name).count() as i64;
            Some(ok(obj([(
                "StreamDescriptionSummary",
                obj([
                    ("StreamName", Value::from(name.clone())),
                    ("StreamARN", Value::from(stream_arn(&name))),
                    ("StreamStatus", Value::from("ACTIVE")),
                    ("OpenShardCount", Value::Int(open)),
                    ("ConsumerCount", Value::Int(consumers)),
                    ("RetentionPeriodHours", Value::Int(24)),
                ]),
            )])))
        }
        "RegisterStreamConsumer" => {
            let (sarn, name) = (s_(&b, "StreamARN"), s_(&b, "ConsumerName"));
            if !s.ext.streams.contains_key(&stream_name_of(&sarn)) {
                return Some(kerr("ResourceNotFoundException", &format!("Stream {} not found", sarn)));
            }
            if s.gaps.consumers.values().any(|c| c.stream_arn == sarn && c.name == name) {
                return Some(kerr("ResourceInUseException", &format!("Consumer {} already exists", name)));
            }
            let created = now_secs();
            let arn = format!("{}/consumer/{}:{}", sarn, name, created);
            s.gaps.consumers.insert(arn.clone(), StreamConsumer { stream_arn: sarn, name: name.clone(), status: "CREATING".into(), created });
            Some(ok(obj([("Consumer", obj([("ConsumerName", Value::from(name)), ("ConsumerARN", Value::from(arn)), ("ConsumerStatus", Value::from("CREATING")), ("ConsumerCreationTimestamp", Value::Int(created))]))])))
        }
        "DescribeStreamConsumer" => {
            let Some(key) = consumer_key(&s, &b) else { return Some(kerr("ResourceNotFoundException", "Consumer not found")) };
            let c = s.gaps.consumers.get_mut(&key).unwrap();
            let status = c.status.clone();
            // becomes ACTIVE after having been seen CREATING once
            c.status = "ACTIVE".into();
            Some(ok(obj([(
                "ConsumerDescription",
                obj([
                    ("ConsumerName", Value::from(c.name.clone())),
                    ("ConsumerARN", Value::from(key.clone())),
                    ("ConsumerStatus", Value::from(status)),
                    ("ConsumerCreationTimestamp", Value::Int(c.created)),
                    ("StreamARN", Value::from(c.stream_arn.clone())),
                ]),
            )])))
        }
        "ListStreamConsumers" => {
            let sarn = s_(&b, "StreamARN");
            let list: Vec<Value> = s
                .gaps
                .consumers
                .iter()
                .filter(|(_, c)| c.stream_arn == sarn)
                .map(|(k, c)| obj([("ConsumerName", Value::from(c.name.clone())), ("ConsumerARN", Value::from(k.clone())), ("ConsumerStatus", Value::from(c.status.clone()))]))
                .collect();
            Some(ok(obj([("Consumers", Value::Array(list))])))
        }
        "DeregisterStreamConsumer" => match consumer_key(&s, &b).and_then(|k| s.gaps.consumers.remove(&k)) {
            Some(_) => Some(ok(obj([]))),
            None => Some(kerr("ResourceNotFoundException", "Consumer not found")),
        },
        _ => None,
    }
}

fn h2_json_error(stream: &camel_grpc::h2::H2Stream, code: &str, msg: &str) {
    let body = obj([("__type", Value::from(code)), ("message", Value::from(msg))]).to_json();
    let _ = camel_grpc::h2::ServerRequest::respond(stream, 400, &[("content-type", "application/x-amz-json-1.1"), ("x-amzn-errortype", code)], body.as_bytes());
}

/// Serve one `SubscribeToShard` call (HTTP/2): stream `SubscribeToShardEvent`s for the
/// subscription lifetime, or until the shard ends.
pub(super) fn subscribe_to_shard(st: &Shared, req: &ServerRequest, stream: &camel_grpc::h2::H2Stream) {
    let b = body(req);
    let (consumer, shard_name) = (s_(&b, "ConsumerARN"), s_(&b, "ShardId"));
    let pos_spec = b.get("StartingPosition").cloned().unwrap_or_default();
    let (stream_name, lifetime, idx, mut pos) = {
        let mut s = st.0.lock().unwrap();
        let Some(c) = s.gaps.consumers.get(&consumer) else { return h2_json_error(stream, "ResourceNotFoundException", &format!("Consumer {} not found", consumer)) };
        if c.status != "ACTIVE" {
            return h2_json_error(stream, "ResourceInUseException", "Consumer is not ACTIVE");
        }
        let name = stream_name_of(&c.stream_arn);
        let lifetime = Duration::from_millis(s.gaps.subscription_millis.unwrap_or(300_000));
        let Some(k) = s.ext.streams.get(&name) else { return h2_json_error(stream, "ResourceNotFoundException", "Stream not found") };
        let Some(idx) = (0..k.shards.len()).find(|i| shard_id(*i) == shard_name) else { return h2_json_error(stream, "ResourceNotFoundException", &format!("Shard {} not found", shard_name)) };
        let seq: u128 = s_(&pos_spec, "SequenceNumber").parse().unwrap_or(0);
        let ts = pos_spec.get("Timestamp").and_then(|t| t.as_f64()).unwrap_or(0.0);
        let Some(pos) = start_position(&k.shards[idx], &s_(&pos_spec, "Type"), seq, ts) else { return h2_json_error(stream, "InvalidArgumentException", "Invalid StartingPosition") };
        s.gaps.subscriptions += 1;
        (name, lifetime, idx, pos)
    };
    let hs = [(":status".to_string(), "200".to_string()), ("content-type".to_string(), "application/vnd.amazon.eventstream".to_string()), ("x-amzn-requestid".to_string(), request_id())];
    if stream.send_headers(&hs, false).is_err() {
        return;
    }
    if stream.send_data(&Message::event("initial-response", &obj([])).encode(), false).is_err() {
        return;
    }
    let start = Instant::now();
    let mut last_event = Instant::now() - Duration::from_secs(1);
    let mut continuation: Option<String> = None;
    loop {
        let (records, ended, children) = {
            let s = st.0.lock().unwrap();
            let Some(k) = s.ext.streams.get(&stream_name) else { break };
            let recs = &k.shards[idx];
            let batch: Vec<Value> = recs[pos.min(recs.len())..]
                .iter()
                .take(1000)
                .map(|r| {
                    obj([
                        ("SequenceNumber", Value::from(r.seq.to_string())),
                        ("ApproximateArrivalTimestamp", Value::Float(r.arrival_millis as f64 / 1000.0)),
                        ("Data", Value::from(base64_encode(&r.data))),
                        ("PartitionKey", Value::from(r.partition_key.clone())),
                    ])
                })
                .collect();
            if let Some(last) = batch.last() {
                continuation = Some(s_(last, "SequenceNumber"));
            } else if continuation.is_none() {
                continuation = Some(pos.checked_sub(1).and_then(|p| recs.get(p)).map(|r| r.seq.to_string()).unwrap_or_else(|| "49000000000000000000".into()));
            }
            pos += batch.len();
            let ended = k.closed[idx] && pos >= recs.len();
            let children: Vec<Value> = (0..k.shards.len())
                .filter(|j| k.parents.get(*j).is_some_and(|(p, a)| *p == Some(idx) || *a == Some(idx)))
                .map(|j| obj([("ShardId", Value::from(shard_id(j))), ("ParentShards", Value::Array(vec![Value::from(shard_id(idx))]))]))
                .collect();
            (batch, ended, children)
        };
        if !records.is_empty() || ended || last_event.elapsed() >= Duration::from_millis(200) {
            let mut m = ObjectMap::new();
            m.insert("Records".into(), Value::Array(records));
            m.insert("MillisBehindLatest".into(), Value::Int(0));
            if ended {
                m.insert("ChildShards".into(), Value::Array(children));
            } else {
                m.insert("ContinuationSequenceNumber".into(), Value::from(continuation.clone().unwrap_or_default()));
            }
            if stream.send_data(&Message::event("SubscribeToShardEvent", &Value::Object(m)).encode(), false).is_err() {
                return;
            }
            last_event = Instant::now();
        }
        if ended || start.elapsed() >= lifetime || stream.is_remote_closed() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = stream.send_data(&[], true);
}

// ---------------------------------------------------------------------------
// S3: tagging, ACLs, object lock, versioning, restore, DeleteObjects, SSE
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
pub(super) struct ObjExtra {
    tags: Vec<(String, String)>,
    acl: String,
    retention: Option<(String, String)>,
    legal_hold: Option<String>,
    sse: Option<String>,
    kms_key: Option<String>,
    ssec_md5: Option<String>,
    restored_until: Option<i64>,
}

#[derive(Default)]
pub(super) struct S3Extra {
    objects: BTreeMap<(String, String), ObjExtra>,
    pending: BTreeMap<(String, String), ObjExtra>,
    versioning: BTreeMap<String, String>,
    /// (bucket, key) -> versions, oldest first: (version id, object or None for a delete marker)
    versions: BTreeMap<(String, String), Vec<(String, Option<super::S3Obj>)>>,
}

impl AwsMock {
    /// Server-side encryption recorded for an object: (algorithm, KMS key id, SSE-C key MD5).
    pub fn s3_object_encryption(&self, bucket: &str, key: &str) -> Option<(Option<String>, Option<String>, Option<String>)> {
        self.lock().gaps.s3.objects.get(&(bucket.to_string(), key.to_string())).map(|e| (e.sse.clone(), e.kms_key.clone(), e.ssec_md5.clone()))
    }
    /// Tags of an object.
    pub fn s3_object_tags(&self, bucket: &str, key: &str) -> Vec<(String, String)> {
        self.lock().gaps.s3.objects.get(&(bucket.to_string(), key.to_string())).map(|e| e.tags.clone()).unwrap_or_default()
    }
    /// Store an object with a storage class (e.g. `GLACIER`).
    pub fn put_object_with_class(&self, bucket: &str, key: &str, data: &[u8], storage_class: &str) {
        let mut s = self.lock();
        let b = s.buckets.entry(bucket.into()).or_default();
        b.objects.insert(key.into(), super::new_obj(data.to_vec(), "binary/octet-stream".into(), vec![], storage_class.into()));
    }
}

fn qflag(req: &ServerRequest, name: &str) -> bool {
    req.query.split('&').any(|p| p == name || p.starts_with(&format!("{}=", name)))
}

fn qval(req: &ServerRequest, name: &str) -> Option<String> {
    crate::util::parse_form(&req.query).into_iter().find(|(k, _)| k == name).map(|(_, v)| v)
}

fn xml_ok(body: String) -> Response {
    Response::xml(200, format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>{}", body))
}

fn check_md5(req: &ServerRequest) -> Option<Response> {
    match req.header("Content-MD5") {
        None => Some(super::s3_error(400, "InvalidRequest", "Missing required header for this request: Content-MD5")),
        Some(m) if m != base64_encode(&crate::util::md5(&req.body)) => Some(super::s3_error(400, "BadDigest", "The Content-MD5 you specified did not match what we received.")),
        _ => None,
    }
}

/// Lock check for a delete; `Some(error)` when the object may not be deleted.
fn locked(e: Option<&ObjExtra>, req: &ServerRequest) -> Option<Response> {
    let e = e?;
    if e.legal_hold.as_deref() == Some("ON") {
        return Some(super::s3_error(403, "AccessDenied", "Access Denied because object protected by object lock."));
    }
    if let Some((mode, until)) = &e.retention {
        if crate::util::parse_iso8601(until).unwrap_or(0) > now_secs() {
            let bypass = req.header("x-amz-bypass-governance-retention") == Some("true");
            if mode == "COMPLIANCE" || !bypass {
                return Some(super::s3_error(403, "AccessDenied", "Access Denied because object protected by object lock."));
            }
        }
    }
    None
}

fn ssec_md5(req: &ServerRequest) -> Result<Option<String>, Response> {
    let Some(key) = req.header("x-amz-server-side-encryption-customer-key") else { return Ok(None) };
    let md5 = req.header("x-amz-server-side-encryption-customer-key-MD5").unwrap_or("");
    let raw = base64_decode(key).unwrap_or_default();
    if raw.len() != 32 || base64_encode(&crate::util::md5(&raw)) != md5 {
        return Err(super::s3_error(400, "InvalidArgument", "The calculated MD5 hash of the key did not match the hash that was provided."));
    }
    Ok(Some(md5.to_string()))
}

fn upload_extra(req: &ServerRequest) -> Result<ObjExtra, Response> {
    let mut e = ObjExtra { acl: req.header("x-amz-acl").unwrap_or("private").to_string(), ..Default::default() };
    e.ssec_md5 = ssec_md5(req)?;
    e.sse = req.header("x-amz-server-side-encryption").map(|s| s.to_string()).or_else(|| e.ssec_md5.as_ref().map(|_| "SSE-C".to_string()));
    e.kms_key = req.header("x-amz-server-side-encryption-aws-kms-key-id").map(|s| s.to_string());
    if e.sse.as_deref() == Some("aws:kms") && e.kms_key.is_none() {
        e.kms_key = Some(format!("arn:aws:kms:us-east-1:{}:alias/aws/s3", ACCOUNT));
    }
    if let Some(t) = req.header("x-amz-tagging") {
        e.tags = crate::util::parse_form(t);
    }
    if let (Some(m), Some(u)) = (req.header("x-amz-object-lock-mode"), req.header("x-amz-object-lock-retain-until-date")) {
        e.retention = Some((m.to_string(), u.to_string()));
    }
    e.legal_hold = req.header("x-amz-object-lock-legal-hold").map(|s| s.to_string());
    Ok(e)
}

fn with_sse_headers(mut r: Response, e: &ObjExtra) -> Response {
    match e.sse.as_deref() {
        Some("SSE-C") => {
            r = r.with_header("x-amz-server-side-encryption-customer-algorithm", "AES256").with_header("x-amz-server-side-encryption-customer-key-MD5", e.ssec_md5.clone().unwrap_or_default());
        }
        Some(alg) => {
            r = r.with_header("x-amz-server-side-encryption", alg.to_string());
            if let Some(k) = &e.kms_key {
                r = r.with_header("x-amz-server-side-encryption-aws-kms-key-id", k.clone());
            }
        }
        None => {}
    }
    r
}

fn version_xml(key: &str, vid: &str, latest: bool, o: &Option<super::S3Obj>) -> String {
    match o {
        Some(o) => format!(
            "<Version><Key>{}</Key><VersionId>{}</VersionId><IsLatest>{}</IsLatest><ETag>{}</ETag><Size>{}</Size><StorageClass>{}</StorageClass></Version>",
            xml_escape(key),
            vid,
            latest,
            xml_escape(&o.etag),
            o.data.len(),
            o.storage_class
        ),
        None => format!("<DeleteMarker><Key>{}</Key><VersionId>{}</VersionId><IsLatest>{}</IsLatest></DeleteMarker>", xml_escape(key), vid, latest),
    }
}

fn s3_more(st: &Shared, req: &ServerRequest) -> Option<Response> {
    let (bucket, key) = super::s3_target(req);
    if bucket.is_empty() {
        return None;
    }
    let Some(key) = key else { return s3_bucket_more(st, req, &bucket) };
    let id = (bucket.clone(), key.clone());
    let sub = ["tagging", "acl", "retention", "legal-hold", "restore"].into_iter().find(|q| qflag(req, q));
    if let Some(sub) = sub {
        let mut s = st.0.lock().unwrap();
        let Some(obj_) = s.buckets.get(&bucket).and_then(|b| b.objects.get(&key)).cloned() else { return Some(super::s3_error(404, "NoSuchKey", "The specified key does not exist.")) };
        if matches!(req.method.as_str(), "PUT" | "POST") && sub != "restore" && sub != "acl" {
            if let Some(r) = check_md5(req) {
                return Some(r);
            }
        }
        let e = s.gaps.s3.objects.entry(id.clone()).or_insert_with(|| ObjExtra { acl: "private".into(), ..Default::default() });
        let x = Xml::parse(&req.text()).ok();
        let text = |name: &str| x.as_ref().and_then(|x| x.find_text(name)).unwrap_or_default();
        return Some(match (sub, req.method.as_str()) {
            ("tagging", "GET") => {
                let tags: String = e.tags.iter().map(|(k, v)| format!("<Tag><Key>{}</Key><Value>{}</Value></Tag>", xml_escape(k), xml_escape(v))).collect();
                xml_ok(format!("<Tagging xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><TagSet>{}</TagSet></Tagging>", tags))
            }
            ("tagging", "PUT") => {
                let Some(x) = &x else { return Some(super::s3_error(400, "MalformedXML", "The XML you provided was not well-formed")) };
                e.tags = x.descendants("Tag").into_iter().map(|t| (x.child_text(t, "Key").unwrap_or_default(), x.child_text(t, "Value").unwrap_or_default())).collect();
                Response::new(200)
            }
            ("tagging", "DELETE") => {
                e.tags.clear();
                Response::new(204)
            }
            ("acl", "GET") => {
                let mut grants = String::from("<Grant><Grantee xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:type=\"CanonicalUser\"><ID>mock</ID></Grantee><Permission>FULL_CONTROL</Permission></Grant>");
                match e.acl.as_str() {
                    "public-read" => grants.push_str("<Grant><Grantee xsi:type=\"Group\"><URI>http://acs.amazonaws.com/groups/global/AllUsers</URI></Grantee><Permission>READ</Permission></Grant>"),
                    "public-read-write" => grants.push_str(
                        "<Grant><Grantee xsi:type=\"Group\"><URI>http://acs.amazonaws.com/groups/global/AllUsers</URI></Grantee><Permission>READ</Permission></Grant><Grant><Grantee xsi:type=\"Group\"><URI>http://acs.amazonaws.com/groups/global/AllUsers</URI></Grantee><Permission>WRITE</Permission></Grant>",
                    ),
                    "authenticated-read" => grants.push_str("<Grant><Grantee xsi:type=\"Group\"><URI>http://acs.amazonaws.com/groups/global/AuthenticatedUsers</URI></Grantee><Permission>READ</Permission></Grant>"),
                    _ => {}
                }
                xml_ok(format!("<AccessControlPolicy xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Owner><ID>mock</ID></Owner><AccessControlList>{}</AccessControlList></AccessControlPolicy>", grants))
            }
            ("acl", "PUT") => {
                if let Some(acl) = req.header("x-amz-acl") {
                    if !["private", "public-read", "public-read-write", "authenticated-read", "bucket-owner-read", "bucket-owner-full-control"].contains(&acl) {
                        return Some(super::s3_error(400, "InvalidArgument", "Invalid canned ACL"));
                    }
                    e.acl = acl.to_string();
                } else if let Some(r) = check_md5(req) {
                    return Some(r);
                } else if x.as_ref().is_some_and(|x| x.descendants("Grant").len() > 1) {
                    e.acl = "public-read".into();
                }
                Response::new(200)
            }
            ("retention", "GET") => match &e.retention {
                Some((m, u)) => xml_ok(format!("<Retention xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Mode>{}</Mode><RetainUntilDate>{}</RetainUntilDate></Retention>", m, xml_escape(u))),
                None => super::s3_error(404, "NoSuchObjectLockConfiguration", "The specified object does not have a ObjectLock configuration"),
            },
            ("retention", "PUT") => {
                let (mode, until) = (text("Mode"), text("RetainUntilDate"));
                if !matches!(mode.as_str(), "GOVERNANCE" | "COMPLIANCE") || crate::util::parse_iso8601(&until).is_none() {
                    return Some(super::s3_error(400, "MalformedXML", "The XML you provided was not well-formed"));
                }
                // shortening a COMPLIANCE retention is not allowed
                if let Some((m, u)) = &e.retention {
                    let shorter = crate::util::parse_iso8601(&until) < crate::util::parse_iso8601(u);
                    if shorter && (m == "COMPLIANCE" || req.header("x-amz-bypass-governance-retention") != Some("true")) {
                        return Some(super::s3_error(403, "AccessDenied", "Access Denied because object protected by object lock."));
                    }
                }
                e.retention = Some((mode, until));
                Response::new(200)
            }
            ("legal-hold", "GET") => match &e.legal_hold {
                Some(h) => xml_ok(format!("<LegalHold xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Status>{}</Status></LegalHold>", h)),
                None => super::s3_error(404, "NoSuchObjectLockConfiguration", "The specified object does not have a ObjectLock configuration"),
            },
            ("legal-hold", "PUT") => {
                let status = text("Status");
                if !matches!(status.as_str(), "ON" | "OFF") {
                    return Some(super::s3_error(400, "MalformedXML", "The XML you provided was not well-formed"));
                }
                e.legal_hold = Some(status);
                Response::new(200)
            }
            ("restore", "POST") => {
                if !matches!(obj_.storage_class.as_str(), "GLACIER" | "DEEP_ARCHIVE") {
                    return Some(super::s3_error(403, "InvalidObjectState", "Restore is not allowed for the object's current storage class"));
                }
                let days: i64 = text("Days").parse().unwrap_or(1);
                let status = if e.restored_until.is_some() { 200 } else { 202 };
                e.restored_until = Some(now_secs() + days * 86_400);
                Response::new(status)
            }
            _ => super::s3_error(405, "MethodNotAllowed", "Method not allowed"),
        });
    }
    if qflag(req, "uploads") && req.method == "POST" {
        let e = match upload_extra(req) {
            Ok(e) => e,
            Err(r) => return Some(r),
        };
        st.0.lock().unwrap().gaps.s3.pending.insert(id, e);
        return None;
    }
    if qflag(req, "partNumber") {
        let pending = st.0.lock().unwrap().gaps.s3.pending.get(&id).and_then(|e| e.ssec_md5.clone());
        if let Some(md5) = pending {
            match ssec_md5(req) {
                Ok(Some(m)) if m == md5 => {}
                Ok(_) => return Some(super::s3_error(400, "InvalidRequest", "The multipart upload initiate requested encryption. Subsequent part requests must include the appropriate encryption parameters.")),
                Err(r) => return Some(r),
            }
        }
        return None;
    }
    if qflag(req, "uploadId") {
        if req.method == "POST" {
            let r = super::s3(st, req);
            let mut s = st.0.lock().unwrap();
            if let Some(e) = s.gaps.s3.pending.remove(&id) {
                if r.status == 200 {
                    s.gaps.s3.objects.insert(id, e);
                }
            }
            return Some(r);
        }
        return None;
    }
    match req.method.as_str() {
        "PUT" if req.header("x-amz-copy-source").is_none() => {
            let exists = st.0.lock().unwrap().buckets.get(&bucket).is_some_and(|b| b.objects.contains_key(&key));
            if req.header("If-None-Match") == Some("*") && exists {
                return Some(super::s3_error(412, "PreconditionFailed", "At least one of the pre-conditions you specified did not hold"));
            }
            let e = match upload_extra(req) {
                Ok(e) => e,
                Err(r) => return Some(r),
            };
            let r = super::s3(st, req);
            if r.status != 200 {
                return Some(r);
            }
            let mut s = st.0.lock().unwrap();
            let mut r = with_sse_headers(r, &e);
            s.gaps.s3.objects.insert(id.clone(), e);
            if s.gaps.s3.versioning.get(&bucket).map(|v| v.as_str()) == Some("Enabled") {
                let vid = crate::util::random_hex(16);
                let o = s.buckets.get(&bucket).and_then(|b| b.objects.get(&key)).cloned();
                s.gaps.s3.versions.entry(id).or_default().push((vid.clone(), o));
                r = r.with_header("x-amz-version-id", vid);
            }
            Some(r)
        }
        "GET" | "HEAD" => {
            let s = st.0.lock().unwrap();
            if let Some(vid) = qval(req, "versionId") {
                let Some(o) = s.gaps.s3.versions.get(&id).and_then(|v| v.iter().find(|(x, _)| *x == vid)).map(|(_, o)| o.clone()) else { return Some(super::s3_error(404, "NoSuchVersion", "The specified version does not exist.")) };
                let Some(o) = o else { return Some(super::s3_error(405, "MethodNotAllowed", "The specified method is not allowed against this resource.").with_header("x-amz-delete-marker", "true")) };
                let body = if req.method == "HEAD" { Vec::new() } else { o.data.clone() };
                return Some(Response::new(200).with_header("ETag", o.etag.clone()).with_header("Content-Type", o.content_type.clone()).with_header("x-amz-version-id", vid).with_header("Content-Length", o.data.len().to_string()).with_body(body));
            }
            let e = s.gaps.s3.objects.get(&id).cloned().unwrap_or_default();
            let class = s.buckets.get(&bucket).and_then(|b| b.objects.get(&key)).map(|o| o.storage_class.clone());
            let latest = s.gaps.s3.versions.get(&id).and_then(|v| v.last()).map(|(v, _)| v.clone());
            drop(s);
            if let Some(md5) = &e.ssec_md5 {
                match ssec_md5(req) {
                    Ok(Some(m)) if &m == md5 => {}
                    Ok(Some(_)) => return Some(super::s3_error(403, "AccessDenied", "The provided customer key does not match the key used to encrypt the object.")),
                    Ok(None) => return Some(super::s3_error(400, "InvalidRequest", "The object was stored using a form of Server Side Encryption. The correct parameters must be provided to retrieve the object.")),
                    Err(r) => return Some(r),
                }
            }
            let archived = class.as_deref().is_some_and(|c| c == "GLACIER" || c == "DEEP_ARCHIVE");
            if archived && req.method == "GET" && e.restored_until.is_none() {
                return Some(super::s3_error(403, "InvalidObjectState", "The operation is not valid for the object's storage class"));
            }
            let mut r = with_sse_headers(super::s3(st, req), &e);
            if let Some(until) = e.restored_until {
                r = r.with_header("x-amz-restore", format!("ongoing-request=\"false\", expiry-date=\"{}\"", crate::util::http_date(until)));
            }
            if let Some(v) = latest {
                r = r.with_header("x-amz-version-id", v);
            }
            Some(r)
        }
        "DELETE" => {
            let mut s = st.0.lock().unwrap();
            if let Some(r) = locked(s.gaps.s3.objects.get(&id), req) {
                return Some(r);
            }
            if let Some(vid) = qval(req, "versionId") {
                let versions = s.gaps.s3.versions.entry(id.clone()).or_default();
                versions.retain(|(v, _)| *v != vid);
                let current = versions.last().and_then(|(_, o)| o.clone());
                let b = s.buckets.entry(bucket.clone()).or_default();
                match current {
                    Some(o) => {
                        b.objects.insert(key.clone(), o);
                    }
                    None => {
                        b.objects.remove(&key);
                    }
                }
                return Some(Response::new(204).with_header("x-amz-version-id", vid));
            }
            if s.gaps.s3.versioning.get(&bucket).map(|v| v.as_str()) == Some("Enabled") {
                let vid = crate::util::random_hex(16);
                s.gaps.s3.versions.entry(id).or_default().push((vid.clone(), None));
                if let Some(b) = s.buckets.get_mut(&bucket) {
                    b.objects.remove(&key);
                }
                return Some(Response::new(204).with_header("x-amz-delete-marker", "true").with_header("x-amz-version-id", vid));
            }
            s.gaps.s3.objects.remove(&id);
            None
        }
        _ => None,
    }
}

fn s3_bucket_more(st: &Shared, req: &ServerRequest, bucket: &str) -> Option<Response> {
    if qflag(req, "versioning") {
        let mut s = st.0.lock().unwrap();
        if !s.buckets.contains_key(bucket) {
            return Some(super::s3_error(404, "NoSuchBucket", "The specified bucket does not exist"));
        }
        return Some(match req.method.as_str() {
            "PUT" => {
                let status = Xml::parse(&req.text()).ok().and_then(|x| x.find_text("Status")).unwrap_or_default();
                if !matches!(status.as_str(), "Enabled" | "Suspended") {
                    return Some(super::s3_error(400, "MalformedXML", "The XML you provided was not well-formed"));
                }
                s.gaps.s3.versioning.insert(bucket.to_string(), status);
                Response::new(200)
            }
            _ => {
                let status = s.gaps.s3.versioning.get(bucket).map(|v| format!("<Status>{}</Status>", v)).unwrap_or_default();
                xml_ok(format!("<VersioningConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">{}</VersioningConfiguration>", status))
            }
        });
    }
    if qflag(req, "versions") && req.method == "GET" {
        let s = st.0.lock().unwrap();
        let prefix = qval(req, "prefix").unwrap_or_default();
        let mut xml = format!("<ListVersionsResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Name>{}</Name><IsTruncated>false</IsTruncated>", xml_escape(bucket));
        for ((b, k), versions) in &s.gaps.s3.versions {
            if b != bucket || !k.starts_with(&prefix) {
                continue;
            }
            for (i, (vid, o)) in versions.iter().enumerate().rev() {
                xml.push_str(&version_xml(k, vid, i + 1 == versions.len(), o));
            }
        }
        xml.push_str("</ListVersionsResult>");
        return Some(xml_ok(xml));
    }
    if qflag(req, "delete") && req.method == "POST" {
        if let Some(r) = check_md5(req) {
            return Some(r);
        }
        let Ok(x) = Xml::parse(&req.text()) else { return Some(super::s3_error(400, "MalformedXML", "The XML you provided was not well-formed")) };
        let keys: Vec<String> = x.descendants("Object").into_iter().filter_map(|o| x.child_text(o, "Key")).collect();
        let mut s = st.0.lock().unwrap();
        if !s.buckets.contains_key(bucket) {
            return Some(super::s3_error(404, "NoSuchBucket", "The specified bucket does not exist"));
        }
        let mut out = String::from("<DeleteResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">");
        for k in keys {
            let id = (bucket.to_string(), k.clone());
            if locked(s.gaps.s3.objects.get(&id), req).is_some() {
                out.push_str(&format!("<Error><Key>{}</Key><Code>AccessDenied</Code><Message>Access Denied because object protected by object lock.</Message></Error>", xml_escape(&k)));
                continue;
            }
            s.gaps.s3.objects.remove(&id);
            if let Some(b) = s.buckets.get_mut(bucket) {
                b.objects.remove(&k);
            }
            out.push_str(&format!("<Deleted><Key>{}</Key></Deleted>", xml_escape(&k)));
        }
        out.push_str("</DeleteResult>");
        return Some(xml_ok(out));
    }
    None
}

// ---------------------------------------------------------------------------
// STS AssumeRoleWithWebIdentity (unsigned)
// ---------------------------------------------------------------------------

impl AwsMock {
    /// Accept `token` as an OIDC web identity token (for `AssumeRoleWithWebIdentity`).
    pub fn add_web_identity_token(&self, token: &str) {
        self.lock().gaps.web_tokens.push(token.to_string());
    }
    /// Number of `AssumeRoleWithWebIdentity` calls served.
    pub fn web_identity_requests(&self) -> usize {
        self.lock().gaps.web_identity_requests
    }
}

pub(super) fn assume_role_with_web_identity(st: &Shared, req: &ServerRequest) -> Response {
    const NS: &str = "https://sts.amazonaws.com/doc/2011-06-15/";
    let p = crate::util::parse_form(&req.text());
    let get = |k: &str| p.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap_or_default();
    let err = |status: u16, code: &str, msg: &str| Response::xml(status, format!("<ErrorResponse xmlns=\"{}\"><Error><Type>Sender</Type><Code>{}</Code><Message>{}</Message></Error><RequestId>{}</RequestId></ErrorResponse>", NS, code, xml_escape(msg), request_id()));
    let mut s = st.0.lock().unwrap();
    s.gaps.web_identity_requests += 1;
    let (role, session, token) = (get("RoleArn"), get("RoleSessionName"), get("WebIdentityToken"));
    if role.is_empty() || session.is_empty() {
        return err(400, "ValidationError", "RoleArn and RoleSessionName are required");
    }
    if !s.gaps.web_tokens.contains(&token) {
        return err(400, "InvalidIdentityToken", "Couldn't retrieve verification key from your identity provider");
    }
    let Some(role_name) = role.strip_prefix(&format!("arn:aws:iam::{}:role/", ACCOUNT)).map(|r| r.to_string()) else { return err(403, "AccessDenied", "Not authorized to perform sts:AssumeRoleWithWebIdentity") };
    let ak = format!("ASIA{}", crate::util::random_hex(8).to_uppercase());
    let sk = crate::util::random_hex(20);
    let tok = base64_encode(format!("IQoJb3JpZ2luX2Vj{}", crate::util::random_hex(24)).as_bytes());
    s.credentials.insert(ak.clone(), sk.clone());
    s.session_tokens.insert(ak.clone(), tok.clone());
    Response::xml(
        200,
        format!(
            "<AssumeRoleWithWebIdentityResponse xmlns=\"{ns}\"><AssumeRoleWithWebIdentityResult><Credentials><AccessKeyId>{ak}</AccessKeyId><SecretAccessKey>{sk}</SecretAccessKey><SessionToken>{tok}</SessionToken><Expiration>{exp}</Expiration></Credentials><SubjectFromWebIdentityToken>system:serviceaccount:default:camel</SubjectFromWebIdentityToken><AssumedRoleUser><Arn>arn:aws:sts::{acct}:assumed-role/{role}/{session}</Arn><AssumedRoleId>AROA{rid}:{session}</AssumedRoleId></AssumedRoleUser><Provider>oidc.eks.amazonaws.com</Provider></AssumeRoleWithWebIdentityResult><ResponseMetadata><RequestId>{req}</RequestId></ResponseMetadata></AssumeRoleWithWebIdentityResponse>",
            ns = NS,
            ak = ak,
            sk = sk,
            tok = xml_escape(&tok),
            exp = crate::util::iso8601(now_secs() + 3600),
            acct = ACCOUNT,
            role = xml_escape(&role_name),
            session = xml_escape(&session),
            rid = crate::util::random_hex(6).to_uppercase(),
            req = request_id()
        ),
    )
}

// ---------------------------------------------------------------------------
// SQS: queue attributes, redrive (dead-letter queues), batch deletes
// ---------------------------------------------------------------------------

impl AwsMock {
    /// Attributes of an emulated queue.
    pub fn queue_attributes(&self, name: &str) -> BTreeMap<String, String> {
        self.lock().queues.get(name).map(|q| q.attributes.clone()).unwrap_or_default()
    }
}

fn sqs_json_err(code: &str, msg: &str) -> Response {
    super::json_error(400, "com.amazonaws.sqs", code, msg)
}

pub(super) fn sqs_more(st: &Shared, req: &ServerRequest) -> Option<Response> {
    let action = req.header("X-Amz-Target").and_then(|t| t.strip_prefix("AmazonSQS."))?.to_string();
    let b = body(req);
    let ok_ = |v: Value| Response::json(200, v.to_json()).with_header("Content-Type", "application/x-amz-json-1.0");
    let name = super::queue_name_of(&s_(&b, "QueueUrl"));
    let region = st.0.lock().unwrap().region.clone();
    match action.as_str() {
        "GetQueueAttributes" => {
            let s = st.0.lock().unwrap();
            let Some(q) = s.queues.get(&name) else { return Some(sqs_json_err("QueueDoesNotExist", "The specified queue does not exist.")) };
            let now = crate::util::now_millis();
            let mut m: ObjectMap = q.attributes.iter().map(|(k, v)| (k.clone(), Value::from(v.clone()))).collect();
            m.insert("QueueArn".into(), Value::from(format!("arn:aws:sqs:{}:{}:{}", region, ACCOUNT, name)));
            m.insert("ApproximateNumberOfMessages".into(), Value::from(q.messages.iter().filter(|m| m.visible_at <= now).count().to_string()));
            m.insert("ApproximateNumberOfMessagesNotVisible".into(), Value::from(q.messages.iter().filter(|m| m.visible_at > now).count().to_string()));
            Some(ok_(obj([("Attributes", Value::Object(m))])))
        }
        "SetQueueAttributes" => {
            let mut s = st.0.lock().unwrap();
            let Some(q) = s.queues.get_mut(&name) else { return Some(sqs_json_err("QueueDoesNotExist", "The specified queue does not exist.")) };
            if let Some(Value::Object(a)) = b.get("Attributes") {
                for (k, v) in a {
                    q.attributes.insert(k.clone(), v.to_string());
                }
            }
            Some(ok_(obj([])))
        }
        "DeleteMessageBatch" => {
            let mut s = st.0.lock().unwrap();
            let Some(q) = s.queues.get_mut(&name) else { return Some(sqs_json_err("QueueDoesNotExist", "The specified queue does not exist.")) };
            let (mut okl, mut failed) = (Vec::new(), Vec::new());
            for e in b.get("Entries").and_then(|e| e.as_array()).cloned().unwrap_or_default() {
                let handle = s_(&e, "ReceiptHandle");
                let before = q.messages.len();
                q.messages.retain(|m| m.receipt.as_deref() != Some(handle.as_str()));
                if q.messages.len() < before {
                    okl.push(obj([("Id", e.get("Id").cloned().unwrap_or_default())]));
                } else {
                    failed.push(obj([("Id", e.get("Id").cloned().unwrap_or_default()), ("Code", Value::from("ReceiptHandleIsInvalid")), ("SenderFault", Value::Bool(true)), ("Message", Value::from("The input receipt handle is invalid."))]));
                }
            }
            Some(ok_(obj([("Successful", Value::Array(okl)), ("Failed", Value::Array(failed))])))
        }
        "SendMessage" | "SendMessageBatch" => {
            let s = st.0.lock().unwrap();
            let q = s.queues.get(&name)?;
            let max = q.attributes.get("MaximumMessageSize").and_then(|m| m.parse::<usize>().ok());
            let delay = q.attributes.get("DelaySeconds").and_then(|d| d.parse::<i64>().ok());
            drop(s);
            let entries: Vec<Value> = if action == "SendMessage" { vec![b.clone()] } else { b.get("Entries").and_then(|e| e.as_array()).cloned().unwrap_or_default() };
            if let Some(max) = max {
                if entries.iter().any(|e| s_(e, "MessageBody").len() > max) {
                    return Some(sqs_json_err("InvalidParameterValue", &format!("One or more parameters are invalid. Reason: Message must be shorter than {} bytes.", max)));
                }
            }
            let delay = delay.filter(|d| *d > 0)?;
            // apply the queue's delivery delay to messages without their own
            let mut b2 = b.clone();
            if let Value::Object(m) = &mut b2 {
                if action == "SendMessage" {
                    m.entry("DelaySeconds".to_string()).or_insert(Value::Int(delay));
                } else if let Some(Value::Array(es)) = m.get_mut("Entries") {
                    for e in es.iter_mut() {
                        if let Value::Object(em) = e {
                            em.entry("DelaySeconds".to_string()).or_insert(Value::Int(delay));
                        }
                    }
                }
            }
            let mut r2 = req.clone();
            r2.body = b2.to_json().into_bytes();
            Some(super::sqs(st, &r2))
        }
        "ReceiveMessage" => {
            // redrive: messages received maxReceiveCount times move to the dead-letter queue
            let mut s = st.0.lock().unwrap();
            let policy = s.queues.get(&name)?.attributes.get("RedrivePolicy").and_then(|p| parse_json(p.as_bytes()).ok())?;
            let max = policy.get("maxReceiveCount").and_then(|m| m.as_i64().or_else(|| m.to_string().parse().ok())).unwrap_or(10);
            let dlq = s_(&policy, "deadLetterTargetArn").rsplit(':').next().unwrap_or("").to_string();
            let now = crate::util::now_millis();
            let q = s.queues.get_mut(&name)?;
            let (dead, keep): (Vec<_>, Vec<_>) = q.messages.drain(..).partition(|m| m.visible_at <= now && m.receive_count >= max);
            q.messages = keep;
            if !dead.is_empty() {
                let target = s.queues.entry(dlq).or_default();
                for mut m in dead {
                    m.receive_count = 0;
                    m.receipt = None;
                    target.messages.push(m);
                }
            }
            None
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// SNS: subscriptions (SQS delivery), topic attributes, FIFO, PublishBatch
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub(super) struct SnsSubscription {
    arn: String,
    topic: String,
    protocol: String,
    endpoint: String,
    raw: bool,
}

#[derive(Default)]
pub(super) struct SnsExtra {
    subscriptions: Vec<SnsSubscription>,
    topic_attributes: BTreeMap<String, BTreeMap<String, String>>,
}

impl AwsMock {
    /// (topic ARN, protocol, endpoint) of the SNS subscriptions.
    pub fn sns_subscriptions(&self) -> Vec<(String, String, String)> {
        self.lock().gaps.sns.subscriptions.iter().map(|s| (s.topic.clone(), s.protocol.clone(), s.endpoint.clone())).collect()
    }
    /// Attributes of a topic.
    pub fn topic_attributes(&self, topic_arn: &str) -> BTreeMap<String, String> {
        self.lock().gaps.sns.topic_attributes.get(topic_arn).cloned().unwrap_or_default()
    }
}

fn sns_wrap(name: &str, inner: String) -> Response {
    Response::xml(
        200,
        format!("<{0}Response xmlns=\"http://sns.amazonaws.com/doc/2010-03-31/\"><{0}Result>{1}</{0}Result><ResponseMetadata><RequestId>{2}</RequestId></ResponseMetadata></{0}Response>", name, inner, request_id()),
    )
}

/// Deliver a published message to the topic's SQS subscriptions.
fn sns_fanout(s: &mut super::State, topic: &str, message_id: &str, message: &str, subject: Option<&str>, attributes: &[(String, String)]) {
    let subs: Vec<SnsSubscription> = s.gaps.sns.subscriptions.iter().filter(|x| x.topic == topic && x.protocol == "sqs").cloned().collect();
    for sub in subs {
        let queue = sub.endpoint.rsplit(':').next().unwrap_or("").to_string();
        let Some(q) = s.queues.get_mut(&queue) else { continue };
        let body = if sub.raw {
            message.to_string()
        } else {
            let mut m = ObjectMap::new();
            m.insert("Type".into(), Value::from("Notification"));
            m.insert("MessageId".into(), Value::from(message_id));
            m.insert("TopicArn".into(), Value::from(topic));
            if let Some(sj) = subject {
                m.insert("Subject".into(), Value::from(sj));
            }
            m.insert("Message".into(), Value::from(message));
            m.insert("Timestamp".into(), Value::from(crate::util::iso8601_millis(crate::util::now_millis())));
            m.insert("SignatureVersion".into(), Value::from("1"));
            m.insert("UnsubscribeURL".into(), Value::from(format!("https://sns.us-east-1.amazonaws.com/?Action=Unsubscribe&SubscriptionArn={}", sub.arn)));
            if !attributes.is_empty() {
                m.insert("MessageAttributes".into(), Value::Object(attributes.iter().map(|(k, v)| (k.clone(), obj([("Type", Value::from("String")), ("Value", Value::from(v.clone()))]))).collect()));
            }
            Value::Object(m).to_json()
        };
        let attrs: ObjectMap = if sub.raw { attributes.iter().map(|(k, v)| (k.clone(), obj([("DataType", Value::from("String")), ("StringValue", Value::from(v.clone()))]))).collect() } else { ObjectMap::new() };
        q.messages.push(super::new_msg(body, Value::Object(attrs), 0));
    }
}

fn sns_entries(p: &[(String, String)], prefix: &str) -> BTreeMap<String, String> {
    let get = |k: &str| p.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let mut out = BTreeMap::new();
    let mut n = 1;
    while let Some(k) = get(&format!("{}.entry.{}.key", prefix, n)) {
        out.insert(k, get(&format!("{}.entry.{}.value", prefix, n)).unwrap_or_default());
        n += 1;
    }
    out
}

fn sns_more(st: &Shared, req: &ServerRequest) -> Option<Response> {
    let p = crate::util::parse_form(&req.text());
    let get = |k: &str| p.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let action = get("Action").unwrap_or_default();
    let err = |status: u16, code: &str, msg: &str| super::query_error(status, code, msg);
    let mut s = st.0.lock().unwrap();
    let topic = get("TopicArn").unwrap_or_default();
    match action.as_str() {
        "CreateTopic" => {
            let name = get("Name").unwrap_or_default();
            let attrs = sns_entries(&p, "Attributes");
            let fifo = attrs.get("FifoTopic").map(|v| v == "true").unwrap_or(false);
            if fifo != name.ends_with(".fifo") {
                return Some(err(400, "InvalidParameter", "Invalid parameter: Fifo Topic names must end with .fifo and must be made up of only uppercase and lowercase ASCII letters, numbers, underscores, and hyphens"));
            }
            let arn = format!("arn:aws:sns:{}:{}:{}", s.region, ACCOUNT, name);
            s.topics.entry(arn.clone()).or_default();
            s.gaps.sns.topic_attributes.entry(arn.clone()).or_default().extend(attrs);
            Some(sns_wrap("CreateTopic", format!("<TopicArn>{}</TopicArn>", xml_escape(&arn))))
        }
        "SetTopicAttributes" | "GetTopicAttributes" => {
            if !s.topics.contains_key(&topic) {
                return Some(err(404, "NotFound", "Topic does not exist"));
            }
            let attrs = s.gaps.sns.topic_attributes.entry(topic.clone()).or_default();
            if action == "SetTopicAttributes" {
                attrs.insert(get("AttributeName").unwrap_or_default(), get("AttributeValue").unwrap_or_default());
                return Some(sns_wrap("SetTopicAttributes", String::new()));
            }
            let mut all = attrs.clone();
            all.insert("TopicArn".into(), topic.clone());
            let subs = s.gaps.sns.subscriptions.iter().filter(|x| x.topic == topic).count();
            all.insert("SubscriptionsConfirmed".into(), subs.to_string());
            let entries: String = all.iter().map(|(k, v)| format!("<entry><key>{}</key><value>{}</value></entry>", xml_escape(k), xml_escape(v))).collect();
            Some(sns_wrap("GetTopicAttributes", format!("<Attributes>{}</Attributes>", entries)))
        }
        "Subscribe" => {
            if !s.topics.contains_key(&topic) {
                return Some(err(404, "NotFound", "Topic does not exist"));
            }
            let (protocol, endpoint) = (get("Protocol").unwrap_or_default(), get("Endpoint").unwrap_or_default());
            let fifo_topic = topic.ends_with(".fifo");
            if protocol == "sqs" && fifo_topic && !endpoint.ends_with(".fifo") {
                return Some(err(400, "InvalidParameter", "Invalid parameter: Invalid parameter: Endpoint Reason: FIFO SNS Topics currently only support FIFO SQS Queues as endpoints"));
            }
            if let Some(existing) = s.gaps.sns.subscriptions.iter().find(|x| x.topic == topic && x.protocol == protocol && x.endpoint == endpoint) {
                return Some(sns_wrap("Subscribe", format!("<SubscriptionArn>{}</SubscriptionArn>", xml_escape(&existing.arn))));
            }
            let arn = format!("{}:{}", topic, crate::util::uuid());
            let raw = sns_entries(&p, "Attributes").get("RawMessageDelivery").is_some_and(|v| v == "true");
            s.gaps.sns.subscriptions.push(SnsSubscription { arn: arn.clone(), topic, protocol, endpoint, raw });
            Some(sns_wrap("Subscribe", format!("<SubscriptionArn>{}</SubscriptionArn>", xml_escape(&arn))))
        }
        "Unsubscribe" => {
            let arn = get("SubscriptionArn").unwrap_or_default();
            s.gaps.sns.subscriptions.retain(|x| x.arn != arn);
            Some(sns_wrap("Unsubscribe", String::new()))
        }
        "ListSubscriptionsByTopic" => {
            let members: String = s
                .gaps
                .sns
                .subscriptions
                .iter()
                .filter(|x| x.topic == topic)
                .map(|x| format!("<member><SubscriptionArn>{}</SubscriptionArn><Protocol>{}</Protocol><Endpoint>{}</Endpoint><TopicArn>{}</TopicArn></member>", xml_escape(&x.arn), x.protocol, xml_escape(&x.endpoint), xml_escape(&x.topic)))
                .collect();
            Some(sns_wrap("ListSubscriptionsByTopic", format!("<Subscriptions>{}</Subscriptions>", members)))
        }
        "Publish" => {
            let fifo = s.gaps.sns.topic_attributes.get(&topic).and_then(|a| a.get("FifoTopic")).is_some_and(|v| v == "true");
            let content_dedup = s.gaps.sns.topic_attributes.get(&topic).and_then(|a| a.get("ContentBasedDeduplication")).is_some_and(|v| v == "true");
            if fifo && get("MessageGroupId").is_none() {
                return Some(err(400, "InvalidParameter", "Invalid parameter: The MessageGroupId parameter is required for FIFO topics"));
            }
            if fifo && get("MessageDeduplicationId").is_none() && !content_dedup {
                return Some(err(400, "InvalidParameter", "Invalid parameter: The topic should either have ContentBasedDeduplication enabled or MessageDeduplicationId provided explicitly"));
            }
            if !fifo && !topic.ends_with(".fifo") && get("MessageGroupId").is_some() && s.topics.contains_key(&topic) {
                return Some(err(400, "InvalidParameter", "Invalid parameter: MessageGroupId Reason: The request includes MessageGroupId parameter that is not valid for this topic type"));
            }
            drop(s);
            let resp = super::sns(st, req);
            if resp.status == 200 {
                let id = Xml::parse(&resp.text()).ok().and_then(|x| x.find_text("MessageId")).unwrap_or_default();
                let mut attrs = Vec::new();
                let mut n = 1;
                while let Some(name) = get(&format!("MessageAttributes.entry.{}.Name", n)) {
                    attrs.push((name, get(&format!("MessageAttributes.entry.{}.Value.StringValue", n)).unwrap_or_default()));
                    n += 1;
                }
                let mut s = st.0.lock().unwrap();
                sns_fanout(&mut s, &topic, &id, &get("Message").unwrap_or_default(), get("Subject").as_deref(), &attrs);
                st.1.notify_all();
            }
            Some(resp)
        }
        "PublishBatch" => {
            if !s.topics.contains_key(&topic) {
                return Some(err(404, "NotFound", "Topic does not exist"));
            }
            let fifo = s.gaps.sns.topic_attributes.get(&topic).and_then(|a| a.get("FifoTopic")).is_some_and(|v| v == "true");
            let (mut okx, mut failed) = (String::new(), String::new());
            let mut n = 1;
            while let Some(id) = get(&format!("PublishBatchRequestEntries.member.{}.Id", n)) {
                let pre = format!("PublishBatchRequestEntries.member.{}.", n);
                let msg = get(&format!("{}Message", pre)).unwrap_or_default();
                if fifo && get(&format!("{}MessageGroupId", pre)).is_none() {
                    failed.push_str(&format!("<member><Id>{}</Id><Code>InvalidParameter</Code><Message>The MessageGroupId parameter is required for FIFO topics</Message><SenderFault>true</SenderFault></member>", xml_escape(&id)));
                } else {
                    let mid = crate::util::uuid();
                    let subject = get(&format!("{}Subject", pre));
                    s.topics.entry(topic.clone()).or_default().push(super::PublishedMessage {
                        message_id: mid.clone(),
                        message: msg.clone(),
                        subject: subject.clone(),
                        message_structure: None,
                        attributes: BTreeMap::new(),
                        group_id: get(&format!("{}MessageGroupId", pre)),
                    });
                    sns_fanout(&mut s, &topic, &mid, &msg, subject.as_deref(), &[]);
                    okx.push_str(&format!("<member><Id>{}</Id><MessageId>{}</MessageId></member>", xml_escape(&id), mid));
                }
                n += 1;
            }
            st.1.notify_all();
            Some(sns_wrap("PublishBatch", format!("<Successful>{}</Successful><Failed>{}</Failed>", okx, failed)))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// IAM: access key status, roles, managed policies, attachments, instance profiles
// ---------------------------------------------------------------------------

#[derive(Default)]
pub(super) struct IamExtra {
    key_status: BTreeMap<String, String>,
    /// name -> (id, path, created, assume role document, description)
    roles: BTreeMap<String, (String, String, i64, String, String)>,
    /// arn -> (name, id, path, document)
    policies: BTreeMap<String, (String, String, String, String)>,
    /// (kind user/group/role, principal name) -> policy arns
    attached: BTreeMap<(String, String), Vec<String>>,
    /// name -> (id, path, roles)
    profiles: BTreeMap<String, (String, String, Vec<String>)>,
}

impl AwsMock {
    /// Names of the emulated IAM roles.
    pub fn iam_roles(&self) -> Vec<String> {
        self.lock().gaps.iam.roles.keys().cloned().collect()
    }
}

fn iam_more(st: &Shared, req: &ServerRequest) -> Option<Response> {
    const NS: &str = "https://iam.amazonaws.com/doc/2010-05-08/";
    let p = crate::util::parse_form(&req.text());
    let get = |k: &str| p.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap_or_default();
    let action = get("Action");
    let ok = |inner: String| Response::xml(200, format!("<{0}Response xmlns=\"{1}\"><{0}Result>{2}</{0}Result><ResponseMetadata><RequestId>{3}</RequestId></ResponseMetadata></{0}Response>", action, NS, inner, request_id()));
    let err = |status: u16, code: &str, msg: &str| Response::xml(status, format!("<ErrorResponse xmlns=\"{}\"><Error><Type>Sender</Type><Code>{}</Code><Message>{}</Message></Error><RequestId>{}</RequestId></ErrorResponse>", NS, code, xml_escape(msg), request_id()));
    let mut s = st.0.lock().unwrap();
    let iso = |t: i64| crate::util::iso8601(t);
    let role_xml = |n: &str, r: &(String, String, i64, String, String)| {
        format!(
            "<RoleName>{}</RoleName><RoleId>{}</RoleId><Arn>arn:aws:iam::{}:role{}{}</Arn><Path>{}</Path><CreateDate>{}</CreateDate><AssumeRolePolicyDocument>{}</AssumeRolePolicyDocument><Description>{}</Description>",
            xml_escape(n),
            r.0,
            ACCOUNT,
            xml_escape(&r.1),
            xml_escape(n),
            xml_escape(&r.1),
            iso(r.2),
            xml_escape(&crate::util::uri_encode(&r.3, false)),
            xml_escape(&r.4)
        )
    };
    let attach_count = |s: &super::State, arn: &str| s.gaps.iam.attached.values().filter(|v| v.iter().any(|a| a == arn)).count();
    let policy_xml = |s: &super::State, arn: &str, p: &(String, String, String, String)| {
        format!("<PolicyName>{}</PolicyName><PolicyId>{}</PolicyId><Arn>{}</Arn><Path>{}</Path><DefaultVersionId>v1</DefaultVersionId><AttachmentCount>{}</AttachmentCount>", xml_escape(&p.0), p.1, xml_escape(arn), xml_escape(&p.2), attach_count(s, arn))
    };
    let path = Some(get("Path")).filter(|p| !p.is_empty()).unwrap_or_else(|| "/".into());
    match action.as_str() {
        "UpdateAccessKey" => {
            let (ak, status) = (get("AccessKeyId"), get("Status"));
            if !matches!(status.as_str(), "Active" | "Inactive") {
                return Some(err(400, "ValidationError", "Status must be Active or Inactive"));
            }
            if !s.ext.access_keys.iter().any(|(_, k, _)| *k == ak) {
                return Some(err(404, "NoSuchEntity", &format!("The Access Key with id {} cannot be found.", ak)));
            }
            s.gaps.iam.key_status.insert(ak.clone(), status.clone());
            // inactive keys cannot sign requests any more
            let secret = s.ext.access_keys.iter().find(|(_, k, _)| *k == ak).map(|(_, _, sk)| sk.clone()).unwrap_or_default();
            if status == "Inactive" {
                s.credentials.remove(&ak);
            } else {
                s.credentials.insert(ak, secret);
            }
            Some(ok(String::new()))
        }
        "ListAccessKeys" => {
            let user = get("UserName");
            let members: String = s
                .ext
                .access_keys
                .iter()
                .filter(|(u, _, _)| user.is_empty() || *u == user)
                .map(|(u, ak, _)| format!("<member><UserName>{}</UserName><AccessKeyId>{}</AccessKeyId><Status>{}</Status></member>", xml_escape(u), ak, s.gaps.iam.key_status.get(ak).map(|x| x.as_str()).unwrap_or("Active")))
                .collect();
            Some(ok(format!("<AccessKeyMetadata>{}</AccessKeyMetadata><IsTruncated>false</IsTruncated>", members)))
        }
        "GetGroup" => {
            let g = get("GroupName");
            let Some(grp) = s.ext.groups.get(&g) else { return Some(err(404, "NoSuchEntity", &format!("The group with name {} cannot be found.", g))) };
            let users: String = grp
                .2
                .iter()
                .filter_map(|u| s.ext.users.get(u).map(|x| (u, x)))
                .map(|(n, u)| format!("<member><UserName>{}</UserName><UserId>{}</UserId><Arn>{}</Arn><Path>/</Path><CreateDate>{}</CreateDate></member>", xml_escape(n), u.id, xml_escape(&u.arn), iso(u.created)))
                .collect();
            Some(ok(format!(
                "<Group><GroupName>{}</GroupName><GroupId>{}</GroupId><Arn>arn:aws:iam::{}:group/{}</Arn><Path>/</Path><CreateDate>{}</CreateDate></Group><Users>{}</Users><IsTruncated>false</IsTruncated>",
                xml_escape(&g),
                grp.0,
                ACCOUNT,
                xml_escape(&g),
                iso(grp.1),
                users
            )))
        }
        "CreateRole" => {
            let n = get("RoleName");
            if s.gaps.iam.roles.contains_key(&n) {
                return Some(err(409, "EntityAlreadyExists", &format!("Role with name {} already exists.", n)));
            }
            let doc = get("AssumeRolePolicyDocument");
            if parse_json(doc.as_bytes()).is_err() {
                return Some(err(400, "MalformedPolicyDocument", "This policy contains invalid Json"));
            }
            let r = (format!("AROA{}", crate::util::random_hex(8).to_uppercase()), path, now_secs(), doc, get("Description"));
            let x = role_xml(&n, &r);
            s.gaps.iam.roles.insert(n, r);
            Some(ok(format!("<Role>{}</Role>", x)))
        }
        "GetRole" => {
            let n = get("RoleName");
            match s.gaps.iam.roles.get(&n) {
                Some(r) => Some(ok(format!("<Role>{}</Role>", role_xml(&n, r)))),
                None => Some(err(404, "NoSuchEntity", &format!("The role with name {} cannot be found.", n))),
            }
        }
        "DeleteRole" => {
            let n = get("RoleName");
            if s.gaps.iam.attached.get(&("role".to_string(), n.clone())).is_some_and(|v| !v.is_empty()) {
                return Some(err(409, "DeleteConflict", "Cannot delete entity, must detach all policies first."));
            }
            if s.gaps.iam.profiles.values().any(|p| p.2.contains(&n)) {
                return Some(err(409, "DeleteConflict", "Cannot delete entity, must remove roles from instance profile first."));
            }
            match s.gaps.iam.roles.remove(&n) {
                Some(_) => Some(ok(String::new())),
                None => Some(err(404, "NoSuchEntity", &format!("The role with name {} cannot be found.", n))),
            }
        }
        "ListRoles" => {
            let members: String = s.gaps.iam.roles.iter().map(|(n, r)| format!("<member>{}</member>", role_xml(n, r))).collect();
            Some(ok(format!("<Roles>{}</Roles><IsTruncated>false</IsTruncated>", members)))
        }
        "CreatePolicy" => {
            let n = get("PolicyName");
            let doc = get("PolicyDocument");
            if parse_json(doc.as_bytes()).ok().and_then(|d| d.get("Statement").cloned()).is_none() {
                return Some(err(400, "MalformedPolicyDocument", "Syntax errors in policy."));
            }
            let arn = format!("arn:aws:iam::{}:policy{}{}", ACCOUNT, path, n);
            if s.gaps.iam.policies.contains_key(&arn) {
                return Some(err(409, "EntityAlreadyExists", &format!("A policy called {} already exists. Duplicate names are not allowed.", n)));
            }
            let pol = (n, format!("ANPA{}", crate::util::random_hex(8).to_uppercase()), path, doc);
            let x = policy_xml(&s, &arn, &pol);
            s.gaps.iam.policies.insert(arn, pol);
            Some(ok(format!("<Policy>{}</Policy>", x)))
        }
        "GetPolicy" => {
            let arn = get("PolicyArn");
            match s.gaps.iam.policies.get(&arn) {
                Some(pol) => Some(ok(format!("<Policy>{}</Policy>", policy_xml(&s, &arn, pol)))),
                None => Some(err(404, "NoSuchEntity", &format!("Policy {} does not exist or is not attachable.", arn))),
            }
        }
        "DeletePolicy" => {
            let arn = get("PolicyArn");
            if attach_count(&s, &arn) > 0 {
                return Some(err(409, "DeleteConflict", "Cannot delete a policy attached to entities."));
            }
            match s.gaps.iam.policies.remove(&arn) {
                Some(_) => Some(ok(String::new())),
                None => Some(err(404, "NoSuchEntity", &format!("Policy {} does not exist or is not attachable.", arn))),
            }
        }
        "ListPolicies" => {
            let members: String = s.gaps.iam.policies.iter().map(|(arn, pol)| format!("<member>{}</member>", policy_xml(&s, arn, pol))).collect();
            Some(ok(format!("<Policies>{}</Policies><IsTruncated>false</IsTruncated>", members)))
        }
        a if a.starts_with("Attach") || a.starts_with("Detach") || a.starts_with("ListAttached") => {
            let (kind, principal) = if a.contains("User") {
                ("user", get("UserName"))
            } else if a.contains("Group") {
                ("group", get("GroupName"))
            } else {
                ("role", get("RoleName"))
            };
            let exists = match kind {
                "user" => s.ext.users.contains_key(&principal),
                "group" => s.ext.groups.contains_key(&principal),
                _ => s.gaps.iam.roles.contains_key(&principal),
            };
            if !exists {
                return Some(err(404, "NoSuchEntity", &format!("The {} with name {} cannot be found.", kind, principal)));
            }
            let key = (kind.to_string(), principal);
            if a.starts_with("ListAttached") {
                let members: String = s
                    .gaps
                    .iam
                    .attached
                    .get(&key)
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|arn| format!("<member><PolicyName>{}</PolicyName><PolicyArn>{}</PolicyArn></member>", xml_escape(arn.rsplit('/').next().unwrap_or("")), xml_escape(arn)))
                    .collect();
                return Some(ok(format!("<AttachedPolicies>{}</AttachedPolicies><IsTruncated>false</IsTruncated>", members)));
            }
            let arn = get("PolicyArn");
            let aws_managed = arn.starts_with("arn:aws:iam::aws:policy/");
            if !aws_managed && !s.gaps.iam.policies.contains_key(&arn) {
                return Some(err(404, "NoSuchEntity", &format!("Policy {} does not exist or is not attachable.", arn)));
            }
            let list = s.gaps.iam.attached.entry(key).or_default();
            if a.starts_with("Attach") {
                if !list.contains(&arn) {
                    list.push(arn);
                }
            } else {
                if !list.contains(&arn) {
                    return Some(err(404, "NoSuchEntity", &format!("Policy {} was not found.", arn)));
                }
                list.retain(|x| *x != arn);
            }
            Some(ok(String::new()))
        }
        "CreateInstanceProfile" | "GetInstanceProfile" | "DeleteInstanceProfile" | "AddRoleToInstanceProfile" | "RemoveRoleFromInstanceProfile" | "ListInstanceProfiles" => {
            let n = get("InstanceProfileName");
            let xml = |n: &str, p: &(String, String, Vec<String>)| {
                let roles: String = p.2.iter().map(|r| format!("<member><RoleName>{}</RoleName></member>", xml_escape(r))).collect();
                format!("<InstanceProfileName>{}</InstanceProfileName><InstanceProfileId>{}</InstanceProfileId><Arn>arn:aws:iam::{}:instance-profile{}{}</Arn><Path>{}</Path><Roles>{}</Roles>", xml_escape(n), p.0, ACCOUNT, xml_escape(&p.1), xml_escape(n), xml_escape(&p.1), roles)
            };
            let missing = |n: &str| err(404, "NoSuchEntity", &format!("Instance Profile {} cannot be found.", n));
            match action.as_str() {
                "CreateInstanceProfile" => {
                    if s.gaps.iam.profiles.contains_key(&n) {
                        return Some(err(409, "EntityAlreadyExists", &format!("Instance Profile {} already exists.", n)));
                    }
                    let pr = (format!("AIPA{}", crate::util::random_hex(8).to_uppercase()), path, Vec::new());
                    let x = xml(&n, &pr);
                    s.gaps.iam.profiles.insert(n, pr);
                    Some(ok(format!("<InstanceProfile>{}</InstanceProfile>", x)))
                }
                "GetInstanceProfile" => match s.gaps.iam.profiles.get(&n) {
                    Some(pr) => Some(ok(format!("<InstanceProfile>{}</InstanceProfile>", xml(&n, pr)))),
                    None => Some(missing(&n)),
                },
                "DeleteInstanceProfile" => match s.gaps.iam.profiles.remove(&n) {
                    Some(_) => Some(ok(String::new())),
                    None => Some(missing(&n)),
                },
                "ListInstanceProfiles" => {
                    let members: String = s.gaps.iam.profiles.iter().map(|(n, pr)| format!("<member>{}</member>", xml(n, pr))).collect();
                    Some(ok(format!("<InstanceProfiles>{}</InstanceProfiles><IsTruncated>false</IsTruncated>", members)))
                }
                _ => {
                    let role = get("RoleName");
                    if !s.gaps.iam.roles.contains_key(&role) {
                        return Some(err(404, "NoSuchEntity", &format!("The role with name {} cannot be found.", role)));
                    }
                    let Some(pr) = s.gaps.iam.profiles.get_mut(&n) else { return Some(missing(&n)) };
                    if action == "AddRoleToInstanceProfile" {
                        if !pr.2.is_empty() {
                            return Some(err(409, "LimitExceeded", "Cannot exceed quota for InstanceSessionsPerInstanceProfile: 1"));
                        }
                        pr.2.push(role);
                    } else {
                        pr.2.retain(|r| *r != role);
                    }
                    Some(ok(String::new()))
                }
            }
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Step Functions: activities (Task states with activity resources), UpdateStateMachine
// ---------------------------------------------------------------------------

struct ActivityTask {
    token: String,
    activity: String,
    execution: String,
    state: String,
    input: String,
    taken: bool,
}

#[derive(Default)]
pub(super) struct SfnExtra {
    /// arn -> (name, created millis)
    activities: BTreeMap<String, (String, i64)>,
    tasks: Vec<ActivityTask>,
}

enum Outcome {
    Done(String, Option<String>, Option<(String, String)>),
    Waiting(String, String, String),
}

/// Interpret a definition from `start` (Pass / Succeed / Fail / Task with an activity
/// resource, which suspends the execution).
fn interpret(def: &Value, start: &str, input: &str) -> Outcome {
    let mut data = parse_json(input.as_bytes()).unwrap_or_default();
    let mut current = start.to_string();
    for _ in 0..100 {
        let Some(state) = def.get("States").and_then(|s| s.get(&current)) else { return Outcome::Done("FAILED".into(), None, Some(("States.Runtime".into(), format!("No state {}", current)))) };
        let next_or_end = |state: &Value, data: &Value| -> Option<Outcome> {
            if state.get("End").is_some_and(|e| e.is_truthy()) {
                Some(Outcome::Done("SUCCEEDED".into(), Some(data.to_json()), None))
            } else {
                None
            }
        };
        match s_(state, "Type").as_str() {
            "Pass" => {
                if let Some(r) = state.get("Result") {
                    data = r.clone();
                }
                if let Some(o) = next_or_end(state, &data) {
                    return o;
                }
                current = s_(state, "Next");
            }
            "Task" if s_(state, "Resource").contains(":activity:") => return Outcome::Waiting(s_(state, "Resource"), current, data.to_json()),
            "Succeed" => return Outcome::Done("SUCCEEDED".into(), Some(data.to_json()), None),
            "Fail" => return Outcome::Done("FAILED".into(), None, Some((s_(state, "Error"), s_(state, "Cause")))),
            other => return Outcome::Done("FAILED".into(), None, Some(("States.Runtime".into(), format!("Unsupported state type {}", other)))),
        }
    }
    Outcome::Done("FAILED".into(), None, Some(("States.Runtime".into(), "Too many transitions".into())))
}

fn has_activity(def: &str) -> bool {
    parse_json(def.as_bytes()).ok().and_then(|d| d.get("States").and_then(|s| s.as_object().cloned())).is_some_and(|m| m.values().any(|st| s_(st, "Type") == "Task" && s_(st, "Resource").contains(":activity:")))
}

/// Apply an interpretation outcome to an execution.
fn settle(s: &mut super::State, exec_arn: &str, outcome: Outcome) {
    match outcome {
        Outcome::Done(status, output, error) => {
            if let Some(e) = s.ext.executions.get_mut(exec_arn) {
                e.status = status;
                e.output = output;
                e.error = error;
                e.stop = Some(crate::util::now_millis());
            }
        }
        Outcome::Waiting(activity, state, input) => {
            s.gaps.sfn.tasks.push(ActivityTask { token: base64_encode(format!("{}|{}", exec_arn, crate::util::random_hex(8)).as_bytes()), activity, execution: exec_arn.to_string(), state, input, taken: false });
        }
    }
}

fn states_more(st: &Shared, req: &ServerRequest) -> Option<Response> {
    let b = body(req);
    let err = |code: &str, msg: &str| super::json_error(400, "com.amazonaws.swf.service.model", code, msg).with_header("Content-Type", "application/x-amz-json-1.0");
    let ok = |v: Value| Response::json(200, v.to_json()).with_header("Content-Type", "application/x-amz-json-1.0");
    let action = target(req);
    if action == "GetActivityTask" {
        // long poll (up to 5 s here; 60 s on AWS) for a task of the activity
        let activity = s_(&b, "activityArn");
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut s = st.0.lock().unwrap();
        loop {
            if !s.gaps.sfn.activities.contains_key(&activity) {
                return Some(err("ActivityDoesNotExist", &format!("Activity Does Not Exist: '{}'", activity)));
            }
            if let Some(t) = s.gaps.sfn.tasks.iter_mut().find(|t| t.activity == activity && !t.taken) {
                t.taken = true;
                return Some(ok(obj([("taskToken", Value::from(t.token.clone())), ("input", Value::from(t.input.clone()))])));
            }
            if Instant::now() >= deadline {
                return Some(ok(obj([])));
            }
            s = st.1.wait_timeout(s, Duration::from_millis(20)).unwrap().0;
        }
    }
    let mut s = st.0.lock().unwrap();
    let region = s.region.clone();
    match action.as_str() {
        "CreateActivity" => {
            let name = s_(&b, "name");
            let arn = format!("arn:aws:states:{}:{}:activity:{}", region, ACCOUNT, name);
            let created = s.gaps.sfn.activities.entry(arn.clone()).or_insert((name, crate::util::now_millis())).1;
            Some(ok(obj([("activityArn", Value::from(arn)), ("creationDate", Value::Float(created as f64 / 1000.0))])))
        }
        "DeleteActivity" => {
            s.gaps.sfn.activities.remove(&s_(&b, "activityArn"));
            Some(ok(obj([])))
        }
        "DescribeActivity" => {
            let arn = s_(&b, "activityArn");
            match s.gaps.sfn.activities.get(&arn) {
                Some((name, created)) => Some(ok(obj([("activityArn", Value::from(arn)), ("name", Value::from(name.clone())), ("creationDate", Value::Float(*created as f64 / 1000.0))]))),
                None => Some(err("ActivityDoesNotExist", &format!("Activity Does Not Exist: '{}'", arn))),
            }
        }
        "ListActivities" => {
            let list: Vec<Value> = s.gaps.sfn.activities.iter().map(|(arn, (n, c))| obj([("activityArn", Value::from(arn.clone())), ("name", Value::from(n.clone())), ("creationDate", Value::Float(*c as f64 / 1000.0))])).collect();
            Some(ok(obj([("activities", Value::Array(list))])))
        }
        "UpdateStateMachine" => {
            let arn = s_(&b, "stateMachineArn");
            let Some(m) = s.ext.machines.get_mut(&arn) else { return Some(err("StateMachineDoesNotExist", &format!("State Machine Does Not Exist: '{}'", arn))) };
            let def = s_(&b, "definition");
            let role = s_(&b, "roleArn");
            if def.is_empty() && role.is_empty() {
                return Some(err("MissingRequiredParameter", "Either the definition or the roleArn must be specified"));
            }
            if !def.is_empty() {
                if parse_json(def.as_bytes()).ok().and_then(|d| d.get("StartAt").cloned()).is_none() {
                    return Some(err("InvalidDefinition", "Invalid State Machine Definition: 'MISSING_START_AT'"));
                }
                m.definition = def;
            }
            if !role.is_empty() {
                m.role = role;
            }
            Some(ok(obj([("updateDate", Value::Float(crate::util::now_millis() as f64 / 1000.0))])))
        }
        "StartExecution" => {
            let machine = s_(&b, "stateMachineArn");
            let m = s.ext.machines.get(&machine)?;
            if !has_activity(&m.definition) {
                return None;
            }
            let def = parse_json(m.definition.as_bytes()).unwrap_or_default();
            let name = Some(s_(&b, "name")).filter(|n| !n.is_empty()).unwrap_or_else(crate::util::uuid);
            let input = Some(s_(&b, "input")).filter(|i| !i.is_empty()).unwrap_or_else(|| "{}".into());
            let arn = format!("arn:aws:states:{}:{}:execution:{}:{}", region, ACCOUNT, m.name, name);
            if s.ext.executions.contains_key(&arn) {
                return Some(err("ExecutionAlreadyExists", &format!("Execution Already Exists: '{}'", arn)));
            }
            let now = crate::util::now_millis();
            s.ext.executions.insert(arn.clone(), super::ext::Execution { machine, name, status: "RUNNING".into(), input: input.clone(), output: None, error: None, start: now, stop: None });
            let outcome = interpret(&def, &s_(&def, "StartAt"), &input);
            settle(&mut s, &arn, outcome);
            st.1.notify_all();
            Some(ok(obj([("executionArn", Value::from(arn)), ("startDate", Value::Float(now as f64 / 1000.0))])))
        }
        "SendTaskSuccess" | "SendTaskFailure" | "SendTaskHeartbeat" => {
            let token = s_(&b, "taskToken");
            let Some(idx) = s.gaps.sfn.tasks.iter().position(|t| t.token == token) else { return Some(err("TaskDoesNotExist", "Task Does Not Exist")) };
            if action == "SendTaskHeartbeat" {
                return Some(ok(obj([])));
            }
            let t = s.gaps.sfn.tasks.remove(idx);
            let running = s.ext.executions.get(&t.execution).is_some_and(|e| e.status == "RUNNING");
            if !running {
                return Some(err("TaskTimedOut", "Task Timed Out"));
            }
            if action == "SendTaskFailure" {
                settle(&mut s, &t.execution, Outcome::Done("FAILED".into(), None, Some((s_(&b, "error"), s_(&b, "cause")))));
                return Some(ok(obj([])));
            }
            let output = s_(&b, "output");
            if parse_json(output.as_bytes()).is_err() {
                return Some(err("InvalidOutput", "Invalid output: not JSON"));
            }
            let machine = s.ext.executions.get(&t.execution).map(|e| e.machine.clone()).unwrap_or_default();
            let def = s.ext.machines.get(&machine).and_then(|m| parse_json(m.definition.as_bytes()).ok()).unwrap_or_default();
            let state = def.get("States").and_then(|x| x.get(&t.state)).cloned().unwrap_or_default();
            let outcome = if state.get("End").is_some_and(|e| e.is_truthy()) { Outcome::Done("SUCCEEDED".into(), Some(output), None) } else { interpret(&def, &s_(&state, "Next"), &output) };
            settle(&mut s, &t.execution, outcome);
            st.1.notify_all();
            Some(ok(obj([])))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Rekognition: face collections, face search / comparison, stored video jobs
// ---------------------------------------------------------------------------

/// (face id, identity, external image id)
type StoredFace = (String, String, String);

#[derive(Default)]
pub(super) struct RekExtra {
    collections: BTreeMap<String, (i64, Vec<StoredFace>)>,
    /// job id -> (kind, video bytes, collection)
    jobs: BTreeMap<String, (String, Vec<u8>, String)>,
}

/// The faces of an emulated image: one per `face:NAME` token (NAME = the person).
fn faces_of(data: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(data).split_whitespace().filter_map(|w| w.strip_prefix("face:")).map(|n| n.trim_matches(|c: char| !c.is_alphanumeric()).to_ascii_lowercase()).filter(|n| !n.is_empty()).collect()
}

fn image_bytes(st: &Shared, img: &Value) -> Result<Vec<u8>, Response> {
    if let Some(b) = img.get("Bytes") {
        return base64_decode(&b.to_string()).ok_or_else(|| rek_err("InvalidImageFormatException", "Invalid image encoding"));
    }
    let s3 = img.get("S3Object").cloned().unwrap_or_default();
    let (bucket, name) = (s_(&s3, "Bucket"), s_(&s3, "Name"));
    st.0.lock().unwrap().buckets.get(&bucket).and_then(|b| b.objects.get(&name)).map(|o| o.data.clone()).ok_or_else(|| rek_err("InvalidS3ObjectException", &format!("Unable to get object metadata from S3: s3://{}/{}", bucket, name)))
}

fn rek_err(code: &str, msg: &str) -> Response {
    super::json_error(400, "com.amazonaws.rekognition.model", code, msg)
}

fn bbox(i: usize) -> Value {
    obj([("Width", Value::Float(0.2)), ("Height", Value::Float(0.3)), ("Left", Value::Float(0.1 + 0.25 * i as f64)), ("Top", Value::Float(0.1))])
}

fn face_value(f: &StoredFace, i: usize) -> Value {
    obj([("FaceId", Value::from(f.0.clone())), ("ExternalImageId", Value::from(f.2.clone())), ("ImageId", Value::from(crate::util::md5_hex(f.1.as_bytes()))), ("Confidence", Value::Float(99.9)), ("BoundingBox", bbox(i))])
}

fn rekognition_more(st: &Shared, req: &ServerRequest) -> Option<Response> {
    let b = body(req);
    let action = target(req);
    let coll = s_(&b, "CollectionId");
    let threshold = b.get("FaceMatchThreshold").or_else(|| b.get("SimilarityThreshold")).and_then(|t| t.as_f64()).unwrap_or(80.0);
    let max = b.get("MaxFaces").and_then(|m| m.as_i64()).unwrap_or(100).max(1) as usize;
    let missing = |c: &str| super::json_error(400, "com.amazonaws.rekognition.model", "ResourceNotFoundException", &format!("The collection id: {} does not exist", c));
    match action.as_str() {
        "CreateCollection" => {
            let mut s = st.0.lock().unwrap();
            if s.gaps.rek.collections.contains_key(&coll) {
                return Some(super::json_error(400, "com.amazonaws.rekognition.model", "ResourceAlreadyExistsException", &format!("The collection id: {} already exists", coll)));
            }
            s.gaps.rek.collections.insert(coll.clone(), (crate::util::now_millis(), Vec::new()));
            Some(ok(obj([("StatusCode", Value::Int(200)), ("CollectionArn", Value::from(format!("aws:rekognition:us-east-1:{}:collection/{}", ACCOUNT, coll))), ("FaceModelVersion", Value::from("7.0"))])))
        }
        "DeleteCollection" => {
            let mut s = st.0.lock().unwrap();
            match s.gaps.rek.collections.remove(&coll) {
                Some(_) => Some(ok(obj([("StatusCode", Value::Int(200))]))),
                None => Some(missing(&coll)),
            }
        }
        "DescribeCollection" => {
            let s = st.0.lock().unwrap();
            let Some((created, faces)) = s.gaps.rek.collections.get(&coll) else { return Some(missing(&coll)) };
            Some(ok(obj([
                ("FaceCount", Value::Int(faces.len() as i64)),
                ("FaceModelVersion", Value::from("7.0")),
                ("CollectionARN", Value::from(format!("aws:rekognition:us-east-1:{}:collection/{}", ACCOUNT, coll))),
                ("CreationTimestamp", Value::Float(*created as f64 / 1000.0)),
            ])))
        }
        "ListCollections" => {
            let s = st.0.lock().unwrap();
            let ids: Vec<Value> = s.gaps.rek.collections.keys().map(|k| Value::from(k.clone())).collect();
            Some(ok(obj([("CollectionIds", Value::Array(ids))])))
        }
        "IndexFaces" => {
            let data = match image_bytes(st, &b.get("Image").cloned().unwrap_or_default()) {
                Ok(d) => d,
                Err(r) => return Some(r),
            };
            let mut s = st.0.lock().unwrap();
            let Some((_, faces)) = s.gaps.rek.collections.get_mut(&coll) else { return Some(missing(&coll)) };
            let ext = s_(&b, "ExternalImageId");
            let mut records = Vec::new();
            for (i, who) in faces_of(&data).into_iter().take(max).enumerate() {
                let f = (crate::util::uuid(), who, ext.clone());
                records.push(obj([("Face", face_value(&f, i)), ("FaceDetail", obj([("BoundingBox", bbox(i)), ("Confidence", Value::Float(99.9))]))]));
                faces.push(f);
            }
            Some(ok(obj([("FaceRecords", Value::Array(records)), ("FaceModelVersion", Value::from("7.0")), ("UnindexedFaces", Value::Array(vec![]))])))
        }
        "ListFaces" => {
            let s = st.0.lock().unwrap();
            let Some((_, faces)) = s.gaps.rek.collections.get(&coll) else { return Some(missing(&coll)) };
            Some(ok(obj([("Faces", Value::Array(faces.iter().enumerate().map(|(i, f)| face_value(f, i)).collect())), ("FaceModelVersion", Value::from("7.0"))])))
        }
        "DeleteFaces" => {
            let ids: Vec<String> = b.get("FaceIds").and_then(|a| a.as_array()).map(|a| a.iter().map(|x| x.to_string()).collect()).unwrap_or_default();
            let mut s = st.0.lock().unwrap();
            let Some((_, faces)) = s.gaps.rek.collections.get_mut(&coll) else { return Some(missing(&coll)) };
            let deleted: Vec<Value> = faces.iter().filter(|f| ids.contains(&f.0)).map(|f| Value::from(f.0.clone())).collect();
            faces.retain(|f| !ids.contains(&f.0));
            Some(ok(obj([("DeletedFaces", Value::Array(deleted))])))
        }
        "SearchFaces" | "SearchFacesByImage" => {
            let who = if action == "SearchFaces" {
                let id = s_(&b, "FaceId");
                let s = st.0.lock().unwrap();
                let Some((_, faces)) = s.gaps.rek.collections.get(&coll) else { return Some(missing(&coll)) };
                match faces.iter().find(|f| f.0 == id) {
                    Some(f) => f.1.clone(),
                    None => return Some(rek_err("InvalidParameterException", &format!("Face {} not found in the collection", id))),
                }
            } else {
                let data = match image_bytes(st, &b.get("Image").cloned().unwrap_or_default()) {
                    Ok(d) => d,
                    Err(r) => return Some(r),
                };
                match faces_of(&data).into_iter().next() {
                    Some(w) => w,
                    None => return Some(rek_err("InvalidParameterException", "There are no faces in the image. Should be at least 1.")),
                }
            };
            let search_id = s_(&b, "FaceId");
            let s = st.0.lock().unwrap();
            let Some((_, faces)) = s.gaps.rek.collections.get(&coll) else { return Some(missing(&coll)) };
            let matches: Vec<Value> = faces
                .iter()
                .filter(|f| f.1 == who && f.0 != search_id && 99.5 >= threshold)
                .take(max)
                .enumerate()
                .map(|(i, f)| obj([("Similarity", Value::Float(99.5)), ("Face", face_value(f, i))]))
                .collect();
            let mut m = ObjectMap::new();
            m.insert("FaceMatches".into(), Value::Array(matches));
            m.insert("FaceModelVersion".into(), Value::from("7.0"));
            if action == "SearchFaces" {
                m.insert("SearchedFaceId".into(), Value::from(search_id));
            } else {
                m.insert("SearchedFaceBoundingBox".into(), bbox(0));
                m.insert("SearchedFaceConfidence".into(), Value::Float(99.9));
            }
            Some(ok(Value::Object(m)))
        }
        "CompareFaces" => {
            let (src, tgt) = match (image_bytes(st, &b.get("SourceImage").cloned().unwrap_or_default()), image_bytes(st, &b.get("TargetImage").cloned().unwrap_or_default())) {
                (Ok(a), Ok(t)) => (a, t),
                (Err(r), _) | (_, Err(r)) => return Some(r),
            };
            let Some(who) = faces_of(&src).into_iter().next() else { return Some(rek_err("InvalidParameterException", "There are no faces in the source image.")) };
            let (mut matched, mut unmatched) = (Vec::new(), Vec::new());
            for (i, t) in faces_of(&tgt).into_iter().enumerate() {
                let face = obj([("BoundingBox", bbox(i)), ("Confidence", Value::Float(99.9))]);
                if t == who && 99.0 >= threshold {
                    matched.push(obj([("Similarity", Value::Float(99.0)), ("Face", face)]));
                } else {
                    unmatched.push(face);
                }
            }
            Some(ok(obj([("SourceImageFace", obj([("BoundingBox", bbox(0)), ("Confidence", Value::Float(99.9))])), ("FaceMatches", Value::Array(matched)), ("UnmatchedFaces", Value::Array(unmatched))])))
        }
        a if a.starts_with("Start") && a.ends_with("Detection") || a == "StartFaceSearch" => {
            let data = match image_bytes(st, &b.get("Video").cloned().unwrap_or_default()) {
                Ok(d) => d,
                Err(r) => return Some(r),
            };
            let mut s = st.0.lock().unwrap();
            if a == "StartFaceSearch" && !s.gaps.rek.collections.contains_key(&coll) {
                return Some(missing(&coll));
            }
            let job = crate::util::random_hex(16);
            s.gaps.rek.jobs.insert(job.clone(), (a.trim_start_matches("Start").to_string(), data, coll));
            Some(ok(obj([("JobId", Value::from(job))])))
        }
        a if a.starts_with("Get") && (a.ends_with("Detection") || a == "GetFaceSearch") => {
            let job = s_(&b, "JobId");
            let s = st.0.lock().unwrap();
            let Some((kind, data, coll)) = s.gaps.rek.jobs.get(&job) else { return Some(super::json_error(400, "com.amazonaws.rekognition.model", "ResourceNotFoundException", "The job id does not exist")) };
            if *kind != a.trim_start_matches("Get") {
                return Some(rek_err("InvalidParameterException", "The job is of another type"));
            }
            let text = String::from_utf8_lossy(data).to_string();
            let mut m = ObjectMap::new();
            m.insert("JobStatus".into(), Value::from("SUCCEEDED"));
            m.insert("VideoMetadata".into(), obj([("Codec", Value::from("h264")), ("DurationMillis", Value::Int(1000 * text.lines().count().max(1) as i64)), ("Format", Value::from("QuickTime / MOV"))]));
            match kind.as_str() {
                "LabelDetection" => {
                    let labels: Vec<Value> = text
                        .lines()
                        .enumerate()
                        .flat_map(|(sec, line)| line.split_whitespace().filter(|w| !w.starts_with("face:") && w.len() > 2).map(move |w| obj([("Timestamp", Value::Int(sec as i64 * 1000)), ("Label", obj([("Name", Value::from(format!("{}{}", w[..1].to_ascii_uppercase(), &w[1..]))), ("Confidence", Value::Float(95.0))]))])).collect::<Vec<_>>())
                        .collect();
                    m.insert("Labels".into(), Value::Array(labels));
                }
                "TextDetection" => {
                    let dets: Vec<Value> = text.lines().enumerate().map(|(sec, l)| obj([("Timestamp", Value::Int(sec as i64 * 1000)), ("TextDetection", obj([("DetectedText", Value::from(l.trim())), ("Type", Value::from("LINE")), ("Confidence", Value::Float(97.0))]))])).collect();
                    m.insert("TextDetections".into(), Value::Array(dets));
                }
                "FaceDetection" => {
                    let faces: Vec<Value> = text.lines().enumerate().flat_map(|(sec, l)| faces_of(l.as_bytes()).into_iter().enumerate().map(move |(i, _)| obj([("Timestamp", Value::Int(sec as i64 * 1000)), ("Face", obj([("BoundingBox", bbox(i)), ("Confidence", Value::Float(99.0))]))]))).collect();
                    m.insert("Faces".into(), Value::Array(faces));
                }
                _ => {
                    let stored = s.gaps.rek.collections.get(coll).map(|c| c.1.clone()).unwrap_or_default();
                    let persons: Vec<Value> = text
                        .lines()
                        .enumerate()
                        .flat_map(|(sec, l)| {
                            let stored = stored.clone();
                            faces_of(l.as_bytes()).into_iter().enumerate().map(move |(i, who)| {
                                let matches: Vec<Value> = stored.iter().filter(|f| f.1 == who).enumerate().map(|(j, f)| obj([("Similarity", Value::Float(99.0)), ("Face", face_value(f, j))])).collect();
                                obj([("Timestamp", Value::Int(sec as i64 * 1000)), ("Person", obj([("Index", Value::Int(i as i64))])), ("FaceMatches", Value::Array(matches))])
                            })
                        })
                        .collect();
                    m.insert("Persons".into(), Value::Array(persons));
                }
            }
            Some(ok(Value::Object(m)))
        }
        _ => None,
    }
}

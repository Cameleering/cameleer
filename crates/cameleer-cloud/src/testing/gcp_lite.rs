//! Pub/Sub Lite emulation for [`super::GcpMock`] on the gRPC port (bearer tokens verified):
//! `AdminService` (CreateTopic, GetTopicPartitions, DeleteTopic, CreateSubscription,
//! GetSubscription, DeleteSubscription), `PublisherService/Publish` (offsets per partition),
//! `SubscriberService/Subscribe` (initial location: committed cursor / head / an explicit cursor,
//! flow control by messages and bytes) and `CursorService/StreamingCommitCursor`.

use std::collections::BTreeMap;
use std::time::Duration;

use cameleer_core::value::{ObjectMap, Value};
use cameleer_grpc::grpc::server::ServerCall;
use cameleer_grpc::grpc::{Code, Status};

use super::{GcpMock, Shared};
use crate::gcp::pubsub_lite::pool;
use crate::util::{now_millis, obj};

/// A stored message: (message, publish time millis, size).
type LiteMsg = (Value, i64, i64);

#[derive(Default)]
pub(super) struct LiteState {
    /// topic path -> partitions of messages
    topics: BTreeMap<String, Vec<Vec<LiteMsg>>>,
    /// subscription path -> (topic path, committed offset per partition)
    subs: BTreeMap<String, (String, Vec<i64>)>,
    commits: usize,
}

impl GcpMock {
    /// Messages (data) of a Pub/Sub Lite topic partition.
    pub fn lite_messages(&self, topic_path: &str, partition: usize) -> Vec<Vec<u8>> {
        self.state.lock().unwrap().lite.topics.get(topic_path).and_then(|p| p.get(partition)).map(|m| m.iter().map(|(v, _, _)| v.get("data").map(|d| d.to_bytes()).unwrap_or_default()).collect()).unwrap_or_default()
    }
    /// Committed cursors of a Pub/Sub Lite subscription.
    pub fn lite_committed(&self, sub_path: &str) -> Vec<i64> {
        self.state.lock().unwrap().lite.subs.get(sub_path).map(|s| s.1.clone()).unwrap_or_default()
    }
    /// Cursor commits received.
    pub fn lite_commits(&self) -> usize {
        self.state.lock().unwrap().lite.commits
    }
}

fn m(name: &str) -> String {
    format!("google.cloud.pubsublite.v1.{}", name)
}

fn dec(name: &str, b: &[u8]) -> Result<Value, Status> {
    pool().decode(&m(name), b).map_err(|e| Status::new(Code::InvalidArgument, e.to_string()))
}

fn enc(name: &str, v: &Value) -> Result<Vec<u8>, Status> {
    pool().encode(&m(name), v).map_err(|e| Status::new(Code::Internal, e.to_string()))
}

fn ts(ms: i64) -> Value {
    obj([("seconds", Value::Int(ms / 1000)), ("nanos", Value::Int((ms % 1000) * 1_000_000))])
}

/// Handle a Pub/Sub Lite call; None when the service is not a Pub/Sub Lite one.
pub(super) fn handle(st: &Shared, call: &ServerCall) -> Option<Status> {
    let svc = call.service().strip_prefix("google.cloud.pubsublite.v1.")?.to_string();
    Some(match (svc.as_str(), call.method()) {
        ("AdminService", method) => admin(st, call, method),
        ("PublisherService", "Publish") => publish(st, call),
        ("SubscriberService", "Subscribe") => subscribe(st, call),
        ("CursorService", "StreamingCommitCursor") => commit(st, call),
        (s, me) => Status::new(Code::Unimplemented, format!("Method not found: {}/{}", s, me)),
    })
}

fn admin(st: &Shared, call: &ServerCall, method: &str) -> Status {
    let Ok(Some(b)) = call.recv() else { return Status::new(Code::InvalidArgument, "missing request") };
    let (req_t, resp): (&str, Result<(String, Value), Status>) = match method {
        "CreateTopic" => ("CreateTopicRequest", (|| {
            let r = dec("CreateTopicRequest", &b)?;
            let name = format!("{}/topics/{}", r.get("parent").map(|p| p.to_string()).unwrap_or_default(), r.get("topic_id").map(|t| t.to_string()).unwrap_or_default());
            let count = r.get("topic").and_then(|t| t.get("partition_config")).and_then(|p| p.get("count")).and_then(|c| c.as_i64()).unwrap_or(1).max(1);
            let mut s = st.lock().unwrap();
            if s.lite.topics.contains_key(&name) {
                return Err(Status::new(Code::AlreadyExists, format!("Topic {} already exists", name)));
            }
            s.lite.topics.insert(name.clone(), vec![Vec::new(); count as usize]);
            let mut t = r.get("topic").and_then(|t| t.as_object().cloned()).unwrap_or_default();
            t.insert("name".into(), Value::from(name));
            Ok(("Topic".to_string(), Value::Object(t)))
        })()),
        "GetTopicPartitions" => ("GetTopicPartitionsRequest", (|| {
            let r = dec("GetTopicPartitionsRequest", &b)?;
            let name = r.get("name").map(|n| n.to_string()).unwrap_or_default();
            let s = st.lock().unwrap();
            let n = s.lite.topics.get(&name).map(|p| p.len()).ok_or_else(|| Status::new(Code::NotFound, format!("Topic {} not found", name)))?;
            Ok(("TopicPartitions".to_string(), obj([("partition_count", Value::Int(n as i64))])))
        })()),
        "DeleteTopic" => ("DeleteTopicRequest", (|| {
            let r = dec("DeleteTopicRequest", &b)?;
            let name = r.get("name").map(|n| n.to_string()).unwrap_or_default();
            st.lock().unwrap().lite.topics.remove(&name).ok_or_else(|| Status::new(Code::NotFound, format!("Topic {} not found", name)))?;
            Ok(("Empty".to_string(), obj([])))
        })()),
        "CreateSubscription" => ("CreateSubscriptionRequest", (|| {
            let r = dec("CreateSubscriptionRequest", &b)?;
            let name = format!("{}/subscriptions/{}", r.get("parent").map(|p| p.to_string()).unwrap_or_default(), r.get("subscription_id").map(|t| t.to_string()).unwrap_or_default());
            let topic = r.get("subscription").and_then(|x| x.get("topic")).map(|t| t.to_string()).unwrap_or_default();
            let mut s = st.lock().unwrap();
            let n = s.lite.topics.get(&topic).map(|p| p.len()).ok_or_else(|| Status::new(Code::NotFound, format!("Topic {} not found", topic)))?;
            if s.lite.subs.contains_key(&name) {
                return Err(Status::new(Code::AlreadyExists, format!("Subscription {} already exists", name)));
            }
            s.lite.subs.insert(name.clone(), (topic.clone(), vec![0; n]));
            Ok(("Subscription".to_string(), obj([("name", Value::from(name)), ("topic", Value::from(topic))])))
        })()),
        "GetSubscription" | "DeleteSubscription" => ("GetSubscriptionRequest", (|| {
            let r = dec("GetSubscriptionRequest", &b)?;
            let name = r.get("name").map(|n| n.to_string()).unwrap_or_default();
            let mut s = st.lock().unwrap();
            let topic = s.lite.subs.get(&name).map(|x| x.0.clone()).ok_or_else(|| Status::new(Code::NotFound, format!("Subscription {} not found", name)))?;
            if method == "DeleteSubscription" {
                s.lite.subs.remove(&name);
                return Ok(("Empty".to_string(), obj([])));
            }
            Ok(("Subscription".to_string(), obj([("name", Value::from(name)), ("topic", Value::from(topic))])))
        })()),
        other => return Status::new(Code::Unimplemented, format!("AdminService/{} is not emulated", other)),
    };
    let _ = req_t;
    match resp.and_then(|(t, v)| enc(&t, &v)) {
        Ok(bytes) => {
            let _ = call.send(&bytes);
            Status::ok()
        }
        Err(s) => s,
    }
}

fn publish(st: &Shared, call: &ServerCall) -> Status {
    let Ok(Some(b)) = call.recv() else { return Status::new(Code::InvalidArgument, "missing initial request") };
    let init = match dec("PublishRequest", &b) {
        Ok(v) => v,
        Err(s) => return s,
    };
    let Some(ir) = init.get("initial_request") else { return Status::new(Code::InvalidArgument, "The first request must be an initial request") };
    let topic = ir.get("topic").map(|t| t.to_string()).unwrap_or_default();
    let partition = ir.get("partition").and_then(|p| p.as_i64()).unwrap_or(0) as usize;
    {
        let s = st.lock().unwrap();
        match s.lite.topics.get(&topic) {
            None => return Status::new(Code::NotFound, format!("Topic {} not found", topic)),
            Some(p) if partition >= p.len() => return Status::new(Code::InvalidArgument, format!("Invalid partition {}", partition)),
            _ => {}
        }
    }
    match enc("PublishResponse", &obj([("initial_response", obj([]))])) {
        Ok(r) => {
            if call.send(&r).is_err() {
                return Status::ok();
            }
        }
        Err(s) => return s,
    }
    while let Ok(Some(b)) = call.recv() {
        let r = match dec("PublishRequest", &b) {
            Ok(v) => v,
            Err(s) => return s,
        };
        let msgs = r.get("message_publish_request").and_then(|p| p.get("messages")).and_then(|m| m.as_array()).cloned().unwrap_or_default();
        let start = {
            let mut s = st.lock().unwrap();
            let Some(parts) = s.lite.topics.get_mut(&topic) else { return Status::new(Code::NotFound, "Topic deleted") };
            let p = &mut parts[partition];
            let start = p.len() as i64;
            let now = now_millis();
            for m in &msgs {
                let size = m.get("data").map(|d| d.to_bytes().len()).unwrap_or(0) as i64 + m.get("key").map(|k| k.to_bytes().len()).unwrap_or(0) as i64;
                p.push((m.clone(), now, size));
            }
            start
        };
        let resp = obj([("message_response", obj([("cursor_ranges", Value::Array(vec![obj([("start_cursor", obj([("offset", Value::Int(start))])), ("start_index", Value::Int(0)), ("end_index", Value::Int(msgs.len() as i64))])]))]))]);
        match enc("PublishResponse", &resp) {
            Ok(bytes) => {
                if call.send(&bytes).is_err() {
                    break;
                }
            }
            Err(s) => return s,
        }
    }
    Status::ok()
}

fn subscribe(st: &Shared, call: &ServerCall) -> Status {
    let Ok(Some(b)) = call.recv() else { return Status::new(Code::InvalidArgument, "missing initial request") };
    let init = match dec("SubscribeRequest", &b) {
        Ok(v) => v,
        Err(s) => return s,
    };
    let Some(ir) = init.get("initial") else { return Status::new(Code::InvalidArgument, "The first request must be an initial request") };
    let sub = ir.get("subscription").map(|t| t.to_string()).unwrap_or_default();
    let partition = ir.get("partition").and_then(|p| p.as_i64()).unwrap_or(0) as usize;
    let (topic, committed) = {
        let s = st.lock().unwrap();
        match s.lite.subs.get(&sub) {
            Some((t, c)) if partition < c.len() => (t.clone(), c[partition]),
            Some(_) => return Status::new(Code::InvalidArgument, format!("Invalid partition {}", partition)),
            None => return Status::new(Code::NotFound, format!("Subscription {} not found", sub)),
        }
    };
    let loc = ir.get("initial_location").cloned().unwrap_or_default();
    let mut next = match (loc.get("named_target").map(|n| n.to_string()), loc.get("cursor").and_then(|c| c.get("offset")).and_then(|o| o.as_i64())) {
        (_, Some(o)) => o,
        (Some(n), _) if n == "HEAD" || n == "1" => {
            let s = st.lock().unwrap();
            s.lite.topics.get(&topic).and_then(|p| p.get(partition)).map(|p| p.len() as i64).unwrap_or(0)
        }
        _ => committed,
    };
    if let Ok(r) = enc("SubscribeResponse", &obj([("initial", obj([("cursor", obj([("offset", Value::Int(next))]))]))])) {
        if call.send(&r).is_err() {
            return Status::ok();
        }
    }
    let (mut allowed_msgs, mut allowed_bytes) = (0i64, 0i64);
    // the reader of flow control requests runs inline: requests are small and arrive first
    let (tx, rx) = std::sync::mpsc::channel::<Value>();
    let reader = {
        let call = call.clone();
        std::thread::spawn(move || {
            while let Ok(Some(b)) = call.recv() {
                match dec("SubscribeRequest", &b) {
                    Ok(v) => {
                        if tx.send(v).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        })
    };
    while !call.is_cancelled() {
        while let Ok(r) = rx.try_recv() {
            if let Some(fc) = r.get("flow_control") {
                allowed_msgs += fc.get("allowed_messages").and_then(|a| a.as_i64()).unwrap_or(0);
                allowed_bytes += fc.get("allowed_bytes").and_then(|a| a.as_i64()).unwrap_or(0);
            }
            if let Some(o) = r.get("seek").and_then(|s| s.get("cursor")).and_then(|c| c.get("offset")).and_then(|o| o.as_i64()) {
                next = o;
            }
        }
        let batch: Vec<Value> = {
            let s = st.lock().unwrap();
            let msgs = s.lite.topics.get(&topic).and_then(|p| p.get(partition)).cloned().unwrap_or_default();
            let mut out = Vec::new();
            while (next as usize) < msgs.len() && allowed_msgs > 0 && allowed_bytes > 0 {
                let (msg, publish, size) = &msgs[next as usize];
                out.push(obj([("cursor", obj([("offset", Value::Int(next))])), ("publish_time", ts(*publish)), ("message", msg.clone()), ("size_bytes", Value::Int(*size))]));
                allowed_msgs -= 1;
                allowed_bytes -= size;
                next += 1;
            }
            out
        };
        if batch.is_empty() {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        match enc("SubscribeResponse", &obj([("messages", obj([("messages", Value::Array(batch))]))])) {
            Ok(bytes) => {
                if call.send(&bytes).is_err() {
                    break;
                }
            }
            Err(s) => return s,
        }
    }
    drop(rx);
    let _ = reader.join();
    Status::ok()
}

fn commit(st: &Shared, call: &ServerCall) -> Status {
    let Ok(Some(b)) = call.recv() else { return Status::new(Code::InvalidArgument, "missing initial request") };
    let init = match dec("StreamingCommitCursorRequest", &b) {
        Ok(v) => v,
        Err(s) => return s,
    };
    let Some(ir) = init.get("initial") else { return Status::new(Code::InvalidArgument, "The first request must be an initial request") };
    let sub = ir.get("subscription").map(|t| t.to_string()).unwrap_or_default();
    let partition = ir.get("partition").and_then(|p| p.as_i64()).unwrap_or(0) as usize;
    if !st.lock().unwrap().lite.subs.get(&sub).is_some_and(|s| partition < s.1.len()) {
        return Status::new(Code::NotFound, format!("Subscription {} / partition {} not found", sub, partition));
    }
    if let Ok(r) = enc("StreamingCommitCursorResponse", &obj([("initial", obj([]))])) {
        if call.send(&r).is_err() {
            return Status::ok();
        }
    }
    while let Ok(Some(b)) = call.recv() {
        let r = match dec("StreamingCommitCursorRequest", &b) {
            Ok(v) => v,
            Err(s) => return s,
        };
        let Some(o) = r.get("commit").and_then(|c| c.get("cursor")).and_then(|c| c.get("offset")).and_then(|o| o.as_i64()) else { continue };
        {
            let mut s = st.lock().unwrap();
            if let Some(x) = s.lite.subs.get_mut(&sub) {
                x.1[partition] = o;
            }
            s.lite.commits += 1;
        }
        let mut ack = ObjectMap::new();
        ack.insert("commit".into(), obj([("acknowledged_commits", Value::Int(1))]));
        match enc("StreamingCommitCursorResponse", &Value::Object(ack)) {
            Ok(bytes) => {
                if call.send(&bytes).is_err() {
                    break;
                }
            }
            Err(s) => return s,
        }
    }
    Status::ok()
}

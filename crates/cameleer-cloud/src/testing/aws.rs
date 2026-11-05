//! In-memory AWS emulator: S3 (REST/XML, including multipart uploads), SQS (JSON 1.0), SNS
//! (Query), DynamoDB (JSON 1.0 subset), Lambda (invoke registered closures), Kinesis, Firehose,
//! EventBridge (rules with simple pattern matching delivering to emulated SQS queues), Secrets
//! Manager, Step Functions (Pass/Succeed/Fail state machines), SES, CloudWatch, STS (issues
//! temporary credentials that the emulator then accepts), IAM and EC2, on one port. Every
//! request must carry a valid SigV4 signature (header or presigned query) for a registered
//! access key, plus the matching `X-Amz-Security-Token` for temporary credentials; the
//! service is taken from the credential scope. Bad signatures get `403 SignatureDoesNotMatch`.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use cameleer_core::value::{ObjectMap, Value};

#[path = "aws_ext.rs"]
mod ext;
#[path = "aws_more.rs"]
mod more;
#[path = "aws_tail.rs"]
mod tail;
#[path = "aws_gaps.rs"]
mod gaps;
pub use ext::{MetricDatum, SentEmail};

use super::{Handler, MockServer};
use crate::aws::ddb::{from_attribute_value, marshal_item, unmarshal_item};
use crate::aws::sigv4::{verify_request, AuthFailure};
use crate::http::{Response, ServerRequest};
use crate::util::{http_date, iso8601_millis, md5_hex, now_millis, now_secs, obj, parse_form, parse_json, percent_decode, random_hex, uuid, xml_escape};

/// Account id used in queue URLs and ARNs.
pub const ACCOUNT: &str = "000000000000";

/// A registered Lambda function: payload in, payload out (`Err` = function error).
pub type LambdaFn = Arc<dyn Fn(&[u8]) -> Result<Vec<u8>, String> + Send + Sync>;

#[derive(Clone, Debug)]
struct S3Obj {
    data: Vec<u8>,
    content_type: String,
    etag: String,
    last_modified: i64,
    metadata: Vec<(String, String)>,
    storage_class: String,
}

#[derive(Default)]
struct Bucket {
    created: i64,
    objects: BTreeMap<String, S3Obj>,
}

#[derive(Clone, Debug)]
struct SqsMsg {
    id: String,
    body: String,
    attributes: Value,
    sent: i64,
    visible_at: i64,
    receipt: Option<String>,
    receive_count: i64,
}

#[derive(Default)]
struct Queue {
    attributes: BTreeMap<String, String>,
    messages: Vec<SqsMsg>,
    /// FIFO deduplication ids seen in the last 5 minutes: id -> (time, message id).
    dedup: HashMap<String, (i64, String)>,
}

/// `MD5OfMessageAttributes` as SQS computes it: attributes sorted by name, each encoded as
/// length-prefixed name, data type, a transport byte (1 string / number, 2 binary) and the
/// length-prefixed value.
fn message_attributes_md5(attrs: &Value) -> Option<String> {
    let a = attrs.as_object().filter(|a| !a.is_empty())?;
    let mut names: Vec<&String> = a.keys().collect();
    names.sort();
    let mut buf = Vec::new();
    let put = |b: &[u8], buf: &mut Vec<u8>| {
        buf.extend_from_slice(&(b.len() as u32).to_be_bytes());
        buf.extend_from_slice(b);
    };
    for n in names {
        let v = &a[n];
        let dt = v.get("DataType").map(|d| d.to_string()).unwrap_or_else(|| "String".into());
        put(n.as_bytes(), &mut buf);
        put(dt.as_bytes(), &mut buf);
        if dt.starts_with("Binary") {
            buf.push(2);
            let raw = v.get("BinaryValue").map(|b| match b {
                Value::Bytes(x) => x.clone(),
                other => crate::util::base64_decode(&other.to_string()).unwrap_or_default(),
            });
            put(&raw.unwrap_or_default(), &mut buf);
        } else {
            buf.push(1);
            put(v.get("StringValue").map(|s| s.to_string()).unwrap_or_default().as_bytes(), &mut buf);
        }
    }
    Some(md5_hex(&buf))
}

/// A message published to an emulated SNS topic.
#[derive(Clone, Debug, PartialEq)]
pub struct PublishedMessage {
    pub message_id: String,
    pub message: String,
    pub subject: Option<String>,
    pub message_structure: Option<String>,
    pub attributes: BTreeMap<String, String>,
    pub group_id: Option<String>,
}

struct Table {
    hash: String,
    range: Option<String>,
    key_schema: Value,
    attribute_definitions: Value,
    items: Vec<ObjectMap>,
}

struct Upload {
    bucket: String,
    key: String,
    content_type: String,
    metadata: Vec<(String, String)>,
    parts: BTreeMap<i32, Vec<u8>>,
}

#[derive(Default)]
struct State {
    credentials: HashMap<String, String>,
    session_tokens: HashMap<String, String>,
    visibility_changes: usize,
    uploads: HashMap<String, Upload>,
    completed_uploads: usize,
    min_part_size: usize,
    ext: ext::ExtState,
    more: more::MoreState,
    tail: tail::TailState,
    gaps: gaps::GapsState,
    buckets: BTreeMap<String, Bucket>,
    queues: BTreeMap<String, Queue>,
    topics: BTreeMap<String, Vec<PublishedMessage>>,
    tables: BTreeMap<String, Table>,
    functions: BTreeMap<String, LambdaFn>,
    auth_failures: usize,
    region: String,
}

type Shared = Arc<(Mutex<State>, Condvar)>;

/// The AWS emulator.
pub struct AwsMock {
    server: MockServer,
    state: Shared,
    access_key: String,
    secret_key: String,
}

impl AwsMock {
    /// Default test credentials.
    pub const ACCESS_KEY: &'static str = "AKIAMOCKACCESSKEY";
    pub const SECRET_KEY: &'static str = "mock/secret+key/1234567890";

    /// Start with the default credentials and region `us-east-1`.
    pub fn start() -> io::Result<AwsMock> {
        Self::start_with(Self::ACCESS_KEY, Self::SECRET_KEY)
    }

    /// Start accepting one access key / secret pair.
    pub fn start_with(access_key: &str, secret_key: &str) -> io::Result<AwsMock> {
        Self::start_inner(access_key, secret_key, false)
    }

    /// Start over HTTPS (self-signed certificate for `localhost`, see [`AwsMock::cert_pem`]).
    pub fn start_tls() -> io::Result<AwsMock> {
        Self::start_inner(Self::ACCESS_KEY, Self::SECRET_KEY, true)
    }

    fn start_inner(access_key: &str, secret_key: &str, tls: bool) -> io::Result<AwsMock> {
        let state: Shared = Arc::new((Mutex::new(State { region: "us-east-1".into(), min_part_size: 5 * 1024 * 1024, ..Default::default() }), Condvar::new()));
        state.0.lock().unwrap().credentials.insert(access_key.to_string(), secret_key.to_string());
        let st = state.clone();
        let handler: Handler = Arc::new(move |req| handle(&st, req));
        let st2 = state.clone();
        let h2: super::H2Handler = Arc::new(move |req, stream| handle_h2(&st2, req, stream));
        let server = if tls { MockServer::start_tls(handler)? } else { MockServer::start_with_h2(handler, h2)? };
        Ok(AwsMock { server, state, access_key: access_key.into(), secret_key: secret_key.into() })
    }

    /// The server certificate (PEM) when started with [`AwsMock::start_tls`].
    pub fn cert_pem(&self) -> Option<&str> {
        self.server.cert_pem.as_deref()
    }

    pub fn url(&self) -> String {
        self.server.url()
    }

    pub fn port(&self) -> u16 {
        self.server.port()
    }

    /// Endpoint options for component uris:
    /// `accessKey=..&secretKey=..&region=us-east-1&uriEndpointOverride=..&overrideEndpoint=true`.
    pub fn endpoint_options(&self) -> String {
        format!(
            "accessKey=RAW({})&secretKey=RAW({})&region=us-east-1&uriEndpointOverride={}&overrideEndpoint=true",
            self.access_key,
            self.secret_key,
            self.url()
        )
    }

    /// Accept another access key.
    pub fn add_credentials(&self, access_key: &str, secret_key: &str) {
        self.lock().credentials.insert(access_key.into(), secret_key.into());
    }

    /// Accept temporary credentials (requests must also carry `X-Amz-Security-Token: token`).
    pub fn add_session_credentials(&self, access_key: &str, secret_key: &str, token: &str) {
        let mut s = self.lock();
        s.credentials.insert(access_key.into(), secret_key.into());
        s.session_tokens.insert(access_key.into(), token.into());
    }

    /// Number of `ChangeMessageVisibility` calls served.
    pub fn visibility_changes(&self) -> usize {
        self.lock().visibility_changes
    }

    /// Minimum size of non-final multipart parts (5 MiB like S3 by default).
    pub fn set_min_part_size(&self, bytes: usize) {
        self.lock().min_part_size = bytes;
    }

    /// (uploads in progress, completed multipart uploads).
    pub fn multipart_stats(&self) -> (usize, usize) {
        let s = self.lock();
        (s.uploads.len(), s.completed_uploads)
    }

    /// Number of requests rejected by authentication.
    pub fn auth_failures(&self) -> usize {
        self.lock().auth_failures
    }

    pub fn request_count(&self) -> usize {
        self.server.request_count()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.0.lock().unwrap()
    }

    // ---- S3 helpers
    pub fn create_bucket(&self, name: &str) {
        self.lock().buckets.entry(name.into()).or_insert_with(|| Bucket { created: now_secs(), objects: BTreeMap::new() });
    }
    pub fn bucket_exists(&self, name: &str) -> bool {
        self.lock().buckets.contains_key(name)
    }
    pub fn put_object(&self, bucket: &str, key: &str, data: &[u8]) {
        self.create_bucket(bucket);
        self.lock().buckets.get_mut(bucket).unwrap().objects.insert(key.into(), new_obj(data.to_vec(), "application/octet-stream".into(), vec![], "STANDARD".into()));
    }
    pub fn object(&self, bucket: &str, key: &str) -> Option<Vec<u8>> {
        self.lock().buckets.get(bucket).and_then(|b| b.objects.get(key)).map(|o| o.data.clone())
    }
    /// Content type and user metadata of an object.
    pub fn object_info(&self, bucket: &str, key: &str) -> Option<(String, Vec<(String, String)>)> {
        self.lock().buckets.get(bucket).and_then(|b| b.objects.get(key)).map(|o| (o.content_type.clone(), o.metadata.clone()))
    }
    pub fn object_keys(&self, bucket: &str) -> Vec<String> {
        self.lock().buckets.get(bucket).map(|b| b.objects.keys().cloned().collect()).unwrap_or_default()
    }

    // ---- SQS helpers
    /// Create a queue; returns its URL.
    pub fn create_queue(&self, name: &str) -> String {
        self.lock().queues.entry(name.into()).or_default();
        format!("{}/{}/{}", self.url(), ACCOUNT, name)
    }
    pub fn queue_exists(&self, name: &str) -> bool {
        self.lock().queues.contains_key(name)
    }
    /// Bodies of all messages (visible or in flight) of a queue.
    pub fn queue_messages(&self, name: &str) -> Vec<String> {
        self.lock().queues.get(name).map(|q| q.messages.iter().map(|m| m.body.clone()).collect()).unwrap_or_default()
    }
    /// Message attributes (name → string value) of all messages of a queue.
    pub fn queue_message_attributes(&self, name: &str) -> Vec<BTreeMap<String, String>> {
        self.lock()
            .queues
            .get(name)
            .map(|q| {
                q.messages
                    .iter()
                    .map(|m| m.attributes.as_object().map(|a| a.iter().map(|(k, v)| (k.clone(), v.get("StringValue").map(|s| s.to_string()).unwrap_or_default())).collect()).unwrap_or_default())
                    .collect()
            })
            .unwrap_or_default()
    }
    /// Enqueue a message directly.
    pub fn send_sqs(&self, name: &str, body: &str) {
        let (lock, cv) = &*self.state;
        let mut st = lock.lock().unwrap();
        st.queues.entry(name.into()).or_default().messages.push(new_msg(body.into(), Value::Object(ObjectMap::new()), 0));
        cv.notify_all();
    }

    // ---- SNS helpers
    /// Create a topic; returns its ARN.
    pub fn create_topic(&self, name: &str) -> String {
        let mut st = self.lock();
        let arn = format!("arn:aws:sns:{}:{}:{}", st.region, ACCOUNT, name);
        st.topics.entry(arn.clone()).or_default();
        arn
    }
    /// Messages published to a topic (by name).
    pub fn published(&self, topic: &str) -> Vec<PublishedMessage> {
        self.lock().topics.iter().find(|(arn, _)| arn.rsplit(':').next() == Some(topic)).map(|(_, m)| m.clone()).unwrap_or_default()
    }

    // ---- DynamoDB helpers
    /// Create a table with a hash key and optional range key (string attributes).
    pub fn create_table(&self, name: &str, hash: &str, range: Option<&str>) {
        let mut ks = vec![obj([("AttributeName", Value::from(hash)), ("KeyType", Value::from("HASH"))])];
        if let Some(r) = range {
            ks.push(obj([("AttributeName", Value::from(r)), ("KeyType", Value::from("RANGE"))]));
        }
        self.lock().tables.insert(
            name.into(),
            Table { hash: hash.into(), range: range.map(|r| r.to_string()), key_schema: Value::Array(ks), attribute_definitions: Value::Array(vec![]), items: vec![] },
        );
    }
    pub fn table_exists(&self, name: &str) -> bool {
        self.lock().tables.contains_key(name)
    }
    /// Items of a table (unmarshalled).
    pub fn table_items(&self, name: &str) -> Vec<Value> {
        self.lock().tables.get(name).map(|t| t.items.iter().map(|i| unmarshal_item(&Value::Object(i.clone()))).collect()).unwrap_or_default()
    }

    // ---- Lambda helpers
    /// Register a function implementation.
    pub fn register_function(&self, name: &str, f: impl Fn(&[u8]) -> Result<Vec<u8>, String> + Send + Sync + 'static) {
        self.lock().functions.insert(name.into(), Arc::new(f));
    }
}

fn new_obj(data: Vec<u8>, content_type: String, metadata: Vec<(String, String)>, storage_class: String) -> S3Obj {
    S3Obj { etag: format!("\"{}\"", md5_hex(&data)), data, content_type, last_modified: now_secs(), metadata, storage_class }
}

fn new_msg(body: String, attributes: Value, delay: i64) -> SqsMsg {
    let now = now_millis();
    SqsMsg { id: uuid(), body, attributes, sent: now, visible_at: now + delay * 1000, receipt: None, receive_count: 0 }
}

fn request_id() -> String {
    uuid()
}

fn s3_error(status: u16, code: &str, message: &str) -> Response {
    Response::xml(status, format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>{}</Code><Message>{}</Message><RequestId>{}</RequestId></Error>", code, xml_escape(message), request_id()))
        .with_header("x-amz-request-id", request_id())
}

fn json_error(status: u16, prefix: &str, code: &str, message: &str) -> Response {
    Response::json(status, obj([("__type", Value::from(format!("{}#{}", prefix, code))), ("message", Value::from(message))]).to_json())
        .with_header("Content-Type", "application/x-amz-json-1.0")
        .with_header("x-amzn-RequestId", request_id())
}

fn query_error(status: u16, code: &str, message: &str) -> Response {
    Response::xml(
        status,
        format!(
            "<ErrorResponse xmlns=\"http://sns.amazonaws.com/doc/2010-03-31/\"><Error><Type>Sender</Type><Code>{}</Code><Message>{}</Message></Error><RequestId>{}</RequestId></ErrorResponse>",
            code,
            xml_escape(message),
            request_id()
        ),
    )
}

fn auth_error(req: &ServerRequest, f: &AuthFailure) -> Response {
    let service = f.service.clone().unwrap_or_else(|| {
        if req.header("X-Amz-Target").is_some() {
            "sqs".into()
        } else if req.text().contains("Action=") {
            "sns".into()
        } else {
            "s3".into()
        }
    });
    // an unknown access key is InvalidAccessKeyId only for S3; Query services report
    // InvalidClientTokenId and JSON services UnrecognizedClientException
    let unknown_key = f.code == "InvalidAccessKeyId";
    let query_code = if unknown_key { "InvalidClientTokenId" } else { f.code };
    let query_msg = if unknown_key { "The security token included in the request is invalid." } else { f.message.as_str() };
    match service.as_str() {
        "s3" => s3_error(403, f.code, &f.message),
        "sns" | "ses" | "monitoring" | "sts" | "iam" | "ec2" => query_error(403, query_code, query_msg),
        // SQS speaks both the Query (XML) and the JSON protocol: answer in the request's
        "sqs" if req.header("X-Amz-Target").is_none() => query_error(403, query_code, query_msg),
        "sqs" if unknown_key => json_error(403, "com.amazon.coral.service", "UnrecognizedClientException", query_msg).with_header("x-amzn-query-error", "InvalidClientTokenId;Sender"),
        "sqs" => json_error(403, "com.amazon.coral.service", f.code, &f.message).with_header("x-amzn-query-error", format!("{};Sender", f.code)),
        "lambda" | "polly" | "mq" | "eks" | "kafka" | "bedrock" => Response::json(403, obj([("message", Value::from(f.message.clone()))]).to_json()).with_header("x-amzn-ErrorType", format!("{}:", f.code)),
        // an unknown access key is UnrecognizedClientException for the JSON services
        _ if f.code == "InvalidAccessKeyId" => json_error(403, "com.amazon.coral.service", "UnrecognizedClientException", "The security token included in the request is invalid."),
        _ => json_error(403, "com.amazon.coral.service", f.code, &f.message),
    }
}

/// HTTP/2 requests (h2c): event stream operations (`SubscribeToShard`) stream their events,
/// every other request is answered like over HTTP/1.1.
fn handle_h2(st: &Shared, h2req: cameleer_grpc::h2::ServerRequest, stream: cameleer_grpc::h2::H2Stream) {
    let body = stream.read_body(Some(Duration::from_secs(30))).unwrap_or_default();
    let (path, query) = match h2req.path.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (h2req.path.clone(), String::new()),
    };
    let mut headers: Vec<(String, String)> = h2req.headers.clone();
    headers.push(("host".into(), h2req.authority.clone()));
    let req = ServerRequest { method: h2req.method.clone(), path, query, headers, body };
    let creds = st.0.lock().unwrap().credentials.clone();
    let resp = match verify_request(&req, &|k| creds.get(k).cloned(), now_secs()) {
        Err(f) => {
            st.0.lock().unwrap().auth_failures += 1;
            auth_error(&req, &f)
        }
        Ok(v) if v.service == "kinesis" && req.header("X-Amz-Target").is_some_and(|t| t.ends_with(".SubscribeToShard")) => {
            gaps::subscribe_to_shard(st, &req, &stream);
            return;
        }
        Ok(_) => handle(st, &req),
    };
    let hs: Vec<(&str, &str)> = resp.headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let hs: Vec<(String, String)> = hs.iter().map(|(k, v)| (k.to_ascii_lowercase(), v.to_string())).filter(|(k, _)| k != "content-length" && k != "connection").collect();
    let hs: Vec<(&str, &str)> = hs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let _ = cameleer_grpc::h2::ServerRequest::respond(&stream, resp.status, &hs, &resp.body);
}

fn handle(st: &Shared, req: &ServerRequest) -> Response {
    if req.header("Authorization").is_none() && req.method == "POST" && req.text().contains("Action=AssumeRoleWithWebIdentity") {
        return gaps::assume_role_with_web_identity(st, req);
    }
    let creds = st.0.lock().unwrap().credentials.clone();
    let verified = match verify_request(req, &|k| creds.get(k).cloned(), now_secs()) {
        Ok(v) => v,
        Err(f) => {
            st.0.lock().unwrap().auth_failures += 1;
            return auth_error(req, &f);
        }
    };
    let expected_token = st.0.lock().unwrap().session_tokens.get(&verified.access_key).cloned();
    if let Some(tok) = expected_token {
        if req.header("X-Amz-Security-Token") != Some(tok.as_str()) {
            st.0.lock().unwrap().auth_failures += 1;
            let f = AuthFailure { code: "InvalidClientTokenId", message: "The security token included in the request is invalid.".into(), service: Some(verified.service.clone()) };
            return auth_error(req, &f);
        }
    }
    dispatch(st, &verified.service, req)
}

/// Routes a verified request to the service emulators.
fn dispatch(st: &Shared, service: &str, req: &ServerRequest) -> Response {
    if service == "dynamodb" {
        match req.header("X-Amz-Target").and_then(|t| t.strip_prefix("DynamoDB_20120810.")) {
            Some("BatchWriteItem") => return ddb_batch_write(st, req),
            Some("TransactWriteItems") => return ddb_transact_write(st, req),
            _ => {}
        }
    }
    let verified = Verified { service: service.to_string() };
    if let Some(resp) = gaps::handle(st, &verified.service, req) {
        return resp;
    }
    if let Some(resp) = tail::handle(st, &verified.service, req) {
        return resp;
    }
    if let Some(resp) = ext::handle(st, &verified.service, req) {
        return resp;
    }
    if let Some(resp) = more::handle(st, &verified.service, req) {
        return resp;
    }
    match verified.service.as_str() {
        "s3" => s3(st, req),
        "sqs" => sqs(st, req),
        "sns" => sns(st, req),
        "dynamodb" => ddb(st, req),
        "lambda" => lambda(st, req),
        other => s3_error(400, "InvalidRequest", &format!("Service {} is not emulated", other)),
    }
}

struct Verified {
    service: String,
}

/// Runs one DynamoDB operation (with the full expression support of the emulator).
fn ddb_call(st: &Shared, req: &ServerRequest, op: &str, body: Value) -> Response {
    let mut r = req.clone();
    r.headers.retain(|(k, _)| !k.eq_ignore_ascii_case("X-Amz-Target"));
    r.headers.push(("X-Amz-Target".into(), format!("DynamoDB_20120810.{}", op)));
    r.body = body.to_json().into_bytes();
    dispatch(st, "dynamodb", &r)
}

/// BatchWriteItem: `PutRequest` / `DeleteRequest` entries for several tables (no
/// `UnprocessedItems`: the emulator always has capacity).
fn ddb_batch_write(st: &Shared, req: &ServerRequest) -> Response {
    let body = match parse_json(&req.body) {
        Ok(b) => b,
        Err(e) => return ddb_err("SerializationException", &e.to_string()),
    };
    let items = body.get("RequestItems").and_then(|v| v.as_object()).cloned().unwrap_or_default();
    let total: usize = items.values().map(|v| v.as_array().map(|a| a.len()).unwrap_or(0)).sum();
    if total == 0 || total > 25 {
        return ddb_err("ValidationException", "Too many items requested for the BatchWriteItem call");
    }
    for (table, reqs) in &items {
        if !st.0.lock().unwrap().tables.contains_key(table) {
            return ddb_err("ResourceNotFoundException", "Requested resource not found");
        }
        for r in reqs.as_array().cloned().unwrap_or_default() {
            let resp = if let Some(p) = r.get("PutRequest") {
                ddb_call(st, req, "PutItem", obj([("TableName", Value::from(table.as_str())), ("Item", p.get("Item").cloned().unwrap_or_default())]))
            } else if let Some(d) = r.get("DeleteRequest") {
                ddb_call(st, req, "DeleteItem", obj([("TableName", Value::from(table.as_str())), ("Key", d.get("Key").cloned().unwrap_or_default())]))
            } else {
                return ddb_err("ValidationException", "Supplied AttributeValue has neither PutRequest nor DeleteRequest");
            };
            if resp.status != 200 {
                return resp;
            }
        }
    }
    Response::json(200, obj([("UnprocessedItems", Value::Object(ObjectMap::new()))]).to_json()).with_header("Content-Type", "application/x-amz-json-1.0")
}

/// TransactWriteItems: `Put` / `Update` / `Delete` / `ConditionCheck`, all or nothing. A
/// failed condition cancels the transaction with per-item `CancellationReasons`.
fn ddb_transact_write(st: &Shared, req: &ServerRequest) -> Response {
    let body = match parse_json(&req.body) {
        Ok(b) => b,
        Err(e) => return ddb_err("SerializationException", &e.to_string()),
    };
    let ops = body.get("TransactItems").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    if ops.is_empty() || ops.len() > 100 {
        return ddb_err("ValidationException", "1 validation error detected: Value at 'transactItems' failed to satisfy constraint: Member must have length between 1 and 100");
    }
    let snapshot: Vec<(String, Vec<ObjectMap>)> = st.0.lock().unwrap().tables.iter().map(|(k, t)| (k.clone(), t.items.clone())).collect();
    let mut reasons = Vec::new();
    let mut failed = false;
    for op in &ops {
        let Some((kind, spec)) = op.as_object().and_then(|o| o.iter().next()).map(|(k, v)| (k.clone(), v.clone())) else { continue };
        let resp = match kind.as_str() {
            "Put" => Some(ddb_call(st, req, "PutItem", spec)),
            "Delete" => Some(ddb_call(st, req, "DeleteItem", spec)),
            "Update" => Some(ddb_call(st, req, "UpdateItem", spec)),
            "ConditionCheck" => {
                // a condition check is an update that changes nothing
                let mut probe = spec.clone();
                if let Value::Object(o) = &mut probe {
                    o.insert("UpdateExpression".into(), Value::from("SET #camelcc = #camelcc"));
                    let names = o.entry("ExpressionAttributeNames".to_string()).or_insert_with(|| Value::Object(ObjectMap::new()));
                    if let Value::Object(n) = names {
                        n.insert("#camelcc".into(), Value::from(spec.get("Key").and_then(|k| k.as_object()).and_then(|k| k.keys().next().cloned()).unwrap_or_default()));
                    }
                }
                Some(ddb_call(st, req, "UpdateItem", probe))
            }
            other => return ddb_err("ValidationException", &format!("Unknown TransactItem {}", other)),
        };
        match resp {
            Some(r) if r.status == 200 => reasons.push(obj([("Code", Value::from("None"))])),
            Some(r) => {
                failed = true;
                let v = parse_json(&r.body).unwrap_or_default();
                let code = v.get("__type").map(|t| t.to_string()).unwrap_or_default();
                let code = code.rsplit('#').next().unwrap_or("").trim_end_matches("Exception").to_string();
                reasons.push(obj([("Code", Value::from(code)), ("Message", v.get("message").or_else(|| v.get("Message")).cloned().unwrap_or_default())]));
            }
            None => {}
        }
    }
    if failed {
        let mut s = st.0.lock().unwrap();
        for (name, items) in snapshot {
            if let Some(t) = s.tables.get_mut(&name) {
                t.items = items;
            }
        }
        let codes: Vec<String> = reasons.iter().map(|r| r.get("Code").map(|c| c.to_string()).unwrap_or_default()).collect();
        let mut v = obj([
            ("__type", Value::from("com.amazonaws.dynamodb.v20120810#TransactionCanceledException")),
            ("message", Value::from(format!("Transaction cancelled, please refer cancellation reasons for specific reasons [{}]", codes.join(", ")))),
        ]);
        if let Value::Object(o) = &mut v {
            o.insert("CancellationReasons".into(), Value::Array(reasons));
        }
        return Response::json(400, v.to_json()).with_header("Content-Type", "application/x-amz-json-1.0");
    }
    Response::json(200, "{}".to_string()).with_header("Content-Type", "application/x-amz-json-1.0")
}

// ---------------------------------------------------------------------------
// S3
// ---------------------------------------------------------------------------

fn s3_target(req: &ServerRequest) -> (String, Option<String>) {
    let host = req.header("Host").unwrap_or("").split(':').next().unwrap_or("").to_string();
    let raw = req.path.trim_start_matches('/');
    if let Some(bucket) = host.strip_suffix(".localhost").filter(|b| !b.is_empty()) {
        let key = if raw.is_empty() { None } else { Some(percent_decode(raw, false)) };
        return (bucket.to_string(), key);
    }
    match raw.split_once('/') {
        Some((b, k)) if !k.is_empty() => (percent_decode(b, false), Some(percent_decode(k, false))),
        Some((b, _)) => (percent_decode(b, false), None),
        None => (percent_decode(raw, false), None),
    }
}

fn s3(st: &Shared, req: &ServerRequest) -> Response {
    let (bucket, key) = s3_target(req);
    let mut s = st.0.lock().unwrap();
    if bucket.is_empty() {
        if req.method != "GET" {
            return s3_error(405, "MethodNotAllowed", "Method not allowed");
        }
        let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListAllMyBucketsResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Owner><ID>mock</ID></Owner><Buckets>");
        for (name, b) in &s.buckets {
            xml.push_str(&format!("<Bucket><Name>{}</Name><CreationDate>{}</CreationDate></Bucket>", xml_escape(name), iso8601_millis(b.created * 1000)));
        }
        xml.push_str("</Buckets></ListAllMyBucketsResult>");
        return Response::xml(200, xml);
    }
    if key.is_none() && req.method == "GET" && req.query.split('&').any(|p| p == "uploads" || p.starts_with("uploads=")) {
        return s3_multipart(&mut s, req, &bucket, "");
    }
    let Some(key) = key else {
        return match req.method.as_str() {
            "PUT" => {
                s.buckets.entry(bucket).or_insert_with(|| Bucket { created: now_secs(), objects: BTreeMap::new() });
                Response::new(200)
            }
            "HEAD" => Response::new(if s.buckets.contains_key(&bucket) { 200 } else { 404 }),
            "DELETE" => match s.buckets.get(&bucket) {
                None => s3_error(404, "NoSuchBucket", "The specified bucket does not exist"),
                Some(b) if !b.objects.is_empty() => s3_error(409, "BucketNotEmpty", "The bucket you tried to delete is not empty"),
                Some(_) => {
                    s.buckets.remove(&bucket);
                    Response::new(204)
                }
            },
            "GET" => {
                let Some(b) = s.buckets.get(&bucket) else { return s3_error(404, "NoSuchBucket", "The specified bucket does not exist") };
                s3_list(&bucket, b, req)
            }
            _ => s3_error(405, "MethodNotAllowed", "Method not allowed"),
        };
    };
    let has_q = |name: &str| req.query.split('&').any(|p| p == name || p.starts_with(&format!("{}=", name)));
    if has_q("uploads") || has_q("uploadId") {
        return s3_multipart(&mut s, req, &bucket, &key);
    }
    match req.method.as_str() {
        "PUT" => {
            if let Some(src) = req.header("x-amz-copy-source") {
                let src = percent_decode(src.trim_start_matches('/'), false);
                let (sb, sk) = src.split_once('/').unwrap_or((&src, ""));
                let Some(o) = s.buckets.get(sb).and_then(|b| b.objects.get(sk)).cloned() else {
                    return s3_error(404, "NoSuchKey", "The specified key does not exist.");
                };
                let Some(dst) = s.buckets.get_mut(&bucket) else { return s3_error(404, "NoSuchBucket", "The specified bucket does not exist") };
                let copy = S3Obj { last_modified: now_secs(), ..o };
                let etag = copy.etag.clone();
                dst.objects.insert(key, copy);
                return Response::xml(
                    200,
                    format!("<CopyObjectResult><LastModified>{}</LastModified><ETag>{}</ETag></CopyObjectResult>", iso8601_millis(now_millis()), xml_escape(&etag)),
                );
            }
            let Some(b) = s.buckets.get_mut(&bucket) else { return s3_error(404, "NoSuchBucket", "The specified bucket does not exist") };
            let meta: Vec<(String, String)> =
                req.headers.iter().filter_map(|(k, v)| k.to_ascii_lowercase().strip_prefix("x-amz-meta-").map(|m| (m.to_string(), v.clone()))).collect();
            let o = new_obj(
                req.body.clone(),
                req.header("Content-Type").unwrap_or("binary/octet-stream").to_string(),
                meta,
                req.header("x-amz-storage-class").unwrap_or("STANDARD").to_string(),
            );
            let etag = o.etag.clone();
            b.objects.insert(key, o);
            Response::new(200).with_header("ETag", etag)
        }
        "GET" | "HEAD" => {
            let Some(b) = s.buckets.get(&bucket) else { return s3_error(404, "NoSuchBucket", "The specified bucket does not exist") };
            let Some(o) = b.objects.get(&key) else {
                return if req.method == "HEAD" { Response::new(404) } else { s3_error(404, "NoSuchKey", "The specified key does not exist.") };
            };
            let mut status = 200;
            let mut data = o.data.clone();
            if let Some(range) = req.header("Range").and_then(|r| r.strip_prefix("bytes=")) {
                let (a, z) = range.split_once('-').unwrap_or((range, ""));
                let a: usize = a.parse().unwrap_or(0);
                let z: usize = z.parse().unwrap_or(data.len().saturating_sub(1)).min(data.len().saturating_sub(1));
                data = if a <= z && a < data.len() { data[a..=z].to_vec() } else { Vec::new() };
                status = 206;
            }
            let mut r = Response::new(status)
                .with_header("Content-Type", o.content_type.clone())
                .with_header("ETag", o.etag.clone())
                .with_header("Last-Modified", http_date(o.last_modified))
                .with_header("x-amz-storage-class", o.storage_class.clone())
                .with_header("Content-Length", data.len().to_string());
            for (k, v) in &o.metadata {
                r = r.with_header(&format!("x-amz-meta-{}", k), v.clone());
            }
            r.with_body(data)
        }
        "DELETE" => {
            if let Some(b) = s.buckets.get_mut(&bucket) {
                b.objects.remove(&key);
                Response::new(204)
            } else {
                s3_error(404, "NoSuchBucket", "The specified bucket does not exist")
            }
        }
        _ => s3_error(405, "MethodNotAllowed", "Method not allowed"),
    }
}

fn s3_multipart(s: &mut State, req: &ServerRequest, bucket: &str, key: &str) -> Response {
    if !s.buckets.contains_key(bucket) {
        return s3_error(404, "NoSuchBucket", "The specified bucket does not exist");
    }
    let upload_id = req.query_param("uploadId").unwrap_or_default();
    let no_upload = || s3_error(404, "NoSuchUpload", "The specified upload does not exist. The upload ID may be invalid, or the upload may have been aborted or completed.");
    match req.method.as_str() {
        "POST" if upload_id.is_empty() => {
            let id = format!("{}{}", random_hex(16), random_hex(16));
            let metadata = req.headers.iter().filter_map(|(k, v)| k.to_ascii_lowercase().strip_prefix("x-amz-meta-").map(|m| (m.to_string(), v.clone()))).collect();
            s.uploads.insert(
                id.clone(),
                Upload { bucket: bucket.into(), key: key.into(), content_type: req.header("Content-Type").unwrap_or("binary/octet-stream").into(), metadata, parts: BTreeMap::new() },
            );
            Response::xml(
                200,
                format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?><InitiateMultipartUploadResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Bucket>{}</Bucket><Key>{}</Key><UploadId>{}</UploadId></InitiateMultipartUploadResult>",
                    xml_escape(bucket),
                    xml_escape(key),
                    id
                ),
            )
        }
        "PUT" => {
            let Some(u) = s.uploads.get_mut(&upload_id).filter(|u| u.bucket == bucket && u.key == key) else { return no_upload() };
            let Some(n) = req.query_param("partNumber").and_then(|n| n.parse::<i32>().ok()).filter(|n| (1..=10000).contains(n)) else {
                return s3_error(400, "InvalidArgument", "Part number must be an integer between 1 and 10000, inclusive");
            };
            let etag = format!("\"{}\"", md5_hex(&req.body));
            u.parts.insert(n, req.body.clone());
            Response::new(200).with_header("ETag", etag)
        }
        "POST" => {
            let Some(u) = s.uploads.get(&upload_id).filter(|u| u.bucket == bucket && u.key == key) else { return no_upload() };
            let Ok(x) = crate::util::Xml::parse(&req.text()) else { return s3_error(400, "MalformedXML", "The XML you provided was not well-formed") };
            let listed: Vec<(i32, String)> = x.descendants("Part").into_iter().map(|p| (x.child_text(p, "PartNumber").and_then(|n| n.trim().parse().ok()).unwrap_or(0), x.child_text(p, "ETag").unwrap_or_default())).collect();
            if listed.is_empty() {
                return s3_error(400, "MalformedXML", "The XML you provided was not well-formed or did not validate against our published schema");
            }
            let mut data = Vec::new();
            let mut md5s = Vec::new();
            for (i, (n, etag)) in listed.iter().enumerate() {
                if i > 0 && *n <= listed[i - 1].0 {
                    return s3_error(400, "InvalidPartOrder", "The list of parts was not in ascending order. Parts must be ordered by part number.");
                }
                let Some(part) = u.parts.get(n) else { return s3_error(400, "InvalidPart", "One or more of the specified parts could not be found.") };
                if etag.trim_matches('"') != md5_hex(part) {
                    return s3_error(400, "InvalidPart", "One or more of the specified parts could not be found. The part may not have been uploaded, or the specified entity tag may not match the part's entity tag.");
                }
                if i + 1 < listed.len() && part.len() < s.min_part_size {
                    return s3_error(400, "EntityTooSmall", "Your proposed upload is smaller than the minimum allowed object size.");
                }
                data.extend_from_slice(part);
                md5s.extend_from_slice(&crate::util::md5(part));
            }
            let u = s.uploads.remove(&upload_id).expect("checked");
            let etag = format!("\"{}-{}\"", md5_hex(&md5s), listed.len());
            let mut o = new_obj(data, u.content_type, u.metadata, "STANDARD".into());
            o.etag = etag.clone();
            s.buckets.get_mut(bucket).expect("checked").objects.insert(key.to_string(), o);
            s.completed_uploads += 1;
            Response::xml(
                200,
                format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?><CompleteMultipartUploadResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Location>/{0}/{1}</Location><Bucket>{0}</Bucket><Key>{1}</Key><ETag>{2}</ETag></CompleteMultipartUploadResult>",
                    xml_escape(bucket),
                    xml_escape(key),
                    xml_escape(&etag)
                ),
            )
        }
        "DELETE" => match s.uploads.remove(&upload_id) {
            Some(_) => Response::new(204),
            None => no_upload(),
        },
        // ListMultipartUploads (GET /bucket?uploads)
        "GET" if upload_id.is_empty() => {
            let prefix = req.query_param("prefix").unwrap_or_default();
            let mut list: Vec<(&String, &Upload)> = s.uploads.iter().filter(|(_, u)| u.bucket == bucket && u.key.starts_with(&prefix)).collect();
            list.sort_by(|a, b| a.1.key.cmp(&b.1.key).then_with(|| a.0.cmp(b.0)));
            let items: String = list
                .iter()
                .map(|(id, u)| format!("<Upload><Key>{}</Key><UploadId>{}</UploadId><StorageClass>STANDARD</StorageClass><Initiated>{}</Initiated></Upload>", xml_escape(&u.key), id, iso8601_millis(now_millis())))
                .collect();
            Response::xml(
                200,
                format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListMultipartUploadsResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Bucket>{}</Bucket><KeyMarker></KeyMarker><UploadIdMarker></UploadIdMarker><Prefix>{}</Prefix><MaxUploads>1000</MaxUploads><IsTruncated>false</IsTruncated>{}</ListMultipartUploadsResult>",
                    xml_escape(bucket),
                    xml_escape(&prefix),
                    items
                ),
            )
        }
        // ListParts (GET /bucket/key?uploadId=..)
        "GET" => {
            let Some(u) = s.uploads.get(&upload_id).filter(|u| u.bucket == bucket && u.key == key) else { return no_upload() };
            let marker: i32 = req.query_param("part-number-marker").and_then(|m| m.parse().ok()).unwrap_or(0);
            let max: usize = req.query_param("max-parts").and_then(|m| m.parse().ok()).unwrap_or(1000);
            let all: Vec<(&i32, &Vec<u8>)> = u.parts.iter().filter(|(n, _)| **n > marker).collect();
            let truncated = all.len() > max;
            let page = &all[..all.len().min(max)];
            let parts: String = page
                .iter()
                .map(|(n, d)| format!("<Part><PartNumber>{}</PartNumber><LastModified>{}</LastModified><ETag>\"{}\"</ETag><Size>{}</Size></Part>", n, iso8601_millis(now_millis()), md5_hex(d), d.len()))
                .collect();
            let next = page.last().map(|(n, _)| **n).unwrap_or(marker);
            Response::xml(
                200,
                format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListPartsResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Bucket>{}</Bucket><Key>{}</Key><UploadId>{}</UploadId><StorageClass>STANDARD</StorageClass><PartNumberMarker>{}</PartNumberMarker><NextPartNumberMarker>{}</NextPartNumberMarker><MaxParts>{}</MaxParts><IsTruncated>{}</IsTruncated>{}</ListPartsResult>",
                    xml_escape(bucket),
                    xml_escape(key),
                    upload_id,
                    marker,
                    next,
                    max,
                    truncated,
                    parts
                ),
            )
        }
        _ => s3_error(405, "MethodNotAllowed", "Method not allowed"),
    }
}

fn s3_list(bucket: &str, b: &Bucket, req: &ServerRequest) -> Response {
    let q = req.query_params();
    let get = |k: &str| q.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let prefix = get("prefix").unwrap_or_default();
    let delimiter = get("delimiter").filter(|d| !d.is_empty());
    let max: usize = get("max-keys").and_then(|m| m.parse().ok()).unwrap_or(1000);
    let after = get("continuation-token").or_else(|| get("start-after")).or_else(|| get("marker")).unwrap_or_default();
    let mut contents = Vec::new();
    let mut prefixes: Vec<String> = Vec::new();
    let mut truncated = false;
    for (k, o) in b.objects.range(after.clone()..).filter(|(k, _)| **k != after && k.starts_with(&prefix)) {
        if let Some(d) = &delimiter {
            if let Some(i) = k[prefix.len()..].find(d.as_str()) {
                let p = k[..prefix.len() + i + d.len()].to_string();
                if !prefixes.contains(&p) {
                    prefixes.push(p);
                }
                continue;
            }
        }
        if contents.len() >= max {
            truncated = true;
            break;
        }
        contents.push((k.clone(), o));
    }
    let mut xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Name>{}</Name><Prefix>{}</Prefix><KeyCount>{}</KeyCount><MaxKeys>{}</MaxKeys><IsTruncated>{}</IsTruncated>",
        xml_escape(bucket),
        xml_escape(&prefix),
        contents.len(),
        max,
        truncated
    );
    if truncated {
        if let Some((k, _)) = contents.last() {
            xml.push_str(&format!("<NextContinuationToken>{}</NextContinuationToken>", xml_escape(k)));
        }
    }
    for (k, o) in &contents {
        xml.push_str(&format!(
            "<Contents><Key>{}</Key><LastModified>{}</LastModified><ETag>{}</ETag><Size>{}</Size><StorageClass>{}</StorageClass></Contents>",
            xml_escape(k),
            iso8601_millis(o.last_modified * 1000),
            xml_escape(&o.etag),
            o.data.len(),
            o.storage_class
        ));
    }
    for p in prefixes {
        xml.push_str(&format!("<CommonPrefixes><Prefix>{}</Prefix></CommonPrefixes>", xml_escape(&p)));
    }
    xml.push_str("</ListBucketResult>");
    Response::xml(200, xml)
}

// ---------------------------------------------------------------------------
// SQS (JSON protocol)
// ---------------------------------------------------------------------------

pub(super) fn sqs_err(code: &str, msg: &str) -> Response {
    json_error(400, "com.amazonaws.sqs", code, msg).with_header("x-amzn-query-error", format!("{};Sender", sqs_query_code(code)))
}

/// The legacy Query error code SQS reports in `x-amzn-query-error` for a JSON error type
/// (what AWS SDKs with the `awsQueryCompatible` trait expose as the error code).
pub(super) fn sqs_query_code(code: &str) -> String {
    match code {
        "QueueDoesNotExist" => "AWS.SimpleQueueService.NonExistentQueue".into(),
        "QueueNameExists" => "QueueAlreadyExists".into(),
        "QueueDeletedRecently" | "PurgeQueueInProgress" | "MessageNotInflight" | "BatchEntryIdsNotDistinct" | "EmptyBatchRequest" | "TooManyEntriesInBatchRequest" | "BatchRequestTooLong"
        | "InvalidBatchEntryId" | "UnsupportedOperation" => format!("AWS.SimpleQueueService.{}", code),
        other => other.to_string(),
    }
}

fn queue_name_of(url: &str) -> String {
    url.trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string()
}

fn sqs(st: &Shared, req: &ServerRequest) -> Response {
    let action = req.header("X-Amz-Target").and_then(|t| t.strip_prefix("AmazonSQS.")).unwrap_or("").to_string();
    let body = match parse_json(&req.body) {
        Ok(b) => b,
        Err(e) => return sqs_err("InvalidParameterValue", &e.to_string()),
    };
    let s_ = |k: &str| body.get(k).map(|v| v.to_string()).unwrap_or_default();
    let host = req.header("Host").unwrap_or("localhost");
    let url_of = |name: &str| format!("http://{}/{}/{}", host, ACCOUNT, name);
    let (lock, cv) = &**st;
    let mut s = lock.lock().unwrap();
    let ok = |v: Value| Response::json(200, v.to_json()).with_header("Content-Type", "application/x-amz-json-1.0");
    match action.as_str() {
        "CreateQueue" => {
            let name = s_("QueueName");
            let q = s.queues.entry(name.clone()).or_default();
            if let Some(Value::Object(a)) = body.get("Attributes") {
                for (k, v) in a {
                    q.attributes.insert(k.clone(), v.to_string());
                }
            }
            ok(obj([("QueueUrl", Value::from(url_of(&name)))]))
        }
        "GetQueueUrl" => {
            let name = s_("QueueName");
            if s.queues.contains_key(&name) {
                ok(obj([("QueueUrl", Value::from(url_of(&name)))]))
            } else {
                sqs_err("QueueDoesNotExist", "The specified queue does not exist.")
            }
        }
        "ListQueues" => {
            let prefix = s_("QueueNamePrefix");
            let urls: Vec<Value> = s.queues.keys().filter(|n| n.starts_with(&prefix)).map(|n| Value::from(url_of(n))).collect();
            ok(obj([("QueueUrls", Value::Array(urls))]))
        }
        _ => {
            let name = queue_name_of(&s_("QueueUrl"));
            let Some(q) = s.queues.get_mut(&name) else { return sqs_err("QueueDoesNotExist", "The specified queue does not exist.") };
            match action.as_str() {
                "SendMessage" | "SendMessageBatch" => {
                    let fifo = name.ends_with(".fifo") || q.attributes.get("FifoQueue").is_some_and(|v| v == "true");
                    let content_dedup = q.attributes.get("ContentBasedDeduplication").is_some_and(|v| v == "true");
                    let entries: Vec<Value> = if action == "SendMessage" { vec![body.clone()] } else { body.get("Entries").and_then(|e| e.as_array()).cloned().unwrap_or_default() };
                    let mut out = Vec::new();
                    let mut failed = Vec::new();
                    let now = now_millis();
                    q.dedup.retain(|_, (t, _)| now - *t < 5 * 60 * 1000);
                    for e in &entries {
                        let text = e.get("MessageBody").map(|b| b.to_string()).unwrap_or_default();
                        let attrs = e.get("MessageAttributes").cloned().unwrap_or(Value::Object(ObjectMap::new()));
                        let entry_id = e.get("Id").cloned().unwrap_or_default();
                        let fail = |code: &str, msg: &str| obj([("Id", entry_id.clone()), ("Code", Value::from(code)), ("Message", Value::from(msg)), ("SenderFault", Value::Bool(true))]);
                        let mut msg_id;
                        if fifo {
                            if e.get("MessageGroupId").is_none() {
                                if action == "SendMessage" {
                                    return sqs_err("MissingParameter", "The request must contain the parameter MessageGroupId.");
                                }
                                failed.push(fail("MissingParameter", "The request must contain the parameter MessageGroupId."));
                                continue;
                            }
                            let dedup_id = match e.get("MessageDeduplicationId") {
                                Some(d) => d.to_string(),
                                None if content_dedup => crate::util::sha256_hex(text.as_bytes()),
                                None => {
                                    let m = "The queue should either have ContentBasedDeduplication enabled or MessageDeduplicationId provided explicitly";
                                    if action == "SendMessage" {
                                        return sqs_err("InvalidParameterValue", m);
                                    }
                                    failed.push(fail("InvalidParameterValue", m));
                                    continue;
                                }
                            };
                            // a duplicate within the 5 minute deduplication interval is accepted but not enqueued
                            msg_id = q.dedup.get(&dedup_id).map(|(_, id)| id.clone());
                            if msg_id.is_none() {
                                let m = new_msg(text.clone(), attrs.clone(), 0);
                                q.dedup.insert(dedup_id, (now, m.id.clone()));
                                msg_id = Some(m.id.clone());
                                q.messages.push(m);
                            }
                        } else {
                            let m = new_msg(text.clone(), attrs.clone(), e.get("DelaySeconds").and_then(|d| d.as_i64()).unwrap_or(0));
                            msg_id = Some(m.id.clone());
                            q.messages.push(m);
                        }
                        let mut r = obj([("MessageId", Value::from(msg_id.unwrap_or_default())), ("MD5OfMessageBody", Value::from(md5_hex(text.as_bytes())))]);
                        if let (Some(md5), Value::Object(o)) = (message_attributes_md5(&attrs), &mut r) {
                            o.insert("MD5OfMessageAttributes".into(), Value::from(md5));
                        }
                        if fifo {
                            if let Value::Object(o) = &mut r {
                                o.insert("SequenceNumber".into(), Value::from(format!("{:020}", now)));
                            }
                        }
                        if action == "SendMessageBatch" {
                            if let Value::Object(o) = &mut r {
                                o.insert("Id".into(), entry_id.clone());
                            }
                        }
                        out.push(r);
                    }
                    cv.notify_all();
                    if action == "SendMessage" {
                        ok(out.pop().unwrap_or_default())
                    } else {
                        ok(obj([("Successful", Value::Array(out)), ("Failed", Value::Array(failed))]))
                    }
                }
                "ReceiveMessage" => {
                    let max = body.get("MaxNumberOfMessages").and_then(|m| m.as_i64()).unwrap_or(1).clamp(1, 10) as usize;
                    let wait = body.get("WaitTimeSeconds").and_then(|m| m.as_i64()).unwrap_or(0).clamp(0, 20);
                    let deadline = Instant::now() + Duration::from_secs(wait as u64);
                    let names: Vec<String> = body.get("MessageAttributeNames").and_then(|a| a.as_array()).map(|a| a.iter().map(|v| v.to_string()).collect()).unwrap_or_default();
                    drop(s);
                    let mut s = lock.lock().unwrap();
                    loop {
                        let now = now_millis();
                        let Some(q) = s.queues.get_mut(&name) else { return sqs_err("QueueDoesNotExist", "The specified queue does not exist.") };
                        let vis = body.get("VisibilityTimeout").and_then(|v| v.as_i64()).or_else(|| q.attributes.get("VisibilityTimeout").and_then(|v| v.parse().ok())).unwrap_or(30);
                        let mut out = Vec::new();
                        for m in q.messages.iter_mut().filter(|m| m.visible_at <= now).take(max) {
                            m.visible_at = now + vis * 1000;
                            m.receive_count += 1;
                            let handle = format!("{}#{}", m.id, uuid());
                            m.receipt = Some(handle.clone());
                            let attrs: ObjectMap = m
                                .attributes
                                .as_object()
                                .map(|a| a.iter().filter(|(k, _)| names.iter().any(|n| n == "All" || n == ".*" || n == *k)).map(|(k, v)| (k.clone(), v.clone())).collect())
                                .unwrap_or_default();
                            out.push(obj([
                                ("MessageId", Value::from(m.id.clone())),
                                ("ReceiptHandle", Value::from(handle)),
                                ("MD5OfBody", Value::from(md5_hex(m.body.as_bytes()))),
                                ("Body", Value::from(m.body.clone())),
                                (
                                    "Attributes",
                                    obj([
                                        ("SentTimestamp", Value::from(m.sent.to_string())),
                                        ("ApproximateReceiveCount", Value::from(m.receive_count.to_string())),
                                    ]),
                                ),
                                ("MessageAttributes", Value::Object(attrs)),
                            ]));
                        }
                        if !out.is_empty() || Instant::now() >= deadline {
                            return ok(if out.is_empty() { obj([]) } else { obj([("Messages", Value::Array(out))]) });
                        }
                        let left = deadline.saturating_duration_since(Instant::now()).min(Duration::from_millis(50));
                        s = cv.wait_timeout(s, left).unwrap().0;
                    }
                }
                "DeleteMessage" => {
                    let handle = s_("ReceiptHandle");
                    let before = q.messages.len();
                    q.messages.retain(|m| m.receipt.as_deref() != Some(handle.as_str()));
                    if q.messages.len() == before {
                        return sqs_err("ReceiptHandleIsInvalid", "The input receipt handle is invalid.");
                    }
                    ok(obj([]))
                }
                "ChangeMessageVisibility" => {
                    let handle = s_("ReceiptHandle");
                    s.visibility_changes += 1;
                    let Some(q) = s.queues.get_mut(&name) else { return sqs_err("QueueDoesNotExist", "The specified queue does not exist.") };
                    let secs = body.get("VisibilityTimeout").and_then(|v| v.as_i64()).unwrap_or(0);
                    match q.messages.iter_mut().find(|m| m.receipt.as_deref() == Some(handle.as_str())) {
                        Some(m) => {
                            m.visible_at = now_millis() + secs * 1000;
                            cv.notify_all();
                            ok(obj([]))
                        }
                        None => sqs_err("ReceiptHandleIsInvalid", "The input receipt handle is invalid."),
                    }
                }
                "PurgeQueue" => {
                    q.messages.clear();
                    ok(obj([]))
                }
                "DeleteQueue" => {
                    s.queues.remove(&name);
                    ok(obj([]))
                }
                other => sqs_err("InvalidAction", &format!("Unsupported action {}", other)),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// SNS (Query protocol)
// ---------------------------------------------------------------------------

fn sns(st: &Shared, req: &ServerRequest) -> Response {
    let p = parse_form(&req.text());
    let get = |k: &str| p.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let action = get("Action").unwrap_or_default();
    let mut s = st.0.lock().unwrap();
    let wrap = |name: &str, inner: String| {
        Response::xml(
            200,
            format!(
                "<{0}Response xmlns=\"http://sns.amazonaws.com/doc/2010-03-31/\"><{0}Result>{1}</{0}Result><ResponseMetadata><RequestId>{2}</RequestId></ResponseMetadata></{0}Response>",
                name,
                inner,
                request_id()
            ),
        )
    };
    match action.as_str() {
        "CreateTopic" => {
            let name = get("Name").unwrap_or_default();
            let arn = format!("arn:aws:sns:{}:{}:{}", s.region, ACCOUNT, name);
            s.topics.entry(arn.clone()).or_default();
            wrap("CreateTopic", format!("<TopicArn>{}</TopicArn>", xml_escape(&arn)))
        }
        "ListTopics" => {
            let members: String = s.topics.keys().map(|a| format!("<member><TopicArn>{}</TopicArn></member>", xml_escape(a))).collect();
            wrap("ListTopics", format!("<Topics>{}</Topics>", members))
        }
        "DeleteTopic" => {
            s.topics.remove(&get("TopicArn").unwrap_or_default());
            wrap("DeleteTopic", String::new())
        }
        "Publish" => {
            let arn = get("TopicArn").unwrap_or_default();
            let Some(topic) = s.topics.get_mut(&arn) else { return query_error(404, "NotFound", "Topic does not exist") };
            let mut attributes = BTreeMap::new();
            let mut n = 1;
            while let Some(name) = get(&format!("MessageAttributes.entry.{}.Name", n)) {
                attributes.insert(name, get(&format!("MessageAttributes.entry.{}.Value.StringValue", n)).unwrap_or_default());
                n += 1;
            }
            let structure = get("MessageStructure");
            let message = get("Message").unwrap_or_default();
            if structure.as_deref() == Some("json") && parse_json(message.as_bytes()).ok().and_then(|v| v.get("default").cloned()).is_none() {
                return query_error(400, "InvalidParameter", "Invalid parameter: Message Structure - No default entry in JSON message body");
            }
            let id = uuid();
            topic.push(PublishedMessage { message_id: id.clone(), message, subject: get("Subject"), message_structure: structure, attributes, group_id: get("MessageGroupId") });
            wrap("Publish", format!("<MessageId>{}</MessageId>", id))
        }
        other => query_error(400, "InvalidAction", &format!("Unsupported action {}", other)),
    }
}

// ---------------------------------------------------------------------------
// DynamoDB
// ---------------------------------------------------------------------------

fn ddb_err(code: &str, msg: &str) -> Response {
    json_error(400, "com.amazonaws.dynamodb.v20120810", code, msg)
}

fn cmp_av(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    let (ta, va) = a.as_object()?.iter().next()?;
    let (tb, vb) = b.as_object()?.iter().next()?;
    if ta != tb {
        return None;
    }
    match ta.as_str() {
        "N" => va.to_string().parse::<f64>().ok()?.partial_cmp(&vb.to_string().parse::<f64>().ok()?),
        _ => Some(va.to_string().cmp(&vb.to_string())),
    }
}

fn condition_matches(item: &ObjectMap, attr: &str, cond: &Value) -> bool {
    let op = cond.get("ComparisonOperator").map(|o| o.to_string()).unwrap_or_else(|| "EQ".into());
    let list = cond.get("AttributeValueList").and_then(|l| l.as_array()).cloned().unwrap_or_default();
    let v = item.get(attr);
    use std::cmp::Ordering::*;
    let c = |i: usize| v.and_then(|v| list.get(i).and_then(|x| cmp_av(v, x)));
    match op.as_str() {
        "EQ" => c(0) == Some(Equal),
        "NE" => c(0) != Some(Equal),
        "LT" => c(0) == Some(Less),
        "LE" => matches!(c(0), Some(Less | Equal)),
        "GT" => c(0) == Some(Greater),
        "GE" => matches!(c(0), Some(Greater | Equal)),
        "BETWEEN" => matches!(c(0), Some(Greater | Equal)) && matches!(c(1), Some(Less | Equal)),
        "BEGINS_WITH" => match (v.map(from_attribute_value), list.first().map(from_attribute_value)) {
            (Some(Value::String(a)), Some(Value::String(b))) => a.starts_with(&b),
            _ => false,
        },
        "CONTAINS" => match (v.map(from_attribute_value), list.first().map(from_attribute_value)) {
            (Some(Value::String(a)), Some(Value::String(b))) => a.contains(&b),
            (Some(Value::Array(a)), Some(b)) => a.contains(&b),
            _ => false,
        },
        "NOT_NULL" => v.is_some(),
        "NULL" => v.is_none(),
        "IN" => (0..list.len()).any(|i| c(i) == Some(Equal)),
        _ => false,
    }
}

fn item_matches_key(t: &Table, item: &ObjectMap, key: &ObjectMap) -> bool {
    let eq = |a: &str| match (item.get(a), key.get(a)) {
        (Some(x), Some(y)) => cmp_av(x, y) == Some(std::cmp::Ordering::Equal),
        _ => false,
    };
    eq(&t.hash) && t.range.as_ref().is_none_or(|r| eq(r))
}

fn ddb(st: &Shared, req: &ServerRequest) -> Response {
    let action = req.header("X-Amz-Target").and_then(|t| t.strip_prefix("DynamoDB_20120810.")).unwrap_or("").to_string();
    let body = match parse_json(&req.body) {
        Ok(b) => b,
        Err(e) => return ddb_err("SerializationException", &e.to_string()),
    };
    let ok = |v: Value| Response::json(200, v.to_json()).with_header("Content-Type", "application/x-amz-json-1.0");
    let table_name = body.get("TableName").map(|t| t.to_string()).unwrap_or_default();
    let mut s = st.0.lock().unwrap();
    let describe = |t: &Table, name: &str| {
        obj([
            ("TableName", Value::from(name)),
            ("TableStatus", Value::from("ACTIVE")),
            ("KeySchema", t.key_schema.clone()),
            ("AttributeDefinitions", t.attribute_definitions.clone()),
            ("ItemCount", Value::Int(t.items.len() as i64)),
        ])
    };
    if action == "ListTables" {
        let start = body.get("ExclusiveStartTableName").map(|t| t.to_string());
        let limit = body.get("Limit").and_then(|l| l.as_i64()).unwrap_or(100).clamp(1, 100) as usize;
        let mut names: Vec<&String> = s.tables.keys().filter(|n| start.as_ref().is_none_or(|st| *n > st)).collect();
        names.sort();
        let more = names.len() > limit;
        names.truncate(limit);
        let mut v = obj([("TableNames", Value::Array(names.iter().map(|n| Value::from(n.as_str())).collect()))]);
        if let (true, Some(last), Value::Object(o)) = (more, names.last(), &mut v) {
            o.insert("LastEvaluatedTableName".into(), Value::from(last.as_str()));
        }
        return ok(v);
    }
    if action == "CreateTable" {
        if s.tables.contains_key(&table_name) {
            return ddb_err("ResourceInUseException", "Table already exists");
        }
        let ks = body.get("KeySchema").cloned().unwrap_or(Value::Array(vec![]));
        let find = |kt: &str| ks.as_array().and_then(|a| a.iter().find(|k| k.get("KeyType").map(|t| t.to_string()).as_deref() == Some(kt))).and_then(|k| k.get("AttributeName")).map(|n| n.to_string());
        let Some(hash) = find("HASH") else { return ddb_err("ValidationException", "No HASH key") };
        let t = Table { hash, range: find("RANGE"), key_schema: ks.clone(), attribute_definitions: body.get("AttributeDefinitions").cloned().unwrap_or_default(), items: vec![] };
        let d = describe(&t, &table_name);
        s.tables.insert(table_name, t);
        return ok(obj([("TableDescription", d)]));
    }
    let Some(t) = s.tables.get_mut(&table_name) else { return ddb_err("ResourceNotFoundException", "Requested resource not found") };
    let map_of = |k: &str| body.get(k).and_then(|v| v.as_object()).cloned().unwrap_or_default();
    let return_values = body.get("ReturnValues").map(|r| r.to_string()).unwrap_or_else(|| "NONE".into());
    match action.as_str() {
        "DescribeTable" => ok(obj([("Table", describe(t, &table_name))])),
        "DeleteTable" => {
            let d = describe(t, &table_name);
            s.tables.remove(&table_name);
            ok(obj([("TableDescription", d)]))
        }
        "PutItem" => {
            let item = map_of("Item");
            if !item.contains_key(&t.hash) {
                return ddb_err("ValidationException", "One of the required keys was not given a value");
            }
            let pos = t.items.iter().position(|i| item_matches_key(t, i, &item));
            let old = pos.map(|p| t.items.remove(p));
            t.items.push(item);
            match (return_values.as_str(), old) {
                ("ALL_OLD", Some(o)) => ok(obj([("Attributes", Value::Object(o))])),
                _ => ok(obj([])),
            }
        }
        "GetItem" => {
            let key = map_of("Key");
            match t.items.iter().find(|i| item_matches_key(t, i, &key)) {
                Some(i) => {
                    let item = match body.get("AttributesToGet").and_then(|a| a.as_array()) {
                        Some(names) => i.iter().filter(|(k, _)| names.iter().any(|n| n.to_string() == **k)).map(|(k, v)| (k.clone(), v.clone())).collect(),
                        None => i.clone(),
                    };
                    ok(obj([("Item", Value::Object(item))]))
                }
                None => ok(obj([])),
            }
        }
        "DeleteItem" => {
            let key = map_of("Key");
            let old = t.items.iter().position(|i| item_matches_key(t, i, &key)).map(|p| t.items.remove(p));
            match (return_values.as_str(), old) {
                ("ALL_OLD", Some(o)) => ok(obj([("Attributes", Value::Object(o))])),
                _ => ok(obj([])),
            }
        }
        "UpdateItem" => {
            let key = map_of("Key");
            let pos = match t.items.iter().position(|i| item_matches_key(t, i, &key)) {
                Some(p) => p,
                None => {
                    t.items.push(key.clone());
                    t.items.len() - 1
                }
            };
            let old = t.items[pos].clone();
            let mut updated = ObjectMap::new();
            for (attr, u) in map_of("AttributeUpdates") {
                match u.get("Action").map(|a| a.to_string()).as_deref().unwrap_or("PUT") {
                    "DELETE" => {
                        t.items[pos].remove(&attr);
                    }
                    "ADD" => {
                        let add = u.get("Value").cloned().unwrap_or_default();
                        let cur = t.items[pos].get(&attr).map(from_attribute_value).and_then(|v| v.as_f64()).unwrap_or(0.0);
                        let inc = from_attribute_value(&add).as_f64().unwrap_or(0.0);
                        let n = cur + inc;
                        let v = if n.fract() == 0.0 { Value::Int(n as i64) } else { Value::Float(n) };
                        let av = crate::aws::ddb::to_attribute_value(&v);
                        t.items[pos].insert(attr.clone(), av.clone());
                        updated.insert(attr, av);
                    }
                    _ => {
                        let v = u.get("Value").cloned().unwrap_or_default();
                        t.items[pos].insert(attr.clone(), v.clone());
                        updated.insert(attr, v);
                    }
                }
            }
            match return_values.as_str() {
                "ALL_NEW" => ok(obj([("Attributes", Value::Object(t.items[pos].clone()))])),
                "ALL_OLD" => ok(obj([("Attributes", Value::Object(old))])),
                "UPDATED_NEW" => ok(obj([("Attributes", Value::Object(updated))])),
                _ => ok(obj([])),
            }
        }
        "Query" | "Scan" => {
            let conds = if action == "Query" { map_of("KeyConditions") } else { map_of("ScanFilter") };
            if action == "Query" && !conds.contains_key(&t.hash) {
                return ddb_err("ValidationException", "Query condition missed key schema element");
            }
            let mut items: Vec<ObjectMap> = t.items.iter().filter(|i| conds.iter().all(|(a, c)| condition_matches(i, a, c))).cloned().collect();
            if let Some(r) = &t.range {
                items.sort_by(|a, b| match (a.get(r), b.get(r)) {
                    (Some(x), Some(y)) => cmp_av(x, y).unwrap_or(std::cmp::Ordering::Equal),
                    _ => std::cmp::Ordering::Equal,
                });
            }
            if body.get("ScanIndexForward").and_then(|f| f.as_bool()) == Some(false) {
                items.reverse();
            }
            let scanned = if action == "Scan" { t.items.len() } else { items.len() };
            let mut last = None;
            if let Some(l) = body.get("Limit").and_then(|l| l.as_i64()) {
                if items.len() > l as usize {
                    items.truncate(l as usize);
                    let key: ObjectMap =
                        items.last().unwrap().iter().filter(|(k, _)| **k == t.hash || Some(*k) == t.range.as_ref()).map(|(k, v)| (k.clone(), v.clone())).collect();
                    last = Some(Value::Object(key));
                }
            }
            let mut out = ObjectMap::new();
            out.insert("Count".to_string(), Value::Int(items.len() as i64));
            out.insert("ScannedCount".to_string(), Value::Int(scanned as i64));
            out.insert("Items".to_string(), Value::Array(items.into_iter().map(Value::Object).collect()));
            if let Some(l) = last {
                out.insert("LastEvaluatedKey".to_string(), l);
            }
            ok(Value::Object(out))
        }
        other => ddb_err("UnknownOperationException", &format!("Unsupported operation {}", other)),
    }
}

/// Marshal helper for tests seeding DynamoDB data through the API shape.
pub fn marshal(v: &Value) -> Value {
    marshal_item(v).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Lambda
// ---------------------------------------------------------------------------

fn lambda_err(status: u16, code: &str, msg: &str) -> Response {
    Response::json(status, obj([("Type", Value::from("User")), ("message", Value::from(msg))]).to_json()).with_header("x-amzn-ErrorType", format!("{}:", code))
}

fn lambda(st: &Shared, req: &ServerRequest) -> Response {
    let segs = req.segments();
    if segs.len() < 2 || segs[0] != "2015-03-31" || segs[1] != "functions" {
        return lambda_err(404, "ResourceNotFoundException", "Unknown resource");
    }
    let s = st.0.lock().unwrap();
    let region = s.region.clone();
    let config = |name: &str| {
        obj([
            ("FunctionName", Value::from(name)),
            ("FunctionArn", Value::from(format!("arn:aws:lambda:{}:{}:function:{}", region, ACCOUNT, name))),
            ("Runtime", Value::from("provided.al2")),
            ("Handler", Value::from("mock")),
        ])
    };
    if segs.len() == 2 {
        let fs: Vec<Value> = s.functions.keys().map(|n| config(n)).collect();
        return Response::json(200, obj([("Functions", Value::Array(fs))]).to_json());
    }
    let name = segs[2].rsplit(':').next().unwrap_or(&segs[2]).to_string();
    let name = if segs[2].contains(":function:") { segs[2].split(":function:").nth(1).unwrap_or(&name).split(':').next().unwrap_or("").to_string() } else { name };
    let Some(f) = s.functions.get(&name).cloned() else {
        return lambda_err(404, "ResourceNotFoundException", &format!("Function not found: arn:aws:lambda:{}:{}:function:{}", region, ACCOUNT, name));
    };
    drop(s);
    match (req.method.as_str(), segs.get(3).map(|s| s.as_str())) {
        ("POST", Some("invocations")) => {
            let it = req.header("X-Amz-Invocation-Type").unwrap_or("RequestResponse");
            if it == "DryRun" {
                return Response::new(204);
            }
            let result = f(&req.body);
            if it == "Event" {
                return Response::new(202);
            }
            match result {
                Ok(out) => Response::new(200).with_header("X-Amz-Executed-Version", "$LATEST").with_header("Content-Type", "application/json").with_body(out),
                Err(msg) => Response::json(200, obj([("errorMessage", Value::from(msg)), ("errorType", Value::from("Error"))]).to_json()).with_header("X-Amz-Function-Error", "Unhandled"),
            }
        }
        ("GET", None) => Response::json(200, obj([("Configuration", config(&name))]).to_json()),
        ("DELETE", None) => {
            st.0.lock().unwrap().functions.remove(&name);
            Response::new(204)
        }
        _ => lambda_err(400, "InvalidRequestContentException", "Unsupported request"),
    }
}

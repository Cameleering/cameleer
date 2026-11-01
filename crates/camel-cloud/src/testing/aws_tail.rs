//! Tail of the AWS emulator (on the [`AwsMock`] port, SigV4 verified by the parent handler):
//!
//! * SQS **Query protocol** (`Action=...` form requests, XML responses, translated onto the JSON
//!   emulation) and a Query-only mode rejecting the JSON protocol like older SQS-compatible
//!   servers ([`AwsMock::set_sqs_query_only`]).
//! * Lambda function management: CreateFunction, UpdateFunctionCode, versions, aliases, event
//!   source mappings and tags (created functions echo their payload when invoked unless a
//!   closure is registered).
//! * DynamoDB expressions (`UpdateExpression`, `KeyConditionExpression`, `FilterExpression`,
//!   `ConditionExpression`, `ProjectionExpression` with `#name` / `:value` placeholders),
//!   BatchGetItem, UpdateTable (throughput, stream specification) and **DynamoDB Streams**
//!   (DescribeStream / GetShardIterator / GetRecords over the item changes).
//! * CloudTrail events for Secrets Manager writes (PutSecretValue, UpdateSecret, RotateSecret,
//!   with `requestParameters.secretId`), as the real service records them.

use std::collections::BTreeMap;

use camel_core::value::{ObjectMap, Value};

use super::{cmp_av, item_matches_key, json_error, request_id, AwsMock, Shared, ACCOUNT};
use crate::http::{Response, ServerRequest};
use crate::util::{base64_decode, base64_encode, now_millis, now_secs, obj, parse_form, parse_json, sha256, uuid, xml_escape};

struct LambdaFunction {
    config: ObjectMap,
    versions: Vec<ObjectMap>,
    aliases: BTreeMap<String, ObjectMap>,
}

struct DdbStream {
    arn: String,
    view_type: String,
    enabled: bool,
    records: Vec<Value>,
}

#[derive(Default)]
pub(super) struct TailState {
    sqs_query_only: bool,
    sqs_query_requests: usize,
    lambda: BTreeMap<String, LambdaFunction>,
    mappings: BTreeMap<String, Value>,
    tags: BTreeMap<String, BTreeMap<String, String>>,
    streams: BTreeMap<String, DdbStream>,
    throughput: BTreeMap<String, (i64, i64)>,
    stream_seq: u64,
}

impl AwsMock {
    /// Reject the SQS JSON protocol (`InvalidAction`), accepting only Query requests.
    pub fn set_sqs_query_only(&self, on: bool) {
        self.lock().tail.sqs_query_only = on;
    }
    /// Number of SQS Query protocol requests served.
    pub fn sqs_query_requests(&self) -> usize {
        self.lock().tail.sqs_query_requests
    }
    /// Enable a DynamoDB stream on a table (`NEW_AND_OLD_IMAGES`, ...); returns the stream ARN.
    pub fn enable_ddb_stream(&self, table: &str, view_type: &str) -> String {
        enable_stream(&mut self.lock().tail, table, view_type)
    }
    /// Records of a table's stream.
    pub fn ddb_stream_records(&self, table: &str) -> Vec<Value> {
        self.lock().tail.streams.get(table).map(|s| s.records.clone()).unwrap_or_default()
    }
    /// Event source mappings (UUID -> mapping).
    pub fn lambda_event_source_mappings(&self) -> Vec<Value> {
        self.lock().tail.mappings.values().cloned().collect()
    }
}

fn enable_stream(t: &mut TailState, table: &str, view_type: &str) -> String {
    let arn = format!("arn:aws:dynamodb:us-east-1:{}:table/{}/stream/{}", ACCOUNT, table, crate::util::iso8601_millis(now_millis()));
    let s = t.streams.entry(table.to_string()).or_insert(DdbStream { arn: arn.clone(), view_type: view_type.to_string(), enabled: true, records: Vec::new() });
    s.enabled = true;
    s.view_type = view_type.to_string();
    s.arn.clone()
}

pub(super) fn handle(st: &Shared, service: &str, req: &ServerRequest) -> Option<Response> {
    match service {
        "sqs" => sqs(st, req),
        "lambda" => lambda(st, req),
        "dynamodb" => dynamodb(st, req),
        "secretsmanager" => secrets(st, req),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// SQS Query protocol
// ---------------------------------------------------------------------------

const SQS_NS: &str = "http://queue.amazonaws.com/doc/2012-11-05/";

fn sqs_query_error(status: u16, code: &str, msg: &str) -> Response {
    Response::xml(
        status,
        format!("<ErrorResponse xmlns=\"{}\"><Error><Type>Sender</Type><Code>{}</Code><Message>{}</Message><Detail/></Error><RequestId>{}</RequestId></ErrorResponse>", SQS_NS, code, xml_escape(msg), request_id()),
    )
}

/// Query parameters -> SQS JSON request.
fn query_to_json(params: &[(String, String)]) -> Value {
    let int_fields = ["MaxNumberOfMessages", "WaitTimeSeconds", "VisibilityTimeout", "DelaySeconds"];
    let mut out = ObjectMap::new();
    let mut attrs: BTreeMap<usize, (String, String)> = BTreeMap::new();
    let mut mattrs: BTreeMap<usize, ObjectMap> = BTreeMap::new();
    let mut entries: BTreeMap<usize, ObjectMap> = BTreeMap::new();
    let mut names: Vec<(usize, String)> = Vec::new();
    let mut mnames: Vec<(usize, String)> = Vec::new();
    for (k, v) in params {
        let parts: Vec<&str> = k.split('.').collect();
        let idx = |i: usize| parts.get(i).and_then(|p| p.parse::<usize>().ok()).unwrap_or(0);
        match parts[0] {
            "Action" | "Version" => {}
            "Attribute" if parts.len() == 3 => {
                let e = attrs.entry(idx(1)).or_default();
                if parts[2] == "Name" {
                    e.0 = v.clone();
                } else {
                    e.1 = v.clone();
                }
            }
            "MessageAttribute" => {
                let e = mattrs.entry(idx(1)).or_default();
                if parts.get(2) == Some(&"Name") {
                    e.insert("@name".into(), Value::from(v.clone()));
                } else if let Some(f) = parts.get(3) {
                    e.insert(f.to_string(), Value::from(v.clone()));
                }
            }
            "AttributeName" => names.push((idx(1), v.clone())),
            "MessageAttributeName" => mnames.push((idx(1), v.clone())),
            "SendMessageBatchRequestEntry" | "DeleteMessageBatchRequestEntry" | "ChangeMessageVisibilityBatchRequestEntry" if parts.len() >= 3 => {
                let e = entries.entry(idx(1)).or_default();
                let f = parts[2];
                e.insert(f.to_string(), if int_fields.contains(&f) { v.parse::<i64>().map(Value::Int).unwrap_or_else(|_| Value::from(v.clone())) } else { Value::from(v.clone()) });
            }
            f if parts.len() == 1 => {
                out.insert(f.to_string(), if int_fields.contains(&f) { v.parse::<i64>().map(Value::Int).unwrap_or_else(|_| Value::from(v.clone())) } else { Value::from(v.clone()) });
            }
            _ => {}
        }
    }
    if !attrs.is_empty() {
        out.insert("Attributes".into(), Value::Object(attrs.into_values().map(|(n, v)| (n, Value::from(v))).collect()));
    }
    if !mattrs.is_empty() {
        let m: ObjectMap = mattrs
            .into_values()
            .map(|mut e| {
                let n = e.remove("@name").map(|n| n.to_string()).unwrap_or_default();
                (n, Value::Object(e))
            })
            .collect();
        out.insert("MessageAttributes".into(), Value::Object(m));
    }
    if !names.is_empty() {
        names.sort();
        out.insert("AttributeNames".into(), Value::Array(names.into_iter().map(|(_, n)| Value::from(n)).collect()));
    }
    if !mnames.is_empty() {
        mnames.sort();
        out.insert("MessageAttributeNames".into(), Value::Array(mnames.into_iter().map(|(_, n)| Value::from(n)).collect()));
    }
    if !entries.is_empty() {
        out.insert("Entries".into(), Value::Array(entries.into_values().map(Value::Object).collect()));
    }
    Value::Object(out)
}

fn xml_scalars(m: &ObjectMap, out: &mut String) {
    for (k, v) in m {
        if !matches!(v, Value::Object(_) | Value::Array(_)) {
            out.push_str(&format!("<{0}>{1}</{0}>", k, xml_escape(&v.to_string())));
        }
    }
}

fn xml_attributes(m: Option<&Value>, out: &mut String) {
    for (k, v) in m.and_then(|a| a.as_object()).into_iter().flatten() {
        out.push_str(&format!("<Attribute><Name>{}</Name><Value>{}</Value></Attribute>", xml_escape(k), xml_escape(&v.to_string())));
    }
}

/// SQS JSON response -> Query XML.
fn json_to_query_xml(action: &str, r: &Value) -> String {
    let m = r.as_object().cloned().unwrap_or_default();
    let mut body = String::new();
    match action {
        "ListQueues" => {
            for u in m.get("QueueUrls").and_then(|u| u.as_array()).into_iter().flatten() {
                body.push_str(&format!("<QueueUrl>{}</QueueUrl>", xml_escape(&u.to_string())));
            }
        }
        "ReceiveMessage" => {
            for msg in m.get("Messages").and_then(|u| u.as_array()).into_iter().flatten() {
                let mm = msg.as_object().cloned().unwrap_or_default();
                body.push_str("<Message>");
                xml_scalars(&mm, &mut body);
                xml_attributes(mm.get("Attributes"), &mut body);
                for (n, a) in mm.get("MessageAttributes").and_then(|a| a.as_object()).into_iter().flatten() {
                    body.push_str(&format!("<MessageAttribute><Name>{}</Name><Value>", xml_escape(n)));
                    if let Some(am) = a.as_object() {
                        xml_scalars(am, &mut body);
                    }
                    body.push_str("</Value></MessageAttribute>");
                }
                body.push_str("</Message>");
            }
        }
        "SendMessageBatch" | "DeleteMessageBatch" => {
            let entry = if action == "SendMessageBatch" { "SendMessageBatchResultEntry" } else { "DeleteMessageBatchResultEntry" };
            for (field, tag) in [("Successful", entry), ("Failed", "BatchResultErrorEntry")] {
                for e in m.get(field).and_then(|u| u.as_array()).into_iter().flatten() {
                    body.push_str(&format!("<{}>", tag));
                    if let Some(em) = e.as_object() {
                        xml_scalars(em, &mut body);
                    }
                    body.push_str(&format!("</{}>", tag));
                }
            }
        }
        "GetQueueAttributes" => xml_attributes(m.get("Attributes"), &mut body),
        _ => xml_scalars(&m, &mut body),
    }
    format!(
        "<?xml version=\"1.0\"?><{0}Response xmlns=\"{1}\"><{0}Result>{2}</{0}Result><ResponseMetadata><RequestId>{3}</RequestId></ResponseMetadata></{0}Response>",
        action,
        SQS_NS,
        body,
        request_id()
    )
}

fn sqs(st: &Shared, req: &ServerRequest) -> Option<Response> {
    if let Some(t) = req.header("X-Amz-Target") {
        if st.0.lock().unwrap().tail.sqs_query_only {
            return Some(sqs_query_error(400, "InvalidAction", &format!("The action {} is not valid for this endpoint.", t)));
        }
        return None;
    }
    let mut params = parse_form(&req.text());
    params.extend(req.query_params());
    let action = params.iter().find(|(k, _)| k == "Action").map(|(_, v)| v.clone())?;
    st.0.lock().unwrap().tail.sqs_query_requests += 1;
    let json = query_to_json(&params);
    let inner = ServerRequest {
        method: "POST".into(),
        path: "/".into(),
        query: String::new(),
        headers: vec![("X-Amz-Target".into(), format!("AmazonSQS.{}", action)), ("Content-Type".into(), "application/x-amz-json-1.0".into())],
        body: json.to_json().into_bytes(),
    };
    let resp = super::gaps::sqs_more(st, &inner).unwrap_or_else(|| super::sqs(st, &inner));
    let v = parse_json(&resp.body).unwrap_or_default();
    if resp.status >= 300 {
        let code = v.get("__type").map(|t| t.to_string()).unwrap_or_default();
        let code = code.rsplit('#').next().unwrap_or("InternalError").to_string();
        let code = match code.as_str() {
            "QueueDoesNotExist" => "AWS.SimpleQueueService.NonExistentQueue".to_string(),
            c => c.to_string(),
        };
        return Some(sqs_query_error(resp.status, &code, &v.get("message").map(|m| m.to_string()).unwrap_or_default()));
    }
    Some(Response::xml(200, json_to_query_xml(&action, &v)))
}

// ---------------------------------------------------------------------------
// Lambda function management
// ---------------------------------------------------------------------------

fn lerr(status: u16, code: &str, msg: &str) -> Response {
    Response::json(status, obj([("Type", Value::from("User")), ("message", Value::from(msg))]).to_json()).with_header("x-amzn-ErrorType", format!("{}:", code))
}

fn function_arn(name: &str) -> String {
    format!("arn:aws:lambda:us-east-1:{}:function:{}", ACCOUNT, name)
}

fn name_of(seg: &str) -> String {
    if let Some(rest) = seg.split(":function:").nth(1) {
        rest.split(':').next().unwrap_or("").to_string()
    } else {
        seg.to_string()
    }
}

fn code_sha(code: &Value) -> Result<(String, i64), Response> {
    if let Some(z) = code.get("ZipFile") {
        let bytes = base64_decode(&z.to_string()).ok_or_else(|| lerr(400, "InvalidParameterValueException", "ZipFile is not valid base64"))?;
        if bytes.is_empty() {
            return Err(lerr(400, "InvalidParameterValueException", "Uploaded file must be a non-empty zip"));
        }
        return Ok((base64_encode(&sha256(&bytes)), bytes.len() as i64));
    }
    match (code.get("S3Bucket"), code.get("S3Key")) {
        (Some(b), Some(k)) => Ok((base64_encode(&sha256(format!("s3://{}/{}", b, k).as_bytes())), 0)),
        _ => Err(lerr(400, "InvalidParameterValueException", "Code must contain ZipFile or S3Bucket and S3Key")),
    }
}

fn publish(f: &mut LambdaFunction, description: Option<String>) -> ObjectMap {
    let n = f.versions.len() + 1;
    let mut v = f.config.clone();
    let name = v.get("FunctionName").map(|x| x.to_string()).unwrap_or_default();
    v.insert("Version".into(), Value::from(n.to_string()));
    v.insert("FunctionArn".into(), Value::from(format!("{}:{}", function_arn(&name), n)));
    if let Some(d) = description {
        v.insert("Description".into(), Value::from(d));
    }
    f.versions.push(v.clone());
    v
}

fn lambda(st: &Shared, req: &ServerRequest) -> Option<Response> {
    let segs = req.segments();
    let body = parse_json(&req.body).unwrap_or_default();
    let s_ = |k: &str| body.get(k).map(|v| v.to_string()).filter(|v| !v.is_empty());
    let mut g = st.0.lock().unwrap();
    let ok = |status: u16, v: Value| Some(Response::json(status, v.to_json()));
    let seg: Vec<&str> = segs.iter().map(|s| s.as_str()).collect();
    match (req.method.as_str(), seg.as_slice()) {
        ("POST", ["2015-03-31", "functions"]) => {
            let Some(name) = s_("FunctionName") else { return Some(lerr(400, "InvalidParameterValueException", "FunctionName is required")) };
            if g.tail.lambda.contains_key(&name) || g.functions.contains_key(&name) {
                return Some(lerr(409, "ResourceConflictException", &format!("Function already exist: {}", name)));
            }
            if s_("Role").is_none() || s_("Runtime").is_none() {
                return Some(lerr(400, "InvalidParameterValueException", "Role and Runtime are required"));
            }
            let (sha, size) = match code_sha(&body.get("Code").cloned().unwrap_or_default()) {
                Ok(x) => x,
                Err(r) => return Some(r),
            };
            let mut c = ObjectMap::new();
            c.insert("FunctionName".into(), Value::from(name.clone()));
            c.insert("FunctionArn".into(), Value::from(function_arn(&name)));
            for f in ["Runtime", "Role", "Handler", "Description", "Environment", "DeadLetterConfig", "TracingConfig", "KMSKeyArn", "VpcConfig"] {
                if let Some(v) = body.get(f) {
                    c.insert(f.into(), v.clone());
                }
            }
            c.insert("Timeout".into(), body.get("Timeout").cloned().unwrap_or(Value::Int(3)));
            c.insert("MemorySize".into(), body.get("MemorySize").cloned().unwrap_or(Value::Int(128)));
            c.insert("CodeSha256".into(), Value::from(sha));
            c.insert("CodeSize".into(), Value::Int(size));
            c.insert("Version".into(), Value::from("$LATEST"));
            c.insert("State".into(), Value::from("Active"));
            c.insert("LastModified".into(), Value::from(crate::util::iso8601_millis(now_millis())));
            let mut f = LambdaFunction { config: c.clone(), versions: Vec::new(), aliases: BTreeMap::new() };
            let mut resp = c;
            if body.get("Publish").and_then(|p| p.as_bool()) == Some(true) {
                resp = publish(&mut f, None);
            }
            if let Some(Value::Object(tags)) = body.get("Tags") {
                g.tail.tags.insert(function_arn(&name), tags.iter().map(|(k, v)| (k.clone(), v.to_string())).collect());
            }
            g.tail.lambda.insert(name.clone(), f);
            g.functions.insert(name, std::sync::Arc::new(|p: &[u8]| Ok::<Vec<u8>, String>(p.to_vec())));
            ok(201, Value::Object(resp))
        }
        ("GET", ["2015-03-31", "functions"]) => {
            let mut fs: Vec<Value> = g.tail.lambda.values().map(|f| Value::Object(f.config.clone())).collect();
            for n in g.functions.keys().filter(|n| !g.tail.lambda.contains_key(*n)) {
                fs.push(obj([("FunctionName", Value::from(n.clone())), ("FunctionArn", Value::from(function_arn(n))), ("Runtime", Value::from("provided.al2")), ("Handler", Value::from("mock"))]));
            }
            ok(200, obj([("Functions", Value::Array(fs))]))
        }
        ("POST", ["2015-03-31", "event-source-mappings"]) => {
            let (Some(src), Some(fname)) = (s_("EventSourceArn"), s_("FunctionName")) else {
                return Some(lerr(400, "InvalidParameterValueException", "EventSourceArn and FunctionName are required"));
            };
            let name = name_of(&fname);
            if !g.functions.contains_key(&name) {
                return Some(lerr(404, "ResourceNotFoundException", &format!("Function not found: {}", function_arn(&name))));
            }
            let id = uuid();
            let m = obj([
                ("UUID", Value::from(id.clone())),
                ("EventSourceArn", Value::from(src)),
                ("FunctionArn", Value::from(function_arn(&name))),
                ("BatchSize", body.get("BatchSize").cloned().unwrap_or(Value::Int(10))),
                ("StartingPosition", body.get("StartingPosition").cloned().unwrap_or(Value::Null)),
                ("State", Value::from("Enabled")),
                ("LastModified", Value::Float(now_millis() as f64 / 1000.0)),
            ]);
            g.tail.mappings.insert(id, m.clone());
            ok(202, m)
        }
        ("GET", ["2015-03-31", "event-source-mappings"]) => {
            let filter = req.query_param("FunctionName").map(|f| function_arn(&name_of(&f)));
            let ms: Vec<Value> = g.tail.mappings.values().filter(|m| filter.as_ref().is_none_or(|f| m.get("FunctionArn").map(|a| a.to_string()).as_deref() == Some(f.as_str()))).cloned().collect();
            ok(200, obj([("EventSourceMappings", Value::Array(ms))]))
        }
        ("DELETE", ["2015-03-31", "event-source-mappings", id]) => match g.tail.mappings.remove(*id) {
            Some(Value::Object(mut m)) => {
                m.insert("State".into(), Value::from("Deleting"));
                ok(202, Value::Object(m))
            }
            _ => Some(lerr(404, "ResourceNotFoundException", "The resource you requested does not exist.")),
        },
        (method, ["2017-03-31", "tags", arn]) => {
            let arn = arn.to_string();
            match method {
                "GET" => {
                    let tags: ObjectMap = g.tail.tags.get(&arn).map(|t| t.iter().map(|(k, v)| (k.clone(), Value::from(v.clone()))).collect()).unwrap_or_default();
                    ok(200, obj([("Tags", Value::Object(tags))]))
                }
                "POST" => {
                    let e = g.tail.tags.entry(arn).or_default();
                    for (k, v) in body.get("Tags").and_then(|t| t.as_object()).into_iter().flatten() {
                        e.insert(k.clone(), v.to_string());
                    }
                    Some(Response::new(204))
                }
                "DELETE" => {
                    let keys: Vec<String> = req.query_params().into_iter().filter(|(k, _)| k == "tagKeys").map(|(_, v)| v).collect();
                    if let Some(e) = g.tail.tags.get_mut(&arn) {
                        for k in keys {
                            e.remove(&k);
                        }
                    }
                    Some(Response::new(204))
                }
                _ => None,
            }
        }
        (method, ["2015-03-31", "functions", fname, rest @ ..]) => {
            let name = name_of(fname);
            if !g.tail.lambda.contains_key(&name) {
                if method == "DELETE" && rest.is_empty() {
                    return None;
                }
                if rest.first() == Some(&"invocations") || (method == "GET" && rest.is_empty()) {
                    return None;
                }
                if !g.functions.contains_key(&name) {
                    return Some(lerr(404, "ResourceNotFoundException", &format!("Function not found: {}", function_arn(&name))));
                }
                // a closure-only function gets management metadata on first use
                let c = obj([("FunctionName", Value::from(name.clone())), ("FunctionArn", Value::from(function_arn(&name))), ("Runtime", Value::from("provided.al2")), ("Handler", Value::from("mock")), ("Version", Value::from("$LATEST"))]);
                g.tail.lambda.insert(name.clone(), LambdaFunction { config: c.as_object().cloned().unwrap_or_default(), versions: Vec::new(), aliases: BTreeMap::new() });
            }
            let tags = g.tail.tags.get(&function_arn(&name)).cloned();
            let f = g.tail.lambda.get_mut(&name).unwrap();
            match (method, rest) {
                ("GET", []) => {
                    let mut out = ObjectMap::new();
                    out.insert("Configuration".into(), Value::Object(f.config.clone()));
                    out.insert("Code".into(), obj([("RepositoryType", Value::from("S3")), ("Location", Value::from(format!("https://awslambda.example/{}", name)))]));
                    if let Some(t) = tags {
                        out.insert("Tags".into(), Value::Object(t.into_iter().map(|(k, v)| (k, Value::from(v))).collect()));
                    }
                    ok(200, Value::Object(out))
                }
                ("DELETE", []) => {
                    g.tail.lambda.remove(&name);
                    g.functions.remove(&name);
                    Some(Response::new(204))
                }
                ("PUT", ["code"]) => {
                    let (sha, size) = match code_sha(&body) {
                        Ok(x) => x,
                        Err(r) => return Some(r),
                    };
                    f.config.insert("CodeSha256".into(), Value::from(sha));
                    f.config.insert("CodeSize".into(), Value::Int(size));
                    f.config.insert("LastModified".into(), Value::from(crate::util::iso8601_millis(now_millis())));
                    let resp = if body.get("Publish").and_then(|p| p.as_bool()) == Some(true) { publish(f, None) } else { f.config.clone() };
                    ok(200, Value::Object(resp))
                }
                ("POST", ["versions"]) => ok(201, Value::Object(publish(f, s_("Description")))),
                ("GET", ["versions"]) => {
                    let mut vs = vec![Value::Object(f.config.clone())];
                    vs.extend(f.versions.iter().cloned().map(Value::Object));
                    ok(200, obj([("Versions", Value::Array(vs))]))
                }
                ("POST", ["aliases"]) => {
                    let (Some(alias), Some(version)) = (s_("Name"), s_("FunctionVersion")) else {
                        return Some(lerr(400, "InvalidParameterValueException", "Name and FunctionVersion are required"));
                    };
                    if version != "$LATEST" && version.parse::<usize>().map_or(true, |v| v == 0 || v > f.versions.len()) {
                        return Some(lerr(404, "ResourceNotFoundException", &format!("Function not found: {}:{}", function_arn(&name), version)));
                    }
                    if f.aliases.contains_key(&alias) {
                        return Some(lerr(409, "ResourceConflictException", &format!("Alias already exists: {}:{}", function_arn(&name), alias)));
                    }
                    let mut a = ObjectMap::new();
                    a.insert("AliasArn".into(), Value::from(format!("{}:{}", function_arn(&name), alias)));
                    a.insert("Name".into(), Value::from(alias.clone()));
                    a.insert("FunctionVersion".into(), Value::from(version));
                    a.insert("Description".into(), Value::from(s_("Description").unwrap_or_default()));
                    a.insert("RevisionId".into(), Value::from(uuid()));
                    f.aliases.insert(alias, a.clone());
                    ok(201, Value::Object(a))
                }
                ("GET", ["aliases"]) => ok(200, obj([("Aliases", Value::Array(f.aliases.values().cloned().map(Value::Object).collect()))])),
                ("GET", ["aliases", a]) => match f.aliases.get(*a) {
                    Some(x) => ok(200, Value::Object(x.clone())),
                    None => Some(lerr(404, "ResourceNotFoundException", &format!("Alias not found: {}:{}", function_arn(&name), a))),
                },
                ("DELETE", ["aliases", a]) => {
                    f.aliases.remove(*a);
                    Some(Response::new(204))
                }
                ("POST", ["invocations"]) => None,
                _ => Some(lerr(400, "InvalidRequestContentException", "Unsupported request")),
            }
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// DynamoDB expressions
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Ident(String),
    Name(String),
    Val(String),
    Num(usize),
    Sym(&'static str),
}

fn tokenize(s: &str) -> Result<Vec<Tok>, String> {
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
            continue;
        }
        let word = |i: &mut usize| {
            let st = *i;
            while *i < c.len() && (c[*i].is_alphanumeric() || c[*i] == '_' || c[*i] == '-') {
                *i += 1;
            }
            c[st..*i].iter().collect::<String>()
        };
        if ch == '#' {
            i += 1;
            out.push(Tok::Name(format!("#{}", word(&mut i))));
        } else if ch == ':' {
            i += 1;
            out.push(Tok::Val(format!(":{}", word(&mut i))));
        } else if ch.is_ascii_digit() {
            let w = word(&mut i);
            out.push(Tok::Num(w.parse().map_err(|_| format!("Invalid number {}", w))?));
        } else if ch.is_alphabetic() || ch == '_' {
            out.push(Tok::Ident(word(&mut i)));
        } else {
            let two: String = c[i..(i + 2).min(c.len())].iter().collect();
            let sym = match two.as_str() {
                "<>" => Some("<>"),
                "<=" => Some("<="),
                ">=" => Some(">="),
                _ => None,
            };
            if let Some(s) = sym {
                out.push(Tok::Sym(s));
                i += 2;
                continue;
            }
            let s = match ch {
                '=' => "=",
                '<' => "<",
                '>' => ">",
                '(' => "(",
                ')' => ")",
                ',' => ",",
                '.' => ".",
                '[' => "[",
                ']' => "]",
                '+' => "+",
                '-' => "-",
                _ => return Err(format!("Invalid character '{}' in expression", ch)),
            };
            out.push(Tok::Sym(s));
            i += 1;
        }
    }
    Ok(out)
}

#[derive(Clone, Debug)]
enum PathElem {
    Key(String),
    Index(usize),
}

#[derive(Clone, Debug)]
enum Operand {
    Path(Vec<PathElem>),
    Val(Value),
    Size(Box<Operand>),
    IfNotExists(Vec<PathElem>, Box<Operand>),
    ListAppend(Box<Operand>, Box<Operand>),
    Add(Box<Operand>, Box<Operand>),
    Sub(Box<Operand>, Box<Operand>),
}

#[derive(Clone, Debug)]
enum Cond {
    And(Box<Cond>, Box<Cond>),
    Or(Box<Cond>, Box<Cond>),
    Not(Box<Cond>),
    Cmp(Operand, &'static str, Operand),
    Between(Operand, Operand, Operand),
    In(Operand, Vec<Operand>),
    Func(String, Vec<Operand>),
}

struct Parser<'a> {
    t: Vec<Tok>,
    p: usize,
    names: &'a ObjectMap,
    values: &'a ObjectMap,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.p)
    }
    fn kw(&self, k: &str) -> bool {
        matches!(self.peek(), Some(Tok::Ident(i)) if i.eq_ignore_ascii_case(k))
    }
    fn sym(&self, s: &str) -> bool {
        matches!(self.peek(), Some(Tok::Sym(x)) if *x == s)
    }
    fn expect(&mut self, s: &str) -> Result<(), String> {
        if self.sym(s) {
            self.p += 1;
            Ok(())
        } else {
            Err(format!("Expected '{}' at token {}", s, self.p))
        }
    }
    fn path(&mut self) -> Result<Vec<PathElem>, String> {
        let mut out = vec![self.elem()?];
        loop {
            if self.sym(".") {
                self.p += 1;
                out.push(self.elem()?);
            } else if self.sym("[") {
                self.p += 1;
                match self.peek().cloned() {
                    Some(Tok::Num(n)) => {
                        self.p += 1;
                        out.push(PathElem::Index(n));
                    }
                    _ => return Err("Expected a list index".into()),
                }
                self.expect("]")?;
            } else {
                return Ok(out);
            }
        }
    }
    fn elem(&mut self) -> Result<PathElem, String> {
        match self.peek().cloned() {
            Some(Tok::Ident(i)) => {
                self.p += 1;
                Ok(PathElem::Key(i))
            }
            Some(Tok::Name(n)) => {
                self.p += 1;
                let v = self.names.get(&n).ok_or_else(|| format!("An expression attribute name used in the document path is not defined; attribute name: {}", n))?;
                Ok(PathElem::Key(v.to_string()))
            }
            t => Err(format!("Expected an attribute path, found {:?}", t)),
        }
    }
    fn operand(&mut self) -> Result<Operand, String> {
        let first = self.term()?;
        if self.sym("+") || self.sym("-") {
            let plus = self.sym("+");
            self.p += 1;
            let second = self.term()?;
            return Ok(if plus { Operand::Add(Box::new(first), Box::new(second)) } else { Operand::Sub(Box::new(first), Box::new(second)) });
        }
        Ok(first)
    }
    fn term(&mut self) -> Result<Operand, String> {
        match self.peek().cloned() {
            Some(Tok::Val(v)) => {
                self.p += 1;
                self.values.get(&v).cloned().map(Operand::Val).ok_or_else(|| format!("An expression attribute value used in expression is not defined; attribute value: {}", v))
            }
            Some(Tok::Ident(f)) if self.t.get(self.p + 1) == Some(&Tok::Sym("(")) => {
                self.p += 2;
                let r = match f.as_str() {
                    "size" => Operand::Size(Box::new(Operand::Path(self.path()?))),
                    "if_not_exists" => {
                        let p = self.path()?;
                        self.expect(",")?;
                        Operand::IfNotExists(p, Box::new(self.operand()?))
                    }
                    "list_append" => {
                        let a = self.operand()?;
                        self.expect(",")?;
                        Operand::ListAppend(Box::new(a), Box::new(self.operand()?))
                    }
                    other => return Err(format!("Invalid function name; function: {}", other)),
                };
                self.expect(")")?;
                Ok(r)
            }
            _ => Ok(Operand::Path(self.path()?)),
        }
    }
    fn cond(&mut self) -> Result<Cond, String> {
        let mut l = self.and()?;
        while self.kw("OR") {
            self.p += 1;
            l = Cond::Or(Box::new(l), Box::new(self.and()?));
        }
        Ok(l)
    }
    fn and(&mut self) -> Result<Cond, String> {
        let mut l = self.not()?;
        while self.kw("AND") {
            self.p += 1;
            l = Cond::And(Box::new(l), Box::new(self.not()?));
        }
        Ok(l)
    }
    fn not(&mut self) -> Result<Cond, String> {
        if self.kw("NOT") {
            self.p += 1;
            return Ok(Cond::Not(Box::new(self.not()?)));
        }
        self.primary()
    }
    fn primary(&mut self) -> Result<Cond, String> {
        if self.sym("(") {
            self.p += 1;
            let c = self.cond()?;
            self.expect(")")?;
            return Ok(c);
        }
        if let Some(Tok::Ident(f)) = self.peek().cloned() {
            if matches!(f.as_str(), "attribute_exists" | "attribute_not_exists" | "attribute_type" | "begins_with" | "contains") && self.t.get(self.p + 1) == Some(&Tok::Sym("(")) {
                self.p += 2;
                let mut args = vec![self.operand()?];
                while self.sym(",") {
                    self.p += 1;
                    args.push(self.operand()?);
                }
                self.expect(")")?;
                return Ok(Cond::Func(f, args));
            }
        }
        let l = self.operand()?;
        if self.kw("BETWEEN") {
            self.p += 1;
            let a = self.operand()?;
            if !self.kw("AND") {
                return Err("Expected AND in BETWEEN".into());
            }
            self.p += 1;
            return Ok(Cond::Between(l, a, self.operand()?));
        }
        if self.kw("IN") {
            self.p += 1;
            self.expect("(")?;
            let mut list = vec![self.operand()?];
            while self.sym(",") {
                self.p += 1;
                list.push(self.operand()?);
            }
            self.expect(")")?;
            return Ok(Cond::In(l, list));
        }
        let op = match self.peek() {
            Some(Tok::Sym(s)) if matches!(*s, "=" | "<>" | "<" | "<=" | ">" | ">=") => *s,
            t => return Err(format!("Expected a comparator, found {:?}", t)),
        };
        self.p += 1;
        Ok(Cond::Cmp(l, op, self.operand()?))
    }
}

fn get_path<'a>(item: &'a ObjectMap, path: &[PathElem]) -> Option<&'a Value> {
    let PathElem::Key(first) = &path[0] else { return None };
    let mut cur = item.get(first)?;
    for e in &path[1..] {
        cur = match e {
            PathElem::Key(k) => cur.get("M")?.get(k)?,
            PathElem::Index(i) => cur.get("L")?.as_array()?.get(*i)?,
        };
    }
    Some(cur)
}

fn number_av(n: f64) -> Value {
    let s = if n.fract() == 0.0 && n.abs() < 1e15 { format!("{}", n as i64) } else { n.to_string() };
    obj([("N", Value::from(s))])
}

fn num_of(v: &Value) -> Option<f64> {
    v.get("N").and_then(|n| n.to_string().parse().ok())
}

fn eval(item: &ObjectMap, o: &Operand) -> Result<Option<Value>, String> {
    Ok(match o {
        Operand::Path(p) => get_path(item, p).cloned(),
        Operand::Val(v) => Some(v.clone()),
        Operand::Size(inner) => eval(item, inner)?.map(|v| {
            let (t, x) = v.as_object().and_then(|m| m.iter().next().map(|(t, x)| (t.clone(), x.clone()))).unwrap_or_default();
            let n = match t.as_str() {
                "S" => x.to_string().chars().count(),
                "B" => base64_decode(&x.to_string()).map(|b| b.len()).unwrap_or(0),
                "L" | "SS" | "NS" | "BS" => x.as_array().map(|a| a.len()).unwrap_or(0),
                "M" => x.as_object().map(|m| m.len()).unwrap_or(0),
                _ => 0,
            };
            number_av(n as f64)
        }),
        Operand::IfNotExists(p, d) => match get_path(item, p) {
            Some(v) => Some(v.clone()),
            None => eval(item, d)?,
        },
        Operand::ListAppend(a, b) => {
            let la = eval(item, a)?.and_then(|v| v.get("L").and_then(|l| l.as_array()).cloned()).ok_or("list_append requires lists")?;
            let lb = eval(item, b)?.and_then(|v| v.get("L").and_then(|l| l.as_array()).cloned()).ok_or("list_append requires lists")?;
            Some(obj([("L", Value::Array([la, lb].concat()))]))
        }
        Operand::Add(a, b) | Operand::Sub(a, b) => {
            let x = eval(item, a)?.as_ref().and_then(num_of).ok_or("An operand in the update expression has an incorrect data type")?;
            let y = eval(item, b)?.as_ref().and_then(num_of).ok_or("An operand in the update expression has an incorrect data type")?;
            Some(number_av(if matches!(o, Operand::Add(..)) { x + y } else { x - y }))
        }
    })
}

fn test(item: &ObjectMap, c: &Cond) -> Result<bool, String> {
    use std::cmp::Ordering::*;
    Ok(match c {
        Cond::And(a, b) => test(item, a)? && test(item, b)?,
        Cond::Or(a, b) => test(item, a)? || test(item, b)?,
        Cond::Not(a) => !test(item, a)?,
        Cond::Cmp(a, op, b) => {
            let (x, y) = (eval(item, a)?, eval(item, b)?);
            match (x, y) {
                (Some(x), Some(y)) => {
                    let ord = cmp_av(&x, &y);
                    match *op {
                        "=" => ord == Some(Equal) || (ord.is_none() && x == y),
                        "<>" => !(ord == Some(Equal) || (ord.is_none() && x == y)),
                        "<" => ord == Some(Less),
                        "<=" => matches!(ord, Some(Less | Equal)),
                        ">" => ord == Some(Greater),
                        _ => matches!(ord, Some(Greater | Equal)),
                    }
                }
                _ => *op == "<>",
            }
        }
        Cond::Between(a, lo, hi) => match (eval(item, a)?, eval(item, lo)?, eval(item, hi)?) {
            (Some(x), Some(l), Some(h)) => matches!(cmp_av(&x, &l), Some(Greater | Equal)) && matches!(cmp_av(&x, &h), Some(Less | Equal)),
            _ => false,
        },
        Cond::In(a, list) => match eval(item, a)? {
            Some(x) => {
                let mut found = false;
                for o in list {
                    if eval(item, o)?.is_some_and(|y| cmp_av(&x, &y) == Some(Equal)) {
                        found = true;
                    }
                }
                found
            }
            None => false,
        },
        Cond::Func(f, args) => {
            let path_of = |o: &Operand| match o {
                Operand::Path(p) => Ok(p.clone()),
                _ => Err(format!("{} requires an attribute path", f)),
            };
            match f.as_str() {
                "attribute_exists" => get_path(item, &path_of(&args[0])?).is_some(),
                "attribute_not_exists" => get_path(item, &path_of(&args[0])?).is_none(),
                "attribute_type" => match (get_path(item, &path_of(&args[0])?), args.get(1).map(|a| eval(item, a)).transpose()?.flatten()) {
                    (Some(v), Some(t)) => v.as_object().and_then(|m| m.keys().next().cloned()) == t.get("S").map(|s| s.to_string()),
                    _ => false,
                },
                "begins_with" => match (eval(item, &args[0])?, args.get(1).map(|a| eval(item, a)).transpose()?.flatten()) {
                    (Some(v), Some(p)) => match (v.get("S"), p.get("S")) {
                        (Some(a), Some(b)) => a.to_string().starts_with(&b.to_string()),
                        _ => false,
                    },
                    _ => false,
                },
                _ => match (eval(item, &args[0])?, args.get(1).map(|a| eval(item, a)).transpose()?.flatten()) {
                    (Some(v), Some(x)) => {
                        if let (Some(a), Some(b)) = (v.get("S"), x.get("S")) {
                            a.to_string().contains(&b.to_string())
                        } else if let Some(l) = v.get("L").and_then(|l| l.as_array()) {
                            l.iter().any(|e| cmp_av(e, &x) == Some(Equal))
                        } else if let Some((_, set)) = v.as_object().and_then(|m| m.iter().find(|(k, _)| matches!(k.as_str(), "SS" | "NS" | "BS"))) {
                            let needle = x.as_object().and_then(|m| m.values().next().cloned()).unwrap_or_default();
                            set.as_array().is_some_and(|a| a.iter().any(|e| e.to_string() == needle.to_string()))
                        } else {
                            false
                        }
                    }
                    _ => false,
                },
            }
        }
    })
}

fn parse_cond(expr: &str, names: &ObjectMap, values: &ObjectMap) -> Result<Cond, String> {
    let mut p = Parser { t: tokenize(expr)?, p: 0, names, values };
    let c = p.cond()?;
    if p.p != p.t.len() {
        return Err(format!("Syntax error; token: {:?}", p.t.get(p.p)));
    }
    Ok(c)
}

fn set_path(item: &mut ObjectMap, path: &[PathElem], v: Value) -> Result<(), String> {
    let PathElem::Key(first) = &path[0] else { return Err("Invalid document path".into()) };
    if path.len() == 1 {
        item.insert(first.clone(), v);
        return Ok(());
    }
    let mut cur = item.get_mut(first).ok_or("The document path provided in the update expression is invalid for update")?;
    for (i, e) in path[1..].iter().enumerate() {
        let last = i == path.len() - 2;
        cur = match e {
            PathElem::Key(k) => {
                let m = cur.as_object_mut().and_then(|o| o.get_mut("M")).and_then(|m| m.as_object_mut()).ok_or("The document path provided in the update expression is invalid for update")?;
                if last {
                    m.insert(k.clone(), v);
                    return Ok(());
                }
                m.get_mut(k).ok_or("The document path provided in the update expression is invalid for update")?
            }
            PathElem::Index(n) => {
                let l = cur.as_object_mut().and_then(|o| o.get_mut("L")).and_then(|m| match m {
                    Value::Array(a) => Some(a),
                    _ => None,
                });
                let l = l.ok_or("The document path provided in the update expression is invalid for update")?;
                if last {
                    if *n >= l.len() {
                        l.push(v);
                    } else {
                        l[*n] = v;
                    }
                    return Ok(());
                }
                l.get_mut(*n).ok_or("The document path provided in the update expression is invalid for update")?
            }
        };
    }
    Ok(())
}

fn remove_path(item: &mut ObjectMap, path: &[PathElem]) {
    let PathElem::Key(first) = &path[0] else { return };
    if path.len() == 1 {
        item.remove(first);
        return;
    }
    let Some(mut cur) = item.get_mut(first) else { return };
    for (i, e) in path[1..].iter().enumerate() {
        let last = i == path.len() - 2;
        let next = match e {
            PathElem::Key(k) => {
                let Some(m) = cur.as_object_mut().and_then(|o| o.get_mut("M")).and_then(|m| m.as_object_mut()) else { return };
                if last {
                    m.remove(k);
                    return;
                }
                m.get_mut(k)
            }
            PathElem::Index(n) => {
                let Some(Value::Array(l)) = cur.as_object_mut().and_then(|o| o.get_mut("L")) else { return };
                if last {
                    if *n < l.len() {
                        l.remove(*n);
                    }
                    return;
                }
                l.get_mut(*n)
            }
        };
        match next {
            Some(n) => cur = n,
            None => return,
        }
    }
}

/// Apply an update expression; returns the top-level attribute names touched.
fn apply_update(item: &mut ObjectMap, expr: &str, names: &ObjectMap, values: &ObjectMap) -> Result<Vec<String>, String> {
    let mut p = Parser { t: tokenize(expr)?, p: 0, names, values };
    let mut touched = Vec::new();
    let top = |path: &[PathElem]| match &path[0] {
        PathElem::Key(k) => k.clone(),
        PathElem::Index(_) => String::new(),
    };
    while p.p < p.t.len() {
        let clause = match p.peek() {
            Some(Tok::Ident(i)) => i.to_ascii_uppercase(),
            t => return Err(format!("Syntax error; token: {:?}", t)),
        };
        p.p += 1;
        loop {
            let path = p.path()?;
            touched.push(top(&path));
            match clause.as_str() {
                "SET" => {
                    p.expect("=")?;
                    let o = p.operand()?;
                    let v = eval(item, &o)?.ok_or("The provided expression refers to an attribute that does not exist in the item")?;
                    set_path(item, &path, v)?;
                }
                "REMOVE" => remove_path(item, &path),
                "ADD" | "DELETE" => {
                    let v = match p.term()? {
                        Operand::Val(v) => v,
                        _ => return Err(format!("{} requires a value", clause)),
                    };
                    let cur = get_path(item, &path).cloned();
                    let new = match (clause.as_str(), cur, v.as_object().and_then(|m| m.iter().next().map(|(k, x)| (k.clone(), x.clone())))) {
                        ("ADD", None, Some(_)) => v.clone(),
                        ("ADD", Some(c), Some((t, _))) if t == "N" => number_av(num_of(&c).unwrap_or(0.0) + num_of(&v).unwrap_or(0.0)),
                        (op, Some(c), Some((t, x))) if matches!(t.as_str(), "SS" | "NS" | "BS") => {
                            let mut set: Vec<Value> = c.get(&t).and_then(|s| s.as_array()).cloned().unwrap_or_default();
                            for e in x.as_array().into_iter().flatten() {
                                if op == "ADD" {
                                    if !set.contains(e) {
                                        set.push(e.clone());
                                    }
                                } else {
                                    set.retain(|s| s != e);
                                }
                            }
                            if set.is_empty() {
                                remove_path(item, &path);
                                if !(p.sym(",")) {
                                    break;
                                }
                                p.p += 1;
                                continue;
                            }
                            obj([(t.as_str(), Value::Array(set))])
                        }
                        ("DELETE", None, _) => {
                            if !(p.sym(",")) {
                                break;
                            }
                            p.p += 1;
                            continue;
                        }
                        _ => return Err("An operand in the update expression has an incorrect data type".into()),
                    };
                    set_path(item, &path, new)?;
                }
                other => return Err(format!("Invalid UpdateExpression clause {}", other)),
            }
            if p.sym(",") {
                p.p += 1;
            } else {
                break;
            }
        }
    }
    Ok(touched)
}

fn project(item: &ObjectMap, proj: Option<&str>, names: &ObjectMap) -> ObjectMap {
    let Some(proj) = proj.filter(|p| !p.trim().is_empty()) else { return item.clone() };
    let mut out = ObjectMap::new();
    for part in proj.split(',') {
        let first = part.trim().split(['.', '[']).next().unwrap_or("").trim().to_string();
        let name = if first.starts_with('#') { names.get(&first).map(|n| n.to_string()).unwrap_or(first) } else { first };
        if let Some(v) = item.get(&name) {
            out.insert(name, v.clone());
        }
    }
    out
}

fn ddb_err(code: &str, msg: &str) -> Response {
    json_error(400, "com.amazonaws.dynamodb.v20120810", code, msg)
}

fn ddb_ok(v: Value) -> Response {
    Response::json(200, v.to_json()).with_header("Content-Type", "application/x-amz-json-1.0")
}

fn key_of(t: &super::Table, item: &ObjectMap) -> ObjectMap {
    item.iter().filter(|(k, _)| **k == t.hash || Some(*k) == t.range.as_ref()).map(|(k, v)| (k.clone(), v.clone())).collect()
}

fn push_stream_record(tail: &mut TailState, table: &str, key: ObjectMap, old: Option<ObjectMap>, new: Option<ObjectMap>) {
    if old == new {
        return;
    }
    tail.stream_seq += 1;
    let seq = format!("{:021}", tail.stream_seq * 100);
    let Some(s) = tail.streams.get_mut(table).filter(|s| s.enabled) else { return };
    let event = match (&old, &new) {
        (None, Some(_)) => "INSERT",
        (Some(_), None) => "REMOVE",
        _ => "MODIFY",
    };
    let mut d = ObjectMap::new();
    d.insert("ApproximateCreationDateTime".into(), Value::Int(now_secs()));
    d.insert("Keys".into(), Value::Object(key));
    let view = s.view_type.clone();
    if matches!(view.as_str(), "NEW_IMAGE" | "NEW_AND_OLD_IMAGES") {
        if let Some(n) = new {
            d.insert("NewImage".into(), Value::Object(n));
        }
    }
    if matches!(view.as_str(), "OLD_IMAGE" | "NEW_AND_OLD_IMAGES") {
        if let Some(o) = old {
            d.insert("OldImage".into(), Value::Object(o));
        }
    }
    d.insert("SequenceNumber".into(), Value::from(seq));
    d.insert("SizeBytes".into(), Value::Int(Value::Object(d.clone()).to_json().len() as i64));
    d.insert("StreamViewType".into(), Value::from(view));
    s.records.push(obj([
        ("eventID", Value::from(uuid())),
        ("eventName", Value::from(event)),
        ("eventVersion", Value::from("1.1")),
        ("eventSource", Value::from("aws:dynamodb")),
        ("awsRegion", Value::from("us-east-1")),
        ("dynamodb", Value::Object(d)),
    ]));
}

fn dynamodb(st: &Shared, req: &ServerRequest) -> Option<Response> {
    let target = req.header("X-Amz-Target")?.to_string();
    let body = parse_json(&req.body).unwrap_or_default();
    if let Some(op) = target.strip_prefix("DynamoDBStreams_20120810.") {
        return Some(streams(st, op, &body));
    }
    let op = target.strip_prefix("DynamoDB_20120810.")?.to_string();
    let table_name = body.get("TableName").map(|t| t.to_string()).unwrap_or_default();
    let map_of = |k: &str| body.get(k).and_then(|v| v.as_object()).cloned().unwrap_or_default();
    let names = map_of("ExpressionAttributeNames");
    let values = map_of("ExpressionAttributeValues");
    let return_values = body.get("ReturnValues").map(|r| r.to_string()).unwrap_or_else(|| "NONE".into());
    match op.as_str() {
        "BatchGetItem" => {
            let g = st.0.lock().unwrap();
            let mut responses = ObjectMap::new();
            for (t, spec) in map_of("RequestItems") {
                let Some(table) = g.tables.get(&t) else { return Some(ddb_err("ResourceNotFoundException", "Requested resource not found")) };
                let nm = spec.get("ExpressionAttributeNames").and_then(|n| n.as_object()).cloned().unwrap_or_default();
                let proj = spec.get("ProjectionExpression").map(|p| p.to_string());
                let mut items = Vec::new();
                for k in spec.get("Keys").and_then(|k| k.as_array()).into_iter().flatten() {
                    let key = k.as_object().cloned().unwrap_or_default();
                    if let Some(i) = table.items.iter().find(|i| item_matches_key(table, i, &key)) {
                        items.push(Value::Object(project(i, proj.as_deref(), &nm)));
                    }
                }
                responses.insert(t, Value::Array(items));
            }
            Some(ddb_ok(obj([("Responses", Value::Object(responses)), ("UnprocessedKeys", Value::Object(ObjectMap::new()))])))
        }
        "UpdateTable" => {
            let mut g = st.0.lock().unwrap();
            if !g.tables.contains_key(&table_name) {
                return Some(ddb_err("ResourceNotFoundException", "Requested resource not found"));
            }
            if let Some(pt) = body.get("ProvisionedThroughput") {
                let r = pt.get("ReadCapacityUnits").and_then(|x| x.as_i64()).unwrap_or(1);
                let w = pt.get("WriteCapacityUnits").and_then(|x| x.as_i64()).unwrap_or(1);
                g.tail.throughput.insert(table_name.clone(), (r, w));
            }
            if let Some(spec) = body.get("StreamSpecification") {
                if spec.get("StreamEnabled").and_then(|e| e.as_bool()) == Some(true) {
                    let view = spec.get("StreamViewType").map(|v| v.to_string()).unwrap_or_else(|| "NEW_AND_OLD_IMAGES".into());
                    enable_stream(&mut g.tail, &table_name, &view);
                } else if let Some(s) = g.tail.streams.get_mut(&table_name) {
                    s.enabled = false;
                }
            }
            let d = describe_extra(&g, &table_name);
            Some(ddb_ok(obj([("TableDescription", d)])))
        }
        "DescribeTable" | "CreateTable" => {
            let resp = super::ddb(st, req);
            if resp.status != 200 {
                return Some(resp);
            }
            let mut g = st.0.lock().unwrap();
            if op == "CreateTable" {
                if let Some(spec) = body.get("StreamSpecification").filter(|s| s.get("StreamEnabled").and_then(|e| e.as_bool()) == Some(true)) {
                    let view = spec.get("StreamViewType").map(|v| v.to_string()).unwrap_or_else(|| "NEW_AND_OLD_IMAGES".into());
                    enable_stream(&mut g.tail, &table_name, &view);
                }
                if let Some(pt) = body.get("ProvisionedThroughput") {
                    g.tail.throughput.insert(table_name.clone(), (pt.get("ReadCapacityUnits").and_then(|x| x.as_i64()).unwrap_or(1), pt.get("WriteCapacityUnits").and_then(|x| x.as_i64()).unwrap_or(1)));
                }
            }
            let field = if op == "CreateTable" { "TableDescription" } else { "Table" };
            Some(ddb_ok(obj([(field, describe_extra(&g, &table_name))])))
        }
        "GetItem" if body.get("ProjectionExpression").is_some() => {
            let g = st.0.lock().unwrap();
            let Some(t) = g.tables.get(&table_name) else { return Some(ddb_err("ResourceNotFoundException", "Requested resource not found")) };
            let key = map_of("Key");
            Some(match t.items.iter().find(|i| item_matches_key(t, i, &key)) {
                Some(i) => ddb_ok(obj([("Item", Value::Object(project(i, body.get("ProjectionExpression").map(|p| p.to_string()).as_deref(), &names)))])),
                None => ddb_ok(obj([])),
            })
        }
        "Query" | "Scan" if body.get("KeyConditionExpression").is_some() || body.get("FilterExpression").is_some() || body.get("ProjectionExpression").is_some() => {
            let g = st.0.lock().unwrap();
            let Some(t) = g.tables.get(&table_name) else { return Some(ddb_err("ResourceNotFoundException", "Requested resource not found")) };
            let kce = match body.get("KeyConditionExpression").map(|e| parse_cond(&e.to_string(), &names, &values)).transpose() {
                Ok(c) => c,
                Err(e) => return Some(ddb_err("ValidationException", &format!("Invalid KeyConditionExpression: {}", e))),
            };
            let filter = match body.get("FilterExpression").map(|e| parse_cond(&e.to_string(), &names, &values)).transpose() {
                Ok(c) => c,
                Err(e) => return Some(ddb_err("ValidationException", &format!("Invalid FilterExpression: {}", e))),
            };
            let legacy = if op == "Query" { map_of("KeyConditions") } else { map_of("ScanFilter") };
            if op == "Query" && kce.is_none() && legacy.is_empty() {
                return Some(ddb_err("ValidationException", "Either the KeyConditions or KeyConditionExpression parameter must be specified in the request."));
            }
            let mut items = Vec::new();
            for i in &t.items {
                let k = match &kce {
                    Some(c) => match test(i, c) {
                        Ok(b) => b,
                        Err(e) => return Some(ddb_err("ValidationException", &e)),
                    },
                    None => legacy.iter().all(|(a, c)| super::condition_matches(i, a, c)),
                };
                if k {
                    items.push(i.clone());
                }
            }
            if let Some(r) = &t.range {
                items.sort_by(|a, b| match (a.get(r), b.get(r)) {
                    (Some(x), Some(y)) => cmp_av(x, y).unwrap_or(std::cmp::Ordering::Equal),
                    _ => std::cmp::Ordering::Equal,
                });
            }
            if body.get("ScanIndexForward").and_then(|f| f.as_bool()) == Some(false) {
                items.reverse();
            }
            let mut last = None;
            if let Some(l) = body.get("Limit").and_then(|l| l.as_i64()) {
                if items.len() > l as usize {
                    items.truncate(l as usize);
                    last = items.last().map(|i| Value::Object(key_of(t, i)));
                }
            }
            let scanned = items.len();
            if let Some(f) = &filter {
                let mut kept = Vec::new();
                for i in items {
                    match test(&i, f) {
                        Ok(true) => kept.push(i),
                        Ok(false) => {}
                        Err(e) => return Some(ddb_err("ValidationException", &e)),
                    }
                }
                items = kept;
            }
            let proj = body.get("ProjectionExpression").map(|p| p.to_string());
            let mut out = ObjectMap::new();
            out.insert("Count".into(), Value::Int(items.len() as i64));
            out.insert("ScannedCount".into(), Value::Int(scanned as i64));
            out.insert("Items".into(), Value::Array(items.iter().map(|i| Value::Object(project(i, proj.as_deref(), &names))).collect()));
            if let Some(l) = last {
                out.insert("LastEvaluatedKey".into(), l);
            }
            Some(ddb_ok(Value::Object(out)))
        }
        "PutItem" | "DeleteItem" | "UpdateItem" => {
            let mut g = st.0.lock().unwrap();
            let Some(t) = g.tables.get(&table_name) else { return Some(ddb_err("ResourceNotFoundException", "Requested resource not found")) };
            let key_src = if op == "PutItem" { map_of("Item") } else { map_of("Key") };
            let key = key_of(t, &key_src);
            let old = t.items.iter().find(|i| item_matches_key(t, i, &key)).cloned();
            if let Some(ce) = body.get("ConditionExpression") {
                let cond = match parse_cond(&ce.to_string(), &names, &values) {
                    Ok(c) => c,
                    Err(e) => return Some(ddb_err("ValidationException", &format!("Invalid ConditionExpression: {}", e))),
                };
                match test(&old.clone().unwrap_or_default(), &cond) {
                    Ok(true) => {}
                    Ok(false) => return Some(ddb_err("ConditionalCheckFailedException", "The conditional request failed")),
                    Err(e) => return Some(ddb_err("ValidationException", &e)),
                }
            }
            let resp = if op == "UpdateItem" && body.get("UpdateExpression").is_some() {
                let mut item = old.clone().unwrap_or_else(|| key.clone());
                let touched = match apply_update(&mut item, &body.get("UpdateExpression").map(|e| e.to_string()).unwrap_or_default(), &names, &values) {
                    Ok(t) => t,
                    Err(e) => return Some(ddb_err("ValidationException", &format!("Invalid UpdateExpression: {}", e))),
                };
                if touched.iter().any(|a| *a == t.hash || Some(a) == t.range.as_ref()) {
                    return Some(ddb_err("ValidationException", "One or more parameter values were invalid: Cannot update attribute of the key"));
                }
                let t = g.tables.get_mut(&table_name).unwrap();
                match t.items.iter().position(|i| item_matches_key(t, i, &key)) {
                    Some(p) => t.items[p] = item.clone(),
                    None => t.items.push(item.clone()),
                }
                let pick = |m: &ObjectMap| -> ObjectMap { m.iter().filter(|(k, _)| touched.contains(k)).map(|(k, v)| (k.clone(), v.clone())).collect() };
                match return_values.as_str() {
                    "ALL_NEW" => ddb_ok(obj([("Attributes", Value::Object(item.clone()))])),
                    "ALL_OLD" => ddb_ok(old.clone().map(|o| obj([("Attributes", Value::Object(o))])).unwrap_or_else(|| obj([]))),
                    "UPDATED_NEW" => ddb_ok(obj([("Attributes", Value::Object(pick(&item)))])),
                    "UPDATED_OLD" => ddb_ok(obj([("Attributes", Value::Object(old.as_ref().map(pick).unwrap_or_default()))])),
                    _ => ddb_ok(obj([])),
                }
            } else {
                drop(g);
                let r = super::ddb(st, req);
                g = st.0.lock().unwrap();
                r
            };
            if resp.status == 200 && g.tail.streams.get(&table_name).is_some_and(|s| s.enabled) {
                let new = g.tables.get(&table_name).and_then(|t| t.items.iter().find(|i| item_matches_key(t, i, &key)).cloned());
                let tail = &mut g.tail;
                push_stream_record(tail, &table_name, key, old, new);
            }
            Some(resp)
        }
        _ => None,
    }
}

fn describe_extra(g: &super::State, name: &str) -> Value {
    let Some(t) = g.tables.get(name) else { return Value::Null };
    let mut d = ObjectMap::new();
    d.insert("TableName".into(), Value::from(name));
    d.insert("TableStatus".into(), Value::from("ACTIVE"));
    d.insert("KeySchema".into(), t.key_schema.clone());
    d.insert("AttributeDefinitions".into(), t.attribute_definitions.clone());
    d.insert("ItemCount".into(), Value::Int(t.items.len() as i64));
    let (r, w) = g.tail.throughput.get(name).copied().unwrap_or((1, 1));
    d.insert("ProvisionedThroughput".into(), obj([("ReadCapacityUnits", Value::Int(r)), ("WriteCapacityUnits", Value::Int(w))]));
    if let Some(s) = g.tail.streams.get(name) {
        d.insert("StreamSpecification".into(), obj([("StreamEnabled", Value::Bool(s.enabled)), ("StreamViewType", Value::from(s.view_type.clone()))]));
        if s.enabled {
            d.insert("LatestStreamArn".into(), Value::from(s.arn.clone()));
            d.insert("LatestStreamLabel".into(), Value::from(s.arn.rsplit('/').next().unwrap_or("").to_string()));
        }
    }
    Value::Object(d)
}

const SHARD_ID: &str = "shardId-00000001700000000000-00000001";

fn streams(st: &Shared, op: &str, body: &Value) -> Response {
    let g = st.0.lock().unwrap();
    let arn = body.get("StreamArn").map(|a| a.to_string()).unwrap_or_default();
    let find = |arn: &str| g.tail.streams.iter().find(|(_, s)| s.arn == arn).map(|(t, s)| (t.clone(), s));
    let err = |code: &str, msg: &str| json_error(400, "com.amazonaws.dynamodb.v20120810", code, msg);
    match op {
        "ListStreams" => {
            let list: Vec<Value> = g.tail.streams.iter().filter(|(_, s)| s.enabled).map(|(t, s)| obj([("StreamArn", Value::from(s.arn.clone())), ("TableName", Value::from(t.clone()))])).collect();
            ddb_ok(obj([("Streams", Value::Array(list))]))
        }
        "DescribeStream" => {
            let Some((table, s)) = find(&arn) else { return err("ResourceNotFoundException", "Requested resource not found: Stream") };
            let first = s.records.first().and_then(|r| r.get("dynamodb")).and_then(|d| d.get("SequenceNumber")).cloned().unwrap_or(Value::from("000000000000000000100"));
            ddb_ok(obj([(
                "StreamDescription",
                obj([
                    ("StreamArn", Value::from(s.arn.clone())),
                    ("StreamStatus", Value::from(if s.enabled { "ENABLED" } else { "DISABLED" })),
                    ("StreamViewType", Value::from(s.view_type.clone())),
                    ("TableName", Value::from(table)),
                    ("Shards", Value::Array(vec![obj([("ShardId", Value::from(SHARD_ID)), ("SequenceNumberRange", obj([("StartingSequenceNumber", first)]))])])),
                ]),
            )]))
        }
        "GetShardIterator" => {
            let Some((_, s)) = find(&arn) else { return err("ResourceNotFoundException", "Requested resource not found: Stream") };
            if body.get("ShardId").map(|x| x.to_string()).as_deref() != Some(SHARD_ID) {
                return err("ResourceNotFoundException", "Requested resource not found: Shard");
            }
            let seq = body.get("SequenceNumber").map(|x| x.to_string()).unwrap_or_default();
            let pos_of = |seq: &str| s.records.iter().position(|r| r.get("dynamodb").and_then(|d| d.get("SequenceNumber")).map(|x| x.to_string()).as_deref() == Some(seq));
            let pos = match body.get("ShardIteratorType").map(|t| t.to_string()).as_deref() {
                Some("TRIM_HORIZON") => 0,
                Some("LATEST") => s.records.len(),
                Some("AT_SEQUENCE_NUMBER") => match pos_of(&seq) {
                    Some(p) => p,
                    None => return err("ValidationException", "Invalid SequenceNumber"),
                },
                Some("AFTER_SEQUENCE_NUMBER") => match pos_of(&seq) {
                    Some(p) => p + 1,
                    None => return err("ValidationException", "Invalid SequenceNumber"),
                },
                _ => return err("ValidationException", "Invalid ShardIteratorType"),
            };
            ddb_ok(obj([("ShardIterator", Value::from(base64_encode(format!("{}|{}", s.arn, pos).as_bytes())))]))
        }
        "GetRecords" => {
            let it = body.get("ShardIterator").map(|x| x.to_string()).unwrap_or_default();
            let decoded = base64_decode(&it).and_then(|b| String::from_utf8(b).ok()).unwrap_or_default();
            let Some((sarn, pos)) = decoded.rsplit_once('|') else { return err("ValidationException", "Invalid ShardIterator") };
            let Some((_, s)) = find(sarn) else { return err("ResourceNotFoundException", "Requested resource not found: Stream") };
            let pos: usize = pos.parse().unwrap_or(0);
            let limit = body.get("Limit").and_then(|l| l.as_i64()).unwrap_or(1000).max(1) as usize;
            let recs: Vec<Value> = s.records.iter().skip(pos).take(limit).cloned().collect();
            let next = pos + recs.len();
            ddb_ok(obj([("Records", Value::Array(recs)), ("NextShardIterator", Value::from(base64_encode(format!("{}|{}", sarn, next).as_bytes())))]))
        }
        other => err("UnknownOperationException", &format!("Unsupported operation {}", other)),
    }
}

// ---------------------------------------------------------------------------
// Secrets Manager -> CloudTrail
// ---------------------------------------------------------------------------

fn secrets(st: &Shared, req: &ServerRequest) -> Option<Response> {
    let target = req.header("X-Amz-Target").and_then(|t| t.split('.').nth(1)).unwrap_or("").to_string();
    if !matches!(target.as_str(), "PutSecretValue" | "UpdateSecret" | "RotateSecret" | "DeleteSecret" | "RestoreSecret") {
        return None;
    }
    let resp = super::ext::handle(st, "secretsmanager", req)?;
    if resp.status == 200 {
        let body = parse_json(&req.body).unwrap_or_default();
        let id = body.get("SecretId").map(|v| v.to_string()).unwrap_or_default();
        let ev_id = uuid();
        let t = now_millis() as f64 / 1000.0;
        let doc = obj([
            ("eventVersion", Value::from("1.08")),
            ("eventSource", Value::from("secretsmanager.amazonaws.com")),
            ("eventName", Value::from(target.clone())),
            ("requestParameters", obj([("secretId", Value::from(id.clone()))])),
        ]);
        let ev = obj([
            ("EventId", Value::from(ev_id)),
            ("EventName", Value::from(target)),
            ("EventSource", Value::from("secretsmanager.amazonaws.com")),
            ("Username", Value::from("mock")),
            ("EventTime", Value::Float(t)),
            ("Resources", Value::Array(vec![obj([("ResourceType", Value::from("AWS::SecretsManager::Secret")), ("ResourceName", Value::from(id))])])),
            ("CloudTrailEvent", Value::from(doc.to_json())),
        ]);
        st.0.lock().unwrap().more.trail.push(ev);
    }
    Some(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn av(v: Value) -> Value {
        crate::aws::ddb::to_attribute_value(&v)
    }

    #[test]
    fn expressions() {
        let names: ObjectMap = [("#n".to_string(), Value::from("name"))].into_iter().collect();
        let values: ObjectMap = [(":a", av(Value::from("Ann"))), (":one", av(Value::Int(1))), (":lo", av(Value::Int(1))), (":hi", av(Value::Int(5))), (":l", av(Value::from(vec!["z"]))), (":p", av(Value::from("An")))]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        let mut item: ObjectMap = [("id".to_string(), av(Value::from("1"))), ("name".to_string(), av(Value::from("Ann"))), ("n".to_string(), av(Value::Int(3))), ("tags".to_string(), av(Value::from(vec!["a"]))), ("m".to_string(), av(Value::object([("x", Value::Int(1))])))]
            .into_iter()
            .collect();
        let c = |e: &str, item: &ObjectMap| test(item, &parse_cond(e, &names, &values).unwrap()).unwrap();
        assert!(c("#n = :a AND n BETWEEN :lo AND :hi", &item));
        assert!(c("begins_with(#n, :p) AND attribute_exists(m.x) AND NOT attribute_exists(nope)", &item));
        assert!(c("n IN (:one, :hi) OR (size(tags) = :one)", &item));
        assert!(!c("n < :one", &item));
        assert!(parse_cond("#missing = :a", &names, &values).is_err());
        let touched = apply_update(&mut item, "SET n = n + :one, tags = list_append(tags, :l), m.y = :one, c = if_not_exists(c, :lo) REMOVE m.x", &names, &values).unwrap();
        assert_eq!(touched, vec!["n", "tags", "m", "c", "m"]);
        assert_eq!(item["n"].to_json(), r#"{"N":"4"}"#);
        assert_eq!(item["tags"].to_json(), r#"{"L":[{"S":"a"},{"S":"z"}]}"#);
        assert_eq!(item["m"].to_json(), r#"{"M":{"y":{"N":"1"}}}"#);
        apply_update(&mut item, "ADD n :hi, s :ss DELETE x :ss", &names, &[(":hi".to_string(), av(Value::Int(5))), (":ss".to_string(), obj([("SS", Value::from(vec!["q"]))]))].into_iter().collect()).unwrap();
        assert_eq!(item["n"].to_json(), r#"{"N":"9"}"#);
        assert_eq!(item["s"].to_json(), r#"{"SS":["q"]}"#);
    }

    #[test]
    fn sqs_query_translation_round_trip() {
        let body = obj([
            ("QueueUrl", Value::from("http://x/000/q")),
            ("MessageBody", Value::from("hi & bye")),
            ("DelaySeconds", Value::Int(2)),
            ("MessageAttributes", obj([("k", obj([("DataType", Value::from("String")), ("StringValue", Value::from("v"))]))])),
        ]);
        let params = crate::aws::sqs::to_query_params(&body);
        let back = query_to_json(&params);
        assert_eq!(back, body);
        let resp = obj([("Messages", Value::Array(vec![obj([("MessageId", Value::from("m1")), ("Body", Value::from("<b>")), ("Attributes", obj([("SentTimestamp", Value::from("1"))]))])]))]);
        let x = crate::util::Xml::parse(&json_to_query_xml("ReceiveMessage", &resp)).unwrap();
        let v = crate::aws::sqs::from_query_xml("ReceiveMessage", &x);
        assert_eq!(v.get("Messages").unwrap().as_array().unwrap()[0].get("Body").unwrap(), &Value::from("<b>"));
    }
}

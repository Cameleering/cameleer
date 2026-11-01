//! More services on the [`AzureMock`] port:
//!
//! * Azure Functions: `/functions/{app}/api/{function}` runs a registered closure; function
//!   keys are checked (`x-functions-key` header or `code` query; the host master key also
//!   works).
//! * Event Grid topics: `POST /eventgrid/{topic}/api/events?api-version=...` validates the
//!   CloudEvents / Event Grid schema and the `aeg-sas-key`, `aeg-sas-token` (HMAC-SHA256
//!   recomputed with the topic key, resource = request URL, expiry checked) or an Entra ID
//!   bearer token for `https://eventgrid.azure.net/.default`.
//! * Schema Registry: `/$schemaGroups/...` (register / get-id / get by id / versions / groups)
//!   with bearer tokens for `https://eventhubs.azure.net/.default`; content types must match the
//!   group's format.
//! * Azure OpenAI: `/openai/deployments/{d}/chat/completions` and `/embeddings` with the
//!   `api-key` or a bearer token for `https://cognitiveservices.azure.com/.default`; replies
//!   echo the last user message.

use std::collections::BTreeMap;
use std::sync::Arc;

use camel_core::value::{ObjectMap, Value};

use super::{bearer_ok, AzureMock, Shared};
use crate::azure::eventgrid::sas_token;
use crate::http::{Response, ServerRequest};
use crate::util::{ct_eq, obj, parse_form, parse_iso8601, parse_json, now_secs, uuid};

/// Master (host) key accepted by every emulated function.
pub const FUNCTIONS_MASTER_KEY: &str = "mock-master-key";
/// API key of the emulated Azure OpenAI resource.
pub const OPENAI_KEY: &str = "mock-openai-key";

/// An emulated HTTP-triggered function: (method, query, body) → (status, content type, body).
pub type AzureFn = Arc<dyn Fn(&str, &str, &[u8]) -> (u16, String, Vec<u8>) + Send + Sync>;

struct Schema {
    id: String,
    group: String,
    name: String,
    version: i64,
    content: String,
}

/// State of these services.
#[derive(Default)]
pub(super) struct MoreState {
    functions: BTreeMap<(String, String), (Option<String>, AzureFn)>,
    topics: BTreeMap<String, (String, Vec<Value>)>,
    groups: BTreeMap<String, String>,
    schemas: Vec<Schema>,
    deployments: Vec<String>,
    openai_requests: Vec<Value>,
    /// id -> assistant
    assistants: BTreeMap<String, Value>,
    /// thread id -> messages (oldest first)
    threads: BTreeMap<String, Vec<Value>>,
    /// run id -> (thread, assistant, polls)
    runs: BTreeMap<String, (String, String, usize)>,
}

impl AzureMock {
    /// Register a function (`key` = its function key, None = anonymous).
    pub fn register_function(&self, app: &str, name: &str, key: Option<&str>, f: impl Fn(&str, &str, &[u8]) -> (u16, String, Vec<u8>) + Send + Sync + 'static) {
        self.state.lock().unwrap().more.functions.insert((app.into(), name.into()), (key.map(|k| k.to_string()), Arc::new(f)));
    }
    /// Options for `azure-functions://{app}/{function}` (endpoint override only).
    pub fn functions_options(&self, app: &str) -> String {
        format!("uriEndpointOverride={}/functions/{}", self.url(), app)
    }
    /// Create an Event Grid topic with an access key (base64).
    pub fn create_eventgrid_topic(&self, name: &str, key: &str) {
        self.state.lock().unwrap().more.topics.insert(name.into(), (key.into(), Vec::new()));
    }
    /// The topic endpoint URL.
    pub fn eventgrid_topic_url(&self, name: &str) -> String {
        format!("{}/eventgrid/{}/api/events", self.url(), name)
    }
    /// Events accepted by a topic.
    pub fn eventgrid_events(&self, name: &str) -> Vec<Value> {
        self.state.lock().unwrap().more.topics.get(name).map(|t| t.1.clone()).unwrap_or_default()
    }
    /// Create a schema group with a format (`Avro`, `Json`, `Custom`).
    pub fn create_schema_group(&self, name: &str, format: &str) {
        self.state.lock().unwrap().more.groups.insert(name.into(), format.into());
    }
    /// Options for `azure-schema-registry://namespace/group` (client credentials).
    pub fn schema_registry_options(&self) -> String {
        self.key_vault_options()
    }
    /// Deploy a model name on the emulated Azure OpenAI resource.
    pub fn add_openai_deployment(&self, name: &str) {
        self.state.lock().unwrap().more.deployments.push(name.into());
    }
    /// Options for `azure-openai://resource/deployment` with the API key.
    pub fn openai_options(&self) -> String {
        format!("apiKey=RAW({})&uriEndpointOverride={}", OPENAI_KEY, self.url())
    }
    /// Request bodies received by the OpenAI endpoints.
    pub fn openai_requests(&self) -> Vec<Value> {
        self.state.lock().unwrap().more.openai_requests.clone()
    }
}

fn json_err(status: u16, code: &str, msg: &str) -> Response {
    Response::json(status, obj([("error", obj([("code", Value::from(code)), ("message", Value::from(msg))]))]).to_json())
}

/// Dispatch (`None` = not one of these services).
pub(super) fn handle(st: &Shared, req: &ServerRequest, segs: &[String]) -> Option<Response> {
    let s: Vec<&str> = segs.iter().map(|x| x.as_str()).collect();
    Some(match s.as_slice() {
        ["functions", app, "api", name] => function(st, req, app, name),
        ["eventgrid", topic, "api", "events"] => eventgrid(st, req, topic),
        ["$schemaGroups", rest @ ..] => schema_registry(st, req, rest),
        ["openai", "deployments", d, rest @ ..] => openai(st, req, d, &rest.join("/")),
        ["openai", "assistants", ..] | ["openai", "threads", ..] => assistants(st, req, &s[1..]),
        _ => return None,
    })
}

fn function(st: &Shared, req: &ServerRequest, app: &str, name: &str) -> Response {
    let entry = st.lock().unwrap().more.functions.get(&(app.to_string(), name.to_string())).map(|(k, f)| (k.clone(), f.clone()));
    let Some((key, f)) = entry else { return Response::new(404).with_body(&b""[..]) };
    if let Some(k) = key {
        let given = req.header("x-functions-key").map(|s| s.to_string()).or_else(|| req.query_param("code")).unwrap_or_default();
        if !ct_eq(given.as_bytes(), k.as_bytes()) && !ct_eq(given.as_bytes(), FUNCTIONS_MASTER_KEY.as_bytes()) {
            st.lock().unwrap().auth_failures += 1;
            return Response::new(401).with_body(&b""[..]);
        }
    }
    let (status, ct, body) = f(&req.method, &req.query, &req.body);
    Response::new(status).with_header("Content-Type", ct).with_body(body)
}

fn eventgrid(st: &Shared, req: &ServerRequest, topic: &str) -> Response {
    let mut s = st.lock().unwrap();
    let Some(key) = s.more.topics.get(topic).map(|t| t.0.clone()) else { return json_err(404, "NotFound", "The topic does not exist") };
    let url = format!("http://{}{}", req.header("Host").unwrap_or(""), req.path);
    let authorized = if let Some(k) = req.header("aeg-sas-key") {
        ct_eq(k.as_bytes(), key.as_bytes())
    } else if let Some(tok) = req.header("aeg-sas-token") {
        let p = parse_form(tok);
        let get = |n: &str| p.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone()).unwrap_or_default();
        let (r, e) = (get("r"), get("e"));
        let expected = sas_token(&r, &e, &key).unwrap_or_default();
        r.eq_ignore_ascii_case(&url) && parse_iso8601(&e).is_some_and(|t| t > now_secs()) && ct_eq(expected.as_bytes(), tok.as_bytes())
    } else {
        bearer_ok(&s, req, "https://eventgrid.azure.net/.default")
    };
    if !authorized {
        s.auth_failures += 1;
        return json_err(401, "Unauthorized", "The request authorization key is not authorized for this topic.");
    }
    if req.query_param("api-version").is_none_or(|v| v.is_empty()) {
        return json_err(400, "BadRequest", "The api-version query parameter is required.");
    }
    let Ok(Value::Array(events)) = parse_json(&req.body) else { return json_err(400, "BadRequest", "The request body must be a JSON array of events.") };
    let cloud = req.header("Content-Type").is_some_and(|c| c.starts_with("application/cloudevents"));
    let required: &[&str] = if cloud { &["specversion", "id", "source", "type"] } else { &["id", "subject", "eventType", "eventTime", "dataVersion"] };
    for e in &events {
        if let Some(missing) = required.iter().find(|f| e.get(f).is_none_or(|v| v.to_string().is_empty())) {
            return json_err(400, "BadRequest", &format!("Required property '{}' was not set.", missing));
        }
        if cloud && e.get("specversion").map(|v| v.to_string()).as_deref() != Some("1.0") {
            return json_err(400, "BadRequest", "specversion must be 1.0");
        }
    }
    if let Some(t) = s.more.topics.get_mut(topic) {
        t.1.extend(events);
    }
    Response::new(200)
}

fn format_of(ct: &str) -> &'static str {
    if ct.contains("serialization=Avro") {
        "Avro"
    } else if ct.contains("serialization=Json") {
        "Json"
    } else {
        "Custom"
    }
}

fn content_type_of(format: &str) -> &'static str {
    match format {
        "Avro" => "application/json; serialization=Avro",
        "Json" => "application/json; serialization=Json",
        _ => "text/plain; charset=utf-8",
    }
}

fn schema_headers(resp: Response, sc: &Schema, host: &str) -> Response {
    resp.with_header("Schema-Id", sc.id.clone())
        .with_header("Schema-Version", sc.version.to_string())
        .with_header("Schema-Group-Name", sc.group.clone())
        .with_header("Schema-Name", sc.name.clone())
        .with_header("Location", format!("http://{}/$schemaGroups/{}/schemas/{}/versions/{}", host, sc.group, sc.name, sc.version))
}

fn schema_registry(st: &Shared, req: &ServerRequest, rest: &[&str]) -> Response {
    let mut s = st.lock().unwrap();
    if !bearer_ok(&s, req, "https://eventhubs.azure.net/.default") {
        s.auth_failures += 1;
        return json_err(401, "Unauthorized", "Unauthorized");
    }
    if req.query_param("api-version").is_none_or(|v| v.is_empty()) {
        return json_err(400, "InvalidRequest", "api-version is required");
    }
    let host = req.header("Host").unwrap_or("").to_string();
    let not_found = || json_err(404, "ItemNotFound", "Schema not found");
    let content = String::from_utf8_lossy(&req.body).into_owned();
    match (req.method.as_str(), rest) {
        ("GET", []) => Response::json(200, obj([("schemaGroups", Value::from(s.more.groups.keys().cloned().collect::<Vec<_>>()))]).to_json()),
        ("GET", ["$schemas", id]) => match s.more.schemas.iter().find(|x| x.id == *id) {
            Some(sc) => {
                let fmt = s.more.groups.get(&sc.group).cloned().unwrap_or_default();
                schema_headers(Response::new(200).with_header("Content-Type", content_type_of(&fmt)).with_body(sc.content.clone().into_bytes()), sc, &host)
            }
            None => not_found(),
        },
        (_, [group, "schemas", name_verb, more @ ..]) => {
            let Some(fmt) = s.more.groups.get(*group).cloned() else { return json_err(404, "GroupNotFound", &format!("Schema group {} does not exist", group)) };
            let (name, verb) = name_verb.split_once(':').unwrap_or((name_verb, ""));
            match (req.method.as_str(), verb, more) {
                ("PUT", "", []) | ("POST", "get-id", []) => {
                    let ct = req.header("Content-Type").unwrap_or("");
                    if format_of(ct) != fmt {
                        return json_err(415, "InvalidSchemaType", &format!("Content type {} does not match the group format {}", ct, fmt));
                    }
                    if let Some(sc) = s.more.schemas.iter().find(|x| x.group == *group && x.name == name && x.content == content) {
                        return schema_headers(Response::new(204), sc, &host);
                    }
                    if verb == "get-id" {
                        return not_found();
                    }
                    if fmt == "Avro" && parse_json(content.as_bytes()).is_err() {
                        return json_err(400, "InvalidSchema", "The schema is not valid Avro JSON");
                    }
                    let version = s.more.schemas.iter().filter(|x| x.group == *group && x.name == name).count() as i64 + 1;
                    let sc = Schema { id: uuid().replace('-', ""), group: group.to_string(), name: name.to_string(), version, content };
                    let resp = schema_headers(Response::new(204), &sc, &host);
                    s.more.schemas.push(sc);
                    resp
                }
                ("GET", "", ["versions"]) => {
                    let vs: Vec<Value> = s.more.schemas.iter().filter(|x| x.group == *group && x.name == name).map(|x| Value::Int(x.version)).collect();
                    if vs.is_empty() {
                        return not_found();
                    }
                    Response::json(200, obj([("schemaVersions", Value::Array(vs))]).to_json())
                }
                ("GET", "", ["versions", v]) => match s.more.schemas.iter().find(|x| x.group == *group && x.name == name && x.version.to_string() == *v) {
                    Some(sc) => schema_headers(Response::new(200).with_header("Content-Type", content_type_of(&fmt)).with_body(sc.content.clone().into_bytes()), sc, &host),
                    None => not_found(),
                },
                _ => json_err(400, "InvalidRequest", "Unsupported schema request"),
            }
        }
        _ => json_err(400, "InvalidRequest", "Unsupported schema registry request"),
    }
}

fn openai(st: &Shared, req: &ServerRequest, deployment: &str, op: &str) -> Response {
    let mut s = st.lock().unwrap();
    let key_ok = req.header("api-key").is_some_and(|k| ct_eq(k.as_bytes(), OPENAI_KEY.as_bytes()));
    if !key_ok && !bearer_ok(&s, req, "https://cognitiveservices.azure.com/.default") {
        s.auth_failures += 1;
        return json_err(401, "401", "Access denied due to invalid subscription key or wrong API endpoint.");
    }
    if req.query_param("api-version").is_none_or(|v| v.is_empty()) {
        return json_err(404, "404", "Resource not found");
    }
    if !s.more.deployments.iter().any(|d| d == deployment) {
        return json_err(404, "DeploymentNotFound", "The API deployment for this resource does not exist.");
    }
    let body = parse_json(&req.body).unwrap_or_default();
    s.more.openai_requests.push(body.clone());
    match op {
        "chat/completions" => {
            let msgs = body.get("messages").and_then(|m| m.as_array()).cloned().unwrap_or_default();
            if msgs.is_empty() {
                return json_err(400, "BadRequest", "'messages' is a required property");
            }
            let last = msgs.iter().rev().find(|m| m.get("role").map(|r| r.to_string()).as_deref() == Some("user")).and_then(|m| m.get("content")).map(|c| c.to_string()).unwrap_or_default();
            let system = msgs.iter().find(|m| m.get("role").map(|r| r.to_string()).as_deref() == Some("system")).and_then(|m| m.get("content")).map(|c| c.to_string());
            let mut message = ObjectMap::new();
            message.insert("role".into(), Value::from("assistant"));
            let finish = if let Some(tool) = body.get("tools").and_then(|t| t.as_array()).and_then(|t| t.first().cloned()) {
                let fname = tool.get("function").and_then(|f| f.get("name")).cloned().unwrap_or_default();
                message.insert("content".into(), Value::Null);
                message.insert("tool_calls".into(), Value::Array(vec![obj([("id", Value::from("call_1")), ("type", Value::from("function")), ("function", obj([("name", fname), ("arguments", Value::from("{}"))]))])]));
                "tool_calls"
            } else {
                let reply = match system {
                    Some(sys) => format!("[{}] Echo: {}", sys, last),
                    None => format!("Echo: {}", last),
                };
                message.insert("content".into(), Value::from(reply));
                "stop"
            };
            let (p, c) = (last.split_whitespace().count() as i64, 3);
            if body.get("stream").is_some_and(|v| v.is_truthy()) {
                // text/event-stream: one chunk per word, a finish chunk, a usage chunk, [DONE]
                let id = format!("chatcmpl-{}", uuid());
                let text = message.get("content").map(|c| c.to_string()).unwrap_or_default();
                let words: Vec<String> = text.split_inclusive(' ').map(|w| w.to_string()).collect();
                let mut events: Vec<(Option<&str>, String)> = Vec::new();
                let chunk = |delta: Value, finish: Value| obj([("id", Value::from(id.clone())), ("object", Value::from("chat.completion.chunk")), ("model", Value::from(deployment)), ("choices", Value::Array(vec![obj([("index", Value::Int(0)), ("delta", delta), ("finish_reason", finish)])]))]).to_json();
                events.push((None, chunk(obj([("role", Value::from("assistant")), ("content", Value::from(""))]), Value::Null)));
                for w in words {
                    events.push((None, chunk(obj([("content", Value::from(w))]), Value::Null)));
                }
                events.push((None, chunk(obj([]), Value::from(finish))));
                if body.get("stream_options").and_then(|o| o.get("include_usage")).is_some_and(|v| v.is_truthy()) {
                    events.push((None, obj([("id", Value::from(id.clone())), ("object", Value::from("chat.completion.chunk")), ("model", Value::from(deployment)), ("choices", Value::Array(vec![])), ("usage", obj([("prompt_tokens", Value::Int(p)), ("completion_tokens", Value::Int(c)), ("total_tokens", Value::Int(p + c))]))]).to_json()));
                }
                events.push((None, "[DONE]".to_string()));
                return Response::new(200).with_header("Content-Type", "text/event-stream").with_body(crate::sse::encode(&events));
            }
            Response::json(
                200,
                obj([
                    ("id", Value::from(format!("chatcmpl-{}", uuid()))),
                    ("object", Value::from("chat.completion")),
                    ("model", Value::from(deployment)),
                    ("choices", Value::Array(vec![obj([("index", Value::Int(0)), ("message", Value::Object(message)), ("finish_reason", Value::from(finish))])])),
                    ("usage", obj([("prompt_tokens", Value::Int(p)), ("completion_tokens", Value::Int(c)), ("total_tokens", Value::Int(p + c))])),
                ])
                .to_json(),
            )
        }
        "embeddings" => {
            let inputs: Vec<String> = match body.get("input") {
                Some(Value::Array(a)) => a.iter().map(|x| x.to_string()).collect(),
                Some(v) => vec![v.to_string()],
                None => return json_err(400, "BadRequest", "'input' is a required property"),
            };
            let data: Vec<Value> = inputs
                .iter()
                .enumerate()
                .rev()
                .map(|(i, t)| obj([("object", Value::from("embedding")), ("index", Value::Int(i as i64)), ("embedding", Value::Array((0..4).map(|k| Value::Float(((t.len() * (k + 1)) % 10) as f64 / 10.0)).collect()))]))
                .collect();
            Response::json(200, obj([("object", Value::from("list")), ("data", Value::Array(data)), ("usage", obj([("prompt_tokens", Value::Int(inputs.len() as i64)), ("total_tokens", Value::Int(inputs.len() as i64))]))]).to_json())
        }
        "images/generations" => {
            let prompt = body.get("prompt").map(|p| p.to_string()).unwrap_or_default();
            if prompt.is_empty() {
                return json_err(400, "BadRequest", "'prompt' is a required property");
            }
            let size = body.get("size").map(|x| x.to_string()).unwrap_or_else(|| "1024x1024".into());
            if !["1024x1024", "1792x1024", "1024x1792"].contains(&size.as_str()) {
                return json_err(400, "InvalidRequest", "The size is not supported by this model.");
            }
            let n = body.get("n").and_then(|n| n.as_i64()).unwrap_or(1);
            if n != 1 {
                return json_err(400, "InvalidRequest", "n must be 1 for dall-e-3");
            }
            let b64 = body.get("response_format").map(|f| f.to_string()).as_deref() == Some("b64_json");
            let mut item = ObjectMap::new();
            item.insert("revised_prompt".into(), Value::from(format!("A detailed picture of {}", prompt)));
            if b64 {
                item.insert("b64_json".into(), Value::from(crate::util::base64_encode(format!("PNG:{}:{}", size, prompt).as_bytes())));
            } else {
                item.insert("url".into(), Value::from(format!("https://dalleprodsec.blob.core.windows.net/private/images/{}.png", uuid())));
            }
            Response::json(200, obj([("created", Value::Int(crate::util::now_secs())), ("data", Value::Array(vec![Value::Object(item)]))]).to_json())
        }
        _ => json_err(404, "404", "Resource not found"),
    }
}

/// The Assistants API (`/openai/assistants`, `/openai/threads/...`): runs complete on the
/// second poll with an assistant message `[instructions] Echo: <last user message>`.
fn assistants(st: &Shared, req: &ServerRequest, path: &[&str]) -> Response {
    let mut s = st.lock().unwrap();
    let key_ok = req.header("api-key").is_some_and(|k| ct_eq(k.as_bytes(), OPENAI_KEY.as_bytes()));
    if !key_ok && !bearer_ok(&s, req, "https://cognitiveservices.azure.com/.default") {
        s.auth_failures += 1;
        return json_err(401, "401", "Access denied due to invalid subscription key or wrong API endpoint.");
    }
    if req.query_param("api-version").is_none_or(|v| v.is_empty()) {
        return json_err(404, "404", "Resource not found");
    }
    let body = parse_json(&req.body).unwrap_or_default();
    let now = Value::Int(crate::util::now_secs());
    let not_found = |what: &str| json_err(404, "not_found", &format!("No {} found", what));
    let m = &mut s.more;
    match (req.method.as_str(), path) {
        ("POST", ["assistants"]) => {
            let model = body.get("model").map(|x| x.to_string()).unwrap_or_default();
            if !m.deployments.contains(&model) {
                return json_err(400, "invalid_request_error", &format!("The deployment {} does not exist", model));
            }
            let id = format!("asst_{}", uuid().replace('-', "")[..20].to_string());
            let mut a = body.as_object().cloned().unwrap_or_default();
            a.insert("id".into(), Value::from(id.clone()));
            a.insert("object".into(), Value::from("assistant"));
            a.insert("created_at".into(), now);
            m.assistants.insert(id, Value::Object(a.clone()));
            Response::json(200, Value::Object(a).to_json())
        }
        ("GET", ["assistants"]) => Response::json(200, obj([("object", Value::from("list")), ("data", Value::Array(m.assistants.values().cloned().collect()))]).to_json()),
        ("GET", ["assistants", id]) => match m.assistants.get(*id) {
            Some(a) => Response::json(200, a.to_json()),
            None => not_found("assistant"),
        },
        ("DELETE", ["assistants", id]) => match m.assistants.remove(*id) {
            Some(_) => Response::json(200, obj([("id", Value::from(*id)), ("object", Value::from("assistant.deleted")), ("deleted", Value::Bool(true))]).to_json()),
            None => not_found("assistant"),
        },
        ("POST", ["threads"]) => {
            let id = format!("thread_{}", &uuid().replace('-', "")[..20]);
            m.threads.insert(id.clone(), Vec::new());
            Response::json(200, obj([("id", Value::from(id)), ("object", Value::from("thread")), ("created_at", now)]).to_json())
        }
        ("POST", ["threads", t, "messages"]) => {
            let Some(msgs) = m.threads.get_mut(*t) else { return not_found("thread") };
            let text = body.get("content").map(|c| c.to_string()).unwrap_or_default();
            let msg = obj([("id", Value::from(format!("msg_{}", &uuid().replace('-', "")[..20]))), ("object", Value::from("thread.message")), ("thread_id", Value::from(*t)), ("role", Value::from("user")), ("content", Value::Array(vec![obj([("type", Value::from("text")), ("text", obj([("value", Value::from(text)), ("annotations", Value::Array(vec![]))]))])]))]);
            msgs.push(msg.clone());
            Response::json(200, msg.to_json())
        }
        ("GET", ["threads", t, "messages"]) => match m.threads.get(*t) {
            Some(msgs) => Response::json(200, obj([("object", Value::from("list")), ("data", Value::Array(msgs.iter().rev().cloned().collect()))]).to_json()),
            None => not_found("thread"),
        },
        ("POST", ["threads", t, "runs"]) => {
            if !m.threads.contains_key(*t) {
                return not_found("thread");
            }
            let a = body.get("assistant_id").map(|x| x.to_string()).unwrap_or_default();
            if !m.assistants.contains_key(&a) {
                return not_found("assistant");
            }
            let id = format!("run_{}", &uuid().replace('-', "")[..20]);
            m.runs.insert(id.clone(), (t.to_string(), a.clone(), 0));
            Response::json(200, obj([("id", Value::from(id)), ("object", Value::from("thread.run")), ("thread_id", Value::from(*t)), ("assistant_id", Value::from(a)), ("status", Value::from("queued")), ("created_at", now)]).to_json())
        }
        ("GET", ["threads", t, "runs", r]) => {
            let Some((thread, a, polls)) = m.runs.get_mut(*r) else { return not_found("run") };
            if thread != t {
                return not_found("run");
            }
            *polls += 1;
            let (a, polls) = (a.clone(), *polls);
            let status = match polls {
                1 => "in_progress",
                2 => {
                    let instructions = m.assistants.get(&a).and_then(|x| x.get("instructions")).map(|i| i.to_string()).unwrap_or_default();
                    let msgs = m.threads.get_mut(*t).expect("thread");
                    let last = msgs.iter().rev().find(|x| x.get("role").map(|r| r.to_string()).as_deref() == Some("user")).and_then(|x| x.get("content")).and_then(|c| c.as_array()).and_then(|c| c.first()).and_then(|p| p.get("text")).and_then(|t| t.get("value")).map(|v| v.to_string()).unwrap_or_default();
                    let reply = if instructions.is_empty() { format!("Echo: {}", last) } else { format!("[{}] Echo: {}", instructions, last) };
                    msgs.push(obj([("id", Value::from(format!("msg_{}", &uuid().replace('-', "")[..20]))), ("object", Value::from("thread.message")), ("thread_id", Value::from(*t)), ("role", Value::from("assistant")), ("assistant_id", Value::from(a.clone())), ("run_id", Value::from(*r)), ("content", Value::Array(vec![obj([("type", Value::from("text")), ("text", obj([("value", Value::from(reply)), ("annotations", Value::Array(vec![]))]))])]))]));
                    "completed"
                }
                _ => "completed",
            };
            Response::json(200, obj([("id", Value::from(*r)), ("object", Value::from("thread.run")), ("thread_id", Value::from(*t)), ("assistant_id", Value::from(a)), ("status", Value::from(status))]).to_json())
        }
        _ => json_err(404, "404", "Resource not found"),
    }
}

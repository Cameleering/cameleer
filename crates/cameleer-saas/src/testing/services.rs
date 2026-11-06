//! Per-service mock servers.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use cameleer_core::value::{ObjectMap, Value};
use cameleer_http::HttpResponse;

use super::{basic, bearer, json, MockServer, Recorded};

fn obj(items: Vec<(&str, Value)>) -> Value {
    Value::Object(items.into_iter().map(|(k, v)| (k.to_string(), v)).collect::<ObjectMap>())
}

fn is_json(r: &Recorded) -> bool {
    r.header("Content-Type").map(|c| c.starts_with("application/json")).unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Slack
// ---------------------------------------------------------------------------

#[derive(Default)]
struct SlackState {
    posted: Vec<Value>,
    webhook_posts: Vec<Value>,
    history: Vec<(String, Value)>,
    next_ts: u64,
}

/// Slack mock: token [`SlackMock::TOKEN`], incoming webhook at [`SlackMock::webhook_url`],
/// channels `general` (`C001`) and `random` (`C002`).
pub struct SlackMock {
    pub server: MockServer,
    state: Arc<Mutex<SlackState>>,
}

impl SlackMock {
    pub const TOKEN: &'static str = "xoxb-test-token";
    pub const WEBHOOK_PATH: &'static str = "/services/T000/B000/XXXX";

    pub fn start() -> SlackMock {
        let state = Arc::new(Mutex::new(SlackState { next_ts: 1_700_000_000, ..Default::default() }));
        let st = state.clone();
        let server = MockServer::start(move |r| {
            let mut s = st.lock().unwrap();
            if r.path == Self::WEBHOOK_PATH {
                if r.method != "POST" || !is_json(r) {
                    return HttpResponse::text(400, "invalid_payload");
                }
                let v = r.json();
                if v.get("text").is_none() && v.get("blocks").is_none() && v.get("attachments").is_none() {
                    return HttpResponse::text(400, "no_text");
                }
                s.webhook_posts.push(v);
                return HttpResponse::text(200, "ok");
            }
            if !r.path.starts_with("/api/") {
                return HttpResponse::text(404, "not found");
            }
            if bearer(r).as_deref() != Some(Self::TOKEN) {
                return json(200, &obj(vec![("ok", Value::Bool(false)), ("error", "invalid_auth".into())]));
            }
            let channel_id = |name: &str| -> Option<String> {
                match name.trim_start_matches('#') {
                    "general" | "C001" => Some("C001".into()),
                    "random" | "C002" => Some("C002".into()),
                    _ => None,
                }
            };
            match r.path.as_str() {
                "/api/chat.postMessage" => {
                    if !is_json(r) {
                        return json(200, &obj(vec![("ok", Value::Bool(false)), ("error", "invalid_form_data".into())]));
                    }
                    let v = r.json();
                    let Some(ch) = v.get("channel").and_then(|c| channel_id(&c.to_string())) else {
                        return json(200, &obj(vec![("ok", Value::Bool(false)), ("error", "channel_not_found".into())]));
                    };
                    if v.get("text").is_none() && v.get("blocks").is_none() {
                        return json(200, &obj(vec![("ok", Value::Bool(false)), ("error", "no_text".into())]));
                    }
                    s.next_ts += 1;
                    let ts = format!("{}.000100", s.next_ts);
                    s.posted.push(v.clone());
                    let mut msg = v.clone();
                    if let Some(m) = msg.as_object_mut() {
                        m.insert("ts".into(), Value::String(ts.clone()));
                        m.remove("channel");
                    }
                    s.history.push((ch.clone(), msg));
                    json(200, &obj(vec![("ok", Value::Bool(true)), ("channel", ch.into()), ("ts", ts.into())]))
                }
                "/api/conversations.list" => json(
                    200,
                    &obj(vec![
                        ("ok", Value::Bool(true)),
                        (
                            "channels",
                            Value::Array(vec![obj(vec![("id", "C001".into()), ("name", "general".into())]), obj(vec![("id", "C002".into()), ("name", "random".into())])]),
                        ),
                    ]),
                ),
                "/api/conversations.history" => {
                    let Some(ch) = r.query_param("channel").and_then(channel_id) else {
                        return json(200, &obj(vec![("ok", Value::Bool(false)), ("error", "channel_not_found".into())]));
                    };
                    let oldest: f64 = r.query_param("oldest").and_then(|o| o.parse().ok()).unwrap_or(0.0);
                    let limit: usize = r.query_param("limit").and_then(|l| l.parse().ok()).unwrap_or(100);
                    let mut msgs: Vec<Value> = s
                        .history
                        .iter()
                        .filter(|(c, m)| *c == ch && m.get("ts").and_then(|t| t.to_string().parse::<f64>().ok()).unwrap_or(0.0) > oldest)
                        .map(|(_, m)| m.clone())
                        .collect();
                    msgs.reverse(); // newest first
                    msgs.truncate(limit);
                    json(200, &obj(vec![("ok", Value::Bool(true)), ("messages", Value::Array(msgs))]))
                }
                _ => json(200, &obj(vec![("ok", Value::Bool(false)), ("error", "unknown_method".into())])),
            }
        });
        SlackMock { server, state }
    }

    pub fn url(&self) -> String {
        self.server.url()
    }

    pub fn webhook_url(&self) -> String {
        format!("{}{}", self.server.url(), Self::WEBHOOK_PATH)
    }

    /// Add a message to a channel's history; returns its `ts`.
    pub fn add_message(&self, channel: &str, user: &str, text: &str) -> String {
        let mut s = self.state.lock().unwrap();
        s.next_ts += 1;
        let ts = format!("{}.000100", s.next_ts);
        let id = if channel.trim_start_matches('#') == "random" { "C002" } else { "C001" };
        s.history.push((id.into(), obj(vec![("type", "message".into()), ("user", user.into()), ("text", text.into()), ("ts", ts.clone().into())])));
        ts
    }

    /// Messages posted with `chat.postMessage`.
    pub fn posted(&self) -> Vec<Value> {
        self.state.lock().unwrap().posted.clone()
    }

    /// Payloads posted to the incoming webhook.
    pub fn webhook_posts(&self) -> Vec<Value> {
        self.state.lock().unwrap().webhook_posts.clone()
    }
}

// ---------------------------------------------------------------------------
// Telegram
// ---------------------------------------------------------------------------

#[derive(Default)]
struct TelegramState {
    sent: Vec<(String, Value)>,
    updates: Vec<Value>,
    next_update: i64,
    next_message: i64,
    webhook: Option<String>,
}

/// Telegram Bot API mock for token [`TelegramMock::TOKEN`].
pub struct TelegramMock {
    pub server: MockServer,
    state: Arc<Mutex<TelegramState>>,
}

impl TelegramMock {
    pub const TOKEN: &'static str = "123456:ABC-test";

    pub fn start() -> TelegramMock {
        let state = Arc::new(Mutex::new(TelegramState { next_update: 1000, next_message: 1, ..Default::default() }));
        let st = state.clone();
        let server = MockServer::start(move |r| {
            let prefix = format!("/bot{}/", Self::TOKEN);
            let Some(method) = r.path.strip_prefix(&prefix) else {
                return json(401, &obj(vec![("ok", Value::Bool(false)), ("error_code", Value::Int(401)), ("description", "Unauthorized".into())]));
            };
            let mut s = st.lock().unwrap();
            // parameters from JSON, form or query
            let mut params: ObjectMap = ObjectMap::new();
            for (k, v) in &r.query {
                params.insert(k.clone(), Value::String(v.clone()));
            }
            if is_json(r) {
                if let Value::Object(m) = r.json() {
                    params.extend(m);
                }
            } else if r.header("Content-Type").map(|c| c.starts_with("multipart/")).unwrap_or(false) {
                for (name, filename, data) in r.multipart() {
                    if let Some(f) = filename {
                        params.insert(name, obj(vec![("filename", f.into()), ("size", Value::Int(data.len() as i64))]));
                    } else {
                        params.insert(name, Value::String(String::from_utf8_lossy(&data).into_owned()));
                    }
                }
            } else if r.header("Content-Type").map(|c| c.starts_with("application/x-www-form-urlencoded")).unwrap_or(false) {
                for (k, v) in r.form() {
                    params.insert(k, Value::String(v));
                }
            }
            let bad = |d: &str| json(400, &obj(vec![("ok", Value::Bool(false)), ("error_code", Value::Int(400)), ("description", d.into())]));
            let ok = |v: Value| json(200, &obj(vec![("ok", Value::Bool(true)), ("result", v)]));
            match method {
                "getMe" => ok(obj(vec![("id", Value::Int(123456)), ("is_bot", Value::Bool(true)), ("username", "camel_test_bot".into())])),
                "sendMessage" | "sendPhoto" | "sendDocument" | "sendAudio" | "sendVideo" => {
                    let Some(chat) = params.get("chat_id").cloned() else { return bad("Bad Request: chat_id is empty") };
                    let field = match method {
                        "sendMessage" => "text",
                        "sendPhoto" => "photo",
                        "sendDocument" => "document",
                        "sendAudio" => "audio",
                        _ => "video",
                    };
                    if params.get(field).map(|v| v.to_string().is_empty()).unwrap_or(true) {
                        return bad(&format!("Bad Request: {} is empty", field));
                    }
                    let id = s.next_message;
                    s.next_message += 1;
                    let mut msg = params.clone();
                    msg.insert("message_id".into(), Value::Int(id));
                    msg.insert("chat".into(), obj(vec![("id", chat)]));
                    s.sent.push((method.to_string(), Value::Object(params)));
                    ok(Value::Object(msg))
                }
                "getUpdates" => {
                    let offset: i64 = params.get("offset").and_then(|o| o.to_string().parse().ok()).unwrap_or(0);
                    let limit: usize = params.get("limit").and_then(|o| o.to_string().parse().ok()).unwrap_or(100);
                    if s.webhook.is_some() {
                        return json(409, &obj(vec![("ok", Value::Bool(false)), ("error_code", Value::Int(409)), ("description", "Conflict: can't use getUpdates method while webhook is active".into())]));
                    }
                    // confirm (drop) updates before the offset
                    s.updates.retain(|u| u.get("update_id").and_then(|i| i.as_i64()).unwrap_or(0) >= offset);
                    let v: Vec<Value> = s.updates.iter().take(limit).cloned().collect();
                    ok(Value::Array(v))
                }
                "setWebhook" => {
                    let Some(url) = params.get("url").map(|u| u.to_string()) else { return bad("Bad Request: url is empty") };
                    s.webhook = if url.is_empty() { None } else { Some(url) };
                    ok(Value::Bool(true))
                }
                "deleteWebhook" => {
                    s.webhook = None;
                    ok(Value::Bool(true))
                }
                _ => json(404, &obj(vec![("ok", Value::Bool(false)), ("error_code", Value::Int(404)), ("description", "Not Found".into())])),
            }
        });
        TelegramMock { server, state }
    }

    pub fn url(&self) -> String {
        self.server.url()
    }

    /// Queue an incoming text message update; returns the update.
    pub fn push_message(&self, chat_id: i64, text: &str) -> Value {
        let mut s = self.state.lock().unwrap();
        let id = s.next_update;
        s.next_update += 1;
        let mid = s.next_message;
        s.next_message += 1;
        let u = obj(vec![
            ("update_id", Value::Int(id)),
            (
                "message",
                obj(vec![
                    ("message_id", Value::Int(mid)),
                    ("text", text.into()),
                    ("chat", obj(vec![("id", Value::Int(chat_id)), ("type", "private".into())])),
                    ("from", obj(vec![("id", Value::Int(chat_id)), ("first_name", "Tester".into())])),
                    ("date", Value::Int(1_700_000_000)),
                ]),
            ),
        ]);
        s.updates.push(u.clone());
        u
    }

    /// Messages sent by the bot: `(method, parameters)`.
    pub fn sent(&self) -> Vec<(String, Value)> {
        self.state.lock().unwrap().sent.clone()
    }

    /// The registered webhook url.
    pub fn webhook(&self) -> Option<String> {
        self.state.lock().unwrap().webhook.clone()
    }

    /// Deliver an update to the registered webhook (as Telegram would).
    pub fn deliver_to_webhook(&self, update: &Value) -> cameleer_core::error::Result<u16> {
        let url = self.webhook().ok_or_else(|| cameleer_core::error::CamelError::illegal_state("no webhook registered"))?;
        let r = cameleer_http::HttpClient::new().post(&url, "application/json", update.to_json().as_bytes())?;
        Ok(r.status)
    }
}

// ---------------------------------------------------------------------------
// GitHub
// ---------------------------------------------------------------------------

#[derive(Default)]
struct GithubState {
    pulls: Vec<Value>,
    issues: Vec<Value>,
    commits: Vec<Value>,
    tags: Vec<Value>,
    comments: Vec<(i64, Value)>,
    statuses: Vec<(String, Value)>,
    hooks: Vec<Value>,
    files: Vec<(i64, Value)>,
    blobs: Vec<(String, Vec<u8>)>,
    events: Vec<Value>,
    next_id: i64,
    next_number: i64,
}

/// GitHub REST API mock for repository [`GithubMock::OWNER`]/[`GithubMock::REPO`] and token
/// [`GithubMock::TOKEN`].
pub struct GithubMock {
    pub server: MockServer,
    state: Arc<Mutex<GithubState>>,
}

impl GithubMock {
    pub const TOKEN: &'static str = "ghp_testtoken";
    pub const OWNER: &'static str = "apache";
    pub const REPO: &'static str = "camel";

    pub fn start() -> GithubMock {
        let state = Arc::new(Mutex::new(GithubState { next_id: 100, next_number: 1, ..Default::default() }));
        let st = state.clone();
        let server = MockServer::start(move |r| {
            if bearer(r).as_deref() != Some(Self::TOKEN) {
                return json(401, &obj(vec![("message", "Bad credentials".into())]));
            }
            if r.header("User-Agent").is_none() {
                return json(403, &obj(vec![("message", "Request forbidden by administrative rules. Please make sure your request has a User-Agent header".into())]));
            }
            let base = format!("/repos/{}/{}", Self::OWNER, Self::REPO);
            let Some(rest) = r.path.strip_prefix(&base) else { return json(404, &obj(vec![("message", "Not Found".into())])) };
            let seg: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
            let mut s = st.lock().unwrap();
            match (r.method.as_str(), seg.as_slice()) {
                ("GET", ["pulls"]) => {
                    let mut v = s.pulls.clone();
                    v.reverse();
                    json(200, &Value::Array(v))
                }
                ("GET", ["issues"]) => {
                    let mut v: Vec<Value> = s.issues.clone();
                    v.extend(s.pulls.iter().map(|p| {
                        let mut p = p.clone();
                        p.as_object_mut().unwrap().insert("pull_request".into(), obj(vec![]));
                        p
                    }));
                    v.sort_by_key(|i| -i.get("id").and_then(|x| x.as_i64()).unwrap_or(0));
                    json(200, &Value::Array(v))
                }
                ("GET", ["commits"]) => {
                    let branch = r.query_param("sha").unwrap_or("main").to_string();
                    let mut v: Vec<Value> = s.commits.iter().filter(|c| c.get("branch").map(|b| b.to_string() == branch).unwrap_or(false)).cloned().collect();
                    v.reverse();
                    json(200, &Value::Array(v))
                }
                ("GET", ["pulls", n, "files"]) => {
                    let n: i64 = n.parse().unwrap_or(-1);
                    if !s.pulls.iter().any(|p| p.get("number").and_then(|x| x.as_i64()) == Some(n)) {
                        return json(404, &obj(vec![("message", "Not Found".into())]));
                    }
                    json(200, &Value::Array(s.files.iter().filter(|(p, _)| *p == n).map(|(_, f)| f.clone()).collect()))
                }
                ("PATCH", ["pulls", n]) => {
                    let n: i64 = n.parse().unwrap_or(-1);
                    let b = r.json();
                    match s.pulls.iter_mut().find(|p| p.get("number").and_then(|x| x.as_i64()) == Some(n)) {
                        Some(p) => {
                            if let Some(st) = b.get("state") {
                                p.as_object_mut().unwrap().insert("state".into(), st.clone());
                            }
                            json(200, p)
                        }
                        None => json(404, &obj(vec![("message", "Not Found".into())])),
                    }
                }
                ("GET", ["git", "blobs", sha]) => match s.blobs.iter().find(|(b, _)| b == sha) {
                    Some((b, data)) => {
                        // GitHub wraps base64 content at 60 columns
                        let enc = crate::rest::base64(data);
                        let wrapped: Vec<String> = enc.as_bytes().chunks(60).map(|c| String::from_utf8_lossy(c).into_owned()).collect();
                        json(200, &obj(vec![("sha", b.clone().into()), ("size", Value::Int(data.len() as i64)), ("encoding", "base64".into()), ("content", (wrapped.join("\n") + "\n").into())]))
                    }
                    None => json(404, &obj(vec![("message", "Not Found".into())])),
                },
                ("GET", ["events"]) => {
                    let mut v = s.events.clone();
                    v.reverse();
                    json(200, &Value::Array(v))
                }
                ("GET", ["tags"]) => {
                    let mut v = s.tags.clone();
                    v.reverse();
                    json(200, &Value::Array(v))
                }
                ("POST", ["issues"]) => {
                    let b = r.json();
                    let Some(title) = b.get("title").map(|t| t.to_string()).filter(|t| !t.is_empty()) else {
                        return json(422, &obj(vec![("message", "Validation Failed".into())]));
                    };
                    s.next_id += 1;
                    let (id, number) = (s.next_id, s.next_number);
                    s.next_number += 1;
                    let issue = obj(vec![
                        ("id", Value::Int(id)),
                        ("number", Value::Int(number)),
                        ("title", title.into()),
                        ("body", b.get("body").cloned().unwrap_or(Value::Null)),
                        ("state", "open".into()),
                        ("html_url", format!("https://github.com/{}/{}/issues/{}", Self::OWNER, Self::REPO, number).into()),
                    ]);
                    s.issues.push(issue.clone());
                    json(201, &issue)
                }
                ("POST", ["issues", n, "comments"]) => {
                    let Ok(n) = n.parse::<i64>() else { return json(404, &obj(vec![("message", "Not Found".into())])) };
                    let b = r.json();
                    if b.get("body").map(|t| t.to_string().is_empty()).unwrap_or(true) {
                        return json(422, &obj(vec![("message", "Validation Failed".into())]));
                    }
                    s.next_id += 1;
                    let c = obj(vec![("id", Value::Int(s.next_id)), ("body", b.get("body").cloned().unwrap())]);
                    s.comments.push((n, c.clone()));
                    json(201, &c)
                }
                ("POST", ["pulls", n, "comments", id, "replies"]) => {
                    let n: i64 = n.parse().unwrap_or(0);
                    s.next_id += 1;
                    let c = obj(vec![("id", Value::Int(s.next_id)), ("body", r.json().get("body").cloned().unwrap_or(Value::Null)), ("in_reply_to_id", Value::Int(id.parse().unwrap_or(0)))]);
                    s.comments.push((n, c.clone()));
                    json(201, &c)
                }
                ("POST", ["statuses", sha]) => {
                    let b = r.json();
                    let state = b.get("state").map(|x| x.to_string()).unwrap_or_default();
                    if !matches!(state.as_str(), "error" | "failure" | "pending" | "success") {
                        return json(422, &obj(vec![("message", "Validation Failed".into())]));
                    }
                    s.statuses.push((sha.to_string(), b.clone()));
                    json(201, &b)
                }
                ("POST", ["hooks"]) => {
                    let b = r.json();
                    if b.get("config").and_then(|c| c.get("url")).is_none() {
                        return json(422, &obj(vec![("message", "Validation Failed".into())]));
                    }
                    s.next_id += 1;
                    let mut h = b.clone();
                    h.as_object_mut().unwrap().insert("id".into(), Value::Int(s.next_id));
                    s.hooks.push(h.clone());
                    json(201, &h)
                }
                ("DELETE", ["hooks", id]) => {
                    let id: i64 = id.parse().unwrap_or(-1);
                    let before = s.hooks.len();
                    s.hooks.retain(|h| h.get("id").and_then(|x| x.as_i64()) != Some(id));
                    if s.hooks.len() == before {
                        return json(404, &obj(vec![("message", "Not Found".into())]));
                    }
                    HttpResponse::new(204)
                }
                _ => json(404, &obj(vec![("message", "Not Found".into())])),
            }
        });
        GithubMock { server, state }
    }

    pub fn url(&self) -> String {
        self.server.url()
    }

    /// Open a pull request; returns its number.
    pub fn add_pull(&self, title: &str, head_sha: &str) -> i64 {
        let mut s = self.state.lock().unwrap();
        s.next_id += 1;
        let (id, n) = (s.next_id, s.next_number);
        s.next_number += 1;
        s.pulls.push(obj(vec![("id", Value::Int(id)), ("number", Value::Int(n)), ("title", title.into()), ("state", "open".into()), ("head", obj(vec![("sha", head_sha.into())]))]));
        n
    }

    /// Open an issue; returns its number.
    pub fn add_issue(&self, title: &str) -> i64 {
        let mut s = self.state.lock().unwrap();
        s.next_id += 1;
        let (id, n) = (s.next_id, s.next_number);
        s.next_number += 1;
        s.issues.push(obj(vec![("id", Value::Int(id)), ("number", Value::Int(n)), ("title", title.into()), ("state", "open".into())]));
        n
    }

    /// Push a commit to a branch.
    pub fn add_commit(&self, branch: &str, sha: &str, message: &str, author: &str) {
        let mut s = self.state.lock().unwrap();
        s.commits.push(obj(vec![
            ("sha", sha.into()),
            ("branch", branch.into()),
            ("html_url", format!("https://github.com/{}/{}/commit/{}", Self::OWNER, Self::REPO, sha).into()),
            (
                "commit",
                obj(vec![
                    ("message", message.into()),
                    ("author", obj(vec![("name", author.into()), ("email", format!("{}@example.com", author).into())])),
                    ("committer", obj(vec![("name", "GitHub".into())])),
                ]),
            ),
        ]));
    }

    /// Add a changed file to pull request `pull` with blob `sha` holding `content`.
    pub fn add_pull_file(&self, pull: i64, filename: &str, sha: &str, content: &[u8]) {
        let mut s = self.state.lock().unwrap();
        s.files.push((pull, obj(vec![("sha", sha.into()), ("filename", filename.into()), ("status", "modified".into()), ("additions", Value::Int(1)), ("deletions", Value::Int(0)), ("changes", Value::Int(1))])));
        s.blobs.push((sha.to_string(), content.to_vec()));
    }

    /// Record a repository event (`PushEvent`...); returns its id.
    pub fn add_event(&self, ty: &str, payload: Value) -> i64 {
        let mut s = self.state.lock().unwrap();
        s.next_id += 1;
        let id = s.next_id;
        s.events.push(obj(vec![("id", id.to_string().into()), ("type", ty.into()), ("payload", payload), ("public", Value::Bool(true))]));
        id
    }

    /// Pull requests (with their current state).
    pub fn pulls(&self) -> Vec<Value> {
        self.state.lock().unwrap().pulls.clone()
    }

    /// Create a tag.
    pub fn add_tag(&self, name: &str) {
        self.state.lock().unwrap().tags.push(obj(vec![("name", name.into())]));
    }

    /// Issues (created through the API or [`GithubMock::add_issue`]).
    pub fn issues(&self) -> Vec<Value> {
        self.state.lock().unwrap().issues.clone()
    }

    /// Comments `(issue / PR number, comment)`.
    pub fn comments(&self) -> Vec<(i64, Value)> {
        self.state.lock().unwrap().comments.clone()
    }

    /// Commit statuses `(sha, status)`.
    pub fn statuses(&self) -> Vec<(String, Value)> {
        self.state.lock().unwrap().statuses.clone()
    }

    /// Registered webhooks.
    pub fn hooks(&self) -> Vec<Value> {
        self.state.lock().unwrap().hooks.clone()
    }
}

// ---------------------------------------------------------------------------
// Twilio
// ---------------------------------------------------------------------------

/// Twilio REST API mock for account [`TwilioMock::ACCOUNT_SID`] / auth token
/// [`TwilioMock::AUTH_TOKEN`]. Any collection below `/2010-04-01/` is a generic in-memory
/// resource store (create = POST to `<collection>.json`, list = GET, fetch / update /
/// delete on `<collection>/<sid>.json`, form fields stored in snake_case); `Messages`
/// validates `To` / `From` / `Body` like the real API.
pub struct TwilioMock {
    pub server: MockServer,
    store: Arc<Mutex<Vec<(String, Value)>>>,
}

fn snake_case(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

impl TwilioMock {
    pub const ACCOUNT_SID: &'static str = "ACtest0000000000000000000000000000";
    pub const AUTH_TOKEN: &'static str = "twilio-secret";

    pub fn start() -> TwilioMock {
        let store: Arc<Mutex<Vec<(String, Value)>>> = Arc::new(Mutex::new(Vec::new()));
        let st = store.clone();
        let server = MockServer::start(move |r| {
            let err = |status: u16, code: i64, m: &str| json(status, &obj(vec![("code", Value::Int(code)), ("message", m.into()), ("status", Value::Int(status as i64))]));
            if basic(r) != Some((Self::ACCOUNT_SID.to_string(), Self::AUTH_TOKEN.to_string())) {
                return err(401, 20003, "Authenticate");
            }
            let Some(path) = r.path.strip_prefix("/2010-04-01/").and_then(|p| p.strip_suffix(".json")) else { return err(404, 20404, "The requested resource was not found") };
            let mut s = st.lock().unwrap();
            // `<collection>/<sid>` when the last segment is a Twilio sid (2 letters + 32 chars)
            let (collection, sid) = match path.rsplit_once('/') {
                Some((c, last)) if last.len() == 34 && last.chars().take(2).all(|ch| ch.is_ascii_uppercase()) => (c.to_string(), Some(last.to_string())),
                _ => (path.to_string(), None),
            };
            let leaf = collection.rsplit('/').next().unwrap_or("").to_string();
            match (r.method.as_str(), sid) {
                ("POST", None) => {
                    if !r.header("Content-Type").map(|c| c.starts_with("application/x-www-form-urlencoded")).unwrap_or(false) {
                        return err(400, 20001, "Content-Type must be application/x-www-form-urlencoded");
                    }
                    if leaf == "Messages" {
                        if r.form_param("To").is_none() {
                            return err(400, 21604, "A 'To' phone number is required.");
                        }
                        if r.form_param("From").is_none() && r.form_param("MessagingServiceSid").is_none() {
                            return err(400, 21603, "A 'From' phone number is required.");
                        }
                        if r.form_param("Body").is_none() && r.form_param("MediaUrl").is_none() {
                            return err(400, 21602, "Message body is required.");
                        }
                    }
                    let prefix = match leaf.as_str() {
                        "Messages" => "SM",
                        "Calls" => "CA",
                        "Queues" => "QU",
                        "Addresses" => "AD",
                        "Applications" => "AP",
                        "IncomingPhoneNumbers" => "PN",
                        _ => "XX",
                    };
                    let n = s.len() + 1;
                    let sid = format!("{}{:032}", prefix, n);
                    let mut m = obj(vec![("sid", sid.into()), ("account_sid", Self::ACCOUNT_SID.into())]);
                    let mut fields: Vec<(String, Vec<String>)> = Vec::new();
                    for (k, v) in r.form() {
                        match fields.iter_mut().find(|(fk, _)| *fk == k) {
                            Some((_, vs)) => vs.push(v),
                            None => fields.push((k, vec![v])),
                        }
                    }
                    for (k, vs) in fields {
                        let v = if vs.len() == 1 { Value::String(vs[0].clone()) } else { Value::Array(vs.into_iter().map(Value::String).collect()) };
                        m.as_object_mut().unwrap().insert(snake_case(&k), v);
                    }
                    if leaf == "Messages" {
                        for k in ["from", "body"] {
                            if m.get(k).is_none() {
                                m.as_object_mut().unwrap().insert(k.into(), Value::Null);
                            }
                        }
                        m.as_object_mut().unwrap().insert("status".into(), "queued".into());
                    } else if leaf == "Calls" {
                        m.as_object_mut().unwrap().insert("status".into(), "queued".into());
                    }
                    s.push((collection.clone(), m.clone()));
                    json(201, &m)
                }
                ("GET", None) => {
                    let items: Vec<Value> = s.iter().filter(|(c, _)| *c == collection).map(|(_, v)| v.clone()).collect();
                    json(200, &obj(vec![(snake_case(&leaf).as_str(), Value::Array(items)), ("page", Value::Int(0)), ("page_size", Value::Int(50)), ("uri", format!("/2010-04-01/{}.json", collection).into())]))
                }
                (method, Some(sid)) => {
                    let pos = s.iter().position(|(c, v)| *c == collection && v.get("sid").map(|x| x.to_string() == sid).unwrap_or(false));
                    let Some(pos) = pos else { return err(404, 20404, "The requested resource was not found") };
                    match method {
                        "GET" => json(200, &s[pos].1),
                        "POST" => {
                            let obj_mut = s[pos].1.as_object_mut().unwrap();
                            for (k, v) in r.form() {
                                obj_mut.insert(snake_case(&k), Value::String(v));
                            }
                            json(200, &s[pos].1)
                        }
                        "DELETE" => {
                            s.remove(pos);
                            HttpResponse::new(204)
                        }
                        _ => err(405, 20004, "Method not allowed"),
                    }
                }
                _ => err(405, 20004, "Method not allowed"),
            }
        });
        TwilioMock { server, store }
    }

    pub fn url(&self) -> String {
        self.server.url()
    }

    /// Resources of a collection path below `/2010-04-01/` (`Accounts/AC.../Calls`).
    pub fn resources(&self, collection: &str) -> Vec<Value> {
        self.store.lock().unwrap().iter().filter(|(c, _)| c == collection).map(|(_, v)| v.clone()).collect()
    }

    /// Sent messages.
    pub fn messages(&self) -> Vec<Value> {
        self.resources(&format!("Accounts/{}/Messages", Self::ACCOUNT_SID))
    }
}

// ---------------------------------------------------------------------------
// Jira
// ---------------------------------------------------------------------------

#[derive(Default)]
struct JiraState {
    issues: Vec<Value>,
    comments: Vec<(String, Value)>,
    transitions: Vec<(String, Value)>,
    attachments: Vec<(String, String, Vec<u8>)>,
    watchers: Vec<(String, String)>,
    links: Vec<Value>,
    worklogs: Vec<(String, Value)>,
    next_id: i64,
}

/// Jira REST v2 mock: Basic auth [`JiraMock::USER`]/[`JiraMock::PASSWORD`] or bearer
/// [`JiraMock::PAT`].
pub struct JiraMock {
    pub server: MockServer,
    state: Arc<Mutex<JiraState>>,
}

impl JiraMock {
    pub const USER: &'static str = "bob";
    pub const PASSWORD: &'static str = "jira-secret";
    pub const PAT: &'static str = "jira-pat-token";

    pub fn start() -> JiraMock {
        let state = Arc::new(Mutex::new(JiraState { next_id: 10000, ..Default::default() }));
        let st = state.clone();
        let server = MockServer::start(move |r| {
            let authed = basic(r) == Some((Self::USER.into(), Self::PASSWORD.into())) || bearer(r).as_deref() == Some(Self::PAT);
            if !authed {
                return HttpResponse::text(401, "Unauthorized");
            }
            let errs = |m: &str| json(400, &obj(vec![("errorMessages", Value::Array(vec![m.into()])), ("errors", obj(vec![]))]));
            let mut s = st.lock().unwrap();
            let seg: Vec<&str> = r.path.split('/').filter(|x| !x.is_empty()).collect();
            match (r.method.as_str(), seg.as_slice()) {
                ("POST", ["rest", "api", "2", "issue"]) => {
                    if !is_json(r) {
                        return HttpResponse::text(415, "Unsupported Media Type");
                    }
                    let b = r.json();
                    let fields = b.get("fields").cloned().unwrap_or(Value::Null);
                    let Some(project) = fields.get("project").and_then(|p| p.get("key")).map(|k| k.to_string()) else { return errs("project is required") };
                    let Some(summary) = fields.get("summary").map(|k| k.to_string()).filter(|s| !s.is_empty()) else { return errs("summary is required") };
                    if fields.get("issuetype").map(|t| t.get("name").is_none() && t.get("id").is_none()).unwrap_or(true) {
                        return errs("issue type is required");
                    }
                    s.next_id += 1;
                    let n = s.issues.iter().filter(|i| i.get("project").map(|p| p.to_string() == project).unwrap_or(false)).count() + 1;
                    let key = format!("{}-{}", project, n);
                    let id = s.next_id.to_string();
                    s.issues.push(obj(vec![
                        ("id", id.clone().into()),
                        ("key", key.clone().into()),
                        ("project", project.into()),
                        ("fields", obj(vec![("summary", summary.into()), ("description", fields.get("description").cloned().unwrap_or(Value::Null)), ("issuetype", fields.get("issuetype").cloned().unwrap()), ("assignee", fields.get("assignee").cloned().unwrap_or(Value::Null)), ("priority", fields.get("priority").cloned().unwrap_or(Value::Null))])),
                    ]));
                    json(201, &obj(vec![("id", id.into()), ("key", key.clone().into()), ("self", format!("/rest/api/2/issue/{}", key).into())]))
                }
                ("GET", ["rest", "api", "2", "search"]) => {
                    let jql = r.query_param("jql").unwrap_or("").to_string();
                    let max: usize = r.query_param("maxResults").and_then(|m| m.parse().ok()).unwrap_or(50);
                    let project = jql.split("project").nth(1).map(|rest| rest.trim_start_matches([' ', '=']).split_whitespace().next().unwrap_or("").trim_matches(['"', '\'']).to_string());
                    let mut v: Vec<Value> = s.issues.iter().filter(|i| project.as_ref().map(|p| i.get("project").map(|x| x.to_string() == *p).unwrap_or(false)).unwrap_or(true)).cloned().collect();
                    if jql.to_ascii_lowercase().contains("order by key desc") {
                        v.reverse();
                    }
                    let total = v.len();
                    v.truncate(max);
                    json(200, &obj(vec![("startAt", Value::Int(0)), ("maxResults", Value::Int(max as i64)), ("total", Value::Int(total as i64)), ("issues", Value::Array(v))]))
                }
                ("POST", ["rest", "api", "2", "issue", key, "comment"]) => {
                    if !s.issues.iter().any(|i| i.get("key").map(|k| k.to_string() == *key).unwrap_or(false)) {
                        return json(404, &obj(vec![("errorMessages", Value::Array(vec!["Issue Does Not Exist".into()]))]));
                    }
                    let b = r.json();
                    let Some(body) = b.get("body").map(|x| x.to_string()).filter(|x| !x.is_empty()) else { return errs("Comment body can not be empty!") };
                    s.next_id += 1;
                    let c = obj(vec![("id", s.next_id.to_string().into()), ("body", body.into())]);
                    s.comments.push((key.to_string(), c.clone()));
                    json(201, &c)
                }
                ("GET", ["rest", "api", "2", "issue", key, "comment"]) => {
                    let v: Vec<Value> = s.comments.iter().filter(|(k, _)| k == key).map(|(_, c)| c.clone()).collect();
                    json(200, &obj(vec![("startAt", Value::Int(0)), ("total", Value::Int(v.len() as i64)), ("comments", Value::Array(v))]))
                }
                ("PUT", ["rest", "api", "2", "issue", key]) => {
                    let b = r.json();
                    let Some(issue) = s.issues.iter_mut().find(|i| i.get("key").map(|k| k.to_string() == *key).unwrap_or(false)) else {
                        return json(404, &obj(vec![("errorMessages", Value::Array(vec!["Issue Does Not Exist".into()]))]));
                    };
                    if let (Some(fields), Some(upd)) = (issue.as_object_mut().and_then(|o| o.get_mut("fields")).and_then(|f| f.as_object_mut()), b.get("fields").and_then(|f| f.as_object())) {
                        for (k, v) in upd.iter() {
                            fields.insert(k.clone(), v.clone());
                        }
                    }
                    HttpResponse::new(204)
                }
                ("GET", ["rest", "api", "2", "issue", _key, "transitions"]) => json(200, &obj(vec![("transitions", Value::Array(vec![obj(vec![("id", "11".into()), ("name", "To Do".into())]), obj(vec![("id", "21".into()), ("name", "In Progress".into())]), obj(vec![("id", "31".into()), ("name", "Done".into())])]))])),
                ("POST", ["rest", "api", "2", "issue", key, "transitions"]) => {
                    let b = r.json();
                    let id = b.get("transition").and_then(|t| t.get("id")).map(|i| i.to_string()).unwrap_or_default();
                    let name = match id.as_str() {
                        "11" => "To Do",
                        "21" => "In Progress",
                        "31" => "Done",
                        _ => return errs("It seems that you have tried to perform an invalid workflow operation."),
                    };
                    let Some(issue) = s.issues.iter_mut().find(|i| i.get("key").map(|k| k.to_string() == *key).unwrap_or(false)) else {
                        return json(404, &obj(vec![("errorMessages", Value::Array(vec!["Issue Does Not Exist".into()]))]));
                    };
                    if let Some(f) = issue.as_object_mut().and_then(|o| o.get_mut("fields")).and_then(|f| f.as_object_mut()) {
                        f.insert("status".into(), obj(vec![("name", name.into())]));
                    }
                    if let Some(c) = b.get("update").and_then(|u| u.get("comment")).and_then(|c| c.as_array()).and_then(|a| a.first()).and_then(|c| c.get("add")).and_then(|a| a.get("body")).cloned() {
                        s.next_id += 1;
                        let cm = obj(vec![("id", s.next_id.to_string().into()), ("body", c)]);
                        s.comments.push((key.to_string(), cm));
                    }
                    s.transitions.push((key.to_string(), b));
                    HttpResponse::new(204)
                }
                ("POST", ["rest", "api", "2", "issue", key, "attachments"]) => {
                    if r.header("X-Atlassian-Token") != Some("no-check") {
                        return HttpResponse::text(403, "XSRF check failed");
                    }
                    let parts = r.multipart();
                    let Some((_, fname, data)) = parts.into_iter().find(|(n, _, _)| n == "file") else { return errs("no file part") };
                    let fname = fname.unwrap_or_default();
                    s.next_id += 1;
                    let a = obj(vec![("id", s.next_id.to_string().into()), ("filename", fname.clone().into()), ("size", Value::Int(data.len() as i64))]);
                    s.attachments.push((key.to_string(), fname, data));
                    json(200, &Value::Array(vec![a]))
                }
                ("GET", ["rest", "api", "2", "issue", key, "watchers"]) => {
                    let w: Vec<Value> = s.watchers.iter().filter(|(k, _)| k == key).map(|(_, u)| obj(vec![("name", u.clone().into())])).collect();
                    json(200, &obj(vec![("isWatching", Value::Bool(false)), ("watchCount", Value::Int(w.len() as i64)), ("watchers", Value::Array(w))]))
                }
                ("POST", ["rest", "api", "2", "issue", key, "watchers"]) => {
                    let u = r.json().to_string();
                    if u.is_empty() {
                        return errs("user name required");
                    }
                    s.watchers.push((key.to_string(), u));
                    HttpResponse::new(204)
                }
                ("DELETE", ["rest", "api", "2", "issue", key, "watchers"]) => {
                    let u = r.query_param("username").unwrap_or("").to_string();
                    s.watchers.retain(|(k, w)| !(k == key && *w == u));
                    HttpResponse::new(204)
                }
                ("POST", ["rest", "api", "2", "issueLink"]) => {
                    let b = r.json();
                    if b.get("type").and_then(|t| t.get("name")).is_none() {
                        return errs("link type required");
                    }
                    s.links.push(b);
                    HttpResponse::new(201)
                }
                ("POST", ["rest", "api", "2", "issue", key, "worklog"]) => {
                    let b = r.json();
                    if b.get("timeSpentSeconds").and_then(|t| t.as_i64()).unwrap_or(0) <= 0 {
                        return errs("timeSpent required");
                    }
                    s.next_id += 1;
                    let mut w = b.clone();
                    w.as_object_mut().unwrap().insert("id".into(), s.next_id.to_string().into());
                    s.worklogs.push((key.to_string(), w.clone()));
                    json(201, &w)
                }
                ("GET", ["rest", "api", "2", "issue", key]) => match s.issues.iter().find(|i| i.get("key").map(|k| k.to_string() == *key).unwrap_or(false)) {
                    Some(i) => json(200, i),
                    None => json(404, &obj(vec![("errorMessages", Value::Array(vec!["Issue Does Not Exist".into()]))])),
                },
                ("DELETE", ["rest", "api", "2", "issue", key]) => {
                    let before = s.issues.len();
                    s.issues.retain(|i| i.get("key").map(|k| k.to_string() != *key).unwrap_or(true));
                    if s.issues.len() == before {
                        return json(404, &obj(vec![("errorMessages", Value::Array(vec!["Issue Does Not Exist".into()]))]));
                    }
                    HttpResponse::new(204)
                }
                _ => HttpResponse::text(404, "Not Found"),
            }
        });
        JiraMock { server, state }
    }

    pub fn url(&self) -> String {
        self.server.url()
    }

    /// Create an issue directly (as if created by someone else); returns its key.
    pub fn add_issue(&self, project: &str, summary: &str) -> String {
        let mut s = self.state.lock().unwrap();
        s.next_id += 1;
        let n = s.issues.iter().filter(|i| i.get("project").map(|p| p.to_string() == project).unwrap_or(false)).count() + 1;
        let key = format!("{}-{}", project, n);
        let id = s.next_id.to_string();
        s.issues.push(obj(vec![("id", id.into()), ("key", key.clone().into()), ("project", project.into()), ("fields", obj(vec![("summary", summary.into())]))]));
        key
    }

    pub fn issues(&self) -> Vec<Value> {
        self.state.lock().unwrap().issues.clone()
    }

    pub fn comments(&self) -> Vec<(String, Value)> {
        self.state.lock().unwrap().comments.clone()
    }

    /// Add a comment as another user; returns its id.
    pub fn add_comment(&self, key: &str, body: &str) -> String {
        let mut s = self.state.lock().unwrap();
        s.next_id += 1;
        let id = s.next_id.to_string();
        s.comments.push((key.to_string(), obj(vec![("id", id.clone().into()), ("body", body.into())])));
        id
    }

    /// Set a field of an issue (as if edited by someone else).
    pub fn set_field(&self, key: &str, field: &str, value: Value) {
        let mut s = self.state.lock().unwrap();
        if let Some(f) = s.issues.iter_mut().find(|i| i.get("key").map(|k| k.to_string() == key).unwrap_or(false)).and_then(|i| i.as_object_mut()).and_then(|o| o.get_mut("fields")).and_then(|f| f.as_object_mut()) {
            f.insert(field.to_string(), value);
        }
    }

    /// Attachments `(issue key, file name, content)`.
    pub fn attachments(&self) -> Vec<(String, String, Vec<u8>)> {
        self.state.lock().unwrap().attachments.clone()
    }

    /// Watchers `(issue key, user)`.
    pub fn watchers(&self) -> Vec<(String, String)> {
        self.state.lock().unwrap().watchers.clone()
    }

    /// Issue links (request bodies).
    pub fn links(&self) -> Vec<Value> {
        self.state.lock().unwrap().links.clone()
    }

    /// Work logs `(issue key, worklog)`.
    pub fn worklogs(&self) -> Vec<(String, Value)> {
        self.state.lock().unwrap().worklogs.clone()
    }
}

// ---------------------------------------------------------------------------
// OpenAI
// ---------------------------------------------------------------------------

/// The scripted tool calls in a user message: `call NAME {json}`, several separated by `;`
/// (the model mocks call these tools when tools are offered).
pub fn scripted_tool_calls(text: &str) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for part in text.split(';') {
        let part = part.trim();
        let Some(rest) = part.strip_prefix("call ") else { continue };
        let (name, args) = match rest.find(' ') {
            Some(i) => (&rest[..i], rest[i..].trim()),
            None => (rest, "{}"),
        };
        let args = cameleer_core::json::parse(args).unwrap_or(Value::Object(ObjectMap::new()));
        out.push((name.trim().to_string(), args));
    }
    out
}

/// OpenAI-compatible API mock (API key [`OpenAiMock::API_KEY`], base url `url() + "/v1"`).
/// Chat completions answer `Echo: <last user message>`. With `tools`, a last user message
/// `call NAME {json}` (see [`scripted_tool_calls`]) gets tool calls, and tool results get
/// `Done: <results>`. Tool messages must answer every call id, as on the real API.
pub struct OpenAiMock {
    pub server: MockServer,
}

impl OpenAiMock {
    pub const API_KEY: &'static str = "sk-test-key";

    pub fn start() -> OpenAiMock {
        let server = MockServer::start(move |r| {
            let err = |status: u16, m: &str, t: &str| json(status, &obj(vec![("error", obj(vec![("message", m.into()), ("type", t.into())]))]));
            if bearer(r).as_deref() != Some(Self::API_KEY) {
                return err(401, "Incorrect API key provided", "invalid_request_error");
            }
            if !is_json(r) {
                return err(400, "Content-Type must be application/json", "invalid_request_error");
            }
            let b = r.json();
            let Some(model) = b.get("model").map(|m| m.to_string()).filter(|m| !m.is_empty()) else {
                return err(400, "you must provide a model parameter", "invalid_request_error");
            };
            match r.path.as_str() {
                "/v1/chat/completions" => {
                    let Some(msgs) = b.get("messages").and_then(|m| m.as_array()).filter(|m| !m.is_empty()) else {
                        return err(400, "'messages' must contain at least one message", "invalid_request_error");
                    };
                    if msgs.iter().any(|m| m.get("role").is_none() || (m.get("content").is_none() && m.get("tool_calls").is_none())) {
                        return err(400, "each message requires 'role' and 'content'", "invalid_request_error");
                    }
                    // every tool call is answered by a tool message right after it
                    for (i, m) in msgs.iter().enumerate() {
                        let Some(Value::Array(calls)) = m.get("tool_calls") else { continue };
                        let answered: Vec<String> = msgs[i + 1..]
                            .iter()
                            .take_while(|n| n.get("role").map(|r| r.to_string() == "tool").unwrap_or(false))
                            .filter_map(|n| n.get("tool_call_id").map(|x| x.to_string()))
                            .collect();
                        for c in calls {
                            let id = c.get("id").map(|x| x.to_string()).unwrap_or_default();
                            if !answered.contains(&id) {
                                return err(400, &format!("An assistant message with 'tool_calls' must be followed by tool messages responding to each 'tool_call_id'. The following tool_call_ids did not have response messages: {}", id), "invalid_request_error");
                            }
                        }
                    }
                    let has_tools = b.get("tools").and_then(|t| t.as_array()).map(|t| !t.is_empty()).unwrap_or(false);
                    let last = msgs.last().cloned().unwrap_or(Value::Null);
                    let usage = obj(vec![("prompt_tokens", Value::Int(12)), ("completion_tokens", Value::Int(4)), ("total_tokens", Value::Int(16))]);
                    let reply = |message: Value, finish: &str| {
                        json(
                            200,
                            &obj(vec![
                                ("id", "chatcmpl-test".into()),
                                ("object", "chat.completion".into()),
                                ("model", model.clone().into()),
                                ("choices", Value::Array(vec![obj(vec![("index", Value::Int(0)), ("message", message), ("finish_reason", finish.into())])])),
                                ("usage", usage.clone()),
                            ]),
                        )
                    };
                    if has_tools && last.get("role").map(|r| r.to_string() == "tool").unwrap_or(false) {
                        let results: Vec<String> = msgs
                            .iter()
                            .rev()
                            .take_while(|m| m.get("role").map(|r| r.to_string() == "tool").unwrap_or(false))
                            .map(|m| m.get("content").map(|c| c.to_string()).unwrap_or_default())
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .collect();
                        return reply(obj(vec![("role", "assistant".into()), ("content", format!("Done: {}", results.join(" | ")).into())]), "stop");
                    }
                    if has_tools && last.get("role").map(|r| r.to_string() == "user").unwrap_or(false) {
                        let calls = scripted_tool_calls(&last.get("content").map(|c| c.to_string()).unwrap_or_default());
                        if !calls.is_empty() {
                            let tool_calls: Vec<Value> = calls
                                .iter()
                                .enumerate()
                                .map(|(i, (n, a))| {
                                    obj(vec![
                                        ("id", format!("call_{}", i + 1).into()),
                                        ("type", "function".into()),
                                        ("function", obj(vec![("name", n.as_str().into()), ("arguments", a.to_json().into())])),
                                    ])
                                })
                                .collect();
                            return reply(obj(vec![("role", "assistant".into()), ("content", Value::Null), ("tool_calls", Value::Array(tool_calls))]), "tool_calls");
                        }
                    }
                    let last_user = msgs.iter().rev().find(|m| m.get("role").map(|r| r.to_string() == "user").unwrap_or(false)).and_then(|m| m.get("content")).map(|c| c.to_string()).unwrap_or_default();
                    let system = msgs.iter().find(|m| m.get("role").map(|r| r.to_string() == "system").unwrap_or(false)).and_then(|m| m.get("content")).map(|c| format!(" [{}]", c)).unwrap_or_default();
                    let content = format!("Echo: {}{}", last_user, system);
                    let prompt_tokens = msgs.iter().map(|m| m.get("content").map(|c| c.to_string().split_whitespace().count()).unwrap_or(0)).sum::<usize>() as i64;
                    let completion_tokens = content.split_whitespace().count() as i64;
                    json(
                        200,
                        &obj(vec![
                            ("id", "chatcmpl-test".into()),
                            ("object", "chat.completion".into()),
                            ("model", model.into()),
                            ("choices", Value::Array(vec![obj(vec![("index", Value::Int(0)), ("message", obj(vec![("role", "assistant".into()), ("content", content.into())])), ("finish_reason", "stop".into())])])),
                            ("usage", obj(vec![("prompt_tokens", Value::Int(prompt_tokens)), ("completion_tokens", Value::Int(completion_tokens)), ("total_tokens", Value::Int(prompt_tokens + completion_tokens))])),
                        ]),
                    )
                }
                "/v1/embeddings" => {
                    let inputs: Vec<String> = match b.get("input") {
                        Some(Value::Array(a)) => a.iter().map(|x| x.to_string()).collect(),
                        Some(v) => vec![v.to_string()],
                        None => return err(400, "'input' is required", "invalid_request_error"),
                    };
                    let data: Vec<Value> = inputs
                        .iter()
                        .enumerate()
                        .map(|(i, s)| {
                            let l = s.len() as f64;
                            obj(vec![("index", Value::Int(i as i64)), ("object", "embedding".into()), ("embedding", Value::Array(vec![Value::Float(l), Value::Float(l / 2.0), Value::Float(1.0)]))])
                        })
                        .collect();
                    json(200, &obj(vec![("object", "list".into()), ("model", model.into()), ("data", Value::Array(data)), ("usage", obj(vec![("prompt_tokens", Value::Int(1)), ("total_tokens", Value::Int(1))]))]))
                }
                _ => err(404, "Unknown endpoint", "invalid_request_error"),
            }
        });
        OpenAiMock { server }
    }

    /// Base url to use as `baseUrl` (`http://127.0.0.1:port/v1`).
    pub fn base_url(&self) -> String {
        format!("{}/v1", self.server.url())
    }
}

/// Anthropic Messages API mock (`x-api-key` [`AnthropicMock::API_KEY`], `anthropic-version`
/// required, base url `url()`). A text answer is `Echo: <last user text>`; with `tools`, a user
/// message `call NAME {json}` (see [`scripted_tool_calls`]) gets `tool_use` blocks and tool
/// results get `Done: <results>`. Every `tool_use` must be answered by a `tool_result` in the
/// next message, as on the real API.
pub struct AnthropicMock {
    pub server: MockServer,
}

impl AnthropicMock {
    pub const API_KEY: &'static str = "sk-ant-test-key";

    pub fn start() -> AnthropicMock {
        let server = MockServer::start(move |r| {
            let err = |status: u16, t: &str, m: &str| json(status, &obj(vec![("type", "error".into()), ("error", obj(vec![("type", t.into()), ("message", m.into())]))]));
            if r.header("x-api-key") != Some(Self::API_KEY) {
                return err(401, "authentication_error", "invalid x-api-key");
            }
            if r.header("anthropic-version").is_none() {
                return err(400, "invalid_request_error", "anthropic-version: header is required");
            }
            if r.path != "/v1/messages" {
                return err(404, "not_found_error", "Not found");
            }
            let b = r.json();
            if b.get("model").is_none() || b.get("max_tokens").is_none() {
                return err(400, "invalid_request_error", "model and max_tokens are required");
            }
            let msgs = b.get("messages").and_then(|m| m.as_array()).cloned().unwrap_or_default();
            if msgs.is_empty() || msgs[0].get("role").map(|x| x.to_string()).as_deref() != Some("user") {
                return err(400, "invalid_request_error", "messages: the first message must use the user role");
            }
            let blocks = |m: &Value| m.get("content").and_then(|c| c.as_array()).cloned().unwrap_or_default();
            for (i, m) in msgs.iter().enumerate() {
                let uses: Vec<String> = blocks(m).iter().filter(|x| x.get("type").map(|t| t.to_string()).as_deref() == Some("tool_use")).map(|x| x.get("id").map(|v| v.to_string()).unwrap_or_default()).collect();
                if uses.is_empty() {
                    continue;
                }
                let answered: Vec<String> = msgs.get(i + 1).map(|n| blocks(n).iter().filter(|x| x.get("type").map(|t| t.to_string()).as_deref() == Some("tool_result")).map(|x| x.get("tool_use_id").map(|v| v.to_string()).unwrap_or_default()).collect()).unwrap_or_default();
                if let Some(missing) = uses.iter().find(|u| !answered.contains(u)) {
                    return err(400, "invalid_request_error", &format!("messages.{}: `tool_use` ids were found without `tool_result` blocks immediately after: {}", i, missing));
                }
            }
            let last = msgs.last().cloned().unwrap_or_default();
            let text_of = |m: &Value| match m.get("content") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Array(a)) => a.iter().filter_map(|x| x.get("text").map(|t| t.to_string())).collect::<Vec<_>>().join(""),
                _ => String::new(),
            };
            let reply = |content: Vec<Value>, stop: &str| {
                json(
                    200,
                    &obj(vec![
                        ("id", "msg_test".into()),
                        ("type", "message".into()),
                        ("role", "assistant".into()),
                        ("model", b.get("model").cloned().unwrap_or(Value::Null)),
                        ("content", Value::Array(content)),
                        ("stop_reason", stop.into()),
                        ("usage", obj(vec![("input_tokens", Value::Int(11)), ("output_tokens", Value::Int(5))])),
                    ]),
                )
            };
            let text_block = |t: String| obj(vec![("type", "text".into()), ("text", t.into())]);
            let has_tools = b.get("tools").and_then(|t| t.as_array()).map(|t| !t.is_empty()).unwrap_or(false);
            let results: Vec<String> = blocks(&last)
                .iter()
                .filter(|x| x.get("type").map(|t| t.to_string()).as_deref() == Some("tool_result"))
                .map(|x| x.get("content").map(|c| c.to_string()).unwrap_or_default())
                .collect();
            if has_tools && !results.is_empty() {
                return reply(vec![text_block(format!("Done: {}", results.join(" | ")))], "end_turn");
            }
            if has_tools {
                let calls = scripted_tool_calls(&text_of(&last));
                if !calls.is_empty() {
                    let uses: Vec<Value> = calls
                        .iter()
                        .enumerate()
                        .map(|(i, (n, a))| obj(vec![("type", "tool_use".into()), ("id", format!("toolu_{}", i + 1).into()), ("name", n.as_str().into()), ("input", a.clone())]))
                        .collect();
                    return reply(uses, "tool_use");
                }
            }
            let system = b.get("system").map(|s| format!(" [{}]", s)).unwrap_or_default();
            reply(vec![text_block(format!("Echo: {}{}", text_of(&last), system))], "end_turn")
        });
        AnthropicMock { server }
    }

    /// Base url to use as `baseUrl`.
    pub fn base_url(&self) -> String {
        self.server.url()
    }
}

// ---------------------------------------------------------------------------
// GraphQL
// ---------------------------------------------------------------------------

/// GraphQL endpoint mock at `/graphql` with a tiny schema: `books { id name author }`,
/// `book(id: ID)` and `addBook(name, author)`; optional bearer token
/// [`GraphqlMock::TOKEN`] (required when `require_token` is true).
pub struct GraphqlMock {
    pub server: MockServer,
}

impl GraphqlMock {
    pub const TOKEN: &'static str = "gql-token";

    pub fn start(require_token: bool) -> GraphqlMock {
        let books = Arc::new(Mutex::new(vec![("1", "Camel in Action", "Claus"), ("2", "Enterprise Integration Patterns", "Gregor")].into_iter().map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string())).collect::<Vec<_>>()));
        let server = MockServer::start(move |r| {
            if r.path != "/graphql" {
                return HttpResponse::text(404, "Not Found");
            }
            if require_token && bearer(r).as_deref() != Some(Self::TOKEN) {
                return json(401, &obj(vec![("errors", Value::Array(vec![obj(vec![("message", "Unauthorized".into())])]))]));
            }
            if r.method != "POST" || !is_json(r) {
                return json(400, &obj(vec![("errors", Value::Array(vec![obj(vec![("message", "POST application/json expected".into())])]))]));
            }
            let b = r.json();
            let Some(q) = b.get("query").map(|q| q.to_string()) else {
                return json(400, &obj(vec![("errors", Value::Array(vec![obj(vec![("message", "Must provide query string.".into())])]))]));
            };
            let vars = b.get("variables").cloned().unwrap_or(Value::Null);
            let mut books = books.lock().unwrap();
            let book = |b: &(String, String, String)| obj(vec![("id", b.0.clone().into()), ("name", b.1.clone().into()), ("author", b.2.clone().into())]);
            let mut data: ObjectMap = ObjectMap::new();
            if q.contains("addBook") {
                let name = vars.get("name").map(|v| v.to_string()).unwrap_or_default();
                let author = vars.get("author").map(|v| v.to_string()).unwrap_or_default();
                let id = (books.len() + 1).to_string();
                books.push((id, name, author));
                data.insert("addBook".into(), book(books.last().unwrap()));
            } else if q.contains("book(") {
                let id = vars.get("id").map(|v| v.to_string()).unwrap_or_default();
                data.insert("book".into(), books.iter().find(|b| b.0 == id).map(book).unwrap_or(Value::Null));
            } else if q.contains("books") {
                data.insert("books".into(), Value::Array(books.iter().map(book).collect()));
            } else {
                return json(200, &obj(vec![("errors", Value::Array(vec![obj(vec![("message", format!("Cannot query: {}", q).into())])]))]));
            }
            if let Some(op) = b.get("operationName").filter(|o| !o.is_null()) {
                data.insert("_operationName".into(), op.clone());
            }
            json(200, &obj(vec![("data", Value::Object(data))]))
        });
        GraphqlMock { server }
    }

    /// The endpoint url (`http://127.0.0.1:port/graphql`).
    pub fn endpoint(&self) -> String {
        format!("{}/graphql", self.server.url())
    }
}

// ---------------------------------------------------------------------------
// Feeds
// ---------------------------------------------------------------------------

/// Serves RSS / Atom documents set with [`FeedMock::set`] at their path.
pub struct FeedMock {
    pub server: MockServer,
    docs: Arc<Mutex<BTreeMap<String, String>>>,
}

impl FeedMock {
    pub fn start() -> FeedMock {
        let docs: Arc<Mutex<BTreeMap<String, String>>> = Arc::new(Mutex::new(BTreeMap::new()));
        let d = docs.clone();
        let server = MockServer::start(move |r| match d.lock().unwrap().get(&r.path) {
            Some(x) => HttpResponse::new(200).with_header("Content-Type", "application/xml").with_body(x.clone().into_bytes()),
            None => HttpResponse::text(404, "Not Found"),
        });
        FeedMock { server, docs }
    }

    /// Set the document served at `path`.
    pub fn set(&self, path: &str, xml: &str) {
        self.docs.lock().unwrap().insert(path.to_string(), xml.to_string());
    }

    /// Url of a path.
    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.server.url(), path)
    }
}

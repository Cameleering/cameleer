//! Per-service mock servers.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use camel_core::value::{ObjectMap, Value};
use camel_http::HttpResponse;

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
    pub fn deliver_to_webhook(&self, update: &Value) -> camel_core::error::Result<u16> {
        let url = self.webhook().ok_or_else(|| camel_core::error::CamelError::illegal_state("no webhook registered"))?;
        let r = camel_http::HttpClient::new().post(&url, "application/json", update.to_json().as_bytes())?;
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
/// [`TwilioMock::AUTH_TOKEN`].
pub struct TwilioMock {
    pub server: MockServer,
    messages: Arc<Mutex<Vec<Value>>>,
}

impl TwilioMock {
    pub const ACCOUNT_SID: &'static str = "ACtest0000000000000000000000000000";
    pub const AUTH_TOKEN: &'static str = "twilio-secret";

    pub fn start() -> TwilioMock {
        let messages: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let ms = messages.clone();
        let server = MockServer::start(move |r| {
            let err = |status: u16, code: i64, m: &str| json(status, &obj(vec![("code", Value::Int(code)), ("message", m.into()), ("status", Value::Int(status as i64))]));
            if basic(r) != Some((Self::ACCOUNT_SID.to_string(), Self::AUTH_TOKEN.to_string())) {
                return err(401, 20003, "Authenticate");
            }
            let base = format!("/2010-04-01/Accounts/{}/Messages", Self::ACCOUNT_SID);
            let mut msgs = ms.lock().unwrap();
            if r.path == format!("{}.json", base) {
                match r.method.as_str() {
                    "POST" => {
                        if !r.header("Content-Type").map(|c| c.starts_with("application/x-www-form-urlencoded")).unwrap_or(false) {
                            return err(400, 20001, "Content-Type must be application/x-www-form-urlencoded");
                        }
                        let (to, from, body) = (r.form_param("To"), r.form_param("From"), r.form_param("Body"));
                        let Some(to) = to else { return err(400, 21604, "A 'To' phone number is required.") };
                        if from.is_none() && r.form_param("MessagingServiceSid").is_none() {
                            return err(400, 21603, "A 'From' phone number is required.");
                        }
                        if body.is_none() && r.form_param("MediaUrl").is_none() {
                            return err(400, 21602, "Message body is required.");
                        }
                        let sid = format!("SM{:032}", msgs.len() + 1);
                        let m = obj(vec![
                            ("sid", sid.into()),
                            ("account_sid", Self::ACCOUNT_SID.into()),
                            ("to", to.into()),
                            ("from", from.map(Value::String).unwrap_or(Value::Null)),
                            ("body", body.map(Value::String).unwrap_or(Value::Null)),
                            ("status", "queued".into()),
                        ]);
                        msgs.push(m.clone());
                        json(201, &m)
                    }
                    "GET" => json(200, &obj(vec![("messages", Value::Array(msgs.clone()))])),
                    _ => err(405, 20004, "Method not allowed"),
                }
            } else if let Some(sid) = r.path.strip_prefix(&format!("{}/", base)).and_then(|s| s.strip_suffix(".json")) {
                match msgs.iter().find(|m| m.get("sid").map(|s| s.to_string() == sid).unwrap_or(false)) {
                    Some(m) => json(200, m),
                    None => err(404, 20404, "The requested resource was not found"),
                }
            } else {
                err(404, 20404, "The requested resource was not found")
            }
        });
        TwilioMock { server, messages }
    }

    pub fn url(&self) -> String {
        self.server.url()
    }

    pub fn messages(&self) -> Vec<Value> {
        self.messages.lock().unwrap().clone()
    }
}

// ---------------------------------------------------------------------------
// Jira
// ---------------------------------------------------------------------------

#[derive(Default)]
struct JiraState {
    issues: Vec<Value>,
    comments: Vec<(String, Value)>,
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
}

// ---------------------------------------------------------------------------
// OpenAI
// ---------------------------------------------------------------------------

/// OpenAI-compatible API mock (API key [`OpenAiMock::API_KEY`], base url `url() + "/v1"`).
/// Chat completions answer `Echo: <last user message>`.
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
                    if msgs.iter().any(|m| m.get("role").is_none() || m.get("content").is_none()) {
                        return err(400, "each message requires 'role' and 'content'", "invalid_request_error");
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

//! `jira:` component (Camel `camel-jira`) over the Jira REST API v2.
//!
//! `jira:type?jiraUrl=https://jira.example.com&username=bob&password=token`
//!
//! Producers:
//! * `addIssue`: headers `ProjectKey`, `IssueTypeName` or `IssueTypeId`, `IssueSummary`,
//!   optional `IssueAssignee` / `IssueAssigneeId`, `IssuePriorityName` / `IssuePriorityId`,
//!   `IssueComponents` (comma separated); body = description. The body becomes the created
//!   issue (`id`, `key`, `self`) and header `IssueKey` its key.
//! * `addComment`: header `IssueKey`, body = comment; the body becomes the created comment.
//! * `deleteIssue`: header `IssueKey`.
//! * `fetchIssue` (extension): header `IssueKey`; the body becomes the issue.
//! * `searchIssues` (extension): JQL from the `jql` option, the `CamelJiraJql` header or the
//!   body; the body becomes the list of issues (`maxResults`, default 50).
//! * `updateIssue`: header `IssueKey`, optional `IssueSummary`, `IssueTypeName`/`IssueTypeId`,
//!   `IssueAssignee`/`IssueAssigneeId`, `IssuePriorityName`/`IssuePriorityId`,
//!   `IssueComponents`; a non-null body becomes the description.
//! * `transitionIssue`: headers `IssueKey`, `IssueTransitionId`; a non-null body is added as
//!   a comment of the transition.
//! * `attach`: header `IssueKey`; body = file content (bytes / text), file name from
//!   `CamelFileName` (default `attachment`). Uploads multipart with `X-Atlassian-Token:
//!   no-check`; the body becomes the list of created attachments.
//! * `watchers`: header `IssueKey`, `IssueWatchersAdd` / `IssueWatchersRemove` (comma
//!   separated text or a list of user names); the body becomes the current watcher list.
//! * `addIssueLink`: headers `ParentIssueKey`, `ChildIssueKey`, `linkType`; a non-null body
//!   becomes the link comment.
//! * `addWorkLog`: headers `IssueKey`, `minutesSpent`; body = work log comment.
//! * `fetchComments`: header `IssueKey`; the body becomes the list of comments.
//!
//! Consumers (scheduled poll options apply, `delay` default 6 s, `jql` required):
//! * `newIssues`: polls `jql` ordered by key and delivers issues created after the consumer
//!   started (the first poll records the newest issue id); body = issue object, header
//!   `IssueKey`.
//! * `newComments`: comments added to the issues of `jql` after the consumer started; body =
//!   comment object, header `IssueKey`.
//! * `watchUpdates`: changes of `watchedFields` (default `Status,Priority`, field names
//!   case-insensitive) of the issues of `jql`; one exchange per changed field with the new
//!   value as body and headers `IssueKey`, `IssueChanged` (field name),
//!   `IssueWatchedIssues` (keys of the watched issues). `sendOnlyUpdatedField=false` sends
//!   the whole issue instead.
//!
//! Authentication: `username` + `password` (Basic, also API tokens) or `accessToken`
//! (Bearer, personal access tokens). `jiraUrl` is the base url.

use std::any::Any;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cameleer_core::context::{CamelContext, WeakContext};
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use cameleer_core::util::Parameters;
use cameleer_core::value::Value;

use crate::rest::{body_text, emit, header_str, join_url, poll_config, poll_consumer, query, required, Req, Rest};

pub const PROJECT_KEY: &str = "ProjectKey";
pub const ISSUE_TYPE_NAME: &str = "IssueTypeName";
pub const ISSUE_TYPE_ID: &str = "IssueTypeId";
pub const ISSUE_SUMMARY: &str = "IssueSummary";
pub const ISSUE_ASSIGNEE: &str = "IssueAssignee";
pub const ISSUE_ASSIGNEE_ID: &str = "IssueAssigneeId";
pub const ISSUE_PRIORITY_NAME: &str = "IssuePriorityName";
pub const ISSUE_PRIORITY_ID: &str = "IssuePriorityId";
pub const ISSUE_COMPONENTS: &str = "IssueComponents";
pub const ISSUE_KEY: &str = "IssueKey";
pub const JIRA_JQL: &str = "CamelJiraJql";
pub const ISSUE_TRANSITION_ID: &str = "IssueTransitionId";
pub const ISSUE_WATCHERS_ADD: &str = "IssueWatchersAdd";
pub const ISSUE_WATCHERS_REMOVE: &str = "IssueWatchersRemove";
pub const PARENT_ISSUE_KEY: &str = "ParentIssueKey";
pub const CHILD_ISSUE_KEY: &str = "ChildIssueKey";
pub const LINK_TYPE: &str = "linkType";
pub const MINUTES_SPENT: &str = "minutesSpent";
pub const ISSUE_CHANGED: &str = "IssueChanged";
pub const ISSUE_WATCHED_ISSUES: &str = "IssueWatchedIssues";

const CONSUMER_TYPES: &[&str] = &["newIssues", "newComments", "watchUpdates"];

/// The `jira` component.
#[derive(Default)]
pub struct JiraComponent;

#[derive(Clone, Debug)]
enum Auth {
    Basic(String, String),
    Bearer(String),
}

#[derive(Clone, Debug)]
struct Config {
    ty: String,
    url: String,
    auth: Auth,
    jql: Option<String>,
    max_results: usize,
    watched_fields: Vec<String>,
    send_only_updated_field: bool,
}

impl Config {
    fn req(&self, r: Req) -> Req {
        match &self.auth {
            Auth::Basic(u, p) => r.basic(u, p),
            Auth::Bearer(t) => r.bearer(t),
        }
    }

    fn api(&self, path: &str) -> String {
        join_url(&self.url, &format!("/rest/api/2{}", path))
    }
}

impl Component for JiraComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let ty = remaining.trim().to_string();
        const TYPES: &[&str] = &[
            "addIssue", "addComment", "deleteIssue", "fetchIssue", "searchIssues", "updateIssue", "transitionIssue", "attach", "watchers", "addIssueLink", "addWorkLog", "fetchComments", "newIssues", "newComments", "watchUpdates",
        ];
        if !TYPES.contains(&ty.as_str()) {
            return Err(CamelError::illegal_argument(format!("jira: unsupported type '{}' (one of {})", ty, TYPES.join(", "))));
        }
        let url = required(params, "jiraUrl", "jira")?;
        let auth = match params.take("accessToken").filter(|s| !s.is_empty()) {
            Some(t) => Auth::Bearer(t),
            None => Auth::Basic(required(params, "username", "jira")?, required(params, "password", "jira")?),
        };
        for k in ["consumerKey", "privateKey", "verificationCode", "lazyStartProducer"] {
            params.take(k);
        }
        let watched_fields = params.take("watchedFields").unwrap_or_else(|| "Status,Priority".into()).split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        let send_only_updated_field = params.take_bool("sendOnlyUpdatedField", true)?;
        let cfg = Config { ty, url, auth, jql: params.take("jql").filter(|s| !s.is_empty()), max_results: params.take_usize("maxResults", 50)?, watched_fields, send_only_updated_field };
        let poll = poll_config(params, Duration::from_secs(6))?;
        Ok(Arc::new(JiraEndpoint { uri: uri.to_string(), cfg, poll, rest: Rest::default(), ctx: ctx.downgrade() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A jira endpoint.
pub struct JiraEndpoint {
    uri: String,
    cfg: Config,
    poll: cameleer_core::component::scheduled::ScheduledPollConfig,
    rest: Rest,
    ctx: WeakContext,
}

fn search(rest: &Rest, cfg: &Config, jql: &str) -> Result<Vec<Value>> {
    let url = format!("{}?{}", cfg.api("/search"), query(&[("jql", jql.to_string()), ("maxResults", cfg.max_results.to_string())]));
    let v = rest.json(cfg.req(Req::get(url)))?;
    Ok(v.get("issues").and_then(|i| i.as_array()).cloned().unwrap_or_default())
}

fn comments(rest: &Rest, cfg: &Config, key: &str) -> Result<Vec<Value>> {
    let v = rest.json(cfg.req(Req::get(cfg.api(&format!("/issue/{}/comment", key)))))?;
    Ok(v.get("comments").and_then(|c| c.as_array()).cloned().unwrap_or_default())
}

/// Value of a watched field (case-insensitive name; objects reduce to their `name`).
fn field_value(issue: &Value, field: &str) -> Value {
    let Some(fields) = issue.get("fields").and_then(|f| f.as_object()) else { return Value::Null };
    let v = fields.iter().find(|(k, _)| k.eq_ignore_ascii_case(field)).map(|(_, v)| v.clone()).unwrap_or(Value::Null);
    match v.get("name") {
        Some(n) if v.as_object().is_some() => n.clone(),
        _ => v,
    }
}

fn list_header(ex: &Exchange, name: &str) -> Vec<String> {
    match ex.message.headers.get(name) {
        Some(Value::Array(a)) => a.iter().map(|v| v.to_string()).filter(|s| !s.is_empty()).collect(),
        Some(Value::Null) | None => Vec::new(),
        Some(v) => v.to_string().split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
    }
}

fn issue_id(v: &Value) -> i64 {
    v.get("id").and_then(|i| i.to_string().parse().ok()).unwrap_or(0)
}

impl Endpoint for JiraEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        if CONSUMER_TYPES.contains(&self.cfg.ty.as_str()) {
            return Err(CamelError::illegal_argument(format!("jira: {} is a consumer type", self.cfg.ty)));
        }
        Ok(Arc::new(JiraProducer { cfg: self.cfg.clone(), rest: self.rest.clone() }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        if !CONSUMER_TYPES.contains(&self.cfg.ty.as_str()) {
            return Err(CamelError::illegal_argument(format!("jira: '{}' does not support consumers (use newIssues, newComments or watchUpdates)", self.cfg.ty)));
        }
        let jql = self.cfg.jql.clone().ok_or_else(|| CamelError::illegal_argument(format!("jira: {} requires the jql option", self.cfg.ty)))?;
        let jql = if jql.to_ascii_lowercase().contains("order by") { jql } else { format!("{} ORDER BY key desc", jql) };
        let (cfg, rest, ctx) = (self.cfg.clone(), self.rest.clone(), self.ctx.clone());
        match cfg.ty.as_str() {
            "newIssues" => {
                let last: Arc<Mutex<Option<i64>>> = Arc::new(Mutex::new(None));
                Ok(Arc::new(poll_consumer("jira", self.poll.clone(), move || {
                    let issues = search(&rest, &cfg, &jql)?;
                    let max = issues.iter().map(issue_id).max().unwrap_or(0);
                    let mut l = last.lock().unwrap();
                    let Some(prev) = *l else {
                        *l = Some(max);
                        return Ok(0);
                    };
                    let mut fresh: Vec<Value> = issues.into_iter().filter(|i| issue_id(i) > prev).collect();
                    fresh.sort_by_key(issue_id);
                    *l = Some(prev.max(max));
                    drop(l);
                    let n = fresh.len();
                    for i in fresh {
                        let key = i.get("key").cloned().unwrap_or(Value::Null);
                        emit(&ctx, processor.as_ref(), "jira", i, vec![(ISSUE_KEY, key)]);
                    }
                    Ok(n)
                })))
            }
            "newComments" => {
                let last: Arc<Mutex<Option<i64>>> = Arc::new(Mutex::new(None));
                Ok(Arc::new(poll_consumer("jira", self.poll.clone(), move || {
                    let issues = search(&rest, &cfg, &jql)?;
                    let mut all: Vec<(Value, Value)> = Vec::new();
                    for i in &issues {
                        let key = i.get("key").map(|k| k.to_string()).unwrap_or_default();
                        for c in comments(&rest, &cfg, &key)? {
                            all.push((Value::String(key.clone()), c));
                        }
                    }
                    let max = all.iter().map(|(_, c)| issue_id(c)).max().unwrap_or(0);
                    let mut l = last.lock().unwrap();
                    let Some(prev) = *l else {
                        *l = Some(max);
                        return Ok(0);
                    };
                    let mut fresh: Vec<(Value, Value)> = all.into_iter().filter(|(_, c)| issue_id(c) > prev).collect();
                    fresh.sort_by_key(|(_, c)| issue_id(c));
                    *l = Some(prev.max(max));
                    drop(l);
                    let n = fresh.len();
                    for (key, c) in fresh {
                        emit(&ctx, processor.as_ref(), "jira", c, vec![(ISSUE_KEY, key)]);
                    }
                    Ok(n)
                })))
            }
            _ => {
                // watchUpdates
                type Snapshot = std::collections::HashMap<String, Vec<Value>>;
                let snapshot: Arc<Mutex<Option<Snapshot>>> = Arc::new(Mutex::new(None));
                Ok(Arc::new(poll_consumer("jira", self.poll.clone(), move || {
                    let issues = search(&rest, &cfg, &jql)?;
                    let keys: Vec<Value> = issues.iter().filter_map(|i| i.get("key").cloned()).collect();
                    let mut snap = snapshot.lock().unwrap();
                    let first = snap.is_none();
                    let map = snap.get_or_insert_with(Default::default);
                    let mut events: Vec<(Value, String, Value, Value)> = Vec::new();
                    for i in &issues {
                        let key = i.get("key").map(|k| k.to_string()).unwrap_or_default();
                        let vals: Vec<Value> = cfg.watched_fields.iter().map(|f| field_value(i, f)).collect();
                        if let Some(old) = map.get(&key) {
                            for (idx, f) in cfg.watched_fields.iter().enumerate() {
                                if old.get(idx) != vals.get(idx) {
                                    events.push((Value::String(key.clone()), f.clone(), vals[idx].clone(), i.clone()));
                                }
                            }
                        }
                        map.insert(key, vals);
                    }
                    drop(snap);
                    if first {
                        return Ok(0);
                    }
                    let n = events.len();
                    for (key, field, value, issue) in events {
                        let body = if cfg.send_only_updated_field { value } else { issue };
                        emit(&ctx, processor.as_ref(), "jira", body, vec![(ISSUE_KEY, key), (ISSUE_CHANGED, Value::String(field)), (ISSUE_WATCHED_ISSUES, Value::Array(keys.clone()))]);
                    }
                    Ok(n)
                })))
            }
        }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct JiraProducer {
    cfg: Config,
    rest: Rest,
}

impl Service for JiraProducer {}

fn need(ex: &Exchange, h: &str) -> Result<String> {
    header_str(ex, h).ok_or_else(|| CamelError::illegal_argument(format!("jira: missing header {}", h)))
}

impl Processor for JiraProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let cfg = &self.cfg;
        match cfg.ty.as_str() {
            "addIssue" => {
                let mut fields = vec![
                    ("project", Value::object([("key", need(ex, PROJECT_KEY)?)])),
                    ("summary", Value::String(need(ex, ISSUE_SUMMARY)?)),
                    (
                        "issuetype",
                        match (header_str(ex, ISSUE_TYPE_ID), header_str(ex, ISSUE_TYPE_NAME)) {
                            (Some(id), _) => Value::object([("id", id)]),
                            (None, Some(n)) => Value::object([("name", n)]),
                            _ => return Err(CamelError::illegal_argument("jira: addIssue requires the IssueTypeId or IssueTypeName header")),
                        },
                    ),
                ];
                if let Some(d) = body_text(ex) {
                    fields.push(("description", Value::String(d)));
                }
                if let Some(a) = header_str(ex, ISSUE_ASSIGNEE_ID) {
                    fields.push(("assignee", Value::object([("accountId", a)])));
                } else if let Some(a) = header_str(ex, ISSUE_ASSIGNEE) {
                    fields.push(("assignee", Value::object([("name", a)])));
                }
                if let Some(p) = header_str(ex, ISSUE_PRIORITY_ID) {
                    fields.push(("priority", Value::object([("id", p)])));
                } else if let Some(p) = header_str(ex, ISSUE_PRIORITY_NAME) {
                    fields.push(("priority", Value::object([("name", p)])));
                }
                if let Some(c) = header_str(ex, ISSUE_COMPONENTS) {
                    fields.push(("components", Value::Array(c.split(',').map(|n| Value::object([("name", n.trim())])).collect())));
                }
                let created = self.rest.json(cfg.req(Req::post(cfg.api("/issue")).json(&Value::object([("fields", Value::object(fields))]))))?;
                if let Some(k) = created.get("key") {
                    ex.message.headers.set(ISSUE_KEY, k.clone());
                }
                ex.message.body = created;
            }
            "addComment" => {
                let key = need(ex, ISSUE_KEY)?;
                let text = body_text(ex).ok_or_else(|| CamelError::illegal_argument("jira: addComment requires a body"))?;
                ex.message.body = self.rest.json(cfg.req(Req::post(cfg.api(&format!("/issue/{}/comment", key))).json(&Value::object([("body", text)]))))?;
            }
            "deleteIssue" => {
                let key = need(ex, ISSUE_KEY)?;
                self.rest.send_ok(cfg.req(Req::new("DELETE", cfg.api(&format!("/issue/{}", key)))))?;
            }
            "fetchIssue" => {
                let key = need(ex, ISSUE_KEY)?;
                ex.message.body = self.rest.json(cfg.req(Req::get(cfg.api(&format!("/issue/{}", key)))))?;
            }
            "searchIssues" => {
                let jql = header_str(ex, JIRA_JQL).or_else(|| cfg.jql.clone()).or_else(|| body_text(ex)).ok_or_else(|| CamelError::illegal_argument("jira: searchIssues requires a JQL query"))?;
                ex.message.body = Value::Array(search(&self.rest, cfg, &jql)?);
            }
            "updateIssue" => {
                let key = need(ex, ISSUE_KEY)?;
                let mut fields: Vec<(&str, Value)> = Vec::new();
                if let Some(v) = header_str(ex, ISSUE_SUMMARY) {
                    fields.push(("summary", Value::String(v)));
                }
                match (header_str(ex, ISSUE_TYPE_ID), header_str(ex, ISSUE_TYPE_NAME)) {
                    (Some(id), _) => fields.push(("issuetype", Value::object([("id", id)]))),
                    (None, Some(n)) => fields.push(("issuetype", Value::object([("name", n)]))),
                    _ => {}
                }
                if let Some(d) = body_text(ex) {
                    fields.push(("description", Value::String(d)));
                }
                if let Some(a) = header_str(ex, ISSUE_ASSIGNEE_ID) {
                    fields.push(("assignee", Value::object([("accountId", a)])));
                } else if let Some(a) = header_str(ex, ISSUE_ASSIGNEE) {
                    fields.push(("assignee", Value::object([("name", a)])));
                }
                if let Some(p) = header_str(ex, ISSUE_PRIORITY_ID) {
                    fields.push(("priority", Value::object([("id", p)])));
                } else if let Some(p) = header_str(ex, ISSUE_PRIORITY_NAME) {
                    fields.push(("priority", Value::object([("name", p)])));
                }
                if let Some(c) = header_str(ex, ISSUE_COMPONENTS) {
                    fields.push(("components", Value::Array(c.split(',').map(|n| Value::object([("name", n.trim())])).collect())));
                }
                self.rest.send_ok(cfg.req(Req::new("PUT", cfg.api(&format!("/issue/{}", key))).json(&Value::object([("fields", Value::object(fields))]))))?;
            }
            "transitionIssue" => {
                let key = need(ex, ISSUE_KEY)?;
                let id = need(ex, ISSUE_TRANSITION_ID)?;
                let mut body = vec![("transition", Value::object([("id", id)]))];
                if let Some(c) = body_text(ex) {
                    body.push(("update", Value::object([("comment", Value::Array(vec![Value::object([("add", Value::object([("body", c)]))])]))])));
                }
                self.rest.send_ok(cfg.req(Req::post(cfg.api(&format!("/issue/{}/transitions", key))).json(&Value::object(body))))?;
            }
            "attach" => {
                let key = need(ex, ISSUE_KEY)?;
                let data = match &ex.message.body {
                    Value::Bytes(b) => b.clone(),
                    Value::Null => return Err(CamelError::illegal_argument("jira: attach requires a body")),
                    other => other.to_string().into_bytes(),
                };
                let name = header_str(ex, "CamelFileName").map(|n| n.rsplit('/').next().unwrap_or(&n).to_string()).unwrap_or_else(|| "attachment".into());
                let part = crate::rest::Part { name: "file".into(), filename: Some(name), content_type: Some("application/octet-stream".into()), data };
                ex.message.body = self.rest.json(cfg.req(Req::post(cfg.api(&format!("/issue/{}/attachments", key))).header("X-Atlassian-Token", "no-check").multipart(&[part])))?;
            }
            "watchers" => {
                let key = need(ex, ISSUE_KEY)?;
                let url = cfg.api(&format!("/issue/{}/watchers", key));
                for u in list_header(ex, ISSUE_WATCHERS_ADD) {
                    self.rest.send_ok(cfg.req(Req::post(url.clone()).json(&Value::String(u))))?;
                }
                for u in list_header(ex, ISSUE_WATCHERS_REMOVE) {
                    self.rest.send_ok(cfg.req(Req::new("DELETE", format!("{}?{}", url, query(&[("username", u)])))))?;
                }
                let w = self.rest.json(cfg.req(Req::get(url)))?;
                ex.message.body = w.get("watchers").cloned().unwrap_or(Value::Array(vec![]));
            }
            "addIssueLink" => {
                let parent = need(ex, PARENT_ISSUE_KEY)?;
                let child = need(ex, CHILD_ISSUE_KEY)?;
                let ty = need(ex, LINK_TYPE)?;
                let mut body = vec![("type", Value::object([("name", ty)])), ("inwardIssue", Value::object([("key", parent)])), ("outwardIssue", Value::object([("key", child)]))];
                if let Some(c) = body_text(ex) {
                    body.push(("comment", Value::object([("body", c)])));
                }
                self.rest.send_ok(cfg.req(Req::post(cfg.api("/issueLink")).json(&Value::object(body))))?;
            }
            "addWorkLog" => {
                let key = need(ex, ISSUE_KEY)?;
                let minutes: i64 = need(ex, MINUTES_SPENT)?.trim().parse().map_err(|_| CamelError::illegal_argument("jira: minutesSpent must be a number"))?;
                let mut body = vec![("timeSpentSeconds", Value::Int(minutes * 60))];
                if let Some(c) = body_text(ex) {
                    body.push(("comment", Value::String(c)));
                }
                ex.message.body = self.rest.json(cfg.req(Req::post(cfg.api(&format!("/issue/{}/worklog", key))).json(&Value::object(body))))?;
            }
            "fetchComments" => {
                let key = need(ex, ISSUE_KEY)?;
                ex.message.body = Value::Array(comments(&self.rest, cfg, &key)?);
            }
            _ => unreachable!(),
        }
        Ok(())
    }
}

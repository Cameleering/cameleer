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
//!
//! Consumer `newIssues`: polls `jql` (required) ordered by key and delivers issues created
//! after the consumer started (the first poll records the newest issue id); body = issue
//! object, header `IssueKey`. Scheduled poll options apply (`delay`, default 6 s).
//!
//! Authentication: `username` + `password` (Basic, also API tokens) or `accessToken`
//! (Bearer, personal access tokens). `jiraUrl` is the base url.

use std::any::Any;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::value::Value;

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
        const TYPES: &[&str] = &["addIssue", "addComment", "deleteIssue", "fetchIssue", "searchIssues", "newIssues"];
        if !TYPES.contains(&ty.as_str()) {
            return Err(CamelError::illegal_argument(format!("jira: unsupported type '{}' (one of {})", ty, TYPES.join(", "))));
        }
        let url = required(params, "jiraUrl", "jira")?;
        let auth = match params.take("accessToken").filter(|s| !s.is_empty()) {
            Some(t) => Auth::Bearer(t),
            None => Auth::Basic(required(params, "username", "jira")?, required(params, "password", "jira")?),
        };
        for k in ["consumerKey", "privateKey", "verificationCode", "sendOnlyUpdatedField", "watchedFields", "lazyStartProducer"] {
            params.take(k);
        }
        let cfg = Config { ty, url, auth, jql: params.take("jql").filter(|s| !s.is_empty()), max_results: params.take_usize("maxResults", 50)? };
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
    poll: camel_core::component::scheduled::ScheduledPollConfig,
    rest: Rest,
    ctx: WeakContext,
}

fn search(rest: &Rest, cfg: &Config, jql: &str) -> Result<Vec<Value>> {
    let url = format!("{}?{}", cfg.api("/search"), query(&[("jql", jql.to_string()), ("maxResults", cfg.max_results.to_string())]));
    let v = rest.json(cfg.req(Req::get(url)))?;
    Ok(v.get("issues").and_then(|i| i.as_array()).cloned().unwrap_or_default())
}

fn issue_id(v: &Value) -> i64 {
    v.get("id").and_then(|i| i.to_string().parse().ok()).unwrap_or(0)
}

impl Endpoint for JiraEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        if self.cfg.ty == "newIssues" {
            return Err(CamelError::illegal_argument("jira: newIssues is a consumer type"));
        }
        Ok(Arc::new(JiraProducer { cfg: self.cfg.clone(), rest: self.rest.clone() }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        if self.cfg.ty != "newIssues" {
            return Err(CamelError::illegal_argument(format!("jira: '{}' does not support consumers (use newIssues)", self.cfg.ty)));
        }
        let jql = self.cfg.jql.clone().ok_or_else(|| CamelError::illegal_argument("jira: newIssues requires the jql option"))?;
        let jql = if jql.to_ascii_lowercase().contains("order by") { jql } else { format!("{} ORDER BY key desc", jql) };
        let (cfg, rest, ctx) = (self.cfg.clone(), self.rest.clone(), self.ctx.clone());
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
            _ => unreachable!(),
        }
        Ok(())
    }
}

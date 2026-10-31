//! `github:` component (Camel `camel-github`) over the GitHub REST API.
//!
//! `github:type[/branch]?repoOwner=apache&repoName=camel&oauthToken=...`
//!
//! Consumers (scheduled polling, `delay` default 10 s):
//! * `pullRequest`: new open pull requests (the first poll records the existing ones);
//!   body = pull request object, header `GitHubPullRequest` = number.
//! * `issue` (rust-camel extension): new open issues (pull requests excluded); header
//!   `GitHubIssue` = number.
//! * `commit/<branch>`: new commits of a branch (`startingSha=last` (default) skips existing
//!   commits, `beginning` delivers all, oldest first); body = commit message, headers
//!   `GitHubCommitSha`, `GitHubCommitAuthor`, `GitHubCommitCommitter`, `GitHubCommitUrl`.
//! * `tag`: new tags; body = tag object.
//!
//! Producers:
//! * `createIssue`: title from header `GitHubIssueTitle`, body = issue text; the body becomes
//!   the created issue object.
//! * `pullRequestComment`: comment the pull request of header `GitHubPullRequest` (a reply to
//!   review comment `GitHubInResponseTo` when set); body becomes the created comment.
//! * `issueComment` (extension): comment issue `GitHubIssue`.
//! * `pullRequestState`: set the commit status of `GitHubPullRequestHeadCommitSHA` (option
//!   `state`: success / error / failure / pending, `targetUrl`; body = description).
//!
//! Webhooks: `webhook:github:pullRequest?...` creates a repository hook (`hookEvents`, default
//! `push,pull_request,issues`) and removes it on stop; incoming payloads become the body
//! (parsed JSON) with header `GitHubEvent` from `X-GitHub-Event`.
//!
//! `serverUrl` (extension, default `https://api.github.com`) overrides the API base url.

use std::any::Any;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::value::Value;

use crate::rest::{body_text, emit, header_str, join_url, poll_config, poll_consumer, required, Req, Rest};
use crate::webhook::WebhookCapable;

pub const GITHUB_PULL_REQUEST: &str = "GitHubPullRequest";
pub const GITHUB_ISSUE: &str = "GitHubIssue";
pub const GITHUB_IN_RESPONSE_TO: &str = "GitHubInResponseTo";
pub const GITHUB_PULL_REQUEST_HEAD_COMMIT_SHA: &str = "GitHubPullRequestHeadCommitSHA";
pub const GITHUB_ISSUE_TITLE: &str = "GitHubIssueTitle";
pub const GITHUB_COMMIT_SHA: &str = "GitHubCommitSha";
pub const GITHUB_COMMIT_AUTHOR: &str = "GitHubCommitAuthor";
pub const GITHUB_COMMIT_COMMITTER: &str = "GitHubCommitCommitter";
pub const GITHUB_COMMIT_URL: &str = "GitHubCommitUrl";
pub const GITHUB_EVENT: &str = "GitHubEvent";

/// The `github` component.
#[derive(Default)]
pub struct GithubComponent;

#[derive(Clone, Debug)]
struct Config {
    ty: String,
    branch: Option<String>,
    owner: String,
    repo: String,
    token: Option<String>,
    server_url: String,
    starting_sha: String,
    state: Option<String>,
    target_url: Option<String>,
    hook_events: Vec<String>,
}

impl Config {
    fn repo_url(&self, path: &str) -> String {
        join_url(&self.server_url, &format!("/repos/{}/{}{}", self.owner, self.repo, path))
    }

    fn req(&self, r: Req) -> Req {
        let r = r.header("Accept", "application/vnd.github+json").header("X-GitHub-Api-Version", "2022-11-28");
        match &self.token {
            Some(t) => r.bearer(t),
            None => r,
        }
    }
}

impl Component for GithubComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let (ty, branch) = match remaining.trim().split_once('/') {
            Some((t, b)) => (t.to_string(), Some(b.to_string())),
            None => (remaining.trim().to_string(), None),
        };
        const TYPES: &[&str] = &["pullRequest", "issue", "commit", "tag", "createIssue", "pullRequestComment", "issueComment", "pullRequestState", "event"];
        if !TYPES.contains(&ty.as_str()) {
            return Err(CamelError::illegal_argument(format!("github: unknown type '{}' (one of {})", ty, TYPES.join(", "))));
        }
        let cfg = Config {
            ty,
            branch,
            owner: required(params, "repoOwner", "github")?,
            repo: required(params, "repoName", "github")?,
            token: params.take("oauthToken").filter(|s| !s.is_empty()),
            server_url: params.take("serverUrl").unwrap_or_else(|| "https://api.github.com".into()),
            starting_sha: params.take("startingSha").unwrap_or_else(|| "last".into()),
            state: params.take("state"),
            target_url: params.take("targetUrl"),
            hook_events: params.take("hookEvents").unwrap_or_else(|| "push,pull_request,issues".into()).split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        };
        for k in ["encoding", "eventFetchStrategy", "lazyStartProducer"] {
            params.take(k);
        }
        let poll = poll_config(params, Duration::from_secs(10))?;
        Ok(Arc::new(GithubEndpoint { uri: uri.to_string(), cfg, poll, rest: Rest::default(), ctx: ctx.downgrade(), hook_id: Mutex::new(None) }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A github endpoint.
pub struct GithubEndpoint {
    uri: String,
    cfg: Config,
    poll: camel_core::component::scheduled::ScheduledPollConfig,
    rest: Rest,
    ctx: WeakContext,
    hook_id: Mutex<Option<i64>>,
}

fn id_of(v: &Value) -> i64 {
    v.get("id").and_then(|i| i.as_i64()).unwrap_or(0)
}

impl Endpoint for GithubEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        match self.cfg.ty.as_str() {
            "createIssue" | "pullRequestComment" | "issueComment" | "pullRequestState" => Ok(Arc::new(GithubProducer { cfg: self.cfg.clone(), rest: self.rest.clone() })),
            other => Err(CamelError::illegal_argument(format!("github: '{}' is a consumer type", other))),
        }
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        let cfg = self.cfg.clone();
        let rest = self.rest.clone();
        let ctx = self.ctx.clone();
        match cfg.ty.as_str() {
            "pullRequest" | "issue" => {
                let last: Arc<Mutex<Option<i64>>> = Arc::new(Mutex::new(None));
                Ok(Arc::new(poll_consumer("github", self.poll.clone(), move || {
                    let pulls = cfg.ty == "pullRequest";
                    let url = if pulls { format!("{}?state=open&per_page=100", cfg.repo_url("/pulls")) } else { format!("{}?state=open&sort=created&direction=desc&per_page=100", cfg.repo_url("/issues")) };
                    let list = rest.json(cfg.req(Req::get(url)))?;
                    let mut items: Vec<Value> = list.as_array().cloned().unwrap_or_default();
                    if !pulls {
                        items.retain(|i| i.get("pull_request").is_none());
                    }
                    let mut l = last.lock().unwrap();
                    let max = items.iter().map(id_of).max().unwrap_or(0);
                    let Some(prev) = *l else {
                        *l = Some(max);
                        return Ok(0);
                    };
                    let mut fresh: Vec<Value> = items.into_iter().filter(|i| id_of(i) > prev).collect();
                    fresh.sort_by_key(id_of);
                    *l = Some(prev.max(max));
                    drop(l);
                    let n = fresh.len();
                    for it in fresh {
                        let num = it.get("number").cloned().unwrap_or(Value::Null);
                        emit(&ctx, processor.as_ref(), "github", it, vec![(if pulls { GITHUB_PULL_REQUEST } else { GITHUB_ISSUE }, num)]);
                    }
                    Ok(n)
                })))
            }
            "commit" => {
                let branch = cfg.branch.clone().ok_or_else(|| CamelError::illegal_argument("github: commit consumer requires a branch (github:commit/main)"))?;
                let seen: Arc<Mutex<Option<HashSet<String>>>> = Arc::new(Mutex::new(None));
                Ok(Arc::new(poll_consumer("github", self.poll.clone(), move || {
                    let url = format!("{}?sha={}&per_page=100", cfg.repo_url("/commits"), camel_core::util::url_encode(&branch));
                    let list = rest.json(cfg.req(Req::get(url)))?;
                    let commits: Vec<Value> = list.as_array().cloned().unwrap_or_default();
                    let sha = |c: &Value| c.get("sha").map(|s| s.to_string()).unwrap_or_default();
                    let mut s = seen.lock().unwrap();
                    let first = s.is_none();
                    let set = s.get_or_insert_with(HashSet::new);
                    let mut fresh: Vec<Value> = commits.iter().filter(|c| !set.contains(&sha(c))).cloned().collect();
                    for c in &commits {
                        set.insert(sha(c));
                    }
                    drop(s);
                    if first && cfg.starting_sha != "beginning" {
                        return Ok(0);
                    }
                    fresh.reverse(); // oldest first
                    let n = fresh.len();
                    for c in fresh {
                        let commit = c.get("commit").cloned().unwrap_or(Value::Null);
                        let msg = commit.get("message").cloned().unwrap_or(Value::Null);
                        let headers = vec![
                            (GITHUB_COMMIT_SHA, Value::String(sha(&c))),
                            (GITHUB_COMMIT_AUTHOR, commit.get("author").and_then(|a| a.get("name")).cloned().unwrap_or(Value::Null)),
                            (GITHUB_COMMIT_COMMITTER, commit.get("committer").and_then(|a| a.get("name")).cloned().unwrap_or(Value::Null)),
                            (GITHUB_COMMIT_URL, c.get("html_url").cloned().unwrap_or(Value::Null)),
                        ];
                        emit(&ctx, processor.as_ref(), "github", msg, headers);
                    }
                    Ok(n)
                })))
            }
            "tag" => {
                let seen: Arc<Mutex<Option<HashSet<String>>>> = Arc::new(Mutex::new(None));
                Ok(Arc::new(poll_consumer("github", self.poll.clone(), move || {
                    let list = rest.json(cfg.req(Req::get(format!("{}?per_page=100", cfg.repo_url("/tags")))))?;
                    let tags: Vec<Value> = list.as_array().cloned().unwrap_or_default();
                    let name = |t: &Value| t.get("name").map(|s| s.to_string()).unwrap_or_default();
                    let mut s = seen.lock().unwrap();
                    let first = s.is_none();
                    let set = s.get_or_insert_with(HashSet::new);
                    let mut fresh: Vec<Value> = tags.iter().filter(|t| !set.contains(&name(t))).cloned().collect();
                    for t in &tags {
                        set.insert(name(t));
                    }
                    drop(s);
                    if first {
                        return Ok(0);
                    }
                    fresh.reverse();
                    let n = fresh.len();
                    for t in fresh {
                        emit(&ctx, processor.as_ref(), "github", t, vec![]);
                    }
                    Ok(n)
                })))
            }
            other => Err(CamelError::illegal_argument(format!("github: '{}' does not support consumers (use pullRequest, issue, commit/<branch> or tag)", other))),
        }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl WebhookCapable for GithubEndpoint {
    fn register_webhook(&self, url: &str) -> Result<()> {
        let body = Value::object([
            ("name", Value::from("web")),
            ("active", Value::Bool(true)),
            ("events", Value::Array(self.cfg.hook_events.iter().map(|e| Value::from(e.as_str())).collect())),
            ("config", Value::object([("url", url), ("content_type", "json")])),
        ]);
        let hook = self.rest.json(self.cfg.req(Req::post(self.cfg.repo_url("/hooks")).json(&body)))?;
        *self.hook_id.lock().unwrap() = hook.get("id").and_then(|i| i.as_i64());
        Ok(())
    }
    fn unregister_webhook(&self, _url: &str) -> Result<()> {
        if let Some(id) = self.hook_id.lock().unwrap().take() {
            self.rest.send_ok(self.cfg.req(Req::new("DELETE", self.cfg.repo_url(&format!("/hooks/{}", id)))))?;
        }
        Ok(())
    }
    fn webhook_processor(&self, next: Arc<dyn Processor>) -> Arc<dyn Processor> {
        Arc::new(move |ex: &mut Exchange| {
            let text = body_text(ex).unwrap_or_default();
            let payload = camel_core::json::parse(&text).unwrap_or(Value::String(text));
            if let Some(ev) = header_str(ex, "X-GitHub-Event") {
                ex.message.headers.set(GITHUB_EVENT, ev);
            }
            ex.message.body = payload;
            next.process(ex)?;
            ex.message.headers.set("CamelHttpResponseCode", Value::Int(200));
            ex.message.body = Value::String(String::new());
            Ok(())
        })
    }
}

struct GithubProducer {
    cfg: Config,
    rest: Rest,
}

impl Service for GithubProducer {}

fn number_header(ex: &Exchange, name: &str) -> Result<i64> {
    let v = header_str(ex, name).ok_or_else(|| CamelError::illegal_argument(format!("github: missing header {}", name)))?;
    v.trim().parse().map_err(|_| CamelError::illegal_argument(format!("github: header {} must be a number but was {}", name, v)))
}

impl Processor for GithubProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let text = body_text(ex).unwrap_or_default();
        match self.cfg.ty.as_str() {
            "createIssue" => {
                let title = header_str(ex, GITHUB_ISSUE_TITLE).ok_or_else(|| CamelError::illegal_argument("github: createIssue requires the GitHubIssueTitle header"))?;
                let issue = self.rest.json(self.cfg.req(Req::post(self.cfg.repo_url("/issues")).json(&Value::object([("title", title), ("body", text)]))))?;
                if let Some(n) = issue.get("number") {
                    ex.message.headers.set(GITHUB_ISSUE, n.clone());
                }
                ex.message.body = issue;
            }
            "pullRequestComment" | "issueComment" => {
                let header = if self.cfg.ty == "issueComment" { GITHUB_ISSUE } else { GITHUB_PULL_REQUEST };
                let n = number_header(ex, header)?;
                let url = match header_str(ex, GITHUB_IN_RESPONSE_TO).filter(|_| self.cfg.ty == "pullRequestComment") {
                    Some(id) => self.cfg.repo_url(&format!("/pulls/{}/comments/{}/replies", n, id.trim())),
                    None => self.cfg.repo_url(&format!("/issues/{}/comments", n)),
                };
                ex.message.body = self.rest.json(self.cfg.req(Req::post(url).json(&Value::object([("body", text)]))))?;
            }
            "pullRequestState" => {
                let sha = header_str(ex, GITHUB_PULL_REQUEST_HEAD_COMMIT_SHA).ok_or_else(|| CamelError::illegal_argument("github: pullRequestState requires the GitHubPullRequestHeadCommitSHA header"))?;
                let state = self.cfg.state.clone().ok_or_else(|| CamelError::illegal_argument("github: pullRequestState requires the state option"))?;
                let mut body = vec![("state", Value::String(state)), ("description", Value::String(text))];
                if let Some(t) = &self.cfg.target_url {
                    body.push(("target_url", Value::String(t.clone())));
                }
                ex.message.body = self.rest.json(self.cfg.req(Req::post(self.cfg.repo_url(&format!("/statuses/{}", sha))).json(&Value::object(body))))?;
            }
            _ => unreachable!(),
        }
        Ok(())
    }
}

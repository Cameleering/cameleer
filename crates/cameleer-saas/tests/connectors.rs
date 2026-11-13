//! SaaS connectors against their embedded mock servers.

use std::time::{Duration, Instant};

use cameleer_core::prelude::*;
use cameleer_saas::testing::*;

fn ctx() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    cameleer_saas::register(&ctx);
    ctx
}

fn wait_for(m: &MockEndpoint, n: usize) -> Vec<Exchange> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while m.received_counter() < n && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let r = m.received_exchanges();
    assert!(r.len() >= n, "expected {} exchanges, got {}", n, r.len());
    r
}

fn err_text(e: CamelError) -> String {
    let mut s = format!("{}: {}", e.kind, e.message);
    let mut c = e.cause.as_deref();
    while let Some(x) = c {
        s.push_str(&format!(" / {}: {}", x.kind, x.message));
        c = x.cause.as_deref();
    }
    s
}

const POLL: &str = "delay=20&initialDelay=0";

// ---------------------------------------------------------------------------

#[test]
fn slack_webhook_and_web_api_producers() {
    let slack = SlackMock::start();
    let ctx = ctx();
    let (wh, api, bad) = (
        format!("slack:#general?webhookUrl={}&username=camel-bot&iconEmoji=:camel:", slack.webhook_url()),
        format!("slack:#general?token={}&serverUrl={}", SlackMock::TOKEN, slack.url()),
        format!("slack:#nope?token=wrong&serverUrl={}", slack.url()),
    );
    let unknown = format!("slack:#nope?token={}&serverUrl={}", SlackMock::TOKEN, slack.url());
    ctx.add_routes(move |b| {
        b.from("direct:wh").to(&wh);
        b.from("direct:api").to(&api);
        b.from("direct:bad").to(&bad);
        b.from("direct:unknown").to(&unknown);
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:wh", "Hello from Camel").unwrap();
    t.send_body("direct:wh", Value::object([("blocks", Value::Array(vec![Value::object([("type", "divider")])])), ("text", Value::from("fallback"))])).unwrap();
    let posts = slack.webhook_posts();
    assert_eq!(posts.len(), 2);
    assert_eq!(posts[0].get("text"), Some(&Value::from("Hello from Camel")));
    assert_eq!(posts[0].get("channel"), Some(&Value::from("#general")));
    assert_eq!(posts[0].get("username"), Some(&Value::from("camel-bot")));
    assert_eq!(posts[0].get("icon_emoji"), Some(&Value::from(":camel:")));
    assert!(posts[1].get("blocks").is_some());

    let ex = t.request("direct:api", |ex| ex.set_body("via web api")).unwrap();
    assert_eq!(ex.message.headers.get("CamelSlackResponse").and_then(|r| r.get("ok")), Some(&Value::Bool(true)));
    assert_eq!(slack.posted()[0].get("text"), Some(&Value::from("via web api")));
    let auth = slack.server.requests_to("/api/chat.postMessage");
    assert_eq!(auth[0].header("Authorization"), Some(format!("Bearer {}", SlackMock::TOKEN).as_str()));
    let e = err_text(t.request_body("direct:bad", "x").unwrap_err());
    assert!(e.contains("invalid_auth"), "{}", e);
    let e = err_text(t.request_body("direct:unknown", "x").unwrap_err());
    assert!(e.contains("ApiException") && e.contains("channel_not_found"), "{}", e);
    ctx.stop().unwrap();
}

#[test]
fn slack_consumer_polls_history() {
    let slack = SlackMock::start();
    slack.add_message("general", "U1", "first");
    slack.add_message("general", "U2", "second");
    slack.add_message("random", "U3", "elsewhere");
    let ctx = ctx();
    let uri = format!("slack://general?token={}&serverUrl={}&naturalOrder=true&maxResults=10&{}", SlackMock::TOKEN, slack.url(), POLL);
    ctx.add_routes(move |b| {
        b.from(&uri).to("mock:slack");
    })
    .unwrap();
    ctx.start().unwrap();
    let m = ctx.mock_endpoint("mock:slack").unwrap();
    let r = wait_for(&m, 2);
    assert_eq!(r[0].body().get("text"), Some(&Value::from("first")));
    assert_eq!(r[1].body().get("text"), Some(&Value::from("second")));
    slack.add_message("general", "U1", "third");
    let r = wait_for(&m, 3);
    assert_eq!(r[2].body().get("text"), Some(&Value::from("third")));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(m.received_counter(), 3, "messages must not be delivered twice");
    ctx.stop().unwrap();
    assert!(slack.server.requests_to("/api/conversations.history").iter().all(|r| r.query_param("channel") == Some("C001")));
}

// ---------------------------------------------------------------------------

#[test]
fn telegram_producer_text_photo_and_errors() {
    let tg = TelegramMock::start();
    let ctx = ctx();
    let base = format!("telegram:bots?authorizationToken={}&baseUri={}", TelegramMock::TOKEN, tg.url());
    let (a, b2, c) = (format!("{}&chatId=42", base), base.clone(), format!("telegram:bots?authorizationToken=bad:token&chatId=1&baseUri={}", tg.url()));
    ctx.add_routes(move |b| {
        b.from("direct:text").to(&a);
        b.from("direct:nochat").to(&b2);
        b.from("direct:bad").to(&c);
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:text", "Hello Telegram").unwrap();
    let ex = t
        .request("direct:text", |ex| {
            ex.set_body("<b>bold</b>");
            ex.set_header("CamelTelegramChatId", "99");
            ex.set_header("CamelTelegramParseMode", "HTML");
        })
        .unwrap();
    assert_eq!(ex.body().get("text"), Some(&Value::from("<b>bold</b>")));
    assert!(ex.message.headers.get("CamelTelegramMessageId").is_some());
    t.request("direct:text", |ex| {
        ex.set_body(vec![0xFFu8, 0xD8, 0xFF, 0x00]);
        ex.set_header("CamelTelegramMediaType", "PHOTO_JPG");
        ex.set_header("CamelTelegramMediaTitleCaption", "a photo");
    })
    .unwrap();
    let sent = tg.sent();
    assert_eq!(sent.len(), 3);
    assert_eq!(sent[0].0, "sendMessage");
    assert_eq!(sent[0].1.get("chat_id"), Some(&Value::from("42")));
    assert_eq!(sent[1].1.get("parse_mode"), Some(&Value::from("HTML")));
    assert_eq!(sent[1].1.get("chat_id"), Some(&Value::from("99")));
    assert_eq!(sent[2].0, "sendPhoto");
    assert_eq!(sent[2].1.get("caption"), Some(&Value::from("a photo")));
    assert_eq!(sent[2].1.get("photo").and_then(|p| p.get("size")), Some(&Value::Int(4)));
    let e = err_text(t.request_body("direct:nochat", "x").unwrap_err());
    assert!(e.contains("chat id is not set"), "{}", e);
    let e = err_text(t.request_body("direct:bad", "x").unwrap_err());
    assert!(e.contains("Unauthorized"), "{}", e);
    assert!(!e.contains("bad:token"), "token leaked: {}", e);
    ctx.stop().unwrap();
}

#[test]
fn telegram_consumer_long_polls_updates() {
    let tg = TelegramMock::start();
    tg.push_message(7, "hi bot");
    tg.push_message(8, "second");
    let ctx = ctx();
    let uri = format!("telegram:bots?authorizationToken={}&baseUri={}&timeout=0&{}", TelegramMock::TOKEN, tg.url(), POLL);
    ctx.add_routes(move |b| {
        b.from(&uri).to("mock:tg");
    })
    .unwrap();
    ctx.start().unwrap();
    let m = ctx.mock_endpoint("mock:tg").unwrap();
    let r = wait_for(&m, 2);
    assert_eq!(r[0].body().get("text"), Some(&Value::from("hi bot")));
    assert_eq!(r[0].message.headers.get("CamelTelegramChatId"), Some(&Value::Int(7)));
    assert_eq!(r[1].message.headers.get("CamelTelegramChatId"), Some(&Value::Int(8)));
    tg.push_message(7, "third");
    let r = wait_for(&m, 3);
    assert_eq!(r[2].body().get("text"), Some(&Value::from("third")));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(m.received_counter(), 3);
    ctx.stop().unwrap();
}

#[test]
fn webhook_with_telegram_registers_and_receives() {
    let tg = TelegramMock::start();
    let port = free_port();
    let ctx = ctx();
    ctx.properties().set("camel.server.port", &port.to_string());
    let uri = format!(
        "webhook:telegram:bots?authorizationToken={}&baseUri={}&webhookExternalUrl=http://127.0.0.1:{}&webhookPath=/tg-hook",
        TelegramMock::TOKEN,
        tg.url(),
        port
    );
    ctx.add_routes(move |b| {
        b.from(&uri).to("mock:hook");
    })
    .unwrap();
    ctx.start().unwrap();
    assert_eq!(tg.webhook(), Some(format!("http://127.0.0.1:{}/tg-hook", port)));
    let u = tg.push_message(5, "via webhook");
    assert_eq!(tg.deliver_to_webhook(&u).unwrap(), 200);
    let m = ctx.mock_endpoint("mock:hook").unwrap();
    let r = wait_for(&m, 1);
    assert_eq!(r[0].body().get("text"), Some(&Value::from("via webhook")));
    assert_eq!(r[0].message.headers.get("CamelTelegramChatId"), Some(&Value::Int(5)));
    ctx.stop().unwrap();
    assert_eq!(tg.webhook(), None, "webhook must be removed on stop");
    // a missing external url is reported
    let ctx2 = self::ctx();
    let bad = format!("webhook:telegram:bots?authorizationToken={}&baseUri={}", TelegramMock::TOKEN, tg.url());
    let r = ctx2.add_routes(move |b| {
        b.from(&bad).to("mock:x");
    });
    let started = r.and_then(|_| ctx2.start());
    assert!(started.is_err());
    assert!(err_text(started.unwrap_err()).contains("webhookExternalUrl"));
}

// ---------------------------------------------------------------------------

fn gh(ty: &str, gh: &GithubMock) -> String {
    format!("github:{}?repoOwner={}&repoName={}&oauthToken={}&serverUrl={}", ty, GithubMock::OWNER, GithubMock::REPO, GithubMock::TOKEN, gh.url())
}

#[test]
fn github_producers() {
    let mock = GithubMock::start();
    let ctx = ctx();
    let (a, b2, c, d) = (gh("createIssue", &mock), gh("pullRequestComment", &mock), format!("{}&state=success&targetUrl=https://ci", gh("pullRequestState", &mock)), format!("github:createIssue?repoOwner={}&repoName={}&oauthToken=nope&serverUrl={}", GithubMock::OWNER, GithubMock::REPO, mock.url()));
    ctx.add_routes(move |b| {
        b.from("direct:issue").to(&a);
        b.from("direct:comment").to(&b2);
        b.from("direct:state").to(&c);
        b.from("direct:bad").to(&d);
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let issue = t.request_body_and_header("direct:issue", "Something broke", "GitHubIssueTitle", "Bug report").unwrap();
    assert_eq!(issue.get("title"), Some(&Value::from("Bug report")));
    assert_eq!(mock.issues()[0].get("body"), Some(&Value::from("Something broke")));
    let pr = mock.add_pull("Feature", "abc123");
    let c = t.request_body_and_header("direct:comment", "LGTM", "GitHubPullRequest", pr).unwrap();
    assert_eq!(c.get("body"), Some(&Value::from("LGTM")));
    assert_eq!(mock.comments()[0].0, pr);
    t.request_body_and_header("direct:state", "all green", "GitHubPullRequestHeadCommitSHA", "abc123").unwrap();
    let st = mock.statuses();
    assert_eq!(st[0].0, "abc123");
    assert_eq!(st[0].1.get("state"), Some(&Value::from("success")));
    assert_eq!(st[0].1.get("target_url"), Some(&Value::from("https://ci")));
    let e = err_text(t.request_body_and_header("direct:bad", "x", "GitHubIssueTitle", "t").unwrap_err());
    assert!(e.contains("401") && e.contains("Bad credentials"), "{}", e);
    let e = err_text(t.request_body("direct:issue", "no title").unwrap_err());
    assert!(e.contains("GitHubIssueTitle"), "{}", e);
    ctx.stop().unwrap();
}

#[test]
fn github_consumers_pull_requests_issues_commits() {
    let mock = GithubMock::start();
    mock.add_pull("existing", "s0");
    mock.add_issue("existing issue");
    mock.add_commit("main", "c1", "initial commit", "ann");
    let ctx = ctx();
    let (a, b2, c) = (format!("{}&{}", gh("pullRequest", &mock), POLL), format!("{}&{}", gh("issue", &mock), POLL), format!("{}&{}", gh("commit/main", &mock), POLL));
    ctx.add_routes(move |b| {
        b.from(&a).to("mock:prs");
        b.from(&b2).to("mock:issues");
        b.from(&c).to("mock:commits");
    })
    .unwrap();
    ctx.start().unwrap();
    // the first polls (one per consumer) record the existing state: wait until they were answered, not for a fixed time
    assert!(mock.server.wait_for_requests(6, Duration::from_secs(20)));
    std::thread::sleep(Duration::from_millis(100));
    let n = mock.add_pull("new feature", "s1");
    mock.add_issue("new issue");
    mock.add_commit("main", "c2", "fix bug", "bob");
    mock.add_commit("main", "c3", "add docs", "cid");
    mock.add_commit("other", "x1", "elsewhere", "dan");
    let prs = wait_for(&ctx.mock_endpoint("mock:prs").unwrap(), 1);
    assert_eq!(prs[0].body().get("title"), Some(&Value::from("new feature")));
    assert_eq!(prs[0].message.headers.get("GitHubPullRequest"), Some(&Value::Int(n)));
    let issues = wait_for(&ctx.mock_endpoint("mock:issues").unwrap(), 1);
    assert_eq!(issues[0].body().get("title"), Some(&Value::from("new issue")));
    let commits = wait_for(&ctx.mock_endpoint("mock:commits").unwrap(), 2);
    assert_eq!(commits[0].body(), &Value::from("fix bug"));
    assert_eq!(commits[1].body(), &Value::from("add docs"));
    assert_eq!(commits[0].message.headers.get("GitHubCommitSha"), Some(&Value::from("c2")));
    assert_eq!(commits[0].message.headers.get("GitHubCommitAuthor"), Some(&Value::from("bob")));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(ctx.mock_endpoint("mock:prs").unwrap().received_counter(), 1);
    assert_eq!(ctx.mock_endpoint("mock:issues").unwrap().received_counter(), 1, "pull requests are not issues");
    assert_eq!(ctx.mock_endpoint("mock:commits").unwrap().received_counter(), 2);
    ctx.stop().unwrap();
    assert!(mock.server.requests().iter().all(|r| r.header("User-Agent").is_some()));
}

#[test]
fn github_webhook_registers_repository_hook() {
    let mock = GithubMock::start();
    let port = free_port();
    let ctx = ctx();
    ctx.properties().set("camel.server.port", &port.to_string());
    let uri = format!("webhook:{}&webhookExternalUrl=http://127.0.0.1:{}&webhookPath=/gh", gh("pullRequest", &mock), port);
    ctx.add_routes(move |b| {
        b.from(&uri).to("mock:gh");
    })
    .unwrap();
    ctx.start().unwrap();
    let hooks = mock.hooks();
    assert_eq!(hooks.len(), 1);
    assert_eq!(hooks[0].get("config").and_then(|c| c.get("url")), Some(&Value::from(format!("http://127.0.0.1:{}/gh", port))));
    let r = cameleer_http::HttpClient::new()
        .request("POST", &format!("http://127.0.0.1:{}/gh", port), &[("Content-Type", "application/json"), ("X-GitHub-Event", "pull_request")], Some(br#"{"action":"opened","number":7}"#), None)
        .unwrap();
    assert_eq!(r.status, 200);
    let m = ctx.mock_endpoint("mock:gh").unwrap();
    let ex = wait_for(&m, 1);
    assert_eq!(ex[0].body().get("action"), Some(&Value::from("opened")));
    assert_eq!(ex[0].message.headers.get("GitHubEvent"), Some(&Value::from("pull_request")));
    ctx.stop().unwrap();
    assert!(mock.hooks().is_empty());
}

// ---------------------------------------------------------------------------

#[test]
fn twilio_send_fetch_and_auth() {
    let tw = TwilioMock::start();
    let ctx = ctx();
    let base = format!("username={}&password={}&serverUrl={}", TwilioMock::ACCOUNT_SID, TwilioMock::AUTH_TOKEN, tw.url());
    let (a, b2, c) = (
        format!("twilio:message/create?from=%2B15005550006&to=%2B15005550001&{}", base),
        format!("twilio:message/fetch?{}", base),
        format!("twilio:message/create?from=%2B1&to=%2B2&username={}&password=wrong&serverUrl={}", TwilioMock::ACCOUNT_SID, tw.url()),
    );
    ctx.add_routes(move |b| {
        b.from("direct:sms").to(&a);
        b.from("direct:fetch").to(&b2);
        b.from("direct:bad").to(&c);
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let m = t.request_body("direct:sms", "Hello SMS").unwrap();
    assert_eq!(m.get("status"), Some(&Value::from("queued")));
    let m2 = t.request_body_and_header("direct:sms", "Other", "CamelTwilio.to", "+15005550002").unwrap();
    assert_eq!(m2.get("to"), Some(&Value::from("+15005550002")));
    let msgs = tw.messages();
    assert_eq!(msgs[0].get("to"), Some(&Value::from("+15005550001")));
    assert_eq!(msgs[0].get("from"), Some(&Value::from("+15005550006")));
    assert_eq!(msgs[0].get("body"), Some(&Value::from("Hello SMS")));
    let sid = m.get("sid").unwrap().to_string();
    let f = t.request_body_and_header("direct:fetch", Value::Null, "CamelTwilio.pathSid", sid.clone()).unwrap();
    assert_eq!(f.get("sid"), Some(&Value::from(sid)));
    let e = err_text(t.request_body("direct:bad", "x").unwrap_err());
    assert!(e.contains("401") && e.contains("20003") && e.contains("Authenticate"), "{}", e);
    ctx.stop().unwrap();
}

// ---------------------------------------------------------------------------

#[test]
fn jira_producers_and_new_issues_consumer() {
    let jira = JiraMock::start();
    jira.add_issue("CAMEL", "old issue");
    let ctx = ctx();
    let auth = format!("jiraUrl={}&username={}&password={}", jira.url(), JiraMock::USER, JiraMock::PASSWORD);
    let (a, b2, c, d, e) = (
        format!("jira:addIssue?{}", auth),
        format!("jira:addComment?{}", auth),
        format!("jira:searchIssues?jiraUrl={}&accessToken={}", jira.url(), JiraMock::PAT),
        format!("jira:newIssues?{}&jql=RAW(project = CAMEL)&{}", auth, POLL),
        format!("jira:addIssue?jiraUrl={}&username=x&password=y", jira.url()),
    );
    ctx.add_routes(move |b| {
        b.from("direct:add").to(&a);
        b.from("direct:comment").to(&b2);
        b.from("direct:search").to(&c);
        b.from(&d).to("mock:new");
        b.from("direct:bad").to(&e);
    })
    .unwrap();
    ctx.start().unwrap();
    std::thread::sleep(Duration::from_millis(150));
    let t = ctx.create_producer_template();
    let ex = t
        .request("direct:add", |ex| {
            ex.set_body("The description");
            ex.set_header("ProjectKey", "CAMEL");
            ex.set_header("IssueTypeName", "Bug");
            ex.set_header("IssueSummary", "Something is wrong");
            ex.set_header("IssuePriorityName", "High");
        })
        .unwrap();
    assert_eq!(ex.message.headers.get("IssueKey"), Some(&Value::from("CAMEL-2")));
    assert_eq!(ex.body().get("key"), Some(&Value::from("CAMEL-2")));
    let issue = &jira.issues()[1];
    assert_eq!(issue.get("fields").and_then(|f| f.get("description")), Some(&Value::from("The description")));
    assert_eq!(issue.get("fields").and_then(|f| f.get("priority")).and_then(|p| p.get("name")), Some(&Value::from("High")));
    let c = t.request_body_and_header("direct:comment", "A comment", "IssueKey", "CAMEL-2").unwrap();
    assert_eq!(c.get("body"), Some(&Value::from("A comment")));
    assert_eq!(jira.comments()[0].0, "CAMEL-2");
    let found = t.request_body("direct:search", "project = CAMEL").unwrap();
    assert_eq!(found.as_array().map(|a| a.len()), Some(2));
    let m = ctx.mock_endpoint("mock:new").unwrap();
    let r = wait_for(&m, 1);
    assert_eq!(r[0].message.headers.get("IssueKey"), Some(&Value::from("CAMEL-2")));
    jira.add_issue("CAMEL", "third");
    jira.add_issue("OTHER", "not mine");
    let r = wait_for(&m, 2);
    assert_eq!(r[1].message.headers.get("IssueKey"), Some(&Value::from("CAMEL-3")));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(m.received_counter(), 2);
    let e = err_text(t.request_body_and_headers("direct:bad", "x", vec![("ProjectKey", Value::from("CAMEL")), ("IssueTypeName", Value::from("Bug")), ("IssueSummary", Value::from("s"))]).unwrap_err());
    assert!(e.contains("401"), "{}", e);
    ctx.stop().unwrap();
    assert!(jira.server.requests_to("/rest/api/2/search").iter().any(|r| r.query_param("jql").map(|q| q.contains("ORDER BY key desc")).unwrap_or(false)));
}

// ---------------------------------------------------------------------------

#[test]
fn openai_chat_and_embeddings() {
    let ai = OpenAiMock::start();
    let ctx = ctx();
    let base = format!("apiKey={}&baseUrl={}", OpenAiMock::API_KEY, ai.base_url());
    let (a, b2, c, d) = (
        format!("openai:chat-completion?model=gpt-4o-mini&temperature=0.2&maxTokens=50&systemMessage=Be brief&{}", base),
        format!("openai:embeddings?model=text-embedding-3-small&{}", base),
        format!("openai:chat-completion?model=gpt-4o-mini&outputType=full&{}", base),
        format!("openai:chat-completion?model=m&apiKey=wrong&baseUrl={}", ai.base_url()),
    );
    ctx.add_routes(move |b| {
        b.from("direct:chat").to(&a);
        b.from("direct:embed").to(&b2);
        b.from("direct:full").to(&c);
        b.from("direct:bad").to(&d);
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let ex = t.request("direct:chat", |ex| ex.set_body("What is Camel?")).unwrap();
    assert_eq!(ex.body(), &Value::from("Echo: What is Camel? [Be brief]"));
    assert_eq!(ex.message.headers.get("CamelOpenAIFinishReason"), Some(&Value::from("stop")));
    assert_eq!(ex.message.headers.get("CamelOpenAIResponseModel"), Some(&Value::from("gpt-4o-mini")));
    assert!(ex.message.headers.get("CamelOpenAITotalTokens").and_then(|v| v.as_i64()).unwrap() > 0);
    let req = ai.server.requests_to("/v1/chat/completions")[0].json();
    assert_eq!(req.get("temperature"), Some(&Value::Float(0.2)));
    assert_eq!(req.get("max_tokens"), Some(&Value::Int(50)));
    assert_eq!(req.get("messages").and_then(|m| m.as_array()).map(|m| m.len()), Some(2));
    let r = t.request_body("direct:chat", Value::Array(vec![Value::object([("role", "user"), ("content", "one")]), Value::object([("role", "assistant"), ("content", "two")]), Value::from("three")])).unwrap();
    assert_eq!(r, Value::from("Echo: three [Be brief]"));
    let full = t.request_body("direct:full", "x").unwrap();
    assert_eq!(full.get("object"), Some(&Value::from("chat.completion")));
    let v = t.request_body("direct:embed", "abcd").unwrap();
    assert_eq!(v, Value::Array(vec![Value::Float(4.0), Value::Float(2.0), Value::Float(1.0)]));
    let vs = t.request_body("direct:embed", Value::Array(vec![Value::from("ab"), Value::from("abcdef")])).unwrap();
    assert_eq!(vs.as_array().map(|a| a.len()), Some(2));
    let e = err_text(t.request_body("direct:bad", "x").unwrap_err());
    assert!(e.contains("401") && e.contains("Incorrect API key"), "{}", e);
    ctx.stop().unwrap();
}

// ---------------------------------------------------------------------------

#[test]
fn graphql_queries_variables_and_auth() {
    let gql = GraphqlMock::start(true);
    let ctx = ctx();
    let ep = gql.endpoint();
    ctx.registry().bind("bookVars", Value::object([("id", "2")]));
    let (a, b2, c, d, e) = (
        format!("graphql://{}?query=RAW({{books{{id name}}}})&accessToken={}", ep, GraphqlMock::TOKEN),
        format!("graphql://{}?accessToken={}", ep, GraphqlMock::TOKEN),
        format!("graphql://{}?query=RAW(query Book($id: ID) {{ book(id: $id) {{ name }} }})&operationName=Book&variables=#bookVars&accessToken={}", ep, GraphqlMock::TOKEN),
        format!("graphql://{}?query=RAW(mutation {{ addBook(name: $name, author: $author) {{ id }} }})&accessToken={}", ep, GraphqlMock::TOKEN),
        format!("graphql://{}?query=RAW({{books{{id}}}})", ep),
    );
    ctx.add_routes(move |b| {
        b.from("direct:books").to(&a);
        b.from("direct:body").to(&b2);
        b.from("direct:book").to(&c);
        b.from("direct:add").to(&d);
        b.from("direct:noauth").to(&e);
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let r = t.request_body("direct:books", Value::Null).unwrap();
    let v = cameleer_core::json::parse(&r.to_string()).unwrap();
    assert_eq!(v.get("data").and_then(|d| d.get("books")).and_then(|b| b.as_array()).map(|b| b.len()), Some(2));
    let r = t.request_body("direct:body", "{ books { name } }").unwrap();
    assert!(r.to_string().contains("Camel in Action"));
    let r = t.request_body("direct:book", Value::Null).unwrap().to_string();
    assert!(r.contains("Enterprise Integration Patterns") && r.contains("\"_operationName\":\"Book\""), "{}", r);
    let r = t.request_body("direct:add", Value::object([("name", "New Book"), ("author", "Me")])).unwrap().to_string();
    assert!(r.contains("\"id\":\"3\""), "{}", r);
    let sent = gql.server.requests_to("/graphql");
    assert_eq!(sent[3].json().get("variables").and_then(|v| v.get("name")), Some(&Value::from("New Book")));
    assert_eq!(sent[0].header("Authorization"), Some(format!("Bearer {}", GraphqlMock::TOKEN).as_str()));
    let e = err_text(t.request_body("direct:noauth", Value::Null).unwrap_err());
    assert!(e.contains("401"), "{}", e);
    ctx.stop().unwrap();
}

// ---------------------------------------------------------------------------

const RSS: &str = r#"<?xml version="1.0"?>
<rss version="2.0" xmlns:dc="http://purl.org/dc/elements/1.1/"><channel>
  <title>Camel News</title><link>https://camel.apache.org</link><description>News</description>
  <item><title>Camel 4.0</title><link>https://camel.apache.org/4.0</link><guid>n1</guid><pubDate>Mon, 01 Jan 2024 10:00:00 GMT</pubDate><category>release</category></item>
  <item><title>Camel 4.1</title><link>https://camel.apache.org/4.1</link><guid>n2</guid><pubDate>Mon, 01 Apr 2024 10:00:00 GMT</pubDate><dc:creator>Claus</dc:creator></item>
</channel></rss>"#;

const ATOM: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom"><title>Example Feed</title><link href="http://example.org/"/><updated>2024-01-02T00:00:00Z</updated>
  <author><name>John Doe</name></author>
  <entry><title>Entry One</title><link href="http://example.org/1"/><id>urn:1</id><updated>2024-01-01T00:00:00Z</updated><summary>First</summary><category term="a"/></entry>
  <entry><title>Entry Two</title><link rel="alternate" href="http://example.org/2"/><id>urn:2</id><published>2024-01-02T00:00:00Z</published><content>Second body</content></entry>
</feed>"#;

#[test]
fn rss_split_filter_and_new_entries() {
    let feeds = FeedMock::start();
    feeds.set("/rss.xml", RSS);
    let ctx = ctx();
    let uri = format!("rss:{}?splitEntries=true&sortEntries=true&throttleEntries=false&{}", feeds.url("/rss.xml"), POLL);
    let filtered = format!("rss:{}?lastUpdate=2024-02-01T00:00:00&throttleEntries=false&{}", feeds.url("/rss.xml"), POLL);
    let whole = format!("rss:{}?splitEntries=false&{}", feeds.url("/rss.xml"), POLL);
    ctx.add_routes(move |b| {
        b.from(&uri).to("mock:rss");
        b.from(&filtered).to("mock:filtered");
        b.from(&whole).to("mock:whole");
    })
    .unwrap();
    ctx.start().unwrap();
    let m = ctx.mock_endpoint("mock:rss").unwrap();
    let r = wait_for(&m, 2);
    let entry = |ex: &Exchange| ex.body().get("entries").and_then(|e| e.as_array()).and_then(|e| e.first()).cloned().unwrap();
    assert_eq!(entry(&r[0]).get("title"), Some(&Value::from("Camel 4.0")));
    assert_eq!(entry(&r[1]).get("author"), Some(&Value::from("Claus")));
    assert_eq!(entry(&r[0]).get("categories"), Some(&Value::Array(vec![Value::from("release")])));
    assert_eq!(r[0].body().get("title"), Some(&Value::from("Camel News")));
    assert_eq!(r[0].message.headers.get("CamelRssFeed").and_then(|f| f.get("entries")).and_then(|e| e.as_array()).map(|e| e.len()), Some(2));
    let f = wait_for(&ctx.mock_endpoint("mock:filtered").unwrap(), 1);
    assert_eq!(entry(&f[0]).get("title"), Some(&Value::from("Camel 4.1")));
    let w = wait_for(&ctx.mock_endpoint("mock:whole").unwrap(), 1);
    assert_eq!(w[0].body().get("entries").and_then(|e| e.as_array()).map(|e| e.len()), Some(2));
    // a new item is delivered once
    feeds.set("/rss.xml", &RSS.replace("</channel>", "<item><title>Camel 4.2</title><guid>n3</guid><pubDate>Mon, 01 Jul 2024 10:00:00 GMT</pubDate></item></channel>"));
    let r = wait_for(&m, 3);
    assert_eq!(entry(&r[2]).get("title"), Some(&Value::from("Camel 4.2")));
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(m.received_counter(), 3);
    assert_eq!(ctx.mock_endpoint("mock:filtered").unwrap().received_counter(), 2);
    ctx.stop().unwrap();
}

#[test]
fn atom_entries_with_throttling() {
    let feeds = FeedMock::start();
    feeds.set("/atom.xml", ATOM);
    let ctx = ctx();
    let uri = format!("atom:{}?{}", feeds.url("/atom.xml"), POLL);
    ctx.add_routes(move |b| {
        b.from(&uri).to("mock:atom");
    })
    .unwrap();
    ctx.start().unwrap();
    let m = ctx.mock_endpoint("mock:atom").unwrap();
    let r = wait_for(&m, 2);
    assert_eq!(r[0].body().get("title"), Some(&Value::from("Entry One")));
    assert_eq!(r[0].body().get("link"), Some(&Value::from("http://example.org/1")));
    assert_eq!(r[0].body().get("description"), Some(&Value::from("First")));
    assert_eq!(r[1].body().get("contents"), Some(&Value::from("Second body")));
    assert_eq!(r[1].body().get("uri"), Some(&Value::from("urn:2")));
    assert_eq!(r[0].message.headers.get("CamelAtomFeed").and_then(|f| f.get("author")), Some(&Value::from("John Doe")));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(m.received_counter(), 2);
    ctx.stop().unwrap();
    // throttling delivered one entry per poll
    let polls = feeds.server.requests_to("/atom.xml").len();
    assert!(polls >= 2, "{}", polls);
}

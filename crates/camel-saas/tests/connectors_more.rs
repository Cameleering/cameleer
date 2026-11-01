//! More SaaS connector operations (github files/events, jira workflow, twilio resources).

use std::time::{Duration, Instant};

use camel_core::prelude::*;
use camel_saas::testing::*;

fn ctx() -> CamelContext {
    let ctx = CamelContext::new();
    camel_http::register(&ctx);
    camel_saas::register(&ctx);
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

const POLL: &str = "delay=20&initialDelay=0";

fn gh(ty: &str, gh: &GithubMock) -> String {
    format!("github:{}?repoOwner={}&repoName={}&oauthToken={}&serverUrl={}", ty, GithubMock::OWNER, GithubMock::REPO, GithubMock::TOKEN, gh.url())
}

#[test]
fn github_pull_request_files_commit_file_close_and_events() {
    let mock = GithubMock::start();
    let pr = mock.add_pull("Feature", "abc");
    mock.add_pull_file(pr, "src/lib.rs", "b1", b"fn main() {}\n// a long enough line to wrap the base64 content at sixty columns\n");
    mock.add_pull_file(pr, "README.md", "b2", b"# Readme");
    mock.add_event("WatchEvent", Value::object([("action", "started")]));
    let ctx = ctx();
    let (files, file, file64, close, events) = (gh("pullRequestFiles", &mock), gh("getCommitFile", &mock), format!("{}&encoding=base64", gh("getCommitFile", &mock)), gh("closePullRequest", &mock), format!("{}&{}", gh("event", &mock), POLL));
    ctx.add_routes(move |b| {
        b.from("direct:files").to(&files);
        b.from("direct:file").to(&file);
        b.from("direct:file64").to(&file64);
        b.from("direct:close").to(&close);
        b.from(&events).to("mock:events");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let list = t.request_body_and_header("direct:files", Value::Null, "GitHubPullRequest", pr).unwrap();
    let list = list.as_array().unwrap().clone();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].get("filename"), Some(&Value::from("src/lib.rs")));
    // the file object of pullRequestFiles feeds getCommitFile
    let content = t.request_body("direct:file", list[0].clone()).unwrap();
    assert_eq!(content, Value::from("fn main() {}\n// a long enough line to wrap the base64 content at sixty columns\n"));
    assert_eq!(t.request_body("direct:file", "b2").unwrap(), Value::from("# Readme"));
    assert_eq!(t.request_body("direct:file64", "b2").unwrap().to_string().trim(), "IyBSZWFkbWU=");
    let closed = t.request_body_and_header("direct:close", Value::Null, "GitHubPullRequest", pr).unwrap();
    assert_eq!(closed.get("state"), Some(&Value::from("closed")));
    assert_eq!(mock.pulls()[0].get("state"), Some(&Value::from("closed")));
    std::thread::sleep(Duration::from_millis(150)); // first poll records the existing events
    mock.add_event("PushEvent", Value::object([("ref", "refs/heads/main")]));
    mock.add_event("IssuesEvent", Value::object([("action", "opened")]));
    let m = ctx.mock_endpoint("mock:events").unwrap();
    let r = wait_for(&m, 2);
    assert_eq!(r[0].body(), &Value::from("PushEvent"));
    assert_eq!(r[0].message.headers.get("CamelGitHubEventPayload").and_then(|p| p.get("ref")), Some(&Value::from("refs/heads/main")));
    assert_eq!(r[1].body(), &Value::from("IssuesEvent"));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(m.received_counter(), 2);
    let e = t.request_body_and_header("direct:files", Value::Null, "GitHubPullRequest", 999).unwrap_err();
    assert!(e.message.contains("404"), "{}", e.message);
    ctx.stop().unwrap();
}

#[test]
fn jira_workflow_operations_and_consumers() {
    let jira = JiraMock::start();
    let k1 = jira.add_issue("CAMEL", "first");
    let k2 = jira.add_issue("CAMEL", "second");
    jira.add_comment(&k1, "old comment");
    jira.set_field(&k1, "status", Value::object([("name", "To Do")]));
    jira.set_field(&k2, "status", Value::object([("name", "To Do")]));
    let ctx = ctx();
    let auth = format!("jiraUrl={}&username={}&password={}", jira.url(), JiraMock::USER, JiraMock::PASSWORD);
    let uris: Vec<String> = ["updateIssue", "transitionIssue", "attach", "watchers", "addIssueLink", "addWorkLog", "fetchComments"].iter().map(|t| format!("jira:{}?{}", t, auth)).collect();
    let comments = format!("jira:newComments?{}&jql=project = CAMEL&{}", auth, POLL);
    let updates = format!("jira:watchUpdates?{}&jql=project = CAMEL&watchedFields=Status,Summary&{}", auth, POLL);
    let u2 = uris.clone();
    ctx.add_routes(move |b| {
        for (i, u) in u2.iter().enumerate() {
            b.from(&format!("direct:op{}", i)).to(u);
        }
        b.from(&comments).to("mock:comments");
        b.from(&updates).to("mock:updates");
    })
    .unwrap();
    ctx.start().unwrap();
    std::thread::sleep(Duration::from_millis(200)); // consumers record the current state
    let t = ctx.create_producer_template();
    // updateIssue
    t.request_body_and_headers("direct:op0", "new description", vec![("IssueKey", Value::from(k1.clone())), ("IssueSummary", Value::from("first (edited)"))]).unwrap();
    let issue = jira.issues().into_iter().find(|i| i.get("key") == Some(&Value::from(k1.clone()))).unwrap();
    assert_eq!(issue.get("fields").and_then(|f| f.get("summary")), Some(&Value::from("first (edited)")));
    assert_eq!(issue.get("fields").and_then(|f| f.get("description")), Some(&Value::from("new description")));
    // transitionIssue with a comment
    t.request_body_and_headers("direct:op1", "moving on", vec![("IssueKey", Value::from(k2.clone())), ("IssueTransitionId", Value::from("21"))]).unwrap();
    let issue2 = jira.issues().into_iter().find(|i| i.get("key") == Some(&Value::from(k2.clone()))).unwrap();
    assert_eq!(issue2.get("fields").and_then(|f| f.get("status")).and_then(|s| s.get("name")), Some(&Value::from("In Progress")));
    assert!(t.request_body_and_headers("direct:op1", Value::Null, vec![("IssueKey", Value::from(k2.clone())), ("IssueTransitionId", Value::from("99"))]).is_err());
    // attach
    let att = t.request_body_and_headers("direct:op2", Value::Bytes(vec![1, 2, 3, 0, 255]), vec![("IssueKey", Value::from(k1.clone())), ("CamelFileName", Value::from("dir/data.bin"))]).unwrap();
    assert_eq!(att.as_array().unwrap()[0].get("filename"), Some(&Value::from("data.bin")));
    assert_eq!(jira.attachments()[0], (k1.clone(), "data.bin".to_string(), vec![1, 2, 3, 0, 255]));
    // watchers
    let w = t.request_body_and_headers("direct:op3", Value::Null, vec![("IssueKey", Value::from(k1.clone())), ("IssueWatchersAdd", Value::from("ann, bob"))]).unwrap();
    assert_eq!(w.as_array().unwrap().len(), 2);
    let w = t.request_body_and_headers("direct:op3", Value::Null, vec![("IssueKey", Value::from(k1.clone())), ("IssueWatchersRemove", Value::Array(vec![Value::from("ann")]))]).unwrap();
    assert_eq!(w.as_array().unwrap()[0].get("name"), Some(&Value::from("bob")));
    assert_eq!(jira.watchers(), vec![(k1.clone(), "bob".to_string())]);
    // addIssueLink
    t.request_body_and_headers("direct:op4", "related", vec![("ParentIssueKey", Value::from(k1.clone())), ("ChildIssueKey", Value::from(k2.clone())), ("linkType", Value::from("Relates"))]).unwrap();
    assert_eq!(jira.links()[0].get("outwardIssue").and_then(|o| o.get("key")), Some(&Value::from(k2.clone())));
    // addWorkLog
    let wl = t.request_body_and_headers("direct:op5", "worked", vec![("IssueKey", Value::from(k1.clone())), ("minutesSpent", Value::Int(90))]).unwrap();
    assert_eq!(wl.get("timeSpentSeconds"), Some(&Value::Int(5400)));
    // fetchComments: old comment + transition comment on k2 is elsewhere
    let cs = t.request_body_and_header("direct:op6", Value::Null, "IssueKey", k1.clone()).unwrap();
    assert_eq!(cs.as_array().unwrap()[0].get("body"), Some(&Value::from("old comment")));
    // newComments: the transition comment plus one added by someone else
    jira.add_comment(&k1, "fresh comment");
    let m = ctx.mock_endpoint("mock:comments").unwrap();
    let r = wait_for(&m, 2);
    assert_eq!(r[0].body().get("body"), Some(&Value::from("moving on")));
    assert_eq!(r[0].message.headers.get("IssueKey"), Some(&Value::from(k2.clone())));
    assert_eq!(r[1].body().get("body"), Some(&Value::from("fresh comment")));
    // watchUpdates: summary of k1 and status of k2 changed
    let m = ctx.mock_endpoint("mock:updates").unwrap();
    let r = wait_for(&m, 2);
    let mut changes: Vec<(String, String, String)> = r.iter().map(|e| (e.message.headers.get("IssueKey").unwrap().to_string(), e.message.headers.get("IssueChanged").unwrap().to_string(), e.body().to_string())).collect();
    changes.sort();
    assert!(changes.contains(&(k1.clone(), "Summary".into(), "first (edited)".into())), "{:?}", changes);
    assert!(changes.contains(&(k2.clone(), "Status".into(), "In Progress".into())), "{:?}", changes);
    assert_eq!(r[0].message.headers.get("IssueWatchedIssues").and_then(|w| w.as_array()).map(|a| a.len()), Some(2));
    ctx.stop().unwrap();
}

#[test]
fn twilio_calls_queues_and_usage_records() {
    let tw = TwilioMock::start();
    let ctx = ctx();
    let base = format!("username={}&password={}&serverUrl={}", TwilioMock::ACCOUNT_SID, TwilioMock::AUTH_TOKEN, tw.url());
    let routes: Vec<(&str, String)> = vec![
        ("call-create", format!("twilio:call/creator?from=%2B15005550006&url=http://demo.twilio.com/docs/voice.xml&{}", base)),
        ("call-read", format!("twilio:call/reader?{}", base)),
        ("call-update", format!("twilio:call/updater?status=completed&{}", base)),
        ("call-delete", format!("twilio:call/deleter?{}", base)),
        ("queue-create", format!("twilio:queue/create?friendlyName=support&{}", base)),
        ("member-read", format!("twilio:queue-member/read?{}", base)),
        ("usage", format!("twilio:usage-record-today/read?category=calls&{}", base)),
        ("mms", format!("twilio:message/create?from=%2B1&to=%2B2&inBody=mediaUrl&{}", base)),
    ];
    let r2 = routes.clone();
    ctx.add_routes(move |b| {
        for (name, uri) in &r2 {
            b.from(&format!("direct:{}", name)).to(uri);
        }
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let call = t.request_body_and_header("direct:call-create", Value::Null, "CamelTwilio.to", "+15005550001").unwrap();
    let sid = call.get("sid").unwrap().to_string();
    assert!(sid.starts_with("CA"), "{}", sid);
    assert_eq!(call.get("url"), Some(&Value::from("http://demo.twilio.com/docs/voice.xml")));
    assert_eq!(call.get("to"), Some(&Value::from("+15005550001")));
    let calls = t.request_body("direct:call-read", Value::Null).unwrap();
    assert_eq!(calls.as_array().map(|a| a.len()), Some(1));
    let upd = t.request_body_and_header("direct:call-update", Value::Null, "CamelTwilio.pathSid", sid.clone()).unwrap();
    assert_eq!(upd.get("status"), Some(&Value::from("completed")));
    assert_eq!(t.request_body_and_header("direct:call-delete", Value::Null, "CamelTwilio.pathSid", sid.clone()).unwrap(), Value::Bool(true));
    assert!(tw.resources(&format!("Accounts/{}/Calls", TwilioMock::ACCOUNT_SID)).is_empty());
    let q = t.request_body("direct:queue-create", Value::Null).unwrap();
    assert_eq!(q.get("friendly_name"), Some(&Value::from("support")));
    let qsid = q.get("sid").unwrap().to_string();
    let members = t.request_body_and_header("direct:member-read", Value::Null, "CamelTwilio.pathQueueSid", qsid.clone()).unwrap();
    assert_eq!(members, Value::Array(vec![]));
    assert!(tw.server.requests().iter().any(|r| r.path == format!("/2010-04-01/Accounts/{}/Queues/{}/Members.json", TwilioMock::ACCOUNT_SID, qsid)));
    // missing parent path parameter
    let e = t.request_body("direct:member-read", Value::Null).unwrap_err();
    assert!(e.message.contains("pathQueueSid"), "{}", e.message);
    t.request_body("direct:usage", Value::Null).unwrap();
    assert!(tw.server.requests().iter().any(|r| r.path.ends_with("/Usage/Records/Today.json") && r.query_param("Category") == Some("calls")));
    // repeated form fields from a list body
    let mms = t.request_body("direct:mms", Value::Array(vec![Value::from("http://x/1.png"), Value::from("http://x/2.png")])).unwrap();
    assert_eq!(mms.get("media_url").and_then(|m| m.as_array()).map(|a| a.len()), Some(2));
    // unsupported method for an api
    let err = ctx.get_endpoint(&format!("twilio:usage-record/create?{}", base)).err().unwrap();
    assert!(err.message.contains("does not support"), "{}", err.message);
    ctx.stop().unwrap();
}

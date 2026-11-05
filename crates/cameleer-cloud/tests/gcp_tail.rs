//! Remaining Google features against the verifying emulator: OAuth refresh-token user
//! credentials (+ `authorized_user` files and fixed access tokens), the `google-mail-stream`,
//! `google-calendar-stream` and `google-sheets-stream` consumers, the Drive file-change
//! consumer, Pub/Sub dead-letter policies and exactly-once acknowledgements, and BigQuery load /
//! extract jobs.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cameleer_cloud::testing::gcp::PROJECT;
use cameleer_cloud::testing::GcpMock;
use cameleer_core::prelude::*;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_cloud::register(&ctx);
    ctx
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out waiting for {}", what);
        std::thread::sleep(Duration::from_millis(5));
    }
}

struct Env {
    mock: GcpMock,
    key: std::path::PathBuf,
}

impl Env {
    fn new() -> Env {
        let mock = GcpMock::start().unwrap();
        let key = mock.write_service_account_file().unwrap();
        Env { mock, key }
    }
    fn opts(&self) -> String {
        self.mock.endpoint_options(&self.key)
    }
    /// User credential options (refresh token flow) for alice.
    fn user_opts(&self) -> String {
        format!("clientId=camel-app&clientSecret=RAW(app-secret)&refreshToken=RAW(1//refresh-alice)&tokenServerUrl={}&uriEndpointOverride={}&overrideEndpoint=true", self.mock.token_uri(), self.mock.url())
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.key);
    }
}

fn req(t: &ProducerTemplate, uri: &str, body: impl Into<Value>, headers: &[(&str, Value)]) -> Exchange {
    let body = body.into();
    let hs: Vec<(String, Value)> = headers.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    t.request(uri, move |ex| {
        ex.set_body(body.clone());
        for (k, v) in &hs {
            ex.set_header(k.clone(), v.clone());
        }
    })
    .unwrap_or_else(|e| panic!("{}: {:?}", uri, e))
}

fn req_err(t: &ProducerTemplate, uri: &str, body: impl Into<Value>) -> CamelError {
    let body = body.into();
    t.request(uri, move |ex| ex.set_body(body.clone())).unwrap_err().root_cause().clone()
}

fn alice(env: &Env) {
    env.mock.add_user_credentials("camel-app", "app-secret", "1//refresh-alice", "alice@acme.com", &["https://mail.google.com/", "https://www.googleapis.com/auth/calendar", "https://www.googleapis.com/auth/drive", "https://www.googleapis.com/auth/spreadsheets"]);
}

#[test]
fn oauth_refresh_token_user_credentials() {
    let env = Env::new();
    alice(&env);
    env.mock.gmail_deliver("alice@acme.com", "From: bob@acme.com\r\nTo: alice@acme.com\r\nSubject: hello\r\n\r\nhi alice");
    let ctx = context();
    let t = ctx.create_producer_template();
    let before = env.mock.token_requests();
    let list = req(&t, &format!("google-mail://messages/list?{}", env.user_opts()), "", &[]);
    assert_eq!(list.body().as_array().unwrap().len(), 1, "me = the user of the refresh token");
    req(&t, &format!("google-mail://messages/list?{}", env.user_opts()), "", &[]);
    assert_eq!(env.mock.token_requests(), before + 1, "the access token is cached");
    // an authorized_user credentials file
    let file = std::env::temp_dir().join(format!("cameleer-cloud-user-{}.json", std::process::id()));
    std::fs::write(&file, r#"{"type":"authorized_user","client_id":"camel-app","client_secret":"app-secret","refresh_token":"1//refresh-alice"}"#).unwrap();
    let o = format!("serviceAccountKey=file:{}&tokenServerUrl={}&uriEndpointOverride={}&overrideEndpoint=true", file.display(), env.mock.token_uri(), env.mock.url());
    assert_eq!(req(&t, &format!("google-calendar://calendars/get?{}", o), "", &[]).body().get("id").unwrap().to_string(), "primary");
    let _ = std::fs::remove_file(&file);
    // revoked tokens are refreshed after a 401
    env.mock.revoke_tokens();
    req(&t, &format!("google-mail://messages/list?{}", env.user_opts()), "", &[]);
    assert_eq!(env.mock.token_requests(), before + 2, "same client + refresh token share the cached token; refreshed once after the revocation");
    // wrong client secret / unknown refresh token
    let bad = env.user_opts().replace("app-secret", "nope");
    assert!(req_err(&t, &format!("google-mail://messages/list?{}", bad), "").message.contains("Unauthorized"));
    let bad = env.user_opts().replace("refresh-alice", "refresh-mallory");
    assert!(req_err(&t, &format!("google-drive://drive-files/list?{}", bad), "").message.contains("expired or revoked"));
    assert!(ctx.get_endpoint(&format!("google-mail://messages/list?refreshToken=x&uriEndpointOverride={}&overrideEndpoint=true", env.mock.url())).is_err());
    // a fixed access token that the emulator does not know
    assert!(req_err(&t, &format!("google-mail://labels/list?accessToken=RAW(ya29.unknown)&uriEndpointOverride={}&overrideEndpoint=true", env.mock.url()), "").message.contains("401"));
}

#[test]
fn mail_calendar_sheets_streams() {
    let env = Env::new();
    alice(&env);
    let ctx = context();
    let t = ctx.create_producer_template();
    env.mock.gmail_deliver("alice@acme.com", "From: bob@acme.com\r\nTo: alice@acme.com\r\nSubject: first\r\n\r\nbody one");
    env.mock.gmail_deliver("alice@acme.com", "From: carol@acme.com\r\nTo: alice@acme.com\r\nSubject: second\r\n\r\nbody two");
    let sheet = env.mock.create_spreadsheet("Orders");
    req(&t, &format!("google-sheets://data/append?spreadsheetId={}&range=Sheet1!A1&{}", sheet, env.user_opts()), Value::Array(vec![Value::from(vec!["id", "item"]), Value::from(vec!["1", "apple"])]), &[]);
    let o = env.user_opts();
    let sheet2 = sheet.clone();
    ctx.add_routes(move |b| {
        let sheet = sheet2;
        b.from(&format!("google-mail-stream://index?delay=20&initialDelay=0&{}", o)).to("mock:mail");
        b.from(&format!("google-calendar-stream://index?considerLastUpdate=true&delay=20&initialDelay=0&{}", o)).to("mock:cal");
        b.from(&format!("google-sheets-stream://{}?range=Sheet1!A1:B100&splitResults=true&delay=20&initialDelay=0&{}", sheet, o)).to("mock:rows");
    })
    .unwrap();
    let mail = ctx.mock_endpoint("mock:mail").unwrap();
    let cal = ctx.mock_endpoint("mock:cal").unwrap();
    let rows = ctx.mock_endpoint("mock:rows").unwrap();
    ctx.start().unwrap();
    wait_until("two mails", || mail.received_exchanges().len() >= 2);
    wait_until("two rows", || rows.received_exchanges().len() >= 2);
    let m0 = &mail.received_exchanges()[0];
    assert_eq!(m0.body().to_string(), "body one");
    assert_eq!(m0.header("CamelGoogleMailStreamSubject").unwrap().to_string(), "first");
    assert_eq!(m0.header("CamelGoogleMailStreamFrom").unwrap().to_string(), "bob@acme.com");
    // marked as read
    wait_until("marked read", || env.mock.gmail_messages("alice@acme.com").iter().all(|(_, l)| !l.contains(&"UNREAD".to_string())));
    env.mock.gmail_deliver("alice@acme.com", "From: dave@acme.com\r\nTo: alice@acme.com\r\nSubject: third\r\n\r\nbody three");
    wait_until("third mail", || mail.received_exchanges().len() >= 3);
    // calendar: a future event, then a change to it; a past event is ignored (consumeFromNow)
    let future = cameleer_cloud::util::iso8601_millis(cameleer_cloud::util::now_millis() + 86_400_000);
    let later = cameleer_cloud::util::iso8601_millis(cameleer_cloud::util::now_millis() + 90_000_000);
    let past = cameleer_cloud::util::iso8601_millis(cameleer_cloud::util::now_millis() - 86_400_000);
    let ev = req(&t, &format!("google-calendar://events/insert?{}", env.user_opts()), Value::object([("summary", Value::from("Standup")), ("start", Value::object([("dateTime", Value::from(future.clone()))])), ("end", Value::object([("dateTime", Value::from(later))]))]), &[]);
    req(&t, &format!("google-calendar://events/insert?{}", env.user_opts()), Value::object([("summary", Value::from("Old")), ("start", Value::object([("dateTime", Value::from(past.clone()))])), ("end", Value::object([("dateTime", Value::from(past))]))]), &[]);
    let id = ev.body().get("id").unwrap().to_string();
    wait_until("event", || !cal.received_exchanges().is_empty());
    std::thread::sleep(Duration::from_millis(30));
    req(&t, &format!("google-calendar://events/patch?{}", env.user_opts()), Value::object([("summary", Value::from("Standup (moved)"))]), &[("CamelGoogleCalendar.eventId", Value::from(id.clone()))]);
    wait_until("event change", || cal.received_exchanges().len() >= 2);
    std::thread::sleep(Duration::from_millis(100));
    let evs = cal.received_exchanges();
    assert_eq!(evs.len(), 2, "unchanged events are not re-emitted");
    assert_eq!(evs[1].body().get("summary").unwrap().to_string(), "Standup (moved)");
    assert_eq!(evs[1].header("CamelGoogleCalendarStreamEventId").unwrap().to_string(), id);
    // sheets: appended rows only
    req(&t, &format!("google-sheets://data/append?spreadsheetId={}&range=Sheet1!A1&{}", sheet, env.user_opts()), Value::Array(vec![Value::from(vec!["2", "pear"])]), &[]);
    wait_until("third row", || rows.received_exchanges().len() >= 3);
    std::thread::sleep(Duration::from_millis(100));
    let r = rows.received_exchanges();
    assert_eq!(r.len(), 3);
    assert_eq!(r[2].body(), &Value::from(vec!["2", "pear"]));
    assert_eq!(r[2].header("CamelGoogleSheets.valueIndex").unwrap(), &Value::Int(3));
    ctx.stop().unwrap();
    assert_eq!(mail.received_exchanges().len(), 3, "no mail emitted twice");
    assert!(context().get_endpoint(&format!("google-sheets-stream://{}?{}", sheet, env.user_opts())).is_err(), "range is required");
}

#[test]
fn drive_file_change_consumer() {
    let env = Env::new();
    alice(&env);
    let existing = env.mock.put_drive_file("old.txt", "text/plain", b"old");
    let ctx = context();
    let t = ctx.create_producer_template();
    let o = env.user_opts();
    ctx.add_routes({
        let o = o.clone();
        move |b| {
            b.from(&format!("google-drive://drive-files/list?delay=20&initialDelay=0&{}", o)).to("mock:changes");
            b.from(&format!("google-drive://drive-files/list?consumeExisting=false&delay=20&initialDelay=0&{}", o)).to("mock:later");
        }
    })
    .unwrap();
    let m = ctx.mock_endpoint("mock:changes").unwrap();
    let later = ctx.mock_endpoint("mock:later").unwrap();
    ctx.start().unwrap();
    wait_until("existing file", || !m.received_exchanges().is_empty());
    std::thread::sleep(Duration::from_millis(60));
    let created = req(&t, &format!("google-drive://drive-files/create?name=new.txt&mimeType=text/plain&{}", o), "v1", &[]);
    let new_id = created.body().get("id").unwrap().to_string();
    wait_until("created", || m.received_exchanges().len() >= 2);
    std::thread::sleep(Duration::from_millis(10));
    req(&t, &format!("google-drive://drive-files/update?{}", o), "v2", &[("CamelGoogleDrive.fileId", Value::from(new_id.clone()))]);
    wait_until("modified", || m.received_exchanges().len() >= 3);
    req(&t, &format!("google-drive://drive-files/delete?{}", o), "", &[("CamelGoogleDrive.fileId", Value::from(existing.clone()))]);
    wait_until("removed", || m.received_exchanges().len() >= 4);
    let kinds: Vec<(String, String)> = m.received_exchanges().iter().map(|e| (e.header("CamelGoogleDrive.changeType").unwrap().to_string(), e.header("CamelGoogleDrive.fileId").unwrap().to_string())).collect();
    assert_eq!(kinds, vec![("created".to_string(), existing.clone()), ("created".to_string(), new_id.clone()), ("modified".to_string(), new_id.clone()), ("removed".to_string(), existing.clone())]);
    // consumeExisting=false: the files present at its first poll are not reported as created
    let later_kinds = || later.received_exchanges().iter().map(|e| (e.header("CamelGoogleDrive.changeType").unwrap().to_string(), e.header("CamelGoogleDrive.fileId").unwrap().to_string())).collect::<Vec<_>>();
    wait_until("later consumer sees the removal", || later_kinds().contains(&("removed".to_string(), existing.clone())));
    assert!(!later_kinds().contains(&("created".to_string(), existing.clone())));
    ctx.stop().unwrap();
}

#[test]
fn pubsub_dead_letter_policy() {
    let env = Env::new();
    let ctx = context();
    let t = ctx.create_producer_template();
    let ps = |dest: &str, extra: &str| format!("google-pubsub://{}:{}?synchronousPull=true&{}{}", PROJECT, dest, env.opts(), extra);
    req(&t, &ps("orders", "&operation=createTopic"), "", &[]);
    req(&t, &ps("orders-dlq", "&operation=createTopic"), "", &[]);
    req(&t, &ps("dlq-sub", "&operation=createSubscription&topic=orders-dlq"), "", &[]);
    let sub = req(&t, &ps("orders-sub", "&operation=createSubscription&topic=orders&deadLetterTopic=orders-dlq&maxDeliveryAttempts=5"), "", &[]);
    assert_eq!(sub.body().get("deadLetterPolicy").unwrap().get("maxDeliveryAttempts").unwrap(), &Value::Int(5));
    assert!(ctx.get_endpoint(&ps("x", "&maxDeliveryAttempts=2")).is_err(), "5..100");
    let attempts = Arc::new(AtomicUsize::new(0));
    let a2 = attempts.clone();
    let (from, dlq) = (ps("orders-sub", "&delay=5&initialDelay=0"), ps("dlq-sub", "&delay=5&initialDelay=0"));
    ctx.add_routes(move |b| {
        b.from(&from).process(move |ex: &mut Exchange| {
            a2.fetch_add(1, Ordering::SeqCst);
            assert!(ex.header("CamelGooglePubsubDeliveryAttempt").is_some());
            Err(CamelError::illegal_state("cannot process"))
        });
        b.from(&dlq).convert_body_to("String").to("mock:dlq");
    })
    .unwrap();
    let m = ctx.mock_endpoint("mock:dlq").unwrap();
    m.expected_bodies_received(vec!["poison"]);
    ctx.start().unwrap();
    t.send_body(&ps("orders", ""), "poison").unwrap();
    m.assert_is_satisfied_within(Duration::from_secs(10)).unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 5, "delivered maxDeliveryAttempts times");
    let attrs = m.received_exchanges()[0].header("CamelGooglePubsubAttributes").unwrap().clone();
    assert_eq!(attrs.get("CloudPubSubDeadLetterSourceDeliveryCount").unwrap().to_string(), "5");
    ctx.stop().unwrap();
    req(&t, &ps("orders-sub", "&operation=deleteSubscription"), "", &[]);
    assert!(t.request(&ps("orders-sub", "&operation=getSubscription"), |_| {}).is_err());
}

#[test]
fn pubsub_exactly_once_acknowledgements() {
    let env = Env::new();
    let ctx = context();
    let t = ctx.create_producer_template();
    let ps = |dest: &str, extra: &str| format!("google-pubsub://{}:{}?synchronousPull=true&{}{}", PROJECT, dest, env.opts(), extra);
    req(&t, &ps("jobs", "&operation=createTopic"), "", &[]);
    for sub in ["eo-extended", "eo-plain"] {
        let s = req(&t, &ps(sub, "&operation=createSubscription&topic=jobs&enableExactlyOnceDelivery=true&ackDeadlineSeconds=1"), "", &[]);
        assert_eq!(s.body().get("enableExactlyOnceDelivery").unwrap(), &Value::Bool(true));
    }
    let (extended_n, plain_n) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (e2, p2) = (extended_n.clone(), plain_n.clone());
    // processing the first delivery takes longer than the 1 s ack deadline
    let slow_first = |counter: Arc<AtomicUsize>| {
        move |ex: &mut Exchange| -> Result<()> {
            counter.fetch_add(1, Ordering::SeqCst);
            if ex.header("CamelGooglePubsubDeliveryAttempt").and_then(|a| a.as_i64()) == Some(1) {
                std::thread::sleep(Duration::from_millis(1300));
            }
            Ok(())
        }
    };
    let (ext_uri, plain_uri) = (ps("eo-extended", "&ackDeadlineSeconds=1&delay=10&initialDelay=0"), ps("eo-plain", "&ackDeadlineSeconds=1&maxAckExtensionPeriod=0&delay=10&initialDelay=0"));
    ctx.add_routes(move |b| {
        b.from(&ext_uri).process(slow_first(e2));
        b.from(&plain_uri).process(slow_first(p2));
    })
    .unwrap();
    ctx.start().unwrap();
    t.send_body(&ps("jobs", ""), "job-1").unwrap();
    // without lease extension the late ack fails permanently and the message comes back
    wait_until("redelivery", || plain_n.load(Ordering::SeqCst) >= 2);
    wait_until("extended processed", || extended_n.load(Ordering::SeqCst) >= 1);
    wait_until("backlogs drained", || env.mock.backlog(PROJECT, "eo-extended").is_empty() && env.mock.backlog(PROJECT, "eo-plain").is_empty());
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(extended_n.load(Ordering::SeqCst), 1, "deadline extension keeps the lease: processed exactly once");
    assert_eq!(plain_n.load(Ordering::SeqCst), 2);
    ctx.stop().unwrap();
}

#[test]
fn bigquery_load_and_extract_jobs() {
    let env = Env::new();
    env.mock.create_bucket("staging");
    env.mock.put_object("staging", "in/people.csv", b"name,age\nann,42\n\"smith, bob\",7\n");
    env.mock.put_object("staging", "in/more.json", b"{\"name\":\"cy\",\"age\":3}\n");
    env.mock.create_bq_table(PROJECT, "ds", "people", &[("name", "STRING"), ("age", "INTEGER")]);
    env.mock.set_bq_async_polls(0);
    let ctx = context();
    let t = ctx.create_producer_template();
    let bq = |extra: &str| format!("google-bigquery://{}:ds:people?jobPollInterval=5&{}{}", PROJECT, env.opts(), extra);
    let job = req(&t, &bq("&operation=load&sourceUris=gs://staging/in/people.csv&skipLeadingRows=1"), "", &[]);
    assert_eq!(job.body().get("status").unwrap().get("state").unwrap().to_string(), "DONE");
    assert!(job.header("CamelGoogleBigQueryJobId").is_some());
    req(&t, &bq("&operation=load&sourceFormat=NEWLINE_DELIMITED_JSON"), Value::from(vec!["gs://staging/in/more.json"]), &[]);
    let rows = env.mock.bq_rows(PROJECT, "ds", "people");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[1].get("name").unwrap().to_string(), "smith, bob");
    assert_eq!(rows[1].get("age").unwrap(), &Value::Int(7));
    // extract as NDJSON and CSV
    req(&t, &bq("&operation=extract&destinationUris=gs://staging/out/people.json"), "", &[]);
    let out = String::from_utf8(env.mock.object("staging", "out/people.json").unwrap()).unwrap();
    assert_eq!(out.lines().count(), 3);
    assert!(out.contains(r#"{"name":"cy","age":3}"#), "{}", out);
    req(&t, &bq("&operation=extract&destinationFormat=CSV"), "", &[("CamelGoogleBigQueryDestinationUris", Value::from(vec!["gs://staging/out/people.csv"]))]);
    let csv = String::from_utf8(env.mock.object("staging", "out/people.csv").unwrap()).unwrap();
    assert!(csv.starts_with("name,age\nann,42\n\"smith, bob\",7\n"), "{}", csv);
    // WRITE_TRUNCATE replaces the rows
    req(&t, &bq("&operation=load&sourceFormat=NEWLINE_DELIMITED_JSON&writeDisposition=WRITE_TRUNCATE&sourceUris=gs://staging/in/more.json"), "", &[]);
    assert_eq!(env.mock.bq_rows(PROJECT, "ds", "people").len(), 1);
    // job errors
    let e = req_err(&t, &bq("&operation=load&sourceUris=gs://staging/missing.csv"), "");
    assert!(e.message.contains("Not found: URI"), "{}", e.message);
    assert!(req_err(&t, &bq("&operation=load"), "").message.contains("sourceUris"));
}

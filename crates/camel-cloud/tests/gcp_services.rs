//! The newer Google Cloud features end-to-end against the emulator: Pub/Sub streaming pull over
//! gRPC (ack, nack redelivery, ack-deadline redelivery, lease extension, flow control, token
//! verification), Gmail / Calendar / Drive with domain-wide delegation, Vertex AI,
//! Speech-to-Text, Text-to-Speech and Cloud Storage V4 signed URLs.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use camel_cloud::testing::gcp::PROJECT;
use camel_cloud::testing::GcpMock;
use camel_core::prelude::*;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    camel_cloud::register(&ctx);
    ctx
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out waiting for {}", what);
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn s(v: &Value) -> String {
    v.to_string()
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

fn req_err(t: &ProducerTemplate, uri: &str, body: impl Into<Value>, headers: &[(&str, Value)]) -> CamelError {
    let body = body.into();
    let hs: Vec<(String, Value)> = headers.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    t.request(uri, move |ex| {
        ex.set_body(body.clone());
        for (k, v) in &hs {
            ex.set_header(k.clone(), v.clone());
        }
    })
    .unwrap_err()
    .root_cause()
    .clone()
}

#[test]
fn streaming_pull_acks_and_redelivers_nacked_messages() {
    let env = Env::new();
    env.mock.create_subscription(PROJECT, "stream-sub", "events");
    env.mock.publish(PROJECT, "events", b"one", &[("k", "v")]);
    env.mock.publish(PROJECT, "events", b"flaky", &[]);
    let ctx = context();
    let opts = env.opts();
    let failures = Arc::new(AtomicUsize::new(0));
    let f2 = failures.clone();
    ctx.add_routes(move |b| {
        b.from(&format!("google-pubsub://{}:stream-sub?{}", PROJECT, opts))
            .process(move |ex: &mut Exchange| {
                if ex.body().to_bytes() == b"flaky" && f2.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(CamelError::runtime("first attempt fails"));
                }
                Ok(())
            })
            .to("mock:stream");
    })
    .unwrap();
    let out = ctx.mock_endpoint("mock:stream").unwrap();
    out.expected_bodies_received_in_any_order(vec![b"one".to_vec(), b"flaky".to_vec()]);
    ctx.start().unwrap();
    out.assert_is_satisfied_within(Duration::from_secs(10)).unwrap();
    wait_until("acked", || env.mock.backlog(PROJECT, "stream-sub").is_empty());
    let one = out.received_exchanges().into_iter().find(|e| e.body().to_bytes() == b"one").unwrap();
    assert_eq!(one.header("CamelGooglePubsubAttributes").unwrap().get("k").unwrap(), &Value::from("v"));
    assert!(s(one.header("CamelGooglePubsubAckId").unwrap()).starts_with("sack-"));
    assert!(s(one.header("CamelGooglePubsubPublishTime").unwrap()).ends_with('Z'));
    let flaky = out.received_exchanges().into_iter().find(|e| e.body().to_bytes() == b"flaky").unwrap();
    assert_eq!(flaky.header("CamelGooglePubsubDeliveryAttempt").unwrap().as_i64(), Some(2), "redelivered after the nack");
    let stats = env.mock.streaming_stats();
    assert_eq!((stats.acks, stats.nacks), (2, 1), "{:?}", stats);
    assert!(stats.streams >= 1);
    // messages published while the stream is open are pushed too
    out.reset();
    out.expected_bodies_received(vec![b"later".to_vec()]);
    env.mock.publish(PROJECT, "events", b"later", &[]);
    out.assert_is_satisfied_within(Duration::from_secs(10)).unwrap();
    ctx.stop().unwrap();
}

#[test]
fn streaming_pull_redelivers_after_the_ack_deadline() {
    let env = Env::new();
    env.mock.create_subscription(PROJECT, "nack-sub", "t");
    env.mock.publish(PROJECT, "t", b"unacked", &[]);
    let ctx = context();
    let opts = env.opts();
    // ackMode=NONE: never acknowledged, so the 1 s lease expires and the message comes back
    ctx.add_routes(move |b| {
        b.from(&format!("google-pubsub://{}:nack-sub?ackMode=NONE&ackDeadlineSeconds=1&{}", PROJECT, opts)).to("mock:redelivered");
    })
    .unwrap();
    let out = ctx.mock_endpoint("mock:redelivered").unwrap();
    out.expected_minimum_message_count(2);
    ctx.start().unwrap();
    out.assert_is_satisfied_within(Duration::from_secs(10)).unwrap();
    ctx.stop().unwrap();
    let attempts: Vec<i64> = out.received_exchanges().iter().map(|e| e.header("CamelGooglePubsubDeliveryAttempt").and_then(|v| v.as_i64()).unwrap_or(0)).collect();
    assert_eq!(&attempts[..2], &[1, 2]);
    assert_eq!(env.mock.backlog(PROJECT, "nack-sub"), vec!["unacked"], "still not acknowledged");
    assert_eq!(env.mock.streaming_stats().acks, 0);
}

#[test]
fn streaming_pull_extends_leases_and_honours_flow_control() {
    let env = Env::new();
    env.mock.create_subscription(PROJECT, "slow-sub", "t");
    for m in ["a", "b", "c"] {
        env.mock.publish(PROJECT, "t", m.as_bytes(), &[]);
    }
    let ctx = context();
    let opts = env.opts();
    let first = Arc::new(AtomicUsize::new(0));
    let f2 = first.clone();
    // one message outstanding at a time; the first takes longer than its 1 s deadline
    ctx.add_routes(move |b| {
        b.from(&format!("google-pubsub://{}:slow-sub?ackDeadlineSeconds=1&maxOutstandingMessages=1&{}", PROJECT, opts))
            .process(move |_ex: &mut Exchange| {
                if f2.fetch_add(1, Ordering::SeqCst) == 0 {
                    std::thread::sleep(Duration::from_millis(1500));
                }
                Ok(())
            })
            .to("mock:slow");
    })
    .unwrap();
    let out = ctx.mock_endpoint("mock:slow").unwrap();
    out.expected_bodies_received(vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]);
    out.all_messages(|ex| ex.property("CamelBatchSize").and_then(|v| v.as_i64()) == Some(1));
    ctx.start().unwrap();
    out.assert_is_satisfied_within(Duration::from_secs(10)).unwrap();
    wait_until("acked", || env.mock.backlog(PROJECT, "slow-sub").is_empty());
    ctx.stop().unwrap();
    let stats = env.mock.streaming_stats();
    assert!(stats.extensions >= 1, "lease extended while processing: {:?}", stats);
    assert_eq!(stats.delivered, 3, "no redelivery thanks to the extension: {:?}", stats);
    assert_eq!(stats.acks, 3);
}

#[test]
fn streaming_pull_verifies_tokens_and_uses_several_streams() {
    let env = Env::new();
    env.mock.create_subscription(PROJECT, "auth-sub", "t");
    // an untrusted service account cannot get a token: nothing is consumed
    let intruder = std::env::temp_dir().join(format!("camel-intruder-{}.json", camel_cloud::util::random_hex(6)));
    std::fs::write(&intruder, camel_cloud::testing::gcp::service_account_json(&env.mock.token_uri(), "intruder@evil.iam.gserviceaccount.com")).unwrap();
    let bad_opts = env.mock.endpoint_options(&intruder);
    let ctx = context();
    ctx.add_routes(move |b| {
        b.from(&format!("google-pubsub://{}:auth-sub?{}", PROJECT, bad_opts)).to("mock:never");
    })
    .unwrap();
    env.mock.publish(PROJECT, "t", b"secret", &[]);
    ctx.start().unwrap();
    wait_until("token rejection", || env.mock.auth_failures() >= 1);
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(ctx.mock_endpoint("mock:never").unwrap().received_counter(), 0);
    ctx.stop().unwrap();
    let _ = std::fs::remove_file(&intruder);

    // revoked tokens are refreshed after UNAUTHENTICATED; two concurrent streams
    let ctx = context();
    let opts = env.opts();
    // obtain (and cache) a token, then revoke it
    ctx.create_producer_template().send_body(&format!("google-pubsub://{}:t?{}", PROJECT, opts), "warm-up").unwrap();
    env.mock.revoke_tokens();
    ctx.add_routes(move |b| {
        b.from(&format!("google-pubsub://{}:auth-sub?concurrentConsumers=2&{}", PROJECT, opts)).to("mock:auth");
    })
    .unwrap();
    let out = ctx.mock_endpoint("mock:auth").unwrap();
    out.expected_bodies_received_in_any_order(vec![b"secret".to_vec(), b"warm-up".to_vec()]);
    ctx.start().unwrap();
    out.assert_is_satisfied_within(Duration::from_secs(10)).unwrap();
    wait_until("two streams", || env.mock.streaming_stats().streams >= 2);
    let stats = env.mock.streaming_stats();
    assert!(stats.rejected >= 1, "the revoked token was rejected: {:?}", stats);
    ctx.stop().unwrap();
}

#[test]
fn gmail_with_domain_wide_delegation() {
    let env = Env::new();
    env.mock.gmail_deliver("alice@acme.com", "From: carol@acme.com\r\nTo: alice@acme.com\r\nSubject: Lunch\r\n\r\nPizza at noon?");
    let ctx = context();
    let t = ctx.create_producer_template();
    let o = format!("delegate=alice@acme.com&{}", env.opts());
    let msg = Value::object([("to", Value::from(vec!["bob@acme.com"])), ("subject", Value::from("Report")), ("text", Value::from("All good")), ("html", Value::from("<b>All good</b>"))]);
    let ex = req(&t, &format!("google-mail://messages/send?{}", o), msg, &[]);
    assert!(ex.header("CamelGoogleMail.id").is_some());
    assert_eq!(s(&ex.body().get("labelIds").unwrap().as_array().unwrap()[0]), "SENT");
    let bob = env.mock.gmail_messages("bob@acme.com");
    assert_eq!(bob.len(), 1);
    assert!(bob[0].0.contains("Subject: Report") && bob[0].1.contains(&"INBOX".to_string()));
    // a text body with the to / subject options
    req(&t, &format!("google-mail://messages/send?to=bob@acme.com&subject=Ping&{}", o), "pong", &[]);
    assert_eq!(env.mock.gmail_messages("bob@acme.com").len(), 2);
    // list with a query, get with decoded headers / text
    let list = req(&t, &format!("google-mail://messages/list?q=subject:lunch&{}", o), "", &[]);
    let ids = list.body().as_array().unwrap().clone();
    assert_eq!(ids.len(), 1);
    let id = ids[0].get("id").unwrap().clone();
    let got = req(&t, &format!("google-mail://messages/get?{}", o), "", &[("CamelGoogleMail.id", id.clone())]);
    assert_eq!(s(got.header("CamelGoogleMail.subject").unwrap()), "Lunch");
    assert_eq!(s(got.header("CamelGoogleMail.from").unwrap()), "carol@acme.com");
    assert_eq!(s(got.header("CamelGoogleMail.text").unwrap()), "Pizza at noon?");
    req(&t, &format!("google-mail://messages/modify?addLabelIds=Label_1&removeLabelIds=UNREAD&{}", o), "", &[("CamelGoogleMail.id", id.clone())]);
    req(&t, &format!("google-mail://messages/trash?{}", o), "", &[("CamelGoogleMail.id", id.clone())]);
    assert!(env.mock.gmail_messages("alice@acme.com").iter().any(|(_, l)| l.contains(&"TRASH".to_string()) && l.contains(&"Label_1".to_string())));
    let inbox = req(&t, &format!("google-mail://messages/list?labelIds=INBOX&{}", o), "", &[]);
    assert!(inbox.body().as_array().unwrap().is_empty());
    let labels = req(&t, &format!("google-mail://labels/list?{}", o), "", &[]);
    assert!(labels.body().as_array().unwrap().iter().any(|l| s(l.get("id").unwrap()) == "Label_1"));
    // service accounts need a delegated subject for Gmail, and the right scope
    let e = req_err(&t, &format!("google-mail://messages/list?{}", env.opts()), "", &[]);
    assert!(e.is_a("GoogleJsonResponseException") && e.message.contains("FAILED_PRECONDITION"), "{:?}", e);
    let e = req_err(&t, &format!("google-mail://messages/list?scopes=https://www.googleapis.com/auth/calendar&{}", o), "", &[]);
    assert!(e.message.contains("PERMISSION_DENIED"), "{}", e);
    assert!(ctx.get_endpoint("google-mail://messages/explode").is_err());
}

#[test]
fn calendar_and_drive() {
    let env = Env::new();
    let ctx = context();
    let t = ctx.create_producer_template();
    let o = format!("delegate=alice@acme.com&{}", env.opts());
    let ev = |summary: &str, start: &str| Value::object([("summary", Value::from(summary)), ("start", Value::object([("dateTime", start)])), ("end", Value::object([("dateTime", start)]))]);
    let created = req(&t, &format!("google-calendar://events/insert?{}", o), ev("Standup", "2026-11-02T09:00:00Z"), &[]);
    let id = created.body().get("id").unwrap().clone();
    req(&t, &format!("google-calendar://events/insert?{}", o), "", &[("CamelGoogleCalendar.content", ev("Retro", "2026-11-06T15:00:00Z"))]);
    let e = req_err(&t, &format!("google-calendar://events/insert?{}", o), Value::object([("summary", "no times")]), &[]);
    assert!(e.message.contains("Missing end time"), "{}", e);
    let all = req(&t, &format!("google-calendar://events/list?orderBy=startTime&{}", o), "", &[]);
    assert_eq!(all.body().as_array().unwrap().iter().map(|e| s(e.get("summary").unwrap())).collect::<Vec<_>>(), vec!["Standup", "Retro"]);
    let later = req(&t, &format!("google-calendar://events/list?timeMin=2026-11-03T00:00:00Z&{}", o), "", &[]);
    assert_eq!(later.body().as_array().unwrap().len(), 1);
    let patched = req(&t, &format!("google-calendar://events/patch?{}", o), Value::object([("location", "Room 1")]), &[("CamelGoogleCalendar.eventId", id.clone())]);
    assert_eq!(s(patched.body().get("summary").unwrap()), "Standup");
    assert_eq!(s(patched.body().get("location").unwrap()), "Room 1");
    req(&t, &format!("google-calendar://events/delete?{}", o), "", &[("CamelGoogleCalendar.eventId", id.clone())]);
    let e = req_err(&t, &format!("google-calendar://events/get?{}", o), "", &[("CamelGoogleCalendar.eventId", id)]);
    assert!(e.message.contains("404"), "{}", e);
    req(&t, &format!("google-calendar://events/quickAdd?text=Lunch%20tomorrow&{}", o), "", &[]);
    assert_eq!(env.mock.calendar_events("primary").len(), 2);
    assert!(!req(&t, &format!("google-calendar://calendarList/list?{}", o), "", &[]).body().as_array().unwrap().is_empty());

    // drive: multipart upload, query, download, update, copy, delete
    let up = req(&t, &format!("google-drive://drive-files/create?name=report.csv&mimeType=text/csv&{}", o), "a,b\n1,2\n", &[]);
    let fid = up.body().get("id").unwrap().clone();
    assert_eq!(s(up.body().get("mimeType").unwrap()), "text/csv");
    env.mock.put_drive_file("notes.txt", "text/plain", b"hello");
    let found = req(&t, &format!("google-drive://drive-files/list?{}", o), "", &[("CamelGoogleDrive.q", "name contains 'report'".into())]);
    assert_eq!(found.body().as_array().unwrap().len(), 1);
    assert_eq!(req(&t, &format!("google-drive://drive-files/list?pageSize=1&{}", o), "", &[]).body().as_array().unwrap().len(), 2, "all pages");
    let dl = req(&t, &format!("google-drive://drive-files/download?{}", o), "", &[("CamelGoogleDrive.fileId", fid.clone())]);
    assert_eq!(dl.body().to_bytes(), b"a,b\n1,2\n".to_vec());
    req(&t, &format!("google-drive://drive-files/update?{}", o), "a,b\n3,4\n", &[("CamelGoogleDrive.fileId", fid.clone())]);
    let copy = req(&t, &format!("google-drive://drive-files/copy?name=backup.csv&{}", o), "", &[("CamelGoogleDrive.fileId", fid.clone())]);
    assert_eq!(s(copy.body().get("name").unwrap()), "backup.csv");
    assert!(env.mock.drive_files().contains(&("backup.csv".to_string(), b"a,b\n3,4\n".to_vec())));
    req(&t, &format!("google-drive://drive-files/delete?{}", o), "", &[("CamelGoogleDrive.fileId", fid.clone())]);
    let e = req_err(&t, &format!("google-drive://drive-files/get?{}", o), "", &[("CamelGoogleDrive.fileId", fid)]);
    assert!(e.message.contains("404"), "{}", e);
    let e = req_err(&t, &format!("google-drive://drive-files/list?scopes=https://mail.google.com/&{}", o), "", &[]);
    assert!(e.message.contains("PERMISSION_DENIED"), "{}", e);
}

#[test]
fn vertex_ai_generate_content() {
    let env = Env::new();
    let ctx = context();
    let t = ctx.create_producer_template();
    let o = env.opts();
    let uri = format!("google-vertexai:{}:us-central1:gemini-1.5-flash?temperature=0.3&maxOutputTokens=100&{}", PROJECT, o);
    let ex = req(&t, &uri, "What is Apache Camel?", &[]);
    assert_eq!(s(ex.body()), "Echo: What is Apache Camel?");
    assert_eq!(s(ex.header("CamelGoogleVertexAIFinishReason").unwrap()), "STOP");
    assert_eq!(ex.header("CamelGoogleVertexAIPromptTokenCount").unwrap().as_i64(), Some(4));
    let sent = env.mock.vertex_requests().pop().unwrap();
    assert_eq!(sent.get("generationConfig").unwrap().get("temperature").unwrap().as_f64(), Some(0.3));
    // chat history (assistant → model), system instruction
    let chat = Value::Array(vec![Value::object([("role", "user"), ("content", "hi")]), Value::object([("role", "assistant"), ("content", "hello")]), Value::object([("role", "user"), ("content", "and now?")])]);
    let ex = req(&t, &format!("{}&operation=generateChat", uri), chat, &[("CamelGoogleVertexAISystemInstruction", "be nice".into())]);
    assert_eq!(s(ex.body()), "[be nice] Echo: and now?");
    let roles: Vec<String> = env.mock.vertex_requests().pop().unwrap().get("contents").unwrap().as_array().unwrap().iter().map(|c| s(c.get("role").unwrap())).collect();
    assert_eq!(roles, vec!["user", "model", "user"]);
    // multimodal inline data, token limit
    let ex = req(&t, &format!("{}&operation=generateMultimodal", uri), Value::Bytes(vec![1, 2, 3]), &[("CamelGoogleVertexAIPrompt", "describe".into()), ("CamelGoogleVertexAIMediaMimeType", "image/jpeg".into())]);
    assert_eq!(s(ex.body()), "Echo: describe (+image/jpeg, 3 bytes)");
    let ex = req(&t, &format!("google-vertexai:{}:us-central1:gemini-1.5-flash?maxOutputTokens=2&{}", PROJECT, o), "one two three", &[]);
    assert_eq!((s(ex.body()).as_str(), s(ex.header("CamelGoogleVertexAIFinishReason").unwrap()).as_str()), ("Echo: one", "MAX_TOKENS"));
    let n = req(&t, &format!("{}&operation=countTokens", uri), "count these three", &[]);
    assert_eq!(n.body().as_i64(), Some(3));
    let e = req_err(&t, &format!("google-vertexai:{}:us-central1:missing-model?{}", PROJECT, o), "x", &[]);
    assert!(e.is_a("ApiException") && e.message.contains("NOT_FOUND"), "{:?}", e);
    assert!(ctx.get_endpoint("google-vertexai:only-project").is_err());
}

#[test]
fn speech_to_text_and_text_to_speech() {
    let env = Env::new();
    env.mock.put_object("audio", "talk.raw", b"hello world. second sentence");
    let ctx = context();
    let t = ctx.create_producer_template();
    let o = env.opts();
    let ex = req(&t, &format!("google-speech-to-text://stt?sampleRateHertz=16000&{}", o), Value::Bytes(b"turn on the lights".to_vec()), &[]);
    assert_eq!(s(ex.body()), "turn on the lights");
    assert_eq!(ex.header("CamelGoogleSpeechToTextConfidence").unwrap().as_f64(), Some(0.92));
    let ex = req(&t, &format!("google-speech-to-text://stt?{}", o), "gs://audio/talk.raw", &[]);
    assert_eq!(s(ex.body()), "hello world second sentence");
    assert_eq!(ex.header("CamelGoogleSpeechToTextResults").unwrap().as_array().unwrap().len(), 2);
    let ex = req(&t, &format!("google-speech-to-text://stt?operation=longRunningRecognize&{}", o), Value::Bytes(b"long audio".to_vec()), &[]);
    let name = ex.header("CamelGoogleSpeechToTextOperationName").unwrap().clone();
    let first = req(&t, &format!("google-speech-to-text://stt?operation=getOperation&{}", o), "", &[("CamelGoogleSpeechToTextOperationName", name.clone())]);
    assert_eq!(first.header("CamelGoogleSpeechToTextDone").unwrap(), &Value::Bool(false));
    let done = req(&t, &format!("google-speech-to-text://stt?operation=getOperation&{}", o), "", &[("CamelGoogleSpeechToTextOperationName", name)]);
    assert_eq!(s(done.body()), "long audio");
    let e = req_err(&t, &format!("google-speech-to-text://stt?{}", o), "gs://audio/missing.raw", &[]);
    assert!(e.message.contains("INVALID_ARGUMENT"), "{}", e);

    let ex = req(&t, &format!("google-text-to-speech://tts?voiceName=en-US-Neural2-C&audioEncoding=OGG_OPUS&{}", o), "Good morning", &[]);
    assert_eq!(ex.body().to_bytes(), b"MOCKAUDIO:en-US-Neural2-C:OGG_OPUS:text:Good morning".to_vec());
    assert_eq!(s(ex.header("CamelGoogleTextToSpeechAudioEncoding").unwrap()), "OGG_OPUS");
    let ex = req(&t, &format!("google-text-to-speech://tts?{}", o), "<speak>Hi</speak>", &[]);
    assert_eq!(ex.body().to_bytes(), b"MOCKAUDIO:en-US:MP3:ssml:<speak>Hi</speak>".to_vec());
    let voices = req(&t, &format!("google-text-to-speech://tts?operation=listVoices&languageCode=it-IT&{}", o), "", &[]);
    assert_eq!(voices.body().as_array().unwrap().len(), 1);
    // revoked tokens are refreshed transparently; an untrusted key is rejected
    env.mock.revoke_tokens();
    req(&t, &format!("google-text-to-speech://tts?{}", o), "again", &[]);
    let intruder = std::env::temp_dir().join(format!("camel-intruder-{}.json", camel_cloud::util::random_hex(6)));
    std::fs::write(&intruder, camel_cloud::testing::gcp::service_account_json(&env.mock.token_uri(), "intruder@evil.iam.gserviceaccount.com")).unwrap();
    let e = req_err(&t, &format!("google-text-to-speech://tts?{}", env.mock.endpoint_options(&intruder)), "x", &[]);
    assert!(e.is_a("GoogleAuthException"), "{:?}", e);
    let _ = std::fs::remove_file(&intruder);
}

#[test]
fn storage_v4_signed_download_links() {
    let env = Env::new();
    env.mock.put_object("reports", "2026/q3 summary.pdf", b"%PDF-signed");
    let ctx = context();
    let t = ctx.create_producer_template();
    let ex = req(
        &t,
        &format!("google-storage://reports?operation=createDownloadLink&autoCreateBucket=false&{}", env.opts()),
        "",
        &[("CamelGoogleCloudStorageObjectName", "2026/q3 summary.pdf".into()), ("CamelGoogleCloudStorageDownloadLinkExpirationTime", Value::Int(60_000))],
    );
    let link = s(ex.body());
    assert!(link.contains("X-Goog-Algorithm=GOOG4-RSA-SHA256") && link.contains("X-Goog-Expires=60&"), "{}", link);
    assert_eq!(s(ex.header("CamelGoogleCloudStorageDownloadLink").unwrap()), link);
    // no bearer token: the signature alone authorizes the download
    let http = camel_cloud::http::HttpClient::new();
    let get = |u: &str| http.send(&camel_cloud::http::Request::new("GET", camel_cloud::http::Url::parse(u).unwrap())).unwrap();
    let resp = get(&link);
    assert_eq!((resp.status, resp.body.clone()), (200, b"%PDF-signed".to_vec()));
    // tampering with the object or the expiry breaks the signature
    assert_eq!(get(&link.replace("q3%20summary", "q4%20summary")).status, 403);
    assert_eq!(get(&link.replace("X-Goog-Expires=60", "X-Goog-Expires=600")).status, 403);
}

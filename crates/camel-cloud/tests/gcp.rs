//! Google Cloud components end-to-end against the embedded emulator (JWT-verifying OAuth
//! token endpoint, Cloud Storage JSON API, Pub/Sub REST).

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

#[test]
fn storage_put_get_list_copy_delete() {
    let env = Env::new();
    let ctx = context();
    let base = format!("google-storage://media?{}", env.opts());
    let t = ctx.create_producer_template();
    // autoCreateBucket (default true) creates the bucket in the key's project when the producer starts
    let ex = t
        .request(&base, |ex| {
            ex.set_body("picture bytes");
            ex.set_header("CamelGoogleCloudStorageObjectName", "img/cat.jpg");
            ex.set_header("CamelGoogleCloudStorageContentType", "image/jpeg");
        })
        .unwrap();
    assert!(env.mock.bucket_exists("media"));
    assert_eq!(env.mock.object("media", "img/cat.jpg").unwrap(), b"picture bytes");
    assert_eq!(ex.body().get("name").unwrap(), &Value::from("img/cat.jpg"));
    assert_eq!(ex.header("CamelGoogleCloudStorageContentLength").unwrap(), &Value::Int(13));
    assert!(ex.header("CamelGoogleCloudStorageETag").is_some());
    t.send_body(&format!("{}&objectName=notes.txt", base), "notes").unwrap();
    // getObject
    let ex = t
        .request(&format!("{}&operation=getObject", base), |ex| ex.set_header("CamelGoogleCloudStorageObjectName", "img/cat.jpg"))
        .unwrap();
    assert_eq!(ex.body_as::<String>().unwrap(), "picture bytes");
    assert_eq!(ex.header("CamelGoogleCloudStorageContentType").unwrap().to_string(), "image/jpeg");
    // listObjects
    let list = t.request_body(&format!("{}&operation=listObjects", base), "").unwrap();
    let names: Vec<String> = list.as_array().unwrap().iter().map(|o| o.get("name").unwrap().to_string()).collect();
    assert_eq!(names, vec!["img/cat.jpg", "notes.txt"]);
    // copyObject
    env.mock.create_bucket("backup");
    t.request(&format!("{}&operation=copyObject", base), |ex| {
        ex.set_header("CamelGoogleCloudStorageObjectName", "notes.txt");
        ex.set_header("CamelGoogleCloudStorageDestinationBucketName", "backup");
        ex.set_header("CamelGoogleCloudStorageDestinationObjectName", "notes-copy.txt");
    })
    .unwrap();
    assert_eq!(env.mock.object("backup", "notes-copy.txt").unwrap(), b"notes");
    // listBuckets
    let buckets = t.request_body(&format!("{}&operation=listBuckets", base), "").unwrap();
    assert_eq!(buckets.as_array().unwrap().len(), 2);
    // deleteObject; missing object -> StorageException 404
    t.send_body_and_header(&format!("{}&operation=deleteObject", base), "", "CamelGoogleCloudStorageObjectName", "notes.txt").unwrap();
    assert_eq!(env.mock.object_names("media"), vec!["img/cat.jpg"]);
    let e = t.request_body_and_header(&format!("{}&operation=getObject", base), "", "CamelGoogleCloudStorageObjectName", "notes.txt").unwrap_err();
    let e = e.root_cause();
    assert!(e.is_a("StorageException") && e.message.contains("404"), "{}", e);
    // the token was fetched once and cached for all calls
    assert_eq!(env.mock.token_requests(), 1);
    assert_eq!(env.mock.auth_failures(), 0);
}

#[test]
fn storage_consumer_delete_and_move() {
    let env = Env::new();
    env.mock.put_object("uploads", "a.txt", b"A");
    env.mock.put_object("uploads", "b.txt", b"B");
    env.mock.put_object("staging", "s.txt", b"S");
    env.mock.create_bucket("done");
    let ctx = context();
    let opts = env.opts();
    ctx.add_routes(move |b| {
        b.from(&format!("google-storage://uploads?delay=20&initialDelay=0&{}", opts)).to("mock:uploads");
        b.from(&format!("google-storage://staging?moveAfterRead=true&destinationBucket=done&delay=20&initialDelay=0&{}", opts)).to("mock:staged");
    })
    .unwrap();
    let uploads = ctx.mock_endpoint("mock:uploads").unwrap();
    uploads.expected_bodies_received_in_any_order(vec!["A", "B"]);
    uploads.all_messages(|ex| {
        ex.header("CamelGoogleCloudStorageObjectName").is_some()
            && ex.header("CamelGoogleCloudStorageBucketName").map(|b| b.to_string()).as_deref() == Some("uploads")
            && ex.header("CamelGoogleCloudStorageContentLength").and_then(|v| v.as_i64()) == Some(1)
    });
    let staged = ctx.mock_endpoint("mock:staged").unwrap();
    staged.expected_bodies_received(vec!["S"]);
    ctx.start().unwrap();
    MockEndpoint::assert_all_satisfied(&[&uploads, &staged]).unwrap();
    wait_until("objects deleted", || env.mock.object_names("uploads").is_empty());
    wait_until("object moved", || env.mock.object("done", "s.txt").is_some() && env.mock.object_names("staging").is_empty());
    ctx.stop().unwrap();
}

#[test]
fn pubsub_publish_pull_ack() {
    let env = Env::new();
    env.mock.create_topic(PROJECT, "orders");
    env.mock.create_subscription(PROJECT, "orders-sub", "orders");
    env.mock.create_subscription(PROJECT, "audit-sub", "orders");
    let ctx = context();
    let opts = env.opts();
    let t = ctx.create_producer_template();
    let ex = t
        .request(&format!("google-pubsub://{}:orders?{}", PROJECT, opts), |ex| {
            ex.set_body("order 42");
            ex.set_header("CamelGooglePubsubAttributes", Value::object([("region", "eu")]));
            ex.set_header("CamelGooglePubsubOrderingKey", "customer-7");
        })
        .unwrap();
    let id = ex.header("CamelGooglePubsubMessageId").unwrap().to_string();
    assert!(!id.is_empty());
    assert_eq!(env.mock.backlog(PROJECT, "audit-sub"), vec!["order 42"]);
    assert_eq!(env.mock.backlog_attributes(PROJECT, "audit-sub")[0].get("region").unwrap(), &Value::from("eu"));
    // list body -> one message per element
    let ex = t.request(&format!("google-pubsub://{}:orders?{}", PROJECT, opts), |ex| ex.set_body(Value::from(vec!["b1", "b2"]))).unwrap();
    assert_eq!(ex.header("CamelGooglePubsubMessageId").unwrap().as_array().unwrap().len(), 2);
    // consume the subscription: successful exchanges are acked
    ctx.add_routes(move |b| {
        b.from(&format!("google-pubsub://{}:orders-sub?maxMessagesPerPoll=5&delay=10&initialDelay=0&{}", PROJECT, opts)).to("mock:orders");
    })
    .unwrap();
    let orders = ctx.mock_endpoint("mock:orders").unwrap();
    orders.expected_bodies_received(vec!["order 42", "b1", "b2"]);
    let id2 = id.clone();
    orders.message(0, move |ex| {
        ex.header("CamelGooglePubsubMessageId").map(|v| v.to_string()) == Some(id2.clone())
            && ex.header("CamelGooglePubsubAttributes").and_then(|a| a.get("region").cloned()) == Some(Value::from("eu"))
            && ex.header("CamelGooglePubsubAckId").is_some()
            && ex.header("CamelGooglePubsubPublishTime").is_some()
            && ex.header("CamelGooglePubsubOrderingKey").map(|v| v.to_string()).as_deref() == Some("customer-7")
    });
    ctx.start().unwrap();
    orders.assert_is_satisfied().unwrap();
    wait_until("acked", || env.mock.backlog(PROJECT, "orders-sub").is_empty());
    ctx.stop().unwrap();
    // the other subscription still has everything
    assert_eq!(env.mock.backlog(PROJECT, "audit-sub"), vec!["order 42", "b1", "b2"]);
    // publishing to a missing topic -> ApiException
    let e = t.request_body(&format!("google-pubsub://{}:missing?{}", PROJECT, env.opts()), "x").unwrap_err();
    assert!(e.root_cause().is_a("ApiException"), "{}", e);
}

#[test]
fn pubsub_failed_exchange_is_nacked_and_redelivered() {
    let env = Env::new();
    env.mock.create_subscription(PROJECT, "s", "t");
    env.mock.publish(PROJECT, "t", b"flaky", &[]);
    let ctx = context();
    let opts = env.opts();
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let a2 = attempts.clone();
    ctx.add_routes(move |b| {
        b.from(&format!("google-pubsub://{}:s?delay=10&initialDelay=0&{}", PROJECT, opts))
            .process(move |_ex: &mut Exchange| {
                if a2.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    return Err(CamelError::runtime("first attempt fails"));
                }
                Ok(())
            })
            .to("mock:out");
    })
    .unwrap();
    let out = ctx.mock_endpoint("mock:out").unwrap();
    out.expected_bodies_received(vec!["flaky"]);
    out.message(0, |ex| ex.header("CamelGooglePubsubDeliveryAttempt").and_then(|v| v.as_i64()) == Some(2));
    ctx.start().unwrap();
    out.assert_is_satisfied().unwrap();
    wait_until("acked", || env.mock.backlog(PROJECT, "s").is_empty());
    ctx.stop().unwrap();
}

#[test]
fn pubsub_emulator_without_authentication() {
    let mock = GcpMock::start().unwrap();
    mock.set_require_auth(false);
    mock.create_subscription("p", "s", "t");
    let ctx = context();
    let host = mock.url().trim_start_matches("http://").to_string();
    ctx.create_producer_template().send_body(&format!("google-pubsub://p:t?authenticate=false&endpoint={}", host), "no auth").unwrap();
    assert_eq!(mock.backlog("p", "s"), vec!["no auth"]);
    assert_eq!(mock.token_requests(), 0);
}

#[test]
fn tokens_are_verified_and_refreshed() {
    let env = Env::new();
    env.mock.create_bucket("b");
    let ctx = context();
    let uri = format!("google-storage://b?autoCreateBucket=false&operation=listObjects&{}", env.opts());
    let t = ctx.create_producer_template();
    t.request_body(&uri, "").unwrap();
    assert_eq!(env.mock.token_requests(), 1);
    // revoked tokens: the client gets a 401, refreshes its token once and succeeds
    env.mock.revoke_tokens();
    t.request_body(&uri, "").unwrap();
    assert_eq!(env.mock.token_requests(), 2);
    assert_eq!(env.mock.auth_failures(), 1);
}

#[test]
fn untrusted_service_account_key_is_rejected() {
    // a mock whose trusted key differs from the one signing the JWT
    let mock = GcpMock::start().unwrap();
    // a key file for a client email the emulator does not trust
    let json = camel_cloud::testing::gcp::service_account_json(&mock.token_uri(), "intruder@evil.iam.gserviceaccount.com");
    let path = std::env::temp_dir().join(format!("camel-cloud-intruder-{}.json", camel_cloud::util::random_hex(6)));
    std::fs::write(&path, json).unwrap();
    let ctx = context();
    let uri = format!("google-storage://b?autoCreateBucket=false&operation=listObjects&serviceAccountKey=file:{}&uriEndpointOverride={}&overrideEndpoint=true", path.display(), mock.url());
    let e = ctx.create_producer_template().request_body(&uri, "").unwrap_err();
    let _ = std::fs::remove_file(&path);
    let root = e.root_cause();
    assert!(root.is_a("GoogleAuthException") && root.message.contains("Invalid JWT"), "{}", root);
    assert_eq!(mock.token_requests(), 0);
    // trusting the email with a different public key still fails signature verification
    let ss = camel_tls::self_signed("x");
    mock.trust_key("intruder@evil.iam.gserviceaccount.com", ss.key.public_key());
    let json = camel_cloud::testing::gcp::service_account_json(&mock.token_uri(), "intruder@evil.iam.gserviceaccount.com");
    let key = camel_cloud::gcp::ServiceAccountKey::parse(&json).unwrap();
    let e = camel_cloud::gcp::access_token(&key, camel_cloud::gcp::CLOUD_PLATFORM_SCOPE, &camel_cloud::http::HttpClient::new()).unwrap_err();
    assert!(e.message.contains("Invalid JWT Signature"), "{}", e);
}

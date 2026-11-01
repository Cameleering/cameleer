//! Third-party simulators: `google-storage` against fake-gcs-server (HTTP on 14443, HTTPS on
//! 14444, see `sim_cloud_tls.rs`) and the `google-pubsub` streaming-pull consumer against
//! pstest, the Pub/Sub fake of Google's Go client library (gRPC on 18085, HTTP control API on
//! 18086 to create topics / subscriptions, publish and inspect acks). Both without
//! authentication (`authenticate=false`), as emulators are used. Runs only with
//! `CAMEL_SIMULATORS=1`.

use std::time::{Duration, Instant};

use camel_core::prelude::*;
use camel_tls::testing::sim;

const GCS: &str = "uriEndpointOverride=http://127.0.0.1:14443&overrideEndpoint=true&authenticate=false";
const PROJECT: &str = "camel-sim";

fn context() -> CamelContext {
    // the client libraries' default project (no service account key with emulators)
    std::env::set_var("GOOGLE_CLOUD_PROJECT", PROJECT);
    let ctx = CamelContext::new();
    camel_cloud::register(&ctx);
    ctx
}

fn s(v: &Value) -> String {
    v.to_string()
}

fn req(t: &ProducerTemplate, uri: &str, body: impl Into<Value>, headers: &[(&str, Value)]) -> Exchange {
    let body = body.into();
    t.request(uri, |ex| {
        ex.set_body(body.clone());
        for (k, v) in headers {
            ex.set_header(*k, v.clone());
        }
    })
    .unwrap_or_else(|e| panic!("{}: {}", uri, e))
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < Duration::from_secs(30), "timed out waiting for {}", what);
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn bucket(prefix: &str) -> String {
    sim::unique(prefix).to_ascii_lowercase()
}

fn names(t: &ProducerTemplate, b: &str) -> Vec<String> {
    let list = t.request_body(&format!("google-storage://{}?autoCreateBucket=false&operation=listObjects&{}", b, GCS), "").unwrap();
    list.as_array().unwrap().iter().map(|o| s(o.get("name").unwrap())).collect()
}

#[test]
fn sim_gcs_objects_buckets_copy_and_errors() {
    if !sim::enabled() {
        return;
    }
    let ctx = context();
    let t = ctx.create_producer_template();
    let media = bucket("media");
    let base = format!("google-storage://{}?{}", media, GCS);
    // autoCreateBucket creates the bucket in the default project
    let ex = req(&t, &base, "picture bytes", &[("CamelGoogleCloudStorageObjectName", Value::from("img/cat ü+1.jpg")), ("CamelGoogleCloudStorageContentType", Value::from("image/jpeg"))]);
    assert_eq!(s(ex.body().get("name").unwrap()), "img/cat ü+1.jpg");
    assert_eq!(ex.header("CamelGoogleCloudStorageContentLength").unwrap(), &Value::Int(13));
    assert!(ex.header("CamelGoogleCloudStorageETag").is_some());
    t.send_body(&format!("{}&objectName=notes.txt", base), "notes").unwrap();
    let ex = req(&t, &format!("{}&operation=getObject", base), "", &[("CamelGoogleCloudStorageObjectName", Value::from("img/cat ü+1.jpg"))]);
    assert_eq!(ex.body_as::<String>().unwrap(), "picture bytes");
    assert_eq!(s(ex.header("CamelGoogleCloudStorageContentType").unwrap()), "image/jpeg");
    assert_eq!(names(&t, &media), vec!["img/cat ü+1.jpg", "notes.txt"]);
    let some = t.request_body_and_header(&format!("{}&operation=listObjects", base), "", "CamelGoogleCloudStoragePrefix", "img/");
    if let Ok(v) = some {
        assert!(v.as_array().unwrap().len() <= 2);
    }
    // copy to a bucket created by the producer
    let backup = bucket("backup");
    req(&t, &format!("google-storage://{}?autoCreateBucket=false&operation=createBucket&{}", backup, GCS), "", &[]);
    req(&t, &format!("{}&operation=copyObject", base), "", &[("CamelGoogleCloudStorageObjectName", Value::from("notes.txt")), ("CamelGoogleCloudStorageDestinationBucketName", Value::from(backup.clone())), ("CamelGoogleCloudStorageDestinationObjectName", Value::from("notes-copy.txt"))]);
    let ex = req(&t, &format!("google-storage://{}?autoCreateBucket=false&operation=getObject&objectName=notes-copy.txt&{}", backup, GCS), "", &[]);
    assert_eq!(ex.body_as::<String>().unwrap(), "notes");
    let buckets = t.request_body(&format!("{}&operation=listBuckets", base), "").unwrap();
    let bnames: Vec<String> = buckets.as_array().unwrap().iter().map(|b| s(b.get("name").unwrap())).collect();
    assert!(bnames.contains(&media) && bnames.contains(&backup), "{:?}", bnames);
    // a large body goes through a resumable upload session in several chunks
    let big: Vec<u8> = (0..(700 * 1024u32)).map(|i| (i % 253) as u8).collect();
    req(&t, &format!("{}&resumableUploadThreshold=0&resumableChunkSize=262144", base), Value::Bytes(big.clone()), &[("CamelGoogleCloudStorageObjectName", Value::from("big.bin"))]);
    let ex = req(&t, &format!("{}&operation=getObject", base), "", &[("CamelGoogleCloudStorageObjectName", Value::from("big.bin"))]);
    assert_eq!(ex.body_as::<Vec<u8>>().unwrap(), big);
    // deletes and errors
    req(&t, &format!("{}&operation=deleteObject", base), "", &[("CamelGoogleCloudStorageObjectName", Value::from("notes.txt"))]);
    assert_eq!(names(&t, &media), vec!["big.bin", "img/cat ü+1.jpg"]);
    let e = t.request_body_and_header(&format!("{}&operation=getObject", base), "", "CamelGoogleCloudStorageObjectName", "notes.txt").unwrap_err();
    let e = e.root_cause();
    assert!(e.message.contains("404"), "{}", e);
    req(&t, &format!("google-storage://{}?autoCreateBucket=false&operation=deleteObject&objectName=notes-copy.txt&{}", backup, GCS), "", &[]);
    req(&t, &format!("google-storage://{}?autoCreateBucket=false&operation=deleteBucket&{}", backup, GCS), "", &[]);
    let bnames: Vec<String> = t.request_body(&format!("{}&operation=listBuckets", base), "").unwrap().as_array().unwrap().iter().map(|b| s(b.get("name").unwrap())).collect();
    assert!(!bnames.contains(&backup));
}

#[test]
fn sim_gcs_consumer_move_and_delete_after_read() {
    if !sim::enabled() {
        return;
    }
    let ctx = context();
    let t = ctx.create_producer_template();
    let staging = bucket("staging");
    let done = bucket("done");
    for (b, v) in [("in/a.txt", "A"), ("in/b.txt", "B"), ("other.txt", "O")] {
        req(&t, &format!("google-storage://{}?objectName={}&{}", staging, b, GCS), v, &[]);
    }
    req(&t, &format!("google-storage://{}?operation=createBucket&autoCreateBucket=false&{}", done, GCS), "", &[]);
    let (s2, d2) = (staging.clone(), done.clone());
    ctx.add_routes(move |r| {
        r.from(&format!("google-storage://{}?prefix=in/&moveAfterRead=true&destinationBucket={}&delay=50&initialDelay=0&{}", s2, d2, GCS)).to("mock:objects");
    })
    .unwrap();
    let m = ctx.mock_endpoint("mock:objects").unwrap();
    m.expected_bodies_received_in_any_order(vec![b"A".to_vec(), b"B".to_vec()]);
    m.all_messages(|ex| ex.header("CamelGoogleCloudStorageObjectName").is_some_and(|n| n.to_string().starts_with("in/")) && ex.header("CamelGoogleCloudStorageBucketName").is_some());
    ctx.start().unwrap();
    m.assert_is_satisfied_within(Duration::from_secs(30)).unwrap();
    wait_until("moved", || names(&t, &staging) == vec!["other.txt"] && names(&t, &done).len() == 2);
    ctx.stop().unwrap();
}

// ----------------------------------------------------------------------------- Pub/Sub (pstest)

fn control(method: &str, path_query: &str, body: &[u8]) -> String {
    let req = camel_cloud::http::Request::new(method, camel_cloud::http::Url::parse(&format!("http://127.0.0.1:18086{}", path_query)).unwrap()).body(body.to_vec());
    let resp = camel_cloud::http::HttpClient::new().send(&req).unwrap();
    assert!(resp.is_success(), "{} {}: {}", method, path_query, resp.text());
    resp.text()
}

fn messages(topic: &str) -> Vec<Value> {
    let all = camel_core::json::parse(&control("GET", "/messages", b"")).unwrap();
    // pstest lists every topic's messages; ours carry the topic name in an attribute
    all.as_array().unwrap().iter().filter(|m| m.get("attributes").and_then(|a| a.get("topic")).map(|t| t.to_string()) == Some(topic.to_string())).cloned().collect()
}

#[test]
fn sim_pubsub_streaming_pull_acks_attributes_and_redelivery() {
    if !sim::enabled() {
        return;
    }
    let topic = sim::unique("orders");
    let sub = format!("{}-sub", topic);
    control("POST", &format!("/topic?name=projects/{}/topics/{}", PROJECT, topic), b"");
    control("POST", &format!("/subscription?name=projects/{}/subscriptions/{}&topic=projects/{}/topics/{}&ackDeadline=10", PROJECT, sub, PROJECT, topic), b"");
    for (i, body) in ["order 1", "order 2", "poison"].iter().enumerate() {
        control("POST", &format!("/publish?topic=projects/{}/topics/{}&attr.topic={}&attr.n={}", PROJECT, topic, topic, i), body.as_bytes());
    }
    let ctx = context();
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let a2 = attempts.clone();
    let sub2 = sub.clone();
    ctx.add_routes(move |r| {
        r.from(&format!("google-pubsub://{}:{}?authenticate=false&pubsubEndpoint=127.0.0.1:18085&ackDeadlineSeconds=10", PROJECT, sub2))
            .process(move |ex: &mut Exchange| {
                if ex.body_as::<String>()? == "poison" && a2.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 2 {
                    return Err(CamelError::runtime("poison"));
                }
                Ok(())
            })
            .to("mock:orders");
    })
    .unwrap();
    let m = ctx.mock_endpoint("mock:orders").unwrap();
    m.expected_bodies_received_in_any_order(vec!["order 1", "order 2", "poison"]);
    m.all_messages(|ex| ex.header("CamelGooglePubsubMessageId").is_some() && ex.header("CamelGooglePubsubAckId").is_some() && ex.header("CamelGooglePubsubPublishTime").is_some() && ex.header("CamelGooglePubsubAttributes").and_then(|a| a.get("n").cloned()).is_some());
    ctx.start().unwrap();
    m.assert_is_satisfied_within(Duration::from_secs(30)).unwrap();
    // pstest saw every message acknowledged once; the failing one was nacked and delivered again
    wait_until("acks", || messages(&topic).iter().all(|m| m.get("acks").and_then(|v| v.as_i64()) == Some(1)));
    ctx.stop().unwrap();
    let msgs = messages(&topic);
    assert_eq!(msgs.len(), 3);
    let poison = msgs.iter().find(|m| s(m.get("data").unwrap()) == "poison").unwrap();
    assert_eq!(poison.get("deliveries").and_then(|v| v.as_i64()), Some(3), "{}", poison.to_json());
    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 3);
    for m in msgs.iter().filter(|m| s(m.get("data").unwrap()) != "poison") {
        assert_eq!(m.get("deliveries").and_then(|v| v.as_i64()), Some(1), "{}", m.to_json());
    }
}

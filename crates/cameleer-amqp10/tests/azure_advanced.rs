//! `azure-servicebus` sessions, lock renewal, deferral, transactions, Azure AD credentials
//! and AMQP over WebSockets; `azure-eventhubs` load balancing through the checkpoint
//! store's ownership records, epochs and event positions.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cameleer_amqp10::eventhubs::{CheckpointStore, InMemoryCheckpointStore};
use cameleer_amqp10::identity::{AccessToken, TokenCredential};
use cameleer_amqp10::testing::Amqp10Broker;
use cameleer_core::prelude::*;

const KEY_NAME: &str = "RootManageSharedAccessKey";
const KEY: &str = "c2VjcmV0K2tleS8xMjM=";

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_amqp10::register(&ctx);
    ctx
}

fn sb(b: &Amqp10Broker, entity: &str, opts: &str) -> String {
    format!("azure-servicebus:{}?connectionString=RAW({}){}", entity, b.connection_string(KEY_NAME, KEY), opts)
}

fn eh(b: &Amqp10Broker, opts: &str) -> String {
    format!("azure-eventhubs:?connectionString=RAW({};EntityPath=hub){}", b.connection_string(KEY_NAME, KEY), opts)
}

/// A one-thread HTTP server answering Azure AD token requests; records the request bodies.
fn token_server(token: &'static str) -> (u16, Arc<Mutex<Vec<String>>>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { break };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = c.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf).to_string();
                if let Some(i) = text.find("\r\n\r\n") {
                    let len = text.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0))).unwrap_or(0);
                    if buf.len() >= i + 4 + len {
                        break;
                    }
                }
            }
            let text = String::from_utf8_lossy(&buf).to_string();
            s2.lock().unwrap().push(text.clone());
            let ok = text.contains("client_secret=s3cret") || text.contains("Metadata: true");
            let (status, body) = if ok { ("200 OK", format!("{{\"access_token\":\"{}\",\"expires_in\":3600,\"token_type\":\"Bearer\"}}", token)) } else { ("401 Unauthorized", "{\"error\":\"invalid_client\",\"error_description\":\"bad secret\"}".to_string()) };
            let _ = c.write_all(format!("HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", status, body.len(), body).as_bytes());
        }
    });
    (port, seen)
}

#[test]
fn servicebus_sessions_with_state() {
    let b = Amqp10Broker::builder().azure(KEY_NAME, KEY).session_entity("orders").start().unwrap();
    let ctx = context();
    let from = sb(&b, "orders", "&sessionEnabled=true&maxConcurrentCalls=2&sessionIdleTimeout=200");
    ctx.add_routes(move |r| {
        r.from(&from).to("mock:sessions");
    })
    .unwrap();
    let t = ctx.create_producer_template();
    for (sid, body) in [("a", "a1"), ("b", "b1"), ("a", "a2"), ("b", "b2"), ("c", "c1")] {
        t.send_body_and_header(&sb(&b, "orders", ""), body, "CamelAzureServiceBusSessionId", sid).unwrap();
    }
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:sessions").unwrap();
    mock.expected_message_count(5);
    mock.assert_is_satisfied().unwrap();
    // per session, in order
    let exs = mock.received_exchanges();
    for sid in ["a", "b"] {
        let bodies: Vec<String> = exs.iter().filter(|e| e.header("CamelAzureServiceBusSessionId") == Some(&Value::from(sid))).map(|e| e.body().to_string()).collect();
        assert_eq!(bodies, vec![format!("{}1", sid), format!("{}2", sid)]);
    }
    assert!(b.wait_for(Duration::from_secs(5), |b| b.inflight_count() == 0 && b.queue_depth("orders") == 0));
    // a fixed session id
    let ctx2 = context();
    let fixed = sb(&b, "orders", "&sessionId=z");
    ctx2.add_routes(move |r| {
        r.from(&fixed).to("mock:z");
    })
    .unwrap();
    ctx2.start().unwrap();
    let z = ctx2.mock_endpoint("mock:z").unwrap();
    z.expected_bodies_received(vec!["z1"]);
    t.send_body_and_header(&sb(&b, "orders", ""), "y1", "CamelAzureServiceBusSessionId", "y").unwrap();
    t.send_body_and_header(&sb(&b, "orders", ""), "z1", "CamelAzureServiceBusSessionId", "z").unwrap();
    z.assert_is_satisfied().unwrap();
    // session state through the producer
    t.request_body_and_header(&sb(&b, "orders", "&producerOperation=setSessionState"), Value::Bytes(b"checkpoint-7".to_vec()), "CamelAzureServiceBusSessionId", "y").unwrap();
    assert_eq!(b.session_state("orders", "y").as_deref(), Some(&b"checkpoint-7"[..]));
    let out = t.request_body_and_header(&sb(&b, "orders", "&producerOperation=getSessionState"), Value::Null, "CamelAzureServiceBusSessionId", "y").unwrap();
    assert_eq!(out, Value::Bytes(b"checkpoint-7".to_vec()));
    ctx2.stop().unwrap();
    ctx.stop().unwrap();
}

#[test]
fn servicebus_auto_lock_renewal() {
    let b = Amqp10Broker::builder().azure(KEY_NAME, KEY).lock_duration(Duration::from_millis(300)).start().unwrap();
    let ctx = context();
    let attempts = Arc::new(AtomicUsize::new(0));
    let a2 = attempts.clone();
    let from = sb(&b, "slow", "&maxAutoLockRenewDuration=PT10S");
    ctx.add_routes(move |r| {
        r.from(&from)
            .process(move |_: &mut Exchange| {
                a2.fetch_add(1, Ordering::SeqCst);
                // longer than the lock duration: only lock renewal avoids a redelivery
                std::thread::sleep(Duration::from_millis(900));
                Ok(())
            })
            .to("mock:slow");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:slow").unwrap();
    mock.expected_message_count(1);
    ctx.create_producer_template().send_body(&sb(&b, "slow", ""), "long job").unwrap();
    mock.assert_is_satisfied().unwrap();
    assert!(b.wait_for(Duration::from_secs(5), |b| b.inflight_count() == 0 && b.queue_depth("slow") == 0));
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    ctx.stop().unwrap();
}

#[test]
fn servicebus_deferral_and_receive_deferred() {
    let b = Amqp10Broker::builder().azure(KEY_NAME, KEY).start().unwrap();
    let ctx = context();
    let from = sb(&b, "work", "");
    ctx.add_routes(move |r| {
        r.from(&from)
            .process(|ex: &mut Exchange| {
                if ex.body().to_string() == "later" {
                    ex.set_header("CamelAzureServiceBusDisposition", "defer");
                }
                Ok(())
            })
            .to("mock:work");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:work").unwrap();
    mock.expected_bodies_received(vec!["now", "later"]);
    let t = ctx.create_producer_template();
    t.send_body(&sb(&b, "work", ""), "now").unwrap();
    t.send_body(&sb(&b, "work", ""), "later").unwrap();
    mock.assert_is_satisfied().unwrap();
    assert!(b.wait_for(Duration::from_secs(5), |b| b.deferred_count("work") == 1 && b.inflight_count() == 0));
    let seq = mock.exchange(1).unwrap().header("CamelAzureServiceBusSequenceNumber").unwrap().clone();
    let out = t.request_body_and_header(&sb(&b, "work", "&producerOperation=receiveDeferredMessages"), Value::Null, "CamelAzureServiceBusSequenceNumber", seq).unwrap();
    assert_eq!(out, Value::from("later"));
    assert_eq!(b.deferred_count("work"), 0);
    assert!(t.request_body_and_header(&sb(&b, "work", "&producerOperation=receiveDeferredMessages"), Value::Null, "CamelAzureServiceBusSequenceNumber", 999_999).is_err());
    ctx.stop().unwrap();
}

#[test]
fn servicebus_transactional_batches() {
    let b = Amqp10Broker::builder().azure(KEY_NAME, KEY).start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    t.send_body(&sb(&b, "batch", "&transacted=true"), Value::Array(vec![Value::from("1"), Value::from("2"), Value::from("3")])).unwrap();
    assert_eq!(b.queue_depth("batch"), 3);
    assert_eq!(b.transaction_counts(), (1, 0));
    ctx.stop().unwrap();
}

#[test]
fn servicebus_azure_ad_client_secret_and_websockets() {
    let (port, seen) = token_server("aad-token-1");
    let b = Amqp10Broker::builder().azure_jwt("aad-token-1").start().unwrap();
    let ctx = context();
    // the emulator speaks plain AMQP (UseDevelopmentEmulator); no key, an Azure AD client secret
    let cs = format!("Endpoint=sb://127.0.0.1:{}/;UseDevelopmentEmulator=true", b.port());
    let uri = format!("azure-servicebus:aad?connectionString=RAW({})&credentialType=AZURE_IDENTITY&tenantId=t1&clientId=app&clientSecret=s3cret&authorityHost=http://127.0.0.1:{}", cs, port);
    let ws = format!("{}&amqpTransportType=AMQP_WEB_SOCKETS", uri);
    let (from, ws2) = (uri.clone(), ws.clone());
    ctx.add_routes(move |r| {
        r.from(&from).to("mock:aad");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:aad").unwrap();
    mock.expected_bodies_received_in_any_order(vec!["with a token", "over websockets"]);
    let t = ctx.create_producer_template();
    t.send_body(&uri, "with a token").unwrap();
    t.send_body(&ws2, "over websockets").unwrap();
    mock.assert_is_satisfied().unwrap();
    let reqs = seen.lock().unwrap().clone();
    assert!(reqs.iter().any(|r| r.starts_with("POST /t1/oauth2/v2.0/token") && r.contains("scope=https%3A%2F%2Fservicebus.azure.net%2F.default")), "{:?}", reqs);
    // a wrong secret fails with an authentication error
    let bad = format!("azure-servicebus:aad?connectionString=RAW({})&credentialType=AZURE_IDENTITY&tenantId=t1&clientId=app&clientSecret=nope&authorityHost=http://127.0.0.1:{}", cs, port);
    let err = t.send_body(&bad, "x").unwrap_err();
    assert!(err.chain().any(|c| c.message.contains("bad secret")), "{:?}", err);
    ctx.stop().unwrap();
}

struct ShortLived {
    calls: AtomicUsize,
}

impl TokenCredential for ShortLived {
    fn get_token(&self, scope: &str) -> cameleer_core::error::Result<AccessToken> {
        assert_eq!(scope, cameleer_amqp10::identity::EVENT_HUBS_SCOPE);
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(AccessToken { token: "bean-token".into(), expires_on: cameleer_amqp10::util::now_secs() + 1 })
    }
}

#[test]
fn eventhubs_token_credential_bean_is_refreshed() {
    let b = Amqp10Broker::builder().azure_jwt("bean-token").event_hub("hub", 1).start().unwrap();
    let ctx = context();
    let cred = Arc::new(ShortLived { calls: AtomicUsize::new(0) });
    let bean: Arc<dyn TokenCredential> = cred.clone();
    ctx.registry().bind("cred", bean);
    let uri = format!("azure-eventhubs:?connectionString=RAW(Endpoint=sb://127.0.0.1:{}/;UseDevelopmentEmulator=true;EntityPath=hub)&credentialType=TOKEN_CREDENTIAL&tokenCredential=#cred&initialPosition=earliest", b.port());
    let from = uri.clone();
    ctx.add_routes(move |r| {
        r.from(&from).convert_body_to("String").to("mock:bean");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:bean").unwrap();
    mock.expected_bodies_received(vec!["e1"]);
    ctx.create_producer_template().send_body(&uri, "e1").unwrap();
    mock.assert_is_satisfied().unwrap();
    let before = cred.calls.load(Ordering::SeqCst);
    // tokens expiring after one second are put again on $cbs before they expire
    assert!(b.wait_for(Duration::from_secs(5), |_| cred.calls.load(Ordering::SeqCst) >= before + 2));
    ctx.stop().unwrap();
}

#[test]
fn eventhubs_load_balancing_between_consumers() {
    let b = Amqp10Broker::builder().azure(KEY_NAME, KEY).event_hub("hub", 4).start().unwrap();
    let store = Arc::new(InMemoryCheckpointStore::default());
    let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let opts = "&checkpointStore=#store&initialPosition=earliest&checkpointBatchSize=1&loadBalancingUpdateInterval=100&partitionOwnershipExpirationInterval=2000";
    let mk = |owner: &str| {
        let ctx = context();
        let s: Arc<dyn CheckpointStore> = store.clone();
        ctx.registry().bind("store", s);
        let from = eh(&b, &format!("{}&ownerId={}", opts, owner));
        let (seen, owner) = (seen.clone(), owner.to_string());
        ctx.add_routes(move |r| {
            r.from(&from).process(move |ex: &mut Exchange| {
                seen.lock().unwrap().push((owner.clone(), ex.body_as::<String>().unwrap_or_default()));
                Ok(())
            });
        })
        .unwrap();
        ctx
    };
    let c1 = mk("owner-a");
    c1.start().unwrap();
    let owned = |owner: &str| store.list_ownership("127.0.0.1", "hub", "$Default").unwrap().iter().filter(|o| o.owner_id == owner).count();
    assert!(b.wait_for(Duration::from_secs(5), |_| owned("owner-a") == 4));
    let c2 = mk("owner-b");
    c2.start().unwrap();
    // the second processor steals its share: 2 + 2, one receiver (epoch 0) per partition
    assert!(b.wait_for(Duration::from_secs(10), |b| owned("owner-a") == 2 && owned("owner-b") == 2 && (0..4).all(|p| b.partition_receivers("hub", "$Default", p).len() == 1)), "a={} b={}", owned("owner-a"), owned("owner-b"));
    let t = c1.create_producer_template();
    for p in 0..4 {
        t.send_body(&eh(&b, &format!("&partitionId={}", p)), format!("p{}", p)).unwrap();
    }
    assert!(b.wait_for(Duration::from_secs(5), |_| seen.lock().unwrap().len() >= 4));
    std::thread::sleep(Duration::from_millis(200));
    {
        let s = seen.lock().unwrap();
        let mut bodies: Vec<String> = s.iter().map(|x| x.1.clone()).collect();
        bodies.sort();
        assert_eq!(bodies, vec!["p0", "p1", "p2", "p3"], "each event once");
        assert!(s.iter().any(|x| x.0 == "owner-a") && s.iter().any(|x| x.0 == "owner-b"), "{:?}", s);
    }
    // stopping one processor gives its partitions back: the other takes over all four
    c2.stop().unwrap();
    assert!(b.wait_for(Duration::from_secs(10), |_| owned("owner-a") == 4));
    t.send_body(&eh(&b, "&partitionId=3"), "after").unwrap();
    assert!(b.wait_for(Duration::from_secs(5), |_| seen.lock().unwrap().iter().any(|x| x.1 == "after" && x.0 == "owner-a")));
    c1.stop().unwrap();
}

#[test]
fn eventhubs_event_positions_and_owner_level() {
    let b = Amqp10Broker::builder().azure(KEY_NAME, KEY).event_hub("hub", 2).start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    for x in ["a", "b", "c"] {
        t.send_body(&eh(&b, "&partitionId=0"), x).unwrap();
    }
    t.send_body(&eh(&b, "&partitionId=1"), "old").unwrap();
    // partition 0 from sequence number of "b" (inclusive), partition 1 from latest
    let first_seq = {
        let conn_ctx = context();
        let tmp = eh(&b, "&partitionId=0&initialPosition=earliest&consumerGroupName=peek");
        let from = tmp.clone();
        conn_ctx.add_routes(move |r| {
            r.from(&from).to("mock:peek");
        })
        .unwrap();
        conn_ctx.start().unwrap();
        let m = conn_ctx.mock_endpoint("mock:peek").unwrap();
        m.expected_message_count(3);
        m.assert_is_satisfied().unwrap();
        let s = m.exchange(1).unwrap().header("CamelAzureEventHubsSequenceNumber").unwrap().as_i64().unwrap();
        conn_ctx.stop().unwrap();
        s
    };
    let from = eh(&b, &format!("&eventPosition=0=sequenceNumber:{}:inclusive,1=latest&ownerLevel=5&loadBalancing=false", first_seq));
    ctx.add_routes(move |r| {
        r.from(&from).convert_body_to("String").to("mock:pos");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:pos").unwrap();
    mock.expected_bodies_received_in_any_order(vec!["b", "c", "new"]);
    assert!(b.wait_for(Duration::from_secs(5), |b| b.partition_receivers("hub", "$Default", 1) == vec![Some(5)]));
    t.send_body(&eh(&b, "&partitionId=1"), "new").unwrap();
    mock.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
    assert!(cameleer_amqp10::eventhubs::EventPosition::parse("offset:x").is_err());
}

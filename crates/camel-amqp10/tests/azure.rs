//! `azure-servicebus` and `azure-eventhubs` against the embedded broker's Azure emulation
//! (CBS with SAS tokens, peek-lock, dead-lettering, scheduling, partitions, checkpoints).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use camel_amqp10::client::{Connection, ConnectionOptions, LinkOptions};
use camel_amqp10::eventhubs::{CheckpointStore, InMemoryCheckpointStore};
use camel_amqp10::message::Message;
use camel_amqp10::testing::Amqp10Broker;
use camel_core::prelude::*;

const KEY_NAME: &str = "RootManageSharedAccessKey";
const KEY: &str = "c2VjcmV0K2tleS8xMjM=";

fn broker() -> Amqp10Broker {
    Amqp10Broker::builder().azure(KEY_NAME, KEY).max_delivery_count(3).subscription("events", "audit").subscription("events", "billing").event_hub("telemetry", 3).start().unwrap()
}

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    camel_amqp10::register(&ctx);
    ctx
}

fn sb(b: &Amqp10Broker, entity: &str, opts: &str) -> String {
    format!("azure-servicebus:{}?connectionString=RAW({}){}", entity, b.connection_string(KEY_NAME, KEY), opts)
}

fn eh(b: &Amqp10Broker, opts: &str) -> String {
    format!("azure-eventhubs:?connectionString=RAW({};EntityPath=telemetry){}", b.connection_string(KEY_NAME, KEY), opts)
}

#[test]
fn servicebus_send_receive_peek_lock_complete() {
    let b = broker();
    let ctx = context();
    let from = sb(&b, "orders", "");
    ctx.add_routes(move |r| {
        r.from(&from).to("mock:orders");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:orders").unwrap();
    mock.expected_bodies_received(vec!["order-1", "order-2"]);
    mock.expected_header_received("CamelAzureServiceBusDeliveryCount", 0);
    let t = ctx.create_producer_template();
    let mut props = camel_core::value::ObjectMap::new();
    props.insert("region".to_string(), Value::from("EU"));
    t.send_body_and_headers(
        &sb(&b, "orders", ""),
        "order-1",
        vec![("CamelAzureServiceBusMessageId", Value::from("m-1")), ("CamelAzureServiceBusCorrelationId", Value::from("corr")), ("CamelAzureServiceBusApplicationProperties", Value::Object(props))],
    )
    .unwrap();
    t.send_body(&sb(&b, "orders", ""), "order-2").unwrap();
    mock.assert_is_satisfied().unwrap();
    let ex = mock.exchange(0).unwrap();
    assert_eq!(ex.header("CamelAzureServiceBusMessageId"), Some(&Value::from("m-1")));
    assert_eq!(ex.header("CamelAzureServiceBusCorrelationId"), Some(&Value::from("corr")));
    assert!(ex.header("CamelAzureServiceBusSequenceNumber").unwrap().as_i64().unwrap() > 0);
    assert!(ex.header("CamelAzureServiceBusEnqueuedTime").unwrap().as_i64().unwrap() > 0);
    assert!(ex.header("CamelAzureServiceBusLockedUntil").unwrap().as_i64().unwrap() > 0);
    assert_eq!(ex.header("CamelAzureServiceBusLockToken").unwrap().to_string().len(), 36);
    let ap = ex.header("CamelAzureServiceBusApplicationProperties").unwrap();
    assert_eq!(ap.get("region"), Some(&Value::from("EU")));
    assert!(b.wait_for(Duration::from_secs(5), |b| b.inflight_count() == 0 && b.queue_depth("orders") == 0));
    ctx.stop().unwrap();
}

#[test]
fn servicebus_abandon_redelivery_and_dead_letter() {
    let b = broker();
    let ctx = context();
    let attempts = Arc::new(AtomicUsize::new(0));
    let a2 = attempts.clone();
    let (flaky, poison, explicit, dlq) = (sb(&b, "flaky", ""), sb(&b, "poison", ""), sb(&b, "explicit", "&enableDeadLettering=true"), sb(&b, "poison", "&subQueue=DEAD_LETTER_QUEUE"));
    let dlq2 = sb(&b, "explicit", "&subQueue=DEAD_LETTER_QUEUE");
    ctx.add_routes(move |r| {
        r.from(&flaky)
            .process(move |_ex: &mut Exchange| {
                if a2.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(CamelError::new("IOException", "transient"));
                }
                Ok(())
            })
            .to("mock:flaky");
        r.from(&poison).throw_exception("IllegalArgumentException", "poison message");
        r.from(&explicit).throw_exception("IllegalStateException", "cannot handle");
        r.from(&dlq).to("mock:dlq");
        r.from(&dlq2).to("mock:dlq2");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let flaky_mock = ctx.mock_endpoint("mock:flaky").unwrap();
    flaky_mock.expected_bodies_received(vec!["again"]);
    flaky_mock.expected_header_received("CamelAzureServiceBusDeliveryCount", 1);
    t.send_body(&sb(&b, "flaky", ""), "again").unwrap();
    flaky_mock.assert_is_satisfied().unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 2);

    // MaxDeliveryCount (3) exceeded -> $deadletterqueue
    let dlq_mock = ctx.mock_endpoint("mock:dlq").unwrap();
    dlq_mock.expected_bodies_received(vec!["bad"]);
    dlq_mock.expected_header_received("CamelAzureServiceBusDeadLetterReason", "MaxDeliveryCountExceeded");
    dlq_mock.expected_header_received("CamelAzureServiceBusDeadLetterSource", "poison");
    t.send_body(&sb(&b, "poison", ""), "bad").unwrap();
    dlq_mock.assert_is_satisfied().unwrap();

    // enableDeadLettering: dead-lettered on the first failure
    let dlq2_mock = ctx.mock_endpoint("mock:dlq2").unwrap();
    dlq2_mock.expected_bodies_received(vec!["nope"]);
    dlq2_mock.expected_header_received("CamelAzureServiceBusDeadLetterReason", "ProcessingError");
    dlq2_mock.expected_header_received("CamelAzureServiceBusDeliveryCount", 0);
    t.send_body(&sb(&b, "explicit", ""), "nope").unwrap();
    dlq2_mock.assert_is_satisfied().unwrap();
    let ex = dlq2_mock.exchange(0).unwrap();
    assert!(ex.header("CamelAzureServiceBusDeadLetterErrorDescription").unwrap().to_string().contains("cannot handle"));
    ctx.stop().unwrap();
}

#[test]
fn servicebus_scheduled_messages() {
    let b = broker();
    let ctx = context();
    let t = ctx.create_producer_template();
    let when = camel_amqp10::util::now_millis() + 400;
    let ex = t
        .request(&sb(&b, "later", "&producerOperation=scheduleMessages"), |ex| {
            ex.set_body("tick");
            ex.set_header("CamelAzureServiceBusScheduledEnqueueTime", when);
        })
        .unwrap();
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
    assert!(ex.header("CamelAzureServiceBusSequenceNumber").unwrap().as_i64().unwrap() > 0);
    assert_eq!(b.scheduled_count(), 1);
    assert_eq!(b.queue_depth("later"), 0);
    let from = sb(&b, "later", "");
    ctx.add_routes(move |r| {
        r.from(&from).to("mock:later");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:later").unwrap();
    mock.expected_bodies_received(vec!["tick"]);
    mock.assert_is_satisfied().unwrap();
    assert!(camel_amqp10::util::now_millis() >= when);
    let enq = mock.exchange(0).unwrap().header("CamelAzureServiceBusEnqueuedTime").unwrap().as_i64().unwrap();
    assert!(enq >= when, "enqueued {} < scheduled {}", enq, when);
    // the producerOperation header and an ISO time in the past enqueue immediately
    mock.reset();
    mock.expected_bodies_received(vec!["now"]);
    t.send_body_and_headers(
        &sb(&b, "later", ""),
        "now",
        vec![("CamelAzureServiceBusProducerOperation", Value::from("scheduleMessages")), ("CamelAzureServiceBusScheduledEnqueueTime", Value::from("2020-01-01T00:00:00Z"))],
    )
    .unwrap();
    mock.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
}

#[test]
fn servicebus_topics_receive_and_delete_and_peek() {
    let b = broker();
    let ctx = context();
    let (audit, billing, rad, peek) = (
        sb(&b, "events", "&serviceBusType=topic&subscriptionName=audit"),
        sb(&b, "events", "&serviceBusType=topic&subscriptionName=billing&receiveMode=RECEIVE_AND_DELETE"),
        sb(&b, "fast", "&receiveMode=RECEIVE_AND_DELETE"),
        sb(&b, "browse", "&consumerOperation=peekMessages&peekNumMaxMessages=5&delay=50"),
    );
    ctx.add_routes(move |r| {
        r.from(&audit).to("mock:audit");
        r.from(&billing).to("mock:billing");
        r.from(&rad).to("mock:fast");
        r.from(&peek).to("mock:peek");
    })
    .unwrap();
    let t = ctx.create_producer_template();
    t.send_body(&sb(&b, "browse", ""), "b1").unwrap();
    t.send_body(&sb(&b, "browse", ""), "b2").unwrap();
    ctx.start().unwrap();
    let (audit_m, billing_m, fast_m, peek_m) = (ctx.mock_endpoint("mock:audit").unwrap(), ctx.mock_endpoint("mock:billing").unwrap(), ctx.mock_endpoint("mock:fast").unwrap(), ctx.mock_endpoint("mock:peek").unwrap());
    audit_m.expected_bodies_received(vec!["e1"]);
    billing_m.expected_bodies_received(vec!["e1"]);
    fast_m.expected_bodies_received(vec!["f1", "f2"]);
    peek_m.expected_bodies_received(vec!["b1", "b2"]);
    t.send_body(&sb(&b, "events", "&serviceBusType=topic"), "e1").unwrap();
    t.send_body(&sb(&b, "fast", ""), Value::Array(vec![Value::from("f1"), Value::from("f2")])).unwrap();
    MockEndpoint::assert_all_satisfied(&[&audit_m, &billing_m, &fast_m, &peek_m]).unwrap();
    assert!(fast_m.exchange(0).unwrap().header("CamelAzureServiceBusLockToken").is_none());
    // peeked messages stay in the queue
    assert_eq!(b.queue_depth("browse"), 2);
    ctx.stop().unwrap();
}

#[test]
fn wrong_sas_key_is_rejected() {
    let b = broker();
    let ctx = context();
    let t = ctx.create_producer_template();
    let bad = format!("azure-servicebus:orders?connectionString=RAW({})", b.connection_string(KEY_NAME, "d3Jvbmc="));
    let err = t.send_body(&bad, "x").unwrap_err();
    assert!(err.chain().any(|c| c.kind == "SecurityException" && c.message.contains("401")), "{:?}", err);
    let unknown = format!("azure-servicebus:orders?connectionString=RAW({})", b.connection_string("Other", KEY));
    assert!(t.send_body(&unknown, "x").is_err());
    let err = t.send_body(&eh(&b, "").replace(KEY, "d3Jvbmc="), "x").unwrap_err();
    assert!(err.chain().any(|c| c.kind == "SecurityException"), "{:?}", err);
    assert_eq!(b.queue_depth("orders"), 0);
    // attaching without a token is refused with amqp:unauthorized-access
    let conn = Connection::open(&ConnectionOptions::new("127.0.0.1", b.port())).unwrap();
    let s = conn.session().unwrap();
    let err = s.sender(LinkOptions::new("orders")).err().unwrap();
    assert_eq!(err.kind, "SecurityException");
    // a token for one entity does not cover another
    let s = conn.session().unwrap();
    let token = camel_amqp10::util::sas_token("sb://localhost/orders", KEY_NAME, KEY, camel_amqp10::util::now_secs() + 60);
    camel_amqp10::cbs::put_token(&s, "sb://localhost/orders", &token, camel_amqp10::cbs::SAS_TOKEN_TYPE, Duration::from_secs(5)).unwrap();
    s.sender(LinkOptions::new("orders")).unwrap().send_accepted(&Message::text("ok"), Duration::from_secs(5)).unwrap();
    assert!(s.sender(LinkOptions::new("other")).is_err());
    let expired = camel_amqp10::util::sas_token("sb://localhost/x", KEY_NAME, KEY, 1000);
    assert!(camel_amqp10::cbs::put_token(&conn.session().unwrap(), "sb://localhost/x", &expired, camel_amqp10::cbs::SAS_TOKEN_TYPE, Duration::from_secs(5)).is_err());
}

#[test]
fn servicebus_over_tls() {
    let b = Amqp10Broker::builder().azure(KEY_NAME, KEY).tls(camel_tls::testing::server_params().server_config().unwrap()).start().unwrap();
    let ctx = context();
    camel_tls::testing::bind_test_beans(&ctx);
    let cs = b.connection_string(KEY_NAME, KEY);
    assert!(!cs.contains("UseDevelopmentEmulator"));
    let uri = format!("azure-servicebus:secure?connectionString=RAW({})&sslContextParameters=#ssl", cs);
    let from = uri.clone();
    ctx.add_routes(move |r| {
        r.from(&from).to("mock:secure");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:secure").unwrap();
    mock.expected_bodies_received(vec!["amqps"]);
    ctx.create_producer_template().send_body(&uri, "amqps").unwrap();
    mock.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
}

#[test]
fn eventhubs_partitioned_send_receive_with_offsets() {
    let b = broker();
    let ctx = context();
    let from = eh(&b, "&initialPosition=earliest");
    ctx.add_routes(move |r| {
        r.from(&from).convert_body_to("String").to("mock:events");
    })
    .unwrap();
    let t = ctx.create_producer_template();
    // before the consumer starts: earliest picks these up
    t.send_body(&eh(&b, "&partitionId=1"), "p1-a").unwrap();
    t.send_body_and_header(&eh(&b, ""), "p2-a", "CamelAzureEventHubsPartitionId", "2").unwrap();
    for i in 0..3 {
        t.send_body(&eh(&b, "&partitionKey=device-7"), format!("k-{}", i)).unwrap();
    }
    assert_eq!(b.partition_size("telemetry", 1) + b.partition_size("telemetry", 2) + b.partition_size("telemetry", 0), 5);
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:events").unwrap();
    mock.expected_bodies_received_in_any_order(vec!["p1-a", "p2-a", "k-0", "k-1", "k-2", "batch-1", "batch-2"]);
    t.send_body(&eh(&b, "&partitionId=0"), Value::Array(vec![Value::from("batch-1"), Value::from("batch-2")])).unwrap();
    mock.assert_is_satisfied().unwrap();
    let exs = mock.received_exchanges();
    let find = |body: &str| exs.iter().find(|e| e.body().to_string() == body).unwrap().clone();
    assert_eq!(find("p1-a").header("CamelAzureEventHubsPartitionId"), Some(&Value::from("1")));
    assert_eq!(find("p2-a").header("CamelAzureEventHubsPartitionId"), Some(&Value::from("2")));
    // same partition key -> same partition, in order, increasing offsets
    let keyed: Vec<Exchange> = (0..3).map(|i| find(&format!("k-{}", i))).collect();
    let pid = keyed[0].header("CamelAzureEventHubsPartitionId").unwrap().clone();
    assert!(keyed.iter().all(|e| e.header("CamelAzureEventHubsPartitionId") == Some(&pid)));
    assert_eq!(keyed[0].header("CamelAzureEventHubsPartitionKey"), Some(&Value::from("device-7")));
    let offs: Vec<i64> = keyed.iter().map(|e| e.header("CamelAzureEventHubsOffset").unwrap().to_string().parse().unwrap()).collect();
    assert!(offs.windows(2).all(|w| w[0] < w[1]), "{:?}", offs);
    let b1 = find("batch-1");
    assert_eq!(b1.header("CamelAzureEventHubsPartitionId"), Some(&Value::from("0")));
    assert!(b1.header("CamelAzureEventHubsSequenceNumber").is_some());
    assert!(b1.header("CamelAzureEventHubsEnqueuedTime").unwrap().as_i64().unwrap() > 0);
    ctx.stop().unwrap();
}

#[test]
fn eventhubs_checkpoint_resume_and_latest_position() {
    let b = broker();
    let ctx = context();
    let store: Arc<dyn CheckpointStore> = Arc::new(InMemoryCheckpointStore::default());
    ctx.registry().bind("ckpt", store.clone());
    let from = eh(&b, "&partitionId=0&initialPosition=earliest&checkpointStore=#ckpt&checkpointBatchSize=1&consumerGroupName=cg1");
    let latest = eh(&b, "&partitionId=1&consumerGroupName=cg2");
    ctx.add_routes(move |r| {
        r.from(&from).route_id("ckpt").convert_body_to("String").to("mock:ckpt");
        r.from(&latest).route_id("latest").convert_body_to("String").to("mock:latest");
    })
    .unwrap();
    let t = ctx.create_producer_template();
    t.send_body(&eh(&b, "&partitionId=1"), "old").unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:ckpt").unwrap();
    mock.expected_bodies_received(vec!["a", "b", "c"]);
    for x in ["a", "b", "c"] {
        t.send_body(&eh(&b, "&partitionId=0"), x).unwrap();
    }
    mock.assert_is_satisfied().unwrap();
    let last_offset: i64 = mock.exchange(2).unwrap().header("CamelAzureEventHubsOffset").unwrap().to_string().parse().unwrap();
    ctx.stop_route("ckpt").unwrap();
    let cp = store.get("127.0.0.1", "telemetry", "cg1", "0").unwrap();
    assert_eq!(cp.offset, last_offset);
    t.send_body(&eh(&b, "&partitionId=0"), "d").unwrap();
    t.send_body(&eh(&b, "&partitionId=0"), "e").unwrap();
    mock.reset();
    mock.expected_bodies_received(vec!["d", "e"]);
    ctx.start_route("ckpt").unwrap();
    mock.assert_is_satisfied().unwrap();
    // latest: the event sent before the consumer started is skipped
    let lm = ctx.mock_endpoint("mock:latest").unwrap();
    lm.expected_bodies_received(vec!["new"]);
    t.send_body(&eh(&b, "&partitionId=1"), "new").unwrap();
    lm.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
    // unknown partitions are refused
    assert!(t.send_body(&eh(&b, "&partitionId=9"), "x").is_err());
}

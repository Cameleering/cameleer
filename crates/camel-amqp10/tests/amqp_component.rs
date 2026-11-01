//! The `amqp` component (JMS over AMQP 1.0) against the embedded broker.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use camel_amqp10::codec::AmqpValue;
use camel_amqp10::message::Body;
use camel_amqp10::testing::Amqp10Broker;
use camel_core::prelude::*;

fn context(b: &Amqp10Broker) -> CamelContext {
    let ctx = CamelContext::new();
    camel_amqp::register(&ctx);
    camel_amqp10::register(&ctx);
    ctx.properties().set("camel.component.amqp.remoteAddress", b.url());
    ctx
}

#[test]
fn scheme_precedence() {
    // whatever the registration order, `amqp` is the AMQP 1.0 component
    let b = Amqp10Broker::start().unwrap();
    for order in 0..2 {
        let ctx = CamelContext::new();
        if order == 0 {
            camel_amqp::register(&ctx);
            camel_amqp10::register(&ctx);
        } else {
            camel_amqp10::register(&ctx);
            camel_amqp::register(&ctx);
        }
        let ep = ctx.get_endpoint(&format!("amqp:queue:x?remoteAddress={}", b.url())).unwrap();
        assert!(ep.as_any().downcast_ref::<camel_amqp10::jms::AmqpEndpoint>().is_some());
        assert!(ctx.has_component("amqp-stomp"));
    }
}

#[test]
fn queue_round_trip_with_jms_headers() {
    let b = Amqp10Broker::start().unwrap();
    let ctx = context(&b);
    ctx.add_routes(|r| {
        r.from("amqp:queue:orders").to("mock:orders");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:orders").unwrap();
    mock.expected_bodies_received(vec!["hello amqp"]);
    mock.expected_header_received("my.key", "v1");
    mock.expected_header_received("count", 3);
    mock.expected_header_received("JMSType", "order");
    mock.expected_header_received("JMSPriority", 7);
    mock.expected_header_received("JMSDeliveryMode", 2);
    mock.expected_header_received("JMSRedelivered", false);
    mock.expected_header_received("JMSDestination", "orders");
    mock.expected_header_received("JMSCorrelationID", "c-1");
    let t = ctx.create_producer_template();
    t.send_body_and_headers(
        "amqp:queue:orders?priority=7",
        "hello amqp",
        vec![("my.key", Value::from("v1")), ("count", Value::from(3)), ("JMSType", Value::from("order")), ("JMSCorrelationID", Value::from("c-1"))],
    )
    .unwrap();
    mock.assert_is_satisfied().unwrap();
    let ex = mock.exchange(0).unwrap();
    assert!(ex.header("JMSMessageID").unwrap().to_string().starts_with("ID:"));
    assert!(ex.header("JMSTimestamp").unwrap().as_i64().unwrap() > 0);
    // wire format: text message, queue destination, encoded key
    ctx.stop().unwrap();
    t.send_body("amqp:queue:wire", Value::Bytes(vec![1, 2, 3])).unwrap();
    t.send_body_and_header("amqp:queue:wire", "txt", "a.b", "x").unwrap();
    let msgs = b.queue_messages("wire");
    assert_eq!(msgs[0].body, Body::Data(vec![vec![1, 2, 3]]));
    assert_eq!(msgs[0].annotation("x-opt-jms-msg-type"), Some(&AmqpValue::Byte(3)));
    assert_eq!(msgs[1].body, Body::Value(AmqpValue::str("txt")));
    assert_eq!(msgs[1].annotation("x-opt-jms-msg-type"), Some(&AmqpValue::Byte(5)));
    assert_eq!(msgs[1].annotation("x-opt-jms-dest"), Some(&AmqpValue::Byte(0)));
    assert_eq!(msgs[1].app_property("a_DOT_b"), Some(&AmqpValue::str("x")));
    assert!(msgs[1].header.as_ref().unwrap().durable);
}

#[test]
fn competing_consumers_and_topics() {
    let b = Amqp10Broker::start().unwrap();
    let ctx = context(&b);
    ctx.add_routes(|r| {
        r.from("amqp:queue:work?concurrentConsumers=3").to("mock:work");
        r.from("amqp:topic:news").to("mock:sub1");
        r.from("amqp:topic:news").to("mock:sub2");
    })
    .unwrap();
    ctx.start().unwrap();
    assert_eq!(b.consumer_count("work"), 3);
    assert!(b.wait_for(Duration::from_secs(5), |b| b.subscriber_count("news") == 2));
    let work = ctx.mock_endpoint("mock:work").unwrap();
    let bodies: Vec<String> = (0..12).map(|i| format!("job-{}", i)).collect();
    work.expected_bodies_received_in_any_order(bodies.clone());
    let s1 = ctx.mock_endpoint("mock:sub1").unwrap();
    let s2 = ctx.mock_endpoint("mock:sub2").unwrap();
    s1.expected_bodies_received(vec!["breaking"]);
    s2.expected_bodies_received(vec!["breaking"]);
    let t = ctx.create_producer_template();
    for body in &bodies {
        t.send_body("amqp:queue:work", body.as_str()).unwrap();
    }
    t.send_body("amqp:topic:news", "breaking").unwrap();
    MockEndpoint::assert_all_satisfied(&[&work, &s1, &s2]).unwrap();
    ctx.stop().unwrap();
}

#[test]
fn request_reply_in_out() {
    let b = Amqp10Broker::start().unwrap();
    let ctx = context(&b);
    ctx.add_routes(|r| {
        r.from("amqp:queue:upper").transform(simple("${body.toUpperCase()}")).set_header("replied", constant(true));
        r.from("amqp:queue:fails").throw_exception("IllegalStateException", "boom");
        r.from("amqp:queue:fixed").transform(simple("fixed-${body}"));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    assert_eq!(t.request_body("amqp:queue:upper", "hello").unwrap(), Value::from("HELLO"));
    let ex = t.request("amqp:queue:upper", |ex| ex.set_body("again")).unwrap();
    assert_eq!(ex.body(), &Value::from("AGAIN"));
    assert_eq!(ex.header("replied"), Some(&Value::Bool(true)));
    let err = t.request_body("amqp:queue:fails", "x").unwrap_err();
    assert!(err.chain().any(|c| c.kind == "IllegalStateException" && c.message.contains("boom")), "{:?}", err);
    assert_eq!(t.request_body("amqp:queue:fixed?replyTo=replies", "y").unwrap(), Value::from("fixed-y"));
    // no consumer: timeout
    let err = t.request_body("amqp:queue:nobody?requestTimeout=300", "z").unwrap_err();
    assert!(err.chain().any(|c| c.kind == "ExchangeTimedOutException"), "{:?}", err);
    ctx.stop().unwrap();
}

#[test]
fn selector_and_redelivery() {
    let b = Amqp10Broker::start().unwrap();
    let ctx = context(&b);
    let attempts = Arc::new(AtomicUsize::new(0));
    let a2 = attempts.clone();
    ctx.add_routes(move |r| {
        r.from("amqp:queue:events?selector=region = 'EU' AND amount > 10").to("mock:eu");
        r.from("amqp:queue:flaky?acknowledgementModeName=CLIENT_ACKNOWLEDGE")
            .process(move |ex: &mut Exchange| {
                if a2.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(CamelError::new("IOException", "first attempt fails"));
                }
                assert_eq!(ex.header("JMSRedelivered"), Some(&Value::Bool(true)));
                Ok(())
            })
            .to("mock:flaky");
    })
    .unwrap();
    ctx.start().unwrap();
    let eu = ctx.mock_endpoint("mock:eu").unwrap();
    eu.expected_bodies_received(vec!["e2"]);
    let t = ctx.create_producer_template();
    t.send_body_and_headers("amqp:queue:events", "e1", vec![("region", Value::from("EU")), ("amount", Value::from(5))]).unwrap();
    t.send_body_and_headers("amqp:queue:events", "e2", vec![("region", Value::from("EU")), ("amount", Value::from(50))]).unwrap();
    t.send_body_and_headers("amqp:queue:events", "e3", vec![("region", Value::from("US")), ("amount", Value::from(50))]).unwrap();
    eu.assert_is_satisfied().unwrap();
    assert_eq!(b.queue_depth("events"), 2);
    let flaky = ctx.mock_endpoint("mock:flaky").unwrap();
    flaky.expected_bodies_received(vec!["retry me"]);
    flaky.expected_header_received("JMSXDeliveryCount", 2);
    t.send_body("amqp:queue:flaky", "retry me").unwrap();
    flaky.assert_is_satisfied().unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    ctx.stop().unwrap();
    // invalid selector fails at endpoint creation
    assert!(ctx.get_endpoint("amqp:queue:x?selector=a = ").is_err());
}

#[test]
fn durable_subscription_keeps_messages_while_offline() {
    let b = Amqp10Broker::start().unwrap();
    let ctx = context(&b);
    ctx.add_routes(|r| {
        r.from("amqp:topic:prices?clientId=app1&durableSubscriptionName=sub1").route_id("durable").to("mock:prices");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:prices").unwrap();
    let t = ctx.create_producer_template();
    assert!(b.wait_for(Duration::from_secs(5), |b| b.subscriber_count("prices") == 1));
    mock.expected_bodies_received(vec!["p1"]);
    t.send_body("amqp:topic:prices", "p1").unwrap();
    mock.assert_is_satisfied().unwrap();
    ctx.stop_route("durable").unwrap();
    t.send_body("amqp:topic:prices", "p2").unwrap();
    assert_eq!(b.subscriber_count("prices"), 1);
    mock.reset();
    mock.expected_bodies_received(vec!["p2"]);
    ctx.start_route("durable").unwrap();
    mock.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
}

#[test]
fn credentials_and_tls() {
    let b = Amqp10Broker::builder().user("app", "pw").tls(camel_tls::testing::server_params().server_config().unwrap()).start().unwrap();
    let ctx = CamelContext::new();
    camel_amqp10::register(&ctx);
    camel_tls::testing::bind_test_beans(&ctx);
    let uri = format!("amqp:queue:secure?remoteAddress=amqps://localhost:{}&username=app&password=pw&sslContextParameters=#ssl", b.port());
    let from = uri.clone();
    ctx.add_routes(move |r| {
        r.from(&from).to("mock:secure");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:secure").unwrap();
    mock.expected_bodies_received(vec!["tls+sasl"]);
    let t = ctx.create_producer_template();
    t.send_body(&uri, "tls+sasl").unwrap();
    mock.assert_is_satisfied().unwrap();
    let bad = format!("amqp:queue:secure?remoteAddress=amqps://localhost:{}&username=app&password=nope&sslContextParameters=#ssl", b.port());
    let err = t.send_body(&bad, "x").unwrap_err();
    assert!(err.chain().any(|c| c.kind == "AuthenticationFailureException"), "{:?}", err);
    ctx.stop().unwrap();
}

#[test]
fn transacted_consumer_uses_amqp_transactions() {
    let b = Amqp10Broker::start().unwrap();
    let ctx = context(&b);
    let attempts = Arc::new(AtomicUsize::new(0));
    let a2 = attempts.clone();
    ctx.add_routes(move |r| {
        r.from("amqp:queue:tx-in?transacted=true")
            .process(move |ex: &mut Exchange| {
                if a2.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(CamelError::new("IOException", "roll back"));
                }
                assert_eq!(ex.header("JMSRedelivered"), Some(&Value::Bool(true)));
                Ok(())
            })
            .to("mock:tx");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:tx").unwrap();
    mock.expected_bodies_received(vec!["atomic"]);
    let t = ctx.create_producer_template();
    t.send_body("amqp:queue:tx-in?transacted=true", "atomic").unwrap();
    mock.assert_is_satisfied().unwrap();
    // producer txn and consumer commit, consumer rollback
    assert!(b.wait_for(Duration::from_secs(5), |b| b.transaction_counts() == (2, 1)), "{:?}", b.transaction_counts());
    assert!(b.wait_for(Duration::from_secs(5), |b| b.inflight_count() == 0 && b.queue_depth("tx-in") == 0));
    ctx.stop().unwrap();
}

#[test]
fn shared_subscription_members_compete() {
    let b = Amqp10Broker::start().unwrap();
    let ctx = context(&b);
    let count = Arc::new(AtomicUsize::new(0));
    let (c1, c2) = (count.clone(), count.clone());
    ctx.add_routes(move |r| {
        r.from("amqp:topic:feed?clientId=app&subscriptionShared=true&subscriptionDurable=true&subscriptionName=workers&concurrentConsumers=2")
            .process(move |_: &mut Exchange| {
                c1.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .to("mock:shared");
        r.from("amqp:topic:feed?subscriptionShared=true&subscriptionName=other").process(move |_: &mut Exchange| {
            c2.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    assert!(b.wait_for(Duration::from_secs(5), |b| b.subscriber_count("feed") == 2), "one queue per shared subscription");
    let mock = ctx.mock_endpoint("mock:shared").unwrap();
    mock.expected_message_count(6);
    let t = ctx.create_producer_template();
    for i in 0..6 {
        t.send_body("amqp:topic:feed", format!("f{}", i)).unwrap();
    }
    mock.assert_is_satisfied().unwrap();
    // each message once per subscription (6 + 6), never twice within a subscription
    assert!(b.wait_for(Duration::from_secs(5), |_| count.load(Ordering::SeqCst) == 12));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(mock.received_counter(), 6);
    ctx.stop().unwrap();
}

#[test]
fn object_messages_and_connection_factory_beans() {
    let b = Amqp10Broker::builder().user("app", "pw").start().unwrap();
    let ctx = CamelContext::new();
    camel_amqp10::register(&ctx);
    ctx.registry().bind("cf", camel_amqp10::jms::AmqpConnectionFactory::new(&b.url()).credentials("app", "pw").client_id("cf-client"));
    ctx.properties().set("camel.component.amqp.connectionFactory", "#cf");
    ctx.add_routes(|r| {
        r.from("amqp:queue:objects?jmsMessageType=Object").to("mock:objects");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:objects").unwrap();
    mock.expected_message_count(1);
    let mut map = camel_core::value::ObjectMap::new();
    map.insert("id".into(), Value::Int(7));
    map.insert("raw".into(), Value::Bytes(vec![0, 1, 2]));
    map.insert("tags".into(), Value::Array(vec![Value::from("a"), Value::Float(1.5)]));
    let t = ctx.create_producer_template();
    t.send_body("amqp:queue:objects?jmsMessageType=Object", Value::Object(map.clone())).unwrap();
    mock.assert_is_satisfied().unwrap();
    assert_eq!(mock.exchange(0).unwrap().body(), &Value::Object(map));
    // a missing bean is an error
    assert!(ctx.get_endpoint("amqp:queue:x?connectionFactory=#missing").is_err());
    ctx.stop().unwrap();
}

#[test]
fn amqp_over_websocket_urls() {
    let b = Amqp10Broker::start().unwrap();
    let ctx = CamelContext::new();
    camel_amqp10::register(&ctx);
    let uri = format!("amqp:queue:ws?remoteAddress=amqpws://127.0.0.1:{}/amqp", b.port());
    let from = uri.clone();
    ctx.add_routes(move |r| {
        r.from(&from).to("mock:ws");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:ws").unwrap();
    mock.expected_bodies_received(vec!["over websockets"]);
    ctx.create_producer_template().send_body(&uri, "over websockets").unwrap();
    mock.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
}

/// Regression (live RabbitMQ 3.12): a broker without `ANONYMOUS-RELAY` refuses the consumer's
/// anonymous reply link; consumers must still start and reply through per-address links.
#[test]
fn consumers_work_without_anonymous_relay() {
    let b = Amqp10Broker::builder().anonymous_relay(false).start().unwrap();
    let ctx = context(&b);
    ctx.add_routes(|r| {
        r.from("amqp:queue:upper").transform(simple("${body.toUpperCase()}")).to("mock:upper");
    })
    .unwrap();
    ctx.start().unwrap();
    assert!(b.wait_for(Duration::from_secs(5), |b| b.consumer_count("upper") == 1), "the consumer started");
    let t = ctx.create_producer_template();
    assert_eq!(t.request_body("amqp:queue:upper?replyTo=replies", "hello").unwrap(), Value::from("HELLO"));
    assert_eq!(t.request_body("amqp:queue:upper?replyTo=replies", "again").unwrap(), Value::from("AGAIN"));
    ctx.stop().unwrap();
}

/// ActiveMQ 5 types nodes by the `queue://` / `topic://` prefixes it announces in its open
/// frame and ignores the `topic` capability: two topic routes were competing consumers of a
/// queue (found against ActiveMQ 5.17.6).
#[test]
fn activemq_flavoured_broker_address_prefixes() {
    let b = Amqp10Broker::builder().activemq().start().unwrap();
    let ctx = context(&b);
    ctx.add_routes(|r| {
        r.from("amqp:topic:news").to("mock:sub1");
        r.from("amqp:topic:news").to("mock:sub2");
        r.from("amqp:queue:orders").to("mock:orders");
        r.from("amqp:queue:upper").transform(simple("${body.toUpperCase()}"));
        r.from("amqp:queue:fixed").transform(simple("fixed-${body}"));
    })
    .unwrap();
    ctx.start().unwrap();
    assert!(b.wait_for(Duration::from_secs(5), |b| b.subscriber_count("topic://news") == 2));
    assert_eq!(b.consumer_count("news"), 0, "no queue named like the topic");
    let (s1, s2, orders) = (ctx.mock_endpoint("mock:sub1").unwrap(), ctx.mock_endpoint("mock:sub2").unwrap(), ctx.mock_endpoint("mock:orders").unwrap());
    s1.expected_bodies_received(vec!["breaking"]);
    s2.expected_bodies_received(vec!["breaking"]);
    s1.expected_header_received("JMSDestination", "topic:news");
    orders.expected_bodies_received(vec!["o1"]);
    orders.expected_header_received("JMSDestination", "orders");
    orders.expected_header_received("JMSReplyTo", "topic:answers");
    let t = ctx.create_producer_template();
    t.send_body("amqp:topic:news", "breaking").unwrap();
    t.send_body_and_headers("amqp:queue:orders", "o1", vec![("JMSReplyTo", Value::from("topic:answers"))]).unwrap();
    MockEndpoint::assert_all_satisfied(&[&s1, &s2, &orders]).unwrap();
    // request/reply over dynamic and fixed (prefixed) reply queues
    assert_eq!(t.request_body("amqp:queue:upper", "hello").unwrap(), Value::from("HELLO"));
    assert_eq!(t.request_body("amqp:queue:fixed?replyTo=replies", "y").unwrap(), Value::from("fixed-y"));
    ctx.stop().unwrap();
    // JMS 2.0 shared subscriptions are refused instead of silently duplicating messages
    let ctx = context(&b);
    ctx.add_routes(|r| {
        r.from("amqp:topic:feed?subscriptionShared=true&subscriptionName=workers&concurrentConsumers=2").to("mock:never");
    })
    .unwrap();
    let err = ctx.start().unwrap_err();
    assert!(err.chain().any(|c| c.kind == "UnsupportedOperationException" && c.message.contains("shared subscriptions")), "{:?}", err);
    let _ = ctx.stop();
}

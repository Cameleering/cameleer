//! `spring-rabbitmq` and `rabbitmq` components against the embedded AMQP broker.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use cameleer_amqp::testing::AmqpBroker;
use cameleer_core::prelude::*;

fn context(broker: &AmqpBroker) -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_amqp::register(&ctx);
    ctx.properties().set("camel.component.spring-rabbitmq.host", "127.0.0.1");
    ctx.properties().set("camel.component.spring-rabbitmq.port", broker.port().to_string());
    ctx.properties().set("camel.component.rabbitmq.hostname", "127.0.0.1");
    ctx.properties().set("camel.component.rabbitmq.portNumber", broker.port().to_string());
    ctx
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < Duration::from_secs(5), "timed out waiting for {}", what);
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn spring_direct_routing_headers_and_properties() {
    let broker = AmqpBroker::start().unwrap();
    let ctx = context(&broker);
    ctx.add_routes(|b| {
        b.from("spring-rabbitmq:orders?queues=eu-orders&routingKey=eu").to("mock:eu");
        b.from("spring-rabbitmq:orders?queues=us-orders&routingKey=us").to("mock:us");
    })
    .unwrap();
    ctx.start().unwrap();
    assert_eq!(broker.exchange_type("orders").as_deref(), Some("direct"));
    assert_eq!(broker.bindings("orders").len(), 2);
    let eu = ctx.mock_endpoint("mock:eu").unwrap();
    let us = ctx.mock_endpoint("mock:us").unwrap();
    eu.expected_bodies_received(vec!["order-1", "order-3"]);
    us.expected_bodies_received(vec!["order-2"]);
    eu.message(0, |ex| {
        ex.header("CamelRabbitmqRoutingKey").map(|v| v.to_string()).as_deref() == Some("eu")
            && ex.header("CamelRabbitmqExchangeName").map(|v| v.to_string()).as_deref() == Some("orders")
            && ex.header("CamelSpringRabbitmqDeliveryTag").and_then(|v| v.as_i64()).is_some()
            && ex.header("CamelRabbitmqDeliveryTag").and_then(|v| v.as_i64()).is_some()
            && ex.header("CamelRabbitmqRedeliveryTag").and_then(|v| v.as_bool()) == Some(false)
            && ex.header("customer").map(|v| v.to_string()).as_deref() == Some("ACME")
            && ex.header("quantity").and_then(|v| v.as_i64()) == Some(42)
            && ex.header("CamelRabbitmqCorrelationId").map(|v| v.to_string()).as_deref() == Some("corr-1")
            && ex.header("CamelRabbitmqPriority").and_then(|v| v.as_i64()) == Some(6)
            && ex.header("CamelRabbitmqContentType").map(|v| v.to_string()).as_deref() == Some("text/plain")
            && ex.header("CamelRabbitmqDeliveryMode").and_then(|v| v.as_i64()) == Some(2)
            && matches!(ex.body(), Value::String(_))
    });
    let t = ctx.create_producer_template();
    t.send_body_and_headers(
        "spring-rabbitmq:orders?routingKey=eu",
        "order-1",
        vec![
            ("customer", Value::from("ACME")),
            ("quantity", Value::Int(42)),
            ("CamelRabbitmqCorrelationId", Value::from("corr-1")),
            ("CamelRabbitmqPriority", Value::Int(6)),
            ("CamelInternal", Value::from("not sent")),
        ],
    )
    .unwrap();
    t.send_body("spring-rabbitmq:orders?routingKey=us", "order-2").unwrap();
    t.send_body_and_header("spring-rabbitmq:orders?routingKey=us", "order-3", "CamelSpringRabbitmqRoutingOverrideKey", "eu").unwrap();
    MockEndpoint::assert_all_satisfied(&[&eu, &us]).unwrap();
    assert!(eu.exchange(0).unwrap().header("CamelInternal").is_none());
    // null bodies are refused unless allowNullBody
    let ex = t.send("spring-rabbitmq:orders?routingKey=eu", Exchange::new(Some(ctx.clone())));
    assert_eq!(ex.exception.as_ref().unwrap().kind, "MessageConversionException");
    ctx.stop().unwrap();
    wait_until("consumers gone", || broker.consumer_count("eu-orders") == 0);
    broker.stop();
}

#[test]
fn topic_and_fanout_exchanges() {
    let broker = AmqpBroker::start().unwrap();
    let ctx = context(&broker);
    ctx.add_routes(|b| {
        b.from("spring-rabbitmq:events?exchangeType=topic&queues=all-events&routingKey=events.#").to("mock:all");
        b.from("spring-rabbitmq:events?exchangeType=topic&queues=eu-events&routingKey=events.eu.*").to("mock:eu");
        b.from("spring-rabbitmq:news?exchangeType=fanout&queues=news-a").to("mock:a");
        b.from("spring-rabbitmq:news?exchangeType=fanout&queues=news-b").to("mock:b");
        b.from("rabbitmq:legacy-fanout?exchangeType=fanout&queue=lf1&autoDelete=false").to("mock:lf1");
        b.from("rabbitmq:legacy-fanout?exchangeType=fanout&queue=lf2&autoDelete=false").to("mock:lf2");
    })
    .unwrap();
    ctx.start().unwrap();
    let m = |n: &str| ctx.mock_endpoint(n).unwrap();
    let (all, eu, a, b, lf1, lf2) = (m("mock:all"), m("mock:eu"), m("mock:a"), m("mock:b"), m("mock:lf1"), m("mock:lf2"));
    all.expected_bodies_received_in_any_order(vec!["de", "us", "deep"]);
    eu.expected_bodies_received(vec!["de"]);
    a.expected_bodies_received(vec!["breaking"]);
    b.expected_bodies_received(vec!["breaking"]);
    lf1.expected_bodies_received(vec!["legacy"]);
    lf2.expected_bodies_received(vec!["legacy"]);
    let t = ctx.create_producer_template();
    t.send_body("spring-rabbitmq:events?exchangeType=topic&routingKey=events.eu.de", "de").unwrap();
    t.send_body("spring-rabbitmq:events?exchangeType=topic&routingKey=events.us.ny", "us").unwrap();
    t.send_body("spring-rabbitmq:events?exchangeType=topic&routingKey=events.eu.de.berlin", "deep").unwrap();
    t.send_body("spring-rabbitmq:news?exchangeType=fanout", "breaking").unwrap();
    t.send_body("rabbitmq:legacy-fanout?exchangeType=fanout", "legacy").unwrap();
    MockEndpoint::assert_all_satisfied(&[&all, &eu, &a, &b, &lf1, &lf2]).unwrap();
    // legacy consumers get bytes (no content type) plus the routing headers
    let ex = lf1.exchange(0).unwrap();
    assert!(matches!(ex.body(), Value::Bytes(_)));
    assert_eq!(ex.header("CamelRabbitmqExchangeName").unwrap().to_string(), "legacy-fanout");
    ctx.stop().unwrap();
}

#[test]
fn legacy_manual_ack_reject_requeue() {
    let broker = AmqpBroker::start().unwrap();
    let ctx = context(&broker);
    let attempts = Arc::new(AtomicUsize::new(0));
    let a2 = attempts.clone();
    ctx.add_routes(move |b| {
        b.from("rabbitmq:work?queue=tasks&routingKey=task&autoAck=false&autoDelete=false")
            .process(move |ex: &mut Exchange| {
                if a2.fetch_add(1, Ordering::SeqCst) == 0 {
                    ex.set_header("CamelRabbitmqRequeue", true);
                    return Err(CamelError::illegal_state("first attempt fails"));
                }
                Ok(())
            })
            .to("mock:done");
    })
    .unwrap();
    ctx.start().unwrap();
    let done = ctx.mock_endpoint("mock:done").unwrap();
    done.expected_bodies_received(vec!["job"]);
    done.expected_header_received("CamelRabbitmqRedeliveryTag", true);
    ctx.create_producer_template().send_body("rabbitmq:work?routingKey=task", "job").unwrap();
    done.assert_is_satisfied().unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    wait_until("acked", || broker.unacked_count("tasks") == 0 && broker.queue_depth("tasks") == 0);
    ctx.stop().unwrap();
}

#[test]
fn spring_retries_then_dead_letters() {
    let broker = AmqpBroker::start().unwrap();
    let ctx = context(&broker);
    let attempts = Arc::new(AtomicUsize::new(0));
    let a2 = attempts.clone();
    ctx.add_routes(move |b| {
        b.from("spring-rabbitmq:payments?queues=payments&routingKey=pay&deadLetterExchange=payments.dlx&deadLetterQueue=payments.dlq&deadLetterRoutingKey=dead&maximumRetryAttempts=3&retryDelay=5")
            .process(move |_ex: &mut Exchange| {
                a2.fetch_add(1, Ordering::SeqCst);
                Err(CamelError::illegal_state("cannot pay"))
            });
        b.from("spring-rabbitmq:payments.dlx?queues=payments.dlq&routingKey=dead").to("mock:dead");
    })
    .unwrap();
    ctx.start().unwrap();
    let dead = ctx.mock_endpoint("mock:dead").unwrap();
    dead.expected_bodies_received(vec!["$100"]);
    dead.message(0, |ex| {
        let death = ex.header("x-death").and_then(|v| v.as_array().cloned()).unwrap_or_default();
        death.first().and_then(|d| d.get("reason")).map(|v| v.to_string()).as_deref() == Some("rejected")
            && ex.header("x-first-death-queue").map(|v| v.to_string()).as_deref() == Some("payments")
    });
    ctx.create_producer_template().send_body("spring-rabbitmq:payments?routingKey=pay", "$100").unwrap();
    dead.assert_is_satisfied().unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 3, "maximumRetryAttempts in-process attempts");
    assert_eq!(broker.queue_depth("payments"), 0);
    ctx.stop().unwrap();
}

#[test]
fn spring_prefetch_limits_unacked_messages() {
    let broker = AmqpBroker::start().unwrap();
    let ctx = context(&broker);
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let g2 = gate.clone();
    ctx.add_routes(move |b| {
        b.from("spring-rabbitmq:bulk?queues=bulk&routingKey=b&prefetchCount=2")
            .process(move |_ex: &mut Exchange| {
                let (m, c) = &*g2;
                let mut open = m.lock().unwrap();
                while !*open {
                    open = c.wait(open).unwrap();
                }
                Ok(())
            })
            .to("mock:bulk");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for i in 0..5 {
        t.send_body("spring-rabbitmq:bulk?routingKey=b", format!("m{}", i)).unwrap();
    }
    wait_until("prefetch window filled", || broker.unacked_count("bulk") == 2 && broker.queue_depth("bulk") == 3);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!((broker.unacked_count("bulk"), broker.queue_depth("bulk")), (2, 3));
    let mock = ctx.mock_endpoint("mock:bulk").unwrap();
    mock.expected_bodies_received(vec!["m0", "m1", "m2", "m3", "m4"]);
    {
        let (m, c) = &*gate;
        *m.lock().unwrap() = true;
        c.notify_all();
    }
    mock.assert_is_satisfied().unwrap();
    wait_until("all acked", || broker.unacked_count("bulk") == 0 && broker.queue_depth("bulk") == 0);
    ctx.stop().unwrap();
}

#[test]
fn request_reply_in_out() {
    let broker = AmqpBroker::start().unwrap();
    let ctx = context(&broker);
    ctx.add_routes(|b| {
        b.from("spring-rabbitmq:rpc?queues=greeter&routingKey=greet&concurrentConsumers=2").transform(simple("Hello ${body}")).set_header("served-by", constant("greeter"));
        b.from("rabbitmq:legacy-rpc?queue=legacy-greeter&routingKey=greet&autoDelete=false").transform(simple("Hi ${body}"));
        b.from("spring-rabbitmq:rpc?queues=failing&routingKey=fail&maximumRetryAttempts=1").process(|_ex: &mut Exchange| Err(CamelError::new("OrderRejectedException", "no stock")));
        b.from("direct:start").to_with_pattern(ExchangePattern::InOut, "spring-rabbitmq:rpc?routingKey=greet").to("mock:replies");
    })
    .unwrap();
    ctx.start().unwrap();
    assert_eq!(broker.consumer_count("greeter"), 2);
    let t = ctx.create_producer_template();
    for i in 0..5 {
        let reply = t.request_body("spring-rabbitmq:rpc?routingKey=greet", format!("World{}", i)).unwrap();
        assert_eq!(reply.to_string(), format!("Hello World{}", i));
    }
    let ex = t.request("spring-rabbitmq:rpc?routingKey=greet", |ex| ex.set_body("Camel")).unwrap();
    assert_eq!(ex.header("served-by").unwrap().to_string(), "greeter");
    assert!(ex.header("CamelRabbitmqCorrelationId").is_some());
    assert_eq!(t.request_body("rabbitmq:legacy-rpc?routingKey=greet", "Bob").unwrap().to_string(), "Hi Bob");
    let mock = ctx.mock_endpoint("mock:replies").unwrap();
    mock.expected_bodies_received(vec!["Hello Route"]);
    t.send_body("direct:start", "Route").unwrap();
    mock.assert_is_satisfied().unwrap();
    // remote failure is propagated
    let err = t.request_body("spring-rabbitmq:rpc?routingKey=fail", "x").unwrap_err();
    let cause = err.cause.as_deref().unwrap_or(&err);
    assert_eq!(cause.kind, "OrderRejectedException", "{:?}", err);
    // no consumer => timeout
    let start = Instant::now();
    let err = t.request_body("spring-rabbitmq:rpc?routingKey=nobody&replyTimeout=200", "x").unwrap_err();
    let cause = err.cause.as_deref().unwrap_or(&err);
    assert_eq!(cause.kind, "ExchangeTimedOutException", "{:?}", err);
    assert!(start.elapsed() < Duration::from_secs(3));
    // disableReplyTo turns InOut into fire-and-forget
    let reply = t.request_body("spring-rabbitmq:rpc?routingKey=greet&disableReplyTo=true", "quiet").unwrap();
    assert_eq!(reply.to_string(), "quiet");
    ctx.stop().unwrap();
    wait_until("reply queues removed", || broker.queue_names().iter().all(|q| !q.starts_with("amq.gen-")));
}

#[test]
fn publisher_confirms_and_guaranteed_delivery() {
    let broker = AmqpBroker::start().unwrap();
    let ctx = context(&broker);
    broker.declare_queue("confirmed", Default::default());
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("spring-rabbitmq:default?routingKey=confirmed&confirm=enabled", "c1").unwrap();
    t.send_body("rabbitmq:?routingKey=confirmed&publisherAcknowledgements=true&declare=false", "c2").unwrap();
    assert_eq!(broker.queue_depth("confirmed"), 2);
    let msgs = broker.queue_messages("confirmed");
    assert_eq!(msgs[0].body_string(), "c1");
    assert_eq!(msgs[0].properties.content_type.as_deref(), Some("text/plain"));
    let ex = t.send("rabbitmq:?routingKey=nowhere&guaranteedDeliveries=true&declare=false", Exchange::with_body(Some(ctx.clone()), "lost"));
    let e = ex.exception.expect("returned message must fail");
    assert!(e.message.contains("312"), "{}", e.message);
    // wrong credentials fail the exchange
    let ex = t.send("rabbitmq:?routingKey=confirmed&username=bad&password=bad", Exchange::with_body(Some(ctx.clone()), "x"));
    assert_eq!(ex.exception.unwrap().kind, "AuthenticationFailureException");
    // unknown options are rejected
    assert!(ctx.get_endpoint("spring-rabbitmq:x?bogus=1").is_err());
    assert!(ctx.get_endpoint("spring-rabbitmq:x?exchangeType=weird").is_err());
    ctx.stop().unwrap();
}

#[test]
fn consumer_reconnects_after_connection_loss() {
    let broker = AmqpBroker::start().unwrap();
    let ctx = context(&broker);
    ctx.add_routes(|b| {
        b.from("spring-rabbitmq:resilient?queues=resilient&routingKey=r&recoveryInterval=50").to("mock:r");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:r").unwrap();
    mock.expected_bodies_received(vec!["before", "after"]);
    let t = ctx.create_producer_template();
    t.send_body("spring-rabbitmq:resilient?routingKey=r", "before").unwrap();
    mock.wait_for_messages(1, Duration::from_secs(5));
    wait_until("first message acked", || broker.unacked_count("resilient") == 0 && broker.queue_depth("resilient") == 0);
    broker.drop_connections();
    std::thread::sleep(Duration::from_millis(100));
    wait_until("consumer reconnected", || broker.consumer_count("resilient") == 1 && broker.connection_count() >= 1);
    // the producer reconnects lazily on the next send (the first attempt may hit the dead connection)
    if t.send_body("spring-rabbitmq:resilient?routingKey=r", "after").is_err() {
        t.send_body("spring-rabbitmq:resilient?routingKey=r", "after").unwrap();
    }
    mock.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
}

/// Regression (found against a real RabbitMQ): an endpoint's `addresses` must win over the
/// component-wide host/port instead of being overridden by them.
#[test]
fn endpoint_addresses_take_precedence_over_component_host_and_port() {
    let broker = AmqpBroker::start().unwrap();
    let ctx = CamelContext::new();
    cameleer_amqp::register(&ctx);
    // component defaults point at a port nobody listens on
    let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    ctx.properties().set("camel.component.spring-rabbitmq.host", "localhost");
    ctx.properties().set("camel.component.spring-rabbitmq.port", dead.to_string());
    ctx.properties().set("camel.component.rabbitmq.hostname", "localhost");
    ctx.properties().set("camel.component.rabbitmq.portNumber", dead.to_string());
    broker.declare_queue("addr", Default::default());
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body(&format!("spring-rabbitmq:default?routingKey=addr&addresses=amqp://127.0.0.1:{}", broker.port()), "spring").unwrap();
    t.send_body(&format!("rabbitmq:?routingKey=addr&declare=false&addresses=127.0.0.1:{}", broker.port()), "legacy").unwrap();
    wait_until("both messages queued", || broker.queue_depth("addr") == 2);
    ctx.stop().unwrap();
}

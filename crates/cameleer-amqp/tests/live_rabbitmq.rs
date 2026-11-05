//! Live interop: `spring-rabbitmq` / `rabbitmq` (AMQP 0-9-1) against a real RabbitMQ broker
//! started by `scripts/live-interop.sh`. Runs only with `CAMEL_LIVE_INTEROP=1`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use cameleer_amqp::amqp::client::{AmqpConfig, Connection, QueueOptions};
use cameleer_core::prelude::*;
use cameleer_tls::testing::live;

const PORT: u16 = 15673;
const TLS_PORT: u16 = 15674;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_amqp::register(&ctx);
    live::bind_beans(&ctx);
    let p = ctx.properties();
    p.set("camel.component.spring-rabbitmq.host", "127.0.0.1");
    p.set("camel.component.spring-rabbitmq.port", PORT.to_string());
    p.set("camel.component.spring-rabbitmq.username", "camel");
    p.set("camel.component.spring-rabbitmq.password", live::PASSWORD);
    p.set("camel.component.rabbitmq.hostname", "127.0.0.1");
    p.set("camel.component.rabbitmq.portNumber", PORT.to_string());
    p.set("camel.component.rabbitmq.username", "camel");
    p.set("camel.component.rabbitmq.password", live::PASSWORD);
    ctx
}

/// A broker object name unique to this test process.
fn n(name: &str) -> String {
    format!("live-{}-{}", std::process::id(), name)
}

fn admin() -> Connection {
    Connection::open(&AmqpConfig { host: "127.0.0.1".into(), port: PORT, username: "camel".into(), password: live::PASSWORD.into(), heartbeat: 0, ..Default::default() }).unwrap()
}

/// (ready, consumers) of a queue, via a passive declare.
fn queue_state(conn: &Connection, queue: &str) -> (u32, u32) {
    let ch = conn.channel().unwrap();
    let info = ch.queue_declare(queue, &QueueOptions { passive: true, ..Default::default() }).unwrap_or_else(|e| panic!("{}: {}", queue, e));
    let _ = ch.close();
    (info.message_count, info.consumer_count)
}

fn cleanup(queues: &[&str], exchanges: &[&str]) {
    let conn = admin();
    let ch = conn.channel().unwrap();
    for q in queues {
        let _ = ch.queue_delete(q, false, false);
    }
    for e in exchanges {
        let _ = ch.exchange_delete(e, false);
    }
    conn.close();
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out waiting for {}", what);
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn live_rabbitmq_direct_routing_headers_and_properties() {
    if !live::enabled() {
        return;
    }
    let (ex, qeu, qus) = (n("orders"), n("eu-orders"), n("us-orders"));
    let ctx = context();
    let (e1, e2, q1, q2) = (ex.clone(), ex.clone(), qeu.clone(), qus.clone());
    ctx.add_routes(move |b| {
        b.from(&format!("spring-rabbitmq:{}?queues={}&routingKey=eu", e1, q1)).to("mock:eu");
        b.from(&format!("spring-rabbitmq:{}?queues={}&routingKey=us", e2, q2)).to("mock:us");
    })
    .unwrap();
    ctx.start().unwrap();
    let eu = ctx.mock_endpoint("mock:eu").unwrap();
    let us = ctx.mock_endpoint("mock:us").unwrap();
    eu.expected_bodies_received(vec!["order-1", "order-3"]);
    us.expected_bodies_received(vec!["order-2"]);
    let t = ctx.create_producer_template();
    t.send_body_and_headers(
        &format!("spring-rabbitmq:{}?routingKey=eu", ex),
        "order-1",
        vec![
            ("customer", Value::from("ACME")),
            ("quantity", Value::Int(42)),
            ("price", Value::Float(9.75)),
            ("flag", Value::Bool(true)),
            ("CamelRabbitmqCorrelationId", Value::from("corr-1")),
            ("CamelRabbitmqPriority", Value::Int(6)),
            ("CamelRabbitmqMessageId", Value::from("m-1")),
            ("CamelRabbitmqExpiration", Value::from("60000")),
            ("CamelInternal", Value::from("not sent")),
        ],
    )
    .unwrap();
    t.send_body(&format!("spring-rabbitmq:{}?routingKey=us", ex), "order-2").unwrap();
    t.send_body_and_header(&format!("spring-rabbitmq:{}?routingKey=us", ex), "order-3", "CamelSpringRabbitmqRoutingOverrideKey", "eu").unwrap();
    MockEndpoint::assert_all_satisfied(&[&eu, &us]).unwrap();
    let m = eu.exchange(0).unwrap();
    let s = |h: &str| m.header(h).map(|v| v.to_string()).unwrap_or_default();
    assert_eq!(s("CamelRabbitmqRoutingKey"), "eu");
    assert_eq!(s("CamelRabbitmqExchangeName"), ex);
    assert_eq!(s("customer"), "ACME");
    assert_eq!(m.header("quantity").and_then(|v| v.as_i64()), Some(42));
    assert_eq!(m.header("price").and_then(|v| v.as_f64()), Some(9.75));
    assert_eq!(m.header("flag").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(s("CamelRabbitmqCorrelationId"), "corr-1");
    assert_eq!(s("CamelRabbitmqMessageId"), "m-1");
    assert_eq!(s("CamelRabbitmqExpiration"), "60000");
    assert_eq!(m.header("CamelRabbitmqPriority").and_then(|v| v.as_i64()), Some(6));
    assert_eq!(s("CamelRabbitmqContentType"), "text/plain");
    assert_eq!(m.header("CamelRabbitmqDeliveryMode").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(m.header("CamelRabbitmqRedeliveryTag").and_then(|v| v.as_bool()), Some(false));
    assert!(m.header("CamelInternal").is_none());
    assert!(matches!(m.body(), Value::String(_)));
    // JSON bodies
    eu.reset();
    eu.expected_message_count(1);
    t.send_body(&format!("spring-rabbitmq:{}?routingKey=eu", ex), Value::object([("id", Value::Int(7)), ("items", Value::Array(vec![Value::from("a")]))])).unwrap();
    eu.assert_is_satisfied().unwrap();
    assert_eq!(eu.exchange(0).unwrap().header("CamelRabbitmqContentType").map(|v| v.to_string()).as_deref(), Some("application/json"));
    ctx.stop().unwrap();
    let conn = admin();
    wait_until("consumers gone", || queue_state(&conn, &qeu).1 == 0);
    cleanup(&[&qeu, &qus], &[&ex]);
}

#[test]
fn live_rabbitmq_topic_and_fanout_exchanges() {
    if !live::enabled() {
        return;
    }
    let (events, news, legacy) = (n("events"), n("news"), n("legacy-fanout"));
    let qs: Vec<String> = ["all-events", "eu-events", "news-a", "news-b", "lf1", "lf2"].iter().map(|q| n(q)).collect();
    let ctx = context();
    let (ev, nw, lg, q) = (events.clone(), news.clone(), legacy.clone(), qs.clone());
    ctx.add_routes(move |b| {
        b.from(&format!("spring-rabbitmq:{}?exchangeType=topic&queues={}&routingKey=events.#", ev, q[0])).to("mock:all");
        b.from(&format!("spring-rabbitmq:{}?exchangeType=topic&queues={}&routingKey=events.eu.*", ev, q[1])).to("mock:eu");
        b.from(&format!("spring-rabbitmq:{}?exchangeType=fanout&queues={}", nw, q[2])).to("mock:a");
        b.from(&format!("spring-rabbitmq:{}?exchangeType=fanout&queues={}", nw, q[3])).to("mock:b");
        b.from(&format!("rabbitmq:{}?exchangeType=fanout&queue={}&autoDelete=false", lg, q[4])).to("mock:lf1");
        b.from(&format!("rabbitmq:{}?exchangeType=fanout&queue={}&autoDelete=false", lg, q[5])).to("mock:lf2");
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
    t.send_body(&format!("spring-rabbitmq:{}?exchangeType=topic&routingKey=events.eu.de", events), "de").unwrap();
    t.send_body(&format!("spring-rabbitmq:{}?exchangeType=topic&routingKey=events.us.ny", events), "us").unwrap();
    t.send_body(&format!("spring-rabbitmq:{}?exchangeType=topic&routingKey=events.eu.de.berlin", events), "deep").unwrap();
    t.send_body(&format!("spring-rabbitmq:{}?exchangeType=fanout", news), "breaking").unwrap();
    t.send_body(&format!("rabbitmq:{}?exchangeType=fanout&autoDelete=false", legacy), "legacy").unwrap();
    MockEndpoint::assert_all_satisfied(&[&all, &eu, &a, &b, &lf1, &lf2]).unwrap();
    let ex = lf1.exchange(0).unwrap();
    assert!(matches!(ex.body(), Value::Bytes(_)));
    assert_eq!(ex.header("CamelRabbitmqExchangeName").unwrap().to_string(), legacy);
    ctx.stop().unwrap();
    let qr: Vec<&str> = qs.iter().map(|s| s.as_str()).collect();
    cleanup(&qr, &[&events, &news, &legacy]);
}

#[test]
fn live_rabbitmq_manual_ack_reject_and_dead_letters() {
    if !live::enabled() {
        return;
    }
    let (work, tasks) = (n("work"), n("tasks"));
    let (pay, payq, dlx, dlq) = (n("payments"), n("payments-q"), n("payments.dlx"), n("payments.dlq"));
    let ctx = context();
    let attempts = Arc::new(AtomicUsize::new(0));
    let pay_attempts = Arc::new(AtomicUsize::new(0));
    let (a2, p2) = (attempts.clone(), pay_attempts.clone());
    let (w, tq, p, pq, x, xq) = (work.clone(), tasks.clone(), pay.clone(), payq.clone(), dlx.clone(), dlq.clone());
    ctx.add_routes(move |b| {
        b.from(&format!("rabbitmq:{}?queue={}&routingKey=task&autoAck=false&autoDelete=false", w, tq))
            .process(move |ex: &mut Exchange| {
                if a2.fetch_add(1, Ordering::SeqCst) == 0 {
                    ex.set_header("CamelRabbitmqRequeue", true);
                    return Err(CamelError::illegal_state("first attempt fails"));
                }
                Ok(())
            })
            .to("mock:done");
        b.from(&format!(
            "spring-rabbitmq:{}?queues={}&routingKey=pay&deadLetterExchange={}&deadLetterQueue={}&deadLetterRoutingKey=dead&maximumRetryAttempts=3&retryDelay=5",
            p, pq, x, xq
        ))
        .process(move |_ex: &mut Exchange| {
            p2.fetch_add(1, Ordering::SeqCst);
            Err(CamelError::illegal_state("cannot pay"))
        });
        b.from(&format!("spring-rabbitmq:{}?queues={}&routingKey=dead", x, xq)).to("mock:dead");
    })
    .unwrap();
    ctx.start().unwrap();
    // reject with requeue: the broker redelivers with the redelivered flag
    let done = ctx.mock_endpoint("mock:done").unwrap();
    done.expected_bodies_received(vec!["job"]);
    done.expected_header_received("CamelRabbitmqRedeliveryTag", true);
    let t = ctx.create_producer_template();
    t.send_body(&format!("rabbitmq:{}?routingKey=task&autoDelete=false", work), "job").unwrap();
    done.assert_is_satisfied().unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    let conn = admin();
    wait_until("acked", || queue_state(&conn, &tasks).0 == 0);

    // retries exhausted: rejected without requeue, dead-lettered by RabbitMQ with x-death
    let dead = ctx.mock_endpoint("mock:dead").unwrap();
    dead.expected_bodies_received(vec!["$100"]);
    t.send_body(&format!("spring-rabbitmq:{}?routingKey=pay", pay), "$100").unwrap();
    dead.assert_is_satisfied().unwrap();
    assert_eq!(pay_attempts.load(Ordering::SeqCst), 3);
    let ex = dead.exchange(0).unwrap();
    let death = ex.header("x-death").and_then(|v| v.as_array().cloned()).unwrap_or_default();
    let first = death.first().cloned().unwrap_or(Value::Null);
    assert_eq!(first.get("reason").map(|v| v.to_string()).as_deref(), Some("rejected"), "{:?}", ex.message.headers);
    assert_eq!(first.get("queue").map(|v| v.to_string()), Some(payq.clone()));
    assert_eq!(first.get("count").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(ex.header("x-first-death-queue").map(|v| v.to_string()), Some(payq.clone()));
    assert_eq!(ex.header("x-first-death-reason").map(|v| v.to_string()).as_deref(), Some("rejected"));
    assert_eq!(queue_state(&conn, &payq).0, 0);
    ctx.stop().unwrap();
    cleanup(&[&tasks, &payq, &dlq], &[&work, &pay, &dlx]);
}

#[test]
fn live_rabbitmq_prefetch_limits_unacked_messages() {
    if !live::enabled() {
        return;
    }
    let (bulk, bulkq) = (n("bulk"), n("bulk-q"));
    let ctx = context();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let g2 = gate.clone();
    let (b1, q1) = (bulk.clone(), bulkq.clone());
    ctx.add_routes(move |b| {
        b.from(&format!("spring-rabbitmq:{}?queues={}&routingKey=b&prefetchCount=2", b1, q1))
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
        t.send_body(&format!("spring-rabbitmq:{}?routingKey=b", bulk), format!("m{}", i)).unwrap();
    }
    let conn = admin();
    // two messages are in flight (unacked), three stay ready in the queue
    wait_until("prefetch window filled", || queue_state(&conn, &bulkq).0 == 3);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(queue_state(&conn, &bulkq).0, 3);
    let mock = ctx.mock_endpoint("mock:bulk").unwrap();
    mock.expected_bodies_received(vec!["m0", "m1", "m2", "m3", "m4"]);
    {
        let (m, c) = &*gate;
        *m.lock().unwrap() = true;
        c.notify_all();
    }
    mock.assert_is_satisfied().unwrap();
    wait_until("all consumed", || queue_state(&conn, &bulkq).0 == 0);
    ctx.stop().unwrap();
    cleanup(&[&bulkq], &[&bulk]);
}

#[test]
fn live_rabbitmq_request_reply() {
    if !live::enabled() {
        return;
    }
    let (rpc, greeter, failing, legacy, legacyq) = (n("rpc"), n("greeter"), n("failing"), n("legacy-rpc"), n("legacy-greeter"));
    let ctx = context();
    let (r1, r2, r3, g, f, l, lq) = (rpc.clone(), rpc.clone(), rpc.clone(), greeter.clone(), failing.clone(), legacy.clone(), legacyq.clone());
    ctx.add_routes(move |b| {
        b.from(&format!("spring-rabbitmq:{}?queues={}&routingKey=greet&concurrentConsumers=2", r1, g)).transform(simple("Hello ${body}")).set_header("served-by", constant("greeter"));
        b.from(&format!("rabbitmq:{}?queue={}&routingKey=greet&autoDelete=false", l, lq)).transform(simple("Hi ${body}"));
        b.from(&format!("spring-rabbitmq:{}?queues={}&routingKey=fail&maximumRetryAttempts=1", r2, f)).process(|_ex: &mut Exchange| Err(CamelError::new("OrderRejectedException", "no stock")));
        b.from("direct:start").to_with_pattern(ExchangePattern::InOut, &format!("spring-rabbitmq:{}?routingKey=greet", r3)).to("mock:replies");
    })
    .unwrap();
    ctx.start().unwrap();
    let conn = admin();
    assert_eq!(queue_state(&conn, &greeter).1, 2, "two competing consumers");
    let t = ctx.create_producer_template();
    for i in 0..5 {
        let reply = t.request_body(&format!("spring-rabbitmq:{}?routingKey=greet", rpc), format!("World{}", i)).unwrap();
        assert_eq!(reply.to_string(), format!("Hello World{}", i));
    }
    let ex = t.request(&format!("spring-rabbitmq:{}?routingKey=greet", rpc), |ex| ex.set_body("Camel")).unwrap();
    assert_eq!(ex.header("served-by").unwrap().to_string(), "greeter");
    assert!(ex.header("CamelRabbitmqCorrelationId").is_some());
    assert_eq!(t.request_body(&format!("rabbitmq:{}?routingKey=greet&autoDelete=false", legacy), "Bob").unwrap().to_string(), "Hi Bob");
    let mock = ctx.mock_endpoint("mock:replies").unwrap();
    mock.expected_bodies_received(vec!["Hello Route"]);
    t.send_body("direct:start", "Route").unwrap();
    mock.assert_is_satisfied().unwrap();
    let err = t.request_body(&format!("spring-rabbitmq:{}?routingKey=fail", rpc), "x").unwrap_err();
    let cause = err.cause.as_deref().unwrap_or(&err);
    assert_eq!(cause.kind, "OrderRejectedException", "{:?}", err);
    let start = Instant::now();
    let err = t.request_body(&format!("spring-rabbitmq:{}?routingKey=nobody&replyTimeout=300", rpc), "x").unwrap_err();
    let cause = err.cause.as_deref().unwrap_or(&err);
    assert_eq!(cause.kind, "ExchangeTimedOutException", "{:?}", err);
    assert!(start.elapsed() < Duration::from_secs(5));
    ctx.stop().unwrap();
    cleanup(&[&greeter, &failing, &legacyq], &[&rpc, &legacy]);
}

#[test]
fn live_rabbitmq_publisher_confirms_returns_and_auth() {
    if !live::enabled() {
        return;
    }
    let confirmed = n("confirmed");
    let conn = admin();
    let ch = conn.channel().unwrap();
    ch.queue_declare(&confirmed, &QueueOptions::default()).unwrap();
    let ctx = context();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body(&format!("spring-rabbitmq:default?routingKey={}&confirm=enabled", confirmed), "c1").unwrap();
    t.send_body(&format!("rabbitmq:?routingKey={}&publisherAcknowledgements=true&declare=false", confirmed), "c2").unwrap();
    wait_until("both confirmed messages queued", || queue_state(&conn, &confirmed).0 == 2);
    let d = ch.basic_get(&confirmed, true).unwrap().unwrap();
    assert_eq!(d.body, b"c1".to_vec());
    assert_eq!(d.properties.content_type.as_deref(), Some("text/plain"));
    // an unroutable mandatory message comes back with basic.return 312 NO_ROUTE
    let ex = t.send(&format!("rabbitmq:?routingKey={}&guaranteedDeliveries=true&declare=false", n("nowhere")), Exchange::with_body(Some(ctx.clone()), "lost"));
    let e = ex.exception.expect("returned message must fail");
    assert!(e.message.contains("312"), "{}", e.message);
    // wrong credentials
    let ex = t.send(&format!("rabbitmq:?routingKey={}&username=bad&password=bad", confirmed), Exchange::with_body(Some(ctx.clone()), "x"));
    assert_eq!(ex.exception.unwrap().kind, "AuthenticationFailureException");
    // re-declaring an existing queue with different arguments: PRECONDITION_FAILED from the broker
    let ex = t.send(&format!("rabbitmq:?queue={}&routingKey={}&durable=false&autoDelete=false&arg.queue.x-max-length=1", confirmed, confirmed), Exchange::with_body(Some(ctx.clone()), "x"));
    if let Some(e) = ex.exception {
        assert!(e.message.contains("406") || e.message.contains("PRECONDITION"), "{}", e.message);
    }
    ctx.stop().unwrap();
    cleanup(&[&confirmed], &[]);
}

#[test]
fn live_rabbitmq_amqps() {
    if !live::enabled() {
        return;
    }
    let (ex, q) = (n("secure"), n("secure-q"));
    let ctx = context();
    let opts = format!("addresses=amqps://localhost:{}&sslContextParameters=#liveSsl", TLS_PORT);
    let (e, qq, o) = (ex.clone(), q.clone(), opts.clone());
    ctx.add_routes(move |b| {
        b.from(&format!("spring-rabbitmq:{}?queues={}&routingKey=s&{}", e, qq, o)).to("mock:secure");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:secure").unwrap();
    mock.expected_bodies_received(vec!["over tls", "legacy tls"]);
    let t = ctx.create_producer_template();
    t.send_body(&format!("spring-rabbitmq:{}?routingKey=s&{}", ex, opts), "over tls").unwrap();
    t.send_body(&format!("rabbitmq:{}?routingKey=s&hostname=localhost&portNumber={}&sslProtocol=TLSv1.2&sslContextParameters=#liveSsl&declare=false", ex, TLS_PORT), "legacy tls").unwrap();
    mock.assert_is_satisfied().unwrap();
    let ex2 = t.send(&format!("spring-rabbitmq:{}?routingKey=s&addresses=amqps://localhost:{}&sslContextParameters=#liveUntrusted", ex, TLS_PORT), Exchange::with_body(Some(ctx.clone()), "x"));
    assert!(ex2.exception.is_some(), "untrusted broker certificate must fail");
    ctx.stop().unwrap();
    cleanup(&[&q], &[&ex]);
}

//! Live interop: `paho` / `paho-mqtt5` against a real Eclipse Mosquitto broker (and the RabbitMQ
//! MQTT plugin) started by `scripts/live-interop.sh`. Runs only with `CAMEL_LIVE_INTEROP=1`.

use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use cameleer_core::prelude::*;
use cameleer_kafka_mqtt::mqtt::packet::Will;
use cameleer_kafka_mqtt::mqtt::{MqttClient, MqttMessage, MqttOptions, MqttVersion};
use cameleer_tls::testing::live;

const WAIT: Duration = Duration::from_secs(10);
const ANON: &str = "tcp://127.0.0.1:11883";
const AUTH: &str = "tcp://127.0.0.1:11884";
const TLS: &str = "ssl://localhost:18883";
const RABBIT: &str = "tcp://127.0.0.1:11885";

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_kafka_mqtt::register(&ctx);
    live::bind_beans(&ctx);
    ctx
}

/// A topic prefix unique to this test process and test.
fn ns(test: &str) -> String {
    format!("live/{}/{}", std::process::id(), test)
}

fn options(port: u16, id: &str, version: MqttVersion) -> MqttOptions {
    let mut o = MqttOptions::new("127.0.0.1", port, id);
    o.version = version;
    o.connect_timeout = Duration::from_secs(5);
    o.ack_timeout = Duration::from_secs(5);
    o
}

fn collector() -> (cameleer_kafka_mqtt::mqtt::MessageHandler, mpsc::Receiver<MqttMessage>) {
    let (tx, rx) = mpsc::channel::<MqttMessage>();
    let tx = std::sync::Mutex::new(tx);
    (
        Arc::new(move |m| {
            let _ = tx.lock().unwrap().send(m);
        }),
        rx,
    )
}

#[test]
fn live_mosquitto_qos_0_1_2() {
    if !live::enabled() {
        return;
    }
    for scheme in ["paho", "paho-mqtt5"] {
        let base = ns(&format!("qos-{}", scheme));
        let ctx = context();
        let b = base.clone();
        ctx.add_routes(move |r| {
            for q in 0..3 {
                r.from(&format!("{}:{}/q{}?brokerUrl={}&qos={}", scheme, b, q, ANON, q)).to(&format!("mock:q{}", q));
            }
        })
        .unwrap();
        ctx.start().unwrap();
        let t = ctx.create_producer_template();
        for q in 0..3 {
            let mock = ctx.mock_endpoint(&format!("mock:q{}", q)).unwrap();
            mock.expected_bodies_received(vec!["one", "two"]);
            mock.expected_header_received("CamelMqttTopic", format!("{}/q{}", base, q));
            mock.expected_header_received("CamelMqttQoS", q as i64);
            mock.expected_header_received("CamelMqttRetained", false);
            for body in ["one", "two"] {
                t.send_body(&format!("{}:{}/q{}?brokerUrl={}&qos={}", scheme, base, q, ANON, q), body).unwrap_or_else(|e| panic!("{} qos {}: {}", scheme, q, e));
            }
            mock.assert_is_satisfied_within(WAIT).unwrap_or_else(|e| panic!("{} qos {}: {}", scheme, q, e));
        }
        // the subscription QoS caps the delivery QoS (broker side downgrade)
        let mock = ctx.mock_endpoint("mock:q1").unwrap();
        mock.reset();
        mock.expected_header_received("CamelMqttQoS", 1i64);
        mock.expected_message_count(1);
        t.send_body(&format!("{}:{}/q1?brokerUrl={}&qos=2", scheme, base, ANON), "capped").unwrap();
        mock.assert_is_satisfied_within(WAIT).unwrap();
        // a 256 KiB payload
        let mock = ctx.mock_endpoint("mock:q2").unwrap();
        mock.reset();
        mock.expected_message_count(1);
        let big = vec![b'm'; 256 * 1024];
        t.send_body(&format!("{}:{}/q2?brokerUrl={}&qos=2", scheme, base, ANON), Value::Bytes(big.clone())).unwrap();
        mock.assert_is_satisfied_within(WAIT).unwrap();
        assert_eq!(mock.received_exchanges()[0].body(), &Value::Bytes(big));
        ctx.stop().unwrap();
    }
}

#[test]
fn live_mosquitto_wildcards_retained_and_credentials() {
    if !live::enabled() {
        return;
    }
    let base = ns("wild");
    let ctx = context();
    let b = base.clone();
    ctx.add_routes(move |r| {
        r.from(&format!("paho:{}/sensors/+/temperature?brokerUrl={}&qos=1", b, ANON)).to("mock:plus");
        r.from(&format!("paho-mqtt5:{}/sensors/#?brokerUrl={}&qos=1", b, ANON)).to("mock:hash");
    })
    .unwrap();
    ctx.start().unwrap();
    let plus = ctx.mock_endpoint("mock:plus").unwrap();
    plus.expected_bodies_received(vec!["21", "23"]);
    let hash = ctx.mock_endpoint("mock:hash").unwrap();
    hash.expected_bodies_received(vec!["21", "40", "23", "on"]);
    let t = ctx.create_producer_template();
    let producer = format!("paho:unused?brokerUrl={}&qos=1", ANON);
    for (topic, body) in [("sensors/kitchen/temperature", "21"), ("sensors/kitchen/humidity", "40"), ("sensors/bath/temperature", "23"), ("sensors", "on"), ("other/x", "no")] {
        t.send_body_and_header(&producer, body, "CamelPahoOverrideTopic", format!("{}/{}", base, topic)).unwrap();
    }
    MockEndpoint::assert_all_satisfied(&[&plus, &hash]).unwrap();
    assert_eq!(plus.received_exchanges()[1].header("CamelMqttTopic").map(|v| v.to_string()).unwrap_or_default(), format!("{}/sensors/bath/temperature", base));
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(hash.received_counter(), 4, "other/x must not match");

    // retained messages reach late subscribers; an empty retained payload clears them
    t.send_body(&format!("paho:{}/status/device1?brokerUrl={}&qos=1&retained=true", base, ANON), "online").unwrap();
    t.send_body(&format!("paho:{}/status/device2?brokerUrl={}&qos=1", base, ANON), "not retained").unwrap();
    let b = base.clone();
    ctx.add_routes(move |r| {
        r.from(&format!("paho-mqtt5:{}/status/+?brokerUrl={}&qos=1", b, ANON)).to("mock:late");
    })
    .unwrap();
    let late = ctx.mock_endpoint("mock:late").unwrap();
    late.expected_bodies_received(vec!["online"]);
    late.expected_header_received("CamelMqttRetained", true);
    late.assert_is_satisfied_within(WAIT).unwrap();
    late.reset();
    late.expected_bodies_received(vec!["busy"]);
    late.expected_header_received("CamelMqttRetained", false);
    t.send_body_and_header(&format!("paho:{}/status/device1?brokerUrl={}&qos=1", base, ANON), "busy", "CamelPahoRetained", true).unwrap();
    late.assert_is_satisfied_within(WAIT).unwrap();
    t.send_body(&format!("paho:{}/status/device1?brokerUrl={}&qos=1&retained=true", base, ANON), "").unwrap();
    // verify with a raw client: nothing retained any more
    let (h, rx) = collector();
    let c = MqttClient::connect(options(11883, &format!("chk-{}", std::process::id()), MqttVersion::V311), Some(h)).unwrap();
    c.subscribe(&[(format!("{}/status/device1", base), 1)]).unwrap();
    assert!(rx.recv_timeout(Duration::from_millis(500)).is_err(), "retained message was cleared");
    c.disconnect();
    ctx.stop().unwrap();

    // username/password listener, QoS 2, both protocol versions
    for scheme in ["paho", "paho-mqtt5"] {
        let ctx = context();
        let creds = format!("userName=camel&password={}", live::PASSWORD);
        let (b, cr) = (base.clone(), creds.clone());
        ctx.add_routes(move |r| {
            r.from("direct:start").to(&format!("{}:{}/chat/room1?brokerUrl={}&qos=2&{}", scheme, b, AUTH, cr));
            r.from(&format!("{}:{}/chat/#?brokerUrl={}&qos=2&clientId=listener-{}-{}&{}", scheme, b, AUTH, scheme, std::process::id(), cr)).to("mock:chat");
        })
        .unwrap();
        ctx.start().unwrap();
        let mock = ctx.mock_endpoint("mock:chat").unwrap();
        mock.expected_bodies_received(vec!["hello", "world", "!"]);
        mock.expected_header_received("CamelMqttQoS", 2i64);
        let t = ctx.create_producer_template();
        for b in ["hello", "world", "!"] {
            t.send_body("direct:start", b).unwrap();
        }
        mock.assert_is_satisfied_within(WAIT).unwrap();
        let ex = t.send(&format!("{}:{}/chat/room1?brokerUrl={}&userName=camel&password=wrong&automaticReconnect=false", scheme, base, AUTH), Exchange::with_body(Some(ctx.clone()), "x"));
        let err = ex.exception.expect("bad credentials must fail");
        let m = err.message.to_ascii_lowercase();
        assert!(m.contains("bad user name or password") || m.contains("not authorized"), "{}: {}", scheme, err.message);
        let ex = t.send(&format!("{}:{}/chat/room1?brokerUrl={}&automaticReconnect=false", scheme, base, AUTH), Exchange::with_body(Some(ctx.clone()), "x"));
        assert!(ex.exception.is_some(), "anonymous connections are refused on the auth listener");
        ctx.stop().unwrap();
    }
}

#[test]
fn live_mosquitto_tls_listener() {
    if !live::enabled() {
        return;
    }
    let base = ns("tls");
    for scheme in ["paho", "paho-mqtt5"] {
        let ctx = context();
        let opts = format!("brokerUrl={}&qos=1&sslContextParameters=#liveSsl&userName=camel&password={}", TLS, live::PASSWORD);
        let (b, o) = (base.clone(), opts.clone());
        ctx.add_routes(move |r| {
            r.from(&format!("{}:{}/{}?{}", scheme, b, scheme, o)).to("mock:mqtt");
        })
        .unwrap();
        ctx.start().unwrap();
        let mock = ctx.mock_endpoint("mock:mqtt").unwrap();
        mock.expected_bodies_received(vec!["tls 1", "tls 2"]);
        let t = ctx.create_producer_template();
        for body in ["tls 1", "tls 2"] {
            t.send_body(&format!("{}:{}/{}?{}", scheme, base, scheme, opts), body).unwrap_or_else(|e| panic!("{}: {}", scheme, e));
        }
        mock.assert_is_satisfied_within(WAIT).unwrap();
        let err = t
            .send_body(&format!("{}:{}/x?brokerUrl={}&qos=1&sslContextParameters=#liveUntrusted&automaticReconnect=false&connectionTimeout=5", scheme, base, TLS), "x")
            .unwrap_err();
        assert!(format!("{:?}", err).to_ascii_lowercase().contains("certificate") || err.root_cause().is_a("IOException"), "{:?}", err);
        ctx.stop().unwrap();
    }
}

#[test]
fn live_mosquitto_client_will_persistent_session() {
    if !live::enabled() {
        return;
    }
    for version in [MqttVersion::V311, MqttVersion::V5] {
        let base = ns(&format!("session-{:?}", version));
        let id = |s: &str| format!("{}-{}-{:?}", s, std::process::id(), version);
        let (handler, rx) = collector();
        let watcher = MqttClient::connect(options(11883, &id("w"), version), Some(handler)).unwrap();
        assert_eq!(watcher.subscribe(&[(format!("{}/wills/#", base), 1), (format!("{}/data", base), 2)]).unwrap(), vec![1, 2]);

        // the broker publishes the will when the connection dies without DISCONNECT
        let mut o = options(11883, &id("dying"), version);
        o.auto_reconnect = false;
        o.will = Some(Will { topic: format!("{}/wills/dying", base), payload: b"gone".to_vec(), qos: 1, retain: false, props: Default::default() });
        let dying = MqttClient::connect(o, None).unwrap();
        dying.kill_connection();
        let m = rx.recv_timeout(WAIT).unwrap();
        assert_eq!((m.topic.clone(), m.payload.as_slice(), m.qos), (format!("{}/wills/dying", base), &b"gone"[..], 1));
        let mut o = options(11883, &id("polite"), version);
        o.will = Some(Will { topic: format!("{}/wills/polite", base), payload: b"x".to_vec(), qos: 0, retain: false, props: Default::default() });
        MqttClient::connect(o, None).unwrap().disconnect();

        // persistent session: QoS 1 messages are queued by mosquitto while offline
        let mut o = options(11883, &id("durable"), version);
        o.clean_session = false;
        if version == MqttVersion::V5 {
            o.session_expiry_interval = Some(300);
        }
        let durable = MqttClient::connect(o.clone(), None).unwrap();
        durable.subscribe(&[(format!("{}/queue/x", base), 1)]).unwrap();
        durable.disconnect();
        let publisher = MqttClient::connect(options(11883, &id("pub"), version), None).unwrap();
        publisher.publish(&format!("{}/queue/x", base), b"while-offline", 1, false).unwrap();
        publisher.publish(&format!("{}/data", base), b"qos2", 2, false).unwrap();
        let m = rx.recv_timeout(WAIT).unwrap();
        assert_eq!((m.topic.clone(), m.qos), (format!("{}/data", base), 2));
        let (h2, rx2) = collector();
        let durable = MqttClient::connect(o.clone(), Some(h2)).unwrap();
        let m = rx2.recv_timeout(WAIT).unwrap();
        assert_eq!(m.payload, b"while-offline".to_vec());
        durable.disconnect();
        // clean up the persistent session
        let mut clean = o;
        clean.clean_session = true;
        clean.session_expiry_interval = None;
        MqttClient::connect(clean, None).unwrap().disconnect();

        watcher.unsubscribe(&[format!("{}/data", base)]).unwrap();
        publisher.publish(&format!("{}/data", base), b"ignored", 1, false).unwrap();
        publisher.publish(&format!("{}/wills/marker", base), b"marker", 1, false).unwrap();
        let m = rx.recv_timeout(WAIT).unwrap();
        assert_eq!(m.topic, format!("{}/wills/marker", base), "no message after unsubscribe and no will for a clean disconnect");
        publisher.disconnect();
        watcher.disconnect();
    }
}

#[test]
fn live_rabbitmq_mqtt_plugin() {
    if !live::enabled() {
        return;
    }
    let base = ns("rabbit");
    let ctx = context();
    let creds = format!("userName=camel&password={}", live::PASSWORD);
    let (b, c) = (base.clone(), creds.clone());
    ctx.add_routes(move |r| {
        r.from(&format!("paho:{}/+/in?brokerUrl={}&qos=1&{}", b, RABBIT, c)).to("mock:rabbit");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:rabbit").unwrap();
    mock.expected_bodies_received(vec!["r0", "r1"]);
    let t = ctx.create_producer_template();
    t.send_body(&format!("paho:{}/a/in?brokerUrl={}&qos=0&{}", base, RABBIT, creds), "r0").unwrap();
    t.send_body(&format!("paho:{}/b/in?brokerUrl={}&qos=1&{}", base, RABBIT, creds), "r1").unwrap();
    mock.assert_is_satisfied_within(WAIT).unwrap();
    ctx.stop().unwrap();
}

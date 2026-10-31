use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use camel_core::prelude::*;
use camel_kafka_mqtt::mqtt::packet::Will;
use camel_kafka_mqtt::mqtt::{MqttClient, MqttMessage, MqttOptions, MqttVersion};
use camel_kafka_mqtt::testing::{MqttBroker, MqttBrokerConfig};

const WAIT: Duration = Duration::from_secs(10);

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    camel_kafka_mqtt::register(&ctx);
    ctx
}

fn text(ex: &Exchange, h: &str) -> String {
    ex.header(h).map(|v| v.to_string()).unwrap_or_default()
}

fn options(broker: &MqttBroker, id: &str, version: MqttVersion) -> MqttOptions {
    let mut o = MqttOptions::new("127.0.0.1", broker.port(), id);
    o.version = version;
    o.connect_timeout = Duration::from_secs(5);
    o.ack_timeout = Duration::from_secs(5);
    o
}

#[test]
fn paho_qos_0_1_2_round_trips() {
    for scheme in ["paho", "paho-mqtt5"] {
        let broker = MqttBroker::start().unwrap();
        let url = broker.url();
        let ctx = context();
        ctx.add_routes(|r| {
            for q in 0..3 {
                r.from(&format!("{}:test/q{}?brokerUrl={}&qos={}", scheme, q, url, q)).to(&format!("mock:q{}", q));
            }
        })
        .unwrap();
        ctx.start().unwrap();
        let t = ctx.create_producer_template();
        for q in 0..3 {
            let mock = ctx.mock_endpoint(&format!("mock:q{}", q)).unwrap();
            mock.expected_bodies_received(vec!["one", "two"]);
            mock.expected_header_received("CamelMqttTopic", format!("test/q{}", q));
            mock.expected_header_received("CamelMqttQoS", q as i64);
            mock.expected_header_received("CamelMqttRetained", false);
            for body in ["one", "two"] {
                t.send_body(&format!("{}:test/q{}?brokerUrl={}&qos={}", scheme, q, url, q), body).unwrap();
            }
            mock.assert_is_satisfied_within(WAIT).unwrap();
            assert!(matches!(mock.received_exchanges()[0].body(), Value::Bytes(_)));
        }
        // the subscriber's QoS caps the delivery QoS
        let mock = ctx.mock_endpoint("mock:q1").unwrap();
        mock.reset();
        mock.expected_header_received("CamelMqttQoS", 1i64);
        mock.expected_message_count(1);
        t.send_body(&format!("{}:test/q1?brokerUrl={}&qos=2", scheme, url), "capped").unwrap();
        mock.assert_is_satisfied_within(WAIT).unwrap();
        ctx.stop().unwrap();
        assert!(broker.wait_until(WAIT, |b| b.connected_clients().is_empty()), "{:?}", broker.connected_clients());
    }
}

#[test]
fn wildcard_subscriptions_and_override_topic() {
    let broker = MqttBroker::start().unwrap();
    let url = broker.url();
    let ctx = context();
    ctx.add_routes(|r| {
        r.from(&format!("paho:sensors/+/temperature?brokerUrl={}&qos=1", url)).to("mock:plus");
        r.from(&format!("paho:sensors/#?brokerUrl={}&qos=1", url)).to("mock:hash");
    })
    .unwrap();
    ctx.start().unwrap();
    let plus = ctx.mock_endpoint("mock:plus").unwrap();
    plus.expected_bodies_received(vec!["21", "23"]);
    let hash = ctx.mock_endpoint("mock:hash").unwrap();
    hash.expected_bodies_received(vec!["21", "40", "23", "on"]);
    let t = ctx.create_producer_template();
    let producer = format!("paho:unused?brokerUrl={}&qos=1", url);
    for (topic, body) in [("sensors/kitchen/temperature", "21"), ("sensors/kitchen/humidity", "40"), ("sensors/bath/temperature", "23"), ("sensors", "on"), ("other/x", "no")] {
        t.send_body_and_header(&producer, body, "CamelPahoOverrideTopic", topic).unwrap();
    }
    MockEndpoint::assert_all_satisfied(&[&plus, &hash]).unwrap();
    assert_eq!(text(&plus.received_exchanges()[1], "CamelMqttTopic"), "sensors/bath/temperature");
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(hash.received_counter(), 4, "other/x must not match");
    ctx.stop().unwrap();
}

#[test]
fn retained_message_is_delivered_to_a_late_subscriber() {
    let broker = MqttBroker::start().unwrap();
    let url = broker.url();
    let ctx = context();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body(&format!("paho:status/device1?brokerUrl={}&qos=1&retained=true", url), "online").unwrap();
    t.send_body(&format!("paho:status/device2?brokerUrl={}&qos=1", url), "not retained").unwrap();
    assert_eq!(broker.retained("status/device1"), Some(b"online".to_vec()));
    assert_eq!(broker.retained("status/device2"), None);
    // a subscriber that starts afterwards still gets the retained message
    ctx.add_routes(|r| {
        r.from(&format!("paho-mqtt5:status/+?brokerUrl={}&qos=1", url)).to("mock:late");
    })
    .unwrap();
    let late = ctx.mock_endpoint("mock:late").unwrap();
    late.expected_bodies_received(vec!["online"]);
    late.expected_header_received("CamelMqttRetained", true);
    late.assert_is_satisfied_within(WAIT).unwrap();
    // live messages are not flagged retained; the retain header overrides; empty payload clears
    late.reset();
    late.expected_bodies_received(vec!["busy"]);
    late.expected_header_received("CamelMqttRetained", false);
    t.send_body_and_header(&format!("paho:status/device1?brokerUrl={}&qos=1", url), "busy", "CamelPahoRetained", true).unwrap();
    late.assert_is_satisfied_within(WAIT).unwrap();
    assert_eq!(broker.retained("status/device1"), Some(b"busy".to_vec()));
    t.send_body(&format!("paho:status/device1?brokerUrl={}&qos=1&retained=true", url), "").unwrap();
    assert!(broker.wait_until(WAIT, |b| b.retained("status/device1").is_none()));
    ctx.stop().unwrap();
}

#[test]
fn two_routes_publish_and_subscribe() {
    for scheme in ["paho", "paho-mqtt5"] {
        let broker = MqttBroker::start_with(MqttBrokerConfig { users: vec![("camel".into(), "secret".into())], ..Default::default() }).unwrap();
        let url = broker.url();
        let ctx = context();
        let creds = "userName=camel&password=secret";
        ctx.add_routes(|r| {
            r.from("direct:start").to(&format!("{}:chat/room1?brokerUrl={}&qos=2&{}", scheme, url, creds));
            r.from(&format!("{}:chat/#?brokerUrl={}&qos=2&clientId=listener-{}&{}", scheme, url, scheme, creds)).set_header("seen", constant("yes")).to("mock:chat");
        })
        .unwrap();
        ctx.start().unwrap();
        let mock = ctx.mock_endpoint("mock:chat").unwrap();
        mock.expected_bodies_received(vec!["hello", "world", "!"]);
        mock.expected_header_received("seen", "yes");
        mock.expected_header_received("CamelMqttQoS", 2i64);
        let t = ctx.create_producer_template();
        for b in ["hello", "world", "!"] {
            t.send_body("direct:start", b).unwrap();
        }
        mock.assert_is_satisfied_within(WAIT).unwrap();
        assert!(broker.connected_clients().contains(&format!("listener-{}", scheme)));
        // wrong credentials fail to connect
        let ex = t.send(&format!("{}:chat/room1?brokerUrl={}&userName=camel&password=wrong", scheme, url), Exchange::with_body(Some(ctx.clone()), "x"));
        let err = ex.exception.expect("bad credentials must fail");
        assert!(err.message.contains("bad user name or password"), "{}", err.message);
        ctx.stop().unwrap();
    }
}

#[test]
fn consumer_reconnects_and_resubscribes() {
    let broker = MqttBroker::start().unwrap();
    let url = broker.url();
    let ctx = context();
    ctx.add_routes(|r| {
        r.from(&format!("paho:alerts?brokerUrl={}&qos=1&clientId=watcher", url)).to("mock:alerts");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:alerts").unwrap();
    let t = ctx.create_producer_template();
    let producer = format!("paho:alerts?brokerUrl={}&qos=1", url);
    t.send_body(&producer, "before").unwrap();
    assert!(mock.wait_for_messages(1, WAIT));
    assert!(broker.disconnect_client("watcher"));
    // the client reconnects and subscribes again
    assert!(broker.wait_until(WAIT, |b| b.connect_count("watcher") == 2 && b.subscriptions("watcher") == vec!["alerts".to_string()]));
    t.send_body(&producer, "after").unwrap();
    mock.expected_bodies_received(vec!["before", "after"]);
    mock.assert_is_satisfied_within(WAIT).unwrap();
    ctx.stop().unwrap();
}

#[test]
fn client_will_persistent_session_and_unsubscribe() {
    for version in [MqttVersion::V311, MqttVersion::V5] {
        let broker = MqttBroker::start().unwrap();
        let (tx, rx) = mpsc::channel::<MqttMessage>();
        let tx = std::sync::Mutex::new(tx);
        let handler: camel_kafka_mqtt::mqtt::MessageHandler = Arc::new(move |m| {
            let _ = tx.lock().unwrap().send(m);
        });
        let watcher = MqttClient::connect(options(&broker, "w", version), Some(handler)).unwrap();
        assert_eq!(watcher.subscribe(&[("wills/#".to_string(), 1), ("data".to_string(), 2)]).unwrap(), vec![1, 2]);

        // will message published when a client dies
        let mut o = options(&broker, "dying", version);
        o.auto_reconnect = false;
        o.will = Some(Will { topic: "wills/dying".into(), payload: b"gone".to_vec(), qos: 1, retain: false, props: Default::default() });
        let dying = MqttClient::connect(o, None).unwrap();
        dying.kill_connection();
        let m = rx.recv_timeout(WAIT).unwrap();
        assert_eq!((m.topic.as_str(), m.payload.as_slice(), m.qos), ("wills/dying", &b"gone"[..], 1));
        // a clean DISCONNECT does not publish the will
        let mut o = options(&broker, "polite", version);
        o.will = Some(Will { topic: "wills/polite".into(), payload: b"x".to_vec(), qos: 0, retain: false, props: Default::default() });
        MqttClient::connect(o, None).unwrap().disconnect();

        // persistent session: messages are queued while offline
        let mut o = options(&broker, "durable", version);
        o.clean_session = false;
        if version == MqttVersion::V5 {
            o.session_expiry_interval = Some(300);
        }
        let durable = MqttClient::connect(o.clone(), None).unwrap();
        durable.subscribe(&[("queue/x".to_string(), 1)]).unwrap();
        durable.disconnect();
        let publisher = MqttClient::connect(options(&broker, "pub", version), None).unwrap();
        publisher.publish("queue/x", b"while-offline", 1, false).unwrap();
        publisher.publish("data", b"qos2", 2, false).unwrap();
        let m = rx.recv_timeout(WAIT).unwrap();
        assert_eq!((m.topic.as_str(), m.qos), ("data", 2));
        let (tx2, rx2) = mpsc::channel::<MqttMessage>();
        let tx2 = std::sync::Mutex::new(tx2);
        let h2: camel_kafka_mqtt::mqtt::MessageHandler = Arc::new(move |m| {
            let _ = tx2.lock().unwrap().send(m);
        });
        let durable = MqttClient::connect(o, Some(h2)).unwrap();
        let m = rx2.recv_timeout(WAIT).unwrap();
        assert_eq!(m.payload, b"while-offline".to_vec());
        durable.disconnect();

        // unsubscribe stops delivery
        watcher.unsubscribe(&["data".to_string()]).unwrap();
        publisher.publish("data", b"ignored", 1, false).unwrap();
        publisher.publish("wills/marker", b"marker", 1, false).unwrap();
        let m = rx.recv_timeout(WAIT).unwrap();
        assert_eq!(m.topic, "wills/marker", "no message on unsubscribed topic and no will for a clean disconnect");
        assert!(watcher.subscribe(&[("bad/#/filter".to_string(), 0)]).is_err());
        assert!(publisher.publish("bad/+", b"x", 0, false).is_err());
        publisher.disconnect();
        watcher.disconnect();
        broker.stop();
    }
}

#[test]
fn unsupported_transports_fail_clearly() {
    let ctx = context();
    for uri in ["paho:t?brokerUrl=ws://localhost:8883", "paho-mqtt5:t?brokerUrl=wss://localhost:443"] {
        let err = ctx.get_endpoint(uri).err().expect("must fail");
        assert!(err.message.contains("not supported"), "{}", err.message);
    }
    assert!(ctx.get_endpoint("paho:t?qos=3").is_err());
    assert!(ctx.get_endpoint("paho:t?nope=1").is_err());
}

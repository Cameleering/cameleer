//! Live interop: `paho` against the MQTT transport connector of a real Apache ActiveMQ 5.17
//! broker (Ubuntu package) started by `scripts/live-interop.sh`, with ActiveMQ's simple
//! authentication plugin. Runs only with `CAMEL_LIVE_INTEROP=1`.
//!
//! ActiveMQ 5 speaks MQTT 3.1 / 3.1.1 only, so `paho-mqtt5` is expected to be refused.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use cameleer_core::prelude::*;
use cameleer_kafka_mqtt::mqtt::{MqttClient, MqttMessage, MqttOptions, MqttVersion};
use cameleer_tls::testing::live;

const WAIT: Duration = Duration::from_secs(10);
const PORT: u16 = 11886;
const URL: &str = "tcp://127.0.0.1:11886";
const TLS: &str = "ssl://localhost:18886";
const STOMP_PORT: u16 = 16615;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_kafka_mqtt::register(&ctx);
    live::bind_beans(&ctx);
    ctx
}

fn ns(test: &str) -> String {
    format!("live/{}/{}", std::process::id(), test)
}

fn creds() -> String {
    format!("userName=camel&password={}", live::PASSWORD)
}

fn options(id: &str) -> MqttOptions {
    let mut o = MqttOptions::new("127.0.0.1", PORT, id);
    o.version = MqttVersion::V311;
    o.username = Some("camel".into());
    o.password = Some(live::PASSWORD.into());
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
fn live_activemq_mqtt_qos_0_1_2() {
    if !live::enabled() {
        return;
    }
    let base = ns("qos");
    let ctx = context();
    let (b, c) = (base.clone(), creds());
    ctx.add_routes(move |r| {
        for q in 0..3 {
            r.from(&format!("paho:{}/q{}?brokerUrl={}&qos={}&{}", b, q, URL, q, c)).to(&format!("mock:q{}", q));
        }
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for q in 0..3 {
        let mock = ctx.mock_endpoint(&format!("mock:q{}", q)).unwrap();
        mock.expected_bodies_received(vec!["one", "two", "three"]);
        mock.expected_header_received("CamelMqttTopic", format!("{}/q{}", base, q));
        mock.expected_header_received("CamelMqttQoS", q as i64);
        for body in ["one", "two", "three"] {
            t.send_body(&format!("paho:{}/q{}?brokerUrl={}&qos={}&{}", base, q, URL, q, creds()), body).unwrap_or_else(|e| panic!("qos {}: {}", q, e));
        }
        mock.assert_is_satisfied_within(WAIT).unwrap_or_else(|e| panic!("qos {}: {}", q, e));
    }
    // the subscription QoS caps the delivery QoS
    let mock = ctx.mock_endpoint("mock:q1").unwrap();
    mock.reset();
    mock.expected_header_received("CamelMqttQoS", 1i64);
    mock.expected_message_count(1);
    t.send_body(&format!("paho:{}/q1?brokerUrl={}&qos=2&{}", base, URL, creds()), "capped").unwrap();
    mock.assert_is_satisfied_within(WAIT).unwrap();
    // a 256 KiB payload at QoS 2
    let mock = ctx.mock_endpoint("mock:q2").unwrap();
    mock.reset();
    mock.expected_message_count(1);
    let big = vec![b'a'; 256 * 1024];
    t.send_body(&format!("paho:{}/q2?brokerUrl={}&qos=2&{}", base, URL, creds()), Value::Bytes(big.clone())).unwrap();
    mock.assert_is_satisfied_within(WAIT).unwrap();
    assert_eq!(mock.received_exchanges()[0].body(), &Value::Bytes(big));
    ctx.stop().unwrap();
}

#[test]
fn live_activemq_mqtt_wildcards_retained_and_auth() {
    if !live::enabled() {
        return;
    }
    let base = ns("wild");
    let ctx = context();
    let (b, c) = (base.clone(), creds());
    ctx.add_routes(move |r| {
        r.from(&format!("paho:{}/sensors/+/temperature?brokerUrl={}&qos=1&{}", b, URL, c)).to("mock:plus");
        r.from(&format!("paho:{}/sensors/#?brokerUrl={}&qos=1&{}", b, URL, c)).to("mock:hash");
    })
    .unwrap();
    ctx.start().unwrap();
    let plus = ctx.mock_endpoint("mock:plus").unwrap();
    plus.expected_bodies_received(vec!["21", "23"]);
    let hash = ctx.mock_endpoint("mock:hash").unwrap();
    hash.expected_bodies_received(vec!["21", "40", "23"]);
    let t = ctx.create_producer_template();
    let producer = format!("paho:unused?brokerUrl={}&qos=1&{}", URL, creds());
    for (topic, body) in [("sensors/kitchen/temperature", "21"), ("sensors/kitchen/humidity", "40"), ("sensors/bath/temperature", "23"), ("other/x", "no")] {
        t.send_body_and_header(&producer, body, "CamelPahoOverrideTopic", format!("{}/{}", base, topic)).unwrap();
    }
    MockEndpoint::assert_all_satisfied(&[&plus, &hash]).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(hash.received_counter(), 3, "other/x must not match");

    // retained messages reach late subscribers; an empty retained payload clears them
    t.send_body(&format!("paho:{}/status/device1?brokerUrl={}&qos=1&retained=true&{}", base, URL, creds()), "online").unwrap();
    let (b, c) = (base.clone(), creds());
    ctx.add_routes(move |r| {
        r.from(&format!("paho:{}/status/+?brokerUrl={}&qos=1&{}", b, URL, c)).to("mock:late");
    })
    .unwrap();
    let late = ctx.mock_endpoint("mock:late").unwrap();
    late.expected_bodies_received(vec!["online"]);
    late.expected_header_received("CamelMqttRetained", true);
    late.assert_is_satisfied_within(WAIT).unwrap();
    t.send_body(&format!("paho:{}/status/device1?brokerUrl={}&qos=1&retained=true&{}", base, URL, creds()), "").unwrap();
    let (h, rx) = collector();
    let c = MqttClient::connect(options(&format!("chk-{}", std::process::id())), Some(h)).unwrap();
    c.subscribe(&[(format!("{}/status/device1", base), 1)]).unwrap();
    assert!(rx.recv_timeout(Duration::from_millis(500)).is_err(), "retained message was cleared");
    c.disconnect();
    ctx.stop().unwrap();

    // the simple authentication plugin refuses bad and missing credentials
    let ctx = context();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for extra in ["&userName=camel&password=wrong", ""] {
        let ex = t.send(&format!("paho:{}/x?brokerUrl={}&automaticReconnect=false{}", base, URL, extra), Exchange::with_body(Some(ctx.clone()), "x"));
        let err = ex.exception.expect("refused");
        let m = err.message.to_ascii_lowercase();
        assert!(m.contains("not authorized") || m.contains("bad user name or password"), "{:?}", err);
    }
    // ActiveMQ 5 has no MQTT 5
    let ex = t.send(&format!("paho-mqtt5:{}/x?brokerUrl={}&automaticReconnect=false&{}", base, URL, creds()), Exchange::with_body(Some(ctx.clone()), "x"));
    assert!(ex.exception.is_some(), "MQTT 5 refused by ActiveMQ 5");
    ctx.stop().unwrap();
}

#[test]
fn live_activemq_mqtt_persistent_session() {
    if !live::enabled() {
        return;
    }
    let base = ns("session");
    let id = format!("durable-{}", std::process::id());
    let mut o = options(&id);
    o.clean_session = false;
    let durable = MqttClient::connect(o.clone(), None).unwrap();
    durable.subscribe(&[(format!("{}/queue/x", base), 1)]).unwrap();
    durable.disconnect();
    let publisher = MqttClient::connect(options(&format!("pub-{}", std::process::id())), None).unwrap();
    publisher.publish(&format!("{}/queue/x", base), b"while-offline", 1, false).unwrap();
    publisher.disconnect();
    let (h, rx) = collector();
    let durable = MqttClient::connect(o.clone(), Some(h)).unwrap();
    let m = rx.recv_timeout(WAIT).unwrap();
    assert_eq!((m.payload.as_slice(), m.qos), (&b"while-offline"[..], 1));
    durable.disconnect();
    let mut clean = o;
    clean.clean_session = true;
    MqttClient::connect(clean, None).unwrap().disconnect();
}

#[test]
fn live_activemq_mqtt_over_tls() {
    if !live::enabled() {
        return;
    }
    let base = ns("tls");
    let ctx = context();
    let opts = format!("brokerUrl={}&qos=1&sslContextParameters=#liveSsl&{}", TLS, creds());
    let (b, o) = (base.clone(), opts.clone());
    ctx.add_routes(move |r| {
        r.from(&format!("paho:{}/in?{}", b, o)).to("mock:tls");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:tls").unwrap();
    mock.expected_bodies_received(vec!["tls 1", "tls 2"]);
    let t = ctx.create_producer_template();
    for body in ["tls 1", "tls 2"] {
        t.send_body(&format!("paho:{}/in?{}", base, opts), body).unwrap();
    }
    mock.assert_is_satisfied_within(WAIT).unwrap();
    let err = t.send_body(&format!("paho:{}/x?brokerUrl={}&qos=1&sslContextParameters=#liveUntrusted&automaticReconnect=false&connectionTimeout=5&{}", base, TLS, creds()), "x").unwrap_err();
    assert!(format!("{:?}", err).to_ascii_lowercase().contains("certificate") || err.root_cause().is_a("IOException"), "{:?}", err);
    ctx.stop().unwrap();
}

/// A minimal STOMP client (this crate does not depend on the STOMP component).
struct Stomp {
    w: TcpStream,
    r: BufReader<TcpStream>,
}

impl Stomp {
    fn connect() -> Stomp {
        let w = TcpStream::connect(("127.0.0.1", STOMP_PORT)).unwrap();
        w.set_read_timeout(Some(WAIT)).unwrap();
        let r = BufReader::new(w.try_clone().unwrap());
        let mut s = Stomp { w, r };
        s.write(&format!("CONNECT\naccept-version:1.2\nhost:localhost\nlogin:camel\npasscode:{}\n\n", live::PASSWORD), b"");
        assert_eq!(s.read().0, "CONNECTED");
        s
    }
    fn write(&mut self, head: &str, body: &[u8]) {
        self.w.write_all(head.as_bytes()).unwrap();
        self.w.write_all(body).unwrap();
        self.w.write_all(b"\0").unwrap();
    }
    /// (command, headers, body)
    fn read(&mut self) -> (String, Vec<(String, String)>, Vec<u8>) {
        let mut line = String::new();
        loop {
            line.clear();
            self.r.read_line(&mut line).unwrap();
            if !line.trim().is_empty() {
                break;
            }
        }
        let command = line.trim().to_string();
        let mut headers = Vec::new();
        loop {
            line.clear();
            self.r.read_line(&mut line).unwrap();
            let l = line.trim_end_matches(['\r', '\n']);
            if l.is_empty() {
                break;
            }
            if let Some((k, v)) = l.split_once(':') {
                headers.push((k.to_string(), v.to_string()));
            }
        }
        let len = headers.iter().find(|(k, _)| k == "content-length").and_then(|(_, v)| v.parse::<usize>().ok());
        let mut body = Vec::new();
        match len {
            Some(n) => {
                body.resize(n, 0);
                self.r.read_exact(&mut body).unwrap();
                let mut nul = [0u8];
                self.r.read_exact(&mut nul).unwrap();
            }
            None => {
                self.r.read_until(0, &mut body).unwrap();
                body.pop();
            }
        }
        (command, headers, body)
    }
}

/// ActiveMQ bridges protocols: MQTT topic `a/b/c` is the JMS / STOMP topic `a.b.c`.
#[test]
fn live_activemq_mqtt_stomp_bridge() {
    if !live::enabled() {
        return;
    }
    let base = ns("bridge");
    let jms_topic = base.replace('/', ".");
    let mut stomp = Stomp::connect();
    stomp.write(&format!("SUBSCRIBE\ndestination:/topic/{}.from-mqtt\nid:1\nack:auto\nreceipt:r1\n\n", jms_topic), b"");
    assert_eq!(stomp.read().0, "RECEIPT");
    let ctx = context();
    let (b, c) = (base.clone(), creds());
    ctx.add_routes(move |r| {
        r.from(&format!("paho:{}/from-stomp?brokerUrl={}&qos=1&{}", b, URL, c)).to("mock:from-stomp");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body(&format!("paho:{}/from-mqtt?brokerUrl={}&qos=1&{}", base, URL, creds()), "mqtt says hi").unwrap();
    let (cmd, headers, body) = stomp.read();
    assert_eq!(cmd, "MESSAGE");
    assert_eq!(String::from_utf8_lossy(&body), "mqtt says hi");
    assert!(headers.iter().any(|(k, v)| k == "destination" && *v == format!("/topic/{}.from-mqtt", jms_topic)), "{:?}", headers);
    let mock = ctx.mock_endpoint("mock:from-stomp").unwrap();
    mock.expected_bodies_received(vec!["stomp says hi"]);
    mock.expected_header_received("CamelMqttTopic", format!("{}/from-stomp", base));
    // a bytes message (content-length) reaches MQTT as the raw payload
    stomp.write(&format!("SEND\ndestination:/topic/{}.from-stomp\ncontent-length:13\n\n", jms_topic), b"stomp says hi");
    mock.assert_is_satisfied_within(WAIT).unwrap();
    ctx.stop().unwrap();
    stomp.write("DISCONNECT\n\n", b"");
}

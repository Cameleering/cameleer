//! End-to-end TLS / SASL tests: Kafka `securityProtocol=SSL|SASL_PLAINTEXT|SASL_SSL`
//! (PLAIN, SCRAM-SHA-256, SCRAM-SHA-512) and MQTT `ssl://` against the embedded brokers.

use std::time::Duration;

use camel_core::prelude::*;
use camel_kafka_mqtt::testing::{KafkaBroker, KafkaBrokerConfig, MqttBroker, MqttBrokerConfig};
use camel_tls::testing;

const WAIT: Duration = Duration::from_secs(20);
const FAST: &str = "pollTimeoutMs=100&heartbeatIntervalMs=50&fetchWaitMaxMs=100";

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    camel_kafka_mqtt::register(&ctx);
    testing::bind_test_beans(&ctx);
    ctx
}

fn ssl_error(e: &CamelError) -> bool {
    e.chain().any(|c| c.is_a("SSLException"))
}

fn tls_broker(sasl_users: Vec<(String, String)>, tls: bool) -> KafkaBroker {
    KafkaBroker::start_with(KafkaBrokerConfig {
        tls: if tls { Some(testing::server_params().server_config().unwrap()) } else { None },
        sasl_users,
        ..Default::default()
    })
    .unwrap()
}

fn jaas(user: &str, password: &str) -> String {
    format!("org.apache.kafka.common.security.scram.ScramLoginModule required username=\"{}\" password=\"{}\";", user, password)
}

#[test]
fn kafka_ssl_produce_and_consume() {
    let broker = tls_broker(Vec::new(), true);
    broker.create_topic("secure", 1);
    let b = format!("localhost:{}", broker.port());
    let ctx = context();
    let consumer = format!("kafka:secure?brokers={}&securityProtocol=SSL&sslContextParameters=#ssl&groupId=g&autoOffsetReset=earliest&{}", b, FAST);
    ctx.add_routes(|r| {
        r.from(&consumer).to("mock:secure");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:secure").unwrap();
    mock.expected_bodies_received(vec!["a", "b"]);
    let t = ctx.create_producer_template();
    let producer = format!("kafka:secure?brokers={}&securityProtocol=SSL&sslContextParameters=#ssl", b);
    t.send_body(&producer, "a").unwrap();
    t.send_body(&producer, "b").unwrap();
    mock.assert_is_satisfied_within(WAIT).unwrap();
    ctx.stop().unwrap();

    // PLAINTEXT client against the TLS listener fails, untrusted / wrong-name certificates fail
    let ctx = context();
    let t = ctx.create_producer_template();
    assert!(t.send_body(&format!("kafka:secure?brokers={}&requestTimeoutMs=3000", b), "x").is_err());
    let err = t.send_body(&format!("kafka:secure?brokers={}&securityProtocol=SSL&sslContextParameters=#sslUntrusted", b), "x").unwrap_err();
    assert!(ssl_error(&err), "{:?}", err);
    let wrong = KafkaBroker::start_with(KafkaBrokerConfig { tls: Some(testing::wrong_host_server_params().server_config().unwrap()), ..Default::default() }).unwrap();
    let err = t.send_body(&format!("kafka:secure?brokers=localhost:{}&securityProtocol=SSL&sslContextParameters=#ssl", wrong.port()), "x").unwrap_err();
    assert!(ssl_error(&err), "{:?}", err);
    // ... unless host name verification is disabled (sslEndpointIdentificationAlgorithm empty)
    t.send_body(&format!("kafka:secure?brokers=localhost:{}&securityProtocol=SSL&sslContextParameters=#ssl&sslEndpointIdentificationAlgorithm=", wrong.port()), "ok")
        .unwrap();
    wrong.stop();
    broker.stop();
}

#[test]
fn kafka_sasl_ssl_plain_and_scram() {
    let users = vec![("alice".to_string(), "alice-secret".to_string())];
    let broker = tls_broker(users, true);
    broker.create_topic("auth", 1);
    let b = format!("localhost:{}", broker.port());
    let ctx = context();
    let t = ctx.create_producer_template();
    for mech in ["PLAIN", "SCRAM-SHA-256", "SCRAM-SHA-512"] {
        let uri = format!(
            "kafka:auth?brokers={}&securityProtocol=SASL_SSL&saslMechanism={}&sslContextParameters=#ssl&saslJaasConfig={}",
            b,
            mech,
            camel_core::util::url_encode(&jaas("alice", "alice-secret"))
        );
        t.send_body(&uri, format!("via {}", mech)).unwrap_or_else(|e| panic!("{}: {:?}", mech, e));
        let bad = format!(
            "kafka:auth?brokers={}&securityProtocol=SASL_SSL&saslMechanism={}&sslContextParameters=#ssl&saslJaasConfig={}",
            b,
            mech,
            camel_core::util::url_encode(&jaas("alice", "wrong"))
        );
        let err = t.send_body(&bad, "x").unwrap_err();
        assert!(err.chain().any(|c| c.kind == "SaslAuthenticationException"), "{}: {:?}", mech, err);
    }
    let logins = broker.sasl_logins();
    assert!(logins.contains(&("PLAIN".to_string(), "alice".to_string())), "{:?}", logins);
    // every connection (bootstrap and leader) authenticates; both SCRAM variants succeeded
    assert!(logins.iter().filter(|(m, _)| m == "SCRAM").count() >= 2, "{:?}", logins);
    // unauthenticated clients are disconnected
    assert!(t.send_body(&format!("kafka:auth?brokers={}&securityProtocol=SSL&sslContextParameters=#ssl&requestTimeoutMs=3000", b), "x").is_err());

    // consume with SASL_SSL + SCRAM-SHA-512
    let consumer = format!(
        "kafka:auth?brokers={}&securityProtocol=SASL_SSL&saslMechanism=SCRAM-SHA-512&sslContextParameters=#ssl&saslJaasConfig={}&groupId=g&autoOffsetReset=earliest&{}",
        b,
        camel_core::util::url_encode(&jaas("alice", "alice-secret")),
        FAST
    );
    ctx.add_routes(|r| {
        r.from(&consumer).to("mock:auth");
    })
    .unwrap();
    let mock = ctx.mock_endpoint("mock:auth").unwrap();
    mock.expected_bodies_received(vec!["via PLAIN", "via SCRAM-SHA-256", "via SCRAM-SHA-512"]);
    ctx.start().unwrap();
    mock.assert_is_satisfied_within(WAIT).unwrap();
    ctx.stop().unwrap();
    broker.stop();
}

#[test]
fn kafka_sasl_plaintext() {
    let broker = tls_broker(vec![("bob".to_string(), "pw".to_string())], false);
    let ctx = context();
    let t = ctx.create_producer_template();
    let uri = format!(
        "kafka:plain?brokers={}&securityProtocol=SASL_PLAINTEXT&saslJaasConfig={}",
        broker.bootstrap(),
        camel_core::util::url_encode("org.apache.kafka.common.security.plain.PlainLoginModule required username=\"bob\" password=\"pw\";")
    );
    t.send_body(&uri, "hello").unwrap();
    let logins = broker.sasl_logins();
    assert!(!logins.is_empty() && logins.iter().all(|l| l == &("PLAIN".to_string(), "bob".to_string())), "{:?}", logins);
    assert!(ctx.get_endpoint("kafka:x?securityProtocol=SASL_SSL").is_err(), "saslJaasConfig is required");
    assert!(ctx.get_endpoint("kafka:x?securityProtocol=BOGUS").is_err());
    broker.stop();
}

#[test]
fn mqtt_over_ssl() {
    for scheme in ["paho", "paho-mqtt5"] {
        let broker = MqttBroker::start_with(MqttBrokerConfig { tls: Some(testing::server_params().server_config().unwrap()), ..Default::default() }).unwrap();
        let url = format!("ssl://localhost:{}", broker.port());
        let ctx = context();
        ctx.add_routes(|r| {
            r.from(&format!("{}:secure/t?brokerUrl={}&qos=1&sslContextParameters=#ssl", scheme, url)).to("mock:mqtt");
        })
        .unwrap();
        ctx.start().unwrap();
        let mock = ctx.mock_endpoint("mock:mqtt").unwrap();
        mock.expected_bodies_received(vec!["tls 1", "tls 2"]);
        let t = ctx.create_producer_template();
        for body in ["tls 1", "tls 2"] {
            t.send_body(&format!("{}:secure/t?brokerUrl={}&qos=1&sslContextParameters=#ssl", scheme, url), body).unwrap();
        }
        mock.assert_is_satisfied_within(WAIT).unwrap();
        // untrusted certificate
        let err = t
            .send_body(&format!("{}:secure/t?brokerUrl={}&qos=1&sslContextParameters=#sslUntrusted&automaticReconnect=false&connectionTimeout=5", scheme, url), "x")
            .unwrap_err();
        assert!(ssl_error(&err), "{:?}", err);
        ctx.stop().unwrap();
        broker.stop();
    }
}

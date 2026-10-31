//! End-to-end TLS tests: `amqps` (spring-rabbitmq / rabbitmq) and STOMP over TLS
//! (`stomp`, `jms` with `ssl://` / `stomp+ssl://` broker urls) against the embedded brokers.

use camel_amqp::testing::{AmqpBroker, StompBroker};
use camel_core::prelude::*;
use camel_tls::testing;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    camel_amqp::register(&ctx);
    testing::bind_test_beans(&ctx);
    ctx
}

fn ssl_error(e: &CamelError) -> bool {
    e.chain().any(|c| c.is_a("SSLException"))
}

#[test]
fn amqps_spring_rabbitmq_and_rabbitmq() {
    let broker = AmqpBroker::builder().tls(testing::server_params().server_config().unwrap()).start().unwrap();
    assert!(broker.url().starts_with("amqps://"));
    let ctx = context();
    ctx.properties().set("camel.component.spring-rabbitmq.addresses", format!("amqps://localhost:{}", broker.port()));
    ctx.properties().set("camel.component.spring-rabbitmq.sslContextParameters", "#ssl");
    let legacy = format!("rabbitmq:legacy?hostname=localhost&portNumber={}&queue=lq&sslProtocol=TLS&sslContextParameters=#ssl&autoDelete=false", broker.port());
    ctx.add_routes(|b| {
        b.from("spring-rabbitmq:secure?queues=sq&routingKey=k").to("mock:spring");
        b.from(&legacy).to("mock:legacy");
    })
    .unwrap();
    ctx.start().unwrap();
    let spring = ctx.mock_endpoint("mock:spring").unwrap();
    spring.expected_bodies_received(vec!["s1", "s2"]);
    let legacy_mock = ctx.mock_endpoint("mock:legacy").unwrap();
    legacy_mock.expected_bodies_received(vec!["l1"]);
    let t = ctx.create_producer_template();
    t.send_body("spring-rabbitmq:secure?routingKey=k", "s1").unwrap();
    t.send_body("spring-rabbitmq:secure?routingKey=k", "s2").unwrap();
    t.send_body(&legacy, "l1").unwrap();
    MockEndpoint::assert_all_satisfied(&[&spring, &legacy_mock]).unwrap();
    ctx.stop().unwrap();

    // plain AMQP and untrusted certificates are rejected
    let ctx = context();
    let t = ctx.create_producer_template();
    let plain = format!("rabbitmq:x?hostname=localhost&portNumber={}&connectionTimeout=3000", broker.port());
    assert!(t.send_body(&plain, "x").is_err());
    let untrusted = format!("rabbitmq:x?hostname=localhost&portNumber={}&sslProtocol=true&sslContextParameters=#sslUntrusted", broker.port());
    let err = t.send_body(&untrusted, "x").unwrap_err();
    assert!(ssl_error(&err), "{:?}", err);
    let wrong = AmqpBroker::builder().tls(testing::wrong_host_server_params().server_config().unwrap()).start().unwrap();
    let mismatch = format!("rabbitmq:x?hostname=localhost&portNumber={}&sslProtocol=true&sslContextParameters=#ssl", wrong.port());
    let err = t.send_body(&mismatch, "x").unwrap_err();
    assert!(ssl_error(&err), "{:?}", err);
    wrong.stop();
    broker.stop();
}

#[test]
fn stomp_and_jms_over_tls() {
    let broker = StompBroker::builder().tls(testing::server_params().server_config().unwrap()).start().unwrap();
    let ctx = context();
    let stomp_uri = format!("stomp:queue:secure?brokerURL=stomp+ssl://localhost:{}&sslContextParameters=#ssl", broker.port());
    let jms_uri = format!("jms:queue:orders?brokerURL=ssl://localhost:{}&sslContextParameters=#ssl", broker.port());
    ctx.add_routes(|b| {
        b.from(&stomp_uri).to("mock:stomp");
        b.from(&jms_uri).to("mock:jms");
    })
    .unwrap();
    ctx.start().unwrap();
    let stomp = ctx.mock_endpoint("mock:stomp").unwrap();
    stomp.expected_bodies_received(vec!["over tls"]);
    let jms = ctx.mock_endpoint("mock:jms").unwrap();
    jms.expected_bodies_received(vec!["order 1"]);
    let t = ctx.create_producer_template();
    t.send_body(&stomp_uri, "over tls").unwrap();
    t.send_body(&jms_uri, "order 1").unwrap();
    MockEndpoint::assert_all_satisfied(&[&stomp, &jms]).unwrap();
    ctx.stop().unwrap();

    let ctx = context();
    let t = ctx.create_producer_template();
    let err = t.send_body(&format!("stomp:queue:x?brokerURL=ssl://localhost:{}&sslContextParameters=#sslUntrusted", broker.port()), "x").unwrap_err();
    assert!(ssl_error(&err), "{:?}", err);
    assert!(t.send_body(&format!("stomp:queue:x?brokerURL=tcp://localhost:{}", broker.port()), "x").is_err(), "plain STOMP to the TLS port");
    broker.stop();
}

/// Spring Boot `spring.rabbitmq.ssl.key-store` / `trust-store` properties with PKCS#12 stores
/// against a broker requiring client certificates.
#[test]
fn amqps_client_certificates_from_spring_key_store_properties() {
    use camel_tls::keystore::KeyStore;
    let pki = testing::mtls();
    let broker = AmqpBroker::builder().tls(testing::mtls_server_params(camel_tls::ClientAuth::Need).server_config().unwrap()).start().unwrap();
    let dir = std::env::temp_dir().join(format!("camel-amqp-mtls-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ks = dir.join("client.p12");
    std::fs::write(&ks, KeyStore::from_key_entry("client", pki.client.key.clone(), vec![pki.client.cert_der.clone()]).to_pkcs12("secret")).unwrap();
    let ts = dir.join("trust.p12");
    std::fs::write(&ts, KeyStore::new().add_cert("ca", pki.ca.cert_der.clone()).to_pkcs12("secret")).unwrap();
    let ctx = context();
    ctx.properties().set("spring.rabbitmq.addresses", format!("amqps://localhost:{}", broker.port()));
    ctx.properties().set("spring.rabbitmq.ssl.enabled", "true");
    ctx.properties().set("spring.rabbitmq.ssl.key-store", ks.display().to_string());
    ctx.properties().set("spring.rabbitmq.ssl.key-store-password", "secret");
    ctx.properties().set("spring.rabbitmq.ssl.key-store-type", "PKCS12");
    ctx.properties().set("spring.rabbitmq.ssl.trust-store", ts.display().to_string());
    ctx.properties().set("spring.rabbitmq.ssl.trust-store-password", "secret");
    ctx.add_routes(|b| {
        b.from("spring-rabbitmq:mtls?queues=mq&routingKey=k").to("mock:mtls");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:mtls").unwrap();
    mock.expected_bodies_received(vec!["with client cert"]);
    ctx.create_producer_template().send_body("spring-rabbitmq:mtls?routingKey=k", "with client cert").unwrap();
    mock.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
    // without a client certificate the broker refuses the connection
    let ctx = context();
    testing::bind_mtls_beans(&ctx);
    let anon = format!("rabbitmq:x?hostname=localhost&portNumber={}&sslProtocol=true&sslContextParameters=#sslMtlsAnonymous&connectionTimeout=3000", broker.port());
    assert!(ctx.create_producer_template().send_body(&anon, "x").is_err());
    broker.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

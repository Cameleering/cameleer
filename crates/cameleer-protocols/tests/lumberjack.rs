//! `lumberjack` consumer against the embedded Lumberjack client (plain TCP and TLS).

use std::time::{Duration, Instant};

use cameleer_core::prelude::*;
use cameleer_protocols::testing::lumberjack_client::{data_frame, window_frame};
use cameleer_protocols::testing::LumberjackClient;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_protocols::register(&ctx);
    ctx
}

fn wait_for(m: &MockEndpoint, n: usize) -> Vec<Exchange> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while m.received_counter() < n && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let r = m.received_exchanges();
    assert!(r.len() >= n, "expected {} exchanges, got {}", n, r.len());
    r
}

const FILEBEAT_EVENT: &str = r#"{"@timestamp":"2026-11-25T10:00:00.000Z","@metadata":{"beat":"filebeat","type":"_doc","version":"8.15.0"},"message":"GET /index.html 200","log":{"file":{"path":"/var/log/nginx/access.log"},"offset":0},"host":{"name":"web-01"}}"#;

#[test]
fn json_windows_are_routed_in_order_and_acked() {
    let ctx = context();
    let uri = "lumberjack:127.0.0.1:0";
    ctx.add_routes(|b| {
        b.from(uri).to("mock:events");
    })
    .unwrap();
    ctx.start().unwrap();
    let port = cameleer_protocols::local_port(&ctx, uri).unwrap();
    let mock = ctx.mock_endpoint("mock:events").unwrap();

    let mut c = LumberjackClient::connect("127.0.0.1", port).unwrap();
    // compressed, as Filebeat sends it
    assert_eq!(c.send_json(&[FILEBEAT_EVENT, r#"{"message":"second"}"#, r#"{"message":"third"}"#], true).unwrap(), (b'2', 3));
    // a second window on the same connection, uncompressed
    assert_eq!(c.send_json(&[r#"{"message":"fourth"}"#], false).unwrap(), (b'2', 1));
    let got = wait_for(&mock, 4);
    let messages: Vec<String> = got.iter().map(|e| e.body().get("message").map(|m| m.to_string()).unwrap_or_default()).collect();
    assert_eq!(messages, ["GET /index.html 200", "second", "third", "fourth"]);
    let first = got[0].body();
    assert_eq!(first.get("host").and_then(|h| h.get("name")), Some(&Value::from("web-01")));
    assert_eq!(first.get("log").and_then(|l| l.get("file")).and_then(|f| f.get("path")), Some(&Value::from("/var/log/nginx/access.log")));
    assert_eq!(first.get("@metadata").and_then(|m| m.get("beat")), Some(&Value::from("filebeat")));
    ctx.stop().unwrap();
}

#[test]
fn version_1_data_frames_and_simple_on_the_body() {
    let ctx = context();
    let uri = "lumberjack://127.0.0.1:0";
    ctx.add_routes(|b| {
        b.from(uri).set_body(simple("${body[line]} from ${body[file]}")).to("mock:lines");
    })
    .unwrap();
    ctx.start().unwrap();
    let port = cameleer_protocols::local_port(&ctx, uri).unwrap();
    let mock = ctx.mock_endpoint("mock:lines").unwrap();
    let mut c = LumberjackClient::connect("127.0.0.1", port).unwrap();
    let mut b = window_frame(b'1', 2);
    b.extend(data_frame(b'1', 1, &[("line", "one"), ("file", "/var/log/a")]));
    b.extend(data_frame(b'1', 2, &[("line", "two"), ("file", "/var/log/b")]));
    c.write(&b).unwrap();
    assert_eq!(c.read_ack().unwrap(), (b'1', 2));
    let got = wait_for(&mock, 2);
    assert_eq!(got[0].body().to_string(), "one from /var/log/a");
    assert_eq!(got[1].body().to_string(), "two from /var/log/b");
    ctx.stop().unwrap();
}

#[test]
fn a_failed_exchange_closes_the_connection_without_ack_and_the_window_is_resent() {
    let ctx = context();
    let uri = "lumberjack:127.0.0.1:0";
    ctx.add_routes(|b| {
        b.from(uri)
            .process(|ex: &mut Exchange| {
                if ex.body().get("message").map(|m| m.to_string()) == Some("boom".into()) {
                    return Err(CamelError::illegal_argument("cannot index boom"));
                }
                Ok(())
            })
            .to("mock:ok");
    })
    .unwrap();
    ctx.start().unwrap();
    let port = cameleer_protocols::local_port(&ctx, uri).unwrap();
    let mock = ctx.mock_endpoint("mock:ok").unwrap();
    let mut c = LumberjackClient::connect("127.0.0.1", port).unwrap();
    let r = c.send_json(&[r#"{"message":"a"}"#, r#"{"message":"boom"}"#, r#"{"message":"c"}"#], true);
    assert!(r.is_err(), "no ACK after a failure: {:?}", r);
    assert_eq!(mock.received_counter(), 1, "events after the failed one are not routed");
    // the shipper retries on a new connection
    let mut c = LumberjackClient::connect("127.0.0.1", port).unwrap();
    assert_eq!(c.send_json(&[r#"{"message":"a"}"#, r#"{"message":"c"}"#], true).unwrap(), (b'2', 2));
    wait_for(&mock, 3);
    ctx.stop().unwrap();
}

#[test]
fn protocol_errors_close_the_connection_and_the_server_keeps_serving() {
    let ctx = context();
    let uri = "lumberjack:127.0.0.1:0";
    ctx.add_routes(|b| {
        b.from(uri).to("mock:any");
    })
    .unwrap();
    ctx.start().unwrap();
    let port = cameleer_protocols::local_port(&ctx, uri).unwrap();
    let mut c = LumberjackClient::connect("127.0.0.1", port).unwrap();
    c.write(b"9Z").unwrap();
    assert!(c.read_ack().is_err());
    let mut c = LumberjackClient::connect("127.0.0.1", port).unwrap();
    assert_eq!(c.send_json(&[r#"{"ok":true}"#], false).unwrap(), (b'2', 1));
    ctx.stop().unwrap();
}

#[test]
fn tls_with_ssl_context_parameters() {
    let ctx = context();
    ctx.registry().bind("ssl", cameleer_tls::testing::server_params());
    let uri = "lumberjack:127.0.0.1:0?sslContextParameters=#ssl";
    ctx.add_routes(|b| {
        b.from(uri).to("mock:tls");
    })
    .unwrap();
    ctx.start().unwrap();
    let port = cameleer_protocols::local_port(&ctx, uri).unwrap();
    let client = cameleer_tls::testing::client_params().client_config().unwrap();
    let mut c = LumberjackClient::connect_tls("localhost", port, &client).unwrap();
    assert_eq!(c.send_json(&[r#"{"message":"secret"}"#], true).unwrap(), (b'2', 1));
    let got = wait_for(&ctx.mock_endpoint("mock:tls").unwrap(), 1);
    assert_eq!(got[0].body().get("message"), Some(&Value::from("secret")));
    // a client that does not trust the server certificate gets no session
    let untrusting = cameleer_tls::testing::untrusting_client_params().client_config().unwrap();
    assert!(LumberjackClient::connect_tls("localhost", port, &untrusting).and_then(|mut c| c.send_json(&["{}"], false)).is_err());
    ctx.stop().unwrap();
}

#[test]
fn consumer_only_and_tls_needs_key_material() {
    let ctx = context();
    let ep = ctx.get_endpoint("lumberjack:0.0.0.0").unwrap();
    let e = ep.create_producer().err().unwrap();
    assert_eq!(e.kind, "UnsupportedOperationException");
    assert!(e.message.contains("cannot be used as a producer"));
    assert!(ctx.get_endpoint("lumberjack:").is_err());

    let ctx = context();
    ctx.registry().bind("trustOnly", cameleer_tls::testing::client_params());
    ctx.add_routes(|b| {
        b.from("lumberjack:127.0.0.1:0?sslContextParameters=#trustOnly").to("mock:x");
    })
    .unwrap();
    assert!(ctx.start().is_err(), "a TLS server without a key pair must not start");
}

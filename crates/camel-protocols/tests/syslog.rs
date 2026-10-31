use std::io::Write;
use std::net::{TcpStream, UdpSocket};

use camel_core::prelude::*;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    camel_protocols::register(&ctx);
    ctx
}

const RFC5424: &str = "<165>1 2003-10-11T22:14:15.003Z mymachine.example.com evntslog - ID47 [exampleSDID@32473 iut=\"3\" eventSource=\"Application\" eventID=\"1011\"] An application event log entry...";
const RFC3164: &str = "<34>Oct 11 22:14:15 mymachine su: 'su root' failed for lonvick on /dev/pts/8";

#[test]
fn udp_consumer_parses_rfc_examples() {
    let ctx = context();
    let uri = "syslog:udp://127.0.0.1:0";
    ctx.add_routes(|b| {
        b.from(uri).to("mock:syslog");
    })
    .unwrap();
    ctx.start().unwrap();
    let port = camel_protocols::local_port(&ctx, uri).unwrap();
    let mock = ctx.mock_endpoint("mock:syslog").unwrap();
    mock.expected_message_count(2);
    mock.expected_header_values_received_in_any_order("CamelSyslogSeverity", vec!["NOTICE", "CRIT"]);
    mock.expected_header_values_received_in_any_order("CamelSyslogFacility", vec!["LOCAL4", "AUTH"]);
    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    sock.send_to(RFC5424.as_bytes(), ("127.0.0.1", port)).unwrap();
    // UDP is unordered in theory; on loopback the order is kept, but compare by content
    sock.send_to(RFC3164.as_bytes(), ("127.0.0.1", port)).unwrap();
    mock.assert_is_satisfied().unwrap();
    let ex = mock.received_exchanges();
    let first = ex.iter().find(|e| e.header("CamelSyslogSeverity").map(|v| v.to_string()) == Some("NOTICE".into())).unwrap();
    let body = first.body();
    assert_eq!(body.get("appName"), Some(&Value::from("evntslog")));
    assert_eq!(body.get("msgId"), Some(&Value::from("ID47")));
    assert_eq!(body.get("structuredData").and_then(|s| s.get("exampleSDID@32473")).and_then(|p| p.get("eventID")), Some(&Value::from("1011")));
    assert_eq!(first.header("CamelSyslogHostname").unwrap().to_string(), "mymachine.example.com");
    assert!(first.header("CamelSyslogRemoteAddress").is_some());
    let second = ex.iter().find(|e| e.header("CamelSyslogSeverity").map(|v| v.to_string()) == Some("CRIT".into())).unwrap();
    assert_eq!(second.body().get("logMessage"), Some(&Value::from("'su root' failed for lonvick on /dev/pts/8")));
    ctx.stop().unwrap();
}

#[test]
fn producer_tcp_framing_and_data_format() {
    let ctx = context();
    let tcp = "syslog:tcp://127.0.0.1:0";
    ctx.add_routes(|b| {
        b.from(tcp).to("mock:tcp");
        b.from("syslog:udp://127.0.0.1:0?unmarshal=false").to("mock:raw");
        b.from("direct:unmarshal").unmarshal(DataFormatDefinition::named("syslog"));
        b.from("direct:marshal").marshal(DataFormatDefinition::named("syslog"));
    })
    .unwrap();
    ctx.start().unwrap();
    let tcp_port = camel_protocols::local_port(&ctx, tcp).unwrap();
    let udp_port = camel_protocols::local_port(&ctx, "syslog:udp://127.0.0.1:0?unmarshal=false").unwrap();

    let mock = ctx.mock_endpoint("mock:tcp").unwrap();
    mock.expected_message_count(3);
    // octet counting (RFC 6587) and LF terminated frames on one connection
    let mut s = TcpStream::connect(("127.0.0.1", tcp_port)).unwrap();
    let framed = format!("{} {}{}\n", RFC3164.len(), RFC3164, RFC5424);
    s.write_all(framed.as_bytes()).unwrap();
    drop(s);
    // the producer marshals an object body (RFC 5424 by default)
    let t = ctx.create_producer_template();
    let msg = Value::object([("facility", Value::from("LOCAL0")), ("severity", Value::from("ERR")), ("hostname", Value::from("app01")), ("appName", Value::from("billing")), ("logMessage", Value::from("payment failed"))]);
    t.send_body(&format!("syslog:tcp://127.0.0.1:{}", tcp_port), msg.clone()).unwrap();
    mock.assert_is_satisfied().unwrap();
    let produced = mock.received_exchanges().into_iter().find(|e| e.header("CamelSyslogHostname").map(|v| v.to_string()) == Some("app01".into())).unwrap();
    assert_eq!(produced.body().get("logMessage"), Some(&Value::from("payment failed")));
    assert_eq!(produced.body().get("severity"), Some(&Value::from("ERR")));
    assert_eq!(produced.body().get("priority"), Some(&Value::Int(131)));

    // raw mode over UDP
    let raw = ctx.mock_endpoint("mock:raw").unwrap();
    raw.expected_bodies_received(vec![RFC3164]);
    raw.expected_header_received("CamelSyslogFacility", "AUTH");
    t.send_body(&format!("syslog:udp://127.0.0.1:{}", udp_port), RFC3164).unwrap();
    raw.assert_is_satisfied().unwrap();

    // data format round trip
    let parsed = t.request_body("direct:unmarshal", RFC5424).unwrap();
    assert_eq!(parsed.get("hostname"), Some(&Value::from("mymachine.example.com")));
    let text = t.request_body("direct:marshal", parsed).unwrap().to_string();
    assert!(text.starts_with("<165>1 2003-10-11T22:14:15.003Z mymachine.example.com evntslog - ID47 [exampleSDID@32473 "), "{}", text);
    assert!(text.ends_with("] An application event log entry..."), "{}", text);
    assert!(t.request_body("direct:unmarshal", "garbage").is_err());
    ctx.stop().unwrap();
}

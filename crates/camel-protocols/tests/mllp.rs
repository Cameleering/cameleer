use std::io::{BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

use camel_core::prelude::*;
use camel_protocols::mllp::{frame, read_frame};

const ADT: &str = "MSH|^~\\&|SENDAPP|SENDFAC|RECVAPP|RECVFAC|20240102120000||ADT^A01^ADT_A01|MSG00001|P|2.5\rEVN|A01|20240102120000\rPID|1||12345^^^MRN||Doe^John||19800101|M\r";

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    camel_protocols::register(&ctx);
    ctx
}

#[test]
fn mllp_round_trip_with_auto_ack() {
    let ctx = context();
    let uri = "mllp://127.0.0.1:0";
    ctx.add_routes(|b| {
        b.from(uri).set_header("patient", language("hl7terser", "PID-5-1")).to("mock:hl7");
    })
    .unwrap();
    ctx.start().unwrap();
    let port = camel_protocols::local_port(&ctx, uri).unwrap();
    let mock = ctx.mock_endpoint("mock:hl7").unwrap();
    mock.expected_bodies_received(vec![ADT]);
    mock.expected_header_received("CamelMllpMessageControlId", "MSG00001");
    mock.expected_header_received("CamelMllpSendingApplication", "SENDAPP");
    mock.expected_header_received("CamelMllpReceivingFacility", "RECVFAC");
    mock.expected_header_received("CamelMllpMessageType", "ADT^A01^ADT_A01");
    mock.expected_header_received("CamelMllpEventType", "ADT");
    mock.expected_header_received("CamelMllpTriggerEvent", "A01");
    mock.expected_header_received("CamelMllpVersionId", "2.5");
    mock.expected_header_received("patient", "Doe");

    let t = ctx.create_producer_template();
    let producer = format!("mllp://127.0.0.1:{}", port);
    for _ in 0..2 {
        // the second exchange reuses the connection
        let ex = t.request(&producer, |ex| ex.set_body(ADT)).unwrap();
        assert!(ex.exception.is_none(), "{:?}", ex.exception);
        assert_eq!(ex.header("CamelMllpAcknowledgementType").unwrap().to_string(), "AA");
        let ack = ex.header("CamelMllpAcknowledgementString").unwrap().to_string();
        assert!(ack.contains("\rMSA|AA|MSG00001"), "{}", ack);
        assert!(ack.starts_with("MSH|^~\\&|RECVAPP|RECVFAC|SENDAPP|SENDFAC|"), "{}", ack);
        assert!(matches!(ex.header("CamelMllpAcknowledgement"), Some(Value::Bytes(_))));
        assert_eq!(camel_protocols::hl7::hl7_terser(&ack, "MSH-9-2").as_deref(), Some("A01"));
    }
    mock.expected_message_count(2);
    mock.assert_is_satisfied().unwrap();

    // raw client: out-of-band bytes before the frame are ignored
    let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut data = b"\r\n".to_vec();
    data.extend(frame(ADT.replace("MSG00001", "RAW42").as_bytes()));
    (&s).write_all(&data).unwrap();
    let ack = read_frame(&mut BufReader::new(&s), 1 << 20).unwrap().unwrap();
    assert!(String::from_utf8(ack).unwrap().contains("MSA|AA|RAW42"));
    ctx.stop().unwrap();
}

#[test]
fn mllp_errors_custom_acks_and_timeouts() {
    let ctx = context();
    ctx.add_routes(|b| {
        b.from("mllp://127.0.0.1:0?hl7Headers=false").process(|_ex: &mut Exchange| Err(CamelError::illegal_state("database down")));
        b.from("mllp://localhost:0?autoAck=false").process(|ex: &mut Exchange| {
            let id = ex.header("CamelMllpMessageControlId").map(|v| v.to_string()).unwrap_or_default();
            ex.set_header("CamelMllpAcknowledgementString", format!("MSH|^~\\&|A|B|C|D|20240101||ACK|X|P|2.5\rMSA|AR|{}\r", id));
            Ok(())
        });
        b.from("mllp://0.0.0.0:0?autoAck=false").to("mock:silent");
        b.from("mllp://127.0.0.1:0?stringPayload=false").unmarshal(DataFormatDefinition::named("hl7")).to("mock:parsed");
    })
    .unwrap();
    ctx.start().unwrap();
    let port = |u: &str| camel_protocols::local_port(&ctx, u).unwrap();
    let t = ctx.create_producer_template();

    let ex = t.send(&format!("mllp://127.0.0.1:{}", port("mllp://127.0.0.1:0?hl7Headers=false")), Exchange::with_body(Some(ctx.clone()), ADT));
    let e = ex.exception.clone().expect("AE must fail the exchange");
    assert_eq!(e.kind, "MllpApplicationErrorAcknowledgementException");
    assert_eq!(ex.header("CamelMllpAcknowledgementType").unwrap().to_string(), "AE");
    assert!(ex.header("CamelMllpAcknowledgementString").unwrap().to_string().contains("database down"));

    let ex = t.send(&format!("mllp://127.0.0.1:{}", port("mllp://localhost:0?autoAck=false")), Exchange::with_body(Some(ctx.clone()), ADT));
    assert_eq!(ex.exception.as_ref().unwrap().kind, "MllpApplicationRejectAcknowledgementException");
    assert!(ex.header("CamelMllpAcknowledgementString").unwrap().to_string().contains("MSA|AR|MSG00001"));

    let silent = ctx.mock_endpoint("mock:silent").unwrap();
    silent.expected_message_count(1);
    let ex = t.send(&format!("mllp://127.0.0.1:{}?receiveTimeout=200", port("mllp://0.0.0.0:0?autoAck=false")), Exchange::with_body(Some(ctx.clone()), ADT));
    assert_eq!(ex.exception.as_ref().unwrap().kind, "MllpAcknowledgementTimeoutException");
    silent.assert_is_satisfied().unwrap();

    let parsed = ctx.mock_endpoint("mock:parsed").unwrap();
    parsed.expected_message_count(1);
    parsed.expected_header_received("CamelHL7MessageType", "ADT");
    parsed.expected_header_received("CamelHL7TriggerEvent", "A01");
    parsed.expected_header_received("CamelHL7MessageControl", "MSG00001");
    let ex = t.request(&format!("mllp://127.0.0.1:{}", port("mllp://127.0.0.1:0?stringPayload=false")), |ex| ex.set_body(ADT)).unwrap();
    assert!(ex.exception.is_none());
    parsed.assert_is_satisfied().unwrap();
    let body = parsed.exchange(0).unwrap().body().clone();
    assert_eq!(body.get("messageControlId"), Some(&Value::from("MSG00001")));
    let segs = body.get("segments").and_then(|s| s.as_array()).unwrap();
    assert_eq!(segs.len(), 3);
    assert_eq!(segs[2].get("name"), Some(&Value::from("PID")));
    assert_eq!(segs[2].get("fields").and_then(|f| f.as_array()).map(|f| f[4].clone()), Some(Value::from("Doe^John")));

    // marshal the structured value back to ER7
    ctx.add_routes(|b| {
        b.from("direct:marshal").marshal(DataFormatDefinition::named("hl7"));
    })
    .unwrap();
    assert_eq!(t.request_body("direct:marshal", body).unwrap(), Value::from(ADT));
    ctx.stop().unwrap();
}

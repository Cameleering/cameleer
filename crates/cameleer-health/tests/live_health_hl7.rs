//! Live HL7 v2 over MLLP against python-hl7 0.4.5, in both directions.
//!
//! Needs `scripts/health-interop.sh all`, `eval "$(scripts/health-interop.sh env)"` and
//! `CAMEL_HEALTH_INTEROP=1`; returns early otherwise.

use std::process::{Command, Stdio};
use std::io::Write;

use cameleer_core::prelude::*;

fn enabled() -> bool {
    let on = std::env::var("CAMEL_HEALTH_INTEROP").map(|v| v == "1").unwrap_or(false);
    if !on {
        eprintln!("skipped: set CAMEL_HEALTH_INTEROP=1 after `scripts/health-interop.sh all`");
    }
    on
}

fn dir() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("CAMEL_HEALTH_INTEROP_DIR").unwrap_or_else(|_| format!("{}/camel-health-interop", std::env::var("HOME").unwrap_or_default())))
}

fn adt(control_id: &str) -> String {
    format!("MSH|^~\\&|CAMELEER|HOSP|PYHL7|LAB|20261110120000||ADT^A01^ADT_A01|{}|P|2.5\rEVN|A01|20261110120000\rPID|1||{}^^^MRN||Doe^Jane||19700101|F\r", control_id, control_id.len())
}

fn unique(prefix: &str) -> String {
    format!("{}{}", prefix, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros())
}

/// A Cameleer `mllp` producer sends to python-hl7's receiver, which acknowledges with its own
/// ACK; an AR from the receiver fails the exchange.
#[test]
fn live_cameleer_mllp_producer_to_python_hl7() {
    if !enabled() {
        return;
    }
    let ctx = CamelContext::new();
    ctx.add_pack(&cameleer_health::HEALTH);
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let id = unique("CAM");
    let ex = t.request("mllp://127.0.0.1:12575", |ex| ex.set_body(adt(&id))).unwrap();
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
    assert_eq!(ex.header("CamelMllpAcknowledgementType").map(|v| v.to_string()).as_deref(), Some("AA"));
    let stored = std::fs::read_to_string(dir().join("hl7").join(format!("{}.hl7", id))).expect("python-hl7 wrote the message");
    assert!(stored.contains("Doe^Jane") && stored.contains(&id), "{stored}");
    // an AR fails the exchange; the template reports it as an error
    let e = t.request("mllp://127.0.0.1:12575", |ex| ex.set_body(adt(&unique("REJECT")))).unwrap_err();
    assert!(e.chain().any(|x| x.kind == "MllpApplicationRejectAcknowledgementException"), "{e:?}");
    assert!(e.message.contains("MSA|AR|"), "{e:?}");
    ctx.stop().unwrap();
}

/// python-hl7's MLLP client sends to a Cameleer `mllp` consumer, whose route reads fields with
/// hl7terser; the client gets Cameleer's ACK.
#[test]
fn live_python_hl7_client_to_cameleer_mllp_consumer() {
    if !enabled() {
        return;
    }
    let ctx = CamelContext::new();
    ctx.add_pack(&cameleer_health::HEALTH);
    ctx.add_routes(|b| {
        b.from("mllp://127.0.0.1:12576").set_header("family", language("hl7terser", "PID-5-1")).to("mock:adt");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:adt").unwrap();
    mock.expected_message_count(1);
    let id = unique("PY");
    let client = std::env::var("CAMEL_HEALTH_HL7_CLIENT").expect("CAMEL_HEALTH_HL7_CLIENT from health-interop.sh env");
    let mut child = Command::new("python3")
        .arg(&client)
        .args(["127.0.0.1", "12576"])
        .env("PYTHONPATH", std::env::var("CAMEL_HEALTH_HL7_PYTHONPATH").unwrap_or_default())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(adt(&id).as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let ack = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{}{}", ack, String::from_utf8_lossy(&out.stderr));
    assert!(ack.contains("MSA|AA|") && ack.contains(&id), "{ack}");
    mock.assert_is_satisfied().unwrap();
    let got = &mock.received_exchanges()[0];
    assert_eq!(got.header("family").map(|v| v.to_string()).as_deref(), Some("Doe"));
    assert_eq!(got.header("CamelMllpMessageControlId").map(|v| v.to_string()).as_deref(), Some(id.as_str()));
    ctx.stop().unwrap();
}

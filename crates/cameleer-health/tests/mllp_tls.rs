//! MLLP over TLS (moved from cameleer-protocols with MLLP itself).

use cameleer_core::prelude::*;
use cameleer_tls::testing;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    ctx.add_pack(&cameleer_health::HEALTH);
    testing::bind_test_beans(&ctx);
    ctx
}

fn ssl_kind(e: &CamelError) -> bool {
    e.chain().any(|x| x.kind.starts_with("SSL"))
}

const ADT: &str = "MSH|^~\\&|SENDAPP|SENDFAC|RECVAPP|RECVFAC|20240102120000||ADT^A01^ADT_A01|MSG00001|P|2.5\rPID|1||12345^^^MRN||Doe^John\r";

#[test]
fn mllp_over_tls() {
    let ctx = context();
    let uri = "mllp://127.0.0.1:0?sslContextParameters=#ssl";
    ctx.add_routes(|b| {
        b.from(uri).to("mock:hl7");
    })
    .unwrap();
    ctx.start().unwrap();
    let port = cameleer_health::local_port(&ctx, uri).unwrap();
    let mock = ctx.mock_endpoint("mock:hl7").unwrap();
    mock.expected_bodies_received(vec![ADT, ADT]);
    let t = ctx.create_producer_template();
    let producer = format!("mllp://localhost:{}?sslContextParameters=#ssl", port);
    for _ in 0..2 {
        let ex = t.request(&producer, |ex| ex.set_body(ADT)).unwrap();
        assert!(ex.exception.is_none(), "{:?}", ex.exception);
        assert_eq!(ex.header("CamelMllpAcknowledgementType").unwrap().to_string(), "AA");
    }
    mock.assert_is_satisfied().unwrap();
    let err = t.request_body(&format!("mllp://localhost:{}?sslContextParameters=#sslUntrusted", port), ADT).unwrap_err();
    assert!(ssl_kind(&err), "{:?}", err);
    ctx.stop().unwrap();
}


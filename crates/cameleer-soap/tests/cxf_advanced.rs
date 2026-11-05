//! cxf: code-first descriptors, `cxf:bean:`, POJO `soap:header` binding, MTOM / SwA,
//! WS-Addressing + WS-RM, SAML, BinarySecurityToken signatures, XML Encryption, rpc/encoded.

use std::time::{Duration, Instant};

use cameleer_core::prelude::*;
use cameleer_soap::codefirst::{OperationDescriptor, ServiceDescriptor};
use cameleer_soap::testing::*;

fn ctx() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    cameleer_soap::register(&ctx);
    ctx.registry().bind("calcWsdl", CALCULATOR_WSDL.to_string());
    ctx
}

fn wait_for(m: &MockEndpoint, n: usize) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while m.received_counter() < n && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(m.received_counter() >= n, "expected {} exchanges, got {}", n, m.received_counter());
}

fn svc() -> String {
    format!("serviceName={{{0}}}CalculatorService&portName={{{0}}}CalculatorSoap", CALC_NS)
}

fn add(ex: &mut Exchange) -> Result<()> {
    let args = ex.message.body.as_array().cloned().unwrap_or_default();
    ex.message.body = Value::Int(args.iter().filter_map(|v| v.as_i64()).sum());
    Ok(())
}

#[test]
fn code_first_bean_endpoint_headers_mtom_and_attachments() {
    let ctx = ctx();
    let files = ServiceDescriptor::new("Files", "urn:files").operation(OperationDescriptor::new("upload").param("name", "string").param("data", "base64Binary").header("token", "string").returns("string"));
    ctx.registry().bind("files", files);
    let server = "cxf://http://127.0.0.1:0/files?serviceClass=#files&mtomEnabled=true";
    ctx.add_routes(move |b| {
        b.from(server)
            .wire_tap("mock:files")
            .process(|ex: &mut Exchange| {
                let a = ex.message.body.as_array().cloned().unwrap_or_default();
                let len = match &a[1] {
                    Value::Bytes(b) => b.len(),
                    other => panic!("expected bytes, got {:?}", other),
                };
                let atts = ex.property("CamelAttachments").and_then(|v| v.as_object()).map(|m| m.len()).unwrap_or(0);
                ex.message.body = Value::String(format!("{}:{}:{}:{}", a[0], len, a[2], atts));
                ex.set_property("CamelAttachments", Value::object([("reply", Value::Bytes(b"pong".to_vec()))]));
                Ok(())
            });
    })
    .unwrap();
    ctx.start().unwrap();
    let port = cameleer_soap::local_port(&ctx, server).unwrap();
    ctx.registry().bind(
        "filesClient",
        Value::object([
            ("address", Value::from(format!("http://127.0.0.1:{}/files", port))),
            ("serviceClass", Value::from("#files")),
            ("mtomEnabled", Value::from("true")),
            ("defaultOperationName", Value::from("upload")),
        ]),
    );
    let data: Vec<u8> = (0..5000u32).map(|i| (i % 256) as u8).collect();
    let mut ex = Exchange::new(Some(ctx.clone()));
    ex.message.body = Value::object([("name", Value::from("f.bin")), ("data", Value::Bytes(data.clone())), ("token", Value::from("T0K"))]);
    ex.set_property("CamelAttachments", Value::object([("att1@x", Value::Bytes(b"hello".to_vec()))]));
    let p = ctx.get_endpoint("cxf:bean:filesClient").unwrap().create_producer().unwrap();
    p.process(&mut ex).unwrap();
    assert_eq!(ex.message.body, Value::from("f.bin:5000:T0K:1"));
    let back = ex.property("CamelAttachments").and_then(|v| v.as_object()).cloned().unwrap();
    assert_eq!(back.get("reply"), Some(&Value::Bytes(b"pong".to_vec())));
    // the generated WSDL is served
    let resp = cameleer_soap::transport::Client::from_params(&ctx, "cxf", &mut cameleer_core::util::Parameters::default()).unwrap().send("GET", &format!("http://127.0.0.1:{}/files?wsdl", port), &[], None).unwrap();
    let w = resp.body_text();
    assert!(w.contains("<wsdl:definitions") && w.contains("name=\"upload\"") && w.contains(&format!("127.0.0.1:{}", port)), "{}", w);
    ctx.stop().unwrap();
}

#[test]
fn ws_addressing_and_reliable_messaging() {
    let ctx = ctx();
    let server = format!("cxf://http://127.0.0.1:0/calc?wsdlURL=#calcWsdl&{}&wsrmEnabled=true", svc());
    let s2 = server.clone();
    ctx.add_routes(move |b| {
        b.from(&s2).wire_tap("mock:rm").process(add);
    })
    .unwrap();
    ctx.start().unwrap();
    let port = cameleer_soap::local_port(&ctx, &server).unwrap();
    let t = ctx.create_producer_template();
    let client = format!("cxf://http://127.0.0.1:{}/calc?wsdlURL=#calcWsdl&{}&defaultOperationName=Add&wsrmEnabled=true", port, svc());
    for i in 1..=3 {
        let r = t.request_body(&client, Value::Array(vec![Value::Int(i), Value::Int(10)])).unwrap();
        assert_eq!(r, Value::Int(i + 10));
    }
    let m = ctx.mock_endpoint("mock:rm").unwrap();
    wait_for(&m, 3);
    let rx = m.received_exchanges();
    let seq = rx[0].message.headers.get("CamelCxfRmSequenceId").cloned().unwrap();
    for (i, e) in rx.iter().enumerate() {
        assert_eq!(e.message.headers.get("CamelCxfRmSequenceId"), Some(&seq));
        assert_eq!(e.message.headers.get("CamelCxfRmMessageNumber"), Some(&Value::Int(i as i64 + 1)));
        assert!(e.message.headers.get("CamelCxfWsaMessageId").is_some());
        assert_eq!(e.message.headers.get("CamelCxfWsaAction"), Some(&Value::from("http://example.org/calculator/Add")));
    }
    // a duplicate message number is answered from the cache, not processed again
    let env = format!(
        "<soap:Envelope xmlns:soap=\"http://schemas.xmlsoap.org/soap/envelope/\"><soap:Header>{}</soap:Header><soap:Body><c:Add xmlns:c=\"{}\"><c:a>99</c:a><c:b>1</c:b></c:Add></soap:Body></soap:Envelope>",
        cameleer_soap::wsa::sequence_header(&seq.to_string(), 2),
        CALC_NS
    );
    let raw = format!("cxf://http://127.0.0.1:{}/calc?dataFormat=RAW", port);
    let r = t.request_body(&raw, env.clone()).unwrap().to_string();
    assert!(r.contains(">12<"), "{}", r);
    assert!(r.contains("SequenceAcknowledgement"), "{}", r);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(m.received_counter(), 3);
    // unknown sequence -> fault
    let env2 = env.replace(&seq.to_string(), "urn:uuid:nope");
    let r = t.request_body(&raw, env2).unwrap().to_string();
    assert!(r.contains("not a known Sequence"), "{}", r);
    ctx.stop().unwrap();
}

#[test]
fn saml_bst_signature_and_encryption() {
    let ctx = ctx();
    let pki = cameleer_tls::testing::pki();
    let rsa_pem = include_str!("fixtures/rsa2048.pem");
    let rsa = cameleer_tls::keys::PrivateKey::from_pem(rsa_pem.as_bytes()).unwrap();
    let rsa_pub = cameleer_tls::pem::encode_pem("PUBLIC KEY", &rsa.public_key().to_spki());
    ctx.registry().bind("signCert", pki.localhost.cert_pem.clone());
    ctx.registry().bind("signKey", pki.localhost.key_pem.clone());
    ctx.registry().bind("otherCert", pki.unrelated.cert_pem.clone());
    ctx.registry().bind("encPub", rsa_pub);
    ctx.registry().bind("encKey", rsa_pem.to_string());
    let server = format!(
        "cxf://http://127.0.0.1:0/secure?wsdlURL=#calcWsdl&{}&wsSecuritySamlRequired=true&wsSecuritySamlTrustedIssuers=urn:idp&wsSecuritySignatureTrustedCertificates=#signCert&wsSecurityDecryptionKey=#encKey",
        svc()
    );
    let s2 = server.clone();
    ctx.add_routes(move |b| {
        b.from(&s2).wire_tap("mock:sec").process(add);
    })
    .unwrap();
    ctx.start().unwrap();
    let port = cameleer_soap::local_port(&ctx, &server).unwrap();
    let t = ctx.create_producer_template();
    let base = format!("cxf://http://127.0.0.1:{}/secure?wsdlURL=#calcWsdl&{}&defaultOperationName=Add", port, svc());
    for alg in ["aes128-cbc", "aes256-gcm"] {
        let client = format!(
            "{}&wsSecuritySamlSubject=alice&wsSecuritySamlIssuer=urn:idp&wsSecuritySamlAttributes=role=admin&wsSecuritySignatureKey=#signKey&wsSecuritySignatureCertificate=#signCert&wsSecurityEncryptionKey=#encPub&wsSecurityEncryptionAlgorithm={}",
            base, alg
        );
        let r = t.request_body(&client, Value::Array(vec![Value::Int(20), Value::Int(22)])).unwrap();
        assert_eq!(r, Value::Int(42));
    }
    let m = ctx.mock_endpoint("mock:sec").unwrap();
    wait_for(&m, 2);
    let rx = m.received_exchanges();
    assert_eq!(rx[0].message.headers.get("CamelCxfSamlSubject"), Some(&Value::from("alice")));
    assert_eq!(rx[0].message.headers.get("CamelCxfSamlAttributes").and_then(|v| v.get("role")), Some(&Value::from("admin")));
    // wrong issuer, untrusted signer, missing encryption key are rejected
    let bad_issuer = format!("{}&wsSecuritySamlSubject=bob&wsSecuritySamlIssuer=urn:evil&wsSecuritySignatureKey=#signKey&wsSecuritySignatureCertificate=#signCert", base);
    assert!(t.request_body(&bad_issuer, Value::Array(vec![Value::Int(1), Value::Int(1)])).is_err());
    let unsigned = format!("{}&wsSecuritySamlSubject=bob&wsSecuritySamlIssuer=urn:idp", base);
    assert!(t.request_body(&unsigned, Value::Array(vec![Value::Int(1), Value::Int(1)])).is_err());
    ctx.stop().unwrap();
}

const ENCODED_WSDL: &str = r#"<definitions name="Quote" targetNamespace="urn:quote" xmlns="http://schemas.xmlsoap.org/wsdl/" xmlns:soap="http://schemas.xmlsoap.org/wsdl/soap/" xmlns:tns="urn:quote" xmlns:xsd="http://www.w3.org/2001/XMLSchema">
  <message name="GetPriceRequest"><part name="symbol" type="xsd:string"/><part name="qty" type="xsd:int"/></message>
  <message name="GetPriceResponse"><part name="price" type="xsd:float"/></message>
  <portType name="QuotePort"><operation name="GetPrice"><input message="tns:GetPriceRequest"/><output message="tns:GetPriceResponse"/></operation></portType>
  <binding name="QuoteBinding" type="tns:QuotePort"><soap:binding style="rpc" transport="http://schemas.xmlsoap.org/soap/http"/>
    <operation name="GetPrice"><soap:operation soapAction="urn:quote#GetPrice"/>
      <input><soap:body use="encoded" namespace="urn:quote" encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"/></input>
      <output><soap:body use="encoded" namespace="urn:quote" encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"/></output></operation></binding>
  <service name="QuoteService"><port name="QuotePort" binding="tns:QuoteBinding"><soap:address location="http://localhost/quote"/></port></service>
</definitions>"#;

#[test]
fn rpc_encoded_with_xsi_types_and_multirefs() {
    let ctx = ctx();
    ctx.registry().bind("quoteWsdl", ENCODED_WSDL.to_string());
    // a PAYLOAD service answering with SOAP-encoding multi-refs (as Axis 1 does)
    let server = "cxf://http://127.0.0.1:0/quote?dataFormat=PAYLOAD";
    ctx.add_routes(move |b| {
        b.from(server).wire_tap("mock:enc").process(|ex: &mut Exchange| {
            ex.message.body = Value::Array(vec![
                Value::from("<ns1:GetPriceResponse xmlns:ns1=\"urn:quote\"><price href=\"#id0\"/></ns1:GetPriceResponse>"),
                Value::from("<multiRef xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" id=\"id0\" xsi:type=\"xsd:float\">12.5</multiRef>"),
            ]);
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let port = cameleer_soap::local_port(&ctx, server).unwrap();
    let t = ctx.create_producer_template();
    let client = format!("cxf://http://127.0.0.1:{}/quote?wsdlURL=#quoteWsdl&defaultOperationName=GetPrice", port);
    let r = t.request_body(&client, Value::Array(vec![Value::from("ACME"), Value::Int(3)])).unwrap();
    assert_eq!(r, Value::Float(12.5));
    let m = ctx.mock_endpoint("mock:enc").unwrap();
    wait_for(&m, 1);
    let req = m.received_exchanges()[0].message.body.to_string();
    assert!(req.contains("xsi:type=\"xsd:string\"") && req.contains("xsi:type=\"xsd:int\"") && req.contains("encodingStyle"), "{}", req);
    ctx.stop().unwrap();
}

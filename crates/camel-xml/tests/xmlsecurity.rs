//! Enveloped XML signatures (xmlsecurity-sign / xmlsecurity-verify).

use camel_core::prelude::*;
use camel_tls::keys::PrivateKey;
use camel_xml::c14n::C14N;
use camel_xml::xmlsecurity::{sign_document, verify_document};
use camel_xml::Document;

const ORDER: &str = r#"<?xml version="1.0" encoding="UTF-8"?><order xmlns="urn:orders" xmlns:x="urn:x" id="1"><item x:sku="a">2</item><note>hello &amp; bye</note></order>"#;

#[test]
fn rsa_sha256_sign_and_verify_api() {
    let key = PrivateKey::from_pem(include_bytes!("../testdata/rsa2048.pkcs8.pem")).unwrap();
    let mut doc = Document::parse(ORDER).unwrap();
    let root = doc.document_element().unwrap();
    sign_document(&mut doc, root, &key, None, camel_xml::c14n::EXC_C14N, "ds").unwrap();
    let signed = doc.to_xml_string();
    assert!(signed.contains("<ds:SignatureMethod Algorithm=\"http://www.w3.org/2001/04/xmldsig-more#rsa-sha256\"/>"), "{}", signed);
    let parsed = Document::parse(&signed).unwrap();
    verify_document(&parsed, Some(&key.public_key()), false).unwrap();
    // tampering with the content is detected
    let tampered = Document::parse(&signed.replace(">2</item>", ">3</item>")).unwrap();
    let e = verify_document(&tampered, Some(&key.public_key()), false).unwrap_err();
    assert_eq!(e.kind, "XmlSignatureInvalidContentHashException");
    // inclusive canonicalization, signature placed in a nested element
    let mut doc = Document::parse(ORDER).unwrap();
    let note = doc.element_children(doc.document_element().unwrap())[1];
    sign_document(&mut doc, note, &key, None, C14N, "").unwrap();
    let parsed = Document::parse(&doc.to_xml_string()).unwrap();
    verify_document(&parsed, Some(&key.public_key()), false).unwrap();
}

#[test]
fn ecdsa_components_roundtrip_and_failures() {
    let signer = camel_tls::self_signed("signer");
    let other = camel_tls::self_signed("other");
    let dir = std::env::temp_dir().join(format!("camel-xmlsec-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (k, c, o) = (dir.join("key.pem"), dir.join("cert.pem"), dir.join("other.pem"));
    std::fs::write(&k, &signer.key_pem).unwrap();
    std::fs::write(&c, &signer.cert_pem).unwrap();
    std::fs::write(&o, &other.cert_pem).unwrap();
    let ctx = CamelContext::new();
    camel_xml::register(&ctx);
    let (sign, verify, wrong, trust) = (
        format!("xmlsecurity-sign:s?privateKey=file:{}&certificate=file:{}&omitXmlDeclaration=true", k.display(), c.display()),
        format!("xmlsecurity-verify:v?certificate=file:{}&omitXmlDeclaration=true", c.display()),
        format!("xmlsecurity-verify:w?certificate=file:{}", o.display()),
        "xmlsecurity-verify:t?trustKeyInfoCertificate=true&omitXmlDeclaration=true".to_string(),
    );
    ctx.add_routes(move |b| {
        b.from("direct:sign").to(&sign);
        b.from("direct:verify").to(&verify);
        b.from("direct:wrong").to(&wrong);
        b.from("direct:trust").to(&trust);
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let signed = t.request_body("direct:sign", ORDER).unwrap().to_string();
    assert!(signed.contains("ecdsa-sha256") && signed.contains("<ds:X509Certificate>"), "{}", signed);
    assert!(signed.starts_with("<order"), "{}", signed);
    let verified = t.request_body("direct:verify", signed.clone()).unwrap().to_string();
    assert_eq!(verified, Document::parse(ORDER).unwrap().to_xml_string());
    assert_eq!(t.request_body("direct:trust", signed.clone()).unwrap().to_string(), verified);
    let kind = |r: Result<Value>| {
        let e = r.unwrap_err();
        let mut cur = Some(&e);
        let mut kinds = Vec::new();
        while let Some(x) = cur {
            kinds.push(x.kind.clone());
            cur = x.cause.as_deref();
        }
        kinds.join(",")
    };
    assert!(kind(t.request_body("direct:wrong", signed.clone())).contains("XmlSignatureInvalidValueException"));
    assert!(kind(t.request_body("direct:verify", signed.replace("hello", "HELLO"))).contains("XmlSignatureInvalidContentHashException"));
    let sv_start = signed.find("<ds:SignatureValue>").unwrap() + "<ds:SignatureValue>".len();
    let mut broken = signed.clone();
    let c0 = if &broken[sv_start..sv_start + 1] == "A" { "B" } else { "A" };
    broken.replace_range(sv_start..sv_start + 1, c0);
    assert!(kind(t.request_body("direct:verify", broken)).contains("XmlSignatureInvalidValueException"));
    assert!(kind(t.request_body("direct:verify", ORDER)).contains("XmlSignatureFormatException"));
    ctx.stop().unwrap();
    std::fs::remove_dir_all(&dir).ok();
    assert!(ctx.get_endpoint("xmlsecurity-sign:x").is_err(), "privateKey is required");
}

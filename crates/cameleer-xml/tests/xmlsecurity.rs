//! Enveloped XML signatures (xmlsecurity-sign / xmlsecurity-verify).

use cameleer_core::prelude::*;
use cameleer_tls::keys::PrivateKey;
use cameleer_xml::c14n::C14N;
use cameleer_xml::xmlsecurity::{sign_document, verify_document};
use cameleer_xml::Document;

const ORDER: &str = r#"<?xml version="1.0" encoding="UTF-8"?><order xmlns="urn:orders" xmlns:x="urn:x" id="1"><item x:sku="a">2</item><note>hello &amp; bye</note></order>"#;

#[test]
fn rsa_sha256_sign_and_verify_api() {
    let key = PrivateKey::from_pem(include_bytes!("../testdata/rsa2048.pkcs8.pem")).unwrap();
    let mut doc = Document::parse(ORDER).unwrap();
    let root = doc.document_element().unwrap();
    sign_document(&mut doc, root, &key, None, cameleer_xml::c14n::EXC_C14N, "ds").unwrap();
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
    let signer = cameleer_tls::self_signed("signer");
    let other = cameleer_tls::self_signed("other");
    let dir = std::env::temp_dir().join(format!("camel-xmlsec-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (k, c, o) = (dir.join("key.pem"), dir.join("cert.pem"), dir.join("other.pem"));
    std::fs::write(&k, &signer.key_pem).unwrap();
    std::fs::write(&c, &signer.cert_pem).unwrap();
    std::fs::write(&o, &other.cert_pem).unwrap();
    let ctx = CamelContext::new();
    cameleer_xml::register(&ctx);
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

#[test]
fn enveloping_detached_xpath_keyvalue_and_keystores() {
    use cameleer_tls::keystore::KeyStore;
    use cameleer_tls::x509::Certificate;
    let signer = cameleer_tls::self_signed("signer");
    let other = cameleer_tls::self_signed("other");
    let key = PrivateKey::from_pem(signer.key_pem.as_bytes()).unwrap();
    let cert = Certificate::from_pem_all(signer.cert_pem.as_bytes()).unwrap().remove(0);
    let dir = std::env::temp_dir().join(format!("camel-xmlsec2-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p12 = dir.join("signer.p12");
    std::fs::write(&p12, KeyStore::from_key_entry("signer", key.clone(), vec![cert.der.clone()]).to_pkcs12("secret")).unwrap();
    let ctx = CamelContext::new();
    cameleer_xml::register(&ctx);
    // a trust store bean with only the signer's certificate
    ctx.registry().bind("trust", KeyStore::new().add_cert("signer", cert.der.clone()));
    let other_cert = Certificate::from_pem_all(other.cert_pem.as_bytes()).unwrap().remove(0);
    ctx.registry().bind("otherTrust", KeyStore::new().add_cert("other", other_cert.der.clone()));
    let ks = format!("keyStore=file:{}&keyStorePassword=secret&alias=signer", p12.display());
    let routes = vec![
        ("direct:enveloping", format!("xmlsecurity-sign:a?{}&contentObjectId=obj1&omitXmlDeclaration=true", ks)),
        ("direct:detached", format!("xmlsecurity-sign:b?{}&xpathsToIdAttributes=//@Id&omitXmlDeclaration=true", ks)),
        ("direct:xpath", format!("xmlsecurity-sign:c?{}&parentLocalName=order&xpathFilter=not(ancestor-or-self::o:note)&xpathNamespaces=o=urn:orders&omitXmlDeclaration=true", ks)),
        ("direct:xpath2", format!("xmlsecurity-sign:d?{}&xpath2Filter=intersect://o:item;subtract://o:item[2]&xpathNamespaces=o=urn:orders&omitXmlDeclaration=true", ks)),
        ("direct:keyvalue", format!("xmlsecurity-sign:e?privateKey=file:{}&addKeyValue=true&keyName=signer&omitXmlDeclaration=true", { let k = dir.join("k.pem"); std::fs::write(&k, &signer.key_pem).unwrap(); k.display().to_string() })),
        ("direct:verifyTrust", "xmlsecurity-verify:v?keyStore=#trust&omitXmlDeclaration=true".to_string()),
        ("direct:verifyOther", "xmlsecurity-verify:w?keyStore=#otherTrust".to_string()),
        ("direct:verifyKeyInfo", "xmlsecurity-verify:x?trustKeyInfoCertificate=true&omitXmlDeclaration=true".to_string()),
    ];
    let r2 = routes.clone();
    ctx.add_routes(move |b| {
        for (from, to) in &r2 {
            b.from(*from).to(to.as_str());
        }
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let doc = r#"<order xmlns="urn:orders"><item Id="i1">a</item><item Id="i2">b</item><note>free text</note></order>"#;

    // enveloping: the signature is the document element and wraps the content in ds:Object
    let s = t.request_body("direct:enveloping", doc).unwrap().to_string();
    assert!(s.starts_with("<ds:Signature"), "{}", s);
    assert!(s.contains("<ds:Reference URI=\"#obj1\">") && s.contains("<ds:Object Id=\"obj1\"><order xmlns=\"urn:orders\">"), "{}", s);
    assert_eq!(t.request_body("direct:verifyTrust", s.clone()).unwrap().to_string(), doc);
    let tampered = s.replace(">a</item>", ">z</item>");
    assert!(format!("{:?}", t.request_body("direct:verifyTrust", tampered).unwrap_err()).contains("XmlSignatureInvalidContentHashException"));
    assert!(format!("{:?}", t.request_body("direct:verifyOther", s).unwrap_err()).contains("XmlSignatureInvalidKeyException"));

    // detached: one reference per Id, signature after the last signed element
    let s = t.request_body("direct:detached", doc).unwrap().to_string();
    assert!(s.contains("<ds:Reference URI=\"#i1\">") && s.contains("<ds:Reference URI=\"#i2\">"), "{}", s);
    assert!(s.contains("<item Id=\"i2\">b</item><ds:Signature"), "{}", s);
    assert_eq!(t.request_body("direct:verifyTrust", s.clone()).unwrap().to_string(), doc);
    // the unsigned note may change, the signed items may not
    assert!(t.request_body("direct:verifyTrust", s.replace("free text", "other")).is_ok());
    assert!(t.request_body("direct:verifyTrust", s.replace(">b</item>", ">c</item>")).is_err());

    // XPath filter: the note is excluded from the digest
    let s = t.request_body("direct:xpath", doc).unwrap().to_string();
    assert!(s.contains(&format!("Algorithm=\"{}\"", cameleer_xml::xmlsecurity::XPATH_FILTER)), "{}", s);
    assert!(t.request_body("direct:verifyKeyInfo", s.replace("free text", "changed")).is_ok());
    assert!(t.request_body("direct:verifyKeyInfo", s.replace(">a</item>", ">x</item>")).is_err());

    // XPath Filter 2.0: only the first item is covered
    let s = t.request_body("direct:xpath2", doc).unwrap().to_string();
    assert!(t.request_body("direct:verifyKeyInfo", s.replace(">b</item>", ">B</item>")).is_ok());
    assert!(t.request_body("direct:verifyKeyInfo", s.replace(">a</item>", ">A</item>")).is_err());

    // KeyValue + KeyName (no certificate): verified with the key value
    let s = t.request_body("direct:keyvalue", doc).unwrap().to_string();
    assert!(s.contains("<ds:KeyName>signer</ds:KeyName><ds:KeyValue><dsig11:ECKeyValue") || s.contains("<ds:RSAKeyValue>"), "{}", s);
    assert_eq!(t.request_body("direct:verifyKeyInfo", s.clone()).unwrap().to_string(), doc);
    // and with the trust store (KeyName = alias)
    assert_eq!(t.request_body("direct:verifyTrust", s).unwrap().to_string(), doc);
    ctx.stop().unwrap();
    std::fs::remove_dir_all(&dir).ok();
}

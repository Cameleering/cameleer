//! The jaxb data format.

use std::collections::BTreeMap;
use std::path::PathBuf;

use cameleer_core::dataformat::{create_data_format_with, DataFormatDefinition};
use cameleer_core::prelude::*;

fn ctx() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    cameleer_soap::register(&ctx);
    ctx
}

const ORDER_XSD: &str = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:shop" xmlns:s="urn:shop" elementFormDefault="qualified">
  <xs:element name="order">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="id" type="xs:long"/>
        <xs:element name="date" type="xs:date"/>
        <xs:element name="paid" type="xs:boolean"/>
        <xs:element name="total" type="xs:decimal"/>
        <xs:element name="item" type="s:Item" maxOccurs="unbounded"/>
        <xs:element name="tags" minOccurs="0"><xs:simpleType><xs:list itemType="xs:string"/></xs:simpleType></xs:element>
      </xs:sequence>
      <xs:attribute name="channel" type="xs:string"/>
      <xs:attribute name="priority" type="xs:int"/>
    </xs:complexType>
  </xs:element>
  <xs:complexType name="Item">
    <xs:sequence>
      <xs:element name="name" type="xs:string"/>
      <xs:element name="qty" type="xs:positiveInteger"/>
    </xs:sequence>
    <xs:attribute name="sku" type="xs:string" use="required"/>
  </xs:complexType>
</xs:schema>"#;

const ORDER_XML: &str = r#"<s:order xmlns:s="urn:shop" channel="web" priority="2"><s:id>1001</s:id><s:date>2024-05-01</s:date><s:paid>true</s:paid><s:total>19.95</s:total><s:item sku="A-1"><s:name>Pen</s:name><s:qty>3</s:qty></s:item><s:tags>gift urgent</s:tags></s:order>"#;

fn write_tmp(name: &str, text: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cameleer-soap-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, text).unwrap();
    p
}

fn jaxb(ctx: &CamelContext, opts: &[(&str, String)]) -> std::sync::Arc<dyn cameleer_core::dataformat::DataFormat> {
    let o: BTreeMap<String, String> = opts.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    create_data_format_with(ctx.registry(), "jaxb", &o).unwrap()
}

#[test]
fn jaxb_schema_typed_round_trip() {
    let ctx = ctx();
    let xsd = write_tmp("order.xsd", ORDER_XSD).display().to_string();
    let df = jaxb(&ctx, &[("contextPath", xsd.clone()), ("schema", xsd.clone()), ("prettyPrint", "false".into()), ("fragment", "true".into()), ("namespacePrefix", "urn:shop=s".into())]);
    let mut ex = Exchange::with_body(Some(ctx.clone()), ORDER_XML);
    df.unmarshal(&mut ex).unwrap();
    let v = ex.message.body.clone();
    assert_eq!(v.get("id"), Some(&Value::Int(1001)));
    assert_eq!(v.get("date"), Some(&Value::from("2024-05-01")));
    assert_eq!(v.get("paid"), Some(&Value::Bool(true)));
    assert_eq!(v.get("total"), Some(&Value::Float(19.95)));
    assert_eq!(v.get("@priority"), Some(&Value::Int(2)));
    let items = v.get("item").unwrap().as_array().unwrap();
    assert_eq!(items.len(), 1, "maxOccurs=unbounded is always an array");
    assert_eq!(items[0].get("qty"), Some(&Value::Int(3)));
    assert_eq!(items[0].get("@sku"), Some(&Value::from("A-1")));
    assert_eq!(v.get("tags"), Some(&Value::Array(vec![Value::from("gift"), Value::from("urgent")])));
    // marshal back (root = the schema's single global element)
    df.marshal(&mut ex).unwrap();
    assert_eq!(ex.message.body.to_string(), ORDER_XML);
    assert_eq!(ex.message.headers.get("Content-Type"), Some(&Value::from("application/xml")));

    // validation errors
    let mut bad = Exchange::with_body(Some(ctx.clone()), ORDER_XML.replace("<s:qty>3</s:qty>", "<s:qty>0</s:qty>"));
    let e = df.unmarshal(&mut bad).unwrap_err();
    assert_eq!(e.kind, "UnmarshalException");
    assert!(e.is_a("JAXBException") && e.is_a("IOException"));
    assert!(e.message.contains("cvc-"), "{}", e.message);
    let mut missing = Exchange::with_body(Some(ctx.clone()), Value::object([("id", Value::Int(1))]));
    let e = df.marshal(&mut missing).unwrap_err();
    assert_eq!(e.kind, "MarshalException");
    let mut malformed = Exchange::with_body(Some(ctx.clone()), "<s:order");
    assert_eq!(df.unmarshal(&mut malformed).unwrap_err().kind, "UnmarshalException");
}

#[test]
fn jaxb_options_and_structural_fallback() {
    let ctx = ctx();
    let xsd = write_tmp("order2.xsd", ORDER_XSD);
    let desc = write_tmp("jaxb.properties", "# shop model\nschema=order2.xsd\nprefix.shop=urn:shop\n").display().to_string();
    // descriptor, plain attribute keys, wrapped root, pretty print with declaration, ISO-8859-1
    let df = jaxb(&ctx, &[("contextPath", desc), ("plainAttributes", "true".into()), ("ignoreJAXBElement", "false".into()), ("encoding", "ISO-8859-1".into())]);
    let mut ex = Exchange::with_body(Some(ctx.clone()), ORDER_XML.replace("Pen", "Stylo é"));
    df.unmarshal(&mut ex).unwrap();
    let order = ex.message.body.get("order").unwrap().clone();
    assert_eq!(order.get("channel"), Some(&Value::from("web")));
    assert_eq!(order.get("priority"), Some(&Value::Int(2)));
    df.marshal(&mut ex).unwrap();
    let Value::Bytes(b) = &ex.message.body else { panic!("bytes expected") };
    let text: String = b.iter().map(|&c| c as char).collect();
    assert!(text.starts_with("<?xml version=\"1.0\" encoding=\"ISO-8859-1\""), "{}", text);
    assert!(text.contains("<shop:order xmlns:shop=\"urn:shop\""), "{}", text);
    assert!(text.contains("\n  <shop:id>1001</shop:id>"), "{}", text);
    assert!(b.contains(&0xE9), "é encoded as one byte");
    let _ = xsd;

    // no schema: structural mapping, partClass for the root
    let df = jaxb(&ctx, &[("partClass", "{urn:x}note".into()), ("prettyPrint", "false".into()), ("fragment", "true".into())]);
    let mut ex = Exchange::with_body(Some(ctx.clone()), "<n:note xmlns:n=\"urn:x\" lang=\"en\"><n:to>Tove</n:to><n:to>Jani</n:to><n:body>Hi</n:body></n:note>");
    df.unmarshal(&mut ex).unwrap();
    assert_eq!(ex.message.body.to_json(), r#"{"@lang":"en","to":["Tove","Jani"],"body":"Hi"}"#);
    df.marshal(&mut ex).unwrap();
    assert_eq!(ex.message.body.to_string(), "<ns1:note xmlns:ns1=\"urn:x\" lang=\"en\"><ns1:to>Tove</ns1:to><ns1:to>Jani</ns1:to><ns1:body>Hi</ns1:body></ns1:note>");
}

#[test]
fn jaxb_schema_beans_and_typed_dates() {
    let ctx = ctx();
    // a String bean for typing and a compiled validator bean
    ctx.registry().bind("orderXsd", ORDER_XSD.to_string());
    ctx.registry().bind("orderValidator", cameleer_xml::xsd::Schema::parse(ORDER_XSD, None).unwrap());
    let df = jaxb(&ctx, &[("contextPath", "#orderXsd".into()), ("schema", "#orderValidator".into()), ("typedDates", "true".into()), ("prettyPrint", "false".into())]);
    let mut ex = Exchange::with_body(Some(ctx.clone()), ORDER_XML);
    df.unmarshal(&mut ex).unwrap();
    assert_eq!(ex.message.body.get("date").unwrap().to_json(), r#"{"year":2024,"month":5,"day":1}"#);
    assert_eq!(ex.message.body.get("id"), Some(&Value::Int(1001)));
    df.marshal(&mut ex).unwrap();
    assert!(ex.message.body.to_string().contains("<ns1:date>2024-05-01</ns1:date>") || ex.message.body.to_string().contains(":date>2024-05-01<"), "{}", ex.message.body);
    // a SchemaSet bean
    ctx.registry().bind("orderModel", cameleer_soap::xsdmodel::SchemaSet::parse(ORDER_XSD, None).unwrap());
    let df = jaxb(&ctx, &[("contextPath", "ref:orderModel".into())]);
    let mut ex = Exchange::with_body(Some(ctx.clone()), ORDER_XML);
    df.unmarshal(&mut ex).unwrap();
    assert_eq!(ex.message.body.get("paid"), Some(&Value::Bool(true)));
    // validation through the bean validator
    let df = jaxb(&ctx, &[("schema", "#orderValidator".into())]);
    let mut bad = Exchange::with_body(Some(ctx.clone()), ORDER_XML.replace("<s:qty>3</s:qty>", "<s:qty>0</s:qty>"));
    assert_eq!(df.unmarshal(&mut bad).unwrap_err().kind, "UnmarshalException");
}

#[test]
fn jaxb_in_a_route() {
    let ctx = ctx();
    let xsd = write_tmp("order3.xsd", ORDER_XSD).display().to_string();
    let x2 = xsd.clone();
    ctx.add_routes(move |b| {
        b.from("direct:in").unmarshal(DataFormatDefinition::new("jaxb").option("contextPath", &x2)).set_body(simple("${body[item][0][name]} x ${body[item][0][qty]}"));
    })
    .unwrap();
    ctx.start().unwrap();
    let r = ctx.create_producer_template().request_body("direct:in", ORDER_XML).unwrap();
    assert_eq!(r.to_string(), "Pen x 3");
    ctx.stop().unwrap();
}

// ---------------------------------------------------------------------------- FHIR

//! xslt, xslt-saxon, validator and xj components in routes.

use std::path::PathBuf;

use cameleer_core::prelude::*;

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("cameleer-xml-{}-{}", tag, std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn ctx() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_xml::register(&ctx);
    ctx
}

#[test]
fn xslt_component_with_header_parameters_and_outputs() {
    let d = dir("xslt");
    let xsl = d.join("hello.xsl");
    std::fs::write(
        &xsl,
        r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
             <xsl:param name="greeting" select="'Hello'"/><xsl:param name="suffix"/>
             <xsl:template match="/person"><greeting><xsl:value-of select="concat($greeting, ' ', name, $suffix)"/></greeting></xsl:template>
           </xsl:stylesheet>"#,
    )
    .unwrap();
    let out_file = d.join("out").join("result.xml");
    let ctx = ctx();
    let uri = format!("xslt:file:{}", xsl.display());
    let uri2 = uri.clone();
    let uri3 = uri.clone();
    let of = out_file.display().to_string();
    ctx.add_routes(move |b| {
        b.from("direct:a").to(&uri).to("mock:a");
        b.from("direct:bytes").to(&format!("{}?output=bytes&contentCache=false&transformerFactoryClass=ignored", uri2)).to("mock:bytes");
        b.from("direct:file").set_header("CamelXsltFileName", constant(of.clone())).to(&format!("{}?output=file", uri3)).to("mock:file");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let r = t.request_body_and_header("direct:a", "<person><name>Camel</name></person>", "suffix", "!").unwrap();
    assert_eq!(r, Value::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?><greeting>Hello Camel!</greeting>"));
    let r = t.request_body_and_headers("direct:a", "<person><name>Rust</name></person>", vec![("greeting", Value::from("Hi")), ("suffix", Value::from("?"))]).unwrap();
    assert_eq!(r, Value::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?><greeting>Hi Rust?</greeting>"));
    let r = t.request_body("direct:bytes", "<person><name>B</name></person>".as_bytes().to_vec()).unwrap();
    assert!(matches!(r, Value::Bytes(_)));
    let r = t.request_body("direct:file", "<person><name>F</name></person>").unwrap();
    assert_eq!(r, Value::from(out_file.display().to_string()));
    assert!(std::fs::read_to_string(&out_file).unwrap().contains("Hello F"));
    // invalid input
    let e = t.request_body("direct:a", "<person>").unwrap_err();
    assert!(format!("{:?}", e).contains("SAXParseException"), "{:?}", e);
    ctx.stop().unwrap();
    std::fs::remove_dir_all(&d).ok();
}

#[test]
fn xslt_saxon_rejects_xslt2_and_template_from_header() {
    let d = dir("saxon");
    let xsl2 = d.join("v2.xsl");
    std::fs::write(&xsl2, r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform"><xsl:template match="/"><xsl:iterate select="*"/></xsl:template></xsl:stylesheet>"#).unwrap();
    let ctx = ctx();
    let u = format!("xslt-saxon:file:{}?saxonExtensionFunctions=#x", xsl2.display());
    ctx.add_routes(move |b| {
        b.from("direct:v2").to(&u);
        b.from("direct:hdr").to("xslt:ref:unused?allowTemplateFromHeader=true");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let e = t.request_body("direct:v2", "<a/>").unwrap_err();
    let msg = format!("{:?}", e);
    assert!(msg.contains("xsl:iterate is not supported"), "{}", msg);
    let r = t
        .request_body_and_header(
            "direct:hdr",
            "<a>x</a>",
            "CamelXsltStylesheet",
            r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform"><xsl:output method="text"/><xsl:template match="/">[<xsl:value-of select="a"/>]</xsl:template></xsl:stylesheet>"#,
        )
        .unwrap();
    assert_eq!(r, Value::from("[x]"));
    ctx.stop().unwrap();
    std::fs::remove_dir_all(&d).ok();
}

#[test]
fn validator_component_with_schema() {
    let d = dir("validator");
    let xsd = d.join("person.xsd");
    std::fs::write(
        &xsd,
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="person"><xs:complexType><xs:sequence><xs:element name="name" type="xs:string"/><xs:element name="age" type="xs:int"/></xs:sequence></xs:complexType></xs:element></xs:schema>"#,
    )
    .unwrap();
    let ctx = ctx();
    let uri = format!("validator:file:{}", xsd.display());
    ctx.add_routes(move |b| {
        b.from("direct:v")
            .do_try()
            .to(&uri)
            .set_body(constant("valid"))
            .do_catch(&["ValidationException"])
            .set_body(simple("invalid: ${exception.message}"))
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    assert_eq!(t.request_body("direct:v", "<person><name>A</name><age>3</age></person>").unwrap(), Value::from("valid"));
    let r = t.request_body("direct:v", "<person><name>A</name><age>old</age></person>").unwrap().to_string();
    assert!(r.starts_with("invalid: Validation failed for: file:"), "{}", r);
    assert!(r.contains("cvc-datatype-valid.1.2.1: 'old' is not a valid value for 'int'."), "{}", r);
    let r = t.request_body("direct:v", "<person><age>3</age></person>").unwrap().to_string();
    assert!(r.contains("cvc-complex-type.2.4.a: Invalid content was found starting with element 'age'. One of '{name}' is expected."), "{}", r);
    let r = t.request_body("direct:v", "<person><name>A</name>").unwrap().to_string();
    assert!(r.starts_with("invalid: Validation failed"), "{}", r);
    let e = t.request_body("direct:v", Value::Null).unwrap();
    assert!(e.to_string().starts_with("invalid: No body available"), "{}", e);
    ctx.stop().unwrap();
    std::fs::remove_dir_all(&d).ok();
}

#[test]
fn xj_both_directions() {
    let d = dir("xj");
    let ctx = ctx();
    let xsl = d.join("toxj.xsl");
    std::fs::write(
        &xsl,
        r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform" xmlns:xj="http://camel.apache.org/component/xj">
             <xsl:template match="/order"><object xj:type="object"><object xj:name="id" xj:type="int"><xsl:value-of select="@id"/></object><object xj:name="items" xj:type="array"><xsl:for-each select="item"><object xj:type="string"><xsl:value-of select="."/></object></xsl:for-each></object></object></xsl:template>
           </xsl:stylesheet>"#,
    )
    .unwrap();
    let xsl2 = d.join("fromxj.xsl");
    std::fs::write(
        &xsl2,
        r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform" xmlns:xj="http://camel.apache.org/component/xj" exclude-result-prefixes="xj">
             <xsl:output omit-xml-declaration="yes"/>
             <xsl:template match="/object"><person name="{object[@xj:name='name']}"><xsl:for-each select="object[@xj:name='tags']/object"><tag><xsl:value-of select="."/></tag></xsl:for-each></person></xsl:template>
           </xsl:stylesheet>"#,
    )
    .unwrap();
    let (u1, u2) = (format!("xj:file:{}?transformDirection=XML2JSON", xsl.display()), format!("xj:file:{}?transformDirection=JSON2XML", xsl2.display()));
    ctx.add_routes(move |b| {
        b.from("direct:x2j").to("xj:identity?transformDirection=XML2JSON");
        b.from("direct:j2x").to("xj:identity?transformDirection=JSON2XML");
        b.from("direct:x2jxsl").to(&u1);
        b.from("direct:j2xxsl").to(&u2);
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let r = t
        .request_body(
            "direct:x2j",
            "<root><firstname>camel</firstname><lastname>apache</lastname><roles><entry>admin</entry><entry>committer</entry></roles><state ok='yes'><needsWater>true</needsWater></state><n>1</n><n>2</n></root>",
        )
        .unwrap();
    assert_eq!(
        r,
        Value::from(r#"{"firstname":"camel","lastname":"apache","roles":["admin","committer"],"state":{"@ok":"yes","needsWater":"true"},"n":["1","2"]}"#)
    );
    let r = t.request_body("direct:j2x", r#"{"name":"camel","age":15,"ratio":0.5,"tags":["a","b"],"ok":true,"none":null,"nested":{"x":"y"}}"#).unwrap();
    assert_eq!(
        r,
        Value::from(
            r#"<?xml version="1.0" encoding="UTF-8"?><object xmlns:xj="http://camel.apache.org/component/xj" xj:type="object"><object xj:name="name" xj:type="string">camel</object><object xj:name="age" xj:type="int">15</object><object xj:name="ratio" xj:type="float">0.5</object><object xj:name="tags" xj:type="array"><object xj:type="string">a</object><object xj:type="string">b</object></object><object xj:name="ok" xj:type="boolean">true</object><object xj:name="none" xj:type="null"/><object xj:name="nested" xj:type="object"><object xj:name="x" xj:type="string">y</object></object></object>"#
        )
    );
    // round trip of the xj representation
    let back = t.request_body("direct:x2j", r.to_string()).unwrap();
    assert_eq!(back, Value::from(r#"{"name":"camel","age":15,"ratio":0.5,"tags":["a","b"],"ok":true,"none":null,"nested":{"x":"y"}}"#));
    let r = t.request_body("direct:x2jxsl", "<order id='42'><item>x</item><item>y</item></order>").unwrap();
    assert_eq!(r, Value::from(r#"{"id":42,"items":["x","y"]}"#));
    let r = t.request_body("direct:j2xxsl", r#"{"name":"Ann","tags":["t1","t2"]}"#).unwrap();
    assert_eq!(r, Value::from(r#"<person name="Ann"><tag>t1</tag><tag>t2</tag></person>"#));
    assert!(t.request_body("direct:j2x", "{bad json").is_err());
    ctx.stop().unwrap();
    std::fs::remove_dir_all(&d).ok();
}

#[test]
fn xj_requires_direction() {
    let ctx = ctx();
    assert!(ctx.get_endpoint("xj:identity").is_err());
    assert!(ctx.get_endpoint("xj:identity?transformDirection=SIDEWAYS").is_err());
}

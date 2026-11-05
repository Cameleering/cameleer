//! mustache, string-template, velocity and freemarker components.

use std::path::PathBuf;

use cameleer_core::prelude::*;

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("camel-tpl-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn mustache_renders_body_headers_and_sections() {
    let dir = temp_dir("mustache");
    std::fs::write(
        dir.join("letter.mustache"),
        "Dear {{headers.lastName}}, {{headers.firstName}}\n\
         {{#headers.vip}}\nYou are a VIP!\n{{/headers.vip}}\n\
         Items:\n{{#body.items}}\n- {{name}} x{{qty}}\n{{/body.items}}\n\
         {{^body.items}}\nno items\n{{/body.items}}\n\
         Escaped: {{headers.html}} Raw: {{{headers.html}}}\n\
         Prop: {{exchange.properties.orderId}}\n{{> footer}}",
    )
    .unwrap();
    std::fs::write(dir.join("footer.mustache"), "-- {{headers.firstName}}").unwrap();
    let uri = format!("mustache:file:{}", dir.join("letter.mustache").display());
    let ctx = CamelContext::new();
    let u = uri.clone();
    ctx.add_routes(move |b| {
        b.from("direct:letter").set_property("orderId", constant("A-1")).to(u.as_str()).to("mock:letter");
    })
    .unwrap();
    ctx.start().unwrap();
    let body = cameleer_core::json::parse(r#"{"items":[{"name":"apple","qty":2},{"name":"pear","qty":1}]}"#).unwrap();
    let t = ctx.create_producer_template();
    let out = t
        .request_body_and_headers(
            "direct:letter",
            body,
            vec![("firstName", Value::from("Ann")), ("lastName", Value::from("Lee")), ("vip", Value::from(true)), ("html", Value::from("<b>"))],
        )
        .unwrap();
    assert_eq!(
        out.to_string(),
        "Dear Lee, Ann\nYou are a VIP!\nItems:\n- apple x2\n- pear x1\nEscaped: &lt;b&gt; Raw: <b>\nProp: A-1\n-- Ann"
    );
    let out = t.request_body_and_header("direct:letter", Value::object([("items", Value::Array(vec![]))]), "firstName", "Bo").unwrap();
    assert!(out.to_string().contains("no items"), "{}", out);
    assert!(!out.to_string().contains("VIP"));
    // headers are kept
    let mock = ctx.mock_endpoint("mock:letter").unwrap();
    assert_eq!(mock.exchange(0).unwrap().header("firstName").unwrap(), &Value::from("Ann"));
    ctx.stop().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mustache_template_from_header() {
    let dir = temp_dir("mustache-hdr");
    std::fs::write(dir.join("t.mustache"), "file {{body}}").unwrap();
    std::fs::write(dir.join("other.mustache"), "other {{body}}").unwrap();
    let ctx = CamelContext::new();
    let t = ctx.create_producer_template();
    let base = format!("mustache:{}", dir.join("t.mustache").display());
    // header ignored unless allowed
    assert_eq!(t.request_body_and_header(&base, "x", "CamelMustacheTemplate", "hdr {{body}}").unwrap(), Value::from("file x"));
    let allowed = format!("{}?allowTemplateFromHeader=true", base);
    assert_eq!(t.request_body_and_header(&allowed, "x", "CamelMustacheTemplate", "hdr {{body}}").unwrap(), Value::from("hdr x"));
    let other = format!("file:{}", dir.join("other.mustache").display());
    assert_eq!(t.request_body_and_header(&allowed, "y", "CamelMustacheResourceUri", other).unwrap(), Value::from("other y"));
    // contentCache keeps the first content
    let cached = format!("{}?contentCache=true", base);
    assert_eq!(t.request_body(&cached, "1").unwrap(), Value::from("file 1"));
    std::fs::write(dir.join("t.mustache"), "changed {{body}}").unwrap();
    assert_eq!(t.request_body(&cached, "2").unwrap(), Value::from("file 2"));
    assert_eq!(t.request_body(&base, "3").unwrap(), Value::from("changed 3"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mustache_custom_delimiters_and_missing_template() {
    let dir = temp_dir("mustache-delim");
    std::fs::write(dir.join("d.mustache"), "[[body]] {{body}}").unwrap();
    let ctx = CamelContext::new();
    let t = ctx.create_producer_template();
    let uri = format!("mustache:{}?startDelimiter=[[&endDelimiter=]]", dir.join("d.mustache").display());
    assert_eq!(t.request_body(&uri, "v").unwrap(), Value::from("v {{body}}"));
    let err = t.request_body("mustache:does/not/exist.mustache", "v").unwrap_err();
    let cause = err.cause.as_deref().unwrap_or(&err);
    assert_eq!(cause.kind, "FileNotFoundException");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn string_template_velocity_freemarker() {
    let dir = temp_dir("others");
    std::fs::write(dir.join("t.st"), "Hello <headers.name>: <body>").unwrap();
    std::fs::write(dir.join("t.vm"), "Hello $headers.name: ${body}$!{headers.none}").unwrap();
    std::fs::write(dir.join("t.ftl"), "Hello ${headers.name}: ${body} ${headers.none!\"-\"}").unwrap();
    std::fs::write(dir.join("bad.vm"), "#if($body)x#end").unwrap();
    let ctx = CamelContext::new();
    let t = ctx.create_producer_template();
    let st = format!("string-template:{}", dir.join("t.st").display());
    assert_eq!(t.request_body_and_header(&st, "b", "name", "Ann").unwrap(), Value::from("Hello Ann: b"));
    let st2 = format!("string-template:{}?delimiterStart=$&delimiterStop=$", dir.join("t.st").display());
    assert_eq!(t.request_body_and_header(&st2, "b", "name", "Ann").unwrap(), Value::from("Hello <headers.name>: <body>"));
    let vm = format!("velocity:file:{}", dir.join("t.vm").display());
    assert_eq!(t.request_body_and_header(&vm, "b", "name", "Ann").unwrap(), Value::from("Hello Ann: b"));
    let ftl = format!("freemarker:{}", dir.join("t.ftl").display());
    assert_eq!(t.request_body_and_header(&ftl, "b", "name", "Ann").unwrap(), Value::from("Hello Ann: b -"));
    let bad = format!("velocity:{}", dir.join("bad.vm").display());
    assert_eq!(t.request_body(&bad, "b").unwrap(), Value::from("x"));
    std::fs::write(dir.join("broken.vm"), "#if($body)x").unwrap();
    let broken = format!("velocity:{}", dir.join("broken.vm").display());
    assert!(t.request_body(&broken, "b").is_err());
    // CamelVelocityContext replaces the data model, the supplemental context is merged in
    std::fs::write(dir.join("ctx.vm"), "$who/$extra").unwrap();
    let ctxvm = format!("velocity:{}", dir.join("ctx.vm").display());
    let mut ex = cameleer_core::message::Exchange::with_body(Some(ctx.clone()), "b");
    ex.message.set_header("CamelVelocityContext", Value::object([("who", Value::from("W")), ("extra", Value::from("0"))]));
    ex.message.set_header("CamelVelocitySupplementalContext", Value::object([("extra", Value::from("E"))]));
    let out = t.send(&ctxvm, ex);
    assert_eq!(out.message.body, Value::from("W/E"));
    std::fs::write(dir.join("dm.ftl"), "<#list items as i>${i}<#sep>,</#list>").unwrap();
    let dm = format!("freemarker:{}", dir.join("dm.ftl").display());
    let mut ex = cameleer_core::message::Exchange::with_body(Some(ctx.clone()), "b");
    ex.message.set_header("CamelFreemarkerDataModel", Value::object([("items", Value::from(vec![Value::from(1), Value::from(2)]))]));
    assert_eq!(t.send(&dm, ex).message.body, Value::from("1,2"));
    let _ = std::fs::remove_dir_all(&dir);
}

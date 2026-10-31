//! The `xquery` language and `xquery:` component in routes.

use std::path::PathBuf;

use camel_core::builder::{language, ExpressionDefinition, ValueBuilder};
use camel_core::prelude::*;

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("camel-xquery-{}-{}", tag, std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn xq(text: &str, options: &[(&str, &str)]) -> ValueBuilder {
    let mut d = ExpressionDefinition::language("xquery", text);
    for (k, v) in options {
        d = d.option(*k, *v);
    }
    ValueBuilder::new(d)
}

fn ctx() -> CamelContext {
    let ctx = CamelContext::new();
    camel_xml::register(&ctx);
    ctx
}

const ORDERS: &str = r#"<orders>
  <order id="1" customer="acme"><item sku="A" qty="2" price="10.5"/><item sku="B" qty="1" price="4"/></order>
  <order id="2" customer="globex"><item sku="C" qty="10" price="1"/></order>
</orders>"#;

#[test]
fn xquery_language_in_filter_and_set_body() {
    let ctx = ctx();
    ctx.add_routes(|b| {
        b.from("direct:filter").filter(language("xquery", "/order[@priority = 'high']")).to("mock:high").end().to("mock:all");
        b.from("direct:total").set_body(camel_xml::xquery_expr("sum(for $i in //item return $i/@qty * $i/@price)")).to("mock:total");
        b.from("direct:list")
            .set_body(xq("for $o in /orders/order order by $o/@customer descending return string($o/@customer)", &[("resultType", "List")]))
            .to("mock:list");
        b.from("direct:transform")
            .set_body(language(
                "xquery",
                "<summary count='{count(//order)}'>{ for $o in //order return <c n='{$o/@customer}'>{ sum($o/item/@qty) }</c> }</summary>",
            ))
            .to("mock:transform");
        b.from("direct:ns")
            .set_header("sku", xq("string(/o:order/o:item/@sku)", &[("namespace", "o=urn:orders"), ("resultType", "String")]))
            .to("mock:ns");
        b.from("direct:hdr")
            .set_body(xq(
                "declare variable $in.headers.min external; declare variable $threshold external; string-join(//item[@qty >= $in.headers.min][@price >= $threshold]/@sku, ',')",
                &[("resultType", "String")],
            ))
            .to("mock:hdr");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();

    let high = ctx.mock_endpoint("mock:high").unwrap();
    high.expected_message_count(1);
    t.send_body("direct:filter", "<order priority='high'/>").unwrap();
    t.send_body("direct:filter", "<order priority='low'/>").unwrap();
    high.assert_is_satisfied().unwrap();

    assert_eq!(t.request_body("direct:total", ORDERS).unwrap(), Value::from("35"));
    assert_eq!(t.request_body("direct:list", ORDERS).unwrap(), Value::from(vec!["globex", "acme"]));
    assert_eq!(t.request_body("direct:transform", ORDERS).unwrap(), Value::from("<summary count=\"2\"><c n=\"acme\">3</c><c n=\"globex\">10</c></summary>"));
    let ex = t.request("direct:ns", |ex| ex.message.body = Value::from("<order xmlns='urn:orders'><item sku='Z9'/></order>")).unwrap();
    assert_eq!(ex.message.headers.get("sku"), Some(&Value::from("Z9")));
    // external variables from a header (`in.headers.min`) and an exchange property (`threshold`)
    let r = t
        .request("direct:hdr", |ex| {
            ex.message.body = Value::from(ORDERS);
            ex.message.headers.set("min", Value::from("2"));
            ex.properties.insert("threshold".into(), Value::Int(5));
        })
        .unwrap();
    assert_eq!(r.message.body, Value::from("A"));
    // syntax errors fail at route creation
    let bad = CamelContext::new();
    camel_xml::register(&bad);
    let e = bad.add_routes(|b| {
        b.from("direct:x").set_body(language("xquery", "for $x in")).to("mock:x");
    });
    let e = match e {
        Err(e) => e,
        Ok(_) => bad.start().unwrap_err(),
    };
    assert!(e.message.contains("XPST0003"), "{:?}", e);
    ctx.stop().unwrap();
}

#[test]
fn xquery_component_with_query_file_and_results_formats() {
    let d = dir("component");
    let xq = d.join("report.xq");
    std::fs::write(
        &xq,
        r#"xquery version "1.0";
declare variable $in.headers.region external;
declare variable $in.body external;
declare function local:total($o as element(order)) as xs:decimal {
  sum(for $i in $o/item return xs:decimal($i/@qty) * xs:decimal($i/@price))
};
<report region="{$in.headers.region}" bytes="{string-length($in.body) > 0}">{
  for $o in /orders/order
  let $t := local:total($o)
  order by $t descending
  return <order id="{$o/@id}" total="{$t}"/>
}</report>"#,
    )
    .unwrap();
    std::fs::write(d.join("skus.xq"), "for $i in //item return string($i/@sku)").unwrap();
    std::fs::write(d.join("rates.xml"), "<rates><rate cur='EUR'>1.1</rate></rates>").unwrap();
    std::fs::write(d.join("lookup.xq"), "declare variable $cur external; doc('rates.xml')//rate[@cur = $cur]/text()").unwrap();
    let ctx = ctx();
    let u1 = format!("xquery:file:{}", xq.display());
    let u2 = format!("xquery:file:{}?resultsFormat=List&configuration=#ignored", d.join("skus.xq").display());
    let u3 = format!("xquery:{}?resultsFormat=String", d.join("lookup.xq").display());
    let u4 = format!("xquery:file:{}?resultsFormat=Bytes&contentCache=false", d.join("skus.xq").display());
    ctx.add_routes(move |b| {
        b.from("direct:report").to(&u1).to("mock:report");
        b.from("direct:skus").to(&u2);
        b.from("direct:lookup").to(&u3);
        b.from("direct:bytes").to(&u4);
        b.from("direct:missing").to("xquery:file:/nonexistent/q.xq");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let r = t.request_body_and_header("direct:report", ORDERS, "region", "EMEA").unwrap();
    assert_eq!(r, Value::from("<report region=\"EMEA\" bytes=\"true\"><order id=\"1\" total=\"25\"/><order id=\"2\" total=\"10\"/></report>"));
    assert_eq!(t.request_body("direct:skus", ORDERS).unwrap(), Value::from(vec!["A", "B", "C"]));
    assert_eq!(t.request_body_and_header("direct:lookup", "<x/>", "cur", "EUR").unwrap(), Value::from("1.1"));
    assert_eq!(t.request_body("direct:bytes", ORDERS).unwrap(), Value::Bytes(b"A B C".to_vec()));
    let e = t.request_body("direct:missing", "<x/>").unwrap_err();
    assert!(format!("{:?}", e).contains("FileNotFound") || format!("{:?}", e).contains("Cannot find"), "{:?}", e);
    // invalid options
    assert!(ctx.get_endpoint("xquery:file:x.xq?resultsFormat=Bogus").is_err());
    assert!(ctx.get_endpoint("xquery:file:x.xq?unknownOption=1").is_err());
    // non-XML input: evaluation error with an XQuery code
    let e = t.request_body("direct:skus", "not xml").unwrap_err();
    assert!(format!("{:?}", e).contains("XPDY0002"), "{:?}", e);
    ctx.stop().unwrap();
    std::fs::remove_dir_all(&d).ok();
}

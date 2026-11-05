//! XPath language and XML parser tests.

use cameleer_core::language::xml::{xml_to_value, Document};
use cameleer_core::language::xpath::{query, XPath};
use cameleer_core::language::{xpath, xpath_as, LanguageOptions};
use cameleer_core::prelude::*;

const ORDERS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE orders>
<orders xmlns:ns="urn:x" region="EU">
  <!-- first order -->
  <order id="1" priority="high">
    <customer>Ann</customer>
    <item sku="A" qty="2" price="10.5"/>
    <item sku="B" qty="1" price="4"/>
  </order>
  <order id="2">
    <customer>Bob &amp; Co</customer>
    <item sku="C" qty="5" price="1"/>
  </order>
  <ns:note><![CDATA[<raw> & text]]></ns:note>
</orders>"#;

fn s(v: &str) -> Value {
    Value::from(v)
}

#[test]
fn xpath_table() {
    let cases: Vec<(&str, Option<&str>, Value)> = vec![
        ("/orders/order[1]/customer/text()", None, s("Ann")),
        ("/orders/order[2]/customer", Some("String"), s("Bob & Co")),
        ("//customer/text()", None, Value::from(vec!["Ann", "Bob & Co"])),
        ("/orders/@region", None, s("EU")),
        ("/orders/order[@id='2']/item/@sku", None, s("C")),
        ("//order[@priority]/@id", None, s("1")),
        ("//order[customer='Ann']/@id", None, s("1")),
        ("//item[@qty > 1]/@sku", None, Value::from(vec!["A", "C"])),
        ("//order[last()]/@id", None, s("2")),
        ("//order[position() = 1]/customer/text()", None, s("Ann")),
        ("count(//item)", None, Value::Float(3.0)),
        ("count(//item)", Some("Integer"), Value::Int(3)),
        ("count(/orders/order[1]/item) = 2", None, Value::Bool(true)),
        ("sum(//item/@qty)", None, Value::Float(8.0)),
        ("name(/*)", None, s("orders")),
        ("name(/orders/*[last()])", None, s("ns:note")),
        ("local-name(/orders/*[last()])", None, s("note")),
        ("/orders/note/text()", None, s("<raw> & text")),
        ("string(//order[2]/@id)", None, s("2")),
        ("concat(//order[1]/customer, '-', //order[1]/@id)", None, s("Ann-1")),
        ("contains(//order[2]/customer, 'Co')", None, Value::Bool(true)),
        ("starts-with(//order[2]/customer, 'Bob')", None, Value::Bool(true)),
        ("string-length(//order[1]/customer)", None, Value::Float(3.0)),
        ("normalize-space('  a   b ')", None, s("a b")),
        ("substring('12345', 2, 3)", None, s("234")),
        ("substring-before('a-b', '-')", None, s("a")),
        ("substring-after('a-b', '-')", None, s("b")),
        ("translate('abc', 'abc', 'ABC')", None, s("ABC")),
        ("upper-case('abc')", None, s("ABC")),
        ("1 + 2 * 3", None, Value::Float(7.0)),
        ("10 div 4", None, Value::Float(2.5)),
        ("10 mod 4", None, Value::Float(2.0)),
        ("-(3)", None, Value::Float(-3.0)),
        ("//order[1]/item[1]/@price * 2", None, Value::Float(21.0)),
        ("boolean(//order[@id='3'])", None, Value::Bool(false)),
        ("not(//order[@id='3'])", None, Value::Bool(true)),
        ("//order[@id='1'] and //order[@id='2']", None, Value::Bool(true)),
        ("//order[@id='1'] or //order[@id='9']", None, Value::Bool(true)),
        ("//order/@id | //item/@sku", Some("NodeList"), Value::from(vec!["1", "A", "B", "2", "C"])),
        ("//customer/..//item/@sku", None, Value::from(vec!["A", "B", "C"])),
        ("/orders/order/item[@sku='B']/../@id", None, s("1")),
        ("//item[@sku='B']/preceding-sibling::item/@sku", None, s("A")),
        ("//item[@sku='A']/following-sibling::*/@sku", None, s("B")),
        ("//item[@sku='C']/ancestor::orders/@region", None, s("EU")),
        ("count(/orders/order[1]/descendant::*)", None, Value::Float(3.0)),
        ("count(//order/child::item)", None, Value::Float(3.0)),
        ("count(//order[1]/@*)", None, Value::Float(2.0)),
        ("/orders/order[2]/item", None, s("<item sku=\"C\" qty=\"5\" price=\"1\"/>")),
        ("//order[@id='2']/customer", None, s("<customer>Bob &amp; Co</customer>")),
        ("count(//comment())", None, Value::Float(1.0)),
        ("/orders/order[1]/customer", Some("Boolean"), Value::Bool(true)),
        ("//nothing", None, Value::Array(vec![])),
        ("//nothing", Some("String"), s("")),
        ("//nothing", Some("Node"), Value::Null),
        ("//ns:note", Some("String"), s("<raw> & text")),
    ];
    for (expr, rt, expected) in cases {
        assert_eq!(query(ORDERS, expr, rt).unwrap_or_else(|e| panic!("{}: {}", expr, e)), expected, "xpath: {}", expr);
    }
}

#[test]
fn xpath_errors_and_parser() {
    for bad in ["/orders/[", "//order[@id=", "unknownfn(1)", "", "/a/b)", "$var"] {
        let e = XPath::compile(bad).err().unwrap_or_else(|| panic!("expected error for {:?}", bad));
        assert_eq!(e.kind, kinds::EXPRESSION_ILLEGAL_SYNTAX, "{}", bad);
    }
    for bad in ["<a>", "<a></b>", "<a x='1' x='2'/>", "text", "<a>&unknown;</a>", "<a/><b/>"] {
        assert!(Document::parse(bad).is_err(), "{}", bad);
    }
    let d = Document::parse("<a>&#65;&#x42;&lt;&quot;</a>").unwrap();
    assert_eq!(d.text_content(d.root_element().unwrap()), "AB<\"");
}

#[test]
fn xml_value_mapping() {
    let v = xml_to_value(ORDERS).unwrap();
    let orders = v.get("order").unwrap().as_array().unwrap();
    assert_eq!(orders.len(), 2);
    assert_eq!(orders[0].get("id"), Some(&s("1")));
    assert_eq!(orders[1].get("customer"), Some(&s("Bob & Co")));
    assert_eq!(orders[0].get("item").unwrap().as_array().unwrap()[1].get("price"), Some(&s("4")));
    assert_eq!(v.get("note"), Some(&s("<raw> & text")));
    assert_eq!(v.get("region"), Some(&s("EU")));
}

#[test]
fn xpath_in_routes() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:x")
            .set_header("region", xpath_as("/orders/@region", "String"))
            .set_header("items", xpath("count(//item)").with_option("resultType", "Integer"))
            .set_body(xpath("//order[@id='1']/customer/text()"))
            .to("mock:x");
        b.from("direct:hdr").set_body(xpath("/a/text()").with_option("headerName", "xml")).to("mock:hdr");
    })
    .unwrap();
    ctx.start().unwrap();
    let m = ctx.mock_endpoint("mock:x").unwrap();
    m.expected_bodies_received(vec!["Ann"]);
    m.expected_header_received("region", "EU");
    m.expected_header_received("items", 3);
    let t = ctx.create_producer_template();
    t.send_body("direct:x", ORDERS).unwrap();
    m.assert_is_satisfied().unwrap();
    let h = ctx.mock_endpoint("mock:hdr").unwrap();
    h.expected_bodies_received(vec!["inner"]);
    t.send_body_and_header("direct:hdr", "body", "xml", "<a>inner</a>").unwrap();
    h.assert_is_satisfied().unwrap();
    // invalid XML fails the exchange
    assert!(t.send_body("direct:x", "<broken").is_err());
    // predicates
    let p = xpath("//order[@priority='high']").def.resolve_predicate(&ctx).unwrap();
    let mut ex = ctx.create_exchange();
    ex.set_body(ORDERS);
    assert!(p.matches(&ex).unwrap());
    let p = xpath("count(//order) > 5").def.resolve_predicate(&ctx).unwrap();
    assert!(!p.matches(&ex).unwrap());
    ctx.stop().unwrap();
}

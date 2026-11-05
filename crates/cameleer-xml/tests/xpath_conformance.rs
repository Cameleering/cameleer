//! XPath 1.0 conformance table.

use cameleer_core::prelude::*;
use cameleer_xml::dom::Document;
use cameleer_xml::xpath::{Docs, NoEnv, XPath, XValue};

const DOC: &str = r#"<?xml version="1.0"?>
<!DOCTYPE library [ <!ATTLIST book code ID #IMPLIED> ]>
<library xmlns:x="urn:extra" xml:lang="en">
  <!-- catalogue -->
  <?render fast?>
  <book code="b1" year="1999" price="10.5">
    <title>Alpha</title>
    <author>Ann</author>
    <author>Bob</author>
  </book>
  <book code="b2" year="2005" price="20">
    <title xml:lang="de-AT">Beta</title>
    <author>Cid</author>
    <x:note>extra note</x:note>
  </book>
  <book code="b3" year="2010" price="4.5">
    <title>Gamma</title>
    <author>Ann</author>
  </book>
  <magazine><title>Delta</title></magazine>
</library>"#;

fn ns() -> Vec<(String, String)> {
    vec![("e".to_string(), "urn:extra".to_string())]
}

fn eval(expr: &str) -> String {
    let doc = Document::parse(DOC).unwrap();
    let docs = Docs::new(doc);
    let xp = XPath::compile_ns(expr, &ns()).unwrap_or_else(|e| panic!("compile {}: {}", expr, e));
    let v = xp.eval_docs(&docs, &NoEnv).unwrap_or_else(|e| panic!("eval {}: {}", expr, e));
    match v {
        XValue::Nodes(ns) => format!("[{}]", ns.iter().map(|n| docs.string_value(*n).trim().to_string()).collect::<Vec<_>>().join("|")),
        XValue::Num(_) | XValue::Str(_) | XValue::Bool(_) => v.to_string_value(&docs),
    }
}

#[test]
fn conformance_table() {
    let cases: &[(&str, &str)] = &[
        // location paths and abbreviations
        ("count(/library/book)", "3"),
        ("/library/book[1]/title", "[Alpha]"),
        ("/library/book[last()]/title", "[Gamma]"),
        ("//title[. = 'Beta']/../@code", "[b2]"),
        ("count(//author)", "4"),
        ("//book[author='Ann']/title", "[Alpha|Gamma]"),
        ("//book[@year > 2000][@price < 10]/title", "[Gamma]"),
        ("//book[2]/author[1]", "[Cid]"),
        ("(//author)[2]", "[Bob]"),
        ("(//author)[last()]", "[Ann]"),
        ("//book/author[1]", "[Ann|Cid|Ann]"),
        ("count(//book[author][title])", "3"),
        ("name(/*)", "library"),
        ("count(/library/*)", "4"),
        ("count(//*)", "14"),
        ("count(//node())", "43"),
        ("count(//text()[normalize-space()])", "9"),
        ("//comment()", "[catalogue]"),
        ("//processing-instruction('render')", "[fast]"),
        ("count(//processing-instruction())", "1"),
        ("count(//book/@*)", "9"),
        ("//book[@code='b3']/@year", "[2010]"),
        (".//magazine/title", "[Delta]"),
        // axes
        ("//title[.='Beta']/parent::book/@code", "[b2]"),
        ("count(//title[.='Gamma']/ancestor::*)", "2"),
        ("count(//title[.='Gamma']/ancestor-or-self::*)", "3"),
        ("//book[1]/following-sibling::book[1]/title", "[Beta]"),
        ("//book[3]/preceding-sibling::book[1]/title", "[Beta]"),
        ("//book[3]/preceding-sibling::*[last()]/title", "[Alpha]"),
        ("//title[.='Beta']/following::title", "[Gamma|Delta]"),
        ("//title[.='Beta']/preceding::title", "[Alpha]"),
        ("count(//title[.='Beta']/preceding::author)", "2"),
        ("//author[.='Cid']/preceding::*[1]", "[Beta]"),
        ("//book[2]/descendant::*[2]", "[Cid]"),
        ("count(//book[2]/descendant-or-self::*)", "4"),
        ("//book[1]/self::book/@code", "[b1]"),
        ("count(//book[1]/self::magazine)", "0"),
        ("//book[1]/attribute::code", "[b1]"),
        ("//book[1]/child::title", "[Alpha]"),
        ("//book[@code='b1']/@year/following::title[1]", "[Alpha]"),
        ("count(/library/namespace::*)", "2"),
        ("name(/library/namespace::*[. = 'urn:extra'])", "x"),
        // namespaces
        ("//e:note", "[extra note]"),
        ("count(//e:*)", "1"),
        ("local-name(//e:note)", "note"),
        ("name(//e:note)", "x:note"),
        ("namespace-uri(//e:note)", "urn:extra"),
        ("count(//note)", "0"),
        // id() and lang()
        ("id('b2')/title", "[Beta]"),
        ("count(id('b1 b3 nope'))", "2"),
        ("//title[lang('de')]", "[Beta]"),
        ("count(//title[lang('en')])", "3"),
        // operators and comparisons
        ("1 + 2 * 3", "7"),
        ("10 div 4", "2.5"),
        ("10 mod 3", "1"),
        ("-5 mod 3", "-2"),
        ("1 div 0", "Infinity"),
        ("-1 div 0", "-Infinity"),
        ("0 div 0", "NaN"),
        ("- - 2", "2"),
        ("//book/@price = 20", "true"),
        ("//book/@price != 20", "true"),
        ("//book/@price > 100", "false"),
        ("//author = //title", "false"),
        ("//author = 'Bob'", "true"),
        ("not(//author = 'Zed')", "true"),
        ("//book = true()", "true"),
        ("'abc' < 'abd'", "false"),
        ("2 < 3 and 3 > 2", "true"),
        ("1 = 2 or 2 = 2", "true"),
        ("true() = 'false'", "true"),
        ("count(//book | //magazine | //book)", "4"),
        ("(//book | //magazine)[last()]/title", "[Delta]"),
        // string functions
        ("string(//book[1]/@price)", "10.5"),
        ("concat('a', 'b', 'c', 1)", "abc1"),
        ("starts-with('camel', 'ca')", "true"),
        ("contains('camel', 'me')", "true"),
        ("substring-before('2024-01-02', '-')", "2024"),
        ("substring-after('2024-01-02', '-')", "01-02"),
        ("substring('12345', 2, 3)", "234"),
        ("substring('12345', 1.5, 2.6)", "234"),
        ("substring('12345', 0, 3)", "12"),
        ("substring('12345', 0 div 0, 3)", ""),
        ("substring('12345', -42, 1 div 0)", "12345"),
        ("string-length('h\u{e9}llo')", "5"),
        ("normalize-space('  a   b  ')", "a b"),
        ("translate('bar', 'abc', 'ABC')", "BAr"),
        ("translate('--aaa--', 'abc-', 'ABC')", "AAA"),
        ("string(1 div 3 > 0)", "true"),
        ("string(0.1 + 0.2)", "0.30000000000000004"),
        ("string(-0)", "0"),
        ("string(123456789012)", "123456789012"),
        // number functions
        ("sum(//book/@price)", "35"),
        ("floor(2.7)", "2"),
        ("ceiling(-2.7)", "-2"),
        ("round(2.5)", "3"),
        ("round(-2.5)", "-2"),
        ("number('  42 ')", "42"),
        ("number('4e2')", "NaN"),
        ("number(true())", "1"),
        ("count(//book[number(@year) mod 2 = 1])", "2"),
        // boolean functions
        ("boolean('')", "false"),
        ("boolean(' ')", "true"),
        ("boolean(0)", "false"),
        ("boolean(//nothing)", "false"),
        ("true() and not(false())", "true"),
        // positions
        ("//book[position() = 2]/title", "[Beta]"),
        ("//book[position() < 3][last()]/title", "[Beta]"),
        ("//book[position() mod 2 = 1]/title", "[Alpha|Gamma]"),
        ("//author[count(preceding-sibling::author) = 1]", "[Bob]"),
        ("//book[count(author) > 1]/title", "[Alpha]"),
        ("//book[title='Beta']/following-sibling::*[last()]/title", "[Delta]"),
        ("string(/library/book[1]/title/text())", "Alpha"),
        ("count(//book[not(@price > 5)])", "1"),
        ("//*[local-name()='magazine']/title", "[Delta]"),
    ];
    assert!(cases.len() >= 60);
    let mut failures = Vec::new();
    for (expr, want) in cases {
        let got = eval(expr);
        if got != *want {
            failures.push(format!("{} => {} (expected {})", expr, got, want));
        }
    }
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn syntax_errors() {
    for bad in ["", "/a/[", "//", "a[", "count(", "1 +", "foo:bar", "@", "a b", "'unterminated", "child::", "badaxis::x", "count(1, 2)"] {
        assert!(XPath::compile(bad).is_err(), "should fail: {:?}", bad);
    }
    let doc = Document::parse("<a/>").unwrap();
    let e = XPath::compile("unknown-fn()").unwrap().eval_string(&doc).unwrap_err();
    assert!(e.message.contains("Could not find function"));
    let e = XPath::compile("$nope").unwrap().eval_string(&doc).unwrap_err();
    assert!(e.message.contains("nope"));
}

#[test]
fn lexer_disambiguation() {
    let doc = Document::parse("<r><div>6</div><mod>4</mod><and>1</and><or>2</or><a-b>3</a-b></r>").unwrap();
    let ev = |e: &str| XPath::compile(e).unwrap().eval_string(&doc).unwrap();
    assert_eq!(ev("/r/div div /r/mod"), "1.5");
    assert_eq!(ev("/r/mod mod 3"), "1");
    assert_eq!(ev("/r/and and /r/or"), "true");
    assert_eq!(ev("/r/a-b - 1"), "2");
    assert_eq!(ev("count(/r/*) * 2"), "10");
}

#[test]
fn camel_language_options() {
    let ctx = CamelContext::new();
    cameleer_xml::register(&ctx);
    ctx.add_routes(|b| {
        b.from("direct:ns").set_header("id", cameleer_xml::xpath_ns("/c:order/c:id", &[("c", "urn:orders")]).with_option("resultType", "String")).to("mock:ns");
        b.from("direct:types")
            .set_header("count", cameleer_core::language::xpath_as("count(//i)", "Integer"))
            .set_header("total", cameleer_core::language::xpath_as("sum(//i)", "number"))
            .set_header("has", cameleer_core::language::xpath_as("//i[. = 2]", "boolean"))
            .set_header("items", cameleer_core::language::xpath_as("//i", "nodeset"))
            .set_header("text", cameleer_core::language::xpath_as("/l", "String"))
            .set_header("fromHeader", cameleer_core::language::xpath("concat(in:header('who'), '-', $who, '-', in:body() != '')"))
            .to("mock:types");
        b.from("direct:filter").filter(cameleer_core::language::xpath("/order[@priority='high']")).to("mock:high");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:ns", "<order xmlns='urn:orders'><id> 42 </id></order>").unwrap();
    let ns = ctx.mock_endpoint("mock:ns").unwrap();
    assert_eq!(ns.received_exchanges()[0].message.headers.get("id"), Some(&Value::from("42")));

    t.send_body_and_header("direct:types", "<l> <i>1</i><i>2</i><i>3</i> </l>", "who", "me").unwrap();
    let m = ctx.mock_endpoint("mock:types").unwrap();
    let ex = &m.received_exchanges()[0];
    let h = |k: &str| ex.message.headers.get(k).cloned().unwrap();
    assert_eq!(h("count"), Value::Int(3));
    assert_eq!(h("total"), Value::Float(6.0));
    assert_eq!(h("has"), Value::Bool(true));
    assert_eq!(h("items"), Value::Array(vec![Value::from("<i>1</i>"), Value::from("<i>2</i>"), Value::from("<i>3</i>")]));
    assert_eq!(h("text"), Value::from("123"));
    assert_eq!(h("fromHeader"), Value::from("me-me-true"));

    t.send_body("direct:filter", "<order priority='low'/>").unwrap();
    t.send_body("direct:filter", "<order priority='high'/>").unwrap();
    assert_eq!(ctx.mock_endpoint("mock:high").unwrap().received_exchanges().len(), 1);

    // saxon=true is rejected, unknown prefixes fail at route creation
    let lang = ctx.resolve_language("xpath").unwrap();
    let mut o = std::collections::BTreeMap::new();
    o.insert("saxon".to_string(), "true".to_string());
    assert!(lang.create_expression("/a", &o).is_err());
    assert!(lang.create_expression("/zz:a", &Default::default()).is_err());
    let mut o = std::collections::BTreeMap::new();
    o.insert("namespace".to_string(), r#"[{"key":"zz","value":"urn:zz"}]"#.to_string());
    assert!(lang.create_expression("/zz:a", &o).is_ok());
    ctx.stop().unwrap();
}

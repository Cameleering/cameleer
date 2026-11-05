//! Tests for tokenize, jq, ref, method/bean languages, regex engine and language builders.

use std::sync::Arc;

use cameleer_core::language::jq::{query as jq_query, Jq};
use cameleer_core::language::{
    bean_method, bind_bean_method, jq, method, ref_expression, tokenize, tokenize_pair, tokenize_regex, tokenize_xml, LanguageOptions,
};
use cameleer_core::prelude::*;
use cameleer_core::regex::Regex;

fn eval(ctx: &CamelContext, def: ValueBuilder, body: impl Into<Value>) -> Result<Value> {
    let mut ex = ctx.create_exchange();
    ex.set_body(body);
    def.def.resolve_expression(ctx)?.evaluate(&ex)
}

fn strs(v: &[&str]) -> Value {
    Value::from(v.to_vec())
}

#[test]
fn tokenize_language() {
    let ctx = CamelContext::new();
    assert_eq!(eval(&ctx, tokenize(","), "a,b,c").unwrap(), strs(&["a", "b", "c"]));
    assert_eq!(eval(&ctx, tokenize("\n"), "l1\nl2\n").unwrap(), strs(&["l1", "l2"]));
    assert_eq!(eval(&ctx, tokenize("\\n"), "l1\nl2").unwrap(), strs(&["l1", "l2"]));
    assert_eq!(eval(&ctx, tokenize(",").with_option("group", "2"), "a,b,c,d,e").unwrap(), strs(&["a,b", "c,d", "e"]));
    assert_eq!(
        eval(&ctx, tokenize(",").with_option("group", "2").with_option("groupDelimiter", "|"), "a,b,c").unwrap(),
        strs(&["a|b", "c"])
    );
    assert_eq!(eval(&ctx, tokenize("\n").with_option("skipFirst", "true"), "header\nr1\nr2").unwrap(), strs(&["r1", "r2"]));
    assert_eq!(eval(&ctx, tokenize_regex("\\s*;\\s*"), "a ; b;c").unwrap(), strs(&["a", "b", "c"]));
    assert_eq!(eval(&ctx, tokenize_pair("[", "]", false), "x[1]y[2]z").unwrap(), strs(&["1", "2"]));
    assert_eq!(eval(&ctx, tokenize_pair("[", "]", true), "x[1]y[2]z").unwrap(), strs(&["[1]", "[2]"]));
    assert_eq!(
        eval(&ctx, tokenize_xml("order"), "<orders><order id=\"1\"><a/></order><order id=\"2\"/><orderx/></orders>").unwrap(),
        strs(&["<order id=\"1\"><a/></order>", "<order id=\"2\"/>"])
    );
    assert_eq!(
        eval(&ctx, tokenize_xml("<item>").with_option("group", "2"), "<r><item>1</item><item>2</item><item>3</item></r>").unwrap(),
        strs(&["<item>1</item><item>2</item>", "<item>3</item>"])
    );
    assert_eq!(eval(&ctx, tokenize(","), Value::Null).unwrap(), Value::Array(vec![]));
    assert_eq!(eval(&ctx, tokenize(","), "").unwrap(), Value::Array(vec![]));
    assert_eq!(eval(&ctx, tokenize(","), strs(&["x", "y"])).unwrap(), strs(&["x", "y"]));
    // header as source (expression text = header name)
    let mut ex = ctx.create_exchange();
    ex.set_header("ids", "1;2;3");
    let e = language("tokenize", "ids").with_option("token", ";").def.resolve_expression(&ctx).unwrap();
    assert_eq!(e.evaluate(&ex).unwrap(), strs(&["1", "2", "3"]));
    // invalid regex fails at creation
    assert!(tokenize_regex("(").def.resolve_expression(&ctx).is_err());
}

#[test]
fn jq_table() {
    let doc = cameleer_core::json::parse(
        r#"{"name":"shop","items":[{"id":1,"price":5,"tags":["a"]},{"id":2,"price":15,"tags":[]},{"id":3,"price":25,"tags":["a","b"]}],"meta":{"x":null}}"#,
    )
    .unwrap();
    let ints = |v: &[i64]| Value::Array(v.iter().map(|i| Value::Int(*i)).collect());
    let cases: Vec<(&str, Value)> = vec![
        (".", doc.clone()),
        ("", doc.clone()),
        (".name", Value::from("shop")),
        (".\"name\"", Value::from("shop")),
        (".[\"name\"]", Value::from("shop")),
        (".items[0].id", Value::Int(1)),
        (".items[-1].id", Value::Int(3)),
        (".items[].id", ints(&[1, 2, 3])),
        ("[.items[].id]", ints(&[1, 2, 3])),
        (".items | length", Value::Int(3)),
        ("[.items[1:][] | .id]", ints(&[2, 3])),
        ("[.items[] | select(.price > 10) | .id]", ints(&[2, 3])),
        ("[.items[] | select(.price > 10 and .price < 20) | .id]", ints(&[2])),
        ("[.items[] | select(.price < 10 or .price > 20) | .id]", ints(&[1, 3])),
        (".items | map(.price) | add", Value::Int(45)),
        (".items | map(.price * 2)", ints(&[10, 30, 50])),
        ("[.items[].price] | max", Value::Int(25)),
        ("[.items[].price] | min", Value::Int(5)),
        (".items | map(.id) | reverse", ints(&[3, 2, 1])),
        (".meta | keys", strs(&["x"])),
        (".meta.x // \"fallback\"", Value::from("fallback")),
        (".missing.deeper", Value::Null),
        ("{n: .name, count: (.items | length)}", Value::object([("n", Value::from("shop")), ("count", Value::Int(3))])),
        ("{name}", Value::object([("name", "shop")])),
        ("{(.name): 1}", Value::object([("shop", 1)])),
        ("[.items[] | {id, expensive: (.price > 10)}] | .[2]", Value::object([("id", Value::Int(3)), ("expensive", Value::Bool(true))])),
        (".items | map(select(.tags | length > 0)) | length", Value::Int(2)),
        (".items[0] | has(\"price\")", Value::Bool(true)),
        (".items[0] | to_entries | .[0].key", Value::from("id")),
        ("keys", Value::from(vec!["items", "meta", "name"])),
        ("keys_unsorted", Value::from(vec!["name", "items", "meta"])),
        ("{z: 1, a: 2} | tojson", Value::from(r#"{"z":1,"a":2}"#)),
        (".items[0] | with_entries(select(.key == \"id\"))", Value::object([("id", 1)])),
        (".items | sort_by(-.price) | .[0].id", Value::Int(3)),
        (".items | group_by(.tags | length) | length", Value::Int(3)),
        ("[.items[].tags[]] | unique", strs(&["a", "b"])),
        (".name | ascii_upcase", Value::from("SHOP")),
        (".name | test(\"^sh\")", Value::Bool(true)),
        (".name | split(\"o\")", strs(&["sh", "p"])),
        ("[\"a\",\"b\"] | join(\"-\")", Value::from("a-b")),
        ("if .items | length > 2 then \"many\" else \"few\" end", Value::from("many")),
        ("if .name == \"x\" then 1 elif .name == \"shop\" then 2 else 3 end", Value::Int(2)),
        ("1 + 2 * 3 - 4 / 2", Value::Int(5)),
        ("10 % 3", Value::Int(1)),
        ("\"a\" + \"b\"", Value::from("ab")),
        ("[1,2] + [3]", ints(&[1, 2, 3])),
        ("{a:1} + {b:2}", Value::object([("a", 1), ("b", 2)])),
        (".items[0].id, .items[1].id", ints(&[1, 2])),
        ("[range(3)]", ints(&[0, 1, 2])),
        (".name | type", Value::from("string")),
        ("[.items[].price | tostring]", strs(&["5", "15", "25"])),
        ("\"42\" | tonumber", Value::Int(42)),
        (".items | first | .id", Value::Int(1)),
        (".items | last | .id", Value::Int(3)),
        ("[.items[] | .price] | any(. > 20)", Value::Bool(true)),
        ("[.items[] | .price] | all(. > 20)", Value::Bool(false)),
        ("[[1,[2]],3] | flatten", ints(&[1, 2, 3])),
        (".name | not", Value::Bool(false)),
        ("empty", Value::Null),
        // objects iterate in key order (items, meta, name)
        ("[.[]?]", Value::Array(vec![Value::from("shop"), doc.get("items").unwrap().clone(), doc.get("meta").unwrap().clone()])),
        (".name? // \"x\"", Value::from("shop")),
        ("[.. | numbers] | length", Value::Int(6)),
        (".items[0] | tojson", Value::from(r#"{"id":1,"price":5,"tags":["a"]}"#)),
        ("\"{\\\"a\\\":1}\" | fromjson | .a", Value::Int(1)),
        (".name | contains(\"ho\")", Value::Bool(true)),
        (".name | sub(\"s\"; \"S\")", Value::from("Shop")),
    ];
    for (q, expected) in cases {
        assert_eq!(jq_query(&doc, q).unwrap_or_else(|e| panic!("{}: {}", q, e)), expected, "jq: {}", q);
    }
    for bad in [".items[", "nosuch", "{a:", "if . then 1", ".a | map(", "\"unterminated"] {
        assert!(Jq::compile(bad).is_err(), "{}", bad);
    }
    assert!(jq_query(&doc, ".name | keys").is_err());
    assert!(jq_query(&doc, ".name.x").is_err());
    assert!(jq_query(&doc, ".items[1:].id").is_err());
}

#[test]
fn jq_in_routes() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:jq").set_header("n", jq(".items | length")).set_body(jq("[.items[] | .id]")).to("mock:jq");
    })
    .unwrap();
    ctx.start().unwrap();
    let m = ctx.mock_endpoint("mock:jq").unwrap();
    m.expected_header_received("n", 2);
    m.expected_bodies_received(vec![Value::from(vec![Value::Int(1), Value::Int(2)])]);
    ctx.create_producer_template().send_body("direct:jq", r#"{"items":[{"id":1},{"id":2}]}"#).unwrap();
    m.assert_is_satisfied().unwrap();
    let p = jq(".ok").def.resolve_predicate(&ctx).unwrap();
    let mut ex = ctx.create_exchange();
    ex.set_body(r#"{"ok":true}"#);
    assert!(p.matches(&ex).unwrap());
    ex.set_body(r#"{"ok":false}"#);
    assert!(!p.matches(&ex).unwrap());
    ctx.stop().unwrap();
}

#[test]
fn ref_bean_method_languages() {
    let ctx = CamelContext::new();
    let e: Arc<dyn Expression> = Arc::new(|ex: &Exchange| Ok(Value::String(format!("<{}>", ex.body()))));
    ctx.registry().bind("wrap", e);
    let p: Arc<dyn Predicate> = Arc::new(cameleer_core::spi::FnPredicate(|ex: &Exchange| Ok(ex.body().to_string().len() > 3)));
    ctx.registry().bind("isLong", p);
    bind_bean_method(ctx.registry(), "calc.double", |ex: &Exchange| Ok(Value::Int(ex.body_as::<i64>()? * 2)));
    bind_bean_method(ctx.registry(), "calc", |_ex: &Exchange| Ok(Value::from("default method")));
    ctx.registry().bind_processor("upper", |ex: &mut Exchange| {
        let s = ex.body().to_string().to_uppercase();
        ex.set_body(s);
        Ok(())
    });

    assert_eq!(eval(&ctx, ref_expression("wrap"), "x").unwrap(), Value::from("<x>"));
    assert_eq!(eval(&ctx, ref_expression("isLong"), "abcd").unwrap(), Value::Bool(true));
    assert!(eval(&ctx, ref_expression("nobody"), "x").unwrap_err().is_a(kinds::NO_SUCH_BEAN));
    assert_eq!(eval(&ctx, method("calc.double"), 21).unwrap(), Value::Int(42));
    assert_eq!(eval(&ctx, bean_method("calc", "double"), 5).unwrap(), Value::Int(10));
    assert_eq!(eval(&ctx, method("calc"), 5).unwrap(), Value::from("default method"));
    assert_eq!(eval(&ctx, method("calc?method=double"), 4).unwrap(), Value::Int(8));
    assert_eq!(eval(&ctx, language("bean", "calc::double"), 3).unwrap(), Value::Int(6));
    assert_eq!(eval(&ctx, method("upper"), "abc").unwrap(), Value::from("ABC"));
    assert!(eval(&ctx, method("ghost"), 1).unwrap_err().is_a(kinds::NO_SUCH_BEAN));
    let p = ref_expression("isLong").def.resolve_predicate(&ctx).unwrap();
    let mut ex = ctx.create_exchange();
    ex.set_body("ab");
    assert!(!p.matches(&ex).unwrap());

    // unsupported languages
    for l in ["groovy", "js", "javascript", "mvel"] {
        let e = language(l, "1 + 1").def.resolve_expression(&ctx).err().unwrap();
        assert!(e.is_a(kinds::NO_SUCH_LANGUAGE), "{}", l);
    }
    // constant with resultType, variable language
    assert_eq!(eval(&ctx, language("constant", "12").with_option("resultType", "Integer"), "x").unwrap(), Value::Int(12));
    let mut ex = ctx.create_exchange();
    ex.set_property("CamelVariable.v1", "var!");
    assert_eq!(language("variable", "v1").def.resolve_expression(&ctx).unwrap().evaluate(&ex).unwrap(), Value::from("var!"));
    let names = ctx.language_names();
    for n in ["simple", "jsonpath", "xpath", "tokenize", "jq", "ref", "method", "bean", "constant", "header", "exchangeProperty", "variable"] {
        assert!(names.contains(&n.to_string()), "{}", n);
    }
}

#[test]
fn regex_engine_table() {
    // (pattern, text, full match?)
    let full: Vec<(&str, &str, bool)> = vec![
        ("[A-Z][a-z]+", "Camel", true),
        ("[A-Z][a-z]+", "camel", false),
        ("\\d{4}-\\d{2}-\\d{2}", "2024-02-29", true),
        ("(?i)CAMEL", "camel", true),
        ("[\\w.+-]+@[\\w-]+\\.[a-z]{2,}", "john.doe+x@mail-server.org", true),
        ("^(\\+\\d{1,3} )?\\d{3}-\\d{4}$", "+44 555-1234", true),
        ("(foo|bar)+baz", "foobarfoobaz", true),
        ("a.*?b", "axxbyyb", true),
        ("(?:ab){2,3}", "ababab", true),
        ("(?:ab){2,3}", "abababab", false),
        ("[^,]+(,[^,]+)*", "a,b,c", true),
        ("\\s+", " \t\n", true),
        ("Camel.*", "CamelFileName", true),
        ("Camel.*", "camelFileName", false),
        ("x*", "", true),
        ("\\p{Upper}\\p{Lower}*", "Hello", true),
        ("(\\d+)(?=px)px", "12px", true),
        ("(a)(b)?c\\2?", "ac", true),
    ];
    for (p, t, expected) in full {
        assert_eq!(Regex::new(p).unwrap().is_full_match(t), expected, "{} vs {}", p, t);
    }
    let re = Regex::new("(?<key>\\w+)=(?<val>[^&]*)").unwrap();
    let all = re.captures_iter("a=1&b=&c=xyz");
    assert_eq!(all.len(), 3);
    assert_eq!(all[2].name("key"), Some("c"));
    assert_eq!(all[1].name("val"), Some(""));
    assert_eq!(re.replace_all("a=1&b=2", "${val}:${key}"), "1:a&2:b");
    assert_eq!(Regex::new("(\\d)(\\d)").unwrap().replace_all("1234", "$2$1"), "2143");
    assert_eq!(Regex::new("o").unwrap().replace_all("foo", "0"), "f00");
    assert_eq!(Regex::new("\\|").unwrap().split("a|b||c"), vec!["a", "b", "", "c"]);
    assert_eq!(Regex::new(Regex::quote("a.b").as_str()).unwrap().find("xa.by"), Some((1, 4)));
    assert_eq!(Regex::new("b+").unwrap().find("aabbbcc"), Some((2, 5)));
    assert_eq!(Regex::new("z").unwrap().find("abc"), None);
    // unicode-aware byte offsets
    let re = Regex::new("é+").unwrap();
    let text = "caféé!";
    let (s, e) = re.find(text).unwrap();
    assert_eq!(&text[s..e], "éé");
    // header pattern matching used by removeHeaders
    assert!(cameleer_core::util::matches_pattern("CamelHttpUri", "Camel(Http|File).*"));
    assert!(!cameleer_core::util::matches_pattern("Other", "Camel(Http|File).*"));
}

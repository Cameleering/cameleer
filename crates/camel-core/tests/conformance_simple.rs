//! Conformance tests for the Simple language, encoding examples from the Apache Camel
//! Simple language documentation (camel.apache.org/components/latest/languages/simple-language.html).

use camel_core::language::simple::{SimpleExpression, SimplePredicate};
use camel_core::prelude::*;

fn eval(ex: &Exchange, text: &str) -> Value {
    SimpleExpression::parse(text)
        .unwrap_or_else(|e| panic!("parse {:?}: {}", text, e))
        .evaluate(ex)
        .unwrap_or_else(|e| panic!("eval {:?}: {}", text, e))
}

fn matches(ex: &Exchange, text: &str) -> bool {
    SimplePredicate::parse(text)
        .unwrap_or_else(|e| panic!("parse {:?}: {}", text, e))
        .matches(ex)
        .unwrap_or_else(|e| panic!("eval {:?}: {}", text, e))
}

fn exchange() -> Exchange {
    let ctx = CamelContext::new();
    ctx.properties().set("greeting", "Hi");
    let mut ex = ctx.create_exchange();
    ex.set_body("Hello Camel");
    ex
}

#[test]
fn doc_header_equals_string_literal() {
    let mut ex = exchange();
    ex.set_header("foo", "bar");
    assert!(matches(&ex, "${header.foo} == 'bar'"));
    assert!(matches(&ex, "${in.header.foo} == 'bar'"));
    assert!(matches(&ex, "${header.foo} != 'baz'"));
    // header lookups are case-insensitive in Camel
    assert!(matches(&ex, "${header.FOO} == 'bar'"));
    // missing header compared with null
    assert!(matches(&ex, "${header.missing} == null"));
    assert!(!matches(&ex, "${header.missing} == 'bar'"));
}

#[test]
fn doc_body_contains() {
    let ex = exchange();
    assert!(matches(&ex, "${body} contains 'Camel'"));
    assert!(!matches(&ex, "${body} contains 'World'"));
    assert!(matches(&ex, "${body} not contains 'World'"));
    assert!(matches(&ex, "${body} !contains 'World'"));
    assert!(matches(&ex, "${body} ~~ 'camel'"), "~~ is contains ignore case");
}

#[test]
fn doc_in_operator() {
    let mut ex = exchange();
    ex.set_header("type", "gold");
    assert!(matches(&ex, "${in.header.type} in 'gold,silver'"));
    ex.set_header("type", "bronze");
    assert!(!matches(&ex, "${in.header.type} in 'gold,silver'"));
    assert!(matches(&ex, "${in.header.type} not in 'gold,silver'"));
    assert!(matches(&ex, "${in.header.type} !in 'gold,silver'"));
}

#[test]
fn doc_range_operator() {
    let mut ex = exchange();
    ex.set_header("number", 150);
    assert!(matches(&ex, "${header.number} range '100..199'"));
    ex.set_header("number", "123");
    assert!(matches(&ex, "${header.number} range '100..199'"), "string header is converted to a number");
    ex.set_header("number", 200);
    assert!(!matches(&ex, "${header.number} range '100..199'"));
    assert!(matches(&ex, "${header.number} not range '100..199'"));
    ex.set_header("number", 199);
    assert!(matches(&ex, "${header.number} range '100..199'"), "range is inclusive");
}

#[test]
fn doc_numeric_comparisons_coerce_strings() {
    let mut ex = exchange();
    ex.set_header("bar", "123");
    assert!(matches(&ex, "${header.bar} > 100"));
    assert!(matches(&ex, "${header.bar} == 123"));
    assert!(matches(&ex, "${header.bar} >= 123"));
    assert!(!matches(&ex, "${header.bar} < 100"));
    ex.set_header("bar", 5);
    // "5" < "10" as numbers, not lexicographically
    assert!(matches(&ex, "${header.bar} < 10"));
}

#[test]
fn doc_regex_operator() {
    let mut ex = exchange();
    ex.set_header("number", "12345");
    assert!(matches(&ex, "${header.number} regex '\\d{5}'"));
    assert!(!matches(&ex, "${header.number} regex '\\d{4}'"), "regex must match the whole value");
    assert!(matches(&ex, "${header.number} not regex '[a-z]+'"));
}

#[test]
fn doc_logical_operators() {
    let mut ex = exchange();
    ex.set_header("title", "Camel in Action");
    ex.set_header("number", 150);
    assert!(matches(&ex, "${header.title} contains 'Camel' && ${header.number} range '100..199'"));
    assert!(matches(&ex, "${header.title} contains 'Java' || ${header.number} > 100"));
    assert!(!matches(&ex, "${header.title} contains 'Java' && ${header.number} > 100"));
}

#[test]
fn doc_is_operator_and_null_checks() {
    let mut ex = exchange();
    ex.set_header("num", 5);
    assert!(matches(&ex, "${body} is 'String'"));
    assert!(matches(&ex, "${body} is 'java.lang.String'"));
    assert!(matches(&ex, "${header.num} is 'Integer'"));
    assert!(matches(&ex, "${header.num} != null"));
    assert!(matches(&ex, "${header.nope} == null"));
}

#[test]
fn doc_starts_ends_with() {
    let ex = exchange();
    assert!(matches(&ex, "${body} startsWith 'Hello'"));
    assert!(matches(&ex, "${body} endsWith 'Camel'"));
    assert!(matches(&ex, "${body} starts with 'Hello'"));
    assert!(!matches(&ex, "${body} startsWith 'Camel'"));
}

#[test]
fn doc_date_now_format() {
    let ex = exchange();
    let v = eval(&ex, "${date:now:yyyyMMdd}").to_string();
    assert_eq!(v.len(), 8, "{}", v);
    assert!(v.chars().all(|c| c.is_ascii_digit()));
    assert!(v.starts_with("20"));
    let v = eval(&ex, "${date:now:yyyy-MM-dd'T'HH:mm:ss}").to_string();
    assert_eq!(v.len(), 19, "{}", v);
    assert_eq!(&v[10..11], "T");
}

#[test]
fn doc_body_as_with_method_call() {
    let ex = exchange();
    assert_eq!(eval(&ex, "${bodyAs(String).length()}"), Value::Int(11));
    assert_eq!(eval(&ex, "${bodyAs(String).toUpperCase()}"), Value::from("HELLO CAMEL"));
    assert_eq!(eval(&ex, "${body.length()}"), Value::Int(11));
    let mut ex2 = exchange();
    ex2.set_body(Value::Bytes(b"bytes".to_vec()));
    assert_eq!(eval(&ex2, "${bodyAs(String)}"), Value::from("bytes"));
}

#[test]
fn doc_ognl_list_and_map_navigation() {
    let mut ex = exchange();
    ex.set_body(camel_core::json::parse(r#"[{"name":"Camel","age":17},{"name":"Rust","age":10}]"#).unwrap());
    assert_eq!(eval(&ex, "${body[0][name]}"), Value::from("Camel"));
    assert_eq!(eval(&ex, "${body[1][age]}"), Value::Int(10));
    assert_eq!(eval(&ex, "${body[last][name]}"), Value::from("Rust"));
    // null safe operator
    ex.set_header("order", camel_core::json::parse(r#"{"customer":null}"#).unwrap());
    assert_eq!(eval(&ex, "${header.order?.customer?.name}"), Value::Null);
}

#[test]
fn doc_header_map_index() {
    let mut ex = exchange();
    ex.set_header("foo", camel_core::json::parse(r#"{"bar":"baz"}"#).unwrap());
    assert_eq!(eval(&ex, "${header.foo[bar]}"), Value::from("baz"));
    assert_eq!(eval(&ex, "${header.foo.bar}"), Value::from("baz"));
}

#[test]
fn doc_exchange_property_split_index() {
    let mut ex = exchange();
    ex.set_property("CamelSplitIndex", 2);
    assert_eq!(eval(&ex, "${exchangeProperty.CamelSplitIndex}"), Value::Int(2));
    assert_eq!(eval(&ex, "${exchangeProperty[CamelSplitIndex]}"), Value::Int(2));
    assert_eq!(eval(&ex, "Part ${exchangeProperty.CamelSplitIndex}"), Value::from("Part 2"));
}

#[test]
fn doc_ternary_and_elvis() {
    let mut ex = exchange();
    ex.set_header("foo", 44);
    assert_eq!(eval(&ex, "${header.foo > 0 ? 'positive' : 'negative'}"), Value::from("positive"));
    assert_eq!(eval(&ex, "${header.missing ?: 'default'}"), Value::from("default"));
    assert_eq!(eval(&ex, "${header.foo ?: 'default'}"), Value::Int(44));
    assert_eq!(eval(&ex, "${iif(${header.foo} > 40,'big','small')}"), Value::from("big"));
}

#[test]
fn doc_properties_function_and_defaults() {
    let ex = exchange();
    assert_eq!(eval(&ex, "${properties:greeting}"), Value::from("Hi"));
    assert_eq!(eval(&ex, "${properties:missing:fallback}"), Value::from("fallback"));
    // {{placeholders}} in Simple expressions are resolved when the route is created
    let ctx = CamelContext::new();
    ctx.properties().set("greeting", "Hi");
    ctx.add_routes(|b| {
        b.from("direct:p").set_body(simple("{{greeting}} ${body} ${properties:greeting}"));
    })
    .unwrap();
    ctx.start().unwrap();
    assert_eq!(ctx.create_producer_template().request_body("direct:p", "you").unwrap(), Value::from("Hi you Hi"));
}

#[test]
fn doc_exception_message_and_stacktrace() {
    let mut ex = exchange();
    ex.set_exception(CamelError::illegal_argument("Forced"));
    assert_eq!(eval(&ex, "${exception.message}"), Value::from("Forced"));
    assert_eq!(eval(&ex, "${exception.class}"), Value::from("IllegalArgumentException"));
    assert!(eval(&ex, "${exception.stacktrace}").to_string().contains("Forced"));
    // caught exceptions (after handling) are also visible as ${exception}
    let mut ex2 = exchange();
    camel_core::processor::errors::set_caught_exception(&mut ex2, &CamelError::io("caught one"));
    assert_eq!(eval(&ex2, "${exception.message}"), Value::from("caught one"));
}

#[test]
fn doc_string_concatenation_and_type_of_mixed_text() {
    let mut ex = exchange();
    ex.set_header("name", "Claus");
    ex.set_header("n", 3);
    // a single function returns the typed value; mixed text is a String
    assert_eq!(eval(&ex, "${header.n}"), Value::Int(3));
    assert_eq!(eval(&ex, "n=${header.n}"), Value::from("n=3"));
    assert_eq!(eval(&ex, "Hello ${header.name} how are you?"), Value::from("Hello Claus how are you?"));
}

#[test]
fn doc_headers_size_and_body_type() {
    let mut ex = exchange();
    ex.set_header("a", 1);
    ex.set_header("b", 2);
    assert_eq!(eval(&ex, "${headers}").len(), Some(2));
    // the Java class name Camel reports
    assert_eq!(eval(&ex, "${bodyType}"), Value::from("java.lang.String"));
    assert!(matches(&ex, "${bodyType} == 'java.lang.String'"));
    let types = [
        (Value::from(1), "java.lang.Integer"),
        (Value::from(1i64 << 40), "java.lang.Long"),
        (Value::from(1.5), "java.lang.Double"),
        (Value::from(true), "java.lang.Boolean"),
        (Value::Bytes(vec![1]), "byte[]"),
        (Value::Array(vec![Value::Int(1)]), "java.util.ArrayList"),
        (Value::Object(Default::default()), "java.util.LinkedHashMap"),
    ];
    for (body, name) in types {
        ex.set_body(body);
        assert_eq!(eval(&ex, "${bodyType}"), Value::from(name));
    }
    ex.set_body(Value::Null);
    assert_eq!(eval(&ex, "${bodyType}"), Value::Null);
}

#[test]
fn doc_type_function_for_exchange_constants() {
    let mut ex = exchange();
    ex.set_header("CamelFileName", "a.txt");
    assert_eq!(eval(&ex, "${type:org.apache.camel.Exchange.FILE_NAME}"), Value::from("CamelFileName"));
    assert_eq!(eval(&ex, "${type:org.apache.camel.Exchange.REDELIVERY_COUNTER}"), Value::from("CamelRedeliveryCounter"));
    assert_eq!(eval(&ex, "${type:org.apache.camel.Exchange.HTTP_RESPONSE_CODE}"), Value::from("CamelHttpResponseCode"));
    assert_eq!(eval(&ex, "${type:org.apache.camel.Exchange.CONTENT_TYPE}"), Value::from("Content-Type"));
    assert_eq!(eval(&ex, "${type:org.apache.camel.Exchange.ERRORHANDLER_HANDLED}"), Value::from("CamelErrorHandlerHandled"));
    assert_eq!(eval(&ex, "${type:java.lang.Integer.MAX_VALUE}"), Value::Int(i32::MAX as i64));
    assert!(matches(&ex, "${header.CamelFileName} == 'a.txt' && ${type:org.apache.camel.Exchange.FILE_NAME} == 'CamelFileName'"));
    assert!(SimpleExpression::parse("${type:org.apache.camel.Nope.X}").is_err());
}

#[test]
fn doc_message_history_function() {
    let ctx = CamelContext::new();
    ctx.set_message_history(true);
    ctx.add_routes(|b| {
        b.from("direct:start").route_id("hist").to("mock:x").set_body(simple("${messageHistory}")).to("mock:full");
        b.from("direct:short").route_id("short").set_body(simple("${messageHistory(false)}")).to("mock:short");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:start", "payload").unwrap();
    t.send_body("direct:short", "payload").unwrap();
    let full = ctx.mock_endpoint("mock:full").unwrap().exchange(0).unwrap().body().to_string();
    assert!(full.starts_with("Message History\n"), "{}", full);
    assert!(full.contains("[hist              ] [hist              ] [from[direct://start]"), "{}", full);
    assert!(full.contains("to[mock:x]"), "{}", full);
    assert!(full.contains("\nExchange\n"), "{}", full);
    assert!(full.contains("Body: payload"), "{}", full);
    let short = ctx.mock_endpoint("mock:short").unwrap().exchange(0).unwrap().body().to_string();
    assert!(short.contains("from[direct://short]"), "{}", short);
    assert!(!short.contains("Exchange"), "{}", short);
}

#[test]
fn doc_exchange_id_and_route_id() {
    let mut ex = exchange();
    ex.from_route_id = Some("myRoute".into());
    assert_eq!(eval(&ex, "${exchangeId}"), Value::from(ex.exchange_id.to_string()));
    assert_eq!(eval(&ex, "${routeId}"), Value::from("myRoute"));
}

#[test]
fn doc_functions_in_predicates_used_by_routes() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .choice()
            .when(simple("${header.type} in 'gold,silver'"))
            .to("mock:premium")
            .when(simple("${body} contains 'Camel'"))
            .to("mock:camel")
            .otherwise()
            .to("mock:other")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body_and_header("direct:start", "x", "type", "gold").unwrap();
    t.send_body("direct:start", "Apache Camel").unwrap();
    t.send_body("direct:start", "Spring").unwrap();
    assert_eq!(ctx.mock_endpoint("mock:premium").unwrap().received_counter(), 1);
    assert_eq!(ctx.mock_endpoint("mock:camel").unwrap().received_counter(), 1);
    assert_eq!(ctx.mock_endpoint("mock:other").unwrap().received_counter(), 1);
}

#[test]
fn doc_file_functions_available_in_simple() {
    // ${file:name} etc are Simple language functions reading the CamelFileName header
    let mut ex = exchange();
    ex.set_header("CamelFileName", "sub/report.txt");
    ex.set_header("CamelFileNameOnly", "report.txt");
    assert_eq!(eval(&ex, "${file:name}"), Value::from("sub/report.txt"));
    assert_eq!(eval(&ex, "${file:name.noext}"), Value::from("sub/report"));
    assert_eq!(eval(&ex, "${file:onlyname}"), Value::from("report.txt"));
    assert_eq!(eval(&ex, "${file:onlyname.noext}"), Value::from("report"));
    assert_eq!(eval(&ex, "${file:ext}"), Value::from("txt"));
    assert_eq!(eval(&ex, "${file:name.ext}"), Value::from("txt"));
    assert_eq!(eval(&ex, "done/${file:name.noext}.bak"), Value::from("done/sub/report.bak"));
}

#[test]
fn doc_variables_and_set_variable() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:v").set_variable("greet", constant("hey")).set_body(simple("${variable.greet} ${body}"));
    })
    .unwrap();
    ctx.start().unwrap();
    assert_eq!(ctx.create_producer_template().request_body("direct:v", "you").unwrap(), Value::from("hey you"));
}

#[test]
fn doc_empty_and_boolean_literals() {
    let mut ex = exchange();
    ex.set_header("e", "");
    ex.set_header("flag", true);
    assert!(matches(&ex, "${header.e} == ''"));
    assert!(matches(&ex, "${header.flag} == true"));
    assert!(matches(&ex, "${header.flag}"));
    assert!(!matches(&ex, "${header.missing}"));
}

/// Camel `ObjectHelper.evaluateValuePredicate`: a bare value used as a predicate is true unless
/// it is null, `false`, the string "false" (trimmed, any case) or an empty collection.
#[test]
fn doc_value_predicate_truthiness_matches_camel() {
    let mut ex = exchange();
    ex.set_header("e", "");
    ex.set_header("zero", 0);
    ex.set_header("f", " FALSE ");
    ex.set_header("t", "True");
    ex.set_header("word", "no");
    ex.set_header("list", Value::Array(vec![]));
    ex.set_header("map", Value::Object(Default::default()));
    assert!(matches(&ex, "${header.e}"));
    assert!(matches(&ex, "${header.zero}"));
    assert!(!matches(&ex, "${header.f}"));
    assert!(matches(&ex, "${header.t}"));
    assert!(matches(&ex, "${header.word}"));
    assert!(!matches(&ex, "${header.list}"));
    assert!(matches(&ex, "${header.map}"));
    // the same rules apply to other languages used as predicates (header, constant)
    let ctx = CamelContext::new();
    let p = |lang: &str, text: &str| ctx.resolve_language(lang).unwrap().create_predicate(text, &Default::default()).unwrap().matches(&ex).unwrap();
    assert!(p("header", "zero"));
    assert!(p("header", "e"));
    assert!(!p("header", "f"));
    assert!(!p("header", "missing"));
    assert!(p("constant", "0"));
    assert!(!p("constant", "false"));
}

#[test]
fn doc_equals_ignore_case_and_not_equals_ignore_case() {
    let mut ex = exchange();
    ex.set_header("foo", "BAR");
    assert!(matches(&ex, "${header.foo} =~ 'bar'"));
    assert!(!matches(&ex, "${header.foo} == 'bar'"));
    assert!(matches(&ex, "${header.foo} !=~ 'baz'"));
}

#[test]
fn doc_null_body_and_empty_checks() {
    let mut ex = exchange();
    ex.set_body(Value::Null);
    assert!(matches(&ex, "${body} == null"));
    assert!(matches(&ex, "${body} == ${null}"));
    assert_eq!(eval(&ex, "${body}"), Value::Null);
    assert_eq!(eval(&ex, "x${body}y"), Value::from("xy"), "null renders as empty text");
}

#[test]
fn doc_header_as_and_body_as_types() {
    let mut ex = exchange();
    ex.set_header("num", "42");
    ex.set_body("7");
    assert_eq!(eval(&ex, "${headerAs(num,Integer)}"), Value::Int(42));
    assert_eq!(eval(&ex, "${bodyAs(Integer)}"), Value::Int(7));
    assert!(matches(&ex, "${bodyAs(Integer)} == 7"));
}

#[test]
fn doc_ognl_method_calls_on_strings() {
    let mut ex = exchange();
    ex.set_header("name", "  Camel  ");
    assert_eq!(eval(&ex, "${header.name.trim()}"), Value::from("Camel"));
    assert_eq!(eval(&ex, "${body.substring(6)}"), Value::from("Camel"));
    assert_eq!(eval(&ex, "${body.replace('Camel','Rust')}"), Value::from("Hello Rust"));
    assert!(matches(&ex, "${body.startsWith('Hello')}"));
}

#[test]
fn doc_built_in_functions() {
    let mut ex = exchange();
    ex.set_body("a-b-c");
    assert_eq!(eval(&ex, "${replace(-,+)}"), Value::from("a+b+c"));
    assert_eq!(eval(&ex, "${substringBefore('-')}"), Value::from("a"));
    assert_eq!(eval(&ex, "${substringAfter('-')}"), Value::from("b-c"));
    assert_eq!(eval(&ex, "${uppercase()}"), Value::from("A-B-C"));
    let r: i64 = eval(&ex, "${random(1,10)}").as_i64().unwrap();
    assert!((1..10).contains(&r));
    assert_eq!(eval(&ex, "${uuid}").to_string().len(), 36);
}

#[test]
fn doc_array_header_index_and_size() {
    let mut ex = exchange();
    ex.set_header("list", Value::from(vec!["x", "y", "z"]));
    assert_eq!(eval(&ex, "${header.list[1]}"), Value::from("y"));
    assert_eq!(eval(&ex, "${header.list.size()}"), Value::Int(3));
    assert!(matches(&ex, "${header.list} contains 'z'"));
}

#[test]
fn doc_date_from_header_with_offset() {
    let mut ex = exchange();
    // 2024-01-15T10:00:00Z
    ex.set_header("d", camel_core::language::support::parse_date("2024-01-15T10:00:00Z").unwrap());
    assert_eq!(eval(&ex, "${date:header.d:yyyy-MM-dd}"), Value::from("2024-01-15"));
    assert_eq!(eval(&ex, "${date:header.d+1d:yyyy-MM-dd}"), Value::from("2024-01-16"));
    assert_eq!(eval(&ex, "${date-with-timezone:header.d:GMT+02:00:HH:mm}"), Value::from("12:00"));
}

#[test]
fn doc_date_with_named_timezone() {
    let mut ex = exchange();
    // just before / after the Europe/Paris spring-forward transition (2024-03-31 01:00 UTC)
    ex.set_header("a", camel_core::language::support::parse_date("2024-03-31T00:59:00Z").unwrap());
    ex.set_header("b", camel_core::language::support::parse_date("2024-03-31T01:00:00Z").unwrap());
    assert_eq!(eval(&ex, "${date-with-timezone:header.a:Europe/Paris:yyyy-MM-dd HH:mm}"), Value::from("2024-03-31 01:59"));
    assert_eq!(eval(&ex, "${date-with-timezone:header.b:Europe/Paris:yyyy-MM-dd HH:mm z}"), Value::from("2024-03-31 03:00 CEST"));
    // America/New_York fall back (2024-11-03 06:00 UTC)
    ex.set_header("c", camel_core::language::support::parse_date("2024-11-03T05:30:00Z").unwrap());
    ex.set_header("d", camel_core::language::support::parse_date("2024-11-03T06:30:00Z").unwrap());
    assert_eq!(eval(&ex, "${date-with-timezone:header.c:America/New_York:HH:mm z}"), Value::from("01:30 EDT"));
    assert_eq!(eval(&ex, "${date-with-timezone:header.d:America/New_York:HH:mm z}"), Value::from("01:30 EST"));
    // now is accepted too
    let v = eval(&ex, "${date-with-timezone:now:Europe/Paris:yyyy-MM-dd HH:mm}").to_string();
    assert_eq!(v.len(), 16, "{}", v);
}

#[test]
fn doc_messages_with_dollar_and_braces_text() {
    let mut ex = exchange();
    ex.set_header("price", 10);
    assert_eq!(eval(&ex, "Total: $ ${header.price}"), Value::from("Total: $ 10"));
    assert_eq!(eval(&ex, "{\"price\": ${header.price}}"), Value::from("{\"price\": 10}"));
}

#[test]
fn doc_invalid_syntax_is_reported() {
    assert!(SimpleExpression::parse("${header.foo").is_err());
    assert!(SimplePredicate::parse("${header.foo} == ").is_err());
    let e = SimpleExpression::parse("${nosuchfunction()}").err().expect("unknown function");
    assert!(e.kind.contains("Syntax") || e.kind.contains("Expression"), "{}", e);
}

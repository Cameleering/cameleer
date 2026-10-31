//! Table-driven tests for the Simple language.

use camel_core::language::simple::{SimpleExpression, SimplePredicate};
use camel_core::language::{bind_bean_method, support};
use camel_core::prelude::*;

fn ctx() -> CamelContext {
    let ctx = CamelContext::with_name("myCamel");
    ctx.properties().set("app.name", "demo");
    ctx.properties().set("app.greeting", "Hello {{app.name}}");
    ctx.registry().bind("myString", String::from("from-registry"));
    bind_bean_method(ctx.registry(), "greeter", |ex: &Exchange| Ok(Value::String(format!("Hi {}", ex.body()))));
    bind_bean_method(ctx.registry(), "greeter.shout", |ex: &Exchange| Ok(Value::String(ex.body().to_string().to_uppercase())));
    ctx
}

fn exchange(ctx: &CamelContext) -> Exchange {
    let mut ex = ctx.create_exchange();
    ex.set_body("Hello World");
    ex.set_header("foo", "abc");
    ex.set_header("num", 10);
    ex.set_header("price", 4.5);
    ex.set_header("flag", true);
    ex.set_header("empty", "");
    ex.set_header("list", Value::from(vec!["a", "b", "c"]));
    ex.set_header("Content-Type", "text/plain");
    ex.set_header("my.dotted", "dotted-value");
    ex.set_header("ts", support::parse_date("2024-02-29T13:45:10.007Z").unwrap());
    ex.set_header("order", camel_core::json::parse(r#"{"id":7,"items":[{"name":"x","qty":2},{"name":"y","qty":3}],"customer":{"name":"Ann","address":null}}"#).unwrap());
    ex.set_property("prop1", "pval");
    ex.set_property("CamelVariable.myVar", "vval");
    ex.from_route_id = Some("route-1".into());
    ex
}

fn eval(ex: &Exchange, text: &str) -> Value {
    match SimpleExpression::parse(text) {
        Ok(e) => e.evaluate(ex).unwrap_or_else(|err| panic!("evaluation of {:?} failed: {}", text, err)),
        Err(err) => panic!("parse of {:?} failed: {}", text, err),
    }
}

fn pred(ex: &Exchange, text: &str) -> bool {
    match SimplePredicate::parse(text) {
        Ok(p) => p.matches(ex).unwrap_or_else(|err| panic!("evaluation of predicate {:?} failed: {}", text, err)),
        Err(err) => panic!("parse of predicate {:?} failed: {}", text, err),
    }
}

#[test]
fn templating_table() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    let cases: Vec<(&str, Value)> = vec![
        ("Hello", Value::from("Hello")),
        ("", Value::from("")),
        ("${body}", Value::from("Hello World")),
        ("${in.body}", Value::from("Hello World")),
        ("Got ${body}!", Value::from("Got Hello World!")),
        ("${header.foo}", Value::from("abc")),
        ("${headers.foo}", Value::from("abc")),
        ("${header[foo]}", Value::from("abc")),
        ("${header['foo']}", Value::from("abc")),
        ("${header:foo}", Value::from("abc")),
        ("${in.header.foo}", Value::from("abc")),
        ("${in.headers.foo}", Value::from("abc")),
        ("${header.FOO}", Value::from("abc")),
        ("${header.Content-Type}", Value::from("text/plain")),
        ("${header.my.dotted}", Value::from("dotted-value")),
        ("${header.missing}", Value::Null),
        ("${header.num}", Value::Int(10)),
        ("${header.foo}-${header.num}", Value::from("abc-10")),
        ("${exchangeProperty.prop1}", Value::from("pval")),
        ("${exchangeProperty[prop1]}", Value::from("pval")),
        ("${exchangeProperty:prop1}", Value::from("pval")),
        ("${variable.myVar}", Value::from("vval")),
        ("${variable:myVar}", Value::from("vval")),
        ("${variables}", Value::object([("myVar", "vval")])),
        ("${camelId}", Value::from("myCamel")),
        ("${camelContext.name}", Value::from("myCamel")),
        ("${routeId}", Value::from("route-1")),
        ("${fromRouteId}", Value::from("route-1")),
        ("${null}", Value::Null),
        ("x${null}y", Value::from("xy")),
        ("${bodyOneLine}", Value::from("Hello World")),
        ("${bodyType}", Value::from("string")),
        ("${body.length()}", Value::Int(11)),
        ("${body.toUpperCase()}", Value::from("HELLO WORLD")),
        ("${body.substring(6)}", Value::from("World")),
        ("${body.substring(0, 5)}", Value::from("Hello")),
        ("${body.replace('World', 'Camel')}", Value::from("Hello Camel")),
        ("${body.startsWith('Hello')}", Value::Bool(true)),
        ("${bodyAs(String)}", Value::from("Hello World")),
        ("${bodyAs(String).toLowerCase()}", Value::from("hello world")),
        ("${header.order.id}", Value::Int(7)),
        ("${header.order.items[1].name}", Value::from("y")),
        ("${header.order.items[last].qty}", Value::Int(3)),
        ("${header.order.items.size()}", Value::Int(2)),
        ("${header.order.customer.name}", Value::from("Ann")),
        ("${header.order.customer?.address?.street}", Value::Null),
        ("${header.list[0]}", Value::from("a")),
        ("${header.list[-1]}", Value::from("c")),
        ("${headerAs(num, String)}", Value::from("10")),
        ("${headerAs('num', Double)}", Value::Float(10.0)),
        ("${properties:app.name}", Value::from("demo")),
        ("${properties:app.greeting}", Value::from("Hello demo")),
        ("${properties:missing:fallback}", Value::from("fallback")),
        ("${properties-exist:app.name}", Value::Bool(true)),
        ("${properties-exist:nope}", Value::Bool(false)),
        ("${ref:myString}", Value::from("from-registry")),
        ("${bean:greeter}", Value::from("Hi Hello World")),
        ("${bean:greeter.shout}", Value::from("HELLO WORLD")),
        ("${bean:greeter?method=shout}", Value::from("HELLO WORLD")),
        ("${date:header.ts:yyyy-MM-dd}", Value::from("2024-02-29")),
        ("${date:header.ts:yyyy-MM-dd'T'HH:mm:ss.SSS}", Value::from("2024-02-29T13:45:10.007")),
        ("${date:header.ts+1h:HH:mm}", Value::from("14:45")),
        ("${date:header.ts-1d:dd}", Value::from("28")),
        ("${date-with-timezone:header.ts:GMT+02:00:HH:mm}", Value::from("15:45")),
        ("${date-with-timezone:header.ts:UTC:HH:mm z}", Value::from("13:45 UTC")),
        ("${date:header.ts}", Value::Int(support::parse_date("2024-02-29T13:45:10.007Z").unwrap())),
        ("${date:header.nope:yyyy}", Value::Null),
        ("a\\tb", Value::from("a\tb")),
        ("line1\\nline2", Value::from("line1\nline2")),
        ("$${body}", Value::from("${body}")),
        ("\\${body}", Value::from("${body}")),
        ("$simple{header.foo}", Value::from("abc")),
        ("${header.${header.foo}}", Value::Null),
        ("${header.num}++", Value::Int(11)),
        ("${header.num}--", Value::Int(9)),
        ("${header.num} ++x", Value::from("10 ++x")),
    ];
    for (text, expected) in cases {
        assert_eq!(eval(&ex, text), expected, "expression: {}", text);
    }
}

#[test]
fn camel4_functions_table() {
    let ctx = ctx();
    let mut ex = exchange(&ctx);
    ex.set_header("csv", "a,b,c,d,e");
    ex.set_header("nums", Value::from(vec![Value::Int(3), Value::Int(9), Value::Int(-2)]));
    ex.set_header("padded", "  spaced out  ");
    ex.set_header("neg", -7);
    ex.set_header("real", 2.4);
    let cases: Vec<(&str, Value)> = vec![
        ("${uppercase()}", Value::from("HELLO WORLD")),
        ("${lowercase(${header.foo})}", Value::from("abc")),
        ("${trim(${header.padded})}", Value::from("spaced out")),
        ("${normalizeWhitespace(${header.padded})}", Value::from("spaced out")),
        ("${length()}", Value::Int(11)),
        ("${size(${header.list})}", Value::Int(3)),
        ("${size()}", Value::Int(1)),
        ("${substring(6)}", Value::from("World")),
        ("${substring(-6)}", Value::from("Hello")),
        ("${substring(1,1)}", Value::from("ello Worl")),
        ("${substring(2,0,${header.foo})}", Value::from("c")),
        ("${substringBefore(' ')}", Value::from("Hello")),
        ("${substringAfter(' ')}", Value::from("World")),
        ("${substringBefore(${header.csv},',')}", Value::from("a")),
        ("${substringBetween('H','o')}", Value::from("ell")),
        ("${replace(World,Camel)}", Value::from("Hello Camel")),
        ("${replace(&comma;,;,${header.csv})}", Value::from("a;b;c;d;e")),
        ("${replace(' ',&empty;)}", Value::from("HelloWorld")),
        ("${join(;)}", Value::from("Hello World")),
        ("${join(-,x,${header.list})}", Value::from("xa-xb-xc")),
        ("${join(',','',${header.nums})}", Value::from("3,9,-2")),
        ("${split(${header.csv},',')}", Value::from(vec!["a", "b", "c", "d", "e"])),
        ("${split(' ')}", Value::from(vec!["Hello", "World"])),
        ("${sum(${header.nums})}", Value::Int(10)),
        ("${sum(1,2,3)}", Value::Int(6)),
        ("${max(${header.nums})}", Value::Int(9)),
        ("${min(${header.nums})}", Value::Int(-2)),
        ("${average(2,4)}", Value::Int(3)),
        ("${abs(${header.neg})}", Value::Int(7)),
        ("${floor(${header.real})}", Value::Int(2)),
        ("${ceil(${header.real})}", Value::Int(3)),
        ("${round(${header.price})}", Value::Int(5)),
        ("${concat(${header.foo},${header.num},-)}", Value::from("abc-10")),
        ("${isEmpty(${header.empty})}", Value::Bool(true)),
        ("${isEmpty(${header.foo})}", Value::Bool(false)),
        ("${hash(abc)}", Value::from("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")),
        ("${hash(abc,SHA-1)}", Value::from("a9993e364706816aba3e25717850c26c9cd0d89d")),
        ("${hash(abc,MD5)}", Value::from("900150983cd24fb0d6963f7d28e17f72")),
        ("${base64Encode(${header.foo})}", Value::from("YWJj")),
        ("${base64Decode(YWJj)}", Value::from("abc")),
        ("${iif(${header.num} > 5,big,small)}", Value::from("big")),
        ("${iif(header.num < 5,'big','small')}", Value::from("small")),
        ("${not(${header.flag})}", Value::Bool(false)),
        ("${empty(list)}", Value::Array(vec![])),
        ("${empty(string)}", Value::from("")),
        ("${list(a,b)}", Value::from(vec!["a", "b"])),
        ("${map(k1,v1,k2,${header.num})}", Value::object([("k1", Value::from("v1")), ("k2", Value::Int(10))])),
        ("${collate(2)}", Value::Array(vec![Value::Array(vec![Value::from("Hello World")])])),
        ("${skip(1)}", Value::Array(vec![])),
        ("${jsonpath(${header.order},$.items[0].name)}", Value::from("x")),
        ("${jq(${header.order},.customer.name)}", Value::from("Ann")),
        ("${xpath('<a><b>v</b></a>','/a/b/text()')}", Value::from("v")),
        ("${increment(${header.num})}", Value::Int(11)),
        ("${decrement(${header.num})}", Value::Int(9)),
    ];
    for (text, expected) in cases {
        assert_eq!(eval(&ex, text), expected, "expression: {}", text);
    }
    // collate / skip over a list body
    let mut ex2 = exchange(&ctx);
    ex2.set_body(Value::from(vec!["1", "2", "3", "4", "5"]));
    assert_eq!(eval(&ex2, "${collate(2)}").as_array().unwrap().len(), 3);
    assert_eq!(eval(&ex2, "${skip(3)}"), Value::from(vec!["4", "5"]));
    assert_eq!(eval(&ex2, "${body[2]}"), Value::from("3"));
    assert_eq!(eval(&ex2, "${sum()}"), Value::Int(15));
}

#[test]
fn ternary_and_elvis() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    assert_eq!(eval(&ex, "${header.num > 5 ? 'big' : 'small'}"), Value::from("big"));
    assert_eq!(eval(&ex, "${header.num > 50 ? 'big' : 'small'}"), Value::from("small"));
    assert_eq!(eval(&ex, "${header.foo == 'abc' ? header.num : 0}"), Value::Int(10));
    assert_eq!(eval(&ex, "${${header.num} > 5 && ${header.flag} ? 'yes' : 'no'}"), Value::from("yes"));
    assert_eq!(eval(&ex, "Size: ${header.num >= 10 ? 'L' : 'S'}"), Value::from("Size: L"));
    assert_eq!(eval(&ex, "${header.missing ?: 'default'}"), Value::from("default"));
    assert_eq!(eval(&ex, "${header.empty ?: 'default'}"), Value::from("default"));
    assert_eq!(eval(&ex, "${header.foo ?: 'default'}"), Value::from("abc"));
}

#[test]
fn exception_functions() {
    let ctx = ctx();
    let mut ex = exchange(&ctx);
    assert_eq!(eval(&ex, "${exception}"), Value::Null);
    assert_eq!(eval(&ex, "${exception.message}"), Value::Null);
    ex.set_exception(CamelError::illegal_argument("bad input").with_cause(CamelError::io("disk full")));
    assert_eq!(eval(&ex, "${exception.message}"), Value::from("bad input"));
    assert_eq!(eval(&ex, "${exception.kind}"), Value::from("IllegalArgumentException"));
    assert_eq!(eval(&ex, "${exception.class}"), Value::from("IllegalArgumentException"));
    assert_eq!(eval(&ex, "${exception.class.name}"), Value::from("IllegalArgumentException"));
    assert_eq!(eval(&ex, "${exception.cause.message}"), Value::from("disk full"));
    assert_eq!(eval(&ex, "${exception}"), Value::from("IllegalArgumentException: bad input (caused by IOException: disk full)"));
    assert_eq!(
        eval(&ex, "${exception.stacktrace}"),
        Value::from("IllegalArgumentException: bad input\nCaused by: IOException: disk full")
    );
    assert_eq!(eval(&ex, "Error: ${exception.message}"), Value::from("Error: bad input"));
    // caught exception stored as property (onException / doCatch)
    let mut ex = exchange(&ctx);
    ex.set_property(props::EXCEPTION_CAUGHT, Value::object([("kind", "IOException"), ("message", "oops")]));
    assert_eq!(eval(&ex, "${exception.message}"), Value::from("oops"));
    assert!(pred(&ex, "${exception.kind} == 'IOException'"));
}

#[test]
fn ids_random_uuid_env() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    assert_eq!(eval(&ex, "${exchangeId}"), Value::String(ex.exchange_id.clone()));
    assert_eq!(eval(&ex, "${id}"), Value::String(ex.message.message_id.clone()));
    assert_eq!(eval(&ex, "${messageId}"), Value::String(ex.message.message_id.clone()));
    for _ in 0..50 {
        let r = eval(&ex, "${random(10)}").as_i64().unwrap();
        assert!((0..10).contains(&r));
        let r = eval(&ex, "${random(5,8)}").as_i64().unwrap();
        assert!((5..8).contains(&r));
    }
    let u1 = eval(&ex, "${uuid}").to_string();
    let u2 = eval(&ex, "${uuid}").to_string();
    assert_eq!(u1.len(), 36);
    assert_ne!(u1, u2);
    let path = std::env::var("PATH").unwrap();
    assert_eq!(eval(&ex, "${env.PATH}"), Value::String(path.clone()));
    assert_eq!(eval(&ex, "${env:PATH}"), Value::String(path.clone()));
    assert_eq!(eval(&ex, "${sysenv.PATH}"), Value::String(path));
    assert_eq!(eval(&ex, "${env.SURELY_NOT_DEFINED_VAR_X}"), Value::Null);
    assert_eq!(eval(&ex, "${sys.line.separator}"), Value::from("\n"));
    let now = camel_core::util_time::now_millis();
    let d = eval(&ex, "${date:now}").as_i64().unwrap();
    assert!((d - now).abs() < 5000);
    let y = eval(&ex, "${date:now:yyyy}").as_i64().unwrap();
    assert!(y >= 2024);
    let m = eval(&ex, "${date:millis}").as_i64().unwrap();
    assert!((m - now).abs() < 5000);
    let created = eval(&ex, "${date:exchangeCreated}").as_i64().unwrap();
    assert_eq!(created, ex.created_millis());
    assert_eq!(eval(&ex, "${messageTimestamp}"), Value::Int(ex.created_millis()));
}

#[test]
fn predicates_table() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    let cases: Vec<(&str, bool)> = vec![
        ("${header.foo} == 'abc'", true),
        ("${header.foo} == abc", true),
        ("${header.foo} == \"abc\"", true),
        ("${header.foo} != 'abc'", false),
        ("${header.foo} =~ 'ABC'", true),
        ("${header.foo} !=~ 'ABC'", false),
        ("${header.num} == 10", true),
        ("${header.num} == '10'", true),
        ("${header.num} > 9", true),
        ("${header.num} > 10", false),
        ("${header.num} >= 10", true),
        ("${header.num} < 11", true),
        ("${header.num} <= 9", false),
        ("${header.num} > 9.5", true),
        ("${header.price} < 5", true),
        ("${header.num} > ${header.price}", true),
        ("${body} contains 'World'", true),
        ("${body} contains 'world'", false),
        ("${body} !contains 'world'", true),
        ("${body} not contains 'World'", false),
        ("${body} ~~ 'world'", true),
        ("${body} !~~ 'WORLD'", false),
        ("${header.list} contains 'b'", true),
        ("${header.list} contains 'z'", false),
        ("${header.foo} regex '[a-c]+'", true),
        ("${header.foo} regex 'a'", false),
        ("${header.foo} !regex '\\d+'", true),
        ("${header.foo} not regex '[a-c]{3}'", false),
        ("${header.foo} in 'x,abc,y'", true),
        ("${header.foo} in 'x,y'", false),
        ("${header.foo} !in 'x,y'", true),
        ("${header.foo} not in 'abc,y'", false),
        ("${header.num} in '1,10,100'", true),
        ("${header.foo} is 'String'", true),
        ("${header.foo} is String", true),
        ("${header.foo} is 'java.lang.String'", true),
        ("${header.num} is 'Integer'", true),
        ("${header.num} !is 'String'", true),
        ("${header.list} is 'List'", true),
        ("${header.order} is 'Map'", true),
        ("${header.num} range '1..10'", true),
        ("${header.num} range '11..20'", false),
        ("${header.num} !range '11..20'", true),
        ("${header.num} range 1..100", true),
        ("${body} startsWith 'Hello'", true),
        ("${body} starts with 'Hello'", true),
        ("${body} startsWith 'World'", false),
        ("${body} endsWith 'World'", true),
        ("${body} ends with 'Hello'", false),
        ("${header.missing} == null", true),
        ("${header.foo} == null", false),
        ("${header.missing} != null", false),
        ("${header.foo} != null", true),
        ("${header.missing} > 5", false),
        ("${header.flag} == true", true),
        ("${header.flag}", true),
        ("${header.missing}", false),
        ("${header.empty}", false),
        ("true", true),
        ("false", false),
        ("${header.num} > 5 && ${header.foo} == 'abc'", true),
        ("${header.num} > 5 && ${header.foo} == 'x'", false),
        ("${header.num} > 50 || ${header.foo} == 'abc'", true),
        ("${header.num} > 50 || ${header.foo} == 'x'", false),
        ("${header.num} > 5 and ${header.foo} == 'abc'", true),
        ("${header.num} > 50 or ${header.foo} == 'abc'", true),
        // && binds tighter than ||
        ("${header.num} > 50 && ${header.foo} == 'x' || ${header.flag} == true", true),
        ("${header.flag} == true || ${header.num} > 50 && ${header.foo} == 'x'", true),
        ("${header.flag} == false || ${header.num} > 50 && ${header.foo} == 'x'", false),
        ("${header.num} > 5 && ${header.num} < 20 && ${header.foo} startsWith 'a'", true),
        ("'${header.foo}' == 'abc'", true),
        ("${header.foo}x == 'abcx'", true),
        ("${body} == 'Hello World'", true),
        ("${body} == 'it\\'s'", false),
        ("${bodyAs(String).length()} == 11", true),
        ("${header.order.items[0].qty} == 2", true),
        ("${date:header.ts:yyyy} == 2024", true),
        ("${header.num}++ == 11", true),
    ];
    for (text, expected) in cases {
        assert_eq!(pred(&ex, text), expected, "predicate: {}", text);
    }
}

#[test]
fn syntax_errors_table() {
    let bad_exprs = [
        "${body",
        "${unknownFunction}",
        "${}",
        "Hello ${header.foo",
        "${bodyAs()}",
        "${random()}",
        "${substring(1,2,3,4)}",
        "${date:sometime:yyyy}",
        "${header.x > 5 ? 'a'}",
        "${iif(a,b)}",
        "${type:java.lang.Integer.MAX_VALUE}",
        "${trim(${body}}",
    ];
    for text in bad_exprs {
        let err = SimpleExpression::parse(text).err().unwrap_or_else(|| panic!("expected syntax error for {:?}", text));
        assert_eq!(err.kind, kinds::EXPRESSION_ILLEGAL_SYNTAX, "{}: {}", text, err);
        assert!(err.message.contains("at location"), "{}", err.message);
    }
    let bad_preds = [
        "${header.foo} ==",
        "== 'abc'",
        "${header.foo} == 'abc' &&",
        "${header.foo} == 'abc",
        "${header.foo} regex '[a-'",
        "${header.foo} == 'a' 'b'",
        "|| ${header.foo}",
    ];
    for text in bad_preds {
        let err = SimplePredicate::parse(text).err().unwrap_or_else(|| panic!("expected syntax error for predicate {:?}", text));
        assert_eq!(err.kind, kinds::EXPRESSION_ILLEGAL_SYNTAX, "{}: {}", text, err);
    }
    // position info
    let err = SimpleExpression::parse("abc ${nope}").err().unwrap();
    assert!(err.message.starts_with("Unknown function: nope at location 6"), "{}", err.message);
}

#[test]
fn evaluation_errors() {
    let ctx = ctx();
    let mut ex = exchange(&ctx);
    // mandatoryBodyAs with null body
    ex.set_body(Value::Null);
    let e = SimpleExpression::parse("${mandatoryBodyAs(String)}").unwrap().evaluate(&ex).unwrap_err();
    assert_eq!(e.kind, "InvalidPayloadException");
    // conversion failure
    ex.set_body("abc");
    let e = SimpleExpression::parse("${bodyAs(Integer)}").unwrap().evaluate(&ex).unwrap_err();
    assert!(e.is_a(kinds::NO_TYPE_CONVERSION_AVAILABLE));
    // missing property without default
    let e = SimpleExpression::parse("${properties:nope}").unwrap().evaluate(&ex).unwrap_err();
    assert!(e.is_a(kinds::ILLEGAL_ARGUMENT));
    // non-null-safe navigation through null
    let e = SimpleExpression::parse("${header.order.customer.address.street}").unwrap().evaluate(&ex).unwrap_err();
    assert_eq!(e.kind, kinds::NULL_POINTER);
    // unknown method
    let e = SimpleExpression::parse("${body.frobnicate()}").unwrap().evaluate(&ex).unwrap_err();
    assert_eq!(e.kind, "MethodNotFoundException");
    // unknown bean
    let e = SimpleExpression::parse("${bean:nobody}").unwrap().evaluate(&ex).unwrap_err();
    assert!(e.is_a(kinds::NO_SUCH_BEAN));
}

#[test]
fn raw_values_and_body_types() {
    let ctx = ctx();
    let mut ex = exchange(&ctx);
    ex.set_body(camel_core::json::parse(r#"{"a":{"b":[10,20]},"name":"n"}"#).unwrap());
    assert_eq!(eval(&ex, "${body.a.b[1]}"), Value::Int(20));
    assert_eq!(eval(&ex, "${body[name]}"), Value::from("n"));
    assert!(matches!(eval(&ex, "${body}"), Value::Object(_)));
    assert_eq!(eval(&ex, "${body.a.b}"), Value::from(vec![Value::Int(10), Value::Int(20)]));
    assert_eq!(eval(&ex, "x=${body.a.b}"), Value::from("x=[10,20]"));
    // JSON text body is navigable too
    ex.set_body(r#"{"k":"v"}"#);
    assert_eq!(eval(&ex, "${body.k}"), Value::from("v"));
    assert_eq!(eval(&ex, "${bodyAs(Map).k}"), Value::from("v"));
    assert_eq!(eval(&ex, "${prettyBody}"), Value::from("{\n  \"k\": \"v\"\n}"));
    // bytes
    ex.set_body(Value::Bytes(b"raw".to_vec()));
    assert_eq!(eval(&ex, "${body}"), Value::Bytes(b"raw".to_vec()));
    assert_eq!(eval(&ex, "${bodyAs(String)}"), Value::from("raw"));
    // multi-line body
    ex.set_body("a\nb\r\nc");
    assert_eq!(eval(&ex, "${bodyOneLine}"), Value::from("abc"));
    // numbers
    ex.set_body("42");
    assert_eq!(eval(&ex, "${bodyAs(Integer)}"), Value::Int(42));
    assert_eq!(eval(&ex, "${bodyAs(java.lang.Long)}"), Value::Int(42));
    assert!(pred(&ex, "${body} > 41"));
    assert!(pred(&ex, "${body} == 42"));
}

#[test]
fn simple_in_routes() {
    let ctx = ctx();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .route_id("simple-route")
            .set_header("size", simple("${body.length()}"))
            .set_body(simple("${routeId}: ${body} (${header.size}) ${header.size > 3 ? 'long' : 'short'}"))
            .to("mock:out");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:out").unwrap();
    mock.expected_bodies_received(vec!["simple-route: abcd (4) long", "simple-route: ab (2) short"]);
    let t = ctx.create_producer_template();
    t.send_body("direct:start", "abcd").unwrap();
    t.send_body("direct:start", "ab").unwrap();
    mock.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
}

#[test]
fn simple_predicate_via_language_resolution() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    let p = simple("${header.num} > 5").def.resolve_predicate(&ctx).unwrap();
    assert!(p.matches(&ex).unwrap());
    let p = simple("${header.num} > 50").def.resolve_predicate(&ctx).unwrap();
    assert!(!p.matches(&ex).unwrap());
    let e = simple("${unknown}").def.resolve_expression(&ctx).err().unwrap();
    assert_eq!(e.kind, kinds::EXPRESSION_ILLEGAL_SYNTAX);
    // placeholders are resolved before parsing
    let v = simple("{{app.name}}-${header.foo}").def.resolve_expression(&ctx).unwrap().evaluate(&ex).unwrap();
    assert_eq!(v, Value::from("demo-abc"));
}

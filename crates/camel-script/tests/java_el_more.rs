//! OGNL, MVEL and SpEL: static members, typed declarations, `with` blocks, bean references and
//! more operators.

mod common;
use common::*;

use camel_core::prelude::*;

#[test]
fn ognl_more() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "ognl",
        &ex,
        &[
            ("@java.lang.Math@max(1, 2)", "2"),
            ("@Math@abs(-3)", "3"),
            ("@Integer@MAX_VALUE", "2147483647"),
            ("@java.lang.Long@MIN_VALUE < 0", "true"),
            ("@java.lang.String@valueOf(5) + 'x'", "\"5x\""),
            ("@Integer@parseInt(request.headers.num) * 2", "10"),
            ("@Boolean@TRUE", "true"),
            ("@java.lang.Math@min(request.headers.num, 3)", "3"),
            ("request.body.substring(0, 5)", "\"Hello\""),
            ("request.body.split(' ').length", "2"),
            ("request.body.indexOf('World')", "6"),
            ("request.body.startsWith('Hello') ? 'yes' : 'no'", "\"yes\""),
            ("request.body.replace('World', 'There')", "\"Hello There\""),
            ("request.headers.containsKey('foo')", "true"),
            ("request.headers.size() >= 2", "true"),
            ("headers.foo.equals('bar')", "true"),
            ("exchange.getProperty('p1')", "\"v1\""),
            ("{1, 2, 3}.{$ #this < 3}", "2"),
            ("{3, 1, 2}.{#this + 1}", "[4,2,3]"),
            ("#{'k': 'v'}.k", "\"v\""),
            ("#{'a': 1, 'b': 2}.size()", "2"),
            ("#a = 2, #b = 3, #a * #b", "6"),
            ("1 lt 2 and 2 lte 2 and 3 gt 2 and 3 gte 3", "true"),
            ("1 neq 2", "true"),
            ("5 bor 2", "7"),
            ("6 band 3", "2"),
            ("5 xor 1", "4"),
            ("10 % 4", "2"),
            ("-request.headers.num", "-5"),
            ("request.body != null && request.body.length() > 5", "true"),
            ("{'a', 'b'}[1]", "\"b\""),
            ("'x' in {'x', 'y'}", "true"),
        ],
    );
}

#[test]
fn mvel_more() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "mvel",
        &ex,
        &[
            ("int x = 5; x * 2", "10"),
            ("String s = 'a'; s + 'b'", "\"ab\""),
            ("java.util.List l = [1, 2]; l.size()", "2"),
            ("double d = 1.5; d * 2", "3.0"),
            ("long n = 7; n / 2", "3"),
            ("m = ['a': 1]; with (m) { a = 5, b = 'x' }; m", "{\"a\":5,\"b\":\"x\"}"),
            ("m = ['n': 1]; with (m) { n += 2 }; m.n", "3"),
            ("x = 1; if (x > 0) { 'pos' } else { 'neg' }", "\"pos\""),
            ("s = 0; foreach (i : [1, 2, 3]) { s += i; } s", "6"),
            ("s = 0; for (i : [4, 5]) { s += i; } s", "9"),
            ("def twice(n) { n * 2 }; twice(21)", "42"),
            ("[1, 2, 3] contains 2", "true"),
            ("'' == empty", "true"),
            ("null == nil", "true"),
            ("'abc' is String", "true"),
            ("(name in [['name': 'a'], ['name': 'b']])", "[\"a\",\"b\"]"),
            ("request.headers.foo", "\"bar\""),
            ("request.body.toUpperCase()", "\"HELLO WORLD\""),
            ("headers.num * 3", "15"),
            ("exchange.properties.p1", "\"v1\""),
            ("7 / 2", "3"),
            ("x = 10; x -= 3; x", "7"),
            ("[1, 2, 3].size()", "3"),
            ("['a': 1, 'b': 2].b", "2"),
            ("{1, 2, 3}[2]", "3"),
            ("a = [3, 1, 2]; a[0] + a[2]", "5"),
            ("x = 4; x > 3 && x < 5", "true"),
            ("x = 4; x == 4 ? 'four' : 'other'", "\"four\""),
            ("Math.max(3, 8)", "8"),
            ("Integer.parseInt('12') + 1", "13"),
            ("'Hello'.length()", "5"),
            ("i = 0; while (i < 5) { i++; } i", "5"),
        ],
    );
}

#[test]
fn spel_more() {
    let ctx = ctx();
    ctx.registry().bind("greeting", Value::String("hi".into()));
    ctx.registry().bind("limits", camel_core::json::parse(r#"{"max": 10, "names": ["a", "b"]}"#).unwrap());
    camel_core::language::support::bind_bean_method(&ctx.registry(), "calc.twice", |ex: &Exchange| Ok(Value::Int(ex.message.body.as_i64().unwrap_or(0) * 2)));
    camel_core::language::support::bind_bean_method(&ctx.registry(), "calc.upper", |ex: &Exchange| Ok(Value::String(ex.message.body.to_string().to_uppercase())));
    let ex = exchange(&ctx);
    check(
        "spel",
        &ex,
        &[
            ("@greeting", "\"hi\""),
            ("@greeting.toUpperCase()", "\"HI\""),
            ("@limits.max + 1", "11"),
            ("@limits['names'][1]", "\"b\""),
            ("@limits.names.size()", "2"),
            ("@calc.twice(21)", "42"),
            ("@calc.upper(body)", "\"HELLO WORLD\""),
            ("@calc.twice(headers.num) > 9", "true"),
            ("#{@greeting + ' ' + body}", "\"hi Hello World\""),
            ("Hi #{@greeting}!", "\"Hi hi!\""),
            ("T(Math).max(1, 2)", "2"),
            ("T(java.lang.Integer).parseInt('7') * 2", "14"),
            ("T(Integer).MAX_VALUE", "2147483647"),
            ("new java.util.ArrayList().size()", "0"),
            ("body.length() > 5 ? 'long' : 'short'", "\"long\""),
            ("headers.foo ?: 'none'", "\"bar\""),
            ("headers.missing ?: 'none'", "\"none\""),
            ("headers?.missing?.length()", "null"),
            ("{1, 2, 3, 4}.?[#this > 2]", "[3,4]"),
            ("{1, 2, 3}.![#this * 10]", "[10,20,30]"),
            ("{1, 2, 3, 4}.^[#this > 1]", "2"),
            ("{1, 2, 3, 4}.$[#this < 3]", "2"),
            ("{a: 1, b: 2}['b']", "2"),
            ("'abc' matches '[a-c]+'", "true"),
            ("5 between {1, 10}", "true"),
            ("'x' instanceof T(String)", "true"),
            ("10 div 3 + 10 mod 3", "4"),
            ("1 eq 1 and 2 ne 3 and not false", "true"),
            ("1 lt 2 or 1 gt 2", "true"),
            ("#root.message.body", "\"Hello World\""),
            ("exchange.getProperty('p1')", "\"v1\""),
            ("message.getHeader('foo')", "\"bar\""),
        ],
    );
    check_err("spel", &ex, &[("@nope", "EL1058E"), ("@calc.missing()", "")]);
}

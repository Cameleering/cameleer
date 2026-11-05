//! DataSonnet language: table-driven tests (results are JSON strings, as with Camel's default
//! `application/json` output media type).

mod common;
use common::*;

use std::collections::BTreeMap;

use cameleer_core::prelude::*;

fn ds(ex: &Exchange, cases: &[(&str, &str)]) {
    let mut failures = Vec::new();
    for (src, expected) in cases {
        match cameleer_script::eval("datasonnet", src, ex) {
            Ok(Value::String(s)) if s == *expected => {}
            Ok(other) => failures.push(format!("`{}`\n    expected {}\n    got      {}", src, expected, other)),
            Err(e) => failures.push(format!("`{}`\n    expected {}\n    error    {}", src, expected, e.message)),
        }
    }
    if !failures.is_empty() {
        panic!("{} of {} datasonnet cases failed:\n{}", failures.len(), cases.len(), failures.join("\n"));
    }
}

#[test]
fn datasonnet_core_language() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    ds(
        &ex,
        &[
            ("1 + 2", "3"),
            ("7 / 2", "3.5"),
            ("'a' + 'b' + 1", "\"ab1\""),
            ("{b: 2, a: 1}", "{\"a\":1,\"b\":2}"),
            ("{a: 1} + {b: 2, a: 3}", "{\"a\":3,\"b\":2}"),
            ("[1, 2] + [3]", "[1,2,3]"),
            ("local x = 5; x * x", "25"),
            ("local f(a, b=2) = a * b; [f(3), f(3, 3)]", "[6,9]"),
            ("local add = function(a, b) a + b; add(1, 2)", "3"),
            ("if 3 > 2 then 'yes' else 'no'", "\"yes\""),
            ("if false then 1", "null"),
            ("[x * 2 for x in [1, 2, 3] if x != 2]", "[2,6]"),
            ("{[k]: k + '!' for k in ['a', 'b']}", "{\"a\":\"a!\",\"b\":\"b!\"}"),
            ("{local base = 10, x: base, y: base * 2}", "{\"x\":10,\"y\":20}"),
            ("'%s has %d items' % ['cart', 3]", "\"cart has 3 items\""),
            ("'%05.2f' % 3.14159", "\"03.14\""),
            ("[1, 2, 3, 4][1:3]", "[2,3]"),
            ("'abc'[1]", "\"b\""),
            ("'a' in {a: 1}", "true"),
            ("local fact(n) = if n <= 1 then 1 else n * fact(n - 1); fact(10)", "3628800"),
            ("|||\n  line one\n  line two\n|||", "\"line one\\nline two\\n\""),
            ("/** DataSonnet\nversion=2.0\n*/\n{ ok: true } // comment", "{\"ok\":true}"),
            ("{a: {b: {c: 'deep'}}}.a.b.c", "\"deep\""),
            ("{f(x): x + 1}.f(1)", "2"),
            ("assert 1 < 2 : 'math'; 'ok'", "\"ok\""),
        ],
    );
}

#[test]
fn datasonnet_std_library() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    ds(
        &ex,
        &[
            ("std.length([1, 2, 3]) + std.length('ab') + std.length({a: 1})", "6"),
            ("std.map(function(x) x * 10, [1, 2])", "[10,20]"),
            ("std.filter(function(x) x > 1, [1, 2, 3])", "[2,3]"),
            ("std.join(', ', ['a', 'b'])", "\"a, b\""),
            ("std.objectFields({b: 1, a: 2})", "[\"a\",\"b\"]"),
            ("std.toString(42) + std.toString([1])", "\"42[1]\""),
            ("std.parseJson('{\"x\": [1, 2]}').x[1]", "2"),
            ("std.manifestJsonEx({a: [1, 2]}, '  ')", "\"{\\n  \\\"a\\\": [\\n    1,\\n    2\\n  ]\\n}\""),
            ("std.foldl(function(acc, x) acc + x, [1, 2, 3], 0)", "6"),
            ("std.range(1, 4)", "[1,2,3,4]"),
            ("std.split('a/b/c', '/')", "[\"a\",\"b\",\"c\"]"),
            ("std.asciiUpper('abc') + std.asciiLower('DEF')", "\"ABCdef\""),
            ("std.strReplace('a-b-c', '-', '+')", "\"a+b+c\""),
            ("std.substr('camel', 1, 3)", "\"ame\""),
            ("std.startsWith('camel', 'ca') && std.endsWith('camel', 'el')", "true"),
            ("std.sort([3, 1, 2])", "[1,2,3]"),
            ("std.uniq([1, 1, 2, 2, 3])", "[1,2,3]"),
            ("std.sort([{n: 2}, {n: 1}], function(o) o.n)", "[{\"n\":1},{\"n\":2}]"),
            ("std.type([]) + std.type({}) + std.type('s') + std.type(1) + std.type(null)", "\"arrayobjectstringnumbernull\""),
            ("std.objectHas({a: 1}, 'a') && !std.objectHas({a: 1}, 'b')", "true"),
            ("std.format('%s-%s', ['x', 'y'])", "\"x-y\""),
            ("std.flatMap(function(x) [x, x], [1, 2])", "[1,1,2,2]"),
            ("std.mapWithKey(function(k, v) v * 2, {a: 1, b: 2})", "{\"a\":2,\"b\":4}"),
            ("std.member([1, 2], 2) && std.member('abc', 'b')", "true"),
            ("std.abs(-3) + std.max(2, 5) + std.floor(2.7)", "10"),
            ("std.base64('camel')", "\"Y2FtZWw=\""),
            ("std.mergePatch({a: 1, b: 2}, {b: null, c: 3})", "{\"a\":1,\"c\":3}"),
            ("std.makeArray(3, function(i) i * i)", "[0,1,4]"),
            ("std.trim('  x  ')", "\"x\""),
        ],
    );
}

#[test]
fn datasonnet_payload_and_cml() {
    let ctx = ctx();
    let mut ex = order_exchange(&ctx);
    // the body as a JSON string (bodyMediaType application/json)
    ex.message.body = Value::String(ex.message.body.to_json());
    ds(
        &ex,
        &[
            ("payload.customer.name", "\"Alice\""),
            ("{id: payload.id, skus: [i.sku for i in payload.items]}", "{\"id\":7,\"skus\":[\"A1\",\"B2\",\"C3\"]}"),
            ("std.foldl(function(t, i) t + i.qty * i.price, payload.items, 0)", "30.25"),
            ("{name: payload.customer.name, foo: cml.header('foo'), p: cml.exchangeProperty('p1'), v: cml.variable('v1')}", "{\"foo\":\"bar\",\"name\":\"Alice\",\"p\":\"v1\",\"v\":\"x\"}"),
            ("[i for i in payload.items if i.qty > 1]", "[{\"price\":10.5,\"qty\":2,\"sku\":\"A1\"},{\"price\":1.25,\"qty\":5,\"sku\":\"C3\"}]"),
            ("ds.upper(payload.customer.name)", "\"ALICE\""),
            ("ds.map(payload.tags, function(t) t + '!')", "[\"new!\",\"web!\"]"),
        ],
    );
    // output media type: Java object and plain text
    let mut opts = BTreeMap::new();
    opts.insert("outputMediaType".to_string(), "application/x-java-object".to_string());
    let s = cameleer_script::Script::compile(cameleer_script::Dialect::Jsonnet, "{n: payload.id + 1}", &opts).unwrap();
    use cameleer_core::spi::Expression;
    assert_eq!(s.evaluate(&ex).unwrap().to_json(), "{\"n\":8}");
    let mut opts = BTreeMap::new();
    opts.insert("outputMediaType".to_string(), "text/plain".to_string());
    let s = cameleer_script::Script::compile(cameleer_script::Dialect::Jsonnet, "'id=' + payload.id", &opts).unwrap();
    assert_eq!(s.evaluate(&ex).unwrap().to_string(), "id=7");
    let mut opts = BTreeMap::new();
    opts.insert("resultType".to_string(), "Integer".to_string());
    let s = cameleer_script::Script::compile(cameleer_script::Dialect::Jsonnet, "payload.id * 2", &opts).unwrap();
    assert_eq!(s.evaluate(&ex).unwrap().to_json(), "14");
}

#[test]
fn datasonnet_errors() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check_err(
        "datasonnet",
        &ex,
        &[
            ("{a: }", "DataSonnet syntax error at line 1, column 5"),
            ("local x = 1;\nerror 'custom ' + x", "RUNTIME ERROR: custom 1 (line 2, column 1)"),
            ("unknownVar", "Unknown variable: unknownVar"),
            ("{a: 1}.b", "field does not exist: b"),
            ("[1][3]", "array bounds error"),
            ("1 + {}", "does not operate on number and object"),
            ("assert false : 'failed check'; 1", "failed check"),
            ("std.nope(1)", "field does not exist: nope"),
        ],
    );
}

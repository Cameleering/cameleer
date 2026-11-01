//! DataSonnet: `self` / `super` / `$`, hidden fields, `+:`, local imports, media types and
//! time-zone aware `ds.datetime`.

mod common;
use common::*;

use std::collections::BTreeMap;

use camel_core::prelude::*;
use camel_core::spi::Expression;

fn ds(ex: &Exchange, cases: &[(&str, &str)]) {
    let mut failures = Vec::new();
    for (src, expected) in cases {
        match camel_script::eval("datasonnet", src, ex) {
            Ok(Value::String(s)) if s == *expected => {}
            Ok(other) => failures.push(format!("`{}`\n    expected {}\n    got      {}", src, expected, other)),
            Err(e) => failures.push(format!("`{}`\n    expected {}\n    error    {}", src, expected, e.message)),
        }
    }
    if !failures.is_empty() {
        panic!("{} of {} datasonnet cases failed:\n{}", failures.len(), cases.len(), failures.join("\n"));
    }
}

fn eval_with(ex: &Exchange, src: &str, opts: &[(&str, &str)]) -> camel_core::error::Result<Value> {
    let o: BTreeMap<String, String> = opts.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    camel_script::Script::compile(camel_script::Dialect::Jsonnet, src, &o)?.evaluate(ex)
}

#[test]
fn datasonnet_objects() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    ds(
        &ex,
        &[
            ("{a: 1, b: self.a + 1}", "{\"a\":1,\"b\":2}"),
            ("{a: 1} + {b: super.a}", "{\"a\":1,\"b\":1}"),
            ("{a: 1} + {a: super.a + 10}", "{\"a\":11}"),
            ("{x: {y: $.z}, z: 3}", "{\"x\":{\"y\":3},\"z\":3}"),
            ("{a:: 1, b: self.a}", "{\"b\":1}"),
            ("{a:: 1} + {a::: 2}", "{\"a\":2}"),
            ("{a: 1} + {a:: 5}", "{}"),
            ("{a: 1} + {a+: 2}", "{\"a\":3}"),
            ("{a: [1]} + {a+: [2]}", "{\"a\":[1,2]}"),
            ("{a: {b: 1}} + {a+: {c: 2}}", "{\"a\":{\"b\":1,\"c\":2}}"),
            ("local base = {name: 'x', greet: 'hi ' + self.name}; base + {name: 'y'}", "{\"greet\":\"hi y\",\"name\":\"y\"}"),
            ("local o = {h:: 'x'}; o.h", "\"x\""),
            ("{f(x):: x * 2, g: self.f(3)}", "{\"g\":6}"),
            ("{local l = 5, v: l * self.w, w: 2}", "{\"v\":10,\"w\":2}"),
            ("std.objectFields({a:: 1, b: 2})", "[\"b\"]"),
            ("std.objectFieldsAll({a:: 1, b: 2})", "[\"a\",\"b\"]"),
            ("std.objectHas({a:: 1}, 'a')", "false"),
            ("std.objectHasAll({a:: 1}, 'a')", "true"),
            ("{x: 1} + {y: 'x' in super}", "{\"x\":1,\"y\":true}"),
            ("local Person = {name:: error 'name required', hello: 'Hi ' + self.name}; Person + {name: 'Ann'}", "{\"hello\":\"Hi Ann\"}"),
            ("local a = {v: 1, w: self.v * 10}; local b = a + {v: 2}; [a.w, b.w]", "[10,20]"),
            ("{['k' + i]: i for i in [1, 2]}", "{\"k1\":1,\"k2\":2}"),
            ("std.mergePatch({a: 1, b: {c: 2}}, {b: {c: null, d: 3}})", "{\"a\":1,\"b\":{\"d\":3}}"),
        ],
    );
    let e = camel_script::eval("datasonnet", "{assert self.a > 0 : 'a must be positive', a: -1}", &ex).unwrap_err();
    assert!(e.message.contains("a must be positive"), "{}", e.message);
    let e = camel_script::eval("datasonnet", "local P = {name:: error 'name required', hello: 'Hi ' + self.name}; P", &ex).unwrap_err();
    assert!(e.message.contains("name required"), "{}", e.message);
}

#[test]
fn datasonnet_imports() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    ds(
        &ex,
        &[
            (&format!("local lib = import '{}/lib.libsonnet'; lib.greet('Bob')", dir), "\"Hello Bob\""),
            (&format!("(import '{}/lib.libsonnet').version", dir), "2"),
            (&format!("importstr '{}/note.txt'", dir), "\"plain text\\n\""),
            (&format!("std.length(importbin '{}/note.txt')", dir), "11"),
            (&format!("(import '{}/lib.libsonnet') + {{version: 3}}", dir), "{\"version\":3}"),
        ],
    );
    let e = camel_script::eval("datasonnet", "import 'does/not/exist.libsonnet'", &ex).unwrap_err();
    assert!(e.message.contains("couldn't open import"), "{}", e.message);
}

#[test]
fn datasonnet_datetime() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    ds(
        &ex,
        &[
            ("ds.datetime.format('2024-01-01T10:00:00Z', 'yyyy-MM-dd')", "\"2024-01-01\""),
            ("ds.datetime.changeTimeZone('2024-07-01T10:00:00Z', 'Europe/Paris')", "\"2024-07-01T12:00:00+02:00\""),
            ("ds.datetime.changeTimeZone('2024-01-01T10:00:00Z', 'Europe/Paris')", "\"2024-01-01T11:00:00+01:00\""),
            ("ds.datetime.changeTimeZone('2024-01-01T10:00:00Z', 'America/New_York')", "\"2024-01-01T05:00:00-05:00\""),
            ("ds.datetime.changeTimeZone('2024-01-01T10:00:00Z', '+05:30')", "\"2024-01-01T15:30:00+05:30\""),
            ("ds.datetime.parse('12/31/2023 23:59', 'MM/dd/yyyy HH:mm')", "\"2023-12-31T23:59:00Z\""),
            ("ds.datetime.parse('1700000000', 'unix')", "\"2023-11-14T22:13:20Z\""),
            ("ds.datetime.plus('2024-01-31T00:00:00Z', 'P1M')", "\"2024-02-29T00:00:00Z\""),
            ("ds.datetime.minus('2024-03-01T00:00:00+02:00', 'PT2H30M')", "\"2024-02-29T21:30:00+02:00\""),
            ("ds.datetime.compare('2024-01-01T00:00:00Z', '2024-01-01T01:00:00+02:00')", "1"),
            ("ds.datetime.toLocalDate('2024-05-06T23:30:00-05:00')", "\"2024-05-06\""),
            ("ds.datetime.toLocalTime('2024-05-06T23:30:00-05:00')", "\"23:30:00\""),
            ("ds.datetime.isLeapYear('2024-02-01T00:00:00Z')", "true"),
            ("std.length(ds.datetime.now()) >= 20", "true"),
            ("ds.datetime.atBeginningOfMonth('2024-05-17T13:45:10+01:00')", "\"2024-05-01T00:00:00+01:00\""),
            ("ds.datetime.daysBetween('2024-01-01T00:00:00Z', '2024-03-01T00:00:00Z')", "60"),
            ("ds.datetime.format(ds.datetime.changeTimeZone('2024-07-01T10:00:00.250Z', 'Europe/Paris'), 'HH:mm:ss.SSS XXX')", "\"12:00:00.250 +02:00\""),
        ],
    );
}

#[test]
fn datasonnet_media_types() {
    let ctx = ctx();
    let mut ex = exchange(&ctx);
    ex.message.body = Value::String("<order id=\"7\"><item>a</item><item>b</item></order>".into());
    let v = eval_with(&ex, "payload.order['@id'] + ':' + std.join(',', [i['$'] for i in payload.order.item])", &[("bodyMediaType", "application/xml"), ("outputMediaType", "text/plain")]).unwrap();
    assert_eq!(v.to_string(), "7:a,b");
    ex.message.body = Value::String("name,qty\nA,2\nB,5\n".into());
    let v = eval_with(&ex, "std.foldl(function(t, r) t + std.parseInt(r.qty), payload, 0)", &[("bodyMediaType", "application/csv")]).unwrap();
    assert_eq!(v.to_string(), "7");
    let v = eval_with(&ex, "[{sku: r.name, n: r.qty} for r in payload]", &[("bodyMediaType", "application/csv"), ("outputMediaType", "application/csv")]).unwrap();
    assert_eq!(v.to_string(), "n,sku\n2,A\n5,B\n");
    ex.message.body = Value::String("a=1&b=x%20y".into());
    let v = eval_with(&ex, "payload.b", &[("bodyMediaType", "application/x-www-form-urlencoded"), ("outputMediaType", "text/plain")]).unwrap();
    assert_eq!(v.to_string(), "x y");
    let v = eval_with(&ex, "{root: {'@a': '1', child: 'text'}}", &[("outputMediaType", "application/xml")]).unwrap();
    assert_eq!(v.to_string(), "<root a=\"1\"><child>text</child></root>");
    let v = eval_with(&ex, "{q: 'a b', n: 1}", &[("outputMediaType", "application/x-www-form-urlencoded")]).unwrap();
    assert_eq!(v.to_string(), "n=1&q=a+b");
}

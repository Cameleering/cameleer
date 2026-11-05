//! XQuery 3.1 maps / arrays / higher-order functions, library modules, decimals,
//! date/duration arithmetic and the update facility.

use cameleer_core::value::Value;
use cameleer_xml::xquery::{register_module, serialize_items, XQuery};

fn run_on(q: &str, xml: Option<&str>) -> String {
    let xq = XQuery::compile(q, &[]).unwrap_or_else(|e| panic!("compile {}: {}", q, e));
    let ctx = xml.map(Value::from);
    xq.evaluate(ctx.as_ref(), &|_, _| None, |d, items| serialize_items(d, items)).unwrap_or_else(|e| panic!("eval {}: {}", q, e))
}

fn run(q: &str) -> String {
    run_on(q, None)
}

#[test]
fn maps_arrays_and_lookups() {
    let cases: &[(&str, &str)] = &[
        (r#"map { "a": 1, "b": (2, 3) }?b"#, "2 3"),
        (r#"let $m := map { "a": 1, "b": 2 } return $m("a") + $m?b"#, "3"),
        (r#"map:size(map:put(map { 1: "x" }, 2, "y"))"#, "2"),
        (r#"let $m := map:merge((map { "a": 1 }, map { "a": 2, "b": 3 })) return ($m?a, $m?b)"#, "1 3"),
        (r#"map:contains(map:remove(map { "a": 1, "b": 2 }, "a"), "a")"#, "false"),
        (r#"sort(map:keys(map { "z": 1, "a": 2 }))"#, "a z"),
        (r#"[1, (2, 3), 4]?2"#, "2 3"),
        (r#"array:size([1, (2, 3), 4])"#, "3"),
        (r#"array { 1 to 3 }?*"#, "1 2 3"),
        (r#"array:flatten([1, [2, [3]]])"#, "1 2 3"),
        (r#"[1, 2, 3] => array:reverse() => array:head()"#, "3"),
        (r#"array:join(([1], [2, 3]))?3"#, "3"),
        (r#"array:sort([3, 1, 2])?*"#, "1 2 3"),
        (r#"map { "a": [1, map { "b": "x" }] }?a?2?b"#, "x"),
        (r#"serialize(map { "a": [1, "s"] })"#, r#"map{"a":[1,"s"]}"#),
        (r#"[1, 2] instance of array(*)"#, "true"),
        (r#"map {} instance of map(*)"#, "true"),
        (r#"for $x in (map { "k": 1 }, map { "k": 2 }) return $x?k"#, "1 2"),
        (r#"array:fold-left([1, 2, 3], 0, function($a, $b) { $a + $b })"#, "6"),
        (r#"map:for-each(map { "a": 1 }, function($k, $v) { $k || '=' || $v })"#, "a=1"),
    ];
    for (q, want) in cases {
        assert_eq!(run(q), *want, "query: {}", q);
    }
}

#[test]
fn higher_order_functions() {
    let cases: &[(&str, &str)] = &[
        ("for-each(1 to 3, function($x) { $x * $x })", "1 4 9"),
        ("filter(1 to 10, function($x) { $x mod 3 = 0 })", "3 6 9"),
        ("fold-left(1 to 5, 0, function($acc, $x) { $acc + $x })", "15"),
        ("fold-right(('a', 'b', 'c'), '', function($x, $acc) { $acc || $x })", "cba"),
        ("for-each-pair((1, 2), (10, 20), function($a, $b) { $a + $b })", "11 22"),
        ("let $f := upper-case#1 return $f('abc')", "ABC"),
        ("let $add := function($a, $b) { $a + $b }, $inc := $add(1, ?) return $inc(41)", "42"),
        ("let $f := substring(?, 2) return $f('hello')", "ello"),
        ("sort((3, 1, 2), (), function($x) { -$x })", "3 2 1"),
        ("apply(concat#3, ['a', 'b', 'c'])", "abc"),
        ("function-arity(function($a, $b) { $a })", "2"),
        ("local-name-from-QName(function-name(count#1))", "count"),
        ("declare function local:twice($f, $x) { $f($f($x)) }; local:twice(function($n) { $n + 10 }, 1)", "21"),
        ("declare function local:sq($x) { $x * $x }; for-each(1 to 3, local:sq#1)", "1 4 9"),
        ("let $compose := function($f, $g) { function($x) { $g($f($x)) } } return $compose(string-length#1, function($n) { $n * 2 })('abcd')", "8"),
        ("'hello' => upper-case() => substring(1, 3)", "HEL"),
        ("let $y := 5 return (function($x) { $x + $y })(1)", "6"),
        ("exists(function-lookup(xs:QName('fn:count'), 1))", "true"),
    ];
    for (q, want) in cases {
        assert_eq!(run(q), *want, "query: {}", q);
    }
}

#[test]
fn arbitrary_precision_decimals() {
    let cases: &[(&str, &str)] = &[
        ("0.1 + 0.2", "0.3"),
        ("12345678901234567890.123456789 * 10", "123456789012345678901.23456789"),
        ("1 div 3", "0.333333333333333333333333"),
        ("1.0 div 8", "0.125"),
        ("99999999999999999999.99 + 0.01", "100000000000000000000"),
        ("round-half-to-even(2.5)", "2"),
        ("round-half-to-even(1.2345, 3)", "1.234"),
        ("xs:decimal('1.10') = 1.1", "true"),
        ("floor(-1.5)", "-2"),
        ("10.5 mod 3", "1.5"),
        ("7.5 idiv 2", "3"),
    ];
    for (q, want) in cases {
        assert_eq!(run(q), *want, "query: {}", q);
    }
}

#[test]
fn date_and_duration_arithmetic() {
    let cases: &[(&str, &str)] = &[
        ("xs:dateTime('2024-03-01T12:00:00Z') - xs:dateTime('2024-02-28T10:30:00Z')", "P2DT1H30M"),
        ("xs:date('2024-01-31') + xs:yearMonthDuration('P1M')", "2024-02-29"),
        ("xs:date('2024-03-10') - xs:date('2024-03-01')", "P9D"),
        ("xs:dateTime('2024-01-01T00:00:00') + xs:dayTimeDuration('PT36H')", "2024-01-02T12:00:00"),
        ("xs:dayTimeDuration('PT90M')", "PT1H30M"),
        ("xs:dayTimeDuration('PT1H') * 2.5", "PT2H30M"),
        ("xs:yearMonthDuration('P2Y') div xs:yearMonthDuration('P6M')", "4"),
        ("days-from-duration(xs:dayTimeDuration('P3DT4H'))", "3"),
        ("hours-from-duration(xs:dayTimeDuration('P3DT4H'))", "4"),
        ("xs:dayTimeDuration('PT60M') eq xs:dayTimeDuration('PT1H')", "true"),
        ("xs:dateTime('2024-01-01T10:00:00+02:00') = xs:dateTime('2024-01-01T08:00:00Z')", "true"),
        ("adjust-dateTime-to-timezone(xs:dateTime('2024-01-01T10:00:00+02:00'), xs:dayTimeDuration('-PT5H'))", "2024-01-01T03:00:00-05:00"),
        ("dateTime(xs:date('2024-05-06'), xs:time('07:08:09'))", "2024-05-06T07:08:09"),
        ("xs:time('23:30:00') + xs:dayTimeDuration('PT1H')", "00:30:00"),
    ];
    for (q, want) in cases {
        assert_eq!(run(q), *want, "query: {}", q);
    }
}

#[test]
fn library_modules() {
    let dir = std::env::temp_dir().join(format!("camel-xq-lib-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("util.xqm"),
        r#"module namespace u = "urn:util";
           declare variable $u:greeting := "Hello";
           declare function u:greet($n as xs:string) as xs:string { $u:greeting || ", " || $n };
           declare function u:fact($n) { if ($n le 1) then 1 else $n * u:fact($n - 1) };"#,
    )
    .unwrap();
    std::fs::write(dir.join("main.xq"), r#"import module namespace u = "urn:util" at "util.xqm"; (u:greet("World"), u:fact(5))"#).unwrap();
    let q = XQuery::compile_file(&dir.join("main.xq"), &[]).unwrap();
    let out = q.evaluate(None, &|_, _| None, |d, items| serialize_items(d, items)).unwrap();
    assert_eq!(out, "Hello, World 120");
    // registered modules (no location hint)
    register_module("urn:reg", r#"module namespace r = "urn:reg"; declare function r:twice($x) { 2 * $x };"#);
    assert_eq!(run(r#"import module namespace r = "urn:reg"; r:twice(21)"#), "42");
    // errors
    assert!(XQuery::compile(r#"import module namespace x = "urn:none"; 1"#, &[]).is_err());
    assert!(XQuery::compile(r#"module namespace m = "urn:m"; declare function m:f() { 1 };"#, &[]).is_err());
    assert!(XQuery::compile_file(&dir.join("util.xqm"), &[]).is_err());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn update_facility() {
    let xml = r#"<r><a id="1">x</a><b>y</b><c/></r>"#;
    let cases: &[(&str, &str)] = &[
        ("insert node <n/> into /r", r#"<r><a id="1">x</a><b>y</b><c/><n/></r>"#),
        ("insert node <n/> as first into /r", r#"<r><n/><a id="1">x</a><b>y</b><c/></r>"#),
        ("insert nodes (<n/>, <m/>) before /r/b", r#"<r><a id="1">x</a><n/><m/><b>y</b><c/></r>"#),
        ("insert node attribute z { 'q' } into /r/c", r#"<r><a id="1">x</a><b>y</b><c z="q"/></r>"#),
        ("delete node /r/b", r#"<r><a id="1">x</a><c/></r>"#),
        ("delete nodes /r/*[not(self::a)]", r#"<r><a id="1">x</a></r>"#),
        ("replace node /r/b with <B>Y</B>", r#"<r><a id="1">x</a><B>Y</B><c/></r>"#),
        ("replace value of node /r/a with 'new'", r#"<r><a id="1">new</a><b>y</b><c/></r>"#),
        ("replace value of node /r/a/@id with 42", r#"<r><a id="42">x</a><b>y</b><c/></r>"#),
        ("rename node /r/c as 'd'", r#"<r><a id="1">x</a><b>y</b><d/></r>"#),
        ("(rename node /r/a as 'z', insert node <e/> after /r/a)", r#"<r><z id="1">x</z><e/><b>y</b><c/></r>"#),
    ];
    for (q, want) in cases {
        assert_eq!(run_on(q, Some(xml)), *want, "query: {}", q);
    }
    // copy / modify / return leaves the input unchanged
    let out = run_on("let $in := /r return (copy $c := $in modify (delete node $c/a, insert node <z/> into $c) return $c, count(/r/a))", Some(xml));
    assert_eq!(out, r#"<r><b>y</b><c/><z/></r>1"#);
    let out = run_on("copy $c := <x><y/></x> modify rename node $c/y as 'w' return $c", None);
    assert_eq!(out, "<x><w/></x>");
}

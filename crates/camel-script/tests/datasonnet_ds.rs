//! DataSonnet: the `ds` library modules, `std.extVar`, top-level arguments and the XML plugin
//! options.

mod common;
use common::*;

use std::collections::BTreeMap;

use camel_core::prelude::*;

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

fn run(src: &str, opts: &[(&str, &str)], ex: &Exchange) -> Result<Value> {
    let options: BTreeMap<String, String> = opts.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    camel_script::Script::compile(camel_script::Dialect::Jsonnet, src, &options)?.evaluate_copy(ex)
}

#[test]
fn ds_core_functions() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    ds(
        &ex,
        &[
            ("ds.map([1, 2, 3], function(x, i) x * 10 + i)", "[10,21,32]"),
            ("ds.filter([1, 2, 3, 4], function(x) x % 2 == 0)", "[2,4]"),
            ("ds.filterObject({a: 1, b: 2}, function(v, k) k == 'b')", "{\"b\":2}"),
            ("ds.mapObject({a: 1, b: 2}, function(v, k) {[k + 'x']: v * 2})", "{\"ax\":2,\"bx\":4}"),
            ("ds.mapEntries({a: 1, b: 2}, function(v, k, i) k + v + i)", "[\"a10\",\"b21\"]"),
            ("ds.entriesOf({a: 1})", "[{\"key\":\"a\",\"value\":1}]"),
            ("ds.flatMap([[1, 2], [3]], function(x) x)", "[1,2,3]"),
            ("ds.foldLeft([1, 2, 3], 10, function(curr, acc) acc + curr)", "16"),
            ("ds.reduce([1, 2, 3, 4], function(curr, acc) acc * curr)", "24"),
            ("ds.groupBy(['apple', 'avocado', 'banana'], function(s) s[0])", "{\"a\":[\"apple\",\"avocado\"],\"b\":[\"banana\"]}"),
            ("ds.orderBy([{n: 3}, {n: 1}, {n: 2}], function(x) x.n)", "[{\"n\":1},{\"n\":2},{\"n\":3}]"),
            ("ds.distinctBy([1, 2, 1, 3, 2], function(x) x)", "[1,2,3]"),
            ("[ds.maxBy([{a: 1}, {a: 5}], function(x) x.a).a, ds.minBy([{a: 1}, {a: 5}], function(x) x.a).a, ds.max([3, 9, 2]), ds.min([3, 9, 2])]", "[5,1,9,2]"),
            ("[ds.combine('a', 1), ds.combine([1], [2]), ds.combine({a: 1}, {b: 2})]", "[\"a1\",[1,2],{\"a\":1,\"b\":2}]"),
            ("[ds.contains([1, 2], 2), ds.contains('hello', 'ell'), ds.startsWith('Hello', 'he'), ds.endsWith('Hello', 'LO')]", "[true,true,true,true]"),
            ("[ds.find('banana', 'an'), ds.find([1, 2, 1], 1)]", "[[1,3],[0,2]]"),
            ("[ds.isBlank('  '), ds.isEmpty([]), ds.isEven(4), ds.isOdd(3), ds.isInteger(2.0), ds.isDecimal(2.5)]", "[true,true,true,true,true,true]"),
            ("[ds.joinBy(['a', 'b'], '-'), ds.splitBy('a,b,,c', ','), ds.splitBy('a1b22c', '[0-9]+')]", "[\"a-b\",[\"a\",\"b\",\"\",\"c\"],[\"a\",\"b\",\"c\"]]"),
            ("[ds.match('2024-01-05', '(\\\\d+)-(\\\\d+)-(\\\\d+)'), ds.matches('abc', '[a-c]+'), ds.scan('a1 b2', '([a-z])(\\\\d)')]", "[[\"2024-01-05\",\"2024\",\"01\",\"05\"],true,[[\"a1\",\"a\",\"1\"],[\"b2\",\"b\",\"2\"]]]"),
            ("[ds.replace('a1b2', '[0-9]', '#'), ds.upper('ab'), ds.lower('AB'), ds.trim(' x ')]", "[\"a#b#\",\"AB\",\"ab\",\"x\"]"),
            ("[ds.parseInt('42'), ds.parseDouble('2.5'), ds.parseHex('ff'), ds.parseOctal('17')]", "[42,2.5,255,15]"),
            ("[ds.range(1, 4), ds.append([1], 2), ds.prepend([1], 0), ds.flatten([[1], [2, [3]]])]", "[[1,2,3,4],[1,2],[0,1],[1,2,[3]]]"),
            ("[ds.remove({a: 1, b: 2}, 'a'), ds.remove([1, 2, 3], 2), ds.removeMatch({a: 1, b: 2}, {a: 1})]", "[{\"b\":2},[1,3],{\"b\":2}]"),
            ("[ds.select({a: {b: {c: 7}}}, 'a.b.c'), ds.sizeOf('héllo'), ds.typeOf([]), ds.typeOf(null)]", "[7,5,\"array\",\"null\"]"),
            ("[ds.zip([1, 2], ['a', 'b']), ds.unzip([[1, 'a'], [2, 'b']])]", "[[[1,\"a\"],[2,\"b\"]],[[1,2],[\"a\",\"b\"]]]"),
            ("[ds.keysOf({b: 1, a: 2}), ds.valuesOf({b: 1, a: 2})]", "[[\"a\",\"b\"],[2,1]]"),
            ("std.length(ds.uuid())", "36"),
            ("ds.read('{\"a\": [1, 2]}', 'application/json').a[1]", "2"),
            ("ds.write({a: 1}, 'application/json')", "\"{\\\"a\\\":1}\""),
            ("ds.read('a;b\\n1;2', 'application/csv', {separator: ';'})", "[{\"a\":\"1\",\"b\":\"2\"}]"),
        ],
    );
}

#[test]
fn ds_modules() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    ds(
        &ex,
        &[
            ("[ds.arrays.countBy([1, 2, 3], function(x) x > 1), ds.arrays.deepFlatten([1, [2, [3, [4]]]]), ds.arrays.divideBy([1, 2, 3, 4, 5], 2)]", "[2,[1,2,3,4],[[1,2],[3,4],[5]]]"),
            ("[ds.arrays.drop([1, 2, 3], 1), ds.arrays.take([1, 2, 3], 2), ds.arrays.dropWhile([1, 2, 5, 1], function(x) x < 3), ds.arrays.takeWhile([1, 2, 5, 1], function(x) x < 3)]", "[[2,3],[1,2],[5,1],[1,2]]"),
            ("[ds.arrays.duplicates([1, 2, 1, 3, 3]), ds.arrays.every([2, 4], function(x) x % 2 == 0), ds.arrays.some([1, 3], function(x) x > 2), ds.arrays.firstWith([1, 5, 7], function(x) x > 4)]", "[[1,3],true,true,5]"),
            ("[ds.arrays.indexOf([5, 6, 7], 6), ds.arrays.lastIndexOf([1, 2, 1], 1), ds.arrays.indexWhere([1, 5], function(x) x > 2)]", "[1,2,1]"),
            ("ds.arrays.join([{id: 1, n: 'a'}, {id: 2, n: 'b'}], [{id: 1, v: 10}], function(l) l.id, function(r) r.id)", "[{\"l\":{\"id\":1,\"n\":\"a\"},\"r\":{\"id\":1,\"v\":10}}]"),
            ("std.length(ds.arrays.leftJoin([{id: 1}, {id: 2}], [{id: 1}], function(l) l.id, function(r) r.id))", "2"),
            ("std.length(ds.arrays.outerJoin([{id: 1}], [{id: 2}], function(l) l.id, function(r) r.id))", "2"),
            ("[ds.arrays.occurrences(['a', 'b', 'a'], function(x) x), ds.arrays.partition([1, 2, 3], function(x) x > 1)]", "[{\"a\":2,\"b\":1},{\"failure\":[1],\"success\":[2,3]}]"),
            ("[ds.arrays.slice([1, 2, 3, 4], 1, 3), ds.arrays.splitAt([1, 2, 3], 1), ds.arrays.splitWhere([1, 2, 3], function(x) x == 2), ds.arrays.sumBy([{a: 1}, {a: 2}], function(x) x.a)]", "[[2,3],{\"l\":[1],\"r\":[2,3]},{\"l\":[1],\"r\":[2,3]},3]"),
            ("[ds.binaries.toBase64('Hello'), ds.binaries.fromBase64('SGVsbG8='), ds.binaries.toHex('Hi'), ds.binaries.fromHex('4869'), ds.binaries.readLinesWith('a\\nb', 'UTF-8')]", "[\"SGVsbG8=\",\"Hello\",\"4869\",\"Hi\",[\"a\",\"b\"]]"),
            ("[ds.crypto.hash('HelloWorld', 'MD5'), ds.crypto.hash('abc', 'SHA-256')]", "[\"68e109f0f40ca72a15e05cc22786f8e6\",\"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\"]"),
            ("ds.crypto.hmac('The quick brown fox jumps over the lazy dog', 'key', 'HmacSHA256')", "\"f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8\""),
            ("local secret = 'my-secret-key-123'; ds.crypto.decrypt(ds.crypto.encrypt('top secret', secret, 'AES/CBC/PKCS5Padding'), secret, 'AES/CBC/PKCS5Padding')", "\"top secret\""),
            ("ds.crypto.decrypt(ds.crypto.encrypt('ecb', '0123456789abcdef', 'AES/ECB/PKCS5Padding'), '0123456789abcdef', 'AES/ECB/PKCS5Padding')", "\"ecb\""),
            ("ds.jsonpath.select({store: {book: [{price: 8}, {price: 12}]}}, '$.store.book[*].price')", "[8,12]"),
            ("[ds.math.abs(-2), ds.math.avg([1, 2, 3, 4]), ds.math.ceil(1.2), ds.math.floor(1.8), ds.math.pow(2, 10), ds.math.round(2.456, 2), ds.math.sqrt(16), ds.math.sum([1, 2]), ds.math.mod(7, 3)]", "[2,2.5,2,1,1024,2.46,4,3,1]"),
            ("[ds.numbers.toBinary(5), ds.numbers.fromBinary('101'), ds.numbers.toHex(255), ds.numbers.fromHex('FF'), ds.numbers.toRadixNumber(35, 36), ds.numbers.fromRadixNumber('z', 36)]", "[\"101\",5,\"FF\",255,\"z\",35]"),
            ("[ds.objects.divideBy({a: 1, b: 2, c: 3}, 2), ds.objects.everyEntry({a: 1, b: 2}, function(v, k) v > 0), ds.objects.someEntry({a: 1}, function(v, k) k == 'z'), ds.objects.mergeWith({a: 1}, {b: 2}), ds.objects.takeWhile({a: 1, b: 5, c: 1}, function(v, k) v < 3)]", "[[{\"a\":1,\"b\":2},{\"c\":3}],true,false,{\"a\":1,\"b\":2},{\"a\":1}]"),
            ("[ds.period.years(2), ds.period.months(3), ds.period.days(4), ds.period.hours(5), ds.period.minutes(90), ds.period.seconds(30)]", "[\"P2Y\",\"P3M\",\"P4D\",\"PT5H\",\"PT1H30M\",\"PT30S\"]"),
            ("[ds.period.period({years: 1, months: 2, days: 3}), ds.period.duration({days: 1, hours: 2, minutes: 3, seconds: 4}), ds.period.between('2020-01-15', '2021-03-10')]", "[\"P1Y2M3D\",\"PT26H3M4S\",\"P1Y1M23D\"]"),
            ("[ds.strings.camelize('customer_first_name'), ds.strings.capitalize('hello world'), ds.strings.dasherize('fooBarBaz'), ds.strings.underscore('fooBarBaz')]", "[\"customerFirstName\",\"Hello World\",\"foo-bar-baz\",\"foo_bar_baz\"]"),
            ("[ds.strings.charCode('A'), ds.strings.charCodeAt('AB', 1), ds.strings.fromCharCode(97), ds.strings.isAlpha('abc'), ds.strings.isNumeric('123'), ds.strings.isUpperCase('ABC'), ds.strings.isLowerCase('abc'), ds.strings.isWhitespace('  '), ds.strings.isAlphanumeric('a1')]", "[65,66,\"a\",true,true,true,true,true,true]"),
            ("[ds.strings.leftPad('7', 3, '0'), ds.strings.rightPad('ab', 4, '.'), ds.strings.repeat('ab', 2), ds.strings.ordinalize(1), ds.strings.ordinalize(22), ds.strings.ordinalize(13)]", "[\"007\",\"ab..\",\"abab\",\"1st\",\"22nd\",\"13th\"]"),
            ("[ds.strings.pluralize('box'), ds.strings.pluralize('city'), ds.strings.pluralize('person'), ds.strings.singularize('boxes'), ds.strings.singularize('cities')]", "[\"boxes\",\"cities\",\"people\",\"box\",\"city\"]"),
            ("[ds.strings.substringAfter('a.b.c', '.'), ds.strings.substringAfterLast('a.b.c', '.'), ds.strings.substringBefore('a.b.c', '.'), ds.strings.substringBeforeLast('a.b.c', '.')]", "[\"b.c\",\"c\",\"a\",\"a.b\"]"),
            ("[ds.strings.appendIfMissing('file', '.txt'), ds.strings.prependIfMissing('/x', '/'), ds.strings.wrapWith('x', '\"'), ds.strings.wrapIfMissing('\"x', '\"'), ds.strings.unwrap('[x]', '['), ds.strings.withMaxSize('abcdef', 3)]", "[\"file.txt\",\"/x\",\"\\\"x\\\"\",\"\\\"x\\\"\",\"[x]\",\"abc\"]"),
            ("[ds.url.encode('a b&c=d'), ds.url.decode('a+b%26c%3Dd')]", "[\"a+b%26c%3Dd\",\"a b&c=d\"]"),
            ("ds.xml.flattenContents({p: {'$': 'Hello ', b: {'$': 'World'}}})", "\"Hello World\""),
        ],
    );
}

#[test]
fn ds_ext_vars_and_tla() {
    let ctx = ctx();
    let mut ex = exchange(&ctx);
    ex.set_property("CamelVariable.region", "EU");
    let v = run("std.extVar('env') + '/' + std.extVar('region') + '/' + std.extVar('foo')", &[("extVar.env", "prod")], &ex).unwrap();
    assert_eq!(v, Value::from("\"prod/EU/bar\""));
    let v = run("std.extVar('cfg').port + 1", &[("extVar.cfg", "{\"port\": 8080}")], &ex).unwrap();
    assert_eq!(v, Value::from("8081"));
    let err = run("std.extVar('nope')", &[], &ex).unwrap_err();
    assert!(err.message.contains("undefined external variable: nope"), "{}", err.message);
    let v = run("function(name, count=2, payload) {greeting: 'hi ' + name, n: count, body: payload}", &[("tla.name", "\"bob\"")], &ex).unwrap();
    assert_eq!(v, Value::from("{\"body\":\"Hello World\",\"greeting\":\"hi bob\",\"n\":2}"));
    let v = run("function(region, num) region + num", &[], &ex).unwrap();
    assert_eq!(v, Value::from("\"EU5\""));
}

#[test]
fn ds_xml_options() {
    let ctx = ctx();
    let mut ex = exchange(&ctx);
    ex.message.body = Value::from(r#"<ns0:order xmlns:ns0="urn:shop" id="7"><ns0:item>A1</ns0:item><note>hi</note></ns0:order>"#);
    let read = |src: &str, media: &str| run(src, &[("bodyMediaType", media), ("outputMediaType", "application/json")], &ex);
    assert_eq!(read("payload['shop:order']['shop:item']['$']", "application/xml; NamespaceDeclarations.shop=urn:shop").unwrap(), Value::from("\"A1\""));
    assert_eq!(read("payload['ns0:order']['@xmlns'].ns0", "application/xml").unwrap(), Value::from("\"urn:shop\""));
    assert_eq!(read("payload['ns0|order']['ns0|item']['#text']", "application/xml; NamespaceSeparator=|; TextValueKey=#text").unwrap(), Value::from("\"A1\""));
    assert_eq!(read("payload['ns0:order']['_id']", "application/xml; AttributeCharacterKey=_").unwrap(), Value::from("\"7\""));
    let write = |src: &str, media: &str| run(src, &[("outputMediaType", media)], &ex);
    assert_eq!(write("{a: {b: 'x'}}", "application/xml; RootElement=root").unwrap(), Value::from("<root><a><b>x</b></a></root>"));
    assert_eq!(write("{a: {b: null, c: ''}}", "application/xml; AutoEmptyElements=false").unwrap(), Value::from("<a><b></b><c></c></a>"));
    assert_eq!(write("{a: {b: null, c: 1}}", "application/xml; NullAsEmptyElement=false").unwrap(), Value::from("<a><c>1</c></a>"));
    assert_eq!(write("{'s:a': {'#': '<x>'}}", "application/xml; NamespaceDeclarations.s=urn:s").unwrap(), Value::from("<s:a xmlns:s=\"urn:s\"><![CDATA[<x>]]></s:a>"));
    assert_eq!(write("{a: 1}", "application/xml; OmitXmlDeclaration=false; XmlVersion=1.1").unwrap(), Value::from("<?xml version=\"1.1\" encoding=\"UTF-8\"?><a>1</a>"));
    assert_eq!(write("{'p|a': {'@xmlns': {p: 'urn:p'}, '@p|k': 'v'}}", "application/xml; NamespaceSeparator=|").unwrap(), Value::from("<p:a xmlns:p=\"urn:p\" p:k=\"v\"/>"));
}

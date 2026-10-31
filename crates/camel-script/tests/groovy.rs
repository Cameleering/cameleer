//! Groovy language: table-driven tests.

mod common;
use common::*;

#[test]
fn groovy_basics() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "groovy",
        &ex,
        &[
            ("1 + 2 * 3", "7"),
            ("7 / 2", "3.5"),
            ("10 / 2", "5"),
            ("7.intdiv(2)", "3"),
            ("2 ** 8", "256"),
            ("def x = 5\nx * 2", "10"),
            ("def s = 'abc'\ns.size() + s.length()", "6"),
            ("def name = 'World'\n\"Hello ${name}!\"", "\"Hello World!\""),
            ("def m = [a: 1]\n\"v=$m.a\"", "\"v=1\""),
            ("'single $quotes'", "\"single $quotes\""),
            ("def x = null\nx?.foo ?: 'default'", "\"default\""),
            ("def x = ''\nx ?: 'empty'", "\"empty\""),
            ("[1, 2] == [1, 2]", "true"),
            ("'abc' == 'abc' && 1 == 1.0", "true"),
            ("3 <=> 5", "-1"),
            ("'5' as Integer", "5"),
            ("'2.5' as Double", "2.5"),
            ("42 as String", "\"42\""),
            ("[1, 2, 2] as Set", "[1,2]"),
            ("'abc' * 2", "\"abcabc\""),
            ("'hello world' - 'o'", "\"hell world\""),
            ("[1, 2, 3, 2] - [2]", "[1,3]"),
            ("[1, 2] + [3]", "[1,2,3]"),
            ("def l = [1]\nl << 2 << 3\nl", "[1,2,3]"),
            ("[a: 1] + [b: 2]", "{\"a\":1,\"b\":2}"),
            ("(1..5).toList()", "[1,2,3,4,5]"),
            ("(1..<4).sum()", "6"),
            ("(5..1).collect { it }", "[5,4,3,2,1]"),
            ("3 in [1, 2, 3] && !(4 in [1, 2, 3])", "true"),
            ("'b' in 'abc'", "true"),
            ("def list = [10, 20, 30]\nlist[-1] + list[0]", "40"),
            ("[1, 2, 3, 4, 5][1..3]", "[2,3,4]"),
            ("'camel'[1..-1]", "\"amel\""),
            ("'camel'[0]", "\"c\""),
            ("5 instanceof Integer && 'x' instanceof String && [] instanceof List && [:] instanceof Map", "true"),
            ("1.5 instanceof BigDecimal", "true"),
        ],
    );
}

#[test]
fn groovy_closures_and_collections() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "groovy",
        &ex,
        &[
            ("[1, 2, 3].collect { it * 2 }", "[2,4,6]"),
            ("[1, 2, 3, 4].findAll { it % 2 == 0 }", "[2,4]"),
            ("[1, 2, 3, 4].find { it > 2 }", "3"),
            ("[1, 2, 3].inject(0) { acc, x -> acc + x }", "6"),
            ("[1, 2, 3].sum()", "6"),
            ("[1, 2, 3].size()", "3"),
            ("['a', 'b'].join('-')", "\"a-b\""),
            ("['a', 'b'].join()", "\"ab\""),
            ("def r = []\n[1, 2].each { r << it * 10 }\nr", "[10,20]"),
            ("def r = []\n['x', 'y'].eachWithIndex { v, i -> r << \"$i:$v\" }\nr", "[\"0:x\",\"1:y\"]"),
            ("[3, 1, 2].sort()", "[1,2,3]"),
            ("['bb', 'a', 'ccc'].sort { it.size() }", "[\"a\",\"bb\",\"ccc\"]"),
            ("[3, 1, 2].max() + [3, 1, 2].min()", "4"),
            ("[1, 2, 3].any { it > 2 } && [1, 2, 3].every { it > 0 }", "true"),
            ("[1, 1, 2].unique()", "[1,2]"),
            ("[1, 2, 3, 4].groupBy { it % 2 == 0 ? 'even' : 'odd' }", "{\"even\":[2,4],\"odd\":[1,3]}"),
            ("[1, 2, 3].collectEntries { [(it.toString()): it * it] }", "{\"1\":1,\"2\":4,\"3\":9}"),
            ("[[1, 2], [3]].flatten()", "[1,2,3]"),
            ("[1, 2, 3].reverse()", "[3,2,1]"),
            ("[1, 2, 3].first() + [1, 2, 3].last()", "4"),
            ("[1, 2, 3, 2].count(2)", "2"),
            ("def m = [a: 1, b: 2]\nm.collect { k, v -> k + v }", "[\"a1\",\"b2\"]"),
            ("def m = [a: 1, b: 2]\nm.findAll { it.value > 1 }", "{\"b\":2}"),
            ("def m = [a: 1, b: 2]\ndef s = 0\nm.each { k, v -> s += v }\ns", "3"),
            ("def m = [:]\nm.x = 1\nm['y'] = 2\nm.put('z', 3)\nm.keySet().join(',')", "\"x,y,z\""),
            ("def m = [name: 'camel', ver: 4]\nm.name.toUpperCase() + m.ver", "\"CAMEL4\""),
            ("[[name: 'a'], [name: 'b']]*.name", "[\"a\",\"b\"]"),
            ("[[name: 'a'], [name: 'b']].name", "[\"a\",\"b\"]"),
            ("def add = { a, b -> a + b }\nadd(2, 3)", "5"),
            ("def twice = { it * 2 }\ntwice.call(21)", "42"),
            ("def c = 0\n3.times { c += it }\nc", "3"),
            ("def out = []\n1.upto(3) { out << it }\nout", "[1,2,3]"),
            ("def f(a, b = 10) { a + b }\nf(1) + f(1, 2)", "14"),
            ("int fib(int n) { n < 2 ? n : fib(n - 1) + fib(n - 2) }\nfib(15)", "610"),
            ("String greet(String n) { return \"hi $n\" }\ngreet('bob')", "\"hi bob\""),
            ("def (a, b) = [1, 2]\na + b", "3"),
            ("[1, 2, 3].withIndex().collect { v, i -> v * i }", "[0,2,6]"),
        ],
    );
}

#[test]
fn groovy_strings_regex_control() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "groovy",
        &ex,
        &[
            ("'Camel'.toUpperCase() + 'Camel'.toLowerCase()", "\"CAMELcamel\""),
            ("'  x '.trim()", "\"x\""),
            ("'a,b,c'.split(',')", "[\"a\",\"b\",\"c\"]"),
            ("'a b  c'.tokenize()", "[\"a\",\"b\",\"c\"]"),
            ("'hello'.capitalize()", "\"Hello\""),
            ("'abc'.reverse()", "\"cba\""),
            ("'abc'.contains('b') && 'abc'.startsWith('a') && 'abc'.endsWith('c')", "true"),
            ("'a.b.c'.replace('.', '/')", "\"a/b/c\""),
            ("'a1b22'.replaceAll(/\\d+/, '#')", "\"a#b#\""),
            ("'12' ==~ /\\d+/", "true"),
            ("'abc' ==~ /\\d+/", "false"),
            ("def m = 'order-42' =~ /order-(\\d+)/\nm[0][1]", "\"42\""),
            ("('x1y2' =~ /\\d/) ? 'has digits' : 'none'", "\"has digits\""),
            ("('x1y2' =~ /\\d/).collect { it }", "[\"1\",\"2\"]"),
            ("'5'.toInteger() + '2.5'.toDouble()", "7.5"),
            ("'abc'.padLeft(5, '*')", "\"**abc\""),
            ("'%s=%d' .format('a', 3)", "\"a=3\""),
            ("String.format('%05.1f', 3.14159)", "\"003.1\""),
            ("def x = 7\nif (x > 5) { 'big' } else { 'small' }", "\"big\""),
            ("def r = ''\nfor (i in 1..3) { r += i }\nr", "\"123\""),
            ("def r = 0\nfor (int i = 0; i < 4; i++) { r += i }\nr", "6"),
            ("def r = []\nfor (e in [a: 1, b: 2]) { r << e.key + e.value }\nr", "[\"a1\",\"b2\"]"),
            ("def n = 0\nwhile (n < 10) { n += 3 }\nn", "12"),
            ("def classify(x) {\n  switch (x) {\n    case 1..5: return 'low'\n    case [10, 20]: return 'listed'\n    case ~/[a-z]+/: return 'word'\n    case String: return 'string'\n    default: return 'other'\n  }\n}\n[classify(3), classify(20), classify('abc'), classify('ABC'), classify(99)]", "[\"low\",\"listed\",\"word\",\"string\",\"other\"]"),
            ("try { throw new IllegalArgumentException('bad') } catch (IllegalArgumentException e) { 'caught ' + e.message }", "\"caught bad\""),
            ("try { Integer.parseInt('x') } catch (NumberFormatException e) { 'nfe' } finally { }", "\"nfe\""),
            ("try { null.foo() } catch (Exception e) { e.getClass().name }", "\"NullPointerException\""),
            ("def json = new groovy.json.JsonSlurper().parseText('{\"a\": [1, 2]}')\njson.a[1]", "2"),
            ("groovy.json.JsonOutput.toJson([a: 1, b: [true]])", "\"{\\\"a\\\":1,\\\"b\\\":[true]}\""),
            ("def sb = new StringBuilder()\nsb.append('a').append(1)\nsb.toString()", "\"a1\""),
            ("Math.max(3, 7) + Math.abs(-2)", "9"),
            ("assert 1 == 1\n'ok'", "\"ok\""),
            ("def l = new ArrayList()\nl.add(1)\nl.addAll([2, 3])\nl", "[1,2,3]"),
        ],
    );
}

#[test]
fn groovy_exchange_bindings() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "groovy",
        &ex,
        &[
            ("body", "\"Hello World\""),
            ("request.body", "\"Hello World\""),
            ("request.headers.foo", "\"bar\""),
            ("request.headers['num'] + 1", "6"),
            ("headers.foo == 'bar'", "true"),
            ("exchange.in.body.size()", "11"),
            ("exchange.message.headers.foo", "\"bar\""),
            ("exchange.properties.p1", "\"v1\""),
            ("exchangeProperties.p1", "\"v1\""),
            ("exchange.getProperty('p1')", "\"v1\""),
            ("exchange.getIn().getHeader('foo')", "\"bar\""),
            ("exchange.getIn().getBody(String)", "\"Hello World\""),
            ("exchange.in.getHeader('num', Integer) * 2", "10"),
            ("variables.v1", "\"x\""),
            ("camelContext.name.size() > 0", "true"),
            ("\"${request.body} from ${headers.foo}\"", "\"Hello World from bar\""),
        ],
    );
    let ex = order_exchange(&ctx);
    check(
        "groovy",
        &ex,
        &[
            ("request.body.customer.name", "\"Alice\""),
            ("body.items.findAll { it.qty > 1 }*.sku", "[\"A1\",\"C3\"]"),
            ("body.items.sum { it.qty * it.price }", "30.25"),
            ("body.items.sku", "[\"A1\",\"B2\",\"C3\"]"),
            ("body.customer?.vip ? 'gold' : 'std'", "\"gold\""),
        ],
    );
}

#[test]
fn groovy_mutations_affect_exchange() {
    let ctx = ctx();
    let mut ex = exchange(&ctx);
    let p = camel_script::script("groovy", "request.headers.extra = 'yes'\nexchange.in.body = body.reverse()\nexchange.properties.prop = 99\nheaders.remove('num')\nrequest.setHeader('count', headers.size())").unwrap();
    use camel_core::spi::Processor;
    p.process(&mut ex).unwrap();
    assert_eq!(ex.body().to_string(), "dlroW olleH");
    assert_eq!(ex.header("extra").unwrap().to_string(), "yes");
    assert!(ex.header("num").is_none());
    assert_eq!(ex.property("prop").unwrap().to_json(), "99");
    assert_eq!(ex.header("count").unwrap().to_json(), "2");
}

#[test]
fn groovy_errors_with_positions() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check_err(
        "groovy",
        &ex,
        &[
            ("def x = (1 + ", "Groovy syntax error at line 1, column 14"),
            ("def x = 1\nunknownVar + 1", "MissingPropertyException: No such property: unknownVar for class: Script1 (line 2, column 1)"),
            ("def x = null\nx.foo", "NullPointerException: Cannot get property 'foo' on null object (line 2, column 2)"),
            ("'abc'.noSuchMethod()", "MissingMethodException: No signature of method: java.lang.String.noSuchMethod()"),
            ("1 / 0", "ArithmeticException: Division by zero"),
            ("'x' as Integer", "NumberFormatException"),
            ("throw new IllegalStateException('nope')", "IllegalStateException: nope"),
            ("assert 1 == 2", "AssertionError"),
            ("undefinedMethod(1)", "MissingMethodException"),
        ],
    );
    let err = camel_script::eval("groovy", "throw new IllegalStateException('nope')", &ex).unwrap_err();
    assert_eq!(err.kind, "IllegalStateException");
}

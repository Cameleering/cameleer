//! JavaScript language: table-driven tests.

mod common;
use common::*;

#[test]
fn js_expressions_and_operators() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "js",
        &ex,
        &[
            ("1 + 2 * 3", "7"),
            ("7 / 2", "3.5"),
            ("10 / 2", "5"),
            ("7 % 3", "1"),
            ("2 ** 10", "1024"),
            ("'a' + 1 + 2", "\"a12\""),
            ("1 + 2 + 'a'", "\"3a\""),
            ("'5' * 2", "10"),
            ("'5' - 2", "3"),
            ("1 == '1'", "true"),
            ("1 === '1'", "false"),
            ("null == undefined", "true"),
            ("null === undefined", "false"),
            ("NaN === NaN", "false"),
            ("typeof 1 + typeof 'x' + typeof null + typeof undefined + typeof {} + typeof function(){}", "\"numberstringobjectundefinedobjectfunction\""),
            ("!!'' || 'fallback'", "\"fallback\""),
            ("0 || null || 'x'", "\"x\""),
            ("1 && 2 && 3", "3"),
            ("null ?? 'd'", "\"d\""),
            ("0 ?? 'd'", "0"),
            ("true ? 'y' : 'n'", "\"y\""),
            ("5 > 3 && 3 >= 3 && 2 < 3 && 2 <= 2 && 1 != 2", "true"),
            ("'abc' < 'abd'", "true"),
            ("(5 & 3) + (5 | 3) + (5 ^ 3) + (1 << 4) + (-16 >> 2)", "26"),
            ("-7 >>> 28", "15"),
            ("var x = 1; x += 5; x *= 2; x", "12"),
            ("var i = 0; i++; ++i; i--; i", "1"),
            ("0.1 + 0.2", "0.30000000000000004"),
            ("String(1/0) + String(-1/0)", "\"Infinity-Infinity\""),
            ("[1, 2] + ''", "\"1,2\""),
            ("({}) + ''", "\"[object Object]\""),
            ("void 0", "null"),
            ("'b' in {a: 1, b: 2}", "true"),
        ],
    );
}

#[test]
fn js_statements() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "js",
        &ex,
        &[
            ("var s = 0; for (var i = 1; i <= 10; i++) { s += i; } s", "55"),
            ("let s = 0; let i = 0; while (i < 5) { i++; if (i == 3) continue; s += i; } s", "12"),
            ("let n = 0; do { n++; } while (n < 3); n", "3"),
            ("let r = []; for (const k in {a: 1, b: 2}) r.push(k); r.join(',')", "\"a,b\""),
            ("let t = 0; for (const v of [1, 2, 3]) t += v; t", "6"),
            ("let r = ''; for (const c of 'abc') r = c + r; r", "\"cba\""),
            ("outer: for (var i = 0; i < 3; i++) { for (var j = 0; j < 3; j++) { if (j == 1) continue outer; if (i == 2) break outer; } } i", "2"),
            ("function f(x) { switch (x) { case 1: return 'one'; case 2: case 3: return 'few'; default: return 'many'; } } [f(1), f(3), f(9)].join()", "\"one,few,many\""),
            ("var r = ''; switch (2) { case 1: r += 'a'; case 2: r += 'b'; case 3: r += 'c'; break; case 4: r += 'd'; } r", "\"bc\""),
            ("try { throw new Error('boom'); } catch (e) { e.message + '/' + e.name }", "\"boom/Error\""),
            ("var log = []; try { log.push('t'); } finally { log.push('f'); } log.join()", "\"t,f\""),
            ("function g() { try { return 'try'; } finally { } } g()", "\"try\""),
            ("try { null.x } catch (e) { e instanceof TypeError }", "true"),
            ("try { undefinedVar } catch (e) { e.name }", "\"ReferenceError\""),
            ("try { throw 'plain' } catch (e) { e }", "\"plain\""),
            ("if (false) { 1 } else if (true) { 2 } else { 3 }", "2"),
            ("var x = 1; { let x = 2; } x", "1"),
            ("const a = 5; a * 2", "10"),
        ],
    );
}

#[test]
fn js_functions_and_objects() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "js",
        &ex,
        &[
            ("function add(a, b) { return a + b; } add(2, 3)", "5"),
            ("var sq = x => x * x; sq(7)", "49"),
            ("var f = (a, b = 10) => a + b; f(1)", "11"),
            ("function counter() { var c = 0; return function() { return ++c; }; } var n = counter(); n(); n(); n()", "3"),
            ("function fact(n) { return n <= 1 ? 1 : n * fact(n - 1); } fact(10)", "3628800"),
            ("function sum(...xs) { return xs.reduce((a, b) => a + b, 0); } sum(1, 2, 3, 4)", "10"),
            ("Math.max(...[3, 9, 2])", "9"),
            ("var fns = []; for (let i = 0; i < 3; i++) fns.push(() => i); fns.map(f => f()).join()", "\"0,1,2\""),
            ("var o = {a: 1, b: {c: [1, 2, {d: 'deep'}]}}; o.b.c[2].d", "\"deep\""),
            ("var o = {x: 1}; o.y = 2; o['z'] = 3; Object.keys(o).join('')", "\"xyz\""),
            ("var o = {n: 'cam', greet() { return 'hi ' + this.n; }}; o.greet()", "\"hi cam\""),
            ("function P(name) { this.name = name; } P.prototype.hello = function() { return 'I am ' + this.name; }; new P('Bob').hello()", "\"I am Bob\""),
            ("function P() {} var p = new P(); p instanceof P", "true"),
            ("class A { constructor(x) { this.x = x; } double() { return this.x * 2; } } new A(21).double()", "42"),
            ("class A { hi() { return 'A'; } } class B extends A { hi() { return 'B' + super.hi(); } } new B().hi()", "\"BA\""),
            ("class A { constructor(v) { this.v = v; } } class B extends A { constructor() { super(5); } } new B().v", "5"),
            ("var {a, b: {c}} = {a: 1, b: {c: 2}}; a + c", "3"),
            ("var [x, , y = 9, ...rest] = [1, 2, undefined, 4, 5]; [x, y, rest.length].join()", "\"1,9,2\""),
            ("var a = 1, b = 2; [a, b] = [b, a]; a * 10 + b", "21"),
            ("var o = {...{a: 1, b: 2}, b: 3}; o.a + o.b", "4"),
            ("[...'ab', ...[1]].length", "3"),
            ("var k = 'dyn'; var o = {[k + 1]: true}; o.dyn1", "true"),
            ("var x = 3, y = 4; var o = {x, y}; o.x + o.y", "7"),
            ("(function() { return arguments.length; })(1, 2, 3)", "3"),
            ("var o = {v: 1, get() { return this.v; }}; var g = o.get.bind({v: 42}); g()", "42"),
            ("function f(a, b) { return this.k + a + b; } f.call({k: 1}, 2, 3) + f.apply({k: 10}, [20, 30])", "66"),
            ("`sum: ${1 + 2}, body: ${body}`", "\"sum: 3, body: Hello World\""),
            ("var o = null; o?.a?.b ?? 'none'", "\"none\""),
            ("var o = {a: {b: 1}}; o?.a?.b", "1"),
            ("var o = {}; o.f?.()", "null"),
        ],
    );
}

#[test]
fn js_builtins() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "js",
        &ex,
        &[
            ("[3, 1, 2].map(x => x * 2).filter(x => x > 2)", "[6,4]"),
            ("[1, 2, 3, 4].reduce((a, b) => a + b)", "10"),
            ("var s = []; [1, 2].forEach((x, i) => s.push(x + i)); s", "[1,3]"),
            ("[10, 1, 3].sort()", "[1,10,3]"),
            ("[10, 1, 3].sort((a, b) => a - b)", "[1,3,10]"),
            ("['b', 'a'].concat(['c'], 'd').join('-')", "\"b-a-c-d\""),
            ("[1, 2, 3].indexOf(2) + [1, 2, 3].includes(3) + [1, 2, 3].slice(-2).length", "4"),
            ("var a = [1, 2, 3, 4]; var r = a.splice(1, 2, 'x'); a.join() + '|' + r.join()", "\"1,x,4|2,3\""),
            ("[1, [2, [3]]].flat(2)", "[1,2,3]"),
            ("[{n: 1}, {n: 5}].find(o => o.n > 2).n", "5"),
            ("[1, 2, 3].some(x => x > 2) && [1, 2, 3].every(x => x > 0)", "true"),
            ("[1, 2, 3].findIndex(x => x == 3)", "2"),
            ("Array.isArray([]) && !Array.isArray('x')", "true"),
            ("Array.from('abc').reverse().join('')", "\"cba\""),
            ("'Hello'.toUpperCase() + 'WORLD'.toLowerCase()", "\"HELLOworld\""),
            ("'  pad  '.trim().length", "3"),
            ("'a,b,,c'.split(',')", "[\"a\",\"b\",\"\",\"c\"]"),
            ("'a1b22c'.split(/\\d+/)", "[\"a\",\"b\",\"c\"]"),
            ("'camel'.substring(1, 3) + 'camel'.slice(-2) + 'camel'.charAt(0)", "\"amelc\""),
            ("'aaa'.replace('a', 'b') + ' ' + 'aaa'.replace(/a/g, 'b')", "\"baa bbb\""),
            ("'John Smith'.replace(/(\\w+)\\s(\\w+)/, '$2, $1')", "\"Smith, John\""),
            ("'x-y-z'.replaceAll('-', '+')", "\"x+y+z\""),
            ("'abc'.replace(/b/, m => m.toUpperCase())", "\"aBc\""),
            ("'abc'.indexOf('c') + 'abc'.startsWith('ab') + 'abc'.endsWith('bc') + 'abc'.includes('z')", "4"),
            ("'5'.padStart(3, '0') + 'ab'.repeat(2)", "\"005abab\""),
            ("/^[a-z]+$/i.test('Camel')", "true"),
            ("'order-123'.match(/(\\w+)-(\\d+)/)[2]", "\"123\""),
            ("'a1b2'.match(/\\d/g)", "[\"1\",\"2\"]"),
            ("JSON.stringify({a: 1, b: [true, null], c: 'x'})", "\"{\\\"a\\\":1,\\\"b\\\":[true,null],\\\"c\\\":\\\"x\\\"}\""),
            ("JSON.parse('{\"z\": 1, \"a\": [1, 2]}').a[1]", "2"),
            ("Object.keys(JSON.parse('{\"z\": 1, \"a\": 2}')).join()", "\"z,a\""),
            ("JSON.stringify({a: 1}, null, 2)", "\"{\\n  \\\"a\\\": 1\\n}\""),
            ("Math.max(1, 5, 3) + Math.min(4, 2) + Math.abs(-3) + Math.floor(2.7) + Math.ceil(2.1) + Math.round(2.5)", "18"),
            ("Math.sqrt(16) + Math.pow(2, 3)", "12"),
            ("parseInt('42px') + parseFloat('3.5abc') + Number('8')", "53.5"),
            ("isNaN(parseInt('x')) && Number.isInteger(5)", "true"),
            ("(255).toString(16) + (3.14159).toFixed(2)", "\"ff3.14\""),
            ("Object.entries({a: 1, b: 2}).map(([k, v]) => k + v).join()", "\"a1,b2\""),
            ("Object.assign({a: 1}, {b: 2}, {a: 3}).a", "3"),
            ("Object.values({a: 1, b: 2}).reduce((x, y) => x + y)", "3"),
            ("new Date(0).toISOString()", "\"1970-01-01T00:00:00.000Z\""),
            ("new Date('2024-02-29T10:20:30Z').getFullYear() + new Date('2024-02-29T10:20:30Z').getMonth()", "2025"),
            ("typeof Date.now()", "\"number\""),
            ("encodeURIComponent('a b&c')", "\"a%20b%26c\""),
            ("String(123) + Boolean(0)", "\"123false\""),
        ],
    );
}

#[test]
fn js_exchange_bindings() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "js",
        &ex,
        &[
            ("body", "\"Hello World\""),
            ("exchange.getIn().getBody()", "\"Hello World\""),
            ("exchange.getMessage().getHeader('foo')", "\"bar\""),
            ("request.headers.foo + headers.num", "\"bar5\""),
            ("headers.get('num') * 2", "10"),
            ("exchange.getProperty('p1')", "\"v1\""),
            ("exchange.properties.p1 + exchangeProperties.p1", "\"v1v1\""),
            ("exchange.getVariable('v1') + variables.v1", "\"xx\""),
            ("exchange.getIn().getHeader('missing') == null", "true"),
            ("exchange.getIn().getHeader('missing', 'dflt')", "\"dflt\""),
            ("typeof exchange.getExchangeId()", "\"string\""),
            ("camelContext.getName().length > 0", "true"),
            ("body.toUpperCase().split(' ')", "[\"HELLO\",\"WORLD\"]"),
            ("exchange.getIn().setHeader('x', 1); exchange.getIn().getHeader('x')", "1"),
        ],
    );
}

#[test]
fn js_mutations_affect_exchange() {
    let ctx = ctx();
    let mut ex = exchange(&ctx);
    let s = cameleer_script::Script::compile_named(
        "js",
        "exchange.getIn().setBody(body.toUpperCase()); exchange.getIn().setHeader('h', 42); exchange.setProperty('np', [1, 2]); headers.foo = 'baz'; exchange.setVariable('vv', true); 'result'",
    )
    .unwrap();
    let r = s.evaluate_mut(&mut ex).unwrap();
    assert_eq!(r.to_string(), "result");
    assert_eq!(ex.body().to_string(), "HELLO WORLD");
    assert_eq!(ex.header("h").unwrap().to_json(), "42");
    assert_eq!(ex.header("foo").unwrap().to_string(), "baz");
    assert_eq!(ex.property("np").unwrap().to_json(), "[1,2]");
    assert_eq!(ex.property("CamelVariable.vv").unwrap().to_json(), "true");
    // a JSON body mutated in place
    let mut ex = order_exchange(&ctx);
    cameleer_script::script("js", "body.items.push({sku: 'D4', qty: 1, price: 2}); body.customer.name = 'Bob';").unwrap();
    let s = cameleer_script::Script::compile_named("js", "body.items.push({sku: 'D4'}); body.customer.name = 'Bob';").unwrap();
    s.evaluate_mut(&mut ex).unwrap();
    assert_eq!(ex.body().get("customer").unwrap().get("name").unwrap().to_string(), "Bob");
    assert_eq!(ex.body().get("items").unwrap().as_array().unwrap().len(), 4);
    // evaluate (copy) leaves the exchange untouched
    let ex2 = exchange(&ctx);
    cameleer_script::eval("js", "exchange.getIn().setBody('changed')", &ex2).unwrap();
    assert_eq!(ex2.body().to_string(), "Hello World");
}

#[test]
fn js_json_body() {
    let ctx = ctx();
    let ex = order_exchange(&ctx);
    check(
        "js",
        &ex,
        &[
            ("body.customer.name", "\"Alice\""),
            ("body.items.length", "3"),
            ("body.items.map(i => i.qty * i.price).reduce((a, b) => a + b, 0)", "30.25"),
            ("body.items.filter(i => i.qty > 1).map(i => i.sku)", "[\"A1\",\"C3\"]"),
            ("body.tags.includes('web') && body.customer.vip", "true"),
            ("var o = JSON.parse(JSON.stringify(body)); o.id", "7"),
            ("({id: body.id, n: body.items.length})", "{\"id\":7,\"n\":3}"),
        ],
    );
}

#[test]
fn js_errors_with_positions() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check_err(
        "js",
        &ex,
        &[
            ("var x = ;", "JavaScript syntax error at line 1, column 9"),
            ("foo(1,", "line 1, column 7"),
            ("var a = 1;\nvar b = (2 + ;", "line 2, column 14"),
            ("'unterminated", "unterminated string literal"),
            ("missing + 1", "ReferenceError: missing is not defined (line 1, column 1)"),
            ("var o = null;\no.x", "TypeError: Cannot read properties of null (reading 'x') (line 2, column 2)"),
            ("var n = 5; n()", "is not a function"),
            ("throw new Error('custom failure')", "Error: custom failure"),
            ("'abc'.nope()", "is not a function"),
            ("function f() { return f(); } f()", "Maximum call stack size exceeded"),
        ],
    );
    let err = cameleer_script::eval("js", "var x = ;", &ex).unwrap_err();
    assert_eq!(err.kind, "ExpressionIllegalSyntaxException");
}

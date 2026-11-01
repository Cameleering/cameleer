//! JavaScript: accessors, generators, promises / async, keyed collections, symbols,
//! iterators, `Java.type`, deep recursion and cyclic structures.

mod common;
use common::*;

#[test]
fn js_accessors() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "js",
        &ex,
        &[
            ("var o = { get full() { return 'a' + 'b'; } }; o.full", "\"ab\""),
            ("var o = { _v: 1, get v() { return this._v; }, set v(x) { this._v = x * 2; } }; o.v = 5; o.v", "10"),
            ("class P { constructor() { this._n = 'x'; } get name() { return this._n.toUpperCase(); } set name(v) { this._n = v; } } var p = new P(); p.name = 'bob'; p.name", "\"BOB\""),
            ("class C { static get k() { return 42; } } C.k", "42"),
            ("var o = {}; Object.defineProperty(o, 'z', { get: function() { return 7; } }); o.z", "7"),
            ("var o = { get a() { return 1; } }; Object.keys(o)", "[\"a\"]"),
            ("class A { get v() { return 1; } } class B extends A { get v() { return super.v + 1; } } new B().v", "2"),
        ],
    );
}

#[test]
fn js_generators_and_iterators() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "js",
        &ex,
        &[
            ("function* g() { yield 1; yield 2; yield 3; } var a = []; for (var x of g()) a.push(x); a", "[1,2,3]"),
            ("function* g() { yield 1; return 9; } var it = g(); [it.next().value, it.next().value, it.next().done]", "[1,9,true]"),
            ("function* g() { var x = yield 1; yield x * 10; } var it = g(); it.next(); it.next(4).value", "40"),
            ("function* nat() { var i = 0; while (true) yield i++; } var it = nat(); it.next(); it.next(); it.next().value", "2"),
            ("function* g() { yield* [1, 2]; yield 3; } [...g()]", "[1,2,3]"),
            ("function* inner() { yield 'a'; yield 'b'; } function* outer() { yield* inner(); yield 'c'; } Array.from(outer()).join('')", "\"abc\""),
            ("function* g() { try { yield 1; yield 2; } finally { log.push('f'); } } var log = []; var it = g(); it.next(); it.return(5); log", "[\"f\"]"),
            ("function* g() { try { yield 1; } catch (e) { yield 'caught ' + e; } } var it = g(); it.next(); it.throw('x').value", "\"caught x\""),
            ("var o = { *[Symbol.iterator]() { yield 1; yield 2; } }; [...o]", "[1,2]"),
            ("class R { constructor(n) { this.n = n; } [Symbol.iterator]() { var i = 0, n = this.n; return { next: () => i < n ? { value: i++, done: false } : { value: undefined, done: true } }; } } [...new R(3)]", "[0,1,2]"),
            ("var it = [10, 20][Symbol.iterator](); it.next(); it.next().value", "20"),
            ("var s = 0; for (const [k, v] of Object.entries({a: 1, b: 2})) s += v; s", "3"),
            ("function* fib() { var a = 0, b = 1; for (;;) { yield a; [a, b] = [b, a + b]; } } var r = []; for (var f of fib()) { if (f > 20) break; r.push(f); } r", "[0,1,1,2,3,5,8,13]"),
            ("var [a, b] = (function*() { yield 'x'; yield 'y'; })(); a + b", "\"xy\""),
        ],
    );
}

#[test]
fn js_promises_and_async() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "js",
        &ex,
        &[
            ("var r; Promise.resolve(5).then(v => r = v * 2); r", "null"),
            ("var out = []; Promise.resolve(1).then(v => out.push(v)); out.push(0); out", "[0,1]"),
            ("async function f() { return 3; } var r; f().then(v => { r = v; }); r", "null"),
            ("var log = []; async function f() { log.push('a'); await null; log.push('c'); } f(); log.push('b'); log", "[\"a\",\"b\",\"c\"]"),
            ("async function f() { var x = await Promise.resolve(20); return x + 1; } f()", "21"),
            ("async function f() { throw new Error('boom'); } f().catch(e => e.message)", "\"boom\""),
            ("Promise.all([1, Promise.resolve(2), new Promise(r => r(3))])", "[1,2,3]"),
            ("Promise.race([new Promise(r => setTimeout(() => r('slow'), 50)), new Promise(r => setTimeout(() => r('fast'), 10))])", "\"fast\""),
            ("Promise.allSettled([Promise.resolve(1), Promise.reject('e')]).then(rs => rs.map(r => r.status))", "[\"fulfilled\",\"rejected\"]"),
            ("Promise.any([Promise.reject(1), Promise.resolve(2)])", "2"),
            ("var order = []; setTimeout(() => order.push('timeout'), 0); Promise.resolve().then(() => order.push('micro')); order.push('sync'); var done = new Promise(r => setTimeout(() => r(order), 1)); done", "[\"sync\",\"micro\",\"timeout\"]"),
            ("const sleep = ms => new Promise(r => setTimeout(r, ms)); async function f() { await sleep(5); return 'done'; } f()", "\"done\""),
            ("(async () => { try { await Promise.reject(new Error('no')); } catch (e) { return 'got ' + e.message; } })()", "\"got no\""),
            ("new Promise((res, rej) => rej('x')).then(() => 'ok', e => 'err ' + e)", "\"err x\""),
            ("Promise.resolve(1).finally(() => 'ignored').then(v => v + 1)", "2"),
            ("async function* ag() { yield 1; yield 2; } (async () => { var s = 0; for await (const v of ag()) s += v; return s; })()", "3"),
            ("(async () => { var r = []; for await (const v of [Promise.resolve('a'), 'b']) r.push(v); return r.join(''); })()", "\"ab\""),
            ("async function* ag() { var x = await Promise.resolve(5); yield x; } (async () => { var s = 0; for await (const v of ag()) s += v; return s; })()", "5"),
        ],
    );
}

#[test]
fn js_collections_and_symbols() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "js",
        &ex,
        &[
            ("var m = new Map(); m.set('a', 1).set('b', 2); [m.size, m.get('b'), m.has('c')]", "[2,2,false]"),
            ("var m = new Map([['x', 1], ['y', 2]]); [...m.keys()]", "[\"x\",\"y\"]"),
            ("var m = new Map([[1, 'one']]); m.get(1) + (m.get('1') === undefined)", "\"onetrue\""),
            ("var m = new Map([['a', 1], ['b', 2]]); var s = ''; m.forEach((v, k) => s += k + v); s", "\"a1b2\""),
            ("var m = new Map([['a', 1]]); m.delete('a'); m.size", "0"),
            ("var k = {}; var m = new Map(); m.set(k, 'obj'); m.get(k)", "\"obj\""),
            ("Object.fromEntries(new Map([['a', 1], ['b', 2]]))", "{\"a\":1,\"b\":2}"),
            ("var s = new Set([1, 2, 2, 3, 1]); [s.size, s.has(2)]", "[3,true]"),
            ("[...new Set('banana')].join('')", "\"ban\""),
            ("var s = new Set(); s.add(NaN).add(NaN); s.size", "1"),
            ("var w = new WeakMap(); var k = {}; w.set(k, 5); [w.get(k), w.has({})]", "[5,false]"),
            ("var ws = new WeakSet(); var k = {}; ws.add(k); ws.has(k)", "true"),
            ("var s1 = Symbol('t'); var s2 = Symbol('t'); s1 === s2", "false"),
            ("Symbol.for('k') === Symbol.for('k')", "true"),
            ("Symbol('desc').toString()", "\"Symbol(desc)\""),
            ("Symbol('d').description", "\"d\""),
            ("var s = Symbol('h'); var o = {}; o[s] = 1; o.v = 2; Object.keys(o)", "[\"v\"]"),
            ("typeof Symbol.iterator", "\"symbol\""),
            ("Array.from(new Map([['a', 1]]).entries())", "[[\"a\",1]]"),
            ("var m = new Map(); m.set('n', 1); JSON.stringify([...m])", "\"[[\\\"n\\\",1]]\""),
        ],
    );
}

#[test]
fn js_java_type() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "js",
        &ex,
        &[
            ("var Integer = Java.type('java.lang.Integer'); Integer.parseInt('42') + 1", "43"),
            ("var M = Java.type('java.lang.Math'); M.max(3, 9)", "9"),
            ("var AL = Java.type('java.util.ArrayList'); var l = new AL(); l.add('a'); l.add('b'); l.size()", "2"),
            ("var HM = Java.type('java.util.HashMap'); var m = new HM(); m.put('k', 'v'); m.get('k')", "\"v\""),
            ("var S = Java.type('java.lang.String'); S.valueOf(12) + S.join('-', ['a', 'b'])", "\"12a-b\""),
        ],
    );
    check_err("js", &ex, &[("Java.type('java.lang.Runtime')", "is not allowed")]);
}

#[test]
fn js_deep_recursion_and_cycles() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "js",
        &ex,
        &[
            ("function f(n) { return n == 0 ? 0 : 1 + f(n - 1); } f(800)", "800"),
            ("var o = {}; o.self = o; o.self.self.self === o", "true"),
            ("var a = [1]; a.push(a); a.length", "2"),
        ],
    );
    check_err("js", &ex, &[("function f() { return f(); } f()", "")]);
}

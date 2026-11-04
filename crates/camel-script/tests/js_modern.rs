//! JavaScript: Proxy / Reflect, BigInt, typed arrays / ArrayBuffer / DataView, Intl, ES modules,
//! async generators with microtask scheduling, tagged templates, globalThis, structuredClone,
//! regex named groups / lookbehind.

mod common;
use common::*;

#[test]
fn js_proxy_reflect() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "js",
        &ex,
        &[
            ("var p = new Proxy({a: 1}, { get: (t, k) => k in t ? t[k] : 'none' }); [p.a, p.b]", "[1,\"none\"]"),
            ("var log = []; var p = new Proxy({}, { set(t, k, v) { log.push(k); t[k] = v * 2; return true; } }); p.x = 5; [p.x, log]", "[10,[\"x\"]]"),
            ("var p = new Proxy({}, { has: (t, k) => k.startsWith('a') }); ['abc' in p, 'b' in p]", "[true,false]"),
            ("var p = new Proxy({a: 1, b: 2}, { ownKeys: t => ['b'] }); Object.keys(p)", "[\"b\"]"),
            ("var t = {a: 1}; var p = new Proxy(t, { deleteProperty(t, k) { delete t[k]; return true; } }); delete p.a; Object.keys(t)", "[]"),
            ("var f = new Proxy(function(a, b) { return a + b; }, { apply: (t, self, args) => t(...args) * 10 }); [f(1, 2), typeof f]", "[30,\"function\"]"),
            ("class P { constructor(x) { this.x = x; } } var C = new Proxy(P, { construct(t, args) { return new t(args[0] + 1); } }); new C(1).x", "2"),
            ("var p = new Proxy({}, {}); p.y = 3; p.y", "3"),
            ("var p = new Proxy({ greet() { return 'hi ' + this.name; }, name: 'bob' }, {}); p.greet()", "\"hi bob\""),
            ("var v = {n: 1}; var p = new Proxy(v, { get(t, k, r) { return Reflect.get(t, k, r) + 1; } }); p.n", "2"),
            ("[Reflect.has({a: 1}, 'a'), Reflect.ownKeys({x: 1, y: 2}), Reflect.apply(Math.max, null, [1, 5, 3])]", "[true,[\"x\",\"y\"],5]"),
            ("var o = {}; Reflect.defineProperty(o, 'k', { value: 7 }); [o.k, Reflect.deleteProperty(o, 'k'), 'k' in o]", "[7,true,false]"),
            ("Reflect.getPrototypeOf(Object.create({z: 1})).z", "1"),
            ("var r = Proxy.revocable({a: 1}, {}); r.proxy.a", "1"),
        ],
    );
}

#[test]
fn js_bigint() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "js",
        &ex,
        &[
            ("(2n ** 64n).toString()", "\"18446744073709551616\""),
            ("12345678901234567890n * 98765432109876543210n + ''", "\"1219326311370217952237463801111263526900\""),
            ("typeof 10n", "\"bigint\""),
            ("[7n / 2n, -7n / 2n, 7n % 3n, -7n % 3n].map(String)", "[\"3\",\"-3\",\"1\",\"-1\"]"),
            ("[1n == 1, 1n === 1, 2n > 1, 1n < 1.5, 0n ? 'y' : 'n']", "[true,false,true,true,\"n\"]"),
            ("BigInt('0xff') + BigInt(1)", "256"),
            ("BigInt(Number.MAX_SAFE_INTEGER) + 2n + ''", "\"9007199254740993\""),
            ("[(255n).toString(16), (-6n & 3n).toString(), (1n << 70n).toString(2).length]", "[\"ff\",\"2\",71]"),
            ("[BigInt.asIntN(8, 255n), BigInt.asUintN(8, -1n)].map(String)", "[\"-1\",\"255\"]"),
            ("try { 1n + 1 } catch (e) { e.name }", "\"TypeError\""),
            ("let x = 5n; x++; x += 10n; String(x)", "\"16\""),
            ("[Number(123n), -(-5n) + '', (2n ** 100n).toLocaleString('en-US').length]", "[123,\"5\",41]"),
        ],
    );
}

#[test]
fn js_typed_arrays() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "js",
        &ex,
        &[
            ("var a = new Uint8Array(4); a[0] = 255; a[1] = 256; a[2] = -1; Array.from(a)", "[255,0,255,0]"),
            ("var a = new Int16Array([1, -2, 40000]); [a.length, a.byteLength, a[2]]", "[3,6,-25536]"),
            ("Array.from(new Uint8ClampedArray([300, -5, 1.5, 2.5]))", "[255,0,2,2]"),
            ("var f = new Float32Array([1.5, 0.1]); [f[0], f.BYTES_PER_ELEMENT, Math.abs(f[1] - 0.1) < 1e-7]", "[1.5,4,true]"),
            ("var buf = new ArrayBuffer(8); var u32 = new Uint32Array(buf); var u8 = new Uint8Array(buf); u32[0] = 0x01020304; [u8[0], u8[3], buf.byteLength]", "[4,1,8]"),
            ("var dv = new DataView(new ArrayBuffer(8)); dv.setUint16(0, 0x1234); dv.setUint16(2, 0x1234, true); [dv.getUint8(0), dv.getUint8(2), dv.getUint16(0, false)]", "[18,52,4660]"),
            ("var dv = new DataView(new ArrayBuffer(8)); dv.setFloat64(0, Math.PI); dv.getFloat64(0) === Math.PI", "true"),
            ("var dv = new DataView(new ArrayBuffer(8)); dv.setBigInt64(0, -2n); [String(dv.getBigInt64(0)), String(dv.getBigUint64(0))]", "[\"-2\",\"18446744073709551614\"]"),
            ("var a = Uint8Array.from([1, 2, 3, 4]); var s = a.subarray(1, 3); s[0] = 9; [Array.from(a), s.length, s.byteOffset]", "[[1,9,3,4],2,1]"),
            ("var a = new Int32Array([5, 1, 4]); a.sort(); [a.join('-'), a.map(x => x * 2).join(','), a.filter(x => x > 1).length, a.reduce((s, x) => s + x, 0)]", "[\"1-4-5\",\"2,8,10\",2,10]"),
            ("var a = new Uint8Array(5); a.set([7, 8], 2); a.fill(1, 0, 2); [...a]", "[1,1,7,8,0]"),
            ("[ArrayBuffer.isView(new Uint8Array(1)), ArrayBuffer.isView([]), new Uint8Array([1,2]) instanceof Object]", "[true,false,true]"),
            ("Array.from(new TextEncoder().encode('hé'))", "[104,195,169]"),
            ("new TextDecoder().decode(new Uint8Array([104, 105, 0xe2, 0x82, 0xac]))", "\"hi€\""),
            ("new Uint8Array([104, 105])", "\"hi\""),
        ],
    );
}

#[test]
fn js_intl() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "js",
        &ex,
        &[
            ("new Intl.NumberFormat('en-US').format(1234567.891)", "\"1,234,567.891\""),
            ("new Intl.NumberFormat('de-DE').format(1234567.891)", "\"1.234.567,891\""),
            ("new Intl.NumberFormat('fr-FR').format(1234.5)", "\"1\u{202f}234,5\""),
            ("new Intl.NumberFormat('en-GB', { style: 'currency', currency: 'GBP' }).format(1234.5)", "\"£1,234.50\""),
            ("new Intl.NumberFormat('de-DE', { style: 'currency', currency: 'EUR' }).format(1234.5)", "\"1.234,50\u{a0}€\""),
            ("new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD' }).format(-5)", "\"-$5.00\""),
            ("new Intl.NumberFormat('en-US', { style: 'currency', currency: 'JPY' }).format(1234.6)", "\"¥1,235\""),
            ("new Intl.NumberFormat('en-US', { style: 'percent' }).format(0.256)", "\"26%\""),
            ("new Intl.NumberFormat('de-DE', { style: 'percent', minimumFractionDigits: 1 }).format(0.256)", "\"25,6\u{a0}%\""),
            ("new Intl.NumberFormat('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 }).format(1.005)", "\"1.01\""),
            ("(1234.5).toLocaleString('de-DE')", "\"1.234,5\""),
            ("(1234.5).toLocaleString()", "\"1,234.5\""),
            ("new Intl.NumberFormat('en-US', { notation: 'compact' }).format(1530000)", "\"1.5M\""),
            ("new Intl.NumberFormat('en-US', { useGrouping: false }).format(1234567)", "\"1234567\""),
            ("new Intl.NumberFormat('en-US').formatToParts(1234.5).map(p => p.type).join(',')", "\"integer,group,integer,decimal,fraction\""),
            ("var d = new Date(Date.UTC(2024, 0, 5, 15, 4, 5)); new Intl.DateTimeFormat('en-US').format(d)", "\"1/5/2024\""),
            ("var d = new Date(Date.UTC(2024, 0, 5, 15, 4, 5)); [new Intl.DateTimeFormat('en-GB').format(d), new Intl.DateTimeFormat('de-DE').format(d), new Intl.DateTimeFormat('fr-FR').format(d)]", "[\"05/01/2024\",\"5.1.2024\",\"05/01/2024\"]"),
            ("var d = new Date(Date.UTC(2024, 0, 5)); ['en-US', 'en-GB', 'de-DE', 'fr-FR'].map(l => new Intl.DateTimeFormat(l, { dateStyle: 'long' }).format(d))", "[\"January 5, 2024\",\"5 January 2024\",\"5. Januar 2024\",\"5 janvier 2024\"]"),
            ("var d = new Date(Date.UTC(2024, 0, 5)); new Intl.DateTimeFormat('de-DE', { dateStyle: 'full' }).format(d)", "\"Freitag, 5. Januar 2024\""),
            ("var d = new Date(Date.UTC(2024, 0, 5, 15, 4, 5)); [d.toLocaleTimeString('en-US'), d.toLocaleTimeString('de-DE')]", "[\"3:04:05\u{202f}PM\",\"15:04:05\"]"),
            ("var d = new Date(Date.UTC(2024, 0, 5, 15, 4, 5)); d.toLocaleString('en-GB')", "\"05/01/2024, 15:04:05\""),
            ("var d = new Date(Date.UTC(2024, 2, 9)); d.toLocaleDateString('en-US', { weekday: 'long', year: 'numeric', month: 'long', day: 'numeric' })", "\"Saturday, March 9, 2024\""),
            ("var d = new Date(Date.UTC(2024, 2, 9)); d.toLocaleDateString('fr-FR', { month: 'short', year: 'numeric' })", "\"mars 2024\""),
            ("[new Intl.PluralRules('en-US').select(1), new Intl.PluralRules('en-US').select(2), new Intl.PluralRules('en-US', { type: 'ordinal' }).select(22)]", "[\"one\",\"other\",\"two\"]"),
            ("['b', 'a', 'Ä', 'z'].sort(new Intl.Collator('de').compare)", "[\"a\",\"Ä\",\"b\",\"z\"]"),
            ("['item10', 'item2', 'item1'].sort(new Intl.Collator('en', { numeric: true }).compare)", "[\"item1\",\"item2\",\"item10\"]"),
            ("new Intl.ListFormat('en', { type: 'conjunction' }).format(['a', 'b', 'c'])", "\"a, b, and c\""),
            ("new Intl.RelativeTimeFormat('en', { numeric: 'auto' }).format(-1, 'day')", "\"yesterday\""),
            ("new Intl.NumberFormat('de-DE').resolvedOptions().locale", "\"de-DE\""),
        ],
    );
    // region zones need a zoneinfo database (none on Windows unless TZDIR is set)
    if camel_core::tz::zoneinfo_available() {
        check(
            "js",
            &ex,
            &[("var d = new Date(Date.UTC(2024, 6, 1, 12, 0)); new Intl.DateTimeFormat('en-US', { timeZone: 'America/New_York', hour: 'numeric', minute: '2-digit' }).format(d)", "\"8:00\u{202f}AM\"")],
        );
    }
}

#[test]
fn js_modules() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "js",
        &ex,
        &[
            ("import greet, { square, PI2 } from './tests/fixtures/js/math.js'; [greet('x'), square(4), PI2]", "[\"hi x\",16,6.28]"),
            ("import * as m from './tests/fixtures/js/math.js'; [m.viaHelper, typeof m.default, Object.keys(m).includes('bump')]", "[20,\"function\",true]"),
            ("import { Box as B } from './tests/fixtures/js/math.js'; new B(4).double", "8"),
            ("import { ANSWER, NAME } from './tests/fixtures/js/util.js'; ANSWER + NAME", "\"42consts\""),
            ("import { bump } from './tests/fixtures/js/math.js'; import * as again from './tests/fixtures/js/math.js'; bump(); again.bump()", "2"),
            ("import cfg from './tests/fixtures/js/config.json'; cfg.port", "8080"),
            ("import { square } from 'classpath:tests/fixtures/js/math.js'; square(3)", "9"),
            ("export const local = 1; export default 5; local + 1", "2"),
            ("(async () => { const m = await import('./tests/fixtures/js/math.js'); return m.square(5); })()", "25"),
            ("import('./tests/fixtures/js/nothing.js').then(() => 'loaded', e => 'failed: ' + e.message)", "\"failed: Cannot find module './tests/fixtures/js/nothing.js'\""),
        ],
    );
    check_err("js", &ex, &[("import x from './tests/fixtures/js/missing.js'; x", "Cannot find module")]);
}

#[test]
fn js_async_generators_and_microtasks() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "js",
        &ex,
        &[
            ("var log = []; async function* g() { log.push('a'); await null; log.push('b'); yield 1; } (async () => { for await (const x of g()) log.push(x); })(); log.push('sync'); log", "[\"a\",\"sync\",\"b\",1]"),
            ("async function* g() { yield 1; yield 2; } var out = []; (async () => { for await (const x of g()) out.push(x); out.push('end'); })(); out", "[1,2,\"end\"]"),
            ("async function* g() { for (let i = 0; i < 3; i++) { await new Promise(r => setTimeout(r, 10)); yield i; } } (async () => { let s = 0; for await (const x of g()) s += x; return s; })()", "3"),
            ("async function* g() { yield 'x'; } var it = g(); var p = it.next(); [typeof p.then, p instanceof Promise]", "[\"function\",true]"),
            ("async function* g() { yield 'x'; } var it = g(); (async () => { var a = await it.next(); var b = await it.next(); return [a.value, a.done, b.done]; })()", "[\"x\",false,true]"),
            ("var log = []; Promise.resolve().then(() => log.push('micro')); setTimeout(() => log.push('macro'), 0); log.push('sync'); log", "[\"sync\",\"micro\",\"macro\"]"),
            ("(async () => { var r = []; for await (const x of [Promise.resolve(1), 2, Promise.resolve(3)]) r.push(x); return r; })()", "[1,2,3]"),
            ("var o = { async *[Symbol.asyncIterator]() { yield 'a'; yield 'b'; } }; (async () => { var r = ''; for await (const x of o) r += x; return r; })()", "\"ab\""),
            ("async function* g() { try { yield 1; await Promise.reject(new Error('boom')); } catch (e) { yield e.message; } } (async () => { var r = []; for await (const x of g()) r.push(x); return r; })()", "[1,\"boom\"]"),
            ("var order = []; async function a() { order.push(1); await null; order.push(3); } a(); order.push(2); order", "[1,2,3]"),
        ],
    );
}

#[test]
fn js_misc_modern() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "js",
        &ex,
        &[
            ("function tag(s, ...v) { return s.raw.join('|') + ':' + v.join(','); } tag`a${1}b${2}c`", "\"a|b|c:1,2\""),
            ("function t(s) { return s[0] + '/' + s.raw[0]; } t`x\\ny`", "\"x\\ny/x\\\\ny\""),
            ("String.raw`C:\\temp\\x${1 + 1}`", "\"C:\\\\temp\\\\x2\""),
            ("var html = (s, ...v) => s.reduce((a, p, i) => a + p + (i < v.length ? String(v[i]).toUpperCase() : ''), ''); html`<b>${'x'}</b>`", "\"<b>X</b>\""),
            ("globalThis.answer = 42; answer", "42"),
            ("var q = 7; [typeof globalThis, globalThis.Math === Math, typeof globalThis.parseInt]", "[\"object\",true,\"function\"]"),
            ("try { JSON.parse('{') } catch { 'bad json' }", "\"bad json\""),
            ("Object.fromEntries(new Map([['a', 1], ['b', 2]]))", "{\"a\":1,\"b\":2}"),
            ("var a = { d: new Date(0), m: new Map([[1, { x: 1 }]]), s: new Set([1]) }; var b = structuredClone(a); [b.m.get(1).x, b.m.get(1) !== a.m.get(1), b.s.has(1), b.d.getTime()]", "[1,true,true,0]"),
            ("var o = { n: 1 }; o.self = o; var c = structuredClone(o); [c.self === c, c !== o]", "[true,true]"),
            ("try { structuredClone({ f() {} }) } catch (e) { e.name }", "\"DataCloneError\""),
            ("var m = '2024-01-05'.match(/(?<y>\\d{4})-(?<m>\\d\\d)-(?<d>\\d\\d)/); [m.groups.y, m.groups.d, m.index]", "[\"2024\",\"05\",0]"),
            ("'2024-01-05'.replace(/(?<y>\\d+)-(?<m>\\d+)-(?<d>\\d+)/, '$<d>.$<m>.$<y>')", "\"05.01.2024\""),
            ("'price: $30, cost: $5'.match(/(?<=\\$)\\d+/g)", "[\"30\",\"5\"]"),
            ("'abc123def'.replace(/(?<!\\d)[a-z]+/g, 'X')", "\"X123dX\""),
            ("[...'a1b2'.matchAll(/(?<l>[a-z])(?<d>\\d)/g)].map(m => m.groups.l + m.groups.d + m.index)", "[\"a10\",\"b22\"]"),
            ("var re = /o/g; var r = []; var m; while ((m = re.exec('foo boo')) !== null) r.push(m.index); r", "[1,2,5,6]"),
            ("'x-y'.replace(/(?<a>\\w)-(?<b>\\w)/, (m, a, b, off, s, g) => g.b + g.a)", "\"yx\""),
            ("class T { #v = 2; get v() { return this.#v; } static get k() { return 'K'; } } [new T().v, T.k]", "[2,\"K\"]"),
        ],
    );
}

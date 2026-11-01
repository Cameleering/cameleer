//! Groovy: overloading by parameter type, AST transforms (`@Immutable`, `@Builder`,
//! `@Singleton`, `@Sortable`, `@AutoClone`, `@Slf4j`...), switch expressions and `isCase`
//! (types, closures, ranges, regexes), safe index `?[]`, spread map `*:`, varargs, `@Grab`.

mod common;
use common::*;

#[test]
fn groovy_overloading_by_type() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "groovy",
        &ex,
        &[
            ("def f(String s) { 'str' }\ndef f(Integer i) { 'int' }\ndef f(Object o) { 'obj' }\n[f('a'), f(1), f([1])]", "[\"str\",\"int\",\"obj\"]"),
            ("class A { def m(String s) { 's' }\n def m(int i) { 'i' }\n def m(List l) { 'l' } }\ndef a = new A()\n[a.m('x'), a.m(2), a.m([])]", "[\"s\",\"i\",\"l\"]"),
            ("class Shape {}\nclass Circle extends Shape {}\ndef area(Shape s) { 'shape' }\ndef area(Circle c) { 'circle' }\n[area(new Shape()), area(new Circle())]", "[\"shape\",\"circle\"]"),
            ("def f(String s, Integer i) { 's,i' }\ndef f(Integer i, String s) { 'i,s' }\n[f('a', 1), f(1, 'a')]", "[\"s,i\",\"i,s\"]"),
            ("def f(Map m) { 'map' }\ndef f(List l) { 'list' }\ndef f(Closure c) { 'closure' }\n[f([a: 1]), f([1]), f({ -> 1 })]", "[\"map\",\"list\",\"closure\"]"),
            ("def f(int x) { 'int' }\ndef f(double x) { 'double' }\ndef f(String x) { 'string' }\n[f(1), f(1.5d), f('1')]", "[\"int\",\"double\",\"string\"]"),
            ("class C { def v\n C(String s) { v = 's' }\n C(Integer i) { v = 'i' }\n C(String a, String b) { v = a + b } }\n[new C('a').v, new C(1).v, new C('x', 'y').v]", "[\"s\",\"i\",\"xy\"]"),
            ("def f(String s) { 1 }\ndef f(String s, int n) { 2 }\ndef f(Object... xs) { xs.size() + 10 }\n[f('a'), f('a', 1), f(1, 2, 3)]", "[1,2,13]"),
            ("def sum(int... xs) { xs.sum() }\nsum(1, 2, 3)", "6"),
            ("class S { static String kind(Number n) { 'num' }\n static String kind(String s) { 'str' } }\n[S.kind(1), S.kind('x'), S.kind(2.5)]", "[\"num\",\"str\",\"num\"]"),
            ("def f(String s) { 's' }\ndef f(Integer i) { 'i' }\nf(null as String)", "\"s\""),
        ],
    );
    check_err("groovy", &ex, &[("def f(String s) { 1 }\ndef f(Integer i) { 2 }\nf([])", "No signature of method: f()")]);
}

#[test]
fn groovy_ast_transforms() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "groovy",
        &ex,
        &[
            ("@groovy.transform.Immutable\nclass P { String name }\ndef p = new P(name: 'x')\ntry { p.name = 'y'; 'mutated' } catch (e) { e.class.simpleName + ':' + p.name }", "\"ReadOnlyPropertyException:x\""),
            ("@groovy.transform.Immutable\nclass Pt { int x; int y }\ndef a = new Pt(1, 2)\n[a.toString(), a == new Pt(1, 2)]", "[\"Pt(1, 2)\",true]"),
            ("import groovy.transform.builder.Builder\n@Builder\nclass P { String a; int b }\ndef p = P.builder().a('x').b(2).build()\n[p.a, p.b]", "[\"x\",2]"),
            ("@Singleton\nclass Cfg { int port = 8080 }\n[Cfg.instance.port, Cfg.instance.is(Cfg.getInstance())]", "[8080,true]"),
            ("@groovy.transform.Sortable\nclass V { String name; int n }\n[new V(name: 'b', n: 1), new V(name: 'a', n: 2)].sort().collect { it.name }", "[\"a\",\"b\"]"),
            ("@groovy.transform.AutoClone\nclass B { List items = [] }\ndef b = new B(items: [1])\ndef c = b.clone()\nc.items << 2\n[b.items, c.items]", "[[1],[1,2]]"),
            ("@groovy.transform.ToString(includeNames = true)\nclass P { String name; int age }\nnew P(name: 'a', age: 3).toString().startsWith('P(')", "true"),
            ("@groovy.transform.EqualsAndHashCode\nclass K { String id }\n[new K(id: 'a') == new K(id: 'a'), new K(id: 'a') == new K(id: 'b'), new K(id: 'a').hashCode() == new K(id: 'a').hashCode()]", "[true,false,true]"),
            ("@groovy.transform.TupleConstructor\nclass T { String a; String b }\nnew T('1', '2').b", "\"2\""),
            ("@groovy.transform.Canonical\nclass C { String n; int v }\ndef c = new C('a', 3)\n[c.toString(), c == new C('a', 3), [c, new C('a', 3)].unique().size()]", "[\"C(a, 3)\",true,1]"),
            ("@groovy.util.logging.Slf4j\nclass S { def run() { log.info('running'); 'ok' } }\nnew S().run()", "\"ok\""),
            ("@groovy.util.logging.Log\nclass S { def run() { log.debug('x'); 'logged' } }\nnew S().run()", "\"logged\""),
            ("@groovy.transform.CompileStatic\nclass M { int twice(int x) { x * 2 } }\nnew M().twice(4)", "8"),
            ("@groovy.transform.Field int counter = 5\ncounter + 1", "6"),
        ],
    );
}

#[test]
fn groovy_switch_and_operators() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "groovy",
        &ex,
        &[
            ("def classify(n) { switch (n) { case { it < 0 }: return 'neg'; case 0: return 'zero'; case 1..9: return 'digit'; case Integer: return 'int'; default: return 'other' } }\n[-5, 0, 7, 42, 'x'].collect { classify(it) }", "[\"neg\",\"zero\",\"digit\",\"int\",\"other\"]"),
            ("def r = switch (5) { case 1..3 -> 'low'; case 4..6 -> 'mid'; default -> 'high' }\nr", "\"mid\""),
            ("switch ('apple') { case ~/a.*/ -> 'starts a'; default -> 'other' }", "\"starts a\""),
            ("def s = switch (3) {\n  case 1, 2 -> 'small'\n  case 3 -> 'three'\n  default -> 'big'\n}\ns", "\"three\""),
            ("def t = { x -> switch (x) { case String -> 's'; case List -> 'l'; case Map -> 'm'; default -> 'o' } }\n[t('a'), t([]), t([:]), t(1)]", "[\"s\",\"l\",\"m\",\"o\"]"),
            ("def v = switch (12) { case { it % 2 == 0 } -> { def h = 12.intdiv(2); yield \"half ${h}\" }; default -> 'odd' }\nv", "\"half 6\""),
            ("switch ([1, 2]) { case [[1, 2], [3]] -> 'listed'; default -> 'no' }", "\"listed\""),
            ("def m = null\nm?['key']", "null"),
            ("def l = [1, 2]\nl?[1]", "2"),
            ("def nested = [a: [b: [c: 1]]]\nnested?.a?['b']?['c']", "1"),
            ("def flag = true\nflag ? [1] : [2]", "[1]"),
            ("def m1 = [x: 1]; def m2 = [y: 2]\n[*:m1, *:m2, z: 3]", "{\"x\":1,\"y\":2,\"z\":3}"),
            ("def base = [a: 1, b: 2]\n[*:base, b: 20]", "{\"a\":1,\"b\":20}"),
            ("def f(Map args) { args.a + args.b + args.c }\ndef m = [a: 1, b: 2]\nf(*:m, c: 3)", "6"),
        ],
    );
    check_err(
        "groovy",
        &ex,
        &[("@Grab('org.apache.commons:commons-lang3:3.12.0')\nimport org.apache.commons.lang3.StringUtils\n1", "@Grab is not supported")],
    );
}

#[test]
fn groovy_metaprogramming() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "groovy",
        &ex,
        &[
            ("class StrCat { static String shout(String self) { self.toUpperCase() + '!' } }\nuse(StrCat) { 'hi'.shout() }", "\"HI!\""),
            ("class NumCat { static int doubled(Integer self) { self * 2 } }\nclass StrCat { static String doubled(String self) { self * 2 } }\nuse(NumCat, StrCat) { [3.doubled(), 'ab'.doubled()] }", "[6,\"abab\"]"),
            ("class C { static String shout(String s) { s.toUpperCase() } }\nuse(C) { 'a'.shout() }\ntry { 'a'.shout() } catch (e) { 'gone' }", "\"gone\""),
            ("def o = new Expando(name: 'x')\no.greet = { -> \"hi ${name}\" }\no.age = 3\n[o.greet(), o.age]", "[\"hi x\",3]"),
            ("class P { String n }\ndef p = new P(n: 'a')\np.metaClass.extra = { -> n + '!' }\np.extra()", "\"a!\""),
            ("Integer.metaClass.static.answer = { -> 42 }\nInteger.answer()", "42"),
            ("String.metaClass.twice = { -> delegate * 2 }\n'ab'.twice()", "\"abab\""),
        ],
    );
}

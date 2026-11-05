//! Groovy: classes (fields, properties, constructors, methods, inheritance, static members),
//! interfaces / traits, enums and deep recursion.

mod common;
use common::*;

#[test]
fn groovy_classes() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "groovy",
        &ex,
        &[
            ("class P { String name; int age }\ndef p = new P(name: 'Al', age: 3)\np.name + p.age", "\"Al3\""),
            ("class P { def name\n P(n) { name = n } }\nnew P('x').name", "\"x\""),
            ("class P { String name\n String greet() { \"Hi $name\" } }\nnew P(name: 'Bo').greet()", "\"Hi Bo\""),
            ("class A { def hello() { 'A' } }\nclass B extends A { def hello() { super.hello() + 'B' } }\nnew B().hello()", "\"AB\""),
            ("class A { def x = 1 }\nclass B extends A { def y = 2 }\ndef b = new B()\nb.x + b.y", "3"),
            ("class C { static int twice(int n) { n * 2 } }\nC.twice(21)", "42"),
            ("class C { static final String K = 'k' }\nC.K", "\"k\""),
            ("class Counter { int n = 0\n def inc() { n++; this } }\nnew Counter().inc().inc().n", "2"),
            ("class P { private String secret = 's'\n String getSecret() { secret.toUpperCase() } }\nnew P().secret", "\"S\""),
            ("class P { def v\n void setV(x) { this.@v = x * 10 } }\ndef p = new P()\np.v = 2\np.v", "20"),
            ("class Shape { def area() { 0 } }\nclass Sq extends Shape { def s\n def area() { s * s } }\n[new Sq(s: 3), new Shape()].collect { it.area() }", "[9,0]"),
            ("class A {}\nclass B extends A {}\ndef b = new B()\n[b instanceof A, b instanceof B, b.getClass().simpleName]", "[true,true,\"B\"]"),
            ("class P { def a, b\n P(a, b) { this.a = a; this.b = b } }\ndef p = new P(1, 2)\np.a + p.b", "3"),
            ("class P { String n\n String toString() { \"P($n)\" } }\n\"${new P(n: 'z')}\"", "\"P(z)\""),
            ("class P { int v\n boolean equals(o) { o instanceof P && o.v == v }\n int hashCode() { v } }\nnew P(v: 1) == new P(v: 1)", "true"),
            ("class Base { Base() { log << 'base' }\n static log = [] }\nclass D extends Base { D() { super(); log << 'd' } }\nnew D()\nBase.log", "[\"base\",\"d\"]"),
            ("class V { def plus(V o) { new V(n: n + o.n) }\n int n }\n(new V(n: 2) + new V(n: 3)).n", "5"),
            ("class L { def items = []\n def leftShift(x) { items << x; this } }\ndef l = new L()\nl << 1 << 2\nl.items", "[1,2]"),
            ("class P { def name }\ndef ps = [new P(name: 'b'), new P(name: 'a')]\nps.sort { it.name }*.name", "[\"a\",\"b\"]"),
            ("class P { def name }\nnew P(name: 'q').properties.name", "\"q\""),
            ("interface Greeter { String greet() }\nclass En implements Greeter { String greet() { 'hello' } }\nnew En().greet()", "\"hello\""),
            ("trait Flyer { String fly() { \"${name()} flies\" } }\nclass Bird implements Flyer { String name() { 'bird' } }\nnew Bird().fly()", "\"bird flies\""),
            ("abstract class A { abstract def f()\n def g() { f() * 2 } }\nclass B extends A { def f() { 4 } }\nnew B().g()", "8"),
            ("enum Color { RED, GREEN }\nColor.GREEN.name() + Color.values().size()", "\"GREEN2\""),
            ("enum Planet { MERCURY(1), VENUS(2)\n final int n\n Planet(int n) { this.n = n }\n def twice() { n * 2 } }\nPlanet.VENUS.twice() + Planet.valueOf('MERCURY').ordinal()", "4"),
            ("enum Op { ADD, SUB }\ndef f(Op o) { switch (o) { case Op.ADD: return '+'; default: return '-' } }\nf(Op.ADD) + f(Op.SUB) + Op.SUB", "\"+-SUB\""),
            ("class P { def x = 5 }\nnew P().with { x * 2 }", "10"),
            ("class Outer { def make() { return { -> 'c' } } }\nnew Outer().make()()", "\"c\""),
            ("class P { def name }\ndef p = new P()\np.name = 'n'\np.getName() + p.hasProperty('name').name", "\"nname\""),
            ("@groovy.transform.ToString\nclass P { String a; int b }\nnew P(a: 'x', b: 1).toString()", "\"P(x, 1)\""),
            ("@groovy.transform.Canonical\nclass P { String a }\nnew P('x') == new P('x')", "true"),
            ("class Fact { static long f(int n) { n <= 1 ? 1 : n * f(n - 1) } }\nFact.f(20)", "2432902008176640000"),
            ("def fib\nfib = { n -> n < 2 ? n : fib(n - 1) + fib(n - 2) }\nfib(15)", "610"),
            ("def deep(n) { n == 0 ? 0 : 1 + deep(n - 1) }\ndeep(700)", "700"),
        ],
    );
}

#[test]
fn groovy_class_errors() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check_err(
        "groovy",
        &ex,
        &[
            ("class P {}\nnew P().nope()", "No signature of method"),
            ("class P {}\nnew P().zz", "No such property"),
            ("def f() { f() }\nf()", "StackOverflowError"),
        ],
    );
}

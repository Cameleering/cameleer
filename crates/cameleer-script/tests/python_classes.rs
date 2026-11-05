//! Python: classes (methods, constructors, inheritance, properties, special methods),
//! `with`, generators / `yield`, bare `raise`, decorators.

mod common;
use common::*;

#[test]
fn python_classes() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "python",
        &ex,
        &[
            ("class P:\n    def __init__(self, n):\n        self.n = n\n    def hi(self):\n        return 'hi ' + self.n\nP('bo').hi()", "\"hi bo\""),
            ("class C:\n    count = 0\n    def __init__(self):\n        C.count += 1\nC(); C()\nC.count", "2"),
            ("class A:\n    def who(self):\n        return 'A'\nclass B(A):\n    pass\nB().who()", "\"A\""),
            ("class A:\n    def who(self):\n        return 'A'\nclass B(A):\n    def who(self):\n        return 'B' + super().who()\nB().who()", "\"BA\""),
            ("class A:\n    def __init__(self, x):\n        self.x = x\nclass B(A):\n    def __init__(self, x, y):\n        super().__init__(x)\n        self.y = y\nb = B(1, 2)\nb.x + b.y", "3"),
            ("class P:\n    def __init__(self):\n        self._v = 1\n    @property\n    def v(self):\n        return self._v * 10\np = P()\np.v", "10"),
            ("class P:\n    def __init__(self):\n        self._v = 0\n    @property\n    def v(self):\n        return self._v\n    @v.setter\n    def v(self, x):\n        self._v = x + 1\np = P()\np.v = 4\np.v", "5"),
            ("class M:\n    @staticmethod\n    def add(a, b):\n        return a + b\nM.add(2, 3)", "5"),
            ("class M:\n    k = 7\n    @classmethod\n    def get(cls):\n        return cls.k\nM.get()", "7"),
            ("class V:\n    def __init__(self, x):\n        self.x = x\n    def __add__(self, o):\n        return V(self.x + o.x)\n(V(1) + V(2)).x", "3"),
            ("class V:\n    def __init__(self, x):\n        self.x = x\n    def __eq__(self, o):\n        return self.x == o.x\nV(1) == V(1)", "true"),
            ("class V:\n    def __init__(self, x):\n        self.x = x\n    def __str__(self):\n        return 'V(%d)' % self.x\nstr(V(3))", "\"V(3)\""),
            ("class V:\n    def __repr__(self):\n        return 'R'\nrepr(V())", "\"R\""),
            ("class L:\n    def __init__(self):\n        self.items = [1, 2, 3]\n    def __len__(self):\n        return len(self.items)\n    def __getitem__(self, i):\n        return self.items[i]\nl = L()\n[len(l), l[1]]", "[3,2]"),
            ("class A:\n    pass\nclass B(A):\n    pass\n[isinstance(B(), A), isinstance(A(), B)]", "[true,false]"),
            ("class P:\n    def __init__(self, name='x'):\n        self.name = name\n[P().name, P(name='y').name]", "[\"x\",\"y\"]"),
            ("class MyErr(Exception):\n    pass\ntry:\n    raise MyErr('bad')\nexcept MyErr as e:\n    r = 'caught ' + str(e)\nr", "\"caught bad\""),
            ("class P:\n    \"\"\"doc\"\"\"\n    def f(self):\n        return type(self).__name__\nP().f()", "\"P\""),
            ("class Counter:\n    def __init__(self):\n        self.n = 0\n    def inc(self):\n        self.n += 1\n        return self\nCounter().inc().inc().n", "2"),
            ("from dataclasses import dataclass\n@dataclass\nclass Pt:\n    x: int\n    y: int = 5\np = Pt(1)\n[p.x, p.y, Pt(1, 5) == p, repr(Pt(2, 3))]", "[1,5,true,\"Pt(x=2, y=3)\"]"),
            ("class S:\n    def __init__(self):\n        self.d = {}\n    def __setitem__(self, k, v):\n        self.d[k] = v * 2\n    def __contains__(self, k):\n        return k in self.d\ns = S()\ns['a'] = 2\n['a' in s, s.d['a']]", "[true,4]"),
            ("class It:\n    def __init__(self, n):\n        self.n = n\n    def __iter__(self):\n        return iter(range(self.n))\nlist(It(3))", "[0,1,2]"),
            ("class P:\n    pass\np = P()\np.z = 3\ngetattr(p, 'z') + (1 if hasattr(p, 'z') else 0)", "4"),
        ],
    );
}

#[test]
fn python_generators_with_and_raise() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "python",
        &ex,
        &[
            ("def g():\n    yield 1\n    yield 2\nlist(g())", "[1,2]"),
            ("def g(n):\n    i = 0\n    while i < n:\n        yield i * i\n        i += 1\nsum(g(4))", "14"),
            ("def g():\n    yield from [1, 2]\n    yield 3\n[x for x in g()]", "[1,2,3]"),
            ("def g():\n    x = yield 1\n    yield x * 2\nit = g()\nnext(it)\nit.send(5)", "10"),
            ("def nat():\n    n = 0\n    while True:\n        yield n\n        n += 1\nit = nat()\n[next(it), next(it), next(it)]", "[0,1,2]"),
            ("def g():\n    yield 1\nit = g()\nnext(it)\nr = next(it, 'done')\nr", "\"done\""),
            ("def g():\n    yield 1\nit = g()\nnext(it)\ntry:\n    next(it)\nexcept StopIteration:\n    r = 'stop'\nr", "\"stop\""),
            ("def pairs(d):\n    for k in sorted(d):\n        yield k, d[k]\ndict(pairs({'b': 2, 'a': 1}))", "{\"a\":1,\"b\":2}"),
            ("def g():\n    for i in range(3):\n        if i == 1:\n            return\n        yield i\nlist(g())", "[0]"),
            ("class R:\n    def __init__(self, n):\n        self.n = n\n    def __iter__(self):\n        for i in range(self.n):\n            yield i\nlist(R(2))", "[0,1]"),
            ("log = []\nclass Ctx:\n    def __enter__(self):\n        log.append('in')\n        return 42\n    def __exit__(self, t, v, tb):\n        log.append('out')\n        return False\nwith Ctx() as x:\n    log.append(x)\nlog", "[\"in\",42,\"out\"]"),
            ("log = []\nclass Ctx:\n    def __enter__(self):\n        return self\n    def __exit__(self, t, v, tb):\n        log.append('exit')\n        return True\nwith Ctx():\n    raise ValueError('x')\nlog", "[\"exit\"]"),
            ("log = []\nclass Ctx:\n    def __enter__(self):\n        return self\n    def __exit__(self, t, v, tb):\n        log.append(t is not None)\n        return False\ntry:\n    with Ctx():\n        raise ValueError('boom')\nexcept ValueError as e:\n    log.append(str(e))\nlog", "[true,\"boom\"]"),
            ("class C:\n    def __init__(self, n):\n        self.n = n\n    def __enter__(self):\n        return self.n\n    def __exit__(self, *a):\n        pass\nwith C(1) as a, C(2) as b:\n    r = a + b\nr", "3"),
            ("def f():\n    class C:\n        def __enter__(self):\n            return 1\n        def __exit__(self, *a):\n            pass\n    with C() as v:\n        return v + 1\nf()", "2"),
            ("try:\n    try:\n        raise KeyError('k')\n    except KeyError:\n        raise\nexcept KeyError as e:\n    r = 'outer'\nr", "\"outer\""),
            ("def deco(f):\n    def w(*a):\n        return f(*a) * 2\n    return w\n@deco\ndef three():\n    return 3\nthree()", "6"),
            ("def deep(n):\n    return 0 if n == 0 else 1 + deep(n - 1)\ndeep(800)", "800"),
        ],
    );
    check_err(
        "python",
        &ex,
        &[
            ("def f():\n    return f()\nf()", "RecursionError"),
            ("class P:\n    pass\nP().nope", "AttributeError"),
            ("raise", "RuntimeError"),
        ],
    );
}

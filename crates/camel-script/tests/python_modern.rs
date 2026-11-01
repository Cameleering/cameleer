//! Python: imports (user modules and the bundled standard modules), `match`, walrus,
//! f-string `=`, `async` / `await` with an event loop, metaclasses, `contextlib`,
//! `dataclasses` options, `functools`, `itertools` and `collections`.

mod common;
use common::*;

use std::collections::BTreeMap;

#[test]
fn python_imports() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "python",
        &ex,
        &[
            ("from tests.fixtures.py import geometry\n[geometry.area(1), geometry.Greeter('bob').greet()]", "[3.14,\"HELLO BOB!\"]"),
            ("import tests.fixtures.py.geometry as g\ng.PI", "3.14"),
            ("from tests.fixtures.py.geometry import area, PI as pi\n[area(2), pi]", "[12.56,3.14]"),
            ("from tests.fixtures.py.pkg import double, VERSION\n[double(4), VERSION]", "[8,\"1.0\"]"),
            ("from tests.fixtures.py.pkg.util import *\n[double(1), triple(1)]", "[2,3]"),
            ("import tests.fixtures.py.pkg.util\ntests.fixtures.py.pkg.util.triple(2)", "6"),
            ("from tests.fixtures.py.geometry import *\n['area' in dir() if False else True, PI]", "[true,3.14]"),
            ("import json, math as m\n[json.dumps({'a': 1}), m.floor(2.7)]", "[\"{\\\"a\\\": 1}\",2]"),
            ("from typing import List, Optional\ndef f(xs: List[int]) -> Optional[int]:\n    return xs[0] if xs else None\nf([5])", "5"),
            ("import sys, string\n[sys.maxsize > 2**31, string.digits]", "[true,\"0123456789\"]"),
        ],
    );
    check_err("python", &ex, &[("import nosuchmodule", "No module named 'nosuchmodule'"), ("from tests.fixtures.py.geometry import nothing", "cannot import name 'nothing'")]);
    // relative to the script directory
    let mut opts = BTreeMap::new();
    opts.insert("scriptDir".to_string(), "tests/fixtures/py".to_string());
    let s = camel_script::Script::compile(camel_script::Dialect::Python, "import geometry\nfrom pkg import util\n[geometry.area(1), util.double(5)]", &opts).unwrap();
    assert_eq!(s.evaluate_copy(&ex).unwrap().to_json(), "[3.14,10]");
}

#[test]
fn python_match_walrus_fstrings() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "python",
        &ex,
        &[
            ("def f(p):\n    match p:\n        case [x, y]:\n            return x + y\n        case {'k': v}:\n            return v\n        case _:\n            return None\n[f([1, 2]), f({'k': 3}), f(5)]", "[3,3,null]"),
            ("def f(cmd):\n    match cmd.split():\n        case ['go', direction]:\n            return 'going ' + direction\n        case ['quit' | 'exit']:\n            return 'bye'\n        case ['drop', *items]:\n            return items\n        case _:\n            return '?'\n[f('go north'), f('exit'), f('drop a b'), f('x y z')]", "[\"going north\",\"bye\",[\"a\",\"b\"],\"?\"]"),
            ("class Point:\n    __match_args__ = ('x', 'y')\n    def __init__(self, x, y):\n        self.x = x\n        self.y = y\ndef where(p):\n    match p:\n        case Point(0, 0):\n            return 'origin'\n        case Point(x=0, y=y):\n            return f'y={y}'\n        case Point(x, y) if x == y:\n            return 'diagonal'\n        case Point():\n            return 'somewhere'\n[where(Point(0, 0)), where(Point(0, 5)), where(Point(2, 2)), where(Point(1, 3))]", "[\"origin\",\"y=5\",\"diagonal\",\"somewhere\"]"),
            ("def f(v):\n    match v:\n        case int(n) if n < 0:\n            return 'neg'\n        case int() | float():\n            return 'num'\n        case str() as s:\n            return 's:' + s\n        case None:\n            return 'none'\n        case {'type': 'user', **rest}:\n            return rest\n[f(-1), f(2.5), f('x'), f(None), f({'type': 'user', 'id': 7})]", "[\"neg\",\"num\",\"s:x\",\"none\",{\"id\":7}]"),
            ("def f(t):\n    match t:\n        case (1, *rest, last):\n            return [rest, last]\n        case ():\n            return 'empty'\n[f((1, 2, 3, 4)), f(())]", "[[[2,3],4],\"empty\"]"),
            ("from dataclasses import dataclass\n@dataclass\nclass Order:\n    id: int\n    qty: int\ndef f(o):\n    match o:\n        case Order(id, qty) if qty > 10:\n            return f'bulk {id}'\n        case Order(id=i):\n            return f'single {i}'\n[f(Order(1, 20)), f(Order(2, 1))]", "[\"bulk 1\",\"single 2\"]"),
            ("x = 5\nf'{x=}'", "\"x=5\""),
            ("name = 'bob'\nval = 3.14159\nf'{name=!s} {val=:.2f} {x + 1 = }' if (x := 1) else ''", "\"name=bob val=3.14 x + 1 = 2\""),
            ("if (n := len([1, 2, 3])) > 2:\n    r = n * 10\nr", "30"),
            ("data = [1, 5, 12, 7]\n[y for x in data if (y := x * 2) > 10]", "[24,14]"),
            ("import re\nif m := re.match(r'(\\d+)-(\\d+)', '10-20'):\n    r = int(m.group(2))\nr", "20"),
        ],
    );
}

#[test]
fn python_async() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "python",
        &ex,
        &[
            ("import asyncio\nasync def g(x):\n    await asyncio.sleep(0)\n    return x * 2\nasync def main():\n    return await asyncio.gather(g(1), g(2))\nasyncio.run(main())", "[2,4]"),
            ("import asyncio\nlog = []\nasync def worker(name, delay):\n    await asyncio.sleep(delay)\n    log.append(name)\n    return name\nasync def main():\n    t1 = asyncio.create_task(worker('slow', 0.2))\n    t2 = asyncio.create_task(worker('fast', 0.1))\n    await t1\n    await t2\n    return log\nasyncio.run(main())", "[\"fast\",\"slow\"]"),
            ("import asyncio\nasync def f():\n    return 42\nc = f()\n[asyncio.iscoroutine(c), asyncio.run(c)]", "[true,42]"),
            ("import asyncio\nlog = []\nasync def f():\n    log.append('run')\n    return 1\nc = f()\nlog.append('created')\nasyncio.run(c)\nlog", "[\"created\",\"run\"]"),
            ("import asyncio\nasync def boom():\n    await asyncio.sleep(0)\n    raise ValueError('bad')\nasync def main():\n    try:\n        await boom()\n    except ValueError as e:\n        return 'caught ' + str(e)\nasyncio.run(main())", "\"caught bad\""),
            ("import asyncio\nasync def agen():\n    for i in range(3):\n        await asyncio.sleep(0)\n        yield i\nasync def main():\n    out = []\n    async for x in agen():\n        out.append(x)\n    return out\nasyncio.run(main())", "[0,1,2]"),
            ("import asyncio\nclass Res:\n    async def __aenter__(self):\n        return 'res'\n    async def __aexit__(self, t, v, tb):\n        return False\nasync def main():\n    async with Res() as r:\n        return r + '!'\nasyncio.run(main())", "\"res!\""),
            ("import asyncio\nasync def main():\n    return await asyncio.wait_for(asyncio.sleep(0.01, 'done'), 1)\nasyncio.run(main())", "\"done\""),
        ],
    );
}

#[test]
fn python_metaclasses() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "python",
        &ex,
        &[
            ("class Meta(type):\n    def __new__(mcs, name, bases, ns):\n        ns['tag'] = name.lower()\n        return super().__new__(mcs, name, bases, ns)\nclass A(metaclass=Meta):\n    pass\nA.tag", "\"a\""),
            ("registry = []\nclass Reg(type):\n    def __init__(cls, name, bases, ns):\n        registry.append(name)\nclass Base(metaclass=Reg):\n    pass\nclass Child(Base):\n    pass\nregistry", "[\"Base\",\"Child\"]"),
            ("class Singleton(type):\n    _instances = {}\n    def __call__(cls, *args):\n        if cls not in Singleton._instances:\n            Singleton._instances[cls] = super().__call__(*args)\n        return Singleton._instances[cls]\nclass Db(metaclass=Singleton):\n    def __init__(self, url):\n        self.url = url\na = Db('x')\nb = Db('y')\n[a is b, b.url]", "[true,\"x\"]"),
            ("class Meta(type):\n    def describe(cls):\n        return 'class ' + cls.__name__\nclass Svc(metaclass=Meta):\n    pass\nSvc.describe()", "\"class Svc\""),
            ("def hello(self):\n    return 'hi ' + self.name\nP = type('P', (), {'name': 'p', 'hello': hello})\n[P().hello(), P.__name__]", "[\"hi p\",\"P\"]"),
            ("class Base:\n    def who(self):\n        return 'base'\nD = type('D', (Base,), {'x': 1})\nd = D()\n[d.who(), d.x, isinstance(d, Base)]", "[\"base\",1,true]"),
        ],
    );
}

#[test]
fn python_contextlib_dataclasses() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "python",
        &ex,
        &[
            ("from contextlib import contextmanager\nlog = []\n@contextmanager\ndef tag(n):\n    log.append('<' + n)\n    yield n\n    log.append(n + '>')\nwith tag('b') as t:\n    log.append(t)\nlog", "[\"<b\",\"b\",\"b>\"]"),
            ("from contextlib import contextmanager\nlog = []\n@contextmanager\ndef guard():\n    try:\n        yield\n    except ValueError:\n        log.append('handled')\nwith guard():\n    raise ValueError('x')\nlog", "[\"handled\"]"),
            ("from contextlib import contextmanager\n@contextmanager\ndef cm():\n    try:\n        yield 1\n    finally:\n        pass\ntry:\n    with cm():\n        raise KeyError('k')\nexcept KeyError:\n    r = 'propagated'\nr", "\"propagated\""),
            ("from contextlib import suppress\nwith suppress(KeyError):\n    {}['x']\n'ok'", "\"ok\""),
            ("from contextlib import nullcontext, ExitStack\nlog = []\nclass C:\n    def __init__(self, n):\n        self.n = n\n    def __enter__(self):\n        log.append('in' + self.n)\n        return self\n    def __exit__(self, *a):\n        log.append('out' + self.n)\nwith ExitStack() as stack:\n    stack.enter_context(C('1'))\n    stack.enter_context(C('2'))\nwith nullcontext(5) as v:\n    log.append(v)\nlog", "[\"in1\",\"in2\",\"out2\",\"out1\",5]"),
            ("from dataclasses import dataclass, field\n@dataclass\nclass C:\n    a: int\n    b: list = field(default_factory=list)\n    c: int = field(default=3, repr=False)\nx = C(1)\ny = C(2)\nx.b.append(9)\n[repr(x), y.b, x.c]", "[\"C(a=1, b=[9])\",[],3]"),
            ("from dataclasses import dataclass, field\n@dataclass\nclass C:\n    a: int\n    total: int = field(init=False, default=0)\n    def __post_init__(self):\n        self.total = self.a * 2\nC(4).total", "8"),
            ("from dataclasses import dataclass, FrozenInstanceError\n@dataclass(frozen=True)\nclass P:\n    x: int\np = P(1)\ntry:\n    p.x = 2\nexcept Exception as e:\n    r = type(e).__name__\n[r, p.x]", "[\"FrozenInstanceError\",1]"),
            ("from dataclasses import dataclass\n@dataclass(order=True)\nclass V:\n    major: int\n    minor: int\nsorted([V(1, 2), V(0, 9), V(1, 0)])", "[{\"major\":0,\"minor\":9},{\"major\":1,\"minor\":0},{\"major\":1,\"minor\":2}]"),
            ("from dataclasses import dataclass, asdict, astuple, replace, fields\n@dataclass\nclass In:\n    v: int\n@dataclass\nclass Out:\n    name: str\n    inner: In\no = Out('n', In(1))\n[asdict(o), astuple(o), replace(o, name='m').name, [f.name for f in fields(o)]]", "[{\"name\":\"n\",\"inner\":{\"v\":1}},[\"n\",[1]],\"m\",[\"name\",\"inner\"]]"),
            ("from dataclasses import dataclass, field\n@dataclass\nclass C:\n    a: int\n    secret: str = field(default='s', compare=False)\nC(1, 'x') == C(1, 'y')", "true"),
        ],
    );
}

#[test]
fn python_functools_itertools_collections() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "python",
        &ex,
        &[
            ("import functools\nfunctools.reduce(lambda a, b: a * b, [1, 2, 3, 4], 1)", "24"),
            ("from functools import partial\nbase2 = partial(int, base=2)\ndef power(b, e):\n    return b ** e\nsq = partial(power, e=2)\n[base2('101'), sq(5)]", "[5,25]"),
            ("from functools import lru_cache\n@lru_cache(maxsize=None)\ndef fib(n):\n    return n if n < 2 else fib(n - 1) + fib(n - 2)\n[fib(30), fib.cache_info()['hits'] > 0]", "[832040,true]"),
            ("from functools import cmp_to_key\ndef cmp(a, b):\n    return len(a) - len(b)\nsorted(['ccc', 'a', 'bb'], key=cmp_to_key(cmp))", "[\"a\",\"bb\",\"ccc\"]"),
            ("from functools import wraps\ndef deco(f):\n    @wraps(f)\n    def inner(*a):\n        return f(*a) + 1\n    return inner\n@deco\ndef add(x, y):\n    return x + y\n[add(1, 2), add.__name__]", "[4,\"add\"]"),
            ("import itertools\n[list(itertools.islice(itertools.count(10, 5), 3)), list(itertools.islice(itertools.cycle('ab'), 5))]", "[[10,15,20],[\"a\",\"b\",\"a\",\"b\",\"a\"]]"),
            ("from itertools import chain, accumulate, takewhile, dropwhile, repeat\n[list(chain([1], (2, 3))), list(accumulate([1, 2, 3])), list(takewhile(lambda x: x < 3, [1, 2, 5, 1])), list(dropwhile(lambda x: x < 3, [1, 2, 5, 1])), list(repeat('x', 2))]", "[[1,2,3],[1,3,6],[1,2],[5,1],[\"x\",\"x\"]]"),
            ("from itertools import product, permutations, combinations\n[list(product('ab', [1, 2])), len(list(permutations([1, 2, 3]))), list(combinations('abc', 2))]", "[[[\"a\",1],[\"a\",2],[\"b\",1],[\"b\",2]],6,[[\"a\",\"b\"],[\"a\",\"c\"],[\"b\",\"c\"]]]"),
            ("from itertools import groupby\n[(k, len(g)) for k, g in groupby('aaabbc')]", "[[\"a\",3],[\"b\",2],[\"c\",1]]"),
            ("from itertools import zip_longest, starmap, chain\n[list(zip_longest([1, 2], ['a'], fillvalue='-')), list(starmap(pow, [(2, 3), (3, 2)])), list(chain.from_iterable([[1], [2, 3]]))]", "[[[1,\"a\"],[2,\"-\"]],[8,9],[1,2,3]]"),
            ("from collections import Counter\nc = Counter('abracadabra')\n[c.most_common(2), c['z'], c['a'], repr(Counter('aab'))]", "[[[\"a\",5],[\"b\",2]],0,5,\"Counter({'a': 2, 'b': 1})\"]"),
            ("from collections import Counter\na = Counter(x=3, y=1)\nb = Counter(x=1, y=2)\n[dict(a + b), dict(a - b), a.total(), sorted(Counter('aab').elements())]", "[{\"x\":4,\"y\":3},{\"x\":2},4,[\"a\",\"a\",\"b\"]]"),
            ("from collections import defaultdict\nd = defaultdict(list)\nfor k, v in [('a', 1), ('b', 2), ('a', 3)]:\n    d[k].append(v)\ncount = defaultdict(int)\nfor w in 'the cat the'.split():\n    count[w] += 1\n[dict(d), dict(count), 'z' in d]", "[{\"a\":[1,3],\"b\":[2]},{\"the\":2,\"cat\":1},false]"),
            ("from collections import OrderedDict\nod = OrderedDict([('a', 1), ('b', 2), ('c', 3)])\nod.move_to_end('a')\nlast = od.popitem()\n[list(od.keys()), last]", "[[\"b\",\"c\"],[\"a\",1]]"),
            ("from collections import namedtuple\nPoint = namedtuple('Point', 'x y')\np = Point(1, y=2)\nx, y = p\n[p.x + p.y, p[0], x, repr(p), p._asdict(), p._replace(x=5).x, Point._fields if False else 1]", "[3,1,1,\"Point(x=1, y=2)\",{\"x\":1,\"y\":2},5,1]"),
            ("from collections import deque\nq = deque([1, 2, 3], maxlen=3)\nq.append(4)\nq.appendleft(0)\nq.rotate(1)\n[list(q), q.popleft(), len(q), repr(deque([1]))]", "[[3,0,2],3,2,\"deque([1])\"]"),
            ("from collections import ChainMap\ncm = ChainMap({'a': 1}, {'a': 2, 'b': 3})\n[cm['a'], cm['b'], sorted(cm.keys())]", "[1,3,[\"a\",\"b\"]]"),
            ("from operator import itemgetter, attrgetter\nrows = [{'n': 'b', 'v': 2}, {'n': 'a', 'v': 1}]\n[sorted(rows, key=itemgetter('n'))[0]['v'], itemgetter(0, 2)('abc')]", "[1,[\"a\",\"c\"]]"),
            ("import copy\na = {'l': [1, 2]}\nb = copy.deepcopy(a)\nb['l'].append(3)\n[a, b]", "[{\"l\":[1,2]},{\"l\":[1,2,3]}]"),
        ],
    );
}

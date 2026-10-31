//! Python language: table-driven tests.

mod common;
use common::*;

#[test]
fn python_expressions() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "python",
        &ex,
        &[
            ("1 + 2 * 3", "7"),
            ("7 / 2", "3.5"),
            ("7 // 2", "3"),
            ("-7 // 2", "-4"),
            ("-7 % 3", "2"),
            ("2 ** 10", "1024"),
            ("'ab' * 3", "\"ababab\""),
            ("1 < 2 < 3", "true"),
            ("3 > 2 > 2", "false"),
            ("not None and 'x' or 'y'", "\"x\""),
            ("[] or 'empty'", "\"empty\""),
            ("'yes' if 3 > 2 else 'no'", "\"yes\""),
            ("2 in [1, 2, 3] and 'a' not in 'xyz'", "true"),
            ("None is None", "true"),
            ("str(3.0) + str(True) + str(None)", "\"3.0TrueNone\""),
            ("int('42') + int(3.9) + int('ff', 16)", "300"),
            ("float('1.5') * 2", "3.0"),
            ("len('hello') + len([1, 2]) + len({'a': 1})", "8"),
            ("[x * x for x in range(5) if x % 2 == 0]", "[0,4,16]"),
            ("{k: v * 2 for k, v in {'a': 1, 'b': 2}.items()}", "{\"a\":2,\"b\":4}"),
            ("sorted({x % 3 for x in range(10)})", "[0,1,2]"),
            ("sum(x for x in [1, 2, 3])", "6"),
            ("[1, 2, 3, 4, 5][1:3]", "[2,3]"),
            ("[1, 2, 3, 4, 5][::-1]", "[5,4,3,2,1]"),
            ("'camel'[-3:]", "\"mel\""),
            ("[1, 2, 3][-1]", "3"),
            ("(1, 2) + (3,)", "[1,2,3]"),
            ("f'{1 + 1} and {\"x\"!r} {3.14159:.2f} {42:>5} {1234567:,}'", "\"2 and 'x' 3.14    42 1,234,567\""),
            ("'%s is %d years, %.1f%%' % ('Bob', 30, 99.5)", "\"Bob is 30 years, 99.5%\""),
            ("'{} + {} = {total}'.format(1, 2, total=3)", "\"1 + 2 = 3\""),
            ("'{0}{1}{0}'.format('a', 'b')", "\"aba\""),
        ],
    );
}

#[test]
fn python_builtins_and_methods() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "python",
        &ex,
        &[
            ("'Hello'.upper() + 'Hello'.lower()", "\"HELLOhello\""),
            ("'  x  '.strip() + '|' + 'xxhixx'.strip('x')", "\"x|hi\""),
            ("'a b  c'.split()", "[\"a\",\"b\",\"c\"]"),
            ("'a,b,c'.split(',', 1)", "[\"a\",\"b,c\"]"),
            ("'-'.join(['a', 'b', 'c'])", "\"a-b-c\""),
            ("'banana'.replace('a', 'o')", "\"bonono\""),
            ("'banana'.count('a') + 'banana'.find('n') + 'banana'.find('z')", "4"),
            ("'abc'.startswith('a') and 'abc'.endswith('c')", "true"),
            ("'hello world'.title()", "\"Hello World\""),
            ("'42'.isdigit() and not 'x1'.isdigit()", "true"),
            ("'7'.zfill(3)", "\"007\""),
            ("min([3, 1, 2]) + max(3, 9) + abs(-4)", "14"),
            ("round(2.5) + round(3.5) + round(1.2345, 2)", "7.23"),
            ("sorted(['bb', 'a', 'ccc'], key=len, reverse=True)", "[\"ccc\",\"bb\",\"a\"]"),
            ("list(reversed([1, 2, 3]))", "[3,2,1]"),
            ("list(enumerate(['a', 'b'], 1))", "[[1,\"a\"],[2,\"b\"]]"),
            ("list(zip([1, 2], ['a', 'b']))", "[[1,\"a\"],[2,\"b\"]]"),
            ("list(map(lambda x: x + 1, [1, 2]))", "[2,3]"),
            ("list(filter(lambda x: x > 1, [1, 2, 3]))", "[2,3]"),
            ("any([0, '', 3]) and not all([1, 0])", "true"),
            ("isinstance(1, int) and isinstance('s', str) and isinstance([], (list, tuple))", "true"),
            ("d = {'a': 1}\nd['b'] = 2\nd.get('c', 0) + d.get('b')", "2"),
            ("d = {'a': 1, 'b': 2}\nlist(d.keys()) + list(d.values())", "[\"a\",\"b\",1,2]"),
            ("d = {'a': 1}\nd.update({'b': 2})\nd.pop('a')\nd", "{\"b\":2}"),
            ("l = [3, 1, 2]\nl.sort()\nl.append(4)\nl.insert(0, 0)\nl", "[0,1,2,3,4]"),
            ("l = [1, 2, 3]\nl.extend([4])\nl.remove(2)\nl.pop()\nl", "[1,3]"),
            ("[1, 2, 2, 3].count(2) + [1, 2, 3].index(3)", "4"),
            ("import json\njson.dumps({'a': [1, True, None]})", "\"{\\\"a\\\": [1, true, null]}\""),
            ("import json\njson.loads('{\"x\": {\"y\": 5}}')['x']['y']", "5"),
            ("import math\nmath.sqrt(16) + math.floor(2.7) + math.ceil(2.1)", "9.0"),
            ("import re\nre.sub(r'\\d+', '#', 'a1b22')", "\"a#b#\""),
            ("import re\nm = re.match(r'(\\w+)-(\\d+)', 'order-42')\nm.group(2)", "\"42\""),
            ("import re\nre.findall(r'\\d', 'a1b2c3')", "[\"1\",\"2\",\"3\"]"),
            ("str([1, 'a', None]) + str({'k': 'v'})", "\"[1, 'a', None]{'k': 'v'}\""),
            ("divmod(17, 5)", "[3,2]"),
            ("chr(65) + str(ord('a'))", "\"A97\""),
        ],
    );
}

#[test]
fn python_statements() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "python",
        &ex,
        &[
            ("x = 10\nif x > 5:\n    r = 'big'\nelif x > 2:\n    r = 'mid'\nelse:\n    r = 'small'\nr", "\"big\""),
            ("total = 0\nfor i in range(1, 11):\n    if i % 2:\n        continue\n    total += i\ntotal", "30"),
            ("n = 0\nwhile True:\n    n += 1\n    if n >= 5:\n        break\nn", "5"),
            ("for x in [1, 2]:\n    pass\nelse:\n    done = 'yes'\ndone", "\"yes\""),
            ("def add(a, b=10):\n    return a + b\nadd(1) + add(1, b=2)", "14"),
            ("def fact(n):\n    if n <= 1:\n        return 1\n    return n * fact(n - 1)\nfact(10)", "3628800"),
            ("def outer():\n    x = 1\n    def inner():\n        return x + 1\n    return inner()\nouter()", "2"),
            ("def f(*args):\n    return len(args)\nf(1, 2, 3)", "3"),
            ("a, b = 1, 2\na, b = b, a\n[a, b]", "[2,1]"),
            ("pairs = [(1, 'a'), (2, 'b')]\n[f'{n}{c}' for n, c in pairs]", "[\"1a\",\"2b\"]"),
            ("count = 0\ndef inc():\n    global count\n    count += 1\ninc()\ninc()\ncount", "2"),
            ("try:\n    x = 1 / 0\nexcept ZeroDivisionError as e:\n    x = str(e)\nx", "\"division by zero\""),
            ("try:\n    {}['k']\nexcept KeyError:\n    r = 'missing'\nfinally:\n    r = r + '!'\nr", "\"missing!\""),
            ("try:\n    raise ValueError('bad value')\nexcept Exception as e:\n    msg = str(e)\nmsg", "\"bad value\""),
            ("x = [1, 2, 3]\nx[0] = 9\nx", "[9,2,3]"),
            ("# comment\nx = 1  # trailing\nx", "1"),
            ("square = lambda v: v * v\nsquare(9)", "81"),
            ("if True: y = 'inline'\ny", "\"inline\""),
            ("matrix = [[1, 2], [3, 4]]\n[row[i] for row in matrix for i in range(2)]", "[1,2,3,4]"),
        ],
    );
}

#[test]
fn python_exchange_bindings() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "python",
        &ex,
        &[
            ("body", "\"Hello World\""),
            ("body.upper()", "\"HELLO WORLD\""),
            ("headers['foo']", "\"bar\""),
            ("headers.get('num') + 1", "6"),
            ("'foo' in headers", "true"),
            ("request.headers['foo']", "\"bar\""),
            ("exchange.getIn().getBody()", "\"Hello World\""),
            ("exchange.getProperty('p1')", "\"v1\""),
            ("properties['p1']", "\"v1\""),
            ("variables['v1']", "\"x\""),
            ("f\"{body} / {headers['foo']}\"", "\"Hello World / bar\""),
        ],
    );
    let ex = order_exchange(&ctx);
    check(
        "python",
        &ex,
        &[
            ("body['customer']['name']", "\"Alice\""),
            ("sum(i['qty'] * i['price'] for i in body['items'])", "30.25"),
            ("[i['sku'] for i in body['items'] if i['qty'] > 1]", "[\"A1\",\"C3\"]"),
        ],
    );
    let mut ex = exchange(&ctx);
    let s = camel_script::Script::compile_named("python", "exchange.getIn().setBody(body.lower())\nheaders['added'] = len(body)\nexchange.setProperty('p2', [1, 2])").unwrap();
    s.evaluate_mut(&mut ex).unwrap();
    assert_eq!(ex.body().to_string(), "hello world");
    assert_eq!(ex.header("added").unwrap().to_json(), "11");
    assert_eq!(ex.property("p2").unwrap().to_json(), "[1,2]");
}

#[test]
fn python_errors_with_positions() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check_err(
        "python",
        &ex,
        &[
            ("x = (1 +", "Python syntax error at line 1"),
            ("if True:\nx = 1", "expected an indented block"),
            ("if True:\n    x = 1\n  y = 2", "unindent does not match any outer indentation level"),
            ("x = 1\nprint(undefined_name)", "NameError: name 'undefined_name' is not defined (line 2, column 7)"),
            ("'a' + 1", "TypeError: can only concatenate str (not \"int\") to str"),
            ("[1, 2][5]", "IndexError: list index out of range"),
            ("{'a': 1}['b']", "KeyError: 'b'"),
            ("int('abc')", "ValueError: invalid literal for int() with base 10: 'abc'"),
            ("1 / 0", "ZeroDivisionError: division by zero"),
            ("None.upper()", "AttributeError: 'NoneType' object has no attribute 'upper'"),
            ("raise ValueError('custom')", "ValueError: custom"),
        ],
    );
}

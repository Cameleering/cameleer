//! OGNL, MVEL and SpEL: table-driven tests.

mod common;
use common::*;

#[test]
fn ognl_expressions() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "ognl",
        &ex,
        &[
            ("request.body", "\"Hello World\""),
            ("request.headers.foo", "\"bar\""),
            ("request.headers['foo'] == 'bar'", "true"),
            ("request.headers.foo eq 'bar' and request.headers.num gt 3", "true"),
            ("request.headers.num + 1", "6"),
            ("request.body.length()", "11"),
            ("request.body.toUpperCase()", "\"HELLO WORLD\""),
            ("getRequest().getBody()", "\"Hello World\""),
            ("exchange.properties.p1", "\"v1\""),
            ("properties.p1", "\"v1\""),
            ("property('p1')", "\"v1\""),
            ("exchange.getIn().getHeader('foo')", "\"bar\""),
            ("7 / 2", "3"),
            ("7.0 / 2", "3.5"),
            ("headers.num == '5'", "true"),
            ("request.headers.num > 3 ? 'big' : 'small'", "\"big\""),
            ("{1, 2, 3}.size()", "3"),
            ("{1, 2, 3}.{#this * 2}", "[2,4,6]"),
            ("{1, 2, 3, 4}.{? #this > 2}", "[3,4]"),
            ("{1, 2, 3, 4}.{^ #this > 1}", "2"),
            ("#{'a': 1, 'b': 2}['b']", "2"),
            ("3 in {1, 2, 3} and 4 not in {1, 2, 3}", "true"),
            ("#x = 5, #x * 2", "10"),
            ("!(1 == 2) && not false", "true"),
            ("'a' + 'b' + 1", "\"ab1\""),
        ],
    );
    let ex = order_exchange(&ctx);
    check(
        "ognl",
        &ex,
        &[
            ("request.body.customer.name", "\"Alice\""),
            ("request.body.items.{sku}", "[\"A1\",\"B2\",\"C3\"]"),
            ("request.body.items.{? qty > 1}.size()", "2"),
            ("request.body.items[0].price", "10.5"),
            ("request.body.tags.contains('web')", "true"),
        ],
    );
}

#[test]
fn mvel_expressions() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "mvel",
        &ex,
        &[
            ("request.body", "\"Hello World\""),
            ("request.headers.foo == 'bar'", "true"),
            ("request.headers['num'] * 2", "10"),
            ("headers.foo", "\"bar\""),
            ("exchange.properties.p1", "\"v1\""),
            ("in.body.toUpperCase()", "\"HELLO WORLD\""),
            ("request.body contains 'World'", "true"),
            ("request.headers.missing == empty", "true"),
            ("request.body != empty", "true"),
            ("request.headers.missing == null && request.headers.missing == nil", "true"),
            ("7 / 2", "3"),
            ("x = 10; y = 20; x + y", "30"),
            ("def square(n) { n * n }; square(7)", "49"),
            ("[1, 2, 3].size()", "3"),
            ("['a': 1, 'b': 2].b", "2"),
            ("{1, 2, 3}[1]", "2"),
            ("r = 0; foreach (x : [1, 2, 3]) { r += x; } r", "6"),
            ("if (request.headers.num > 3) { 'big' } else { 'small' }", "\"big\""),
            ("request.body is String", "true"),
            ("headers.num == '5'", "true"),
            ("request.body.length() > 5 ? 'long' : 'short'", "\"long\""),
        ],
    );
    let ex = order_exchange(&ctx);
    check(
        "mvel",
        &ex,
        &[
            ("request.body.customer.name", "\"Alice\""),
            ("(sku in request.body.items)", "[\"A1\",\"B2\",\"C3\"]"),
            ("request.body.items[2].qty", "5"),
            ("request.body.customer.vip == true", "true"),
        ],
    );
}

#[test]
fn spel_expressions() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "spel",
        &ex,
        &[
            ("#{request.body}", "\"Hello World\""),
            ("#{request.headers['foo'] == 'bar'}", "true"),
            ("#{request.headers.foo}", "\"bar\""),
            ("Hello #{request.headers.foo}!", "\"Hello bar!\""),
            ("#{request.headers.num + 1}", "6"),
            ("request.body.length()", "11"),
            ("#{exchange.properties['p1']}", "\"v1\""),
            ("#{properties['p1']}", "\"v1\""),
            ("#{message.body}", "\"Hello World\""),
            ("#{body.toUpperCase()}", "\"HELLO WORLD\""),
            ("#{headers.num gt 3 and headers.num le 5}", "true"),
            ("#{10 div 3} #{10 mod 3}", "\"3 1\""),
            ("#{T(java.lang.Math).max(3, 8)}", "8"),
            ("#{T(Integer).parseInt('12') * 2}", "24"),
            ("#{{1, 2, 3, 4}.?[#this > 2]}", "[3,4]"),
            ("#{{1, 2, 3}.![#this * 10]}", "[10,20,30]"),
            ("#{{1, 2, 3, 4}.^[#this > 1]}", "2"),
            ("#{{1, 2, 3, 4}.$[#this < 4]}", "3"),
            ("#{{a: 1, b: 2}['b']}", "2"),
            ("#{request.headers.missing ?: 'dflt'}", "\"dflt\""),
            ("#{request.headers.missing?.length()}", "null"),
            ("#{'abc' matches '[a-c]+'}", "true"),
            ("#{5 between {1, 10}}", "true"),
            ("#{request.body instanceof T(String)}", "true"),
            ("#{'It''s'}", "\"It's\""),
            ("#{#root.request.headers.foo}", "\"bar\""),
            ("#{!false}", "true"),
        ],
    );
    let ex = order_exchange(&ctx);
    check(
        "spel",
        &ex,
        &[
            ("#{request.body.customer.name}", "\"Alice\""),
            ("#{request.body.items.?[qty > 1].![sku]}", "[\"A1\",\"C3\"]"),
            ("#{request.body.items[1].sku}", "\"B2\""),
            ("#{request.body.tags.size()}", "2"),
        ],
    );
}

#[test]
fn java_el_errors() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check_err("spel", &ex, &[("#{request.body.nope()}", "EL1004E"), ("#{(1 + }", "SpEL syntax error at line 1, column 8"), ("#{unknownThing}", "EL1008E")]);
    check_err("ognl", &ex, &[("request.headers.missing.foo", "OgnlException"), ("request.body.(", "OGNL syntax error")]);
    check_err("mvel", &ex, &[("x = ", "MVEL syntax error"), ("request.headers.missing.foo", "null pointer")]);
}

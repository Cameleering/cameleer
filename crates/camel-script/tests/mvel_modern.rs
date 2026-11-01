//! MVEL: templates (orb tags) with the `mvel:` component, and interceptors.

mod common;
use common::*;

use camel_core::prelude::*;
use camel_script::mvel::{register_interceptor, render_template, MvelInterceptor};

fn model() -> Value {
    camel_core::json::parse(
        r#"{"body": {"id": 7, "items": [{"sku": "A1", "qty": 2}, {"sku": "B2", "qty": 1}], "vip": true, "tags": []},
            "headers": {"name": "Alice", "count": 3, "shop": "Camel Shop"},
            "variables": {"region": "EU"}}"#,
    )
    .unwrap()
}

#[test]
fn mvel_templates() {
    let m = model();
    let cases: &[(&str, &str)] = &[
        ("Hello @{headers.name}!", "Hello Alice!"),
        ("@{body.id + 1}", "8"),
        ("@{headers.count * 2} items", "6 items"),
        ("@{headers.missing}", "null"),
        ("@@{not a tag} and @ alone", "@{not a tag} and @ alone"),
        ("@if{body.vip}VIP@end{}", "VIP"),
        ("@if{headers.count > 5}many@else{}few@end{}", "few"),
        ("@if{headers.count == 1}one@elseif{headers.count == 3}three@else{}other@end{}", "three"),
        ("@if{headers.count == 1}one@else{headers.count == 3}three@end{}", "three"),
        ("@foreach{i : body.items}[@{i.sku}]@end{}", "[A1][B2]"),
        ("@foreach{i : body.items}@{i.sku}@end{', '}", "A1, B2"),
        ("@foreach{i : body.tags}x@end{}empty", "empty"),
        ("@foreach{n : [1, 2, 3]}@{n * n}@end{'-'}", "1-4-9"),
        ("@foreach{a : ['x', 'y'], b : [1, 2]}@{a}@{b}@end{' '}", "x1 y2"),
        ("@foreach{i : body.items}@if{i.qty > 1}@{i.sku}*@{i.qty}@end{}@end{}", "A1*2"),
        ("@code{total = 0; foreach (i : body.items) { total += i.qty; }}Total: @{total}", "Total: 3"),
        ("@code{def greet(n) { 'Hi ' + n }}@{greet(headers.name)}", "Hi Alice"),
        ("a@comment{ ignored @{boom} }b", "ab"),
        ("@declare{'line'}- @{headers.name}\n@end{}@includeNamed{'line'}@includeNamed{'line'}", "- Alice\n- Alice\n"),
        ("@{variables.region}", "EU"),
        ("@{body.items[0].sku}", "A1"),
        ("@{body.items.size()} / @{body.items[1].qty}", "2 / 1"),
        ("@{headers.name.toUpperCase()}", "ALICE"),
        ("@{headers.name == 'Alice' ? 'yes' : 'no'}", "yes"),
        ("@{body.items contains body.items[0]}", "true"),
        ("@{headers.name is String}", "true"),
        ("@{ (sku in body.items) }", "[A1, B2]"),
        ("Line1\n@if{true}  kept  @end{}\nLine3", "Line1\n  kept  \nLine3"),
        ("@{[1, 2, 3]}", "[1, 2, 3]"),
        ("@{['a': 1]}", "{a=1}"),
        ("@code{x = 'v'}@{x}@{x}", "vv"),
    ];
    let mut failures = vec![];
    for (tpl, expected) in cases {
        match render_template(tpl, &m) {
            Ok(out) if out == *expected => {}
            Ok(out) => failures.push(format!("{:?}: expected {:?} got {:?}", tpl, expected, out)),
            Err(e) => failures.push(format!("{:?}: error {}", tpl, e.message)),
        }
    }
    for (tpl, frag) in [("@if{true}open", "without @end"), ("@unknown{x}", "unknown orb tag"), ("@end{}", "unexpected"), ("@{1 +}", "syntax")] {
        match render_template(tpl, &m) {
            Ok(out) => failures.push(format!("{:?}: expected an error, got {:?}", tpl, out)),
            Err(e) if e.message.contains(frag) => {}
            Err(e) => failures.push(format!("{:?}: error {:?} lacks {:?}", tpl, e.message, frag)),
        }
    }
    assert!(failures.is_empty(), "{} template failures:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn mvel_component() {
    let ctx = common::ctx();
    ctx.add_routes(|rb| {
        rb.from("direct:order").to("mvel:tests/fixtures/mvel/order.mvel");
        rb.from("direct:header").to("mvel:tests/fixtures/mvel/order.mvel?allowTemplateFromHeader=true");
    })
    .unwrap();
    ctx.start().unwrap();
    let body = camel_core::json::parse(r#"{"id": 7, "items": [{"sku": "A1", "qty": 2}, {"sku": "B2", "qty": 1}]}"#).unwrap();
    let pt = ctx.create_producer_template();
    let out = pt.request_body_and_headers("direct:order", body, vec![("name", Value::from("Alice")), ("shop", Value::from("Camel Shop"))]).unwrap();
    assert_eq!(out, Value::from("Dear Alice, your order 7 has 2 items: A1x2, B2x1. -- Camel Shop"));
    let out = pt.request_body_and_headers("direct:header", "x", vec![("CamelMvelTemplate", Value::from("body=@{body} via @{exchange.in.body}"))]).unwrap();
    assert_eq!(out, Value::from("body=x via x"));
    ctx.stop().unwrap();
}

#[test]
fn mvel_interceptors() {
    register_interceptor(
        "Double",
        MvelInterceptor::new().after(|_ctx, v| Ok(match v {
            Value::Int(i) => Value::Int(i * 2),
            other => other,
        })),
    );
    register_interceptor(
        "Seed",
        MvelInterceptor::new().before(|ctx| {
            ctx.variables.insert("seed".into(), Value::Int(100));
            Ok(())
        }),
    );
    register_interceptor(
        "Audit",
        MvelInterceptor::new()
            .before(|ctx| {
                let n = ctx.variables.get("calls").and_then(|v| v.as_i64()).unwrap_or(0);
                ctx.variables.insert("calls".into(), Value::Int(n + 1));
                Ok(())
            })
            .after(|ctx, v| {
                ctx.variables.insert("last".into(), v.clone());
                Ok(v)
            }),
    );
    register_interceptor("Deny", MvelInterceptor::new().before(|ctx| Err(CamelError::illegal_state(format!("{} denied", ctx.name)))));
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "mvel",
        &ex,
        &[
            ("@Double 21", "42"),
            ("@Double x = 5", "10"),
            ("@Seed y = seed + 1; y", "101"),
            ("calls = 0; @Audit a = 1; @Audit b = 2; [calls, last]", "[2,2]"),
            ("total = 0; foreach (n : [1, 2, 3]) { @Audit total += n; } [total, calls, last]", "[6,3,6]"),
            ("@Double request.headers.num", "10"),
        ],
    );
    check_err("mvel", &ex, &[("@Deny 1", "Deny denied"), ("@Missing 1", "no interceptor named 'Missing'")]);
    // registry-bound interceptors take precedence
    ctx.registry().bind("Double", MvelInterceptor::new().after(|_c, _v| Ok(Value::from("from registry"))));
    check("mvel", &exchange(&ctx), &[("@Double 1", "\"from registry\"")]);
}

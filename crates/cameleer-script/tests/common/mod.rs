//! Shared helpers for the language test suites.
#![allow(dead_code)]

use cameleer_core::prelude::*;

/// A context with the script languages registered.
pub fn ctx() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_script::register(&ctx);
    ctx
}

/// An exchange with body `Hello World`, headers `foo=bar`, `num=5`, property `p1=v1` and
/// variable `v1=x`.
pub fn exchange(ctx: &CamelContext) -> Exchange {
    let mut ex = Exchange::with_body(Some(ctx.clone()), "Hello World");
    ex.set_header("foo", "bar");
    ex.set_header("num", 5);
    ex.set_property("p1", "v1");
    ex.set_property("CamelVariable.v1", "x");
    ex
}

/// An exchange with a JSON-like order body.
pub fn order_exchange(ctx: &CamelContext) -> Exchange {
    let mut ex = exchange(ctx);
    ex.message.body = cameleer_core::json::parse(
        r#"{"id": 7, "customer": {"name": "Alice", "vip": true}, "items": [{"sku": "A1", "qty": 2, "price": 10.5}, {"sku": "B2", "qty": 1, "price": 3.0}, {"sku": "C3", "qty": 5, "price": 1.25}], "tags": ["new", "web"]}"#,
    )
    .unwrap();
    ex
}

/// Evaluate every case and assert the JSON rendering of the result; reports all failures.
pub fn check(lang: &str, ex: &Exchange, cases: &[(&str, &str)]) {
    let mut failures = Vec::new();
    for (src, expected) in cases {
        match cameleer_script::eval(lang, src, ex) {
            Ok(v) => {
                let got = v.to_json();
                if got != *expected {
                    failures.push(format!("{}: `{}`\n    expected {}\n    got      {}", lang, src, expected, got));
                }
            }
            Err(e) => failures.push(format!("{}: `{}`\n    expected {}\n    error    {}: {}", lang, src, expected, e.kind, e.message)),
        }
    }
    if !failures.is_empty() {
        panic!("{} of {} {} cases failed:\n{}", failures.len(), cases.len(), lang, failures.join("\n"));
    }
}

/// Evaluate every case and assert that it fails with a message containing the fragment.
pub fn check_err(lang: &str, ex: &Exchange, cases: &[(&str, &str)]) {
    let mut failures = Vec::new();
    for (src, fragment) in cases {
        match cameleer_script::eval(lang, src, ex) {
            Ok(v) => failures.push(format!("{}: `{}` expected error containing '{}', got {}", lang, src, fragment, v.to_json())),
            Err(e) => {
                if !e.message.contains(fragment) {
                    failures.push(format!("{}: `{}` expected error containing '{}', got {}: {}", lang, src, fragment, e.kind, e.message));
                }
            }
        }
    }
    if !failures.is_empty() {
        panic!("{} of {} error cases failed:\n{}", failures.len(), cases.len(), failures.join("\n"));
    }
}

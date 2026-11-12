# Ref language

> The `ref` language looks up an expression or predicate by name in the registry and evaluates it, so routes can reuse logic written in Rust.

**Name:** `ref` · **Crate:** `cameleer-core` · **Cargo feature:** none, always included ([build features](../guides/build-features.md)) · **Apache Camel:** [Ref](https://camel.apache.org/components/next/languages/ref-language.html)

## When to use it

- Keep a routing rule in Rust code (a scoring function, a feature flag, a policy check) and use
  it by name from YAML routes.
- Share one expression between many routes.

## Syntax

The expression text is the registry name. At evaluation time Cameleer looks up, in order:

1. an `Arc<dyn Expression>` bound under that name: it is evaluated;
2. an `Arc<dyn Predicate>`: as an expression it returns `true` or `false`;
3. any other bound value (for example a bean method or a plain value).

When nothing is bound under the name, the evaluation fails with `NoSuchBeanException`. When
used as a predicate, a bound predicate is called directly; otherwise the expression result is
tested for truth.

## Examples

The examples below use two registry entries bound in Rust (see [Rust DSL](#rust-dsl)): an
expression `shippingCost` and a predicate `isVip`.

<!-- example: ref-expression-and-predicate -->
```yaml
- route:
    id: ref-demo
    from:
      uri: direct:ref
      steps:
        - setHeader:
            name: shipping
            ref: shippingCost
        - choice:
            when:
              - ref: isVip
                steps:
                  - setBody:
                      simple: "VIP order, shipping ${header.shipping}"
            otherwise:
              steps:
                - setBody:
                    simple: "order, shipping ${header.shipping}"
```

<!-- input: ref-expression-and-predicate {"headers":{"customerTier":"gold","weightKg":3}} -->
```text
order
```

<!-- output: ref-expression-and-predicate -->
```text
VIP order, shipping 0
```

<!-- input: ref-expression-and-predicate#2 {"headers":{"customerTier":"basic","weightKg":3}} -->
```text
order
```

<!-- output: ref-expression-and-predicate#2 -->
```text
order, shipping 15
```

## Options

The ref language has no options.

## Rust DSL

Bind the expression and the predicate, then refer to them by name:

```rust
let reg = ctx.registry();
let shipping: Arc<dyn Expression> = Arc::new(|ex: &Exchange| -> Result<Value> {
    let vip = ex.header("customerTier").map(|t| t.to_string() == "gold").unwrap_or(false);
    let kg = ex.header("weightKg").and_then(|w| w.as_i64()).unwrap_or(0);
    Ok(Value::Int(if vip { 0 } else { 5 * kg }))
});
reg.bind("shippingCost", shipping);
let vip: Arc<dyn Predicate> = Arc::new(cameleer_core::spi::FnPredicate(|ex: &Exchange| {
    Ok(ex.header("customerTier").map(|t| t.to_string() == "gold").unwrap_or(false))
}));
reg.bind("isVip", vip);
```

```rust
b.from("direct:ref-rust")
    .filter(ref_expression("isVip"))
    .set_body(ref_expression("shippingCost"));
```

## Common questions

### How do I call my own Rust code from a YAML route?

Bind an `Arc<dyn Expression>` or `Arc<dyn Predicate>` in the registry and use `ref: name`. For
functions that compute a value from the exchange, the [bean language](bean.md) with
`bind_bean_method` is an alternative.

### What happens if the name is not bound?

Evaluation fails with `NoSuchBeanException`.

## Related

- [Bean language](bean.md), [Simple language](simple.md) (`${ref:name}`)
- [Apache Camel Ref language](https://camel.apache.org/components/next/languages/ref-language.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.

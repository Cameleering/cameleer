# Bean language

> The `bean` language (also named `method`) calls a function registered in the registry and uses its return value as the expression result.

**Name:** `bean` · **Also:** `method` · **Crate:** `cameleer-core` · **Cargo feature:** none, always included ([build features](../guides/build-features.md)) · **Apache Camel:** [Bean method](https://camel.apache.org/components/next/languages/bean-language.html)

## When to use it

- Compute a value in Rust (a price, a risk score, a lookup in a local cache) and use it in a
  YAML route wherever an expression is expected.
- Route with a Rust function as the predicate.
- Replace Java bean calls when you port Camel routes: `method: {ref: pricing, method: discount}`.

## Syntax

Cameleer has no Java classes, so "beans" are functions bound in the registry with
`cameleer_core::language::bind_bean_method(registry, name, |ex| ...)`. The expression names
the bean and, optionally, the method:

| text | looks up |
|---|---|
| `pricing.discount`, `pricing::discount`, `pricing?method=discount` | `pricing.discount`, then `pricing::discount`, then `pricing` |
| `pricing` | `pricing` |

A parameter list in the method name (`discount(${body})`) is stripped: the function always
receives the whole exchange. A registered processor can be used too: it runs on a copy of the
exchange and its resulting body is the value.

In YAML the language key is `method:` (`bean` is the name of the bean processor step). Use
`method: pricing.discount`, or the mapping form with `ref` and `method`. In
[Simple](simple.md) the same call is `${bean:pricing.discount}`.

## Examples

The registry holds two functions bound in Rust (see [Rust DSL](#rust-dsl)):
`pricing.discount` and `risk.isHigh`.

<!-- example: bean-method-call -->
```yaml
- route:
    id: bean-demo
    from:
      uri: direct:bean
      steps:
        - setHeader:
            name: discount
            method:
              ref: pricing
              method: discount
        - choice:
            when:
              - method: risk.isHigh
                steps:
                  - setBody:
                      simple: "manual review (discount ${header.discount}%)"
            otherwise:
              steps:
                - setBody:
                    simple: "auto approve (discount ${header.discount}%)"
```

<!-- input: bean-method-call -->
```text
120
```

<!-- output: bean-method-call -->
```text
auto approve (discount 10%)
```

<!-- input: bean-method-call#2 -->
```text
5000
```

<!-- output: bean-method-call#2 -->
```text
manual review (discount 10%)
```

## Options

| option | default | description |
|---|---|---|
| `ref` | (none) | The bean name, when the expression text is empty. |
| `method` | (none) | The method name; overrides a method given in the text. |
| `beanType`, `scope` | | Not used by the language (there are no classes to instantiate). |

## Rust DSL

Bind the functions:

```rust
let reg = ctx.registry();
cameleer_core::language::bind_bean_method(reg, "pricing.discount", |ex: &Exchange| -> Result<Value> {
    let amount = ex.body().to_string().parse::<i64>().unwrap_or(0);
    Ok(Value::Int(if amount >= 100 { 10 } else { 0 }))
});
cameleer_core::language::bind_bean_method(reg, "risk.isHigh", |ex: &Exchange| -> Result<Value> {
    Ok(Value::Bool(ex.body().to_string().parse::<i64>().unwrap_or(0) > 1000))
});
```

Use them in a route:

```rust
b.from("direct:bean-rust")
    .set_header("discount", bean_method("pricing", "discount"))
    .set_body(method("risk.isHigh"));
```

## Common questions

### How do I call a Rust function from YAML?

Bind it with `bind_bean_method(ctx.registry(), "name.method", |ex| ...)` and use
`method: name.method` in the route.

### Can I pass arguments to the method?

Not as a parameter list. The function receives the exchange, so read the body, headers or
properties from it; set them with `setHeader` before the call if needed.

### What is the difference between the `bean` language and the `bean` step?

The language returns a value for an expression (`setHeader`, `filter`, `choice`...). The
`bean` step calls the bean as a processor and sets the body.

## Related

- [Ref language](ref.md), [Simple language](simple.md) (`${bean:name.method}`)
- [Apache Camel Bean language](https://camel.apache.org/components/next/languages/bean-language.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.

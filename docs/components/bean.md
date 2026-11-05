# Bean component

> The `bean` component calls your own Rust code, registered in the registry under a name, from a route.

**Scheme:** `bean:` · **Also:** `class:` (see [class](class.md)) · **Crate:** `cameleer-core` ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** [Bean](https://camel.apache.org/components/next/bean-component.html)

## When to use it

- Put business logic (pricing, scoring, validation) in plain Rust functions and call them from YAML or Rust routes.
- Give an AI agent route a deterministic tool, such as a fraud score, that runs in-process.
- Reuse one service object with several named methods (`?method=score`, `?method=explain`).

## URI format

```text
bean:beanName[?method=name]
```

## Example

<!-- example: bean-fraud-score -->
```yaml
- route:
    id: score-payment
    from:
      uri: direct:score
      steps:
        - to: bean:fraudScorer?method=score
        - setBody:
            simple: "risk=${body}"
```

The bean is registered from Rust before the routes start. Methods added with `method_value` return the
new body:

```rust
use cameleer_core::component::bean::{bind_bean, BeanMethods};
use cameleer_core::prelude::*;

pub fn register(ctx: &CamelContext) {
    let scorer = BeanMethods::new()
        .method_value("score", |ex: &Exchange| {
            let amount: f64 = ex.body().to_string().parse().unwrap_or(0.0);
            Ok(Value::from(if amount > 1000.0 { "high" } else { "low" }))
        })
        .method("tag", |ex: &mut Exchange| {
            ex.set_header("scored", true);
            Ok(())
        });
    bind_bean(ctx.registry(), "fraudScorer", scorer);
}
```

Sending `2500` to `direct:score` returns `risk=high`.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("direct:score")
            .route_id("score-payment")
            .to("bean:fraudScorer?method=score")
            .set_body(simple("risk=${body}"));
    })
}
```

## Options

| option | default | description |
|---|---|---|
| `method` | | The method to call. `score(${body})` is read as `score`; argument binding is up to the bean. Without it, the bean's only or default method is called. |
| `scope`, `cache`, `multiParameterArray`, `lazyStartProducer`, `parameters.*`, `bean.*` | | Accepted and ignored. |

A registry entry can be:

- a `BeanMethods` value: named closures (`method` works on the exchange, `method_value` returns the body);
- any type implementing the `Bean` trait, bound with `bind_bean`;
- a `Processor` bound with `Registry::bind_processor` (the method name is ignored).

## Message headers

| header | direction | description |
|---|---|---|
| `CamelBeanMethodName` | in | Overrides the `method` option for this message. |

## Run it with Docker

Beans are Rust code, so they need a Rust program that registers them (build your own binary with
`cameleer-main` and call `bind_bean` in `main.configure(...)`). The stock `pmsudhi/cameleer` image has no
custom beans; use [language](language.md) or a `script` step there instead.

## Common questions

### Why do I get NoSuchBeanException?

Nothing is bound under that name in the registry. Bind it before the route first uses it.

### How do I call a different method per message?

Set the header `CamelBeanMethodName`, for example from a `choice`.

### What happens when the method does not exist?

The exchange fails with `MethodNotFoundException`, which lists the methods of the bean.

## Related

- [class](class.md), [ref](ref.md), [language](language.md)
- [Apache Camel Bean component](https://camel.apache.org/components/next/bean-component.html)

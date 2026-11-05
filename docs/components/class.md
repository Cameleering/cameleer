# Class component

> The `class` component calls a bean by name, like [bean](bean.md); in Cameleer the "class name" is looked up in the registry.

**Scheme:** `class:` · **Crate:** `cameleer-core` ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** [Class](https://camel.apache.org/components/next/class-component.html)

## When to use it

- Keep Camel routes that use `class:com.example.Greeter` working: bind a Rust bean under that name.
- Any case where [bean](bean.md) fits; the two schemes behave the same.

Difference from Apache Camel: Camel creates a new instance of a Java class. Rust has no runtime class
loading, so Cameleer resolves the name in the registry, exactly like `bean:`.

## URI format

```text
class:name[?method=methodName]
```

## Example

<!-- example: class-greeter -->
```yaml
- route:
    id: greet
    from:
      uri: direct:greet
      steps:
        - to: class:com.example.Greeter?method=greet
```

Register the bean under the class name:

```rust
use cameleer_core::component::bean::{bind_bean, BeanMethods};
use cameleer_core::prelude::*;

pub fn register(ctx: &CamelContext) {
    let greeter = BeanMethods::new().method_value("greet", |ex: &Exchange| Ok(Value::from(format!("Hello {}", ex.body()))));
    bind_bean(ctx.registry(), "com.example.Greeter", greeter);
}
```

Sending `Ada` returns `Hello Ada`.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("direct:greet").route_id("greet").to("class:com.example.Greeter?method=greet");
    })
}
```

## Options

The same as [bean](bean.md#options): `method`; `scope`, `cache`, `parameters.*` and `bean.*` are accepted
and ignored.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelBeanMethodName` | in | Overrides the `method` option. |

## Run it with Docker

Like [bean](bean.md), the bean must be registered by a Rust program, so the stock image cannot run this
route on its own.

## Common questions

### Does `class:` create a new object per call?

No. It calls the object bound in the registry under that name.

## Related

- [bean](bean.md), [ref](ref.md)
- [Apache Camel Class component](https://camel.apache.org/components/next/class-component.html)

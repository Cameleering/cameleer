# SpEL language

> The `spel` language evaluates Spring Expression Language expressions and `#{...}` templates against the exchange.

**Name:** `spel` · **Crate:** `cameleer-script` · **Cargo feature:** [`script`](../guides/build-features.md) ·
**Apache Camel:** [SpEL](https://camel.apache.org/components/next/languages/spel-language.html)

## When to use it

- Port Spring-based Camel routes that use SpEL.
- Build a string with embedded expressions: `Hello #{request.body}`.

## Syntax

Cameleer interprets SpEL itself, without Spring or a JVM. Text with `#{...}` blocks is a
template; a lone `#{expr}` returns the typed value (a number stays a number); text without
`#{` is one plain expression. Supported: property and index navigation, method calls, `#root`,
`#this` and `#variables`, `@bean` references from the Camel registry, `T(Type)` static calls for
known JDK types, `new` for JDK collections, `?.`, `?:`, the ternary operator, list `{1, 2}` and
map `{k: v}` literals, selection `.?[]`, projection `.![]`, `matches`, `between`, `instanceof`
and word operators (`eq`, `div`, `mod`...). Arbitrary Java classes are not available.

The root object has `request`, `response`, `message`, `exchange`, `body`, `headers` and
`properties`.

## Examples

<!-- example: spel-greeting -->
```yaml
- route:
    id: spel-demo
    from:
      uri: direct:spel
      steps:
        - setHeader:
            name: tier
            spel: "#{request.headers.amount > 100 ? 'gold' : 'standard'}"
        - setBody:
            spel: "Hello #{request.body}, you are #{request.headers.tier}"
```

<!-- input: spel-greeting {"headers":{"amount":500}} -->
```text
Ada
```

<!-- output: spel-greeting -->
```text
Hello Ada, you are gold
```

<!-- fixture: spel-msg -->
```json
{"body": "hello", "headers": {"amount": 250, "name": "Ada"}}
```

<!-- eval: spel-expressions fixture=spel-msg language=spel -->
| expression | result |
|---|---|
| `#{request.body.toUpperCase()}` | `HELLO` |
| `#{request.headers.amount > 100}` | `true` |
| `Hello #{request.headers.name}` | `Hello Ada` |
| `#{request.headers['name']}` | `Ada` |
| `#{headers.missing ?: 'none'}` | `none` |
| `#{exchange.in.body}` | `hello` |
| `#{{1, 2, 3, 4}.?[#this > 2]}` | `[3,4]` |

## Options

| option | default | description |
|---|---|---|
| `resultType` | (none) | Convert the result to this type. |

## Rust DSL

```rust
b.from("direct:spel-rust")
    .set_body(cameleer_script::spel("Hello #{request.body}"));
```

## Common questions

### Do I need `#{...}` around the expression?

No. Without `#{` the whole text is one expression. Use `#{...}` to mix literal text and
expressions.

### Can I call Spring beans?

`@name` resolves a bean from the Camel registry: plain values and bean methods bound with
`bind_bean_method`. There is no Spring application context.

## Related

- [OGNL](ognl.md), [MVEL](mvel.md).
- [Simple language](simple.md).
- [Apache Camel SpEL language](https://camel.apache.org/components/next/languages/spel-language.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.

# MVEL language

> The `mvel` language evaluates MVEL 2 expressions against the exchange, with `request`, `headers`, `body` and `exchange` as the root object's properties.

**Name:** `mvel` · **Crate:** `cameleer-script` · **Cargo feature:** [`script`](../guides/build-features.md) ·
**Apache Camel:** [MVEL](https://camel.apache.org/components/next/languages/mvel-language.html)

## When to use it

- Port Camel routes that use MVEL predicates and expressions.
- Write compact property-navigation expressions (`request.headers.amount * 2`) with Java-like
  method calls.
- For MVEL templates (`@{...}` orb tags), use the [mvel component](../components/mvel.md).

## Syntax

Cameleer interprets MVEL itself, without the MVEL library or a JVM. Supported: property
navigation, method calls, inline lists `['a', 'b']`, maps and arrays, `contains`, `is`, `empty`,
`nil`, projections `(name in list)`, `;`-separated statements, assignments, typed declarations
(`int x = 5`), `with (obj) {...}`, `if`/`else`, `foreach`, `while`, `def` functions and
interceptors (`@Name statement`, registered from Rust). Arbitrary Java classes are not
available.

The root object has these properties:

| property | what it is |
|---|---|
| `request`, `in` | the message: `request.body`, `request.headers.name` |
| `response`, `out` | the message (Camel's out message) |
| `exchange` | the exchange: `exchange.in.body`, `exchange.properties` |
| `body`, `headers` | the body and headers directly |
| `properties` | the exchange properties |
| `context`, `camelContext` | the context |

## Examples

<!-- example: mvel-set-header -->
```yaml
- route:
    id: mvel-demo
    from:
      uri: direct:mvel
      steps:
        - setHeader:
            name: doubled
            mvel: "request.headers.amount * 2"
        - filter:
            mvel: "request.headers.doubled > 100"
            steps:
              - setBody:
                  mvel: "request.body.toUpperCase() + ' ' + request.headers.doubled"
```

<!-- input: mvel-set-header {"headers":{"amount":60}} -->
```text
priority
```

<!-- output: mvel-set-header -->
```text
PRIORITY 120
```

<!-- fixture: mvel-msg -->
```json
{"body": "hello", "headers": {"amount": 250, "name": "Ada"}}
```

<!-- eval: mvel-expressions fixture=mvel-msg language=mvel -->
| expression | result |
|---|---|
| `request.body.toUpperCase()` | `HELLO` |
| `request.headers.amount > 100` | `true` |
| `headers.name + '!'` | `Ada!` |
| `exchange.in.body` | `hello` |
| `body.length()` | `5` |
| `['a', 'b'].size()` | `2` |
| `int x = 5; x * request.headers.amount` | `1250` |

## Options

| option | default | description |
|---|---|---|
| `resultType` | (none) | Convert the result to this type. |

## Rust DSL

```rust
b.from("direct:mvel-rust")
    .set_header("next", cameleer_script::mvel("request.headers.n + 1"));
```

## Common questions

### What is the difference between the mvel language and the mvel component?

The language evaluates one expression inside a route step. The
[mvel component](../components/mvel.md) renders a template file (`mvel:template.mvel`) into the
body.

### Can MVEL call my Java classes?

No. There is no JVM; only the Camel root object, values and common JDK-like methods work.

## Related

- [OGNL](ognl.md) and [SpEL](spel.md): similar Java expression languages.
- [mvel component](../components/mvel.md) for templates.
- [Apache Camel MVEL language](https://camel.apache.org/components/next/languages/mvel-language.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.

# OGNL language

> The `ognl` language evaluates OGNL expressions against the exchange, with `request`, `headers`, `body` and `exchange` as the root object's properties.

**Name:** `ognl` · **Crate:** `cameleer-script` · **Cargo feature:** [`script`](../guides/build-features.md) ·
**Apache Camel:** [OGNL](https://camel.apache.org/components/next/languages/ognl-language.html)

## When to use it

- Port Camel routes that use OGNL predicates and expressions.
- Navigate properties and call methods in one line, with OGNL's projection and selection on
  lists.

## Syntax

Cameleer interprets OGNL itself, without the OGNL library or a JVM. Supported: property and
index navigation, method calls, static members of known JDK classes
(`@java.lang.Math@max(1, 2)`), `#this`, `#root` and `#var`, list literals `{a, b}`, map
literals `#{k: v}`, projection `.{expr}`, selection `.{? expr}`, `in`/`not in`, word operators
(`eq`, `gt`, `and`...), the ternary operator and `,` sequences. Arbitrary Java classes are not
available.

The root object has these properties:

| property | what it is |
|---|---|
| `request` | the message: `request.body`, `request.headers.name` |
| `response` | the message (Camel's out message) |
| `exchange` | the exchange: `exchange.in.body` |
| `body`, `headers` | the body and headers directly |
| `properties`, `property(name)` | the exchange properties |
| `context` | the context |

## Examples

<!-- example: ognl-choice -->
```yaml
- route:
    id: ognl-demo
    from:
      uri: direct:ognl
      steps:
        - choice:
            when:
              - ognl: "request.headers.n > 10"
                steps:
                  - setBody:
                      ognl: "'large: ' + request.headers.n"
            otherwise:
              steps:
                - setBody:
                    ognl: "request.headers.n > 2 ? 'medium' : 'small'"
```

<!-- input: ognl-choice {"headers":{"n":42}} -->
```text
x
```

<!-- output: ognl-choice -->
```text
large: 42
```

<!-- input: ognl-choice#2 {"headers":{"n":3}} -->
```text
x
```

<!-- output: ognl-choice#2 -->
```text
medium
```

<!-- fixture: ognl-msg -->
```json
{"body": "hello", "headers": {"amount": 250, "name": "Ada"}}
```

<!-- eval: ognl-expressions fixture=ognl-msg language=ognl -->
| expression | result |
|---|---|
| `request.body.toUpperCase()` | `HELLO` |
| `request.headers.amount > 100` | `true` |
| `request.headers.name + '!'` | `Ada!` |
| `exchange.in.body` | `hello` |
| `body.length()` | `5` |
| `request.headers.amount > 100 ? 'big' : 'small'` | `big` |
| `@java.lang.Math@max(3, 7)` | `7` |

## Options

| option | default | description |
|---|---|---|
| `resultType` | (none) | Convert the result to this type. |

## Rust DSL

```rust
b.from("direct:ognl-rust")
    .set_header("twice", cameleer_script::ognl("request.body.length() * 2"));
```

## Common questions

### Does OGNL support calling static methods?

Yes for the JDK classes the interpreter knows (`Math`, `Integer`, `String`...), with the
`@class@member` syntax. Other classes are not available.

## Related

- [MVEL](mvel.md) and [SpEL](spel.md).
- [Simple language](simple.md), which covers most of the same needs.
- [Apache Camel OGNL language](https://camel.apache.org/components/next/languages/ognl-language.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.

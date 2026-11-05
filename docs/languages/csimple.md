# CSimple language

> The `csimple` language is an alias of the Simple language in Cameleer: the same syntax and the same results, so routes written for Camel's compiled Simple run unchanged.

**Name:** `csimple` · **Crate:** `cameleer-script` · **Apache Camel:** [CSimple](https://camel.apache.org/components/next/languages/csimple-language.html)

## When to use it

- You are moving routes from Apache Camel that use `csimple:` and want them to run as they are.
- For new routes, use `simple:` directly; it is already parsed once into a syntax tree when the
  route starts, which is what csimple gives you in Camel.

## Syntax

Everything on the [Simple language](simple.md) page applies. In Apache Camel, csimple compiles
the expression to Java source at build time and supports a smaller function set; in Cameleer
it is the full Simple language.

## Examples

<!-- example: csimple-greeting -->
```yaml
- route:
    id: csimple-demo
    from:
      uri: direct:csimple
      steps:
        - filter:
            csimple: "${header.amount} > 100"
            steps:
              - setHeader:
                  name: review
                  constant: "true"
        - setBody:
            csimple: "Hello ${body}, review=${header.review}"
```

<!-- input: csimple-greeting {"headers":{"amount":250}} -->
```text
Ada
```

<!-- output: csimple-greeting -->
```text
Hello Ada, review=true
```

<!-- fixture: csimple-msg -->
```json
{"body": "Hello World", "headers": {"amount": 250}}
```

<!-- eval: csimple-values fixture=csimple-msg language=csimple -->
| expression | result |
|---|---|
| `${body.toUpperCase()}` | `HELLO WORLD` |
| `${header.amount > 100 ? 'big' : 'small'}` | `big` |

<!-- eval: csimple-predicates fixture=csimple-msg language=csimple predicate -->
| expression | result |
|---|---|
| `${header.amount} range '100..300'` | `true` |
| `${body} contains 'Moon'` | `false` |

## Options

None.

## Rust DSL

```rust
b.from("direct:csimple-rust")
    .set_body(language("csimple", "Hello ${body}"));
```

## Common questions

### Is csimple faster than simple in Cameleer?

No, they are the same code. Both parse the expression once when the route is created.

## Related

- [Simple language](simple.md)
- [Apache Camel CSimple language](https://camel.apache.org/components/next/languages/csimple-language.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.

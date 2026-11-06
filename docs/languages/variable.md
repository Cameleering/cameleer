# Variable language

> The `variable` language returns the value of one exchange variable (set with `setVariable`), or null when it is not set.

**Name:** `variable` · **Crate:** `cameleer-core` · **Cargo feature:** none, always included ([build features](../guides/build-features.md)) · **Apache Camel:** [Variable](https://camel.apache.org/components/next/languages/variable-language.html)

## When to use it

- Store intermediate results under a name (Camel 4 variables) without adding headers that
  would be sent to the next endpoint.
- Keep the pieces of a multi-step agent flow apart: the user question, the retrieved
  context, the model answer.

## Syntax

The expression text is the variable name. In [Simple](simple.md) the same value is
`${variable.name}`, and `${variables}` returns all of them. Variables are stored per exchange.

## Examples

<!-- example: variable-build-prompt -->
```yaml
- route:
    id: variable-demo
    from:
      uri: direct:variable
      steps:
        - setVariable:
            name: question
            simple: "${body}"
        - setVariable:
            name: context
            constant: "Orders ship within 2 days."
        - setBody:
            simple: "Context: ${variable.context}\nQuestion: ${variable.question}"
        - setHeader:
            name: asked
            variable: question
```

<!-- input: variable-build-prompt -->
```text
When will my order ship?
```

<!-- output: variable-build-prompt -->
```text
Context: Orders ship within 2 days.
Question: When will my order ship?
```

<!-- fixture: variable-msg -->
```json
{"body": "x", "variables": {"question": "When?", "score": 0.82}}
```

<!-- eval: variable-values fixture=variable-msg language=variable -->
| expression | result |
|---|---|
| `question` | `When?` |
| `score` | `0.82` |
| `missing` | (empty) |

## Options

The variable language has no options. It reads exchange variables only: Apache Camel's
`global:` and `route:` prefixes are not interpreted by this language.

## Rust DSL

```rust
b.from("direct:variable-rust")
    .set_body(language("variable", "question"));
```

## Common questions

### How do I set a variable in YAML?

Use the `setVariable` step with `name` and an expression, for example
`setVariable: {name: question, simple: "${body}"}`.

### Are variables sent to endpoints?

No. Unlike headers, variables stay inside the exchange.

## Related

- [Simple language](simple.md) (`${variable.name}`, `${variables}`)
- [Header language](header.md), [exchangeProperty language](exchangeProperty.md)
- [Apache Camel Variable language](https://camel.apache.org/components/next/languages/variable-language.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.

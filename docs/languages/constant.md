# Constant language

> The `constant` language returns its text unchanged, optionally converted to a type with `resultType`.

**Name:** `constant` · **Crate:** `cameleer-core` · **Apache Camel:** [Constant](https://camel.apache.org/components/next/languages/constant-language.html)

## When to use it

- Set a header, property or body to a fixed value: a content type, a status, a model name.
- Pass a literal where an expression is required, without Simple parsing `${...}` in it.
- Set a typed value (a number or a boolean) with `resultType`.

## Syntax

The expression text is the value. Nothing in it is evaluated, so `${body}` stays the literal
text `${body}`.

In YAML, a plain number or boolean in an expression position is also a constant:
`setHeader: {name: retries, expression: 3}` gives the integer `3`.

## Example

Set a header and the body to fixed values.

<!-- example: constant-set-header-and-body -->
```yaml
- route:
    id: constant-demo
    from:
      uri: direct:constant
      steps:
        - setHeader:
            name: Content-Type
            constant: application/json
        - setHeader:
            name: retries
            constant:
              expression: "3"
              resultType: int
        - setBody:
            simple: '{"type":"${header.Content-Type}","retries":${header.retries},"retriesType":"${header.retries.getClass()}"}'
```

<!-- input: constant-set-header-and-body -->
```text
anything
```

<!-- output: constant-set-header-and-body -->
```json
{"type":"application/json","retries":3,"retriesType":"java.lang.Integer"}
```

The text is not evaluated:

<!-- fixture: constant-plain -->
```json
{"body": "Hello"}
```

<!-- eval: constant-values fixture=constant-plain language=constant -->
| expression | result |
|---|---|
| `application/json` | `application/json` |
| `${body}` | `${body}` |
| `42` | `42` |

## Options

| option | default | description |
|---|---|---|
| `resultType` | (none) | Convert the text to this type: `int`, `Integer`, `long`, `double`, `boolean`, `String`... |

## Rust DSL

```rust
b.from("direct:constant-rust")
    .set_header("Content-Type", constant("application/json"))
    .set_body(constant("fixed body"));
```

## Common questions

### How do I set a header to a fixed value?

Use `setHeader` with `constant: value`. A plain string under `expression:` is parsed as Simple,
so prefer `constant:` when the value contains `${`.

### How do I set a number instead of a string?

Add `resultType: int` (or `long`, `double`, `boolean`), or write the number unquoted as the
expression in YAML.

## Related

- [Simple language](simple.md): templates with `${...}` functions.
- [Header language](header.md), [exchangeProperty language](exchangeProperty.md),
  [variable language](variable.md).
- [Apache Camel Constant language](https://camel.apache.org/components/next/languages/constant-language.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.

# Groovy language

> The `groovy` language evaluates Groovy expressions and scripts against the message, with `body`, `headers`, `request` and `exchange` available as variables.

**Name:** `groovy` · **Crate:** `cameleer-script` · **Cargo feature:** [`script`](../guides/build-features.md) ·
**Apache Camel:** [Groovy](https://camel.apache.org/components/next/languages/groovy-language.html)

## When to use it

- Port existing Camel routes that use Groovy predicates and transformations without a JVM.
- Transform lists and maps with GDK methods (`collect`, `findAll`, `sum`, `groupBy`) and
  GPath (`items*.price`).
- Write a short `script` step that changes several headers and the body.

## Syntax

Cameleer runs Groovy in its own interpreter, written in Rust. There is no JVM and no Groovy
compiler. It covers Groovy as used in route scripts: `def` and typed variables, closures (`it`),
GStrings, `?.`, `?:`, the spread operator `*.`, GPath, list and map literals, ranges, regex
operators (`=~`, `==~`), `switch` expressions, classes, traits, enums, common AST transforms
(`@ToString`, `@Canonical`, `@Immutable`...), metaclass additions, `JsonSlurper` and
`JsonOutput`, and the GDK collection methods. Not supported: `@Grab` (no dependencies can be
loaded), static type checking (`@CompileStatic` is accepted and ignored), arbitrary Java
libraries.

Variables available to the script:

| variable | what it is |
|---|---|
| `body` | the message body |
| `headers` | the message headers (`headers.name`, `headers['name']`) |
| `request`, `message` | the message: `request.body`, `request.headers.name = 'x'` |
| `exchange` | the exchange: `exchange.in.body`, `exchange.properties.x`, `exchange.getIn().setHeader(...)` |
| `properties`, `exchangeProperties` | the exchange properties |
| `variables` | the exchange variables |
| `camelContext` | the context |
| `log` | a logger |

The value of the last statement is the result. Used as an expression the script works on a copy
of the message; in a `script` step its changes are kept and the result is ignored.

## Examples

### Total an order with GPath

<!-- example: groovy-order-total -->
```yaml
- route:
    id: groovy-total
    from:
      uri: direct:groovy-total
      steps:
        - unmarshal:
            json: {}
        - setHeader:
            name: total
            groovy: "body.items.collect { it.qty * it.price }.sum()"
        - setBody:
            groovy: "\"${body.items*.sku.join(',')} = ${headers.total}\""
```

<!-- input: groovy-order-total -->
```json
{"items":[{"sku":"A1","qty":2,"price":5},{"sku":"B2","qty":1,"price":12}]}
```

<!-- output: groovy-order-total -->
```text
A1,B2 = 22
```

### Filter with a Groovy predicate

<!-- example: groovy-filter -->
```yaml
- route:
    id: groovy-filter
    from:
      uri: direct:groovy-filter
      steps:
        - setHeader:
            name: verdict
            constant: dropped
        - filter:
            groovy: "request.body ==~ /(?i).*refund.*/"
            steps:
              - setHeader:
                  name: verdict
                  constant: kept
        - setBody:
            simple: "${header.verdict}"
```

<!-- input: groovy-filter -->
```text
Please process my Refund
```

<!-- output: groovy-filter -->
```text
kept
```

<!-- input: groovy-filter#2 -->
```text
Where is my parcel?
```

<!-- output: groovy-filter#2 -->
```text
dropped
```

### A script step

<!-- example: groovy-script-step -->
```yaml
- route:
    id: groovy-script
    from:
      uri: direct:groovy-script
      steps:
        - script:
            groovy: |
              def items = request.body.split(',')
              request.headers.count = items.size()
              request.body = items.collect { it.trim().toUpperCase() }.join('|')
        - setBody:
            simple: "${body} (${header.count})"
```

<!-- input: groovy-script-step -->
```text
kafka, mqtt , amqp
```

<!-- output: groovy-script-step -->
```text
KAFKA|MQTT|AMQP (3)
```

### Expressions

<!-- fixture: groovy-msg -->
```json
{"body": "hello", "headers": {"amount": 250, "name": "Ada"}}
```

<!-- eval: groovy-expressions fixture=groovy-msg language=groovy -->
| expression | result |
|---|---|
| `body.toUpperCase()` | `HELLO` |
| `body.reverse()` | `olleh` |
| `request.headers.amount > 100` | `true` |
| `"Hi ${headers.name}"` | `Hi Ada` |
| `[1, 2, 3].collect { it * 2 }.sum()` | `12` |
| `headers.missing ?: 'none'` | `none` |
| `body?.size()` | `5` |
| `(1..4).findAll { it % 2 == 0 }` | `[2,4]` |

## Options

| option | default | description |
|---|---|---|
| `resultType` | (none) | Convert the result to this type (`int`, `String`, `boolean`...). |

## Rust DSL

```rust
b.from("direct:groovy-rust")
    .filter(cameleer_script::groovy("request.headers.amount > 100"))
    .set_body(cameleer_script::groovy("'big order: ' + headers.amount"));
```

## Common questions

### Can I run my existing Camel Groovy scripts?

Usually yes, if they only use the Camel bindings, Groovy syntax and the GDK. Scripts that load
Java libraries, use `@Grab` or call Spring beans by class need changes.

### How do I parse JSON in Groovy?

Either unmarshal first (`unmarshal: json`) and navigate `body.field`, or call
`new groovy.json.JsonSlurper().parseText(body)`.

### Why does my header change disappear?

Changes made in `setBody`/`setHeader`/`filter` expressions are made on a copy. Use a `script`
step to modify the message.

## Related

- [JavaScript](js.md), [Python](python.md), [Java](java.md) script languages.
- [Simple language](simple.md).
- [Apache Camel Groovy language](https://camel.apache.org/components/next/languages/groovy-language.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.

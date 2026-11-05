# Java language

> The `java` language (also `joor`) evaluates Java snippets against the message, with `exchange`, `message` and `body` available as variables.

**Name:** `java` · **Also:** `joor` · **Crate:** `cameleer-script` ·
**Apache Camel:** [Java](https://camel.apache.org/components/next/languages/java-language.html),
[jOOR](https://camel.apache.org/components/next/languages/joor-language.html)

## When to use it

- Reuse Java snippets from existing Camel `java`/`joor` expressions.
- Write a transformation with streams, `String.format` or `java.time` when the team thinks in
  Java.

## Syntax

Apache Camel compiles these snippets with the JDK at startup. Cameleer interprets them instead:
there is no JDK, no compiler and no classpath. The snippet is Java syntax evaluated with Java
semantics (integer division, `ArithmeticException`, Java `toString`): typed locals and `var`,
casts, `if`/`for`/`while`/`switch` (also switch expressions and type patterns), lambdas, method
references, text blocks, classes, records and enums declared in the snippet, `try`/`catch`, and
the common JDK API (`String`, `Math`, `List`/`Map`/`Set`, `Arrays`, `Collections`, `Optional`,
streams and `Collectors`, `java.time`). Not available: other JDK or third-party classes,
reflection, threads, and compile-time checks (generics are not checked).

A single expression is its own result; a snippet with statements ends with `return`.

Variables and helpers:

| name | what it is |
|---|---|
| `exchange` | the exchange (`exchange.getIn()`, `exchange.getMessage()`, `exchange.setProperty(..)`) |
| `message` | the message (`message.getHeader("x")`, `message.getHeader("x", String.class)`, `message.setBody(..)`) |
| `body` | the message body (cast it: `((String) body)`) |
| `optionalBody` | the body as an `Optional` |
| `bodyAs(Type.class)` | the body converted to a type |
| `header(name)`, `headerAs(name, Type.class)` | a header |
| `exchangeProperty(name)`, `variable(name)` | a property, a variable |

## Examples

### Format a summary with streams

<!-- example: java-stream-summary -->
```yaml
- route:
    id: java-summary
    from:
      uri: direct:java-summary
      steps:
        - setBody:
            java: |
              var parts = ((String) body).split(",");
              int total = Arrays.stream(parts).mapToInt(p -> Integer.parseInt(p.trim())).sum();
              return String.format("%d readings, total %d", parts.length, total);
```

<!-- input: java-stream-summary -->
```text
4, 8, 15, 16
```

<!-- output: java-stream-summary -->
```text
4 readings, total 43
```

### A script step that sets a header

<!-- example: java-script-step -->
```yaml
- route:
    id: java-script
    from:
      uri: direct:java-script
      steps:
        - script:
            joor: |
              exchange.getMessage().setHeader("len", body.toString().length());
              message.setBody(((String) body).toUpperCase());
        - setBody:
            simple: "${body}/${header.len}"
```

<!-- input: java-script-step -->
```text
camel
```

<!-- output: java-script-step -->
```text
CAMEL/5
```

### Expressions

<!-- fixture: java-msg -->
```json
{"body": "hello", "headers": {"amount": 250, "name": "Ada"}}
```

<!-- eval: java-expressions fixture=java-msg language=java -->
| expression | result |
|---|---|
| `((String) body).toUpperCase()` | `HELLO` |
| `message.getHeader("name")` | `Ada` |
| `"Hi " + message.getHeader("name", String.class)` | `Hi Ada` |
| `var n = (Integer) message.getHeader("amount"); return n > 100;` | `true` |
| `exchange.getIn().getBody(String.class).substring(1)` | `ello` |
| `7 / 2` | `3` |

## Options

| option | default | description |
|---|---|---|
| `resultType` | (none) | Convert the result to this type. |

## Rust DSL

```rust
b.from("direct:java-rust")
    .set_body(cameleer_script::joor("((String) body).length() * 2"));
```

## Common questions

### Is my Java code compiled?

No. It is parsed and interpreted at route start, so a syntax error fails the route at startup,
but type errors (wrong generics, missing returns) are not reported.

### What is the difference between `java` and `joor`?

None in Cameleer: both names select the same interpreter. Camel 4 renamed `joor` to `java`.

## Related

- [Groovy](groovy.md), [JavaScript](js.md) script languages.
- [Simple language](simple.md).
- [Apache Camel Java language](https://camel.apache.org/components/next/languages/java-language.html)

# JavaScript language

> The `js` language evaluates JavaScript expressions and scripts against the message, with `body`, `headers` and `exchange` available as variables.

**Name:** `js` · **Also:** `javascript`, `javaScript` · **Crate:** `cameleer-script` ·
**Apache Camel:** [JavaScript](https://camel.apache.org/components/next/languages/js-language.html)

## When to use it

- Reshape a JSON answer from an LLM or an API with a few lines of code (`JSON.parse`, `map`,
  `filter`, `JSON.stringify`) where Simple or JSONPath gets awkward.
- Route on a computed condition in `filter` or `choice`.
- Run a small script step that sets several headers at once.

## Syntax

Cameleer runs JavaScript in its own interpreter, written in Rust. It is not GraalJS or V8 and
does not need a JVM. It covers ES5.1 plus most of ES2015-2023 as used in route scripts: `let` and
`const`, arrow functions, template literals, destructuring, spread, classes, optional chaining
`?.`, `??`, `Map` and `Set`, generators, `Promise` and `async`/`await` (drained before the
result is returned), `JSON`, `Math`, `Intl` for a few locales. There is no Node.js API
(`require('fs')`, `process`, network access), and no access to Java classes beyond a few safe
JDK types through `Java.type`.

Variables available to the script:

| variable | what it is |
|---|---|
| `body` | the message body (a string body stays a string: use `JSON.parse(body)` for JSON text) |
| `headers` | the message headers: `headers.get('name')` or `headers.name` |
| `request`, `message` | the message: `request.body`, `request.headers.name`, `request.setHeader(k, v)` |
| `exchange` | the exchange with Java-like accessors: `exchange.getIn().getHeader('x')`, `exchange.setProperty(k, v)` |
| `properties`, `exchangeProperties` | the exchange properties |
| `variables` | the exchange variables |
| `camelContext` | the context (`camelContext.getName()`) |
| `log` | a logger (`log.info('...')`) |

The value of the last expression statement is the result. Used as an expression (in `setBody`,
`setHeader`, `filter`...) the script works on a copy of the message and only its result counts.
In a `script` step the result is ignored and its changes to body, headers, properties and
variables are kept.

## Examples

### Reshape an LLM's JSON answer

The model answered with JSON text. Keep only confident labels and build a compact routing
decision.

<!-- example: js-reshape-llm-answer -->
```yaml
- route:
    id: js-reshape
    from:
      uri: direct:js-reshape
      steps:
        - setBody:
            js: |
              const answer = JSON.parse(body);
              const labels = answer.labels.filter(l => l.score >= 0.5).map(l => l.name);
              JSON.stringify({ ticket: answer.id, labels, urgent: labels.includes('outage') });
```

<!-- input: js-reshape-llm-answer -->
```json
{"id":"T-17","labels":[{"name":"billing","score":0.2},{"name":"outage","score":0.93},{"name":"eu","score":0.71}]}
```

<!-- output: js-reshape-llm-answer -->
```json
{"ticket":"T-17","labels":["outage","eu"],"urgent":true}
```

### Route with a JavaScript predicate

<!-- example: js-choice -->
```yaml
- route:
    id: js-choice
    from:
      uri: direct:js-choice
      steps:
        - choice:
            when:
              - js: "headers.get('amount') > 1000"
                steps:
                  - setBody:
                      constant: needs approval
            otherwise:
              steps:
                - setBody:
                    js: "'auto-approved: ' + headers.get('amount')"
```

<!-- input: js-choice {"headers":{"amount":5000}} -->
```text
order
```

<!-- output: js-choice -->
```text
needs approval
```

<!-- input: js-choice#2 {"headers":{"amount":40}} -->
```text
order
```

<!-- output: js-choice#2 -->
```text
auto-approved: 40
```

### A script step that changes the message

<!-- example: js-script-step -->
```yaml
- route:
    id: js-script
    from:
      uri: direct:js-script
      steps:
        - script:
            js: |
              const words = body.trim().split(/\s+/);
              request.setHeader('wordCount', words.length);
              request.body = words.map(w => w[0].toUpperCase() + w.slice(1)).join(' ');
        - setBody:
            simple: "${body} (${header.wordCount} words)"
```

<!-- input: js-script-step -->
```text
send messages to kafka
```

<!-- output: js-script-step -->
```text
Send Messages To Kafka (4 words)
```

### Expressions

Evaluated against this message:

<!-- fixture: js-msg -->
```json
{"body": "hello", "headers": {"amount": 250, "name": "Ada"}, "properties": {"region": "eu"}, "variables": {"user": "ada"}}
```

<!-- eval: js-expressions fixture=js-msg language=js -->
| expression | result |
|---|---|
| `body.toUpperCase()` | `HELLO` |
| `headers.get('amount') > 100` | `true` |
| `request.headers.name + '!'` | `Ada!` |
| `exchange.getIn().getHeader('name')` | `Ada` |
| `` `Hi ${headers.get('name')}` `` | `Hi Ada` |
| `[1, 2, 3].map(x => x * 2).join(',')` | `2,4,6` |
| `properties.region` | `eu` |
| `variables.user` | `ada` |
| `typeof body` | `string` |

## Options

| option | default | description |
|---|---|---|
| `resultType` | (none) | Convert the result to this type (`int`, `String`, `boolean`...). |
| `scriptDir` | (none) | Directory that relative ES module imports resolve against. |

## Rust DSL

```rust
b.from("direct:js-rust")
    .set_header("len", cameleer_script::js("body.length"))
    .set_body(cameleer_script::js("body.toUpperCase()"));
```

## Common questions

### How do I parse a JSON body in JavaScript?

A JSON text body is a string, so call `JSON.parse(body)`. If an earlier step already
unmarshalled the body (for example `unmarshal: json`), `body` is an object and `body.field` works
directly.

### How do I change headers from a script?

Use a `script` step and call `request.setHeader('name', value)` or
`exchange.getIn().setHeader(...)`. In `setBody` or `setHeader` the script runs on a copy, so
other changes are discarded.

### Can I use npm packages or Node.js modules?

No. The interpreter has no Node.js runtime. Local ES modules can be imported relative to
`scriptDir`.

## Related

- [Groovy](groovy.md), [Python](python.md) and [Java](java.md) script languages.
- [Simple language](simple.md) for one-line templates, [JSONPath](jsonpath.md) and [jq](jq.md)
  for picking fields from JSON.
- [JSON data format](../dataformats/json.md).
- [Apache Camel JavaScript language](https://camel.apache.org/components/next/languages/js-language.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.

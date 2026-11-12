# DataSonnet language

> The `datasonnet` language maps the message body to a new document with Jsonnet: the body is `payload`, the result is JSON text by default.

**Name:** `datasonnet` · **Crate:** `cameleer-script` · **Cargo feature:** [`script`](../guides/build-features.md) ·
**Apache Camel:** [DataSonnet](https://camel.apache.org/components/next/languages/datasonnet-language.html)

## When to use it

- Map an incoming JSON request into the payload a model API or another service expects, as a
  declarative template.
- Convert between JSON, XML, CSV and form data with `bodyMediaType` and `outputMediaType`.
- Aggregate and reshape arrays (`std.foldl`, comprehensions, the `ds` library).

## Syntax

The expression is a Jsonnet program. Cameleer runs its own Jsonnet interpreter (no JVM, no
sjsonnet). Available:

| name | what it is |
|---|---|
| `payload` | the body, read according to `bodyMediaType` (JSON is parsed) |
| `cml.header(name)` | a header |
| `cml.exchangeProperty(name)` | an exchange property |
| `cml.variable(name)` | an exchange variable |
| `cml.properties(key)` | a property placeholder value |
| `std.*` | the common Jsonnet standard library functions |
| `ds.*` | the DataSonnet library: `ds.map`, `ds.filter`, `ds.upper`, `ds.datetime`, `ds.crypto`, `ds.strings`... |

The language covers Jsonnet objects (with `self`, `super`, hidden `::` fields, `+:` merges),
arrays, `local`, functions, `if`/`then`/`else`, comprehensions, `%` formatting, `error`,
`assert` and local `import`. The result is written according to `outputMediaType`: with the
default `application/json` it is JSON text with the object keys sorted (as sjsonnet does), so a
string result comes back quoted (`"x"`). Use `outputMediaType: text/plain` for plain text.

## Examples

### Map a request into a model API payload

<!-- example: datasonnet-model-payload -->
```yaml
- route:
    id: datasonnet-payload
    from:
      uri: direct:datasonnet-payload
      steps:
        - setBody:
            datasonnet: |
              {
                model: cml.header('model'),
                messages: [
                  { role: 'system', content: 'You answer questions about orders.' },
                  { role: 'user', content: payload.question },
                ],
                max_tokens: if payload.short then 100 else 500,
              }
```

<!-- input: datasonnet-model-payload {"headers":{"model":"gpt-4o-mini"}} -->
```json
{"question":"Where is order 42?","short":true}
```

<!-- output: datasonnet-model-payload -->
```json
{"max_tokens":100,"messages":[{"content":"You answer questions about orders.","role":"system"},{"content":"Where is order 42?","role":"user"}],"model":"gpt-4o-mini"}
```

### Aggregate an array to plain text

<!-- example: datasonnet-total -->
```yaml
- route:
    id: datasonnet-total
    from:
      uri: direct:datasonnet-total
      steps:
        - setBody:
            datasonnet:
              expression: "'total=' + std.toString(std.foldl(function(t, i) t + i.qty, payload.items, 0))"
              outputMediaType: text/plain
```

<!-- input: datasonnet-total -->
```json
{"items":[{"qty":2},{"qty":5},{"qty":1}]}
```

<!-- output: datasonnet-total -->
```text
total=8
```

### Expressions

<!-- fixture: datasonnet-msg -->
```json
{"bodyText": "{\"user\":{\"name\":\"Ada\"},\"values\":[1,2,3]}", "headers": {"h": "v"}, "properties": {"p": "q"}}
```

<!-- eval: datasonnet-expressions fixture=datasonnet-msg language=datasonnet -->
| expression | result |
|---|---|
| `{name: payload.user.name}` | `{"name":"Ada"}` |
| `std.foldl(function(t, i) t + i, payload.values, 0)` | `6` |
| `[v * 2 for v in payload.values]` | `[2,4,6]` |
| `ds.upper(payload.user.name)` | `"ADA"` |
| `cml.header('h')` | `"v"` |
| `cml.exchangeProperty('p')` | `"q"` |
| `std.length(payload.values)` | `3` |

## Options

| option | default | description |
|---|---|---|
| `bodyMediaType` | `application/json` | How to read the body into `payload`: `application/json`, `text/plain`, `application/xml`, `application/csv`, `application/x-www-form-urlencoded`, `application/x-java-object`. Parameters such as `application/csv; separator=;` are supported. |
| `outputMediaType` | `application/json` | How to write the result: the same media types. |
| `resultType` | (none) | Convert the result to this type instead of writing it with `outputMediaType`. |

## Rust DSL

```rust
b.from("direct:datasonnet-rust")
    .set_body(cameleer_script::datasonnet("{total: std.foldl(function(t, i) t + i, payload.values, 0)}"));
```

## Common questions

### Why is my string result wrapped in quotes?

The default output media type is `application/json`, and a JSON string is quoted. Set
`outputMediaType: text/plain`.

### Why are the keys of my output sorted?

Jsonnet objects have no field order; the output is written with sorted keys, like sjsonnet used
by Apache Camel.

### How do I read a header?

`cml.header('name')`. Properties and variables use `cml.exchangeProperty` and `cml.variable`.

## Related

- [JSONPath](jsonpath.md) and [jq](jq.md) for picking fields.
- [JSON data format](../dataformats/json.md).
- [Apache Camel DataSonnet language](https://camel.apache.org/components/next/languages/datasonnet-language.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.

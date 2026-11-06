# JSON data format

> The `json` data format turns JSON text into a structured message body (unmarshal) and any body back into JSON text (marshal).

**Name:** `json` · **Also:** `jackson`, `gson`, `jsonb`, `fastjson`, `johnzon` · **Crate:** `cameleer-core` · **Cargo feature:** none, always included ([build features](../guides/build-features.md)) ·
**Apache Camel:** [Jackson](https://camel.apache.org/components/next/dataformats/jackson-dataformat.html)

## When to use it

- Parse the JSON answer of an LLM or a REST API so later steps can read fields with
  `${body.intent}`, [JSONPath](../languages/jsonpath.md) or [jq](../languages/jq.md).
- Turn a map or list built in the route (by `setBody`, an aggregation or a script) into JSON
  text before you send it over HTTP, Kafka or a file.
- Pretty print JSON for logs and human review.

## Marshal and unmarshal

| direction | body in | body out |
|---|---|---|
| `unmarshal` | JSON text (string or bytes) | an object, array, string, number, boolean or null. Blank text gives null. A body that is already structured is left alone. |
| `marshal` | any value | JSON text (a string). Sets `Content-Type: application/json` unless `contentTypeHeader: false`. |

All the alias names (`jackson`, `gson`, `jsonb`, `fastjson`, `johnzon`) are the same data format,
so routes written for any of those Camel libraries run unchanged.

**Difference from Apache Camel:** there is no POJO binding. `unmarshalType` and similar class
options have no effect: JSON always maps to Cameleer's generic `Value` (objects, arrays and
scalars), which Simple, JSONPath, jq and the script languages navigate directly.

## Example

Parse a model's JSON answer and route on one of its fields. The answer is unmarshalled once;
after that the fields are plain map lookups.

<!-- example: json-parse-llm-answer -->
```yaml
- route:
    id: json-llm-answer
    from:
      uri: direct:llm-answer
      steps:
        - unmarshal:
            json: {}
        - setHeader:
            name: intent
            simple: "${body.intent}"
        - setBody:
            simple: "intent=${body.intent} order=${body.order.id} confidence=${body.confidence}"
```

<!-- input: json-parse-llm-answer -->
```json
{"intent":"refund","confidence":0.92,"order":{"id":"A-17","total":49.5}}
```

<!-- output: json-parse-llm-answer -->
```text
intent=refund order=A-17 confidence=0.92
```

Build a structure and write it out as pretty-printed JSON:

<!-- example: json-marshal-pretty -->
```yaml
- route:
    id: json-pretty
    from:
      uri: direct:json-pretty
      steps:
        - unmarshal:
            jackson: {}
        - marshal:
            json:
              prettyPrint: true
```

<!-- input: json-marshal-pretty -->
```json
{"name":"Ada","skills":["math","engines"]}
```

<!-- output: json-marshal-pretty -->
```json
{
  "name": "Ada",
  "skills": [
    "math",
    "engines"
  ]
}
```

## Rust DSL

```rust
b.from("direct:json-rust")
    .unmarshal(DataFormatDefinition::json())
    .set_body(simple("${body.intent}"));
```

## Options

| option | default | description |
|---|---|---|
| `prettyPrint` | `false` | Indent the JSON written by `marshal`. |
| `contentTypeHeader` | `true` | Set `Content-Type: application/json` on `marshal`. |

## Common questions

### How do I parse a JSON string into an object?

Use `unmarshal: {json: {}}`. After it, read fields with `${body.field}`, `${body.items[0].sku}`
or a [JSONPath](../languages/jsonpath.md) expression.

### Do I need to unmarshal before using JSONPath or jq?

No. The JSONPath and jq languages parse a JSON text body themselves. Unmarshal when several steps
read the body, so it is parsed only once.

### How do I convert a map to JSON text?

Use `marshal: {json: {}}`. Any body (map, list, string, number) becomes JSON text.

### Can I bind JSON to a typed class?

No, Cameleer has no Java classes. JSON maps to a generic value. Validate its shape with the
[json-validator](../components/json-validator.md) component if you need a contract.

## Related

- [JSONPath language](../languages/jsonpath.md), [jq language](../languages/jq.md),
  [Simple language](../languages/simple.md)
- [json-validator component](../components/json-validator.md): check JSON (for example an LLM's
  output) against a JSON Schema.
- [YAML data format](yaml.md), [CBOR data format](cbor.md), [XML data format](jacksonxml.md)
- [Apache Camel Jackson data format](https://camel.apache.org/components/next/dataformats/jackson-dataformat.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.

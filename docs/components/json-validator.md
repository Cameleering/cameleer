# JSON Schema validator component

> The `json-validator` component checks the message body (or a header) against a JSON Schema and fails the exchange with a `JsonValidationException` listing every error when it does not conform.

**Scheme:** `json-validator:` · **Crate:** `cameleer-core` · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [JSON Schema Validator](https://camel.apache.org/components/next/json-validator-component.html)

## When to use it

- Check an LLM's JSON answer against a schema before acting on it: a tool call, a
  classification, an extraction. Ask the model again or route to a human when it fails.
- Validate incoming REST payloads before they reach a database or a queue.
- Enforce a contract between services that exchange JSON events.

## URI format

```text
json-validator:schemaResource[?options]
```

`schemaResource` is a file path relative to the working directory, `file:path`,
`classpath:path` (read as a relative path) or `ref:beanName` (a `String` in the registry).

## Example

A model was asked to triage a ticket and answer with JSON. The route checks the answer before
any action is taken and reports the schema errors otherwise (for a retry prompt, for example).

<!-- file: schemas/triage-answer.json -->
```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "type": "object",
  "required": ["priority", "team", "summary"],
  "additionalProperties": false,
  "properties": {
    "priority": {"enum": ["P1", "P2", "P3"]},
    "team": {"type": "string", "minLength": 2},
    "summary": {"type": "string", "maxLength": 120},
    "confidence": {"type": "number", "minimum": 0, "maximum": 1}
  }
}
```

<!-- example: json-validator-llm-answer -->
```yaml
- route:
    id: json-validator-llm-answer
    from:
      uri: direct:check-llm-answer
      steps:
        - doTry:
            steps:
              - to: json-validator:schemas/triage-answer.json
              - setBody:
                  simple: "accepted: ${body}"
            doCatch:
              - exception:
                  - org.apache.camel.ValidationException
                steps:
                  - setBody:
                      simple: "rejected: ${exception.message}"
```

A well-formed answer passes unchanged:

<!-- input: json-validator-llm-answer -->
```json
{"priority":"P1","team":"payments","summary":"Checkout fails for EU cards","confidence":0.92}
```

<!-- output: json-validator-llm-answer -->
```text
accepted: {"priority":"P1","team":"payments","summary":"Checkout fails for EU cards","confidence":0.92}
```

An answer with an invented priority, a missing field and an extra key is rejected with every
error:

<!-- input: json-validator-llm-answer#2 -->
```json
{"priority":"urgent","team":"payments","reason":"card declined"}
```

<!-- output: json-validator-llm-answer#2 {"contains":true} -->
```text
rejected: JSON validation error with 3 errors:
$.priority
$.summary
$.reason
```

## Rust DSL

```rust
b.from("direct:json-validator-rust")
    .to("json-validator:schemas/triage-answer.json")
    .set_body(constant("ok"));
```

## Options

| option | default | description |
|---|---|---|
| `failOnNullBody` | `true` | Fail with `NoJsonBodyValidationException` when the body is null. |
| `failOnNullHeader` | `true` | Fail with `NoJsonHeaderValidationException` when `headerName` is set and the header is missing. |
| `headerName` | (none) | Validate this header instead of the body. |
| `contentCache` | `false` | Load the schema once instead of for every message. |

Accepted and ignored (Java-only settings): `allowContextMapAll`, `allowTemplateFromHeader`,
`errorHandler`, `schemaLoader`, `uriSchemaLoader`, `objectMapper`,
`disabledDeserializationFeatures`, `enabledDeserializationFeatures`, `lazyStartProducer`.

A string or bytes body is parsed as JSON (invalid JSON is itself a validation error); an object
body, for example after `unmarshal: json`, is validated directly.

### Supported JSON Schema

Drafts 04 to 07 and a 2019-09 subset: boolean schemas, `type` (also lists), `enum`, `const`,
`minimum` / `maximum`, `exclusiveMinimum` / `exclusiveMaximum`, `multipleOf`,
`minLength` / `maxLength`, `pattern`, `format` (`date-time`, `date`, `time`, `email`, `ipv4`,
`uuid`, `hostname`, `uri`; other formats are ignored), `items` / `additionalItems`,
`minItems` / `maxItems`, `uniqueItems`, `contains`, `properties`, `required`,
`additionalProperties`, `patternProperties`, `propertyNames`, `minProperties` /
`maxProperties`, `dependencies` / `dependentRequired`, `allOf` / `anyOf` / `oneOf` / `not`,
`if` / `then` / `else`, and `$ref` to local pointers (`#/definitions/x`, `#/$defs/x`).
**Remote `$ref`s are not supported.**

## Message headers

| header / property | direction | description |
|---|---|---|
| `CamelJsonValidationErrors` (exchange property) | out | The list of error messages, set when validation fails. |

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/check-answer.camel.yaml
```

## Common questions

### How do I check that an LLM returned valid JSON in the shape I asked for?

Send the model's answer to `json-validator:schema.json`. Invalid JSON and schema violations
both fail with `JsonValidationException`, which you can catch to retry or escalate.

### How do I read the individual errors?

The exception message lists them, one per line (`$.field: ...`), and the
`CamelJsonValidationErrors` exchange property holds them as a list, handy for building a
correction prompt.

### Can my schema reference another file?

No, only local `$ref`s (`#/definitions/...`, `#/$defs/...`) are resolved. Inline the shared
definitions in one schema file.

## Related

- [validator](validator.md) for XML Schema.
- [JSONPath language](../languages/jsonpath.md) and [jq language](../languages/jq.md) for
  reading fields after validation.
- [Apache Camel JSON Schema Validator component](https://camel.apache.org/components/next/json-validator-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.

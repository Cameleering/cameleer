# URL-encoded form data format

> The `urlencoded` data format parses `application/x-www-form-urlencoded` text into a map (unmarshal) and writes a map as form text (marshal).

**Name:** `urlencoded` · **Crate:** `cameleer-core` · **Cargo feature:** none, always included ([build features](../guides/build-features.md)) · **Apache Camel:** no direct equivalent (Camel
handles forms in the HTTP components)

## When to use it

- Read HTML form posts and webhook callbacks that arrive as `a=1&b=2` (Slack slash commands,
  Twilio, payment providers).
- Build the body of an OAuth token request or any form POST.

## Marshal and unmarshal

| direction | body in | body out |
|---|---|---|
| `unmarshal` | form text | a map. `+` and `%XX` are decoded; a repeated key becomes a list. |
| `marshal` | a map (null gives an empty string) | form text. List values repeat the key. |

## Example

Parse a form post and turn it into JSON:

<!-- example: urlencoded-form-to-json -->
```yaml
- route:
    id: form-to-json
    from:
      uri: direct:form-to-json
      steps:
        - unmarshal:
            urlencoded: {}
        - marshal:
            json: {}
```

<!-- input: urlencoded-form-to-json -->
```text
name=Ada+Lovelace&topic=math&topic=engines&note=50%25+off
```

<!-- output: urlencoded-form-to-json -->
```json
{"name":"Ada Lovelace","topic":["math","engines"],"note":"50% off"}
```

Build a form body from JSON, for example for an OAuth client-credentials request:

<!-- example: urlencoded-json-to-form -->
```yaml
- route:
    id: json-to-form
    from:
      uri: direct:json-to-form
      steps:
        - unmarshal:
            json: {}
        - marshal:
            urlencoded: {}
```

<!-- input: urlencoded-json-to-form -->
```json
{"grant_type":"client_credentials","scope":"read write"}
```

<!-- output: urlencoded-json-to-form -->
```text
grant_type=client_credentials&scope=read%20write
```

## Rust DSL

```rust
b.from("direct:urlencoded-rust")
    .unmarshal(DataFormatDefinition::urlencoded())
    .set_body(simple("${body[name]}"));
```

## Options

None.

## Common questions

### How do I read a form field from a POST body?

`unmarshal: {urlencoded: {}}`, then `${body[field]}` in Simple. Repeated fields are lists.

### Are spaces written as + or %20?

`marshal` writes `%20`. `unmarshal` accepts both.

## Related

- [JSON data format](json.md)
- [Simple language](../languages/simple.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.

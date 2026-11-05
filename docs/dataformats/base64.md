# Base64 data format

> The `base64` data format encodes the message body as Base64 text (marshal) and decodes Base64 text back to bytes (unmarshal).

**Name:** `base64` · **Crate:** `cameleer-core` ·
**Apache Camel:** [Base64](https://camel.apache.org/components/next/dataformats/base64-dataformat.html)

## When to use it

- Put binary data (an image, a PDF, a compressed payload) into a JSON field, an email body or an
  HTTP header that only carries text.
- Send an image to a multimodal model API that expects Base64 content.
- Decode Base64 attachments or tokens received from another system.

## Marshal and unmarshal

| direction | body in | body out |
|---|---|---|
| `marshal` | any body (text is taken as UTF-8 bytes) | Base64 text, wrapped every `lineLength` characters |
| `unmarshal` | Base64 text (standard or URL-safe alphabet; line breaks ignored) | bytes |

## Example

Encode a body, then decode it again:

<!-- example: base64-encode -->
```yaml
- route:
    id: base64-encode
    from:
      uri: direct:base64-encode
      steps:
        - marshal:
            base64: {}
```

<!-- input: base64-encode -->
```text
Hello, Cameleer!
```

<!-- output: base64-encode -->
```text
SGVsbG8sIENhbWVsZWVyIQ==
```

<!-- example: base64-decode -->
```yaml
- route:
    id: base64-decode
    from:
      uri: direct:base64-decode
      steps:
        - unmarshal:
            base64: {}
        - convertBodyTo:
            type: String
```

<!-- input: base64-decode -->
```text
SGVsbG8sIENhbWVsZWVyIQ==
```

<!-- output: base64-decode -->
```text
Hello, Cameleer!
```

URL-safe output (no `+`, `/` or padding), handy for tokens in URLs:

<!-- example: base64-url-safe -->
```yaml
- route:
    id: base64-url-safe
    from:
      uri: direct:base64-url-safe
      steps:
        - marshal:
            base64:
              urlSafe: true
```

<!-- input: base64-url-safe -->
```text
??>>
```

<!-- output: base64-url-safe -->
```text
Pz8-Pg
```

## Rust DSL

```rust
b.from("direct:base64-rust")
    .marshal(DataFormatDefinition::base64().line_length(0));
```

## Options

| option | default | description |
|---|---|---|
| `lineLength` | `76` | Wrap the output after this many characters; `0` disables wrapping. |
| `lineSeparator` | CRLF | Separator inserted when wrapping (`\n`, `\r\n`). |
| `urlSafe` | `false` | Use the URL-safe alphabet (`-` and `_`) without padding. |

## Common questions

### How do I Base64-encode a file for an API request?

Read the file, `marshal: {base64: {lineLength: 0}}`, then put `${body}` into the JSON request. Use
`lineLength: 0` so the text has no line breaks.

### How do I encode a single header instead of the body?

Use the Simple functions `${base64Encode(${header.x})}` and `${base64Decode(...)}`; see the
[Simple language](../languages/simple.md).

### Why does the decoded body look like bytes?

`unmarshal` returns bytes. Add `convertBodyTo: {type: String}` when the content is text.

## Related

- [Hex data format](hex.md), [String data format](string.md), [GZip data format](gzip.md)
- [Simple language](../languages/simple.md) `base64Encode` / `base64Decode` functions
- [Apache Camel Base64 data format](https://camel.apache.org/components/next/dataformats/base64-dataformat.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.

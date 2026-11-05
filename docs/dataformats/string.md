# String data format

> The `string` data format converts the body to bytes in a chosen charset (marshal) and bytes back to text (unmarshal).

**Name:** `string` · **Crate:** `cameleer-core` ·
**Apache Camel:** [String](https://camel.apache.org/components/next/dataformats/string-dataformat.html)

## When to use it

- Talk to legacy systems that expect Latin-1 (ISO-8859-1) or ASCII instead of UTF-8.
- Decode bytes from a socket, a file or a message queue into text before parsing it.
- Make the byte encoding of a payload explicit.

## Marshal and unmarshal

| direction | body in | body out |
|---|---|---|
| `marshal` | text (bytes pass through unchanged) | bytes in `charset`. Characters the charset cannot hold become `?`. |
| `unmarshal` | bytes | text decoded from `charset` (invalid UTF-8 sequences are replaced) |

Supported charsets: `UTF-8` (default), `ISO-8859-1` (also `latin1`) and `US-ASCII` (`ASCII`).
Other names are treated as UTF-8.

## Example

Encode text as Latin-1 and show the bytes. `é` is the single byte `e9` in Latin-1 (it would be
`c3 a9` in UTF-8):

<!-- example: string-latin1 -->
```yaml
- route:
    id: string-latin1
    from:
      uri: direct:string-latin1
      steps:
        - marshal:
            string:
              charset: ISO-8859-1
        - marshal:
            hex: {}
```

<!-- input: string-latin1 -->
```text
café
```

<!-- output: string-latin1 -->
```text
636166e9
```

Decode Latin-1 bytes back into text:

<!-- example: string-decode-latin1 -->
```yaml
- route:
    id: string-decode-latin1
    from:
      uri: direct:string-decode-latin1
      steps:
        - unmarshal:
            hex: {}
        - unmarshal:
            string:
              charset: ISO-8859-1
```

<!-- input: string-decode-latin1 -->
```text
636166e9
```

<!-- output: string-decode-latin1 -->
```text
café
```

## Rust DSL

```rust
b.from("direct:string-rust")
    .marshal(DataFormatDefinition::string().charset("ISO-8859-1"));
```

## Options

| option | default | description |
|---|---|---|
| `charset` | `UTF-8` | `UTF-8`, `ISO-8859-1` / `latin1` or `US-ASCII`. |

## Common questions

### How do I convert bytes to a string?

`unmarshal: {string: {}}` decodes UTF-8. `convertBodyTo: {type: String}` does the same.

### Which charsets are supported?

UTF-8, ISO-8859-1 and US-ASCII. Apache Camel accepts any Java charset; Cameleer treats other names
as UTF-8.

## Related

- [Hex data format](hex.md), [Base64 data format](base64.md)
- [Apache Camel String data format](https://camel.apache.org/components/next/dataformats/string-dataformat.html)

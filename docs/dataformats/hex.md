# Hex data format

> The `hex` data format writes the message body as hexadecimal text (marshal) and reads hexadecimal text back into bytes (unmarshal).

**Name:** `hex` · **Crate:** `cameleer-core` · **Apache Camel:** no direct equivalent (Camel uses
`base64` or a type converter)

## When to use it

- Log or display binary payloads (sensor frames, Modbus or CAN messages from edge devices) in a
  readable form.
- Exchange binary data with systems that use hex strings, such as hashes, keys or device IDs.
- Check the exact bytes another data format produced.

## Marshal and unmarshal

| direction | body in | body out |
|---|---|---|
| `marshal` | any body (text is taken as UTF-8 bytes) | lowercase hex text (uppercase with `upperCase: true`) |
| `unmarshal` | hex text (whitespace ignored, even length) | bytes |

## Example

<!-- example: hex-encode -->
```yaml
- route:
    id: hex-encode
    from:
      uri: direct:hex-encode
      steps:
        - marshal:
            hex:
              upperCase: true
```

<!-- input: hex-encode -->
```text
Hi!
```

<!-- output: hex-encode -->
```text
486921
```

Decode a frame received as hex:

<!-- example: hex-decode -->
```yaml
- route:
    id: hex-decode
    from:
      uri: direct:hex-decode
      steps:
        - unmarshal:
            hex: {}
        - convertBodyTo:
            type: String
```

<!-- input: hex-decode -->
```text
48 65 6c 6c 6f
```

<!-- output: hex-decode -->
```text
Hello
```

## Rust DSL

```rust
b.from("direct:hex-rust")
    .marshal(DataFormatDefinition::hex());
```

## Options

| option | default | description |
|---|---|---|
| `upperCase` | `false` | Write `A`-`F` instead of `a`-`f`. |

## Common questions

### How do I see the bytes another data format produced?

Add `marshal: {hex: {}}` after it. The [GZip](gzip.md), [Avro](avro.md) and
[Protobuf](protobuf.md) pages use this to show their output.

### What happens with an odd number of hex digits?

`unmarshal` fails with a `DataFormatException` ("Hex string has odd length").

## Related

- [Base64 data format](base64.md), [String data format](string.md)
- [Apache Camel data formats](https://camel.apache.org/components/next/dataformats/index.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.

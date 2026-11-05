# CBOR data format

> The `cbor` data format encodes the message body as CBOR, a compact binary form of JSON (RFC 8949), and decodes CBOR back into a structured body.

**Name:** `cbor` · **Crate:** `cameleer-dataformats` ·
**Apache Camel:** [CBOR](https://camel.apache.org/components/next/dataformats/cbor-dataformat.html)

## When to use it

- Send sensor readings from constrained devices: CBOR is smaller than JSON and is the payload
  format of CoAP, LwM2M and many IoT stacks.
- Read CBOR from WebAuthn/COSE messages or devices, and turn it into JSON for the cloud side.
- Shrink JSON-like payloads on edge links without a schema.

## Marshal and unmarshal

| direction | body in | body out |
|---|---|---|
| `marshal` | any value (objects, lists, strings, numbers, booleans, null, bytes) | CBOR bytes, preferred serialization (shortest integers; floats as half, single or double precision, whichever is exact) |
| `unmarshal` | CBOR bytes (empty gives null) | a structured value. Indefinite lengths are supported; bignums become integers when they fit, else decimal strings; other tags are dropped. |

`marshal` encodes the body as it is: a JSON text body becomes a CBOR text string, so unmarshal the
JSON first.

## Example

Encode a reading and show the CBOR bytes (13 bytes, against 23 for the JSON text):

<!-- example: cbor-encode -->
```yaml
- route:
    id: cbor-encode
    from:
      uri: direct:cbor-encode
      steps:
        - unmarshal:
            json: {}
        - marshal:
            cbor: {}
        - marshal:
            hex: {}
```

<!-- input: cbor-encode -->
```json
{"temp":21.5,"ok":true}
```

<!-- output: cbor-encode -->
```text
a26474656d70f94d60626f6bf5
```

Decode CBOR from a device back to JSON:

<!-- example: cbor-decode -->
```yaml
- route:
    id: cbor-decode
    from:
      uri: direct:cbor-decode
      steps:
        - unmarshal:
            hex: {}
        - unmarshal:
            cbor: {}
        - marshal:
            json: {}
```

<!-- input: cbor-decode -->
```text
a26474656d70f94d60626f6bf5
```

<!-- output: cbor-decode -->
```json
{"temp":21.5,"ok":true}
```

## Rust DSL

```rust
b.from("direct:cbor-rust")
    .marshal(DataFormatDefinition::new("cbor"))
    .unmarshal(DataFormatDefinition::new("cbor"));
```

## Options

None. Camel's options (`unmarshalType`, `useList`, `allowJmsType`...) are accepted and ignored:
CBOR always maps to a generic value.

## Common questions

### How much smaller is CBOR than JSON?

Usually 20-50% for small messages: keys and numbers are stored in binary, without quotes or
separators. Compress it as well for large payloads.

### How do I convert CBOR to JSON?

`unmarshal: {cbor: {}}` then `marshal: {json: {}}`.

## Related

- [JSON data format](json.md), [Avro data format](avro.md), [Protobuf data format](protobuf.md)
- [Hex data format](hex.md)
- [Apache Camel CBOR data format](https://camel.apache.org/components/next/dataformats/cbor-dataformat.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.

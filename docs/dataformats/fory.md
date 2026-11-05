# Fory data format

> The `fory` data format serializes values to and from the Apache Fory (formerly Fury) cross-language binary format, so Rust, Java, Python, Go and JavaScript services can share compact payloads.

**Name:** `fory` · **Crate:** `cameleer-dataformats` ·
**Apache Camel:** [Fory](https://camel.apache.org/components/next/dataformats/fory-dataformat.html)

## When to use it

- Exchange compact binary payloads with services that already use Apache Fory's xlang mode.
- Replace JSON on a hot path (queues, caches) with a faster, smaller binary encoding while
  keeping dynamic, schema-less values.
- Read Fory records produced by a Java Camel route that uses `fory` with xlang enabled.

## Marshal and unmarshal

| direction | input body | output body |
|---|---|---|
| marshal | any value: null, boolean, integer, float, string, bytes, array, object | Fory xlang bytes |
| unmarshal | Fory xlang bytes | the value |

Mapping: integers are written as `VAR_INT64` and every integer type reads back as an integer;
floats as `FLOAT64`; strings as `STRING` (Latin-1 when every character fits, UTF-8
otherwise); bytes as `BINARY`; arrays as `LIST`; objects as `MAP`. An object with an `@type`
key (`"namespace.TypeName"`) is written as a named struct; reading such a struct needs its
field list (`structTypes`) unless it was written in compatible mode, which carries the field
names.

Marshal a string body as-is and you get a Fory `STRING`; to send a JSON document as a map,
`unmarshal: json` first, as in the example.

## Example

JSON to Fory and back. The `byteCount` header shows the binary size: 49 bytes for a
58-byte JSON document.

<!-- example: fory-round-trip -->
```yaml
- route:
    id: fory-round-trip
    from:
      uri: direct:fory
      steps:
        - unmarshal:
            json: {}
        - marshal:
            fory: {}
        - setHeader:
            name: byteCount
            simple: "${body.length()}"
        - unmarshal:
            fory: {}
        - marshal:
            json: {}
        - setBody:
            simple: "${header.byteCount} bytes: ${body}"
```

<!-- input: fory-round-trip -->
```json
{"device": "edge-7", "readings": [21, 22, 23], "ok": true}
```

<!-- output: fory-round-trip -->
```text
49 bytes: {"device":"edge-7","readings":[21,22,23],"ok":true}
```

Named structs in compatible mode: the type definition with field names travels with the
data, so the reader needs no `structTypes`.

<!-- example: fory-compatible-struct -->
```yaml
- route:
    id: fory-struct
    from:
      uri: direct:fory-struct
      steps:
        - unmarshal:
            json: {}
        - marshal:
            fory:
              compatible: true
        - unmarshal:
            fory:
              compatible: true
        - marshal:
            json: {}
```

<!-- input: fory-compatible-struct -->
```json
{"@type": "example.Person", "name": "Ada", "age": 36}
```

<!-- output: fory-compatible-struct -->
```text
{"@type":"example.Person","age":36,"name":"Ada"}
```

Struct fields are written in snake_case-sorted order, so they come back sorted.

## Rust DSL

```rust
b.from("direct:fory-rust")
    .unmarshal_json()
    .marshal(DataFormatDefinition::new("fory"))
    .unmarshal(DataFormatDefinition::new("fory"))
    .marshal_json();
```

## Options

| option | default | description |
|---|---|---|
| `refTracking` | `false` | Write reference flags so repeated strings and containers become back references. Alias `trackingRef`. |
| `compatible` | `false` | Write `@type` objects as compatible structs with a shared type definition (field names included). `compatibleMode: COMPATIBLE` does the same. |
| `structTypes` | (none) | Field lists of named structs for reading non-compatible structs: `example.Person=name,age;example.Address=city`. |
| `requireClassRegistration` | `false` | With `true`, marshalling an `@type` not listed in `structTypes` fails. |
| `xlangSpec` | `CURRENT` | Protocol edition: `CURRENT` (current xlang specification) or `LEGACY` (Fory 0.10/0.11 layout). Readers detect the edition from the header. |
| `threadSafe`, `allowAutoWiredFory`, `unmarshalType` | | Accepted for Camel compatibility; no effect. |

## Differences from Apache Camel

Camel's `fory` format serializes Java objects (Java native mode by default, classes
registered with Fory). This implementation speaks only the cross-language (xlang) protocol and
works with dynamic values; Java native payloads are rejected on unmarshal. Configure the Java
side with xlang enabled to interoperate.

## Common questions

### Is Fory smaller than JSON?

Usually: numbers are varints, there are no quotes or field names for lists, and strings are
length-prefixed. Measure with your own payloads; small maps with long keys gain less.

### Why does reading a struct fail with an unknown type?

A struct written without compatible mode has no field names in the data. Add
`structTypes: example.Person=name,age` (field order as the writer sorted them), or write
with `compatible: true`.

## Related

- [CBOR data format](cbor.md), [Avro data format](avro.md), [Protobuf data format](protobuf.md)
- [JSON data format](json.md)
- [Apache Camel Fory data format](https://camel.apache.org/components/next/dataformats/fory-dataformat.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.

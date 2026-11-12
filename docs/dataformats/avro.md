# Avro data format

> The `avro` data format encodes the message body as Apache Avro (binary, JSON or Object Container File) using an Avro schema, and decodes Avro data back into a structured body.

**Name:** `avro` · **Also:** `avroJackson` (`avro-jackson`, `avrojackson`, `jacksonAvro`) ·
**Crate:** `cameleer-dataformats` · **Cargo feature:** [`dataformats`](../guides/build-features.md) ·
**Apache Camel:** [Avro](https://camel.apache.org/components/next/dataformats/avro-dataformat.html),
[Avro Jackson](https://camel.apache.org/components/next/dataformats/avroJackson-dataformat.html)

## When to use it

- Produce and consume Avro records on Kafka topics shared with Java, Python or Spark services.
- Read and write Avro container files (`.avro`) in data lakes.
- Keep sensor or event payloads small and schema-checked on edge links.

## Marshal and unmarshal

| direction | body in | body out |
|---|---|---|
| `marshal` | a structured value matching the schema (`avroJackson` also accepts JSON text) | `encoding: binary` (default): a bare Avro datum, like Camel. `json`: Avro JSON encoding text. `container`: an Object Container File with the schema in its header (a list body writes one record per item). |
| `unmarshal` | Avro data | a structured value: records and maps become objects, enums strings, `bytes`/`fixed` bytes, unions the selected branch. Container files are detected and need no schema; they give a list of records. |

With `logicalTypeConversions` (default on), `date`, `time-*` and `timestamp-*` values decode to
ISO-8601 strings, `decimal` to decimal strings and `duration` to `{months, days, millis}`; marshal
accepts these forms as well as the raw numbers.

**Difference from Apache Camel:** no generated classes. The schema comes from `schema`,
`schemaFile` or a registry bean named by `instanceClassName`, and values are generic.

## Example

The schema used by the examples:

<!-- file: sensor.avsc -->
```json
{"type":"record","name":"Reading","namespace":"edge","fields":[
  {"name":"id","type":"string"},
  {"name":"temp","type":"double"},
  {"name":"ok","type":"boolean"}]}
```

Encode a JSON reading as binary Avro and show the bytes. The 13 bytes are the string length and
text, the 8-byte double and the boolean; field names are not sent:

<!-- example: avro-encode-binary -->
```yaml
- route:
    id: avro-encode
    from:
      uri: direct:avro-encode
      steps:
        - unmarshal:
            json: {}
        - marshal:
            avro:
              schemaFile: sensor.avsc
        - marshal:
            hex: {}
```

<!-- input: avro-encode-binary -->
```json
{"id":"s-1","temp":21.5,"ok":true}
```

<!-- output: avro-encode-binary -->
```text
06732d31000000000080354001
```

Decode binary Avro (for example a Kafka record value) to JSON:

<!-- example: avro-decode-binary -->
```yaml
- route:
    id: avro-decode
    from:
      uri: direct:avro-decode
      steps:
        - unmarshal:
            hex: {}
        - unmarshal:
            avro:
              schemaFile: sensor.avsc
        - marshal:
            json: {}
```

<!-- input: avro-decode-binary -->
```text
06732d31000000000080354001
```

<!-- output: avro-decode-binary -->
```json
{"id":"s-1","temp":21.5,"ok":true}
```

`avroJackson` works on JSON text directly. Here it writes a container file and reads it back
without any schema option, since the file carries its schema:

<!-- example: avro-jackson-container -->
```yaml
- route:
    id: avro-container
    from:
      uri: direct:avro-container
      steps:
        - marshal:
            avroJackson:
              schemaFile: sensor.avsc
              encoding: container
        - unmarshal:
            avroJackson:
              jsonView: true
```

<!-- input: avro-jackson-container -->
```json
[{"id":"s-1","temp":21.5,"ok":true},{"id":"s-2","temp":19.0,"ok":false}]
```

<!-- output: avro-jackson-container -->
```json
[{"id":"s-1","temp":21.5,"ok":true},{"id":"s-2","temp":19.0,"ok":false}]
```

## Rust DSL

```rust
b.from("direct:avro-rust")
    .unmarshal(DataFormatDefinition::json())
    .marshal(DataFormatDefinition::new("avro").option("schemaFile", "sensor.avsc"))
    .unmarshal(DataFormatDefinition::new("avro").option("schemaFile", "sensor.avsc"));
```

## Options

| option | default | description |
|---|---|---|
| `schema` | (none) | The schema as inline JSON. |
| `schemaFile` (`schemaLocation`) | (none) | Path of an `.avsc` file (`file:` and `classpath:` prefixes accepted). |
| `instanceClassName` | (none) | Full name of the type to use from the schema, or the name of a registry bean holding the schema. |
| `encoding` | `binary` | `binary`, `json` or `container` (Object Container File). |
| `codec` | `null` | Container compression: `null` or `deflate`. |
| `syncInterval` | `64000` | Container block size in bytes. |
| `contentTypeHeader` | `true` | Set `Content-Type` (`avro/binary` or `application/json`). |
| `logicalTypeConversions` | `true` | Convert logical types to and from readable strings. |
| `jsonView` | `false` | Unmarshal to JSON text instead of a structured value. |
| `library` | (none) | `Jackson` gives the `avroJackson` behaviour (marshal accepts JSON text). |

## Common questions

### How do I decode Avro messages from Kafka?

Consume with the [kafka](../components/kafka.md) component and `unmarshal: {avro: {schemaFile: x.avsc}}`.
The value must be a bare datum; Confluent's 5-byte schema-registry prefix is not stripped.

### Do I need the schema to read an .avro file?

No. Container files embed their schema; `unmarshal` reads it from the header.

### Why does marshal fail with my JSON text body?

The plain `avro` format expects a structured value: add `unmarshal: {json: {}}` first, or use
`avroJackson`, which parses JSON text itself.

## Related

- [Protobuf data format](protobuf.md), [Thrift data format](thrift.md), [CBOR data format](cbor.md)
- [JSON data format](json.md), [Hex data format](hex.md)
- [Apache Camel Avro data format](https://camel.apache.org/components/next/dataformats/avro-dataformat.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.

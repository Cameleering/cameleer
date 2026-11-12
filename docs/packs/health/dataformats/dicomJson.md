# DICOM JSON data format

> The `dicomJson` data format writes DICOM objects as the DICOM JSON model of PS3.18 Annex F, the format of DICOMweb metadata and search results, and reads that JSON back into a Value tree.

**Name:** `dicomJson` · **Crate:** `cameleer-health` · **Pack:** [health](../index.md) (feature `health`) · **Cargo feature:** [`health`](../../../guides/build-features.md) ·
**Apache Camel:** no equivalent (Cameleer extension)

## When to use it

- Index incoming studies in a search engine or database (Elasticsearch, MongoDB, PostgreSQL
  JSONB) as DICOM JSON.
- Hand metadata to an AI model or a web viewer that expects DICOMweb JSON.
- Build an instance from JSON: `unmarshal` DICOM JSON with inline binary, then `marshal` with
  the [`dicom`](dicom.md) data format.

## Example

<!-- example: dicom-json-index -->
```yaml
- route:
    id: index-metadata
    from:
      uri: "dicom:INDEXER@0.0.0.0:{{env:DICOM_PORT:11112}}"
      steps:
        - marshal:
            dicomJson:
              bulkDataThreshold: "1024"
        - to: "file:{{env:OUT_DIR:metadata}}?fileName=${header.CamelDicomSopInstanceUid}.json"
```

Each received instance is written as its DICOM JSON, without Pixel Data and other values over
1 KB.

## What it writes

```json
{
  "00100010": {"vr": "PN", "Value": [{"Alphabetic": "Doe^Jane"}]},
  "00100020": {"vr": "LO", "Value": ["P1"]},
  "00280010": {"vr": "US", "Value": [512]},
  "7FE00010": {"vr": "OW", "BulkDataURI": "https://pacs/studies/.../bulkdata/7FE00010"}
}
```

- Binary values are base64 `InlineBinary`, or `BulkDataURI` when over `bulkDataThreshold` and a
  `bulkDataUri` template is set (left out otherwise). Encapsulated Pixel Data is always left out
  unless `bulkDataUri` is set.
- The file meta information (group 0002) is left out unless `includeFileMeta=true`.
- `marshal` takes Part 10 bytes, a Value tree, or an array of either (written as a JSON array).
  It sets `Content-Type: application/dicom+json`.
- `unmarshal` takes JSON text (an object or an array) and gives Value trees with
  `InlineBinary` decoded to bytes.

## Options

| option | default | description |
|---|---|---|
| `includeFileMeta` | `false` | Keep group 0002. |
| `array` | `false` | Wrap a single object in an array, as DICOMweb responses are. |
| `bulkDataThreshold` | none | Values larger than this many bytes become `BulkDataURI` (or are left out). |
| `bulkDataUri` | | URI template for left-out values: `{tag}` and `{sopInstanceUid}` are replaced. |
| `pretty` | `false` | Indent the JSON. |

## Rust DSL

```rust
use cameleer_core::dataformat::DataFormatDefinition;
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_pack(&cameleer_health::HEALTH);
    ctx.add_routes(|b| {
        b.from("direct:json").marshal(DataFormatDefinition::new("dicomJson").option("array", true));
    })
}
```

## Common questions

### Is the output exactly what a DICOMweb server returns?

It follows PS3.18 Annex F. DS and IS values whose text would change as a JSON number (`"1.50"`)
stay strings, so no precision or formatting is lost; most readers accept both.

## Related

- [dicom data format](dicom.md), [dicomweb](../components/dicomweb.md), [json](../../../dataformats/json.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.

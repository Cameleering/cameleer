# DICOM data format

> The `dicom` data format reads DICOM Part 10 files into a Value tree shaped like the DICOM JSON model, and writes such trees back to Part 10, byte for byte when nothing changed.

**Name:** `dicom` · **Crate:** `cameleer-health` · **Pack:** [health](../index.md) (feature `health`) · **Cargo feature:** [`health`](../../../guides/build-features.md) ·
**Apache Camel:** no equivalent (Cameleer extension)

## When to use it

- Change tags before forwarding: fix a wrong Institution Name, add an Accession Number, remove a
  private block.
- Convert between native transfer syntaxes (Implicit, Explicit, Deflated, Big Endian) for a
  receiver that wants a particular one.
- Look at every attribute of an instance in a route, for logging or for rules that the
  [`dicom` language](../languages/dicom.md) cannot express.

To read a few tags you do not need it: `${dicom.PatientID}` parses the body on the fly.

## Example

<!-- example: dicom-df-coerce -->
```yaml
- route:
    id: coerce-institution
    from:
      uri: "dicom:GATEWAY@0.0.0.0:{{env:DICOM_PORT:11112}}"
      steps:
        - unmarshal:
            dicom: {}
        - script:
            groovy: |
              body['00080080'] = [vr: 'LO', Value: ['Example Imaging Centre']]
        - marshal:
            dicom:
              transferSyntax: 1.2.840.10008.1.2.1
        - to: "dicom:PACS@{{env:PACS_HOST:pacs.local}}:104"
```

## The Value tree

A data set is an object keyed by the eight-hex-digit tag. Each element has `vr` and, when it has
a value, one of:

| key | for | value |
|---|---|---|
| `Value` | text, numbers, person names, AT, sequences | an array: strings, numbers, `{"Alphabetic": ..., "Ideographic": ..., "Phonetic": ...}` for PN, item objects for SQ; empty values are `null` |
| `InlineBinary` | OB, OD, OF, OL, OV, OW, UN | the bytes |
| `Fragments` | encapsulated (compressed) Pixel Data | the Basic Offset Table, then each fragment, as bytes (kept unchanged) |

```json
{
  "00020010": {"vr": "UI", "Value": ["1.2.840.10008.1.2.1"]},
  "00100010": {"vr": "PN", "Value": [{"Alphabetic": "Doe^Jane"}]},
  "00180050": {"vr": "DS", "Value": [5]},
  "00081140": {"vr": "SQ", "Value": [{"00081155": {"vr": "UI", "Value": ["1.2.3"]}}]},
  "7FE00010": {"vr": "OW", "InlineBinary": "<bytes>"}
}
```

The file meta information (group 0002) is in the tree too, so a file read and written again is
the same file. DS and IS values are numbers when the number prints back as the same text, and
strings otherwise (`"1.50"`). Keys that start with `_` keep the encoding exact and are left out
of DICOM JSON: `_undefinedLength` (sequences and items written with undefined length), `_raw`
(the original bytes of a value whose re-encoding would differ, used again only while the value is
unchanged), `_preamble` and `_transferSyntax`.

When marshalling, an element can also be written in short form, `"00100020": "P1"` or a keyword
key, `"PatientID": "P1"`: the VR then comes from the dictionary.

Strings are decoded with the Specific Character Set (0008,0005) of the data set (items inherit
it): the ISO 8859 sets, JIS X 0201, TIS 620, ISO 2022 with JIS X 0208 and 0212, KS X 1001 and
GB 2312, UTF-8, GB18030 and GBK. They are encoded back in the same set.

## What it reads and writes

- Part 10 files with or without the preamble, files without file meta information, and bare
  data sets (the transfer syntax is guessed).
- Explicit and Implicit VR Little Endian, Deflated Explicit VR Little Endian and Explicit VR Big
  Endian, in both directions; every compressed transfer syntax with its pixel data as fragments.
- Sequences and items of defined and undefined length, UN of undefined length, private
  attributes (kept, not interpreted), retired group lengths (kept as they are).

## Options

| option | default | description |
|---|---|---|
| `stopBeforePixelData` | `false` | `unmarshal`: leave Pixel Data out (faster for metadata). |
| `transferSyntax` | the object's | `marshal`: write in this transfer syntax. Native syntaxes convert into each other; anything involving a compressed syntax fails with `DicomTranscodeException` (no pixel codecs in this build). |

`unmarshal` sets `CamelDicomSopClassUid`, `CamelDicomSopInstanceUid` and
`CamelDicomTransferSyntaxUid`.

## Rust DSL

```rust
use cameleer_core::dataformat::DataFormatDefinition;
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_pack(&cameleer_health::HEALTH);
    ctx.add_routes(|b| {
        b.from("direct:to-implicit")
            .marshal(DataFormatDefinition::new("dicom").option("transferSyntax", "1.2.840.10008.1.2"));
    })
}
```

The Rust API works on `cameleer_health::dicom::DicomFile` and `DataSet` directly:
`DicomFile::parse(&bytes, ReadOptions::default())`, `dataset.string(tag)`, `dataset.put_string(...)`,
`file.to_bytes()`.

## Common questions

### Where does the dictionary come from?

From the DocBook XML of the DICOM standard, edition 2026d (PS3.6 data elements and UIDs, PS3.7
command elements), generated into a compact table by `scripts/update-dicom-dict.py`, which checks
pinned SHA-256 digests. See `crates/cameleer-health/DICTIONARY.md`.

### Has it been tested on real files?

Yes: 33 public test files from pydicom (NEMA WG-04 images, David Clunie's character set and
deflate samples, RT objects, an SR, private sequences, big endian and implicit files) all read
and write back byte for byte, directly and through the Value tree
(`scripts/fetch-dicom-testdata.sh`, `tests/dicom_corpus.rs`).

## Related

- [dicomJson](dicomJson.md), [dicomDeidentify](dicomDeidentify.md), [dicom language](../languages/dicom.md), [dicom component](../components/dicom.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.

# DICOM language

> The `dicom` language reads an attribute of the DICOM object in the message body by keyword, tag or sequence path, such as `PatientID`, `(0010,0020)` or `ReferencedSeriesSequence[0].SeriesInstanceUID`; Simple has the same paths as `${dicom.PatientID}`.

**Name:** `dicom` · **Crate:** `cameleer-health` · **Pack:** [health](../index.md) (feature `health`) · **Cargo feature:** [`health`](../../../guides/build-features.md) · **Apache Camel:** no equivalent (Cameleer extension)

## When to use it

- Route studies by modality, body part, station or calling AE: a content-based router for
  imaging.
- Copy identifiers into headers (Study Instance UID, Accession Number) before the body is
  changed, compressed or de-identified.
- Write log lines and file names from tags.

## Example

<!-- example: dicom-language-route -->
```yaml
- route:
    id: route-by-modality
    from:
      uri: "dicom:ROUTER@0.0.0.0:{{env:DICOM_PORT:11112}}"
      steps:
        - setHeader:
            name: StudyUid
            dicom: StudyInstanceUID
        - choice:
            when:
              - simple: "${dicom.Modality} == 'CT' && ${dicom.BodyPartExamined} == 'HEAD'"
                steps:
                  - to: "dicom:STROKE_AI@{{env:AI_HOST:ai.local}}:104"
              - simple: "${dicom.Modality} in 'MG,DX'"
                steps:
                  - to: "dicom:MAMMO@{{env:MAMMO_HOST:mammo.local}}:104"
            otherwise:
              steps:
                - to: "dicom:PACS@{{env:PACS_HOST:pacs.local}}:104"
```

## Paths

| path | value |
|---|---|
| `PatientID` | the value as text, several values joined with `\` (keywords from the PS3.6 dictionary) |
| `(0010,0020)`, `00100020` | the same, by tag |
| `ImageType[2]` | the third value of a multi-valued attribute |
| `ReferencedSeriesSequence[0].SeriesInstanceUID` | an attribute inside the first item of a sequence (`[n]` defaults to `0` in the middle of a path) |
| `ReferencedSeriesSequence` | the items, as keyword maps |
| `PixelData` | the bytes |
| `TransferSyntaxUID` | file meta attributes work too |

An attribute that is not there gives `null`. Text is decoded with the object's Specific Character
Set. The body can be Part 10 bytes (it is parsed up to Pixel Data, unless the path asks for Pixel
Data) or a Value tree from the [`dicom` data format](../dataformats/dicom.md), so the paths see
changes made to the tree.

## Rust DSL

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_pack(&cameleer_health::HEALTH);
    ctx.add_routes(|b| {
        b.from("dicom:ROUTER@0.0.0.0:11112")
            .set_header("StudyUid", language("dicom", "StudyInstanceUID"))
            .filter(simple("${dicom.Modality} == 'CT'"))
            .to("dicom:PACS@pacs.local:104");
    })
}
```

## Common questions

### Why `${dicom.X}` and not `${body.X}`?

The body is binary DICOM; `${dicom.X}` parses it and decodes the value with the right character
set and VR. `cameleer_health::register` adds the `dicom` function to Simple (any crate can add
Simple functions through `cameleer_core::language::simple::register_function`).

### Is the body parsed for every expression?

Yes, up to Pixel Data, which is fast for typical headers (a few kilobytes). For many expressions
on large objects, unmarshal once with the `dicom` data format and evaluate on the tree.

## Related

- [dicom data format](../dataformats/dicom.md), [simple](../../../languages/simple.md), [hl7terser](hl7terser.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.

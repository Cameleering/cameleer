# HL7 Terser language

> The `hl7terser` language reads a field from an HL7 v2 message with a terser path such as `PID-5-1` (patient family name).

**Name:** `hl7terser` · **Crate:** `cameleer-health` · **Pack:** [health](../index.md) (feature `health`) · **Cargo feature:** [`health`](../../../guides/build-features.md) · **Apache Camel:** [HL7 Terser](https://camel.apache.org/components/next/languages/hl7terser-language.html)

## When to use it

- Route hospital messages by type (`MSH-9`, `MSH-9-2`) or sending application (`MSH-3`).
- Copy patient or order fields into headers before converting the message to JSON for another
  system, or before handing a de-identified summary to an AI service.

## Syntax

The body is an HL7 v2 message in ER7 text (segments separated by `\r` or newlines) or the
structure produced by the [hl7 data format](../dataformats/hl7.md).

| path | meaning |
|---|---|
| `PID-5` | segment `PID`, field 5 (the first component when the field has components) |
| `PID-5-2` | field 5, component 2 |
| `PID-5-1-1` | sub-component |
| `PID-3(1)-1` | the second repetition (0-based) of field 3, component 1 |
| `OBX(1)-5` | the second `OBX` segment (0-based), field 5 |
| `/.MSH-10` | a leading `/.` is accepted, as in HAPI terser paths |

For `MSH`, field 1 is the field separator and field 2 the encoding characters, as in HL7.
Escape sequences (`\F\`, `\S\`...) are decoded. A field that is absent in an existing segment
gives an empty string; a missing segment gives null. An invalid path fails when the route is
created.

## Examples

<!-- example: hl7terser-route-by-type -->
```yaml
- route:
    id: hl7terser-demo
    from:
      uri: direct:hl7terser
      steps:
        - setHeader:
            name: patient
            hl7terser: PID-5-1
        - setHeader:
            name: mrn
            hl7terser: PID-3-1
        - choice:
            when:
              - hl7terser: PV1-2
                steps:
                  - setBody:
                      simple: "visit for ${header.patient} (MRN ${header.mrn})"
            otherwise:
              steps:
                - setBody:
                    simple: "no visit for ${header.patient}"
```

A terser path used as a predicate is true when the field has a value:

<!-- input: hl7terser-route-by-type -->
```text
MSH|^~\&|ADT|HOSP|EHR|HOSP|20240102120000||ADT^A01|MSG0001|P|2.5
PID|1||12345^^^MRN||Doe^John
PV1|1|I
```

<!-- output: hl7terser-route-by-type -->
```text
visit for Doe (MRN 12345)
```

<!-- input: hl7terser-route-by-type#2 -->
```text
MSH|^~\&|ADT|HOSP|EHR|HOSP|20240102120000||ADT^A08|MSG0002|P|2.5
PID|1||12345^^^MRN||Doe^John
```

<!-- output: hl7terser-route-by-type#2 -->
```text
no visit for Doe
```

Read several fields, including repetitions and repeated segments:

<!-- example: hl7terser-fields -->
```yaml
- route:
    id: hl7terser-fields
    from:
      uri: direct:hl7terser-fields
      steps:
        - setHeader:
            name: type
            hl7terser: MSH-9
        - setHeader:
            name: event
            hl7terser: MSH-9-2
        - setHeader:
            name: secondId
            hl7terser: PID-3(1)-1
        - setHeader:
            name: secondObx
            hl7terser: OBX(1)-5
        - setBody:
            simple: "${header.type}^${header.event} ids=${header.secondId} obx=${header.secondObx}"
```

<!-- input: hl7terser-fields -->
```text
MSH|^~\&|LAB|HOSP|EHR|HOSP|20240102120000||ORU^R01|MSG0001|P|2.5
PID|1||12345^^^MRN~67890^^^SSN||Doe^John
OBX|1|ST|GLU||5.4
OBX|2|ST|HB||13.9
```

<!-- output: hl7terser-fields -->
```text
ORU^R01 ids=67890 obx=13.9
```

## Options

None.

## Rust DSL

```rust
b.from("direct:hl7terser-rust")
    .set_header("patient", language("hl7terser", "PID-5-1"))
    .set_body(language("hl7terser", "MSH-9-2"));
```

## Common questions

### How do I get the message type of an HL7 message?

`hl7terser: MSH-9` gives the message code (`ADT`), `MSH-9-2` the trigger event (`A01`). The
[hl7 data format](../dataformats/hl7.md) also sets `CamelHL7MessageType` and `CamelHL7TriggerEvent`
headers when it unmarshals.

### Are repetition and segment indexes 0-based?

Yes, as in HAPI: `PID-3(0)` is the first repetition, `OBX(1)` the second OBX segment.

## Related

- [HL7 data format](../dataformats/hl7.md), [mllp component](../components/mllp.md)
- [Simple language](../../../languages/simple.md)
- [Apache Camel HL7 Terser language](https://camel.apache.org/components/next/languages/hl7terser-language.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.

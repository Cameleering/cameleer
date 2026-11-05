# FHIR data formats

> The `fhirJson` and `fhirXml` data formats parse HL7 FHIR resources from JSON or XML into a value and serialize resources back, which also converts between the two FHIR encodings.

**Name:** `fhirJson`, `fhirXml` · **Crate:** `cameleer-soap` ·
**Apache Camel:** [FHIR JSon](https://camel.apache.org/components/next/dataformats/fhirJson-dataformat.html),
[FHIR XML](https://camel.apache.org/components/next/dataformats/fhirXml-dataformat.html)

## When to use it

- Accept FHIR resources (Patient, Observation, Bundle...) from apps and EHRs and route on
  their content.
- Convert FHIR XML from older systems to FHIR JSON for modern APIs, or the other way round.
- Strip narratives or selected elements before passing a resource to an analytics job or an
  LLM, so it only sees the fields it needs.

## Marshal and unmarshal

| direction | input body | output body |
|---|---|---|
| unmarshal (`fhirJson` / `fhirXml`) | FHIR JSON / FHIR XML text | a resource value in the FHIR JSON model: `resourceType`, arrays for repeating elements, `_name` for primitive extensions, typed numbers and booleans |
| marshal (`fhirJson` / `fhirXml`) | a resource value, or FHIR JSON / XML text | FHIR JSON / XML text; sets `Content-Type` to `application/fhir+json` / `application/fhir+xml` |

Because marshal also accepts FHIR text in either encoding, `marshal: fhirXml` directly on a
JSON body converts it to XML (and `marshal: fhirJson` on XML converts to JSON). XML output
follows the specification's element order. Decimals keep their lexical form (`1.50` stays
`1.50`).

## Example

Convert a FHIR JSON Patient to FHIR XML.

<!-- example: fhir-json-to-xml -->
```yaml
- route:
    id: fhir-json-to-xml
    from:
      uri: direct:fhir-xml
      steps:
        - marshal:
            fhirXml:
              prettyPrint: false
```

<!-- input: fhir-json-to-xml -->
```json
{"resourceType": "Patient", "id": "p1", "active": true,
 "name": [{"family": "Doe", "given": ["Jane"]}], "birthDate": "1980-01-01"}
```

<!-- output: fhir-json-to-xml -->
```xml
<Patient xmlns="http://hl7.org/fhir"><id value="p1"/><active value="true"/><name><family value="Doe"/><given value="Jane"/></name><birthDate value="1980-01-01"/></Patient>
```

Unmarshal an Observation, route on its value, and drop the narrative before forwarding:

<!-- example: fhir-observation-route -->
```yaml
- route:
    id: fhir-observation
    from:
      uri: direct:fhir-observation
      steps:
        - unmarshal:
            fhirJson: {}
        - setHeader:
            name: glucose
            simple: "${body.valueQuantity.value}"
        - marshal:
            fhirJson:
              suppressNarratives: true
        - setBody:
            simple: "${header.glucose} -> ${body}"
```

<!-- input: fhir-observation-route -->
```json
{"resourceType": "Observation", "status": "final",
 "text": {"status": "generated", "div": "<div xmlns=\"http://www.w3.org/1999/xhtml\">Glucose 5.4</div>"},
 "code": {"coding": [{"system": "http://loinc.org", "code": "15074-8"}]},
 "valueQuantity": {"value": 5.4, "unit": "mmol/L"}}
```

<!-- output: fhir-observation-route -->
```text
5.4 -> {"resourceType":"Observation","status":"final","code":{"coding":[{"system":"http://loinc.org","code":"15074-8"}]},"valueQuantity":{"value":5.4,"unit":"mmol/L"}}
```

## Rust DSL

```rust
b.from("direct:fhir-rust")
    .unmarshal(DataFormatDefinition::new("fhirXml"))
    .marshal(DataFormatDefinition::new("fhirJson"));
```

## Options

| option | default | description |
|---|---|---|
| `fhirVersion` | `R4` | `R4`, `R4B`, `R5` or `DSTU3`: selects the type registry (element order, types). |
| `prettyPrint` | `false` | Indent the output. |
| `suppressNarratives` | `false` | Drop the `text` narrative. |
| `omitResourceId` | `false` | Leave out the resource `id`. |
| `encodeElements` | (none) | Only encode these elements, comma separated (`Patient.name` or `name`). |
| `dontEncodeElements` | (none) | Leave out these elements. |
| `stripVersionsFromReferences` | `false` | Remove `/_history/n` from references. |
| `contentTypeHeader` | `true` | Set `Content-Type` on marshal. |

`summaryMode`, `serverBaseUrl`, `parserOptions`, `preferTypes`, `forceResourceId`,
`overrideResourceIdWithBundleEntryFullUrl`, `encodeElementsAppliesToChildResourcesOnly` and
`dontStripVersionsFromReferencesAtPaths` are accepted for compatibility.

**Known limitation:** `suppressNarratives` currently removes every element named `text` at
any depth, not only the resource narrative. That also drops `CodeableConcept.text`,
`Annotation.text` and similar fields, so do not enable it when you need those (HAPI only
removes the `Narrative`).

## Differences from Apache Camel

Camel uses HAPI FHIR and produces HAPI model classes. Here resources are plain values in the
FHIR JSON shape, so Simple paths like `${body.name[0].family}` read them, and there is no
HAPI validation (the [FHIR component](../components/fhir.md) offers validation against the
base profiles).

## Common questions

### How do I convert FHIR XML to JSON?

`marshal: fhirJson: {}` on the XML text (or `unmarshal: fhirXml` then `marshal: fhirJson`).

### How do I read a field of a FHIR resource in a route?

Unmarshal with `fhirJson` (or `fhirXml`) and use Simple: `${body.name[0].given[0]}`,
`${body.valueQuantity.value}`.

### Which FHIR versions are supported?

DSTU3, R4 (default), R4B and R5 serialization rules, chosen with `fhirVersion`.

## Related

- [HL7 data format](hl7.md): HL7 v2 messages
- [FHIR component](../components/fhir.md)
- [JSON data format](json.md), [Jackson XML data format](jacksonxml.md)
- [Apache Camel FHIR JSON](https://camel.apache.org/components/next/dataformats/fhirJson-dataformat.html)
  and [FHIR XML](https://camel.apache.org/components/next/dataformats/fhirXml-dataformat.html)

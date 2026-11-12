# DICOM de-identification

> The `dicomDeidentify` data format (and the `DeidentifyProcessor`) removes or replaces identifying attributes of DICOM instances with the PS3.15 Annex E Basic Application Level Confidentiality Profile and its options, repeatably under a secret key, and writes an audit record for each instance.

**Name:** `dicomDeidentify` · **Crate:** `cameleer-health` · **Pack:** [health](../index.md) (feature `health`) · **Cargo feature:** [`health`](../../../guides/build-features.md) ·
**Apache Camel:** no equivalent (Cameleer extension)

## When to use it

- Send studies to an external AI model or a teleradiology partner without patient identity.
- Build research and training datasets: the same patient, study and series keep the same new
  identifiers across batches and across machines that share the key.
- Keep a verifiable record of what was de-identified, under which policy, without storing any
  identifying value.

## Example

<!-- example: dicom-deidentify -->
```yaml
- route:
    id: deidentify-for-research
    from:
      uri: "dicom:RESEARCH@0.0.0.0:{{env:DICOM_PORT:11112}}"
      steps:
        - marshal:
            dicomDeidentify:
              uidKey: "{{env:DEID_KEY:change-me}}"
              options: "retainLongitudinalModifiedDates,retainPatientCharacteristics,cleanDescriptors"
              pseudonymousPatient: "true"
        - to: "dicomweb:{{env:RESEARCH_DICOMWEB:http://localhost:8042/dicom-web}}"
```

`marshal` and `unmarshal` do the same thing: de-identify the body. Part 10 bytes stay bytes and
a Value tree stays a tree. `CamelDicomSopInstanceUid` is set to the new instance UID.

## What it does

Every attribute listed in Table E.1-1 of the 2026d edition gets the action of the basic profile,
or of the first selected option that names one:

| code | action here |
|---|---|
| `X` | removed |
| `Z`, `X/Z` | zero length (kept, empty) |
| `D`, `Z/D`, `X/D`, `X/Z/D` | a dummy value of the VR (`ANONYMOUS`, `19000101`, `0`...), which keeps the object valid whatever the attribute's type |
| `K` | kept |
| `U` | a new UID: `uidRoot` + the decimal HMAC-SHA256 of the old UID under `uidKey` |
| `X/Z/U*` | the sequence is kept and its items de-identified (their UIDs replaced) |
| `C` | dates and date-times shifted by a per-patient number of days (with the Modified Dates option); free text removed (Clean Descriptors, Structured Content, Graphics), since it cannot be shown to be free of identity |

- Private attributes are removed, except those listed in `safePrivateTags` with the Retain Safe
  Private option.
- Attributes not in the table are kept. Sequences are de-identified item by item.
- Patient Identity Removed (0012,0062) becomes `YES`; De-identification Method (0012,0063) and
  De-identification Method Code Sequence (0012,0064) list the profile and options (CID 7050
  codes `113100` to `113112`); Longitudinal Temporal Information Modified (0028,0303) is
  `REMOVED`, `MODIFIED` or `UNMODIFIED`.
- With `pseudonymousPatient=true`, Patient ID and Patient's Name become a pseudonym derived from
  the original Patient ID (`ANON-` + 12 hex digits), the same for every instance of the patient.

**Deterministic:** the same instance and key give the same output, byte for byte. References
stay consistent: a Referenced SOP Instance UID gets the same new value as the instance it points
to, wherever that instance is de-identified.

**Audit:** each instance appends a record to the hash-chained audit log of the action layer
(`camel.ai.audit.file`, `camel.ai.audit.enabled`): tool `dicom-deidentify`, SHA-256 digests of the
input and output, the profile, edition and options, the counts, the new SOP Instance UID and a
short digest of the key. No identifying value is recorded. `cameleer_ai::audit::verify_file`
checks the chain.

## Options

| option | default | description |
|---|---|---|
| `uidKey` | required | The secret for replacement UIDs, pseudonyms and date shifts. Keep it in a vault (`{{vault:...}}`) or a property. |
| `profile` | `basic` | Only `basic`. |
| `options` | | Comma list: `retainSafePrivate`, `retainUids`, `retainDeviceIdentity`, `retainInstitutionIdentity`, `retainPatientCharacteristics`, `retainLongitudinalFullDates`, `retainLongitudinalModifiedDates`, `cleanDescriptors`, `cleanStructuredContent`, `cleanGraphics`. |
| `uidRoot` | `2.25` | Root of the new UIDs (numeric, at most 40 characters). |
| `dateShiftDays` | `365` | With Modified Dates: dates move by 1 to this many days, earlier or later, per patient. |
| `pseudonymousPatient` | `false` | Replace Patient ID and Patient's Name with a stable pseudonym. |
| `pseudonymPrefix` | `ANON-` | Prefix of the pseudonym. |
| `safePrivateTags` | | `CREATOR:gggg,xxee` entries separated by `;` (for example `ACME 1.1:0019,xx10`), kept with Retain Safe Private. |
| `rejectBurnedInAnnotation` | `true` | Refuse instances with Burned In Annotation `YES` (`DicomDeidentificationException`). |

## Rust DSL

```rust
use cameleer_core::prelude::*;
use cameleer_health::dicom::deid::{Deidentifier, DeidentifyProcessor, Options};

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_pack(&cameleer_health::HEALTH);
    let mut d = Deidentifier::new(b"secret-from-a-vault", Options::parse("retainLongitudinalModifiedDates")?);
    d.pseudonymous_patient = true;
    ctx.add_routes(move |b| {
        b.from("direct:deid").process(DeidentifyProcessor::new(d.clone())).to("mock:out");
    })
}
```

## Common questions

### Is this a conforming PS3.15 de-identifier?

It applies Table E.1-1 of the current edition attribute by attribute (the tests check every
attribute of a test instance against the table for six option sets) and adds the attributes the
profile requires. It does not offer Clean Pixel Data or Clean Recognizable Visual Features, which
need pixel codecs and region rules; instances that declare burned-in annotation are refused by
default. The Encrypted Attributes Sequence (for re-identification) is not written.

### Can I re-identify data later?

Not from the output: replacement UIDs and pseudonyms are one-way HMAC values. Keep your own
mapping (for example by logging the original and new SOP Instance UIDs to a secure store in the
same route) if you need to go back.

### Does it handle FHIR too?

Not yet. The design keeps one rules engine for DICOM and FHIR; `fhirDeidentify` is planned.

## Related

- [dicom data format](dicom.md), [dicom component](../components/dicom.md), [dicomweb](../components/dicomweb.md)
- [DICOM guide](../guides/dicom.md) (de-identification inside a confidential VM)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.

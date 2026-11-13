# Health pack

> The health pack (`cameleer-health`) makes Cameleer an open-source healthcare integration engine: HL7 v2 over MLLP, FHIR R4, DICOM over DIMSE and DICOMweb, and PS3.15 de-identification, as Apache Camel routes in the same single `cameleer` binary.

**Crate:** `cameleer-health` 0.6.0 · **Cargo feature:** [`health`](../../guides/build-features.md) (not in `full`) · **Image:** `cameleering/cameleer:health`

For an overview of what healthcare teams build with it, how it compares with an interface engine
such as Mirth Connect, and what it does not do yet, see
[healthcare integration with Cameleer](https://cameleer.ing/cameleer/healthcare/).

## Get it

| you use | command |
|---|---|
| Docker | `docker run --rm -v "$PWD:/routes:ro" cameleering/cameleer:health run /routes/route.camel.yaml` (also `:<version>-health`, and on ghcr.io) |
| cargo | `cargo install cameleer-main --features health` |
| a small edge build | `cargo build --profile dist -p cameleer-main --no-default-features --features "health,http2"` |
| a release archive | `cameleer-health-<version>-<target>.tar.gz` (or `.zip`) |
| Rust | depend on `cameleer-health` and call `ctx.add_pack(&cameleer_health::HEALTH)` |

`cameleer version` shows the pack: `Cameleer 0.6.0 + health 0.6.0 (core 0.6.0)`. In a build
without it, a route that names `mllp`, `fhir`, `dicom` or `dicomweb` fails at start-up with a
message naming the pack.

## Verify a download

From the next release, release archives are published, each with a SHA-256 file, a CycloneDX SBOM
(`.cdx.json`) and a Sigstore bundle (`.sigstore.json`) per file, signed by the release workflow.
0.6.0 has no archives: use the image (`cameleering/cameleer:health`) or `cargo install`. To verify
an archive once they are published:

```sh
sha256sum -c cameleer-health-<version>-<target>.tar.gz.sha256
cosign verify-blob   --bundle cameleer-health-<version>-<target>.tar.gz.sigstore.json   --certificate-identity-regexp '^https://github.com/cameleering/cameleer/\.github/workflows/archives\.yml@refs/tags/v'   --certificate-oidc-issuer https://token.actions.githubusercontent.com   cameleer-health-<version>-<target>.tar.gz
```

The SBOM lists exactly the crates compiled into that binary: the `cameleer-*` crates and the
Rust standard library, and no third-party crates. `scripts/sbom.py` writes it from `cargo tree`
for any feature set, for example a pack-only edge build
(`--no-default-features --features health`) or the `cameleer-health` crate on its own. The
images carry their SBOM and build provenance as OCI attestations.

## What it contains

| crate feature | components | data formats | languages |
|---|---|---|---|
| `hl7` | [mllp](components/mllp.md) | [hl7](dataformats/hl7.md) | [hl7terser](languages/hl7terser.md) |
| `fhir` | [fhir](components/fhir.md) | [fhirJson, fhirXml](dataformats/fhir.md) | |
| `dicom` | [dicom](components/dicom.md), [dicomweb](components/dicomweb.md) | [dicom](dataformats/dicom.md), [dicomJson](dataformats/dicomJson.md), [dicomDeidentify](dataformats/dicomDeidentify.md) | [dicom](languages/dicom.md) and `${dicom.*}` in Simple |

All three crate features are on by default; a Rust embedder can take only one
(`cameleer-health = { version = "0.6", default-features = false, features = ["dicom"] }`).

The [DICOM Conformance Statement](conformance/dicom-conformance-statement-0.6.0.md) (PS3.2 form,
its tables generated from the code) lists the services, policies and profiles of this release.
The [DICOM guide](guides/dicom.md) explains the imaging parts end to end, with the conformance
summary and what has been verified. The examples are in `examples/medical/` and
`examples/edge/hl7-mllp-to-http.camel.yaml`; they need the pack.

## Example

<!-- example: health-pack-hl7-to-fhir -->
```yaml
- route:
    id: adt-to-fhir
    from:
      uri: "mllp://0.0.0.0:{{env:MLLP_PORT:2575}}"
      steps:
        - setHeader:
            name: PatientId
            hl7terser: PID-3-1
        - log: "ADT ${header.CamelMllpTriggerEvent} for patient ${header.PatientId}"
        - to: "log:adt"
```

## Common questions

### Is there an open-source integration engine for HL7, FHIR and DICOM?

The health pack is one, under the Apache-2.0 licence: `mllp` with automatic acknowledgements, the
`hl7` data format and `hl7terser` paths, the `fhir` component with `fhirJson` and `fhirXml`, the
`dicom` (DIMSE) and `dicomweb` components, and `dicomDeidentify`. Interfaces are YAML routes with
Apache Camel's names, error handling and routing patterns.

### Can it replace Mirth Connect?

For teams that keep interfaces as code, it can: channels become routes in Git, checked with
`cameleer validate` and `cameleer test`. There is no visual channel editor, no message browser and
no importer for Mirth channels, and Mirth's JavaScript transformers do not run.

### Which image has HL7 and FHIR now?

The health pack image `cameleering/cameleer:health` (or `:0.6.0-health`), or a build with
`--features health`. The default image and `full` no longer include `mllp`, `hl7`, `hl7terser`,
`fhir`, `fhirJson` and `fhirXml`; up to 0.5.1 they were in the default image.

### What has it been tested against?

The live tier (`CAMEL_HEALTH_INTEROP=1`) passes 10 of 10 against DCMTK 3.6.7, Orthanc 1.12.2 with
DICOMweb 1.16, python-hl7 0.4.5 and HAPI FHIR 8.12.0; 33 public pydicom test files read and write
back byte for byte. Cloud healthcare services (Azure, Google, AWS) are not tested yet.

### Is it HIPAA compliant?

Compliance belongs to a deployment. The pack provides controls (TLS and mutual TLS, PS3.15
de-identification with audit records, a hash-chained audit log); it is not certified for HIPAA or
cleared by the FDA.

### How do I ask for a missing feature?

[Open a feature request](https://github.com/cameleering/cameleer/issues/new?template=feature_request.md&title=Health+pack%3A+)
with your workflow, the systems involved and what you use today; it is answered and worked on in
the issue.

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.

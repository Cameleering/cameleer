# Health pack

> The health pack (`cameleer-health`) adds healthcare integration to Cameleer: HL7 v2 over MLLP, FHIR R4, DICOM over DIMSE and DICOMweb, and PS3.15 de-identification, in the same single `cameleer` binary.

**Crate:** `cameleer-health` 0.6.0 · **Cargo feature:** [`health`](../../guides/build-features.md) (not in `full`) · **Image:** `pmsudhi/cameleer:health`

## Get it

| you use | command |
|---|---|
| Docker | `docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer:health run /routes/route.camel.yaml` (also `:<version>-health`, and on ghcr.io) |
| cargo | `cargo install cameleer-main --features health` |
| a small edge build | `cargo build --profile dist -p cameleer-main --no-default-features --features "health,http2"` |
| a release archive | `cameleer-health-<version>-<target>.tar.gz` (or `.zip`) |
| Rust | depend on `cameleer-health` and call `ctx.add_pack(&cameleer_health::HEALTH)` |

`cameleer version` shows the pack: `Cameleer 0.5.1 + health 0.6.0 (core 0.5.1)`. In a build
without it, a route that names `mllp`, `fhir`, `dicom` or `dicomweb` fails at start-up with a
message naming the pack.

## Verify a download

Every release archive comes with a SHA-256 file, a CycloneDX SBOM (`.cdx.json`) and, for
releases, a Sigstore bundle (`.sigstore.json`) per file, signed by the release workflow:

```sh
sha256sum -c cameleer-health-<version>-<target>.tar.gz.sha256
cosign verify-blob   --bundle cameleer-health-<version>-<target>.tar.gz.sigstore.json   --certificate-identity-regexp '^https://github.com/pmsudhi/cameleer/\.github/workflows/archives\.yml@refs/tags/v'   --certificate-oidc-issuer https://token.actions.githubusercontent.com   cameleer-health-<version>-<target>.tar.gz
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

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.

# Packs

> A pack is an optional set of components, data formats and languages for one industry, built and released as its own crate and distribution, so the general Cameleer build stays small and focused.

Cameleer's default build (`full`) is a general integration engine: transports, data formats, EIPs,
cloud and SaaS services that many unrelated users need. Code that serves one industry and its
standards lives in a pack instead. A pack:

- is one crate (`cameleer-<pack>`) with its own version, change log and test tier;
- is added to a build with one cargo feature of `cameleer-main` (`--features <pack>`), which is
  **not** part of `full`;
- has its own distribution: the image tag `pmsudhi/cameleer:<pack>` and the archive
  `cameleer-<pack>-<version>-<target>`, with the same `cameleer` command inside;
- ships, for each release, an SBOM of exactly what is compiled in (`scripts/sbom.py`), SHA-256
  sums and Sigstore signatures of its archives, and its own change log
  (`crates/cameleer-<pack>/CHANGELOG.md`);
- describes itself (`cameleer version` prints `health 0.6.0 (core 0.5.1)`), so validation, the
  YAML DSL and error messages know its components without central tables.

A route that names a pack's component in a build without the pack fails at start-up with a message
naming the pack, the image and the feature to use.

## The packs

| pack | crate | what it adds | pages |
|---|---|---|---|
| health | `cameleer-health` | HL7 v2 and MLLP, FHIR R4, DICOM (DIMSE and DICOMweb), PS3.15 de-identification | [health pack](health/index.md) |

## When something becomes a pack

A component belongs in a pack when any of these hold:

1. It serves one industry and its standards (HL7, DICOM, SWIFT, ISO 20022, FIX, OPC UA).
2. Buyers validate or certify it separately (conformance statements, connectathons).
3. It brings large, specialised code (dictionaries, codecs, profiles) most users never run.
4. It needs its own test tier against domain tools.

It belongs in `full` when it is a general capability, has no domain-specific compliance artifacts,
and can follow Cameleer's release cadence.

## Embedding a pack in Rust

```rust
use cameleer_core::prelude::*;

pub fn routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_pack(&cameleer_health::HEALTH);
    ctx.add_routes(|b| {
        b.from("mllp://0.0.0.0:2575").to("log:hl7");
    })
}
```

`Main::add_pack` does the same for a runner, and `cameleer_core::pack::install` installs a pack for
every runner the process creates.

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.

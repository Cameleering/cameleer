# Domain packs: a health pack, and how Cameleer grows without feature creep

_Internal design, 2026-11-09. Written in answer to the owner's question: should the DICOM work
(branch `dicom`) go into `main` as one more feature, or should all medical code go into a pack that
users load only when they need it? Nothing was changed in code, git, GitHub, crates.io or Docker Hub._

_Update the same day: the owner chose the health pack and said no backward compatibility is needed
("no one started using it"). So `mllp`, `hl7`, `hl7terser` and `fhir` move straight into the pack,
with no aliases, re-exports or deprecation shims. Section 8 is written to be implemented directly._

Every fact is tagged:

- **[verified]**: checked today in this repository or at the cited URL.
- **[docs]**: taken from the cited documentation or article, not tested here.
- **[unverified]**: plausible, but not confirmed. Check before relying on it.
- **[judgment]**: a recommendation.

## 0. Decision table

| question | answer |
|---|---|
| Merge DICOM into `main` as one more feature in `full`? | **No.** That is the feature creep the owner wants to avoid [judgment] |
| Make a health pack? | **Yes: one crate, `cameleer-health`**, in this workspace, holding HL7 v2, MLLP, `hl7terser`, FHIR, DICOM, DICOMweb and de-identification [judgment] |
| Separate repository now? | **No.** Same workspace, separate crate, separate distribution. Move it out only when the criteria in section 6 say so [judgment] |
| Runtime-loadable packs (dylib or WASM)? | **No.** Rust has no stable ABI, and a std-only WASM runtime is a large project. Third-party extensions out of process use the MCP client that already exists [judgment] |
| Is the pack in `full`? | **No.** `full` means "the general integration engine". The pack is the cargo feature `health`, and the distribution is `cameleer-health` [judgment] |
| What does a user install? | `cargo install cameleer-main --features health`, or the image `cameleering/cameleer:health` (and `ghcr.io/.../cameleer:<version>-health`), or the `cameleer-health-<version>-<target>` archive [judgment] |
| Extension point in core | A small `Pack` trait plus `PackInfo` metadata in `cameleer_core::pack`. The metadata replaces three hard-coded tables (section 4.3) [judgment] |
| Versioning | The pack crate has its own version (starting `0.6.0`), depends on `cameleer-core = "0.6"`, and can ship patch releases on its own [judgment] |
| Per pack release | CycloneDX SBOM, cosign signature, DICOM Conformance Statement, test report [judgment] |
| Backward compatibility for `mllp`, `hl7`, `fhir` | **None** (owner's decision). A route that names them in a build without the pack already fails at startup with a message naming the feature; the message names `health` [verified for the mechanism, `crates/cameleer-main/src/features.rs`] |

## 1. What is true today

- `cameleer-main` exposes component families as cargo features. `default = ["full"]`, and `full`
  lists 16 families plus `templates`, `jq`, `xpath`, `http2`, `validate`
  (`crates/cameleer-main/Cargo.toml`) [verified].
- Registration is a fixed sequence of `#[cfg(feature = "...")] cameleer_x::register(main.context())`
  calls in `new_main_inner` (`crates/cameleer-main/src/lib.rs`, lines 921-963). Order matters: `xml`
  first, `ssh` before `protocols` [verified].
- The `dicom` branch (5 commits, 75 files, +21,406 lines) adds `cameleer-dicom` with 17,322 lines of
  source, a `dicom` feature added **to `full`**, and edits to three central tables: `features.rs`
  (`FEATURES`, `COMPONENTS`, `LANGUAGES`), `cameleer-yaml/src/dsl.rs` (`LANGUAGES`), plus the
  register call [verified, `git diff --stat main...dicom`].
- `cameleer-dicom` depends on `cameleer-core`, `cameleer-http`, `cameleer-tls` **and `cameleer-ai`**,
  only for the hash-chained audit record and `sha256_hex` in `deid.rs` [verified].
- Healthcare code already on `main` is spread over two unrelated crates:
  - `cameleer-protocols`: `hl7.rs` (384 lines: the `hl7` data format and the `hl7terser` language)
    and `mllp.rs` (517 lines), which use `common::{TcpServer, connect_stream, split_host_port, ...}`
    [verified];
  - `cameleer-soap`: `src/fhir/` (8 files, 3,458 lines), which uses `cameleer-xml`, `cameleer-http`,
    `support::now_utc` and `transport::Client` [verified].
- The Simple language has a global extension hook, `cameleer_core::language::simple::register_function`
  (added on the branch for `${dicom.*}`) [verified].

The branch, as it stands, follows the same path every family took: one more crate, one more
feature, one more entry in `full`, and edits to central tables. The edits to central tables are the
part that does not scale: each new domain touches core files.

## 2. How comparable ecosystems package domain extensions

| ecosystem | unit of extension | how it is loaded | curated sets | source |
|---|---|---|---|---|
| Apache Camel | one jar per component (`org.apache.camel:camel-mllp`, since 2.17; `camel-hl7`, `camel-fhir`) in the main repo | classpath at build time | none for healthcare; Kamelet catalog versioned separately from Camel | [Camel MLLP](https://camel.apache.org/components/next/mllp-component.html) [docs]; [camel-kamelets](https://github.com/apache/camel-kamelets) ("Camel K ... will only use a specific version of this Kamelet catalog") [docs] |
| Camel Quarkus | one extension per component (`camel-quarkus-hl7`, `-mllp`, `-fhir`) | build time, native image | Red Hat sells a supported subset with support levels per extension | [Red Hat reference](https://docs.redhat.com/en/documentation/red_hat_build_of_apache_camel/4.8/html/red_hat_build_of_apache_camel_for_quarkus_reference/camel-quarkus-extensions-overview) [docs] |
| Spring Boot | starter + auto-configure module | classpath | third parties must not use the `spring-boot` prefix; name it `acme-spring-boot-starter` | [Spring Boot reference](https://docs.spring.io/spring-boot/docs/2.0.0.M6/reference/html/boot-features-developing-auto-configuration.html) [docs] |
| OpenTelemetry Collector | Go module per component; core repo and contrib repo | compiled in by the builder (`ocb`) from a manifest | six official distributions: `otelcol`, `otelcol-contrib`, `otelcol-k8s`, `otelcol-otlp`, `otelcol-prometheus`, `otelcol-ebpf-profiler` | [releases repo](https://github.com/open-telemetry/opentelemetry-collector-releases), [ocb](https://opentelemetry.io/docs/collector/extend/ocb/) [docs] |
| Vector (Rust) | cargo feature per source, transform, sink | compile time | the default build has everything; `FEATURES="sources-file,..." make build` | [custom builds](https://vector.dev/highlights/2020-02-28-custom-vector-builds/) [docs] |
| Envoy | extensions compiled in, each with a security posture; separate contrib build | compile time; WASM at run time | `envoy` and `envoy-contrib` builds | [threat model](https://www.envoyproxy.io/docs/envoy/latest/intro/arch_overview/security/threat_model) [docs] |
| Fluent Bit | C plugins compiled in; Go output plugins as shared objects; WASM filters | compile time, `.so`, WASM | one build with CMake switches | [build options](https://docs.fluentbit.io/manual/installation/downloads/source/build-and-install), [WASM filters](https://docs.fluentbit.io/manual/development/wasm-filter-plugins) [docs] |
| Kong | Lua plugins in process; Go/Python/JS plugins in a separate process over a socket | run time | Kong Hub | [external plugins](https://docs.jp.konghq.com/gateway/2.8.x/reference/external-plugins/) [docs] |
| Apache NiFi | NAR (a zip of jars) with its own class loader | run time | none | [NiFi NARs](https://docs.cloudera.com/cfm/2.1.1/nifi-dev-guide/topics/nifi-nars.html) [docs] |
| Mirth / NextGen Connect | extensions (connectors, plugins), some sold separately | run time (Java) | from 4.6, commercial only; SSL Manager, Channel History, Message Generator in the Enterprise package | [NextGen, 2025-03-19](https://www.nextgen.com/blog/industry-news/a-new-era-for-mirth-connect-by-nextgen-healthcare) [docs] |
| Rhapsody | engine plus licensed modules; DICOM routing is a **separate product**, Rhapsody Image Director | product boundary | per product | [Rhapsody brochure 2026](https://rhapsody.health/wp-content/uploads/2023/03/rg_Product_brochure_Rhapsody-Integration-Engine_2026.pdf) [docs, vendor marketing] |
| Bevy (Rust) | `Plugin` trait with `build(&self, app: &mut App)`; `PluginGroup`; third-party plugins are crates | compile time | `DefaultPlugins`, `MinimalPlugins` | [Bevy plugins](https://bevy.org/learn/quick-start/getting-started/plugins/) [docs] |
| Linux distributions | base system plus add-on packages | package manager | task or meta packages | general knowledge [unverified as cited] |

### Patterns

1. **Compile-time features in one crate graph** (Vector, Cameleer today). Cheap and fast, but every
   option lives in one namespace, and the default build tends to grow until it has everything.
2. **Separate crates or artifacts in the same repository** (Camel's jars, Camel Quarkus extensions).
   Each unit has its own name, docs and dependency surface, but one release train and one CI.
3. **Separate repositories with their own governance** (OpenTelemetry contrib, Envoy contrib,
   Kamelets, Kubernetes cloud providers). They are used when the core team cannot or should not
   review and secure everything. OpenTelemetry contrib asks for a sponsor from a different company
   and at least three code owners for a new component, and wants the component built and hosted
   outside the repository first. "Unmaintained components will be excluded from the default build"
   ([new-components.md](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/docs/new-components.md)) [docs].
   Envoy says contrib extensions are "not officially covered by the threat model or the Envoy
   security team" [docs].
4. **Curated distributions** (OpenTelemetry's six, Envoy contrib, Red Hat's supported subset,
   Bevy's `DefaultPlugins`). One code base, several named binaries, each for an audience. This is
   the pattern that answers "people don't need to load it" without splitting the code.
5. **Runtime plugins** (NiFi NARs, Mirth, Kong, Fluent Bit `.so` and WASM, Envoy WASM). All of them
   sit on a runtime with a stable ABI (JVM, Lua, C) or embed a WASM VM. Kong's out-of-process model
   shows the third way: a plugin is a separate program speaking a protocol.

The healthcare vendors draw the line in the same place the owner suggests. Rhapsody sells DICOM
routing as a separate product, and Mirth sells extensions apart from the engine. The open projects
(OpenTelemetry, Envoy) draw it by who maintains and secures the code. **Nobody puts a whole vertical
into the default build of a general engine** [judgment, based on the table].

## 3. Rust specifics

### 3.1 Runtime loading costs too much for a std-only project

- **No stable Rust ABI.** The only stable boundary is `extern "C"`. stabby's docs say the C ABI "has
  no concept of sum-types", and `abi_stable` defines a new incompatible ABI per 0.y version and
  "assumes that dynamic libraries come from a benign source"
  ([stabby](https://docs.rs/crate/stabby/0.1.5), [abi_stable](https://docs.rs/abi_stable)) [docs].
  Both are third-party crates, so they are out anyway.
- **dlopen in std only** means declaring `dlopen`/`dlsym` and `LoadLibraryW`/`GetProcAddress` by hand
  (the way `simoom-sys` does OS calls). It also means a C-shaped API for `Component`, `Endpoint`,
  `Exchange`, `Value` and errors, with ownership rules across the boundary, and a plugin built with
  exactly the same compiler and core version if any Rust type crosses. That is a second public API,
  larger than the first [judgment].
- **WASM** needs a std-only interpreter or JIT: a validator, interpreter, WASI and a host API. The
  YAML DSL already lists `wasm` as a language that is "not compiled"
  (`crates/cameleer-main/src/validate.rs`, `NOT_COMPILED`) [verified]. It would be a project the
  size of the script interpreters, and slow without a JIT [judgment].
- **Out of process** costs nothing new: Cameleer already has an MCP client and gateway
  (`camel.ai.mcp.NAME.*`, HTTP and stdio, rug-pull protection) and the `exec` component. A
  third-party extension can be an MCP server or a process on stdio, in any language, in its own
  sandbox. That is Kong's model, and it is how untrusted code should be loaded anyway (the proxy-wasm
  spec recommends an out-of-process VM for untrusted modules,
  [WebAssembly in Envoy](https://www.github.com/proxy-wasm/spec/blob/main/docs/WebAssembly-in-Envoy.md)) [docs].
- **Security.** A runtime-loaded native plugin runs with the process's full rights. Signing helps
  with provenance, but not with a bug in the plugin. A compiled-in pack is covered by the same SBOM,
  signature and tests as the binary [judgment].

### 3.2 Features or crates

Cargo features should be additive: Cargo builds a dependency once with the union of all features
asked for anywhere in the graph, and a feature should not introduce a SemVer-incompatible change
([smithy-rs RFC 15](https://smithy-lang.github.io/smithy-rs/design/rfcs/rfc0015_using_features_responsibly.html),
quoting the Cargo book) [docs]. That suits packs: `health` only adds components.

| | feature in an existing crate | separate crate behind a feature |
|---|---|---|
| discoverability | hidden in a feature list | its own crates.io page, docs.rs, keywords (`healthcare`, `dicom`, `hl7`, `fhir`) |
| compile time | rebuilt with the host crate | own compilation unit, cached separately, parallel |
| versioning | the host crate's version | its own version |
| dependency surface | the host's dependencies | only what the pack needs |
| library users | must depend on the host | can depend on `cameleer-health` alone, for example to parse DICOM |

So: **a separate crate, and a feature in `cameleer-main` that turns it on**. This is what the branch
already does for DICOM. The change is to put all healthcare code in that crate and keep it out of
`full`.

### 3.3 Is the component API a good enough plugin surface?

Mostly yes. `cameleer_core::spi::{Component, Endpoint, Language}`, `dataformat::DataFormat`,
`CamelContext::add_component_factory`, `add_language`, `register_data_format_factory` and the
Simple hook are enough to implement everything on the branch [verified by reading the branch]. Three
things are missing, and each one is a place where adding a domain edits core files today:

1. **Static metadata tables.** `cameleer-main/src/features.rs` lists every feature, component and
   language by hand, and `cameleer-yaml/src/dsl.rs` keeps a `LANGUAGES` list that validation uses.
   A pack must be able to declare these itself.
2. **No pack identity.** There is no way to ask a running binary which packs and versions it has,
   which a validated deployment needs ("which de-identification code produced this file?").
3. **Audit in the wrong place.** The hash-chained audit record lives in `cameleer-ai`, so the
   health pack depends on the AI crate to write a de-identification audit. Audit is a core concern
   for any regulated pack.

## 4. Standards that apply to a pack system

### 4.1 Supply chain

- **SBOM.** CycloneDX ([cyclonedx.org](https://cyclonedx.org/)) and SPDX ([spdx.dev](https://spdx.dev/))
  [docs, not fetched today]. Std-only makes the SBOM short: the `cameleer-*` crates, the Rust
  standard library and the toolchain version. Generate it without third-party tools from
  `cargo metadata --format-version 1 --locked`, which a std-only Python script or a `cameleer`
  subcommand can turn into CycloneDX JSON. Publish one per distribution and one per pack crate.
- **Signing.** Sigstore cosign signs container images and blobs. Attaching an SBOM does not sign it,
  so signing is a separate step ([Sigstore, other types](https://docs.sigstore.dev/cosign/signing/other_types/))
  [docs]. Recent cosign stores signatures as OCI 1.1 referrers where the registry supports them
  ([Chainguard](https://www.chainguard.dev/de-DE/unchained/building-towards-oci-v1-1-support-in-cosign))
  [docs]. `container.yml` already attaches SBOM and provenance to images
  (see `docs/design/medical/dicom-and-healthcare.md` section 4) [verified in that document]. The
  release archives also need signing (`cosign sign-blob`) and a published checksum.
- **OCI artifacts.** Packs are compiled in, so the OCI unit is the distribution image
  (`cameleering/cameleer:health`). OCI artifacts for pack files would only matter for runtime packs,
  which are rejected.
- **SemVer ranges.** In Cargo, `"0.6"` means `>=0.6.0, <0.7.0`: on 0.x a minor bump is breaking. A
  pack built against `cameleer-core 0.6` works with every 0.6.y [docs, Cargo book].

### 4.2 What regulated healthcare buyers need

- **A DICOM Conformance Statement** for each product version that claims conformance. It is "a single
  Conformance Statement" with irrelevant sections kept and marked not applicable, and the network part
  follows the Annex N template
  ([PS3.2](https://dicom.nema.org/medical/dicom/current/output/chtml/part02/chapter_6.html)) [docs].
  The statement names the Implementation Class UID and Implementation Version Name sent in
  association requests (PS3.7 Annex D), so each pack release gets its own version name
  [unverified: Annex D wording not re-read today].
- **IHE profiles** (ATNA, SWF.b, XDS-I.b, PIX/PDQ, AIR) are claimed per product version in an IHE
  Integration Statement [unverified as cited; see the medical research document].
- **FDA, when an OEM embeds Cameleer in a device.** Section 524B(b)(3) of the FD&C Act requires an
  SBOM for "cyber devices", and FDA finalized the "select updates" to its premarket cybersecurity
  guidance in late June 2025, which recommend an SBOM and still ask for one with changes "unlikely to
  impact cybersecurity" ([Hogan Lovells, 2025-07-03](https://www.hlc.com/en/publications/fda-finalizes-cyber-device-select-updates-guidance))
  [docs]. Reports of a February 2026 reissue are [unverified on fda.gov]. The OEM is the submitter;
  Cameleer's job is to make the OEM's SBOM and vulnerability management easy.

**Why this argues for a pack.** A hospital or an OEM validates a specific build. If DICOM is inside
the general binary, every release (a Kafka fix, a new SaaS component) is a new version of the medical
product that they have to re-assess. With a pack and a `cameleer-health` distribution, the medical
pieces have their own version, conformance statement, SBOM and change log. A release that does not
touch the pack can be shown not to touch it [judgment].

## 5. Feature creep: where mature projects draw the core line

- **OpenTelemetry:** core holds the pipeline, the OTLP components and the APIs; everything vendor- or
  domain-specific goes to contrib, which needs a sponsor, three code owners and a stability level,
  and gets dropped from the default build when unmaintained [docs, cited above].
- **Envoy:** core extensions must have a security posture and fall under the threat model; contrib
  does not [docs].
- **Kubernetes:** in-tree cloud providers were moved out over several years; 1.31 removed the last of
  them, which the project called the largest migration in its history. The drivers were the cost of
  keeping vendor code in the core and vendor neutrality
  ([Kubernetes blog](https://kubernetes.io/blog/2024/05/20/completing-cloud-provider-migration/),
  [CNCF](https://cncf.io/blog/2024/09/27/why-kubernetes-is-removing-in-tree-cloud-provider-integration-support-in-v1-31-and-how-it-can-affect-you)) [docs].
  The lesson: moving things out later is much more expensive than starting outside.
- **Camel:** keeps every component in the main repository but as separate artifacts, so users pull
  only what they name [docs].

### Criteria for Cameleer [judgment]

A component belongs in **core or `full`** when all of these hold:

1. It is a general integration capability (a transport, a data format, an EIP, a cloud or SaaS
   service) that many unrelated users need.
2. It has no domain-specific compliance artifacts (no conformance statement, no certification).
3. Its release cadence can follow Cameleer's.

A component belongs in **a pack** when any of these hold:

1. It serves one industry and its standards (HL7, DICOM, SWIFT, ISO 20022, FIX, OPC UA, IEC 61850).
2. Buyers validate or certify it separately (conformance statements, connectathons, accreditation).
3. It brings large, specialised code (dictionaries, codecs, profiles) most users never run.
4. It needs its own test tier against domain tools (dcmtk, Orthanc, HAPI FHIR).

A pack moves to **its own repository** when any of these hold:

1. It has maintainers or contributors who are not Cameleer core maintainers.
2. Its release cadence diverges for more than two releases in a row.
3. It is licensed differently (for example a commercial pack).
4. Its CI time or test infrastructure slows down core work.

None of these is true for the health pack today, so it stays in the workspace.

## 6. Options compared

| | (a) merge as a feature | (b) pack crate + distribution, same repo | (c) separate repo | (d) runtime packs |
|---|---|---|---|---|
| UX | `cameleer` has everything (if in `full`) or a rebuild (if not) | `cameleer` for general use, `cameleer-health` for healthcare, one feature to add it | same as (b), and the user must match versions across repos | download and load a pack file |
| default build size | grows by the pack (DICOM not measured; see risks) | unchanged | unchanged | unchanged |
| release cadence | locked to core | own version, released from the same repo | fully independent | independent, needs ABI version checks |
| testing burden | in the main suite | in the main suite, plus a health tier | duplicated CI, cross-repo breakage found late | an ABI test matrix on top |
| repo/org | none | none | a new repo under `cameleer-rs` | new repo and a pack registry |
| positioning | "one more component family" | "Cameleer for Healthcare", its own landing page and image | same as (b) | same as (b) |
| commercial options | none | a validated distribution, support, LTS | also a differently licensed pack | also closed packs |
| std-only | fine | fine | fine (family rule from `family-crates-std-only.md`) | dlopen by hand or a WASM VM: large, risky |
| feature creep | **yes** | no | no | no |

(b) gives the owner what they asked for (a pack users do not load unless they want it, a pattern for
other domains, no creep in core) at the lowest cost, and keeps the path to (c) open.

## 7. Commercial patterns (laid out, not recommended)

- **Open core by buyer** (GitLab: features tiered by who buys them,
  [GitLab handbook](https://handbook.gitlab.com/handbook/product/product-processes/tiering-guidance-for-features/)) [docs].
- **Supported, validated builds of open code** (Red Hat's build of Camel and Camel Quarkus: same
  code, support levels per extension) [docs, cited above]. For healthcare, the paid item can be the
  evidence (conformance statement, test reports, SBOM and VEX, long-term support backports, a managed
  service under a BAA) rather than the code.
- **Going proprietary** (Mirth 4.6). It produced community forks within weeks: Open Integration
  Engine in April 2025 and BridgeLink ([OIE](https://github.com/openintegrationengine/engine)) [docs].
  A healthcare community punishes a closed turn quickly.

A pack that is Apache-2.0 like the rest keeps every option open. A paid pack, if ever, belongs in a
separate repository under a different licence (section 5 criterion 3) [judgment].

## 8. Recommendation: the health pack, ready to implement

### 8.1 Names

| thing | name |
|---|---|
| pack | **health** |
| crate | **`cameleer-health`** (module path `cameleer_health`) |
| cargo feature in `cameleer-main` | **`health`** (not in `full`) |
| crate features inside `cameleer-health` | `hl7`, `fhir`, `dicom`, all on by default |
| distribution | **`cameleer-health`**: archive `cameleer-health-<version>-<target>.tar.gz`/`.zip`, image tags `cameleering/cameleer:health` and `:<version>-health`, same on ghcr.io |
| binary inside the distribution | still `cameleer` (same CLI, more components), so docs and scripts are the same |
| future packs | `cameleer-finance` (SWIFT MT, ISO 20022, FIX), `cameleer-industrial` (OPC UA, Modbus, Sparkplug B), `cameleer-energy` (IEC 61850, DNP3) [judgment: examples only] |

"health" rather than "medical": it covers HL7 and FHIR administrative and clinical data, not only
devices, and it matches common usage (FHIR, IHE, "health IT").

### 8.2 Crate layout and what moves

One crate, because the owner asked for "all medical related code" in one place, and the parts share
de-identification, audit and the DICOM-FHIR bridge later. Crate features keep library users able to
take only DICOM or only HL7.

```
crates/cameleer-health/
  Cargo.toml          features: default = ["hl7", "fhir", "dicom"]
  src/lib.rs          pub static PACK: PackInfo; pub fn register(ctx); impl Pack
  src/hl7/mod.rs      <- crates/cameleer-protocols/src/hl7.rs     (hl7 data format, hl7terser language)
  src/hl7/mllp.rs     <- crates/cameleer-protocols/src/mllp.rs    (mllp component)
  src/fhir/*.rs       <- crates/cameleer-soap/src/fhir/*.rs       (8 files: fhir component, fhirJson, fhirXml)
  src/dicom/*.rs      <- crates/cameleer-dicom/src/*.rs            (branch dicom: Part 10, dictionary,
                                                                     DIMSE SCU/SCP, dicomweb, deid, dicomJson,
                                                                     dicom language, ${dicom.*})
  tests/mllp.rs       <- crates/cameleer-protocols/tests/mllp.rs
  tests/mllp_tls.rs   <- the MLLP cases in crates/cameleer-protocols/tests/tls.rs
  tests/fhir_ops.rs   <- crates/cameleer-soap/tests/fhir_ops.rs
  tests/fhir_*.rs     <- the FHIR half of crates/cameleer-soap/tests/jaxb_fhir.rs (the JAXB half stays)
  tests/dicom_*.rs    <- crates/cameleer-dicom/tests/*.rs (component, corpus, dataformat, deid,
                                                            dicomweb, dimse, live_dicom)
```

Dependencies of `cameleer-health`: `cameleer-core`, `cameleer-tls`, `cameleer-http`; `cameleer-xml`
optional under `fhir`. **Not** `cameleer-protocols`, `cameleer-soap` or `cameleer-ai`. Three helper
moves make that possible:

| helper used by the moved code | today | moves to | why |
|---|---|---|---|
| `TcpServer`, `connect_stream`, `split_host_port`, `io_error`, `take_ignored` (MLLP) | `cameleer-protocols/src/common.rs` | `cameleer_tls::net` (next to `bind_listener`, `shutdown_socket`); `cameleer-protocols` imports them from there | they are generic socket helpers; STATUS already lists three duplicate `TcpServer::stop`s (protocols, amqp, kafka-mqtt) that can then share one |
| `transport::Client`, `support::now_utc` (FHIR) | `cameleer-soap` | `Client` to `cameleer_http::client` (it is an HTTP client with Camel options); `now_utc` to `cameleer_core::util_time` | FHIR is REST, not SOAP; it was in `cameleer-soap` only for the XML stack |
| `AuditEntry`, the hash chain, `sha256_hex` (de-identification audit) | `cameleer-ai` | `cameleer_core::audit` (the record and chain); SHA-256 from `cameleer_core::language::support::sha256`, which already exists [verified]; `cameleer-ai` re-uses both (there are also separate `sha256`/`sha256_hex` copies in `cameleer-ai`, `cameleer-cloud`, `cameleer-sql` and `cameleer-attest`, a candidate for the same "one home" clean-up) | every regulated pack needs audit; the AI registry stays the writer for tool calls |

Then remove from the old crates, with no shims: `pub mod hl7; pub mod mllp;` and their registration
from `cameleer-protocols`; `pub mod fhir;`, `fhir::register_kinds()` and the `fhir`, `fhirJson`,
`fhirXml` registrations from `cameleer-soap`; the whole `cameleer-dicom` crate (it never shipped).
Update the crate descriptions and lib docs of `cameleer-protocols` and `cameleer-soap`.

**What stays in core and why.** The `syslog` component and data format stay in `cameleer-protocols`
(general), even though ATNA uses syslog TLS. The health pack sends ATNA audit messages through it.
`netty` and `platform-http` stay. `validator`, `xslt` and JSON Schema stay in their crates; FHIR
validation uses them.

### 8.3 The registration hook

Add `crates/cameleer-core/src/pack.rs`:

```rust
/// Static description of a pack: what it adds, for `cameleer version`, validation,
/// missing-component messages and /q/info.
pub struct PackInfo {
    pub name: &'static str,             // "health"
    pub version: &'static str,          // env!("CARGO_PKG_VERSION")
    pub core_api: &'static str,         // the cameleer-core version it was built against
    pub description: &'static str,
    pub components: &'static [&'static str],   // "mllp", "fhir", "dicom", "dicomweb"
    pub data_formats: &'static [&'static str], // "hl7", "fhirJson", "fhirXml", "dicom", "dicomJson", "dicomDeidentify"
    pub languages: &'static [&'static str],    // "hl7terser", "dicom"
    pub simple_prefixes: &'static [&'static str], // "dicom."
}

pub trait Pack: Send + Sync {
    fn info(&self) -> &'static PackInfo;
    /// Register components, data formats, languages and Simple functions on the context.
    fn register(&self, ctx: &CamelContext);
}

impl CamelContext {
    pub fn add_pack(&self, pack: &dyn Pack);   // calls register, records info
    pub fn packs(&self) -> Vec<&'static PackInfo>;
}
```

In `cameleer-main`:

- `Main::add_pack(&mut self, pack: &'static dyn Pack)` for embedders (`Main::new().add_pack(&cameleer_health::HEALTH)`),
  and in `new_main_inner`, after the families:
  `#[cfg(feature = "health")] main.add_pack(&cameleer_health::HEALTH);`
- `features.rs`: keep the family tables for the families, and add
  `pub const PACKS: &[(&str, &[&str] /*components*/, &[&str] /*languages*/)]` for packs that are
  **not** compiled in, so the missing-component message can still say "`dicom` is in the health
  pack: install `cameleer-health` or build with `--features health`". (A pack that is not compiled in
  cannot describe itself, so this one small table stays; it holds only names.)
- `cameleer-yaml/src/dsl.rs` `LANGUAGES` and `validate.rs`: accept a language when it is in the static
  list **or** in `ctx.packs()`; remove `hl7terser` and `dicom` from the static list.
- `cameleer version` and `/q/info` print the packs with versions (`health 0.6.0 (core 0.6.0)`).
- `cameleer schema` includes pack components when the pack is compiled in.

No linker-section or `inventory`-style auto-registration: it is not available without third-party
crates and is fragile; one explicit line per pack is enough.

### 8.4 How users get it

| user | command |
|---|---|
| general integration | `cargo install cameleer-main`, image `cameleering/cameleer:latest` (no health code) |
| healthcare | `cargo install cameleer-main --features health`, image `cameleering/cameleer:health`, archive `cameleer-health-<version>-<target>` |
| a small healthcare edge build | `cargo build --profile dist -p cameleer-main --no-default-features --features "health,http2"` |
| a Rust embedder | `cameleer-core` + `cameleer-health` (crate features `dicom` only, say) and `ctx.add_pack(&cameleer_health::HEALTH)` |

`Dockerfile`: the `FEATURES` build argument already exists; `container.yml` gains a matrix entry
`FEATURES=full,health` tagged `health` and `<version>-health`. `docker/README.dockerhub.md` lists the
tags. A `--features health` build without `full` still has core, YAML, HTTP and TLS, which is what an
MLLP-to-FHIR route needs.

### 8.5 Versioning and compatibility

- `cameleer-health` has its **own `version` field** (not `version.workspace = true`), starting at
  `0.6.0` with the core release that introduces packs, and depends on `cameleer-core = "0.6"` etc.
- A health-only fix ships as `cameleer-health 0.6.1` plus a rebuilt `cameleer-health` distribution;
  the general distribution is not re-released. `scripts/publish-crates.sh` already skips versions
  that exist, so it publishes only the bumped crate (and `cameleer-main` if its pin changes).
- Core minor bumps (0.7) require a pack release that declares the new range; CI catches drift
  (section 8.7).
- `PackInfo.version` also feeds the DICOM Implementation Version Name (for example
  `CAMELEER_H_060`, at most 16 characters) so the wire identifies the pack release [judgment; check
  the PS3.7 length rule].
- Every pack release has its own `CHANGELOG` section, under `crates/cameleer-health/CHANGELOG.md`,
  so a validator can read only what changed in the medical code.

### 8.6 Per-release artifacts

For each `cameleer-health` release:

1. `docs/packs/health/conformance/dicom-conformance-statement-<version>.md` (PS3.2 Annex N form, SOP
   class tables generated from the dictionary).
2. CycloneDX SBOM for the crate and for the `cameleer-health` distribution, generated from
   `cargo metadata`.
3. cosign signatures on the image (already) and on the archives (`sign-blob`), plus SHA-256 sums.
4. A test report: hermetic counts plus the health tier results (below), stored with the release.
5. Later: IHE Integration Statement once ATNA and SWF are built.

### 8.7 CI and test tiers

| tier | command | runs |
|---|---|---|
| hermetic | `cargo test --workspace` (includes `cameleer-health` tests) | every commit |
| pack gate | `cargo test -p cameleer-main --no-default-features --features health` and `--no-default-features` (checks that `mllp` names the health pack in its error) | every commit |
| no pack in `full` | a test in `crates/cameleer-main/tests/features.rs`: `full` must not enable `health`; `features::PACKS` names every component of `cameleer_health::HEALTH` | every commit |
| health interop | `scripts/dicom-interop.sh` (dcmtk, Orthanc and the rest from the branch) plus HL7 and FHIR tools, gated by `CAMEL_HEALTH_INTEROP=1` | before a pack release |
| distribution build | Docker build with `FEATURES=full,health`, smoke run of `examples/medical/*` and `examples/edge/hl7-mllp-to-http.camel.yaml` | tags |

### 8.8 Docs and site

- Move `docs/components/{mllp,fhir,dicom,dicomweb}.md`, `docs/dataformats/{hl7,fhir,dicom,dicomJson,dicomDeidentify}.md`,
  `docs/languages/{hl7terser,dicom}.md` and `docs/guides/dicom.md` under `docs/packs/health/`,
  each with a header line `**Pack:** health (feature \`health\`)` beside the existing
  `**Cargo feature:**` line; extend `reference_pages_name_their_cargo_feature` to accept it.
- `docs/packs/index.md`: what a pack is, the criteria from section 5, the list of packs.
- `docs/FEATURES.md`: a "pack" column; `docs/guides/build-features.md`: a packs section.
- Site: `/packs/health/` as the landing page for "Cameleer for Healthcare" (HL7 v2, FHIR, DICOM,
  de-identification, the conformance statement, the image tag), linked from the HL7 use case and the
  trust hub. Update `examples/medical/` and `examples/edge/hl7-mllp-to-http.camel.yaml` READMEs to
  say they need the health pack.
- Update the docs tests that grep these pages (`docs_guides.rs`, `docs_languages_formats.rs`,
  `docs_messaging.rs`, `docs_web.rs`) and `scripts/build-docs.py`.

### 8.9 Order of work

1. Core: `cameleer_core::pack`, `cameleer_core::audit`, `util_time::now_utc`; `Main::add_pack`;
   pack-aware language list in YAML and `validate`; `cameleer version` shows packs.
2. Helper moves: socket helpers to `cameleer_tls::net`, FHIR's HTTP client to `cameleer_http`.
3. Create `cameleer-health`; move HL7/MLLP, FHIR, then DICOM from the `dicom` branch (rebase the
   branch onto step 2, then `git mv`), with tests.
4. `cameleer-main`: `health` feature, `PACKS` table, remove `hl7terser`/`fhir`/`mllp` from the family
   tables, drop `cameleer-dicom` from `full`.
5. Docs, site, Dockerfile matrix, SBOM script, release notes for 0.6.0 stating plainly that `mllp`,
   `hl7`, `hl7terser`, `fhir`, `fhirJson`, `fhirXml` now need the health pack.
6. Full hermetic suite on Windows and Linux, the health tier, then merge.

### 8.10 Risks

| risk | mitigation |
|---|---|
| Users of `cameleering/cameleer:latest` with an MLLP or FHIR route break on upgrade | Owner says there are none; the startup error names the pack and image; release notes say so |
| DICOM's size and the size argument are not measured | Measure `--no-default-features --features health` against `core`, and `full` against `full,health`, before writing size claims [unverified today] |
| Pack and core drift apart | The pack is in the workspace, so `cargo test --workspace` builds both on every commit |
| Helper moves widen the change | Do them first as a separate commit with no behaviour change; the existing protocols, amqp and soap tests cover them |
| "Pack" becomes a second dumping ground | Apply the criteria in section 5 to every proposed pack; one owner per pack in `CODEOWNERS` |
| Healthcare buyers ask about from-scratch TLS and parsers | STATUS item 5 (audit and fuzzing) becomes a pack release prerequisite: fuzz the DICOM, HL7 and FHIR parsers first |
| Regulatory claims | Say "supports building conformant systems"; never "FDA-cleared" or "HIPAA-certified"; classification questions need counsel |

## 9. Sources

- Apache Camel MLLP component: https://camel.apache.org/components/next/mllp-component.html
- camel-kamelets: https://github.com/apache/camel-kamelets
- Red Hat build of Apache Camel for Quarkus, extensions overview: https://docs.redhat.com/en/documentation/red_hat_build_of_apache_camel/4.8/html/red_hat_build_of_apache_camel_for_quarkus_reference/camel-quarkus-extensions-overview
- Spring Boot, creating your own auto-configuration: https://docs.spring.io/spring-boot/docs/2.0.0.M6/reference/html/boot-features-developing-auto-configuration.html
- OpenTelemetry Collector releases: https://github.com/open-telemetry/opentelemetry-collector-releases
- OpenTelemetry Collector Builder: https://opentelemetry.io/docs/collector/extend/ocb/
- OpenTelemetry contrib, new components: https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/docs/new-components.md
- OpenTelemetry contrib, CONTRIBUTING: https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/CONTRIBUTING.md
- Vector custom builds: https://vector.dev/highlights/2020-02-28-custom-vector-builds/
- Envoy threat model: https://www.envoyproxy.io/docs/envoy/latest/intro/arch_overview/security/threat_model
- proxy-wasm, WebAssembly in Envoy: https://www.github.com/proxy-wasm/spec/blob/main/docs/WebAssembly-in-Envoy.md
- Fluent Bit build options and WASM filters: https://docs.fluentbit.io/manual/installation/downloads/source/build-and-install, https://docs.fluentbit.io/manual/development/wasm-filter-plugins
- Kong external plugins: https://docs.jp.konghq.com/gateway/2.8.x/reference/external-plugins/
- NiFi NARs: https://docs.cloudera.com/cfm/2.1.1/nifi-dev-guide/topics/nifi-nars.html
- NextGen on Mirth Connect 4.6: https://www.nextgen.com/blog/industry-news/a-new-era-for-mirth-connect-by-nextgen-healthcare
- Open Integration Engine: https://github.com/openintegrationengine/engine
- Rhapsody brochure 2026: https://rhapsody.health/wp-content/uploads/2023/03/rg_Product_brochure_Rhapsody-Integration-Engine_2026.pdf
- Bevy plugins: https://bevy.org/learn/quick-start/getting-started/plugins/
- smithy-rs RFC 15, features: https://smithy-lang.github.io/smithy-rs/design/rfcs/rfc0015_using_features_responsibly.html
- stabby: https://docs.rs/crate/stabby/0.1.5 ; abi_stable: https://docs.rs/abi_stable
- Sigstore, signing other types: https://docs.sigstore.dev/cosign/signing/other_types/
- Chainguard, OCI 1.1 in cosign: https://www.chainguard.dev/de-DE/unchained/building-towards-oci-v1-1-support-in-cosign
- DICOM PS3.2: https://dicom.nema.org/medical/dicom/current/output/chtml/part02/chapter_6.html
- Hogan Lovells on FDA cyber device select updates: https://www.hlc.com/en/publications/fda-finalizes-cyber-device-select-updates-guidance
- Kubernetes, completing the cloud provider migration: https://kubernetes.io/blog/2024/05/20/completing-cloud-provider-migration/
- CNCF on the in-tree removal: https://cncf.io/blog/2024/09/27/why-kubernetes-is-removing-in-tree-cloud-provider-integration-support-in-v1-31-and-how-it-can-affect-you
- GitLab tiering guidance: https://handbook.gitlab.com/handbook/product/product-processes/tiering-guidance-for-features/
- CycloneDX: https://cyclonedx.org/ ; SPDX: https://spdx.dev/
- Internal: `docs/design/medical/dicom-and-healthcare.md`, `docs/design/family-crates-std-only.md`,
  `docs/design/github-organization.md`

# Attestation for Cameleer: research and design

_Status: design research, 2026-11-09. Internal (like the rest of `docs/design/`). No code, no
cloud testing yet. Facts were checked against the sources listed at the end on 2026-11-09. Statements
from general knowledge that this research did not re-check are marked "(unverified)"._

## 1. Goal and scope

When Cameleer runs inside a hardware TEE (an Intel TDX or AMD SEV-SNP confidential VM, or an AWS
Nitro enclave), an optional `attest` feature should let it do three things:

- **(a) Prove what it is.** An MCP client, an HTTP caller or an operator can get evidence that
  names the exact binary, configuration and routes. The evidence is bound to a fresh nonce and to the
  TLS key of the connection, so the caller knows it is talking to that TEE.
- **(b) Receive secrets only after attestation.** KMS or vault secret release is gated on evidence.
- **(c) Verify remote MCP servers.** As a gateway policy, Cameleer calls tools only on servers that
  attest.

In RATS terms (RFC 9334), Cameleer is the **Attester** for (a) and (b). It is the **Relying Party**,
and optionally the **Verifier**, for (c). The std-only rule applies throughout: every interface below
was judged on what it takes to reach it from Rust `std` alone.

## 2. Where a Linux guest gets evidence today

### 2.1 Linux configfs-tsm (`/sys/kernel/config/tsm/report`)

**Interface.** This is the kernel's vendor-neutral report ABI:

1. `mkdir /sys/kernel/config/tsm/report/$name`.
2. Write up to **64 bytes** to `inblob`.
3. Read `outblob`, plus the optional `auxblob` and `manifestblob`.
4. Read `provider` (`tdx_guest` or `sev_guest`) and check that `generation` did not change. The
   counter goes up on every write, so a changed value means another writer interfered.
5. `rmdir`.

For SEV-SNP there is also `privlevel` (VMPL, at most 3). `service_provider` (only `svsm` today),
`service_guid` and `service_manifest_version` cover SVSM service reports.

**Kernel versions.** The core attributes are `KernelVersion: v6.7`. `manifestblob` and the
`service_*` attributes are v6.10.

**Evidence and what it measures.**

- **TDX:** `outblob` is a signed TD Quote (DCAP, ECDSA). It covers MRTD (the initial TD image),
  RTMR0-3 (firmware, kernel and command line, and an OS or app register), the TCB SVNs and the
  64-byte REPORTDATA. (unverified: field layout, from Intel's quote format.)
- **SEV-SNP:** `outblob` is the 1184-byte (0x4A0) attestation report: MEASUREMENT (the launch
  digest), HOST_DATA, the policy, the TCB versions and the 64-byte REPORT_DATA. `auxblob` carries the
  GHCB cert table, which can hold the VCEK.

**Std-only difficulty: trivial.** It is plain `std::fs` (`create_dir`, `write`, `read`,
`remove_dir`). There is no ioctl and no FFI. It is also the best place to start, because one code path
covers TDX and SNP on any recent kernel.

**Related since 6.16: the TSM measurement registers.** These are exposed in sysfs as
`/sys/devices/virtual/misc/tdx_guest/measurements/rtmr[0-3]:sha384`. Reading an attribute returns
the register; writing to a writable one extends it. This is again plain file I/O.

### 2.2 `/dev/tdx_guest` and `/dev/sev-guest`

**`/dev/tdx_guest`** offers one ioctl, `TDX_CMD_GET_REPORT0`, which returns a **TDREPORT**. A
TDREPORT is MAC'd and verifiable only on the same platform. Turning it into a quote needs a quoting
service, so on its own it is not remote evidence. **`/dev/sev-guest`** provides
`SNP_GET_REPORT`/`SNP_GET_EXT_REPORT` ioctls (unverified: ioctl names, from the kernel `sev-guest`
docs).

Both require `ioctl(2)`, which `std` does not wrap. Calling it means an `extern "C"` declaration
against the libc that `std` already links. That is FFI but no crate, and it is small. Still, now that
configfs-tsm exists there is little reason to use these devices.

### 2.3 dstack guest agent (`/var/run/dstack.sock`)

dstack is the open-source layer under Phala Cloud. Its docs list GCP Confidential VMs (Intel TDX
plus vTPM) and AWS Nitro Enclaves as "Available" targets of dstack-cloud.

**Interface.** HTTP over a unix socket. Methods are served at `/v1/<Method>` (current), `/v0/...`
(frozen at 0.5.11) and unversioned (same as v0).

| Method | Request | Response / notes |
|---|---|---|
| `GetQuote` (v0) | `report_data`, raw, ≤ 64 bytes, zero-padded on the right; longer is an error | `quote`, `event_log`, `report_data`, `vm_config`. TDX only. On GCP it returns only the TDX half, so use `Attest`. |
| `Attest` | same `report_data` | hex `attestation`, accepted by the verifier's `/verify`. v1 can add boot-time GPU evidence. |
| `GetKey` | `path`, `purpose`, `algorithm` (`secp256k1` default, `ed25519`) | `key`, `signature_chain`. Details below. |
| `Info` | none | `app_id`, `instance_id`, `app_cert`, `tcb_info` (MRTD, RTMR0-3, event log, verbatim `app_compose`), `compose_hash`, `os_image_hash`, `key_provider_info`, `vm_config`, `cloud_vendor`... |
| `GetTlsKey` (v0) / `IssueCert` (v1) | `subject`, `alt_names`, `usage_ra_tls`, ... | key plus certificate chain (dstack's own RA-TLS) |
| `Sign`, `Version`, `AttestGpu` | | |

`GetKey` details:

- The KDF is HKDF-SHA256 with salt `"RATLS"`, the app root key as input keying material, `info =
  path` and 32 bytes out. `purpose` and `algorithm` do not enter the KDF.
- The two-link signature chain is secp256k1, `r||s||v`, over keccak256 preimages. Link 0 is signed by
  the app root key. Link 1 is the KMS root key's statement over `app_id` and the app root public key.

**What the quote binds.**

- **RTMR3** holds the application identity events: `app-id`, `compose-hash`, `instance-id` and
  `key-provider` (the docs also list `system-preparing` and `boot-mr-done`, so the set varies by
  version). They are SHA-384 chained and replayed from the event log.
- `compose_hash` is the SHA-256 of the exact `app_compose` bytes.
- MRTD and RTMR0-2 identify the dstack OS image.
- **`EmitEvent` was removed in 0.6.0:** "runtime RTMR3 events are system-owned and cannot be
  extended by apps". Applications bind their own data through `report_data`.

**Std-only difficulty: easy.** `std::os::unix::net::UnixStream` plus Cameleer's existing HTTP/1.1
client framing and JSON. It works only under `cfg(unix)`, which is fine for a guest.

### 2.4 AWS Nitro Enclaves (`/dev/nsm`)

**Interface.** The NSM device is driven through `aws-nitro-enclaves-nsm-api`. The protocol is CBOR
requests and responses passed through an ioctl on `/dev/nsm` (unverified: transport detail, the repo
landing page does not describe it). The requests cover attestation (with optional `user_data`, `nonce`
and `public_key`), PCR describe, extend and lock, and random bytes.

**Evidence.** A CBOR document, signed as COSE_Sign1 with tag 18 and protected header `{1: -35}`
(ES384). Fields: `module_id`, `timestamp`, `digest` = `"SHA384"`, `pcrs`, `certificate`, `cabundle`
(ordered root first), and optional `public_key`, `user_data` and `nonce`. The AWS doc's validation
section caps `user_data` and `nonce` at 512 bytes, while its CDDL says 1024. The document chains to the
AWS Nitro root `CN=aws.nitro-enclaves` with a published SHA-256 fingerprint. AWS says CRL checking
must be disabled when validating.

**What it measures.** The PCRs. PCR0 is the image (EIF), PCR1 the kernel and boot, PCR2 the
application, PCR3 the IAM role, PCR4 the parent instance and PCR8 the signing certificate
(unverified: the PCR meanings come from the Nitro user guide, which was not re-fetched).

**KMS.** `Decrypt`, `GenerateDataKey`, `GenerateDataKeyPair`, `DeriveSharedSecret` and
`GenerateRandom` accept the attestation document as a Recipient. KMS then returns the result
encrypted to the document's `public_key`, never in plaintext. Key policies can use condition keys
(`kms:RecipientAttestation:ImageSha384`, `...:PCR<n>`). NitroTPM on EC2 is supported the same way.

**Std-only difficulty: moderate.** It needs the ioctl through FFI, a CBOR encoder for the request,
and vsock (`AF_VSOCK`, which is not in `std`) to reach anything outside the enclave. Vsock is the
larger cost: the enclave has no network, so every Cameleer endpoint would need a vsock transport.

### 2.5 Azure confidential VMs (vTPM, MAA, SKR)

**Interface and evidence.**

- On Azure CVMs the hardware report sits behind a paravisor and the vTPM. NV index `0x01400001`
  holds an Azure-defined "HCL report" of 2600 bytes, with the SNP or TDX report embedded.
- NV index `0x01400002` takes 64 bytes of user data. That data is reflected in the HCL report and
  mixed into a hash that becomes the hardware `report_data`. The same runtime data carries the vTPM
  attestation key (AKpub).
- So the chain is hardware report → HCL runtime data → AK → TPM quote over PCRs with a nonce. The
  hardware report's REPORT_DATA is **not** directly caller-chosen.

**Verification and release.** Microsoft Azure Attestation (MAA) turns this into a JWT. MAA tokens
last 8 hours, with claims such as `x-ms-isolation-tee.x-ms-attestation-type = sevsnpvm` and
`x-ms-compliance-status = azure-compliant-cvm`. **Secure Key Release** (Key Vault Premium and Managed
HSM) releases an *exportable HSM key* when an MAA token satisfies the key's release policy. The SKR
FAQ says policies cannot be attached to certificates or secrets "at this time", and that a non-MAA
authority's token is accepted only if it chains to a CA Key Vault trusts (DigiCert).

**Std-only difficulty: hard-ish.** It needs TPM 2.0 command marshalling over `/dev/tpmrm0`
(TPM2_NV_Write, TPM2_NV_Read and TPM2_Quote with the persistent AK `0x81000003`), plus the HCL
layout. That is all file I/O and byte layouts, but there is a lot of it. The low-level layout comes
from third-party code (`az-cvm-vtpm` and Constellation), not Microsoft reference docs.

### 2.6 Google Cloud

**Confidential Space.** The workload POSTs to `http://localhost/v1/token` on the unix socket
`/run/container_launcher/teeserver.sock`. ITA tokens use `/v1/intel/token`, TDX only. The request
body:

- `audience`: at most 512 bytes; it cannot be `https://sts.google.com`.
- `token_type`: `OIDC` or `PKI`, or the AWS principal-tags type, whose spelling the docs give
  inconsistently.
- `nonces`: at most six, each 10-74 bytes.

The result is a JWT from `https://confidentialcomputing.googleapis.com` with `eat_nonce`,
`submods.container.image_digest`, `hwmodel`, `swname`, `dbgstat` and similar claims. Requests are
limited to 5 per second per project per region.

**Std-only difficulty: easy** (unix socket plus HTTP plus JSON). The catch is that the *launcher*,
not Cameleer, collects the evidence; Cameleer only sees the token. So binding a TLS key means putting
its hash into a nonce.

**Plain TDX/SNP CVMs.** These expose configfs-tsm (or the devices) plus a Google vTPM. Google Cloud
Attestation verifies a TDX quote with CCEL and RTMRs (`VerifyAttestationRequest`, `TdCcel`) and
returns an EAT-style token. A TDX CVM can also run dstack.

### 2.7 TPM 2.0 quotes

A quote is `TPMS_ATTEST` signed by an AK over selected PCRs, with `qualifyingData` as the nonce. The
AK is certified by the cloud's vTPM CA, and a TCG or IMA event log explains the PCRs (unverified:
general TPM knowledge). Quotes matter on Azure (the AK is bound to the CVM report), on GCP vTPM and on
NitroTPM. Std-only, a TPM2 command subset is a few hundred lines of marshalling over
`/dev/tpmrm0`.

### 2.8 Summary

| Source | Interface | Evidence | std-only | Cameleer can already... |
|---|---|---|---|---|
| configfs-tsm | files | TDX quote / SNP report | trivial | read it; verify the ECDSA P-256/P-384 signatures |
| dstack socket | HTTP on unix socket | TDX quote + event log + compose | easy | talk HTTP and JSON; SHA-384 replay |
| Confidential Space | HTTP on unix socket | OIDC/PKI JWT (EAT claims) | easy | verify the JWT (MCP auth has JWKS RS/PS/ES) |
| Nitro NSM | ioctl + CBOR | COSE_Sign1 document | moderate (FFI, vsock) | decode CBOR (`cameleer-dataformats`), ES384, X.509 |
| Azure vTPM | TPM2 over `/dev/tpmrm0` | HCL report + TPM quote → MAA JWT | hard-ish | verify the MAA JWT |
| `/dev/*-guest` | ioctl | report | moderate (FFI) | as configfs |

What `cameleer-tls` already has: ECDSA P-256 and P-384 sign and verify; RSA PKCS#1 v1.5 and
**RSASSA-PSS** verify; X.509 parsing and chain validation **with CRLs**; OCSP; SHA-256/384/512;
HKDF; AES-GCM; Ed25519 and X25519; and certificate generation (`certgen`). `cameleer-dataformats`
has a CBOR codec, but its decoder drops tags (tag 18 would need handling, although COSE needs only the
raw bytes of the protected header and payload, which are byte strings anyway). It has **no
secp256k1 and no keccak**, which only matters for verifying dstack `GetKey` signature chains. Per
the simoom plan, this crypto is moving to the shared `saltire`/`saltire-pki` family, and the new
attestation parsers belong there too.

## 3. Verification

### 3.1 What each format requires

**Intel TDX (DCAP).**

1. Parse the v4/v5 quote: header, TD report body, signature data with the ECDSA P-256 quote
   signature, the attestation key, the QE report and its signature, and the certification data with
   the PCK chain.
2. Check:
   - the quote signature with the attestation key;
   - that the QE report's REPORTDATA binds the attestation key;
   - the QE report signature with the PCK key;
   - the PCK chain (PCK → Platform/Processor CA → Intel SGX Root CA, all ECDSA P-256) plus the PCK
     and root CRLs.
3. Fetch the collateral from Intel PCS v4 (or a PCCS cache): TCB Info and QE Identity, JSON signed
   by the TCB Signing key, with issuer chains in HTTP headers such as `TCB-Info-Issuer-Chain`.
4. Match the QE report against the **QE Identity**: MRSIGNER, ISVPRODID and the ISVSVN level.
5. Match the PCK certificate's SGX extensions (FMSPC, CPUSVN, PCESVN) and the TD's TEE TCB SVN
   against **TCB Info**, giving a status of UpToDate, SWHardeningNeeded, OutOfDate and so on, with
   advisory IDs.
6. Apply policy: debug off, and the MRTD, RTMR and REPORTDATA values.

How subtle this is: **CVE-2026-22696** was the Rust `dcap-qvl` verifier missing the QE identity
check, fixed in 0.3.9. Phala told dstack users to upgrade images to v0.5.6.

**AMD SEV-SNP.**

1. Check the ECDSA P-384 signature over the report body (the signed part, hashed with SHA-384).
2. Fetch the **VCEK** from AMD KDS by `chip_id` and the reported TCB. On shared-tenancy AWS, use the
   **VLEK**.
3. Check the chain ARK (self-signed) → ASK → VCEK. The ARK and ASK are RSA-4096 with RSASSA-PSS
   signatures (unverified, from AMD's KDS spec). Check the KDS CRL.
4. Compare the VCEK TCB extensions with the report's reported TCB.
5. Apply policy: MEASUREMENT, HOST_DATA, policy bits (debug), the minimum TCB and REPORT_DATA.

**AWS Nitro.**

1. Decode CBOR, then COSE_Sign1.
2. Rebuild the `Sig_structure` `["Signature1", protected, h'', payload]` and verify it with ES384
   against `certificate`.
3. Validate the chain from `cabundle` to the pinned AWS root (by fingerprint), with validity
   periods and no CRL.
4. Apply policy: PCRs, nonce, `user_data` and `public_key`.

### 3.2 Std-only implementation cost

| Verifier | New code (estimate) | Hard parts |
|---|---|---|
| Nitro | ~500 lines | CBOR tag 18 and the canonical `Sig_structure` encoding; everything else exists |
| SEV-SNP | ~1,200 lines | report layout, KDS fetch and cache, VCEK extension OIDs; RSA-PSS and P-384 exist |
| TDX DCAP | ~2,500-3,500 lines | quote layout, PCK SGX extension ASN.1, TCB level matching rules, collateral caching, CRLs |
| dstack extras | ~300 lines (+ secp256k1/keccak ~800 if `GetKey` chains are verified) | event log replay, compose hash |
| JWT tokens (ITA, MAA, Google) | ~100 lines on top of `cameleer-ai` JWT/JWKS code | issuer pinning, claim policy, nonce |

The crypto is there. The cost is in format parsing and, for TDX, in Intel's TCB evaluation rules,
which change over time. That makes it maintenance, not a one-off job.

### 3.3 Verify locally or delegate?

| Option | Pros | Cons |
|---|---|---|
| **Local** | no third party, no outbound calls per check, works air-gapped with cached collateral | TCB-rule maintenance, collateral fetching (Intel PCS, AMD KDS), a security-critical parser |
| **Intel Trust Authority** | TDX and SGX (and vTPM) → JWT; the ITA Go client puts a hash of nonce and held data in REPORTDATA (the Go client docs say SHA-512, the Java docs say SHA-256, so check before use) | account needed; vendor-specific |
| **Microsoft Azure Attestation** | native for Azure CVMs and SKR | Azure-centric |
| **Google Cloud Attestation** | EAT tokens for TDX/SNP CVMs and Confidential Space | GCP-centric |
| **dstack-verifier** | open source (`dstacktee/dstack-verifier`, uses `dcap-qvl`), checks the quote, RTMR3 replay, TCB status, debug | self-hosted; does not verify the KMS itself |

**Recommendation.** Make the verifier pluggable and offer local verification first where it is cheap.

- **Local first for Nitro and SEV-SNP.** The formats are small and the crypto already exists.
- **Delegated first for TDX.** Use ITA, Google, MAA or a self-hosted dstack-verifier and verify the
  resulting JWT locally, since that code already exists. Add local DCAP later behind the same trait.
- **Result format.** Whatever path is taken, express the result internally as an EAT-shaped claim
  set (RFC 9711: `eat_nonce`, `measurements`, `submods`, `eat_profile`) so policy code is
  verifier-independent.
- **Standards.** RATS architecture: RFC 9334 (Informational, Jan 2023). EAT: RFC 9711 (Standards
  Track, Apr 2025). CMW: RFC 9999 (Proposed Standard, 2026; it defines the X.509 extension
  `id-pe-cmw`, OID 1.3.6.1.5.5.7.1.35). Supporting standards: CBOR RFC 8949, COSE RFC 9052
  (unverified number), Exported Authenticators RFC 9261.

## 4. Binding evidence to the transport

### 4.1 Options

1. **RA-TLS.** Evidence goes into an X.509 extension of a certificate whose key hash is in
   REPORTDATA. Gramine (from v1.8) and RATS-TLS implement "Interoperable RA-TLS":
   - the evidence is a tagged CBOR array, `60000([quote, claims-buffer])`, with IANA CBOR tags
     60000-60002;
   - the claims buffer is a CBOR map with `pubkey-hash` and optionally `nonce`;
   - the quote's user data is SHA-256(claims-buffer).

   The project's own README warns the scheme "is vulnerable to replay and diversion attacks" when
   there is no nonce, and the repository is archived. dstack's `GetTlsKey`/`IssueCert` with
   `usage_ra_tls` is another RA-TLS variant. RFC 9999 now gives a standard extension (`id-pe-cmw`) to
   carry such evidence.
2. **IETF SEAT working group.** As of 2026-11-02 the only WG document is
   `draft-ietf-seat-use-cases-02`. The protocol drafts are still individual submissions:
   - `draft-fossati-seat-early-attestation-09` (2026-11-01): a `remoteAttestation` TLS 1.3
     extension, evidence in the Certificate message, a binder from the handshake transcript, intended
     Standards Track, on the IETF-127 agenda;
   - `draft-fossati-seat-expat-05`: attestation through RFC 9261 exported authenticators after the
     handshake;
   - `draft-reddy-seat-expat-transport-02`: an HTTP transport for those;
   - `draft-usama-seat-intra-vs-post-05`, `draft-zhang-seat-selection-02`,
     `draft-many-seat-architecture-01`, and others.

   None is stable enough to build on.
3. **report_data binding at the application layer.** The client sends a nonce. The server returns
   evidence whose `report_data = H(domain || SPKI-hash of its TLS cert || nonce || manifest digest)`.
   The client checks that the SPKI hash matches the certificate it actually saw on this connection.

### 4.2 Recommendation

Use **option 3 as the primary mechanism**, plus an **optional RA-TLS certificate** using RFC 9999's
CMW extension.

- **Why option 3.** It works with every TLS stack and every HTTP and MCP client, gives fresh nonces,
  and needs nothing new in the TLS handshake. It also binds the same thing that RA-TLS binds (the
  TLS key), provided the private key is generated inside the TEE at start-up and never written out.
  The certificate can still come from a public CA (ACME) for browser trust, since the binding is to
  the SPKI, not to the issuer.
- **The RA-TLS certificate.** At start-up, Cameleer generates its key and a self-signed certificate
  (using `certgen`) carrying `id-pe-cmw` with boot-time evidence (nonce = the hash of the
  certificate's notBefore and a random value). This serves RA-TLS-aware clients and dstack-style
  gateways. It is cheap because `certgen` already writes extensions.
- **SEAT.** Track `early-attestation` and `expat` and implement whichever the WG adopts later. The
  TLS stack is Cameleer's own, so adding a TLS extension then is feasible.

Concrete `report_data` (64 bytes) for all TEEs:

```
report_data = SHA-512( "cameleer-attest-v1" || 0x00
                     || manifest_digest (SHA-384, 48 bytes)
                     || tls_spki_sha256 (32 bytes, or 32 zero bytes when no TLS)
                     || nonce_len (u16 BE) || nonce (16..64 bytes) )
```

SHA-512 fills the field exactly. The domain string stops a Cameleer quote from being replayed as
another protocol's quote. For Nitro, the nonce goes in `nonce`, the manifest digest in `user_data`
and the TLS key in `public_key`. For Confidential Space, `nonces = [nonce, hex(report_data)[..74]]`
to fit the 74-byte limit.

## 5. MCP

### 5.1 What exists

There is **no accepted MCP spec feature or SEP for server attestation.** The relevant items:

- **The MCP Security Interest Group charter** (2026-06-13, restructured 2026-08-26). It puts
  "Server identity, attestation, and admission" in scope, and lists **SEP-2809 "Attested Tool-Server
  Admission (ATSA)"** as Draft. ATSA is an offline-signed "clearance assertion" at a well-known URI,
  checked against a pinned trust root, with a host-side tool allow-list and hash-chained audit. It is
  **not** hardware attestation. The PR was moved to draft with `wg-support-needed` on 2026-11-06 and
  **closed on 2026-11-08**.
- **Issue #3354 "Verifiable tool results"** (2026-09-10). It attached ZK proofs or TEE attestations
  to `tools/call` results under `result._meta["io.github.ripple-node-lab/verifiable-tools"]` and
  advertised a capability under `extensions[...]`. It was closed on 2026-09-22, and the author was
  told to go through a Working Group first. One review comment noted that `structuredContent` was not
  covered by the output commitment.
- **Issue #1461 "Attested Client Registration and Proof-of-Possession"**: client-side, aimed at the
  authorization server.
- Maintainers now require new SEPs to come through a WG. For attestation, that means the Security IG
  (Discord `#security-ig`). If Cameleer wants to influence the standard, the path is a Security IG
  agenda item, not a direct SEP.

### 5.2 How Cameleer should expose it inside the spec

**For existing clients (no changes):**

- **A resource template** `attestation://cameleer/report{?nonce}` (MIME `application/json`),
  listed only when `camel.server.mcp-attestation=true`. Any client that can read resources can fetch
  and show it, and an operator can paste the JSON into a verifier.
- **An opt-in tool**, `cameleer_attestation(nonce?)`, for clients that only use tools. Its
  description must say plainly that a model *displaying* evidence is not *verification*.
- **The HTTP endpoint** `GET /q/attestation?nonce=<b64url>` on the shared server, next to
  `/q/health`. Curl, scripts and verifier services use this. Don't squat `/.well-known/attestation`:
  RFC 8615 suffixes are IANA-registered and none exists for this. The SEAT
  `expat-transport` draft may define an HTTP binding later.

**For verifying clients:**

- Put a **`_meta` entry** in the `server/discover` result (2026-07-28) and the `initialize` result
  (older revisions), using Cameleer's reverse-DNS prefix:

  ```json
  "_meta": { "ing.cameleer/attestation": {
      "version": 1, "platform": "tdx", "formats": ["tdx-quote", "dstack-attest", "cmw"],
      "manifestDigest": "sha384:…", "tlsSpkiSha256": "…",
      "endpoint": "/q/attestation", "resource": "attestation://cameleer/report{?nonce}" } }
  ```

- A client that sends `params._meta["ing.cameleer/attestation"] = {"nonce": "…"}` on
  `server/discover` (or `initialize`) gets the evidence inline in the result's `_meta`. That gives one
  round trip, binds to the session's TLS connection, and stays within the spec, since `_meta` is open
  for extensions under a vendor prefix.
- Cameleer's own MCP client (phase 3) uses this, so two Cameleer instances verify each other with no
  extra calls.

## 6. What to measure

Platform measurements (MRTD/RTMRs, MEASUREMENT, PCRs, compose hash, container digest) show **which
binary and OS image** booted. Evidence is only meaningful if the relying party can map those values to
something it trusts. That gives three requirements:

1. **Binary digest with reproducible builds.**
   - Publish the SHA-384 of every release binary and container layer, and make the `dist` build
     reproducible (pinned toolchain, `--remap-path-prefix`, `SOURCE_DATE_EPOCH`, no build-time
     timestamps). Then anyone can rebuild and match it. On dstack, the image digest is pinned in
     `app_compose` and therefore in `compose_hash`/RTMR3. On Nitro it is PCR0. On Confidential Space
     it is `image_digest`.
   - Cameleer also includes `sha384(/proc/self/exe)` in its manifest. That is self-reported and only
     a convenience, since the platform measurement is what counts.
2. **The Cameleer manifest.** A canonical JSON (sorted keys, no whitespace), hashed with SHA-384,
   containing:
   - the version, git commit, cargo features and the target triple;
   - each loaded route file (path and SHA-256 of its bytes as loaded) and each Rust-DSL route's
     definition digest;
   - the effective properties, **without secret values**. Record each key with either its literal
     value or, for values from `{{aws:…}}`, `{{azure:…}}`, `{{gcp:…}}`, `{{env:…}}` or keys matching
     the secret patterns (`password`, `secret`, `token`, `key`), the *placeholder text* and a marker.
     Never hash secret values: a hash of a low-entropy secret leaks it;
   - the MCP surface: exposed tools with their schema digests and envelope options (approval,
     authorization policy, rate limits) from `AiRegistry`, which already fingerprints tools for
     rug-pull protection; resources; prompts; `mcp-expose-routes` overrides;
   - the gateway policy (remote servers and their required measurements).
3. **An application event log.** Precedents:
   - TPM/TCG and IMA event logs;
   - dstack's RTMR3 events (system-owned since dstack 0.6.0);
   - the 6.16 kernel's writable RTMR attributes.

   Cameleer keeps an append-only, hash-chained **manifest log**: start-up, every route add or remove
   at runtime, every tool enable or disable (the `cameleer tool disable` kill switch), and every
   properties reload. `manifest_digest` in `report_data` is the head of that chain.

   Where a writable RTMR exists and the platform does not own it (raw TDX with kernel 6.16 or later,
   not dstack), Cameleer can optionally extend it with each log entry. Changes are then measured in
   hardware, not just reported. This is opt-in, because extending a register that the platform's
   verifier expects to own breaks the platform's verification.

## 7. Secret release

### 7.1 Patterns to follow

| Pattern | How | Gives |
|---|---|---|
| AWS KMS Recipient (Nitro, NitroTPM) | send attestation document with `Decrypt`/`GenerateDataKey`; key policy conditions on `ImageSha384`/`PCR<n>` | plaintext never leaves KMS unencrypted; only the enclave key can open it |
| Azure SKR | MAA token → Key Vault `release` of an exportable HSM key, policy on MAA claims | a key, not a secret (SKR does not attach to secrets) |
| GCP Confidential Space | OIDC attestation token → Workload Identity Federation with attribute conditions (e.g. image digest) → Secret Manager / Cloud KMS | ordinary IAM, gated on measurements (unverified detail: WIF attribute mapping syntax) |
| dstack KMS | `GetKey(path)` derives keys bound to the app identity; only the attested app (its `app_id`, and the compose its KMS policy allows) gets them | sealing keys without any cloud service |
| Confidential Containers Trustee (KBS) | evidence → attestation service → resource release by policy | vendor-neutral, self-hosted |

The common shape is to release a **key** after attestation, and to keep secrets encrypted under it
(envelope encryption). That fits all five patterns, including SKR, which only releases keys.

### 7.2 Mapping onto Cameleer

Cameleer resolves secrets through **properties functions**: `cameleer-cloud` registers `aws`,
`azure` and `gcp` with `PropertiesComponent::add_function`, giving `{{aws:name/field}}`,
`{{azure:…}}` and `{{gcp:…}}`, configured by `camel.vault.<provider>.*` with refresh support. There
is **no HashiCorp Vault component** in the repo today.

Proposed:

1. **A new `{{sealed:<base64>}}` function** (or `sealed:` values in a sealed properties file).
   - The value is AES-256-GCM ciphertext with the property key as additional data.
   - The data key comes from a pluggable **key release backend**, chosen by
     `camel.attestation.keyRelease=aws-kms|azure-skr|gcp-kms|dstack|trustee`.
   - A `cameleer seal` CLI command encrypts values for a given backend and key.
   - All the needed primitives are in `cameleer-tls`, except KMS calls, which already exist in
     `cameleer-cloud` (`aws2-kms` and the Azure and GCP clients). The new part is the attestation
     parameter: the KMS Recipient, the MAA token and the WIF token exchange.
2. **Attested credentials for the existing functions.** `camel.vault.gcp.attested=true` gets the
   Secret Manager access token through the Confidential Space token and WIF. Then `{{gcp:…}}` works
   unchanged, and IAM refuses it outside the measured workload. For AWS, Secrets Manager decrypts on
   the server side, so the Recipient doesn't apply. Use `{{sealed:…}}` with a KMS key whose policy
   requires the attestation conditions. For Azure, secrets can't be SKR-released, so use
   `{{sealed:…}}` with an SKR-released key.
3. **Ordering and failure.** With `camel.attestation.required=true`:
   - Main produces evidence before resolving any secret-bearing property, and refuses to start if no
     provider works;
   - released keys stay in memory, zeroized on drop where practical, and are never logged;
   - `cameleer validate` never resolves `sealed:`, and the manifest records placeholders only;
   - secret refresh re-attests, because tokens expire (MAA lasts 8 hours).

## 8. Recommended architecture

- **New crate `cameleer-attest`**, behind feature `attest`. It is in `full` but not in `core`,
  gated by `camel.attestation.enabled`. It is std-only and moves its parsers to `saltire-pki` when
  that exists.
  - `EvidenceProvider` trait: `platform()`, `evidence(report_data: [u8; 64]) -> Evidence`. The
    implementations are `Tsm { root }` (configurable path, default
    `/sys/kernel/config/tsm/report`), `Dstack { socket }`, `ConfidentialSpace { socket }`, later
    `Nitro` and `AzureVtpm`, and `Fake` for tests. Auto-detection probes in this order: dstack socket,
    launcher socket, configfs `provider`, `/dev/nsm`. The evidence carries a CMW-style type tag.
  - `Manifest` and the hash-chained `ManifestLog`, fed by `CamelContext` route events,
    `AiRegistry` change events (already exist) and properties reloads.
  - `Binder`: builds `report_data` from the manifest head, the TLS SPKI and the nonce, as in §4.2.
  - `Verifier` trait: `verify(evidence, nonce, expected_spki) -> Claims (EAT-shaped)`. The
    implementations are `Jwt` (ITA, MAA, Google, with pinned issuers and JWKS), `Nitro`, `SevSnp`,
    `TdxDcap` and `DstackVerifier` (HTTP to a self-hosted verifier).
  - `Policy`: allowed platforms, measurement allow-lists (MRTD, RTMR0-3, compose hash, PCR0, image
    digest), minimum TCB status, debug forbidden, maximum age.
- **Surfaces.** `/q/attestation`; MCP `_meta` in `server/discover` and `initialize`; the resource
  and the opt-in tool; the RA-TLS certificate extension; `cameleer attest show|verify <url>` in the
  CLI.
- **Gateway** (`camel.ai.mcp.NAME.attestation.*`): `required`, `policy`, `verifier` and
  `reattest` (on session start and on every `list_changed` re-sync). It fails closed: tools from a
  server that fails policy are never registered, which reuses rug-pull protection. Audit records gain
  the verified claims digest.

## 9. Phased plan

### Phase 1: produce evidence, expose it, bind it to TLS

- `cameleer-attest` with the `Tsm` and `Dstack` providers, `Manifest`, `ManifestLog` and `Binder`.
- `/q/attestation`, MCP `_meta`, the resource and the tool, and the RA-TLS certificate with
  `id-pe-cmw` when TLS is self-signed.
- The `cameleer attest show` command. Docs: `docs/guides/attestation.md`, plus FEATURES rows marked
  as extensions, since Camel has no equivalent.
- **Hermetic tests:**
  - **Fake TSM directory.** A temp dir with a watcher thread that emulates configfs. When `inblob`
    is written, it writes a synthetic TDX v4 quote or SNP report (a test-only key signs it) into
    `outblob` with REPORTDATA = inblob, sets `provider`, and bumps `generation`. Cases: the generation
    race is detected, `inblob` over 64 bytes is refused, the directory is removed afterwards.
  - **Fake dstack socket** (`cfg(unix)`). A `UnixListener` serving `/GetQuote`, `/Attest` and
    `/Info` with recorded responses. It checks raw 64-byte `report_data`, zero padding, and the
    400 error for `EmitEvent`.
  - **End to end.** Main with `--mcp` over HTTPS, a client nonce, then check that the evidence's
    REPORTDATA recomputes from the manifest, the *observed* peer SPKI and the nonce. A changed route
    file changes the digest, and a disabled tool appears in the log.
  - **No secrets in the manifest.** A property from `{{aws:…}}` appears only as a placeholder.
- On Windows, the dstack tests are `cfg(unix)` and the TSM tests run anywhere, since they are plain
  files.

### Phase 2: secret release

- `{{sealed:…}}` with the `dstack` (GetKey) and `aws-kms` (Recipient; needs the Nitro provider and a
  vsock transport, or NitroTPM) backends first, then `azure-skr` and `gcp-kms` with WIF.
- `camel.attestation.required`, start-up ordering, `cameleer seal`.
- **Hermetic tests.** The existing KMS mocks in `cameleer-cloud` gain Recipient handling (encrypt the
  response to the document's key and enforce the PCR conditions). A fake MAA issuer with a JWKS and a
  fake STS/WIF exchange. Tests prove that a wrong measurement releases nothing and that nothing is
  resolved before attestation.

### Phase 3: verifying remote servers

- The `Verifier` implementations in this order: `Jwt` (ITA, MAA, Google), `DstackVerifier`
  (HTTP), local `Nitro`, local `SevSnp`, then local `TdxDcap` with PCS/PCCS collateral caching.
- Gateway policy and `cameleer attest verify <url>`.
- **Hermetic tests.** Test CA chains standing in for the Intel, AMD and AWS roots (the roots are
  configurable, and the real ones pinned by default). Recorded real quotes from public test vectors,
  such as the `dcap-qvl` and `go-sev-guest` test data, used only with their licences checked. Negative
  cases for each check in §3.1, including the QE-identity check from CVE-2026-22696.
- Live tier `CAMEL_REAL_TEE=1`, once the owner picks a cloud (the dstack-on-GCP-TDX path is the
  simplest), as a new gated test file.

## 10. Risks and what can't be claimed

- **report_data is self-asserted.** The hardware vouches only that *the measured code* chose those 64
  bytes. The manifest is credible only because the platform measurement (compose hash, PCR0, image
  digest) shows a genuine Cameleer build that computes it honestly. Without reproducible builds and
  published digests, evidence proves "some binary", not "Cameleer 0.x".
- **Configuration outside the measurement.** Route files and properties loaded from a mounted volume
  or at runtime are covered only by the manifest, which rests on the point above. Runtime changes are
  logged, not prevented. Policy can require a sealed (measured) configuration.
- **TEEs are not proof against physical attackers.** TEE.fail (October 2025) used a DDR5 interposer
  costing under $1,000 against SGX, TDX and SEV-SNP. Its authors report it can extract keys,
  including ECDSA attestation keys. Battering RAM and WireTap did the same on DDR4. Intel and AMD
  class physical attacks as out of scope. A forged quote with a stolen PCK or attestation key verifies
  correctly, so docs must say that attestation trusts the cloud's physical security as well as the
  CPU vendor.
- **TCB lag.** An "UpToDate" status depends on collateral freshness and vendor advisories. Policy
  must decide whether `SWHardeningNeeded` or `OutOfDate` is acceptable.
- **The verifier is security-critical code.** CVE-2026-22696 shows that one missed check voids the
  whole verifier. A std-only DCAP verifier needs the independent audit already listed in STATUS
  (todo 5), plus fuzzing of every parser.
- **MCP clients through a model.** An LLM reading the attestation tool's output is not verification.
  Only a client that checks the SPKI binding and the policy itself gets the guarantee.
- **TLS terminators.** A load balancer or gateway that terminates TLS breaks the SPKI binding. The
  evidence then proves the backend, not the connection. Document the supported topologies
  (passthrough, or dstack-gateway style).
- **Liveness and replay.** Evidence fetched once at connect says nothing about later; re-attest on a
  schedule and on `list_changed`. Nonces must be at least 16 random bytes and single-use per
  session.
- **Platform churn.** dstack removed `EmitEvent` in 0.6.0; GCP lists TDX on `c4-standard-*` as
  Preview (2026-07-31); MAA claim names differ between examples; Nitro's `user_data` limit differs
  between spec and validator. Pin versions and test against recorded fixtures.
- **Std-only costs.** Nitro needs FFI for `ioctl` and an `AF_VSOCK` transport, which are not in
  `std`. That is acceptable FFI to libc, but owner approval is worth getting since it is a first. No
  crates are needed anywhere.

## 11. Competitive position

- **Integration engines and iPaaS.** Searches found **no** product that advertises attestation or
  confidential execution: Apache Camel (including Camel K and Camel Quarkus), MuleSoft, Boomi, WSO2
  and n8n. This is absence of evidence, not proof. The generic option is to run any of them inside a
  confidential VM or Confidential Containers, where Trustee releases secrets, with attestation handled
  by the infrastructure rather than the engine.
- **MCP products.**
  - Phala Cloud hosts MCP servers in TDX with attestation reports a user can check before configuring
    a client (posts predating 2026).
  - Oasis runs MCP servers in its ROFL TEE runtime.
  - A Gramine-based "Attestable MCP" uses RA-TLS so clients verify before connecting.
  - `cmcp-runtime` on PyPI is a developer preview of a "hardware-attested MCP runtime".
  - The community `HarrisonCN/mcp-gateway` added TEE attestation checks against a measurement
    allow-list.
  - CoSAI's MCP security paper recommends TEEs plus remote attestation for MCP servers.
  - A 2026 survey (arXiv 2605.03213) calls client verification of an attested MCP server an open
    protocol design problem without a standard.
- **Cameleer's angle.** It would be the only integration engine where the engine itself produces
  evidence covering *routes, properties and the exposed tool surface*, binds it to its own TLS key,
  and gates both secret release and outbound MCP calls on attestation. All of it is in one std-only
  binary with a small audit surface. It could also bring a concrete, spec-compatible `_meta` design
  to the MCP Security IG.

## Sources

- Linux configfs-tsm ABI: https://www.kernel.org/doc/Documentation/ABI/testing/configfs-tsm
- TDX guest driver: https://docs.kernel.org/virt/coco/tdx-guest.html
- TDX measurement registers in sysfs (6.16): https://kernel.org/doc/Documentation/ABI/testing/sysfs-devices-virtual-misc-tdx_guest ;
  patch series https://patchew.org/linux/20250424-tdx-rtmr-v5-0-4fe28ddf85d4@intel.com/20250424-tdx-rtmr-v5-5-4fe28ddf85d4@intel.com
- dstack API: https://github.com/Dstack-TEE/dstack/blob/master/sdk/curl/api.md ;
  https://raw.githubusercontent.com/Dstack-TEE/dstack/master/docs/guest-api-v0.md
- dstack trust center and RTMR3: https://docs.phala.com/dstack/trust-center-technical ;
  https://docs.phala.com/phala-cloud/attestation/verify-the-platform ; audit https://phala.network/dstack/dstack-audit.pdf
- dstack-cloud (GCP, Nitro): https://docs.phala.com/dstack-cloud/overview ; https://docs.phala.com/dstack-cloud/run-on-gcp
- dcap-qvl advisory: https://nvd.nist.gov/vuln/detail/CVE-2026-22696 ;
  https://phala.com/posts/dstack-security-update-attestation-pipeline-hardening
- Nitro root of trust: https://docs.aws.amazon.com/enclaves/latest/user/verify-root.html ;
  https://github.com/aws/aws-nitro-enclaves-nsm-api ;
  https://github.com/aws/aws-nitro-enclaves-nsm-api/blob/main/docs/attestation_process.md
- AWS KMS attestation: https://docs.aws.amazon.com/kms/latest/developerguide/services-nitro-enclaves.html
- AWS SEV-SNP (VLEK): https://docs.aws.amazon.com/AWSEC2/latest/UserGuide/snp-attestation.html
- Azure SKR: https://learn.microsoft.com/en-us/azure/confidential-computing/concept-skr-attestation ;
  https://learn.microsoft.com/en-us/azure/confidential-computing/skr-flow-confidential-vm-sev-snp
- Azure guest attestation design (NV indexes): https://learn.microsoft.com/en-us/azure/confidential-computing/guest-attestation-confidential-virtual-machines-design ;
  az-cvm-vtpm https://docs.rs/az-cvm-vtpm/0.8.1/src/az_cvm_vtpm/vtpm/mod.rs.html
- GCP Confidential Space tokens: https://docs.cloud.google.com/confidential-computing/confidential-space/docs/connect-external-resources ;
  claims https://docs.cloud.google.com/confidential-computing/confidential-space/docs/reference/token-claims
- Google Cloud Attestation: https://docs.cloud.google.com/confidential-computing/docs/attestation ;
  https://docs.cloud.google.com/confidential-computing/confidential-vm/docs/attestation
- GCP TDX machine types: https://docs.cloud.google.com/confidential-computing/confidential-vm/docs/supported-configurations
- Intel Trust Authority: https://docs.trustauthority.intel.com/main/articles/concept-attestation-overview.html ;
  https://docs.trustauthority.intel.com/main/articles/integrate-go-client.html ;
  https://github.com/intel/trustauthority-client-for-go/blob/main/go-tdx
- Intel PCS v3→v4: https://api.trustedservices.intel.com/documents/PCS_V3-V4_migration_guide.pdf ; dcap-qvl https://docs.rs/dcap-qvl
- SEV-SNP verification: https://beta.pkg.go.dev/github.com/google/go-sev-guest/verify ; https://lib.rs/crates/snpguest
- RFC 9334 RATS: https://www.rfc-editor.org/rfc/rfc9334.html ; RFC 9711 EAT: https://www.rfc-editor.org/rfc/rfc9711.html ;
  RFC 9999 CMW: https://www.rfc-editor.org/info/rfc9999 , https://datatracker.ietf.org/doc/draft-ietf-rats-msg-wrap/
- IETF SEAT: https://datatracker.ietf.org/wg/seat/documents/ ;
  https://datatracker.ietf.org/doc/draft-fossati-seat-early-attestation/ ;
  https://datatracker.ietf.org/doc/html/draft-fossati-seat-expat ;
  https://www.ietf.org/archive/id/draft-reddy-seat-expat-transport-01.txt
- Interoperable RA-TLS: https://github.com/CCC-Attestation/interoperable-ra-tls ;
  https://github.com/CCC-Attestation/interoperable-ra-tls/blob/main/docs/Interoperable-RA-TLS-SGX-TDX-evidence-formats.md ;
  IANA CBOR tag https://www.iana.org/assignments/cbor-tags/template/60001 ; Gramine https://gramine.readthedocs.io/en/latest/glossary.html
- MCP Security IG: https://modelcontextprotocol.io/community/interest-groups/security ;
  SEP-2809 https://github.com/modelcontextprotocol/modelcontextprotocol/pull/2809 ;
  issue #3354 https://github.com/modelcontextprotocol/modelcontextprotocol/issues/3354 ;
  issue #1461 https://github.com/modelcontextprotocol/modelcontextprotocol/issues/1461
- MCP-in-TEE products: https://phala.com/posts/mcp-got-you-worried-build-securely-with-phala-clouds-teepowered-hosting ;
  https://oasis.net/blog/confidential-mcp-servers-for-agents ; https://agentpedia.codes/mcp/attestable-mcp ;
  https://pypi.org/project/cmcp-runtime/ ; https://github.com/HarrisonCN/mcp-gateway/pull/114 ;
  CoSAI https://www.coalitionforsecureai.org/wp-content/uploads/2026/03/model-context-protocol-security-1.pdf ;
  survey https://arxiv.org/pdf/2605.03213
- Confidential Containers / Trustee: https://learn.microsoft.com/en-us/azure/openshift/confidential-containers-overview
- Physical attacks: https://thehackernews.com/2025/10/new-teefail-side-channel-attack.html

# DICOM and healthcare: research, positioning and build plan

_Research note, 2026-11-09. Internal (under `docs/design/`, not on the site). Nothing here is
built yet. Facts were checked against the sources linked inline. Items marked **[unverified]**
were not confirmed from a primary source; check them before they go on the site._

The owner asked for two things: start DICOM research, and work out how to make Cameleer the
default choice in the medical domain. The prompt came from a medical-imaging startup working
with DICOMweb, Azure Health Data Services DICOM and FHIR anonymisation.

## 0. Summary

- **No Apache Camel DICOM component exists.** The Camel component index lists `fhir` and `mllp`
  for healthcare (HL7 v2 is a data format, `camel-hl7`), and nothing for DICOM or IHE
  ([Camel components](https://camel.apache.org/components/next/index.html)). The only Camel and
  DICOM link is an unreleased `dcm4che-camel` artifact last dated 2018
  ([mvnrepository](https://mvnrepository.com/artifact/com.github.dcm4che.dcm4che/dcm4che-camel/master)).
  We can therefore choose the names: `dicom` (DIMSE), `dicomweb` and a `dicom` data format. That
  is also a real gap that Cameleer can fill, rather than parity work.
- **Mirth Connect went closed-source.** NextGen announced on 2025-03-19 that Mirth Connect 4.6 and
  later ship under a single commercial, proprietary licence, with 4.5.2 as the last open-source
  release ([NextGen](https://www.nextgen.com/blog/industry-news/a-new-era-for-mirth-connect-by-nextgen-healthcare)).
  Two forks followed:
  - **Open Integration Engine** (MPL 2.0, now an Eclipse project)
    ([GitHub](https://github.com/openintegrationengine/engine),
    [Eclipse](https://projects.eclipse.org/node/35849)).
  - **BridgeLink** (Innovar, 4.5.4 in July 2025)
    ([press release](https://natlawreview.com/press-releases/innovar-releases-bridgelink-454-only-enterprise-grade-open-source-interface)).

  Search terms such as "Mirth alternative" now have real volume, and a wave of migrations is
  under way.
- **An integration engine routes DICOM without decoding pixels.** It needs Part 10 parsing and
  writing, the data dictionary, character sets, DIMSE over the Upper Layer protocol, DICOMweb, and
  tag-level de-identification. Pixel codecs are needed only for three things: transcoding for a
  receiver that will not accept the stored transfer syntax, burned-in text redaction (Clean Pixel
  Data), and rendering. RLE and Deflate are cheap. JPEG 2000 and HTJ2K are the expensive ones.
- **Estimated size:** about 16-22k lines of std-only Rust for routing-grade DICOM (phases 1-3),
  plus 3-12k for the optional codecs. About a third of the DICOMweb and security work reuses what
  Cameleer already has: HTTP/1.1 and HTTP/2, TLS 1.2/1.3 and X.509, multipart, JSON, deflate, the
  hash-chained audit, MCP and attestation.
- **Positioning:** "the DICOM and HL7/FHIR router that fits in a modality cabinet and a
  confidential VM". This means a single small binary for edge imaging gateways and for AI
  inference routing, with de-identification and an audit trail that a buyer can verify. Win the
  edge gateway and AI routing niche first, then the Mirth migrations. Do not lead with a
  replacement for a full PACS or a hospital-wide engine.

## 1. DICOM essentials for an integration engine

### 1.1 File format (PS3.5, PS3.6, PS3.10)

A Part 10 file has four parts in this order:

- a 128-byte preamble;
- the magic bytes `DICM`;
- the File Meta Information group (0002,xxxx), which is always Explicit VR Little Endian and
  names the Transfer Syntax UID;
- the data set in that transfer syntax.

Sources: [PS3.10](https://dicom.nema.org/medical/dicom/current/output/chtml/part10/PS3.10.html)
and the [PS3.5 encoding rules](https://dicom.nema.org/medical/dicom/current/output/chtml/part05/PS3.5.html).

A data element is laid out as tag (group, element), then VR, then length, then value:

- **Implicit VR Little Endian** (`1.2.840.10008.1.2`) omits the VR and takes it from the
  dictionary. It is the default that every implementation must accept.
- **Explicit VR Little Endian** (`1.2.840.10008.1.2.1`) carries a two-letter VR. OB, OD, OF, OL,
  OV, OW, SQ, SV, UC, UN, UR, UT and UV use two reserved bytes and a 32-bit length; all the other
  VRs use a 16-bit length.
- **Deflated Explicit VR Little Endian** (`1.2.840.10008.1.2.1.99`) is also in use.
- **Explicit VR Big Endian** is retired but still appears in old archives.

There are about 34 VRs, for example:

- strings: AE, CS, LO, PN, UI, UT;
- numbers as text: DS, IS;
- binary numbers: US, SS, UL, SL, FL, FD, SV, UV;
- binary blobs: OB, OW;
- sequences: SQ, with undefined length and item delimiters.

Sequences nest arbitrarily, and so do items of undefined length.

**Data dictionary.** PS3.6 has several thousand attributes and the UID registry (SOP classes,
transfer syntaxes). It is published as DocBook XML. Treat it the way Cameleer treats `zones.bin`:
generate a compact table with a pinned standard edition and a SHA-256, using
`scripts/update-dicom-dict.py`, and never hand-edit it. The current edition at the time of writing
is 2026d (seen in the PS3.15 page header).

**Private tags.** These are odd groups with a private creator element (gggg,0010-00FF) that
reserves a block. A router must keep them intact and must not reinterpret them. The Retain Safe
Private option in de-identification depends on a list of private tags known to be safe.

**Character sets.** Specific Character Set (0008,0005) can name:

- single-byte ISO 8859 sets;
- ISO 2022 code extensions with escape sequences (JIS X 0208/0212 for Japanese, KS X 1001 for
  Korean);
- GB18030, GBK;
- UTF-8 (`ISO_IR 192`).

Cameleer has no general multi-byte charset tables today. The only Japanese table found is the
kanji table in `cameleer-dataformats/src/barcode`. This is real work: about 2-4k lines of tables
and code, mostly generated.

**Encapsulated pixel data.** For compressed transfer syntaxes, Pixel Data (7FE0,0010) has
undefined length. It contains a Basic Offset Table item, then fragments, then a sequence
delimiter. The Extended Offset Table (7FE0,0001) is used for large multi-frame objects. A router
copies these bytes unchanged. Splitting frames needs only the offset tables, not a codec.

Common compressed transfer syntaxes:

| transfer syntax | UID |
|---|---|
| JPEG Baseline | `.1.2.4.50` |
| JPEG Lossless SV1 | `.1.2.4.70` |
| JPEG-LS lossless and near-lossless | `.1.2.4.80` and `.81` |
| JPEG 2000 lossless and lossy | `.1.2.4.90` and `.91` |
| HTJ2K | `.1.2.4.201` to `.203` |
| RLE Lossless | `.1.2.5` |

### 1.2 DIMSE networking (PS3.7, PS3.8)

**Upper Layer (PS3.8).** DIMSE runs over TCP. The PDU types are:

- A-ASSOCIATE-RQ, AC and RJ;
- P-DATA-TF;
- A-RELEASE-RQ and RP;
- A-ABORT.

The ARTIM timer governs association set-up and release. An A-ASSOCIATE-RQ carries:

- the calling and called AE titles;
- the application context;
- one presentation context per abstract syntax (SOP class), each with an odd ID and a list of
  transfer syntaxes;
- user information: maximum PDU length, Implementation Class UID and version name,
  asynchronous operations window, SCP/SCU role selection (C-GET needs it), SOP Class Extended
  Negotiation, and User Identity Negotiation.

Each PDV in P-DATA-TF carries a command or data fragment flag and a last-fragment flag. The
command set is always Implicit VR Little Endian. Source:
[PS3.8](https://dicom.nema.org/medical/dicom/current/output/chtml/part08/PS3.8.html).

**DIMSE services (PS3.7, PS3.4).** These services matter for an engine:

- **C-ECHO** (Verification, `1.2.840.10008.1.1`).
- **C-STORE**, for the hundreds of Storage SOP classes. An SCP must accept whatever the
  configuration lists, or "all storage".
- **C-FIND**:
  - Patient Root and Study Root query/retrieve at the PATIENT, STUDY, SERIES and IMAGE levels;
  - Modality Worklist (`1.2.840.10008.5.1.4.31`).
- **C-MOVE.** The Move SCP opens a *new* association to the destination AE. It needs an AE-title
  to host:port table, and it reports pending, warning and failure counts.
- **C-GET.** Data comes back on the same association, using role selection. It is firewall- and
  cloud-friendly.
- **N-services:**
  - Storage Commitment Push Model (`1.2.840.10008.1.20.1`): N-ACTION, then an N-EVENT-REPORT on
    the same association or a later one;
  - MPPS: N-CREATE and N-SET.

The DIMSE status codes map onto Camel exception semantics as follows:

- success maps to success;
- warnings (`B000` and similar) map to a header, not an exception;
- failures (`A7xx`, `A9xx`, `Cxxx`) raise a `DicomStatusException` carrying the status.

**SCU and SCP roles.** An engine needs both:

- an SCP that receives from modalities and lets the route decide;
- an SCU that forwards to PACS or the cloud.

Retries, a pool of associations per destination, and queueing (seda or a persistent store)
belong to the engine. Today that work is done by appliances such as Laurel Bridge Compass and by
custom dcm4che code **[unverified: product capabilities taken from vendor marketing, not
checked]**.

### 1.3 DICOMweb (PS3.18)

DICOMweb is RESTful HTTP. Source:
[PS3.18](https://dicom.nema.org/medical/dicom/current/output/chtml/part18/PS3.18.html).

- **QIDO-RS** searches with `GET /studies`, `/studies/{uid}/series` and
  `/studies/{uid}/series/{uid}/instances`. Query parameters are tags or keywords, plus
  `includefield`, `fuzzymatching`, `limit` and `offset`. The response is the DICOM JSON model.
- **WADO-RS** retrieves:
  - `/studies/{uid}[/series/{uid}[/instances/{uid}]]`, returning `multipart/related;
    type="application/dicom"` with an optional `transfer-syntax=` parameter;
  - `/metadata`, returning JSON;
  - `/frames/{list}`, returning bulk data as `application/octet-stream` or an image type;
  - `/bulkdata`;
  - `/rendered` and `/thumbnail`.
- **STOW-RS** stores with `POST /studies[/{uid}]`. The request is `multipart/related` with Part 10
  instances or JSON plus bulk data. The response is a JSON or XML document listing referenced,
  failed and warning SOP sequences.
- **UPS-RS** is the worklist: `/workitems`, change-state, cancel request, and subscriptions with
  events delivered over WebSocket. Azure's DICOM service supports UPS-RS Push and Pull
  ([Microsoft](https://learn.microsoft.com/en-us/azure/healthcare-apis/dicom/overview)).
- **JSON model (PS3.18 Annex F).** It is an object keyed by an eight-hex-digit tag, for example
  `{"0020000D": {"vr": "UI", "Value": ["1.2..."]}}`. Details:
  - PN values are objects with `Alphabetic`, `Ideographic` and `Phonetic`;
  - binary values appear as `InlineBinary` (base64) or `BulkDataURI`;
  - sequences nest as arrays of objects.

  An XML model (Native DICOM Model, PS3.19) also exists.

Cloud targets all speak DICOMweb:

- **Azure Health Data Services DICOM** ([overview](https://learn.microsoft.com/en-us/azure/healthcare-apis/dicom/overview),
  [v2 API changes](https://learn.microsoft.com/en-ie/azure/healthcare-apis/dicom/dicom-service-v2-api-changes),
  [extended query tags](https://learn.microsoft.com/en-my/azure/healthcare-apis/dicom/dicom-extended-query-tags-overview)):
  QIDO, WADO, STOW, UPS-RS, a change feed, and up to 128 extended query tags.
- **Google Cloud Healthcare API DICOM stores**: DICOMweb plus a `deidentify` operation that
  removes tags and redacts burned-in text by OCR
  ([Google](https://docs.cloud.google.com/healthcare-api/docs/concepts/de-identification)).
- **AWS HealthImaging**:
  - STOW-RS import since July 2025, synchronous, up to 1 GB per call
    ([AWS](https://aws.amazon.com/about-aws/whats-new/2025/07/aws-healthimaging-dicomweb-stow-rs-data-imports));
  - asynchronous import jobs that transcode most frames to HTJ2K lossless
    ([AWS](https://aws.amazon.com/about-aws/whats-new/2024/03/aws-healthimaging-import-dicom-objects-htj2k-transfer-syntaxes)).

A cloud-aware `dicomweb` producer therefore needs per-provider authentication. Cameleer already
has the Azure AD, GCP OAuth and AWS SigV4 signers in `cameleer-cloud`.

### 1.4 De-identification (PS3.15 Annex E)

The Basic Application Level Confidentiality Profile is defined by Table E.1-1. Each attribute
gets an action code
([PS3.15 Annex E](https://dicom.nema.org/medical/dicom/current/output/chtml/part15/chapter_E.html)):

| code | action |
|---|---|
| D | replace with a dummy value |
| Z | zero-length or dummy value |
| X | remove |
| K | keep |
| C | clean |
| U | replace the UID consistently across a set of instances |

There are also compound codes: Z/D, X/Z, X/D, X/Z/D and X/Z/U\*.

The options (E.3) are:

- Clean Pixel Data;
- Clean Recognizable Visual Features;
- Clean Graphics;
- Clean Structured Content;
- Clean Descriptors;
- Retain Longitudinal Temporal Information, with Full Dates or Modified Dates;
- Retain Patient Characteristics;
- Retain Device Identity;
- Retain UIDs;
- Retain Safe Private;
- Retain Institution Identity.

A conforming de-identifier also adds Patient Identity Removed (0012,0062) and De-identification
Method Code Sequence (0012,0064). The method codes are DCM 113100 to 113111, one per option. It
may also write the original values into an Encrypted Attributes Sequence so that a re-identifier
can reverse them.

What this means for Cameleer:

- **Tag-level de-identification needs no pixels.** Table E.1-1 becomes a generated table, the
  options become flags, and UID remapping is consistent: keyed by an HMAC with a secret from the
  vault, so the same input UID always gives the same output UID across a batch and across nodes.
  Date shifting is per patient.
- **Clean Pixel Data** (burned-in annotation, common in ultrasound and secondary capture) needs:
  - a decoder;
  - either a region list per modality and manufacturer, which is the practical approach, the
    CTP/RSNA-style "pixel anonymizer" script approach **[unverified: CTP script syntax not
    checked]**, or OCR, which Google offers and which a std-only engine should not attempt
    first;
  - re-encoding.
- **FHIR de-identification** should share the same engine: a rules file, consistent
  pseudonymisation, date shifting and redaction. Microsoft's Azure de-identification service
  covers unstructured text only, with 50 KB requests and per-MB pricing; DICOM is not part of it
  ([Microsoft](https://learn.microsoft.com/en-us/azure/healthcare-apis/deidentification/overview)).
  This leaves room for a local, attested de-identifier that handles both FHIR and DICOM.

### 1.5 Security profiles (PS3.15 Annex B and Annex A.5)

Annex B lists these profiles
([PS3.15 contents](https://dicom.nema.org/medical/dicom/current/output/chtml/part15/PS3.15.html)):

- **Retired:** Basic TLS (B.1). Its page now reads "Retired. See PS3.3-2018a".
- **Older:** ISCL (B.2) and AES TLS (B.3).
- **User identity association profiles:**
  - B.4 Basic User Identity;
  - B.5 User Identity Plus Passcode;
  - B.6 Kerberos;
  - B.7 SAML.
- **Email:** B.8 Secure Use of Email Transport.
- **The current TLS family:**
  - B.9 BCP 195 TLS;
  - B.10 Non-Downgrading BCP 195;
  - B.11 Extended BCP 195;
  - B.12 BCP 195 RFC 8996, 9325;
  - B.13 Modified BCP 195 RFC 8996, 9325.

B.12 is the profile to claim. RFC 8996 deprecates TLS 1.0 and 1.1, and RFC 9325 is the 2022 TLS
recommendations. `cameleer-tls` already does TLS 1.2 and 1.3 with mutual authentication and
X.509 path validation, so DICOM TLS is mostly configuration plus tests.

Other security details:

- **JWT user identity.** A JWT user identity type was added to User Identity Negotiation
  **[unverified: number and supplement not checked]**.
- **Port.** The registered port for DICOM over TLS is 2762 **[unverified against IANA in this
  pass]**.
- **Audit (Annex A.5)** is the DICOM audit message schema (the successor to RFC 3881). It
  includes events such as Application Activity, Begin Transferring, DICOM Instances Transferred,
  Data Export and Import, Query, Security Alert and User Authentication, and it is sent as syslog
  over TLS for IHE ATNA.

### 1.6 Codecs: routing versus viewing

| need | codec required? |
|---|---|
| Receive, route on tags, coerce tags, forward in the same transfer syntax | No |
| DICOMweb metadata, QIDO indexing, JSON conversion | No |
| Tag de-identification, UID remapping | No |
| Split or assemble multi-frame objects, WADO-RS `/frames` in the stored syntax | No (offset tables only) |
| Forward to a receiver that accepts only Implicit or Explicit LE | **Decode** the stored codec |
| Deflated transfer syntax | inflate/deflate (exists in `cameleer-core`) |
| Clean Pixel Data (burned-in PHI) | Decode, mask, re-encode (or store uncompressed) |
| Rendered or thumbnail images, AI pre-processing | Decode (and a PNG/JPEG writer) |

The practical rule is to negotiate. The SCU proposes the stored syntax plus Explicit and Implicit
LE. If the peer refuses the compressed syntax, the engine either decompresses, if it has the
codec, or fails with a clear error naming the transfer syntax. In this pass I found no data on
how often real peers refuse; vendor folklore says older PACS and some AI vendors accept only
uncompressed data **[unverified]**.

Codecs in rough lines of std-only Rust:

| codec | decode | encode |
|---|---|---|
| RLE Lossless | ~300 lines | ~300 lines |
| JPEG Lossless (Process 14, SV1) | ~1k | ~800 |
| JPEG Baseline / Extended (8 and 12 bit) | ~1.5k | ~1k |
| JPEG-LS | ~1.5k | ~1.5k |
| JPEG 2000 (Part 1) | 6-8k | considerably more |
| HTJ2K | ~3k extra | |

The reference tools treat codecs as add-ons: fo-dicom ships them in a separate package
([fo-dicom](https://github.com/fo-dicom/fo-dicom)), and dicom-rs gates them behind features with
partial support ([dicom-rs](https://github.com/enet4/dicom-rs)).

### 1.7 Size estimate, by part

| part | content | new lines | reuse |
|---|---|---|---|
| Data model and Part 10 read/write | VR codec, explicit and implicit LE, BE read, deflate, sequences, undefined lengths, encapsulated fragments, streaming parse that stops before pixel data | 3-4k | deflate in `cameleer-core` |
| Dictionary | generated tables (attributes, UIDs, SOP classes), generator script with pinned SHA | 1k code + generated data | tzdata pattern |
| Character sets | ISO 8859-x, ISO 2022 JIS/KS X, GB18030/GBK, UTF-8 | 2-4k (mostly tables) | kanji table idea |
| Upper Layer + DIMSE | PDUs, association state machine (PS3.8 states Sta1-Sta13), ARTIM, presentation-context negotiation, role selection, user identity, C-ECHO/STORE/FIND/MOVE/GET, N-ACTION/N-EVENT-REPORT, N-CREATE/N-SET | 4-5k | `net::bind_listener`, `shutdown_socket`, `NetStream`, TLS |
| `dicom` component | SCP and SCU endpoints, AE table, association pool, C-MOVE destination handling, storage commitment tracking, status mapping | 2-3k | consumer and producer base, seda, idempotent repository |
| DICOM JSON and XML models | PS3.18 Annex F, PS3.19 native XML, bulk-data URIs | 1k | `cameleer-core` JSON, `cameleer-xml` |
| `dicomweb` component | QIDO, WADO, STOW, UPS-RS client and server, `multipart/related` streaming, accept and transfer-syntax negotiation, cloud authentication | 2.5-3.5k | HTTP/1.1 and HTTP/2 server and client, multipart, cloud signers, WebSocket |
| De-identification | Table E.1-1 generated, options, HMAC UID map, date shift, Encrypted Attributes Sequence (CMS: AES and RSA already in `cameleer-tls/crypto`) | 1.5-2k | crypto, vault |
| Audit | DICOM A.5 XML messages, syslog TLS (RFC 5424/5425) | 600-800 | `syslog` component, audit chain |
| Tests and embedded servers | test SCP/SCU, test DICOMweb server, fixtures | 3-4k | test harness style |

**Total for routing-grade DICOM: about 16-22k lines plus tests, without codecs.** For scale,
Cameleer's HL7 plus MLLP code is about 1.1k lines and its FHIR module about 3.7k
(`crates/cameleer-protocols/src/hl7.rs`, `mllp.rs`, `crates/cameleer-soap/src/fhir`). DICOM is a
much larger standard, but the engine-relevant subset is bounded.

## 2. What exists today

### 2.1 Apache Camel and Camel Quarkus

- `camel-mllp` (since 2.17), `camel-fhir` (since 2.23, HAPI-based), the `camel-hl7` data format
  and the `hl7terser` language. Cameleer already mirrors all of these.
- **No DICOM and no IHE component** in the index
  ([Camel components](https://camel.apache.org/components/next/index.html)).
- **IHE in the Java world:** IHE profiles are covered by the separate Open eHealth IPF project,
  built on Camel. It has components such as `xdsi-rad69` for RAD-69 Retrieve Imaging Document Set
  ([IPF](https://oehf.github.io/ipf-docs/docs/ihe/rad69)). IPF is the model to look at for
  IHE-as-components.
- Camel Quarkus follows the Camel list, so it has no DICOM component either **[unverified
  directly on the Camel Quarkus extension list; inferred from the Camel index]**.

### 2.2 Interface engines

| engine | notes |
|---|---|
| **Mirth Connect / NextGen Connect** | Java. Commercial-only from 4.6 (announced 2025-03-19); 4.5.2 is the last open-source release; free licence for non-profit research ([NextGen](https://www.nextgen.com/blog/industry-news/a-new-era-for-mirth-connect-by-nextgen-healthcare)). Forks: OIE (MPL 2.0, Eclipse) and BridgeLink. Mirth has a DICOM connector built on dcm4che 2 **[unverified version]**. Typical complaints are heap tuning (512 MB default), queues filling memory, and storing every message in the database ([NextGen performance docs](https://docs.nextgen.com/en-US/mirthc2ae-connect-by-nextgen-healthcare-user-guide-3273569/channel-performance-12761)). Many tuning posts come from consultancies selling services. |
| **Rhapsody** (Rhapsody Health, includes the former Corepoint) | Commercial. Reviewers cite premium total cost of ownership and professional-services spend; no public list price ([rfp.wiki](https://www.rfp.wiki/vendors/rhapsody)). Rhapsody actively runs "Rhapsody vs Mirth" campaigns ([Rhapsody](https://rhapsody.health/rhapsody-mirth-tko)). |
| **Infor Cloverleaf** | Commercial. A UK G-Cloud listing shows £18,000 per licence; a user forum thread complains about price ([Clovertech](https://clovertech.infor.com/forums/topic/is-infor-pricing-causing-cl-to-lose-market-share/)). |
| **InterSystems HealthShare / IRIS for Health** | Commercial platform: database, engine and FHIR server. Heavyweight; priced per core **[unverified]**. |
| **Iguana** (iNTERFACEWARE) | Commercial. Priced per number of integrations, from $10,000 a year for two integrations ([iNTERFACEWARE](https://interfaceware.com/pricing)). |

### 2.3 Cloud healthcare services

- **Azure Health Data Services:** FHIR and DICOM services (see §1.3), and a text
  de-identification service. Azure API for FHIR is retired on 2026-09-30 in favour of the
  Health Data Services FHIR service
  ([Microsoft](https://learn.microsoft.com/en-gb/Azure/healthcare-apis/services-features-regional-availability)).
- **Google Cloud Healthcare API:** FHIR, HL7v2 and DICOM stores, with de-identification for DICOM
  (tags, OCR redaction) and FHIR.
- **AWS:** HealthImaging (DICOMweb, HTJ2K storage) and HealthLake (FHIR).

In every case the integration need is the same: get DICOM off the modality, which speaks DIMSE,
into the cloud, which speaks DICOMweb, and get results back. The hospital-side gateway is the
gap that every cloud leaves to partners or to the customer's own code.

### 2.4 Open-source DICOM tools (for the feature map; not dependencies)

| tool | language, licence | what it covers |
|---|---|---|
| dcm4che 5 / dcm4chee-arc | Java; MPL 1.1 / GPL 2 / LGPL 2.1 tri-licence ([PMC table](https://pmc.ncbi.nlm.nih.gov/articles/PMC10584756/table/Tab3)) | full toolkit (storescu, storescp, findscu, movescu, getscu, stgcmtscu, dcmqrscp), archive, DICOMweb, audit, HL7 |
| DCMTK | C++; OFFIS BSD-style, some modules BSD-2/3 ([Debian copyright](https://sources.debian.org/copyright/license/dcmtk/3.6.7-9~deb12u3)) | the reference CLI tools (echoscu, storescu, storescp, findscu, movescu, getscu, dcmqrscp, dcmodify, dcmdjpeg, dcmcjpls) |
| Orthanc | C++; GPLv3 with OpenSSL exception ([README](https://orthanc.uclouvain.be/hg/orthanc/annotate/3f9569917745/README)) | lightweight PACS, REST, DICOMweb plugin, Lua and Python scripting, routing |
| pydicom | Python; MIT | parsing and writing; the de facto test-file corpus |
| fo-dicom | C#; MS-PL ([GitHub](https://github.com/fo-dicom/fo-dicom)) | toolkit; codecs in a separate package; used by MONAI's gateway |
| dicom-rs | Rust; MIT/Apache-2.0 | parsing, async UL, storescu/storescp/findscu, codecs behind features via C libraries ([GitHub](https://github.com/enet4/dicom-rs)) |
| MONAI Deploy Informatics Gateway | C#; Apache-2.0 | SCP (C-ECHO, C-STORE), SCU C-STORE, STOW-RS, HL7; its SCP conforms to no DICOM security profile ([MONAI docs](https://monai.io/monai-deploy-informatics-gateway/compliance/dicom.html)) |
| mercure | Python; core MIT, some modules GPL-3.0 ([GitHub](https://github.com/mercure-imaging)) | rule-based DICOM routing and AI orchestration |

The last two are the closest functional competitors for AI routing. Both are multi-process
stacks (Docker, a message bus, a database) rather than a single binary. MONAI MIG explicitly
lacks DICOM security profiles. That is the opening.

### 2.5 What teams complain about

- **Licensing:** the Mirth change; per-interface pricing (Iguana); premium total cost of
  ownership (Rhapsody, Cloverleaf).
- **JVM footprint and tuning:** heap sizing and GC pauses in Mirth. dcm4chee-arc needs WildFly
  plus a database plus LDAP **[unverified: dcm4chee-arc deployment docs not re-checked this
  pass]**.
- **Edge deployment:** gateways run on small boxes next to modalities, or inside the modality
  vendor's own appliance. Docker-and-database stacks are a burden there, and a single 6-24 MB
  binary with 3 ms startup is a real advantage. This is the claim to measure.
- **Cloud cost and egress:** per-request and per-GB pricing on cloud DICOM services rewards
  sending only what is needed. A filtering, de-identifying router at the edge saves money.
  Sizing this needs real price sheets **[unverified: pricing pages not fetched]**.
- **Security posture:** gateways with no DICOM TLS profile, plaintext port 104 on hospital LANs,
  and weak audit trails.

## 3. Market and positioning

### 3.1 Where Cameleer can become the default

Ranked by fit, from best to weakest:

1. **Edge imaging gateway, from modality to PACS or cloud.** The DIMSE SCP, rules, de-identify
   and coerce, then STOW-RS to Azure, Google or AWS, or C-STORE to PACS, with a store-and-forward
   queue. Footprint, startup, and no JVM or database matter most here. Buyers include:
   - imaging startups;
   - teleradiology companies;
   - modality OEMs that need an embedded forwarder;
   - hospital IT departments moving to cloud PACS.
2. **AI inference routing.** Receive a study, select series by tags, de-identify if the model is
   external, send to the model, then convert the result back to DICOM (SR, SEG, Secondary
   Capture) or to FHIR (DiagnosticReport, Observation) and route it to PACS or the EHR. This fits
   IHE AIR (AI Results, still at Trial Implementation, Rev 1.2 from 2022)
   ([IHE wiki](https://wiki.ihe.net/index.php/AI_Results)). Cameleer's `agent` step and model
   components can route to LLM and vision models, and the action layer lets an AI agent query and
   retrieve studies as MCP tools, behind approvals, redaction and an audit trail. No competitor
   in §2 has this.
3. **De-identification pipelines for AI training data.** These need PS3.15 conformance with
   options, deterministic pseudonymisation across batches and nodes, and a verifiable log. Run
   inside a confidential VM, an attested de-identifier lets a data owner check that only the
   attested binary and policy saw identifiable data. That is a strong story for GDPR and for
   research data-sharing agreements.
4. **HL7 v2 and FHIR interface engine, for Mirth migrations.** Cameleer already has MLLP, HL7
   and FHIR. To win migrations it needs:
   - a Mirth channel importer (channel XML to Cameleer YAML, at least for common connector and
     transformer patterns);
   - JavaScript transformer compatibility through `cameleer-script`;
   - a message browser.

   This is a larger fight with entrenched UIs, so enter it on cost, footprint and openness. Do
   not try to win it on UI first.
5. **TEFCA and HIE exchange.** TEFCA runs through QHINs, using IHE XCA/XDS-style document
   exchange plus FHIR, with exchange purposes such as Treatment and Individual Access Services
   ([ASTP/ONC](https://www.healthit.gov/topic/interoperability/policy/trusted-exchange-framework-and-common-agreement-tefca),
   [RCE](https://rce.sequoiaproject.org/exchange-purposes-explained/)). This is a later target:
   it needs IHE XDS/XCA (SOAP and MTOM, which `cameleer-soap` partly has) and participant
   onboarding. It is not imaging-led.
6. **PHI processing in the cloud with attestation.** This is a differentiator under 1-3, not a
   market on its own: "the gateway proves what code handled your PHI". It builds on
   `docs/design/attestation/research.md`.

**Recommendation:** lead with 1 and 2, under the tagline *"The DICOM router for the AI era: one
small binary, DIMSE to DICOMweb, de-identification and AI results, verifiable."* Make 3 the
proof point for confidential computing. Run 4 as a parallel content and SEO track that uses the
Mirth change.

### 3.2 Buyer personas and their search terms

| persona | cares about | searches **[search volumes not measured]** |
|---|---|---|
| Imaging-startup CTO or engineer (the follower who prompted this) | DICOMweb to Azure/GCP/AWS, DIMSE from hospitals, de-identification, cost | "DICOM router open source", "DIMSE to DICOMweb", "DICOM to Azure DICOM service", "STOW-RS proxy", "DICOM gateway docker", "dcm4che alternative", "Orthanc routing" |
| Hospital or IDN integration analyst | HL7 v2, FHIR, reliability, licence cost | "Mirth Connect alternative", "Mirth open source fork", "Open Integration Engine", "HL7 interface engine open source", "FHIR integration engine" |
| Radiology IT / PACS administrator | modality routing, worklist, C-MOVE, TLS | "DICOM router", "DICOM tag morphing", "DICOM TLS", "modality worklist broker" |
| AI or ML platform lead | study selection, de-identification, results back to PACS | "DICOM de-identification PS3.15", "AI orchestration DICOM", "DICOM SR SEG export", "IHE AIR", "MONAI deploy alternative" |
| Modality or device OEM engineer | embedded footprint, SBOM, FDA cybersecurity | "embedded DICOM library", "DICOM SDK Rust", "SBOM medical device", "FDA 524B" |
| CISO / privacy officer | audit, encryption, attestation, BAA | "ATNA audit", "HIPAA audit log", "confidential computing healthcare" |

## 4. Compliance and trust

| item | what it really is | Cameleer analogue today | needed |
|---|---|---|---|
| **HIPAA / BAA** | A BAA is signed by an entity that creates, receives, maintains or transmits PHI for a covered entity. Cloud hosts that store PHI are business associates even when they never view it; the conduit exception is narrow ([Hunton on OCR cloud guidance](https://huntonak.com/privacy-and-information-security-law/hhs-releases-guidance-hipaa-cloud-computing)). Software a customer runs itself does not need a BAA. The vendor needs one only if it hosts PHI or touches it in support. | n/a | A clear "you run it, no BAA needed; managed offering later with a BAA" page. Support procedures that avoid PHI (redacted diagnostics). |
| **HIPAA Security Rule controls** | Access control, audit controls, integrity, transmission security | TLS everywhere; hash-chained audit (`cameleer-ai/src/audit.rs`); redaction guardrails; vault | Map the controls in a guide. The 2025 NPRM to tighten the Security Rule is pending **[unverified: status not checked]**. |
| **GDPR** | Pseudonymisation (Art. 4(5)), data minimisation, processing records | de-identification (planned), attestation | DPIA-ready documentation of the de-identification engine |
| **DICOM Conformance Statement** | Required by PS3.2 for any product claiming conformance: SOP classes, roles, transfer syntaxes, AE configuration, security profiles, character sets ([PS3.2](https://dicom.nema.org/medical/dicom/current/output/chtml/part02/PS3.2.html)) | `cameleer validate` and `schema`; FEATURES table | Write `docs/conformance/dicom-conformance-statement.md` in PS3.2 template form; generate the SOP class tables from the dictionary |
| **IHE profiles** | Scheduled Workflow (SWF.b), XDS-I.b (RAD-68/69), PIX/PDQ (and PIXm/PDQm on FHIR), ATNA, CT (consistent time), AIR, AIW-I | none | IHE actors as route templates or kamelets; ATNA first because everything else assumes it |
| **IHE Connectathon** | Peer-to-peer testing week. NA 2025 was Toronto, CAD 8,500 per system ([newswire](https://newswire.ca/news-releases/canada-to-host-the-2025-ihe-north-america-connectathon-week-837529530.html)). Test tools in Gazelle; AIR test data published ([Gazelle](https://gazelle.ihe-europe.net/content/airtestdata)) | none | A later goal, after ATNA, SWF and AIR. Needs the owner's budget decision (registration is a purchase). |
| **ATNA / DICOM audit** | PS3.15 A.5 messages over syslog TLS | hash-chained tool audit | Emit A.5 events from `dicom`, `dicomweb`, `mllp` and `fhir`, chained and exportable to an ATNA repository |
| **SBOM** | FDA §524B requires an SBOM for "cyber devices" in premarket submissions | Container builds carry an SBOM and provenance (`.github/workflows/container.yml`). Std-only means the SBOM is essentially Cameleer's own crates plus Rust std, which is a strong story | Publish a CycloneDX/SPDX SBOM for the binary releases too, and a VEX process |
| **FDA premarket cybersecurity** | Applies when Cameleer is *part of a medical device* (an OEM embeds it in a modality or in software as a medical device). The OEM is the submitter. Final guidance June 2025, reissued in early February 2026 to align with QMSR/ISO 13485 ([RAPS](https://www.raps.org/resource/fda-reissues-cybersecurity-guidance-to-align-with.html)) **[the February 2026 title and date are not confirmed on fda.gov]**. A standalone hospital routing engine is generally not a device (MDDS-type software) **[regulatory classification: needs counsel]** | std-only supply chain; reproducible builds (to verify) | An "OEM pack": SBOM, vulnerability disclosure policy, threat model, a security test report, the fuzzing results from STATUS item 5 |
| **Attestation for PHI** | Not a regulatory requirement; a trust differentiator | attestation design in progress | Bind de-identification policy hashes and the binary measurement into the evidence |

The security audit and fuzzing item in STATUS (item 5) becomes a sales prerequisite in this
market. The first questions a hospital CISO asks are about the from-scratch TLS and the DICOM
parsers.

## 5. Build plan

### 5.1 Names

Camel has no DICOM names to mirror, and dcm4che-camel was never released. Proposal, following
Camel conventions (scheme = protocol, data format = encoding):

- **`dicom` component (DIMSE):**
  - consumer = SCP: `dicom:MYAE@0.0.0.0:11112?sopClasses=storage&transferSyntaxes=...`;
  - producer = SCU: `dicom:PACS@pacs.local:104?operation=store|echo|find|move|get|commit`.

  Options mirror dcm4che and DCMTK CLI names: `callingAet`, `calledAet`, `maxPduLength`,
  `acceptTimeout`, `idleTimeout`, `tls=true`, `tlsProfile=bcp195-rfc9325`, `userIdentity`,
  `queryLevel`, `queryModel=StudyRoot|PatientRoot|MWL`, `moveDestination`, `priority`.
- **`dicomweb` component:**
  - producer: `dicomweb:https://host/dicomweb?operation=qido|wado|stow|ups&provider=azure|gcp|aws`;
  - consumer: a DICOMweb server on the shared HTTP server, `dicomweb:/dicom-web?operations=stow,qido,wado`,
    and an Azure change-feed poller.
- **`dicom` data format.** `unmarshal` gives a `DicomObject` body, with pixel data held lazily.
  `marshal` writes Part 10 in a chosen transfer syntax. There are JSON (PS3.18 Annex F) and XML
  (PS3.19) variants: `dicomJson`, `dicomXml`.
- **Simple access to tags:** `${dicom.PatientID}`, `${dicom.(0010,0020)}`,
  `${dicom.ReferencedSeriesSequence[0].SeriesInstanceUID}`. Add a `dicom` expression language
  for predicates and setters (`setDicom` processor), the way `hl7terser` works for HL7.
- **Headers:** `CamelDicomCallingAet`, `CamelDicomCalledAet`, `CamelDicomSopClassUid`,
  `CamelDicomSopInstanceUid`, `CamelDicomTransferSyntaxUid`, `CamelDicomStatus`,
  `CamelDicomRemoteAddress`. Follow Camel's `Camel<Component><Name>` style.
- **De-identification:** a processor, `deidentify` (YAML `dicomDeidentify:`), with
  `profile=basic` and `options=retainLongitudinalModifiedDates,retainDeviceIdentity,...`,
  `uidRoot`, `uidKey={{vault:...}}` and `dateShift=perPatient`. Also a `fhirDeidentify` sibling
  sharing the rules engine. Make it a processor rather than a data format, because it transforms
  a decoded object in place and needs state (the UID map and the date-shift key).

### 5.2 Phases

**Phase 1: data format and dictionary (about 6-8k lines).**

- Part 10 and data-set codec (implicit, explicit, deflated LE, BE read), streaming parse with
  `stopBeforePixelData`.
- Generated dictionary and UID tables with the pinned script, and character sets (single-byte and
  UTF-8 first, then ISO 2022 and GB18030).
- JSON and XML models, Simple tag access, `setDicom`.
- Tests: round trips of every file in the pydicom and NEMA sets (see §5.4), byte-exact rewrites
  where the input was canonical, and JSON compared with the standard's examples.

**Phase 2: DIMSE (about 6-8k lines).**

- Upper Layer state machine, negotiation, C-ECHO, C-STORE SCP and SCU, C-FIND, C-MOVE, C-GET,
  storage commitment, and the TLS profile B.12.
- Hermetic tests against an embedded SCP and SCU in `cameleer-protocols` or a new
  `cameleer-dicom` crate (preferred: its own crate and its own cargo feature `dicom`, in `full`).
- **Live tier `CAMEL_DICOM_INTEROP=1`:**
  - DCMTK from Ubuntu 24.04 apt (`dcmtk`), checksummed by apt: `echoscu`, `storescu`, `storescp`,
    `findscu`, `movescu`, `getscu`, `dcmqrscp`, with and without `+tls`;
  - dcm4che 5 CLI tools from Maven Central, jar SHA-256 pinned as with ActiveMQ:
    `storescu`, `storescp`, `findscu`, `movescu`, `getscu`, `stgcmtscu`;
  - Orthanc from apt (`orthanc`), as a PACS for C-MOVE and C-FIND and as a peer.
  - Extend `scripts/live-interop.sh` with a `dicom` target.

**Phase 3: DICOMweb and cloud (about 3-4k lines).**

- QIDO, WADO, STOW client and server; `multipart/related` streaming both ways; UPS-RS client;
  providers for Azure, GCP and AWS HealthImaging.
- Hermetic tests with an embedded DICOMweb server.
- Simulator tier:
  - Orthanc with its DICOMweb plugin (apt `orthanc-dicomweb`);
  - Microsoft's open-source `dicom-server` (MIT, the code behind the Azure service) as an
    emulator, which needs .NET and storage emulation (Azurite is already in the simulator
    tier) **[unverified: build effort]**.
- Real-cloud runs join STATUS item 1, with credentials from the owner only.

**Phase 4: de-identification and audit (about 2.5-3k lines).**

- `deidentify` with every Annex E option except Clean Pixel Data; HMAC UID mapping; date shift;
  Encrypted Attributes Sequence.
- `fhirDeidentify`.
- DICOM A.5 audit events to syslog TLS, chained.
- Tests: compare against the pydicom-based `deid` and DCMTK `dcmodify` outputs **[unverified:
  tool availability]**, and assert Table E.1-1 attribute by attribute.

**Phase 5: examples, guides and site.**

- `examples/medical/modality-to-cloud.camel.yaml`: C-STORE SCP, coerce, de-identify optionally,
  STOW-RS to Azure, persistent retry, storage commitment back to the modality.
- `examples/medical/ai-inference-loop.camel.yaml`: SCP, series filter, de-identify, model call
  (HTTP or a model component), result as DICOM SR (and later SEG) to PACS plus a FHIR
  DiagnosticReport; written to the IHE AIR shape.
- `examples/medical/deid-pipeline.camel.yaml`: folder or DICOMweb in, de-identified out, audit
  log, run attested.
- `examples/medical/mirth-style-hl7.camel.yaml`: an ADT router with FHIR conversion, as a
  migration showcase.
- The DICOM Conformance Statement (PS3.2 form) and component pages with `**Cargo feature:**`.
- Site pages:
  - `/use-cases/dicom-router/`;
  - `/use-cases/dicom-to-cloud-dicomweb/` (Azure, GCP, AWS variants);
  - `/use-cases/dicom-de-identification/`;
  - `/use-cases/medical-imaging-ai-routing/`;
  - `/compare/mirth-connect-alternative/`;
  - `/compare/orthanc-vs-cameleer-routing/`;
  - `/compare/dcm4che-vs-cameleer/`;
  - a hub `/healthcare/` linking to the existing HL7/MLLP page.

**Phase 6: codecs and pixel options (3-12k lines, decide per need).**

- RLE and JPEG Lossless first: small, and lossless, so they are safe for diagnostic forwarding.
- Then JPEG Baseline and JPEG-LS.
- Clean Pixel Data with region rules.
- JPEG 2000 and HTJ2K only if customers require transcoding to or from them. AWS HealthImaging
  does its own HTJ2K transcoding on import, which lowers the need.

**Phase 7: IHE.**

- ATNA (done in phase 4), SWF.b (MWL, MPPS), PIX/PDQ v2 and FHIR (PIXm, PDQm), XDS-I.b on
  `cameleer-soap`, AIR.
- Then the IHE Connectathon, on the owner's decision and budget.

**Optional: a Mirth channel importer** (channel XML to YAML) as a separate track, with a tool to
check what will not convert.

### 5.3 What must be true to be the default

Measure these and publish them in `docs/BENCHMARKS.md`, using `scripts/cloud-bench` as for
Camel:

1. **Throughput:**
   - C-STORE instances per second and MB/s, SCP to SCU forward, for small CT slices (about 512
     KB) and large multi-frame objects (100 MB or more), against dcm4che storescp/storescu
     forwarding, DCMTK and Orthanc routing on the same machines;
   - HL7 v2 messages per second over MLLP against Open Integration Engine 4.5.2 and BridgeLink
     (Mirth 4.6 or later needs a licence; do not buy one without the owner).
2. **Footprint:** RSS idle and under load, binary size with only `--features dicom`, and threads.
   Compare with a JVM (dcm4che, OIE) and with Orthanc.
3. **Startup and recovery:** time from start to an SCP accepting associations, and catch-up after
   a downstream outage with a persistent queue.
4. **Correctness:**
   - parse and rewrite every file in the public corpora with no unexpected diffs;
   - DIMSE interop with DCMTK, dcm4che and Orthanc in both roles, with and without TLS;
   - DICOMweb against Orthanc's plugin and against at least one real cloud.
5. **Security:** fuzzing of the Part 10 parser, the PDU parser and multipart, with results
   published; the third-party audit (STATUS item 5).
6. **Edge proof:** a run on a Raspberry Pi-class arm64 board and in a 64 MB container limit.

### 5.4 Public test sets and their licences

| set | where | licence |
|---|---|---|
| NEMA WG-04 compression samples | `ftp://medical.nema.org/MEDICAL/Dicom/DataSets/WG04` (uncompressed, JPEG, JPEG-LS, J2K versions of GE and Toshiba images; WG-04 notes from 2024 say they need updating) ([WG-04 minutes](https://dicom.nema.org/dicom/minutes/wg-04/2024/WG-04-2024-10-02-tcon-Mins.pdf)) | **[unverified]**: no explicit licence found; NEMA distributes them for testing. Download with checksums recorded; do not commit them to the repo until terms are clear. |
| NEMA multiframe and WG-15 sets | same FTP tree (`Multiframe/`, `DataSets/WG15/`) | same caveat |
| pydicom test files | `pydicom/pydicom-data` and files in the pydicom repo | packaged as MIT ([CERN mirror](https://simple-repository.app.cern.ch/project/pydicom-data)); individual files come from many sources **[unverified per file: read LICENSE]** |
| dicom-rs `dicom-test-files` | crates.io (we would fetch the files, not depend on the crate) | "Custom" licence **[read LICENSE.txt]** ([lib.rs](https://lib.rs/crates/dicom-test-files)) |
| DICOM standard examples | JSON and XML examples in PS3.18 and PS3.19; DICOM is published by NEMA with a copyright notice allowing use **[unverified wording]** | NEMA copyright |
| David Clunie's test images, IHE MESA, TCIA | listed by the DCMTK wiki; TCIA collections are mostly CC BY 3.0/4.0 **[unverified per collection]** | per collection |
| IHE AIR test data | Gazelle ([link](https://gazelle.ihe-europe.net/content/airtestdata)) | IHE terms **[unverified]** |

Follow the tzdata pattern: a `scripts/fetch-dicom-testdata.sh` with pinned URLs and SHA-256s,
files kept outside git, and a small set of self-generated fixtures in the repo for the hermetic
tier.

## 6. Risks and open questions

- **Scope creep into a PACS.** Stay a router: storage is the target's job. Short-term caching is
  enough, through the file and idempotent repositories.
- **Codec demand is unknown.** Ask the follower and two or three other imaging teams which
  transfer syntaxes they actually receive and whether their targets refuse any.
- **Character sets** are a frequent source of interop bugs, especially Japanese, Korean and
  Chinese. Budget tests with real ISO 2022 samples from the pydicom corpus.
- **Regulatory claims.** Never claim "HIPAA compliant" or "FDA cleared". Say "supports HIPAA
  Security Rule controls" and "designed for inclusion in regulated devices (SBOM, std-only
  supply chain)".
- **Trust in the from-scratch crypto** is the main objection in this market. The audit in STATUS
  item 5 moves up the priority list.
- **Owner decisions needed:**
  - the new crate `cameleer-dicom` and feature `dicom` (no external crates, so within the rules);
  - whether to pursue the Connectathon (a fee);
  - a managed offering, which would change the BAA position.

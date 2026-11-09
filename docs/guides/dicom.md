# DICOM imaging with Cameleer

> Cameleer routes medical images: it receives DICOM from modalities and PACS over DIMSE, filters and fixes tags, de-identifies with the PS3.15 profile, and forwards to PACS, cloud DICOMweb stores and AI models, in one small std-only binary.

**Cargo feature:** [`dicom`](build-features.md) (in the default build) · **Crate:** `cameleer-dicom`

Apache Camel has no DICOM component, so the names here are Cameleer's own, chosen to read like
Camel: the [`dicom`](../components/dicom.md) component for DIMSE, the
[`dicomweb`](../components/dicomweb.md) component for DICOMweb, the
[`dicom`](../dataformats/dicom.md), [`dicomJson`](../dataformats/dicomJson.md) and
[`dicomDeidentify`](../dataformats/dicomDeidentify.md) data formats, and the
[`dicom` language](../languages/dicom.md) with `${dicom.PatientID}` in Simple. HL7 v2
([`mllp`](../components/mllp.md)) and FHIR ([`fhir`](../components/fhir.md)) live in the same
binary.

## The building blocks

| you want to | use |
|---|---|
| receive images from a modality (be a Storage SCP) | `from: dicom:MYAE@0.0.0.0:11112` |
| send images to a PACS (Storage SCU) | `to: dicom:PACS@host:104` |
| store in Azure, Google Cloud, AWS or Orthanc over DICOMweb | `to: dicomweb:https://host/base` |
| query and retrieve from a PACS | `dicom:...?operation=find`, `move`, `get` |
| answer queries and retrieves from your own source | a `dicom:` consumer route that returns matches or instances |
| serve DICOMweb | `from: dicomweb:/dicom-web` |
| route on tags | `${dicom.Modality}`, `${dicom.(0018,0015)}`, `ReferencedSeriesSequence[0].SeriesInstanceUID` |
| change tags | `unmarshal: dicom`, edit the Value tree, `marshal: dicom` |
| de-identify | `marshal: {dicomDeidentify: {uidKey: ..., options: ...}}` |
| index metadata as JSON | `marshal: dicomJson` |

## Examples you can run

The files are in [`examples/medical/`](../../examples/medical/) and run end to end in the test
suite (`crates/cameleer-main/tests/dicom_examples.rs`).

- **Modality to cloud** (`modality-to-cloud.camel.yaml`): a Storage SCP for named modalities,
  keeps CT and MR, uploads with STOW-RS; when the cloud is down the instance is parked in a
  folder and a second route uploads it later, so the modality is never told "stored" for an
  image that was lost.
- **AI inference loop** (`ai-inference-loop.camel.yaml`): keeps the series a model is for,
  de-identifies it, sends each instance to an inference service, and posts the finding as a FHIR
  DiagnosticReport that points at the original study. The model sees no identity; the original
  Study Instance UID never leaves the gateway except to the hospital's FHIR server.
- **De-identification pipeline** (`deid-pipeline.camel.yaml`): a folder in, a folder out, with
  pseudonyms, shifted dates and an audit log.

## De-identification you can verify

`dicomDeidentify` applies Table E.1-1 of PS3.15 (edition 2026d) with its options. Replacement
UIDs, pseudonyms and date shifts are HMAC-SHA256 values under one secret key, so the output is
deterministic: two gateways with the same key produce the same identifiers, and a study split
across batches stays together. Each instance writes a record to the hash-chained audit log
(digests of input and output, the profile and options, no identity).

Run the de-identifier inside a confidential VM (AMD SEV-SNP, Intel TDX) with Cameleer's
attestation (`cameleer-attest`) to let a data owner check which binary and which policy saw the
identifiable data before they send it.

## Conformance summary

A full DICOM Conformance Statement in the PS3.2 template is still to be written; this is the
short form.

| area | supported |
|---|---|
| Verification | SCU and SCP |
| Storage | SCU and SCP for every Storage SOP class of PS3.4 (the SCP accepts all by default; private SOP classes too) |
| Query/Retrieve | Patient Root and Study Root FIND, MOVE and GET as SCU and SCP; Modality Worklist FIND as SCU and SCP |
| Not supported | Storage Commitment, MPPS, Instance Availability, C-CANCEL while a request runs, asynchronous operations |
| Transfer syntaxes | Implicit VR Little Endian, Explicit VR Little Endian, Deflated Explicit VR Little Endian, Explicit VR Big Endian (read, write, convert); every compressed syntax passed through unchanged (no transcoding to or from it) |
| Association | maximum PDU length negotiation (default 16384), role selection, User Identity (username, username and passcode) as SCU and accepted as SCP, called and calling AE checks, extended negotiation items read and ignored |
| Character sets | ISO_IR 6, 100, 101, 109, 110, 126, 127, 138, 144, 148, 166, 13, 192, GB18030, GBK and their ISO 2022 forms with IR 87, 159, 149, 58 |
| Security | TLS 1.2 and 1.3 (BCP 195 profiles), mutual TLS; PS3.15 Annex E basic profile and options except the pixel options; hash-chained audit records (not yet the PS3.15 A.5 ATNA message format) |
| DICOMweb | STOW-RS (Part 10 parts), QIDO-RS (all levels, `limit`, `offset`, `includefield`), WADO-RS (instances, metadata, bulk data); not frames, rendered, thumbnail or UPS-RS |

## How it was verified

- **Hermetic tests** (`cargo test -p cameleer-dicom`): codec round trips in every native transfer
  syntax, the Value tree, character sets against the byte sequences of PS3.5 Annexes H to K,
  DIMSE between Cameleer SCU and SCP (store, find, move, get, rejections, small PDUs, TLS),
  DICOMweb client against server, de-identification attribute by attribute against Table E.1-1.
- **Public test files**: 33 pydicom test files (NEMA WG-04 images, character set and deflate
  samples, RT, SR, private sequences) read and write back byte for byte
  (`scripts/fetch-dicom-testdata.sh`).
- **Live interop** (`scripts/dicom-interop.sh`, `CAMEL_DICOM_INTEROP=1`): DCMTK 3.6.7
  (`storescp`, `echoscu`, `storescu`, `findscu`, `getscu`, `movescu`, with and without TLS) and
  Orthanc 1.12.2 (as a PACS, and its DICOMweb plugin 1.16 as both server and client).
- **Not yet**: real cloud DICOM services (Azure, Google, AWS), dcm4che, real modalities, fuzzing of
  the Part 10 and PDU parsers, an independent security review, throughput benchmarks.

## Related

- [dicom](../components/dicom.md), [dicomweb](../components/dicomweb.md), [dicom data format](../dataformats/dicom.md), [dicomJson](../dataformats/dicomJson.md), [dicomDeidentify](../dataformats/dicomDeidentify.md), [dicom language](../languages/dicom.md)
- [mllp](../components/mllp.md), [fhir](../components/fhir.md), [Interop](../INTEROP.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.

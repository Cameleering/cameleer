# cameleer-health changelog

The health pack has its own version and change log, so a validator can read only what changed
in the medical code (docs/design/packs/domain-packs.md section 8.5).

## 0.6.0 (unreleased)

First release of the pack.

- HL7 v2 (`hl7` data format, `hl7terser` language) and the `mllp` component, moved from
  `cameleer-protocols`.
- FHIR R4 (`fhir` component, `fhirJson` and `fhirXml` data formats, `FhirMock`), moved from
  `cameleer-soap`.
- DICOM, new: Part 10 and the PS3.6 dictionary (2026d), specific character sets, the `dicom`,
  `dicomJson` and `dicomDeidentify` data formats, the `dicom` language and `${dicom.*}`, the
  `dicom` component (DIMSE SCU and SCP: C-ECHO, C-STORE, C-FIND, C-MOVE, C-GET, TLS), the
  `dicomweb` component (STOW-RS, QIDO-RS, WADO-RS client and server), PS3.15 Annex E
  de-identification with audit records.
- Crate features `hl7`, `fhir`, `dicom` (all default). Depends on cameleer-core, cameleer-http,
  cameleer-tls and (for `fhir`) cameleer-xml.
- Verified: hermetic tests; 33 public DICOM test files byte-exact; live against DCMTK 3.6.7 and
  Orthanc 1.12.2 with its DICOMweb plugin (7/7). Not verified: cloud DICOM services, dcm4che,
  real modalities.

# DICOM Conformance Statement: Cameleer health pack 0.6.0

> This statement describes the DICOM conformance of the Cameleer health pack 0.6.0 (`cameleer-health`): the DIMSE services it offers as SCU and SCP, its association and transfer syntax policies, DICOMweb, character sets, and its security and de-identification support, in the layout of DICOM PS3.2.

**Pack:** [health](../index.md) (feature `health`) · **Product:** Cameleer (`cameleer` binary built with the health pack, or `pmsudhi/cameleer:health`) · **Date:** 2026-11-10

The tables marked as generated are produced from the code by
`crates/cameleer-health/tests/dicom_conformance_statement.rs`, which fails when this document no
longer matches the implementation.

## 1. Overview

Cameleer is an integration engine. With the health pack it acts as a DICOM router: it receives
objects and queries from DICOM applications, hands them to user-defined routes, and sends objects
and queries to other DICOM applications and to DICOMweb services. It does not store objects
itself (it is not a PACS); what happens to a received object is decided by the route.

### Network services

| SOP class family | User of service (SCU) | Provider of service (SCP) |
|---|---|---|
| Verification | yes | yes |
| Storage (every Storage SOP class, section 4.2.1) | yes | yes |
| Patient Root Query/Retrieve: FIND, MOVE, GET | yes | yes (answered by the route) |
| Study Root Query/Retrieve: FIND, MOVE, GET | yes | yes (answered by the route) |
| Modality Worklist Information Model: FIND | yes | yes (answered by the route) |
| Storage Commitment Push Model | no | no |
| Modality Performed Procedure Step | no | no |
| Instance Availability Notification | no | no |

### Web services (PS3.18)

| service | user agent (client) | origin server |
|---|---|---|
| STOW-RS (`application/dicom` parts) | yes | yes |
| QIDO-RS (studies, series, instances) | yes | yes |
| WADO-RS (instances, metadata, bulk data) | yes | yes |
| WADO-RS rendered, thumbnail, frames | no | no (answers 501) |
| UPS-RS | no | no |

### Media storage

Not applicable: Cameleer reads and writes Part 10 files through routes (for example the `file`
component), but does not create or read DICOMDIR file-sets.

## 2. Introduction

### Revision history

| version | date | change |
|---|---|---|
| 0.6.0 | 2026-11-10 | First statement: the health pack's first release |

### Audience and remarks

For integrators and buyers who connect Cameleer to modalities, PACS, VNAs, AI services and
cloud DICOM stores. Interoperability was tested against DCMTK 3.6.7 (`storescp`, `echoscu`,
`storescu`, `findscu`, `getscu`, `movescu`, with and without TLS) and Orthanc 1.12.2 with its
DICOMweb plugin 1.16; it was not tested against dcm4che, commercial PACS, cloud DICOM services or
real modalities. Interoperability with any other product must be verified.

### Implementation identification

<!-- generated: identification -->
| item | value |
|---|---|
| Implementation Class UID | `2.25.201273755793964741390560873578799652301` |
| Implementation Version Name | `CAMELEER_060` |
| Pack | `cameleer-health` 0.6.0 |
| Data dictionary | DICOM PS3.6 2026d |
| De-identification table | PS3.15 Table E.1-1, 2026d |
<!-- end generated: identification -->

## 3. Networking

### 3.1 Implementation model

One Application Entity per endpoint:

- **SCP AE**: a `dicom:` consumer endpoint (`dicom:AET@host:port`). It listens on its port, accepts
  associations, answers C-ECHO itself, and turns each C-STORE, C-FIND, C-MOVE and C-GET into an
  exchange for the route. C-STORE gives the route the object as Part 10 bytes; C-FIND, C-MOVE and
  C-GET give it the identifier, and the route answers with the matches or the instances. For
  C-MOVE the SCP opens a new association to the move destination (from its configured AE table)
  and sends the instances with C-STORE; for C-GET it sends them on the same association.
- **SCU AE**: a `dicom:` producer endpoint (`dicom:AET@host:port?operation=...`). Each message the
  route sends opens one association, performs the operation (C-ECHO, C-STORE of one instance or
  a batch, C-FIND, C-MOVE, C-GET with C-STORE sub-operations received on the same association)
  and releases it.

Several SCP and SCU AEs can run in one process. The AE titles, ports and policies are set per
endpoint in the route (section 3.4).

### 3.2 AE specifications

#### 3.2.1 SOP classes

Verification (`1.2.840.10008.1.1`), the Query/Retrieve models (`1.2.840.10008.5.1.4.1.2.1.1`-`.3`,
`1.2.840.10008.5.1.4.1.2.2.1`-`.3`), Modality Worklist (`1.2.840.10008.5.1.4.31`) and these Storage
SOP classes, all as SCU and SCP:

<!-- generated: storage-sop-classes -->
| SOP class | UID | SCU | SCP |
|---|---|---|---|
| 12-lead ECG Waveform Storage | `1.2.840.10008.5.1.4.1.1.9.1.1` | yes | yes |
| Acquisition Context SR Storage | `1.2.840.10008.5.1.4.1.1.88.71` | yes | yes |
| Advanced Blending Presentation State Storage | `1.2.840.10008.5.1.4.1.1.11.8` | yes | yes |
| Ambulatory ECG Waveform Storage | `1.2.840.10008.5.1.4.1.1.9.1.3` | yes | yes |
| Arterial Pulse Waveform Storage | `1.2.840.10008.5.1.4.1.1.9.5.1` | yes | yes |
| Autorefraction Measurements Storage | `1.2.840.10008.5.1.4.1.1.78.2` | yes | yes |
| Basic Structured Display Storage | `1.2.840.10008.5.1.4.1.1.131` | yes | yes |
| Basic Text SR Storage | `1.2.840.10008.5.1.4.1.1.88.11` | yes | yes |
| Basic Voice Audio Waveform Storage | `1.2.840.10008.5.1.4.1.1.9.4.1` | yes | yes |
| Blending Softcopy Presentation State Storage | `1.2.840.10008.5.1.4.1.1.11.4` | yes | yes |
| Body Position Waveform Storage | `1.2.840.10008.5.1.4.1.1.9.8.1` | yes | yes |
| Breast Projection X-Ray Image Storage - For Presentation | `1.2.840.10008.5.1.4.1.1.13.1.4` | yes | yes |
| Breast Projection X-Ray Image Storage - For Processing | `1.2.840.10008.5.1.4.1.1.13.1.5` | yes | yes |
| Breast Tomosynthesis Image Storage | `1.2.840.10008.5.1.4.1.1.13.1.3` | yes | yes |
| C-Arm Photon-Electron Radiation Record Storage | `1.2.840.10008.5.1.4.1.1.481.19` | yes | yes |
| C-Arm Photon-Electron Radiation Storage | `1.2.840.10008.5.1.4.1.1.481.13` | yes | yes |
| CT Defined Procedure Protocol Storage | `1.2.840.10008.5.1.4.1.1.200.1` | yes | yes |
| CT Image Storage | `1.2.840.10008.5.1.4.1.1.2` | yes | yes |
| CT Image Storage - For Processing | `1.2.840.10008.5.1.4.1.1.2.3` | yes | yes |
| CT Performed Procedure Protocol Storage | `1.2.840.10008.5.1.4.1.1.200.2` | yes | yes |
| Cardiac Electrophysiology Waveform Storage | `1.2.840.10008.5.1.4.1.1.9.3.1` | yes | yes |
| Chest CAD SR Storage | `1.2.840.10008.5.1.4.1.1.88.65` | yes | yes |
| Colon CAD SR Storage | `1.2.840.10008.5.1.4.1.1.88.69` | yes | yes |
| Color Palette Storage | `1.2.840.10008.5.1.4.39.1` | yes | yes |
| Color Softcopy Presentation State Storage | `1.2.840.10008.5.1.4.1.1.11.2` | yes | yes |
| Compositing Planar MPR Volumetric Presentation State Storage | `1.2.840.10008.5.1.4.1.1.11.7` | yes | yes |
| Comprehensive 3D SR Storage | `1.2.840.10008.5.1.4.1.1.88.34` | yes | yes |
| Comprehensive SR Storage | `1.2.840.10008.5.1.4.1.1.88.33` | yes | yes |
| Computed Radiography Image Storage | `1.2.840.10008.5.1.4.1.1.1` | yes | yes |
| Confocal Microscopy Image Storage | `1.2.840.10008.5.1.4.1.1.77.1.8` | yes | yes |
| Confocal Microscopy Tiled Pyramidal Image Storage | `1.2.840.10008.5.1.4.1.1.77.1.9` | yes | yes |
| Content Assessment Results Storage | `1.2.840.10008.5.1.4.1.1.90.1` | yes | yes |
| Corneal Topography Map Storage | `1.2.840.10008.5.1.4.1.1.82.1` | yes | yes |
| DICOS 2D AIT Storage | `1.2.840.10008.5.1.4.1.1.501.4` | yes | yes |
| DICOS 3D AIT Storage | `1.2.840.10008.5.1.4.1.1.501.5` | yes | yes |
| DICOS CT Image Storage | `1.2.840.10008.5.1.4.1.1.501.1` | yes | yes |
| DICOS Digital X-Ray Image Storage - For Presentation | `1.2.840.10008.5.1.4.1.1.501.2.1` | yes | yes |
| DICOS Digital X-Ray Image Storage - For Processing | `1.2.840.10008.5.1.4.1.1.501.2.2` | yes | yes |
| DICOS Quadrupole Resonance (QR) Storage | `1.2.840.10008.5.1.4.1.1.501.6` | yes | yes |
| DICOS Threat Detection Report Storage | `1.2.840.10008.5.1.4.1.1.501.3` | yes | yes |
| Deformable Spatial Registration Storage | `1.2.840.10008.5.1.4.1.1.66.3` | yes | yes |
| Dermoscopic Photography Image Storage | `1.2.840.10008.5.1.4.1.1.77.1.7` | yes | yes |
| Digital Intra-Oral X-Ray Image Storage - For Presentation | `1.2.840.10008.5.1.4.1.1.1.3` | yes | yes |
| Digital Intra-Oral X-Ray Image Storage - For Processing | `1.2.840.10008.5.1.4.1.1.1.3.1` | yes | yes |
| Digital Mammography X-Ray Image Storage - For Presentation | `1.2.840.10008.5.1.4.1.1.1.2` | yes | yes |
| Digital Mammography X-Ray Image Storage - For Processing | `1.2.840.10008.5.1.4.1.1.1.2.1` | yes | yes |
| Digital X-Ray Image Storage - For Presentation | `1.2.840.10008.5.1.4.1.1.1.1` | yes | yes |
| Digital X-Ray Image Storage - For Processing | `1.2.840.10008.5.1.4.1.1.1.1.1` | yes | yes |
| Eddy Current Image Storage | `1.2.840.10008.5.1.4.1.1.601.1` | yes | yes |
| Eddy Current Multi-frame Image Storage | `1.2.840.10008.5.1.4.1.1.601.2` | yes | yes |
| Electromyogram Waveform Storage | `1.2.840.10008.5.1.4.1.1.9.7.2` | yes | yes |
| Electrooculogram Waveform Storage | `1.2.840.10008.5.1.4.1.1.9.7.3` | yes | yes |
| Encapsulated CDA Storage | `1.2.840.10008.5.1.4.1.1.104.2` | yes | yes |
| Encapsulated MTL Storage | `1.2.840.10008.5.1.4.1.1.104.5` | yes | yes |
| Encapsulated OBJ Storage | `1.2.840.10008.5.1.4.1.1.104.4` | yes | yes |
| Encapsulated PDF Storage | `1.2.840.10008.5.1.4.1.1.104.1` | yes | yes |
| Encapsulated STL Storage | `1.2.840.10008.5.1.4.1.1.104.3` | yes | yes |
| Enhanced CT Image Storage | `1.2.840.10008.5.1.4.1.1.2.1` | yes | yes |
| Enhanced CT Image Storage - For Processing | `1.2.840.10008.5.1.4.1.1.2.4` | yes | yes |
| Enhanced Continuous RT Image Storage | `1.2.840.10008.5.1.4.1.1.481.24` | yes | yes |
| Enhanced MR Color Image Storage | `1.2.840.10008.5.1.4.1.1.4.3` | yes | yes |
| Enhanced MR Image Storage | `1.2.840.10008.5.1.4.1.1.4.1` | yes | yes |
| Enhanced PET Image Storage | `1.2.840.10008.5.1.4.1.1.130` | yes | yes |
| Enhanced RT Image Storage | `1.2.840.10008.5.1.4.1.1.481.23` | yes | yes |
| Enhanced SR Storage | `1.2.840.10008.5.1.4.1.1.88.22` | yes | yes |
| Enhanced US Volume Storage | `1.2.840.10008.5.1.4.1.1.6.2` | yes | yes |
| Enhanced X-Ray Radiation Dose SR Storage | `1.2.840.10008.5.1.4.1.1.88.76` | yes | yes |
| Enhanced XA Image Storage | `1.2.840.10008.5.1.4.1.1.12.1.1` | yes | yes |
| Enhanced XRF Image Storage | `1.2.840.10008.5.1.4.1.1.12.2.1` | yes | yes |
| Extensible SR Storage | `1.2.840.10008.5.1.4.1.1.88.35` | yes | yes |
| General 32-bit ECG Waveform Storage | `1.2.840.10008.5.1.4.1.1.9.1.4` | yes | yes |
| General Audio Waveform Storage | `1.2.840.10008.5.1.4.1.1.9.4.2` | yes | yes |
| General ECG Waveform Storage | `1.2.840.10008.5.1.4.1.1.9.1.2` | yes | yes |
| Generic Implant Template Storage | `1.2.840.10008.5.1.4.43.1` | yes | yes |
| Grayscale Planar MPR Volumetric Presentation State Storage | `1.2.840.10008.5.1.4.1.1.11.6` | yes | yes |
| Grayscale Softcopy Presentation State Storage | `1.2.840.10008.5.1.4.1.1.11.1` | yes | yes |
| Hanging Protocol Storage | `1.2.840.10008.5.1.4.38.1` | yes | yes |
| Height Map Segmentation Storage | `1.2.840.10008.5.1.4.1.1.66.8` | yes | yes |
| Hemodynamic Waveform Storage | `1.2.840.10008.5.1.4.1.1.9.2.1` | yes | yes |
| Implant Assembly Template Storage | `1.2.840.10008.5.1.4.44.1` | yes | yes |
| Implant Template Group Storage | `1.2.840.10008.5.1.4.45.1` | yes | yes |
| Implantation Plan SR Storage | `1.2.840.10008.5.1.4.1.1.88.70` | yes | yes |
| Intraocular Lens Calculations Storage | `1.2.840.10008.5.1.4.1.1.78.8` | yes | yes |
| Intravascular Optical Coherence Tomography Image Storage - For Presentation | `1.2.840.10008.5.1.4.1.1.14.1` | yes | yes |
| Intravascular Optical Coherence Tomography Image Storage - For Processing | `1.2.840.10008.5.1.4.1.1.14.2` | yes | yes |
| Inventory Storage | `1.2.840.10008.5.1.4.1.1.201.1` | yes | yes |
| Keratometry Measurements Storage | `1.2.840.10008.5.1.4.1.1.78.3` | yes | yes |
| Key Object Selection Document Storage | `1.2.840.10008.5.1.4.1.1.88.59` | yes | yes |
| Label Map Segmentation Storage | `1.2.840.10008.5.1.4.1.1.66.7` | yes | yes |
| Legacy Converted Enhanced CT Image Storage | `1.2.840.10008.5.1.4.1.1.2.2` | yes | yes |
| Legacy Converted Enhanced CT Image Storage - For Processing | `1.2.840.10008.5.1.4.1.1.2.5` | yes | yes |
| Legacy Converted Enhanced MR Image Storage | `1.2.840.10008.5.1.4.1.1.4.4` | yes | yes |
| Legacy Converted Enhanced PET Image Storage | `1.2.840.10008.5.1.4.1.1.128.1` | yes | yes |
| Lensometry Measurements Storage | `1.2.840.10008.5.1.4.1.1.78.1` | yes | yes |
| MR Image Storage | `1.2.840.10008.5.1.4.1.1.4` | yes | yes |
| MR Spectroscopy Storage | `1.2.840.10008.5.1.4.1.1.4.2` | yes | yes |
| Macular Grid Thickness and Volume Report Storage | `1.2.840.10008.5.1.4.1.1.79.1` | yes | yes |
| Mammography CAD SR Storage | `1.2.840.10008.5.1.4.1.1.88.50` | yes | yes |
| Microscopy Bulk Simple Annotations Storage | `1.2.840.10008.5.1.4.1.1.91.1` | yes | yes |
| Multi-channel Respiratory Waveform Storage | `1.2.840.10008.5.1.4.1.1.9.6.2` | yes | yes |
| Multi-frame Grayscale Byte Secondary Capture Image Storage | `1.2.840.10008.5.1.4.1.1.7.2` | yes | yes |
| Multi-frame Grayscale Word Secondary Capture Image Storage | `1.2.840.10008.5.1.4.1.1.7.3` | yes | yes |
| Multi-frame Single Bit Secondary Capture Image Storage | `1.2.840.10008.5.1.4.1.1.7.1` | yes | yes |
| Multi-frame True Color Secondary Capture Image Storage | `1.2.840.10008.5.1.4.1.1.7.4` | yes | yes |
| Multiple Volume Rendering Volumetric Presentation State Storage | `1.2.840.10008.5.1.4.1.1.11.11` | yes | yes |
| Nuclear Medicine Image Storage | `1.2.840.10008.5.1.4.1.1.20` | yes | yes |
| Ophthalmic Axial Measurements Storage | `1.2.840.10008.5.1.4.1.1.78.7` | yes | yes |
| Ophthalmic Optical Coherence Tomography B-scan Volume Analysis Storage | `1.2.840.10008.5.1.4.1.1.77.1.5.8` | yes | yes |
| Ophthalmic Optical Coherence Tomography En Face Image Storage | `1.2.840.10008.5.1.4.1.1.77.1.5.7` | yes | yes |
| Ophthalmic Photography 16 Bit Image Storage | `1.2.840.10008.5.1.4.1.1.77.1.5.2` | yes | yes |
| Ophthalmic Photography 8 Bit Image Storage | `1.2.840.10008.5.1.4.1.1.77.1.5.1` | yes | yes |
| Ophthalmic Thickness Map Storage | `1.2.840.10008.5.1.4.1.1.81.1` | yes | yes |
| Ophthalmic Tomography Image Storage | `1.2.840.10008.5.1.4.1.1.77.1.5.4` | yes | yes |
| Ophthalmic Visual Field Static Perimetry Measurements Storage | `1.2.840.10008.5.1.4.1.1.80.1` | yes | yes |
| Parametric Map Storage | `1.2.840.10008.5.1.4.1.1.30` | yes | yes |
| Patient Radiation Dose SR Storage | `1.2.840.10008.5.1.4.1.1.88.73` | yes | yes |
| Performed Imaging Agent Administration SR Storage | `1.2.840.10008.5.1.4.1.1.88.75` | yes | yes |
| Photoacoustic Image Storage | `1.2.840.10008.5.1.4.1.1.6.3` | yes | yes |
| Planned Imaging Agent Administration SR Storage | `1.2.840.10008.5.1.4.1.1.88.74` | yes | yes |
| Positron Emission Tomography Image Storage | `1.2.840.10008.5.1.4.1.1.128` | yes | yes |
| Procedure Log Storage | `1.2.840.10008.5.1.4.1.1.88.40` | yes | yes |
| Protocol Approval Storage | `1.2.840.10008.5.1.4.1.1.200.3` | yes | yes |
| Pseudo-Color Softcopy Presentation State Storage | `1.2.840.10008.5.1.4.1.1.11.3` | yes | yes |
| RT Beams Delivery Instruction Storage | `1.2.840.10008.5.1.4.34.7` | yes | yes |
| RT Beams Treatment Record Storage | `1.2.840.10008.5.1.4.1.1.481.4` | yes | yes |
| RT Brachy Application Setup Delivery Instruction Storage | `1.2.840.10008.5.1.4.34.10` | yes | yes |
| RT Brachy Treatment Record Storage | `1.2.840.10008.5.1.4.1.1.481.6` | yes | yes |
| RT Dose Storage | `1.2.840.10008.5.1.4.1.1.481.2` | yes | yes |
| RT Image Storage | `1.2.840.10008.5.1.4.1.1.481.1` | yes | yes |
| RT Ion Beams Treatment Record Storage | `1.2.840.10008.5.1.4.1.1.481.9` | yes | yes |
| RT Ion Plan Storage | `1.2.840.10008.5.1.4.1.1.481.8` | yes | yes |
| RT Patient Position Acquisition Instruction Storage | `1.2.840.10008.5.1.4.1.1.481.25` | yes | yes |
| RT Physician Intent Storage | `1.2.840.10008.5.1.4.1.1.481.10` | yes | yes |
| RT Plan Storage | `1.2.840.10008.5.1.4.1.1.481.5` | yes | yes |
| RT Radiation Record Set Storage | `1.2.840.10008.5.1.4.1.1.481.16` | yes | yes |
| RT Radiation Salvage Record Storage | `1.2.840.10008.5.1.4.1.1.481.17` | yes | yes |
| RT Radiation Set Delivery Instruction Storage | `1.2.840.10008.5.1.4.1.1.481.21` | yes | yes |
| RT Radiation Set Storage | `1.2.840.10008.5.1.4.1.1.481.12` | yes | yes |
| RT Segment Annotation Storage | `1.2.840.10008.5.1.4.1.1.481.11` | yes | yes |
| RT Structure Set Storage | `1.2.840.10008.5.1.4.1.1.481.3` | yes | yes |
| RT Treatment Preparation Storage | `1.2.840.10008.5.1.4.1.1.481.22` | yes | yes |
| RT Treatment Summary Record Storage | `1.2.840.10008.5.1.4.1.1.481.7` | yes | yes |
| Radiopharmaceutical Radiation Dose SR Storage | `1.2.840.10008.5.1.4.1.1.88.68` | yes | yes |
| Raw Data Storage | `1.2.840.10008.5.1.4.1.1.66` | yes | yes |
| Real World Value Mapping Storage | `1.2.840.10008.5.1.4.1.1.67` | yes | yes |
| Respiratory Waveform Storage | `1.2.840.10008.5.1.4.1.1.9.6.1` | yes | yes |
| Robotic Radiation Record Storage | `1.2.840.10008.5.1.4.1.1.481.20` | yes | yes |
| Robotic-Arm Radiation Storage | `1.2.840.10008.5.1.4.1.1.481.15` | yes | yes |
| Routine Scalp Electroencephalogram Waveform Storage | `1.2.840.10008.5.1.4.1.1.9.7.1` | yes | yes |
| Secondary Capture Image Storage | `1.2.840.10008.5.1.4.1.1.7` | yes | yes |
| Segmentation Storage | `1.2.840.10008.5.1.4.1.1.66.4` | yes | yes |
| Segmented Volume Rendering Volumetric Presentation State Storage | `1.2.840.10008.5.1.4.1.1.11.10` | yes | yes |
| Simplified Adult Echo SR Storage | `1.2.840.10008.5.1.4.1.1.88.72` | yes | yes |
| Sleep Electroencephalogram Waveform Storage | `1.2.840.10008.5.1.4.1.1.9.7.4` | yes | yes |
| Spatial Fiducials Storage | `1.2.840.10008.5.1.4.1.1.66.2` | yes | yes |
| Spatial Registration Storage | `1.2.840.10008.5.1.4.1.1.66.1` | yes | yes |
| Spectacle Prescription Report Storage | `1.2.840.10008.5.1.4.1.1.78.6` | yes | yes |
| Stereometric Relationship Storage | `1.2.840.10008.5.1.4.1.1.77.1.5.3` | yes | yes |
| Subjective Refraction Measurements Storage | `1.2.840.10008.5.1.4.1.1.78.4` | yes | yes |
| Surface Scan Mesh Storage | `1.2.840.10008.5.1.4.1.1.68.1` | yes | yes |
| Surface Scan Point Cloud Storage | `1.2.840.10008.5.1.4.1.1.68.2` | yes | yes |
| Surface Segmentation Storage | `1.2.840.10008.5.1.4.1.1.66.5` | yes | yes |
| Thermography Image Storage | `1.2.840.10008.5.1.4.1.1.601.3` | yes | yes |
| Thermography Multi-frame Image Storage | `1.2.840.10008.5.1.4.1.1.601.4` | yes | yes |
| Tomotherapeutic Radiation Record Storage | `1.2.840.10008.5.1.4.1.1.481.18` | yes | yes |
| Tomotherapeutic Radiation Storage | `1.2.840.10008.5.1.4.1.1.481.14` | yes | yes |
| Tractography Results Storage | `1.2.840.10008.5.1.4.1.1.66.6` | yes | yes |
| Ultrasound Image Storage | `1.2.840.10008.5.1.4.1.1.6.1` | yes | yes |
| Ultrasound Multi-frame Image Storage | `1.2.840.10008.5.1.4.1.1.3.1` | yes | yes |
| Ultrasound Waveform Storage | `1.2.840.10008.5.1.4.1.1.601.5` | yes | yes |
| VL Endoscopic Image Storage | `1.2.840.10008.5.1.4.1.1.77.1.1` | yes | yes |
| VL Microscopic Image Storage | `1.2.840.10008.5.1.4.1.1.77.1.2` | yes | yes |
| VL Photographic Image Storage | `1.2.840.10008.5.1.4.1.1.77.1.4` | yes | yes |
| VL Slide-Coordinates Microscopic Image Storage | `1.2.840.10008.5.1.4.1.1.77.1.3` | yes | yes |
| VL Whole Slide Microscopy Image Storage | `1.2.840.10008.5.1.4.1.1.77.1.6` | yes | yes |
| Variable Modality LUT Softcopy Presentation State Storage | `1.2.840.10008.5.1.4.1.1.11.12` | yes | yes |
| Video Endoscopic Image Storage | `1.2.840.10008.5.1.4.1.1.77.1.1.1` | yes | yes |
| Video Microscopic Image Storage | `1.2.840.10008.5.1.4.1.1.77.1.2.1` | yes | yes |
| Video Photographic Image Storage | `1.2.840.10008.5.1.4.1.1.77.1.4.1` | yes | yes |
| Visual Acuity Measurements Storage | `1.2.840.10008.5.1.4.1.1.78.5` | yes | yes |
| Volume Rendering Volumetric Presentation State Storage | `1.2.840.10008.5.1.4.1.1.11.9` | yes | yes |
| Waveform Acquisition Presentation State Storage | `1.2.840.10008.5.1.4.1.1.9.100.2` | yes | yes |
| Waveform Annotation SR Storage | `1.2.840.10008.5.1.4.1.1.88.77` | yes | yes |
| Waveform Presentation State Storage | `1.2.840.10008.5.1.4.1.1.9.100.1` | yes | yes |
| Wide Field Ophthalmic Photography 3D Coordinates Image Storage | `1.2.840.10008.5.1.4.1.1.77.1.5.6` | yes | yes |
| Wide Field Ophthalmic Photography Stereographic Projection Image Storage | `1.2.840.10008.5.1.4.1.1.77.1.5.5` | yes | yes |
| X-Ray 3D Angiographic Image Storage | `1.2.840.10008.5.1.4.1.1.13.1.1` | yes | yes |
| X-Ray 3D Craniofacial Image Storage | `1.2.840.10008.5.1.4.1.1.13.1.2` | yes | yes |
| X-Ray Angiographic Image Storage | `1.2.840.10008.5.1.4.1.1.12.1` | yes | yes |
| X-Ray Radiation Dose SR Storage | `1.2.840.10008.5.1.4.1.1.88.67` | yes | yes |
| X-Ray Radiofluoroscopic Image Storage | `1.2.840.10008.5.1.4.1.1.12.2` | yes | yes |
| XA Defined Procedure Protocol Storage | `1.2.840.10008.5.1.4.1.1.200.7` | yes | yes |
| XA Performed Procedure Protocol Storage | `1.2.840.10008.5.1.4.1.1.200.8` | yes | yes |
| XA/XRF Grayscale Softcopy Presentation State Storage | `1.2.840.10008.5.1.4.1.1.11.5` | yes | yes |

194 Storage SOP classes (the non-retired ones of PS3.6 2026d). The SCP also accepts retired and private Storage SOP classes; the SCU sends any SOP class an instance has.
<!-- end generated: storage-sop-classes -->

The SCP's accepted set is configurable (`sopClasses`: `verification`, `storage`, `query`, `all`,
or UIDs and keywords).

#### 3.2.2 Association policies

| policy | SCP | SCU |
|---|---|---|
| Application context | `1.2.840.10008.3.1.1.1` | `1.2.840.10008.3.1.1.1` |
| Maximum PDU length received | 16384 by default (`maxPduLength`) | 16384 by default |
| PDU length sent | the peer's maximum, with even PDV fragments | the same |
| Number of associations | at most 50 at a time by default (`maxAssociations`); more are rejected as transient (source 3, reason 2) | one per operation |
| Asynchronous operations | not supported (one request at a time per association; a proposed window is ignored) | not proposed |
| Called AE title | any by default; with `checkCalledAet=true` others are rejected (reason 7) | the endpoint's AE title |
| Calling AE title | any by default; `acceptedCallingAets` restricts it (reason 3) | `callingAet`, default `CAMELEER` |
| Timeouts | `timeout` (60 s) for the association request and each message; idle associations closed after 5 x `timeout` | `connectTimeout` (30 s), `timeout` (60 s) for the answer and each response |
| Release | answers A-RELEASE-RQ; aborts on protocol errors | releases after the operation |

#### 3.2.3 Presentation contexts

**As SCP**, for each proposed abstract syntax in the accepted set, the first transfer syntax of
this list that the SCU proposed is accepted: Explicit VR Little Endian, Implicit VR Little
Endian, Deflated Explicit VR Little Endian, Explicit VR Big Endian, then (by default) any other
proposed transfer syntax, which covers every compressed syntax (`transferSyntaxes` changes the
list). Abstract syntaxes outside the set are rejected (result 3); a context without an acceptable
transfer syntax is rejected with result 4. SCP/SCU Role Selection items are accepted as proposed
(C-GET needs the SCP role for the SCU). SOP Class Extended Negotiation items are read and not
answered.

**As SCU:**

| operation | proposed contexts |
|---|---|
| C-ECHO, C-FIND, C-MOVE | the SOP class with Explicit VR Little Endian and Implicit VR Little Endian |
| C-STORE | per SOP class: the object's own transfer syntax first, plus Explicit and Implicit VR Little Endian (a compressed object gets a separate context with only its own syntax), plus any `transferSyntaxes` |
| C-GET | the Q/R model, plus each SOP class of `storageSopClasses` (29 common ones by default) with Explicit, Implicit and Deflated little endian and `transferSyntaxes`, each with SCP role selection |

When the peer accepts only another native syntax, the object is converted (no pixel data
changes); when an object is compressed and the peer accepts only native syntaxes, the store fails
with `DicomTranscodeException`: no pixel codecs are included.

#### 3.2.4 Transfer syntaxes

| transfer syntax | UID | read / write | as SCP | as SCU |
|---|---|---|---|---|
| Implicit VR Little Endian | `1.2.840.10008.1.2` | yes / yes | yes | yes |
| Explicit VR Little Endian | `1.2.840.10008.1.2.1` | yes / yes | yes | yes |
| Deflated Explicit VR Little Endian | `1.2.840.10008.1.2.1.99` | yes / yes | yes | yes |
| Explicit VR Big Endian (retired) | `1.2.840.10008.1.2.2` | yes / yes | yes | yes |
| every encapsulated (compressed) syntax | (JPEG, JPEG-LS, JPEG 2000, HTJ2K, RLE...) | passed through as fragments | yes | yes, unchanged |

#### 3.2.5 Statuses

| situation | status sent by the SCP |
|---|---|
| success | `0000` |
| the route failed a C-STORE | `A700` (Refused: Out of Resources) with the error as Error Comment, or the route's `CamelDicomStatus` |
| the route failed a C-FIND | `C000` with the error |
| unknown C-MOVE destination | `A801` |
| C-MOVE or C-GET with failed sub-operations | `B000` (some failed) or `A702` (all failed), with counts |
| unsupported command | `0211` |
| C-CANCEL-RQ | ignored: requests are answered in full |

As SCU, failure statuses raise `DicomStatusException`; warnings are reported in `CamelDicomStatus`.

### 3.3 Network interfaces

TCP/IP over IPv4 and IPv6 (bind addresses prefer IPv4 for `localhost`). DICOM over TLS is
available on any port (2762 is customary).

### 3.4 Configuration

Each endpoint is configured in its URI and in application properties: AE title, host, port,
accepted SOP classes and transfer syntaxes, calling AE list, C-MOVE destinations
(`moveDestinations=AET=host:port,...`), PDU size, timeouts, TLS. See
[the dicom component](../components/dicom.md).

### 3.5 DICOMweb

The `dicomweb` component (section 1) speaks PS3.18 over HTTP/1.1 and HTTPS: STOW-RS with
`multipart/related; type="application/dicom"` (200, 202, 409 with ReferencedSOPSequence,
FailedSOPSequence and Retrieve URLs; an instance posted to the wrong study fails with reason
`A900`), QIDO-RS at the study, series and instance levels with `limit`, `offset`, `includefield`
and `fuzzymatching`, WADO-RS of studies, series and instances (`transfer-syntax=` in `Accept`
converts between native syntaxes), metadata (bulk data over a threshold as `BulkDataURI`) and bulk
data. As a client it authenticates with Bearer tokens or Basic authentication. See
[the dicomweb component](../components/dicomweb.md).

## 4. Media interchange

Not applicable.

## 5. Support of character sets

Text values are decoded with the Specific Character Set of each data set (items inherit it) and
written back in the same set. Escape sequences of ISO 2022 are honoured on input and generated
only where needed on output.

<!-- generated: character-sets -->
| Specific Character Set (0008,0005) defined term | supported |
|---|---|
| `ISO_IR 6` | yes |
| `ISO_IR 100` | yes |
| `ISO_IR 101` | yes |
| `ISO_IR 109` | yes |
| `ISO_IR 110` | yes |
| `ISO_IR 126` | yes |
| `ISO_IR 127` | yes |
| `ISO_IR 138` | yes |
| `ISO_IR 144` | yes |
| `ISO_IR 148` | yes |
| `ISO_IR 166` | yes |
| `ISO_IR 203` | yes |
| `ISO_IR 13` | yes |
| `ISO_IR 192` | yes |
| `GB18030` | yes |
| `GBK` | yes |
| `ISO 2022 IR 6` | yes |
| `ISO 2022 IR 100` | yes |
| `ISO 2022 IR 101` | yes |
| `ISO 2022 IR 109` | yes |
| `ISO 2022 IR 110` | yes |
| `ISO 2022 IR 126` | yes |
| `ISO 2022 IR 127` | yes |
| `ISO 2022 IR 138` | yes |
| `ISO 2022 IR 144` | yes |
| `ISO 2022 IR 148` | yes |
| `ISO 2022 IR 166` | yes |
| `ISO 2022 IR 203` | yes |
| `ISO 2022 IR 13` | yes |
| `ISO 2022 IR 87` | yes |
| `ISO 2022 IR 159` | yes |
| `ISO 2022 IR 149` | yes |
| `ISO 2022 IR 58` | yes |
<!-- end generated: character-sets -->

## 6. Security

### 6.1 Security profiles

- **TLS:** TLS 1.2 and 1.3 from `cameleer-tls`, for the SCP and the SCU, with server and mutual
  authentication by X.509 certificates. Interoperability was tested with DCMTK's Non-Downgrading
  BCP 195 TLS profile in both directions. The BCP 195 profiles of PS3.15 B.9-B.13 are intended;
  a formal profile claim needs an independent review of the TLS implementation, which has not
  been done.
- **User Identity Negotiation:** the SCU sends a username (type 1) or username and passcode
  (type 2). The SCP accepts any user identity, returns a positive response when asked, and gives
  the username to the route (`CamelDicomUsername`), which decides; `requireUserIdentity=true`
  rejects associations without one. The SCP does not check passcodes itself.

### 6.2 Association-level access control

Called and calling AE title checks (section 3.2.2), TLS client certificates.

### 6.3 Application-level confidentiality

The `dicomDeidentify` data format implements the Basic Application Level Confidentiality Profile
of PS3.15 Annex E with the options Retain Safe Private, Retain UIDs, Retain Device Identity,
Retain Institution Identity, Retain Patient Characteristics, Retain Longitudinal Temporal
Information (Full Dates or Modified Dates), Clean Descriptors, Clean Structured Content and
Clean Graphics, from Table E.1-1 of the edition in the identification table. It does **not**
offer Clean Pixel Data or Clean Recognizable Visual Features, and refuses objects with Burned In
Annotation `YES` by default. It sets Patient Identity Removed (0012,0062) to `YES`, writes
De-identification Method (0012,0063) and the Method Code Sequence (0012,0064, CID 7050 codes
`113100`-`113112`), and Longitudinal Temporal Information Modified (0028,0303). Replacement UIDs
(root `2.25` by default) are HMAC-SHA256 values under a configured key, so they are consistent
across instances and runs. The Encrypted Attributes Sequence is not written. See
[dicomDeidentify](../dataformats/dicomDeidentify.md).

### 6.4 Audit

Each de-identified instance writes a record to Cameleer's hash-chained audit log (digests, the
profile and options, the new SOP Instance UID). DICOM Audit Trail messages (PS3.15 A.5) for IHE
ATNA are not generated yet.

## 7. Annexes

### 7.1 IOD contents

Cameleer does not create new SOP instances. Received objects are forwarded unchanged unless a
route changes them (tag edits, de-identification). Objects re-encoded in another native transfer
syntax keep every attribute; the file meta information is rebuilt (Implementation Class UID and
Version Name above, Source Application Entity Title of the sender).

### 7.2 Data dictionary of private attributes

None. Private attributes of received objects are kept and not interpreted (removed by
de-identification unless retained as safe).

### 7.3 Coded terminology and templates

De-identification uses CID 7050 (De-identification Method) codes `113100`-`113112`, scheme `DCM`.

### 7.4 Private SOP classes and transfer syntaxes

None defined. Private Storage SOP classes of other products are accepted and forwarded.

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.

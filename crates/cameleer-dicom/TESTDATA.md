# DICOM test data

## In the repository

The hermetic tests use only synthetic instances built in code
(`cameleer_dicom::testing::Instance`): made-up names, IDs and UIDs under a test root, and a 4x4
test pattern for pixel data. No real patient data, no third-party files.

## Downloaded (not committed): `scripts/fetch-dicom-testdata.sh`

The corpus test (`tests/corpus.rs`) runs on public files when they are present
(`CAMEL_DICOM_TESTDATA=<dir>`, or `target/dicom-testdata` after running the script). Each file
is pinned by SHA-256 in the script.

Source: the [pydicom](https://github.com/pydicom/pydicom) repository at tag `v3.0.1`, released
under the MIT licence ("Copyright (c) 2008-2020 Darcy Mason and pydicom contributors"). Its
`test_files/README.txt` records the origin of each file and states that images were downsized
and binary-edited "to replace anything that looks like a real patient name", with "no
restriction on using any of these files in this manner".

| files | origin (from pydicom's README) |
|---|---|
| `CT_small.dcm`, `MR_small.dcm` (and its `_implicit`, `_bigendian` versions made with DCMTK), `JPEG2000.dcm` | NEMA WG-04 sample images (`ftp://medical.nema.org/MEDICAL/Dicom/DataSets/WG04`), downsized |
| `image_dfl.dcm` | David Clunie's compressed samples (`http://www.dclunie.com/images/compressed/`) |
| `chr*.dcm` | David Clunie's character set samples (`http://www.dclunie.com/images/charset/SCS*`), downsized to 32x32 |
| `test-SR.dcm` | the OFFIS DCMTK DICOMscope SR example (`srdoc103.zip`, `test.dcm`) |
| `ExplVR_BigEndNoMeta.dcm`, `ExplVR_LitEndNoMeta.dcm`, `rtplan.dcm`, `rtstruct.dcm`, `rtdose.dcm`, `nested_priv_SQ.dcm`, `priv_SQ.dcm`, `UN_sequence.dcm`, `reportsi.dcm`, `no_meta_group_length.dcm` | pydicom's own test files (MIT) |

These files are not redistributed by Cameleer: the script downloads them from pydicom at test
time. The NEMA WG-04 sets themselves carry no explicit licence (see
`docs/design/medical/dicom-and-healthcare.md` section 5.4), which is a further reason they stay
out of git.

What the corpus test checks: every file parses; writing it again gives the same bytes (for the
deflated file, the same data set, since the DEFLATE stream is not the same as zlib's); the Value
tree converts back to the same bytes; the character set samples decode to the expected names and
encode back to the original bytes.

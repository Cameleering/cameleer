//! Test helpers: synthetic DICOM instances (no real patient data), used by the hermetic tests,
//! the examples and the embedded test servers.

use crate::codec::DicomFile;
use crate::object::{DataSet, Element};
use crate::tag::tags;
use crate::ts;
use crate::vr::Vr;

/// CT Image Storage.
pub const CT_IMAGE_STORAGE: &str = "1.2.840.10008.5.1.4.1.1.2";
/// Secondary Capture Image Storage.
pub const SECONDARY_CAPTURE_STORAGE: &str = "1.2.840.10008.5.1.4.1.1.7";

/// What a synthetic instance contains.
#[derive(Clone, Debug)]
pub struct Instance {
    pub patient_id: String,
    pub patient_name: String,
    pub study_uid: String,
    pub series_uid: String,
    pub instance_uid: String,
    pub sop_class: String,
    pub modality: String,
    pub study_date: String,
    pub accession: String,
    pub instance_number: u32,
}

impl Instance {
    /// A CT instance with the given numbers in its UIDs (`1.2.826.0.1.3680043.10.1234.<study>...`
    /// is a test root: the UIDs are synthetic).
    pub fn ct(patient: &str, study: u32, series: u32, instance: u32) -> Instance {
        Instance {
            patient_id: patient.to_string(),
            patient_name: format!("Test^{}", patient),
            study_uid: format!("1.2.826.0.1.3680043.10.1234.{}", study),
            series_uid: format!("1.2.826.0.1.3680043.10.1234.{}.{}", study, series),
            instance_uid: format!("1.2.826.0.1.3680043.10.1234.{}.{}.{}", study, series, instance),
            sop_class: CT_IMAGE_STORAGE.to_string(),
            modality: "CT".to_string(),
            study_date: "20260101".to_string(),
            accession: format!("ACC{}", study),
            instance_number: instance,
        }
    }

    /// The data set: patient, study, series and image modules with a 4x4 16-bit image.
    pub fn dataset(&self) -> DataSet {
        let s = |t, vr, v: &str| Element::string(t, vr, v);
        let mut ds = DataSet::new();
        ds.insert(s(tags::SPECIFIC_CHARACTER_SET, Vr::CS, "ISO_IR 100"));
        ds.insert(s(tags::IMAGE_TYPE, Vr::CS, "ORIGINAL\\PRIMARY\\AXIAL"));
        ds.insert(s(tags::SOP_CLASS_UID, Vr::UI, &self.sop_class));
        ds.insert(s(tags::SOP_INSTANCE_UID, Vr::UI, &self.instance_uid));
        ds.insert(s(tags::STUDY_DATE, Vr::DA, &self.study_date));
        ds.insert(s(0x0008_0021, Vr::DA, &self.study_date));
        ds.insert(s(0x0008_0030, Vr::TM, "101500"));
        ds.insert(s(0x0008_0050, Vr::SH, &self.accession));
        ds.insert(s(tags::MODALITY, Vr::CS, &self.modality));
        ds.insert(s(0x0008_0070, Vr::LO, "ACME Imaging"));
        ds.insert(s(0x0008_0080, Vr::LO, "General Hospital"));
        ds.insert(s(0x0008_0090, Vr::PN, "Referrer^Rita"));
        ds.insert(s(0x0008_1030, Vr::LO, "CT HEAD"));
        ds.insert(s(0x0008_103E, Vr::LO, "AXIAL 5MM"));
        ds.insert(s(0x0008_1090, Vr::LO, "ScannerModel"));
        ds.insert(s(tags::PATIENT_NAME, Vr::PN, &self.patient_name));
        ds.insert(s(tags::PATIENT_ID, Vr::LO, &self.patient_id));
        ds.insert(s(0x0010_0030, Vr::DA, "19700101"));
        ds.insert(s(0x0010_0040, Vr::CS, "O"));
        ds.insert(s(0x0010_1010, Vr::AS, "056Y"));
        ds.insert(s(0x0018_0050, Vr::DS, "5"));
        ds.insert(s(0x0018_1000, Vr::LO, "SN-0001"));
        ds.insert(s(tags::STUDY_INSTANCE_UID, Vr::UI, &self.study_uid));
        ds.insert(s(tags::SERIES_INSTANCE_UID, Vr::UI, &self.series_uid));
        ds.insert(s(0x0020_0010, Vr::SH, "S1"));
        ds.insert(s(0x0020_0011, Vr::IS, "1"));
        ds.insert(s(0x0020_0013, Vr::IS, &self.instance_number.to_string()));
        ds.insert(s(0x0020_0052, Vr::UI, &format!("{}.9", self.study_uid)));
        ds.insert(Element::us(0x0028_0002, 1));
        ds.insert(s(0x0028_0004, Vr::CS, "MONOCHROME2"));
        ds.insert(Element::us(0x0028_0010, 4));
        ds.insert(Element::us(0x0028_0011, 4));
        ds.insert(Element::us(0x0028_0100, 16));
        ds.insert(Element::us(0x0028_0101, 12));
        ds.insert(Element::us(0x0028_0102, 11));
        ds.insert(Element::us(0x0028_0103, 0));
        let px: Vec<u8> = (0u16..16).flat_map(|i| (i * 100).to_le_bytes()).collect();
        ds.insert(Element::bytes(tags::PIXEL_DATA, Vr::OW, px));
        ds
    }

    /// As a Part 10 file in Explicit VR Little Endian.
    pub fn part10(&self) -> Vec<u8> {
        self.part10_in(ts::EXPLICIT_VR_LITTLE_ENDIAN)
    }

    /// As a Part 10 file in a transfer syntax.
    pub fn part10_in(&self, transfer_syntax: &str) -> Vec<u8> {
        DicomFile::new(self.dataset(), transfer_syntax).to_bytes().expect("synthetic instance encodes")
    }
}

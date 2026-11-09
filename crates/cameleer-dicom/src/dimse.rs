//! DIMSE messages (PS3.7 section 9 and Annex E): command fields, status codes, SOP classes
//! of the services, and building command sets.

use cameleer_core::error::CamelError;

use crate::object::{DataSet, Element};
use crate::tag::tags;
use crate::vr::Vr;

/// Command Field (0000,0100) values.
pub mod command {
    pub const C_STORE_RQ: u16 = 0x0001;
    pub const C_STORE_RSP: u16 = 0x8001;
    pub const C_GET_RQ: u16 = 0x0010;
    pub const C_GET_RSP: u16 = 0x8010;
    pub const C_FIND_RQ: u16 = 0x0020;
    pub const C_FIND_RSP: u16 = 0x8020;
    pub const C_MOVE_RQ: u16 = 0x0021;
    pub const C_MOVE_RSP: u16 = 0x8021;
    pub const C_ECHO_RQ: u16 = 0x0030;
    pub const C_ECHO_RSP: u16 = 0x8030;
    pub const C_CANCEL_RQ: u16 = 0x0FFF;
    pub const N_EVENT_REPORT_RQ: u16 = 0x0100;
    pub const N_ACTION_RQ: u16 = 0x0130;
}

/// The name of a command field (`C-STORE-RQ`).
pub fn command_name(field: u16) -> String {
    use command::*;
    match field {
        C_STORE_RQ => "C-STORE-RQ".into(),
        C_STORE_RSP => "C-STORE-RSP".into(),
        C_GET_RQ => "C-GET-RQ".into(),
        C_GET_RSP => "C-GET-RSP".into(),
        C_FIND_RQ => "C-FIND-RQ".into(),
        C_FIND_RSP => "C-FIND-RSP".into(),
        C_MOVE_RQ => "C-MOVE-RQ".into(),
        C_MOVE_RSP => "C-MOVE-RSP".into(),
        C_ECHO_RQ => "C-ECHO-RQ".into(),
        C_ECHO_RSP => "C-ECHO-RSP".into(),
        C_CANCEL_RQ => "C-CANCEL-RQ".into(),
        other => format!("0x{:04X}", other),
    }
}

/// Command Data Set Type (0000,0800) meaning "no data set".
pub const NO_DATA_SET: u16 = 0x0101;
/// A value meaning "a data set follows" (any other value is allowed).
pub const DATA_SET_PRESENT: u16 = 0x0000;

/// Priority (0000,0700).
pub mod priority {
    pub const MEDIUM: u16 = 0x0000;
    pub const HIGH: u16 = 0x0001;
    pub const LOW: u16 = 0x0002;
}

/// Parse `LOW`, `MEDIUM`, `HIGH` (or a number).
pub fn parse_priority(s: &str) -> Option<u16> {
    match s.trim().to_ascii_uppercase().as_str() {
        "MEDIUM" | "" => Some(priority::MEDIUM),
        "HIGH" => Some(priority::HIGH),
        "LOW" => Some(priority::LOW),
        n => n.parse().ok(),
    }
}

/// SOP classes of the services.
pub mod sop {
    pub const VERIFICATION: &str = "1.2.840.10008.1.1";
    pub const PATIENT_ROOT_FIND: &str = "1.2.840.10008.5.1.4.1.2.1.1";
    pub const PATIENT_ROOT_MOVE: &str = "1.2.840.10008.5.1.4.1.2.1.2";
    pub const PATIENT_ROOT_GET: &str = "1.2.840.10008.5.1.4.1.2.1.3";
    pub const STUDY_ROOT_FIND: &str = "1.2.840.10008.5.1.4.1.2.2.1";
    pub const STUDY_ROOT_MOVE: &str = "1.2.840.10008.5.1.4.1.2.2.2";
    pub const STUDY_ROOT_GET: &str = "1.2.840.10008.5.1.4.1.2.2.3";
    pub const MODALITY_WORKLIST_FIND: &str = "1.2.840.10008.5.1.4.31";
    pub const STORAGE_COMMITMENT_PUSH: &str = "1.2.840.10008.1.20.1";
}

/// Query/Retrieve information model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryModel {
    PatientRoot,
    StudyRoot,
    /// Modality Worklist (C-FIND only).
    Mwl,
}

impl QueryModel {
    /// Parse `StudyRoot`, `PatientRoot`, `MWL` (case-insensitive).
    pub fn parse(s: &str) -> Option<QueryModel> {
        match s.trim().to_ascii_lowercase().as_str() {
            "studyroot" | "study" => Some(QueryModel::StudyRoot),
            "patientroot" | "patient" => Some(QueryModel::PatientRoot),
            "mwl" | "worklist" | "modalityworklist" => Some(QueryModel::Mwl),
            _ => None,
        }
    }

    pub fn find_uid(self) -> &'static str {
        match self {
            QueryModel::PatientRoot => sop::PATIENT_ROOT_FIND,
            QueryModel::StudyRoot => sop::STUDY_ROOT_FIND,
            QueryModel::Mwl => sop::MODALITY_WORKLIST_FIND,
        }
    }

    pub fn move_uid(self) -> Option<&'static str> {
        match self {
            QueryModel::PatientRoot => Some(sop::PATIENT_ROOT_MOVE),
            QueryModel::StudyRoot => Some(sop::STUDY_ROOT_MOVE),
            QueryModel::Mwl => None,
        }
    }

    pub fn get_uid(self) -> Option<&'static str> {
        match self {
            QueryModel::PatientRoot => Some(sop::PATIENT_ROOT_GET),
            QueryModel::StudyRoot => Some(sop::STUDY_ROOT_GET),
            QueryModel::Mwl => None,
        }
    }
}

/// Whether a SOP class is a Storage SOP class (by its registry name).
pub fn is_storage_sop_class(uid: &str) -> bool {
    match crate::dict::uid(uid) {
        Some(e) => e.kind == crate::dict::UidKind::SopClass && e.name.contains("Storage") && !e.name.contains("Commitment") && !e.name.contains("Retrieve") && !e.name.contains("Media"),
        // private storage classes are common; treat unknown SOP classes as storage
        None => !uid.starts_with("1.2.840.10008."),
    }
}

/// Every non-retired Storage SOP class of the registry.
pub fn storage_sop_classes() -> Vec<&'static str> {
    crate::dict::uids(crate::dict::UidKind::SopClass).into_iter().filter(|e| !e.retired && is_storage_sop_class(e.uid)).map(|e| e.uid).collect()
}

/// A set of common storage SOP classes for C-GET proposals (at most 128 presentation contexts
/// fit in one association).
pub const COMMON_STORAGE: &[&str] = &[
    "1.2.840.10008.5.1.4.1.1.1",       // CR
    "1.2.840.10008.5.1.4.1.1.1.1",     // DX presentation
    "1.2.840.10008.5.1.4.1.1.1.1.1",   // DX processing
    "1.2.840.10008.5.1.4.1.1.1.2",     // MG presentation
    "1.2.840.10008.5.1.4.1.1.1.2.1",   // MG processing
    "1.2.840.10008.5.1.4.1.1.2",       // CT
    "1.2.840.10008.5.1.4.1.1.2.1",     // Enhanced CT
    "1.2.840.10008.5.1.4.1.1.3.1",     // US multi-frame
    "1.2.840.10008.5.1.4.1.1.4",       // MR
    "1.2.840.10008.5.1.4.1.1.4.1",     // Enhanced MR
    "1.2.840.10008.5.1.4.1.1.6.1",     // US
    "1.2.840.10008.5.1.4.1.1.7",       // SC
    "1.2.840.10008.5.1.4.1.1.7.4",     // multi-frame true colour SC
    "1.2.840.10008.5.1.4.1.1.12.1",    // XA
    "1.2.840.10008.5.1.4.1.1.12.2",    // XRF
    "1.2.840.10008.5.1.4.1.1.20",      // NM
    "1.2.840.10008.5.1.4.1.1.66.4",    // Segmentation
    "1.2.840.10008.5.1.4.1.1.77.1.4",  // VL photographic
    "1.2.840.10008.5.1.4.1.1.88.11",   // Basic Text SR
    "1.2.840.10008.5.1.4.1.1.88.22",   // Enhanced SR
    "1.2.840.10008.5.1.4.1.1.88.33",   // Comprehensive SR
    "1.2.840.10008.5.1.4.1.1.88.34",   // Comprehensive 3D SR
    "1.2.840.10008.5.1.4.1.1.104.1",   // Encapsulated PDF
    "1.2.840.10008.5.1.4.1.1.128",     // PET
    "1.2.840.10008.5.1.4.1.1.481.1",   // RT Image
    "1.2.840.10008.5.1.4.1.1.481.2",   // RT Dose
    "1.2.840.10008.5.1.4.1.1.481.3",   // RT Structure Set
    "1.2.840.10008.5.1.4.1.1.481.5",   // RT Plan
    "1.2.840.10008.5.1.4.1.1.11.1",    // Grayscale Softcopy Presentation State
];

/// Classes of status codes (PS3.7 Annex C).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusClass {
    Success,
    Pending,
    Cancel,
    Warning,
    Failure,
}

/// Classify a status code.
pub fn status_class(s: u16) -> StatusClass {
    match s {
        0x0000 => StatusClass::Success,
        0xFF00 | 0xFF01 => StatusClass::Pending,
        0xFE00 => StatusClass::Cancel,
        0x0001 | 0x0107 | 0x0116 | 0xB000..=0xBFFF => StatusClass::Warning,
        _ => StatusClass::Failure,
    }
}

/// Status codes used here.
pub mod status {
    pub const SUCCESS: u16 = 0x0000;
    pub const PENDING: u16 = 0xFF00;
    pub const CANCEL: u16 = 0xFE00;
    pub const WARNING_SUBOPS: u16 = 0xB000;
    /// Refused: Out of Resources.
    pub const OUT_OF_RESOURCES: u16 = 0xA700;
    /// Refused: Out of Resources - unable to calculate number of matches.
    pub const OUT_OF_RESOURCES_MATCHES: u16 = 0xA701;
    /// Refused: Out of Resources - unable to perform sub-operations.
    pub const OUT_OF_RESOURCES_SUBOPS: u16 = 0xA702;
    /// Refused: Move Destination unknown.
    pub const MOVE_DESTINATION_UNKNOWN: u16 = 0xA801;
    /// Error: Data Set does not match SOP Class.
    pub const DATASET_MISMATCH: u16 = 0xA900;
    /// Error: Cannot understand / Unable to process.
    pub const CANNOT_UNDERSTAND: u16 = 0xC000;
    /// Failure: SOP class not supported.
    pub const SOP_CLASS_NOT_SUPPORTED: u16 = 0x0122;
    /// Failure: Unrecognized operation.
    pub const UNRECOGNIZED_OPERATION: u16 = 0x0211;
    /// Failure: Processing failure.
    pub const PROCESSING_FAILURE: u16 = 0x0110;
}

/// Exception kind for a DIMSE failure status.
pub const DICOM_STATUS_EXCEPTION: &str = "DicomStatusException";

/// The exception for a failure status.
pub fn status_error(op: &str, s: u16, comment: Option<&str>) -> CamelError {
    let mut msg = format!("{} failed with status 0x{:04X}", op, s);
    if let Some(c) = comment.filter(|c| !c.is_empty()) {
        msg.push_str(": ");
        msg.push_str(c);
    }
    CamelError::new(DICOM_STATUS_EXCEPTION, msg)
}

/// Start a command set: the group length (recomputed when written), Command Field and the
/// Command Data Set Type.
pub fn command_set(field: u16, has_data: bool) -> DataSet {
    let mut c = DataSet::new();
    c.insert(Element::ul(tags::COMMAND_GROUP_LENGTH, 0));
    c.insert(Element::us(tags::COMMAND_FIELD, field));
    c.insert(Element::us(tags::COMMAND_DATA_SET_TYPE, if has_data { DATA_SET_PRESENT } else { NO_DATA_SET }));
    c
}

/// A UI element for a command (padded with NUL).
pub fn ui(tag: u32, uid: &str) -> Element {
    let mut b = uid.as_bytes().to_vec();
    if b.len() % 2 == 1 {
        b.push(0);
    }
    Element::bytes(tag, Vr::UI, b)
}

/// An AE element.
pub fn ae(tag: u32, title: &str) -> Element {
    Element::string(tag, Vr::AE, title)
}

/// The Command Field of a command set.
pub fn field(c: &DataSet) -> u16 {
    c.int(tags::COMMAND_FIELD).unwrap_or(0) as u16
}

/// Whether a command announces a data set.
pub fn has_data_set(c: &DataSet) -> bool {
    c.int(tags::COMMAND_DATA_SET_TYPE).map(|v| v as u16 != NO_DATA_SET).unwrap_or(false)
}

/// The Status of a response.
pub fn status_of(c: &DataSet) -> u16 {
    c.int(tags::STATUS).unwrap_or(0xFFFF) as u16
}

/// A response command for a request: Command Field, Message ID Being Responded To, Affected
/// SOP Class (and Instance) UID, Status.
pub fn response(rq: &DataSet, field: u16, status: u16, has_data: bool) -> DataSet {
    let mut c = command_set(field, has_data);
    if let Some(id) = rq.int(tags::MESSAGE_ID) {
        c.insert(Element::us(tags::MESSAGE_ID_BEING_RESPONDED_TO, id as u16));
    }
    let class = rq.string(tags::AFFECTED_SOP_CLASS_UID).filter(|s| !s.is_empty()).or_else(|| rq.string(tags::REQUESTED_SOP_CLASS_UID));
    if let Some(cl) = class {
        c.insert(ui(tags::AFFECTED_SOP_CLASS_UID, &cl));
    }
    if let Some(i) = rq.string(tags::AFFECTED_SOP_INSTANCE_UID).filter(|s| !s.is_empty()) {
        c.insert(ui(tags::AFFECTED_SOP_INSTANCE_UID, &i));
    }
    c.insert(Element::us(tags::STATUS, status));
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_and_commands() {
        assert_eq!(status_class(0), StatusClass::Success);
        assert_eq!(status_class(0xFF01), StatusClass::Pending);
        assert_eq!(status_class(0xB007), StatusClass::Warning);
        assert_eq!(status_class(0xA700), StatusClass::Failure);
        assert_eq!(status_class(0xC123), StatusClass::Failure);
        let mut rq = command_set(command::C_ECHO_RQ, false);
        rq.insert(Element::us(tags::MESSAGE_ID, 7));
        rq.insert(ui(tags::AFFECTED_SOP_CLASS_UID, sop::VERIFICATION));
        let rsp = response(&rq, command::C_ECHO_RSP, 0, false);
        assert_eq!(rsp.int(tags::MESSAGE_ID_BEING_RESPONDED_TO), Some(7));
        assert_eq!(rsp.string(tags::AFFECTED_SOP_CLASS_UID).unwrap(), sop::VERIFICATION);
        assert!(!has_data_set(&rsp));
        assert!(is_storage_sop_class("1.2.840.10008.5.1.4.1.1.2"));
        assert!(!is_storage_sop_class(sop::STORAGE_COMMITMENT_PUSH));
        assert!(storage_sop_classes().len() > 80);
        assert_eq!(QueryModel::parse("StudyRoot"), Some(QueryModel::StudyRoot));
    }
}

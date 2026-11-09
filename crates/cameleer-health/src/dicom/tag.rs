//! Tags: `(group << 16) | element`, parsing and formatting, and the tags this crate uses.

use cameleer_core::error::{CamelError, Result};

/// Build a tag from its group and element numbers.
pub const fn tag(group: u16, element: u16) -> u32 {
    ((group as u32) << 16) | element as u32
}

/// The group number.
pub const fn group(tag: u32) -> u16 {
    (tag >> 16) as u16
}

/// The element number.
pub const fn element(tag: u32) -> u16 {
    tag as u16
}

/// Whether a tag is private (odd group, other than groups 0001, 0003, 0005, 0007 and FFFF).
pub fn is_private(tag: u32) -> bool {
    let g = group(tag);
    g % 2 == 1 && g > 0x0008 && g != 0xFFFF
}

/// Whether a tag is a private creator element (gggg,0010-00FF) in a private group.
pub fn is_private_creator(tag: u32) -> bool {
    is_private(tag) && (0x0010..=0x00FF).contains(&element(tag))
}

/// Whether a tag is a group length element (gggg,0000).
pub fn is_group_length(tag: u32) -> bool {
    element(tag) == 0
}

/// `(GGGG,EEEE)` in upper-case hex.
pub fn format(tag: u32) -> String {
    format!("({:04X},{:04X})", group(tag), element(tag))
}

/// `GGGGEEEE` in upper-case hex, the key of the DICOM JSON model.
pub fn format_hex(tag: u32) -> String {
    format!("{:08X}", tag)
}

/// Parse `(0010,0020)`, `0010,0020` or `00100020` (hex digits, any case).
pub fn parse(s: &str) -> Result<u32> {
    try_parse(s).ok_or_else(|| CamelError::new("IllegalArgumentException", format!("Not a DICOM tag: {}", s)))
}

/// Like [`parse`], `None` when the text is not a tag.
pub fn try_parse(s: &str) -> Option<u32> {
    let s = s.trim();
    let s = s.strip_prefix('(').and_then(|r| r.strip_suffix(')')).unwrap_or(s);
    let hex = |t: &str| -> Option<u32> {
        if t.is_empty() || !t.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        u32::from_str_radix(t, 16).ok()
    };
    if let Some((g, e)) = s.split_once(',') {
        let (g, e) = (g.trim(), e.trim());
        if g.len() != 4 || e.len() != 4 {
            return None;
        }
        return Some((hex(g)? << 16) | hex(e)?);
    }
    if s.len() == 8 {
        return hex(s);
    }
    None
}

/// Tags named in this crate.
pub mod tags {
    use super::tag;
    pub const COMMAND_GROUP_LENGTH: u32 = tag(0x0000, 0x0000);
    pub const AFFECTED_SOP_CLASS_UID: u32 = tag(0x0000, 0x0002);
    pub const REQUESTED_SOP_CLASS_UID: u32 = tag(0x0000, 0x0003);
    pub const COMMAND_FIELD: u32 = tag(0x0000, 0x0100);
    pub const MESSAGE_ID: u32 = tag(0x0000, 0x0110);
    pub const MESSAGE_ID_BEING_RESPONDED_TO: u32 = tag(0x0000, 0x0120);
    pub const MOVE_DESTINATION: u32 = tag(0x0000, 0x0600);
    pub const PRIORITY: u32 = tag(0x0000, 0x0700);
    pub const COMMAND_DATA_SET_TYPE: u32 = tag(0x0000, 0x0800);
    pub const STATUS: u32 = tag(0x0000, 0x0900);
    pub const OFFENDING_ELEMENT: u32 = tag(0x0000, 0x0901);
    pub const ERROR_COMMENT: u32 = tag(0x0000, 0x0902);
    pub const AFFECTED_SOP_INSTANCE_UID: u32 = tag(0x0000, 0x1000);
    pub const REQUESTED_SOP_INSTANCE_UID: u32 = tag(0x0000, 0x1001);
    pub const NUMBER_OF_REMAINING_SUBOPERATIONS: u32 = tag(0x0000, 0x1020);
    pub const NUMBER_OF_COMPLETED_SUBOPERATIONS: u32 = tag(0x0000, 0x1021);
    pub const NUMBER_OF_FAILED_SUBOPERATIONS: u32 = tag(0x0000, 0x1022);
    pub const NUMBER_OF_WARNING_SUBOPERATIONS: u32 = tag(0x0000, 0x1023);
    pub const MOVE_ORIGINATOR_AE_TITLE: u32 = tag(0x0000, 0x1030);
    pub const MOVE_ORIGINATOR_MESSAGE_ID: u32 = tag(0x0000, 0x1031);

    pub const FILE_META_GROUP_LENGTH: u32 = tag(0x0002, 0x0000);
    pub const FILE_META_VERSION: u32 = tag(0x0002, 0x0001);
    pub const MEDIA_STORAGE_SOP_CLASS_UID: u32 = tag(0x0002, 0x0002);
    pub const MEDIA_STORAGE_SOP_INSTANCE_UID: u32 = tag(0x0002, 0x0003);
    pub const TRANSFER_SYNTAX_UID: u32 = tag(0x0002, 0x0010);
    pub const IMPLEMENTATION_CLASS_UID: u32 = tag(0x0002, 0x0012);
    pub const IMPLEMENTATION_VERSION_NAME: u32 = tag(0x0002, 0x0013);
    pub const SOURCE_APPLICATION_ENTITY_TITLE: u32 = tag(0x0002, 0x0016);

    pub const SPECIFIC_CHARACTER_SET: u32 = tag(0x0008, 0x0005);
    pub const IMAGE_TYPE: u32 = tag(0x0008, 0x0008);
    pub const SOP_CLASS_UID: u32 = tag(0x0008, 0x0016);
    pub const SOP_INSTANCE_UID: u32 = tag(0x0008, 0x0018);
    pub const STUDY_DATE: u32 = tag(0x0008, 0x0020);
    pub const QUERY_RETRIEVE_LEVEL: u32 = tag(0x0008, 0x0052);
    pub const RETRIEVE_URL: u32 = tag(0x0008, 0x1190);
    pub const MODALITY: u32 = tag(0x0008, 0x0060);
    pub const FAILED_SOP_SEQUENCE: u32 = tag(0x0008, 0x1198);
    pub const REFERENCED_SOP_SEQUENCE: u32 = tag(0x0008, 0x1199);
    pub const WARNING_REASON: u32 = tag(0x0008, 0x1196);
    pub const FAILURE_REASON: u32 = tag(0x0008, 0x1197);
    pub const REFERENCED_SOP_CLASS_UID: u32 = tag(0x0008, 0x1150);
    pub const REFERENCED_SOP_INSTANCE_UID: u32 = tag(0x0008, 0x1155);
    pub const PATIENT_NAME: u32 = tag(0x0010, 0x0010);
    pub const PATIENT_ID: u32 = tag(0x0010, 0x0020);
    pub const PATIENT_IDENTITY_REMOVED: u32 = tag(0x0012, 0x0062);
    pub const DEIDENTIFICATION_METHOD: u32 = tag(0x0012, 0x0063);
    pub const DEIDENTIFICATION_METHOD_CODE_SEQUENCE: u32 = tag(0x0012, 0x0064);
    pub const LONGITUDINAL_TEMPORAL_INFORMATION_MODIFIED: u32 = tag(0x0028, 0x0303);
    pub const STUDY_INSTANCE_UID: u32 = tag(0x0020, 0x000D);
    pub const SERIES_INSTANCE_UID: u32 = tag(0x0020, 0x000E);
    pub const PIXEL_DATA: u32 = tag(0x7FE0, 0x0010);
    pub const ITEM: u32 = tag(0xFFFE, 0xE000);
    pub const ITEM_DELIMITATION: u32 = tag(0xFFFE, 0xE00D);
    pub const SEQUENCE_DELIMITATION: u32 = tag(0xFFFE, 0xE0DD);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_format() {
        assert_eq!(parse("(0010,0020)").unwrap(), 0x0010_0020);
        assert_eq!(parse("0010,0020").unwrap(), 0x0010_0020);
        assert_eq!(parse("7fe00010").unwrap(), 0x7FE0_0010);
        assert!(try_parse("PatientID").is_none());
        assert!(try_parse("(10,20)").is_none());
        assert_eq!(format(0x0010_0020), "(0010,0020)");
        assert_eq!(format_hex(0x0010_0020), "00100020");
        assert!(is_private(0x0009_0010) && is_private_creator(0x0009_0010) && !is_private_creator(0x0009_1001));
        assert!(!is_private(0xFFFE_E000));
    }
}

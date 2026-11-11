//! Transfer syntaxes (PS3.5 section 10 and Annex A).

/// Implicit VR Little Endian, the default every implementation accepts.
pub const IMPLICIT_VR_LITTLE_ENDIAN: &str = "1.2.840.10008.1.2";
/// Explicit VR Little Endian.
pub const EXPLICIT_VR_LITTLE_ENDIAN: &str = "1.2.840.10008.1.2.1";
/// Deflated Explicit VR Little Endian.
pub const DEFLATED_EXPLICIT_VR_LITTLE_ENDIAN: &str = "1.2.840.10008.1.2.1.99";
/// Explicit VR Big Endian (retired; read only in practice).
pub const EXPLICIT_VR_BIG_ENDIAN: &str = "1.2.840.10008.1.2.2";
/// JPEG Baseline (Process 1).
pub const JPEG_BASELINE: &str = "1.2.840.10008.1.2.4.50";
/// JPEG Lossless, Non-Hierarchical, First-Order Prediction (Process 14, SV1).
pub const JPEG_LOSSLESS_SV1: &str = "1.2.840.10008.1.2.4.70";
/// JPEG-LS Lossless.
pub const JPEG_LS_LOSSLESS: &str = "1.2.840.10008.1.2.4.80";
/// JPEG 2000 Lossless Only.
pub const JPEG_2000_LOSSLESS: &str = "1.2.840.10008.1.2.4.90";
/// JPEG 2000.
pub const JPEG_2000: &str = "1.2.840.10008.1.2.4.91";
/// RLE Lossless.
pub const RLE_LOSSLESS: &str = "1.2.840.10008.1.2.5";

/// How a data set is encoded: VR explicit or implicit, byte order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Encoding {
    pub explicit_vr: bool,
    pub big_endian: bool,
}

impl Encoding {
    pub const IMPLICIT_LE: Encoding = Encoding { explicit_vr: false, big_endian: false };
    pub const EXPLICIT_LE: Encoding = Encoding { explicit_vr: true, big_endian: false };
    pub const EXPLICIT_BE: Encoding = Encoding { explicit_vr: true, big_endian: true };
}

/// What a transfer syntax UID means for the codec.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferSyntax {
    pub uid: String,
    pub encoding: Encoding,
    /// The data set after the file meta information is DEFLATE-compressed.
    pub deflated: bool,
    /// Pixel data is encapsulated (a compressed transfer syntax).
    pub encapsulated: bool,
}

impl TransferSyntax {
    /// The properties of a transfer syntax UID. Unknown UIDs are taken as an encapsulated
    /// (compressed) Explicit VR Little Endian syntax, which is what every compressed transfer
    /// syntax is (PS3.5 A.4).
    pub fn from_uid(uid: &str) -> TransferSyntax {
        let uid = uid.trim_end_matches(['\0', ' ']);
        let (encoding, deflated, encapsulated) = match uid {
            IMPLICIT_VR_LITTLE_ENDIAN => (Encoding::IMPLICIT_LE, false, false),
            EXPLICIT_VR_LITTLE_ENDIAN => (Encoding::EXPLICIT_LE, false, false),
            DEFLATED_EXPLICIT_VR_LITTLE_ENDIAN => (Encoding::EXPLICIT_LE, true, false),
            EXPLICIT_VR_BIG_ENDIAN => (Encoding::EXPLICIT_BE, false, false),
            // Encapsulated Uncompressed Explicit VR Little Endian
            "1.2.840.10008.1.2.1.98" => (Encoding::EXPLICIT_LE, false, true),
            _ => (Encoding::EXPLICIT_LE, false, true),
        };
        TransferSyntax { uid: uid.to_string(), encoding, deflated, encapsulated }
    }

    /// Whether this is one of the uncompressed native syntaxes (implicit LE, explicit LE,
    /// deflated, explicit BE).
    pub fn is_native(&self) -> bool {
        !self.encapsulated
    }

    /// A short name for messages (the registry name where known).
    pub fn name(&self) -> String {
        crate::dicom::dict::uid(&self.uid).map(|e| e.name.to_string()).unwrap_or_else(|| self.uid.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_syntaxes() {
        assert_eq!(TransferSyntax::from_uid(IMPLICIT_VR_LITTLE_ENDIAN).encoding, Encoding::IMPLICIT_LE);
        assert!(TransferSyntax::from_uid(DEFLATED_EXPLICIT_VR_LITTLE_ENDIAN).deflated);
        assert!(TransferSyntax::from_uid(EXPLICIT_VR_BIG_ENDIAN).encoding.big_endian);
        let j = TransferSyntax::from_uid("1.2.840.10008.1.2.4.50\0");
        assert!(j.encapsulated && j.encoding.explicit_vr && j.uid == JPEG_BASELINE);
    }
}

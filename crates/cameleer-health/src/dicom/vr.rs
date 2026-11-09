//! Value Representations (PS3.5 section 6.2).

use std::fmt;

/// A DICOM Value Representation.
#[allow(clippy::upper_case_acronyms)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
pub enum Vr {
    AE,
    AS,
    AT,
    CS,
    DA,
    DS,
    DT,
    FL,
    FD,
    IS,
    LO,
    LT,
    OB,
    OD,
    OF,
    OL,
    OV,
    OW,
    PN,
    SH,
    SL,
    SQ,
    SS,
    ST,
    SV,
    TM,
    UC,
    UI,
    UL,
    UN,
    UR,
    US,
    UT,
    UV,
}

/// Every VR, in alphabetical order.
pub const ALL: [Vr; 34] = [
    Vr::AE,
    Vr::AS,
    Vr::AT,
    Vr::CS,
    Vr::DA,
    Vr::DS,
    Vr::DT,
    Vr::FL,
    Vr::FD,
    Vr::IS,
    Vr::LO,
    Vr::LT,
    Vr::OB,
    Vr::OD,
    Vr::OF,
    Vr::OL,
    Vr::OV,
    Vr::OW,
    Vr::PN,
    Vr::SH,
    Vr::SL,
    Vr::SQ,
    Vr::SS,
    Vr::ST,
    Vr::SV,
    Vr::TM,
    Vr::UC,
    Vr::UI,
    Vr::UL,
    Vr::UN,
    Vr::UR,
    Vr::US,
    Vr::UT,
    Vr::UV,
];

impl Vr {
    /// The two-letter code.
    pub fn as_str(self) -> &'static str {
        use Vr::*;
        match self {
            AE => "AE",
            AS => "AS",
            AT => "AT",
            CS => "CS",
            DA => "DA",
            DS => "DS",
            DT => "DT",
            FL => "FL",
            FD => "FD",
            IS => "IS",
            LO => "LO",
            LT => "LT",
            OB => "OB",
            OD => "OD",
            OF => "OF",
            OL => "OL",
            OV => "OV",
            OW => "OW",
            PN => "PN",
            SH => "SH",
            SL => "SL",
            SQ => "SQ",
            SS => "SS",
            ST => "ST",
            SV => "SV",
            TM => "TM",
            UC => "UC",
            UI => "UI",
            UL => "UL",
            UN => "UN",
            UR => "UR",
            US => "US",
            UT => "UT",
            UV => "UV",
        }
    }

    /// Parse a two-letter code (`None` for anything else).
    pub fn parse(s: &str) -> Option<Vr> {
        Self::from_bytes(s.as_bytes())
    }

    /// Parse the two VR bytes of an explicit-VR element.
    pub fn from_bytes(b: &[u8]) -> Option<Vr> {
        if b.len() != 2 {
            return None;
        }
        ALL.iter().copied().find(|v| v.as_str().as_bytes() == b)
    }

    /// Whether explicit-VR encoding uses two reserved bytes and a 32-bit length (PS3.5 7.1.2).
    pub fn has_long_length(self) -> bool {
        use Vr::*;
        matches!(self, OB | OD | OF | OL | OV | OW | SQ | SV | UC | UN | UR | UT | UV)
    }

    /// Whether the value is character data (decoded with the specific character set where
    /// [`Vr::uses_charset`]).
    pub fn is_string(self) -> bool {
        use Vr::*;
        matches!(self, AE | AS | CS | DA | DS | DT | IS | LO | LT | PN | SH | ST | TM | UC | UI | UR | UT)
    }

    /// Whether the value is affected by Specific Character Set (0008,0005) (PS3.5 6.1.2.3).
    pub fn uses_charset(self) -> bool {
        use Vr::*;
        matches!(self, LO | LT | PN | SH | ST | UC | UT)
    }

    /// Whether the value can hold several values separated by a backslash.
    pub fn is_multi_valued_string(self) -> bool {
        use Vr::*;
        self.is_string() && !matches!(self, LT | ST | UT | UR)
    }

    /// The padding byte for odd-length values: NUL for UI and the binary VRs, space for text.
    pub fn pad_byte(self) -> u8 {
        if self.is_string() && self != Vr::UI {
            b' '
        } else {
            0
        }
    }

    /// Size in bytes of one binary number (0 for the other VRs).
    pub fn number_size(self) -> usize {
        use Vr::*;
        match self {
            US | SS => 2,
            UL | SL | FL | AT => 4,
            FD | SV | UV => 8,
            _ => 0,
        }
    }

    /// Whether the value is a byte or word stream (OB, OD, OF, OL, OV, OW, UN).
    pub fn is_bulk(self) -> bool {
        use Vr::*;
        matches!(self, OB | OD | OF | OL | OV | OW | UN)
    }

    /// The size of one word for byte swapping between little and big endian (1 for OB/UN).
    pub fn swap_size(self) -> usize {
        use Vr::*;
        match self {
            OW | US | SS | AT => 2,
            OF | OL | UL | SL | FL => 4,
            OD | OV | FD | SV | UV => 8,
            _ => 1,
        }
    }
}

impl fmt::Display for Vr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_round_trip() {
        for v in ALL {
            assert_eq!(Vr::parse(v.as_str()), Some(v));
        }
        assert_eq!(Vr::parse("XX"), None);
        assert!(Vr::OB.has_long_length() && !Vr::US.has_long_length() && Vr::UC.has_long_length());
        assert_eq!(Vr::UI.pad_byte(), 0);
        assert_eq!(Vr::LO.pad_byte(), b' ');
    }
}

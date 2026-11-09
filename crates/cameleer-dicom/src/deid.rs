//! De-identification: the PS3.15 Annex E Basic Application Level Confidentiality Profile and
//! its options, from Table E.1-1 of the current edition ([`crate::dict::deid`]).
//!
//! Every attribute listed in Table E.1-1 gets the action of the basic profile, or of the first
//! selected option that names one. Actions:
//! - `X` remove; `Z` zero length; `D` a dummy value of the VR; `K` keep;
//! - `U` a new UID: `uidRoot` + the decimal HMAC-SHA256 of the original under `uidKey`, so
//!   the same input UID always gives the same output UID for the same key, across instances,
//!   batches and nodes;
//! - `C` clean: dates and times shifted by a per-patient number of days (Retain Longitudinal
//!   Temporal Information with Modified Dates), and for text values (Clean Descriptors, Clean
//!   Structured Content, Clean Graphics) the value is removed, since free text cannot be shown to
//!   be free of identifying information;
//! - compound codes keep the IOD valid: a code with `D` (`Z/D`, `X/D`, `X/Z/D`) gives a dummy
//!   value, `X/Z` gives zero length, `X/Z/U*` keeps the sequence and de-identifies its items.
//!
//! Private attributes are removed, except, with Retain Safe Private, those listed in
//! `safePrivateTags` (with their private creator). Attributes not in the table are kept.
//! Sequences are de-identified item by item with the same rules.
//!
//! Clean Pixel Data and Clean Recognizable Visual Features need pixel codecs and are not
//! offered; an instance with Burned In Annotation (0028,0301) `YES` is refused unless
//! `rejectBurnedInAnnotation=false`.
//!
//! Afterwards Patient Identity Removed (0012,0062) is `YES`, De-identification Method
//! (0012,0063) and De-identification Method Code Sequence (0012,0064) name the profile and the
//! options (CID 7050), and Longitudinal Temporal Information Modified (0028,0303) says what
//! happened to dates.
//!
//! Each de-identified instance writes a record to the hash-chained audit log of the action
//! layer (`camel.ai.audit.*`): digests of the input and output, the profile and options, the
//! new SOP Instance UID; no identifying values.

use std::collections::BTreeMap;
use std::sync::Arc;

use cameleer_core::context::CamelContext;
use cameleer_core::dataformat::DataFormat;
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::spi::Processor;
use cameleer_core::util::Parameters;
use cameleer_core::value::{ObjectMap, Value};
use cameleer_tls::crypto::hash::{hmac, Sha256};

use crate::charset::CharacterSet;
use crate::codec::{DicomFile, ReadOptions};
use crate::dict::{self, deid::DeidRow};
use crate::model;
use crate::object::{self as obj, DataSet, Element};
use crate::tag::{self, tags};
use crate::vr::Vr;

/// Exception kind for instances that cannot be de-identified.
pub const DEIDENTIFICATION_EXCEPTION: &str = "DicomDeidentificationException";

/// The options of PS3.15 E.3 that this implementation offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Options {
    pub retain_safe_private: bool,
    pub retain_uids: bool,
    pub retain_device_identity: bool,
    pub retain_institution_identity: bool,
    pub retain_patient_characteristics: bool,
    pub retain_long_full_dates: bool,
    pub retain_long_modified_dates: bool,
    pub clean_descriptors: bool,
    pub clean_structured_content: bool,
    pub clean_graphics: bool,
}

/// Option names, their CID 7050 codes and meanings.
const OPTION_CODES: [(&str, &str, &str); 10] = [
    ("cleanGraphics", "113103", "Clean Graphics Option"),
    ("cleanStructuredContent", "113104", "Clean Structured Content Option"),
    ("cleanDescriptors", "113105", "Clean Descriptors Option"),
    ("retainLongitudinalFullDates", "113106", "Retain Longitudinal Temporal Information Full Dates Option"),
    ("retainLongitudinalModifiedDates", "113107", "Retain Longitudinal Temporal Information Modified Dates Option"),
    ("retainPatientCharacteristics", "113108", "Retain Patient Characteristics Option"),
    ("retainDeviceIdentity", "113109", "Retain Device Identity Option"),
    ("retainUids", "113110", "Retain UIDs Option"),
    ("retainSafePrivate", "113111", "Retain Safe Private Option"),
    ("retainInstitutionIdentity", "113112", "Retain Institution Identity Option"),
];

impl Options {
    /// Parse a comma list of option names (`retainUids,cleanDescriptors`).
    pub fn parse(s: &str) -> Result<Options> {
        let mut o = Options::default();
        for p in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            match p.to_ascii_lowercase().as_str() {
                "retainsafeprivate" => o.retain_safe_private = true,
                "retainuids" => o.retain_uids = true,
                "retaindeviceidentity" => o.retain_device_identity = true,
                "retaininstitutionidentity" => o.retain_institution_identity = true,
                "retainpatientcharacteristics" => o.retain_patient_characteristics = true,
                "retainlongitudinalfulldates" | "retainlongitudinal" => o.retain_long_full_dates = true,
                "retainlongitudinalmodifieddates" => o.retain_long_modified_dates = true,
                "cleandescriptors" => o.clean_descriptors = true,
                "cleanstructuredcontent" => o.clean_structured_content = true,
                "cleangraphics" => o.clean_graphics = true,
                "cleanpixeldata" | "cleanrecognizablevisualfeatures" => {
                    return Err(CamelError::illegal_argument(format!("The {} option needs pixel codecs, which this build does not have", p)))
                }
                other => return Err(CamelError::illegal_argument(format!("Unknown de-identification option: {}", other))),
            }
        }
        if o.retain_long_full_dates && o.retain_long_modified_dates {
            return Err(CamelError::illegal_argument("retainLongitudinalFullDates and retainLongitudinalModifiedDates exclude each other"));
        }
        Ok(o)
    }

    fn enabled(&self) -> Vec<&'static (&'static str, &'static str, &'static str)> {
        let flags = [
            self.clean_graphics,
            self.clean_structured_content,
            self.clean_descriptors,
            self.retain_long_full_dates,
            self.retain_long_modified_dates,
            self.retain_patient_characteristics,
            self.retain_device_identity,
            self.retain_uids,
            self.retain_safe_private,
            self.retain_institution_identity,
        ];
        OPTION_CODES.iter().zip(flags).filter(|(_, on)| *on).map(|(c, _)| c).collect()
    }

    /// The action for a row: the first selected option that names one, else the basic one.
    /// Retain options come before clean options, so a value both retained and cleaned is cleaned
    /// only where the clean option is the one that names it.
    pub fn action(&self, r: &DeidRow) -> &'static str {
        let opts = [
            (self.retain_safe_private, r.retain_safe_private),
            (self.retain_uids, r.retain_uids),
            (self.retain_device_identity, r.retain_device_identity),
            (self.retain_institution_identity, r.retain_institution_identity),
            (self.retain_patient_characteristics, r.retain_patient_characteristics),
            (self.retain_long_full_dates, r.retain_long_full_dates),
            (self.retain_long_modified_dates, r.retain_long_modified_dates),
            (self.clean_descriptors, r.clean_descriptors),
            (self.clean_structured_content, r.clean_structured_content),
            (self.clean_graphics, r.clean_graphics),
        ];
        opts.iter().find(|(on, code)| *on && !code.is_empty()).map(|(_, c)| *c).unwrap_or(r.basic)
    }
}

/// The simple action an action code resolves to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Remove,
    Zero,
    Dummy,
    Keep,
    Clean,
    Uid,
    /// Keep the sequence, de-identify its items (`X/Z/U*`).
    Items,
}

/// Resolve an action code (see the module documentation).
pub fn resolve(code: &str) -> Action {
    match code.trim() {
        "K" => Action::Keep,
        "X" => Action::Remove,
        "Z" | "X/Z" => Action::Zero,
        "D" | "Z/D" | "X/D" | "X/Z/D" => Action::Dummy,
        "C" => Action::Clean,
        "U" => Action::Uid,
        c if c.contains('U') => Action::Items,
        _ => Action::Remove,
    }
}

/// A de-identifier.
#[derive(Clone)]
pub struct Deidentifier {
    pub options: Options,
    key: Vec<u8>,
    pub uid_root: String,
    /// Maximum number of days dates move (either way) with Modified Dates.
    pub date_shift_days: u32,
    /// Replace Patient ID and Patient's Name with a pseudonym derived from the Patient ID (the
    /// same patient keeps the same pseudonym), instead of the profile's dummy values.
    pub pseudonymous_patient: bool,
    pub pseudonym_prefix: String,
    /// Private attributes kept with Retain Safe Private: (private creator, element & 0xFF).
    pub safe_private: Vec<(String, u16, u8)>,
    pub reject_burned_in_annotation: bool,
}

/// What a de-identification did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub removed: usize,
    pub replaced: usize,
    pub uids: usize,
    pub date_shift_days: Option<i64>,
    pub original_sop_instance_uid: Option<String>,
    pub sop_instance_uid: Option<String>,
}

impl Deidentifier {
    /// A de-identifier with a secret key (the "salt": UIDs, pseudonyms and date shifts are
    /// HMAC-SHA256 values under it).
    pub fn new(key: &[u8], options: Options) -> Deidentifier {
        Deidentifier {
            options,
            key: key.to_vec(),
            uid_root: "2.25".into(),
            date_shift_days: 365,
            pseudonymous_patient: false,
            pseudonym_prefix: "ANON-".into(),
            safe_private: Vec::new(),
            reject_burned_in_annotation: true,
        }
    }

    /// From options: `options`, `uidKey` (required), `uidRoot`, `dateShiftDays`,
    /// `pseudonymousPatient`, `pseudonymPrefix`, `safePrivateTags`
    /// (`CREATOR:gggg,xxee;...`), `rejectBurnedInAnnotation`, `profile` (`basic`).
    pub fn from_options(o: &BTreeMap<String, String>) -> Result<Deidentifier> {
        let mut p = Parameters::new(o.clone());
        let profile = p.take_string("profile", "basic");
        if !profile.eq_ignore_ascii_case("basic") {
            return Err(CamelError::illegal_argument(format!("Unknown de-identification profile '{}' (basic)", profile)));
        }
        let key = p.take("uidKey").filter(|k| !k.is_empty()).ok_or_else(|| CamelError::illegal_argument("uidKey is required: the secret that makes replacement UIDs, pseudonyms and date shifts repeatable (use a {{vault:...}} or property placeholder)"))?;
        let mut d = Deidentifier::new(key.as_bytes(), Options::parse(&p.take_string("options", ""))?);
        d.uid_root = p.take_string("uidRoot", "2.25").trim_end_matches('.').to_string();
        if d.uid_root.is_empty() || !d.uid_root.split('.').all(|c| !c.is_empty() && c.bytes().all(|b| b.is_ascii_digit())) || d.uid_root.len() > 40 {
            return Err(CamelError::illegal_argument(format!("uidRoot must be a numeric UID prefix of at most 40 characters, not '{}'", d.uid_root)));
        }
        d.date_shift_days = p.take_u64("dateShiftDays", 365)? as u32;
        if d.date_shift_days == 0 {
            return Err(CamelError::illegal_argument("dateShiftDays must be at least 1"));
        }
        d.pseudonymous_patient = p.take_bool("pseudonymousPatient", false)?;
        d.pseudonym_prefix = p.take_string("pseudonymPrefix", "ANON-");
        d.reject_burned_in_annotation = p.take_bool("rejectBurnedInAnnotation", true)?;
        if let Some(list) = p.take("safePrivateTags") {
            for item in list.split(';').map(str::trim).filter(|s| !s.is_empty()) {
                let (creator, t) = item.rsplit_once(':').ok_or_else(|| CamelError::illegal_argument(format!("safePrivateTags entries are CREATOR:gggg,xxee, not '{}'", item)))?;
                let t = t.trim().trim_matches(['(', ')']);
                let (g, e) = t.split_once(',').ok_or_else(|| CamelError::illegal_argument(format!("Bad private tag '{}'", t)))?;
                let g = u16::from_str_radix(g.trim(), 16).map_err(|_| CamelError::illegal_argument(format!("Bad private group '{}'", g)))?;
                let e = u8::from_str_radix(e.trim().trim_start_matches(|c| c == 'x' || c == 'X'), 16).map_err(|_| CamelError::illegal_argument(format!("Bad private element '{}' (xxee)", e)))?;
                d.safe_private.push((creator.trim().to_string(), g, e));
            }
        }
        if !p.is_empty() {
            return Err(CamelError::illegal_argument(format!("Unknown de-identification option(s): {:?}", p)));
        }
        Ok(d)
    }

    fn mac(&self, domain: &str, value: &str) -> Vec<u8> {
        let mut data = domain.as_bytes().to_vec();
        data.push(0);
        data.extend_from_slice(value.as_bytes());
        hmac::<Sha256>(&self.key, &data)
    }

    /// The replacement for a UID.
    pub fn map_uid(&self, uid: &str) -> String {
        let h = self.mac("uid", uid.trim_end_matches(['\0', ' ']));
        let n = u128::from_be_bytes(h[..16].try_into().unwrap());
        let mut digits = n.to_string();
        let room = 64 - self.uid_root.len() - 1;
        digits.truncate(room);
        let digits = digits.trim_start_matches('0');
        format!("{}.{}", self.uid_root, if digits.is_empty() { "1" } else { digits })
    }

    /// The pseudonym for a Patient ID.
    pub fn pseudonym(&self, patient_id: &str) -> String {
        let h = self.mac("patient", patient_id);
        let hex: String = h[..6].iter().map(|b| format!("{:02X}", b)).collect();
        let mut s = format!("{}{}", self.pseudonym_prefix, hex);
        s.truncate(64);
        s
    }

    /// The date shift for a patient in days (never zero).
    pub fn shift_for(&self, patient_id: &str) -> i64 {
        let h = self.mac("dates", patient_id);
        let n = u64::from_be_bytes(h[..8].try_into().unwrap());
        let span = self.date_shift_days as u64;
        let v = (n % span) as i64 + 1;
        if n >> 63 == 1 {
            -v
        } else {
            v
        }
    }

    /// De-identify a data set in place.
    pub fn apply(&self, ds: &mut DataSet) -> Result<Report> {
        if self.reject_burned_in_annotation && ds.string(0x0028_0301).map(|v| v.eq_ignore_ascii_case("YES")).unwrap_or(false) {
            return Err(CamelError::new(DEIDENTIFICATION_EXCEPTION, "The instance has Burned In Annotation (0028,0301) YES; removing burned-in text needs Clean Pixel Data, which this build cannot do (set rejectBurnedInAnnotation=false to pass it on anyway)"));
        }
        let patient = ds.string(tags::PATIENT_ID).unwrap_or_default();
        let shift = self.shift_for(&patient);
        let mut rep = Report { original_sop_instance_uid: ds.sop_instance_uid(), ..Default::default() };
        if self.options.retain_long_modified_dates {
            rep.date_shift_days = Some(shift);
        }
        let cs = ds.charset();
        self.walk(ds, &cs, shift, &mut rep, true)?;
        if self.pseudonymous_patient {
            let p = self.pseudonym(&patient);
            ds.insert(Element::string(tags::PATIENT_ID, Vr::LO, &p));
            ds.insert(Element::string(tags::PATIENT_NAME, Vr::PN, &p));
        }
        ds.insert(Element::string(tags::PATIENT_IDENTITY_REMOVED, Vr::CS, "YES"));
        let enabled = self.options.enabled();
        let mut method = format!("PS3.15 {} Basic Application Confidentiality Profile", dict::deid::EDITION);
        for (name, _, _) in &enabled {
            method.push_str(", ");
            method.push_str(name);
        }
        method.truncate(64);
        ds.insert(Element::string(tags::DEIDENTIFICATION_METHOD, Vr::LO, &method));
        let code = |value: &str, meaning: &str| {
            DataSet::from_elements(vec![Element::string(0x0008_0100, Vr::SH, value), Element::string(0x0008_0102, Vr::SH, "DCM"), Element::string(0x0008_0104, Vr::LO, meaning)])
        };
        let mut items = vec![code("113100", "Basic Application Confidentiality Profile")];
        for (_, c, m) in &enabled {
            items.push(code(c, m));
        }
        ds.insert(Element::sequence(tags::DEIDENTIFICATION_METHOD_CODE_SEQUENCE, items));
        let ltim = if self.options.retain_long_full_dates {
            "UNMODIFIED"
        } else if self.options.retain_long_modified_dates {
            "MODIFIED"
        } else {
            "REMOVED"
        };
        ds.insert(Element::string(tags::LONGITUDINAL_TEMPORAL_INFORMATION_MODIFIED, Vr::CS, ltim));
        rep.sop_instance_uid = ds.sop_instance_uid();
        Ok(rep)
    }

    fn walk(&self, ds: &mut DataSet, inherited: &CharacterSet, shift: i64, rep: &mut Report, top: bool) -> Result<()> {
        let cs = ds.charset_or(inherited);
        // private blocks to keep: creator element number of each kept (group, block)
        let mut keep_blocks: Vec<(u16, u8)> = Vec::new();
        if self.options.retain_safe_private {
            for e in ds.iter().filter(|e| tag::is_private_creator(e.tag)) {
                let creator = e.to_string_value(&cs);
                if self.safe_private.iter().any(|(c, g, _)| *c == creator && *g == tag::group(e.tag)) {
                    keep_blocks.push((tag::group(e.tag), tag::element(e.tag) as u8));
                }
            }
        }
        let tags_now: Vec<u32> = ds.iter().map(|e| e.tag).collect();
        for t in tags_now {
            if top && (tag::group(t) == 0x0002 || t == tags::SPECIFIC_CHARACTER_SET) {
                continue;
            }
            if t == tags::SPECIFIC_CHARACTER_SET {
                continue;
            }
            if tag::is_private(t) {
                let g = tag::group(t);
                let keep = if tag::is_private_creator(t) {
                    keep_blocks.contains(&(g, tag::element(t) as u8))
                } else {
                    let block = (tag::element(t) >> 8) as u8;
                    keep_blocks.contains(&(g, block))
                        && ds
                            .get(tag::tag(g, block as u16))
                            .map(|c| c.to_string_value(&cs))
                            .is_some_and(|creator| self.safe_private.iter().any(|(c, sg, el)| *c == creator && *sg == g && *el == (tag::element(t) & 0xFF) as u8))
                };
                if !keep {
                    ds.remove(t);
                    rep.removed += 1;
                } else if let Some(el) = ds.get_mut(t) {
                    if let Some(items) = el.items_mut() {
                        for it in items {
                            self.walk(&mut it.dataset, &cs, shift, rep, false)?;
                        }
                    }
                }
                continue;
            }
            // the most specific matching row
            let row = dict::deid_rows(t).filter(|r| r.mask != 0x0001_0000).max_by_key(|r| r.mask.count_ones());
            let action = match row {
                Some(r) => resolve(self.options.action(r)),
                None => Action::Keep,
            };
            let Some(el) = ds.get_mut(t) else { continue };
            match action {
                Action::Remove => {
                    ds.remove(t);
                    rep.removed += 1;
                }
                Action::Zero => {
                    el.value = if el.vr == Vr::SQ { obj::Value::Sequence { items: Vec::new(), undefined_length: false } } else { obj::Value::Bytes(Vec::new()) };
                    rep.replaced += 1;
                }
                Action::Dummy => {
                    *el = dummy(t, el.vr);
                    rep.replaced += 1;
                }
                Action::Uid => {
                    if el.vr == Vr::UI {
                        let vals = el.str_values(&cs);
                        let mapped: Vec<String> = vals.iter().map(|u| if u.is_empty() { String::new() } else { self.map_uid(u) }).collect();
                        let refs: Vec<&str> = mapped.iter().map(String::as_str).collect();
                        *el = Element::strings(t, Vr::UI, &refs, &cs)?;
                        rep.uids += 1;
                    } else if let Some(items) = el.items_mut() {
                        for it in items {
                            self.walk(&mut it.dataset, &cs, shift, rep, false)?;
                        }
                    }
                }
                Action::Clean => {
                    if matches!(el.vr, Vr::DA | Vr::DT) && self.options.retain_long_modified_dates {
                        let vals = el.str_values(&cs);
                        let shifted: Vec<String> = vals.iter().map(|v| shift_date_value(v, el.vr, shift)).collect();
                        let refs: Vec<&str> = shifted.iter().map(String::as_str).collect();
                        *el = Element::strings(t, el.vr, &refs, &cs)?;
                        rep.replaced += 1;
                    } else if el.vr == Vr::TM {
                        // times carry no date; kept with modified dates
                    } else if el.vr == Vr::SQ {
                        if let Some(items) = el.items_mut() {
                            for it in items {
                                self.walk(&mut it.dataset, &cs, shift, rep, false)?;
                            }
                        }
                    } else {
                        el.value = obj::Value::Bytes(Vec::new());
                        rep.replaced += 1;
                    }
                }
                Action::Keep | Action::Items => {
                    if let Some(items) = el.items_mut() {
                        for it in items {
                            self.walk(&mut it.dataset, &cs, shift, rep, false)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// A dummy value for a VR (PS3.15 action `D`).
pub fn dummy(t: u32, vr: Vr) -> Element {
    let s = |v: &str| Element::string(t, vr, v);
    match vr {
        Vr::DA => s("19000101"),
        Vr::TM => s("000000"),
        Vr::DT => s("19000101000000"),
        Vr::AS => s("000Y"),
        Vr::DS | Vr::IS => s("0"),
        Vr::UI => s("2.25.0"),
        Vr::AE | Vr::CS | Vr::LO | Vr::LT | Vr::PN | Vr::SH | Vr::ST | Vr::UC | Vr::UT => s("ANONYMOUS"),
        Vr::UR => s(""),
        Vr::SQ => Element::sequence(t, Vec::new()),
        v if v.number_size() > 0 => Element::numbers(t, v, &[0.0]),
        _ => Element::bytes(t, vr, Vec::new()),
    }
}

/// Days since 1970-01-01 of a civil date.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// The civil date of a day number.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn shift_ymd(s: &str, days: i64) -> Option<String> {
    if s.len() < 8 || !s[..8].bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (y, m, d) = (s[..4].parse::<i64>().ok()?, s[4..6].parse::<u32>().ok()?, s[6..8].parse::<u32>().ok()?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let (ny, nm, nd) = civil_from_days(days_from_civil(y, m, d) + days);
    Some(format!("{:04}{:02}{:02}{}", ny, nm, nd, &s[8..]))
}

/// Shift a DA or DT value (ranges `a-b` too) by days; values that are not dates are emptied.
pub fn shift_date_value(v: &str, vr: Vr, days: i64) -> String {
    if v.is_empty() {
        return String::new();
    }
    let one = |p: &str| if p.is_empty() { Some(String::new()) } else { shift_ymd(p, days) };
    let r = if vr == Vr::DA && v.contains('-') {
        let (a, b) = v.split_once('-').unwrap();
        one(a).zip(one(b)).map(|(a, b)| format!("{}-{}", a, b))
    } else {
        one(v)
    };
    r.unwrap_or_default()
}

// ---------------------------------------------------------------------------------------
// In routes: processor and data format

fn sha256_hex(b: &[u8]) -> String {
    cameleer_ai::tool::sha256_hex(b)
}

/// Write the audit record of one de-identified instance.
pub fn audit(ctx: Option<&CamelContext>, d: &Deidentifier, input: &[u8], output: &[u8], rep: &Report) {
    let Some(ctx) = ctx else { return };
    let Ok(reg) = cameleer_ai::registry::AiRegistry::of(ctx) else { return };
    let mut detail = ObjectMap::new();
    detail.insert("profile".into(), Value::from("basic"));
    detail.insert("edition".into(), Value::from(dict::deid::EDITION));
    detail.insert("options".into(), Value::Array(d.options.enabled().iter().map(|(n, _, _)| Value::from(*n)).collect()));
    detail.insert("removed".into(), Value::Int(rep.removed as i64));
    detail.insert("replaced".into(), Value::Int(rep.replaced as i64));
    detail.insert("uids".into(), Value::Int(rep.uids as i64));
    if let Some(u) = &rep.sop_instance_uid {
        detail.insert("sopInstanceUid".into(), Value::from(u.clone()));
    }
    detail.insert("keyDigest".into(), Value::from(sha256_hex(&d.key)[..16].to_string()));
    let entry = cameleer_core::audit::AuditEntry {
        tool: "dicom-deidentify".into(),
        arguments_digest: sha256_hex(input),
        decision: "allowed".into(),
        outcome: "ok".into(),
        result_digest: Some(sha256_hex(output)),
        detail: Some(Value::Object(detail).to_json()),
        ..Default::default()
    };
    reg.audit().append(&entry);
}

/// De-identify the body of an exchange: Part 10 bytes stay bytes, a Value tree stays a tree.
pub fn deidentify_exchange(d: &Deidentifier, ex: &mut Exchange) -> Result<Report> {
    let (input, tree) = match ex.body() {
        Value::Bytes(b) => (b.clone(), false),
        Value::Object(_) => (model::value_to_file(ex.body())?.to_bytes()?, true),
        other => return Err(CamelError::new("InvalidPayloadException", format!("De-identification needs Part 10 bytes or a dicom Value tree, got {}", other.type_name()))),
    };
    let mut f = DicomFile::parse(&input, ReadOptions::default())?;
    let rep = d.apply(&mut f.dataset)?;
    // the file meta follows the instance
    if let Some(u) = &rep.sop_instance_uid {
        f.meta.insert(crate::dimse::ui(tags::MEDIA_STORAGE_SOP_INSTANCE_UID, u));
    }
    f.meta.remove(tags::SOURCE_APPLICATION_ENTITY_TITLE);
    let out = f.to_bytes()?;
    audit(ex.context.as_ref(), d, &input, &out, &rep);
    if let Some(u) = &rep.sop_instance_uid {
        ex.set_header(crate::headers::SOP_INSTANCE_UID, u.clone());
    }
    ex.set_body(if tree { model::file_to_value(&f) } else { Value::Bytes(out) });
    Ok(rep)
}

/// The de-identification processor (`.process(DeidentifyProcessor::new(..))`).
pub struct DeidentifyProcessor(pub Arc<Deidentifier>);

impl DeidentifyProcessor {
    pub fn new(d: Deidentifier) -> DeidentifyProcessor {
        DeidentifyProcessor(Arc::new(d))
    }
}

impl Processor for DeidentifyProcessor {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        deidentify_exchange(&self.0, ex).map(|_| ())
    }
}

/// The `dicomDeidentify` data format: `marshal` (and `unmarshal`) de-identify the body, so the
/// YAML and XML DSLs can use it as `marshal: {dicomDeidentify: {uidKey: ..., options: ...}}`.
pub struct DeidentifyDataFormat(pub Arc<Deidentifier>);

impl DataFormat for DeidentifyDataFormat {
    fn name(&self) -> &str {
        "dicomDeidentify"
    }
    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        deidentify_exchange(&self.0, ex).map(|_| ())
    }
    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        deidentify_exchange(&self.0, ex).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        for (y, m, d) in [(1970, 1, 1), (2000, 2, 29), (2026, 12, 31), (1900, 1, 1), (2400, 3, 1)] {
            assert_eq!(civil_from_days(days_from_civil(y, m, d)), (y, m, d));
        }
        assert_eq!(days_from_civil(1970, 1, 2), 1);
        assert_eq!(shift_date_value("20240228", Vr::DA, 2), "20240301");
        assert_eq!(shift_date_value("20240101-20240131", Vr::DA, -1), "20231231-20240130");
        assert_eq!(shift_date_value("-20240131", Vr::DA, 1), "-20240201");
        assert_eq!(shift_date_value("20240101120000.5", Vr::DT, 10), "20240111120000.5");
        assert_eq!(shift_date_value("garbage", Vr::DA, 10), "");
    }

    #[test]
    fn codes() {
        assert_eq!(resolve("X/Z/U*"), Action::Items);
        assert_eq!(resolve("Z/D"), Action::Dummy);
        assert_eq!(resolve("X/Z"), Action::Zero);
        let d = Deidentifier::new(b"k", Options::default());
        let u = d.map_uid("1.2.3");
        assert!(u.starts_with("2.25.") && u.len() <= 64 && u == d.map_uid("1.2.3\0"));
        assert_ne!(u, Deidentifier::new(b"other", Options::default()).map_uid("1.2.3"));
        let s = d.shift_for("P1");
        assert!(s != 0 && s.abs() <= 365);
        assert!(Options::parse("cleanPixelData").is_err());
        assert!(Options::parse("retainLongitudinalFullDates,retainLongitudinalModifiedDates").is_err());
    }
}

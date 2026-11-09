//! De-identification (PS3.15 Annex E): every attribute checked against Table E.1-1 for several
//! option sets, determinism under one key, consistent UIDs across instances and references,
//! date shifting, safe private attributes, refusal of burned-in annotation, the added
//! attributes, and audit records in routes.

use cameleer_core::dataformat::DataFormatDefinition;
use cameleer_core::prelude::*;
use cameleer_dicom::charset::CharacterSet;
use cameleer_dicom::deid::{self, Action, Deidentifier, Options};
use cameleer_dicom::object::{DataSet, Element};
use cameleer_dicom::tag::{self, tags};
use cameleer_dicom::testing::Instance;
use cameleer_dicom::vr::Vr;
use cameleer_dicom::{dict, DicomFile, ReadOptions};

/// A CT instance with many identifying attributes, a private block and nested UIDs.
fn phi_instance(patient: &str, study: u32, inst: u32) -> DataSet {
    let i = Instance::ct(patient, study, 1, inst);
    let mut ds = i.dataset();
    let s = |t, vr, v: &str| Element::string(t, vr, v);
    for e in [
        s(0x0008_0081, Vr::ST, "1 Hospital Road"),
        s(0x0008_1010, Vr::SH, "CT-ROOM-3"),
        s(0x0008_1040, Vr::LO, "Radiology"),
        s(0x0008_1048, Vr::PN, "Physician^Pat"),
        s(0x0008_1070, Vr::PN, "Operator^Olga"),
        s(0x0010_1000, Vr::LO, "OTHER-ID-1"),
        s(0x0010_1040, Vr::LO, "5 Patient Street"),
        s(0x0010_2154, Vr::SH, "555-0100"),
        s(0x0010_4000, Vr::LT, "Patient comments with a name"),
        s(0x0010_1020, Vr::DS, "1.80"),
        s(0x0010_1030, Vr::DS, "80"),
        s(0x0018_1030, Vr::LO, "Head protocol"),
        s(0x0020_4000, Vr::LT, "Image comments"),
        s(0x0032_1032, Vr::PN, "Requester^Rob"),
        s(0x0040_0244, Vr::DA, "20251231"),
        s(0x0008_002A, Vr::DT, "20251231101500"),
        s(0x0028_0301, Vr::CS, "NO"),
        s(0x0009_0010, Vr::LO, "SAFE VENDOR"),
        s(0x0009_1001, Vr::LO, "safe value"),
        s(0x0009_1002, Vr::LO, "unsafe value"),
        s(0x0011_0010, Vr::LO, "OTHER VENDOR"),
        s(0x0011_1001, Vr::LO, "secret"),
    ] {
        ds.insert(e);
    }
    // Referenced Image Sequence (X/Z/U*) pointing at another instance of the same series
    let other = Instance::ct(patient, study, 1, inst + 100);
    ds.insert(Element::sequence(
        0x0008_1140,
        vec![DataSet::from_elements(vec![s(tags::REFERENCED_SOP_CLASS_UID, Vr::UI, &other.sop_class), s(tags::REFERENCED_SOP_INSTANCE_UID, Vr::UI, &other.instance_uid)])],
    ));
    ds
}

fn key() -> Deidentifier {
    Deidentifier::new(b"test-salt-1", Options::default())
}

/// Check every element of the original against the action Table E.1-1 gives it.
fn check_against_table(d: &Deidentifier, before: &DataSet, after: &DataSet) {
    let cs = before.charset();
    let mut checked = 0;
    for e in before.iter() {
        let t = e.tag;
        if t == tags::SPECIFIC_CHARACTER_SET || tag::is_private(t) {
            continue;
        }
        if d.pseudonymous_patient && (t == tags::PATIENT_ID || t == tags::PATIENT_NAME) {
            continue; // replaced by the pseudonym
        }
        let row = dict::deid_rows(t).filter(|r| r.mask != 0x0001_0000).max_by_key(|r| r.mask.count_ones());
        let Some(row) = row else {
            assert_eq!(after.get(t), Some(e), "{} is not in Table E.1-1 and must be kept", tag::format(t));
            continue;
        };
        let code = d.options.action(row);
        let got = after.get(t);
        let name = format!("{} {} ({})", tag::format(t), row.name, code);
        match deid::resolve(code) {
            Action::Remove => assert!(got.is_none(), "{name}: should be removed"),
            Action::Zero => assert!(got.is_some_and(|g| g.raw().is_some_and(|b| b.is_empty()) || g.items().is_some_and(|i| i.is_empty())), "{name}: should be empty, got {got:?}"),
            Action::Dummy => {
                let g = got.unwrap_or_else(|| panic!("{name}: missing"));
                assert_eq!(g, &deid::dummy(t, e.vr), "{name}: should be the dummy value");
            }
            Action::Keep => assert_eq!(got.map(|g| g.to_string_value(&cs)), Some(e.to_string_value(&cs)), "{name}: should be kept"),
            Action::Uid => {
                let g = got.unwrap_or_else(|| panic!("{name}: missing"));
                assert_eq!(g.to_string_value(&cs), d.map_uid(&e.to_string_value(&cs)), "{name}: should be remapped");
            }
            Action::Clean => {
                let g = got.unwrap_or_else(|| panic!("{name}: missing"));
                if matches!(e.vr, Vr::DA | Vr::DT) && d.options.retain_long_modified_dates {
                    let shift = d.shift_for(&before.string(tags::PATIENT_ID).unwrap());
                    assert_eq!(g.to_string_value(&cs), deid::shift_date_value(&e.to_string_value(&cs), e.vr, shift), "{name}: should be shifted");
                } else if e.vr == Vr::TM {
                    assert_eq!(g, e, "{name}");
                } else if e.vr != Vr::SQ {
                    assert!(g.raw().is_some_and(|b| b.is_empty()), "{name}: cleaned text is removed");
                }
            }
            Action::Items => {}
        }
        checked += 1;
    }
    assert!(checked > 25, "only {checked} attributes were in the table");
}

#[test]
fn basic_profile_attribute_by_attribute() {
    for opts in [
        "",
        "retainUids",
        "retainLongitudinalModifiedDates",
        "retainLongitudinalFullDates,retainPatientCharacteristics",
        "retainDeviceIdentity,retainInstitutionIdentity",
        "cleanDescriptors,cleanStructuredContent,cleanGraphics",
    ] {
        let mut d = key();
        d.options = Options::parse(opts).unwrap();
        let before = phi_instance("PHI-1", 40, 1);
        let mut after = before.clone();
        let rep = d.apply(&mut after).unwrap();
        check_against_table(&d, &before, &after);
        assert!(rep.removed > 5 && rep.replaced > 5, "{opts}: {rep:?}");
        // nothing identifying is left anywhere in the encoded instance
        let bytes = DicomFile::new(after.clone(), cameleer_dicom::ts::EXPLICIT_VR_LITTLE_ENDIAN).to_bytes().unwrap();
        let text = String::from_utf8_lossy(&bytes);
        for phi in ["PHI-1", "Hospital Road", "Patient Street", "OTHER-ID-1", "555-0100", "Physician^Pat", "Requester^Rob", "secret", "unsafe value"] {
            // Retain Institution Identity keeps the institution's address on purpose
            if phi == "Hospital Road" && opts.contains("retainInstitutionIdentity") {
                continue;
            }
            assert!(!text.contains(phi), "{opts}: '{phi}' survived");
        }
        // what the profile adds
        assert_eq!(after.string(tags::PATIENT_IDENTITY_REMOVED).unwrap(), "YES");
        let codes: Vec<String> = after.get(tags::DEIDENTIFICATION_METHOD_CODE_SEQUENCE).unwrap().items().unwrap().iter().map(|i| i.dataset.string(0x0008_0100).unwrap()).collect();
        assert_eq!(codes[0], "113100");
        assert_eq!(codes.len(), 1 + opts.split(',').filter(|s| !s.is_empty()).count(), "{opts}");
        let ltim = after.string(tags::LONGITUDINAL_TEMPORAL_INFORMATION_MODIFIED).unwrap();
        assert_eq!(ltim, if opts.contains("FullDates") { "UNMODIFIED" } else if opts.contains("ModifiedDates") { "MODIFIED" } else { "REMOVED" });
    }
}

#[test]
fn deterministic_and_consistent_uids() {
    let d = key();
    let mut a = phi_instance("P1", 41, 1);
    let mut a2 = a.clone();
    let mut b = phi_instance("P1", 41, 101); // the instance that `a` references
    d.apply(&mut a).unwrap();
    d.apply(&mut a2).unwrap();
    d.apply(&mut b).unwrap();
    assert_eq!(a, a2, "same input, same key, same output");
    // the study and series UIDs agree across instances
    assert_eq!(a.string(tags::STUDY_INSTANCE_UID), b.string(tags::STUDY_INSTANCE_UID));
    assert_eq!(a.string(tags::SERIES_INSTANCE_UID), b.string(tags::SERIES_INSTANCE_UID));
    // the reference inside the sequence points at the new UID of the referenced instance
    let r = a.get(0x0008_1140).unwrap().items().unwrap()[0].dataset.string(tags::REFERENCED_SOP_INSTANCE_UID).unwrap();
    assert_eq!(Some(r), b.sop_instance_uid());
    // another key gives other UIDs
    let mut c = phi_instance("P1", 41, 1);
    Deidentifier::new(b"another-salt", Options::default()).apply(&mut c).unwrap();
    assert_ne!(c.sop_instance_uid(), a.sop_instance_uid());
    // with a uidRoot
    let mut o = std::collections::BTreeMap::new();
    o.insert("uidKey".to_string(), "k".to_string());
    o.insert("uidRoot".to_string(), "1.2.840.99999".to_string());
    let rooted = Deidentifier::from_options(&o).unwrap();
    let u = rooted.map_uid("1.2.3.4");
    assert!(u.starts_with("1.2.840.99999.") && u.len() <= 64, "{u}");
}

#[test]
fn modified_dates_pseudonyms_and_safe_private() {
    let mut o = std::collections::BTreeMap::new();
    o.insert("uidKey".to_string(), "salt".to_string());
    o.insert("options".to_string(), "retainLongitudinalModifiedDates,retainSafePrivate".to_string());
    o.insert("pseudonymousPatient".to_string(), "true".to_string());
    o.insert("dateShiftDays".to_string(), "30".to_string());
    o.insert("safePrivateTags".to_string(), "SAFE VENDOR:0009,xx01".to_string());
    let d = Deidentifier::from_options(&o).unwrap();
    let before = phi_instance("PAT-9", 42, 1);
    let mut after = before.clone();
    let rep = d.apply(&mut after).unwrap();
    let shift = rep.date_shift_days.unwrap();
    assert!(shift != 0 && shift.abs() <= 30);
    let expected = deid::shift_date_value("20260101", Vr::DA, shift);
    assert_eq!(after.string(tags::STUDY_DATE).unwrap(), expected);
    // another instance of the same patient moves by the same number of days
    let mut other = phi_instance("PAT-9", 43, 1);
    d.apply(&mut other).unwrap();
    assert_eq!(other.string(tags::STUDY_DATE).unwrap(), expected);
    // pseudonyms: stable per patient, not the original
    let p = after.string(tags::PATIENT_ID).unwrap();
    assert!(p.starts_with("ANON-") && p != "PAT-9");
    assert_eq!(other.string(tags::PATIENT_ID).unwrap(), p);
    assert_eq!(after.string(tags::PATIENT_NAME).unwrap(), p);
    check_against_table(&d, &before, &after);
    // the safe private attribute and its creator stay, the rest of the private data goes
    assert_eq!(after.string(0x0009_0010).unwrap(), "SAFE VENDOR");
    assert_eq!(after.string(0x0009_1001).unwrap(), "safe value");
    assert!(after.get(0x0009_1002).is_none() && after.get(0x0011_0010).is_none() && after.get(0x0011_1001).is_none());
}

#[test]
fn refusals() {
    let mut ds = phi_instance("P", 44, 1);
    ds.insert(Element::string(0x0028_0301, Vr::CS, "YES"));
    let e = key().apply(&mut ds.clone()).unwrap_err();
    assert_eq!(e.kind, "DicomDeidentificationException");
    let mut d = key();
    d.reject_burned_in_annotation = false;
    assert!(d.apply(&mut ds).is_ok());
    let mut o = std::collections::BTreeMap::new();
    assert!(Deidentifier::from_options(&o).is_err(), "uidKey is required");
    o.insert("uidKey".to_string(), "k".to_string());
    o.insert("options".to_string(), "cleanPixelData".to_string());
    assert!(Deidentifier::from_options(&o).is_err());
    let _ = CharacterSet::default();
}

#[test]
fn data_format_in_a_route_writes_audit_records() {
    let ctx = CamelContext::new();
    cameleer_dicom::register(&ctx);
    ctx.properties().set("deid.key", "route-salt");
    ctx.add_routes(|b| {
        b.from("direct:deid").marshal(DataFormatDefinition::new("dicomDeidentify").option("uidKey", "{{deid.key}}").option("options", "retainLongitudinalModifiedDates"));
        b.from("direct:tree").unmarshal(DataFormatDefinition::new("dicom")).marshal(DataFormatDefinition::new("dicomDeidentify").option("uidKey", "{{deid.key}}"));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let input = DicomFile::new(phi_instance("AUDIT-1", 45, 1), cameleer_dicom::ts::EXPLICIT_VR_LITTLE_ENDIAN).to_bytes().unwrap();
    let mut ex = ctx.create_exchange();
    ex.set_body(Value::Bytes(input.clone()));
    let ex = t.send("direct:deid", ex);
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
    let Value::Bytes(out) = ex.body() else { panic!() };
    let f = DicomFile::parse(out, ReadOptions::default()).unwrap();
    assert_eq!(f.dataset.string(tags::PATIENT_IDENTITY_REMOVED).unwrap(), "YES");
    // the file meta follows the new instance UID
    assert_eq!(f.meta.string(tags::MEDIA_STORAGE_SOP_INSTANCE_UID), f.dataset.sop_instance_uid());
    assert_eq!(ex.header("CamelDicomSopInstanceUid").map(|v| v.to_string()), f.dataset.sop_instance_uid());
    // a Value tree in, a Value tree out
    let tree = t.request_body("direct:tree", Value::Bytes(input.clone())).unwrap();
    assert!(tree.get("00120062").is_some());
    // one audit record per instance, chained, without identifying values
    let reg = cameleer_ai::registry::AiRegistry::of(&ctx).unwrap();
    let records = reg.audit().records();
    let ours: Vec<&Value> = records.iter().filter(|r| r.get("tool").map(|v| v.to_string()) == Some("dicom-deidentify".into())).collect();
    assert_eq!(ours.len(), 2);
    let text = Value::Array(records.clone()).to_json();
    assert!(!text.contains("AUDIT-1") && !text.contains("Hospital"));
    assert!(text.contains("retainLongitudinalModifiedDates"));
    assert_eq!(cameleer_core::audit::verify_records(&records).unwrap().0, records.len(), "the chain verifies");
    ctx.stop().unwrap();
}

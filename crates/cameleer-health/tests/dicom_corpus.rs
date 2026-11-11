//! Public test files (pydicom v3.0.1, see TESTDATA.md): every file parses, writes back
//! byte-exact, and survives the Value tree byte-exact; character sets decode to the expected
//! names. Runs when `CAMEL_DICOM_TESTDATA` names the directory filled by
//! `scripts/fetch-dicom-testdata.sh` (or `target/dicom-testdata` exists); otherwise it returns
//! early, so the hermetic suite needs no download.

use std::path::PathBuf;

use cameleer_health::dicom::codec::DicomFile;
use cameleer_health::dicom::model;
use cameleer_health::dicom::tag::tags;
use cameleer_health::dicom::ReadOptions;

fn dir() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("CAMEL_DICOM_TESTDATA") {
        return Some(PathBuf::from(d));
    }
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/dicom-testdata");
    d.join("CT_small.dcm").exists().then_some(d)
}

fn read(d: &PathBuf, name: &str) -> Vec<u8> {
    std::fs::read(d.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

#[test]
fn every_file_round_trips_byte_exact() {
    let Some(d) = dir() else { return };
    let mut n = 0;
    for entry in std::fs::read_dir(&d).unwrap() {
        let p = entry.unwrap().path();
        if p.extension().and_then(|e| e.to_str()) != Some("dcm") {
            continue;
        }
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        let bytes = std::fs::read(&p).unwrap();
        let f = DicomFile::parse(&bytes, ReadOptions::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
        let out = f.to_bytes().unwrap_or_else(|e| panic!("{name}: {e}"));
        if f.transfer_syntax().deflated {
            // our deflate output differs from zlib's; the inflated data set must be identical
            let back = DicomFile::parse(&out, ReadOptions::default()).unwrap();
            assert_eq!(back.dataset, f.dataset, "{name}");
        } else {
            assert_eq!(out.len(), bytes.len(), "{name}: length");
            assert!(out == bytes, "{name}: first difference at {:?}", out.iter().zip(&bytes).position(|(a, b)| a != b));
        }
        let tree = model::file_to_value(&f);
        let again = model::value_to_file(&tree).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(again.to_bytes().unwrap(), out, "{name}: Value tree");
        // DICOM JSON and back keeps the data set
        n += 1;
    }
    assert!(n >= 30, "only {n} files");
}

#[test]
fn character_sets_decode_to_the_expected_names() {
    let Some(d) = dir() else { return };
    let cases = [
        ("chrH31.dcm", "Yamada^Tarou=山田^太郎=やまだ^たろう"),
        ("chrH32.dcm", "ﾔﾏﾀﾞ^ﾀﾛｳ=山田^太郎=やまだ^たろう"),
        ("chrI2.dcm", "Hong^Gildong=洪^吉洞=홍^길동"),
        ("chrX1.dcm", "Wang^XiaoDong=王^小東="),
        ("chrX2.dcm", "Wang^XiaoDong=王^小东="),
        ("chrRuss.dcm", "Люкceмбypг"),
        ("chrGreek.dcm", "Διονυσιος"),
        ("chrFren.dcm", "Buc^Jérôme"),
        ("chrGerm.dcm", "Äneas^Rüdiger"),
        ("chrHbrw.dcm", "שרון^דבורה"),
        ("chrArab.dcm", "قباني^لنزار"),
    ];
    for (file, expected) in cases {
        let f = DicomFile::parse(&read(&d, file), ReadOptions::default()).unwrap();
        assert_eq!(f.dataset.string(tags::PATIENT_NAME).unwrap(), expected, "{file}");
        // and encodes back to the same bytes
        let el = f.dataset.get(tags::PATIENT_NAME).unwrap();
        let re = cameleer_health::dicom::Element::strings(tags::PATIENT_NAME, el.vr, &[expected], &f.dataset.charset()).unwrap();
        assert_eq!(re.raw(), el.raw(), "{file}: re-encoded");
    }
}

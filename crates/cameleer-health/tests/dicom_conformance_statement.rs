//! The DICOM Conformance Statement (docs/packs/health/conformance/) stays true to the code: its
//! generated tables (the Storage SOP classes the SCP accepts, the identification of this
//! implementation, the character sets) are rebuilt here from the dictionary and the constants and
//! must match the document. `CAMEL_UPDATE_CONFORMANCE=1` rewrites the generated parts.

use std::path::PathBuf;

use cameleer_health::dicom::{charset::CharacterSet, codec, dict, dimse};

fn statement() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../docs/packs/health/conformance/dicom-conformance-statement-{}.md", env!("CARGO_PKG_VERSION")))
}

fn storage_table() -> String {
    let mut rows: Vec<(String, String)> = dimse::storage_sop_classes().into_iter().map(|u| (dict::uid(u).map(|e| e.name.to_string()).unwrap_or_default(), u.to_string())).collect();
    rows.sort();
    let mut t = String::from("| SOP class | UID | SCU | SCP |\n|---|---|---|---|\n");
    for (name, uid) in &rows {
        t.push_str(&format!("| {} | `{}` | yes | yes |\n", name, uid));
    }
    t.push_str(&format!("\n{} Storage SOP classes (the non-retired ones of PS3.6 {}). The SCP also accepts retired and private Storage SOP classes; the SCU sends any SOP class an instance has.\n", rows.len(), dict::EDITION));
    t
}

fn identification() -> String {
    format!(
        "| item | value |\n|---|---|\n| Implementation Class UID | `{}` |\n| Implementation Version Name | `{}` |\n| Pack | `cameleer-health` {} |\n| Data dictionary | DICOM PS3.6 {} |\n| De-identification table | PS3.15 Table E.1-1, {} |\n",
        codec::IMPLEMENTATION_CLASS_UID,
        codec::IMPLEMENTATION_VERSION_NAME,
        env!("CARGO_PKG_VERSION"),
        dict::EDITION,
        dict::deid::EDITION
    )
}

fn charsets() -> String {
    let terms = [
        "ISO_IR 6", "ISO_IR 100", "ISO_IR 101", "ISO_IR 109", "ISO_IR 110", "ISO_IR 126", "ISO_IR 127", "ISO_IR 138", "ISO_IR 144", "ISO_IR 148", "ISO_IR 166", "ISO_IR 203", "ISO_IR 13", "ISO_IR 192", "GB18030", "GBK",
        "ISO 2022 IR 6", "ISO 2022 IR 100", "ISO 2022 IR 101", "ISO 2022 IR 109", "ISO 2022 IR 110", "ISO 2022 IR 126", "ISO 2022 IR 127", "ISO 2022 IR 138", "ISO 2022 IR 144", "ISO 2022 IR 148", "ISO 2022 IR 166", "ISO 2022 IR 203", "ISO 2022 IR 13", "ISO 2022 IR 87", "ISO 2022 IR 159", "ISO 2022 IR 149", "ISO 2022 IR 58",
    ];
    let mut t = String::from("| Specific Character Set (0008,0005) defined term | supported |\n|---|---|\n");
    for term in terms {
        let ok = CharacterSet::from_value(term).is_supported();
        assert!(ok, "{term} should be supported");
        t.push_str(&format!("| `{}` | yes |\n", term));
    }
    t
}

fn replace_section(doc: &str, name: &str, body: &str) -> String {
    let start = format!("<!-- generated: {} -->\n", name);
    let end = format!("<!-- end generated: {} -->", name);
    let a = doc.find(&start).unwrap_or_else(|| panic!("the statement has no generated section {name}")) + start.len();
    let b = doc[a..].find(&end).unwrap_or_else(|| panic!("section {name} is not closed")) + a;
    format!("{}{}{}", &doc[..a], body, &doc[b..])
}

#[test]
fn the_conformance_statement_matches_the_code() {
    let path = statement();
    let doc = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())).replace("\r\n", "\n");
    let mut want = doc.clone();
    for (name, body) in [("identification", identification()), ("storage-sop-classes", storage_table()), ("character-sets", charsets())] {
        want = replace_section(&want, name, &body);
    }
    if std::env::var("CAMEL_UPDATE_CONFORMANCE").map(|v| v == "1").unwrap_or(false) {
        std::fs::write(&path, &want).unwrap();
        return;
    }
    assert!(doc == want, "the conformance statement is out of date: run CAMEL_UPDATE_CONFORMANCE=1 cargo test -p cameleer-health --test dicom_conformance_statement");
}

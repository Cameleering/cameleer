//! DIMSE over TCP and TLS: the SCU functions against the embedded test PACS (C-ECHO, C-STORE,
//! C-FIND at each level, C-MOVE to a second PACS, C-GET), association rejections, transfer
//! syntax negotiation and status mapping.

use std::sync::Arc;

use cameleer_dicom::dimse::{self, sop, status};
use cameleer_dicom::object::{DataSet, Element};
use cameleer_dicom::scu::{self, ScuConfig};
use cameleer_dicom::tag::tags;
use cameleer_dicom::testing::{Instance, TestPacs};
use cameleer_dicom::ts;
use cameleer_dicom::vr::Vr;
use cameleer_dicom::{DicomFile, ReadOptions};
use cameleer_tls::camel_support::EndpointTls;

fn cfg(pacs: &TestPacs) -> ScuConfig {
    let mut c = ScuConfig::new("CAMELEER", "PACS", "127.0.0.1", pacs.port());
    c.timeout = std::time::Duration::from_secs(10);
    c
}

fn query(level: &str, keys: &[(u32, Vr, &str)]) -> DataSet {
    let mut q = DataSet::new();
    q.insert(Element::string(tags::QUERY_RETRIEVE_LEVEL, Vr::CS, level));
    for (t, vr, v) in keys {
        q.insert(Element::string(*t, *vr, v));
    }
    q
}

#[test]
fn echo_store_find_get() {
    let pacs = TestPacs::start("PACS");
    let c = cfg(&pacs);
    assert_eq!(scu::echo(&c).unwrap().status, status::SUCCESS);
    let files: Vec<Vec<u8>> = vec![
        Instance::ct("P1", 1, 1, 1).part10(),
        Instance::ct("P1", 1, 1, 2).part10_in(ts::IMPLICIT_VR_LITTLE_ENDIAN),
        Instance::ct("P1", 1, 2, 1).part10_in(ts::DEFLATED_EXPLICIT_VR_LITTLE_ENDIAN),
        Instance::ct("P2", 2, 1, 1).part10_in(ts::EXPLICIT_VR_BIG_ENDIAN),
    ];
    let out = scu::store(&c, &files, &[]).unwrap();
    assert_eq!(out.len(), 4);
    assert!(out.iter().all(|(_, o)| o.status == 0), "{out:?}");
    assert_eq!(pacs.archive.len(), 4);
    // what arrived is the same data set
    let stored = pacs.archive.all();
    let first = DicomFile::parse(&stored[0], ReadOptions::default()).unwrap();
    assert_eq!(first.dataset, Instance::ct("P1", 1, 1, 1).dataset());
    assert_eq!(first.meta.string(tags::SOURCE_APPLICATION_ENTITY_TITLE).unwrap(), "CAMELEER");

    // C-FIND at the study, series and image levels
    let (studies, o) = scu::find(&c, sop::STUDY_ROOT_FIND, &query("STUDY", &[(tags::PATIENT_ID, Vr::LO, "P1"), (tags::STUDY_INSTANCE_UID, Vr::UI, ""), (tags::PATIENT_NAME, Vr::PN, "")])).unwrap();
    assert_eq!(o.status, 0);
    assert_eq!(studies.len(), 1);
    assert_eq!(studies[0].string(tags::PATIENT_NAME).unwrap(), "Test^P1");
    let (series, _) = scu::find(&c, sop::STUDY_ROOT_FIND, &query("SERIES", &[(tags::STUDY_INSTANCE_UID, Vr::UI, "2.25.144682546011839860307890821118846661177.1"), (tags::SERIES_INSTANCE_UID, Vr::UI, "")])).unwrap();
    assert_eq!(series.len(), 2);
    let (images, _) = scu::find(&c, sop::PATIENT_ROOT_FIND, &query("IMAGE", &[(tags::PATIENT_ID, Vr::LO, "P*"), (tags::SOP_INSTANCE_UID, Vr::UI, "")])).unwrap();
    assert_eq!(images.len(), 4);
    let (none, _) = scu::find(&c, sop::STUDY_ROOT_FIND, &query("STUDY", &[(tags::STUDY_DATE, Vr::DA, "20250101-20251231")])).unwrap();
    assert!(none.is_empty());

    // C-GET of one series
    let (got, ops) = scu::get(&c, sop::STUDY_ROOT_GET, &query("SERIES", &[(tags::SERIES_INSTANCE_UID, Vr::UI, "2.25.144682546011839860307890821118846661177.1.1")]), &[dimse::COMMON_STORAGE[5].to_string()], &[]).unwrap();
    assert_eq!((ops.status, ops.completed, ops.failed), (0, 2, 0), "{ops:?}");
    assert_eq!(got.len(), 2);
    let g = DicomFile::parse(&got[0], ReadOptions::default()).unwrap();
    assert_eq!(g.dataset.string(tags::SERIES_INSTANCE_UID).unwrap(), "2.25.144682546011839860307890821118846661177.1.1");
    assert!(pacs.requests.lock().unwrap().iter().any(|(c, a)| c == "C-GET" && a == "CAMELEER"));
}

#[test]
fn move_to_a_known_destination_and_unknown_one() {
    let dest = TestPacs::start("DEST");
    let dest_port = dest.port();
    let pacs = TestPacs::start_with("PACS", |c| {
        c.ae_table.insert("DEST".into(), ("127.0.0.1".into(), dest_port));
    });
    for i in 1..=3 {
        pacs.archive.add(Instance::ct("P1", 7, 1, i).part10());
    }
    pacs.archive.add(Instance::ct("P2", 8, 1, 1).part10());
    let c = cfg(&pacs);
    let ops = scu::move_(&c, sop::STUDY_ROOT_MOVE, &query("STUDY", &[(tags::STUDY_INSTANCE_UID, Vr::UI, "2.25.144682546011839860307890821118846661177.7")]), "DEST").unwrap();
    assert_eq!((ops.status, ops.completed, ops.failed, ops.warning), (0, 3, 0, 0), "{ops:?}");
    assert_eq!(dest.archive.len(), 3);
    // the move SCP calls the destination with its own AE title
    assert!(dest.requests.lock().unwrap().iter().all(|(c, a)| c == "C-STORE" && a == "PACS"));
    let ops = scu::move_(&c, sop::STUDY_ROOT_MOVE, &query("STUDY", &[(tags::STUDY_INSTANCE_UID, Vr::UI, "2.25.144682546011839860307890821118846661177.8")]), "NOWHERE").unwrap();
    assert_eq!(ops.status, status::MOVE_DESTINATION_UNKNOWN);
    assert!(ops.error_comment.unwrap().contains("NOWHERE"));
}

#[test]
fn rejections_and_negotiation() {
    // called AE checked
    let strict = TestPacs::start_with("PACS", |c| c.check_called_ae = true);
    let mut c = cfg(&strict);
    c.called_ae = "OTHER".into();
    let e = scu::echo(&c).unwrap_err();
    assert_eq!(e.kind, "DicomAssociationException");
    assert!(e.message.contains("called AE title not recognized"), "{}", e.message);
    // calling AE allow-list
    let listed = TestPacs::start_with("PACS", |c| c.calling_aes = vec!["MODALITY1".into()]);
    let e = scu::echo(&cfg(&listed)).unwrap_err();
    assert!(e.message.contains("calling AE title not recognized"), "{}", e.message);
    // an SCP accepting only verification refuses storage
    let echo_only = TestPacs::start_with("PACS", |c| c.sop_classes = cameleer_dicom::scp::SopFilter::parse("verification").unwrap());
    let e = scu::store(&cfg(&echo_only), &[Instance::ct("P1", 1, 1, 1).part10()], &[]).unwrap_err();
    assert!(e.message.contains("accepted no presentation context"), "{}", e.message);
    // an SCP accepting only implicit VR gets the explicit object converted
    let implicit_only = TestPacs::start_with("PACS", |c| c.transfer_syntaxes = vec![ts::IMPLICIT_VR_LITTLE_ENDIAN.into()]);
    let out = scu::store(&cfg(&implicit_only), &[Instance::ct("P1", 1, 1, 1).part10()], &[]).unwrap();
    assert_eq!(out[0].1.status, 0);
    let f = DicomFile::parse(&implicit_only.archive.all()[0], ReadOptions::default()).unwrap();
    assert_eq!(f.transfer_syntax_uid(), ts::IMPLICIT_VR_LITTLE_ENDIAN);
    assert_eq!(f.dataset, Instance::ct("P1", 1, 1, 1).dataset());
    // a compressed object the peer will only take uncompressed fails with a clear error
    let mut jpeg = DicomFile::new(Instance::ct("P1", 1, 1, 9).dataset(), ts::JPEG_BASELINE);
    jpeg.dataset.insert(Element { tag: tags::PIXEL_DATA, vr: Vr::OB, value: cameleer_dicom::object::Value::Fragments(vec![vec![], vec![0xFF, 0xD8, 0xFF, 0xD9]]) });
    let e = scu::store(&cfg(&implicit_only), &[jpeg.to_bytes().unwrap()], &[]).unwrap_err();
    assert_eq!(e.kind, "DicomTranscodeException");
    assert!(e.message.contains("JPEG Baseline"), "{}", e.message);
    // ... and is kept compressed when the peer accepts it
    let any = TestPacs::start("PACS");
    let out = scu::store(&cfg(&any), &[jpeg.to_bytes().unwrap()], &[]).unwrap();
    assert_eq!(out[0].1.status, 0);
    let f = DicomFile::parse(&any.archive.all()[0], ReadOptions::default()).unwrap();
    assert_eq!(f.transfer_syntax_uid(), ts::JPEG_BASELINE);
    assert_eq!(f.dataset, jpeg.dataset);
    // connection refused
    let mut c = cfg(&any);
    c.port = 1;
    assert!(scu::echo(&c).is_err());
}

#[test]
fn small_pdus_split_large_objects() {
    let pacs = TestPacs::start_with("PACS", |c| c.max_pdu_length = 4096);
    let mut c = cfg(&pacs);
    c.max_pdu_length = 4096;
    let mut ds = Instance::ct("P1", 1, 1, 1).dataset();
    ds.insert(Element::bytes(tags::PIXEL_DATA, Vr::OW, (0..200_000u32).map(|i| i as u8).collect()));
    let big = DicomFile::new(ds.clone(), ts::EXPLICIT_VR_LITTLE_ENDIAN).to_bytes().unwrap();
    assert_eq!(scu::store(&c, &[big], &[]).unwrap()[0].1.status, 0);
    let f = DicomFile::parse(&pacs.archive.all()[0], ReadOptions::default()).unwrap();
    assert_eq!(f.dataset, ds);
}

#[test]
fn dimse_over_tls() {
    let pacs = TestPacs::start_with("PACS", |c| c.tls = Some(EndpointTls::new(Arc::new(cameleer_tls::testing::server_params()))));
    let mut c = cfg(&pacs);
    c.host = "localhost".into();
    c.tls = Some(EndpointTls::new(Arc::new(cameleer_tls::testing::client_params())));
    assert_eq!(scu::echo(&c).unwrap().status, 0);
    assert_eq!(scu::store(&c, &[Instance::ct("P1", 1, 1, 1).part10()], &[]).unwrap()[0].1.status, 0);
    assert_eq!(pacs.archive.len(), 1);
    // a client that does not trust the server's certificate fails the handshake
    c.tls = Some(EndpointTls::new(Arc::new(cameleer_tls::testing::untrusting_client_params())));
    let e = scu::echo(&c).unwrap_err();
    assert!(e.kind.contains("SSL"), "{e:?}");
    // a plain client talking to a TLS SCP gets no association
    let mut plain = cfg(&pacs);
    plain.timeout = std::time::Duration::from_secs(3);
    assert!(scu::echo(&plain).is_err());
}

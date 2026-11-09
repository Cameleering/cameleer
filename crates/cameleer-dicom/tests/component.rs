//! The dicom: component in routes: a Cameleer SCP whose C-STORE, C-FIND, C-MOVE and C-GET are
//! answered by routes, driven by Cameleer SCU producers and by the SCU functions; and producers
//! against the test PACS.

use cameleer_core::prelude::*;
use cameleer_dicom::dimse::sop;
use cameleer_dicom::object::{DataSet, Element};
use cameleer_dicom::scu::{self, ScuConfig};
use cameleer_dicom::tag::tags;
use cameleer_dicom::testing::{Instance, MemoryArchive, TestPacs};
use cameleer_dicom::vr::Vr;
use cameleer_dicom::{DicomFile, ReadOptions};

fn ctx() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_dicom::register(&ctx);
    ctx
}

fn object(pairs: &[(&str, &str)]) -> Value {
    let mut m = cameleer_core::value::ObjectMap::new();
    for (k, v) in pairs {
        m.insert(k.to_string(), Value::from(*v));
    }
    Value::Object(m)
}

#[test]
fn producer_operations_against_a_pacs() {
    let dest = TestPacs::start("DEST");
    let dport = dest.port();
    let pacs = TestPacs::start_with("PACS", |c| {
        c.ae_table.insert("DEST".into(), ("127.0.0.1".into(), dport));
    });
    let ctx = ctx();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let base = format!("dicom:PACS@127.0.0.1:{}?callingAet=ROUTER&timeout=10s", pacs.port());
    // echo
    let mut ex = ctx.create_exchange();
    ex.set_header("CamelDicomOperation", "echo");
    let ex = t.send(&base, ex);
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
    assert_eq!(ex.header("CamelDicomStatus"), Some(&Value::Int(0)));
    // store one and a batch
    t.request_body(&base, Value::Bytes(Instance::ct("P1", 1, 1, 1).part10())).unwrap();
    let batch = Value::Array((2..=4).map(|i| Value::Bytes(Instance::ct("P1", 1, 1, i).part10())).collect());
    let mut ex = ctx.create_exchange();
    ex.set_body(batch);
    let ex = t.send(&base, ex);
    assert_eq!(ex.header("CamelDicomCompleted"), Some(&Value::Int(3)));
    assert_eq!(pacs.archive.len(), 4);
    assert!(pacs.requests.lock().unwrap().iter().all(|(_, a)| a == "ROUTER"));
    // a Value tree is marshalled before sending
    let tree = cameleer_dicom::model::file_to_value(&DicomFile::parse(&Instance::ct("P2", 2, 1, 1).part10(), ReadOptions::default()).unwrap());
    t.request_body(&base, tree).unwrap();
    assert_eq!(pacs.archive.len(), 5);
    // find with a keyword map; the result is an array of Value trees
    let found = t.request_body(&format!("{}&operation=find&queryLevel=SERIES", base), object(&[("PatientID", "P1"), ("SeriesInstanceUID", ""), ("Modality", "")])).unwrap();
    let arr = found.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0].get("0020000E").unwrap().get("Value").unwrap().as_array().unwrap()[0], Value::from("2.25.144682546011839860307890821118846661177.1.1"));
    // get
    let got = t.request_body(&format!("{}&operation=get&queryLevel=STUDY", base), object(&[("StudyInstanceUID", "2.25.144682546011839860307890821118846661177.2")])).unwrap();
    assert_eq!(got.as_array().unwrap().len(), 1);
    // move to DEST
    let mut ex = ctx.create_exchange();
    ex.set_body(object(&[("StudyInstanceUID", "2.25.144682546011839860307890821118846661177.1")]));
    let ex = t.send(&format!("{}&operation=move&moveDestination=DEST", base), ex);
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
    assert_eq!(ex.header("CamelDicomCompleted"), Some(&Value::Int(4)));
    assert_eq!(dest.archive.len(), 4);
    // an unknown destination is a failure status
    let mut ex = ctx.create_exchange();
    ex.set_body(object(&[("StudyInstanceUID", "2.25.144682546011839860307890821118846661177.1")]));
    let ex = t.send(&format!("{}&operation=move&moveDestination=NOPE", base), ex);
    let e = ex.exception.unwrap();
    assert_eq!(e.kind, "DicomStatusException");
    assert!(e.message.contains("0xA801"), "{}", e.message);
    ctx.stop().unwrap();
}

#[test]
fn scp_routes_answer_store_find_move_get() {
    let archive = MemoryArchive::new();
    let dest = TestPacs::start("DEST");
    let ctx = ctx();
    let a1 = archive.clone();
    let a2 = archive.clone();
    let a3 = archive.clone();
    let uri = format!("dicom:ROUTER@127.0.0.1:0?moveDestinations=DEST=127.0.0.1:{}&timeout=10s", dest.port());
    let uri2 = uri.clone();
    ctx.add_routes(move |b| {
        b.from(&uri2)
            .choice()
            .when(header("CamelDicomCommand").is_equal_to("C-STORE"))
            .process(move |ex: &mut Exchange| {
                if ex.header("CamelDicomCallingAet").map(|v| v.to_string()) == Some("BAD".into()) {
                    return Err(CamelError::new("IOException", "disk full"));
                }
                if let Value::Bytes(b) = ex.body() {
                    a1.add(b.clone());
                }
                Ok(())
            })
            .when(header("CamelDicomCommand").is_equal_to("C-FIND"))
            .process(move |ex: &mut Exchange| {
                let ident = cameleer_dicom::model::value_to_dataset(ex.body(), &Default::default())?;
                let found: Vec<Value> = a2.find(&ident).iter().map(|d| cameleer_dicom::model::dataset_to_value(d, &Default::default())).collect();
                ex.set_body(Value::Array(found));
                Ok(())
            })
            .otherwise()
            .process(move |ex: &mut Exchange| {
                let ident = cameleer_dicom::model::value_to_dataset(ex.body(), &Default::default())?;
                ex.set_body(Value::Array(a3.matching(&ident).into_iter().map(|(_, b)| Value::Bytes(b)).collect()));
                Ok(())
            })
            .end()
            .to("mock:seen");
    })
    .unwrap();
    ctx.start().unwrap();
    let port = cameleer_dicom::component::local_port(&ctx, &uri).unwrap();
    let mut c = ScuConfig::new("MODALITY", "ROUTER", "127.0.0.1", port);
    c.timeout = std::time::Duration::from_secs(10);
    assert_eq!(scu::echo(&c).unwrap().status, 0);
    let out = scu::store(&c, &[Instance::ct("P1", 5, 1, 1).part10(), Instance::ct("P1", 5, 1, 2).part10()], &[]).unwrap();
    assert!(out.iter().all(|(_, o)| o.status == 0));
    assert_eq!(archive.len(), 2);
    let seen = ctx.mock_endpoint("mock:seen").unwrap();
    let first = &seen.received_exchanges()[0];
    assert_eq!(first.header("CamelDicomCallingAet").unwrap().to_string(), "MODALITY");
    assert_eq!(first.header("CamelDicomSopInstanceUid").unwrap().to_string(), "2.25.144682546011839860307890821118846661177.5.1.1");
    // a failing route answers with a failure status and the reason
    let mut bad = c.clone();
    bad.calling_ae = "BAD".into();
    let out = scu::store(&bad, &[Instance::ct("P9", 9, 1, 1).part10()], &[]).unwrap();
    assert_eq!(out[0].1.status, 0xA700);
    assert!(out[0].1.error_comment.as_deref().unwrap().contains("disk full"));
    // C-FIND answered by the route
    let mut q = DataSet::new();
    q.insert(Element::string(tags::QUERY_RETRIEVE_LEVEL, Vr::CS, "IMAGE"));
    q.insert(Element::string(tags::STUDY_INSTANCE_UID, Vr::UI, "2.25.144682546011839860307890821118846661177.5"));
    q.insert(Element::string(tags::SOP_INSTANCE_UID, Vr::UI, ""));
    let (r, o) = scu::find(&c, sop::STUDY_ROOT_FIND, &q).unwrap();
    assert_eq!((r.len(), o.status), (2, 0));
    // C-GET answered by the route
    let (files, ops) = scu::get(&c, sop::STUDY_ROOT_GET, &q, &[sop_ct()], &[]).unwrap();
    assert_eq!((files.len(), ops.completed), (2, 2));
    // C-MOVE: the route returns the instances, the SCP sends them to DEST
    let ops = scu::move_(&c, sop::STUDY_ROOT_MOVE, &q, "DEST").unwrap();
    assert_eq!((ops.status, ops.completed), (0, 2), "{ops:?}");
    assert_eq!(dest.archive.len(), 2);
    assert!(dest.requests.lock().unwrap().iter().all(|(_, a)| a == "ROUTER"));
    ctx.stop().unwrap();
}

fn sop_ct() -> String {
    cameleer_dicom::testing::CT_IMAGE_STORAGE.to_string()
}

#[test]
fn router_from_modality_to_pacs() {
    // the edge gateway in one route: SCP in, filter on tags, SCU out
    let pacs = TestPacs::start("PACS");
    let ctx = ctx();
    let uri = "dicom:GATEWAY@127.0.0.1:0?timeout=10s";
    let out = format!("dicom:PACS@127.0.0.1:{}?callingAet=GATEWAY", pacs.port());
    ctx.add_routes(move |b| {
        b.from(uri).filter(simple("${dicom.Modality} == 'CT'")).to(out.as_str());
    })
    .unwrap();
    ctx.start().unwrap();
    let port = cameleer_dicom::component::local_port(&ctx, uri).unwrap();
    let c = ScuConfig::new("CT01", "GATEWAY", "127.0.0.1", port);
    let mut mr = Instance::ct("P1", 3, 1, 2);
    mr.modality = "MR".into();
    scu::store(&c, &[Instance::ct("P1", 3, 1, 1).part10(), mr.part10()], &[]).unwrap();
    assert_eq!(pacs.archive.len(), 1);
    let f = DicomFile::parse(&pacs.archive.all()[0], ReadOptions::default()).unwrap();
    assert_eq!(f.dataset, Instance::ct("P1", 3, 1, 1).dataset());
    ctx.stop().unwrap();
}

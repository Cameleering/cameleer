//! DICOMweb: the dicomweb: client against a dicomweb: server backed by an in-memory archive
//! (STOW-RS, QIDO-RS, WADO-RS, metadata, bulk data, errors), and a DIMSE to DICOMweb gateway.

use std::sync::Arc;

use cameleer_core::prelude::*;
use cameleer_dicom::scu::{self, ScuConfig};
use cameleer_dicom::tag::tags;
use cameleer_dicom::testing::{Instance, MemoryArchive, TEST_UID_ROOT};
use cameleer_dicom::{ts, DicomFile, ReadOptions};
use cameleer_http::client::HttpClient;

fn ctx() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    cameleer_dicom::register(&ctx);
    ctx
}

/// A DICOMweb server route over an archive; returns the port.
fn server(ctx: &CamelContext, archive: Arc<MemoryArchive>) -> u16 {
    let (a1, a2, a3) = (archive.clone(), archive.clone(), archive);
    ctx.add_routes(move |b| {
        b.from("dicomweb:/dicom-web?port=0")
            .choice()
            .when(header("CamelDicomWebOperation").is_equal_to("stow"))
            .process(move |ex: &mut Exchange| {
                if ex.header("CamelDicomSopInstanceUid").map(|v| v.to_string()).unwrap_or_default().ends_with(".666") {
                    return Err(CamelError::new("IOException", "rejected"));
                }
                if let Value::Bytes(b) = ex.body() {
                    a1.add(b.clone());
                }
                Ok(())
            })
            .when(header("CamelDicomWebOperation").is_equal_to("qido"))
            .process(move |ex: &mut Exchange| {
                let ident = cameleer_dicom::model::value_to_dataset(ex.body(), &Default::default())?;
                ex.set_body(Value::Array(a2.find(&ident).iter().map(|d| cameleer_dicom::model::dataset_to_value(d, &Default::default())).collect()));
                Ok(())
            })
            .otherwise()
            .process(move |ex: &mut Exchange| {
                let ident = cameleer_dicom::model::value_to_dataset(ex.body(), &Default::default())?;
                ex.set_body(Value::Array(a3.matching(&ident).into_iter().map(|(_, b)| Value::Bytes(b)).collect()));
                Ok(())
            })
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    cameleer_dicom::dicomweb::local_port(ctx, "dicomweb:/dicom-web?port=0").expect("bound")
}

fn object(pairs: &[(&str, &str)]) -> Value {
    let mut m = cameleer_core::value::ObjectMap::new();
    for (k, v) in pairs {
        m.insert(k.to_string(), Value::from(*v));
    }
    Value::Object(m)
}

fn first_value(tree: &Value, tag: &str) -> Value {
    tree.get(tag).and_then(|e| e.get("Value")).and_then(|v| v.as_array()).and_then(|a| a.first()).cloned().unwrap_or(Value::Null)
}

#[test]
fn stow_qido_wado_metadata_bulkdata() {
    let archive = MemoryArchive::new();
    let ctx = ctx();
    let port = server(&ctx, archive.clone());
    let base = format!("dicomweb:http://127.0.0.1:{}/dicom-web", port);
    let t = ctx.create_producer_template();
    // STOW-RS: a batch of three in different transfer syntaxes
    let insts = [Instance::ct("WEB1", 10, 1, 1), Instance::ct("WEB1", 10, 1, 2), Instance::ct("WEB2", 11, 1, 1)];
    let batch = Value::Array(vec![
        Value::Bytes(insts[0].part10()),
        Value::Bytes(insts[1].part10_in(ts::IMPLICIT_VR_LITTLE_ENDIAN)),
        Value::Bytes(insts[2].part10_in(ts::EXPLICIT_VR_BIG_ENDIAN)),
    ]);
    let mut ex = ctx.create_exchange();
    ex.set_body(batch);
    let ex = t.send(&base, ex);
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
    assert_eq!(ex.header("CamelDicomCompleted"), Some(&Value::Int(3)));
    assert_eq!(ex.header("CamelDicomHttpStatus"), Some(&Value::Int(200)));
    let refs = ex.body().get("00081199").unwrap().get("Value").unwrap().as_array().unwrap().clone();
    assert_eq!(refs.len(), 3);
    assert!(first_value(&refs[0], "00081190").to_string().contains("/dicom-web/studies/"));
    assert_eq!(archive.len(), 3);

    // QIDO-RS at the three levels
    let studies = t.request_body(&format!("{}?operation=qido", base), object(&[("PatientID", "WEB1"), ("PatientName", "")])).unwrap();
    let studies = studies.as_array().unwrap();
    assert_eq!(studies.len(), 1);
    assert_eq!(first_value(&studies[0], "0020000D"), Value::from(format!("{}.10", TEST_UID_ROOT)));
    assert_eq!(first_value(&studies[0], "00100010").get("Alphabetic"), Some(&Value::from("Test^WEB1")));
    let mut ex = ctx.create_exchange();
    ex.set_header("CamelDicomStudyInstanceUid", format!("{}.10", TEST_UID_ROOT));
    let ex = t.send(&format!("{}?operation=qido&queryLevel=instances&limit=1", base), ex);
    assert_eq!(ex.header("CamelDicomResultCount"), Some(&Value::Int(1)));
    let all = t.request_body(&format!("{}?operation=qido&queryLevel=series", base), Value::Null).unwrap();
    assert_eq!(all.as_array().unwrap().len(), 2);
    let none = t.request_body(&format!("{}?operation=qido", base), object(&[("PatientID", "NOBODY")])).unwrap();
    assert!(none.as_array().unwrap().is_empty());

    // WADO-RS: a study, then one instance converted to implicit VR
    let mut ex = ctx.create_exchange();
    ex.set_header("CamelDicomStudyInstanceUid", format!("{}.10", TEST_UID_ROOT));
    let ex = t.send(&format!("{}?operation=wado", base), ex);
    let files = ex.body().as_array().unwrap().clone();
    assert_eq!(files.len(), 2);
    let Value::Bytes(b) = &files[0] else { panic!() };
    let f = DicomFile::parse(b, ReadOptions::default()).unwrap();
    assert!(f.dataset == insts[0].dataset() || f.dataset == insts[1].dataset());
    let mut ex = ctx.create_exchange();
    ex.set_body(object(&[("StudyInstanceUID", &insts[0].study_uid), ("SeriesInstanceUID", &insts[0].series_uid), ("SOPInstanceUID", &insts[0].instance_uid)]));
    let ex = t.send(&format!("{}?operation=wado&transferSyntax={}", base, ts::IMPLICIT_VR_LITTLE_ENDIAN), ex);
    let files = ex.body().as_array().unwrap().clone();
    let Value::Bytes(b) = &files[0] else { panic!() };
    let f = DicomFile::parse(b, ReadOptions::default()).unwrap();
    assert_eq!(f.transfer_syntax_uid(), ts::IMPLICIT_VR_LITTLE_ENDIAN);
    assert_eq!(f.dataset, insts[0].dataset());

    // metadata: pixel data as a BulkDataURI, then fetch it
    let mut ex = ctx.create_exchange();
    ex.set_header("CamelDicomStudyInstanceUid", insts[2].study_uid.clone());
    let ex = t.send(&format!("{}?operation=metadata", base), ex);
    let md = ex.body().as_array().unwrap().clone();
    assert_eq!(md.len(), 1);
    assert_eq!(first_value(&md[0], "00100020"), Value::from("WEB2"));
    // the 32-byte test image is under the 1024-byte threshold, so it is inline
    assert!(md[0].get("7FE00010").unwrap().get("InlineBinary").is_some());
    let url = format!("http://127.0.0.1:{}/dicom-web/studies/{}/series/{}/instances/{}/bulkdata/7FE00010", port, insts[2].study_uid, insts[2].series_uid, insts[2].instance_uid);
    let r = HttpClient::new().get(&url).unwrap();
    assert_eq!(r.status, 200);
    let parts = cameleer_dicom::dicomweb::split_multipart(r.headers.get("Content-Type").unwrap(), &r.body).unwrap();
    assert_eq!(parts[0].body, insts[2].dataset().get(tags::PIXEL_DATA).unwrap().raw().unwrap());
    ctx.stop().unwrap();
}

#[test]
fn stow_failures_and_http_errors() {
    let archive = MemoryArchive::new();
    let ctx = ctx();
    let port = server(&ctx, archive.clone());
    let base = format!("dicomweb:http://127.0.0.1:{}/dicom-web", port);
    let t = ctx.create_producer_template();
    // one of two rejected by the route: 202, a DicomWebException by default
    let batch = Value::Array(vec![Value::Bytes(Instance::ct("F", 20, 1, 1).part10()), Value::Bytes(Instance::ct("F", 20, 1, 666).part10())]);
    let mut ex = ctx.create_exchange();
    ex.set_body(batch.clone());
    let ex = t.send(&base, ex);
    let e = ex.exception.clone().unwrap();
    assert_eq!(e.kind, "DicomWebException");
    assert!(e.message.contains("1 of 2") && e.message.contains("0xA700"), "{}", e.message);
    assert_eq!(ex.header("CamelDicomHttpStatus"), Some(&Value::Int(202)));
    // ... or tolerated
    let mut ex = ctx.create_exchange();
    ex.set_body(batch);
    let ex = t.send(&format!("{}?failOnPartialSuccess=false", base), ex);
    assert!(ex.exception.is_none());
    assert_eq!(ex.header("CamelDicomFailed"), Some(&Value::Int(1)));
    // an instance posted to another study's URL: 409
    let mut ex = ctx.create_exchange();
    ex.set_body(Value::Bytes(Instance::ct("F", 21, 1, 1).part10()));
    ex.set_header("CamelDicomStudyInstanceUid", "1.2.3");
    let ex = t.send(&base, ex);
    assert!(ex.exception.unwrap().message.contains("HTTP 409"));
    // raw HTTP: wrong content type, unknown resource, frames
    let c = HttpClient::new();
    let r = c.post(&format!("http://127.0.0.1:{}/dicom-web/studies", port), "text/plain", b"x").unwrap();
    assert_eq!(r.status, 415);
    assert_eq!(c.get(&format!("http://127.0.0.1:{}/dicom-web/nothing", port)).unwrap().status, 404);
    assert_eq!(c.get(&format!("http://127.0.0.1:{}/dicom-web/studies/1/series/2/instances/3/frames/1", port)).unwrap().status, 501);
    assert_eq!(c.get(&format!("http://127.0.0.1:{}/dicom-web/studies/9.9.9", port)).unwrap().status, 404);
    let r = c.get(&format!("http://127.0.0.1:{}/dicom-web/studies?NoSuchKeyword=1", port)).unwrap();
    assert_eq!(r.status, 400);
    // a server that is not there
    assert!(t.request_body("dicomweb:http://127.0.0.1:1/dicom-web?timeout=2s", Value::Bytes(Instance::ct("F", 22, 1, 1).part10())).is_err());
    ctx.stop().unwrap();
}

#[test]
fn dimse_to_dicomweb_gateway() {
    // a modality speaks DIMSE; the cloud archive speaks DICOMweb
    let archive = MemoryArchive::new();
    let cloud = ctx();
    let port = server(&cloud, archive.clone());
    let gw = ctx();
    let out = format!("dicomweb:http://127.0.0.1:{}/dicom-web?bearerToken=secret-token", port);
    gw.add_routes(move |b| {
        b.from("dicom:GATEWAY@127.0.0.1:0").to(out.as_str());
    })
    .unwrap();
    gw.start().unwrap();
    let p = cameleer_dicom::component::local_port(&gw, "dicom:GATEWAY@127.0.0.1:0").unwrap();
    let c = ScuConfig::new("MOD1", "GATEWAY", "127.0.0.1", p);
    let o = scu::store(&c, &[Instance::ct("G1", 30, 1, 1).part10(), Instance::ct("G1", 30, 1, 2).part10()], &[]).unwrap();
    assert!(o.iter().all(|(_, x)| x.status == 0));
    assert_eq!(archive.len(), 2);
    let f = DicomFile::parse(&archive.all()[0], ReadOptions::default()).unwrap();
    assert_eq!(f.dataset, Instance::ct("G1", 30, 1, 1).dataset());
    gw.stop().unwrap();
    cloud.stop().unwrap();
}

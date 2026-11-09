//! The medical examples in `examples/medical/` run end to end: a modality sends over DIMSE, a
//! Cameleer DICOMweb server stands in for the cloud store, an HTTP route for the inference
//! service and the FHIR server. Endpoints are redirected by replacing `{{env:...}}`
//! placeholders in the file text; the routes run unchanged.

// needs every component family (the default features)
#![cfg(feature = "full")]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cameleer_core::prelude::*;
use cameleer_dicom::scu::{self, ScuConfig};
use cameleer_dicom::tag::tags;
use cameleer_dicom::testing::{Instance, MemoryArchive};
use cameleer_dicom::{DicomFile, ReadOptions};

fn example(name: &str, replace: &[(&str, &str)]) -> String {
    let path = format!("{}/../../examples/medical/{}", env!("CARGO_MANIFEST_DIR"), name);
    let mut text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {}", path, e));
    for (from, to) in replace {
        assert!(text.contains(from), "{} does not contain {}", name, from);
        text = text.replace(from, to);
    }
    text
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn wait_for(what: &str, f: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {}", what);
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn temp_dir(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("cameleer-dicom-example-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.to_string_lossy().replace('\\', "/")
}

/// A DICOMweb server over an archive on a fixed port.
fn cloud(port: u16, archive: Arc<MemoryArchive>) -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    cameleer_dicom::register(&ctx);
    let uri = format!("dicomweb:/dicom-web?port={}", port);
    ctx.add_routes(move |b| {
        b.from(uri.as_str()).process(move |ex: &mut Exchange| {
            if let Value::Bytes(b) = ex.body() {
                archive.add(b.clone());
            }
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    ctx
}

#[test]
fn modality_to_cloud_example() {
    let web_port = free_port();
    let dicom_port = free_port();
    let park = temp_dir("park");
    let url = format!("http://127.0.0.1:{}/dicom-web", web_port);
    let archive = MemoryArchive::new();
    let cloud_ctx = cloud(web_port, archive.clone());
    let mut main = cameleer_main::new_main();
    main.add_route_source(
        "modality-to-cloud.camel.yaml",
        example(
            "modality-to-cloud.camel.yaml",
            &[
                ("{{env:DICOMWEB_URL:http://localhost:8042/dicom-web}}", &url),
                ("{{env:DICOM_PORT:11112}}", &dicom_port.to_string()),
                ("{{env:PARK_DIR:parked}}", &park),
                ("delay=60000", "delay=100"),
            ],
        ),
    );
    main.start().unwrap();
    let mut mr = Instance::ct("P1", 60, 2, 1);
    mr.modality = "MR".into();
    let mut us = Instance::ct("P1", 60, 3, 1);
    us.modality = "US".into();
    let c = ScuConfig::new("CT01", "GATEWAY", "127.0.0.1", dicom_port);
    let out = scu::store(&c, &[Instance::ct("P1", 60, 1, 1).part10(), mr.part10(), us.part10()], &[]).unwrap();
    assert!(out.iter().all(|(_, o)| o.status == 0), "{out:?}");
    assert_eq!(archive.len(), 2, "CT and MR reach the cloud, US is filtered out");
    // an unknown modality is refused at association time
    let mut bad = c.clone();
    bad.calling_ae = "ROGUE".into();
    assert!(scu::echo(&bad).is_err());
    // the cloud goes away: the instance is parked and the modality still gets success
    cloud_ctx.stop().unwrap();
    let late = Instance::ct("P1", 60, 1, 2);
    let out = scu::store(&c, &[late.part10()], &[]).unwrap();
    assert_eq!(out[0].1.status, 0);
    // ... and uploaded by the retry route once the cloud is back
    let archive2 = MemoryArchive::new();
    let cloud_ctx = cloud(web_port, archive2.clone());
    wait_for("the parked instance to be uploaded", || archive2.len() == 1);
    let f = DicomFile::parse(&archive2.all()[0], ReadOptions::default()).unwrap();
    assert_eq!(f.dataset, late.dataset());
    main.stop().unwrap();
    cloud_ctx.stop().unwrap();
}

#[test]
fn ai_inference_loop_example() {
    let http_port = free_port();
    let dicom_port = free_port();
    let seen_by_model: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
    let reports: Arc<Mutex<Vec<String>>> = Arc::default();
    let mocks = CamelContext::new();
    cameleer_http::register(&mocks);
    let (m1, r1) = (seen_by_model.clone(), reports.clone());
    let infer = format!("platform-http:/infer?port={}", http_port);
    let fhir = format!("platform-http:/fhir/DiagnosticReport?port={}", http_port);
    mocks
        .add_routes(move |b| {
            b.from(infer.as_str()).process(move |ex: &mut Exchange| {
                if let Value::Bytes(b) = ex.body() {
                    m1.lock().unwrap().push(b.clone());
                }
                ex.set_body(r#"{"finding": "No acute intracranial abnormality", "score": 0.03}"#);
                ex.set_header("Content-Type", "application/json");
                Ok(())
            });
            b.from(fhir.as_str()).process(move |ex: &mut Exchange| {
                r1.lock().unwrap().push(ex.body().to_string());
                ex.set_header("CamelHttpResponseCode", 201);
                ex.set_body("{}");
                Ok(())
            });
        })
        .unwrap();
    mocks.start().unwrap();
    let mut main = cameleer_main::new_main();
    main.add_route_source(
        "ai-inference-loop.camel.yaml",
        example(
            "ai-inference-loop.camel.yaml",
            &[
                ("{{env:INFERENCE_URL:http://localhost:9000/infer}}", &format!("http://127.0.0.1:{}/infer", http_port)),
                ("{{env:FHIR_URL:http://localhost:8080/fhir}}", &format!("http://127.0.0.1:{}/fhir", http_port)),
                ("{{env:DICOM_PORT:11113}}", &dicom_port.to_string()),
            ],
        ),
    );
    main.start().unwrap();
    let inst = Instance::ct("PAT-AI", 61, 1, 1); // series description "AXIAL 5MM"
    let mut localizer = Instance::ct("PAT-AI", 61, 2, 1);
    localizer.modality = "MR".into();
    let c = ScuConfig::new("CT01", "AI_GATEWAY", "127.0.0.1", dicom_port);
    let out = scu::store(&c, &[inst.part10(), localizer.part10()], &[]).unwrap();
    assert!(out.iter().all(|(_, o)| o.status == 0), "{out:?}");
    assert_eq!(seen_by_model.lock().unwrap().len(), 1, "only the axial CT goes to the model");
    // the model sees de-identified data only
    let sent = DicomFile::parse(&seen_by_model.lock().unwrap()[0], ReadOptions::default()).unwrap();
    assert_eq!(sent.dataset.string(tags::PATIENT_IDENTITY_REMOVED).unwrap(), "YES");
    assert_ne!(sent.dataset.string(tags::STUDY_INSTANCE_UID).unwrap(), inst.study_uid);
    assert!(!String::from_utf8_lossy(&seen_by_model.lock().unwrap()[0]).contains("PAT-AI"));
    // the FHIR server gets the finding, pointing at the original study
    let reports = reports.lock().unwrap();
    assert_eq!(reports.len(), 1);
    let r = cameleer_core::json::parse(&reports[0]).unwrap();
    assert_eq!(r.get("resourceType"), Some(&Value::from("DiagnosticReport")));
    assert_eq!(r.get("conclusion"), Some(&Value::from("No acute intracranial abnormality")));
    assert!(reports[0].contains(&inst.study_uid) && reports[0].contains("PAT-AI"), "{}", reports[0]);
    main.stop().unwrap();
    mocks.stop().unwrap();
}

#[test]
fn deid_pipeline_example() {
    let input = temp_dir("in");
    let output = temp_dir("out");
    let audit = format!("{}/audit.jsonl", temp_dir("audit"));
    let mut main = cameleer_main::new_main();
    main.load_properties_text(&format!("camel.ai.audit.file={}\n", audit), false).unwrap();
    main.add_route_source(
        "deid-pipeline.camel.yaml",
        example("deid-pipeline.camel.yaml", &[("{{env:IN_DIR:in}}", &input), ("{{env:OUT_DIR:out}}", &output), ("{{env:DEID_KEY:change-me}}", "example-key")]),
    );
    main.start().unwrap();
    let insts = [Instance::ct("PAT-D", 62, 1, 1), Instance::ct("PAT-D", 62, 1, 2)];
    for (i, inst) in insts.iter().enumerate() {
        std::fs::write(format!("{}/{}.dcm", input, i), inst.part10()).unwrap();
    }
    let count = || std::fs::read_dir(&output).map(|r| r.filter_map(|e| e.ok()).filter(|e| e.path().extension().is_some_and(|x| x == "dcm")).count()).unwrap_or(0);
    wait_for("two de-identified files", || count() == 2);
    let files: Vec<DicomFile> = std::fs::read_dir(&output)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "dcm"))
        .map(|e| DicomFile::parse(&std::fs::read(e.path()).unwrap(), ReadOptions::default()).unwrap())
        .collect();
    let pid = files[0].dataset.string(tags::PATIENT_ID).unwrap();
    assert!(pid.starts_with("ANON-"));
    assert_eq!(files[1].dataset.string(tags::PATIENT_ID).unwrap(), pid, "one pseudonym per patient");
    assert_eq!(files[0].dataset.string(tags::STUDY_INSTANCE_UID), files[1].dataset.string(tags::STUDY_INSTANCE_UID), "the study stays together");
    assert_eq!(files[0].dataset.string(0x0010_0040).unwrap(), "O", "patient characteristics are retained");
    // the audit file has one chained record per instance
    main.stop().unwrap();
    let (n, _) = cameleer_core::audit::verify_file(std::path::Path::new(&audit)).unwrap();
    assert_eq!(n, 2);
}

/// The YAML block after `<!-- example: id -->` in a docs page.
fn doc_example(page: &str, id: &str) -> String {
    let path = format!("{}/../../docs/{}", env!("CARGO_MANIFEST_DIR"), page);
    let text = std::fs::read_to_string(&path).unwrap();
    let marker = format!("<!-- example: {} -->", id);
    let at = text.find(&marker).unwrap_or_else(|| panic!("{} has no example {}", page, id));
    let rest = &text[at..];
    let start = rest.find("```yaml\n").unwrap() + 8;
    let end = start + rest[start..].find("```").unwrap();
    rest[start..end].to_string()
}

/// Docs examples that change data run for real: the tag coercion with a Groovy script on the
/// Value tree, and routing with the dicom language.
#[test]
fn docs_examples_coerce_and_route() {
    let pacs = cameleer_dicom::testing::TestPacs::start("PACS");
    let stroke = cameleer_dicom::testing::TestPacs::start("STROKE_AI");
    let p1 = free_port();
    let p2 = free_port();
    let mut main = cameleer_main::new_main();
    let coerce = doc_example("dataformats/dicom.md", "dicom-df-coerce")
        .replace("{{env:DICOM_PORT:11112}}", &p1.to_string())
        .replace("{{env:PACS_HOST:pacs.local}}:104", &format!("127.0.0.1:{}", pacs.port()));
    let route = doc_example("languages/dicom.md", "dicom-language-route")
        .replace("{{env:DICOM_PORT:11112}}", &p2.to_string())
        .replace("{{env:AI_HOST:ai.local}}:104", &format!("127.0.0.1:{}", stroke.port()))
        .replace("{{env:PACS_HOST:pacs.local}}:104", &format!("127.0.0.1:{}", pacs.port()));
    main.add_route_source("coerce.camel.yaml", coerce);
    main.add_route_source("route.camel.yaml", route);
    main.start().unwrap();
    // coercion: the institution name is replaced, the rest is unchanged
    let inst = Instance::ct("P7", 70, 1, 1);
    let c = ScuConfig::new("CT01", "GATEWAY", "127.0.0.1", p1);
    assert_eq!(scu::store(&c, &[inst.part10()], &[]).unwrap()[0].1.status, 0);
    let got = DicomFile::parse(&pacs.archive.all()[0], ReadOptions::default()).unwrap();
    assert_eq!(got.dataset.string(0x0008_0080).unwrap(), "Example Imaging Centre");
    let mut expected = inst.dataset();
    expected.insert(cameleer_dicom::Element::string(0x0008_0080, cameleer_dicom::vr::Vr::LO, "Example Imaging Centre"));
    assert_eq!(got.dataset, expected);
    // routing: a head CT goes to the stroke AI, the rest to the PACS
    let mut head = Instance::ct("P8", 71, 1, 1).dataset();
    head.insert(cameleer_dicom::Element::string(0x0018_0015, cameleer_dicom::vr::Vr::CS, "HEAD"));
    let head = DicomFile::new(head, cameleer_dicom::ts::EXPLICIT_VR_LITTLE_ENDIAN).to_bytes().unwrap();
    let c = ScuConfig::new("CT01", "ROUTER", "127.0.0.1", p2);
    scu::store(&c, &[head, Instance::ct("P8", 71, 2, 1).part10()], &[]).unwrap();
    assert_eq!(stroke.archive.len(), 1);
    assert_eq!(pacs.archive.len(), 2);
    main.stop().unwrap();
}

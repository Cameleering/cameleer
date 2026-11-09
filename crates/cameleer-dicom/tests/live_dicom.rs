//! Live DIMSE interop with DCMTK 3.6.7 and Orthanc 1.12.2, in both roles, with and without TLS.
//!
//! Needs the peers of `scripts/dicom-interop.sh` (`all`, then `eval "$(scripts/dicom-interop.sh env)"`)
//! and `CAMEL_DICOM_INTEROP=1`; returns early otherwise. Ports are listed in the script.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use cameleer_core::prelude::*;
use cameleer_dicom::dimse::sop;
use cameleer_dicom::object::{DataSet, Element};
use cameleer_dicom::scu::{self, ScuConfig};
use cameleer_dicom::tag::tags;
use cameleer_dicom::testing::{Instance, MemoryArchive};
use cameleer_dicom::ts;
use cameleer_dicom::vr::Vr;
use cameleer_dicom::{DicomFile, ReadOptions};
use cameleer_tls::camel_support::EndpointTls;
use cameleer_tls::params::SslContextParameters;

/// Tests that run a Cameleer SCP on the fixed ports 11140/11141 take this lock.
static SCP_PORT: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn enabled() -> bool {
    let on = std::env::var("CAMEL_DICOM_INTEROP").map(|v| v == "1").unwrap_or(false);
    if !on {
        eprintln!("skipped: set CAMEL_DICOM_INTEROP=1 after `scripts/dicom-interop.sh all`");
    }
    on
}

fn dir() -> PathBuf {
    PathBuf::from(std::env::var("CAMEL_DICOM_INTEROP_DIR").unwrap_or_else(|_| format!("{}/camel-dicom-interop", std::env::var("HOME").unwrap_or_default())))
}

fn tool(name: &str) -> Command {
    let bin = std::env::var("CAMEL_DICOM_DCMTK_BIN").unwrap_or_else(|_| "/usr/bin".into());
    Command::new(Path::new(&bin).join(name))
}

fn run(mut c: Command) -> String {
    let out = c.output().unwrap_or_else(|e| panic!("cannot run {:?}: {e}", c));
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "{:?} failed:\n{}", c, text);
    text
}

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("camel-dicom-live-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn unique_study() -> u32 {
    // seconds since the epoch modulo 10^6, then a per-process counter: unique across parallel
    // tests and across runs against the same Orthanc database
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() % 1_000_000;
    secs as u32 * 1000 + N.fetch_add(1, std::sync::atomic::Ordering::SeqCst) % 1000
}

fn scu_to(ae: &str, port: u16) -> ScuConfig {
    let mut c = ScuConfig::new("CAMELEER", ae, "127.0.0.1", port);
    c.timeout = Duration::from_secs(20);
    c
}

fn cameleer_tls_server() -> SslContextParameters {
    let certs = dir().join("certs");
    SslContextParameters::new().key_pair_files(certs.join("cameleer.pem"), certs.join("cameleer.key")).trust_file(certs.join("ca.pem"))
}

fn cameleer_tls_client() -> SslContextParameters {
    let certs = dir().join("certs");
    SslContextParameters::new().trust_file(certs.join("ca.pem")).key_pair_files(certs.join("cameleer.pem"), certs.join("cameleer.key"))
}

fn files_in(d: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(d).map(|r| r.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.is_file()).collect()).unwrap_or_default();
    v.sort();
    v
}

/// Cameleer SCU to DCMTK storescp: echo, then C-STORE in three transfer syntaxes; the files
/// DCMTK wrote parse back to the same data sets.
#[test]
fn live_cameleer_scu_to_dcmtk_storescp() {
    if !enabled() {
        return;
    }
    let study = unique_study();
    let c = scu_to("STORESCP", 11130);
    assert_eq!(scu::echo(&c).unwrap().status, 0);
    let insts: Vec<Instance> = (1..=3).map(|i| Instance::ct("LIVE1", study, 1, i)).collect();
    let files = vec![insts[0].part10(), insts[1].part10_in(ts::IMPLICIT_VR_LITTLE_ENDIAN), insts[2].part10_in(ts::DEFLATED_EXPLICIT_VR_LITTLE_ENDIAN)];
    let out = scu::store(&c, &files, &[]).unwrap();
    assert!(out.iter().all(|(_, o)| o.status == 0), "{out:?}");
    let written = files_in(&dir().join("storescp"));
    for inst in &insts {
        let p = written.iter().find(|p| p.file_name().unwrap().to_string_lossy().contains(&inst.instance_uid)).unwrap_or_else(|| panic!("storescp did not write {}", inst.instance_uid));
        let f = DicomFile::parse(&std::fs::read(p).unwrap(), ReadOptions::default()).unwrap();
        assert_eq!(f.dataset, inst.dataset(), "{}", inst.instance_uid);
        assert_eq!(f.meta.string(tags::SOURCE_APPLICATION_ENTITY_TITLE).unwrap(), "CAMELEER");
    }
    // and DCMTK's dcmdump reads what we wrote
    let d = temp("dump");
    let path = d.join("x.dcm");
    std::fs::write(&path, &files[2]).unwrap();
    let mut dump = tool("dcmdump");
    dump.arg(&path);
    let text = run(dump);
    assert!(text.contains("LIVE1") && text.contains("DeflatedLittleEndianExplicit"), "{text}");
}

/// Cameleer SCU over TLS to DCMTK storescp +tls (BCP 195 non-downgrading profile).
#[test]
fn live_cameleer_scu_tls_to_dcmtk() {
    if !enabled() {
        return;
    }
    let mut c = scu_to("STORESCP", 11131);
    c.host = "localhost".into();
    c.tls = Some(EndpointTls::new(Arc::new(cameleer_tls_client())));
    assert_eq!(scu::echo(&c).unwrap().status, 0);
    let inst = Instance::ct("LIVETLS", unique_study(), 1, 1);
    assert_eq!(scu::store(&c, &[inst.part10()], &[]).unwrap()[0].1.status, 0);
    assert!(files_in(&dir().join("storescp")).iter().any(|p| p.to_string_lossy().contains(&inst.instance_uid)));
}

fn archive_route(ctx: &CamelContext, uri: &str, archive: Arc<MemoryArchive>) {
    let (a1, a2, a3) = (archive.clone(), archive.clone(), archive);
    let uri = uri.to_string();
    ctx.add_routes(move |b| {
        b.from(uri.as_str())
            .choice()
            .when(header("CamelDicomCommand").is_equal_to("C-STORE"))
            .process(move |ex: &mut Exchange| {
                if let Value::Bytes(b) = ex.body() {
                    a1.add(b.clone());
                }
                Ok(())
            })
            .when(header("CamelDicomCommand").is_equal_to("C-FIND"))
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
}

/// DCMTK SCU tools against a Cameleer SCP route: echoscu, storescu, findscu, getscu and movescu
/// (moving to DCMTK's storescp).
#[test]
fn live_dcmtk_tools_to_cameleer_scp() {
    if !enabled() {
        return;
    }
    let _port = SCP_PORT.lock().unwrap_or_else(|e| e.into_inner());
    let archive = MemoryArchive::new();
    let ctx = CamelContext::new();
    cameleer_dicom::register(&ctx);
    archive_route(&ctx, "dicom:CAMELEER@127.0.0.1:11140?moveDestinations=STORESCP=127.0.0.1:11130", archive.clone());
    ctx.start().unwrap();
    let mut echo = tool("echoscu");
    echo.args(["-aet", "DCMTK", "-aec", "CAMELEER", "127.0.0.1", "11140"]);
    run(echo);
    let study = unique_study();
    let d = temp("dcmtk-scu");
    let mut paths = Vec::new();
    for i in 1..=2 {
        let p = d.join(format!("in{i}.dcm"));
        std::fs::write(&p, Instance::ct("LIVE2", study, 1, i).part10()).unwrap();
        paths.push(p);
    }
    let mut store = tool("storescu");
    store.args(["-aet", "DCMTK", "-aec", "CAMELEER", "127.0.0.1", "11140"]).args(&paths);
    run(store);
    assert_eq!(archive.len(), 2);
    let got = DicomFile::parse(&archive.all()[0], ReadOptions::default()).unwrap();
    assert_eq!(got.dataset, Instance::ct("LIVE2", study, 1, 1).dataset());
    // findscu with -X writes each response to rsp0001.dcm...
    let out = temp("find");
    let mut find = tool("findscu");
    find.args(["-S", "-aet", "DCMTK", "-aec", "CAMELEER", "-k", "QueryRetrieveLevel=IMAGE", "-k", &format!("StudyInstanceUID=2.25.144682546011839860307890821118846661177.{study}"), "-k", "SOPInstanceUID", "-k", "PatientName", "-X", "-od"]).arg(&out).args(["127.0.0.1", "11140"]);
    run(find);
    let rsps = files_in(&out);
    assert_eq!(rsps.len(), 2, "{rsps:?}");
    let r = DicomFile::parse(&std::fs::read(&rsps[0]).unwrap(), ReadOptions::default()).unwrap();
    assert_eq!(r.dataset.string(tags::PATIENT_NAME).unwrap(), "Test^LIVE2");
    // getscu stores the retrieved instances in its output directory
    let gout = temp("get");
    let mut get = tool("getscu");
    get.args(["-S", "-aet", "DCMTK", "-aec", "CAMELEER", "-k", "QueryRetrieveLevel=STUDY", "-k", &format!("StudyInstanceUID=2.25.144682546011839860307890821118846661177.{study}"), "-od"]).arg(&gout).args(["127.0.0.1", "11140"]);
    run(get);
    assert_eq!(files_in(&gout).len(), 2);
    // movescu: the Cameleer SCP sends the study to DCMTK's storescp
    let mut mv = tool("movescu");
    mv.args(["-S", "-aet", "DCMTK", "-aec", "CAMELEER", "-aem", "STORESCP", "-k", "QueryRetrieveLevel=STUDY", "-k", &format!("StudyInstanceUID=2.25.144682546011839860307890821118846661177.{study}"), "127.0.0.1", "11140"]);
    run(mv);
    let written = files_in(&dir().join("storescp"));
    for i in 1..=2 {
        let uid = Instance::ct("LIVE2", study, 1, i).instance_uid;
        assert!(written.iter().any(|p| p.to_string_lossy().contains(&uid)), "{uid} not moved");
    }
    ctx.stop().unwrap();
}

/// storescu +tls against a Cameleer SCP with TLS.
#[test]
fn live_dcmtk_tls_to_cameleer_scp() {
    if !enabled() {
        return;
    }
    let _port = SCP_PORT.lock().unwrap_or_else(|e| e.into_inner());
    let archive = MemoryArchive::new();
    let ctx = CamelContext::new();
    cameleer_dicom::register(&ctx);
    ctx.registry().bind("dicomTls", cameleer_tls_server());
    archive_route(&ctx, "dicom:CAMELEER@127.0.0.1:11141?sslContextParameters=#dicomTls", archive.clone());
    ctx.start().unwrap();
    let d = temp("tls-scu");
    let p = d.join("in.dcm");
    let inst = Instance::ct("LIVE3", unique_study(), 1, 1);
    std::fs::write(&p, inst.part10()).unwrap();
    let certs = dir().join("certs");
    let mut store = tool("storescu");
    store
        .args(["-aet", "DCMTK", "-aec", "CAMELEER", "+tls"])
        .arg(certs.join("dcmtk.key"))
        .arg(certs.join("dcmtk.pem"))
        .arg("+cf")
        .arg(certs.join("ca.pem"))
        .args(["+py", "localhost", "11141"])
        .arg(&p);
    run(store);
    assert_eq!(archive.len(), 1);
    ctx.stop().unwrap();
}

fn query(level: &str, keys: &[(u32, Vr, &str)]) -> DataSet {
    let mut q = DataSet::new();
    q.insert(Element::string(tags::QUERY_RETRIEVE_LEVEL, Vr::CS, level));
    for (t, vr, v) in keys {
        q.insert(Element::string(*t, *vr, v));
    }
    q
}

/// Orthanc as a PACS: C-ECHO, C-STORE, C-FIND, C-GET and C-MOVE (to a Cameleer SCP on 11140).
#[test]
fn live_orthanc_as_pacs() {
    if !enabled() {
        return;
    }
    let _port = SCP_PORT.lock().unwrap_or_else(|e| e.into_inner());
    let c = scu_to("ORTHANC", 14242);
    assert_eq!(scu::echo(&c).unwrap().status, 0);
    let study = unique_study();
    let insts: Vec<Instance> = (1..=3).map(|i| Instance::ct("LIVE4", study, 1, i)).collect();
    let out = scu::store(&c, &insts.iter().map(|i| i.part10()).collect::<Vec<_>>(), &[]).unwrap();
    assert!(out.iter().all(|(_, o)| o.status == 0), "{out:?}");
    let study_uid = format!("2.25.144682546011839860307890821118846661177.{study}");
    let (found, o) = scu::find(&c, sop::STUDY_ROOT_FIND, &query("STUDY", &[(tags::STUDY_INSTANCE_UID, Vr::UI, &study_uid), (tags::PATIENT_ID, Vr::LO, ""), (tags::PATIENT_NAME, Vr::PN, "")])).unwrap();
    assert_eq!((found.len(), o.status), (1, 0));
    assert_eq!(found[0].string(tags::PATIENT_ID).unwrap(), "LIVE4");
    let (images, _) = scu::find(&c, sop::STUDY_ROOT_FIND, &query("IMAGE", &[(tags::STUDY_INSTANCE_UID, Vr::UI, &study_uid), (tags::SERIES_INSTANCE_UID, Vr::UI, &insts[0].series_uid), (tags::SOP_INSTANCE_UID, Vr::UI, "")])).unwrap();
    assert_eq!(images.len(), 3);
    // C-GET
    let (files, ops) = scu::get(&c, sop::STUDY_ROOT_GET, &query("STUDY", &[(tags::STUDY_INSTANCE_UID, Vr::UI, &study_uid)]), &[cameleer_dicom::testing::CT_IMAGE_STORAGE.to_string()], &[]).unwrap();
    assert_eq!((files.len(), ops.completed, ops.status), (3, 3, 0), "{ops:?}");
    let back: Vec<DataSet> = files.iter().map(|f| DicomFile::parse(f, ReadOptions::default()).unwrap().dataset).collect();
    assert!(back.contains(&insts[1].dataset()), "Orthanc returned a different data set");
    // C-MOVE to a Cameleer SCP that Orthanc knows as CAMELEER
    let archive = MemoryArchive::new();
    let ctx = CamelContext::new();
    cameleer_dicom::register(&ctx);
    archive_route(&ctx, "dicom:CAMELEER@127.0.0.1:11140", archive.clone());
    ctx.start().unwrap();
    let ops = scu::move_(&c, sop::STUDY_ROOT_MOVE, &query("STUDY", &[(tags::STUDY_INSTANCE_UID, Vr::UI, &study_uid)]), "CAMELEER").unwrap();
    assert_eq!((ops.status, ops.completed), (0, 3), "{ops:?}");
    assert_eq!(archive.len(), 3);
    ctx.stop().unwrap();
}

/// Orthanc's DICOMweb plugin: STOW-RS, QIDO-RS, WADO-RS and metadata through the dicomweb:
/// producer.
#[test]
fn live_orthanc_dicomweb() {
    if !enabled() {
        return;
    }
    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    cameleer_dicom::register(&ctx);
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let base = "dicomweb:http://127.0.0.1:18042/dicom-web";
    let study = unique_study();
    let insts: Vec<Instance> = (1..=2).map(|i| Instance::ct("LIVEWEB", study, 1, i)).collect();
    let mut ex = ctx.create_exchange();
    ex.set_body(Value::Array(insts.iter().map(|i| Value::Bytes(i.part10())).collect()));
    let ex = t.send(base, ex);
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
    assert_eq!(ex.header("CamelDicomCompleted"), Some(&Value::Int(2)));
    let mut q = cameleer_core::value::ObjectMap::new();
    q.insert("StudyInstanceUID".into(), Value::from(insts[0].study_uid.clone()));
    q.insert("PatientID".into(), Value::from(""));
    let found = t.request_body(&format!("{}?operation=qido", base), Value::Object(q)).unwrap();
    let found = found.as_array().unwrap();
    assert_eq!(found.len(), 1);
    let pid = found[0].get("00100020").and_then(|e| e.get("Value")).and_then(|v| v.as_array()).unwrap()[0].clone();
    assert_eq!(pid, Value::from("LIVEWEB"));
    let mut ex = ctx.create_exchange();
    ex.set_header("CamelDicomStudyInstanceUid", insts[0].study_uid.clone());
    ex.set_header("CamelDicomSeriesInstanceUid", insts[0].series_uid.clone());
    ex.set_header("CamelDicomSopInstanceUid", insts[0].instance_uid.clone());
    let ex = t.send(&format!("{}?operation=wado", base), ex);
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
    let files = ex.body().as_array().unwrap().clone();
    assert_eq!(files.len(), 1);
    let Value::Bytes(b) = &files[0] else { panic!() };
    let f = DicomFile::parse(b, ReadOptions::default()).unwrap();
    assert_eq!(f.dataset, insts[0].dataset());
    let mut ex = ctx.create_exchange();
    ex.set_header("CamelDicomStudyInstanceUid", insts[0].study_uid.clone());
    let ex = t.send(&format!("{}?operation=metadata", base), ex);
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
    assert_eq!(ex.body().as_array().unwrap().len(), 2);
    ctx.stop().unwrap();
}

/// Orthanc's DICOMweb client against a Cameleer DICOMweb server (port 18090): Orthanc sends a
/// study with STOW-RS, searches it with QIDO-RS and retrieves it with WADO-RS.
#[test]
fn live_orthanc_dicomweb_client_to_cameleer_server() {
    if !enabled() {
        return;
    }
    let orthanc = "http://127.0.0.1:18042";
    let http = cameleer_http::client::HttpClient::new();
    // put a study into Orthanc first
    let study = unique_study();
    let insts: Vec<Instance> = (1..=2).map(|i| Instance::ct("LIVEWS", study, 1, i)).collect();
    assert!(scu::store(&scu_to("ORTHANC", 14242), &insts.iter().map(|i| i.part10()).collect::<Vec<_>>(), &[]).unwrap().iter().all(|(_, o)| o.status == 0));
    let r = http.post(&format!("{}/tools/lookup", orthanc), "text/plain", insts[0].study_uid.as_bytes()).unwrap();
    let ids = cameleer_core::json::parse(&String::from_utf8_lossy(&r.body)).unwrap();
    let orthanc_id = ids.as_array().unwrap()[0].get("ID").unwrap().to_string();
    // the Cameleer DICOMweb server
    let archive = MemoryArchive::new();
    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    cameleer_dicom::register(&ctx);
    let (a1, a2, a3) = (archive.clone(), archive.clone(), archive.clone());
    ctx.add_routes(move |b| {
        b.from("dicomweb:/dicom-web?port=18090")
            .choice()
            .when(header("CamelDicomWebOperation").is_equal_to("stow"))
            .process(move |ex: &mut Exchange| {
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
    // STOW-RS from Orthanc
    let body = format!("{{\"Resources\": [\"{}\"], \"Synchronous\": true}}", orthanc_id);
    let r = http.post(&format!("{}/dicom-web/servers/cameleer/stow", orthanc), "application/json", body.as_bytes()).unwrap();
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    assert_eq!(archive.len(), 2);
    let got = DicomFile::parse(&archive.all()[0], ReadOptions::default()).unwrap();
    assert!(got.dataset == insts[0].dataset() || got.dataset == insts[1].dataset());
    // QIDO-RS from Orthanc
    let body = format!("{{\"Uri\": \"/studies\", \"Arguments\": {{\"StudyInstanceUID\": \"{}\"}}}}", insts[0].study_uid);
    let r = http.post(&format!("{}/dicom-web/servers/cameleer/qido", orthanc), "application/json", body.as_bytes()).unwrap();
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    let text = String::from_utf8_lossy(&r.body).to_string();
    assert!(text.contains(&insts[0].study_uid), "{text}");
    // WADO-RS from Orthanc: delete the study in Orthanc, then retrieve it back from Cameleer
    let r = http.request("DELETE", &format!("{}/studies/{}", orthanc, orthanc_id), &[], None, None).unwrap();
    assert_eq!(r.status, 200);
    let body = format!("{{\"Resources\": [{{\"Study\": \"{}\"}}]}}", insts[0].study_uid);
    let r = http.post(&format!("{}/dicom-web/servers/cameleer/retrieve", orthanc), "application/json", body.as_bytes()).unwrap();
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    let r = http.post(&format!("{}/tools/lookup", orthanc), "text/plain", insts[0].study_uid.as_bytes()).unwrap();
    assert!(String::from_utf8_lossy(&r.body).contains("Study"), "Orthanc did not get the study back");
    ctx.stop().unwrap();
}

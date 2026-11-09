//! Test helpers: synthetic DICOM instances (no real patient data), used by the hermetic tests,
//! the examples and the embedded test servers.

use crate::codec::DicomFile;
use crate::object::{DataSet, Element};
use crate::tag::tags;
use crate::ts;
use crate::vr::Vr;

/// CT Image Storage.
pub const CT_IMAGE_STORAGE: &str = "1.2.840.10008.5.1.4.1.1.2";
/// The root of the synthetic UIDs: a UUID-derived UID (ISO/IEC 9834-8), fixed for these tests.
pub const TEST_UID_ROOT: &str = "2.25.144682546011839860307890821118846661177";
/// Secondary Capture Image Storage.
pub const SECONDARY_CAPTURE_STORAGE: &str = "1.2.840.10008.5.1.4.1.1.7";

/// What a synthetic instance contains.
#[derive(Clone, Debug)]
pub struct Instance {
    pub patient_id: String,
    pub patient_name: String,
    pub study_uid: String,
    pub series_uid: String,
    pub instance_uid: String,
    pub sop_class: String,
    pub modality: String,
    pub study_date: String,
    pub accession: String,
    pub instance_number: u32,
}

impl Instance {
    /// A CT instance with the given numbers in its UIDs, under [`TEST_UID_ROOT`]
    /// (`<root>.<study>.<series>.<instance>`).
    pub fn ct(patient: &str, study: u32, series: u32, instance: u32) -> Instance {
        Instance {
            patient_id: patient.to_string(),
            patient_name: format!("Test^{}", patient),
            study_uid: format!("2.25.144682546011839860307890821118846661177.{}", study),
            series_uid: format!("2.25.144682546011839860307890821118846661177.{}.{}", study, series),
            instance_uid: format!("2.25.144682546011839860307890821118846661177.{}.{}.{}", study, series, instance),
            sop_class: CT_IMAGE_STORAGE.to_string(),
            modality: "CT".to_string(),
            study_date: "20260101".to_string(),
            accession: format!("ACC{}", study),
            instance_number: instance,
        }
    }

    /// The data set: patient, study, series and image modules with a 4x4 16-bit image.
    pub fn dataset(&self) -> DataSet {
        let s = |t, vr, v: &str| Element::string(t, vr, v);
        let mut ds = DataSet::new();
        ds.insert(s(tags::SPECIFIC_CHARACTER_SET, Vr::CS, "ISO_IR 100"));
        ds.insert(s(tags::IMAGE_TYPE, Vr::CS, "ORIGINAL\\PRIMARY\\AXIAL"));
        ds.insert(s(tags::SOP_CLASS_UID, Vr::UI, &self.sop_class));
        ds.insert(s(tags::SOP_INSTANCE_UID, Vr::UI, &self.instance_uid));
        ds.insert(s(tags::STUDY_DATE, Vr::DA, &self.study_date));
        ds.insert(s(0x0008_0021, Vr::DA, &self.study_date));
        ds.insert(s(0x0008_0030, Vr::TM, "101500"));
        ds.insert(s(0x0008_0050, Vr::SH, &self.accession));
        ds.insert(s(tags::MODALITY, Vr::CS, &self.modality));
        ds.insert(s(0x0008_0070, Vr::LO, "ACME Imaging"));
        ds.insert(s(0x0008_0080, Vr::LO, "General Hospital"));
        ds.insert(s(0x0008_0090, Vr::PN, "Referrer^Rita"));
        ds.insert(s(0x0008_1030, Vr::LO, "CT HEAD"));
        ds.insert(s(0x0008_103E, Vr::LO, "AXIAL 5MM"));
        ds.insert(s(0x0008_1090, Vr::LO, "ScannerModel"));
        ds.insert(s(tags::PATIENT_NAME, Vr::PN, &self.patient_name));
        ds.insert(s(tags::PATIENT_ID, Vr::LO, &self.patient_id));
        ds.insert(s(0x0010_0030, Vr::DA, "19700101"));
        ds.insert(s(0x0010_0040, Vr::CS, "O"));
        ds.insert(s(0x0010_1010, Vr::AS, "056Y"));
        ds.insert(s(0x0018_0050, Vr::DS, "5"));
        ds.insert(s(0x0018_1000, Vr::LO, "SN-0001"));
        ds.insert(s(tags::STUDY_INSTANCE_UID, Vr::UI, &self.study_uid));
        ds.insert(s(tags::SERIES_INSTANCE_UID, Vr::UI, &self.series_uid));
        ds.insert(s(0x0020_0010, Vr::SH, "S1"));
        ds.insert(s(0x0020_0011, Vr::IS, "1"));
        ds.insert(s(0x0020_0013, Vr::IS, &self.instance_number.to_string()));
        ds.insert(s(0x0020_0052, Vr::UI, &format!("{}.9", self.study_uid)));
        ds.insert(Element::us(0x0028_0002, 1));
        ds.insert(s(0x0028_0004, Vr::CS, "MONOCHROME2"));
        ds.insert(Element::us(0x0028_0010, 4));
        ds.insert(Element::us(0x0028_0011, 4));
        ds.insert(Element::us(0x0028_0100, 16));
        ds.insert(Element::us(0x0028_0101, 12));
        ds.insert(Element::us(0x0028_0102, 11));
        ds.insert(Element::us(0x0028_0103, 0));
        let px: Vec<u8> = (0u16..16).flat_map(|i| (i * 100).to_le_bytes()).collect();
        ds.insert(Element::bytes(tags::PIXEL_DATA, Vr::OW, px));
        ds
    }

    /// As a Part 10 file in Explicit VR Little Endian.
    pub fn part10(&self) -> Vec<u8> {
        self.part10_in(ts::EXPLICIT_VR_LITTLE_ENDIAN)
    }

    /// As a Part 10 file in a transfer syntax.
    pub fn part10_in(&self, transfer_syntax: &str) -> Vec<u8> {
        DicomFile::new(self.dataset(), transfer_syntax).to_bytes().expect("synthetic instance encodes")
    }
}

// ---------------------------------------------------------------------------------------
// An in-memory archive and a test PACS

use std::sync::{Arc, Mutex};

use crate::codec::ReadOptions;
use crate::scp::{DicomServer, Request, ScpConfig, ScpHandler, Status};

/// Instances kept in memory, with Query/Retrieve matching (PS3.4 C.2.2.2: universal, single
/// value, wildcard, UID list and date/time range matching).
#[derive(Default)]
pub struct MemoryArchive {
    instances: Mutex<Vec<(DataSet, Vec<u8>)>>,
}

impl MemoryArchive {
    /// An empty archive.
    pub fn new() -> Arc<MemoryArchive> {
        Arc::new(MemoryArchive::default())
    }

    /// Add (or replace, by SOP Instance UID) an instance.
    pub fn add(&self, part10: Vec<u8>) -> crate::object::DataSet {
        let f = DicomFile::parse(&part10, ReadOptions { stop_before_pixel_data: true }).expect("valid DICOM");
        let mut g = self.instances.lock().unwrap();
        let uid = f.dataset.sop_instance_uid();
        g.retain(|(d, _)| d.sop_instance_uid() != uid);
        g.push((f.dataset.clone(), part10));
        f.dataset
    }

    /// Number of instances.
    pub fn len(&self) -> usize {
        self.instances.lock().unwrap().len()
    }

    /// Whether the archive is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The Part 10 bytes of every instance.
    pub fn all(&self) -> Vec<Vec<u8>> {
        self.instances.lock().unwrap().iter().map(|(_, b)| b.clone()).collect()
    }

    /// Instances whose attributes match every non-empty key of the identifier.
    pub fn matching(&self, ident: &DataSet) -> Vec<(DataSet, Vec<u8>)> {
        let g = self.instances.lock().unwrap();
        g.iter().filter(|(d, _)| matches(d, ident)).cloned().collect()
    }

    /// C-FIND answers: one identifier per unique entity at the Query/Retrieve level.
    pub fn find(&self, ident: &DataSet) -> Vec<DataSet> {
        let level = ident.string(tags::QUERY_RETRIEVE_LEVEL).unwrap_or_else(|| "STUDY".into()).to_ascii_uppercase();
        let key = match level.as_str() {
            "PATIENT" => tags::PATIENT_ID,
            "SERIES" => tags::SERIES_INSTANCE_UID,
            "IMAGE" => tags::SOP_INSTANCE_UID,
            _ => tags::STUDY_INSTANCE_UID,
        };
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for (d, _) in self.matching(ident) {
            if !seen.insert(d.string(key).unwrap_or_default()) {
                continue;
            }
            let mut r = DataSet::new();
            for e in ident.iter() {
                match d.get(e.tag) {
                    Some(v) => r.insert(v.clone()),
                    None => r.insert(e.clone()),
                };
            }
            r.insert(Element::string(tags::QUERY_RETRIEVE_LEVEL, Vr::CS, &level));
            if let Some(cs) = d.get(tags::SPECIFIC_CHARACTER_SET) {
                r.insert(cs.clone());
            }
            out.push(r);
        }
        out
    }
}

fn matches(d: &DataSet, ident: &DataSet) -> bool {
    let cs = d.charset();
    for k in ident.iter() {
        if k.tag == tags::QUERY_RETRIEVE_LEVEL || k.tag == tags::SPECIFIC_CHARACTER_SET || k.vr == Vr::SQ {
            continue;
        }
        let want = k.to_string_value(&ident.charset());
        if want.is_empty() {
            continue;
        }
        let have = d.get(k.tag).map(|e| e.to_string_value(&cs)).unwrap_or_default();
        let ok = if k.vr == Vr::UI {
            want.split('\\').any(|w| w == have)
        } else if want.contains(['*', '?']) {
            wildcard(&want, &have)
        } else if matches!(k.vr, Vr::DA | Vr::TM | Vr::DT) && want.contains('-') {
            let (lo, hi) = want.split_once('-').unwrap();
            (lo.is_empty() || have.as_str() >= lo) && (hi.is_empty() || have.as_str() <= hi)
        } else {
            want == have
        };
        if !ok {
            return false;
        }
    }
    true
}

fn wildcard(p: &str, s: &str) -> bool {
    let (p, s): (Vec<char>, Vec<char>) = (p.chars().collect(), s.chars().collect());
    fn go(p: &[char], s: &[char]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some('*') => (0..=s.len()).any(|i| go(&p[1..], &s[i..])),
            Some('?') => !s.is_empty() && go(&p[1..], &s[1..]),
            Some(c) => s.first() == Some(c) && go(&p[1..], &s[1..]),
        }
    }
    go(&p, &s)
}

/// A PACS for tests: stores what it receives, answers C-FIND, C-MOVE and C-GET from its
/// archive.
pub struct TestPacs {
    pub archive: Arc<MemoryArchive>,
    pub server: DicomServer,
    /// The requests received, in order (command, calling AE).
    pub requests: Arc<Mutex<Vec<(String, String)>>>,
}

struct PacsHandler {
    archive: Arc<MemoryArchive>,
    requests: Arc<Mutex<Vec<(String, String)>>>,
}

impl ScpHandler for PacsHandler {
    fn echo(&self, req: &Request) -> Status {
        self.requests.lock().unwrap().push((req.command.to_string(), req.calling_ae.clone()));
        Status::SUCCESS
    }
    fn store(&self, req: &Request, part10: Vec<u8>) -> Status {
        self.requests.lock().unwrap().push((req.command.to_string(), req.calling_ae.clone()));
        self.archive.add(part10);
        Status::SUCCESS
    }
    fn find(&self, req: &Request, identifier: DataSet) -> std::result::Result<Vec<DataSet>, Status> {
        self.requests.lock().unwrap().push((req.command.to_string(), req.calling_ae.clone()));
        Ok(self.archive.find(&identifier))
    }
    fn retrieve(&self, req: &Request, identifier: DataSet) -> std::result::Result<Vec<Vec<u8>>, Status> {
        self.requests.lock().unwrap().push((req.command.to_string(), req.calling_ae.clone()));
        Ok(self.archive.matching(&identifier).into_iter().map(|(_, b)| b).collect())
    }
}

impl TestPacs {
    /// Start on an ephemeral port of 127.0.0.1 with this AE title and configuration changes.
    pub fn start_with(ae: &str, configure: impl FnOnce(&mut ScpConfig)) -> TestPacs {
        let archive = MemoryArchive::new();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let mut cfg = ScpConfig::new(ae, "127.0.0.1", 0);
        cfg.timeout = std::time::Duration::from_secs(10);
        configure(&mut cfg);
        let server = DicomServer::start(cfg, Arc::new(PacsHandler { archive: archive.clone(), requests: requests.clone() })).expect("test PACS starts");
        TestPacs { archive, server, requests }
    }

    /// Start with the defaults.
    pub fn start(ae: &str) -> TestPacs {
        Self::start_with(ae, |_| {})
    }

    /// The port.
    pub fn port(&self) -> u16 {
        self.server.port()
    }
}

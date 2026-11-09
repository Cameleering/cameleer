//! The SCP side: a server accepting associations and answering C-ECHO, C-STORE, C-FIND,
//! C-MOVE and C-GET through a [`ScpHandler`].

use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use cameleer_core::error::{CamelError, Result};
use cameleer_tls::camel_support::EndpointTls;
use cameleer_tls::net::NetStream;

use crate::assoc::{Association, Message, Received, DEFAULT_MAX_PDU, NATIVE_TRANSFER_SYNTAXES};
use crate::codec::{self, ReadOptions};
use crate::dimse::{self, command, status, status_class, StatusClass};
use crate::object::{DataSet, Element};
use crate::scu::{self, ScuConfig, StoreItem, SubOps};
use crate::tag::tags;
use crate::ts::TransferSyntax;
use crate::ul::{pc_result, AssociateAc, AssociateRj, AssociateRq, PresentationContextAc, UserInfo};

/// A status with an optional Error Comment.
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    pub code: u16,
    pub comment: Option<String>,
}

impl Status {
    pub const SUCCESS: Status = Status { code: status::SUCCESS, comment: None };

    pub fn new(code: u16, comment: impl Into<String>) -> Status {
        Status { code, comment: Some(comment.into()) }
    }
}

/// What the handler knows about a request.
#[derive(Clone, Debug, Default)]
pub struct Request {
    /// `C-ECHO`, `C-STORE`, `C-FIND`, `C-MOVE`, `C-GET`.
    pub command: &'static str,
    pub calling_ae: String,
    pub called_ae: String,
    pub remote: String,
    /// Affected SOP Class UID (the information model for queries).
    pub sop_class: String,
    pub sop_instance: String,
    /// Transfer syntax of the data set.
    pub transfer_syntax: String,
    pub message_id: u16,
    pub priority: u16,
    pub move_destination: Option<String>,
    pub move_originator: Option<String>,
    /// Username of a User Identity Negotiation (types 1 and 2).
    pub username: Option<String>,
}

/// The application behind an SCP.
pub trait ScpHandler: Send + Sync {
    /// C-ECHO (default: success).
    fn echo(&self, _req: &Request) -> Status {
        Status::SUCCESS
    }
    /// C-STORE: the instance as Part 10 bytes.
    fn store(&self, req: &Request, part10: Vec<u8>) -> Status;
    /// C-FIND: the matches for an identifier.
    fn find(&self, _req: &Request, _identifier: DataSet) -> std::result::Result<Vec<DataSet>, Status> {
        Err(Status::new(status::SOP_CLASS_NOT_SUPPORTED, "C-FIND is not supported"))
    }
    /// C-MOVE and C-GET: the instances (Part 10 bytes) for an identifier.
    fn retrieve(&self, _req: &Request, _identifier: DataSet) -> std::result::Result<Vec<Vec<u8>>, Status> {
        Err(Status::new(status::SOP_CLASS_NOT_SUPPORTED, "Retrieve is not supported"))
    }
}

/// Which abstract syntaxes an SCP accepts.
#[derive(Clone, Debug, PartialEq)]
pub enum SopFilter {
    /// Every abstract syntax.
    All,
    /// These UIDs, plus every Storage SOP class when `storage` is true.
    Some { uids: Vec<String>, storage: bool },
}

impl SopFilter {
    /// Verification, every Storage SOP class and the Query/Retrieve models.
    pub fn default_scp() -> SopFilter {
        use dimse::sop::*;
        SopFilter::Some {
            uids: [VERIFICATION, PATIENT_ROOT_FIND, PATIENT_ROOT_MOVE, PATIENT_ROOT_GET, STUDY_ROOT_FIND, STUDY_ROOT_MOVE, STUDY_ROOT_GET, MODALITY_WORKLIST_FIND].iter().map(|s| s.to_string()).collect(),
            storage: true,
        }
    }

    /// Parse `all`, or a comma list of UIDs, UID keywords (`CTImageStorage`) and the words
    /// `storage`, `verification`, `query` (the Q/R models and MWL).
    pub fn parse(s: &str) -> Result<SopFilter> {
        use dimse::sop::*;
        let mut uids = Vec::new();
        let mut storage = false;
        for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            match part.to_ascii_lowercase().as_str() {
                "all" | "*" => return Ok(SopFilter::All),
                "storage" => storage = true,
                "verification" => uids.push(VERIFICATION.to_string()),
                "query" | "qr" => uids.extend([PATIENT_ROOT_FIND, PATIENT_ROOT_MOVE, PATIENT_ROOT_GET, STUDY_ROOT_FIND, STUDY_ROOT_MOVE, STUDY_ROOT_GET, MODALITY_WORKLIST_FIND].iter().map(|s| s.to_string())),
                _ => {
                    if part.starts_with(|c: char| c.is_ascii_digit()) {
                        uids.push(part.to_string());
                    } else {
                        let e = crate::dict::uid_by_keyword(part).ok_or_else(|| CamelError::illegal_argument(format!("Unknown SOP class: {}", part)))?;
                        uids.push(e.uid.to_string());
                    }
                }
            }
        }
        Ok(SopFilter::Some { uids, storage })
    }

    pub fn accepts(&self, uid: &str) -> bool {
        match self {
            SopFilter::All => true,
            SopFilter::Some { uids, storage } => uids.iter().any(|u| u == uid) || (*storage && dimse::is_storage_sop_class(uid)),
        }
    }
}

/// SCP configuration.
#[derive(Clone)]
pub struct ScpConfig {
    pub ae_title: String,
    pub host: String,
    pub port: u16,
    pub tls: Option<EndpointTls>,
    pub max_pdu_length: u32,
    pub sop_classes: SopFilter,
    /// Accepted transfer syntaxes in order of preference; `*` accepts any proposed one (after
    /// the listed ones).
    pub transfer_syntaxes: Vec<String>,
    /// Reject associations whose Called AE Title is not ours.
    pub check_called_ae: bool,
    /// Accept only these calling AE titles (empty = any).
    pub calling_aes: Vec<String>,
    /// Reject associations without a User Identity.
    pub require_user_identity: bool,
    /// AE title to `host:port` for C-MOVE destinations.
    pub ae_table: HashMap<String, (String, u16)>,
    /// TLS for associations opened to C-MOVE destinations.
    pub move_tls: Option<EndpointTls>,
    /// Time to wait for the association request and for each message.
    pub timeout: Duration,
    pub max_associations: usize,
}

impl ScpConfig {
    pub fn new(ae_title: &str, host: &str, port: u16) -> ScpConfig {
        ScpConfig {
            ae_title: ae_title.to_string(),
            host: host.to_string(),
            port,
            tls: None,
            max_pdu_length: DEFAULT_MAX_PDU,
            sop_classes: SopFilter::default_scp(),
            transfer_syntaxes: NATIVE_TRANSFER_SYNTAXES.iter().map(|s| s.to_string()).chain(std::iter::once("*".to_string())).collect(),
            check_called_ae: false,
            calling_aes: Vec::new(),
            require_user_identity: false,
            ae_table: HashMap::new(),
            move_tls: None,
            timeout: Duration::from_secs(60),
            max_associations: 50,
        }
    }

    /// Answer an association request.
    pub fn negotiate(&self, rq: &AssociateRq, active: usize) -> std::result::Result<AssociateAc, AssociateRj> {
        if active >= self.max_associations {
            return Err(AssociateRj { result: 2, source: 3, reason: 2 });
        }
        if rq.application_context != crate::ul::APPLICATION_CONTEXT {
            return Err(AssociateRj { result: 1, source: 1, reason: 2 });
        }
        if self.check_called_ae && rq.called_ae != self.ae_title {
            return Err(AssociateRj { result: 1, source: 1, reason: 7 });
        }
        if !self.calling_aes.is_empty() && !self.calling_aes.iter().any(|a| *a == rq.calling_ae) {
            return Err(AssociateRj { result: 1, source: 1, reason: 3 });
        }
        if self.require_user_identity && rq.user.user_identity.is_none() {
            return Err(AssociateRj { result: 1, source: 1, reason: 1 });
        }
        let any = self.transfer_syntaxes.iter().any(|t| t == "*");
        let pcs = rq
            .presentation_contexts
            .iter()
            .map(|pc| {
                if !self.sop_classes.accepts(&pc.abstract_syntax) {
                    return PresentationContextAc { id: pc.id, result: pc_result::ABSTRACT_SYNTAX_NOT_SUPPORTED, transfer_syntax: pc.transfer_syntaxes.first().cloned().unwrap_or_default() };
                }
                let chosen = self.transfer_syntaxes.iter().filter(|t| *t != "*").find(|t| pc.transfer_syntaxes.contains(t)).cloned().or_else(|| if any { pc.transfer_syntaxes.first().cloned() } else { None });
                match chosen {
                    Some(t) => PresentationContextAc { id: pc.id, result: pc_result::ACCEPTANCE, transfer_syntax: t },
                    None => PresentationContextAc { id: pc.id, result: pc_result::TRANSFER_SYNTAXES_NOT_SUPPORTED, transfer_syntax: pc.transfer_syntaxes.first().cloned().unwrap_or_default() },
                }
            })
            .collect();
        let user_identity_response = rq.user.user_identity.as_ref().filter(|u| u.positive_response_requested).map(|_| Vec::new());
        Ok(AssociateAc {
            called_ae: rq.called_ae.clone(),
            calling_ae: rq.calling_ae.clone(),
            application_context: rq.application_context.clone(),
            presentation_contexts: pcs,
            user: UserInfo {
                max_pdu_length: self.max_pdu_length,
                implementation_class_uid: codec::IMPLEMENTATION_CLASS_UID.to_string(),
                implementation_version_name: Some(codec::IMPLEMENTATION_VERSION_NAME.to_string()),
                // role selections are accepted as proposed (C-GET needs the SCP role)
                role_selections: rq.user.role_selections.clone(),
                user_identity_response,
                ..Default::default()
            },
        })
    }
}

/// A running SCP.
pub struct DicomServer {
    local: SocketAddr,
    running: Arc<AtomicBool>,
    accept: Mutex<Option<JoinHandle<()>>>,
    conns: Arc<Mutex<HashMap<u64, TcpStream>>>,
}

impl DicomServer {
    /// Bind and start serving.
    pub fn start(cfg: ScpConfig, handler: Arc<dyn ScpHandler>) -> Result<DicomServer> {
        let addr = format!("{}:{}", if cfg.host.is_empty() { "0.0.0.0" } else { &cfg.host }, cfg.port);
        let listener: TcpListener = cameleer_tls::net::bind_listener(&addr).map_err(|e| CamelError::io(format!("Failed to bind the DICOM SCP on {}: {}", addr, e)))?;
        let local = listener.local_addr().map_err(|e| CamelError::io(e.to_string()))?;
        listener.set_nonblocking(true).map_err(|e| CamelError::io(e.to_string()))?;
        let running = Arc::new(AtomicBool::new(true));
        let conns: Arc<Mutex<HashMap<u64, TcpStream>>> = Arc::new(Mutex::new(HashMap::new()));
        let active = Arc::new(AtomicUsize::new(0));
        let cfg = Arc::new(cfg);
        let (run, c2) = (running.clone(), conns.clone());
        let accept = std::thread::Builder::new()
            .name(format!("dicom-scp-{}", local.port()))
            .spawn(move || {
                let ids = AtomicU64::new(1);
                while run.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((s, _)) => {
                            let _ = s.set_nonblocking(false);
                            let _ = s.set_nodelay(true);
                            let id = ids.fetch_add(1, Ordering::Relaxed);
                            if let Ok(c) = s.try_clone() {
                                c2.lock().unwrap().insert(id, c);
                            }
                            let (cfg, h, c3, act) = (cfg.clone(), handler.clone(), c2.clone(), active.clone());
                            let _ = std::thread::Builder::new().name(format!("dicom-assoc-{}", id)).spawn(move || {
                                let n = act.fetch_add(1, Ordering::SeqCst);
                                serve(&cfg, h.as_ref(), s, n);
                                act.fetch_sub(1, Ordering::SeqCst);
                                c3.lock().unwrap().remove(&id);
                            });
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(20)),
                        Err(_) => std::thread::sleep(Duration::from_millis(50)),
                    }
                }
            })
            .map_err(|e| CamelError::io(e.to_string()))?;
        Ok(DicomServer { local, running, accept: Mutex::new(Some(accept)), conns })
    }

    /// The bound port.
    pub fn port(&self) -> u16 {
        self.local.port()
    }

    /// The bound address.
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    /// Stop accepting and close open associations.
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(j) = self.accept.lock().unwrap().take() {
            let _ = j.join();
        }
        for (_, s) in self.conns.lock().unwrap().drain() {
            let _ = cameleer_tls::net::shutdown_socket(&s, std::net::Shutdown::Both);
        }
    }
}

impl Drop for DicomServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn serve(cfg: &ScpConfig, h: &dyn ScpHandler, tcp: TcpStream, active: usize) {
    let stream = match &cfg.tls {
        Some(t) => match t.accept(tcp, "dicom SCP") {
            Ok(s) => s,
            Err(e) => {
                cameleer_core::camel_debug!("dicom", "DICOM TLS handshake failed: {}", e);
                return;
            }
        },
        None => NetStream::plain(tcp),
    };
    let mut a = match Association::accept(stream, cfg.max_pdu_length, cfg.timeout, |rq| cfg.negotiate(rq, active)) {
        Ok(Some(a)) => a,
        Ok(None) => return,
        Err(e) => {
            cameleer_core::camel_debug!("dicom", "DICOM association failed: {}", e);
            return;
        }
    };
    // an open association may idle between requests
    a.set_timeout(cfg.timeout * 5);
    loop {
        let m = match a.receive() {
            Ok(Received::Message(m)) => m,
            Ok(_) => return,
            Err(e) => {
                cameleer_core::camel_debug!("dicom", "DICOM association with {} ended: {}", a.remote, e);
                return;
            }
        };
        if let Err(e) = dispatch(cfg, h, &mut a, m) {
            cameleer_core::camel_warn!("dicom", "DICOM request from {} failed: {}", a.remote, e);
            a.abort();
            return;
        }
    }
}

fn request(a: &Association, m: &Message, name: &'static str) -> Request {
    let c = &m.command;
    let username = a.rq.user.user_identity.as_ref().filter(|u| u.kind == 1 || u.kind == 2).map(|u| String::from_utf8_lossy(&u.primary).to_string());
    Request {
        command: name,
        calling_ae: a.calling_ae().to_string(),
        called_ae: a.called_ae().to_string(),
        remote: a.remote.clone(),
        sop_class: c.string(tags::AFFECTED_SOP_CLASS_UID).filter(|s| !s.is_empty()).or_else(|| a.abstract_syntax(m.pc_id).map(str::to_string)).unwrap_or_default(),
        sop_instance: c.string(tags::AFFECTED_SOP_INSTANCE_UID).unwrap_or_default(),
        transfer_syntax: a.transfer_syntax(m.pc_id).unwrap_or_default().to_string(),
        message_id: c.int(tags::MESSAGE_ID).unwrap_or(0) as u16,
        priority: c.int(tags::PRIORITY).unwrap_or(0) as u16,
        move_destination: c.string(tags::MOVE_DESTINATION).filter(|s| !s.is_empty()),
        move_originator: c.string(tags::MOVE_ORIGINATOR_AE_TITLE).filter(|s| !s.is_empty()),
        username,
    }
}

fn with_comment(mut c: DataSet, st: &Status) -> DataSet {
    if let Some(cm) = &st.comment {
        if status_class(st.code) != StatusClass::Success {
            let mut cm = cm.clone();
            cm.truncate(64);
            c.insert(Element::string(tags::ERROR_COMMENT, crate::vr::Vr::LO, &cm));
        }
    }
    c
}

fn dispatch(cfg: &ScpConfig, h: &dyn ScpHandler, a: &mut Association, m: Message) -> Result<()> {
    let tsx = TransferSyntax::from_uid(a.transfer_syntax(m.pc_id).unwrap_or(crate::ts::IMPLICIT_VR_LITTLE_ENDIAN));
    match dimse::field(&m.command) {
        command::C_ECHO_RQ => {
            let st = h.echo(&request(a, &m, "C-ECHO"));
            let rsp = with_comment(dimse::response(&m.command, command::C_ECHO_RSP, st.code, false), &st);
            a.send(m.pc_id, &rsp, None)
        }
        command::C_STORE_RQ => {
            let req = request(a, &m, "C-STORE");
            let st = match scu::store_received(&m, &tsx.uid, a.calling_ae()) {
                Ok(bytes) => h.store(&req, bytes),
                Err(e) => Status::new(status::CANNOT_UNDERSTAND, e.message),
            };
            let rsp = with_comment(dimse::response(&m.command, command::C_STORE_RSP, st.code, false), &st);
            a.send(m.pc_id, &rsp, None)
        }
        command::C_FIND_RQ => {
            let req = request(a, &m, "C-FIND");
            let ident = codec::read_dataset_ts(m.data.as_deref().unwrap_or(&[]), &tsx, ReadOptions::default())?;
            match h.find(&req, ident) {
                Ok(results) => {
                    for r in results {
                        let rsp = dimse::response(&m.command, command::C_FIND_RSP, status::PENDING, true);
                        a.send(m.pc_id, &rsp, Some(&codec::dataset_to_bytes(&r, &tsx)?))?;
                    }
                    a.send(m.pc_id, &dimse::response(&m.command, command::C_FIND_RSP, status::SUCCESS, false), None)
                }
                Err(st) => a.send(m.pc_id, &with_comment(dimse::response(&m.command, command::C_FIND_RSP, st.code, false), &st), None),
            }
        }
        command::C_MOVE_RQ | command::C_GET_RQ => {
            let is_move = dimse::field(&m.command) == command::C_MOVE_RQ;
            let rsp_field = if is_move { command::C_MOVE_RSP } else { command::C_GET_RSP };
            let req = request(a, &m, if is_move { "C-MOVE" } else { "C-GET" });
            let ident = codec::read_dataset_ts(m.data.as_deref().unwrap_or(&[]), &tsx, ReadOptions::default())?;
            if is_move {
                let dest = req.move_destination.clone().unwrap_or_default();
                if !cfg.ae_table.contains_key(&dest) {
                    let st = Status::new(status::MOVE_DESTINATION_UNKNOWN, format!("Unknown move destination {}", dest));
                    return a.send(m.pc_id, &with_comment(dimse::response(&m.command, rsp_field, st.code, false), &st), None);
                }
            }
            let files = match h.retrieve(&req, ident) {
                Ok(f) => f,
                Err(st) => return a.send(m.pc_id, &with_comment(dimse::response(&m.command, rsp_field, st.code, false), &st), None),
            };
            let items: Vec<StoreItem> = files.iter().filter_map(|f| StoreItem::from_bytes(f).ok()).collect();
            let mut ops = SubOps { failed: (files.len() - items.len()) as u32, remaining: items.len() as u32, ..Default::default() };
            let pending = |a: &mut Association, ops: &SubOps| -> Result<()> {
                let mut c = dimse::response(&m.command, rsp_field, status::PENDING, false);
                counts(&mut c, ops, true);
                a.send(m.pc_id, &c, None)
            };
            if is_move {
                let dest = req.move_destination.clone().unwrap_or_default();
                let (host, port) = cfg.ae_table[&dest].clone();
                let mut scu_cfg = ScuConfig::new(&cfg.ae_title, &dest, &host, port);
                scu_cfg.tls = cfg.move_tls.clone();
                scu_cfg.timeout = cfg.timeout;
                match scu_cfg.associate(scu::store_contexts(&items, &[]), vec![]) {
                    Ok(mut sub) => {
                        for it in &items {
                            tally(&mut ops, scu::store_on(&mut sub, it, Some((a.calling_ae(), req.message_id)), req.priority));
                            if ops.remaining > 0 {
                                pending(a, &ops)?;
                            }
                        }
                        let _ = sub.release();
                    }
                    Err(e) => {
                        let st = Status::new(status::OUT_OF_RESOURCES_SUBOPS, e.message.clone());
                        let mut c = with_comment(dimse::response(&m.command, rsp_field, st.code, false), &st);
                        ops.failed += ops.remaining;
                        ops.remaining = 0;
                        counts(&mut c, &ops, false);
                        return a.send(m.pc_id, &c, None);
                    }
                }
            } else {
                for it in &items {
                    let r = scu::store_on(a, it, None, req.priority);
                    if let Err(e) = &r {
                        if e.kind == crate::ul::DICOM_ASSOCIATION_EXCEPTION && a.is_closed() {
                            return Err(e.clone());
                        }
                    }
                    tally(&mut ops, r);
                    if ops.remaining > 0 {
                        pending(a, &ops)?;
                    }
                }
            }
            let code = if ops.failed == 0 && ops.warning == 0 {
                status::SUCCESS
            } else if ops.completed == 0 && ops.warning == 0 {
                status::OUT_OF_RESOURCES_SUBOPS
            } else {
                status::WARNING_SUBOPS
            };
            let mut c = dimse::response(&m.command, rsp_field, code, false);
            counts(&mut c, &ops, false);
            a.send(m.pc_id, &c, None)
        }
        command::C_CANCEL_RQ => Ok(()), // nothing in progress: requests are answered in full
        other => {
            let rsp = dimse::response(&m.command, other | 0x8000, status::UNRECOGNIZED_OPERATION, false);
            a.send(m.pc_id, &rsp, None)
        }
    }
}

fn tally(ops: &mut SubOps, r: Result<scu::Outcome>) {
    ops.remaining = ops.remaining.saturating_sub(1);
    match r.map(|o| status_class(o.status)) {
        Ok(StatusClass::Success) => ops.completed += 1,
        Ok(StatusClass::Warning) => ops.warning += 1,
        _ => ops.failed += 1,
    }
}

fn counts(c: &mut DataSet, ops: &SubOps, with_remaining: bool) {
    if with_remaining {
        c.insert(Element::us(tags::NUMBER_OF_REMAINING_SUBOPERATIONS, ops.remaining as u16));
    }
    c.insert(Element::us(tags::NUMBER_OF_COMPLETED_SUBOPERATIONS, ops.completed as u16));
    c.insert(Element::us(tags::NUMBER_OF_FAILED_SUBOPERATIONS, ops.failed as u16));
    c.insert(Element::us(tags::NUMBER_OF_WARNING_SUBOPERATIONS, ops.warning as u16));
}

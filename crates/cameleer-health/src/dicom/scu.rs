//! The SCU side of the DIMSE services: C-ECHO, C-STORE, C-FIND, C-MOVE and C-GET.

use std::time::Duration;

use cameleer_core::error::{CamelError, Result};
use cameleer_tls::camel_support::EndpointTls;

use crate::dicom::assoc::{Association, Received, DEFAULT_MAX_PDU, NATIVE_TRANSFER_SYNTAXES};
use crate::dicom::codec::{self, DicomFile, ReadOptions};
use crate::dicom::dimse::{self, command, sop, status_class, StatusClass};
use crate::dicom::object::{DataSet, Element};
use crate::dicom::tag::tags;
use crate::dicom::ts::{self, TransferSyntax};
use crate::dicom::ul::{AssociateRq, PresentationContextRq, RoleSelection, UserIdentity, UserInfo, APPLICATION_CONTEXT};

/// Where and how to connect.
#[derive(Clone)]
pub struct ScuConfig {
    pub calling_ae: String,
    pub called_ae: String,
    pub host: String,
    pub port: u16,
    pub tls: Option<EndpointTls>,
    pub max_pdu_length: u32,
    pub connect_timeout: Duration,
    /// Time to wait for the association answer and each response.
    pub timeout: Duration,
    pub user_identity: Option<UserIdentity>,
    pub priority: u16,
}

impl ScuConfig {
    /// Defaults for a peer.
    pub fn new(calling_ae: &str, called_ae: &str, host: &str, port: u16) -> ScuConfig {
        ScuConfig {
            calling_ae: calling_ae.to_string(),
            called_ae: called_ae.to_string(),
            host: host.to_string(),
            port,
            tls: None,
            max_pdu_length: DEFAULT_MAX_PDU,
            connect_timeout: Duration::from_secs(30),
            timeout: Duration::from_secs(60),
            user_identity: None,
            priority: dimse::priority::MEDIUM,
        }
    }

    /// Request an association with these presentation contexts (abstract syntax, transfer
    /// syntaxes) and role selections.
    pub fn associate(&self, contexts: Vec<(String, Vec<String>)>, roles: Vec<RoleSelection>) -> Result<Association> {
        if contexts.len() > 128 {
            return Err(CamelError::illegal_argument(format!("{} presentation contexts requested; an association holds at most 128", contexts.len())));
        }
        let pcs = contexts.into_iter().enumerate().map(|(i, (a, t))| PresentationContextRq { id: (2 * i + 1) as u8, abstract_syntax: a, transfer_syntaxes: t }).collect();
        let rq = AssociateRq {
            called_ae: self.called_ae.clone(),
            calling_ae: self.calling_ae.clone(),
            application_context: APPLICATION_CONTEXT.to_string(),
            presentation_contexts: pcs,
            user: UserInfo {
                max_pdu_length: self.max_pdu_length,
                implementation_class_uid: codec::IMPLEMENTATION_CLASS_UID.to_string(),
                implementation_version_name: Some(codec::IMPLEMENTATION_VERSION_NAME.to_string()),
                role_selections: roles,
                user_identity: self.user_identity.clone(),
                ..Default::default()
            },
        };
        let host = if self.host == "0.0.0.0" { "127.0.0.1" } else { &self.host };
        Association::request(host, self.port, self.tls.as_ref(), rq, self.connect_timeout, self.timeout)
    }
}

fn native() -> Vec<String> {
    NATIVE_TRANSFER_SYNTAXES[..2].iter().map(|s| s.to_string()).collect()
}

fn no_context(a: &Association, what: &str) -> CamelError {
    CamelError::new(crate::dicom::ul::DICOM_ASSOCIATION_EXCEPTION, format!("{} ({}) accepted no presentation context for {}", a.called_ae(), a.remote, what))
}

/// The result of one DIMSE request.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub status: u16,
    pub error_comment: Option<String>,
}

impl Outcome {
    fn from(c: &DataSet) -> Outcome {
        Outcome { status: dimse::status_of(c), error_comment: c.string(tags::ERROR_COMMENT).filter(|s| !s.is_empty()) }
    }
}

/// C-ECHO.
pub fn echo(cfg: &ScuConfig) -> Result<Outcome> {
    let mut a = cfg.associate(vec![(sop::VERIFICATION.to_string(), native())], vec![])?;
    let (pc, _) = a.contexts_for(sop::VERIFICATION, None).into_iter().next().ok_or_else(|| no_context(&a, "Verification"))?;
    let mut c = dimse::command_set(command::C_ECHO_RQ, false);
    let id = a.next_message_id();
    c.insert(dimse::ui(tags::AFFECTED_SOP_CLASS_UID, sop::VERIFICATION));
    c.insert(Element::us(tags::MESSAGE_ID, id));
    a.send(pc, &c, None)?;
    let rsp = a.receive_message()?;
    let out = Outcome::from(&rsp.command);
    a.release()?;
    Ok(out)
}

/// One instance prepared for C-STORE.
pub struct StoreItem {
    pub sop_class: String,
    pub sop_instance: String,
    pub file: DicomFile,
    /// The data set bytes after the file meta information (as stored).
    pub raw: Vec<u8>,
}

impl StoreItem {
    /// From Part 10 bytes (or a bare data set).
    pub fn from_bytes(bytes: &[u8]) -> Result<StoreItem> {
        let (meta, offset) = codec::split_part10(bytes)?;
        let file = DicomFile::parse(bytes, ReadOptions::default())?;
        let sop_class = file.dataset.sop_class_uid().or_else(|| meta.string(tags::MEDIA_STORAGE_SOP_CLASS_UID)).ok_or_else(|| CamelError::new("InvalidPayloadException", "The object has no SOP Class UID"))?;
        let sop_instance = file.dataset.sop_instance_uid().or_else(|| meta.string(tags::MEDIA_STORAGE_SOP_INSTANCE_UID)).ok_or_else(|| CamelError::new("InvalidPayloadException", "The object has no SOP Instance UID"))?;
        let raw = if offset.is_some() { bytes[offset.unwrap()..].to_vec() } else { bytes.to_vec() };
        Ok(StoreItem { sop_class, sop_instance, file, raw })
    }

    /// The data set encoded in a transfer syntax (the stored bytes when it is the same).
    pub fn encoded(&self, target: &str) -> Result<Vec<u8>> {
        let from = self.file.transfer_syntax();
        if from.uid == target {
            return Ok(self.raw.clone());
        }
        let to = TransferSyntax::from_uid(target);
        if from.encapsulated || to.encapsulated {
            return Err(CamelError::new("DicomTranscodeException", format!("Cannot send {} as {}: no pixel data codec in this build", from.name(), to.name())));
        }
        codec::dataset_to_bytes(&self.file.dataset, &to)
    }
}

/// Presentation contexts proposed to store items: per SOP class, the stored syntax (alone when
/// it is compressed) and the native little endian syntaxes.
pub fn store_contexts(items: &[StoreItem], extra: &[String]) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for it in items {
        let stored = it.file.transfer_syntax();
        let mut wanted: Vec<Vec<String>> = Vec::new();
        if stored.encapsulated {
            wanted.push(vec![stored.uid.clone()]);
            wanted.push(native());
        } else {
            let mut v = vec![stored.uid.clone()];
            for n in native() {
                if !v.contains(&n) {
                    v.push(n);
                }
            }
            wanted.push(v);
        }
        for e in extra {
            if !wanted.iter().any(|w| w.contains(e)) {
                wanted.push(vec![e.clone()]);
            }
        }
        for w in wanted {
            if !out.iter().any(|(a, t)| *a == it.sop_class && *t == w) {
                out.push((it.sop_class.clone(), w));
            }
        }
    }
    out
}

/// Send one C-STORE on an open association, choosing the accepted context.
pub fn store_on(a: &mut Association, it: &StoreItem, move_originator: Option<(&str, u16)>, priority: u16) -> Result<Outcome> {
    let stored = it.file.transfer_syntax_uid();
    let ctxs = a.contexts_for(&it.sop_class, Some(&stored));
    // a context in the stored syntax, else a native one we can convert into
    let pick = ctxs.iter().find(|(_, t)| *t == stored).or_else(|| if TransferSyntax::from_uid(&stored).encapsulated { None } else { ctxs.iter().find(|(_, t)| !TransferSyntax::from_uid(t).encapsulated) });
    let Some((pc, tsx)) = pick.cloned() else {
        if ctxs.is_empty() {
            return Err(no_context(a, &format!("SOP class {}", sop_name(&it.sop_class))));
        }
        return Err(CamelError::new(
            "DicomTranscodeException",
            format!("{} accepts {} only as {}; the object is {} and this build has no codec to convert it", a.called_ae(), sop_name(&it.sop_class), ctxs.iter().map(|(_, t)| TransferSyntax::from_uid(t).name()).collect::<Vec<_>>().join(", "), TransferSyntax::from_uid(&stored).name()),
        ));
    };
    let data = it.encoded(&tsx)?;
    let mut c = dimse::command_set(command::C_STORE_RQ, true);
    let id = a.next_message_id();
    c.insert(dimse::ui(tags::AFFECTED_SOP_CLASS_UID, &it.sop_class));
    c.insert(Element::us(tags::MESSAGE_ID, id));
    c.insert(Element::us(tags::PRIORITY, priority));
    c.insert(dimse::ui(tags::AFFECTED_SOP_INSTANCE_UID, &it.sop_instance));
    if let Some((aet, mid)) = move_originator {
        c.insert(dimse::ae(tags::MOVE_ORIGINATOR_AE_TITLE, aet));
        c.insert(Element::us(tags::MOVE_ORIGINATOR_MESSAGE_ID, mid));
    }
    a.send(pc, &c, Some(&data))?;
    let rsp = a.receive_message()?;
    Ok(Outcome::from(&rsp.command))
}

/// The registry name of a SOP class (or the UID).
pub fn sop_name(uid: &str) -> String {
    crate::dicom::dict::uid(uid).map(|e| e.name.to_string()).unwrap_or_else(|| uid.to_string())
}

/// C-STORE of several instances on one association; one outcome per instance.
pub fn store(cfg: &ScuConfig, files: &[Vec<u8>], extra_ts: &[String]) -> Result<Vec<(String, Outcome)>> {
    let items = files.iter().map(|f| StoreItem::from_bytes(f)).collect::<Result<Vec<_>>>()?;
    let mut a = cfg.associate(store_contexts(&items, extra_ts), vec![])?;
    let mut out = Vec::new();
    for it in &items {
        out.push((it.sop_instance.clone(), store_on(&mut a, it, None, cfg.priority)?));
    }
    a.release()?;
    Ok(out)
}

/// C-FIND: the matches (identifiers) and the final outcome.
pub fn find(cfg: &ScuConfig, model_uid: &str, identifier: &DataSet) -> Result<(Vec<DataSet>, Outcome)> {
    let mut a = cfg.associate(vec![(model_uid.to_string(), native())], vec![])?;
    let (pc, tsx) = a.contexts_for(model_uid, None).into_iter().next().ok_or_else(|| no_context(&a, &sop_name(model_uid)))?;
    let enc = TransferSyntax::from_uid(&tsx);
    let mut c = dimse::command_set(command::C_FIND_RQ, true);
    let id = a.next_message_id();
    c.insert(dimse::ui(tags::AFFECTED_SOP_CLASS_UID, model_uid));
    c.insert(Element::us(tags::MESSAGE_ID, id));
    c.insert(Element::us(tags::PRIORITY, cfg.priority));
    a.send(pc, &c, Some(&codec::dataset_to_bytes(identifier, &enc)?))?;
    let mut results = Vec::new();
    loop {
        let m = a.receive_message()?;
        let st = dimse::status_of(&m.command);
        if status_class(st) == StatusClass::Pending {
            if let Some(d) = m.data {
                results.push(codec::read_dataset_ts(&d, &enc, ReadOptions::default())?);
            }
            continue;
        }
        let out = Outcome::from(&m.command);
        a.release()?;
        return Ok((results, out));
    }
}

/// Sub-operation counts of C-MOVE and C-GET.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SubOps {
    pub status: u16,
    pub error_comment: Option<String>,
    pub completed: u32,
    pub failed: u32,
    pub warning: u32,
    pub remaining: u32,
}

impl SubOps {
    fn update(&mut self, c: &DataSet) {
        self.status = dimse::status_of(c);
        self.error_comment = c.string(tags::ERROR_COMMENT).filter(|s| !s.is_empty());
        let n = |t| c.int(t).map(|v| v as u32);
        if let Some(v) = n(tags::NUMBER_OF_COMPLETED_SUBOPERATIONS) {
            self.completed = v;
        }
        if let Some(v) = n(tags::NUMBER_OF_FAILED_SUBOPERATIONS) {
            self.failed = v;
        }
        if let Some(v) = n(tags::NUMBER_OF_WARNING_SUBOPERATIONS) {
            self.warning = v;
        }
        self.remaining = n(tags::NUMBER_OF_REMAINING_SUBOPERATIONS).unwrap_or(0);
    }
}

/// C-MOVE to `destination`.
pub fn move_(cfg: &ScuConfig, model_uid: &str, identifier: &DataSet, destination: &str) -> Result<SubOps> {
    let mut a = cfg.associate(vec![(model_uid.to_string(), native())], vec![])?;
    let (pc, tsx) = a.contexts_for(model_uid, None).into_iter().next().ok_or_else(|| no_context(&a, &sop_name(model_uid)))?;
    let enc = TransferSyntax::from_uid(&tsx);
    let mut c = dimse::command_set(command::C_MOVE_RQ, true);
    let id = a.next_message_id();
    c.insert(dimse::ui(tags::AFFECTED_SOP_CLASS_UID, model_uid));
    c.insert(Element::us(tags::MESSAGE_ID, id));
    c.insert(Element::us(tags::PRIORITY, cfg.priority));
    c.insert(dimse::ae(tags::MOVE_DESTINATION, destination));
    a.send(pc, &c, Some(&codec::dataset_to_bytes(identifier, &enc)?))?;
    let mut ops = SubOps::default();
    loop {
        let m = a.receive_message()?;
        ops.update(&m.command);
        if status_class(ops.status) != StatusClass::Pending {
            a.release()?;
            return Ok(ops);
        }
    }
}

/// C-GET: the retrieved instances as Part 10 files, and the counts. `storage_classes` are the
/// SOP classes offered for the C-STORE sub-operations (SCP role), each in the native syntaxes
/// and in `extra_ts`.
pub fn get(cfg: &ScuConfig, model_uid: &str, identifier: &DataSet, storage_classes: &[String], extra_ts: &[String]) -> Result<(Vec<Vec<u8>>, SubOps)> {
    let mut contexts = vec![(model_uid.to_string(), native())];
    let mut roles = Vec::new();
    let mut tsl = native();
    tsl.push(ts::DEFLATED_EXPLICIT_VR_LITTLE_ENDIAN.to_string());
    for e in extra_ts {
        if !tsl.contains(e) {
            tsl.push(e.clone());
        }
    }
    for s in storage_classes.iter().take(127) {
        contexts.push((s.clone(), tsl.clone()));
        roles.push(RoleSelection { sop_class: s.clone(), scu: false, scp: true });
    }
    let mut a = cfg.associate(contexts, roles)?;
    let (pc, tsx) = a.contexts_for(model_uid, None).into_iter().next().ok_or_else(|| no_context(&a, &sop_name(model_uid)))?;
    let enc = TransferSyntax::from_uid(&tsx);
    let mut c = dimse::command_set(command::C_GET_RQ, true);
    let id = a.next_message_id();
    c.insert(dimse::ui(tags::AFFECTED_SOP_CLASS_UID, model_uid));
    c.insert(Element::us(tags::MESSAGE_ID, id));
    c.insert(Element::us(tags::PRIORITY, cfg.priority));
    a.send(pc, &c, Some(&codec::dataset_to_bytes(identifier, &enc)?))?;
    let mut ops = SubOps::default();
    let mut files = Vec::new();
    loop {
        let m = match a.receive()? {
            Received::Message(m) => m,
            Received::Released => break,
            Received::Aborted(why) => return Err(CamelError::new(crate::dicom::ul::DICOM_ASSOCIATION_EXCEPTION, why)),
        };
        match dimse::field(&m.command) {
            command::C_STORE_RQ => {
                let tsx = a.transfer_syntax(m.pc_id).unwrap_or(ts::IMPLICIT_VR_LITTLE_ENDIAN).to_string();
                let st = match store_received(&m, &tsx, a.calling_ae()) {
                    Ok(bytes) => {
                        files.push(bytes);
                        dimse::status::SUCCESS
                    }
                    Err(_) => dimse::status::CANNOT_UNDERSTAND,
                };
                let rsp = dimse::response(&m.command, command::C_STORE_RSP, st, false);
                a.send(m.pc_id, &rsp, None)?;
            }
            command::C_GET_RSP => {
                ops.update(&m.command);
                if status_class(ops.status) != StatusClass::Pending {
                    a.release()?;
                    return Ok((files, ops));
                }
            }
            _ => {}
        }
    }
    Ok((files, ops))
}

/// Turn a received C-STORE-RQ into Part 10 bytes (meta from the command and the context).
pub fn store_received(m: &crate::dicom::assoc::Message, transfer_syntax: &str, source_ae: &str) -> Result<Vec<u8>> {
    let data = m.data.as_deref().ok_or_else(|| CamelError::new("InvalidPayloadException", "C-STORE-RQ without a data set"))?;
    let class = m.command.string(tags::AFFECTED_SOP_CLASS_UID).unwrap_or_default();
    let inst = m.command.string(tags::AFFECTED_SOP_INSTANCE_UID).unwrap_or_default();
    Ok(part10_from_dataset_bytes(data, transfer_syntax, &class, &inst, source_ae))
}

/// Build Part 10 bytes from data set bytes without decoding them.
pub fn part10_from_dataset_bytes(data: &[u8], transfer_syntax: &str, sop_class: &str, sop_instance: &str, source_ae: &str) -> Vec<u8> {
    let mut f = DicomFile::new(DataSet::new(), transfer_syntax);
    f.meta.insert(dimse::ui(tags::MEDIA_STORAGE_SOP_CLASS_UID, sop_class));
    f.meta.insert(dimse::ui(tags::MEDIA_STORAGE_SOP_INSTANCE_UID, sop_instance));
    if !source_ae.is_empty() {
        f.meta.insert(dimse::ae(tags::SOURCE_APPLICATION_ENTITY_TITLE, source_ae));
    }
    let mut out = f.header_bytes().unwrap_or_default();
    out.extend_from_slice(data);
    out
}

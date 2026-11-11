//! The `dicom:` component: DIMSE over TCP and TLS.
//!
//! `dicom:AET@host:port[?options]`
//!
//! - **Consumer** (SCP): listens on `host:port` as `AET`. C-ECHO is answered directly. Each
//!   C-STORE becomes an exchange whose body is the instance as Part 10 bytes; C-FIND, C-MOVE and
//!   C-GET become exchanges whose body is the identifier as a Value tree, and the route answers
//!   with the matches (C-FIND: an array of Value trees or data sets) or the instances (C-MOVE
//!   and C-GET: an array of Part 10 bytes or Value trees), which the SCP sends back on the same
//!   association (C-GET) or to the move destination (`moveDestinations`). A failed exchange
//!   answers with a failure status (C-STORE `A700`, queries `C000`) and its message as the
//!   Error Comment; a route can set any status with `CamelDicomStatus`.
//! - **Producer** (SCU): `operation=store` (default; the body is Part 10 bytes, a Value tree or
//!   an array of them), `echo`, `find` (the body is the identifier: a Value tree or a map of
//!   keywords such as `{"PatientID": "P1", "StudyInstanceUID": ""}`; the result is an array of
//!   Value trees), `move` (to `moveDestination`) and `get` (the result is an array of Part 10
//!   bytes). Failure statuses raise `DicomStatusException`; warnings only set
//!   `CamelDicomStatus`.

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cameleer_core::context::{CamelContext, WeakContext};
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::{Exchange, ExchangePattern};
use cameleer_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use cameleer_core::util::Parameters;
use cameleer_core::value::Value;
use cameleer_tls::camel_support::{endpoint_tls, EndpointTls};

use crate::dicom::charset::CharacterSet;
use crate::dicom::dataformat::body_bytes;
use crate::dicom::dimse::{self, status, status_class, QueryModel, StatusClass};
use crate::dicom::headers;
use crate::dicom::model;
use crate::dicom::object::{DataSet, Element};
use crate::dicom::scp::{DicomServer, Request, ScpConfig, ScpHandler, SopFilter, Status};
use crate::dicom::scu::{self, ScuConfig};
use crate::dicom::tag::tags;
use crate::dicom::ul::UserIdentity;
use crate::dicom::vr::Vr;

/// The `dicom` component.
#[derive(Default)]
pub struct DicomComponent;

/// Producer operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Echo,
    Store,
    Find,
    Move,
    Get,
}

impl Operation {
    fn parse(s: &str) -> Result<Operation> {
        match s.trim().to_ascii_lowercase().as_str() {
            "echo" | "c-echo" => Ok(Operation::Echo),
            "store" | "c-store" => Ok(Operation::Store),
            "find" | "c-find" | "query" => Ok(Operation::Find),
            "move" | "c-move" => Ok(Operation::Move),
            "get" | "c-get" => Ok(Operation::Get),
            other => Err(CamelError::illegal_argument(format!("Unknown dicom operation '{}' (echo, store, find, move, get)", other))),
        }
    }
}

/// Endpoint configuration.
#[derive(Clone)]
pub struct DicomConfig {
    pub ae_title: String,
    pub host: String,
    pub port: u16,
    pub calling_aet: String,
    pub max_pdu_length: u32,
    pub connect_timeout: Duration,
    pub timeout: Duration,
    pub tls: Option<EndpointTls>,
    pub operation: Operation,
    pub query_model: QueryModel,
    pub query_level: String,
    pub move_destination: Option<String>,
    pub priority: u16,
    pub transfer_syntaxes: Vec<String>,
    pub storage_sop_classes: Vec<String>,
    pub user_identity: Option<UserIdentity>,
    pub sop_classes: SopFilter,
    pub check_called_aet: bool,
    pub accepted_calling_aets: Vec<String>,
    pub move_destinations: HashMap<String, (String, u16)>,
    pub max_associations: usize,
    pub echo_to_route: bool,
    pub require_user_identity: bool,
}

fn list(s: Option<String>) -> Vec<String> {
    s.map(|s| s.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect()).unwrap_or_default()
}

fn uid_list(s: Option<String>) -> Result<Vec<String>> {
    list(s)
        .into_iter()
        .map(|p| {
            if p.starts_with(|c: char| c.is_ascii_digit()) {
                Ok(p)
            } else {
                crate::dicom::dict::uid_by_keyword(&p).map(|e| e.uid.to_string()).ok_or_else(|| CamelError::illegal_argument(format!("Unknown UID keyword: {}", p)))
            }
        })
        .collect()
}

/// Parse `AET@host:port`, `host:port` or `AET@host`.
pub fn parse_address(remaining: &str, default_ae: &str) -> Result<(String, String, u16)> {
    let r = remaining.trim_start_matches('/').trim_end_matches('/');
    let (ae, hp) = match r.split_once('@') {
        Some((a, h)) => (a.to_string(), h),
        None => (default_ae.to_string(), r),
    };
    if ae.is_empty() || ae.len() > 16 {
        return Err(CamelError::illegal_argument(format!("AE title must be 1 to 16 characters: '{}'", ae)));
    }
    let (host, port) = match hp.rsplit_once(':') {
        Some((h, p)) if !h.ends_with(']') || h.starts_with('[') => (h.trim_matches(['[', ']']).to_string(), p.parse::<u16>().map_err(|_| CamelError::illegal_argument(format!("Invalid port in dicom URI: {}", p)))?),
        _ => (hp.to_string(), 104),
    };
    Ok((ae, if host.is_empty() { "0.0.0.0".into() } else { host }, port))
}

impl Component for DicomComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, p: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let (ae_title, host, port) = parse_address(remaining, "CAMELEER")?;
        let tls = endpoint_tls(ctx, p, "dicom", false, &["tls"])?;
        let operation = Operation::parse(&p.take_string("operation", "store"))?;
        let query_model = QueryModel::parse(&p.take_string("queryModel", "StudyRoot")).ok_or_else(|| CamelError::illegal_argument("queryModel must be StudyRoot, PatientRoot or MWL"))?;
        let priority = dimse::parse_priority(&p.take_string("priority", "MEDIUM")).ok_or_else(|| CamelError::illegal_argument("priority must be LOW, MEDIUM or HIGH"))?;
        let user_identity = match p.take("username") {
            Some(u) => {
                let pw = p.take("password");
                Some(UserIdentity { kind: if pw.is_some() { 2 } else { 1 }, positive_response_requested: p.take_bool("userIdentityResponse", false)?, primary: u.into_bytes(), secondary: pw.unwrap_or_default().into_bytes() })
            }
            None => None,
        };
        let mut move_destinations = HashMap::new();
        for d in list(p.take("moveDestinations")) {
            let (name, addr) = d.split_once('=').ok_or_else(|| CamelError::illegal_argument(format!("moveDestinations entries are AET=host:port, got '{}'", d)))?;
            let (_, h, port) = parse_address(addr, name)?;
            move_destinations.insert(name.trim().to_string(), (h, port));
        }
        let storage = match p.take("storageSopClasses") {
            None => dimse::COMMON_STORAGE.iter().map(|s| s.to_string()).collect(),
            Some(s) if s.eq_ignore_ascii_case("common") => dimse::COMMON_STORAGE.iter().map(|s| s.to_string()).collect(),
            Some(s) => uid_list(Some(s))?,
        };
        let cfg = DicomConfig {
            ae_title,
            host,
            port,
            calling_aet: p.take_string("callingAet", "CAMELEER"),
            max_pdu_length: p.take_u64("maxPduLength", crate::dicom::assoc::DEFAULT_MAX_PDU as u64)? as u32,
            connect_timeout: p.take_duration("connectTimeout", Duration::from_secs(30))?,
            timeout: p.take_duration("timeout", Duration::from_secs(60))?,
            tls,
            operation,
            query_model,
            query_level: p.take_string("queryLevel", "STUDY").to_ascii_uppercase(),
            move_destination: p.take("moveDestination").filter(|s| !s.is_empty()),
            priority,
            transfer_syntaxes: uid_list(p.take("transferSyntaxes"))?,
            storage_sop_classes: storage,
            user_identity,
            sop_classes: SopFilter::parse(&p.take_string("sopClasses", "verification,storage,query"))?,
            check_called_aet: p.take_bool("checkCalledAet", false)?,
            accepted_calling_aets: list(p.take("acceptedCallingAets")),
            move_destinations,
            max_associations: p.take_usize("maxAssociations", 50)?,
            echo_to_route: p.take_bool("echoToRoute", false)?,
            require_user_identity: p.take_bool("requireUserIdentity", false)?,
        };
        if cfg.calling_aet.is_empty() || cfg.calling_aet.len() > 16 {
            return Err(CamelError::illegal_argument("callingAet must be 1 to 16 characters"));
        }
        Ok(Arc::new(DicomEndpoint { uri: uri.to_string(), ctx: ctx.downgrade(), cfg, server: Arc::new(Mutex::new(None)) }))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A `dicom` endpoint.
pub struct DicomEndpoint {
    uri: String,
    ctx: WeakContext,
    cfg: DicomConfig,
    server: Arc<Mutex<Option<Arc<DicomServer>>>>,
}

impl DicomEndpoint {
    /// The bound port while a consumer runs.
    pub fn local_port(&self) -> Option<u16> {
        self.server.lock().unwrap().as_ref().map(|s| s.port())
    }

    pub fn config(&self) -> &DicomConfig {
        &self.cfg
    }
}

impl Endpoint for DicomEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(DicomProducer { cfg: self.cfg.clone() }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        Ok(Arc::new(DicomConsumer { cfg: self.cfg.clone(), ctx: self.ctx.clone(), processor, server: self.server.clone() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---------------------------------------------------------------------------------------
// consumer

struct DicomConsumer {
    cfg: DicomConfig,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
    server: Arc<Mutex<Option<Arc<DicomServer>>>>,
}

struct RouteHandler {
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
    echo_to_route: bool,
}

impl RouteHandler {
    fn exchange(&self, req: &Request, body: Value) -> Exchange {
        let mut ex = Exchange::new(self.ctx.upgrade());
        ex.pattern = ExchangePattern::InOut;
        ex.set_body(body);
        ex.set_header(headers::COMMAND, req.command);
        ex.set_header(headers::CALLING_AET, req.calling_ae.clone());
        ex.set_header(headers::CALLED_AET, req.called_ae.clone());
        ex.set_header(headers::REMOTE_ADDRESS, req.remote.clone());
        ex.set_header(headers::SOP_CLASS_UID, req.sop_class.clone());
        if !req.sop_instance.is_empty() {
            ex.set_header(headers::SOP_INSTANCE_UID, req.sop_instance.clone());
        }
        ex.set_header(headers::TRANSFER_SYNTAX_UID, req.transfer_syntax.clone());
        ex.set_header(headers::MESSAGE_ID, req.message_id as i64);
        ex.set_header(headers::PRIORITY, match req.priority {
            1 => "HIGH",
            2 => "LOW",
            _ => "MEDIUM",
        });
        if let Some(d) = &req.move_destination {
            ex.set_header(headers::MOVE_DESTINATION, d.clone());
        }
        if let Some(o) = &req.move_originator {
            ex.set_header(headers::MOVE_ORIGINATOR_AET, o.clone());
        }
        if let Some(u) = &req.username {
            ex.set_header("CamelDicomUsername", u.clone());
        }
        ex
    }

    fn run(&self, ex: &mut Exchange) {
        if let Err(e) = self.processor.process(ex) {
            ex.set_exception(e);
        }
        if let Some(e) = &ex.exception {
            cameleer_core::camel_warn!("dicom", "Error processing DICOM exchange {}: {} - {}", ex.exchange_id, e.kind, e.message);
        }
    }

    /// The status the route chose, or the failure status for an exception.
    fn status(ex: &Exchange, failure: u16) -> Status {
        let forced = ex.header(headers::STATUS).and_then(|v| v.as_i64()).map(|s| s as u16);
        let comment = ex.header(headers::ERROR_COMMENT).map(|v| v.to_string()).filter(|s| !s.is_empty());
        match (&ex.exception, forced) {
            (_, Some(code)) => Status { code, comment },
            (Some(e), None) => Status { code: failure, comment: Some(format!("{}: {}", e.kind, e.message)) },
            (None, None) => Status::SUCCESS,
        }
    }
}

fn to_dataset(v: &Value) -> Result<DataSet> {
    match v {
        Value::Bytes(b) => crate::dicom::DicomFile::parse(b, crate::dicom::ReadOptions::default()).map(|f| f.dataset),
        Value::Object(_) => model::value_to_dataset(v, &CharacterSet::default()),
        other => Err(CamelError::new("InvalidPayloadException", format!("Expected a DICOM data set (Value tree or bytes), got {}", other.type_name()))),
    }
}

fn items(body: &Value) -> Vec<Value> {
    match body {
        Value::Null => Vec::new(),
        Value::Array(a) => a.clone(),
        other => vec![other.clone()],
    }
}

impl ScpHandler for RouteHandler {
    fn echo(&self, req: &Request) -> Status {
        if !self.echo_to_route {
            return Status::SUCCESS;
        }
        let mut ex = self.exchange(req, Value::Null);
        self.run(&mut ex);
        Self::status(&ex, status::PROCESSING_FAILURE)
    }

    fn store(&self, req: &Request, part10: Vec<u8>) -> Status {
        let mut ex = self.exchange(req, Value::Bytes(part10));
        self.run(&mut ex);
        Self::status(&ex, status::OUT_OF_RESOURCES)
    }

    fn find(&self, req: &Request, identifier: DataSet) -> std::result::Result<Vec<DataSet>, Status> {
        let level = identifier.string(tags::QUERY_RETRIEVE_LEVEL).unwrap_or_default();
        let mut ex = self.exchange(req, model::dataset_to_value(&identifier, &CharacterSet::default()));
        ex.set_header(headers::QUERY_LEVEL, level);
        self.run(&mut ex);
        let st = Self::status(&ex, status::CANNOT_UNDERSTAND);
        if status_class(st.code) == StatusClass::Failure {
            return Err(st);
        }
        items(ex.body()).iter().map(to_dataset).collect::<Result<Vec<_>>>().map_err(|e| Status::new(status::CANNOT_UNDERSTAND, e.message))
    }

    fn retrieve(&self, req: &Request, identifier: DataSet) -> std::result::Result<Vec<Vec<u8>>, Status> {
        let level = identifier.string(tags::QUERY_RETRIEVE_LEVEL).unwrap_or_default();
        let mut ex = self.exchange(req, model::dataset_to_value(&identifier, &CharacterSet::default()));
        ex.set_header(headers::QUERY_LEVEL, level);
        self.run(&mut ex);
        let st = Self::status(&ex, status::OUT_OF_RESOURCES_SUBOPS);
        if status_class(st.code) == StatusClass::Failure {
            return Err(st);
        }
        items(ex.body()).iter().map(body_bytes).collect::<Result<Vec<_>>>().map_err(|e| Status::new(status::OUT_OF_RESOURCES_SUBOPS, e.message))
    }
}

impl Service for DicomConsumer {
    fn start(&self) -> Result<()> {
        let mut g = self.server.lock().unwrap();
        if g.is_some() {
            return Ok(());
        }
        let c = &self.cfg;
        let mut sc = ScpConfig::new(&c.ae_title, &c.host, c.port);
        sc.tls = c.tls.clone();
        sc.move_tls = c.tls.clone();
        sc.max_pdu_length = c.max_pdu_length;
        sc.sop_classes = c.sop_classes.clone();
        if !c.transfer_syntaxes.is_empty() {
            sc.transfer_syntaxes = c.transfer_syntaxes.clone();
        }
        sc.check_called_ae = c.check_called_aet;
        sc.calling_aes = c.accepted_calling_aets.clone();
        sc.require_user_identity = c.require_user_identity;
        sc.ae_table = c.move_destinations.clone();
        sc.timeout = c.timeout;
        sc.max_associations = c.max_associations;
        let handler = Arc::new(RouteHandler { ctx: self.ctx.clone(), processor: self.processor.clone(), echo_to_route: c.echo_to_route });
        let s = DicomServer::start(sc, handler)?;
        cameleer_core::camel_info!("dicom", "DICOM SCP {} listening on {}", c.ae_title, s.local_addr());
        *g = Some(Arc::new(s));
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        if let Some(s) = self.server.lock().unwrap().take() {
            s.stop();
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------
// producer

struct DicomProducer {
    cfg: DicomConfig,
}

impl Service for DicomProducer {}

impl DicomProducer {
    fn scu(&self, ex: &Exchange) -> ScuConfig {
        let c = &self.cfg;
        let mut s = ScuConfig::new(&c.calling_aet, &c.ae_title, &c.host, c.port);
        if let Some(a) = ex.header(headers::CALLED_AET).map(|v| v.to_string()).filter(|s| !s.is_empty()) {
            s.called_ae = a;
        }
        if let Some(a) = ex.header(headers::CALLING_AET).map(|v| v.to_string()).filter(|s| !s.is_empty()) {
            s.calling_ae = a;
        }
        s.tls = c.tls.clone();
        s.max_pdu_length = c.max_pdu_length;
        s.connect_timeout = c.connect_timeout;
        s.timeout = c.timeout;
        s.user_identity = c.user_identity.clone();
        s.priority = ex.header(headers::PRIORITY).and_then(|v| dimse::parse_priority(&v.to_string())).unwrap_or(c.priority);
        s
    }

    fn identifier(&self, ex: &Exchange) -> Result<DataSet> {
        let mut ds = match ex.body() {
            Value::Null => DataSet::new(),
            Value::String(s) if s.trim().is_empty() => DataSet::new(),
            Value::String(s) => model::value_to_dataset(&crate::dicom::dataformat::json_model_to_tree(&cameleer_core::json::parse(s)?).or_else(|_| cameleer_core::json::parse(s))?, &CharacterSet::default())?,
            other => to_dataset(other)?,
        };
        if !ds.contains(tags::QUERY_RETRIEVE_LEVEL) && self.cfg.query_model != QueryModel::Mwl {
            let level = ex.header(headers::QUERY_LEVEL).map(|v| v.to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| self.cfg.query_level.clone());
            ds.insert(Element::string(tags::QUERY_RETRIEVE_LEVEL, Vr::CS, &level.to_ascii_uppercase()));
        }
        Ok(ds)
    }
}

fn check(op: &str, ex: &mut Exchange, code: u16, comment: Option<String>) -> Result<()> {
    ex.set_header(headers::STATUS, code as i64);
    if let Some(c) = &comment {
        ex.set_header(headers::ERROR_COMMENT, c.clone());
    }
    match status_class(code) {
        StatusClass::Failure | StatusClass::Cancel => Err(dimse::status_error(op, code, comment.as_deref())),
        _ => Ok(()),
    }
}

impl Processor for DicomProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let op = match ex.header(headers::OPERATION).map(|v| v.to_string()).filter(|s| !s.is_empty()) {
            Some(o) => Operation::parse(&o)?,
            None => self.cfg.operation,
        };
        let scu_cfg = self.scu(ex);
        match op {
            Operation::Echo => {
                let o = scu::echo(&scu_cfg)?;
                check("C-ECHO", ex, o.status, o.error_comment)
            }
            Operation::Store => {
                let files = items(ex.body()).iter().map(body_bytes).collect::<Result<Vec<_>>>()?;
                if files.is_empty() {
                    return Err(CamelError::new("InvalidPayloadException", "Nothing to store: the body is empty"));
                }
                let out = scu::store(&scu_cfg, &files, &self.cfg.transfer_syntaxes)?;
                let failed: Vec<&(String, scu::Outcome)> = out.iter().filter(|(_, o)| status_class(o.status) == StatusClass::Failure).collect();
                ex.set_header(headers::COMPLETED, (out.len() - failed.len()) as i64);
                ex.set_header(headers::FAILED, failed.len() as i64);
                if out.len() == 1 {
                    ex.set_header(headers::SOP_INSTANCE_UID, out[0].0.clone());
                }
                match failed.first() {
                    Some((uid, o)) => check(&format!("C-STORE of {}", uid), ex, o.status, o.error_comment.clone()),
                    None => {
                        let worst = out.iter().map(|(_, o)| o.status).find(|s| *s != 0).unwrap_or(0);
                        check("C-STORE", ex, worst, out.iter().find_map(|(_, o)| o.error_comment.clone()))
                    }
                }
            }
            Operation::Find => {
                let ident = self.identifier(ex)?;
                let (results, o) = scu::find(&scu_cfg, self.cfg.query_model.find_uid(), &ident)?;
                ex.set_header(headers::RESULT_COUNT, results.len() as i64);
                let arr = results.iter().map(|d| model::dataset_to_value(d, &CharacterSet::default())).collect::<Vec<_>>();
                check("C-FIND", ex, o.status, o.error_comment)?;
                ex.set_body(Value::Array(arr));
                Ok(())
            }
            Operation::Move | Operation::Get => {
                let ident = self.identifier(ex)?;
                let (name, ops, files) = if op == Operation::Move {
                    let dest = ex
                        .header(headers::MOVE_DESTINATION)
                        .map(|v| v.to_string())
                        .filter(|s| !s.is_empty())
                        .or_else(|| self.cfg.move_destination.clone())
                        .ok_or_else(|| CamelError::illegal_argument("C-MOVE needs moveDestination (option or CamelDicomMoveDestination header)"))?;
                    let uid = self.cfg.query_model.move_uid().ok_or_else(|| CamelError::illegal_argument("C-MOVE is not defined for the worklist model"))?;
                    ("C-MOVE", scu::move_(&scu_cfg, uid, &ident, &dest)?, None)
                } else {
                    let uid = self.cfg.query_model.get_uid().ok_or_else(|| CamelError::illegal_argument("C-GET is not defined for the worklist model"))?;
                    let (files, ops) = scu::get(&scu_cfg, uid, &ident, &self.cfg.storage_sop_classes, &self.cfg.transfer_syntaxes)?;
                    ("C-GET", ops, Some(files))
                };
                ex.set_header(headers::COMPLETED, ops.completed as i64);
                ex.set_header(headers::FAILED, ops.failed as i64);
                ex.set_header(headers::WARNING, ops.warning as i64);
                check(name, ex, ops.status, ops.error_comment.clone())?;
                if let Some(f) = files {
                    ex.set_header(headers::RESULT_COUNT, f.len() as i64);
                    ex.set_body(Value::Array(f.into_iter().map(Value::Bytes).collect()));
                }
                Ok(())
            }
        }
    }
}

/// The bound port of a started `dicom:` consumer (useful with port 0).
pub fn local_port(ctx: &CamelContext, uri: &str) -> Option<u16> {
    let ep = ctx.get_endpoint(uri).ok()?;
    ep.as_any().downcast_ref::<DicomEndpoint>().and_then(|e| e.local_port())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        assert_eq!(parse_address("PACS@pacs.local:11112", "X").unwrap(), ("PACS".into(), "pacs.local".into(), 11112));
        assert_eq!(parse_address("//MYAE@0.0.0.0:0", "X").unwrap(), ("MYAE".into(), "0.0.0.0".into(), 0));
        assert_eq!(parse_address("host", "DEF").unwrap(), ("DEF".into(), "host".into(), 104));
        assert!(parse_address("WAY_TOO_LONG_AE_TITLE@h:1", "X").is_err());
        assert!(parse_address("A@h:notaport", "X").is_err());
    }
}

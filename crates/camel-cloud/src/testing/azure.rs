//! In-memory Azure Storage emulator (Azurite-style path addressing
//! `http://127.0.0.1:port/{account}/...`): Blob service (block, append and page blobs, leases),
//! Queue service, the ADLS Gen2 DFS endpoint (sharing the blob containers) and Azure Files on
//! four ports.
//! Requests must be authorized with a valid Shared Key signature (recomputed with the account
//! key) or a valid, unexpired account SAS token granting the needed service, resource type and
//! permission; otherwise `403 AuthenticationFailed` / `AuthorizationPermissionMismatch`.

#[path = "azure_blob_ext.rs"]
mod blob_ext;
#[path = "azure_files_ext.rs"]
mod files_ext;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;
use std::sync::{Arc, Mutex};

use super::{Handler, MockServer};
use crate::azure::{shared_key_signature, shared_key_string_to_sign, AccountSas};
use crate::http::{Response, ServerRequest};
use crate::util::{ct_eq, http_date, now_millis, now_secs, parse_http_date, parse_iso8601, percent_decode, random_hex, uuid, xml_escape, Xml};

/// Azurite's well-known development account.
pub const ACCOUNT: &str = "devstoreaccount1";
/// Azurite's well-known development account key.
pub const ACCOUNT_KEY: &str = "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==";

#[derive(Clone)]
struct Blob {
    data: Vec<u8>,
    content_type: String,
    etag: String,
    last_modified: i64,
    metadata: Vec<(String, String)>,
    blob_type: String,
    lease: Option<blob_ext::Lease>,
    pages: BTreeSet<usize>,
    committed_blocks: i64,
}

#[derive(Default)]
struct Container {
    created: i64,
    blobs: BTreeMap<String, Blob>,
}

#[derive(Clone)]
struct QMsg {
    id: String,
    text: String,
    insertion: i64,
    expiration: i64,
    visible_at: i64,
    pop_receipt: String,
    dequeue_count: i64,
}

#[derive(Default)]
struct State {
    containers: BTreeMap<String, Container>,
    dfs_pending: HashMap<(String, String), Vec<u8>>,
    queues: BTreeMap<String, Vec<QMsg>>,
    shares: BTreeMap<String, files_ext::Share>,
    auth_failures: usize,
    sas_requests: usize,
}

type Shared = Arc<Mutex<State>>;

/// The Azure Storage emulator.
pub struct AzuriteMock {
    blob: MockServer,
    queue: MockServer,
    dfs: MockServer,
    file: MockServer,
    state: Shared,
}

impl AzuriteMock {
    /// Start both services for the development account.
    pub fn start() -> io::Result<AzuriteMock> {
        let state: Shared = Arc::new(Mutex::new(State::default()));
        let (s1, s2, s3, s4) = (state.clone(), state.clone(), state.clone(), state.clone());
        let bh: Handler = Arc::new(move |r| handle(&s1, r, 'b'));
        let qh: Handler = Arc::new(move |r| handle(&s2, r, 'q'));
        let dh: Handler = Arc::new(move |r| handle(&s3, r, 'd'));
        let fh: Handler = Arc::new(move |r| handle(&s4, r, 'f'));
        Ok(AzuriteMock { blob: MockServer::start(bh)?, queue: MockServer::start(qh)?, dfs: MockServer::start(dh)?, file: MockServer::start(fh)?, state })
    }

    /// The Azure Files endpoint.
    pub fn file_url(&self) -> String {
        self.file.url()
    }
    /// Options for `azure-storage-files://devstoreaccount1/share?...` (Shared Key).
    pub fn files_options(&self) -> String {
        format!("accessKey=RAW({})&uriEndpointOverride={}&overrideEndpoint=true", ACCOUNT_KEY, self.file_url())
    }
    /// Create a file share.
    pub fn create_share(&self, name: &str) {
        self.state.lock().unwrap().shares.entry(name.into()).or_insert_with(|| files_ext::Share { created: now_secs(), ..Default::default() });
    }
    /// Put a file (parent directories are created).
    pub fn put_share_file(&self, share: &str, path: &str, data: &[u8]) {
        self.create_share(share);
        let mut s = self.state.lock().unwrap();
        let sh = s.shares.get_mut(share).unwrap();
        let mut acc = String::new();
        for part in path.split('/').collect::<Vec<_>>().split_last().map(|(_, dirs)| dirs.to_vec()).unwrap_or_default() {
            acc = if acc.is_empty() { part.to_string() } else { format!("{}/{}", acc, part) };
            sh.dirs.insert(acc.clone());
        }
        sh.files.insert(path.into(), files_ext::AzFile { data: data.to_vec(), content_type: "application/octet-stream".into(), etag: format!("\"0x{}\"", random_hex(8)), last_modified: now_secs() });
    }
    /// Content of a file of a share.
    pub fn share_file(&self, share: &str, path: &str) -> Option<Vec<u8>> {
        self.state.lock().unwrap().shares.get(share).and_then(|s| s.files.get(path)).map(|f| f.data.clone())
    }
    /// Paths of the files of a share.
    pub fn share_files(&self, share: &str) -> Vec<String> {
        self.state.lock().unwrap().shares.get(share).map(|s| s.files.keys().cloned().collect()).unwrap_or_default()
    }
    /// Directories of a share.
    pub fn share_dirs(&self, share: &str) -> Vec<String> {
        self.state.lock().unwrap().shares.get(share).map(|s| s.dirs.iter().cloned().collect()).unwrap_or_default()
    }

    pub fn blob_url(&self) -> String {
        self.blob.url()
    }
    pub fn queue_url(&self) -> String {
        self.queue.url()
    }
    /// The ADLS Gen2 (dfs) endpoint.
    pub fn dfs_url(&self) -> String {
        self.dfs.url()
    }
    /// Options for `azure-storage-datalake://devstoreaccount1/...` (Shared Key).
    pub fn datalake_options(&self) -> String {
        format!("accountKey=RAW({})&uriEndpointOverride={}&overrideEndpoint=true", ACCOUNT_KEY, self.dfs_url())
    }
    /// (blob type, lease state) of a blob.
    pub fn blob_state(&self, container: &str, name: &str) -> Option<(String, bool)> {
        self.state.lock().unwrap().containers.get(container).and_then(|c| c.blobs.get(name)).map(|b| (b.blob_type.clone(), b.lease.as_ref().is_some_and(|l| l.expires.is_none_or(|e| e > now_secs()))))
    }

    /// Options for `azure-storage-blob://devstoreaccount1/...` (Shared Key).
    pub fn blob_options(&self) -> String {
        format!("accessKey=RAW({})&uriEndpointOverride={}&overrideEndpoint=true", ACCOUNT_KEY, self.blob_url())
    }
    /// Options for `azure-storage-queue://devstoreaccount1/...` (Shared Key).
    pub fn queue_options(&self) -> String {
        format!("accessKey=RAW({})&uriEndpointOverride={}&overrideEndpoint=true", ACCOUNT_KEY, self.queue_url())
    }

    /// A full-access account SAS token valid for an hour.
    pub fn sas_token(&self) -> String {
        AccountSas::full_access(3600).token(ACCOUNT, ACCOUNT_KEY).expect("valid key")
    }

    pub fn auth_failures(&self) -> usize {
        self.state.lock().unwrap().auth_failures
    }
    /// Number of requests authorized with a SAS token.
    pub fn sas_requests(&self) -> usize {
        self.state.lock().unwrap().sas_requests
    }

    pub fn create_container(&self, name: &str) {
        self.state.lock().unwrap().containers.entry(name.into()).or_insert_with(|| Container { created: now_secs(), blobs: BTreeMap::new() });
    }
    pub fn container_exists(&self, name: &str) -> bool {
        self.state.lock().unwrap().containers.contains_key(name)
    }
    pub fn put_blob(&self, container: &str, name: &str, data: &[u8]) {
        self.create_container(container);
        self.state.lock().unwrap().containers.get_mut(container).unwrap().blobs.insert(name.into(), new_blob(data.to_vec(), "application/octet-stream".into(), vec![]));
    }
    pub fn blob(&self, container: &str, name: &str) -> Option<Vec<u8>> {
        self.state.lock().unwrap().containers.get(container).and_then(|c| c.blobs.get(name)).map(|b| b.data.clone())
    }
    /// Content type and metadata of a blob.
    pub fn blob_info(&self, container: &str, name: &str) -> Option<(String, Vec<(String, String)>)> {
        self.state.lock().unwrap().containers.get(container).and_then(|c| c.blobs.get(name)).map(|b| (b.content_type.clone(), b.metadata.clone()))
    }
    pub fn blob_names(&self, container: &str) -> Vec<String> {
        self.state.lock().unwrap().containers.get(container).map(|c| c.blobs.keys().cloned().collect()).unwrap_or_default()
    }
    pub fn create_queue(&self, name: &str) {
        self.state.lock().unwrap().queues.entry(name.into()).or_default();
    }
    pub fn queue_exists(&self, name: &str) -> bool {
        self.state.lock().unwrap().queues.contains_key(name)
    }
    /// Texts of all messages of a queue.
    pub fn queue_messages(&self, name: &str) -> Vec<String> {
        self.state.lock().unwrap().queues.get(name).map(|q| q.iter().map(|m| m.text.clone()).collect()).unwrap_or_default()
    }
    pub fn put_queue_message(&self, name: &str, text: &str) {
        self.state.lock().unwrap().queues.entry(name.into()).or_default().push(new_msg(text.into(), 0, 7 * 86400));
    }
}

fn new_blob(data: Vec<u8>, content_type: String, metadata: Vec<(String, String)>) -> Blob {
    Blob {
        data,
        content_type,
        etag: format!("\"0x{}\"", random_hex(8).to_uppercase()),
        last_modified: now_secs(),
        metadata,
        blob_type: "BlockBlob".into(),
        lease: None,
        pages: BTreeSet::new(),
        committed_blocks: 0,
    }
}

fn new_msg(text: String, visibility: i64, ttl: i64) -> QMsg {
    let now = now_secs();
    QMsg { id: uuid(), text, insertion: now, expiration: now + ttl, visible_at: now_millis() + visibility * 1000, pop_receipt: random_hex(8), dequeue_count: 0 }
}

fn error(status: u16, code: &str, message: &str) -> Response {
    Response::xml(status, format!("<?xml version=\"1.0\" encoding=\"utf-8\"?><Error><Code>{}</Code><Message>{}\nRequestId:{}</Message></Error>", code, xml_escape(message), uuid()))
        .with_header("x-ms-error-code", code)
        .with_header("x-ms-request-id", uuid())
}

/// (permission letters accepted, resource type) needed by a request on the given service.
fn sas_requirement(service: char, method: &str, segs: &[String], req: &ServerRequest) -> (&'static str, char) {
    let comp_list = req.query_param("comp").as_deref() == Some("list");
    if service == 'f' {
        return files_ext::sas_requirement(method, segs, req);
    }
    if service == 'b' {
        match (segs.len(), method) {
            (0, _) => ("l", 's'),
            (1, "GET") if comp_list => ("l", 'c'),
            (1, "PUT") => ("cw", 'c'),
            (1, "DELETE") => ("d", 'c'),
            (1, _) => ("r", 'c'),
            (_, "PUT") => ("cw", 'o'),
            (_, "DELETE") => ("d", 'o'),
            _ => ("r", 'o'),
        }
    } else {
        match (segs.len(), method) {
            (0, _) => ("l", 's'),
            (1, "PUT") => ("c", 'c'),
            (1, "DELETE") => ("d", 'c'),
            (1, _) => ("r", 'c'),
            (2, "POST") => ("a", 'o'),
            (2, "GET") if req.query_param("peekonly").as_deref() == Some("true") => ("rp", 'o'),
            (2, "GET") => ("p", 'o'),
            (2, "DELETE") => ("d", 'o'),
            (_, "PUT") => ("u", 'o'),
            _ => ("p", 'o'),
        }
    }
}

fn authorize(st: &Shared, req: &ServerRequest, service: char, segs: &[String]) -> Result<(), Response> {
    let params = req.query_params();
    if params.iter().any(|(k, _)| k == "sig") {
        let Some((sas, sig)) = AccountSas::parse(&params) else { return Err(error(403, "AuthenticationFailed", "Malformed SAS token")) };
        let expected = shared_key_signature(ACCOUNT_KEY, &sas.string_to_sign(ACCOUNT)).unwrap_or_default();
        if !ct_eq(expected.as_bytes(), sig.as_bytes()) {
            return Err(error(403, "AuthenticationFailed", "Signature did not match"));
        }
        if parse_iso8601(&sas.expiry).is_none_or(|e| e < now_secs()) {
            return Err(error(403, "AuthenticationFailed", "Signed expiry time has passed"));
        }
        let (perms, rt) = sas_requirement(service, &req.method, segs, req);
        if !sas.services.contains(service) || !sas.resource_types.contains(rt) || !perms.chars().any(|p| sas.permissions.contains(p)) {
            return Err(error(403, "AuthorizationPermissionMismatch", "This request is not authorized to perform this operation using this permission."));
        }
        st.lock().unwrap().sas_requests += 1;
        return Ok(());
    }
    let Some(auth) = req.header("Authorization") else {
        return Err(error(403, "NoAuthenticationInformation", "Server failed to authenticate the request. Please refer to the information in the www-authenticate header."));
    };
    let Some((account, sig)) = auth.strip_prefix("SharedKey ").and_then(|r| r.split_once(':')) else {
        return Err(error(403, "AuthenticationFailed", "Unsupported authorization scheme"));
    };
    if account != ACCOUNT {
        return Err(error(403, "AuthenticationFailed", "Unknown account"));
    }
    let date = req.header("x-ms-date").and_then(parse_http_date);
    if date.is_none_or(|d| (now_secs() - d).abs() > 15 * 60) {
        return Err(error(403, "AuthenticationFailed", "Request date header too old or missing"));
    }
    let sts = shared_key_string_to_sign(&req.method, &req.headers, ACCOUNT, &req.path, &req.query);
    let expected = shared_key_signature(ACCOUNT_KEY, &sts).unwrap_or_default();
    if !ct_eq(expected.as_bytes(), sig.as_bytes()) {
        return Err(error(
            403,
            "AuthenticationFailed",
            "Server failed to authenticate the request. Make sure the value of Authorization header is formed correctly including the signature.",
        ));
    }
    Ok(())
}

fn handle(st: &Shared, req: &ServerRequest, service: char) -> Response {
    let raw = req.path.trim_start_matches('/');
    let (account, rest) = raw.split_once('/').unwrap_or((raw, ""));
    if account != ACCOUNT {
        return error(400, "InvalidUri", "Unknown account in path (expected http://host:port/devstoreaccount1/...)");
    }
    let segs: Vec<String> = rest.split('/').filter(|s| !s.is_empty()).map(|s| percent_decode(s, false)).collect();
    if let Err(resp) = authorize(st, req, if service == 'd' { 'b' } else { service }, &segs) {
        st.lock().unwrap().auth_failures += 1;
        return resp;
    }
    if service == 'f' {
        return files_ext::file_op(st, req, rest);
    }
    if service == 'd' {
        let path = rest.split_once('/').map(|(_, b)| percent_decode(b, false)).filter(|b| !b.is_empty());
        return blob_ext::dfs_op(st, req, segs.first().cloned(), path);
    }
    if service == 'b' {
        // the blob name may contain '/': take the raw remainder after the container
        let blob = rest.split_once('/').map(|(_, b)| percent_decode(b, false)).filter(|b| !b.is_empty());
        blob_op(st, req, segs.first().cloned(), blob)
    } else {
        queue_op(st, req, &segs)
    }
}

fn blob_op(st: &Shared, req: &ServerRequest, container: Option<String>, blob: Option<String>) -> Response {
    let mut s = st.lock().unwrap();
    let Some(container) = container else {
        if req.query_param("comp").as_deref() != Some("list") {
            return error(400, "InvalidQueryParameterValue", "Unsupported service operation");
        }
        let mut xml = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?><EnumerationResults><Containers>");
        for (n, c) in &s.containers {
            xml.push_str(&format!("<Container><Name>{}</Name><Properties><Last-Modified>{}</Last-Modified><Etag>\"0x1\"</Etag></Properties></Container>", xml_escape(n), http_date(c.created)));
        }
        xml.push_str("</Containers><NextMarker /></EnumerationResults>");
        return Response::xml(200, xml);
    };
    let Some(blob) = blob else {
        let restype = req.query_param("restype");
        return match req.method.as_str() {
            "PUT" if restype.as_deref() == Some("container") => {
                if s.containers.contains_key(&container) {
                    return error(409, "ContainerAlreadyExists", "The specified container already exists.");
                }
                s.containers.insert(container, Container { created: now_secs(), blobs: BTreeMap::new() });
                Response::new(201).with_header("ETag", "\"0x1\"")
            }
            "DELETE" if restype.as_deref() == Some("container") => match s.containers.remove(&container) {
                Some(_) => Response::new(202),
                None => error(404, "ContainerNotFound", "The specified container does not exist."),
            },
            "GET" if req.query_param("comp").as_deref() == Some("list") => {
                let Some(c) = s.containers.get(&container) else { return error(404, "ContainerNotFound", "The specified container does not exist.") };
                let prefix = req.query_param("prefix").unwrap_or_default();
                let max: usize = req.query_param("maxresults").and_then(|m| m.parse().ok()).unwrap_or(5000);
                let marker = req.query_param("marker").unwrap_or_default();
                let mut xml = format!("<?xml version=\"1.0\" encoding=\"utf-8\"?><EnumerationResults ContainerName=\"{}\"><Prefix>{}</Prefix><Blobs>", xml_escape(&container), xml_escape(&prefix));
                let mut next = String::new();
                let mut count = 0;
                for (n, b) in c.blobs.range(marker.clone()..).filter(|(n, _)| n.starts_with(&prefix)) {
                    if count == max {
                        next = n.clone();
                        break;
                    }
                    count += 1;
                    xml.push_str(&format!(
                        "<Blob><Name>{}</Name><Properties><Last-Modified>{}</Last-Modified><Etag>{}</Etag><Content-Length>{}</Content-Length><Content-Type>{}</Content-Type><BlobType>BlockBlob</BlobType></Properties></Blob>",
                        xml_escape(n),
                        http_date(b.last_modified),
                        xml_escape(&b.etag),
                        b.data.len(),
                        xml_escape(&b.content_type)
                    ));
                }
                xml.push_str(&format!("</Blobs><NextMarker>{}</NextMarker></EnumerationResults>", xml_escape(&next)));
                Response::xml(200, xml)
            }
            _ => error(400, "InvalidQueryParameterValue", "Unsupported container operation"),
        };
    };
    let Some(c) = s.containers.get_mut(&container) else { return error(404, "ContainerNotFound", "The specified container does not exist.") };
    blob_ext::blob_item_op(c, req, blob)
}

fn message_xml(m: &QMsg, with_text: bool) -> String {
    let mut x = format!(
        "<QueueMessage><MessageId>{}</MessageId><InsertionTime>{}</InsertionTime><ExpirationTime>{}</ExpirationTime><PopReceipt>{}</PopReceipt><TimeNextVisible>{}</TimeNextVisible>",
        m.id,
        http_date(m.insertion),
        http_date(m.expiration),
        m.pop_receipt,
        http_date(m.visible_at / 1000)
    );
    if with_text {
        x.push_str(&format!("<DequeueCount>{}</DequeueCount><MessageText>{}</MessageText>", m.dequeue_count, xml_escape(&m.text)));
    }
    x.push_str("</QueueMessage>");
    x
}

fn queue_op(st: &Shared, req: &ServerRequest, segs: &[String]) -> Response {
    let mut s = st.lock().unwrap();
    let int = |k: &str, d: i64| req.query_param(k).and_then(|v| v.parse::<i64>().ok()).unwrap_or(d);
    match (segs.len(), req.method.as_str()) {
        (0, "GET") => {
            let mut xml = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?><EnumerationResults><Queues>");
            for n in s.queues.keys() {
                xml.push_str(&format!("<Queue><Name>{}</Name></Queue>", xml_escape(n)));
            }
            xml.push_str("</Queues><NextMarker /></EnumerationResults>");
            Response::xml(200, xml)
        }
        (1, "PUT") => {
            if s.queues.contains_key(&segs[0]) {
                return Response::new(204);
            }
            s.queues.insert(segs[0].clone(), Vec::new());
            Response::new(201)
        }
        (1, "DELETE") => match s.queues.remove(&segs[0]) {
            Some(_) => Response::new(204),
            None => error(404, "QueueNotFound", "The specified queue does not exist."),
        },
        (n, method) if n >= 2 && segs[1] == "messages" => {
            let Some(q) = s.queues.get_mut(&segs[0]) else { return error(404, "QueueNotFound", "The specified queue does not exist.") };
            let now = now_millis();
            q.retain(|m| m.expiration * 1000 > now);
            match (n, method) {
                (2, "POST") => {
                    let text = Xml::parse(&req.text()).ok().and_then(|x| x.find_text("MessageText"));
                    let Some(text) = text else { return error(400, "InvalidXmlDocument", "XML specified is not syntactically valid.") };
                    let m = new_msg(text, int("visibilitytimeout", 0), int("messagettl", 7 * 86400));
                    let x = format!("<?xml version=\"1.0\" encoding=\"utf-8\"?><QueueMessagesList>{}</QueueMessagesList>", message_xml(&m, false));
                    q.push(m);
                    Response::xml(201, x)
                }
                (2, "GET") => {
                    let max = int("numofmessages", 1).clamp(1, 32) as usize;
                    let peek = req.query_param("peekonly").as_deref() == Some("true");
                    let vis = int("visibilitytimeout", 30);
                    let mut x = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?><QueueMessagesList>");
                    for m in q.iter_mut().filter(|m| m.visible_at <= now).take(max) {
                        if !peek {
                            m.dequeue_count += 1;
                            m.pop_receipt = random_hex(8);
                            m.visible_at = now + vis * 1000;
                        }
                        x.push_str(&message_xml(m, true));
                    }
                    x.push_str("</QueueMessagesList>");
                    Response::xml(200, x)
                }
                (2, "DELETE") => {
                    q.clear();
                    Response::new(204)
                }
                (3, "DELETE") | (3, "PUT") => {
                    let pr = req.query_param("popreceipt").unwrap_or_default();
                    let Some(pos) = q.iter().position(|m| m.id == segs[2]) else { return error(404, "MessageNotFound", "The specified message does not exist.") };
                    if q[pos].pop_receipt != pr {
                        return error(400, "PopReceiptMismatch", "The specified pop receipt did not match the pop receipt for a dequeued message.");
                    }
                    if method == "DELETE" {
                        q.remove(pos);
                        return Response::new(204);
                    }
                    if let Some(t) = Xml::parse(&req.text()).ok().and_then(|x| x.find_text("MessageText")) {
                        q[pos].text = t;
                    }
                    q[pos].pop_receipt = random_hex(8);
                    q[pos].visible_at = now + int("visibilitytimeout", 0) * 1000;
                    Response::new(204).with_header("x-ms-popreceipt", q[pos].pop_receipt.clone()).with_header("x-ms-time-next-visible", http_date(q[pos].visible_at / 1000))
                }
                _ => error(405, "UnsupportedHttpVerb", "The resource doesn't support the specified HTTP verb."),
            }
        }
        _ => error(400, "InvalidUri", "Unsupported queue operation"),
    }
}

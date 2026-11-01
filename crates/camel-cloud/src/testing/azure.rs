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
#[path = "azure_changefeed.rs"]
mod changefeed;
#[path = "azure_query.rs"]
mod query;

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
    /// committed block list (id, size) of block blobs built with Put Block List
    block_list: Vec<(String, usize)>,
}

#[derive(Default)]
struct Container {
    created: i64,
    blobs: BTreeMap<String, Blob>,
    /// blob -> staged (uncommitted) blocks (id, data)
    staged: BTreeMap<String, Vec<(String, Vec<u8>)>>,
    /// ADLS path -> (owner, group, permission bits, extra ACL entries)
    acls: BTreeMap<String, (String, String, u32, Vec<String>)>,
    /// (blob, snapshot time) -> read-only copy (Snapshot Blob)
    snapshots: BTreeMap<(String, String), Blob>,
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
    /// queue -> metadata
    queue_meta: BTreeMap<String, Vec<(String, String)>>,
    shares: BTreeMap<String, files_ext::Share>,
    auth_failures: usize,
    sas_requests: usize,
    bearer_requests: usize,
    change_feed: bool,
    feed_events: Vec<(i64, camel_core::value::Value)>,
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

    /// Number of requests authorized with an Entra ID bearer token.
    pub fn bearer_requests(&self) -> usize {
        self.state.lock().unwrap().bearer_requests
    }

    pub fn auth_failures(&self) -> usize {
        self.state.lock().unwrap().auth_failures
    }
    /// Number of requests authorized with a SAS token.
    pub fn sas_requests(&self) -> usize {
        self.state.lock().unwrap().sas_requests
    }

    pub fn create_container(&self, name: &str) {
        self.state.lock().unwrap().containers.entry(name.into()).or_insert_with(|| Container { created: now_secs(), ..Default::default() });
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
        block_list: Vec::new(),
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

/// Verify a blob service SAS (`sr=b`) for `/blob/{account}/{container}/{blob}`.
fn verify_blob_service_sas(params: &[(String, String)], container: &str, blob: &str, perms_needed: &str) -> Result<(), Response> {
    let get = |k: &str| params.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap_or_default();
    // blob (sr=b) or container (sr=c) service SAS
    let resource = match get("sr").as_str() {
        "b" => format!("/blob/{}/{}/{}", ACCOUNT, container, blob),
        "c" => format!("/blob/{}/{}", ACCOUNT, container),
        _ => return Err(error(403, "AuthenticationFailed", "Only blob (sr=b) and container (sr=c) service SAS are supported")),
    };
    let sts = [get("sp"), get("st"), get("se"), resource, get("si"), get("sip"), get("spr"), get("sv"), get("sr"), String::new(), get("ses"), get("rscc"), get("rscd"), get("rsce"), get("rscl"), get("rsct")].join("\n");
    let expected = shared_key_signature(ACCOUNT_KEY, &sts).unwrap_or_default();
    if !ct_eq(expected.as_bytes(), get("sig").as_bytes()) {
        return Err(error(403, "AuthenticationFailed", "Signature did not match"));
    }
    if parse_iso8601(&get("se")).is_none_or(|e| e < now_secs()) {
        return Err(error(403, "AuthenticationFailed", "Signed expiry time has passed"));
    }
    if !perms_needed.chars().any(|p| get("sp").contains(p)) {
        return Err(error(403, "AuthorizationPermissionMismatch", "This request is not authorized to perform this operation using this permission."));
    }
    Ok(())
}

/// Verify a queue service SAS for `/queue/{account}/{queue}` (no `sr`; version 2015-04-05+).
fn verify_queue_service_sas(params: &[(String, String)], queue: &str, perms_needed: &str) -> Result<(), Response> {
    let get = |k: &str| params.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap_or_default();
    let resource = format!("/queue/{}/{}", ACCOUNT, queue);
    let sts = [get("sp"), get("st"), get("se"), resource, get("si"), get("sip"), get("spr"), get("sv")].join("\n");
    let expected = shared_key_signature(ACCOUNT_KEY, &sts).unwrap_or_default();
    if !ct_eq(expected.as_bytes(), get("sig").as_bytes()) {
        return Err(error(403, "AuthenticationFailed", "Signature did not match"));
    }
    if parse_iso8601(&get("se")).is_none_or(|e| e < now_secs()) {
        return Err(error(403, "AuthenticationFailed", "Signed expiry time has passed"));
    }
    if !perms_needed.chars().any(|p| get("sp").contains(p)) {
        return Err(error(403, "AuthorizationPermissionMismatch", "This request is not authorized to perform this operation using this permission."));
    }
    Ok(())
}

fn authorize(st: &Shared, req: &ServerRequest, service: char, segs: &[String]) -> Result<(), Response> {
    let params = req.query_params();
    if service == 'q' && params.iter().any(|(k, _)| k == "sig") && !params.iter().any(|(k, _)| k == "ss") {
        let (perms, _) = sas_requirement(service, &req.method, segs, req);
        verify_queue_service_sas(&params, segs.first().map(|s| s.as_str()).unwrap_or(""), perms)?;
        st.lock().unwrap().sas_requests += 1;
        return Ok(());
    }
    if params.iter().any(|(k, _)| k == "sig") && params.iter().any(|(k, _)| k == "sr") && !params.iter().any(|(k, _)| k == "ss") {
        // service SAS of one blob
        let (perms, _) = sas_requirement(service, &req.method, segs, req);
        let container = segs.first().cloned().unwrap_or_default();
        let blob = segs.get(1..).map(|b| b.join("/")).unwrap_or_default();
        verify_blob_service_sas(&params, &container, &blob, perms)?;
        st.lock().unwrap().sas_requests += 1;
        return Ok(());
    }
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
    if let Some(tok) = auth.strip_prefix("Bearer ") {
        // Microsoft Entra ID tokens issued by the AzureMock token / managed identity endpoints
        if service == 'f' && req.header("x-ms-file-request-intent").is_none() {
            return Err(error(400, "MissingRequiredHeader", "x-ms-file-request-intent is required with OAuth authentication."));
        }
        return match super::verify_entra_token(tok, crate::azure::STORAGE_SCOPE) {
            Some(_) => {
                st.lock().unwrap().bearer_requests += 1;
                Ok(())
            }
            None => Err(error(401, "InvalidAuthenticationInfo", "Server failed to authenticate the request. Please refer to the information in the www-authenticate header.")),
        };
    }
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
    if service == 'b' && req.method == "POST" && req.query_param("comp").as_deref() == Some("batch") {
        return blob_batch(st, req);
    }
    if service == 'f' {
        return files_ext::file_op(st, req, rest);
    }
    // the DFS endpoint also serves Blob API calls on the same data (multi-protocol access):
    // the ADLS SDKs send container create/delete and ranged reads there in Blob API form
    let blob_api = req.query_param("restype").is_some() || req.query_param("comp").is_some() || (req.method == "GET" && req.query_param("action").is_none() && req.query_param("resource").is_none() && rest.contains('/'));
    if service == 'd' && blob_api {
        let blob = rest.split_once('/').map(|(_, b)| percent_decode(b, false)).filter(|b| !b.is_empty());
        return blob_op(st, req, segs.first().cloned(), blob);
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

/// Blob Batch (`POST ?comp=batch`, multipart/mixed): each part is an HTTP request (Delete Blob
/// or Set Blob Tier) with its own Shared Key / SAS authorization, answered in a multipart/mixed
/// response with one embedded HTTP response per part.
fn blob_batch(st: &Shared, req: &ServerRequest) -> Response {
    let ct = req.header("Content-Type").unwrap_or("");
    let Some(boundary) = ct.split(';').find_map(|p| p.trim().strip_prefix("boundary=")).map(|b| b.trim_matches('"').to_string()) else {
        return error(400, "InvalidInput", "The batch request is missing the multipart boundary.");
    };
    let text = String::from_utf8_lossy(&req.body).into_owned();
    let delim = format!("--{}", boundary);
    let mut parts = Vec::new();
    for chunk in text.split(delim.as_str()).skip(1) {
        if chunk.starts_with("--") {
            break;
        }
        // part headers, blank line, embedded request
        let Some((_, http)) = chunk.split_once("\r\n\r\n") else { continue };
        let (head, body) = http.split_once("\r\n\r\n").unwrap_or((http, ""));
        let mut lines = head.lines();
        let Some(request_line) = lines.next() else { continue };
        let mut rl = request_line.split_whitespace();
        let (method, target) = (rl.next().unwrap_or("").to_string(), rl.next().unwrap_or("").to_string());
        let headers: Vec<(String, String)> = lines.filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))).collect();
        let (path, query) = match target.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (target.clone(), String::new()),
        };
        // sub-request paths are relative to the account endpoint (no account segment)
        let path = if path.starts_with(&format!("/{}/", ACCOUNT)) { path } else { format!("/{}{}", ACCOUNT, path) };
        parts.push(ServerRequest { method, path, query, headers, body: body.trim_end_matches("\r\n").as_bytes().to_vec() });
    }
    if parts.is_empty() || parts.len() > 256 {
        return error(400, "InvalidInput", "One of the request inputs is not valid.");
    }
    let rb = format!("batchresponse_{}", uuid());
    let mut out = String::new();
    for (i, p) in parts.iter().enumerate() {
        // the SDKs sign sub-requests over the path they send (relative to the account
        // endpoint); verify that form, then run the request on the account-qualified path
        let relative = ServerRequest { path: p.path.trim_start_matches(&format!("/{}", ACCOUNT)).to_string(), ..p.clone() };
        let segs: Vec<String> = p.path.trim_start_matches('/').split('/').skip(1).filter(|s| !s.is_empty()).map(|s| percent_decode(s, false)).collect();
        let r = if authorize(st, &relative, 'b', &segs).is_ok() {
            let rest = p.path.trim_start_matches('/').split_once('/').map(|(_, r)| r.to_string()).unwrap_or_default();
            let blob = rest.split_once('/').map(|(_, b)| percent_decode(b, false)).filter(|b| !b.is_empty());
            blob_op(st, p, segs.first().cloned(), blob)
        } else {
            handle(st, p, 'b')
        };
        out.push_str(&format!("--{}\r\nContent-Type: application/http\r\nContent-ID: {}\r\n\r\nHTTP/1.1 {} {}\r\n", rb, i, r.status, if r.status < 300 { "Accepted" } else { "Error" }));
        for (k, v) in &r.headers {
            out.push_str(&format!("{}: {}\r\n", k, v));
        }
        out.push_str(&format!("Content-Length: {}\r\n\r\n", r.body.len()));
        out.push_str(&String::from_utf8_lossy(&r.body));
        if !r.body.is_empty() {
            out.push_str("\r\n");
        }
    }
    out.push_str(&format!("--{}--\r\n", rb));
    Response::new(202).with_header("Content-Type", format!("multipart/mixed; boundary={}", rb)).with_body(out.into_bytes())
}

fn blob_op(st: &Shared, req: &ServerRequest, container: Option<String>, blob: Option<String>) -> Response {
    let mut s = st.lock().unwrap();
    let Some(container) = container else {
        if req.query_param("comp").as_deref() != Some("list") {
            return error(400, "InvalidQueryParameterValue", "Unsupported service operation");
        }
        let prefix = req.query_param("prefix").unwrap_or_default();
        let mut xml = format!("<?xml version=\"1.0\" encoding=\"utf-8\"?><EnumerationResults><Prefix>{}</Prefix><Containers>", xml_escape(&prefix));
        for (n, c) in s.containers.iter().filter(|(n, _)| n.starts_with(&prefix)) {
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
                s.containers.insert(container, Container { created: now_secs(), ..Default::default() });
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
                let delimiter = req.query_param("delimiter").filter(|d| !d.is_empty());
                let mut xml = format!("<?xml version=\"1.0\" encoding=\"utf-8\"?><EnumerationResults ContainerName=\"{}\"><Prefix>{}</Prefix>", xml_escape(&container), xml_escape(&prefix));
                if let Some(d) = &delimiter {
                    xml.push_str(&format!("<Delimiter>{}</Delimiter>", xml_escape(d)));
                }
                xml.push_str("<Blobs>");
                let mut next = String::new();
                let mut count = 0;
                let mut last_prefix = String::new();
                for (n, b) in c.blobs.range(marker.clone()..).filter(|(n, _)| n.starts_with(&prefix)) {
                    // with a delimiter, names continuing past it are rolled up into one BlobPrefix
                    let rolled = delimiter.as_ref().and_then(|d| n[prefix.len()..].find(d.as_str()).map(|i| n[..prefix.len() + i + d.len()].to_string()));
                    if let Some(p) = &rolled {
                        if *p == last_prefix {
                            continue;
                        }
                    }
                    if count == max {
                        next = n.clone();
                        break;
                    }
                    count += 1;
                    if let Some(p) = rolled {
                        xml.push_str(&format!("<BlobPrefix><Name>{}</Name></BlobPrefix>", xml_escape(&p)));
                        last_prefix = p;
                        continue;
                    }
                    let meta: String = if req.query_param("include").is_some_and(|i| i.split(',').any(|x| x == "metadata")) {
                        format!("<Metadata>{}</Metadata>", b.metadata.iter().map(|(k, v)| format!("<{0}>{1}</{0}>", k, xml_escape(v))).collect::<String>())
                    } else {
                        String::new()
                    };
                    xml.push_str(&format!(
                        "<Blob><Name>{}</Name><Properties><Last-Modified>{}</Last-Modified><Etag>{}</Etag><Content-Length>{}</Content-Length><Content-Type>{}</Content-Type><BlobType>{}</BlobType></Properties>{}</Blob>",
                        xml_escape(n),
                        http_date(b.last_modified),
                        xml_escape(&b.etag),
                        b.data.len(),
                        xml_escape(&b.content_type),
                        if b.blob_type.is_empty() { "BlockBlob" } else { b.blob_type.as_str() },
                        meta
                    ));
                }
                xml.push_str(&format!("</Blobs><NextMarker>{}</NextMarker></EnumerationResults>", xml_escape(&next)));
                Response::xml(200, xml)
            }
            _ => error(400, "InvalidQueryParameterValue", "Unsupported container operation"),
        };
    };
    if req.method == "PUT" && req.query_param("comp").is_none() {
        if let Some(src) = req.header("x-ms-copy-source") {
            return copy_blob(&mut s, &container, &blob, src).unwrap_or_else(|r| r);
        }
    }
    let Some(c) = s.containers.get_mut(&container) else { return error(404, "ContainerNotFound", "The specified container does not exist.") };
    if let Some(resp) = blob_ext::block_list_op(c, req, &blob) {
        return resp;
    }
    let before = c.blobs.get(&blob).map(|b| b.etag.clone());
    let resp = blob_ext::blob_item_op(c, req, blob.clone());
    let after = c.blobs.get(&blob).map(|b| (b.etag.clone(), b.content_type.clone(), b.data.len(), b.blob_type.clone()));
    if before != after.as_ref().map(|a| a.0.clone()) {
        let api = match (&after, req.query_param("comp").as_deref()) {
            (None, _) => "DeleteBlob",
            (Some(_), Some("appendblock")) => "AppendBlock",
            (Some(_), Some("page")) => "PutPage",
            (Some(_), Some("blocklist")) => "PutBlockList",
            _ => "PutBlob",
        };
        let (etag, ct, len, bt) = match after {
            Some((e, c, l, t)) => (Some(e), c, l, t),
            None => (None, String::new(), 0, "BlockBlob".to_string()),
        };
        changefeed::record(&mut s, &container, &blob, api, etag, &ct, len, &bt);
    }
    resp
}

/// Copy Blob (synchronous here): the source is a blob of this account (path-style URL),
/// authorized by a service / account SAS in its query or by the request itself.
fn copy_blob(s: &mut State, container: &str, blob: &str, src: &str) -> Result<Response, Response> {
    let bad = || error(400, "InvalidHeaderValue", "The value for the x-ms-copy-source header is invalid.");
    let url = crate::http::Url::parse(src).map_err(|_| bad())?;
    let path = url.path.trim_start_matches('/');
    let (account, rest) = path.split_once('/').ok_or_else(bad)?;
    if account != ACCOUNT {
        return Err(error(403, "CannotVerifyCopySource", "The source account is unknown to this emulator."));
    }
    let (src_container, src_blob) = rest.split_once('/').map(|(c, b)| (percent_decode(c, false), percent_decode(b, false))).ok_or_else(bad)?;
    let query = url.query.clone().unwrap_or_default();
    let params: Vec<(String, String)> = query.split('&').filter(|p| !p.is_empty()).map(|p| p.split_once('=').map(|(k, v)| (percent_decode(k, true), percent_decode(v, true))).unwrap_or((p.to_string(), String::new()))).collect();
    if params.iter().any(|(k, _)| k == "sr") {
        verify_blob_service_sas(&params, &src_container, &src_blob, "r").map_err(|_| error(403, "CannotVerifyCopySource", "Server failed to authenticate the copy source."))?;
    }
    let Some(source) = s.containers.get(&src_container).and_then(|c| c.blobs.get(&src_blob)).cloned() else { return Err(error(404, "CannotVerifyCopySource", "The specified blob does not exist.")) };
    let Some(c) = s.containers.get_mut(container) else { return Err(error(404, "ContainerNotFound", "The specified container does not exist.")) };
    let mut b = new_blob(source.data.clone(), source.content_type.clone(), source.metadata.clone());
    b.blob_type = source.blob_type.clone();
    c.blobs.insert(blob.to_string(), b);
    Ok(Response::new(202).with_header("x-ms-copy-id", uuid()).with_header("x-ms-copy-status", "success").with_header("ETag", c.blobs[blob].etag.clone()).with_header("Last-Modified", http_date(now_secs())))
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
            let prefix = req.query_param("prefix").unwrap_or_default();
            let with_meta = req.query_param("include").is_some_and(|i| i.split(',').any(|x| x == "metadata"));
            let mut xml = format!("<?xml version=\"1.0\" encoding=\"utf-8\"?><EnumerationResults ServiceEndpoint=\"{}\"><Prefix>{}</Prefix><Queues>", xml_escape(ACCOUNT), xml_escape(&prefix));
            for n in s.queues.keys().filter(|n| n.starts_with(&prefix)) {
                let meta = if with_meta {
                    format!("<Metadata>{}</Metadata>", s.queue_meta.get(n).map(|m| m.iter().map(|(k, v)| format!("<{0}>{1}</{0}>", k, xml_escape(v))).collect::<String>()).unwrap_or_default())
                } else {
                    String::new()
                };
                xml.push_str(&format!("<Queue><Name>{}</Name>{}</Queue>", xml_escape(n), meta));
            }
            xml.push_str("</Queues><NextMarker /></EnumerationResults>");
            Response::xml(200, xml)
        }
        (1, "PUT") if req.query_param("comp").as_deref() == Some("metadata") => {
            if !s.queues.contains_key(&segs[0]) {
                return error(404, "QueueNotFound", "The specified queue does not exist.");
            }
            s.queue_meta.insert(segs[0].clone(), blob_ext::meta_headers(req));
            Response::new(204)
        }
        (1, "GET" | "HEAD") if req.query_param("comp").as_deref() == Some("metadata") => {
            let Some(q) = s.queues.get(&segs[0]) else { return error(404, "QueueNotFound", "The specified queue does not exist.") };
            let mut r = Response::new(200).with_header("x-ms-approximate-messages-count", q.len().to_string());
            for (k, v) in s.queue_meta.get(&segs[0]).cloned().unwrap_or_default() {
                r = r.with_header(&format!("x-ms-meta-{}", k), v);
            }
            r
        }
        (1, "PUT") => {
            let meta = blob_ext::meta_headers(req);
            if s.queues.contains_key(&segs[0]) {
                // re-creating with the same metadata is a no-op, with different metadata a conflict
                let same = s.queue_meta.get(&segs[0]).cloned().unwrap_or_default() == meta;
                return if same { Response::new(204) } else { error(409, "QueueAlreadyExists", "The specified queue already exists.") };
            }
            s.queues.insert(segs[0].clone(), Vec::new());
            s.queue_meta.insert(segs[0].clone(), meta);
            Response::new(201)
        }
        (1, "DELETE") => match s.queues.remove(&segs[0]) {
            Some(_) => {
                s.queue_meta.remove(&segs[0]);
                Response::new(204)
            }
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

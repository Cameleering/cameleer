//! In-memory Google Cloud emulator: an OAuth2 token endpoint (`POST /token`, JWT bearer
//! grant), Cloud Storage JSON API (`/storage/v1`, `/upload/storage/v1`), Pub/Sub REST
//! (`/v1/projects/...`), and BigQuery, Secret Manager, Cloud Functions, Firestore and Sheets
//! (see the `gcp_ext` part: [`CloudFn`]), Gmail, Calendar, Drive, Vertex AI, Speech-to-Text,
//! Text-to-Speech and Storage V4 signed URLs (the `gcp_more` part) on one port, plus the
//! Pub/Sub StreamingPull gRPC service on a second port ([`GcpMock::pubsub_grpc_address`],
//! included in [`GcpMock::endpoint_options`] as `pubsubEndpoint`).
//!
//! The token endpoint verifies the RS256 signature of the JWT assertion with the registered
//! service account public key and checks `iss`, `aud` (= the token URL), `iat`/`exp` and
//! `scope`. Storage and Pub/Sub calls require a bearer token issued by it (unexpired), unless
//! authentication is disabled with [`GcpMock::set_require_auth`].

#[path = "gcp_ext.rs"]
mod ext;
pub use ext::CloudFn;
#[path = "gcp_more.rs"]
mod more;
#[path = "gcp_lite.rs"]
mod lite;
pub use more::StreamingStats;

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use camel_core::value::{ObjectMap, Value};
use camel_tls::PublicKey;

use super::{Handler, MockServer, TEST_RSA_KEY_PEM};
use crate::gcp::{verify_jwt, ServiceAccountKey};
use crate::http::{Response, ServerRequest};
use crate::util::{base64_decode, base64_encode, iso8601_millis, md5, now_millis, now_secs, obj, parse_form, parse_json, random_hex};

/// Project of the generated test service account.
pub const PROJECT: &str = "test-project";
/// Email of the generated test service account.
pub const CLIENT_EMAIL: &str = "camel@test-project.iam.gserviceaccount.com";

#[derive(Clone)]
struct GcsObj {
    data: Vec<u8>,
    content_type: String,
    generation: i64,
    created: i64,
    updated: i64,
}

#[derive(Default)]
struct GcsBucket {
    created: i64,
    objects: BTreeMap<String, GcsObj>,
}

#[derive(Clone)]
struct PsMsg {
    id: String,
    data: String,
    attributes: Value,
    ordering_key: String,
    publish_time: i64,
    ack_id: Option<String>,
    visible_at: i64,
    attempts: i64,
}

struct Subscription {
    topic: String,
    ack_deadline: i64,
    messages: Vec<PsMsg>,
    /// (dead-letter topic, max delivery attempts)
    dead_letter: Option<(String, i64)>,
    exactly_once: bool,
}

struct State {
    keys: HashMap<String, PublicKey>,
    tokens: HashMap<String, i64>,
    require_auth: bool,
    token_requests: usize,
    auth_failures: usize,
    buckets: BTreeMap<String, GcsBucket>,
    topics: BTreeMap<String, Vec<String>>,
    subscriptions: BTreeMap<String, Subscription>,
    next_id: i64,
    /// upload id -> (bucket, object, content type, declared size, received bytes)
    resumable: HashMap<String, (String, String, String, Option<usize>, Vec<u8>)>,
    /// fail the next resumable chunk after storing half of it (resume tests)
    fail_next_chunk: bool,
    resumable_chunks: usize,
    /// accepted (audience, subject token) pairs of workload identity providers
    wif_providers: Vec<(String, String)>,
    sts_requests: usize,
    impersonations: Vec<String>,
    metadata_requests: usize,
    ext: ext::ExtState,
    more: more::MoreState,
    lite: lite::LiteState,
}

type Shared = Arc<Mutex<State>>;

/// The Google Cloud emulator.
pub struct GcpMock {
    server: MockServer,
    state: Shared,
    grpc: camel_grpc::grpc::server::GrpcServer,
}

impl GcpMock {
    /// Start; the generated test service account ([`GcpMock::service_account_json`]) is trusted.
    pub fn start() -> io::Result<GcpMock> {
        let state: Shared = Arc::new(Mutex::new(State {
            keys: HashMap::new(),
            tokens: HashMap::new(),
            require_auth: true,
            token_requests: 0,
            auth_failures: 0,
            buckets: BTreeMap::new(),
            topics: BTreeMap::new(),
            subscriptions: BTreeMap::new(),
            next_id: 1,
            resumable: HashMap::new(),
            fail_next_chunk: false,
            resumable_chunks: 0,
            wif_providers: Vec::new(),
            sts_requests: 0,
            impersonations: Vec::new(),
            metadata_requests: 0,
            ext: Default::default(),
            more: Default::default(),
            lite: Default::default(),
        }));
        let st = state.clone();
        let handler: Handler = Arc::new(move |r| handle(&st, r));
        let grpc = more::start_grpc(state.clone())?;
        let mock = GcpMock { server: MockServer::start(handler)?, state, grpc };
        let key = ServiceAccountKey::parse(&mock.service_account_json()).map_err(|e| io::Error::other(e.to_string()))?;
        mock.trust_key(CLIENT_EMAIL, key.public_key());
        Ok(mock)
    }

    pub fn url(&self) -> String {
        self.server.url()
    }

    /// The token endpoint URL.
    pub fn token_uri(&self) -> String {
        format!("{}/token", self.url())
    }

    /// A service account key JSON (test RSA key) whose `token_uri` is this emulator.
    pub fn service_account_json(&self) -> String {
        service_account_json(&self.token_uri(), CLIENT_EMAIL)
    }

    /// Write the service account key to a unique temp file and return its path.
    pub fn write_service_account_file(&self) -> io::Result<PathBuf> {
        let p = std::env::temp_dir().join(format!("camel-cloud-sa-{}.json", random_hex(8)));
        std::fs::write(&p, self.service_account_json())?;
        Ok(p)
    }

    /// Endpoint options: `serviceAccountKey=file:...&uriEndpointOverride=...&overrideEndpoint=true&pubsubEndpoint=...`.
    pub fn endpoint_options(&self, key_file: &std::path::Path) -> String {
        format!("serviceAccountKey=file:{}&uriEndpointOverride={}&overrideEndpoint=true&pubsubEndpoint={}", key_file.display(), self.url(), self.pubsub_grpc_address())
    }

    /// Trust another service account's public key.
    pub fn trust_key(&self, client_email: &str, key: PublicKey) {
        self.state.lock().unwrap().keys.insert(client_email.into(), key);
    }

    /// Require (default) or not bearer tokens on API calls.
    pub fn set_require_auth(&self, yes: bool) {
        self.state.lock().unwrap().require_auth = yes;
    }

    /// Number of successful token exchanges.
    pub fn token_requests(&self) -> usize {
        self.state.lock().unwrap().token_requests
    }

    pub fn auth_failures(&self) -> usize {
        self.state.lock().unwrap().auth_failures
    }

    /// Make the next resumable upload chunk fail with 503 after storing half of it.
    pub fn fail_next_resumable_chunk(&self) {
        self.state.lock().unwrap().fail_next_chunk = true;
    }
    /// Number of resumable upload chunks received.
    pub fn resumable_chunks(&self) -> usize {
        self.state.lock().unwrap().resumable_chunks
    }
    /// Accept `subject_token` for the workload identity provider `audience`
    /// (`//iam.googleapis.com/projects/.../providers/...`).
    pub fn add_workload_identity_provider(&self, audience: &str, subject_token: &str) {
        self.state.lock().unwrap().wif_providers.push((audience.into(), subject_token.into()));
    }
    /// STS token exchanges served.
    pub fn sts_requests(&self) -> usize {
        self.state.lock().unwrap().sts_requests
    }
    /// Service accounts impersonated through `generateAccessToken`.
    pub fn impersonations(&self) -> Vec<String> {
        self.state.lock().unwrap().impersonations.clone()
    }
    /// Metadata server token requests served.
    pub fn metadata_requests(&self) -> usize {
        self.state.lock().unwrap().metadata_requests
    }
    /// An `external_account` (workload identity federation) credentials JSON for a provider,
    /// reading the subject token from `token_file` and impersonating `service_account` (or
    /// using the federated token directly when None).
    pub fn external_account_json(&self, audience: &str, token_file: &str, service_account: Option<&str>) -> String {
        let mut m = ObjectMap::new();
        m.insert("type".into(), Value::from("external_account"));
        m.insert("audience".into(), Value::from(audience));
        m.insert("subject_token_type".into(), Value::from("urn:ietf:params:oauth:token-type:jwt"));
        m.insert("token_url".into(), Value::from(format!("{}/v1/token", self.url())));
        m.insert("credential_source".into(), obj([("file", Value::from(token_file)), ("format", obj([("type", Value::from("text"))]))]));
        if let Some(sa) = service_account {
            m.insert("service_account_impersonation_url".into(), Value::from(format!("{}/v1/projects/-/serviceAccounts/{}:generateAccessToken", self.url(), sa)));
        }
        Value::Object(m).to_json()
    }
    /// The metadata server base URL (`/computeMetadata/v1/...`).
    pub fn metadata_server_url(&self) -> String {
        self.url()
    }

    /// Revoke all issued tokens (clients must fetch new ones).
    pub fn revoke_tokens(&self) {
        self.state.lock().unwrap().tokens.clear();
    }

    pub fn create_bucket(&self, name: &str) {
        self.state.lock().unwrap().buckets.entry(name.into()).or_insert_with(|| GcsBucket { created: now_secs(), objects: BTreeMap::new() });
    }
    pub fn bucket_exists(&self, name: &str) -> bool {
        self.state.lock().unwrap().buckets.contains_key(name)
    }
    pub fn put_object(&self, bucket: &str, name: &str, data: &[u8]) {
        self.create_bucket(bucket);
        let mut s = self.state.lock().unwrap();
        let gen = next(&mut s);
        s.buckets.get_mut(bucket).unwrap().objects.insert(name.into(), new_obj(data.to_vec(), "application/octet-stream".into(), gen));
    }
    pub fn object(&self, bucket: &str, name: &str) -> Option<Vec<u8>> {
        self.state.lock().unwrap().buckets.get(bucket).and_then(|b| b.objects.get(name)).map(|o| o.data.clone())
    }
    pub fn object_names(&self, bucket: &str) -> Vec<String> {
        self.state.lock().unwrap().buckets.get(bucket).map(|b| b.objects.keys().cloned().collect()).unwrap_or_default()
    }

    /// Create a topic `projects/{project}/topics/{topic}`.
    pub fn create_topic(&self, project: &str, topic: &str) {
        self.state.lock().unwrap().topics.entry(format!("projects/{}/topics/{}", project, topic)).or_default();
    }
    /// Create a subscription to a topic.
    pub fn create_subscription(&self, project: &str, subscription: &str, topic: &str) {
        let mut s = self.state.lock().unwrap();
        let t = format!("projects/{}/topics/{}", project, topic);
        let name = format!("projects/{}/subscriptions/{}", project, subscription);
        s.topics.entry(t.clone()).or_default().push(name.clone());
        s.subscriptions.insert(name, Subscription { topic: t, ack_deadline: 10, messages: vec![], dead_letter: None, exactly_once: false });
    }
    /// Publish directly; returns the message id.
    pub fn publish(&self, project: &str, topic: &str, data: &[u8], attributes: &[(&str, &str)]) -> String {
        let attrs = Value::Object(attributes.iter().map(|(k, v)| (k.to_string(), Value::from(*v))).collect());
        let mut s = self.state.lock().unwrap();
        publish_one(&mut s, &format!("projects/{}/topics/{}", project, topic), base64_encode(data), attrs, String::new()).unwrap_or_default()
    }
    /// Decoded data of the messages still in a subscription (acked ones are removed).
    pub fn backlog(&self, project: &str, subscription: &str) -> Vec<String> {
        let s = self.state.lock().unwrap();
        s.subscriptions
            .get(&format!("projects/{}/subscriptions/{}", project, subscription))
            .map(|sub| sub.messages.iter().map(|m| String::from_utf8_lossy(&base64_decode(&m.data).unwrap_or_default()).into_owned()).collect())
            .unwrap_or_default()
    }
    /// Attributes of the messages still in a subscription.
    pub fn backlog_attributes(&self, project: &str, subscription: &str) -> Vec<Value> {
        let s = self.state.lock().unwrap();
        s.subscriptions.get(&format!("projects/{}/subscriptions/{}", project, subscription)).map(|sub| sub.messages.iter().map(|m| m.attributes.clone()).collect()).unwrap_or_default()
    }
}

/// A service account key JSON for the embedded test RSA key.
pub fn service_account_json(token_uri: &str, client_email: &str) -> String {
    obj([
        ("type", Value::from("service_account")),
        ("project_id", Value::from(PROJECT)),
        ("private_key_id", Value::from("mock-key-1")),
        ("private_key", Value::from(TEST_RSA_KEY_PEM)),
        ("client_email", Value::from(client_email)),
        ("client_id", Value::from("1234567890")),
        ("token_uri", Value::from(token_uri)),
    ])
    .to_json()
}

fn next(s: &mut State) -> i64 {
    s.next_id += 1;
    s.next_id
}

fn new_obj(data: Vec<u8>, content_type: String, generation: i64) -> GcsObj {
    let now = now_millis();
    GcsObj { data, content_type, generation, created: now, updated: now }
}

fn publish_one(s: &mut State, topic: &str, data: String, attributes: Value, ordering_key: String) -> Option<String> {
    let subs = s.topics.get(topic)?.clone();
    let id = next(s).to_string();
    for sub in subs {
        if let Some(sb) = s.subscriptions.get_mut(&sub) {
            sb.messages.push(PsMsg {
                id: id.clone(),
                data: data.clone(),
                attributes: attributes.clone(),
                ordering_key: ordering_key.clone(),
                publish_time: now_millis(),
                ack_id: None,
                visible_at: 0,
                attempts: 0,
            });
        }
    }
    Some(id)
}

fn gerror(code: u16, status: &str, message: &str) -> Response {
    Response::json(code, obj([("error", obj([("code", Value::Int(code as i64)), ("message", Value::from(message)), ("status", Value::from(status))]))]).to_json())
}

fn handle(st: &Shared, req: &ServerRequest) -> Response {
    if req.path == "/token" && req.method == "POST" {
        return token(st, req);
    }
    if req.path == "/v1/token" && req.method == "POST" {
        return sts_token(st, req);
    }
    if req.path.starts_with("/computeMetadata/v1/instance/service-accounts/") && req.method == "GET" {
        return metadata_token(st, req);
    }
    // signed source archive URLs carry their own authorization
    if let Some(r) = ext::function_sources(st, req) {
        return r;
    }
    if let Some(r) = more::signed_url(st, req) {
        return r;
    }
    {
        let mut s = st.lock().unwrap();
        if s.require_auth {
            let tok = req.header("Authorization").and_then(|a| a.strip_prefix("Bearer ")).unwrap_or("");
            let valid = s.tokens.get(tok).is_some_and(|exp| *exp > now_secs());
            if !valid {
                s.auth_failures += 1;
                return gerror(401, "UNAUTHENTICATED", "Request had invalid authentication credentials. Expected OAuth 2 access token.");
            }
        }
    }
    let segs = req.segments();
    let s: Vec<&str> = segs.iter().map(|x| x.as_str()).collect();
    if let ["v1", "operations", id] = s.as_slice() {
        return ext::function_operation(st, id);
    }
    if let ["v1", "projects", "-", "serviceAccounts", target] = s.as_slice() {
        if let Some(email) = target.strip_suffix(":generateAccessToken") {
            return generate_access_token(st, req, email);
        }
    }
    if let Some(r) = ext::handle(st, req, &s) {
        return r;
    }
    if let Some(r) = more::handle(st, req, &s) {
        return r;
    }
    match s.as_slice() {
        ["storage", "v1", ..] | ["upload", "storage", "v1", ..] => storage(st, req, &s),
        ["v1", "projects", ..] => pubsub(st, req, &s),
        _ => gerror(404, "NOT_FOUND", "Unknown resource"),
    }
}

/// STS token exchange (workload identity federation).
fn sts_token(st: &Shared, req: &ServerRequest) -> Response {
    let p = parse_form(&req.text());
    let get = |k: &str| p.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap_or_default();
    let mut s = st.lock().unwrap();
    let fail = |s: &mut State, code: &str, desc: &str| {
        s.auth_failures += 1;
        Response::json(400, obj([("error", Value::from(code)), ("error_description", Value::from(desc))]).to_json())
    };
    if get("grant_type") != "urn:ietf:params:oauth:grant-type:token-exchange" {
        return fail(&mut s, "unsupported_grant_type", "Invalid grant_type");
    }
    if get("requested_token_type") != "urn:ietf:params:oauth:token-type:access_token" || get("subject_token_type").is_empty() {
        return fail(&mut s, "invalid_request", "Invalid requested_token_type or missing subject_token_type");
    }
    let (aud, subject) = (get("audience"), get("subject_token"));
    if !s.wif_providers.iter().any(|(a, t)| *a == aud && crate::util::ct_eq(t.as_bytes(), subject.as_bytes())) {
        return fail(&mut s, "invalid_grant", "The audience or the subject token is invalid for this workload identity pool provider.");
    }
    let tok = format!("ya29.sts-{}", random_hex(16));
    s.tokens.insert(tok.clone(), now_secs() + 3600);
    s.ext.token_scopes.insert(tok.clone(), get("scope"));
    s.sts_requests += 1;
    Response::json(200, obj([("access_token", Value::from(tok)), ("issued_token_type", Value::from("urn:ietf:params:oauth:token-type:access_token")), ("token_type", Value::from("Bearer")), ("expires_in", Value::Int(3600))]).to_json())
}

/// IAM Credentials `generateAccessToken` (service account impersonation).
fn generate_access_token(st: &Shared, req: &ServerRequest, email: &str) -> Response {
    let body = parse_json(&req.body).unwrap_or_default();
    let mut s = st.lock().unwrap();
    let scopes: Vec<String> = body.get("scope").and_then(|a| a.as_array()).map(|a| a.iter().map(|x| x.to_string()).collect()).unwrap_or_default();
    if scopes.is_empty() {
        return gerror(400, "INVALID_ARGUMENT", "Scope required.");
    }
    if !s.keys.contains_key(email) {
        return gerror(404, "NOT_FOUND", &format!("Service account {} not found", email));
    }
    let tok = format!("ya29.impersonated-{}", random_hex(16));
    s.tokens.insert(tok.clone(), now_secs() + 3600);
    s.ext.token_scopes.insert(tok.clone(), scopes.join(" "));
    s.impersonations.push(email.to_string());
    Response::json(200, obj([("accessToken", Value::from(tok)), ("expireTime", Value::from(crate::util::iso8601(now_secs() + 3600)))]).to_json())
}

/// The metadata server's service account token (`Metadata-Flavor: Google` required).
fn metadata_token(st: &Shared, req: &ServerRequest) -> Response {
    if req.header("Metadata-Flavor") != Some("Google") {
        return Response::new(403).with_body(&b"Missing Metadata-Flavor:Google header."[..]);
    }
    if !req.path.ends_with("/token") {
        return Response::new(404).with_body(&b"not found"[..]);
    }
    let mut s = st.lock().unwrap();
    let tok = format!("ya29.metadata-{}", random_hex(16));
    s.tokens.insert(tok.clone(), now_secs() + 3600);
    s.ext.token_scopes.insert(tok.clone(), req.query_param("scopes").unwrap_or_else(|| crate::gcp::CLOUD_PLATFORM_SCOPE.to_string()).replace(',', " "));
    s.metadata_requests += 1;
    Response::json(200, obj([("access_token", Value::from(tok)), ("expires_in", Value::Int(3599)), ("token_type", Value::from("Bearer"))]).to_json()).with_header("Metadata-Flavor", "Google")
}

fn token(st: &Shared, req: &ServerRequest) -> Response {
    let p = parse_form(&req.text());
    let get = |k: &str| p.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap_or_default();
    let fail = |st: &Shared, desc: &str| {
        st.lock().unwrap().auth_failures += 1;
        Response::json(400, obj([("error", Value::from("invalid_grant")), ("error_description", Value::from(desc))]).to_json())
    };
    if get("grant_type") == "refresh_token" {
        return more::refresh_grant(st, &p);
    }
    if get("grant_type") != "urn:ietf:params:oauth:grant-type:jwt-bearer" {
        return fail(st, "Invalid grant_type");
    }
    let jwt = get("assertion");
    // the issuer is needed to pick the key: read the (unverified) claims first
    let iss = jwt
        .split('.')
        .nth(1)
        .and_then(base64_decode)
        .and_then(|c| parse_json(&c).ok())
        .and_then(|c| c.get("iss").map(|i| i.to_string()))
        .unwrap_or_default();
    let key = st.lock().unwrap().keys.get(&iss).cloned();
    let Some(key) = key else { return fail(st, "Invalid JWT: unknown service account") };
    let claims = match verify_jwt(&jwt, &key) {
        Ok(c) => c,
        Err(e) => return fail(st, &format!("Invalid JWT Signature: {}", e)),
    };
    let now = now_secs();
    let num = |k: &str| claims.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
    let aud = claims.get("aud").map(|a| a.to_string()).unwrap_or_default();
    let expected_aud = format!("http://{}/token", req.header("Host").unwrap_or(""));
    if aud != expected_aud {
        return fail(st, "Invalid JWT: audience mismatch");
    }
    if num("exp") <= now || num("iat") > now + 300 || num("exp") - num("iat") > 3600 {
        return fail(st, "Invalid JWT: Token must be a short-lived token (60 minutes) and in a reasonable timeframe");
    }
    let scope = claims.get("scope").map(|s| s.to_string()).unwrap_or_default();
    if scope.is_empty() {
        return fail(st, "Invalid JWT: missing scope");
    }
    let tok = format!("ya29.mock-{}", random_hex(16));
    let mut s = st.lock().unwrap();
    s.tokens.insert(tok.clone(), now + 3600);
    s.ext.token_scopes.insert(tok.clone(), scope);
    if let Some(sub) = claims.get("sub").map(|x| x.to_string()).filter(|x| !x.is_empty()) {
        s.more.token_subjects.insert(tok.clone(), sub);
    }
    s.token_requests += 1;
    Response::json(200, obj([("access_token", Value::from(tok)), ("expires_in", Value::Int(3599)), ("token_type", Value::from("Bearer"))]).to_json())
}

fn object_resource(bucket: &str, name: &str, o: &GcsObj) -> Value {
    obj([
        ("kind", Value::from("storage#object")),
        ("id", Value::from(format!("{}/{}/{}", bucket, name, o.generation))),
        ("name", Value::from(name)),
        ("bucket", Value::from(bucket)),
        ("generation", Value::from(o.generation.to_string())),
        ("size", Value::from(o.data.len().to_string())),
        ("contentType", Value::from(o.content_type.clone())),
        ("md5Hash", Value::from(base64_encode(&md5(&o.data)))),
        ("etag", Value::from(format!("C{}", o.generation))),
        ("timeCreated", Value::from(iso8601_millis(o.created))),
        ("updated", Value::from(iso8601_millis(o.updated))),
        ("storageClass", Value::from("STANDARD")),
    ])
}

fn bucket_resource(name: &str, b: &GcsBucket) -> Value {
    obj([
        ("kind", Value::from("storage#bucket")),
        ("id", Value::from(name)),
        ("name", Value::from(name)),
        ("timeCreated", Value::from(iso8601_millis(b.created * 1000))),
        ("location", Value::from("US-EAST1")),
        ("storageClass", Value::from("STANDARD")),
    ])
}

fn storage(st: &Shared, req: &ServerRequest, s: &[&str]) -> Response {
    let mut state = st.lock().unwrap();
    let not_found = || gerror(404, "NOT_FOUND", "No such object or bucket");
    match (req.method.as_str(), s) {
        ("POST", ["upload", "storage", "v1", "b", bucket, "o"]) if req.query_param("uploadType").as_deref() == Some("resumable") => {
            if !state.buckets.contains_key(*bucket) {
                return gerror(404, "NOT_FOUND", "The specified bucket does not exist.");
            }
            let meta = parse_json(&req.body).unwrap_or_default();
            let Some(name) = meta.get("name").map(|n| n.to_string()).or_else(|| req.query_param("name")).filter(|n| !n.is_empty()) else { return gerror(400, "INVALID_ARGUMENT", "Missing name") };
            let ct = req.header("X-Upload-Content-Type").map(|c| c.to_string()).or_else(|| meta.get("contentType").map(|c| c.to_string())).unwrap_or_else(|| "application/octet-stream".into());
            let size = req.header("X-Upload-Content-Length").and_then(|l| l.parse().ok());
            let id = random_hex(16);
            state.resumable.insert(id.clone(), (bucket.to_string(), name, ct, size, Vec::new()));
            let host = req.header("Host").unwrap_or("localhost").to_string();
            Response::new(200).with_header("Location", format!("http://{}/upload/storage/v1/b/{}/o?uploadType=resumable&upload_id={}", host, bucket, id)).with_header("X-GUploader-UploadID", id)
        }
        ("PUT", ["upload", "storage", "v1", "b", _bucket, "o"]) => {
            let Some(id) = req.query_param("upload_id") else { return gerror(400, "INVALID_ARGUMENT", "Missing upload_id") };
            let range = req.header("Content-Range").unwrap_or("").trim().to_string();
            let Some(rest) = range.strip_prefix("bytes ") else { return gerror(400, "INVALID_ARGUMENT", "Missing Content-Range") };
            let (span, total) = rest.split_once('/').unwrap_or((rest, "*"));
            let total: Option<usize> = total.parse().ok();
            let fail_now = std::mem::take(&mut state.fail_next_chunk) && span != "*";
            state.resumable_chunks += usize::from(span != "*");
            let gen = next(&mut state);
            let Some(sess) = state.resumable.get_mut(&id) else { return gerror(404, "NOT_FOUND", "No such upload session") };
            let committed = sess.4.len();
            if span != "*" {
                let Some((a, b)) = span.split_once('-').and_then(|(a, b)| Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?))) else { return gerror(400, "INVALID_ARGUMENT", "Invalid Content-Range") };
                if a != committed {
                    return gerror(400, "INVALID_ARGUMENT", &format!("Invalid request: the chunk starts at {} but {} bytes are committed", a, committed));
                }
                if req.body.len() != b + 1 - a {
                    return gerror(400, "INVALID_ARGUMENT", "The body does not match Content-Range");
                }
                let last = total == Some(b + 1);
                if !last && req.body.len() % (256 * 1024) != 0 {
                    return gerror(400, "INVALID_ARGUMENT", "Non-final chunks must be a multiple of 256 KiB");
                }
                if fail_now {
                    let half = req.body.len() / 2 / (256 * 1024) * (256 * 1024);
                    sess.4.extend_from_slice(&req.body[..half]);
                    return gerror(503, "UNAVAILABLE", "Backend error, please retry");
                }
                sess.4.extend_from_slice(&req.body);
            }
            let total = total.or(sess.3);
            if total == Some(sess.4.len()) {
                let (bucket, name, ct, _, data) = state.resumable.remove(&id).expect("session");
                let o = new_obj(data, ct, gen);
                let r = object_resource(&bucket, &name, &o);
                match state.buckets.get_mut(&bucket) {
                    Some(b) => {
                        b.objects.insert(name, o);
                        Response::json(200, r.to_json())
                    }
                    None => gerror(404, "NOT_FOUND", "The specified bucket does not exist."),
                }
            } else {
                let mut r = Response::new(308).with_header("X-GUploader-UploadID", id.clone());
                if !sess.4.is_empty() {
                    r = r.with_header("Range", format!("bytes=0-{}", sess.4.len() - 1));
                }
                r
            }
        }
        ("POST", ["upload", "storage", "v1", "b", bucket, "o"]) => {
            let Some(name) = req.query_param("name") else { return gerror(400, "INVALID_ARGUMENT", "Missing name") };
            let gen = next(&mut state);
            let Some(b) = state.buckets.get_mut(*bucket) else { return gerror(404, "NOT_FOUND", "The specified bucket does not exist.") };
            let o = new_obj(req.body.clone(), req.header("Content-Type").unwrap_or("application/octet-stream").to_string(), gen);
            let r = object_resource(bucket, &name, &o);
            b.objects.insert(name, o);
            Response::json(200, r.to_json())
        }
        ("GET", ["storage", "v1", "b"]) => {
            let items: Vec<Value> = state.buckets.iter().map(|(n, b)| bucket_resource(n, b)).collect();
            Response::json(200, obj([("kind", Value::from("storage#buckets")), ("items", Value::Array(items))]).to_json())
        }
        ("POST", ["storage", "v1", "b"]) => {
            let name = parse_json(&req.body).ok().and_then(|v| v.get("name").map(|n| n.to_string())).unwrap_or_default();
            if req.query_param("project").is_none_or(|p| p.is_empty()) || name.is_empty() {
                return gerror(400, "INVALID_ARGUMENT", "project and name are required");
            }
            if state.buckets.contains_key(&name) {
                return gerror(409, "ALREADY_EXISTS", "Your previous request to create the named bucket succeeded and you already own it.");
            }
            let b = GcsBucket { created: now_secs(), objects: BTreeMap::new() };
            let r = bucket_resource(&name, &b);
            state.buckets.insert(name, b);
            Response::json(200, r.to_json())
        }
        ("GET", ["storage", "v1", "b", bucket]) => match state.buckets.get(*bucket) {
            Some(b) => Response::json(200, bucket_resource(bucket, b).to_json()),
            None => not_found(),
        },
        ("DELETE", ["storage", "v1", "b", bucket]) => match state.buckets.get(*bucket) {
            None => not_found(),
            Some(b) if !b.objects.is_empty() => gerror(409, "FAILED_PRECONDITION", "The bucket you tried to delete is not empty."),
            Some(_) => {
                state.buckets.remove(*bucket);
                Response::new(204)
            }
        },
        ("GET", ["storage", "v1", "b", bucket, "o"]) => {
            let Some(b) = state.buckets.get(*bucket) else { return not_found() };
            let prefix = req.query_param("prefix").unwrap_or_default();
            let max: usize = req.query_param("maxResults").and_then(|m| m.parse().ok()).unwrap_or(1000);
            let token = req.query_param("pageToken").unwrap_or_default();
            let mut items = Vec::new();
            let mut next_token = None;
            for (n, o) in b.objects.range(token..).filter(|(n, _)| n.starts_with(&prefix)) {
                if items.len() == max {
                    next_token = Some(n.clone());
                    break;
                }
                items.push(object_resource(bucket, n, o));
            }
            let mut m = ObjectMap::new();
            m.insert("kind".to_string(), Value::from("storage#objects"));
            m.insert("items".to_string(), Value::Array(items));
            if let Some(t) = next_token {
                m.insert("nextPageToken".to_string(), Value::from(t));
            }
            Response::json(200, Value::Object(m).to_json())
        }
        ("GET", ["storage", "v1", "b", bucket, "o", name]) => match state.buckets.get(*bucket).and_then(|b| b.objects.get(*name)) {
            None => not_found(),
            Some(o) if req.query_param("alt").as_deref() == Some("media") => Response::new(200).with_header("Content-Type", o.content_type.clone()).with_body(o.data.clone()),
            Some(o) => Response::json(200, object_resource(bucket, name, o).to_json()),
        },
        ("DELETE", ["storage", "v1", "b", bucket, "o", name]) => match state.buckets.get_mut(*bucket).and_then(|b| b.objects.remove(*name)) {
            Some(_) => Response::new(204),
            None => not_found(),
        },
        ("POST", ["storage", "v1", "b", sb, "o", so, "copyTo", "b", db, "o", dobj]) => {
            let Some(src) = state.buckets.get(*sb).and_then(|b| b.objects.get(*so)).cloned() else { return not_found() };
            let gen = next(&mut state);
            let Some(dst) = state.buckets.get_mut(*db) else { return not_found() };
            let o = GcsObj { generation: gen, updated: now_millis(), ..src };
            let r = object_resource(db, dobj, &o);
            dst.objects.insert(dobj.to_string(), o);
            Response::json(200, r.to_json())
        }
        _ => gerror(400, "INVALID_ARGUMENT", "Unsupported storage request"),
    }
}

fn sub_resource(name: &str, s: &Subscription) -> Value {
    let mut m = ObjectMap::new();
    m.insert("name".into(), Value::from(name));
    m.insert("topic".into(), Value::from(s.topic.clone()));
    m.insert("ackDeadlineSeconds".into(), Value::Int(s.ack_deadline));
    if s.exactly_once {
        m.insert("enableExactlyOnceDelivery".into(), Value::Bool(true));
    }
    if let Some((t, max)) = &s.dead_letter {
        m.insert("deadLetterPolicy".into(), obj([("deadLetterTopic", Value::from(t.clone())), ("maxDeliveryAttempts", Value::Int(*max))]));
    }
    Value::Object(m)
}

/// Move messages delivered `maxDeliveryAttempts` times (and due again) to the dead-letter topic.
fn forward_dead_letters(state: &mut State, sub: &str) {
    let now = now_millis();
    let Some(s) = state.subscriptions.get_mut(sub) else { return };
    let Some((dlt, max)) = s.dead_letter.clone() else { return };
    let mut moved = Vec::new();
    s.messages.retain(|m| {
        if m.attempts >= max && m.visible_at <= now {
            moved.push(m.clone());
            false
        } else {
            true
        }
    });
    for m in moved {
        let mut attrs = m.attributes.as_object().cloned().unwrap_or_default();
        attrs.insert("CloudPubSubDeadLetterSourceSubscription".into(), Value::from(sub));
        attrs.insert("CloudPubSubDeadLetterSourceDeliveryCount".into(), Value::from(m.attempts.to_string()));
        publish_one(state, &dlt, m.data, Value::Object(attrs), m.ordering_key);
    }
}

fn pubsub(st: &Shared, req: &ServerRequest, s: &[&str]) -> Response {
    let mut state = st.lock().unwrap();
    let body = parse_json(&req.body).unwrap_or_default();
    let ok = |v: Value| Response::json(200, v.to_json());
    match s {
        ["v1", "projects", project, "topics", rest] => {
            let (topic, verb) = rest.split_once(':').unwrap_or((rest, ""));
            let name = format!("projects/{}/topics/{}", project, topic);
            match (req.method.as_str(), verb) {
                ("PUT", "") => {
                    if state.topics.contains_key(&name) {
                        return gerror(409, "ALREADY_EXISTS", "Topic already exists");
                    }
                    state.topics.insert(name.clone(), vec![]);
                    ok(obj([("name", Value::from(name))]))
                }
                ("GET", "") if state.topics.contains_key(&name) => ok(obj([("name", Value::from(name))])),
                ("DELETE", "") => match state.topics.remove(&name) {
                    Some(_) => ok(obj([])),
                    None => gerror(404, "NOT_FOUND", "Resource not found (resource=topic)."),
                },
                ("POST", "publish") => {
                    if !state.topics.contains_key(&name) {
                        return gerror(404, "NOT_FOUND", "Resource not found (resource=topic).");
                    }
                    let mut ids = Vec::new();
                    for m in body.get("messages").and_then(|m| m.as_array()).cloned().unwrap_or_default() {
                        let data = m.get("data").map(|d| d.to_string()).unwrap_or_default();
                        if base64_decode(&data).is_none() {
                            return gerror(400, "INVALID_ARGUMENT", "Invalid data in message: must be base64");
                        }
                        let attrs = m.get("attributes").cloned().unwrap_or(Value::Object(ObjectMap::new()));
                        let key = m.get("orderingKey").map(|k| k.to_string()).unwrap_or_default();
                        ids.push(Value::from(publish_one(&mut state, &name, data, attrs, key).unwrap_or_default()));
                    }
                    ok(obj([("messageIds", Value::Array(ids))]))
                }
                _ => gerror(404, "NOT_FOUND", "Resource not found (resource=topic)."),
            }
        }
        ["v1", "projects", project, "subscriptions", rest] => {
            let (sub, verb) = rest.split_once(':').unwrap_or((rest, ""));
            let name = format!("projects/{}/subscriptions/{}", project, sub);
            if req.method == "PUT" && verb.is_empty() {
                let topic = body.get("topic").map(|t| t.to_string()).unwrap_or_default();
                if !state.topics.contains_key(&topic) {
                    return gerror(404, "NOT_FOUND", "Resource not found (resource=topic).");
                }
                if state.subscriptions.contains_key(&name) {
                    return gerror(409, "ALREADY_EXISTS", "Subscription already exists");
                }
                let dead_letter = match body.get("deadLetterPolicy") {
                    Some(p) => {
                        let t = p.get("deadLetterTopic").map(|t| t.to_string()).unwrap_or_default();
                        let max = p.get("maxDeliveryAttempts").and_then(|m| m.as_i64()).unwrap_or(5);
                        if !state.topics.contains_key(&t) {
                            return gerror(404, "NOT_FOUND", "Resource not found (resource=dead letter topic).");
                        }
                        if !(5..=100).contains(&max) {
                            return gerror(400, "INVALID_ARGUMENT", "Invalid max_delivery_attempts: must be between 5 and 100.");
                        }
                        Some((t, max))
                    }
                    None => None,
                };
                let exactly_once = body.get("enableExactlyOnceDelivery").and_then(|e| e.as_bool()) == Some(true);
                state.topics.get_mut(&topic).unwrap().push(name.clone());
                let deadline = body.get("ackDeadlineSeconds").and_then(|d| d.as_i64()).unwrap_or(10);
                state.subscriptions.insert(name.clone(), Subscription { topic: topic.clone(), ack_deadline: deadline, messages: vec![], dead_letter: dead_letter.clone(), exactly_once });
                return ok(sub_resource(&name, &state.subscriptions[&name]));
            }
            if req.method == "DELETE" && verb.is_empty() {
                let Some(sub) = state.subscriptions.remove(&name) else { return gerror(404, "NOT_FOUND", "Resource not found (resource=subscription).") };
                if let Some(subs) = state.topics.get_mut(&sub.topic) {
                    subs.retain(|s| *s != name);
                }
                return ok(obj([]));
            }
            if req.method == "POST" && verb == "pull" {
                forward_dead_letters(&mut state, &name);
            }
            let Some(subscription) = state.subscriptions.get_mut(&name) else { return gerror(404, "NOT_FOUND", "Resource not found (resource=subscription).") };
            let ack_ids: Vec<String> = body.get("ackIds").and_then(|a| a.as_array()).map(|a| a.iter().map(|v| v.to_string()).collect()).unwrap_or_default();
            match (req.method.as_str(), verb) {
                ("GET", "") => ok(sub_resource(&name, subscription)),
                ("POST", "pull") => {
                    let max = body.get("maxMessages").and_then(|m| m.as_i64()).unwrap_or(1).max(1) as usize;
                    let now = now_millis();
                    let deadline = subscription.ack_deadline;
                    let mut out = Vec::new();
                    for m in subscription.messages.iter_mut().filter(|m| m.visible_at <= now).take(max) {
                        let ack = format!("ack-{}-{}", m.id, random_hex(6));
                        m.ack_id = Some(ack.clone());
                        m.visible_at = now + deadline * 1000;
                        m.attempts += 1;
                        let mut msg = ObjectMap::new();
                        msg.insert("data".to_string(), Value::from(m.data.clone()));
                        msg.insert("attributes".to_string(), m.attributes.clone());
                        msg.insert("messageId".to_string(), Value::from(m.id.clone()));
                        msg.insert("publishTime".to_string(), Value::from(iso8601_millis(m.publish_time)));
                        if !m.ordering_key.is_empty() {
                            msg.insert("orderingKey".to_string(), Value::from(m.ordering_key.clone()));
                        }
                        out.push(obj([("ackId", Value::from(ack)), ("message", Value::Object(msg)), ("deliveryAttempt", Value::Int(m.attempts))]));
                    }
                    ok(if out.is_empty() { obj([]) } else { obj([("receivedMessages", Value::Array(out))]) })
                }
                ("POST", "acknowledge") => {
                    if subscription.exactly_once {
                        // every ack id must belong to a message whose lease is still valid
                        let now = now_millis();
                        let invalid: Vec<&String> = ack_ids.iter().filter(|a| !subscription.messages.iter().any(|m| m.ack_id.as_ref() == Some(*a) && m.visible_at > now)).collect();
                        if !invalid.is_empty() {
                            let meta: ObjectMap = invalid.iter().map(|a| (a.to_string(), Value::from("PERMANENT_FAILURE_INVALID_ACK_ID"))).collect();
                            let valid: Vec<String> = ack_ids.iter().filter(|a| !invalid.contains(a)).cloned().collect();
                            subscription.messages.retain(|m| m.ack_id.as_ref().is_none_or(|a| !valid.contains(a)));
                            return Response::json(
                                400,
                                obj([(
                                    "error",
                                    obj([
                                        ("code", Value::Int(400)),
                                        ("message", Value::from("Some acknowledgement ids in the request were invalid. This could be because the acknowledgement ids have expired or the acknowledgement ids were malformed.")),
                                        ("status", Value::from("INVALID_ARGUMENT")),
                                        ("details", Value::Array(vec![obj([("@type", Value::from("type.googleapis.com/google.rpc.ErrorInfo")), ("reason", Value::from("EXACTLY_ONCE_ACKID_FAILURE")), ("domain", Value::from("pubsub.googleapis.com")), ("metadata", Value::Object(meta))])])),
                                    ]),
                                )])
                                .to_json(),
                            );
                        }
                    }
                    subscription.messages.retain(|m| m.ack_id.as_ref().is_none_or(|a| !ack_ids.contains(a)));
                    ok(obj([]))
                }
                ("POST", "modifyAckDeadline") => {
                    let secs = body.get("ackDeadlineSeconds").and_then(|d| d.as_i64()).unwrap_or(0);
                    let now = now_millis();
                    for m in subscription.messages.iter_mut().filter(|m| m.ack_id.as_ref().is_some_and(|a| ack_ids.contains(a))) {
                        m.visible_at = now + secs * 1000;
                    }
                    ok(obj([]))
                }
                _ => gerror(400, "INVALID_ARGUMENT", "Unsupported subscription request"),
            }
        }
        _ => gerror(404, "NOT_FOUND", "Unknown Pub/Sub resource"),
    }
}

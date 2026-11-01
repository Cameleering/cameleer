//! In-memory emulator of Azure services that do not live in Azurite: a Microsoft Entra ID
//! (Azure AD) token endpoint (client credentials), Key Vault secrets and the Cosmos DB SQL API,
//! on one port.
//!
//! * `POST /{tenant}/oauth2/v2.0/token` verifies the registered client id / secret and issues
//!   bearer tokens for the requested `/.default` scope.
//! * Key Vault (`/secrets/...`, `/deletedsecrets/...`) requires a valid token for
//!   `https://vault.azure.net/.default` (`401` otherwise).
//! * Cosmos DB (`/dbs/...`) verifies the master-key `authorization` signature (recomputed with
//!   the account key) and the `x-ms-date` freshness; supports databases, containers, items
//!   with partition keys, a SQL subset (`SELECT * FROM c WHERE c.a = @p AND c.b > 3 ORDER BY
//!   c.x DESC`) and the change feed (`A-IM: Incremental feed`).

#[path = "azure_more.rs"]
mod more;
#[path = "azure_keys.rs"]
mod keys;
#[path = "azure_keycrypto.rs"]
mod crypto;
pub use more::{AzureFn, FUNCTIONS_MASTER_KEY, OPENAI_KEY};

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::{Arc, Mutex};

use camel_core::value::Value;

use super::{Handler, MockServer};
use crate::azure::cosmosdb::{master_key_signature, resource_of};
use crate::http::{Response, ServerRequest};
use crate::util::{ct_eq, now_secs, obj, parse_form, parse_http_date, parse_json, percent_decode, random_hex};

/// Tenant of the default test client.
pub const TENANT_ID: &str = "00000000-0000-0000-0000-00000000aaaa";
/// Client id of the default test client.
pub const CLIENT_ID: &str = "11111111-2222-3333-4444-555555555555";
/// Client secret of the default test client.
pub const CLIENT_SECRET: &str = "mock~client~secret";
/// The Cosmos DB account key (base64), the emulator's well-known key.
pub const COSMOS_KEY: &str = "C2y6yDjf5/R+ob0N8A7Cgv30VRDJIWEHLM+4QDU5DE2nQ9nDuVTqobD4b8mGGyPMbIZnqyMsEcaGQy67XIw/Jw==";

#[derive(Clone)]
struct KvVersion {
    version: String,
    value: String,
    created: i64,
}

#[derive(Default)]
struct Coll {
    rid: String,
    pk_path: String,
    lsn: u64,
    docs: BTreeMap<(String, String), (Value, u64)>,
    /// number of partition key ranges (documents spread by partition key hash)
    ranges: usize,
}

#[derive(Default)]
struct Db {
    rid: String,
    colls: BTreeMap<String, Coll>,
}

/// A Cosmos offer (throughput) of a database or container.
#[derive(Clone)]
struct Offer {
    id: String,
    resource_rid: String,
    resource_link: String,
    throughput: Option<i64>,
    autoscale_max: Option<i64>,
}

#[derive(Default)]
struct State {
    clients: HashMap<(String, String), String>,
    /// (tenant, client id) -> accepted federated tokens (workload identity)
    federated: HashMap<(String, String), Vec<String>>,
    /// user-assigned managed identities (client ids); the system identity is always present
    identities: Vec<String>,
    /// App Service `X-IDENTITY-HEADER` secret
    identity_header: String,
    /// (endpoint kind, resource, client id) of managed identity token requests
    identity_requests: Vec<(String, String, String)>,
    tokens: HashMap<String, (String, i64)>,
    token_requests: usize,
    auth_failures: usize,
    secrets: BTreeMap<String, Vec<KvVersion>>,
    deleted: BTreeMap<String, Vec<KvVersion>>,
    dbs: BTreeMap<String, Db>,
    offers: Vec<Offer>,
    cosmos_consistency: Vec<String>,
    cosmos_minimal_writes: usize,
    cosmos_aad_requests: usize,
    more: more::MoreState,
    keys: keys::KeysState,
}

type Shared = Arc<Mutex<State>>;

/// The Azure AD + Key Vault + Cosmos DB emulator.
pub struct AzureMock {
    server: MockServer,
    state: Shared,
}

impl AzureMock {
    /// Start with the default client ([`TENANT_ID`], [`CLIENT_ID`], [`CLIENT_SECRET`]) registered.
    pub fn start() -> io::Result<AzureMock> {
        let state: Shared = Arc::new(Mutex::new(State::default()));
        {
            let mut s = state.lock().unwrap();
            s.clients.insert((TENANT_ID.into(), CLIENT_ID.into()), CLIENT_SECRET.into());
            s.identity_header = format!("identity-{}", random_hex(8));
        }
        let st = state.clone();
        let handler: Handler = Arc::new(move |r| handle(&st, r));
        Ok(AzureMock { server: MockServer::start(handler)?, state })
    }

    pub fn url(&self) -> String {
        self.server.url()
    }

    /// Options for `azure-key-vault://vault?...` (client credentials against this emulator).
    pub fn key_vault_options(&self) -> String {
        format!("tenantId={}&clientId={}&clientSecret=RAW({})&authorityHost={}&uriEndpointOverride={}&overrideEndpoint=true", TENANT_ID, CLIENT_ID, CLIENT_SECRET, self.url(), self.url())
    }

    /// Options for `azure-cosmosdb://db/container?...`.
    pub fn cosmos_options(&self) -> String {
        format!("databaseEndpoint={}&accountKey=RAW({})", self.url(), COSMOS_KEY)
    }

    /// Register another client.
    pub fn add_client(&self, tenant: &str, client_id: &str, secret: &str) {
        self.state.lock().unwrap().clients.insert((tenant.into(), client_id.into()), secret.into());
    }

    /// Accept `token` as federated credential of the client (workload identity federation).
    pub fn add_federated_credential(&self, tenant: &str, client_id: &str, token: &str) {
        self.state.lock().unwrap().federated.entry((tenant.into(), client_id.into())).or_default().push(token.into());
    }

    /// Register a user-assigned managed identity (its client id).
    pub fn add_managed_identity(&self, client_id: &str) {
        self.state.lock().unwrap().identities.push(client_id.into());
    }

    /// The managed identity endpoint of the Instance Metadata Service (`Metadata: true`).
    pub fn imds_endpoint(&self) -> String {
        format!("{}/metadata/identity/oauth2/token", self.url())
    }

    /// The App Service managed identity endpoint and its `X-IDENTITY-HEADER` secret.
    pub fn app_service_identity(&self) -> (String, String) {
        (format!("{}/msi/token", self.url()), self.state.lock().unwrap().identity_header.clone())
    }

    /// Managed identity token requests: (`imds` / `appservice`, resource, client id).
    pub fn identity_requests(&self) -> Vec<(String, String, String)> {
        self.state.lock().unwrap().identity_requests.clone()
    }

    pub fn token_requests(&self) -> usize {
        self.state.lock().unwrap().token_requests
    }

    pub fn auth_failures(&self) -> usize {
        self.state.lock().unwrap().auth_failures
    }

    /// Revoke all issued tokens.
    pub fn revoke_tokens(&self) {
        self.state.lock().unwrap().tokens.clear();
    }

    /// Set a Key Vault secret directly.
    pub fn put_secret(&self, name: &str, value: &str) {
        let mut s = self.state.lock().unwrap();
        s.secrets.entry(name.into()).or_default().push(KvVersion { version: random_hex(16), value: value.into(), created: now_secs() });
    }

    /// Latest value of a (non deleted) secret.
    pub fn secret(&self, name: &str) -> Option<String> {
        self.state.lock().unwrap().secrets.get(name).and_then(|v| v.last()).map(|v| v.value.clone())
    }

    /// Whether a secret is in the deleted (recoverable) state.
    pub fn secret_is_deleted(&self, name: &str) -> bool {
        self.state.lock().unwrap().deleted.contains_key(name)
    }

    /// Items of a Cosmos container (by id order).
    pub fn cosmos_items(&self, db: &str, coll: &str) -> Vec<Value> {
        let s = self.state.lock().unwrap();
        s.dbs.get(db).and_then(|d| d.colls.get(coll)).map(|c| c.docs.values().map(|(d, _)| d.clone()).collect()).unwrap_or_default()
    }

    /// Split a container into `n` partition key ranges (documents spread by partition key hash).
    pub fn set_cosmos_partition_ranges(&self, db: &str, coll: &str, n: usize) {
        if let Some(c) = self.state.lock().unwrap().dbs.get_mut(db).and_then(|d| d.colls.get_mut(coll)) {
            c.ranges = n.max(1);
        }
    }

    /// `x-ms-consistency-level` values received.
    pub fn cosmos_consistency_levels(&self) -> Vec<String> {
        self.state.lock().unwrap().cosmos_consistency.clone()
    }

    /// Number of requests with `Prefer: return=minimal`.
    pub fn cosmos_minimal_writes(&self) -> usize {
        self.state.lock().unwrap().cosmos_minimal_writes
    }

    /// Number of Cosmos requests authorized with an Entra ID token.
    pub fn cosmos_aad_requests(&self) -> usize {
        self.state.lock().unwrap().cosmos_aad_requests
    }

    /// Database names.
    pub fn cosmos_databases(&self) -> Vec<String> {
        self.state.lock().unwrap().dbs.keys().cloned().collect()
    }
}

fn handle(st: &Shared, req: &ServerRequest) -> Response {
    let segs = req.segments();
    if segs.len() == 4 && segs[1] == "oauth2" && segs[3] == "token" && req.method == "POST" {
        return token(st, &segs[0], req);
    }
    if req.method == "GET" && (segs == ["metadata", "identity", "oauth2", "token"] || segs == ["msi", "token"]) {
        return managed_identity_token(st, req, segs[0] == "msi");
    }
    if let Some(r) = more::handle(st, req, &segs) {
        return r;
    }
    if let Some(r) = keys::handle(st, req, &segs) {
        return r;
    }
    match segs.first().map(|s| s.as_str()) {
        Some("secrets") | Some("deletedsecrets") => key_vault(st, req, &segs),
        Some("dbs") | Some("offers") => cosmos(st, req, &segs),
        _ => Response::json(404, "{\"error\":{\"code\":\"NotFound\",\"message\":\"Unknown resource\"}}"),
    }
}

// ---------------------------------------------------------------------------
// Azure AD
// ---------------------------------------------------------------------------

fn token(st: &Shared, tenant: &str, req: &ServerRequest) -> Response {
    let p = parse_form(&req.text());
    let get = |k: &str| p.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap_or_default();
    let mut s = st.lock().unwrap();
    let fail = |s: &mut State, code: &str, desc: &str| {
        s.auth_failures += 1;
        Response::json(if code == "invalid_client" { 401 } else { 400 }, obj([("error", Value::from(code)), ("error_description", Value::from(desc))]).to_json())
    };
    if get("grant_type") != "client_credentials" {
        return fail(&mut s, "unsupported_grant_type", "AADSTS70003: The app requested an unsupported grant type.");
    }
    let scope = get("scope");
    if !scope.ends_with("/.default") {
        return fail(&mut s, "invalid_scope", "AADSTS1002012: The provided value for scope is not valid. Client credential flows must have a scope value with /.default suffixed.");
    }
    let key = (tenant.to_string(), get("client_id"));
    if !get("client_assertion_type").is_empty() {
        if get("client_assertion_type") != crate::azure::aad::JWT_BEARER {
            return fail(&mut s, "invalid_request", "AADSTS50027: Invalid client_assertion_type.");
        }
        let assertion = get("client_assertion");
        if !s.federated.get(&key).is_some_and(|t| t.iter().any(|x| ct_eq(x.as_bytes(), assertion.as_bytes()))) {
            return fail(&mut s, "invalid_client", "AADSTS70021: No matching federated identity record found for presented assertion.");
        }
    } else {
        match s.clients.get(&key) {
            None => return fail(&mut s, "unauthorized_client", "AADSTS700016: Application not found in the directory."),
            Some(secret) if !ct_eq(secret.as_bytes(), get("client_secret").as_bytes()) => return fail(&mut s, "invalid_client", "AADSTS7000215: Invalid client secret provided."),
            _ => {}
        }
    }
    let tok = super::issue_entra_token(&scope, &key.1, 3600);
    s.tokens.insert(tok.clone(), (scope, now_secs() + 3600));
    s.token_requests += 1;
    Response::json(200, obj([("token_type", Value::from("Bearer")), ("expires_in", Value::Int(3599)), ("access_token", Value::from(tok))]).to_json())
}

/// Managed identity tokens (IMDS with `Metadata: true`, or App Service with
/// `X-IDENTITY-HEADER`); `client_id` selects a registered user-assigned identity.
fn managed_identity_token(st: &Shared, req: &ServerRequest, app_service: bool) -> Response {
    let mut s = st.lock().unwrap();
    let bad = |status: u16, code: &str, msg: &str| Response::json(status, obj([("error", Value::from(code)), ("error_description", Value::from(msg))]).to_json());
    let api = req.query_param("api-version").unwrap_or_default();
    if app_service {
        if !req.header("X-IDENTITY-HEADER").is_some_and(|h| ct_eq(h.as_bytes(), s.identity_header.as_bytes())) {
            return bad(401, "unauthorized", "Missing or invalid X-IDENTITY-HEADER");
        }
        if api != "2019-08-01" {
            return bad(400, "invalid_request", "api-version 2019-08-01 is required");
        }
    } else {
        if req.header("Metadata") != Some("true") {
            return bad(400, "invalid_request", "Required metadata header not specified");
        }
        if api.is_empty() {
            return bad(400, "invalid_request", "Required query variable 'api-version' is missing");
        }
    }
    let resource = req.query_param("resource").unwrap_or_default();
    if resource.is_empty() {
        return bad(400, "invalid_request", "Required query variable 'resource' is missing");
    }
    let client = req.query_param("client_id").unwrap_or_default();
    if !client.is_empty() && !s.identities.contains(&client) {
        return bad(400, "invalid_request", "Identity not found");
    }
    s.identity_requests.push((if app_service { "appservice" } else { "imds" }.to_string(), resource.clone(), client.clone()));
    let scope = format!("{}/.default", resource.trim_end_matches('/'));
    let subject = if client.is_empty() { "system-assigned".to_string() } else { client.clone() };
    let tok = super::issue_entra_token(&scope, &subject, 3600);
    s.tokens.insert(tok.clone(), (scope, now_secs() + 3600));
    s.token_requests += 1;
    let exp = now_secs() + 3600;
    Response::json(200, obj([("access_token", Value::from(tok)), ("expires_in", Value::from("3599")), ("expires_on", Value::from(exp.to_string())), ("resource", Value::from(resource)), ("token_type", Value::from("Bearer")), ("client_id", Value::from(client))]).to_json())
}

fn bearer_ok(s: &State, req: &ServerRequest, scope: &str) -> bool {
    let tok = req.header("Authorization").and_then(|a| a.strip_prefix("Bearer ")).unwrap_or("");
    s.tokens.get(tok).is_some_and(|(sc, exp)| sc == scope && *exp > now_secs())
}

// ---------------------------------------------------------------------------
// Key Vault
// ---------------------------------------------------------------------------

fn kv_error(status: u16, code: &str, msg: &str) -> Response {
    Response::json(status, obj([("error", obj([("code", Value::from(code)), ("message", Value::from(msg))]))]).to_json())
}

fn bundle(base: &str, name: &str, v: &KvVersion) -> Value {
    obj([
        ("value", Value::from(v.value.clone())),
        ("id", Value::from(format!("{}/secrets/{}/{}", base, name, v.version))),
        ("attributes", obj([("enabled", Value::Bool(true)), ("created", Value::Int(v.created)), ("updated", Value::Int(v.created)), ("recoveryLevel", Value::from("Recoverable+Purgeable"))])),
    ])
}

fn key_vault(st: &Shared, req: &ServerRequest, segs: &[String]) -> Response {
    let mut s = st.lock().unwrap();
    if !bearer_ok(&s, req, "https://vault.azure.net/.default") {
        s.auth_failures += 1;
        return kv_error(401, "Unauthorized", "AKV10000: Request is missing a Bearer or PoP token.")
            .with_header("WWW-Authenticate", format!("Bearer authorization=\"https://login.microsoftonline.com/{}\", resource=\"https://vault.azure.net\"", TENANT_ID));
    }
    if req.query_param("api-version").is_none_or(|v| v.is_empty()) {
        return kv_error(400, "BadParameter", "The 'api-version' query parameter is missing.");
    }
    let base = format!("http://{}", req.header("Host").unwrap_or("localhost"));
    let s_ = segs.iter().map(|x| x.as_str()).collect::<Vec<_>>();
    let not_found = |name: &str| kv_error(404, "SecretNotFound", &format!("A secret with (name/id) {} was not found in this key vault.", name));
    match (req.method.as_str(), s_.as_slice()) {
        ("GET", ["secrets"]) => {
            let items: Vec<Value> = s.secrets.keys().map(|n| obj([("id", Value::from(format!("{}/secrets/{}", base, n))), ("attributes", obj([("enabled", Value::Bool(true))]))])).collect();
            Response::json(200, obj([("value", Value::Array(items)), ("nextLink", Value::Null)]).to_json())
        }
        ("PUT", ["secrets", name]) => {
            if s.deleted.contains_key(*name) {
                return kv_error(409, "Conflict", &format!("Secret {} is currently in a deleted but recoverable state.", name));
            }
            let Some(value) = parse_json(&req.body).ok().and_then(|v| v.get("value").map(|x| x.to_string())) else { return kv_error(400, "BadParameter", "Property 'value' is required.") };
            let v = KvVersion { version: random_hex(16), value, created: now_secs() };
            let b = bundle(&base, name, &v);
            s.secrets.entry(name.to_string()).or_default().push(v);
            Response::json(200, b.to_json())
        }
        ("GET", ["secrets", name]) | ("GET", ["secrets", name, ""]) => match s.secrets.get(*name).and_then(|v| v.last()) {
            Some(v) => Response::json(200, bundle(&base, name, v).to_json()),
            None => not_found(name),
        },
        ("GET", ["secrets", name, version]) => match s.secrets.get(*name).and_then(|vs| vs.iter().find(|v| v.version == *version)) {
            Some(v) => Response::json(200, bundle(&base, name, v).to_json()),
            None => not_found(name),
        },
        ("DELETE", ["secrets", name]) => match s.secrets.remove(*name) {
            Some(vs) => {
                let mut b = bundle(&base, name, vs.last().expect("non empty"));
                if let Value::Object(m) = &mut b {
                    m.insert("recoveryId".into(), Value::from(format!("{}/deletedsecrets/{}", base, name)));
                    m.insert("deletedDate".into(), Value::Int(now_secs()));
                    m.insert("scheduledPurgeDate".into(), Value::Int(now_secs() + 90 * 86400));
                    m.remove("value");
                }
                s.deleted.insert(name.to_string(), vs);
                Response::json(200, b.to_json())
            }
            None => not_found(name),
        },
        ("DELETE", ["deletedsecrets", name]) => match s.deleted.remove(*name) {
            Some(_) => Response::new(204),
            None => kv_error(404, "SecretNotFound", &format!("Deleted Secret not found: {}", name)),
        },
        ("POST", ["deletedsecrets", name, "recover"]) => match s.deleted.remove(*name) {
            Some(vs) => {
                let b = bundle(&base, name, vs.last().expect("non empty"));
                s.secrets.insert(name.to_string(), vs);
                Response::json(200, b.to_json())
            }
            None => kv_error(404, "SecretNotFound", &format!("Deleted Secret not found: {}", name)),
        },
        _ => kv_error(400, "BadParameter", "Unsupported Key Vault operation"),
    }
}

// ---------------------------------------------------------------------------
// Cosmos DB
// ---------------------------------------------------------------------------

fn cerr(status: u16, code: &str, msg: &str) -> Response {
    Response::json(status, obj([("code", Value::from(code)), ("message", Value::from(msg))]).to_json())
}

fn verify_master_key(req: &ServerRequest) -> Result<(), Response> {
    let unauthorized = |m: &str| cerr(401, "Unauthorized", m);
    let auth = percent_decode(req.header("authorization").unwrap_or(""), false);
    let parts: HashMap<&str, &str> = auth.split('&').filter_map(|p| p.split_once('=')).collect();
    if parts.get("type") == Some(&"aad") {
        // Entra ID token (any account scope or the generic Cosmos scope)
        let tok = parts.get("sig").copied().unwrap_or("");
        return match super::verify_entra_token(tok, "https://cosmos.azure.com/.default") {
            Some(_) => Ok(()),
            None => Err(unauthorized("The AAD token is invalid or expired.")),
        };
    }
    if parts.get("type") != Some(&"master") || parts.get("ver") != Some(&"1.0") {
        return Err(unauthorized("Required Header authorization is missing or not a master key token."));
    }
    let date = req.header("x-ms-date").unwrap_or("");
    if parse_http_date(date).is_none_or(|d| (now_secs() - d).abs() > 15 * 60) {
        return Err(cerr(403, "Forbidden", "The authorization token is not valid at the current time."));
    }
    if req.header("x-ms-version").is_none() {
        return Err(cerr(400, "BadRequest", "Required header x-ms-version is missing."));
    }
    let path: Vec<String> = req.segments();
    let (rtype, rlink) = resource_of(&path.join("/"));
    let expected = master_key_signature(COSMOS_KEY, &req.method, &rtype, &rlink, date).unwrap_or_default();
    if !ct_eq(expected.as_bytes(), parts.get("sig").unwrap_or(&"").as_bytes()) {
        return Err(unauthorized(&format!(
            "The input authorization token can't serve the request. The wrong key is being used or the expected payload is not built as per the protocol. Server used the following payload to sign: '{}\\n{}\\n{}\\n{}\\n\\n'",
            req.method.to_ascii_lowercase(),
            rtype,
            rlink,
            date.to_ascii_lowercase()
        )));
    }
    Ok(())
}

fn sys(mut v: Value, rid: &str, self_link: &str, lsn: u64) -> Value {
    if let Value::Object(m) = &mut v {
        m.insert("_rid".into(), Value::from(rid));
        m.insert("_self".into(), Value::from(self_link));
        m.insert("_etag".into(), Value::from(format!("\"{:016x}\"", lsn)));
        m.insert("_ts".into(), Value::Int(now_secs()));
        if lsn > 0 {
            m.insert("_lsn".into(), Value::Int(lsn as i64));
        }
    }
    v
}

fn db_doc(name: &str, d: &Db) -> Value {
    sys(obj([("id", Value::from(name))]), &d.rid, &format!("dbs/{}/", d.rid), 0)
}

fn coll_doc(name: &str, c: &Coll, db_rid: &str) -> Value {
    sys(
        obj([("id", Value::from(name)), ("partitionKey", obj([("paths", Value::from(vec![c.pk_path.clone()])), ("kind", Value::from("Hash"))]))]),
        &c.rid,
        &format!("dbs/{}/colls/{}/", db_rid, c.rid),
        0,
    )
}

fn pk_of(doc: &Value, path: &str) -> Value {
    let mut v = Some(doc);
    for seg in path.trim_start_matches('/').split('/') {
        v = v.and_then(|x| x.get(seg));
    }
    v.cloned().unwrap_or(Value::Null)
}

fn header_pk(req: &ServerRequest) -> Option<Value> {
    let h = req.header("x-ms-documentdb-partitionkey")?;
    parse_json(h.as_bytes()).ok().and_then(|v| v.as_array().and_then(|a| a.first().cloned()))
}

fn feed(field: &str, items: Vec<Value>) -> Response {
    let n = items.len();
    Response::json(200, obj([("_rid", Value::from("")), (field, Value::Array(items)), ("_count", Value::Int(n as i64))]).to_json())
}

fn is_query(req: &ServerRequest) -> bool {
    req.header("x-ms-documentdb-isquery").is_some_and(|v| v.eq_ignore_ascii_case("true")) || req.header("Content-Type").is_some_and(|c| c.starts_with("application/query+json"))
}

fn cosmos(st: &Shared, req: &ServerRequest, segs: &[String]) -> Response {
    let mut guard = st.lock().unwrap();
    let s = &mut *guard;
    if let Err(r) = verify_master_key(req) {
        s.auth_failures += 1;
        return r;
    }
    let p: Vec<&str> = segs.iter().map(|x| x.as_str()).collect();
    let body = parse_json(&req.body).unwrap_or_default();
    let query = || -> Result<Query, Response> {
        let text = body.get("query").map(|q| q.to_string()).unwrap_or_default();
        let params: HashMap<String, Value> = body.get("parameters").and_then(|a| a.as_array()).map(|a| a.iter().map(|x| (x.get("name").map(|n| n.to_string()).unwrap_or_default(), x.get("value").cloned().unwrap_or_default())).collect()).unwrap_or_default();
        Query::parse(&text, params).map_err(|e| cerr(400, "BadRequest", &format!("Syntax error: {}", e)))
    };
    let created = |v: Value| Response::json(201, v.to_json());
    if parse_aad(req) {
        s.cosmos_aad_requests += 1;
    }
    if let Some(c) = req.header("x-ms-consistency-level") {
        s.cosmos_consistency.push(c.to_string());
    }
    let minimal = req.header("Prefer").is_some_and(|p| p.contains("return=minimal"));
    if minimal {
        s.cosmos_minimal_writes += 1;
    }
    if let Some(r) = offers_op(s, req, &p, &body) {
        return r;
    }
    let mut pending_offer: Option<(String, String)> = None;
    let r = cosmos_inner(s, req, &p, &body, created, minimal, &mut pending_offer, query);
    if let Some((rid, link)) = pending_offer {
        if r.status == 201 {
            let _ = add_offer(s, req, &rid, &link);
        }
    }
    r
}

fn parse_aad(req: &ServerRequest) -> bool {
    percent_decode(req.header("authorization").unwrap_or(""), false).starts_with("type=aad")
}

/// Validate the offer headers of a create request.
fn offer_check(req: &ServerRequest) -> Option<Response> {
    if let Some(t) = req.header("x-ms-offer-throughput") {
        match t.trim().parse::<i64>() {
            Ok(n) if n >= 400 && n % 100 == 0 => {}
            _ => return Some(cerr(400, "BadRequest", "The offer throughput must be at least 400 RU/s and a multiple of 100.")),
        }
    }
    None
}

fn add_offer(s: &mut State, req: &ServerRequest, rid: &str, link: &str) -> Result<(), Response> {
    if let Some(r) = offer_check(req) {
        return Err(r);
    }
    let throughput = req.header("x-ms-offer-throughput").and_then(|t| t.trim().parse().ok());
    let autoscale_max = req.header("x-ms-cosmos-offer-autopilot-settings").and_then(|a| parse_json(a.as_bytes()).ok()).and_then(|v| v.get("maxThroughput").and_then(|m| m.as_i64()));
    if throughput.is_some() || autoscale_max.is_some() {
        s.offers.push(Offer { id: random_hex(4), resource_rid: rid.to_string(), resource_link: link.to_string(), throughput, autoscale_max });
    }
    Ok(())
}

fn offer_doc(o: &Offer) -> Value {
    let mut content = camel_core::value::ObjectMap::new();
    if let Some(t) = o.throughput {
        content.insert("offerThroughput".into(), Value::Int(t));
    }
    if let Some(m) = o.autoscale_max {
        content.insert("offerAutopilotSettings".into(), obj([("maxThroughput", Value::Int(m))]));
        content.insert("offerThroughput".into(), Value::Int(m / 10));
    }
    sys(
        obj([("id", Value::from(o.id.clone())), ("offerVersion", Value::from("V2")), ("offerType", Value::from("Invalid")), ("resource", Value::from(o.resource_link.clone())), ("offerResourceId", Value::from(o.resource_rid.clone())), ("content", Value::Object(content))]),
        &o.id,
        &format!("offers/{}/", o.id),
        0,
    )
}

/// The Offers API (`/offers`): list, query by `offerResourceId`, replace.
fn offers_op(s: &mut State, req: &ServerRequest, p: &[&str], body: &Value) -> Option<Response> {
    if p.first() != Some(&"offers") {
        return None;
    }
    Some(match (req.method.as_str(), p) {
        ("GET", ["offers"]) => feed("Offers", s.offers.iter().map(offer_doc).collect()),
        ("POST", ["offers"]) => {
            let rid = body.get("parameters").and_then(|a| a.as_array()).and_then(|a| a.iter().find(|x| x.get("name").map(|n| n.to_string()).as_deref() == Some("@rid"))).and_then(|x| x.get("value")).map(|v| v.to_string()).unwrap_or_default();
            feed("Offers", s.offers.iter().filter(|o| o.resource_rid == rid).map(offer_doc).collect())
        }
        ("GET", ["offers", id]) => match s.offers.iter().find(|o| o.id.eq_ignore_ascii_case(id)) {
            Some(o) => Response::json(200, offer_doc(o).to_json()),
            None => cerr(404, "NotFound", "Resource Not Found"),
        },
        ("PUT", ["offers", id]) => {
            let Some(o) = s.offers.iter_mut().find(|o| o.id.eq_ignore_ascii_case(id)) else { return Some(cerr(404, "NotFound", "Resource Not Found")) };
            let content = body.get("content").cloned().unwrap_or_default();
            let auto = content.get("offerAutopilotSettings").and_then(|a| a.get("maxThroughput")).and_then(|m| m.as_i64());
            let manual = content.get("offerThroughput").and_then(|t| t.as_i64());
            match (auto, manual) {
                (Some(m), _) if m >= 1000 && m % 1000 == 0 => {
                    o.autoscale_max = Some(m);
                    o.throughput = None;
                }
                (None, Some(t)) if t >= 400 && t % 100 == 0 => {
                    o.throughput = Some(t);
                    o.autoscale_max = None;
                }
                _ => return Some(cerr(400, "BadRequest", "Invalid offer content: throughput must be >= 400 and a multiple of 100 (autoscale: >= 1000, multiple of 1000).")),
            }
            Response::json(200, offer_doc(o).to_json())
        }
        _ => cerr(405, "MethodNotAllowed", "Method not allowed"),
    })
}

/// The partition key range of a document key.
fn range_of(pk_json: &str, ranges: usize) -> usize {
    (crate::util::md5(pk_json.as_bytes())[0] as usize) % ranges.max(1)
}

#[allow(clippy::too_many_arguments)]
fn cosmos_inner(s: &mut State, req: &ServerRequest, p: &[&str], body: &Value, created: impl Fn(Value) -> Response, minimal: bool, pending_offer: &mut Option<(String, String)>, query: impl Fn() -> Result<Query, Response>) -> Response {
    match (req.method.as_str(), p) {
        ("GET", ["dbs"]) => feed("Databases", s.dbs.iter().map(|(n, d)| db_doc(n, d)).collect()),
        ("POST", ["dbs"]) if is_query(req) => {
            let q = match query() {
                Ok(q) => q,
                Err(r) => return r,
            };
            feed("Databases", q.run(s.dbs.iter().map(|(n, d)| db_doc(n, d)).collect()))
        }
        ("POST", ["dbs"]) => {
            let id = body.get("id").map(|i| i.to_string()).unwrap_or_default();
            if id.is_empty() {
                return cerr(400, "BadRequest", "The input content is invalid because the required properties - 'id; ' - are missing");
            }
            if s.dbs.contains_key(&id) {
                return cerr(409, "Conflict", "Entity with the specified id already exists in the system.");
            }
            let d = Db { rid: random_hex(4), colls: BTreeMap::new() };
            if let Err(r) = add_offer(s, req, &d.rid, &format!("dbs/{}/", d.rid)) {
                return r;
            }
            let doc = db_doc(&id, &d);
            s.dbs.insert(id, d);
            created(doc)
        }
        (m, ["dbs", db]) => match (m, s.dbs.get(*db)) {
            (_, None) => cerr(404, "NotFound", "Resource Not Found"),
            ("GET", Some(d)) => Response::json(200, db_doc(db, d).to_json()),
            ("DELETE", Some(_)) => {
                s.dbs.remove(*db);
                Response::new(204)
            }
            _ => cerr(405, "MethodNotAllowed", "Method not allowed"),
        },
        (m, ["dbs", db, "colls"]) => {
            let Some(d) = s.dbs.get_mut(*db) else { return cerr(404, "NotFound", "Resource Not Found") };
            match m {
                "GET" => feed("DocumentCollections", d.colls.iter().map(|(n, c)| coll_doc(n, c, &d.rid)).collect()),
                "POST" if is_query(req) => {
                    let q = match query() {
                        Ok(q) => q,
                        Err(r) => return r,
                    };
                    feed("DocumentCollections", q.run(d.colls.iter().map(|(n, c)| coll_doc(n, c, &d.rid)).collect()))
                }
                "POST" => {
                    let id = body.get("id").map(|i| i.to_string()).unwrap_or_default();
                    let pk = body.get("partitionKey").and_then(|p| p.get("paths")).and_then(|p| p.as_array()).and_then(|a| a.first()).map(|p| p.to_string());
                    let Some(pk) = pk.filter(|p| p.starts_with('/')) else { return cerr(400, "BadRequest", "A partition key definition with a path is required.") };
                    if d.colls.contains_key(&id) {
                        return cerr(409, "Conflict", "Entity with the specified id already exists in the system.");
                    }
                    let c = Coll { rid: format!("{}{}", d.rid, random_hex(4)), pk_path: pk, lsn: 0, docs: BTreeMap::new(), ranges: 1 };
                    let (crid, link) = (c.rid.clone(), format!("dbs/{}/colls/{}/", d.rid, c.rid));
                    if let Some(r) = offer_check(req) {
                        return r;
                    }
                    *pending_offer = Some((crid, link));
                    let doc = coll_doc(&id, &c, &d.rid);
                    d.colls.insert(id, c);
                    created(doc)
                }
                _ => cerr(405, "MethodNotAllowed", "Method not allowed"),
            }
        }
        (m, ["dbs", db, "colls", coll, rest @ ..]) => {
            let Some(d) = s.dbs.get_mut(*db) else { return cerr(404, "NotFound", "Resource Not Found") };
            let db_rid = d.rid.clone();
            if rest.is_empty() && m == "DELETE" {
                return match d.colls.remove(*coll) {
                    Some(_) => Response::new(204),
                    None => cerr(404, "NotFound", "Resource Not Found"),
                };
            }
            let Some(c) = d.colls.get_mut(*coll) else { return cerr(404, "NotFound", "Resource Not Found") };
            match (m, rest) {
                ("GET", []) => Response::json(200, coll_doc(coll, c, &db_rid).to_json()),
                ("GET", ["pkranges"]) => feed("PartitionKeyRanges", (0..c.ranges).map(|i| obj([("id", Value::from(i.to_string())), ("minInclusive", Value::from(if i == 0 { String::new() } else { format!("{:02X}", i * 256 / c.ranges) })), ("maxExclusive", Value::from(if i + 1 == c.ranges { "FF".to_string() } else { format!("{:02X}", (i + 1) * 256 / c.ranges) }))])).collect()),
                ("GET", ["docs"]) if req.header("A-IM").is_some_and(|a| a.eq_ignore_ascii_case("Incremental feed")) => {
                    let since: Option<u64> = match req.header("If-None-Match") {
                        Some("*") => Some(c.lsn),
                        Some(e) => e.trim_matches('"').parse().ok(),
                        None => Some(0),
                    };
                    let since = since.unwrap_or(0);
                    let range: Option<usize> = req.header("x-ms-documentdb-partitionkeyrangeid").and_then(|r| r.parse().ok());
                    let n = c.ranges;
                    let mut changed: Vec<(u64, Value)> = c.docs.iter().filter(|((pk, _), (_, l))| *l > since && range.is_none_or(|r| range_of(pk, n) == r)).map(|(_, (d, l))| (*l, d.clone())).collect();
                    changed.sort_by_key(|(l, _)| *l);
                    let etag = format!("\"{}\"", changed.last().map(|(l, _)| *l).unwrap_or(since));
                    if changed.is_empty() {
                        return Response::new(304).with_header("etag", etag);
                    }
                    feed("Documents", changed.into_iter().map(|(_, d)| d).collect()).with_header("etag", etag)
                }
                ("GET", ["docs"]) => {
                    let pk = header_pk(req).map(|v| v.to_json());
                    feed("Documents", c.docs.iter().filter(|((k, _), _)| pk.as_ref().is_none_or(|p| p == k)).map(|(_, (d, _))| d.clone()).collect())
                }
                ("POST", ["docs"]) if is_query(req) => {
                    let q = match query() {
                        Ok(q) => q,
                        Err(r) => return r,
                    };
                    let pk = header_pk(req).map(|v| v.to_json());
                    let docs: Vec<Value> = c.docs.iter().filter(|((k, _), _)| pk.as_ref().is_none_or(|p| p == k)).map(|(_, (d, _))| d.clone()).collect();
                    feed("Documents", q.run(docs))
                }
                ("POST", ["docs"]) | ("PUT", ["docs", _]) => {
                    let Some(id) = body.get("id").map(|i| i.to_string()).filter(|i| !i.is_empty()) else {
                        return cerr(400, "BadRequest", "The input content is invalid because the required properties - 'id; ' - are missing");
                    };
                    let pk = pk_of(&body, &c.pk_path);
                    if header_pk(req).is_some_and(|h| h != pk) {
                        return cerr(400, "BadRequest", "PartitionKey extracted from document doesn't match the one specified in the header.");
                    }
                    let key = (pk.to_json(), id.clone());
                    let upsert = req.header("x-ms-documentdb-is-upsert").is_some_and(|v| v.eq_ignore_ascii_case("true"));
                    let exists = c.docs.contains_key(&key);
                    let status = match (m, exists, upsert) {
                        ("POST", true, false) => return cerr(409, "Conflict", "Entity with the specified id already exists in the system."),
                        ("POST", true, true) => 200,
                        ("POST", false, _) => 201,
                        (_, false, _) => return cerr(404, "NotFound", "Entity with the specified id does not exist in the system."),
                        _ => 200,
                    };
                    if let ("PUT", ["docs", url_id]) = (m, rest) {
                        if *url_id != id {
                            return cerr(400, "BadRequest", "The id in the URL does not match the id of the document.");
                        }
                    }
                    if let Some(want) = req.header("If-Match") {
                        let current = c.docs.get(&key).map(|(_, l)| format!("\"{:016x}\"", l));
                        if current.as_deref() != Some(want) {
                            return cerr(412, "PreconditionFailed", "Operation cannot be performed because one of the specified precondition is not met.");
                        }
                    }
                    c.lsn += 1;
                    let rid = format!("{}{}", c.rid, random_hex(4));
                    let doc = sys(body.clone(), &rid, &format!("dbs/{}/colls/{}/docs/{}/", db_rid, c.rid, rid), c.lsn);
                    c.docs.insert(key, (doc.clone(), c.lsn));
                    let etag = format!("\"{:016x}\"", c.lsn);
                    if minimal {
                        return Response::new(status).with_header("etag", etag).with_header("x-ms-request-charge", "6.29");
                    }
                    Response::json(status, doc.to_json()).with_header("etag", etag).with_header("x-ms-request-charge", "6.29")
                }
                ("GET" | "DELETE", ["docs", id]) => {
                    let Some(pk) = header_pk(req) else { return cerr(400, "BadRequest", "PartitionKey value must be supplied for this operation.") };
                    let key = (pk.to_json(), id.to_string());
                    if m == "GET" {
                        match c.docs.get(&key) {
                            Some((d, l)) => Response::json(200, d.to_json()).with_header("etag", format!("\"{:016x}\"", l)),
                            None => cerr(404, "NotFound", "Entity with the specified id does not exist in the system."),
                        }
                    } else {
                        match c.docs.remove(&key) {
                            Some(_) => Response::new(204),
                            None => cerr(404, "NotFound", "Entity with the specified id does not exist in the system."),
                        }
                    }
                }
                _ => cerr(400, "BadRequest", "Unsupported Cosmos DB operation"),
            }
        }
        _ => cerr(400, "BadRequest", "Unsupported Cosmos DB operation"),
    }
}

// ---------------------------------------------------------------------------
// SQL subset
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Word(String),
    Str(String),
    Num(f64),
    Param(String),
    Op(String),
}

fn tokenize(q: &str) -> Result<Vec<Tok>, String> {
    let c: Vec<char> = q.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() || ch == ',' {
            i += 1;
        } else if ch == '\'' || ch == '"' {
            let mut s = String::new();
            i += 1;
            while i < c.len() && c[i] != ch {
                s.push(c[i]);
                i += 1;
            }
            i += 1;
            out.push(Tok::Str(s));
        } else if ch == '@' || ch.is_alphabetic() || ch == '_' || ch == '*' {
            let start = i;
            i += 1;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_' || c[i] == '.' || c[i] == '@') {
                i += 1;
            }
            let w: String = c[start..i].iter().collect();
            out.push(if w.starts_with('@') { Tok::Param(w) } else { Tok::Word(w) });
        } else if ch.is_ascii_digit() || (ch == '-' && i + 1 < c.len() && c[i + 1].is_ascii_digit()) {
            let start = i;
            i += 1;
            while i < c.len() && (c[i].is_ascii_digit() || c[i] == '.') {
                i += 1;
            }
            let n: String = c[start..i].iter().collect();
            out.push(Tok::Num(n.parse().map_err(|_| format!("bad number {}", n))?));
        } else if "=<>!".contains(ch) {
            let start = i;
            i += 1;
            while i < c.len() && "=<>".contains(c[i]) {
                i += 1;
            }
            out.push(Tok::Op(c[start..i].iter().collect()));
        } else {
            return Err(format!("unexpected character '{}'", ch));
        }
    }
    Ok(out)
}

struct Cond {
    path: Vec<String>,
    op: String,
    value: Value,
}

struct Query {
    conds: Vec<Cond>,
    order: Option<(Vec<String>, bool)>,
    top: Option<usize>,
}

impl Query {
    fn parse(q: &str, params: HashMap<String, Value>) -> Result<Query, String> {
        let toks = tokenize(q)?;
        let kw = |t: Option<&Tok>, w: &str| matches!(t, Some(Tok::Word(x)) if x.eq_ignore_ascii_case(w));
        let mut i = 0;
        if !kw(toks.first(), "SELECT") {
            return Err("expected SELECT".into());
        }
        i += 1;
        let mut top = None;
        if kw(toks.get(i), "TOP") {
            if let Some(Tok::Num(n)) = toks.get(i + 1) {
                top = Some(*n as usize);
            }
            i += 2;
        }
        if !matches!(toks.get(i), Some(Tok::Word(w)) if w == "*") {
            return Err("only SELECT * is supported".into());
        }
        i += 1;
        if !kw(toks.get(i), "FROM") {
            return Err("expected FROM".into());
        }
        let mut alias = match toks.get(i + 1) {
            Some(Tok::Word(a)) => a.clone(),
            _ => return Err("expected a collection alias".into()),
        };
        i += 2;
        if kw(toks.get(i), "AS") {
            i += 1;
        }
        if let Some(Tok::Word(a)) = toks.get(i) {
            if !["WHERE", "ORDER"].iter().any(|k| a.eq_ignore_ascii_case(k)) {
                alias = a.clone();
                i += 1;
            }
        }
        let path_of = |w: &str| -> Result<Vec<String>, String> {
            let rest = w.strip_prefix(&format!("{}.", alias)).ok_or_else(|| format!("unknown identifier {}", w))?;
            Ok(rest.split('.').map(|s| s.to_string()).collect())
        };
        let mut conds = Vec::new();
        if kw(toks.get(i), "WHERE") {
            i += 1;
            loop {
                let path = match toks.get(i) {
                    Some(Tok::Word(w)) => path_of(w)?,
                    _ => return Err("expected a property".into()),
                };
                let op = match toks.get(i + 1) {
                    Some(Tok::Op(o)) => o.clone(),
                    _ => return Err("expected an operator".into()),
                };
                let value = match toks.get(i + 2) {
                    Some(Tok::Str(s)) => Value::from(s.clone()),
                    Some(Tok::Num(n)) if n.fract() == 0.0 => Value::Int(*n as i64),
                    Some(Tok::Num(n)) => Value::Float(*n),
                    Some(Tok::Param(p)) => params.get(p).cloned().ok_or_else(|| format!("parameter {} is not defined", p))?,
                    Some(Tok::Word(w)) if w.eq_ignore_ascii_case("true") => Value::Bool(true),
                    Some(Tok::Word(w)) if w.eq_ignore_ascii_case("false") => Value::Bool(false),
                    Some(Tok::Word(w)) if w.eq_ignore_ascii_case("null") => Value::Null,
                    _ => return Err("expected a value".into()),
                };
                conds.push(Cond { path, op, value });
                i += 3;
                if kw(toks.get(i), "AND") {
                    i += 1;
                    continue;
                }
                break;
            }
        }
        let mut order = None;
        if kw(toks.get(i), "ORDER") && kw(toks.get(i + 1), "BY") {
            let path = match toks.get(i + 2) {
                Some(Tok::Word(w)) => path_of(w)?,
                _ => return Err("expected an ORDER BY property".into()),
            };
            let desc = kw(toks.get(i + 3), "DESC");
            if desc || kw(toks.get(i + 3), "ASC") {
                i += 1;
            }
            i += 3;
            order = Some((path, desc));
        }
        if i < toks.len() {
            return Err(format!("unexpected token {:?}", toks[i]));
        }
        Ok(Query { conds, order, top })
    }

    fn run(&self, docs: Vec<Value>) -> Vec<Value> {
        let get = |d: &Value, path: &[String]| {
            let mut v = Some(d.clone());
            for p in path {
                v = v.and_then(|x| x.get(p).cloned());
            }
            v
        };
        let mut out: Vec<Value> = docs
            .into_iter()
            .filter(|d| {
                self.conds.iter().all(|c| {
                    let Some(v) = get(d, &c.path) else { return false };
                    let ord = camel_core::value::compare_values(&v, &c.value);
                    use std::cmp::Ordering::*;
                    match c.op.as_str() {
                        "=" => camel_core::value::values_equal(&v, &c.value),
                        "!=" | "<>" => !camel_core::value::values_equal(&v, &c.value),
                        "<" => ord == Some(Less),
                        ">" => ord == Some(Greater),
                        "<=" => matches!(ord, Some(Less | Equal)),
                        ">=" => matches!(ord, Some(Greater | Equal)),
                        _ => false,
                    }
                })
            })
            .collect();
        if let Some((path, desc)) = &self.order {
            out.sort_by(|a, b| {
                let o = match (get(a, path), get(b, path)) {
                    (Some(x), Some(y)) => camel_core::value::compare_values(&x, &y).unwrap_or(std::cmp::Ordering::Equal),
                    _ => std::cmp::Ordering::Equal,
                };
                if *desc {
                    o.reverse()
                } else {
                    o
                }
            });
        }
        if let Some(t) = self.top {
            out.truncate(t);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_subset() {
        let docs = vec![
            obj([("id", Value::from("1")), ("city", Value::from("Rome")), ("age", Value::Int(30))]),
            obj([("id", Value::from("2")), ("city", Value::from("Paris")), ("age", Value::Int(40))]),
            obj([("id", Value::from("3")), ("city", Value::from("Rome")), ("age", Value::Int(50))]),
        ];
        let mut p = HashMap::new();
        p.insert("@city".to_string(), Value::from("Rome"));
        let q = Query::parse("SELECT * FROM c WHERE c.city = @city AND c.age > 35 ORDER BY c.age DESC", p).unwrap();
        let r = q.run(docs.clone());
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].get("id").unwrap(), &Value::from("3"));
        let all = Query::parse("SELECT TOP 2 * FROM root r ORDER BY r.age", HashMap::new()).unwrap().run(docs);
        assert_eq!(all.iter().map(|d| d.get("id").unwrap().to_string()).collect::<Vec<_>>(), vec!["1", "2"]);
        assert!(Query::parse("SELECT c.id FROM c", HashMap::new()).is_err());
        assert!(Query::parse("SELECT * FROM c WHERE c.x = @missing", HashMap::new()).is_err());
    }
}

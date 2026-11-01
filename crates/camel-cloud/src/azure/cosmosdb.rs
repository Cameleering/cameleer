//! The `azure-cosmosdb` component (Camel `camel-azure-cosmosdb`) over the Cosmos DB SQL (Core)
//! REST API with master-key authorization.
//!
//! `azure-cosmosdb://databaseName[/containerName]?databaseEndpoint=https://acc.documents.azure.com:443/&accountKey=RAW(...)`
//!
//! Every request carries `authorization: type=master&ver=1.0&sig=...` where `sig` is the
//! base64 HMAC-SHA256 (account key) of `verb\nresourceType\nresourceLink\nx-ms-date\n\n`
//! (lower-cased verb, type and date), as the REST API specifies ([`master_key_signature`]).
//!
//! Producer operations (`operation` / `CamelAzureCosmosDbOperation`, default `listDatabases`):
//! `listDatabases`, `createDatabase`, `queryDatabases`, `deleteDatabase`, `createContainer`
//! (`containerPartitionKeyPath` / `CamelAzureCosmosDbContainerPartitionKeyPath`),
//! `listContainers`, `queryContainers`, `deleteContainer`, `createItem`, `upsertItem`,
//! `replaceItem` (body = item map, partition key `itemPartitionKey` /
//! `CamelAzureCosmosDbItemPartitionKey`), `deleteItem`, `readItem` (`itemId` /
//! `CamelAzureCosmosDbItemId`), `readAllItems`, `queryItems` (`query` / `CamelAzureCosmosDbQuery`,
//! parameters from the `CamelAzureCosmosDbQueryParameters` map, all result pages). Database /
//! container names can be overridden with `CamelAzureCosmosDbDatabaseName` /
//! `CamelAzureCosmosDbContainerName`. `createDatabaseIfNotExists` /
//! `createContainerIfNotExists` create them when the endpoint starts. Response headers:
//! `CamelAzureCosmosDbResourceId`, `CamelAzureCosmosDbEtag`, `CamelAzureCosmosDbStatusCode`,
//! `CamelAzureCosmosDbResponseHeaders`.
//!
//! Consumer: the change feed of the container (`A-IM: Incremental feed`) read per partition key
//! range; one exchange per non-empty batch with the list of changed items as the body. Starts
//! from now, or from the beginning with `changeFeedStartFromBeginning=true` (rust-camel).
//! Without `leaseContainerName` the continuations are kept in memory; with it the consumer is a
//! change feed processor storing one lease per partition key range in the lease container (see
//! [`super::cosmosdb_ops`]), balancing ranges between hosts.
//!
//! Throughput, Entra ID credentials and request options: see [`super::cosmosdb_ops`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camel_core::component::scheduled::ScheduledPollConfig;
use camel_core::context::CamelContext;
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::Processor;
use camel_core::value::Value;

use crate::endpoint::{operation, process, set_batch, unsupported, CloudService};
use crate::http::{HttpClient, Proxy, Request, Response, Url};
use crate::util::{base64_decode, cloud_error, header_str, hmac_sha256, http_date, kinds, now_secs, obj, parse_json, uri_encode, Opts};

/// REST API version sent in `x-ms-version`.
pub const API_VERSION: &str = "2018-12-31";

/// Header names (Camel `CosmosDbConstants`).
pub mod headers {
    pub const OPERATION: &str = "CamelAzureCosmosDbOperation";
    pub const DATABASE_NAME: &str = "CamelAzureCosmosDbDatabaseName";
    pub const CONTAINER_NAME: &str = "CamelAzureCosmosDbContainerName";
    pub const QUERY: &str = "CamelAzureCosmosDbQuery";
    pub const ITEM_PARTITION_KEY: &str = "CamelAzureCosmosDbItemPartitionKey";
    pub const ITEM_ID: &str = "CamelAzureCosmosDbItemId";
    pub const CONTAINER_PARTITION_KEY_PATH: &str = "CamelAzureCosmosDbContainerPartitionKeyPath";
    pub const RESOURCE_ID: &str = "CamelAzureCosmosDbResourceId";
    pub const E_TAG: &str = "CamelAzureCosmosDbEtag";
    pub const STATUS_CODE: &str = "CamelAzureCosmosDbStatusCode";
    pub const RESPONSE_HEADERS: &str = "CamelAzureCosmosDbResponseHeaders";
    /// rust-camel extension: query parameters (`@name` → value map).
    pub const QUERY_PARAMETERS: &str = "CamelAzureCosmosDbQueryParameters";
}

/// The master-key signature (base64) of a request.
pub fn master_key_signature(key_b64: &str, verb: &str, resource_type: &str, resource_link: &str, date: &str) -> Result<String> {
    let key = base64_decode(key_b64).ok_or_else(|| CamelError::illegal_argument("accountKey must be the base64 encoded Cosmos DB key"))?;
    let text = format!("{}\n{}\n{}\n{}\n\n", verb.to_ascii_lowercase(), resource_type.to_ascii_lowercase(), resource_link, date.to_ascii_lowercase());
    Ok(crate::util::base64_encode(&hmac_sha256(&key, text.as_bytes())))
}

/// The `authorization` header value for a signature.
pub fn auth_header(sig: &str) -> String {
    uri_encode(&format!("type=master&ver=1.0&sig={}", sig), false)
}

/// (resource type, resource link) of a decoded resource path such as `dbs/db/colls/c/docs`.
pub fn resource_of(path: &str) -> (String, String) {
    let segs: Vec<&str> = path.trim_matches('/').split('/').filter(|s| !s.is_empty()).collect();
    if segs.is_empty() {
        return (String::new(), String::new());
    }
    if segs.len() % 2 == 1 {
        (segs[segs.len() - 1].to_string(), segs[..segs.len() - 1].join("/"))
    } else {
        (segs[segs.len() - 2].to_string(), segs.join("/"))
    }
}

/// The Cosmos DB endpoint configuration and client.
pub struct CosmosService {
    pub endpoint: String,
    pub key: String,
    pub database: Option<String>,
    pub container: Option<String>,
    pub operation: Option<String>,
    pub query: Option<String>,
    pub item_id: Option<String>,
    pub item_partition_key: Option<String>,
    pub container_partition_key_path: Option<String>,
    pub create_database_if_not_exists: bool,
    pub create_container_if_not_exists: bool,
    pub start_from_beginning: bool,
    pub proxy: Proxy,
    pub trust_all_certificates: bool,
    pub scheduler: ScheduledPollConfig,
    continuations: Mutex<HashMap<String, String>>,
    /// Entra ID credential (`credentialType=AZURE_IDENTITY`), instead of the account key.
    pub aad: Option<super::aad::AadCredential>,
    /// `consistencyLevel` sent as `x-ms-consistency-level`.
    pub consistency_level: Option<String>,
    /// `contentResponseOnWriteEnabled` (false: `Prefer: return=minimal`).
    pub content_response_on_write: bool,
    /// `throughputProperties` of created databases / containers.
    pub throughput: Option<String>,
    pub lease: Option<super::cosmosdb_ops::LeaseConfig>,
    pub(crate) leases: Mutex<HashMap<String, super::cosmosdb_ops::Lease>>,
}

/// Factory for the component.
pub fn create(remaining: &str, o: &mut Opts) -> Result<Arc<dyn CloudService>> {
    let (db, container) = match remaining.split_once('/') {
        Some((d, c)) => (Some(d.to_string()).filter(|d| !d.is_empty()), Some(c.trim_matches('/').to_string()).filter(|c| !c.is_empty())),
        None => (Some(remaining.to_string()).filter(|d| !d.is_empty()), None),
    };
    let endpoint = o
        .get("databaseEndpoint")
        .or_else(|| o.get("uriEndpointOverride"))
        .ok_or_else(|| CamelError::illegal_argument("databaseEndpoint must be specified (https://<account>.documents.azure.com:443/)"))?;
    o.ignore(&["overrideEndpoint"]);
    let ct = o.string("credentialType", "SHARED_ACCOUNT_KEY").to_ascii_uppercase();
    let (key, aad) = match ct.as_str() {
        "SHARED_ACCOUNT_KEY" => {
            let key = o.get("accountKey").ok_or_else(|| CamelError::illegal_argument("accountKey must be specified"))?;
            if base64_decode(&key).is_none() {
                return Err(CamelError::illegal_argument("accountKey must be the base64 encoded Cosmos DB key"));
            }
            (key, None)
        }
        "AZURE_IDENTITY" => (String::new(), Some(super::aad_from_opts(o, true)?)),
        other => return Err(CamelError::new(camel_core::error::kinds::UNSUPPORTED_OPERATION, format!("credentialType {} is not supported (use SHARED_ACCOUNT_KEY or AZURE_IDENTITY)", other))),
    };
    let proxy = match o.get("proxyHost") {
        Some(h) => Proxy::Explicit(format!("http://{}:{}", h, o.string("proxyPort", "3128"))),
        None => {
            o.ignore(&["proxyPort"]);
            Proxy::Environment
        }
    };
    let s = CosmosService {
        endpoint: endpoint.trim_end_matches('/').to_string(),
        key,
        database: db.clone(),
        container,
        operation: o.get("operation"),
        query: o.get("query"),
        item_id: o.get("itemId"),
        item_partition_key: o.get("itemPartitionKey"),
        container_partition_key_path: o.get("containerPartitionKeyPath"),
        create_database_if_not_exists: o.bool("createDatabaseIfNotExists", false)?,
        create_container_if_not_exists: o.bool("createContainerIfNotExists", false)?,
        start_from_beginning: o.bool("changeFeedStartFromBeginning", false)?,
        trust_all_certificates: o.bool("trustAllCertificates", false)?,
        scheduler: ScheduledPollConfig::from_params(o.params, Duration::from_millis(1000), Duration::from_millis(500))?,
        continuations: Mutex::new(HashMap::new()),
        aad,
        consistency_level: o.get("consistencyLevel").map(|c| c.to_ascii_uppercase()),
        content_response_on_write: o.bool("contentResponseOnWriteEnabled", true)?,
        throughput: o.get("throughputProperties"),
        lease: super::cosmosdb_ops::LeaseConfig::from_opts(o, db.as_deref())?,
        leases: Mutex::new(HashMap::new()),
        proxy,
    };
    o.ignore(&[
        "multipleWriteRegionsEnabled",
        "preferredRegions",
        "readRequestsFallbackEnabled",
        "connectionSharingAcrossClientsEnabled",
        "clientTelemetryEnabled",
        "queryRequestOptions",
        "changeFeedProcessorOptions",
        "cosmosAsyncClient",
        "indexingPolicy",
    ]);
    Ok(Arc::new(s))
}

impl CosmosService {
    fn http(&self) -> HttpClient {
        HttpClient::new().with_proxy(self.proxy.clone()).trust_all(self.trust_all_certificates)
    }

    /// Sign and send a request for the decoded resource `path` (e.g. `dbs/db/colls/c/docs`).
    pub fn request(&self, method: &str, path: &str, body: Option<Value>, extra: &[(&str, String)]) -> Result<Response> {
        let encoded: Vec<String> = path.split('/').filter(|s| !s.is_empty()).map(|s| uri_encode(s, false)).collect();
        let url = Url::parse(&format!("{}/{}", self.endpoint, encoded.join("/")))?;
        let date = http_date(now_secs());
        let (rtype, rlink) = resource_of(path);
        let authorization = match &self.aad {
            Some(c) => uri_encode(&format!("type=aad&ver=1.0&sig={}", c.token(&self.aad_scope(), &self.http())?), false),
            None => auth_header(&master_key_signature(&self.key, method, &rtype, &rlink, &date)?),
        };
        let mut req = Request::new(method, url).header("x-ms-date", date).header("x-ms-version", API_VERSION).header("authorization", authorization).header("Accept", "application/json");
        if let Some(c) = &self.consistency_level {
            req.set_header("x-ms-consistency-level", c.clone());
        }
        if !self.content_response_on_write && matches!(method, "POST" | "PUT") && rtype == "docs" && !extra.iter().any(|(k, _)| k.eq_ignore_ascii_case("x-ms-documentdb-isquery")) {
            req.set_header("Prefer", "return=minimal");
        }
        if let Some(b) = body {
            req = req.header("Content-Type", "application/json").body(b.to_json().into_bytes());
        }
        for (k, v) in extra {
            req.set_header(k, v.clone());
        }
        let resp = self.http().send(&req)?;
        if resp.is_success() || resp.status == 304 {
            Ok(resp)
        } else {
            if let (Some(c), 401) = (&self.aad, resp.status) {
                c.invalidate(&self.aad_scope());
            }
            Err(cosmos_error(&resp))
        }
    }

    pub(crate) fn db(&self, ex: &Exchange) -> Result<String> {
        header_str(ex, headers::DATABASE_NAME).or_else(|| self.database.clone()).ok_or_else(|| CamelError::illegal_argument("Database name must be specified (path or CamelAzureCosmosDbDatabaseName)"))
    }

    pub(crate) fn coll(&self, ex: &Exchange) -> Result<String> {
        header_str(ex, headers::CONTAINER_NAME).or_else(|| self.container.clone()).ok_or_else(|| CamelError::illegal_argument("Container name must be specified (path or CamelAzureCosmosDbContainerName)"))
    }

    fn partition_key(&self, ex: &Exchange, item: Option<&Value>, db: &str, coll: &str) -> Result<Option<String>> {
        if let Some(pk) = ex.header(headers::ITEM_PARTITION_KEY).filter(|v| !v.is_null()).cloned().or_else(|| self.item_partition_key.clone().map(Value::from)) {
            return Ok(Some(Value::Array(vec![pk]).to_json()));
        }
        // derive it from the item and the container's partition key path
        if let Some(item) = item {
            let r = self.request("GET", &format!("dbs/{}/colls/{}", db, coll), None, &[])?;
            let c = parse_json(&r.body)?;
            if let Some(path) = c.get("partitionKey").and_then(|p| p.get("paths")).and_then(|p| p.as_array()).and_then(|a| a.first()).map(|p| p.to_string()) {
                let mut v = Some(item);
                for seg in path.trim_start_matches('/').split('/') {
                    v = v.and_then(|x| x.get(seg));
                }
                if let Some(v) = v {
                    return Ok(Some(Value::Array(vec![v.clone()]).to_json()));
                }
            }
        }
        Ok(None)
    }

    pub(crate) fn query_all(&self, path: &str, query: &str, params: Option<&Value>, extra: &[(&str, String)]) -> Result<Vec<Value>> {
        let parameters: Vec<Value> = match params {
            Some(Value::Object(m)) => m.iter().map(|(k, v)| obj([("name", Value::from(if k.starts_with('@') { k.clone() } else { format!("@{}", k) })), ("value", v.clone())])).collect(),
            _ => Vec::new(),
        };
        let body = obj([("query", Value::from(query)), ("parameters", Value::Array(parameters))]);
        let field = match resource_of(path).0.as_str() {
            "dbs" => "Databases",
            "colls" => "DocumentCollections",
            "offers" => "Offers",
            _ => "Documents",
        };
        let mut out = Vec::new();
        let mut continuation: Option<String> = None;
        loop {
            let mut h: Vec<(&str, String)> = vec![
                ("Content-Type", "application/query+json".into()),
                ("x-ms-documentdb-isquery", "True".into()),
                ("x-ms-documentdb-query-enablecrosspartition", "True".into()),
            ];
            h.extend(extra.iter().cloned());
            if let Some(c) = &continuation {
                h.push(("x-ms-continuation", c.clone()));
            }
            let resp = self.request("POST", path, Some(body.clone()), &h)?;
            let v = parse_json(&resp.body)?;
            out.extend(v.get(field).and_then(|d| d.as_array()).cloned().unwrap_or_default());
            continuation = resp.header("x-ms-continuation").map(|c| c.to_string()).filter(|c| !c.is_empty());
            if continuation.is_none() {
                return Ok(out);
            }
        }
    }

    pub(crate) fn list_feed(&self, path: &str, field: &str, extra: &[(&str, String)]) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut continuation: Option<String> = None;
        loop {
            let mut h: Vec<(&str, String)> = extra.to_vec();
            if let Some(c) = &continuation {
                h.push(("x-ms-continuation", c.clone()));
            }
            let resp = self.request("GET", path, None, &h)?;
            let v = parse_json(&resp.body)?;
            out.extend(v.get(field).and_then(|d| d.as_array()).cloned().unwrap_or_default());
            continuation = resp.header("x-ms-continuation").map(|c| c.to_string()).filter(|c| !c.is_empty());
            if continuation.is_none() {
                return Ok(out);
            }
        }
    }

    pub(crate) fn create_database(&self, db: &str, if_not_exists: bool) -> Result<Option<Response>> {
        self.create_database_with(db, if_not_exists, None)
    }

    pub(crate) fn create_database_with(&self, db: &str, if_not_exists: bool, throughput: Option<&Value>) -> Result<Option<Response>> {
        let h = self.throughput_headers(throughput)?;
        let h: Vec<(&str, String)> = h.iter().map(|(k, v)| (*k, v.clone())).collect();
        match self.request("POST", "dbs", Some(obj([("id", Value::from(db))])), &h) {
            Ok(r) => Ok(Some(r)),
            Err(e) if if_not_exists && super::error_code(&e).as_deref() == Some("Conflict") => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub(crate) fn create_container(&self, db: &str, coll: &str, pk_path: Option<String>, if_not_exists: bool) -> Result<Option<Response>> {
        self.create_container_with(db, coll, pk_path, if_not_exists, None)
    }

    pub(crate) fn create_container_with(&self, db: &str, coll: &str, pk_path: Option<String>, if_not_exists: bool, throughput: Option<&Value>) -> Result<Option<Response>> {
        let pk = pk_path.or_else(|| self.container_partition_key_path.clone()).unwrap_or_else(|| "/id".into());
        let pk = if pk.starts_with('/') { pk } else { format!("/{}", pk) };
        let body = obj([("id", Value::from(coll)), ("partitionKey", obj([("paths", Value::from(vec![pk])), ("kind", Value::from("Hash"))]))]);
        let h = self.throughput_headers(throughput)?;
        let h: Vec<(&str, String)> = h.iter().map(|(k, v)| (*k, v.clone())).collect();
        match self.request("POST", &format!("dbs/{}/colls", db), Some(body), &h) {
            Ok(r) => Ok(Some(r)),
            Err(e) if if_not_exists && super::error_code(&e).as_deref() == Some("Conflict") => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub(crate) fn set_response(ex: &mut Exchange, resp: &Response) {
        let v = parse_json(&resp.body).unwrap_or_default();
        if let Some(r) = v.get("_rid") {
            ex.set_header(headers::RESOURCE_ID, r.clone());
        }
        if let Some(e) = resp.header("etag").or_else(|| v.get("_etag").and_then(|e| e.as_str())) {
            ex.set_header(headers::E_TAG, e.to_string());
        }
        ex.set_header(headers::STATUS_CODE, resp.status as i64);
        ex.set_header(headers::RESPONSE_HEADERS, crate::util::string_map(resp.headers.iter().filter(|(k, _)| k.starts_with("x-ms-")).cloned()));
    }
}

/// Build the exception for a Cosmos DB error (`{"code", "message"}`).
pub fn cosmos_error(resp: &Response) -> CamelError {
    let v = parse_json(&resp.body).unwrap_or_default();
    let code = v.get("code").map(|c| c.to_string()).unwrap_or_else(|| format!("Http{}", resp.status));
    let msg = v.get("message").map(|m| m.to_string()).unwrap_or_else(|| resp.text());
    cloud_error(kinds::COSMOS, format!("Status code {}, \"{}\" (ErrorCode: {})", resp.status, msg.lines().next().unwrap_or(""), code))
}

fn item_body(ex: &Exchange) -> Result<Value> {
    match ex.body() {
        v @ Value::Object(_) => Ok(v.clone()),
        other => {
            let v = parse_json(&other.to_bytes())?;
            if v.as_object().is_none() {
                return Err(CamelError::illegal_argument("The item must be a map / JSON object"));
            }
            Ok(v)
        }
    }
}

impl CloudService for CosmosService {
    fn produce(&self, ex: &mut Exchange) -> Result<()> {
        let op = operation(ex, headers::OPERATION, &self.operation, "listDatabases");
        match op.as_str() {
            "listDatabases" => {
                let v = self.list_feed("dbs", "Databases", &[])?;
                ex.set_body(Value::Array(v));
            }
            "createDatabase" => {
                let tp = ex.header(super::cosmosdb_ops::headers::THROUGHPUT_PROPERTIES).cloned();
                if let Some(r) = self.create_database_with(&self.db(ex)?, self.create_database_if_not_exists, tp.as_ref())? {
                    Self::set_response(ex, &r);
                }
            }
            "queryDatabases" => {
                let q = header_str(ex, headers::QUERY).or_else(|| self.query.clone()).ok_or_else(|| CamelError::illegal_argument("queryDatabases requires a query"))?;
                let v = self.query_all("dbs", &q, ex.header(headers::QUERY_PARAMETERS), &[])?;
                ex.set_body(Value::Array(v));
            }
            "deleteDatabase" => {
                let r = self.request("DELETE", &format!("dbs/{}", self.db(ex)?), None, &[])?;
                Self::set_response(ex, &r);
            }
            "createContainer" => {
                let (db, coll) = (self.db(ex)?, self.coll(ex)?);
                if self.create_database_if_not_exists {
                    self.create_database(&db, true)?;
                }
                let tp = ex.header(super::cosmosdb_ops::headers::THROUGHPUT_PROPERTIES).cloned();
                if let Some(r) = self.create_container_with(&db, &coll, header_str(ex, headers::CONTAINER_PARTITION_KEY_PATH), self.create_container_if_not_exists, tp.as_ref())? {
                    Self::set_response(ex, &r);
                }
            }
            "listContainers" => {
                let v = self.list_feed(&format!("dbs/{}/colls", self.db(ex)?), "DocumentCollections", &[])?;
                ex.set_body(Value::Array(v));
            }
            "queryContainers" => {
                let q = header_str(ex, headers::QUERY).or_else(|| self.query.clone()).ok_or_else(|| CamelError::illegal_argument("queryContainers requires a query"))?;
                let v = self.query_all(&format!("dbs/{}/colls", self.db(ex)?), &q, ex.header(headers::QUERY_PARAMETERS), &[])?;
                ex.set_body(Value::Array(v));
            }
            "deleteContainer" => {
                let r = self.request("DELETE", &format!("dbs/{}/colls/{}", self.db(ex)?, self.coll(ex)?), None, &[])?;
                Self::set_response(ex, &r);
            }
            "createItem" | "upsertItem" | "replaceItem" => {
                let (db, coll) = (self.db(ex)?, self.coll(ex)?);
                let item = item_body(ex)?;
                let mut h: Vec<(&str, String)> = Vec::new();
                if let Some(pk) = self.partition_key(ex, Some(&item), &db, &coll)? {
                    h.push(("x-ms-documentdb-partitionkey", pk));
                }
                let r = if op == "replaceItem" {
                    let id = header_str(ex, headers::ITEM_ID).or_else(|| item.get("id").map(|i| i.to_string())).ok_or_else(|| CamelError::illegal_argument("replaceItem requires an item id"))?;
                    self.request("PUT", &format!("dbs/{}/colls/{}/docs/{}", db, coll, id), Some(item), &h)?
                } else {
                    if op == "upsertItem" {
                        h.push(("x-ms-documentdb-is-upsert", "True".into()));
                    }
                    self.request("POST", &format!("dbs/{}/colls/{}/docs", db, coll), Some(item), &h)?
                };
                Self::set_response(ex, &r);
                ex.set_body(if r.body.is_empty() { Value::Null } else { parse_json(&r.body)? });
            }
            "readItem" | "deleteItem" => {
                let (db, coll) = (self.db(ex)?, self.coll(ex)?);
                let id = header_str(ex, headers::ITEM_ID).or_else(|| self.item_id.clone()).ok_or_else(|| CamelError::illegal_argument("An item id must be specified (itemId / CamelAzureCosmosDbItemId)"))?;
                let mut h: Vec<(&str, String)> = Vec::new();
                if let Some(pk) = self.partition_key(ex, None, &db, &coll)? {
                    h.push(("x-ms-documentdb-partitionkey", pk));
                }
                let method = if op == "readItem" { "GET" } else { "DELETE" };
                let r = self.request(method, &format!("dbs/{}/colls/{}/docs/{}", db, coll, id), None, &h)?;
                Self::set_response(ex, &r);
                if op == "readItem" {
                    ex.set_body(parse_json(&r.body)?);
                }
            }
            "readAllItems" => {
                let (db, coll) = (self.db(ex)?, self.coll(ex)?);
                let mut h: Vec<(&str, String)> = Vec::new();
                if let Some(pk) = self.partition_key(ex, None, &db, &coll)? {
                    h.push(("x-ms-documentdb-partitionkey", pk));
                }
                let v = self.list_feed(&format!("dbs/{}/colls/{}/docs", db, coll), "Documents", &h)?;
                ex.set_body(Value::Array(v));
            }
            "queryItems" => {
                let (db, coll) = (self.db(ex)?, self.coll(ex)?);
                let q = header_str(ex, headers::QUERY).or_else(|| self.query.clone()).ok_or_else(|| CamelError::illegal_argument("queryItems requires a query (query option or CamelAzureCosmosDbQuery header)"))?;
                let mut h: Vec<(&str, String)> = Vec::new();
                if let Some(pk) = ex.header(headers::ITEM_PARTITION_KEY).filter(|v| !v.is_null()) {
                    h.push(("x-ms-documentdb-partitionkey", Value::Array(vec![pk.clone()]).to_json()));
                }
                let v = self.query_all(&format!("dbs/{}/colls/{}/docs", db, coll), &q, ex.header(headers::QUERY_PARAMETERS), &h)?;
                ex.set_body(Value::Array(v));
            }
            other => {
                if !self.more_ops(ex, other)? {
                    return Err(unsupported("azure-cosmosdb", other));
                }
            }
        }
        Ok(())
    }

    fn on_stop(&self) -> Result<()> {
        self.release_leases();
        Ok(())
    }

    fn on_start(&self) -> Result<()> {
        self.start_leases()?;
        if let Some(db) = &self.database {
            if self.create_database_if_not_exists {
                self.create_database(db, true)?;
            }
            if let (Some(c), true) = (&self.container, self.create_container_if_not_exists) {
                self.create_container(db, c, None, true)?;
            }
        }
        Ok(())
    }

    fn scheduler(&self) -> Option<&ScheduledPollConfig> {
        Some(&self.scheduler)
    }

    fn poll(&self, ctx: Option<CamelContext>, processor: &dyn Processor, logger: &str) -> Result<usize> {
        if self.lease.is_some() {
            return self.poll_with_leases(ctx, processor, logger);
        }
        let db = self.database.clone().ok_or_else(|| CamelError::illegal_argument("The change feed consumer requires a database name"))?;
        let coll = self.container.clone().ok_or_else(|| CamelError::illegal_argument("The change feed consumer requires a container name"))?;
        let ranges = self.list_feed(&format!("dbs/{}/colls/{}/pkranges", db, coll), "PartitionKeyRanges", &[])?;
        let mut batches: Vec<(String, Vec<Value>)> = Vec::new();
        for r in ranges {
            let id = r.get("id").map(|i| i.to_string()).unwrap_or_default();
            let cont = self.continuations.lock().unwrap().get(&id).cloned();
            let mut h: Vec<(&str, String)> = vec![("A-IM", "Incremental feed".into()), ("x-ms-documentdb-partitionkeyrangeid", id.clone())];
            match cont {
                Some(c) => h.push(("If-None-Match", c)),
                None if !self.start_from_beginning => h.push(("If-None-Match", "*".into())),
                None => {}
            }
            let resp = self.request("GET", &format!("dbs/{}/colls/{}/docs", db, coll), None, &h)?;
            if let Some(etag) = resp.header("etag") {
                self.continuations.lock().unwrap().insert(id.clone(), etag.to_string());
            }
            if resp.status == 304 {
                continue;
            }
            let docs = parse_json(&resp.body)?.get("Documents").and_then(|d| d.as_array()).cloned().unwrap_or_default();
            if !docs.is_empty() {
                batches.push((id, docs));
            }
        }
        let total = batches.len();
        for (i, (_range, docs)) in batches.into_iter().enumerate() {
            let mut ex = Exchange::new(ctx.clone());
            ex.set_header(headers::DATABASE_NAME, db.clone());
            ex.set_header(headers::CONTAINER_NAME, coll.clone());
            ex.set_body(Value::Array(docs));
            set_batch(&mut ex, i, total);
            process(logger, processor, &mut ex);
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_links() {
        assert_eq!(resource_of("dbs"), ("dbs".into(), "".into()));
        assert_eq!(resource_of("dbs/db1"), ("dbs".into(), "dbs/db1".into()));
        assert_eq!(resource_of("dbs/db1/colls/c1/docs"), ("docs".into(), "dbs/db1/colls/c1".into()));
        assert_eq!(resource_of("dbs/db1/colls/c1/docs/item 1"), ("docs".into(), "dbs/db1/colls/c1/docs/item 1".into()));
    }

    #[test]
    fn documented_signature_example() {
        // example of the "Access control in the Azure Cosmos DB SQL API" documentation
        let key = "dsZQi3KtZmCv1ljt3VNWNm7sQUF1y5rJfC6kv5JiwvW0EndXdDku/dkKBp8/ufDToSxLzR4y+O/0H/t4bQtVNw==";
        let sig = master_key_signature(key, "GET", "dbs", "dbs/ToDoList", "Thu, 27 Apr 2017 00:51:12 GMT").unwrap();
        assert_eq!(auth_header(&sig), "type%3Dmaster%26ver%3D1.0%26sig%3Dc09PEVJrgp2uQRkr934kFbTqhByc7TVr3OHyqlu%2Bc%2Bc%3D");
    }
}

//! CouchDB: the Camel `couchdb` component over the CouchDB REST API and an embedded test
//! server. (Couchbase is not implemented: it needs the binary memcached/KV protocol with
//! cluster maps and SASL, which is out of scope.)
//!
//! `couchdb:http://localhost:5984/mydb?createDatabase=true&username=admin&password=secret`
//!
//! - **Producer**: without the `CouchDbMethod` header the body (JSON text or object) is saved
//!   (`PUT /db/{_id}` when it has an `_id`, else `POST /db`; an `_rev` updates the document)
//!   and headers `CouchDbId` / `CouchDbRev` are set. `CouchDbMethod=GET` reads the document
//!   with id `CouchDbId` (or the body) into the body; `CouchDbMethod=DELETE` deletes the
//!   document identified by the body's `_id`/`_rev` (or the `CouchDbId`/`CouchDbRev` headers).
//!   Conflicts raise `DocumentConflictException`, missing documents `NoDocumentException`.
//! - **Consumer**: follows the `_changes` feed (`feed=longpoll`, `include_docs=true`) from the
//!   current update sequence (or `since`) and routes each change with body = the document and
//!   headers `CouchDbDatabase`, `CouchDbSeq`, `CouchDbId`, `CouchDbRev`, `CouchDbMethod`
//!   (`UPDATE` or `DELETE`). Options `updates` (true), `deletes` (true), `heartbeat` (longpoll
//!   timeout, 30000 ms), `style` (`main_only`/`all_docs`).
//!
//! `https://` urls use TLS (trust from `sslContextParameters=#bean`, the global
//! `camel.ssl.*` parameters, else the system trust store).

pub mod server;

use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

use camel_core::component::support::{process_consumer_exchange, BackgroundThreads};
use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::value::Value;
use camel_http::client::HttpClient;
use camel_http::http::HttpResponse;

use crate::kinds;
use crate::util::{body_to_json_value, header_str};

/// Header names (Camel `CouchDbConstants`).
pub mod headers {
    pub const DATABASE: &str = "CouchDbDatabase";
    pub const SEQ: &str = "CouchDbSeq";
    pub const ID: &str = "CouchDbId";
    pub const REV: &str = "CouchDbRev";
    pub const METHOD: &str = "CouchDbMethod";
}

/// The `couchdb` component.
#[derive(Default)]
pub struct CouchDbComponent;

/// Endpoint configuration.
#[derive(Clone)]
pub struct CouchDbConfig {
    /// `http(s)://host:port`
    pub base_url: String,
    pub database: String,
    pub create_database: bool,
    pub authorization: Option<String>,
    pub heartbeat: Duration,
    pub style: String,
    pub deletes: bool,
    pub updates: bool,
    pub since: Option<String>,
    pub client: HttpClient,
}

impl Component for CouchDbComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let (scheme, rest) = remaining.split_once("://").ok_or_else(|| CamelError::illegal_argument(format!("Invalid couchdb uri (http://host:port/db expected): {}", remaining)))?;
        let scheme = scheme.to_ascii_lowercase();
        if scheme != "http" && scheme != "https" {
            return Err(CamelError::illegal_argument(format!("Invalid couchdb protocol {}", scheme)));
        }
        let (authority, db) = rest.split_once('/').ok_or_else(|| CamelError::illegal_argument(format!("Missing database name in couchdb uri: {}", remaining)))?;
        let db = db.trim_matches('/');
        if db.is_empty() {
            return Err(CamelError::illegal_argument(format!("Missing database name in couchdb uri: {}", remaining)));
        }
        let authority = if authority.contains(':') { authority.to_string() } else { format!("{}:5984", authority) };
        let mut client = HttpClient::new();
        client.read_timeout = Duration::from_secs(60);
        let explicit = camel_tls::camel_support::take_endpoint_ssl(ctx, params, "couchdb")?;
        if scheme == "https" {
            let p = camel_tls::camel_support::effective_client_params(ctx, explicit.as_ref())?;
            client.tls = Some(Arc::new(p.client_config().map_err(|e| CamelError::illegal_argument(format!("TLS client configuration: {}", e)))?));
            client.tls_server_name = p.server_name.clone();
        }
        let authorization = params.take("username").map(|u| format!("Basic {}", camel_tls::pem::base64_encode(format!("{}:{}", u, params.take("password").unwrap_or_default()).as_bytes())));
        params.take("password");
        let style = params.take("style").unwrap_or_else(|| "main_only".into());
        if style != "main_only" && style != "all_docs" {
            return Err(CamelError::illegal_argument(format!("Invalid style {} (main_only or all_docs)", style)));
        }
        let cfg = CouchDbConfig {
            base_url: format!("{}://{}", scheme, authority),
            database: db.to_string(),
            create_database: params.take_bool("createDatabase", false)?,
            authorization,
            heartbeat: params.take_duration("heartbeat", Duration::from_millis(30_000))?,
            style,
            deletes: params.take_bool("deletes", true)?,
            updates: params.take_bool("updates", true)?,
            since: params.take("since"),
            client,
        };
        for k in ["lazyStartProducer", "bridgeErrorHandler", "exceptionHandler", "exchangePattern", "maxMessagesPerPoll"] {
            params.take(k);
        }
        Ok(Arc::new(CouchDbEndpoint { uri: uri.to_string(), ctx: ctx.downgrade(), cfg }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A `couchdb` endpoint.
pub struct CouchDbEndpoint {
    uri: String,
    ctx: WeakContext,
    cfg: CouchDbConfig,
}

impl CouchDbEndpoint {
    pub fn config(&self) -> &CouchDbConfig {
        &self.cfg
    }
}

impl Endpoint for CouchDbEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(CouchDbProducer { api: Api { cfg: self.cfg.clone() } }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        Ok(Arc::new(CouchDbConsumer { api: Api { cfg: self.cfg.clone() }, ctx: self.ctx.clone(), processor, logger: self.uri.clone(), threads: BackgroundThreads::new() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn enc(s: &str) -> String {
    camel_core::util::url_encode(s)
}

#[derive(Clone)]
struct Api {
    cfg: CouchDbConfig,
}

impl Api {
    fn request(&self, method: &str, path: &str, body: Option<&Value>, timeout: Option<Duration>) -> Result<(u16, Value)> {
        let url = format!("{}/{}{}", self.cfg.base_url, enc(&self.cfg.database), path);
        let mut headers: Vec<(&str, &str)> = vec![("Accept", "application/json")];
        if let Some(a) = &self.cfg.authorization {
            headers.push(("Authorization", a.as_str()));
        }
        let data = body.map(|b| b.to_json().into_bytes());
        if data.is_some() {
            headers.push(("Content-Type", "application/json"));
        }
        let r: HttpResponse = self.cfg.client.request(method, &url, &headers, data.as_deref(), timeout)?;
        let v = if r.body.is_empty() { Value::Null } else { camel_core::json::parse(&String::from_utf8_lossy(&r.body)).unwrap_or(Value::Null) };
        Ok((r.status, v))
    }

    fn check(&self, (status, v): (u16, Value), what: &str) -> Result<Value> {
        if (200..300).contains(&status) {
            return Ok(v);
        }
        let reason = format!("{} failed with status {}: {} ({})", what, status, v.get("error").map(|e| e.to_string()).unwrap_or_default(), v.get("reason").map(|e| e.to_string()).unwrap_or_default());
        Err(CamelError::new(
            match status {
                409 => kinds::COUCHDB_CONFLICT,
                404 => kinds::COUCHDB_NOT_FOUND,
                _ => kinds::COUCHDB,
            },
            reason,
        ))
    }

    fn ensure_database(&self) -> Result<()> {
        if !self.cfg.create_database {
            return Ok(());
        }
        let (status, v) = self.request("PUT", "", None, None)?;
        if status == 201 || status == 202 || status == 412 {
            return Ok(());
        }
        self.check((status, v), "Create database").map(|_| ())
    }
}

struct CouchDbProducer {
    api: Api,
}

impl Service for CouchDbProducer {
    fn start(&self) -> Result<()> {
        self.api.ensure_database()
    }
}

impl Processor for CouchDbProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let method = header_str(ex, headers::METHOD).map(|m| m.to_ascii_uppercase());
        match method.as_deref() {
            Some("GET") => {
                let id = header_str(ex, headers::ID).or_else(|| (!ex.body().is_null()).then(|| ex.body().to_string())).ok_or_else(|| CamelError::illegal_argument("couchdb GET needs the CouchDbId header"))?;
                let doc = self.api.check(self.api.request("GET", &format!("/{}", enc(&id)), None, None)?, "Get document")?;
                ex.set_header(headers::ID, id);
                if let Some(r) = doc.get("_rev") {
                    ex.set_header(headers::REV, r.clone());
                }
                ex.set_body(doc);
            }
            Some("DELETE") => {
                let body = if ex.body().is_null() { Value::Null } else { body_to_json_value(ex.body())? };
                let id = body.get("_id").map(|v| v.to_string()).or_else(|| header_str(ex, headers::ID)).ok_or_else(|| CamelError::illegal_argument("couchdb DELETE needs a document with _id (or the CouchDbId header)"))?;
                let rev = body.get("_rev").map(|v| v.to_string()).or_else(|| header_str(ex, headers::REV)).ok_or_else(|| CamelError::illegal_argument("couchdb DELETE needs a document with _rev (or the CouchDbRev header)"))?;
                let r = self.api.check(self.api.request("DELETE", &format!("/{}?rev={}", enc(&id), enc(&rev)), None, None)?, "Delete document")?;
                ex.set_header(headers::ID, id);
                ex.set_header(headers::REV, r.get("rev").cloned().unwrap_or(Value::Null));
            }
            Some("PUT") | Some("POST") | None => {
                let doc = body_to_json_value(ex.body())?;
                if !matches!(doc, Value::Object(_)) {
                    return Err(CamelError::new(camel_core::error::kinds::TYPE_CONVERSION, "couchdb: body must be a JSON object"));
                }
                let r = match doc.get("_id") {
                    Some(id) => self.api.request("PUT", &format!("/{}", enc(&id.to_string())), Some(&doc), None)?,
                    None => self.api.request("POST", "", Some(&doc), None)?,
                };
                let r = self.api.check(r, "Save document")?;
                ex.set_header(headers::ID, r.get("id").cloned().unwrap_or(Value::Null));
                ex.set_header(headers::REV, r.get("rev").cloned().unwrap_or(Value::Null));
            }
            Some(other) => return Err(CamelError::illegal_argument(format!("Unsupported CouchDbMethod {}", other))),
        }
        Ok(())
    }
}

struct CouchDbConsumer {
    api: Api,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
    logger: String,
    threads: BackgroundThreads,
}

impl Service for CouchDbConsumer {
    fn start(&self) -> Result<()> {
        self.api.ensure_database()?;
        // start from the current sequence (like Camel) unless `since` is given
        let since = match &self.api.cfg.since {
            Some(s) => s.clone(),
            None => {
                let info = self.api.check(self.api.request("GET", "", None, None)?, "Database info")?;
                info.get("update_seq").map(|s| s.to_string()).unwrap_or_else(|| "0".into())
            }
        };
        let (api, ctx, processor, logger) = (self.api.clone(), self.ctx.clone(), self.processor.clone(), self.logger.clone());
        self.threads.spawn("couchdb-changes", move |stop| {
            let mut since = since;
            let poll = api.cfg.heartbeat.min(Duration::from_millis(500)).max(Duration::from_millis(10));
            while !stop.is_stopped() {
                let path = format!("/_changes?feed=longpoll&include_docs=true&style={}&since={}&timeout={}", api.cfg.style, enc(&since), poll.as_millis());
                let res = api.request("GET", &path, None, Some(poll + Duration::from_secs(10))).and_then(|r| api.check(r, "Changes feed"));
                let feed = match res {
                    Ok(f) => f,
                    Err(e) => {
                        if !stop.is_stopped() {
                            camel_core::camel_warn!("couchdb", "Changes feed error: {}", e);
                            stop.sleep(Duration::from_millis(500));
                        }
                        continue;
                    }
                };
                for change in feed.get("results").and_then(|r| r.as_array()).cloned().unwrap_or_default() {
                    if stop.is_stopped() {
                        break;
                    }
                    let deleted = change.get("deleted").is_some_and(|d| d.is_truthy());
                    if let Some(s) = change.get("seq") {
                        since = s.to_string();
                    }
                    if (deleted && !api.cfg.deletes) || (!deleted && !api.cfg.updates) {
                        continue;
                    }
                    let mut ex = Exchange::new(ctx.upgrade());
                    ex.set_header(headers::DATABASE, api.cfg.database.clone());
                    ex.set_header(headers::SEQ, change.get("seq").cloned().unwrap_or(Value::Null));
                    ex.set_header(headers::ID, change.get("id").cloned().unwrap_or(Value::Null));
                    let rev = change.get("changes").and_then(|c| c.as_array()).and_then(|a| a.first()).and_then(|c| c.get("rev")).cloned().unwrap_or(Value::Null);
                    ex.set_header(headers::REV, rev);
                    ex.set_header(headers::METHOD, if deleted { "DELETE" } else { "UPDATE" });
                    ex.set_body(change.get("doc").cloned().unwrap_or(Value::Null));
                    process_consumer_exchange(&logger, processor.as_ref(), &mut ex);
                }
                if let Some(l) = feed.get("last_seq") {
                    since = l.to_string();
                }
            }
        })
    }
    fn stop(&self) -> Result<()> {
        self.threads.stop();
        Ok(())
    }
}

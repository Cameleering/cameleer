//! The `fhir:` component (Camel `camel-fhir`): a FHIR REST client.
//!
//! `fhir://<api>/<method>?serverUrl=http://host/fhir&fhirVersion=R4&encoding=JSON|XML`
//!
//! | api / method | parameters | result body |
//! |---|---|---|
//! | `create/resource` | `resource` (body) or `resourceAsString` | the created resource (server representation) |
//! | `read/resourceById` | `resourceClass` (or `type`), `id` / `stringId` (`Patient/1` also accepted), `version` | the resource |
//! | `read/resourceByUrl` | `url` (body) | the resource |
//! | `update/resource` | `resource` (body), optional `id` / `stringId` | the updated resource |
//! | `delete/resource` | `resource` (body) | the `OperationOutcome` (or null) |
//! | `delete/resourceById` | `type` / `resourceClass`, `id` / `stringId` | the `OperationOutcome` (or null) |
//! | `delete/resourceConditionalByUrl` | `url` (body) | the `OperationOutcome` (or null) |
//! | `search/searchByUrl` | `url` (body), e.g. `Patient?family=Chalmers` | a `searchset` Bundle |
//! | `history/onInstance` / `onType` / `onServer` | `id` / `resourceType` (or `resourceClass`), `count` | a `history` Bundle |
//! | `transaction/withBundle` / `withResources` | `bundle` (body) / `resources` (body, array) | the `transaction-response` Bundle (resp. the response entries' resources) |
//! | `capabilities/ofType` | | the `CapabilityStatement` |
//! | `load-page/next` / `previous` / `byUrl` | `bundle` (body) / `url` | the linked Bundle |
//!
//! Parameters come from endpoint options or `CamelFhir.<name>` headers (headers win); the
//! body supplies the method's main parameter (`inBody` overrides which). Resources may be
//! given as values or as FHIR JSON / XML text; results are resource values. Authentication:
//! `username` / `password` (basic) or `accessToken` (bearer). HTTP errors map to the HAPI
//! exceptions (`ResourceNotFoundException`, `ResourceGoneException`,
//! `InvalidRequestException`, `AuthenticationException`, `UnprocessableEntityException`...)
//! with the `OperationOutcome` diagnostics as message. The response `Location` and `ETag`
//! headers are copied to the exchange.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use camel_core::context::CamelContext;
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::{Component, Endpoint, Processor, Producer, Service};
use camel_core::util::{url_encode, Parameters};
use camel_core::value::Value;

use super::convert::{parse_json, tree_to_xml, xml_to_tree};
use super::dataformat::{body_tree, check_version};
use super::json::{self, J};
use super::*;
use crate::transport::Client;

/// Prefix of parameter headers.
pub const HEADER_PREFIX: &str = "CamelFhir.";

const METHODS: &[(&str, Option<&str>)] = &[
    ("create/resource", Some("resource")),
    ("read/resourceById", None),
    ("read/resourceByUrl", Some("url")),
    ("update/resource", Some("resource")),
    ("delete/resource", Some("resource")),
    ("delete/resourceById", None),
    ("delete/resourceConditionalByUrl", Some("url")),
    ("search/searchByUrl", Some("url")),
    ("history/onInstance", None),
    ("history/onType", None),
    ("history/onServer", None),
    ("transaction/withBundle", Some("bundle")),
    ("transaction/withResources", Some("resources")),
    ("capabilities/ofType", None),
    ("load-page/next", Some("bundle")),
    ("load-page/previous", Some("bundle")),
    ("load-page/byUrl", Some("url")),
];

const PARAMS: &[&str] = &["resource", "resourceAsString", "resourceClass", "type", "resourceType", "id", "stringId", "version", "url", "bundle", "resources", "count", "extraParameters", "preferReturn"];

/// The `fhir` component.
#[derive(Default)]
pub struct FhirComponent;

struct Config {
    method: String,
    in_body: Option<String>,
    server: String,
    xml: bool,
    pretty: bool,
    params: BTreeMap<String, String>,
    bearer: Option<String>,
    client: Client,
}

impl Component for FhirComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        register_kinds();
        let method = remaining.trim_matches('/').to_string();
        let default_in_body = METHODS
            .iter()
            .find(|(m, _)| m.eq_ignore_ascii_case(&method))
            .map(|(_, b)| *b)
            .ok_or_else(|| CamelError::illegal_argument(format!("fhir: unsupported api/method '{}'; supported: {}", method, METHODS.iter().map(|m| m.0).collect::<Vec<_>>().join(", "))))?;
        let canonical = METHODS.iter().find(|(m, _)| m.eq_ignore_ascii_case(&method)).map(|m| m.0.to_string()).unwrap_or(method);
        let server = params.take("serverUrl").ok_or_else(|| CamelError::illegal_argument("fhir: serverUrl is required"))?;
        check_version(params.take("fhirVersion").as_deref())?;
        let xml = match params.take("encoding").map(|e| e.to_ascii_uppercase()) {
            None => false,
            Some(e) if e == "JSON" => false,
            Some(e) if e == "XML" => true,
            Some(e) => return Err(CamelError::illegal_argument(format!("fhir: unknown encoding {} (JSON, XML)", e))),
        };
        let mut p = BTreeMap::new();
        for k in PARAMS {
            if let Some(v) = params.take(k) {
                p.insert(k.to_string(), v);
            }
        }
        if let Some(t) = params.take("socketTimeout") {
            params.insert("receiveTimeout", t);
        }
        let bearer = params.take("accessToken");
        let client = Client::from_params(ctx, "fhir", params)?;
        let cfg = Config {
            method: canonical,
            in_body: params.take("inBody").or_else(|| default_in_body.map(str::to_string)),
            server: server.trim_end_matches('/').to_string(),
            xml,
            pretty: params.take_bool("prettyPrint", false)?,
            params: p,
            bearer,
            client,
        };
        for k in ["log", "compress", "summary", "sessionCookie", "validationMode", "clientFactory", "client", "deferModelScanning", "forceConformanceCheck", "lazyStartProducer", "proxyHost", "proxyPort", "proxyUser", "proxyPassword", "serverUrlPrefix", "encodingStyle", "lazyStartProducer"] {
            params.take(k);
        }
        Ok(Arc::new(FhirEndpoint { uri: uri.to_string(), cfg: Arc::new(cfg) }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A fhir endpoint (producer only).
pub struct FhirEndpoint {
    uri: String,
    cfg: Arc<Config>,
}

impl Endpoint for FhirEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(FhirProducer { cfg: self.cfg.clone() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct FhirProducer {
    cfg: Arc<Config>,
}

impl Service for FhirProducer {}

fn status_kind(status: u16) -> &'static str {
    match status {
        400 => INVALID_REQUEST_EXCEPTION,
        401 => AUTHENTICATION_EXCEPTION,
        403 => FORBIDDEN_OPERATION_EXCEPTION,
        404 => RESOURCE_NOT_FOUND_EXCEPTION,
        405 => METHOD_NOT_ALLOWED_EXCEPTION,
        409 => RESOURCE_VERSION_CONFLICT_EXCEPTION,
        410 => RESOURCE_GONE_EXCEPTION,
        412 => PRECONDITION_FAILED_EXCEPTION,
        422 => UNPROCESSABLE_ENTITY_EXCEPTION,
        s if s >= 500 => INTERNAL_ERROR_EXCEPTION,
        _ => BASE_SERVER_RESPONSE_EXCEPTION,
    }
}

impl FhirProducer {
    fn param(&self, ex: &Exchange, name: &str) -> Option<Value> {
        if let Some(v) = ex.message.headers.get(&format!("{}{}", HEADER_PREFIX, name)).filter(|v| !v.is_null()) {
            return Some(v.clone());
        }
        if self.cfg.in_body.as_deref() == Some(name) && !ex.message.body.is_null() {
            return Some(ex.message.body.clone());
        }
        self.cfg.params.get(name).map(|s| Value::String(s.clone()))
    }

    fn param_str(&self, ex: &Exchange, names: &[&str]) -> Option<String> {
        names.iter().find_map(|n| self.param(ex, n)).map(|v| v.to_string()).filter(|s| !s.is_empty())
    }

    fn resource(&self, ex: &Exchange) -> Result<J> {
        let v = self.param(ex, "resource").or_else(|| self.param(ex, "resourceAsString")).ok_or_else(|| CamelError::illegal_argument(format!("fhir {}: missing resource", self.cfg.method)))?;
        body_tree(&v)
    }

    fn url(&self, rel: &str) -> String {
        if rel.starts_with("http://") || rel.starts_with("https://") {
            rel.to_string()
        } else {
            format!("{}/{}", self.cfg.server, rel.trim_start_matches('/'))
        }
    }

    fn encode(&self, j: &J) -> Result<(Vec<u8>, &'static str)> {
        Ok(if self.cfg.xml { (tree_to_xml(j, self.cfg.pretty)?.into_bytes(), "application/fhir+xml; charset=UTF-8") } else { (json::write(j, self.cfg.pretty).into_bytes(), "application/fhir+json; charset=UTF-8") })
    }

    fn call(&self, ex: &mut Exchange, method: &str, url: &str, body: Option<&J>) -> Result<Option<J>> {
        let mut headers = vec![("Accept".to_string(), if self.cfg.xml { "application/fhir+xml".to_string() } else { "application/fhir+json".to_string() })];
        if let Some(t) = &self.cfg.bearer {
            headers.push(("Authorization".into(), format!("Bearer {}", t)));
        }
        let payload = match body {
            Some(j) => {
                let (b, ct) = self.encode(j)?;
                headers.push(("Content-Type".into(), ct.into()));
                headers.push(("Prefer".into(), self.cfg.params.get("preferReturn").map(|p| format!("return={}", p)).unwrap_or_else(|| "return=representation".into())));
                Some(b)
            }
            None => None,
        };
        let resp = self.cfg.client.send(method, url, &headers, payload.as_deref())?;
        for h in ["Location", "Content-Location", "ETag", "Last-Modified"] {
            if let Some(v) = resp.headers.get(h) {
                ex.message.headers.set(h, v.to_string());
            }
        }
        ex.message.headers.set("CamelHttpResponseCode", resp.status as i64);
        let text = resp.body_text();
        let parsed = if text.trim().is_empty() {
            None
        } else if resp.content_type().map(|c| c.contains("xml")).unwrap_or(false) || text.trim_start().starts_with('<') {
            xml_to_tree(&text).ok()
        } else {
            parse_json(&text).ok()
        };
        if resp.status >= 400 {
            let diag = parsed
                .as_ref()
                .and_then(|j| j.get("issue"))
                .and_then(|i| match i {
                    J::Arr(a) => a.first().and_then(|x| x.get("diagnostics")).and_then(J::as_str).map(str::to_string),
                    _ => None,
                })
                .unwrap_or_else(|| text.clone());
            return Err(CamelError::new(status_kind(resp.status), format!("HTTP {} {}: {}", resp.status, resp.reason_phrase(), diag)));
        }
        Ok(parsed)
    }

    fn split_id(&self, ex: &Exchange) -> Result<(String, String)> {
        let id = self.param_str(ex, &["id", "stringId"]).ok_or_else(|| CamelError::illegal_argument(format!("fhir {}: missing id", self.cfg.method)))?;
        let id = id.split("/_history/").next().unwrap_or(&id).to_string();
        if let Some((t, i)) = id.rsplit_once('/') {
            let t = t.rsplit('/').next().unwrap_or(t);
            return Ok((t.to_string(), i.to_string()));
        }
        let ty = self.param_str(ex, &["resourceClass", "type", "resourceType"]).ok_or_else(|| CamelError::illegal_argument(format!("fhir {}: missing resourceClass", self.cfg.method)))?;
        Ok((ty.rsplit('.').next().unwrap_or(&ty).to_string(), id))
    }

    fn type_param(&self, ex: &Exchange) -> Result<String> {
        let t = self.param_str(ex, &["resourceType", "resourceClass", "type"]).ok_or_else(|| CamelError::illegal_argument(format!("fhir {}: missing resourceType", self.cfg.method)))?;
        Ok(t.rsplit('.').next().unwrap_or(&t).to_string())
    }

    fn count_suffix(&self, ex: &Exchange) -> String {
        self.param_str(ex, &["count"]).map(|c| format!("?_count={}", url_encode(&c))).unwrap_or_default()
    }

    fn link(&self, ex: &Exchange, rel: &str) -> Result<String> {
        let b = self.param(ex, "bundle").ok_or_else(|| CamelError::illegal_argument("fhir load-page: missing bundle"))?;
        let j = body_tree(&b)?;
        if let Some(J::Arr(links)) = j.get("link") {
            for l in links {
                if l.get("relation").and_then(J::as_str) == Some(rel) {
                    if let Some(u) = l.get("url").and_then(J::as_str) {
                        return Ok(u.to_string());
                    }
                }
            }
        }
        Err(CamelError::illegal_argument(format!("Bundle has no '{}' link", rel)))
    }
}

impl Processor for FhirProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let result: Option<J> = match self.cfg.method.as_str() {
            "create/resource" => {
                let r = self.resource(ex)?;
                let ty = r.get("resourceType").and_then(J::as_str).unwrap_or("").to_string();
                self.call(ex, "POST", &self.url(&ty), Some(&r))?
            }
            "read/resourceById" => {
                let (ty, id) = self.split_id(ex)?;
                let mut u = format!("{}/{}", ty, id);
                if let Some(v) = self.param_str(ex, &["version"]) {
                    u.push_str(&format!("/_history/{}", v));
                }
                self.call(ex, "GET", &self.url(&u), None)?
            }
            "read/resourceByUrl" | "load-page/byUrl" => {
                let u = self.param_str(ex, &["url"]).ok_or_else(|| CamelError::illegal_argument("fhir: missing url"))?;
                self.call(ex, "GET", &self.url(&u), None)?
            }
            "update/resource" => {
                let r = self.resource(ex)?;
                let ty = r.get("resourceType").and_then(J::as_str).unwrap_or("").to_string();
                let id = match self.param_str(ex, &["id", "stringId"]) {
                    Some(i) => i.rsplit('/').next().unwrap_or(&i).to_string(),
                    None => r.get("id").and_then(J::as_str).map(str::to_string).ok_or_else(|| CamelError::illegal_argument("fhir update: the resource has no id"))?,
                };
                let r = match r {
                    J::Obj(mut m) => {
                        match m.iter_mut().find(|(k, _)| k == "id") {
                            Some(e) => e.1 = J::Str(id.clone()),
                            None => m.insert(1.min(m.len()), ("id".into(), J::Str(id.clone()))),
                        }
                        J::Obj(m)
                    }
                    other => other,
                };
                self.call(ex, "PUT", &self.url(&format!("{}/{}", ty, id)), Some(&r))?
            }
            "delete/resource" => {
                let r = self.resource(ex)?;
                let ty = r.get("resourceType").and_then(J::as_str).unwrap_or("");
                let id = r.get("id").and_then(J::as_str).ok_or_else(|| CamelError::illegal_argument("fhir delete: the resource has no id"))?;
                self.call(ex, "DELETE", &self.url(&format!("{}/{}", ty, id)), None)?
            }
            "delete/resourceById" => {
                let (ty, id) = self.split_id(ex)?;
                self.call(ex, "DELETE", &self.url(&format!("{}/{}", ty, id)), None)?
            }
            "delete/resourceConditionalByUrl" => {
                let u = self.param_str(ex, &["url"]).ok_or_else(|| CamelError::illegal_argument("fhir: missing url"))?;
                self.call(ex, "DELETE", &self.url(&u), None)?
            }
            "search/searchByUrl" => {
                let u = self.param_str(ex, &["url"]).ok_or_else(|| CamelError::illegal_argument("fhir search: missing url"))?;
                self.call(ex, "GET", &self.url(&u), None)?
            }
            "history/onInstance" => {
                let (ty, id) = self.split_id(ex)?;
                let u = format!("{}/{}/_history{}", ty, id, self.count_suffix(ex));
                self.call(ex, "GET", &self.url(&u), None)?
            }
            "history/onType" => {
                let ty = self.type_param(ex)?;
                let u = format!("{}/_history{}", ty, self.count_suffix(ex));
                self.call(ex, "GET", &self.url(&u), None)?
            }
            "history/onServer" => {
                let u = format!("_history{}", self.count_suffix(ex));
                self.call(ex, "GET", &self.url(&u), None)?
            }
            "transaction/withBundle" => {
                let b = self.param(ex, "bundle").ok_or_else(|| CamelError::illegal_argument("fhir transaction: missing bundle"))?;
                let j = body_tree(&b)?;
                self.call(ex, "POST", &self.cfg.server.clone(), Some(&j))?
            }
            "transaction/withResources" => {
                let rs = self.param(ex, "resources").ok_or_else(|| CamelError::illegal_argument("fhir transaction: missing resources"))?;
                let items = match rs {
                    Value::Array(a) => a,
                    other => vec![other],
                };
                let mut entries = Vec::new();
                for r in &items {
                    let j = body_tree(r)?;
                    let ty = j.get("resourceType").and_then(J::as_str).unwrap_or("").to_string();
                    let request = match j.get("id").and_then(J::as_str) {
                        Some(id) => J::Obj(vec![("method".into(), J::Str("PUT".into())), ("url".into(), J::Str(format!("{}/{}", ty, id)))]),
                        None => J::Obj(vec![("method".into(), J::Str("POST".into())), ("url".into(), J::Str(ty))]),
                    };
                    entries.push(J::Obj(vec![("resource".into(), j), ("request".into(), request)]));
                }
                let bundle = J::Obj(vec![("resourceType".into(), J::Str("Bundle".into())), ("type".into(), J::Str("transaction".into())), ("entry".into(), J::Arr(entries))]);
                let resp = self.call(ex, "POST", &self.cfg.server.clone(), Some(&bundle))?;
                let list: Vec<Value> = match resp.as_ref().and_then(|r| r.get("entry")) {
                    Some(J::Arr(es)) => es.iter().map(|e| e.get("resource").map(J::to_value).unwrap_or(Value::Null)).collect(),
                    _ => Vec::new(),
                };
                ex.message.body = Value::Array(list);
                return Ok(());
            }
            "capabilities/ofType" => self.call(ex, "GET", &self.url("metadata"), None)?,
            "load-page/next" => {
                let u = self.link(ex, "next")?;
                self.call(ex, "GET", &self.url(&u), None)?
            }
            "load-page/previous" => {
                let u = self.link(ex, "previous").or_else(|_| self.link(ex, "prev"))?;
                self.call(ex, "GET", &self.url(&u), None)?
            }
            other => return Err(CamelError::illegal_argument(format!("fhir: unsupported method {}", other))),
        };
        ex.message.body = result.map(|j| j.to_value()).unwrap_or(Value::Null);
        Ok(())
    }
}

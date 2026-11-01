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
//! | `operation/onServer` / `onType` / `onInstance` / `onInstanceVersion` | `name` (`$everything`), `parameters` (body: a `Parameters` resource, or an object of name → value), `resourceType` / `id` (`Patient/1`, with `version` for `onInstanceVersion`), `useHttpGet`, `returnType` | the operation's response resource (`returnType=Parameters`... is informative) |
//! | `operation/processMessage` | `bundle` (body), `respondToUri`, `asynchronous` | the response Bundle |
//! | `patch/withBody` / `withUrl` | `patchBody` (body: JSON Patch array or text, XML Patch text, or a FHIRPath Patch `Parameters` resource), `id` / `stringId` resp. `url` (conditional), `preferReturn` | the patched resource (or the `OperationOutcome`) |
//! | `meta/getFromServer` / `getFromType` / `getFromResource` | `resourceType` / `id` | the `Meta` |
//! | `meta/add` / `meta/delete` | `meta` (body: a `Meta` value) and `id` | the resulting `Meta` |
//! | `validate/resource` | `resource` (body), optional `profile` (a `StructureDefinition`: JSON/XML file path, `classpath:`, `#bean` or text) | an `OperationOutcome` (`CamelFhirValidationSuccessful` header); without `profile` the server's `$validate` is called, with it the resource is validated locally ([`super::validation`]) |
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

use super::convert::{parse_json, tree_to_xml_v, xml_to_tree_v};
use super::dataformat::{body_tree_v, check_version};
use super::model::FhirVersion;
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
    ("operation/onServer", Some("parameters")),
    ("operation/onType", Some("parameters")),
    ("operation/onInstance", Some("parameters")),
    ("operation/onInstanceVersion", Some("parameters")),
    ("operation/processMessage", Some("bundle")),
    ("patch/withBody", Some("patchBody")),
    ("patch/withUrl", Some("patchBody")),
    ("meta/getFromServer", None),
    ("meta/getFromType", None),
    ("meta/getFromResource", None),
    ("meta/add", Some("meta")),
    ("meta/delete", Some("meta")),
    ("validate/resource", Some("resource")),
];

const PARAMS: &[&str] = &[
    "resource", "resourceAsString", "resourceClass", "type", "resourceType", "id", "stringId", "version", "url", "bundle", "resources", "count", "extraParameters", "preferReturn", "name",
    "parameters", "outputParameterType", "returnType", "useHttpGet", "respondToUri", "asynchronous", "patchBody", "meta", "metaType", "profile",
];

/// Header set by `validate/resource`: whether the outcome has no errors.
pub const VALIDATION_SUCCESSFUL: &str = "CamelFhirValidationSuccessful";

/// The `fhir` component.
#[derive(Default)]
pub struct FhirComponent;

struct Config {
    method: String,
    version: FhirVersion,
    ctx: CamelContext,
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
        let version = check_version(params.take("fhirVersion").as_deref())?;
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
            version,
            ctx: ctx.clone(),
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
        Ok(Arc::new(FhirProducer { cfg: self.cfg.clone(), last_error_body: std::sync::Mutex::new(None) }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct FhirProducer {
    cfg: Arc<Config>,
    /// The parsed body of the last error response (for `validate`).
    last_error_body: std::sync::Mutex<Option<J>>,
}

/// Load a `StructureDefinition` from a parameter value: a resource value, JSON / XML text,
/// `#bean` / `ref:` registry entries, `classpath:` / `file:` resources or a file path.
pub fn load_profile(ctx: &CamelContext, p: &Value, v: FhirVersion) -> Result<J> {
    let text = match p {
        Value::Object(_) => return body_tree_v(p, v),
        Value::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
        other => other.to_string(),
    };
    let t = text.trim();
    if t.starts_with('{') || t.starts_with('<') {
        return body_tree_v(&Value::String(text), v);
    }
    if let Some(name) = t.strip_prefix('#').or_else(|| t.strip_prefix("ref:")).or_else(|| t.strip_prefix("bean:")) {
        let bean = ctx.registry().lookup::<Value>(name).map(|b| (*b).clone()).or_else(|| ctx.registry().lookup::<String>(name).map(|s| Value::String((*s).clone())));
        return match bean {
            Some(b) => load_profile(ctx, &b, v),
            None => Err(CamelError::illegal_argument(format!("fhir: no profile bean '{}' in the registry", name))),
        };
    }
    let path = t.strip_prefix("classpath:").or_else(|| t.strip_prefix("file:")).unwrap_or(t);
    let content = std::fs::read_to_string(path).map_err(|e| CamelError::new("IOException", format!("Cannot read profile {}: {}", path, e)))?;
    body_tree_v(&Value::String(content), v)
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
        body_tree_v(&v, self.cfg.version)
    }

    /// A `Parameters` resource from the `parameters` parameter: a Parameters resource (value
    /// or text), or an object of name → value (strings → `valueString`, numbers →
    /// `valueInteger` / `valueDecimal`, booleans → `valueBoolean`, resources → `resource`).
    fn parameters(&self, ex: &Exchange) -> Result<Option<J>> {
        let Some(p) = self.param(ex, "parameters") else { return Ok(None) };
        if let Value::Object(o) = &p {
            if !o.contains_key("resourceType") {
                let mut list = Vec::new();
                for (k, v) in o {
                    let items = match v {
                        Value::Array(a) => a.clone(),
                        other => vec![other.clone()],
                    };
                    for it in items {
                        let entry = match &it {
                            Value::Object(r) if r.contains_key("resourceType") => ("resource".to_string(), body_tree_v(&it, self.cfg.version)?),
                            Value::Bool(b) => ("valueBoolean".to_string(), J::Bool(*b)),
                            Value::Int(i) => ("valueInteger".to_string(), J::Num(i.to_string())),
                            Value::Float(_) => ("valueDecimal".to_string(), J::from_value(&it)),
                            other => ("valueString".to_string(), J::Str(other.to_string())),
                        };
                        list.push(J::Obj(vec![("name".into(), J::Str(k.clone())), entry]));
                    }
                }
                return Ok(Some(J::Obj(vec![("resourceType".into(), J::Str("Parameters".into())), ("parameter".into(), J::Arr(list))])));
            }
        }
        if p.is_null() {
            return Ok(None);
        }
        body_tree_v(&p, self.cfg.version).map(Some)
    }

    fn operation_name(&self, ex: &Exchange) -> Result<String> {
        let n = self.param_str(ex, &["name"]).ok_or_else(|| CamelError::illegal_argument(format!("fhir {}: missing operation name", self.cfg.method)))?;
        Ok(if n.starts_with('$') { n } else { format!("${}", n) })
    }

    fn operation(&self, ex: &mut Exchange, prefix: String) -> Result<Option<J>> {
        let name = self.operation_name(ex)?;
        let params = self.parameters(ex)?;
        let get = self.param_str(ex, &["useHttpGet"]).map(|s| s.eq_ignore_ascii_case("true")).unwrap_or(false);
        let target = if prefix.is_empty() { name } else { format!("{}/{}", prefix, name) };
        if get {
            let mut q = Vec::new();
            if let Some(J::Arr(ps)) = params.as_ref().and_then(|p| p.get("parameter")) {
                for p in ps {
                    let n = p.get("name").and_then(J::as_str).unwrap_or("");
                    let J::Obj(m) = p else { continue };
                    if let Some((_, v)) = m.iter().find(|(k, _)| k.starts_with("value")) {
                        let text = match v {
                            J::Str(s) | J::Num(s) => s.clone(),
                            J::Bool(b) => b.to_string(),
                            _ => return Err(CamelError::illegal_argument(format!("fhir operation: parameter {} cannot be sent with HTTP GET", n))),
                        };
                        q.push(format!("{}={}", url_encode(n), url_encode(&text)));
                    }
                }
            }
            let u = if q.is_empty() { target } else { format!("{}?{}", target, q.join("&")) };
            self.call(ex, "GET", &self.url(&u), None)
        } else {
            let body = params.unwrap_or_else(|| J::Obj(vec![("resourceType".into(), J::Str("Parameters".into()))]));
            self.call(ex, "POST", &self.url(&target), Some(&body))
        }
    }

    fn instance_path(&self, ex: &Exchange) -> Result<String> {
        let (ty, id) = self.split_id(ex)?;
        Ok(format!("{}/{}", ty, id))
    }

    fn patch(&self, ex: &mut Exchange, target: &str) -> Result<Option<J>> {
        let body = self.param(ex, "patchBody").ok_or_else(|| CamelError::illegal_argument("fhir patch: missing patchBody"))?;
        let (payload, ct) = match &body {
            Value::Array(_) => (body.to_json(), "application/json-patch+json"),
            Value::Object(o) if o.contains_key("resourceType") => {
                let j = body_tree_v(&body, self.cfg.version)?;
                let (b, ct) = self.encode(&j)?;
                (String::from_utf8_lossy(&b).into_owned(), ct)
            }
            other => {
                let t = match other {
                    Value::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
                    o => o.to_string(),
                };
                let tt = t.trim_start();
                if tt.starts_with('[') {
                    (t, "application/json-patch+json")
                } else if tt.starts_with('<') {
                    if tt.contains("http://hl7.org/fhir") && tt.contains("<Parameters") {
                        (t, "application/fhir+xml; charset=UTF-8")
                    } else {
                        (t, "application/xml-patch+xml")
                    }
                } else {
                    (t, "application/fhir+json; charset=UTF-8")
                }
            }
        };
        self.send(ex, "PATCH", &self.url(target), Some((payload.into_bytes(), ct.to_string())))
    }

    fn meta_param(&self, ex: &Exchange) -> Result<J> {
        let m = self.param(ex, "meta").ok_or_else(|| CamelError::illegal_argument(format!("fhir {}: missing meta", self.cfg.method)))?;
        let j = match &m {
            Value::String(s) if s.trim_start().starts_with('{') => json::parse(s)?,
            other => J::from_value(other),
        };
        Ok(j)
    }

    fn meta_result(resp: Option<J>) -> Option<J> {
        let r = resp?;
        if r.get("resourceType").and_then(J::as_str) == Some("Parameters") {
            if let Some(J::Arr(ps)) = r.get("parameter") {
                for p in ps {
                    if let Some(m) = p.get("valueMeta") {
                        return Some(m.clone());
                    }
                }
            }
            return Some(J::Obj(Vec::new()));
        }
        Some(r)
    }

    fn validate(&self, ex: &mut Exchange) -> Result<Option<J>> {
        let r = self.resource(ex)?;
        let outcome = match self.param(ex, "profile").filter(|p| !p.is_null()) {
            Some(p) => {
                let profile = load_profile(&self.cfg.ctx, &p, self.cfg.version)?;
                let mut issues = super::validation::validate_base(&r, self.cfg.version);
                issues.extend(super::validation::validate_profile(&r, &profile));
                ex.message.headers.set(VALIDATION_SUCCESSFUL, super::validation::is_successful(&issues));
                Some(super::validation::outcome(&issues))
            }
            None => {
                let ty = r.get("resourceType").and_then(J::as_str).unwrap_or("").to_string();
                let params = J::Obj(vec![("resourceType".into(), J::Str("Parameters".into())), ("parameter".into(), J::Arr(vec![J::Obj(vec![("name".into(), J::Str("resource".into())), ("resource".into(), r)])]))]);
                let resp = match self.call(ex, "POST", &self.url(&format!("{}/$validate", ty)), Some(&params)) {
                    Ok(r) => r,
                    // HAPI returns the OperationOutcome of a failed validation in the error
                    Err(e) if e.kind == UNPROCESSABLE_ENTITY_EXCEPTION || e.kind == INVALID_REQUEST_EXCEPTION || e.kind == PRECONDITION_FAILED_EXCEPTION => {
                        ex.message.headers.set(VALIDATION_SUCCESSFUL, false);
                        return Ok(self.last_error_body.lock().unwrap().take());
                    }
                    Err(e) => return Err(e),
                };
                let ok = match resp.as_ref().and_then(|o| o.get("issue")) {
                    Some(J::Arr(is)) => !is.iter().any(|i| matches!(i.get("severity").and_then(J::as_str), Some("error") | Some("fatal"))),
                    _ => true,
                };
                ex.message.headers.set(VALIDATION_SUCCESSFUL, ok);
                resp
            }
        };
        Ok(outcome)
    }

    fn url(&self, rel: &str) -> String {
        if rel.starts_with("http://") || rel.starts_with("https://") {
            rel.to_string()
        } else {
            format!("{}/{}", self.cfg.server, rel.trim_start_matches('/'))
        }
    }

    fn encode(&self, j: &J) -> Result<(Vec<u8>, &'static str)> {
        Ok(if self.cfg.xml { (tree_to_xml_v(j, self.cfg.pretty, self.cfg.version)?.into_bytes(), "application/fhir+xml; charset=UTF-8") } else { (json::write(j, self.cfg.pretty).into_bytes(), "application/fhir+json; charset=UTF-8") })
    }

    fn call(&self, ex: &mut Exchange, method: &str, url: &str, body: Option<&J>) -> Result<Option<J>> {
        let payload = match body {
            Some(j) => {
                let (b, ct) = self.encode(j)?;
                Some((b, ct.to_string()))
            }
            None => None,
        };
        self.send(ex, method, url, payload)
    }

    fn send(&self, ex: &mut Exchange, method: &str, url: &str, payload: Option<(Vec<u8>, String)>) -> Result<Option<J>> {
        let mut headers = vec![("Accept".to_string(), if self.cfg.xml { "application/fhir+xml".to_string() } else { "application/fhir+json".to_string() })];
        if let Some(t) = &self.cfg.bearer {
            headers.push(("Authorization".into(), format!("Bearer {}", t)));
        }
        let payload = match payload {
            Some((b, ct)) => {
                headers.push(("Content-Type".into(), ct));
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
            xml_to_tree_v(&text, self.cfg.version).ok()
        } else {
            parse_json(&text).ok()
        };
        if resp.status >= 400 {
            *self.last_error_body.lock().unwrap() = parsed.clone();
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
        let j = body_tree_v(&b, self.cfg.version)?;
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
                let j = body_tree_v(&b, self.cfg.version)?;
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
                    let j = body_tree_v(r, self.cfg.version)?;
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
            "operation/onServer" => self.operation(ex, String::new())?,
            "operation/onType" => {
                let ty = self.type_param(ex)?;
                self.operation(ex, ty)?
            }
            "operation/onInstance" => {
                let p = self.instance_path(ex)?;
                self.operation(ex, p)?
            }
            "operation/onInstanceVersion" => {
                let mut p = self.instance_path(ex)?;
                let v = self.param_str(ex, &["version"]).ok_or_else(|| CamelError::illegal_argument("fhir operation/onInstanceVersion: missing version"))?;
                p.push_str(&format!("/_history/{}", v));
                self.operation(ex, p)?
            }
            "operation/processMessage" => {
                let b = self.param(ex, "bundle").ok_or_else(|| CamelError::illegal_argument("fhir processMessage: missing bundle"))?;
                let j = body_tree_v(&b, self.cfg.version)?;
                let mut q = Vec::new();
                if let Some(r) = self.param_str(ex, &["respondToUri"]) {
                    q.push(format!("response-url={}", url_encode(&r)));
                }
                if let Some(a) = self.param_str(ex, &["asynchronous"]) {
                    q.push(format!("async={}", url_encode(&a)));
                }
                let u = if q.is_empty() { "$process-message".to_string() } else { format!("$process-message?{}", q.join("&")) };
                self.call(ex, "POST", &self.url(&u), Some(&j))?
            }
            "patch/withBody" => {
                let p = self.instance_path(ex)?;
                self.patch(ex, &p)?
            }
            "patch/withUrl" => {
                let u = self.param_str(ex, &["url"]).ok_or_else(|| CamelError::illegal_argument("fhir patch/withUrl: missing url"))?;
                self.patch(ex, &u)?
            }
            "meta/getFromServer" => Self::meta_result(self.call(ex, "GET", &self.url("$meta"), None)?),
            "meta/getFromType" => {
                let ty = self.type_param(ex)?;
                Self::meta_result(self.call(ex, "GET", &self.url(&format!("{}/$meta", ty)), None)?)
            }
            "meta/getFromResource" => {
                let p = self.instance_path(ex)?;
                Self::meta_result(self.call(ex, "GET", &self.url(&format!("{}/$meta", p)), None)?)
            }
            "meta/add" | "meta/delete" => {
                let meta = self.meta_param(ex)?;
                let p = self.instance_path(ex)?;
                let op = if self.cfg.method == "meta/add" { "$meta-add" } else { "$meta-delete" };
                let params = J::Obj(vec![("resourceType".into(), J::Str("Parameters".into())), ("parameter".into(), J::Arr(vec![J::Obj(vec![("name".into(), J::Str("meta".into())), ("valueMeta".into(), meta)])]))]);
                Self::meta_result(self.call(ex, "POST", &self.url(&format!("{}/{}", p, op)), Some(&params))?)
            }
            "validate/resource" => self.validate(ex)?,
            other => return Err(CamelError::illegal_argument(format!("fhir: unsupported method {}", other))),

        };
        ex.message.body = result.map(|j| j.to_value()).unwrap_or(Value::Null);
        Ok(())
    }
}

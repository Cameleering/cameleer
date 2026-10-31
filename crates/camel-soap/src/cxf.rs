//! The `cxf:` component (Camel `camel-cxf-soap`): SOAP web service producer and consumer.
//!
//! ```text
//! cxf://http://host:port/path?wsdlURL=file:calc.wsdl&serviceName={ns}Svc&portName={ns}Port
//!     &dataFormat=POJO|PAYLOAD|RAW|CXF_MESSAGE&defaultOperationName=Add
//! cxf:/path?...            (consumer on the platform port: camel.component.cxf.port / camel.server.port / 8080)
//! ```
//!
//! **Producer.** The operation comes from the `operationName` header (else
//! `defaultOperationName`), its namespace from `operationNamespace` (else
//! `defaultOperationNamespace`, the WSDL), the SOAP action from the `SOAPAction` header (else
//! the binding). The address is the uri address, the `address` option or the WSDL port's
//! `soap:address`. Data formats:
//!
//! * `POJO` (default) — the body is the parameter list (`Array`), a parameter object or a
//!   single value, mapped to the document/literal wrapper (or rpc wrapper, or bare parts)
//!   per WSDL / XSD with typed values; the response becomes the return value (or an object
//!   for several out parameters). See [`crate::pojo`]. Without WSDL, a structural wrapper
//!   `{operationNamespace}operationName` with `arg0..n` is sent.
//! * `PAYLOAD` — the body is the XML of the body element(s) (a string, or an array of
//!   strings); the response body element(s) come back the same way.
//! * `RAW` / `CXF_MESSAGE` — the body is the whole envelope, sent and returned as is (no
//!   fault mapping, no WS-Security processing).
//!
//! A SOAP fault response fails the exchange with a `SoapFault` error (message = fault
//! string) and sets the headers `CamelCxfFaultCode`, `CamelCxfFaultString` and
//! `CamelCxfFaultDetail`. Extra request SOAP headers can be given in `CamelCxfSoapHeaders`
//! (XML string or array); response headers are returned in it.
//!
//! **Consumer.** Hosts the service on camel-http's server, accepts SOAP 1.1 and 1.2 (replies
//! in the request's version), dispatches by SOAP action (`SOAPAction` header or the 1.2
//! `action` content-type parameter) or by the body element (document: part element; rpc:
//! operation name) and sets `operationName`, `operationNamespace`, `SOAPAction`,
//! `CamelHttpMethod`, `CamelHttpPath` and `CamelCxfSoapHeaders`. The body follows the data
//! format (POJO: the typed parameter list). Exceptions become SOAP faults (`Server`/
//! `Receiver`; `Client`/`Sender` for `IllegalArgumentException`; a [`SoapFault`] raised with
//! [`SoapFault::into_error`] keeps its code and detail). One-way operations answer `202`.
//! `GET <path>?wsdl` returns the WSDL with the address rewritten (`publishedEndpointUrl`).
//! `schemaValidationEnabled=true` validates request bodies against the WSDL schema.
//!
//! **WS-Security.** Producer: `wsSecurityUsername`, `wsSecurityPassword`,
//! `wsSecurityPasswordType=PasswordText|PasswordDigest` (also `properties.ws-security.username`,
//! `properties.ws-security.password`, `properties.ws-security.password-type`); signing with
//! `wsSecuritySignatureKey` (PEM private key) and optional `wsSecuritySignatureCertificate`.
//! Consumer: `wsSecurityUsers=alice:secret,bob:pw` (or a `#bean` holding such a string /
//! `Map`) requires a valid UsernameToken; `wsSecuritySignatureVerifyCertificate` requires a
//! valid envelope signature. Other options: `soapVersion=1.1|1.2` (producer without
//! WSDL), `username`/`password` (HTTP basic auth), `sslContextParameters`, `receiveTimeout`,
//! `connectionTimeout`, `attributePrefix` (POJO attribute keys, default `@`), `loggingFeatureEnabled`
//! and a few other accepted-and-ignored Camel options. MTOM (`mtomEnabled=true`) is not supported.

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{CamelError, Result};
use camel_core::message::{Exchange, ExchangePattern};
use camel_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::value::Value;
use camel_http::registry::RouteHandler;
use camel_http::{HttpRequest, HttpResponse};
use camel_tls::keys::PrivateKey;
use camel_tls::x509::Certificate;
use camel_xml::dom::Document;

use crate::envelope::{build_envelope, build_fault, parse_envelope, parse_envelope_doc, request_action, SoapFault, SoapVersion};
use crate::pojo::{self, Shape};
use crate::support::{load_text, parse_clark, split_fragments, Key};
use crate::transport::{http_failed, Binding, Client, ListenAddress};
use crate::wsdl::{OperationInfo, Wsdl};
use crate::wssec::{self, PasswordType, SecurityPolicy};
use crate::xsdmodel::{BindOptions, SchemaSet};

/// Header: operation name.
pub const OPERATION_NAME: &str = "operationName";
/// Header: operation namespace.
pub const OPERATION_NAMESPACE: &str = "operationNamespace";
/// Header: SOAP action.
pub const SOAP_ACTION: &str = "SOAPAction";
/// Header: SOAP header blocks (XML strings).
pub const SOAP_HEADERS: &str = "CamelCxfSoapHeaders";
/// Header: fault code of a received fault.
pub const FAULT_CODE: &str = "CamelCxfFaultCode";
/// Header: fault string of a received fault.
pub const FAULT_STRING: &str = "CamelCxfFaultString";
/// Header: fault detail XML of a received fault.
pub const FAULT_DETAIL: &str = "CamelCxfFaultDetail";

/// Message data format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CxfDataFormat {
    Pojo,
    Payload,
    Raw,
    CxfMessage,
}

impl CxfDataFormat {
    fn parse(s: &str) -> Result<CxfDataFormat> {
        Ok(match s.trim().to_ascii_uppercase().as_str() {
            "POJO" => CxfDataFormat::Pojo,
            "PAYLOAD" => CxfDataFormat::Payload,
            "RAW" | "MESSAGE" => CxfDataFormat::Raw,
            "CXF_MESSAGE" => CxfDataFormat::CxfMessage,
            other => return Err(CamelError::illegal_argument(format!("Unknown cxf dataFormat {} (POJO, PAYLOAD, RAW, CXF_MESSAGE)", other))),
        })
    }

    fn is_raw(self) -> bool {
        matches!(self, CxfDataFormat::Raw | CxfDataFormat::CxfMessage)
    }
}

/// The `cxf` component.
#[derive(Default)]
pub struct CxfComponent;

pub(crate) struct WsSecClient {
    pub user: Option<(String, String, PasswordType)>,
    pub sign: Option<(PrivateKey, Option<Certificate>)>,
}

impl WsSecClient {
    pub(crate) fn from_params(ctx: &CamelContext, params: &mut Parameters) -> Result<WsSecClient> {
        let take = |params: &mut Parameters, names: &[&str]| names.iter().find_map(|n| params.take(n));
        let user = take(params, &["wsSecurityUsername", "properties.ws-security.username"]);
        let pw = take(params, &["wsSecurityPassword", "properties.ws-security.password"]);
        let ty = match take(params, &["wsSecurityPasswordType", "properties.ws-security.password-type"]) {
            Some(t) => PasswordType::parse(&t).ok_or_else(|| CamelError::illegal_argument(format!("Unknown wsSecurityPasswordType {} (PasswordText, PasswordDigest)", t)))?,
            None => PasswordType::Text,
        };
        let key = take(params, &["wsSecuritySignatureKey"]);
        let cert = take(params, &["wsSecuritySignatureCertificate"]);
        let sign = match key {
            Some(k) => Some((crate::support::load_private_key(Some(ctx), &k)?, cert.map(|c| crate::support::load_certificate(Some(ctx), &c)).transpose()?)),
            None => None,
        };
        Ok(WsSecClient { user: user.map(|u| (u, pw.unwrap_or_default(), ty)), sign })
    }

    /// Add the security header (and signature) to an envelope built from parts.
    pub(crate) fn envelope(&self, version: SoapVersion, mut headers: Vec<String>, body: &[String]) -> Result<String> {
        if let Some((u, p, t)) = &self.user {
            headers.insert(0, wssec::security_header(version, u, p, *t));
        } else if self.sign.is_some() {
            headers.insert(0, wssec::empty_security_header(version));
        }
        let env = build_envelope(version, &headers, body);
        match &self.sign {
            Some((k, c)) => wssec::sign_envelope(&env, k, c.as_ref()),
            None => Ok(env),
        }
    }
}

pub(crate) fn security_policy(ctx: &CamelContext, params: &mut Parameters) -> Result<SecurityPolicy> {
    let users_spec = params.take("wsSecurityUsers").or_else(|| params.take("properties.ws-security.users"));
    let mut users = HashMap::new();
    if let Some(spec) = users_spec {
        let mut spec = spec;
        if let Some(name) = spec.strip_prefix('#') {
            let name = name.to_string();
            if let Some(m) = ctx.registry().lookup::<HashMap<String, String>>(&name) {
                users.extend((*m).clone());
                spec = String::new();
            } else if let Some(s) = ctx.registry().lookup::<String>(&name) {
                spec = (*s).clone();
            } else {
                return Err(CamelError::no_such_bean(&name));
            }
        }
        for pair in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let (u, p) = pair.split_once(':').unwrap_or((pair, ""));
            users.insert(u.to_string(), p.to_string());
        }
    }
    let mut policy = SecurityPolicy::with_users(users);
    if let Some(c) = params.take("wsSecuritySignatureVerifyCertificate") {
        policy.signature_key = Some(crate::support::load_public_key(Some(ctx), &c)?);
    }
    if let Some(age) = params.take("wsSecurityTimestampTTL") {
        policy.max_age_ms = age.trim().parse::<i64>().map_err(|_| CamelError::illegal_argument("wsSecurityTimestampTTL must be seconds"))? * 1000;
    }
    Ok(policy)
}

pub(crate) struct Config {
    address: String,
    wsdl: Option<Arc<Wsdl>>,
    service: Option<Key>,
    port: Option<Key>,
    data_format: CxfDataFormat,
    default_operation: Option<String>,
    default_operation_ns: Option<String>,
    soap_version: Option<SoapVersion>,
    schema_validation: bool,
    published_url: Option<String>,
    bind: BindOptions,
    wssec: WsSecClient,
    policy: SecurityPolicy,
    client: Client,
    listen: Option<std::result::Result<ListenAddress, String>>,
    empty_schema: SchemaSet,
}

impl Config {
    fn schema(&self) -> &SchemaSet {
        self.wsdl.as_ref().map(|w| &w.schema).unwrap_or(&self.empty_schema)
    }
}

impl Component for CxfComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        crate::register_kinds();
        if remaining.starts_with("bean:") {
            return Err(CamelError::illegal_argument("cxf:bean: endpoints need a Spring CxfEndpoint bean; use cxf://http://host:port/path?wsdlURL=... instead"));
        }
        if params.take_bool("mtomEnabled", false)? {
            return Err(CamelError::illegal_argument("cxf: MTOM (mtomEnabled=true) is not supported"));
        }
        let address = params.take("address").unwrap_or_else(|| remaining.to_string());
        let wsdl = match params.take("wsdlURL").or_else(|| params.take("wsdlUrl")) {
            Some(loc) => {
                let (text, dir) = load_text(Some(ctx), &loc)?;
                Some(Arc::new(Wsdl::parse(&text, dir.as_deref())?))
            }
            None => None,
        };
        let data_format = CxfDataFormat::parse(&params.take("dataFormat").unwrap_or_else(|| "POJO".into()))?;
        let attr_prefix = params.take("attributePrefix").unwrap_or_else(|| "@".into());
        let mut prefixes = Vec::new();
        if let Some(w) = &wsdl {
            if !w.target_namespace.is_empty() {
                prefixes.push((w.target_namespace.clone(), "tns".to_string()));
            }
        }
        let service = params.take("serviceName").map(|s| parse_clark(&s));
        let port = params.take("portName").map(|s| parse_clark(&s));
        if let Some(w) = &wsdl {
            w.select_port(service.as_ref(), port.as_ref())?;
        }
        let soap_version = match params.take("soapVersion") {
            Some(v) => Some(SoapVersion::parse(&v).ok_or_else(|| CamelError::illegal_argument(format!("Invalid soapVersion {}", v)))?),
            None => None,
        };
        let wssec = WsSecClient::from_params(ctx, params)?;
        let policy = security_policy(ctx, params)?;
        let listen = if address.starts_with("http://") || address.starts_with("https://") || address.starts_with('/') {
            let mut p2 = params.clone();
            let l = ListenAddress::parse(ctx, "cxf", &address, &mut p2);
            Some(l)
        } else {
            None
        };
        let client = Client::from_params(ctx, "cxf", params)?;
        let listen = listen.map(|l| l.map_err(|e| e.message));
        let cfg = Config {
            address,
            wsdl,
            service,
            port,
            data_format,
            default_operation: params.take("defaultOperationName"),
            default_operation_ns: params.take("defaultOperationNamespace"),
            soap_version,
            schema_validation: params.take_bool("schemaValidationEnabled", false)?,
            published_url: params.take("publishedEndpointUrl"),
            bind: BindOptions { attr_prefix, prefixes, ..Default::default() },
            wssec,
            policy,
            client,
            listen,
            empty_schema: SchemaSet::new(),
        };
        for k in [
            "loggingFeatureEnabled",
            "loggingSizeLimit",
            "wrappedStyle",
            "allowStreaming",
            "skipFaultLogging",
            "synchronous",
            "lazyStartProducer",
            "bridgeErrorHandler",
            "exchangePattern",
            "headerFilterStrategy",
            "skipPayloadMessagePartCheck",
            "mergeProtocolHeaders",
            "continuationTimeout",
            "serviceClass",
            "clientClass",
            "cookieHandler",
            "hostnameVerifier",
            "useGlobalSslContextParameters",
            "sslContextParameters",
            "clientAuth",
        ] {
            params.take(k);
        }
        Ok(Arc::new(CxfEndpoint { uri: uri.to_string(), cfg: Arc::new(cfg), ctx: ctx.downgrade(), binding: Arc::new(Binding::default()) }))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A cxf endpoint.
pub struct CxfEndpoint {
    uri: String,
    cfg: Arc<Config>,
    ctx: WeakContext,
    binding: Arc<Binding>,
}

impl CxfEndpoint {
    /// The bound port of a started consumer.
    pub fn local_port(&self) -> Option<u16> {
        self.binding.port()
    }
    /// The parsed WSDL.
    pub fn wsdl(&self) -> Option<&Arc<Wsdl>> {
        self.cfg.wsdl.as_ref()
    }
}

impl Endpoint for CxfEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        let address = if self.cfg.address.starts_with("http://") || self.cfg.address.starts_with("https://") {
            self.cfg.address.clone()
        } else {
            let w = self.cfg.wsdl.as_ref().ok_or_else(|| CamelError::illegal_argument(format!("cxf producer {} needs an http(s) address or a WSDL with soap:address", self.uri)))?;
            let (_, p) = w.select_port(self.cfg.service.as_ref(), self.cfg.port.as_ref())?;
            p.address.clone().ok_or_else(|| CamelError::illegal_argument("WSDL port has no soap:address"))?
        };
        Ok(Arc::new(CxfProducer { cfg: self.cfg.clone(), address }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        let listen = self
            .cfg
            .listen
            .clone()
            .ok_or_else(|| CamelError::illegal_argument(format!("cxf consumer {} needs an http(s)://host:port/path or /path address", self.uri)))?
            .map_err(CamelError::illegal_argument)?;
        let ops = match &self.cfg.wsdl {
            Some(w) => w.operations(self.cfg.service.as_ref(), self.cfg.port.as_ref())?,
            None => Vec::new(),
        };
        Ok(Arc::new(CxfConsumer { cfg: self.cfg.clone(), ctx: self.ctx.clone(), processor, listen, ops: Arc::new(ops), binding: self.binding.clone() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---------------------------------------------------------------------- producer

struct CxfProducer {
    cfg: Arc<Config>,
    address: String,
}

impl Service for CxfProducer {}

fn header_str(ex: &Exchange, name: &str) -> Option<String> {
    ex.message.headers.get(name).filter(|v| !v.is_null()).map(|v| v.to_string()).filter(|s| !s.is_empty())
}

fn header_fragments(v: Option<&Value>) -> Result<Vec<String>> {
    match v {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(a)) => {
            let mut out = Vec::new();
            for x in a {
                out.extend(split_fragments(&x.to_string())?);
            }
            Ok(out)
        }
        Some(other) => split_fragments(&other.to_string()),
    }
}

/// Body fragments of a PAYLOAD-mode body.
pub(crate) fn payload_fragments(body: &Value) -> Result<Vec<String>> {
    match body {
        Value::Null => Ok(Vec::new()),
        Value::Array(a) => {
            let mut out = Vec::new();
            for x in a {
                out.extend(split_fragments(&x.to_string())?);
            }
            Ok(out)
        }
        Value::Bytes(b) => split_fragments(&String::from_utf8_lossy(b)),
        other => split_fragments(&other.to_string()),
    }
}

pub(crate) fn payload_value(body: Vec<String>) -> Value {
    match body.len() {
        0 => Value::Null,
        1 => Value::String(body.into_iter().next().unwrap_or_default()),
        _ => Value::Array(body.into_iter().map(Value::String).collect()),
    }
}

pub(crate) fn set_fault_headers(ex: &mut Exchange, f: &SoapFault) {
    ex.message.headers.set(FAULT_CODE, f.code.clone());
    ex.message.headers.set(FAULT_STRING, f.reason.clone());
    if let Some(d) = &f.detail {
        ex.message.headers.set(FAULT_DETAIL, d.clone());
    }
}

impl CxfProducer {
    fn operation(&self, ex: &Exchange) -> Result<(String, Option<OperationInfo>)> {
        let name = header_str(ex, OPERATION_NAME).or_else(|| self.cfg.default_operation.clone());
        match (&self.cfg.wsdl, name) {
            (Some(w), Some(n)) => Ok((n.clone(), Some(w.operation(self.cfg.service.as_ref(), self.cfg.port.as_ref(), &n)?))),
            (Some(w), None) => {
                let ops = w.operations(self.cfg.service.as_ref(), self.cfg.port.as_ref())?;
                if ops.len() == 1 {
                    let op = ops.into_iter().next();
                    Ok((op.as_ref().map(|o| o.name.clone()).unwrap_or_default(), op))
                } else if self.cfg.data_format.is_raw() || self.cfg.data_format == CxfDataFormat::Payload {
                    Ok((String::new(), None))
                } else {
                    Err(CamelError::illegal_argument("cxf: no operationName header and no defaultOperationName for a WSDL with several operations"))
                }
            }
            (None, Some(n)) => Ok((n, None)),
            (None, None) => Ok((String::new(), None)),
        }
    }
}

impl Processor for CxfProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let cfg = &self.cfg;
        let (op_name, op) = self.operation(ex)?;
        let version = cfg.soap_version.or(op.as_ref().map(|o| o.version)).unwrap_or(SoapVersion::V11);
        let action = header_str(ex, SOAP_ACTION).or_else(|| op.as_ref().map(|o| o.soap_action.clone())).unwrap_or_default();
        let op_ns = header_str(ex, OPERATION_NAMESPACE)
            .or_else(|| cfg.default_operation_ns.clone())
            .or_else(|| op.as_ref().map(|o| o.namespace.clone()))
            .or_else(|| cfg.wsdl.as_ref().map(|w| w.target_namespace.clone()))
            .unwrap_or_default();
        let envelope = if cfg.data_format.is_raw() {
            match &ex.message.body {
                Value::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
                other => other.to_string(),
            }
        } else {
            let body = match cfg.data_format {
                CxfDataFormat::Pojo => {
                    let shape = match &op {
                        Some(o) => Shape::of(cfg.wsdl.as_ref().unwrap_or_else(|| unreachable!()), o, false).ok_or_else(|| CamelError::illegal_argument("Operation has no input"))?,
                        None => {
                            if op_name.is_empty() {
                                return Err(CamelError::illegal_argument("cxf POJO mode needs an operationName header or defaultOperationName"));
                            }
                            Shape::structural(&op_ns, &op_name, false)
                        }
                    };
                    pojo::encode(cfg.schema(), &shape, &ex.message.body, &cfg.bind)?
                }
                _ => payload_fragments(&ex.message.body)?,
            };
            let headers = header_fragments(ex.message.headers.get(SOAP_HEADERS))?;
            cfg.wssec.envelope(version, headers, &body)?
        };
        let mut http_headers = vec![("Content-Type".to_string(), version.content_type(&action))];
        if version == SoapVersion::V11 {
            http_headers.push(("SOAPAction".to_string(), format!("\"{}\"", action)));
        }
        http_headers.push(("Accept".to_string(), "text/xml, application/soap+xml, multipart/related".to_string()));
        let resp = cfg.client.send("POST", &self.address, &http_headers, Some(envelope.as_bytes()))?;
        ex.message.headers.set("CamelHttpResponseCode", resp.status as i64);
        let text = resp.body_text();
        let one_way = op.as_ref().map(|o| o.output.is_none()).unwrap_or(false);
        if text.trim().is_empty() {
            if resp.status >= 300 {
                return Err(http_failed(&self.address, &resp));
            }
            ex.message.body = Value::Null;
            return Ok(());
        }
        if cfg.data_format.is_raw() {
            ex.message.body = Value::String(text);
            return Ok(());
        }
        let env = match parse_envelope(&text) {
            Ok(e) => e,
            Err(e) => {
                if resp.status >= 300 {
                    return Err(http_failed(&self.address, &resp));
                }
                return Err(e);
            }
        };
        if let Some(f) = env.fault {
            set_fault_headers(ex, &f);
            return Err(f.into_error());
        }
        if !env.headers.is_empty() {
            ex.message.headers.set(SOAP_HEADERS, Value::Array(env.headers.iter().cloned().map(Value::String).collect()));
        } else {
            ex.message.headers.remove(SOAP_HEADERS);
        }
        ex.message.body = match cfg.data_format {
            CxfDataFormat::Pojo if !one_way => {
                let shape = match &op {
                    Some(o) => Shape::of(cfg.wsdl.as_ref().unwrap_or_else(|| unreachable!()), o, true),
                    None => Some(Shape::structural(&op_ns, &op_name, true)),
                };
                match shape {
                    Some(s) => pojo::decode(cfg.schema(), &s, &env.body, true, &cfg.bind)?,
                    None => Value::Null,
                }
            }
            CxfDataFormat::Pojo => Value::Null,
            _ => payload_value(env.body),
        };
        Ok(())
    }
}

// ---------------------------------------------------------------------- consumer

struct CxfConsumer {
    cfg: Arc<Config>,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
    listen: ListenAddress,
    ops: Arc<Vec<OperationInfo>>,
    binding: Arc<Binding>,
}

pub(crate) fn soap_response(version: SoapVersion, status: u16, body: String) -> HttpResponse {
    HttpResponse::new(status).with_header("Content-Type", &version.content_type("")).with_body(body.into_bytes())
}

pub(crate) fn fault_response(version: SoapVersion, f: &SoapFault) -> HttpResponse {
    soap_response(version, 500, build_fault(version, f))
}

/// Serve `?wsdl`.
pub(crate) fn wsdl_response(wsdl: &Wsdl, published: Option<&str>, req: &HttpRequest, https: bool) -> HttpResponse {
    let address = match published {
        Some(p) => p.to_string(),
        None => format!("{}://{}{}", if https { "https" } else { "http" }, req.headers.get("Host").unwrap_or("localhost"), req.path),
    };
    HttpResponse::new(200).with_header("Content-Type", "text/xml; charset=UTF-8").with_body(wsdl.text_with_address(&address).into_bytes())
}

pub(crate) fn wants_wsdl(req: &HttpRequest) -> bool {
    req.method == "GET" && req.raw_query.as_deref().map(|q| q.split('&').any(|p| p.eq_ignore_ascii_case("wsdl") || p.to_ascii_lowercase().starts_with("wsdl="))).unwrap_or(false)
}

fn handle(cfg: &Config, ctx: &WeakContext, processor: &Arc<dyn Processor>, ops: &[OperationInfo], https: bool, req: &mut HttpRequest) -> HttpResponse {
    if wants_wsdl(req) {
        return match &cfg.wsdl {
            Some(w) => wsdl_response(w, cfg.published_url.as_deref(), req, https),
            None => HttpResponse::text(404, "No WSDL available for this service"),
        };
    }
    if req.method != "POST" {
        return HttpResponse::text(405, "Method Not Allowed: SOAP requests use POST").with_header("Allow", "POST");
    }
    let ctx = match ctx.upgrade() {
        Some(c) => c,
        None => return HttpResponse::text(503, "Service Unavailable"),
    };
    let text = req.body_text();
    let guess = if req.content_type().map(|c| c.contains("soap+xml")).unwrap_or(false) { SoapVersion::V12 } else { SoapVersion::V11 };
    let doc = match Document::parse(&text) {
        Ok(d) => d,
        Err(e) => return fault_response(guess, &SoapFault::new(guess.client_code(), format!("Error reading XMLStreamReader: {}", e))),
    };
    let env = match parse_envelope_doc(&doc) {
        Ok(e) => e,
        Err(e) => {
            let code = if e.message.starts_with("VersionMismatch") { "VersionMismatch" } else { guess.client_code() };
            return fault_response(guess, &SoapFault::new(code, e.message));
        }
    };
    let version = env.version;
    if let Err(f) = wssec::verify_security(&doc, version, &cfg.policy) {
        return fault_response(version, &f);
    }
    let action = request_action(req.headers.get("SOAPAction"), req.content_type());
    let body_name = env.body_element_name();
    let op: Option<&OperationInfo> = if cfg.wsdl.is_some() {
        let by_action = action.as_deref().and_then(|a| ops.iter().find(|o| !o.soap_action.is_empty() && o.soap_action == a));
        let by_body = || body_name.as_ref().and_then(|b| ops.iter().find(|o| o.request_element().as_ref() == Some(b)));
        match by_action.or_else(by_body) {
            Some(o) => Some(o),
            None => {
                let what = body_name.as_ref().map(crate::support::clark).unwrap_or_else(|| "(empty body)".into());
                return fault_response(version, &SoapFault::new(version.client_code(), format!("Unexpected wrapper element {} found. No operation matches it (SOAPAction '{}').", what, action.unwrap_or_default())));
            }
        }
    } else {
        None
    };
    let (op_name, op_ns) = match (op, &body_name) {
        (Some(o), _) => (o.name.clone(), o.namespace.clone()),
        (None, Some(b)) => (b.1.clone(), b.0.clone()),
        (None, None) => (String::new(), String::new()),
    };
    if cfg.schema_validation {
        if let Some(w) = &cfg.wsdl {
            for b in &env.body {
                let errs = w.validate_element(b);
                if !errs.is_empty() {
                    return fault_response(version, &SoapFault::new(version.client_code(), format!("Unmarshalling Error: {}", errs.join("; "))));
                }
            }
        }
    }
    let mut ex = Exchange::new(Some(ctx));
    ex.pattern = ExchangePattern::InOut;
    {
        let h = &mut ex.message.headers;
        h.set(OPERATION_NAME, op_name.clone());
        h.set(OPERATION_NAMESPACE, op_ns.clone());
        if let Some(a) = &action {
            h.set(SOAP_ACTION, a.clone());
        }
        h.set("CamelHttpMethod", req.method.clone());
        h.set("CamelHttpPath", req.path.clone());
        h.set("CamelHttpUri", req.raw_path.clone());
        h.set("CamelCxfSoapVersion", version.as_str());
        if let Some(ct) = req.content_type() {
            h.set("Content-Type", ct.to_string());
        }
        let user_headers = wssec::strip_security(&env.headers);
        if !user_headers.is_empty() {
            h.set(SOAP_HEADERS, Value::Array(user_headers.into_iter().map(Value::String).collect()));
        }
    }
    let decoded = match cfg.data_format {
        CxfDataFormat::Raw | CxfDataFormat::CxfMessage => Ok(Value::String(text.clone())),
        CxfDataFormat::Payload => Ok(payload_value(env.body.clone())),
        CxfDataFormat::Pojo => {
            let shape = match (op, &cfg.wsdl) {
                (Some(o), Some(w)) => Shape::of(w, o, false),
                _ => Some(Shape::structural(&op_ns, &op_name, false)),
            };
            match shape {
                Some(s) => pojo::decode(cfg.schema(), &s, &env.body, false, &cfg.bind),
                None => Ok(Value::Null),
            }
        }
    };
    match decoded {
        Ok(v) => ex.message.body = v,
        Err(e) => return fault_response(version, &SoapFault::new(version.client_code(), format!("Unmarshalling Error: {}", e.message))),
    }
    if let Err(e) = processor.process(&mut ex) {
        ex.set_exception(e);
    }
    if let Some(e) = ex.take_exception() {
        return fault_response(version, &SoapFault::from_error(&e, version));
    }
    let one_way = op.map(|o| o.output.is_none()).unwrap_or(false);
    if one_way {
        return HttpResponse::new(202);
    }
    let out = match cfg.data_format {
        CxfDataFormat::Raw | CxfDataFormat::CxfMessage => {
            let t = match &ex.message.body {
                Value::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
                other => other.to_string(),
            };
            let status = match parse_envelope(&t) {
                Ok(e) if e.fault.is_some() => 500,
                _ => 200,
            };
            return soap_response(version, status, t);
        }
        CxfDataFormat::Payload => payload_fragments(&ex.message.body),
        CxfDataFormat::Pojo => {
            let shape = match (op, &cfg.wsdl) {
                (Some(o), Some(w)) => Shape::of(w, o, true),
                _ => Some(Shape::structural(&op_ns, &op_name, true)),
            };
            match shape {
                Some(s) => pojo::encode(cfg.schema(), &s, &ex.message.body, &cfg.bind),
                None => Ok(Vec::new()),
            }
        }
    };
    match out {
        Ok(body) => soap_response(version, 200, build_envelope(version, &[], &body)),
        Err(e) => fault_response(version, &SoapFault::from_error(&e, version)),
    }
}

impl Service for CxfConsumer {
    fn start(&self) -> Result<()> {
        let (cfg, ctx, processor, ops) = (self.cfg.clone(), self.ctx.clone(), self.processor.clone(), self.ops.clone());
        let https = self.listen.server_config.tls.is_some();
        let handler: RouteHandler = Arc::new(move |req: &mut HttpRequest, _params: Vec<(String, String)>| handle(&cfg, &ctx, &processor, &ops, https, req));
        self.binding.start(&self.listen, false, handler)
    }
    fn stop(&self) -> Result<()> {
        self.binding.stop();
        Ok(())
    }
}

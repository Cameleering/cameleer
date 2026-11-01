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
use crate::xmlenc::{self, EncAlg};
use crate::xsdmodel::{BindOptions, SchemaSet};
use crate::{mtom, wsa};
use camel_tls::keys::PublicKey;

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
    /// Reference the signing certificate as a BinarySecurityToken (else `ds:X509Data`).
    pub bst: bool,
    /// SAML assertion: issuer, subject, attributes.
    pub saml: Option<(String, String, Vec<(String, String)>)>,
    /// Encrypt outgoing bodies for this recipient.
    pub encrypt: Option<(PublicKey, Option<Certificate>, EncAlg)>,
    /// Decrypt incoming encrypted bodies.
    pub decrypt: Option<PrivateKey>,
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
        let bst = match take(params, &["wsSecuritySignatureKeyIdentifier", "properties.ws-security.signature.key.identifier"]) {
            Some(k) => matches!(k.trim(), "DirectReference" | "BinarySecurityToken" | "BST"),
            None => true,
        };
        let saml = match take(params, &["wsSecuritySamlSubject"]) {
            Some(subject) => {
                let issuer = take(params, &["wsSecuritySamlIssuer"]).unwrap_or_else(|| "rust-camel".into());
                let attrs = take(params, &["wsSecuritySamlAttributes"])
                    .map(|a| a.split(',').filter_map(|kv| kv.split_once('=').map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))).collect())
                    .unwrap_or_default();
                Some((issuer, subject, attrs))
            }
            None => {
                params.take("wsSecuritySamlIssuer");
                params.take("wsSecuritySamlAttributes");
                None
            }
        };
        let alg = match take(params, &["wsSecurityEncryptionAlgorithm"]) {
            Some(a) => EncAlg::parse(&a).ok_or_else(|| CamelError::illegal_argument(format!("Unknown wsSecurityEncryptionAlgorithm {} (aes128-cbc, aes256-cbc, aes128-gcm, aes256-gcm)", a)))?,
            None => EncAlg::Aes256Gcm,
        };
        let encrypt = match take(params, &["wsSecurityEncryptionCertificate", "wsSecurityEncryptionKey"]) {
            Some(spec) => match crate::support::load_certificate(Some(ctx), &spec) {
                Ok(c) => {
                    let pk = c.public_key.clone().ok_or_else(|| CamelError::illegal_argument("The encryption certificate has no supported public key"))?;
                    Some((pk, Some(c), alg))
                }
                Err(_) => Some((crate::support::load_public_key(Some(ctx), &spec)?, None, alg)),
            },
            None => None,
        };
        let decrypt = match take(params, &["wsSecurityDecryptionKey"]) {
            Some(k) => Some(crate::support::load_private_key(Some(ctx), &k)?),
            None => None,
        };
        Ok(WsSecClient { user: user.map(|u| (u, pw.unwrap_or_default(), ty)), sign, bst, saml, encrypt, decrypt })
    }

    /// Decrypt an incoming envelope when it carries encrypted data and a key is configured.
    pub(crate) fn decrypt_text(&self, text: String) -> Result<String> {
        match &self.decrypt {
            Some(k) if xmlenc::is_encrypted(&text) => xmlenc::decrypt_envelope(&text, k),
            _ => Ok(text),
        }
    }

    /// Encrypt an outgoing envelope when a recipient is configured.
    pub(crate) fn encrypt_text(&self, version: SoapVersion, env: String) -> Result<String> {
        match &self.encrypt {
            Some((pk, cert, alg)) => xmlenc::encrypt_envelope(&env, version, pk, cert.as_ref(), *alg),
            None => Ok(env),
        }
    }

    /// Add the security header (and signature) to an envelope built from parts.
    pub(crate) fn envelope(&self, version: SoapVersion, mut headers: Vec<String>, body: &[String]) -> Result<String> {
        let mut sec = if let Some((u, p, t)) = &self.user {
            Some(wssec::security_header(version, u, p, *t))
        } else if self.sign.is_some() || self.saml.is_some() {
            Some(wssec::empty_security_header(version))
        } else {
            None
        };
        if let (Some(s), Some((issuer, subject, attrs))) = (&mut sec, &self.saml) {
            let a = wssec::saml_assertion(issuer, subject, attrs, 300_000);
            *s = if let Some(open) = s.strip_suffix("/>") { format!("{}>{}</wsse:Security>", open, a) } else { s.replacen("</wsse:Security>", &format!("{}</wsse:Security>", a), 1) };
        }
        if let Some(s) = sec {
            headers.insert(0, s);
        }
        let env = build_envelope(version, &headers, body);
        let env = match &self.sign {
            Some((k, Some(c))) if self.bst => wssec::sign_envelope_bst(&env, k, c)?,
            Some((k, c)) => wssec::sign_envelope(&env, k, c.as_ref())?,
            None => env,
        };
        self.encrypt_text(version, env)
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
    if let Some(spec) = params.take("wsSecuritySignatureTrustedCertificates") {
        for s in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let (text, _) = load_text(Some(ctx), s).map_err(|e| CamelError::illegal_argument(format!("Cannot load trusted certificates {}: {}", s, e.message)))?;
            let certs = Certificate::from_pem_all(text.as_bytes()).map_err(CamelError::illegal_argument)?;
            policy.trusted_certs.extend(certs);
        }
    }
    policy.saml_required = params.take_bool("wsSecuritySamlRequired", false)?;
    if let Some(i) = params.take("wsSecuritySamlTrustedIssuers") {
        policy.saml_issuers = i.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
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
    mtom: bool,
    mtom_threshold: usize,
    wsa: bool,
    wsrm: bool,
    rm_source: wsa::RmSource,
    rm_dest: wsa::RmDestination,
}

impl Config {
    fn schema(&self) -> &SchemaSet {
        self.wsdl.as_ref().map(|w| &w.schema).unwrap_or(&self.empty_schema)
    }
}

impl Component for CxfComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        crate::register_kinds();
        let mut remaining = remaining.to_string();
        if let Some(name) = remaining.strip_prefix("bean:").map(str::to_string) {
            // a CxfEndpoint bean: an object / map of endpoint options (address, wsdlURL...)
            let opts: Vec<(String, String)> = if let Some(v) = ctx.registry().lookup::<Value>(&name) {
                match &*v {
                    Value::Object(m) => m.iter().map(|(k, v)| (k.clone(), v.to_string())).collect(),
                    _ => return Err(CamelError::illegal_argument(format!("cxf:bean:{} must be an object of endpoint options", name))),
                }
            } else if let Some(m) = ctx.registry().lookup::<HashMap<String, String>>(&name) {
                m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
            } else {
                return Err(CamelError::no_such_bean(&name));
            };
            remaining = String::new();
            for (k, v) in opts {
                if k == "address" {
                    remaining = v;
                } else if params.get(&k).is_none() {
                    params.insert(&k, v);
                }
            }
        }
        let remaining = remaining.as_str();
        let mtom = params.take_bool("mtomEnabled", false)? || params.take_bool("properties.mtom-enabled", false)?;
        let mtom_threshold = params.take("mtomThreshold").and_then(|t| t.trim().parse().ok()).unwrap_or(1024usize);
        let features = params.take("features").unwrap_or_default().to_ascii_lowercase();
        let wsrm = params.take_bool("wsrmEnabled", false)? || features.contains("wsrm") || features.contains("reliablemessaging");
        let wsa_on = params.take_bool("wsAddressingEnabled", false)? || params.take_bool("wsaEnabled", false)? || features.contains("wsa") || features.contains("addressing") || wsrm;
        let address = params.take("address").unwrap_or_else(|| remaining.to_string());
        let wsdl = match params.take("wsdlURL").or_else(|| params.take("wsdlUrl")) {
            Some(loc) => {
                let (text, dir) = load_text(Some(ctx), &loc)?;
                Some(Arc::new(Wsdl::parse(&text, dir.as_deref())?))
            }
            None => match params.get("serviceClass").and_then(|s| s.strip_prefix('#').map(str::to_string)) {
                Some(bean) => {
                    params.take("serviceClass");
                    let d = ctx.registry().lookup::<crate::codefirst::ServiceDescriptor>(&bean).ok_or_else(|| CamelError::no_such_bean(&bean))?;
                    let addr = if address.starts_with("http") { address.clone() } else { format!("http://localhost{}", address) };
                    Some(Arc::new(Wsdl::parse(&d.to_wsdl(&addr), None)?))
                }
                None => None,
            },
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
            mtom,
            mtom_threshold,
            wsa: wsa_on,
            wsrm,
            rm_source: wsa::RmSource::default(),
            rm_dest: wsa::RmDestination::default(),
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

/// Header: WS-Addressing action of a received message.
pub const WSA_ACTION: &str = "CamelCxfWsaAction";
/// Header: WS-Addressing message id of a received message.
pub const WSA_MESSAGE_ID: &str = "CamelCxfWsaMessageId";
/// Header: WS-Addressing `RelatesTo` of a received response.
pub const WSA_RELATES_TO: &str = "CamelCxfWsaRelatesTo";
/// Header: WS-Addressing `ReplyTo` of a received request.
pub const WSA_REPLY_TO: &str = "CamelCxfWsaReplyTo";
/// Header: WS-RM sequence identifier of a received message.
pub const RM_SEQUENCE: &str = "CamelCxfRmSequenceId";
/// Header: WS-RM message number of a received message.
pub const RM_MESSAGE_NUMBER: &str = "CamelCxfRmMessageNumber";
/// Header: SAML subject of a received request.
pub const SAML_SUBJECT: &str = "CamelCxfSamlSubject";
/// Header: SAML issuer of a received request.
pub const SAML_ISSUER: &str = "CamelCxfSamlIssuer";
/// Header: SAML attributes (object) of a received request.
pub const SAML_ATTRIBUTES: &str = "CamelCxfSamlAttributes";

/// Read an HTTP message body as a SOAP envelope (multipart/related: XOP parts inlined,
/// other parts returned as attachments).
fn read_message(content_type: Option<&str>, body: &[u8]) -> Result<(String, Vec<mtom::Part>)> {
    if mtom::is_multipart(content_type) {
        let (root, parts) = mtom::parse_multipart(content_type.unwrap_or(""), body)?;
        let text = String::from_utf8_lossy(&root).into_owned();
        return mtom::xop_inline(&text, parts);
    }
    Ok((String::from_utf8_lossy(body).into_owned(), Vec::new()))
}

/// Write an envelope (MTOM / attachments as multipart/related): (Content-Type, bytes).
fn write_message(cfg: &Config, version: SoapVersion, action: &str, envelope: String, attachments: Vec<mtom::Part>) -> Result<(String, Vec<u8>)> {
    let (env, mut parts) = if cfg.mtom { mtom::xop_optimize(&envelope, cfg.mtom_threshold)? } else { (envelope, Vec::new()) };
    parts.extend(attachments);
    if parts.is_empty() && !(cfg.mtom && env.contains(mtom::XOP_NS)) {
        return Ok((version.content_type(action), env.into_bytes()));
    }
    Ok(mtom::build_multipart(&env, version, cfg.mtom, action, &parts))
}

impl CxfProducer {
    /// The WS-RM sequence of this producer (created on first use).
    fn rm_sequence(&self, version: SoapVersion) -> Result<(String, u64)> {
        let cfg = &self.cfg;
        let mut st = cfg.rm_source.state.lock().unwrap();
        if let Some((id, n)) = st.as_mut() {
            *n += 1;
            return Ok((id.clone(), *n));
        }
        let mut headers = wsa::request_headers(wsa::CREATE_SEQUENCE, &self.address, &wsa::new_message_id(), None);
        headers.retain(|h| !h.contains("ReplyTo"));
        let env = build_envelope(version, &headers, &[wsa::create_sequence_body()]);
        let resp = cfg.client.send("POST", &self.address, &[("Content-Type".into(), version.content_type(wsa::CREATE_SEQUENCE))], Some(env.as_bytes()))?;
        let parsed = parse_envelope(&resp.body_text()).map_err(|_| http_failed(&self.address, &resp))?;
        if let Some(f) = parsed.fault {
            return Err(f.into_error());
        }
        let id = wsa::parse_create_sequence_response(&parsed.body).ok_or_else(|| CamelError::illegal_state("WS-RM: no sequence identifier in CreateSequenceResponse"))?;
        *st = Some((id.clone(), 1));
        Ok((id, 1))
    }
}

impl Service for CxfProducer {
    fn stop(&self) -> Result<()> {
        let cfg = &self.cfg;
        if let Some((id, last)) = cfg.rm_source.state.lock().unwrap().take() {
            let version = cfg.soap_version.unwrap_or(SoapVersion::V11);
            let headers = wsa::request_headers(wsa::TERMINATE_SEQUENCE, &self.address, &wsa::new_message_id(), None);
            let env = build_envelope(version, &headers, &[wsa::terminate_sequence_body(&id, last)]);
            let _ = cfg.client.send("POST", &self.address, &[("Content-Type".into(), version.content_type(wsa::TERMINATE_SEQUENCE))], Some(env.as_bytes()));
        }
        Ok(())
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
        let mut message_id = None;
        let envelope = if cfg.data_format.is_raw() {
            match &ex.message.body {
                Value::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
                other => other.to_string(),
            }
        } else {
            let mut headers = header_fragments(ex.message.headers.get(SOAP_HEADERS))?;
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
                    // soap:header parts bound from the POJO body
                    let hparams = match (&op, &cfg.wsdl, shape.param_count()) {
                        (Some(o), Some(w), Some(_)) => pojo::header_params(w, &o.input),
                        _ => Vec::new(),
                    };
                    let (body, hvals) = pojo::split_header_values(shape.param_count().unwrap_or(0), &hparams, &ex.message.body);
                    headers.extend(pojo::encode_header_values(cfg.schema(), &hparams, &hvals, &cfg.bind)?);
                    pojo::encode(cfg.schema(), &shape, &body, &cfg.bind)?
                }
                _ => payload_fragments(&ex.message.body)?,
            };
            if cfg.wsa {
                let a = if action.is_empty() { format!("{}/{}", op_ns.trim_end_matches('/'), op_name) } else { action.clone() };
                let id = wsa::new_message_id();
                headers.extend(wsa::request_headers(&a, &self.address, &id, None));
                message_id = Some(id);
            }
            if cfg.wsrm {
                let (id, n) = self.rm_sequence(version)?;
                headers.push(wsa::sequence_header(&id, n));
            }
            cfg.wssec.envelope(version, headers, &body)?
        };
        let attachments = mtom::attachments_from(ex.property(mtom::ATTACHMENTS));
        let (content_type, payload) = if cfg.data_format.is_raw() && attachments.is_empty() && !cfg.mtom {
            (version.content_type(&action), envelope.into_bytes())
        } else {
            write_message(cfg, version, &action, envelope, attachments)?
        };
        let mut http_headers = vec![("Content-Type".to_string(), content_type)];
        if version == SoapVersion::V11 {
            http_headers.push(("SOAPAction".to_string(), format!("\"{}\"", action)));
        }
        http_headers.push(("Accept".to_string(), "text/xml, application/soap+xml, multipart/related".to_string()));
        let resp = cfg.client.send("POST", &self.address, &http_headers, Some(&payload))?;
        ex.message.headers.set("CamelHttpResponseCode", resp.status as i64);
        let (text, parts) = read_message(resp.headers.get("Content-Type"), &resp.body)?;
        if !parts.is_empty() {
            ex.set_property(mtom::ATTACHMENTS, mtom::attachments_value(parts));
        } else {
            ex.remove_property(mtom::ATTACHMENTS);
        }
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
        let text = cfg.wssec.decrypt_text(text)?;
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
        if cfg.wsa {
            let a = wsa::parse(&env.headers);
            if let (Some(sent), Some(rel)) = (&message_id, &a.relates_to) {
                if sent != rel {
                    return Err(CamelError::illegal_state(format!("WS-Addressing: response RelatesTo {} does not match the request MessageID {}", rel, sent)));
                }
            }
            if let Some(v) = a.action {
                ex.message.headers.set(WSA_ACTION, v);
            }
            if let Some(v) = a.relates_to {
                ex.message.headers.set(WSA_RELATES_TO, v);
            }
        }
        let user_headers: Vec<String> = env.headers.iter().filter(|h| !cfg.wsa || !wsa::is_wsa_or_rm(h)).cloned().collect();
        if !user_headers.is_empty() {
            ex.message.headers.set(SOAP_HEADERS, Value::Array(user_headers.iter().cloned().map(Value::String).collect()));
        } else {
            ex.message.headers.remove(SOAP_HEADERS);
        }
        ex.message.body = match cfg.data_format {
            CxfDataFormat::Pojo if !one_way => {
                let shape = match &op {
                    Some(o) => Shape::of(cfg.wsdl.as_ref().unwrap_or_else(|| unreachable!()), o, true),
                    None => Some(Shape::structural(&op_ns, &op_name, true)),
                };
                let result = match &shape {
                    Some(s) => pojo::decode(cfg.schema(), s, &env.body, true, &cfg.bind)?,
                    None => Value::Null,
                };
                // response soap:header parts
                match (&op, &cfg.wsdl) {
                    (Some(o), Some(w)) if o.output.as_ref().is_some_and(|m| !m.header_parts.is_empty()) => {
                        let hparams = pojo::header_params(w, o.output.as_ref().unwrap());
                        let hv = pojo::decode_header_values(cfg.schema(), &hparams, &user_headers, &cfg.bind);
                        let mut m = match result {
                            Value::Object(m) if shape.as_ref().and_then(|s| s.param_count()).unwrap_or(0) > 1 => m,
                            other => {
                                let mut m = camel_core::value::ObjectMap::new();
                                let name = match &shape {
                                    Some(Shape::Wrapper { params: Some(ps), .. }) if ps.len() == 1 => ps[0].name.1.clone(),
                                    Some(Shape::Bare { params }) if params.len() == 1 => params[0].name.1.clone(),
                                    _ => "return".to_string(),
                                };
                                m.insert(name, other);
                                m
                            }
                        };
                        for (k, v) in hv {
                            m.insert(k, v);
                        }
                        Value::Object(m)
                    }
                    _ => result,
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
    let guess = if req.content_type().map(|c| c.contains("soap+xml") || c.contains("application/soap")).unwrap_or(false) { SoapVersion::V12 } else { SoapVersion::V11 };
    let (text, attachments) = match read_message(req.content_type(), &req.body) {
        Ok(x) => x,
        Err(e) => return fault_response(guess, &SoapFault::new(guess.client_code(), format!("Error reading the multipart message: {}", e.message))),
    };
    let text = match cfg.wssec.decrypt_text(text) {
        Ok(t) => t,
        Err(e) => return fault_response(guess, &SoapFault::new(format!("{{{}}}FailedCheck", wssec::WSSE_NS), e.message)),
    };
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
    let saml = match wssec::verify_saml(&doc, version, &cfg.policy) {
        Ok(s) => s,
        Err(f) => return fault_response(version, &f),
    };
    let addressing = wsa::parse(&env.headers);
    // WS-RM protocol messages and duplicates
    let mut rm_ack = None;
    if cfg.wsrm {
        match cfg.rm_dest.on_request(&addressing, &env.body) {
            wsa::RmAction::Reply(body, action) => {
                let headers = wsa::reply_headers(&addressing, action);
                return soap_response(version, 200, build_envelope(version, &headers, &[body]));
            }
            wsa::RmAction::Duplicate(status, resp) => return soap_response(version, status, resp),
            wsa::RmAction::UnknownSequence(id) => {
                return fault_response(version, &SoapFault::new(version.client_code(), format!("The value of wsrm:Identifier is not a known Sequence identifier: {}", id)).with_subcode(format!("{{{}}}UnknownSequence", wsa::WSRM_NS)));
            }
            wsa::RmAction::Process(ack) => rm_ack = ack,
        }
    }
    let action = request_action(req.headers.get("SOAPAction"), req.content_type()).filter(|a| !a.is_empty()).or_else(|| addressing.action.clone());
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
    let user_headers: Vec<String> = wssec::strip_security(&env.headers).into_iter().filter(|h| !wsa::is_wsa_or_rm(h)).collect();
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
        if !user_headers.is_empty() {
            h.set(SOAP_HEADERS, Value::Array(user_headers.iter().cloned().map(Value::String).collect()));
        }
        if let Some(v) = &addressing.action {
            h.set(WSA_ACTION, v.clone());
        }
        if let Some(v) = &addressing.message_id {
            h.set(WSA_MESSAGE_ID, v.clone());
        }
        if let Some(v) = &addressing.reply_to {
            h.set(WSA_REPLY_TO, v.clone());
        }
        if let Some((id, n)) = &addressing.sequence {
            h.set(RM_SEQUENCE, id.clone());
            h.set(RM_MESSAGE_NUMBER, *n as i64);
        }
        if let Some(s) = &saml {
            h.set(SAML_SUBJECT, s.subject.clone());
            h.set(SAML_ISSUER, s.issuer.clone());
            h.set(SAML_ATTRIBUTES, Value::object(s.attributes.iter().map(|(k, v)| (k.clone(), v.clone()))));
        }
    }
    if !attachments.is_empty() {
        ex.set_property(mtom::ATTACHMENTS, mtom::attachments_value(attachments));
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
                Some(s) => pojo::decode(cfg.schema(), &s, &env.body, false, &cfg.bind).map(|v| match (op, &cfg.wsdl, v) {
                    // soap:header parts follow the body parameters
                    (Some(o), Some(w), Value::Array(mut items)) if !o.input.header_parts.is_empty() => {
                        let hp = pojo::header_params(w, &o.input);
                        items.extend(pojo::decode_header_values(cfg.schema(), &hp, &user_headers, &cfg.bind).into_iter().map(|(_, v)| v));
                        Value::Array(items)
                    }
                    (_, _, v) => v,
                }),
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
    let mut reply_headers: Vec<String> = Vec::new();
    if addressing.message_id.is_some() || cfg.wsa {
        let a = format!("{}Response", action.clone().unwrap_or_else(|| format!("{}/{}", op_ns.trim_end_matches('/'), op_name)));
        reply_headers.extend(wsa::reply_headers(&addressing, &a));
    }
    if let Some(ack) = rm_ack {
        reply_headers.push(ack);
    }
    if let Some(e) = ex.take_exception() {
        let r = fault_response(version, &SoapFault::from_error(&e, version));
        cfg.rm_dest.store(&addressing, 500, &String::from_utf8_lossy(&r.body));
        return r;
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
                Some(s) => {
                    let hparams = match (op, &cfg.wsdl) {
                        (Some(o), Some(w)) => o.output.as_ref().map(|m| pojo::header_params(w, m)).unwrap_or_default(),
                        _ => Vec::new(),
                    };
                    let (body, hvals) = pojo::split_header_values(s.param_count().unwrap_or(0), &hparams, &ex.message.body);
                    match pojo::encode_header_values(cfg.schema(), &hparams, &hvals, &cfg.bind) {
                        Ok(h) => reply_headers.extend(h),
                        Err(e) => return fault_response(version, &SoapFault::from_error(&e, version)),
                    }
                    pojo::encode(cfg.schema(), &s, &body, &cfg.bind)
                }
                None => Ok(Vec::new()),
            }
        }
    };
    let body = match out {
        Ok(b) => b,
        Err(e) => return fault_response(version, &SoapFault::from_error(&e, version)),
    };
    let envelope = match cfg.wssec.encrypt_text(version, build_envelope(version, &reply_headers, &body)) {
        Ok(e) => e,
        Err(e) => return fault_response(version, &SoapFault::from_error(&e, version)),
    };
    cfg.rm_dest.store(&addressing, 200, &envelope);
    let atts = mtom::attachments_from(ex.property(mtom::ATTACHMENTS));
    let (ct, bytes) = match write_message(cfg, version, "", envelope, atts) {
        Ok(x) => x,
        Err(e) => return fault_response(version, &SoapFault::from_error(&e, version)),
    };
    // decoupled response (non-anonymous wsa:ReplyTo)
    if let Some(reply_to) = addressing.reply_to.as_ref().filter(|r| r.starts_with("http://") || r.starts_with("https://")) {
        let (url, client) = (reply_to.clone(), cfg.client.clone());
        std::thread::spawn(move || {
            let _ = client.send("POST", &url, &[("Content-Type".into(), ct)], Some(&bytes));
        });
        return HttpResponse::new(202);
    }
    HttpResponse::new(200).with_header("Content-Type", &ct).with_body(bytes)
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

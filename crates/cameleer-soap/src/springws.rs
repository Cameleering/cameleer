//! The `spring-ws:` component (Camel `camel-spring-ws`): payload-oriented SOAP endpoints.
//!
//! **Consumer** — `spring-ws:<type>:<lookupKey>[?options]` where the type is
//!
//! * `rootqname:{ns}local` — the payload root element,
//! * `soapaction:<action>` — the SOAP action,
//! * `uri:http://host:port/path` — every request to that address,
//! * `xpathresult:<value>?expression=<xpath>` — the XPath result (string) on the payload,
//! * `action:<wsa action>` / `to:<wsa to>` — the WS-Addressing `Action` / `To` headers,
//! * `beanname:<bean>` — a registry bean deciding which requests the endpoint receives: a
//!   [`SpringWsMapping`] (predicate over the request, with constructors for the common
//!   mappings) or a `String` holding another endpoint spec (`rootqname:{ns}local`,
//!   `soapaction:...`, `xpathresult:value?expression=...`), as a Spring
//!   `CamelEndpointMapping` bean would be configured.
//!
//! Endpoints of the non-`uri` types listen on `address` (default `/ws` on the platform port
//! `camel.component.spring-ws.port` / `camel.server.port` / 8080; `http(s)://host:port/path`
//! for an explicit one). All consumers of the process on the same address — port `0`
//! included, which binds one ephemeral port for all of them — share one dispatcher (Spring's
//! `MessageDispatcherServlet`); a request matching no endpoint gets HTTP 404. The body is the
//! payload XML; headers `CamelSpringWebserviceSoapAction`, `CamelSpringWebserviceSoapHeader`
//! (header blocks) and `CamelSpringWebserviceAddressingAction` are set. The response body is
//! the reply payload (none → `202`); exceptions become SOAP faults (`Server`/`Receiver`).
//!
//! **Producer** — `spring-ws:http://host:port/path?soapAction=...&wsAddressingAction=...`:
//! sends the body as payload (SOAP 1.1 unless `soapVersion=1.2`), returns the reply payload.
//! Headers `CamelSpringWebserviceSoapAction`, `CamelSpringWebserviceEndpointUri`,
//! `CamelSpringWebserviceAddressingAction` and `CamelSpringWebserviceSoapHeader` override the
//! options. A fault fails with `SoapFaultClientException` (message = fault reason). Options
//! `timeout` (ms), `username`/`password` (basic auth), `sslContextParameters`, and the WS-Security
//! options of the cxf component (`wsSecurityUsername`...) are supported.

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use cameleer_core::context::{CamelContext, WeakContext};
use cameleer_core::error::{kinds, register_kind_parent, CamelError, Result};
use cameleer_core::message::{Exchange, ExchangePattern};
use cameleer_core::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use cameleer_core::util::Parameters;
use cameleer_core::value::Value;
use cameleer_http::registry::RouteHandler;
use cameleer_http::{HttpRequest, HttpResponse};
use cameleer_xml::dom::Document;

use crate::cxf::{fault_response, payload_fragments, payload_value, security_policy, soap_response, WsSecClient};
use crate::envelope::{build_envelope, parse_envelope, parse_envelope_doc, request_action, SoapFault, SoapVersion};
use crate::support::{parse_clark, split_fragments, Key};
use crate::transport::{http_failed, Binding, ListenAddress};
use cameleer_http::client::EndpointClient as Client;
use crate::wssec::{self, SecurityPolicy};

/// Header: SOAP action.
pub const SOAP_ACTION: &str = "CamelSpringWebserviceSoapAction";
/// Header: endpoint uri override (producer).
pub const ENDPOINT_URI: &str = "CamelSpringWebserviceEndpointUri";
/// Header: WS-Addressing action.
pub const ADDRESSING_ACTION: &str = "CamelSpringWebserviceAddressingAction";
/// Header: SOAP header blocks.
pub const SOAP_HEADER: &str = "CamelSpringWebserviceSoapHeader";
/// Error kind of a received fault.
pub const SOAP_FAULT_CLIENT_EXCEPTION: &str = "SoapFaultClientException";

const WSA_NS: &str = "http://www.w3.org/2005/08/addressing";

pub(crate) fn register_kinds() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        register_kind_parent(SOAP_FAULT_CLIENT_EXCEPTION, kinds::RUNTIME);
    });
}

/// The `spring-ws` component.
#[derive(Default)]
pub struct SpringWsComponent;

/// What a [`SpringWsMapping`] sees of a request.
pub struct SpringWsRequest<'a> {
    /// Expanded name of the payload root element.
    pub root: Option<&'a Key>,
    pub soap_action: &'a str,
    /// The payload XML.
    pub payload: Option<&'a str>,
    pub wsa_action: Option<&'a str>,
    pub wsa_to: Option<&'a str>,
    /// The HTTP request path.
    pub path: &'a str,
}

type MappingFn = dyn Fn(&SpringWsRequest) -> bool + Send + Sync;

/// An endpoint mapping bean for `spring-ws:beanname:<bean>`.
#[derive(Clone)]
pub struct SpringWsMapping {
    predicate: Arc<MappingFn>,
}

impl std::fmt::Debug for SpringWsMapping {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SpringWsMapping")
    }
}

impl SpringWsMapping {
    /// A custom predicate.
    pub fn new(f: impl Fn(&SpringWsRequest) -> bool + Send + Sync + 'static) -> SpringWsMapping {
        SpringWsMapping { predicate: Arc::new(f) }
    }
    /// Payload root element (`{ns}local`, or `local` for any namespace).
    pub fn root_qname(clark: &str) -> SpringWsMapping {
        let k = parse_clark(clark);
        SpringWsMapping::new(move |r| r.root.is_some_and(|x| *x == k || (k.0.is_empty() && x.1 == k.1)))
    }
    /// SOAP action.
    pub fn soap_action(action: &str) -> SpringWsMapping {
        let a = action.to_string();
        SpringWsMapping::new(move |r| r.soap_action == a)
    }
    /// WS-Addressing action.
    pub fn wsa_action(action: &str) -> SpringWsMapping {
        let a = action.to_string();
        SpringWsMapping::new(move |r| r.wsa_action == Some(a.as_str()))
    }
    /// Any of several mappings.
    pub fn any(mappings: Vec<SpringWsMapping>) -> SpringWsMapping {
        SpringWsMapping::new(move |r| mappings.iter().any(|m| (m.predicate)(r)))
    }
    pub fn matches(&self, r: &SpringWsRequest) -> bool {
        (self.predicate)(r)
    }
}

#[derive(Clone, Debug)]
enum Mapping {
    RootQName(Key),
    SoapAction(String),
    Uri,
    XPathResult(String, Arc<cameleer_xml::XPath>),
    Action(String),
    To(String),
    Bean(SpringWsMapping),
}

enum Kind {
    Producer { url: String, soap_action: Option<String>, wsa_action: Option<String>, version: SoapVersion, client: Client, wssec: WsSecClient },
    Consumer { mapping: Mapping, listen: ListenAddress, policy: Arc<SecurityPolicy> },
}

impl Component for SpringWsComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        register_kinds();
        let version = match params.take("soapVersion") {
            Some(v) => SoapVersion::parse(&v).ok_or_else(|| CamelError::illegal_argument(format!("Invalid soapVersion {}", v)))?,
            None => SoapVersion::V11,
        };
        for k in ["messageFactory", "messageSender", "messageFilter", "messageIdStrategy", "webServiceTemplate", "endpointMapping", "endpointDispatcher", "allowResponseHeaderOverride", "allowResponseAttachmentOverride", "faultAction", "faultTo", "replyTo", "outputAction", "sslContextParameters_ignored", "lazyStartProducer", "bridgeErrorHandler", "exchangePattern"] {
            params.take(k);
        }
        let kind = if remaining.starts_with("http://") || remaining.starts_with("https://") {
            if let Some(t) = params.take("timeout") {
                params.insert("receiveTimeout", t);
            }
            Kind::Producer {
                url: remaining.to_string(),
                soap_action: params.take("soapAction"),
                wsa_action: params.take("wsAddressingAction"),
                version,
                wssec: WsSecClient::from_params(ctx, params)?,
                client: Client::from_params(ctx, "spring-ws", params)?,
            }
        } else {
            let (ty, key) = remaining.split_once(':').ok_or_else(|| CamelError::illegal_argument(format!("spring-ws: expected <type>:<lookupKey> or an http(s) url, got '{}'", remaining)))?;
            let mut address = params.take("address").unwrap_or_else(|| "/ws".into());
            let mapping = match ty.to_ascii_lowercase().as_str() {
                "rootqname" => Mapping::RootQName(parse_clark(key)),
                "soapaction" => Mapping::SoapAction(key.to_string()),
                "uri" => {
                    address = key.to_string();
                    Mapping::Uri
                }
                "xpathresult" => {
                    let expr = params.take("expression").ok_or_else(|| CamelError::illegal_argument("spring-ws xpathresult needs the expression option"))?;
                    let xp = cameleer_xml::XPath::compile_ns(&expr, &namespace_bindings(params.take("namespaces"))).map_err(|e| CamelError::illegal_argument(format!("Invalid XPath {}: {}", expr, e)))?;
                    Mapping::XPathResult(key.to_string(), Arc::new(xp))
                }
                "action" => Mapping::Action(key.to_string()),
                "to" => Mapping::To(key.to_string()),
                "beanname" => {
                    if let Some(m) = ctx.registry().lookup::<SpringWsMapping>(key) {
                        Mapping::Bean((*m).clone())
                    } else if let Some(spec) = ctx.registry().lookup::<String>(key) {
                        // an endpoint spec held by the bean: rootqname:..., soapaction:..., xpathresult:...?expression=...
                        let (spec_path, query) = spec.split_once('?').unwrap_or((spec.as_str(), ""));
                        let (t, k) = spec_path.split_once(':').ok_or_else(|| CamelError::illegal_argument(format!("spring-ws bean {} must hold <type>:<lookupKey>", key)))?;
                        let mut q = Parameters::default();
                        for kv in query.split('&').filter(|s| !s.is_empty()) {
                            let (a, b) = kv.split_once('=').unwrap_or((kv, ""));
                            q.insert(a, cameleer_core::util::url_decode(b));
                        }
                        match t.to_ascii_lowercase().as_str() {
                            "rootqname" => Mapping::RootQName(parse_clark(k)),
                            "soapaction" => Mapping::SoapAction(k.to_string()),
                            "action" => Mapping::Action(k.to_string()),
                            "to" => Mapping::To(k.to_string()),
                            "xpathresult" => {
                                let expr = q.take("expression").ok_or_else(|| CamelError::illegal_argument("spring-ws xpathresult needs the expression option"))?;
                                let xp = cameleer_xml::XPath::compile_ns(&expr, &namespace_bindings(q.take("namespaces"))).map_err(|e| CamelError::illegal_argument(format!("Invalid XPath {}: {}", expr, e)))?;
                                Mapping::XPathResult(k.to_string(), Arc::new(xp))
                            }
                            other => return Err(CamelError::illegal_argument(format!("Unsupported endpoint type '{}' in spring-ws bean {}", other, key))),
                        }
                    } else {
                        return Err(CamelError::no_such_bean(key));
                    }
                }
                other => return Err(CamelError::illegal_argument(format!("Unknown spring-ws endpoint type '{}'", other))),
            };
            let listen = ListenAddress::parse(ctx, "spring-ws", &address, params)?;
            Kind::Consumer { mapping, listen, policy: Arc::new(security_policy(ctx, params)?) }
        };
        Ok(Arc::new(SpringWsEndpoint { uri: uri.to_string(), kind: Arc::new(kind), ctx: ctx.downgrade(), port: Arc::new(Mutex::new(None)) }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn namespace_bindings(spec: Option<String>) -> Vec<(String, String)> {
    spec.map(|s| {
        s.split(',')
            .filter_map(|p| p.split_once('=').map(|(a, b)| (a.trim().to_string(), b.trim().to_string())))
            .collect()
    })
    .unwrap_or_default()
}

/// A spring-ws endpoint.
pub struct SpringWsEndpoint {
    uri: String,
    kind: Arc<Kind>,
    ctx: WeakContext,
    port: Arc<Mutex<Option<u16>>>,
}

impl SpringWsEndpoint {
    /// The port of the dispatcher a started consumer is registered with.
    pub fn local_port(&self) -> Option<u16> {
        *self.port.lock().unwrap()
    }
}

impl Endpoint for SpringWsEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        match &*self.kind {
            Kind::Producer { .. } => Ok(Arc::new(SpringWsProducer { kind: self.kind.clone() })),
            _ => Err(CamelError::illegal_argument(format!("{} is a consumer endpoint; producers need spring-ws:http(s)://...", self.uri))),
        }
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        match &*self.kind {
            Kind::Consumer { .. } => Ok(Arc::new(SpringWsConsumer { kind: self.kind.clone(), ctx: self.ctx.clone(), processor, port: self.port.clone(), id: Mutex::new(None) })),
            _ => Err(CamelError::illegal_argument(format!("{} is a producer endpoint", self.uri))),
        }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---------------------------------------------------------------------- producer

struct SpringWsProducer {
    kind: Arc<Kind>,
}

impl Service for SpringWsProducer {}

impl Processor for SpringWsProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let Kind::Producer { url, soap_action, wsa_action, version, client, wssec } = &*self.kind else { unreachable!() };
        let hs = |n: &str| ex.message.headers.get(n).filter(|v| !v.is_null()).map(|v| v.to_string());
        let url = hs(ENDPOINT_URI).unwrap_or_else(|| url.clone());
        let action = hs(SOAP_ACTION).or_else(|| soap_action.clone()).unwrap_or_default();
        let wsa = hs(ADDRESSING_ACTION).or_else(|| wsa_action.clone());
        let mut headers = match ex.message.headers.get(SOAP_HEADER) {
            Some(Value::Array(a)) => a.iter().map(|v| split_fragments(&v.to_string())).collect::<Result<Vec<_>>>()?.concat(),
            Some(v) if !v.is_null() => split_fragments(&v.to_string())?,
            _ => Vec::new(),
        };
        if let Some(a) = &wsa {
            let id = cameleer_core::message::generate_id();
            headers.push(format!("<wsa:Action xmlns:wsa=\"{}\">{}</wsa:Action>", WSA_NS, cameleer_xml::serialize::escape_text(a)));
            headers.push(format!("<wsa:To xmlns:wsa=\"{}\">{}</wsa:To>", WSA_NS, cameleer_xml::serialize::escape_text(&url)));
            headers.push(format!("<wsa:MessageID xmlns:wsa=\"{}\">urn:uuid:{}</wsa:MessageID>", WSA_NS, id));
        }
        let body = payload_fragments(&ex.message.body)?;
        let env = wssec.envelope(*version, headers, &body)?;
        let mut hh = vec![("Content-Type".to_string(), version.content_type(&action))];
        if *version == SoapVersion::V11 {
            hh.push(("SOAPAction".to_string(), format!("\"{}\"", action)));
        }
        let resp = client.send("POST", &url, &hh, Some(env.as_bytes()))?;
        let text = resp.body_text();
        if text.trim().is_empty() {
            if resp.status >= 300 {
                return Err(http_failed(&url, &resp));
            }
            ex.message.body = Value::Null;
            return Ok(());
        }
        let parsed = match parse_envelope(&text) {
            Ok(e) => e,
            Err(e) => return Err(if resp.status >= 300 { http_failed(&url, &resp) } else { e }),
        };
        if let Some(f) = parsed.fault {
            crate::cxf::set_fault_headers(ex, &f);
            return Err(CamelError::new(SOAP_FAULT_CLIENT_EXCEPTION, f.reason.clone()).with_cause(f.into_error()));
        }
        if parsed.headers.is_empty() {
            ex.message.headers.remove(SOAP_HEADER);
        } else {
            ex.message.headers.set(SOAP_HEADER, Value::Array(parsed.headers.into_iter().map(Value::String).collect()));
        }
        ex.message.body = payload_value(parsed.body);
        Ok(())
    }
}

// ---------------------------------------------------------------------- consumer

struct Registered {
    id: u64,
    mapping: Mapping,
    processor: Arc<dyn Processor>,
    ctx: WeakContext,
    policy: Arc<SecurityPolicy>,
}

struct Dispatcher {
    entries: Mutex<Vec<Registered>>,
    binding: Binding,
}

type DispatcherKey = (String, u16, String);

fn dispatchers() -> &'static Mutex<HashMap<DispatcherKey, Arc<Dispatcher>>> {
    static D: OnceLock<Mutex<HashMap<DispatcherKey, Arc<Dispatcher>>>> = OnceLock::new();
    D.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_id() -> u64 {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

fn header_text(headers: &[String], ns: &str, local: &str) -> Option<String> {
    headers.iter().find_map(|h| {
        let d = Document::parse(h).ok()?;
        let r = d.document_element()?;
        if d.namespace_uri(r) == ns && d.local_name(r) == local {
            Some(d.string_value(r).trim().to_string())
        } else {
            None
        }
    })
}

impl Dispatcher {
    fn handle(&self, req: &mut HttpRequest) -> HttpResponse {
        if req.method != "POST" {
            return HttpResponse::text(405, "Method Not Allowed").with_header("Allow", "POST");
        }
        let text = req.body_text();
        let guess = if req.content_type().map(|c| c.contains("soap+xml")).unwrap_or(false) { SoapVersion::V12 } else { SoapVersion::V11 };
        let doc = match Document::parse(&text) {
            Ok(d) => d,
            Err(e) => return fault_response(guess, &SoapFault::new(guess.client_code(), format!("Invalid XML: {}", e))),
        };
        let env = match parse_envelope_doc(&doc) {
            Ok(e) => e,
            Err(e) => return fault_response(guess, &SoapFault::new(guess.client_code(), e.message)),
        };
        let version = env.version;
        let action = request_action(req.headers.get("SOAPAction"), req.content_type()).unwrap_or_default();
        let root = env.body_element_name();
        let wsa_action = header_text(&env.headers, WSA_NS, "Action");
        let wsa_to = header_text(&env.headers, WSA_NS, "To");
        let payload_doc = env.body.first().and_then(|b| Document::parse(b).ok());
        let entries = self.entries.lock().unwrap();
        let found = entries.iter().find(|e| match &e.mapping {
            Mapping::RootQName(k) => root.as_ref() == Some(k) || (k.0.is_empty() && root.as_ref().map(|r| r.1 == k.1).unwrap_or(false)),
            Mapping::SoapAction(a) => *a == action,
            Mapping::Uri => true,
            Mapping::XPathResult(v, xp) => payload_doc.as_ref().and_then(|d| xp.eval_string(d).ok()).map(|r| r == *v).unwrap_or(false),
            Mapping::Action(a) => wsa_action.as_deref() == Some(a.as_str()),
            Mapping::To(t) => wsa_to.as_deref() == Some(t.as_str()),
            Mapping::Bean(m) => m.matches(&SpringWsRequest {
                root: root.as_ref(),
                soap_action: &action,
                payload: env.body.first().map(String::as_str),
                wsa_action: wsa_action.as_deref(),
                wsa_to: wsa_to.as_deref(),
                path: &req.path,
            }),
        });
        let Some(entry) = found else {
            return HttpResponse::text(404, format!("No endpoint mapping found for {}", root.as_ref().map(crate::support::clark).unwrap_or_default()));
        };
        if let Err(f) = wssec::verify_security(&doc, version, &entry.policy) {
            return fault_response(version, &f);
        }
        let Some(ctx) = entry.ctx.upgrade() else { return HttpResponse::text(503, "Service Unavailable") };
        let processor = entry.processor.clone();
        drop(entries);
        let mut ex = Exchange::new(Some(ctx));
        ex.pattern = ExchangePattern::InOut;
        ex.message.body = payload_value(env.body.clone());
        if !action.is_empty() {
            ex.message.headers.set(SOAP_ACTION, action);
        }
        if let Some(a) = wsa_action {
            ex.message.headers.set(ADDRESSING_ACTION, a);
        }
        let user_headers = wssec::strip_security(&env.headers);
        if !user_headers.is_empty() {
            ex.message.headers.set(SOAP_HEADER, Value::Array(user_headers.into_iter().map(Value::String).collect()));
        }
        ex.message.headers.set("CamelHttpPath", req.path.clone());
        if let Err(e) = processor.process(&mut ex) {
            ex.set_exception(e);
        }
        if let Some(e) = ex.take_exception() {
            return fault_response(version, &SoapFault::from_error(&e, version));
        }
        if ex.message.body.is_null() {
            return HttpResponse::new(202);
        }
        match payload_fragments(&ex.message.body) {
            Ok(b) => soap_response(version, 200, build_envelope(version, &[], &b)),
            Err(e) => fault_response(version, &SoapFault::from_error(&e, version)),
        }
    }
}

struct SpringWsConsumer {
    kind: Arc<Kind>,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
    port: Arc<Mutex<Option<u16>>>,
    id: Mutex<Option<(u64, DispatcherKey)>>,
}

impl Service for SpringWsConsumer {
    fn start(&self) -> Result<()> {
        let Kind::Consumer { mapping, listen, policy } = &*self.kind else { unreachable!() };
        let mut idg = self.id.lock().unwrap();
        if idg.is_some() {
            return Ok(());
        }
        let key: DispatcherKey = (listen.host.clone(), listen.port, listen.path.clone());
        let mut all = dispatchers().lock().unwrap();
        let d = match all.get(&key) {
            Some(d) => d.clone(),
            None => {
                let d = Arc::new(Dispatcher { entries: Mutex::new(Vec::new()), binding: Binding::default() });
                let weak = Arc::downgrade(&d);
                let handler: RouteHandler = Arc::new(move |req: &mut HttpRequest, _p: Vec<(String, String)>| match weak.upgrade() {
                    Some(d) => d.handle(req),
                    None => HttpResponse::text(503, "Service Unavailable"),
                });
                d.binding.start(listen, false, handler)?;
                all.insert(key.clone(), d.clone());
                d
            }
        };
        let id = next_id();
        d.entries.lock().unwrap().push(Registered { id, mapping: mapping.clone(), processor: self.processor.clone(), ctx: self.ctx.clone(), policy: policy.clone() });
        *self.port.lock().unwrap() = d.binding.port();
        *idg = Some((id, key));
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        if let Some((id, key)) = self.id.lock().unwrap().take() {
            let mut all = dispatchers().lock().unwrap();
            let empty = match all.get(&key) {
                Some(d) => {
                    let mut e = d.entries.lock().unwrap();
                    e.retain(|r| r.id != id);
                    e.is_empty()
                }
                None => false,
            };
            if empty {
                if let Some(d) = all.remove(&key) {
                    d.binding.stop();
                }
            }
        }
        *self.port.lock().unwrap() = None;
        Ok(())
    }
}

impl Drop for SpringWsConsumer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

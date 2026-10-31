//! SOAP 1.1 / 1.2 envelopes and faults.
//!
//! [`build_envelope`] assembles an envelope from header and body fragments,
//! [`build_fault`] a fault envelope (SOAP 1.1 `faultcode` / `faultstring` / `faultactor` /
//! `detail`, SOAP 1.2 `Code` (+ `Subcode`) / `Reason` / `Role` / `Detail`), and
//! [`parse_envelope`] parses either version (the version comes from the envelope namespace).

use camel_core::error::{kinds, register_kind_parent, CamelError, Result};
use camel_xml::dom::{Document, NodeId};
use camel_xml::serialize::{escape_attr, escape_text};

use crate::support::{first_element, fragment, key_of, resolve_qname, strip_xml_decl, Key};

/// The SOAP 1.1 envelope namespace.
pub const SOAP11_NS: &str = "http://schemas.xmlsoap.org/soap/envelope/";
/// The SOAP 1.2 envelope namespace.
pub const SOAP12_NS: &str = "http://www.w3.org/2003/05/soap-envelope";

/// Error kind of a received / raised SOAP fault (`org.apache.cxf.binding.soap.SoapFault`).
pub const SOAP_FAULT: &str = "SoapFault";
/// Error kind of a malformed SOAP message.
pub const SOAP_EXCEPTION: &str = "SOAPException";

pub(crate) fn register_kinds() {
    static INIT: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    INIT.get_or_init(|| {
        register_kind_parent(SOAP_FAULT, kinds::RUNTIME);
        register_kind_parent(SOAP_EXCEPTION, kinds::EXCEPTION);
        register_kind_parent(FAULT_CODE_KIND, kinds::EXCEPTION);
        register_kind_parent(FAULT_DETAIL_KIND, kinds::EXCEPTION);
    });
}

const FAULT_CODE_KIND: &str = "SoapFaultCode";
const FAULT_DETAIL_KIND: &str = "SoapFaultDetail";

/// SOAP version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoapVersion {
    V11,
    V12,
}

impl SoapVersion {
    /// Envelope namespace.
    pub fn ns(self) -> &'static str {
        match self {
            SoapVersion::V11 => SOAP11_NS,
            SoapVersion::V12 => SOAP12_NS,
        }
    }

    /// Version from an envelope namespace.
    pub fn from_ns(ns: &str) -> Option<SoapVersion> {
        match ns {
            SOAP11_NS => Some(SoapVersion::V11),
            SOAP12_NS => Some(SoapVersion::V12),
            _ => None,
        }
    }

    /// Parse `1.1` / `1.2` (also `soap11`, `soap12`).
    pub fn parse(s: &str) -> Option<SoapVersion> {
        match s.trim().to_ascii_lowercase().as_str() {
            "1.1" | "11" | "soap11" | "soap" => Some(SoapVersion::V11),
            "1.2" | "12" | "soap12" => Some(SoapVersion::V12),
            _ => None,
        }
    }

    /// The HTTP `Content-Type` (SOAP 1.2 carries the action as a parameter).
    pub fn content_type(self, action: &str) -> String {
        match self {
            SoapVersion::V11 => "text/xml; charset=UTF-8".into(),
            SoapVersion::V12 if action.is_empty() => "application/soap+xml; charset=UTF-8".into(),
            SoapVersion::V12 => format!("application/soap+xml; charset=UTF-8; action=\"{}\"", action),
        }
    }

    /// Version string.
    pub fn as_str(self) -> &'static str {
        match self {
            SoapVersion::V11 => "1.1",
            SoapVersion::V12 => "1.2",
        }
    }

    /// The fault code for sender-side errors (`Client` / `Sender`).
    pub fn client_code(self) -> &'static str {
        match self {
            SoapVersion::V11 => "Client",
            SoapVersion::V12 => "Sender",
        }
    }

    /// The fault code for receiver-side errors (`Server` / `Receiver`).
    pub fn server_code(self) -> &'static str {
        match self {
            SoapVersion::V11 => "Server",
            SoapVersion::V12 => "Receiver",
        }
    }
}

/// The SOAP action of an HTTP request: the `SOAPAction` header (1.1, quotes removed) or the
/// `action` parameter of an `application/soap+xml` content type (1.2).
pub fn request_action(soap_action_header: Option<&str>, content_type: Option<&str>) -> Option<String> {
    if let Some(a) = soap_action_header {
        let a = a.trim().trim_matches('"');
        if !a.is_empty() {
            return Some(a.to_string());
        }
    }
    let ct = content_type?;
    ct.split(';').map(str::trim).find_map(|p| p.strip_prefix("action=").map(|v| v.trim_matches('"').to_string())).filter(|s| !s.is_empty())
}

/// A SOAP fault (version independent).
#[derive(Clone, Debug, PartialEq)]
pub struct SoapFault {
    /// Fault code: a SOAP code local name (`Client`/`Server`/`VersionMismatch`/
    /// `MustUnderstand`, `Sender`/`Receiver`/`DataEncodingUnknown`) or a Clark name
    /// `{ns}local` for an application code.
    pub code: String,
    /// SOAP 1.2 subcode (Clark name).
    pub subcode: Option<String>,
    pub reason: String,
    /// `faultactor` / `Role`.
    pub actor: Option<String>,
    /// XML content of the `detail` / `Detail` element.
    pub detail: Option<String>,
}

impl SoapFault {
    /// A fault with a code and reason.
    pub fn new(code: impl Into<String>, reason: impl Into<String>) -> SoapFault {
        SoapFault { code: code.into(), subcode: None, reason: reason.into(), actor: None, detail: None }
    }

    /// Set the detail XML.
    pub fn with_detail(mut self, detail: impl Into<String>) -> SoapFault {
        self.detail = Some(detail.into());
        self
    }

    /// Set the SOAP 1.2 subcode.
    pub fn with_subcode(mut self, subcode: impl Into<String>) -> SoapFault {
        self.subcode = Some(subcode.into());
        self
    }

    /// The local part of the code.
    pub fn code_local(&self) -> &str {
        match self.code.rfind('}') {
            Some(i) => &self.code[i + 1..],
            None => &self.code,
        }
    }

    /// Whether this is a sender (`Client`/`Sender`) fault.
    pub fn is_client(&self) -> bool {
        matches!(self.code_local(), "Client" | "Sender")
    }

    /// Convert to an error (kind [`SOAP_FAULT`]); [`SoapFault::from_error`] reads it back.
    pub fn into_error(self) -> CamelError {
        register_kinds();
        let mut chain: Option<CamelError> = self.subcode.map(|s| CamelError::new("SoapFaultSubcode", s));
        if let Some(d) = self.detail {
            let mut de = CamelError::new(FAULT_DETAIL_KIND, d);
            if let Some(c) = chain.take() {
                de = de.with_cause(c);
            }
            chain = Some(de);
        }
        let mut code = CamelError::new(FAULT_CODE_KIND, self.code.clone());
        if let Some(c) = chain {
            code = code.with_cause(c);
        }
        CamelError::new(SOAP_FAULT, self.reason).with_cause(code)
    }

    /// Recover a fault from an error chain: an error made by [`SoapFault::into_error`] keeps
    /// its code and detail; any other error becomes a `Server`/`Receiver` fault
    /// (`Client`/`Sender` for `IllegalArgumentException`s) with the error message.
    pub fn from_error(e: &CamelError, version: SoapVersion) -> SoapFault {
        if let Some(f) = e.chain().find(|x| x.kind == SOAP_FAULT) {
            let mut fault = SoapFault::new(version.server_code(), f.message.clone());
            for c in f.chain() {
                match c.kind.as_str() {
                    FAULT_CODE_KIND => fault.code = c.message.clone(),
                    FAULT_DETAIL_KIND => fault.detail = Some(c.message.clone()),
                    "SoapFaultSubcode" => fault.subcode = Some(c.message.clone()),
                    _ => {}
                }
            }
            return fault;
        }
        let code = if e.is_a(kinds::ILLEGAL_ARGUMENT) { version.client_code() } else { version.server_code() };
        SoapFault::new(code, e.message.clone())
    }

    fn code_for(&self, v: SoapVersion) -> (String, Option<(String, String)>) {
        // (qualified code text, extra namespace declaration)
        if let Some(r) = self.code.strip_prefix('{') {
            if let Some(i) = r.find('}') {
                let (ns, local) = (&r[..i], &r[i + 1..]);
                if ns == v.ns() {
                    return (format!("soap:{}", local), None);
                }
                return (format!("fc:{}", local), Some(("fc".into(), ns.into())));
            }
        }
        let local = match (v, self.code.as_str()) {
            (SoapVersion::V11, "Sender") => "Client",
            (SoapVersion::V11, "Receiver") => "Server",
            (SoapVersion::V12, "Client") => "Sender",
            (SoapVersion::V12, "Server") => "Receiver",
            (_, c) => c,
        };
        (format!("soap:{}", local), None)
    }
}

impl std::fmt::Display for SoapFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.reason)
    }
}

/// A parsed envelope.
#[derive(Clone, Debug)]
pub struct Envelope {
    pub version: SoapVersion,
    /// Header blocks (self-contained XML fragments).
    pub headers: Vec<String>,
    /// Body elements (self-contained XML fragments); empty for a fault.
    pub body: Vec<String>,
    pub fault: Option<SoapFault>,
}

impl Envelope {
    /// Expanded name of the first body element.
    pub fn body_element_name(&self) -> Option<Key> {
        let b = self.body.first()?;
        let d = Document::parse(b).ok()?;
        d.document_element().map(|r| key_of(&d, r))
    }
}

/// Build an envelope from header and body fragments.
pub fn build_envelope(version: SoapVersion, headers: &[String], body: &[String]) -> String {
    let mut s = format!("<soap:Envelope xmlns:soap=\"{}\">", version.ns());
    if !headers.is_empty() {
        s.push_str("<soap:Header>");
        for h in headers {
            s.push_str(strip_xml_decl(h));
        }
        s.push_str("</soap:Header>");
    }
    s.push_str("<soap:Body>");
    for b in body {
        s.push_str(strip_xml_decl(b));
    }
    s.push_str("</soap:Body></soap:Envelope>");
    s
}

/// Build a fault envelope.
pub fn build_fault(version: SoapVersion, f: &SoapFault) -> String {
    let (code, extra) = f.code_for(version);
    let decl = extra.map(|(p, u)| format!(" xmlns:{}=\"{}\"", p, escape_attr(&u))).unwrap_or_default();
    let mut s = format!("<soap:Envelope xmlns:soap=\"{}\"><soap:Body><soap:Fault{}>", version.ns(), decl);
    match version {
        SoapVersion::V11 => {
            s.push_str(&format!("<faultcode>{}</faultcode><faultstring>{}</faultstring>", code, escape_text(&f.reason)));
            if let Some(a) = &f.actor {
                s.push_str(&format!("<faultactor>{}</faultactor>", escape_text(a)));
            }
            if let Some(d) = &f.detail {
                s.push_str(&format!("<detail>{}</detail>", strip_xml_decl(d)));
            }
        }
        SoapVersion::V12 => {
            s.push_str(&format!("<soap:Code><soap:Value>{}</soap:Value>", code));
            if let Some(sc) = &f.subcode {
                let (ns, local) = crate::support::parse_clark(sc);
                if ns.is_empty() {
                    s.push_str(&format!("<soap:Subcode><soap:Value>{}</soap:Value></soap:Subcode>", escape_text(&local)));
                } else {
                    s.push_str(&format!("<soap:Subcode><soap:Value xmlns:sc=\"{}\">sc:{}</soap:Value></soap:Subcode>", escape_attr(&ns), local));
                }
            }
            s.push_str(&format!("</soap:Code><soap:Reason><soap:Text xml:lang=\"en\">{}</soap:Text></soap:Reason>", escape_text(&f.reason)));
            if let Some(a) = &f.actor {
                s.push_str(&format!("<soap:Role>{}</soap:Role>", escape_text(a)));
            }
            if let Some(d) = &f.detail {
                s.push_str(&format!("<soap:Detail>{}</soap:Detail>", strip_xml_decl(d)));
            }
        }
    }
    s.push_str("</soap:Fault></soap:Body></soap:Envelope>");
    s
}

fn soap_err(m: impl Into<String>) -> CamelError {
    register_kinds();
    CamelError::new(SOAP_EXCEPTION, m.into())
}

fn code_text(doc: &Document, el: NodeId, version: SoapVersion) -> String {
    let (ns, local) = resolve_qname(doc, el, &doc.string_value(el));
    if ns == version.ns() || ns.is_empty() {
        local
    } else {
        format!("{{{}}}{}", ns, local)
    }
}

fn inner_xml(doc: &Document, el: NodeId) -> String {
    let mut s = String::new();
    for &c in doc.children(el) {
        if doc.is_element(c) {
            s.push_str(&fragment(doc, c));
        } else if doc.node_type(c) == camel_xml::NodeType::Text && !doc.value(c).trim().is_empty() {
            s.push_str(&escape_text(doc.value(c)));
        }
    }
    s
}

fn parse_fault(doc: &Document, f: NodeId, version: SoapVersion) -> SoapFault {
    let ns = version.ns();
    let child = |parent: NodeId, uri: &str, local: &str| doc.element_children(parent).into_iter().find(|&c| doc.local_name(c) == local && (doc.namespace_uri(c) == uri || doc.namespace_uri(c).is_empty()));
    match version {
        SoapVersion::V11 => SoapFault {
            code: child(f, "", "faultcode").map(|c| code_text(doc, c, version)).unwrap_or_else(|| "Server".into()),
            subcode: None,
            reason: child(f, "", "faultstring").map(|c| doc.string_value(c)).unwrap_or_default(),
            actor: child(f, "", "faultactor").map(|c| doc.string_value(c)),
            detail: child(f, "", "detail").map(|d| inner_xml(doc, d)).filter(|s| !s.is_empty()),
        },
        SoapVersion::V12 => {
            let code_el = child(f, ns, "Code");
            let code = code_el.and_then(|c| child(c, ns, "Value")).map(|v| code_text(doc, v, version)).unwrap_or_else(|| "Receiver".into());
            let subcode = code_el.and_then(|c| child(c, ns, "Subcode")).and_then(|s| child(s, ns, "Value")).map(|v| {
                let (sns, l) = resolve_qname(doc, v, &doc.string_value(v));
                if sns.is_empty() {
                    l
                } else {
                    format!("{{{}}}{}", sns, l)
                }
            });
            let reason = child(f, ns, "Reason").and_then(|r| child(r, ns, "Text")).map(|t| doc.string_value(t)).unwrap_or_default();
            SoapFault {
                code,
                subcode,
                reason,
                actor: child(f, ns, "Role").map(|c| doc.string_value(c)),
                detail: child(f, ns, "Detail").map(|d| inner_xml(doc, d)).filter(|s| !s.is_empty()),
            }
        }
    }
}

/// Parse an envelope of either SOAP version.
pub fn parse_envelope(text: &str) -> Result<Envelope> {
    let doc = Document::parse(text).map_err(|e| soap_err(format!("Invalid SOAP message: {}", e)))?;
    parse_envelope_doc(&doc)
}

/// Parse an envelope document.
pub fn parse_envelope_doc(doc: &Document) -> Result<Envelope> {
    let env = doc.document_element().ok_or_else(|| soap_err("Empty SOAP message"))?;
    if doc.local_name(env) != "Envelope" {
        return Err(soap_err(format!("Not a SOAP envelope: <{}>", doc.qualified_name(env))));
    }
    let version = SoapVersion::from_ns(doc.namespace_uri(env)).ok_or_else(|| soap_err(format!("VersionMismatch: unknown SOAP envelope namespace '{}'", doc.namespace_uri(env))))?;
    let ns = version.ns();
    let mut headers = Vec::new();
    let mut body = Vec::new();
    let mut fault = None;
    let mut has_body = false;
    for c in doc.element_children(env) {
        if doc.namespace_uri(c) != ns {
            continue;
        }
        match doc.local_name(c) {
            "Header" => headers.extend(doc.element_children(c).into_iter().map(|h| fragment(doc, h))),
            "Body" => {
                has_body = true;
                for b in doc.element_children(c) {
                    if doc.namespace_uri(b) == ns && doc.local_name(b) == "Fault" {
                        fault = Some(parse_fault(doc, b, version));
                    } else {
                        body.push(fragment(doc, b));
                    }
                }
            }
            _ => {}
        }
    }
    if !has_body {
        return Err(soap_err("SOAP envelope has no Body"));
    }
    Ok(Envelope { version, headers, body, fault })
}

/// The first body element of an envelope document.
pub fn body_element(doc: &Document) -> Option<NodeId> {
    let env = doc.document_element()?;
    let ns = doc.namespace_uri(env).to_string();
    let body = doc.element_children(env).into_iter().find(|&c| doc.local_name(c) == "Body" && doc.namespace_uri(c) == ns)?;
    first_element(doc, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_round_trip() {
        let e = build_envelope(SoapVersion::V11, &["<h:Trace xmlns:h=\"urn:h\">1</h:Trace>".into()], &["<m:Get xmlns:m=\"urn:m\"><m:id>3</m:id></m:Get>".into()]);
        let p = parse_envelope(&e).unwrap();
        assert_eq!(p.version, SoapVersion::V11);
        assert_eq!(p.headers, vec!["<h:Trace xmlns:h=\"urn:h\">1</h:Trace>".to_string()]);
        assert_eq!(p.body_element_name(), Some(("urn:m".to_string(), "Get".to_string())));
        assert!(p.fault.is_none());
    }

    #[test]
    fn faults_both_versions() {
        let f = SoapFault::new("Client", "bad <input>").with_detail("<e:Err xmlns:e=\"urn:e\">x</e:Err>");
        let x11 = build_fault(SoapVersion::V11, &f);
        assert!(x11.contains("<faultcode>soap:Client</faultcode>"));
        let p = parse_envelope(&x11).unwrap().fault.unwrap();
        assert_eq!(p, f);
        let x12 = build_fault(SoapVersion::V12, &f.clone().with_subcode("{urn:app}Invalid"));
        assert!(x12.contains("<soap:Value>soap:Sender</soap:Value>"));
        let p = parse_envelope(&x12).unwrap().fault.unwrap();
        assert_eq!(p.code, "Sender");
        assert_eq!(p.subcode.as_deref(), Some("{urn:app}Invalid"));
        assert_eq!(p.reason, "bad <input>");
        assert!(p.is_client());
        let custom = build_fault(SoapVersion::V11, &SoapFault::new("{urn:app}Overflow", "too big"));
        assert_eq!(parse_envelope(&custom).unwrap().fault.unwrap().code, "{urn:app}Overflow");
        // error round trip
        let back = SoapFault::from_error(&f.clone().into_error(), SoapVersion::V11);
        assert_eq!(back, f);
        assert_eq!(SoapFault::from_error(&CamelError::runtime("boom"), SoapVersion::V12).code, "Receiver");
    }

    #[test]
    fn actions() {
        assert_eq!(request_action(Some("\"urn:a\""), None).as_deref(), Some("urn:a"));
        assert_eq!(request_action(Some("\"\""), Some("application/soap+xml; charset=utf-8; action=\"urn:b\"")).as_deref(), Some("urn:b"));
        assert!(parse_envelope("<x/>").is_err());
    }
}

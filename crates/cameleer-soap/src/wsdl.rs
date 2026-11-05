//! WSDL 1.1 parser: `types` (XML Schema, see [`crate::xsdmodel`] and cameleer-xml's validator),
//! `message`s and their parts (`element=` / `type=`), `portType` operations (input, output,
//! faults), `binding`s (SOAP 1.1 `soap:binding` and SOAP 1.2 `soap12:binding`, `style`
//! document / rpc, `soapAction`, `soap:body use="literal"` with `namespace` and `parts`,
//! `soap:header`), and `service` / `port` with their `soap:address`.
//!
//! [`Wsdl::operation`] resolves everything a SOAP client or server needs for one operation
//! into an [`OperationInfo`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cameleer_core::error::{CamelError, Result};
use cameleer_xml::dom::{Document, NodeId};

use crate::envelope::SoapVersion;
use crate::support::{clark, resolve_qname, Key};
use crate::xsdmodel::{ElementDecl, SchemaSet, TypeInfo, TypeRef};

/// The WSDL 1.1 namespace.
pub const WSDL_NS: &str = "http://schemas.xmlsoap.org/wsdl/";
/// The WSDL SOAP 1.1 binding namespace.
pub const WSDL_SOAP11_NS: &str = "http://schemas.xmlsoap.org/wsdl/soap/";
/// The WSDL SOAP 1.2 binding namespace.
pub const WSDL_SOAP12_NS: &str = "http://schemas.xmlsoap.org/wsdl/soap12/";
/// The SOAP over HTTP transport.
pub const HTTP_TRANSPORT: &str = "http://schemas.xmlsoap.org/soap/http";

/// Binding style.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Document,
    Rpc,
}

/// A message part.
#[derive(Clone, Debug)]
pub struct Part {
    pub name: String,
    pub element: Option<Key>,
    pub type_name: Option<Key>,
}

/// A message.
#[derive(Clone, Debug)]
pub struct Message {
    pub name: Key,
    pub parts: Vec<Part>,
}

/// An abstract operation of a port type.
#[derive(Clone, Debug)]
pub struct PortTypeOperation {
    pub name: String,
    pub input: Option<Key>,
    pub output: Option<Key>,
    /// `(fault name, message)`.
    pub faults: Vec<(String, Key)>,
}

/// A port type.
#[derive(Clone, Debug)]
pub struct PortType {
    pub name: Key,
    pub operations: Vec<PortTypeOperation>,
}

/// `soap:body` / `soap:header` settings of a binding operation's input or output.
#[derive(Clone, Debug, Default)]
pub struct BodyBinding {
    pub literal: bool,
    pub namespace: Option<String>,
    /// Parts in the body (None = all parts not bound to headers).
    pub parts: Option<Vec<String>>,
    /// `(message, part)` bound to SOAP headers.
    pub headers: Vec<(Key, String)>,
}

/// An operation of a binding.
#[derive(Clone, Debug)]
pub struct BindingOperation {
    pub name: String,
    pub soap_action: String,
    pub style: Style,
    pub input: BodyBinding,
    pub output: BodyBinding,
}

/// A SOAP binding.
#[derive(Clone, Debug)]
pub struct Binding {
    pub name: Key,
    pub port_type: Key,
    pub version: SoapVersion,
    pub style: Style,
    pub transport: String,
    pub operations: Vec<BindingOperation>,
}

/// A port of a service.
#[derive(Clone, Debug)]
pub struct Port {
    pub name: Key,
    pub binding: Key,
    pub address: Option<String>,
}

/// A service.
#[derive(Clone, Debug)]
pub struct Service {
    pub name: Key,
    pub ports: Vec<Port>,
}

/// A parsed WSDL document.
#[derive(Debug)]
pub struct Wsdl {
    pub target_namespace: String,
    pub name: Option<String>,
    pub schema: SchemaSet,
    /// Schemas of `types` for validation (None when there is no schema or it uses
    /// constructs cameleer-xml's validator does not support).
    pub validators: Vec<cameleer_xml::xsd::Schema>,
    pub messages: HashMap<Key, Message>,
    pub port_types: HashMap<Key, PortType>,
    pub bindings: HashMap<Key, Binding>,
    pub services: Vec<Service>,
    /// The WSDL text as loaded.
    pub text: String,
}

/// A part of an operation's message, with its schema declaration.
#[derive(Clone, Debug)]
pub struct PartInfo {
    pub name: String,
    /// The element name used on the wire.
    pub element: Key,
    /// Declared element (document style `element=`) or synthesized from `type=`.
    pub decl: ElementDecl,
}

/// One direction (input or output) of an operation.
#[derive(Clone, Debug, Default)]
pub struct MessageInfo {
    pub body_parts: Vec<PartInfo>,
    pub header_parts: Vec<PartInfo>,
}

/// Everything needed to call or serve one operation.
#[derive(Clone, Debug)]
pub struct OperationInfo {
    pub name: String,
    /// Namespace of the operation (rpc: `soap:body namespace`; document: the input
    /// wrapper element's namespace, else the target namespace).
    pub namespace: String,
    pub soap_action: String,
    pub style: Style,
    pub version: SoapVersion,
    pub input: MessageInfo,
    pub output: Option<MessageInfo>,
    /// Fault names and the element of their single part.
    pub faults: Vec<(String, Option<Key>)>,
    /// Address of the selected port.
    pub address: Option<String>,
    /// SOAP encoding (`use="encoded"`, rpc/encoded): `xsi:type` on accessors, multi-refs.
    pub encoded: bool,
}

impl OperationInfo {
    /// Whether this is a document/literal *wrapped* operation (a single input part whose
    /// element has the operation's name).
    pub fn wrapped(&self) -> bool {
        self.style == Style::Document && self.input.body_parts.len() == 1 && self.input.body_parts[0].element.1 == self.name
    }

    /// Expanded name of the request body element (rpc wrapper or first document part).
    pub fn request_element(&self) -> Option<Key> {
        match self.style {
            Style::Rpc => Some((self.namespace.clone(), self.name.clone())),
            Style::Document => self.input.body_parts.first().map(|p| p.element.clone()),
        }
    }

    /// Expanded name of the response wrapper / first part element.
    pub fn response_element(&self) -> Option<Key> {
        match self.style {
            Style::Rpc => Some((self.namespace.clone(), format!("{}Response", self.name))),
            Style::Document => self.output.as_ref().and_then(|o| o.body_parts.first()).map(|p| p.element.clone()),
        }
    }
}

fn wsdl_children<'a>(doc: &'a Document, n: NodeId, local: &'a str) -> impl Iterator<Item = NodeId> + 'a {
    doc.child_elements_named(n, WSDL_NS, local)
}

fn soap_child(doc: &Document, n: NodeId, local: &str) -> Option<(NodeId, SoapVersion)> {
    doc.element_children(n).into_iter().find_map(|c| {
        if doc.local_name(c) != local {
            return None;
        }
        match doc.namespace_uri(c) {
            WSDL_SOAP11_NS => Some((c, SoapVersion::V11)),
            WSDL_SOAP12_NS => Some((c, SoapVersion::V12)),
            _ => None,
        }
    })
}

fn soap_children(doc: &Document, n: NodeId, local: &str) -> Vec<NodeId> {
    doc.element_children(n).into_iter().filter(|&c| doc.local_name(c) == local && (doc.namespace_uri(c) == WSDL_SOAP11_NS || doc.namespace_uri(c) == WSDL_SOAP12_NS)).collect()
}

fn style_of(s: Option<&str>, default: Style) -> Style {
    match s {
        Some("rpc") => Style::Rpc,
        Some("document") => Style::Document,
        _ => default,
    }
}

impl Wsdl {
    /// Parse WSDL text; `base_dir` resolves relative schema / WSDL imports.
    pub fn parse(text: &str, base_dir: Option<&Path>) -> Result<Wsdl> {
        let mut w = Wsdl {
            target_namespace: String::new(),
            name: None,
            schema: SchemaSet::new(),
            validators: Vec::new(),
            messages: HashMap::new(),
            port_types: HashMap::new(),
            bindings: HashMap::new(),
            services: Vec::new(),
            text: text.to_string(),
        };
        let mut seen = Vec::new();
        w.load(text, base_dir, &mut seen, true)?;
        Ok(w)
    }

    /// Parse a WSDL file.
    pub fn parse_file(path: &Path) -> Result<Wsdl> {
        let text = std::fs::read_to_string(path).map_err(|e| CamelError::io(format!("Cannot read WSDL {}: {}", path.display(), e)))?;
        Wsdl::parse(&text, path.parent())
    }

    fn load(&mut self, text: &str, base_dir: Option<&Path>, seen: &mut Vec<PathBuf>, top: bool) -> Result<()> {
        let doc = Document::parse(text).map_err(|e| CamelError::new(cameleer_xml::SAX_PARSE_EXCEPTION, format!("Invalid WSDL: {}", e)))?;
        let root = doc.document_element().ok_or_else(|| CamelError::illegal_argument("Empty WSDL"))?;
        if doc.namespace_uri(root) != WSDL_NS || doc.local_name(root) != "definitions" {
            return Err(CamelError::illegal_argument(format!("Not a WSDL 1.1 document: root element is <{}>", doc.qualified_name(root))));
        }
        let tns = doc.attribute(root, "targetNamespace").unwrap_or("").to_string();
        if top {
            self.target_namespace = tns.clone();
            self.name = doc.attribute(root, "name").map(str::to_string);
        }
        for imp in wsdl_children(&doc, root, "import").collect::<Vec<_>>() {
            if let (Some(loc), Some(dir)) = (doc.attribute(imp, "location"), base_dir) {
                let p = dir.join(loc);
                let canon = p.canonicalize().unwrap_or(p.clone());
                if !seen.contains(&canon) && p.exists() {
                    seen.push(canon);
                    let t = std::fs::read_to_string(&p).map_err(|e| CamelError::io(format!("Cannot read {}: {}", p.display(), e)))?;
                    if t.contains(WSDL_NS) && t.contains("definitions") {
                        self.load(&t, p.parent(), seen, false)?;
                    } else {
                        let sd = Document::parse(&t).map_err(|e| CamelError::illegal_argument(format!("Invalid schema {}: {}", p.display(), e)))?;
                        if let Some(r) = sd.document_element() {
                            self.schema.add_schema(&sd, r, p.parent())?;
                            if let Ok(v) = cameleer_xml::xsd::Schema::parse(&t, p.parent()) {
                                self.validators.push(v);
                            }
                        }
                    }
                }
            }
        }
        for types in wsdl_children(&doc, root, "types").collect::<Vec<_>>() {
            for s in doc.element_children(types) {
                if doc.namespace_uri(s) == cameleer_xml::xsd::XSD_NS && doc.local_name(s) == "schema" {
                    self.schema.add_schema(&doc, s, base_dir)?;
                    // a self-contained copy of the schema for the validator (in-scope
                    // namespaces of wsdl:definitions are declared on the copy)
                    let text = crate::support::fragment(&doc, s);
                    if let Ok(v) = cameleer_xml::xsd::Schema::parse(&text, base_dir) {
                        self.validators.push(v);
                    }
                }
            }
        }
        for m in wsdl_children(&doc, root, "message").collect::<Vec<_>>() {
            let name = (tns.clone(), doc.attribute(m, "name").unwrap_or("").to_string());
            let parts = wsdl_children(&doc, m, "part")
                .map(|p| Part {
                    name: doc.attribute(p, "name").unwrap_or("").to_string(),
                    element: doc.attribute(p, "element").map(|e| resolve_qname(&doc, p, e)),
                    type_name: doc.attribute(p, "type").map(|t| resolve_qname(&doc, p, t)),
                })
                .collect();
            self.messages.insert(name.clone(), Message { name, parts });
        }
        for pt in wsdl_children(&doc, root, "portType").collect::<Vec<_>>() {
            let name = (tns.clone(), doc.attribute(pt, "name").unwrap_or("").to_string());
            let operations = wsdl_children(&doc, pt, "operation")
                .map(|o| {
                    let msg = |local: &str| wsdl_children(&doc, o, local).next().and_then(|e| doc.attribute(e, "message").map(|m| resolve_qname(&doc, e, m)));
                    PortTypeOperation {
                        name: doc.attribute(o, "name").unwrap_or("").to_string(),
                        input: msg("input"),
                        output: msg("output"),
                        faults: wsdl_children(&doc, o, "fault")
                            .filter_map(|f| Some((doc.attribute(f, "name").unwrap_or("").to_string(), resolve_qname(&doc, f, doc.attribute(f, "message")?))))
                            .collect(),
                    }
                })
                .collect();
            self.port_types.insert(name.clone(), PortType { name, operations });
        }
        for b in wsdl_children(&doc, root, "binding").collect::<Vec<_>>() {
            let Some((sb, version)) = soap_child(&doc, b, "binding") else {
                continue; // not a SOAP binding (HTTP GET/POST bindings are ignored)
            };
            let style = style_of(doc.attribute(sb, "style"), Style::Document);
            let name = (tns.clone(), doc.attribute(b, "name").unwrap_or("").to_string());
            let port_type = resolve_qname(&doc, b, doc.attribute(b, "type").unwrap_or(""));
            let operations = wsdl_children(&doc, b, "operation")
                .map(|o| {
                    let so = soap_child(&doc, o, "operation").map(|x| x.0);
                    let body = |local: &str| -> BodyBinding {
                        let Some(io) = wsdl_children(&doc, o, local).next() else { return BodyBinding { literal: true, ..Default::default() } };
                        let mut bb = BodyBinding { literal: true, ..Default::default() };
                        if let Some((sbody, _)) = soap_child(&doc, io, "body") {
                            bb.literal = doc.attribute(sbody, "use").map(|u| u == "literal").unwrap_or(true);
                            bb.namespace = doc.attribute(sbody, "namespace").map(str::to_string);
                            bb.parts = doc.attribute(sbody, "parts").map(|p| p.split_whitespace().map(str::to_string).collect());
                        }
                        for h in soap_children(&doc, io, "header") {
                            if let (Some(m), Some(p)) = (doc.attribute(h, "message"), doc.attribute(h, "part")) {
                                bb.headers.push((resolve_qname(&doc, h, m), p.to_string()));
                            }
                        }
                        bb
                    };
                    BindingOperation {
                        name: doc.attribute(o, "name").unwrap_or("").to_string(),
                        soap_action: so.and_then(|s| doc.attribute(s, "soapAction")).unwrap_or("").to_string(),
                        style: style_of(so.and_then(|s| doc.attribute(s, "style")), style),
                        input: body("input"),
                        output: body("output"),
                    }
                })
                .collect();
            self.bindings.insert(name.clone(), Binding { name, port_type, version, style, transport: doc.attribute(sb, "transport").unwrap_or(HTTP_TRANSPORT).to_string(), operations });
        }
        for s in wsdl_children(&doc, root, "service").collect::<Vec<_>>() {
            let name = (tns.clone(), doc.attribute(s, "name").unwrap_or("").to_string());
            let ports = wsdl_children(&doc, s, "port")
                .map(|p| Port {
                    name: (tns.clone(), doc.attribute(p, "name").unwrap_or("").to_string()),
                    binding: resolve_qname(&doc, p, doc.attribute(p, "binding").unwrap_or("")),
                    address: soap_child(&doc, p, "address").and_then(|(a, _)| doc.attribute(a, "location").map(str::to_string)),
                })
                .collect();
            self.services.push(Service { name, ports });
        }
        Ok(())
    }

    /// Select a service and port: by name when given (Clark keys; an empty namespace
    /// matches any), else the first service and its first port with a SOAP binding.
    pub fn select_port(&self, service: Option<&Key>, port: Option<&Key>) -> Result<(&Service, &Port)> {
        let matches = |want: &Key, have: &Key| want.1 == have.1 && (want.0.is_empty() || want.0 == have.0);
        let svc = match service {
            Some(s) => self.services.iter().find(|x| matches(s, &x.name)).ok_or_else(|| CamelError::illegal_argument(format!("Service {} not found in WSDL", clark(s))))?,
            None => self.services.iter().find(|s| s.ports.iter().any(|p| self.bindings.contains_key(&p.binding))).ok_or_else(|| CamelError::illegal_argument("No service with a SOAP port in WSDL"))?,
        };
        let p = match port {
            Some(pn) => svc.ports.iter().find(|x| matches(pn, &x.name)).ok_or_else(|| CamelError::illegal_argument(format!("Port {} not found in service {}", clark(pn), clark(&svc.name))))?,
            None => svc.ports.iter().find(|p| self.bindings.contains_key(&p.binding)).ok_or_else(|| CamelError::illegal_argument(format!("No SOAP port in service {}", clark(&svc.name))))?,
        };
        Ok((svc, p))
    }

    /// The binding of a port.
    pub fn binding_of(&self, port: &Port) -> Result<&Binding> {
        self.bindings.get(&port.binding).ok_or_else(|| CamelError::illegal_argument(format!("Binding {} not found (or not a SOAP binding)", clark(&port.binding))))
    }

    /// Operation names of a binding.
    pub fn operation_names(&self, binding: &Binding) -> Vec<String> {
        binding.operations.iter().map(|o| o.name.clone()).collect()
    }

    fn part_infos(&self, msg: Option<&Key>, bb: &BodyBinding, style: Style, op_ns: &str) -> MessageInfo {
        let mut mi = MessageInfo::default();
        let Some(m) = msg.and_then(|k| self.messages.get(k)) else { return mi };
        let header_parts: Vec<&String> = bb.headers.iter().filter(|(hm, _)| *hm == m.name).map(|(_, p)| p).collect();
        for p in &m.parts {
            let in_body = match &bb.parts {
                Some(list) => list.contains(&p.name),
                None => !header_parts.contains(&&p.name),
            };
            let info = self.part_info(p, style, op_ns);
            if header_parts.contains(&&p.name) {
                mi.header_parts.push(info);
            } else if in_body {
                mi.body_parts.push(info);
            }
        }
        // header parts from other messages
        for (hm, hp) in &bb.headers {
            if *hm != m.name {
                if let Some(p) = self.messages.get(hm).and_then(|x| x.parts.iter().find(|p| p.name == *hp)) {
                    mi.header_parts.push(self.part_info(p, Style::Document, op_ns));
                }
            }
        }
        mi
    }

    fn part_info(&self, p: &Part, style: Style, _op_ns: &str) -> PartInfo {
        match (&p.element, &p.type_name) {
            (Some(e), _) => {
                let decl = self.schema.global_element(&e.0, &e.1).cloned().unwrap_or(ElementDecl { name: e.clone(), ty: TypeRef::Any, min: 1, max: Some(1), nillable: false, reference: false });
                PartInfo { name: p.name.clone(), element: e.clone(), decl }
            }
            (None, Some(t)) => {
                // rpc parts are unqualified accessors named after the part
                let el = (String::new(), p.name.clone());
                let _ = style;
                PartInfo { name: p.name.clone(), element: el.clone(), decl: ElementDecl { name: el, ty: TypeRef::Named(t.clone()), min: 1, max: Some(1), nillable: true, reference: false } }
            }
            _ => {
                let el = (String::new(), p.name.clone());
                PartInfo { name: p.name.clone(), element: el.clone(), decl: ElementDecl { name: el, ty: TypeRef::Any, min: 1, max: Some(1), nillable: true, reference: false } }
            }
        }
    }

    /// Resolve an operation of the selected port (see [`Wsdl::select_port`]).
    pub fn operation(&self, service: Option<&Key>, port: Option<&Key>, name: &str) -> Result<OperationInfo> {
        let (_, p) = self.select_port(service, port)?;
        let b = self.binding_of(p)?;
        self.binding_operation(b, p.address.clone(), name)
    }

    /// All operations of the selected port.
    pub fn operations(&self, service: Option<&Key>, port: Option<&Key>) -> Result<Vec<OperationInfo>> {
        let (_, p) = self.select_port(service, port)?;
        let b = self.binding_of(p)?;
        b.operations.iter().map(|o| self.binding_operation(b, p.address.clone(), &o.name)).collect()
    }

    fn binding_operation(&self, b: &Binding, address: Option<String>, name: &str) -> Result<OperationInfo> {
        let bo = b.operations.iter().find(|o| o.name == name).ok_or_else(|| {
            CamelError::illegal_argument(format!("Operation {} not found in binding {}; operations: {}", name, clark(&b.name), self.operation_names(b).join(", ")))
        })?;
        let pt = self.port_types.get(&b.port_type).ok_or_else(|| CamelError::illegal_argument(format!("PortType {} not found", clark(&b.port_type))))?;
        let pto = pt.operations.iter().find(|o| o.name == name).ok_or_else(|| CamelError::illegal_argument(format!("Operation {} not found in portType {}", name, clark(&pt.name))))?;
        let op_ns = bo.input.namespace.clone().unwrap_or_else(|| self.target_namespace.clone());
        let input = self.part_infos(pto.input.as_ref(), &bo.input, bo.style, &op_ns);
        let output = pto.output.as_ref().map(|o| self.part_infos(Some(o), &bo.output, bo.style, &op_ns));
        let namespace = match bo.style {
            Style::Rpc => op_ns,
            Style::Document => input.body_parts.first().map(|p| p.element.0.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| self.target_namespace.clone()),
        };
        let faults = pto
            .faults
            .iter()
            .map(|(n, m)| (n.clone(), self.messages.get(m).and_then(|m| m.parts.first()).and_then(|p| p.element.clone())))
            .collect();
        Ok(OperationInfo { name: name.to_string(), namespace, soap_action: bo.soap_action.clone(), style: bo.style, version: b.version, input, output, faults, address, encoded: !bo.input.literal })
    }

    /// The shape of a part's element.
    pub fn part_type(&self, p: &PartInfo) -> TypeInfo {
        self.schema.element_info(&p.decl)
    }

    /// Validate a body element against the WSDL schemas (no-op without schemas). Returns
    /// error messages.
    pub fn validate_element(&self, xml: &str) -> Vec<String> {
        let Ok(doc) = Document::parse(xml) else { return vec!["Invalid XML".into()] };
        let Some(root) = doc.document_element() else { return Vec::new() };
        let key = (doc.namespace_uri(root).to_string(), doc.local_name(root).to_string());
        for v in &self.validators {
            if v.global_elements().contains(&key) {
                return v.validate(&doc).into_iter().map(|e| e.message).collect();
            }
        }
        Vec::new()
    }

    /// The WSDL text with every `soap:address location` replaced by `address`.
    pub fn text_with_address(&self, address: &str) -> String {
        let Ok(doc) = Document::parse(&self.text) else { return self.text.clone() };
        let mut doc = doc;
        let ids: Vec<NodeId> = (0..doc.nodes.len())
            .filter(|&i| doc.is_element(i) && doc.local_name(i) == "address" && (doc.namespace_uri(i) == WSDL_SOAP11_NS || doc.namespace_uri(i) == WSDL_SOAP12_NS))
            .collect();
        for i in ids {
            doc.set_attribute(i, cameleer_xml::QName::local("location"), address);
        }
        format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n{}", doc.to_xml_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_calculator() {
        let w = Wsdl::parse(crate::testing::CALCULATOR_WSDL, None).unwrap();
        assert_eq!(w.target_namespace, crate::testing::CALC_NS);
        assert_eq!(w.services.len(), 1);
        assert_eq!(w.services[0].ports.len(), 2);
        let ops = w.operations(None, None).unwrap();
        assert_eq!(ops.iter().map(|o| o.name.as_str()).collect::<Vec<_>>(), vec!["Add", "Divide", "Ping"]);
        let add = &ops[0];
        assert!(add.wrapped());
        assert_eq!(add.version, SoapVersion::V11);
        assert_eq!(add.soap_action, "http://example.org/calculator/Add");
        assert_eq!(add.request_element(), Some((crate::testing::CALC_NS.to_string(), "Add".to_string())));
        assert_eq!(add.response_element().unwrap().1, "AddResponse");
        assert_eq!(ops[1].faults.len(), 1);
        assert!(ops[2].output.is_none(), "Ping is one-way");
        let svc = (crate::testing::CALC_NS.to_string(), "CalculatorService".to_string());
        let p12 = (String::new(), "CalculatorSoap12".to_string());
        let op12 = w.operation(Some(&svc), Some(&p12), "Add").unwrap();
        assert_eq!(op12.version, SoapVersion::V12);
        assert!(w.operation(None, None, "Nope").unwrap_err().message.contains("Add, Divide, Ping"));
        assert!(w.validate_element(&format!("<c:Add xmlns:c=\"{}\"><c:a>1</c:a><c:b>x</c:b></c:Add>", crate::testing::CALC_NS))[0].contains("'x'"));
    }

    #[test]
    fn parse_rpc() {
        let w = Wsdl::parse(crate::testing::STOCK_RPC_WSDL, None).unwrap();
        let op = w.operation(None, None, "GetLastTradePrice").unwrap();
        assert_eq!(op.style, Style::Rpc);
        assert_eq!(op.namespace, "urn:example:stock");
        assert_eq!(op.input.body_parts[0].element, (String::new(), "tickerSymbol".to_string()));
        assert!(op.input.header_parts.is_empty());
    }
}

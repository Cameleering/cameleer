//! The `jaxb` data format without generated classes: XML ⇄ [`Value`] typed by an XML Schema
//! (see [`crate::xsdmodel`] for the type mapping).
//!
//! Options:
//!
//! | option | |
//! |---|---|
//! | `contextPath` | schema(s) used for typing: one or more XSD locations separated by `,` / `;` (`file:`, `classpath:`, path, `#bean`, inline), or a `.properties` descriptor with `schema=` (locations) and `prefix.<prefix>=<namespace>` lines |
//! | `schema` | XSD location used to **validate** on unmarshal and marshal (also used for typing when `contextPath` gives none) |
//! | `partClass` | root element for marshalling, `{ns}local` or `local` (with `partNamespace`) |
//! | `partNamespace` | namespace of `partClass` |
//! | `ignoreJAXBElement` | default `true`: unmarshal returns the root element's content; `false` wraps it as `{root: content}` |
//! | `prettyPrint` | indent output (default `true`, like Camel) |
//! | `fragment` | omit the XML declaration |
//! | `encoding` | encoding of the declaration / output bytes (`UTF-8` default, `ISO-8859-1` supported for bytes) |
//! | `namespacePrefix` | preferred prefixes `uri=prefix,uri2=prefix2` (or a `#bean` `Map`) |
//! | `attributePrefix` | key prefix of attributes (default `@`); `plainAttributes=true` uses plain keys |
//! | `mustBeJAXBElement`, `objectFactory`, `filterNonXmlChars`, `noNamespaceSchemaLocation`, `schemaLocation`, `contentTypeHeader`, `schemaSeverityLevel`, `jaxbProviderProperties`, `xmlStreamWriterWrapper` | accepted (`filterNonXmlChars` removes characters illegal in XML; `contentTypeHeader` sets `Content-Type`) |
//!
//! Marshal root: `partClass`, else a body object with a single key naming a global element,
//! else the single global element of the schema, else a single-key object (structural).
//! Without a schema the mapping is the structural fallback (text-only elements → strings,
//! repeated siblings → arrays, attributes → prefixed keys).
//!
//! Validation failures raise `UnmarshalException` / `MarshalException` (both `JAXBException`s,
//! `IOException` subclasses like in Camel) with the Xerces-style messages of camel-xml.

use std::collections::BTreeMap;
use std::sync::Arc;

use camel_core::dataformat::DataFormat;
use camel_core::error::{kinds, register_kind_parent, CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::Value;
use camel_xml::dom::Document;
use camel_xml::serialize::{serialize, OutputOptions};

use crate::support::{clark, load_text, parse_clark, strip_xml_decl, Key};
use crate::xsdmodel::{BindOptions, SchemaSet, TypeInfo, XmlWriter};

/// `JAXBException`.
pub const JAXB_EXCEPTION: &str = "JAXBException";
/// `UnmarshalException` (a `JAXBException`).
pub const UNMARSHAL_EXCEPTION: &str = "UnmarshalException";
/// `MarshalException` (a `JAXBException`).
pub const MARSHAL_EXCEPTION: &str = "MarshalException";

pub(crate) fn register_kinds() {
    static INIT: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    INIT.get_or_init(|| {
        register_kind_parent(JAXB_EXCEPTION, kinds::IO);
        register_kind_parent(UNMARSHAL_EXCEPTION, JAXB_EXCEPTION);
        register_kind_parent(MARSHAL_EXCEPTION, JAXB_EXCEPTION);
    });
}

/// The `jaxb` data format.
pub struct JaxbDataFormat {
    schema: SchemaSet,
    validator: Option<camel_xml::xsd::Schema>,
    part: Option<Key>,
    ignore_element: bool,
    pretty: bool,
    fragment: bool,
    encoding: String,
    bind: BindOptions,
    filter_non_xml: bool,
    content_type_header: bool,
}

fn opt<'a>(o: &'a BTreeMap<String, String>, k: &str) -> Option<&'a str> {
    o.get(k).map(|s| s.as_str()).filter(|s| !s.is_empty())
}

fn flag(o: &BTreeMap<String, String>, k: &str, d: bool) -> Result<bool> {
    match opt(o, k).map(|s| s.to_ascii_lowercase()) {
        None => Ok(d),
        Some(v) if v == "true" => Ok(true),
        Some(v) if v == "false" => Ok(false),
        Some(v) => Err(CamelError::illegal_argument(format!("Option {} must be true or false but was: {}", k, v))),
    }
}

impl JaxbDataFormat {
    /// Create from data format options.
    pub fn from_options(o: &BTreeMap<String, String>) -> Result<JaxbDataFormat> {
        register_kinds();
        let mut schema = SchemaSet::new();
        let mut prefixes: Vec<(String, String)> = Vec::new();
        let mut locations: Vec<String> = Vec::new();
        if let Some(cp) = opt(o, "contextPath") {
            if cp.trim_end().ends_with(".properties") {
                let (text, dir) = load_text(None, cp)?;
                for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
                    let (k, v) = line.split_once('=').map(|(k, v)| (k.trim(), v.trim())).unwrap_or((line, ""));
                    if k == "schema" {
                        for loc in v.split([',', ';']).map(str::trim).filter(|s| !s.is_empty()) {
                            let p = match &dir {
                                Some(d) if !loc.contains(':') && !std::path::Path::new(loc).is_absolute() => d.join(loc).display().to_string(),
                                _ => loc.to_string(),
                            };
                            locations.push(p);
                        }
                    } else if let Some(p) = k.strip_prefix("prefix.") {
                        prefixes.push((v.to_string(), p.to_string()));
                    }
                }
            } else if cp.trim_start().starts_with('<') {
                locations.push(cp.to_string());
            } else {
                locations.extend(cp.split([',', ';']).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string));
            }
        }
        let validator = match opt(o, "schema") {
            Some(s) => {
                let (text, dir) = load_text(None, s)?;
                if locations.is_empty() {
                    locations.push(s.to_string());
                }
                Some(camel_xml::xsd::Schema::parse(&text, dir.as_deref()).map_err(CamelError::illegal_argument)?)
            }
            None => None,
        };
        for loc in &locations {
            let (text, dir) = load_text(None, loc)?;
            let doc = Document::parse(&text).map_err(|e| CamelError::illegal_argument(format!("Invalid schema {}: {}", loc, e)))?;
            if let Some(r) = doc.document_element() {
                schema.add_schema(&doc, r, dir.as_deref())?;
            }
        }
        if let Some(np) = opt(o, "namespacePrefix") {
            for pair in np.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                if let Some(i) = pair.rfind('=') {
                    prefixes.push((pair[..i].trim().to_string(), pair[i + 1..].trim().to_string()));
                }
            }
        }
        let part = opt(o, "partClass").map(|p| {
            let mut k = parse_clark(p);
            // a Java class name: use its simple name, first letter lower-cased
            if k.0.is_empty() && k.1.contains('.') {
                let simple = k.1.rsplit('.').next().unwrap_or("").to_string();
                let mut c = simple.chars();
                k.1 = c.next().map(|f| f.to_lowercase().collect::<String>() + c.as_str()).unwrap_or_default();
            }
            if let Some(ns) = opt(o, "partNamespace") {
                let pn = parse_clark(ns);
                k.0 = if pn.0.is_empty() { ns.to_string() } else { pn.0 };
            }
            k
        });
        let attr_prefix = if flag(o, "plainAttributes", false)? { String::new() } else { opt(o, "attributePrefix").unwrap_or("@").to_string() };
        Ok(JaxbDataFormat {
            schema,
            validator,
            part,
            ignore_element: flag(o, "ignoreJAXBElement", true)?,
            pretty: flag(o, "prettyPrint", true)?,
            fragment: flag(o, "fragment", false)?,
            encoding: opt(o, "encoding").unwrap_or("UTF-8").to_string(),
            bind: BindOptions { attr_prefix, prefixes, ..Default::default() },
            filter_non_xml: flag(o, "filterNonXmlChars", false)?,
            content_type_header: flag(o, "contentTypeHeader", true)?,
        })
    }

    fn validate(&self, doc: &Document, kind: &str) -> Result<()> {
        if let Some(v) = &self.validator {
            let errs = v.validate(doc);
            if !errs.is_empty() {
                let msg = errs.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("; ");
                return Err(CamelError::new(kind, msg.clone()).with_cause(CamelError::new(camel_xml::SAX_PARSE_EXCEPTION, msg)));
            }
        }
        Ok(())
    }

    /// Marshal a value to XML text.
    pub fn to_xml(&self, body: &Value) -> Result<String> {
        let globals = self.schema.global_elements();
        let (root, content): (Key, &Value) = if let Some(p) = &self.part {
            let ns = if p.0.is_empty() { globals.iter().find(|g| g.1 == p.1).map(|g| g.0.clone()).unwrap_or_default() } else { p.0.clone() };
            ((ns, p.1.clone()), body)
        } else {
            let single_key = match body {
                Value::Object(m) if m.len() == 1 => m.iter().next().map(|(k, v)| (k.clone(), v)),
                _ => None,
            };
            match single_key {
                Some((k, v)) if globals.iter().any(|g| g.1 == k) => (globals.iter().find(|g| g.1 == k).cloned().unwrap_or_default(), v),
                _ if globals.len() == 1 => (globals[0].clone(), body),
                Some((k, v)) => ((String::new(), k), v),
                None => return Err(CamelError::new(MARSHAL_EXCEPTION, "Cannot determine the root element: set partClass or use an object with a single root key")),
            }
        };
        let info = match self.schema.global_element(&root.0, &root.1) {
            Some(d) => self.schema.element_info(d),
            None => TypeInfo::Unknown,
        };
        let mut w = XmlWriter::new(&self.bind.prefixes);
        self.schema.write_value(&mut w, 0, &root, content, &info, true, &self.bind).map_err(|e| CamelError::new(MARSHAL_EXCEPTION, e.message))?;
        if w.root().is_none() {
            return Err(CamelError::new(MARSHAL_EXCEPTION, format!("Nothing to marshal for {}", clark(&root))));
        }
        self.validate(&w.doc, MARSHAL_EXCEPTION)?;
        let opts = OutputOptions {
            method: Some(camel_xml::serialize::Method::Xml),
            indent: self.pretty,
            indent_amount: 2,
            omit_xml_declaration: self.fragment,
            encoding: self.encoding.clone(),
            standalone: if self.fragment { None } else { Some("yes".into()) },
            ..Default::default()
        };
        Ok(serialize(&w.doc, 0, &opts))
    }

    /// Unmarshal XML text to a value.
    pub fn from_xml(&self, text: &str) -> Result<Value> {
        let t = if self.filter_non_xml { text.chars().filter(|&c| matches!(c, '\t' | '\n' | '\r') || c >= ' ').collect::<String>() } else { text.to_string() };
        let doc = Document::parse(strip_xml_decl(&t)).map_err(|e| CamelError::new(UNMARSHAL_EXCEPTION, e.to_string()).with_cause(CamelError::new(camel_xml::SAX_PARSE_EXCEPTION, e.to_string())))?;
        self.validate(&doc, UNMARSHAL_EXCEPTION)?;
        let root = doc.document_element().ok_or_else(|| CamelError::new(UNMARSHAL_EXCEPTION, "Empty document"))?;
        let info = match self.schema.global_element(doc.namespace_uri(root), doc.local_name(root)) {
            Some(d) => self.schema.element_info(d),
            None => TypeInfo::Unknown,
        };
        let v = self.schema.element_to_value(&doc, root, &info, &self.bind);
        Ok(if self.ignore_element { v } else { Value::object([(doc.local_name(root).to_string(), v)]) })
    }
}

fn encode_bytes(s: &str, enc: &str) -> Vec<u8> {
    match enc.to_ascii_uppercase().as_str() {
        "ISO-8859-1" | "LATIN1" | "US-ASCII" => s.chars().map(|c| if (c as u32) < 256 { c as u8 } else { b'?' }).collect(),
        _ => s.as_bytes().to_vec(),
    }
}

impl DataFormat for JaxbDataFormat {
    fn name(&self) -> &str {
        "jaxb"
    }
    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        let xml = self.to_xml(&ex.message.body)?;
        ex.message.body = if self.encoding.eq_ignore_ascii_case("UTF-8") { Value::String(xml) } else { Value::Bytes(encode_bytes(&xml, &self.encoding)) };
        if self.content_type_header {
            ex.message.headers.set("Content-Type", "application/xml");
        }
        Ok(())
    }
    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        let text = match &ex.message.body {
            Value::Bytes(b) => camel_xml::parser::decode_bytes(b).map_err(|e| CamelError::new(UNMARSHAL_EXCEPTION, e.to_string()))?,
            other => other.to_string(),
        };
        ex.message.body = self.from_xml(&text)?;
        Ok(())
    }
}

/// The factory registered as `jaxb`.
pub fn factory(o: &BTreeMap<String, String>) -> Result<Arc<dyn DataFormat>> {
    Ok(Arc::new(JaxbDataFormat::from_options(o)?))
}

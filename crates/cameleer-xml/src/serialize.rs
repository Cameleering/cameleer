//! XML / HTML / text serialization of DOM trees (the XSLT `xsl:output` model).
//!
//! Namespace declarations are fixed up while serializing: declarations already in scope
//! are not repeated, and prefixes needed by element / attribute names are declared.

use crate::dom::{Document, NodeId, NodeType, XML_NS};

/// Marker PI target used for text written with `disable-output-escaping`.
pub(crate) const RAW_TEXT_TARGET: &str = "#raw";

/// Output method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Xml,
    Html,
    Text,
}

/// Serialization options (the attributes of `xsl:output`).
#[derive(Clone, Debug)]
pub struct OutputOptions {
    /// None = choose (`html` when the first element is `<html>` without namespace).
    pub method: Option<Method>,
    pub indent: bool,
    pub indent_amount: usize,
    pub omit_xml_declaration: bool,
    pub encoding: String,
    pub standalone: Option<String>,
    pub doctype_public: Option<String>,
    pub doctype_system: Option<String>,
    /// Expanded names `(uri, local)` of elements whose text is written as CDATA.
    pub cdata_section_elements: Vec<(String, String)>,
    pub media_type: Option<String>,
    pub version: String,
    /// `html` method: add `<META http-equiv="Content-Type" ...>` after `<head>`
    /// (XSLT 2.0 `include-content-type`; default yes).
    pub include_content_type: bool,
}

impl Default for OutputOptions {
    fn default() -> Self {
        OutputOptions {
            method: None,
            indent: false,
            indent_amount: 4,
            omit_xml_declaration: false,
            encoding: "UTF-8".into(),
            standalone: None,
            doctype_public: None,
            doctype_system: None,
            cdata_section_elements: Vec::new(),
            media_type: None,
            version: "1.0".into(),
            include_content_type: true,
        }
    }
}

impl OutputOptions {
    /// Options for writing a fragment: no declaration, no indentation.
    pub fn fragment() -> OutputOptions {
        OutputOptions { method: Some(Method::Xml), omit_xml_declaration: true, ..Default::default() }
    }

    /// The method that will be used for a tree rooted at `root`.
    pub fn effective_method(&self, doc: &Document, root: NodeId) -> Method {
        if let Some(m) = self.method {
            return m;
        }
        for &c in doc.children(root) {
            match doc.node_type(c) {
                NodeType::Element => {
                    let n = doc.name(c);
                    return if n.uri.is_empty() && n.local.eq_ignore_ascii_case("html") { Method::Html } else { Method::Xml };
                }
                NodeType::Text if !doc.value(c).trim().is_empty() => return Method::Xml,
                _ => {}
            }
        }
        Method::Xml
    }
}

/// Serialize a single node as an XML fragment (no declaration).
pub fn node_to_xml(doc: &Document, id: NodeId) -> String {
    let mut s = Serializer::new(doc, OutputOptions::fragment(), Method::Xml);
    s.node(id, 0);
    s.out
}

/// Serialize a tree (normally rooted at a document node) with output options.
pub fn serialize(doc: &Document, root: NodeId, opts: &OutputOptions) -> String {
    let method = opts.effective_method(doc, root);
    let mut s = Serializer::new(doc, opts.clone(), method);
    match method {
        Method::Text => {
            s.text_only(root);
            return s.out;
        }
        Method::Xml => {
            if !opts.omit_xml_declaration {
                s.out.push_str(&format!("<?xml version=\"{}\" encoding=\"{}\"", opts.version, opts.encoding));
                if let Some(sa) = &opts.standalone {
                    s.out.push_str(&format!(" standalone=\"{}\"", sa));
                }
                s.out.push_str("?>");
                if opts.indent {
                    s.out.push('\n');
                }
            }
        }
        Method::Html => {}
    }
    if opts.doctype_system.is_some() || (method == Method::Html && opts.doctype_public.is_some()) {
        let root_el = if doc.node_type(root) == NodeType::Document { doc.document_element_of(root) } else { Some(root) };
        let name = root_el.map(|e| doc.qualified_name(e)).unwrap_or_else(|| "html".into());
        s.doctype = Some(match (&opts.doctype_public, &opts.doctype_system) {
            (Some(p), Some(sy)) => format!("<!DOCTYPE {} PUBLIC \"{}\" \"{}\">", name, p, sy),
            (Some(p), None) => format!("<!DOCTYPE {} PUBLIC \"{}\">", name, p),
            (None, Some(sy)) => format!("<!DOCTYPE {} SYSTEM \"{}\">", name, sy),
            (None, None) => unreachable!(),
        });
    }
    s.node(root, 0);
    if opts.indent && method == Method::Html && !s.out.ends_with('\n') {
        s.out.push('\n');
    }
    s.out
}

const HTML_VOID: &[&str] = &["area", "base", "basefont", "br", "col", "embed", "frame", "hr", "img", "input", "isindex", "link", "meta", "param", "source", "track", "wbr"];
const HTML_BOOLEAN: &[&str] = &["checked", "compact", "declare", "defer", "disabled", "ismap", "multiple", "nohref", "noresize", "noshade", "nowrap", "readonly", "selected"];

struct Serializer<'a> {
    doc: &'a Document,
    opts: OutputOptions,
    method: Method,
    out: String,
    /// Namespace bindings in scope in the output (prefix, uri).
    scope: Vec<(String, String)>,
    doctype: Option<String>,
    gen: usize,
    max_char: u32,
}

impl<'a> Serializer<'a> {
    fn new(doc: &'a Document, opts: OutputOptions, method: Method) -> Self {
        let enc = opts.encoding.to_ascii_uppercase().replace(['-', '_'], "");
        let max_char = match enc.as_str() {
            "ISO88591" | "LATIN1" => 0xFF,
            "USASCII" | "ASCII" => 0x7F,
            _ => 0x10FFFF,
        };
        Serializer { doc, opts, method, out: String::new(), scope: vec![("xml".into(), XML_NS.into())], doctype: None, gen: 0, max_char }
    }

    fn binding(&self, prefix: &str) -> Option<&str> {
        self.scope.iter().rev().find(|(p, _)| p == prefix).map(|(_, u)| u.as_str())
    }

    fn push_char(&mut self, c: char) {
        if c as u32 > self.max_char {
            self.out.push_str(&format!("&#{};", c as u32));
        } else {
            self.out.push(c);
        }
    }

    fn escape_text(&mut self, s: &str) {
        for c in s.chars() {
            match c {
                '&' => self.out.push_str("&amp;"),
                '<' => self.out.push_str("&lt;"),
                '>' => self.out.push_str("&gt;"),
                '\r' => self.out.push_str("&#13;"),
                c => self.push_char(c),
            }
        }
    }

    fn escape_attr(&mut self, s: &str) {
        let html = self.method == Method::Html;
        let chars: Vec<char> = s.chars().collect();
        for (i, &c) in chars.iter().enumerate() {
            match c {
                '&' if html && chars.get(i + 1) == Some(&'{') => self.out.push('&'),
                '&' => self.out.push_str("&amp;"),
                '<' if html => self.out.push('<'),
                '<' => self.out.push_str("&lt;"),
                '>' if !html => self.out.push_str("&gt;"),
                '"' => self.out.push_str("&quot;"),
                '\n' if !html => self.out.push_str("&#10;"),
                '\r' => self.out.push_str("&#13;"),
                '\t' if !html => self.out.push_str("&#9;"),
                c => self.push_char(c),
            }
        }
    }

    fn text_only(&mut self, id: NodeId) {
        let n = self.doc.node(id);
        match n.ty {
            NodeType::Text => {
                let v = n.value.clone();
                for c in v.chars() {
                    self.push_char(c);
                }
            }
            NodeType::ProcessingInstruction if n.name.local == RAW_TEXT_TARGET => self.out.push_str(&n.value),
            NodeType::Document | NodeType::Element => {
                for &c in &n.children {
                    self.text_only(c);
                }
            }
            _ => {}
        }
    }

    fn newline_indent(&mut self, level: usize) {
        self.out.push('\n');
        for _ in 0..level * self.opts.indent_amount {
            self.out.push(' ');
        }
    }

    /// Whether an element's children should be indented (no non-whitespace text).
    fn indentable(&self, id: NodeId) -> bool {
        let n = self.doc.node(id);
        if self.method == Method::Html {
            let l = n.name.local.to_ascii_lowercase();
            if l == "pre" || l == "script" || l == "style" || l == "textarea" {
                return false;
            }
        }
        n.children.iter().all(|&c| {
            let cn = self.doc.node(c);
            match cn.ty {
                NodeType::Text => cn.value.trim().is_empty(),
                NodeType::ProcessingInstruction => cn.name.local != RAW_TEXT_TARGET,
                _ => true,
            }
        }) && n.children.iter().any(|&c| self.doc.node(c).ty != NodeType::Text)
    }

    fn node(&mut self, id: NodeId, level: usize) {
        let doc = self.doc;
        let n = doc.node(id);
        match n.ty {
            NodeType::Document => {
                let indent = self.opts.indent && self.method != Method::Text;
                let mut first = true;
                for &c in &n.children {
                    let cn = doc.node(c);
                    if indent && cn.ty == NodeType::Text && cn.value.trim().is_empty() {
                        continue;
                    }
                    if cn.ty == NodeType::Element {
                        if let Some(dt) = self.doctype.take() {
                            self.out.push_str(&dt);
                            if indent || self.method == Method::Html {
                                self.out.push('\n');
                            }
                        }
                    }
                    if indent && !first && cn.ty != NodeType::Text {
                        self.out.push('\n');
                    }
                    first = false;
                    self.node(c, level);
                }
            }
            NodeType::Text => {
                let v = &n.value;
                let parent_cdata = n.parent.map(|p| {
                    let pn = doc.node(p);
                    pn.ty == NodeType::Element && self.opts.cdata_section_elements.iter().any(|(u, l)| *u == pn.name.uri && *l == pn.name.local)
                });
                if self.method == Method::Html {
                    if let Some(p) = n.parent {
                        let l = doc.name(p).local.to_ascii_lowercase();
                        if doc.node_type(p) == NodeType::Element && (l == "script" || l == "style") {
                            self.out.push_str(v);
                            return;
                        }
                    }
                }
                if parent_cdata == Some(true) && self.method == Method::Xml {
                    self.out.push_str("<![CDATA[");
                    self.out.push_str(&v.replace("]]>", "]]]]><![CDATA[>"));
                    self.out.push_str("]]>");
                } else {
                    self.escape_text(v);
                }
            }
            NodeType::Comment => {
                self.out.push_str("<!--");
                self.out.push_str(&n.value);
                self.out.push_str("-->");
            }
            NodeType::ProcessingInstruction => {
                if n.name.local == RAW_TEXT_TARGET {
                    self.out.push_str(&n.value);
                    return;
                }
                self.out.push_str("<?");
                self.out.push_str(&n.name.local);
                if !n.value.is_empty() {
                    self.out.push(' ');
                    self.out.push_str(&n.value);
                }
                self.out.push_str(if self.method == Method::Html { ">" } else { "?>" });
            }
            NodeType::Attribute => {
                let v = n.value.clone();
                self.escape_text(&v);
            }
            NodeType::Element => self.element(id, level),
        }
    }

    fn declare(&mut self, decls: &mut Vec<(String, String)>, prefix: &str, uri: &str) {
        if self.binding(prefix).unwrap_or("") != uri {
            self.scope.push((prefix.to_string(), uri.to_string()));
            decls.push((prefix.to_string(), uri.to_string()));
        }
    }

    fn element(&mut self, id: NodeId, level: usize) {
        crate::tree_level(|| self.element_here(id, level))
    }

    fn element_here(&mut self, id: NodeId, level: usize) {
        let doc = self.doc;
        let n = doc.node(id);
        let mark = self.scope.len();
        let mut decls: Vec<(String, String)> = Vec::new();
        let html = self.method == Method::Html && n.name.uri.is_empty();
        if self.method != Method::Html || !n.name.uri.is_empty() {
            for (p, u) in &n.ns_decls {
                if p == "xml" {
                    continue;
                }
                self.declare(&mut decls, p, u);
            }
            let (ep, eu) = (n.name.prefix.clone(), n.name.uri.clone());
            self.declare(&mut decls, &ep, &eu);
        }
        let mut attrs: Vec<(String, String)> = Vec::new();
        for &a in &n.attrs {
            let an = doc.node(a);
            let mut prefix = an.name.prefix.clone();
            if !an.name.uri.is_empty() {
                if an.name.uri == XML_NS {
                    prefix = "xml".into();
                } else {
                    let conflict = !prefix.is_empty() && self.binding(&prefix).map(|u| u != an.name.uri).unwrap_or(false) && {
                        // a binding made for this element (element name or an earlier attribute) wins
                        decls.iter().any(|(p, _)| *p == prefix) || prefix == n.name.prefix
                    };
                    if prefix.is_empty() || conflict {
                        // find an existing prefix for the uri or generate one
                        prefix = self
                            .scope
                            .iter()
                            .rev()
                            .find(|(p, u)| *u == an.name.uri && !p.is_empty())
                            .map(|(p, _)| p.clone())
                            .unwrap_or_else(|| {
                                self.gen += 1;
                                format!("ns{}", self.gen)
                            });
                    }
                    let u = an.name.uri.clone();
                    self.declare(&mut decls, &prefix, &u);
                }
            } else {
                prefix.clear();
            }
            let q = if prefix.is_empty() { an.name.local.clone() } else { format!("{}:{}", prefix, an.name.local) };
            attrs.push((q, an.value.clone()));
        }
        let qname = if html { n.name.local.clone() } else { n.name.qualified() };
        self.out.push('<');
        self.out.push_str(&qname);
        for (p, u) in &decls {
            if p.is_empty() {
                self.out.push_str(" xmlns=\"");
            } else {
                self.out.push_str(" xmlns:");
                self.out.push_str(p);
                self.out.push_str("=\"");
            }
            let u = u.clone();
            self.escape_attr(&u);
            self.out.push('"');
        }
        for (q, v) in &attrs {
            self.out.push(' ');
            self.out.push_str(q);
            if html && HTML_BOOLEAN.contains(&q.to_ascii_lowercase().as_str()) && v.eq_ignore_ascii_case(q) {
                continue;
            }
            self.out.push_str("=\"");
            self.escape_attr(v);
            self.out.push('"');
        }
        let lower = n.name.local.to_ascii_lowercase();
        if html && HTML_VOID.contains(&lower.as_str()) {
            self.out.push('>');
            self.scope.truncate(mark);
            return;
        }
        if n.children.is_empty() && !html {
            self.out.push_str("/>");
            self.scope.truncate(mark);
            return;
        }
        self.out.push('>');
        let indent = self.opts.indent && self.indentable(id);
        let is_head = html && lower == "head" && self.opts.include_content_type;
        if is_head {
            if indent {
                self.newline_indent(level + 1);
            }
            let media = self.opts.media_type.clone().unwrap_or_else(|| "text/html".into());
            let meta = format!("<META http-equiv=\"Content-Type\" content=\"{}; charset={}\">", media, self.opts.encoding);
            self.out.push_str(&meta);
        }
        for &c in &n.children {
            if is_head {
                let cn = doc.node(c);
                let is_ct_meta = cn.ty == NodeType::Element
                    && cn.name.local.eq_ignore_ascii_case("meta")
                    && cn.attrs.iter().any(|&a| doc.node(a).name.local.eq_ignore_ascii_case("http-equiv") && doc.node(a).value.eq_ignore_ascii_case("content-type"));
                if is_ct_meta {
                    continue;
                }
            }
            if indent {
                let cn = doc.node(c);
                if cn.ty == NodeType::Text && cn.value.trim().is_empty() {
                    continue;
                }
                self.newline_indent(level + 1);
            }
            self.node(c, level + 1);
        }
        if indent {
            self.newline_indent(level);
        }
        self.out.push_str("</");
        self.out.push_str(&qname);
        self.out.push('>');
        self.scope.truncate(mark);
    }
}

/// Escape text content for XML.
pub fn escape_text(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Escape an attribute value for XML (double quotes).
pub fn escape_attr(s: &str) -> String {
    escape_text(s).replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    #[test]
    fn roundtrip_and_options() {
        let d = parse("<a xmlns='urn:a' xmlns:p='urn:p'><p:b x='&lt;&quot;'>1 &amp; 2</p:b><c/></a>").unwrap();
        assert_eq!(node_to_xml(&d, 0), "<a xmlns=\"urn:a\" xmlns:p=\"urn:p\"><p:b x=\"&lt;&quot;\">1 &amp; 2</p:b><c/></a>");
        let b = d.element_children(d.document_element().unwrap())[0];
        assert_eq!(node_to_xml(&d, b), "<p:b xmlns:p=\"urn:p\" x=\"&lt;&quot;\">1 &amp; 2</p:b>");
        let opts = OutputOptions { indent: true, indent_amount: 2, ..Default::default() };
        assert_eq!(
            serialize(&d, 0, &opts),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<a xmlns=\"urn:a\" xmlns:p=\"urn:p\">\n  <p:b x=\"&lt;&quot;\">1 &amp; 2</p:b>\n  <c/>\n</a>"
        );
        let h = parse("<html><body><br/><p>x</p><script>a &lt; b</script></body></html>").unwrap();
        assert_eq!(serialize(&h, 0, &OutputOptions::default()), "<html><body><br><p>x</p><script>a < b</script></body></html>");
        let opts = OutputOptions { encoding: "US-ASCII".into(), omit_xml_declaration: true, ..Default::default() };
        assert_eq!(serialize(&parse("<a>\u{e9}</a>").unwrap(), 0, &opts), "<a>&#233;</a>");
    }
}

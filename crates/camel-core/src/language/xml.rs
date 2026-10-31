//! A small, non-validating XML parser producing an arena-based DOM, an XML serializer,
//! and conversions between XML and [`Value`] (a Jackson-XML-like mapping).
//!
//! Supports elements, attributes, text, CDATA, comments, processing instructions, the XML
//! declaration, DOCTYPE (skipped), predefined and numeric character entities.
//! Namespaces are kept as part of the qualified names (`ns:tag`).

use std::collections::BTreeMap;

use crate::error::{kinds, CamelError, Result};
use crate::value::Value;

/// Kind of a DOM node.
#[derive(Clone, Debug, PartialEq)]
pub enum NodeKind {
    /// The document node (index 0).
    Document,
    Element { name: String, attrs: Vec<(String, String)> },
    Text(String),
    Comment(String),
    ProcessingInstruction { target: String, data: String },
}

/// A node in the arena.
#[derive(Clone, Debug)]
pub struct Node {
    pub kind: NodeKind,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
}

/// An XML document: nodes stored in document order; index 0 is the document node.
#[derive(Clone, Debug)]
pub struct Document {
    pub nodes: Vec<Node>,
}

impl Document {
    /// Parse an XML document.
    pub fn parse(text: &str) -> Result<Document> {
        let mut p = XmlParser { s: text.as_bytes(), text, i: 0, doc: Document { nodes: vec![Node { kind: NodeKind::Document, parent: None, children: vec![] }] } };
        p.document()?;
        Ok(p.doc)
    }

    /// Index of the root (document) element.
    pub fn root_element(&self) -> Option<usize> {
        self.nodes[0].children.iter().copied().find(|&c| matches!(self.nodes[c].kind, NodeKind::Element { .. }))
    }

    pub fn node(&self, i: usize) -> &Node {
        &self.nodes[i]
    }

    /// Qualified element name.
    pub fn name(&self, i: usize) -> &str {
        match &self.nodes[i].kind {
            NodeKind::Element { name, .. } => name,
            NodeKind::ProcessingInstruction { target, .. } => target,
            _ => "",
        }
    }

    /// Attributes of an element.
    pub fn attrs(&self, i: usize) -> &[(String, String)] {
        match &self.nodes[i].kind {
            NodeKind::Element { attrs, .. } => attrs,
            _ => &[],
        }
    }

    /// Attribute value of an element.
    pub fn attr(&self, i: usize, name: &str) -> Option<&str> {
        self.attrs(i).iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    /// Element children.
    pub fn element_children(&self, i: usize) -> Vec<usize> {
        self.nodes[i].children.iter().copied().filter(|&c| matches!(self.nodes[c].kind, NodeKind::Element { .. })).collect()
    }

    /// XPath string-value of a node (concatenated descendant text for elements).
    pub fn text_content(&self, i: usize) -> String {
        let mut out = String::new();
        self.collect_text(i, &mut out);
        out
    }

    fn collect_text(&self, i: usize, out: &mut String) {
        match &self.nodes[i].kind {
            NodeKind::Text(t) => out.push_str(t),
            NodeKind::Element { .. } | NodeKind::Document => {
                for &c in &self.nodes[i].children {
                    if !matches!(self.nodes[c].kind, NodeKind::Comment(_) | NodeKind::ProcessingInstruction { .. }) {
                        self.collect_text(c, out);
                    }
                }
            }
            NodeKind::Comment(c) => out.push_str(c),
            NodeKind::ProcessingInstruction { data, .. } => out.push_str(data),
        }
    }

    /// Serialize a node (and its subtree) to XML text.
    pub fn serialize(&self, i: usize) -> String {
        let mut out = String::new();
        self.write(i, &mut out);
        out
    }

    fn write(&self, i: usize, out: &mut String) {
        match &self.nodes[i].kind {
            NodeKind::Document => {
                for &c in &self.nodes[i].children {
                    self.write(c, out);
                }
            }
            NodeKind::Element { name, attrs } => {
                out.push('<');
                out.push_str(name);
                for (k, v) in attrs {
                    out.push(' ');
                    out.push_str(k);
                    out.push_str("=\"");
                    out.push_str(&escape_attr(v));
                    out.push('"');
                }
                if self.nodes[i].children.is_empty() {
                    out.push_str("/>");
                } else {
                    out.push('>');
                    for &c in &self.nodes[i].children {
                        self.write(c, out);
                    }
                    out.push_str("</");
                    out.push_str(name);
                    out.push('>');
                }
            }
            NodeKind::Text(t) => out.push_str(&escape_text(t)),
            NodeKind::Comment(c) => {
                out.push_str("<!--");
                out.push_str(c);
                out.push_str("-->");
            }
            NodeKind::ProcessingInstruction { target, data } => {
                out.push_str("<?");
                out.push_str(target);
                if !data.is_empty() {
                    out.push(' ');
                    out.push_str(data);
                }
                out.push_str("?>");
            }
        }
    }
}

/// Escape text content.
pub fn escape_text(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Escape an attribute value (double-quoted).
pub fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('"', "&quot;")
}

/// Local part of a qualified name.
pub fn local_name(qname: &str) -> &str {
    qname.rsplit(':').next().unwrap_or(qname)
}

struct XmlParser<'a> {
    s: &'a [u8],
    text: &'a str,
    i: usize,
    doc: Document,
}

impl<'a> XmlParser<'a> {
    fn err(&self, msg: &str) -> CamelError {
        let (mut line, mut col) = (1, 1);
        for &b in &self.s[..self.i.min(self.s.len())] {
            if b == b'\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        CamelError::new(kinds::PARSE, format!("Invalid XML: {} at line {} column {}", msg, line, col))
    }

    fn starts(&self, p: &str) -> bool {
        self.s[self.i..].starts_with(p.as_bytes())
    }

    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn add(&mut self, parent: usize, kind: NodeKind) -> usize {
        let idx = self.doc.nodes.len();
        self.doc.nodes.push(Node { kind, parent: Some(parent), children: vec![] });
        self.doc.nodes[parent].children.push(idx);
        idx
    }

    fn document(&mut self) -> Result<()> {
        // BOM
        if self.starts("\u{feff}") {
            self.i += 3;
        }
        let mut seen_root = false;
        loop {
            self.ws();
            if self.i >= self.s.len() {
                break;
            }
            if self.starts("<?") {
                self.pi(0)?;
            } else if self.starts("<!--") {
                self.comment(0)?;
            } else if self.starts("<!DOCTYPE") || self.starts("<!doctype") {
                self.doctype()?;
            } else if self.starts("<") {
                if seen_root {
                    return Err(self.err("Content is not allowed after the root element"));
                }
                self.element(0, 0)?;
                seen_root = true;
            } else {
                return Err(self.err("Content is not allowed in prolog"));
            }
        }
        if !seen_root {
            return Err(self.err("Premature end of file (no root element)"));
        }
        Ok(())
    }

    fn doctype(&mut self) -> Result<()> {
        let mut depth = 0;
        while self.i < self.s.len() {
            match self.s[self.i] {
                b'[' => depth += 1,
                b']' => depth -= 1,
                b'>' if depth <= 0 => {
                    self.i += 1;
                    return Ok(());
                }
                _ => {}
            }
            self.i += 1;
        }
        Err(self.err("Unterminated DOCTYPE"))
    }

    fn pi(&mut self, parent: usize) -> Result<()> {
        self.i += 2;
        let st = self.i;
        let end = self.text[self.i..].find("?>").ok_or_else(|| self.err("Unterminated processing instruction"))? + self.i;
        let content = &self.text[st..end];
        self.i = end + 2;
        let (target, data) = match content.find(|c: char| c.is_whitespace()) {
            Some(k) => (&content[..k], content[k..].trim()),
            None => (content, ""),
        };
        if !target.eq_ignore_ascii_case("xml") {
            self.add(parent, NodeKind::ProcessingInstruction { target: target.to_string(), data: data.to_string() });
        }
        Ok(())
    }

    fn comment(&mut self, parent: usize) -> Result<()> {
        self.i += 4;
        let end = self.text[self.i..].find("-->").ok_or_else(|| self.err("Unterminated comment"))? + self.i;
        let c = self.text[self.i..end].to_string();
        self.i = end + 3;
        self.add(parent, NodeKind::Comment(c));
        Ok(())
    }

    fn name(&mut self) -> Result<String> {
        let st = self.i;
        while self.i < self.s.len() {
            let b = self.s[self.i];
            if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.' || b == b':' || b >= 0x80 {
                self.i += 1;
            } else {
                break;
            }
        }
        if st == self.i {
            return Err(self.err("Expected a name"));
        }
        Ok(self.text[st..self.i].to_string())
    }

    fn element(&mut self, parent: usize, depth: usize) -> Result<()> {
        if depth > 1000 {
            return Err(self.err("Element nesting too deep"));
        }
        self.i += 1; // <
        let name = self.name()?;
        let mut attrs = Vec::new();
        loop {
            self.ws();
            if self.i >= self.s.len() {
                return Err(self.err("Unexpected end of input in start tag"));
            }
            if self.starts("/>") {
                self.i += 2;
                self.add(parent, NodeKind::Element { name, attrs });
                return Ok(());
            }
            if self.s[self.i] == b'>' {
                self.i += 1;
                break;
            }
            let an = self.name()?;
            self.ws();
            if self.i >= self.s.len() || self.s[self.i] != b'=' {
                return Err(self.err(&format!("Attribute name \"{}\" must be followed by the '=' character", an)));
            }
            self.i += 1;
            self.ws();
            let q = *self.s.get(self.i).ok_or_else(|| self.err("Unexpected end of input"))?;
            if q != b'"' && q != b'\'' {
                return Err(self.err("Attribute value must be quoted"));
            }
            self.i += 1;
            let st = self.i;
            while self.i < self.s.len() && self.s[self.i] != q {
                self.i += 1;
            }
            if self.i >= self.s.len() {
                return Err(self.err("Unterminated attribute value"));
            }
            let raw = &self.text[st..self.i];
            self.i += 1;
            if attrs.iter().any(|(k, _): &(String, String)| *k == an) {
                return Err(self.err(&format!("Attribute \"{}\" was already specified", an)));
            }
            attrs.push((an, decode_entities(raw).map_err(|m| self.err(&m))?));
        }
        let me = self.add(parent, NodeKind::Element { name: name.clone(), attrs });
        // content
        loop {
            if self.i >= self.s.len() {
                return Err(self.err(&format!("XML document structures must start and end within the same entity (unclosed <{}>)", name)));
            }
            if self.starts("</") {
                self.i += 2;
                let end_name = self.name()?;
                if end_name != name {
                    return Err(self.err(&format!("The element type \"{}\" must be terminated by the matching end-tag \"</{}>\"", name, name)));
                }
                self.ws();
                if self.i >= self.s.len() || self.s[self.i] != b'>' {
                    return Err(self.err("Expected '>'"));
                }
                self.i += 1;
                return Ok(());
            }
            if self.starts("<!--") {
                self.comment(me)?;
            } else if self.starts("<![CDATA[") {
                self.i += 9;
                let end = self.text[self.i..].find("]]>").ok_or_else(|| self.err("Unterminated CDATA section"))? + self.i;
                let t = self.text[self.i..end].to_string();
                self.i = end + 3;
                self.push_text(me, t);
            } else if self.starts("<?") {
                self.pi(me)?;
            } else if self.s[self.i] == b'<' {
                self.element(me, depth + 1)?;
            } else {
                let st = self.i;
                while self.i < self.s.len() && self.s[self.i] != b'<' {
                    self.i += 1;
                }
                let t = decode_entities(&self.text[st..self.i]).map_err(|m| self.err(&m))?;
                self.push_text(me, t);
            }
        }
    }

    fn push_text(&mut self, parent: usize, t: String) {
        // merge adjacent text nodes (text + CDATA)
        if let Some(&last) = self.doc.nodes[parent].children.last() {
            if let NodeKind::Text(prev) = &mut self.doc.nodes[last].kind {
                prev.push_str(&t);
                return;
            }
        }
        self.add(parent, NodeKind::Text(t));
    }
}

/// Decode XML character/entity references.
pub fn decode_entities(s: &str) -> std::result::Result<String, String> {
    if !s.contains('&') {
        return Ok(s.to_string());
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(k) = rest.find('&') {
        out.push_str(&rest[..k]);
        let after = &rest[k + 1..];
        let end = after.find(';').ok_or_else(|| "The entity name must immediately be followed by ';'".to_string())?;
        let ent = &after[..end];
        match ent {
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "amp" => out.push('&'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            e if e.starts_with("#x") || e.starts_with("#X") => {
                let v = u32::from_str_radix(&e[2..], 16).map_err(|_| format!("Invalid character reference &{};", e))?;
                out.push(char::from_u32(v).ok_or_else(|| format!("Invalid character reference &{};", e))?);
            }
            e if e.starts_with('#') => {
                let v: u32 = e[1..].parse().map_err(|_| format!("Invalid character reference &{};", e))?;
                out.push(char::from_u32(v).ok_or_else(|| format!("Invalid character reference &{};", e))?);
            }
            e => return Err(format!("The entity \"{}\" was referenced, but not declared", e)),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

// ---------------------------------------------------------------------------
// XML <-> Value
// ---------------------------------------------------------------------------

/// Convert an element to a Value (Jackson XML style): attributes and child elements become
/// object keys (repeated elements become arrays), text-only elements become strings and
/// mixed text is stored under the `""` key.
pub fn element_to_value(doc: &Document, i: usize) -> Value {
    let attrs = doc.attrs(i);
    let elems = doc.element_children(i);
    let text: String = doc.nodes[i]
        .children
        .iter()
        .filter_map(|&c| match &doc.nodes[c].kind {
            NodeKind::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    if attrs.is_empty() && elems.is_empty() {
        return Value::String(text);
    }
    let mut m: BTreeMap<String, Value> = BTreeMap::new();
    for (k, v) in attrs {
        if k == "xmlns" || k.starts_with("xmlns:") {
            continue;
        }
        m.insert(local_name(k).to_string(), Value::String(v.clone()));
    }
    let mut grouped: Vec<(String, Vec<Value>)> = Vec::new();
    for c in elems {
        let key = local_name(doc.name(c)).to_string();
        let v = element_to_value(doc, c);
        match grouped.iter_mut().find(|(k, _)| *k == key) {
            Some((_, list)) => list.push(v),
            None => grouped.push((key, vec![v])),
        }
    }
    for (k, mut list) in grouped {
        let v = if list.len() == 1 { list.pop().unwrap() } else { Value::Array(list) };
        m.insert(k, v);
    }
    if !text.trim().is_empty() {
        m.insert(String::new(), Value::String(text.trim().to_string()));
    }
    Value::Object(m)
}

/// Parse XML text into a Value (root element name dropped).
pub fn xml_to_value(text: &str) -> Result<Value> {
    let doc = Document::parse(text)?;
    let root = doc.root_element().ok_or_else(|| CamelError::new(kinds::PARSE, "No root element"))?;
    Ok(element_to_value(&doc, root))
}

fn write_value(name: &str, v: &Value, out: &mut String, indent: Option<usize>, level: usize) {
    let pad = |out: &mut String, lvl: usize| {
        if let Some(n) = indent {
            out.push('\n');
            out.push_str(&" ".repeat(n * lvl));
        }
    };
    match v {
        Value::Array(items) => {
            for (k, it) in items.iter().enumerate() {
                if k > 0 {
                    pad(out, level);
                }
                write_value(name, it, out, indent, level);
            }
        }
        Value::Object(o) => {
            out.push('<');
            out.push_str(name);
            out.push('>');
            let mut any = false;
            for (k, child) in o {
                if k.is_empty() {
                    out.push_str(&escape_text(&child.to_string()));
                    continue;
                }
                any = true;
                pad(out, level + 1);
                write_value(k, child, out, indent, level + 1);
            }
            if any {
                pad(out, level);
            }
            out.push_str("</");
            out.push_str(name);
            out.push('>');
        }
        Value::Null => {
            out.push('<');
            out.push_str(name);
            out.push_str("/>");
        }
        other => {
            out.push('<');
            out.push_str(name);
            out.push('>');
            out.push_str(&escape_text(&other.to_string()));
            out.push_str("</");
            out.push_str(name);
            out.push('>');
        }
    }
}

/// Serialize a Value as XML with the given root element name.
pub fn value_to_xml(v: &Value, root: &str, pretty: bool) -> String {
    let mut out = String::new();
    let indent = if pretty { Some(2) } else { None };
    match v {
        Value::Array(items) => {
            out.push('<');
            out.push_str(root);
            out.push('>');
            for it in items {
                if pretty {
                    out.push_str("\n  ");
                }
                write_value("item", it, &mut out, indent, 1);
            }
            if pretty && !items.is_empty() {
                out.push('\n');
            }
            out.push_str("</");
            out.push_str(root);
            out.push('>');
        }
        other => write_value(root, other, &mut out, indent, 0),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_serialize() {
        let d = Document::parse("<?xml version=\"1.0\"?><!-- c --><a x=\"1\"><b>t&amp;t</b><![CDATA[<raw>]]><c/></a>").unwrap();
        let r = d.root_element().unwrap();
        assert_eq!(d.name(r), "a");
        assert_eq!(d.attr(r, "x"), Some("1"));
        assert_eq!(d.text_content(r), "t&t<raw>");
        assert_eq!(d.serialize(r), "<a x=\"1\"><b>t&amp;t</b>&lt;raw&gt;<c/></a>");
        assert!(Document::parse("<a><b></a>").is_err());
        assert!(Document::parse("<a>").is_err());
        assert!(Document::parse("<a></a><b/>").is_err());
        assert!(Document::parse("<a x=1/>").is_err());
    }

    #[test]
    fn value_mapping() {
        let v = xml_to_value("<order id=\"7\"><item>a</item><item>b</item><total>3</total></order>").unwrap();
        assert_eq!(v.get("id"), Some(&Value::from("7")));
        assert_eq!(v.get("item"), Some(&Value::from(vec!["a", "b"])));
        let x = value_to_xml(&v, "order", false);
        let back = xml_to_value(&x).unwrap();
        assert_eq!(back, v);
    }
}

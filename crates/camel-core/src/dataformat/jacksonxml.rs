//! The `jacksonXml` data format (aliases `xml`, `jaxb`): structured value <-> XML.
//!
//! Mapping (Jackson `XmlMapper` style): object keys become child elements, arrays repeat the
//! element, scalars become text. Like Jackson's `@JacksonXmlProperty(isAttribute = true)`,
//! `@JacksonXmlProperty(localName/namespace)`, `@JacksonXmlText` and
//! `@JacksonXmlElementWrapper` the mapping is tuned with options:
//!
//! | option | meaning |
//! |---|---|
//! | `rootName` | root element name (default `root`); may be `p:name` or `{uri}name` |
//! | `prettyPrint` | indent output |
//! | `contentTypeHeader` | set `Content-Type: application/xml` (default true) |
//! | `xmlDeclaration` | write `<?xml version="1.0" encoding="UTF-8"?>` |
//! | `attributePrefix` | keys starting with this prefix (default `@`) are written as attributes; when set explicitly, unmarshal prefixes attribute keys with it |
//! | `attributes` | comma separated keys always written as attributes (`isAttribute = true`) |
//! | `textProperty` | key holding the element text (`@JacksonXmlText`, default `""`; `#text` and `$` are always accepted on marshal) |
//! | `properties` | `key:xmlName,...` renames (`localName`); `xmlName` may be `p:local` or `{uri}local` |
//! | `namespace` | default namespace of the root element (`xmlns="..."`) |
//! | `namespaces` | `prefix=uri,...` prefixes declared on the root element |
//! | `wrappers` | `key:itemName,...` arrays written inside a wrapper element (`@JacksonXmlElementWrapper`), unwrapped again on unmarshal |
//!
//! Unmarshal drops namespace prefixes (keys are local names, mapped back through `properties`).

use std::collections::BTreeMap;

use crate::error::{CamelError, Result};
use crate::language::xml::{escape_attr, escape_text, local_name, Document, NodeKind};
use crate::message::Exchange;
use crate::value::{ObjectMap, Value};

use super::DataFormat;

/// The `jacksonXml` data format; see the module documentation for the options.
#[derive(Clone, Debug)]
pub struct XmlDataFormat {
    pub root_name: String,
    pub pretty_print: bool,
    pub content_type_header: bool,
    pub xml_declaration: bool,
    pub attribute_prefix: String,
    /// Unmarshal attribute keys with `attribute_prefix`.
    pub prefix_attributes_on_unmarshal: bool,
    pub attributes: Vec<String>,
    pub text_property: String,
    /// key -> XML name.
    pub properties: Vec<(String, String)>,
    pub default_namespace: Option<String>,
    /// prefix -> uri.
    pub namespaces: Vec<(String, String)>,
    /// key -> item element name.
    pub wrappers: Vec<(String, String)>,
}

impl Default for XmlDataFormat {
    fn default() -> Self {
        XmlDataFormat {
            root_name: "root".into(),
            pretty_print: false,
            content_type_header: true,
            xml_declaration: false,
            attribute_prefix: "@".into(),
            prefix_attributes_on_unmarshal: false,
            attributes: Vec::new(),
            text_property: String::new(),
            properties: Vec::new(),
            default_namespace: None,
            namespaces: Vec::new(),
            wrappers: Vec::new(),
        }
    }
}

fn pairs(s: &str, sep: char) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for item in s.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        // `{uri}local` values contain ':' — split on the first separator outside braces.
        let mut depth = 0;
        let mut at = None;
        for (i, c) in item.char_indices() {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                c if c == sep && depth == 0 => {
                    at = Some(i);
                    break;
                }
                _ => {}
            }
        }
        let i = at.ok_or_else(|| CamelError::illegal_argument(format!("Invalid mapping '{}': expected key{}value", item, sep)))?;
        out.push((item[..i].trim().to_string(), item[i + 1..].trim().to_string()));
    }
    Ok(out)
}

impl XmlDataFormat {
    /// Build from data format options.
    pub fn from_options(o: &BTreeMap<String, String>) -> Result<XmlDataFormat> {
        let b = |k: &str, d: bool| -> Result<bool> {
            match o.get(k).map(|s| s.to_ascii_lowercase()) {
                None => Ok(d),
                Some(v) if v == "true" => Ok(true),
                Some(v) if v == "false" => Ok(false),
                Some(v) => Err(CamelError::illegal_argument(format!("Option {} must be true or false but was: {}", k, v))),
            }
        };
        let mut df = XmlDataFormat {
            root_name: o.get("rootName").cloned().unwrap_or_else(|| "root".into()),
            pretty_print: b("prettyPrint", false)?,
            content_type_header: b("contentTypeHeader", true)?,
            xml_declaration: b("xmlDeclaration", false)?,
            ..Default::default()
        };
        if let Some(p) = o.get("attributePrefix") {
            df.attribute_prefix = p.clone();
            df.prefix_attributes_on_unmarshal = !p.is_empty();
        }
        if let Some(a) = o.get("attributes") {
            df.attributes = a.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        }
        if let Some(t) = o.get("textProperty") {
            df.text_property = t.clone();
        }
        if let Some(p) = o.get("properties") {
            df.properties = pairs(p, ':')?;
        }
        df.default_namespace = o.get("namespace").filter(|s| !s.is_empty()).cloned();
        if let Some(n) = o.get("namespaces") {
            df.namespaces = pairs(n, '=')?;
        }
        if let Some(w) = o.get("wrappers") {
            df.wrappers = pairs(w, ':')?;
        }
        Ok(df)
    }

    fn xml_name_of(&self, key: &str) -> String {
        self.properties.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()).unwrap_or_else(|| key.to_string())
    }

    fn key_of(&self, local: &str) -> String {
        self.properties
            .iter()
            .find(|(_, v)| local_of(v) == local)
            .map(|(k, _)| k.clone())
            .unwrap_or_else(|| local.to_string())
    }

    fn is_text_key(&self, k: &str) -> bool {
        k == self.text_property || k == "#text" || k == "$" || k.is_empty()
    }

    /// Value -> XML text.
    pub fn to_xml(&self, v: &Value) -> String {
        let mut w = Writer { df: self, out: String::new(), extra_ns: Vec::new(), indent: if self.pretty_print { Some(2) } else { None } };
        if self.xml_declaration {
            w.out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
            if self.pretty_print {
                w.out.push('\n');
            }
        }
        let root = self.root_name.clone();
        match v {
            Value::Array(items) => {
                let mut o = ObjectMap::new();
                o.insert("item".to_string(), Value::Array(items.clone()));
                w.element(&root, &Value::Object(o), 0, true);
            }
            other => w.element(&root, other, 0, true),
        }
        w.out
    }

    /// XML text -> Value.
    pub fn from_xml(&self, text: &str) -> Result<Value> {
        let doc = Document::parse(text)?;
        let root = doc.root_element().ok_or_else(|| CamelError::new(crate::error::kinds::PARSE, "No root element"))?;
        Ok(self.element_to_value(&doc, root))
    }

    fn element_to_value(&self, doc: &Document, i: usize) -> Value {
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
        let real_attrs: Vec<&(String, String)> = attrs.iter().filter(|(k, _)| k != "xmlns" && !k.starts_with("xmlns:") && !k.starts_with("xsi:")).collect();
        if real_attrs.is_empty() && elems.is_empty() {
            return Value::String(text);
        }
        let mut m = ObjectMap::new();
        for (k, v) in real_attrs {
            let key = self.key_of(local_name(k));
            let key = if self.prefix_attributes_on_unmarshal && !self.attributes.contains(&key) { format!("{}{}", self.attribute_prefix, key) } else { key };
            m.insert(key, Value::String(v.clone()));
        }
        let mut grouped: Vec<(String, Vec<Value>)> = Vec::new();
        for c in elems {
            let key = self.key_of(local_name(doc.name(c)));
            let v = if let Some((_, item)) = self.wrappers.iter().find(|(k, _)| *k == key) {
                let items: Vec<Value> = doc.element_children(c).into_iter().filter(|&g| local_name(doc.name(g)) == local_of(item)).map(|g| self.element_to_value(doc, g)).collect();
                grouped.push((key, vec![Value::Array(items)]));
                continue;
            } else {
                self.element_to_value(doc, c)
            };
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
            m.insert(self.text_property.clone(), Value::String(text.trim().to_string()));
        }
        Value::Object(m)
    }
}

fn local_of(name: &str) -> &str {
    if let Some(rest) = name.strip_prefix('{') {
        if let Some(i) = rest.find('}') {
            return &rest[i + 1..];
        }
    }
    local_name(name)
}

struct Writer<'a> {
    df: &'a XmlDataFormat,
    out: String,
    /// Generated prefixes for `{uri}local` names: (prefix, uri).
    extra_ns: Vec<(String, String)>,
    indent: Option<usize>,
}

impl Writer<'_> {
    fn pad(&mut self, level: usize) {
        if let Some(n) = self.indent {
            self.out.push('\n');
            self.out.push_str(&" ".repeat(n * level));
        }
    }

    /// Resolve `{uri}local` to a qualified name, returning a namespace declaration to add when
    /// the uri has no prefix in scope yet.
    fn qname(&mut self, name: &str, decls: &mut Vec<(String, String)>, root: bool) -> String {
        let Some(rest) = name.strip_prefix('{') else { return name.to_string() };
        let Some(end) = rest.find('}') else { return name.to_string() };
        let (uri, local) = (&rest[..end], &rest[end + 1..]);
        if self.df.default_namespace.as_deref() == Some(uri) {
            return local.to_string();
        }
        if let Some((p, _)) = self.df.namespaces.iter().chain(self.extra_ns.iter()).find(|(_, u)| u == uri) {
            return format!("{}:{}", p, local);
        }
        let p = format!("ns{}", self.extra_ns.len() + 1);
        decls.push((p.clone(), uri.to_string()));
        // Declarations on the root stay in scope for the whole document; others are redeclared
        // locally where used.
        if root {
            self.extra_ns.push((p.clone(), uri.to_string()));
        }
        format!("{}:{}", p, local)
    }

    fn element(&mut self, name: &str, v: &Value, level: usize, root: bool) {
        let mut decls: Vec<(String, String)> = Vec::new();
        let qn = self.qname(name, &mut decls, root);
        self.out.push('<');
        self.out.push_str(&qn);
        if root {
            if let Some(ns) = &self.df.default_namespace {
                self.out.push_str(&format!(" xmlns=\"{}\"", escape_attr(ns)));
            }
            for (p, u) in &self.df.namespaces {
                self.out.push_str(&format!(" xmlns:{}=\"{}\"", p, escape_attr(u)));
            }
        }
        let start_decl = self.out.len();
        let o = match v {
            Value::Object(o) => o,
            Value::Null => {
                self.write_decls(&decls, start_decl);
                self.out.push_str("/>");
                return;
            }
            other => {
                self.write_decls(&decls, start_decl);
                self.out.push('>');
                self.out.push_str(&escape_text(&other.to_string()));
                self.out.push_str("</");
                self.out.push_str(&qn);
                self.out.push('>');
                return;
            }
        };
        let mut text = String::new();
        let mut children: Vec<(&String, &Value)> = Vec::new();
        let mut attr_buf = String::new();
        for (k, child) in o {
            if self.df.is_text_key(k) {
                text.push_str(&child.to_string());
                continue;
            }
            let attr_key = if !self.df.attribute_prefix.is_empty() { k.strip_prefix(self.df.attribute_prefix.as_str()) } else { None };
            let is_attr = attr_key.is_some() || self.df.attributes.iter().any(|a| a == k);
            if is_attr && !matches!(child, Value::Object(_) | Value::Array(_)) {
                let key = attr_key.unwrap_or(k);
                let an = self.df.xml_name_of(key);
                let aq = if an.starts_with('{') { self.qname(&an, &mut decls, root) } else { an };
                attr_buf.push_str(&format!(" {}=\"{}\"", aq, escape_attr(&child.to_string())));
                continue;
            }
            children.push((k, child));
        }
        self.write_decls(&decls, start_decl);
        self.out.push_str(&attr_buf);
        if children.is_empty() && text.is_empty() {
            self.out.push_str("/>");
            return;
        }
        self.out.push('>');
        self.out.push_str(&escape_text(&text));
        for (k, child) in &children {
            let xml_name = self.df.xml_name_of(k);
            if let Some((_, item)) = self.df.wrappers.iter().find(|(wk, _)| wk == *k).cloned() {
                self.pad(level + 1);
                let items = match child {
                    Value::Array(a) => a.clone(),
                    Value::Null => Vec::new(),
                    other => vec![(*other).clone()],
                };
                let mut d2 = Vec::new();
                let wq = self.qname(&xml_name, &mut d2, false);
                self.out.push('<');
                self.out.push_str(&wq);
                self.write_decls(&d2, self.out.len());
                if items.is_empty() {
                    self.out.push_str("/>");
                    continue;
                }
                self.out.push('>');
                for it in &items {
                    self.pad(level + 2);
                    self.element(&item, it, level + 2, false);
                }
                self.pad(level + 1);
                self.out.push_str(&format!("</{}>", wq));
                continue;
            }
            match child {
                Value::Array(items) => {
                    for it in items {
                        self.pad(level + 1);
                        self.element(&xml_name, it, level + 1, false);
                    }
                }
                other => {
                    self.pad(level + 1);
                    self.element(&xml_name, other, level + 1, false);
                }
            }
        }
        if !children.is_empty() {
            self.pad(level);
        }
        self.out.push_str("</");
        self.out.push_str(&qn);
        self.out.push('>');
    }

    fn write_decls(&mut self, decls: &[(String, String)], at: usize) {
        let mut s = String::new();
        for (p, u) in decls {
            s.push_str(&format!(" xmlns:{}=\"{}\"", p, escape_attr(u)));
        }
        self.out.insert_str(at, &s);
    }
}

fn body_text(v: &Value) -> Result<String> {
    Ok(match v {
        Value::Bytes(b) => String::from_utf8(b.clone()).map_err(|e| CamelError::new("DataFormatException", format!("Body is not valid UTF-8: {}", e)))?,
        other => other.to_string(),
    })
}

impl DataFormat for XmlDataFormat {
    fn name(&self) -> &str {
        "jacksonXml"
    }
    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        let v = match &ex.message.body {
            Value::String(_) | Value::Bytes(_) => {
                let t = body_text(&ex.message.body)?;
                match crate::json::parse(&t) {
                    Ok(j @ (Value::Object(_) | Value::Array(_))) => j,
                    _ => Value::String(t),
                }
            }
            other => other.clone(),
        };
        ex.message.body = Value::String(self.to_xml(&v));
        if self.content_type_header {
            ex.message.headers.set(crate::message::props::CONTENT_TYPE, "application/xml");
        }
        Ok(())
    }
    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        let t = body_text(&ex.message.body)?;
        ex.message.body = self.from_xml(&t)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn df(opts: &[(&str, &str)]) -> XmlDataFormat {
        let o: BTreeMap<String, String> = opts.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        XmlDataFormat::from_options(&o).unwrap()
    }

    #[test]
    fn plain_mapping_unchanged() {
        let d = df(&[("rootName", "order")]);
        let v = crate::json::parse(r#"{"id":"1","items":["a","b"],"n":null}"#).unwrap();
        assert_eq!(d.to_xml(&v), "<order><id>1</id><items>a</items><items>b</items><n/></order>");
        assert_eq!(d.from_xml("<order><id>1</id><items>a</items><items>b</items><n/></order>").unwrap().get("items").unwrap(), &Value::Array(vec!["a".into(), "b".into()]));
    }

    #[test]
    fn attributes_and_text() {
        let d = df(&[("rootName", "price"), ("attributes", "currency")]);
        let v = crate::json::parse(r#"{"currency":"EUR","@scale":2,"":"9.99"}"#).unwrap();
        assert_eq!(d.to_xml(&v), r#"<price currency="EUR" scale="2">9.99</price>"#);
        let d2 = df(&[("rootName", "price"), ("attributePrefix", "@"), ("textProperty", "value")]);
        let back = d2.from_xml(r#"<price currency="EUR">9.99</price>"#).unwrap();
        assert_eq!(back.to_json(), r#"{"@currency":"EUR","value":"9.99"}"#);
        assert_eq!(d2.to_xml(&back), r#"<price currency="EUR">9.99</price>"#);
    }

    #[test]
    fn namespaces_and_property_names() {
        let d = df(&[
            ("rootName", "{urn:o}order"),
            ("namespace", "urn:o"),
            ("namespaces", "c=urn:c"),
            ("properties", "customerName:c:name,ref:{urn:x}ref,id:OrderId"),
            ("attributes", "id"),
        ]);
        let v = crate::json::parse(r#"{"id":"7","customerName":"Ann","ref":"r1"}"#).unwrap();
        let xml = d.to_xml(&v);
        assert_eq!(xml, r#"<order xmlns="urn:o" xmlns:c="urn:c" OrderId="7"><c:name>Ann</c:name><ns1:ref xmlns:ns1="urn:x">r1</ns1:ref></order>"#);
        let back = d.from_xml(&xml).unwrap();
        assert_eq!(back.to_json(), r#"{"id":"7","customerName":"Ann","ref":"r1"}"#);
    }

    #[test]
    fn wrappers_and_declaration() {
        let d = df(&[("rootName", "cart"), ("wrappers", "items:item"), ("xmlDeclaration", "true"), ("prettyPrint", "true")]);
        let v = crate::json::parse(r#"{"items":[{"sku":"a"},{"sku":"b"}]}"#).unwrap();
        let xml = d.to_xml(&v);
        assert_eq!(xml, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<cart>\n  <items>\n    <item>\n      <sku>a</sku>\n    </item>\n    <item>\n      <sku>b</sku>\n    </item>\n  </items>\n</cart>");
        let back = d.from_xml(&xml).unwrap();
        assert_eq!(back.to_json(), r#"{"items":[{"sku":"a"},{"sku":"b"}]}"#);
        let one = d.from_xml("<cart><items><item><sku>a</sku></item></items></cart>").unwrap();
        assert_eq!(one.to_json(), r#"{"items":[{"sku":"a"}]}"#);
    }
}

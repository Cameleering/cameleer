//! Minimal XML parser and Camel XML DSL loader.
//!
//! XML routes are converted to the same [`Value`] shape as the YAML DSL and then
//! loaded with the YAML step handlers:
//!
//! ```xml
//! <routes>
//!   <route id="r1">
//!     <from uri="direct:start"/>
//!     <setBody><simple>${body}</simple></setBody>
//!     <to uri="mock:result"/>
//!   </route>
//! </routes>
//! ```
//!
//! Conversion rules for an element: attributes become options; child elements named
//! after an expression language become the expression; child elements that are
//! registered steps become `steps`; other child elements become nested options
//! (`when` / `doCatch` / `exception` are always lists, repeated elements become lists);
//! an element with only text becomes a string.

use std::collections::BTreeSet;

use camel_core::error::kinds;
use camel_core::model::RouteDefinition;
use camel_core::{CamelContext, CamelError, Result, Value, value::ObjectMap};

use crate::dsl::{StepRegistry, YamlLoader, LANGUAGES};

/// Child elements that are always converted to lists.
const ARRAY_KEYS: &[&str] = &["when", "doCatch", "exception", "onWhen"];

/// A parsed XML element (namespace prefixes are stripped from names).
#[derive(Clone, Debug, PartialEq)]
pub struct Element {
    pub name: String,
    pub attributes: Vec<(String, String)>,
    pub children: Vec<XmlNode>,
    pub line: usize,
}

/// Element content.
#[derive(Clone, Debug, PartialEq)]
pub enum XmlNode {
    Element(Element),
    Text(String),
}

impl Element {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attributes.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    /// Child elements.
    pub fn elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|c| match c {
            XmlNode::Element(e) => Some(e),
            _ => None,
        })
    }

    /// Concatenated text content (direct children only).
    pub fn text(&self) -> String {
        self.children
            .iter()
            .filter_map(|c| match c {
                XmlNode::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

struct XmlParser<'a> {
    s: &'a [u8],
    src: &'a str,
    pos: usize,
}

fn xml_error(msg: impl std::fmt::Display, src: &str, pos: usize) -> CamelError {
    let before = &src[..pos.min(src.len())];
    let line = before.matches('\n').count() + 1;
    let col = before.rfind('\n').map(|i| before[i + 1..].chars().count()).unwrap_or_else(|| before.chars().count()) + 1;
    CamelError::new(kinds::PARSE, format!("Invalid XML: {} at line {}, column {}", msg, line, col))
}

/// Parse an XML document and return its root element.
pub fn parse_xml(text: &str) -> Result<Element> {
    let mut p = XmlParser { s: text.as_bytes(), src: text, pos: 0 };
    if text.starts_with('\u{feff}') {
        p.pos = 3;
    }
    p.skip_misc()?;
    if p.peek() != Some(b'<') {
        return Err(p.err("expected a root element"));
    }
    let root = p.parse_element()?;
    p.skip_misc()?;
    if p.pos < p.s.len() {
        return Err(p.err("unexpected content after the root element"));
    }
    Ok(root)
}

impl<'a> XmlParser<'a> {
    fn err(&self, msg: impl std::fmt::Display) -> CamelError {
        xml_error(msg, self.src, self.pos)
    }
    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }
    fn starts(&self, t: &str) -> bool {
        self.s[self.pos..].starts_with(t.as_bytes())
    }
    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.pos += 1;
        }
    }
    fn skip_until(&mut self, end: &str, what: &str) -> Result<()> {
        let start = self.pos;
        match self.src[self.pos..].find(end) {
            Some(i) => {
                self.pos += i + end.len();
                Ok(())
            }
            None => {
                self.pos = start;
                Err(self.err(format!("unterminated {}", what)))
            }
        }
    }
    /// Skip whitespace, comments, processing instructions and DOCTYPE.
    fn skip_misc(&mut self) -> Result<()> {
        loop {
            self.skip_ws();
            if self.starts("<?") {
                self.skip_until("?>", "processing instruction")?;
            } else if self.starts("<!--") {
                self.skip_until("-->", "comment")?;
            } else if self.starts("<!DOCTYPE") {
                self.skip_until(">", "DOCTYPE")?;
            } else {
                return Ok(());
            }
        }
    }
    fn parse_name(&mut self) -> Result<String> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b':') || c >= 0x80 {
                self.pos += 1;
            } else {
                break;
            }
        }
        if start == self.pos {
            return Err(self.err("expected a name"));
        }
        Ok(self.src[start..self.pos].to_string())
    }
    fn line(&self) -> usize {
        self.src[..self.pos].matches('\n').count() + 1
    }
    fn parse_element(&mut self) -> Result<Element> {
        let line = self.line();
        self.pos += 1; // '<'
        let qname = self.parse_name()?;
        let name = local(&qname);
        let mut attributes = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                Some(b'/') => {
                    self.pos += 1;
                    if self.peek() != Some(b'>') {
                        return Err(self.err("expected '>' after '/'"));
                    }
                    self.pos += 1;
                    return Ok(Element { name, attributes, children: Vec::new(), line });
                }
                Some(b'>') => {
                    self.pos += 1;
                    break;
                }
                None => return Err(self.err(format!("unterminated start tag <{}>", qname))),
                _ => {
                    let an = self.parse_name()?;
                    self.skip_ws();
                    if self.peek() != Some(b'=') {
                        return Err(self.err(format!("expected '=' after attribute '{}'", an)));
                    }
                    self.pos += 1;
                    self.skip_ws();
                    let q = match self.peek() {
                        Some(q @ (b'"' | b'\'')) => q,
                        _ => return Err(self.err(format!("expected a quoted value for attribute '{}'", an))),
                    };
                    self.pos += 1;
                    let start = self.pos;
                    while self.peek().map(|c| c != q).unwrap_or(false) {
                        self.pos += 1;
                    }
                    if self.peek().is_none() {
                        return Err(self.err(format!("unterminated value of attribute '{}'", an)));
                    }
                    let raw = &self.src[start..self.pos];
                    self.pos += 1;
                    let value = decode_entities(raw).map_err(|m| xml_error(m, self.src, start))?;
                    if an == "xmlns" || an.starts_with("xmlns:") {
                        continue;
                    }
                    attributes.push((local(&an), value));
                }
            }
        }
        let mut children = Vec::new();
        let mut text = String::new();
        loop {
            if self.peek().is_none() {
                return Err(self.err(format!("missing end tag </{}>", qname)));
            }
            if self.starts("</") {
                if !text.is_empty() {
                    children.push(XmlNode::Text(std::mem::take(&mut text)));
                }
                self.pos += 2;
                let end = self.parse_name()?;
                if end != qname {
                    return Err(self.err(format!("mismatched end tag: expected </{}> but found </{}>", qname, end)));
                }
                self.skip_ws();
                if self.peek() != Some(b'>') {
                    return Err(self.err("expected '>'"));
                }
                self.pos += 1;
                return Ok(Element { name, attributes, children, line });
            } else if self.starts("<!--") {
                self.skip_until("-->", "comment")?;
            } else if self.starts("<![CDATA[") {
                self.pos += 9;
                let start = self.pos;
                self.skip_until("]]>", "CDATA section")?;
                text.push_str(&self.src[start..self.pos - 3]);
            } else if self.starts("<?") {
                self.skip_until("?>", "processing instruction")?;
            } else if self.peek() == Some(b'<') {
                if !text.is_empty() {
                    children.push(XmlNode::Text(std::mem::take(&mut text)));
                }
                children.push(XmlNode::Element(self.parse_element()?));
            } else {
                let start = self.pos;
                while self.peek().map(|c| c != b'<').unwrap_or(false) {
                    self.pos += 1;
                }
                text.push_str(&decode_entities(&self.src[start..self.pos]).map_err(|m| xml_error(m, self.src, start))?);
            }
        }
    }
}

fn local(qname: &str) -> String {
    qname.rsplit(':').next().unwrap_or(qname).to_string()
}

fn decode_entities(s: &str) -> std::result::Result<String, String> {
    if !s.contains('&') {
        return Ok(s.to_string());
    }
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let end = after.find(';').ok_or_else(|| "unterminated entity reference".to_string())?;
        let ent = &after[..end];
        let c = match ent {
            "lt" => '<',
            "gt" => '>',
            "amp" => '&',
            "quot" => '"',
            "apos" => '\'',
            _ if ent.starts_with("#x") || ent.starts_with("#X") => {
                u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32).ok_or_else(|| format!("invalid character reference &{};", ent))?
            }
            _ if ent.starts_with('#') => ent[1..].parse::<u32>().ok().and_then(char::from_u32).ok_or_else(|| format!("invalid character reference &{};", ent))?,
            _ => return Err(format!("unknown entity &{};", ent)),
        };
        out.push(c);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Conversion to the YAML DSL shape
// ---------------------------------------------------------------------------

fn attrs_map(el: &Element) -> ObjectMap {
    el.attributes.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect()
}

fn insert_child(map: &mut ObjectMap, repeated: &mut BTreeSet<String>, key: &str, v: Value) {
    if ARRAY_KEYS.contains(&key) || repeated.contains(key) {
        match map.get_mut(key) {
            Some(Value::Array(a)) => a.push(v),
            _ => {
                map.insert(key.to_string(), Value::Array(vec![v]));
            }
        }
        return;
    }
    match map.remove(key) {
        None => {
            map.insert(key.to_string(), v);
        }
        Some(prev) => {
            // repeated element: turn into a list
            repeated.insert(key.to_string());
            map.insert(key.to_string(), Value::Array(vec![prev, v]));
        }
    }
}

/// Convert an element body (attributes + children) to the YAML option shape.
fn convert_body(el: &Element, steps: &StepRegistry) -> Value {
    let elements: Vec<&Element> = el.elements().collect();
    let text = el.text();
    if el.attributes.is_empty() && elements.is_empty() {
        let t = text.trim();
        return if t.is_empty() { Value::Object(ObjectMap::new()) } else { Value::String(t.to_string()) };
    }
    let mut map = attrs_map(el);
    let mut step_list: Vec<Value> = Vec::new();
    let mut repeated = BTreeSet::new();
    for child in elements {
        let name = child.name.as_str();
        if LANGUAGES.contains(&name) {
            let body = child.text().trim().to_string();
            let v = if child.attributes.is_empty() {
                Value::String(body)
            } else {
                let mut m = attrs_map(child);
                if !body.is_empty() {
                    m.insert("expression".into(), Value::String(body));
                }
                Value::Object(m)
            };
            map.insert(name.to_string(), v);
        } else if (el.name == "setHeaders" && name == "setHeader") || (el.name == "setVariables" && name == "setVariable") {
            let key = if name == "setHeader" { "headers" } else { "variables" };
            let v = convert_body(child, steps);
            match map.get_mut(key) {
                Some(Value::Array(a)) => a.push(v),
                _ => {
                    map.insert(key.to_string(), Value::Array(vec![v]));
                }
            }
        } else if steps.contains(name) {
            step_list.push(convert_step(child, steps));
        } else {
            insert_child(&mut map, &mut repeated, name, convert_body(child, steps));
        }
    }
    if !step_list.is_empty() {
        map.insert("steps".into(), Value::Array(step_list));
    }
    Value::Object(map)
}

fn convert_step(el: &Element, steps: &StepRegistry) -> Value {
    Value::object([(el.name.clone(), convert_body(el, steps))])
}

fn convert_route(el: &Element, steps: &StepRegistry) -> Result<Value> {
    let mut route = attrs_map(el);
    let mut from: Option<ObjectMap> = None;
    let mut step_list = Vec::new();
    for child in el.elements() {
        match child.name.as_str() {
            "from" => from = Some(attrs_map(child)),
            "description" => {
                route.insert("description".into(), Value::String(child.text().trim().to_string()));
            }
            _ => step_list.push(convert_step(child, steps)),
        }
    }
    let mut from = from.ok_or_else(|| CamelError::illegal_argument(format!("<route> at line {} requires a <from> element", el.line)))?;
    from.insert("steps".into(), Value::Array(step_list));
    route.insert("from".into(), Value::Object(from));
    Ok(Value::Object(route))
}

fn convert_top(el: &Element, steps: &StepRegistry, out: &mut Vec<Value>) -> Result<()> {
    match el.name.as_str() {
        "routes" | "camel" | "camelContext" | "routeTemplates" | "templatedRoutes" => {
            for c in el.elements() {
                convert_top(c, steps, out)?;
            }
        }
        "route" => out.push(Value::object([("route", convert_route(el, steps)?)])),
        "routeTemplate" => {
            let mut t = attrs_map(el);
            let mut params = Vec::new();
            for c in el.elements() {
                match c.name.as_str() {
                    "templateParameter" => params.push(Value::Object(attrs_map(c))),
                    "route" => {
                        t.insert("route".into(), convert_route(c, steps)?);
                    }
                    "description" => {
                        t.insert("description".into(), Value::String(c.text().trim().to_string()));
                    }
                    other => return Err(CamelError::illegal_argument(format!("unexpected <{}> in <routeTemplate> at line {}", other, c.line))),
                }
            }
            t.insert("parameters".into(), Value::Array(params));
            out.push(Value::object([("routeTemplate", Value::Object(t))]));
        }
        "templatedRoute" => {
            let mut t = attrs_map(el);
            let params: Vec<Value> = el.elements().filter(|c| c.name == "parameter").map(|c| Value::Object(attrs_map(c))).collect();
            t.insert("parameters".into(), Value::Array(params));
            out.push(Value::object([("templatedRoute", Value::Object(t))]));
        }
        "beans" => {
            for c in el.elements() {
                convert_top(c, steps, out)?;
            }
        }
        "bean" => {
            let mut b = attrs_map(el);
            for c in el.elements() {
                if c.name == "properties" {
                    let props: ObjectMap = c
                        .elements()
                        .filter_map(|p| Some((p.attr("key")?.to_string(), Value::String(p.attr("value").unwrap_or("").to_string()))))
                        .collect();
                    b.insert("properties".into(), Value::Object(props));
                }
            }
            out.push(Value::object([("beans", Value::Array(vec![Value::Object(b)]))]));
        }
        other => out.push(Value::object([(other.to_string(), convert_body(el, steps))])),
    }
    Ok(())
}

/// Convert a parsed XML DSL document to the YAML DSL value shape (a list of
/// top-level elements). `steps` is used to tell steps from nested options.
pub fn xml_to_value(root: &Element, steps: &StepRegistry) -> Result<Value> {
    let mut out = Vec::new();
    convert_top(root, steps, &mut out)?;
    Ok(Value::Array(out))
}

/// Load routes from XML DSL text with the given loader.
pub fn load_routes_with(loader: &YamlLoader, ctx: &CamelContext, xml: &str) -> Result<Vec<RouteDefinition>> {
    let root = parse_xml(xml)?;
    let value = xml_to_value(&root, &loader.steps)?;
    loader.load_routes_from_value(ctx, &value)
}

/// Load routes from XML DSL text.
pub fn load_routes_from_xml(ctx: &CamelContext, xml: &str) -> Result<Vec<RouteDefinition>> {
    load_routes_with(&YamlLoader::new(), ctx, xml)
}

/// Load routes from XML DSL text and add them to the context.
pub fn add_routes_from_xml(ctx: &CamelContext, xml: &str) -> Result<()> {
    for r in load_routes_from_xml(ctx, xml)? {
        ctx.add_route_definition(r)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_xml() {
        let x = r#"<?xml version="1.0"?>
<!-- c -->
<routes xmlns="http://camel.apache.org/schema/spring">
  <route id="r1">
    <from uri="direct:a?x=1&amp;y=2"/>
    <setBody><simple><![CDATA[a < ${body}]]></simple></setBody>
    <log message='&lt;hi&gt; &#65;&#x42;'/>
  </route>
</routes>"#;
        let root = parse_xml(x).unwrap();
        assert_eq!(root.name, "routes");
        let route = root.elements().next().unwrap();
        assert_eq!(route.attr("id"), Some("r1"));
        let from = route.elements().next().unwrap();
        assert_eq!(from.attr("uri"), Some("direct:a?x=1&y=2"));
        let set_body = route.elements().nth(1).unwrap();
        assert_eq!(set_body.elements().next().unwrap().text(), "a < ${body}");
        assert_eq!(route.elements().nth(2).unwrap().attr("message"), Some("<hi> AB"));
    }

    #[test]
    fn xml_errors_have_positions() {
        let e = parse_xml("<a>\n  <b></c>\n</a>").unwrap_err();
        assert!(e.message.contains("mismatched end tag"), "{}", e);
        assert!(e.message.contains("line 2"), "{}", e);
        let e = parse_xml("<a x=1/>").unwrap_err();
        assert!(e.message.contains("quoted value"), "{}", e);
        let e = parse_xml("<a>").unwrap_err();
        assert!(e.message.contains("missing end tag"), "{}", e);
        let e = parse_xml("<a>&bogus;</a>").unwrap_err();
        assert!(e.message.contains("unknown entity"), "{}", e);
    }

    #[test]
    fn conversion_shape() {
        let x = r#"<routes>
  <route id="r1" autoStartup="false">
    <description>demo</description>
    <from uri="direct:a"/>
    <setHeader name="h"><constant>v</constant></setHeader>
    <choice>
      <when><simple>${body}</simple><to uri="mock:a"/></when>
      <when><header>x</header><to uri="mock:b"/></when>
      <otherwise><to uri="mock:c"/></otherwise>
    </choice>
    <setHeaders>
      <setHeader name="a"><constant>1</constant></setHeader>
      <setHeader name="b"><simple resultType="int">2</simple></setHeader>
    </setHeaders>
  </route>
</routes>"#;
        let steps = StepRegistry::core();
        let v = xml_to_value(&parse_xml(x).unwrap(), &steps).unwrap();
        let yaml = crate::yaml::to_yaml(&v);
        let expected = crate::yaml::parse(
            r#"
- route:
    id: r1
    autoStartup: "false"
    description: demo
    from:
      uri: direct:a
      steps:
        - setHeader: {name: h, constant: v}
        - choice:
            when:
              - simple: ${body}
                steps: [{to: {uri: mock:a}}]
              - header: x
                steps: [{to: {uri: mock:b}}]
            otherwise:
              steps: [{to: {uri: mock:c}}]
        - setHeaders:
            headers:
              - {name: a, constant: "1"}
              - name: b
                simple: {expression: "2", resultType: int}
"#,
        )
        .unwrap();
        assert_eq!(v, expected, "{}", yaml);
    }
}

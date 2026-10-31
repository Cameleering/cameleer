//! `xj:` component (Camel `camel-xj`): XML ⇄ JSON, optionally through an XSLT stylesheet.
//!
//! `xj:identity?transformDirection=XML2JSON`, `xj:file:x.xsl?transformDirection=JSON2XML`
//!
//! **JSON → XML** uses Camel's xj representation:
//! ```xml
//! <?xml version="1.0" encoding="UTF-8"?><object xmlns:xj="http://camel.apache.org/component/xj" xj:type="object">
//!   <object xj:name="name" xj:type="string">camel</object>
//!   <object xj:name="tags" xj:type="array"><object xj:type="string">a</object></object>
//! </object>
//! ```
//! with `xj:type` one of `object`, `array`, `string`, `int`, `float`, `boolean`, `null`.
//! With a stylesheet, the xj document is the input of the transformation and the
//! transformation result is the output.
//!
//! **XML → JSON**: with a stylesheet, the transformation result is converted. Elements
//! carrying `xj:name` / `xj:type` are converted as annotated. Other elements: the root
//! element's value is the result; an element with only text becomes a string; an element
//! with child elements or attributes becomes an object whose keys are the child names
//! (repeated names become arrays), attributes become `@name` keys and mixed text a `#text`
//! key; an element whose children all share one name (two or more) becomes an array.
//! Key order follows the document.
//!
//! Options: `transformDirection` (required), `output` (`string`, `bytes`),
//! `contentCache` (default true), `failOnNullBody` (default true); the XSLT options of the
//! `xslt` component are accepted and ignored.

use std::any::Any;
use std::sync::{Arc, Mutex};

use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::{Component, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::value::{json_escape, Value};

use crate::dom::{Document, NodeId, NodeType, QName};
use crate::serialize::{serialize, OutputOptions};
use crate::xslt::Stylesheet;

/// The xj namespace.
pub const XJ_NS: &str = "http://camel.apache.org/component/xj";

/// JSON value with ordered object keys.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Object(Vec<(String, Json)>),
    Array(Vec<Json>),
    String(String),
    Number(String),
    Bool(bool),
    Null,
}

impl Json {
    /// Serialize as compact JSON.
    pub fn to_json(&self) -> String {
        let mut s = String::new();
        self.write(&mut s);
        s
    }

    fn write(&self, out: &mut String) {
        match self {
            Json::Object(m) => {
                out.push('{');
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    json_escape(k, out);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
            Json::Array(a) => {
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Json::String(s) => json_escape(s, out),
            Json::Number(n) => out.push_str(n),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Null => out.push_str("null"),
        }
    }
}

fn xj_attr<'a>(doc: &'a Document, el: NodeId, local: &str) -> Option<&'a str> {
    doc.attribute_ns(el, XJ_NS, local)
}

fn text_of(doc: &Document, el: NodeId) -> String {
    doc.children(el).iter().filter(|&&c| doc.node_type(c) == NodeType::Text).map(|&c| doc.value(c)).collect()
}

fn typed(doc: &Document, el: NodeId, ty: &str) -> Result<Json> {
    Ok(match ty {
        "object" => {
            let mut m: Vec<(String, Json)> = Vec::new();
            for c in doc.element_children(el) {
                let name = xj_attr(doc, c, "name").map(String::from).unwrap_or_else(|| doc.local_name(c).to_string());
                m.push((name, element_to_json(doc, c)?));
            }
            Json::Object(m)
        }
        "array" => Json::Array(doc.element_children(el).into_iter().map(|c| element_to_json(doc, c)).collect::<Result<Vec<_>>>()?),
        "string" => Json::String(text_of(doc, el)),
        "int" | "float" => {
            let t = text_of(doc, el).trim().to_string();
            if t.parse::<f64>().is_err() {
                return Err(CamelError::new("XJException", format!("Invalid {} value '{}' in element <{}>", ty, t, doc.qualified_name(el))));
            }
            Json::Number(t)
        }
        "boolean" => Json::Bool(text_of(doc, el).trim() == "true"),
        "null" => Json::Null,
        other => return Err(CamelError::new("XJException", format!("Unknown xj:type '{}'", other))),
    })
}

/// Convert an element to JSON (see the module documentation for the rules).
pub fn element_to_json(doc: &Document, el: NodeId) -> Result<Json> {
    if let Some(t) = xj_attr(doc, el, "type") {
        return typed(doc, el, t);
    }
    let kids = doc.element_children(el);
    let attrs: Vec<NodeId> = doc.attributes(el).iter().copied().filter(|&a| doc.namespace_uri(a) != XJ_NS).collect();
    let text = text_of(doc, el);
    if kids.is_empty() && attrs.is_empty() {
        return Ok(Json::String(text));
    }
    if attrs.is_empty() && kids.len() >= 2 && text.trim().is_empty() {
        let first = doc.name(kids[0]).clone();
        if kids.iter().all(|&k| doc.name(k).same_expanded(&first)) && kids.iter().all(|&k| xj_attr(doc, k, "name").is_none()) {
            return Ok(Json::Array(kids.into_iter().map(|k| element_to_json(doc, k)).collect::<Result<Vec<_>>>()?));
        }
    }
    let mut m: Vec<(String, Json)> = Vec::new();
    for a in attrs {
        m.push((format!("@{}", doc.local_name(a)), Json::String(doc.value(a).to_string())));
    }
    let mut arrayed: Vec<String> = Vec::new();
    for k in kids {
        let name = xj_attr(doc, k, "name").map(String::from).unwrap_or_else(|| doc.local_name(k).to_string());
        let v = element_to_json(doc, k)?;
        if let Some(pos) = m.iter().position(|(n, _)| *n == name) {
            if arrayed.contains(&name) {
                if let Json::Array(a) = &mut m[pos].1 {
                    a.push(v);
                }
            } else {
                let old = std::mem::replace(&mut m[pos].1, Json::Null);
                m[pos].1 = Json::Array(vec![old, v]);
                arrayed.push(name);
            }
        } else {
            m.push((name, v));
        }
    }
    if !text.trim().is_empty() {
        m.push(("#text".into(), Json::String(text.trim().to_string())));
    }
    Ok(Json::Object(m))
}

/// Convert an XML document to JSON text.
pub fn xml_to_json(doc: &Document) -> Result<String> {
    let root = doc.document_element().ok_or_else(|| CamelError::new("XJException", "No root element"))?;
    Ok(element_to_json(doc, root)?.to_json())
}

fn value_to_json(v: &Value) -> Json {
    match v {
        Value::Null => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Int(i) => Json::Number(i.to_string()),
        Value::Float(f) => Json::Number(crate::xpath::format_number(*f)),
        Value::String(s) => Json::String(s.clone()),
        Value::Bytes(b) => Json::String(String::from_utf8_lossy(b).into_owned()),
        Value::Array(a) => Json::Array(a.iter().map(value_to_json).collect()),
        Value::Object(m) => Json::Object(m.iter().map(|(k, v)| (k.clone(), value_to_json(v))).collect()),
    }
}

fn json_to_dom(doc: &mut Document, parent: NodeId, name: Option<&str>, j: &Json) {
    let el = doc.append_element(parent, QName::local("object"));
    if let Some(n) = name {
        doc.set_attribute(el, QName::ns("xj", "name", XJ_NS), n);
    }
    let ty = match j {
        Json::Object(_) => "object",
        Json::Array(_) => "array",
        Json::String(_) => "string",
        Json::Number(n) => {
            if n.contains(['.', 'e', 'E']) {
                "float"
            } else {
                "int"
            }
        }
        Json::Bool(_) => "boolean",
        Json::Null => "null",
    };
    doc.set_attribute(el, QName::ns("xj", "type", XJ_NS), ty);
    match j {
        Json::Object(m) => {
            for (k, v) in m {
                json_to_dom(doc, el, Some(k), v);
            }
        }
        Json::Array(a) => {
            for v in a {
                json_to_dom(doc, el, None, v);
            }
        }
        Json::String(s) => doc.append_text(el, s),
        Json::Number(n) => doc.append_text(el, n),
        Json::Bool(b) => doc.append_text(el, if *b { "true" } else { "false" }),
        Json::Null => {}
    }
}

/// Parse JSON text keeping object key order.
pub fn parse_json(text: &str) -> Result<Json> {
    let c: Vec<char> = text.chars().collect();
    let mut i = 0;
    let v = parse_value(&c, &mut i)?;
    skip_ws(&c, &mut i);
    if i != c.len() {
        return Err(json_err("unexpected trailing characters", i));
    }
    Ok(v)
}

fn json_err(m: &str, at: usize) -> CamelError {
    CamelError::new("JsonParseException", format!("Invalid JSON: {} at offset {}", m, at))
}

fn skip_ws(c: &[char], i: &mut usize) {
    while *i < c.len() && c[*i].is_whitespace() {
        *i += 1;
    }
}

fn parse_string(c: &[char], i: &mut usize) -> Result<String> {
    *i += 1;
    let mut s = String::new();
    while *i < c.len() {
        match c[*i] {
            '"' => {
                *i += 1;
                return Ok(s);
            }
            '\\' => {
                *i += 1;
                let e = *c.get(*i).ok_or_else(|| json_err("bad escape", *i))?;
                match e {
                    'n' => s.push('\n'),
                    't' => s.push('\t'),
                    'r' => s.push('\r'),
                    'b' => s.push('\u{8}'),
                    'f' => s.push('\u{c}'),
                    'u' => {
                        let hex: String = c.get(*i + 1..*i + 5).ok_or_else(|| json_err("bad unicode escape", *i))?.iter().collect();
                        let mut code = u32::from_str_radix(&hex, 16).map_err(|_| json_err("bad unicode escape", *i))?;
                        *i += 4;
                        if (0xD800..0xDC00).contains(&code) && c.get(*i + 1) == Some(&'\\') && c.get(*i + 2) == Some(&'u') {
                            let hex2: String = c.get(*i + 3..*i + 7).unwrap_or(&[]).iter().collect();
                            if let Ok(lo) = u32::from_str_radix(&hex2, 16) {
                                code = 0x10000 + ((code - 0xD800) << 10) + (lo - 0xDC00);
                                *i += 6;
                            }
                        }
                        s.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                    }
                    other => s.push(other),
                }
                *i += 1;
            }
            ch => {
                s.push(ch);
                *i += 1;
            }
        }
    }
    Err(json_err("unterminated string", *i))
}

fn parse_value(c: &[char], i: &mut usize) -> Result<Json> {
    skip_ws(c, i);
    match c.get(*i) {
        None => Err(json_err("unexpected end", *i)),
        Some('{') => {
            *i += 1;
            let mut m = Vec::new();
            skip_ws(c, i);
            if c.get(*i) == Some(&'}') {
                *i += 1;
                return Ok(Json::Object(m));
            }
            loop {
                skip_ws(c, i);
                if c.get(*i) != Some(&'"') {
                    return Err(json_err("expected a key", *i));
                }
                let k = parse_string(c, i)?;
                skip_ws(c, i);
                if c.get(*i) != Some(&':') {
                    return Err(json_err("expected ':'", *i));
                }
                *i += 1;
                let v = parse_value(c, i)?;
                m.push((k, v));
                skip_ws(c, i);
                match c.get(*i) {
                    Some(',') => *i += 1,
                    Some('}') => {
                        *i += 1;
                        return Ok(Json::Object(m));
                    }
                    _ => return Err(json_err("expected ',' or '}'", *i)),
                }
            }
        }
        Some('[') => {
            *i += 1;
            let mut a = Vec::new();
            skip_ws(c, i);
            if c.get(*i) == Some(&']') {
                *i += 1;
                return Ok(Json::Array(a));
            }
            loop {
                a.push(parse_value(c, i)?);
                skip_ws(c, i);
                match c.get(*i) {
                    Some(',') => *i += 1,
                    Some(']') => {
                        *i += 1;
                        return Ok(Json::Array(a));
                    }
                    _ => return Err(json_err("expected ',' or ']'", *i)),
                }
            }
        }
        Some('"') => Ok(Json::String(parse_string(c, i)?)),
        Some('t') if c[*i..].starts_with(&['t', 'r', 'u', 'e']) => {
            *i += 4;
            Ok(Json::Bool(true))
        }
        Some('f') if c[*i..].starts_with(&['f', 'a', 'l', 's', 'e']) => {
            *i += 5;
            Ok(Json::Bool(false))
        }
        Some('n') if c[*i..].starts_with(&['n', 'u', 'l', 'l']) => {
            *i += 4;
            Ok(Json::Null)
        }
        Some(ch) if *ch == '-' || ch.is_ascii_digit() => {
            let st = *i;
            *i += 1;
            while *i < c.len() && (c[*i].is_ascii_digit() || matches!(c[*i], '.' | 'e' | 'E' | '+' | '-')) {
                *i += 1;
            }
            let s: String = c[st..*i].iter().collect();
            if s.parse::<f64>().is_err() {
                return Err(json_err("invalid number", st));
            }
            Ok(Json::Number(s))
        }
        Some(_) => Err(json_err("unexpected character", *i)),
    }
}

/// Convert JSON to an xj document.
pub fn json_to_document(j: &Json) -> Document {
    let mut doc = Document::new();
    json_to_dom(&mut doc, 0, None, j);
    if let Some(root) = doc.document_element() {
        doc.declare_namespace(root, "xj", XJ_NS);
    }
    doc
}

/// Convert JSON text to xj XML text (with an XML declaration).
pub fn json_to_xml(text: &str) -> Result<String> {
    let doc = json_to_document(&parse_json(text)?);
    Ok(serialize(&doc, 0, &OutputOptions::default()))
}

/// The `xj` component.
#[derive(Default)]
pub struct XjComponent;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Direction {
    Xml2Json,
    Json2Xml,
}

impl Component for XjComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let dir = match params.take("transformDirection").map(|s| s.to_ascii_uppercase()) {
            Some(d) if d == "XML2JSON" => Direction::Xml2Json,
            Some(d) if d == "JSON2XML" => Direction::Json2Xml,
            Some(d) => return Err(CamelError::illegal_argument(format!("Invalid transformDirection: {} (XML2JSON or JSON2XML)", d))),
            None => return Err(CamelError::illegal_argument("xj requires the transformDirection option (XML2JSON or JSON2XML)")),
        };
        let bytes = match params.take("output").map(|s| s.to_ascii_lowercase()) {
            None => false,
            Some(o) if o == "string" || o == "dom" => false,
            Some(o) if o == "bytes" => true,
            Some(o) => return Err(CamelError::illegal_argument(format!("Unsupported xj output: {}", o))),
        };
        let content_cache = params.take_bool("contentCache", true)?;
        let fail_on_null_body = params.take_bool("failOnNullBody", true)?;
        for k in [
            "transformerFactory",
            "transformerFactoryClass",
            "transformerFactoryConfigurationStrategy",
            "uriResolver",
            "uriResolverFactory",
            "resultHandlerFactory",
            "entityResolver",
            "errorListener",
            "xsltMessageLogger",
            "transformerCacheSize",
            "allowStAX",
            "lazyStartProducer",
            "deleteOutputFile",
            "allowTemplateFromHeader",
            "saxonExtensionFunctions",
            "saxonConfiguration",
            "saxonReaderProperties",
            "secureProcessing",
            "jsonFactory",
        ] {
            params.take(k);
        }
        let resource = remaining.trim().to_string();
        let stylesheet = if resource.is_empty() || resource == "identity" { None } else { Some(resource) };
        Ok(Arc::new(XjEndpoint { uri: uri.to_string(), dir, bytes, content_cache, fail_on_null_body, stylesheet, cache: Arc::new(Mutex::new(None)), ctx: ctx.downgrade() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// An xj endpoint (producer only).
pub struct XjEndpoint {
    uri: String,
    dir: Direction,
    bytes: bool,
    content_cache: bool,
    fail_on_null_body: bool,
    stylesheet: Option<String>,
    cache: Arc<Mutex<Option<Arc<Stylesheet>>>>,
    ctx: WeakContext,
}

impl Endpoint for XjEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(XjProducer {
            dir: self.dir,
            bytes: self.bytes,
            content_cache: self.content_cache,
            fail_on_null_body: self.fail_on_null_body,
            stylesheet: self.stylesheet.clone(),
            cache: self.cache.clone(),
            ctx: self.ctx.clone(),
        }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct XjProducer {
    dir: Direction,
    bytes: bool,
    content_cache: bool,
    fail_on_null_body: bool,
    stylesheet: Option<String>,
    cache: Arc<Mutex<Option<Arc<Stylesheet>>>>,
    ctx: WeakContext,
}

impl Service for XjProducer {}

impl XjProducer {
    fn stylesheet(&self, ex: &Exchange) -> Result<Option<Arc<Stylesheet>>> {
        let Some(res) = &self.stylesheet else { return Ok(None) };
        let ctx = ex.context.clone().or_else(|| self.ctx.upgrade());
        if self.content_cache {
            let mut g = self.cache.lock().unwrap();
            if let Some(s) = g.as_ref() {
                return Ok(Some(s.clone()));
            }
            let s = Arc::new(crate::xslt_component::load_stylesheet(ctx.as_ref(), res)?);
            *g = Some(s.clone());
            return Ok(Some(s));
        }
        Ok(Some(Arc::new(crate::xslt_component::load_stylesheet(ctx.as_ref(), res)?)))
    }
}

impl Processor for XjProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        if ex.message.body.is_null() {
            if self.fail_on_null_body {
                return Err(CamelError::illegal_argument("Input body is null"));
            }
            return Ok(());
        }
        let ss = self.stylesheet(ex)?;
        let params = crate::xslt_component::exchange_parameters(ex);
        let out = match self.dir {
            Direction::Xml2Json => {
                let doc = crate::xslt_component::body_document(&ex.message.body)?;
                match ss {
                    Some(ss) => {
                        let r = ss.transform(doc, &params)?;
                        xml_to_json(&r.tree)?
                    }
                    None => xml_to_json(&doc)?,
                }
            }
            Direction::Json2Xml => {
                let json = match &ex.message.body {
                    Value::String(s) => parse_json(s)?,
                    Value::Bytes(b) => parse_json(&String::from_utf8_lossy(b))?,
                    other => value_to_json(other),
                };
                let doc = json_to_document(&json);
                match ss {
                    Some(ss) => ss.transform(doc, &params)?.output,
                    None => serialize(&doc, 0, &OutputOptions::default()),
                }
            }
        };
        ex.message.body = if self.bytes { Value::Bytes(out.into_bytes()) } else { Value::String(out) };
        Ok(())
    }
}

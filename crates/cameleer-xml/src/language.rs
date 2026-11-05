//! The Camel `xpath` language backed by the full XPath 1.0 engine ([`crate::xpath`]).
//!
//! Registered by [`crate::register`] under the name `xpath`, replacing cameleer-core's subset.
//!
//! Options (Camel `XPathExpression`):
//! * `resultType`: `String` (string value, trimmed unless `trim=false`), `nodeset` /
//!   `NodeList` (array with one entry per node: elements serialized as XML, other nodes as
//!   their string value), `node` / `Node` (first node), `number` / `Double`, `Integer` /
//!   `Long`, `boolean` / `Boolean`; Java class names (`java.lang.String`,
//!   `org.w3c.dom.NodeList`) and `XPathConstants` names are accepted. Without a result
//!   type a node-set of one node becomes a string (elements as XML), a larger node-set an
//!   array, and strings / numbers / booleans keep their type.
//! * `namespace` / `namespaces`: prefix bindings, as `p=uri,q=uri2` (also `;` or newline
//!   separated), a JSON object `{"p":"uri"}` or a JSON list `[{"key":"p","value":"uri"}]`
//!   (YAML DSL form); options named `namespace.<prefix>` also bind a prefix.
//! * `headerName`, `propertyName`, `variableName`, `source` (`header:x`,
//!   `exchangeProperty:x`, `variable:x`, `body`): the input to evaluate.
//! * `saxon`: `false` (default) is accepted; `true` fails (no Saxon / XPath 2.0).
//! * `trim` (default true), `threadSafety`, `preCompile`, `logNamespaces`,
//!   `documentType`, `factoryRef`, `objectModel` are accepted.
//!
//! Camel functions and variables (default prefixes, which a `namespace` option may rebind):
//! `in:body()`, `in:header('name')`, `out:body()`, `out:header('name')`,
//! `function:properties('key')`, `function:simple('${...}')`; variables `$name` (message
//! header, then exchange property, then exchange variable), `$env:NAME`,
//! `$system:NAME` (environment, as there are no Java system properties), `$exchangeProperty:NAME`.

use std::collections::BTreeMap;
use std::sync::Arc;

use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::spi::{Expression, Language, Predicate};
use cameleer_core::value::Value;

use crate::dom::{Document, NodeType};
use crate::xpath::{Ctx, Docs, Env, XNode, XPath, XPathError, XValue};

/// Camel `in:` namespace.
pub const NS_IN: &str = "http://camel.apache.org/xml/in/";
/// Camel `out:` namespace.
pub const NS_OUT: &str = "http://camel.apache.org/xml/out/";
/// Camel `function:` namespace.
pub const NS_FUNCTION: &str = "http://camel.apache.org/xml/function/";
/// Camel `env:` variables namespace.
pub const NS_ENV: &str = "http://camel.apache.org/xml/variables/environment-variables";
/// Camel `system:` variables namespace.
pub const NS_SYSTEM: &str = "http://camel.apache.org/xml/variables/system-properties";
/// Camel exchange property variables namespace.
pub const NS_EXCHANGE_PROPERTY: &str = "http://camel.apache.org/xml/variables/exchange-property";

/// Parse a namespace option value into `(prefix, uri)` bindings.
pub fn parse_namespaces(v: &str) -> Vec<(String, String)> {
    let t = v.trim();
    if t.is_empty() {
        return Vec::new();
    }
    if t.starts_with('{') || t.starts_with('[') {
        if let Ok(j) = cameleer_core::json::parse(t) {
            let mut out = Vec::new();
            match j {
                Value::Object(m) => {
                    for (k, v) in m {
                        out.push((k, v.to_string()));
                    }
                }
                Value::Array(items) => {
                    for it in items {
                        match (it.get("key").or_else(|| it.get("prefix")), it.get("value").or_else(|| it.get("uri"))) {
                            (Some(k), Some(v)) => out.push((k.to_string(), v.to_string())),
                            _ => {
                                if let Value::Object(m) = it {
                                    for (k, v) in m {
                                        out.push((k, v.to_string()));
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
            return out;
        }
    }
    t.split([',', ';', '\n'])
        .filter_map(|p| p.split_once('='))
        .map(|(a, b)| (a.trim().trim_start_matches("xmlns:").to_string(), b.trim().to_string()))
        .filter(|(a, _)| !a.is_empty())
        .collect()
}

/// Namespace bindings from language options, plus the Camel default prefixes.
pub fn namespaces_from_options(options: &BTreeMap<String, String>) -> Vec<(String, String)> {
    let mut ns: Vec<(String, String)> = Vec::new();
    for k in ["namespace", "namespaces"] {
        if let Some(v) = options.get(k) {
            ns.extend(parse_namespaces(v));
        }
    }
    for (k, v) in options {
        if let Some(p) = k.strip_prefix("namespace.").or_else(|| k.strip_prefix("ns.")) {
            ns.push((p.to_string(), v.clone()));
        }
    }
    for (p, u) in [("in", NS_IN), ("out", NS_OUT), ("function", NS_FUNCTION), ("env", NS_ENV), ("system", NS_SYSTEM), ("exchangeProperty", NS_EXCHANGE_PROPERTY)] {
        if !ns.iter().any(|(q, _)| q == p) {
            ns.push((p.to_string(), u.to_string()));
        }
    }
    ns
}

/// Evaluation environment exposing the exchange to XPath.
pub struct ExchangeEnv<'a> {
    pub exchange: Option<&'a Exchange>,
}

fn camel_err(e: CamelError) -> XPathError {
    XPathError::eval(format!("{}: {}", e.kind, e.message))
}

impl Env for ExchangeEnv<'_> {
    fn variable(&self, uri: &str, local: &str) -> std::result::Result<XValue, XPathError> {
        let undefined = || XPathError::eval(format!("Variable ${} is not defined", local));
        match uri {
            NS_ENV | NS_SYSTEM => Ok(XValue::Str(std::env::var(local).unwrap_or_default())),
            NS_EXCHANGE_PROPERTY => {
                let ex = self.exchange.ok_or_else(undefined)?;
                Ok(XValue::Str(ex.properties.get(local).map(|v| v.to_string()).unwrap_or_default()))
            }
            "" | NS_IN => {
                let ex = self.exchange.ok_or_else(undefined)?;
                if let Some(v) = ex.message.headers.get(local) {
                    return Ok(value_to_xvalue(v));
                }
                if uri.is_empty() {
                    if let Some(v) = ex.properties.get(local) {
                        return Ok(value_to_xvalue(v));
                    }
                    if let Some(v) = ex.properties.get(&format!("CamelVariable.{}", local)) {
                        return Ok(value_to_xvalue(v));
                    }
                }
                Err(undefined())
            }
            _ => Err(undefined()),
        }
    }

    fn function(&self, uri: &str, local: &str, args: &[XValue], ctx: &Ctx) -> Option<std::result::Result<XValue, XPathError>> {
        let ex = self.exchange?;
        let arg = |i: usize| args.get(i).map(|a| a.to_string_value(ctx.docs)).unwrap_or_default();
        match (uri, local) {
            (NS_IN | NS_OUT, "body") => Some(Ok(XValue::Str(ex.message.body.to_string()))),
            (NS_IN | NS_OUT, "header") => Some(Ok(XValue::Str(ex.message.headers.get(&arg(0)).map(|v| v.to_string()).unwrap_or_default()))),
            (NS_FUNCTION, "properties") => {
                let key = arg(0);
                Some(match ex.context() {
                    Ok(c) => c.resolve_placeholders(&format!("{{{{{}}}}}", key)).map(XValue::Str).map_err(camel_err),
                    Err(e) => Err(camel_err(e)),
                })
            }
            (NS_FUNCTION, "simple") => {
                let text = arg(0);
                let r = (|| -> Result<Value> {
                    let ctx = ex.context()?;
                    let e = ctx.resolve_language("simple")?.create_expression(&text, &BTreeMap::new())?;
                    e.evaluate(ex)
                })();
                Some(r.map(|v| XValue::Str(v.to_string())).map_err(camel_err))
            }
            _ => None,
        }
    }
}

fn value_to_xvalue(v: &Value) -> XValue {
    match v {
        Value::Bool(b) => XValue::Bool(*b),
        Value::Int(i) => XValue::Num(*i as f64),
        Value::Float(f) => XValue::Num(*f),
        other => XValue::Str(other.to_string()),
    }
}

/// Parse a message value as an XML document.
pub fn value_to_document(v: &Value) -> Result<Document> {
    match v {
        Value::Null => Err(CamelError::new("InvalidPayloadException", "No body available to evaluate xpath")),
        Value::Bytes(b) => Ok(Document::parse_bytes(b)?),
        other => Ok(Document::parse(&other.to_string())?),
    }
}

/// Convert an XPath result to a Camel value for a result type (see the module docs).
pub fn convert_result(docs: &Docs, v: XValue, result_type: Option<&str>, trim: bool) -> Result<Value> {
    let rt = result_type.map(|t| {
        let t = t.rsplit('.').next().unwrap_or(t).to_ascii_lowercase();
        t
    });
    let node_str = |n: &XNode| docs.node_to_string(*n);
    Ok(match rt.as_deref() {
        Some("string") | Some("str") => {
            let s = v.to_string_value(docs);
            Value::String(if trim { s.trim().to_string() } else { s })
        }
        Some("boolean") | Some("bool") => Value::Bool(v.to_bool()),
        Some("number") | Some("double") | Some("float") | Some("bigdecimal") => Value::Float(v.to_number(docs)),
        Some("integer") | Some("int") | Some("long") | Some("short") => {
            let n = v.to_number(docs);
            if n.is_nan() {
                Value::Null
            } else {
                Value::Int(n as i64)
            }
        }
        Some("nodeset") | Some("nodelist") | Some("list") => match &v {
            XValue::Nodes(ns) => Value::Array(ns.iter().map(|n| Value::String(node_str(n))).collect()),
            other => Value::Array(vec![Value::String(other.to_string_value(docs))]),
        },
        Some("node") | Some("element") | Some("document") => match &v {
            XValue::Nodes(ns) => ns.first().map(|n| Value::String(node_str(n))).unwrap_or(Value::Null),
            other => Value::String(other.to_string_value(docs)),
        },
        Some(other) => return Err(CamelError::illegal_argument(format!("Unsupported xpath resultType: {}", other))),
        None => match v {
            XValue::Nodes(ns) => {
                if ns.len() == 1 {
                    Value::String(node_str(&ns[0]))
                } else {
                    Value::Array(ns.iter().map(|n| Value::String(node_str(n))).collect())
                }
            }
            XValue::Str(s) => Value::String(s),
            XValue::Num(n) => Value::Float(n),
            XValue::Bool(b) => Value::Bool(b),
        },
    })
}

/// Evaluate an XPath against XML text (default namespaces only).
pub fn query(xml: &str, expr: &str, result_type: Option<&str>) -> Result<Value> {
    let doc = Document::parse(xml)?;
    let xp = XPath::compile_ns(expr, &namespaces_from_options(&BTreeMap::new()))?;
    let docs = Docs::new(doc);
    let v = xp.eval_docs(&docs, &ExchangeEnv { exchange: None })?;
    convert_result(&docs, v, result_type, true)
}

fn source_value(ex: &Exchange, options: &BTreeMap<String, String>) -> Value {
    let opt = |k: &str| options.get(k).filter(|s| !s.is_empty());
    if let Some(h) = opt("headerName") {
        return ex.message.headers.get(h).cloned().unwrap_or(Value::Null);
    }
    if let Some(p) = opt("propertyName") {
        return ex.properties.get(p).cloned().unwrap_or(Value::Null);
    }
    if let Some(v) = opt("variableName") {
        return ex.properties.get(&format!("CamelVariable.{}", v)).cloned().unwrap_or(Value::Null);
    }
    if let Some(src) = opt("source") {
        if let Some(h) = src.strip_prefix("header:") {
            return ex.message.headers.get(h).cloned().unwrap_or(Value::Null);
        }
        if let Some(p) = src.strip_prefix("property:").or_else(|| src.strip_prefix("exchangeProperty:")) {
            return ex.properties.get(p).cloned().unwrap_or(Value::Null);
        }
        if let Some(v) = src.strip_prefix("variable:") {
            return ex.properties.get(&format!("CamelVariable.{}", v)).cloned().unwrap_or(Value::Null);
        }
        if src == "body" {
            return ex.message.body.clone();
        }
        return ex.message.headers.get(src).cloned().unwrap_or(Value::Null);
    }
    ex.message.body.clone()
}

/// A compiled `xpath` language expression.
pub struct XPathLanguageExpression {
    xpath: XPath,
    options: BTreeMap<String, String>,
    trim: bool,
}

impl XPathLanguageExpression {
    /// Compile an expression with Camel language options.
    pub fn new(text: &str, options: &BTreeMap<String, String>) -> Result<Self> {
        if let Some(s) = options.get("saxon") {
            if s.eq_ignore_ascii_case("true") {
                return Err(CamelError::illegal_argument("saxon=true is not supported: Cameleer implements XPath 1.0 only (use saxon=false)"));
            }
        }
        let ns = namespaces_from_options(options);
        let xpath = XPath::compile_ns(text.trim(), &ns)?;
        let trim = options.get("trim").map(|t| !t.eq_ignore_ascii_case("false")).unwrap_or(true);
        Ok(XPathLanguageExpression { xpath, options: options.clone(), trim })
    }

    fn raw(&self, ex: &Exchange) -> Result<(Docs, XValue)> {
        let doc = value_to_document(&source_value(ex, &self.options))?;
        let docs = Docs::new(doc);
        let v = self.xpath.eval_docs(&docs, &ExchangeEnv { exchange: Some(ex) })?;
        Ok((docs, v))
    }
}

impl Expression for XPathLanguageExpression {
    fn evaluate(&self, ex: &Exchange) -> Result<Value> {
        let (docs, v) = self.raw(ex)?;
        convert_result(&docs, v, self.options.get("resultType").map(|s| s.as_str()).filter(|s| !s.is_empty()), self.trim)
    }
}

impl Predicate for XPathLanguageExpression {
    fn matches(&self, ex: &Exchange) -> Result<bool> {
        Ok(self.raw(ex)?.1.to_bool())
    }
}

/// The `xpath` language.
#[derive(Default)]
pub struct XPathLanguage;

impl Language for XPathLanguage {
    fn name(&self) -> &str {
        "xpath"
    }
    fn create_expression(&self, text: &str, options: &BTreeMap<String, String>) -> Result<Arc<dyn Expression>> {
        Ok(Arc::new(XPathLanguageExpression::new(text, options)?))
    }
    fn create_predicate(&self, text: &str, options: &BTreeMap<String, String>) -> Result<Arc<dyn Predicate>> {
        Ok(Arc::new(XPathLanguageExpression::new(text, options)?))
    }
}

/// Rust DSL: XPath expression with namespace bindings, e.g.
/// `xpath_ns("/c:order/@id", &[("c", "urn:orders")])`.
pub fn xpath_ns(expr: impl Into<String>, namespaces: &[(&str, &str)]) -> cameleer_core::builder::ValueBuilder {
    let ns: Vec<String> = namespaces.iter().map(|(p, u)| format!("{}={}", p, u)).collect();
    cameleer_core::builder::ValueBuilder::new(cameleer_core::builder::ExpressionDefinition::language("xpath", expr.into()).option("namespace", ns.join(",")))
}

/// Whether a node is an element (helper for callers working with [`Docs`]).
pub fn is_element(docs: &Docs, n: XNode) -> bool {
    docs.node_type(n) == Some(NodeType::Element)
}

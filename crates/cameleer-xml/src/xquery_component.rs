//! The Camel `xquery` language and `xquery:` component (Camel `camel-saxon`) backed by the
//! XQuery processor in [`crate::xquery`].
//!
//! **Language** `xquery` (`.set_body(lang("xquery", "..."))`, `filter`, YAML `xquery:`):
//! the message body (or `headerName` / `source`) is parsed as the context document.
//! Options: `resultType` — `DOM` (default: the result serialized as XML text, adjacent
//! atomic values separated by spaces), `String` (string values concatenated, as Camel's
//! `evaluateAsString`), `List` / `NodeList` (one entry per item: elements as XML, other
//! nodes as their string value, atomic values typed), `Node` / `Element` (the first item),
//! `Bytes`, `Integer` / `Long`, `Double`, `Boolean`, and Java class names of these;
//! `namespace` / `namespaces` prefix bindings (same syntax as the `xpath` language);
//! `stripsAllWhiteSpace` (default true: whitespace-only text nodes of the input are
//! dropped, as Saxon does for Camel). Predicates use the effective boolean value.
//!
//! **External variables** (as camel-saxon binds them): `declare variable $in.body
//! external;` (the body), `$in.headers.NAME` (a header), `$NAME` (an exchange property,
//! then a header, then an exchange variable). Declared atomic types cast string values
//! (`declare variable $in.headers.qty as xs:integer external;`); declared node types
//! (`as element()`, `as document-node()`, `as node()`) parse the value as XML.
//!
//! **Component** `xquery:file:query.xq?resultsFormat=DOM|String|Bytes|List` (also
//! `classpath:`, `ref:bean`, plain paths; `BytesSource`/`StringSource`/`DOMSource` map to
//! `Bytes`/`String`/`DOM`): producer replacing the body with the query result. Options:
//! `resultsFormat` (default `DOM`), `stripsAllWhiteSpace` (default true), `contentCache`
//! (default true), `headerName` / `source` (input), `namespacePrefixes` (map of prefixes,
//! e.g. `p=urn:p`), and accepted-but-ignored Saxon settings: `configuration`,
//! `configurationProperties`, `staticQueryContext`, `moduleURIResolver`, `parameters`,
//! `properties`, `allowStAX`, `resultType`, `lazyStartProducer`. Relative `doc()` uris
//! resolve against the query file's directory.

use std::any::Any;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use cameleer_core::component::template_support::{load_resource, resource_path};
use cameleer_core::context::{CamelContext, WeakContext};
use cameleer_core::error::{kinds, CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::spi::{Component, Endpoint, Expression, Language, Predicate, Processor, Producer, Service};
use cameleer_core::util::Parameters;
use cameleer_core::value::Value;

use crate::language::{namespaces_from_options, parse_namespaces};
use crate::xpath::Docs;
use crate::xquery::{items_to_bool, items_to_list, items_to_string, serialize_items, Item, XQuery, XqError};

/// `XQueryException` kind (Saxon `XPathException`): XQuery compilation or evaluation failure.
pub const XQUERY_EXCEPTION: &str = "XPathException";

pub(crate) fn register_kinds() {
    cameleer_core::error::register_kind_parent(XQUERY_EXCEPTION, kinds::EXCEPTION);
}

/// Convert an engine error to a Camel error (`ExpressionIllegalSyntaxException` for
/// static errors, `XPathException` otherwise).
pub fn xq_error(e: XqError) -> CamelError {
    if e.syntax {
        CamelError::new(kinds::EXPRESSION_ILLEGAL_SYNTAX, format!("Invalid xquery: {}", e))
    } else {
        register_kinds();
        CamelError::new(XQUERY_EXCEPTION, e.to_string())
    }
}

/// Result formats (Camel `ResultFormat` plus language result types).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ResultFormat {
    Dom,
    String,
    Bytes,
    List,
    Node,
    Integer,
    Double,
    Boolean,
}

impl ResultFormat {
    /// Parse a `resultsFormat` / `resultType` value (Java class names accepted).
    pub fn parse(s: &str) -> Result<ResultFormat> {
        let t = s.rsplit('.').next().unwrap_or(s).to_ascii_lowercase();
        Ok(match t.as_str() {
            "dom" | "domsource" | "document" | "xml" => ResultFormat::Dom,
            "string" | "stringsource" | "str" => ResultFormat::String,
            "bytes" | "bytessource" | "byte[]" => ResultFormat::Bytes,
            "list" | "nodelist" | "arraylist" | "sequence" => ResultFormat::List,
            "node" | "element" => ResultFormat::Node,
            "integer" | "int" | "long" => ResultFormat::Integer,
            "double" | "number" | "float" | "bigdecimal" => ResultFormat::Double,
            "boolean" | "bool" => ResultFormat::Boolean,
            other => return Err(CamelError::illegal_argument(format!("Unsupported xquery result format: {} (DOM, String, Bytes, List, Node, Integer, Double or Boolean)", other))),
        })
    }

    /// Convert a result sequence.
    pub fn convert(self, docs: &Docs, items: &[Item]) -> Value {
        match self {
            ResultFormat::Dom => Value::String(serialize_items(docs, items)),
            ResultFormat::String => Value::String(items_to_string(docs, items)),
            ResultFormat::Bytes => Value::Bytes(serialize_items(docs, items).into_bytes()),
            ResultFormat::List => items_to_list(docs, items),
            ResultFormat::Node => items.first().map(|i| crate::xquery::item_to_value(docs, i)).unwrap_or(Value::Null),
            ResultFormat::Integer => {
                let s = items_to_string(docs, items);
                let t = s.trim();
                t.parse::<i64>().map(Value::Int).or_else(|_| t.parse::<f64>().map(|f| Value::Int(f as i64))).unwrap_or(Value::Null)
            }
            ResultFormat::Double => items_to_string(docs, items).trim().parse::<f64>().map(Value::Float).unwrap_or(Value::Float(f64::NAN)),
            ResultFormat::Boolean => Value::Bool(items_to_bool(items)),
        }
    }
}

/// Values of external variables for an exchange, camel-saxon style (see the module docs).
pub fn exchange_variables(q: &XQuery, ex: &Exchange) -> HashMap<String, Value> {
    let mut out = HashMap::new();
    for v in q.external_variables() {
        if !v.name.uri.is_empty() {
            continue;
        }
        let n = v.name.local.as_str();
        let val = if n == "in.body" || n == "body" {
            Some(ex.message.body.clone())
        } else if let Some(h) = n.strip_prefix("in.headers.").or_else(|| n.strip_prefix("in.header.")).or_else(|| n.strip_prefix("headers.")).or_else(|| n.strip_prefix("out.headers.")) {
            ex.message.headers.get(h).cloned()
        } else if n == "exchange" || n == "camelContext" {
            None
        } else {
            ex.properties.get(n).cloned().or_else(|| ex.message.headers.get(n).cloned()).or_else(|| ex.properties.get(&format!("CamelVariable.{}", n)).cloned())
        };
        if let Some(val) = val {
            out.insert(n.to_string(), val);
        }
    }
    out
}

fn input_value(ex: &Exchange, header: Option<&str>, source: Option<&str>) -> Value {
    if let Some(h) = header.filter(|s| !s.is_empty()) {
        return ex.message.headers.get(h).cloned().unwrap_or(Value::Null);
    }
    if let Some(src) = source.filter(|s| !s.is_empty()) {
        if let Some(h) = src.strip_prefix("header:") {
            return ex.message.headers.get(h).cloned().unwrap_or(Value::Null);
        }
        if let Some(p) = src.strip_prefix("property:").or_else(|| src.strip_prefix("exchangeProperty:")) {
            return ex.properties.get(p).cloned().unwrap_or(Value::Null);
        }
        if let Some(v) = src.strip_prefix("variable:") {
            return ex.properties.get(&format!("CamelVariable.{}", v)).cloned().unwrap_or(Value::Null);
        }
        if src != "body" {
            return ex.message.headers.get(src).cloned().unwrap_or(Value::Null);
        }
    }
    ex.message.body.clone()
}

/// Evaluate a compiled query against an exchange.
pub fn evaluate_on_exchange<T: Send>(q: &XQuery, ex: &Exchange, input: &Value, convert: impl FnOnce(&Docs, &[Item]) -> T + Send) -> Result<T> {
    let vars = exchange_variables(q, ex);
    let resolve = |uri: &str, local: &str| if uri.is_empty() { vars.get(local).cloned() } else { None };
    q.evaluate(Some(input), &resolve, convert).map_err(xq_error)
}

/// A compiled `xquery` language expression.
pub struct XQueryExpression {
    query: Arc<XQuery>,
    format: ResultFormat,
    header: Option<String>,
    source: Option<String>,
}

impl XQueryExpression {
    /// Compile an expression with Camel language options.
    pub fn new(text: &str, options: &BTreeMap<String, String>) -> Result<XQueryExpression> {
        let ns = namespaces_from_options(options);
        let strip = options.get("stripsAllWhiteSpace").map(|v| !v.eq_ignore_ascii_case("false")).unwrap_or(true);
        let query = XQuery::compile(text.trim(), &ns).map_err(xq_error)?.with_strip_whitespace(strip);
        let format = match options.get("resultType").filter(|s| !s.is_empty()) {
            Some(t) => ResultFormat::parse(t)?,
            None => ResultFormat::Dom,
        };
        Ok(XQueryExpression {
            query: Arc::new(query),
            format,
            header: options.get("headerName").cloned(),
            source: options.get("source").cloned(),
        })
    }
}

impl Expression for XQueryExpression {
    fn evaluate(&self, ex: &Exchange) -> Result<Value> {
        let input = input_value(ex, self.header.as_deref(), self.source.as_deref());
        let fmt = self.format;
        evaluate_on_exchange(&self.query, ex, &input, move |d, i| fmt.convert(d, i))
    }
}

impl Predicate for XQueryExpression {
    fn matches(&self, ex: &Exchange) -> Result<bool> {
        let input = input_value(ex, self.header.as_deref(), self.source.as_deref());
        evaluate_on_exchange(&self.query, ex, &input, |_, i| items_to_bool(i))
    }
}

/// The `xquery` language.
#[derive(Default)]
pub struct XQueryLanguage;

impl Language for XQueryLanguage {
    fn name(&self) -> &str {
        "xquery"
    }
    fn create_expression(&self, text: &str, options: &BTreeMap<String, String>) -> Result<Arc<dyn Expression>> {
        Ok(Arc::new(XQueryExpression::new(text, options)?))
    }
    fn create_predicate(&self, text: &str, options: &BTreeMap<String, String>) -> Result<Arc<dyn Predicate>> {
        Ok(Arc::new(XQueryExpression::new(text, options)?))
    }
}

/// Rust DSL helper: an `xquery` expression (`b.from(..).set_body(cameleer_xml::xquery_expr("..."))`).
pub fn xquery_expr(text: impl Into<String>) -> cameleer_core::builder::ValueBuilder {
    cameleer_core::builder::ValueBuilder::new(cameleer_core::builder::ExpressionDefinition::language("xquery", text.into()))
}

/// The `xquery` component.
#[derive(Default)]
pub struct XQueryComponent;

#[derive(Clone, Debug)]
struct Config {
    resource_uri: String,
    format: ResultFormat,
    strip: bool,
    content_cache: bool,
    header: Option<String>,
    source: Option<String>,
    namespaces: Vec<(String, String)>,
}

impl Component for XQueryComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        crate::register_kinds();
        if remaining.trim().is_empty() {
            return Err(CamelError::illegal_argument("xquery requires a query resource uri (e.g. xquery:file:query.xq)"));
        }
        let format = match params.take("resultsFormat") {
            Some(f) => ResultFormat::parse(&f)?,
            None => ResultFormat::Dom,
        };
        let mut namespaces = Vec::new();
        for k in ["namespacePrefixes", "namespaces", "namespace"] {
            if let Some(v) = params.take(k) {
                namespaces.extend(parse_namespaces(&v));
            }
        }
        let cfg = Config {
            resource_uri: remaining.to_string(),
            format,
            strip: params.take_bool("stripsAllWhiteSpace", true)?,
            content_cache: params.take_bool("contentCache", true)?,
            header: params.take("headerName"),
            source: params.take("source"),
            namespaces,
        };
        for k in ["configuration", "configurationProperties", "staticQueryContext", "moduleURIResolver", "parameters", "properties", "allowStAX", "resultType", "lazyStartProducer"] {
            params.take(k);
        }
        Ok(Arc::new(XQueryEndpoint { uri: uri.to_string(), cfg, cache: Arc::new(Mutex::new(None)), ctx: ctx.downgrade() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// An `xquery` endpoint (producer).
pub struct XQueryEndpoint {
    uri: String,
    cfg: Config,
    cache: Arc<Mutex<Option<Arc<XQuery>>>>,
    ctx: WeakContext,
}

impl Endpoint for XQueryEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(XQueryProducer { cfg: self.cfg.clone(), cache: self.cache.clone(), ctx: self.ctx.clone() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct XQueryProducer {
    cfg: Config,
    cache: Arc<Mutex<Option<Arc<XQuery>>>>,
    ctx: WeakContext,
}

impl Service for XQueryProducer {}

/// Compile a query from a resource uri (`file:`, `classpath:`, `ref:`, plain path).
pub fn load_query(ctx: Option<&CamelContext>, resource_uri: &str, namespaces: &[(String, String)]) -> Result<XQuery> {
    let r = resource_uri.trim();
    if r.starts_with("ref:") || r.starts_with("bean:") {
        let text = load_resource(ctx, r)?;
        return XQuery::compile(&text, namespaces).map_err(xq_error);
    }
    let path = resource_path(r);
    if !path.exists() {
        return Err(CamelError::new(kinds::FILE_NOT_FOUND, format!("Cannot find xquery resource {}", resource_uri)));
    }
    XQuery::compile_file(&path, namespaces).map_err(xq_error)
}

impl XQueryProducer {
    fn query(&self, ex: &Exchange) -> Result<Arc<XQuery>> {
        let ctx = ex.context.clone().or_else(|| self.ctx.upgrade());
        let load = || -> Result<Arc<XQuery>> { Ok(Arc::new(load_query(ctx.as_ref(), &self.cfg.resource_uri, &self.cfg.namespaces)?.with_strip_whitespace(self.cfg.strip))) };
        if !self.cfg.content_cache {
            return load();
        }
        let mut g = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(q) = g.as_ref() {
            return Ok(q.clone());
        }
        let q = load()?;
        *g = Some(q.clone());
        Ok(q)
    }
}

impl Processor for XQueryProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let q = self.query(ex)?;
        let input = input_value(ex, self.cfg.header.as_deref(), self.cfg.source.as_deref());
        let fmt = self.cfg.format;
        let out = evaluate_on_exchange(&q, ex, &input, move |d, i| fmt.convert(d, i))?;
        ex.message.body = out;
        Ok(())
    }
}

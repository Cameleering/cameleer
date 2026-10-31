//! Basic EIPs: to, toD, setBody, transform, setHeader, setProperty, removeHeader(s),
//! removeProperty(ies), process, bean, log, convertBodyTo, stop, setExchangePattern, step.

use std::sync::Arc;

use crate::builder::ExpressionDefinition;
use crate::error::{CamelError, Result};
use crate::impl_definition_boilerplate;
use crate::log::Level;
use crate::message::{props, Exchange, ExchangePattern};
use crate::model::{OutputList, ProcessorDefinition, RouteContext};
use crate::spi::{Expression, Processor, Producer};
use crate::value::Value;

// ---------------------------------------------------------------------------
// to
// ---------------------------------------------------------------------------

/// Sends the exchange to a static endpoint.
pub struct ToDefinition {
    pub id: Option<String>,
    pub uri: String,
    pub pattern: Option<ExchangePattern>,
}

impl ToDefinition {
    pub fn new(uri: impl Into<String>) -> Self {
        ToDefinition { id: None, uri: uri.into(), pattern: None }
    }
}

pub struct SendProcessor {
    pub uri: String,
    producer: Arc<dyn Producer>,
    pattern: Option<ExchangePattern>,
}

impl SendProcessor {
    pub fn new(uri: String, producer: Arc<dyn Producer>, pattern: Option<ExchangePattern>) -> Self {
        SendProcessor { uri, producer, pattern }
    }
}

impl Processor for SendProcessor {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        let saved = exchange.pattern;
        if let Some(p) = self.pattern {
            exchange.pattern = p;
        }
        exchange.set_property(props::TO_ENDPOINT, self.uri.clone());
        let r = self.producer.process(exchange);
        if self.pattern.is_some() {
            exchange.pattern = saved;
        }
        r
    }
}

impl ProcessorDefinition for ToDefinition {
    fn name(&self) -> &str {
        "to"
    }
    fn label(&self) -> String {
        format!("to[{}]", self.uri)
    }
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        let endpoint = rc.context.get_endpoint(&self.uri)?;
        let producer = endpoint.create_producer()?;
        rc.add_service(producer.clone());
        Ok(Arc::new(SendProcessor::new(endpoint.uri().to_string(), producer, self.pattern)))
    }
    impl_definition_boilerplate!();
}

// ---------------------------------------------------------------------------
// toD
// ---------------------------------------------------------------------------

/// Sends to a dynamic endpoint computed with an expression (simple by default).
pub struct ToDynamicDefinition {
    pub id: Option<String>,
    pub uri: ExpressionDefinition,
    pub pattern: Option<ExchangePattern>,
    pub ignore_invalid_endpoint: bool,
}

struct ToDynamicProcessor {
    uri: Arc<dyn Expression>,
    pattern: Option<ExchangePattern>,
    ignore_invalid: bool,
}

impl Processor for ToDynamicProcessor {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        let uri = self.uri.evaluate(exchange)?.to_string();
        let uri = uri.trim().to_string();
        let ctx = exchange.context()?.clone();
        let producer = match ctx.get_producer(&uri) {
            Ok(p) => p,
            Err(e) if self.ignore_invalid => {
                crate::camel_debug!("toD", "Ignoring invalid endpoint {}: {}", uri, e);
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let saved = exchange.pattern;
        if let Some(p) = self.pattern {
            exchange.pattern = p;
        }
        exchange.set_property(props::TO_ENDPOINT, uri);
        let r = producer.process(exchange);
        exchange.pattern = saved;
        r
    }
}

impl ProcessorDefinition for ToDynamicDefinition {
    fn name(&self) -> &str {
        "toD"
    }
    fn label(&self) -> String {
        format!("toD[{:?}]", self.uri)
    }
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        Ok(Arc::new(ToDynamicProcessor {
            uri: self.uri.resolve_expression(&rc.context)?,
            pattern: self.pattern,
            ignore_invalid: self.ignore_invalid_endpoint,
        }))
    }
    impl_definition_boilerplate!();
}

// ---------------------------------------------------------------------------
// setBody / transform / setHeader / setProperty
// ---------------------------------------------------------------------------

/// What an expression-based setter writes to.
#[derive(Clone, Debug, PartialEq)]
pub enum SetTarget {
    Body,
    Header(String),
    Property(String),
    /// Variable (Camel 4) - stored as exchange property `CamelVariable.<name>`.
    Variable(String),
}

pub struct SetDefinition {
    pub id: Option<String>,
    pub name: &'static str,
    pub target: SetTarget,
    pub expression: ExpressionDefinition,
}

impl SetDefinition {
    pub fn body(e: ExpressionDefinition) -> Self {
        SetDefinition { id: None, name: "setBody", target: SetTarget::Body, expression: e }
    }
    pub fn transform(e: ExpressionDefinition) -> Self {
        SetDefinition { id: None, name: "transform", target: SetTarget::Body, expression: e }
    }
    pub fn header(n: impl Into<String>, e: ExpressionDefinition) -> Self {
        SetDefinition { id: None, name: "setHeader", target: SetTarget::Header(n.into()), expression: e }
    }
    pub fn property(n: impl Into<String>, e: ExpressionDefinition) -> Self {
        SetDefinition { id: None, name: "setProperty", target: SetTarget::Property(n.into()), expression: e }
    }
    pub fn variable(n: impl Into<String>, e: ExpressionDefinition) -> Self {
        SetDefinition { id: None, name: "setVariable", target: SetTarget::Variable(n.into()), expression: e }
    }
}

pub struct SetProcessor {
    target: SetTarget,
    expression: Arc<dyn Expression>,
}

impl SetProcessor {
    pub fn new(target: SetTarget, expression: Arc<dyn Expression>) -> Self {
        SetProcessor { target, expression }
    }
}

pub fn variable_key(name: &str) -> String {
    format!("CamelVariable.{}", name)
}

impl Processor for SetProcessor {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        let v = self.expression.evaluate(exchange)?;
        match &self.target {
            SetTarget::Body => exchange.message.body = v,
            SetTarget::Header(n) => exchange.message.headers.set(n.clone(), v),
            SetTarget::Property(n) => {
                exchange.properties.insert(n.clone(), v);
            }
            SetTarget::Variable(n) => {
                exchange.properties.insert(variable_key(n), v);
            }
        }
        Ok(())
    }
}

impl ProcessorDefinition for SetDefinition {
    fn name(&self) -> &str {
        self.name
    }
    fn label(&self) -> String {
        match &self.target {
            SetTarget::Body => format!("{}[{:?}]", self.name, self.expression),
            SetTarget::Header(n) | SetTarget::Property(n) | SetTarget::Variable(n) => {
                format!("{}[{}, {:?}]", self.name, n, self.expression)
            }
        }
    }
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        Ok(Arc::new(SetProcessor::new(self.target.clone(), self.expression.resolve_expression(&rc.context)?)))
    }
    impl_definition_boilerplate!();
}

// ---------------------------------------------------------------------------
// removeHeader(s) / removeProperty(ies)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum RemoveTarget {
    Header(String),
    Headers { pattern: String, excludes: Vec<String> },
    Property(String),
    Properties { pattern: String, excludes: Vec<String> },
    Variable(String),
}

pub struct RemoveDefinition {
    pub id: Option<String>,
    pub target: RemoveTarget,
}

impl ProcessorDefinition for RemoveDefinition {
    fn name(&self) -> &str {
        match self.target {
            RemoveTarget::Header(_) => "removeHeader",
            RemoveTarget::Headers { .. } => "removeHeaders",
            RemoveTarget::Property(_) => "removeProperty",
            RemoveTarget::Properties { .. } => "removeProperties",
            RemoveTarget::Variable(_) => "removeVariable",
        }
    }
    fn create_processor(&self, _rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        let target = self.target.clone();
        Ok(Arc::new(move |ex: &mut Exchange| {
            match &target {
                RemoveTarget::Header(n) => {
                    ex.message.headers.remove(n);
                }
                RemoveTarget::Headers { pattern, excludes } => {
                    let ex_refs: Vec<&str> = excludes.iter().map(|s| s.as_str()).collect();
                    ex.message.headers.remove_matching(pattern, &ex_refs);
                }
                RemoveTarget::Property(n) => {
                    ex.properties.remove(n);
                }
                RemoveTarget::Properties { pattern, excludes } => {
                    let keys: Vec<String> = ex
                        .properties
                        .keys()
                        .filter(|k| {
                            crate::util::matches_pattern(k, pattern)
                                && !excludes.iter().any(|e| crate::util::matches_pattern(k, e))
                        })
                        .cloned()
                        .collect();
                    for k in keys {
                        ex.properties.remove(&k);
                    }
                }
                RemoveTarget::Variable(n) => {
                    ex.properties.remove(&variable_key(n));
                }
            }
            Ok(())
        }))
    }
    impl_definition_boilerplate!();
}

// ---------------------------------------------------------------------------
// process / bean
// ---------------------------------------------------------------------------

/// Invoke a custom processor (inline or from the registry by name).
pub struct ProcessDefinition {
    pub id: Option<String>,
    pub processor: Option<Arc<dyn Processor>>,
    pub reference: Option<String>,
}

impl ProcessorDefinition for ProcessDefinition {
    fn name(&self) -> &str {
        "process"
    }
    fn label(&self) -> String {
        match &self.reference {
            Some(r) => format!("process[ref:{}]", r),
            None => "process[Processor]".to_string(),
        }
    }
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        if let Some(p) = &self.processor {
            return Ok(p.clone());
        }
        let name = self.reference.as_deref().ok_or_else(|| CamelError::illegal_argument("process requires a processor or ref"))?;
        rc.context.registry().lookup_processor(name).ok_or_else(|| CamelError::no_such_bean(name))
    }
    impl_definition_boilerplate!();
}

// ---------------------------------------------------------------------------
// log EIP
// ---------------------------------------------------------------------------

pub struct LogDefinition {
    pub id: Option<String>,
    pub message: ExpressionDefinition,
    pub level: Level,
    pub log_name: Option<String>,
    pub marker: Option<String>,
}

impl ProcessorDefinition for LogDefinition {
    fn name(&self) -> &str {
        "log"
    }
    fn label(&self) -> String {
        format!("log[{:?}]", self.message)
    }
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        let msg = self.message.resolve_expression(&rc.context)?;
        let level = self.level;
        let name = self.log_name.clone().unwrap_or_else(|| rc.route_id.clone());
        Ok(Arc::new(move |ex: &mut Exchange| {
            if crate::log::enabled(level) {
                let m = msg.evaluate(ex)?.to_string();
                crate::log::log(level, &name, m);
            }
            Ok(())
        }))
    }
    impl_definition_boilerplate!();
}

// ---------------------------------------------------------------------------
// convertBodyTo / convertHeaderTo
// ---------------------------------------------------------------------------

/// Convert a value to a named target type.
/// Supported: String, string, int/Integer/Long/i64, double/Double/float/f64, boolean/Boolean,
/// bytes/byte[], json (parse string to structured value), Map/List (parse json).
pub fn convert_to_type(v: &Value, type_name: &str) -> Result<Value> {
    let t = type_name.rsplit('.').next().unwrap_or(type_name);
    Ok(match t {
        "String" | "string" | "str" => Value::String(v.to_string()),
        "Integer" | "int" | "Long" | "long" | "i64" | "i32" | "Short" | "short" | "integer" => {
            Value::Int(v.convert::<i64>()?)
        }
        "Double" | "double" | "Float" | "float" | "f64" | "f32" | "BigDecimal" | "Number" => Value::Float(v.convert::<f64>()?),
        "Boolean" | "boolean" | "bool" => Value::Bool(v.convert::<bool>()?),
        "byte[]" | "bytes" | "Bytes" | "InputStream" | "Vec<u8>" => Value::Bytes(v.to_bytes()),
        "Map" | "HashMap" | "LinkedHashMap" | "List" | "ArrayList" | "json" | "Json" | "JsonNode" | "Object" | "object"
        | "array" => match v {
            Value::Object(_) | Value::Array(_) => v.clone(),
            other if t == "Object" => other.clone(),
            other => crate::json::parse(&other.to_string())?,
        },
        other => {
            return Err(CamelError::new(
                crate::error::kinds::NO_TYPE_CONVERSION_AVAILABLE,
                format!("No type converter available to convert from type: {} to the required type: {}", v.type_name(), other),
            ))
        }
    })
}

pub struct ConvertDefinition {
    pub id: Option<String>,
    pub type_name: String,
    /// None = body, Some(name) = header.
    pub header: Option<String>,
    pub mandatory: bool,
}

impl ProcessorDefinition for ConvertDefinition {
    fn name(&self) -> &str {
        if self.header.is_some() {
            "convertHeaderTo"
        } else {
            "convertBodyTo"
        }
    }
    fn label(&self) -> String {
        format!("{}[{}]", self.name(), self.type_name)
    }
    fn create_processor(&self, _rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        let t = self.type_name.clone();
        let header = self.header.clone();
        let mandatory = self.mandatory;
        Ok(Arc::new(move |ex: &mut Exchange| {
            match &header {
                None => {
                    if ex.message.body.is_null() && !mandatory {
                        return Ok(());
                    }
                    ex.message.body = convert_to_type(&ex.message.body, &t)?;
                }
                Some(h) => {
                    if let Some(v) = ex.message.headers.get(h).cloned() {
                        ex.message.headers.set(h.clone(), convert_to_type(&v, &t)?);
                    } else if mandatory {
                        return Err(CamelError::new(crate::error::kinds::NO_SUCH_HEADER, format!("No '{}' header available", h)));
                    }
                }
            }
            Ok(())
        }))
    }
    impl_definition_boilerplate!();
}

// ---------------------------------------------------------------------------
// stop / setExchangePattern
// ---------------------------------------------------------------------------

pub struct StopDefinition {
    pub id: Option<String>,
}

impl ProcessorDefinition for StopDefinition {
    fn name(&self) -> &str {
        "stop"
    }
    fn create_processor(&self, _rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        Ok(Arc::new(|ex: &mut Exchange| {
            ex.set_route_stop(true);
            Ok(())
        }))
    }
    impl_definition_boilerplate!();
}

pub struct SetExchangePatternDefinition {
    pub id: Option<String>,
    pub pattern: ExchangePattern,
}

impl ProcessorDefinition for SetExchangePatternDefinition {
    fn name(&self) -> &str {
        "setExchangePattern"
    }
    fn create_processor(&self, _rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        let p = self.pattern;
        Ok(Arc::new(move |ex: &mut Exchange| {
            ex.pattern = p;
            Ok(())
        }))
    }
    impl_definition_boilerplate!();
}

// ---------------------------------------------------------------------------
// step (grouping block)
// ---------------------------------------------------------------------------

pub struct StepDefinition {
    pub id: Option<String>,
    pub outputs: OutputList,
}

impl ProcessorDefinition for StepDefinition {
    fn name(&self) -> &str {
        "step"
    }
    fn is_block(&self) -> bool {
        true
    }
    fn add_output(&mut self, def: Box<dyn ProcessorDefinition>) -> Result<()> {
        self.outputs.push(def);
        Ok(())
    }
    fn outputs(&self) -> Vec<&dyn ProcessorDefinition> {
        self.outputs.refs()
    }
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        let child = rc.create_child_processor(&self.outputs.outputs)?;
        let id = self.id.clone().unwrap_or_else(|| rc.next_node_id("step"));
        Ok(Arc::new(move |ex: &mut Exchange| {
            let prev = ex.properties.insert(props::STEP_ID.to_string(), Value::String(id.clone()));
            let r = child.process(ex);
            match prev {
                Some(p) => {
                    ex.properties.insert(props::STEP_ID.to_string(), p);
                }
                None => {
                    ex.properties.remove(props::STEP_ID);
                }
            }
            r
        }))
    }
    impl_definition_boilerplate!();
}

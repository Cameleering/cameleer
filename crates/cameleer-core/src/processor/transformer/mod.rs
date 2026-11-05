//! Transformer / Validator (Camel data types).
//!
//! * [`DataType`]: a urn `model:name` (`json:Order`, `xml:Order`, `java:com.acme.Order`)
//!   or a plain name (`text-plain`, `application-json`).
//! * Transformers are registered on the context with
//!   [`CamelContextTransformerExt::add_transformer`] (from/to pair, `from` may be `None`
//!   = any), [`add_scheme_transformer`](CamelContextTransformerExt::add_scheme_transformer)
//!   (applies to any transformation from or to a model, like Camel's `scheme("csv")`) and
//!   [`add_named_transformer`](CamelContextTransformerExt::add_named_transformer) (Camel 4
//!   `DataTypeTransformer`s, looked up by the target type name). Built-in named
//!   transformers: `text-plain`, `application-json`, `application-octet-stream`,
//!   `application-x-struct`.
//! * Built-in transformer kinds: [`data_format_transformer`] (unmarshal when transforming
//!   to `java`, marshal when transforming from `java`), [`endpoint_transformer`] (send to
//!   an endpoint), [`processor_transformer`] (any processor).
//! * Validators: [`predicate_validator`], [`endpoint_validator`], [`processor_validator`],
//!   registered per data type with [`add_validator`](CamelContextTransformerExt::add_validator).
//!   A failed validation raises `ValidationException`.
//! * Route contracts: `.input_type(urn, validate)` / `.output_type(urn, validate)` (YAML
//!   `inputType` / `outputType`, `{urn, validate}`). The message data type is tracked in
//!   the exchange property [`MESSAGE_DATA_TYPE`]: when a route with an input type receives
//!   a message of another declared type (for example sent with `to("direct:x")` from a
//!   route whose message is `json:Order`), the matching transformer is applied
//!   automatically; the output type is applied when the route completes successfully.
//! * `transformDataType(fromType, toType)` EIP: [`TransformDataTypeDefinition`].

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock, RwLock};

use crate::builder::ExpressionDefinition;
use crate::context::{CamelContext, RouteFactoryHook};
use crate::dataformat::{DataFormat, DataFormatDefinition};
use crate::error::{kinds, CamelError, Result};
use crate::impl_definition_boilerplate;
use crate::message::Exchange;
use crate::model::{ProcessorDefinition, RouteContext, RouteDefinition};
use crate::spi::{Predicate, Processor};
use crate::value::Value;

/// Exchange property holding the urn of the message's current data type.
pub const MESSAGE_DATA_TYPE: &str = "CamelMessageDataType";
/// Route property (and YAML route key) declaring the input type.
pub const INPUT_TYPE: &str = "inputType";
/// Route property (and YAML route key) declaring the output type.
pub const OUTPUT_TYPE: &str = "outputType";

// ---------------------------------------------------------------------------
// DataType
// ---------------------------------------------------------------------------

/// A data type urn: `model:name` or just `model`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DataType {
    pub model: String,
    pub name: Option<String>,
}

impl DataType {
    pub fn parse(urn: &str) -> DataType {
        let urn = urn.trim();
        match urn.split_once(':') {
            Some((m, n)) if !n.is_empty() => DataType { model: m.to_string(), name: Some(n.to_string()) },
            Some((m, _)) => DataType { model: m.to_string(), name: None },
            None => DataType { model: urn.to_string(), name: None },
        }
    }
    /// The urn (`json:Order`).
    pub fn urn(&self) -> String {
        match &self.name {
            Some(n) => format!("{}:{}", self.model, n),
            None => self.model.clone(),
        }
    }
    /// True for `java:...` types.
    pub fn is_java(&self) -> bool {
        self.model == "java"
    }
    /// The type without its name (`json`).
    pub fn model_only(&self) -> DataType {
        DataType { model: self.model.clone(), name: None }
    }
}

impl std::fmt::Display for DataType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.urn())
    }
}

impl From<&str> for DataType {
    fn from(s: &str) -> Self {
        DataType::parse(s)
    }
}
impl From<String> for DataType {
    fn from(s: String) -> Self {
        DataType::parse(&s)
    }
}
impl From<&String> for DataType {
    fn from(s: &String) -> Self {
        DataType::parse(s)
    }
}

/// The data type currently recorded on the exchange.
pub fn message_data_type(exchange: &Exchange) -> Option<DataType> {
    exchange.property(MESSAGE_DATA_TYPE).filter(|v| !v.is_null()).map(|v| DataType::parse(&v.to_string()))
}

/// Record the message data type.
pub fn set_message_data_type(exchange: &mut Exchange, t: &DataType) {
    exchange.set_property(MESSAGE_DATA_TYPE, t.urn());
}

// ---------------------------------------------------------------------------
// Transformer / Validator SPI
// ---------------------------------------------------------------------------

/// Transforms the message of an exchange from one data type to another.
pub trait Transformer: Send + Sync {
    fn transform(&self, exchange: &mut Exchange, from: Option<&DataType>, to: &DataType) -> Result<()>;
}

impl<F> Transformer for F
where
    F: Fn(&mut Exchange, Option<&DataType>, &DataType) -> Result<()> + Send + Sync,
{
    fn transform(&self, exchange: &mut Exchange, from: Option<&DataType>, to: &DataType) -> Result<()> {
        self(exchange, from, to)
    }
}

/// Validates that the message conforms to a data type.
pub trait Validator: Send + Sync {
    fn validate(&self, exchange: &mut Exchange, data_type: &DataType) -> Result<()>;
}

impl<F> Validator for F
where
    F: Fn(&mut Exchange, &DataType) -> Result<()> + Send + Sync,
{
    fn validate(&self, exchange: &mut Exchange, data_type: &DataType) -> Result<()> {
        self(exchange, data_type)
    }
}

/// A transformer running a processor (the data types are ignored).
pub fn processor_transformer(p: impl Processor + 'static) -> Arc<dyn Transformer> {
    let p: Arc<dyn Processor> = Arc::new(p);
    Arc::new(move |ex: &mut Exchange, _f: Option<&DataType>, _t: &DataType| p.process(ex))
}

/// A data format transformer (Camel `DataFormatTransformer`): unmarshals when
/// transforming to `java` (or any type) from another model, marshals when transforming
/// from `java` (or an unknown type) to another model.
pub fn data_format_transformer(df: impl Into<DataFormatDefinition>) -> Arc<dyn Transformer> {
    Arc::new(DataFormatTransformer { definition: df.into(), resolved: OnceLock::new() })
}

struct DataFormatTransformer {
    definition: DataFormatDefinition,
    resolved: OnceLock<Arc<dyn DataFormat>>,
}

impl Transformer for DataFormatTransformer {
    fn transform(&self, exchange: &mut Exchange, from: Option<&DataType>, to: &DataType) -> Result<()> {
        let df = match self.resolved.get() {
            Some(d) => d.clone(),
            None => {
                let d = self.definition.resolve(exchange.context()?)?;
                let _ = self.resolved.set(d.clone());
                d
            }
        };
        let from_java = from.map(|f| f.is_java()).unwrap_or(true);
        if to.is_java() && !from_java {
            df.unmarshal(exchange)
        } else if from_java && !to.is_java() {
            df.marshal(exchange)
        } else {
            Err(CamelError::illegal_argument(format!(
                "Unsupported transformation: from='{}', to='{}'",
                from.map(|f| f.urn()).unwrap_or_default(),
                to
            )))
        }
    }
}

/// A transformer sending the exchange to an endpoint (Camel `EndpointTransformer`);
/// the reply becomes the message.
pub fn endpoint_transformer(uri: impl Into<String>) -> Arc<dyn Transformer> {
    let uri = uri.into();
    Arc::new(move |ex: &mut Exchange, _f: Option<&DataType>, _t: &DataType| {
        let producer = ex.context()?.get_producer(&uri)?;
        let saved = ex.pattern;
        ex.pattern = crate::message::ExchangePattern::InOut;
        let r = producer.process(ex);
        ex.pattern = saved;
        r?;
        match ex.exception.take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    })
}

fn validation_error(t: &DataType, msg: impl std::fmt::Display) -> CamelError {
    CamelError::new(kinds::VALIDATION, format!("Validation failed for '{}': {}", t, msg))
}

/// A validator checking a predicate (Camel `PredicateValidator`).
pub fn predicate_validator(p: impl Into<ExpressionDefinition>) -> Arc<dyn Validator> {
    Arc::new(PredicateValidator { definition: p.into(), resolved: OnceLock::new() })
}

struct PredicateValidator {
    definition: ExpressionDefinition,
    resolved: OnceLock<Arc<dyn Predicate>>,
}

impl Validator for PredicateValidator {
    fn validate(&self, exchange: &mut Exchange, data_type: &DataType) -> Result<()> {
        let p = match self.resolved.get() {
            Some(p) => p.clone(),
            None => {
                let p = self.definition.resolve_predicate(exchange.context()?)?;
                let _ = self.resolved.set(p.clone());
                p
            }
        };
        match p.matches(exchange) {
            Ok(true) => Ok(()),
            Ok(false) => Err(validation_error(data_type, format!("predicate {:?} is false", self.definition))),
            Err(e) => Err(validation_error(data_type, e.message.clone()).with_cause(e)),
        }
    }
}

/// A validator sending a copy of the exchange to an endpoint (e.g. `json-validator:`);
/// a failure of the endpoint is a validation failure (Camel `EndpointValidator`).
pub fn endpoint_validator(uri: impl Into<String>) -> Arc<dyn Validator> {
    let uri = uri.into();
    Arc::new(move |ex: &mut Exchange, t: &DataType| {
        let producer = ex.context()?.get_producer(&uri)?;
        let mut copy = ex.clone();
        let r = producer.process(&mut copy);
        match r.err().or(copy.exception.take()) {
            Some(e) => Err(validation_error(t, e.message.clone()).with_cause(e)),
            None => Ok(()),
        }
    })
}

/// A validator running a processor; an error is a validation failure.
pub fn processor_validator(p: impl Processor + 'static) -> Arc<dyn Validator> {
    let p: Arc<dyn Processor> = Arc::new(p);
    Arc::new(move |ex: &mut Exchange, t: &DataType| {
        p.process(ex).map_err(|e| if e.kind == kinds::VALIDATION { e } else { validation_error(t, e.message.clone()).with_cause(e) })
    })
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum TransformerKey {
    Pair(Option<String>, String),
    Scheme(String),
    Name(String),
}

/// Transformers and validators of a context.
#[derive(Default)]
pub struct TransformerRegistry {
    transformers: RwLock<BTreeMap<TransformerKey, Arc<dyn Transformer>>>,
    validators: RwLock<BTreeMap<String, Arc<dyn Validator>>>,
}

impl TransformerRegistry {
    fn with_builtins() -> Self {
        let r = TransformerRegistry::default();
        r.add_named("text-plain", Arc::new(text_plain));
        r.add_named("application-json", Arc::new(application_json));
        r.add_named("application-octet-stream", Arc::new(application_octet_stream));
        r.add_named("application-x-struct", Arc::new(application_struct));
        r
    }

    pub fn add_pair(&self, from: Option<&DataType>, to: &DataType, t: Arc<dyn Transformer>) {
        self.transformers.write().unwrap().insert(TransformerKey::Pair(from.map(|f| f.urn()), to.urn()), t);
    }
    pub fn add_scheme(&self, scheme: &str, t: Arc<dyn Transformer>) {
        self.transformers.write().unwrap().insert(TransformerKey::Scheme(scheme.to_string()), t);
    }
    pub fn add_named(&self, name: &str, t: Arc<dyn Transformer>) {
        self.transformers.write().unwrap().insert(TransformerKey::Name(name.to_string()), t);
    }
    pub fn add_validator(&self, t: &DataType, v: Arc<dyn Validator>) {
        self.validators.write().unwrap().insert(t.urn(), v);
    }

    fn get(&self, k: &TransformerKey) -> Option<Arc<dyn Transformer>> {
        self.transformers.read().unwrap().get(k).cloned()
    }

    /// Find a transformer for `from -> to` (exact pair, model-level pairs, scheme,
    /// named transformer for the target, any-source pair). Scheme transformers only
    /// apply when the source type is known.
    pub fn resolve(&self, from: Option<&DataType>, to: &DataType) -> Option<Arc<dyn Transformer>> {
        let pair = |f: Option<String>, t: String| self.get(&TransformerKey::Pair(f, t));
        if let Some(f) = from {
            let candidates = [
                (Some(f.urn()), to.urn()),
                (Some(f.model.clone()), to.urn()),
                (Some(f.urn()), to.model.clone()),
                (Some(f.model.clone()), to.model.clone()),
            ];
            for (a, b) in candidates {
                if let Some(t) = pair(a, b) {
                    return Some(t);
                }
            }
            for scheme in [&f.model, &to.model] {
                if let Some(t) = self.get(&TransformerKey::Scheme(scheme.clone())) {
                    return Some(t);
                }
            }
        }
        if let Some(t) = self.get(&TransformerKey::Name(to.urn())) {
            return Some(t);
        }
        if let Some(n) = &to.name {
            if let Some(t) = self.get(&TransformerKey::Name(n.clone())) {
                return Some(t);
            }
        }
        pair(None, to.urn()).or_else(|| pair(None, to.model.clone()))
    }

    /// A two-step chain `from -> X -> to` over registered pairs.
    fn resolve_chain(&self, from: &DataType, to: &DataType) -> Option<(Arc<dyn Transformer>, DataType, Arc<dyn Transformer>)> {
        let keys: Vec<TransformerKey> = self.transformers.read().unwrap().keys().cloned().collect();
        for k in keys {
            if let TransformerKey::Pair(Some(a), x) = k {
                if a == from.urn() || a == from.model {
                    let mid = DataType::parse(&x);
                    if mid == *to {
                        continue;
                    }
                    if let (Some(t1), Some(t2)) = (self.resolve(Some(from), &mid), self.resolve(Some(&mid), to)) {
                        return Some((t1, mid, t2));
                    }
                }
            }
        }
        None
    }

    /// The validator for a data type (exact urn, then model).
    pub fn resolve_validator(&self, t: &DataType) -> Option<Arc<dyn Validator>> {
        let v = self.validators.read().unwrap();
        v.get(&t.urn()).or_else(|| v.get(&t.model)).cloned()
    }

    /// Transform the exchange to `to`. Returns an `IllegalArgumentException` when
    /// the message has a different declared type and no transformer is found.
    pub fn transform(&self, exchange: &mut Exchange, from: Option<&DataType>, to: &DataType) -> Result<()> {
        if from == Some(to) {
            return Ok(());
        }
        if let Some(t) = self.resolve(from, to) {
            t.transform(exchange, from, to)?;
        } else if let Some((t1, mid, t2)) = from.and_then(|f| self.resolve_chain(f, to)) {
            t1.transform(exchange, from, &mid)?;
            t2.transform(exchange, Some(&mid), to)?;
        } else if from.is_none() || from.map(|f| f.model == to.model && to.is_java()).unwrap_or(false) {
            convert_java(exchange, to)?;
        } else {
            return Err(CamelError::illegal_argument(format!(
                "No Transformer found for [from='{}', to='{}']",
                from.map(|f| f.urn()).unwrap_or_default(),
                to
            )));
        }
        set_message_data_type(exchange, to);
        Ok(())
    }

    /// Validate the exchange against `t` (`ValidationException` when no validator exists).
    pub fn validate(&self, exchange: &mut Exchange, t: &DataType) -> Result<()> {
        match self.resolve_validator(t) {
            Some(v) => v.validate(exchange, t),
            None => Err(CamelError::new(kinds::VALIDATION, format!("No Validator found for '{}'", t))),
        }
    }
}

/// Type conversion for `java:` targets without a transformer.
fn convert_java(exchange: &mut Exchange, to: &DataType) -> Result<()> {
    if !to.is_java() {
        return Ok(());
    }
    let name = to.name.clone().unwrap_or_default();
    let body = exchange.body().clone();
    let simple = name.rsplit('.').next().unwrap_or("").to_string();
    let v = match simple.as_str() {
        "String" => Value::String(body.convert::<String>()?),
        "byte[]" => Value::Bytes(body.convert::<Vec<u8>>()?),
        "Integer" | "Long" | "int" | "long" => Value::Int(body.convert::<i64>()?),
        "Double" | "Float" | "double" | "float" => Value::Float(body.convert::<f64>()?),
        "Boolean" | "boolean" => Value::Bool(body.convert::<bool>()?),
        _ => body,
    };
    exchange.set_body(v);
    Ok(())
}

fn text_plain(ex: &mut Exchange, _f: Option<&DataType>, _t: &DataType) -> Result<()> {
    let s = match ex.body() {
        Value::String(s) => s.clone(),
        Value::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
        Value::Null => String::new(),
        v @ (Value::Object(_) | Value::Array(_)) => v.to_json(),
        v => v.to_string(),
    };
    ex.set_body(s);
    ex.set_header("Content-Type", "text/plain");
    Ok(())
}

fn application_json(ex: &mut Exchange, _f: Option<&DataType>, _t: &DataType) -> Result<()> {
    let s = match ex.body() {
        Value::String(s) => s.clone(),
        Value::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
        v => v.to_json(),
    };
    ex.set_body(s);
    ex.set_header("Content-Type", "application/json");
    Ok(())
}

fn application_octet_stream(ex: &mut Exchange, _f: Option<&DataType>, _t: &DataType) -> Result<()> {
    let b = match ex.body() {
        Value::Bytes(b) => b.clone(),
        Value::String(s) => s.as_bytes().to_vec(),
        v @ (Value::Object(_) | Value::Array(_)) => v.to_json().into_bytes(),
        v => v.to_bytes(),
    };
    ex.set_body(Value::Bytes(b));
    ex.set_header("Content-Type", "application/octet-stream");
    Ok(())
}

fn application_struct(ex: &mut Exchange, _f: Option<&DataType>, _t: &DataType) -> Result<()> {
    let v = match ex.body() {
        Value::String(s) => crate::json::parse(s)?,
        Value::Bytes(b) => crate::json::parse(&String::from_utf8_lossy(b))?,
        v => v.clone(),
    };
    ex.set_body(v);
    Ok(())
}

const REGISTRY_BEAN: &str = "CamelTransformerRegistry";

/// Transformer/validator registration on the [`CamelContext`].
pub trait CamelContextTransformerExt {
    /// The context's registry (created with the built-in transformers on first use).
    fn transformer_registry(&self) -> Arc<TransformerRegistry>;
    /// Register a transformer for `from -> to` (`from = None`: from any type).
    fn add_transformer<'a>(&self, from: impl Into<Option<&'a str>>, to: &str, t: Arc<dyn Transformer>);
    /// Register a transformer for any transformation from or to `scheme` (a model).
    fn add_scheme_transformer(&self, scheme: &str, t: Arc<dyn Transformer>);
    /// Register a named transformer (Camel 4 `DataTypeTransformer`, e.g. `application-json`).
    fn add_named_transformer(&self, name: &str, t: Arc<dyn Transformer>);
    /// Register a validator for a data type (urn or model).
    fn add_validator(&self, data_type: &str, v: Arc<dyn Validator>);
}

impl CamelContextTransformerExt for CamelContext {
    fn transformer_registry(&self) -> Arc<TransformerRegistry> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = LOCK.lock().unwrap();
        if let Some(r) = self.registry().lookup::<TransformerRegistry>(REGISTRY_BEAN) {
            return r;
        }
        let r = Arc::new(TransformerRegistry::with_builtins());
        self.registry().bind_arc(REGISTRY_BEAN, r.clone());
        r
    }
    fn add_transformer<'a>(&self, from: impl Into<Option<&'a str>>, to: &str, t: Arc<dyn Transformer>) {
        let from = from.into().map(DataType::parse);
        self.transformer_registry().add_pair(from.as_ref(), &DataType::parse(to), t);
    }
    fn add_scheme_transformer(&self, scheme: &str, t: Arc<dyn Transformer>) {
        self.transformer_registry().add_scheme(scheme, t);
    }
    fn add_named_transformer(&self, name: &str, t: Arc<dyn Transformer>) {
        self.transformer_registry().add_named(name, t);
    }
    fn add_validator(&self, data_type: &str, v: Arc<dyn Validator>) {
        self.transformer_registry().add_validator(&DataType::parse(data_type), v);
    }
}

// ---------------------------------------------------------------------------
// Route contract (inputType / outputType)
// ---------------------------------------------------------------------------

/// A declared route input or output type.
#[derive(Clone, Debug, PartialEq)]
pub struct TypeDeclaration {
    pub data_type: DataType,
    pub validate: bool,
}

impl TypeDeclaration {
    /// Parse a route property: a urn, or JSON `{"urn": "...", "validate": true}`
    /// (also `scheme`/`name` or `type` keys).
    pub fn parse(s: &str) -> Result<TypeDeclaration> {
        let t = s.trim();
        if t.starts_with('{') {
            let v = crate::json::parse(t)?;
            let urn = v
                .get("urn")
                .or_else(|| v.get("type"))
                .map(|u| u.to_string())
                .or_else(|| v.get("scheme").map(|sc| format!("{}:{}", sc, v.get("name").map(|n| n.to_string()).unwrap_or_default())))
                .ok_or_else(|| CamelError::illegal_argument(format!("A type declaration requires 'urn': {}", s)))?;
            let validate = v.get("validate").map(|b| b.is_predicate_true()).unwrap_or(false);
            Ok(TypeDeclaration { data_type: DataType::parse(&urn), validate })
        } else {
            Ok(TypeDeclaration { data_type: DataType::parse(t), validate: false })
        }
    }

    /// Encode as a route property.
    pub fn encode(&self) -> String {
        Value::object([("urn", Value::from(self.data_type.urn())), ("validate", Value::Bool(self.validate))]).to_json()
    }
}

/// The contract (input/output types) declared on a route definition.
pub fn route_contract(def: &RouteDefinition) -> Result<(Option<TypeDeclaration>, Option<TypeDeclaration>)> {
    let get = |k: &str| def.properties.get(k).map(|s| TypeDeclaration::parse(s)).transpose();
    Ok((get(INPUT_TYPE)?, get(OUTPUT_TYPE)?))
}

struct ContractProcessor {
    input: Option<TypeDeclaration>,
    output: Option<TypeDeclaration>,
    inner: Arc<dyn Processor>,
    registry: Arc<TransformerRegistry>,
}

impl ContractProcessor {
    fn apply(&self, exchange: &mut Exchange, decl: &TypeDeclaration) -> Result<()> {
        let from = message_data_type(exchange);
        self.registry.transform(exchange, from.as_ref(), &decl.data_type)?;
        if decl.validate {
            self.registry.validate(exchange, &decl.data_type)?;
        }
        Ok(())
    }
}

impl Processor for ContractProcessor {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        if let Some(i) = &self.input {
            self.apply(exchange, i)?;
        }
        self.inner.process(exchange)?;
        if let Some(o) = &self.output {
            if exchange.should_continue() {
                if let Err(e) = self.apply(exchange, o) {
                    exchange.set_exception(e);
                }
            }
        }
        Ok(())
    }
}

struct ContractHook;

impl RouteFactoryHook for ContractHook {
    fn wrap_route(&self, def: &RouteDefinition, rc: &RouteContext, processor: Arc<dyn Processor>) -> Result<Arc<dyn Processor>> {
        let (input, output) = route_contract(def)?;
        if input.is_none() && output.is_none() {
            return Ok(processor);
        }
        Ok(Arc::new(ContractProcessor { input, output, inner: processor, registry: rc.context.transformer_registry() }))
    }
}

/// Install the route contract hook (called for every new context).
pub fn register(ctx: &CamelContext) {
    ctx.add_route_factory_hook(Arc::new(ContractHook));
}

// ---------------------------------------------------------------------------
// transformDataType EIP
// ---------------------------------------------------------------------------

/// Model of the Camel 4 `transformDataType(fromType, toType)` EIP: transforms the message
/// with the registered transformer (the from type defaults to the message's data type).
pub struct TransformDataTypeDefinition {
    pub id: Option<String>,
    pub from_type: Option<String>,
    pub to_type: String,
}

impl TransformDataTypeDefinition {
    pub fn new(from_type: Option<String>, to_type: impl Into<String>) -> Self {
        TransformDataTypeDefinition { id: None, from_type, to_type: to_type.into() }
    }
}

impl ProcessorDefinition for TransformDataTypeDefinition {
    fn name(&self) -> &str {
        "transformDataType"
    }
    fn label(&self) -> String {
        format!("transformDataType[{}->{}]", self.from_type.clone().unwrap_or_default(), self.to_type)
    }
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        let registry = rc.context.transformer_registry();
        let from = self.from_type.as_deref().map(DataType::parse);
        let to = DataType::parse(&rc.context.resolve_placeholders(&self.to_type)?);
        Ok(Arc::new(move |ex: &mut Exchange| {
            let from = from.clone().or_else(|| message_data_type(ex));
            match registry.resolve(from.as_ref(), &to) {
                Some(t) => {
                    t.transform(ex, from.as_ref(), &to)?;
                    set_message_data_type(ex, &to);
                    Ok(())
                }
                None => registry.transform(ex, from.as_ref(), &to),
            }
        }))
    }
    impl_definition_boilerplate!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_type_parsing() {
        assert_eq!(DataType::parse("json:Order"), DataType { model: "json".into(), name: Some("Order".into()) });
        assert_eq!(DataType::parse("text-plain").urn(), "text-plain");
        assert_eq!(DataType::parse("java:com.acme.Order").name.as_deref(), Some("com.acme.Order"));
        let d = TypeDeclaration::parse("{\"urn\":\"xml:Order\",\"validate\":true}").unwrap();
        assert!(d.validate);
        assert_eq!(d.data_type.urn(), "xml:Order");
        assert_eq!(TypeDeclaration::parse(&d.encode()).unwrap(), d);
    }
}

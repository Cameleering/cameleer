//! `validator:` component (Camel `camel-validator`) with XML Schema validation
//! ([`crate::xsd`]), replacing cameleer-core's well-formedness-only validator when
//! [`crate::register`] is called.
//!
//! `validator:file:schemas/order.xsd?failOnNullBody=true&headerName=`
//!
//! * The body (or the header named by `headerName`) is parsed and validated against the
//!   schema; failures raise `SchemaValidationException` (a `ValidationException`) whose
//!   message lists every error: `Validation failed for: <schema> errors: [lineNumber: 3;
//!   columnNumber: 5; cvc-complex-type.2.4.a: ...]`. The individual messages are also
//!   stored in the `CamelValidationErrors` exchange property (a list).
//! * Not well-formed input fails with `SchemaValidationException` too (Xerces reports
//!   parse errors through the same error handler).
//! * A null body fails with `NoXmlBodyValidationException` (`failOnNullBody`, default true),
//!   a missing header with `NoXmlHeaderValidationException` (`failOnNullHeader`).
//! * The schema is loaded once (`useSharedSchema`, default true; `contentCache`).
//!   `include`/`import` resolve relative to the schema file. `ref:bean` resources are
//!   supported.
//! * Accepted and ignored: `schemaFactory`, `schemaLanguage`, `resourceResolver`,
//!   `resourceResolverFactory`, `errorHandler`, `useDom`, `schemaReaderFactory`,
//!   `lazyStartProducer`.

use std::any::Any;
use std::sync::{Arc, Mutex};

use cameleer_core::component::template_support::{load_resource, resource_path};
use cameleer_core::context::{CamelContext, WeakContext};
use cameleer_core::error::{kinds, register_kind_parent, CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::spi::{Component, Endpoint, Processor, Producer, Service};
use cameleer_core::util::Parameters;
use cameleer_core::value::Value;

use crate::dom::Document;
use crate::xsd::Schema;

pub const SCHEMA_VALIDATION_EXCEPTION: &str = "SchemaValidationException";
pub const NO_XML_BODY_VALIDATION_EXCEPTION: &str = "NoXmlBodyValidationException";
pub const NO_XML_HEADER_VALIDATION_EXCEPTION: &str = "NoXmlHeaderValidationException";
/// Exchange property holding the validation error messages.
pub const VALIDATION_ERRORS_PROPERTY: &str = "CamelValidationErrors";

pub(crate) fn register_validation_kinds() {
    register_kind_parent(SCHEMA_VALIDATION_EXCEPTION, kinds::VALIDATION);
    register_kind_parent(NO_XML_BODY_VALIDATION_EXCEPTION, kinds::VALIDATION);
    register_kind_parent(NO_XML_HEADER_VALIDATION_EXCEPTION, kinds::VALIDATION);
}

/// The `validator` component.
#[derive(Default)]
pub struct ValidatorComponent;

#[derive(Clone, Debug)]
struct Config {
    resource_uri: String,
    fail_on_null_body: bool,
    fail_on_null_header: bool,
    header_name: Option<String>,
    shared: bool,
}

impl Component for ValidatorComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        crate::register_kinds();
        if remaining.trim().is_empty() {
            return Err(CamelError::illegal_argument("validator requires a schema resource uri"));
        }
        let cfg = Config {
            resource_uri: remaining.to_string(),
            fail_on_null_body: params.take_bool("failOnNullBody", true)?,
            fail_on_null_header: params.take_bool("failOnNullHeader", true)?,
            header_name: params.take("headerName").filter(|s| !s.is_empty()),
            shared: params.take_bool("useSharedSchema", true)? && params.take_bool("contentCache", true)?,
        };
        for k in ["schemaFactory", "schemaLanguage", "resourceResolver", "resourceResolverFactory", "errorHandler", "useDom", "schemaReaderFactory", "lazyStartProducer"] {
            params.take(k);
        }
        Ok(Arc::new(ValidatorEndpoint { uri: uri.to_string(), cfg, schema: Arc::new(Mutex::new(None)), ctx: ctx.downgrade() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A validator endpoint (producer only).
pub struct ValidatorEndpoint {
    uri: String,
    cfg: Config,
    schema: Arc<Mutex<Option<Arc<Schema>>>>,
    ctx: WeakContext,
}

impl Endpoint for ValidatorEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(ValidatorProducer { cfg: self.cfg.clone(), schema: self.schema.clone(), ctx: self.ctx.clone() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct ValidatorProducer {
    cfg: Config,
    schema: Arc<Mutex<Option<Arc<Schema>>>>,
    ctx: WeakContext,
}

impl Service for ValidatorProducer {}

/// Load a schema from a resource uri.
pub fn load_schema(ctx: Option<&CamelContext>, uri: &str) -> Result<Schema> {
    let r = uri.trim();
    let res = if r.starts_with("ref:") || r.starts_with("bean:") {
        Schema::parse(&load_resource(ctx, r)?, None)
    } else {
        let p = resource_path(r);
        if !p.exists() {
            return Err(CamelError::new(kinds::FILE_NOT_FOUND, format!("Cannot find resource: {} for schema", uri)));
        }
        Schema::parse_file(&p)
    };
    res.map_err(|m| CamelError::illegal_argument(format!("Cannot load schema {}: {}", uri, m)))
}

impl ValidatorProducer {
    fn schema(&self, ex: &Exchange) -> Result<Arc<Schema>> {
        let ctx = ex.context.clone().or_else(|| self.ctx.upgrade());
        if !self.cfg.shared {
            return Ok(Arc::new(load_schema(ctx.as_ref(), &self.cfg.resource_uri)?));
        }
        let mut g = self.schema.lock().unwrap();
        if let Some(s) = g.as_ref() {
            return Ok(s.clone());
        }
        let s = Arc::new(load_schema(ctx.as_ref(), &self.cfg.resource_uri)?);
        *g = Some(s.clone());
        Ok(s)
    }
}

impl Processor for ValidatorProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let schema = self.schema(ex)?;
        let content = match &self.cfg.header_name {
            Some(h) => match ex.header(h) {
                Some(v) if !v.is_null() => v.clone(),
                _ if self.cfg.fail_on_null_header => return Err(CamelError::new(NO_XML_HEADER_VALIDATION_EXCEPTION, format!("No header with name {} present", h))),
                _ => return Ok(()),
            },
            None => match ex.body() {
                Value::Null if self.cfg.fail_on_null_body => {
                    return Err(CamelError::new(NO_XML_BODY_VALIDATION_EXCEPTION, format!("No body available on exchange {}", ex.exchange_id)))
                }
                Value::Null => return Ok(()),
                b => b.clone(),
            },
        };
        let parsed = match &content {
            Value::Bytes(b) => Document::parse_bytes(b),
            other => Document::parse(&other.to_string()),
        };
        let errors: Vec<String> = match parsed {
            Err(e) => vec![e.to_string()],
            Ok(doc) => schema.validate(&doc).into_iter().map(|e| e.to_string()).collect(),
        };
        if !errors.is_empty() {
            ex.set_property(VALIDATION_ERRORS_PROPERTY, Value::Array(errors.iter().map(|e| Value::String(e.clone())).collect()));
            return Err(CamelError::new(SCHEMA_VALIDATION_EXCEPTION, format!("Validation failed for: {} errors: [{}]", self.cfg.resource_uri, errors.join(", "))));
        }
        Ok(())
    }
}

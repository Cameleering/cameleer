//! `validator:` component - XML validation (Camel `camel-validator`), **reduced to a
//! well-formedness check**.
//!
//! `validator:schemas/order.xsd?failOnNullBody=true&headerName=`
//!
//! Cameleer has no XML Schema engine. The producer parses the body (or the header
//! given by `headerName`) with the built-in XML parser ([`crate::language::xml`]) and
//! fails with `SchemaValidationException` (a `ValidationException`) when it is not
//! well-formed. The XSD itself is loaded (it must exist and be well-formed XML) and only
//! one schema rule is applied: when the schema declares global `xs:element`s, the
//! document's root element must be one of them. Content models, types and facets are
//! **not** checked. A null body fails with `NoXmlBodyValidationException`
//! (`failOnNullBody`, default true), a missing header with
//! `NoXmlHeaderValidationException` (`failOnNullHeader`).

use std::any::Any;
use std::sync::{Arc, Mutex, OnceLock};

use crate::context::{CamelContext, WeakContext};
use crate::error::{kinds, register_kind_parent, CamelError, Result};
use crate::language::xml::{Document, NodeKind};
use crate::message::Exchange;
use crate::spi::{Component, Endpoint, Processor, Producer, Service};
use crate::util::Parameters;
use crate::value::Value;

use super::template_support::load_resource;

pub const SCHEMA_VALIDATION_EXCEPTION: &str = "SchemaValidationException";
pub const NO_XML_BODY_VALIDATION_EXCEPTION: &str = "NoXmlBodyValidationException";
pub const NO_XML_HEADER_VALIDATION_EXCEPTION: &str = "NoXmlHeaderValidationException";

fn register_kinds() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        register_kind_parent(SCHEMA_VALIDATION_EXCEPTION, kinds::VALIDATION);
        register_kind_parent(NO_XML_BODY_VALIDATION_EXCEPTION, kinds::VALIDATION);
        register_kind_parent(NO_XML_HEADER_VALIDATION_EXCEPTION, kinds::VALIDATION);
    });
}

/// The `validator` component.
#[derive(Default)]
pub struct XmlValidatorComponent;

impl Component for XmlValidatorComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        register_kinds();
        if remaining.trim().is_empty() {
            return Err(CamelError::illegal_argument("validator requires a schema resource uri"));
        }
        let cfg = Config {
            resource_uri: remaining.to_string(),
            fail_on_null_body: params.take_bool("failOnNullBody", true)?,
            fail_on_null_header: params.take_bool("failOnNullHeader", true)?,
            header_name: params.take("headerName").filter(|s| !s.is_empty()),
        };
        for k in ["useSharedSchema", "schemaFactory", "schemaLanguage", "resourceResolver", "resourceResolverFactory", "errorHandler", "useDom", "schemaReaderFactory", "lazyStartProducer", "contentCache"] {
            params.take(k);
        }
        Ok(Arc::new(XmlValidatorEndpoint { uri: uri.to_string(), cfg, roots: Arc::new(Mutex::new(None)), ctx: ctx.downgrade() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Clone, Debug)]
struct Config {
    resource_uri: String,
    fail_on_null_body: bool,
    fail_on_null_header: bool,
    header_name: Option<String>,
}

/// A validator endpoint (producer only).
pub struct XmlValidatorEndpoint {
    uri: String,
    cfg: Config,
    /// Global element names declared by the schema (loaded on first use).
    roots: Arc<Mutex<Option<Vec<String>>>>,
    ctx: WeakContext,
}

impl Endpoint for XmlValidatorEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(XmlValidatorProducer { cfg: self.cfg.clone(), roots: self.roots.clone(), ctx: self.ctx.clone() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct XmlValidatorProducer {
    cfg: Config,
    roots: Arc<Mutex<Option<Vec<String>>>>,
    ctx: WeakContext,
}

impl Service for XmlValidatorProducer {}

fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

/// Global element names declared in an XSD document.
fn schema_roots(xsd: &str) -> Result<Vec<String>> {
    let doc = Document::parse(xsd).map_err(|e| CamelError::illegal_argument(format!("Invalid XML schema: {}", e.message)))?;
    let Some(root) = doc.root_element() else { return Ok(Vec::new()) };
    Ok(doc
        .element_children(root)
        .into_iter()
        .filter(|&c| local(doc.name(c)) == "element")
        .filter_map(|c| doc.attr(c, "name").map(|s| s.to_string()))
        .collect())
}

impl XmlValidatorProducer {
    fn roots(&self, ex: &Exchange) -> Result<Vec<String>> {
        let mut g = self.roots.lock().unwrap();
        if let Some(r) = g.as_ref() {
            return Ok(r.clone());
        }
        let ctx = ex.context.clone().or_else(|| self.ctx.upgrade());
        let r = schema_roots(&load_resource(ctx.as_ref(), &self.cfg.resource_uri)?)?;
        *g = Some(r.clone());
        Ok(r)
    }
}

impl Processor for XmlValidatorProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let roots = self.roots(ex)?;
        let content = match &self.cfg.header_name {
            Some(h) => match ex.header(h) {
                Some(v) if !v.is_null() => v.to_string(),
                _ if self.cfg.fail_on_null_header => return Err(CamelError::new(NO_XML_HEADER_VALIDATION_EXCEPTION, format!("No header with name {} present", h))),
                _ => return Ok(()),
            },
            None => match ex.body() {
                Value::Null if self.cfg.fail_on_null_body => {
                    return Err(CamelError::new(NO_XML_BODY_VALIDATION_EXCEPTION, format!("No body available on exchange {}", ex.exchange_id)))
                }
                Value::Null => return Ok(()),
                b => b.to_string(),
            },
        };
        let doc = Document::parse(&content).map_err(|e| {
            CamelError::new(SCHEMA_VALIDATION_EXCEPTION, format!("Validation failed for: {} errors: [{}]", self.cfg.resource_uri, e.message))
        })?;
        let root = doc.root_element().ok_or_else(|| {
            CamelError::new(SCHEMA_VALIDATION_EXCEPTION, format!("Validation failed for: {} errors: [no root element]", self.cfg.resource_uri))
        })?;
        if let NodeKind::Element { name, .. } = &doc.node(root).kind {
            if !roots.is_empty() && !roots.iter().any(|r| r == local(name)) {
                return Err(CamelError::new(
                    SCHEMA_VALIDATION_EXCEPTION,
                    format!(
                        "Validation failed for: {} errors: [cvc-elt.1.a: Cannot find the declaration of element '{}'.]",
                        self.cfg.resource_uri, name
                    ),
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_of_schema() {
        let xsd = r#"<?xml version="1.0"?><xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="order"/><xs:complexType name="t"/><xs:element name="invoice"/></xs:schema>"#;
        assert_eq!(schema_roots(xsd).unwrap(), ["order", "invoice"]);
        assert!(schema_roots("<broken").is_err());
    }
}

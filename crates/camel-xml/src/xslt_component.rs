//! `xslt:` and `xslt-saxon:` components (Camel `camel-xslt` / `camel-xslt-saxon`) backed by
//! the XSLT 1.0 processor in [`crate::xslt`].
//!
//! `xslt:file:templates/a.xsl?output=string&contentCache=true&deleteOutputFile=false`
//!
//! * Resource: `file:path`, `classpath:path` (relative to the working directory),
//!   `ref:beanName` (a `String` bean) or a plain path. `xsl:include` / `xsl:import` /
//!   `document()` resolve relative to the stylesheet file.
//! * `output`: `string` (default), `bytes`, `DOM` (the serialized string, as there is no
//!   DOM type in `Value`), `file` (written to the file named by the `CamelXsltFileName`
//!   header; the body becomes the file path; `deleteOutputFile=true` deletes it when the
//!   exchange completes).
//! * `contentCache` (default true): compile the stylesheet once.
//! * `failOnNullBody` (default true), `allowTemplateFromHeader` (default false: enables the
//!   `CamelXsltStylesheet` and `CamelXsltResourceUri` headers).
//! * Parameters: exchange properties, then message headers, are passed as global
//!   `xsl:param` values, as in Camel.
//! * Accepted and ignored: `transformerFactory`, `transformerFactoryClass`,
//!   `transformerFactoryConfigurationStrategy`, `uriResolver`, `uriResolverFactory`,
//!   `resultHandlerFactory`, `entityResolver`, `errorListener`, `xsltMessageLogger`,
//!   `transformerCacheSize`, `allowStAX`, `lazyStartProducer`, and for `xslt-saxon` also
//!   `saxonExtensionFunctions`, `saxonConfiguration`, `saxonReaderProperties`,
//!   `secureProcessing`.
//!
//! `xslt-saxon` uses the same XSLT 1.0 processor: stylesheets using XSLT 2.0/3.0
//! instructions or XPath 2.0 expressions fail with a `TransformerConfigurationException`
//! naming the unsupported feature.

use std::any::Any;
use std::sync::{Arc, Mutex};

use camel_core::component::template_support::{load_resource, resource_path};
use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{kinds, CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::{Component, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::value::Value;

use crate::dom::Document;
use crate::xslt::Stylesheet;

/// Header naming the output file for `output=file`.
pub const XSLT_FILE_NAME: &str = "CamelXsltFileName";
/// Header with a stylesheet resource uri (with `allowTemplateFromHeader`).
pub const XSLT_RESOURCE_URI: &str = "CamelXsltResourceUri";
/// Header with stylesheet text (with `allowTemplateFromHeader`).
pub const XSLT_STYLESHEET: &str = "CamelXsltStylesheet";

/// The `xslt` / `xslt-saxon` component.
pub struct XsltComponent {
    saxon: bool,
}

impl XsltComponent {
    pub fn new(saxon: bool) -> XsltComponent {
        XsltComponent { saxon }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Output {
    String,
    Bytes,
    Dom,
    File,
}

#[derive(Clone, Debug)]
struct Config {
    resource_uri: String,
    output: Output,
    content_cache: bool,
    fail_on_null_body: bool,
    delete_output_file: bool,
    allow_template_from_header: bool,
}

impl Component for XsltComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        crate::register_kinds();
        if remaining.trim().is_empty() {
            return Err(CamelError::illegal_argument("xslt requires a stylesheet resource uri"));
        }
        let output = match params.take("output").as_deref().map(|s| s.to_ascii_lowercase()) {
            None => Output::String,
            Some(o) => match o.as_str() {
                "string" => Output::String,
                "bytes" => Output::Bytes,
                "dom" => Output::Dom,
                "file" => Output::File,
                other => return Err(CamelError::illegal_argument(format!("Invalid xslt output: {} (string, bytes, DOM or file)", other))),
            },
        };
        let cfg = Config {
            resource_uri: remaining.to_string(),
            output,
            content_cache: params.take_bool("contentCache", true)?,
            fail_on_null_body: params.take_bool("failOnNullBody", true)?,
            delete_output_file: params.take_bool("deleteOutputFile", false)?,
            allow_template_from_header: params.take_bool("allowTemplateFromHeader", false)?,
        };
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
            "saxonExtensionFunctions",
            "saxonConfiguration",
            "saxonReaderProperties",
            "secureProcessing",
        ] {
            params.take(k);
        }
        let _ = self.saxon;
        Ok(Arc::new(XsltEndpoint { uri: uri.to_string(), cfg, cache: Arc::new(Mutex::new(None)), ctx: ctx.downgrade() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// An xslt endpoint (producer only).
pub struct XsltEndpoint {
    uri: String,
    cfg: Config,
    cache: Arc<Mutex<Option<Arc<Stylesheet>>>>,
    ctx: WeakContext,
}

impl Endpoint for XsltEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(XsltProducer { cfg: self.cfg.clone(), cache: self.cache.clone(), ctx: self.ctx.clone() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct XsltProducer {
    cfg: Config,
    cache: Arc<Mutex<Option<Arc<Stylesheet>>>>,
    ctx: WeakContext,
}

impl Service for XsltProducer {}

/// Compile a stylesheet from a resource uri (files compile with their directory as base).
pub fn load_stylesheet(ctx: Option<&CamelContext>, resource_uri: &str) -> Result<Stylesheet> {
    let r = resource_uri.trim();
    if r.starts_with("ref:") || r.starts_with("bean:") {
        let text = load_resource(ctx, r)?;
        return Stylesheet::compile(&text, None);
    }
    let path = resource_path(r);
    Stylesheet::compile_file(&path)
}

impl XsltProducer {
    fn stylesheet(&self, ex: &Exchange) -> Result<Arc<Stylesheet>> {
        let ctx = ex.context.clone().or_else(|| self.ctx.upgrade());
        if self.cfg.allow_template_from_header {
            if let Some(t) = ex.message.headers.get(XSLT_STYLESHEET).filter(|v| !v.is_null()) {
                return Ok(Arc::new(Stylesheet::compile(&t.to_string(), None)?));
            }
            if let Some(u) = ex.message.headers.get(XSLT_RESOURCE_URI).filter(|v| !v.is_null()) {
                return Ok(Arc::new(load_stylesheet(ctx.as_ref(), &u.to_string())?));
            }
        }
        if self.cfg.content_cache {
            let mut g = self.cache.lock().unwrap();
            if let Some(s) = g.as_ref() {
                return Ok(s.clone());
            }
            let s = Arc::new(load_stylesheet(ctx.as_ref(), &self.cfg.resource_uri)?);
            *g = Some(s.clone());
            return Ok(s);
        }
        Ok(Arc::new(load_stylesheet(ctx.as_ref(), &self.cfg.resource_uri)?))
    }
}

/// Parameters passed to a stylesheet: exchange properties, then headers.
pub fn exchange_parameters(ex: &Exchange) -> Vec<(String, Value)> {
    let mut params: Vec<(String, Value)> = Vec::new();
    for (k, v) in ex.properties.iter() {
        params.push((k.clone(), v.clone()));
    }
    for (k, v) in ex.message.headers.iter() {
        params.retain(|(pk, _)| pk != k);
        params.push((k.to_string(), v.clone()));
    }
    params
}

/// Parse a message body as a source document.
pub fn body_document(body: &Value) -> Result<Document> {
    match body {
        Value::Bytes(b) => Ok(Document::parse_bytes(b)?),
        Value::String(s) => Ok(Document::parse(s)?),
        Value::Object(_) | Value::Array(_) => Err(CamelError::new(
            kinds::NO_TYPE_CONVERSION_AVAILABLE,
            format!("No type converter available to convert from type: {} to the required type: javax.xml.transform.Source", body.type_name()),
        )),
        other => Ok(Document::parse(&other.to_string())?),
    }
}

impl Processor for XsltProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        if ex.message.body.is_null() {
            if self.cfg.fail_on_null_body {
                return Err(CamelError::illegal_argument("Input body is null"));
            }
            return Ok(());
        }
        let ss = self.stylesheet(ex)?;
        let doc = body_document(&ex.message.body)?;
        let params = exchange_parameters(ex);
        let result = ss.transform(doc, &params)?;
        match self.cfg.output {
            Output::String | Output::Dom => ex.message.body = Value::String(result.output),
            Output::Bytes => ex.message.body = Value::Bytes(result.output.into_bytes()),
            Output::File => {
                let name = ex
                    .message
                    .headers
                    .get(XSLT_FILE_NAME)
                    .filter(|v| !v.is_null())
                    .map(|v| v.to_string())
                    .ok_or_else(|| CamelError::new(kinds::NO_SUCH_HEADER, format!("No '{}' header present", XSLT_FILE_NAME)))?;
                let path = std::path::PathBuf::from(&name);
                if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                    std::fs::create_dir_all(parent).map_err(|e| CamelError::new(kinds::IO, format!("Cannot create directory {}: {}", parent.display(), e)))?;
                }
                std::fs::write(&path, result.output.as_bytes()).map_err(|e| CamelError::new(kinds::IO, format!("Cannot write {}: {}", name, e)))?;
                ex.message.body = Value::String(name.clone());
                if self.cfg.delete_output_file {
                    ex.add_on_completion(move |_| {
                        let _ = std::fs::remove_file(&path);
                    });
                }
            }
        }
        Ok(())
    }
}

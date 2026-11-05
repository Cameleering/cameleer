//! Template components: `string-template:`, `velocity:` and `freemarker:`.
//!
//! Each component uses a real engine implemented in this crate (see
//! [`velocity`](super::velocity), [`freemarker`](super::freemarker) and
//! [`stringtemplate`](super::stringtemplate)) and shares the data model and the options of
//! [`template_support`](super::template_support) (`body`, `headers`, `exchange.properties`,
//! `variables`, `request`/`in`, `camelContext` with `allowContextMapAll`).
//!
//! - `string-template:file.st?delimiterStart=<&delimiterStop=>` — headers
//!   `CamelStringTemplateTemplate`/`CamelStringTemplateResourceUri`.
//! - `velocity:file.vm` — headers `CamelVelocityTemplate`/`CamelVelocityResourceUri`,
//!   `CamelVelocityContext` (a map used as the whole context) and
//!   `CamelVelocitySupplementalContext` (a map merged into the context).
//! - `freemarker:file.ftl` — headers `CamelFreemarkerTemplate`/`CamelFreemarkerResourceUri`
//!   and `CamelFreemarkerDataModel` (a map used as the data model).

use std::any::Any;
use std::sync::Arc;

use crate::context::CamelContext;
use crate::error::Result;
use crate::spi::{Component, Endpoint};
use crate::util::Parameters;

pub use super::freemarker::FreemarkerEngine;
pub use super::stringtemplate::StringTemplateEngine;
pub use super::velocity::VelocityEngine;

use super::template_support::{TemplateEndpoint, TemplateEngine, TemplateHeaders, TemplateOptions};

fn create(
    ctx: &CamelContext,
    uri: &str,
    remaining: &str,
    params: &mut Parameters,
    headers: TemplateHeaders,
    engine: Arc<dyn TemplateEngine>,
) -> Result<Arc<dyn Endpoint>> {
    let options = TemplateOptions::from_params(params)?;
    Ok(Arc::new(TemplateEndpoint::new(ctx, uri, remaining, options, headers, engine)?))
}

/// The `string-template` component.
#[derive(Default)]
pub struct StringTemplateComponent;

impl Component for StringTemplateComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let engine = StringTemplateEngine {
            start: params.take("delimiterStart").and_then(|s| s.chars().next()).unwrap_or('<'),
            stop: params.take("delimiterStop").and_then(|s| s.chars().next()).unwrap_or('>'),
        };
        let headers = TemplateHeaders { template: "CamelStringTemplateTemplate", resource_uri: "CamelStringTemplateResourceUri", data_model: None, supplemental: None };
        create(ctx, uri, remaining, params, headers, Arc::new(engine))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The `velocity` component.
#[derive(Default)]
pub struct VelocityComponent;

impl Component for VelocityComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        for k in ["loaderCache", "propertiesFile"] {
            params.take(k);
        }
        let headers = TemplateHeaders {
            template: "CamelVelocityTemplate",
            resource_uri: "CamelVelocityResourceUri",
            data_model: Some("CamelVelocityContext"),
            supplemental: Some("CamelVelocitySupplementalContext"),
        };
        create(ctx, uri, remaining, params, headers, Arc::new(VelocityEngine))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The `freemarker` component.
#[derive(Default)]
pub struct FreemarkerComponent;

impl Component for FreemarkerComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        for k in ["configuration", "templateUpdateDelay", "localizedLookup"] {
            params.take(k);
        }
        let headers = TemplateHeaders {
            template: "CamelFreemarkerTemplate",
            resource_uri: "CamelFreemarkerResourceUri",
            data_model: Some("CamelFreemarkerDataModel"),
            supplemental: None,
        };
        create(ctx, uri, remaining, params, headers, Arc::new(FreemarkerEngine))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

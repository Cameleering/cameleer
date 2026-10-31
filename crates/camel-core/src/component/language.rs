//! `language:` component - evaluate a script in any registered language.
//!
//! `language:simple:Hello ${body}?transform=true`
//!
//! The script comes from the uri path, the `script` option (prefix `resource:file:` to load
//! it from a file), the `CamelLanguageScript` header, or else the message body. With
//! `transform=true` (default) the result replaces the body.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::context::{CamelContext, WeakContext};
use crate::error::{CamelError, Result};
use crate::message::Exchange;
use crate::spi::{Component, Endpoint, Expression, Processor, Producer, Service};
use crate::util::Parameters;

pub const LANGUAGE_SCRIPT: &str = "CamelLanguageScript";

#[derive(Default)]
pub struct LanguageComponent;

fn load_script(s: &str) -> Result<String> {
    if let Some(path) = s.strip_prefix("resource:file:").or_else(|| s.strip_prefix("file:")) {
        return std::fs::read_to_string(path).map_err(|e| CamelError::io(format!("Cannot load script {}: {}", path, e)));
    }
    if s.starts_with("resource:classpath:") || s.starts_with("classpath:") {
        return Err(CamelError::illegal_argument(format!("classpath resources are not supported: {}", s)));
    }
    Ok(s.to_string())
}

impl Component for LanguageComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let (lang, path_script) = match remaining.split_once(':') {
            Some((l, s)) => (l.to_string(), Some(s.to_string())),
            None => (remaining.to_string(), None),
        };
        let language = ctx.resolve_language(&lang)?;
        let transform = params.take_bool("transform", true)?;
        let script = match params.take("script").or(path_script).filter(|s| !s.is_empty()) {
            Some(s) => Some(load_script(&s)?),
            None => None,
        };
        let mut options = BTreeMap::new();
        if let Some(rt) = params.take("resultType") {
            options.insert("resultType".to_string(), rt);
        }
        for k in ["contentCache", "cacheScript", "binary", "allowContextMapAll", "allowTemplateFromHeader", "lazyStartProducer"] {
            params.take(k);
        }
        let expression = match &script {
            Some(s) => Some(language.create_expression(s, &options)?),
            None => None,
        };
        Ok(Arc::new(LanguageEndpoint { uri: uri.to_string(), lang, transform, expression, options, ctx: ctx.downgrade() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct LanguageEndpoint {
    uri: String,
    lang: String,
    transform: bool,
    expression: Option<Arc<dyn Expression>>,
    options: BTreeMap<String, String>,
    ctx: WeakContext,
}

impl Endpoint for LanguageEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(LanguageProducer {
            lang: self.lang.clone(),
            transform: self.transform,
            expression: self.expression.clone(),
            options: self.options.clone(),
            ctx: self.ctx.clone(),
        }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct LanguageProducer {
    lang: String,
    transform: bool,
    expression: Option<Arc<dyn Expression>>,
    options: BTreeMap<String, String>,
    ctx: WeakContext,
}

impl Service for LanguageProducer {}

impl Processor for LanguageProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let expr = match (&self.expression, ex.header(LANGUAGE_SCRIPT).map(|v| v.to_string())) {
            (_, Some(script)) => self.dynamic(ex, &script)?,
            (Some(e), None) => e.clone(),
            (None, None) => {
                let script = ex.body_as::<String>()?;
                self.dynamic(ex, &script)?
            }
        };
        let v = expr.evaluate(ex)?;
        if self.transform {
            ex.set_body(v);
        }
        Ok(())
    }
}

impl LanguageProducer {
    fn dynamic(&self, ex: &Exchange, script: &str) -> Result<Arc<dyn Expression>> {
        let ctx = ex.context.clone().or_else(|| self.ctx.upgrade()).ok_or_else(|| CamelError::illegal_state("No CamelContext"))?;
        ctx.resolve_language(&self.lang)?.create_expression(&load_script(script)?, &self.options)
    }
}

//! Interceptors: `intercept` (every step), `interceptFrom(pattern)` (route inputs) and
//! `interceptSendToEndpoint(pattern)` (sends to matching endpoints).

use std::sync::Arc;

use crate::builder::ExpressionDefinition;
use crate::error::Result;
use crate::impl_definition_boilerplate;
use crate::message::{props, Exchange};
use crate::model::{OutputList, ProcessorDefinition, RouteContext};
use crate::spi::{Expression, Predicate, Processor};

use super::{matches_opt, resolve_opt_pred};

/// Which kind of interceptor.
#[derive(Clone, Debug, PartialEq)]
pub enum InterceptKind {
    /// Intercept every processing step.
    All,
    /// Intercept exchanges consumed from endpoints matching the pattern.
    From(String),
    /// Intercept sends to endpoints matching the pattern.
    SendToEndpoint { pattern: String, skip_send_to_original_endpoint: bool },
}

/// An interceptor block.
pub struct InterceptDefinition {
    pub id: Option<String>,
    pub kind: InterceptKind,
    pub on_when: Option<ExpressionDefinition>,
    pub outputs: OutputList,
}

impl InterceptDefinition {
    pub fn new(kind: InterceptKind) -> Self {
        InterceptDefinition { id: None, kind, on_when: None, outputs: OutputList::default() }
    }

    pub(crate) fn create_runtime(&self, rc: &RouteContext) -> Result<InterceptRuntime> {
        let processor = if self.outputs.outputs.is_empty() {
            Arc::new(crate::processor::pipeline::Pipeline::new(vec![])) as Arc<dyn Processor>
        } else {
            rc.create_child_processor(&self.outputs.outputs)?
        };
        Ok(InterceptRuntime { kind: self.kind.clone(), on_when: resolve_opt_pred(&self.on_when, &rc.context)?, processor })
    }
}

impl ProcessorDefinition for InterceptDefinition {
    fn name(&self) -> &str {
        match self.kind {
            InterceptKind::All => "intercept",
            InterceptKind::From(_) => "interceptFrom",
            InterceptKind::SendToEndpoint { .. } => "interceptSendToEndpoint",
        }
    }
    fn create_processor(&self, _rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        Ok(Arc::new(|_: &mut Exchange| Ok(())))
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
    fn wrap_in_channel(&self) -> bool {
        false
    }
    impl_definition_boilerplate!();
}

/// Runtime interceptor.
#[derive(Clone)]
pub(crate) struct InterceptRuntime {
    pub kind: InterceptKind,
    pub on_when: Option<Arc<dyn Predicate>>,
    pub processor: Arc<dyn Processor>,
}

impl InterceptRuntime {
    pub(crate) fn pattern(&self) -> Option<&str> {
        match &self.kind {
            InterceptKind::All => None,
            InterceptKind::From(p) => Some(p),
            InterceptKind::SendToEndpoint { pattern, .. } => Some(pattern),
        }
    }

    pub(crate) fn skip(&self) -> bool {
        matches!(self.kind, InterceptKind::SendToEndpoint { skip_send_to_original_endpoint: true, .. })
    }

    /// Run the interceptor (if its `when` matches). Returns `(intercepted, proceed)`.
    pub(crate) fn run(&self, ex: &mut Exchange) -> Result<(bool, bool)> {
        if !matches_opt(&self.on_when, ex) {
            return Ok((false, true));
        }
        self.processor.process(ex)?;
        Ok((true, ex.should_continue()))
    }
}

/// True if `uri` matches an interceptor endpoint pattern.
pub(crate) fn uri_matches(uris: &[String], pattern: &str) -> bool {
    uris.iter().any(|u| crate::util::matches_pattern(u, pattern) || crate::util::matches_pattern(&u.replacen("://", ":", 1), pattern))
}

/// Runs `intercept()` interceptors before each step.
pub(crate) struct InterceptProcessor {
    pub intercepts: Arc<Vec<InterceptRuntime>>,
    pub inner: Arc<dyn Processor>,
}

impl Processor for InterceptProcessor {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        for i in self.intercepts.iter() {
            let (_, proceed) = i.run(ex)?;
            if !proceed {
                return Ok(());
            }
        }
        self.inner.process(ex)
    }
}

/// Runs `interceptSendToEndpoint` interceptors around a send.
pub(crate) struct SendInterceptProcessor {
    /// Static uri (from `to`) or dynamic expression (from `toD`).
    pub static_uris: Vec<String>,
    pub dynamic_uri: Option<Arc<dyn Expression>>,
    pub intercepts: Vec<InterceptRuntime>,
    pub inner: Arc<dyn Processor>,
}

impl Processor for SendInterceptProcessor {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let (uris, header_uri) = match &self.dynamic_uri {
            Some(e) => {
                let u = e.evaluate(ex)?.to_string().trim().to_string();
                (vec![u.clone()], u)
            }
            None => (self.static_uris.clone(), self.static_uris.last().cloned().unwrap_or_default()),
        };
        let mut skip = false;
        for i in &self.intercepts {
            if self.dynamic_uri.is_some() && !uri_matches(&uris, i.pattern().unwrap_or("*")) {
                continue;
            }
            if !matches_opt(&i.on_when, ex) {
                continue;
            }
            ex.set_header(props::INTERCEPTED_ENDPOINT, header_uri.clone());
            let (intercepted, proceed) = i.run(ex)?;
            if !proceed {
                return Ok(());
            }
            if intercepted && i.skip() {
                skip = true;
            }
        }
        if skip {
            return Ok(());
        }
        self.inner.process(ex)
    }
}

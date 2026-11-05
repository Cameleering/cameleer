//! `onException` clauses.
//!
//! A clause is a block definition: its outputs are built with the normal DSL. Route-scoped
//! clauses live in the route's outputs (they create a no-op processor in the pipeline and
//! are collected by the route hook); builder-scoped clauses are collected per builder.

use std::sync::Arc;

use crate::builder::ExpressionDefinition;
use crate::error::Result;
use crate::impl_definition_boilerplate;
use crate::model::{OutputList, ProcessorDefinition, RouteContext};
use crate::spi::Processor;

use super::handler::{ExceptionPolicy, RedeliveryOverrides, RedeliveryPolicy};
use super::{resolve_opt_pred, BoolOrPredicate};

/// An `onException` clause.
#[derive(Default)]
pub struct OnExceptionDefinition {
    pub id: Option<String>,
    /// Exception kinds handled by this clause (matched with the kind hierarchy).
    pub exceptions: Vec<String>,
    pub handled: Option<BoolOrPredicate>,
    pub continued: Option<BoolOrPredicate>,
    pub on_when: Option<ExpressionDefinition>,
    pub retry_while: Option<ExpressionDefinition>,
    pub policy: RedeliveryOverrides,
    pub on_redelivery: Option<Arc<dyn Processor>>,
    pub on_exception_occurred: Option<Arc<dyn Processor>>,
    pub use_original_message: bool,
    pub use_original_body: bool,
    pub outputs: OutputList,
}

impl OnExceptionDefinition {
    pub fn new(exceptions: &[&str]) -> Self {
        let exceptions = if exceptions.is_empty() { vec!["Exception".to_string()] } else { exceptions.iter().map(|s| s.to_string()).collect() };
        OnExceptionDefinition { exceptions, ..Default::default() }
    }

    /// Build the runtime policy. The outputs are created with `rc` (call before the error
    /// handler channel is installed so they are not wrapped by it).
    pub(crate) fn create_policy(&self, rc: &RouteContext, base: &RedeliveryPolicy, route_scoped: bool) -> Result<ExceptionPolicy> {
        let ctx = &rc.context;
        let processor = if self.outputs.outputs.is_empty() { None } else { Some(rc.create_child_processor(&self.outputs.outputs)?) };
        Ok(ExceptionPolicy {
            exceptions: self.exceptions.clone(),
            route_scoped,
            on_when: resolve_opt_pred(&self.on_when, ctx)?,
            retry_while: resolve_opt_pred(&self.retry_while, ctx)?,
            handled: self.handled.as_ref().map(|h| h.resolve(ctx)).transpose()?,
            continued: self.continued.as_ref().map(|h| h.resolve(ctx)).transpose()?,
            policy: self.policy.apply(base),
            on_redelivery: self.on_redelivery.clone(),
            on_exception_occurred: self.on_exception_occurred.clone(),
            use_original_message: self.use_original_message,
            use_original_body: self.use_original_body,
            processor,
        })
    }
}

impl ProcessorDefinition for OnExceptionDefinition {
    fn name(&self) -> &str {
        "onException"
    }
    fn label(&self) -> String {
        format!("onException[{}]", self.exceptions.join(", "))
    }
    fn create_processor(&self, _rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        // handled by the route hook; in the pipeline it is a no-op
        Ok(Arc::new(|_: &mut crate::message::Exchange| Ok(())))
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

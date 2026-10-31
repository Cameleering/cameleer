//! Script EIP (Camel `script`): evaluate an expression in any language for its side
//! effects. The result is discarded and the message is left unchanged (unless the script
//! itself changes it, e.g. a processor-backed expression).

use std::sync::Arc;

use crate::builder::ExpressionDefinition;
use crate::error::Result;
use crate::impl_definition_boilerplate;
use crate::message::Exchange;
use crate::model::{ProcessorDefinition, RouteContext};
use crate::spi::Processor;

/// Model of the `script` EIP.
pub struct ScriptDefinition {
    pub id: Option<String>,
    pub expression: ExpressionDefinition,
}

impl ScriptDefinition {
    pub fn new(expression: impl Into<ExpressionDefinition>) -> Self {
        ScriptDefinition { id: None, expression: expression.into() }
    }
}

impl ProcessorDefinition for ScriptDefinition {
    fn name(&self) -> &str {
        "script"
    }
    fn label(&self) -> String {
        format!("script[{:?}]", self.expression)
    }
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        let e = self.expression.resolve_expression(&rc.context)?;
        Ok(Arc::new(move |ex: &mut Exchange| {
            e.evaluate(ex)?;
            Ok(())
        }))
    }
    impl_definition_boilerplate!();
}

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

/// Registry name of an optional factory turning `(language, text, options)` into a processor
/// for the `script` EIP (bound by script-language crates such as camel-script).
pub const SCRIPT_PROCESSOR_FACTORY: &str = "camelScriptProcessorFactory";

/// Type of the factory bound under [`SCRIPT_PROCESSOR_FACTORY`]; `None` means "not my language".
pub type ScriptProcessorFactory =
    Box<dyn Fn(&str, &str, &std::collections::BTreeMap<String, String>) -> Option<Result<Arc<dyn Processor>>> + Send + Sync>;

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
        // Script languages (e.g. camel-script's js/groovy/python) can register a factory that
        // runs the script as a processor, so its changes to the exchange are kept.
        if let ExpressionDefinition::Language { language, expression, options } = &self.expression {
            if let Some(f) = rc.context.registry().lookup::<ScriptProcessorFactory>(SCRIPT_PROCESSOR_FACTORY) {
                if let Some(p) = f(language, &rc.context.resolve_placeholders(expression)?, options) {
                    return p;
                }
            }
        }
        let e = self.expression.resolve_expression(&rc.context)?;
        Ok(Arc::new(move |ex: &mut Exchange| {
            e.evaluate(ex)?;
            Ok(())
        }))
    }
    impl_definition_boilerplate!();
}

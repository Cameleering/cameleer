//! Splitter EIP.

use std::sync::Arc;

use crate::builder::ExpressionDefinition;
use crate::error::Result;
use crate::impl_definition_boilerplate;
use crate::message::{props, Exchange};
use crate::model::{OutputList, ProcessorDefinition, RouteContext};
use crate::spi::{Expression, Processor};

use super::multicast::MulticastEngine;
use super::options::{EipOption, EipOptions, MulticastSettings};
use super::support::to_iterable;

/// Splitter: the expression result is split into parts (array elements; strings are
/// split on `delimiter`, default `,`), each part is routed as a sub-exchange
/// (a correlated copy with the part as body) through the block's outputs.
///
/// Without an aggregation strategy the original exchange continues unchanged
/// (but a sub-exchange failure is propagated); with a strategy the aggregated
/// result replaces the message.
pub struct SplitDefinition {
    pub id: Option<String>,
    pub expression: ExpressionDefinition,
    pub outputs: OutputList,
    pub settings: MulticastSettings,
    /// Delimiter for string results; `None` or `"false"` disables splitting strings.
    pub delimiter: Option<String>,
}

impl SplitDefinition {
    pub fn new(expression: ExpressionDefinition) -> Self {
        SplitDefinition {
            id: None,
            expression,
            outputs: OutputList::default(),
            settings: MulticastSettings::default(),
            delimiter: Some(",".to_string()),
        }
    }
}

impl EipOptions for SplitDefinition {
    fn apply_option(&mut self, option: &EipOption) -> bool {
        if let EipOption::Delimiter(d) = option {
            self.delimiter = if d == "false" { None } else { Some(d.clone()) };
            return true;
        }
        self.settings.apply(option)
    }
}

struct SplitProcessor {
    expression: Arc<dyn Expression>,
    delimiter: Option<String>,
    child: Arc<dyn Processor>,
    engine: MulticastEngine,
}

impl Processor for SplitProcessor {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        let value = self.expression.evaluate(exchange)?;
        let parts = to_iterable(value, self.delimiter.as_deref());
        let n = parts.len();
        let pairs = parts
            .into_iter()
            .enumerate()
            .map(|(i, part)| {
                let mut sub = exchange.correlated_copy();
                sub.message.body = part;
                sub.set_property(props::SPLIT_INDEX, i);
                sub.set_property(props::SPLIT_SIZE, n);
                sub.set_property(props::SPLIT_COMPLETE, i + 1 == n);
                (sub, self.child.clone())
            })
            .collect();
        self.engine.execute(exchange, pairs)
    }
}

impl ProcessorDefinition for SplitDefinition {
    fn name(&self) -> &str {
        "split"
    }
    fn label(&self) -> String {
        format!("split[{:?}]", self.expression)
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
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        let expression = self.expression.resolve_expression(&rc.context)?;
        let child = rc.create_child_processor(&self.outputs.outputs)?;
        let engine = MulticastEngine::create(rc, &self.settings, "split", false)?;
        Ok(Arc::new(SplitProcessor { expression, delimiter: self.delimiter.clone(), child, engine }))
    }
    impl_definition_boilerplate!();
}

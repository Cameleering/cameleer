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
use super::support::{to_iterable, to_lazy_iter};

/// Splitter: the expression result is split into parts (array elements; strings are
/// split on `delimiter`, default `,`), each part is routed as a sub-exchange
/// (a correlated copy with the part as body) through the block's outputs.
///
/// Without an aggregation strategy the original exchange continues unchanged
/// (but a sub-exchange failure is propagated); with a strategy the aggregated
/// result replaces the message.
///
/// With `streaming` (and no parallel processing) the parts are produced lazily: a string body
/// is split one part at a time and each sub-exchange is created only when the previous one
/// is done. As in Camel, `CamelSplitSize` is then only set on the last sub-exchange.
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
    /// The parts are discarded after their only step, an aggregate (no strategy, not
    /// parallel): the aggregate may move a part's message instead of copying it.
    consume_parts: bool,
}

impl Processor for SplitProcessor {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        let value = self.expression.evaluate(exchange)?;
        if self.engine.is_streaming() && !self.engine.is_parallel() {
            let mut parts = to_lazy_iter(value, self.delimiter.as_deref()).peekable();
            // a template without the (possibly large) body: parts become the bodies
            let body = std::mem::replace(&mut exchange.message.body, crate::value::Value::Null);
            let template = exchange.clone();
            exchange.message.body = body;
            let child = self.child.clone();
            let consume = self.consume_parts;
            let mut index = 0usize;
            let mut pairs = std::iter::from_fn(move || {
                let part = parts.next()?;
                let last = parts.peek().is_none();
                let mut sub = template.correlated_copy();
                sub.consumable = consume;
                sub.message.body = part;
                sub.set_property(props::SPLIT_INDEX, index);
                sub.set_property(props::SPLIT_COMPLETE, last);
                if last {
                    sub.set_property(props::SPLIT_SIZE, index + 1);
                }
                index += 1;
                Some((sub, child.clone()))
            });
            return self.engine.execute_lazy(exchange, &mut pairs);
        }
        let parts = to_iterable(value, self.delimiter.as_deref());
        let n = parts.len();
        if !self.engine.is_parallel() {
            // sequential: each sub-exchange is created when its turn comes (the size is
            // known up front, so every part still gets `CamelSplitSize`)
            let body = std::mem::replace(&mut exchange.message.body, crate::value::Value::Null);
            let template = exchange.clone();
            exchange.message.body = body;
            let child = self.child.clone();
            let consume = self.consume_parts;
            let mut pairs = parts.into_iter().enumerate().map(move |(i, part)| {
                let mut sub = template.correlated_copy();
                sub.consumable = consume;
                sub.message.body = part;
                sub.set_property(props::SPLIT_INDEX, i);
                sub.set_property(props::SPLIT_SIZE, n);
                sub.set_property(props::SPLIT_COMPLETE, i + 1 == n);
                (sub, child.clone())
            });
            return self.engine.execute_lazy(exchange, &mut pairs);
        }
        // copy the exchange without its body for each part (the part becomes the body)
        let body = std::mem::replace(&mut exchange.message.body, crate::value::Value::Null);
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
        exchange.message.body = body;
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
        let consume_parts = self.settings.strategy.is_none()
            && !self.settings.parallel_processing
            && self.outputs.outputs.len() == 1
            && self.outputs.outputs[0].name() == "aggregate";
        Ok(Arc::new(SplitProcessor { expression, delimiter: self.delimiter.clone(), child, engine, consume_parts }))
    }
    impl_definition_boilerplate!();
}

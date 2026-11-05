//! Message Filter EIP and the explicit pipeline block.

use std::sync::Arc;

use crate::builder::ExpressionDefinition;
use crate::error::Result;
use crate::impl_definition_boilerplate;
use crate::message::{props, Exchange};
use crate::model::{OutputList, ProcessorDefinition, RouteContext};
use crate::spi::{Predicate, Processor};

/// Message Filter: only exchanges matching the predicate are routed to the block's
/// outputs. Sets the `CamelFilterMatched` property.
pub struct FilterDefinition {
    pub id: Option<String>,
    pub predicate: ExpressionDefinition,
    pub outputs: OutputList,
}

impl FilterDefinition {
    pub fn new(predicate: ExpressionDefinition) -> Self {
        FilterDefinition { id: None, predicate, outputs: OutputList::default() }
    }
}

pub struct FilterProcessor {
    predicate: Arc<dyn Predicate>,
    child: Arc<dyn Processor>,
}

impl FilterProcessor {
    pub fn new(predicate: Arc<dyn Predicate>, child: Arc<dyn Processor>) -> Self {
        FilterProcessor { predicate, child }
    }
    /// Evaluate the predicate and record the result in `CamelFilterMatched`.
    pub fn matches(predicate: &dyn Predicate, exchange: &mut Exchange) -> Result<bool> {
        let m = predicate.matches(exchange)?;
        exchange.set_property(props::FILTER_MATCHED, m);
        Ok(m)
    }
}

impl Processor for FilterProcessor {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        self.process_timed(exchange, 0).0
    }

    /// The children's last clock reading is passed out (the filter ends when they do).
    fn process_timed(&self, exchange: &mut Exchange, _start: u64) -> (Result<()>, u64) {
        match Self::matches(self.predicate.as_ref(), exchange) {
            Err(e) => (Err(e), 0),
            Ok(true) => self.child.process_timed(exchange, 0),
            Ok(false) => (Ok(()), 0),
        }
    }
}

impl ProcessorDefinition for FilterDefinition {
    fn name(&self) -> &str {
        "filter"
    }
    fn label(&self) -> String {
        format!("filter[{:?}]", self.predicate)
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
        let predicate = self.predicate.resolve_predicate(&rc.context)?;
        let child = rc.create_child_processor(&self.outputs.outputs)?;
        Ok(Arc::new(FilterProcessor::new(predicate, child)))
    }
    impl_definition_boilerplate!();
}

/// Explicit pipeline block: the outputs run in sequence (pipes and filters).
#[derive(Default)]
pub struct PipelineDefinition {
    pub id: Option<String>,
    pub outputs: OutputList,
}

impl ProcessorDefinition for PipelineDefinition {
    fn name(&self) -> &str {
        "pipeline"
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
        if self.outputs.outputs.is_empty() {
            return Ok(Arc::new(crate::processor::pipeline::Pipeline::new(vec![])));
        }
        rc.create_child_processor(&self.outputs.outputs)
    }
    impl_definition_boilerplate!();
}

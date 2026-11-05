//! Content Based Router EIP: choice / when / otherwise.

use std::sync::Arc;

use crate::builder::ExpressionDefinition;
use crate::error::{CamelError, Result};
use crate::impl_definition_boilerplate;
use crate::message::Exchange;
use crate::model::{OutputList, ProcessorDefinition, RouteContext};
use crate::spi::{Predicate, Processor};

use super::filter::FilterProcessor;

/// A `when` clause of a choice.
pub struct WhenDefinition {
    pub id: Option<String>,
    pub predicate: ExpressionDefinition,
    pub outputs: OutputList,
}

impl WhenDefinition {
    pub fn new(predicate: ExpressionDefinition) -> Self {
        WhenDefinition { id: None, predicate, outputs: OutputList::default() }
    }
}

impl ProcessorDefinition for WhenDefinition {
    fn name(&self) -> &str {
        "when"
    }
    fn label(&self) -> String {
        format!("when[{:?}]", self.predicate)
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
    fn wrap_in_channel(&self) -> bool {
        false
    }
    impl_definition_boilerplate!();
}

/// The `otherwise` clause of a choice.
#[derive(Default)]
pub struct OtherwiseDefinition {
    pub id: Option<String>,
    pub outputs: OutputList,
}

impl ProcessorDefinition for OtherwiseDefinition {
    fn name(&self) -> &str {
        "otherwise"
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
        rc.create_child_processor(&self.outputs.outputs)
    }
    fn wrap_in_channel(&self) -> bool {
        false
    }
    impl_definition_boilerplate!();
}

/// Content Based Router. Outputs added while building go to the last `when`
/// (or to `otherwise` once it was opened).
#[derive(Default)]
pub struct ChoiceDefinition {
    pub id: Option<String>,
    pub whens: Vec<WhenDefinition>,
    pub otherwise: Option<OtherwiseDefinition>,
    /// Precondition mode: the predicates are evaluated once when the route is created
    /// and only the selected branch becomes part of the route.
    pub precondition: bool,
}

impl ChoiceDefinition {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a new `when` clause.
    pub fn add_when(&mut self, predicate: ExpressionDefinition) -> Result<()> {
        if self.otherwise.is_some() {
            return Err(CamelError::illegal_argument("when() cannot be added after otherwise() in a choice"));
        }
        self.whens.push(WhenDefinition::new(predicate));
        Ok(())
    }

    /// Start the `otherwise` clause.
    pub fn add_otherwise(&mut self) -> Result<()> {
        if self.otherwise.is_some() {
            return Err(CamelError::illegal_argument("otherwise() has already been specified on this choice"));
        }
        self.otherwise = Some(OtherwiseDefinition::default());
        Ok(())
    }
}

struct ChoiceProcessor {
    whens: Vec<(Arc<dyn Predicate>, Arc<dyn Processor>)>,
    otherwise: Option<Arc<dyn Processor>>,
}

impl Processor for ChoiceProcessor {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        self.process_timed(exchange, 0).0
    }

    /// The branch's last clock reading is passed out (the choice ends when its branch does).
    fn process_timed(&self, exchange: &mut Exchange, _start: u64) -> (Result<()>, u64) {
        for (p, child) in &self.whens {
            match FilterProcessor::matches(p.as_ref(), exchange) {
                Err(e) => return (Err(e), 0),
                Ok(true) => return child.process_timed(exchange, 0),
                Ok(false) => {}
            }
        }
        if let Some(o) = &self.otherwise {
            return o.process_timed(exchange, 0);
        }
        (Ok(()), 0)
    }
}

impl ProcessorDefinition for ChoiceDefinition {
    fn name(&self) -> &str {
        "choice"
    }
    fn is_block(&self) -> bool {
        true
    }
    fn add_output(&mut self, def: Box<dyn ProcessorDefinition>) -> Result<()> {
        if let Some(o) = self.otherwise.as_mut() {
            o.outputs.push(def);
            return Ok(());
        }
        match self.whens.last_mut() {
            Some(w) => {
                w.outputs.push(def);
                Ok(())
            }
            None => Err(CamelError::illegal_argument(format!(
                "choice() must have at least one when() before adding outputs ({})",
                def.label()
            ))),
        }
    }
    fn outputs(&self) -> Vec<&dyn ProcessorDefinition> {
        let mut v: Vec<&dyn ProcessorDefinition> = self.whens.iter().map(|w| w as &dyn ProcessorDefinition).collect();
        if let Some(o) = &self.otherwise {
            v.push(o);
        }
        v
    }
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        if self.precondition {
            let probe = rc.context.create_exchange();
            for w in &self.whens {
                if w.predicate.resolve_predicate(&rc.context)?.matches(&probe)? {
                    return rc.create_child_processor(&w.outputs.outputs);
                }
            }
            return match &self.otherwise {
                Some(o) => rc.create_child_processor(&o.outputs.outputs),
                None => Ok(Arc::new(crate::processor::pipeline::Pipeline::new(vec![]))),
            };
        }
        let mut whens = Vec::new();
        for w in &self.whens {
            let predicate = w.predicate.resolve_predicate(&rc.context)?;
            let child = rc.create_child_processor(&w.outputs.outputs)?;
            whens.push((predicate, child));
        }
        let otherwise = match &self.otherwise {
            Some(o) => Some(rc.create_child_processor(&o.outputs.outputs)?),
            None => None,
        };
        Ok(Arc::new(ChoiceProcessor { whens, otherwise }))
    }
    impl_definition_boilerplate!();
}

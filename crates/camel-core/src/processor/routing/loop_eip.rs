//! Loop EIP: run the block's outputs a number of times (count expression) or
//! while a predicate holds (`doWhile`). Sets `CamelLoopIndex` and (count mode) `CamelLoopSize`.

use std::sync::Arc;

use crate::builder::ExpressionDefinition;
use crate::error::{CamelError, Result};
use crate::impl_definition_boilerplate;
use crate::message::{props, Exchange};
use crate::model::{OutputList, ProcessorDefinition, RouteContext};
use crate::spi::{Expression, Predicate, Processor};

use super::options::{EipOption, EipOptions};
use super::support::{copy_results, run_processor};

/// Loop definition.
pub struct LoopDefinition {
    pub id: Option<String>,
    /// Count expression, or the while predicate when `do_while` is true.
    pub expression: ExpressionDefinition,
    pub do_while: bool,
    /// Each iteration starts from a copy of the input exchange (the result is the last iteration's).
    pub copy: bool,
    pub outputs: OutputList,
}

impl LoopDefinition {
    pub fn count(expression: ExpressionDefinition) -> Self {
        LoopDefinition { id: None, expression, do_while: false, copy: false, outputs: OutputList::default() }
    }
    pub fn do_while(predicate: ExpressionDefinition) -> Self {
        LoopDefinition { id: None, expression: predicate, do_while: true, copy: false, outputs: OutputList::default() }
    }
}

impl EipOptions for LoopDefinition {
    fn apply_option(&mut self, option: &EipOption) -> bool {
        match option {
            EipOption::Copy(b) => self.copy = *b,
            _ => return false,
        }
        true
    }
}

enum LoopMode {
    Count(Arc<dyn Expression>),
    While(Arc<dyn Predicate>),
}

struct LoopProcessor {
    mode: LoopMode,
    copy: bool,
    child: Arc<dyn Processor>,
}

impl LoopProcessor {
    fn iterate(&self, exchange: &mut Exchange, snapshot: &Option<Exchange>, index: i64, size: Option<i64>) {
        match snapshot {
            Some(snap) => {
                let mut it = snap.clone();
                it.set_property(props::LOOP_INDEX, index);
                if let Some(s) = size {
                    it.set_property(props::LOOP_SIZE, s);
                }
                run_processor(self.child.as_ref(), &mut it);
                copy_results(exchange, it);
            }
            None => {
                exchange.set_property(props::LOOP_INDEX, index);
                if let Some(s) = size {
                    exchange.set_property(props::LOOP_SIZE, s);
                }
                run_processor(self.child.as_ref(), exchange);
            }
        }
    }
}

impl Processor for LoopProcessor {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        let snapshot = if self.copy { Some(exchange.clone()) } else { None };
        match &self.mode {
            LoopMode::Count(e) => {
                let v = e.evaluate(exchange)?;
                let count = if v.is_null() {
                    0
                } else {
                    v.as_i64().ok_or_else(|| {
                        CamelError::new(crate::error::kinds::NO_TYPE_CONVERSION_AVAILABLE, format!("Loop count is not a number: {}", v))
                    })?
                };
                let mut i = 0;
                while i < count && exchange.should_continue() {
                    self.iterate(exchange, &snapshot, i, Some(count));
                    i += 1;
                }
            }
            LoopMode::While(p) => {
                let mut i = 0;
                while exchange.should_continue() && p.matches(exchange)? {
                    self.iterate(exchange, &snapshot, i, None);
                    i += 1;
                }
            }
        }
        Ok(())
    }
}

impl ProcessorDefinition for LoopDefinition {
    fn name(&self) -> &str {
        "loop"
    }
    fn label(&self) -> String {
        if self.do_while {
            format!("loop[doWhile {:?}]", self.expression)
        } else {
            format!("loop[{:?}]", self.expression)
        }
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
        let mode = if self.do_while {
            LoopMode::While(self.expression.resolve_predicate(&rc.context)?)
        } else {
            LoopMode::Count(self.expression.resolve_expression(&rc.context)?)
        };
        let child = rc.create_child_processor(&self.outputs.outputs)?;
        Ok(Arc::new(LoopProcessor { mode, copy: self.copy, child }))
    }
    impl_definition_boilerplate!();
}

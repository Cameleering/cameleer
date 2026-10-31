//! `doTry` / `doCatch` / `doFinally`.
//!
//! Inside the try block the error handler is suppressed (no redelivery, no onException):
//! failures go straight to the matching `doCatch`. Catch matching uses the exception kind
//! hierarchy over the whole cause chain; clauses are tried in order. `doFinally` always runs.

use std::sync::Arc;

use crate::builder::ExpressionDefinition;
use crate::error::{kind_is_a, CamelError, Result};
use crate::impl_definition_boilerplate;
use crate::message::{props, Exchange};
use crate::model::{OutputList, ProcessorDefinition, RouteContext};
use crate::processor::pipeline::{process_safely, Pipeline};
use crate::spi::{Predicate, Processor};

use super::{enter_suppressed, exit_suppressed, matches_opt, resolve_opt_pred, set_caught_exception};

/// A `doCatch` clause.
pub struct CatchClause {
    /// Exception kinds; empty = catch everything.
    pub exceptions: Vec<String>,
    pub on_when: Option<ExpressionDefinition>,
    pub outputs: OutputList,
}

/// Which part of the doTry receives new outputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrySection {
    Try,
    Catch(usize),
    Finally,
}

/// The `doTry` block.
pub struct DoTryDefinition {
    pub id: Option<String>,
    pub try_outputs: OutputList,
    pub catches: Vec<CatchClause>,
    pub finally: Option<OutputList>,
    pub section: TrySection,
}

impl Default for DoTryDefinition {
    fn default() -> Self {
        DoTryDefinition { id: None, try_outputs: OutputList::default(), catches: Vec::new(), finally: None, section: TrySection::Try }
    }
}

impl DoTryDefinition {
    /// Start a new `doCatch` clause.
    pub fn add_catch(&mut self, exceptions: &[&str]) {
        self.catches.push(CatchClause { exceptions: exceptions.iter().map(|s| s.to_string()).collect(), on_when: None, outputs: OutputList::default() });
        self.section = TrySection::Catch(self.catches.len() - 1);
    }
    /// Start the `doFinally` section.
    pub fn start_finally(&mut self) {
        if self.finally.is_none() {
            self.finally = Some(OutputList::default());
        }
        self.section = TrySection::Finally;
    }
    /// Set `onWhen` on the current catch clause. Returns false if not in a catch.
    pub fn set_on_when(&mut self, p: ExpressionDefinition) -> bool {
        if let TrySection::Catch(i) = self.section {
            self.catches[i].on_when = Some(p);
            true
        } else {
            false
        }
    }
}

impl ProcessorDefinition for DoTryDefinition {
    fn name(&self) -> &str {
        "doTry"
    }
    fn is_block(&self) -> bool {
        true
    }
    fn add_output(&mut self, def: Box<dyn ProcessorDefinition>) -> Result<()> {
        match self.section {
            TrySection::Try => self.try_outputs.push(def),
            TrySection::Catch(i) => self.catches[i].outputs.push(def),
            TrySection::Finally => self.finally.get_or_insert_with(OutputList::default).push(def),
        }
        Ok(())
    }
    fn outputs(&self) -> Vec<&dyn ProcessorDefinition> {
        let mut v = self.try_outputs.refs();
        for c in &self.catches {
            v.extend(c.outputs.refs());
        }
        if let Some(f) = &self.finally {
            v.extend(f.refs());
        }
        v
    }
    fn label(&self) -> String {
        let catches: Vec<String> = self.catches.iter().map(|c| format!("doCatch[{}]", c.exceptions.join(","))).collect();
        format!("doTry[{}{}]", catches.join(","), if self.finally.is_some() { ",doFinally" } else { "" })
    }
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        let child = |o: &OutputList| -> Result<Arc<dyn Processor>> {
            if o.outputs.is_empty() {
                Ok(Arc::new(Pipeline::new(vec![])))
            } else {
                rc.create_child_processor(&o.outputs)
            }
        };
        let mut catches = Vec::new();
        for c in &self.catches {
            catches.push(CatchRuntime { exceptions: c.exceptions.clone(), on_when: resolve_opt_pred(&c.on_when, &rc.context)?, processor: child(&c.outputs)? });
        }
        Ok(Arc::new(TryProcessor {
            try_processor: child(&self.try_outputs)?,
            catches,
            finally: self.finally.as_ref().map(child).transpose()?,
        }))
    }
    impl_definition_boilerplate!();
}

struct CatchRuntime {
    exceptions: Vec<String>,
    on_when: Option<Arc<dyn Predicate>>,
    processor: Arc<dyn Processor>,
}

impl CatchRuntime {
    fn catches(&self, err: &CamelError) -> bool {
        self.exceptions.is_empty() || err.chain().any(|e| self.exceptions.iter().any(|k| kind_is_a(&e.kind, k)))
    }
}

struct TryProcessor {
    try_processor: Arc<dyn Processor>,
    catches: Vec<CatchRuntime>,
    finally: Option<Arc<dyn Processor>>,
}

impl Processor for TryProcessor {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        enter_suppressed(ex);
        process_safely(self.try_processor.as_ref(), ex);
        exit_suppressed(ex);
        if let Some(err) = ex.exception.clone() {
            for c in &self.catches {
                if !c.catches(&err) {
                    continue;
                }
                // onWhen is evaluated with the caught exception available
                let saved_caught = ex.property(props::EXCEPTION_CAUGHT).cloned();
                set_caught_exception(ex, &err);
                if !matches_opt(&c.on_when, ex) {
                    match saved_caught {
                        Some(v) => ex.set_property(props::EXCEPTION_CAUGHT, v),
                        None => {
                            ex.remove_property(props::EXCEPTION_CAUGHT);
                        }
                    }
                    continue;
                }
                ex.exception = None;
                ex.remove_property(props::REDELIVERY_EXHAUSTED);
                process_safely(c.processor.as_ref(), ex);
                break;
            }
        }
        if let Some(f) = &self.finally {
            let saved = ex.exception.take();
            let saved_stop = ex.is_route_stop();
            ex.set_route_stop(false);
            let saved_rollback = std::mem::replace(&mut ex.rollback_only, false);
            process_safely(f.as_ref(), ex);
            if ex.exception.is_none() {
                ex.exception = saved;
            }
            if saved_stop {
                ex.set_route_stop(true);
            } else if !ex.is_route_stop() {
                ex.remove_property(props::ROUTE_STOP);
            }
            ex.rollback_only = ex.rollback_only || saved_rollback;
        }
        Ok(())
    }
}

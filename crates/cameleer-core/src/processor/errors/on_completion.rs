//! `onCompletion`: run a sub-route when the exchange completes (successfully or failed).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::builder::ExpressionDefinition;
use crate::error::Result;
use crate::impl_definition_boilerplate;
use crate::message::{props, Exchange};
use crate::model::{OutputList, ProcessorDefinition, RouteContext};
use crate::processor::pipeline::process_safely;
use crate::spi::{Predicate, Processor};
use crate::value::Value;

use super::{matches_opt, resolve_opt_pred, set_caught_exception, ON_COMPLETION_REGISTERED};

/// Which outcome triggers the onCompletion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum OnCompletionMode {
    #[default]
    Always,
    OnCompleteOnly,
    OnFailureOnly,
}

static OC_IDS: AtomicUsize = AtomicUsize::new(1);

/// An `onCompletion` block.
pub struct OnCompletionDefinition {
    pub id: Option<String>,
    /// Unique id used to make sure a (builder-scoped) onCompletion runs only once per exchange.
    pub uid: usize,
    pub mode: OnCompletionMode,
    pub on_when: Option<ExpressionDefinition>,
    /// Run before the consumer sends the response (changes are visible to the caller).
    pub before_consumer: bool,
    pub outputs: OutputList,
}

impl Default for OnCompletionDefinition {
    fn default() -> Self {
        OnCompletionDefinition {
            id: None,
            uid: OC_IDS.fetch_add(1, Ordering::Relaxed),
            mode: OnCompletionMode::Always,
            on_when: None,
            before_consumer: false,
            outputs: OutputList::default(),
        }
    }
}

impl OnCompletionDefinition {
    pub(crate) fn create_runtime(&self, rc: &RouteContext) -> Result<OnCompletionRuntime> {
        let processor = if self.outputs.outputs.is_empty() {
            Arc::new(crate::processor::pipeline::Pipeline::new(vec![])) as Arc<dyn Processor>
        } else {
            rc.create_child_processor(&self.outputs.outputs)?
        };
        Ok(OnCompletionRuntime {
            uid: self.uid,
            mode: self.mode,
            on_when: resolve_opt_pred(&self.on_when, &rc.context)?,
            before_consumer: self.before_consumer,
            processor,
        })
    }
}

impl ProcessorDefinition for OnCompletionDefinition {
    fn name(&self) -> &str {
        "onCompletion"
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

/// Runtime onCompletion.
pub(crate) struct OnCompletionRuntime {
    pub uid: usize,
    pub mode: OnCompletionMode,
    pub on_when: Option<Arc<dyn Predicate>>,
    pub before_consumer: bool,
    pub processor: Arc<dyn Processor>,
}

impl OnCompletionRuntime {
    fn mode_matches(&self, failed: bool) -> bool {
        match self.mode {
            OnCompletionMode::Always => true,
            OnCompletionMode::OnCompleteOnly => !failed,
            OnCompletionMode::OnFailureOnly => failed,
        }
    }

    /// Mark this onCompletion as registered on the exchange; false if it already was.
    pub(crate) fn mark_registered(&self, ex: &mut Exchange) -> bool {
        let mut ids = match ex.property(ON_COMPLETION_REGISTERED) {
            Some(Value::Array(a)) => a.clone(),
            _ => Vec::new(),
        };
        let me = Value::from(self.uid as i64);
        if ids.contains(&me) {
            return false;
        }
        ids.push(me);
        ex.set_property(ON_COMPLETION_REGISTERED, Value::Array(ids));
        true
    }

    /// Run on a copy of the completed exchange (AfterConsumer mode).
    pub(crate) fn run_on_copy(&self, ex: &Exchange) {
        let failed = ex.is_failed();
        if !self.mode_matches(failed) {
            return;
        }
        let mut copy = ex.correlated_copy();
        prepare(&mut copy);
        if !matches_opt(&self.on_when, &copy) {
            return;
        }
        process_safely(self.processor.as_ref(), &mut copy);
        if let Some(e) = &copy.exception {
            crate::camel_warn!("OnCompletion", "Error processing onCompletion for exchange {}: {}", ex.exchange_id, e);
        }
    }

    /// Run on the exchange itself (BeforeConsumer mode): changes are visible to the caller.
    pub(crate) fn run_in_place(&self, ex: &mut Exchange) {
        let failed = ex.is_failed();
        if !self.mode_matches(failed) {
            return;
        }
        let saved_ex = ex.exception.take();
        if let Some(e) = &saved_ex {
            set_caught_exception(ex, e);
        }
        let saved_stop = ex.is_route_stop();
        ex.set_route_stop(false);
        ex.set_property(props::ON_COMPLETION, true);
        if matches_opt(&self.on_when, ex) {
            process_safely(self.processor.as_ref(), ex);
            if let Some(e) = ex.exception.take() {
                crate::camel_warn!("OnCompletion", "Error processing onCompletion for exchange {}: {}", ex.exchange_id, e);
            }
        }
        ex.remove_property(props::ON_COMPLETION);
        if saved_stop {
            ex.set_route_stop(true);
        } else {
            ex.remove_property(props::ROUTE_STOP);
        }
        ex.exception = saved_ex;
    }
}

fn prepare(copy: &mut Exchange) {
    if let Some(e) = copy.exception.take() {
        set_caught_exception(copy, &e);
    }
    copy.remove_property(props::ROUTE_STOP);
    copy.remove_property(props::REDELIVERY_EXHAUSTED);
    copy.rollback_only = false;
    copy.set_property(props::ON_COMPLETION, true);
}

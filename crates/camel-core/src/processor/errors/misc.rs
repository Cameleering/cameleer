//! `validate`, `rollback`/`markRollbackOnly`, `throwException`, `threads`, `transacted`, `policy`.

use std::sync::{Arc, Mutex};

use crate::builder::ExpressionDefinition;
use crate::error::{kinds, CamelError, Result};
use crate::executor::ThreadPool;
use crate::impl_definition_boilerplate;
use crate::message::Exchange;
use crate::model::{OutputList, ProcessorDefinition, RouteContext};
use crate::processor::pipeline::{process_safely, Pipeline};
use crate::spi::{Processor, Service};

use super::ROLLBACK_ONLY_LAST;

fn child_of(rc: &RouteContext, o: &OutputList) -> Result<Arc<dyn Processor>> {
    if o.outputs.is_empty() {
        Ok(Arc::new(Pipeline::new(vec![])))
    } else {
        rc.create_child_processor(&o.outputs)
    }
}

// ---------------------------------------------------------------------------
// validate
// ---------------------------------------------------------------------------

/// Validates the exchange with a predicate; fails with `PredicateValidationException`.
pub struct ValidateDefinition {
    pub id: Option<String>,
    pub predicate: ExpressionDefinition,
}

impl ProcessorDefinition for ValidateDefinition {
    fn name(&self) -> &str {
        "validate"
    }
    fn label(&self) -> String {
        format!("validate[{:?}]", self.predicate)
    }
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        let p = self.predicate.resolve_predicate(&rc.context)?;
        let text = format!("{:?}", self.predicate);
        Ok(Arc::new(move |ex: &mut Exchange| {
            if p.matches(ex)? {
                Ok(())
            } else {
                Err(CamelError::new(
                    kinds::PREDICATE_VALIDATION,
                    format!("Validation failed for Predicate[{}]. Exchange[{}]", text, ex.exchange_id),
                ))
            }
        }))
    }
    impl_definition_boilerplate!();
}

// ---------------------------------------------------------------------------
// rollback
// ---------------------------------------------------------------------------

/// Rollback modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RollbackMode {
    /// Fail with `RollbackExchangeException`.
    Throw,
    /// Mark the exchange rollback-only (stops routing, no exception).
    MarkRollbackOnly,
    /// Mark only the last (innermost) transaction for rollback (stops routing).
    MarkRollbackOnlyLast,
}

pub struct RollbackDefinition {
    pub id: Option<String>,
    pub message: Option<String>,
    pub mode: RollbackMode,
}

impl ProcessorDefinition for RollbackDefinition {
    fn name(&self) -> &str {
        "rollback"
    }
    fn create_processor(&self, _rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        let mode = self.mode;
        let msg = self.message.clone();
        Ok(Arc::new(move |ex: &mut Exchange| {
            match mode {
                RollbackMode::MarkRollbackOnly => {
                    ex.rollback_only = true;
                }
                RollbackMode::MarkRollbackOnlyLast => {
                    ex.set_property(ROLLBACK_ONLY_LAST, true);
                    ex.set_route_stop(true);
                }
                RollbackMode::Throw => {
                    let m = msg.clone().unwrap_or_else(|| "Intended rollback".to_string());
                    return Err(CamelError::new(kinds::ROLLBACK_EXCHANGE, m));
                }
            }
            Ok(())
        }))
    }
    impl_definition_boilerplate!();
}

// ---------------------------------------------------------------------------
// throwException
// ---------------------------------------------------------------------------

/// Fails the exchange with an exception of the given kind. The message is a Simple
/// expression.
pub struct ThrowExceptionDefinition {
    pub id: Option<String>,
    pub kind: String,
    pub message: String,
}

impl ProcessorDefinition for ThrowExceptionDefinition {
    fn name(&self) -> &str {
        "throwException"
    }
    fn label(&self) -> String {
        format!("throwException[{}, {}]", self.kind, self.message)
    }
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        let msg = if self.message.contains("${") {
            Some(ExpressionDefinition::language("simple", self.message.clone()).resolve_expression(&rc.context)?)
        } else {
            None
        };
        let literal = self.message.clone();
        let kind = self.kind.clone();
        Ok(Arc::new(move |ex: &mut Exchange| {
            let m = match &msg {
                Some(e) => e.evaluate(ex)?.to_string(),
                None => literal.clone(),
            };
            Err(CamelError::new(kind.clone(), m))
        }))
    }
    impl_definition_boilerplate!();
}

// ---------------------------------------------------------------------------
// threads
// ---------------------------------------------------------------------------

/// Continue routing on a thread pool. InOut exchanges wait for the result; InOnly
/// exchanges return immediately to the caller.
pub struct ThreadsDefinition {
    pub id: Option<String>,
    pub pool_size: usize,
    pub thread_name: String,
    pub outputs: OutputList,
}

struct PoolService {
    name: String,
    size: usize,
    pool: Mutex<Option<Arc<ThreadPool>>>,
}

impl PoolService {
    fn get(&self) -> Arc<ThreadPool> {
        let mut p = self.pool.lock().unwrap();
        match p.as_ref() {
            Some(pool) if !pool.is_shutdown() => pool.clone(),
            _ => {
                let pool = ThreadPool::new(&self.name, self.size);
                *p = Some(pool.clone());
                pool
            }
        }
    }
}

impl Service for PoolService {
    fn stop(&self) -> Result<()> {
        if let Some(p) = self.pool.lock().unwrap().take() {
            p.shutdown();
        }
        Ok(())
    }
}

impl ProcessorDefinition for ThreadsDefinition {
    fn name(&self) -> &str {
        "threads"
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
        let child = child_of(rc, &self.outputs)?;
        let svc = Arc::new(PoolService { name: self.thread_name.clone(), size: self.pool_size.max(1), pool: Mutex::new(None) });
        rc.add_service(svc.clone());
        Ok(Arc::new(move |ex: &mut Exchange| {
            let pool = svc.get();
            let mut copy = ex.clone();
            let child = child.clone();
            if ex.is_out_capable() {
                let fut = pool.submit(move || {
                    process_safely(child.as_ref(), &mut copy);
                    copy
                });
                match fut.get() {
                    Some(done) => {
                        ex.message = done.message;
                        ex.properties = done.properties;
                        ex.exception = done.exception;
                        ex.rollback_only = done.rollback_only;
                        Ok(())
                    }
                    None => Err(CamelError::new(kinds::REJECTED_EXECUTION, "Thread pool rejected the task")),
                }
            } else {
                // the async copy continues on its own unit of work
                let ok = pool.execute(move || {
                    process_safely(child.as_ref(), &mut copy);
                    if let Some(e) = &copy.exception {
                        crate::camel_warn!("Threads", "Error processing exchange {} asynchronously: {}", copy.exchange_id, e);
                    }
                    copy.run_on_completions();
                });
                if ok {
                    Ok(())
                } else {
                    Err(CamelError::new(kinds::REJECTED_EXECUTION, "Thread pool rejected the task"))
                }
            }
        }))
    }
    impl_definition_boilerplate!();
}

// ---------------------------------------------------------------------------
// transacted
// ---------------------------------------------------------------------------

/// `transacted()`: marker only (rust-camel has no transaction managers).
pub struct TransactedDefinition {
    pub id: Option<String>,
    pub reference: Option<String>,
}

impl ProcessorDefinition for TransactedDefinition {
    fn name(&self) -> &str {
        "transacted"
    }
    fn create_processor(&self, rc: &RouteContext) -> Result<Arc<dyn Processor>> {
        let route = rc.route_id.clone();
        let r = self.reference.clone().unwrap_or_else(|| "PROPAGATION_REQUIRED".to_string());
        Ok(Arc::new(move |ex: &mut Exchange| {
            crate::camel_debug!("Transacted", "Exchange {} on route {} is transacted ({}): no transaction manager, no-op", ex.exchange_id, route, r);
            Ok(())
        }))
    }
    impl_definition_boilerplate!();
}

// ---------------------------------------------------------------------------
// policy
// ---------------------------------------------------------------------------

/// A policy wraps the processors of a `policy(...)` block (Camel `Policy`).
pub trait Policy: Send + Sync {
    /// Called before the block's processors are created.
    fn before_wrap(&self, _rc: &RouteContext) -> Result<()> {
        Ok(())
    }
    /// Wrap the block's processor.
    fn wrap(&self, rc: &RouteContext, processor: Arc<dyn Processor>) -> Result<Arc<dyn Processor>>;
}

/// The `policy` block.
pub struct PolicyDefinition {
    pub id: Option<String>,
    pub policy: Option<Arc<dyn Policy>>,
    /// Registry name of an `Arc<dyn Policy>`.
    pub reference: Option<String>,
    pub outputs: OutputList,
}

impl ProcessorDefinition for PolicyDefinition {
    fn name(&self) -> &str {
        "policy"
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
        let policy = match (&self.policy, &self.reference) {
            (Some(p), _) => p.clone(),
            (None, Some(r)) => rc.context.registry().lookup::<Arc<dyn Policy>>(r).map(|p| (*p).clone()).ok_or_else(|| CamelError::no_such_bean(r))?,
            (None, None) => return Err(CamelError::illegal_argument("policy requires a Policy or a ref")),
        };
        policy.before_wrap(rc)?;
        let child = child_of(rc, &self.outputs)?;
        policy.wrap(rc, child)
    }
    impl_definition_boilerplate!();
}

//! `validate`, `rollback`/`markRollbackOnly`, `throwException`, `threads`, `transacted`, `policy`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::builder::ExpressionDefinition;
use crate::error::{kinds, CamelError, Result};
use crate::executor::{BoundedThreadPool, Future, RejectedPolicy, Submission, Task, ThreadPoolProfile};
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

/// Options of the `threads` EIP (Camel `ThreadsDefinition`). Unset options come from
/// Camel's default thread pool profile (poolSize 10, maxPoolSize 20, keepAliveTime 60s,
/// maxQueueSize 1000). The rejection policy is `rejectedPolicy`, else CallerRuns when
/// `callerRunsWhenRejected` (default true), else Abort.
#[derive(Clone, Debug, Default)]
pub struct ThreadsOptions {
    pub pool_size: Option<usize>,
    pub max_pool_size: Option<usize>,
    pub keep_alive_time: Option<Duration>,
    /// `0` = direct hand-off, negative = unbounded.
    pub max_queue_size: Option<i64>,
    pub allow_core_thread_timeout: Option<bool>,
    pub rejected_policy: Option<RejectedPolicy>,
    pub caller_runs_when_rejected: Option<bool>,
    pub thread_name: Option<String>,
    /// Registry name of a shared `Arc<BoundedThreadPool>` or a `ThreadPoolProfile`.
    pub executor_service: Option<String>,
}

impl ThreadsOptions {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn pool_size(mut self, n: usize) -> Self {
        self.pool_size = Some(n);
        self
    }
    pub fn max_pool_size(mut self, n: usize) -> Self {
        self.max_pool_size = Some(n);
        self
    }
    pub fn keep_alive_time(mut self, d: Duration) -> Self {
        self.keep_alive_time = Some(d);
        self
    }
    pub fn max_queue_size(mut self, n: i64) -> Self {
        self.max_queue_size = Some(n);
        self
    }
    pub fn allow_core_thread_timeout(mut self, b: bool) -> Self {
        self.allow_core_thread_timeout = Some(b);
        self
    }
    pub fn rejected_policy(mut self, p: RejectedPolicy) -> Self {
        self.rejected_policy = Some(p);
        self
    }
    pub fn caller_runs_when_rejected(mut self, b: bool) -> Self {
        self.caller_runs_when_rejected = Some(b);
        self
    }
    pub fn thread_name(mut self, n: impl Into<String>) -> Self {
        self.thread_name = Some(n.into());
        self
    }
    pub fn executor_service(mut self, r: impl Into<String>) -> Self {
        self.executor_service = Some(r.into());
        self
    }

    /// The thread pool profile these options describe.
    pub fn profile(&self) -> ThreadPoolProfile {
        let d = ThreadPoolProfile::default();
        let pool_size = self.pool_size.unwrap_or(d.pool_size).max(1);
        let max_pool_size = self.max_pool_size.unwrap_or(d.max_pool_size).max(pool_size);
        let rejected_policy = match (self.rejected_policy, self.caller_runs_when_rejected.unwrap_or(true)) {
            (Some(p), _) => p,
            (None, true) => RejectedPolicy::CallerRuns,
            (None, false) => RejectedPolicy::Abort,
        };
        ThreadPoolProfile {
            name: self.thread_name.clone().unwrap_or_else(|| "Threads".into()),
            pool_size,
            max_pool_size,
            keep_alive: self.keep_alive_time.unwrap_or(d.keep_alive),
            max_queue_size: self.max_queue_size.unwrap_or(d.max_queue_size),
            allow_core_thread_timeout: self.allow_core_thread_timeout.unwrap_or(false),
            rejected_policy,
        }
    }
}

/// Continue routing on a bounded thread pool. InOut exchanges wait for the result; InOnly
/// exchanges return immediately to the caller (unless the task ran in the caller thread
/// because of CallerRuns). Rejected tasks fail with `RejectedExecutionException` (Abort, or
/// a discarded InOut exchange); discarded InOnly exchanges are dropped with a warning.
pub struct ThreadsDefinition {
    pub id: Option<String>,
    pub options: ThreadsOptions,
    pub outputs: OutputList,
}

impl ThreadsDefinition {
    pub fn new(options: ThreadsOptions) -> Self {
        ThreadsDefinition { id: None, options, outputs: Default::default() }
    }
}

struct PoolService {
    profile: ThreadPoolProfile,
    shared: Option<Arc<BoundedThreadPool>>,
    pool: Mutex<Option<Arc<BoundedThreadPool>>>,
}

impl PoolService {
    fn get(&self) -> Arc<BoundedThreadPool> {
        if let Some(p) = &self.shared {
            return p.clone();
        }
        let mut p = self.pool.lock().unwrap();
        match p.as_ref() {
            Some(pool) if !pool.is_shutdown() => pool.clone(),
            _ => {
                let pool = BoundedThreadPool::new(self.profile.clone());
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

fn rejected() -> CamelError {
    CamelError::new(kinds::REJECTED_EXECUTION, "Thread pool rejected the task")
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
        let mut profile = self.options.profile();
        let mut shared = None;
        if let Some(r) = &self.options.executor_service {
            let reg = rc.context.registry();
            if let Some(p) = reg.lookup::<Arc<BoundedThreadPool>>(r) {
                shared = Some((*p).clone());
            } else if let Some(p) = reg.lookup::<ThreadPoolProfile>(r) {
                profile = (*p).clone();
            } else {
                return Err(CamelError::no_such_bean(r));
            }
        }
        let svc = Arc::new(PoolService { profile, shared, pool: Mutex::new(None) });
        rc.add_service(svc.clone());
        Ok(Arc::new(move |ex: &mut Exchange| {
            let pool = svc.get();
            let mut copy = ex.clone();
            let child = child.clone();
            if ex.is_out_capable() {
                let fut: Future<Exchange> = Future::new();
                let (f1, f2) = (fut.clone(), fut.clone());
                let task = Task::new(move || {
                    process_safely(child.as_ref(), &mut copy);
                    f1.complete(copy);
                })
                .on_reject(move || f2.cancel());
                match pool.submit_task(task) {
                    Submission::Rejected(_) => return Err(rejected()),
                    Submission::Accepted | Submission::RanInCaller | Submission::Discarded => {}
                }
                match fut.get() {
                    Some(done) => {
                        ex.message = done.message;
                        ex.properties = done.properties;
                        ex.exception = done.exception;
                        ex.rollback_only = done.rollback_only;
                        Ok(())
                    }
                    None => Err(rejected()),
                }
            } else {
                // the async copy continues on its own unit of work
                let id = ex.exchange_id.clone();
                let task = Task::new(move || {
                    process_safely(child.as_ref(), &mut copy);
                    if let Some(e) = &copy.exception {
                        crate::camel_warn!("Threads", "Error processing exchange {} asynchronously: {}", copy.exchange_id, e);
                    }
                    copy.run_on_completions();
                })
                .on_reject(move || crate::camel_warn!("Threads", "Exchange {} discarded by the thread pool (rejected)", id));
                match pool.submit_task(task) {
                    Submission::Rejected(_) => Err(rejected()),
                    _ => Ok(()),
                }
            }
        }))
    }
    impl_definition_boilerplate!();
}

// ---------------------------------------------------------------------------
// transacted
// ---------------------------------------------------------------------------

/// `transacted()`: runs the rest of the route (or its `steps`) in a transaction of a
/// [`TransactionPolicy`](crate::transaction::TransactionPolicy) — see
/// [`crate::transaction`] for how the policy and transaction manager are resolved.
pub struct TransactedDefinition {
    pub id: Option<String>,
    pub reference: Option<String>,
    pub policy: Option<Arc<crate::transaction::TransactionPolicy>>,
    pub outputs: OutputList,
}

impl TransactedDefinition {
    pub fn new(reference: Option<String>) -> Self {
        TransactedDefinition { id: None, reference, policy: None, outputs: Default::default() }
    }
}

impl ProcessorDefinition for TransactedDefinition {
    fn name(&self) -> &str {
        "transacted"
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
        let policy = match &self.policy {
            Some(p) => p.clone(),
            None => crate::transaction::resolve_policy(&rc.context, self.reference.as_deref())?,
        };
        let child = child_of(rc, &self.outputs)?;
        Ok(crate::transaction::transacted_processor(policy, rc.route_id.clone(), child))
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
            (None, Some(r)) => {
                let reg = rc.context.registry();
                match reg.lookup::<Arc<dyn Policy>>(r) {
                    Some(p) => (*p).clone(),
                    // a TransactionPolicy bean (Camel SpringTransactionPolicy used as a policy)
                    None => match reg.lookup::<crate::transaction::TransactionPolicy>(r) {
                        Some(p) => p as Arc<dyn Policy>,
                        None => return Err(CamelError::no_such_bean(r)),
                    },
                }
            }
            (None, None) => return Err(CamelError::illegal_argument("policy requires a Policy or a ref")),
        };
        policy.before_wrap(rc)?;
        let child = child_of(rc, &self.outputs)?;
        policy.wrap(rc, child)
    }
    impl_definition_boilerplate!();
}

//! Transactions (Camel's `transacted()` with a Spring-style `PlatformTransactionManager`).
//!
//! - [`TransactionManager`]: the SPI a resource implements (`begin`/`commit`/`rollback`,
//!   optional savepoints for `NESTED`, `suspend`/`resume` hooks). [`FnTransactionManager`]
//!   builds one from closures.
//! - [`TransactionPolicy`]: a manager plus a [`TransactionDefinition`] (propagation,
//!   isolation, timeout, read-only). [`TransactionPolicy::execute`] is the
//!   `TransactionTemplate` equivalent; the policy is also a [`Policy`] usable with
//!   `.policy(...)`.
//! - `transacted()` / `transacted_ref(name)` (YAML `transacted`, `transacted: {ref: x}`) runs
//!   the rest of the route in a transaction. The policy is resolved as in Camel: a
//!   `TransactionPolicy` bean with that name (default `PROPAGATION_REQUIRED`); a name such
//!   as `PROPAGATION_REQUIRES_NEW` builds a policy with that propagation; otherwise the
//!   only `TransactionPolicy` in the registry; otherwise a `TransactionManager` bean (named
//!   `transactionManager`, or the only one) with `PROPAGATION_REQUIRED`.
//!
//! Transactions are bound to the current thread (as in Spring): routes called synchronously
//! from a transacted route (`direct:`, `bean`, ...) participate; `seda:`/`threads` hand-offs
//! do not. Resources find the active transaction with [`current_transaction`] and can
//! register [`TransactionSynchronization`] callbacks.
//!
//! Propagation: `REQUIRED` (join or begin), `REQUIRES_NEW` (suspend the current one and begin
//! a new one), `MANDATORY` (join, fail without one), `SUPPORTS` (join or run without),
//! `NOT_SUPPORTED` (suspend and run without), `NEVER` (fail when one exists), `NESTED`
//! (savepoint in the current one, or begin).
//!
//! Outcome of a transacted scope: an exception on the exchange or `markRollbackOnly()` rolls
//! the transaction back (the exception stays on the exchange); a participating scope marks
//! the shared transaction rollback-only instead. `markRollbackOnlyLast()` rolls back only the
//! innermost transaction that the scope began, and the outer route continues. Otherwise the
//! transaction is committed (a commit failure becomes the exchange exception); a transaction
//! that exceeded its timeout is rolled back with `TransactionTimedOutException`.

use std::any::Any;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::context::CamelContext;
use crate::error::{kinds, CamelError, Result};
use crate::message::Exchange;
use crate::model::RouteContext;
use crate::processor::errors::{Policy, ROLLBACK_ONLY_LAST};
use crate::spi::Processor;

/// Error kinds raised by the transaction support.
pub mod tx_kinds {
    pub const ILLEGAL_TRANSACTION_STATE: &str = "IllegalTransactionStateException";
    pub const NESTED_NOT_SUPPORTED: &str = "NestedTransactionNotSupportedException";
    pub const TIMED_OUT: &str = "TransactionTimedOutException";
    pub const UNEXPECTED_ROLLBACK: &str = "UnexpectedRollbackException";
    pub const TRANSACTION_SYSTEM: &str = "TransactionSystemException";
}

/// Transaction propagation behaviour (Spring `Propagation`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Propagation {
    Required,
    RequiresNew,
    Mandatory,
    Supports,
    NotSupported,
    Never,
    Nested,
}

impl Propagation {
    /// Parse `PROPAGATION_REQUIRES_NEW`, `REQUIRES_NEW`, `requiresNew`, ...
    pub fn parse(s: &str) -> Option<Propagation> {
        let n = s.trim().to_ascii_lowercase().replace(['_', '-'], "");
        let n = n.strip_prefix("propagation").unwrap_or(&n);
        Some(match n {
            "required" => Propagation::Required,
            "requiresnew" => Propagation::RequiresNew,
            "mandatory" => Propagation::Mandatory,
            "supports" => Propagation::Supports,
            "notsupported" => Propagation::NotSupported,
            "never" => Propagation::Never,
            "nested" => Propagation::Nested,
            _ => return None,
        })
    }
    /// The Spring constant name (`PROPAGATION_REQUIRED`, ...).
    pub fn constant(&self) -> &'static str {
        match self {
            Propagation::Required => "PROPAGATION_REQUIRED",
            Propagation::RequiresNew => "PROPAGATION_REQUIRES_NEW",
            Propagation::Mandatory => "PROPAGATION_MANDATORY",
            Propagation::Supports => "PROPAGATION_SUPPORTS",
            Propagation::NotSupported => "PROPAGATION_NOT_SUPPORTED",
            Propagation::Never => "PROPAGATION_NEVER",
            Propagation::Nested => "PROPAGATION_NESTED",
        }
    }
}

/// Isolation level hint passed to the manager.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Isolation {
    #[default]
    Default,
    ReadUncommitted,
    ReadCommitted,
    RepeatableRead,
    Serializable,
}

/// Attributes of a transaction (Spring `TransactionDefinition`).
#[derive(Clone, Debug)]
pub struct TransactionDefinition {
    pub name: String,
    pub propagation: Propagation,
    pub isolation: Isolation,
    pub timeout: Option<Duration>,
    pub read_only: bool,
}

impl Default for TransactionDefinition {
    fn default() -> Self {
        TransactionDefinition { name: String::new(), propagation: Propagation::Required, isolation: Isolation::Default, timeout: None, read_only: false }
    }
}

/// The resource state a manager keeps for one physical transaction (or savepoint).
pub type TxResource = Box<dyn Any + Send + Sync>;

/// The transaction manager SPI (Spring `PlatformTransactionManager`, reduced to the
/// resource-level operations; propagation is handled by [`TransactionPolicy`]).
pub trait TransactionManager: Send + Sync {
    /// Begin a physical transaction and return its resource state.
    fn begin(&self, definition: &TransactionDefinition) -> Result<TxResource>;
    fn commit(&self, tx: &TxResource) -> Result<()>;
    fn rollback(&self, tx: &TxResource) -> Result<()>;
    /// Create a savepoint in `tx` (for `NESTED`).
    fn create_savepoint(&self, _tx: &TxResource) -> Result<TxResource> {
        Err(CamelError::new(tx_kinds::NESTED_NOT_SUPPORTED, "Transaction manager does not support savepoints"))
    }
    fn rollback_to_savepoint(&self, _tx: &TxResource, _savepoint: &TxResource) -> Result<()> {
        Err(CamelError::new(tx_kinds::NESTED_NOT_SUPPORTED, "Transaction manager does not support savepoints"))
    }
    fn release_savepoint(&self, _tx: &TxResource, _savepoint: &TxResource) -> Result<()> {
        Ok(())
    }
    /// Called when `tx` is suspended by `REQUIRES_NEW`/`NOT_SUPPORTED`, and resumed after.
    fn suspend(&self, _tx: &TxResource) -> Result<()> {
        Ok(())
    }
    fn resume(&self, _tx: &TxResource) -> Result<()> {
        Ok(())
    }
}

type BeginFn = dyn Fn(&TransactionDefinition) -> Result<TxResource> + Send + Sync;
type TxFn = dyn Fn(&TxResource) -> Result<()> + Send + Sync;

/// A [`TransactionManager`] built from closures.
pub struct FnTransactionManager {
    begin: Box<BeginFn>,
    commit: Box<TxFn>,
    rollback: Box<TxFn>,
}

impl FnTransactionManager {
    pub fn new(
        begin: impl Fn(&TransactionDefinition) -> Result<TxResource> + Send + Sync + 'static,
        commit: impl Fn(&TxResource) -> Result<()> + Send + Sync + 'static,
        rollback: impl Fn(&TxResource) -> Result<()> + Send + Sync + 'static,
    ) -> Self {
        FnTransactionManager { begin: Box::new(begin), commit: Box::new(commit), rollback: Box::new(rollback) }
    }
}

impl TransactionManager for FnTransactionManager {
    fn begin(&self, d: &TransactionDefinition) -> Result<TxResource> {
        (self.begin)(d)
    }
    fn commit(&self, tx: &TxResource) -> Result<()> {
        (self.commit)(tx)
    }
    fn rollback(&self, tx: &TxResource) -> Result<()> {
        (self.rollback)(tx)
    }
}

/// Final status passed to [`TransactionSynchronization::after_completion`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionStatus {
    Committed,
    RolledBack,
}

/// Callbacks registered on a transaction by a resource (Spring `TransactionSynchronization`).
pub trait TransactionSynchronization: Send + Sync {
    /// Before commit; an error rolls the transaction back.
    fn before_commit(&self) -> Result<()> {
        Ok(())
    }
    fn after_completion(&self, _status: CompletionStatus) {}
}

static TX_IDS: AtomicU64 = AtomicU64::new(1);

/// An active physical transaction.
pub struct Transaction {
    id: u64,
    definition: TransactionDefinition,
    manager: Arc<dyn TransactionManager>,
    resource: TxResource,
    rollback_only: AtomicBool,
    started: Instant,
    synchronizations: Mutex<Vec<Arc<dyn TransactionSynchronization>>>,
}

impl Transaction {
    pub fn id(&self) -> u64 {
        self.id
    }
    pub fn definition(&self) -> &TransactionDefinition {
        &self.definition
    }
    pub fn manager(&self) -> &Arc<dyn TransactionManager> {
        &self.manager
    }
    /// The manager's resource state, downcast to `T`.
    pub fn resource<T: Any>(&self) -> Option<&T> {
        self.resource.downcast_ref::<T>()
    }
    pub fn set_rollback_only(&self) {
        self.rollback_only.store(true, Ordering::SeqCst);
    }
    pub fn is_rollback_only(&self) -> bool {
        self.rollback_only.load(Ordering::SeqCst)
    }
    pub fn register_synchronization(&self, s: Arc<dyn TransactionSynchronization>) {
        self.synchronizations.lock().unwrap().push(s);
    }
    fn timed_out(&self) -> bool {
        self.definition.timeout.map(|t| self.started.elapsed() > t).unwrap_or(false)
    }
    fn syncs(&self) -> Vec<Arc<dyn TransactionSynchronization>> {
        self.synchronizations.lock().unwrap().clone()
    }
}

thread_local! {
    /// Transaction scopes of this thread; `None` = a scope running without transaction
    /// (`NOT_SUPPORTED`), hiding the outer one.
    static STACK: RefCell<Vec<Option<Arc<Transaction>>>> = const { RefCell::new(Vec::new()) };
}

/// The transaction bound to the current thread, if any.
pub fn current_transaction() -> Option<Arc<Transaction>> {
    STACK.with(|s| s.borrow().last().cloned().flatten())
}

/// Whether a transaction is active on the current thread.
pub fn is_transaction_active() -> bool {
    current_transaction().is_some()
}

struct ScopeGuard;
impl ScopeGuard {
    fn push(tx: Option<Arc<Transaction>>) -> ScopeGuard {
        STACK.with(|s| s.borrow_mut().push(tx));
        ScopeGuard
    }
}
impl Drop for ScopeGuard {
    fn drop(&mut self) {
        STACK.with(|s| {
            s.borrow_mut().pop();
        });
    }
}

/// How a unit of work ended, as reported to [`TransactionPolicy::run`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Commit,
    /// Roll back (or mark the joined transaction rollback-only).
    Rollback,
    /// Roll back only a transaction this scope began (`markRollbackOnlyLast`).
    RollbackLast,
}

/// A transaction manager with transaction attributes (Camel `SpringTransactionPolicy`).
#[derive(Clone)]
pub struct TransactionPolicy {
    pub manager: Arc<dyn TransactionManager>,
    pub definition: TransactionDefinition,
}

impl TransactionPolicy {
    pub fn new(manager: Arc<dyn TransactionManager>, propagation: Propagation) -> Self {
        TransactionPolicy { manager, definition: TransactionDefinition { name: propagation.constant().into(), propagation, ..Default::default() } }
    }
    pub fn required(manager: Arc<dyn TransactionManager>) -> Self {
        Self::new(manager, Propagation::Required)
    }
    pub fn with_timeout(mut self, t: Duration) -> Self {
        self.definition.timeout = Some(t);
        self
    }
    pub fn with_isolation(mut self, i: Isolation) -> Self {
        self.definition.isolation = i;
        self
    }
    pub fn read_only(mut self, b: bool) -> Self {
        self.definition.read_only = b;
        self
    }
    pub fn propagation(&self) -> Propagation {
        self.definition.propagation
    }

    /// Run `f` within this policy's transaction scope (Spring `TransactionTemplate`): `f`
    /// returns its value and the [`Outcome`] deciding commit or rollback. Errors are those
    /// of the transaction infrastructure (begin/commit/propagation), not of `f`.
    pub fn run<T>(&self, f: impl FnOnce() -> (T, Outcome)) -> Result<T> {
        let existing = current_transaction();
        let d = &self.definition;
        match (d.propagation, &existing) {
            (Propagation::Required | Propagation::Supports | Propagation::Mandatory, Some(tx)) => Ok(participate(tx, f)),
            (Propagation::Mandatory, None) => Err(CamelError::new(
                tx_kinds::ILLEGAL_TRANSACTION_STATE,
                "No existing transaction found for transaction marked with propagation 'mandatory'",
            )),
            (Propagation::Never, Some(_)) => Err(CamelError::new(
                tx_kinds::ILLEGAL_TRANSACTION_STATE,
                "Existing transaction found for transaction marked with propagation 'never'",
            )),
            (Propagation::Supports | Propagation::Never | Propagation::NotSupported, None) => Ok(f().0),
            (Propagation::NotSupported, Some(tx)) => {
                tx.manager.suspend(&tx.resource)?;
                let v = {
                    let _g = ScopeGuard::push(None);
                    f().0
                };
                tx.manager.resume(&tx.resource)?;
                Ok(v)
            }
            (Propagation::RequiresNew, Some(tx)) => {
                tx.manager.suspend(&tx.resource)?;
                let r = self.new_transaction(f);
                tx.manager.resume(&tx.resource)?;
                r
            }
            (Propagation::Nested, Some(tx)) => {
                let sp = tx.manager.create_savepoint(&tx.resource)?;
                let (v, outcome) = {
                    let _g = ScopeGuard::push(Some(tx.clone()));
                    f()
                };
                if outcome == Outcome::Commit {
                    tx.manager.release_savepoint(&tx.resource, &sp)?;
                } else {
                    tx.manager.rollback_to_savepoint(&tx.resource, &sp)?;
                }
                Ok(v)
            }
            (Propagation::Required | Propagation::RequiresNew | Propagation::Nested, None) => self.new_transaction(f),
        }
    }

    /// `run` for a fallible unit of work: an `Err` from `f` rolls back and is returned.
    pub fn execute<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        self.run(|| {
            let r = f();
            let o = if r.is_ok() { Outcome::Commit } else { Outcome::Rollback };
            (r, o)
        })?
    }

    fn new_transaction<T>(&self, f: impl FnOnce() -> (T, Outcome)) -> Result<T> {
        let resource = self.manager.begin(&self.definition)?;
        let tx = Arc::new(Transaction {
            id: TX_IDS.fetch_add(1, Ordering::Relaxed),
            definition: self.definition.clone(),
            manager: self.manager.clone(),
            resource,
            rollback_only: AtomicBool::new(false),
            started: Instant::now(),
            synchronizations: Mutex::new(Vec::new()),
        });
        crate::camel_debug!("Transaction", "Begin transaction {} ({})", tx.id, self.definition.name);
        let (v, outcome) = {
            let _g = ScopeGuard::push(Some(tx.clone()));
            f()
        };
        complete(&tx, outcome != Outcome::Commit || tx.is_rollback_only())?;
        Ok(v)
    }
}

fn participate<T>(tx: &Arc<Transaction>, f: impl FnOnce() -> (T, Outcome)) -> T {
    let (v, outcome) = {
        let _g = ScopeGuard::push(Some(tx.clone()));
        f()
    };
    if outcome == Outcome::Rollback {
        tx.set_rollback_only();
    }
    v
}

fn complete(tx: &Arc<Transaction>, rollback: bool) -> Result<()> {
    let syncs = tx.syncs();
    let do_rollback = |reason: Option<CamelError>| -> Result<()> {
        let r = tx.manager.rollback(&tx.resource);
        crate::camel_debug!("Transaction", "Rolled back transaction {}", tx.id);
        for s in &syncs {
            s.after_completion(CompletionStatus::RolledBack);
        }
        match (r, reason) {
            (Err(e), _) => Err(CamelError::new(tx_kinds::TRANSACTION_SYSTEM, format!("Rollback failed: {}", e.message)).with_cause(e)),
            (Ok(()), Some(e)) => Err(e),
            (Ok(()), None) => Ok(()),
        }
    };
    if rollback {
        return do_rollback(None);
    }
    if tx.timed_out() {
        return do_rollback(Some(CamelError::new(
            tx_kinds::TIMED_OUT,
            format!("Transaction timed out: deadline was {:?} after begin", tx.definition.timeout.unwrap_or_default()),
        )));
    }
    for s in &syncs {
        if let Err(e) = s.before_commit() {
            return do_rollback(Some(e));
        }
    }
    match tx.manager.commit(&tx.resource) {
        Ok(()) => {
            crate::camel_debug!("Transaction", "Committed transaction {}", tx.id);
            for s in &syncs {
                s.after_completion(CompletionStatus::Committed);
            }
            Ok(())
        }
        Err(e) => {
            let _ = tx.manager.rollback(&tx.resource);
            for s in &syncs {
                s.after_completion(CompletionStatus::RolledBack);
            }
            Err(e)
        }
    }
}

/// The processor of a transacted scope: runs `child` within `policy`.
pub fn transacted_processor(policy: Arc<TransactionPolicy>, route_id: String, child: Arc<dyn Processor>) -> Arc<dyn Processor> {
    Arc::new(move |ex: &mut Exchange| {
        let propagation = policy.propagation();
        let began_here = matches!(propagation, Propagation::RequiresNew | Propagation::Nested)
            || (matches!(propagation, Propagation::Required) && !is_transaction_active());
        let r = policy.run(|| {
            crate::processor::pipeline::process_safely(child.as_ref(), ex);
            let last = ex.properties.get(ROLLBACK_ONLY_LAST).map(|v| v.is_truthy()).unwrap_or(false);
            let outcome = if ex.exception.is_some() || ex.rollback_only {
                Outcome::Rollback
            } else if last && began_here {
                Outcome::RollbackLast
            } else {
                Outcome::Commit
            };
            ((), outcome)
        });
        let last = ex.properties.get(ROLLBACK_ONLY_LAST).map(|v| v.is_truthy()).unwrap_or(false);
        if last && began_here {
            // only this (innermost) transaction is rolled back: the caller continues
            ex.properties.remove(ROLLBACK_ONLY_LAST);
            ex.set_route_stop(false);
        }
        if let Err(e) = r {
            crate::camel_warn!("Transaction", "Transaction failure on route {} for exchange {}: {}", route_id, ex.exchange_id, e);
            if ex.exception.is_none() {
                ex.set_exception(e);
            }
        }
        Ok(())
    })
}

impl Policy for TransactionPolicy {
    fn wrap(&self, rc: &RouteContext, processor: Arc<dyn Processor>) -> Result<Arc<dyn Processor>> {
        Ok(transacted_processor(Arc::new(self.clone()), rc.route_id.clone(), processor))
    }
}

/// Resolve the policy of `transacted(reference)` (see the module documentation).
pub fn resolve_policy(ctx: &CamelContext, reference: Option<&str>) -> Result<Arc<TransactionPolicy>> {
    let reg = ctx.registry();
    let name = reference.unwrap_or("PROPAGATION_REQUIRED");
    if let Some(p) = reg.lookup::<TransactionPolicy>(name) {
        return Ok(p);
    }
    if let Some(p) = reg.lookup::<Arc<TransactionPolicy>>(name) {
        return Ok((*p).clone());
    }
    let propagation = Propagation::parse(name);
    if reference.is_some() && propagation.is_none() {
        return Err(CamelError::no_such_bean(name));
    }
    if reference.is_none() {
        let policies = reg.find_by_type::<TransactionPolicy>();
        if policies.len() == 1 {
            return Ok(policies[0].1.clone());
        }
    }
    let manager = find_manager(ctx).ok_or_else(|| {
        CamelError::new(
            kinds::NO_SUCH_BEAN,
            format!("No TransactionPolicy or TransactionManager found in the registry for transacted({})", name),
        )
    })?;
    Ok(Arc::new(TransactionPolicy::new(manager, propagation.unwrap_or(Propagation::Required))))
}

/// The transaction manager bean: `transactionManager`, or the only one in the registry.
pub fn find_manager(ctx: &CamelContext) -> Option<Arc<dyn TransactionManager>> {
    let reg = ctx.registry();
    if let Some(m) = reg.lookup::<Arc<dyn TransactionManager>>("transactionManager") {
        return Some((*m).clone());
    }
    let all = reg.find_by_type::<Arc<dyn TransactionManager>>();
    if all.len() == 1 {
        return Some((*all[0].1).clone());
    }
    None
}

impl crate::context::Registry {
    /// Bind a transaction manager (found by `transacted()`).
    pub fn bind_transaction_manager(&self, name: impl Into<String>, m: impl TransactionManager + 'static) {
        let m: Arc<dyn TransactionManager> = Arc::new(m);
        self.bind(name, m);
    }
}

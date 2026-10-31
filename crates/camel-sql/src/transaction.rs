//! Thread-bound JDBC transactions (the equivalent of Spring's `DataSourceTransactionManager`
//! with `PROPAGATION_REQUIRED`).
//!
//! A transaction is bound to the current thread per data source ([`DataSource::key`]). While
//! it is bound, the `sql` / `jdbc` producers and the JDBC repositories use its connection.
//! Wrap route steps in a [`JdbcTransactionPolicy`] (`.policy(...) ... .end()`, or bind it with
//! [`bind_transaction_policy`] and use `.policy_ref(name)`): the block commits when the
//! exchange completes successfully and rolls back when it failed or was marked rollback-only.

use std::cell::RefCell;
use std::sync::Arc;

use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::model::RouteContext;
use camel_core::processor::errors::misc::Policy;
use camel_core::spi::Processor;

use crate::datasource::{Connection, DataSource};

thread_local! {
    static BOUND: RefCell<Vec<(String, Box<dyn Connection>)>> = const { RefCell::new(Vec::new()) };
}

fn take_bound(key: &str) -> Option<(String, Box<dyn Connection>)> {
    BOUND.with(|b| {
        let mut b = b.borrow_mut();
        let i = b.iter().position(|(k, _)| k == key)?;
        Some(b.remove(i))
    })
}

fn put_bound(entry: (String, Box<dyn Connection>)) {
    BOUND.with(|b| b.borrow_mut().push(entry));
}

/// Whether a transaction is bound to the current thread for this data source.
pub fn is_transaction_bound(ds: &Arc<dyn DataSource>) -> bool {
    let key = ds.key();
    BOUND.with(|b| b.borrow().iter().any(|(k, _)| *k == key))
}

/// Run `f` with a connection: the thread-bound transactional connection if any, otherwise
/// a fresh auto-commit connection.
pub fn with_connection<R>(ds: &Arc<dyn DataSource>, f: impl FnOnce(&mut dyn Connection) -> Result<R>) -> Result<R> {
    let key = ds.key();
    match take_bound(&key) {
        Some((k, mut c)) => {
            let r = f(c.as_mut());
            put_bound((k, c));
            r
        }
        None => {
            let mut c = ds.get_connection()?;
            f(c.as_mut())
        }
    }
}

/// A transaction bound to the current thread; rolled back on drop unless committed.
pub struct BoundTransaction {
    key: String,
    done: bool,
}

impl BoundTransaction {
    /// Begin a transaction on a new connection and bind it to the current thread.
    /// Returns `None` if a transaction is already bound for this data source (join it).
    pub fn begin(ds: &Arc<dyn DataSource>) -> Result<Option<BoundTransaction>> {
        if is_transaction_bound(ds) {
            return Ok(None);
        }
        let mut c = ds.get_connection()?;
        c.begin()?;
        let key = ds.key();
        put_bound((key.clone(), c));
        Ok(Some(BoundTransaction { key, done: false }))
    }

    /// Commit and unbind.
    pub fn commit(mut self) -> Result<()> {
        self.done = true;
        match take_bound(&self.key) {
            Some((_, mut c)) => c.commit(),
            None => Ok(()),
        }
    }

    /// Roll back and unbind.
    pub fn rollback(mut self) -> Result<()> {
        self.done = true;
        match take_bound(&self.key) {
            Some((_, mut c)) => c.rollback(),
            None => Ok(()),
        }
    }
}

impl Drop for BoundTransaction {
    fn drop(&mut self) {
        if !self.done {
            if let Some((_, mut c)) = take_bound(&self.key) {
                let _ = c.rollback();
            }
        }
    }
}

/// Run `f` inside a transaction (joining an already bound one): commit if it returns Ok,
/// roll back otherwise.
pub fn run_in_transaction<R>(ds: &Arc<dyn DataSource>, f: impl FnOnce() -> Result<R>) -> Result<R> {
    match BoundTransaction::begin(ds)? {
        None => f(),
        Some(tx) => match f() {
            Ok(r) => {
                tx.commit()?;
                Ok(r)
            }
            Err(e) => {
                let _ = tx.rollback();
                Err(e)
            }
        },
    }
}

/// A `Policy` that runs the wrapped steps in a JDBC transaction (Camel `transacted()` with a
/// `SpringTransactionPolicy` / `PROPAGATION_REQUIRED`).
pub struct JdbcTransactionPolicy {
    ds: Arc<dyn DataSource>,
}

impl JdbcTransactionPolicy {
    pub fn new(ds: Arc<dyn DataSource>) -> Self {
        JdbcTransactionPolicy { ds }
    }
}

impl Policy for JdbcTransactionPolicy {
    fn wrap(&self, _rc: &RouteContext, processor: Arc<dyn Processor>) -> Result<Arc<dyn Processor>> {
        Ok(Arc::new(TransactedProcessor { ds: self.ds.clone(), inner: processor }))
    }
}

/// Create an `Arc<dyn Policy>` for `.policy(...)`.
pub fn transaction_policy(ds: Arc<dyn DataSource>) -> Arc<dyn Policy> {
    Arc::new(JdbcTransactionPolicy::new(ds))
}

/// Bind a transaction policy in the registry (for `.policy_ref(name)` / YAML `policy: ref`).
pub fn bind_transaction_policy(ctx: &camel_core::context::CamelContext, name: &str, ds: Arc<dyn DataSource>) {
    ctx.registry().bind(name, transaction_policy(ds));
}

struct TransactedProcessor {
    ds: Arc<dyn DataSource>,
    inner: Arc<dyn Processor>,
}

impl Processor for TransactedProcessor {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        let Some(tx) = BoundTransaction::begin(&self.ds)? else {
            return self.inner.process(exchange);
        };
        let r = self.inner.process(exchange);
        if r.is_err() || exchange.is_failed() || exchange.rollback_only {
            camel_core::camel_debug!("transaction", "Rolling back transaction for exchange {}", exchange.exchange_id);
            let _ = tx.rollback();
            return r;
        }
        tx.commit().map_err(|e| CamelError::new(e.kind.clone(), format!("Error committing transaction: {}", e.message)).with_cause(e))?;
        r
    }
}

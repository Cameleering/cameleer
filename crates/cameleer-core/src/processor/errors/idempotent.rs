//! Idempotent consumer EIP and idempotent repositories.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use crate::builder::ExpressionDefinition;
use crate::error::{kinds, CamelError, Result};
use crate::impl_definition_boilerplate;
use crate::message::{props, Exchange};
use crate::model::{OutputList, ProcessorDefinition, RouteContext};
use crate::processor::pipeline::{process_safely, Pipeline};
use crate::spi::{Expression, Processor};

/// Repository of already processed message ids (Camel `IdempotentRepository`).
pub trait IdempotentRepository: Send + Sync {
    /// Add the key; returns true if it was not already present.
    fn add(&self, key: &str) -> bool;
    fn contains(&self, key: &str) -> bool;
    /// Remove the key; returns true if it was present.
    fn remove(&self, key: &str) -> bool;
    /// Confirm the key (after successful processing). Default: no-op.
    fn confirm(&self, _key: &str) -> bool {
        true
    }
    fn clear(&self);
}

/// In-memory repository with LRU eviction (Camel `MemoryIdempotentRepository`).
pub struct MemoryIdempotentRepository {
    capacity: usize,
    inner: Mutex<LruState>,
}

#[derive(Default)]
struct LruState {
    tick: u64,
    keys: HashMap<String, u64>,
    order: BTreeMap<u64, String>,
}

impl MemoryIdempotentRepository {
    /// Repository holding at most `capacity` keys (least recently added/used are evicted).
    pub fn new(capacity: usize) -> Self {
        MemoryIdempotentRepository { capacity: capacity.max(1), inner: Mutex::new(LruState::default()) }
    }
    pub fn capacity(&self) -> usize {
        self.capacity
    }
    pub fn size(&self) -> usize {
        self.inner.lock().unwrap().keys.len()
    }
}

impl Default for MemoryIdempotentRepository {
    fn default() -> Self {
        Self::new(1000)
    }
}

/// Create a shareable in-memory repository.
pub fn memory_idempotent_repository(capacity: usize) -> Arc<MemoryIdempotentRepository> {
    Arc::new(MemoryIdempotentRepository::new(capacity))
}

impl IdempotentRepository for MemoryIdempotentRepository {
    fn add(&self, key: &str) -> bool {
        let mut s = self.inner.lock().unwrap();
        s.tick += 1;
        let t = s.tick;
        if let Some(old) = s.keys.insert(key.to_string(), t) {
            s.order.remove(&old);
            s.order.insert(t, key.to_string());
            return false;
        }
        s.order.insert(t, key.to_string());
        while s.keys.len() > self.capacity {
            let (&oldest, _) = s.order.iter().next().unwrap();
            let k = s.order.remove(&oldest).unwrap();
            s.keys.remove(&k);
        }
        true
    }
    fn contains(&self, key: &str) -> bool {
        self.inner.lock().unwrap().keys.contains_key(key)
    }
    fn remove(&self, key: &str) -> bool {
        let mut s = self.inner.lock().unwrap();
        match s.keys.remove(key) {
            Some(t) => {
                s.order.remove(&t);
                true
            }
            None => false,
        }
    }
    fn clear(&self) {
        let mut s = self.inner.lock().unwrap();
        s.keys.clear();
        s.order.clear();
    }
}

/// The idempotent consumer block.
pub struct IdempotentConsumerDefinition {
    pub id: Option<String>,
    pub expression: ExpressionDefinition,
    pub repository: Arc<dyn IdempotentRepository>,
    /// Add the key to the repository before processing (default true).
    pub eager: bool,
    /// Skip duplicates (default true); when false duplicates are processed with
    /// `CamelDuplicateMessage=true`.
    pub skip_duplicate: bool,
    /// Remove the key if the exchange failed (default true).
    pub remove_on_failure: bool,
    /// Complete (confirm/remove) right after the block instead of at the end of the exchange.
    pub completion_eager: bool,
    pub outputs: OutputList,
}

impl IdempotentConsumerDefinition {
    pub fn new(expression: ExpressionDefinition, repository: Arc<dyn IdempotentRepository>) -> Self {
        IdempotentConsumerDefinition {
            id: None,
            expression,
            repository,
            eager: true,
            skip_duplicate: true,
            remove_on_failure: true,
            completion_eager: false,
            outputs: OutputList::default(),
        }
    }
}

impl ProcessorDefinition for IdempotentConsumerDefinition {
    fn name(&self) -> &str {
        "idempotentConsumer"
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
        let child: Arc<dyn Processor> =
            if self.outputs.outputs.is_empty() { Arc::new(Pipeline::new(vec![])) } else { rc.create_child_processor(&self.outputs.outputs)? };
        Ok(Arc::new(IdempotentConsumer {
            expression: self.expression.resolve_expression(&rc.context)?,
            repository: self.repository.clone(),
            eager: self.eager,
            skip_duplicate: self.skip_duplicate,
            remove_on_failure: self.remove_on_failure,
            completion_eager: self.completion_eager,
            child,
        }))
    }
    impl_definition_boilerplate!();
}

struct IdempotentConsumer {
    expression: Arc<dyn Expression>,
    repository: Arc<dyn IdempotentRepository>,
    eager: bool,
    skip_duplicate: bool,
    remove_on_failure: bool,
    completion_eager: bool,
    child: Arc<dyn Processor>,
}

fn complete(repo: &dyn IdempotentRepository, key: &str, failed: bool, eager: bool, remove_on_failure: bool) {
    if failed {
        if remove_on_failure {
            repo.remove(key);
        }
    } else {
        if !eager {
            repo.add(key);
        }
        repo.confirm(key);
    }
}

impl Processor for IdempotentConsumer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let v = self.expression.evaluate(ex)?;
        if v.is_null() {
            return Err(CamelError::new(kinds::NO_MESSAGE_ID, format!("No message ID could be found using expression on message exchange: {}", ex.exchange_id)));
        }
        let key = v.to_string();
        let is_new = if self.eager { self.repository.add(&key) } else { !self.repository.contains(&key) };
        if !is_new {
            ex.set_property(props::DUPLICATE_MESSAGE, true);
            if self.skip_duplicate {
                crate::camel_debug!("IdempotentConsumer", "Ignoring duplicate message with id: {} for exchange: {}", key, ex.exchange_id);
                return Ok(());
            }
            process_safely(self.child.as_ref(), ex);
            return Ok(());
        }
        process_safely(self.child.as_ref(), ex);
        if self.completion_eager {
            complete(self.repository.as_ref(), &key, ex.is_failed(), self.eager, self.remove_on_failure);
        } else {
            let repo = self.repository.clone();
            let (eager, rof) = (self.eager, self.remove_on_failure);
            ex.add_on_completion(move |done: &Exchange| complete(repo.as_ref(), &key, done.is_failed(), eager, rof));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lru_eviction() {
        let r = MemoryIdempotentRepository::new(2);
        assert!(r.add("a"));
        assert!(r.add("b"));
        assert!(!r.add("a")); // refreshes a
        assert!(r.add("c")); // evicts b
        assert!(r.contains("a"));
        assert!(!r.contains("b"));
        assert!(r.contains("c"));
        assert_eq!(r.size(), 2);
        assert!(r.remove("a"));
        assert!(!r.remove("a"));
        r.clear();
        assert_eq!(r.size(), 0);
    }
}

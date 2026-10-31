//! `seda:`, `vm:` and `stub:` components - asynchronous in-process queues.
//!
//! `seda:name?size=1000&concurrentConsumers=1&waitForTaskToComplete=IfReplyExpected&timeout=30000
//! &multipleConsumers=false&blockWhenFull=false&discardWhenFull=false&offerTimeout=0&pollTimeout=1000
//! &purgeWhenStopping=false&limitConcurrentConsumers=true&failIfNoConsumers=false&discardIfNoConsumers=false`
//!
//! * Queues are bounded and shared by name within the component (seda) or across every
//!   context in the process (vm).
//! * Exchanges crossing the queue are correlated copies (new exchange id, property
//!   `CamelCorrelationId`). For fire-and-forget sends the on-completion callbacks are
//!   handed over to the copy (they run when the consumer route completes).
//! * InOut exchanges wait (`waitForTaskToComplete=IfReplyExpected`) for the consumer to
//!   finish and the result is copied back.
//! * `multipleConsumers=true` turns the queue into publish-subscribe: every consumer
//!   receives a copy of each message. Otherwise consumers compete.
//! * Endpoints support polling consumers (ConsumerTemplate, pollEnrich).
//! * `stub:` accepts any uri (all options ignored) and behaves like seda.

use std::any::Any;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use crate::context::{CamelContext, WeakContext};
use crate::error::{kinds, CamelError, Result};
use crate::message::Exchange;
use crate::spi::{Component, Consumer, Endpoint, PollingConsumer, Processor, Producer, Service};
use crate::util::Parameters;

use super::support::{copy_results, deadline_after, process_consumer_exchange, BackgroundThreads};

/// Default queue capacity.
pub const DEFAULT_QUEUE_SIZE: usize = 1000;

/// When a producer waits for the consumer to complete the exchange.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitForTaskToComplete {
    Never,
    IfReplyExpected,
    Always,
}

impl WaitForTaskToComplete {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "Never" | "never" => Ok(Self::Never),
            "IfReplyExpected" | "ifReplyExpected" => Ok(Self::IfReplyExpected),
            "Always" | "always" => Ok(Self::Always),
            other => Err(CamelError::illegal_argument(format!("Invalid waitForTaskToComplete: {}", other))),
        }
    }
}

struct QueueItem {
    id: u64,
    exchange: Exchange,
    reply: Option<mpsc::Sender<Exchange>>,
}

type Subscriber = (u64, Arc<dyn Processor>, WeakContext);

#[derive(Default)]
struct QueueState {
    items: VecDeque<QueueItem>,
    consumers: usize,
    subscribers: Vec<Subscriber>,
}

/// A bounded blocking queue of exchanges shared by seda/vm endpoints with the same name.
pub struct SedaQueue {
    name: String,
    capacity: usize,
    state: Mutex<QueueState>,
    not_empty: Condvar,
    not_full: Condvar,
    ids: AtomicU64,
}

impl SedaQueue {
    pub fn new(name: &str, capacity: usize) -> Arc<SedaQueue> {
        Arc::new(SedaQueue {
            name: name.to_string(),
            capacity: capacity.max(1),
            state: Mutex::new(QueueState::default()),
            not_empty: Condvar::new(),
            not_full: Condvar::new(),
            ids: AtomicU64::new(0),
        })
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn capacity(&self) -> usize {
        self.capacity
    }
    /// Number of exchanges currently waiting in the queue.
    pub fn size(&self) -> usize {
        self.state.lock().unwrap().items.len()
    }
    pub fn remaining_capacity(&self) -> usize {
        self.capacity.saturating_sub(self.size())
    }
    /// Snapshot of the queued exchanges.
    pub fn exchanges(&self) -> Vec<Exchange> {
        self.state.lock().unwrap().items.iter().map(|i| i.exchange.clone()).collect()
    }
    /// Remove all queued exchanges.
    pub fn purge(&self) {
        self.state.lock().unwrap().items.clear();
        self.not_full.notify_all();
    }
    /// Number of started consumers.
    pub fn consumer_count(&self) -> usize {
        self.state.lock().unwrap().consumers
    }
    fn wake_all(&self) {
        let _g = self.state.lock().unwrap();
        self.not_empty.notify_all();
        self.not_full.notify_all();
    }

    fn offer(&self, item: QueueItem, block: bool, discard: bool, offer_timeout: Duration) -> Result<bool> {
        let mut st = self.state.lock().unwrap();
        if st.items.len() >= self.capacity {
            if discard {
                crate::camel_debug!("seda", "Discarding exchange as queue {} is full", self.name);
                return Ok(false);
            }
            if !block {
                return Err(CamelError::new(kinds::ILLEGAL_STATE, "Queue full"));
            }
            let deadline = if offer_timeout.is_zero() { None } else { deadline_after(offer_timeout) };
            while st.items.len() >= self.capacity {
                match deadline {
                    None => st = self.not_full.wait(st).unwrap(),
                    Some(d) => {
                        let now = std::time::Instant::now();
                        if now >= d {
                            return Err(CamelError::new(
                                kinds::ILLEGAL_STATE,
                                format!("Fails to insert element into queue, after timeout of {} milliseconds", offer_timeout.as_millis()),
                            ));
                        }
                        st = self.not_full.wait_timeout(st, d - now).unwrap().0;
                    }
                }
            }
        }
        st.items.push_back(item);
        self.not_empty.notify_one();
        Ok(true)
    }

    /// Put an exchange on the queue (fire-and-forget). Fails with `IllegalStateException`
    /// "Queue full" when the queue is at capacity.
    pub fn put(&self, exchange: Exchange) -> Result<()> {
        let id = self.ids.fetch_add(1, Ordering::Relaxed);
        self.offer(QueueItem { id, exchange, reply: None }, false, false, Duration::ZERO).map(|_| ())
    }

    fn remove_item(&self, id: u64) -> bool {
        let mut st = self.state.lock().unwrap();
        let before = st.items.len();
        st.items.retain(|i| i.id != id);
        let removed = st.items.len() != before;
        if removed {
            self.not_full.notify_all();
        }
        removed
    }

    /// Take an item, waiting up to `timeout` (None = don't wait).
    fn poll(&self, timeout: Option<Duration>) -> Option<QueueItem> {
        let mut st = self.state.lock().unwrap();
        if let Some(i) = st.items.pop_front() {
            self.not_full.notify_one();
            return Some(i);
        }
        let deadline = match timeout {
            None => return None,
            Some(t) if t.is_zero() => return None,
            Some(t) => deadline_after(t),
        };
        loop {
            match deadline {
                None => st = self.not_empty.wait(st).unwrap(),
                Some(d) => {
                    let now = std::time::Instant::now();
                    if now >= d {
                        return None;
                    }
                    st = self.not_empty.wait_timeout(st, d - now).unwrap().0;
                }
            }
            if let Some(i) = st.items.pop_front() {
                self.not_full.notify_one();
                return Some(i);
            }
        }
    }
}

/// Queue registry shared by a component (seda) or process-wide (vm).
#[derive(Default)]
pub struct QueueRegistry {
    queues: Mutex<HashMap<String, Arc<SedaQueue>>>,
}

impl QueueRegistry {
    /// Get or create the queue for `name`. An explicitly configured size that differs
    /// from the existing queue's size is an error (as in Camel).
    pub fn get_or_create(&self, scheme: &str, name: &str, size: Option<usize>) -> Result<Arc<SedaQueue>> {
        let mut q = self.queues.lock().unwrap();
        if let Some(existing) = q.get(name) {
            if let Some(s) = size {
                if s != existing.capacity {
                    return Err(CamelError::illegal_argument(format!(
                        "Cannot use existing queue {}://{} as the existing queue has a different size: {} than requested: {}",
                        scheme, name, existing.capacity, s
                    )));
                }
            }
            return Ok(existing.clone());
        }
        let queue = SedaQueue::new(name, size.unwrap_or(DEFAULT_QUEUE_SIZE));
        q.insert(name.to_string(), queue.clone());
        Ok(queue)
    }
    pub fn get(&self, name: &str) -> Option<Arc<SedaQueue>> {
        self.queues.lock().unwrap().get(name).cloned()
    }
}

fn vm_registry() -> &'static QueueRegistry {
    static VM: OnceLock<QueueRegistry> = OnceLock::new();
    VM.get_or_init(QueueRegistry::default)
}

/// Endpoint options of seda/vm endpoints.
#[derive(Clone, Debug)]
pub struct SedaConfig {
    pub concurrent_consumers: usize,
    pub wait_for_task_to_complete: WaitForTaskToComplete,
    pub timeout: Duration,
    pub multiple_consumers: bool,
    pub block_when_full: bool,
    pub discard_when_full: bool,
    pub offer_timeout: Duration,
    pub poll_timeout: Duration,
    pub purge_when_stopping: bool,
    pub fail_if_no_consumers: bool,
    pub discard_if_no_consumers: bool,
}

impl Default for SedaConfig {
    fn default() -> Self {
        SedaConfig {
            concurrent_consumers: 1,
            wait_for_task_to_complete: WaitForTaskToComplete::IfReplyExpected,
            timeout: Duration::from_millis(30_000),
            multiple_consumers: false,
            block_when_full: false,
            discard_when_full: false,
            offer_timeout: Duration::ZERO,
            poll_timeout: Duration::from_millis(1000),
            purge_when_stopping: false,
            fail_if_no_consumers: false,
            discard_if_no_consumers: false,
        }
    }
}

impl SedaConfig {
    fn from_params(params: &mut Parameters) -> Result<(SedaConfig, Option<usize>)> {
        let mut c = SedaConfig::default();
        let size = match params.take("size").or_else(|| params.take("queueSize")) {
            Some(s) => Some(s.trim().parse::<usize>().map_err(|_| CamelError::illegal_argument(format!("Invalid size: {}", s)))?),
            None => None,
        };
        c.concurrent_consumers = params.take_usize("concurrentConsumers", 1)?.max(1);
        let limit = params.take_bool("limitConcurrentConsumers", true)?;
        if limit && c.concurrent_consumers > 500 {
            return Err(CamelError::illegal_argument(format!(
                "The limitConcurrentConsumers flag in set to true. ConcurrentConsumers cannot be set at a value greater than 500. Value was {}",
                c.concurrent_consumers
            )));
        }
        if let Some(w) = params.take("waitForTaskToComplete") {
            c.wait_for_task_to_complete = WaitForTaskToComplete::parse(&w)?;
        }
        c.timeout = params.take_duration("timeout", c.timeout)?;
        c.multiple_consumers = params.take_bool("multipleConsumers", false)?;
        c.block_when_full = params.take_bool("blockWhenFull", false)?;
        c.discard_when_full = params.take_bool("discardWhenFull", false)?;
        if c.block_when_full && c.discard_when_full {
            return Err(CamelError::illegal_argument("Cannot enable both discardWhenFull=true and blockWhenFull=true."));
        }
        c.offer_timeout = params.take_duration("offerTimeout", Duration::ZERO)?;
        c.poll_timeout = params.take_duration("pollTimeout", c.poll_timeout)?.max(Duration::from_millis(1));
        c.purge_when_stopping = params.take_bool("purgeWhenStopping", false)?;
        c.fail_if_no_consumers = params.take_bool("failIfNoConsumers", false)?;
        c.discard_if_no_consumers = params.take_bool("discardIfNoConsumers", false)?;
        for k in ["synchronous", "exchangePattern", "bridgeErrorHandler", "exceptionHandler", "lazyStartProducer", "queue", "defaultQueueFactory"] {
            params.take(k);
        }
        Ok((c, size))
    }
}

/// The `seda:` component (also used for `vm:` and `stub:` via [`SedaComponent::vm`]/[`SedaComponent::stub`]).
pub struct SedaComponent {
    scheme: &'static str,
    registry: Option<QueueRegistry>,
    lenient: bool,
}

impl Default for SedaComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl SedaComponent {
    pub fn new() -> Self {
        SedaComponent { scheme: "seda", registry: Some(QueueRegistry::default()), lenient: false }
    }
    /// The `vm:` component: queues are shared by all contexts in the process.
    pub fn vm() -> Self {
        SedaComponent { scheme: "vm", registry: None, lenient: false }
    }
    /// The `stub:` component: accepts any uri and behaves like seda.
    pub fn stub() -> Self {
        SedaComponent { scheme: "stub", registry: Some(QueueRegistry::default()), lenient: true }
    }
    fn queues(&self) -> &QueueRegistry {
        self.registry.as_ref().unwrap_or_else(|| vm_registry())
    }
    /// The queue with this name, if created.
    pub fn queue(&self, name: &str) -> Option<Arc<SedaQueue>> {
        self.queues().get(name)
    }
}

impl Component for SedaComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let (config, size) = if self.lenient {
            params.take_all();
            (SedaConfig::default(), None)
        } else {
            SedaConfig::from_params(params)?
        };
        let queue = self.queues().get_or_create(self.scheme, remaining, size)?;
        Ok(Arc::new(SedaEndpoint { uri: uri.to_string(), ctx: ctx.downgrade(), queue, config }))
    }
    fn lenient_properties(&self) -> bool {
        self.lenient
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A seda/vm/stub endpoint.
pub struct SedaEndpoint {
    uri: String,
    ctx: WeakContext,
    queue: Arc<SedaQueue>,
    config: SedaConfig,
}

impl SedaEndpoint {
    pub fn queue(&self) -> &Arc<SedaQueue> {
        &self.queue
    }
    /// Current number of exchanges in the queue.
    pub fn queue_size(&self) -> usize {
        self.queue.size()
    }
    /// Snapshot of queued exchanges (Camel `getExchanges()`).
    pub fn exchanges(&self) -> Vec<Exchange> {
        self.queue.exchanges()
    }
    pub fn purge_queue(&self) {
        self.queue.purge()
    }
    pub fn consumer_count(&self) -> usize {
        self.queue.consumer_count()
    }
    pub fn config(&self) -> &SedaConfig {
        &self.config
    }
}

impl Endpoint for SedaEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(SedaProducer { uri: self.uri.clone(), queue: self.queue.clone(), config: self.config.clone() }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        static IDS: AtomicUsize = AtomicUsize::new(0);
        Ok(Arc::new(SedaConsumer {
            id: IDS.fetch_add(1, Ordering::Relaxed) as u64,
            uri: self.uri.clone(),
            ctx: self.ctx.clone(),
            queue: self.queue.clone(),
            config: self.config.clone(),
            processor,
            threads: BackgroundThreads::new(),
            started: Mutex::new(false),
        }))
    }
    fn create_polling_consumer(&self) -> Result<Arc<dyn PollingConsumer>> {
        Ok(Arc::new(SedaPollingConsumer { queue: self.queue.clone(), ctx: self.ctx.clone() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct SedaProducer {
    uri: String,
    queue: Arc<SedaQueue>,
    config: SedaConfig,
}

impl Service for SedaProducer {}

impl Processor for SedaProducer {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        let c = &self.config;
        if (c.fail_if_no_consumers || c.discard_if_no_consumers) && self.queue.consumer_count() == 0 {
            if c.discard_if_no_consumers {
                return Ok(());
            }
            return Err(CamelError::new(
                "SedaConsumerNotAvailableException",
                format!("No consumers available on endpoint: {}. Exchange[{}]", self.uri, exchange.exchange_id),
            ));
        }
        let wait = match c.wait_for_task_to_complete {
            WaitForTaskToComplete::Always => true,
            WaitForTaskToComplete::Never => false,
            WaitForTaskToComplete::IfReplyExpected => exchange.is_out_capable(),
        };
        let mut copy = exchange.correlated_copy();
        copy.from_route_id = None;
        copy.from_endpoint = None;
        let id = self.queue.ids.fetch_add(1, Ordering::Relaxed);
        if !wait {
            // hand over synchronizations: they run when the consumer completes the copy
            copy.on_completions = std::mem::take(&mut exchange.on_completions);
            self.queue.offer(QueueItem { id, exchange: copy, reply: None }, c.block_when_full, c.discard_when_full, c.offer_timeout)?;
            return Ok(());
        }
        let (tx, rx) = mpsc::channel();
        if !self.queue.offer(QueueItem { id, exchange: copy, reply: Some(tx) }, c.block_when_full, c.discard_when_full, c.offer_timeout)? {
            return Ok(());
        }
        let reply = if c.timeout.is_zero() {
            rx.recv().ok()
        } else {
            match rx.recv_timeout(c.timeout) {
                Ok(r) => Some(r),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    self.queue.remove_item(id);
                    return Err(CamelError::new(
                        kinds::EXCHANGE_TIMED_OUT,
                        format!("Exchange timed out after waiting: {} millis. Exchange[{}]", c.timeout.as_millis(), exchange.exchange_id),
                    ));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => None,
            }
        };
        match reply {
            Some(r) => {
                copy_results(exchange, &r);
                Ok(())
            }
            None => Err(CamelError::new(kinds::CAMEL_EXCHANGE, format!("Exchange was discarded by {} before completion", self.uri))),
        }
    }
}

struct SedaConsumer {
    id: u64,
    uri: String,
    ctx: WeakContext,
    queue: Arc<SedaQueue>,
    config: SedaConfig,
    processor: Arc<dyn Processor>,
    threads: BackgroundThreads,
    started: Mutex<bool>,
}

fn dispatch(item: QueueItem, queue: &SedaQueue, multiple: bool, processor: &Arc<dyn Processor>, ctx: &WeakContext, logger: &str) {
    let QueueItem { mut exchange, reply, .. } = item;
    if multiple {
        let subs: Vec<Subscriber> = queue.state.lock().unwrap().subscribers.clone();
        let mut last: Option<Exchange> = None;
        let n = subs.len();
        for (i, (_, p, sctx)) in subs.into_iter().enumerate() {
            let mut copy = if i + 1 == n { std::mem::take(&mut exchange) } else { exchange.clone() };
            if let Some(c) = sctx.upgrade() {
                copy.context = Some(c);
            }
            process_consumer_exchange(logger, p.as_ref(), &mut copy);
            last = Some(copy);
        }
        if let (Some(tx), Some(l)) = (reply, last) {
            let _ = tx.send(l);
        }
    } else {
        if let Some(c) = ctx.upgrade() {
            exchange.context = Some(c);
        }
        process_consumer_exchange(logger, processor.as_ref(), &mut exchange);
        if let Some(tx) = reply {
            let _ = tx.send(exchange);
        }
    }
}

impl Service for SedaConsumer {
    fn start(&self) -> Result<()> {
        let mut started = self.started.lock().unwrap();
        if *started {
            return Ok(());
        }
        {
            let mut st = self.queue.state.lock().unwrap();
            st.consumers += 1;
            if self.config.multiple_consumers {
                st.subscribers.push((self.id, self.processor.clone(), self.ctx.clone()));
            }
        }
        for i in 0..self.config.concurrent_consumers {
            let (queue, processor, ctx) = (self.queue.clone(), self.processor.clone(), self.ctx.clone());
            let (multiple, poll_timeout) = (self.config.multiple_consumers, self.config.poll_timeout);
            let logger = self.uri.clone();
            self.threads.spawn(&format!("Camel {} #{}", self.uri, i + 1), move |flag| {
                let mut st = queue.state.lock().unwrap();
                loop {
                    if flag.is_stopped() {
                        break;
                    }
                    if let Some(item) = st.items.pop_front() {
                        queue.not_full.notify_one();
                        drop(st);
                        dispatch(item, &queue, multiple, &processor, &ctx, &logger);
                        st = queue.state.lock().unwrap();
                        continue;
                    }
                    st = queue.not_empty.wait_timeout(st, poll_timeout).unwrap().0;
                }
            })?;
        }
        *started = true;
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        let mut started = self.started.lock().unwrap();
        if !*started {
            return Ok(());
        }
        self.threads.flag().stop();
        self.queue.wake_all();
        self.threads.stop();
        {
            let mut st = self.queue.state.lock().unwrap();
            st.consumers = st.consumers.saturating_sub(1);
            st.subscribers.retain(|s| s.0 != self.id);
        }
        if self.config.purge_when_stopping {
            self.queue.purge();
        }
        *started = false;
        Ok(())
    }
}

struct SedaPollingConsumer {
    queue: Arc<SedaQueue>,
    ctx: WeakContext,
}

impl Service for SedaPollingConsumer {}

impl PollingConsumer for SedaPollingConsumer {
    fn receive(&self, timeout: Option<Duration>) -> Result<Option<Exchange>> {
        Ok(self.queue.poll(timeout).map(|item| {
            let mut ex = item.exchange;
            if ex.context.is_none() {
                ex.context = self.ctx.upgrade();
            }
            if let Some(tx) = item.reply {
                // a polling consumer completes the exchange as-is for a waiting producer
                let _ = tx.send(ex.clone());
            }
            ex
        }))
    }
}

/// Look up the seda/vm/stub endpoint behind a uri (for tests and management).
pub fn seda_endpoint(ctx: &CamelContext, uri: &str) -> Result<Arc<dyn Endpoint>> {
    let e = ctx.get_endpoint(uri)?;
    if e.as_any().is::<SedaEndpoint>() {
        Ok(e)
    } else {
        Err(CamelError::illegal_argument(format!("{} is not a seda endpoint", uri)))
    }
}

/// Current queue size of a seda/vm/stub endpoint uri.
pub fn queue_size(ctx: &CamelContext, uri: &str) -> Result<usize> {
    let e = seda_endpoint(ctx, uri)?;
    Ok(e.as_any().downcast_ref::<SedaEndpoint>().map(|s| s.queue_size()).unwrap_or(0))
}

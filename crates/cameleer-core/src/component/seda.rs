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
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use crate::context::{CamelContext, WeakContext};
use crate::error::{kinds, CamelError, Result};
use crate::message::Exchange;
use crate::spi::{Component, Consumer, Endpoint, PollingConsumer, Processor, Producer, Service, SuspendableService};
use crate::util::Parameters;

use super::support::{copy_results, deadline_after, process_consumer_exchange, BackgroundThreads, StopFlag, SuspendState, ThreadSpawner};
use crate::async_delay;

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
    reply: Option<Reply>,
}

/// Iterations a thread spins, watching the queue length or its reply slot, before it
/// parks: waking a parked thread costs the other side a system call, and under load the
/// state changes within microseconds.
const SPIN: u32 = 4000;

/// Where a producer waiting for a reply (InOut) receives it. One per producer thread,
/// reused for every request; a generation number tells replies to old (timed out)
/// requests apart.
struct ReplySlot {
    state: Mutex<SlotState>,
    filled: Condvar,
    /// The generation of the last request answered (or discarded), for spinning without the lock.
    done_gen: AtomicU64,
}

#[derive(Default)]
struct SlotState {
    gen: u64,
    reply: Option<Exchange>,
    discarded: bool,
    waiting: bool,
}

thread_local! {
    static REPLY_SLOT: Arc<ReplySlot> =
        Arc::new(ReplySlot { state: Mutex::new(SlotState::default()), filled: Condvar::new(), done_gen: AtomicU64::new(0) });
}

/// The producer's side of a request: waits on this thread's slot.
struct ReplyWaiter {
    slot: Arc<ReplySlot>,
    gen: u64,
}

/// The consumer's side, carried by the queued exchange. Dropping it unanswered tells the
/// producer the exchange was discarded (like dropping a channel sender).
struct Reply {
    slot: Arc<ReplySlot>,
    gen: u64,
    sent: bool,
}

impl ReplyWaiter {
    /// Start a new request on this thread's slot.
    fn begin() -> (ReplyWaiter, Reply) {
        let slot = REPLY_SLOT.with(|s| s.clone());
        let gen = {
            let mut st = slot.state.lock().unwrap();
            st.gen += 1;
            st.reply = None;
            st.discarded = false;
            st.gen
        };
        (ReplyWaiter { slot: slot.clone(), gen }, Reply { slot, gen, sent: false })
    }

    /// Wait for the reply: `Ok(None)` if discarded, `Err(())` on timeout (zero = forever).
    fn wait(&self, timeout: Duration) -> std::result::Result<Option<Exchange>, ()> {
        for _ in 0..SPIN {
            if self.slot.done_gen.load(Ordering::Acquire) == self.gen {
                break;
            }
            std::hint::spin_loop();
        }
        let deadline = if timeout.is_zero() { None } else { deadline_after(timeout) };
        let mut st = self.slot.state.lock().unwrap();
        loop {
            if st.reply.is_some() || st.discarded {
                return Ok(st.reply.take());
            }
            st.waiting = true;
            st = match deadline {
                None => self.slot.filled.wait(st).unwrap(),
                Some(d) => {
                    let now = std::time::Instant::now();
                    if now >= d {
                        st.waiting = false;
                        // a late reply to this request is dropped
                        st.gen += 1;
                        return Err(());
                    }
                    self.slot.filled.wait_timeout(st, d - now).unwrap().0
                }
            };
            st.waiting = false;
        }
    }
}

impl Reply {
    fn deliver(&mut self, ex: Option<Exchange>) {
        self.sent = true;
        let mut st = self.slot.state.lock().unwrap();
        if st.gen != self.gen {
            return; // the producer gave up on this request
        }
        match ex {
            Some(ex) => st.reply = Some(ex),
            None => st.discarded = true,
        }
        self.slot.done_gen.store(self.gen, Ordering::Release);
        if st.waiting {
            self.slot.filled.notify_one();
        }
    }

    fn send(mut self, ex: Exchange) {
        self.deliver(Some(ex));
    }
}

impl Drop for Reply {
    fn drop(&mut self) {
        if !self.sent {
            self.deliver(None);
        }
    }
}

type Subscriber = (u64, Arc<dyn Processor>, WeakContext);

/// A value alone on its cache line(s), so threads writing neighbouring fields do not
/// invalidate it (128 bytes: adjacent-line prefetching pairs 64-byte lines).
#[repr(align(128))]
struct Padded<T>(T);

impl<T> std::ops::Deref for Padded<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

/// Consumers and publish-subscribe subscribers of a queue.
#[derive(Default)]
struct Members {
    consumers: usize,
    subscribers: Vec<Subscriber>,
}

/// A bounded blocking queue of exchanges shared by seda/vm endpoints with the same name.
///
/// Two locks, like Java's `LinkedBlockingQueue`: producers append to `inbox` under its
/// lock, consumers take from `outbox` under another, and a consumer that finds the outbox
/// empty swaps the inbox in. Producers and consumers then rarely contend with each other,
/// and the order stays first-in first-out. Parked threads are only signalled when some
/// are waiting (a signal is a system call), and both sides spin briefly before parking.
/// Lock order: `outbox`, then `inbox`.
pub struct SedaQueue {
    name: String,
    capacity: usize,
    // the fields producers and consumers write are on their own cache lines
    outbox: Padded<Mutex<VecDeque<QueueItem>>>,
    inbox: Padded<Mutex<VecDeque<QueueItem>>>,
    /// Waited on with the `outbox` lock.
    not_empty: Condvar,
    /// Waited on with the `inbox` lock.
    not_full: Condvar,
    /// Items in both halves; only increased under the `inbox` lock, so the capacity check holds.
    count: Padded<AtomicUsize>,
    takers_waiting: Padded<AtomicUsize>,
    putters_waiting: Padded<AtomicUsize>,
    members: Mutex<Members>,
    ids: AtomicU64,
    /// Serializes publish-subscribe dispatch so all subscribers see messages in order.
    pubsub: Mutex<()>,
}

impl SedaQueue {
    pub fn new(name: &str, capacity: usize) -> Arc<SedaQueue> {
        Arc::new(SedaQueue {
            name: name.to_string(),
            capacity: capacity.max(1),
            outbox: Padded(Mutex::new(VecDeque::new())),
            inbox: Padded(Mutex::new(VecDeque::new())),
            not_empty: Condvar::new(),
            not_full: Condvar::new(),
            count: Padded(AtomicUsize::new(0)),
            takers_waiting: Padded(AtomicUsize::new(0)),
            putters_waiting: Padded(AtomicUsize::new(0)),
            members: Mutex::new(Members::default()),
            pubsub: Mutex::new(()),
            ids: AtomicU64::new(0),
        })
    }

    /// Append under the `inbox` lock (capacity already checked), then wake a parked consumer.
    fn push(&self, mut inbox: std::sync::MutexGuard<'_, VecDeque<QueueItem>>, item: QueueItem) {
        inbox.push_back(item);
        self.count.fetch_add(1, Ordering::SeqCst);
        drop(inbox);
        if self.takers_waiting.load(Ordering::SeqCst) > 0 {
            // taking the lock orders this signal after the consumer started waiting
            let _g = self.outbox.lock().unwrap();
            self.not_empty.notify_one();
        }
    }

    /// Take the oldest item; the caller holds the `outbox` lock.
    fn pop_locked(&self, outbox: &mut VecDeque<QueueItem>) -> Option<QueueItem> {
        if outbox.is_empty() {
            if self.count.load(Ordering::SeqCst) == 0 {
                return None;
            }
            std::mem::swap(outbox, &mut *self.inbox.lock().unwrap());
        }
        let item = outbox.pop_front()?;
        self.count.fetch_sub(1, Ordering::SeqCst);
        if self.putters_waiting.load(Ordering::SeqCst) > 0 {
            let _g = self.inbox.lock().unwrap();
            self.not_full.notify_one();
        }
        Some(item)
    }

    fn try_pop(&self) -> Option<QueueItem> {
        let mut out = self.outbox.lock().unwrap();
        self.pop_locked(&mut out)
    }

    /// Park until an item may be available, `timeout` passes or `stop` is set.
    fn park_taker(&self, timeout: Duration, stop: Option<&StopFlag>) {
        let out = self.outbox.lock().unwrap();
        self.takers_waiting.fetch_add(1, Ordering::SeqCst);
        if self.count.load(Ordering::SeqCst) == 0 && !stop.map(|f| f.is_stopped()).unwrap_or(false) {
            drop(self.not_empty.wait_timeout(out, timeout).unwrap());
        }
        self.takers_waiting.fetch_sub(1, Ordering::SeqCst);
    }

    /// Spin, without locks, until the item count satisfies `ready` or a while passed.
    fn spin_until(&self, ready: impl Fn(usize) -> bool, stop: Option<&StopFlag>) {
        for i in 0..SPIN {
            if ready(self.count.load(Ordering::Acquire)) || (i % 64 == 0 && stop.map(|f| f.is_stopped()).unwrap_or(false)) {
                return;
            }
            std::hint::spin_loop();
        }
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn capacity(&self) -> usize {
        self.capacity
    }
    /// Number of exchanges currently waiting in the queue.
    pub fn size(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
    pub fn remaining_capacity(&self) -> usize {
        self.capacity.saturating_sub(self.size())
    }
    /// Snapshot of the queued exchanges (oldest first).
    pub fn exchanges(&self) -> Vec<Exchange> {
        let out = self.outbox.lock().unwrap();
        let inbox = self.inbox.lock().unwrap();
        out.iter().chain(inbox.iter()).map(|i| i.exchange.clone()).collect()
    }
    /// Remove all queued exchanges.
    pub fn purge(&self) {
        let mut out = self.outbox.lock().unwrap();
        let mut inbox = self.inbox.lock().unwrap();
        out.clear();
        inbox.clear();
        self.count.store(0, Ordering::SeqCst);
        self.not_full.notify_all();
    }
    /// Number of started consumers.
    pub fn consumer_count(&self) -> usize {
        self.members.lock().unwrap().consumers
    }
    fn wake_all(&self) {
        {
            let _g = self.outbox.lock().unwrap();
            self.not_empty.notify_all();
        }
        let _g = self.inbox.lock().unwrap();
        self.not_full.notify_all();
    }

    fn offer(&self, item: QueueItem, block: bool, discard: bool, offer_timeout: Duration) -> Result<bool> {
        if block && self.count.load(Ordering::Acquire) >= self.capacity {
            self.spin_until(|n| n < self.capacity, None);
        }
        let mut inbox = self.inbox.lock().unwrap();
        if self.count.load(Ordering::SeqCst) >= self.capacity {
            if discard {
                crate::camel_debug!("seda", "Discarding exchange as queue {} is full", self.name);
                return Ok(false);
            }
            if !block {
                return Err(CamelError::new(kinds::ILLEGAL_STATE, "Queue full"));
            }
            let deadline = if offer_timeout.is_zero() { None } else { deadline_after(offer_timeout) };
            loop {
                self.putters_waiting.fetch_add(1, Ordering::SeqCst);
                if self.count.load(Ordering::SeqCst) < self.capacity {
                    self.putters_waiting.fetch_sub(1, Ordering::SeqCst);
                    break;
                }
                match deadline {
                    None => inbox = self.not_full.wait(inbox).unwrap(),
                    Some(d) => {
                        let now = std::time::Instant::now();
                        if now >= d {
                            self.putters_waiting.fetch_sub(1, Ordering::SeqCst);
                            return Err(CamelError::new(
                                kinds::ILLEGAL_STATE,
                                format!("Fails to insert element into queue, after timeout of {} milliseconds", offer_timeout.as_millis()),
                            ));
                        }
                        inbox = self.not_full.wait_timeout(inbox, d - now).unwrap().0;
                    }
                }
                self.putters_waiting.fetch_sub(1, Ordering::SeqCst);
            }
        }
        self.push(inbox, item);
        Ok(true)
    }

    /// Put an exchange on the queue (fire-and-forget). Fails with `IllegalStateException`
    /// "Queue full" when the queue is at capacity.
    pub fn put(&self, exchange: Exchange) -> Result<()> {
        let id = self.ids.fetch_add(1, Ordering::Relaxed);
        self.offer(QueueItem { id, exchange, reply: None }, false, false, Duration::ZERO).map(|_| ())
    }

    fn remove_item(&self, id: u64) -> bool {
        let mut out = self.outbox.lock().unwrap();
        let mut inbox = self.inbox.lock().unwrap();
        let before = out.len() + inbox.len();
        out.retain(|i| i.id != id);
        inbox.retain(|i| i.id != id);
        let removed = before - (out.len() + inbox.len());
        if removed > 0 {
            self.count.fetch_sub(removed, Ordering::SeqCst);
            self.not_full.notify_all();
        }
        removed > 0
    }

    /// Take an item, waiting up to `timeout` (None = don't wait).
    fn poll(&self, timeout: Option<Duration>) -> Option<QueueItem> {
        if let Some(i) = self.try_pop() {
            return Some(i);
        }
        let deadline = match timeout {
            None => return None,
            Some(t) if t.is_zero() => return None,
            Some(t) => deadline_after(t),
        };
        loop {
            match deadline {
                None => self.park_taker(Duration::from_secs(3600), None),
                Some(d) => {
                    let now = std::time::Instant::now();
                    if now >= d {
                        return None;
                    }
                    self.park_taker(d - now, None);
                }
            }
            if let Some(i) = self.try_pop() {
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
            suspend: SuspendState::new(),
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
        let (waiter, reply) = ReplyWaiter::begin();
        if !self.queue.offer(QueueItem { id, exchange: copy, reply: Some(reply) }, c.block_when_full, c.discard_when_full, c.offer_timeout)? {
            return Ok(());
        }
        let reply = match waiter.wait(c.timeout) {
            Ok(r) => r,
            Err(()) => {
                self.queue.remove_item(id);
                return Err(CamelError::new(
                    kinds::EXCHANGE_TIMED_OUT,
                    format!("Exchange timed out after waiting: {} millis. Exchange[{}]", c.timeout.as_millis(), exchange.exchange_id),
                ));
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
    suspend: Arc<SuspendState>,
}

fn dispatch(item: QueueItem, queue: &SedaQueue, multiple: bool, processor: &Arc<dyn Processor>, ctx: &WeakContext, logger: &str) {
    let QueueItem { mut exchange, reply, .. } = item;
    if multiple {
        let subs: Vec<Subscriber> = queue.members.lock().unwrap().subscribers.clone();
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
            tx.send(l);
        }
    } else {
        // usually the exchange already belongs to this context: leave its (shared) handle alone
        if !exchange.context.as_ref().map(|c| ctx.is(c)).unwrap_or(false) {
            if let Some(c) = ctx.upgrade() {
                exchange.context = Some(c);
            }
        }
        process_consumer_exchange(logger, processor.as_ref(), &mut exchange);
        if let Some(tx) = reply {
            tx.send(exchange);
        }
    }
}

/// State of a (non pub/sub) seda consumer thread.
struct SedaWorker {
    queue: Arc<SedaQueue>,
    processor: Arc<dyn Processor>,
    ctx: WeakContext,
    logger: String,
    poll_timeout: Duration,
    spawner: ThreadSpawner,
    name: String,
    suspend: Arc<SuspendState>,
}

/// The consumer loop. Each exchange runs inside [`async_delay::run_detachable`]: when an
/// asynchronous delay (`asyncDelayed`, `asyncDelayedRedelivery`) releases this thread, a
/// replacement consumer thread takes over the queue and this thread exits after the exchange.
fn seda_worker(w: Arc<SedaWorker>, flag: Arc<StopFlag>) {
    let mut spun = false;
    let on_release = || {
        let (w2, f2) = (w.clone(), flag.clone());
        if let Err(e) = w.spawner.spawn(&w.name, f2, move |flag| seda_worker(w2, flag)) {
            crate::camel_warn!("seda", "Cannot start a replacement consumer thread for {}: {}", w.logger, e);
        }
    };
    loop {
        if flag.is_stopped() {
            break;
        }
        if w.suspend.get() {
            // suspended: leave the messages queued until resumed
            if w.suspend.wait_while_suspended(&flag) {
                break;
            }
            continue;
        }
        if let Some(item) = w.queue.try_pop() {
            spun = false;
            let ((), released) = async_delay::run_detachable_ref(&on_release, || dispatch(item, &w.queue, false, &w.processor, &w.ctx, &w.logger));
            if released {
                return;
            }
            continue;
        }
        if !spun {
            // empty: spin a little before parking (producers are usually microseconds away)
            w.queue.spin_until(|n| n > 0, Some(&flag));
            spun = true;
            continue;
        }
        spun = false;
        w.queue.park_taker(w.poll_timeout, Some(&flag));
    }
}

impl Service for SedaConsumer {
    fn start(&self) -> Result<()> {
        let mut started = self.started.lock().unwrap();
        if *started {
            return Ok(());
        }
        {
            let mut m = self.queue.members.lock().unwrap();
            m.consumers += 1;
            if self.config.multiple_consumers {
                m.subscribers.push((self.id, self.processor.clone(), self.ctx.clone()));
            }
        }
        for i in 0..self.config.concurrent_consumers {
            let (queue, processor, ctx) = (self.queue.clone(), self.processor.clone(), self.ctx.clone());
            let (multiple, poll_timeout) = (self.config.multiple_consumers, self.config.poll_timeout);
            let logger = self.uri.clone();
            let spawner = self.threads.spawner();
            let suspend = self.suspend.clone();
            self.threads.spawn(&format!("Camel {} #{}", self.uri, i + 1), move |flag| {
                if multiple {
                    // pub/sub: pop + fan-out under one lock to preserve ordering
                    while !flag.is_stopped() {
                        if suspend.wait_while_suspended(&flag) {
                            break;
                        }
                        let _guard = match queue.pubsub.try_lock() {
                            Ok(g) => g,
                            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
                            Err(std::sync::TryLockError::WouldBlock) => {
                                std::thread::sleep(Duration::from_millis(2));
                                continue;
                            }
                        };
                        let item = match queue.try_pop() {
                            Some(i) => Some(i),
                            None => {
                                queue.park_taker(poll_timeout.min(Duration::from_millis(100)), Some(&flag));
                                queue.try_pop()
                            }
                        };
                        if let Some(item) = item {
                            dispatch(item, &queue, multiple, &processor, &ctx, &logger);
                        }
                    }
                    return;
                }
                let name = std::thread::current().name().unwrap_or("Camel seda").to_string();
                seda_worker(Arc::new(SedaWorker { queue, processor, ctx, logger, poll_timeout, spawner, name, suspend }), flag);
            })?;
        }
        *started = true;
        Ok(())
    }

    fn as_suspendable(&self) -> Option<&dyn SuspendableService> {
        Some(&*self.suspend)
    }

    fn stop(&self) -> Result<()> {
        let mut started = self.started.lock().unwrap();
        if !*started {
            return Ok(());
        }
        // Graceful shutdown (Camel's ShutdownAware seda consumer): pending exchanges in the
        // queue are completed before the consumer stops, unless purgeWhenStopping=true.
        if !self.config.purge_when_stopping && !self.suspend.get() && !self.threads.is_current_thread() {
            let timeout = self.ctx.upgrade().map(|c| c.shutdown_timeout()).unwrap_or(Duration::from_secs(45));
            let deadline = std::time::Instant::now() + timeout;
            while self.queue.size() > 0 && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(2));
            }
            if self.queue.size() > 0 {
                crate::camel_warn!("seda", "Timeout waiting for {} pending exchanges on {} to complete", self.queue.size(), self.uri);
            }
        }
        self.threads.flag().stop();
        self.queue.wake_all();
        self.threads.stop();
        {
            let mut m = self.queue.members.lock().unwrap();
            m.consumers = m.consumers.saturating_sub(1);
            m.subscribers.retain(|s| s.0 != self.id);
        }
        if self.config.purge_when_stopping {
            self.queue.purge();
        }
        self.suspend.set(false);
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
                tx.send(ex.clone());
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

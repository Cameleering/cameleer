//! Asynchronous routing (Camel's asynchronous routing engine): exchanges of routes whose steps
//! can complete without holding a thread run as tasks.
//!
//! Design and measurements: `docs/design/async-routing.md`. A processor that can wait for I/O
//! without a thread offers [`AsyncProcessor`](crate::spi::AsyncProcessor) through
//! [`Processor::as_async`](crate::spi::Processor::as_async); containers (pipelines, the error
//! channel, the route) do so when a child does. Such a route's exchange is a [`spawn`]ed task: a
//! `std::future::Future` polled on the thread that wakes it (the event loop that read a reply),
//! so a completion costs no queue, no thread wake and no system call. This is Camel's "the
//! route continues on the I/O thread", with Camel's `ReactiveExecutor` trampoline done by a state
//! word: a wake during a poll makes the poller loop instead of recursing.
//!
//! Event loop threads must never block: code running there is marked with [`enter_loop`], a
//! pipeline moves to a worker ([`offload`]) before a step that may block, and [`block_on`] warns
//! when it is called on a loop.

use std::cell::{Cell, UnsafeCell};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll, Wake, Waker};

use crate::error::{CamelError, Result};
use crate::message::Exchange;
use crate::spi::{AsyncProcessor, ProcessFuture, Processor};

static ENABLED: OnceLock<AtomicBool> = OnceLock::new();

fn enabled_flag() -> &'static AtomicBool {
    // `CAMEL_ASYNC_ROUTING=false` builds every route on the synchronous path (a control for tests
    // and benchmarks, read once)
    ENABLED.get_or_init(|| AtomicBool::new(!matches!(std::env::var("CAMEL_ASYNC_ROUTING").as_deref(), Ok("false") | Ok("0"))))
}

/// Whether consumers may run async-capable routes as tasks (default true; `CAMEL_ASYNC_ROUTING=false`
/// or [`set_enabled`] turns it off). Consumers read it when they start.
pub fn enabled() -> bool {
    enabled_flag().load(Ordering::Relaxed)
}

/// Turns asynchronous routing on or off for consumers started afterwards (tests, benchmarks).
pub fn set_enabled(on: bool) {
    enabled_flag().store(on, Ordering::Relaxed);
}

/// The asynchronous form of `p`, when asynchronous routing is on and `p` has one.
pub fn async_of(p: &dyn Processor) -> Option<&dyn AsyncProcessor> {
    if enabled() {
        p.as_async()
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Event loop threads
// ---------------------------------------------------------------------------

thread_local! {
    static ON_LOOP: Cell<bool> = const { Cell::new(false) };
}

/// Marks the current thread as an event loop thread until the guard is dropped.
pub fn enter_loop() -> LoopGuard {
    LoopGuard { prev: ON_LOOP.with(|c| c.replace(true)) }
}

/// See [`enter_loop`].
pub struct LoopGuard {
    prev: bool,
}

impl Drop for LoopGuard {
    fn drop(&mut self) {
        let prev = self.prev;
        let _ = ON_LOOP.try_with(|c| c.set(prev));
    }
}

/// Whether the current thread runs inside [`enter_loop`].
pub fn on_loop() -> bool {
    ON_LOOP.with(|c| c.get())
}

// ---------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------

const IDLE: u8 = 0;
const POLLING: u8 = 1;
const NOTIFIED: u8 = 2;
const DONE: u8 = 3;

/// Called with the finished exchange, on the thread that finished it.
pub type OnDone = Box<dyn FnOnce(Box<Exchange>) + Send>;

struct Parts {
    // Declared first: dropped before what it borrows.
    fut: Option<ProcessFuture<'static>>,
    /// The exchange, `Box::into_raw`: borrowed mutably by `fut` while it lives.
    exchange: *mut Exchange,
    /// Keeps the processor that `fut` borrows alive.
    _processor: Arc<dyn Processor>,
    on_done: Option<OnDone>,
}

struct Task {
    state: AtomicU8,
    parts: UnsafeCell<Parts>,
}

// SAFETY: `parts` is only touched by the thread that moved `state` to POLLING (one at a time, the
// state word's acquire/release orders the accesses); the future, the exchange and the callback
// are `Send`.
unsafe impl Send for Task {}
unsafe impl Sync for Task {}

impl Wake for Task {
    fn wake(self: Arc<Self>) {
        Task::schedule(&self);
    }
    fn wake_by_ref(self: &Arc<Self>) {
        Task::schedule(self);
    }
}

impl Task {
    fn schedule(this: &Arc<Task>) {
        let mut s = this.state.load(Ordering::Acquire);
        loop {
            let next = match s {
                IDLE => POLLING,
                POLLING => NOTIFIED,
                _ => return,
            };
            match this.state.compare_exchange_weak(s, next, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) if next == POLLING => return Task::run(this),
                Ok(_) => return,
                Err(now) => s = now,
            }
        }
    }

    /// Polls until the future is pending with no wake in between, or done. The caller moved the
    /// state to POLLING.
    fn run(this: &Arc<Task>) {
        let waker = Waker::from(this.clone());
        let mut cx = Context::from_waker(&waker);
        loop {
            // SAFETY: this thread holds POLLING (see `unsafe impl Sync`).
            let parts = unsafe { &mut *this.parts.get() };
            let polled = match parts.fut.as_mut() {
                Some(f) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f.as_mut().poll(&mut cx))),
                None => return,
            };
            let result = match polled {
                Ok(Poll::Pending) => {
                    match this.state.compare_exchange(POLLING, IDLE, Ordering::AcqRel, Ordering::Acquire) {
                        Ok(_) => return,
                        // woken while polling: poll again
                        Err(_) => {
                            this.state.store(POLLING, Ordering::Release);
                            continue;
                        }
                    }
                }
                Ok(Poll::Ready(r)) => r,
                Err(panic) => Err(CamelError::new(crate::error::kinds::CAMEL_EXCHANGE, format!("A processor panicked: {}", panic_message(&panic)))),
            };
            // the future goes first: it borrows the exchange
            parts.fut = None;
            // SAFETY: the only borrow of the exchange (the future) is gone; it came from `Box::into_raw`.
            let mut exchange = unsafe { Box::from_raw(std::mem::replace(&mut parts.exchange, std::ptr::null_mut())) };
            if let Err(e) = result {
                exchange.set_exception(e);
            }
            let on_done = parts.on_done.take();
            this.state.store(DONE, Ordering::Release);
            if let Some(f) = on_done {
                f(exchange);
            }
            return;
        }
    }
}

impl Drop for Parts {
    fn drop(&mut self) {
        // a task dropped before it finished (its waker forgotten by a stopped component): the
        // future first, then the exchange it borrowed
        self.fut = None;
        if !self.exchange.is_null() {
            // SAFETY: as in `Task::run`.
            drop(unsafe { Box::from_raw(self.exchange) });
        }
    }
}

fn panic_message(p: &Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "unknown panic".into())
}

/// Runs `exchange` through `processor`'s asynchronous form as a task, polling it first on the
/// calling thread. `on_done` gets the exchange (failed if the route failed) on the thread that
/// finishes it. Falls back to the synchronous `process` when the processor has no asynchronous form.
pub fn spawn(processor: Arc<dyn Processor>, exchange: Box<Exchange>, on_done: OnDone) {
    if processor.as_async().is_none() {
        let mut exchange = exchange;
        if let Err(e) = processor.process(&mut exchange) {
            exchange.set_exception(e);
        }
        on_done(exchange);
        return;
    }
    let raw = Box::into_raw(exchange);
    // SAFETY: the processor is kept alive by `_processor` and the exchange by `raw` for as long as
    // the future lives (`Parts` drops the future first); nothing else touches either meanwhile.
    let fut: ProcessFuture<'static> = unsafe {
        let p: &'static dyn Processor = &*Arc::as_ptr(&processor);
        let ex: &'static mut Exchange = &mut *raw;
        p.as_async().expect("checked above").process_async(ex)
    };
    let task = Arc::new(Task { state: AtomicU8::new(POLLING), parts: UnsafeCell::new(Parts { fut: Some(fut), exchange: raw, _processor: processor, on_done: Some(on_done) }) });
    Task::run(&task);
}

// ---------------------------------------------------------------------------
// Synchronous callers
// ---------------------------------------------------------------------------

struct ThreadWaker {
    thread: std::thread::Thread,
    notified: AtomicBool,
}

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        if !self.notified.swap(true, Ordering::Release) {
            self.thread.unpark();
        }
    }
}

static WARNED_LOOP: AtomicBool = AtomicBool::new(false);

/// Runs `fut` to completion on the calling thread, parking while it waits (Camel's
/// `AsyncProcessorAwaitManager`, for synchronous callers of asynchronous processors).
pub fn block_on<F: Future>(fut: F) -> F::Output {
    if on_loop() && !WARNED_LOOP.swap(true, Ordering::Relaxed) {
        crate::camel_warn!("Reactive", "A synchronous call waits on an event loop thread: the loop stalls until it completes");
    }
    let mut fut = std::pin::pin!(fut);
    let w = Arc::new(ThreadWaker { thread: std::thread::current(), notified: AtomicBool::new(false) });
    let waker = Waker::from(w.clone());
    let mut cx = Context::from_waker(&waker);
    loop {
        if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
            return v;
        }
        while !w.notified.swap(false, Ordering::Acquire) {
            std::thread::park();
        }
    }
}

// ---------------------------------------------------------------------------
// Moving to a worker
// ---------------------------------------------------------------------------

fn workers() -> &'static Arc<crate::executor::ThreadPool> {
    static POOL: OnceLock<Arc<crate::executor::ThreadPool>> = OnceLock::new();
    POOL.get_or_init(|| crate::executor::ThreadPool::with_keep_alive("Camel (reactive) worker", 1000, std::time::Duration::from_secs(60)))
}

/// Continues the current task on a worker thread (a step that may block is next). Completes at
/// once when the task is not on an event loop thread.
pub fn offload() -> Offload {
    Offload { moved: false }
}

/// See [`offload`].
pub struct Offload {
    moved: bool,
}

impl Future for Offload {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.moved || !on_loop() {
            return Poll::Ready(());
        }
        self.moved = true;
        let waker = cx.waker().clone();
        if !workers().execute(move || waker.wake()) {
            // the pool is shut down: continue here
            return Poll::Ready(());
        }
        Poll::Pending
    }
}

/// Runs `p` from an asynchronous container: awaits its asynchronous form, or calls it, moving to
/// a worker first when it may block and the task is on an event loop.
pub async fn run_step(p: &dyn Processor, exchange: &mut Exchange) -> Result<()> {
    match p.as_async() {
        Some(a) => a.process_async(exchange).await,
        None => {
            if matches!(p.blocking(), crate::spi::Blocking::Maybe) {
                offload().await;
            }
            p.process(exchange)
        }
    }
}

// ---------------------------------------------------------------------------
// One-shot completion
// ---------------------------------------------------------------------------

struct OneshotState<T> {
    value: Option<T>,
    waker: Option<Waker>,
}

/// The sending half of [`oneshot`]: completes the receiver, from any thread.
pub struct Complete<T> {
    shared: Arc<Mutex<OneshotState<T>>>,
}

impl<T> Complete<T> {
    pub fn complete(self, v: T) {
        let waker = {
            let mut g = self.shared.lock().unwrap_or_else(|p| p.into_inner());
            g.value = Some(v);
            g.waker.take()
        };
        if let Some(w) = waker {
            w.wake();
        }
    }
}

/// The receiving half of [`oneshot`]: a future of the value.
pub struct Receive<T> {
    shared: Arc<Mutex<OneshotState<T>>>,
}

impl<T> Future for Receive<T> {
    type Output = T;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        let mut g = self.shared.lock().unwrap_or_else(|p| p.into_inner());
        match g.value.take() {
            Some(v) => Poll::Ready(v),
            None => {
                if !g.waker.as_ref().is_some_and(|w| w.will_wake(cx.waker())) {
                    g.waker = Some(cx.waker().clone());
                }
                Poll::Pending
            }
        }
    }
}

/// A one-shot channel: the bridge from a completion callback to a future.
pub fn oneshot<T>() -> (Complete<T>, Receive<T>) {
    let shared = Arc::new(Mutex::new(OneshotState { value: None, waker: None }));
    (Complete { shared: shared.clone() }, Receive { shared })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spi::Blocking;
    use std::sync::atomic::AtomicUsize;

    /// A step that completes from another thread after `ms`.
    struct Later(u64);
    impl Processor for Later {
        fn process(&self, ex: &mut Exchange) -> Result<()> {
            block_on(self.process_async(ex))
        }
        fn as_async(&self) -> Option<&dyn AsyncProcessor> {
            Some(self)
        }
    }
    impl AsyncProcessor for Later {
        fn process_async<'a>(&'a self, ex: &'a mut Exchange) -> ProcessFuture<'a> {
            let ms = self.0;
            Box::pin(async move {
                let (tx, rx) = oneshot();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(ms));
                    tx.complete(std::thread::current().id());
                });
                let tid = rx.await;
                ex.set_header("thread", format!("{:?}", tid));
                ex.set_header("count", ex.header("count").and_then(|v| v.as_i64()).unwrap_or(0) + 1);
                Ok(())
            })
        }
    }

    /// Completes synchronously, many times: must not grow the stack.
    struct Now;
    impl Processor for Now {
        fn process(&self, _: &mut Exchange) -> Result<()> {
            Ok(())
        }
        fn as_async(&self) -> Option<&dyn AsyncProcessor> {
            Some(self)
        }
    }
    impl AsyncProcessor for Now {
        fn process_async<'a>(&'a self, ex: &'a mut Exchange) -> ProcessFuture<'a> {
            Box::pin(async move {
                ex.set_header("count", ex.header("count").and_then(|v| v.as_i64()).unwrap_or(0) + 1);
                Ok(())
            })
        }
    }

    fn run(p: Arc<dyn Processor>) -> Box<Exchange> {
        let (tx, rx) = std::sync::mpsc::channel();
        spawn(p, Box::new(Exchange::new(None)), Box::new(move |ex| tx.send(ex).unwrap()));
        rx.recv_timeout(std::time::Duration::from_secs(10)).expect("the task finished")
    }

    #[test]
    fn a_pipeline_continues_on_the_thread_that_completes_a_step() {
        let p: Arc<dyn Processor> = Arc::new(crate::processor::pipeline::Pipeline::new(vec![Arc::new(Later(5)), Arc::new(Later(5))]));
        let ex = run(p);
        assert_eq!(ex.header("count").and_then(|v| v.as_i64()), Some(2));
        assert!(ex.exception.is_none());
        assert_ne!(ex.header("thread").map(|v| v.to_string()), Some(format!("{:?}", std::thread::current().id())));
    }

    #[test]
    fn synchronous_completions_do_not_recurse() {
        let steps: Vec<Arc<dyn Processor>> = (0..100_000).map(|_| Arc::new(Now) as Arc<dyn Processor>).collect();
        let p: Arc<dyn Processor> = Arc::new(crate::processor::pipeline::Pipeline::new(steps));
        // a small stack: a recursive engine would overflow it
        let ex = std::thread::Builder::new().stack_size(256 * 1024).spawn(move || run(p)).unwrap().join().unwrap();
        assert_eq!(ex.header("count").and_then(|v| v.as_i64()), Some(100_000));
    }

    #[test]
    fn a_panicking_step_fails_the_exchange() {
        struct Boom;
        impl Processor for Boom {
            fn process(&self, _: &mut Exchange) -> Result<()> {
                Ok(())
            }
            fn as_async(&self) -> Option<&dyn AsyncProcessor> {
                Some(self)
            }
        }
        impl AsyncProcessor for Boom {
            fn process_async<'a>(&'a self, _: &'a mut Exchange) -> ProcessFuture<'a> {
                Box::pin(async move { panic!("boom") })
            }
        }
        let p: Arc<dyn Processor> = Arc::new(crate::processor::pipeline::Pipeline::new(vec![Arc::new(Boom)]));
        let ex = run(p);
        assert!(ex.exception.as_ref().unwrap().message.contains("boom"));
    }

    #[test]
    fn a_blocking_step_moves_off_the_loop() {
        let seen = Arc::new(Mutex::new(None));
        let s = seen.clone();
        let blocking: Arc<dyn Processor> = Arc::new(move |_: &mut Exchange| {
            *s.lock().unwrap() = Some(on_loop());
            Ok(())
        });
        assert!(matches!(blocking.blocking(), Blocking::Maybe));
        let p: Arc<dyn Processor> = Arc::new(crate::processor::pipeline::Pipeline::new(vec![Arc::new(Now), blocking]));
        let (tx, rx) = std::sync::mpsc::channel();
        {
            let _g = enter_loop();
            spawn(p, Box::new(Exchange::new(None)), Box::new(move |ex| tx.send(ex).unwrap()));
        }
        let ex = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        assert!(ex.exception.is_none());
        assert_eq!(*seen.lock().unwrap(), Some(false), "the closure ran on a worker, not on the loop");
    }

    #[test]
    fn block_on_waits_for_a_completion_from_another_thread() {
        let n = Arc::new(AtomicUsize::new(0));
        let mut ex = Exchange::new(None);
        block_on(Later(5).process_async(&mut ex)).unwrap();
        n.fetch_add(1, Ordering::SeqCst);
        assert_eq!(ex.header("count").and_then(|v| v.as_i64()), Some(1));
    }
}

//! Thread pools (Camel `ExecutorServiceManager` equivalent, std-only).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

type Job = Box<dyn FnOnce() + Send + 'static>;

/// A fixed size thread pool.
pub struct ThreadPool {
    name: String,
    sender: Mutex<Option<Sender<Job>>>,
    workers: Mutex<Vec<JoinHandle<()>>>,
    active: Arc<AtomicUsize>,
    pending: Arc<AtomicUsize>,
    shutdown: AtomicBool,
}

impl ThreadPool {
    /// Create a pool with `size` worker threads named `<name>-<n>`.
    pub fn new(name: &str, size: usize) -> Arc<ThreadPool> {
        let size = size.max(1);
        let (tx, rx) = channel::<Job>();
        let rx = Arc::new(Mutex::new(rx));
        let active = Arc::new(AtomicUsize::new(0));
        let pending = Arc::new(AtomicUsize::new(0));
        let mut workers = Vec::with_capacity(size);
        for i in 0..size {
            let rx: Arc<Mutex<Receiver<Job>>> = rx.clone();
            let active = active.clone();
            let pending = pending.clone();
            let h = std::thread::Builder::new()
                .name(format!("{}-{}", name, i + 1))
                .spawn(move || loop {
                    let job = { rx.lock().unwrap().recv() };
                    match job {
                        Ok(job) => {
                            pending.fetch_sub(1, Ordering::SeqCst);
                            active.fetch_add(1, Ordering::SeqCst);
                            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                            active.fetch_sub(1, Ordering::SeqCst);
                        }
                        Err(_) => break,
                    }
                })
                .expect("failed to spawn thread");
            workers.push(h);
        }
        Arc::new(ThreadPool {
            name: name.to_string(),
            sender: Mutex::new(Some(tx)),
            workers: Mutex::new(workers),
            active,
            pending,
            shutdown: AtomicBool::new(false),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Submit a job. Returns false if the pool is shut down.
    pub fn execute(&self, f: impl FnOnce() + Send + 'static) -> bool {
        if self.shutdown.load(Ordering::SeqCst) {
            return false;
        }
        match self.sender.lock().unwrap().as_ref() {
            Some(tx) => {
                self.pending.fetch_add(1, Ordering::SeqCst);
                tx.send(Box::new(f)).is_ok()
            }
            None => false,
        }
    }

    /// Submit a job and get a handle to wait for its result.
    pub fn submit<T: Send + 'static>(&self, f: impl FnOnce() -> T + Send + 'static) -> Future<T> {
        let fut = Future::new();
        let f2 = fut.clone();
        if !self.execute(move || f2.complete(f())) {
            fut.cancel();
        }
        fut
    }

    pub fn active_count(&self) -> usize {
        self.active.load(Ordering::SeqCst)
    }

    pub fn pending_count(&self) -> usize {
        self.pending.load(Ordering::SeqCst)
    }

    /// Stop accepting jobs and wait for workers to finish queued jobs.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        self.sender.lock().unwrap().take();
        let workers: Vec<JoinHandle<()>> = std::mem::take(&mut *self.workers.lock().unwrap());
        let me = std::thread::current().id();
        for w in workers {
            if w.thread().id() != me {
                let _ = w.join();
            }
        }
    }

    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }
}

impl Drop for ThreadPool {
    fn drop(&mut self) {
        self.sender.lock().unwrap().take();
    }
}

/// A simple completable future.
pub struct Future<T> {
    inner: Arc<(Mutex<FutureState<T>>, Condvar)>,
}

enum FutureState<T> {
    Pending,
    Done(T),
    Taken,
    Cancelled,
}

impl<T> Clone for Future<T> {
    fn clone(&self) -> Self {
        Future { inner: self.inner.clone() }
    }
}

impl<T> Default for Future<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Future<T> {
    pub fn new() -> Self {
        Future { inner: Arc::new((Mutex::new(FutureState::Pending), Condvar::new())) }
    }
    pub fn complete(&self, v: T) {
        let mut s = self.inner.0.lock().unwrap();
        if matches!(*s, FutureState::Pending) {
            *s = FutureState::Done(v);
        }
        self.inner.1.notify_all();
    }
    pub fn cancel(&self) {
        let mut s = self.inner.0.lock().unwrap();
        if matches!(*s, FutureState::Pending) {
            *s = FutureState::Cancelled;
        }
        self.inner.1.notify_all();
    }
    pub fn is_done(&self) -> bool {
        !matches!(*self.inner.0.lock().unwrap(), FutureState::Pending)
    }
    /// Wait for the value. Returns None if cancelled or already taken.
    pub fn get(&self) -> Option<T> {
        let mut s = self.inner.0.lock().unwrap();
        while matches!(*s, FutureState::Pending) {
            s = self.inner.1.wait(s).unwrap();
        }
        match std::mem::replace(&mut *s, FutureState::Taken) {
            FutureState::Done(v) => Some(v),
            _ => None,
        }
    }
    /// Wait at most `timeout`. Returns None on timeout/cancel.
    pub fn get_timeout(&self, timeout: std::time::Duration) -> Option<T> {
        let deadline = std::time::Instant::now() + timeout;
        let mut s = self.inner.0.lock().unwrap();
        while matches!(*s, FutureState::Pending) {
            let now = std::time::Instant::now();
            if now >= deadline {
                return None;
            }
            s = self.inner.1.wait_timeout(s, deadline - now).unwrap().0;
        }
        match std::mem::replace(&mut *s, FutureState::Taken) {
            FutureState::Done(v) => Some(v),
            _ => None,
        }
    }
}


// ---------------------------------------------------------------------------
// Bounded pool (java.util.concurrent.ThreadPoolExecutor semantics)
// ---------------------------------------------------------------------------

/// What a [`BoundedThreadPool`] does with a task it cannot accept (Camel
/// `ThreadPoolRejectedPolicy`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectedPolicy {
    /// Reject the task: the caller gets a `RejectedExecutionException`.
    Abort,
    /// Run the task in the caller's thread.
    CallerRuns,
    /// Silently drop the new task (its rejection callback runs).
    Discard,
    /// Drop the oldest queued task (its rejection callback runs) and queue the new one.
    DiscardOldest,
}

impl RejectedPolicy {
    pub fn parse(s: &str) -> crate::error::Result<Self> {
        Ok(match s.trim().to_ascii_lowercase().replace(['_', '-'], "").as_str() {
            "abort" => RejectedPolicy::Abort,
            "callerruns" => RejectedPolicy::CallerRuns,
            "discard" => RejectedPolicy::Discard,
            "discardoldest" => RejectedPolicy::DiscardOldest,
            _ => return Err(crate::error::CamelError::illegal_argument(format!("Unknown rejectedPolicy: {}", s))),
        })
    }
}

/// Configuration of a [`BoundedThreadPool`] (Camel `ThreadPoolProfile`). Defaults are
/// Camel's default profile: poolSize 10, maxPoolSize 20, keepAlive 60s, maxQueueSize 1000,
/// CallerRuns.
#[derive(Clone, Debug)]
pub struct ThreadPoolProfile {
    pub name: String,
    pub pool_size: usize,
    pub max_pool_size: usize,
    pub keep_alive: std::time::Duration,
    /// Maximum queued tasks; `0` = direct hand-off (SynchronousQueue), negative = unbounded.
    pub max_queue_size: i64,
    pub allow_core_thread_timeout: bool,
    pub rejected_policy: RejectedPolicy,
}

impl Default for ThreadPoolProfile {
    fn default() -> Self {
        ThreadPoolProfile {
            name: "Camel Thread".into(),
            pool_size: 10,
            max_pool_size: 20,
            keep_alive: std::time::Duration::from_secs(60),
            max_queue_size: 1000,
            allow_core_thread_timeout: false,
            rejected_policy: RejectedPolicy::CallerRuns,
        }
    }
}

/// A task with an optional rejection callback (Camel `Rejectable`).
pub struct Task {
    run: Job,
    reject: Option<Job>,
}

impl Task {
    pub fn new(run: impl FnOnce() + Send + 'static) -> Self {
        Task { run: Box::new(run), reject: None }
    }
    /// A callback invoked instead of `run` when the task is discarded.
    pub fn on_reject(mut self, f: impl FnOnce() + Send + 'static) -> Self {
        self.reject = Some(Box::new(f));
        self
    }
    fn rejected(self) {
        if let Some(r) = self.reject {
            r();
        }
    }
}

/// The outcome of [`BoundedThreadPool::submit_task`].
pub enum Submission {
    /// Queued or handed to a worker.
    Accepted,
    /// Ran in the calling thread (CallerRuns).
    RanInCaller,
    /// Discarded (Discard / DiscardOldest): the rejection callback of the dropped task ran.
    Discarded,
    /// Rejected (Abort, or the pool is shut down): the task is handed back.
    Rejected(Task),
}

struct BoundedState {
    queue: std::collections::VecDeque<Task>,
    workers: usize,
    idle: usize,
    shutdown: bool,
    next_id: usize,
    completed: u64,
    largest: usize,
}

struct BoundedShared {
    profile: ThreadPoolProfile,
    state: Mutex<BoundedState>,
    cond: Condvar,
    handles: Mutex<Vec<JoinHandle<()>>>,
    active: AtomicUsize,
}

/// A thread pool with core/max sizes, keep-alive, a bounded queue and a rejection policy
/// (Java `ThreadPoolExecutor` semantics: core threads first, then the queue, then extra
/// threads up to the maximum, then the rejection policy).
pub struct BoundedThreadPool {
    shared: Arc<BoundedShared>,
}

impl BoundedThreadPool {
    pub fn new(profile: ThreadPoolProfile) -> Arc<BoundedThreadPool> {
        let mut profile = profile;
        profile.max_pool_size = profile.max_pool_size.max(profile.pool_size).max(1);
        Arc::new(BoundedThreadPool {
            shared: Arc::new(BoundedShared {
                profile,
                state: Mutex::new(BoundedState {
                    queue: Default::default(),
                    workers: 0,
                    idle: 0,
                    shutdown: false,
                    next_id: 0,
                    completed: 0,
                    largest: 0,
                }),
                cond: Condvar::new(),
                handles: Mutex::new(Vec::new()),
                active: AtomicUsize::new(0),
            }),
        })
    }

    pub fn profile(&self) -> &ThreadPoolProfile {
        &self.shared.profile
    }

    fn spawn_worker(&self, st: &mut BoundedState, first: Option<Task>) -> bool {
        st.workers += 1;
        st.next_id += 1;
        st.largest = st.largest.max(st.workers);
        let shared = self.shared.clone();
        let name = format!("{} #{}", shared.profile.name, st.next_id);
        match std::thread::Builder::new().name(name).spawn(move || worker_loop(shared, first)) {
            Ok(h) => {
                let mut hs = self.shared.handles.lock().unwrap();
                hs.retain(|h| !h.is_finished());
                hs.push(h);
                true
            }
            Err(_) => {
                st.workers -= 1;
                false
            }
        }
    }

    /// Submit a task, applying the rejection policy when it cannot be accepted.
    pub fn submit_task(&self, task: Task) -> Submission {
        let p = &self.shared.profile;
        let mut st = self.shared.state.lock().unwrap();
        if st.shutdown {
            return Submission::Rejected(task);
        }
        if st.workers < p.pool_size {
            return if self.spawn_worker(&mut st, Some(task)) { Submission::Accepted } else { Submission::Rejected(Task::new(|| {})) };
        }
        let queue_ok = match p.max_queue_size {
            n if n < 0 => true,
            0 => st.idle > st.queue.len(),
            n => st.queue.len() < n as usize,
        };
        if queue_ok {
            st.queue.push_back(task);
            self.shared.cond.notify_one();
            return Submission::Accepted;
        }
        if st.workers < p.max_pool_size {
            return if self.spawn_worker(&mut st, Some(task)) { Submission::Accepted } else { Submission::Rejected(Task::new(|| {})) };
        }
        match p.rejected_policy {
            RejectedPolicy::Abort => Submission::Rejected(task),
            RejectedPolicy::CallerRuns => {
                drop(st);
                (task.run)();
                Submission::RanInCaller
            }
            RejectedPolicy::Discard => {
                drop(st);
                task.rejected();
                Submission::Discarded
            }
            RejectedPolicy::DiscardOldest => {
                let oldest = st.queue.pop_front();
                if p.max_queue_size != 0 {
                    st.queue.push_back(task);
                    self.shared.cond.notify_one();
                    drop(st);
                    if let Some(o) = oldest {
                        o.rejected();
                    }
                } else {
                    drop(st);
                    task.rejected();
                }
                Submission::Discarded
            }
        }
    }

    /// Number of live worker threads.
    pub fn pool_size(&self) -> usize {
        self.shared.state.lock().unwrap().workers
    }
    /// Largest number of worker threads seen.
    pub fn largest_pool_size(&self) -> usize {
        self.shared.state.lock().unwrap().largest
    }
    pub fn active_count(&self) -> usize {
        self.shared.active.load(Ordering::SeqCst)
    }
    pub fn queue_size(&self) -> usize {
        self.shared.state.lock().unwrap().queue.len()
    }
    pub fn completed_task_count(&self) -> u64 {
        self.shared.state.lock().unwrap().completed
    }
    pub fn is_shutdown(&self) -> bool {
        self.shared.state.lock().unwrap().shutdown
    }

    /// Stop accepting tasks, let queued tasks finish and join the workers.
    pub fn shutdown(&self) {
        self.shared.state.lock().unwrap().shutdown = true;
        self.shared.cond.notify_all();
        let hs: Vec<JoinHandle<()>> = std::mem::take(&mut *self.shared.handles.lock().unwrap());
        let me = std::thread::current().id();
        for h in hs {
            if h.thread().id() != me {
                let _ = h.join();
            }
        }
    }
}

impl Drop for BoundedThreadPool {
    fn drop(&mut self) {
        self.shared.state.lock().unwrap().shutdown = true;
        self.shared.cond.notify_all();
    }
}

fn worker_loop(shared: Arc<BoundedShared>, mut first: Option<Task>) {
    loop {
        let task = match first.take() {
            Some(t) => t,
            None => {
                let mut st = shared.state.lock().unwrap();
                loop {
                    if let Some(t) = st.queue.pop_front() {
                        break t;
                    }
                    if st.shutdown {
                        st.workers -= 1;
                        return;
                    }
                    let timed = st.workers > shared.profile.pool_size || shared.profile.allow_core_thread_timeout;
                    st.idle += 1;
                    if timed {
                        let (g, to) = shared.cond.wait_timeout(st, shared.profile.keep_alive).unwrap();
                        st = g;
                        st.idle -= 1;
                        let may_exit = st.workers > shared.profile.pool_size || shared.profile.allow_core_thread_timeout;
                        if to.timed_out() && st.queue.is_empty() && may_exit {
                            st.workers -= 1;
                            return;
                        }
                    } else {
                        st = shared.cond.wait(st).unwrap();
                        st.idle -= 1;
                    }
                }
            }
        };
        shared.active.fetch_add(1, Ordering::SeqCst);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(task.run));
        shared.active.fetch_sub(1, Ordering::SeqCst);
        shared.state.lock().unwrap().completed += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pool_runs_jobs() {
        let pool = ThreadPool::new("t", 3);
        let futs: Vec<Future<usize>> = (0..10).map(|i| pool.submit(move || i * 2)).collect();
        let total: usize = futs.iter().map(|f| f.get().unwrap()).sum();
        assert_eq!(total, 90);
        pool.shutdown();
        assert!(!pool.execute(|| {}));
    }

    fn profile(core: usize, max: usize, queue: i64, policy: RejectedPolicy) -> ThreadPoolProfile {
        ThreadPoolProfile {
            name: "b".into(),
            pool_size: core,
            max_pool_size: max,
            keep_alive: std::time::Duration::from_millis(50),
            max_queue_size: queue,
            allow_core_thread_timeout: false,
            rejected_policy: policy,
        }
    }

    /// Fill a pool with blocked tasks; returns the gate releasing them.
    fn block(pool: &BoundedThreadPool, n: usize) -> Arc<(Mutex<bool>, Condvar)> {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        for _ in 0..n {
            let g = gate.clone();
            assert!(matches!(
                pool.submit_task(Task::new(move || {
                    let mut open = g.0.lock().unwrap();
                    while !*open {
                        open = g.1.wait(open).unwrap();
                    }
                })),
                Submission::Accepted
            ));
        }
        gate
    }

    fn open(g: &Arc<(Mutex<bool>, Condvar)>) {
        *g.0.lock().unwrap() = true;
        g.1.notify_all();
    }

    #[test]
    fn bounded_pool_grows_to_max_then_rejects() {
        let pool = BoundedThreadPool::new(profile(1, 2, 1, RejectedPolicy::Abort));
        // core thread, queue slot, extra thread
        let gate = block(&pool, 3);
        assert_eq!(pool.pool_size(), 2);
        assert_eq!(pool.queue_size(), 1);
        assert!(matches!(pool.submit_task(Task::new(|| {})), Submission::Rejected(_)));
        open(&gate);
        pool.shutdown();
        assert_eq!(pool.completed_task_count(), 3);
        assert_eq!(pool.largest_pool_size(), 2);
    }

    #[test]
    fn rejection_policies() {
        let pool = BoundedThreadPool::new(profile(1, 1, 1, RejectedPolicy::CallerRuns));
        let gate = block(&pool, 2);
        let me = std::thread::current().id();
        let ran = Arc::new(Mutex::new(None));
        let r2 = ran.clone();
        assert!(matches!(pool.submit_task(Task::new(move || *r2.lock().unwrap() = Some(std::thread::current().id()))), Submission::RanInCaller));
        assert_eq!(*ran.lock().unwrap(), Some(me));
        open(&gate);
        pool.shutdown();

        let pool = BoundedThreadPool::new(profile(1, 1, 1, RejectedPolicy::Discard));
        let gate = block(&pool, 2);
        let rejected = Arc::new(AtomicUsize::new(0));
        let r = rejected.clone();
        assert!(matches!(pool.submit_task(Task::new(|| panic!("discarded")).on_reject(move || {
            r.fetch_add(1, Ordering::SeqCst);
        })), Submission::Discarded));
        assert_eq!(rejected.load(Ordering::SeqCst), 1);
        open(&gate);
        pool.shutdown();

        let pool = BoundedThreadPool::new(profile(1, 1, 1, RejectedPolicy::DiscardOldest));
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let g = gate.clone();
        pool.submit_task(Task::new(move || {
            let mut o = g.0.lock().unwrap();
            while !*o {
                o = g.1.wait(o).unwrap();
            }
        }));
        let order = Arc::new(Mutex::new(Vec::new()));
        let (o1, o1r, o2) = (order.clone(), order.clone(), order.clone());
        pool.submit_task(Task::new(move || o1.lock().unwrap().push("old")).on_reject(move || o1r.lock().unwrap().push("old-rejected")));
        assert!(matches!(pool.submit_task(Task::new(move || o2.lock().unwrap().push("new"))), Submission::Discarded));
        open(&gate);
        pool.shutdown();
        assert_eq!(*order.lock().unwrap(), vec!["old-rejected", "new"]);
    }

    #[test]
    fn extra_threads_time_out() {
        let pool = BoundedThreadPool::new(profile(1, 3, 0, RejectedPolicy::Abort));
        let gate = block(&pool, 3);
        assert_eq!(pool.pool_size(), 3);
        open(&gate);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while pool.pool_size() > 1 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(pool.pool_size(), 1);
        pool.shutdown();
    }
}

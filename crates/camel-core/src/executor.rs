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
}

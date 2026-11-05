//! Asynchronous delays: releasing the consumer thread while an exchange waits.
//!
//! In Apache Camel, `delay`/`throttle` with `asyncDelayed=true` and redelivery with
//! `asyncDelayedRedelivery=true` schedule the rest of the exchange's processing on a scheduler
//! thread pool, so the consumer that picked up the exchange (seda, timer...) can take the next
//! message straight away. Synchronous callers (direct endpoints called by a `ProducerTemplate`)
//! still wait for the exchange to complete.
//!
//! rust-camel processors are synchronous (the rest of the route is the call stack of the
//! current thread), so the equivalent is a thread hand-off: asynchronous consumers run each
//! exchange inside [`run_detachable`]. When an async-delayed processor calls [`async_sleep`],
//! the consumer is *released*: a replacement consumer thread is started at once (the consumer
//! keeps taking messages without waiting), and the current thread becomes a delay thread that
//! waits, finishes the exchange and then exits. Exchanges processed on any other thread
//! (direct calls from a `ProducerTemplate`, parallel split workers...) wait synchronously, which
//! is Camel's behaviour for synchronous callers.
//!
//! The number of threads released at the same time is bounded by
//! [`MAX_RELEASED_THREADS`]; beyond it delays are applied on the calling thread.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Maximum number of consumer threads released (waiting) at the same time.
pub const MAX_RELEASED_THREADS: usize = 1000;

static RELEASED: AtomicUsize = AtomicUsize::new(0);

/// What starts a replacement consumer: owned, or borrowed from the consumer loop's stack
/// for the duration of one exchange (no allocation per exchange).
enum Hook {
    Owned(Box<dyn FnOnce()>),
    Borrowed(*const (dyn Fn() + 'static)),
}

impl Hook {
    fn call(self) {
        match self {
            Hook::Owned(f) => f(),
            // SAFETY: a borrowed hook is only installed while `run_detachable_ref` runs on
            // this thread, and removed (also on unwinding) before the borrow ends.
            Hook::Borrowed(f) => unsafe { (*f)() },
        }
    }
}

/// Restores the previous hook and release flag when `run_*` returns or unwinds.
struct Restore {
    prev_hook: Option<Option<Hook>>,
    prev_released: bool,
}

impl Drop for Restore {
    fn drop(&mut self) {
        let prev = self.prev_hook.take().flatten();
        let _ = HOOK.try_with(|h| *h.borrow_mut() = prev);
        if let Ok(true) = RELEASED_HERE.try_with(|r| r.replace(self.prev_released)) {
            RELEASED.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

thread_local! {
    static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
    static RELEASED_HERE: Cell<bool> = const { Cell::new(false) };
}

/// Run `f` (the processing of one exchange by an asynchronous consumer). If an async delay
/// releases the consumer during `f`, `on_release` is called once at that moment (it must start
/// a replacement consumer thread) and `true` is returned: the calling thread should finish its
/// current work and exit its consumer loop.
pub fn run_detachable<R>(on_release: impl FnOnce() + 'static, f: impl FnOnce() -> R) -> (R, bool) {
    run_with(Hook::Owned(Box::new(on_release)), f)
}

/// [`run_detachable`] with a hook the consumer loop keeps for all its exchanges.
pub fn run_detachable_ref<R>(on_release: &dyn Fn(), f: impl FnOnce() -> R) -> (R, bool) {
    // SAFETY: only the lifetime is erased; `run_with` removes the hook before returning
    // (also when `f` unwinds), so it is never called after `on_release` is gone.
    let hook: *const (dyn Fn() + 'static) = unsafe { std::mem::transmute::<*const dyn Fn(), *const (dyn Fn() + 'static)>(on_release) };
    run_with(Hook::Borrowed(hook), f)
}

fn run_with<R>(hook: Hook, f: impl FnOnce() -> R) -> (R, bool) {
    let mut restore = Restore {
        prev_hook: Some(HOOK.with(|h| h.borrow_mut().replace(hook))),
        prev_released: RELEASED_HERE.with(|r| r.replace(false)),
    };
    let result = f();
    let released = RELEASED_HERE.with(|r| r.get());
    let prev = restore.prev_hook.take().flatten();
    HOOK.with(|h| *h.borrow_mut() = prev);
    RELEASED_HERE.with(|r| r.set(restore.prev_released));
    std::mem::forget(restore);
    if released {
        RELEASED.fetch_sub(1, Ordering::SeqCst);
    }
    (result, released)
}

/// Whether the current thread runs an exchange for an asynchronous consumer that can be
/// released (or was already released).
pub fn can_release() -> bool {
    RELEASED_HERE.with(|r| r.get()) || HOOK.with(|h| h.borrow().is_some())
}

/// Release the consumer thread (see the module docs). Returns true when the current thread
/// is (now or already) released; false when the exchange is processed synchronously.
pub fn release_consumer() -> bool {
    if RELEASED_HERE.with(|r| r.get()) {
        return true;
    }
    let Some(hook) = HOOK.with(|h| h.borrow_mut().take()) else {
        return false;
    };
    if RELEASED.fetch_add(1, Ordering::SeqCst) >= MAX_RELEASED_THREADS {
        RELEASED.fetch_sub(1, Ordering::SeqCst);
        HOOK.with(|h| *h.borrow_mut() = Some(hook));
        return false;
    }
    RELEASED_HERE.with(|r| r.set(true));
    hook.call();
    true
}

/// Wait for `d` as an asynchronous delay: the consumer thread is released first when
/// possible. Returns true when the wait happened asynchronously.
pub fn async_sleep(d: Duration) -> bool {
    let released = release_consumer();
    if !d.is_zero() {
        std::thread::sleep(d);
    }
    released
}

/// Number of threads currently released by asynchronous delays.
pub fn released_threads() -> usize {
    RELEASED.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn a_borrowed_hook_releases_once_and_is_removed_afterwards() {
        let calls = Cell::new(0);
        let hook = || calls.set(calls.get() + 1);
        let ((), released) = run_detachable_ref(&hook, || {
            assert!(can_release());
            assert!(async_sleep(Duration::ZERO));
            assert!(async_sleep(Duration::ZERO));
        });
        assert!(released);
        assert_eq!(calls.get(), 1);
        assert!(!can_release());
        // not released: the hook is not called
        let ((), released) = run_detachable_ref(&hook, || ());
        assert!(!released);
        assert_eq!(calls.get(), 1);
        // the hook is removed also when the exchange panics
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_detachable_ref(&hook, || panic!("boom"))));
        assert!(r.is_err());
        assert!(!can_release());
        assert!(!release_consumer());
    }

    #[test]
    fn release_runs_hook_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let (v, released) = run_detachable(
            move || {
                c.fetch_add(1, Ordering::SeqCst);
            },
            || {
                assert!(can_release());
                assert!(async_sleep(Duration::ZERO));
                assert!(async_sleep(Duration::from_millis(1)));
                42
            },
        );
        assert_eq!(v, 42);
        assert!(released);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // outside an async consumer: synchronous
        assert!(!can_release());
        assert!(!async_sleep(Duration::ZERO));
        let (_, released) = run_detachable(|| {}, || ());
        assert!(!released);
    }
}

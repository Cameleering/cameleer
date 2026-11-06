//! How much stack the current thread has left, for recursive evaluators (scripts, SQL
//! routines) that must fail cleanly instead of overflowing. The thread's stack bounds come
//! from `pthread_getattr_np` on Linux and `GetCurrentThreadStackLimits` on Windows; other
//! platforms assume 1 MiB below the first measurement point.

use std::cell::Cell;

thread_local! {
    static LOW: Cell<usize> = const { Cell::new(0) };
}

#[inline(never)]
fn sp() -> usize {
    let marker = 0u8;
    std::hint::black_box(&marker) as *const u8 as usize
}

#[cfg(target_os = "linux")]
fn stack_low() -> Option<usize> {
    #[repr(C, align(16))]
    struct Attr([u8; 128]);
    extern "C" {
        fn pthread_self() -> usize;
        fn pthread_getattr_np(thread: usize, attr: *mut Attr) -> i32;
        fn pthread_attr_getstack(attr: *const Attr, addr: *mut *mut u8, size: *mut usize) -> i32;
        fn pthread_attr_destroy(attr: *mut Attr) -> i32;
    }
    let mut attr = Attr([0; 128]);
    let mut addr: *mut u8 = std::ptr::null_mut();
    let mut size = 0usize;
    // SAFETY: plain libc calls on a zeroed, sufficiently large and aligned attribute buffer
    // (pthread_attr_t is 56 bytes on glibc / musl x86_64 and aarch64); the attribute is
    // destroyed after use.
    unsafe {
        if pthread_getattr_np(pthread_self(), &mut attr) != 0 {
            return None;
        }
        let r = pthread_attr_getstack(&attr, &mut addr, &mut size);
        pthread_attr_destroy(&mut attr);
        if r != 0 || addr.is_null() {
            return None;
        }
    }
    Some(addr as usize)
}

#[cfg(windows)]
fn stack_low() -> Option<usize> {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentThreadStackLimits(low: *mut usize, high: *mut usize);
    }
    let (mut low, mut high) = (0usize, 0usize);
    // SAFETY: writes the current thread's stack reservation bounds into the two locals.
    unsafe { GetCurrentThreadStackLimits(&mut low, &mut high) };
    // the lowest pages of the reservation are guard pages
    (low != 0).then_some(low + 64 * 1024)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn stack_low() -> Option<usize> {
    None
}

fn low() -> usize {
    LOW.with(|l| {
        if l.get() == 0 {
            let v = stack_low().unwrap_or_else(|| sp().saturating_sub(1 << 20));
            l.set(v.max(1));
        }
        l.get()
    })
}

/// Bytes of stack left on the current thread (approximate).
pub fn remaining() -> usize {
    sp().saturating_sub(low())
}

/// Run `f` on the current thread when at least `red_zone` bytes of stack are left, otherwise
/// on a new stack segment (see [`on_new_stack`]). Recursive evaluators call this at their
/// recursion points, so ordinary evaluations never leave the caller's thread and only deep
/// recursion pays for a thread.
///
/// # Safety
/// As for [`on_new_stack`].
#[inline]
pub unsafe fn maybe_grow<T>(red_zone: usize, segment: usize, f: impl FnOnce() -> T) -> T {
    if remaining() >= red_zone {
        f()
    } else {
        // SAFETY: forwarded to the caller.
        unsafe { on_new_stack(segment, f) }
    }
}

struct AssertSend<T>(T);
// SAFETY: see `on_new_stack`: the value moves to a helper thread while the thread that owns it
// is blocked joining that helper, so it is never used by two threads at once; spawn and join
// order the accesses.
unsafe impl<T> Send for AssertSend<T> {}

impl<T> AssertSend<T> {
    fn into_inner(self) -> T {
        self.0
    }
}

/// Run `f` on a new thread with a `segment`-byte stack while the current thread waits for it,
/// and return its result (a panic in `f` resumes in the caller). When no thread can be
/// started, `f` runs on the current thread.
///
/// # Safety
/// `f` may capture values that are not `Send` (`Rc`, `Cell`, references to `RefCell`s): that is
/// sound only because the calling thread is blocked until `f` returns, so they are never used
/// by two threads at once. `f` must not depend on which thread runs it: it must not release
/// lock guards taken before the call, and it sees the new thread's thread-locals, not the
/// caller's.
pub unsafe fn on_new_stack<T, F: FnOnce() -> T>(segment: usize, f: F) -> T {
    let mut slot = Some(f);
    let job = AssertSend(&mut slot as *mut Option<F>);
    let r = std::thread::scope(|s| {
        std::thread::Builder::new()
            .name("cameleer-stack".into())
            .stack_size(segment)
            .spawn_scoped(s, move || {
                let job = job.into_inner();
                // SAFETY: `slot` outlives the scope and the owning thread does not touch it
                // until the scoped thread has been joined.
                let f = unsafe { (*job).take() }.expect("job");
                AssertSend(f())
            })
            .map(|h| h.join())
    });
    match r {
        Ok(Ok(v)) => v.into_inner(),
        Ok(Err(panic)) => std::panic::resume_unwind(panic),
        Err(_) => (slot.take().expect("job"))(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remaining_shrinks_with_depth_and_fits_the_thread() {
        fn deeper(n: u32) -> usize {
            let pad = std::hint::black_box([0u8; 4096]);
            if n == 0 {
                remaining()
            } else {
                deeper(n - 1) + pad[0] as usize
            }
        }
        let size = 4 << 20;
        let (top, deep) = std::thread::Builder::new().stack_size(size).spawn(|| (remaining(), deeper(64))).unwrap().join().unwrap();
        assert!(top <= size && top > size / 2, "top {top}");
        assert!(top - deep >= 64 * 4096, "top {top} deep {deep}");
    }

    #[test]
    fn maybe_grow_stays_on_the_caller_until_the_stack_is_low() {
        use std::rc::Rc;
        // an `Rc` (not Send) is used from the segments while the caller waits
        fn deep(n: usize, c: &Rc<std::cell::Cell<usize>>, threads: &mut Vec<std::thread::ThreadId>) -> usize {
            let pad = std::hint::black_box([0u8; 2048]);
            c.set(c.get() + 1);
            let me = std::thread::current().id();
            if !threads.contains(&me) {
                threads.push(me);
            }
            if n == 0 {
                return pad[0] as usize;
            }
            // SAFETY: the closure only touches state of this (blocked) caller.
            unsafe { maybe_grow(64 * 1024, 1 << 20, || deep(n - 1, c, threads)) + pad[1] as usize }
        }
        let shallow = std::thread::Builder::new()
            .stack_size(1 << 20)
            .spawn(|| {
                let c = Rc::new(std::cell::Cell::new(0));
                let mut t = Vec::new();
                deep(10, &c, &mut t);
                t.len()
            })
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(shallow, 1, "a shallow recursion stays on its thread");
        // 2000 levels of >= 2 KiB do not fit in 1 MiB: segments are added as needed
        std::thread::Builder::new()
            .stack_size(1 << 20)
            .spawn(|| {
                let counter = Rc::new(std::cell::Cell::new(0usize));
                let mut threads = Vec::new();
                deep(2000, &counter, &mut threads);
                assert_eq!(counter.get(), 2001);
                assert!(threads.len() > 1, "{threads:?}");
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn panics_on_a_new_segment_reach_the_caller() {
        // SAFETY: the closure captures nothing thread-affine.
        let r = std::panic::catch_unwind(|| unsafe { on_new_stack(256 * 1024, || -> u8 { panic!("boom") }) });
        assert!(r.is_err());
        // SAFETY: as above.
        assert_eq!(unsafe { on_new_stack(256 * 1024, || 7) }, 7);
    }
}

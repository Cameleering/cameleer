//! Stack depth guard for the recursive evaluator.
//!
//! Scripts are evaluated on the caller's thread (no thread per evaluation). User-defined
//! recursion is bounded by the interpreter's call depth limit and by the stack actually left on
//! the current thread, measured from the thread's stack bounds (`pthread_getattr_np` on Linux).
//! When a deeply recursive script reaches the end of the caller's stack before the depth limit,
//! evaluation continues on a fresh, larger stack segment (a scoped helper thread that runs while
//! the caller waits, so the single-threaded object graph is never shared concurrently). Scripts
//! that do not recurse deeply never leave the caller's thread.

use std::cell::Cell;

/// Stack kept free for builtins and native code between two depth checks.
pub const RED_ZONE: usize = 256 * 1024;
/// Size of an additional stack segment.
const SEGMENT: usize = 256 << 20;

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

#[cfg(not(target_os = "linux"))]
fn stack_low() -> Option<usize> {
    None
}

fn low() -> usize {
    LOW.with(|l| {
        if l.get() == 0 {
            // unknown platforms: assume 1 MiB below the first measurement point
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

/// Whether the current thread is about to run out of stack.
pub fn low_on_stack() -> bool {
    remaining() < RED_ZONE
}

struct AssertSend<T>(T);
// SAFETY: the job runs on a helper thread while the spawning thread is blocked in `join`, so the
// (single-threaded, `Rc` based) state it touches is never accessed by two threads at once;
// spawn/join provide the happens-before edges.
unsafe impl<T> Send for AssertSend<T> {}

/// Run `f` on a new stack segment (see the module documentation). `None` when no segment could
/// be created.
pub fn on_new_segment<T>(f: impl FnOnce() -> T) -> Option<T> {
    let job = AssertSend(f);
    let arena = AssertSend(crate::arena::take());
    let r = std::thread::scope(|s| {
        let h = std::thread::Builder::new().name("camel-script-stack".into()).stack_size(SEGMENT).spawn_scoped(s, move || {
            let job = job;
            let arena = arena;
            crate::arena::put(arena.0);
            let r = (job.0)();
            AssertSend((r, crate::arena::take()))
        });
        match h {
            Ok(h) => Some(h.join()),
            Err(_) => None,
        }
    });
    match r {
        Some(Ok(AssertSend((v, arena)))) => {
            crate::arena::put(arena);
            Some(v)
        }
        Some(Err(p)) => std::panic::resume_unwind(p),
        None => None,
    }
}

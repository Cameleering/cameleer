//! Stack depth guard for the recursive evaluator.
//!
//! Scripts are evaluated on the caller's thread (no thread per evaluation). User-defined
//! recursion is bounded by the interpreter's call depth limit and by the stack actually left on
//! the current thread, measured from the thread's stack bounds (`cameleer_core::stack`).
//! When a deeply recursive script reaches the end of the caller's stack before the depth limit,
//! evaluation continues on a fresh, larger stack segment (a scoped helper thread that runs while
//! the caller waits, so the single-threaded object graph is never shared concurrently). Scripts
//! that do not recurse deeply never leave the caller's thread.

/// Stack kept free for builtins and native code between two depth checks.
pub const RED_ZONE: usize = 256 * 1024;
/// Size of an additional stack segment.
const SEGMENT: usize = 256 << 20;

/// Bytes of stack left on the current thread (approximate).
pub fn remaining() -> usize {
    cameleer_core::stack::remaining()
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
        let h = std::thread::Builder::new().name("cameleer-script-stack".into()).stack_size(SEGMENT).spawn_scoped(s, move || {
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

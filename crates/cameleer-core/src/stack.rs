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
}

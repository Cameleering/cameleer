//! SIGINT/SIGTERM handling without external crates (POSIX `signal(2)` via FFI).

use std::sync::atomic::{AtomicBool, Ordering};

static RECEIVED: AtomicBool = AtomicBool::new(false);

/// True once SIGINT or SIGTERM has been received (or [`trigger`] was called).
pub fn received() -> bool {
    RECEIVED.load(Ordering::SeqCst)
}

/// Programmatically request shutdown.
pub fn trigger() {
    RECEIVED.store(true, Ordering::SeqCst);
}

/// Reset the flag (tests).
pub fn reset() {
    RECEIVED.store(false, Ordering::SeqCst);
}

#[cfg(unix)]
mod imp {
    use super::RECEIVED;
    use std::sync::atomic::Ordering;

    const SIGINT: i32 = 2;
    const SIGTERM: i32 = 15;

    extern "C" {
        fn signal(signum: i32, handler: extern "C" fn(i32)) -> usize;
    }

    extern "C" fn handler(_sig: i32) {
        // async-signal-safe: only an atomic store
        RECEIVED.store(true, Ordering::SeqCst);
    }

    pub fn install() {
        // SAFETY: installing a handler that only performs an atomic store is async-signal-safe.
        unsafe {
            signal(SIGINT, handler);
            signal(SIGTERM, handler);
        }
    }
}

#[cfg(not(unix))]
mod imp {
    pub fn install() {}
}

/// Install handlers for SIGINT and SIGTERM.
pub fn install() {
    imp::install();
}

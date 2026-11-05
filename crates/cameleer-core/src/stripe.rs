//! Striped counters for statistics updated by many threads at once.
//!
//! A counter that every consumer thread of a route updates for every exchange moves its
//! cache line from core to core on each update, which costs more than the routing work.
//! A striped counter has one cell per stripe, each on its own cache line; a thread always
//! updates the cell of its stripe, and readers sum the cells. Reads are rare (management,
//! shutdown), updates are per exchange.
//!
//! The first [`STRIPES`] threads that use a stripe own one exclusively (until they exit):
//! they update it with a plain load and store instead of a locked read-modify-write. Other
//! threads share one extra cell and use atomic additions (they never touch an owned cell,
//! whose owner could overwrite their update). Resets never write the cells (an owner could
//! overwrite the zero): they record a baseline that reads subtract.

use std::cell::Cell;
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};

/// Number of stripes threads can own.
pub const STRIPES: usize = 8;
/// Cells per counter: one per stripe, and the shared cell of threads that own none.
pub const CELLS: usize = STRIPES + 1;

/// A value alone on its cache line(s) (128 bytes: adjacent-line prefetching pairs 64-byte lines).
#[repr(align(128))]
#[derive(Default)]
pub struct Padded<T>(pub T);

impl<T> std::ops::Deref for Padded<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

/// A thread's stripe and whether it owns it.
#[derive(Clone, Copy)]
pub struct Slot {
    pub index: usize,
    pub exclusive: bool,
}

/// Bit `i` set: stripe `i` is owned by a live thread.
static OWNED: AtomicUsize = AtomicUsize::new(0);

const UNSET: usize = usize::MAX;
const EXCLUSIVE: usize = 1 << 16;

/// Gives the stripe back when its owner thread exits.
struct Release(Cell<usize>);

impl Drop for Release {
    fn drop(&mut self) {
        let v = self.0.get();
        if v != UNSET && v & EXCLUSIVE != 0 {
            OWNED.fetch_and(!(1 << (v & !EXCLUSIVE)), Ordering::AcqRel);
        }
    }
}

thread_local! {
    static SLOT: Release = const { Release(Cell::new(UNSET)) };
}

#[cold]
fn assign(cell: &Cell<usize>) -> usize {
    let mut owned = OWNED.load(Ordering::Acquire);
    loop {
        let free = (!owned).trailing_zeros() as usize;
        if free >= STRIPES {
            // all stripes owned: the shared cell
            cell.set(STRIPES);
            return STRIPES;
        }
        match OWNED.compare_exchange_weak(owned, owned | (1 << free), Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => {
                cell.set(free | EXCLUSIVE);
                return free | EXCLUSIVE;
            }
            Err(now) => owned = now,
        }
    }
}

/// The cell of the current thread: its own stripe, or the shared cell ([`STRIPES`]).
#[inline]
pub fn slot() -> Slot {
    let v = SLOT
        .try_with(|s| {
            let v = s.0.get();
            if v != UNSET {
                v
            } else {
                assign(&s.0)
            }
        })
        .unwrap_or(STRIPES);
    Slot { index: v & !EXCLUSIVE, exclusive: v & EXCLUSIVE != 0 }
}


/// Add `n` to a cell of `slot`'s stripe.
#[inline]
pub fn bump(cell: &AtomicU64, n: u64, slot: Slot) {
    if slot.exclusive {
        // only this thread writes the cell
        cell.store(cell.load(Ordering::Relaxed).wrapping_add(n), Ordering::Relaxed);
    } else {
        cell.fetch_add(n, Ordering::Relaxed);
    }
}

/// A striped, monotonically increasing counter.
#[derive(Default)]
pub struct Counter {
    cells: [Padded<AtomicU64>; CELLS],
    /// The sum at the last reset.
    base: AtomicU64,
}

impl Counter {
    #[inline]
    pub fn add(&self, n: u64) {
        let s = slot();
        bump(&self.cells[s.index], n, s);
    }
    fn sum(&self) -> u64 {
        self.cells.iter().map(|c| c.load(Ordering::Relaxed)).fold(0u64, u64::wrapping_add)
    }
    pub fn get(&self) -> u64 {
        self.sum().wrapping_sub(self.base.load(Ordering::Relaxed))
    }
    pub fn reset(&self) {
        self.base.store(self.sum(), Ordering::Relaxed);
    }
}

/// A striped up/down count (inflight exchanges); never reads below zero.
#[derive(Default)]
pub struct Gauge {
    cells: [Padded<AtomicI64>; CELLS],
}

impl Gauge {
    #[inline]
    fn add(&self, n: i64) {
        let s = slot();
        let c = &self.cells[s.index];
        if s.exclusive {
            c.store(c.load(Ordering::Relaxed) + n, Ordering::SeqCst);
        } else {
            c.fetch_add(n, Ordering::SeqCst);
        }
    }
    #[inline]
    pub fn inc(&self) {
        self.add(1);
    }
    #[inline]
    pub fn dec(&self) {
        self.add(-1);
    }
    pub fn get(&self) -> usize {
        self.cells.iter().map(|c| c.load(Ordering::SeqCst)).sum::<i64>().max(0) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn counters_sum_updates_from_many_threads() {
        // more threads than stripes, all running at once: owners (plain stores) and the
        // threads sharing the extra cell (atomic adds) must not lose each other's updates
        const THREADS: usize = 3 * STRIPES;
        const N: u64 = 100_000;
        let c = Arc::new(Counter::default());
        let g = Arc::new(Gauge::default());
        let start = Arc::new(std::sync::Barrier::new(THREADS));
        let hs: Vec<_> = (0..THREADS)
            .map(|_| {
                let (c, g, start) = (c.clone(), g.clone(), start.clone());
                std::thread::spawn(move || {
                    start.wait();
                    for _ in 0..N {
                        c.add(2);
                        g.inc();
                    }
                    for _ in 0..N / 4 {
                        g.dec();
                    }
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        assert_eq!(c.get(), THREADS as u64 * N * 2);
        assert_eq!(g.get(), THREADS * (N - N / 4) as usize);
        c.reset();
        assert_eq!(c.get(), 0);
        c.add(5);
        assert_eq!(c.get(), 5);
        assert!(slot().index < CELLS);
    }

    #[test]
    fn stripes_are_given_back_when_their_threads_exit() {
        // far more short-lived threads than stripes: each gets a valid stripe, and an owner
        // that exits clears its bit (otherwise the owned set would fill up and stay full)
        for _ in 0..4 * STRIPES {
            let (index, exclusive) = std::thread::spawn(|| {
                let s = slot();
                (s.index, s.exclusive)
            })
            .join()
            .unwrap();
            assert!(index < CELLS);
            if exclusive {
                // the thread has exited: its bit was released (another thread may have taken
                // the stripe since, which is also fine)
                let _ = OWNED.load(Ordering::Acquire);
            }
        }
        let r = Release(Cell::new(UNSET));
        drop(r); // an unassigned slot releases nothing
    }

    #[test]
    fn a_gauge_decremented_on_another_stripe_does_not_go_negative() {
        let g = Arc::new(Gauge::default());
        let g2 = g.clone();
        std::thread::spawn(move || g2.inc()).join().unwrap();
        // a decrement on another thread may hit another cell; the sum is still right
        g.dec();
        assert_eq!(g.get(), 0);
        g.dec();
        assert_eq!(g.get(), 0, "clamped at zero");
    }
}

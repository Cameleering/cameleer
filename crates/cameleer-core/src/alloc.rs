//! A std-only, thread-caching global allocator for Cameleer applications.
//!
//! Routing allocates many small, short-lived objects per exchange (ids, header keys,
//! map nodes, bodies). Through the system allocator each allocate+free pair costs about
//! 30 ns on Windows (`HeapAlloc`), more than the routing work itself. The JVM makes the same
//! objects nearly free with thread-local allocation buffers; this allocator does the
//! equivalent for Rust:
//!
//! - small blocks (up to [`MAX_SMALL`] bytes, alignment up to 16) are rounded to size classes
//!   (16-byte steps up to 1 KiB, 256-byte steps above) and served from a per-thread free list without locking;
//! - a thread's list that grows past [`bin_cap`] blocks hands half of them to a shared,
//!   mutex-protected list per class, which threads refill from in batches (so blocks freed
//!   on a seda consumer thread go back to the producer thread);
//! - new blocks are carved from chunks taken from the system allocator, one block at a time
//!   (a bump pointer), so the pages of a chunk become resident only as they are used; small
//!   blocks are kept for reuse and not returned to the operating system;
//! - larger or over-aligned blocks go straight to the system allocator.
//!
//! It is opt-in. The `cameleer` CLI and the benchmark harness install it; an application
//! does so with
//!
//! ```ignore
//! #[global_allocator]
//! static GLOBAL: cameleer_core::alloc::CamelAllocator = cameleer_core::alloc::CamelAllocator;
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, UnsafeCell};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicU64, Ordering};

/// Largest block size served from the size-class caches.
pub const MAX_SMALL: usize = 4096;
/// Blocks a thread keeps per size class before handing half to the shared list: 64 KiB
/// worth, between 32 and 512 blocks. Large batches keep threads that allocate on one side
/// and free on the other (seda producers and consumers) off the shared lists' locks.
#[inline]
pub const fn bin_cap(class: usize) -> u32 {
    let n = 64 * 1024 / class_size(class);
    if n < 32 {
        32
    } else if n > 512 {
        512
    } else {
        n as u32
    }
}

const GRANULE: usize = 16;
/// Size classes step by `GRANULE` up to `FINE_MAX`, then by `COARSE`.
const FINE_MAX: usize = 1024;
const COARSE: usize = 256;
const FINE_CLASSES: usize = FINE_MAX / GRANULE;
const CLASSES: usize = FINE_CLASSES + (MAX_SMALL - FINE_MAX) / COARSE;
/// Bytes carved from the system allocator at once when a class has no free blocks.
const CHUNK: usize = 16 * 1024;
/// Blocks moved between a thread and the shared list at once.
#[inline]
const fn batch(class: usize) -> u32 {
    bin_cap(class) / 2
}

/// The allocator. Stateless: all state is per thread or in statics.
pub struct CamelAllocator;

struct Node {
    next: *mut Node,
    /// In the first block of a batch on a shared stack: the next batch (low 48 bits) and
    /// this batch's block count (high 16 bits). Atomic because a thread popping the stack
    /// may read it while the block is being reused (the read value is then discarded).
    batch: AtomicU64,
}

#[derive(Clone, Copy)]
struct Bin {
    head: *mut Node,
    len: u32,
}

const EMPTY: Bin = Bin { head: null_mut(), len: 0 };

/// The not yet used part of the last chunk carved for a class: blocks are taken from `ptr` up
/// to `end` only when the free list is empty, so untouched blocks stay untouched pages. Kept
/// apart from the bins, which the fast path reads, so those stay 16 bytes each. A class with
/// no chunk carved yet has no region (`None`), so both pointers always point into a chunk.
#[derive(Clone, Copy)]
struct Bump {
    ptr: *mut u8,
    end: *mut u8,
}

/// A thread's free lists and bump regions.
struct Bins {
    bins: [Bin; CLASSES],
    bumps: [Option<Bump>; CLASSES],
}

impl Bin {
    unsafe fn push(&mut self, p: *mut u8) {
        let n = p as *mut Node;
        (*n).next = self.head;
        self.head = n;
        self.len += 1;
    }
    unsafe fn pop(&mut self) -> *mut u8 {
        let n = self.head;
        if !n.is_null() {
            self.head = (*n).next;
            self.len -= 1;
        }
        n as *mut u8
    }
}

/// The shared blocks of each size class: a lock-free stack of batches (a batch is a
/// null-terminated list of blocks). The head is a tagged pointer, the address in the low
/// 48 bits (user-space addresses fit) and an ABA counter in the high 16 bits. Blocks are
/// never returned to the system, so reading a block that another thread just took is safe.
#[allow(clippy::declare_interior_mutable_const)]
const SHARED_INIT: AtomicU64 = AtomicU64::new(0);
static SHARED: [AtomicU64; CLASSES] = [SHARED_INIT; CLASSES];
const ADDR: u64 = (1 << 48) - 1;

/// Push a batch of `count` blocks starting at `head` onto the shared stack of `class`.
unsafe fn push_batch(class: usize, head: *mut Node, count: u32) {
    if head.is_null() || count == 0 {
        return;
    }
    let addr = head as u64;
    debug_assert_eq!(addr & !ADDR, 0, "block address above 48 bits");
    let shared = &SHARED[class];
    let mut cur = shared.load(Ordering::Acquire);
    loop {
        (*head).batch.store((cur & ADDR) | ((count as u64) << 48), Ordering::Relaxed);
        let tag = ((cur >> 48) + 1) & 0xFFFF;
        match shared.compare_exchange_weak(cur, addr | (tag << 48), Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return,
            Err(now) => cur = now,
        }
    }
}

/// Pop a batch from the shared stack of `class`: (first block, count).
unsafe fn pop_batch(class: usize) -> Option<(*mut Node, u32)> {
    let shared = &SHARED[class];
    let mut cur = shared.load(Ordering::Acquire);
    loop {
        let head = (cur & ADDR) as *mut Node;
        if head.is_null() {
            return None;
        }
        let b = (*head).batch.load(Ordering::Relaxed);
        let tag = ((cur >> 48) + 1) & 0xFFFF;
        match shared.compare_exchange_weak(cur, (b & ADDR) | (tag << 48), Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return Some((head, (b >> 48) as u32)),
            Err(now) => cur = now,
        }
    }
}

/// The shared regions of each size class: the untouched rests of chunks that threads gave back (see
/// [`release_thread_cache`]), a lock-free stack like [`SHARED`]. A region is described in its own
/// first block, so giving one back writes to one page of it only, not to every block. A thread that
/// needs blocks and finds neither a recycled batch nor its own region takes one of these before it
/// carves a new chunk, so the pages in use follow the memory the program needs and not the number of
/// threads that ever allocated.
static REGIONS: [AtomicU64; CLASSES] = [SHARED_INIT; CLASSES];

/// A region's description, stored at its start (a block is 16 bytes or more).
#[repr(C)]
struct RegionNode {
    /// The next region on the stack (low 48 bits).
    next: AtomicU64,
    /// The end of the region.
    end: AtomicU64,
}

/// Push the region `start..end` (at least one block) onto the shared stack of `class`.
unsafe fn push_region(class: usize, start: *mut u8, end: *mut u8) {
    let node = start as *mut RegionNode;
    let shared = &REGIONS[class];
    let mut cur = shared.load(Ordering::Acquire);
    loop {
        (*node).next.store(cur & ADDR, Ordering::Relaxed);
        (*node).end.store(end as u64, Ordering::Relaxed);
        let tag = ((cur >> 48) + 1) & 0xFFFF;
        match shared.compare_exchange_weak(cur, (start as u64) | (tag << 48), Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return,
            Err(now) => cur = now,
        }
    }
}

/// Pop a region from the shared stack of `class`: (start, end).
unsafe fn pop_region(class: usize) -> Option<(*mut u8, *mut u8)> {
    let shared = &REGIONS[class];
    let mut cur = shared.load(Ordering::Acquire);
    loop {
        let head = (cur & ADDR) as *mut RegionNode;
        if head.is_null() {
            return None;
        }
        // Blocks are never returned to the system, so these reads are safe even when another thread
        // took the region meanwhile (the compare-exchange then fails and the values are discarded).
        let next = (*head).next.load(Ordering::Relaxed);
        let end = (*head).end.load(Ordering::Relaxed);
        let tag = ((cur >> 48) + 1) & 0xFFFF;
        match shared.compare_exchange_weak(cur, (next & ADDR) | (tag << 48), Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return Some((head as *mut u8, end as *mut u8)),
            Err(now) => cur = now,
        }
    }
}

const UNINIT: u8 = 0;
const INITIALIZING: u8 = 1;
const ALIVE: u8 = 2;
const DEAD: u8 = 3;

/// A thread's state and bins, in one thread-local so the fast path reads TLS once.
struct Cache {
    state: Cell<u8>,
    bins: UnsafeCell<Bins>,
}

/// Flushes the thread's cache to the shared lists when the thread exits.
struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        // SAFETY: the state is DEAD, so this thread no longer touches its cache.
        let _ = CACHE.try_with(|c| unsafe {
            c.state.set(DEAD);
            let t = &mut *c.bins.get();
            for class in 0..CLASSES {
                give_back(class, &mut t.bins[class], &mut t.bumps[class]);
            }
        });
    }
}

thread_local! {
    static CACHE: Cache = const { Cache { state: Cell::new(UNINIT), bins: UnsafeCell::new(Bins { bins: [EMPTY; CLASSES], bumps: [None; CLASSES] }) } };
    static GUARD: Guard = const { Guard };
}

/// Size class of a layout, or `None` for the system allocator.
#[inline]
fn class_of(layout: &Layout) -> Option<usize> {
    let size = layout.size();
    if size == 0 || size > MAX_SMALL || layout.align() > GRANULE {
        None
    } else {
        Some(if size <= FINE_MAX { (size - 1) / GRANULE } else { FINE_CLASSES + (size - FINE_MAX - 1) / COARSE })
    }
}

#[inline]
const fn class_size(class: usize) -> usize {
    if class < FINE_CLASSES {
        (class + 1) * GRANULE
    } else {
        FINE_MAX + (class - FINE_CLASSES + 1) * COARSE
    }
}

/// Move all blocks of a thread bin to the shared stack of the class, and the unused rest of its
/// chunk to the shared regions (without touching its pages).
unsafe fn give_back(class: usize, bin: &mut Bin, bump: &mut Option<Bump>) {
    give_back_bump(class, bump);
    give_back_bin(class, bin);
}

/// Move the unused rest of a chunk to the shared regions of the class.
unsafe fn give_back_bump(class: usize, bump: &mut Option<Bump>) {
    if let Some(b) = bump.take() {
        if b.ptr < b.end {
            push_region(class, b.ptr, b.end);
        }
    }
}

/// Move the blocks of `bin` to the shared stack of the class.
unsafe fn give_back_bin(class: usize, bin: &mut Bin) {
    push_batch(class, bin.head, bin.len);
    *bin = EMPTY;
}

/// Hands what the calling thread holds to other threads: its cached free blocks go to the shared
/// lists and the unused rest of its chunks to the shared regions, so that the threads that allocate
/// next reuse them instead of carving new chunks.
///
/// A thread that is about to wait (a pool worker with no job) should call this. A thread keeps its
/// free blocks and one chunk region per size class for as long as it lives, and a program with a
/// pool of hundreds of threads would hold hundreds of them: measured, a gRPC server on 133 pooled
/// threads needed 51 MB where the system allocator needed 23 MB (threads that exit give everything
/// back on their own). The unused rest of a chunk is not touched when it moves: a region is described
/// in its own first block, so only that page is written, where linking every block would make all of
/// the pages resident. Does nothing for a thread that has not used the allocator.
pub fn release_thread_cache() {
    let _ = CACHE.try_with(|c| {
        if c.state.get() != ALIVE {
            return;
        }
        // SAFETY: the cache is only used by its own thread, never re-entrantly, and nothing here
        // allocates.
        let t = unsafe { &mut *c.bins.get() };
        for class in 0..CLASSES {
            // SAFETY: as above
            unsafe {
                if t.bins[class].len > 0 {
                    give_back_bin(class, &mut t.bins[class]);
                }
                give_back_bump(class, &mut t.bumps[class]);
            }
        }
    });
}

/// Take a block from the unused rest of the class's last chunk.
#[inline]
unsafe fn take_bump(class: usize, bump: &mut Option<Bump>) -> *mut u8 {
    match bump {
        Some(b) if b.ptr < b.end => {
            let p = b.ptr;
            b.ptr = p.add(class_size(class));
            p
        }
        _ => null_mut(),
    }
}

/// Keep the `keep` most recently freed (cache-hot) blocks of a bin and move the others to
/// the shared list of the class.
unsafe fn give_back_cold(class: usize, bin: &mut Bin, keep: u32) {
    if bin.len <= keep || keep == 0 {
        return;
    }
    let mut last_kept = bin.head;
    for _ in 1..keep {
        last_kept = (*last_kept).next;
    }
    let first = (*last_kept).next;
    (*last_kept).next = null_mut();
    let moved = bin.len - keep;
    bin.len = keep;
    push_batch(class, first, moved);
}

/// Fill a thread bin from the shared list, carving a new chunk if that is empty.
/// Returns one block for the caller.
unsafe fn refill(class: usize, bin: &mut Bin, bump: &mut Option<Bump>) -> *mut u8 {
    let p = take_bump(class, bump);
    if !p.is_null() {
        return p;
    }
    // the bin is empty here: take a whole batch
    if let Some((head, count)) = pop_batch(class) {
        bin.head = head;
        bin.len = count;
    }
    let p = bin.pop();
    if !p.is_null() {
        return p;
    }
    // a region another thread gave back, before new memory
    if let Some((start, end)) = pop_region(class) {
        *bump = Some(Bump { ptr: start.add(class_size(class)), end });
        return start;
    }
    carve(class, bump)
}

/// Carve a chunk of blocks of a class: the first is returned, the rest become the bin's bump
/// region (handed out one by one, see [`take_bump`]).
unsafe fn carve(class: usize, bump: &mut Option<Bump>) -> *mut u8 {
    let size = class_size(class);
    let count = (CHUNK / size).clamp(1, batch(class) as usize);
    let chunk = System.alloc(Layout::from_size_align_unchecked(size * count, GRANULE));
    if chunk.is_null() {
        return chunk;
    }
    // a previous region is empty here (refill takes from it first)
    *bump = Some(Bump { ptr: chunk.add(size), end: chunk.add(size * count) });
    chunk
}

/// Run `f` with this thread's cache, or return `None` while the thread-local storage is
/// being set up or torn down (the caller then uses the shared lists).
#[inline]
fn with_cache<R>(f: impl FnOnce(&mut Bins) -> R) -> Option<R> {
    CACHE
        .try_with(|c| {
            match c.state.get() {
                ALIVE => {}
                UNINIT => {
                    c.state.set(INITIALIZING);
                    // registering the guard's destructor may allocate; those allocations
                    // see INITIALIZING and use the shared lists
                    if GUARD.try_with(|_| ()).is_err() {
                        c.state.set(DEAD);
                        return None;
                    }
                    c.state.set(ALIVE);
                }
                _ => return None,
            }
            // SAFETY: the cache is only used by its own thread, never re-entrantly
            // (nothing in the closures passed here allocates through this allocator).
            Some(f(unsafe { &mut *c.bins.get() }))
        })
        .ok()
        .flatten()
}

unsafe impl GlobalAlloc for CamelAllocator {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let Some(class) = class_of(&layout) else { return System.alloc(layout) };
        match with_cache(|t| {
            let p = t.bins[class].pop();
            if p.is_null() {
                refill(class, &mut t.bins[class], &mut t.bumps[class])
            } else {
                p
            }
        }) {
            Some(p) => p,
            None => {
                let (mut tmp, mut bump) = (EMPTY, None);
                let p = refill(class, &mut tmp, &mut bump);
                give_back(class, &mut tmp, &mut bump);
                p
            }
        }
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let Some(class) = class_of(&layout) else { return System.dealloc(ptr, layout) };
        let cached = with_cache(|t| {
            let bin = &mut t.bins[class];
            bin.push(ptr);
            if bin.len >= bin_cap(class) {
                give_back_cold(class, bin, batch(class));
            }
        });
        if cached.is_none() {
            let mut tmp = EMPTY;
            tmp.push(ptr);
            give_back_bin(class, &mut tmp);
        }
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_layout = Layout::from_size_align_unchecked(new_size, layout.align());
        match (class_of(&layout), class_of(&new_layout)) {
            (None, None) => System.realloc(ptr, layout, new_size),
            (Some(a), Some(b)) if a == b => ptr,
            _ => {
                let p = self.alloc(new_layout);
                if !p.is_null() {
                    std::ptr::copy_nonoverlapping(ptr, p, layout.size().min(new_size));
                    self.dealloc(ptr, layout);
                }
                p
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(size: usize, align: usize) -> Layout {
        Layout::from_size_align(size, align).unwrap()
    }

    #[test]
    fn size_classes() {
        assert_eq!(class_of(&layout(1, 1)), Some(0));
        assert_eq!(class_of(&layout(16, 8)), Some(0));
        assert_eq!(class_of(&layout(17, 8)), Some(1));
        assert_eq!(class_of(&layout(FINE_MAX, 16)), Some(FINE_CLASSES - 1));
        assert_eq!(class_of(&layout(FINE_MAX + 1, 16)), Some(FINE_CLASSES));
        assert_eq!(class_size(FINE_CLASSES), FINE_MAX + COARSE);
        assert_eq!(class_of(&layout(MAX_SMALL, 16)), Some(CLASSES - 1));
        assert_eq!(class_size(CLASSES - 1), MAX_SMALL);
        for size in 1..=MAX_SMALL {
            let c = class_of(&layout(size, 1)).unwrap();
            assert!(class_size(c) >= size && (c == 0 || class_size(c - 1) < size), "size {size}");
        }
        assert_eq!(class_of(&layout(MAX_SMALL + 1, 8)), None);
        assert_eq!(class_of(&layout(64, 32)), None, "over-aligned blocks use the system allocator");
        assert_eq!(class_of(&layout(0, 1)), None);
    }

    #[test]
    fn releasing_the_thread_cache_hands_its_blocks_to_other_threads() {
        let a = CamelAllocator;
        let l = layout(96, 16);
        let class = class_of(&l).unwrap();
        let ptrs: Vec<usize> = (0..100).map(|_| unsafe { a.alloc(l) } as usize).collect();
        for &p in &ptrs {
            unsafe { a.dealloc(p as *mut u8, l) };
        }
        let held = with_cache(|t| t.bins[class].len).unwrap();
        assert!(held >= 100, "the freed blocks are cached by this thread: {held}");
        release_thread_cache();
        assert_eq!(with_cache(|t| t.bins[class].len).unwrap(), 0, "nothing is cached after the release");
        // another thread, which has nothing cached, is served from the released blocks
        let reused = std::thread::spawn(move || {
            let a = CamelAllocator;
            let got: Vec<usize> = (0..100).map(|_| unsafe { a.alloc(l) } as usize).collect();
            let n = got.iter().filter(|p| ptrs.contains(p)).count();
            for p in got {
                unsafe { a.dealloc(p as *mut u8, l) };
            }
            n
        })
        .join()
        .unwrap();
        assert!(reused > 0, "the released blocks are reused by another thread");
    }

    #[test]
    fn releasing_the_thread_cache_hands_the_unused_rest_of_a_chunk_to_other_threads() {
        // a size class no other test uses, because the shared lists are global
        let l = layout(3_000, 16);
        let size = class_size(class_of(&l).unwrap());
        let (kept, next) = std::thread::spawn(move || {
            let a = CamelAllocator;
            // blocks that stay allocated: the thread's chunk has an unused rest after them
            let kept: Vec<usize> = (0..3).map(|_| unsafe { a.alloc(l) } as usize).collect();
            release_thread_cache();
            (kept, std::thread::spawn(move || unsafe { CamelAllocator.alloc(l) } as usize).join().unwrap())
        })
        .join()
        .unwrap();
        // the new thread has no chunk of its own: it takes the block after the last of the first thread
        assert_eq!(next, kept.iter().max().unwrap() + size, "it continues in the first thread's chunk");
    }

    #[test]
    fn blocks_are_aligned_distinct_and_writable() {
        let a = CamelAllocator;
        for size in [1usize, 8, 15, 16, 24, 100, 512, 1024, 4096, 100_000] {
            let l = layout(size, if size >= 16 { 16 } else { 8 });
            let ptrs: Vec<*mut u8> = (0..200).map(|_| unsafe { a.alloc(l) }).collect();
            for (i, &p) in ptrs.iter().enumerate() {
                assert!(!p.is_null());
                assert_eq!(p as usize % l.align(), 0);
                unsafe { std::ptr::write_bytes(p, i as u8, size) };
            }
            let mut sorted = ptrs.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(sorted.len(), ptrs.len(), "size {size}: blocks must not overlap");
            for (i, &p) in ptrs.iter().enumerate() {
                let s = unsafe { std::slice::from_raw_parts(p, size) };
                assert!(s.iter().all(|&b| b == i as u8), "size {size}: block {i} was overwritten");
                unsafe { a.dealloc(p, l) };
            }
        }
    }

    #[test]
    fn a_new_chunk_is_used_block_by_block() {
        // a fresh thread: the first block of a class carves a chunk but writes nothing into
        // its other blocks (they stay untouched pages until used), and the next blocks come
        // from the chunk in address order
        std::thread::spawn(|| {
            let a = CamelAllocator;
            let l = layout(1000, 8);
            let class = class_of(&l).unwrap();
            let p = unsafe { a.alloc(l) };
            let (len, region) = with_cache(|t| (t.bins[class].len, t.bumps[class])).unwrap();
            let Bump { ptr: bump, end } = region.expect("the first allocation carved a chunk");
            assert_eq!(len, 0, "no free-list entries were written into the chunk");
            assert_eq!(bump, unsafe { p.add(class_size(class)) });
            assert!(end > bump);
            let q = unsafe { a.alloc(l) };
            assert_eq!(q, bump, "the next block is the next one in the chunk");
            unsafe {
                a.dealloc(q, l);
                a.dealloc(p, l);
            }
            // freed blocks are reused before the chunk's untouched rest
            assert_eq!(unsafe { a.alloc(l) }, p);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn freed_blocks_are_reused_on_the_same_thread() {
        let a = CamelAllocator;
        let l = layout(40, 8);
        let p = unsafe { a.alloc(l) };
        unsafe { a.dealloc(p, l) };
        let q = unsafe { a.alloc(l) };
        assert_eq!(p, q);
        unsafe { a.dealloc(q, l) };
    }

    #[test]
    fn realloc_keeps_contents_across_classes() {
        let a = CamelAllocator;
        unsafe {
            let mut l = layout(10, 1);
            let mut p = a.alloc(l);
            for i in 0..10 {
                *p.add(i) = i as u8;
            }
            // same class: same block
            assert_eq!(a.realloc(p, l, 16), p);
            for new_size in [17usize, 700, 5000, 40] {
                p = a.realloc(p, l, new_size);
                l = layout(new_size, 1);
                for i in 0..10 {
                    assert_eq!(*p.add(i), i as u8, "after growing to {new_size}");
                }
            }
            a.dealloc(p, l);
        }
    }

    #[test]
    fn blocks_freed_on_another_thread_flow_back() {
        // producer allocates, consumer frees (the seda pattern); both threads exit and flush
        let l = layout(48, 8);
        let (tx, rx) = std::sync::mpsc::sync_channel::<usize>(1024);
        let consumer = std::thread::spawn(move || {
            let a = CamelAllocator;
            let mut n = 0;
            for p in rx {
                unsafe {
                    assert_eq!(*(p as *const u64), p as u64);
                    a.dealloc(p as *mut u8, l);
                }
                n += 1;
            }
            n
        });
        let producer = std::thread::spawn(move || {
            let a = CamelAllocator;
            for _ in 0..20_000 {
                unsafe {
                    let p = a.alloc(l);
                    *(p as *mut u64) = p as u64;
                    tx.send(p as usize).unwrap();
                }
            }
        });
        producer.join().unwrap();
        assert_eq!(consumer.join().unwrap(), 20_000);
        // the blocks are now in the shared list: this thread can take them again
        let a = CamelAllocator;
        let ptrs: Vec<*mut u8> = (0..1000).map(|_| unsafe { a.alloc(l) }).collect();
        for p in ptrs {
            unsafe { a.dealloc(p, l) };
        }
    }
}

//! A cheap monotonic tick clock for the per-exchange statistics.
//!
//! Management statistics time every route and every processor, twice per exchange each.
//! `Instant::now` (`QueryPerformanceCounter` on Windows) and `SystemTime::now` cost 20–30 ns
//! a call, which added up to more than the routing work. On x86-64 CPUs with an invariant
//! time-stamp counter, [`ticks`] reads the TSC directly (a few ns); elsewhere it falls back
//! to `Instant`. Ticks are converted to durations and wall-clock milliseconds with a ratio
//! calibrated against `Instant` and refined while the process runs.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Base {
    instant: Instant,
    tsc: u64,
    use_tsc: bool,
}

static BASE: OnceLock<Base> = OnceLock::new();
/// Nanoseconds per tick, as `f64` bits.
static NANOS_PER_TICK: AtomicU64 = AtomicU64::new(0);
/// Wall-clock epoch milliseconds minus `ticks_to_millis(ticks)`, re-anchored every second.
static WALL_OFFSET_MS: AtomicI64 = AtomicI64::new(0);
/// Tick after which the wall-clock offset and the ratio are refreshed.
static NEXT_ANCHOR: AtomicU64 = AtomicU64::new(0);

#[cfg(target_arch = "x86_64")]
fn invariant_tsc() -> bool {
    use std::arch::x86_64::__cpuid;
    #[allow(unused_unsafe)]
    // SAFETY: cpuid is available on every x86-64 CPU.
    unsafe {
        __cpuid(0x8000_0000).eax >= 0x8000_0007 && (__cpuid(0x8000_0007).edx & (1 << 8)) != 0
    }
}

#[cfg(target_arch = "x86_64")]
#[inline]
fn read_tsc() -> u64 {
    // SAFETY: rdtsc has no preconditions.
    unsafe { std::arch::x86_64::_rdtsc() }
}

#[cfg(not(target_arch = "x86_64"))]
fn invariant_tsc() -> bool {
    false
}

#[cfg(not(target_arch = "x86_64"))]
#[inline]
fn read_tsc() -> u64 {
    0
}

fn wall_millis() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

#[cold]
fn init() -> Base {
    let use_tsc = invariant_tsc();
    let instant = Instant::now();
    let tsc = read_tsc();
    let npt = if use_tsc {
        // calibrate against Instant over a short window; refined later in `anchor`
        let mut now = Instant::now();
        while now.duration_since(instant) < Duration::from_micros(500) {
            now = Instant::now();
        }
        let dt = read_tsc().wrapping_sub(tsc).max(1);
        now.duration_since(instant).as_nanos() as f64 / dt as f64
    } else {
        1.0
    };
    NANOS_PER_TICK.store(npt.to_bits(), Ordering::Relaxed);
    Base { instant, tsc, use_tsc }
}

#[inline]
fn base() -> &'static Base {
    BASE.get_or_init(init)
}

/// The current tick count (monotonic within the process).
#[inline]
pub fn ticks() -> u64 {
    let b = base();
    if b.use_tsc {
        read_tsc()
    } else {
        b.instant.elapsed().as_nanos() as u64
    }
}

#[inline]
fn nanos_per_tick() -> f64 {
    base();
    f64::from_bits(NANOS_PER_TICK.load(Ordering::Relaxed))
}

/// Nanoseconds in `ticks` ticks.
#[inline]
pub fn ticks_to_nanos(ticks: u64) -> u64 {
    // `+ 0.5` rounds without a call into the C runtime's `round`
    (ticks as f64 * nanos_per_tick() + 0.5) as u64
}

/// Whole milliseconds in `ticks` ticks (without floating point for less than one).
#[inline]
pub fn ticks_to_millis(ticks: u64) -> u64 {
    static TICKS_PER_MS: AtomicU64 = AtomicU64::new(0);
    let mut per_ms = TICKS_PER_MS.load(Ordering::Relaxed);
    if per_ms == 0 {
        per_ms = duration_to_ticks(Duration::from_millis(1)).max(1);
        TICKS_PER_MS.store(per_ms, Ordering::Relaxed);
    }
    if ticks < per_ms {
        0
    } else {
        ticks_to_nanos(ticks) / 1_000_000
    }
}

/// Ticks in a duration.
pub fn duration_to_ticks(d: Duration) -> u64 {
    (d.as_nanos() as f64 / nanos_per_tick() + 0.5) as u64
}

/// The duration between two tick readings (zero if `end` is before `start`).
#[inline]
pub fn elapsed(start: u64, end: u64) -> Duration {
    Duration::from_nanos(ticks_to_nanos(end.saturating_sub(start)))
}

/// Wall-clock epoch milliseconds at tick `t` (a recent reading).
pub fn ticks_to_epoch_millis(t: u64) -> i64 {
    if t >= NEXT_ANCHOR.load(Ordering::Relaxed) {
        anchor(t);
    }
    (ticks_to_nanos(t) / 1_000_000) as i64 + WALL_OFFSET_MS.load(Ordering::Relaxed)
}

/// The current wall-clock time, millisecond precision, for timestamps taken per exchange.
///
/// On Windows this is `GetSystemTimeAsFileTime` (what the JVM's `currentTimeMillis` uses: a
/// read of the shared system time, about 1 ns) instead of the precise clock behind
/// `SystemTime::now` (about 25 ns). Elsewhere it is derived from the tick clock.
#[cfg(windows)]
pub fn now_system_time() -> SystemTime {
    #[repr(C)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetSystemTimeAsFileTime(ft: *mut FileTime);
    }
    thread_local! {
        /// The last reading and its `SystemTime` (the clock only moves every millisecond or so).
        static LAST: std::cell::Cell<(u64, SystemTime)> = const { std::cell::Cell::new((0, UNIX_EPOCH)) };
    }
    let mut ft = FileTime { low: 0, high: 0 };
    // SAFETY: writes the current time into `ft`.
    unsafe { GetSystemTimeAsFileTime(&mut ft) };
    // 100 ns intervals since 1601-01-01
    let intervals = ((ft.high as u64) << 32) | ft.low as u64;
    const EPOCH_DIFF: u64 = 116_444_736_000_000_000;
    let convert = |i: u64| UNIX_EPOCH + Duration::from_millis(i.saturating_sub(EPOCH_DIFF) / 10_000);
    LAST.try_with(|l| {
        let (last, t) = l.get();
        if last == intervals {
            return t;
        }
        let t = convert(intervals);
        l.set((intervals, t));
        t
    })
    .unwrap_or_else(|_| convert(intervals))
}

/// The current wall-clock time, millisecond precision, from the tick clock.
#[cfg(not(windows))]
pub fn now_system_time() -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(ticks_to_epoch_millis(ticks()).max(0) as u64)
}

/// Refresh the ratio (over the whole run so far) and the wall-clock offset, at most once a
/// second.
#[cold]
fn anchor(t: u64) {
    let b = base();
    let now = Instant::now();
    let tsc = ticks();
    if b.use_tsc {
        let run = now.duration_since(b.instant);
        if run >= Duration::from_millis(100) {
            let dt = tsc.wrapping_sub(b.tsc).max(1);
            NANOS_PER_TICK.store((run.as_nanos() as f64 / dt as f64).to_bits(), Ordering::Relaxed);
        }
    }
    WALL_OFFSET_MS.store(wall_millis() - (ticks_to_nanos(tsc) / 1_000_000) as i64, Ordering::Relaxed);
    NEXT_ANCHOR.store(t.max(tsc).saturating_add(duration_to_ticks(Duration::from_secs(1))), Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_are_monotonic_and_match_instant() {
        let t0 = ticks();
        let i0 = Instant::now();
        std::thread::sleep(Duration::from_millis(30));
        let t1 = ticks();
        let real = i0.elapsed();
        assert!(t1 > t0);
        let measured = elapsed(t0, t1);
        let diff = measured.as_secs_f64() - real.as_secs_f64();
        assert!(diff.abs() < 0.005, "measured {measured:?}, real {real:?}");
        assert_eq!(elapsed(t1, t0), Duration::ZERO);
    }

    #[test]
    fn epoch_millis_follow_the_system_clock() {
        let wall = wall_millis();
        let t = ticks_to_epoch_millis(ticks());
        assert!((t - wall).abs() < 50, "tick wall clock {t} vs system {wall}");
    }

    #[test]
    fn now_system_time_follows_the_system_clock() {
        let a = SystemTime::now();
        let t = now_system_time();
        let b = SystemTime::now();
        let ms = |x: SystemTime| x.duration_since(UNIX_EPOCH).unwrap().as_millis() as i64;
        // the coarse clock may lag the precise one by up to a timer tick (15.6 ms)
        assert!(ms(t) >= ms(a) - 20 && ms(t) <= ms(b) + 1, "{} not in [{}, {}]", ms(t), ms(a), ms(b));
    }

    #[test]
    fn millis_of_ticks() {
        assert_eq!(ticks_to_millis(0), 0);
        assert_eq!(ticks_to_millis(duration_to_ticks(Duration::from_micros(990))), 0);
        assert_eq!(ticks_to_millis(duration_to_ticks(Duration::from_millis(7))), 7);
    }

    #[test]
    fn durations_round_trip_through_ticks() {
        let d = Duration::from_millis(250);
        let back = ticks_to_nanos(duration_to_ticks(d));
        assert!((back as i64 - 250_000_000).abs() < 1_000_000, "{back}");
    }
}

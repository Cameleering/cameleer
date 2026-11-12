//! Phased load driver shared by the in-process scenarios and the HTTP load generator.
//!
//! `workers` threads call an operation in a closed loop. Time is split into epochs: epoch 0
//! is the warmup (discarded), epochs `1..=iters` are the timed iterations. Each worker keeps
//! one latency histogram per epoch, so recording never needs a lock. The main thread sleeps
//! through each epoch and then bumps the epoch counter.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::hist::Histogram;

/// Warmup and measurement durations.
#[derive(Clone, Copy, Debug)]
pub struct Plan {
    pub warmup: Duration,
    pub iteration: Duration,
    pub iters: usize,
}

impl Plan {
    pub fn total(&self) -> Duration {
        self.warmup + self.iteration * self.iters as u32
    }
}

/// The measured result of one scenario.
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    /// Messages completed in the timed iterations.
    pub msgs: u64,
    /// Wall time of the timed iterations.
    pub secs: f64,
    /// Throughput of each timed iteration (msgs/s).
    pub per_iter: Vec<f64>,
    /// Latency of the timed iterations (empty when the scenario has no per-call latency).
    pub latency: Histogram,
}

impl Outcome {
    pub fn throughput(&self) -> f64 {
        if self.secs > 0.0 {
            self.msgs as f64 / self.secs
        } else {
            0.0
        }
    }

    /// One `RESULT` line, the format both harnesses print.
    pub fn result_line(&self, scenario: &str) -> String {
        let us = |ns: f64| ns / 1000.0;
        let min = self.per_iter.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = self.per_iter.iter().cloned().fold(0.0, f64::max);
        let has_lat = self.latency.count() > 0;
        let lat = |v: f64| if has_lat { format!("{:.2}", us(v)) } else { "-".to_string() };
        format!(
            "RESULT scenario={} msgs={} secs={:.3} tput={:.0} tput_min={:.0} tput_max={:.0} mean_us={} p50_us={} p99_us={} max_us={}",
            scenario,
            self.msgs,
            self.secs,
            self.throughput(),
            if min.is_finite() { min } else { 0.0 },
            max,
            lat(self.latency.mean()),
            lat(self.latency.percentile(50.0) as f64),
            lat(self.latency.percentile(99.0) as f64),
            lat(self.latency.max() as f64),
        )
    }
}

const STOP: usize = usize::MAX;

/// Run `workers` closed-loop workers. `make_worker(i)` builds worker `i`'s operation; each
/// call of the operation is one message and is timed for the latency histogram. When
/// `completions` is given, throughput is taken from that counter (e.g. messages that
/// reached a seda consumer) instead of from the number of calls, and no latency is reported.
pub fn run<F, Op>(plan: Plan, workers: usize, completions: Option<Arc<AtomicU64>>, make_worker: F) -> Outcome
where
    F: Fn(usize) -> Op,
    Op: FnMut() + Send + 'static,
{
    let epoch = Arc::new(AtomicUsize::new(0));
    let epochs = plan.iters + 1;
    let mut handles = Vec::new();
    for i in 0..workers {
        let mut op = make_worker(i);
        let epoch = epoch.clone();
        handles.push(std::thread::spawn(move || {
            let mut hists = vec![Histogram::default(); epochs];
            loop {
                let e = epoch.load(Ordering::Relaxed);
                if e == STOP {
                    break;
                }
                let t0 = Instant::now();
                op();
                hists[e].record(t0.elapsed().as_nanos() as u64);
            }
            hists
        }));
    }
    let mut boundaries = Vec::new();
    let mut counter_marks = Vec::new();
    std::thread::sleep(plan.warmup);
    for e in 1..epochs {
        boundaries.push(Instant::now());
        counter_marks.push(completions.as_ref().map(|c| c.load(Ordering::Relaxed)).unwrap_or(0));
        epoch.store(e, Ordering::Relaxed);
        std::thread::sleep(plan.iteration);
    }
    boundaries.push(Instant::now());
    counter_marks.push(completions.as_ref().map(|c| c.load(Ordering::Relaxed)).unwrap_or(0));
    epoch.store(STOP, Ordering::Relaxed);
    let per_worker: Vec<Vec<Histogram>> = handles.into_iter().map(|h| h.join().expect("bench worker panicked")).collect();

    let mut out = Outcome::default();
    for e in 1..epochs {
        let secs = (boundaries[e] - boundaries[e - 1]).as_secs_f64();
        let mut h = Histogram::default();
        for w in &per_worker {
            h.merge(&w[e]);
        }
        let n = if completions.is_some() { counter_marks[e] - counter_marks[e - 1] } else { h.count() };
        out.msgs += n;
        out.secs += secs;
        out.per_iter.push(n as f64 / secs);
        if completions.is_none() {
            out.latency.merge(&h);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_calls_per_epoch_and_formats_result() {
        // A busy wait, not `thread::sleep`: on Windows CI runners a 200 us sleep can take 15 ms or
        // more, and a loaded runner gave 4 calls in 80 ms. The test checks the counting, not speed.
        // 300 us keeps the median clear of the 200 us check below (histogram buckets round down).
        let plan = Plan { warmup: Duration::from_millis(30), iteration: Duration::from_millis(100), iters: 2 };
        let out = run(plan, 2, None, |_| {
            || {
                let t = Instant::now();
                while t.elapsed() < Duration::from_micros(300) {
                    std::hint::spin_loop();
                }
            }
        });
        assert_eq!(out.per_iter.len(), 2);
        assert!(out.msgs >= 2, "{out:?}");
        assert_eq!(out.msgs, out.latency.count());
        assert!(out.latency.percentile(50.0) >= 200_000);
        let line = out.result_line("x");
        assert!(line.starts_with("RESULT scenario=x msgs="), "{line}");
        assert!(line.contains(" p99_us="));
    }

    #[test]
    fn completion_counter_drives_throughput() {
        let done = Arc::new(AtomicU64::new(0));
        let plan = Plan { warmup: Duration::from_millis(20), iteration: Duration::from_millis(30), iters: 1 };
        let d = done.clone();
        let out = run(plan, 1, Some(done.clone()), move |_| {
            let d = d.clone();
            move || {
                d.fetch_add(3, Ordering::Relaxed);
                std::thread::sleep(Duration::from_micros(100));
            }
        });
        assert!(out.msgs > 0 && out.msgs % 3 == 0, "{out:?}");
        assert_eq!(out.latency.count(), 0);
        assert!(out.result_line("s").contains("p50_us=-"));
    }
}

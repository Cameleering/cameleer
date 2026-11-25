//! The batch span processor (OpenTelemetry `BatchSpanProcessor`): finished spans go into a
//! bounded queue; one thread exports them in batches of at most `max_export_batch_size`, every
//! `schedule_delay` or as soon as a full batch is waiting. When the queue is full a span is
//! dropped and counted, so a slow or unreachable collector never blocks a route.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use cameleer_core::telemetry::span::{SpanData, SpanProcessor};

use crate::exporter::{ExportResult, SpanExporter, LOGGER};

/// Counters of a batch span processor.
#[derive(Default, Debug)]
pub struct BatchStats {
    /// Spans exported successfully.
    pub exported: AtomicU64,
    /// Spans dropped because the queue was full (or the processor was shut down).
    pub dropped: AtomicU64,
    /// Spans lost because their export failed.
    pub failed: AtomicU64,
    /// Export requests made (batches).
    pub batches: AtomicU64,
}

impl BatchStats {
    pub fn snapshot(&self) -> (u64, u64, u64, u64) {
        (self.exported.load(Ordering::Relaxed), self.dropped.load(Ordering::Relaxed), self.failed.load(Ordering::Relaxed), self.batches.load(Ordering::Relaxed))
    }
}

/// Settings of a [`BatchSpanProcessor`].
#[derive(Clone, Debug)]
pub struct BatchConfig {
    pub max_queue_size: usize,
    pub max_export_batch_size: usize,
    pub schedule_delay: Duration,
    pub export_timeout: Duration,
}

impl Default for BatchConfig {
    fn default() -> Self {
        BatchConfig { max_queue_size: 2048, max_export_batch_size: 512, schedule_delay: Duration::from_millis(5000), export_timeout: Duration::from_millis(30000) }
    }
}

struct State {
    queue: VecDeque<SpanData>,
    /// A flush was requested: export everything now.
    flush: bool,
    /// The worker is exporting a batch taken from the queue.
    busy: bool,
    /// Flushes completed (a waiting `force_flush` watches it).
    flushed: u64,
    stop: bool,
}

struct Shared {
    state: Mutex<State>,
    /// Wakes the worker.
    work: Condvar,
    /// Wakes callers waiting for a flush.
    done: Condvar,
    cfg: BatchConfig,
    stats: Arc<BatchStats>,
    shut: AtomicBool,
    /// Whether a failed export was already logged (one warning until an export succeeds).
    warned: AtomicBool,
}

/// The batch span processor.
pub struct BatchSpanProcessor {
    shared: Arc<Shared>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl BatchSpanProcessor {
    pub fn new(exporter: Arc<dyn SpanExporter>, cfg: BatchConfig) -> BatchSpanProcessor {
        let cfg = BatchConfig { max_queue_size: cfg.max_queue_size.max(1), max_export_batch_size: cfg.max_export_batch_size.clamp(1, cfg.max_queue_size.max(1)), ..cfg };
        let shared = Arc::new(Shared {
            state: Mutex::new(State { queue: VecDeque::new(), flush: false, busy: false, flushed: 0, stop: false }),
            work: Condvar::new(),
            done: Condvar::new(),
            cfg,
            stats: Arc::new(BatchStats::default()),
            shut: AtomicBool::new(false),
            warned: AtomicBool::new(false),
        });
        let s = shared.clone();
        let worker = std::thread::Builder::new().name("otel-batch-exporter".into()).spawn(move || run(s, exporter)).expect("spawn the span exporter thread");
        BatchSpanProcessor { shared, worker: Mutex::new(Some(worker)) }
    }

    pub fn stats(&self) -> Arc<BatchStats> {
        self.shared.stats.clone()
    }

    /// Spans waiting for export.
    pub fn queued(&self) -> usize {
        self.shared.state.lock().unwrap().queue.len()
    }
}

fn run(s: Arc<Shared>, exporter: Arc<dyn SpanExporter>) {
    let mut next = Instant::now() + s.cfg.schedule_delay;
    loop {
        let (batch, flushing, stop) = {
            let mut st = s.state.lock().unwrap();
            loop {
                if st.stop || st.flush || st.queue.len() >= s.cfg.max_export_batch_size {
                    break;
                }
                let now = Instant::now();
                if now >= next {
                    break;
                }
                st = s.work.wait_timeout(st, next - now).unwrap().0;
            }
            let n = st.queue.len().min(s.cfg.max_export_batch_size);
            let batch: Vec<SpanData> = st.queue.drain(..n).collect();
            st.busy = !batch.is_empty();
            (batch, st.flush, st.stop)
        };
        if !batch.is_empty() {
            s.stats.batches.fetch_add(1, Ordering::Relaxed);
            match exporter.export(&batch, s.cfg.export_timeout) {
                ExportResult::Success => {
                    s.stats.exported.fetch_add(batch.len() as u64, Ordering::Relaxed);
                    if s.warned.swap(false, Ordering::Relaxed) {
                        cameleer_core::camel_info!(LOGGER, "Exporting spans again");
                    }
                }
                ExportResult::Failure(msg, _) => {
                    s.stats.failed.fetch_add(batch.len() as u64, Ordering::Relaxed);
                    if !s.warned.swap(true, Ordering::Relaxed) {
                        cameleer_core::camel_warn!(LOGGER, "Dropped {} spans: export failed: {}", batch.len(), msg);
                    } else {
                        cameleer_core::camel_debug!(LOGGER, "Dropped {} spans: export failed: {}", batch.len(), msg);
                    }
                }
            }
        }
        let mut st = s.state.lock().unwrap();
        st.busy = false;
        if st.queue.len() < s.cfg.max_export_batch_size && !(flushing && !st.queue.is_empty()) {
            next = Instant::now() + s.cfg.schedule_delay;
        }
        if flushing && st.queue.is_empty() {
            st.flush = false;
            st.flushed += 1;
            s.done.notify_all();
        }
        if stop && st.queue.is_empty() {
            st.flushed += 1;
            s.done.notify_all();
            return;
        }
    }
}

impl SpanProcessor for BatchSpanProcessor {
    fn on_end(&self, span: SpanData) {
        if self.shared.shut.load(Ordering::Relaxed) {
            self.shared.stats.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let mut st = self.shared.state.lock().unwrap();
        if st.queue.len() >= self.shared.cfg.max_queue_size {
            drop(st);
            self.shared.stats.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        st.queue.push_back(span);
        let full = st.queue.len() == self.shared.cfg.max_export_batch_size;
        drop(st);
        if full {
            self.shared.work.notify_one();
        }
    }

    fn force_flush(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut st = self.shared.state.lock().unwrap();
        if st.queue.is_empty() && !st.busy {
            return true;
        }
        let target = st.flushed + 1;
        st.flush = true;
        self.shared.work.notify_one();
        while st.flushed < target {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            st = self.shared.done.wait_timeout(st, deadline - now).unwrap().0;
        }
        true
    }

    fn shutdown(&self, timeout: Duration) {
        if self.shared.shut.swap(true, Ordering::Relaxed) {
            return;
        }
        {
            let mut st = self.shared.state.lock().unwrap();
            st.stop = true;
        }
        self.shared.work.notify_one();
        let deadline = Instant::now() + timeout;
        let mut st = self.shared.state.lock().unwrap();
        while !(st.queue.is_empty() && !st.busy) && Instant::now() < deadline {
            st = self.shared.done.wait_timeout(st, deadline.saturating_duration_since(Instant::now())).unwrap().0;
        }
        let left = st.queue.len();
        st.queue.clear();
        drop(st);
        if left > 0 {
            self.shared.stats.dropped.fetch_add(left as u64, Ordering::Relaxed);
            cameleer_core::camel_warn!(LOGGER, "Dropped {} spans at shutdown", left);
        }
        if let Some(h) = self.worker.lock().unwrap().take() {
            // the worker ends once the queue is empty and its export returned
            if Instant::now() < deadline {
                let _ = h.join();
            }
        }
    }
}

impl Drop for BatchSpanProcessor {
    fn drop(&mut self) {
        let mut st = self.shared.state.lock().unwrap();
        st.stop = true;
        drop(st);
        self.shared.work.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cameleer_core::telemetry::span::{SpanKind, StatusCode};

    pub(crate) fn span(name: &str) -> SpanData {
        SpanData {
            trace_id: [1; 16],
            span_id: [2; 8],
            parent_span_id: [0; 8],
            parent_remote: false,
            trace_state: String::new(),
            flags: 1,
            name: name.into(),
            kind: SpanKind::Internal,
            start_unix_nano: 1,
            end_unix_nano: 2,
            attributes: Vec::new(),
            dropped_attributes: 0,
            events: Vec::new(),
            status: StatusCode::Unset,
            status_message: String::new(),
            scope_name: "camel".into(),
        }
    }

    /// Records batches; blocks while `gate` is closed; fails while `fail` is set.
    #[derive(Default)]
    struct Recorder {
        batches: Mutex<Vec<usize>>,
        gate: Mutex<bool>,
        open: Condvar,
        fail: AtomicBool,
    }

    impl SpanExporter for Recorder {
        fn export(&self, spans: &[SpanData], _budget: Duration) -> ExportResult {
            let mut g = self.gate.lock().unwrap();
            while *g {
                g = self.open.wait(g).unwrap();
            }
            self.batches.lock().unwrap().push(spans.len());
            if self.fail.load(Ordering::Relaxed) {
                ExportResult::Failure("down".into(), true)
            } else {
                ExportResult::Success
            }
        }
    }

    #[test]
    fn batches_by_size_and_flushes() {
        let rec = Arc::new(Recorder::default());
        let p = BatchSpanProcessor::new(rec.clone(), BatchConfig { max_queue_size: 100, max_export_batch_size: 10, schedule_delay: Duration::from_secs(60), export_timeout: Duration::from_secs(1) });
        for i in 0..25 {
            p.on_end(span(&i.to_string()));
        }
        assert!(p.force_flush(Duration::from_secs(5)));
        let b = rec.batches.lock().unwrap().clone();
        assert_eq!(b.iter().sum::<usize>(), 25);
        assert!(b.iter().all(|n| *n <= 10), "{b:?}");
        assert_eq!(p.stats().snapshot(), (25, 0, 0, b.len() as u64));
        p.shutdown(Duration::from_secs(5));
        p.on_end(span("late"));
        assert_eq!(p.stats().dropped.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_full_queue_drops_and_counts_without_blocking() {
        let rec = Arc::new(Recorder::default());
        *rec.gate.lock().unwrap() = true;
        let p = BatchSpanProcessor::new(rec.clone(), BatchConfig { max_queue_size: 5, max_export_batch_size: 5, schedule_delay: Duration::from_millis(10), export_timeout: Duration::from_secs(1) });
        // the first batch is taken by the blocked exporter; then the queue fills up
        for i in 0..5 {
            p.on_end(span(&i.to_string()));
        }
        let t = Instant::now();
        while p.queued() != 0 && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(2));
        }
        let start = Instant::now();
        for i in 0..20 {
            p.on_end(span(&i.to_string()));
        }
        assert!(start.elapsed() < Duration::from_millis(500), "on_end must not block");
        assert_eq!(p.queued(), 5);
        assert_eq!(p.stats().dropped.load(Ordering::Relaxed), 15);
        *rec.gate.lock().unwrap() = false;
        rec.open.notify_all();
        assert!(p.force_flush(Duration::from_secs(5)));
        assert_eq!(p.stats().exported.load(Ordering::Relaxed), 10);
    }

    #[test]
    fn failed_exports_are_counted_and_the_processor_recovers() {
        let rec = Arc::new(Recorder::default());
        rec.fail.store(true, Ordering::Relaxed);
        let p = BatchSpanProcessor::new(rec.clone(), BatchConfig { max_queue_size: 50, max_export_batch_size: 4, schedule_delay: Duration::from_millis(20), export_timeout: Duration::from_secs(1) });
        for i in 0..8 {
            p.on_end(span(&i.to_string()));
        }
        assert!(p.force_flush(Duration::from_secs(5)));
        assert_eq!(p.stats().failed.load(Ordering::Relaxed), 8);
        rec.fail.store(false, Ordering::Relaxed);
        p.on_end(span("ok"));
        assert!(p.force_flush(Duration::from_secs(5)));
        assert_eq!(p.stats().exported.load(Ordering::Relaxed), 1);
        // the schedule exports without a flush too
        p.on_end(span("timed"));
        let t = Instant::now();
        while p.stats().exported.load(Ordering::Relaxed) < 2 && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(p.stats().exported.load(Ordering::Relaxed), 2);
        p.shutdown(Duration::from_secs(5));
    }
}

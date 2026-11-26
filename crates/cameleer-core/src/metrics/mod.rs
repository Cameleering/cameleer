//! The metric registry behind Camel's `micrometer`, `metrics` (Dropwizard) and
//! `opentelemetry-metrics` components and the `camel.metrics.*` meters: counters, gauges, timers,
//! distribution summaries, Dropwizard meters and histograms, kept std-only and exported in the
//! Prometheus text format (`/q/metrics`, `/observe/metrics`) and over OTLP (`cameleer-opentelemetry`).
//!
//! A meter is identified by its name and tags (Micrometer's `Meter.Id`); asking the registry for
//! the same name and tags again returns the same meter. Recording is lock-free: atomics, no
//! allocation. A timer and a distribution summary also keep bucket counts (the bounds of the
//! Prometheus client's default buckets, in the meter's base unit), which the OTLP export sends as a
//! histogram; the Prometheus text renders them as Micrometer does by default: `_count`, `_sum`
//! and `_max`.

pub mod instrument;
pub mod prometheus;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use crate::context::CamelContext;

/// Registry name of a context's [`MeterRegistry`].
pub const REGISTRY_NAME: &str = "cameleer.metrics.registry";

/// Bucket bounds of timers (seconds) and distribution summaries (their unit): the Prometheus
/// client's defaults.
pub const DEFAULT_BOUNDS: &[f64] = &[0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5, 10.0];

/// The OpenTelemetry SDK's default explicit bucket bounds (for `opentelemetry-metrics`
/// histograms, typically milliseconds).
pub const OTEL_BOUNDS: &[f64] = &[0.0, 5.0, 10.0, 25.0, 50.0, 75.0, 100.0, 250.0, 500.0, 750.0, 1000.0, 2500.0, 5000.0, 7500.0, 10000.0];

/// How long a timer's or summary's `max` is kept (Micrometer's default `expiry` is 2 minutes,
/// rotated in 3 buffers; this keeps the larger of the current and the previous minute).
const MAX_WINDOW: Duration = Duration::from_secs(60);

/// A meter's identity: name and tags, tags sorted by key.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MeterId {
    pub name: String,
    pub tags: Vec<(String, String)>,
}

impl MeterId {
    pub fn new(name: impl Into<String>, tags: impl IntoIterator<Item = (String, String)>) -> MeterId {
        let mut tags: Vec<(String, String)> = tags.into_iter().collect();
        // a later tag of the same key wins
        tags.reverse();
        tags.sort_by(|a, b| a.0.cmp(&b.0));
        tags.dedup_by(|a, b| a.0 == b.0);
        MeterId { name: name.into(), tags }
    }
    pub fn tag(&self, k: &str) -> Option<&str> {
        self.tags.iter().find(|(t, _)| t == k).map(|(_, v)| v.as_str())
    }
}

/// Parses Camel's tag syntax `key=value,key2=value2` (the deprecated `tags` option of
/// `micrometer:` and a `CamelMetricsTags` header given as text).
pub fn parse_tags(s: &str) -> Vec<(String, String)> {
    s.split(',')
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            let k = k.trim();
            (!k.is_empty()).then(|| (k.to_string(), v.trim().to_string()))
        })
        .collect()
}

/// An `f64` kept in an `AtomicU64`.
#[derive(Default, Debug)]
pub struct AtomicF64(AtomicU64);

impl AtomicF64 {
    pub fn new(v: f64) -> Self {
        AtomicF64(AtomicU64::new(v.to_bits()))
    }
    pub fn get(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::Relaxed))
    }
    pub fn set(&self, v: f64) {
        self.0.store(v.to_bits(), Ordering::Relaxed)
    }
    pub fn add(&self, d: f64) {
        let mut cur = self.0.load(Ordering::Relaxed);
        loop {
            let new = (f64::from_bits(cur) + d).to_bits();
            match self.0.compare_exchange_weak(cur, new, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => return,
                Err(c) => cur = c,
            }
        }
    }
    pub fn max(&self, v: f64) {
        let mut cur = self.0.load(Ordering::Relaxed);
        while v > f64::from_bits(cur) {
            match self.0.compare_exchange_weak(cur, v.to_bits(), Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => return,
                Err(c) => cur = c,
            }
        }
    }
}

/// A monotonic counter (Micrometer `Counter`). Negative increments are ignored, as Micrometer does.
#[derive(Default, Debug)]
pub struct Counter {
    value: AtomicF64,
}

impl Counter {
    pub fn increment(&self, by: f64) {
        if by > 0.0 {
            self.value.add(by)
        }
    }
    pub fn count(&self) -> f64 {
        self.value.get()
    }
}

/// A value that goes up and down: a gauge set to a value, or an up-down counter.
#[derive(Default, Debug)]
pub struct Gauge {
    value: AtomicF64,
}

impl Gauge {
    pub fn set(&self, v: f64) {
        self.value.set(v)
    }
    pub fn add(&self, d: f64) {
        self.value.add(d)
    }
    pub fn value(&self) -> f64 {
        self.value.get()
    }
}

/// A gauge whose value is read when the meters are exported (Micrometer `Gauge` with a function).
pub struct FnGauge(pub Box<dyn Fn() -> f64 + Send + Sync>);

impl std::fmt::Debug for FnGauge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FnGauge")
    }
}

/// Count, total, max and bucket counts of recorded values: the state of a timer (values in
/// seconds) or a distribution summary (values in its own unit).
#[derive(Debug)]
pub struct Distribution {
    count: AtomicU64,
    total: AtomicF64,
    /// The max of the current window and of the previous one.
    max: [AtomicF64; 2],
    /// Tick reading when the current window started, and which `max` slot it uses.
    window_start: AtomicU64,
    window_slot: AtomicUsize,
    bounds: Arc<[f64]>,
    /// One count per bound, then the overflow bucket.
    buckets: Box<[AtomicU64]>,
}

impl Distribution {
    pub fn new(bounds: &[f64]) -> Distribution {
        Distribution {
            count: AtomicU64::new(0),
            total: AtomicF64::default(),
            max: [AtomicF64::default(), AtomicF64::default()],
            window_start: AtomicU64::new(crate::clock::ticks()),
            window_slot: AtomicUsize::new(0),
            bounds: bounds.into(),
            buckets: (0..=bounds.len()).map(|_| AtomicU64::new(0)).collect(),
        }
    }

    /// The `max` slot of the current window; starts a new window (and clears the older slot)
    /// once a window is over. Lock-free: one thread wins the swap.
    fn rotate(&self) -> usize {
        static WINDOW_TICKS: OnceLock<u64> = OnceLock::new();
        let len = *WINDOW_TICKS.get_or_init(|| crate::clock::duration_to_ticks(MAX_WINDOW));
        let start = self.window_start.load(Ordering::Relaxed);
        let now = crate::clock::ticks();
        let slot = self.window_slot.load(Ordering::Relaxed);
        if now.saturating_sub(start) >= len && self.window_start.compare_exchange(start, now, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
            let next = 1 - slot;
            self.max[next].set(0.0);
            self.window_slot.store(next, Ordering::Relaxed);
            return next;
        }
        slot
    }

    pub fn record(&self, v: f64) {
        if v.is_nan() || v < 0.0 {
            return;
        }
        self.count.fetch_add(1, Ordering::Relaxed);
        self.total.add(v);
        let i = self.bounds.iter().position(|b| v <= *b).unwrap_or(self.bounds.len());
        self.buckets[i].fetch_add(1, Ordering::Relaxed);
        let w = self.rotate();
        self.max[w].max(v);
    }

    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }
    pub fn total(&self) -> f64 {
        self.total.get()
    }
    pub fn max(&self) -> f64 {
        let _ = self.rotate();
        self.max[0].get().max(self.max[1].get())
    }
    pub fn mean(&self) -> f64 {
        let c = self.count();
        if c == 0 {
            0.0
        } else {
            self.total() / c as f64
        }
    }
    pub fn bounds(&self) -> &[f64] {
        &self.bounds
    }
    /// Bucket counts (not cumulative): one per bound, then the overflow bucket.
    pub fn bucket_counts(&self) -> Vec<u64> {
        self.buckets.iter().map(|b| b.load(Ordering::Relaxed)).collect()
    }
    /// The value below which a fraction `q` of the values fall, estimated from the buckets.
    pub fn quantile(&self, q: f64) -> f64 {
        let counts = self.bucket_counts();
        let total: u64 = counts.iter().sum();
        if total == 0 {
            return 0.0;
        }
        let target = (q * total as f64).ceil().max(1.0) as u64;
        let mut seen = 0;
        for (i, c) in counts.iter().enumerate() {
            seen += c;
            if seen >= target {
                return if i < self.bounds.len() { self.bounds[i] } else { self.max() };
            }
        }
        self.max()
    }
}

/// A timer (Micrometer `Timer`, Dropwizard `Timer`): durations in seconds.
#[derive(Debug)]
pub struct Timer(pub Distribution);

impl Timer {
    pub fn record(&self, d: Duration) {
        self.0.record(d.as_secs_f64())
    }
}

/// A rate meter (Dropwizard `Meter`): a count with the mean rate since creation.
#[derive(Debug)]
pub struct Meter {
    count: AtomicU64,
    created: Instant,
}

impl Meter {
    pub fn mark(&self, n: u64) {
        self.count.fetch_add(n, Ordering::Relaxed);
    }
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }
    /// Events per second since the meter was created.
    pub fn mean_rate(&self) -> f64 {
        let s = self.created.elapsed().as_secs_f64();
        if s <= 0.0 {
            0.0
        } else {
            self.count() as f64 / s
        }
    }
}

/// A long task timer (Micrometer `LongTaskTimer`): tasks in progress and how long they have run.
#[derive(Debug, Default)]
pub struct LongTaskTimer {
    active: Mutex<BTreeMap<u64, Instant>>,
    next: AtomicU64,
}

impl LongTaskTimer {
    /// Starts a task; pass the handle to [`LongTaskTimer::stop`].
    pub fn start(&self) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.active.lock().unwrap().insert(id, Instant::now());
        id
    }
    pub fn stop(&self, id: u64) {
        self.active.lock().unwrap().remove(&id);
    }
    pub fn active_tasks(&self) -> usize {
        self.active.lock().unwrap().len()
    }
    /// The summed and the longest duration of the tasks in progress, in seconds.
    pub fn durations(&self) -> (f64, f64) {
        let a = self.active.lock().unwrap();
        let mut total = 0.0;
        let mut max: f64 = 0.0;
        for t in a.values() {
            let d = t.elapsed().as_secs_f64();
            total += d;
            max = max.max(d);
        }
        (total, max)
    }
}

/// The kinds of meters.
#[derive(Clone, Debug)]
pub enum MeterKind {
    Counter(Arc<Counter>),
    /// A value set from outside (Dropwizard's gauge with a subject).
    Gauge(Arc<Gauge>),
    /// A sum that goes up and down (Dropwizard's counter, OpenTelemetry's `LongUpDownCounter`):
    /// a gauge in Prometheus, a non-monotonic sum in OTLP.
    UpDownCounter(Arc<Gauge>),
    FnGauge(Arc<FnGauge>),
    Timer(Arc<Timer>),
    /// A distribution summary (Micrometer), histogram (Dropwizard, OpenTelemetry).
    Summary(Arc<Distribution>),
    Meter(Arc<Meter>),
    LongTaskTimer(Arc<LongTaskTimer>),
}

impl MeterKind {
    pub fn type_name(&self) -> &'static str {
        match self {
            MeterKind::Counter(_) => "counter",
            MeterKind::Gauge(_) | MeterKind::FnGauge(_) => "gauge",
            MeterKind::UpDownCounter(_) => "up_down_counter",
            MeterKind::Timer(_) => "timer",
            MeterKind::Summary(_) => "distribution_summary",
            MeterKind::Meter(_) => "meter",
            MeterKind::LongTaskTimer(_) => "long_task_timer",
        }
    }
}

/// A registered meter: kind, description and base unit.
#[derive(Clone, Debug)]
pub struct Registered {
    pub kind: MeterKind,
    pub description: Option<String>,
    /// `seconds` for timers, `""` or the configured unit otherwise.
    pub unit: String,
}

/// The metric registry of a context (Micrometer's `MeterRegistry`).
pub struct MeterRegistry {
    meters: RwLock<BTreeMap<MeterId, Registered>>,
    /// Tags added to every meter.
    common_tags: RwLock<Vec<(String, String)>>,
    /// When the registry was created: the start time of cumulative OTLP data points.
    pub created_unix_nano: u64,
}

impl Default for MeterRegistry {
    fn default() -> Self {
        MeterRegistry {
            meters: RwLock::new(BTreeMap::new()),
            common_tags: RwLock::new(Vec::new()),
            created_unix_nano: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0),
        }
    }
}

/// Error when a name and tags are registered with another kind.
#[derive(Debug)]
pub struct KindMismatch(pub MeterId, pub &'static str);

impl MeterRegistry {
    /// The registry of `ctx` (created on first use).
    pub fn of(ctx: &CamelContext) -> Arc<MeterRegistry> {
        static LOCK: Mutex<()> = Mutex::new(());
        if let Some(r) = ctx.registry().lookup::<MeterRegistry>(REGISTRY_NAME) {
            return r;
        }
        let _g = LOCK.lock().unwrap();
        if let Some(r) = ctx.registry().lookup::<MeterRegistry>(REGISTRY_NAME) {
            return r;
        }
        let r = Arc::new(MeterRegistry::default());
        ctx.registry().bind_arc(REGISTRY_NAME, r.clone());
        r
    }

    /// The registry of `ctx` if one was created.
    pub fn existing(ctx: &CamelContext) -> Option<Arc<MeterRegistry>> {
        ctx.registry().lookup::<MeterRegistry>(REGISTRY_NAME)
    }

    pub fn set_common_tags(&self, tags: Vec<(String, String)>) {
        *self.common_tags.write().unwrap() = tags;
    }

    fn id(&self, name: &str, tags: &[(String, String)]) -> MeterId {
        let common = self.common_tags.read().unwrap();
        MeterId::new(name, common.iter().cloned().chain(tags.iter().cloned()))
    }

    fn get_or<T>(&self, name: &str, tags: &[(String, String)], description: Option<&str>, unit: &str, make: impl FnOnce() -> MeterKind, pick: impl Fn(&MeterKind) -> Option<T>) -> Result<T, KindMismatch> {
        let id = self.id(name, tags);
        if let Some(r) = self.meters.read().unwrap().get(&id) {
            return pick(&r.kind).ok_or_else(|| KindMismatch(id.clone(), r.kind.type_name()));
        }
        let mut m = self.meters.write().unwrap();
        let r = m.entry(id.clone()).or_insert_with(|| Registered { kind: make(), description: description.map(str::to_string), unit: unit.to_string() });
        if r.description.is_none() {
            r.description = description.map(str::to_string);
        }
        pick(&r.kind).ok_or_else(|| KindMismatch(id, r.kind.type_name()))
    }

    pub fn counter(&self, name: &str, tags: &[(String, String)], description: Option<&str>) -> Result<Arc<Counter>, KindMismatch> {
        self.get_or(name, tags, description, "", || MeterKind::Counter(Arc::default()), |k| match k {
            MeterKind::Counter(c) => Some(c.clone()),
            _ => None,
        })
    }

    pub fn gauge(&self, name: &str, tags: &[(String, String)], description: Option<&str>) -> Result<Arc<Gauge>, KindMismatch> {
        self.get_or(name, tags, description, "", || MeterKind::Gauge(Arc::default()), |k| match k {
            MeterKind::Gauge(c) => Some(c.clone()),
            _ => None,
        })
    }

    pub fn up_down_counter(&self, name: &str, tags: &[(String, String)], description: Option<&str>, unit: &str) -> Result<Arc<Gauge>, KindMismatch> {
        self.get_or(name, tags, description, unit, || MeterKind::UpDownCounter(Arc::default()), |k| match k {
            MeterKind::UpDownCounter(c) => Some(c.clone()),
            _ => None,
        })
    }

    /// A gauge read from `f` at export time (replaces an earlier function of the same id).
    pub fn fn_gauge(&self, name: &str, tags: &[(String, String)], description: Option<&str>, f: impl Fn() -> f64 + Send + Sync + 'static) {
        self.fn_gauge_with_unit(name, tags, description, "", f)
    }

    /// [`MeterRegistry::fn_gauge`] with a base unit (`routes`, `milliseconds`).
    pub fn fn_gauge_with_unit(&self, name: &str, tags: &[(String, String)], description: Option<&str>, unit: &str, f: impl Fn() -> f64 + Send + Sync + 'static) {
        let id = self.id(name, tags);
        self.meters.write().unwrap().insert(id, Registered { kind: MeterKind::FnGauge(Arc::new(FnGauge(Box::new(f)))), description: description.map(str::to_string), unit: unit.to_string() });
    }

    pub fn timer(&self, name: &str, tags: &[(String, String)], description: Option<&str>) -> Result<Arc<Timer>, KindMismatch> {
        self.get_or(name, tags, description, "seconds", || MeterKind::Timer(Arc::new(Timer(Distribution::new(DEFAULT_BOUNDS)))), |k| match k {
            MeterKind::Timer(c) => Some(c.clone()),
            _ => None,
        })
    }

    pub fn summary(&self, name: &str, tags: &[(String, String)], description: Option<&str>) -> Result<Arc<Distribution>, KindMismatch> {
        self.get_or(name, tags, description, "", || MeterKind::Summary(Arc::new(Distribution::new(DEFAULT_BOUNDS))), |k| match k {
            MeterKind::Summary(c) => Some(c.clone()),
            _ => None,
        })
    }

    /// A distribution with its own bucket bounds and unit (OpenTelemetry histograms).
    pub fn histogram(&self, name: &str, tags: &[(String, String)], description: Option<&str>, unit: &str, bounds: &[f64]) -> Result<Arc<Distribution>, KindMismatch> {
        self.get_or(name, tags, description, unit, || MeterKind::Summary(Arc::new(Distribution::new(bounds))), |k| match k {
            MeterKind::Summary(c) => Some(c.clone()),
            _ => None,
        })
    }

    pub fn meter(&self, name: &str, tags: &[(String, String)], description: Option<&str>) -> Result<Arc<Meter>, KindMismatch> {
        self.get_or(name, tags, description, "", || MeterKind::Meter(Arc::new(Meter { count: AtomicU64::new(0), created: Instant::now() })), |k| match k {
            MeterKind::Meter(c) => Some(c.clone()),
            _ => None,
        })
    }

    pub fn long_task_timer(&self, name: &str, tags: &[(String, String)], description: Option<&str>) -> Result<Arc<LongTaskTimer>, KindMismatch> {
        self.get_or(name, tags, description, "seconds", || MeterKind::LongTaskTimer(Arc::default()), |k| match k {
            MeterKind::LongTaskTimer(c) => Some(c.clone()),
            _ => None,
        })
    }

    /// Every meter, sorted by name and tags.
    pub fn meters(&self) -> Vec<(MeterId, Registered)> {
        self.meters.read().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    /// The meter with this name and tags (common tags included).
    pub fn find(&self, name: &str, tags: &[(String, String)]) -> Option<Registered> {
        self.meters.read().unwrap().get(&self.id(name, tags)).cloned()
    }

    /// Meters named `name`, any tags.
    pub fn find_all(&self, name: &str) -> Vec<(MeterId, Registered)> {
        self.meters.read().unwrap().iter().filter(|(k, _)| k.name == name).map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    /// Removes the meters a predicate selects.
    pub fn remove_if(&self, f: impl Fn(&MeterId) -> bool) {
        self.meters.write().unwrap().retain(|id, _| !f(id));
    }

    /// Removes every meter.
    pub fn clear(&self) {
        self.meters.write().unwrap().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_id_same_meter_and_kind_mismatch() {
        let r = MeterRegistry::default();
        let t = vec![("b".to_string(), "2".to_string()), ("a".to_string(), "1".to_string())];
        let c1 = r.counter("orders", &t, Some("Orders")).unwrap();
        let c2 = r.counter("orders", &[("a".into(), "1".into()), ("b".into(), "2".into())], None).unwrap();
        c1.increment(2.0);
        c2.increment(0.5);
        c2.increment(-3.0);
        assert_eq!(c1.count(), 2.5);
        assert!(r.timer("orders", &t, None).is_err());
        assert_eq!(r.meters().len(), 1);
        assert_eq!(r.meters()[0].1.description.as_deref(), Some("Orders"));
        assert_eq!(parse_tags("a=1, b = two,=x,c"), vec![("a".to_string(), "1".to_string()), ("b".to_string(), "two".to_string())]);
        // a later tag of the same key wins
        assert_eq!(MeterId::new("x", [("k".to_string(), "1".to_string()), ("k".to_string(), "2".to_string())]).tag("k"), Some("2"));
    }

    #[test]
    fn distributions_count_total_max_buckets_quantiles() {
        let d = Distribution::new(&[1.0, 10.0]);
        for v in [0.5, 2.0, 3.0, 20.0, -1.0, f64::NAN] {
            d.record(v);
        }
        assert_eq!(d.count(), 4);
        assert_eq!(d.total(), 25.5);
        assert_eq!(d.max(), 20.0);
        assert_eq!(d.bucket_counts(), vec![1, 2, 1]);
        assert_eq!(d.quantile(0.5), 10.0);
        assert_eq!(d.quantile(1.0), 20.0);
        let t = Timer(Distribution::new(DEFAULT_BOUNDS));
        t.record(Duration::from_millis(30));
        assert!((t.0.total() - 0.03).abs() < 1e-9);
        let l = LongTaskTimer::default();
        let a = l.start();
        let _b = l.start();
        l.stop(a);
        assert_eq!(l.active_tasks(), 1);
    }

    #[test]
    fn concurrent_recording_is_exact() {
        let r = Arc::new(MeterRegistry::default());
        let hs: Vec<_> = (0..8)
            .map(|_| {
                let r = r.clone();
                std::thread::spawn(move || {
                    let c = r.counter("c", &[], None).unwrap();
                    let t = r.timer("t", &[], None).unwrap();
                    for _ in 0..10_000 {
                        c.increment(1.0);
                        t.record(Duration::from_millis(1));
                    }
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        assert_eq!(r.counter("c", &[], None).unwrap().count(), 80_000.0);
        assert_eq!(r.timer("t", &[], None).unwrap().0.count(), 80_000);
    }
}

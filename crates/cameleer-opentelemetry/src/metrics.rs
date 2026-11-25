//! OTLP metrics: the protobuf encoding of a [`MeterRegistry`] snapshot
//! (`opentelemetry/proto/collector/metrics/v1/metrics_service.proto`) and the periodic exporter
//! (OpenTelemetry's `PeriodicExportingMetricReader`) that posts it to `/v1/metrics`.
//!
//! | message | fields |
//! |---|---|
//! | `ExportMetricsServiceRequest` | `resource_metrics` 1 |
//! | `ResourceMetrics` | `resource` 1, `scope_metrics` 2 |
//! | `ScopeMetrics` | `scope` 1, `metrics` 2 |
//! | `Metric` | `name` 1, `description` 2, `unit` 3, `gauge` 5, `sum` 7, `histogram` 9 |
//! | `Gauge` | `data_points` 1 |
//! | `Sum` | `data_points` 1, `aggregation_temporality` 2, `is_monotonic` 3 |
//! | `Histogram` | `data_points` 1, `aggregation_temporality` 2 |
//! | `NumberDataPoint` | `start_time_unix_nano` 2, `time_unix_nano` 3 (fixed64), `as_double` 4, `as_int` 6 (sfixed64), `attributes` 7 |
//! | `HistogramDataPoint` | `start_time_unix_nano` 2, `time_unix_nano` 3, `count` 4 (fixed64), `sum` 5 (double), `bucket_counts` 6 (packed fixed64), `explicit_bounds` 7 (packed double), `attributes` 9, `min` 11, `max` 12 |
//!
//! Temporality is cumulative (2): Camel's meters count from the start of the process, as
//! Prometheus does. Meter kinds map to OTLP as: counter and Dropwizard meter → monotonic sum;
//! gauge → gauge; timer (unit `s`) and distribution summary → histogram with the registry's bucket
//! bounds and the window max; long task timer → gauges `<name>.active` and `<name>.duration`.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cameleer_core::metrics::{Distribution, MeterId, MeterKind, MeterRegistry, Registered};
use cameleer_core::telemetry::span::{AttrValue, Attributes};

use crate::proto::{self, Writer};

/// Cumulative temporality (`AGGREGATION_TEMPORALITY_CUMULATIVE`).
pub const CUMULATIVE: u64 = 2;

fn now_nanos() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0)
}

fn attrs(id: &MeterId) -> Attributes {
    id.tags.iter().map(|(k, v)| (k.clone(), AttrValue::String(v.clone()))).collect()
}

fn number_point(w: &mut Writer, field: u32, id: &MeterId, start: u64, time: u64, v: Num) {
    w.message(field, |w| {
        w.fixed64(2, start);
        w.fixed64(3, time);
        match v {
            Num::Double(d) => w.double(4, d),
            // sfixed64: the same 8 little-endian bytes as fixed64
            Num::Int(i) => {
                w.fixed64_always(6, i as u64);
            }
        }
        proto::attributes(w, 7, &attrs(id));
    });
}

#[derive(Clone, Copy)]
enum Num {
    Double(f64),
    Int(i64),
}

fn histogram_point(w: &mut Writer, id: &MeterId, start: u64, time: u64, d: &Distribution) {
    w.message(1, |w| {
        w.fixed64(2, start);
        w.fixed64(3, time);
        w.fixed64_always(4, d.count());
        w.double(5, d.total());
        w.packed_fixed64(6, &d.bucket_counts());
        w.packed_double(7, d.bounds());
        proto::attributes(w, 9, &attrs(id));
        w.double(12, d.max());
    });
}

/// Encodes one metric.
fn metric(w: &mut Writer, id: &MeterId, r: &Registered, start: u64, time: u64) {
    let desc = r.description.as_deref().unwrap_or("");
    let header = |w: &mut Writer, name: &str, unit: &str| {
        w.string(1, name);
        w.string(2, desc);
        w.string(3, unit);
    };
    match &r.kind {
        MeterKind::Counter(c) => w.message(2, |w| {
            header(w, &id.name, &r.unit);
            w.message(7, |w| {
                number_point(w, 1, id, start, time, Num::Double(c.count()));
                w.uint(2, CUMULATIVE);
                w.bool(3, true);
            });
        }),
        MeterKind::Meter(m) => w.message(2, |w| {
            header(w, &id.name, &r.unit);
            w.message(7, |w| {
                number_point(w, 1, id, start, time, Num::Int(m.count() as i64));
                w.uint(2, CUMULATIVE);
                w.bool(3, true);
            });
        }),
        MeterKind::UpDownCounter(g) => w.message(2, |w| {
            header(w, &id.name, &r.unit);
            w.message(7, |w| {
                number_point(w, 1, id, start, time, Num::Double(g.value()));
                w.uint(2, CUMULATIVE);
                w.bool(3, false);
            });
        }),
        MeterKind::Gauge(g) => w.message(2, |w| {
            header(w, &id.name, &r.unit);
            w.message(5, |w| number_point(w, 1, id, start, time, Num::Double(g.value())));
        }),
        MeterKind::FnGauge(g) => w.message(2, |w| {
            header(w, &id.name, &r.unit);
            w.message(5, |w| number_point(w, 1, id, start, time, Num::Double((g.0)())));
        }),
        MeterKind::Timer(t) => w.message(2, |w| {
            header(w, &id.name, "s");
            w.message(9, |w| {
                histogram_point(w, id, start, time, &t.0);
                w.uint(2, CUMULATIVE);
            });
        }),
        MeterKind::Summary(d) => w.message(2, |w| {
            header(w, &id.name, &r.unit);
            w.message(9, |w| {
                histogram_point(w, id, start, time, d);
                w.uint(2, CUMULATIVE);
            });
        }),
        MeterKind::LongTaskTimer(t) => {
            let (total, _) = t.durations();
            w.message(2, |w| {
                header(w, &format!("{}.active", id.name), "{tasks}");
                w.message(5, |w| number_point(w, 1, id, start, time, Num::Int(t.active_tasks() as i64)));
            });
            w.message(2, |w| {
                header(w, &format!("{}.duration", id.name), "s");
                w.message(5, |w| number_point(w, 1, id, start, time, Num::Double(total)));
            });
        }
    }
}

/// Encodes an `ExportMetricsServiceRequest` with one resource and one scope.
pub fn encode_export_request(resource: &Attributes, scope: &str, meters: &[(MeterId, Registered)], start: u64) -> Vec<u8> {
    let time = now_nanos();
    let mut w = Writer::default();
    w.message(1, |w| {
        w.message(1, |w| proto::attributes(w, 1, resource));
        w.message(2, |w| {
            w.message(1, |w| {
                w.string(1, scope);
                w.string(2, env!("CARGO_PKG_VERSION"));
            });
            for (id, r) in meters {
                metric(w, id, r, start, time);
            }
        });
    });
    w.buf
}

// ---------------------------------------------------------------------------
// decoding (tests)
// ---------------------------------------------------------------------------

/// A data point as received.
#[derive(Clone, Debug, PartialEq)]
pub enum Point {
    Double(f64),
    Int(i64),
    Histogram { count: u64, sum: f64, bucket_counts: Vec<u64>, bounds: Vec<f64>, max: f64 },
}

/// A metric as received.
#[derive(Clone, Debug)]
pub struct ReceivedMetric {
    pub resource: Attributes,
    pub scope: String,
    pub name: String,
    pub description: String,
    pub unit: String,
    /// `gauge`, `sum` or `histogram`.
    pub kind: &'static str,
    pub monotonic: bool,
    pub temporality: u64,
    pub points: Vec<(Attributes, Point, u64, u64)>,
}

fn bytes(f: &proto::Field) -> &[u8] {
    match f {
        proto::Field::Bytes(b) => b,
        _ => &[],
    }
}

fn num(f: &proto::Field) -> u64 {
    match f {
        proto::Field::Varint(v) | proto::Field::Fixed64(v) => *v,
        proto::Field::Fixed32(v) => *v as u64,
        proto::Field::Bytes(_) => 0,
    }
}

fn kv(b: &[u8]) -> Result<(String, AttrValue), String> {
    let mut k = String::new();
    let mut v = AttrValue::String(String::new());
    for (n, f) in proto::fields(b)? {
        match n {
            1 => k = String::from_utf8_lossy(bytes(&f)).into_owned(),
            2 => {
                for (n2, f2) in proto::fields(bytes(&f))? {
                    v = match n2 {
                        1 => AttrValue::String(String::from_utf8_lossy(bytes(&f2)).into_owned()),
                        3 => AttrValue::Int(num(&f2) as i64),
                        4 => AttrValue::Double(f64::from_bits(num(&f2))),
                        2 => AttrValue::Bool(num(&f2) != 0),
                        _ => v,
                    }
                }
            }
            _ => {}
        }
    }
    Ok((k, v))
}

fn packed<T>(b: &[u8], f: impl Fn([u8; 8]) -> T) -> Vec<T> {
    b.chunks_exact(8).map(|c| f(c.try_into().unwrap())).collect()
}

fn points(b: &[u8], histogram: bool) -> Result<Vec<(Attributes, Point, u64, u64)>, String> {
    let mut out = Vec::new();
    for (n, f) in proto::fields(b)? {
        if n != 1 {
            continue;
        }
        let mut a = Vec::new();
        let (mut start, mut time) = (0, 0);
        let mut p = if histogram { Point::Histogram { count: 0, sum: 0.0, bucket_counts: Vec::new(), bounds: Vec::new(), max: 0.0 } } else { Point::Double(0.0) };
        for (n2, f2) in proto::fields(bytes(&f))? {
            match (n2, &mut p) {
                (2, _) => start = num(&f2),
                (3, _) => time = num(&f2),
                (4, Point::Double(_) | Point::Int(_)) => p = Point::Double(f64::from_bits(num(&f2))),
                (6, Point::Double(_) | Point::Int(_)) => p = Point::Int(num(&f2) as i64),
                (7, Point::Double(_) | Point::Int(_)) => a.push(kv(bytes(&f2))?),
                (4, Point::Histogram { count, .. }) => *count = num(&f2),
                (5, Point::Histogram { sum, .. }) => *sum = f64::from_bits(num(&f2)),
                (6, Point::Histogram { bucket_counts, .. }) => *bucket_counts = packed(bytes(&f2), u64::from_le_bytes),
                (7, Point::Histogram { bounds, .. }) => *bounds = packed(bytes(&f2), f64::from_le_bytes),
                (9, Point::Histogram { .. }) => a.push(kv(bytes(&f2))?),
                (12, Point::Histogram { max, .. }) => *max = f64::from_bits(num(&f2)),
                _ => {}
            }
        }
        out.push((a, p, start, time));
    }
    Ok(out)
}

/// Decodes an `ExportMetricsServiceRequest`.
pub fn decode_export_request(b: &[u8]) -> Result<Vec<ReceivedMetric>, String> {
    let mut out = Vec::new();
    for (n, rm) in proto::fields(b)? {
        if n != 1 {
            continue;
        }
        let rm = proto::fields(bytes(&rm))?;
        let mut resource = Vec::new();
        for (n, f) in &rm {
            if *n == 1 {
                for (n2, f2) in proto::fields(bytes(f))? {
                    if n2 == 1 {
                        resource.push(kv(bytes(&f2))?);
                    }
                }
            }
        }
        for (n, sm) in &rm {
            if *n != 2 {
                continue;
            }
            let sm = proto::fields(bytes(sm))?;
            let mut scope = String::new();
            for (n2, f2) in &sm {
                if *n2 == 1 {
                    for (n3, f3) in proto::fields(bytes(f2))? {
                        if n3 == 1 {
                            scope = String::from_utf8_lossy(bytes(&f3)).into_owned();
                        }
                    }
                }
            }
            for (n2, m) in &sm {
                if *n2 != 2 {
                    continue;
                }
                let mut r = ReceivedMetric {
                    resource: resource.clone(),
                    scope: scope.clone(),
                    name: String::new(),
                    description: String::new(),
                    unit: String::new(),
                    kind: "",
                    monotonic: false,
                    temporality: 0,
                    points: Vec::new(),
                };
                for (n3, f3) in proto::fields(bytes(m))? {
                    match n3 {
                        1 => r.name = String::from_utf8_lossy(bytes(&f3)).into_owned(),
                        2 => r.description = String::from_utf8_lossy(bytes(&f3)).into_owned(),
                        3 => r.unit = String::from_utf8_lossy(bytes(&f3)).into_owned(),
                        5 | 7 | 9 => {
                            r.kind = match n3 {
                                5 => "gauge",
                                7 => "sum",
                                _ => "histogram",
                            };
                            r.points = points(bytes(&f3), n3 == 9)?;
                            for (n4, f4) in proto::fields(bytes(&f3))? {
                                match n4 {
                                    2 => r.temporality = num(&f4),
                                    3 => r.monotonic = num(&f4) != 0,
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }
                out.push(r);
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// the periodic exporter
// ---------------------------------------------------------------------------

/// Counters of a [`PeriodicMetricExporter`].
#[derive(Default, Debug)]
pub struct MetricExportStats {
    pub exports: AtomicU64,
    pub failures: AtomicU64,
}

/// Exports a registry every `interval` (`OTEL_METRIC_EXPORT_INTERVAL`, default 60 s) over
/// OTLP/HTTP, and once more at shutdown. A failed export is logged once and retried at the next
/// interval (the meters are cumulative, so nothing is lost).
pub struct PeriodicMetricExporter {
    /// (stop, flush requested, exports done)
    state: Arc<(Mutex<(bool, bool, u64)>, Condvar)>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    stats: Arc<MetricExportStats>,
    shut: AtomicBool,
}

/// What a periodic export sends.
pub type Snapshot = Box<dyn Fn() -> Vec<(MeterId, Registered)> + Send + Sync>;

impl PeriodicMetricExporter {
    /// Starts the exporter thread. `snapshot` lists the meters to send (the registry's, plus the
    /// route statistics).
    pub fn start(exporter: crate::exporter::OtlpHttpExporter, scope: String, interval: Duration, timeout: Duration, start_nanos: u64, snapshot: Snapshot) -> PeriodicMetricExporter {
        let state = Arc::new((Mutex::new((false, false, 0u64)), Condvar::new()));
        let stats = Arc::new(MetricExportStats::default());
        let (s2, st2) = (state.clone(), stats.clone());
        let worker = std::thread::Builder::new()
            .name("otel-metric-exporter".into())
            .spawn(move || {
                let mut warned = false;
                loop {
                    let stopping = {
                        let g = s2.0.lock().unwrap();
                        let (mut g, _) = s2.1.wait_timeout_while(g, interval, |st| !st.0 && !st.1).unwrap();
                        g.1 = false;
                        g.0
                    };
                    let body = encode_export_request(exporter.resource(), &scope, &snapshot(), start_nanos);
                    match exporter.post(&body, timeout) {
                        Ok(()) => {
                            st2.exports.fetch_add(1, Ordering::Relaxed);
                            warned = false;
                        }
                        Err(e) => {
                            st2.failures.fetch_add(1, Ordering::Relaxed);
                            if !warned {
                                cameleer_core::camel_warn!(crate::exporter::LOGGER, "Metrics export failed: {}", e);
                                warned = true;
                            }
                        }
                    }
                    s2.0.lock().unwrap().2 += 1;
                    s2.1.notify_all();
                    if stopping {
                        return;
                    }
                }
            })
            .expect("spawn the metric exporter thread");
        PeriodicMetricExporter { state, worker: Mutex::new(Some(worker)), stats, shut: AtomicBool::new(false) }
    }

    pub fn stats(&self) -> Arc<MetricExportStats> {
        self.stats.clone()
    }

    /// Exports now; true when the export finished within `timeout`.
    pub fn force_flush(&self, timeout: Duration) -> bool {
        let mut g = self.state.0.lock().unwrap();
        if g.0 {
            return true;
        }
        let target = g.2 + 1;
        g.1 = true;
        self.state.1.notify_all();
        let (_g, r) = self.state.1.wait_timeout_while(g, timeout, |st| st.2 < target).unwrap();
        !r.timed_out()
    }

    /// Exports one last time and stops.
    pub fn shutdown(&self, timeout: Duration) {
        if self.shut.swap(true, Ordering::Relaxed) {
            return;
        }
        {
            let mut g = self.state.0.lock().unwrap();
            g.0 = true;
            self.state.1.notify_all();
        }
        let start = std::time::Instant::now();
        if let Some(h) = self.worker.lock().unwrap().take() {
            while !h.is_finished() && start.elapsed() < timeout {
                std::thread::sleep(Duration::from_millis(5));
            }
            if h.is_finished() {
                let _ = h.join();
            }
        }
    }
}

/// The registry's meters (for a [`Snapshot`]).
pub fn registry_snapshot(r: Arc<MeterRegistry>) -> Snapshot {
    Box::new(move || r.meters())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_round_trips() {
        let r = MeterRegistry::default();
        let tags = vec![("routeId".to_string(), "orders".to_string())];
        r.counter("orders.placed", &tags, Some("Orders")).unwrap().increment(3.0);
        r.gauge("queue.depth", &[], None).unwrap().set(-2.0);
        r.meter("hits", &[], None).unwrap().mark(5);
        let t = r.timer("camel.route.policy", &tags, None).unwrap();
        t.record(Duration::from_millis(3));
        t.record(Duration::from_millis(300));
        r.summary("order.amount", &[], None).unwrap().record(42.0);
        let l = r.long_task_timer("camel.route.policy.long.task", &[], None).unwrap();
        let _task = l.start();
        let res = vec![("service.name".to_string(), AttrValue::from("svc"))];
        let got = decode_export_request(&encode_export_request(&res, "camel", &r.meters(), 7)).unwrap();
        let by = |n: &str| got.iter().find(|m| m.name == n).unwrap_or_else(|| panic!("{n}: {got:#?}"));
        let c = by("orders.placed");
        assert_eq!((c.kind, c.monotonic, c.temporality, c.description.as_str()), ("sum", true, CUMULATIVE, "Orders"));
        assert_eq!(c.points[0].0, vec![("routeId".to_string(), AttrValue::from("orders"))]);
        assert_eq!(c.points[0].1, Point::Double(3.0));
        assert_eq!(c.points[0].2, 7);
        assert_eq!(by("queue.depth").points[0].1, Point::Double(-2.0));
        assert_eq!(by("hits").points[0].1, Point::Int(5));
        let h = by("camel.route.policy");
        assert_eq!((h.kind, h.unit.as_str()), ("histogram", "s"));
        match &h.points[0].1 {
            Point::Histogram { count, sum, bucket_counts, bounds, max } => {
                assert_eq!(*count, 2);
                assert!((sum - 0.303).abs() < 1e-9);
                assert_eq!(bounds.len(), cameleer_core::metrics::DEFAULT_BOUNDS.len());
                assert_eq!(bucket_counts.len(), bounds.len() + 1);
                assert_eq!(bucket_counts.iter().sum::<u64>(), 2);
                assert!((max - 0.3).abs() < 1e-9);
            }
            p => panic!("{p:?}"),
        }
        assert_eq!(by("camel.route.policy.long.task.active").points[0].1, Point::Int(1));
        assert_eq!(got[0].resource, res);
        assert_eq!(got[0].scope, "camel");
    }
}

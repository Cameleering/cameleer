//! The Prometheus text format of a [`MeterRegistry`], as Micrometer's `PrometheusMeterRegistry`
//! writes it: names with `.` as `_`, counters with `_total`, timers in seconds as a summary
//! (`<name>_seconds_count`, `<name>_seconds_sum`) and a gauge `<name>_seconds_max`, distribution
//! summaries likewise without the unit, long task timers as `<name>_seconds_active_count`,
//! `<name>_seconds_duration_sum` and `<name>_seconds_max`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use super::{MeterId, MeterKind, MeterRegistry, Registered};

/// A Prometheus metric or label name: characters outside `[a-zA-Z0-9_:]` become `_`, and a
/// leading digit gets a `_` in front.
pub fn sanitize(name: &str) -> String {
    let mut s: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == ':' { c } else { '_' }).collect();
    if s.starts_with(|c: char| c.is_ascii_digit()) {
        s.insert(0, '_');
    }
    s
}

fn labels(id: &MeterId, extra: Option<(&str, &str)>) -> String {
    let mut parts: Vec<String> = id.tags.iter().map(|(k, v)| format!("{}=\"{}\"", sanitize(k).trim_start_matches(':'), escape(v))).collect();
    if let Some((k, v)) = extra {
        parts.push(format!("{}=\"{}\"", k, v));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("{{{}}}", parts.join(","))
    }
}

fn escape(v: &str) -> String {
    v.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

fn num(v: f64) -> String {
    if v.is_finite() && v == v.trunc() && v.abs() < 1e15 {
        format!("{:.1}", v)
    } else {
        format!("{}", v)
    }
}

/// One family: TYPE, HELP and sample lines.
#[derive(Default)]
struct Family {
    kind: &'static str,
    help: String,
    lines: Vec<String>,
}

fn add(out: &mut BTreeMap<String, Family>, name: String, kind: &'static str, help: &str, line: String) {
    let f = out.entry(name).or_default();
    f.kind = kind;
    if f.help.is_empty() {
        f.help = help.to_string();
    }
    f.lines.push(line);
}

fn with_unit(base: &str, unit: &str) -> String {
    if unit.is_empty() || base.ends_with(&format!("_{}", unit)) {
        base.to_string()
    } else {
        format!("{}_{}", base, unit)
    }
}

/// Renders the meters. Families named in `skip` (already written by another part of the
/// endpoint) are left out, so a name never has two `# TYPE` lines.
pub fn render(registry: &MeterRegistry, skip: &BTreeSet<String>) -> String {
    render_meters(&registry.meters(), skip)
}

/// Renders a list of meters (see [`render`]).
pub fn render_meters(meters: &[(MeterId, Registered)], skip: &BTreeSet<String>) -> String {
    let mut fams: BTreeMap<String, Family> = BTreeMap::new();
    for (id, r) in meters {
        let base = sanitize(&id.name);
        let help = r.description.clone().unwrap_or_else(|| id.name.clone());
        let l = labels(id, None);
        match &r.kind {
            MeterKind::Counter(c) => {
                let n = if base.ends_with("_total") { base.clone() } else { format!("{}_total", base) };
                add(&mut fams, n.clone(), "counter", &help, format!("{}{} {}", n, l, num(c.count())));
            }
            MeterKind::Meter(m) => {
                let n = if base.ends_with("_total") { base.clone() } else { format!("{}_total", base) };
                add(&mut fams, n.clone(), "counter", &help, format!("{}{} {}", n, l, num(m.count() as f64)));
            }
            MeterKind::UpDownCounter(g) => {
                let n = with_unit(&base, &sanitize(&r.unit));
                add(&mut fams, n.clone(), "gauge", &help, format!("{}{} {}", n, l, num(g.value())))
            }
            MeterKind::Gauge(g) => add(&mut fams, base.clone(), "gauge", &help, format!("{}{} {}", base, l, num(g.value()))),
            MeterKind::FnGauge(g) => {
                let n = with_unit(&base, &sanitize(&r.unit));
                add(&mut fams, n.clone(), "gauge", &help, format!("{}{} {}", n, l, num((g.0)())))
            }
            MeterKind::Timer(t) => {
                let n = with_unit(&base, "seconds");
                add(&mut fams, n.clone(), "summary", &help, format!("{}_count{} {}", n, l, num(t.0.count() as f64)));
                add(&mut fams, n.clone(), "summary", &help, format!("{}_sum{} {}", n, l, num(t.0.total())));
                add(&mut fams, format!("{}_max", n), "gauge", &help, format!("{}_max{} {}", n, l, num(t.0.max())));
            }
            MeterKind::Summary(d) => {
                let n = with_unit(&base, &sanitize(&r.unit));
                add(&mut fams, n.clone(), "summary", &help, format!("{}_count{} {}", n, l, num(d.count() as f64)));
                add(&mut fams, n.clone(), "summary", &help, format!("{}_sum{} {}", n, l, num(d.total())));
                add(&mut fams, format!("{}_max", n), "gauge", &help, format!("{}_max{} {}", n, l, num(d.max())));
            }
            MeterKind::LongTaskTimer(t) => {
                let n = with_unit(&base, "seconds");
                let (total, max) = t.durations();
                add(&mut fams, format!("{}_active_count", n), "gauge", &help, format!("{}_active_count{} {}", n, l, num(t.active_tasks() as f64)));
                add(&mut fams, format!("{}_duration_sum", n), "gauge", &help, format!("{}_duration_sum{} {}", n, l, num(total)));
                add(&mut fams, format!("{}_max", n), "gauge", &help, format!("{}_max{} {}", n, l, num(max)));
            }
        }
    }
    let mut out = String::new();
    for (name, f) in fams {
        // a family is skipped by its TYPE name (`x_total`, `x_seconds`) or its base
        if skip.contains(&name) {
            continue;
        }
        let _ = writeln!(out, "# HELP {} {}", name, f.help.replace('\n', " "));
        let _ = writeln!(out, "# TYPE {} {}", name, f.kind);
        for l in f.lines {
            let _ = writeln!(out, "{}", l);
        }
    }
    out
}

/// The family names (`# TYPE` lines) of a Prometheus text.
pub fn family_names(text: &str) -> BTreeSet<String> {
    text.lines().filter_map(|l| l.strip_prefix("# TYPE ")).filter_map(|l| l.split_whitespace().next()).map(str::to_string).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn micrometer_style_names_and_skipping() {
        let r = MeterRegistry::default();
        let t = vec![("camelContext".to_string(), "c-1".to_string()), ("service.name".to_string(), "a\"b".to_string())];
        r.counter("orders.placed", &t, Some("Orders placed")).unwrap().increment(3.0);
        r.timer("camel.exchange.event.notifier", &t, None).unwrap().record(Duration::from_millis(250));
        r.summary("order.amount", &[], None).unwrap().record(12.5);
        r.gauge("queue.depth", &[], None).unwrap().set(7.0);
        r.fn_gauge("answer", &[], None, || 42.0);
        let text = render(&r, &BTreeSet::new());
        assert!(text.contains("# TYPE orders_placed_total counter\norders_placed_total{camelContext=\"c-1\",service_name=\"a\\\"b\"} 3.0\n"), "{text}");
        assert!(text.contains("# HELP orders_placed_total Orders placed\n"));
        assert!(text.contains("# TYPE camel_exchange_event_notifier_seconds summary\n"));
        assert!(text.contains("camel_exchange_event_notifier_seconds_count{camelContext=\"c-1\",service_name=\"a\\\"b\"} 1.0\n"));
        assert!(text.contains("camel_exchange_event_notifier_seconds_sum{camelContext=\"c-1\",service_name=\"a\\\"b\"} 0.25\n"));
        assert!(text.contains("# TYPE camel_exchange_event_notifier_seconds_max gauge\n"));
        assert!(text.contains("order_amount_count 1.0\norder_amount_sum 12.5\n"));
        assert!(text.contains("queue_depth 7.0\n") && text.contains("answer 42.0\n"));
        let skip: BTreeSet<String> = ["orders_placed_total".to_string()].into();
        assert!(!render(&r, &skip).contains("orders_placed_total"));
        assert_eq!(family_names(&text).len(), 7);
        assert_eq!(sanitize("1a.b-c"), "_1a_b_c");
    }
}

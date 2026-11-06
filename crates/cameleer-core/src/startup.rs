//! Startup step recording (Camel's `StartupStepRecorder`).
//!
//! Startup is measured per process: the steps of starting an application (registering
//! components, loading configuration and route files, creating and starting routes) are
//! recorded on a process-wide recorder. Recording costs two clock reads per step, so it is
//! always on; Camel Main prints the steps when `camel.main.startupRecorder=logging` (or the
//! environment variable `CAMEL_MAIN_STARTUP_RECORDER=logging`).
//!
//! [`mark_process_start`] sets the origin of the timeline; binaries call it first in `main`
//! (the `cameleer` CLI does). Without it the origin is the first recorded step.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// One recorded step.
#[derive(Clone, Debug)]
pub struct StartupStep {
    /// Step type, e.g. `routes`, `route`, `components` (Camel's `getType()`).
    pub kind: &'static str,
    /// What was done, e.g. a route id or a file name (Camel's `getName()`).
    pub name: String,
    /// Nesting level (0 = top).
    pub level: usize,
    /// Start, relative to the process start mark.
    pub offset: Duration,
    pub duration: Duration,
}

struct Recorder {
    origin: Option<Instant>,
    steps: Vec<StartupStep>,
    depth: usize,
}

static RECORDER: Mutex<Recorder> = Mutex::new(Recorder { origin: None, steps: Vec::new(), depth: 0 });

/// Upper bound on recorded steps (a process that keeps adding routes must not grow forever).
const MAX_STEPS: usize = 1000;

/// Set the origin of the startup timeline to now. Later calls are ignored.
pub fn mark_process_start() {
    let mut r = RECORDER.lock().unwrap_or_else(|e| e.into_inner());
    r.origin.get_or_insert_with(Instant::now);
}

/// Time since the process start mark (zero when not marked).
pub fn elapsed() -> Duration {
    let r = RECORDER.lock().unwrap_or_else(|e| e.into_inner());
    r.origin.map(|o| o.elapsed()).unwrap_or_default()
}

/// Run `f` as a recorded step.
pub fn step<T>(kind: &'static str, name: impl Into<String>, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let level = {
        let mut r = RECORDER.lock().unwrap_or_else(|e| e.into_inner());
        r.origin.get_or_insert(start);
        r.depth += 1;
        r.depth - 1
    };
    let out = f();
    let duration = start.elapsed();
    let mut r = RECORDER.lock().unwrap_or_else(|e| e.into_inner());
    r.depth = r.depth.saturating_sub(1);
    if r.steps.len() < MAX_STEPS {
        let offset = start.saturating_duration_since(r.origin.unwrap_or(start));
        r.steps.push(StartupStep { kind, name: name.into(), level, offset, duration });
    }
    out
}

/// The recorded steps in start order.
pub fn steps() -> Vec<StartupStep> {
    let r = RECORDER.lock().unwrap_or_else(|e| e.into_inner());
    let mut v = r.steps.clone();
    v.sort_by_key(|s| s.offset);
    v
}

/// Format a duration for the startup report: `850 µs`, `12.3 ms`.
pub fn format_duration(d: Duration) -> String {
    let us = d.as_secs_f64() * 1e6;
    if us < 1000.0 {
        format!("{:.0} µs", us)
    } else {
        format!("{:.2} ms", us / 1000.0)
    }
}

/// The report lines Camel Main logs with `startupRecorder=logging`: one line per step,
/// indented by nesting level, with its offset from the process start and its duration.
pub fn report_lines() -> Vec<String> {
    steps()
        .iter()
        .map(|s| {
            format!(
                "{:>10} {:>10}  {}{} {}",
                format_duration(s.offset),
                format_duration(s.duration),
                "  ".repeat(s.level),
                s.kind,
                s.name
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_nested_steps_with_levels() {
        mark_process_start();
        let v = step("test-outer", "a", || step("test-inner", "b", || 41) + 1);
        assert_eq!(v, 42);
        let s = steps();
        let outer = s.iter().find(|s| s.kind == "test-outer").unwrap();
        let inner = s.iter().find(|s| s.kind == "test-inner").unwrap();
        assert_eq!(inner.level, outer.level + 1);
        assert!(inner.offset >= outer.offset);
        assert!(outer.duration >= inner.duration);
        assert!(report_lines().iter().any(|l| l.contains("test-inner b")));
    }

    #[test]
    fn formats_micro_and_milliseconds() {
        assert_eq!(format_duration(Duration::from_micros(850)), "850 µs");
        assert_eq!(format_duration(Duration::from_micros(12_340)), "12.34 ms");
    }
}

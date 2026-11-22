//! Start-up regression guard (docs/design/startup-regression-2026-11.md).
//!
//! simoom grows the process's descriptor table when a server starts. Linux grows it with a
//! plain reallocation while one thread uses it, but waits for an RCU grace period (tens of
//! milliseconds) once a second thread exists. `cameleer_http::register` therefore grows the table
//! while the process is still single-threaded, which only holds if nothing has spawned a thread
//! first. This test has no libtest harness, so it runs on the process's only thread, and checks
//! the table size the kernel reports (`FDSize`), which does not depend on timing.

#[cfg(target_os = "linux")]
fn field(file: &str, name: &str) -> Option<String> {
    std::fs::read_to_string(file).ok()?.lines().find_map(|l| l.strip_prefix(name).map(|v| v.trim().to_string()))
}

#[cfg(target_os = "linux")]
fn fd_size() -> usize {
    field("/proc/self/status", "FDSize:").and_then(|v| v.parse().ok()).expect("FDSize in /proc/self/status")
}

#[cfg(target_os = "linux")]
fn main() {
    let soft: usize = field("/proc/self/limits", "Max open files").and_then(|v| v.split_whitespace().next().and_then(|n| n.parse().ok())).unwrap_or(0);
    let before = fd_size();
    if soft < 1024 || before >= 1024 {
        println!("early_descriptors: skipped (soft limit {soft}, table {before})");
        return;
    }
    let ctx = cameleer_core::context::CamelContext::new();
    cameleer_http::register(&ctx);
    let after = fd_size();
    assert!(after >= 1024, "register did not grow the descriptor table: {before} -> {after}");
    // a second call is free
    cameleer_http::server_simoom::reserve_descriptors_early();
    assert_eq!(fd_size(), after);
    println!("early_descriptors: ok (table {before} -> {after})");
}

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("early_descriptors: skipped (Linux only)");
}

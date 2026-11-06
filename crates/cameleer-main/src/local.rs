//! Local process management used by the `cameleer` CLI (like Camel JBang's `~/.camel`
//! directory): a running integration publishes `<pid>.json` (name, routes, context,
//! health, backlog trace), logs to `<pid>.log`, and executes the commands
//! written to `<pid>-action.json` (`stop`, `reload`, `start-route`, `stop-route`,
//! `suspend-route`, `resume-route`, `reset-stats`, `trace`, and the `tool-*` actions of
//! [`crate::tools`]). The answer goes to `<pid>-action-result.json`: `status` (`ok` or
//! `error`), `message`, and `data` for actions that return a document. The directory is
//! `$CAMEL_HOME` or `~/.camel`.
//!
//! The status is checked every 500 ms but the file is only rewritten when it changed, not
//! counting the fields that only move with the clock (`updated`, `uptime`, `uptimeMillis`):
//! readers ([`list_processes`]) bring those up to date from `updated`, the time of the
//! snapshot. Whether the process is still alive is asked of the operating system.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cameleer_core::component::support::{BackgroundThreads, StopFlag};
use cameleer_core::prelude::*;

/// Whether `cameleer run` writes status and log files: off when `CAMEL_LOCAL_STATUS` is
/// `false`, `0`, `off` or `no`.
pub fn local_status_enabled() -> bool {
    !matches!(std::env::var("CAMEL_LOCAL_STATUS").map(|v| v.trim().to_ascii_lowercase()).as_deref(), Ok("false" | "0" | "off" | "no"))
}

/// The directory holding the status files.
pub fn camel_home() -> PathBuf {
    if let Ok(h) = std::env::var("CAMEL_HOME") {
        if !h.is_empty() {
            return PathBuf::from(h);
        }
    }
    std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir()).join(".camel")
}

pub fn status_file(dir: &Path, pid: u32) -> PathBuf {
    dir.join(format!("{}.json", pid))
}
pub fn action_file(dir: &Path, pid: u32) -> PathBuf {
    dir.join(format!("{}-action.json", pid))
}
pub fn log_file(dir: &Path, pid: u32) -> PathBuf {
    dir.join(format!("{}.log", pid))
}

/// The status document of a context.
pub fn status_value(ctx: &CamelContext, name: &str, pid: u32) -> Value {
    let mc = ctx.managed_context();
    let t = ctx.tracer();
    Value::object([
        ("pid", Value::from(pid as i64)),
        ("name", Value::from(name)),
        ("updated", Value::from(cameleer_core::util_time::now_millis())),
        ("context", mc.to_value()),
        ("routes", mc.routes_value()),
        ("endpoints", mc.endpoints_value()),
        ("health", ctx.health().to_value()),
        ("trace", Value::object([("enabled", Value::Bool(t.is_backlog_enabled())), ("events", t.backlog_value())])),
    ])
}

/// Fields that change with the clock alone (left out when deciding whether to rewrite).
const CLOCK_FIELDS: [&str; 3] = ["updated", "uptime", "uptimeMillis"];

/// The status without the fields that change with the clock alone: the status file is
/// rewritten only when this changes.
pub fn status_fingerprint(status: &Value) -> String {
    fn strip(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let mut out = cameleer_core::value::ObjectMap::new();
                for (k, v) in m.iter() {
                    if !CLOCK_FIELDS.contains(&k.as_str()) {
                        out.insert(k.clone(), strip(v));
                    }
                }
                Value::Object(out)
            }
            Value::Array(a) => Value::Array(a.iter().map(strip).collect()),
            other => other.clone(),
        }
    }
    strip(status).to_json()
}

/// Bring the uptimes of a status read from a file up to date: the context and the started
/// routes have been up for the time since the snapshot (`updated`) as well.
pub fn refresh_uptimes(status: &mut Value, now_millis: i64) {
    let Some(updated) = status.get("updated").and_then(|v| v.as_i64()) else { return };
    let elapsed = (now_millis - updated).max(0) as u64;
    if elapsed == 0 {
        return;
    }
    fn bump(m: &mut cameleer_core::value::ObjectMap, elapsed: u64) {
        if m.get("state").map(|s| s.to_string()).as_deref() != Some("Started") {
            return;
        }
        let Some(ms) = m.get("uptimeMillis").and_then(|v| v.as_i64()) else { return };
        let ms = ms.max(0) as u64 + elapsed;
        m.insert("uptimeMillis".into(), Value::from(ms));
        m.insert("uptime".into(), Value::from(cameleer_core::management::format_uptime(ms)));
    }
    let Value::Object(top) = status else { return };
    if let Some(Value::Object(c)) = top.get_mut("context") {
        bump(c, elapsed);
    }
    if let Some(Value::Array(routes)) = top.get_mut("routes") {
        for r in routes.iter_mut() {
            if let Value::Object(r) = r {
                bump(r, elapsed);
            }
        }
    }
}

fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// Execute one action document on a context. Returns the result message.
pub fn execute_action(ctx: &CamelContext, action: &Value) -> Result<String> {
    let name = action.get("action").map(|a| a.to_string()).unwrap_or_default();
    let ids: Vec<String> = match action.get("id").map(|v| v.to_string()) {
        Some(id) if id != "*" && !id.is_empty() => vec![id],
        _ => ctx.route_ids(),
    };
    let mc = ctx.managed_context();
    for id in &ids {
        match name.as_str() {
            "start-route" => mc.start_route(id)?,
            "stop-route" => mc.stop_route(id)?,
            "suspend-route" => mc.suspend_route(id)?,
            "resume-route" => mc.resume_route(id)?,
            "reset-stats" => mc.reset_route_statistics(id)?,
            "trace" => {
                let on = action.get("enabled").map(|v| v.is_truthy()).unwrap_or(true);
                ctx.set_backlog_tracing(on);
                return Ok(format!("backlog tracing {}", if on { "enabled" } else { "disabled" }));
            }
            other => return Err(CamelError::illegal_argument(format!("Unknown action {}", other))),
        }
    }
    Ok(format!("{} {}", name, ids.join(",")))
}

/// A running integration's status publisher.
pub struct LocalStatus {
    dir: PathBuf,
    pid: u32,
    threads: BackgroundThreads,
    stop_requested: Arc<AtomicBool>,
    reload_requested: Arc<AtomicBool>,
}

impl LocalStatus {
    /// Start publishing the status of `ctx` in `dir`.
    pub fn start(ctx: &CamelContext, name: &str, dir: &Path) -> Result<LocalStatus> {
        std::fs::create_dir_all(dir).map_err(|e| CamelError::io(format!("Cannot create {}: {}", dir.display(), e)))?;
        let pid = std::process::id();
        let ls = LocalStatus {
            dir: dir.to_path_buf(),
            pid,
            threads: BackgroundThreads::new(),
            stop_requested: Arc::new(AtomicBool::new(false)),
            reload_requested: Arc::new(AtomicBool::new(false)),
        };
        let (weak, name, dir) = (ctx.downgrade(), name.to_string(), dir.to_path_buf());
        let (stop, reload) = (ls.stop_requested.clone(), ls.reload_requested.clone());
        let mut last = String::new();
        let publish = move |flag: Arc<StopFlag>| loop {
            let Some(ctx) = weak.upgrade() else { return };
            if let Ok(text) = std::fs::read_to_string(action_file(&dir, pid)) {
                let _ = std::fs::remove_file(action_file(&dir, pid));
                let result = match cameleer_core::json::parse(&text) {
                    Ok(a) => match a.get("action").map(|x| x.to_string()).as_deref() {
                        Some("stop") => {
                            stop.store(true, Ordering::SeqCst);
                            Ok(("stopping".to_string(), None))
                        }
                        Some("reload") => {
                            reload.store(true, Ordering::SeqCst);
                            Ok(("reloading".to_string(), None))
                        }
                        // the AI tools' kill switch and approvals (`cameleer tool ...`)
                        _ if crate::tools::is_tool_action(&a) => crate::tools::execute(&ctx, &a).map(|(m, d)| (m, Some(d))),
                        _ => execute_action(&ctx, &a).map(|m| (m, None)),
                    },
                    Err(e) => Err(e),
                };
                let out = match result {
                    Ok((m, None)) => Value::object([("status", Value::from("ok")), ("message", Value::from(m))]),
                    Ok((m, Some(d))) => Value::object([("status", Value::from("ok")), ("message", Value::from(m)), ("data", d)]),
                    Err(e) => Value::object([("status", Value::from("error")), ("message", Value::from(e.to_string()))]),
                };
                let _ = write_atomic(&dir.join(format!("{}-action-result.json", pid)), &out.to_json());
            }
            let status = status_value(&ctx, &name, pid);
            let fingerprint = status_fingerprint(&status);
            let file = status_file(&dir, pid);
            // rewrite when the content changed (or the file was removed)
            if fingerprint != last || !file.exists() {
                if write_atomic(&file, &status.to_json()).is_ok() {
                    last = fingerprint;
                }
            }
            drop(ctx);
            if flag.sleep(Duration::from_millis(500)) {
                return;
            }
        };
        ls.threads.spawn("Camel local status", publish)?;
        Ok(ls)
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }
    pub fn dir(&self) -> &Path {
        &self.dir
    }
    /// A `stop` action was received.
    pub fn stop_requested(&self) -> bool {
        self.stop_requested.load(Ordering::SeqCst)
    }
    /// A `reload` action was received (the flag is cleared).
    pub fn take_reload_request(&self) -> bool {
        self.reload_requested.swap(false, Ordering::SeqCst)
    }

    /// Stop publishing and remove the status files (the result of the last action is left
    /// for the CLI waiting for it; `send_action` removes it).
    pub fn stop(&self) {
        self.threads.stop();
        for f in [status_file(&self.dir, self.pid), action_file(&self.dir, self.pid)] {
            let _ = std::fs::remove_file(f);
        }
    }
}

impl Drop for LocalStatus {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Write log records to a file (`<pid>.log`).
pub fn log_to_file(path: &Path) -> Result<()> {
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| CamelError::io(format!("Cannot open log file {}: {}", path.display(), e)))?;
    let file = Mutex::new(file);
    cameleer_core::log::add_sink(move |r| {
        use std::io::Write;
        let line = format!(
            "{} {:5} [{}] {} : {}\n",
            cameleer_core::log::format_timestamp(r.timestamp_millis),
            r.level.as_str(),
            std::thread::current().name().unwrap_or("main"),
            r.logger,
            r.message
        );
        let _ = file.lock().unwrap().write_all(line.as_bytes());
    });
    Ok(())
}

/// A running integration found in the status directory.
#[derive(Clone, Debug)]
pub struct LocalProcess {
    pub pid: u32,
    pub name: String,
    pub status: Value,
}

/// Whether process `pid` is running.
#[cfg(unix)]
pub fn alive(pid: u32) -> bool {
    let proc_dir = Path::new("/proc");
    if proc_dir.join("self").exists() {
        return proc_dir.join(pid.to_string()).exists();
    }
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    const EPERM: i32 = 1;
    let Ok(p) = i32::try_from(pid) else { return false };
    // SAFETY: signal 0 only checks that the process exists (and may be signalled).
    let ok = unsafe { kill(p, 0) } == 0;
    ok || std::io::Error::last_os_error().raw_os_error() == Some(EPERM)
}

/// Whether process `pid` is running.
#[cfg(windows)]
pub fn alive(pid: u32) -> bool {
    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
        fn GetExitCodeProcess(process: isize, code: *mut u32) -> i32;
        fn CloseHandle(handle: isize) -> i32;
    }
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;
    // SAFETY: plain Win32 calls; the handle is closed before returning.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h == 0 {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code) != 0;
        CloseHandle(h);
        ok && code == STILL_ACTIVE
    }
}

/// Whether process `pid` is running (unknown on this platform: assumed so).
#[cfg(not(any(unix, windows)))]
pub fn alive(_pid: u32) -> bool {
    true
}

/// The running integrations (stale status files of dead processes are removed).
pub fn list_processes(dir: &Path) -> Vec<LocalProcess> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        let Some(stem) = p.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".json")) else { continue };
        let Ok(pid) = stem.parse::<u32>() else { continue };
        let Some(mut status) = std::fs::read_to_string(&p).ok().and_then(|t| cameleer_core::json::parse(&t).ok()) else { continue };
        if !alive(pid) {
            let _ = std::fs::remove_file(&p);
            continue;
        }
        refresh_uptimes(&mut status, cameleer_core::util_time::now_millis());
        let name = status.get("name").map(|n| n.to_string()).unwrap_or_default();
        out.push(LocalProcess { pid, name, status });
    }
    out.sort_by_key(|p| p.pid);
    out
}

/// Find a process by pid or name (the only one when `selector` is None).
pub fn find_process(dir: &Path, selector: Option<&str>) -> Result<LocalProcess> {
    let all = list_processes(dir);
    let found: Vec<LocalProcess> = match selector {
        Some(s) => all.into_iter().filter(|p| p.pid.to_string() == s || p.name == s || cameleer_core::util::wildcard_match(&p.name, s)).collect(),
        None => all,
    };
    match found.len() {
        0 => Err(CamelError::illegal_argument(match selector {
            Some(s) => format!("No running integration matching {}", s),
            None => "No running integration".to_string(),
        })),
        1 => Ok(found.into_iter().next().unwrap()),
        _ => Err(CamelError::illegal_argument(format!(
            "Several running integrations ({}); select one by name or pid",
            found.iter().map(|p| format!("{} ({})", p.name, p.pid)).collect::<Vec<_>>().join(", ")
        ))),
    }
}

/// Send an action to a running integration and wait (up to `timeout`) for its result.
pub fn send_action(dir: &Path, pid: u32, action: &Value, timeout: Duration) -> Result<Value> {
    let result = dir.join(format!("{}-action-result.json", pid));
    let _ = std::fs::remove_file(&result);
    write_atomic(&action_file(dir, pid), &action.to_json()).map_err(|e| CamelError::io(e.to_string()))?;
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if let Ok(t) = std::fs::read_to_string(&result) {
            let _ = std::fs::remove_file(&result);
            return cameleer_core::json::parse(&t);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(CamelError::new(kinds::TIMEOUT, format!("No answer from integration {} within {:?}", pid, timeout)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn idle_context() -> CamelContext {
        let ctx = CamelContext::new();
        ctx.add_routes(|b| {
            b.from("direct:in").route_id("r1").to("log:local-test?level=OFF");
            b.from("direct:off").route_id("r2").auto_startup(false).to("log:local-test?level=OFF");
        })
        .unwrap();
        ctx.start().unwrap();
        ctx
    }

    #[test]
    fn fingerprint_ignores_the_clock_but_not_the_content() {
        let ctx = idle_context();
        let a = status_value(&ctx, "app", 1);
        std::thread::sleep(Duration::from_millis(1100));
        let b = status_value(&ctx, "app", 1);
        assert_ne!(a.to_json(), b.to_json(), "the clock fields moved");
        assert_eq!(status_fingerprint(&a), status_fingerprint(&b), "an idle context's status does not change");
        ctx.create_producer_template().send_body("direct:in", "x").unwrap();
        let c = status_value(&ctx, "app", 1);
        assert_ne!(status_fingerprint(&b), status_fingerprint(&c), "an exchange changes the status");
        ctx.stop().unwrap();
    }

    #[test]
    fn readers_bring_uptimes_up_to_date() {
        let ctx = idle_context();
        let mut s = status_value(&ctx, "app", 1);
        let updated = s.get("updated").and_then(|v| v.as_i64()).unwrap();
        let ctx_up = s.get("context").and_then(|c| c.get("uptimeMillis")).and_then(|v| v.as_i64()).unwrap();
        refresh_uptimes(&mut s, updated + 65_000);
        let c = s.get("context").unwrap();
        assert_eq!(c.get("uptimeMillis").and_then(|v| v.as_i64()), Some(ctx_up + 65_000));
        assert_eq!(c.get("uptime").map(|v| v.to_string()), Some(cameleer_core::management::format_uptime((ctx_up + 65_000) as u64)));
        let routes = s.get("routes").and_then(|r| r.as_array()).unwrap();
        let up = |id: &str| routes.iter().find(|r| r.get("routeId").map(|v| v.to_string()).as_deref() == Some(id)).and_then(|r| r.get("uptimeMillis")).and_then(|v| v.as_i64()).unwrap();
        assert!(up("r1") >= 65_000, "a started route keeps counting");
        assert_eq!(up("r2"), 0, "a stopped route has no uptime");
        ctx.stop().unwrap();
    }

    #[test]
    fn liveness_comes_from_the_operating_system() {
        assert!(alive(std::process::id()));
        let exe = std::env::current_exe().unwrap();
        let mut child = std::process::Command::new(exe).arg("--exact").arg("no-such-test").stdout(std::process::Stdio::null()).spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        // the pid may be reused, but not this quickly in practice
        assert!(!alive(pid), "exited child {pid} still alive");
    }

    #[test]
    fn the_status_file_is_rewritten_only_when_the_status_changes() {
        let dir = std::env::temp_dir().join(format!("cameleer-local-writes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ctx = idle_context();
        let ls = LocalStatus::start(&ctx, "writes", &dir).unwrap();
        let file = status_file(&dir, ls.pid());
        let read = || std::fs::read_to_string(&file).ok().and_then(|t| cameleer_core::json::parse(&t).ok());
        let stamp = |v: &Value| v.get("updated").and_then(|u| u.as_i64()).unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        let first = loop {
            if let Some(v) = read() {
                break v;
            }
            assert!(Instant::now() < end, "no status file");
            std::thread::sleep(Duration::from_millis(10));
        };
        // idle: three publishing rounds go by without a rewrite
        std::thread::sleep(Duration::from_millis(1600));
        assert_eq!(stamp(&read().unwrap()), stamp(&first), "idle status rewritten");
        // the listing still reports a growing uptime
        let listed = list_processes(&dir).into_iter().find(|p| p.pid == ls.pid()).expect("listed");
        let up = |v: &Value| v.get("context").and_then(|c| c.get("uptimeMillis")).and_then(|u| u.as_i64()).unwrap();
        assert!(up(&listed.status) >= up(&first) + 1500, "{} vs {}", up(&listed.status), up(&first));
        // a change is published
        ctx.create_producer_template().send_body("direct:in", "x").unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            let v = read().unwrap();
            if v.get("context").and_then(|c| c.get("exchangesTotal")).and_then(|t| t.as_i64()) == Some(1) {
                assert!(stamp(&v) > stamp(&first));
                break;
            }
            assert!(Instant::now() < end, "change not published");
            std::thread::sleep(Duration::from_millis(20));
        }
        // a removed file is written again
        std::fs::remove_file(&file).unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        while read().is_none() {
            assert!(Instant::now() < end, "status file not restored");
            std::thread::sleep(Duration::from_millis(20));
        }
        ls.stop();
        ctx.stop().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

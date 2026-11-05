//! Local process management used by the `camel` CLI (like Camel JBang's `~/.camel`
//! directory): a running integration publishes `<pid>.json` (name, routes, context,
//! health, backlog trace) every 500 ms, logs to `<pid>.log`, and executes the commands
//! written to `<pid>-action.json` (`stop`, `reload`, `start-route`, `stop-route`,
//! `suspend-route`, `resume-route`, `reset-stats`, `trace`). The directory is
//! `$CAMEL_HOME` or `~/.camel`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cameleer_core::component::support::{BackgroundThreads, StopFlag};
use cameleer_core::prelude::*;

/// Whether `camel run` writes status and log files: off when `CAMEL_LOCAL_STATUS` is
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
        let publish = move |flag: Arc<StopFlag>| loop {
            let Some(ctx) = weak.upgrade() else { return };
            if let Ok(text) = std::fs::read_to_string(action_file(&dir, pid)) {
                let _ = std::fs::remove_file(action_file(&dir, pid));
                let result = match cameleer_core::json::parse(&text) {
                    Ok(a) => match a.get("action").map(|x| x.to_string()).as_deref() {
                        Some("stop") => {
                            stop.store(true, Ordering::SeqCst);
                            Ok("stopping".to_string())
                        }
                        Some("reload") => {
                            reload.store(true, Ordering::SeqCst);
                            Ok("reloading".to_string())
                        }
                        _ => execute_action(&ctx, &a),
                    },
                    Err(e) => Err(e),
                };
                let out = match result {
                    Ok(m) => Value::object([("status", Value::from("ok")), ("message", Value::from(m))]),
                    Err(e) => Value::object([("status", Value::from("error")), ("message", Value::from(e.to_string()))]),
                };
                let _ = write_atomic(&dir.join(format!("{}-action-result.json", pid)), &out.to_json());
            }
            let _ = write_atomic(&status_file(&dir, pid), &status_value(&ctx, &name, pid).to_json());
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

fn alive(pid: u32, status: &Value) -> bool {
    let proc_dir = Path::new("/proc");
    if proc_dir.is_dir() {
        return proc_dir.join(pid.to_string()).exists();
    }
    // no /proc: consider the process alive while it keeps updating its status
    let updated = status.get("updated").and_then(|v| v.as_i64()).unwrap_or(0);
    cameleer_core::util_time::now_millis() - updated < 5000
}

/// The running integrations (stale status files of dead processes are removed).
pub fn list_processes(dir: &Path) -> Vec<LocalProcess> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        let Some(stem) = p.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".json")) else { continue };
        let Ok(pid) = stem.parse::<u32>() else { continue };
        let Some(status) = std::fs::read_to_string(&p).ok().and_then(|t| cameleer_core::json::parse(&t).ok()) else { continue };
        if !alive(pid, &status) {
            let _ = std::fs::remove_file(&p);
            continue;
        }
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

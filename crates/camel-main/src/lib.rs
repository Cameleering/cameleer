//! # camel-main
//!
//! Standalone runner for rust-camel applications (Camel Main equivalent):
//! loads `application.properties`, configures the context, discovers route files,
//! starts the context and keeps running until stopped (signal, max duration,
//! max messages or idle timeout).

pub mod properties;
pub mod signal;
pub mod yaml_rest;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use camel_core::context::CamelEvent;
use camel_core::prelude::*;
use camel_core::spi::EventNotifier;

/// Loads routes from a file's content into a context (registered per file extension).
pub type RouteLoader = Arc<dyn Fn(&CamelContext, &str, &Path) -> Result<()> + Send + Sync>;

/// Configuration options (from `camel.main.*` properties or code).
#[derive(Clone, Debug, Default)]
pub struct MainConfig {
    pub name: Option<String>,
    /// Glob patterns for route files, comma separated (e.g. `routes/*.yaml`).
    pub routes_include_pattern: Vec<String>,
    pub duration_max_seconds: Option<u64>,
    pub duration_max_messages: Option<u64>,
    pub duration_max_idle_seconds: Option<u64>,
    pub shutdown_timeout: Option<Duration>,
    pub log_level: Option<String>,
}

/// The main runner.
pub struct Main {
    ctx: CamelContext,
    config: MainConfig,
    properties: BTreeMap<String, String>,
    loaders: BTreeMap<String, RouteLoader>,
    route_files: Vec<PathBuf>,
    configurers: Vec<Box<dyn FnOnce(&CamelContext) -> Result<()>>>,
}

struct CompletedCounter {
    completed: Arc<AtomicU64>,
    last_activity: Arc<std::sync::Mutex<Instant>>,
}

impl EventNotifier for CompletedCounter {
    fn notify(&self, event: &CamelEvent) {
        match event {
            CamelEvent::ExchangeCompleted { .. } | CamelEvent::ExchangeFailed { .. } => {
                self.completed.fetch_add(1, Ordering::SeqCst);
                *self.last_activity.lock().unwrap() = Instant::now();
            }
            CamelEvent::ExchangeCreated { .. } => {
                *self.last_activity.lock().unwrap() = Instant::now();
            }
            _ => {}
        }
    }
}

impl Default for Main {
    fn default() -> Self {
        Self::new()
    }
}

impl Main {
    pub fn new() -> Self {
        Main {
            ctx: CamelContext::new(),
            config: MainConfig::default(),
            properties: BTreeMap::new(),
            loaders: BTreeMap::new(),
            route_files: Vec::new(),
            configurers: Vec::new(),
        }
    }

    /// Use an existing context.
    pub fn with_context(ctx: CamelContext) -> Self {
        Main { ctx, ..Self::new() }
    }

    pub fn context(&self) -> &CamelContext {
        &self.ctx
    }

    pub fn config_mut(&mut self) -> &mut MainConfig {
        &mut self.config
    }

    /// Register a route loader for a file extension (`yaml`, `yml`, `xml`, ...).
    pub fn add_route_loader(&mut self, extension: &str, loader: impl Fn(&CamelContext, &str, &Path) -> Result<()> + Send + Sync + 'static) {
        self.loaders.insert(extension.to_ascii_lowercase(), Arc::new(loader));
    }

    pub fn supported_extensions(&self) -> Vec<String> {
        self.loaders.keys().cloned().collect()
    }

    /// Add a route file to load at startup.
    pub fn add_route_file(&mut self, path: impl Into<PathBuf>) {
        self.route_files.push(path.into());
    }

    /// Add routes with the Rust DSL.
    pub fn configure(&mut self, f: impl FnOnce(&CamelContext) -> Result<()> + 'static) {
        self.configurers.push(Box::new(f));
    }

    /// Set a property (same as in application.properties).
    pub fn set_property(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.properties.insert(key.into(), value.into());
    }

    /// Load an application.properties file.
    pub fn load_properties_file(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let text = std::fs::read_to_string(path.as_ref())
            .map_err(|e| CamelError::io(format!("Cannot read properties file {}: {}", path.as_ref().display(), e)))?;
        self.properties.extend(properties::parse_properties(&text));
        Ok(())
    }

    /// Apply `camel.main.*` and other well-known properties to the configuration.
    fn apply_properties(&mut self) -> Result<()> {
        let get = |k: &str| -> Option<String> {
            let dashed = k.to_string();
            let camel = dashed_to_camel(k);
            self.properties.get(&dashed).or_else(|| self.properties.get(&camel)).cloned()
        };
        if let Some(v) = get("camel.main.name") {
            self.config.name = Some(v);
        }
        if let Some(v) = get("camel.main.routes-include-pattern") {
            self.config.routes_include_pattern.extend(v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()));
        }
        let num = |v: Option<String>, k: &str| -> Result<Option<u64>> {
            match v {
                None => Ok(None),
                Some(s) => s.trim().parse::<i64>().map(|n| if n > 0 { Some(n as u64) } else { None }).map_err(|_| CamelError::illegal_argument(format!("Invalid number for {}: {}", k, s))),
            }
        };
        if let Some(v) = num(get("camel.main.duration-max-seconds"), "duration-max-seconds")? {
            self.config.duration_max_seconds = Some(v);
        }
        if let Some(v) = num(get("camel.main.duration-max-messages"), "duration-max-messages")? {
            self.config.duration_max_messages = Some(v);
        }
        if let Some(v) = num(get("camel.main.duration-max-idle-seconds"), "duration-max-idle-seconds")? {
            self.config.duration_max_idle_seconds = Some(v);
        }
        if let Some(v) = get("camel.main.shutdown-timeout") {
            let secs: u64 = v.trim().parse().map_err(|_| CamelError::illegal_argument(format!("Invalid shutdown-timeout: {}", v)))?;
            self.config.shutdown_timeout = Some(Duration::from_secs(secs));
        }
        if let Some(v) = get("camel.main.log-level").or_else(|| get("logging.level.root")) {
            self.config.log_level = Some(v);
        }
        // all properties become placeholders
        self.ctx.properties().set_all(self.properties.clone());
        Ok(())
    }

    /// Resolve route include patterns to files.
    fn discover_route_files(&self) -> Result<Vec<PathBuf>> {
        let mut files = self.route_files.clone();
        for pattern in &self.config.routes_include_pattern {
            let pattern = pattern.strip_prefix("file:").unwrap_or(pattern);
            let pattern = pattern.strip_prefix("classpath:").unwrap_or(pattern);
            for f in glob_files(pattern)? {
                if !files.contains(&f) {
                    files.push(f);
                }
            }
        }
        Ok(files)
    }

    /// Load a single route file using the loader for its extension.
    pub fn load_route_file(&self, path: &Path) -> Result<()> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        // support `.camel.yaml` etc.
        let loader = self
            .loaders
            .get(&ext)
            .ok_or_else(|| CamelError::illegal_argument(format!("No route loader for file {} (supported: {:?})", path.display(), self.supported_extensions())))?;
        let text = std::fs::read_to_string(path).map_err(|e| CamelError::io(format!("Cannot read route file {}: {}", path.display(), e)))?;
        loader(&self.ctx, &text, path).map_err(|e| CamelError::new(e.kind.clone(), format!("Error loading routes from {}: {}", path.display(), e.message)).with_cause(e))
    }

    /// Initialize and start the context (does not block).
    pub fn start(&mut self) -> Result<()> {
        self.apply_properties()?;
        if let Some(l) = &self.config.log_level {
            if let Some(level) = camel_core::log::Level::parse(l) {
                camel_core::log::set_level(level);
            }
        }
        if let Some(t) = self.config.shutdown_timeout {
            self.ctx.set_shutdown_timeout(t);
        }
        for c in std::mem::take(&mut self.configurers) {
            c(&self.ctx)?;
        }
        for f in self.discover_route_files()? {
            camel_core::camel_info!("Main", "Loading routes from: {}", f.display());
            self.load_route_file(&f)?;
        }
        self.ctx.start()
    }

    /// Start and block until a stop condition happens, then stop the context.
    pub fn run(&mut self) -> Result<()> {
        let completed = Arc::new(AtomicU64::new(0));
        let last_activity = Arc::new(std::sync::Mutex::new(Instant::now()));
        self.ctx.add_event_notifier(CompletedCounter { completed: completed.clone(), last_activity: last_activity.clone() });
        signal::install();
        self.start()?;
        let started = Instant::now();
        let reason;
        loop {
            if signal::received() {
                reason = "signal received".to_string();
                break;
            }
            if let Some(s) = self.config.duration_max_seconds {
                if started.elapsed() >= Duration::from_secs(s) {
                    reason = format!("duration max seconds ({}) reached", s);
                    break;
                }
            }
            if let Some(m) = self.config.duration_max_messages {
                if completed.load(Ordering::SeqCst) >= m {
                    reason = format!("duration max messages ({}) reached", m);
                    break;
                }
            }
            if let Some(s) = self.config.duration_max_idle_seconds {
                if last_activity.lock().unwrap().elapsed() >= Duration::from_secs(s) {
                    reason = format!("duration max idle seconds ({}) reached", s);
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        camel_core::camel_info!("Main", "Stopping CamelContext: {}", reason);
        self.ctx.stop()
    }

    pub fn stop(&self) -> Result<()> {
        self.ctx.stop()
    }
}

/// `routes-include-pattern` → `routesIncludePattern` style alias.
fn dashed_to_camel(k: &str) -> String {
    let mut out = String::new();
    let mut upper = false;
    for c in k.chars() {
        if c == '-' {
            upper = true;
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// Expand a simple glob (`dir/*.yaml`, `dir/**/*.yaml`, or a plain path) into files.
pub fn glob_files(pattern: &str) -> Result<Vec<PathBuf>> {
    if !pattern.contains('*') && !pattern.contains('?') {
        let p = PathBuf::from(pattern);
        return Ok(if p.is_file() { vec![p] } else if p.is_dir() { list_dir(&p, false)? } else { vec![] });
    }
    // split at the first component containing a wildcard
    let normalized = pattern.replace('\\', "/");
    let parts: Vec<&str> = normalized.split('/').collect();
    let idx = parts.iter().position(|p| p.contains('*') || p.contains('?')).unwrap();
    let base = if idx == 0 { ".".to_string() } else { parts[..idx].join("/") };
    let base = if base.is_empty() { "/".to_string() } else { base };
    let rest = parts[idx..].join("/");
    let recursive = rest.contains("**");
    let mut out = Vec::new();
    let base_path = PathBuf::from(&base);
    if !base_path.is_dir() {
        return Ok(out);
    }
    for f in list_dir(&base_path, recursive)? {
        let rel = f.strip_prefix(&base_path).unwrap_or(&f).to_string_lossy().replace('\\', "/");
        if ant_match(&rest, &rel) {
            out.push(f);
        }
    }
    out.sort();
    Ok(out)
}

fn list_dir(dir: &Path, recursive: bool) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let p = entry?.path();
        if p.is_dir() {
            if recursive {
                out.extend(list_dir(&p, true)?);
            }
        } else {
            out.push(p);
        }
    }
    out.sort();
    Ok(out)
}

/// Ant-style path matching: `**` matches any number of directories, `*`/`?` within a segment.
pub fn ant_match(pattern: &str, path: &str) -> bool {
    let p: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    let s: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    fn rec(p: &[&str], s: &[&str]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some(&"**") => (0..=s.len()).any(|i| rec(&p[1..], &s[i..])),
            Some(seg) => !s.is_empty() && camel_core::util::wildcard_match(s[0], seg) && rec(&p[1..], &s[1..]),
        }
    }
    rec(&p, &s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ant() {
        assert!(ant_match("*.yaml", "a.yaml"));
        assert!(!ant_match("*.yaml", "x/a.yaml"));
        assert!(ant_match("**/*.yaml", "x/y/a.yaml"));
        assert!(ant_match("**/*.yaml", "a.yaml"));
        assert_eq!(dashed_to_camel("camel.main.duration-max-seconds"), "camel.main.durationMaxSeconds");
    }

    #[test]
    fn main_runs_with_max_messages() {
        let mut main = Main::new();
        main.set_property("camel.main.duration-max-messages", "2");
        main.set_property("greeting", "hi");
        main.configure(|ctx| {
            ctx.add_routes(|b| {
                b.from("direct:in").set_body(constant("x")).to("mock:out");
            })
        });
        let ctx = main.context().clone();
        let h = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            let t = ctx.create_producer_template();
            t.send_body("direct:in", 1).unwrap();
            t.send_body("direct:in", 2).unwrap();
        });
        main.run().unwrap();
        h.join().unwrap();
        assert_eq!(main.context().resolve_placeholders("{{greeting}}").unwrap(), "hi");
        assert_eq!(main.context().status(), ServiceStatus::Stopped);
    }
}

/// Create a [`Main`] with all available components and route loaders registered.
pub fn new_main() -> Main {
    let mut main = Main::new();
    camel_http::register(main.context());
    // REST DSL elements (`rest`, `restConfiguration`) in YAML/XML route files
    yaml_rest::register();
    for ext in ["yaml", "yml"] {
        main.add_route_loader(ext, |ctx, text, _path| camel_yaml::add_routes_from_yaml(ctx, text));
    }
    main.add_route_loader("xml", |ctx, text, _path| camel_yaml::add_routes_from_xml(ctx, text));
    main
}

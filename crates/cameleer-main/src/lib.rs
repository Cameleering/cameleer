//! # cameleer-main
//!
//! Standalone runner for Cameleer applications (Camel Main equivalent):
//! loads `application.properties`/`application.yaml` (with profiles, relaxed binding,
//! environment variables, `camel.springboot.*` aliases, `camel.component.*` options and
//! `camel.beans.*` beans, see [`config`]), configures the context, discovers route files
//! (YAML/XML DSL, Kamelets, Pipes and Camel K `Integration` resources, see
//! [`integration`]), starts the context and keeps running until stopped (signal, stop file,
//! max duration, max messages or idle timeout). With `--dev` (`camel.main.dev-reload`) route
//! files are reloaded when they change; [`local`] publishes the status files used by the
//! `cameleer ps/get/cmd/log/trace/stop` commands.

#[cfg(feature = "attest")]
pub mod attest;
pub mod cli;
pub mod testing;
pub mod config;
pub mod console;
pub mod features;
pub mod integration;
pub mod lambda;
pub mod local;
pub mod properties;
#[cfg(feature = "validate")]
pub mod schema;
pub mod signal;
pub mod tools;
#[cfg(feature = "validate")]
pub mod validate;
pub mod yaml_rest;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cameleer_core::context::CamelEvent;
use cameleer_core::prelude::*;
use cameleer_core::spi::EventNotifier;

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
    /// Reload route files when they change (`cameleer run --dev`).
    pub dev_reload: bool,
}

/// The main runner.
pub struct Main {
    ctx: CamelContext,
    config: MainConfig,
    properties: BTreeMap<String, String>,
    loaders: BTreeMap<String, RouteLoader>,
    route_files: Vec<PathBuf>,
    configurers: Vec<Box<dyn FnOnce(&CamelContext) -> Result<()>>>,
    console: Option<console::DevConsoleServer>,
    /// The MCP server's HTTP endpoint (`camel.server.mcp-enabled`), stopped when dropped.
    mcp: std::sync::Mutex<Option<Box<dyn std::any::Any + Send>>>,
    /// The attestation endpoint (`camel.attestation.enabled`), stopped when dropped.
    #[cfg_attr(not(feature = "attest"), allow(dead_code))]
    attestation: std::sync::Mutex<Option<Box<dyn std::any::Any + Send>>>,
    /// Properties set in code / on the command line (highest precedence).
    explicit: BTreeMap<String, String>,
    /// Defaults set in code / on the command line (lowest precedence).
    defaults: BTreeMap<String, String>,
    /// Per-profile YAML documents of the loaded configuration files.
    profile_docs: Vec<(String, BTreeMap<String, String>)>,
    /// Directories of the loaded configuration files (for `application-<profile>.*`).
    config_dirs: Vec<PathBuf>,
    profile: Option<String>,
    env: Option<BTreeMap<String, String>>,
    /// Route files loaded at startup with their modification time (dev reload).
    loaded_files: Vec<(PathBuf, Option<std::time::SystemTime>)>,
    local: Option<local::LocalStatus>,
    local_dir: Option<PathBuf>,
    /// In-memory route sources: (file name, content).
    route_sources: Vec<(String, String)>,
    /// Ids of the routes created from route files (replaced on reload).
    file_route_ids: Vec<String>,
    /// `host:port` of the AWS Lambda Runtime API (default: `AWS_LAMBDA_RUNTIME_API`).
    lambda_api: Option<String>,
}

struct CompletedCounter {
    completed: Arc<AtomicU64>,
    last_activity: Arc<std::sync::Mutex<Instant>>,
    /// `camel.main.duration-max-messages` (known once the context started): reaching it
    /// wakes the run loop at once.
    max_messages: Arc<std::sync::OnceLock<Option<u64>>>,
    wake: Arc<Wake>,
}

/// Wakes [`Main::run`]'s loop before its next periodic check.
#[derive(Default)]
struct Wake {
    flag: std::sync::Mutex<bool>,
    cv: std::sync::Condvar,
}

impl Wake {
    fn notify(&self) {
        *self.flag.lock().unwrap() = true;
        self.cv.notify_all();
    }

    /// Wait until notified or `timeout` passes.
    fn wait(&self, timeout: Duration) {
        let guard = self.flag.lock().unwrap();
        let (mut guard, _) = self.cv.wait_timeout_while(guard, timeout, |woken| !*woken).unwrap();
        *guard = false;
    }
}

impl EventNotifier for CompletedCounter {
    fn notify(&self, event: &CamelEvent) {
        match event {
            CamelEvent::ExchangeCompleted { .. } | CamelEvent::ExchangeFailed { .. } => {
                let n = self.completed.fetch_add(1, Ordering::SeqCst) + 1;
                *self.last_activity.lock().unwrap() = Instant::now();
                if self.max_messages.get().copied().flatten() == Some(n) {
                    self.wake.notify();
                }
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
            console: None,
            mcp: std::sync::Mutex::new(None),
            attestation: std::sync::Mutex::new(None),
            defaults: BTreeMap::new(),
            explicit: BTreeMap::new(),
            profile_docs: Vec::new(),
            config_dirs: Vec::new(),
            profile: None,
            env: None,
            loaded_files: Vec::new(),
            local: None,
            local_dir: None,
            route_sources: Vec::new(),
            file_route_ids: Vec::new(),
            lambda_api: lambda::runtime_api(),
        }
    }

    /// Serve AWS Lambda invocations from the Runtime API at `host:port` in [`Self::run`]
    /// (Lambda sets `AWS_LAMBDA_RUNTIME_API`, which is the default); `None` turns it off.
    pub fn set_lambda_runtime_api(&mut self, api: Option<String>) {
        self.lambda_api = api;
    }

    /// Whether [`Self::run`] serves AWS Lambda invocations (see [`lambda`]).
    pub fn is_lambda(&self) -> bool {
        self.lambda_api.is_some()
    }

    /// Publish status files in `dir` and accept commands from the `cameleer` CLI (see
    /// [`local`]).
    pub fn enable_local_status(&mut self, dir: impl Into<PathBuf>) {
        self.local_dir = Some(dir.into());
    }

    /// The local status publisher, when running with [`Self::enable_local_status`].
    pub fn local_status(&self) -> Option<&local::LocalStatus> {
        self.local.as_ref()
    }

    /// Replace the routes loaded from route files by reloading the files (dev mode).
    /// Errors are logged; the previous routes are removed either way.
    pub fn reload_route_files(&mut self) -> Result<()> {
        for id in std::mem::take(&mut self.file_route_ids) {
            let _ = self.ctx.stop_route(&id);
            let _ = self.ctx.remove_route(&id);
        }
        let before: Vec<String> = self.ctx.route_ids();
        let files: Vec<PathBuf> = self.loaded_files.iter().map(|(f, _)| f.clone()).collect();
        let mut result = Ok(());
        self.loaded_files.clear();
        for f in files {
            let mtime = std::fs::metadata(&f).and_then(|m| m.modified()).ok();
            if let Err(e) = self.load_route_file(&f) {
                cameleer_core::camel_error!("Main", "Failed to reload {}: {}", f.display(), e);
                result = Err(e);
            } else {
                cameleer_core::camel_info!("Main", "Reloaded routes from: {}", f.display());
            }
            self.loaded_files.push((f, mtime));
        }
        self.file_route_ids = self.ctx.route_ids().into_iter().filter(|id| !before.contains(id)).collect();
        result
    }

    /// Whether a loaded route file changed since it was (re)loaded.
    fn route_files_changed(&self) -> bool {
        self.loaded_files.iter().any(|(f, t)| std::fs::metadata(f).and_then(|m| m.modified()).ok() != *t)
    }

    /// Use an existing context.
    pub fn with_context(ctx: CamelContext) -> Self {
        Main { ctx, ..Self::new() }
    }

    /// Add a pack (for example `cameleer_health::HEALTH`): its components, data formats and
    /// languages are registered on the context.
    pub fn add_pack(&mut self, pack: &'static dyn cameleer_core::pack::Pack) -> &mut Self {
        self.context().add_pack(pack);
        self
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

    /// Add an in-memory route source (e.g. embedded with `include_str!` by `cameleer export`);
    /// the loader is chosen by the extension of `name`.
    pub fn add_route_source(&mut self, name: impl Into<String>, content: impl Into<String>) {
        self.route_sources.push((name.into(), content.into()));
    }

    /// Load configuration text (`yaml = true` for application.yaml content).
    pub fn load_properties_text(&mut self, text: &str, yaml: bool) -> Result<()> {
        if yaml {
            let (base, docs) = config::parse_yaml_config(text)?;
            self.properties.extend(base);
            self.profile_docs.extend(docs);
        } else {
            self.properties.extend(properties::parse_properties(text));
        }
        Ok(())
    }

    /// Add routes with the Rust DSL.
    pub fn configure(&mut self, f: impl FnOnce(&CamelContext) -> Result<()> + 'static) {
        self.configurers.push(Box::new(f));
    }

    /// Set a property (overrides configuration files and environment variables, like a
    /// command line argument in Spring Boot).
    pub fn set_property(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.explicit.insert(key.into(), value.into());
    }

    /// Set a property only if no file, profile, environment variable or explicit property
    /// sets it.
    pub fn set_default_property(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.defaults.insert(key.into(), value.into());
    }

    /// The effective properties (after `start`: files, profile, environment and explicit
    /// properties merged).
    pub fn properties(&self) -> &BTreeMap<String, String> {
        &self.properties
    }

    /// Activate a profile (`application-<profile>.*`, `%<profile>.` keys).
    pub fn set_profile(&mut self, profile: impl Into<String>) {
        self.profile = Some(profile.into());
    }

    /// Use these environment variables instead of the process environment.
    pub fn set_environment(&mut self, env: BTreeMap<String, String>) {
        self.env = Some(env);
    }

    /// Load a configuration file: `.properties`, or `.yaml`/`.yml` (flattened; documents
    /// for other profiles are kept for [`Self::set_profile`]).
    pub fn load_properties_file(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let (base, docs) = config::load_file(path)?;
        self.properties.extend(base);
        self.profile_docs.extend(docs);
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        if !self.config_dirs.contains(&dir) {
            self.config_dirs.push(dir);
        }
        Ok(())
    }

    /// Load `application.properties`, `application.yaml` and `application.yml` from `dir`
    /// (those that exist). Returns how many files were loaded.
    pub fn load_application_config(&mut self, dir: impl AsRef<Path>) -> Result<usize> {
        let files = config::application_files(dir.as_ref());
        for f in &files {
            self.load_properties_file(f)?;
        }
        if !self.config_dirs.contains(&dir.as_ref().to_path_buf()) {
            self.config_dirs.push(dir.as_ref().to_path_buf());
        }
        Ok(files.len())
    }

    /// Merge files, profile, environment variables, aliases and explicit properties.
    fn merge_configuration(&mut self) -> Result<()> {
        let env: BTreeMap<String, String> = self.env.clone().unwrap_or_else(|| std::env::vars().collect());
        let mut props = self.properties.clone();
        let mut probe = props.clone();
        probe.extend(self.explicit.clone());
        let profile = self.profile.clone().or_else(|| config::active_profile(&probe, &env));
        if let Some(p) = &profile {
            for name in p.split(',').map(str::trim) {
                for dir in self.config_dirs.clone() {
                    for ext in ["properties", "yaml", "yml"] {
                        let f = dir.join(format!("application-{}.{}", name, ext));
                        if f.is_file() {
                            let (base, docs) = config::load_file(&f)?;
                            props.extend(base);
                            self.profile_docs.extend(docs);
                        }
                    }
                }
            }
            cameleer_core::camel_info!("Main", "The following profiles are active: {}", p);
        }
        config::apply_profile(&mut props, &self.profile_docs, profile.as_deref());
        let schemes = self.ctx.component_names();
        for (k, v) in &env {
            if let Some(key) = config::env_to_property(k, &schemes) {
                props.insert(key, v.clone());
            }
        }
        props.extend(self.explicit.clone());
        for (k, v) in &self.defaults {
            if config::relaxed_get(&props, k).is_none() {
                props.insert(k.clone(), v.clone());
            }
        }
        config::apply_aliases(&mut props);
        if let Some(p) = profile {
            props.entry("camel.main.profile".into()).or_insert(p);
        }
        resolve_runtime_placeholders(&mut props);
        self.properties = props;
        Ok(())
    }

    /// Apply `camel.main.*` and other well-known properties to the configuration.
    fn apply_properties(&mut self) -> Result<()> {
        self.merge_configuration()?;
        let get = |k: &str| -> Option<String> { config::relaxed_get(&self.properties, k).cloned() };
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
        if let Some(v) = get("camel.main.dev-reload") {
            self.config.dev_reload = v.trim().eq_ignore_ascii_case("true");
        }
        // all properties become placeholders
        self.ctx.properties().set_all(self.properties.clone());
        config::create_beans(&self.ctx, &self.properties)?;
        Ok(())
    }

    /// Load the Kamelets of `camel.main.kamelet-locations` (alias
    /// `camel.component.kamelet.location`; comma separated directories, `file:` and
    /// `classpath:` prefixes accepted — the latter relative to the working directory; default
    /// `classpath:kamelets`). Remote (`github:`) locations are skipped with a warning.
    fn load_kamelet_locations(&self) -> Result<()> {
        let explicit = config::relaxed_get(&self.properties, "camel.main.kamelet-locations")
            .or_else(|| config::relaxed_get(&self.properties, "camel.component.kamelet.location"))
            .cloned();
        let locations = explicit.clone().unwrap_or_else(|| "classpath:kamelets".into());
        for loc in locations.split(',').map(str::trim).filter(|l| !l.is_empty()) {
            if loc.starts_with("github:") || loc.starts_with("http:") || loc.starts_with("https:") {
                cameleer_core::camel_warn!("Main", "Remote Kamelet location {} is not supported (no network access); skipped", loc);
                continue;
            }
            let dir = loc.strip_prefix("file:").or_else(|| loc.strip_prefix("classpath:")).unwrap_or(loc);
            let dir = Path::new(dir);
            if dir.is_dir() {
                let names = cameleer_yaml::load_kamelets_from_dir(&self.ctx, dir)?;
                cameleer_core::camel_info!("Main", "Loaded {} Kamelets from {}: {}", names.len(), dir.display(), names.join(", "));
            } else if explicit.is_some() {
                cameleer_core::camel_warn!("Main", "Kamelet location {} does not exist", dir.display());
            }
        }
        Ok(())
    }

    /// Camel K Integration files among the explicit route files contribute properties
    /// before the configuration is applied.
    fn prescan_integrations(&mut self) -> Result<()> {
        for f in self.route_files.clone() {
            let is_yaml = matches!(f.extension().and_then(|e| e.to_str()), Some("yaml" | "yml"));
            if !is_yaml {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&f) {
                if let Some(it) = integration::parse_integration(&text)? {
                    for (k, v) in it.properties {
                        self.properties.entry(k).or_insert(v);
                    }
                }
            }
        }
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
        cameleer_core::startup::step("main", "configure", || -> Result<()> {
            self.prescan_integrations()?;
            self.apply_properties()
        })?;
        if let Some(l) = &self.config.log_level {
            if let Some(level) = cameleer_core::log::Level::parse(l) {
                cameleer_core::log::set_level(level);
            }
        }
        if let Some(t) = self.config.shutdown_timeout {
            self.ctx.set_shutdown_timeout(t);
        }
        self.apply_management_properties()?;
        for c in std::mem::take(&mut self.configurers) {
            c(&self.ctx)?;
        }
        self.load_kamelet_locations()?;
        self.loaded_files.clear();
        let before: Vec<String> = self.ctx.assign_pending_route_ids();
        for f in self.discover_route_files()? {
            cameleer_core::camel_info!("Main", "Loading routes from: {}", f.display());
            cameleer_core::startup::step("routes-loader", f.display().to_string(), || self.load_route_file(&f))?;
            let mtime = std::fs::metadata(&f).and_then(|m| m.modified()).ok();
            self.loaded_files.push((f, mtime));
        }
        self.file_route_ids = self.ctx.assign_pending_route_ids().into_iter().filter(|id| !before.contains(id)).collect();
        for (name, content) in self.route_sources.clone() {
            let ext = Path::new(&name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            let loader = self.loaders.get(&ext).cloned().ok_or_else(|| CamelError::illegal_argument(format!("No route loader for {}", name)))?;
            cameleer_core::camel_info!("Main", "Loading routes from: {} (embedded)", name);
            loader(&self.ctx, &content, Path::new(&name))?;
        }
        cameleer_core::startup::step("context", "start", || self.ctx.start())?;
        if let Some(dir) = &self.local_dir {
            let name = self.config.name.clone().unwrap_or_else(|| self.ctx.name().to_string());
            // best-effort: a read-only filesystem or a home without write access must not
            // stop the integration, only `cameleer ps/stop/log` lose sight of it
            match local::LocalStatus::start(&self.ctx, &name, dir) {
                Ok(l) => self.local = Some(l),
                Err(e) => cameleer_core::camel_warn!("Main", "Local status files disabled: {}", e),
            }
        }
        for line in console::startup_summary(&self.ctx) {
            cameleer_core::camel_info!("Main", "{}", line);
        }
        if self.startup_recorder_logging() {
            cameleer_core::camel_info!("Main", "Startup steps (offset from process start, duration):");
            for line in cameleer_core::startup::report_lines() {
                cameleer_core::camel_info!("Main", "{}", line);
            }
        }
        let cfg = console::ConsoleConfig::from_properties(&self.properties)?;
        if cfg.is_active() && self.console.is_none() {
            match console::DevConsoleServer::start(&self.ctx, &cfg) {
                Ok(c) => self.console = Some(c),
                Err(e) => {
                    let _ = self.ctx.stop();
                    return Err(e);
                }
            }
        }
        if let Err(e) = self.start_mcp() {
            let _ = self.ctx.stop();
            return Err(e);
        }
        Ok(())
    }

    /// The MCP server (`camel.server.mcp-enabled=true`): over HTTP on `camel.server.port`, or
    /// over stdio (`camel.server.mcp-transport=stdio`), where the process stops when stdin
    /// ends.
    #[cfg(feature = "ai")]
    fn start_mcp(&self) -> Result<()> {
        use cameleer_ai::mcp::{http::McpHttpServer, McpConfig, McpServer};
        if self.mcp.lock().unwrap().is_some() {
            return Ok(());
        }
        // MCP clients (camel.ai.mcp.NAME.*): their tools join the registry; a server that is
        // down is retried in the background
        for client in cameleer_ai::mcp::client::McpClientConfig::from_properties(&self.properties)? {
            let clients = cameleer_ai::mcp::client::McpClients::of(&self.ctx);
            let name = client.name.clone();
            match clients.add(&self.ctx, client.clone()) {
                Ok(n) => cameleer_core::camel_info!("mcp-client", "Imported {} tools from MCP server {}", n, name),
                Err(e) => {
                    cameleer_core::camel_warn!("mcp-client", "MCP server {} is not reachable ({}); retrying every 30s", name, e.message);
                    let weak = self.ctx.downgrade();
                    let _ = std::thread::Builder::new().name(format!("mcp-connect-{}", name)).spawn(move || loop {
                        std::thread::sleep(Duration::from_secs(30));
                        let Some(ctx) = weak.upgrade() else { return };
                        if let Ok(n) = cameleer_ai::mcp::client::McpClients::of(&ctx).add(&ctx, client.clone()) {
                            cameleer_core::camel_info!("mcp-client", "Imported {} tools from MCP server {}", n, name);
                            return;
                        }
                    });
                }
            }
        }
        // existing routes as tools, with no route edits (camel.server.mcp-expose-routes,
        // `cameleer run --mcp-expose`); they carry the tag `exposed`
        let patterns: Vec<String> = console::prop(&self.properties, cameleer_ai::expose::PROPERTY)
            .map(|v| v.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect())
            .unwrap_or_default();
        let exposed = cameleer_ai::expose::expose_routes(&self.ctx, &patterns, &self.properties)?;
        if !patterns.is_empty() {
            if exposed.is_empty() {
                cameleer_core::camel_warn!("mcp", "{}={} matches no route", cameleer_ai::expose::PROPERTY, patterns.join(","));
            } else {
                cameleer_core::camel_info!("mcp", "Exposed {} routes as tools: {}", exposed.len(), exposed.join(", "));
            }
        }
        // attestation sees the routes and tools as served, the exposed ones included, and is
        // in the registry before the MCP server starts (its handshake carries it)
        self.start_attestation()?;
        let mut cfg = McpConfig::from_properties(&self.properties)?;
        if !cfg.enabled {
            return Ok(());
        }
        // asking for exposed routes means serving them, whatever the tag filter says otherwise
        if !exposed.is_empty() && !cfg.tags.iter().any(|t| t == "*" || t == cameleer_ai::expose::EXPOSED_TAG) {
            cfg.tags.push(cameleer_ai::expose::EXPOSED_TAG.to_string());
        }
        let server = McpServer::new(&self.ctx, cfg.clone())?;
        if cfg.transport == "stdio" {
            cameleer_core::camel_info!("mcp", "MCP server on stdio (protocol {} and {})", cameleer_ai::mcp::MODERN_VERSION, cameleer_ai::mcp::LEGACY_VERSIONS.join(", "));
            std::thread::Builder::new()
                .name("mcp-stdio".into())
                .spawn(move || {
                    let stdin = std::io::stdin();
                    cameleer_ai::mcp::stdio::serve(server, stdin.lock(), std::io::stdout());
                    cameleer_core::camel_info!("mcp", "stdin closed");
                    signal::trigger();
                })
                .map_err(|e| CamelError::io(format!("Cannot start the MCP stdio server: {}", e)))?;
            *self.mcp.lock().unwrap() = Some(Box::new(()));
        } else {
            let http = McpHttpServer::start(server)?;
            *self.mcp.lock().unwrap() = Some(Box::new(http));
        }
        Ok(())
    }

    /// Attestation in a hardware TEE (`camel.attestation.enabled=true`, the `attest` feature).
    #[cfg(feature = "attest")]
    fn start_attestation(&self) -> Result<()> {
        if self.attestation.lock().unwrap().is_some() {
            return Ok(());
        }
        let files: Vec<PathBuf> = self.loaded_files.iter().map(|(f, _)| f.clone()).collect();
        if let Some(h) = attest::start(&self.ctx, &self.properties, &files)? {
            *self.attestation.lock().unwrap() = Some(Box::new(h));
        }
        Ok(())
    }

    #[cfg(not(feature = "attest"))]
    fn start_attestation(&self) -> Result<()> {
        if console::prop(&self.properties, "camel.attestation.enabled").map(|v| v.trim().eq_ignore_ascii_case("true")).unwrap_or(false) {
            return Err(CamelError::illegal_argument("camel.attestation.enabled=true needs the attest feature: rebuild with --features attest"));
        }
        Ok(())
    }

    #[cfg(not(feature = "ai"))]
    fn start_mcp(&self) -> Result<()> {
        self.start_attestation()?;
        let on = console::prop(&self.properties, "camel.server.mcp-enabled").map(|v| v.trim().eq_ignore_ascii_case("true")).unwrap_or(false);
        if on {
            return Err(CamelError::illegal_argument("camel.server.mcp-enabled=true needs the ai feature: rebuild with --features ai"));
        }
        Ok(())
    }

    /// Whether `camel.main.startupRecorder` (`startup-recorder`) is `logging`.
    fn startup_recorder_logging(&self) -> bool {
        config::relaxed_get(&self.properties, "camel.main.startup-recorder").map(|v| v.trim().eq_ignore_ascii_case("logging")).unwrap_or(false)
    }

    /// Tracing, message history and route controller options
    /// (`camel.main.tracing`, `camel.main.backlog-tracing`, `camel.main.message-history`,
    /// `camel.main.jmxManagementStatisticsLevel`,
    /// `camel.main.route-controller-supervise-enabled` / `camel.routecontroller.enabled`,
    /// `...-initial-delay`, `...-back-off-delay`, `...-back-off-max-attempts`).
    fn apply_management_properties(&mut self) -> Result<()> {
        let flag = |k: &str| console::prop(&self.properties, k).map(|v| v.trim().eq_ignore_ascii_case("true"));
        if let Some(t) = flag("camel.main.tracing") {
            self.ctx.set_tracing(t);
        }
        if let Some(t) = flag("camel.main.backlog-tracing") {
            self.ctx.set_backlog_tracing(t);
        }
        if let Some(t) = flag("camel.main.message-history") {
            self.ctx.set_message_history(t);
        }
        // Camel's name; the kebab-case spelling is accepted too
        let level = console::prop(&self.properties, "camel.main.jmxManagementStatisticsLevel")
            .or_else(|| console::prop(&self.properties, "camel.main.jmx-management-statistics-level"));
        if let Some(l) = level {
            self.ctx.set_statistics_level(cameleer_core::management::ManagementStatisticsLevel::parse(&l)?);
        }
        if flag("camel.main.route-controller-supervise-enabled").or_else(|| flag("camel.routecontroller.enabled")).unwrap_or(false) {
            let dur = |k: &str| -> Result<Option<Duration>> {
                let v = console::prop(&self.properties, &format!("camel.main.route-controller-{}", k))
                    .or_else(|| console::prop(&self.properties, &format!("camel.routecontroller.{}", k)));
                v.map(|v| cameleer_core::util::parse_duration(v.trim())).transpose()
            };
            let mut c = cameleer_core::supervising::SupervisingRouteControllerConfig::default();
            if let Some(d) = dur("initial-delay")? {
                c.initial_delay = d;
            }
            if let Some(d) = dur("back-off-delay")? {
                c.back_off_delay = d;
            }
            let attempts = console::prop(&self.properties, "camel.main.route-controller-back-off-max-attempts")
                .or_else(|| console::prop(&self.properties, "camel.routecontroller.back-off-max-attempts"));
            if let Some(a) = attempts {
                c.back_off_max_attempts = a.trim().parse().map_err(|_| CamelError::illegal_argument(format!("Invalid back-off-max-attempts: {}", a)))?;
            }
            self.ctx.enable_supervising_route_controller(c);
        }
        Ok(())
    }

    /// Port of the management server (`/q/health`, `/q/metrics`, `/q/dev`), if running.
    pub fn console_port(&self) -> Option<u16> {
        self.console.as_ref().map(|c| c.port())
    }

    fn stop_console(&self) {
        if let Some(c) = &self.console {
            c.stop();
        }
        self.mcp.lock().unwrap().take();
        #[cfg(feature = "ai")]
        cameleer_ai::mcp::client::McpClients::of(&self.ctx).close_all(&self.ctx);
    }

    /// Start and block until a stop condition happens, then stop the context.
    pub fn run(&mut self) -> Result<()> {
        let completed = Arc::new(AtomicU64::new(0));
        let last_activity = Arc::new(std::sync::Mutex::new(Instant::now()));
        let wake = Arc::new(Wake::default());
        // the counter reads the limit when the first exchange completes (after start)
        let max_messages = Arc::new(std::sync::OnceLock::new());
        self.ctx.add_event_notifier(CompletedCounter {
            completed: completed.clone(),
            last_activity: last_activity.clone(),
            max_messages: max_messages.clone(),
            wake: wake.clone(),
        });
        signal::install();
        if let Some(api) = self.lambda_api.clone() {
            return self.run_lambda(&api);
        }
        self.start()?;
        let _ = max_messages.set(self.config.duration_max_messages);
        let started = Instant::now();
        let reason;
        let mut last_check = Instant::now();
        loop {
            if signal::received() {
                reason = "signal received".to_string();
                break;
            }
            if self.local.as_ref().map(|l| l.stop_requested()).unwrap_or(false) {
                reason = "stop requested".to_string();
                break;
            }
            let reload_requested = self.local.as_ref().map(|l| l.take_reload_request()).unwrap_or(false);
            if reload_requested || (self.config.dev_reload && last_check.elapsed() >= Duration::from_millis(300)) {
                last_check = Instant::now();
                if reload_requested || self.route_files_changed() {
                    let _ = self.reload_route_files();
                }
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
            // signals and file changes are polled; reaching max messages wakes at once
            wake.wait(Duration::from_millis(20));
        }
        cameleer_core::camel_info!("Main", "Stopping CamelContext: {}", reason);
        self.stop_console();
        if let Some(l) = self.local.take() {
            l.stop();
        }
        self.ctx.stop()
    }

    /// [`Self::run`] as an AWS Lambda custom runtime: start, then serve invocations until a
    /// signal (Lambda sends SIGTERM at shutdown) or `camel.main.duration-max-messages`
    /// invocations.
    fn run_lambda(&mut self, api: &str) -> Result<()> {
        let started = Instant::now();
        // HTTP events arrive through the Runtime API: platform-http routes open no sockets
        cameleer_http::registry::registry().set_in_process_only(true);
        if let Err(e) = self.start() {
            let runtime = lambda::LambdaRuntime::new(api, lambda::DEFAULT_HANDLER);
            if let Err(re) = runtime.init_error(&e) {
                cameleer_core::camel_warn!("Lambda", "Cannot report the init error: {}", re);
            }
            return Err(e);
        }
        let property = config::relaxed_get(&self.properties, "camel.main.lambda-handler").cloned();
        let handler = lambda::handler_endpoint(property.as_deref(), std::env::var("_HANDLER").ok().as_deref());
        let runtime = lambda::LambdaRuntime::new(api, handler);
        cameleer_core::camel_info!(
            "Lambda",
            "Serving AWS Lambda invocations from {} (init {}; HTTP events to platform-http routes, others to {})",
            api,
            cameleer_core::startup::format_duration(started.elapsed()),
            runtime.handler()
        );
        let result = runtime.run(&self.ctx, self.config.duration_max_messages, &signal::received);
        let reason = if signal::received() { "signal received" } else { "duration max messages reached" };
        cameleer_core::camel_info!("Main", "Stopping CamelContext: {}", reason);
        self.stop_console();
        if let Some(l) = self.local.take() {
            l.stop();
        }
        let stopped = self.ctx.stop();
        result?;
        stopped
    }

    pub fn stop(&self) -> Result<()> {
        self.stop_console();
        if let Some(l) = &self.local {
            l.stop();
        }
        self.ctx.stop()
    }
}

/// Resolve `{{...}}` placeholders in the values of the `camel.main.*` and `camel.server.*`
/// settings, as Camel does: `camel.server.port={{env:PORT:8080}}` (Cloud Run, Knative and
/// others pass the port in `PORT`). A value that cannot be resolved is kept as it is.
fn resolve_runtime_placeholders(props: &mut BTreeMap<String, String>) {
    let resolver = cameleer_core::context::PropertiesComponent::default();
    resolver.set_all(props.clone());
    for (k, v) in props.iter_mut() {
        if v.contains("{{") && (k.starts_with("camel.main.") || k.starts_with("camel.server.")) {
            if let Ok(r) = resolver.resolve(v) {
                *v = r;
            }
        }
    }
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
            Some(seg) => !s.is_empty() && cameleer_core::util::wildcard_match(s[0], seg) && rec(&p[1..], &s[1..]),
        }
    }
    rec(&p, &s)
}

/// Create a [`Main`] with all available components and route loaders registered.
pub fn new_main() -> Main {
    cameleer_core::startup::step("main", "register components", new_main_inner)
}

fn new_main_inner() -> Main {
    let mut main = Main::new();
    // families left out of this build explain themselves in "No component found" errors
    cameleer_core::component::set_unavailable_hint(features::unavailable_hint);
    #[cfg(feature = "xml")]
    cameleer_xml::register(main.context()); // xpath 1.0, xslt, validator (XSD), xj (before others: overrides core's xpath/validator)
    cameleer_http::register(main.context());
    cameleer_yaml::register_kamelet_component(main.context());
    #[cfg(feature = "ssh")]
    cameleer_ssh::register(main.context()); // sftp, ssh (before cameleer_protocols, whose sftp is a placeholder)
    // ftp, smtp/pop3/imap, websocket, mllp, syslog
    #[cfg(feature = "protocols")]
    cameleer_protocols::register(main.context());
    #[cfg(feature = "kafka-mqtt")]
    cameleer_kafka_mqtt::register(main.context()); // kafka, paho, paho-mqtt5
    // sql, jdbc, sql-stored + `camel.datasource[.<name>].url` data sources
    #[cfg(feature = "sql")]
    {
        cameleer_sql::register(main.context());
        main.configure(cameleer_sql::configure_datasources_from_properties);
    }
    #[cfg(feature = "ai")]
    cameleer_ai::register(main.context()); // ai-tool, ai-resource, ai-prompt
    #[cfg(feature = "amqp")]
    cameleer_amqp::register(main.context());
    #[cfg(feature = "amqp10")]
    cameleer_amqp10::register(main.context()); // amqp (AMQP 1.0, replaces cameleer-amqp's STOMP flavour, now `amqp-stomp`), azure-servicebus, azure-eventhubs
    #[cfg(feature = "grpc")]
    cameleer_grpc::register(main.context()); // grpc component + protobuf data format
    #[cfg(feature = "cloud")]
    cameleer_cloud::register(main.context()); // aws2-s3/sqs/sns/ddb/lambda, azure-storage-blob/queue, google-storage/pubsub
    #[cfg(feature = "datastores")]
    cameleer_datastores::register(main.context()); // spring-redis, mongodb, elasticsearch/opensearch, cql, couchdb
    #[cfg(feature = "script")]
    cameleer_script::register(main.context()); // js, groovy, python, ognl, mvel, spel, datasonnet, csimple, joor
    #[cfg(feature = "saas")]
    cameleer_saas::register(main.context()); // slack, telegram, github, twilio, jira, openai, webhook, graphql, rss, atom
    #[cfg(feature = "soap")]
    cameleer_soap::register(main.context()); // cxf, cxfrs, spring-ws, fhir + jaxb, fhirJson, fhirXml data formats
    #[cfg(feature = "dataformats")]
    cameleer_dataformats::register(main.context()); // avro, bindy, yaml, tarFile, zipFile, crypto, pgp, cbor, thrift, asn1, iCal, mimeMultipart, univocity
    // packs (not in `full`): one line each to install them; then every installed pack is added
    #[cfg(feature = "health")]
    cameleer_core::pack::install(&cameleer_health::HEALTH);
    for p in cameleer_core::pack::installed_packs() {
        main.add_pack(p);
    }
    // REST DSL elements (`rest`, `restConfiguration`) in YAML/XML route files
    yaml_rest::register();
    for ext in ["yaml", "yml"] {
        main.add_route_loader(ext, |ctx, text, _path| match integration::parse_integration(text)? {
            // Camel K Integration resource: its properties become placeholders (camel.main.*
            // options of explicit route files were applied before startup)
            Some(it) => {
                for (k, v) in &it.properties {
                    if ctx.properties().get(k).is_none() {
                        ctx.properties().set(k.clone(), v.clone());
                    }
                }
                integration::load_integration(ctx, &it)
            }
            None => cameleer_yaml::add_routes_from_yaml(ctx, text),
        });
    }
    main.add_route_loader("xml", |ctx, text, _path| cameleer_yaml::add_routes_from_xml(ctx, text));
    main
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
    }

    #[test]
    #[cfg(feature = "full")]
    fn new_main_binds_datasources_from_properties() {
        let mut main = new_main();
        main.set_property("camel.datasource.url", "jdbc:rustsql:mem:camelMainDsTest");
        main.configure(|ctx| {
            ctx.add_routes(|b| {
                b.from("direct:q").to("sql:select 6 * 7 as answer?dataSource=#dataSource&outputType=SelectOne");
            })
        });
        main.start().unwrap();
        let v = main.context().create_producer_template().request_body("direct:q", 0).unwrap();
        assert_eq!(v, cameleer_core::value::Value::Int(42));
        main.stop().unwrap();
    }

    #[test]
    #[cfg(feature = "full")]
    fn new_main_registers_xml_and_saas() {
        let main = new_main();
        let ctx = main.context();
        for scheme in ["xslt", "xslt-saxon", "validator", "xj", "slack", "telegram", "github", "twilio", "jira", "openai", "webhook", "graphql", "rss", "atom"] {
            assert!(ctx.has_component(scheme), "{}", scheme);
        }
        // the full XPath 1.0 engine replaces core's subset (following axis)
        let e = ctx.resolve_language("xpath").unwrap().create_expression("count(/a/b[1]/following::*)", &Default::default()).unwrap();
        let ex = cameleer_core::message::Exchange::with_body(Some(ctx.clone()), "<a><b/><b/><c/></a>");
        assert_eq!(e.evaluate(&ex).unwrap(), cameleer_core::value::Value::Float(2.0));
    }

    #[test]
    #[cfg(feature = "full")]
    fn new_main_registers_dataformats() {
        let main = new_main();
        for name in ["avro", "bindyCsv", "yaml", "tarFile", "pgp", "crypto", "cbor", "iCal", "mimeMultipart"] {
            assert!(cameleer_core::dataformat::lookup_data_format_factory(main.context().registry(), name).is_some(), "{}", name);
        }
        let df = cameleer_core::dataformat::DataFormatDefinition::new("cbor").resolve(main.context()).unwrap();
        let mut ex = cameleer_core::message::Exchange::with_body(Some(main.context().clone()), cameleer_core::value::Value::Int(500));
        df.marshal(&mut ex).unwrap();
        assert_eq!(ex.message.body, cameleer_core::value::Value::Bytes(vec![0x19, 0x01, 0xf4]));
    }

    #[test]
    fn reaching_max_messages_stops_without_waiting_for_the_next_poll() {
        // a run-once job (FaaS, batch): the process must not linger after its last message
        let mut main = Main::new();
        main.set_property("camel.main.duration-max-messages", "1");
        main.configure(|ctx| {
            ctx.add_routes(|b| {
                b.from("timer:once?repeatCount=1&delay=0").set_body(constant("x")).to("mock:out");
            })
        });
        let t0 = Instant::now();
        main.run().unwrap();
        let wake = Wake::default();
        // the wake itself is immediate once notified
        wake.notify();
        let w0 = Instant::now();
        wake.wait(Duration::from_secs(5));
        assert!(w0.elapsed() < Duration::from_secs(1));
        assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", t0.elapsed());
        assert_eq!(main.context().status(), ServiceStatus::Stopped);
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

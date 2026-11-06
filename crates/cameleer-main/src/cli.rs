//! The `cameleer` command line (a Rust counterpart of Camel JBang).
//!
//! ```text
//! cameleer init <file> [--directory DIR]           create a route (.yaml, .xml), Kamelet
//!                                               (.kamelet.yaml), Pipe (.pipe.yaml) or Camel K
//!                                               Integration (.integration.yaml) from a template
//! cameleer run [files...] [--properties FILE] [--profile P] [--dev] [--name N] [--background]
//!           [--max-seconds N] [--max-messages N] [--max-idle-seconds N] [--prop k=v]
//!           [--log-level L] [--console] [--port N] [--trace]
//! cameleer export [files...] --directory DIR [--name N] [--camel-path PATH]
//!                                               standalone Cargo project embedding the routes
//! cameleer bind <name> --source S --sink S [--step S]... [--property source.k=v]...
//!           [--error-handler log|none|sink:<uri or kamelet>] [--output FILE]
//!                                               Pipe resource binding Kamelets / endpoints
//! cameleer ps                                      running integrations
//! cameleer stop [name|pid] [--all]
//! cameleer get [routes|context|health|endpoints] [name] [--url URL]
//! cameleer cmd start-route|stop-route|suspend-route|resume-route|reset-stats|reload
//!           [name] [--id ROUTE]
//! cameleer log [name] [--follow] [--tail N]
//! cameleer trace [name] [--disable]
//! cameleer tool list|approvals [name|pid] [--json]
//! cameleer tool disable|enable <tool> [name|pid]
//! cameleer tool approve <handle> [--by NAME] [name|pid]
//! cameleer tool deny <handle> [--by NAME] [--reason TEXT] [name|pid]
//!                                               AI tools of a running integration: kill switch
//!                                               and approvals (needs the `ai` feature)
//! cameleer catalog [components|languages|kamelets]
//! cameleer validate <file-or-dir>... [--format text|json] [--strict] [--properties FILE] [--prop k=v]
//!                                               check route files without running them
//!                                               (exit 0 valid, 1 errors, 2 usage error)
//! cameleer schema                                  JSON Schema of the YAML DSL
//! cameleer version | help
//! ```
//!
//! Running integrations publish status files in `$CAMEL_HOME` (default `~/.camel`), see
//! [`crate::local`]; `get`/`cmd`/`log`/`trace`/`ps`/`stop`/`tool` use them (`get --url` queries a
//! management server instead).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use cameleer_core::value::Value;

use crate::local;

type CliResult = Result<(), String>;

pub fn usage() -> &'static str {
    "Usage:
  cameleer init <file> [--directory DIR]
  cameleer run [files...] [--properties FILE] [--profile P] [--dev] [--name N] [--background]
            [--max-seconds N] [--max-messages N] [--max-idle-seconds N] [--prop key=value]
            [--log-level LEVEL] [--console] [--port N] [--trace] [--mcp | --mcp-stdio]
  cameleer export [files...] --directory DIR [--name N] [--camel-path PATH]
  cameleer bind <name> --source S --sink S [--step S]... [--property source.k=v]...
             [--error-handler log|none|sink:URI] [--output FILE]
  cameleer ps
  cameleer stop [name|pid] [--all]
  cameleer get [routes|context|health|endpoints] [name] [--url http://localhost:8080]
  cameleer cmd start-route|stop-route|suspend-route|resume-route|reset-stats|reload [name] [--id ROUTE]
  cameleer log [name] [--follow] [--tail N]
  cameleer trace [name] [--disable]
  cameleer tool list|approvals [name|pid] [--json]
  cameleer tool disable|enable <tool> [name|pid]
  cameleer tool approve <handle> [--by NAME] [name|pid]
  cameleer tool deny <handle> [--by NAME] [--reason TEXT] [name|pid]
  cameleer catalog [components|languages|kamelets]
  cameleer test <test-file>...
  cameleer validate <file-or-dir>... [--format text|json] [--strict] [--properties FILE] [--prop key=value]
  cameleer schema
  cameleer version
  cameleer help"
}

/// Whether the executable was started as `bootstrap` (an AWS Lambda custom runtime).
pub fn invoked_as_bootstrap(argv0: Option<&str>) -> bool {
    argv0.map(Path::new).and_then(|p| p.file_stem()).and_then(|s| s.to_str()) == Some("bootstrap")
}

/// Run a command, writing its output to `out`.
pub fn run(args: &[String], out: &mut dyn Write) -> CliResult {
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("help");
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };
    match cmd {
        "version" | "--version" | "-v" => {
            if crate::features::is_full() {
                w(out, &format!("Cameleer {}", env!("CARGO_PKG_VERSION")))
            } else {
                // a reduced build says which component families it has
                w(out, &format!("Cameleer {} (components: core, http, yaml{}{})", env!("CARGO_PKG_VERSION"), if crate::features::enabled().is_empty() { "" } else { ", " }, crate::features::enabled().join(", ")))
            }
        }
        "help" | "--help" | "-h" => w(out, usage()),
        "catalog" => catalog(rest, out),
        "init" => init(rest, out),
        "run" => run_cmd(rest),
        "export" => export(rest, out),
        "bind" => bind(rest, out),
        "ps" => ps(out),
        "stop" => stop(rest, out),
        "get" => get(rest, out),
        "cmd" => cmd_action(rest, out),
        "log" => log(rest, out),
        "trace" => trace(rest, out),
        "tool" => tool_cmd(rest, out),
        "validate" => match validate_cmd(rest, out)? {
            0 => Ok(()),
            _ => Err("validation failed".into()),
        },
        "schema" => schema_cmd(rest, out),
        "test" => test_cmd(rest, out),
        other => Err(format!("Unknown command: {}\n{}", other, usage())),
    }
}

/// Run a command and return the process exit code, writing errors to `err`: 0 success, 1
/// failure (for `validate`: errors found, or warnings with `--strict`), 2 usage error of
/// `validate` (unknown option, missing or unknown path, bad `--prop`).
pub fn run_status(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    if args.first().map(|s| s.as_str()) == Some("validate") {
        return match validate_cmd(&args[1..], out) {
            Ok(code) => code,
            Err(e) => {
                let _ = writeln!(err, "Error: {}", e);
                2
            }
        };
    }
    match run(args, out) {
        Ok(()) => 0,
        Err(e) => {
            let _ = writeln!(err, "Error: {}", e);
            1
        }
    }
}

/// `cameleer test`: run YAML test files (see [`crate::testing`]); fails when a test fails.
fn test_cmd(args: &[String], out: &mut dyn Write) -> CliResult {
    let a = Args::parse(args, &[])?;
    a.check(&[])?;
    if a.positional.is_empty() {
        return Err(format!("cameleer test requires at least one test file
{}", usage()));
    }
    let files: Vec<PathBuf> = a.positional.iter().map(PathBuf::from).collect();
    let (_, failed) = crate::testing::run_files(&files, out)?;
    if failed > 0 {
        return Err(format!("{} test(s) failed", failed));
    }
    Ok(())
}

fn w(out: &mut dyn Write, s: &str) -> CliResult {
    writeln!(out, "{}", s).map_err(|e| e.to_string())
}

/// Split arguments into positionals and `--options` (flags without value are listed in `flags`).
struct Args {
    positional: Vec<String>,
    options: Vec<(String, String)>,
}

impl Args {
    fn parse(args: &[String], flags: &[&str]) -> Result<Args, String> {
        let mut a = Args { positional: Vec::new(), options: Vec::new() };
        let mut i = 0;
        while i < args.len() {
            let s = &args[i];
            if let Some(name) = s.strip_prefix("--") {
                if let Some((k, v)) = name.split_once('=') {
                    a.options.push((k.to_string(), v.to_string()));
                } else if flags.contains(&name) {
                    a.options.push((name.to_string(), "true".into()));
                } else {
                    i += 1;
                    let v = args.get(i).ok_or_else(|| format!("Missing value for --{}", name))?;
                    a.options.push((name.to_string(), v.clone()));
                }
            } else {
                a.positional.push(s.clone());
            }
            i += 1;
        }
        Ok(a)
    }
    fn get(&self, k: &str) -> Option<&str> {
        self.options.iter().rev().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }
    fn all(&self, k: &str) -> Vec<&str> {
        self.options.iter().filter(|(n, _)| n == k).map(|(_, v)| v.as_str()).collect()
    }
    fn flag(&self, k: &str) -> bool {
        self.get(k).map(|v| v == "true").unwrap_or(false)
    }
    fn check(&self, allowed: &[&str]) -> CliResult {
        for (k, _) in &self.options {
            if !allowed.contains(&k.as_str()) {
                return Err(format!("Unknown option --{}\n{}", k, usage()));
            }
        }
        Ok(())
    }
}

fn catalog(args: &[String], out: &mut dyn Write) -> CliResult {
    let main = crate::new_main();
    let ctx = main.context();
    let items = match args.first().map(|s| s.as_str()).unwrap_or("components") {
        "components" => ctx.component_names(),
        "languages" => ctx.language_names(),
        "kamelets" => cameleer_yaml::kamelet_registry(ctx).names(),
        other => return Err(format!("Unknown catalog kind: {}", other)),
    };
    for i in items {
        w(out, &i)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// validate / schema
// ---------------------------------------------------------------------------

/// Why `cameleer validate` / `cameleer schema` are missing from a build without the `validate`
/// feature.
#[cfg(not(feature = "validate"))]
fn validate_missing(cmd: &str) -> String {
    format!("`cameleer {cmd}` is not in this binary: it was built without the `validate` feature; rebuild cameleer-main with `--features validate`")
}

#[cfg(not(feature = "validate"))]
fn validate_cmd(_args: &[String], _out: &mut dyn Write) -> Result<i32, String> {
    Err(validate_missing("validate"))
}

#[cfg(not(feature = "validate"))]
fn schema_cmd(_args: &[String], _out: &mut dyn Write) -> CliResult {
    Err(validate_missing("schema"))
}

/// `cameleer validate`: returns the exit code (0 valid, 1 invalid); `Err` is a usage error.
#[cfg(feature = "validate")]
fn validate_cmd(args: &[String], out: &mut dyn Write) -> Result<i32, String> {
    let a = Args::parse(args, &["strict"])?;
    a.check(&["format", "strict", "properties", "prop"])?;
    if a.positional.is_empty() {
        return Err(format!("cameleer validate requires at least one file or directory\n{}", usage()));
    }
    let format = a.get("format").unwrap_or("text");
    if format != "text" && format != "json" {
        return Err(format!("Unknown format '{}' (text, json)", format));
    }
    // like `cameleer run`: --properties files, else application.* of the current directory
    let mut props = std::collections::BTreeMap::new();
    let files: Vec<PathBuf> = match a.all("properties") {
        f if f.is_empty() => crate::config::application_files(Path::new(".")),
        f => f.into_iter().map(PathBuf::from).collect(),
    };
    for f in files {
        let (base, _) = crate::config::load_file(&f).map_err(|e| format!("Cannot load properties {}: {}", f.display(), e.message))?;
        props.extend(base);
    }
    for kv in a.all("prop") {
        let (k, v) = kv.split_once('=').ok_or_else(|| format!("--prop expects key=value, got {}", kv))?;
        props.insert(k.to_string(), v.to_string());
    }
    let report = crate::validate::validate_paths(&a.positional, props)?;
    let strict = a.flag("strict");
    let text = if format == "json" { report.to_json(strict) + "\n" } else { report.to_text(strict) };
    out.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    Ok(report.exit_code(strict))
}

#[cfg(feature = "validate")]
fn schema_cmd(args: &[String], out: &mut dyn Write) -> CliResult {
    let a = Args::parse(args, &[])?;
    a.check(&[])?;
    out.write_all(crate::schema::json_schema().as_bytes()).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// init
// ---------------------------------------------------------------------------

fn base_name(file: &str) -> String {
    let f = Path::new(file).file_name().and_then(|n| n.to_str()).unwrap_or(file);
    f.split('.').next().unwrap_or(f).to_string()
}

/// The template for a new file (by its name).
pub fn init_template(file: &str) -> Result<String, String> {
    let name = base_name(file);
    let lower = file.to_ascii_lowercase();
    Ok(if lower.ends_with(".kamelet.yaml") || lower.ends_with(".kamelet.yml") {
        format!(
            "apiVersion: camel.apache.org/v1\nkind: Kamelet\nmetadata:\n  name: {name}\n  labels:\n    camel.apache.org/kamelet.type: source\nspec:\n  definition:\n    title: \"{name}\"\n    description: \"Produces periodic events with a custom payload\"\n    required:\n      - message\n    properties:\n      period:\n        title: Period\n        type: integer\n        default: 1000\n      message:\n        title: Message\n        type: string\n  dataTypes:\n    out:\n      default: text\n      types:\n        text:\n          mediaType: text/plain\n  template:\n    from:\n      uri: \"timer:{name}\"\n      parameters:\n        period: \"{{{{period}}}}\"\n      steps:\n        - setBody:\n            constant: \"{{{{message}}}}\"\n        - to: \"kamelet:sink\"\n"
        )
    } else if lower.ends_with(".pipe.yaml") || lower.ends_with(".pipe.yml") {
        format!(
            "apiVersion: camel.apache.org/v1\nkind: Pipe\nmetadata:\n  name: {name}\nspec:\n  source:\n    ref:\n      kind: Kamelet\n      apiVersion: camel.apache.org/v1\n      name: timer-source\n    properties:\n      message: \"Hello from {name}\"\n  sink:\n    ref:\n      kind: Kamelet\n      apiVersion: camel.apache.org/v1\n      name: log-sink\n"
        )
    } else if lower.ends_with(".integration.yaml") || lower.ends_with(".integration.yml") {
        format!(
            "apiVersion: camel.apache.org/v1\nkind: Integration\nmetadata:\n  name: {name}\nspec:\n  traits:\n    camel:\n      properties:\n        - greeting=Hello from {name}\n  flows:\n    - from:\n        uri: \"timer:yaml\"\n        parameters:\n          period: \"1000\"\n        steps:\n          - setBody:\n              simple: \"{{{{greeting}}}}\"\n          - log: \"${{body}}\"\n"
        )
    } else if lower.ends_with(".yaml") || lower.ends_with(".yml") {
        "# cameleer run this file with: cameleer run FILE\n- route:\n    from:\n      uri: \"timer:yaml\"\n      parameters:\n        period: \"1000\"\n      steps:\n        - setBody:\n            simple: \"Hello Camel from ${routeId}\"\n        - log: \"${body}\"\n".to_string()
    } else if lower.ends_with(".xml") {
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<routes xmlns=\"http://camel.apache.org/schema/spring\">\n    <route>\n        <from uri=\"timer:xml?period=1000\"/>\n        <setBody>\n            <simple>Hello Camel from ${routeId}</simple>\n        </setBody>\n        <log message=\"${body}\"/>\n    </route>\n</routes>\n".to_string()
    } else if lower.ends_with(".properties") {
        "# application configuration\ncamel.main.name = Cameleer\n# camel.main.duration-max-seconds = 60\n".to_string()
    } else {
        return Err(format!("Cannot create {}: supported extensions are .yaml, .xml, .kamelet.yaml, .pipe.yaml, .integration.yaml, .properties", file));
    })
}

fn init(args: &[String], out: &mut dyn Write) -> CliResult {
    let a = Args::parse(args, &[])?;
    a.check(&["directory"])?;
    let file = a.positional.first().ok_or("cameleer init requires a file name")?;
    let dir = PathBuf::from(a.get("directory").unwrap_or("."));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(file);
    if path.exists() {
        return Err(format!("{} already exists", path.display()));
    }
    std::fs::write(&path, init_template(file)?).map_err(|e| e.to_string())?;
    w(out, &format!("Created {}", path.display()))
}

// ---------------------------------------------------------------------------
// run
// ---------------------------------------------------------------------------

fn run_cmd(args: &[String]) -> CliResult {
    let a = Args::parse(args, &["dev", "console", "trace", "background", "mcp", "mcp-stdio"])?;
    a.check(&[
        "properties", "profile", "dev", "name", "background", "max-seconds", "max-messages", "max-idle-seconds", "prop", "log-level", "console",
        "port", "trace", "mcp", "mcp-stdio",
    ])?;
    if a.flag("mcp-stdio") && a.flag("background") {
        return Err("--mcp-stdio talks over stdin and stdout; it can't run in the background".into());
    }
    if a.flag("background") {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let fwd: Vec<&String> = args.iter().filter(|s| s.as_str() != "--background").collect();
        let child = std::process::Command::new(exe)
            .arg("run")
            .args(fwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("Cannot start in background: {}", e))?;
        println!("Running in background with PID: {}", child.id());
        return Ok(());
    }
    let mut main = crate::new_main();
    match a.get("properties") {
        Some(f) => main.load_properties_file(f).map_err(|e| e.to_string())?,
        None => {
            main.load_application_config(".").map_err(|e| e.to_string())?;
        }
    }
    if let Some(p) = a.get("profile") {
        main.set_profile(p);
    } else if a.flag("dev") {
        main.set_profile("dev");
    }
    let mut files = a.positional.clone();
    if files.is_empty() {
        // like Camel JBang: run the route files of the current directory
        for f in crate::glob_files("*").unwrap_or_default() {
            let n = f.to_string_lossy().to_string();
            if [".yaml", ".yml", ".xml"].iter().any(|e| n.ends_with(e)) && !n.contains("application") && !n.ends_with(".test.yaml") && !n.ends_with(".test.yml") {
                files.push(n);
            }
        }
    }
    for f in &files {
        main.add_route_file(f);
    }
    for (opt, key) in [
        ("max-seconds", "camel.main.duration-max-seconds"),
        ("max-messages", "camel.main.duration-max-messages"),
        ("max-idle-seconds", "camel.main.duration-max-idle-seconds"),
        ("log-level", "camel.main.log-level"),
        ("name", "camel.main.name"),
    ] {
        if let Some(v) = a.get(opt) {
            main.set_property(key, v);
        }
    }
    if a.flag("dev") {
        main.set_property("camel.main.dev-reload", "true");
    }
    if a.flag("console") {
        for k in ["camel.server.enabled", "camel.server.devConsoleEnabled", "camel.server.healthCheckEnabled", "camel.server.metricsEnabled"] {
            main.set_property(k, "true");
        }
    }
    if let Some(p) = a.get("port") {
        p.parse::<u16>().map_err(|_| format!("Invalid port: {}", p))?;
        main.set_property("camel.server.port", p);
    }
    if a.flag("trace") {
        main.set_property("camel.main.tracing", "true");
    }
    // the MCP server; on the command line it exposes every tool unless camel.server.mcp-tags says otherwise
    if a.flag("mcp") || a.flag("mcp-stdio") {
        main.set_property("camel.server.mcp-enabled", "true");
        main.set_property("camel.server.mcp-transport", if a.flag("mcp-stdio") { "stdio" } else { "http" });
        main.set_default_property("camel.server.mcp-tags", "*");
    }
    for kv in a.all("prop") {
        let (k, v) = kv.split_once('=').ok_or_else(|| format!("--prop expects key=value, got {}", kv))?;
        main.set_property(k, v);
    }
    if a.get("name").is_none() {
        if let Some(f) = files.first() {
            main.set_property("camel.main.name", base_name(f));
        }
    }
    // status and log files for `cameleer ps/stop/log/get/cmd`; CAMEL_LOCAL_STATUS=false turns
    // them off (the container image does: one process per container, logs go to stdout)
    // not on serverless platforms: AWS Lambda (read-only file system), Cloud Run and Knative
    // (K_SERVICE), Azure Functions custom handlers; one integration per instance there
    let serverless = crate::lambda::runtime_api().is_some() || ["K_SERVICE", "FUNCTIONS_CUSTOMHANDLER_PORT"].iter().any(|v| std::env::var_os(v).is_some());
    if local::local_status_enabled() && !serverless {
        let home = local::camel_home();
        main.enable_local_status(&home);
        let _ = local::log_to_file(&local::log_file(&home, std::process::id()));
    }
    main.run().map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// export
// ---------------------------------------------------------------------------

/// Path of the Cameleer crates directory used by exported projects.
fn default_camel_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().map(Path::to_path_buf).unwrap_or_default()
}

fn rust_ident(s: &str) -> String {
    let mut o: String = s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c.to_ascii_lowercase() } else { '-' }).collect();
    if o.is_empty() || !o.chars().next().unwrap().is_ascii_alphabetic() {
        o = format!("camel-{}", o);
    }
    o
}

/// Generate a standalone Cargo project embedding route and configuration files.
pub fn export_project(files: &[PathBuf], dir: &Path, name: &str, camel_path: &Path) -> Result<Vec<PathBuf>, String> {
    let mut created = Vec::new();
    let res = dir.join("resources");
    std::fs::create_dir_all(dir.join("src")).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&res).map_err(|e| e.to_string())?;
    let mut routes = Vec::new();
    let mut config = Vec::new();
    for f in files {
        let fname = f.file_name().and_then(|n| n.to_str()).ok_or_else(|| format!("Invalid file {}", f.display()))?.to_string();
        let target = res.join(&fname);
        std::fs::copy(f, &target).map_err(|e| format!("Cannot copy {}: {}", f.display(), e))?;
        created.push(target);
        if fname.starts_with("application") {
            config.push(fname);
        } else {
            routes.push(fname);
        }
    }
    if routes.is_empty() {
        return Err("No route files to export".into());
    }
    let pkg = rust_ident(name);
    let cp = camel_path.display().to_string().replace('\\', "/");
    let cargo = format!(
        "[package]\nname = \"{pkg}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n# Exported by `cameleer export`: a standalone Cameleer application.\n[dependencies]\ncameleer-main = {{ path = \"{cp}/cameleer-main\" }}\n\n[workspace]\n"
    );
    let mut main_rs = String::from("//! Generated by `cameleer export`.\n\nfn main() -> std::process::ExitCode {\n    let mut main = cameleer_main::new_main();\n");
    main_rs.push_str(&format!("    main.set_property(\"camel.main.name\", {:?});\n", name));
    for c in &config {
        let yaml = c.ends_with(".yaml") || c.ends_with(".yml");
        main_rs.push_str(&format!(
            "    if let Err(e) = main.load_properties_text(include_str!(\"../resources/{c}\"), {yaml}) {{\n        eprintln!(\"{{}}\", e);\n        return std::process::ExitCode::FAILURE;\n    }}\n"
        ));
    }
    main_rs.push_str("    // application.properties/yaml next to the binary override the embedded configuration\n    let _ = main.load_application_config(\".\");\n");
    for r in &routes {
        main_rs.push_str(&format!("    main.add_route_source({:?}, include_str!(\"../resources/{}\"));\n", r, r));
    }
    main_rs.push_str("    match main.run() {\n        Ok(()) => std::process::ExitCode::SUCCESS,\n        Err(e) => {\n            eprintln!(\"{}\", e);\n            std::process::ExitCode::FAILURE\n        }\n    }\n}\n");
    for (p, content) in [(dir.join("Cargo.toml"), cargo), (dir.join("src/main.rs"), main_rs)] {
        std::fs::write(&p, content).map_err(|e| e.to_string())?;
        created.push(p);
    }
    Ok(created)
}

fn export(args: &[String], out: &mut dyn Write) -> CliResult {
    let a = Args::parse(args, &[])?;
    a.check(&["directory", "name", "camel-path"])?;
    let dir = PathBuf::from(a.get("directory").ok_or("cameleer export requires --directory")?);
    let mut files: Vec<PathBuf> = a.positional.iter().map(PathBuf::from).collect();
    if files.is_empty() {
        for f in crate::glob_files("*").unwrap_or_default() {
            let n = f.to_string_lossy();
            if [".yaml", ".yml", ".xml", ".properties"].iter().any(|e| n.ends_with(e)) {
                files.push(f);
            }
        }
    } else {
        for f in crate::config::application_files(Path::new(".")) {
            if !files.iter().any(|x| x.file_name() == f.file_name()) {
                files.push(f);
            }
        }
    }
    let name = a.get("name").map(str::to_string).unwrap_or_else(|| {
        files.iter().find(|f| !f.to_string_lossy().contains("application")).map(|f| base_name(&f.to_string_lossy())).unwrap_or_else(|| "camel-app".into())
    });
    let camel_path = a.get("camel-path").map(PathBuf::from).unwrap_or_else(default_camel_path);
    let created = export_project(&files, &dir, &name, &camel_path)?;
    for c in created {
        w(out, &format!("Created {}", c.display()))?;
    }
    w(out, &format!("Build and run with: cd {} && cargo run", dir.display()))
}

// ---------------------------------------------------------------------------
// bind
// ---------------------------------------------------------------------------

fn endpoint_ref(s: &str) -> Value {
    if s.contains(':') && !s.starts_with("kamelet:") {
        Value::object([("uri", Value::from(s))])
    } else {
        let name = s.strip_prefix("kamelet:").unwrap_or(s);
        Value::object([(
            "ref",
            Value::object([("kind", Value::from("Kamelet")), ("apiVersion", Value::from("camel.apache.org/v1")), ("name", Value::from(name))]),
        )])
    }
}

/// Build a Pipe resource.
pub fn pipe_yaml(name: &str, source: &str, sink: &str, steps: &[&str], properties: &[&str], error_handler: Option<&str>) -> Result<String, String> {
    let mut source_v = endpoint_ref(source);
    let mut sink_v = endpoint_ref(sink);
    let mut steps_v: Vec<Value> = steps.iter().map(|s| endpoint_ref(s)).collect();
    for p in properties {
        let (k, v) = p.split_once('=').ok_or_else(|| format!("--property expects target.key=value, got {}", p))?;
        let (target, key) = k.split_once('.').ok_or_else(|| format!("--property key must start with source., sink. or step-N.: {}", k))?;
        let node = match target {
            "source" => &mut source_v,
            "sink" => &mut sink_v,
            t => {
                let idx: usize = t.strip_prefix("step-").and_then(|n| n.parse().ok()).ok_or_else(|| format!("Unknown property target {}", t))?;
                steps_v.get_mut(idx).ok_or_else(|| format!("No step {}", idx))?
            }
        };
        if let Value::Object(m) = node {
            let props = m.entry("properties".to_string()).or_insert_with(|| Value::Object(Default::default()));
            if let Value::Object(pm) = props {
                pm.insert(key.to_string(), Value::from(v));
            }
        }
    }
    let mut spec = vec![("source", source_v)];
    if !steps_v.is_empty() {
        spec.push(("steps", Value::Array(steps_v)));
    }
    spec.push(("sink", sink_v));
    if let Some(eh) = error_handler {
        let v = match eh {
            "none" => Value::object([("none", Value::Object(Default::default()))]),
            "log" => Value::object([("log", Value::Object(Default::default()))]),
            other => match other.strip_prefix("sink:") {
                Some(target) => Value::object([("sink", Value::object([("endpoint", endpoint_ref(target))]))]),
                None => return Err(format!("Unknown error handler {} (use log, none or sink:<uri or kamelet>)", other)),
            },
        };
        spec.push(("errorHandler", v));
    }
    let doc = Value::object([
        ("apiVersion", Value::from("camel.apache.org/v1")),
        ("kind", Value::from("Pipe")),
        ("metadata", Value::object([("name", Value::from(name))])),
        ("spec", Value::object(spec)),
    ]);
    Ok(cameleer_yaml::to_yaml(&doc))
}

fn bind(args: &[String], out: &mut dyn Write) -> CliResult {
    let a = Args::parse(args, &[])?;
    a.check(&["source", "sink", "step", "property", "error-handler", "output"])?;
    let name = a.positional.first().ok_or("cameleer bind requires a name")?;
    let text = pipe_yaml(
        name,
        a.get("source").ok_or("cameleer bind requires --source")?,
        a.get("sink").ok_or("cameleer bind requires --sink")?,
        &a.all("step"),
        &a.all("property"),
        a.get("error-handler"),
    )?;
    let file = a.get("output").map(str::to_string).unwrap_or_else(|| format!("{}.yaml", name));
    std::fs::write(&file, text).map_err(|e| e.to_string())?;
    w(out, &format!("Created {}", file))
}

// ---------------------------------------------------------------------------
// ps / stop / get / cmd / log / trace (local integrations)
// ---------------------------------------------------------------------------

fn home() -> PathBuf {
    local::camel_home()
}

fn age(status: &Value) -> String {
    let ms = status.get("context").and_then(|c| c.get("uptimeMillis")).and_then(|v| v.as_i64());
    match ms {
        Some(ms) => format!("{}s", ms / 1000),
        None => status.get("context").and_then(|c| c.get("uptime")).map(|v| v.to_string()).unwrap_or_default(),
    }
}

fn ps(out: &mut dyn Write) -> CliResult {
    let procs = local::list_processes(&home());
    w(out, &format!("{:<8} {:<24} {:<10} {:<8} {:<8} {:<8}", "PID", "NAME", "AGE", "ROUTES", "TOTAL", "FAILED"))?;
    for p in procs {
        let routes = p.status.get("routes").and_then(|r| r.as_array()).map(|a| a.len()).unwrap_or(0);
        let ctx = p.status.get("context").cloned().unwrap_or(Value::Null);
        let num = |k: &str| ctx.get(k).map(|v| v.to_string()).unwrap_or_else(|| "0".into());
        w(out, &format!("{:<8} {:<24} {:<10} {:<8} {:<8} {:<8}", p.pid, p.name, age(&p.status), routes, num("exchangesTotal"), num("exchangesFailed")))?;
    }
    Ok(())
}

fn stop(args: &[String], out: &mut dyn Write) -> CliResult {
    let a = Args::parse(args, &["all"])?;
    a.check(&["all"])?;
    let dir = home();
    let targets = if a.flag("all") {
        local::list_processes(&dir)
    } else {
        vec![local::find_process(&dir, a.positional.first().map(String::as_str)).map_err(|e| e.message)?]
    };
    for p in targets {
        local::send_action(&dir, p.pid, &Value::object([("action", Value::from("stop"))]), Duration::from_secs(10)).map_err(|e| e.message)?;
        w(out, &format!("Stopping {} ({})", p.name, p.pid))?;
    }
    Ok(())
}

fn get(args: &[String], out: &mut dyn Write) -> CliResult {
    let a = Args::parse(args, &[])?;
    a.check(&["url"])?;
    let what = a.positional.first().map(String::as_str).unwrap_or("routes");
    if !["routes", "route", "context", "health", "endpoints"].contains(&what) {
        return Err(format!("Unknown get kind: {} (use routes, context, health or endpoints)", what));
    }
    let v = match a.get("url") {
        Some(url) => {
            let url = url.trim_end_matches('/');
            let path = match what {
                "routes" | "route" => "/q/dev/routes",
                "context" => "/q/dev/context",
                "health" => "/q/health",
                _ => "/q/dev/endpoints",
            };
            let client = cameleer_http::HttpClient { connect_timeout: Duration::from_secs(5), ..Default::default() };
            let resp = client.get(&format!("{}{}", url, path)).map_err(|e| e.to_string())?;
            if resp.status != 200 && !(what == "health" && resp.status == 503) {
                return Err(format!("{} returned HTTP {}: {}", path, resp.status, resp.body_text()));
            }
            cameleer_core::json::parse(&resp.body_text()).map_err(|e| e.to_string())?
        }
        None => {
            let p = local::find_process(&home(), a.positional.get(1).map(String::as_str)).map_err(|e| e.message)?;
            let key = if what == "route" { "routes" } else { what };
            let inner = p.status.get(key).cloned().unwrap_or(Value::Null);
            match key {
                "routes" => Value::object([("routes", inner)]),
                "endpoints" => Value::object([("endpoints", inner)]),
                _ => inner,
            }
        }
    };
    match what {
        "routes" | "route" => write!(out, "{}", crate::console::format_routes_table(&v)).map_err(|e| e.to_string()),
        "health" => {
            w(out, &v.to_json_pretty())?;
            match v.get("status").map(|s| s.to_string()) {
                Some(s) if s.eq_ignore_ascii_case("DOWN") => Err("health check is DOWN".to_string()),
                _ => Ok(()),
            }
        }
        _ => w(out, &v.to_json_pretty()),
    }
}

fn cmd_action(args: &[String], out: &mut dyn Write) -> CliResult {
    let a = Args::parse(args, &[])?;
    a.check(&["id"])?;
    let action = a.positional.first().ok_or("cameleer cmd requires an action")?;
    if !["start-route", "stop-route", "suspend-route", "resume-route", "reset-stats", "reload"].contains(&action.as_str()) {
        return Err(format!("Unknown cmd action {}", action));
    }
    let dir = home();
    let p = local::find_process(&dir, a.positional.get(1).map(String::as_str)).map_err(|e| e.message)?;
    let doc = Value::object([("action", Value::from(action.as_str())), ("id", Value::from(a.get("id").unwrap_or("*")))]);
    let r = local::send_action(&dir, p.pid, &doc, Duration::from_secs(30)).map_err(|e| e.message)?;
    let msg = r.get("message").map(|m| m.to_string()).unwrap_or_default();
    if r.get("status").map(|s| s.to_string()) == Some("error".into()) {
        return Err(msg);
    }
    w(out, &msg)
}

/// `cameleer tool ...`: list, disable and enable the AI tools of a running integration, and
/// list, approve and deny the calls waiting for approval (see [`crate::tools`]).
fn tool_cmd(args: &[String], out: &mut dyn Write) -> CliResult {
    let a = Args::parse(args, &["json"])?;
    let sub = a.positional.first().map(String::as_str).ok_or_else(|| format!("cameleer tool requires list, disable, enable, approvals, approve or deny
{}", usage()))?;
    // (positional arguments after the subcommand, allowed options)
    let (operands, allowed): (usize, &[&str]) = match sub {
        "list" | "approvals" => (0, &["json"]),
        "disable" | "enable" => (1, &[]),
        "approve" => (1, &["by"]),
        "deny" => (1, &["by", "reason"]),
        other => return Err(format!("Unknown tool command {} (list, disable, enable, approvals, approve or deny)", other)),
    };
    a.check(allowed)?;
    if !crate::tools::available() {
        return Err(crate::tools::NO_AI_FEATURE.to_string());
    }
    let operand = if operands == 1 {
        Some(a.positional.get(1).ok_or_else(|| format!("cameleer tool {} requires {}", sub, if sub == "approve" || sub == "deny" { "an approval handle" } else { "a tool name" }))?.as_str())
    } else {
        None
    };
    let selector = a.positional.get(1 + operands).map(String::as_str);
    if a.positional.len() > 2 + operands {
        return Err(format!("Too many arguments for cameleer tool {}
{}", sub, usage()));
    }
    let by = a.get("by").map(str::to_string).unwrap_or_else(|| std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_else(|_| "cli".into()));
    let mut fields: Vec<(&str, Value)> = vec![("action", Value::from(format!("tool-{}", sub)))];
    match sub {
        "disable" | "enable" => fields.push(("tool", Value::from(operand.unwrap_or_default()))),
        "approve" | "deny" => {
            fields.push(("handle", Value::from(operand.unwrap_or_default())));
            fields.push(("by", Value::from(by)));
            fields.push(("reason", Value::from(a.get("reason").unwrap_or(""))));
        }
        _ => {}
    }
    let dir = home();
    let p = local::find_process(&dir, selector).map_err(|e| e.message)?;
    // approving runs the call, which may take up to the tool's timeout
    let timeout = Duration::from_secs(if sub == "approve" { 120 } else { 30 });
    let r = local::send_action(&dir, p.pid, &Value::object(fields), timeout).map_err(|e| e.message)?;
    let msg = r.get("message").map(|m| m.to_string()).unwrap_or_default();
    if r.get("status").map(|s| s.to_string()) == Some("error".into()) {
        return Err(if msg.starts_with("Unknown action tool-") { format!("{} ({}) does not support AI tool commands: it is an older Cameleer", p.name, p.pid) } else { msg });
    }
    let data = r.get("data").cloned().unwrap_or(Value::Null);
    match sub {
        "list" | "approvals" if a.flag("json") => w(out, &data.to_json()),
        "list" => write!(out, "{}", crate::tools::format_tools(&data)).map_err(|e| e.to_string()),
        "approvals" => write!(out, "{}", crate::tools::format_approvals(&data)).map_err(|e| e.to_string()),
        "approve" | "deny" => {
            w(out, &msg)?;
            if data.get("isError").map(|e| e.is_truthy()).unwrap_or(false) && data.get("status").map(|s| s.to_string()).as_deref() != Some("denied") {
                return Err(format!("the approved call failed: {}", data.get("text").map(|t| t.to_string()).unwrap_or_default()));
            }
            Ok(())
        }
        _ => w(out, &msg),
    }
}

fn log(args: &[String], out: &mut dyn Write) -> CliResult {
    let a = Args::parse(args, &["follow"])?;
    a.check(&["follow", "tail"])?;
    let dir = home();
    let p = local::find_process(&dir, a.positional.first().map(String::as_str)).map_err(|e| e.message)?;
    let path = local::log_file(&dir, p.pid);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("Cannot read {}: {}", path.display(), e))?;
    let lines: Vec<&str> = text.lines().collect();
    let tail: usize = a.get("tail").map(|t| t.parse().map_err(|_| format!("Invalid --tail {}", t))).transpose()?.unwrap_or(lines.len());
    for l in &lines[lines.len().saturating_sub(tail)..] {
        w(out, l)?;
    }
    if a.flag("follow") {
        let mut pos = text.len();
        loop {
            std::thread::sleep(Duration::from_millis(250));
            let Ok(t) = std::fs::read_to_string(&path) else { return Ok(()) };
            if t.len() > pos {
                write!(out, "{}", &t[pos..]).map_err(|e| e.to_string())?;
                let _ = out.flush();
                pos = t.len();
            }
            if local::find_process(&dir, Some(&p.pid.to_string())).is_err() {
                return Ok(());
            }
        }
    }
    Ok(())
}

fn trace(args: &[String], out: &mut dyn Write) -> CliResult {
    let a = Args::parse(args, &["disable"])?;
    a.check(&["disable"])?;
    let dir = home();
    let p = local::find_process(&dir, a.positional.first().map(String::as_str)).map_err(|e| e.message)?;
    let enabled = p.status.get("trace").and_then(|t| t.get("enabled")).map(|v| v.is_truthy()).unwrap_or(false);
    if a.flag("disable") || !enabled {
        let doc = Value::object([("action", Value::from("trace")), ("enabled", Value::Bool(!a.flag("disable")))]);
        let r = local::send_action(&dir, p.pid, &doc, Duration::from_secs(10)).map_err(|e| e.message)?;
        return w(out, &r.get("message").map(|m| m.to_string()).unwrap_or_default());
    }
    if let Some(Value::Array(events)) = p.status.get("trace").and_then(|t| t.get("events")) {
        for e in events {
            match e.get("message") {
                Some(m) => w(out, &m.to_string())?,
                None => w(out, &e.to_json())?,
            }
        }
    }
    Ok(())
}

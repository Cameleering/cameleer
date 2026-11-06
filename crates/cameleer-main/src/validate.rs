//! `cameleer validate`: check route files without running them.
//!
//! Nothing is started and no connection is opened. Each file is checked for:
//!
//! | code | severity | what |
//! |---|---|---|
//! | `yaml-syntax` | error | the file is not valid YAML |
//! | `xml-syntax` | error | an `.xml` file is not well-formed XML |
//! | `dsl-structure` | error | not valid Camel YAML DSL: unknown element, step or key, a step in the wrong place, a missing required key (also any other error of the real loader) |
//! | `endpoint` | error | a static endpoint URI does not resolve: unknown scheme or unknown option (like Apache Camel) |
//! | `expression` | error | an expression does not compile |
//! | `unresolved-placeholder` | warning | a `{{placeholder}}` without default and without a value in the given properties |
//! | `dangling-endpoint` | warning | a producer to `direct:`, `seda:` or `vm:` that no validated route consumes |
//! | `duplicate-route-id` | error | two routes with the same id |
//!
//! The structure checks use the DSL model of the loader ([`cameleer_yaml::model`]); every file
//! that passes them is then loaded with the real loader into a context that is not started
//! (errors it reports become `dsl-structure` findings). Endpoints are resolved with
//! [`CamelContext::check_endpoint`] on a context with every component ([`crate::new_main`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cameleer_core::builder::ExpressionDefinition;
use cameleer_core::{CamelContext, Value};
use cameleer_yaml::model::{self, Expr, Key, Kind, Model, Role, Shorthand};
use cameleer_yaml::yaml::{Node, PathSegment};

/// Severity of a finding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

/// The finding codes.
pub mod codes {
    pub const YAML_SYNTAX: &str = "yaml-syntax";
    pub const XML_SYNTAX: &str = "xml-syntax";
    pub const DSL_STRUCTURE: &str = "dsl-structure";
    pub const ENDPOINT: &str = "endpoint";
    pub const EXPRESSION: &str = "expression";
    pub const UNRESOLVED_PLACEHOLDER: &str = "unresolved-placeholder";
    pub const DANGLING_ENDPOINT: &str = "dangling-endpoint";
    pub const DUPLICATE_ROUTE_ID: &str = "duplicate-route-id";
}

/// One problem found in a file.
#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    pub severity: Severity,
    pub code: &'static str,
    pub message: String,
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub hint: Option<String>,
}

/// The result for one file.
#[derive(Clone, Debug, Default)]
pub struct FileReport {
    pub path: String,
    /// Routes defined by the file (routes, templated routes, REST operations, Pipes).
    pub routes: usize,
    pub findings: Vec<Finding>,
}

impl FileReport {
    pub fn errors(&self) -> usize {
        self.findings.iter().filter(|f| f.severity == Severity::Error).count()
    }
    pub fn warnings(&self) -> usize {
        self.findings.iter().filter(|f| f.severity == Severity::Warning).count()
    }
    /// No errors (and, when `strict`, no warnings).
    pub fn valid(&self, strict: bool) -> bool {
        self.errors() == 0 && (!strict || self.warnings() == 0)
    }
    /// Findings with the given code.
    pub fn with_code(&self, code: &str) -> Vec<&Finding> {
        self.findings.iter().filter(|f| f.code == code).collect()
    }
}

/// The result of a validation run.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub files: Vec<FileReport>,
}

impl Report {
    pub fn errors(&self) -> usize {
        self.files.iter().map(|f| f.errors()).sum()
    }
    pub fn warnings(&self) -> usize {
        self.files.iter().map(|f| f.warnings()).sum()
    }
    pub fn valid(&self, strict: bool) -> bool {
        self.files.iter().all(|f| f.valid(strict))
    }
    /// The process exit code: 0 valid, 1 errors (or warnings when `strict`).
    pub fn exit_code(&self, strict: bool) -> i32 {
        if self.valid(strict) {
            0
        } else {
            1
        }
    }

    /// One line per finding (`path:line:column: error[code]: message (hint)`) and a summary.
    pub fn to_text(&self, strict: bool) -> String {
        let mut out = String::new();
        for f in &self.files {
            for x in &f.findings {
                out.push_str(&f.path);
                if let Some(l) = x.line {
                    out.push_str(&format!(":{}", l));
                    if let Some(c) = x.column {
                        out.push_str(&format!(":{}", c));
                    }
                }
                out.push_str(&format!(": {}[{}]: {}", x.severity.as_str(), x.code, x.message));
                if let Some(h) = &x.hint {
                    out.push_str(&format!(" ({})", h));
                }
                out.push('\n');
            }
        }
        let routes: usize = self.files.iter().map(|f| f.routes).sum();
        let (e, w) = (self.errors(), self.warnings());
        out.push_str(&format!(
            "{}: {} {}, {} {}, {} {}, {} {}\n",
            if self.valid(strict) { "valid" } else { "invalid" },
            self.files.len(),
            plural(self.files.len(), "file"),
            routes,
            plural(routes, "route"),
            e,
            plural(e, "error"),
            w,
            plural(w, "warning"),
        ));
        out
    }

    /// The JSON report: `{"valid", "files": [{"path", "valid", "routes", "findings"}], "summary"}`.
    pub fn to_json(&self, strict: bool) -> String {
        let mut out = String::new();
        out.push_str(&format!("{{\"valid\":{},\"files\":[", self.valid(strict)));
        for (i, f) in self.files.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!("{{\"path\":{},\"valid\":{},\"routes\":{},\"findings\":[", json_str(&f.path), f.valid(strict), f.routes));
            for (j, x) in f.findings.iter().enumerate() {
                if j > 0 {
                    out.push(',');
                }
                let opt_n = |n: Option<usize>| n.map(|n| n.to_string()).unwrap_or_else(|| "null".into());
                out.push_str(&format!(
                    "{{\"severity\":\"{}\",\"code\":\"{}\",\"message\":{},\"line\":{},\"column\":{},\"hint\":{}}}",
                    x.severity.as_str(),
                    x.code,
                    json_str(&x.message),
                    opt_n(x.line),
                    opt_n(x.column),
                    x.hint.as_deref().map(json_str).unwrap_or_else(|| "null".into())
                ));
            }
            out.push_str("]}");
        }
        out.push_str(&format!(
            "],\"summary\":{{\"files\":{},\"errors\":{},\"warnings\":{}}}}}",
            self.files.len(),
            self.errors(),
            self.warnings()
        ));
        out
    }
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        word.to_string()
    } else {
        format!("{}s", word)
    }
}

fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

/// Whether a file name is a route file (`*.yaml`, `*.yml`, `*.xml`; not `application*`).
pub fn is_route_file(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_ascii_lowercase();
    // `*.test.yaml` files are `cameleer test` files, not routes
    !name.starts_with("application") && !name.ends_with(".test.yaml") && !name.ends_with(".test.yml") && [".yaml", ".yml", ".xml"].iter().any(|e| name.ends_with(e))
}

/// The route files of a directory, recursively (hidden directories and `target` skipped), sorted.
pub fn route_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect(dir, &mut out);
    out.sort();
    out
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if p.is_dir() {
            if !name.starts_with('.') && name != "target" && name != "node_modules" {
                collect(&p, out);
            }
        } else if is_route_file(&p) {
            out.push(p);
        }
    }
}

/// Expand files and directories into the list of route files. A path that does not exist is
/// an error.
pub fn expand_paths(paths: &[String]) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    for p in paths {
        let pb = PathBuf::from(p);
        if pb.is_dir() {
            out.extend(route_files(&pb));
        } else if pb.is_file() {
            out.push(pb);
        } else {
            return Err(format!("No such file or directory: {}", p));
        }
    }
    Ok(out)
}

/// Validate files and directories (see [`expand_paths`]).
pub fn validate_paths(paths: &[String], properties: BTreeMap<String, String>) -> Result<Report, String> {
    let files = expand_paths(paths)?;
    let mut v = Validator::new(properties);
    for f in &files {
        v.add_file(f);
    }
    Ok(v.finish())
}

/// Validate one in-memory source (`name` decides YAML or XML by extension).
pub fn validate_source(name: &str, text: &str, properties: BTreeMap<String, String>) -> FileReport {
    let mut v = Validator::new(properties);
    v.add_source(name, text);
    v.finish().files.pop().unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Validator
// ---------------------------------------------------------------------------

/// A use of a `direct:`/`seda:`/`vm:` endpoint (for the cross-file checks).
#[derive(Clone, Debug)]
struct Channel {
    file: usize,
    key: String,
    uri: String,
    consumer: bool,
    line: Option<usize>,
    column: Option<usize>,
}

#[derive(Clone, Debug)]
struct RouteId {
    file: usize,
    id: String,
    line: Option<usize>,
    column: Option<usize>,
}

/// Validates route files; call [`Validator::finish`] for the cross-file checks.
pub struct Validator {
    main: crate::Main,
    properties: BTreeMap<String, String>,
    files: Vec<FileReport>,
    channels: Vec<Channel>,
    route_ids: Vec<RouteId>,
}

impl Validator {
    pub fn new(properties: BTreeMap<String, String>) -> Self {
        let main = crate::new_main();
        for (k, v) in &properties {
            main.context().properties().set(k.clone(), v.clone());
        }
        Validator { main, properties, files: Vec::new(), channels: Vec::new(), route_ids: Vec::new() }
    }

    fn ctx(&self) -> &CamelContext {
        self.main.context()
    }

    /// Validate a file.
    pub fn add_file(&mut self, path: &Path) {
        let name = path.display().to_string().replace('\\', "/");
        match std::fs::read_to_string(path) {
            Ok(text) => self.add_source(&name, &text),
            Err(e) => self.files.push(FileReport {
                path: name,
                routes: 0,
                findings: vec![Finding {
                    severity: Severity::Error,
                    code: codes::YAML_SYNTAX,
                    message: format!("cannot read the file: {}", e),
                    line: None,
                    column: None,
                    hint: None,
                }],
            }),
        }
    }

    /// Validate a source text; `name` is reported as the path (`.xml` selects the XML DSL).
    pub fn add_source(&mut self, name: &str, text: &str) {
        let index = self.files.len();
        let xml = name.to_ascii_lowercase().ends_with(".xml");
        let nodes = if xml { None } else { Some(cameleer_yaml::yaml::parse_nodes(text)) };
        let mut w = Walker::new(self, index, xml);
        match &nodes {
            None => w.xml_source(text),
            Some(parsed) => w.yaml_source(parsed, text),
        }
        let Walker { findings, routes, channels, route_ids, .. } = w;
        let mut report = FileReport { path: name.to_string(), routes, findings };
        if report.errors() == 0 {
            self.load_with_loader(text, xml, &mut report);
        }
        self.channels.extend(channels);
        self.route_ids.extend(route_ids);
        self.files.push(report);
    }

    /// Load with the real loader (not started): reports what the structure checks missed.
    fn load_with_loader(&self, text: &str, xml: bool, report: &mut FileReport) {
        let ctx = self.ctx();
        let result = if xml {
            cameleer_yaml::load_routes_from_xml(ctx, text).map(|_| ())
        } else {
            match crate::integration::parse_integration(text) {
                Ok(Some(it)) => {
                    for (k, v) in &it.properties {
                        if ctx.properties().get(k).is_none() {
                            ctx.properties().set(k.clone(), v.clone());
                        }
                    }
                    let loader = cameleer_yaml::YamlLoader::new();
                    let mut r = Ok(());
                    for (_, src) in &it.sources {
                        if let Err(e) = loader.load_routes(ctx, src) {
                            r = Err(e);
                            break;
                        }
                    }
                    r
                }
                Ok(None) => cameleer_yaml::load_routes(ctx, text).map(|_| ()),
                Err(e) => Err(e),
            }
        };
        if let Err(e) = result {
            // unresolved placeholders are reported as warnings already
            if e.message.contains("Property with key [") {
                return;
            }
            let (line, column) = position_in_message(&e.message);
            report.findings.push(Finding {
                severity: Severity::Error,
                code: codes::DSL_STRUCTURE,
                message: one_line(&e.message),
                line,
                column,
                hint: None,
            });
        }
    }

    /// Run the cross-file checks and return the report.
    pub fn finish(mut self) -> Report {
        // duplicate route ids
        let mut first: BTreeMap<String, &RouteId> = BTreeMap::new();
        let mut dups: Vec<(usize, Finding)> = Vec::new();
        for r in &self.route_ids {
            match first.get(&r.id) {
                Some(prev) => {
                    let at = match prev.line {
                        Some(l) => format!("{}:{}", self.files[prev.file].path, l),
                        None => self.files[prev.file].path.clone(),
                    };
                    dups.push((
                        r.file,
                        Finding {
                            severity: Severity::Error,
                            code: codes::DUPLICATE_ROUTE_ID,
                            message: format!("duplicate route id '{}' (first defined at {})", r.id, at),
                            line: r.line,
                            column: r.column,
                            hint: Some("route ids must be unique in a CamelContext".into()),
                        },
                    ));
                }
                None => {
                    first.insert(r.id.clone(), r);
                }
            }
        }
        // producers to direct/seda/vm without a consumer
        let consumers: Vec<&str> = self.channels.iter().filter(|c| c.consumer).map(|c| c.key.as_str()).collect();
        let mut dangling: Vec<(usize, Finding)> = Vec::new();
        for c in self.channels.iter().filter(|c| !c.consumer) {
            if !consumers.contains(&c.key.as_str()) {
                dangling.push((
                    c.file,
                    Finding {
                        severity: Severity::Warning,
                        code: codes::DANGLING_ENDPOINT,
                        message: format!("no route consumes from '{}' in the validated files", c.uri),
                        line: c.line,
                        column: c.column,
                        hint: Some(format!("add a route `from: {}` or validate the file that has it", c.key)),
                    },
                ));
            }
        }
        for (i, f) in dups.into_iter().chain(dangling) {
            self.files[i].findings.push(f);
        }
        for f in &mut self.files {
            f.findings.sort_by_key(|x| (x.line.unwrap_or(0), x.column.unwrap_or(0)));
        }
        Report { files: self.files }
    }
}

/// `(line N, column M` or `at line N, column M` in a loader error message.
fn position_in_message(msg: &str) -> (Option<usize>, Option<usize>) {
    let Some(p) = msg.find("line ") else { return (None, None) };
    let rest = &msg[p + 5..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let line = digits.parse().ok();
    let column = rest.find("column ").and_then(|q| rest[q + 7..].chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().ok());
    (line, column)
}

// ---------------------------------------------------------------------------
// Placeholders
// ---------------------------------------------------------------------------

/// Result of substituting the property placeholders of a text.
struct Substituted {
    text: String,
    /// Placeholders without value (`{{name}}`), and whether each is a template parameter.
    unresolved: Vec<(String, bool)>,
    /// Whether a placeholder was replaced by an empty default (`{{env:SECRET:}}`).
    empty_default: bool,
}

// ---------------------------------------------------------------------------
// Walker
// ---------------------------------------------------------------------------

/// Expression languages that need beans or the registry at runtime: not compiled.
const NOT_COMPILED: &[&str] = &["method", "bean", "ref", "wasm"];

struct Walker<'a> {
    ctx: &'a CamelContext,
    properties: &'a BTreeMap<String, String>,
    file: usize,
    xml: bool,
    node: Option<&'a Node>,
    /// Path prefix of multi-document streams (positions are looked up below it).
    findings: Vec<Finding>,
    routes: usize,
    channels: Vec<Channel>,
    route_ids: Vec<RouteId>,
    /// Template parameters (name, default) while walking a route template or Kamelet.
    template: Option<Vec<(String, Option<String>)>>,
    /// Properties of a Camel K Integration resource.
    extra_properties: BTreeMap<String, String>,
    /// Keys suggested for misspelled keys of the mapping being checked.
    suggested: Vec<&'static str>,
}

impl<'a> Walker<'a> {
    fn new(v: &'a Validator, file: usize, xml: bool) -> Self {
        Walker {
            ctx: v.ctx(),
            properties: &v.properties,
            file,
            xml,
            node: None,
            findings: Vec::new(),
            routes: 0,
            channels: Vec::new(),
            route_ids: Vec::new(),
            template: None,
            extra_properties: BTreeMap::new(),
            suggested: Vec::new(),
        }
    }

    // --- findings ---

    fn pos(&self, path: &[PathSegment]) -> (Option<usize>, Option<usize>) {
        match self.node {
            Some(n) => {
                let (l, c) = n.position(path);
                (Some(l), Some(c))
            }
            None => (None, None),
        }
    }

    fn push(&mut self, severity: Severity, code: &'static str, path: &[PathSegment], message: String, hint: Option<String>) {
        let (line, column) = self.pos(path);
        let message = one_line(&message);
        let f = Finding { severity, code, message, line, column, hint };
        if !self.findings.contains(&f) {
            self.findings.push(f);
        }
    }

    fn error(&mut self, code: &'static str, path: &[PathSegment], message: String, hint: Option<String>) {
        self.push(Severity::Error, code, path, message, hint);
    }

    fn structure(&mut self, path: &[PathSegment], message: String, hint: Option<String>) {
        self.error(codes::DSL_STRUCTURE, path, message, hint);
    }

    // --- sources ---

    fn yaml_source(&mut self, parsed: &'a Result<Vec<Node>, cameleer_yaml::YamlError>, text: &str) {
        let nodes = match parsed {
            Ok(n) => n,
            Err(e) => {
                self.findings.push(Finding {
                    severity: Severity::Error,
                    code: codes::YAML_SYNTAX,
                    message: e.message.clone(),
                    line: Some(e.line),
                    column: Some(e.column),
                    hint: None,
                });
                return;
            }
        };
        // Camel K Integration properties are placeholders for its flows
        if let Ok(Some(it)) = crate::integration::parse_integration(text) {
            self.extra_properties = it.properties.clone();
        }
        for node in nodes {
            let value = match node.to_value() {
                Ok(v) => v,
                Err(e) => {
                    self.findings.push(Finding {
                        severity: Severity::Error,
                        code: codes::YAML_SYNTAX,
                        message: e.message.clone(),
                        line: Some(e.line),
                        column: Some(e.column),
                        hint: None,
                    });
                    continue;
                }
            };
            // positions are looked up in this document's node tree
            self.node = Some(node);
            self.document(&value);
            self.node = None;
        }
    }

    fn xml_source(&mut self, text: &str) {
        let root = match cameleer_yaml::xml::parse_xml(text) {
            Ok(r) => r,
            Err(e) => {
                let (line, column) = position_in_message(&e.message);
                self.findings.push(Finding { severity: Severity::Error, code: codes::XML_SYNTAX, message: e.message, line, column, hint: None });
                return;
            }
        };
        let steps = cameleer_yaml::YamlLoader::new().steps;
        match cameleer_yaml::xml::xml_to_value(&root, &steps) {
            Ok(v) => self.document(&v),
            Err(e) => {
                let (line, column) = position_in_message(&e.message);
                self.findings.push(Finding { severity: Severity::Error, code: codes::DSL_STRUCTURE, message: e.message, line, column, hint: None });
            }
        }
    }

    // --- documents and top-level elements ---

    fn document(&mut self, doc: &Value) {
        match doc {
            Value::Null => {}
            Value::Array(items) => {
                for (i, item) in items.iter().enumerate() {
                    self.top_item(item, vec![PathSegment::Index(i)]);
                }
            }
            Value::Object(m) if m.contains_key("apiVersion") && m.contains_key("kind") => self.resource(doc),
            Value::Object(_) => self.top_item(doc, Vec::new()),
            other => self.structure(&[], format!("expected a list of top-level elements (`- route:`) but found {}", other.type_name()), None),
        }
    }

    /// Kamelet, Pipe, KameletBinding and Integration resources.
    fn resource(&mut self, doc: &Value) {
        let kind = doc.get("kind").and_then(|k| k.as_str()).unwrap_or("").to_string();
        match kind.as_str() {
            "Kamelet" => {
                let params: Vec<(String, Option<String>)> = doc
                    .get("spec")
                    .and_then(|s| s.get("definition"))
                    .and_then(|d| d.get("properties"))
                    .and_then(|p| p.as_object())
                    .map(|m| m.iter().map(|(k, v)| (k.clone(), v.get("default").and_then(scalar))).collect())
                    .unwrap_or_default();
                if let Some(t) = doc.get("spec").and_then(|s| s.get("template")) {
                    let path = vec![PathSegment::Key("spec".into()), PathSegment::Key("template".into())];
                    let saved = self.template.replace(params);
                    self.object(model::model("route").unwrap(), t, &path, true);
                    self.template = saved;
                }
            }
            "Pipe" | "KameletBinding" => self.routes += 1,
            "Integration" => {
                if let Some(flows) = doc.get("spec").and_then(|s| s.get("flows")) {
                    if let Value::Array(items) = flows {
                        for (i, item) in items.iter().enumerate() {
                            self.top_item(item, vec![PathSegment::Key("spec".into()), PathSegment::Key("flows".into()), PathSegment::Index(i)]);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn top_item(&mut self, item: &Value, path: Vec<PathSegment>) {
        let map = match item {
            Value::Object(m) => m,
            other => {
                self.structure(&path, format!("a top-level element must be a mapping (`- route: ...`) but found {}", other.type_name()), None);
                return;
            }
        };
        if map.len() != 1 {
            if map.contains_key("from") && !map.contains_key("route") {
                // a route without the `route:` wrapper
                self.route(item, &path);
                return;
            }
            let keys: Vec<&str> = map.keys().map(|k| k.as_str()).collect();
            self.structure(&path, format!("a top-level element must have exactly one key but found [{}]", keys.join(", ")), Some("start each element with `- `".into()));
            return;
        }
        let (name, v) = map.iter().next().unwrap();
        let mut p = path.clone();
        p.push(PathSegment::Key(name.clone()));
        let Some(k) = model::TOP_LEVEL.iter().find(|k| k.name == name) else {
            let hint = if model::step(name).is_some() {
                Some(format!("`{}` is a step: put it in the `steps:` of a route's `from`", name))
            } else {
                model::closest(name, model::TOP_LEVEL.iter().map(|k| k.name)).map(|c| format!("did you mean `{}`?", c))
            };
            let names: Vec<&str> = model::TOP_LEVEL.iter().map(|k| k.name).collect();
            self.structure(&p, format!("unknown top-level element '{}' (expected one of: {})", name, names.join(", ")), hint);
            return;
        };
        match name.as_str() {
            "route" => self.route(v, &p),
            "from" => self.route(item, &path),
            "routeTemplate" => self.route_template(v, &p),
            "routeTemplates" => self.each_item(v, &p, "routeTemplate", |w, x, xp| w.route_template(x, xp)),
            "templatedRoute" => self.templated_route(v, &p),
            "templatedRoutes" => self.each_item(v, &p, "templatedRoute", |w, x, xp| w.templated_route(x, xp)),
            "rest" => {
                self.value(k.kind, v, &p);
                self.rest_routes(v, &p);
            }
            _ => self.value(k.kind, v, &p),
        }
    }

    fn each_item(&mut self, v: &Value, path: &[PathSegment], wrapper: &str, f: impl Fn(&mut Self, &Value, &[PathSegment])) {
        match v {
            Value::Array(items) => {
                for (i, it) in items.iter().enumerate() {
                    let mut ip = path.to_vec();
                    ip.push(PathSegment::Index(i));
                    match it {
                        Value::Object(m) if m.len() == 1 && m.contains_key(wrapper) => {
                            ip.push(PathSegment::Key(wrapper.into()));
                            f(self, &m[wrapper], &ip)
                        }
                        other => f(self, other, &ip),
                    }
                }
            }
            other => f(self, other, path),
        }
    }

    fn route(&mut self, v: &Value, path: &[PathSegment]) {
        self.routes += 1;
        if let Some(id) = v.get("id").or_else(|| v.get("routeId")).and_then(scalar) {
            let key = if v.get("id").is_some() { "id" } else { "routeId" };
            let mut p = path.to_vec();
            p.push(PathSegment::Key(key.into()));
            let (line, column) = self.pos(&p);
            if !id.contains("{{") {
                self.route_ids.push(RouteId { file: self.file, id, line, column });
            }
        }
        self.object(model::model("route").unwrap(), v, path, true);
    }

    fn route_template(&mut self, v: &Value, path: &[PathSegment]) {
        let params: Vec<(String, Option<String>)> = match v.get("parameters") {
            Some(Value::Array(a)) => a.iter().filter_map(|p| Some((p.get("name").and_then(scalar)?, p.get("defaultValue").and_then(scalar)))).collect(),
            _ => Vec::new(),
        };
        let saved = self.template.replace(params);
        self.object(model::model("routeTemplate").unwrap(), v, path, true);
        self.template = saved;
    }

    fn templated_route(&mut self, v: &Value, path: &[PathSegment]) {
        self.routes += 1;
        if let Some(id) = v.get("routeId").and_then(scalar) {
            let mut p = path.to_vec();
            p.push(PathSegment::Key("routeId".into()));
            let (line, column) = self.pos(&p);
            self.route_ids.push(RouteId { file: self.file, id, line, column });
        }
        self.object(model::model("templatedRoute").unwrap(), v, path, true);
    }

    /// REST operations are routes (with their `id`).
    fn rest_routes(&mut self, v: &Value, path: &[PathSegment]) {
        for verb in ["get", "post", "put", "delete", "patch", "head", "options", "verb"] {
            let items: Vec<(Vec<PathSegment>, &Value)> = match v.get(verb) {
                Some(Value::Array(a)) => a
                    .iter()
                    .enumerate()
                    .map(|(i, x)| {
                        let mut p = path.to_vec();
                        p.push(PathSegment::Key(verb.into()));
                        p.push(PathSegment::Index(i));
                        (p, x)
                    })
                    .collect(),
                Some(x @ Value::Object(_)) => {
                    let mut p = path.to_vec();
                    p.push(PathSegment::Key(verb.into()));
                    vec![(p, x)]
                }
                _ => Vec::new(),
            };
            for (p, x) in items {
                self.routes += 1;
                if let Some(id) = x.get("id").or_else(|| x.get("routeId")).and_then(scalar) {
                    let mut kp = p.clone();
                    kp.push(PathSegment::Key(if x.get("id").is_some() { "id" } else { "routeId" }.into()));
                    let (line, column) = self.pos(&kp);
                    self.route_ids.push(RouteId { file: self.file, id, line, column });
                }
            }
        }
    }

    // --- values ---

    fn value(&mut self, kind: Kind, v: &Value, path: &[PathSegment]) {
        match kind {
            Kind::Any | Kind::Map | Kind::Parameters => self.scan(v, path),
            Kind::Scalar | Kind::Bool | Kind::Int | Kind::Number | Kind::Duration | Kind::Uri => match v {
                Value::Array(_) | Value::Object(_) => {
                    // XML attributes and elements may arrive in other shapes
                    if !self.xml {
                        self.structure(path, format!("expected a {} but found {}", kind_name(kind), v.type_name()), None);
                    }
                }
                _ => self.scan(v, path),
            },
            Kind::StringList => match v {
                Value::Object(_) if !self.xml => self.structure(path, format!("expected a list of strings but found {}", v.type_name()), None),
                _ => self.scan(v, path),
            },
            Kind::Steps => self.steps(v, path),
            Kind::Expression => self.expression_value(v, path),
            Kind::Predicate => match v {
                Value::Bool(_) | Value::Null => {}
                Value::String(s) if s.eq_ignore_ascii_case("true") || s.eq_ignore_ascii_case("false") => {}
                _ => self.expression_value(v, path),
            },
            Kind::Endpoint => self.endpoint_value(v, path, Role::Producer),
            Kind::Object(name) => {
                let m = model::model(name).expect("model");
                self.object(m, v, path, false);
            }
            Kind::List(name) => {
                let m = model::model(name).expect("model");
                match v {
                    Value::Array(items) => {
                        for (i, it) in items.iter().enumerate() {
                            let mut ip = path.to_vec();
                            ip.push(PathSegment::Index(i));
                            match it {
                                Value::Object(om) if om.len() == 1 && om.contains_key(name) => {
                                    ip.push(PathSegment::Key(name.into()));
                                    self.object(m, &om[name], &ip, false);
                                }
                                other => self.object(m, other, &ip, false),
                            }
                        }
                    }
                    Value::Object(om) if om.len() == 1 && om.contains_key(name) => {
                        let mut ip = path.to_vec();
                        ip.push(PathSegment::Key(name.into()));
                        self.object(m, &om[name], &ip, false);
                    }
                    Value::Null => {}
                    other => self.object(m, other, path, false),
                }
            }
            Kind::Section(name) => match v {
                Value::Array(_) => self.steps(v, path),
                Value::Null => {}
                _ => {
                    let m = model::model(name).expect("model");
                    self.object(m, v, path, false);
                }
            },
        }
    }

    /// Check a mapping against a model. `route_like`: a route-shaped mapping (from/steps).
    fn object(&mut self, m: &'static Model, v: &Value, path: &[PathSegment], _route_like: bool) {
        let map = match v {
            Value::Object(map) => map,
            Value::Null => {
                self.missing_required(m, None, path);
                return;
            }
            Value::Array(a) if self.xml && a.len() == 1 => return self.object(m, &a[0], path, _route_like),
            scalar_value => {
                match m.shorthand {
                    Shorthand::No => {
                        let hint = if m.required.is_empty() { None } else { Some(format!("use a mapping with {}", required_list(m))) };
                        self.structure(path, format!("'{}' expects a mapping but found {}", m.name, scalar_value.type_name()), hint);
                    }
                    Shorthand::Uri => self.endpoint_value(scalar_value, path, m.role),
                    Shorthand::Expression => self.expression_value(scalar_value, path),
                    Shorthand::Text => self.scan(scalar_value, path),
                }
                if m.name == "kamelet" {
                    if let Some(name) = scalar(scalar_value) {
                        let uri = if name.starts_with("kamelet:") { name } else { format!("kamelet:{}", name) };
                        self.check_uri(&uri, path, Role::Producer);
                    }
                }
                return;
            }
        };
        let langs = model::languages();
        let mut extras: Vec<&str> = Vec::new();
        let saved_suggested = std::mem::take(&mut self.suggested);
        for (k, val) in map {
            let mut kp = path.to_vec();
            kp.push(PathSegment::Key(k.clone()));
            if let Some(key) = model::key(m, k) {
                // the endpoint keys of an endpoint step are checked together below
                if m.role != Role::No && matches!(key.kind, Kind::Uri | Kind::Parameters) {
                    continue; // checked with the endpoint below
                }
                if m.name == "kamelet" && k == "parameters" {
                    self.scan(val, &kp);
                    continue;
                }
                self.value(key.kind, val, &kp);
                continue;
            }
            if m.expression != Expr::No && (langs.contains(&k.as_str()) || k == "expression") {
                continue; // checked with the whole expression below
            }
            if m.open || m.one_extra {
                extras.push(k.as_str());
                if !m.one_extra {
                    self.scan(val, &kp);
                }
                continue;
            }
            if self.xml {
                // XML attributes (namespaces, customId, ...) are not checked
                self.scan(val, &kp);
                continue;
            }
            self.unknown_key(m, k, &kp);
        }
        if m.one_extra {
            match extras.len() {
                0 => self.structure(path, format!("'{}' requires a data format (for example `json: {{}}`)", m.name), None),
                1 => {}
                _ => self.structure(path, format!("'{}' must specify exactly one data format but found [{}]", m.name, extras.join(", ")), None),
            }
        }
        if m.one_of {
            let present: Vec<&str> = map.keys().map(|k| k.as_str()).filter(|k| *k != "id" && model::key(m, k).is_some()).collect();
            if present.len() > 1 {
                self.structure(path, format!("'{}' must contain exactly one of {} but found [{}]", m.name, key_list(m), present.join(", ")), None);
            } else if present.is_empty() && !map.keys().any(|k| model::key(m, k).is_none()) {
                self.structure(path, format!("'{}' must contain one of {}", m.name, key_list(m)), None);
            }
        }
        self.missing_required(m, Some(v), path);
        self.suggested = saved_suggested;
        // inline expression
        if m.expression != Expr::No {
            match cameleer_yaml::find_expression(v) {
                Ok(Some(def)) => {
                    let ep = expression_path(v, path);
                    self.compile(&def, &ep);
                }
                Ok(None) => {
                    let alternative = m.expression_alternatives.iter().any(|a| map.contains_key(*a));
                    let uri_instead = matches!(m.name, "enrich" | "pollEnrich") && map.contains_key("uri");
                    if m.expression == Expr::Required && !alternative && !uri_instead {
                        let mut hint = format!("add an expression, for example `simple: \"${{body}}\"`");
                        if !m.expression_alternatives.is_empty() {
                            hint = format!("{} or `{}`", hint, m.expression_alternatives.join("` / `"));
                        }
                        self.structure(path, format!("'{}' requires an expression", m.name), Some(hint));
                    } else if matches!(m.name, "enrich" | "pollEnrich") && !uri_instead {
                        self.structure(path, format!("'{}' requires 'uri' or an expression", m.name), None);
                    }
                }
                Err(e) => self.structure(path, e.message.clone(), None),
            }
        }
        // endpoint of an endpoint step / from
        if m.role != Role::No {
            if map.contains_key("uri") {
                self.endpoint_value(v, path, m.role);
            }
        }
        if m.name == "kamelet" {
            if let Some(name) = map.get("name").and_then(scalar) {
                let base = if name.starts_with("kamelet:") { name } else { format!("kamelet:{}", name) };
                if let Ok(uri) = cameleer_yaml::build_uri(&base, map.get("parameters")) {
                    self.check_uri(&uri, path, Role::Producer);
                }
            }
        }
    }

    fn missing_required(&mut self, m: &Model, v: Option<&Value>, path: &[PathSegment]) {
        for alts in m.required {
            let present = v.map(|v| alts.iter().any(|a| v.get(a).is_some())).unwrap_or(false);
            if !present {
                if v.is_some() && alts.iter().any(|a| self.suggested.contains(a)) {
                    continue; // a misspelling of it is reported already
                }
                let what = if alts.len() == 1 { format!("'{}'", alts[0]) } else { format!("'{}' (or '{}')", alts[0], alts[1..].join("', '")) };
                self.structure(path, format!("'{}' is missing the required key {}", m.name, what), None);
            }
        }
        if v.is_none() && m.expression == Expr::Required {
            self.structure(path, format!("'{}' requires an expression", m.name), Some("add an expression, for example `simple: \"${body}\"`".into()));
        }
        if v.is_none() && m.one_extra {
            self.structure(path, format!("'{}' requires a data format (for example `json: {{}}`)", m.name), None);
        }
    }

    fn unknown_key(&mut self, m: &'static Model, k: &str, path: &[PathSegment]) {
        let mut candidates: Vec<&'static str> = model::key_names(m);
        if m.expression != Expr::No {
            candidates.extend(model::languages().iter().copied());
            candidates.push("expression");
        }
        let hint = if model::step(k).is_some() && model::key(m, "steps").is_some() {
            Some(format!("`{}` is a step: put it in the `steps:` list", k))
        } else if let Some(c) = model::closest(k, candidates.iter().copied()) {
            self.suggested.push(c);
            Some(format!("did you mean `{}`?", c))
        } else {
            let mut names = model::key_names(m);
            names.sort();
            names.dedup();
            if names.is_empty() {
                Some(format!("'{}' takes no options", m.name))
            } else {
                Some(format!("valid keys: {}", names.join(", ")))
            }
        };
        let what = if m.step { "step" } else { "element" };
        self.structure(path, format!("unknown key '{}' in {} '{}'", k, what, m.name), hint);
    }

    fn steps(&mut self, v: &Value, path: &[PathSegment]) {
        let items = match v {
            Value::Null => return,
            Value::Array(a) => a,
            other => {
                self.structure(path, format!("expected a list of steps but found {}", other.type_name()), Some("steps are a YAML list: `- to: ...`".into()));
                return;
            }
        };
        for (i, item) in items.iter().enumerate() {
            let mut ip = path.to_vec();
            ip.push(PathSegment::Index(i));
            let (name, cfg): (&str, &Value) = match item {
                Value::Object(m) if m.len() == 1 => {
                    let (k, v) = m.iter().next().unwrap();
                    (k.as_str(), v)
                }
                Value::Object(m) => {
                    let keys: Vec<&str> = m.keys().map(|k| k.as_str()).collect();
                    let hint = if keys.iter().filter(|k| model::step(k).is_some()).count() == 1 {
                        Some("the options of a step are indented below its name".to_string())
                    } else {
                        Some("each step is its own list item (`- `)".to_string())
                    };
                    self.structure(&ip, format!("a step must have exactly one key (the step name) but found [{}]", keys.join(", ")), hint);
                    continue;
                }
                Value::String(s) => (s.as_str(), &Value::Null),
                other => {
                    self.structure(&ip, format!("a step must be a mapping but found {}", other.type_name()), None);
                    continue;
                }
            };
            let mut sp = ip.clone();
            if matches!(item, Value::Object(_)) {
                sp.push(PathSegment::Key(name.to_string()));
            }
            let Some(m) = model::step(name) else {
                let hint = match model::nested_only_parent(name) {
                    Some(parent) if parent.starts_with("a ") => Some(format!("`{}` is only valid as {}", name, parent)),
                    Some(parent) => Some(format!("`{}` is only valid inside `{}`", name, parent)),
                    None => model::closest(name, model::STEPS.iter().map(|m| m.name)).map(|c| format!("did you mean `{}`?", c)),
                };
                self.structure(&sp, format!("unknown step '{}'", name), hint);
                continue;
            };
            if is_disabled(cfg) {
                continue;
            }
            self.object(m, cfg, &sp, false);
        }
    }

    // --- expressions ---

    fn expression_value(&mut self, v: &Value, path: &[PathSegment]) {
        let v = match v {
            Value::Array(a) if a.len() == 1 => &a[0],
            other => other,
        };
        match v {
            Value::Null => {}
            _ => match cameleer_yaml::parse_expression(cameleer_yaml_unwrap(v)) {
                Ok(def) => {
                    let ep = expression_path(v, path);
                    self.compile(&def, &ep);
                }
                Err(e) => self.structure(path, e.message.clone(), None),
            },
        }
    }

    fn compile(&mut self, def: &ExpressionDefinition, path: &[PathSegment]) {
        let ExpressionDefinition::Language { language, expression, options } = def else { return };
        if NOT_COMPILED.contains(&language.as_str()) {
            return;
        }
        let sub = self.substitute(expression);
        self.report_unresolved(&sub, path);
        if !sub.unresolved.is_empty() {
            return;
        }
        let lang = match self.ctx.resolve_language(language) {
            Ok(l) => l,
            Err(e) => {
                let names = self.ctx.language_names();
                let hint = model::closest(language, names.iter().map(|s| s.as_str())).map(|c| format!("did you mean `{}`?", c));
                self.error(codes::EXPRESSION, path, e.message.clone(), hint);
                return;
            }
        };
        let mut opts = BTreeMap::new();
        for (k, v) in options {
            let s = self.substitute(v);
            self.report_unresolved(&s, path);
            if !s.unresolved.is_empty() {
                return;
            }
            opts.insert(k.clone(), s.text);
        }
        if let Err(e) = lang.create_expression(&sub.text, &opts) {
            self.error(codes::EXPRESSION, path, format!("invalid {} expression: {}", language, e.message), None);
        }
    }

    // --- endpoints ---

    /// An endpoint given as a URI string or `{uri, parameters}`.
    fn endpoint_value(&mut self, v: &Value, path: &[PathSegment], role: Role) {
        let uri = match v {
            Value::String(s) => s.clone(),
            Value::Object(m) => {
                let Some(u) = m.get("uri").and_then(scalar) else {
                    if !self.xml || m.contains_key("uri") {
                        self.structure(path, "an endpoint requires 'uri'".into(), None);
                    }
                    return;
                };
                if let Some(p) = m.get("parameters") {
                    if !matches!(p, Value::Object(_) | Value::Null) {
                        let mut pp = path.to_vec();
                        pp.push(PathSegment::Key("parameters".into()));
                        self.structure(&pp, format!("'parameters' must be a mapping but found {}", p.type_name()), None);
                        return;
                    }
                }
                for k in ["uri", "parameters"] {
                    if let Some(x) = m.get(k) {
                        let mut kp = path.to_vec();
                        kp.push(PathSegment::Key(k.into()));
                        self.scan(x, &kp);
                    }
                }
                match cameleer_yaml::build_uri(&u, m.get("parameters")) {
                    Ok(u) => u,
                    Err(e) => {
                        self.structure(path, e.message.clone(), None);
                        return;
                    }
                }
            }
            Value::Null => {
                self.structure(path, "an endpoint requires a URI".into(), None);
                return;
            }
            other => match scalar(other) {
                Some(s) => s,
                None => return,
            },
        };
        let mut up = path.to_vec();
        let mapping = matches!(v, Value::Object(_));
        if mapping {
            up.push(PathSegment::Key("uri".into()));
        }
        self.check_uri_with(&uri, &up, role, !mapping);
    }

    fn check_uri(&mut self, raw: &str, path: &[PathSegment], role: Role) {
        self.check_uri_with(raw, path, role, true)
    }

    /// Check an endpoint URI; `report`: report its unresolved placeholders (not done when the
    /// parts of a `{uri, parameters}` mapping were scanned already).
    fn check_uri_with(&mut self, raw: &str, path: &[PathSegment], role: Role, report: bool) {
        let dynamic = matches!(role, Role::Dynamic | Role::Poll) && (raw.contains("${") || raw.contains("$simple{"));
        let sub = self.substitute(raw);
        if report {
            self.report_unresolved(&sub, path);
        }
        let uri = sub.text.trim().to_string();
        if dynamic || !sub.unresolved.is_empty() {
            // only the scheme can be checked
            let scheme = uri.split(':').next().unwrap_or("");
            if uri.contains(':') && !scheme.is_empty() && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)) {
                self.check_scheme(scheme, path);
            }
            return;
        }
        if !uri.contains(':') {
            self.error(codes::ENDPOINT, path, format!("'{}' is not an endpoint URI (scheme:path?options)", uri), None);
            return;
        }
        let mut result = self.ctx.check_endpoint(&uri);
        if result.is_err() && sub.empty_default {
            // `{{env:SECRET:}}`: a value is required at runtime; check the option names with a
            // stand-in value
            let filled = self.substitute_with(raw, "placeholder");
            if self.ctx.check_endpoint(filled.text.trim()).is_ok() {
                result = Ok(());
            }
        }
        if let Err(e) = result {
            // beans are bound at runtime (code, `beans:`, `camel.beans.*`, `camel.datasource.*`)
            if caused_by(&e, cameleer_core::error::kinds::NO_SUCH_BEAN) {
                return;
            }
            let scheme = uri.split(':').next().unwrap_or("").to_string();
            let hint = if self.ctx.get_component(&scheme).is_none() {
                let names = self.ctx.component_names();
                model::closest(&scheme, names.iter().map(|s| s.as_str())).map(|c| format!("did you mean `{}`?", c))
            } else if e.message.contains("Unknown parameters=") {
                Some(format!("see the options of the {} component: docs/components/{}.md", scheme, scheme))
            } else {
                None
            };
            self.error(codes::ENDPOINT, path, e.message.clone(), hint);
            return;
        }
        // direct / seda / vm channels for the dangling check
        let scheme = uri.split(':').next().unwrap_or("");
        if matches!(scheme, "direct" | "seda" | "vm") {
            let rest = &uri[scheme.len() + 1..];
            let rest = rest.trim_start_matches("//");
            let name = rest.split('?').next().unwrap_or("");
            let (line, column) = self.pos(path);
            self.channels.push(Channel {
                file: self.file,
                key: format!("{}:{}", scheme, name),
                uri: uri.clone(),
                consumer: matches!(role, Role::Consumer | Role::Poll),
                line,
                column,
            });
        }
    }

    fn check_scheme(&mut self, scheme: &str, path: &[PathSegment]) {
        if self.ctx.get_component(scheme).is_none() {
            let names = self.ctx.component_names();
            let hint = model::closest(scheme, names.iter().map(|s| s.as_str())).map(|c| format!("did you mean `{}`?", c));
            self.error(codes::ENDPOINT, path, format!("No component found with scheme: {}", scheme), hint);
        }
    }

    // --- placeholders ---

    /// Report unresolved placeholders of every string below `v`.
    fn scan(&mut self, v: &Value, path: &[PathSegment]) {
        match v {
            Value::String(s) if s.contains("{{") => {
                let sub = self.substitute(s);
                self.report_unresolved(&sub, path);
            }
            Value::Array(a) => {
                for (i, x) in a.iter().enumerate() {
                    let mut p = path.to_vec();
                    p.push(PathSegment::Index(i));
                    self.scan(x, &p);
                }
            }
            Value::Object(m) => {
                for (k, x) in m {
                    let mut p = path.to_vec();
                    p.push(PathSegment::Key(k.clone()));
                    self.scan(x, &p);
                }
            }
            _ => {}
        }
    }

    fn lookup(&self, name: &str) -> Option<String> {
        self.properties.get(name).or_else(|| self.extra_properties.get(name)).cloned()
    }

    /// Substitute `{{name}}`, `{{name:default}}`, `{{env:NAME:default}}`, `{{?name}}` and
    /// template parameters.
    fn substitute(&self, s: &str) -> Substituted {
        self.substitute_with(s, "")
    }

    /// [`Self::substitute`]; an empty default is replaced by `empty_as`.
    fn substitute_with(&self, s: &str, empty_as: &str) -> Substituted {
        let mut out = String::with_capacity(s.len());
        let mut unresolved = Vec::new();
        let mut empty_default = false;
        let mut rest = s;
        while let Some(start) = rest.find("{{") {
            let Some(len) = rest[start + 2..].find("}}") else { break };
            let inner = rest[start + 2..start + 2 + len].trim();
            out.push_str(&rest[..start]);
            let whole = &rest[start..start + 4 + len];
            rest = &rest[start + 4 + len..];
            let (optional, inner) = match inner.strip_prefix('?') {
                Some(i) => (true, i),
                None => (false, inner),
            };
            // template parameters
            if let Some(params) = &self.template {
                let name = inner.split(':').next().unwrap_or(inner).trim();
                if let Some((_, default)) = params.iter().find(|(n, _)| n == name) {
                    match default.clone().or_else(|| inner.split_once(':').map(|(_, d)| d.to_string())) {
                        Some(d) => out.push_str(&d),
                        None => {
                            out.push_str(whole);
                            unresolved.push((whole.to_string(), true));
                        }
                    }
                    continue;
                }
            }
            let (name, default) = match inner.split_once(':') {
                Some((p, r)) if matches!(p, "env" | "sys" | "sysenv") => match r.split_once(':') {
                    Some((n, d)) => (n.trim(), Some(d)),
                    None => (r.trim(), None),
                },
                Some((n, d)) => (n.trim(), Some(d)),
                None => (inner, None),
            };
            match (self.lookup(name), default) {
                (Some(v), _) => out.push_str(&v),
                (None, Some("")) => {
                    empty_default = true;
                    out.push_str(empty_as);
                }
                (None, Some(d)) => out.push_str(d),
                (None, None) if optional => {}
                (None, None) => {
                    out.push_str(whole);
                    unresolved.push((whole.to_string(), false));
                }
            }
        }
        out.push_str(rest);
        Substituted { text: out, unresolved, empty_default }
    }

    fn report_unresolved(&mut self, sub: &Substituted, path: &[PathSegment]) {
        for (p, template) in &sub.unresolved {
            if *template {
                continue;
            }
            let name = p.trim_start_matches("{{").trim_end_matches("}}").trim();
            let key = name.split_once(':').map(|(pre, n)| if matches!(pre, "env" | "sys" | "sysenv") { n } else { name }).unwrap_or(name);
            self.push(
                Severity::Warning,
                codes::UNRESOLVED_PLACEHOLDER,
                path,
                format!("property placeholder {} has no default and no value in the given properties", p),
                Some(format!("give it a default (`{{{{{}:value}}}}`) or pass --prop {}=VALUE / --properties FILE", name, key)),
            );
        }
    }
}

fn cameleer_yaml_unwrap(v: &Value) -> &Value {
    // `correlationExpression: {expression: {simple: ...}}`
    match v {
        Value::Object(m) if m.len() == 1 && matches!(m.get("expression"), Some(Value::Object(_))) => &m["expression"],
        other => other,
    }
}

/// The path of the language key of an expression (for positions).
fn expression_path(v: &Value, path: &[PathSegment]) -> Vec<PathSegment> {
    let mut p = path.to_vec();
    if let Value::Object(m) = v {
        if let Some(k) = m.keys().find(|k| model::languages().contains(&k.as_str()) || k.as_str() == "expression") {
            p.push(PathSegment::Key(k.clone()));
        }
    }
    p
}

/// Whether an error or one of its causes has the given kind.
fn caused_by(e: &cameleer_core::CamelError, kind: &str) -> bool {
    let mut cur = Some(e);
    while let Some(x) = cur {
        if x.kind == kind || x.message.contains("No bean could be found in the registry") {
            return true;
        }
        cur = x.cause.as_deref();
    }
    false
}

/// A message on one line (some parsers point at the error on a second line).
fn one_line(s: &str) -> String {
    s.split('\n').map(|l| l.trim_end()).filter(|l| !l.trim().is_empty()).collect::<Vec<_>>().join(" ")
}

fn is_disabled(cfg: &Value) -> bool {
    match cfg.get("disabled") {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::Null | Value::Array(_) | Value::Object(_) => None,
        other => Some(other.to_string()),
    }
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Bool => "boolean",
        Kind::Int => "integer",
        Kind::Number => "number",
        Kind::Duration => "duration",
        Kind::Uri => "URI",
        _ => "scalar",
    }
}

fn required_list(m: &Model) -> String {
    m.required.iter().map(|alts| format!("`{}`", alts.join("` or `"))).collect::<Vec<_>>().join(", ")
}

fn key_list(m: &Model) -> String {
    m.keys.iter().filter(|k: &&Key| k.name != "id").map(|k| k.name).collect::<Vec<_>>().join(", ")
}

//! Tests for the language, data format and template/XML component pages:
//! `docs/languages/*.md`, `docs/dataformats/*.md` and the template and XML component pages in
//! `docs/components/`.
//!
//! The pages carry their own test data in HTML comments, so what a reader sees is what runs:
//!
//! - `<!-- example: id -->` above a ```yaml block: a route. It is loaded with
//!   `cameleer_main::new_main()`, every endpoint URI in it is resolved with `get_endpoint`
//!   (unknown options fail), and the route is started.
//! - `<!-- input: id -->` / `<!-- output: id -->` above a fenced block: the body sent to the
//!   route's `direct:` endpoint and the expected result body. Further cases of one route use
//!   `id#2`, `id#3`... An optional JSON object after the id sets `headers`, `send` (another
//!   endpoint) or `contains` (output is a substring check instead of an exact match).
//! - `<!-- file: path -->` above a fenced block: a file (template, schema, key...) written to the
//!   working directory of the tests before any route runs.
//! - `<!-- fixture: name -->` above a ```json block: a message (`body`, `headers`, `properties`,
//!   `variables`) used by eval tables.
//! - `<!-- eval: id fixture=name [language=simple] [predicate] -->` above a markdown table:
//!   column 1 is an expression in backticks, column 2 its expected result in backticks.
//!
//! Routes whose result is binary or random are checked by the custom assertions in `custom`.
//! Rust DSL snippets in the pages (```rust blocks) must appear verbatim in this file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Once;

use cameleer_core::prelude::*;

const COMPONENT_PAGES: &[&str] = &[
    "freemarker",
    "mustache",
    "velocity",
    "string-template",
    "mvel",
    "xslt",
    "xslt-saxon",
    "xquery",
    "xj",
    "xmlsecurity-sign",
    "xmlsecurity-verify",
    "validator",
    "json-validator",
];

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The pages owned by this test.
fn pages() -> Vec<PathBuf> {
    let mut v = Vec::new();
    for dir in ["docs/languages", "docs/dataformats"] {
        if let Ok(rd) = std::fs::read_dir(repo().join(dir)) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().map(|x| x == "md").unwrap_or(false) && p.file_name().map(|n| n != "README.md").unwrap_or(false) {
                    v.push(p);
                }
            }
        }
    }
    for c in COMPONENT_PAGES {
        let p = repo().join("docs/components").join(format!("{}.md", c));
        if p.exists() {
            v.push(p);
        }
    }
    v.sort();
    v
}

#[derive(Clone, Debug)]
struct Marked {
    page: String,
    kind: String,
    args: String,
    lang: String,
    content: String,
    /// table rows (cells) for eval tables
    rows: Vec<Vec<String>>,
}

fn split_cells(line: &str) -> Vec<String> {
    let t = line.trim().trim_start_matches('|');
    let t = t.strip_suffix('|').unwrap_or(t);
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek() == Some(&'|') {
            cur.push('|');
            chars.next();
        } else if c == '|' {
            cells.push(cur.trim().to_string());
            cur.clear();
        } else {
            cur.push(c);
        }
    }
    cells.push(cur.trim().to_string());
    cells
}

/// The content of the first code span of a table cell (`(empty)` means the empty string).
fn code_span(cell: &str) -> Option<String> {
    if cell.trim() == "(empty)" {
        return Some(String::new());
    }
    let (ticks, rest) = if let Some(r) = cell.split_once("`` ") {
        ("`` ", r.1)
    } else {
        ("`", cell.split_once('`')?.1)
    };
    let end = if ticks == "`` " { rest.find(" ``")? } else { rest.find('`')? };
    Some(rest[..end].to_string())
}

fn parse_page(path: &Path) -> Vec<Marked> {
    let text = std::fs::read_to_string(path).unwrap().replace("\r\n", "\n");
    let page = path.file_name().unwrap().to_string_lossy().to_string();
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i].trim();
        let fence_start = l.starts_with("```");
        if fence_start {
            // a fenced block without a marker: rust blocks are collected, others skipped
            let ticks = l.chars().take_while(|c| *c == '`').count();
            let lang = l[ticks..].trim().to_string();
            let mut j = i + 1;
            let mut content = String::new();
            while j < lines.len() && !(lines[j].trim_start().starts_with(&"`".repeat(ticks)) && lines[j].trim().chars().all(|c| c == '`')) {
                content.push_str(lines[j]);
                content.push('\n');
                j += 1;
            }
            if lang == "rust" {
                out.push(Marked { page: page.clone(), kind: "rust".into(), args: String::new(), lang: lang.clone(), content, rows: vec![] });
            }
            if lang == "yaml" {
                let marked = i > 0 && lines[i - 1].trim().starts_with("<!-- example:");
                assert!(marked, "{}: ```yaml block at line {} has no <!-- example: id --> marker", page, i + 1);
            }
            i = j + 1;
            continue;
        }
        if let Some(m) = l.strip_prefix("<!--").and_then(|r| r.strip_suffix("-->")) {
            let m = m.trim();
            if let Some((kind, args)) = m.split_once(':') {
                let kind = kind.trim().to_string();
                if matches!(kind.as_str(), "example" | "input" | "output" | "file" | "fixture" | "eval") {
                    let next = lines.get(i + 1).map(|s| s.trim()).unwrap_or("");
                    let mut item = Marked { page: page.clone(), kind, args: args.trim().to_string(), lang: String::new(), content: String::new(), rows: vec![] };
                    if next.starts_with("```") {
                        let ticks = next.chars().take_while(|c| *c == '`').count();
                        item.lang = next[ticks..].trim().to_string();
                        let mut j = i + 2;
                        while j < lines.len() && !(lines[j].trim_start().starts_with(&"`".repeat(ticks)) && lines[j].trim().chars().all(|c| c == '`')) {
                            item.content.push_str(lines[j]);
                            item.content.push('\n');
                            j += 1;
                        }
                        assert!(j < lines.len(), "{}: unclosed fence after marker {}", page, m);
                        i = j + 1;
                    } else if next.starts_with('|') {
                        let mut j = i + 1;
                        while j < lines.len() && lines[j].trim().starts_with('|') {
                            let cells = split_cells(lines[j]);
                            if !cells.iter().all(|c| c.chars().all(|ch| ch == '-' || ch == ':' || ch == ' ') && !c.is_empty()) {
                                item.rows.push(cells);
                            }
                            j += 1;
                        }
                        item.rows.remove(0); // header row
                        i = j;
                    } else {
                        panic!("{}: marker `{}` is not followed by a fenced block or a table", page, m);
                    }
                    out.push(item);
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

fn all_marked() -> Vec<Marked> {
    pages().iter().flat_map(|p| parse_page(p)).collect()
}

/// `DOCS_FILTER=a,b` limits the example and eval tests to pages or ids containing `a` or `b`
/// (handy while writing a page).
fn selected(page: &str, id: &str) -> bool {
    let filter = std::env::var("DOCS_FILTER").unwrap_or_default();
    filter.is_empty() || filter.split(',').filter(|f| !f.is_empty()).any(|f| page.contains(f) || id.contains(f))
}

fn work_dir() -> PathBuf {
    std::env::temp_dir().join(format!("cameleer-docs-lf-{}", std::process::id()))
}

static SETUP: Once = Once::new();

/// Write every `<!-- file: -->` block into the work directory and make it the current
/// directory (once per test process).
fn setup() {
    SETUP.call_once(|| {
        let dir = work_dir();
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for m in all_marked().into_iter().filter(|m| m.kind == "file") {
            let p = dir.join(&m.args);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            let content = if m.lang == "base64" {
                cameleer_core::dataformat::base64::decode(&m.content.split_whitespace().collect::<String>()).unwrap()
            } else {
                m.content.clone().into_bytes()
            };
            std::fs::write(&p, content).unwrap();
        }
        std::env::set_current_dir(&dir).unwrap();
    });
}

fn json(text: &str) -> Value {
    cameleer_core::json::parse(text).unwrap_or_else(|e| panic!("bad JSON {}: {}", text, e))
}

/// Endpoint URIs of a YAML route: every `uri:` (with its `parameters:`) and `to:` value that is
/// not dynamic (`toD` and URIs with `${...}` are skipped).
fn endpoint_uris(yaml: &str) -> Vec<String> {
    fn walk(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Object(o) => {
                if o.get("uri").map(|u| matches!(u, Value::String(_))).unwrap_or(false) {
                    out.push(cameleer_yaml::dsl::endpoint_uri(v).unwrap());
                }
                for (k, child) in o.iter() {
                    if k == "toD" {
                        continue;
                    }
                    if k == "to" {
                        if let Value::String(s) = child {
                            out.push(s.clone());
                        }
                    }
                    walk(child, out);
                }
            }
            Value::Array(a) => a.iter().for_each(|c| walk(c, out)),
            _ => {}
        }
    }
    let doc = cameleer_yaml::parse_yaml(yaml).unwrap_or_else(|e| panic!("YAML does not parse: {}\n{}", e, yaml));
    let mut out = Vec::new();
    walk(&doc, &mut out);
    out.retain(|u| !u.contains("${"));
    out
}

fn from_direct(yaml: &str) -> Option<String> {
    endpoint_uris(yaml).into_iter().find(|u| u.starts_with("direct:"))
}

fn start(id: &str, yaml: &str, configure: Option<fn(&CamelContext)>) -> cameleer_main::Main {
    setup();
    let mut main = cameleer_main::new_main();
    main.add_route_source(format!("{}.camel.yaml", id), yaml.to_string());
    if let Some(f) = configure {
        main.configure(move |ctx| {
            f(ctx);
            Ok(())
        });
    }
    main.start().unwrap_or_else(|e| panic!("example {}: route does not start: {}\n{}", id, e, yaml));
    for uri in endpoint_uris(yaml) {
        main.context().get_endpoint(&uri).unwrap_or_else(|e| panic!("example {}: endpoint {} does not resolve: {}", id, uri, e));
    }
    main
}

fn norm(s: &str) -> String {
    s.replace("\r\n", "\n").trim().to_string()
}

/// Run one example: every input/output case, then its custom assertion (if any).
fn run_example(ex: &Marked, cases: &[(String, Option<Marked>, Option<Marked>)]) {
    let id = ex.args.as_str();
    let main = start(id, &ex.content, configure_for(id));
    let ctx = main.context().clone();
    let t = ctx.create_producer_template();
    let mut asserted = false;
    for (case, input, output) in cases {
        let opts = input.as_ref().map(|i| i.args.split_once(' ').map(|(_, j)| json(j)).unwrap_or(Value::Null)).unwrap_or(Value::Null);
        let send = opts.get("send").map(|v| v.to_string()).or_else(|| from_direct(&ex.content)).unwrap_or_else(|| panic!("{}: no direct: endpoint to send to", case));
        let body = input.as_ref().map(|i| i.content.trim_end_matches('\n').to_string()).unwrap_or_default();
        let headers: Vec<(String, Value)> = opts.get("headers").and_then(|h| h.as_object()).map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
        let result = t.request(&send, |e: &mut Exchange| {
            e.set_body(body.clone());
            for (k, v) in &headers {
                e.set_header(k.clone(), v.clone());
            }
        });
        let result = result.unwrap_or_else(|e| panic!("{}: request failed: {}", case, e));
        if let Some(err) = &result.exception {
            if output.as_ref().map(|o| o.args.contains("error")).unwrap_or(false) {
                let want = norm(&output.as_ref().unwrap().content);
                assert!(err.to_string().contains(&want), "{}: error\n{}\ndoes not contain\n{}", case, err, want);
                asserted = true;
                continue;
            }
            panic!("{}: route failed: {}", case, err);
        }
        if let Some(out) = output {
            let got = norm(&result.body().to_string());
            let want = norm(&out.content);
            let oopts = out.args.split_once(' ').map(|(_, j)| json(j)).unwrap_or(Value::Null);
            if oopts.get("contains").map(|v| v.is_truthy()).unwrap_or(false) {
                for line in want.lines() {
                    assert!(got.contains(line.trim()), "{}: output\n{}\ndoes not contain\n{}", case, got, line);
                }
            } else {
                assert_eq!(got, want, "{}: unexpected output", case);
            }
            asserted = true;
        }
        if let Some(check) = custom(case) {
            check(&ctx, &result);
            asserted = true;
        }
    }
    if cases.is_empty() {
        if let Some(check) = custom(id) {
            check(&ctx, &Exchange::new(Some(ctx.clone())));
            asserted = true;
        }
    }
    assert!(asserted, "example {} has no output block and no custom assertion", id);
    main.stop().unwrap();
}

fn fixture_exchange(ctx: &CamelContext, fixture: &Value) -> Exchange {
    let mut ex = Exchange::new(Some(ctx.clone()));
    if let Some(b) = fixture.get("body") {
        ex.set_body(b.clone());
    }
    if let Some(b) = fixture.get("bodyText") {
        ex.set_body(b.to_string());
    }
    if let Some(h) = fixture.get("headers").and_then(|h| h.as_object()) {
        for (k, v) in h.iter() {
            ex.set_header(k.clone(), v.clone());
        }
    }
    if let Some(h) = fixture.get("properties").and_then(|h| h.as_object()) {
        for (k, v) in h.iter() {
            ex.set_property(k.clone(), v.clone());
        }
    }
    if let Some(h) = fixture.get("variables").and_then(|h| h.as_object()) {
        for (k, v) in h.iter() {
            ex.set_property(format!("CamelVariable.{}", k), v.clone());
        }
    }
    ex
}

/// Evaluate every row of every eval table; returns the number of rows checked.
fn run_eval_tables(marked: &[Marked]) -> usize {
    setup();
    let mut main = cameleer_main::new_main();
    main.set_property("app.greeting", "Hi");
    main.start().unwrap();
    let ctx = main.context().clone();
    let fixtures: BTreeMap<String, Value> = marked.iter().filter(|m| m.kind == "fixture").map(|m| (m.args.clone(), json(&m.content))).collect();
    let mut n = 0;
    let mut failures = Vec::new();
    for t in marked.iter().filter(|m| m.kind == "eval" && selected(&m.page, &m.args)) {
        let mut words = t.args.split_whitespace();
        let id = words.next().unwrap().to_string();
        let mut fixture = Value::Null;
        let mut language = "simple".to_string();
        let mut predicate = false;
        for w in words {
            if let Some(f) = w.strip_prefix("fixture=") {
                fixture = fixtures.get(f).cloned().unwrap_or_else(|| panic!("{}: unknown fixture {}", id, f));
            } else if let Some(l) = w.strip_prefix("language=") {
                language = l.to_string();
            } else if w == "predicate" {
                predicate = true;
            }
        }
        let lang = ctx.resolve_language(&language).unwrap();
        for row in &t.rows {
            let expr = code_span(&row[0]).unwrap_or_else(|| panic!("{} ({}): no expression in {:?}", id, t.page, row));
            let want = code_span(row.get(1).map(|s| s.as_str()).unwrap_or("")).unwrap_or_else(|| panic!("{} ({}): no result in {:?}", id, t.page, row));
            let ex = fixture_exchange(&ctx, &fixture);
            let got = if predicate {
                lang.create_predicate(&expr, &BTreeMap::new()).and_then(|p| p.matches(&ex)).map(|b| b.to_string())
            } else {
                lang.create_expression(&expr, &BTreeMap::new()).and_then(|e| e.evaluate(&ex)).map(|v| v.to_string())
            };
            match got {
                Ok(g) if norm(&g) == norm(&want) => {}
                Ok(g) => failures.push(format!("{} ({}): {}\n   expected: {}\n   got:      {}", id, t.page, expr, want, g)),
                Err(e) if want.starts_with("error:") && e.to_string().contains(want["error:".len()..].trim()) => {}
                Err(e) => failures.push(format!("{} ({}): {}\n   expected: {}\n   error:    {}", id, t.page, expr, want, e)),
            }
            n += 1;
        }
    }
    main.stop().unwrap();
    assert!(failures.is_empty(), "{} eval row(s) failed:\n{}", failures.len(), failures.join("\n"));
    n
}

/// Extra registry setup some examples need (beans a route refers to).
fn configure_for(id: &str) -> Option<fn(&CamelContext)> {
    configure_languages(id).or_else(|| configure_formats_a(id)).or_else(|| configure_formats_b(id)).or_else(|| configure_components(id))
}

type Check = fn(&CamelContext, &Exchange);

/// Assertions for results that are binary, random or otherwise not text-comparable.
/// `case` is the example id or `id#n`; the exchange is the reply of that case.
fn custom(case: &str) -> Option<Check> {
    custom_languages(case).or_else(|| custom_formats_a(case)).or_else(|| custom_formats_b(case)).or_else(|| custom_components(case))
}

// --- languages (docs/languages) ---

fn configure_languages(id: &str) -> Option<fn(&CamelContext)> {
    match id {
        "__none__" => Some(|_| {}),
        _ => None,
    }
}

fn custom_languages(case: &str) -> Option<Check> {
    match case {
        "__none__" => Some(|_, _| {}),
        _ => None,
    }
}

// --- data formats A (docs/dataformats: core formats, json/csv/xml/yaml/avro/protobuf/bindy...) ---

fn configure_formats_a(id: &str) -> Option<fn(&CamelContext)> {
    match id {
        "__none__" => Some(|_| {}),
        _ => None,
    }
}

fn custom_formats_a(case: &str) -> Option<Check> {
    match case {
        "__none__" => Some(|_, _| {}),
        _ => None,
    }
}

// --- data formats B (docs/dataformats: tar/crypto/pgp/thrift/asn1/fory/barcode/ical/mime/...) ---

fn configure_formats_b(id: &str) -> Option<fn(&CamelContext)> {
    match id {
        "__none__" => Some(|_| {}),
        _ => None,
    }
}

fn custom_formats_b(case: &str) -> Option<Check> {
    match case {
        "__none__" => Some(|_, _| {}),
        _ => None,
    }
}

// --- template and XML components (docs/components) ---

fn configure_components(id: &str) -> Option<fn(&CamelContext)> {
    match id {
        "__none__" => Some(|_| {}),
        _ => None,
    }
}

fn custom_components(case: &str) -> Option<Check> {
    match case {
        "__none__" => Some(|_, _| {}),
        _ => None,
    }
}

fn examples_and_cases(marked: &[Marked]) -> Vec<(Marked, Vec<(String, Option<Marked>, Option<Marked>)>)> {
    let mut ids = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for ex in marked.iter().filter(|m| m.kind == "example") {
        assert!(ids.insert(ex.args.clone()), "duplicate example id {}", ex.args);
        assert_eq!(ex.lang, "yaml", "example {} is not a yaml block", ex.args);
        let case_id = |m: &Marked| m.args.split_whitespace().next().unwrap_or("").to_string();
        let mut cases: BTreeMap<String, (Option<Marked>, Option<Marked>)> = BTreeMap::new();
        for m in marked.iter().filter(|m| m.kind == "input" || m.kind == "output") {
            let c = case_id(m);
            if c == ex.args || c.starts_with(&format!("{}#", ex.args)) {
                let e = cases.entry(c).or_default();
                if m.kind == "input" {
                    e.0 = Some(m.clone());
                } else {
                    e.1 = Some(m.clone());
                }
            }
        }
        out.push((ex.clone(), cases.into_iter().map(|(k, (i, o))| (k, i, o)).collect()));
    }
    out
}

#[test]
fn doc_examples_run() {
    let marked = all_marked();
    let mut n = 0;
    let mut failed = Vec::new();
    for (ex, cases) in examples_and_cases(&marked) {
        if !selected(&ex.page, &ex.args) {
            continue;
        }
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_example(&ex, &cases))).is_err() {
            failed.push(format!("{} ({})", ex.args, ex.page));
        }
        n += 1;
    }
    eprintln!("ran {} documentation examples", n);
    assert!(failed.is_empty(), "{} example(s) failed (see the panic messages above): {}", failed.len(), failed.join(", "));
}

#[test]
fn doc_eval_tables() {
    let marked = all_marked();
    let n = run_eval_tables(&marked);
    eprintln!("checked {} eval table rows", n);
}

#[test]
fn doc_inputs_belong_to_examples() {
    let marked = all_marked();
    let ids: std::collections::BTreeSet<String> = marked.iter().filter(|m| m.kind == "example").map(|m| m.args.clone()).collect();
    for m in marked.iter().filter(|m| m.kind == "input" || m.kind == "output") {
        let c = m.args.split_whitespace().next().unwrap();
        let base = c.split('#').next().unwrap();
        assert!(ids.contains(base), "{}: {} {} refers to no example", m.page, m.kind, c);
    }
}

/// Every ```rust block of the pages appears verbatim in this file, inside a function that is
/// compiled (and called by `rust_dsl_snippets_run`).
#[test]
fn rust_snippets_are_compiled() {
    let me = include_str!("docs_languages_formats.rs").replace("\r\n", "\n");
    let squash = |s: &str| s.lines().map(|l| l.trim()).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n");
    let me = squash(&me);
    for m in all_marked().into_iter().filter(|m| m.kind == "rust") {
        assert!(me.contains(&squash(&m.content)), "{}: rust snippet is not in docs_languages_formats.rs:\n{}", m.page, m.content);
    }
}

#[test]
fn every_page_has_answer_line_and_example() {
    for p in pages() {
        let text = std::fs::read_to_string(&p).unwrap().replace("\r\n", "\n");
        let mut lines = text.lines().filter(|l| !l.trim().is_empty());
        let h1 = lines.next().unwrap_or("");
        assert!(h1.starts_with("# "), "{}: first line must be the H1", p.display());
        let answer = lines.next().unwrap_or("");
        assert!(answer.starts_with("> "), "{}: the line under the H1 must be the answer blockquote", p.display());
        assert!(text.contains("<!-- example:") || text.contains("<!-- eval:"), "{}: no tested example", p.display());
    }
}

// ---------------------------------------------------------------------------
// Rust DSL snippets from the pages (copied verbatim, see rust_snippets_are_compiled)
// ---------------------------------------------------------------------------

/// Start a context with the routes of `f` and return (main, request helper data).
fn rust_main(f: fn(&mut RouteBuilder)) -> cameleer_main::Main {
    setup();
    let mut main = cameleer_main::new_main();
    main.configure(move |ctx| ctx.add_routes(move |b| f(b)));
    main.start().unwrap();
    main
}

/// Send `body` to `uri` and return the reply exchange (fails on an exception).
fn rust_req(main: &cameleer_main::Main, uri: &str, body: impl Into<Value>) -> Exchange {
    let t = main.context().create_producer_template();
    let body = body.into();
    let r = t.request(uri, |e: &mut Exchange| e.set_body(body)).unwrap();
    assert!(r.exception.is_none(), "{}: {:?}", uri, r.exception.as_ref().map(|e| e.to_string()));
    r
}

// --- languages ---

fn rust_routes_languages(b: &mut RouteBuilder) {
    // constant.md
    b.from("direct:constant-rust")
        .set_header("Content-Type", constant("application/json"))
        .set_body(constant("fixed body"));
}

#[test]
fn rust_dsl_languages() {
    let main = rust_main(rust_routes_languages);
    let r = rust_req(&main, "direct:constant-rust", "x");
    assert_eq!(r.body().to_string(), "fixed body");
    assert_eq!(r.header("Content-Type").unwrap().to_string(), "application/json");
    main.stop().unwrap();
}

// --- data formats A ---

fn rust_routes_formats_a(_b: &mut RouteBuilder) {}

#[test]
fn rust_dsl_formats_a() {
    let main = rust_main(rust_routes_formats_a);
    main.stop().unwrap();
}

// --- data formats B ---

fn rust_routes_formats_b(_b: &mut RouteBuilder) {}

#[test]
fn rust_dsl_formats_b() {
    let main = rust_main(rust_routes_formats_b);
    main.stop().unwrap();
}

// --- template and XML components ---

fn rust_routes_components(_b: &mut RouteBuilder) {}

#[test]
fn rust_dsl_components() {
    let main = rust_main(rust_routes_components);
    main.stop().unwrap();
}

// ---------------------------------------------------------------------------
// Scratch helpers for writing the pages (not part of the suite unless the variable is set)
// ---------------------------------------------------------------------------

/// `DOCS_SCRATCH=route.yaml DOCS_SCRATCH_BODY=body.txt cargo test ... scratch -- --nocapture`
#[test]
fn scratch() {
    let Ok(file) = std::env::var("DOCS_SCRATCH") else { return };
    let yaml = std::fs::read_to_string(&file).unwrap();
    let body = std::env::var("DOCS_SCRATCH_BODY").ok().map(|f| std::fs::read_to_string(f).unwrap()).unwrap_or_default();
    let main = start("scratch", &yaml, None);
    let t = main.context().create_producer_template();
    let send = from_direct(&yaml).unwrap();
    let r = t.request(&send, |e: &mut Exchange| e.set_body(body.trim_end_matches('\n').to_string())).unwrap();
    println!("---- exception: {:?}", r.exception.as_ref().map(|e| e.to_string()));
    println!("---- headers:");
    for (k, v) in r.message.headers.iter() {
        println!("{} = {:?}", k, v);
    }
    println!("---- body ({}):\n{}", r.body().type_name(), r.body());
    main.stop().unwrap();
}

/// `DOCS_EVAL=lines.txt` with lines `language|fixture-json|expression`.
#[test]
fn scratch_eval() {
    let Ok(file) = std::env::var("DOCS_EVAL") else { return };
    let mut main = cameleer_main::new_main();
    main.set_property("app.greeting", "Hi");
    main.start().unwrap();
    let ctx = main.context().clone();
    for line in std::fs::read_to_string(&file).unwrap().lines().filter(|l| !l.trim().is_empty()) {
        let mut parts = line.splitn(3, "|");
        let (lang, fx, e) = (parts.next().unwrap(), parts.next().unwrap(), parts.next().unwrap());
        let fixture = if fx.trim().is_empty() { Value::Null } else { json(fx) };
        let ex = fixture_exchange(&ctx, &fixture);
        let l = ctx.resolve_language(lang).unwrap();
        let r = l.create_expression(e, &BTreeMap::new()).and_then(|x| x.evaluate(&ex));
        let p = l.create_predicate(e, &BTreeMap::new()).and_then(|x| x.matches(&ex));
        match r {
            Ok(v) => println!("{:<60} => [{}] {} (pred {:?})", e, v.type_name(), v, p.ok()),
            Err(err) => println!("{:<60} => ERROR {} (pred {:?})", e, err, p.ok()),
        }
    }
    main.stop().unwrap();
}

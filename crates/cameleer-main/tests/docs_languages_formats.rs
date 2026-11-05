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
//!   endpoint); on an output marker, `{"contains":true}` checks each line as a substring.
//!   `<!-- output: id error -->` expects the route to fail with an error containing the block.
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
                let marked = i > 0 && (lines[i - 1].trim().starts_with("<!-- example:") || lines[i - 1].trim() == "<!-- not-camel -->");
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
        let expects_error = output.as_ref().map(|o| o.args.split_whitespace().nth(1) == Some("error")).unwrap_or(false);
        let failure = match &result {
            Err(e) => Some(e.to_string()),
            Ok(r) => r.exception.as_ref().map(|e| e.to_string()),
        };
        if let Some(err) = failure {
            if expects_error {
                let want = norm(&output.as_ref().unwrap().content);
                assert!(err.contains(&want), "{}: error\n{}\ndoes not contain\n{}", case, err, want);
                asserted = true;
                continue;
            }
            panic!("{}: route failed: {}", case, err);
        }
        assert!(!expects_error, "{}: expected an error but the route succeeded", case);
        let result = result.unwrap();
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
        // ref.md
        "ref-expression-and-predicate" => Some(bind_ref_examples),
        // bean.md
        "bean-method-call" => Some(bind_bean_examples),
        _ => None,
    }
}

/// ref.md registry entries (snippet copied verbatim from the page).
fn bind_ref_examples(ctx: &CamelContext) {
    use std::sync::Arc;
    let reg = ctx.registry();
    let shipping: Arc<dyn Expression> = Arc::new(|ex: &Exchange| -> Result<Value> {
        let vip = ex.header("customerTier").map(|t| t.to_string() == "gold").unwrap_or(false);
        let kg = ex.header("weightKg").and_then(|w| w.as_i64()).unwrap_or(0);
        Ok(Value::Int(if vip { 0 } else { 5 * kg }))
    });
    reg.bind("shippingCost", shipping);
    let vip: Arc<dyn Predicate> = Arc::new(cameleer_core::spi::FnPredicate(|ex: &Exchange| {
        Ok(ex.header("customerTier").map(|t| t.to_string() == "gold").unwrap_or(false))
    }));
    reg.bind("isVip", vip);
}

/// bean.md registry entries (snippet copied verbatim from the page).
fn bind_bean_examples(ctx: &CamelContext) {
    let reg = ctx.registry();
    cameleer_core::language::bind_bean_method(reg, "pricing.discount", |ex: &Exchange| -> Result<Value> {
        let amount = ex.body().to_string().parse::<i64>().unwrap_or(0);
        Ok(Value::Int(if amount >= 100 { 10 } else { 0 }))
    });
    cameleer_core::language::bind_bean_method(reg, "risk.isHigh", |ex: &Exchange| -> Result<Value> {
        Ok(Value::Bool(ex.body().to_string().parse::<i64>().unwrap_or(0) > 1000))
    });
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
    // simple.md
    b.from("direct:simple-rust")
        .set_header("greeting", simple("Hello ${body}"))
        .filter(simple("${header.greeting} contains 'World'"))
        .set_body(simple("${header.greeting.toUpperCase()}!"))
        .end();
}

#[test]
fn rust_dsl_languages() {
    let main = rust_main(rust_routes_languages);
    let r = rust_req(&main, "direct:constant-rust", "x");
    assert_eq!(r.body().to_string(), "fixed body");
    assert_eq!(r.header("Content-Type").unwrap().to_string(), "application/json");
    assert_eq!(rust_req(&main, "direct:simple-rust", "World").body().to_string(), "HELLO WORLD!");
    assert_eq!(rust_req(&main, "direct:simple-rust", "Moon").body().to_string(), "Moon");
    main.stop().unwrap();
}

/// Script language pages (js, groovy, python, java, mvel, ognl, spel, datasonnet).
fn rust_routes_languages_scripts(b: &mut RouteBuilder) {
    // js.md
    b.from("direct:js-rust")
        .set_header("len", cameleer_script::js("body.length"))
        .set_body(cameleer_script::js("body.toUpperCase()"));
    // groovy.md
    b.from("direct:groovy-rust")
        .filter(cameleer_script::groovy("request.headers.amount > 100"))
        .set_body(cameleer_script::groovy("'big order: ' + headers.amount"));
    // python.md
    b.from("direct:python-rust")
        .set_body(cameleer_script::python("body[::-1]"));
    // java.md
    b.from("direct:java-rust")
        .set_body(cameleer_script::joor("((String) body).length() * 2"));
    // mvel.md
    b.from("direct:mvel-rust")
        .set_header("next", cameleer_script::mvel("request.headers.n + 1"));
    // ognl.md
    b.from("direct:ognl-rust")
        .set_header("twice", cameleer_script::ognl("request.body.length() * 2"));
    // spel.md
    b.from("direct:spel-rust")
        .set_body(cameleer_script::spel("Hello #{request.body}"));
    // datasonnet.md
    b.from("direct:datasonnet-rust")
        .set_body(cameleer_script::datasonnet("{total: std.foldl(function(t, i) t + i, payload.values, 0)}"));
}

#[test]
fn rust_dsl_languages_scripts() {
    let main = rust_main(rust_routes_languages_scripts);
    // js.md
    let r = rust_req(&main, "direct:js-rust", "hello");
    assert_eq!(r.body().to_string(), "HELLO");
    assert_eq!(r.header("len").unwrap().to_string(), "5");
    // groovy.md
    let t = main.context().create_producer_template();
    let r = t.request("direct:groovy-rust", |e: &mut Exchange| {
        e.set_body("order");
        e.set_header("amount", 500);
    });
    assert_eq!(r.unwrap().body().to_string(), "big order: 500");
    // python.md
    assert_eq!(rust_req(&main, "direct:python-rust", "abc").body().to_string(), "cba");
    // java.md
    assert_eq!(rust_req(&main, "direct:java-rust", "abc").body().to_string(), "6");
    // mvel.md
    let r = t.request("direct:mvel-rust", |e: &mut Exchange| e.set_header("n", 41)).unwrap();
    assert_eq!(r.header("next").unwrap().to_string(), "42");
    // ognl.md
    assert_eq!(rust_req(&main, "direct:ognl-rust", "abcd").header("twice").unwrap().to_string(), "8");
    // spel.md
    assert_eq!(rust_req(&main, "direct:spel-rust", "Ada").body().to_string(), "Hello Ada");
    // datasonnet.md
    assert_eq!(rust_req(&main, "direct:datasonnet-rust", r#"{"values":[1,2,3]}"#).body().to_string(), r#"{"total":6}"#);
    main.stop().unwrap();
}

/// Core language pages (header, exchangeProperty, variable, jsonpath, jq, xpath, xquery,
/// tokenize, ref, bean, csimple, hl7terser).
fn rust_routes_languages_core(b: &mut RouteBuilder) {
    // header.md
    b.from("direct:header-rust")
        .filter(header("urgent"))
        .set_body(header("assignee"));
    // exchangeProperty.md
    b.from("direct:property-rust")
        .set_property("original", body())
        .set_body(constant("replaced"))
        .set_body(exchange_property("original"));
    // variable.md
    b.from("direct:variable-rust")
        .set_body(language("variable", "question"));
    // jsonpath.md
    b.from("direct:jsonpath-rust")
        .set_header("finishReason", jsonpath("$.choices[0].finish_reason"))
        .set_body(jsonpath_as("$.choices[0].message.content", "String"));
    // jq.md
    b.from("direct:jq-rust")
        .filter(jq(".total > 100"))
        .set_body(jq("{id, total}"));
    // xpath.md
    b.from("direct:xpath-rust")
        .filter(xpath("/order/@status = 'new'"))
        .set_body(xpath_as("/order/@id", "String"));
    // xquery.md
    b.from("direct:xquery-rust")
        .set_body(cameleer_xml::xquery_component::xquery_expr("string-join(//item/@sku, ',')"));
    // tokenize.md
    b.from("direct:tokenize-rust")
        .split(tokenize("\n"))
        .to("mock:tokenize-line")
        .end()
        .set_body(tokenize_pair("[", "]", false));
    // ref.md
    b.from("direct:ref-rust")
        .filter(ref_expression("isVip"))
        .set_body(ref_expression("shippingCost"));
    // bean.md
    b.from("direct:bean-rust")
        .set_header("discount", bean_method("pricing", "discount"))
        .set_body(method("risk.isHigh"));
    // csimple.md
    b.from("direct:csimple-rust")
        .set_body(language("csimple", "Hello ${body}"));
    // hl7terser.md
    b.from("direct:hl7terser-rust")
        .set_header("patient", language("hl7terser", "PID-5-1"))
        .set_body(language("hl7terser", "MSH-9-2"));
}

#[test]
fn rust_dsl_languages_core() {
    setup();
    let mut main = cameleer_main::new_main();
    main.configure(|ctx| {
        bind_ref_examples(ctx);
        bind_bean_examples(ctx);
        ctx.add_routes(|b| rust_routes_languages_core(b))
    });
    main.start().unwrap();
    let t = main.context().create_producer_template();
    // header.md
    let r = t.request("direct:header-rust", |e: &mut Exchange| {
        e.set_body("x");
        e.set_header("urgent", true);
        e.set_header("assignee", "on-call");
    });
    assert_eq!(r.unwrap().body().to_string(), "on-call");
    // exchangeProperty.md
    assert_eq!(rust_req(&main, "direct:property-rust", "first").body().to_string(), "first");
    // variable.md
    let r = t.request("direct:variable-rust", |e: &mut Exchange| e.set_property("CamelVariable.question", "When?"));
    assert_eq!(r.unwrap().body().to_string(), "When?");
    // jsonpath.md
    let r = rust_req(&main, "direct:jsonpath-rust", r#"{"choices":[{"finish_reason":"stop","message":{"content":"Hi"}}]}"#);
    assert_eq!(r.body().to_string(), "Hi");
    assert_eq!(r.header("finishReason").unwrap().to_string(), "stop");
    // jq.md
    assert_eq!(rust_req(&main, "direct:jq-rust", r#"{"id":1,"total":150,"x":0}"#).body().to_string(), r#"{"id":1,"total":150}"#);
    // xpath.md
    assert_eq!(rust_req(&main, "direct:xpath-rust", r#"<order id="7" status="new"/>"#).body().to_string(), "7");
    // xquery.md
    assert_eq!(rust_req(&main, "direct:xquery-rust", r#"<o><item sku="a"/><item sku="b"/></o>"#).body().to_string(), "a,b");
    // tokenize.md
    assert_eq!(rust_req(&main, "direct:tokenize-rust", "[x]\n[y]").body().to_string(), r#"["x","y"]"#);
    assert_eq!(main.context().mock_endpoint("mock:tokenize-line").unwrap().received_exchanges().len(), 2);
    // ref.md
    let r = t.request("direct:ref-rust", |e: &mut Exchange| {
        e.set_header("customerTier", "gold");
        e.set_header("weightKg", 4);
    });
    assert_eq!(r.unwrap().body().to_string(), "0");
    // bean.md
    let r = rust_req(&main, "direct:bean-rust", "5000");
    assert_eq!(r.body().to_string(), "true");
    assert_eq!(r.header("discount").unwrap().to_string(), "10");
    // csimple.md
    assert_eq!(rust_req(&main, "direct:csimple-rust", "Ada").body().to_string(), "Hello Ada");
    // hl7terser.md
    let r = rust_req(&main, "direct:hl7terser-rust", "MSH|^~\\&|A|B|C|D|20240102||ADT^A01|1|P|2.5\rPID|1||9||Doe^John\r");
    assert_eq!(r.body().to_string(), "A01");
    assert_eq!(r.header("patient").unwrap().to_string(), "Doe");
    main.stop().unwrap();
}

// --- data formats A ---

fn rust_routes_formats_a(b: &mut RouteBuilder) {
    // json.md
    b.from("direct:json-rust")
        .unmarshal(DataFormatDefinition::json())
        .set_body(simple("${body.intent}"));
    // csv.md
    b.from("direct:csv-rust")
        .unmarshal(DataFormatDefinition::csv().use_maps(true))
        .marshal(DataFormatDefinition::json());
    // base64.md
    b.from("direct:base64-rust")
        .marshal(DataFormatDefinition::base64().line_length(0));
    // hex.md
    b.from("direct:hex-rust")
        .marshal(DataFormatDefinition::hex());
    // string.md
    b.from("direct:string-rust")
        .marshal(DataFormatDefinition::string().charset("ISO-8859-1"));
    // gzip.md
    b.from("direct:gzip-rust")
        .marshal(DataFormatDefinition::gzip().compression_level(9))
        .unmarshal(DataFormatDefinition::gzip());
    // zip-deflater.md
    b.from("direct:zlib-rust")
        .marshal(DataFormatDefinition::zip_deflater())
        .unmarshal(DataFormatDefinition::zip_deflater());
    // zipfile.md
    b.from("direct:zipfile-rust")
        .set_header("CamelFileName", constant("data.csv"))
        .marshal(DataFormatDefinition::zip_file());
    // urlencoded.md
    b.from("direct:urlencoded-rust")
        .unmarshal(DataFormatDefinition::urlencoded())
        .set_body(simple("${body[name]}"));
    // jacksonxml.md
    b.from("direct:xml-rust")
        .unmarshal(DataFormatDefinition::json())
        .marshal(DataFormatDefinition::xml().root_name("order"));
    // yaml.md
    b.from("direct:yaml-rust")
        .unmarshal(DataFormatDefinition::new("yaml"))
        .marshal(DataFormatDefinition::json());
    // cbor.md
    b.from("direct:cbor-rust")
        .marshal(DataFormatDefinition::new("cbor"))
        .unmarshal(DataFormatDefinition::new("cbor"));
    // avro.md
    b.from("direct:avro-rust")
        .unmarshal(DataFormatDefinition::json())
        .marshal(DataFormatDefinition::new("avro").option("schemaFile", "sensor.avsc"))
        .unmarshal(DataFormatDefinition::new("avro").option("schemaFile", "sensor.avsc"));
    // protobuf.md
    b.from("direct:protobuf-rust")
        .marshal(
            DataFormatDefinition::new("protobuf")
                .option("instanceClass", "edge.Reading")
                .option("protoFile", "reading.proto"),
        );
    // bindy.md
    b.from("direct:bindy-rust")
        .unmarshal(DataFormatDefinition::new("bindyCsv").option("descriptorFile", "order-csv.yaml"))
        .marshal(DataFormatDefinition::json());
    // univocity.md
    b.from("direct:univocity-rust")
        .unmarshal(DataFormatDefinition::new("univocityTsv").option("headerExtractionEnabled", true).option("asMap", true))
        .marshal(DataFormatDefinition::json());
}

#[test]
fn rust_dsl_formats_a() {
    let main = rust_main(rust_routes_formats_a);
    let s = |uri: &str, body: &str| rust_req(&main, uri, body.to_string());
    assert_eq!(s("direct:json-rust", r#"{"intent":"refund"}"#).body().to_string(), "refund");
    assert_eq!(s("direct:csv-rust", "a,b\n1,2\n").body().to_string(), r#"[{"a":"1","b":"2"}]"#);
    assert_eq!(s("direct:base64-rust", "Hi").body().to_string(), "SGk=");
    assert_eq!(s("direct:hex-rust", "Hi").body().to_string(), "4869");
    assert_eq!(s("direct:string-rust", "é").body().to_bytes(), vec![0xe9]);
    assert_eq!(s("direct:gzip-rust", "abc").body().to_bytes(), b"abc".to_vec());
    assert_eq!(s("direct:zlib-rust", "abc").body().to_bytes(), b"abc".to_vec());
    let z = s("direct:zipfile-rust", "x,y");
    assert_eq!(z.header("CamelFileName").unwrap().to_string(), "data.csv.zip");
    assert_eq!(&z.body().to_bytes()[..2], b"PK");
    assert_eq!(s("direct:urlencoded-rust", "name=Ada&x=1").body().to_string(), "Ada");
    assert_eq!(s("direct:xml-rust", r#"{"id":"1"}"#).body().to_string(), "<order><id>1</id></order>");
    assert_eq!(s("direct:yaml-rust", "a: 1").body().to_string(), r#"{"a":1}"#);
    assert_eq!(rust_req(&main, "direct:cbor-rust", Value::from(vec![Value::Int(1), Value::Int(2)])).body().to_string(), "[1,2]");
    assert_eq!(s("direct:avro-rust", r#"{"id":"s-1","temp":21.5,"ok":true}"#).body().to_string(), r#"{"id":"s-1","temp":21.5,"ok":true}"#);
    assert_eq!(s("direct:protobuf-rust", r#"{"id":"s-1"}"#).body().to_bytes(), b"\x0a\x03s-1".to_vec());
    assert_eq!(s("direct:bindy-rust", "h\n1,Ada,49.5,02-01-2024\n").body().to_string(), r#"{"orderNr":1,"customer":"Ada","amount":49.5,"orderDate":"2024-01-02"}"#);
    assert_eq!(s("direct:univocity-rust", "a\tb\n1\t2\n").body().to_string(), r#"[{"a":"1","b":"2"}]"#);
    main.stop().unwrap();
}

// --- data formats B ---

fn rust_routes_formats_b(b: &mut RouteBuilder) {
    // tarfile.md
    b.from("direct:tar-rust")
        .set_header("CamelFileName", constant("notes.txt"))
        .marshal(DataFormatDefinition::new("tarFile"))
        .unmarshal(DataFormatDefinition::new("tarFile"))
        .convert_body_to("String");
    // crypto.md
    let aes = DataFormatDefinition::new("crypto")
        .option("algorithm", "AES/GCM/NoPadding")
        .option("key", "hex:000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f")
        .option("inline", "true");
    b.from("direct:crypto-rust")
        .marshal(aes.clone())
        .unmarshal(aes)
        .convert_body_to("String");
    // pgp.md
    b.from("direct:pgp-rust")
        .marshal(DataFormatDefinition::new("pgp").option("keyFileName", "keys/docs-pub.asc").option("armored", "true"))
        .unmarshal(DataFormatDefinition::new("pgp").option("keyFileName", "keys/docs-sec.asc").option("password", "docs-only"))
        .convert_body_to("String");
    // thrift.md
    let reading = DataFormatDefinition::new("thrift")
        .option("idlFile", "idl/telemetry.thrift")
        .option("instanceClass", "Reading")
        .option("contentTypeFormat", "compact");
    b.from("direct:thrift-rust")
        .unmarshal_json()
        .marshal(reading.clone())
        .unmarshal(reading)
        .marshal_json();
    // asn1.md
    b.from("direct:asn1-rust")
        .unmarshal(DataFormatDefinition::new("asn1").option("usingIterator", "true"))
        .split(simple("${body}"))
        .to("mock:asn1-record")
        .end();
    // fory.md
    b.from("direct:fory-rust")
        .unmarshal_json()
        .marshal(DataFormatDefinition::new("fory"))
        .unmarshal(DataFormatDefinition::new("fory"))
        .marshal_json();
    // barcode.md
    b.from("direct:barcode-rust")
        .marshal(DataFormatDefinition::new("barcode").option("barcodeFormat", "DATA_MATRIX").option("imageType", "PNG"))
        .unmarshal(DataFormatDefinition::new("barcode").option("barcodeFormat", "DATA_MATRIX"));
    // ical.md
    b.from("direct:ical-rust")
        .unmarshal(DataFormatDefinition::new("iCal"))
        .set_body(simple("${body.components[0].properties.SUMMARY}"));
    // mime-multipart.md
    b.from("direct:mime-rust")
        .marshal(DataFormatDefinition::new("mimeMultipart").option("headersInline", "true").option("multipartWithoutAttachment", "true"))
        .unmarshal(DataFormatDefinition::new("mimeMultipart").option("headersInline", "true"));
    // hl7.md
    b.from("direct:hl7-rust")
        .unmarshal(DataFormatDefinition::named("hl7"))
        .set_body(simple("${header.CamelHL7MessageType}^${header.CamelHL7TriggerEvent}"));
    // syslog.md
    b.from("direct:syslog-rust")
        .unmarshal(DataFormatDefinition::named("syslog"))
        .set_body(simple("${header.CamelSyslogSeverity}: ${body.logMessage}"));
    // fhir.md
    b.from("direct:fhir-rust")
        .unmarshal(DataFormatDefinition::new("fhirXml"))
        .marshal(DataFormatDefinition::new("fhirJson"));
    // jaxb.md
    b.from("direct:jaxb-rust")
        .unmarshal(DataFormatDefinition::new("jaxb").option("contextPath", "schemas/order.xsd"))
        .marshal_json();
}

#[test]
fn rust_dsl_formats_b() {
    let main = rust_main(rust_routes_formats_b);
    let s = |uri: &str, body: &str| rust_req(&main, uri, body.to_string()).body().to_string();
    assert_eq!(s("direct:tar-rust", "tar me"), "tar me");
    assert_eq!(s("direct:crypto-rust", "secret"), "secret");
    assert_eq!(s("direct:pgp-rust", "for docs only"), "for docs only");
    assert_eq!(s("direct:thrift-rust", r#"{"sensor":"t9","celsius":4}"#), r#"{"sensor":"t9","celsius":4,"status":"OK"}"#);
    rust_req(&main, "direct:asn1-rust", Value::Bytes(vec![0x30, 0x03, 0x02, 0x01, 0x01, 0x30, 0x03, 0x02, 0x01, 0x02]));
    assert_eq!(main.context().mock_endpoint("mock:asn1-record").unwrap().received_exchanges().len(), 2);
    assert_eq!(s("direct:fory-rust", r#"{"a":[1,2]}"#), r#"{"a":[1,2]}"#);
    assert_eq!(s("direct:barcode-rust", "LOT-2026-11"), "LOT-2026-11");
    assert_eq!(s("direct:ical-rust", "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:x\r\nBEGIN:VEVENT\r\nSUMMARY:Standup\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"), "Standup");
    assert_eq!(s("direct:mime-rust", "only a body"), "only a body");
    assert_eq!(s("direct:hl7-rust", "MSH|^~\\&|A|B|C|D|20260101||ORU^R01|1|P|2.5\rPID|1"), "ORU^R01");
    assert_eq!(s("direct:syslog-rust", "<11>1 2026-11-05T10:00:00Z h app - - - disk full"), "ERR: disk full");
    assert!(s("direct:fhir-rust", r#"<Patient xmlns="http://hl7.org/fhir"><id value="p2"/></Patient>"#).contains(r#""id":"p2""#));
    assert_eq!(s("direct:jaxb-rust", r#"<order id="1"><customer>C</customer><line><sku>S</sku><qty>2</qty></line></order>"#), r#"{"@id":1,"customer":"C","line":[{"sku":"S","qty":2}]}"#);
    main.stop().unwrap();
}
// --- template and XML components ---

fn rust_routes_components(b: &mut RouteBuilder) {
    // mustache.md
    b.from("direct:mustache-rust")
        .to("mustache:templates/support-prompt.mustache?contentCache=true");
    // freemarker.md
    b.from("direct:freemarker-rust")
        .unmarshal(DataFormatDefinition::json())
        .to("freemarker:templates/greeting.ftl");
    // velocity.md
    b.from("direct:velocity-rust")
        .set_header("CamelVelocitySupplementalContext", simple("${map('name','Ada','team','ops')}"))
        .to("velocity:templates/hello.vm");
    // string-template.md
    b.from("direct:string-template-rust")
        .to("string-template:templates/item.st?delimiterStart=$&delimiterStop=$");
    // mvel.md
    b.from("direct:mvel-rust")
        .unmarshal(DataFormatDefinition::json())
        .to("mvel:templates/order.mvel");
    // xslt.md
    b.from("direct:xslt-rust")
        .set_header("currency", constant("GBP"))
        .to("xslt:xsl/order-to-json.xsl");
    // xslt-saxon.md
    b.from("direct:xslt-saxon-rust")
        .to("xslt-saxon:xsl/sales-by-region.xsl");
    // xquery.md
    b.from("direct:xquery-rust")
        .to("xquery:xq/names.xq?resultsFormat=String");
    // xj.md
    b.from("direct:xj-rust")
        .to("xj:identity?transformDirection=XML2JSON");
    // validator.md
    b.from("direct:validator-rust")
        .to("validator:xsd/order.xsd")
        .set_body(constant("valid"));
    // json-validator.md
    b.from("direct:json-validator-rust")
        .to("json-validator:schemas/triage-answer.json")
        .set_body(constant("ok"));
    // xmlsecurity-sign.md
    b.from("direct:xmlsecurity-sign-rust")
        .to("xmlsecurity-sign:invoice?privateKey=file:keys/docs-sign.pem&addKeyValue=true");
    // xmlsecurity-verify.md
    b.from("direct:xmlsecurity-verify-rust")
        .to("xmlsecurity-sign:report?privateKey=file:keys/docs-verify.pem")
        .to("xmlsecurity-verify:report?publicKey=file:keys/docs-verify.pub.pem&removeSignature=false");
}

#[test]
fn rust_dsl_components() {
    let main = rust_main(rust_routes_components);
    let order = cameleer_core::json::parse(r#"{"customer":"Ada","question":"Q?","docs":[]}"#).unwrap();
    let r = rust_req(&main, "direct:mustache-rust", order);
    assert!(r.body().to_string().contains("Question: Q?"), "{}", r.body());
    let r = rust_req(&main, "direct:freemarker-rust", r#"{"name":"cy"}"#);
    assert_eq!(r.body().to_string().trim(), "Hello Cy, you have 0 points.");
    let r = rust_req(&main, "direct:velocity-rust", "x");
    assert_eq!(r.body().to_string().trim(), "Hello Ada from ops, quiet missing value: []");
    let r = rust_req(&main, "direct:string-template-rust", "Rust");
    assert_eq!(r.body().to_string().trim(), "<li>Rust</li>");
    let r = rust_req(&main, "direct:mvel-rust", r#"{"id":1,"customer":"bo","items":[{"sku":"C3","qty":1,"price":5}]}"#);
    assert!(r.body().to_string().contains("Total: 5"), "{}", r.body());
    let r = rust_req(&main, "direct:xslt-rust", r#"<order id="X"><customer><name>Bo</name></customer><line sku="A" qty="1" price="2"/></order>"#);
    assert!(r.body().to_string().contains(r#""currency":"GBP""#), "{}", r.body());
    let r = rust_req(&main, "direct:xslt-saxon-rust", r#"<sales><sale region="n" rep="x" amount="5"/></sales>"#);
    assert_eq!(r.body().to_string().trim(), "n: 5 (x)");
    let r = rust_req(&main, "direct:xquery-rust", r#"<t><person name="b"/><person name="a"/></t>"#);
    assert_eq!(r.body().to_string().trim(), "a, b");
    let r = rust_req(&main, "direct:xj-rust", "<a><b>1</b></a>");
    assert_eq!(r.body().to_string().trim(), r#"{"b":"1"}"#);
    let r = rust_req(&main, "direct:validator-rust", r#"<order id="1"><customer>A</customer><qty>1</qty></order>"#);
    assert_eq!(r.body().to_string(), "valid");
    let r = rust_req(&main, "direct:json-validator-rust", r#"{"priority":"P3","team":"ops","summary":"s"}"#);
    assert_eq!(r.body().to_string(), "ok");
    let r = rust_req(&main, "direct:xmlsecurity-sign-rust", "<a>1</a>");
    assert!(r.body().to_string().contains("ECKeyValue"), "{}", r.body());
    let r = rust_req(&main, "direct:xmlsecurity-verify-rust", "<a>1</a>");
    assert!(r.body().to_string().contains("<ds:Signature"), "{}", r.body());
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

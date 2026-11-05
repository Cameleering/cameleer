//! The documentation harness: every Camel YAML example in `docs/**/*.md` (and `README.md`)
//! is marked, uniquely named, parsed with the real YAML DSL loader and has its endpoint URIs
//! resolved against a context with every component (which rejects unknown options, like
//! Apache Camel). It also checks that relative links in `docs/` point to existing files and
//! that every component and language in the catalog has a page.
//!
//! Conventions (see docs/README.md):
//! - a ```yaml block is preceded on the line directly above by `<!-- example: <id> -->`, or by
//!   `<!-- not-camel -->` when it is not a Camel route (Docker Compose, Kubernetes, ...);
//! - a component page is `docs/components/<scheme>.md`; alias schemes are listed on the
//!   page's `**Also:**` line; a language page is `docs/languages/<name>.md`.
//!
//! The checks are plain functions over a directory, so the fixture tests at the bottom run
//! them on `tests/docs_fixtures/` (good and deliberately broken pages).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use cameleer_core::value::Value;

// ---------------------------------------------------------------------------
// Markdown scanning
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Marker {
    None,
    Example(String),
    NotCamel,
}

#[derive(Debug, Clone)]
struct Block {
    /// 1-based line of the opening fence.
    line: usize,
    lang: String,
    marker: Marker,
    content: String,
}

/// The marker comment on a line, if any.
fn marker_of(line: &str) -> Marker {
    let t = line.trim();
    let Some(inner) = t.strip_prefix("<!--").and_then(|r| r.strip_suffix("-->")) else {
        return Marker::None;
    };
    let inner = inner.trim();
    if inner == "not-camel" {
        return Marker::NotCamel;
    }
    match inner.strip_prefix("example:") {
        Some(id) if !id.trim().is_empty() => Marker::Example(id.trim().to_string()),
        _ => Marker::None,
    }
}

/// The opening or closing fence on a line: (fence char, length, info string).
fn fence_of(line: &str) -> Option<(char, usize, String)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let c = rest.chars().next()?;
    if c != '`' && c != '~' {
        return None;
    }
    let n = rest.chars().take_while(|&x| x == c).count();
    if n < 3 {
        return None;
    }
    Some((c, n, rest[n..].trim().to_string()))
}

/// Every fenced code block in a markdown text.
fn code_blocks(text: &str) -> Vec<Block> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if let Some((c, n, info)) = fence_of(lines[i]) {
            let start = i;
            let mut body = Vec::new();
            i += 1;
            while i < lines.len() {
                if let Some((c2, n2, info2)) = fence_of(lines[i]) {
                    if c2 == c && n2 >= n && info2.is_empty() {
                        break;
                    }
                }
                body.push(lines[i]);
                i += 1;
            }
            let marker = if start > 0 { marker_of(lines[start - 1]) } else { Marker::None };
            let lang = info.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
            out.push(Block { line: start + 1, lang, marker, content: body.join("\n") + "\n" });
        }
        i += 1;
    }
    out
}

/// The text with fenced code blocks blanked out (line numbers are kept).
fn prose_lines(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    for line in text.lines() {
        match (fence, fence_of(line)) {
            (None, Some((c, n, _))) => {
                fence = Some((c, n));
                out.push(String::new());
            }
            (Some((c, n)), Some((c2, n2, info))) if c == c2 && n2 >= n && info.is_empty() => {
                fence = None;
                out.push(String::new());
            }
            (Some(_), _) => out.push(String::new()),
            (None, None) => out.push(line.to_string()),
        }
    }
    out
}

/// Remove inline code spans and HTML comments from a prose line.
fn strip_inline_code(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(p) = rest.find('`') {
        out.push_str(&rest[..p]);
        let ticks = rest[p..].chars().take_while(|&c| c == '`').count();
        let after = &rest[p + ticks..];
        let close = "`".repeat(ticks);
        match after.find(&close) {
            Some(q) => rest = &after[q + ticks..],
            None => {
                rest = after;
            }
        }
    }
    out.push_str(rest);
    // comments on one line
    while let (Some(a), Some(b)) = (out.find("<!--"), out.find("-->")) {
        if b < a {
            break;
        }
        out.replace_range(a..b + 3, "");
    }
    out
}

/// Link targets of inline links and images (`[t](target "title")`) and reference
/// definitions (`[id]: target`), with their 1-based line numbers.
fn link_targets(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (i, raw) in prose_lines(text).iter().enumerate() {
        let line = strip_inline_code(raw);
        let t = line.trim_start();
        if t.starts_with('[') {
            if let Some(p) = t.find("]:") {
                if !t[..p].contains("](") {
                    let target = t[p + 2..].split_whitespace().next().unwrap_or("").trim_matches(|c| c == '<' || c == '>');
                    if !target.is_empty() {
                        out.push((i + 1, target.to_string()));
                    }
                    continue;
                }
            }
        }
        let bytes = line.as_bytes();
        let mut k = 0;
        while let Some(p) = line[k..].find("](") {
            let start = k + p + 2;
            let mut depth = 0i32;
            let mut end = start;
            while end < bytes.len() {
                match bytes[end] {
                    b'(' => depth += 1,
                    b')' if depth == 0 => break,
                    b')' => depth -= 1,
                    _ => {}
                }
                end += 1;
            }
            let inner = line[start..end].trim();
            let target = if let Some(r) = inner.strip_prefix('<') {
                r.split('>').next().unwrap_or("")
            } else {
                inner.split_whitespace().next().unwrap_or("")
            };
            if !target.is_empty() {
                out.push((i + 1, target.to_string()));
            }
            k = end.min(bytes.len());
            if k >= bytes.len() {
                break;
            }
        }
    }
    out
}

fn is_external(target: &str) -> bool {
    if target.starts_with("//") {
        return true;
    }
    let slash = target.find('/');
    match target.find(':') {
        Some(c) => slash.map(|s| c < s).unwrap_or(true) && target[..c].chars().all(|ch| ch.is_ascii_alphanumeric() || "+-.".contains(ch)),
        None => false,
    }
}

fn markdown_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().and_then(|e| e.to_str()) == Some("md") {
                out.push(p);
            }
        }
    }
    walk(dir, &mut out);
    out
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).to_string_lossy().replace('\\', "/")
}

// ---------------------------------------------------------------------------
// Endpoint URIs
// ---------------------------------------------------------------------------

/// `{{env:NAME:default}}` becomes `default`; `{{env:NAME}}` or an empty default becomes a
/// non-empty dummy (`cGxhY2Vob2xkZXI=`, valid base64).
/// Resolution errors caused by a missing file or registry bean rather than by the URI.
fn needs_environment(e: &str) -> bool {
    ["NoSuchBeanException", "NoSuchEndpointException", "os error 2", "FileNotFoundException", "Cannot read"].iter().any(|m| e.contains(m))
}

fn substitute_env(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(p) = rest.find("{{env:") {
        out.push_str(&rest[..p]);
        let after = &rest[p + 6..];
        match after.find("}}") {
            Some(q) => {
                let inner = &after[..q];
                // a missing or empty default (a secret the reader must supply) becomes a
                // non-empty dummy that is also valid base64, as keys are checked for that
                out.push_str(match inner.split_once(':') {
                    Some((_, default)) if !default.is_empty() => default,
                    _ => "cGxhY2Vob2xkZXI=",
                });
                rest = &after[q + 2..];
            }
            None => {
                out.push_str(&rest[p..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn looks_like_uri(s: &str) -> bool {
    match s.find(':') {
        Some(c) if c > 0 => {
            let scheme = &s[..c];
            scheme.chars().next().map(|ch| ch.is_ascii_alphabetic()).unwrap_or(false)
                && scheme.chars().all(|ch| ch.is_ascii_alphanumeric() || "+-.".contains(ch))
        }
        _ => false,
    }
}

const URI_KEYS: &[&str] = &["from", "to", "toD", "wireTap", "enrich", "pollEnrich"];

/// The endpoint URIs of a parsed YAML DSL document: `uri:` (with `parameters:`), and string
/// values of `from`, `to`, `toD`, `wireTap`, `enrich` and `pollEnrich`.
fn endpoint_uris(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            if let Some(Value::String(u)) = m.get("uri") {
                match cameleer_yaml::build_uri(u, m.get("parameters")) {
                    Ok(u) => out.push(u),
                    Err(_) => out.push(u.clone()),
                }
            }
            for (k, val) in m {
                if let Value::String(s) = val {
                    if URI_KEYS.contains(&k.as_str()) {
                        out.push(s.clone());
                    }
                }
                if k != "parameters" {
                    endpoint_uris(val, out);
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| endpoint_uris(x, out)),
        _ => {}
    }
}

/// The URIs to resolve: dynamic (`${...}`) and template (`{{param}}`) URIs are skipped.
fn resolvable_uris(doc: &Value) -> Vec<String> {
    let mut raw = Vec::new();
    endpoint_uris(doc, &mut raw);
    let mut out = Vec::new();
    for u in raw {
        if u.contains("${") {
            continue;
        }
        let s = substitute_env(&u);
        if s.contains("{{") || !looks_like_uri(&s) {
            continue;
        }
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Checks
// ---------------------------------------------------------------------------

#[derive(Default, Debug)]
struct ExampleReport {
    errors: Vec<String>,
    examples: usize,
    uris: usize,
    /// yaml blocks without a marker, as `file:line`.
    unmarked: Vec<String>,
}

/// Whether unmarked yaml blocks in this file are errors (the documentation pages) or only
/// reported (the project files that predate the docs: README.md and docs/*.md other than
/// docs/README.md).
fn strict_file(rel_path: &str) -> bool {
    rel_path == "docs/README.md" || (rel_path.starts_with("docs/") && rel_path[5..].contains('/'))
}

/// Check the yaml examples of `files` (paths relative to `root`).
fn check_examples(root: &Path, files: &[PathBuf]) -> ExampleReport {
    let mut r = ExampleReport::default();
    let mut ids: BTreeMap<String, String> = BTreeMap::new();
    for f in files {
        let rp = rel(root, f);
        let text = match std::fs::read_to_string(f) {
            Ok(t) => t,
            Err(e) => {
                r.errors.push(format!("{}: {}", rp, e));
                continue;
            }
        };
        for b in code_blocks(&text) {
            if b.lang != "yaml" && b.lang != "yml" {
                if let Marker::Example(id) = &b.marker {
                    r.errors.push(format!("{}:{}: example '{}' is on a ```{} block; examples must be ```yaml", rp, b.line, id, b.lang));
                }
                continue;
            }
            let at = format!("{}:{}", rp, b.line);
            let id = match &b.marker {
                Marker::NotCamel => continue,
                Marker::None => {
                    if strict_file(&rp) {
                        r.errors.push(format!("{}: ```yaml block without `<!-- example: <id> -->` on the line above", at));
                    }
                    r.unmarked.push(at);
                    continue;
                }
                Marker::Example(id) => id.clone(),
            };
            r.examples += 1;
            if let Some(prev) = ids.insert(id.clone(), at.clone()) {
                r.errors.push(format!("{}: example id '{}' is already used at {}", at, id, prev));
            }
            // a list of top-level elements
            let doc = match cameleer_yaml::parse_yaml_all(&b.content) {
                Ok(docs) if docs.len() == 1 => docs.into_iter().next().unwrap(),
                Ok(docs) => Value::Array(docs.into_iter().flat_map(|d| if let Value::Array(a) = d { a } else { vec![d] }).collect()),
                Err(e) => {
                    r.errors.push(format!("{} [{}]: invalid YAML: {}", at, id, e));
                    continue;
                }
            };
            match &doc {
                Value::Array(items) if !items.is_empty() && items.iter().all(|i| matches!(i, Value::Object(_))) => {}
                _ => {
                    r.errors.push(format!("{} [{}]: not a Camel YAML DSL document (expected a list like `- route:`)", at, id));
                    continue;
                }
            }
            // the real loader, into a context that is not started
            let main = cameleer_main::new_main();
            let ctx = main.context().clone();
            if let Err(e) = cameleer_yaml::add_routes_from_yaml(&ctx, &b.content) {
                r.errors.push(format!("{} [{}]: the YAML DSL loader rejects it: {}", at, id, e));
                continue;
            }
            // every endpoint resolves (unknown options fail)
            for uri in resolvable_uris(&doc) {
                r.uris += 1;
                if let Err(e) = ctx.get_endpoint(&uri) {
                    // the URI is fine but needs something the reader provides next to the route
                    // (a .proto/.thrift/WSDL file, a bean registered from Rust); the writers'
                    // own tests run those examples with their fixtures
                    if needs_environment(&e.to_string()) {
                        continue;
                    }
                    r.errors.push(format!("{} [{}]: endpoint `{}` does not resolve: {}", at, id, uri, e));
                }
            }
            let _ = ctx.stop();
        }
    }
    r
}

/// The section directories whose index page the site generator creates.
const SECTION_INDEXES: &[&str] = &["components", "languages", "dataformats", "guides"];

/// A path with `.` and `..` components resolved lexically.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Relative links (and images) in `files` that do not point to an existing file.
fn check_links(root: &Path, files: &[PathBuf]) -> Vec<String> {
    let mut errors = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(f).unwrap_or_default();
        let dir = f.parent().unwrap_or(root);
        for (line, target) in link_targets(&text) {
            if target.starts_with('#') || is_external(&target) {
                continue;
            }
            let path = target.split(['#', '?']).next().unwrap_or("");
            if path.is_empty() {
                continue;
            }
            let path = path.replace("%20", " ");
            let resolved = if let Some(abs) = path.strip_prefix('/') { root.join(abs) } else { dir.join(&path) };
            // the section indexes (`components/`, ...) are generated by scripts/build-docs.py
            let generated = SECTION_INDEXES.iter().any(|s| normalize(&resolved) == normalize(&root.join("docs").join(s)));
            if !resolved.exists() && !generated {
                errors.push(format!("{}:{}: link target `{}` does not exist", rel(root, f), line, target));
            }
        }
    }
    errors
}

/// The names on `**Also:**` lines: backticked words, with a trailing `:` removed.
fn also_names(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(p) = line.find("**Also:**") else { continue };
        let rest = &line[p + 9..];
        // up to the next field (` · **...`) or the end of the line
        let rest = match rest.find("**") {
            Some(q) => &rest[..q],
            None => rest,
        };
        let mut parts = rest.split('`');
        parts.next();
        while let Some(code) = parts.next() {
            let name = code.trim().trim_end_matches("://").trim_end_matches(':').trim();
            if !name.is_empty() {
                out.push(name.to_string());
            }
            parts.next();
        }
    }
    out
}

/// Names that have neither `<dir>/<name>.md` nor an `**Also:**` mention on a page in `dir`.
fn missing_pages(dir: &Path, names: &[String]) -> Vec<String> {
    let mut covered = BTreeSet::new();
    for f in markdown_files(dir) {
        if let Some(stem) = f.file_stem().and_then(|s| s.to_str()) {
            covered.insert(stem.to_string());
        }
        for n in also_names(&std::fs::read_to_string(&f).unwrap_or_default()) {
            covered.insert(n);
        }
    }
    names.iter().filter(|n| !covered.contains(n.as_str())).cloned().collect()
}

// ---------------------------------------------------------------------------
// The real docs
// ---------------------------------------------------------------------------

/// The repository root; `CAMELEER_DOCS_ROOT` points the harness at another tree (a directory
/// with `docs/` and optionally `README.md`), for example a branch checked out elsewhere.
fn repo_root() -> PathBuf {
    match std::env::var_os("CAMELEER_DOCS_ROOT") {
        Some(p) => PathBuf::from(p),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap(),
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/docs_fixtures")
}

/// docs/**/*.md and README.md.
fn doc_files(root: &Path) -> Vec<PathBuf> {
    let mut files = markdown_files(&root.join("docs"));
    let readme = root.join("README.md");
    if readme.exists() {
        files.push(readme);
    }
    files
}

fn fail(what: &str, errors: &[String]) {
    if !errors.is_empty() {
        panic!("{} ({}):\n  {}", what, errors.len(), errors.join("\n  "));
    }
}

/// Every marked yaml example parses with the YAML DSL loader and its endpoints resolve; ids
/// are unique; documentation pages have no unmarked yaml blocks.
#[test]
fn docs_yaml_examples_parse_and_resolve() {
    let root = repo_root();
    let r = check_examples(&root, &doc_files(&root));
    println!("{} examples, {} endpoint URIs resolved", r.examples, r.uris);
    fail("documentation examples", &r.errors);
}

/// Every ```yaml block in the repository's docs and README.md has a marker. Separate from the
/// test above because README.md and the older docs/*.md files are maintained elsewhere: this
/// fails, listing the blocks, until they are marked.
#[test]
fn docs_yaml_blocks_all_marked() {
    let root = repo_root();
    let r = check_examples(&root, &doc_files(&root));
    let msgs: Vec<String> = r
        .unmarked
        .iter()
        .map(|at| format!("{}: add `<!-- example: <id> -->` (Camel route) or `<!-- not-camel -->` on the line above", at))
        .collect();
    fail("unmarked ```yaml blocks", &msgs);
}

/// Relative links in docs/ point to existing files.
#[test]
fn docs_relative_links_exist() {
    let root = repo_root();
    fail("broken relative links", &check_links(&root, &markdown_files(&root.join("docs"))));
}

/// Every component in the catalog has docs/components/<scheme>.md or is listed on another
/// component page's `**Also:**` line; every language has docs/languages/<name>.md (or an
/// `**Also:**` mention on a language page). Fails until the documentation is complete.
#[test]
fn docs_catalog_complete() {
    let root = repo_root();
    let main = cameleer_main::new_main();
    let components = main.context().component_names();
    let languages = main.context().language_names();
    let mc = missing_pages(&root.join("docs/components"), &components);
    let ml = missing_pages(&root.join("docs/languages"), &languages);
    println!(
        "components: {} in the catalog, {} without a page; languages: {} in the catalog, {} without a page",
        components.len(),
        mc.len(),
        languages.len(),
        ml.len()
    );
    let mut errors: Vec<String> = mc.iter().map(|c| format!("component `{}`: no docs/components/{}.md and no **Also:** mention", c, c)).collect();
    errors.extend(ml.iter().map(|l| format!("language `{}`: no docs/languages/{}.md", l, l)));
    fail("catalog entries without documentation", &errors);
}

// ---------------------------------------------------------------------------
// The harness itself, on fixtures
// ---------------------------------------------------------------------------

#[test]
fn harness_accepts_good_fixtures() {
    let root = fixtures().join("good");
    let files = markdown_files(&root.join("docs"));
    assert!(files.len() >= 3, "fixtures: {:?}", files);
    let r = check_examples(&root, &files);
    fail("good fixtures", &r.errors);
    assert_eq!(r.examples, 4);
    assert!(r.uris >= 6, "{} uris", r.uris);
    fail("good fixture links", &check_links(&root, &files));
    // the components fixture page covers `timer` itself and `cron` on its Also line
    let missing = missing_pages(&root.join("docs/components"), &["timer".into(), "cron".into(), "kafka".into()]);
    assert_eq!(missing, vec!["kafka".to_string()]);
}

#[test]
fn harness_rejects_bad_fixtures() {
    let root = fixtures().join("bad");
    let files = markdown_files(&root.join("docs"));
    let r = check_examples(&root, &files);
    let all = r.errors.join("\n");
    let expect = [
        "docs/components/broken.md:5: ```yaml block without",
        "example id 'dup-id' is already used",
        "[bad-yaml]: invalid YAML",
        "[not-a-route]: not a Camel YAML DSL document",
        "[unknown-step]: the YAML DSL loader rejects it",
        "[unknown-option]: endpoint `timer:tick?perod=1000` does not resolve",
        "[no-component]: endpoint `nosuchscheme:x` does not resolve",
    ];
    for e in expect {
        assert!(all.contains(e), "expected an error containing {:?} in:\n{}", e, all);
    }
    assert_eq!(r.errors.len(), expect.len(), "{}", all);
    let links = check_links(&root, &files).join("\n");
    assert!(links.contains("link target `missing.md` does not exist"), "{}", links);
    assert!(links.contains("link target `../guides/nope.md#x` does not exist"), "{}", links);
    assert!(!links.contains("code.md"), "links in code are ignored: {}", links);
    assert_eq!(links.lines().count(), 2, "{}", links);
}

#[test]
fn scanner_units() {
    assert_eq!(substitute_env("kafka:t?brokers={{env:KAFKA_BROKERS:localhost:9092}}&x={{env:Y}}"), "kafka:t?brokers=localhost:9092&x=cGxhY2Vob2xkZXI=");
    assert_eq!(also_names("**Scheme:** `http:` · **Also:** `https:`, `http2` · **Crate:** `x`"), vec!["https", "http2"]);
    assert_eq!(marker_of("<!-- example: kafka-consume -->"), Marker::Example("kafka-consume".into()));
    assert_eq!(marker_of("  <!-- not-camel -->"), Marker::NotCamel);
    assert_eq!(marker_of("<!-- example: -->"), Marker::None);
    assert!(is_external("https://camel.apache.org/x") && is_external("mailto:a@b") && !is_external("../a.md") && !is_external("a.md#x:y"));
    let links = link_targets("See [a](a.md) and ![img](i/p.png \"t\") `[no](x.md)`\n[ref]: <b.md>\n```\n[no](c.md)\n```\n");
    assert_eq!(links, vec![(1, "a.md".to_string()), (1, "i/p.png".to_string()), (2, "b.md".to_string())]);
    let doc = cameleer_yaml::parse_yaml("- route:\n    from:\n      uri: timer:t\n      parameters:\n        period: 5\n      steps:\n        - to: log:x\n        - toD: \"log:${header.a}\"\n        - to:\n            uri: \"seda:q?size={{env:S:10}}\"\n").unwrap();
    assert_eq!(resolvable_uris(&doc), vec!["timer:t?period=5".to_string(), "log:x".to_string(), "seda:q?size=10".to_string()]);
}

/// Every docs page ends with the "Found a problem?" section that asks readers and coding
/// agents to report and fix what they find (issue, fork, fix, test, pull request).
#[test]
fn docs_pages_invite_fixes() {
    let root = repo_root();
    let mut missing = Vec::new();
    for f in markdown_files(&root.join("docs")) {
        // the Read the Docs entry page includes docs/README.md, which has the section
        if f.file_name().and_then(|n| n.to_str()) == Some("index.md") && f.parent() == Some(&root.join("docs")) {
            continue;
        }
        let text = std::fs::read_to_string(&f).unwrap();
        let ok = text.contains("## Found a problem?") && text.contains("https://github.com/pmsudhi/cameleer/issues/new") && text.contains("pull request");
        if !ok {
            missing.push(rel(&root, &f));
        }
    }
    fail("pages without the \"Found a problem?\" section", &missing);
}

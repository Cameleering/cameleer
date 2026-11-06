//! Tests for the YAML DSL pages: `docs/guides/yaml-dsl.md`, `docs/guides/yaml-for-coding-agents.md`
//! and `docs/guides/validate.md`.
//!
//! Every ```yaml block of these pages is preceded by `<!-- example: <id> -->` (or
//! `<!-- not-camel -->` for Kamelet, Pipe and CI files). Each test takes the block out of the
//! markdown (the YAML is never copied here), resolves every endpoint URI it uses with
//! `ctx.get_endpoint` on a context with every component (unknown options fail, as in Apache
//! Camel), then runs it end to end: messages go in through `direct:`, a timer, HTTP or files,
//! and the test checks what reaches the `mock:` endpoints, the HTTP reply or the output folder.
//! The loader error messages quoted on the pages are checked too.

// needs every component family (the default features)
#![cfg(feature = "full")]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cameleer_core::prelude::*;

// ---------------------------------------------------------------------------
// Markdown helpers
// ---------------------------------------------------------------------------

const YAML_DSL: &str = "yaml-dsl.md";
const AGENTS: &str = "yaml-for-coding-agents.md";
const VALIDATE: &str = "validate.md";
const PAGES: &[&str] = &[YAML_DSL, AGENTS, VALIDATE];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn page(name: &str) -> String {
    let p = root().join("docs/guides").join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {}", p.display(), e)).replace("\r\n", "\n")
}

/// A fenced block: language, the marker line directly above it, the text.
struct Block {
    lang: String,
    marker: String,
    text: String,
}

fn blocks(text: &str) -> Vec<Block> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i].trim_start();
        if let Some(lang) = l.strip_prefix("```") {
            let marker = if i > 0 { lines[i - 1].trim().to_string() } else { String::new() };
            let mut body = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                body.push(lines[i]);
                i += 1;
            }
            out.push(Block { lang: lang.trim().to_string(), marker, text: body.join("\n") + "\n" });
        }
        i += 1;
    }
    out
}

fn marker_id(marker: &str) -> Option<String> {
    marker.strip_prefix("<!-- example:").and_then(|r| r.strip_suffix("-->")).map(|s| s.trim().to_string())
}

/// The YAML block marked `<!-- example: id -->` on a page.
fn example(page_name: &str, id: &str) -> String {
    blocks(&page(page_name))
        .into_iter()
        .find(|b| marker_id(&b.marker).as_deref() == Some(id))
        .map(|b| b.text)
        .unwrap_or_else(|| panic!("no example '{}' in {}", id, page_name))
}

/// The `<!-- not-camel -->` YAML block of a page that contains `needle`.
fn not_camel(page_name: &str, needle: &str) -> String {
    blocks(&page(page_name))
        .into_iter()
        .find(|b| b.marker == "<!-- not-camel -->" && b.text.contains(needle))
        .map(|b| b.text)
        .unwrap_or_else(|| panic!("no not-camel block containing {:?} in {}", needle, page_name))
}

/// Replace `{{env:NAME}}` / `{{env:NAME:default}}` placeholders whose NAME is in `vars`.
fn subst(text: &str, vars: &[(&str, &str)]) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{env:") {
        out.push_str(&rest[..start]);
        let end = start + rest[start..].find("}}").expect("unclosed placeholder") + 2;
        let name = rest[start + 6..end - 2].split(':').next().unwrap();
        match vars.iter().find(|(n, _)| *n == name) {
            Some((_, v)) => out.push_str(v),
            None => out.push_str(&rest[start..end]),
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// `{{env:NAME:default}}` becomes `default` (the value the docs harness resolves with).
fn env_defaults(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(p) = rest.find("{{env:") {
        out.push_str(&rest[..p]);
        let after = &rest[p + 6..];
        let q = after.find("}}").expect("unclosed placeholder");
        out.push_str(after[..q].split_once(':').map(|(_, d)| d).unwrap_or("placeholder"));
        rest = &after[q + 2..];
    }
    out.push_str(rest);
    out
}

const URI_KEYS: &[&str] = &["from", "to", "toD", "inOnly", "inOut", "wireTap", "enrich", "pollEnrich", "poll", "deadLetterUri"];

/// Endpoint URIs of a parsed YAML DSL document: `uri:` with its `parameters:`, and string values
/// of the endpoint keys.
fn collect_uris(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            if let Some(Value::String(u)) = m.get("uri") {
                out.push(cameleer_yaml::build_uri(u, m.get("parameters")).unwrap_or_else(|_| u.clone()));
            }
            for (k, val) in m {
                if let Value::String(s) = val {
                    if URI_KEYS.contains(&k.as_str()) {
                        out.push(s.clone());
                    }
                }
                if k != "parameters" {
                    collect_uris(val, out);
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect_uris(x, out)),
        _ => {}
    }
}

/// Resolve every static endpoint URI of a YAML document with all components registered and
/// `props` as properties. Returns the number of URIs resolved.
fn resolve_all(name: &str, yaml: &str, props: &[(&str, &str)]) -> usize {
    let docs = cameleer_yaml::parse_yaml_all(yaml).unwrap_or_else(|e| panic!("{}: invalid YAML: {}", name, e));
    let mut raw = Vec::new();
    for d in &docs {
        collect_uris(d, &mut raw);
    }
    let main = cameleer_main::new_main();
    let ctx = main.context();
    for (k, v) in props {
        ctx.properties().set(*k, *v);
    }
    let mut n = 0;
    for u in raw {
        if u.contains("${") || !u.contains(':') {
            continue;
        }
        let u = env_defaults(&u);
        // route template parameters ({{name}}) are not properties
        let Ok(resolved) = ctx.resolve_placeholders(&u) else { continue };
        if let Err(e) = ctx.get_endpoint(&resolved) {
            panic!("{}: endpoint `{}` does not resolve: {}", name, resolved, e);
        }
        n += 1;
    }
    n
}

// ---------------------------------------------------------------------------
// Running helpers
// ---------------------------------------------------------------------------

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn wait_for(what: &str, f: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {}", what);
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn mock(ctx: &CamelContext, uri: &str) -> MockEndpoint {
    ctx.mock_endpoint(uri).unwrap()
}

fn received(ctx: &CamelContext, uri: &str) -> Vec<Exchange> {
    mock(ctx, uri).received_exchanges()
}

fn bodies(ctx: &CamelContext, uri: &str) -> Vec<String> {
    received(ctx, uri).iter().map(|e| e.body().to_string()).collect()
}

fn wait_bodies(ctx: &CamelContext, uri: &str, n: usize) -> Vec<String> {
    wait_for(&format!("{} messages on {}", n, uri), || received(ctx, uri).len() >= n);
    bodies(ctx, uri)
}

fn header_of(ex: &Exchange, name: &str) -> String {
    ex.header(name).map(|v| v.to_string()).unwrap_or_default()
}

fn template(ctx: &CamelContext) -> ProducerTemplate {
    ctx.create_producer_template()
}

/// Resolve the endpoints of a YAML example, then start it (plus `extra` route sources) with
/// every component. `configure` can set properties before the start.
fn start_with(id: &str, yaml: &str, extra: &[(&str, String)], configure: impl FnOnce(&mut cameleer_main::Main)) -> cameleer_main::Main {
    assert!(resolve_all(id, yaml, &[]) > 0, "{}: no endpoint URI found", id);
    let mut main = cameleer_main::new_main();
    configure(&mut main);
    main.add_route_source(format!("{}.camel.yaml", id), yaml.to_string());
    for (name, text) in extra {
        main.add_route_source(name.to_string(), text.clone());
    }
    main.start().unwrap_or_else(|e| panic!("{}: {}", id, e));
    main
}

fn start(page_name: &str, id: &str) -> cameleer_main::Main {
    start_with(id, &example(page_name, id), &[], |_| {})
}

/// A minimal HTTP/1.1 request; returns (status, body).
fn http(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> (u16, String) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut s = loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(s) => break s,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => panic!("connect {}: {}", port, e),
        }
    };
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let mut req = format!("{} {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n", method, path, body.len());
    for (k, v) in headers {
        req.push_str(&format!("{}: {}\r\n", k, v));
    }
    req.push_str("\r\n");
    req.push_str(body);
    s.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let status = out.split(' ').nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    let body = out.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
    (status, body)
}

/// A fresh, empty temporary directory (forward slashes, so it can go into YAML strings).
fn temp_dir(name: &str) -> String {
    let p = std::env::temp_dir().join(format!("cameleer-docs-yaml-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p.to_string_lossy().replace('\\', "/")
}

// ---------------------------------------------------------------------------
// Page-wide checks
// ---------------------------------------------------------------------------

/// Every yaml block is marked, ids are unique, marked blocks are lists of top-level elements,
/// and every example id has a test in this file.
#[test]
fn every_yaml_block_is_marked_unique_and_tested() {
    let me = include_str!("docs_yaml.rs");
    let mut ids = std::collections::BTreeSet::new();
    for p in PAGES {
        for b in blocks(&page(p)) {
            if b.lang != "yaml" {
                continue;
            }
            if b.marker == "<!-- not-camel -->" {
                continue;
            }
            let id = marker_id(&b.marker).unwrap_or_else(|| panic!("{}: a yaml block has no <!-- example: id --> marker:\n{}", p, b.text));
            assert!(ids.insert(id.clone()), "{}: duplicate example id {}", p, id);
            match cameleer_yaml::parse_yaml_all(&b.text) {
                Ok(docs) => assert!(docs.iter().all(|d| matches!(d, Value::Array(a) if !a.is_empty())), "{}: {} is not a list of elements", p, id),
                Err(e) => panic!("{}: example {} does not parse: {}", p, id, e),
            }
            // used by a test: `example(PAGE, "<id>")` or `start(PAGE, "<id>")`
            let needle = format!(", \"{}\")", id);
            assert!(me.contains(&needle), "{}: example {} has no test in docs_yaml.rs", p, id);
        }
    }
    assert!(ids.len() >= 33, "{} examples", ids.len());
    println!("{} examples", ids.len());
}

/// Each page answers first (a blockquote under the H1), has common questions and ends with the
/// "Found a problem?" section.
#[test]
fn pages_follow_the_docs_template() {
    for p in PAGES {
        let text = page(p);
        let mut lines = text.lines().filter(|l| !l.trim().is_empty());
        assert!(lines.next().unwrap().starts_with("# "), "{}: H1 first", p);
        assert!(lines.next().unwrap().starts_with("> "), "{}: answer-first blockquote under the H1", p);
        assert!(text.contains("\n## Common questions\n"), "{}: Common questions", p);
        let found = text.find("\n## Found a problem?\n").unwrap_or_else(|| panic!("{}: Found a problem? section", p));
        assert!(!text[found + 5..].contains("\n## "), "{}: Found a problem? must be the last section", p);
        assert!(text.contains("https://github.com/pmsudhi/cameleer/issues/new") && text.contains("pull request"), "{}", p);
    }
}

/// AGENTS.md points to existing files.
#[test]
fn agents_md_links_exist() {
    let text = std::fs::read_to_string(root().join("AGENTS.md")).unwrap();
    let mut n = 0;
    let mut rest = text.as_str();
    while let Some(p) = rest.find("](") {
        let after = &rest[p + 2..];
        let end = after.find(')').unwrap();
        let target = &after[..end];
        if !target.starts_with("http") {
            assert!(root().join(target).exists(), "AGENTS.md links to missing {}", target);
            n += 1;
        }
        rest = &after[end..];
    }
    assert!(n >= 5, "{} relative links", n);
    for needle in ["yaml-for-coding-agents.md", "yaml-dsl.md", "validate.md", "cameleer validate", "cargo test --workspace", "Standard library only", "issues/new"] {
        assert!(text.contains(needle), "AGENTS.md mentions {}", needle);
    }
}

/// The loader messages quoted in "Common mistakes" and elsewhere are the real ones.
#[test]
fn quoted_loader_errors_are_real() {
    let cases: &[(&str, &str)] = &[
        ("- route:\n    from:\n      uri: direct:c\n  steps:\n    - to: mock:d\n", "Each top-level element must have exactly one key"),
        (
            "- route:\n    from:\n      uri: direct:e\n      steps:\n        - filter:\n            simple: \"${body} == 'x'\"\n          steps:\n            - to: mock:f\n",
            "A step must have exactly one key",
        ),
        ("- route:\n    from:\n      uri: direct:a\n      steps:\n        - to: mock:a\n          pattern: InOut\n", "A step must have exactly one key"),
        ("- route:\n    autoStartup: yes\n    from:\n      uri: direct:a\n", "'yes' is not a boolean"),
        (
            "- route:\n    from:\n      uri: direct:a\n      steps:\n        - setBody:\n            simple: a\n            constant: b\n",
            "only one expression language may be specified",
        ),
        ("- rout:\n    from:\n      uri: direct:a\n", "Unknown top-level element 'rout'"),
        ("- route:\n    from:\n      uri: direct:a\n      steps:\n        - setbody:\n            simple: a\n", "Unknown step 'setbody'"),
        (
            "- routeTemplate:\n    id: sensor\n    parameters:\n      - name: room\n    from:\n      uri: \"direct:{{room}}\"\n- templatedRoute:\n    routeTemplateRef: sensor\n",
            "Route template sensor the following mandatory parameters are missing: room",
        ),
        (
            "- route:\n    routeConfigurationId: nope\n    from:\n      uri: direct:a\n",
            "Cannot find routeConfiguration with id 'nope'",
        ),
    ];
    for (yaml, expected) in cases {
        let main = cameleer_main::new_main();
        let err = cameleer_yaml::add_routes_from_yaml(main.context(), yaml).expect_err(expected);
        assert!(err.message.contains(expected), "expected {:?} in: {}", expected, err.message);
    }
    // line and column in error locations
    let main = cameleer_main::new_main();
    let err = cameleer_yaml::load_routes(main.context(), "- route:\n    from:\n      uri: direct:a\n      steps:\n        - to: mock:a\n        - setbody: x\n").unwrap_err();
    assert!(err.message.contains("line 6"), "{}", err.message);
    // `disabled: yes` is a string, not a boolean: the step is not disabled
    let main = cameleer_main::new_main();
    let routes = cameleer_yaml::load_routes(
        main.context(),
        "- route:\n    from:\n      uri: direct:a\n      steps:\n        - to:\n            uri: mock:a\n            disabled: yes\n        - to:\n            uri: mock:b\n            disabled: true\n",
    )
    .unwrap();
    assert_eq!(routes[0].outputs.len(), 1);
    // unknown route keys become route properties (silently)
    let routes = cameleer_yaml::load_routes(main.context(), "- route:\n    autostartup: false\n    from:\n      uri: direct:x\n").unwrap();
    assert_eq!(routes[0].properties.get("autostartup").map(String::as_str), Some("false"));
    assert!(routes[0].auto_startup);
}

// ---------------------------------------------------------------------------
// yaml-dsl.md
// ---------------------------------------------------------------------------

#[test]
fn yaml_first_look() {
    let main = start(YAML_DSL, "yaml-first-look");
    assert_eq!(wait_bodies(main.context(), "mock:greetings", 1), vec!["Hello from hello"]);
    main.stop().unwrap();
}

#[test]
fn yaml_multi_document() {
    let main = start(YAML_DSL, "yaml-multi-document");
    let ctx = main.context();
    assert!(ctx.route("intake").is_some() && ctx.route("store").is_some());
    template(ctx).send_body("direct:intake", "x").unwrap();
    assert_eq!(bodies(ctx, "mock:stored"), vec!["stored x"]);
    main.stop().unwrap();
}

#[test]
fn yaml_route_options() {
    let main = start(YAML_DSL, "yaml-route-options");
    let ctx = main.context();
    // created but not started
    assert!(matches!(ctx.route_status("nightly-report"), Some(ServiceStatus::Initialized | ServiceStatus::Stopped)));
    assert_eq!(ctx.route_status("always-on"), Some(ServiceStatus::Started));
    assert_eq!(template(ctx).request_body("direct:ping", "").unwrap().to_string(), "pong");
    main.stop().unwrap();
}

#[test]
fn yaml_from_short_form() {
    let main = start(YAML_DSL, "yaml-from-short-form");
    let ctx = main.context();
    assert_eq!(template(ctx).request_body("direct:short", "a").unwrap().to_string(), "short form: a");
    assert_eq!(bodies(ctx, "mock:short"), vec!["short form: a"]);
    main.stop().unwrap();
}

#[test]
fn yaml_route_configuration() {
    let main = start(YAML_DSL, "yaml-route-configuration");
    let ctx = main.context();
    let t = template(ctx);
    assert_eq!(t.request_body("direct:create-order", "").unwrap().to_string(), "rejected: empty order");
    assert_eq!(bodies(ctx, "mock:rejected"), vec!["rejected: empty order"]);
    assert_eq!(t.request_body("direct:create-order", "A-1").unwrap().to_string(), "created A-1");
    wait_for("the audit copy", || bodies(ctx, "mock:audit").contains(&"created A-1".to_string()));
    main.stop().unwrap();
}

#[test]
fn yaml_error_handler() {
    let main = start(YAML_DSL, "yaml-error-handler");
    let ctx = main.context();
    template(ctx).send_body("direct:charge", "card-1").unwrap();
    // the original message, after one call and two redeliveries
    assert_eq!(wait_bodies(ctx, "mock:dead-letters", 1), vec!["card-1"]);
    main.stop().unwrap();
}

#[test]
fn yaml_route_template() {
    let main = start(YAML_DSL, "yaml-route-template");
    let ctx = main.context();
    assert!(ctx.route("kitchen-sensor").is_some() && ctx.route("lab-sensor").is_some());
    let t = template(ctx);
    t.send_body("direct:kitchen", "21").unwrap();
    t.send_body("direct:lab", "300").unwrap();
    assert_eq!(bodies(ctx, "mock:readings"), vec!["kitchen: 21 C", "lab: 300 K"]);
    main.stop().unwrap();
}

#[test]
fn yaml_rest() {
    let port = free_port();
    let id = "yaml-rest";
    let main = start_with(id, &example(YAML_DSL, "yaml-rest"), &[], |m| m.set_property("camel.server.port", port.to_string()));
    let (status, body) = http(port, "GET", "/api/orders/42", &[], "");
    assert_eq!((status, body.as_str()), (200, r#"{"id": "42", "status": "shipped"}"#));
    let (status, body) = http(port, "POST", "/api/orders", &[], "o1");
    assert_eq!((status, body.as_str()), (201, "created o1"));
    main.stop().unwrap();
}

#[test]
fn yaml_beans() {
    let main = start(YAML_DSL, "yaml-beans");
    let ctx = main.context();
    let bean = cameleer_yaml::yaml_state(ctx).bean("greeter").expect("the bean definition is recorded");
    assert_eq!(bean.type_name.as_deref(), Some("com.acme.Greeter"));
    assert!(!bean.resolved, "no bean factory is registered in the cameleer binary");
    template(ctx).send_body("direct:beans", "b").unwrap();
    assert_eq!(bodies(ctx, "mock:beans"), vec!["b"]);
    main.stop().unwrap();
}

#[test]
fn yaml_kamelet_source() {
    let main = start(YAML_DSL, "yaml-kamelet-source");
    let ctx = main.context();
    assert_eq!(wait_bodies(ctx, "mock:from-kamelet", 1), vec!["Hello from a Kamelet"]);
    assert_eq!(header_of(&received(ctx, "mock:from-kamelet")[0], "origin"), "kamelet");
    main.stop().unwrap();
}

#[test]
fn yaml_kamelet_resource() {
    let kamelet = not_camel(YAML_DSL, "kind: Kamelet");
    let route = "- route:\n    from:\n      uri: kamelet:greeting-source?message=Hi&period=50\n      steps:\n        - to: mock:greeting\n";
    let mut main = cameleer_main::new_main();
    main.add_route_source("greeting-source.kamelet.yaml", kamelet);
    main.add_route_source("use-greeting.camel.yaml", route);
    main.start().unwrap();
    assert_eq!(wait_bodies(main.context(), "mock:greeting", 1)[0], "Hi");
    main.stop().unwrap();
}

#[test]
fn yaml_pipe_resource() {
    let pipe = not_camel(YAML_DSL, "kind: Pipe");
    let mut main = cameleer_main::new_main();
    main.add_route_source("greetings.pipe.yaml", pipe);
    main.start().unwrap();
    let ctx = main.context();
    assert!(ctx.route("greetings-pipe").is_some());
    assert_eq!(wait_bodies(ctx, "mock:piped", 1), vec!["piped: Hello from a Pipe"]);
    main.stop().unwrap();
}

#[test]
fn yaml_endpoint_forms() {
    let main = start(YAML_DSL, "yaml-endpoint-forms");
    let ctx = main.context();
    template(ctx).send_body("direct:forms", "x").unwrap();
    assert_eq!(bodies(ctx, "mock:forms"), vec!["x"]);
    main.stop().unwrap();
}

#[test]
fn yaml_endpoint_raw() {
    let main = start(YAML_DSL, "yaml-endpoint-raw");
    let ctx = main.context();
    template(ctx).send_body("direct:raw", "x").unwrap();
    assert_eq!(bodies(ctx, "mock:raw"), vec!["x"]);
    main.stop().unwrap();
    // `parameters` values are URL-encoded; `{{..}}`, `${..}` and RAW(..) values are not
    let params = cameleer_yaml::parse_yaml("marker: a+b&c\nsize: \"{{q.size}}\"\npw: RAW(a+b)\nlist: [a, b]\n").unwrap();
    let uri = cameleer_yaml::build_uri("log:audit-mapped", Some(&params)).unwrap();
    assert_eq!(uri, "log:audit-mapped?marker=a%2Bb%26c&size={{q.size}}&pw=RAW(a+b)&list=a%2Cb");
}

#[test]
fn yaml_placeholders() {
    let yaml = example(YAML_DSL, "yaml-placeholders");
    let main = start_with("yaml-placeholders", &yaml, &[], |m| m.set_property("greeting", "Hi"));
    let site = std::env::var("SITE_NAME").unwrap_or_else(|_| "the lab".into());
    assert_eq!(wait_bodies(main.context(), "mock:configured", 1), vec![format!("Hi from {}", site)]);
    assert_eq!(main.context().resolve_placeholders("{{broker:localhost:9092}}").unwrap(), "localhost:9092");
    main.stop().unwrap();
}

#[test]
fn yaml_sending() {
    let main = start(YAML_DSL, "yaml-sending");
    let ctx = main.context();
    template(ctx).send_body_and_header("direct:dispatch", "pen", "region", "eu").unwrap();
    assert_eq!(bodies(ctx, "mock:region-eu"), vec!["pen costs 42"]);
    assert_eq!(wait_bodies(ctx, "mock:audit-copy", 1), vec!["pen"]);
    main.stop().unwrap();
}

#[test]
fn yaml_expression_forms() {
    let main = start(YAML_DSL, "yaml-expression-forms");
    let ctx = main.context();
    template(ctx).send_body("direct:forms-of-expressions", "abc").unwrap();
    let ex = &received(ctx, "mock:expressions")[0];
    assert_eq!(header_of(ex, "short"), "ABC");
    assert_eq!(ex.header("typed"), Some(&Value::Int(3)));
    assert_eq!(header_of(ex, "wrapped"), "fixed");
    assert_eq!(header_of(ex, "plain"), "plain abc");
    assert_eq!(ex.body().to_string(), "Hello abc");
    main.stop().unwrap();
}

#[test]
fn yaml_languages() {
    let main = start(YAML_DSL, "yaml-languages");
    let ctx = main.context();
    template(ctx).send_body("direct:languages", r#"{"items":[{"sku":"A1"},{"sku":"B2"}]}"#).unwrap();
    let ex = &received(ctx, "mock:languages")[0];
    assert_eq!(header_of(ex, "firstSku"), "A1");
    assert_eq!(header_of(ex, "count"), "2");
    assert_eq!(header_of(ex, "from"), "sensor-7");
    assert_eq!(header_of(ex, "copy"), "A1");
    main.stop().unwrap();
}

#[test]
fn yaml_message_steps() {
    let main = start(YAML_DSL, "yaml-message-steps");
    let ctx = main.context();
    let order = Value::object([("customer", Value::from("Ada")), ("total", Value::from(30))]);
    template(ctx).send_body("direct:message", order).unwrap();
    let ex = &received(ctx, "mock:message")[0];
    assert_eq!(ex.body().to_string(), "Dear Ada, total 30");
    assert_eq!(header_of(ex, "customer"), "Ada");
    assert!(ex.header("tmpTrace").is_none(), "removeHeaders removed tmpTrace");
    main.stop().unwrap();
}

#[test]
fn yaml_choice() {
    let main = start(YAML_DSL, "yaml-choice");
    let ctx = main.context();
    let t = template(ctx);
    for (body, p) in [("outage", "P1"), ("bug", "P2"), ("question", "P3")] {
        t.send_body_and_header("direct:tickets", body, "priority", p).unwrap();
    }
    assert_eq!(bodies(ctx, "mock:page-oncall"), vec!["outage"]);
    assert_eq!(bodies(ctx, "mock:team-queue"), vec!["bug"]);
    assert_eq!(bodies(ctx, "mock:backlog"), vec!["question"]);
    assert_eq!(bodies(ctx, "mock:escalated"), vec!["outage", "bug"]);
    main.stop().unwrap();
}

#[test]
fn yaml_multicast_loop() {
    let main = start(YAML_DSL, "yaml-multicast-loop");
    let ctx = main.context();
    let t = template(ctx);
    t.send_body("direct:fan-out", "hi").unwrap();
    let fanned = received(ctx, "mock:fanned-out");
    assert_eq!(fanned[0].body(), &Value::Array(vec![Value::from("email:hi"), Value::from("sms:hi")]));
    assert_eq!(bodies(ctx, "mock:loop"), vec!["attempt 0", "attempt 1", "attempt 2"]);
    t.send_body("direct:fan-out", "again").unwrap();
    assert_eq!(received(ctx, "mock:node-a").len(), 1);
    assert_eq!(received(ctx, "mock:node-b").len(), 1);
    main.stop().unwrap();
}

#[test]
fn yaml_split_aggregate() {
    let main = start(YAML_DSL, "yaml-split-aggregate");
    let ctx = main.context();
    let t = template(ctx);
    t.send_body("direct:csv-lines", "a\nb").unwrap();
    assert_eq!(bodies(ctx, "mock:each-line"), vec!["A", "B"]);
    assert_eq!(received(ctx, "mock:all-lines")[0].body(), &Value::Array(vec![Value::from("A"), Value::from("B")]));
    t.send_body_and_header("direct:readings", "20", "device", "t1").unwrap();
    t.send_body_and_header("direct:readings", "21", "device", "t1").unwrap();
    wait_for("the device batch", || received(ctx, "mock:device-batch").len() == 1);
    assert_eq!(received(ctx, "mock:device-batch")[0].body(), &Value::Array(vec![Value::from("20"), Value::from("21")]));
    main.stop().unwrap();
}

#[test]
fn yaml_error_steps() {
    let main = start(YAML_DSL, "yaml-error-steps");
    let ctx = main.context();
    let t = template(ctx);
    for body in ["good", "bad", "boom", "offline"] {
        t.send_body("direct:fragile", body).unwrap();
    }
    assert_eq!(bodies(ctx, "mock:checked"), vec!["good", "replaced a bad message", "boom", "offline"]);
    assert_eq!(bodies(ctx, "mock:fragile-done"), vec!["good", "replaced a bad message", "fallback answer"]);
    assert_eq!(bodies(ctx, "mock:retry-later"), vec!["service unavailable, queued"]);
    main.stop().unwrap();
}

#[test]
fn yaml_flow_control() {
    let main = start(YAML_DSL, "yaml-flow-control");
    let ctx = main.context();
    let t = template(ctx);
    for (body, id) in [("first", "e1"), ("duplicate", "e1"), ("second", "e2")] {
        t.send_body_and_header("direct:events", body, "eventId", id).unwrap();
    }
    assert_eq!(bodies(ctx, "mock:unique-events"), vec!["first", "second"]);
    main.stop().unwrap();
}

#[test]
fn yaml_polling() {
    let main = start(YAML_DSL, "yaml-polling");
    let ctx = main.context();
    let t = template(ctx);
    t.send_body("seda:latest-config", "v2").unwrap();
    t.send_body("direct:with-config", "call").unwrap();
    assert_eq!(bodies(ctx, "mock:configured-call"), vec!["config is v2"]);
    t.send_body("seda:jobs", "job-1").unwrap();
    t.send_body("direct:next-job", "ignored").unwrap();
    assert_eq!(bodies(ctx, "mock:job"), vec!["job-1"]);
    main.stop().unwrap();
}

#[test]
fn yaml_message_more() {
    let main = start(YAML_DSL, "yaml-message-more");
    let ctx = main.context();
    let list = Value::Array(vec![Value::from("pear"), Value::from("apple"), Value::from("fig")]);
    template(ctx).send_body_and_header("direct:more-message-steps", list, "weight", "12").unwrap();
    assert_eq!(bodies(ctx, "mock:weight"), vec!["12 kg"]);
    let w = &received(ctx, "mock:weight")[0];
    assert_eq!(w.header("weight"), Some(&Value::Int(12)));
    let sorted = &received(ctx, "mock:sorted")[0];
    assert_eq!(sorted.body(), &Value::Array(vec![Value::from("apple"), Value::from("fig"), Value::from("pear")]));
    main.stop().unwrap();
}

#[test]
fn yaml_dynamic_routing() {
    let main = start(YAML_DSL, "yaml-dynamic-routing");
    let ctx = main.context();
    let t = template(ctx);
    t.send_body_and_header("direct:notify", "deploy done", "channels", "mock:email,mock:sms").unwrap();
    assert_eq!(bodies(ctx, "mock:email"), vec!["deploy done"]);
    assert_eq!(bodies(ctx, "mock:sms"), vec!["deploy done"]);
    t.send_body_and_header("direct:photo", "cat", "slip", "direct:resize,mock:photos").unwrap();
    assert_eq!(bodies(ctx, "mock:photos"), vec!["cat-resized"]);
    main.stop().unwrap();
}

#[test]
fn yaml_resequence() {
    let main = start(YAML_DSL, "yaml-resequence");
    let ctx = main.context();
    let t = template(ctx);
    for (body, seq) in [("c", 3), ("a", 1), ("b", 2)] {
        t.send_body_and_header("direct:out-of-order", body, "seq", seq).unwrap();
    }
    assert_eq!(wait_bodies(ctx, "mock:in-order", 3), vec!["a", "b", "c"]);
    main.stop().unwrap();
}

#[test]
fn yaml_intercept() {
    let main = start(YAML_DSL, "yaml-intercept");
    let ctx = main.context();
    let t = template(ctx);
    t.send_body("direct:intercepted", "ok").unwrap();
    assert_eq!(bodies(ctx, "mock:modern"), vec!["redirected ok"]);
    assert!(received(ctx, "mock:legacy-system").is_empty(), "skipSendToOriginalEndpoint");
    assert!(t.send_body("direct:intercepted", "fail").is_err());
    assert_eq!(wait_bodies(ctx, "mock:failures", 1).len(), 1);
    main.stop().unwrap();
}

// ---------------------------------------------------------------------------
// yaml-for-coding-agents.md
// ---------------------------------------------------------------------------

#[test]
fn agent_consume_transform() {
    let inbox = temp_dir("inbox");
    let outbox = temp_dir("outbox");
    let yaml = subst(&example(AGENTS, "agent-consume-transform"), &[("INBOX_DIR", &inbox), ("OUTBOX_DIR", &outbox)]);
    std::fs::write(format!("{}/o1.json", inbox), r#"{"id": "A-1", "total": 30}"#).unwrap();
    std::fs::write(format!("{}/notes.txt", inbox), "not an order").unwrap();
    let main = start_with("agent-consume-transform", &yaml, &[], |_| {});
    let out = format!("{}/A-1.json", outbox);
    wait_for("the enriched order file", || std::fs::read_to_string(&out).map(|s| s.contains("received")).unwrap_or(false));
    assert_eq!(std::fs::read_to_string(&out).unwrap(), r#"{"id": "A-1", "status": "received"}"#);
    assert!(Path::new(&format!("{}/notes.txt", inbox)).exists(), "include: only .json files are consumed");
    main.stop().unwrap();
}

#[test]
fn agent_rest_api() {
    let port = free_port();
    let main = start_with("agent-rest-api", &example(AGENTS, "agent-rest-api"), &[], |m| m.set_property("camel.server.port", port.to_string()));
    let (status, body) = http(port, "GET", "/api/devices/d1", &[], "");
    assert_eq!((status, body.as_str()), (200, r#"{"id": "d1", "online": true}"#));
    let (status, body) = http(port, "POST", "/api/devices", &[], "sensor-9");
    assert_eq!((status, body.as_str()), (201, "registered sensor-9"));
    main.stop().unwrap();
}

#[test]
fn agent_scheduled_job() {
    let port = free_port();
    let url = format!("http://127.0.0.1:{}/health", port);
    let yaml = subst(&example(AGENTS, "agent-scheduled-job"), &[("STATUS_URL", &url), ("STATUS_SINK", "mock:status")]);
    // the API the job polls
    let api = "- rest:\n    path: /health\n    get:\n      - to: direct:health\n- route:\n    from:\n      uri: direct:health\n      steps:\n        - setBody:\n            constant: UP\n".to_string();
    let main = start_with("agent-scheduled-job", &yaml, &[("health-api.camel.yaml", api)], |m| {
        m.set_property("camel.server.port", port.to_string());
        m.set_property("status.period", "100");
    });
    assert_eq!(wait_bodies(main.context(), "mock:status", 2)[..2], ["UP".to_string(), "UP".to_string()]);
    main.stop().unwrap();
}

#[test]
fn agent_error_handling() {
    let yaml = subst(&example(AGENTS, "agent-error-handling"), &[("DEAD_LETTER_URI", "mock:dead-letters")]);
    let main = start_with("agent-error-handling", &yaml, &[], |_| {});
    let ctx = main.context();
    let t = template(ctx);
    assert_eq!(t.request_body("direct:payment", "").unwrap().to_string(), "invalid input: empty payment");
    assert_eq!(t.request_body("direct:payment", "p1").unwrap().to_string(), "charged p1");
    assert!(received(ctx, "mock:dead-letters").is_empty());
    main.stop().unwrap();
}

#[test]
fn agent_worked_example() {
    let port = free_port();
    let dir = temp_dir("orders");
    let yaml = subst(&example(AGENTS, "agent-worked-example"), &[("ORDERS_DIR", &dir)]);
    let main = start_with("agent-worked-example", &yaml, &[], |m| m.set_property("camel.server.port", port.to_string()));
    let json = [("Content-Type", "application/json")];
    let (status, body) = http(port, "POST", "/orders", &json, r#"{"id": "A-1", "total": 30}"#);
    assert_eq!((status, body.as_str()), (202, r#"{"accepted": "A-1"}"#));
    let (status, body) = http(port, "POST", "/orders", &json, r#"{"total": 5}"#);
    assert_eq!((status, body.as_str()), (400, r#"{"error": "order id is required"}"#));
    let file = format!("{}/order-A-1.json", dir);
    wait_for("the order file", || std::fs::read_to_string(&file).map(|s| s.contains("A-1")).unwrap_or(false));
    let written = cameleer_core::json::parse(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(written.get("total"), Some(&Value::Int(30)));
    main.stop().unwrap();
    // the draft's option is rejected the way the validator reports it; the fix resolves
    let ctx = cameleer_main::new_main();
    let err = match ctx.context().get_endpoint("seda:orders?concurrentConsumer=4") {
        Ok(_) => panic!("concurrentConsumer is not a seda option"),
        Err(e) => e,
    };
    assert!(err.message.contains("concurrentConsumer"), "{}", err.message);
    ctx.context().get_endpoint("seda:orders?concurrentConsumers=4").unwrap();
}

// ---------------------------------------------------------------------------
// validate.md
// ---------------------------------------------------------------------------

#[test]
fn validate_clean_route() {
    let main = start(VALIDATE, "validate-clean-route");
    let got = wait_bodies(main.context(), "mock:billed", 1);
    assert!(got[0].starts_with("order of 20") && got[0].len() == "order of 2026-11-05".len(), "{:?}", got);
    main.stop().unwrap();
}

//! Tests for the guides in `docs/guides/` and the cookbook routes in `examples/agents/` and
//! `examples/edge/`.
//!
//! Every ```yaml block in a guide is preceded by `<!-- example: <id> -->`. Each test takes the
//! block out of the markdown (the YAML is never copied here), resolves every endpoint URI it
//! uses against a context with all components (unknown options fail, as in Apache Camel), and
//! runs it end to end against `direct:`/`mock:` endpoints, the embedded MQTT and Kafka brokers
//! or the OpenAI mock. The Rust DSL snippets of the guides are copied verbatim into this file
//! (`rust_snippets_are_copied_verbatim` checks it) and most of them run against the same
//! assertions as their YAML twin.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use cameleer_core::prelude::*;
use cameleer_core::template_routes::{TemplateParam, TemplatedRouteBuilder};

// ---------------------------------------------------------------------------
// Markdown helpers
// ---------------------------------------------------------------------------

const PAGES: &[&str] = &[
    "getting-started.md",
    "concepts.md",
    "eips.md",
    "eips/routing.md",
    "eips/split-aggregate.md",
    "eips/transformation.md",
    "eips/flow-control.md",
    "eips/system.md",
    "error-handling.md",
    "agents.md",
    "edge.md",
    "migrating-from-apache-camel.md",
    "faq.md",
];

fn root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn page(name: &str) -> String {
    let p = root().join("docs/guides").join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {}", p.display(), e)).replace("\r\n", "\n")
}

/// Every fenced block of a page: (language, marker id from the line above, text).
fn blocks(text: &str) -> Vec<(String, Option<String>, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i].trim_start();
        if let Some(lang) = l.strip_prefix("```") {
            let lang = lang.trim().to_string();
            let marker = if i > 0 {
                lines[i - 1].trim().strip_prefix("<!-- example:").and_then(|r| r.strip_suffix("-->")).map(|id| id.trim().to_string())
            } else {
                None
            };
            let mut body = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                body.push(lines[i]);
                i += 1;
            }
            out.push((lang, marker, body.join("\n") + "\n"));
        }
        i += 1;
    }
    out
}

/// The YAML block marked `<!-- example: id -->` on a page.
fn example(page_name: &str, id: &str) -> String {
    blocks(&page(page_name))
        .into_iter()
        .find(|(_, m, _)| m.as_deref() == Some(id))
        .map(|(_, _, t)| t)
        .unwrap_or_else(|| panic!("no example '{}' in {}", id, page_name))
}

/// A route file under `examples/`.
fn example_file(path: &str) -> String {
    let p = root().join("examples").join(path);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {}", p.display(), e)).replace("\r\n", "\n")
}

/// Replace `{{env:NAME}}` / `{{env:NAME:default}}` placeholders whose NAME is in `vars`.
fn subst(text: &str, vars: &[(&str, &str)]) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{env:") {
        out.push_str(&rest[..start]);
        let end = start + rest[start..].find("}}").expect("unclosed placeholder") + 2;
        let inner = &rest[start + 6..end - 2];
        let name = inner.split(':').next().unwrap();
        match vars.iter().find(|(n, _)| *n == name) {
            Some((_, v)) => out.push_str(v),
            None => out.push_str(&rest[start..end]),
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// Endpoint URIs used by a YAML route: `uri:`, `to:`, `toD:` (static ones), `wireTap:`,
/// `deadLetterUri:` values. Dynamic URIs (`${...}`) are skipped.
fn uris(yaml: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in yaml.lines() {
        let l = line.trim_start();
        if l.starts_with('#') {
            continue;
        }
        let l = l.strip_prefix("- ").unwrap_or(l);
        for key in ["uri:", "to:", "toD:", "wireTap:", "deadLetterUri:"] {
            if let Some(v) = l.strip_prefix(key) {
                let v = v.trim().trim_matches('"').trim_matches('\'');
                if !v.is_empty() && v.contains(':') && !v.contains("${") {
                    out.push(v.to_string());
                }
            }
        }
    }
    out
}

/// Resolve every endpoint of a YAML route with all components registered.
fn resolve_all(name: &str, yaml: &str) {
    let main = cameleer_main::new_main();
    for u in uris(yaml) {
        // route template parameters ({{name}}) are not properties: skip those URIs
        let Ok(resolved) = main.context().resolve_placeholders(&u) else { continue };
        if let Err(e) = main.context().get_endpoint(&resolved) {
            panic!("{}: endpoint {} does not resolve: {}", name, u, e);
        }
    }
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

fn bodies(ctx: &CamelContext, uri: &str) -> Vec<String> {
    mock(ctx, uri).received_exchanges().iter().map(|e| e.body().to_string()).collect()
}

fn wait_bodies(ctx: &CamelContext, uri: &str, n: usize) -> Vec<String> {
    wait_for(&format!("{} messages on {}", n, uri), || mock(ctx, uri).received_exchanges().len() >= n);
    bodies(ctx, uri)
}

fn header_of(ex: &Exchange, name: &str) -> String {
    ex.header(name).map(|v| v.to_string()).unwrap_or_default()
}

fn template(ctx: &CamelContext) -> ProducerTemplate {
    ctx.create_producer_template()
}

/// Start a YAML route (after resolving its endpoints) with every component.
fn start_yaml(name: &str, yaml: &str, configure: impl FnOnce(&mut cameleer_main::Main)) -> cameleer_main::Main {
    resolve_all(name, yaml);
    let mut main = cameleer_main::new_main();
    configure(&mut main);
    main.add_route_source(format!("{}.camel.yaml", name.replace('/', "-")), yaml.to_string());
    main.start().unwrap_or_else(|e| panic!("{}: {}", name, e));
    main
}

/// Run a guide example as YAML and its Rust DSL twin, each in a fresh context, against the
/// same assertions.
fn run_both(
    page_name: &str,
    id: &str,
    rust: impl FnOnce(&mut RouteBuilder) + 'static,
    check: impl Fn(&CamelContext),
) {
    run_both_ctx(page_name, id, move |ctx| ctx.add_routes(rust), check);
}

/// Like [`run_both`], with a Rust snippet that works on the context.
fn run_both_ctx(
    page_name: &str,
    id: &str,
    rust: impl FnOnce(&CamelContext) -> Result<()> + 'static,
    check: impl Fn(&CamelContext),
) {
    let yaml = example(page_name, id);
    let main = start_yaml(id, &yaml, |_| {});
    check(main.context());
    main.stop().unwrap();

    let mut main = cameleer_main::new_main();
    main.configure(rust);
    main.start().unwrap_or_else(|e| panic!("{} (Rust DSL): {}", id, e));
    check(main.context());
    main.stop().unwrap();
}

/// Run a YAML-only guide example.
fn run_yaml(page_name: &str, id: &str, vars: &[(&str, &str)], check: impl Fn(&CamelContext)) {
    let yaml = subst(&example(page_name, id), vars);
    let main = start_yaml(id, &yaml, |_| {});
    check(main.context());
    main.stop().unwrap();
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

// ---------------------------------------------------------------------------
// Page-wide checks
// ---------------------------------------------------------------------------

#[test]
fn every_yaml_block_is_marked_unique_and_parses() {
    let mut ids = std::collections::BTreeSet::new();
    for p in PAGES {
        for (lang, marker, text) in blocks(&page(p)) {
            if lang != "yaml" {
                continue;
            }
            let id = marker.unwrap_or_else(|| panic!("{}: a yaml block has no <!-- example: id --> marker:\n{}", p, text));
            assert!(ids.insert(id.clone()), "{}: duplicate example id {}", p, id);
            match cameleer_yaml::parse_yaml(&text) {
                Ok(Value::Array(items)) => assert!(!items.is_empty(), "{}: example {} is empty", p, id),
                Ok(other) => panic!("{}: example {} is not a list of routes: {}", p, id, other.type_name()),
                Err(e) => panic!("{}: example {} does not parse: {:?}", p, id, e),
            }
        }
    }
    assert!(ids.len() > 40, "{} examples", ids.len());
}

/// The Rust DSL blocks of the guides appear verbatim in this file, so they compile and cannot
/// drift from the tested code.
#[test]
fn rust_snippets_are_copied_verbatim() {
    let me = include_str!("docs_guides.rs").replace("\r\n", "\n");
    let norm = |s: &str| s.lines().map(|l| l.trim()).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n");
    let me_norm = norm(&me);
    for p in PAGES {
        for (lang, _, text) in blocks(&page(p)) {
            if lang == "rust" {
                assert!(me_norm.contains(&norm(&text)), "{}: this Rust snippet is not in docs_guides.rs:\n{}", p, text);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// eips/routing.md
// ---------------------------------------------------------------------------

const ROUTING: &str = "eips/routing.md";

#[test]
fn eip_pipeline_tod() {
    run_both(
        ROUTING,
        "eip-pipeline-tod",
        |b| {
            b.from("direct:orders")
                .route_id("pipeline")
                .step("normalize")
                    .set_header("region", simple("${body[region]}"))
                .end()
                .to_d("mock:region-${header.region}")
                .to("mock:all-orders");
        },
        |ctx| {
            let order = Value::object([("region", Value::from("eu")), ("id", Value::from(7))]);
            template(ctx).send_body("direct:orders", order).unwrap();
            assert_eq!(wait_bodies(ctx, "mock:region-eu", 1).len(), 1);
            assert_eq!(wait_bodies(ctx, "mock:all-orders", 1).len(), 1);
        },
    );
}

#[test]
fn eip_filter() {
    run_both(
        ROUTING,
        "eip-filter",
        |b| {
            b.from("direct:payments")
                .filter(simple("${header.amount} > 1000"))
                    .to("mock:review")
                .end()
                .to("mock:ledger");
        },
        |ctx| {
            let t = template(ctx);
            t.send_body_and_header("direct:payments", "small", "amount", 20).unwrap();
            t.send_body_and_header("direct:payments", "big", "amount", 5000).unwrap();
            assert_eq!(bodies(ctx, "mock:review"), vec!["big"]);
            assert_eq!(bodies(ctx, "mock:ledger"), vec!["small", "big"]);
        },
    );
}

#[test]
fn eip_choice() {
    run_both(
        ROUTING,
        "eip-choice",
        |b| {
            b.from("direct:tickets")
                .choice()
                    .when(simple("${header.priority} == 'P1'"))
                        .to("mock:page-oncall")
                    .when(simple("${header.priority} == 'P2'"))
                        .to("mock:team-queue")
                    .otherwise()
                        .to("mock:backlog")
                .end();
        },
        |ctx| {
            let t = template(ctx);
            for (body, p) in [("outage", "P1"), ("bug", "P2"), ("question", "P3")] {
                t.send_body_and_header("direct:tickets", body, "priority", p).unwrap();
            }
            assert_eq!(bodies(ctx, "mock:page-oncall"), vec!["outage"]);
            assert_eq!(bodies(ctx, "mock:team-queue"), vec!["bug"]);
            assert_eq!(bodies(ctx, "mock:backlog"), vec!["question"]);
        },
    );
}

#[test]
fn eip_recipient_list() {
    run_both(
        ROUTING,
        "eip-recipient-list",
        |b| {
            b.from("direct:notify")
                .recipient_list(simple("${header.channels}"));
        },
        |ctx| {
            template(ctx).send_body_and_header("direct:notify", "deploy done", "channels", "mock:email,mock:sms").unwrap();
            assert_eq!(bodies(ctx, "mock:email"), vec!["deploy done"]);
            assert_eq!(bodies(ctx, "mock:sms"), vec!["deploy done"]);
        },
    );
}

#[test]
fn eip_routing_slip() {
    run_both(
        ROUTING,
        "eip-routing-slip",
        |b| {
            b.from("direct:process")
                .routing_slip(simple("${header.slip}"));
            b.from("direct:resize")
                .set_body(simple("${body}-resized"));
            b.from("direct:watermark")
                .set_body(simple("${body}-watermarked"));
        },
        |ctx| {
            template(ctx).send_body_and_header("direct:process", "photo", "slip", "direct:resize,direct:watermark,mock:done").unwrap();
            assert_eq!(bodies(ctx, "mock:done"), vec!["photo-resized-watermarked"]);
        },
    );
}

#[test]
fn eip_dynamic_router() {
    run_both(
        ROUTING,
        "eip-dynamic-router",
        |b| {
            b.from("direct:workflow")
                .dynamic_router(simple("${header.next}"))
                .to("mock:workflow-done");
            b.from("direct:validate")
                .set_header("next", constant("direct:approve"));
            b.from("direct:approve")
                .remove_header("next")
                .set_body(simple("${body} approved"));
        },
        |ctx| {
            template(ctx).send_body_and_header("direct:workflow", "invoice 7", "next", "direct:validate").unwrap();
            assert_eq!(bodies(ctx, "mock:workflow-done"), vec!["invoice 7 approved"]);
        },
    );
}

#[test]
fn eip_multicast() {
    run_both(
        ROUTING,
        "eip-multicast",
        |b| {
            b.from("direct:order-placed")
                .multicast()
                    .parallel_processing()
                    .to("mock:billing")
                    .to("mock:shipping")
                    .to("mock:analytics")
                .end();
        },
        |ctx| {
            template(ctx).send_body("direct:order-placed", "order 1").unwrap();
            for m in ["mock:billing", "mock:shipping", "mock:analytics"] {
                assert_eq!(wait_bodies(ctx, m, 1), vec!["order 1"]);
            }
        },
    );
}

#[test]
fn eip_wire_tap() {
    run_both(
        ROUTING,
        "eip-wire-tap",
        |b| {
            b.from("direct:prompts")
                .wire_tap("mock:audit")
                .set_body(simple("answered: ${body}"))
                .to("mock:answers");
        },
        |ctx| {
            template(ctx).send_body("direct:prompts", "what is EIP?").unwrap();
            assert_eq!(wait_bodies(ctx, "mock:answers", 1), vec!["answered: what is EIP?"]);
            assert_eq!(wait_bodies(ctx, "mock:audit", 1), vec!["what is EIP?"]);
        },
    );
}

#[test]
fn eip_load_balancer() {
    run_both(
        ROUTING,
        "eip-load-balancer",
        |b| {
            b.from("direct:inference")
                .load_balance()
                    .round_robin()
                    .to("mock:gpu-1")
                    .to("mock:gpu-2")
                .end();
        },
        |ctx| {
            let t = template(ctx);
            for i in 0..4 {
                t.send_body("direct:inference", format!("job {}", i)).unwrap();
            }
            assert_eq!(bodies(ctx, "mock:gpu-1"), vec!["job 0", "job 2"]);
            assert_eq!(bodies(ctx, "mock:gpu-2"), vec!["job 1", "job 3"]);
        },
    );
}

#[test]
fn eip_load_balancer_failover() {
    run_both(
        ROUTING,
        "eip-load-balancer-failover",
        |b| {
            b.from("direct:call-model")
                .load_balance()
                    .failover(&["IOException"])
                    .to("direct:primary-model")
                    .to("direct:backup-model")
                .end();
            b.from("direct:primary-model")
                .throw_exception("IOException", "primary is down");
            b.from("direct:backup-model")
                .set_body(constant("answer from backup"));
        },
        |ctx| {
            let answer = template(ctx).request_body("direct:call-model", "question").unwrap();
            assert_eq!(answer.to_string(), "answer from backup");
        },
    );
}

#[test]
fn eip_loop() {
    run_both(
        ROUTING,
        "eip-loop",
        |b| {
            b.from("direct:retry-poll")
                .loop_(constant(3))
                    .set_body(simple("${body}+${exchangeProperty.CamelLoopIndex}"))
                .end()
                .to("mock:looped");
        },
        |ctx| {
            template(ctx).send_body("direct:retry-poll", "x").unwrap();
            assert_eq!(bodies(ctx, "mock:looped"), vec!["x+0+1+2"]);
        },
    );
}

// ---------------------------------------------------------------------------
// eips/split-aggregate.md
// ---------------------------------------------------------------------------

const SPLIT: &str = "eips/split-aggregate.md";

#[test]
fn eip_split_json() {
    run_both(
        SPLIT,
        "eip-split-json",
        |b| {
            b.from("direct:order")
                .split(jsonpath("$.items"))
                    .set_header("sku", simple("${body[sku]}"))
                    .to("mock:order-line")
                .end();
        },
        |ctx| {
            template(ctx).send_body("direct:order", r#"{"items":[{"sku":"A1"},{"sku":"B2"}]}"#).unwrap();
            let skus: Vec<String> = mock(ctx, "mock:order-line").received_exchanges().iter().map(|e| header_of(e, "sku")).collect();
            assert_eq!(skus, vec!["A1", "B2"]);
        },
    );
}

#[test]
fn eip_split_aggregate() {
    run_both(
        SPLIT,
        "eip-split-aggregate",
        |b| {
            b.from("direct:lines")
                .split(tokenize("\n"))
                    .aggregation_strategy(strategies::grouped_body())
                    .set_body(simple("${body.toUpperCase()}"))
                .end()
                .to("mock:upper-lines");
        },
        |ctx| {
            template(ctx).send_body("direct:lines", "a\nb\nc").unwrap();
            let got = mock(ctx, "mock:upper-lines").received_exchanges()[0].body().clone();
            assert_eq!(got, Value::Array(vec!["A".into(), "B".into(), "C".into()]));
        },
    );
}

#[test]
fn eip_aggregate() {
    run_both(
        SPLIT,
        "eip-aggregate",
        |b| {
            b.from("direct:readings")
                .aggregate(header("device"), strategies::grouped_body())
                    .completion_size(3)
                    .completion_timeout(Duration::from_millis(5000))
                    .to("mock:device-batch")
                .end();
        },
        |ctx| {
            let t = template(ctx);
            t.send_body_and_header("direct:readings", "20", "device", "t1").unwrap();
            t.send_body_and_header("direct:readings", "5", "device", "t2").unwrap();
            t.send_body_and_header("direct:readings", "21", "device", "t1").unwrap();
            t.send_body_and_header("direct:readings", "22", "device", "t1").unwrap();
            wait_bodies(ctx, "mock:device-batch", 1);
            let ex = mock(ctx, "mock:device-batch").received_exchanges();
            assert_eq!(ex.len(), 1);
            assert_eq!(ex[0].body().clone(), Value::Array(vec!["20".into(), "21".into(), "22".into()]));
            assert_eq!(ex[0].property("CamelAggregatedCompletedBy").map(|v| v.to_string()).as_deref(), Some("size"));
        },
    );
}

#[test]
fn eip_resequence() {
    run_both(
        SPLIT,
        "eip-resequence",
        |b| {
            b.from("direct:out-of-order")
                .resequence(header("seq"))
                    .batch()
                    .batch_size(3)
                    .batch_timeout(Duration::from_millis(2000))
                    .to("mock:in-order")
                .end();
        },
        |ctx| {
            let t = template(ctx);
            for seq in [3, 1, 2] {
                t.send_body_and_header("direct:out-of-order", format!("m{}", seq), "seq", seq).unwrap();
            }
            assert_eq!(wait_bodies(ctx, "mock:in-order", 3), vec!["m1", "m2", "m3"]);
        },
    );
}

#[test]
fn eip_sort() {
    run_both(
        SPLIT,
        "eip-sort",
        |b| {
            b.from("direct:names")
                .sort(tokenize(","))
                .to("mock:sorted");
        },
        |ctx| {
            template(ctx).send_body("direct:names", "pear,apple,fig").unwrap();
            let got = mock(ctx, "mock:sorted").received_exchanges()[0].body().clone();
            assert_eq!(got, Value::Array(vec!["apple".into(), "fig".into(), "pear".into()]));
        },
    );
}

// ---------------------------------------------------------------------------
// eips/transformation.md
// ---------------------------------------------------------------------------

const TRANSFORM: &str = "eips/transformation.md";

#[test]
fn eip_message_translator() {
    run_both(
        TRANSFORM,
        "eip-message-translator",
        |b| {
            b.from("direct:greet")
                .set_header("customer", simple("${body[name]}"))
                .transform(simple("Hello ${header.customer}, your order ${body[order]} has shipped."))
                .remove_headers("internal*")
                .to("mock:greeting");
        },
        |ctx| {
            let body = Value::object([("name", Value::from("Ana")), ("order", Value::from(17))]);
            template(ctx).send_body_and_header("direct:greet", body, "internalTrace", "x").unwrap();
            let ex = mock(ctx, "mock:greeting").received_exchanges();
            assert_eq!(ex[0].body().to_string(), "Hello Ana, your order 17 has shipped.");
            assert!(ex[0].header("internalTrace").is_none());
        },
    );
}

#[test]
fn eip_marshal_json() {
    run_both(
        TRANSFORM,
        "eip-marshal-json",
        |b| {
            b.from("direct:order-json")
                .unmarshal_json()
                .set_body(simple(r#"{"order": "${body[id]}", "total": ${body[qty]}}"#))
                .unmarshal_json()
                .marshal_json()
                .to("mock:order-out");
        },
        |ctx| {
            template(ctx).send_body("direct:order-json", r#"{"id":"A-1","qty":3}"#).unwrap();
            let out = mock(ctx, "mock:order-out").received_exchanges()[0].body().to_string();
            assert_eq!(out.replace(' ', ""), r#"{"order":"A-1","total":3}"#);
        },
    );
}

#[test]
fn eip_convert_body() {
    run_both(
        TRANSFORM,
        "eip-convert-body",
        |b| {
            b.from("direct:reading")
                .convert_body_to("java.lang.Integer")
                .filter(simple("${body} > 100"))
                    .to("mock:too-hot")
                .end();
        },
        |ctx| {
            let t = template(ctx);
            t.send_body("direct:reading", " 250 ").unwrap();
            t.send_body("direct:reading", "99").unwrap();
            let ex = mock(ctx, "mock:too-hot").received_exchanges();
            assert_eq!(ex.len(), 1);
            assert_eq!(ex[0].body().clone(), Value::Int(250));
        },
    );
}

#[test]
fn eip_enrich() {
    run_both(
        TRANSFORM,
        "eip-enrich",
        |b| {
            b.from("direct:ticket")
                .set_property("ticket", simple("${body}"))
                .enrich("direct:crm-lookup")
                .set_body(simple("${exchangeProperty.ticket} (customer tier: ${body})"))
                .to("mock:enriched");
            b.from("direct:crm-lookup")
                .choice()
                    .when(simple("${header.customerId} == 'c-1'"))
                        .set_body(constant("gold"))
                    .otherwise()
                        .set_body(constant("standard"))
                .end();
        },
        |ctx| {
            template(ctx).send_body_and_header("direct:ticket", "Printer on fire", "customerId", "c-1").unwrap();
            assert_eq!(bodies(ctx, "mock:enriched"), vec!["Printer on fire (customer tier: gold)"]);
        },
    );
}

#[test]
fn eip_poll_enrich() {
    run_both(
        TRANSFORM,
        "eip-poll-enrich",
        |b| {
            b.from("direct:with-latest-config")
                .poll_enrich("seda:config-updates", 1000)
                .to("mock:with-config");
        },
        |ctx| {
            let t = template(ctx);
            t.send_body("seda:config-updates", "threshold=40").unwrap();
            t.send_body("direct:with-latest-config", "request").unwrap();
            assert_eq!(bodies(ctx, "mock:with-config"), vec!["threshold=40"]);
        },
    );
}

#[test]
fn eip_claim_check() {
    run_both(
        TRANSFORM,
        "eip-claim-check",
        |b| {
            b.from("direct:big-document")
                .claim_check(ClaimCheckOperation::Set, "document")
                .set_body(constant("summary request"))
                .to("mock:light")
                .claim_check(ClaimCheckOperation::Get, "document")
                .to("mock:restored");
        },
        |ctx| {
            template(ctx).send_body("direct:big-document", "a very long contract").unwrap();
            assert_eq!(bodies(ctx, "mock:light"), vec!["summary request"]);
            assert_eq!(bodies(ctx, "mock:restored"), vec!["a very long contract"]);
        },
    );
}

#[test]
fn eip_script() {
    run_both(
        TRANSFORM,
        "eip-script",
        |b| {
            b.from("direct:tags")
                .process(cameleer_script::script("groovy", "def tags = request.body.split(',')\nrequest.headers.tagCount = tags.size()\nrequest.body = tags.collect { it.trim().toLowerCase() }.join('|')").unwrap())
                .to("mock:tags");
        },
        |ctx| {
            template(ctx).send_body("direct:tags", "Rust, Camel ,EIP").unwrap();
            let ex = mock(ctx, "mock:tags").received_exchanges();
            assert_eq!(ex[0].body().to_string(), "rust|camel|eip");
            assert_eq!(header_of(&ex[0], "tagCount"), "3");
        },
    );
}

// ---------------------------------------------------------------------------
// eips/flow-control.md
// ---------------------------------------------------------------------------

const FLOW: &str = "eips/flow-control.md";

#[test]
fn eip_idempotent_consumer() {
    run_both(
        FLOW,
        "eip-idempotent-consumer",
        |b| {
            b.from("direct:payment-events")
                .idempotent_consumer(header("paymentId"), memory_idempotent_repository(1000))
                    .to("mock:charge")
                .end()
                .to("mock:seen");
        },
        |ctx| {
            let t = template(ctx);
            for id in ["p-1", "p-1", "p-2"] {
                t.send_body_and_header("direct:payment-events", id, "paymentId", id).unwrap();
            }
            assert_eq!(bodies(ctx, "mock:charge"), vec!["p-1", "p-2"]);
            let seen = mock(ctx, "mock:seen").received_exchanges();
            assert_eq!(seen.len(), 3);
            assert_eq!(seen[1].property("CamelDuplicateMessage").map(|v| v.to_string()).as_deref(), Some("true"));
        },
    );
}

#[test]
fn eip_throttle() {
    run_both(
        FLOW,
        "eip-throttle",
        |b| {
            b.from("direct:model-calls")
                .throttle(2, Duration::from_millis(500))
                .to("mock:model");
        },
        |ctx| {
            let t = template(ctx);
            let start = Instant::now();
            for i in 0..5 {
                t.send_body("direct:model-calls", i).unwrap();
            }
            assert_eq!(wait_bodies(ctx, "mock:model", 5).len(), 5);
            assert!(start.elapsed() >= Duration::from_millis(900), "{:?}", start.elapsed());
        },
    );
}

#[test]
fn eip_delay() {
    run_both(
        FLOW,
        "eip-delay",
        |b| {
            b.from("direct:slow-down")
                .delay(constant(200))
                .to("mock:delayed");
        },
        |ctx| {
            let start = Instant::now();
            template(ctx).send_body("direct:slow-down", "x").unwrap();
            assert_eq!(wait_bodies(ctx, "mock:delayed", 1), vec!["x"]);
            assert!(start.elapsed() >= Duration::from_millis(190), "{:?}", start.elapsed());
        },
    );
}

#[test]
fn eip_sample() {
    run_both(
        FLOW,
        "eip-sample",
        |b| {
            b.from("direct:telemetry")
                .sample_every(3)
                .to("mock:sampled");
        },
        |ctx| {
            let t = template(ctx);
            for i in 0..6 {
                t.send_body("direct:telemetry", i).unwrap();
            }
            assert_eq!(bodies(ctx, "mock:sampled").len(), 2);
        },
    );
}

#[test]
fn eip_circuit_breaker() {
    run_both(
        FLOW,
        "eip-circuit-breaker",
        |b| {
            b.from("direct:ask-model")
                .circuit_breaker()
                    .sliding_window_size(2)
                    .minimum_number_of_calls(2)
                    .failure_rate_threshold(50.0)
                    .wait_duration_in_open_state(Duration::from_secs(30))
                    .to("direct:flaky-model")
                .on_fallback()
                    .set_body(constant("The assistant is busy, please retry in a minute."))
                .end();
            b.from("direct:flaky-model")
                .throw_exception("IOException", "model API timed out");
        },
        |ctx| {
            let t = template(ctx);
            let mut last = None;
            for _ in 0..3 {
                let ex = t.request("direct:ask-model", |ex| ex.set_body("hi")).unwrap();
                assert_eq!(ex.body().to_string(), "The assistant is busy, please retry in a minute.");
                last = Some(ex);
            }
            let ex = last.unwrap();
            assert_eq!(ex.property("CamelCircuitBreakerResponseShortCircuited").map(|v| v.to_string()).as_deref(), Some("true"));
            assert_eq!(ex.property("CamelCircuitBreakerResponseFromFallback").map(|v| v.to_string()).as_deref(), Some("true"));
        },
    );
}

#[test]
fn eip_validate() {
    run_both(
        FLOW,
        "eip-validate",
        |b| {
            b.from("direct:signup")
                .validate(simple("${body} contains '@'"))
                .to("mock:signups");
        },
        |ctx| {
            let t = template(ctx);
            t.send_body("direct:signup", "ana@example.com").unwrap();
            let err = t.request_body("direct:signup", "not-an-email").unwrap_err();
            assert!(format!("{:?}", err).contains("PredicateValidationException"), "{:?}", err);
            assert_eq!(bodies(ctx, "mock:signups"), vec!["ana@example.com"]);
        },
    );
}

#[test]
fn eip_threads() {
    run_both(
        FLOW,
        "eip-threads",
        |b| {
            b.from("direct:fan-out")
                .threads(4)
                .to("mock:parallel");
        },
        |ctx| {
            let t = template(ctx);
            for i in 0..8 {
                t.send_body("direct:fan-out", i).unwrap();
            }
            assert_eq!(wait_bodies(ctx, "mock:parallel", 8).len(), 8);
        },
    );
}

// ---------------------------------------------------------------------------
// eips/system.md
// ---------------------------------------------------------------------------

const SYSTEM: &str = "eips/system.md";

#[test]
fn eip_intercept_send() {
    run_both(
        SYSTEM,
        "eip-intercept-send",
        |b| {
            b.intercept_send_to_endpoint("mock:payment-api*")
                .skip_send_to_original_endpoint()
                .set_body(simple("sandbox charge for ${body}"))
                .to("mock:sandbox");
            b.from("direct:checkout")
                .to("mock:payment-api")
                .to("mock:receipts");
        },
        |ctx| {
            template(ctx).send_body("direct:checkout", "order-9").unwrap();
            assert!(bodies(ctx, "mock:payment-api").is_empty());
            assert_eq!(bodies(ctx, "mock:sandbox"), vec!["sandbox charge for order-9"]);
            assert_eq!(bodies(ctx, "mock:receipts"), vec!["sandbox charge for order-9"]);
        },
    );
}

#[test]
fn eip_intercept_from() {
    run_both(
        SYSTEM,
        "eip-intercept-from",
        |b| {
            b.intercept_from("direct:*")
                .set_header("receivedBy", constant("edge-gateway-1"));
            b.from("direct:sensor")
                .to("mock:stamped");
        },
        |ctx| {
            template(ctx).send_body("direct:sensor", "21.5").unwrap();
            assert_eq!(header_of(&mock(ctx, "mock:stamped").received_exchanges()[0], "receivedBy"), "edge-gateway-1");
        },
    );
}

#[test]
fn eip_on_completion() {
    run_both(
        SYSTEM,
        "eip-on-completion",
        |b| {
            b.from("direct:place-order")
                .on_completion()
                    .on_failure_only()
                    .set_body(simple("order failed: ${body}"))
                    .to("mock:failed-orders")
                .end()
                .filter(simple("${body} contains 'invalid'"))
                    .throw_exception("IllegalArgumentException", "bad order")
                .end()
                .to("mock:orders");
        },
        |ctx| {
            let t = template(ctx);
            assert!(t.send_body("direct:place-order", "invalid-1").is_err());
            t.send_body("direct:place-order", "ok-1").unwrap();
            assert_eq!(wait_bodies(ctx, "mock:failed-orders", 1), vec!["order failed: invalid-1"]);
            assert_eq!(bodies(ctx, "mock:orders"), vec!["ok-1"]);
        },
    );
}

#[test]
fn eip_saga() {
    run_both(
        SYSTEM,
        "eip-saga",
        |b| {
            b.from("direct:book-trip")
                .saga()
                    .compensation("direct:cancel-flight")
                    .to("mock:flight-booked")
                    .filter(simple("${header.hotelFull} == 'true'"))
                        .throw_exception("IllegalStateException", "no hotel rooms")
                    .end()
                .end();
            b.from("direct:cancel-flight")
                .to("mock:flight-cancelled");
        },
        |ctx| {
            let t = template(ctx);
            t.send_body_and_header("direct:book-trip", "trip-1", "hotelFull", "false").unwrap();
            assert!(t.send_body_and_header("direct:book-trip", "trip-2", "hotelFull", "true").is_err());
            wait_bodies(ctx, "mock:flight-cancelled", 1);
            assert_eq!(bodies(ctx, "mock:flight-booked").len(), 2);
            std::thread::sleep(Duration::from_millis(100));
            assert_eq!(bodies(ctx, "mock:flight-cancelled").len(), 1);
        },
    );
}

#[test]
fn eip_route_template() {
    run_both_ctx(
        SYSTEM,
        "eip-route-template",
        |ctx| {
            ctx.add_route_template(
                "ask-model",
                &[TemplateParam::required("name"), TemplateParam::optional("prefix", "Answer briefly:")],
                |b, p| {
                    b.from(format!("direct:{}", p.get("name")))
                        .set_body(simple(format!("{} ${{body}}", p.get("prefix"))))
                        .to(format!("mock:{}", p.get("name")));
                },
            );
            TemplatedRouteBuilder::new(ctx, "ask-model").parameter("name", "support-bot").add()?;
            TemplatedRouteBuilder::new(ctx, "ask-model").parameter("name", "sales-bot").parameter("prefix", "Answer politely:").add()?;
            Ok(())
        },
        |ctx| {
            let t = template(ctx);
            t.send_body("direct:support-bot", "reset password?").unwrap();
            t.send_body("direct:sales-bot", "price?").unwrap();
            assert_eq!(bodies(ctx, "mock:support-bot"), vec!["Answer briefly: reset password?"]);
            assert_eq!(bodies(ctx, "mock:sales-bot"), vec!["Answer politely: price?"]);
        },
    );
}

#[test]
fn eip_kamelet() {
    run_both(
        SYSTEM,
        "eip-kamelet",
        |b| {
            b.from("direct:tag-reading")
                .kamelet("insert-header-action?name=site&value=plant-7")
                .to("mock:tagged");
        },
        |ctx| {
            template(ctx).send_body("direct:tag-reading", "42").unwrap();
            assert_eq!(header_of(&mock(ctx, "mock:tagged").received_exchanges()[0], "site"), "plant-7");
        },
    );
}

#[test]
fn eip_poll() {
    run_both(
        SYSTEM,
        "eip-poll",
        |b| {
            b.from("direct:next-job")
                .poll("seda:jobs", 1000)
                .to("mock:job");
        },
        |ctx| {
            let t = template(ctx);
            t.send_body("seda:jobs", "job-1").unwrap();
            t.send_body("direct:next-job", "").unwrap();
            assert_eq!(bodies(ctx, "mock:job"), vec!["job-1"]);
        },
    );
}

#[test]
fn eip_stop() {
    run_both(
        SYSTEM,
        "eip-stop",
        |b| {
            b.from("direct:events")
                .filter(simple("${header.type} == 'heartbeat'"))
                    .stop()
                .end()
                .to("mock:events");
        },
        |ctx| {
            let t = template(ctx);
            t.send_body_and_header("direct:events", "hb", "type", "heartbeat").unwrap();
            t.send_body_and_header("direct:events", "door opened", "type", "alarm").unwrap();
            assert_eq!(bodies(ctx, "mock:events"), vec!["door opened"]);
        },
    );
}

// ---------------------------------------------------------------------------
// error-handling.md
// ---------------------------------------------------------------------------

const ERRORS: &str = "error-handling.md";

fn bind_inventory_api(ctx: &CamelContext) {
    ctx.registry().bind_processor("inventoryApi", |ex: &mut Exchange| {
        let attempt = ex.header("CamelRedeliveryCounter").map(|v| v.to_string()).unwrap_or_default();
        if ex.body().to_string() == "poison" || attempt != "2" {
            return Err(CamelError::new("IOException", "connection reset by inventory API"));
        }
        ex.set_body(format!("reserved {}", ex.body()));
        Ok(())
    });
}

/// Like [`run_both`], with beans bound by `setup` before the routes start.
fn run_both_setup(
    page_name: &str,
    id: &str,
    setup: fn(&CamelContext),
    rust: impl FnOnce(&mut RouteBuilder) + 'static,
    check: impl Fn(&CamelContext),
) {
    let yaml = example(page_name, id);
    let main = start_yaml(id, &yaml, |m| setup(m.context()));
    check(main.context());
    main.stop().unwrap();

    let mut main = cameleer_main::new_main();
    setup(main.context());
    main.configure(move |ctx| ctx.add_routes(rust));
    main.start().unwrap_or_else(|e| panic!("{} (Rust DSL): {}", id, e));
    check(main.context());
    main.stop().unwrap();
}

#[test]
fn eh_dead_letter_channel() {
    run_both_setup(
        ERRORS,
        "eh-dead-letter-channel",
        bind_inventory_api,
        |b| {
            b.error_handler(
                dead_letter_channel("mock:dead-letters")
                    .use_original_message()
                    .maximum_redeliveries(3)
                    .redelivery_delay(10)
                    .use_exponential_back_off()
                    .back_off_multiplier(2.0),
            );
            b.from("direct:reserve")
                .process_ref("inventoryApi")
                .to("mock:reserved");
        },
        |ctx| {
            let t = template(ctx);
            t.send_body("direct:reserve", "sku-1").unwrap();
            let start = Instant::now();
            t.send_body("direct:reserve", "poison").unwrap();
            assert!(start.elapsed() >= Duration::from_millis(60), "{:?}", start.elapsed());
            assert_eq!(bodies(ctx, "mock:reserved"), vec!["reserved sku-1"]);
            let dead = mock(ctx, "mock:dead-letters").received_exchanges();
            assert_eq!(dead.len(), 1);
            assert_eq!(dead[0].body().to_string(), "poison");
            assert!(dead[0].property("CamelExceptionCaught").is_some());
        },
    );
}

#[test]
fn eh_on_exception() {
    run_both_setup(
        ERRORS,
        "eh-on-exception",
        bind_inventory_api,
        |b| {
            b.on_exception(&["IllegalArgumentException"])
                .handled(true)
                .set_header("CamelHttpResponseCode", constant(400))
                .set_body(simple("rejected: ${exception.message}"));
            b.on_exception(&["IOException"])
                .maximum_redeliveries(1)
                .handled(true)
                .set_body(constant("inventory unavailable, order queued"))
                .to("mock:retry-later");
            b.from("direct:order")
                .filter(simple("${body} == ''"))
                    .throw_exception("IllegalArgumentException", "empty order")
                .end()
                .process_ref("inventoryApi")
                .to("mock:orders");
        },
        |ctx| {
            let t = template(ctx);
            let ex = t.request("direct:order", |ex| ex.set_body("")).unwrap();
            assert_eq!(ex.body().to_string(), "rejected: empty order");
            assert_eq!(header_of(&ex, "CamelHttpResponseCode"), "400");
            let reply = t.request_body("direct:order", "sku-1").unwrap();
            assert_eq!(reply.to_string(), "inventory unavailable, order queued");
            assert_eq!(bodies(ctx, "mock:retry-later").len(), 1);
            assert!(bodies(ctx, "mock:orders").is_empty());
        },
    );
}

#[test]
fn eh_do_try() {
    run_both(
        ERRORS,
        "eh-do-try",
        |b| {
            b.from("direct:parse")
                .do_try()
                    .unmarshal_json()
                    .set_body(simple("order ${body[id]}"))
                .do_catch(&["Exception"])
                    .set_body(simple("not JSON: ${exception.message}"))
                .do_finally()
                    .to("mock:parse-attempts")
                .end_do_try()
                .to("mock:parsed");
        },
        |ctx| {
            let t = template(ctx);
            t.send_body("direct:parse", r#"{"id": 5}"#).unwrap();
            t.send_body("direct:parse", "hello").unwrap();
            let parsed = bodies(ctx, "mock:parsed");
            assert_eq!(parsed[0], "order 5");
            assert!(parsed[1].starts_with("not JSON: "), "{}", parsed[1]);
            assert_eq!(bodies(ctx, "mock:parse-attempts").len(), 2);
        },
    );
}

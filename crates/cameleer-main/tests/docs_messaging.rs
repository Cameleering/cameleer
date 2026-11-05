//! The examples on the messaging, streaming, core and edge-protocol component pages
//! (`docs/components/*.md`) are tested here. Each YAML example is read from its page (by its
//! `<!-- example: id -->` marker), loaded with `cameleer_main::new_main()`, every endpoint URI it
//! uses is resolved with `ctx.get_endpoint` (unknown options fail, as in Apache Camel), and the
//! route runs end to end against the embedded brokers and servers. `{{env:NAME:default}}`
//! placeholders are pointed at those servers by setting the property `env:NAME`. Where an example
//! ends in a `log:` (or `stream:out`) endpoint, the test swaps that endpoint for a `mock:` to assert
//! on the result. The Rust DSL snippets of the pages are copied into this file (and checked to be
//! verbatim) so they keep compiling.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use cameleer_core::prelude::*;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

const PAGES: &[&str] = &[
    "activemq", "amqp", "amqp-stomp", "stomp", "jms", "rabbitmq", "spring-rabbitmq", "kafka", "paho", "paho-mqtt5", "seda", "vm", "direct", "timer",
    "scheduler", "cron", "quartz", "log", "mock", "test", "dataset", "dataset-test", "stub", "browse", "controlbus", "bean", "class", "ref", "language",
    "dataformat", "kamelet", "saga", "exec", "stream", "syslog", "mllp", "netty",
];

fn page(name: &str) -> String {
    let path = format!("{}/../../docs/components/{}.md", env!("CARGO_MANIFEST_DIR"), name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {}", path, e)).replace("\r\n", "\n")
}

/// Fenced blocks of a page: (language, preceding line, content).
fn blocks(text: &str) -> Vec<(String, String, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if let Some(lang) = lines[i].strip_prefix("```") {
            let before = if i > 0 { lines[i - 1].to_string() } else { String::new() };
            let mut j = i + 1;
            while j < lines.len() && lines[j] != "```" {
                j += 1;
            }
            out.push((lang.trim().to_string(), before, lines[i + 1..j].join("\n") + "\n"));
            i = j + 1;
        } else {
            i += 1;
        }
    }
    out
}

/// The YAML block right after `<!-- example: id -->` on a page.
fn example(page_name: &str, id: &str) -> String {
    let marker = format!("<!-- example: {} -->", id);
    blocks(&page(page_name))
        .into_iter()
        .find(|(lang, before, _)| lang == "yaml" && before.trim() == marker)
        .map(|(_, _, body)| body)
        .unwrap_or_else(|| panic!("{}.md has no yaml block for example {}", page_name, id))
}

fn replaced(mut yaml: String, replace: &[(&str, &str)]) -> String {
    for (from, to) in replace {
        assert!(yaml.contains(from), "example does not contain {:?}:\n{}", from, yaml);
        yaml = yaml.replace(from, to);
    }
    yaml
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\''))) {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Every endpoint URI of a YAML example (`uri:`, `to:`, `toD:` values), with the options of a
/// following `parameters:` block appended as query parameters.
fn endpoint_uris(yaml: &str) -> Vec<String> {
    let lines: Vec<&str> = yaml.lines().collect();
    let mut uris = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim().trim_start_matches("- ");
        let value = ["uri:", "to:", "toD:"].iter().find_map(|k| t.strip_prefix(k)).map(unquote);
        let Some(mut uri) = value.filter(|v| !v.is_empty()) else { continue };
        if let Some(next) = lines.get(i + 1) {
            if next.trim() == "parameters:" {
                let base = indent(next);
                let mut query = Vec::new();
                for p in &lines[i + 2..] {
                    if p.trim().is_empty() || indent(p) <= base {
                        break;
                    }
                    let (k, v) = p.trim().split_once(':').expect("key: value");
                    let v = unquote(v);
                    let v = v.replace('#', "%23").replace(' ', "+");
                    query.push(format!("{}={}", k.trim(), v));
                }
                if !query.is_empty() {
                    uri = format!("{}{}{}", uri, if uri.contains('?') { "&" } else { "?" }, query.join("&"));
                }
            }
        }
        uris.push(uri);
    }
    assert!(!uris.is_empty(), "no endpoint uri in\n{}", yaml);
    uris
}

struct Running {
    main: cameleer_main::Main,
    ctx: CamelContext,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.main.stop();
    }
}

/// Load a YAML example (plus optional Rust setup), start it and resolve its endpoints.
fn start(id: &str, yaml: &str, props: &[(&str, String)], setup: impl FnOnce(&CamelContext) -> Result<()> + 'static) -> Running {
    let mut main = cameleer_main::new_main();
    for (k, v) in props {
        main.set_property(*k, v.clone());
    }
    main.configure(setup);
    main.add_route_source(format!("{}.camel.yaml", id), yaml.to_string());
    main.start().unwrap_or_else(|e| panic!("example {} does not start: {}\n{}", id, e, yaml));
    let ctx = main.context().clone();
    for uri in endpoint_uris(yaml) {
        ctx.get_endpoint(&uri).unwrap_or_else(|e| panic!("example {}: endpoint {} does not resolve: {}", id, uri, e));
    }
    Running { main, ctx }
}

fn run(id: &str, yaml: &str) -> Running {
    start(id, yaml, &[], |_| Ok(()))
}

fn wait_for(what: &str, timeout: Duration, f: impl Fn() -> bool) {
    let deadline = Instant::now() + timeout;
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {}", what);
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn mock(ctx: &CamelContext, uri: &str) -> MockEndpoint {
    ctx.mock_endpoint(uri).unwrap()
}

fn wait_mock(ctx: &CamelContext, uri: &str, n: usize) -> Vec<Exchange> {
    let m = mock(ctx, uri);
    wait_for(&format!("{} messages at {}", n, uri), Duration::from_secs(20), || m.received_counter() >= n);
    m.received_exchanges()
}

fn header(ex: &Exchange, name: &str) -> String {
    ex.header(name).map(|v| v.to_string()).unwrap_or_default()
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn free_udp_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("cameleer-docs-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn slash(p: &std::path::Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

// ---------------------------------------------------------------------------
// page structure
// ---------------------------------------------------------------------------

/// Every yaml block is marked with a unique example id, every page follows the template, and
/// every Rust DSL snippet (a block with `pub fn`) is copied verbatim into this file.
#[test]
fn pages_follow_the_template_and_snippets_are_verbatim() {
    let this_file: String = include_str!("docs_messaging.rs").lines().map(|l| l.trim()).collect::<Vec<_>>().join("\n");
    let mut ids = std::collections::HashSet::new();
    for name in PAGES {
        let text = page(name);
        let mut lines = text.lines();
        assert!(lines.next().is_some_and(|l| l.starts_with("# ")), "{}: H1 first", name);
        assert_eq!(lines.next(), Some(""), "{}", name);
        assert!(lines.next().is_some_and(|l| l.starts_with("> ")), "{}: answer line", name);
        for section in ["## When to use it", "## URI format", "## Example", "## Rust DSL", "## Options", "## Message headers", "## Run it with Docker", "## Common questions", "## Related"] {
            assert!(text.contains(&format!("\n{}\n", section)), "{}: missing {}", name, section);
        }
        assert!(text.contains(&format!("**Scheme:** `{}:`", name)), "{}: scheme line", name);
        assert!(!text.contains("ghcr.io"), "{}: use the Docker Hub image", name);
        let mut yaml_blocks = 0;
        for (lang, before, body) in blocks(&text) {
            if lang == "yaml" {
                yaml_blocks += 1;
                let id = before.trim().strip_prefix("<!-- example: ").and_then(|s| s.strip_suffix(" -->")).unwrap_or_else(|| panic!("{}: unmarked yaml block", name));
                assert!(ids.insert(id.to_string()), "duplicate example id {}", id);
                assert!(body.starts_with("- route:"), "{}: {} is not a route list", name, id);
                cameleer_yaml::parse_yaml(&body).unwrap_or_else(|e| panic!("{}: {} does not parse: {}", name, id, e));
            }
            if lang == "rust" && body.contains("pub fn ") {
                let normalized: String = body.lines().map(|l| l.trim()).collect::<Vec<_>>().join("\n");
                assert!(this_file.contains(normalized.trim()), "{}: Rust snippet not copied verbatim into docs_messaging.rs:\n{}", name, body);
            }
            if lang == "sh" {
                assert!(body.contains("pmsudhi/cameleer") || !body.contains("docker"), "{}: docker image", name);
            }
        }
        assert!(yaml_blocks >= 1, "{}: no example", name);
    }
}

// ---------------------------------------------------------------------------
// core components
// ---------------------------------------------------------------------------

#[test]
fn direct_subroute() {
    let r = run("direct-subroute", &example("direct", "direct-subroute"));
    let t = r.ctx.create_producer_template();
    assert_eq!(t.request_body("direct:orders", "o-1").unwrap().to_string(), "accepted o-1");
    let err = t.request_body("direct:orders", "").unwrap_err();
    assert!(err.to_string().contains("empty order"), "{}", err);
}

#[test]
fn seda_store_and_forward() {
    let yaml = replaced(example("seda", "seda-store-and-forward"), &[("to: log:stored", "to: mock:stored")]);
    let r = run("seda-store-and-forward", &yaml);
    let t = r.ctx.create_producer_template();
    for i in 0..3 {
        assert_eq!(t.request_body("direct:reading", format!("r{}", i)).unwrap().to_string(), "queued");
    }
    let got = wait_mock(&r.ctx, "mock:stored", 3);
    let mut bodies: Vec<String> = got.iter().map(|e| e.body().to_string()).collect();
    bodies.sort();
    assert_eq!(bodies, vec!["r0", "r1", "r2"]);
}

#[test]
fn vm_shared_audit() {
    let yaml = replaced(example("vm", "vm-shared-audit"), &[("to: log:audit", "to: mock:audit")]);
    let r = run("vm-shared-audit", &yaml);
    r.ctx.create_producer_template().send_body("direct:audit", "from-main").unwrap();
    // another context in the same process reaches the same queue
    let other = CamelContext::new();
    other.start().unwrap();
    other.create_producer_template().send_body("vm:audit", "from-other").unwrap();
    let got = wait_mock(&r.ctx, "mock:audit", 2);
    let mut bodies: Vec<String> = got.iter().map(|e| e.body().to_string()).collect();
    bodies.sort();
    assert_eq!(bodies, vec!["from-main", "from-other"]);
    other.stop().unwrap();
}

#[test]
fn stub_mail_server() {
    let r = run("stub-mail-server", &example("stub", "stub-mail-server"));
    r.ctx.create_producer_template().send_body("direct:notify", "o-7").unwrap();
    let body = r.ctx.create_consumer_template().receive_body("stub:smtp://mail.example.com?to=ops@example.com", Duration::from_secs(5)).unwrap();
    assert_eq!(body.map(|b| b.to_string()).as_deref(), Some("Order o-7 shipped"));
}

#[test]
fn timer_heartbeat() {
    let yaml = replaced(example("timer", "timer-heartbeat"), &[("period: 5000", "period: 100"), ("to: log:heartbeat", "to: mock:heartbeat")]);
    let r = run("timer-heartbeat", &yaml);
    let got = wait_mock(&r.ctx, "mock:heartbeat", 2);
    assert_eq!(got[0].body().to_string(), "edge-gw-1 alive #1");
    assert_eq!(got[1].body().to_string(), "edge-gw-1 alive #2");
    assert!(got[0].header("CamelTimerFiredTime").is_some(), "includeMetadata sets the fired time header");
}

#[test]
fn scheduler_poll_inventory() {
    let yaml = replaced(
        example("scheduler", "scheduler-poll-inventory"),
        &[("initialDelay: 1000", "initialDelay: 10"), ("delay: 60000", "delay: 100"), ("to: log:inventory", "to: mock:inventory")],
    );
    let r = run("scheduler-poll-inventory", &yaml);
    let got = wait_mock(&r.ctx, "mock:inventory", 2);
    assert_eq!(got[0].body().to_string(), "refresh");
}

#[test]
fn cron_nightly_report() {
    let yaml = replaced(example("cron", "cron-nightly-report"), &[("\"0 0 2 * * ?\"", "\"* * * * * ?\""), ("to: log:reports", "to: mock:reports")]);
    let r = run("cron-nightly-report", &yaml);
    let got = wait_mock(&r.ctx, "mock:reports", 1);
    assert!(got[0].body().to_string().starts_with("report fired at "), "{}", got[0].body());
    assert!(got[0].header("fireTime").is_some());
}

#[test]
fn quartz_sync_devices() {
    let yaml = replaced(example("quartz", "quartz-sync-devices"), &[("\"0 0/15 * * * ?\"", "\"* * * * * ?\""), ("to: log:sync", "to: mock:sync")]);
    let r = run("quartz-sync-devices", &yaml);
    let got = wait_mock(&r.ctx, "mock:sync", 1);
    assert_eq!(got[0].body().to_string(), "sync");
    assert_eq!(header(&got[0], "triggerGroup"), "edge");
}

#[test]
fn log_debug_orders() {
    let r = run("log-debug-orders", &example("log", "log-debug-orders"));
    let reply = r.ctx.create_producer_template().request_body_and_header("direct:orders", "order-1", "region", "EU").unwrap();
    assert_eq!(reply.to_string(), "logged order-1");
}

/// The test snippet shown on the mock page, verbatim.
fn mock_page_snippet(ctx: &CamelContext) -> Result<()> {
    let alerts = ctx.mock_endpoint("mock:alerts")?;
    alerts.expected_message_count(1);
    let template = ctx.create_producer_template();
    template.send_body("direct:temperature", 85)?;
    template.send_body("direct:temperature", 20)?;
    alerts.assert_is_satisfied()?;
    Ok(())
}

#[test]
fn mock_temperature_routing() {
    let r = run("mock-temperature-routing", &example("mock", "mock-temperature-routing"));
    mock_page_snippet(&r.ctx).unwrap();
    assert_eq!(mock(&r.ctx, "mock:normal").received_bodies().iter().map(|b| b.to_string()).collect::<Vec<_>>(), vec!["20"]);
}

#[test]
fn browse_rejected_orders() {
    let r = run("browse-rejected-orders", &example("browse", "browse-rejected-orders"));
    let t = r.ctx.create_producer_template();
    t.send_body("direct:reject", "bad-1").unwrap();
    t.send_body("direct:reject", "bad-2").unwrap();
    let kept = cameleer_core::component::browse::browse_exchanges(&r.ctx, "browse:rejected?browseLimit=100").unwrap();
    assert_eq!(kept.iter().map(|e| e.body().to_string()).collect::<Vec<_>>(), vec!["bad-1", "bad-2"]);
    assert!(header(&kept[0], "rejectedAt").starts_with("20"), "{}", header(&kept[0], "rejectedAt"));
}

#[test]
fn controlbus_maintenance() {
    let r = run("controlbus-maintenance", &example("controlbus", "controlbus-maintenance"));
    let t = r.ctx.create_producer_template();
    assert_eq!(t.request_body("direct:status", "").unwrap().to_string(), "Started");
    t.request_body("direct:pause", "").unwrap();
    wait_for("the ingest route to stop", Duration::from_secs(10), || t.request_body("direct:status", "").unwrap().to_string() == "Stopped");
}

#[test]
fn dataset_load_test() {
    let r = start("dataset-load-test", &example("dataset", "dataset-load-test"), &[], |ctx| {
        dataset_dsl::routes_bind_only(ctx);
        Ok(())
    });
    cameleer_core::component::dataset::dataset_endpoint(&r.ctx, "dataset:orders").unwrap().assert_is_satisfied_within(Duration::from_secs(30)).unwrap();
}

#[test]
fn dataset_test_golden_file() {
    let dir = temp_dir("expected-codes");
    std::fs::write(dir.join("codes.txt"), "AB-1\nCD-2\n").unwrap();
    let d = slash(&dir);
    let r = start("dataset-test-golden-file", &example("dataset-test", "dataset-test-golden-file"), &[("env:EXPECTED_DIR", d.clone())], |_| Ok(()));
    let t = r.ctx.create_producer_template();
    t.send_body("direct:normalize", "ab-1").unwrap();
    t.send_body("direct:normalize", "cd-2").unwrap();
    let uri = format!("dataset-test:file:{}?noop=true&split=true", d);
    cameleer_core::component::dataset::dataset_test_endpoint(&r.ctx, &uri).unwrap().assert_is_satisfied().unwrap();
}

#[test]
fn test_any_order() {
    let dir = temp_dir("expected-devices");
    std::fs::write(dir.join("devices.txt"), "device:a\ndevice:b\n").unwrap();
    let d = slash(&dir);
    let r = start("test-any-order", &example("test", "test-any-order"), &[("env:EXPECTED_DIR", d.clone())], |_| Ok(()));
    let t = r.ctx.create_producer_template();
    t.send_body("direct:device", "b").unwrap();
    t.send_body("direct:device", "a").unwrap();
    let uri = format!("test:file:{}?noop=true&split=true&anyOrder=true", d);
    cameleer_core::component::dataset::dataset_test_endpoint(&r.ctx, &uri).unwrap().assert_is_satisfied().unwrap();
}

#[test]
fn bean_fraud_score() {
    let r = start("bean-fraud-score", &example("bean", "bean-fraud-score"), &[], |ctx| {
        bean_register::register(ctx);
        Ok(())
    });
    let t = r.ctx.create_producer_template();
    assert_eq!(t.request_body("direct:score", "2500").unwrap().to_string(), "risk=high");
    assert_eq!(t.request_body("direct:score", "10").unwrap().to_string(), "risk=low");
}

#[test]
fn class_greeter() {
    let r = start("class-greeter", &example("class", "class-greeter"), &[], |ctx| {
        class_register::register(ctx);
        Ok(())
    });
    assert_eq!(r.ctx.create_producer_template().request_body("direct:greet", "Ada").unwrap().to_string(), "Hello Ada");
}

#[test]
fn ref_outbound() {
    let r = start("ref-outbound", &example("ref", "ref-outbound"), &[], |ctx| {
        ctx.registry().bind("outbound", "seda:outbound".to_string());
        Ok(())
    });
    r.ctx.create_producer_template().send_body("direct:publish", "e1").unwrap();
    let ex = r.ctx.create_consumer_template().receive("seda:outbound", Some(Duration::from_secs(5))).unwrap().expect("a message on seda:outbound");
    assert_eq!(ex.body().to_string(), "e1");
    assert_eq!(header(&ex, "source"), "edge-gw-1");
}

#[test]
fn language_prompt() {
    let r = run("language-prompt", &example("language", "language-prompt"));
    let reply = r.ctx.create_producer_template().request_body("direct:prompt", "Printer on floor 3 is jammed").unwrap();
    assert_eq!(reply.to_string(), "Summarize this support ticket in one sentence: Printer on floor 3 is jammed");
}

#[test]
fn dataformat_parse_sensor_json() {
    let r = run("dataformat-parse-sensor-json", &example("dataformat", "dataformat-parse-sensor-json"));
    let reply = r.ctx.create_producer_template().request_body("direct:reading", r#"{"device":"t1","temperature":21.5}"#).unwrap();
    assert_eq!(reply.to_string(), "t1=21.5");
}

#[test]
fn kamelet_timer_to_log() {
    let yaml = replaced(example("kamelet", "kamelet-timer-to-log"), &[("period: 10000", "period: 100"), ("to: log:greetings", "to: mock:greetings")]);
    let r = run("kamelet-timer-to-log", &yaml);
    let got = wait_mock(&r.ctx, "mock:greetings", 1);
    assert_eq!(got[0].body().to_string(), "hello from the edge");
    assert_eq!(header(&got[0], "site"), "edge-1");
}

#[test]
fn saga_book_trip() {
    let yaml = replaced(example("saga", "saga-book-trip"), &[("to: log:compensations", "to: mock:compensations")]);
    let r = run("saga-book-trip", &yaml);
    let t = r.ctx.create_producer_template();
    t.request_body("direct:book", "trip-1").unwrap();
    assert!(t.request_body("direct:book", "trip-2-declined").is_err());
    let got = wait_mock(&r.ctx, "mock:compensations", 1);
    assert_eq!(header(&got[0], "bookingId"), "trip-2-declined");
    assert!(got[0].header("Long-Running-Action").is_some());
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(mock(&r.ctx, "mock:compensations").received_counter(), 1, "the successful booking is not compensated");
}

#[test]
fn exec_tool_version() {
    let yaml = replaced(example("exec", "exec-tool-version"), &[("to: log:versions", "to: mock:versions")]);
    let r = run("exec-tool-version", &yaml);
    let got = wait_mock(&r.ctx, "mock:versions", 1);
    assert!(got[0].body().to_string().contains("git version"), "{}", got[0].body());
    assert_eq!(header(&got[0], "CamelExecExitValue"), "0");
}

#[test]
fn stream_tail_errors() {
    let dir = temp_dir("tail");
    let file = dir.join("app.log");
    std::fs::write(&file, "INFO started\nERROR disk full\n").unwrap();
    let yaml = replaced(example("stream", "stream-tail-errors"), &[("- to: stream:out", "- to: mock:errors")]);
    let r = start("stream-tail-errors", &yaml, &[("env:LOG_FILE", slash(&file))], |_| Ok(()));
    let got = wait_mock(&r.ctx, "mock:errors", 1);
    assert_eq!(got[0].body().to_string(), "ERROR disk full");
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&file).unwrap();
        f.write_all(b"INFO ok\nERROR fan stopped\n").unwrap();
    }
    let got = wait_mock(&r.ctx, "mock:errors", 2);
    assert_eq!(got[1].body().to_string(), "ERROR fan stopped");
}

// ---------------------------------------------------------------------------
// edge protocols
// ---------------------------------------------------------------------------

#[test]
fn syslog_edge_gateway() {
    let port = free_udp_port();
    let yaml = replaced(example("syslog", "syslog-edge-gateway"), &[("to: log:alerts", "to: mock:alerts")]);
    let r = start("syslog-edge-gateway", &yaml, &[("env:SYSLOG_PORT", port.to_string())], |_| Ok(()));
    let t = r.ctx.create_producer_template();
    let target = format!("syslog:udp://127.0.0.1:{}", port);
    t.send_body(&target, "<38>1 2026-11-05T10:00:00Z router1 sshd - - - session opened").unwrap();
    t.send_body(&target, "<34>1 2026-11-05T10:00:01Z router1 sshd - - - authentication failure").unwrap();
    let got = wait_mock(&r.ctx, "mock:alerts", 1);
    assert_eq!(got[0].body().to_string(), "router1: authentication failure");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(mock(&r.ctx, "mock:alerts").received_counter(), 1, "INFO messages are filtered out");
}

#[test]
fn mllp_hl7_gateway() {
    let port = free_port();
    let yaml = replaced(example("mllp", "mllp-hl7-gateway"), &[("to: log:hl7", "to: mock:hl7")]);
    let r = start("mllp-hl7-gateway", &yaml, &[("env:MLLP_PORT", port.to_string())], |_| Ok(()));
    let hl7 = "MSH|^~\\&|LAB|CLINIC|EHR|HOSP|20261105100000||ADT^A01|MSG0001|P|2.5\rPID|1||12345||Doe^John\r";
    let reply = r.ctx.create_producer_template().request(&format!("mllp://127.0.0.1:{}", port), |ex| ex.set_body(hl7)).unwrap();
    assert!(reply.exception.is_none(), "{:?}", reply.exception);
    assert_eq!(header(&reply, "CamelMllpAcknowledgementType"), "AA");
    let got = wait_mock(&r.ctx, "mock:hl7", 1);
    assert_eq!(header(&got[0], "CamelMllpMessageControlId"), "MSG0001");
    assert_eq!(header(&got[0], "CamelMllpSendingApplication"), "LAB");
}

#[test]
fn netty_tcp_line_server() {
    let port = free_port();
    let r = start("netty-tcp-line-server", &example("netty", "netty-tcp-line-server"), &[("env:TCP_PORT", port.to_string())], |_| Ok(()));
    let reply = r.ctx.create_producer_template().request_body(&format!("netty:tcp://127.0.0.1:{}?textline=true&sync=true", port), "reading-42").unwrap();
    assert_eq!(reply.to_string(), "ACK reading-42");
}

// ---------------------------------------------------------------------------
// messaging and streaming
// ---------------------------------------------------------------------------

fn kafka_records(broker: &cameleer_kafka_mqtt::testing::KafkaBroker, topic: &str) -> Vec<(String, String)> {
    let partitions = broker.partition_count(topic).unwrap_or(0);
    let mut out = Vec::new();
    for p in 0..partitions {
        for rec in broker.records(topic, p as i32) {
            let s = |b: Option<Vec<u8>>| b.map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
            out.push((s(rec.key), s(rec.value)));
        }
    }
    out
}

#[test]
fn kafka_consume_and_log() {
    let broker = cameleer_kafka_mqtt::testing::KafkaBroker::start().unwrap();
    let yaml = replaced(example("kafka", "kafka-consume-and-log"), &[("to: log:orders", "to: mock:orders")]);
    let r = start("kafka-consume-and-log", &yaml, &[("env:KAFKA_BROKERS", broker.bootstrap())], |_| Ok(()));
    r.ctx.create_producer_template().send_body(&format!("kafka:orders?brokers={}", broker.bootstrap()), "order-1").unwrap();
    let got = wait_mock(&r.ctx, "mock:orders", 1);
    assert_eq!(got[0].body().to_string(), "order-1");
    assert_eq!(header(&got[0], "kafka.TOPIC"), "orders");
    assert!(got[0].header("kafka.OFFSET").is_some());
}

#[test]
fn kafka_agent_event_bus() {
    let broker = cameleer_kafka_mqtt::testing::KafkaBroker::start().unwrap();
    let r = start("kafka-agent-event-bus", &example("kafka", "kafka-agent-event-bus"), &[("env:KAFKA_BROKERS", broker.bootstrap())], |_| Ok(()));
    r.ctx.create_producer_template().send_body_and_header("direct:agent-event", "task 17 started", "agentId", "agent-7").unwrap();
    assert_eq!(kafka_records(&broker, "agent-events"), vec![("agent-7".to_string(), "task 17 started".to_string())]);
}

#[test]
fn paho_sensors_to_kafka() {
    let mqtt = cameleer_kafka_mqtt::testing::MqttBroker::start().unwrap();
    let kafka = cameleer_kafka_mqtt::testing::KafkaBroker::start().unwrap();
    let _r = start("paho-sensors-to-kafka", &example("paho", "paho-sensors-to-kafka"), &[("env:MQTT_URL", mqtt.url()), ("env:KAFKA_BROKERS", kafka.bootstrap())], |_| Ok(()));
    wait_for("the MQTT subscription", Duration::from_secs(10), || !mqtt.subscriptions("edge-gw-1").is_empty());
    mqtt.publish("sensors/room1/temperature", b"21.5", 1, false);
    mqtt.publish("sensors/room1/humidity", b"40", 1, false);
    wait_for("the Kafka record", Duration::from_secs(10), || !kafka_records(&kafka, "temperatures").is_empty());
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(kafka_records(&kafka, "temperatures"), vec![("sensors/room1/temperature".to_string(), "21.5".to_string())]);
}

#[test]
fn paho_mqtt5_retained_status() {
    let mqtt = cameleer_kafka_mqtt::testing::MqttBroker::start().unwrap();
    let _r = start("paho-mqtt5-retained-status", &example("paho-mqtt5", "paho-mqtt5-retained-status"), &[("env:MQTT_URL", mqtt.url())], |_| Ok(()));
    wait_for("the retained status", Duration::from_secs(10), || mqtt.retained("devices/edge-gw-1/status").is_some());
    let status = String::from_utf8(mqtt.retained("devices/edge-gw-1/status").unwrap()).unwrap();
    assert!(status.starts_with(r#"{"gateway":"edge-gw-1","status":"online","seq":"#), "{}", status);
}

#[test]
fn rabbitmq_orders_queue() {
    let broker = cameleer_amqp::testing::AmqpBroker::start().unwrap();
    let yaml = replaced(example("rabbitmq", "rabbitmq-orders-queue"), &[("to: log:orders", "to: mock:orders")]);
    let props = [("env:RABBITMQ_HOST", "127.0.0.1".to_string()), ("env:RABBITMQ_PORT", broker.port().to_string())];
    let r = start("rabbitmq-orders-queue", &yaml, &props, |_| Ok(()));
    wait_for("the consumer", Duration::from_secs(10), || broker.consumer_count("eu-orders") > 0);
    let uri = format!("rabbitmq:orders?hostname=127.0.0.1&portNumber={}&routingKey=eu&autoDelete=false", broker.port());
    r.ctx.create_producer_template().send_body(&uri, "order-1").unwrap();
    let got = wait_mock(&r.ctx, "mock:orders", 1);
    assert_eq!(got[0].body().to_string(), "order-1");
    wait_for("the ack", Duration::from_secs(5), || broker.unacked_count("eu-orders") == 0 && broker.queue_depth("eu-orders") == 0);
}

#[test]
fn spring_rabbitmq_telemetry() {
    let broker = cameleer_amqp::testing::AmqpBroker::start().unwrap();
    let yaml = replaced(example("spring-rabbitmq", "spring-rabbitmq-telemetry"), &[("to: log:telemetry", "to: mock:telemetry")]);
    let props = [("env:RABBITMQ_HOST", "127.0.0.1".to_string()), ("env:RABBITMQ_PORT", broker.port().to_string())];
    let r = start("spring-rabbitmq-telemetry", &yaml, &props, |_| Ok(()));
    wait_for("the consumer", Duration::from_secs(10), || broker.consumer_count("edge-readings") > 0);
    r.ctx.create_producer_template().send_body("direct:reading", "21.5").unwrap();
    let got = wait_mock(&r.ctx, "mock:telemetry", 1);
    assert_eq!(got[0].body().to_string(), "21.5");
    assert_eq!(header(&got[0], "CamelRabbitmqRoutingKey"), "sensor.temperature");
    assert_eq!(broker.exchange_type("telemetry").as_deref(), Some("topic"));
}

#[test]
fn stomp_alerts() {
    let broker = cameleer_amqp::testing::StompBroker::builder().user("guest", "guest").start().unwrap();
    let yaml = replaced(example("stomp", "stomp-alerts"), &[("to: log:alerts", "to: mock:alerts")]);
    let r = start("stomp-alerts", &yaml, &[("env:STOMP_URL", broker.url())], |_| Ok(()));
    let uri = format!("stomp:queue:alerts?brokerURL={}&login=guest&passcode=guest", broker.url());
    r.ctx.create_producer_template().send_body_and_header(&uri, "disk full", "site", "edge-1").unwrap();
    let got = wait_mock(&r.ctx, "mock:alerts", 1);
    assert_eq!(got[0].body().to_string(), "disk full");
    assert_eq!(header(&got[0], "destination"), "/queue/alerts");
    assert_eq!(header(&got[0], "site"), "edge-1");
}

#[test]
fn jms_request_reply() {
    let broker = cameleer_amqp::testing::StompBroker::builder().user("svc", "s3cret").start().unwrap();
    let props = [("env:JMS_BROKER_URL", broker.url()), ("env:JMS_USER", "svc".to_string()), ("env:JMS_PASSWORD", "s3cret".to_string())];
    let r = start("jms-request-reply", &example("jms", "jms-request-reply"), &props, |_| Ok(()));
    let uri = format!("jms:queue:prices?brokerURL={}&username=svc&password=s3cret", broker.url());
    let reply = r.ctx.create_producer_template().request_body(&uri, "ACME").unwrap();
    assert_eq!(reply.to_string(), "price of ACME: 42.00 EUR");
}

#[test]
fn activemq_selector() {
    let broker = cameleer_amqp::testing::StompBroker::builder().activemq().start().unwrap();
    let yaml = replaced(example("activemq", "activemq-selector"), &[("to: log:eu-orders", "to: mock:eu-orders")]);
    let r = start("activemq-selector", &yaml, &[("env:ACTIVEMQ_URL", broker.url())], |_| Ok(()));
    let t = r.ctx.create_producer_template();
    let uri = format!("activemq:queue:orders?brokerURL={}", broker.url());
    t.send_body_and_header(&uri, "us-order", "region", "US").unwrap();
    t.send_body_and_header(&uri, "eu-order", "region", "EU").unwrap();
    let got = wait_mock(&r.ctx, "mock:eu-orders", 1);
    assert_eq!(got[0].body().to_string(), "eu-order");
    assert!(got[0].header("JMSMessageID").is_some());
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(mock(&r.ctx, "mock:eu-orders").received_counter(), 1, "the US order is not selected");
}

#[test]
fn amqp_stomp_work_queue() {
    let broker = cameleer_amqp::testing::StompBroker::start().unwrap();
    let yaml = replaced(example("amqp-stomp", "amqp-stomp-work-queue"), &[("to: log:jobs", "to: mock:jobs")]);
    let r = start("amqp-stomp-work-queue", &yaml, &[("env:STOMP_BROKER_URL", broker.url())], |_| Ok(()));
    let t = r.ctx.create_producer_template();
    for i in 0..3 {
        t.send_body(&format!("amqp-stomp:queue:jobs?brokerURL={}", broker.url()), format!("job-{}", i)).unwrap();
    }
    let got = wait_mock(&r.ctx, "mock:jobs", 3);
    let mut bodies: Vec<String> = got.iter().map(|e| e.body().to_string()).collect();
    bodies.sort();
    assert_eq!(bodies, vec!["job-0", "job-1", "job-2"]);
}

#[test]
fn amqp_queue_processing() {
    let broker = cameleer_amqp10::testing::Amqp10Broker::builder().user("app", "app-pw").start().unwrap();
    let url = broker.url();
    let props = [("env:AMQP_URL", url.clone()), ("env:AMQP_USER", "app".to_string()), ("env:AMQP_PASSWORD", "app-pw".to_string())];
    let url2 = url.clone();
    let r = start("amqp-queue-processing", &example("amqp", "amqp-queue-processing"), &props, move |ctx| {
        ctx.add_routes(move |b| {
            b.from(format!("amqp:queue:orders.processed?remoteAddress={}&username=app&password=app-pw", url2)).to("mock:processed");
        })
    });
    let uri = format!("amqp:queue:orders?remoteAddress={}&username=app&password=app-pw", url);
    r.ctx.create_producer_template().send_body(&uri, "order-1").unwrap();
    let got = wait_mock(&r.ctx, "mock:processed", 1);
    assert_eq!(got[0].body().to_string(), "order-1");
    assert_eq!(header(&got[0], "processedBy"), "cameleer");
}

// ---------------------------------------------------------------------------
// Rust DSL snippets, copied verbatim from the pages
// ---------------------------------------------------------------------------

/// Every snippet builds its routes on a fresh context (nothing is started or connected).
#[test]
fn rust_dsl_snippets_build() {
    let all: Vec<(&str, fn(&CamelContext) -> Result<()>)> = vec![
        ("direct", direct_dsl::routes),
        ("seda", seda_dsl::routes),
        ("vm", vm_dsl::routes),
        ("stub", stub_dsl::routes),
        ("timer", timer_dsl::routes),
        ("scheduler", scheduler_dsl::routes),
        ("cron", cron_dsl::routes),
        ("quartz", quartz_dsl::routes),
        ("log", log_dsl::routes),
        ("mock", mock_dsl::routes),
        ("browse", browse_dsl::routes),
        ("controlbus", controlbus_dsl::routes),
        ("dataset", dataset_dsl::routes),
        ("dataset-test", dataset_test_dsl::routes),
        ("test", test_dsl::routes),
        ("bean", bean_dsl::routes),
        ("class", class_dsl::routes),
        ("ref", ref_dsl::routes),
        ("language", language_dsl::routes),
        ("dataformat", dataformat_dsl::routes),
        ("kamelet", kamelet_dsl::routes),
        ("saga", saga_dsl::routes),
        ("exec", exec_dsl::routes),
        ("stream", stream_dsl::routes),
        ("mllp", mllp_dsl::routes),
        ("syslog", syslog_dsl::routes),
        ("netty", netty_dsl::routes),
        ("kafka", kafka_dsl::routes),
        ("paho", paho_dsl::routes),
        ("paho-mqtt5", paho_mqtt5_dsl::routes),
        ("spring-rabbitmq", spring_rabbitmq_dsl::routes),
        ("rabbitmq", rabbitmq_dsl::routes),
        ("jms", jms_dsl::routes),
        ("activemq", activemq_dsl::routes),
        ("amqp-stomp", amqp_stomp_dsl::routes),
        ("stomp", stomp_dsl::routes),
        ("amqp", amqp_dsl::routes),
    ];
    let mut covered = BTreeMap::new();
    for (name, f) in all {
        let main = cameleer_main::new_main();
        f(main.context()).unwrap_or_else(|e| panic!("{} snippet: {}", name, e));
        assert!(!main.context().route_ids().is_empty(), "{} snippet adds no route", name);
        covered.insert(name, ());
    }
    for name in PAGES {
        assert!(covered.contains_key(name), "{} has no snippet test", name);
    }
}

mod direct_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("direct:orders")
                .route_id("orders")
                .to("direct:validate")
                .set_body(simple("accepted ${body}"));
            b.from("direct:validate")
                .route_id("validate")
                .filter(simple("${body} == ''"))
                .throw_exception("java.lang.IllegalArgumentException", "empty order")
                .end()
                .set_header("validated", constant(true));
        })
    }
}

mod seda_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("direct:reading")
                .route_id("ingest-readings")
                .to("seda:readings?size=10000&blockWhenFull=true")
                .set_body(constant("queued"));
            b.from("seda:readings?size=10000&concurrentConsumers=2")
                .route_id("store-readings")
                .log("storing ${body}")
                .to("log:stored");
        })
    }
}

mod vm_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("direct:audit").route_id("audit-publisher").to("vm:audit");
            b.from("vm:audit?concurrentConsumers=1")
                .route_id("audit-writer")
                .log("audit: ${body}")
                .to("log:audit");
        })
    }
}

mod stub_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("direct:notify")
                .route_id("notify-shipping")
                .set_body(simple("Order ${body} shipped"))
                .to("stub:smtp://mail.example.com?to=ops@example.com");
        })
    }
}

mod timer_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("timer:heartbeat?period=5000&delay=0&includeMetadata=true")
                .route_id("heartbeat")
                .set_body(simple("edge-gw-1 alive #${exchangeProperty.CamelTimerCounter}"))
                .to("log:heartbeat");
        })
    }
}

mod scheduler_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("scheduler:inventory?initialDelay=1000&delay=60000")
                .route_id("poll-inventory")
                .set_body(constant("refresh"))
                .to("log:inventory");
        })
    }
}

mod cron_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("cron:nightly?schedule=0+0+2+*+*+?&timeZone=Europe/Paris")
                .route_id("nightly-report")
                .set_body(simple("report fired at ${header.fireTime}"))
                .to("log:reports");
        })
    }
}

mod quartz_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("quartz:edge/sync-devices?cron=0+0/15+*+*+*+?&trigger.timeZone=UTC")
                .route_id("sync-devices")
                .set_body(constant("sync"))
                .to("log:sync");
        })
    }
}

mod log_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("direct:orders")
                .route_id("debug-orders")
                .to("log:orders?level=DEBUG&showHeaders=true&showBodyType=false&maxChars=500")
                .set_body(simple("logged ${body}"));
        })
    }
}

mod mock_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("direct:temperature")
                .route_id("route-temperatures")
                .choice()
                .when(simple("${body} > 80"))
                .to("mock:alerts")
                .otherwise()
                .to("mock:normal")
                .end();
        })
    }
}

mod browse_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("direct:reject")
                .route_id("record-rejections")
                .set_header("rejectedAt", simple("${date:now:yyyy-MM-dd'T'HH:mm:ss}"))
                .to("browse:rejected?browseLimit=100");
        })
    }
}

mod controlbus_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("timer:ingest?period=60000").route_id("ingest").to("log:ingest");
            b.from("direct:status")
                .route_id("ingest-status")
                .to("controlbus:route?routeId=ingest&action=status");
            b.from("direct:pause")
                .route_id("ingest-pause")
                .to("controlbus:route?routeId=ingest&action=stop");
        })
    }
}

mod dataset_dsl {
    use cameleer_core::component::dataset::{bind_dataset, SimpleDataSet};
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        bind_dataset(ctx.registry(), "orders", SimpleDataSet::new(500).with_body("order"));
        ctx.add_routes(|b| {
            b.from("dataset:orders?produceDelay=1&initialDelay=100")
                .route_id("load-source")
                .to("seda:work");
            b.from("seda:work")
                .route_id("system-under-test")
                .set_header("processed", constant(true))
                .to("dataset:orders");
        })
    }

    /// The data set binding of the page, for the YAML example.
    pub fn routes_bind_only(ctx: &CamelContext) {
        bind_dataset(ctx.registry(), "orders", SimpleDataSet::new(500).with_body("order"));
    }
}

mod dataset_test_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("direct:normalize")
                .route_id("normalize-codes")
                .set_body(simple("${body.toUpperCase()}"))
                .to("dataset-test:file:expected?noop=true&split=true");
        })
    }
}

mod test_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("direct:device")
                .route_id("enrich-devices")
                .set_body(simple("device:${body}"))
                .to("test:file:expected?noop=true&split=true&anyOrder=true");
        })
    }
}

mod bean_register {
    use cameleer_core::component::bean::{bind_bean, BeanMethods};
    use cameleer_core::prelude::*;

    pub fn register(ctx: &CamelContext) {
        let scorer = BeanMethods::new()
            .method_value("score", |ex: &Exchange| {
                let amount: f64 = ex.body().to_string().parse().unwrap_or(0.0);
                Ok(Value::from(if amount > 1000.0 { "high" } else { "low" }))
            })
            .method("tag", |ex: &mut Exchange| {
                ex.set_header("scored", true);
                Ok(())
            });
        bind_bean(ctx.registry(), "fraudScorer", scorer);
    }
}

mod bean_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("direct:score")
                .route_id("score-payment")
                .to("bean:fraudScorer?method=score")
                .set_body(simple("risk=${body}"));
        })
    }
}

mod class_register {
    use cameleer_core::component::bean::{bind_bean, BeanMethods};
    use cameleer_core::prelude::*;

    pub fn register(ctx: &CamelContext) {
        let greeter = BeanMethods::new().method_value("greet", |ex: &Exchange| Ok(Value::from(format!("Hello {}", ex.body()))));
        bind_bean(ctx.registry(), "com.example.Greeter", greeter);
    }
}

mod class_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("direct:greet").route_id("greet").to("class:com.example.Greeter?method=greet");
        })
    }
}

mod ref_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.registry().bind("outbound", "seda:outbound".to_string());
        ctx.add_routes(|b| {
            b.from("direct:publish")
                .route_id("publish-events")
                .set_header("source", constant("edge-gw-1"))
                .to("ref:outbound");
        })
    }
}

mod language_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("direct:prompt")
                .route_id("build-prompt")
                .to("language:simple:Summarize this support ticket in one sentence: ${body}");
        })
    }
}

mod dataformat_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("direct:reading")
                .route_id("parse-reading")
                .to("dataformat:json:unmarshal")
                .set_body(simple("${body[device]}=${body[temperature]}"));
        })
    }
}

mod kamelet_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        cameleer_yaml::kamelet::register_kamelet_component(ctx);
        ctx.add_routes(|b| {
            b.from("kamelet:timer-source?message=hello from the edge&period=10000")
                .route_id("greetings")
                .to("kamelet:insert-header-action?name=site&value=edge-1")
                .to("log:greetings");
        })
    }
}

mod saga_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("direct:book")
                .route_id("book-trip")
                .saga()
                .compensation("direct:cancel-booking")
                .option("bookingId", simple("${body}"))
                .to("direct:reserve-hotel")
                .to("direct:charge-card")
                .end();
            b.from("direct:reserve-hotel").route_id("reserve-hotel").log("hotel reserved for ${body}");
            b.from("direct:charge-card")
                .route_id("charge-card")
                .filter(simple("${body} contains 'declined'"))
                .throw_exception("java.lang.IllegalStateException", "card declined")
                .end();
            b.from("direct:cancel-booking")
                .route_id("cancel-booking")
                .log("cancelling ${header.bookingId}")
                .to("log:compensations");
        })
    }
}

mod exec_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("timer:version?repeatCount=1&delay=0")
                .route_id("record-tool-version")
                .to("exec:git?args=--version&timeout=5000")
                .log("exit ${header.CamelExecExitValue}: ${body}")
                .to("log:versions");
        })
    }
}

mod stream_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("stream:file?fileName=/var/log/app.log&scanStream=true&scanStreamDelay=500")
                .route_id("tail-errors")
                .filter(simple("${body} contains 'ERROR'"))
                .to("stream:out")
                .end();
        })
    }
}

mod mllp_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("mllp://0.0.0.0:2575")
                .route_id("hl7-inbound")
                .log("HL7 ${header.CamelMllpMessageType} from ${header.CamelMllpSendingApplication}, control id ${header.CamelMllpMessageControlId}")
                .to("log:hl7");
        })
    }
}

mod syslog_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("syslog:udp://0.0.0.0:5514")
                .route_id("syslog-gateway")
                .filter(simple("${header.CamelSyslogSeverity} in 'EMERG,ALERT,CRIT,ERR'"))
                .set_body(simple("${header.CamelSyslogHostname}: ${body[logMessage]}"))
                .to("log:alerts")
                .end();
        })
    }
}

mod netty_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("netty:tcp://0.0.0.0:5150?textline=true&sync=true")
                .route_id("tcp-line-server")
                .set_body(simple("ACK ${body}"));
        })
    }
}

mod kafka_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("kafka:orders?brokers=localhost:9092&groupId=billing&autoOffsetReset=earliest")
                .route_id("orders-consumer")
                .log("partition ${header[kafka.PARTITION]} offset ${header[kafka.OFFSET]}: ${body}")
                .to("log:orders");
            b.from("direct:agent-event")
                .route_id("publish-agent-event")
                .set_header("kafka.KEY", simple("${header.agentId}"))
                .to("kafka:agent-events?brokers=localhost:9092");
        })
    }
}

mod paho_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("paho:sensors/+/temperature?brokerUrl=tcp://localhost:1883&clientId=edge-gw-1&qos=1")
                .route_id("sensors-to-kafka")
                .set_header("kafka.KEY", simple("${header.CamelMqttTopic}"))
                .to("kafka:temperatures?brokers=localhost:9092");
        })
    }
}

mod paho_mqtt5_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("timer:status?period=30000&delay=0")
                .route_id("publish-gateway-status")
                .set_body(simple(r#"{"gateway":"edge-gw-1","status":"online","seq":${exchangeProperty.CamelTimerCounter}}"#))
                .to("paho-mqtt5:devices/edge-gw-1/status?brokerUrl=tcp://localhost:1883&qos=1&retained=true");
        })
    }
}

mod spring_rabbitmq_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("spring-rabbitmq:telemetry?host=localhost&port=5672&exchangeType=topic&queues=edge-readings&routingKey=sensor.*")
                .route_id("telemetry-consumer")
                .log("${header.CamelRabbitmqRoutingKey}: ${body}")
                .to("log:telemetry");
            b.from("direct:reading")
                .route_id("telemetry-publisher")
                .to("spring-rabbitmq:telemetry?host=localhost&port=5672&exchangeType=topic&routingKey=sensor.temperature");
        })
    }
}

mod rabbitmq_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("rabbitmq:orders?hostname=localhost&portNumber=5672&queue=eu-orders&routingKey=eu&autoAck=false&autoDelete=false")
                .route_id("eu-orders")
                .log("EU order: ${body}")
                .to("log:orders");
        })
    }
}

mod jms_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("jms:queue:prices?brokerURL=tcp://localhost:61613&concurrentConsumers=2")
                .route_id("price-service")
                .set_body(simple("price of ${body}: 42.00 EUR"));
        })
    }
}

mod activemq_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("activemq:queue:orders?brokerURL=tcp://localhost:61613&selector=region = 'EU'")
                .route_id("eu-orders")
                .log("EU order ${header.JMSMessageID}: ${body}")
                .to("log:eu-orders");
        })
    }
}

mod amqp_stomp_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("amqp-stomp:queue:jobs?brokerURL=tcp://localhost:61613&concurrentConsumers=3")
                .route_id("work-queue")
                .log("job ${body}")
                .to("log:jobs");
        })
    }
}

mod stomp_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.add_routes(|b| {
            b.from("stomp:queue:alerts?brokerURL=tcp://localhost:61613&login=guest&passcode=guest")
                .route_id("stomp-alerts")
                .log("alert on ${header.destination}: ${body}")
                .to("log:alerts");
        })
    }
}

mod amqp_dsl {
    use cameleer_core::prelude::*;

    pub fn routes(ctx: &CamelContext) -> Result<()> {
        ctx.properties().set("camel.component.amqp.remoteAddress", "amqp://localhost:5672");
        ctx.add_routes(|b| {
            b.from("amqp:queue:orders?acknowledgementModeName=CLIENT_ACKNOWLEDGE")
                .route_id("process-orders")
                .set_header("processedBy", constant("cameleer"))
                .to("amqp:queue:orders.processed");
        })
    }
}

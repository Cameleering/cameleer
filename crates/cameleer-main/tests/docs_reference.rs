//! Tests for the reference guides in `docs/guides/` (configuration, CLI): the files and
//! commands shown on the pages are taken from the markdown, run, and checked against what the
//! pages say they do.

use std::collections::BTreeMap;
use std::path::PathBuf;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn page(name: &str) -> String {
    std::fs::read_to_string(repo().join("docs/guides").join(name)).unwrap().replace("\r\n", "\n")
}

/// The fenced block right after `marker` (an HTML comment line) in `text`.
fn block_after(text: &str, marker: &str) -> String {
    let i = text.find(marker).unwrap_or_else(|| panic!("{} not found", marker));
    let rest = &text[i..];
    let start = rest.find("```").unwrap();
    let body_start = start + rest[start..].find('\n').unwrap() + 1;
    let end = body_start + rest[body_start..].find("\n```").unwrap();
    rest[body_start..end + 1].to_string()
}

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("cameleer-docs-ref-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// docs/guides/configuration.md, "A worked example": every row of the table.
#[test]
fn configuration_guide_worked_example() {
    let text = page("configuration.md");
    let route = block_after(&text, "<!-- example: configuration-orders -->");
    let app = block_after(&text, "<!-- file: application.properties -->");
    let prod = block_after(&text, "<!-- file: application-prod.properties -->");

    struct Row {
        profile: Option<&'static str>,
        env: &'static [(&'static str, &'static str)],
        name_flag: Option<&'static str>,
        target: &'static str,
        name: &'static str,
        port: &'static str,
    }
    let rows = [
        Row { profile: None, env: &[], name_flag: None, target: "log:orders?level=INFO", name: "orders", port: "8080" },
        Row { profile: Some("dev"), env: &[], name_flag: None, target: "log:orders?level=DEBUG", name: "orders", port: "8080" },
        Row { profile: Some("prod"), env: &[("PORT", "7070")], name_flag: None, target: "log:orders?level=WARN", name: "orders", port: "7070" },
        Row { profile: None, env: &[("CAMEL_MAIN_NAME", "orders-eu")], name_flag: None, target: "log:orders?level=INFO", name: "orders-eu", port: "8080" },
        Row { profile: None, env: &[("CAMEL_MAIN_NAME", "orders-eu")], name_flag: Some("orders-test"), target: "log:orders?level=INFO", name: "orders-test", port: "8080" },
    ];
    for (i, row) in rows.iter().enumerate() {
        let dir = temp_dir(&format!("cfg{}", i));
        std::fs::write(dir.join("application.properties"), &app).unwrap();
        std::fs::write(dir.join("application-prod.properties"), &prod).unwrap();
        let mut main = cameleer_main::new_main();
        // the process environment of the row, nothing else
        let env: BTreeMap<String, String> = row.env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        // {{env:PORT}} is resolved through the process environment by the placeholder resolver
        for (k, v) in row.env {
            if *k == "PORT" {
                std::env::set_var("PORT", v);
            }
        }
        main.set_environment(env);
        main.load_application_config(&dir).unwrap();
        if let Some(p) = row.profile {
            main.set_profile(p);
        }
        if let Some(n) = row.name_flag {
            main.set_property("camel.main.name", n);
        }
        main.add_route_source("orders.camel.yaml", route.clone());
        main.start().unwrap();
        let props = main.properties().clone();
        let get = |k: &str| cameleer_main::config::relaxed_get(&props, k).cloned();
        let target = main.context().properties().resolve("{{orders.target}}").unwrap();
        assert_eq!(target, row.target, "row {}", i);
        assert_eq!(get("camel.main.name").as_deref(), Some(row.name), "row {}", i);
        assert_eq!(get("camel.server.port").unwrap_or_else(|| "8080".into()), row.port, "row {}", i);
        main.stop().unwrap();
        std::env::remove_var("PORT");
    }
}

fn cameleer(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_cameleer"))
        .args(args)
        .current_dir(dir)
        .env("CAMEL_LOCAL_STATUS", "false")
        .output()
        .unwrap()
}

/// docs/guides/cli.md: every command and every flag on the page is one the binary knows, and
/// every command and `run` flag the binary knows is on the page.
#[test]
fn cli_guide_lists_every_command_and_flag() {
    let text = page("cli.md");
    let usage = String::from_utf8(cameleer(&std::env::temp_dir(), &["help"]).stdout).unwrap();
    // commands: the first word after `cameleer ` on each usage line
    let commands: Vec<&str> = usage.lines().filter_map(|l| l.trim().strip_prefix("cameleer ")).map(|l| l.split([' ', '|']).next().unwrap()).collect();
    assert!(commands.len() >= 18, "{:?}", commands);
    for c in &commands {
        assert!(text.contains(&format!("`{}`", c)) || text.contains(&format!("cameleer {}", c)), "command {} missing from cli.md", c);
    }
    // flags in the usage are on the page, and flags on the page are in the usage
    let flags = |s: &str| -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        let b = s.as_bytes();
        let mut i = 0;
        while i + 2 < b.len() {
            if &b[i..i + 2] == b"--" && (i == 0 || !b[i - 1].is_ascii_alphanumeric() && b[i - 1] != b'-') && b[i + 2].is_ascii_lowercase() {
                let mut j = i + 2;
                while j < b.len() && (b[j].is_ascii_lowercase() || b[j] == b'-') {
                    j += 1;
                }
                out.insert(s[i..j].trim_end_matches('-').to_string());
                i = j;
            } else {
                i += 1;
            }
        }
        out
    };
    let in_usage = flags(&usage);
    let on_page = flags(&text);
    for f in &in_usage {
        assert!(on_page.contains(f), "flag {} missing from cli.md", f);
    }
    // the page may also name --version and --help, docker flags and cargo test --workspace
    for f in &on_page {
        if ["--version", "--help", "--rm", "--read-only", "--workspace"].contains(&f.as_str()) {
            continue;
        }
        assert!(in_usage.contains(f), "cli.md names {} but the binary has no such flag", f);
    }
}

/// docs/guides/cli.md, "A worked example".
#[test]
fn cli_guide_worked_example() {
    let dir = temp_dir("cli");
    let init = cameleer(&dir, &["init", "hello.camel.yaml"]);
    assert!(init.status.success(), "{:?}", init);
    assert!(String::from_utf8_lossy(&init.stdout).contains("hello.camel.yaml"));
    let v = cameleer(&dir, &["validate", "hello.camel.yaml"]);
    assert_eq!(v.status.code(), Some(0), "{}", String::from_utf8_lossy(&v.stdout));
    let run = cameleer(&dir, &["run", "hello.camel.yaml", "--max-messages", "2"]);
    assert!(run.status.success(), "{:?}", run);
    let all = format!("{}{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr));
    assert_eq!(all.matches("Hello Camel from route1").count(), 2, "{}", all);
}

/// docs/guides/testing.md, "A worked example" and the intercept file: `cameleer test` with the
/// page's route and test files, then the same orders with the intercept file listed after them.
#[test]
fn testing_guide_worked_example() {
    let text = page("testing.md");
    let dir = temp_dir("testing");
    for f in ["orders.camel.yaml", "orders.test.yaml", "intercept-kafka.camel.yaml"] {
        std::fs::write(dir.join(f), block_after(&text, &format!("<!-- file: {} -->", f))).unwrap();
    }
    let out = cameleer(&dir, &["test", "orders.test.yaml"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{}{}", stdout, String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("PASS a high-priority order takes the express lane"), "{}", stdout);
    assert!(stdout.contains("1 passed, 0 failed"), "{}", stdout);
    // the intercept file instead of the properties: nothing connects to a broker
    std::fs::write(
        dir.join("intercept.test.yaml"),
        "- test:\n    name: intercepted\n    routes: [orders.camel.yaml, intercept-kafka.camel.yaml]\n    steps:\n      - send:\n          to: direct:orders\n          body: {id: A3, priority: high}\n        expect:\n          body: accepted A3\n          mocks: {\"mock:kafka\": 1}\n",
    )
    .unwrap();
    let out = cameleer(&dir, &["test", "intercept.test.yaml"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    // a failing expectation fails the command
    std::fs::write(dir.join("orders.test.yaml"), block_after(&text, "<!-- file: orders.test.yaml -->").replace("body: accepted A1", "body: accepted A9")).unwrap();
    let out = cameleer(&dir, &["test", "orders.test.yaml"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("FAIL"));
}

// page: testing.md begin
use cameleer_core::prelude::*;

#[test]
fn high_priority_orders_take_the_express_lane() -> Result<()> {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:orders")
            .choice()
            .when(header("priority").is_equal_to("high"))
            .to("mock:express")
            .otherwise()
            .to("mock:standard")
            .end();
    })?;
    ctx.start()?;

    let express = ctx.mock_endpoint("mock:express")?;
    express.expected_message_count(1);
    express.expected_bodies_received(vec!["A1"]);

    let template = ctx.create_producer_template();
    template.send_body_and_header("direct:orders", "A1", "priority", "high")?;
    template.send_body_and_header("direct:orders", "A2", "priority", "low")?;

    express.assert_is_satisfied()?;
    assert_eq!(ctx.mock_endpoint("mock:standard")?.received_counter(), 1);
    ctx.stop()
}
// page: testing.md end

/// docs/guides/testing.md, "Rust tests": the page shows the test above, as it is.
#[test]
fn testing_guide_rust_example() {
    let src = std::fs::read_to_string(repo().join("crates/cameleer-main/tests/docs_reference.rs")).unwrap().replace("\r\n", "\n");
    let start = src.find("// page: testing.md begin\n").unwrap() + "// page: testing.md begin\n".len();
    let end = src.find("// page: testing.md end").unwrap();
    let text = page("testing.md");
    let i = text.find("```rust\nuse cameleer_core::prelude::*;").unwrap() + "```rust\n".len();
    let j = i + text[i..].find("```").unwrap();
    assert_eq!(&text[i..j], &src[start..end], "the Rust example on testing.md differs from the test");
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn http(method: &str, url: &str) -> (u16, String) {
    let client = cameleer_http::HttpClient { connect_timeout: std::time::Duration::from_secs(5), ..Default::default() };
    let r = match method {
        "POST" => client.post(url, "text/plain", b""),
        _ => client.get(url),
    }
    .unwrap();
    (r.status, r.body_text())
}

/// docs/guides/observability.md, "A worked example": the real binary with the page's route;
/// the management server is turned on with the environment variables shown on the page.
#[test]
fn observability_guide_worked_example() {
    let text = page("observability.md");
    let dir = temp_dir("obs");
    std::fs::write(dir.join("ticks.camel.yaml"), block_after(&text, "<!-- example: observability-ticks -->")).unwrap();
    let port = free_port();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_cameleer"))
        .args(["run", "ticks.camel.yaml", "--port", &port.to_string(), "--max-seconds", "30"])
        .current_dir(&dir)
        .env("CAMEL_LOCAL_STATUS", "false")
        .env("CAMEL_SERVER_ENABLED", "true")
        .env("CAMEL_SERVER_HEALTH_CHECK_ENABLED", "true")
        .env("CAMEL_SERVER_METRICS_ENABLED", "true")
        .env("CAMEL_SERVER_DEV_CONSOLE_ENABLED", "true")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let base = format!("http://127.0.0.1:{}", port);
    let result = std::panic::catch_unwind(|| {
        // ready once the route has run a few times
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let ok = cameleer_http::HttpClient::default().get(&format!("{}/q/metrics", base)).map(|r| r.body_text().contains("routeId=\"ticks\"} 2")).unwrap_or(false);
            if ok || std::time::Instant::now() > deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let (s, body) = http("GET", &format!("{}/q/health/ready", base));
        assert_eq!(s, 200, "{}", body);
        for check in ["\"name\":\"context\"", "\"name\":\"routes\"", "\"name\":\"consumers\""] {
            assert!(body.contains(check), "{}", body);
        }
        let (_, metrics) = http("GET", &format!("{}/q/metrics", base));
        for m in ["camel_exchanges_total{camelContext=", "camel_route_policy_seconds_count{", "camel_routes_running_routes{", "camel_context_uptime_seconds{"] {
            assert!(metrics.contains(m), "{} missing from {}", m, metrics);
            assert!(text.contains(m.trim_end_matches('{').trim_end_matches("{camelContext=")), "{} not on the page", m);
        }
        let (s, body) = http("POST", &format!("{}/q/dev/route/ticks/suspend", base));
        assert_eq!(s, 200, "{}", body);
        assert!(body.contains("\"state\":\"Suspended\""), "{}", body);
        let (s, body) = http("GET", &format!("{}/q/health/ready", base));
        assert_eq!(s, 503, "{}", body);
        assert!(body.contains("Routes not started: ticks"), "{}", body);
        assert_eq!(http("GET", &format!("{}/q/health/live", base)).0, 200);
    });
    let _ = child.kill();
    let _ = child.wait();
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}

/// docs/guides/tls-and-security.md, "A worked example": the page's server route over HTTPS with
/// `camel.server.ssl.*`, and the page's client test file run by `cameleer test`. The certificate is
/// the repository's long-lived test certificate (`cameleer-tls/testdata/rsa_selfsigned.pem`, valid
/// for 127.0.0.1), so the client calls 127.0.0.1 instead of localhost.
#[test]
fn tls_guide_worked_example() {
    let text = page("tls-and-security.md");
    let dir = temp_dir("tls");
    let testdata = repo().join("crates/cameleer-tls/testdata");
    std::fs::copy(testdata.join("rsa_selfsigned.pem"), dir.join("server.crt")).unwrap();
    std::fs::copy(testdata.join("rsa2048.pkcs8.pem"), dir.join("server.key")).unwrap();
    std::fs::write(dir.join("server.camel.yaml"), block_after(&text, "<!-- file: server.camel.yaml -->")).unwrap();
    std::fs::write(dir.join("client.camel.yaml"), block_after(&text, "<!-- file: client.camel.yaml -->").replace("https://localhost:", "https://127.0.0.1:")).unwrap();
    let port = free_port();
    std::fs::write(dir.join("client.test.yaml"), block_after(&text, "<!-- file: client.test.yaml -->").replace("server.port: 8443", &format!("server.port: {}", port))).unwrap();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_cameleer"))
        .args(["run", "server.camel.yaml", "--port", &port.to_string(), "--max-seconds", "60"])
        .args(["--prop", "camel.server.sslEnabled=true", "--prop", "camel.server.ssl.certFile=server.crt", "--prop", "camel.server.ssl.keyFile=server.key"])
        .current_dir(&dir)
        .env("CAMEL_LOCAL_STATUS", "false")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let out = cameleer(&dir, &["test", "client.test.yaml"]);
    let _ = child.kill();
    let _ = child.wait();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{}", stdout);
    assert!(stdout.contains("2 passed, 0 failed"), "{}", stdout);
}

/// docs/guides/observability.md, "A separate management port": the page's properties, with a
/// free port, serve health and metrics on the management port.
#[test]
fn observability_guide_management_port() {
    let text = page("observability.md");
    let i = text.find("## A separate management port").unwrap();
    let props = block_after(&text[i..], "## A separate management port");
    let mgmt = free_port();
    let props = props.replace("camel.management.port = 9876", &format!("camel.management.port = {}", mgmt));
    assert!(props.contains(&format!("camel.management.port = {}", mgmt)), "{}", props);
    let dir = temp_dir("mgmt");
    std::fs::write(dir.join("application.properties"), props.lines().map(|l| l.split('#').next().unwrap().trim_end()).collect::<Vec<_>>().join("\n")).unwrap();
    std::fs::write(dir.join("ticks.camel.yaml"), block_after(&text, "<!-- example: observability-ticks -->")).unwrap();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_cameleer"))
        .args(["run", "ticks.camel.yaml", "--max-seconds", "30"])
        .current_dir(&dir)
        .env("CAMEL_LOCAL_STATUS", "false")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let base = format!("http://127.0.0.1:{}", mgmt);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while std::net::TcpStream::connect(("127.0.0.1", mgmt)).is_err() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let health = http("GET", &format!("{}/q/health", base));
    let metrics = http("GET", &format!("{}/q/metrics", base));
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(health.0, 200, "{}", health.1);
    assert_eq!(metrics.0, 200, "{}", metrics.1);
    assert!(metrics.1.contains("camel_routes_running_routes"), "{}", metrics.1);
}

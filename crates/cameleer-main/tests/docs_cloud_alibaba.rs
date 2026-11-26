//! Tests for `docs/components/alibaba-sls.md`: every YAML example is read from the page (never
//! duplicated here) and run end to end against the embedded Simple Log Service emulator, and the
//! page's Rust DSL block is the code of `rust_dsl_alibaba_sls` below.

// needs every component family (the default features)
#![cfg(feature = "full")]

use std::io::{Read, Write};
use std::net::TcpStream;

use cameleer_cloud::testing::alibaba::{StoredLog, ACCESS_KEY_ID, ACCESS_KEY_SECRET};
use cameleer_cloud::testing::SlsMock;
use cameleer_core::prelude::*;

const PAGE: &str = "components/alibaba-sls.md";

fn page() -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs").join(PAGE);
    std::fs::read_to_string(p).unwrap().replace("\r\n", "\n")
}

/// The yaml block after `<!-- example: id -->`.
fn example(id: &str) -> String {
    let text = page();
    let marker = format!("<!-- example: {} -->\n```yaml\n", id);
    let start = text.find(&marker).unwrap_or_else(|| panic!("no example {}", id)) + marker.len();
    text[start..start + text[start..].find("```").unwrap()].to_string()
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn start(mock: &SlsMock, id: &str) -> (cameleer_main::Main, u16) {
    let port = free_port();
    let mut main = cameleer_main::new_main();
    main.set_property("camel.server.port", port.to_string());
    // the AccessKey of the emulator (the page reads it from the environment) and its address
    main.set_property("camel.component.alibaba-sls.accessKey", ACCESS_KEY_ID);
    main.set_property("camel.component.alibaba-sls.secretKey", ACCESS_KEY_SECRET);
    main.set_property("camel.component.alibaba-sls.uriEndpointOverride", mock.url());
    main.set_property("camel.component.alibaba-sls.overrideEndpoint", "true");
    main.add_route_source(format!("{}.camel.yaml", id), example(id));
    main.start().unwrap_or_else(|e| panic!("{} does not start: {}", id, e));
    (main, port)
}

fn http(port: u16, method: &str, target: &str, body: &str) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(s, "{} {} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", method, target, body.len(), body).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let status = out.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    let body = out.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
    (status, body)
}

#[test]
fn every_example_is_marked() {
    let text = page();
    let lines: Vec<&str> = text.lines().collect();
    let mut n = 0;
    for (i, l) in lines.iter().enumerate() {
        if *l == "```yaml" {
            assert!(i > 0 && lines[i - 1].starts_with("<!-- example: "), "unmarked yaml block at line {}", i + 1);
            n += 1;
        }
    }
    assert_eq!(n, 2);
    assert!(text.contains("**Cargo feature:** [`cloud`]"));
}

#[test]
fn ship_http_events() {
    let mock = SlsMock::start().unwrap();
    mock.create_project("my-project", &["app-events"]);
    let (main, port) = start(&mock, "alibaba-sls-ship-http-events");
    let (status, body) = http(port, "POST", "/events", r#"{"event":"signup","user":"ann"}"#);
    assert_eq!((status, body.trim()), (200, "stored"));
    let logs = mock.logs("my-project", "app-events");
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].contents, vec![("event".to_string(), "signup".to_string()), ("user".to_string(), "ann".to_string())]);
    main.stop().unwrap();
}

#[test]
fn search_errors() {
    let mock = SlsMock::start().unwrap();
    mock.create_project("my-project", &["app-events"]);
    let log = |time: u32, level: &str, msg: &str| StoredLog { time, topic: String::new(), source: "web-1".into(), tags: vec![], contents: vec![("level".into(), level.into()), ("message".into(), msg.into())] };
    mock.add_logs("my-project", "app-events", vec![log(1_700_000_010, "ERROR", "payment declined"), log(1_700_000_020, "INFO", "order placed"), log(1_700_000_030, "ERROR", "stock missing"), log(1_700_009_000, "ERROR", "later")]);
    let (main, port) = start(&mock, "alibaba-sls-search-errors");
    let (status, body) = http(port, "GET", "/errors?from=1700000000&to=1700003600", "");
    assert_eq!(status, 200, "{}", body);
    let v = cameleer_core::json::parse(body.trim()).unwrap();
    let cameleer_core::value::Value::Array(logs) = v else { panic!("{}", body) };
    // reverse=true: newest first
    let messages: Vec<String> = logs.iter().map(|l| l.get("message").unwrap().to_string()).collect();
    assert_eq!(messages, vec!["stock missing", "payment declined"]);
    main.stop().unwrap();
}

/// `examples/syslog-to-sls.camel.yaml`: warnings and worse reach the Logstore, notices do not.
#[test]
fn syslog_to_sls_example() {
    let mock = SlsMock::start().unwrap();
    mock.create_project("my-project", &["syslog"]);
    let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/syslog-to-sls.camel.yaml");
    let port = std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut main = cameleer_main::new_main();
    main.set_property("syslog.host", "127.0.0.1");
    main.set_property("syslog.port", port.to_string());
    main.set_property("camel.component.alibaba-sls.accessKey", ACCESS_KEY_ID);
    main.set_property("camel.component.alibaba-sls.secretKey", ACCESS_KEY_SECRET);
    main.set_property("camel.component.alibaba-sls.uriEndpointOverride", mock.url());
    main.set_property("camel.component.alibaba-sls.overrideEndpoint", "true");
    main.add_route_source("syslog-to-sls.camel.yaml", std::fs::read_to_string(file).unwrap());
    main.start().unwrap();
    let sock = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    // RFC 5424, local4.notice: not written
    sock.send_to(b"<165>1 2003-10-11T22:14:15.003Z plc-4.site evntslog - ID47 - cycle completed", ("127.0.0.1", port)).unwrap();
    // RFC 3164, auth.crit: written
    sock.send_to(b"<34>Oct 11 22:14:15 router-1 su: 'su root' failed for lonvick on /dev/pts/8", ("127.0.0.1", port)).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while mock.logs("my-project", "syslog").is_empty() {
        assert!(std::time::Instant::now() < deadline, "no log reached the Logstore");
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    let logs = mock.logs("my-project", "syslog");
    assert_eq!(logs.len(), 1, "{:?}", logs);
    assert!(logs[0].contents.iter().any(|(_, v)| v.contains("'su root' failed for lonvick")), "{:?}", logs[0]);
    main.stop().unwrap();
}

fn rust_dsl_alibaba_sls(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("direct:audit")
            .to("alibaba-sls:putLogs?project=my-project&logStoreName=audit&region=cn-hangzhou");
    })?;
    Ok(())
}

#[test]
fn rust_dsl_block_compiles_and_matches_the_page() {
    let main = cameleer_main::new_main();
    let mock = SlsMock::start().unwrap();
    mock.create_project("my-project", &["audit"]);
    let ctx = main.context();
    ctx.properties().set("camel.component.alibaba-sls.accessKey", ACCESS_KEY_ID);
    ctx.properties().set("camel.component.alibaba-sls.secretKey", ACCESS_KEY_SECRET);
    ctx.properties().set("camel.component.alibaba-sls.uriEndpointOverride", &mock.url());
    ctx.properties().set("camel.component.alibaba-sls.overrideEndpoint", "true");
    rust_dsl_alibaba_sls(ctx).unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:audit", "user ann signed in").unwrap();
    assert_eq!(mock.logs("my-project", "audit")[0].contents, vec![("message".to_string(), "user ann signed in".to_string())]);
    ctx.stop().unwrap();

    let text = page();
    let start = text.find("```rust\n").unwrap() + 8;
    let snippet = &text[start..start + text[start..].find("```").unwrap()];
    let indented: String = snippet.lines().map(|l| if l.is_empty() { "\n".to_string() } else { format!("    {}\n", l) }).collect();
    let source = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/docs_cloud_alibaba.rs")).unwrap().replace("\r\n", "\n");
    assert!(source.contains(&format!("fn rust_dsl_alibaba_sls(ctx: &CamelContext) -> Result<()> {{\n{}    Ok(())", indented)), "the Rust DSL block differs from rust_dsl_alibaba_sls");
}

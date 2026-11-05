//! Tests for the examples in the HTTP, web, RPC, file and mail component pages
//! (`docs/components/*.md`). Every example is read from the markdown (the block after its
//! `<!-- example: id -->` marker), its endpoint URIs are resolved against a context with
//! every component (so unknown options fail as in Apache Camel), and wherever an embedded
//! server or mock exists the route runs end to end. The Rust DSL snippets of the pages are
//! copied verbatim below and run or at least compile.

#![allow(clippy::needless_raw_string_hashes)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cameleer_core::prelude::*;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// The fenced block following `<!-- example: id -->` in `docs/components/<page>.md`.
fn example(page: &str, id: &str) -> String {
    let path = format!("{}/../../docs/components/{}.md", env!("CARGO_MANIFEST_DIR"), page);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {}", path, e));
    let text = text.replace("\r\n", "\n");
    let marker = format!("<!-- example: {} -->", id);
    let at = text.find(&marker).unwrap_or_else(|| panic!("{}: no marker {}", page, marker));
    let rest = &text[at + marker.len()..];
    let rest = rest.strip_prefix('\n').unwrap_or_else(|| panic!("{}: marker {} must be directly above the block", page, id));
    assert!(rest.starts_with("```"), "{}: marker {} must be directly above a fenced block", page, id);
    let body = &rest[rest.find('\n').unwrap() + 1..];
    let end = body.find("\n```").unwrap_or_else(|| panic!("{}: unterminated block {}", page, id));
    body[..end + 1].to_string()
}

/// Replaces `{{env:NAME:default}}` (and `{{env:NAME}}`) placeholders: by the value given in
/// `env`, else by the default.
fn subst(text: &str, env: &[(&str, &str)]) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("{{env:") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 6..];
        let end = after.find("}}").expect("unterminated placeholder");
        let inner = &after[..end];
        let (name, default) = match inner.split_once(':') {
            Some((n, d)) => (n, Some(d)),
            None => (inner, None),
        };
        let value = env.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string()).or_else(|| default.map(str::to_string));
        out.push_str(&value.unwrap_or_else(|| panic!("no value for placeholder {}", name)));
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

/// The endpoint URIs of a YAML route (`uri:`, `to:`, `toD:` values and `from:` strings).
fn endpoint_uris(yaml: &str) -> Vec<String> {
    let mut uris = Vec::new();
    for line in yaml.lines() {
        let t = line.trim().trim_start_matches("- ");
        for key in ["uri:", "to:", "toD:", "from:", "wireTap:"] {
            if let Some(v) = t.strip_prefix(key) {
                let v = v.trim().trim_matches('"').trim_matches('\'');
                // toD uris are evaluated per exchange
                if !v.is_empty() && v.contains(':') && !(key == "toD:" && v.contains("${")) {
                    uris.push(v.to_string());
                }
            }
        }
    }
    uris
}

/// Resolves every endpoint URI of the (substituted) YAML on a context with all components.
fn resolve_all(yaml: &str) {
    let main = cameleer_main::new_main();
    let ctx = main.context().clone();
    let uris = endpoint_uris(yaml);
    assert!(!uris.is_empty(), "no endpoint uris in\n{}", yaml);
    for uri in uris {
        if let Err(e) = ctx.get_endpoint(&uri) {
            panic!("cannot resolve {}: {}", uri, e.message);
        }
    }
}

/// A free port below the OS ephemeral ranges (Windows 49152+, Linux 32768+), so servers and
/// clients that ask the OS for port 0 cannot take it between this call and the bind; each
/// port is handed out once per process.
fn free_port() -> u16 {
    use std::sync::atomic::{AtomicU16, Ordering};
    static NEXT: AtomicU16 = AtomicU16::new(0);
    let _ = NEXT.compare_exchange(0, 20000 + (std::process::id() % 400) as u16 * 25, Ordering::SeqCst, Ordering::SeqCst);
    loop {
        let p = NEXT.fetch_add(1, Ordering::SeqCst);
        assert!(p < 32000, "no free port");
        if std::net::TcpListener::bind(("0.0.0.0", p)).is_ok() && std::net::TcpListener::bind(("127.0.0.1", p)).is_ok() {
            return p;
        }
    }
}

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("cameleer-docs-web-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// A minimal HTTP/1.1 request; returns (status, headers, body).
fn http(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> (u16, String, String) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut s = loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(s) => break s,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => panic!("connect {}: {}", port, e),
        }
    };
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut req = format!("{} {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n", method, path, body.len());
    for (k, v) in headers {
        req.push_str(&format!("{}: {}\r\n", k, v));
    }
    req.push_str("\r\n");
    req.push_str(body);
    s.write_all(req.as_bytes()).unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let out = String::from_utf8_lossy(&out).to_string();
    let status = out.split(' ').nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    let (head, body) = out.split_once("\r\n\r\n").map(|(h, b)| (h.to_string(), b.to_string())).unwrap_or_default();
    let body = if head.to_ascii_lowercase().contains("transfer-encoding: chunked") { dechunk(&body) } else { body };
    (status, head, body)
}

fn dechunk(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    loop {
        let Some((size, after)) = rest.split_once("\r\n") else { break };
        let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if n == 0 {
            break;
        }
        out.push_str(&after[..n]);
        rest = &after[n + 2..];
    }
    out
}

fn wait_for(what: &str, f: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {}", what);
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Starts a `Main` with the YAML route (and optional Rust routes / properties).
fn start_yaml(name: &str, yaml: &str, props: &[(&str, String)], extra: impl FnOnce(&CamelContext) + Send + 'static) -> cameleer_main::Main {
    let mut main = cameleer_main::new_main();
    for (k, v) in props {
        main.set_property(*k, v.clone());
    }
    main.add_route_source(&format!("{}.camel.yaml", name), yaml.to_string());
    main.configure(move |ctx| {
        extra(ctx);
        Ok(())
    });
    main.start().unwrap_or_else(|e| panic!("{}: {}", name, e.message));
    main
}

// ---------------------------------------------------------------------------
// http
// ---------------------------------------------------------------------------

fn ask_llm_route(b: &mut RouteBuilder, base_url: &str, api_key: &str) {
    b.from("platform-http:/ask?httpMethodRestrict=POST")
        .route_id("ask-llm")
        .remove_headers("*")
        .set_header("Authorization", constant(format!("Bearer {}", api_key)))
        .set_header("Content-Type", constant("application/json"))
        .set_body(simple(r#"{"model": "gpt-4o-mini", "messages": [{"role": "user", "content": "${body}"}]}"#))
        .to(&format!("{}/chat/completions?httpMethod=POST", base_url))
        .unmarshal_json()
        .set_body(simple("${body[choices][0][message][content]}"))
        .remove_headers("*")
        .set_header("Content-Type", constant("text/plain"));
}

#[test]
fn http_ask_llm() {
    let ai = cameleer_saas::testing::OpenAiMock::start();
    let base = ai.base_url();
    let env = [("LLM_BASE_URL", base.as_str())];
    let yaml = subst(&example("http", "http-ask-llm"), &env);
    resolve_all(&yaml);
    let port = free_port();
    let main = start_yaml("ask-llm", &yaml, &[("camel.server.port", port.to_string())], |_| {});
    let (status, head, body) = http(port, "POST", "/ask", &[("Content-Type", "text/plain")], "What is Apache Camel?");
    assert_eq!(status, 200, "{}", body);
    assert_eq!(body, "Echo: What is Apache Camel?");
    assert!(head.to_ascii_lowercase().contains("content-type: text/plain"), "{}", head);
    let req = ai.server.requests_to("/v1/chat/completions")[0].json();
    assert_eq!(req.get("model").map(|m| m.to_string()).as_deref(), Some("gpt-4o-mini"));
    main.stop().unwrap();

    // the Rust DSL version
    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    let p2 = free_port();
    ctx.properties().set("camel.server.port", &p2.to_string());
    let key = cameleer_saas::testing::OpenAiMock::API_KEY;
    ctx.add_routes(move |b| ask_llm_route(b, &base, key)).unwrap();
    ctx.start().unwrap();
    let (status, _, body) = http(p2, "POST", "/ask", &[], "Hi");
    assert_eq!((status, body.as_str()), (200, "Echo: Hi"));
    ctx.stop().unwrap();
}

// ---------------------------------------------------------------------------
// platform-http, jetty, undertow, netty-http
// ---------------------------------------------------------------------------

fn device_readings_routes(b: &mut RouteBuilder) {
    b.from("platform-http:/devices/{id}/readings?httpMethodRestrict=POST")
        .route_id("device-readings")
        .set_header("deviceId", simple("${header.id}"))
        .wire_tap("seda:readings")
        .set_header("CamelHttpResponseCode", constant(202))
        .set_header("Content-Type", constant("application/json"))
        .set_body(simple(r#"{"device": "${header.id}", "accepted": true}"#));
    b.from("seda:readings").route_id("store-readings").log("device ${header.deviceId}: ${body}");
}

#[test]
fn platform_http_device_readings() {
    let yaml = example("platform-http", "platform-http-device-readings");
    resolve_all(&yaml);
    let port = free_port();
    // observe the queue: the log step is swapped for a mock
    let yaml = yaml.replace("        - log: \"device ${header.deviceId}: ${body}\"", "        - to: mock:stored");
    let main = start_yaml("device-readings", &yaml, &[("camel.server.port", port.to_string())], |_| {});
    let (status, head, body) = http(port, "POST", "/devices/sensor-7/readings", &[("Content-Type", "application/json")], r#"{"temp": 21.5}"#);
    assert_eq!(status, 202, "{}", body);
    assert_eq!(body, r#"{"device": "sensor-7", "accepted": true}"#);
    assert!(head.to_ascii_lowercase().contains("content-type: application/json"), "{}", head);
    let stored = main.context().mock_endpoint("mock:stored").unwrap();
    wait_for("the queued reading", || stored.received_exchanges().len() == 1);
    let ex = &stored.received_exchanges()[0];
    assert_eq!(ex.header("deviceId").map(|v| v.to_string()).as_deref(), Some("sensor-7"));
    assert_eq!(ex.body().to_string(), r#"{"temp": 21.5}"#);
    assert_eq!(http(port, "GET", "/devices/sensor-7/readings", &[], "").0, 405);
    assert_eq!(http(port, "POST", "/nope", &[], "").0, 404);
    main.stop().unwrap();

    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    let p2 = free_port();
    ctx.properties().set("camel.server.port", &p2.to_string());
    ctx.add_routes(device_readings_routes).unwrap();
    ctx.start().unwrap();
    assert_eq!(http(p2, "POST", "/devices/d1/readings", &[], "x").2, r#"{"device": "d1", "accepted": true}"#);
    ctx.stop().unwrap();
}

fn device_status_route(b: &mut RouteBuilder, port: u16) {
    b.from(&format!("jetty:http://0.0.0.0:{}/status?matchOnUriPrefix=true&httpMethodRestrict=GET", port))
        .route_id("device-status")
        .set_header("Content-Type", constant("application/json"))
        .set_body(simple(r#"{"device": "gateway-1", "path": "${header.CamelHttpPath}", "status": "up"}"#));
}

#[test]
fn jetty_status_endpoint() {
    let port = free_port();
    let ps = port.to_string();
    let yaml = subst(&example("jetty", "jetty-status-endpoint"), &[("STATUS_PORT", &ps), ("DEVICE_ID", "gateway-7")]);
    resolve_all(&yaml);
    let main = start_yaml("device-status", &yaml, &[], |_| {});
    let (status, _, body) = http(port, "GET", "/status/disk", &[], "");
    assert_eq!(status, 200, "{}", body);
    assert_eq!(body, r#"{"device": "gateway-7", "path": "/status/disk", "status": "up"}"#);
    assert_eq!(http(port, "POST", "/status", &[], "").0, 405);
    main.stop().unwrap();

    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    let p2 = free_port();
    ctx.add_routes(move |b| device_status_route(b, p2)).unwrap();
    ctx.start().unwrap();
    assert!(http(p2, "GET", "/status", &[], "").2.contains("\"up\""));
    ctx.stop().unwrap();
}

fn command_adapter_route(b: &mut RouteBuilder, port: u16) {
    b.from(&format!("undertow:http://0.0.0.0:{}/command?httpMethodRestrict=POST,PUT", port))
        .route_id("command-adapter")
        .set_body(simple("${header.CamelHttpMethod} ${body.toUpperCase()}"))
        .set_header("Content-Type", constant("text/plain"));
}

#[test]
fn undertow_command_adapter() {
    let port = free_port();
    let ps = port.to_string();
    let yaml = subst(&example("undertow", "undertow-command-adapter"), &[("ADAPTER_PORT", &ps)]);
    resolve_all(&yaml);
    let main = start_yaml("command-adapter", &yaml, &[], |_| {});
    let (status, _, body) = http(port, "POST", "/command", &[], "restart pump 3");
    assert_eq!((status, body.as_str()), (200, "POST RESTART PUMP 3"));
    assert_eq!(http(port, "PUT", "/command", &[], "stop").2, "PUT STOP");
    assert_eq!(http(port, "GET", "/command", &[], "").0, 405);
    main.stop().unwrap();

    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    let p2 = free_port();
    ctx.add_routes(move |b| command_adapter_route(b, p2)).unwrap();
    ctx.start().unwrap();
    assert_eq!(http(p2, "POST", "/command", &[], "go").2, "POST GO");
    ctx.stop().unwrap();
}

fn liveness_route(b: &mut RouteBuilder, port: u16) {
    b.from(&format!("netty-http:http://0.0.0.0:{}/ping", port))
        .route_id("liveness")
        .set_body(simple("pong ${header.probe}"));
}

#[test]
fn netty_http_liveness() {
    let port = free_port();
    let ps = port.to_string();
    let yaml = subst(&example("netty-http", "netty-http-liveness"), &[("PROBE_PORT", &ps)]);
    resolve_all(&yaml);
    let main = start_yaml("liveness", &yaml, &[], |_| {});
    let (status, _, body) = http(port, "GET", "/ping?probe=lb", &[], "");
    assert_eq!((status, body.as_str()), (200, "pong lb"));
    main.stop().unwrap();

    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    let p2 = free_port();
    ctx.add_routes(move |b| liveness_route(b, p2)).unwrap();
    ctx.start().unwrap();
    assert_eq!(http(p2, "GET", "/ping?probe=k8s", &[], "").2, "pong k8s");
    ctx.stop().unwrap();
}

// ---------------------------------------------------------------------------
// rest
// ---------------------------------------------------------------------------

use cameleer_http::{RestBindingMode, RestDslExt};

fn agent_tools_routes(b: &mut RouteBuilder) {
    b.rest_configuration().context_path("/tools").api_context_path("/openapi.json").binding_mode(RestBindingMode::Json);
    b.rest("/inventory")
        .get("/{sku}").route_id("getStock").description("Current stock level of a product").to("direct:getStock")
        .post("/{sku}/reserve").route_id("reserveStock").description("Reserve units of a product for an order").to("direct:reserveStock");
    b.from("direct:getStock").set_body(simple(r#"{"sku": "${header.sku}", "inStock": 12}"#)).unmarshal_json();
    b.from("direct:reserveStock").set_body(simple(r#"{"sku": "${header.sku}", "reserved": ${body[qty]}}"#)).unmarshal_json();
}

fn check_agent_tools(port: u16) {
    let (status, _, body) = http(port, "GET", "/tools/inventory/A-100", &[], "");
    assert_eq!(status, 200, "{}", body);
    assert!(body.contains("\"sku\"") && body.contains("A-100") && body.contains("12"), "{}", body);
    let (status, _, body) = http(port, "POST", "/tools/inventory/A-100/reserve", &[("Content-Type", "application/json")], r#"{"qty": 3}"#);
    assert_eq!(status, 200, "{}", body);
    assert!(body.contains("\"reserved\"") && body.contains('3'), "{}", body);
    let (status, _, doc) = http(port, "GET", "/tools/openapi.json", &[], "");
    assert_eq!(status, 200, "{}", doc);
    assert!(doc.contains("3.0.3") && doc.contains("getStock") && doc.contains("reserveStock") && doc.contains("Reserve units"), "{}", doc);
}

#[test]
fn rest_agent_tools() {
    let yaml = example("rest", "rest-agent-tools");
    resolve_all(&yaml);
    let port = free_port();
    let main = start_yaml("agent-tools", &yaml, &[("camel.server.port", port.to_string())], |_| {});
    check_agent_tools(port);
    main.stop().unwrap();

    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    let p2 = free_port();
    ctx.properties().set("camel.server.port", &p2.to_string());
    ctx.add_routes(agent_tools_routes).unwrap();
    ctx.start().unwrap();
    check_agent_tools(p2);
    ctx.stop().unwrap();
}

#[test]
fn rest_producer_get_order() {
    let api = cameleer_saas::testing::MockServer::start(|r| {
        if r.path == "/orders/42" && r.method == "GET" {
            cameleer_http::HttpResponse::text(200, r#"{"id": 42, "status": "shipped"}"#)
        } else {
            cameleer_http::HttpResponse::text(404, "Not Found")
        }
    });
    let url = api.url();
    let yaml = subst(&example("rest", "rest-producer-get-order"), &[("ORDERS_API", &url)]);
    resolve_all(&yaml);
    let yaml = yaml.replace("        - log: \"order 42: ${body}\"", "        - to: mock:order");
    let main = start_yaml("fetch-order", &yaml, &[], |_| {});
    let m = main.context().mock_endpoint("mock:order").unwrap();
    wait_for("the order", || m.received_exchanges().len() == 1);
    assert_eq!(m.received_exchanges()[0].body().to_string(), r#"{"id": 42, "status": "shipped"}"#);
    assert_eq!(api.requests_to("/orders/42").len(), 1);
    main.stop().unwrap();
}

// ---------------------------------------------------------------------------
// websocket, ws, vertx-websocket
// ---------------------------------------------------------------------------

use cameleer_protocols::websocket::{WebSocketEndpoint, WsConnection, WsMessage};

fn ws_connect(port: u16, path: &str) -> WsConnection {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match WsConnection::connect("127.0.0.1", port, path, Duration::from_secs(5)) {
            Ok(c) => return c,
            Err(e) => {
                assert!(Instant::now() < deadline, "ws connect {}: {:?}", port, e);
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    }
}

fn ops_hub_route(b: &mut RouteBuilder, port: u16) {
    b.from(&format!("websocket://0.0.0.0:{}/ops", port))
        .route_id("ops-hub")
        .set_body(simple("${header.CamelWebsocketRemoteAddress}: ${body}"))
        .to(&format!("websocket://0.0.0.0:{}/ops?sendToAll=true", port));
}

fn check_ops_hub(ctx: &CamelContext, port: u16) {
    let a = ws_connect(port, "/ops");
    let b = ws_connect(port, "/ops");
    let ep = ctx.get_endpoint(&format!("websocket://0.0.0.0:{}/ops?sendToAll=true", port)).unwrap();
    wait_for("both clients", || ep.as_any().downcast_ref::<WebSocketEndpoint>().unwrap().connection_keys().len() == 2);
    a.send_text("pump 3 restarted").unwrap();
    for c in [&a, &b] {
        match c.read_message().unwrap() {
            Some(WsMessage::Text(t)) => assert!(t.starts_with("127.0.0.1") && t.ends_with(": pump 3 restarted"), "{}", t),
            other => panic!("unexpected {:?}", other.map(|_| ())),
        }
    }
}

#[test]
fn websocket_broadcast_hub() {
    let port = free_port();
    let ps = port.to_string();
    let yaml = subst(&example("websocket", "websocket-broadcast-hub"), &[("WS_PORT", &ps)]);
    resolve_all(&yaml);
    let main = start_yaml("ops-hub", &yaml, &[], |_| {});
    check_ops_hub(main.context(), port);
    main.stop().unwrap();

    let ctx = CamelContext::new();
    cameleer_protocols::register(&ctx);
    let p2 = free_port();
    ctx.add_routes(move |b| ops_hub_route(b, p2)).unwrap();
    ctx.start().unwrap();
    check_ops_hub(&ctx, p2);
    ctx.stop().unwrap();
}

fn hub_commands_route(b: &mut RouteBuilder, hub: &str) {
    b.from(&format!("ws://{}/commands?reconnectInterval=5s", hub))
        .route_id("hub-commands")
        .filter(simple("${body} starts with 'site-1:'"))
        .log("command for this site: ${body}");
}

#[test]
fn ws_edge_commands() {
    // the hub: a websocket server that broadcasts whatever is sent to direct:push
    let port = free_port();
    let hub_uri = format!("websocket://127.0.0.1:{}/commands?sendToAll=true", port);
    let hub = CamelContext::new();
    cameleer_protocols::register(&hub);
    let h2 = hub_uri.clone();
    hub.add_routes(move |b| {
        b.from("direct:push").to(&h2);
    })
    .unwrap();
    hub.start().unwrap();

    let addr = format!("127.0.0.1:{}", port);
    let yaml = subst(&example("ws", "ws-edge-commands"), &[("HUB_ADDRESS", &addr), ("SITE_ID", "site-7")]);
    resolve_all(&yaml);
    let yaml = yaml.replace("              - log: \"command for this site: ${body}\"", "              - to: mock:commands");
    let main = start_yaml("hub-commands", &yaml, &[], |_| {});
    let ep = hub.get_endpoint(&hub_uri).unwrap();
    wait_for("the edge client to connect", || ep.as_any().downcast_ref::<WebSocketEndpoint>().unwrap().connection_keys().len() == 1);
    let t = hub.create_producer_template();
    t.send_body("direct:push", "site-2: ignore me").unwrap();
    t.send_body("direct:push", "site-7: reboot gateway").unwrap();
    let m = main.context().mock_endpoint("mock:commands").unwrap();
    wait_for("the command", || m.received_exchanges().len() == 1);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(m.received_exchanges().len(), 1);
    assert_eq!(m.received_exchanges()[0].body().to_string(), "site-7: reboot gateway");
    main.stop().unwrap();

    // the Rust DSL version compiles and connects
    let ctx = CamelContext::new();
    cameleer_protocols::register(&ctx);
    ctx.add_routes(move |b| hub_commands_route(b, &addr)).unwrap();
    ctx.start().unwrap();
    wait_for("the second client", || ep.as_any().downcast_ref::<WebSocketEndpoint>().unwrap().connection_keys().len() == 1);
    ctx.stop().unwrap();
    hub.stop().unwrap();
}

fn agent_presence_route(b: &mut RouteBuilder, port: u16) {
    let endpoint = format!("vertx-websocket:0.0.0.0:{}/agents", port);
    b.from(&format!("{}?fireWebSocketConnectionEvents=true", endpoint))
        .route_id("agent-presence")
        .choice()
        .when(simple("${header[CamelVertxWebsocket.event]} == 'MESSAGE'"))
        .set_body(simple("ack: ${body}"))
        .to(&endpoint)
        .otherwise()
        .log("${header[CamelVertxWebsocket.event]} ${header[CamelVertxWebsocket.remoteAddress]}")
        .end_choice();
}

#[test]
fn vertx_websocket_agent_presence() {
    let port = free_port();
    let ps = port.to_string();
    let yaml = subst(&example("vertx-websocket", "vertx-websocket-agent-presence"), &[("WS_PORT", &ps)]);
    resolve_all(&yaml);
    let yaml = yaml.replace(
        "                - log: \"${header[CamelVertxWebsocket.event]} ${header[CamelVertxWebsocket.remoteAddress]}\"",
        "                - to: mock:presence",
    );
    let main = start_yaml("agent-presence", &yaml, &[], |_| {});
    let c = ws_connect(port, "/agents");
    c.send_text("agent-1 ready").unwrap();
    assert_eq!(c.read_message().unwrap(), Some(WsMessage::Text("ack: agent-1 ready".into())));
    c.close(1000, "bye");
    let m = main.context().mock_endpoint("mock:presence").unwrap();
    wait_for("open and close events", || m.received_exchanges().len() == 2);
    let events: Vec<String> = m.received_exchanges().iter().map(|e| e.header("CamelVertxWebsocket.event").map(|v| v.to_string()).unwrap_or_default()).collect();
    assert_eq!(events, vec!["OPEN".to_string(), "CLOSE".to_string()]);
    main.stop().unwrap();

    let ctx = CamelContext::new();
    cameleer_protocols::register(&ctx);
    let p2 = free_port();
    ctx.add_routes(move |b| agent_presence_route(b, p2)).unwrap();
    ctx.start().unwrap();
    let c = ws_connect(p2, "/agents");
    c.send_text("x").unwrap();
    assert_eq!(c.read_message().unwrap(), Some(WsMessage::Text("ack: x".into())));
    ctx.stop().unwrap();
}

// ---------------------------------------------------------------------------
// webhook, graphql
// ---------------------------------------------------------------------------

use cameleer_saas::testing::{GithubMock, GraphqlMock};

fn pr_intake_routes(b: &mut RouteBuilder, github: &str, public_url: &str) {
    b.from(&format!("webhook:{}&webhookExternalUrl={}&webhookPath=/github", github, public_url))
        .route_id("pr-intake")
        .filter(simple("${header.GitHubEvent} == 'pull_request' && ${body[action]} == 'opened'"))
        .set_body(simple("Review pull request #${body[number]}"))
        .to("seda:review-queue");
    b.from("seda:review-queue").route_id("reviewer").log("${body}");
}

fn post_github_event(port: u16, event: &str, body: &str) -> u16 {
    http(port, "POST", "/github", &[("Content-Type", "application/json"), ("X-GitHub-Event", event)], body).0
}

#[test]
fn webhook_github_pull_requests() {
    let gh = GithubMock::start();
    let port = free_port();
    let public = format!("http://127.0.0.1:{}", port);
    let yaml = example("webhook", "webhook-github-pull-requests");
    let api = gh.url();
    let yaml = subst(&yaml, &[("GITHUB_OWNER", GithubMock::OWNER), ("GITHUB_REPO", GithubMock::REPO), ("GITHUB_TOKEN", GithubMock::TOKEN), ("GITHUB_API", &api), ("PUBLIC_URL", &public)]);
    resolve_all(&yaml);
    let yaml = yaml.replace("        - log: \"${body}\"", "        - to: mock:reviews");
    let main = start_yaml("pr-intake", &yaml, &[("camel.server.port", port.to_string())], |_| {});
    let hooks = gh.hooks();
    assert_eq!(hooks.len(), 1);
    assert_eq!(hooks[0].get("config").and_then(|c| c.get("url")).map(|u| u.to_string()), Some(format!("{}/github", public)));
    assert_eq!(post_github_event(port, "pull_request", r#"{"action":"closed","number":6}"#), 200);
    assert_eq!(post_github_event(port, "issues", r#"{"action":"opened","number":5}"#), 200);
    assert_eq!(post_github_event(port, "pull_request", r#"{"action":"opened","number":7}"#), 200);
    let m = main.context().mock_endpoint("mock:reviews").unwrap();
    wait_for("the review request", || m.received_exchanges().len() == 1);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(m.received_exchanges().len(), 1);
    assert_eq!(m.received_exchanges()[0].body().to_string(), "Review pull request #7");
    main.stop().unwrap();
    assert!(gh.hooks().is_empty(), "the hook is removed on stop");

    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    cameleer_saas::register(&ctx);
    let p2 = free_port();
    ctx.properties().set("camel.server.port", &p2.to_string());
    let github = format!("github:pullRequest?repoOwner={}&repoName={}&oauthToken={}&serverUrl={}", GithubMock::OWNER, GithubMock::REPO, GithubMock::TOKEN, gh.url());
    let public2 = format!("http://127.0.0.1:{}", p2);
    ctx.add_routes(move |b| pr_intake_routes(b, &github, &public2)).unwrap();
    ctx.start().unwrap();
    assert_eq!(gh.hooks().len(), 1);
    assert_eq!(post_github_event(p2, "pull_request", r#"{"action":"opened","number":8}"#), 200);
    ctx.stop().unwrap();
}

fn book_lookup_route(b: &mut RouteBuilder, url: &str, token: &str) {
    b.from("direct:book")
        .route_id("book-lookup")
        .set_header("gqlVariables", simple(r#"{"id": "${header.bookId}"}"#))
        .to(&format!("graphql://{}?query=query Book($id: ID) {{ book(id: $id) {{ id name author }} }}&variablesHeader=gqlVariables&accessToken={}", url, token))
        .unmarshal_json()
        .set_body(simple("${body[data][book][name]}"));
}

#[test]
fn graphql_book_lookup() {
    let gql = GraphqlMock::start(true);
    let url = gql.endpoint();
    let yaml = subst(&example("graphql", "graphql-book-lookup"), &[("GRAPHQL_URL", &url), ("GRAPHQL_TOKEN", GraphqlMock::TOKEN)]);
    resolve_all(&yaml);
    let main = start_yaml("book-lookup", &yaml, &[], |_| {});
    let t = main.context().create_producer_template();
    let name = t.request_body_and_header("direct:book", "", "bookId", "2").unwrap();
    assert_eq!(name.to_string(), "Enterprise Integration Patterns");
    let req = gql.server.requests_to("/graphql")[0].json();
    assert_eq!(req.get("variables").and_then(|v| v.get("id")).map(|v| v.to_string()).as_deref(), Some("2"));
    main.stop().unwrap();

    let ctx = CamelContext::new();
    cameleer_saas::register(&ctx);
    ctx.add_routes(move |b| book_lookup_route(b, &url, GraphqlMock::TOKEN)).unwrap();
    ctx.start().unwrap();
    let name = ctx.create_producer_template().request_body_and_header("direct:book", "", "bookId", "1").unwrap();
    assert_eq!(name.to_string(), "Camel in Action");
    ctx.stop().unwrap();
}

// ---------------------------------------------------------------------------
// grpc, thrift
// ---------------------------------------------------------------------------

/// The first fenced block of the given language in a page (e.g. a `.proto` file).
fn fenced(page: &str, lang: &str) -> String {
    let path = format!("{}/../../docs/components/{}.md", env!("CARGO_MANIFEST_DIR"), page);
    let text = std::fs::read_to_string(&path).unwrap().replace("\r\n", "\n");
    let open = format!("```{}\n", lang);
    let at = text.find(&open).unwrap_or_else(|| panic!("{}: no {} block", page, lang)) + open.len();
    let end = text[at..].find("\n```").unwrap();
    text[at..at + end + 1].to_string()
}

fn inventory_service_route(b: &mut RouteBuilder, port: u16, proto: &str) {
    b.from(&format!("grpc://0.0.0.0:{}/inventory.Inventory?protoFile={}", port, proto))
        .route_id("inventory-service")
        .log("${header.CamelGrpcMethodName} for ${body[sku]}")
        .set_body(simple(r#"{"sku": "${body[sku]}", "quantity": 12}"#))
        .unmarshal_json();
}

#[test]
fn grpc_inventory_service_and_client() {
    let dir = temp_dir("grpc");
    let proto = dir.join("inventory.proto");
    std::fs::write(&proto, fenced("grpc", "proto")).unwrap();
    let proto = slash(&proto);
    let port = free_port();
    let ps = port.to_string();
    let service = subst(&example("grpc", "grpc-inventory-service"), &[("GRPC_PORT", &ps), ("PROTO_FILE", &proto)]);
    resolve_all(&service);
    let addr = format!("127.0.0.1:{}", port);
    let client = subst(&example("grpc", "grpc-call-inventory"), &[("INVENTORY_ADDRESS", &addr), ("PROTO_FILE", &proto)]);
    resolve_all(&client);
    let client = client.replace("        - log: \"A-100: ${body[quantity]} in stock\"", "        - to: mock:stock");
    let server = start_yaml("inventory-service", &service, &[], |_| {});
    let main = start_yaml("check-stock", &client, &[], |_| {});
    let m = main.context().mock_endpoint("mock:stock").unwrap();
    wait_for("the stock reply", || m.received_exchanges().len() == 1);
    let reply = m.received_exchanges()[0].body().clone();
    assert_eq!(reply.get("sku").map(|v| v.to_string()).as_deref(), Some("A-100"));
    assert_eq!(reply.get("quantity").and_then(|v| v.as_i64()), Some(12));
    main.stop().unwrap();
    server.stop().unwrap();

    let ctx = CamelContext::new();
    cameleer_grpc::register(&ctx);
    let p2 = free_port();
    let pr = proto.clone();
    ctx.add_routes(move |b| inventory_service_route(b, p2, &pr)).unwrap();
    ctx.start().unwrap();
    let uri = format!("grpc://127.0.0.1:{}/inventory.Inventory?method=GetStock&protoFile={}", p2, proto);
    let r = ctx.create_producer_template().request_body(&uri, r#"{"sku": "B-7"}"#).unwrap();
    assert_eq!(r.get("quantity").and_then(|v| v.as_i64()), Some(12));
    ctx.stop().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

fn device_registry_route(b: &mut RouteBuilder, port: u16, idl: &str) {
    b.from(&format!("thrift://0.0.0.0:{}/registry.DeviceRegistry?idlFile={}", port, idl))
        .route_id("device-registry")
        .set_body(simple("site-north/rack-${body[0]}"));
}

#[test]
fn thrift_device_registry() {
    let dir = temp_dir("thrift");
    let idl = dir.join("registry.thrift");
    std::fs::write(&idl, fenced("thrift", "thrift")).unwrap();
    let idl = slash(&idl);
    let port = free_port();
    let ps = port.to_string();
    let yaml = subst(&example("thrift", "thrift-device-registry"), &[("THRIFT_PORT", &ps), ("IDL_FILE", &idl)]);
    resolve_all(&yaml);
    let main = start_yaml("device-registry", &yaml, &[], |_| {});
    let client = format!("thrift://127.0.0.1:{}/registry.DeviceRegistry?method=location&idlFile={}", port, idl);
    let t = main.context().create_producer_template();
    let deadline = Instant::now() + Duration::from_secs(10);
    let r = loop {
        match t.request_body(&client, "7") {
            Ok(v) => break v,
            Err(e) => {
                assert!(Instant::now() < deadline, "{}", e);
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    assert_eq!(r.to_string(), "site-north/rack-7");
    main.stop().unwrap();

    let ctx = CamelContext::new();
    cameleer_dataformats::register(&ctx);
    let p2 = free_port();
    let i2 = idl.clone();
    ctx.add_routes(move |b| device_registry_route(b, p2, &i2)).unwrap();
    ctx.start().unwrap();
    let client = format!("thrift://127.0.0.1:{}/registry.DeviceRegistry?method=location&idlFile={}", p2, idl);
    assert_eq!(ctx.create_producer_template().request_body(&client, "9").unwrap().to_string(), "site-north/rack-9");
    ctx.stop().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// cxf, cxfrs, spring-ws, fhir
// ---------------------------------------------------------------------------

/// A SOAP calculator service (the cameleer-soap test WSDL) on a free port.
fn soap_calculator() -> (CamelContext, u16) {
    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    cameleer_soap::register(&ctx);
    ctx.registry().bind("calcWsdl", cameleer_soap::testing::CALCULATOR_WSDL.to_string());
    let uri = format!(
        "cxf://http://127.0.0.1:0/calculator?wsdlURL=#calcWsdl&serviceName={{{0}}}CalculatorService&portName={{{0}}}CalculatorSoap",
        cameleer_soap::testing::CALC_NS
    );
    let u = uri.clone();
    ctx.add_routes(move |b| {
        b.from(&u).process(|ex: &mut Exchange| {
            let args = ex.body().as_array().cloned().unwrap_or_default();
            ex.set_body(Value::Int(args.iter().filter_map(|v| v.as_i64()).sum::<i64>()));
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let port = cameleer_soap::local_port(&ctx, &uri).unwrap();
    (ctx, port)
}

fn calc_bridge_route(b: &mut RouteBuilder, address: &str, wsdl: &str) {
    b.from("platform-http:/calc/add?httpMethodRestrict=POST")
        .route_id("calc-json-bridge")
        .unmarshal_json()
        .remove_headers("*")
        .to(&format!(
            "cxf://http://{}/calculator?wsdlURL={}&serviceName={{http://example.org/calculator}}CalculatorService&portName={{http://example.org/calculator}}CalculatorSoap&defaultOperationName=Add",
            address, wsdl
        ))
        .set_header("Content-Type", constant("application/json"))
        .set_body(simple(r#"{"sum": ${body}}"#));
}

#[test]
fn cxf_json_to_soap() {
    let (soap, soap_port) = soap_calculator();
    let dir = temp_dir("cxf");
    let wsdl = dir.join("calculator.wsdl");
    std::fs::write(&wsdl, cameleer_soap::testing::CALCULATOR_WSDL).unwrap();
    let wsdl = slash(&wsdl);
    let addr = format!("127.0.0.1:{}", soap_port);
    let yaml = subst(&example("cxf", "cxf-json-to-soap"), &[("CALC_ADDRESS", &addr), ("CALC_WSDL", &wsdl)]);
    resolve_all(&yaml);
    let port = free_port();
    let main = start_yaml("calc-json-bridge", &yaml, &[("camel.server.port", port.to_string())], |_| {});
    let (status, _, body) = http(port, "POST", "/calc/add", &[("Content-Type", "application/json")], r#"{"a": 2, "b": 3}"#);
    assert_eq!((status, body.as_str()), (200, r#"{"sum": 5}"#));
    main.stop().unwrap();

    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    cameleer_soap::register(&ctx);
    let p2 = free_port();
    ctx.properties().set("camel.server.port", &p2.to_string());
    ctx.add_routes(move |b| calc_bridge_route(b, &addr, &wsdl)).unwrap();
    ctx.start().unwrap();
    assert_eq!(http(p2, "POST", "/calc/add", &[("Content-Type", "application/json")], r#"{"a": 40, "b": 2}"#).2, r#"{"sum": 42}"#);
    ctx.stop().unwrap();
    soap.stop().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

fn customer_resource_route(b: &mut RouteBuilder, port: u16) {
    b.from(&format!("cxfrs:http://0.0.0.0:{}/api?operations=getCustomer:GET:/customers/{{id}};addCustomer:POST:/customers", port))
        .route_id("customer-resource")
        .choice()
        .when(simple("${header.operationName} == 'getCustomer'"))
        .set_body(simple(r#"{"id": "${header.id}", "name": "Ada"}"#))
        .unmarshal_json()
        .when(simple("${header.operationName} == 'addCustomer'"))
        .set_header("CamelHttpResponseCode", constant(201))
        .set_body(simple(r#"{"created": "${body[name]}"}"#))
        .unmarshal_json()
        .end_choice();
}

fn check_customers(port: u16) {
    let (status, _, body) = http(port, "GET", "/api/customers/7", &[], "");
    assert_eq!(status, 200, "{}", body);
    assert!(body.contains("\"id\"") && body.contains("\"7\"") && body.contains("Ada"), "{}", body);
    let (status, _, body) = http(port, "POST", "/api/customers", &[("Content-Type", "application/json")], r#"{"name": "Grace"}"#);
    assert_eq!(status, 201, "{}", body);
    assert!(body.contains("\"created\"") && body.contains("Grace"), "{}", body);
    assert_eq!(http(port, "DELETE", "/api/customers/7", &[], "").0, 404);
}

#[test]
fn cxfrs_customer_resource() {
    let port = free_port();
    let ps = port.to_string();
    let yaml = subst(&example("cxfrs", "cxfrs-customer-resource"), &[("API_PORT", &ps)]);
    resolve_all(&yaml);
    let main = start_yaml("customer-resource", &yaml, &[], |_| {});
    check_customers(port);
    main.stop().unwrap();

    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    cameleer_soap::register(&ctx);
    let p2 = free_port();
    ctx.add_routes(move |b| customer_resource_route(b, p2)).unwrap();
    ctx.start().unwrap();
    check_customers(p2);
    ctx.stop().unwrap();
}

fn order_status_route(b: &mut RouteBuilder, port: u16) {
    b.from(&format!("spring-ws:rootqname:{{urn:orders}}GetOrderStatus?address=http://0.0.0.0:{}/ws", port))
        .route_id("order-status")
        .set_header("orderId", xpath("//*[local-name()='id']/text()"))
        .set_body(simple(r#"<GetOrderStatusResponse xmlns="urn:orders"><id>${header.orderId}</id><status>shipped</status></GetOrderStatusResponse>"#));
}

fn check_order_status(ctx: &CamelContext, port: u16) {
    let r = ctx
        .create_producer_template()
        .request_body(&format!("spring-ws:http://127.0.0.1:{}/ws", port), r#"<GetOrderStatus xmlns="urn:orders"><id>42</id></GetOrderStatus>"#)
        .unwrap()
        .to_string();
    assert!(r.contains("<status>shipped</status>") && r.contains("<id>42</id>") && r.contains("GetOrderStatusResponse"), "{}", r);
}

#[test]
fn spring_ws_order_status() {
    let port = free_port();
    let ps = port.to_string();
    let yaml = subst(&example("spring-ws", "spring-ws-order-status"), &[("SOAP_PORT", &ps)]);
    resolve_all(&yaml);
    let main = start_yaml("order-status", &yaml, &[], |_| {});
    check_order_status(main.context(), port);
    main.stop().unwrap();

    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    cameleer_xml::register(&ctx);
    cameleer_soap::register(&ctx);
    let p2 = free_port();
    ctx.add_routes(move |b| order_status_route(b, p2)).unwrap();
    ctx.start().unwrap();
    check_order_status(&ctx, p2);
    ctx.stop().unwrap();
}

fn bp_to_fhir_route(b: &mut RouteBuilder, server: &str) {
    b.from("platform-http:/readings/blood-pressure?httpMethodRestrict=POST")
        .route_id("bp-to-fhir")
        .unmarshal_json()
        .remove_headers("*")
        .set_body(simple(r#"{"resourceType": "Observation", "status": "final", "code": {"text": "Systolic blood pressure"}, "subject": {"reference": "Patient/${body[patient]}"}, "valueQuantity": {"value": ${body[systolic]}, "unit": "mmHg"}}"#))
        .to(&format!("fhir://create/resource?serverUrl={}&fhirVersion=R4", server))
        .set_body(simple(r#"{"observation": "${body[id]}"}"#))
        .remove_headers("*")
        .set_header("CamelHttpResponseCode", constant(201))
        .set_header("Content-Type", constant("application/json"));
}

#[test]
fn fhir_device_observation() {
    let fhir = cameleer_soap::testing::FhirMock::start();
    let url = fhir.url();
    let yaml = subst(&example("fhir", "fhir-device-observation"), &[("FHIR_SERVER", &url)]);
    resolve_all(&yaml);
    let port = free_port();
    let main = start_yaml("bp-to-fhir", &yaml, &[("camel.server.port", port.to_string())], |_| {});
    let (status, _, body) = http(port, "POST", "/readings/blood-pressure", &[("Content-Type", "application/json")], r#"{"patient": "123", "systolic": 128}"#);
    assert_eq!(status, 201, "{}", body);
    assert!(body.starts_with(r#"{"observation": ""#) && body.len() > r#"{"observation": ""}"#.len(), "{}", body);
    assert_eq!(fhir.count("Observation"), 1);
    let id = body.trim_start_matches(r#"{"observation": ""#).trim_end_matches(r#""}"#).to_string();
    let stored = fhir.resource_json("Observation", &id).expect("stored observation");
    assert!(stored.contains("Patient/123") && stored.contains("128") && stored.contains("mmHg"), "{}", stored);
    main.stop().unwrap();

    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    cameleer_soap::register(&ctx);
    let p2 = free_port();
    ctx.properties().set("camel.server.port", &p2.to_string());
    ctx.add_routes(move |b| bp_to_fhir_route(b, &url)).unwrap();
    ctx.start().unwrap();
    assert_eq!(http(p2, "POST", "/readings/blood-pressure", &[("Content-Type", "application/json")], r#"{"patient": "9", "systolic": 110}"#).0, 201);
    assert_eq!(fhir.count("Observation"), 2);
    ctx.stop().unwrap();
}

// ---------------------------------------------------------------------------
// file, ftp, ftps, sftp, ssh
// ---------------------------------------------------------------------------

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(f) = find_file(&p, name) {
                return Some(f);
            }
        } else if p.file_name().map(|n| n == name).unwrap_or(false) {
            return Some(p);
        }
    }
    None
}

fn drop_folder_route(b: &mut RouteBuilder, inbox: &str, outbox: &str) {
    b.from(&format!("file:{}?include=.*\\.csv&readLock=changed&move=.done/${{date:now:yyyyMMdd}}/${{file:name}}&moveFailed=.failed", inbox))
        .route_id("drop-folder")
        .log("picked up ${file:name} (${file:size} bytes)")
        .to(&format!("file:{}?fileName=${{file:name.noext}}-processed.csv&tempPrefix=.part-", outbox));
}

fn check_drop_folder(inbox: &Path, outbox: &Path) {
    std::fs::write(inbox.join("sales.csv"), "region,total\neu,42\n").unwrap();
    std::fs::write(inbox.join("notes.txt"), "not a csv").unwrap();
    wait_for("the processed file", || outbox.join("sales-processed.csv").exists() && find_file(&inbox.join(".done"), "sales.csv").is_some());
    assert_eq!(std::fs::read_to_string(outbox.join("sales-processed.csv")).unwrap(), "region,total\neu,42\n");
    let archived = find_file(&inbox.join(".done"), "sales.csv").unwrap();
    let day = archived.parent().unwrap().file_name().unwrap().to_string_lossy().to_string();
    assert!(day.len() == 8 && day.chars().all(|c| c.is_ascii_digit()), "dated folder {}", day);
    assert!(inbox.join("notes.txt").exists(), "only .csv files are picked up");
}

#[test]
fn file_drop_folder() {
    let dir = temp_dir("file");
    let (inbox, outbox) = (dir.join("inbox"), dir.join("outbox"));
    std::fs::create_dir_all(&inbox).unwrap();
    let (i, o) = (slash(&inbox), slash(&outbox));
    let yaml = subst(&example("file", "file-drop-folder"), &[("INBOX", &i), ("OUTBOX", &o)]);
    resolve_all(&yaml);
    let main = start_yaml("drop-folder", &yaml, &[], |_| {});
    check_drop_folder(&inbox, &outbox);
    main.stop().unwrap();

    let dir2 = temp_dir("file-dsl");
    let (inbox2, outbox2) = (dir2.join("inbox"), dir2.join("outbox"));
    std::fs::create_dir_all(&inbox2).unwrap();
    let (i2, o2) = (slash(&inbox2), slash(&outbox2));
    let ctx = CamelContext::new();
    ctx.add_routes(move |b| drop_folder_route(b, &i2, &o2)).unwrap();
    ctx.start().unwrap();
    check_drop_folder(&inbox2, &outbox2);
    ctx.stop().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}

use cameleer_protocols::testing::FtpServer;

fn collect_orders_route(b: &mut RouteBuilder, ftp: &str, password: &str, local_dir: &str) {
    b.from(&format!("ftp://{}/outgoing?password={}&passiveMode=true&include=.*\\.xml&move=.archive&delay=10000", ftp, password))
        .route_id("collect-orders")
        .log("received ${file:name} from ${header.CamelFileHost}")
        .to(&format!("file:{}", local_dir));
}

fn check_collect_orders(root: &Path, local: &Path) {
    std::fs::create_dir_all(root.join("outgoing")).unwrap();
    std::fs::write(root.join("outgoing").join("order-1.xml"), "<order id=\"1\"/>").unwrap();
    std::fs::write(root.join("outgoing").join("readme.txt"), "ignored").unwrap();
    wait_for("the downloaded order", || local.join("order-1.xml").exists() && root.join("outgoing").join(".archive").join("order-1.xml").exists());
    assert_eq!(std::fs::read_to_string(local.join("order-1.xml")).unwrap(), "<order id=\"1\"/>");
    assert!(!local.join("readme.txt").exists());
}

#[test]
fn ftp_collect_orders() {
    let dir = temp_dir("ftp");
    let (root, local) = (dir.join("server"), dir.join("received"));
    std::fs::create_dir_all(&root).unwrap();
    let server = FtpServer::start_with_users(&root, &[("camel", "secret")]).unwrap();
    let host = format!("127.0.0.1:{}", server.port());
    let l = slash(&local);
    let yaml = subst(&example("ftp", "ftp-collect-orders"), &[("FTP_USER", "camel"), ("FTP_HOST", &host), ("FTP_PASSWORD", "secret"), ("LOCAL_DIR", &l)]);
    resolve_all(&yaml);
    let main = start_yaml("collect-orders", &yaml, &[], |_| {});
    check_collect_orders(&root, &local);
    main.stop().unwrap();

    let dir2 = temp_dir("ftp-dsl");
    let local2 = dir2.join("received");
    let l2 = slash(&local2);
    let ftp = format!("camel@{}", host);
    let ctx = CamelContext::new();
    cameleer_protocols::register(&ctx);
    ctx.add_routes(move |b| collect_orders_route(b, &ftp, "secret", &l2)).unwrap();
    ctx.start().unwrap();
    std::fs::remove_dir_all(root.join("outgoing")).unwrap();
    check_collect_orders(&root, &local2);
    ctx.stop().unwrap();
    server.stop();
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}

fn upload_reports_route(b: &mut RouteBuilder, outbox: &str, ftps: &str, password: &str) {
    b.from(&format!("file:{}?include=.*\\.csv&move=.sent", outbox))
        .route_id("upload-reports")
        .to(&format!("ftps://{}/reports/site-1?password={}&passiveMode=true&binary=true&tempPrefix=.part-", ftps, password))
        .log("uploaded ${file:name}");
}

/// The `ftps:` endpoint is resolved as written; the route then runs end to end over plain FTP
/// against the embedded server (this test crate has no TLS test certificates; FTPS itself is
/// covered by cameleer-protocols' tests/tls.rs).
#[test]
fn ftps_upload_reports() {
    let dir = temp_dir("ftps");
    let (root, outbox) = (dir.join("server"), dir.join("outbox"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outbox).unwrap();
    let server = FtpServer::start_with_users(&root, &[("edge", "secret")]).unwrap();
    let host = format!("127.0.0.1:{}", server.port());
    let o = slash(&outbox);
    let yaml = subst(&example("ftps", "ftps-upload-reports"), &[("OUTBOX", &o), ("FTPS_USER", "edge"), ("FTPS_HOST", &host), ("SITE_ID", "site-7"), ("FTPS_PASSWORD", "secret")]);
    resolve_all(&yaml);
    let plain = yaml.replace("- to: \"ftps://", "- to: \"ftp://");
    assert_ne!(plain, yaml);
    let main = start_yaml("upload-reports", &plain, &[], |_| {});
    std::fs::write(outbox.join("daily.csv"), "a,b\n1,2\n").unwrap();
    let target = root.join("reports").join("site-7").join("daily.csv");
    wait_for("the uploaded report", || target.exists() && outbox.join(".sent").join("daily.csv").exists());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "a,b\n1,2\n");
    main.stop().unwrap();

    // the Rust DSL version builds and resolves its ftps endpoint
    let ctx = cameleer_main::new_main().context().clone();
    ctx.add_routes(move |b| upload_reports_route(b, &o, &format!("edge@{}", host), "secret")).unwrap();
    server.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

use cameleer_ssh::testing::{SshServer, SshServerConfig};

fn site_pickup_route(b: &mut RouteBuilder, sftp: &str, password: &str, known_hosts: &str, local_dir: &str) {
    b.from(&format!(
        "sftp://{}/outbox?password={}&knownHostsFile={}&strictHostKeyChecking=yes&include=.*\\.json&delete=true&delay=30000",
        sftp, password, known_hosts
    ))
    .route_id("site-pickup")
    .log("picked up ${file:name} from ${header.CamelFileHost}")
    .to(&format!("file:{}", local_dir));
}

#[test]
fn sftp_site_pickup() {
    let dir = temp_dir("sftp");
    let (root, local) = (dir.join("site"), dir.join("incoming"));
    std::fs::create_dir_all(root.join("outbox")).unwrap();
    let server = SshServer::start(SshServerConfig::default().user("edge", "secret").root(&root)).unwrap();
    let kh = dir.join("known_hosts");
    std::fs::write(&kh, server.known_hosts_text("127.0.0.1")).unwrap();
    let (khs, l) = (slash(&kh), slash(&local));
    let host = format!("127.0.0.1:{}", server.port());
    let yaml = subst(&example("sftp", "sftp-site-pickup"), &[("SFTP_USER", "edge"), ("SFTP_HOST", &host), ("SFTP_PASSWORD", "secret"), ("KNOWN_HOSTS", &khs), ("LOCAL_DIR", &l)]);
    resolve_all(&yaml);
    std::fs::write(root.join("outbox").join("sales-0042.json"), r#"{"store": 42}"#).unwrap();
    std::fs::write(root.join("outbox").join("tmp.part"), "partial").unwrap();
    let main = start_yaml("site-pickup", &yaml, &[], |_| {});
    wait_for("the picked up file", || local.join("sales-0042.json").exists() && !root.join("outbox").join("sales-0042.json").exists());
    assert_eq!(std::fs::read_to_string(local.join("sales-0042.json")).unwrap(), r#"{"store": 42}"#);
    assert!(root.join("outbox").join("tmp.part").exists());
    main.stop().unwrap();

    // a wrong host key is refused with strictHostKeyChecking=yes
    let other = SshServer::start(SshServerConfig::default().user("edge", "secret")).unwrap();
    std::fs::write(&kh, other.known_hosts_text("127.0.0.1").replace(&other.port().to_string(), &server.port().to_string())).unwrap();
    let main = cameleer_main::new_main();
    let t = main.context().create_producer_template();
    let up = format!("sftp://edge@{}/outbox?password=secret&knownHostsFile={}&strictHostKeyChecking=yes&maximumReconnectAttempts=0", host, khs);
    assert!(t.send_body_and_header(&up, "x", "CamelFileName", "x.json").is_err(), "changed host key must be rejected");
    other.stop();

    std::fs::write(&kh, server.known_hosts_text("127.0.0.1")).unwrap();
    let local2 = dir.join("incoming2");
    let l2 = slash(&local2);
    let ctx = CamelContext::new();
    cameleer_ssh::register(&ctx);
    let sftp = format!("edge@{}", host);
    ctx.add_routes(move |b| site_pickup_route(b, &sftp, "secret", &khs, &l2)).unwrap();
    ctx.start().unwrap();
    std::fs::write(root.join("outbox").join("b.json"), "{}").unwrap();
    wait_for("the second file", || local2.join("b.json").exists());
    ctx.stop().unwrap();
    server.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

fn device_uptime_route(b: &mut RouteBuilder, host: &str, password: &str, known_hosts: &str) {
    b.from(&format!("ssh://monitor@{}?password={}&knownHostsFile={}&failOnUnknownHost=true&pollCommand=uptime&delay=60000", host, password, known_hosts))
        .route_id("device-uptime")
        .filter(simple("${header.CamelSshExitValue} == 0"))
        .log("uptime: ${body}");
}

#[test]
fn ssh_device_uptime() {
    let dir = temp_dir("ssh");
    let mut cfg = SshServerConfig::default().user("monitor", "secret");
    cfg.command_handler = Some(std::sync::Arc::new(|line: &str, _stdin: &[u8]| {
        if line == "uptime" {
            (b" 10:14:03 up 3 days,  2:01,  0 users,  load average: 0.08, 0.03, 0.01\n".to_vec(), Vec::new(), 0)
        } else {
            (Vec::new(), format!("{}: not found\n", line).into_bytes(), 127)
        }
    }));
    let server = SshServer::start(cfg).unwrap();
    let kh = dir.join("known_hosts");
    std::fs::write(&kh, server.known_hosts_text("127.0.0.1")).unwrap();
    let khs = slash(&kh);
    let host = format!("127.0.0.1:{}", server.port());
    let yaml = subst(&example("ssh", "ssh-device-uptime"), &[("SSH_USER", "monitor"), ("DEVICE_HOST", &host), ("SSH_PASSWORD", "secret"), ("KNOWN_HOSTS", &khs)]);
    resolve_all(&yaml);
    let yaml = yaml.replace("              - log: \"uptime: ${body}\"", "              - to: mock:uptime");
    let main = start_yaml("device-uptime", &yaml, &[], |_| {});
    let m = main.context().mock_endpoint("mock:uptime").unwrap();
    wait_for("the uptime poll", || !m.received_exchanges().is_empty());
    assert!(m.received_exchanges()[0].body().to_string().contains("up 3 days"));
    assert_eq!(server.commands().first().map(|s| s.as_str()), Some("uptime"));
    main.stop().unwrap();

    // on demand: the body is the command
    let main = cameleer_main::new_main();
    let t = main.context().create_producer_template();
    let ex = t.send_with(&format!("ssh://monitor@{}?password=secret&knownHostsFile={}&failOnUnknownHost=true", host, khs), ExchangePattern::InOut, |ex| {
        ex.set_body("reboot-now");
    });
    assert_eq!(ex.header("CamelSshExitValue").and_then(|v| v.as_i64()), Some(127));

    let ctx = CamelContext::new();
    cameleer_ssh::register(&ctx);
    ctx.add_routes(move |b| device_uptime_route(b, &host, "secret", &khs)).unwrap();
    ctx.start().unwrap();
    wait_for("the second poll", || server.commands().len() >= 3);
    ctx.stop().unwrap();
    server.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// smtp, imap, pop3
// ---------------------------------------------------------------------------

use cameleer_protocols::testing::{ImapServer, Mailbox, Pop3Server, SmtpServer};

fn mail_reports_route(b: &mut RouteBuilder, reports_dir: &str, smtp: &str) {
    b.from(&format!("file:{}?include=.*\\.txt&move=.sent", reports_dir))
        .route_id("mail-reports")
        .set_header("Subject", simple("Daily report ${file:name.noext}"))
        .convert_body_to("String")
        .to(smtp);
}

#[test]
fn smtp_mail_reports() {
    let mailbox = Mailbox::new();
    let smtp = SmtpServer::start(&mailbox).unwrap();
    let dir = temp_dir("smtp");
    let reports = dir.join("reports");
    std::fs::create_dir_all(&reports).unwrap();
    let r = slash(&reports);
    let host = format!("127.0.0.1:{}", smtp.port());
    let yaml = subst(&example("smtp", "smtp-mail-reports"), &[("REPORTS_DIR", &r), ("SMTP_HOST", &host), ("SMTP_PASSWORD", "pw")]);
    resolve_all(&yaml);
    let main = start_yaml("mail-reports", &yaml, &[], |_| {});
    std::fs::write(reports.join("site-7.txt"), "All pumps running.\n").unwrap();
    wait_for("the mail", || mailbox.count("ops@example.com") == 1);
    let mail = &mailbox.messages("ops@example.com")[0];
    assert!(mail.raw.contains("Subject: Daily report site-7"), "{}", mail.raw);
    assert!(mail.raw.contains("All pumps running."), "{}", mail.raw);
    assert_eq!(mail.envelope_from, "reports@example.com");
    assert!(mailbox.logins().contains(&("reports".to_string(), "pw".to_string())));
    main.stop().unwrap();

    let ctx = CamelContext::new();
    cameleer_protocols::register(&ctx);
    let smtp_uri = format!("smtp://reports@{}?password=pw&from=reports@example.com&to=ops@example.com", host);
    ctx.add_routes(move |b| mail_reports_route(b, &r, &smtp_uri)).unwrap();
    ctx.start().unwrap();
    std::fs::write(reports.join("site-8.txt"), "ok").unwrap();
    wait_for("the second mail", || mailbox.count("ops@example.com") == 2);
    ctx.stop().unwrap();
    smtp.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

fn raw_mail(subject: &str, body: &str) -> String {
    format!("From: customer@example.org\r\nTo: support@example.com\r\nSubject: {}\r\nMessage-ID: <{}@example.org>\r\n\r\n{}\r\n", subject, subject.replace(' ', "."), body)
}

fn mail_triage_routes(b: &mut RouteBuilder, imap: &str, invoice_dir: &str) {
    b.from(imap)
        .route_id("mail-triage")
        .choice()
        .when(simple("${header.Subject} contains 'invoice'"))
        .to(&format!("file:{}?fileName=${{exchangeId}}.txt", invoice_dir))
        .otherwise()
        .to("seda:triage")
        .end_choice();
    b.from("seda:triage").route_id("triage-agent").log("to triage: ${header.Subject} from ${header.From}");
}

fn dir_files(d: &Path) -> Vec<String> {
    std::fs::read_dir(d).map(|r| r.flatten().filter(|e| e.path().is_file()).map(|e| std::fs::read_to_string(e.path()).unwrap_or_default()).collect()).unwrap_or_default()
}

/// The `imaps:` endpoint is resolved as written; the route runs end to end over plain IMAP
/// against the embedded server (TLS for imaps is covered by cameleer-protocols' tests/tls.rs).
#[test]
fn imap_mail_triage() {
    let mailbox = Mailbox::new();
    mailbox.set_password("support@example.com", "secret");
    let imap = ImapServer::start(&mailbox).unwrap();
    let dir = temp_dir("imap");
    let invoices = dir.join("invoices");
    let inv = slash(&invoices);
    let host = format!("127.0.0.1:{}", imap.port());
    let yaml = subst(&example("imap", "imap-mail-triage"), &[("IMAP_HOST", &host), ("IMAP_USER", "support@example.com"), ("IMAP_PASSWORD", "secret"), ("INVOICE_DIR", &inv)]);
    resolve_all(&yaml);
    let plain = yaml.replace("uri: \"imaps://", "uri: \"imap://").replace("&delay=30000", "&delay=30000&initialDelay=0");
    let plain = plain.replace("        - log: \"to triage: ${header.Subject} from ${header.From}\"", "        - to: mock:triage");
    mailbox.deliver("support@example.com", &raw_mail("Your invoice 2026-11", "Amount due: 42 EUR"));
    mailbox.deliver("support@example.com", &raw_mail("Login problem", "I cannot log in."));
    let main = start_yaml("mail-triage", &plain, &[], |_| {});
    let m = main.context().mock_endpoint("mock:triage").unwrap();
    wait_for("both mails", || m.received_exchanges().len() == 1 && dir_files(&invoices).len() == 1);
    assert_eq!(m.received_exchanges()[0].header("Subject").map(|v| v.to_string()).as_deref(), Some("Login problem"));
    assert!(dir_files(&invoices)[0].contains("Amount due: 42 EUR"));
    main.stop().unwrap();

    let ctx = CamelContext::new();
    cameleer_protocols::register(&ctx);
    let inv2 = slash(&dir.join("invoices2"));
    let uri = format!("imap://{}?username=support@example.com&password=secret&moveTo=Processed&initialDelay=0&delay=50", host);
    ctx.add_routes(move |b| mail_triage_routes(b, &uri, &inv2)).unwrap();
    ctx.start().unwrap();
    mailbox.deliver("support@example.com", &raw_mail("invoice 7", "x"));
    wait_for("the invoice", || dir_files(&dir.join("invoices2")).len() == 1);
    ctx.stop().unwrap();
    imap.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

fn archive_mail_route(b: &mut RouteBuilder, pop3: &str, archive_dir: &str) {
    b.from(pop3)
        .route_id("archive-mail")
        .to(&format!("file:{}?fileName=${{date:now:yyyyMMdd}}/${{exchangeId}}.eml", archive_dir));
}

/// The `pop3s:` endpoint is resolved as written; the route runs end to end over plain POP3.
#[test]
fn pop3_archive_mail() {
    let mailbox = Mailbox::new();
    mailbox.set_password("archive@example.com", "secret");
    let pop3 = Pop3Server::start(&mailbox).unwrap();
    let dir = temp_dir("pop3");
    let archive = dir.join("archive");
    let a = slash(&archive);
    let host = format!("127.0.0.1:{}", pop3.port());
    let yaml = subst(&example("pop3", "pop3-archive-mail"), &[("POP3_HOST", &host), ("POP3_USER", "archive@example.com"), ("POP3_PASSWORD", "secret"), ("ARCHIVE_DIR", &a)]);
    resolve_all(&yaml);
    let plain = yaml.replace("uri: \"pop3s://", "uri: \"pop3://");
    mailbox.deliver("archive@example.com", &raw_mail("Meter reading 7", "kWh=1234"));
    let main = start_yaml("archive-mail", &plain, &[], |_| {});
    wait_for("the archived mail", || mailbox.count("archive@example.com") == 0 && find_eml(&archive).is_some());
    let eml = std::fs::read_to_string(find_eml(&archive).unwrap()).unwrap();
    assert!(eml.contains("Subject: Meter reading 7") && eml.contains("kWh=1234"), "raw message expected: {}", eml);
    main.stop().unwrap();

    let ctx = CamelContext::new();
    cameleer_protocols::register(&ctx);
    let a2 = slash(&dir.join("archive2"));
    let uri = format!("pop3://{}?username=archive@example.com&password=secret&mapMailMessage=false&delete=true&initialDelay=0&delay=50", host);
    ctx.add_routes(move |b| archive_mail_route(b, &uri, &a2)).unwrap();
    ctx.start().unwrap();
    mailbox.deliver("archive@example.com", &raw_mail("Meter reading 8", "kWh=99"));
    wait_for("the second archived mail", || find_eml(&dir.join("archive2")).is_some());
    ctx.stop().unwrap();
    pop3.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

fn find_eml(dir: &Path) -> Option<PathBuf> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(f) = find_eml(&p) {
                return Some(f);
            }
        } else if p.extension().map(|x| x == "eml").unwrap_or(false) {
            return Some(p);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// rss, atom
// ---------------------------------------------------------------------------

use cameleer_saas::testing::FeedMock;

const RSS: &str = r#"<?xml version="1.0"?>
<rss version="2.0"><channel><title>Plant news</title><link>https://news.example.com</link><description>d</description>
<item><title>Line 3 back online</title><link>https://news.example.com/3</link><guid>n3</guid><pubDate>Tue, 03 Nov 2026 10:00:00 GMT</pubDate></item>
<item><title>New safety rules</title><link>https://news.example.com/2</link><guid>n2</guid><pubDate>Mon, 02 Nov 2026 10:00:00 GMT</pubDate></item>
</channel></rss>"#;

const ATOM: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom"><title>Releases</title><id>urn:releases</id><updated>2026-11-03T10:00:00Z</updated>
<entry><title>v4.23.0</title><id>urn:r:4.23.0</id><link href="https://example.com/releases/4.23.0"/><updated>2026-11-03T10:00:00Z</updated></entry>
<entry><title>v4.22.1</title><id>urn:r:4.22.1</id><link href="https://example.com/releases/4.22.1"/><updated>2026-10-20T10:00:00Z</updated></entry>
</feed>"#;

fn news_digest_routes(b: &mut RouteBuilder, feed_url: &str) {
    b.from(&format!("rss:{}?delay=300000&throttleEntries=false", feed_url))
        .route_id("news-digest")
        .set_body(simple("${body[entries][0][title]} - ${body[entries][0][link]}"))
        .to("seda:digest");
    b.from("seda:digest").route_id("summarizer").log("new item: ${body}");
}

fn sorted_bodies(m: &MockEndpoint) -> Vec<String> {
    let mut v: Vec<String> = m.received_exchanges().iter().map(|e| e.body().to_string()).collect();
    v.sort();
    v
}

#[test]
fn rss_news_digest() {
    let feeds = FeedMock::start();
    feeds.set("/news.xml", RSS);
    let url = feeds.url("/news.xml");
    let yaml = subst(&example("rss", "rss-news-digest"), &[("FEED_URL", &url)]);
    resolve_all(&yaml);
    let yaml = yaml.replace("        - log: \"new item: ${body}\"", "        - to: mock:digest");
    let main = start_yaml("news-digest", &yaml, &[], |_| {});
    let m = main.context().mock_endpoint("mock:digest").unwrap();
    wait_for("both entries", || m.received_exchanges().len() == 2);
    assert_eq!(sorted_bodies(&m), vec!["Line 3 back online - https://news.example.com/3".to_string(), "New safety rules - https://news.example.com/2".to_string()]);
    main.stop().unwrap();

    let ctx = CamelContext::new();
    cameleer_saas::register(&ctx);
    ctx.add_routes(move |b| news_digest_routes(b, &url)).unwrap();
    ctx.start().unwrap();
    wait_for("the feed poll", || feeds.server.requests_to("/news.xml").len() >= 2);
    ctx.stop().unwrap();
}

fn release_watch_routes(b: &mut RouteBuilder, feed_url: &str) {
    b.from(&format!("atom:{}?delay=3600000&throttleEntries=false", feed_url))
        .route_id("release-watch")
        .set_body(simple("new release ${body[title]}: ${body[link]}"))
        .to("seda:releases");
    b.from("seda:releases").route_id("notify-release").log("${body}");
}

#[test]
fn atom_release_watch() {
    let feeds = FeedMock::start();
    feeds.set("/releases.atom", ATOM);
    let url = feeds.url("/releases.atom");
    let yaml = subst(&example("atom", "atom-release-watch"), &[("RELEASES_FEED", &url)]);
    resolve_all(&yaml);
    let yaml = yaml.replace("        - log: \"${body}\"", "        - to: mock:releases");
    let main = start_yaml("release-watch", &yaml, &[], |_| {});
    let m = main.context().mock_endpoint("mock:releases").unwrap();
    wait_for("both releases", || m.received_exchanges().len() == 2);
    assert_eq!(
        sorted_bodies(&m),
        vec!["new release v4.22.1: https://example.com/releases/4.22.1".to_string(), "new release v4.23.0: https://example.com/releases/4.23.0".to_string()]
    );
    main.stop().unwrap();

    let ctx = CamelContext::new();
    cameleer_saas::register(&ctx);
    ctx.add_routes(move |b| release_watch_routes(b, &url)).unwrap();
    ctx.start().unwrap();
    wait_for("the feed poll", || feeds.server.requests_to("/releases.atom").len() >= 2);
    ctx.stop().unwrap();
}

// ---------------------------------------------------------------------------
// every page: yaml blocks are marked, ids are unique and all of them are tested above
// ---------------------------------------------------------------------------

const PAGES: &[&str] = &[
    "http", "platform-http", "jetty", "undertow", "netty-http", "rest", "websocket", "ws", "vertx-websocket", "webhook", "graphql", "grpc", "thrift", "cxf",
    "cxfrs", "spring-ws", "fhir", "file", "ftp", "ftps", "sftp", "ssh", "smtp", "imap", "pop3", "rss", "atom",
];

const TESTED: &[&str] = &[
    "http-ask-llm",
    "platform-http-device-readings",
    "jetty-status-endpoint",
    "undertow-command-adapter",
    "netty-http-liveness",
    "rest-agent-tools",
    "rest-producer-get-order",
    "websocket-broadcast-hub",
    "ws-edge-commands",
    "vertx-websocket-agent-presence",
    "webhook-github-pull-requests",
    "graphql-book-lookup",
    "grpc-inventory-service",
    "grpc-call-inventory",
    "thrift-device-registry",
    "cxf-json-to-soap",
    "cxfrs-customer-resource",
    "spring-ws-order-status",
    "fhir-device-observation",
    "file-drop-folder",
    "ftp-collect-orders",
    "ftps-upload-reports",
    "sftp-site-pickup",
    "ssh-device-uptime",
    "smtp-mail-reports",
    "imap-mail-triage",
    "pop3-archive-mail",
    "rss-news-digest",
    "atom-release-watch",
];

#[test]
fn every_example_is_marked_unique_and_tested() {
    let mut ids = Vec::new();
    for page in PAGES {
        let path = format!("{}/../../docs/components/{}.md", env!("CARGO_MANIFEST_DIR"), page);
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {}", path, e)).replace("\r\n", "\n");
        assert!(text.starts_with("# "), "{}: must start with the H1", page);
        let second = text.lines().nth(2).unwrap_or("");
        assert!(second.starts_with("> "), "{}: the answer line must directly follow the H1", page);
        for heading in ["## When to use it", "## URI format", "## Rust DSL", "## Options", "## Message headers", "## Run it with Docker", "## Common questions", "## Related"] {
            assert!(text.contains(heading), "{}: missing section {}", page, heading);
        }
        assert!(!text.contains("ghcr.io"), "{}: use the Docker Hub image", page);
        let lines: Vec<&str> = text.lines().collect();
        for (i, l) in lines.iter().enumerate() {
            if l.trim_start().starts_with("```yaml") {
                let prev = if i > 0 { lines[i - 1] } else { "" };
                let id = prev
                    .strip_prefix("<!-- example: ")
                    .and_then(|r| r.strip_suffix(" -->"))
                    .unwrap_or_else(|| panic!("{}:{}: yaml block without an example marker on the line above", page, i + 1));
                assert!(id.starts_with(&format!("{}-", page)), "{}: example id {} must start with the scheme", page, id);
                ids.push(id.to_string());
            }
        }
    }
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), ids.len(), "duplicate example ids: {:?}", ids);
    let mut tested: Vec<String> = TESTED.iter().map(|s| s.to_string()).collect();
    tested.sort();
    assert_eq!(sorted, tested, "every example id must have a test (and every tested id must exist)");
}

// END OF TESTS

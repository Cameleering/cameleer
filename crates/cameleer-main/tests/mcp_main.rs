//! The MCP server started by Main (`camel.server.mcp-*`) and by `cameleer run --mcp-stdio`.
#![cfg(feature = "ai")]

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

use cameleer_core::value::Value;
use cameleer_http::client::HttpClient;

const ROUTES: &str = r#"
- route:
    from:
      uri: ai-tool:greet
      parameters:
        description: Greet someone
        tags: public
        parameter.name: string
        parameter.name.required: true
        readOnlyHint: true
      steps:
        - setBody:
            simple: "Hello ${header.name}"
- route:
    from:
      uri: ai-tool:hidden
      parameters:
        tags: internal
        readOnlyHint: true
      steps:
        - setBody:
            constant: secret
"#;

fn json(s: &str) -> Value {
    cameleer_core::json::parse(s).unwrap()
}

fn routes_file(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("cameleer-mcp-main-{}-{}", std::process::id(), name));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("tools.camel.yaml");
    std::fs::write(&f, ROUTES).unwrap();
    f
}

#[test]
fn main_serves_mcp_over_http_with_the_configured_tags() {
    let f = routes_file("http");
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let mut main = cameleer_main::new_main();
    main.add_route_file(f.to_string_lossy().to_string());
    main.set_property("camel.server.mcp-enabled", "true");
    main.set_property("camel.server.mcp-tags", "public");
    main.set_property("camel.server.host", "127.0.0.1");
    main.set_property("camel.server.port", port.to_string());
    main.set_property("camel.server.mcp-server-name", "orders-agent-tools");
    main.start().unwrap();
    let url = format!("http://127.0.0.1:{}/mcp", port);
    let call = |method: &str, name: Option<&str>, params: &str| {
        let body = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"{}","params":{{"_meta":{{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{{}}}}{}}}}}"#,
            method, params
        );
        let mut h = vec![("Content-Type", "application/json"), ("MCP-Protocol-Version", "2026-07-28"), ("Mcp-Method", method)];
        if let Some(n) = name {
            h.push(("Mcp-Name", n));
        }
        let r = HttpClient::new().request("POST", &url, &h, Some(body.as_bytes()), Some(Duration::from_secs(10))).unwrap();
        json(&r.body_text())
    };
    let d = call("server/discover", None, "");
    assert!(d.to_json().contains("orders-agent-tools"), "{}", d.to_json());
    let list = call("tools/list", None, "").to_json();
    assert!(list.contains("\"greet\"") && !list.contains("hidden"), "{}", list);
    let r = call("tools/call", Some("greet"), r#","name":"greet","arguments":{"name":"Ann"}"#);
    assert!(r.to_json().contains("Hello Ann"), "{}", r.to_json());
    main.stop().unwrap();
    // the endpoint is gone with Main
    assert!(HttpClient::new().request("POST", &url, &[], Some(b"{}"), Some(Duration::from_secs(2))).map(|r| r.status == 404).unwrap_or(true));
}

#[test]
fn cameleer_run_mcp_stdio() {
    let f = routes_file("stdio");
    let mut child = Command::new(env!("CARGO_BIN_EXE_cameleer"))
        .args(["run", f.to_string_lossy().as_ref(), "--mcp-stdio"])
        .env("CAMEL_LOCAL_STATUS", "false")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut send = |s: &str| {
        stdin.write_all(format!("{}\n", s).as_bytes()).unwrap();
        stdin.flush().unwrap();
    };
    let mut recv = || {
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        json(line.trim())
    };
    send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#);
    let init = recv();
    assert!(init.to_json().contains("\"protocolVersion\":\"2025-11-25\""), "{}", init.to_json());
    send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    send(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
    let list = recv().to_json();
    // --mcp-stdio exposes every tool unless camel.server.mcp-tags is set
    assert!(list.contains("\"greet\"") && list.contains("\"hidden\""), "{}", list);
    send(r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"greet","arguments":{"name":"Bo"}}}"#);
    assert!(recv().to_json().contains("Hello Bo"));
    drop(send);
    drop(stdin); // closing stdin stops the process
    let mut waited = 0;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "exit status {:?}", status);
            break;
        }
        waited += 1;
        assert!(waited < 200, "cameleer did not stop after stdin closed");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn an_agent_uses_tools_of_a_stdio_mcp_server() {
    use cameleer_core::processor::agent::AgentDefinition;
    let f = routes_file("stdio-client");
    let ai = cameleer_saas::testing::OpenAiMock::start();
    let mut main = cameleer_main::new_main();
    main.set_property("camel.ai.mcp.local.command", env!("CARGO_BIN_EXE_cameleer"));
    main.set_property("camel.ai.mcp.local.args", format!("run,{},--mcp-stdio", f.to_string_lossy().replace('\\', "/")));
    main.set_property("camel.ai.mcp.local.env.CAMEL_LOCAL_STATUS", "false");
    main.set_property("camel.ai.mcp.local.include", "greet");
    let model = format!("openai:chat-completion?model=m&apiKey={}&baseUrl={}", cameleer_saas::testing::OpenAiMock::API_KEY, ai.base_url());
    main.configure(move |ctx| {
        ctx.add_routes(move |b| {
            b.from("direct:agent").agent(AgentDefinition::new(model.as_str()).tools("mcp:local"));
        })
    });
    main.start().unwrap();
    let ctx = main.context().clone();
    let reg = cameleer_ai::AiRegistry::of(&ctx).unwrap();
    assert_eq!(reg.tools(&["mcp:local"]).iter().map(|t| t.name.clone()).collect::<Vec<_>>(), ["local_greet"]);
    let out = ctx.create_producer_template().send("direct:agent", cameleer_core::message::Exchange::with_body(Some(ctx.clone()), r#"call local_greet {"name":"Ann"}"#));
    assert!(out.exception.is_none(), "{:?}", out.exception);
    assert_eq!(out.body().to_string(), "Done: Hello Ann");
    main.stop().unwrap();
}

/// An existing route becomes an MCP tool by changing its first line
/// (`examples/agents/route-to-mcp-tool.camel.yaml`, the MCP pages): the file differs from the
/// original `direct:` route in exactly one line, it serves the tool as `cameleer run --mcp`
/// does, and the same change in the Rust DSL does the same.
#[test]
fn an_existing_route_becomes_a_tool_by_changing_one_line() {
    let path = format!("{}/../../examples/agents/route-to-mcp-tool.camel.yaml", env!("CARGO_MANIFEST_DIR"));
    let after = std::fs::read_to_string(&path).unwrap().replace("\r\n", "\n");
    let tool_line = after.lines().find(|l| l.trim_start().starts_with("uri: \"ai-tool:")).expect("the ai-tool line").to_string();
    let before = after.replace(&tool_line, "      uri: direct:lookup-order");
    let changed: Vec<_> = before.lines().zip(after.lines()).filter(|(a, b)| a != b).collect();
    assert_eq!(changed.len(), 1, "one changed line: {:?}", changed);
    assert_eq!(before.lines().count(), after.lines().count());

    // the original route, called by another route
    let mut main = cameleer_main::new_main();
    main.add_route_source("before.camel.yaml", before);
    main.start().unwrap();
    let out = main.context().create_producer_template().send(
        "direct:lookup-order",
        cameleer_core::message::Exchange::with_body(Some(main.context().clone()), ""),
    );
    assert!(out.exception.is_none(), "{:?}", out.exception);
    main.stop().unwrap();

    // the changed file, as `cameleer run route-to-mcp-tool.camel.yaml --mcp` serves it
    let call = |port: u16, args: &str| {
        let body = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"_meta":{{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{{}}}},"name":"lookup_order","arguments":{}}}}}"#,
            args
        );
        let h = [("Content-Type", "application/json"), ("MCP-Protocol-Version", "2026-07-28"), ("Mcp-Method", "tools/call"), ("Mcp-Name", "lookup_order")];
        let url = format!("http://127.0.0.1:{}/mcp", port);
        json(&HttpClient::new().request("POST", &url, &h, Some(body.as_bytes()), Some(Duration::from_secs(10))).unwrap().body_text())
    };
    let free = || std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let port = free();
    let mut main = cameleer_main::new_main();
    main.add_route_file(&path);
    main.set_property("camel.server.mcp-enabled", "true");
    main.set_property("camel.server.mcp-tags", "*,untagged");
    main.set_property("camel.server.host", "127.0.0.1");
    main.set_property("camel.server.port", port.to_string());
    main.start().unwrap();
    let r = call(port, r#"{"orderId":"EU-4411"}"#).to_json();
    assert!(r.contains("EU-4411") && r.contains("shipped"), "{}", r);
    let list = {
        let body = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#;
        let h = [("Content-Type", "application/json"), ("MCP-Protocol-Version", "2026-07-28"), ("Mcp-Method", "tools/list")];
        HttpClient::new().request("POST", &format!("http://127.0.0.1:{}/mcp", port), &h, Some(body.as_bytes()), Some(Duration::from_secs(10))).unwrap().body_text()
    };
    assert!(list.contains("Look up an order by id") && list.contains("\"readOnlyHint\":true"), "{}", list);
    // the declared parameter is checked before the route runs
    let r = call(port, "{}").to_json();
    assert!(r.contains("isError") && r.contains("orderId"), "{}", r);
    main.stop().unwrap();

    // the same one-line change in the Rust DSL
    let port = free();
    let mut main = cameleer_main::new_main();
    main.set_property("camel.server.mcp-enabled", "true");
    main.set_property("camel.server.mcp-tags", "*,untagged");
    main.set_property("camel.server.host", "127.0.0.1");
    main.set_property("camel.server.port", port.to_string());
    main.configure(|ctx| {
        ctx.add_routes(|b| {
            b.from("ai-tool:lookup_order?description=Look up an order by id&parameter.orderId=string&parameter.orderId.required=true")
                .set_body(cameleer_core::prelude::simple(r#"{"orderId": "${header.orderId}", "status": "shipped"}"#));
        })
    });
    main.start().unwrap();
    let r = call(port, r#"{"orderId":"EU-4411"}"#).to_json();
    assert!(r.contains("EU-4411") && r.contains("shipped"), "{}", r);
    main.stop().unwrap();
}

//! Existing routes served as MCP tools with no route edits: `camel.server.mcp-expose-routes`
//! and `cameleer run --mcp-expose`.
#![cfg(feature = "ai")]

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use cameleer_ai::{AiRegistry, ToolCall, ToolStatus};
use cameleer_core::value::Value;
use cameleer_http::client::HttpClient;

/// An ordinary integration: a REST API, a direct route, an ai-tool route and a timer, none of
/// them written with MCP in mind (except the ai-tool).
const ROUTES: &str = r#"
- rest:
    path: /orders
    get:
      - path: /{id}
        description: Look up one order by its number
        param:
          - name: id
            type: path
            dataType: integer
            description: The order number
          - name: view
            type: query
            dataType: string
            allowableValues:
              - summary
              - full
            defaultValue: summary
        to: direct:get-order
    post:
      - description: Create an order
        consumes: application/json
        to: direct:create-order
- route:
    id: get-order
    from:
      uri: direct:get-order
      steps:
        - choice:
            when:
              - simple: "${header.id} == 404"
                steps:
                  - setHeader:
                      name: CamelHttpResponseCode
                      constant: 404
                  - setBody:
                      constant: no such order
            otherwise:
              steps:
                - setBody:
                    simple: '{"orderId": ${header.id}, "view": "${header.view}", "path": "${header.CamelHttpPath}", "method": "${header.CamelHttpMethod}"}'
- route:
    id: create-order
    from:
      uri: direct:create-order
      steps:
        - setHeader:
            name: CamelHttpResponseCode
            constant: 201
        - setBody:
            simple: '{"created": true, "item": "${body[item]}"}'
- route:
    id: lookup-customer
    description: Find a customer by email address
    from:
      uri: direct:lookup-customer
      steps:
        - setBody:
            simple: "customer for ${body} in ${header.region}"
- route:
    id: already-a-tool
    from:
      uri: ai-tool:already
      parameters:
        description: An existing ai-tool route
        tags: public
        readOnlyHint: true
      steps:
        - setBody:
            constant: hi
"#;

fn json(s: &str) -> Value {
    cameleer_core::json::parse(s).unwrap()
}

fn routes_file(name: &str) -> std::path::PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!("cameleer-mcp-expose-{}-{}-{}", std::process::id(), name, N.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("orders.camel.yaml");
    std::fs::write(&f, ROUTES).unwrap();
    f
}

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

fn start(name: &str, props: &[(&str, &str)]) -> (cameleer_main::Main, String) {
    let f = routes_file(name);
    let port = free_port();
    let mut main = cameleer_main::new_main();
    main.add_route_file(f.to_string_lossy().to_string());
    main.set_property("camel.server.host", "127.0.0.1");
    main.set_property("camel.server.port", port.to_string());
    for (k, v) in props {
        main.set_property(*k, *v);
    }
    main.start().unwrap();
    (main, format!("http://127.0.0.1:{}/mcp", port))
}

fn mcp(url: &str, method: &str, name: Option<&str>, params: &str) -> Value {
    let body = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"{}","params":{{"_meta":{{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{{}}}}{}}}}}"#,
        method, params
    );
    let mut h = vec![("Content-Type", "application/json"), ("MCP-Protocol-Version", "2026-07-28"), ("Mcp-Method", method)];
    if let Some(n) = name {
        h.push(("Mcp-Name", n));
    }
    let r = HttpClient::new().request("POST", url, &h, Some(body.as_bytes()), Some(Duration::from_secs(10))).unwrap();
    json(&r.body_text())
}

fn tool<'a>(list: &'a Value, name: &str) -> &'a Value {
    list.get("result")
        .and_then(|r| r.get("tools"))
        .and_then(|t| t.as_array())
        .and_then(|t| t.iter().find(|x| x.get("name").map(|n| n.to_string()) == Some(name.to_string())))
        .unwrap_or_else(|| panic!("no tool {} in {}", name, list.to_json()))
}

#[test]
fn rest_operations_become_typed_tools() {
    let (main, url) = start("rest", &[("camel.server.mcp-enabled", "true"), ("camel.server.mcp-tags", "public"), ("camel.server.mcp-expose-routes", "rest:*")]);
    let list = mcp(&url, "tools/list", None, "");
    // the tag filter still applies to the other tools; exposed ones are served because they were asked for
    assert!(list.to_json().contains("\"already\""), "{}", list.to_json());
    assert!(!list.to_json().contains("\"lookup-customer\""), "only rest:* was asked for: {}", list.to_json());
    // GET /orders/{id}: typed, documented, read-only
    let get = tool(&list, "get_orders_id");
    let schema = get.get("inputSchema").unwrap();
    assert_eq!(schema.get("properties").and_then(|p| p.get("id")).and_then(|i| i.get("type")).map(|t| t.to_string()), Some("integer".into()), "{}", get.to_json());
    let view = schema.get("properties").and_then(|p| p.get("view")).unwrap();
    assert_eq!(view.get("enum").map(|e| e.to_json()), Some(r#"["summary","full"]"#.into()));
    assert_eq!(view.get("default").map(|d| d.to_string()), Some("summary".into()));
    assert_eq!(schema.get("required").map(|r| r.to_json()), Some(r#"["id"]"#.into()));
    assert_eq!(get.get("description").map(|d| d.to_string()), Some("Look up one order by its number".into()));
    let ann = get.get("annotations").unwrap();
    assert_eq!(ann.get("readOnlyHint"), Some(&Value::Bool(true)), "{}", get.to_json());
    // calling it runs the REST route as the HTTP consumer would: path filled in, query as a header
    let r = mcp(&url, "tools/call", Some("get_orders_id"), r#","name":"get_orders_id","arguments":{"id":1007,"view":"full"}"#);
    let text = r.to_json();
    assert!(text.contains(r#""orderId":1007"#) && text.contains(r#""view":"full""#) && text.contains(r#""path":"/orders/1007""#) && text.contains(r#""method":"GET""#), "{}", text);
    assert!(text.contains("structuredContent"), "a JSON reply is structured content: {}", text);
    // a 404 from the route is a tool error the model can read, not a hidden failure
    let r = mcp(&url, "tools/call", Some("get_orders_id"), r#","name":"get_orders_id","arguments":{"id":404}"#);
    let text = r.to_json();
    assert!(text.contains(r#""isError":true"#) && text.contains("HTTP 404: no such order"), "{}", text);
    // a missing required argument is refused by the schema check before the route runs
    let r = mcp(&url, "tools/call", Some("get_orders_id"), r#","name":"get_orders_id","arguments":{}"#);
    assert!(r.to_json().contains("id"), "{}", r.to_json());
    drop(main);
}

#[test]
fn a_post_is_destructive_and_needs_approval() {
    let (main, _url) = start("post", &[("camel.server.mcp-expose-routes", "rest:/orders")]);
    let reg = AiRegistry::of(main.context()).unwrap();
    let def = reg.tool("post_orders").expect("POST /orders exposed");
    assert_eq!(def.hints.destructive, Some(true));
    assert!(def.tags.contains(&"exposed".to_string()));
    assert!(def.declared_arguments().contains(&"body".to_string()), "{:?}", def.declared_arguments());
    let r = reg.call(ToolCall::new("post_orders", json(r#"{"body":{"item":"tent"}}"#)));
    let ToolStatus::ApprovalRequired { handle } = r.status else { panic!("a destructive exposed tool needs approval: {:?}", r.text) };
    let r = reg.approve(&handle, "ann");
    assert!(!r.is_error, "{}", r.text);
    assert_eq!(r.structured.as_ref().and_then(|s| s.get("item")).map(|i| i.to_string()), Some("tent".into()), "{}", r.text);
    drop(main);
}

#[test]
fn a_direct_route_takes_body_and_headers_and_overrides_come_from_properties() {
    let (main, _url) = start(
        "direct",
        &[
            ("camel.server.mcp-expose-routes", "direct:lookup-*"),
            ("camel.server.mcp-expose-routes.lookup-customer.approval", "never"),
            ("camel.server.mcp-expose-routes.lookup-customer.readOnlyHint", "true"),
            ("camel.server.mcp-expose-routes.lookup-customer.name", "find_customer"),
        ],
    );
    let reg = AiRegistry::of(main.context()).unwrap();
    assert!(reg.tool("get-order").is_none(), "direct:get-order does not match direct:lookup-*");
    let def = reg.tool("find_customer").expect("renamed by the override");
    assert_eq!(def.description, "Find a customer by email address", "the route's description");
    assert_eq!(def.hints.read_only, Some(true));
    let r = reg.call(ToolCall::new("find_customer", json(r#"{"body":"ann@example.com","headers":{"region":"EU"}}"#)));
    assert!(!r.is_error && matches!(r.status, ToolStatus::Completed), "{:?} {}", r.status, r.text);
    assert_eq!(r.text, "customer for ann@example.com in EU");
    // ai-tool routes are tools already and are never exposed twice
    assert!(reg.tool("already-a-tool").is_none());
    drop(main);
}

#[test]
fn route_ids_select_routes_and_bad_overrides_are_refused() {
    let (main, _url) = start("ids", &[("camel.server.mcp-expose-routes", "route:create-*")]);
    let reg = AiRegistry::of(main.context()).unwrap();
    assert!(reg.tool("create-order").is_some());
    assert!(reg.tool("lookup-customer").is_none());
    drop(main);

    let f = routes_file("bad");
    let mut main = cameleer_main::new_main();
    main.add_route_file(f.to_string_lossy().to_string());
    main.set_property("camel.server.port", free_port().to_string());
    main.set_property("camel.server.mcp-expose-routes", "direct:lookup-customer");
    main.set_property("camel.server.mcp-expose-routes.lookup-customer.aproval", "never");
    let err = main.start().unwrap_err();
    assert!(err.message.contains("aproval"), "a misspelt option is reported: {}", err.message);
}

#[test]
fn cameleer_run_mcp_expose_over_stdio() {
    let f = routes_file("stdio");
    let mut child = Command::new(env!("CARGO_BIN_EXE_cameleer"))
        .args(["run", f.to_string_lossy().as_ref(), "--mcp-stdio", "--mcp-expose", "direct:lookup-*,rest:*", "--prop", &format!("camel.server.port={}", free_port())])
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
    recv();
    send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    send(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
    let list = recv().to_json();
    for name in ["\"lookup-customer\"", "\"get_orders_id\"", "\"post_orders\"", "\"already\""] {
        assert!(list.contains(name), "{} missing: {}", name, list);
    }
    assert!(!list.contains("\"get-order\""), "direct:get-order was not asked for: {}", list);
    send(r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_orders_id","arguments":{"id":7}}}"#);
    let r = recv().to_json();
    assert!(r.contains("orderId") && r.contains("/orders/7"), "{}", r);
    drop(send);
    drop(stdin);
    let mut waited = 0;
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        waited += 1;
        assert!(waited < 200, "cameleer did not stop after stdin closed");
        std::thread::sleep(Duration::from_millis(50));
    }
}

//! The MCP shop example (`examples/agents/mcp-shop`) and the MCP gateway example
//! (`examples/agents/mcp-gateway`) over real MCP HTTP requests in the 2026-07-28 era: one test
//! per use-case page in `docs/guides/mcp/`. The routes and properties are the example files as
//! they are; only the port is changed. The pages' route excerpts are checked to be verbatim parts
//! of the example file.
#![cfg(feature = "full")]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use cameleer_core::value::Value;
use cameleer_http::client::HttpClient;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn shop_dir() -> PathBuf {
    repo().join("examples/agents/mcp-shop")
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn json(s: &str) -> Value {
    cameleer_core::json::parse(s).unwrap_or_else(|e| panic!("{}: {}", e, s))
}

/// A client that asks its user (elicitation forms), as Claude Desktop does.
const CAPS: &str = r#"{"elicitation":{"form":{}}}"#;

struct Mcp {
    url: String,
}

impl Mcp {
    fn post(&self, method: &str, name: Option<&str>, params: &str) -> Value {
        let body = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"{}","params":{{"_meta":{{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}{}}}}}"#,
            method, CAPS, params
        );
        let mut h = vec![("Content-Type", "application/json"), ("MCP-Protocol-Version", "2026-07-28"), ("Mcp-Method", method)];
        if let Some(n) = name {
            h.push(("Mcp-Name", n));
        }
        let r = HttpClient::new().request("POST", &self.url, &h, Some(body.as_bytes()), Some(Duration::from_secs(20))).unwrap();
        json(&r.body_text())
    }

    /// `tools/call`; returns the `result` object.
    fn call(&self, tool: &str, args: &str) -> Value {
        let r = self.post("tools/call", Some(tool), &format!(r#","name":"{}","arguments":{}"#, tool, args));
        r.get("result").cloned().unwrap_or_else(|| panic!("{} {}: {}", tool, args, r.to_json()))
    }

    /// Answers the input request `key` of an `input_required` result and calls again.
    fn answer(&self, tool: &str, args: &str, first: &Value, key: &str, action: &str, content: &str) -> Value {
        assert_eq!(text_of(first, "resultType"), "input_required", "{}", first.to_json());
        let state = text_of(first, "requestState");
        let r = self.post(
            "tools/call",
            Some(tool),
            &format!(r#","name":"{}","arguments":{},"inputResponses":{{"{}":{{"action":"{}","content":{}}}}},"requestState":"{}""#, tool, args, key, action, content, state),
        );
        r.get("result").cloned().unwrap_or_else(|| panic!("{}: {}", tool, r.to_json()))
    }
}

fn text_of(v: &Value, key: &str) -> String {
    v.get(key).map(|x| x.to_string()).unwrap_or_default()
}

/// The text of a tool result (all text content blocks).
fn text(result: &Value) -> String {
    result.get("content").and_then(|c| c.as_array()).map(|a| a.iter().filter_map(|b| b.get("text").map(|t| t.to_string())).collect::<Vec<_>>().join("\n")).unwrap_or_default()
}

fn is_error(result: &Value) -> bool {
    result.get("isError") == Some(&Value::Bool(true))
}

/// The first input request key of an `input_required` result.
fn input_key(result: &Value) -> String {
    result.get("inputRequests").and_then(|r| r.as_object()).and_then(|o| o.keys().next().cloned()).unwrap_or_else(|| panic!("no input request: {}", result.to_json()))
}

/// Starts the shop example as `cameleer run shop.camel.yaml --properties application.properties`
/// does, on a free port.
fn start_shop() -> (cameleer_main::Main, Mcp) {
    let port = free_port();
    let mut main = cameleer_main::new_main();
    main.load_properties_file(shop_dir().join("application.properties")).unwrap();
    main.set_property("acme.dir", shop_dir().to_string_lossy().replace('\\', "/"));
    main.set_property("camel.server.port", port.to_string());
    main.add_route_file(shop_dir().join("shop.camel.yaml"));
    main.start().unwrap();
    let mcp = Mcp { url: format!("http://127.0.0.1:{}/mcp", port) };
    // a timer route seeds the data at start-up, statement by statement; tickets come last
    let deadline = Instant::now() + Duration::from_secs(20);
    while !text(&mcp.call("list_tickets", r#"{"priority":"urgent"}"#)).contains("T-105") {
        assert!(Instant::now() < deadline, "the shop was never seeded");
        std::thread::sleep(Duration::from_millis(50));
    }
    (main, mcp)
}

/// The shop's database (`jdbc:rustsql:mem:acme`) is shared by name within a process, so the use
/// cases run in order against one shop, the way one person would try them.
#[test]
fn shop_use_cases() {
    let (main, mcp) = start_shop();
    shop_lookups(&main, &mcp);
    shop_multi_step(&main, &mcp);
    shop_approvals(&main, &mcp);
    shop_refund_rules(&main, &mcp);
    shop_manager_approval(&main, &mcp);
    shop_elicitation(&main, &mcp);
    shop_safety(&main, &mcp);
    shop_prompts_and_resources(&main, &mcp);
    main.stop().unwrap();
}

// docs/guides/mcp/customer-service-lookups.md
fn shop_lookups(main: &cameleer_main::Main, mcp: &Mcp) {
    let _ = main;
    let t = text(&mcp.call("track_shipment", r#"{"orderId":1007}"#));
    assert!(t.contains("DPD15501007") && t.contains("2026-11-08") && t.contains("Hinckley"), "{}", t);
    let t = text(&mcp.call("search_products", r#"{"query":"Trailblazer"}"#));
    assert!(t.contains("TENT-TB2") && t.contains("2026-11-14"), "{}", t);
    let t = text(&mcp.call("search_customers", r#"{"query":"Priya Natarajan"}"#));
    assert!(t.contains("priya.n@example.com"), "{}", t);
    let r = mcp.call("track_shipment", r#"{"orderId":9999}"#);
    assert!(is_error(&r) && text(&r).contains("Order 9999 does not exist"), "{}", r.to_json());
}

// docs/guides/mcp/multi-step-questions.md
fn shop_multi_step(main: &cameleer_main::Main, mcp: &Mcp) {
    let _ = main;
    let t = text(&mcp.call("search_customers", r#"{"query":"Sam Lee"}"#));
    assert!(t.contains("sam.lee@example.com") && t.contains("samuel.lee@example.org"), "two customers share the name: {}", t);
    let t = text(&mcp.call("check_inventory", r#"{"lowStockOnly":true}"#));
    assert!(t.contains("LAMP-HD") && t.contains("TENT-TB2") && t.contains("BTL-1L"), "{}", t);
    let t = text(&mcp.call("list_orders", r#"{"status":"paid"}"#));
    for id in ["1012", "1016", "1018", "1019", "1022"] {
        assert!(t.contains(id), "paid order {} missing: {}", id, t);
    }
    let t = text(&mcp.call("sales_report", r#"{"from":"2026-10-26","to":"2026-11-01","groupBy":"product"}"#));
    assert!(t.contains("Summit 4 Family Tent"), "{}", t);
}

// docs/guides/mcp/approvals.md
fn shop_approvals(main: &cameleer_main::Main, mcp: &Mcp) {
    let _ = main;
    // the server asks the user before a destructive tool runs; accepting runs it
    let args = r#"{"orderId":1021,"reason":"Customer changed their mind"}"#;
    let first = mcp.call("cancel_order", args);
    let key = input_key(&first);
    assert!(first.to_json().contains("cancel_order"), "{}", first.to_json());
    let r = mcp.answer("cancel_order", args, &first, &key, "accept", r#"{"approve":true}"#);
    assert!(text(&r).contains(r#""status": "cancelled""#), "{}", r.to_json());
    // declining runs nothing, and the result says so
    let args = r#"{"orderId":1011,"reason":"wrong size"}"#;
    let first = mcp.call("issue_refund", args);
    let key = input_key(&first);
    let r = mcp.answer("issue_refund", args, &first, &key, "decline", "{}");
    let t = text(&r);
    assert!(t.contains("declined it in the approval form") && t.contains("nothing was changed"), "{}", t);
    let t = text(&mcp.call("get_order", r#"{"orderId":1011}"#));
    assert!(t.contains(r#""refunded":0"#) || t.contains(r#""refunded": 0"#), "nothing refunded: {}", t);
    // a business rule still applies after the user approved: order 1003 was delivered
    let args = r#"{"orderId":1003,"reason":"customer asked"}"#;
    let first = mcp.call("cancel_order", args);
    let key = input_key(&first);
    let r = mcp.answer("cancel_order", args, &first, &key, "accept", r#"{"approve":true}"#);
    assert!(is_error(&r) && text(&r).contains("can no longer be cancelled"), "{}", r.to_json());
    // an address change on a paid order needs no approval; on a shipped one the route refuses
    let t = text(&mcp.call("update_shipping_address", r#"{"orderId":1018,"address":"22 Harbour Road, Bristol BS1 5TT"}"#));
    assert!(t.contains("updated"), "{}", t);
    let r = mcp.call("update_shipping_address", r#"{"orderId":1007,"address":"9 Elm St, Leeds"}"#);
    assert!(is_error(&r) && text(&r).contains("can no longer be changed"), "{}", r.to_json());
}

// docs/guides/mcp/refunds-with-business-rules.md
fn shop_refund_rules(main: &cameleer_main::Main, mcp: &Mcp) {
    let _ = main;
    let approve = |args: &str| {
        let first = mcp.call("issue_refund", args);
        let key = input_key(&first);
        mcp.answer("issue_refund", args, &first, &key, "accept", r#"{"approve":true}"#)
    };
    // delivered 45 days ago, not damaged: outside the window
    let r = approve(r#"{"orderId":1002,"reason":"didn't like it"}"#);
    assert!(is_error(&r) && text(&r).contains("45 days ago, outside the 30-day refund window"), "{}", r.to_json());
    // over the limit
    let r = approve(r#"{"orderId":1020,"reason":"customer request","amount":900}"#);
    assert!(is_error(&r) && text(&r).contains("over 500 need a manager"), "{}", r.to_json());
    // one line of an order, defective
    let r = approve(r#"{"orderId":1015,"reason":"boots fell apart after a week","damagedOrDefective":true,"amount":179}"#);
    let t = text(&r);
    assert!(!is_error(&r) && t.contains("RF-") && t.contains(r#""amount": 179"#), "{}", t);
    // within 30 days, the remaining amount
    let r = approve(r#"{"orderId":1011,"reason":"wrong size"}"#);
    assert!(text(&r).contains(r#""amount": 159"#), "{}", r.to_json());
    // already refunded in full
    let r = approve(r#"{"orderId":1011,"reason":"wrong size again"}"#);
    assert!(is_error(&r) && text(&r).contains("already been refunded in full"), "{}", r.to_json());
}

// docs/guides/mcp/manager-approval.md
fn shop_manager_approval(main: &cameleer_main::Main, mcp: &Mcp) {
    let _ = main;
    let reg = cameleer_ai::AiRegistry::of(main.context()).unwrap();
    // the call is parked for a manager; calling again does not ask the manager twice
    let args = r#"{"customerId":2,"amount":20,"reason":"Goodwill gesture: order 1002 is outside the refund window"}"#;
    let t = text(&mcp.call("issue_store_credit", args));
    assert!(t.contains("needs approval") && t.contains("approval-"), "{}", t);
    let t2 = text(&mcp.call("issue_store_credit", args));
    assert!(t2.contains("approval-"), "{}", t2);
    let pending = reg.pending_approvals();
    assert_eq!(pending.len(), 1, "one request for the manager");
    // the approval endpoint wrote it to the manager's queue
    // the manager approves; the next call reports the outcome
    let r = reg.approve(&pending[0].handle, "manager.jo");
    assert!(r.text.contains("SC-1"), "{}", r.text);
    let t = text(&mcp.call("issue_store_credit", args));
    assert!(t.contains("SC-1") && t.contains("manager.jo"), "{}", t);
    // a rejection: nothing is credited, and the reason reaches the model
    let args = r#"{"customerId":8,"amount":50,"reason":"Goodwill gesture for the trouble with her boots"}"#;
    mcp.call("issue_store_credit", args);
    let h = reg.pending_approvals()[0].handle.clone();
    reg.deny(&h, "manager.jo", "Goodwill credit is not justified for this order");
    let r = mcp.call("issue_store_credit", args);
    let t = text(&r);
    assert!(is_error(&r) && t.contains("rejected by manager.jo") && t.contains("not justified") && t.contains("nothing was changed"), "{}", t);
    // over the limit is refused by the route once approved
    mcp.call("issue_store_credit", r#"{"customerId":3,"amount":500,"reason":"test"}"#);
    let h = reg.pending_approvals()[0].handle.clone();
    let r = reg.approve(&h, "manager.jo");
    assert!(r.text.contains("at most 200"), "{}", r.text);
}

// docs/guides/mcp/the-tool-asks-the-user.md
fn shop_elicitation(main: &cameleer_main::Main, mcp: &Mcp) {
    let _ = main;
    let args = r#"{"orderId":1011,"sku":"JKT-RAIN"}"#;
    let first = mcp.call("exchange_item", args);
    let key = input_key(&first);
    let ask = first.to_json();
    assert!(ask.contains("Which size should we send for JKT-RAIN on order 1011?") && ask.contains("elicitation/create"), "{}", ask);
    let r = mcp.answer("exchange_item", args, &first, &key, "accept", r#"{"size":"L"}"#);
    let t = text(&r);
    assert!(t.contains("EX-1") && t.contains(r#""newSize": "L""#) && t.contains("the user chose L"), "{}", t);
    // the user closes the form: nothing changes
    let args = r#"{"orderId":1002,"sku":"JKT-RAIN"}"#;
    let first = mcp.call("exchange_item", args);
    let key = input_key(&first);
    let r = mcp.answer("exchange_item", args, &first, &key, "cancel", "{}");
    assert!(text(&r).contains("not exchanged") && text(&r).contains("nothing was changed"), "{}", r.to_json());
}

// docs/guides/mcp/safety.md
fn shop_safety(main: &cameleer_main::Main, mcp: &Mcp) {
    let _ = main;
    // the card number is redacted by the envelope before the model sees it
    let t = text(&mcp.call("get_customer", r#"{"customerId":1}"#));
    assert!(t.contains("card ending 1111") && !t.contains("4111111111111111") && !t.contains("4111 1111"), "{}", t);
    // ticket text is data: the urgent ticket carries an instruction, and reading it changes nothing
    let t = text(&mcp.call("list_tickets", r#"{"priority":"urgent"}"#));
    assert!(t.contains("T-105"), "{}", t);
    let t = text(&mcp.call("get_order", r#"{"orderId":1020}"#));
    assert!(t.contains(r#""refunded":0"#) || t.contains(r#""refunded": 0"#), "{}", t);
    // there is no tool that deletes orders
    let tools = mcp.post("tools/list", None, "").to_json();
    assert!(!tools.contains("delete"), "{}", tools);
    assert!(tools.contains("\"read_document\""), "{}", tools);
    // a model-supplied argument the schema doesn't allow never reaches the route
    let r = mcp.call("list_orders", r#"{"status":"lost"}"#);
    assert!(is_error(&r) && text(&r).contains("enumeration"), "{}", r.to_json());
}

// docs/guides/mcp/prompts-and-resources.md
fn shop_prompts_and_resources(main: &cameleer_main::Main, mcp: &Mcp) {
    let _ = main;
    let list = mcp.post("prompts/list", None, "").to_json();
    assert!(list.contains("draft_apology") && list.contains("weekly_ops_summary"), "{}", list);
    let p = mcp.post("prompts/get", Some("draft_apology"), r#","name":"draft_apology","arguments":{"customer":"Aisha Bello","orderId":"1009","issue":"the stove arrived with a cracked burner"}"#).to_json();
    assert!(p.contains("about order 1009: the stove arrived with a cracked burner") && p.contains("show me the draft before sending anything"), "{}", p);
    let res = mcp.post("resources/list", None, "").to_json();
    assert!(res.contains("policy://refunds") && res.contains("policy://shipping") && res.contains("policy://support-guide"), "{}", res);
    let r = mcp.post("resources/read", Some("policy://refunds"), r#","uri":"policy://refunds""#).to_json();
    assert!(r.contains("within 30 days of delivery"), "{}", r);
    // the same documents through a tool, for clients that do not attach resources themselves
    let t = text(&mcp.call("read_document", r#"{"uri":"policy://shipping"}"#));
    assert!(t.contains("redirects the parcel with the carrier"), "{}", t);
    let t = text(&mcp.call("read_document", r#"{"uri":"order://1007"}"#));
    assert!(t.contains("Jordan Kim"), "{}", t);
}

// docs/guides/mcp/gateway.md
#[test]
fn gateway_imports_the_shop_over_stdio() {
    let gw = repo().join("examples/agents/mcp-gateway");
    let port = free_port();
    let mut main = cameleer_main::new_main();
    main.load_properties_file(gw.join("application.properties")).unwrap();
    // the example starts `cameleer` from PATH with paths relative to its directory; here the
    // binary under test and absolute paths
    let shop = shop_dir().to_string_lossy().replace('\\', "/");
    let args = main.properties().get("camel.ai.mcp.shop.args").cloned().unwrap();
    assert!(args.contains("../mcp-shop/"), "{}", args);
    main.set_property("camel.ai.mcp.shop.command", env!("CARGO_BIN_EXE_cameleer"));
    main.set_property("camel.ai.mcp.shop.args", args.replace("../mcp-shop", &shop));
    main.set_property("camel.ai.mcp.shop.env.CAMEL_LOCAL_STATUS", "false");
    main.set_property("camel.server.port", port.to_string());
    // the test polls list_tickets until the shop has seeded its data; without a cache for that
    // one tool (the others keep the example's 30 s cache)
    main.set_property("camel.ai.mcp.shop.tools.list_tickets.cacheTtl", "0");
    main.add_route_file(gw.join("gateway.camel.yaml"));
    main.start().unwrap();
    let mcp = Mcp { url: format!("http://127.0.0.1:{}/mcp", port) };
    let deadline = Instant::now() + Duration::from_secs(30);
    let tools = loop {
        let t = mcp.post("tools/list", None, "").to_json();
        if t.contains("shop_track_shipment") {
            break t;
        }
        assert!(Instant::now() < deadline, "the shop's tools were never imported: {}", t);
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(tools.contains("\"support_hours\"") && tools.contains("shop_read_document") && tools.contains("shop_search_customers"), "{}", tools);
    for absent in ["shop_issue_refund", "shop_cancel_order", "shop_send_customer_email", "shop_issue_store_credit"] {
        assert!(!tools.contains(absent), "{} must not be imported: {}", absent, tools);
    }
    // the shop seeds its data at start-up, tickets last; wait for it
    while !text(&mcp.call("shop_list_tickets", r#"{"priority":"urgent"}"#)).contains("T-105") {
        assert!(Instant::now() < deadline, "the shop behind the gateway was never seeded");
        std::thread::sleep(Duration::from_millis(100));
    }
    // a call through the gateway reaches the shop
    let t = text(&mcp.call("shop_track_shipment", r#"{"orderId":1007}"#));
    assert!(t.contains("DPD15501007"), "{}", t);
    let t = text(&mcp.call("support_hours", "{}"));
    assert!(t.contains("Europe/London"), "{}", t);
    main.stop().unwrap();
}

/// Every route excerpt on the use-case pages (`<!-- example: mcp-shop-... -->` blocks) is a
/// verbatim part of the example file, so the pages show the code that the tests above run.
#[test]
fn use_case_pages_show_the_example_file() {
    let file = std::fs::read_to_string(shop_dir().join("shop.camel.yaml")).unwrap().replace("\r\n", "\n");
    let dir = repo().join("docs/guides/mcp");
    let mut checked = 0;
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        let page = std::fs::read_to_string(e.path()).unwrap().replace("\r\n", "\n");
        let mut rest = page.as_str();
        while let Some(i) = rest.find("<!-- example: mcp-shop-") {
            let after = &rest[i..];
            let start = after.find("```yaml\n").unwrap() + 8;
            let end = start + after[start..].find("\n```").unwrap();
            let block = &after[start..end + 1];
            assert!(file.contains(block), "{}: a block is not part of shop.camel.yaml:\n{}", e.path().display(), block);
            checked += 1;
            rest = &after[end..];
        }
    }
    assert!(checked >= 8, "only {} excerpts found", checked);
    // the gateway page shows the gateway example's route and properties
    let page = std::fs::read_to_string(dir.join("gateway.md")).unwrap().replace("
", "
");
    let gw = repo().join("examples/agents/mcp-gateway");
    let routes = std::fs::read_to_string(gw.join("gateway.camel.yaml")).unwrap().replace("
", "
");
    let routes = routes.lines().skip_while(|l| l.starts_with('#')).collect::<Vec<_>>().join("
") + "
";
    assert!(page.contains(&format!("```yaml
{}```", routes)), "gateway.md does not show gateway.camel.yaml");
    let props = std::fs::read_to_string(gw.join("application.properties")).unwrap().replace("
", "
");
    let block = page.split("```properties
").nth(1).unwrap().split("```").next().unwrap();
    for line in block.lines().filter(|l| !l.trim().is_empty()) {
        assert!(props.lines().any(|p| p == line), "gateway.md shows a property the example does not set: {}", line);
    }
}

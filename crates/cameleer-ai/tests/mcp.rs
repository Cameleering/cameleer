//! The MCP server over real connections: Streamable HTTP in both protocol eras, stdio, and
//! authentication.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::Duration;

use cameleer_ai::mcp::auth::AuthConfig;
use cameleer_ai::mcp::http::McpHttpServer;
use cameleer_ai::mcp::{McpConfig, McpServer};
use cameleer_ai::AiRegistry;
use cameleer_core::prelude::*;
use cameleer_http::client::HttpClient;

const V: &str = "2026-07-28";

fn json(s: &str) -> Value {
    cameleer_core::json::parse(s).unwrap()
}

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_ai::register(&ctx);
    ctx.add_routes(|b| {
        b.from("ai-tool:weather?description=Weather for a city&tags=public&parameter.city=string&parameter.city.required=true&readOnlyHint=true")
            .set_body(simple("${header.city}: 21 degrees for ${header.CamelMcpSecurityPrincipal}"));
        b.from("ai-tool:order?tags=public&parameter.id=integer&readOnlyHint=true").process(|ex: &mut Exchange| {
            let id = ex.header("id").and_then(|v| v.as_i64()).unwrap_or(0);
            ex.set_body(json(&format!(r#"{{"id":{},"status":"shipped"}}"#, id)));
            Ok(())
        });
        b.from("ai-tool:delete_account?tags=public&parameter.user=string&destructiveHint=true").set_body(simple("deleted ${header.user}"));
        b.from("ai-tool:secret_admin?tags=internal&readOnlyHint=true").set_body(constant("nope"));
        b.from("ai-resource:handbook?uri=docs://handbook&tags=public&mimeType=text/markdown").set_body(constant("# Handbook"));
        b.from("ai-resource:order?uri=orders://{id}&tags=public&values.id=41,42,420").set_body(simple("order ${header.id}"));
        b.from("ai-prompt:review?tags=public&argument.code.required=true&argument.style.values=rust,go,ruby").set_body(simple("Review ${header.code}"));
    })
    .unwrap();
    ctx.start().unwrap();
    ctx
}

fn config() -> McpConfig {
    McpConfig { enabled: true, host: "127.0.0.1".into(), port: 0, tags: vec!["public".into()], keep_alive: Duration::from_millis(200), ..Default::default() }
}

struct Http {
    base: String,
    port: u16,
}

impl Http {
    fn start(ctx: &CamelContext, cfg: McpConfig) -> (Arc<McpServer>, McpHttpServer, Http) {
        let server = McpServer::new(ctx, cfg).unwrap();
        let http = McpHttpServer::start(server.clone()).unwrap();
        let port = http.port();
        (server, http, Http { base: format!("http://127.0.0.1:{}/mcp", port), port })
    }

    fn post(&self, headers: &[(&str, &str)], body: &Value) -> (u16, Value, cameleer_http::http::HttpResponse) {
        let mut h: Vec<(&str, &str)> = vec![("Content-Type", "application/json"), ("Accept", "application/json, text/event-stream")];
        h.extend_from_slice(headers);
        let r = HttpClient::new().request("POST", &self.base, &h, Some(body.to_json().as_bytes()), Some(Duration::from_secs(10))).unwrap();
        let text = r.body_text();
        let v = if text.trim().is_empty() { Value::Null } else { cameleer_core::json::parse(&text).unwrap_or(Value::from(text)) };
        (r.status, v, r)
    }

    /// A 2026-07-28 request with matching headers.
    fn modern(&self, id: i64, method: &str, mut params: Value, caps: &str, extra: &[(&str, &str)]) -> (u16, Value) {
        let meta = json(&format!(r#"{{"io.modelcontextprotocol/protocolVersion":"{}","io.modelcontextprotocol/clientCapabilities":{}}}"#, V, caps));
        params.as_object_mut().unwrap().insert("_meta".into(), meta);
        let name = params.as_object().unwrap().get("name").or_else(|| params.as_object().unwrap().get("uri")).map(|v| v.to_string());
        let mut h: Vec<(&str, &str)> = extra.to_vec();
        let has = |h: &Vec<(&str, &str)>, k: &str| h.iter().any(|(x, _)| x.eq_ignore_ascii_case(k));
        if !has(&h, "MCP-Protocol-Version") {
            h.push(("MCP-Protocol-Version", V));
        }
        if !has(&h, "Mcp-Method") {
            h.push(("Mcp-Method", method));
        }
        let name_s;
        if let Some(n) = name {
            name_s = n;
            if !has(&h, "Mcp-Name") {
                h.push(("Mcp-Name", name_s.as_str()));
            }
        }
        let msg = Value::object([("jsonrpc", Value::from("2.0")), ("id", Value::Int(id)), ("method", Value::from(method)), ("params", params)]);
        let (s, v, _) = self.post(&h, &msg);
        (s, v)
    }
}

/// A value by path; object keys may contain dots (the longest matching key wins).
fn get<'a>(v: &'a Value, path: &str) -> &'a Value {
    let mut cur = v;
    let mut rest = path;
    while !rest.is_empty() {
        let (next, remaining) = match cur {
            Value::Object(m) => {
                let key = m
                    .keys()
                    .filter(|k| rest == k.as_str() || rest.starts_with(&format!("{}.", k)))
                    .max_by_key(|k| k.len())
                    .unwrap_or_else(|| panic!("no {} in {}", rest, v.to_json()));
                (m.get(key).unwrap(), rest[key.len()..].trim_start_matches('.'))
            }
            Value::Array(a) => {
                let (i, r) = rest.split_once('.').unwrap_or((rest, ""));
                (&a[i.parse::<usize>().unwrap()], r)
            }
            _ => panic!("no {} in {}", rest, v.to_json()),
        };
        cur = next;
        rest = remaining;
    }
    cur
}

/// A raw HTTP/1.1 exchange whose response is an event stream, read event by event.
struct Sse {
    reader: BufReader<TcpStream>,
    buf: String,
}

impl Sse {
    fn open(port: u16, method: &str, headers: &[(&str, &str)], body: Option<&Value>) -> (u16, Sse) {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let body = body.map(|b| b.to_json()).unwrap_or_default();
        let mut req = format!("{} /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\nContent-Length: {}\r\n", method, body.len());
        for (k, v) in headers {
            req.push_str(&format!("{}: {}\r\n", k, v));
        }
        req.push_str("\r\n");
        req.push_str(&body);
        s.write_all(req.as_bytes()).unwrap();
        let mut reader = BufReader::new(s);
        let mut status_line = String::new();
        reader.read_line(&mut status_line).unwrap();
        let status: u16 = status_line.split_whitespace().nth(1).unwrap().parse().unwrap();
        loop {
            let mut l = String::new();
            reader.read_line(&mut l).unwrap();
            if l == "\r\n" || l.is_empty() {
                break;
            }
        }
        (status, Sse { reader, buf: String::new() })
    }

    /// The next `data:` event as JSON (comments skipped).
    fn next(&mut self) -> Value {
        loop {
            if let Some(end) = self.buf.find("\n\n") {
                let event: String = self.buf[..end].to_string();
                self.buf.drain(..end + 2);
                if let Some(data) = event.lines().find_map(|l| l.strip_prefix("data: ")) {
                    return json(data);
                }
                continue;
            }
            // one chunk of the chunked body
            let mut size = String::new();
            self.reader.read_line(&mut size).unwrap();
            let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
            if n == 0 {
                panic!("stream ended; buffered: {}", self.buf);
            }
            let mut chunk = vec![0u8; n + 2];
            self.reader.read_exact(&mut chunk).unwrap();
            self.buf.push_str(&String::from_utf8_lossy(&chunk[..n]));
        }
    }
}

#[test]
fn modern_discover_list_and_call() {
    let ctx = context();
    let (_s, _h, c) = Http::start(&ctx, config());
    let (st, r) = c.modern(1, "server/discover", json("{}"), "{}", &[]);
    assert_eq!(st, 200, "{}", r.to_json());
    let res = get(&r, "result");
    assert_eq!(get(res, "resultType").to_string(), "complete");
    assert_eq!(get(res, "supportedVersions").to_json(), r#"["2026-07-28"]"#);
    assert_eq!(get(res, "capabilities.resources.subscribe"), &Value::Bool(true));
    // as in Camel, the server is named after the context unless camel.server.mcp-server-name is set
    assert_eq!(get(res, "_meta.io.modelcontextprotocol/serverInfo.name").to_string(), ctx.name());
    assert_eq!(get(res, "cacheScope").to_string(), "public");

    let (_, r) = c.modern(2, "tools/list", json("{}"), "{}", &[]);
    let names: Vec<String> = get(&r, "result.tools").as_array().unwrap().iter().map(|t| get(t, "name").to_string()).collect();
    assert_eq!(names, ["delete_account", "order", "weather"]); // secret_admin is tagged internal
    assert!(get(&r, "result").as_object().unwrap().contains_key("ttlMs"));

    let (_, r) = c.modern(3, "tools/call", json(r#"{"name":"weather","arguments":{"city":"Oslo"}}"#), "{}", &[]);
    assert_eq!(get(&r, "result.content.0.text").to_string(), "Oslo: 21 degrees for anonymous");
    assert_eq!(get(&r, "result.isError"), &Value::Bool(false));
    assert_eq!(get(&r, "result.resultType").to_string(), "complete");
    let (_, r) = c.modern(4, "tools/call", json(r#"{"name":"order","arguments":{"id":7}}"#), "{}", &[]);
    assert_eq!(get(&r, "result.structuredContent.status").to_string(), "shipped");
    // an envelope refusal is a tool error the model can read, not a protocol error
    let (_, r) = c.modern(5, "tools/call", json(r#"{"name":"weather","arguments":{}}"#), "{}", &[]);
    assert_eq!(get(&r, "result.isError"), &Value::Bool(true));
    assert!(get(&r, "result.content.0.text").to_string().contains("is missing but it is required"));
    // a tool that isn't exposed doesn't exist for MCP
    let (st, r) = c.modern(6, "tools/call", json(r#"{"name":"secret_admin","arguments":{}}"#), "{}", &[]);
    assert_eq!((st, get(&r, "error.code").as_i64()), (400, Some(-32602)));
    ctx.stop().unwrap();
}

#[test]
fn modern_protocol_errors() {
    let ctx = context();
    let (_s, _h, c) = Http::start(&ctx, config());
    // headers must mirror the body
    let (st, r) = c.modern(1, "tools/call", json(r#"{"name":"weather","arguments":{"city":"x"}}"#), "{}", &[("Mcp-Name", "order")]);
    assert_eq!((st, get(&r, "error.code").as_i64()), (400, Some(-32020)), "{}", r.to_json());
    // Mcp-Name in base64 form
    let (st, _) = c.modern(2, "tools/call", json(r#"{"name":"weather","arguments":{"city":"x"}}"#), "{}", &[("Mcp-Name", "=?base64?d2VhdGhlcg==?=")]);
    assert_eq!(st, 200);
    // no _meta: invalid params
    let msg = json(r#"{"jsonrpc":"2.0","id":3,"method":"tools/list","params":{}}"#);
    let (st, r, _) = c.post(&[("MCP-Protocol-Version", V), ("Mcp-Method", "tools/list")], &msg);
    assert_eq!((st, get(&r, "error.code").as_i64()), (400, Some(-32602)));
    // an unsupported version
    let msg = json(r#"{"jsonrpc":"2.0","id":4,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"1900-01-01","io.modelcontextprotocol/clientCapabilities":{}}}}"#);
    let (st, r, _) = c.post(&[("MCP-Protocol-Version", "1900-01-01"), ("Mcp-Method", "tools/list")], &msg);
    assert_eq!((st, get(&r, "error.code").as_i64()), (400, Some(-32022)));
    assert_eq!(get(&r, "error.data.requested").to_string(), "1900-01-01");
    assert!(get(&r, "error.data.supported").to_json().contains("2025-11-25"));
    // unknown method
    let (st, r) = c.modern(5, "tools/destroy", json("{}"), "{}", &[]);
    assert_eq!((st, get(&r, "error.code").as_i64()), (404, Some(-32601)));
    // a legacy method in a modern request
    let (st, _) = c.modern(6, "ping", json("{}"), "{}", &[]);
    assert_eq!(st, 404);
    // no GET stream, no batches
    assert_eq!(HttpClient::new().request("GET", &c.base, &[], None, None).unwrap().status, 405);
    let (st, r, _) = c.post(&[], &json("[]"));
    assert_eq!((st, get(&r, "error.code").as_i64()), (400, Some(-32600)));
    // a foreign Origin
    let (st, _) = c.modern(7, "tools/list", json("{}"), "{}", &[("Origin", "https://evil.example.com")]);
    assert_eq!(st, 403);
    let (st, _) = c.modern(8, "tools/list", json("{}"), "{}", &[("Origin", "http://localhost:6274")]);
    assert_eq!(st, 200);
    ctx.stop().unwrap();
}

#[test]
fn modern_approval_uses_multi_round_trip_requests() {
    let ctx = context();
    let (_s, _h, c) = Http::start(&ctx, config());
    let caps = r#"{"elicitation":{"form":{}}}"#;
    let call = r#"{"name":"delete_account","arguments":{"user":"bob"}}"#;
    let (_, r) = c.modern(1, "tools/call", json(call), caps, &[]);
    let res = get(&r, "result");
    assert_eq!(get(res, "resultType").to_string(), "input_required", "{}", r.to_json());
    assert_eq!(get(res, "inputRequests.approval.method").to_string(), "elicitation/create");
    assert_eq!(get(res, "inputRequests.approval.params.mode").to_string(), "form");
    assert!(get(res, "inputRequests.approval.params.message").to_string().contains("delete_account"));
    let state = get(res, "requestState").to_string();

    // tampered state
    let mut bad = state.clone();
    bad.replace_range(20..21, if &state[20..21] == "A" { "B" } else { "A" });
    let retry = |st: &str, action: &str, args: &str| {
        json(&format!(r#"{{"name":"delete_account","arguments":{},"inputResponses":{{"approval":{{"action":"{}","content":{{"approve":true}}}}}},"requestState":"{}"}}"#, args, action, st))
    };
    let (st, r) = c.modern(2, "tools/call", retry(&bad, "accept", r#"{"user":"bob"}"#), caps, &[]);
    assert_eq!((st, get(&r, "error.code").as_i64()), (400, Some(-32602)));
    // the arguments can't change under the approval
    let (_, r) = c.modern(3, "tools/call", retry(&state, "accept", r#"{"user":"alice"}"#), caps, &[]);
    assert!(get(&r, "error.message").to_string().contains("the arguments changed"), "{}", r.to_json());
    // accepted
    let (_, r) = c.modern(4, "tools/call", retry(&state, "accept", r#"{"user":"bob"}"#), caps, &[]);
    assert_eq!(get(&r, "result.content.0.text").to_string(), "deleted bob", "{}", r.to_json());
    // a replay of the same state runs nothing
    let (_, r) = c.modern(5, "tools/call", retry(&state, "accept", r#"{"user":"bob"}"#), caps, &[]);
    assert_eq!(get(&r, "result.isError"), &Value::Bool(true));
    assert!(get(&r, "result.content.0.text").to_string().contains("No call is waiting"));

    // declined
    let (_, r) = c.modern(6, "tools/call", json(r#"{"name":"delete_account","arguments":{"user":"cy"}}"#), caps, &[]);
    let state = get(&r, "result.requestState").to_string();
    let (_, r) = c.modern(7, "tools/call", retry(&state, "decline", r#"{"user":"cy"}"#), caps, &[]);
    assert!(get(&r, "result.content.0.text").to_string().contains("rejected"), "{}", r.to_json());

    // a client that can't ask its user
    let (st, r) = c.modern(8, "tools/call", json(call), "{}", &[]);
    assert_eq!((st, get(&r, "error.code").as_i64()), (400, Some(-32021)));
    assert!(get(&r, "error.data.requiredCapabilities").as_object().unwrap().contains_key("elicitation"));
    ctx.stop().unwrap();
}

#[test]
fn modern_resources_prompts_completion_and_pages() {
    let ctx = context();
    let (_s, _h, c) = Http::start(&ctx, McpConfig { page_size: 2, ..config() });
    let (_, r) = c.modern(1, "resources/read", json(r#"{"uri":"docs://handbook"}"#), "{}", &[]);
    assert_eq!(get(&r, "result.contents.0.text").to_string(), "# Handbook");
    assert_eq!(get(&r, "result.contents.0.mimeType").to_string(), "text/markdown");
    let (_, r) = c.modern(2, "resources/read", json(r#"{"uri":"orders://42"}"#), "{}", &[]);
    assert_eq!(get(&r, "result.contents.0.text").to_string(), "order 42");
    let (st, r) = c.modern(3, "resources/read", json(r#"{"uri":"nope://1"}"#), "{}", &[]);
    assert_eq!((st, get(&r, "error.code").as_i64()), (400, Some(-32602)));
    assert_eq!(get(&r, "error.data.uri").to_string(), "nope://1");
    let (_, r) = c.modern(4, "resources/templates/list", json("{}"), "{}", &[]);
    assert_eq!(get(&r, "result.resourceTemplates.0.uriTemplate").to_string(), "orders://{id}");
    let (_, r) = c.modern(5, "prompts/get", json(r#"{"name":"review","arguments":{"code":"x"}}"#), "{}", &[]);
    assert_eq!(get(&r, "result.messages.0.content.text").to_string(), "Review x");
    let (_, r) = c.modern(6, "completion/complete", json(r#"{"ref":{"type":"ref/prompt","name":"review"},"argument":{"name":"style","value":"r"}}"#), "{}", &[]);
    assert_eq!(get(&r, "result.completion.values").to_json(), r#"["rust","ruby"]"#);
    let (_, r) = c.modern(7, "completion/complete", json(r#"{"ref":{"type":"ref/resource","uri":"orders://{id}"},"argument":{"name":"id","value":"42"}}"#), "{}", &[]);
    assert_eq!(get(&r, "result.completion.values").to_json(), r#"["42","420"]"#);
    // pages of two
    let (_, r) = c.modern(8, "tools/list", json("{}"), "{}", &[]);
    assert_eq!(get(&r, "result.tools").as_array().unwrap().len(), 2);
    let cursor = get(&r, "result.nextCursor").to_string();
    let (_, r) = c.modern(9, "tools/list", json(&format!(r#"{{"cursor":"{}"}}"#, cursor)), "{}", &[]);
    assert_eq!(get(&r, "result.tools").as_array().unwrap().len(), 1);
    assert!(!get(&r, "result").as_object().unwrap().contains_key("nextCursor"));
    let (st, _) = c.modern(10, "tools/list", json(r#"{"cursor":"garbage!"}"#), "{}", &[]);
    assert_eq!(st, 400);
    ctx.stop().unwrap();
}

#[test]
fn modern_subscriptions_listen_streams_changes() {
    let ctx = context();
    let (server, _h, c) = Http::start(&ctx, config());
    let body = json(&format!(
        r#"{{"jsonrpc":"2.0","id":11,"method":"subscriptions/listen","params":{{"_meta":{{"io.modelcontextprotocol/protocolVersion":"{}","io.modelcontextprotocol/clientCapabilities":{{}}}},"notifications":{{"toolsListChanged":true,"resourceSubscriptions":["orders://42"]}}}}}}"#,
        V
    ));
    let (st, mut sse) = Sse::open(c.port, "POST", &[("MCP-Protocol-Version", V), ("Mcp-Method", "subscriptions/listen")], Some(&body));
    assert_eq!(st, 200);
    let ack = sse.next();
    assert_eq!(get(&ack, "method").to_string(), "notifications/subscriptions/acknowledged");
    assert_eq!(get(&ack, "params._meta.io.modelcontextprotocol/subscriptionId"), &Value::Int(11));
    assert_eq!(get(&ack, "params.notifications.toolsListChanged"), &Value::Bool(true));
    server.registry().disable("weather");
    let n = sse.next();
    assert_eq!(get(&n, "method").to_string(), "notifications/tools/list_changed");
    assert_eq!(get(&n, "params._meta.io.modelcontextprotocol/subscriptionId"), &Value::Int(11));
    // a resource update for a subscribed URI; others are not sent
    server.registry().resource_updated("orders://7");
    server.registry().resource_updated("orders://42");
    let n = sse.next();
    assert_eq!(get(&n, "method").to_string(), "notifications/resources/updated");
    assert_eq!(get(&n, "params.uri").to_string(), "orders://42");
    // a disabled tool is gone from the list
    let (_, r) = c.modern(12, "tools/list", json("{}"), "{}", &[]);
    assert!(!get(&r, "result.tools").to_json().contains("weather"));
    ctx.stop().unwrap();
}

#[test]
fn legacy_sessions_over_http() {
    let ctx = context();
    let (server, _h, c) = Http::start(&ctx, config());
    let init = json(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#);
    let (st, r, resp) = c.post(&[], &init);
    assert_eq!(st, 200);
    assert_eq!(get(&r, "result.protocolVersion").to_string(), "2025-06-18");
    assert_eq!(get(&r, "result.serverInfo.name").to_string(), ctx.name());
    assert!(!get(&r, "result").as_object().unwrap().contains_key("resultType"));
    let sid = resp.headers.get("Mcp-Session-Id").unwrap().to_string();
    let h = |extra: &'static str| -> Vec<(String, String)> { vec![("Mcp-Session-Id".into(), sid.clone()), ("MCP-Protocol-Version".into(), extra.into())] };
    let post = |body: &str| {
        let hs = h("2025-06-18");
        let hv: Vec<(&str, &str)> = hs.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let (st, v, _) = c.post(&hv, &json(body));
        (st, v)
    };
    assert_eq!(post(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).0, 202);
    assert_eq!(post(r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#).1.to_json(), r#"{"jsonrpc":"2.0","id":2,"result":{}}"#);
    let (_, r) = post(r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"weather","arguments":{"city":"Rome"}}}"#);
    assert_eq!(get(&r, "result.content.0.text").to_string(), "Rome: 21 degrees for anonymous");
    let (_, r) = post(r#"{"jsonrpc":"2.0","id":4,"method":"resources/read","params":{"uri":"nope://x"}}"#);
    assert_eq!(get(&r, "error.code").as_i64(), Some(-32002));
    // the GET stream carries list_changed and subscribed resource updates
    assert_eq!(post(r#"{"jsonrpc":"2.0","id":5,"method":"resources/subscribe","params":{"uri":"orders://42"}}"#).0, 200);
    let (st, mut sse) = Sse::open(c.port, "GET", &[("Mcp-Session-Id", sid.as_str())], None);
    assert_eq!(st, 200);
    std::thread::sleep(Duration::from_millis(100));
    server.registry().disable("order");
    assert_eq!(get(&sse.next(), "method").to_string(), "notifications/tools/list_changed");
    server.registry().resource_updated("orders://42");
    let n = sse.next();
    assert_eq!((get(&n, "method").to_string().as_str(), get(&n, "params.uri").to_string().as_str()), ("notifications/resources/updated", "orders://42"));
    // unknown session, then DELETE ends the session
    let (st, _, _) = c.post(&[("Mcp-Session-Id", "nope")], &json(r#"{"jsonrpc":"2.0","id":6,"method":"ping"}"#));
    assert_eq!(st, 404);
    assert_eq!(HttpClient::new().request("DELETE", &c.base, &[("Mcp-Session-Id", sid.as_str())], None, None).unwrap().status, 200);
    assert_eq!(post(r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#).0, 404);
    ctx.stop().unwrap();
}

#[test]
fn legacy_approval_uses_elicitation_over_the_response_stream() {
    let ctx = context();
    let (_s, _h, c) = Http::start(&ctx, config());
    let init = json(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{"elicitation":{}}}}"#);
    let (_, _, resp) = c.post(&[], &init);
    let sid = resp.headers.get("Mcp-Session-Id").unwrap().to_string();
    let call = json(r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"delete_account","arguments":{"user":"bob"}}}"#);
    let (st, mut sse) = Sse::open(c.port, "POST", &[("Mcp-Session-Id", sid.as_str())], Some(&call));
    assert_eq!(st, 200);
    let ask = sse.next();
    assert_eq!(get(&ask, "method").to_string(), "elicitation/create");
    assert_eq!(get(&ask, "params.mode").to_string(), "form");
    let ask_id = get(&ask, "id").clone();
    let answer = Value::object([("jsonrpc", Value::from("2.0")), ("id", ask_id), ("result", json(r#"{"action":"accept","content":{"approve":true}}"#))]);
    let (st, _, _) = c.post(&[("Mcp-Session-Id", sid.as_str())], &answer);
    assert_eq!(st, 202);
    let done = sse.next();
    assert_eq!(get(&done, "id"), &Value::Int(2));
    assert_eq!(get(&done, "result.content.0.text").to_string(), "deleted bob");
    // a non-destructive call stays plain JSON
    let (st, r, resp) = c.post(&[("Mcp-Session-Id", sid.as_str())], &json(r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"weather","arguments":{"city":"Oslo"}}}"#));
    assert_eq!((st, resp.headers.get("Content-Type").unwrap_or("")), (200, "application/json"));
    assert!(get(&r, "result.content.0.text").to_string().starts_with("Oslo"));
    ctx.stop().unwrap();
}

// --- stdio ---

struct ChannelWriter(std::sync::mpsc::Sender<String>, Vec<u8>);

impl Write for ChannelWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.1.extend_from_slice(buf);
        while let Some(i) = self.1.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.1.drain(..=i).collect();
            let _ = self.0.send(String::from_utf8_lossy(&line).trim().to_string());
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Stdio {
    input: std::io::PipeWriter,
    output: Receiver<String>,
}

impl Stdio {
    fn start(ctx: &CamelContext) -> Stdio {
        let server = McpServer::new(ctx, McpConfig { transport: "stdio".into(), ..config() }).unwrap();
        let (reader, writer) = std::io::pipe().unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || cameleer_ai::mcp::stdio::serve(server, BufReader::new(reader), ChannelWriter(tx, Vec::new())));
        Stdio { input: writer, output: rx }
    }
    fn send(&mut self, s: &str) {
        self.input.write_all(format!("{}\n", s).as_bytes()).unwrap();
    }
    fn recv(&self) -> Value {
        json(&self.output.recv_timeout(Duration::from_secs(10)).expect("no message from the server"))
    }
}

#[test]
fn stdio_legacy_session_with_elicitation() {
    let ctx = context();
    let mut io = Stdio::start(&ctx);
    io.send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{"elicitation":{}}}}"#);
    assert_eq!(get(&io.recv(), "result.protocolVersion").to_string(), "2025-11-25");
    io.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    io.send(r#"{"jsonrpc":"2.0","id":2,"method":"resources/read","params":{"uri":"docs://handbook"}}"#);
    assert_eq!(get(&io.recv(), "result.contents.0.text").to_string(), "# Handbook");
    io.send(r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"delete_account","arguments":{"user":"zed"}}}"#);
    let ask = io.recv();
    assert_eq!(get(&ask, "method").to_string(), "elicitation/create");
    io.send(&format!(r#"{{"jsonrpc":"2.0","id":{},"result":{{"action":"decline"}}}}"#, get(&ask, "id").to_json()));
    let done = io.recv();
    assert_eq!(get(&done, "id"), &Value::Int(3));
    assert!(get(&done, "result.content.0.text").to_string().contains("rejected"));
    // list changes arrive as notifications
    AiRegistry::of(&ctx).unwrap().disable("order");
    assert_eq!(get(&io.recv(), "method").to_string(), "notifications/tools/list_changed");
    ctx.stop().unwrap();
}

#[test]
fn stdio_modern_requests_and_listen() {
    let ctx = context();
    let mut io = Stdio::start(&ctx);
    let meta = format!(r#""_meta":{{"io.modelcontextprotocol/protocolVersion":"{}","io.modelcontextprotocol/clientCapabilities":{{}}}}"#, V);
    io.send(&format!(r#"{{"jsonrpc":"2.0","id":"d","method":"server/discover","params":{{{}}}}}"#, meta));
    assert_eq!(get(&io.recv(), "result.resultType").to_string(), "complete");
    io.send(&format!(r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"weather","arguments":{{"city":"Lima"}},{}}}}}"#, meta));
    assert_eq!(get(&io.recv(), "result.content.0.text").to_string(), "Lima: 21 degrees for stdio");
    io.send(r#"{"jsonrpc":"2.0","id":3,"method":"tools/list","params":{}}"#);
    assert_eq!(get(&io.recv(), "error.code").as_i64(), Some(-32602));
    io.send(&format!(r#"{{"jsonrpc":"2.0","id":4,"method":"subscriptions/listen","params":{{{},"notifications":{{"promptsListChanged":true}}}}}}"#, meta));
    assert_eq!(get(&io.recv(), "method").to_string(), "notifications/subscriptions/acknowledged");
    AiRegistry::of(&ctx).unwrap().unregister_prompt("review");
    let n = io.recv();
    assert_eq!(get(&n, "method").to_string(), "notifications/prompts/list_changed");
    assert_eq!(get(&n, "params._meta.io.modelcontextprotocol/subscriptionId"), &Value::Int(4));
    ctx.stop().unwrap();
}

// --- authentication ---

#[test]
fn api_keys_and_challenges() {
    let ctx = context();
    let auth = AuthConfig { mode: "api-key".into(), api_keys: vec![("ci-agent".into(), cameleer_ai::tool::sha256_hex(b"k-0123456789abcdef"))], ..Default::default() };
    let (_s, _h, c) = Http::start(&ctx, McpConfig { auth, ..config() });
    let (st, _, resp) = c.post(&[], &json(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{}}}"#));
    assert_eq!(st, 401);
    assert!(resp.headers.get("WWW-Authenticate").unwrap().starts_with("Bearer realm=\"mcp\""));
    let (_, r) = c.modern(2, "tools/call", json(r#"{"name":"weather","arguments":{"city":"Kyiv"}}"#), "{}", &[("X-API-Key", "k-0123456789abcdef")]);
    assert_eq!(get(&r, "result.content.0.text").to_string(), "Kyiv: 21 degrees for ci-agent");
    let (_, r) = c.modern(3, "tools/list", json("{}"), "{}", &[("X-API-Key", "k-0123456789abcdef")]);
    assert_eq!(get(&r, "result.cacheScope").to_string(), "private");
    ctx.stop().unwrap();
}

#[test]
fn jwt_tokens_and_protected_resource_metadata() {
    let ctx = context();
    let secret = b"0123456789abcdef0123456789abcdef".to_vec();
    let auth = AuthConfig {
        mode: "jwt".into(),
        jwt_secret: Some(secret.clone()),
        resource_url: Some("https://mcp.example.com/mcp".into()),
        audiences: vec!["https://mcp.example.com/mcp".into()],
        authorization_servers: vec!["https://login.example.com".into()],
        required_scopes: vec!["mcp:tools".into()],
        principal_claim: "sub".into(),
        ..Default::default()
    };
    let (_s, _h, c) = Http::start(&ctx, McpConfig { auth, ..config() });
    let meta = HttpClient::new().get(&format!("http://127.0.0.1:{}/.well-known/oauth-protected-resource/mcp", c.port)).unwrap();
    assert_eq!(meta.status, 200);
    let m = json(&meta.body_text());
    assert_eq!(get(&m, "resource").to_string(), "https://mcp.example.com/mcp");
    assert_eq!(get(&m, "authorization_servers.0").to_string(), "https://login.example.com");

    let (st, _, resp) = c.post(&[], &json("{}"));
    assert_eq!(st, 401);
    assert!(resp.headers.get("WWW-Authenticate").unwrap().contains("resource_metadata=\"https://mcp.example.com/.well-known/oauth-protected-resource/mcp\""));

    let b64 = |b: &[u8]| cameleer_tls::pem::base64_encode(b).replace('+', "-").replace('/', "_").trim_end_matches('=').to_string();
    let token = |claims: &str| {
        let h = b64(br#"{"alg":"HS256"}"#);
        let p = b64(claims.as_bytes());
        let sig = cameleer_tls::crypto::hash::HashAlg::Sha256.hmac(&secret, format!("{}.{}", h, p).as_bytes());
        format!("Bearer {}.{}.{}", h, p, b64(&sig))
    };
    let exp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 300;
    let good = token(&format!(r#"{{"sub":"agent-7","aud":"https://mcp.example.com/mcp","exp":{},"scope":"mcp:tools"}}"#, exp));
    let (_, r) = c.modern(1, "tools/call", json(r#"{"name":"weather","arguments":{"city":"Pune"}}"#), "{}", &[("Authorization", good.as_str())]);
    assert_eq!(get(&r, "result.content.0.text").to_string(), "Pune: 21 degrees for agent-7");
    let other_aud = token(&format!(r#"{{"sub":"agent-7","aud":"https://elsewhere","exp":{},"scope":"mcp:tools"}}"#, exp));
    assert_eq!(c.modern(2, "tools/list", json("{}"), "{}", &[("Authorization", other_aud.as_str())]).0, 401);
    let no_scope = token(&format!(r#"{{"sub":"agent-7","aud":"https://mcp.example.com/mcp","exp":{}}}"#, exp));
    assert_eq!(c.modern(3, "tools/list", json("{}"), "{}", &[("Authorization", no_scope.as_str())]).0, 403);
    ctx.stop().unwrap();
}

#[test]
fn rich_content_and_older_legacy_clients() {
    let ctx = CamelContext::new();
    cameleer_ai::register(&ctx);
    ctx.add_routes(|b| {
        b.from("ai-tool:screenshot?tags=public&readOnlyHint=true")
            .set_header("CamelAiToolContent", constant(r#"[{"type":"text","text":"the page"},{"type":"image","mimeType":"image/png","data":"iVBORw0KGgo="}]"#));
        b.from("ai-prompt:look?tags=public&description=Look at an image").process(|ex: &mut Exchange| {
            ex.set_body(json(r#"[{"role":"user","content":{"type":"image","mimeType":"image/png","data":"iVBORw0KGgo="}},{"role":"user","content":"What is it?"}]"#));
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let (_s, _h, c) = Http::start(&ctx, config());
    let (_, r) = c.modern(1, "tools/call", json(r#"{"name":"screenshot","arguments":{}}"#), "{}", &[]);
    assert_eq!(get(&r, "result.content.1.type").to_string(), "image", "{}", r.to_json());
    assert_eq!(get(&r, "result.content.0.text").to_string(), "the page");
    let (_, r) = c.modern(2, "prompts/get", json(r#"{"name":"look"}"#), "{}", &[]);
    assert_eq!(get(&r, "result.messages.0.content.type").to_string(), "image");
    assert_eq!(get(&r, "result.messages.1.content.text").to_string(), "What is it?");
    // a 2025-03-26 client, and logging/setLevel, which is accepted and ignored
    let (_, r, resp) = c.post(&[], &json(r#"{"jsonrpc":"2.0","id":3,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{}}}"#));
    assert_eq!(get(&r, "result.protocolVersion").to_string(), "2025-03-26");
    let sid = resp.headers.get("Mcp-Session-Id").unwrap().to_string();
    let (st, r, _) = c.post(&[("Mcp-Session-Id", sid.as_str()), ("MCP-Protocol-Version", "2025-03-26")], &json(r#"{"jsonrpc":"2.0","id":4,"method":"logging/setLevel","params":{"level":"debug"}}"#));
    assert_eq!((st, r.to_json()), (200, r#"{"jsonrpc":"2.0","id":4,"result":{}}"#.to_string()));
    ctx.stop().unwrap();
}

fn interactive_context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_ai::register(&ctx);
    ctx.add_routes(|b| {
        b.from("ai-tool:report?tags=public&readOnlyHint=true")
            .set_header("CamelAiToolProgress", constant(50))
            .set_header("CamelAiToolProgressTotal", constant(100))
            .set_body(constant("halfway"))
            .to("ai-tool:current?operation=progress")
            .set_header("CamelAiToolLogLevel", constant("info"))
            .set_body(constant("working"))
            .to("ai-tool:current?operation=log")
            .set_header("CamelAiToolLogLevel", constant("debug"))
            .set_body(constant("noise"))
            .to("ai-tool:current?operation=log")
            .set_body(constant("done"));
        b.from("ai-tool:signup?tags=public&readOnlyHint=true")
            .set_header("CamelAiToolElicitSchema", constant(r#"{"type":"object","properties":{"email":{"type":"string"}},"required":["email"]}"#))
            .set_body(constant("Which email should I use?"))
            .to("ai-tool:current?operation=elicit")
            .set_body(simple("signed up ${body[email]} (${header.CamelAiToolElicitAction})"));
        b.from("ai-tool:summarize?tags=public&readOnlyHint=true")
            .set_body(constant("Summarize the release notes"))
            .to("ai-tool:current?operation=sample")
            .set_body(simple("summary: ${body}"));
    })
    .unwrap();
    ctx.start().unwrap();
    ctx
}

#[test]
fn modern_progress_and_logs_stream_on_the_call() {
    let ctx = interactive_context();
    let (_s, _h, c) = Http::start(&ctx, config());
    let body = json(&format!(
        r#"{{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{{"name":"report","arguments":{{}},"_meta":{{"progressToken":"p1","io.modelcontextprotocol/logLevel":"info","io.modelcontextprotocol/protocolVersion":"{}","io.modelcontextprotocol/clientCapabilities":{{}}}}}}}}"#,
        V
    ));
    let (st, mut sse) = Sse::open(c.port, "POST", &[("MCP-Protocol-Version", V), ("Mcp-Method", "tools/call"), ("Mcp-Name", "report")], Some(&body));
    assert_eq!(st, 200);
    let p = sse.next();
    assert_eq!(get(&p, "method").to_string(), "notifications/progress");
    assert_eq!((get(&p, "params.progressToken").to_string().as_str(), get(&p, "params.progress").as_f64(), get(&p, "params.message").to_string().as_str()), ("p1", Some(50.0), "halfway"));
    let l = sse.next();
    assert_eq!(get(&l, "method").to_string(), "notifications/message");
    assert_eq!((get(&l, "params.level").to_string().as_str(), get(&l, "params.data").to_string().as_str()), ("info", "working"));
    // the debug message is below the requested level; next is the result
    let r = sse.next();
    assert_eq!(get(&r, "id"), &Value::Int(5));
    assert_eq!(get(&r, "result.content.0.text").to_string(), "done");
    // without a progress token or log level the call is plain JSON and sends nothing extra
    let (_, r) = c.modern(6, "tools/call", json(r#"{"name":"report","arguments":{}}"#), "{}", &[]);
    assert_eq!(get(&r, "result.content.0.text").to_string(), "done");
    ctx.stop().unwrap();
}

#[test]
fn modern_elicitation_and_sampling_from_a_route_use_multi_round_trips() {
    let ctx = interactive_context();
    let (_s, _h, c) = Http::start(&ctx, config());
    let caps = r#"{"elicitation":{"form":{}},"sampling":{}}"#;
    let (_, r) = c.modern(1, "tools/call", json(r#"{"name":"signup","arguments":{}}"#), caps, &[]);
    assert_eq!(get(&r, "result.resultType").to_string(), "input_required", "{}", r.to_json());
    assert_eq!(get(&r, "result.inputRequests.input-1.method").to_string(), "elicitation/create");
    assert_eq!(get(&r, "result.inputRequests.input-1.params.message").to_string(), "Which email should I use?");
    let state = get(&r, "result.requestState").to_string();
    let retry = json(&format!(r#"{{"name":"signup","arguments":{{}},"inputResponses":{{"input-1":{{"action":"accept","content":{{"email":"ann@example.com"}}}}}},"requestState":"{}"}}"#, state));
    let (_, r) = c.modern(2, "tools/call", retry, caps, &[]);
    assert_eq!(get(&r, "result.content.0.text").to_string(), "signed up ann@example.com (accept)", "{}", r.to_json());
    // sampling
    let (_, r) = c.modern(3, "tools/call", json(r#"{"name":"summarize","arguments":{}}"#), caps, &[]);
    assert_eq!(get(&r, "result.inputRequests.input-1.method").to_string(), "sampling/createMessage");
    let state = get(&r, "result.requestState").to_string();
    let retry = json(&format!(r#"{{"name":"summarize","arguments":{{}},"inputResponses":{{"input-1":{{"role":"assistant","content":{{"type":"text","text":"All good."}},"model":"m"}}}},"requestState":"{}"}}"#, state));
    let (_, r) = c.modern(4, "tools/call", retry, caps, &[]);
    assert_eq!(get(&r, "result.content.0.text").to_string(), "summary: All good.");
    // a client without the capability gets an error result
    let (_, r) = c.modern(5, "tools/call", json(r#"{"name":"signup","arguments":{}}"#), "{}", &[]);
    assert_eq!(get(&r, "result.isError"), &Value::Bool(true));
    assert!(get(&r, "result.content.0.text").to_string().contains("elicitation capability"), "{}", r.to_json());
    ctx.stop().unwrap();
}

#[test]
fn legacy_sampling_and_logs_over_stdio() {
    let ctx = interactive_context();
    let mut io = Stdio::start(&ctx);
    io.send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{"sampling":{},"elicitation":{}}}}"#);
    io.recv();
    io.send(r#"{"jsonrpc":"2.0","id":2,"method":"logging/setLevel","params":{"level":"debug"}}"#);
    io.recv();
    io.send(r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"report","arguments":{},"_meta":{"progressToken":7}}}"#);
    let methods: Vec<String> = (0..3).map(|_| io.recv()).map(|m| m.get("method").map(|x| x.to_string()).unwrap_or_else(|| "result".into())).collect();
    assert_eq!(methods, ["notifications/progress", "notifications/message", "notifications/message"]);
    assert_eq!(get(&io.recv(), "result.content.0.text").to_string(), "done");
    io.send(r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"summarize","arguments":{}}}"#);
    let ask = io.recv();
    assert_eq!(get(&ask, "method").to_string(), "sampling/createMessage");
    io.send(&format!(r#"{{"jsonrpc":"2.0","id":{},"result":{{"role":"assistant","content":{{"type":"text","text":"Short."}},"model":"m"}}}}"#, get(&ask, "id").to_json()));
    assert_eq!(get(&io.recv(), "result.content.0.text").to_string(), "summary: Short.");
    ctx.stop().unwrap();
}

#[test]
fn star_exposes_tagged_items_only_and_route_errors_stay_in_the_log() {
    let ctx = CamelContext::new();
    cameleer_ai::register(&ctx);
    ctx.add_routes(|b| {
        b.from("ai-tool:tagged?tags=a&readOnlyHint=true").set_body(constant("t"));
        b.from("ai-tool:plain?readOnlyHint=true").set_body(constant("p"));
        b.from("ai-tool:broken?tags=a&readOnlyHint=true").throw_exception("java.lang.IllegalStateException", "db password=hunter2 rejected");
    })
    .unwrap();
    ctx.start().unwrap();
    let names = |tags: &[&str]| {
        let (_s, _h, c) = Http::start(&ctx, McpConfig { tags: tags.iter().map(|t| t.to_string()).collect(), ..config() });
        let (_, r) = c.modern(1, "tools/list", json("{}"), "{}", &[]);
        get(&r, "result.tools").as_array().unwrap().iter().map(|t| get(t, "name").to_string()).collect::<Vec<_>>()
    };
    assert_eq!(names(&["*"]), ["broken", "tagged"]);
    assert_eq!(names(&["*", "untagged"]), ["broken", "plain", "tagged"]);
    let (_s, _h, c) = Http::start(&ctx, McpConfig { tags: vec!["*".into()], ..config() });
    let (_, r) = c.modern(2, "tools/call", json(r#"{"name":"broken","arguments":{}}"#), "{}", &[]);
    let text = get(&r, "result.content.0.text").to_string();
    assert!(text.starts_with("Tool broken failed (error ") && !text.contains("hunter2"), "{}", text);
    let (_s2, _h2, c2) = Http::start(&ctx, McpConfig { tags: vec!["*".into()], expose_errors: true, ..config() });
    let (_, r) = c2.modern(3, "tools/call", json(r#"{"name":"broken","arguments":{}}"#), "{}", &[]);
    assert!(get(&r, "result.content.0.text").to_string().contains("rejected"));
    ctx.stop().unwrap();
}

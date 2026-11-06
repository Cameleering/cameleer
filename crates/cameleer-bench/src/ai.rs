//! Scenarios of the action layer for AI agents (Cameleer only, Camel has no equivalent to run
//! the same way):
//!
//! | scenario | where | what it measures |
//! |---|---|---|
//! | `ai-tool` | in process | one tool call through the `ai-tool` envelope (schema check, rate-limit and breaker bookkeeping, audit, a call id each time) |
//! | `ai-tool-direct` | in process | the same route called with `direct:`, the baseline for the envelope's cost |
//! | `mcp` | over the network | a stateless MCP 2026-07-28 `tools/call` over Streamable HTTP, one JSON-RPC request per HTTP request |

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use cameleer_ai::mcp::http::McpHttpServer;
use cameleer_ai::mcp::{McpConfig, McpServer};
use cameleer_ai::registry::{AiRegistry, ToolCall};
use cameleer_core::prelude::*;

use crate::driver::{self, Outcome, Plan};

/// The tool route, also reachable as `direct:lookup-direct` for the baseline.
pub fn add_routes(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("ai-tool:lookup_order?description=Look up an order&tags=bench&parameter.orderId=integer&parameter.orderId.required=true&readOnlyHint=true")
            .route_id("ai-tool")
            .to("direct:lookup");
        b.from("direct:lookup-direct").route_id("ai-tool-direct").to("direct:lookup");
        b.from("direct:lookup").route_id("lookup").set_body(simple("{\"orderId\":${header.orderId},\"status\":\"shipped\"}"));
    })
}

/// Run an in-process AI scenario.
pub fn run(ctx: &CamelContext, scenario: &str, plan: Plan) -> Result<Outcome> {
    match scenario {
        "ai-tool" => {
            let reg = AiRegistry::of(ctx)?;
            let n = Arc::new(AtomicU64::new(0));
            Ok(driver::run(plan, 1, None, |_| {
                let reg = reg.clone();
                let n = n.clone();
                move || {
                    let i = n.fetch_add(1, Ordering::Relaxed);
                    let r = reg.call(ToolCall {
                        name: "lookup_order".into(),
                        arguments: Value::object([("orderId", Value::Int(i as i64))]),
                        call_id: Some(format!("c{i}")),
                        principal: Some("bench".into()),
                        ..Default::default()
                    });
                    assert!(!r.is_error, "{}", r.text);
                }
            }))
        }
        "ai-tool-direct" => {
            let t = ctx.create_producer_template();
            let n = Arc::new(AtomicU64::new(0));
            Ok(driver::run(plan, 1, None, |_| {
                let t = t.clone();
                let n = n.clone();
                move || {
                    let i = n.fetch_add(1, Ordering::Relaxed);
                    t.send_body_and_headers("direct:lookup-direct", Value::Null, vec![("orderId", Value::Int(i as i64))]).expect("direct send");
                }
            }))
        }
        other => Err(CamelError::illegal_argument(format!("unknown AI scenario: {other}"))),
    }
}

/// Serve the tool over MCP (Streamable HTTP, no auth) on `0.0.0.0:port` at `/mcp`.
pub fn serve_mcp(ctx: &CamelContext, port: u16) -> Result<(Arc<McpServer>, McpHttpServer)> {
    let cfg = McpConfig { enabled: true, host: "0.0.0.0".into(), port, tags: vec!["bench".into()], allowed_origins: vec!["*".into()], ..Default::default() };
    let server = McpServer::new(ctx, cfg)?;
    let http = McpHttpServer::start(server.clone())?;
    Ok((server, http))
}

/// The raw bytes of one stateless `tools/call` (2026-07-28) for order `id`.
pub fn mcp_request(host: &str, port: u16, id: u64) -> Vec<u8> {
    let body = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"tools/call\",\"params\":{{\"name\":\"lookup_order\",\"arguments\":{{\"orderId\":{id}}},\"_meta\":{{\"io.modelcontextprotocol/protocolVersion\":\"2026-07-28\",\"io.modelcontextprotocol/clientCapabilities\":{{}},\"io.cameleer/toolCallId\":\"c{id}\"}}}}}}"
    );
    format!(
        "POST /mcp HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nMCP-Protocol-Version: 2026-07-28\r\nMcp-Method: tools/call\r\nMcp-Name: lookup_order\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

/// One response: status, headers (lower-case names) and body; content-length or chunked.
fn read_full<R: std::io::BufRead>(r: &mut R) -> std::io::Result<(u16, Vec<(String, String)>, String)> {
    let bad = |m: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, m.to_string());
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "connection closed"));
    }
    let status: u16 = line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).ok_or_else(|| bad("bad status line"))?;
    let mut headers = Vec::new();
    loop {
        line.clear();
        if r.read_line(&mut line)? == 0 {
            return Err(bad("eof in headers"));
        }
        let h = line.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    let get = |k: &str| headers.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    let mut body = Vec::new();
    if get("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
        loop {
            line.clear();
            r.read_line(&mut line)?;
            let n = usize::from_str_radix(line.trim().split(';').next().unwrap_or(""), 16).map_err(|_| bad("bad chunk"))?;
            if n == 0 {
                line.clear();
                r.read_line(&mut line)?;
                break;
            }
            let mut c = vec![0; n + 2];
            r.read_exact(&mut c)?;
            body.extend_from_slice(&c[..n]);
        }
    } else if let Some(n) = get("content-length").and_then(|v| v.parse::<usize>().ok()) {
        body = vec![0; n];
        r.read_exact(&mut body)?;
    } else if status != 202 && status != 204 {
        r.read_to_end(&mut body)?;
    }
    Ok((status, headers, String::from_utf8_lossy(&body).into_owned()))
}

/// The session flow every MCP server supports (2025-06-18): each connection sends `initialize`
/// and `notifications/initialized`, then `tools/call` with its `Mcp-Session-Id`, as a client
/// such as Claude Desktop or an agent framework does. Used against Cameleer, Apache Camel's
/// `camel-mcp-server` and the official TypeScript SDK alike.
pub struct McpSession {
    host: String,
    port: u16,
    reader: Option<std::io::BufReader<std::net::TcpStream>>,
    session: Option<String>,
    pub connects: Arc<AtomicU64>,
}

impl McpSession {
    pub fn new(host: &str, port: u16, connects: Arc<AtomicU64>) -> McpSession {
        McpSession { host: host.to_string(), port, reader: None, session: None, connects }
    }

    fn post(&mut self, body: &str) -> std::io::Result<(u16, Vec<(String, String)>, String)> {
        use std::io::Write;
        if self.reader.is_none() {
            let s = std::net::TcpStream::connect((self.host.as_str(), self.port))?;
            self.connects.fetch_add(1, Ordering::Relaxed);
            s.set_nodelay(true)?;
            s.set_read_timeout(Some(std::time::Duration::from_secs(30)))?;
            self.reader = Some(std::io::BufReader::with_capacity(16 * 1024, s));
        }
        let session = self.session.as_ref().map(|s| format!("Mcp-Session-Id: {s}\r\n")).unwrap_or_default();
        let req = format!(
            "POST /mcp HTTP/1.1\r\nHost: {}:{}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nMCP-Protocol-Version: 2025-06-18\r\n{session}Content-Length: {}\r\n\r\n{body}",
            self.host,
            self.port,
            body.len()
        );
        let r = self.reader.as_mut().unwrap();
        r.get_mut().write_all(req.as_bytes())?;
        let out = read_full(r)?;
        if out.1.iter().any(|(k, v)| k == "connection" && v.eq_ignore_ascii_case("close")) {
            self.reader = None;
        }
        Ok(out)
    }

    fn open(&mut self) -> std::io::Result<()> {
        self.session = None;
        let (status, headers, body) = self.post(r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"cameleer-bench","version":"1"}}}"#)?;
        if status != 200 || !body.contains("protocolVersion") {
            return Err(std::io::Error::other(format!("initialize failed: {status} {body}")));
        }
        self.session = headers.into_iter().find(|(k, _)| k == "mcp-session-id").map(|(_, v)| v);
        let (status, _, body) = self.post(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)?;
        if status != 202 && status != 200 && status != 204 {
            return Err(std::io::Error::other(format!("notifications/initialized failed: {status} {body}")));
        }
        Ok(())
    }

    /// One `tools/call`; opens the session (again) when needed.
    pub fn call(&mut self, id: u64) -> std::io::Result<()> {
        // a closed keep-alive connection is reopened by `post`; the session stays
        if self.session.is_none() {
            self.open()?;
        }
        let body = format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"lookup_order","arguments":{{"orderId":{id}}}}}}}"#);
        let (status, _, text) = match self.post(&body) {
            Ok(r) => r,
            Err(_) => {
                self.reader = None;
                self.post(&body)?
            }
        };
        if status != 200 || !text.contains("shipped") || text.contains("\"isError\":true") {
            return Err(std::io::Error::other(format!("tools/call failed: {status} {text}")));
        }
        Ok(())
    }
}

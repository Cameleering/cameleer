//! MCP client: call tools on remote MCP servers, and import them into the [`AiRegistry`] so
//! they go through the same envelope as local tools (argument checks, approvals for
//! destructive tools, deduplication, timeouts, audit, pinning). Imported tools can be offered
//! to models (`tools: mcp:github` on the agent step) and re-exposed by Cameleer's own MCP
//! server, which makes Cameleer an MCP gateway.
//!
//! Transports: Streamable HTTP and stdio (a child process). The protocol era is detected as
//! the specification defines: `server/discover` first (2026-07-28); any other answer falls
//! back to `initialize` (2025-11-25, 2025-06-18).
//!
//! Configuration (context properties, one block per server):
//!
//! | property | description |
//! |---|---|
//! | `camel.ai.mcp.NAME.url` | Streamable HTTP endpoint |
//! | `camel.ai.mcp.NAME.command`, `.args` | stdio: the program and its arguments (comma separated) |
//! | `camel.ai.mcp.NAME.header.H` | extra HTTP headers (`header.Authorization=Bearer {{env:TOKEN}}`) |
//! | `camel.ai.mcp.NAME.bearerToken` | `Authorization: Bearer ...` |
//! | `camel.ai.mcp.NAME.toolPrefix` | prefix of imported tool names (default `NAME_`) |
//! | `camel.ai.mcp.NAME.tags` | extra tags of the imported tools (they always have `mcp` and `mcp:NAME`) |
//! | `camel.ai.mcp.NAME.include`, `.exclude` | tool name patterns (`*` globs, comma separated) |
//! | `camel.ai.mcp.NAME.timeout` | per call (default 30s) |
//! | `camel.ai.mcp.NAME.protocol` | `auto` (default), `2026-07-28` or `2025-11-25` |
//! | `camel.ai.mcp.NAME.refreshInterval` | re-list the tools periodically (default: only on `list_changed`) |

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cameleer_core::context::CamelContext;
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::value::{ObjectMap, Value};

use super::{base64url, glob_match, MODERN_VERSION};
use crate::registry::{AiRegistry, ToolEntry};
use crate::tool::{Hints, ToolDefinition};

const LEGACY_VERSION: &str = "2025-11-25";

/// The bean under which the clients are kept.
pub const CLIENTS_BEAN: &str = "cameleer.ai.mcp.clients";

/// How to reach a server.
#[derive(Clone, Debug)]
pub enum Transport {
    Http { url: String, headers: Vec<(String, String)> },
    Stdio { command: String, args: Vec<String>, env: Vec<(String, String)> },
}

/// One server's settings.
#[derive(Clone, Debug)]
pub struct McpClientConfig {
    pub name: String,
    pub transport: Transport,
    pub protocol: String,
    pub timeout: Duration,
    pub tool_prefix: String,
    pub tags: Vec<String>,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub refresh_interval: Option<Duration>,
    /// Envelope options for the imported tools, as on `ai-tool` (`rateLimit`,
    /// `circuitBreakerThreshold`, `approval`, `redact`, `authorize`, `cacheTtl`, ...): for every
    /// tool of the server (`camel.ai.mcp.NAME.rateLimit`), then per tool
    /// (`camel.ai.mcp.NAME.tools.TOOL.rateLimit`, the remote tool's name).
    pub policy: std::collections::BTreeMap<String, String>,
    pub tool_policy: std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
}

/// The `ai-tool` envelope options an MCP client applies to imported tools.
pub const POLICY_OPTIONS: &[&str] = &[
    "idempotency",
    "idempotencyWindow",
    "idempotentRepository",
    "approval",
    "approvalEndpoint",
    "approvalTimeout",
    "rateLimit",
    "rateLimitPeriod",
    "circuitBreakerThreshold",
    "circuitBreakerDelay",
    "circuitBreakerFallback",
    "compensation",
    "audit",
    "authorize",
    "redact",
    "cacheTtl",
    "authorizationPolicy",
];

impl McpClientConfig {
    pub fn http(name: &str, url: &str) -> McpClientConfig {
        McpClientConfig::new(name, Transport::Http { url: url.to_string(), headers: Vec::new() })
    }
    pub fn stdio(name: &str, command: &str, args: &[&str]) -> McpClientConfig {
        McpClientConfig::new(name, Transport::Stdio { command: command.to_string(), args: args.iter().map(|a| a.to_string()).collect(), env: Vec::new() })
    }
    fn new(name: &str, transport: Transport) -> McpClientConfig {
        McpClientConfig {
            name: name.to_string(),
            transport,
            protocol: "auto".into(),
            timeout: Duration::from_secs(30),
            tool_prefix: format!("{}_", name),
            tags: Vec::new(),
            include: Vec::new(),
            exclude: Vec::new(),
            refresh_interval: None,
            policy: Default::default(),
            tool_policy: Default::default(),
        }
    }

    /// Every server configured under `camel.ai.mcp.NAME.*`.
    pub fn from_properties(props: &std::collections::BTreeMap<String, String>) -> Result<Vec<McpClientConfig>> {
        let mut names: Vec<String> = Vec::new();
        for k in props.keys() {
            if let Some(rest) = k.strip_prefix("camel.ai.mcp.") {
                if let Some((n, _)) = rest.split_once('.') {
                    if !names.iter().any(|x| x == n) {
                        names.push(n.to_string());
                    }
                }
            }
        }
        let mut out = Vec::new();
        for n in names {
            let get = |k: &str| props.get(&format!("camel.ai.mcp.{}.{}", n, k)).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
            let csv = |k: &str| get(k).map(|v| super::csv(&v)).unwrap_or_default();
            let mut headers: Vec<(String, String)> = props
                .iter()
                .filter_map(|(k, v)| k.strip_prefix(&format!("camel.ai.mcp.{}.header.", n)).map(|h| (h.to_string(), v.clone())))
                .collect();
            if let Some(t) = get("bearerToken") {
                headers.push(("Authorization".into(), format!("Bearer {}", t)));
            }
            let transport = match (get("url"), get("command")) {
                (Some(url), _) => Transport::Http { url, headers },
                (None, Some(command)) => Transport::Stdio {
                    command,
                    args: csv("args"),
                    env: props.iter().filter_map(|(k, v)| k.strip_prefix(&format!("camel.ai.mcp.{}.env.", n)).map(|e| (e.to_string(), v.clone()))).collect(),
                },
                _ => return Err(CamelError::illegal_argument(format!("camel.ai.mcp.{} needs url (HTTP) or command (stdio)", n))),
            };
            let mut c = McpClientConfig::new(&n, transport);
            if let Some(p) = get("protocol") {
                c.protocol = p;
            }
            if let Some(t) = get("timeout") {
                c.timeout = cameleer_core::util::parse_duration(&t)?;
            }
            if let Some(p) = props.get(&format!("camel.ai.mcp.{}.toolPrefix", n)) {
                c.tool_prefix = p.trim().to_string();
            }
            c.tags = csv("tags");
            c.include = csv("include");
            c.exclude = csv("exclude");
            if let Some(r) = get("refreshInterval") {
                c.refresh_interval = Some(cameleer_core::util::parse_duration(&r)?);
            }
            for o in POLICY_OPTIONS {
                if let Some(v) = get(o) {
                    c.policy.insert(o.to_string(), v);
                }
            }
            let tp = format!("camel.ai.mcp.{}.tools.", n);
            for (k, v) in props {
                if let Some((tool, o)) = k.strip_prefix(&tp).and_then(|r| r.rsplit_once('.')) {
                    if POLICY_OPTIONS.contains(&o) {
                        c.tool_policy.entry(tool.to_string()).or_default().insert(o.to_string(), v.trim().to_string());
                    } else {
                        return Err(CamelError::illegal_argument(format!("{}: unknown tool option {} ({})", k, o, POLICY_OPTIONS.join(", "))));
                    }
                }
            }
            out.push(c);
        }
        Ok(out)
    }

    fn wants(&self, tool: &str) -> bool {
        (self.include.is_empty() || self.include.iter().any(|p| glob_match(p, tool))) && !self.exclude.iter().any(|p| glob_match(p, tool))
    }
}

/// A tool as a remote server lists it.
#[derive(Clone, Debug)]
pub struct RemoteTool {
    pub name: String,
    pub title: Option<String>,
    pub description: String,
    pub input_schema: Value,
    pub output_schema: Option<Value>,
    pub annotations: Hints,
}

/// A remote tool's result.
#[derive(Clone, Debug)]
pub struct RemoteResult {
    pub content: Vec<Value>,
    pub structured: Option<Value>,
    pub is_error: bool,
}

impl RemoteResult {
    /// The text blocks joined.
    pub fn text(&self) -> String {
        self.content.iter().filter(|c| c.get("type").map(|t| t.to_string()).as_deref() == Some("text")).filter_map(|c| c.get("text").map(|t| t.to_string())).collect::<Vec<_>>().join("\n")
    }
}

enum Era {
    Modern,
    Legacy { version: String, session: Option<String> },
}

struct StdioConn {
    child: Child,
    stdin: std::process::ChildStdin,
    pending: Arc<Mutex<HashMap<String, Sender<Value>>>>,
}

/// A connection to one MCP server.
pub struct McpClient {
    pub config: McpClientConfig,
    era: Mutex<Option<Era>>,
    stdio: Mutex<Option<StdioConn>>,
    next_id: AtomicU64,
    changed: Mutex<Vec<Arc<dyn Fn() + Send + Sync>>>,
    closed: Arc<AtomicBool>,
}

impl McpClient {
    /// Connect and detect the protocol era.
    pub fn connect(config: McpClientConfig) -> Result<Arc<McpClient>> {
        let c = Arc::new(McpClient {
            config,
            era: Mutex::new(None),
            stdio: Mutex::new(None),
            next_id: AtomicU64::new(1),
            changed: Mutex::default(),
            closed: Arc::new(AtomicBool::new(false)),
        });
        if let Transport::Stdio { .. } = c.config.transport {
            c.spawn()?;
        }
        c.handshake()?;
        Ok(c)
    }

    /// Whether the server speaks 2026-07-28 (`true`) or a legacy revision.
    pub fn is_modern(&self) -> bool {
        matches!(*self.era.lock().unwrap_or_else(|p| p.into_inner()), Some(Era::Modern))
    }

    /// The negotiated protocol version.
    pub fn protocol_version(&self) -> String {
        match &*self.era.lock().unwrap_or_else(|p| p.into_inner()) {
            Some(Era::Modern) => MODERN_VERSION.to_string(),
            Some(Era::Legacy { version, .. }) => version.clone(),
            None => String::new(),
        }
    }

    /// Run `f` when the server's tool list changes.
    pub fn on_tools_changed(&self, f: impl Fn() + Send + Sync + 'static) {
        self.changed.lock().unwrap_or_else(|p| p.into_inner()).push(Arc::new(f));
    }

    fn fire_changed(&self) {
        let ls: Vec<Arc<dyn Fn() + Send + Sync>> = self.changed.lock().unwrap_or_else(|p| p.into_inner()).clone();
        for l in ls {
            l();
        }
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        if let Some(mut s) = self.stdio.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = s.child.kill();
            let _ = s.child.wait();
        }
        if let Some(Era::Legacy { session: Some(sid), .. }) = &*self.era.lock().unwrap_or_else(|p| p.into_inner()) {
            if let Transport::Http { url, headers } = &self.config.transport {
                let mut h: Vec<(&str, &str)> = headers.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
                h.push(("Mcp-Session-Id", sid.as_str()));
                let _ = cameleer_http::client::HttpClient::new().request("DELETE", url, &h, None, Some(Duration::from_secs(5)));
            }
        }
    }

    // --- handshake ---

    fn handshake(&self) -> Result<()> {
        let proto = self.config.protocol.as_str();
        if proto == "auto" || proto == MODERN_VERSION {
            match self.raw_request("server/discover", ObjectMap::new(), true, Duration::from_secs(10).min(self.config.timeout)) {
                Ok(r) => {
                    let versions = r.get("supportedVersions").and_then(|v| v.as_array()).map(|a| a.iter().map(|x| x.to_string()).collect::<Vec<_>>()).unwrap_or_default();
                    if versions.iter().any(|v| v == MODERN_VERSION) {
                        *self.era.lock().unwrap_or_else(|p| p.into_inner()) = Some(Era::Modern);
                        return Ok(());
                    }
                }
                Err(e) if proto == MODERN_VERSION => return Err(e),
                Err(_) => {}
            }
        }
        // legacy: initialize
        let params = Value::object([
            ("protocolVersion", Value::from(LEGACY_VERSION)),
            ("capabilities", Value::Object(ObjectMap::new())),
            ("clientInfo", Value::object([("name", Value::from("cameleer")), ("version", Value::from(env!("CARGO_PKG_VERSION")))])),
        ]);
        let (result, session) = self.legacy_initialize(params)?;
        let version = result.get("protocolVersion").map(|v| v.to_string()).unwrap_or_else(|| LEGACY_VERSION.into());
        *self.era.lock().unwrap_or_else(|p| p.into_inner()) = Some(Era::Legacy { version, session });
        self.notify("notifications/initialized");
        Ok(())
    }

    fn legacy_initialize(&self, params: Value) -> Result<(Value, Option<String>)> {
        let id = self.next_id();
        let msg = Value::object([("jsonrpc", Value::from("2.0")), ("id", Value::Int(id as i64)), ("method", Value::from("initialize")), ("params", params)]);
        match &self.config.transport {
            Transport::Http { url, headers } => {
                let mut h = base_headers(headers);
                h.push(("Content-Type".into(), "application/json".into()));
                let hv: Vec<(&str, &str)> = h.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
                let r = cameleer_http::client::HttpClient::new()
                    .request("POST", url, &hv, Some(msg.to_json().as_bytes()), Some(self.config.timeout))
                    .map_err(|e| CamelError::io(format!("MCP server {}: {}", self.config.name, e.message)))?;
                if r.status != 200 {
                    return Err(CamelError::io(format!("MCP server {} refused initialize: HTTP {} {}", self.config.name, r.status, r.body_text())));
                }
                let session = r.headers.get("Mcp-Session-Id").map(str::to_string);
                let reply = pick_response(&r.body_text(), r.headers.get("Content-Type").unwrap_or(""), id)?;
                Ok((rpc_result(&self.config.name, reply)?, session))
            }
            Transport::Stdio { .. } => {
                let reply = self.stdio_exchange(&msg, id, self.config.timeout)?;
                Ok((rpc_result(&self.config.name, reply)?, None))
            }
        }
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    // --- requests ---

    /// Send a request in the detected era and return its result.
    pub fn request(&self, method: &str, params: ObjectMap) -> Result<Value> {
        let modern = self.is_modern();
        match self.raw_request(method, params.clone(), modern, self.config.timeout) {
            // a legacy session that expired: open a new one once
            Err(e) if !modern && e.message.contains("HTTP 404") => {
                self.handshake()?;
                self.raw_request(method, params, false, self.config.timeout)
            }
            r => r,
        }
    }

    fn raw_request(&self, method: &str, mut params: ObjectMap, modern: bool, timeout: Duration) -> Result<Value> {
        if modern {
            let mut meta = params.get("_meta").and_then(|m| m.as_object()).cloned().unwrap_or_default();
            meta.insert("io.modelcontextprotocol/protocolVersion".into(), Value::from(MODERN_VERSION));
            meta.insert("io.modelcontextprotocol/clientCapabilities".into(), Value::Object(ObjectMap::new()));
            meta.insert("io.modelcontextprotocol/clientInfo".into(), Value::object([("name", Value::from("cameleer")), ("version", Value::from(env!("CARGO_PKG_VERSION")))]));
            params.insert("_meta".into(), Value::Object(meta));
        }
        let id = self.next_id();
        let name_header = match method {
            "tools/call" | "prompts/get" => params.get("name").map(|v| v.to_string()),
            "resources/read" => params.get("uri").map(|v| v.to_string()),
            _ => None,
        };
        let msg = Value::object([("jsonrpc", Value::from("2.0")), ("id", Value::Int(id as i64)), ("method", Value::from(method)), ("params", Value::Object(params))]);
        let reply = match &self.config.transport {
            Transport::Http { url, headers } => {
                let mut h = base_headers(headers);
                h.push(("Content-Type".into(), "application/json".into()));
                if modern {
                    h.push(("MCP-Protocol-Version".into(), MODERN_VERSION.into()));
                    h.push(("Mcp-Method".into(), method.into()));
                    if let Some(n) = name_header {
                        let safe = n.bytes().all(|b| (0x20..0x7f).contains(&b));
                        h.push(("Mcp-Name".into(), if safe { n } else { format!("=?base64?{}?=", cameleer_tls::pem::base64_encode(n.as_bytes())) }));
                    }
                } else if let Some(Era::Legacy { version, session }) = &*self.era.lock().unwrap_or_else(|p| p.into_inner()) {
                    h.push(("MCP-Protocol-Version".into(), version.clone()));
                    if let Some(s) = session {
                        h.push(("Mcp-Session-Id".into(), s.clone()));
                    }
                }
                let hv: Vec<(&str, &str)> = h.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
                let r = cameleer_http::client::HttpClient::new()
                    .request("POST", url, &hv, Some(msg.to_json().as_bytes()), Some(timeout))
                    .map_err(|e| CamelError::io(format!("MCP server {}: {}", self.config.name, e.message)))?;
                let text = r.body_text();
                if r.status != 200 {
                    // a JSON-RPC error in the body is the real answer
                    if let Ok(v @ Value::Object(_)) = cameleer_core::json::parse(&text) {
                        if v.get("error").is_some() {
                            return rpc_result(&self.config.name, v);
                        }
                    }
                    return Err(CamelError::io(format!("MCP server {} answered HTTP {} to {}", self.config.name, r.status, method)));
                }
                pick_response(&text, r.headers.get("Content-Type").unwrap_or(""), id)?
            }
            Transport::Stdio { .. } => self.stdio_exchange(&msg, id, timeout)?,
        };
        rpc_result(&self.config.name, reply)
    }

    fn notify(&self, method: &str) {
        let msg = Value::object([("jsonrpc", Value::from("2.0")), ("method", Value::from(method))]);
        match &self.config.transport {
            Transport::Http { url, headers } => {
                let mut h = base_headers(headers);
                h.push(("Content-Type".into(), "application/json".into()));
                if let Some(Era::Legacy { version, session }) = &*self.era.lock().unwrap_or_else(|p| p.into_inner()) {
                    h.push(("MCP-Protocol-Version".into(), version.clone()));
                    if let Some(s) = session {
                        h.push(("Mcp-Session-Id".into(), s.clone()));
                    }
                }
                let hv: Vec<(&str, &str)> = h.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
                let _ = cameleer_http::client::HttpClient::new().request("POST", url, &hv, Some(msg.to_json().as_bytes()), Some(Duration::from_secs(5)));
            }
            Transport::Stdio { .. } => {
                let _ = self.stdio_write(&msg);
            }
        }
    }

    // --- stdio ---

    fn spawn(self: &Arc<Self>) -> Result<()> {
        let Transport::Stdio { command, args, env } = &self.config.transport else { return Ok(()) };
        let mut cmd = Command::new(command);
        cmd.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().map_err(|e| CamelError::io(format!("Cannot start the MCP server {} ({}): {}", self.config.name, command, e)))?;
        let stdin = child.stdin.take().ok_or_else(|| CamelError::io("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| CamelError::io("no stdout"))?;
        let pending: Arc<Mutex<HashMap<String, Sender<Value>>>> = Arc::default();
        let p2 = pending.clone();
        let weak = Arc::downgrade(self);
        std::thread::Builder::new()
            .name(format!("mcp-client-{}", self.config.name))
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    let Ok(Value::Object(m)) = cameleer_core::json::parse(&line) else { continue };
                    if let Some(method) = m.get("method").map(|x| x.to_string()) {
                        let Some(me) = weak.upgrade() else { break };
                        if method == "notifications/tools/list_changed" {
                            me.fire_changed();
                        } else if let Some(id) = m.get("id") {
                            // a server request (elicitation, sampling, roots): this client cannot answer it
                            let _ = me.stdio_write(&Value::object([
                                ("jsonrpc", Value::from("2.0")),
                                ("id", id.clone()),
                                ("error", Value::object([("code", Value::Int(-32601)), ("message", Value::from("The Cameleer MCP client does not support this request"))])),
                            ]));
                        }
                        continue;
                    }
                    if let Some(id) = m.get("id") {
                        if let Some(tx) = p2.lock().unwrap_or_else(|p| p.into_inner()).remove(&id.to_json()) {
                            let _ = tx.send(Value::Object(m));
                        }
                    }
                }
            })
            .map_err(|e| CamelError::io(e.to_string()))?;
        *self.stdio.lock().unwrap_or_else(|p| p.into_inner()) = Some(StdioConn { child, stdin, pending });
        Ok(())
    }

    fn stdio_write(&self, msg: &Value) -> Result<()> {
        let mut g = self.stdio.lock().unwrap_or_else(|p| p.into_inner());
        let conn = g.as_mut().ok_or_else(|| CamelError::io(format!("The MCP server {} is not running", self.config.name)))?;
        conn.stdin.write_all(format!("{}\n", msg.to_json()).as_bytes()).and_then(|_| conn.stdin.flush()).map_err(|e| CamelError::io(format!("MCP server {}: {}", self.config.name, e)))
    }

    fn stdio_exchange(&self, msg: &Value, id: u64, timeout: Duration) -> Result<Value> {
        let (tx, rx) = channel();
        {
            let g = self.stdio.lock().unwrap_or_else(|p| p.into_inner());
            let conn = g.as_ref().ok_or_else(|| CamelError::io(format!("The MCP server {} is not running", self.config.name)))?;
            conn.pending.lock().unwrap_or_else(|p| p.into_inner()).insert(Value::Int(id as i64).to_json(), tx);
        }
        self.stdio_write(msg)?;
        rx.recv_timeout(timeout).map_err(|_| CamelError::new(cameleer_core::error::kinds::SOCKET_TIMEOUT, format!("MCP server {} did not answer within {:?}", self.config.name, timeout)))
    }

    // --- MCP operations ---

    /// All the server's tools (following pagination).
    pub fn list_tools(&self) -> Result<Vec<RemoteTool>> {
        let mut out = Vec::new();
        let mut cursor: Option<Value> = None;
        for _ in 0..100 {
            let mut p = ObjectMap::new();
            if let Some(c) = &cursor {
                p.insert("cursor".into(), c.clone());
            }
            let r = self.request("tools/list", p)?;
            for t in r.get("tools").and_then(|t| t.as_array()).cloned().unwrap_or_default() {
                let ann = t.get("annotations");
                let hint = |k: &str| ann.and_then(|a| a.get(k)).and_then(|v| v.as_bool());
                out.push(RemoteTool {
                    name: t.get("name").map(|v| v.to_string()).unwrap_or_default(),
                    title: t.get("title").map(|v| v.to_string()),
                    description: t.get("description").map(|v| v.to_string()).unwrap_or_default(),
                    input_schema: t.get("inputSchema").cloned().unwrap_or_else(|| Value::object([("type", Value::from("object"))])),
                    output_schema: t.get("outputSchema").cloned(),
                    annotations: Hints { read_only: hint("readOnlyHint"), destructive: hint("destructiveHint"), idempotent: hint("idempotentHint"), open_world: hint("openWorldHint") },
                });
            }
            match r.get("nextCursor").filter(|c| !c.is_null()) {
                Some(c) => cursor = Some(c.clone()),
                None => break,
            }
        }
        Ok(out)
    }

    /// Call a tool. An `input_required` answer (the server wants the user's input) is an error:
    /// this client has no user to ask.
    pub fn call_tool(&self, name: &str, arguments: &Value) -> Result<RemoteResult> {
        let mut p = ObjectMap::new();
        p.insert("name".into(), Value::from(name));
        p.insert("arguments".into(), match arguments {
            Value::Object(_) => arguments.clone(),
            _ => Value::Object(ObjectMap::new()),
        });
        let r = self.request("tools/call", p)?;
        if r.get("resultType").map(|t| t.to_string()).as_deref() == Some("input_required") {
            return Err(CamelError::illegal_state(format!("The MCP server {} asked for input (elicitation) that Cameleer cannot provide for {}", self.config.name, name)));
        }
        Ok(RemoteResult {
            content: r.get("content").and_then(|c| c.as_array()).cloned().unwrap_or_default(),
            structured: r.get("structuredContent").filter(|s| !s.is_null()).cloned(),
            is_error: r.get("isError").map(|v| v.is_truthy()).unwrap_or(false),
        })
    }

    /// Read a resource (`contents`).
    pub fn read_resource(&self, uri: &str) -> Result<Vec<Value>> {
        let mut p = ObjectMap::new();
        p.insert("uri".into(), Value::from(uri));
        Ok(self.request("resources/read", p)?.get("contents").and_then(|c| c.as_array()).cloned().unwrap_or_default())
    }

    /// Get a prompt (`messages`).
    pub fn get_prompt(&self, name: &str, arguments: &Value) -> Result<Vec<Value>> {
        let mut p = ObjectMap::new();
        p.insert("name".into(), Value::from(name));
        if let Value::Object(_) = arguments {
            p.insert("arguments".into(), arguments.clone());
        }
        Ok(self.request("prompts/get", p)?.get("messages").and_then(|c| c.as_array()).cloned().unwrap_or_default())
    }

    /// Follow `tools/list_changed` over HTTP: `subscriptions/listen` (2026-07-28) or the
    /// session's GET stream (legacy), on a background thread that reconnects.
    pub fn watch(self: &Arc<Self>) {
        let Transport::Http { url, headers } = self.config.transport.clone() else { return };
        let weak = Arc::downgrade(self);
        let closed = self.closed.clone();
        let _ = std::thread::Builder::new().name(format!("mcp-watch-{}", self.config.name)).spawn(move || {
            while !closed.load(Ordering::SeqCst) {
                let Some(me) = weak.upgrade() else { return };
                if let Err(e) = me.listen_once(&url, &headers) {
                    cameleer_core::camel_debug!("mcp-client", "The change stream of {} ended: {}", me.config.name, e.message);
                }
                drop(me);
                for _ in 0..50 {
                    if closed.load(Ordering::SeqCst) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        });
    }

    fn listen_once(&self, url: &str, headers: &[(String, String)]) -> Result<()> {
        let parsed = cameleer_http::client::Url::parse(url)?;
        let mut h = cameleer_http::http::HttpHeaders::new();
        h.set("Accept", "text/event-stream, application/json");
        for (k, v) in headers {
            h.set(k.clone(), v.clone());
        }
        let (method, body) = if self.is_modern() {
            h.set("Content-Type", "application/json");
            h.set("MCP-Protocol-Version", MODERN_VERSION);
            h.set("Mcp-Method", "subscriptions/listen");
            let msg = Value::object([
                ("jsonrpc", Value::from("2.0")),
                ("id", Value::from(format!("listen-{}", base64url(&cameleer_tls::crypto::rng::random_bytes(6))))),
                ("method", Value::from("subscriptions/listen")),
                (
                    "params",
                    Value::object([
                        (
                            "_meta",
                            Value::object([("io.modelcontextprotocol/protocolVersion", Value::from(MODERN_VERSION)), ("io.modelcontextprotocol/clientCapabilities", Value::Object(ObjectMap::new()))]),
                        ),
                        ("notifications", Value::object([("toolsListChanged", Value::Bool(true))])),
                    ]),
                ),
            ]);
            ("POST", Some(msg.to_json()))
        } else {
            match &*self.era.lock().unwrap_or_else(|p| p.into_inner()) {
                Some(Era::Legacy { version, session: Some(s) }) => {
                    h.set("Mcp-Session-Id", s.clone());
                    h.set("MCP-Protocol-Version", version.clone());
                }
                _ => return Ok(()), // no session, no stream
            }
            ("GET", None)
        };
        // the stream stays open for as long as the server keeps it: no read timeout
        let client = cameleer_http::HttpClient { connect_timeout: Duration::from_secs(10), read_timeout: Duration::ZERO, ..cameleer_http::HttpClient::default() };
        let request_body = match &body {
            Some(b) => cameleer_http::client::RequestBody::Bytes(b.as_bytes()),
            None => cameleer_http::client::RequestBody::None,
        };
        let (head, mut body_reader) = client.execute_streaming(method, &parsed, &h, request_body, None)?;
        if head.status != 200 {
            return Err(CamelError::io(format!("MCP listen answered {}", head.status)));
        }
        // changes made while the stream was not connected are not replayed: re-sync now
        self.fire_changed();
        let mut buf = String::new();
        let mut chunk = [0u8; 4096];
        loop {
            if self.closed.load(Ordering::SeqCst) {
                return Ok(());
            }
            let n = body_reader.read(&mut chunk).map_err(|e| CamelError::io(e.to_string()))?;
            if n == 0 {
                return Ok(());
            }
            buf.push_str(&String::from_utf8_lossy(&chunk[..n]));
            while let Some(end) = buf.find("\n\n") {
                let event: String = buf[..end].to_string();
                buf.drain(..end + 2);
                for line in event.lines() {
                    if let Some(data) = line.strip_prefix("data:") {
                        if let Ok(v) = cameleer_core::json::parse(data.trim()) {
                            if v.get("method").map(|m| m.to_string()).as_deref() == Some("notifications/tools/list_changed") {
                                self.fire_changed();
                            }
                        }
                    }
                }
            }
        }
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
        if let Some(mut s) = self.stdio.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = s.child.kill();
            let _ = s.child.wait();
        }
    }
}

fn base_headers(extra: &[(String, String)]) -> Vec<(String, String)> {
    let mut h = vec![("Accept".to_string(), "application/json, text/event-stream".to_string())];
    h.extend(extra.iter().cloned());
    h
}

/// The JSON-RPC response with `id` from a JSON or event-stream body.
fn pick_response(body: &str, content_type: &str, id: u64) -> Result<Value> {
    let want = Value::Int(id as i64).to_json();
    if content_type.contains("text/event-stream") {
        for event in body.split("\n\n") {
            for line in event.lines() {
                if let Some(data) = line.strip_prefix("data:") {
                    if let Ok(v) = cameleer_core::json::parse(data.trim()) {
                        if v.get("id").map(|i| i.to_json()) == Some(want.clone()) && v.get("method").is_none() {
                            return Ok(v);
                        }
                    }
                }
            }
        }
        return Err(CamelError::io("the MCP event stream ended without a response"));
    }
    cameleer_core::json::parse(body).map_err(|e| CamelError::io(format!("invalid MCP response: {}", e.message)))
}

fn rpc_result(server: &str, reply: Value) -> Result<Value> {
    if let Some(e) = reply.get("error") {
        let code = e.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
        let msg = e.get("message").map(|m| m.to_string()).unwrap_or_default();
        return Err(CamelError::new(MCP_ERROR, format!("MCP server {} error {}: {}", server, code, msg)));
    }
    Ok(reply.get("result").cloned().unwrap_or(Value::Null))
}

/// Error kind of a JSON-RPC error from a remote MCP server.
pub const MCP_ERROR: &str = "org.apache.camel.component.mcp.McpException";

// ---------------------------------------------------------------------------------------------
// importing into the registry

/// The context's MCP clients, by name.
#[derive(Default)]
pub struct McpClients {
    clients: Mutex<HashMap<String, Arc<McpClient>>>,
    /// Per server: the imported tools' local names and a fingerprint of their definition and
    /// options, so a re-sync keeps unchanged tools (and their rate-limit, breaker and cache state).
    imported: Mutex<HashMap<String, Vec<(String, String)>>>,
}

impl McpClients {
    pub fn of(ctx: &CamelContext) -> Arc<McpClients> {
        static CREATE: Mutex<()> = Mutex::new(());
        let _g = CREATE.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(c) = ctx.registry().lookup::<McpClients>(CLIENTS_BEAN) {
            return c;
        }
        let c = Arc::new(McpClients::default());
        ctx.registry().bind_arc(CLIENTS_BEAN, c.clone());
        c
    }

    pub fn get(&self, name: &str) -> Option<Arc<McpClient>> {
        self.clients.lock().unwrap_or_else(|p| p.into_inner()).get(name).cloned()
    }

    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.clients.lock().unwrap_or_else(|p| p.into_inner()).keys().cloned().collect();
        v.sort();
        v
    }

    /// Connect to a server and import its tools; they are re-imported when the server's list
    /// changes. Returns how many tools were imported.
    pub fn add(self: &Arc<Self>, ctx: &CamelContext, config: McpClientConfig) -> Result<usize> {
        let name = config.name.clone();
        let client = McpClient::connect(config)?;
        if let Some(old) = self.clients.lock().unwrap_or_else(|p| p.into_inner()).insert(name.clone(), client.clone()) {
            old.close();
        }
        let n = self.import(ctx, &name)?;
        let weak_ctx = ctx.downgrade();
        let me = Arc::downgrade(self);
        let server = name.clone();
        client.on_tools_changed(move || {
            if let (Some(ctx), Some(me)) = (weak_ctx.upgrade(), me.upgrade()) {
                if let Err(e) = me.import(&ctx, &server) {
                    cameleer_core::camel_warn!("mcp-client", "Re-importing the tools of {} failed: {}", server, e.message);
                }
            }
        });
        client.watch();
        if let Some(every) = client.config.refresh_interval {
            let weak_ctx = ctx.downgrade();
            let me = Arc::downgrade(self);
            let server = name.clone();
            let closed = client.closed.clone();
            let _ = std::thread::Builder::new().name(format!("mcp-refresh-{}", name)).spawn(move || loop {
                std::thread::sleep(every);
                if closed.load(Ordering::SeqCst) {
                    return;
                }
                match (weak_ctx.upgrade(), me.upgrade()) {
                    (Some(ctx), Some(me)) => {
                        let _ = me.import(&ctx, &server);
                    }
                    _ => return,
                }
            });
        }
        Ok(n)
    }

    /// (Re-)import a server's tools into the registry. A tool whose definition changed
    /// without a new version is refused by pinning and withdrawn (rug-pull protection).
    pub fn import(&self, ctx: &CamelContext, server: &str) -> Result<usize> {
        let client = self.get(server).ok_or_else(|| CamelError::illegal_argument(format!("No MCP client named {}", server)))?;
        let reg = AiRegistry::of(ctx)?;
        let remote = client.list_tools()?;
        let old = self.imported.lock().unwrap_or_else(|p| p.into_inner()).remove(server).unwrap_or_default();
        let mut names: Vec<(String, String)> = Vec::new();
        for t in remote.into_iter().filter(|t| client.config.wants(&t.name)) {
            let local = format!("{}{}", client.config.tool_prefix, t.name);
            let mut tags = vec!["mcp".to_string(), format!("mcp:{}", server)];
            tags.extend(client.config.tags.iter().cloned());
            let def = ToolDefinition {
                name: local.clone(),
                title: t.title.clone(),
                description: t.description.clone(),
                tags,
                input_schema: t.input_schema.clone(),
                output_schema: t.output_schema.clone(),
                hints: t.annotations.clone(),
                version: "1".into(),
                searchable: true,
                return_direct: false,
            };
            // the server-wide options, then the tool's own; defaults as on ai-tool (approval for
            // destructive tools, deduplication of tools with side effects)
            let mut opts = client.config.policy.clone();
            if let Some(own) = client.config.tool_policy.get(&t.name) {
                opts.extend(own.clone());
            }
            let fingerprint = format!("{:?}{:?}", def, opts);
            if old.iter().any(|(n, f)| *n == local && *f == fingerprint) && reg.tool(&local).is_some() {
                names.push((local, fingerprint));
                continue;
            }
            reg.unregister_tool(&local);
            let mut params = cameleer_core::util::Parameters::new(opts);
            let mut policy = match crate::component::tool_policy(ctx, &mut params, &def.hints, client.config.timeout) {
                Ok(p) => p,
                Err(e) => {
                    cameleer_core::camel_warn!("mcp-client", "Tool {} of MCP server {} not imported: {}", t.name, server, e.message);
                    continue;
                }
            };
            policy.arguments_as_body = true;
            let c2 = client.clone();
            let remote_name = t.name.clone();
            let processor = Arc::new(move |ex: &mut Exchange| -> Result<()> {
                let args = match ex.body() {
                    Value::String(s) => cameleer_core::json::parse(s).unwrap_or(Value::Null),
                    v => v.clone(),
                };
                let r = c2.call_tool(&remote_name, &args)?;
                if r.is_error {
                    return Err(CamelError::illegal_state(r.text()));
                }
                if r.content.iter().any(|c| c.get("type").map(|t| t.to_string()).as_deref() != Some("text")) {
                    ex.set_header(crate::headers::CONTENT, Value::Array(r.content.clone()));
                }
                match r.structured {
                    Some(s) => ex.set_body(s),
                    None => ex.set_body(r.text()),
                }
                Ok(())
            });
            match reg.register_tool(ToolEntry::new(def, policy, processor, ctx)) {
                Ok(()) => names.push((local, fingerprint)),
                Err(e) => cameleer_core::camel_warn!("mcp-client", "Tool {} of MCP server {} not imported: {}", t.name, server, e.message),
            }
        }
        for (n, _) in &old {
            if !names.iter().any(|(k, _)| k == n) {
                reg.unregister_tool(n);
            }
        }
        let n = names.len();
        self.imported.lock().unwrap_or_else(|p| p.into_inner()).insert(server.to_string(), names);
        Ok(n)
    }

    /// Disconnect every client and withdraw their tools.
    pub fn close_all(&self, ctx: &CamelContext) {
        let reg = AiRegistry::of(ctx).ok();
        for (_, names) in self.imported.lock().unwrap_or_else(|p| p.into_inner()).drain() {
            if let Some(r) = &reg {
                for (n, _) in names {
                    r.unregister_tool(&n);
                }
            }
        }
        for (_, c) in self.clients.lock().unwrap_or_else(|p| p.into_inner()).drain() {
            c.close();
        }
    }
}

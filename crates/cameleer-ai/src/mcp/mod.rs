//! The MCP server: the context's tools, resources and prompts for MCP clients.
//!
//! Two protocol eras are served side by side, chosen per request from how the client opens
//! (as the specification defines for a dual-era server):
//!
//! - **2026-07-28** (stateless): every request carries `_meta` with
//!   `io.modelcontextprotocol/protocolVersion` and `io.modelcontextprotocol/clientCapabilities`;
//!   `server/discover`; every result has `resultType`; approvals use multi round-trip requests
//!   (`input_required` with an AES-GCM-protected `requestState`); `subscriptions/listen`;
//!   caching hints; the `Mcp-Method` / `Mcp-Name` headers over HTTP.
//! - **2025-11-25 and 2025-06-18** (sessions): `initialize`, `Mcp-Session-Id`, the GET event
//!   stream, `ping`, `resources/subscribe`, and approvals through `elicitation/create`.
//!
//! Transports: Streamable HTTP ([`http`], on the shared HTTP server of `camel.server.port`)
//! and stdio ([`stdio`]). Every tool call goes through the [`AiRegistry`] envelope.

pub mod auth;
pub mod client;
pub mod component;
pub mod http;
pub mod sse;
pub mod stdio;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cameleer_core::context::CamelContext;
use cameleer_core::error::{CamelError, Result};
use cameleer_core::value::{ObjectMap, Value};

use crate::registry::{AiRegistry, Change, CompletionRef, ToolCall, ToolResult, ToolStatus, PROMPT_NOT_FOUND, RESOURCE_NOT_FOUND};
use crate::tool::{canonical_json, sha256_hex};

/// The current protocol revision.
pub const MODERN_VERSION: &str = "2026-07-28";
/// Legacy revisions accepted in `initialize`, newest first.
pub const LEGACY_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26"];

const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_CAPS: &str = "io.modelcontextprotocol/clientCapabilities";
const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";
/// Cameleer's attestation entry in `_meta` (reverse-DNS of cameleer.ing).
pub const META_ATTESTATION: &str = "ing.cameleer/attestation";
const META_SUBSCRIPTION: &str = "io.modelcontextprotocol/subscriptionId";
/// Cameleer `_meta` keys a client may send on `tools/call`.
pub const META_TOOL_CALL_ID: &str = "io.cameleer/toolCallId";
pub const META_CONVERSATION_ID: &str = "io.cameleer/conversationId";

/// JSON-RPC and MCP error codes.
pub mod codes {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
    /// 2026-07-28 `HeaderMismatch`.
    pub const HEADER_MISMATCH: i64 = -32020;
    /// 2026-07-28 `MissingRequiredClientCapability`.
    pub const MISSING_CAPABILITY: i64 = -32021;
    /// 2026-07-28 `UnsupportedProtocolVersion`.
    pub const UNSUPPORTED_VERSION: i64 = -32022;
    /// 2025-11-25 resource not found.
    pub const LEGACY_RESOURCE_NOT_FOUND: i64 = -32002;
    /// 2025-11-25 transports: bad session (Camel / SDK convention).
    pub const SESSION_ERROR: i64 = -32000;
}

/// A JSON-RPC error with the HTTP status the Streamable HTTP transport uses for it.
#[derive(Clone, Debug, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
    pub http_status: u16,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> RpcError {
        let http_status = match code {
            codes::METHOD_NOT_FOUND => 404,
            codes::INTERNAL_ERROR => 500,
            _ => 400,
        };
        RpcError { code, message: message.into(), data: None, http_status }
    }
    pub fn with_data(mut self, data: Value) -> RpcError {
        self.data = Some(data);
        self
    }
    pub fn with_status(mut self, status: u16) -> RpcError {
        self.http_status = status;
        self
    }
    fn to_value(&self) -> Value {
        let mut e = ObjectMap::new();
        e.insert("code".into(), Value::Int(self.code));
        e.insert("message".into(), Value::from(self.message.as_str()));
        if let Some(d) = &self.data {
            e.insert("data".into(), d.clone());
        }
        Value::Object(e)
    }
}

/// A JSON-RPC response object.
pub fn response(id: &Value, result: std::result::Result<Value, RpcError>) -> Value {
    let mut m = ObjectMap::new();
    m.insert("jsonrpc".into(), Value::from("2.0"));
    m.insert("id".into(), id.clone());
    match result {
        Ok(r) => m.insert("result".into(), r),
        Err(e) => m.insert("error".into(), e.to_value()),
    };
    Value::Object(m)
}

/// A JSON-RPC notification object.
pub fn notification(method: &str, params: Value) -> Value {
    let mut m = ObjectMap::new();
    m.insert("jsonrpc".into(), Value::from("2.0"));
    m.insert("method".into(), Value::from(method));
    m.insert("params".into(), params);
    Value::Object(m)
}

// ---------------------------------------------------------------------------------------------
// configuration

/// `camel.server.mcp-*` settings.
#[derive(Clone, Debug)]
pub struct McpConfig {
    pub enabled: bool,
    /// `http` or `stdio`.
    pub transport: String,
    /// Tag patterns of what is exposed (`*`, `crm`, `crm-*`); empty exposes nothing.
    pub tags: Vec<String>,
    pub path: String,
    pub host: String,
    pub port: u16,
    pub server_name: String,
    pub server_version: String,
    pub instructions: Option<String>,
    /// `serverInfo` extras (Camel's `-server-title`, `-server-description`,
    /// `-server-website-url`, `-server-icons`).
    pub server_title: Option<String>,
    pub server_description: Option<String>,
    pub server_website_url: Option<String>,
    pub server_icons: Option<Value>,
    /// Whether `-server-name` was set (otherwise the context's name is used, as in Camel).
    pub server_name_set: bool,
    /// Send route exceptions to MCP clients as they are (default: a generic message, the cause
    /// is logged), as `camel.server.mcp-expose-errors`.
    pub expose_errors: bool,
    /// `camel.server.mcp-resource-tool`: also list a read-only tool of this name that reads any
    /// exposed resource by URI (`true` names it `read_document`). Most MCP clients only attach
    /// resources when the user picks one; the tool lets the model read them when it needs to.
    pub resource_tool: Option<String>,
    /// `camel.attestation.mcp-tool`: a tool that returns attestation evidence (when the process
    /// is attested).
    pub attestation_tool: Option<String>,
    pub keep_alive: Duration,
    pub session_idle_ttl: Duration,
    pub resource_timeout: Duration,
    pub page_size: usize,
    pub cache_ttl: Duration,
    pub allowed_origins: Vec<String>,
    /// AES-256-GCM key for `requestState` (random per process when not set).
    pub state_key: Option<Vec<u8>>,
    pub state_ttl: Duration,
    pub auth: auth::AuthConfig,
}

impl Default for McpConfig {
    fn default() -> Self {
        McpConfig {
            enabled: false,
            transport: "http".into(),
            tags: Vec::new(),
            path: "/mcp".into(),
            host: "0.0.0.0".into(),
            port: 8080,
            server_name: "cameleer".into(),
            server_title: None,
            server_description: None,
            server_website_url: None,
            server_icons: None,
            server_name_set: false,
            expose_errors: false,
            resource_tool: None,
            attestation_tool: None,
            server_version: env!("CARGO_PKG_VERSION").into(),
            instructions: None,
            keep_alive: Duration::from_secs(30),
            session_idle_ttl: Duration::from_secs(300),
            resource_timeout: Duration::from_secs(20),
            page_size: 100,
            cache_ttl: Duration::ZERO,
            allowed_origins: Vec::new(),
            state_key: None,
            state_ttl: Duration::from_secs(600),
            auth: auth::AuthConfig::default(),
        }
    }
}

impl McpConfig {
    /// Read `camel.server.mcp-*` (dashed or camelCase keys) and `camel.server.host`/`port`.
    pub fn from_context(ctx: &CamelContext) -> Result<McpConfig> {
        McpConfig::from_properties(&ctx.properties().all())
    }

    /// Read the settings from a property map.
    pub fn from_properties(props: &BTreeMap<String, String>) -> Result<McpConfig> {
        let props = props.clone();
        let get = |k: &str| prop(&props, k);
        let mut c = McpConfig::default();
        let flag = |k: &str| get(k).map(|v| v.trim().eq_ignore_ascii_case("true"));
        let dur = |k: &str, d: Duration| -> Result<Duration> {
            match get(k) {
                Some(v) => cameleer_core::util::parse_duration(v.trim()),
                None => Ok(d),
            }
        };
        c.enabled = flag("camel.server.mcp-enabled").unwrap_or(false);
        if let Some(t) = get("camel.server.mcp-transport") {
            c.transport = t.trim().to_ascii_lowercase();
            if c.transport != "http" && c.transport != "stdio" {
                return Err(CamelError::illegal_argument(format!("camel.server.mcp-transport must be http or stdio, got {}", t)));
            }
        }
        c.tags = get("camel.server.mcp-tags").map(|t| csv(&t)).unwrap_or_default();
        if let Some(p) = get("camel.server.mcp-path") {
            c.path = if p.starts_with('/') { p } else { format!("/{}", p) };
        }
        if let Some(h) = get("camel.server.host") {
            c.host = h;
        }
        if let Some(p) = get("camel.server.port") {
            c.port = p.trim().parse().map_err(|_| CamelError::illegal_argument(format!("Invalid camel.server.port: {}", p)))?;
        }
        if let Some(n) = get("camel.server.mcp-server-name") {
            c.server_name = n;
        }
        if let Some(v) = get("camel.server.mcp-server-version") {
            c.server_version = v;
        }
        c.instructions = get("camel.server.mcp-instructions");
        c.server_name_set = get("camel.server.mcp-server-name").is_some();
        c.server_title = get("camel.server.mcp-server-title");
        c.server_description = get("camel.server.mcp-server-description");
        c.server_website_url = get("camel.server.mcp-server-website-url");
        if let Some(icons) = get("camel.server.mcp-server-icons") {
            c.server_icons = Some(cameleer_core::json::parse(&icons).map_err(|e| CamelError::illegal_argument(format!("camel.server.mcp-server-icons must be a JSON array: {}", e.message)))?);
        }
        c.expose_errors = flag("camel.server.mcp-expose-errors").unwrap_or(false);
        c.resource_tool = match get("camel.server.mcp-resource-tool").map(|v| v.trim().to_string()) {
            None => None,
            Some(v) if v.is_empty() || v.eq_ignore_ascii_case("false") => None,
            Some(v) if v.eq_ignore_ascii_case("true") => Some("read_document".into()),
            Some(v) => Some(v),
        };
        c.attestation_tool = match get("camel.attestation.mcp-tool").map(|v| v.trim().to_string()) {
            None => None,
            Some(v) if v.is_empty() || v.eq_ignore_ascii_case("false") => None,
            Some(v) if v.eq_ignore_ascii_case("true") => Some("cameleer_attestation".into()),
            Some(v) => Some(v),
        };
        c.keep_alive = dur("camel.server.mcp-session-keep-alive-interval", c.keep_alive)?;
        c.session_idle_ttl = dur("camel.server.mcp-session-idle-ttl", c.session_idle_ttl)?;
        c.resource_timeout = dur("camel.server.mcp-resource-timeout", c.resource_timeout)?;
        c.cache_ttl = dur("camel.server.mcp-cache-ttl", c.cache_ttl)?;
        c.state_ttl = dur("camel.server.mcp-request-state-ttl", c.state_ttl)?;
        if let Some(n) = get("camel.server.mcp-page-size") {
            c.page_size = n.trim().parse().map_err(|_| CamelError::illegal_argument(format!("Invalid camel.server.mcp-page-size: {}", n)))?;
            c.page_size = c.page_size.max(1);
        }
        c.allowed_origins = get("camel.server.mcp-allowed-origins").map(|t| csv(&t)).unwrap_or_default();
        if let Some(k) = get("camel.server.mcp-request-state-key") {
            let key = from_hex(k.trim())
                .filter(|b| b.len() == 32)
                .ok_or_else(|| CamelError::illegal_argument("camel.server.mcp-request-state-key must be 64 hex characters (32 bytes)"))?;
            c.state_key = Some(key);
        }
        c.auth = auth::AuthConfig::from_properties(&props, &c)?;
        Ok(c)
    }

    /// Whether a tool, resource or prompt with these tags is exposed.
    /// As in Camel, `*` matches every tagged item but never untagged ones; the pattern
    /// `untagged` (a Cameleer extension) exposes items without tags.
    pub fn exposes(&self, tags: &[String]) -> bool {
        self.tags.iter().any(|pattern| {
            if tags.is_empty() {
                return pattern == "untagged";
            }
            pattern == "*" || tags.iter().any(|t| glob_match(pattern, t))
        })
    }
}

/// Look up a property by its dashed name or camelCase alias.
pub(crate) fn prop(props: &BTreeMap<String, String>, dashed: &str) -> Option<String> {
    let mut camel = String::new();
    let mut upper = false;
    for c in dashed.chars() {
        if c == '-' {
            upper = true;
        } else if upper {
            camel.extend(c.to_uppercase());
            upper = false;
        } else {
            camel.push(c);
        }
    }
    props.get(dashed).or_else(|| props.get(&camel)).cloned().filter(|v| !v.trim().is_empty())
}

pub(crate) fn csv(s: &str) -> Vec<String> {
    s.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect()
}

pub(crate) fn glob_match(pattern: &str, text: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == text,
        Some((pre, post)) => text.len() >= pre.len() + post.len() && text.starts_with(pre) && glob_match_rest(post, &text[pre.len()..]),
    }
}

fn glob_match_rest(pattern: &str, text: &str) -> bool {
    if !pattern.contains('*') {
        return text.ends_with(pattern);
    }
    let (pre, post) = pattern.split_once('*').unwrap_or((pattern, ""));
    (0..=text.len()).filter(|i| text.is_char_boundary(*i)).any(|i| text[i..].starts_with(pre) && glob_match_rest(post, &text[i + pre.len()..]))
}

// ---------------------------------------------------------------------------------------------
// request context

/// The authenticated caller.
#[derive(Clone, Debug, PartialEq)]
pub struct Principal {
    pub name: String,
    pub scopes: Vec<String>,
    /// How the caller authenticated: `api-key`, `jwt`, `introspection`, `stdio` or `none`.
    pub method: String,
}

impl Principal {
    pub fn anonymous() -> Principal {
        Principal { name: "anonymous".into(), scopes: Vec::new(), method: "none".into() }
    }
}

/// Sends server-to-client requests on the transport and waits for the answers (legacy
/// elicitation).
pub trait ClientChannel: Send + Sync {
    /// Send a request; returns the client's result, or `None` when the client did not answer
    /// in time or the transport can't carry requests.
    fn request(&self, method: &str, params: Value, timeout: Duration) -> Option<std::result::Result<Value, Value>>;
}

/// Which era a request belongs to.
#[derive(Clone, Debug, PartialEq)]
pub enum Era {
    Modern,
    Legacy { version: String },
}

/// Everything a request needs besides its message.
#[derive(Clone)]
pub struct RequestContext {
    pub era: Era,
    pub principal: Principal,
    /// The client's declared capabilities (from `_meta` or `initialize`).
    pub client_capabilities: Value,
    /// The legacy session id, if any.
    pub session_id: Option<String>,
    /// Legacy: a way to send `elicitation/create` to the client during this request.
    pub client: Option<Arc<dyn ClientChannel>>,
    /// Where notifications related to this request go (progress, logs): the request's event
    /// stream over HTTP, stdout over stdio.
    pub notify: Option<Arc<dyn Fn(&Value) + Send + Sync>>,
}

impl RequestContext {
    pub fn new(era: Era, principal: Principal, client_capabilities: Value) -> RequestContext {
        RequestContext { era, principal, client_capabilities, session_id: None, client: None, notify: None }
    }
}

/// A tool call's line back to the MCP client.
struct McpInteraction {
    modern: bool,
    progress_token: Option<Value>,
    log_level: Option<String>,
    notify: Option<Arc<dyn Fn(&Value) + Send + Sync>>,
    client: Option<Arc<dyn ClientChannel>>,
    caps: Value,
    /// 2026-07-28: the answers of a retried call (`inputResponses`).
    responses: ObjectMap,
    asked: std::sync::atomic::AtomicUsize,
    /// 2026-07-28: questions to send back as `inputRequests`.
    pending: Mutex<Vec<(String, Value)>>,
}

fn severity(level: &str) -> usize {
    ["debug", "info", "notice", "warning", "error", "critical", "alert", "emergency"].iter().position(|l| l.eq_ignore_ascii_case(level)).unwrap_or(1)
}

impl crate::registry::CallInteraction for McpInteraction {
    fn progress(&self, progress: f64, total: Option<f64>, message: Option<String>) {
        let (Some(token), Some(notify)) = (&self.progress_token, &self.notify) else { return };
        let mut p = ObjectMap::new();
        p.insert("progressToken".into(), token.clone());
        p.insert("progress".into(), Value::Float(progress));
        if let Some(t) = total {
            p.insert("total".into(), Value::Float(t));
        }
        if let Some(m) = message {
            p.insert("message".into(), Value::from(m));
        }
        notify(&notification("notifications/progress", Value::Object(p)));
    }
    fn log(&self, level: &str, logger: Option<&str>, data: Value) {
        let (Some(min), Some(notify)) = (&self.log_level, &self.notify) else { return };
        if severity(level) < severity(min) {
            return;
        }
        let mut p = ObjectMap::new();
        p.insert("level".into(), Value::from(level.to_ascii_lowercase()));
        if let Some(l) = logger {
            p.insert("logger".into(), Value::from(l));
        }
        p.insert("data".into(), data);
        notify(&notification("notifications/message", Value::Object(p)));
    }
    fn ask(&self, method: &str, params: Value) -> crate::registry::AskOutcome {
        use crate::registry::AskOutcome;
        let cap = if method == "sampling/createMessage" { "sampling" } else { "elicitation" };
        if !self.caps.as_object().map(|c| c.contains_key(cap)).unwrap_or(false) {
            return AskOutcome::Unavailable(format!("the client did not declare the {} capability", cap));
        }
        if self.modern {
            let n = self.asked.fetch_add(1, Ordering::SeqCst) + 1;
            let key = format!("input-{}", n);
            if let Some(answer) = self.responses.get(&key) {
                return AskOutcome::Answer(answer.clone());
            }
            self.pending.lock().unwrap_or_else(|p| p.into_inner()).push((key, Value::object([("method", Value::from(method)), ("params", params)])));
            return AskOutcome::Retry;
        }
        match &self.client {
            Some(c) => match c.request(method, params, Duration::from_secs(600)) {
                Some(Ok(v)) => AskOutcome::Answer(v),
                Some(Err(e)) => AskOutcome::Unavailable(format!("the client answered with an error: {}", e.to_json())),
                None => AskOutcome::Unavailable("the client did not answer".into()),
            },
            None => AskOutcome::Unavailable("this connection cannot carry requests to the client".into()),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// legacy sessions

/// A legacy (2025-11-25 / 2025-06-18) session.
pub struct Session {
    pub id: String,
    pub version: String,
    pub client_capabilities: Value,
    pub principal: Principal,
    last_used: Mutex<Instant>,
    subscriptions: Mutex<HashSet<String>>,
    stream: Mutex<Option<sse::SseStream>>,
    listener: Mutex<Option<u64>>,
    /// `logging/setLevel`: send log notifications at this level and above.
    log_level: Mutex<Option<String>>,
    /// stdio: where server notifications go.
    sink: Mutex<Option<Arc<dyn Fn(&Value) + Send + Sync>>>,
}

impl Session {
    fn touch(&self) {
        *self.last_used.lock().unwrap_or_else(|p| p.into_inner()) = Instant::now();
    }
    fn idle(&self) -> Duration {
        self.last_used.lock().unwrap_or_else(|p| p.into_inner()).elapsed()
    }
    fn deliver(&self, msg: &Value) {
        if let Some(s) = self.stream.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
            s.send(msg);
        }
        if let Some(f) = self.sink.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
            f(msg);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// the server

/// The protocol engine shared by the transports.
pub struct McpServer {
    pub config: McpConfig,
    /// `-server-name`, else the context's name.
    server_name: String,
    registry: Arc<AiRegistry>,
    state_cipher: cameleer_tls::crypto::aes::AesGcm,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    /// When idle sessions were last looked for (at most once a second, never waited for).
    last_expiry: Mutex<std::time::Instant>,
    /// Legacy server-to-client requests waiting for the client's answer, by request id.
    pending: Mutex<HashMap<String, Sender<std::result::Result<Value, Value>>>>,
    seq: AtomicU64,
    /// The process's attestation service, when it runs attested (`camel.attestation.enabled`).
    attestation: Option<Arc<dyn cameleer_core::attest::AttestationService>>,
    /// The embedded server's TLS (`camel.server.sslEnabled`): the HTTP endpoint shares its port.
    server_tls: Option<Arc<cameleer_tls::TlsServerConfig>>,
}

impl McpServer {
    pub fn new(ctx: &CamelContext, config: McpConfig) -> Result<Arc<McpServer>> {
        let registry = AiRegistry::of(ctx)?;
        let key = config.state_key.clone().unwrap_or_else(|| cameleer_tls::crypto::rng::random_bytes(32));
        if config.enabled && config.tags.is_empty() {
            cameleer_core::camel_warn!(
                "mcp",
                "The MCP server is enabled but camel.server.mcp-tags is not set, so it exposes no tools, resources or prompts; set it to * to expose all of them"
            );
        }
        let server_name = if config.server_name_set { config.server_name.clone() } else { ctx.name().to_string() };
        let server_tls = if config.transport == "http" && config.enabled { cameleer_http::platform::embedded_server_tls(ctx)? } else { None };
        Ok(Arc::new(McpServer {
            server_name,
            config,
            registry,
            state_cipher: cameleer_tls::crypto::aes::AesGcm::new(&key),
            sessions: Mutex::default(),
            last_expiry: Mutex::new(std::time::Instant::now()),
            pending: Mutex::default(),
            seq: AtomicU64::new(0),
            attestation: cameleer_core::attest::service(ctx),
            server_tls,
        }))
    }

    pub fn registry(&self) -> &Arc<AiRegistry> {
        &self.registry
    }

    /// The TLS the HTTP endpoint uses: the embedded server's, so it speaks what the port speaks.
    pub fn server_tls(&self) -> Option<Arc<cameleer_tls::TlsServerConfig>> {
        self.server_tls.clone()
    }

    fn server_info(&self) -> Value {
        let mut m = ObjectMap::new();
        m.insert("name".into(), Value::from(self.server_name.as_str()));
        m.insert("version".into(), Value::from(self.config.server_version.as_str()));
        for (k, v) in [("title", &self.config.server_title), ("description", &self.config.server_description), ("websiteUrl", &self.config.server_website_url)] {
            if let Some(v) = v {
                m.insert(k.into(), Value::from(v.as_str()));
            }
        }
        if let Some(i) = &self.config.server_icons {
            m.insert("icons".into(), i.clone());
        }
        Value::Object(m)
    }

    fn capabilities(&self) -> Value {
        let lc = || Value::object([("listChanged", Value::Bool(true))]);
        Value::object([
            ("tools", lc()),
            ("resources", Value::object([("subscribe", Value::Bool(true)), ("listChanged", Value::Bool(true))])),
            ("prompts", lc()),
            ("completions", Value::Object(ObjectMap::new())),
        ])
    }

    fn cache_scope(&self) -> &'static str {
        if self.config.auth.is_enabled() {
            "private"
        } else {
            "public"
        }
    }

    // --- era detection ---

    /// Classify a request that is not `initialize` and has no session: a 2026-07-28 request
    /// must carry valid `_meta`. Returns the declared client capabilities.
    pub fn check_modern_meta(&self, params: &ObjectMap) -> std::result::Result<Value, RpcError> {
        let meta = params.get("_meta").and_then(|m| m.as_object());
        let version = meta.and_then(|m| m.get(META_VERSION)).and_then(|v| v.as_str());
        let caps = meta.and_then(|m| m.get(META_CAPS));
        let (Some(version), Some(caps @ Value::Object(_))) = (version, caps) else {
            return Err(RpcError::new(
                codes::INVALID_PARAMS,
                format!(
                    "Missing _meta[\"{}\"] or _meta[\"{}\"]: a {} request must carry both (legacy clients send initialize first)",
                    META_VERSION, META_CAPS, MODERN_VERSION
                ),
            ));
        };
        if version != MODERN_VERSION {
            return Err(self.unsupported_version(version));
        }
        Ok(caps.clone())
    }

    pub fn unsupported_version(&self, requested: &str) -> RpcError {
        let mut supported = vec![Value::from(MODERN_VERSION)];
        supported.extend(LEGACY_VERSIONS.iter().map(|v| Value::from(*v)));
        RpcError::new(codes::UNSUPPORTED_VERSION, "Unsupported protocol version")
            .with_data(Value::object([("supported", Value::Array(supported)), ("requested", Value::from(requested))]))
    }

    // --- legacy sessions ---

    /// Handle `initialize`: negotiate a legacy version and open a session.
    pub fn initialize(&self, params: &ObjectMap, principal: Principal) -> (Arc<Session>, Value) {
        let requested = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or("");
        let version = if LEGACY_VERSIONS.contains(&requested) { requested } else { LEGACY_VERSIONS[0] };
        let id = cameleer_core::language::support::to_hex(&cameleer_tls::crypto::rng::random_bytes(16));
        let session = Arc::new(Session {
            id: id.clone(),
            version: version.to_string(),
            client_capabilities: params.get("capabilities").cloned().unwrap_or(Value::Object(ObjectMap::new())),
            principal,
            last_used: Mutex::new(Instant::now()),
            subscriptions: Mutex::default(),
            stream: Mutex::new(None),
            listener: Mutex::new(None),
            log_level: Mutex::new(None),
            sink: Mutex::new(None),
        });
        // list_changed and resources/updated go to the session's stream
        let weak = Arc::downgrade(&session);
        let lid = self.registry.subscribe(move |change| {
            if let Some(s) = weak.upgrade() {
                let msg = match change {
                    Change::Tools => notification("notifications/tools/list_changed", Value::Object(ObjectMap::new())),
                    Change::Resources => notification("notifications/resources/list_changed", Value::Object(ObjectMap::new())),
                    Change::Prompts => notification("notifications/prompts/list_changed", Value::Object(ObjectMap::new())),
                    Change::ResourceUpdated(uri) => {
                        let subscribed = s.subscriptions.lock().unwrap_or_else(|p| p.into_inner()).iter().any(|u| covers(u, uri));
                        if !subscribed {
                            return;
                        }
                        notification("notifications/resources/updated", Value::object([("uri", Value::from(uri.as_str()))]))
                    }
                };
                s.deliver(&msg);
            }
        });
        *session.listener.lock().unwrap_or_else(|p| p.into_inner()) = Some(lid);
        self.expire_sessions();
        self.sessions.lock().unwrap_or_else(|p| p.into_inner()).insert(id, session.clone());
        let mut r = ObjectMap::new();
        r.insert("protocolVersion".into(), Value::from(version));
        let mut caps = self.capabilities();
        if let Some(c) = caps.as_object_mut() {
            c.insert("logging".into(), Value::Object(ObjectMap::new()));
        }
        r.insert("capabilities".into(), caps);
        r.insert("serverInfo".into(), self.server_info());
        if let Some(i) = &self.config.instructions {
            r.insert("instructions".into(), Value::from(i.as_str()));
        }
        if let Some(a) = self.attestation_meta(params) {
            r.insert("_meta".into(), Value::object([(META_ATTESTATION, a)]));
        }
        (session, Value::Object(r))
    }

    /// A live session by id (touching it).
    pub fn session(&self, id: &str) -> Option<Arc<Session>> {
        self.expire_sessions();
        let s = self.sessions.lock().unwrap_or_else(|p| p.into_inner()).get(id).cloned();
        if let Some(s) = &s {
            s.touch();
        }
        s
    }

    /// End a session.
    pub fn close_session(&self, id: &str) -> bool {
        let s = self.sessions.lock().unwrap_or_else(|p| p.into_inner()).remove(id);
        match s {
            Some(s) => {
                self.drop_session(&s);
                true
            }
            None => false,
        }
    }

    fn drop_session(&self, s: &Session) {
        if let Some(id) = s.listener.lock().unwrap_or_else(|p| p.into_inner()).take() {
            self.registry.unsubscribe(id);
        }
        if let Some(st) = s.stream.lock().unwrap_or_else(|p| p.into_inner()).take() {
            st.close();
        }
    }

    fn expire_sessions(&self) {
        {
            let Ok(mut last) = self.last_expiry.try_lock() else { return };
            if last.elapsed() < Duration::from_secs(1) {
                return;
            }
            *last = std::time::Instant::now();
        }
        let ttl = self.config.session_idle_ttl;
        let expired: Vec<Arc<Session>> = {
            let mut map = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
            let ids: Vec<String> = map.iter().filter(|(_, s)| s.idle() > ttl && s.stream.lock().map(|x| x.is_none()).unwrap_or(true)).map(|(k, _)| k.clone()).collect();
            ids.iter().filter_map(|id| map.remove(id)).collect()
        };
        for s in expired {
            self.drop_session(&s);
        }
    }

    /// Attach the session's GET event stream (replacing an earlier one).
    pub fn attach_stream(&self, session: &Session, stream: sse::SseStream) {
        if let Some(old) = session.stream.lock().unwrap_or_else(|p| p.into_inner()).replace(stream) {
            old.close();
        }
    }

    /// stdio: deliver the session's notifications through `f`.
    pub fn attach_sink(&self, session: &Session, f: Arc<dyn Fn(&Value) + Send + Sync>) {
        *session.sink.lock().unwrap_or_else(|p| p.into_inner()) = Some(f);
    }

    // --- server-to-client requests (legacy) ---

    /// A new id for a server-to-client request, registered for its answer.
    pub fn begin_client_request(&self) -> (String, std::sync::mpsc::Receiver<std::result::Result<Value, Value>>) {
        let id = format!("cameleer-{}", self.seq.fetch_add(1, Ordering::Relaxed) + 1);
        let (tx, rx) = channel();
        self.pending.lock().unwrap_or_else(|p| p.into_inner()).insert(id_key(&Value::from(id.as_str())), tx);
        (id, rx)
    }

    pub fn end_client_request(&self, id: &str) {
        self.pending.lock().unwrap_or_else(|p| p.into_inner()).remove(&id_key(&Value::from(id)));
    }

    /// A JSON-RPC response from the client; returns false when nothing waited for it.
    pub fn deliver_client_response(&self, msg: &ObjectMap) -> bool {
        let id = msg.get("id").map(id_key).unwrap_or_default();
        let tx = self.pending.lock().unwrap_or_else(|p| p.into_inner()).remove(&id);
        match tx {
            Some(tx) => {
                let r = match (msg.get("result"), msg.get("error")) {
                    (Some(r), _) => Ok(r.clone()),
                    (_, Some(e)) => Err(e.clone()),
                    _ => Err(Value::from("empty response")),
                };
                tx.send(r).is_ok()
            }
            None => false,
        }
    }

    // --- dispatch ---

    /// Answer one request. `params` is the request's params object (empty when absent).
    pub fn dispatch(&self, method: &str, params: &ObjectMap, rc: &RequestContext) -> std::result::Result<Value, RpcError> {
        let modern = rc.era == Era::Modern;
        let result = match method {
            "server/discover" if modern => self.discover(params),
            "ping" if !modern => Ok(Value::Object(ObjectMap::new())),
            // legacy clients choose the level of the log notifications tool routes send
            "logging/setLevel" if !modern => {
                if let Some(s) = rc.session_id.as_deref().and_then(|id| self.session(id)) {
                    *s.log_level.lock().unwrap_or_else(|p| p.into_inner()) = params.get("level").map(|l| l.to_string());
                }
                Ok(Value::Object(ObjectMap::new()))
            }
            "tools/list" => self.list(params, rc, ListKind::Tools),
            "tools/call" => self.call_tool(params, rc),
            "resources/list" => self.list(params, rc, ListKind::Resources),
            "resources/templates/list" => self.list(params, rc, ListKind::Templates),
            "resources/read" => self.read_resource(params, rc),
            "resources/subscribe" | "resources/unsubscribe" if !modern => self.legacy_subscribe(method, params, rc),
            "prompts/list" => self.list(params, rc, ListKind::Prompts),
            "prompts/get" => self.get_prompt(params, rc),
            "completion/complete" => self.complete(params),
            other => Err(RpcError::new(codes::METHOD_NOT_FOUND, format!("Method not found: {}", other))),
        }?;
        Ok(if modern { self.modern_result(result) } else { result })
    }

    fn modern_result(&self, result: Value) -> Value {
        let mut m = match result {
            Value::Object(m) => m,
            other => {
                let mut m = ObjectMap::new();
                m.insert("value".into(), other);
                m
            }
        };
        if !m.contains_key("resultType") {
            m.insert("resultType".into(), Value::from("complete"));
        }
        let mut meta = m.get("_meta").and_then(|x| x.as_object()).cloned().unwrap_or_default();
        meta.insert(META_SERVER_INFO.into(), self.server_info());
        m.insert("_meta".into(), Value::Object(meta));
        Value::Object(m)
    }

    /// The attestation summary for the handshake; with
    /// `params._meta["ing.cameleer/attestation"] = {"nonce": "<base64>"}` the evidence comes
    /// inline (`report`), bound to that nonce, in the same round trip.
    fn attestation_meta(&self, params: &ObjectMap) -> Option<Value> {
        let svc = self.attestation.as_ref()?;
        let Value::Object(mut out) = svc.summary() else { return None };
        let nonce = params.get("_meta").and_then(|m| m.get(META_ATTESTATION)).and_then(|a| a.get("nonce")).map(|n| n.to_string());
        if let Some(n) = nonce {
            match decode_nonce(&n) {
                Some(bytes) => match svc.report(Some(&bytes)) {
                    Ok(r) => {
                        out.insert("report".into(), r);
                    }
                    Err(e) => {
                        out.insert("error".into(), Value::from(e.message));
                    }
                },
                None => {
                    out.insert("error".into(), Value::from("The nonce must be base64 or base64url"));
                }
            }
        }
        Some(Value::Object(out))
    }

    fn attestation_tool_definition(&self) -> Option<Value> {
        self.attestation.as_ref()?;
        let name = self.config.attestation_tool.as_ref()?;
        let schema = Value::object([
            ("type", Value::from("object")),
            (
                "properties",
                Value::object([("nonce", Value::object([("type", Value::from("string")), ("description", Value::from("16 to 64 random bytes, base64, chosen by the verifier"))]))]),
            ),
        ]);
        Some(Value::object([
            ("name", Value::from(name.as_str())),
            ("title", Value::from("Attestation evidence")),
            (
                "description",
                Value::from(
                    "Return hardware attestation evidence for this server: the TEE's signed report, bound to the given nonce, the server's TLS key and the manifest of what runs here. Showing this evidence is not verifying it: a program must check the report's signature chain and its binding.",
                ),
            ),
            ("inputSchema", schema),
            ("annotations", Value::object([("readOnlyHint", Value::Bool(true)), ("openWorldHint", Value::Bool(false))])),
        ]))
    }

    fn call_attestation_tool(&self, params: &ObjectMap) -> Value {
        let Some(svc) = self.attestation.as_ref() else {
            return tool_result(&ToolResult::error("This server is not attested"));
        };
        let nonce = params.get("arguments").and_then(|a| a.as_object()).and_then(|a| a.get("nonce")).map(|n| n.to_string());
        let bytes = match nonce.as_deref().map(decode_nonce) {
            None => None,
            Some(Some(b)) => Some(b),
            Some(None) => return tool_result(&ToolResult::error("The nonce must be base64 or base64url")),
        };
        match svc.report(bytes.as_deref()) {
            Ok(r) => tool_result(&ToolResult::ok(r.to_json(), Some(r))),
            Err(e) => tool_result(&ToolResult::error(format!("No evidence: {}", e.message))),
        }
    }

    fn cache_hints(&self, m: &mut ObjectMap) {
        m.insert("ttlMs".into(), Value::Int(self.config.cache_ttl.as_millis() as i64));
        m.insert("cacheScope".into(), Value::from(self.cache_scope()));
    }

    fn discover(&self, params: &ObjectMap) -> std::result::Result<Value, RpcError> {
        let mut m = ObjectMap::new();
        m.insert("supportedVersions".into(), Value::Array(vec![Value::from(MODERN_VERSION)]));
        m.insert("capabilities".into(), self.capabilities());
        if let Some(i) = &self.config.instructions {
            m.insert("instructions".into(), Value::from(i.as_str()));
        }
        if let Some(a) = self.attestation_meta(params) {
            m.insert("_meta".into(), Value::object([(META_ATTESTATION, a)]));
        }
        self.cache_hints(&mut m);
        Ok(Value::Object(m))
    }

    fn list(&self, params: &ObjectMap, rc: &RequestContext, kind: ListKind) -> std::result::Result<Value, RpcError> {
        let items: Vec<Value> = match kind {
            ListKind::Tools => {
                let mut tools: Vec<Value> = self.registry.tools(&[]).into_iter().filter(|t| self.config.exposes(&t.tags)).map(|t| t.to_value()).collect();
                if let Some(t) = self.resource_tool_definition() {
                    tools.push(t);
                }
                if let Some(t) = self.attestation_tool_definition() {
                    tools.push(t);
                }
                tools
            }
            ListKind::Resources | ListKind::Templates => {
                let want_templates = kind == ListKind::Templates;
                self.registry
                    .resource_entries()
                    .into_iter()
                    .filter(|r| self.config.exposes(&r.tags) && r.is_template() == want_templates)
                    .map(|r| r.to_value())
                    .collect()
            }
            ListKind::Prompts => self.registry.prompt_entries().into_iter().filter(|p| self.config.exposes(&p.tags)).map(|p| p.to_value()).collect(),
        };
        let start = match params.get("cursor") {
            None | Some(Value::Null) => 0,
            Some(Value::String(c)) => decode_cursor(c).ok_or_else(|| RpcError::new(codes::INVALID_PARAMS, "Invalid cursor"))?,
            Some(_) => return Err(RpcError::new(codes::INVALID_PARAMS, "Invalid cursor")),
        };
        let end = (start + self.config.page_size).min(items.len());
        let page: Vec<Value> = items.get(start..end).map(|s| s.to_vec()).unwrap_or_default();
        let key = match kind {
            ListKind::Tools => "tools",
            ListKind::Resources => "resources",
            ListKind::Templates => "resourceTemplates",
            ListKind::Prompts => "prompts",
        };
        let mut m = ObjectMap::new();
        m.insert(key.into(), Value::Array(page));
        if end < items.len() {
            m.insert("nextCursor".into(), Value::from(encode_cursor(end)));
        }
        if rc.era == Era::Modern {
            self.cache_hints(&mut m);
        }
        Ok(Value::Object(m))
    }

    fn interaction_for(&self, params: &ObjectMap, rc: &RequestContext, responses: ObjectMap) -> Arc<McpInteraction> {
        let meta = params.get("_meta").and_then(|m| m.as_object());
        let modern = rc.era == Era::Modern;
        let log_level = if modern {
            meta.and_then(|m| m.get("io.modelcontextprotocol/logLevel")).map(|l| l.to_string())
        } else {
            rc.session_id.as_deref().and_then(|id| self.session(id)).and_then(|s| s.log_level.lock().unwrap_or_else(|p| p.into_inner()).clone())
        };
        Arc::new(McpInteraction {
            modern,
            progress_token: meta.and_then(|m| m.get("progressToken")).filter(|t| !t.is_null()).cloned(),
            log_level,
            notify: rc.notify.clone(),
            client: rc.client.clone(),
            caps: rc.client_capabilities.clone(),
            responses,
            asked: std::sync::atomic::AtomicUsize::new(0),
            pending: Mutex::default(),
        })
    }

    fn tool_call_for(&self, params: &ObjectMap, rc: &RequestContext) -> std::result::Result<ToolCall, RpcError> {
        let name = params.get("name").and_then(|n| n.as_str()).ok_or_else(|| RpcError::new(codes::INVALID_PARAMS, "tools/call needs a name"))?;
        let def = self.registry.tool(name).filter(|d| self.config.exposes(&d.tags) && !self.registry.is_disabled(name));
        if def.is_none() {
            return Err(RpcError::new(codes::INVALID_PARAMS, format!("Unknown tool: {}", name)));
        }
        let meta = params.get("_meta").and_then(|m| m.as_object());
        let meta_str = |k: &str| meta.and_then(|m| m.get(k)).filter(|v| !v.is_null()).map(|v| v.to_string());
        let mut headers = vec![
            ("CamelMcpSecurityPrincipal".to_string(), Value::from(rc.principal.name.as_str())),
            ("CamelMcpSecurityPrincipalName".to_string(), Value::from(rc.principal.name.as_str())),
            ("CamelMcpSecurityPrincipalRoles".to_string(), Value::Array(rc.principal.scopes.iter().map(|s| Value::from(s.as_str())).collect())),
            ("CamelMcpSecurityPrincipalScopes".to_string(), Value::from(rc.principal.scopes.join(" "))),
            ("CamelMcpSecurityMethod".to_string(), Value::from(rc.principal.method.as_str())),
        ];
        for k in ["traceparent", "tracestate", "baggage"] {
            if let Some(v) = meta_str(k) {
                headers.push((k.to_string(), Value::from(v)));
            }
        }
        Ok(ToolCall {
            name: name.to_string(),
            arguments: params.get("arguments").cloned().unwrap_or(Value::Null),
            call_id: meta_str(META_TOOL_CALL_ID),
            conversation_id: meta_str(META_CONVERSATION_ID).or_else(|| rc.session_id.clone()),
            principal: Some(rc.principal.name.clone()),
            approved_by: None,
            properties: Vec::new(),
            headers,
            interaction: Default::default(),
        })
    }

    fn call_tool(&self, params: &ObjectMap, rc: &RequestContext) -> std::result::Result<Value, RpcError> {
        if let (Some(tool), Some(name)) = (&self.config.attestation_tool, params.get("name").and_then(|n| n.as_str())) {
            if tool == name && self.attestation.is_some() && !self.registry.tools(&[]).iter().any(|t| t.name == name) {
                return Ok(self.call_attestation_tool(params));
            }
        }
        if let (Some(tool), Some(name)) = (&self.config.resource_tool, params.get("name").and_then(|n| n.as_str())) {
            if tool == name && !self.registry.tools(&[]).iter().any(|t| t.name == name) {
                return Ok(self.call_resource_tool(params, rc));
            }
        }
        let mut call = self.tool_call_for(params, rc)?;
        let mut responses = ObjectMap::new();
        // 2026-07-28 retry of a call that waited for approval or for the client's answers
        if rc.era == Era::Modern {
            if let Some(state) = params.get("requestState") {
                let handle = self.open_state(state.as_str().unwrap_or(""), &call, rc).map_err(|m| RpcError::new(codes::INVALID_PARAMS, format!("Invalid requestState: {}", m)))?;
                if handle != INPUT_STATE {
                    return self.resume_approval(state, params, &call, rc);
                }
                responses = params.get("inputResponses").and_then(|r| r.as_object()).cloned().unwrap_or_default();
            }
        }
        let interaction = self.interaction_for(params, rc, responses);
        call.interaction = crate::registry::Interaction(Some(interaction.clone()));
        let result = self.registry.call(call.clone());
        let questions = std::mem::take(&mut *interaction.pending.lock().unwrap_or_else(|p| p.into_inner()));
        if !questions.is_empty() {
            // the route asked the client (2026-07-28): return the questions; the client retries
            let mut requests = ObjectMap::new();
            for (k, q) in questions {
                requests.insert(k, q);
            }
            let mut m = ObjectMap::new();
            m.insert("resultType".into(), Value::from("input_required"));
            m.insert("inputRequests".into(), Value::Object(requests));
            m.insert("requestState".into(), Value::from(self.seal_state(INPUT_STATE, &call, rc)));
            return Ok(Value::Object(m));
        }
        match &result.status {
            ToolStatus::ApprovalRequired { handle } => self.ask_approval(handle, &call, &result, rc),
            _ => Ok(self.client_result(&call.name, &result)),
        }
    }

    /// A finished call as the client sees it: an exception in the tool's route becomes a
    /// generic message with an error id (the cause stays in the log, as in Camel) unless
    /// `camel.server.mcp-expose-errors`; refusals the route reports itself are shown as they are.
    fn client_result(&self, tool: &str, result: &ToolResult) -> Value {
        if result.route_error && !self.config.expose_errors {
            let id = cameleer_core::language::support::to_hex(&cameleer_tls::crypto::rng::random_bytes(4));
            cameleer_core::camel_warn!("mcp", "Tool {} failed (error {}): {}", tool, id, result.text);
            return tool_result(&ToolResult::error(format!("Tool {} failed (error {}). The cause is in the server log.", tool, id)));
        }
        tool_result(result)
    }

    /// A call that needs approval: out of band when the tool has an approval endpoint, else
    /// from the client's user (MRTR in 2026-07-28, `elicitation/create` in legacy sessions).
    fn ask_approval(&self, handle: &str, call: &ToolCall, parked: &ToolResult, rc: &RequestContext) -> std::result::Result<Value, RpcError> {
        if self.registry.approval_endpoint(&call.name).is_some() {
            return Ok(tool_result(parked));
        }
        let elicitation = rc.client_capabilities.as_object().map(|c| c.contains_key("elicitation")).unwrap_or(false);
        if !elicitation {
            self.registry.deny(handle, "mcp-server", "the client cannot ask its user for approval");
            return Err(RpcError::new(codes::MISSING_CAPABILITY, format!("Tool {} needs approval, and the client did not declare the elicitation capability", call.name))
                .with_data(Value::object([("requiredCapabilities", Value::object([("elicitation", Value::Object(ObjectMap::new()))]))])));
        }
        let message = approval_message(call, parked);
        match &rc.era {
            Era::Modern => {
                let state = self.seal_state(handle, call, rc);
                let mut m = ObjectMap::new();
                m.insert("resultType".into(), Value::from("input_required"));
                m.insert("inputRequests".into(), Value::object([("approval", Value::object([("method", Value::from("elicitation/create")), ("params", approval_form(&message, true))]))]));
                m.insert("requestState".into(), Value::from(state));
                Ok(Value::Object(m))
            }
            Era::Legacy { version } => {
                let Some(client) = &rc.client else {
                    self.registry.deny(handle, "mcp-server", "the transport cannot ask the client");
                    return Ok(tool_result(&ToolResult::error(format!("Tool {} needs approval, and this connection cannot ask for it", call.name))));
                };
                let answer = client.request("elicitation/create", approval_form(&message, version.as_str() >= "2025-11-25"), Duration::from_secs(600));
                Ok(self.client_result(&call.name, &self.settle_approval(handle, answer.and_then(|r| r.ok()), rc)))
            }
        }
    }

    fn settle_approval(&self, handle: &str, answer: Option<Value>, rc: &RequestContext) -> ToolResult {
        let approver = format!("mcp-client:{}", rc.principal.name);
        let accepted = answer.as_ref().and_then(|a| a.as_object()).map(|a| {
            a.get("action").and_then(|x| x.as_str()) == Some("accept")
                // "Accept" is the approval; only an explicit `approve: false` in the form declines
                && !a.get("content").and_then(|c| c.as_object()).and_then(|c| c.get("approve")).is_some_and(|v| !v.is_truthy())
        });
        match accepted {
            Some(true) => self.registry.approve(handle, &approver),
            Some(false) => self.registry.deny(handle, &approver, "the user declined it in the approval form"),
            None => self.registry.deny(handle, &approver, "the user gave no answer to the approval form"),
        }
    }

    fn resume_approval(&self, state: &Value, params: &ObjectMap, call: &ToolCall, rc: &RequestContext) -> std::result::Result<Value, RpcError> {
        let handle = self.open_state(state.as_str().unwrap_or(""), call, rc).map_err(|m| RpcError::new(codes::INVALID_PARAMS, format!("Invalid requestState: {}", m)))?;
        let answer = params.get("inputResponses").and_then(|r| r.as_object()).and_then(|r| r.get("approval")).cloned();
        if answer.is_none() {
            // the client retried without answering: ask again with the same state
            let message = approval_message(call, &ToolResult::ok("", None));
            let mut m = ObjectMap::new();
            m.insert("resultType".into(), Value::from("input_required"));
            m.insert("inputRequests".into(), Value::object([("approval", Value::object([("method", Value::from("elicitation/create")), ("params", approval_form(&message, true))]))]));
            m.insert("requestState".into(), state.clone());
            return Ok(Value::Object(m));
        }
        Ok(self.client_result(&call.name, &self.settle_approval(&handle, answer, rc)))
    }

    /// `requestState`: AES-256-GCM over {handle, principal, method, tool, argument digest,
    /// expiry}, base64url(nonce || ciphertext).
    fn seal_state(&self, handle: &str, call: &ToolCall, rc: &RequestContext) -> String {
        let exp = now_ms() + self.config.state_ttl.as_millis() as i64;
        let payload = Value::object([
            ("h", Value::from(handle)),
            ("p", Value::from(rc.principal.name.as_str())),
            ("m", Value::from("tools/call")),
            ("n", Value::from(call.name.as_str())),
            ("d", Value::from(argument_digest(&call.arguments))),
            ("e", Value::Int(exp)),
        ]);
        let nonce_v = cameleer_tls::crypto::rng::random_bytes(12);
        let mut nonce = [0u8; 12];
        nonce.copy_from_slice(&nonce_v);
        let mut out = nonce.to_vec();
        out.extend(self.state_cipher.seal(&nonce, b"cameleer-mcp-request-state", payload.to_json().as_bytes()));
        base64url(&out)
    }

    fn open_state(&self, state: &str, call: &ToolCall, rc: &RequestContext) -> std::result::Result<String, &'static str> {
        let raw = base64url_decode(state).ok_or("not base64url")?;
        if raw.len() < 12 + 16 {
            return Err("too short");
        }
        let mut nonce = [0u8; 12];
        nonce.copy_from_slice(&raw[..12]);
        let plain = self.state_cipher.open(&nonce, b"cameleer-mcp-request-state", &raw[12..]).ok_or("integrity check failed")?;
        let v = cameleer_core::json::parse(&String::from_utf8_lossy(&plain)).map_err(|_| "bad payload")?;
        let m = v.as_object().ok_or("bad payload")?;
        let s = |k: &str| m.get(k).map(|x| x.to_string()).unwrap_or_default();
        if m.get("e").and_then(|e| e.as_i64()).unwrap_or(0) < now_ms() {
            return Err("expired");
        }
        if s("p") != rc.principal.name {
            return Err("issued to another principal");
        }
        if s("m") != "tools/call" || s("n") != call.name {
            return Err("issued for another request");
        }
        if s("d") != argument_digest(&call.arguments) {
            return Err("the arguments changed");
        }
        Ok(s("h"))
    }

    /// The read-only tool that reads exposed resources (`camel.server.mcp-resource-tool`); its
    /// description lists them so the model knows what it can read.
    fn resource_tool_definition(&self) -> Option<Value> {
        let name = self.config.resource_tool.as_ref()?;
        let entries: Vec<_> = self.registry.resource_entries().into_iter().filter(|r| self.config.exposes(&r.tags)).collect();
        if entries.is_empty() {
            return None;
        }
        let mut list = String::new();
        for r in &entries {
            let label = r.title.clone().unwrap_or_else(|| r.name.clone());
            let desc = r.description.as_deref().map(|d| format!(": {}", d)).unwrap_or_default();
            if r.is_template() {
                list.push_str(&format!("\n- {} (fill in the braces) - {}{}", r.uri, label, desc));
            } else {
                list.push_str(&format!("\n- {} - {}{}", r.uri, label, desc));
            }
        }
        let description = format!("Read one of the server's documents by its URI and return its content. Available:{}", list);
        let schema = Value::object([
            ("type", Value::from("object")),
            ("properties", Value::object([("uri", Value::object([("type", Value::from("string")), ("description", Value::from("The document's URI from the list"))]))])),
            ("required", Value::Array(vec![Value::from("uri")])),
        ]);
        Some(Value::object([
            ("name", Value::from(name.as_str())),
            ("title", Value::from("Read a document")),
            ("description", Value::from(description)),
            ("inputSchema", schema),
            ("annotations", Value::object([("readOnlyHint", Value::Bool(true)), ("idempotentHint", Value::Bool(true)), ("openWorldHint", Value::Bool(false))])),
        ]))
    }

    fn call_resource_tool(&self, params: &ObjectMap, rc: &RequestContext) -> Value {
        let uri = params.get("arguments").and_then(|a| a.as_object()).and_then(|a| a.get("uri")).map(|u| u.to_string()).unwrap_or_default();
        let mut p = ObjectMap::new();
        p.insert("uri".into(), Value::from(uri.as_str()));
        match self.read_resource(&p, rc) {
            Ok(v) => {
                let text: Vec<String> = v
                    .get("contents")
                    .and_then(|c| c.as_array())
                    .map(|c| c.iter().filter_map(|x| x.get("text").map(|t| t.to_string())).collect())
                    .unwrap_or_default();
                tool_result(&ToolResult::ok(text.join("\n"), None))
            }
            Err(e) => {
                let available: Vec<String> = self.registry.resource_entries().into_iter().filter(|r| self.config.exposes(&r.tags)).map(|r| r.uri.clone()).collect();
                tool_result(&ToolResult::error(format!("{}. Available documents: {}", e.message, available.join(", "))))
            }
        }
    }

    fn read_resource(&self, params: &ObjectMap, rc: &RequestContext) -> std::result::Result<Value, RpcError> {
        let uri = params.get("uri").and_then(|u| u.as_str()).ok_or_else(|| RpcError::new(codes::INVALID_PARAMS, "resources/read needs a uri"))?;
        let not_found = || {
            let code = if rc.era == Era::Modern { codes::INVALID_PARAMS } else { codes::LEGACY_RESOURCE_NOT_FOUND };
            RpcError::new(code, format!("Resource not found: {}", uri)).with_data(Value::object([("uri", Value::from(uri))]))
        };
        if !self.registry.resource_entries().iter().any(|r| self.config.exposes(&r.tags) && (r.uri == uri || (r.is_template() && crate::registry::match_template(&r.uri, uri).is_some()))) {
            return Err(not_found());
        }
        let reg = self.registry.clone();
        let u = uri.to_string();
        let content = with_timeout(self.config.resource_timeout, move || reg.read_resource(&u)).map_err(|e| {
            if e.kind == RESOURCE_NOT_FOUND {
                not_found()
            } else {
                RpcError::new(codes::INTERNAL_ERROR, format!("Reading {} failed: {}", uri, e.message))
            }
        })?;
        let mut c = ObjectMap::new();
        c.insert("uri".into(), Value::from(content.uri.as_str()));
        c.insert("mimeType".into(), Value::from(content.mime_type.as_str()));
        match (content.text, content.blob) {
            (Some(t), _) => c.insert("text".into(), Value::from(t)),
            (None, Some(b)) => c.insert("blob".into(), Value::from(cameleer_tls::pem::base64_encode(&b))),
            _ => c.insert("text".into(), Value::from("")),
        };
        let mut m = ObjectMap::new();
        m.insert("contents".into(), Value::Array(vec![Value::Object(c)]));
        if rc.era == Era::Modern {
            self.cache_hints(&mut m);
        }
        Ok(Value::Object(m))
    }

    fn legacy_subscribe(&self, method: &str, params: &ObjectMap, rc: &RequestContext) -> std::result::Result<Value, RpcError> {
        let uri = params.get("uri").and_then(|u| u.as_str()).ok_or_else(|| RpcError::new(codes::INVALID_PARAMS, format!("{} needs a uri", method)))?;
        let session = rc.session_id.as_deref().and_then(|id| self.session(id)).ok_or_else(|| RpcError::new(codes::SESSION_ERROR, "No session"))?;
        let mut subs = session.subscriptions.lock().unwrap_or_else(|p| p.into_inner());
        if method == "resources/subscribe" {
            subs.insert(uri.to_string());
        } else {
            subs.remove(uri);
        }
        Ok(Value::Object(ObjectMap::new()))
    }

    fn get_prompt(&self, params: &ObjectMap, _rc: &RequestContext) -> std::result::Result<Value, RpcError> {
        let name = params.get("name").and_then(|n| n.as_str()).ok_or_else(|| RpcError::new(codes::INVALID_PARAMS, "prompts/get needs a name"))?;
        if !self.registry.prompt_entries().iter().any(|p| p.name == name && self.config.exposes(&p.tags)) {
            return Err(RpcError::new(codes::INVALID_PARAMS, format!("Prompt not found: {}", name)));
        }
        let args: BTreeMap<String, String> = params.get("arguments").and_then(|a| a.as_object()).map(|a| a.iter().map(|(k, v)| (k.clone(), v.to_string())).collect()).unwrap_or_default();
        let reg = self.registry.clone();
        let n = name.to_string();
        let p = with_timeout(self.config.resource_timeout, move || reg.get_prompt(&n, &args)).map_err(|e| {
            if e.kind == PROMPT_NOT_FOUND || e.kind == cameleer_core::error::kinds::ILLEGAL_ARGUMENT {
                RpcError::new(codes::INVALID_PARAMS, e.message)
            } else {
                RpcError::new(codes::INTERNAL_ERROR, format!("Prompt {} failed: {}", name, e.message))
            }
        })?;
        let messages: Vec<Value> = p
            .messages
            .into_iter()
            .map(|m| {
                let content = m.content.unwrap_or_else(|| Value::object([("type", Value::from("text")), ("text", Value::from(m.text))]));
                Value::object([("role", Value::from(m.role)), ("content", content)])
            })
            .collect();
        let mut m = ObjectMap::new();
        if let Some(d) = p.description {
            m.insert("description".into(), Value::from(d));
        }
        m.insert("messages".into(), Value::Array(messages));
        Ok(Value::Object(m))
    }

    fn complete(&self, params: &ObjectMap) -> std::result::Result<Value, RpcError> {
        let r = params.get("ref").and_then(|r| r.as_object()).ok_or_else(|| RpcError::new(codes::INVALID_PARAMS, "completion/complete needs a ref"))?;
        let reference = match r.get("type").and_then(|t| t.as_str()) {
            Some("ref/prompt") => CompletionRef::Prompt(r.get("name").map(|n| n.to_string()).unwrap_or_default()),
            Some("ref/resource") => CompletionRef::Resource(r.get("uri").map(|n| n.to_string()).unwrap_or_default()),
            _ => return Err(RpcError::new(codes::INVALID_PARAMS, "ref.type must be ref/prompt or ref/resource")),
        };
        let arg = params.get("argument").and_then(|a| a.as_object());
        let name = arg.and_then(|a| a.get("name")).map(|v| v.to_string()).unwrap_or_default();
        let value = arg.and_then(|a| a.get("value")).map(|v| v.to_string()).unwrap_or_default();
        let all = self.registry.completions(&reference, &name, &value);
        let total = all.len();
        let values: Vec<Value> = all.into_iter().take(100).map(Value::from).collect();
        Ok(Value::object([(
            "completion",
            Value::object([("values", Value::Array(values)), ("total", Value::Int(total as i64)), ("hasMore", Value::Bool(total > 100))]),
        )]))
    }

    // --- subscriptions/listen (2026-07-28) ---

    /// Start a `subscriptions/listen` stream: sends the acknowledgement through `send`, then
    /// every requested change. Returns the registry listener id (pass it to
    /// [`McpServer::end_listen`]).
    pub fn listen(&self, id: &Value, params: &ObjectMap, send: Arc<dyn Fn(&Value) -> bool + Send + Sync>) -> u64 {
        let wanted = params.get("notifications").and_then(|n| n.as_object()).cloned().unwrap_or_default();
        let flag = |k: &str| wanted.get(k).map(|v| v.is_truthy()).unwrap_or(false);
        let tools = flag("toolsListChanged");
        let prompts = flag("promptsListChanged");
        let resources = flag("resourcesListChanged");
        let uris: Vec<String> = wanted.get("resourceSubscriptions").and_then(|u| u.as_array()).map(|a| a.iter().map(|v| v.to_string()).collect()).unwrap_or_default();
        let mut agreed = ObjectMap::new();
        if tools {
            agreed.insert("toolsListChanged".into(), Value::Bool(true));
        }
        if prompts {
            agreed.insert("promptsListChanged".into(), Value::Bool(true));
        }
        if resources {
            agreed.insert("resourcesListChanged".into(), Value::Bool(true));
        }
        if !uris.is_empty() {
            agreed.insert("resourceSubscriptions".into(), Value::Array(uris.iter().map(|u| Value::from(u.as_str())).collect()));
        }
        let meta = Value::object([(META_SUBSCRIPTION, id.clone())]);
        // subscribe before acknowledging: a client may act on the acknowledgement at once, and a
        // change made right after it must not be missed
        let ack = notification("notifications/subscriptions/acknowledged", Value::object([("_meta", meta.clone()), ("notifications", Value::Object(agreed))]));
        let send2 = send.clone();
        let listener = self.registry.subscribe(move |change| {
            let (method, extra) = match change {
                Change::Tools if tools => ("notifications/tools/list_changed", None),
                Change::Prompts if prompts => ("notifications/prompts/list_changed", None),
                Change::Resources if resources => ("notifications/resources/list_changed", None),
                Change::ResourceUpdated(u) if uris.iter().any(|s| covers(s, u)) => ("notifications/resources/updated", Some(u.clone())),
                _ => return,
            };
            let mut p = ObjectMap::new();
            p.insert("_meta".into(), meta.clone());
            if let Some(u) = extra {
                p.insert("uri".into(), Value::from(u));
            }
            send2(&notification(method, Value::Object(p)));
        });
        send(&ack);
        listener
    }

    pub fn end_listen(&self, listener: u64) {
        self.registry.unsubscribe(listener);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum ListKind {
    Tools,
    Resources,
    Templates,
    Prompts,
}

/// A tool result as MCP sends it.
/// A nonce in base64 or base64url, padded or not.
fn decode_nonce(s: &str) -> Option<Vec<u8>> {
    let mut t: String = s
        .trim()
        .chars()
        .map(|c| match c {
            '-' => '+',
            '_' => '/',
            c => c,
        })
        .collect();
    while t.len() % 4 != 0 {
        t.push('=');
    }
    cameleer_tls::pem::base64_decode(&t)
}

pub fn tool_result(r: &ToolResult) -> Value {
    let mut m = ObjectMap::new();
    let content = match &r.content {
        Some(blocks) => blocks.clone(),
        None => vec![Value::object([("type", Value::from("text")), ("text", Value::from(r.text.as_str()))])],
    };
    m.insert("content".into(), Value::Array(content));
    if let Some(s) = &r.structured {
        m.insert("structuredContent".into(), s.clone());
    }
    m.insert("isError".into(), Value::Bool(r.is_error));
    Value::Object(m)
}

/// The `requestState` marker of a call retried with the client's answers (not an approval).
const INPUT_STATE: &str = "input";

fn approval_message(call: &ToolCall, parked: &ToolResult) -> String {
    let args = match &call.arguments {
        Value::Null => "{}".to_string(),
        v => v.to_json(),
    };
    let reason = parked.text.split_once('(').and_then(|(_, r)| r.split_once(')')).map(|(r, _)| r.to_string()).unwrap_or_default();
    if reason.is_empty() {
        format!("Allow the tool {} to run with {}?", call.name, args)
    } else {
        format!("Allow the tool {} to run with {}? It needs approval because {}.", call.name, args, reason)
    }
}

fn approval_form(message: &str, with_mode: bool) -> Value {
    let mut p = ObjectMap::new();
    if with_mode {
        p.insert("mode".into(), Value::from("form"));
    }
    p.insert("message".into(), Value::from(message));
    p.insert(
        "requestedSchema".into(),
        Value::object([
            ("type", Value::from("object")),
            ("properties", Value::object([("approve", Value::object([("type", Value::from("boolean")), ("title", Value::from("Approve")), ("description", Value::from("Run this tool call")), ("default", Value::Bool(true))]))])),
        ]),
    );
    Value::Object(p)
}

fn argument_digest(args: &Value) -> String {
    let normalized = match args {
        Value::Null => Value::Object(ObjectMap::new()),
        v => v.clone(),
    };
    sha256_hex(canonical_json(&normalized).as_bytes())
}

/// A subscription to `subscribed` covers `uri` itself and its sub-resources.
fn covers(subscribed: &str, uri: &str) -> bool {
    uri == subscribed || (uri.starts_with(subscribed) && (subscribed.ends_with('/') || uri[subscribed.len()..].starts_with('/')))
}

fn encode_cursor(offset: usize) -> String {
    base64url(format!("o:{}", offset).as_bytes())
}

fn decode_cursor(c: &str) -> Option<usize> {
    let raw = base64url_decode(c)?;
    String::from_utf8(raw).ok()?.strip_prefix("o:")?.parse().ok()
}

pub(crate) fn base64url(data: &[u8]) -> String {
    cameleer_tls::pem::base64_encode(data).replace('+', "-").replace('/', "_").trim_end_matches('=').to_string()
}

pub(crate) fn base64url_decode(s: &str) -> Option<Vec<u8>> {
    let mut t = s.trim().replace('-', "+").replace('_', "/");
    while t.len() % 4 != 0 {
        t.push('=');
    }
    cameleer_tls::pem::base64_decode(&t)
}

pub(crate) fn id_key(id: &Value) -> String {
    match id {
        Value::String(s) => format!("s:{}", s),
        other => format!("n:{}", other.to_json()),
    }
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Workers for resource reads and prompts (started on demand).
fn read_pool() -> &'static Arc<cameleer_core::executor::ThreadPool> {
    static POOL: std::sync::OnceLock<Arc<cameleer_core::executor::ThreadPool>> = std::sync::OnceLock::new();
    POOL.get_or_init(|| cameleer_core::executor::ThreadPool::new("mcp-read", 4096))
}

fn with_timeout<T: Send + 'static>(timeout: Duration, f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    if timeout.is_zero() {
        return f();
    }
    let (tx, rx) = channel();
    if !read_pool().execute(move || {
        let _ = tx.send(f());
    }) {
        return Err(CamelError::io("Cannot start the read: the worker pool is shut down"));
    }
    rx.recv_timeout(timeout).unwrap_or_else(|_| Err(CamelError::new(cameleer_core::error::kinds::SOCKET_TIMEOUT, format!("No answer within {:?}", timeout))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match("crm-*", "crm-read"));
        assert!(!glob_match("crm-*", "erp"));
        assert!(glob_match("*-read", "crm-read"));
        assert!(glob_match("a*b*c", "axxbyyc"));
        assert!(!glob_match("a*b*c", "axxbyy"));
        assert!(glob_match("public", "public"));
    }

    #[test]
    fn cursors_and_base64url() {
        assert_eq!(decode_cursor(&encode_cursor(250)), Some(250));
        assert_eq!(decode_cursor("!!"), None);
        assert_eq!(base64url_decode(&base64url(&[0xfb, 0xff, 0x00])).unwrap(), vec![0xfb, 0xff, 0x00]);
    }

    #[test]
    fn coverage_of_resource_subscriptions() {
        assert!(covers("file:///project", "file:///project/config.json"));
        assert!(covers("file:///project/", "file:///project/a"));
        assert!(!covers("file:///project", "file:///projects/a"));
        assert!(covers("orders://42", "orders://42"));
    }
}

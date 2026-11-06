//! Streamable HTTP transport for both protocol eras, on the shared HTTP server of the port
//! (`camel.server.port`, the same server as `platform-http` routes and the management
//! endpoints).

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cameleer_core::error::Result;
use cameleer_core::value::{ObjectMap, Value};
use cameleer_http::http::{HttpRequest, HttpResponse};
use cameleer_http::registry::{registry, HttpRouteSpec, Registration, RouteHandler};
use cameleer_http::server::ServerConfig;

use super::auth::{AuthError, Authenticator};
use super::sse::SseStream;
use super::{codes, response, ClientChannel, Era, McpServer, Principal, RequestContext, RpcError};

/// A running HTTP endpoint for an [`McpServer`].
pub struct McpHttpServer {
    registrations: Mutex<Vec<Registration>>,
    port: u16,
    path: String,
}

impl McpHttpServer {
    /// Serve `server` on `camel.server.host:port` at `camel.server.mcp-path`.
    pub fn start(server: Arc<McpServer>) -> Result<McpHttpServer> {
        let cfg = server.config.clone();
        let endpoint = Arc::new(Endpoint { auth: Authenticator::new(cfg.auth.clone()), server });
        let server_config = ServerConfig { name: "camel-mcp".into(), ..Default::default() };
        let e2 = endpoint.clone();
        let handler: RouteHandler = Arc::new(move |req: &mut HttpRequest, _p: Vec<(String, String)>| e2.handle(req));
        let reg = registry().register(&cfg.host, cfg.port, &server_config, HttpRouteSpec { path: cfg.path.clone(), ..Default::default() }, handler)?;
        let port = reg.port;
        let mut regs = vec![reg];
        if cfg.auth.is_oauth() {
            for path in ["/.well-known/oauth-protected-resource".to_string(), format!("/.well-known/oauth-protected-resource{}", cfg.path)] {
                let e3 = endpoint.clone();
                let h: RouteHandler = Arc::new(move |req: &mut HttpRequest, _p: Vec<(String, String)>| {
                    if req.method != "GET" && req.method != "HEAD" {
                        return HttpResponse::new(405).with_header("Allow", "GET");
                    }
                    HttpResponse::json(200, e3.auth.config().resource_metadata().to_json()).with_header("Access-Control-Allow-Origin", "*")
                });
                regs.push(registry().register(&cfg.host, port, &server_config, HttpRouteSpec { path, ..Default::default() }, h)?);
            }
        }
        cameleer_core::camel_info!("mcp", "MCP server listening on http://{}:{}{} (protocol {} and {}, auth: {})", cfg.host, port, cfg.path, super::MODERN_VERSION, super::LEGACY_VERSIONS.join(", "), cfg.auth.mode);
        Ok(McpHttpServer { registrations: Mutex::new(regs), port, path: cfg.path })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn stop(&self) {
        for r in self.registrations.lock().unwrap_or_else(|p| p.into_inner()).drain(..) {
            registry().unregister(&r);
        }
    }
}

impl Drop for McpHttpServer {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Endpoint {
    server: Arc<McpServer>,
    auth: Authenticator,
}

fn json_rpc(status: u16, msg: &Value) -> HttpResponse {
    HttpResponse::json(status, msg.to_json())
}

fn rpc_error(id: &Value, e: RpcError) -> HttpResponse {
    let status = e.http_status;
    json_rpc(status, &response(id, Err(e)))
}

fn sse_response(handle: Value) -> HttpResponse {
    let mut r = HttpResponse::new(200);
    r.headers.set("Content-Type", "text/event-stream");
    r.headers.set("Cache-Control", "no-cache");
    r.headers.set("X-Accel-Buffering", "no");
    r.stream = Some(handle);
    r
}

/// Decode an `Mcp-Name` value (`=?base64?...?=` for values that are not plain ASCII).
fn header_value(v: &str) -> String {
    match v.strip_prefix("=?base64?").and_then(|r| r.strip_suffix("?=")) {
        Some(b64) => cameleer_tls::pem::base64_decode(b64).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default(),
        None => v.to_string(),
    }
}

impl Endpoint {
    fn handle(&self, req: &mut HttpRequest) -> HttpResponse {
        if let Some(origin) = req.headers.get("Origin") {
            if !self.origin_allowed(origin) {
                return json_rpc(403, &response(&Value::Null, Err(RpcError::new(codes::INVALID_REQUEST, format!("Origin {} is not allowed", origin)))));
            }
        }
        let principal = match self.auth.authenticate(req.headers.get("Authorization"), req.headers.get("X-API-Key")) {
            Ok(p) => p,
            Err(e) => return self.refuse(req, &e),
        };
        match req.method.as_str() {
            "POST" => self.post(req, principal),
            "GET" => self.get(req, principal),
            "DELETE" => match req.headers.get("Mcp-Session-Id") {
                Some(id) => {
                    if self.server.close_session(id) {
                        HttpResponse::new(200)
                    } else {
                        HttpResponse::new(404)
                    }
                }
                None => HttpResponse::new(405).with_header("Allow", "POST"),
            },
            _ => HttpResponse::new(405).with_header("Allow", "GET, POST, DELETE"),
        }
    }

    fn origin_allowed(&self, origin: &str) -> bool {
        let allowed = &self.server.config.allowed_origins;
        if allowed.iter().any(|a| a == "*" || a.eq_ignore_ascii_case(origin)) {
            return true;
        }
        let host = origin.split("://").nth(1).unwrap_or(origin);
        let host = host.split('/').next().unwrap_or(host);
        let host = if host.starts_with('[') { host.split(']').next().map(|h| format!("{}]", h)).unwrap_or_default() } else { host.split(':').next().unwrap_or(host).to_string() };
        matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]")
    }

    fn metadata_url(&self, req: &HttpRequest) -> String {
        let path = &self.server.config.path;
        match &self.auth.config().resource_url {
            Some(r) => {
                let (scheme, rest) = r.split_once("://").unwrap_or(("https", r));
                let host = rest.split('/').next().unwrap_or(rest);
                let rpath = &rest[host.len()..];
                format!("{}://{}/.well-known/oauth-protected-resource{}", scheme, host, if rpath.is_empty() { path.as_str() } else { rpath })
            }
            None => format!("http://{}/.well-known/oauth-protected-resource{}", req.headers.get("Host").unwrap_or("localhost"), path),
        }
    }

    fn refuse(&self, req: &HttpRequest, e: &AuthError) -> HttpResponse {
        let (status, msg) = match e {
            AuthError::Unauthorized { description, .. } => (401, description.clone()),
            AuthError::Forbidden { description } => (403, description.clone()),
        };
        json_rpc(status, &response(&Value::Null, Err(RpcError::new(codes::INVALID_REQUEST, msg)))).with_header("WWW-Authenticate", &self.auth.challenge(e, Some(&self.metadata_url(req))))
    }

    fn get(&self, req: &HttpRequest, _principal: Principal) -> HttpResponse {
        // legacy: the session's server-to-client event stream; 2026-07-28 has no GET stream
        let Some(id) = req.headers.get("Mcp-Session-Id") else {
            return HttpResponse::new(405).with_header("Allow", "POST");
        };
        let Some(session) = self.server.session(id) else {
            return HttpResponse::new(404);
        };
        let (stream, handle) = SseStream::new();
        stream.keep_alive(self.server.config.keep_alive);
        self.server.attach_stream(&session, stream);
        sse_response(handle)
    }

    fn post(&self, req: &HttpRequest, principal: Principal) -> HttpResponse {
        let msg = match cameleer_core::json::parse(&req.body_text()) {
            Ok(Value::Object(m)) => m,
            Ok(Value::Array(_)) => return rpc_error(&Value::Null, RpcError::new(codes::INVALID_REQUEST, "JSON-RPC batches are not supported")),
            Ok(_) => return rpc_error(&Value::Null, RpcError::new(codes::INVALID_REQUEST, "Not a JSON-RPC message")),
            Err(e) => return rpc_error(&Value::Null, RpcError::new(codes::PARSE_ERROR, format!("Parse error: {}", e.message))),
        };
        let Some(method) = msg.get("method").and_then(|m| m.as_str()).map(str::to_string) else {
            // a response to one of our requests (legacy elicitation)
            if msg.contains_key("result") || msg.contains_key("error") {
                self.server.deliver_client_response(&msg);
                return HttpResponse::new(202);
            }
            return rpc_error(&Value::Null, RpcError::new(codes::INVALID_REQUEST, "Not a JSON-RPC message"));
        };
        let id = msg.get("id").cloned();
        let params = msg.get("params").and_then(|p| p.as_object()).cloned().unwrap_or_default();

        if method == "initialize" {
            let Some(id) = id else { return HttpResponse::new(400) };
            let (session, result) = self.server.initialize(&params, principal);
            return json_rpc(200, &response(&id, Ok(result))).with_header("Mcp-Session-Id", &session.id).with_header("MCP-Protocol-Version", &session.version);
        }
        if let Some(sid) = req.headers.get("Mcp-Session-Id") {
            return self.legacy(req, sid, &method, id, params, principal);
        }
        self.modern(req, &method, id, params, principal)
    }

    fn legacy(&self, req: &HttpRequest, sid: &str, method: &str, id: Option<Value>, params: ObjectMap, principal: Principal) -> HttpResponse {
        let Some(session) = self.server.session(sid) else {
            return rpc_error(&id.unwrap_or(Value::Null), RpcError::new(codes::SESSION_ERROR, "Session not found").with_status(404));
        };
        if session.principal.name != principal.name {
            return rpc_error(&id.unwrap_or(Value::Null), RpcError::new(codes::SESSION_ERROR, "The session belongs to another principal").with_status(403));
        }
        if let Some(v) = req.headers.get("MCP-Protocol-Version") {
            if !super::LEGACY_VERSIONS.contains(&v) {
                return rpc_error(&id.unwrap_or(Value::Null), RpcError::new(codes::INVALID_REQUEST, format!("Unsupported MCP-Protocol-Version: {}", v)));
            }
        }
        let Some(id) = id else {
            return HttpResponse::new(202); // notifications/initialized, notifications/cancelled, ...
        };
        let rc = RequestContext {
            era: Era::Legacy { version: session.version.clone() },
            principal,
            client_capabilities: session.client_capabilities.clone(),
            session_id: Some(session.id.clone()),
            client: None,
            notify: None,
        };
        let wants_sse = req.headers.get("Accept").map(|a| a.contains("text/event-stream")).unwrap_or(false);
        if method == "tools/call" && wants_sse {
            return self.call_with_channel(id, params, rc);
        }
        // legacy JSON-RPC errors travel in a 200 response, as the 2025 SDKs do
        json_rpc(200, &response(&id, self.server.dispatch(method, &params, &rc)))
    }

    /// A `tools/call` that may talk back to the client (progress and log notifications, and in
    /// legacy sessions `elicitation/create` or `sampling/createMessage`): the call runs on a
    /// worker; on its first message to the client the response becomes an event stream that
    /// carries the messages and then the result, otherwise it is plain JSON.
    fn call_with_channel(&self, id: Value, params: ObjectMap, mut rc: RequestContext) -> HttpResponse {
        let (tx, rx) = channel::<Event>();
        if rc.era != Era::Modern {
            let chan = Arc::new(HttpClientChannel { server: self.server.clone(), events: Mutex::new(tx.clone()) });
            rc.client = Some(chan);
        }
        let ntx = Mutex::new(tx.clone());
        rc.notify = Some(Arc::new(move |m: &Value| {
            let _ = ntx.lock().unwrap_or_else(|p| p.into_inner()).send(Event::Request(m.clone()));
        }));
        let server = self.server.clone();
        let id2 = id.clone();
        // a pooled worker (no thread start per call): the call may stream events, and in a
        // legacy session it may wait for the client's answer to an elicitation or sampling request
        let spawned = call_pool().execute(move || {
            let r = server.dispatch("tools/call", &params, &rc);
            let _ = tx.send(Event::Final(response(&id2, r)));
        });
        if !spawned {
            return rpc_error(&id, RpcError::new(codes::INTERNAL_ERROR, "Cannot start the call"));
        }
        match rx.recv() {
            Ok(Event::Final(msg)) => json_rpc(200, &msg),
            Ok(Event::Request(first)) => {
                let (stream, handle) = SseStream::new();
                stream.keep_alive(self.server.config.keep_alive);
                stream.send(&first);
                forward(rx, stream);
                sse_response(handle)
            }
            Err(_) => rpc_error(&id, RpcError::new(codes::INTERNAL_ERROR, "The call ended without a result")),
        }
    }

    fn modern(&self, req: &HttpRequest, method: &str, id: Option<Value>, params: ObjectMap, principal: Principal) -> HttpResponse {
        let Some(id) = id else {
            // no client notifications are defined over HTTP in 2026-07-28
            return HttpResponse::new(202);
        };
        let caps = match self.server.check_modern_meta(&params) {
            Ok(c) => c,
            Err(e) => return rpc_error(&id, e),
        };
        // the headers must mirror the body
        let mismatch = |what: &str| rpc_error(&id, RpcError::new(codes::HEADER_MISMATCH, format!("Header mismatch: {}", what)));
        match req.headers.get("MCP-Protocol-Version") {
            Some(v) if v == super::MODERN_VERSION => {}
            Some(_) => return mismatch("MCP-Protocol-Version does not match _meta"),
            None => return mismatch("MCP-Protocol-Version is required"),
        }
        match req.headers.get("Mcp-Method") {
            Some(m) if m == method => {}
            Some(_) => return mismatch("Mcp-Method does not match the method"),
            None => return mismatch("Mcp-Method is required"),
        }
        let named = match method {
            "tools/call" | "prompts/get" => params.get("name").map(|v| v.to_string()),
            "resources/read" => params.get("uri").map(|v| v.to_string()),
            _ => None,
        };
        if let Some(name) = named {
            match req.headers.get("Mcp-Name").map(header_value) {
                Some(h) if h == name => {}
                Some(_) => return mismatch("Mcp-Name does not match the request"),
                None => return mismatch("Mcp-Name is required"),
            }
        }
        let rc = RequestContext::new(Era::Modern, principal, caps);
        if method == "subscriptions/listen" {
            return self.listen(id, params);
        }
        // a call that wants progress or log notifications gets an event stream
        let meta = params.get("_meta").and_then(|m| m.as_object());
        let wants_events = meta.map(|m| m.get("progressToken").is_some() || m.get("io.modelcontextprotocol/logLevel").is_some()).unwrap_or(false);
        let accepts_sse = req.headers.get("Accept").map(|a| a.contains("text/event-stream")).unwrap_or(false);
        if method == "tools/call" && wants_events && accepts_sse {
            return self.call_with_channel(id, params, rc);
        }
        match self.server.dispatch(method, &params, &rc) {
            Ok(r) => json_rpc(200, &response(&id, Ok(r))),
            Err(e) => rpc_error(&id, e),
        }
    }

    fn listen(&self, id: Value, params: ObjectMap) -> HttpResponse {
        let (stream, handle) = SseStream::new();
        stream.keep_alive(self.server.config.keep_alive);
        let s2 = stream.clone();
        let listener = self.server.listen(&id, &params, Arc::new(move |m: &Value| s2.send(m)));
        // drop the listener once the client goes away
        let server = self.server.clone();
        let watch = stream.clone();
        let _ = std::thread::Builder::new().name("mcp-listen".into()).spawn(move || {
            while watch.is_open() {
                std::thread::sleep(Duration::from_millis(250));
            }
            server.end_listen(listener);
        });
        sse_response(handle)
    }
}

enum Event {
    Request(Value),
    Final(Value),
}

fn forward(rx: Receiver<Event>, stream: SseStream) {
    call_pool().execute(move || {
        while let Ok(ev) = rx.recv() {
            match ev {
                Event::Request(m) => {
                    stream.send(&m);
                }
                Event::Final(m) => {
                    stream.send(&m);
                    break;
                }
            }
        }
        stream.close();
    });
}

struct HttpClientChannel {
    server: Arc<McpServer>,
    events: Mutex<Sender<Event>>,
}

impl ClientChannel for HttpClientChannel {
    fn request(&self, method: &str, params: Value, timeout: Duration) -> Option<std::result::Result<Value, Value>> {
        let (id, rx) = self.server.begin_client_request();
        let msg = Value::object([("jsonrpc", Value::from("2.0")), ("id", Value::from(id.as_str())), ("method", Value::from(method)), ("params", params)]);
        let sent = self.events.lock().unwrap_or_else(|p| p.into_inner()).send(Event::Request(msg)).is_ok();
        let answer = if sent { rx.recv_timeout(timeout).ok() } else { None };
        self.server.end_client_request(&id);
        answer
    }
}

/// Workers for calls that stream events (started on demand, shared by every MCP server).
fn call_pool() -> &'static Arc<cameleer_core::executor::ThreadPool> {
    static POOL: std::sync::OnceLock<Arc<cameleer_core::executor::ThreadPool>> = std::sync::OnceLock::new();
    POOL.get_or_init(|| cameleer_core::executor::ThreadPool::new("mcp-call", 4096))
}

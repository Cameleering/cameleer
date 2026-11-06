//! stdio transport: newline-delimited JSON-RPC on stdin and stdout (logs go to stderr).
//!
//! The era follows the first message: `initialize` opens one legacy session for the life of
//! the process; anything else is answered statelessly under 2026-07-28. Requests run on their
//! own threads, so a long tool call doesn't hold up the others or the client's answers to
//! `elicitation/create`.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cameleer_core::value::Value;

use super::{codes, id_key, response, ClientChannel, Era, McpServer, Principal, RequestContext, RpcError, Session};

type Out = Arc<Mutex<Box<dyn Write + Send>>>;

fn write(out: &Out, msg: &Value) -> bool {
    let mut w = out.lock().unwrap_or_else(|p| p.into_inner());
    let line = format!("{}\n", msg.to_json());
    w.write_all(line.as_bytes()).and_then(|_| w.flush()).is_ok()
}

fn principal() -> Principal {
    Principal { name: "stdio".into(), scopes: Vec::new(), method: "stdio".into() }
}

struct StdioClientChannel {
    server: Arc<McpServer>,
    out: Out,
}

impl ClientChannel for StdioClientChannel {
    fn request(&self, method: &str, params: Value, timeout: Duration) -> Option<std::result::Result<Value, Value>> {
        let (id, rx) = self.server.begin_client_request();
        let msg = Value::object([("jsonrpc", Value::from("2.0")), ("id", Value::from(id.as_str())), ("method", Value::from(method)), ("params", params)]);
        let answer = if write(&self.out, &msg) { rx.recv_timeout(timeout).ok() } else { None };
        self.server.end_client_request(&id);
        answer
    }
}

/// Serve MCP on `input` / `output` until `input` ends.
pub fn serve(server: Arc<McpServer>, input: impl BufRead, output: impl Write + Send + 'static) {
    let out: Out = Arc::new(Mutex::new(Box::new(output)));
    let mut session: Option<Arc<Session>> = None;
    let listens: Arc<Mutex<HashMap<String, u64>>> = Arc::default();
    let mut workers = Vec::new();
    for line in input.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let msg = match cameleer_core::json::parse(&line) {
            Ok(Value::Object(m)) => m,
            Ok(_) => {
                write(&out, &response(&Value::Null, Err(RpcError::new(codes::INVALID_REQUEST, "Not a JSON-RPC message"))));
                continue;
            }
            Err(e) => {
                write(&out, &response(&Value::Null, Err(RpcError::new(codes::PARSE_ERROR, format!("Parse error: {}", e.message)))));
                continue;
            }
        };
        let Some(method) = msg.get("method").and_then(|m| m.as_str()).map(str::to_string) else {
            if msg.contains_key("result") || msg.contains_key("error") {
                server.deliver_client_response(&msg);
            }
            continue;
        };
        let params = msg.get("params").and_then(|p| p.as_object()).cloned().unwrap_or_default();
        let Some(id) = msg.get("id").cloned() else {
            // notifications
            if method == "notifications/cancelled" {
                if let Some(rid) = params.get("requestId") {
                    if let Some(l) = listens.lock().unwrap_or_else(|p| p.into_inner()).remove(&id_key(rid)) {
                        server.end_listen(l);
                    }
                }
            }
            continue;
        };
        if method == "initialize" {
            let (s, result) = server.initialize(&params, principal());
            let o = out.clone();
            server.attach_sink(&s, Arc::new(move |m: &Value| {
                write(&o, m);
            }));
            session = Some(s);
            write(&out, &response(&id, Ok(result)));
            continue;
        }
        let rc = match &session {
            Some(s) => RequestContext {
                era: Era::Legacy { version: s.version.clone() },
                principal: principal(),
                client_capabilities: s.client_capabilities.clone(),
                session_id: Some(s.id.clone()),
                client: Some(Arc::new(StdioClientChannel { server: server.clone(), out: out.clone() })),
            },
            None => match server.check_modern_meta(&params) {
                Ok(caps) => RequestContext { era: Era::Modern, principal: principal(), client_capabilities: caps, session_id: None, client: None },
                Err(e) => {
                    write(&out, &response(&id, Err(e)));
                    continue;
                }
            },
        };
        if method == "subscriptions/listen" && rc.era == Era::Modern {
            let o = out.clone();
            let l = server.listen(&id, &params, Arc::new(move |m: &Value| write(&o, m)));
            listens.lock().unwrap_or_else(|p| p.into_inner()).insert(id_key(&id), l);
            continue;
        }
        let server2 = server.clone();
        let o = out.clone();
        let handle = std::thread::Builder::new().name("mcp-stdio-request".into()).spawn(move || {
            let r = server2.dispatch(&method, &params, &rc);
            write(&o, &response(&id, r));
        });
        if let Ok(h) = handle {
            workers.push(h);
        }
        workers.retain(|h| !h.is_finished());
    }
    for h in workers {
        let _ = h.join();
    }
    for (_, l) in listens.lock().unwrap_or_else(|p| p.into_inner()).drain() {
        server.end_listen(l);
    }
    if let Some(s) = session {
        server.close_session(&s.id);
    }
}

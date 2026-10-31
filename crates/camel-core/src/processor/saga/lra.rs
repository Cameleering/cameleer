//! LRA saga service (Camel `camel-lra`): sagas are Long Running Actions managed by a
//! Narayana-LRA compatible coordinator reached over HTTP (plain `http://` only).
//!
//! * new saga: `POST {coordinator}{coordinatorContextPath}/start?TimeLimit=0` - the reply
//!   (body or `Location` header) is the LRA URL, used as the saga id
//!   (`Long-Running-Action` header);
//! * step: `PUT {lra}?TimeLimit={ms}` with a `Link` header (and body) naming the
//!   participant callbacks `<{participant}/compensate?...>; rel="compensate"` and
//!   `<{participant}/complete?...>; rel="complete"`. The callback urls carry the target
//!   endpoint (`Camel-Saga-Compensate` / `Camel-Saga-Complete`) and the step options;
//! * complete: `PUT {lra}/close`; compensate: `PUT {lra}/cancel`.
//!
//! The service runs a small HTTP server on `localParticipantUrl` (port `0` picks a free
//! port) that receives the coordinator's `PUT .../compensate` and `PUT .../complete`
//! calls and forwards them to the step endpoints with the options and
//! `Long-Running-Action` as headers.
//!
//! [`testing::MockLraCoordinator`] is an embedded coordinator for tests.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::context::{CamelContext, WeakContext};
use crate::error::{kinds, CamelError, Result};
use crate::message::Exchange;
use crate::spi::Service;
use crate::util::{parse_query, url_encode};
use crate::value::Value;

use super::{send_finalization, CamelSagaCoordinator, CamelSagaService, CamelSagaStep, SAGA_LONG_RUNNING_ACTION};

/// Query parameter naming the compensation endpoint in participant urls.
pub const URL_COMPENSATION_KEY: &str = "Camel-Saga-Compensate";
/// Query parameter naming the completion endpoint in participant urls.
pub const URL_COMPLETION_KEY: &str = "Camel-Saga-Complete";

// ---------------------------------------------------------------------------
// minimal HTTP/1.1 helpers (Connection: close)
// ---------------------------------------------------------------------------

/// A parsed HTTP request or response.
#[derive(Debug, Default, Clone)]
pub struct HttpMessage {
    /// Request method (requests) or status code text (responses).
    pub method: String,
    /// Request path without the query.
    pub path: String,
    pub query: BTreeMap<String, String>,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl HttpMessage {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

fn io_err(e: std::io::Error) -> CamelError {
    CamelError::io(e.to_string())
}

fn read_message(stream: &TcpStream, request: bool) -> Result<HttpMessage> {
    let mut r = BufReader::new(stream);
    let mut line = String::new();
    r.read_line(&mut line).map_err(io_err)?;
    let first = line.trim_end().to_string();
    if first.is_empty() {
        return Err(CamelError::io("empty HTTP message"));
    }
    let mut m = HttpMessage::default();
    let mut parts = first.splitn(3, ' ');
    if request {
        m.method = parts.next().unwrap_or("").to_string();
        let target = parts.next().unwrap_or("/");
        let (p, q) = target.split_once('?').unwrap_or((target, ""));
        m.path = p.to_string();
        m.query = parse_query(q);
    } else {
        let _version = parts.next();
        m.status = parts.next().and_then(|s| s.parse().ok()).ok_or_else(|| CamelError::io(format!("bad status line: {}", first)))?;
    }
    loop {
        line.clear();
        if r.read_line(&mut line).map_err(io_err)? == 0 {
            break;
        }
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            m.headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let len = m.header("Content-Length").and_then(|v| v.parse::<usize>().ok());
    let mut body = Vec::new();
    match len {
        Some(n) => {
            body.resize(n, 0);
            r.read_exact(&mut body).map_err(io_err)?;
        }
        None if !request => {
            r.read_to_end(&mut body).map_err(io_err)?;
        }
        None => {}
    }
    m.body = String::from_utf8_lossy(&body).into_owned();
    Ok(m)
}

fn write_response(mut stream: &TcpStream, status: u16, headers: &[(&str, String)], body: &str) {
    let reason = match status {
        200 => "OK",
        201 => "Created",
        404 => "Not Found",
        409 => "Conflict",
        _ => "Error",
    };
    let mut out = format!("HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n", status, reason, body.len());
    for (k, v) in headers {
        out.push_str(&format!("{}: {}\r\n", k, v));
    }
    out.push_str("\r\n");
    out.push_str(body);
    let _ = stream.write_all(out.as_bytes());
    let _ = stream.flush();
}

/// Split `http://host:port/path?query` into (`host:port`, `/path?query`).
fn split_url(url: &str) -> Result<(String, String)> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| CamelError::illegal_argument(format!("Only http:// urls are supported by the LRA client: {}", url)))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let authority = if authority.contains(':') { authority.to_string() } else { format!("{}:80", authority) };
    Ok((authority, path.to_string()))
}

/// Send an HTTP request and read the response (10 s timeouts).
pub fn http_request(method: &str, url: &str, headers: &[(&str, String)], body: &str) -> Result<HttpMessage> {
    let (authority, path) = split_url(url)?;
    let stream = TcpStream::connect(&authority).map_err(|e| CamelError::new(kinds::CONNECT, format!("Cannot connect to {}: {}", authority, e)))?;
    stream.set_read_timeout(Some(Duration::from_secs(10))).map_err(io_err)?;
    stream.set_write_timeout(Some(Duration::from_secs(10))).map_err(io_err)?;
    let mut req = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        method,
        path,
        authority,
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{}: {}\r\n", k, v));
    }
    req.push_str("\r\n");
    req.push_str(body);
    (&stream).write_all(req.as_bytes()).map_err(io_err)?;
    read_message(&stream, false)
}

/// A tiny threaded HTTP server: each request is handled by `handler` on its own thread.
struct MiniServer {
    port: u16,
    stopped: Arc<AtomicBool>,
}

impl MiniServer {
    fn start(bind: &str, handler: Arc<dyn Fn(&HttpMessage) -> (u16, Vec<(&'static str, String)>, String) + Send + Sync>) -> Result<MiniServer> {
        let listener = TcpListener::bind(bind).map_err(|e| CamelError::io(format!("Cannot bind {}: {}", bind, e)))?;
        let port = listener.local_addr().map_err(io_err)?.port();
        let stopped = Arc::new(AtomicBool::new(false));
        let flag = stopped.clone();
        std::thread::Builder::new()
            .name(format!("lra-http-{}", port))
            .spawn(move || {
                for conn in listener.incoming() {
                    if flag.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(stream) = conn else { continue };
                    let h = handler.clone();
                    std::thread::spawn(move || {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                        if let Ok(req) = read_message(&stream, true) {
                            let (status, headers, body) = h(&req);
                            write_response(&stream, status, &headers, &body);
                        }
                        let _ = stream.shutdown(Shutdown::Both);
                    });
                }
            })
            .map_err(io_err)?;
        Ok(MiniServer { port, stopped })
    }

    fn stop(&self) {
        if !self.stopped.swap(true, Ordering::SeqCst) {
            let _ = TcpStream::connect(("127.0.0.1", self.port));
        }
    }
}

impl Drop for MiniServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Parse a `Link` header into (rel, url) pairs.
pub fn parse_link_header(link: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = link;
    while let Some(start) = rest.find('<') {
        let Some(end) = rest[start..].find('>') else { break };
        let url = rest[start + 1..start + end].to_string();
        rest = &rest[start + end + 1..];
        let next = rest.find('<').unwrap_or(rest.len());
        let params = &rest[..next];
        let rel = params
            .split(';')
            .filter_map(|p| p.trim().strip_prefix("rel="))
            .map(|r| r.trim_matches(|c| c == '"' || c == ',' || c == ' ').to_string())
            .next()
            .unwrap_or_default();
        out.push((rel, url));
        rest = &rest[next..];
    }
    out
}

// ---------------------------------------------------------------------------
// service
// ---------------------------------------------------------------------------

struct LraInner {
    ctx: WeakContext,
    coordinator_url: String,
    coordinator_context_path: String,
    local_participant_url: Mutex<String>,
    local_participant_context_path: String,
    server: Mutex<Option<MiniServer>>,
}

/// Saga service backed by an LRA coordinator. Cheap to clone.
#[derive(Clone)]
pub struct LraSagaService {
    inner: Arc<LraInner>,
}

impl LraSagaService {
    /// `coordinator_url` e.g. `http://localhost:8080`; `local_participant_url` is where the
    /// coordinator reaches this application, e.g. `http://localhost:8081` (port 0 = any).
    pub fn new(ctx: &CamelContext, coordinator_url: impl Into<String>, local_participant_url: impl Into<String>) -> Self {
        LraSagaService {
            inner: Arc::new(LraInner {
                ctx: ctx.downgrade(),
                coordinator_url: coordinator_url.into().trim_end_matches('/').to_string(),
                coordinator_context_path: "/lra-coordinator".into(),
                local_participant_url: Mutex::new(local_participant_url.into().trim_end_matches('/').to_string()),
                local_participant_context_path: "/lra-participant".into(),
                server: Mutex::new(None),
            }),
        }
    }

    fn with_inner(self, f: impl FnOnce(&mut LraInner)) -> Self {
        let mut inner = Arc::try_unwrap(self.inner).unwrap_or_else(|_| panic!("configure the LraSagaService before sharing it"));
        f(&mut inner);
        LraSagaService { inner: Arc::new(inner) }
    }

    /// Coordinator context path (default `/lra-coordinator`).
    pub fn with_coordinator_context_path(self, p: impl Into<String>) -> Self {
        let p = p.into();
        self.with_inner(|i| i.coordinator_context_path = p)
    }

    /// Participant context path (default `/lra-participant`).
    pub fn with_local_participant_context_path(self, p: impl Into<String>) -> Self {
        let p = p.into();
        self.with_inner(|i| i.local_participant_context_path = p)
    }

    /// The participant base url (with the actual port once started).
    pub fn participant_url(&self) -> String {
        format!("{}{}", self.inner.local_participant_url.lock().unwrap(), self.inner.local_participant_context_path)
    }

    fn base(&self) -> String {
        format!("{}{}", self.inner.coordinator_url, self.inner.coordinator_context_path)
    }

    fn ensure_started(&self) -> Result<()> {
        let mut server = self.inner.server.lock().unwrap();
        if server.is_some() {
            return Ok(());
        }
        let url = self.inner.local_participant_url.lock().unwrap().clone();
        let (authority, _) = split_url(&url)?;
        let (host, port) = authority.rsplit_once(':').unwrap_or((authority.as_str(), "80"));
        let bind_host = if host == "localhost" { "127.0.0.1" } else { host };
        let bind = format!("{}:{}", bind_host, port);
        let inner = Arc::downgrade(&self.inner);
        let s = MiniServer::start(
            &bind,
            Arc::new(move |req: &HttpMessage| match inner.upgrade() {
                Some(i) => handle_participant_call(&i, req),
                None => (500, vec![], "service stopped".into()),
            }),
        )?;
        if port == "0" {
            *self.inner.local_participant_url.lock().unwrap() = format!("http://{}:{}", host, s.port);
        }
        *server = Some(s);
        Ok(())
    }

    fn coordinator(&self, url: String) -> Arc<dyn CamelSagaCoordinator> {
        Arc::new(LraSagaCoordinator { url, service: self.clone() })
    }
}

fn handle_participant_call(inner: &LraInner, req: &HttpMessage) -> (u16, Vec<(&'static str, String)>, String) {
    let base = &inner.local_participant_context_path;
    let action = req.path.strip_prefix(base.as_str()).unwrap_or(&req.path).trim_matches('/');
    let key = match action {
        "compensate" => URL_COMPENSATION_KEY,
        "complete" => URL_COMPLETION_KEY,
        _ => return (404, vec![], format!("unknown participant action {}", req.path)),
    };
    let mut values: BTreeMap<String, Value> = BTreeMap::new();
    let mut target = None;
    for (k, v) in &req.query {
        if k == key {
            target = Some(v.clone());
        } else if k != URL_COMPENSATION_KEY && k != URL_COMPLETION_KEY {
            values.insert(k.clone(), Value::String(v.clone()));
        }
    }
    let lra = req.header(SAGA_LONG_RUNNING_ACTION).map(String::from).unwrap_or_default();
    let Some(uri) = target else { return (200, vec![], String::new()) };
    let Some(ctx) = inner.ctx.upgrade() else { return (500, vec![], "no context".into()) };
    match send_finalization(&ctx, &uri, &lra, &values) {
        Ok(()) => (200, vec![], String::new()),
        Err(e) => (500, vec![], e.message),
    }
}

impl Service for LraSagaService {
    fn start(&self) -> Result<()> {
        self.ensure_started()
    }
    fn stop(&self) -> Result<()> {
        if let Some(s) = self.inner.server.lock().unwrap().take() {
            s.stop();
        }
        Ok(())
    }
}

impl CamelSagaService for LraSagaService {
    fn new_saga(&self, _exchange: &Exchange) -> Result<Arc<dyn CamelSagaCoordinator>> {
        self.ensure_started()?;
        let r = http_request("POST", &format!("{}/start?TimeLimit=0", self.base()), &[], "")?;
        if r.status != 200 && r.status != 201 {
            return Err(CamelError::io(format!("Cannot obtain LRA id from LRA coordinator due to response status code {}", r.status)));
        }
        let url = match r.header("Location") {
            Some(l) if !l.is_empty() => l.to_string(),
            _ => r.body.trim().to_string(),
        };
        if url.is_empty() {
            return Err(CamelError::io("The LRA coordinator returned no LRA id"));
        }
        Ok(self.coordinator(url))
    }

    fn get_saga(&self, id: &str) -> Result<Option<Arc<dyn CamelSagaCoordinator>>> {
        Ok(Some(self.coordinator(id.to_string())))
    }

    fn register_step(&self, _step: &CamelSagaStep) -> Result<()> {
        self.ensure_started()
    }
}

/// A saga managed by the LRA coordinator.
pub struct LraSagaCoordinator {
    url: String,
    service: LraSagaService,
}

impl LraSagaCoordinator {
    fn participant_link(&self, action: &str, key: &str, uri: &str, values: &BTreeMap<String, Value>) -> String {
        let mut link = format!("{}/{}?{}={}", self.service.participant_url(), action, key, url_encode(uri));
        for (k, v) in values {
            link.push_str(&format!("&{}={}", url_encode(k), url_encode(&v.to_string())));
        }
        link
    }

    fn put(&self, url: &str, what: &str) -> Result<()> {
        let r = http_request("PUT", url, &[], "")?;
        if r.status != 200 && r.status != 202 {
            return Err(CamelError::io(format!("Cannot {} LRA {}: coordinator returned status {}", what, self.url, r.status)));
        }
        Ok(())
    }
}

impl CamelSagaCoordinator for LraSagaCoordinator {
    fn id(&self) -> String {
        self.url.clone()
    }

    fn begin_step(&self, exchange: &Exchange, step: &CamelSagaStep) -> Result<()> {
        if step.compensation.is_none() && step.completion.is_none() && step.timeout.is_none() {
            return Ok(());
        }
        let values = step.evaluate_options(exchange)?;
        let mut links = Vec::new();
        if let Some(c) = &step.compensation {
            links.push(format!("<{}>; rel=\"compensate\"", self.participant_link("compensate", URL_COMPENSATION_KEY, c, &values)));
        }
        if let Some(c) = &step.completion {
            links.push(format!("<{}>; rel=\"complete\"", self.participant_link("complete", URL_COMPLETION_KEY, c, &values)));
        }
        let link = links.join(", ");
        let time_limit = step.timeout.map(|t| t.as_millis()).unwrap_or(0);
        let r = http_request("PUT", &format!("{}?TimeLimit={}", self.url, time_limit), &[("Link", link.clone())], &link)?;
        if r.status != 200 && r.status != 201 {
            return Err(CamelError::io(format!("Cannot join LRA {}: coordinator returned status {}", self.url, r.status)));
        }
        Ok(())
    }

    fn compensate(&self, _exchange: &Exchange) -> Result<()> {
        self.put(&format!("{}/cancel", self.url), "cancel")
    }

    fn complete(&self, _exchange: &Exchange) -> Result<()> {
        self.put(&format!("{}/close", self.url), "close")
    }
}

// ---------------------------------------------------------------------------
// embedded coordinator for tests
// ---------------------------------------------------------------------------

/// Test support.
pub mod testing {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// Status of an LRA in the mock coordinator.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum LraStatus {
        Active,
        Closed,
        Cancelled,
    }

    #[derive(Default)]
    struct Lra {
        participants: Vec<(Option<String>, Option<String>)>,
        status: Option<LraStatus>,
    }

    struct State {
        lras: Mutex<BTreeMap<String, Lra>>,
        counter: AtomicUsize,
        port: Mutex<u16>,
    }

    /// A minimal Narayana-LRA compatible coordinator: `POST /lra-coordinator/start`,
    /// `PUT /lra-coordinator/{id}` (join with `Link`), `PUT .../close`, `PUT .../cancel`
    /// (calling the participants' `complete` / `compensate` links in reverse order with
    /// the `Long-Running-Action` header) and `TimeLimit` (cancel when expired).
    pub struct MockLraCoordinator {
        state: Arc<State>,
        server: MiniServer,
    }

    impl MockLraCoordinator {
        /// Start on a free local port.
        pub fn start() -> Result<MockLraCoordinator> {
            let state = Arc::new(State { lras: Mutex::new(BTreeMap::new()), counter: AtomicUsize::new(0), port: Mutex::new(0) });
            let st = state.clone();
            let server = MiniServer::start("127.0.0.1:0", Arc::new(move |req: &HttpMessage| handle(&st, req)))?;
            *state.port.lock().unwrap() = server.port;
            Ok(MockLraCoordinator { state, server })
        }

        /// Base url, e.g. `http://127.0.0.1:41234`.
        pub fn url(&self) -> String {
            format!("http://127.0.0.1:{}", self.server.port)
        }

        /// Status of an LRA (by its full url).
        pub fn status(&self, lra: &str) -> Option<LraStatus> {
            self.state.lras.lock().unwrap().get(lra).and_then(|l| l.status)
        }

        /// Number of participants that joined an LRA.
        pub fn participants(&self, lra: &str) -> usize {
            self.state.lras.lock().unwrap().get(lra).map(|l| l.participants.len()).unwrap_or(0)
        }

        /// All LRA urls created so far.
        pub fn lras(&self) -> Vec<String> {
            self.state.lras.lock().unwrap().keys().cloned().collect()
        }
    }

    fn finish(state: &Arc<State>, lra: &str, cancel: bool) -> (u16, Vec<(&'static str, String)>, String) {
        let participants = {
            let mut lras = state.lras.lock().unwrap();
            let Some(l) = lras.get_mut(lra) else { return (404, vec![], "unknown LRA".into()) };
            if l.status != Some(LraStatus::Active) {
                return (200, vec![], format!("{:?}", l.status.unwrap()));
            }
            l.status = Some(if cancel { LraStatus::Cancelled } else { LraStatus::Closed });
            l.participants.clone()
        };
        for (compensate, complete) in participants.iter().rev() {
            let target = if cancel { compensate } else { complete };
            if let Some(url) = target {
                // retry a few times like a real coordinator would
                for _ in 0..3 {
                    match http_request("PUT", url, &[(SAGA_LONG_RUNNING_ACTION, lra.to_string())], "") {
                        Ok(r) if r.status == 200 => break,
                        _ => std::thread::sleep(Duration::from_millis(20)),
                    }
                }
            }
        }
        (200, vec![], if cancel { "Cancelled".into() } else { "Closed".into() })
    }

    fn schedule_cancel(state: &Arc<State>, lra: String, ms: u64) {
        let st = state.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(ms));
            finish(&st, &lra, true);
        });
    }

    fn handle(state: &Arc<State>, req: &HttpMessage) -> (u16, Vec<(&'static str, String)>, String) {
        let port = *state.port.lock().unwrap();
        let path = req.path.trim_end_matches('/');
        let rest = match path.strip_prefix("/lra-coordinator") {
            Some(r) => r.trim_start_matches('/'),
            None => return (404, vec![], "not found".into()),
        };
        let time_limit = req.query.get("TimeLimit").and_then(|t| t.parse::<u64>().ok()).unwrap_or(0);
        if req.method == "POST" && rest == "start" {
            let n = state.counter.fetch_add(1, Ordering::SeqCst) + 1;
            let url = format!("http://127.0.0.1:{}/lra-coordinator/lra-{}", port, n);
            state.lras.lock().unwrap().insert(url.clone(), Lra { participants: vec![], status: Some(LraStatus::Active) });
            if time_limit > 0 {
                schedule_cancel(state, url.clone(), time_limit);
            }
            return (201, vec![("Location", url.clone())], url);
        }
        if req.method != "PUT" {
            return (404, vec![], "not found".into());
        }
        let (id, action) = rest.split_once('/').unwrap_or((rest, ""));
        let lra = format!("http://127.0.0.1:{}/lra-coordinator/{}", port, id);
        match action {
            "close" => finish(state, &lra, false),
            "cancel" => finish(state, &lra, true),
            "" => {
                let link = req.header("Link").map(String::from).unwrap_or_else(|| req.body.clone());
                let links = parse_link_header(&link);
                let get = |rel: &str| links.iter().find(|(r, _)| r == rel).map(|(_, u)| u.clone());
                {
                    let mut lras = state.lras.lock().unwrap();
                    let Some(l) = lras.get_mut(&lra) else { return (404, vec![], "unknown LRA".into()) };
                    if l.status != Some(LraStatus::Active) {
                        return (409, vec![], "LRA is not active".into());
                    }
                    l.participants.push((get("compensate"), get("complete")));
                }
                if time_limit > 0 {
                    schedule_cancel(state, lra.clone(), time_limit);
                }
                (200, vec![], lra)
            }
            _ => (404, vec![], "not found".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_header_parsing() {
        let l = parse_link_header("<http://a/compensate?x=1%2C2>; rel=\"compensate\", <http://a/complete>; rel=\"complete\"");
        assert_eq!(l, vec![("compensate".to_string(), "http://a/compensate?x=1%2C2".to_string()), ("complete".to_string(), "http://a/complete".to_string())]);
    }
}

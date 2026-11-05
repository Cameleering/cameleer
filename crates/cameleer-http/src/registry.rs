//! Shared HTTP server registry: one listener per port, path based dispatch.
//!
//! Many consumers (routes) can bind paths on the same port. Paths may contain
//! templates (`/users/{id}`), a trailing `*` wildcard, or be registered with
//! `match_on_uri_prefix`. The [`Router`] answers `404` for unknown paths, `405` (with an
//! `Allow` header) for method mismatches, `415` for `consumes` mismatches and `406` for
//! `produces` / `Accept` mismatches.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use cameleer_core::error::{CamelError, Result};

use crate::http::{media_type_matches, percent_decode, HttpRequest, HttpResponse};
use crate::server::{HttpServer, ServerConfig};

#[derive(Clone, Debug, PartialEq)]
enum Segment {
    Literal(String),
    Param(String),
}

/// A path template such as `/users/{id}/orders` or `/static/*`.
#[derive(Clone, Debug, PartialEq)]
pub struct PathTemplate {
    raw: String,
    segments: Vec<Segment>,
    wildcard: bool,
}

fn split_segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

impl PathTemplate {
    pub fn parse(path: &str) -> PathTemplate {
        let mut segs = split_segments(path);
        let wildcard = segs.last() == Some(&"*");
        if wildcard {
            segs.pop();
        }
        let segments = segs
            .into_iter()
            .map(|s| match s.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                Some(name) => Segment::Param(name.to_string()),
                None => Segment::Literal(percent_decode(s, false)),
            })
            .collect();
        PathTemplate { raw: path.to_string(), segments, wildcard }
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// Names of template parameters.
    pub fn param_names(&self) -> Vec<String> {
        self.segments
            .iter()
            .filter_map(|s| match s {
                Segment::Param(p) => Some(p.clone()),
                _ => None,
            })
            .collect()
    }

    /// Match a raw request path; returns decoded template parameters.
    pub fn matches(&self, raw_path: &str, match_prefix: bool) -> Option<Vec<(String, String)>> {
        let segs = split_segments(raw_path);
        let prefix = match_prefix || self.wildcard;
        if segs.len() < self.segments.len() || (!prefix && segs.len() != self.segments.len()) {
            return None;
        }
        let mut params = Vec::new();
        for (t, s) in self.segments.iter().zip(segs.iter()) {
            let decoded = percent_decode(s, false);
            match t {
                Segment::Literal(l) => {
                    if *l != decoded {
                        return None;
                    }
                }
                Segment::Param(p) => params.push((p.clone(), decoded)),
            }
        }
        Some(params)
    }

    fn literal_count(&self) -> usize {
        self.segments.iter().filter(|s| matches!(s, Segment::Literal(_))).count()
    }

    fn same_shape(&self, other: &PathTemplate) -> bool {
        self.wildcard == other.wildcard
            && self.segments.len() == other.segments.len()
            && self.segments.iter().zip(other.segments.iter()).all(|(a, b)| match (a, b) {
                (Segment::Literal(x), Segment::Literal(y)) => x == y,
                (Segment::Param(_), Segment::Param(_)) => true,
                _ => false,
            })
    }
}

/// What a consumer registers on a server.
#[derive(Clone, Debug, Default)]
pub struct HttpRouteSpec {
    /// Path (template) to bind.
    pub path: String,
    /// Allowed methods (upper case); `None` = all.
    pub methods: Option<Vec<String>>,
    pub match_on_uri_prefix: bool,
    /// Accepted request content types (comma separated).
    pub consumes: Option<String>,
    /// Produced content types (comma separated).
    pub produces: Option<String>,
}

impl HttpRouteSpec {
    fn allows(&self, method: &str) -> bool {
        match &self.methods {
            None => true,
            Some(m) => m.iter().any(|x| x == method || (method == "HEAD" && x == "GET")),
        }
    }
}

/// Handles a matched request; receives the path template parameters.
pub type RouteHandler = Arc<dyn Fn(&mut HttpRequest, Vec<(String, String)>) -> HttpResponse + Send + Sync>;

struct RouteEntry {
    id: u64,
    spec: HttpRouteSpec,
    template: PathTemplate,
    handler: RouteHandler,
    /// Request bodies are streamed (`disableStreamCache=true`).
    streaming: bool,
}

/// Path based dispatcher.
#[derive(Default)]
pub struct Router {
    entries: RwLock<Vec<RouteEntry>>,
}

static ROUTE_IDS: AtomicU64 = AtomicU64::new(1);

impl Router {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a route; fails if an equivalent path with overlapping methods exists.
    pub fn add(&self, spec: HttpRouteSpec, handler: RouteHandler) -> Result<u64> {
        let template = PathTemplate::parse(&spec.path);
        let mut entries = self.entries.write().unwrap();
        for e in entries.iter() {
            if e.template.same_shape(&template) && e.spec.match_on_uri_prefix == spec.match_on_uri_prefix {
                let overlap = match (&e.spec.methods, &spec.methods) {
                    (None, _) | (_, None) => true,
                    (Some(a), Some(b)) => a.iter().any(|m| b.contains(m)),
                };
                if overlap {
                    return Err(CamelError::illegal_state(format!(
                        "An HTTP consumer is already registered for path {} (methods {:?})",
                        spec.path, e.spec.methods
                    )));
                }
            }
        }
        let id = ROUTE_IDS.fetch_add(1, Ordering::Relaxed);
        entries.push(RouteEntry { id, spec, template, handler, streaming: false });
        Ok(id)
    }

    /// Remove a route; returns true if it existed.
    pub fn remove(&self, id: u64) -> bool {
        let mut entries = self.entries.write().unwrap();
        let before = entries.len();
        entries.retain(|e| e.id != id);
        before != entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.read().unwrap().is_empty()
    }

    /// Registered paths (for diagnostics).
    pub fn paths(&self) -> Vec<String> {
        self.entries.read().unwrap().iter().map(|e| e.spec.path.clone()).collect()
    }

    /// Stream request bodies of a route (instead of reading them into memory).
    pub fn set_streaming(&self, id: u64, streaming: bool) {
        for e in self.entries.write().unwrap().iter_mut().filter(|e| e.id == id) {
            e.streaming = streaming;
        }
    }

    /// Whether the route a request (head) would be dispatched to streams its body.
    pub fn wants_stream(&self, req: &HttpRequest) -> bool {
        let entries = self.entries.read().unwrap();
        let mut candidates: Vec<&RouteEntry> = entries.iter().filter(|e| e.template.matches(&req.raw_path, e.spec.match_on_uri_prefix).is_some() && e.spec.allows(&req.method)).collect();
        candidates.sort_by_key(|e| {
            let prefix = e.spec.match_on_uri_prefix || e.template.wildcard;
            (prefix, std::cmp::Reverse(e.template.literal_count()), std::cmp::Reverse(e.template.segments.len()))
        });
        candidates.first().is_some_and(|e| e.streaming)
    }

    /// Dispatch a request to the best matching route.
    pub fn dispatch(&self, req: &mut HttpRequest) -> HttpResponse {
        let (handler, params) = {
            let entries = self.entries.read().unwrap();
            let mut candidates: Vec<(&RouteEntry, Vec<(String, String)>)> = entries
                .iter()
                .filter_map(|e| e.template.matches(&req.raw_path, e.spec.match_on_uri_prefix).map(|p| (e, p)))
                .collect();
            if candidates.is_empty() {
                return HttpResponse::text(404, "Not Found");
            }
            // most specific first: exact before prefix, more literal segments, longer templates
            candidates.sort_by_key(|(e, _)| {
                let prefix = e.spec.match_on_uri_prefix || e.template.wildcard;
                (prefix, std::cmp::Reverse(e.template.literal_count()), std::cmp::Reverse(e.template.segments.len()))
            });
            let allowed: Vec<&(&RouteEntry, Vec<(String, String)>)> = candidates.iter().filter(|(e, _)| e.spec.allows(&req.method)).collect();
            let chosen = match allowed.first() {
                Some(c) => *c,
                None => {
                    let mut methods: Vec<String> = Vec::new();
                    for (e, _) in &candidates {
                        for m in e.spec.methods.iter().flatten() {
                            if !methods.contains(m) {
                                methods.push(m.clone());
                            }
                        }
                    }
                    let allow = methods.join(", ");
                    if req.method == "OPTIONS" {
                        return HttpResponse::new(204).with_header("Allow", &format!("{}, OPTIONS", allow));
                    }
                    return HttpResponse::text(405, "Method Not Allowed").with_header("Allow", &allow);
                }
            };
            let (entry, params) = chosen;
            if let (Some(consumes), Some(ct)) = (&entry.spec.consumes, req.content_type()) {
                if !req.body.is_empty() && !consumes.split(',').any(|c| media_type_matches(c.trim(), ct)) {
                    return HttpResponse::text(415, "Unsupported Media Type");
                }
            }
            if let (Some(produces), Some(accept)) = (&entry.spec.produces, req.headers.get("Accept")) {
                let ok = accept.split(',').any(|a| produces.split(',').any(|p| media_type_matches(a.trim(), p.trim())));
                if !ok {
                    return HttpResponse::text(406, "Not Acceptable");
                }
            }
            (entry.handler.clone(), params.clone())
        };
        handler(req, params)
    }
}

/// The server handler of a [`Router`].
struct RouterHandler(Arc<Router>);

impl crate::server::HttpHandler for RouterHandler {
    fn handle(&self, request: &mut HttpRequest) -> HttpResponse {
        self.0.dispatch(request)
    }
    fn stream_request(&self, request: &HttpRequest) -> bool {
        self.0.wants_stream(request)
    }
}

/// A server shared by all consumers bound to its port.
pub struct SharedServer {
    pub server: HttpServer,
    pub router: Arc<Router>,
}

/// Handle returned by [`ServerRegistry::register`]; pass it to `unregister`.
#[derive(Debug)]
pub struct Registration {
    /// The actual bound port.
    pub port: u16,
    route_id: u64,
}

/// Process wide registry of HTTP servers keyed by port.
#[derive(Default)]
pub struct ServerRegistry {
    servers: Mutex<HashMap<u16, Arc<SharedServer>>>,
}

/// The global server registry.
pub fn registry() -> &'static ServerRegistry {
    static REG: OnceLock<ServerRegistry> = OnceLock::new();
    REG.get_or_init(ServerRegistry::default)
}

impl ServerRegistry {
    /// Stream the request bodies of a registered route (`disableStreamCache=true`).
    pub fn set_streaming(&self, reg: &Registration, streaming: bool) {
        if let Some(s) = self.servers.lock().unwrap().get(&reg.port) {
            s.router.set_streaming(reg.route_id, streaming);
        }
    }

    /// Register a route on `host:port`, binding a new server if no server owns the port.
    /// Port 0 always binds a new server on a free port.
    pub fn register(&self, host: &str, port: u16, config: &ServerConfig, spec: HttpRouteSpec, handler: RouteHandler) -> Result<Registration> {
        let mut servers = self.servers.lock().unwrap();
        if port != 0 {
            if let Some(s) = servers.get(&port) {
                if s.server.is_tls() != config.tls.is_some() {
                    return Err(CamelError::illegal_argument(format!(
                        "Port {} is already used by an {} server; cannot register an {} endpoint on it",
                        port,
                        if s.server.is_tls() { "https" } else { "http" },
                        if config.tls.is_some() { "https" } else { "http" }
                    )));
                }
                let route_id = s.router.add(spec, handler)?;
                return Ok(Registration { port, route_id });
            }
        }
        let router = Arc::new(Router::new());
        let route_id = router.add(spec, handler)?;
        let r2 = router.clone();
        let bind_host = if host.contains(':') && !host.starts_with('[') { format!("[{}]", host) } else { host.to_string() };
        let server = HttpServer::bind(format!("{}:{}", bind_host, port), config.clone(), RouterHandler(r2))
            .map_err(|e| CamelError::io(format!("Failed to bind HTTP server on {}:{}: {}", host, port, e)))?;
        let actual = server.port();
        cameleer_core::camel_info!("cameleer-http", "HTTP server started on {}:{}", host, actual);
        servers.insert(actual, Arc::new(SharedServer { server, router }));
        Ok(Registration { port: actual, route_id })
    }

    /// Remove a registration; the server is stopped when its last route is removed.
    pub fn unregister(&self, reg: &Registration) {
        let to_stop = {
            let mut servers = self.servers.lock().unwrap();
            match servers.get(&reg.port) {
                Some(s) => {
                    s.router.remove(reg.route_id);
                    if s.router.is_empty() {
                        servers.remove(&reg.port)
                    } else {
                        None
                    }
                }
                None => None,
            }
        };
        if let Some(s) = to_stop {
            s.server.stop();
            cameleer_core::camel_info!("cameleer-http", "HTTP server on port {} stopped", reg.port);
        }
    }

    /// Ports with a running server.
    pub fn ports(&self) -> Vec<u16> {
        let mut v: Vec<u16> = self.servers.lock().unwrap().keys().copied().collect();
        v.sort();
        v
    }

    /// Paths registered on a port.
    pub fn paths(&self, port: u16) -> Vec<String> {
        self.servers.lock().unwrap().get(&port).map(|s| s.router.paths()).unwrap_or_default()
    }

    /// Serve a request in process, without a socket (the AWS Lambda runtime turns API Gateway
    /// and function URL events into requests). Servers are tried in port order; the first
    /// answer that is not 404 wins. `None` when no HTTP route is registered.
    pub fn dispatch(&self, req: &mut HttpRequest) -> Option<HttpResponse> {
        let mut servers: Vec<(u16, Arc<SharedServer>)> = self.servers.lock().unwrap().iter().map(|(p, s)| (*p, s.clone())).collect();
        if servers.is_empty() {
            return None;
        }
        servers.sort_by_key(|(p, _)| *p);
        let mut last = None;
        for (_, s) in servers {
            let resp = s.router.dispatch(req);
            if resp.status != 404 {
                return Some(resp);
            }
            last = Some(resp);
        }
        last
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates() {
        let t = PathTemplate::parse("/users/{id}/orders");
        assert_eq!(t.matches("/users/42/orders", false), Some(vec![("id".into(), "42".into())]));
        assert_eq!(t.matches("/users/42/orders/", false), Some(vec![("id".into(), "42".into())]));
        assert_eq!(t.matches("/users/42", false), None);
        assert_eq!(t.matches("/users/42/orders/x", false), None);
        assert!(t.matches("/users/42/orders/x", true).is_some());
        let w = PathTemplate::parse("/static/*");
        assert!(w.matches("/static/a/b.css", false).is_some());
        assert_eq!(PathTemplate::parse("/a/{b}").matches("/a/x%20y", false), Some(vec![("b".into(), "x y".into())]));
    }

    #[test]
    fn router_dispatch() {
        let r = Router::new();
        let h = |name: &'static str| -> RouteHandler { Arc::new(move |_req, p| HttpResponse::text(200, format!("{} {:?}", name, p))) };
        r.add(HttpRouteSpec { path: "/users/{id}".into(), methods: Some(vec!["GET".into()]), ..Default::default() }, h("get")).unwrap();
        r.add(HttpRouteSpec { path: "/users/me".into(), methods: Some(vec!["POST".into()]), ..Default::default() }, h("me")).unwrap();
        assert!(r.add(HttpRouteSpec { path: "/users/{x}".into(), methods: None, ..Default::default() }, h("dup")).is_err());
        let mut req = HttpRequest::new("GET", "/users/me");
        assert!(r.dispatch(&mut req).body_text().starts_with("get"));
        let mut req = HttpRequest::new("POST", "/users/me");
        assert!(r.dispatch(&mut req).body_text().starts_with("me"));
        let mut req = HttpRequest::new("DELETE", "/users/1");
        let resp = r.dispatch(&mut req);
        assert_eq!(resp.status, 405);
        assert_eq!(resp.headers.get("Allow"), Some("GET"));
        let mut req = HttpRequest::new("GET", "/nope");
        assert_eq!(r.dispatch(&mut req).status, 404);
    }
}

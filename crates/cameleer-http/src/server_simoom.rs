//! The simoom event-loop engine behind [`crate::server::HttpServer`] (spike; see
//! `docs/design/simoom-adoption.md`).
//!
//! Selected at run time with `CAMEL_HTTP_ENGINE=simoom` (plain HTTP only; TLS listeners keep the
//! thread-per-connection engine in this spike).
//!
//! How a request runs:
//!
//! 1. A route that never blocks ([`HttpHandler::is_async`], the MCP endpoint) runs on the loop
//!    thread and answers later through a responder, from whatever thread finishes the work.
//! 2. Any other route starts on simoom's offload pool, where a [`Governor`] times its calls. When 126
//!    of 128 calls took under 1 ms the route moves to the loop thread; if it ever stalls the loop
//!    (one call over 50 ms, or three over 5 ms in a second) it goes back to the pool for a minute. A
//!    route that blocks (a call to another service, a database) is not promoted.
//! 3. Requests with a streamed body, handlers that ask for a streamed body, and routes the registry
//!    cannot name (closures, test servers) always run on the offload pool.

use std::collections::HashMap;
use std::io::{self, Read};
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use crate::http::{HttpHeaders, HttpRequest, HttpResponse, Version as HttpVersion};
use crate::server::{HttpHandler, ServerConfig};

/// A call longer than this stalls the loop thread.
const STALL: Duration = Duration::from_millis(5);
/// A call faster than this counts towards moving a route to the loop thread: 126 of 128 must be.
const FAST: Duration = Duration::from_millis(1);
const SAMPLE: u32 = 128;
const MAX_SLOW: u32 = 2;
/// A call this long is plainly blocking.
const BLOCKING: Duration = Duration::from_millis(50);
/// Stalls within [`WINDOW_MS`] that move a route to the pool.
const STALLS_TO_POOL: u32 = 3;
const WINDOW_MS: u64 = 1000;
/// How long a route stays on the pool before the loop thread is tried again.
const POOL_MS: u64 = 60_000;

/// What the engine has learned about one route.
#[derive(Default)]
struct RouteState {
    /// The route runs on the loop thread.
    inline: AtomicBool,
    /// Calls and slow calls of the current sample on the pool.
    seen: AtomicU32,
    slow: AtomicU32,
    /// No promotion to the loop thread before this time (ms since start).
    pool_until: AtomicU64,
    window_start: AtomicU64,
    stalls: AtomicU32,
}

/// Decides per route whether a handler may run on the loop thread.
struct Governor {
    base: Instant,
    routes: RwLock<HashMap<u64, Arc<RouteState>>>,
}

impl Governor {
    fn new() -> Governor {
        Governor { base: Instant::now(), routes: RwLock::new(HashMap::new()) }
    }

    /// Milliseconds since start, never 0.
    fn now(&self) -> u64 {
        self.base.elapsed().as_millis() as u64 + 1
    }

    fn state(&self, key: u64) -> Arc<RouteState> {
        if let Some(s) = self.routes.read().unwrap_or_else(|p| p.into_inner()).get(&key) {
            return s.clone();
        }
        self.routes.write().unwrap_or_else(|p| p.into_inner()).entry(key).or_default().clone()
    }

    fn inline_ok(&self, st: &RouteState) -> bool {
        st.inline.load(Ordering::Relaxed)
    }

    /// A call on the loop thread took `took`: back to the pool when it stalls the loop.
    fn record(&self, st: &RouteState, took: Duration) {
        if took < STALL {
            return;
        }
        let now = self.now();
        if took >= BLOCKING {
            self.demote(st, now);
            return;
        }
        if now.saturating_sub(st.window_start.load(Ordering::Relaxed)) > WINDOW_MS {
            st.window_start.store(now, Ordering::Relaxed);
            st.stalls.store(1, Ordering::Relaxed);
        } else if st.stalls.fetch_add(1, Ordering::Relaxed) + 1 >= STALLS_TO_POOL {
            self.demote(st, now);
        }
    }

    fn demote(&self, st: &RouteState, now: u64) {
        st.inline.store(false, Ordering::Relaxed);
        st.seen.store(0, Ordering::Relaxed);
        st.slow.store(0, Ordering::Relaxed);
        st.stalls.store(0, Ordering::Relaxed);
        st.pool_until.store(now + POOL_MS, Ordering::Relaxed);
    }

    /// A call on the pool took `took` (the handler only, without the queue).
    fn observe(&self, st: &RouteState, took: Duration) {
        if took >= FAST {
            st.slow.fetch_add(1, Ordering::Relaxed);
        }
        if st.seen.fetch_add(1, Ordering::Relaxed) + 1 >= SAMPLE {
            if st.slow.load(Ordering::Relaxed) <= MAX_SLOW && self.now() >= st.pool_until.load(Ordering::Relaxed) {
                st.inline.store(true, Ordering::Relaxed);
            }
            st.seen.store(0, Ordering::Relaxed);
            st.slow.store(0, Ordering::Relaxed);
        }
    }
}

/// A running simoom server.
pub struct SimoomEngine {
    handle: simoom::ServerHandle,
    shutdown: Duration,
}

pub fn selected() -> bool {
    std::env::var("CAMEL_HTTP_ENGINE").map(|v| v.eq_ignore_ascii_case("simoom")).unwrap_or(false)
}

impl SimoomEngine {
    pub fn start(addr: impl ToSocketAddrs, config: &ServerConfig, handler: Arc<dyn HttpHandler>) -> io::Result<SimoomEngine> {
        let mut limits = simoom::Limits::default();
        limits.max_body = config.limits.max_body_bytes as u64;
        limits.max_head = config.limits.max_header_bytes;
        limits.max_headers = config.limits.max_headers;
        limits.keep_alive_timeout = config.keep_alive_timeout;
        limits.write_timeout = config.write_timeout;
        limits.max_requests_per_connection = config.max_keep_alive_requests as u64;
        let governor = Arc::new(Governor::new());
        let service = move |req: &mut simoom::Request<'_>, reply: simoom::Reply<'_>| -> simoom::Outcome {
            // the route, found once (a streamed body goes to the pool unexamined)
            let mut state: Option<Arc<RouteState>> = None;
            if !req.is_streamed() {
                let head = HttpRequest::new(&String::from_utf8_lossy(req.method()), &String::from_utf8_lossy(req.target()));
                // 1. never blocks: answer later
                if handler.is_async(&head) {
                    let r = build_request(req);
                    let head_request = r.method == "HEAD";
                    let responder = reply.defer();
                    handler.handle_async(r, Box::new(move |resp| complete(responder, resp, head_request)));
                    return simoom::Outcome::Deferred;
                }
                // 2. fast so far: on the loop thread, timed
                let key = handler.route_key(&head);
                if key != 0 {
                    let st = governor.state(key);
                    if governor.inline_ok(&st) {
                        let mut r = build_request(req);
                        if !(has_body(&r) && handler.stream_request(&r)) {
                            let head_request = r.method == "HEAD";
                            let t0 = Instant::now();
                            let resp = run(&*handler, &mut r);
                            governor.record(&st, t0.elapsed());
                            return finish(reply, resp, head_request);
                        }
                    }
                    state = Some(st);
                }
            }
            // 3. the offload pool; the handler's calls there are timed for the route
            let h = handler.clone();
            let observe = state.map(|st| (governor.clone(), st));
            reply.offload(req, move |req, reply| serve(&*h, req, reply, observe))
        };
        let handle = simoom::Server::builder().listen(addr).limits(limits).service(service).start()?;
        Ok(SimoomEngine { handle, shutdown: config.shutdown_timeout })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.handle.local_addr()
    }

    pub fn connection_count(&self) -> usize {
        self.handle.stats().active
    }

    pub fn stop(&self) {
        self.handle.stop(self.shutdown);
    }
}

fn has_body(r: &HttpRequest) -> bool {
    !r.body.is_empty()
}

/// Run the handler, turning a panic into a 500.
fn run(handler: &dyn HttpHandler, r: &mut HttpRequest) -> HttpResponse {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler.handle(r))) {
        Ok(resp) => resp,
        Err(_) => HttpResponse::text(500, "Internal Server Error"),
    }
}

/// A request on an offload worker: the body may stream, and the handler may ask for it as a stream.
fn serve(handler: &dyn HttpHandler, req: &mut simoom::Request<'_>, reply: simoom::Reply<'_>, observe: Option<(Arc<Governor>, Arc<RouteState>)>) -> simoom::Outcome {
    let mut r = build_head(req);
    let has_body = req.is_streamed() || !req.body().is_empty();
    let mut stream_handle = None;
    if has_body && handler.stream_request(&r) {
        // hand the body over as a stream handle, as the thread-per-connection engine does
        let len = r.headers.get("Content-Length").and_then(|v| v.trim().parse::<u64>().ok());
        let mut buffered = req.body().to_vec();
        let reader: Box<dyn Read + Send> = match req.take_body() {
            Some(src) => {
                let first = std::io::Cursor::new(std::mem::take(&mut buffered));
                Box::new(first.chain(src))
            }
            None => Box::new(std::io::Cursor::new(buffered)),
        };
        let h = cameleer_tls::stream_body::register(reader, len, &r.target);
        r.body_stream = Some(h.clone());
        stream_handle = Some(h);
    } else {
        let mut body = req.body().to_vec();
        if let Some(mut src) = req.take_body() {
            if src.read_to_end(&mut body).is_err() {
                return reply.status(400).body_static(b"bad request body");
            }
        }
        r.body = body;
    }
    let head_request = r.method == "HEAD";
    let t0 = Instant::now();
    let resp = run(handler, &mut r);
    if let Some((g, st)) = &observe {
        // a handler that streamed its request body, or answers with a stream, is never promoted
        if stream_handle.is_none() && resp.stream.is_none() {
            g.observe(st, t0.elapsed());
        } else {
            st.slow.fetch_add(1, Ordering::Relaxed);
        }
    }
    if let Some(h) = stream_handle {
        cameleer_tls::stream_body::release(&h);
    }
    finish(reply, resp, head_request)
}

/// The head of a request (no body).
fn build_head(req: &mut simoom::Request<'_>) -> HttpRequest {
    let target = String::from_utf8_lossy(req.target()).into_owned();
    let mut r = HttpRequest::new(&String::from_utf8_lossy(req.method()), &target);
    r.version = match req.version() {
        simoom::Version::Http10 => HttpVersion::Http10,
        _ => HttpVersion::Http11,
    };
    let mut headers = HttpHeaders::new();
    for (k, v) in req.headers() {
        headers.add(String::from_utf8_lossy(k).into_owned(), String::from_utf8_lossy(v).into_owned());
    }
    // HTTP/2 and HTTP/3 carry the host in :authority
    if !headers.contains("Host") {
        if let Some(h) = req.host() {
            headers.add("Host", String::from_utf8_lossy(h).into_owned());
        }
    }
    r.headers = headers;
    r.remote_addr = req.peer_addr();
    r
}

/// The head and the buffered body of a request.
fn build_request(req: &mut simoom::Request<'_>) -> HttpRequest {
    let mut r = build_head(req);
    r.body = req.body().to_vec();
    r
}

/// Send a response. Plain bodies go out directly; streams and chunked bodies are pumped from their
/// own thread, so a blocking read never holds the loop (or a pool worker).
fn finish(reply: simoom::Reply<'_>, mut resp: HttpResponse, head_request: bool) -> simoom::Outcome {
    if resp.stream.is_some() || (resp.chunked && !head_request && !resp.body.is_empty()) {
        let responder = reply.defer();
        complete(responder, resp, head_request);
        return simoom::Outcome::Deferred;
    }
    let mut reply = reply.status(resp.status);
    for (k, v) in resp.headers.iter() {
        // framing is the server's business
        if k.eq_ignore_ascii_case("content-length") || k.eq_ignore_ascii_case("transfer-encoding") {
            continue;
        }
        reply = reply.header(k, v);
    }
    reply.body(std::mem::take(&mut resp.body))
}

/// Send a response through a deferred responder, from the thread that finished the work.
fn complete(responder: simoom::Responder, mut resp: HttpResponse, head_request: bool) {
    let mut out = simoom::Response::new(resp.status);
    for (k, v) in resp.headers.iter() {
        if k.eq_ignore_ascii_case("content-length") || k.eq_ignore_ascii_case("transfer-encoding") {
            continue;
        }
        out = out.header(k, v);
    }
    if let Some(handle) = resp.stream.take() {
        if let Some((mut reader, _)) = cameleer_tls::stream_body::take(&handle) {
            if head_request {
                responder.send(out);
                return;
            }
            // an event stream: pumped from its own thread so the caller can go on producing
            let _ = std::thread::Builder::new().name("simoom-stream".into()).spawn(move || {
                let mut sink = responder.stream(out);
                let mut buf = vec![0u8; 16 * 1024];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            if sink.write(&buf[..n]).is_err() {
                                sink.abort();
                                return;
                            }
                        }
                        Err(_) => {
                            sink.abort();
                            return;
                        }
                    }
                }
                sink.finish();
            });
            return;
        }
    }
    if resp.chunked && !head_request && !resp.body.is_empty() {
        // `chunked=true` routes: 8 KiB chunks, like the thread-per-connection engine
        let body = std::mem::take(&mut resp.body);
        let _ = std::thread::Builder::new().name("simoom-stream".into()).spawn(move || {
            let mut sink = responder.stream(out);
            for part in body.chunks(8192) {
                if sink.write(part).is_err() {
                    sink.abort();
                    return;
                }
            }
            sink.finish();
        });
        return;
    }
    responder.send(out.body(std::mem::take(&mut resp.body)));
}

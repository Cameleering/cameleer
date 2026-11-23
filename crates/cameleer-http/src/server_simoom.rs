//! The simoom event-loop engine behind [`crate::server::HttpServer`] (spike; see
//! `docs/design/simoom-adoption.md`).
//!
//! Plain HTTP and HTTPS (TLS 1.2 and 1.3, optional client certificates), HTTP/1.1 and HTTP/2: h2c prior
//! knowledge, `Upgrade: h2c`, and ALPN `h2` on HTTPS.
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
/// A route on the loop thread is also sent back to the pool when its calls keep the loop threads this
/// busy (the share of all loop threads' time over [`BUSY_WINDOW_MS`]). A route that blocks briefly (a
/// Kafka or database call of 0.3 to 1 ms on a quiet connection) can look fast while the server is idle;
/// once load arrives its calls run one after the other on the loop threads and only the stall rule
/// would take it off, which needs a single call over 5 ms and can take 10 s or more.
const BUSY_SHARE: f64 = 0.5;
const BUSY_WINDOW_MS: u64 = 100;
/// Calls shorter than this are not added up: the route cannot be blocking, and a fast route pays nothing.
const BUSY_MIN: Duration = Duration::from_micros(100);

/// What the engine has learned about one route.
#[doc(hidden)]
#[derive(Default)]
pub struct RouteState {
    /// The route runs on the loop thread.
    inline: AtomicBool,
    /// Calls and slow calls of the current sample on the pool.
    seen: AtomicU32,
    slow: AtomicU32,
    /// No promotion to the loop thread before this time (ms since start).
    pool_until: AtomicU64,
    window_start: AtomicU64,
    stalls: AtomicU32,
    /// Time spent in calls on the loop threads in the current busy window, and when the window began.
    busy_ns: AtomicU64,
    busy_start: AtomicU64,
}

/// Decides per route whether a handler may run on the loop thread (also used by the gRPC server
/// for unary calls).
#[doc(hidden)]
pub struct Governor {
    base: Instant,
    routes: RwLock<HashMap<u64, Arc<RouteState>>>,
    /// Event loop threads (the busy limit is a share of their time).
    loops: u64,
}

impl Governor {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Governor {
        Governor::with_loops(std::thread::available_parallelism().map_or(1, |n| n.get()))
    }

    /// A governor for a server with `loops` event loop threads.
    pub fn with_loops(loops: usize) -> Governor {
        Governor { base: Instant::now(), routes: RwLock::new(HashMap::new()), loops: loops.max(1) as u64 }
    }

    /// Milliseconds since start, never 0.
    fn now(&self) -> u64 {
        self.base.elapsed().as_millis() as u64 + 1
    }

    pub fn state(&self, key: u64) -> Arc<RouteState> {
        if let Some(s) = self.routes.read().unwrap_or_else(|p| p.into_inner()).get(&key) {
            return s.clone();
        }
        self.routes.write().unwrap_or_else(|p| p.into_inner()).entry(key).or_default().clone()
    }

    pub fn inline_ok(&self, st: &RouteState) -> bool {
        st.inline.load(Ordering::Relaxed)
    }

    /// A call on the loop thread took `took`: back to the pool when it stalls the loop.
    pub fn record(&self, st: &RouteState, took: Duration) {
        if took < BUSY_MIN {
            return;
        }
        let now = self.now();
        self.account(st, took, now);
        if took < STALL {
            return;
        }
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

    /// Adds `took` to the route's busy time; at the end of a window, demotes the route when it used more
    /// than [`BUSY_SHARE`] of all loop threads' time.
    fn account(&self, st: &RouteState, took: Duration, now: u64) {
        st.busy_ns.fetch_add(took.as_nanos().min(u128::from(u64::MAX / 2)) as u64, Ordering::Relaxed);
        let start = st.busy_start.load(Ordering::Relaxed);
        if now.saturating_sub(start) < BUSY_WINDOW_MS {
            return;
        }
        // one caller closes the window
        if st.busy_start.compare_exchange(start, now, Ordering::Relaxed, Ordering::Relaxed).is_err() {
            return;
        }
        let busy = st.busy_ns.swap(0, Ordering::Relaxed);
        let span_ns = now.saturating_sub(start).min(10 * BUSY_WINDOW_MS) * 1_000_000;
        // the first window of a route has no start: not a measurement
        if start != 0 && (busy as f64) > BUSY_SHARE * (span_ns * self.loops) as f64 {
            self.demote(st, now);
        }
    }

    fn demote(&self, st: &RouteState, now: u64) {
        st.inline.store(false, Ordering::Relaxed);
        st.seen.store(0, Ordering::Relaxed);
        st.slow.store(0, Ordering::Relaxed);
        st.stalls.store(0, Ordering::Relaxed);
        st.pool_until.store(now + POOL_MS, Ordering::Relaxed);
        st.busy_ns.store(0, Ordering::Relaxed);
    }

    /// A call on the pool took `took` (the handler only, without the queue).
    pub fn observe(&self, st: &RouteState, took: Duration) {
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

/// Whether the server starts one event loop first and the others later (simoom's `lazy_loops`).
///
/// A lazy server grows its loops when a third connection is open at once, but connections that
/// are already open stay on loop 0 for their whole life. HTTP/2 and gRPC clients keep a few
/// connections with many streams each, so a lazy server serves 3 of 4 connections from one
/// thread, which saturates at about 60,000 small requests per second while the other loops
/// idle (docs/design/http2-throughput.md). Every loop therefore starts with the server, which
/// costs about 0.8 ms of start-up and 0.5 MB on 12 CPUs. A FaaS instance (the AWS Lambda
/// runtime API is set) serves one connection at a time and keeps the lazy start.
/// `CAMEL_SERVER_LAZY_LOOPS=true|false` overrides.
pub fn lazy_loops() -> bool {
    lazy_loops_from(std::env::var("CAMEL_SERVER_LAZY_LOOPS").ok().as_deref(), std::env::var_os("AWS_LAMBDA_RUNTIME_API").is_some())
}

fn lazy_loops_from(setting: Option<&str>, faas: bool) -> bool {
    match setting.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("true" | "1" | "yes") => true,
        Some("false" | "0" | "no") => false,
        _ => faas,
    }
}

/// Grows the process's descriptor table now, while this is still the only thread.
///
/// simoom grows the table to `reserve_fds` (1024) descriptors when a server starts, because
/// growing it later stalls the loop thread that crosses a step. The kernel grows a table with
/// a plain reallocation while one thread uses it, but waits for an RCU grace period (20 to 40 ms
/// on WSL2, about 10 ms on a cloud VM) as soon as a second thread exists, so a server started
/// after a seda, timer or other consumer had spawned its threads cost tens of milliseconds
/// (docs/design/startup-regression-2026-11.md). [`register`](crate::register) and the first server
/// call this, which makes the reservation cheap whenever the components are registered before the
/// routes start. It is done once per process; a no-op on other systems.
pub fn reserve_descriptors_early() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        simoom_sys::net::reserve_descriptors(RESERVED_DESCRIPTORS);
    });
}

/// The table size simoom reserves (`simoom_rt::Config::reserve_fds`, 1024 by default).
const RESERVED_DESCRIPTORS: usize = 1024;

/// Applies [`lazy_loops`] to a simoom server builder (the HTTP and the gRPC server use it).
pub fn event_loops(builder: simoom::Builder) -> simoom::Builder {
    reserve_descriptors_early();
    let lazy = lazy_loops();
    builder.runtime(move |c| c.lazy_loops = lazy)
}

/// A running simoom server.
pub struct SimoomEngine {
    handle: std::sync::Mutex<Option<simoom::ServerHandle>>,
    addr: SocketAddr,
    shutdown: Duration,
}

impl SimoomEngine {
    pub fn start(addr: impl ToSocketAddrs, config: &ServerConfig, handler: Arc<dyn HttpHandler>) -> io::Result<SimoomEngine> {
        let mut limits = simoom::Limits::default();
        limits.max_body = config.limits.max_body_bytes as u64;
        limits.max_head = config.limits.max_header_bytes;
        limits.max_request_line = config.limits.max_header_bytes;
        limits.max_header_line = config.limits.max_header_bytes;
        limits.max_headers = config.limits.max_headers;
        limits.head_timeout = config.read_timeout;
        limits.keep_alive_timeout = config.keep_alive_timeout;
        limits.write_timeout = config.write_timeout;
        limits.max_requests_per_connection = config.max_keep_alive_requests as u64;
        limits.offload_threads = config.pool_size.max(1);
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
                    // a route started here runs on the loop until a step that may block
                    let _on_loop = cameleer_core::reactive::enter_loop();
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
                            let http10 = r.version == HttpVersion::Http10;
                            let t0 = Instant::now();
                            let resp = run(&*handler, &mut r);
                            governor.record(&st, t0.elapsed());
                            return finish(reply, resp, head_request, http10);
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
        let mut builder = simoom::Server::builder().limits(limits).service(service);
        builder = event_loops(builder);
        builder = match &config.tls {
            Some(tls) => builder.listen_tls(addr).tls(server_tls(tls)?),
            None => builder.listen(addr),
        };
        let handle = builder.start()?;
        let addr = handle.local_addr();
        Ok(SimoomEngine { handle: std::sync::Mutex::new(Some(handle)), addr, shutdown: config.shutdown_timeout })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn connection_count(&self) -> usize {
        self.handle.lock().unwrap_or_else(|p| p.into_inner()).as_ref().map_or(0, |h| h.stats().active)
    }

    /// Stop gracefully. Returns once the listening port can be bound again; the loops finish
    /// closing their connections in the background (a peer that ignores the close can hold one
    /// for a couple of seconds).
    pub fn stop(&self) {
        let handle = self.handle.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(h) = handle {
            h.stop(self.shutdown);
            let _ = std::thread::Builder::new().name("simoom-stop".into()).spawn(move || h.join());
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                if std::net::TcpListener::bind(self.addr).is_ok() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

/// simoom's TLS configuration for the certificate, key and options of a [`cameleer_tls::TlsServerConfig`]
/// (also used by the gRPC server).
#[doc(hidden)]
pub fn server_tls(cfg: &cameleer_tls::TlsServerConfig) -> io::Result<simoom_tls::ServerConfig> {
    use cameleer_tls::TlsVersion;
    let bad = |e: String| io::Error::new(io::ErrorKind::InvalidInput, format!("TLS configuration: {e}"));
    let x = cfg.export();
    let key = simoom_tls::CertifiedKey::from_der(x.chain, &x.key_pkcs8).map_err(|e| bad(e.to_string()))?;
    let mut out = simoom_tls::ServerConfig::new(key);
    out.alpn = x.alpn;
    let version = |v: TlsVersion| match v {
        TlsVersion::Tls12 => simoom_tls::tls::ProtocolVersion::Tls12,
        TlsVersion::Tls13 => simoom_tls::tls::ProtocolVersion::Tls13,
    };
    out.min_version = version(x.min_version);
    out.max_version = version(x.max_version);
    if !x.resumption {
        out.ticketer = None;
        out.session_ids = None;
    }
    if x.client_auth != cameleer_tls::ClientAuth::None {
        let mut roots = simoom_tls::x509::RootStore::empty();
        for der in &x.client_roots {
            roots.add_der(der).map_err(bad)?;
        }
        out.client_auth = match x.client_auth {
            cameleer_tls::ClientAuth::Need => simoom_tls::ClientAuth::Required(roots),
            _ => simoom_tls::ClientAuth::Optional(roots),
        };
    }
    Ok(out)
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
    let http10 = r.version == HttpVersion::Http10;
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
        // an echo route answers with the request's own stream: it is taken when the response goes out
        if resp.stream.as_ref() != Some(&h) {
            cameleer_tls::stream_body::release(&h);
        }
    }
    finish(reply, resp, head_request, http10)
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
    if let Some(t) = req.tls() {
        r.peer_certificates = t.peer_certificates().to_vec();
    }
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
fn finish(reply: simoom::Reply<'_>, mut resp: HttpResponse, head_request: bool, http10: bool) -> simoom::Outcome {
    // (an HTTP/1.0 client gets a plain body with a length: no chunking)
    if resp.stream.is_some() || (resp.chunked && !http10 && !head_request && !resp.body.is_empty()) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn inline_governor(loops: usize) -> (Governor, Arc<RouteState>) {
        let g = Governor::with_loops(loops);
        let st = g.state(1);
        for _ in 0..SAMPLE {
            g.observe(&st, Duration::from_micros(20));
        }
        assert!(g.inline_ok(&st), "128 fast calls move the route to the loop thread");
        (g, st)
    }

    /// A route that blocks for 0.5 ms looks fast on an idle server. Once its calls fill the loop threads
    /// (here 4 ms of calls in 1 ms on a single loop) it goes back to the pool within a window, without
    /// any call reaching the stall limit.
    #[test]
    fn a_route_that_saturates_the_loops_returns_to_the_pool() {
        let (g, st) = inline_governor(1);
        let t = Instant::now();
        while t.elapsed() < Duration::from_millis(400) && g.inline_ok(&st) {
            for _ in 0..50 {
                g.record(&st, Duration::from_micros(2500)); // under STALL, 125 ms of calls per burst
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!g.inline_ok(&st), "busy loops demote the route");
        assert!(st.pool_until.load(Ordering::Relaxed) > g.now(), "and it stays on the pool for a while");
    }

    /// A route whose calls take microseconds stays on the loop thread however many requests arrive.
    #[test]
    fn a_fast_route_stays_on_the_loop() {
        let (g, st) = inline_governor(4);
        let t = Instant::now();
        while t.elapsed() < Duration::from_millis(350) {
            for _ in 0..2000 {
                g.record(&st, Duration::from_micros(4));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(g.inline_ok(&st));
    }

    #[test]
    fn lazy_loops_only_for_faas_unless_set() {
        use super::lazy_loops_from as f;
        assert!(!f(None, false));
        assert!(f(None, true));
        assert!(f(Some("true"), false));
        assert!(!f(Some("FALSE"), true));
        assert!(!f(Some("junk"), false));
    }

    /// Three long-lived connections on a 4-loop server do not all share a loop. With simoom's default
    /// (lazy) start the first three stay on loop 0, and an HTTP/2 client with four connections
    /// is served by one thread.
    #[test]
    fn long_lived_connections_spread_over_the_loops() {
        use std::io::{Read, Write};
        let service = |req: &mut simoom::Request<'_>, reply: simoom::Reply<'_>| -> simoom::Outcome { reply.send(simoom::Response::text(req.loop_index().to_string())) };
        // A lazy server puts the first three connections on loop 0 every time; with every loop
        // started they land on a random loop each, so a few attempts all failing is not chance.
        let spread = (0..6).any(|_| {
            let server = super::event_loops(simoom::Server::builder().threads(4).service(service)).listen("127.0.0.1:0").start().unwrap();
            let addr = server.local_addr();
            let mut conns: Vec<std::net::TcpStream> = (0..3).map(|_| std::net::TcpStream::connect(addr).unwrap()).collect();
            let mut loops = std::collections::BTreeSet::new();
            for c in conns.iter_mut() {
                c.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
                let mut buf = [0u8; 512];
                let n = c.read(&mut buf).unwrap();
                let text = String::from_utf8_lossy(&buf[..n]).to_string();
                loops.insert(text.rsplit("\r\n\r\n").next().unwrap().trim().to_string());
            }
            server.stop(std::time::Duration::from_millis(100));
            loops.len() >= 2
        });
        assert!(spread, "three long-lived connections stayed on one loop in six attempts");
    }

    #[test]
    fn a_route_is_promoted_after_a_fast_sample_and_demoted_when_it_stalls() {
        let g = Governor::new();
        let st = g.state(7);
        assert!(!g.inline_ok(&st), "a new route starts on the pool");
        // 126 of 128 calls under 1 ms: promoted
        for i in 0..SAMPLE {
            g.observe(&st, if i < 2 { Duration::from_millis(3) } else { Duration::from_micros(80) });
        }
        assert!(g.inline_ok(&st));
        // one call over 50 ms is plainly blocking: back to the pool, and not promoted again soon
        g.record(&st, Duration::from_millis(80));
        assert!(!g.inline_ok(&st));
        for _ in 0..SAMPLE {
            g.observe(&st, Duration::from_micros(80));
        }
        assert!(!g.inline_ok(&st), "a demoted route waits out its minute");
    }

    #[test]
    fn a_blocking_route_is_never_promoted() {
        let g = Governor::new();
        let st = g.state(8);
        for _ in 0..(SAMPLE * 4) {
            g.observe(&st, Duration::from_millis(20));
        }
        assert!(!g.inline_ok(&st));
    }

    #[test]
    fn three_stalls_in_a_second_demote_a_route() {
        let g = Governor::new();
        let st = g.state(9);
        for _ in 0..SAMPLE {
            g.observe(&st, Duration::from_micros(50));
        }
        assert!(g.inline_ok(&st));
        g.record(&st, Duration::from_millis(6));
        g.record(&st, Duration::from_millis(6));
        assert!(g.inline_ok(&st));
        g.record(&st, Duration::from_millis(6));
        assert!(!g.inline_ok(&st));
    }
}

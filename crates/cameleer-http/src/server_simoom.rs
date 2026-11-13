//! Spike: the simoom event-loop engine behind [`crate::server::HttpServer`].
//!
//! Selected at run time with `CAMEL_HTTP_ENGINE=simoom` (plain HTTP only; TLS listeners keep the
//! classic engine in this spike). Every request runs on simoom's offload pool, because Cameleer
//! handlers block; the loop threads only do I/O. See `docs/design/simoom-adoption.md`.

use std::io::{self, Read};
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use crate::http::{HttpHeaders, HttpRequest, HttpResponse, Version as HttpVersion};
use crate::server::{HttpHandler, ServerConfig};

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
        // Bodies over `body_buffer` stream; `serve` reads them whole (up to `max_body`) unless the
        // handler asks for a stream (`HttpHandler::stream_request`).
        limits.max_head = config.limits.max_header_bytes;
        limits.max_headers = config.limits.max_headers;
        limits.keep_alive_timeout = config.keep_alive_timeout;
        limits.write_timeout = config.write_timeout;
        limits.max_requests_per_connection = config.max_keep_alive_requests as u64;
        // Spike switch: run handlers on the loop thread (CAMEL_SIMOOM_INLINE=1) instead of the
        // offload pool, to measure the cost of the thread hop.
        let inline = std::env::var("CAMEL_SIMOOM_INLINE").is_ok();
        let service = move |req: &mut simoom::Request<'_>, reply: simoom::Reply<'_>| -> simoom::Outcome {
            // Routes that answer later (never block) run on the loop thread and answer from
            // whatever thread finishes the work: no worker is held while they wait.
            if !req.is_streamed() {
                let head = HttpRequest::new(&String::from_utf8_lossy(req.method()), &String::from_utf8_lossy(req.target()));
                if handler.is_async(&head) {
                    let r = build_request(req);
                    let head_request = r.method == "HEAD";
                    let responder = reply.defer();
                    handler.handle_async(r, Box::new(move |resp| complete(responder, resp, head_request)));
                    return simoom::Outcome::Deferred;
                }
            }
            if inline {
                return serve(&*handler, req, reply);
            }
            let h = handler.clone();
            reply.offload(req, move |req, reply| serve(&*h, req, reply))
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

fn serve(handler: &dyn HttpHandler, req: &mut simoom::Request<'_>, reply: simoom::Reply<'_>) -> simoom::Outcome {
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
    let has_body = req.is_streamed() || !req.body().is_empty();
    let mut stream_handle = None;
    if has_body && handler.stream_request(&r) {
        // hand the body over as a stream handle, as the classic server does
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
    let head = r.method == "HEAD";
    let resp = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler.handle(&mut r))) {
        Ok(resp) => resp,
        Err(_) => HttpResponse::text(500, "Internal Server Error"),
    };
    if let Some(h) = stream_handle {
        cameleer_tls::stream_body::release(&h);
    }
    send(reply, resp, head)
}

fn send(reply: simoom::Reply<'_>, mut resp: HttpResponse, head_request: bool) -> simoom::Outcome {
    let mut reply = reply.status(resp.status);
    for (k, v) in resp.headers.iter() {
        // framing is the server's business
        if k.eq_ignore_ascii_case("content-length") || k.eq_ignore_ascii_case("transfer-encoding") {
            continue;
        }
        reply = reply.header(k, v);
    }
    if let Some(handle) = resp.stream.take() {
        if let Some((mut reader, _len)) = cameleer_tls::stream_body::take(&handle) {
            if head_request {
                return reply.empty();
            }
            let mut sink = reply.stream();
            let mut buf = vec![0u8; 16 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if sink.write(&buf[..n]).is_err() {
                            sink.abort();
                            return simoom::Outcome::Done;
                        }
                    }
                    Err(_) => {
                        sink.abort();
                        return simoom::Outcome::Done;
                    }
                }
            }
            sink.finish();
            return simoom::Outcome::Done;
        }
    }
    if resp.chunked && !head_request && !resp.body.is_empty() {
        // `chunked=true` routes: the body goes out in 8 KiB chunks like the classic server
        let mut sink = reply.stream();
        for part in resp.body.chunks(8192) {
            if sink.write(part).is_err() {
                sink.abort();
                return simoom::Outcome::Done;
            }
        }
        sink.finish();
        return simoom::Outcome::Done;
    }
    reply.body(std::mem::take(&mut resp.body))
}

/// The head and the buffered body of a request.
fn build_request(req: &mut simoom::Request<'_>) -> HttpRequest {
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
    if !headers.contains("Host") {
        if let Some(h) = req.host() {
            headers.add("Host", String::from_utf8_lossy(h).into_owned());
        }
    }
    r.headers = headers;
    r.body = req.body().to_vec();
    r.remote_addr = req.peer_addr();
    r
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
    responder.send(out.body(std::mem::take(&mut resp.body)));
}

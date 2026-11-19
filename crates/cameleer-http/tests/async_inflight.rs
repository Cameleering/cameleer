//! The in-flight limit of asynchronous `platform-http` routes (`maxInflightRequests`,
//! docs/design/async-routing.md): beyond it the route answers 503 at once, and it recovers when
//! requests complete. Its own test binary: the other asynchronous routing test switches the engine
//! off for part of its run.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use cameleer_core::prelude::*;
use cameleer_http::HttpClient;

/// A backend that answers every request after `delay_ms` (thread per connection, keep-alive).
fn backend(delay_ms: u64) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            std::thread::spawn(move || serve(s, delay_ms));
        }
    });
    port
}

fn serve(s: TcpStream, delay_ms: u64) {
    let mut w = s.try_clone().unwrap();
    let mut r = BufReader::new(s);
    loop {
        let mut len = 0usize;
        let mut first = true;
        loop {
            let mut h = String::new();
            if r.read_line(&mut h).unwrap_or(0) == 0 {
                return;
            }
            if h == "\r\n" && !first {
                break;
            }
            first = false;
            if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                len = v.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0u8; len];
        if r.read_exact(&mut body).is_err() {
            return;
        }
        std::thread::sleep(Duration::from_millis(delay_ms));
        if w.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nslow").is_err() {
            return;
        }
    }
}

fn get(url: &str) -> (u16, Option<String>) {
    let r = HttpClient::new().request("GET", url, &[], None, Some(Duration::from_secs(20))).unwrap();
    (r.status, r.headers.get("Retry-After").map(str::to_string))
}

#[test]
fn an_asynchronous_route_answers_503_beyond_its_in_flight_limit_and_recovers() {
    let be = backend(1000);
    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    ctx.properties().set("camel.server.port", "0");
    // the context-wide default, and an endpoint that overrides it
    ctx.properties().set("camel.server.maxInflightRequests", "3");
    ctx.add_routes(move |b| {
        b.from("platform-http:/limited?maxInflightRequests=5").route_id("limited").to(format!("http://127.0.0.1:{be}?bridgeEndpoint=true"));
        b.from("platform-http:/default").route_id("default").to(format!("http://127.0.0.1:{be}?bridgeEndpoint=true"));
    })
    .unwrap();
    ctx.start().unwrap();
    let port = cameleer_http::platform_http_port(&ctx).unwrap();
    for (path, limit) in [("limited", 5), ("default", 3)] {
        let url = format!("http://127.0.0.1:{port}/{path}");
        // 20 requests at once to a backend that takes 1 s: `limit` are served, the rest refused
        let hs: Vec<_> = (0..20).map(|_| {
            let u = url.clone();
            std::thread::spawn(move || get(&u))
        }).collect();
        let answers: Vec<(u16, Option<String>)> = hs.into_iter().map(|h| h.join().unwrap()).collect();
        let ok = answers.iter().filter(|(s, _)| *s == 200).count();
        let refused: Vec<_> = answers.iter().filter(|(s, _)| *s == 503).collect();
        assert_eq!(ok, limit, "{path}: {answers:?}");
        assert_eq!(refused.len(), 20 - limit, "{path}: {answers:?}");
        assert!(refused.iter().all(|(_, ra)| ra.as_deref() == Some("1")), "503 carries Retry-After");
        // the slots came back: the route serves again
        assert_eq!(get(&url).0, 200, "{path} recovers");
    }
    ctx.stop().unwrap();
}

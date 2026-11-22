//! Asynchronous routing (docs/design/async-routing.md): `platform-http` routes whose `http` producer
//! waits without a thread. The same routes run with asynchronous routing turned off at the end and
//! must give the same answers.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cameleer_core::prelude::*;
use cameleer_core::spi::{Blocking, Processor};
use cameleer_http::{HttpClient, ServerConfig};

/// A thread-per-connection HTTP/1.1 backend (keep-alive), reached through bridge endpoints (the
/// request path is kept): `/proxy` answers `echo:<body>`, `/slow` waits 200 ms, `/redeliver` fails
/// with 500 twice out of three, anything else fails.
fn backend() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let flaky = Arc::new(AtomicUsize::new(0));
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(s) = s else { continue };
            let flaky = flaky.clone();
            std::thread::spawn(move || serve(s, &flaky));
        }
    });
    port
}

fn serve(s: TcpStream, flaky: &AtomicUsize) {
    let mut w = s.try_clone().unwrap();
    let mut r = BufReader::new(s);
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
        let mut len = 0usize;
        loop {
            let mut h = String::new();
            if r.read_line(&mut h).unwrap_or(0) == 0 {
                return;
            }
            if h == "\r\n" {
                break;
            }
            if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                len = v.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0u8; len];
        if r.read_exact(&mut body).is_err() {
            return;
        }
        let (status, text) = match path.split('?').next().unwrap_or("") {
            "/proxy" => (200, format!("echo:{}", String::from_utf8_lossy(&body))),
            "/slow" => {
                std::thread::sleep(Duration::from_millis(200));
                (200, "slow".to_string())
            }
            "/redeliver" if flaky.fetch_add(1, Ordering::SeqCst) % 3 < 2 => (500, "not yet".to_string()),
            "/redeliver" => (200, "finally".to_string()),
            _ => (500, "broken".to_string()),
        };
        let reply = format!("HTTP/1.1 {status} X\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{text}", text.len());
        if w.write_all(reply.as_bytes()).is_err() {
            return;
        }
    }
}

/// Records whether it ran on an event loop thread. It only computes, so an asynchronous route
/// runs it where the previous step completed.
struct Probe(Arc<Mutex<Vec<bool>>>);
impl Processor for Probe {
    fn process(&self, _: &mut Exchange) -> Result<()> {
        self.0.lock().unwrap().push(cameleer_core::reactive::on_loop());
        Ok(())
    }
    fn blocking(&self) -> Blocking {
        Blocking::Never
    }
}

/// A context whose platform server has `pool` offload threads.
fn context(pool: usize) -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    ctx.add_component("platform-http", cameleer_http::platform::HttpConsumerComponent::new("platform-http").with_port(0).with_server_config(ServerConfig { pool_size: pool, ..ServerConfig::default() }));
    ctx
}

fn post(url: &str, body: &str) -> (u16, String) {
    let r = HttpClient::new().request("POST", url, &[("Content-Type", "text/plain")], Some(body.as_bytes()), Some(Duration::from_secs(20))).unwrap();
    (r.status, r.body_text())
}

struct Run {
    proxy: (u16, String),
    loops: Vec<bool>,
    handled: (u16, String),
    redelivered: (u16, String),
    failed: u16,
    concurrent: Duration,
}

fn run_all() -> Run {
    let be = backend();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let ctx = context(2);
    let probe = seen.clone();
    ctx.add_routes(move |b| {
        b.from("platform-http:/proxy")
            .route_id("proxy")
            .set_header("X-Step", simple("one"))
            .to(format!("http://127.0.0.1:{be}?bridgeEndpoint=true"))
            .process(Probe(probe.clone()))
            .set_body(simple("${body}|${header.X-Step}"));
        b.from("platform-http:/handled")
            .route_id("handled")
            .on_exception(&["HttpOperationFailedException"])
            .handled(true)
            .set_body(simple("handled ${exception.message}"))
            .end()
            .to(format!("http://127.0.0.1:{be}?bridgeEndpoint=true"));
        b.from("platform-http:/redeliver")
            .route_id("redeliver")
            .error_handler(default_error_handler().maximum_redeliveries(3).redelivery_delay(0))
            .to(format!("http://127.0.0.1:{be}?bridgeEndpoint=true"));
        b.from("platform-http:/fail").route_id("fail").to(format!("http://127.0.0.1:{be}?bridgeEndpoint=true"));
        b.from("platform-http:/slow").route_id("slow").to(format!("http://127.0.0.1:{be}?bridgeEndpoint=true"));
    })
    .unwrap();
    ctx.start().unwrap();
    let port = cameleer_http::platform_http_port(&ctx).unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let proxy = post(&format!("{base}/proxy"), "hi");
    let handled = post(&format!("{base}/handled"), "x");
    let redelivered = post(&format!("{base}/redeliver"), "x");
    let failed = post(&format!("{base}/fail"), "x").0;
    // 40 requests to a backend that takes 200 ms, through a server with 2 offload threads
    let t0 = Instant::now();
    let hs: Vec<_> = (0..40).map(|_| {
        let u = format!("{base}/slow");
        std::thread::spawn(move || post(&u, "x"))
    }).collect();
    for h in hs {
        assert_eq!(h.join().unwrap(), (200, "slow".to_string()));
    }
    let concurrent = t0.elapsed();
    ctx.stop().unwrap();
    let loops = seen.lock().unwrap().clone();
    Run { proxy, loops, handled, redelivered, failed, concurrent }
}

#[test]
fn asynchronous_routes_answer_like_synchronous_ones_without_holding_threads() {
    // asynchronous (the default)
    let a = run_all();
    assert_eq!(a.proxy, (200, "echo:hi|one".to_string()));
    assert_eq!(a.loops, vec![true], "the step after the http call ran on the client's event loop");
    // the producer set CamelHttpResponseCode before it failed, as in Camel: the handled answer keeps it
    assert_eq!(a.handled.0, 500);
    assert!(a.handled.1.starts_with("handled HTTP operation failed"), "{}", a.handled.1);
    assert_eq!(a.redelivered, (200, "finally".to_string()));
    assert_eq!(a.failed, 500);
    // 40 x 200 ms with 2 threads would take 4 s if each request held a thread
    assert!(a.concurrent < Duration::from_millis(2500), "took {:?}", a.concurrent);

    // the same routes on the synchronous path
    cameleer_core::reactive::set_enabled(false);
    let s = run_all();
    cameleer_core::reactive::set_enabled(true);
    assert_eq!(s.proxy, a.proxy);
    assert_eq!(s.loops, vec![false]);
    // the backends run on different ports
    let unport = |t: &str| t.split(':').map(|p| p.trim_start_matches(|c: char| c.is_ascii_digit())).collect::<Vec<_>>().join(":");
    assert_eq!((s.handled.0, unport(&s.handled.1)), (a.handled.0, unport(&a.handled.1)));
    assert_eq!(s.redelivered, a.redelivered);
    assert_eq!(s.failed, a.failed);
    assert!(s.concurrent > Duration::from_millis(3500), "the synchronous path holds a thread per request: {:?}", s.concurrent);
}

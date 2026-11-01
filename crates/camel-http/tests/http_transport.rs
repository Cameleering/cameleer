//! `http` producer transport: keep-alive pooling, content coding and HTTP/2.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use camel_core::prelude::*;
use camel_grpc::h2::{H2Config, H2Server, H2Stream, ServerRequest};

/// A tiny HTTP/1.1 keep-alive server: counts connections, echoes request headers it was
/// asked about and answers gzip when the client accepts it.
fn raw_server() -> (u16, Arc<AtomicUsize>, Arc<Mutex<Vec<String>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let conns = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (c2, s2) = (conns.clone(), seen.clone());
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(s) = s else { continue };
            c2.fetch_add(1, Ordering::SeqCst);
            let seen = s2.clone();
            std::thread::spawn(move || {
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut w = s;
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let mut headers = Vec::new();
                    loop {
                        let mut h = String::new();
                        r.read_line(&mut h).unwrap();
                        let h = h.trim_end().to_string();
                        if h.is_empty() {
                            break;
                        }
                        headers.push(h);
                    }
                    let get = |n: &str| headers.iter().find(|h| h.to_ascii_lowercase().starts_with(&format!("{}:", n))).map(|h| h.split_once(':').unwrap().1.trim().to_string());
                    let len: usize = get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
                    let mut body = vec![0u8; len];
                    r.read_exact(&mut body).unwrap();
                    let enc = get("content-encoding").unwrap_or_default();
                    let body = if enc == "gzip" { camel_core::dataformat::compress::gzip_decompress(&body).unwrap() } else { body };
                    seen.lock().unwrap().push(format!("{} {} enc={} body={}", line.trim(), get("accept-encoding").unwrap_or_default(), enc, String::from_utf8_lossy(&body)));
                    let text = format!("reply to {}", String::from_utf8_lossy(&body));
                    let gzip = get("accept-encoding").map(|a| a.contains("gzip")).unwrap_or(false);
                    let (payload, ce) = if gzip { (camel_core::dataformat::compress::gzip_compress(text.as_bytes(), 6), "Content-Encoding: gzip\r\n") } else { (text.into_bytes(), "") };
                    let head = format!("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n{}Content-Length: {}\r\n\r\n", ce, payload.len());
                    if w.write_all(head.as_bytes()).and_then(|_| w.write_all(&payload)).is_err() {
                        return;
                    }
                }
            });
        }
    });
    (port, conns, seen)
}

#[test]
fn pooling_and_content_coding() {
    let (port, conns, seen) = raw_server();
    let ctx = CamelContext::new();
    camel_http::register(&ctx);
    let t = ctx.create_producer_template();
    let uri = format!("http://127.0.0.1:{}/echo", port);
    for i in 0..5 {
        let out = t.request_body(&uri, format!("m{}", i)).unwrap();
        assert_eq!(out.to_string(), format!("reply to m{}", i));
    }
    // one pooled keep-alive connection, gzip decoded transparently
    assert_eq!(conns.load(Ordering::SeqCst), 1);
    assert!(seen.lock().unwrap()[0].contains("gzip, deflate"));
    // gzip request bodies
    let out = t.request_body_and_header(&uri, "squeeze me", "Content-Encoding", "gzip").unwrap();
    assert_eq!(out.to_string(), "reply to squeeze me");
    assert!(seen.lock().unwrap().last().unwrap().contains("enc=gzip body=squeeze me"));
    // no pooling and no compression
    let plain = format!("{}?connectionClose=true&contentCompressionDisabled=true", uri);
    let before = conns.load(Ordering::SeqCst);
    t.request_body(&plain, "a").unwrap();
    t.request_body(&plain, "b").unwrap();
    assert_eq!(conns.load(Ordering::SeqCst), before + 2);
    assert!(!seen.lock().unwrap().last().unwrap().contains("gzip"));
    assert!(ctx.get_endpoint(&format!("{}?httpVersion=3", uri)).is_err());
}

#[test]
fn http2_prior_knowledge() {
    let streams = Arc::new(AtomicUsize::new(0));
    let s2 = streams.clone();
    let server = H2Server::bind("127.0.0.1:0", H2Config::default(), None, move |req: ServerRequest, stream: H2Stream| {
        s2.fetch_add(1, Ordering::SeqCst);
        let body = stream.read_body(None).unwrap_or_default();
        let text = format!("{} {} {} ua={}", req.method, req.path, String::from_utf8_lossy(&body), req.header("user-agent").unwrap_or(""));
        let _ = ServerRequest::respond(&stream, 201, &[("content-type", "text/plain"), ("x-proto", "h2")], text.as_bytes());
    })
    .unwrap();
    let port = server.local_addr().port();
    let ctx = CamelContext::new();
    camel_http::register(&ctx);
    let t = ctx.create_producer_template();
    let uri = format!("http://127.0.0.1:{}/v2/items?httpVersion=HTTP/2&q=1", port);
    for i in 0..3 {
        let ex = t.request(&uri, |e| e.set_body(format!("b{}", i))).unwrap();
        assert_eq!(ex.header("CamelHttpResponseCode").unwrap().as_i64(), Some(201));
        assert_eq!(ex.header("x-proto").map(|v| v.to_string()), Some("h2".to_string()));
        assert!(ex.body().to_string().starts_with(&format!("POST /v2/items?q=1 b{} ua=rust-camel", i)), "{}", ex.body());
    }
    assert_eq!(streams.load(Ordering::SeqCst), 3);
    server.stop_graceful(std::time::Duration::from_millis(100));
}

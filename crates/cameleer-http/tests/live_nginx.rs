//! Live interop: the `http` / `https` producer against a real nginx (`scripts/live-interop.sh`):
//! HTTP/1.1 GET/POST, status handling, redirects, gzip content coding, keep-alive pooling,
//! HTTPS verified against the interop CA, HTTP/2 over TLS (ALPN) and h2c. Runs only with
//! `CAMEL_LIVE_INTEROP=1`.

use cameleer_core::prelude::*;
use cameleer_tls::testing::live;

const HTTP: &str = "http://127.0.0.1:18080";
const HTTPS: &str = "https://localhost:18443";
const H2C: &str = "http://127.0.0.1:18081";

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    live::bind_beans(&ctx);
    ctx.start().unwrap();
    ctx
}

fn big() -> String {
    std::fs::read_to_string(live::dir().join("www/big.txt")).unwrap()
}

fn s(ex: &Exchange, h: &str) -> String {
    ex.header(h).map(|v| v.to_string()).unwrap_or_default()
}

#[test]
fn live_nginx_http11_get_post_status_and_redirects() {
    if !live::enabled() {
        return;
    }
    let ctx = context();
    let t = ctx.create_producer_template();
    let ex = t.request(&format!("{}/hello.txt", HTTP), |ex| ex.set_header("CamelHttpMethod", "GET")).unwrap();
    assert_eq!(ex.body_as::<String>().unwrap(), "hello from nginx\n");
    assert_eq!(ex.header("CamelHttpResponseCode").and_then(|v| v.as_i64()), Some(200));
    assert!(s(&ex, "Server").starts_with("nginx"), "{:?}", ex.message.headers);
    assert!(s(&ex, "Content-Type").starts_with("text/plain"));
    // POST with a body, custom header and forwarded query parameters
    let ex = t
        .request(&format!("{}/echo?a=1&b=two", HTTP), |ex| {
            ex.set_body("posted body ✓");
            ex.set_header("X-Custom", "value-1");
            ex.set_header("Content-Type", "text/plain; charset=UTF-8");
            ex.set_header("CamelSecret", "not sent");
        })
        .unwrap();
    assert_eq!(ex.body_as::<String>().unwrap(), "posted body ✓");
    assert_eq!(s(&ex, "X-Request-Method"), "POST");
    assert_eq!(s(&ex, "X-Request-Query"), "a=1&b=two");
    assert_eq!(s(&ex, "X-Request-Custom"), "value-1");
    assert_eq!(s(&ex, "X-Request-Ctype"), "text/plain; charset=UTF-8");
    // a 1 MiB binary body (nginx buffers it to a temp file)
    let payload: Vec<u8> = (0..1_048_576u32).map(|i| (i * 7 % 256) as u8).collect();
    let ex = t
        .request(&format!("{}/echo", HTTP), |ex| {
            ex.set_body(Value::Bytes(payload.clone()));
            ex.set_header("Content-Type", "application/octet-stream");
        })
        .unwrap();
    assert_eq!(ex.body().to_bytes(), payload);
    // PUT and DELETE methods reach the server
    for m in ["PUT", "DELETE", "PATCH"] {
        let ex = t.request(&format!("{}/echo?httpMethod={}", HTTP, m), |ex| ex.set_body("x")).unwrap();
        assert_eq!(s(&ex, "X-Request-Method"), m);
    }
    // 404: HttpOperationFailedException with the status, or the code with throwExceptionOnFailure=false
    let err = t.request(&format!("{}/status/missing", HTTP), |ex| ex.set_header("CamelHttpMethod", "GET")).unwrap_err();
    let cause = err.cause.as_deref().unwrap_or(&err);
    assert_eq!(cause.kind, "HttpOperationFailedException", "{:?}", err);
    assert!(cause.message.contains("404"), "{}", cause.message);
    let ex = t.request(&format!("{}/status/missing?throwExceptionOnFailure=false", HTTP), |ex| ex.set_header("CamelHttpMethod", "GET")).unwrap();
    assert_eq!(ex.header("CamelHttpResponseCode").and_then(|v| v.as_i64()), Some(404));
    assert_eq!(ex.body_as::<String>().unwrap(), "not here\n");
    // redirects: followed on request, else a 302 failure
    let ex = t.request(&format!("{}/redirect?followRedirects=true", HTTP), |ex| ex.set_header("CamelHttpMethod", "GET")).unwrap();
    assert_eq!(ex.body_as::<String>().unwrap(), "hello from nginx\n");
    let ex = t.request(&format!("{}/redirect?throwExceptionOnFailure=false", HTTP), |ex| ex.set_header("CamelHttpMethod", "GET")).unwrap();
    assert_eq!(ex.header("CamelHttpResponseCode").and_then(|v| v.as_i64()), Some(302));
    assert!(s(&ex, "Location").ends_with("/hello.txt"));
    ctx.stop().unwrap();
}

#[test]
fn live_nginx_gzip_and_keep_alive_pooling() {
    if !live::enabled() {
        return;
    }
    let ctx = context();
    let t = ctx.create_producer_template();
    // nginx gzips text/plain for clients that accept it; the producer decodes it
    let marker = format!("gz{}", std::process::id());
    let ex = t.request(&format!("{}/big.txt?m={}", HTTP, marker), |ex| ex.set_header("CamelHttpMethod", "GET")).unwrap();
    assert_eq!(ex.body_as::<String>().unwrap(), big());
    assert!(ex.header("Content-Encoding").is_none(), "decoded: {:?}", ex.message.headers);
    // nginx's access log (combined format: status then body bytes) proves it was compressed
    // nginx writes the access-log line after the response has been sent: poll briefly for it
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let line = loop {
        let log = std::fs::read_to_string(live::dir().join("logs/nginx-access.log")).unwrap_or_default();
        if let Some(l) = log.lines().rev().find(|l| l.contains(&marker)) {
            break l.to_string();
        }
        assert!(std::time::Instant::now() < deadline, "access log line for {} not written", marker);
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let sent: usize = line.split("\" ").nth(1).and_then(|r| r.split_whitespace().nth(1)).and_then(|n| n.parse().ok()).expect("bytes sent");
    assert!(sent < big().len() / 4, "gzip on the wire: {} of {} bytes ({})", sent, big().len(), line);
    // with content coding disabled nothing is negotiated: identity body
    let ex = t.request(&format!("{}/big.txt?contentCompressionDisabled=true", HTTP), |ex| ex.set_header("CamelHttpMethod", "GET")).unwrap();
    assert_eq!(ex.body_as::<String>().unwrap(), big());
    // JSON is compressed too
    let ex = t.request(&format!("{}/data.json", HTTP), |ex| ex.set_header("CamelHttpMethod", "GET")).unwrap();
    assert_eq!(ex.body_as::<String>().unwrap(), r#"{"server":"nginx","ok":true}"#);

    // keep-alive: one producer reuses its pooled connection (nginx's $connection serial)
    let uri = format!("{}/echo", HTTP);
    let conn = |t: &ProducerTemplate, uri: &str| t.request(uri, |ex| ex.set_body("k")).unwrap().header("X-Connection").map(|v| v.to_string()).unwrap();
    let first = conn(&t, &uri);
    for _ in 0..5 {
        assert_eq!(conn(&t, &uri), first, "the pooled keep-alive connection is reused");
    }
    let closed = format!("{}/echo?connectionClose=true", HTTP);
    let a = conn(&t, &closed);
    let b = conn(&t, &closed);
    assert_ne!(a, b, "connectionClose=true opens a connection per request");
    ctx.stop().unwrap();
}

#[test]
fn live_nginx_https_verification() {
    if !live::enabled() {
        return;
    }
    let ctx = context();
    let t = ctx.create_producer_template();
    for base in [HTTPS.to_string(), "https://127.0.0.1:18443".to_string()] {
        let ex = t.request(&format!("{}/hello.txt?sslContextParameters=#liveSsl", base), |ex| ex.set_header("CamelHttpMethod", "GET")).unwrap_or_else(|e| panic!("{}: {}", base, e));
        assert_eq!(ex.body_as::<String>().unwrap(), "hello from nginx\n");
        let ex = t.request(&format!("{}/echo?sslContextParameters=#liveSsl", base), |ex| ex.set_body("over tls")).unwrap();
        assert_eq!(ex.body_as::<String>().unwrap(), "over tls");
        assert_eq!(s(&ex, "X-Protocol"), "HTTP/1.1");
    }
    // gzip and keep-alive over TLS
    let ex = t.request(&format!("{}/big.txt?sslContextParameters=#liveSsl", HTTPS), |ex| ex.set_header("CamelHttpMethod", "GET")).unwrap();
    assert_eq!(ex.body_as::<String>().unwrap(), big());
    let uri = format!("{}/echo?sslContextParameters=#liveSsl", HTTPS);
    let c1 = t.request(&uri, |ex| ex.set_body("1")).unwrap().header("X-Connection").map(|v| v.to_string());
    let c2 = t.request(&uri, |ex| ex.set_body("2")).unwrap().header("X-Connection").map(|v| v.to_string());
    assert_eq!(c1, c2, "TLS keep-alive connection reused");
    // an untrusted chain fails
    let err = t.request(&format!("{}/hello.txt?sslContextParameters=#liveUntrusted", HTTPS), |ex| ex.set_header("CamelHttpMethod", "GET")).unwrap_err();
    assert!(err.chain().any(|c| c.is_a("SSLException") || c.is_a("IOException")), "{:?}", err);
    ctx.stop().unwrap();
}

#[test]
fn live_nginx_http2_over_tls_and_h2c() {
    if !live::enabled() {
        return;
    }
    let ctx = context();
    let t = ctx.create_producer_template();
    let h2 = "httpVersion=HTTP/2&sslContextParameters=#liveSsl";
    // ALPN negotiates h2 with nginx: $server_protocol is HTTP/2.0
    let ex = t.request(&format!("{}/proto?{}", HTTPS, h2), |ex| ex.set_header("CamelHttpMethod", "GET")).unwrap_or_else(|e| panic!("{}", e));
    let body = ex.body_as::<String>().unwrap();
    assert!(body.starts_with("HTTP/2.0 on"), "{}", body);
    assert_eq!(ex.header("CamelHttpResponseCode").and_then(|v| v.as_i64()), Some(200));
    // POST over h2 (DATA frames), headers, query
    let ex = t
        .request(&format!("{}/echo?q=1&{}", HTTPS, h2), |ex| {
            ex.set_body("h2 body ✓");
            ex.set_header("X-Custom", "h2-value");
            ex.set_header("Content-Type", "text/plain");
        })
        .unwrap();
    assert_eq!(ex.body_as::<String>().unwrap(), "h2 body ✓");
    assert_eq!(s(&ex, "x-request-custom"), "h2-value");
    // endpoint options are consumed, other query parameters are forwarded
    assert_eq!(s(&ex, "x-request-query"), "q=1");
    assert_eq!(s(&ex, "x-protocol"), "HTTP/2.0");
    // a large body across many DATA frames (flow control windows), and a large response
    let payload: Vec<u8> = (0..300_000u32).map(|i| (i % 253) as u8).collect();
    let ex = t
        .request(&format!("{}/echo?{}", HTTPS, h2), |ex| {
            ex.set_body(Value::Bytes(payload.clone()));
            ex.set_header("Content-Type", "application/octet-stream");
        })
        .unwrap();
    assert_eq!(ex.body().to_bytes(), payload);
    let ex = t.request(&format!("{}/big.txt?{}", HTTPS, h2), |ex| ex.set_header("CamelHttpMethod", "GET")).unwrap();
    assert_eq!(ex.body_as::<String>().unwrap(), big(), "gzip over h2 decoded");
    // multiplexing: all requests of one producer share nginx's single connection
    let uri = format!("{}/proto?{}", HTTPS, h2);
    let ids: Vec<String> = (0..5).map(|_| t.request(&uri, |ex| ex.set_header("CamelHttpMethod", "GET")).unwrap().body_as::<String>().unwrap()).collect();
    assert!(ids.windows(2).all(|w| w[0] == w[1]), "{:?}", ids);
    // errors over h2
    let err = t.request(&format!("{}/status/x?{}", HTTPS, h2), |ex| ex.set_header("CamelHttpMethod", "GET")).unwrap_err();
    assert!(format!("{:?}", err).contains("404"), "{:?}", err);
    // h2c with prior knowledge
    let ex = t.request(&format!("{}/proto?httpVersion=HTTP/2", H2C), |ex| ex.set_header("CamelHttpMethod", "GET")).unwrap();
    assert!(ex.body_as::<String>().unwrap().starts_with("HTTP/2.0"), "{:?}", ex.body());
    let ex = t.request(&format!("{}/echo?httpVersion=HTTP/2", H2C), |ex| ex.set_body("h2c")).unwrap();
    assert_eq!(ex.body_as::<String>().unwrap(), "h2c");
    ctx.stop().unwrap();
}

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use camel_core::prelude::*;
use camel_http::http::HttpResponse;
use camel_http::{HttpClient, HttpConnection, HttpOperationFailed};

/// A context with camel-http registered and the platform server on an ephemeral port.
fn context() -> CamelContext {
    let ctx = CamelContext::new();
    camel_http::register(&ctx);
    ctx.properties().set("camel.server.port", "0");
    ctx
}

fn port(ctx: &CamelContext) -> u16 {
    camel_http::platform_http_port(ctx).expect("platform-http port")
}

fn get(url: &str) -> HttpResponse {
    HttpClient::new().request("GET", url, &[], None, Some(Duration::from_secs(10))).unwrap()
}

/// Send raw bytes and read the full reply until the server closes the connection.
fn raw(port: u16, request: &str) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(request.as_bytes()).unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn get_and_post_round_trip() {
    let ctx = context();
    ctx.add_routes(|b| {
        b.from("platform-http:/hello").route_id("hello").process(|ex: &mut Exchange| {
            let method: String = ex.message.header_as("CamelHttpMethod")?.unwrap();
            let body = match ex.body() {
                Value::Null => "nobody".to_string(),
                v => v.to_string(),
            };
            ex.set_body(format!("{} {}", method, body));
            ex.set_header("Content-Type", "text/plain");
            ex.set_header("X-Reply", "yes");
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let p = port(&ctx);
    let url = format!("http://127.0.0.1:{}/hello", p);

    let r = get(&url);
    assert_eq!(r.status, 200);
    assert_eq!(r.body_text(), "GET nobody");
    assert_eq!(r.headers.get("X-Reply"), Some("yes"));
    assert_eq!(r.headers.get("Content-Type"), Some("text/plain"));
    assert!(r.headers.get("CamelHttpMethod").is_none(), "Camel headers must be filtered");

    let r = HttpClient::new().post(&url, "text/plain", b"payload").unwrap();
    assert_eq!(r.body_text(), "POST payload");

    // through the http producer
    let t = ctx.create_producer_template();
    let reply = t.request_body(&format!("http://127.0.0.1:{}/hello", p), "from producer").unwrap();
    assert_eq!(reply, Value::from("POST from producer"));
    let reply = t.request_body(&format!("http://127.0.0.1:{}/hello", p), Value::Null).unwrap();
    assert_eq!(reply, Value::from("GET nobody"));
    let ex = t.request(&format!("http://127.0.0.1:{}/hello?httpMethod=PUT", p), |ex| ex.set_body("x")).unwrap();
    assert_eq!(ex.body(), &Value::from("PUT x"));
    assert_eq!(ex.header("CamelHttpResponseCode"), Some(&Value::Int(200)));
    assert_eq!(ex.header("CamelHttpResponseText"), Some(&Value::from("OK")));
    assert_eq!(ex.header("X-Reply"), Some(&Value::from("yes")));

    ctx.stop().unwrap();
    assert!(HttpClient { connect_timeout: Duration::from_millis(500), ..HttpClient::new() }.get(&url).is_err());
}

#[test]
fn path_and_query_params() {
    let ctx = context();
    ctx.add_routes(|b| {
        b.from("platform-http:/users/{id}/orders/{order}").process(|ex: &mut Exchange| {
            let id = ex.header("id").map(|v| v.to_string()).unwrap_or_default();
            let order = ex.header("order").map(|v| v.to_string()).unwrap_or_default();
            let q = ex.header("q").map(|v| v.to_string()).unwrap_or_default();
            let tags = ex.header("tag").cloned().unwrap_or_default();
            let path = ex.header("CamelHttpPath").map(|v| v.to_string()).unwrap_or_default();
            let raw = ex.header("CamelHttpRawQuery").map(|v| v.to_string()).unwrap_or_default();
            ex.set_body(format!("id={} order={} q={} tags={} path={} raw={}", id, order, q, tags.to_json(), path, raw));
            Ok(())
        });
        b.from("platform-http:/static?matchOnUriPrefix=true").process(|ex: &mut Exchange| {
            let p = ex.header("CamelHttpPath").map(|v| v.to_string()).unwrap_or_default();
            ex.set_body(format!("static {}", p));
            Ok(())
        });
        b.from("platform-http:/form?httpMethodRestrict=POST").process(|ex: &mut Exchange| {
            let a = ex.header("a").map(|v| v.to_string()).unwrap_or_default();
            ex.set_body(format!("a={}", a));
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let p = port(&ctx);
    let r = get(&format!("http://127.0.0.1:{}/users/42/orders/a%20b?q=hello+world&tag=x&tag=y", p));
    assert_eq!(r.status, 200);
    assert_eq!(
        r.body_text(),
        "id=42 order=a b q=hello world tags=[\"x\",\"y\"] path=/users/42/orders/a%20b raw=q=hello+world&tag=x&tag=y"
    );
    assert_eq!(get(&format!("http://127.0.0.1:{}/static/css/site.css", p)).body_text(), "static /static/css/site.css");
    assert_eq!(get(&format!("http://127.0.0.1:{}/static", p)).body_text(), "static /static");
    let r = HttpClient::new().post(&format!("http://127.0.0.1:{}/form", p), "application/x-www-form-urlencoded", b"a=1%202&b=3").unwrap();
    assert_eq!(r.body_text(), "a=1 2");
    ctx.stop().unwrap();
}

#[test]
fn status_codes_404_405_and_errors() {
    let ctx = context();
    ctx.add_routes(|b| {
        b.from("platform-http:/created?httpMethodRestrict=POST,PUT").process(|ex: &mut Exchange| {
            ex.set_header("CamelHttpResponseCode", 201);
            ex.set_body("made");
            Ok(())
        });
        b.from("platform-http:/empty").process(|ex: &mut Exchange| {
            ex.set_body(Value::Null);
            Ok(())
        });
        b.from("platform-http:/boom").process(|_ex: &mut Exchange| Err(CamelError::illegal_argument("kaboom")));
        b.from("platform-http:/muted?muteException=true").process(|_ex: &mut Exchange| Err(CamelError::illegal_argument("secret")));
        b.from("platform-http:/teapot").process(|ex: &mut Exchange| {
            ex.set_header("CamelHttpResponseCode", 418);
            ex.set_body("short and stout");
            Ok(())
        });
        b.from("platform-http:/json").process(|ex: &mut Exchange| {
            ex.set_body(Value::object([("name", Value::from("camel")), ("legs", Value::from(4))]));
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let p = port(&ctx);
    let base = format!("http://127.0.0.1:{}", p);
    let c = HttpClient::new();
    let r = c.request("POST", &format!("{}/created", base), &[], Some(b"x"), None).unwrap();
    assert_eq!((r.status, r.body_text().as_str()), (201, "made"));
    let r = get(&format!("{}/created", base));
    assert_eq!(r.status, 405);
    assert_eq!(r.headers.get("Allow"), Some("POST, PUT"));
    assert_eq!(get(&format!("{}/nothing-here", base)).status, 404);
    assert_eq!(get(&format!("{}/empty", base)).status, 204);
    let r = get(&format!("{}/boom", base));
    assert_eq!(r.status, 500);
    assert!(r.body_text().contains("kaboom"), "{}", r.body_text());
    let r = get(&format!("{}/muted", base));
    assert_eq!(r.status, 500);
    assert!(r.body.is_empty());
    let r = get(&format!("{}/json", base));
    assert_eq!(r.headers.get("Content-Type"), Some("application/json"));
    assert_eq!(camel_core::json::parse(&r.body_text()).unwrap().get("legs"), Some(&Value::from(4)));

    // http producer: throwExceptionOnFailure
    let t = ctx.create_producer_template();
    let mut ex = Exchange::new(Some(ctx.clone()));
    ex.pattern = ExchangePattern::InOut;
    let ex = t.send(&format!("{}/teapot", base), ex);
    let err = ex.exception.clone().expect("should fail");
    assert_eq!(err.kind, kinds::HTTP_OPERATION_FAILED);
    assert!(err.is_a(kinds::CAMEL_EXCHANGE));
    assert_eq!(HttpOperationFailed::status_code_of(&err), Some(418));
    let info = HttpOperationFailed::from_exchange(&ex).unwrap();
    assert_eq!(info.status_code, 418);
    assert_eq!(info.response_body.as_deref(), Some("short and stout"));
    assert_eq!(ex.body(), &Value::from("short and stout"));
    let r = t.request_body(&format!("{}/teapot?throwExceptionOnFailure=false", base), Value::Null).unwrap();
    assert_eq!(r, Value::from("short and stout"));
    let ex = t.request(&format!("{}/nothing?throwExceptionOnFailure=false", base), |_| {}).unwrap();
    assert_eq!(ex.header("CamelHttpResponseCode"), Some(&Value::Int(404)));
    let r = t.request_body(&format!("{}/teapot?okStatusCodeRange=200-299,418", base), Value::Null).unwrap();
    assert_eq!(r, Value::from("short and stout"));
    ctx.stop().unwrap();
}

#[test]
fn producer_headers_query_forwarding_and_https() {
    let ctx = context();
    ctx.add_routes(|b| {
        b.from("platform-http:/echo?matchOnUriPrefix=true").process(|ex: &mut Exchange| {
            let s = format!(
                "{} {} q={} custom={} ct={} camel={}",
                ex.header("CamelHttpMethod").unwrap(),
                ex.header("CamelHttpUri").unwrap(),
                ex.header("CamelHttpRawQuery").map(|v| v.to_string()).unwrap_or_default(),
                ex.header("X-Custom").map(|v| v.to_string()).unwrap_or_default(),
                ex.header("Content-Type").map(|v| v.to_string()).unwrap_or_default(),
                ex.header("CamelSecret").is_some()
            );
            ex.message.headers.clear();
            ex.set_body(s);
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let p = port(&ctx);
    let t = ctx.create_producer_template();
    // unknown options are forwarded as query parameters
    let r = t
        .request_body_and_headers(
            &format!("http://127.0.0.1:{}/echo?foo=bar&throwExceptionOnFailure=true&connectTimeout=5000", p),
            "hi",
            vec![("X-Custom", Value::from("c1")), ("CamelSecret", Value::from("s"))],
        )
        .unwrap();
    assert_eq!(r, Value::from("POST /echo?foo=bar q=foo=bar custom=c1 ct= camel=false"));
    // header overrides
    let r = t
        .request_body_and_headers(
            &format!("http://127.0.0.1:{}/echo", p),
            Value::Null,
            vec![("CamelHttpMethod", Value::from("DELETE")), ("CamelHttpPath", Value::from("sub/x")), ("CamelHttpQuery", Value::from("a=1"))],
        )
        .unwrap();
    assert_eq!(r, Value::from("DELETE /echo/sub/x?a=1 q=a=1 custom= ct= camel=false"));
    // structured body is sent as JSON
    let r = t.request_body(&format!("http://127.0.0.1:{}/echo", p), Value::object([("a", 1)])).unwrap();
    assert_eq!(r, Value::from("POST /echo q= custom= ct=application/json camel=false"));
    // CamelHttpUri override (ignored with bridgeEndpoint)
    let other = format!("http://127.0.0.1:{}/echo/other", p);
    let r = t.request_body_and_header(&format!("http://127.0.0.1:{}/echo", p), Value::Null, "CamelHttpUri", other.clone()).unwrap();
    assert!(r.to_string().starts_with("GET /echo/other"), "{}", r);
    let r = t.request_body_and_header(&format!("http://127.0.0.1:{}/echo?bridgeEndpoint=true", p), Value::Null, "CamelHttpUri", other).unwrap();
    assert!(r.to_string().starts_with("GET /echo "), "{}", r);

    // https is rejected
    let err = t.request_body("https://127.0.0.1:1/x", "x").unwrap_err();
    assert!(format!("{}", err).contains("HTTPS is not supported"), "{}", err);
    // connection refused
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let err = t.request_body(&format!("http://127.0.0.1:{}/x", free), "x").unwrap_err();
    assert_eq!(err.root_cause().kind, kinds::CONNECT);
    ctx.stop().unwrap();
}

#[test]
fn chunked_responses_and_requests_on_the_wire() {
    let ctx = context();
    let big = "0123456789".repeat(2000);
    let big2 = big.clone();
    ctx.add_routes(move |b| {
        let big = big2.clone();
        b.from("platform-http:/chunked?chunked=true").process(move |ex: &mut Exchange| {
            ex.set_body(big.clone());
            Ok(())
        });
        b.from("platform-http:/len").process(|ex: &mut Exchange| {
            let n = ex.body().to_bytes().len();
            ex.set_body(format!("len={}", n));
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let p = port(&ctx);
    let out = raw(p, "GET /chunked HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    assert!(out.starts_with("HTTP/1.1 200 OK\r\n"), "{}", &out[..60.min(out.len())]);
    assert!(out.contains("Transfer-Encoding: chunked\r\n"));
    assert!(!out.contains("Content-Length"));
    assert!(out.contains("\r\n2000\r\n"), "8192-byte chunks expected");
    assert!(out.ends_with("\r\n0\r\n\r\n"));
    let r = get(&format!("http://127.0.0.1:{}/chunked", p));
    assert!(r.chunked);
    assert_eq!(r.body_text(), big);
    // HTTP/1.0 client: no chunking
    let out = raw(p, "GET /chunked HTTP/1.0\r\n\r\n");
    assert!(out.starts_with("HTTP/1.0 200 OK\r\n"));
    assert!(out.contains(&format!("Content-Length: {}\r\n", big.len())));
    // chunked request body + Expect: 100-continue
    let out = raw(
        p,
        "POST /len HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\nExpect: 100-continue\r\nConnection: close\r\n\r\n4\r\nabcd\r\n3\r\nefg\r\n0\r\n\r\n",
    );
    assert!(out.starts_with("HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\n"), "{}", out);
    assert!(out.ends_with("len=7"), "{}", out);
    // the client can send chunked requests as well
    let c = HttpClient { chunked_requests: true, ..HttpClient::new() };
    let r = c.request("POST", &format!("http://127.0.0.1:{}/len", p), &[], Some(big.as_bytes()), None).unwrap();
    assert_eq!(r.body_text(), format!("len={}", big.len()));
    ctx.stop().unwrap();
}

#[test]
fn keep_alive_and_malformed_requests() {
    let ctx = context();
    ctx.add_routes(|b| {
        b.from("platform-http:/ka").process(|ex: &mut Exchange| {
            let b = ex.body().to_string();
            ex.message.headers.clear();
            ex.set_body(format!("got:{}", b));
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let p = port(&ctx);
    // several requests on one connection
    let mut conn = HttpConnection::connect("127.0.0.1", p, Duration::from_secs(5)).unwrap();
    for i in 0..5 {
        let body = format!("n{}", i);
        let r = conn.send("POST", "/ka", &[("Content-Type", "text/plain")], Some(body.as_bytes())).unwrap();
        assert_eq!(r.body_text(), format!("got:n{}", i));
        assert!(!conn.is_closed());
    }
    let r = conn.send("GET", "/ka", &[("Connection", "close")], None).unwrap();
    assert_eq!(r.headers.get("Connection"), Some("close"));
    assert!(conn.is_closed());

    // pipelined raw requests on one socket
    let out = raw(p, "GET /ka HTTP/1.1\r\nHost: x\r\n\r\nPOST /ka HTTP/1.1\r\nHost: x\r\nContent-Length: 3\r\n\r\nabcGET /ka HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    assert_eq!(out.matches("HTTP/1.1 200 OK").count() + out.matches("HTTP/1.1 204").count(), 3, "{}", out);
    assert!(out.contains("got:abc"));

    // malformed / limits
    assert!(raw(p, "BROKEN\r\n\r\n").starts_with("HTTP/1.1 400"));
    assert!(raw(p, "GET /ka HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 400"), "missing Host");
    let huge = format!("GET /ka HTTP/1.1\r\nHost: x\r\nX-Big: {}\r\n\r\n", "a".repeat(70_000));
    assert!(raw(p, &huge).starts_with("HTTP/1.1 431"));
    let out = raw(p, "POST /ka HTTP/1.1\r\nHost: x\r\nContent-Length: 99999999999\r\nExpect: 100-continue\r\n\r\n");
    assert!(out.starts_with("HTTP/1.1 413"), "{}", out);
    let out = raw(p, "HEAD /ka HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    assert!(out.starts_with("HTTP/1.1 200 OK"), "{}", out);
    assert!(out.ends_with("\r\n\r\n"), "HEAD must not have a body: {}", out);

    // an idle keep-alive connection does not prevent a prompt stop
    let mut idle = TcpStream::connect(("127.0.0.1", p)).unwrap();
    idle.write_all(b"GET /ka HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
    let mut buf = [0u8; 256];
    let _ = idle.read(&mut buf).unwrap();
    let start = Instant::now();
    ctx.stop().unwrap();
    assert!(start.elapsed() < Duration::from_secs(3), "stop took {:?}", start.elapsed());
}

#[test]
fn jetty_style_uri_with_port_and_concurrency() {
    let ctx = CamelContext::new();
    camel_http::register(&ctx);
    ctx.add_routes(|b| {
        b.from("jetty:http://127.0.0.1:0/svc").process(|ex: &mut Exchange| {
            std::thread::sleep(Duration::from_millis(100));
            let b = ex.body().to_string();
            ex.set_body(format!("jetty {}", b));
            Ok(())
        });
        b.from("undertow:http://127.0.0.1:0/u").set_body(constant("undertow"));
    })
    .unwrap();
    ctx.start().unwrap();
    let p = camel_http::local_port(&ctx, "jetty:http://127.0.0.1:0/svc").unwrap();
    let pu = camel_http::local_port(&ctx, "undertow:http://127.0.0.1:0/u").unwrap();
    assert_ne!(p, pu);
    assert_eq!(get(&format!("http://127.0.0.1:{}/u", pu)).body_text(), "undertow");
    // concurrent requests are served in parallel by the pool
    let start = Instant::now();
    let handles: Vec<_> = (0..8)
        .map(|i| {
            std::thread::spawn(move || {
                HttpClient::new().post(&format!("http://127.0.0.1:{}/svc", p), "text/plain", format!("{}", i).as_bytes()).unwrap().body_text()
            })
        })
        .collect();
    let mut results: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    results.sort();
    assert_eq!(results.len(), 8);
    assert_eq!(results[0], "jetty 0");
    assert!(start.elapsed() < Duration::from_millis(700), "requests were not concurrent: {:?}", start.elapsed());
    ctx.stop().unwrap();
}

#[test]
fn routes_share_a_port_and_release_it() {
    let ctx = CamelContext::new();
    camel_http::register(&ctx);
    // find a free port and bind two routes explicitly to it
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    ctx.properties().set("camel.server.port", free.to_string());
    ctx.properties().set("camel.server.host", "127.0.0.1");
    ctx.add_routes(|b| {
        b.from("platform-http:/a").route_id("a").set_body(constant("A"));
        b.from("platform-http:/b").route_id("b").set_body(constant("B"));
    })
    .unwrap();
    ctx.start().unwrap();
    assert_eq!(camel_http::local_port(&ctx, "platform-http:/a"), Some(free));
    assert_eq!(get(&format!("http://127.0.0.1:{}/a", free)).body_text(), "A");
    assert_eq!(get(&format!("http://127.0.0.1:{}/b", free)).body_text(), "B");
    ctx.stop_route("a").unwrap();
    assert_eq!(get(&format!("http://127.0.0.1:{}/a", free)).status, 404);
    assert_eq!(get(&format!("http://127.0.0.1:{}/b", free)).body_text(), "B");
    ctx.start_route("a").unwrap();
    assert_eq!(get(&format!("http://127.0.0.1:{}/a", free)).body_text(), "A");
    ctx.stop().unwrap();
    assert!(!camel_http::registry::registry().ports().contains(&free));
    // duplicate consumers on the same path are rejected
    let ctx2 = CamelContext::new();
    camel_http::register(&ctx2);
    ctx2.properties().set("camel.server.port", "0");
    ctx2.add_routes(|b| {
        b.from("platform-http:/dup").set_body(constant("1"));
        b.from("platform-http:/dup?httpMethodRestrict=GET").set_body(constant("2"));
    })
    .unwrap();
    assert!(ctx2.start().is_err());
    let _ = ctx2.stop();
}

#[test]
fn consumer_to_producer_proxy() {
    let ctx = context();
    ctx.add_routes(|b| {
        b.from("platform-http:/backend").process(|ex: &mut Exchange| {
            let b = ex.body().to_string();
            ex.set_body(format!("backend saw {} via {}", b, ex.header("X-Trace").map(|v| v.to_string()).unwrap_or_default()));
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let p = port(&ctx);
    ctx.add_routes(move |b| {
        b.from("platform-http:/proxy")
            .remove_headers("CamelHttp*")
            .set_header("X-Trace", constant("proxy"))
            .to(format!("http://127.0.0.1:{}/backend?bridgeEndpoint=true", p));
    })
    .unwrap();
    let r = HttpClient::new().post(&format!("http://127.0.0.1:{}/proxy", p), "text/plain", b"ping").unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(r.body_text(), "backend saw ping via proxy");
    ctx.stop().unwrap();
}

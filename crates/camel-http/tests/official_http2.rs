//! Official-client cross-verification of rust-camel's HTTP/2 and HTTPS stacks with Go's
//! net/http (golang.org/x/net/http2 bundled in the standard library) over crypto/tls:
//! Go's HTTP/2 client against camel-grpc's HTTP/2 server (ALPN h2, 1 MiB flow-controlled
//! upload, concurrent streams on one connection, HTTP/1.1 clients answered with 505) and
//! against a camel https consumer (ALPN fallback to HTTP/1.1); camel's HTTP/2 client and
//! the `https` producer with `httpVersion=HTTP/2` against Go's HTTP/2 server.
//! Runs only with `CAMEL_OFFICIAL_CLIENTS=1` (see `scripts/official-clients.sh`).

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use camel_core::prelude::*;
use camel_grpc::h2::{H2Config, H2Connection, H2Request, H2Server, H2Stream, ServerRequest};
use camel_tls::testing::official::{self, run, Run};
use camel_tls::{TlsClientConfig, TlsServerConfig};

fn go_tls() -> &'static PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| official::go_build("go-tls"))
}

fn go_h2(url: &str, ca: &std::path::Path) -> Run {
    let mut cmd = Command::new(go_tls());
    cmd.args(["h2", "-url", url, "-ca"]).arg(ca);
    run(&mut cmd, None, Duration::from_secs(120))
}

#[test]
fn go_http2_client_against_camel_h2_server() {
    if !official::enabled() {
        return;
    }
    let pem = official::pem_files("go-h2-client");
    let p = camel_tls::testing::mtls();
    let tls = TlsServerConfig::builder().cert_chain_pem(p.server.cert_pem.as_bytes()).private_key_pem(p.server.key_pem.as_bytes()).alpn_protocols(&["h2"]).build().unwrap();
    let server = H2Server::bind("127.0.0.1:0", H2Config::default(), Some(tls), |req: ServerRequest, stream: H2Stream| {
        let body = stream.read_body(Some(Duration::from_secs(30))).unwrap_or_default();
        let path = req.path.clone();
        let _ = match path.split('?').next().unwrap_or("") {
            "/hello" => ServerRequest::respond(&stream, 200, &[("content-type", "text/plain")], format!("hello {} {}", req.method, path).as_bytes()),
            "/echo" => ServerRequest::respond(&stream, 200, &[("content-type", "application/octet-stream")], &body),
            _ => ServerRequest::respond(&stream, 404, &[("content-type", "text/plain")], b"not found"),
        };
    })
    .unwrap();
    let url = format!("https://localhost:{}", server.local_addr().port());
    let r = go_h2(&url, &pem.ca).ok();
    assert_eq!(r.value("proto:"), "HTTP/2.0", "{}\n{}", r.stdout, r.stderr);
    assert_eq!(r.value("get:"), "200 text/plain|hello GET /hello?x=1");
    assert!(r.value("tls:").contains("alpn=h2"), "{}", r.stdout);
    assert_eq!(r.value("echo:"), "200 1048576 same=true");
    assert_eq!(r.value("parallel:"), "16/16");
    assert_eq!(r.value("missing:"), "404");
    // an HTTP/1.1-only client: the h2-only server completes the handshake without ALPN
    // (RFC 7301 allows either) and answers 505 HTTP Version Not Supported
    assert_eq!(r.value("h1:"), "HTTP/1.1 505", "{}", r.stdout);
}

#[test]
fn go_http_client_against_camel_https_consumer() {
    if !official::enabled() {
        return;
    }
    let ctx = CamelContext::new();
    camel_http::register(&ctx);
    camel_tls::testing::bind_test_beans(&ctx);
    let uri = "jetty:https://127.0.0.1:0?sslContextParameters=#ssl&matchOnUriPrefix=true";
    ctx.add_routes(move |b| {
        b.from(uri).process(|ex: &mut Exchange| {
            let path = ex.header("CamelHttpUri").map(|v| v.to_string()).unwrap_or_default();
            if path.starts_with("/missing") {
                ex.set_header("CamelHttpResponseCode", 404);
                ex.set_body("no");
            } else if path.starts_with("/echo") {
                let b = ex.body_as::<Vec<u8>>().unwrap_or_default();
                ex.set_body(Value::Bytes(b));
            } else {
                let q = ex.header("CamelHttpQuery").map(|v| v.to_string()).unwrap_or_default();
                ex.set_header("Content-Type", "text/plain");
                ex.set_body(format!("hello {}", q));
            }
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let port = camel_http::local_port(&ctx, uri).unwrap();
    let dir = official::temp_dir("go-https");
    let ca = dir.join("ca.pem");
    std::fs::write(&ca, &camel_tls::testing::pki().localhost.cert_pem).unwrap();
    let r = go_h2(&format!("https://localhost:{}", port), &ca).ok();
    // no h2 in the consumer's ALPN: Go falls back to HTTP/1.1 (keep-alive, pooled connections)
    assert_eq!(r.value("proto:"), "HTTP/1.1", "{}\n{}", r.stdout, r.stderr);
    assert!(r.value("get:").starts_with("200 text/plain"), "{}", r.stdout);
    assert!(r.value("get:").ends_with("hello x=1"), "{}", r.stdout);
    assert_eq!(r.value("echo:"), "200 1048576 same=true");
    assert_eq!(r.value("missing:"), "404");
    assert_eq!(r.value("h1:"), "HTTP/1.1 200");
    ctx.stop().unwrap();
}

/// The Go net/http + crypto/tls server; stopped when dropped (stdin closed).
struct GoServer {
    child: std::process::Child,
    port: u16,
}

impl GoServer {
    fn start(args: &[&str]) -> GoServer {
        let dir = official::temp_dir("go-serve");
        let portfile = dir.join("port");
        let mut child = Command::new(go_tls()).arg("serve").arg("-portfile").arg(&portfile).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().expect("go-tls");
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(p) = std::fs::read_to_string(&portfile) {
                return GoServer { child, port: p.trim().parse().unwrap() };
            }
            if let Ok(Some(st)) = child.try_wait() {
                let mut err = String::new();
                child.stderr.take().unwrap().read_to_string(&mut err).unwrap();
                panic!("go server exited ({}): {}", st, err);
            }
            assert!(Instant::now() < deadline, "go server did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for GoServer {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        std::thread::sleep(Duration::from_millis(50));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn camel_http2_clients_against_go_server() {
    if !official::enabled() {
        return;
    }
    let pem = official::pem_files("go-h2-server");
    let p = camel_tls::testing::mtls();
    let go = GoServer::start(&["-cert", pem.server_cert.to_str().unwrap(), "-key", pem.server_key.to_str().unwrap()]);
    // camel-grpc's HTTP/2 client (TLS, ALPN h2)
    let tls = TlsClientConfig::builder().system_roots(false).add_root_pem(p.ca.cert_pem.as_bytes()).unwrap().alpn_protocols(&["h2"]).build();
    let conn = H2Connection::connect_tls(&format!("127.0.0.1:{}", go.port), "localhost", &tls, H2Config::default()).unwrap();
    let resp = conn.request(&H2Request::get("/info?a=1"), Some(Duration::from_secs(10))).unwrap();
    assert_eq!(resp.status, 200);
    let text = String::from_utf8_lossy(&resp.body).to_string();
    assert!(text.starts_with("proto=HTTP/2.0 version=TLS 1.3 ") && text.contains("alpn=h2") && text.contains("path=/info?a=1"), "{}", text);
    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    let resp = conn.request(&H2Request::post("/echo", big.clone()), Some(Duration::from_secs(30))).unwrap();
    assert_eq!((resp.status, resp.body.len()), (200, big.len()));
    assert!(resp.body == big, "echoed body differs");
    let resp = conn.request(&H2Request::get("/stream"), Some(Duration::from_secs(10))).unwrap();
    assert_eq!(String::from_utf8_lossy(&resp.body).lines().count(), 5);
    let resp = conn.request(&H2Request::get("/missing"), Some(Duration::from_secs(10))).unwrap();
    assert_eq!(resp.status, 404);
    // concurrent streams on the one connection
    let conn = std::sync::Arc::new(conn);
    let handles: Vec<_> = (0..12)
        .map(|i| {
            let c = conn.clone();
            std::thread::spawn(move || c.request(&H2Request::get(&format!("/info?i={}", i)), Some(Duration::from_secs(20))).map(|r| String::from_utf8_lossy(&r.body).contains(&format!("path=/info?i={}", i))))
        })
        .collect();
    for h in handles {
        assert!(h.join().unwrap().unwrap());
    }
    // the https producer with httpVersion=HTTP/2 (headers become request headers)
    let ctx = CamelContext::new();
    camel_http::register(&ctx);
    camel_tls::testing::bind_mtls_beans(&ctx);
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let out = t
        .request_body_and_header(&format!("https://localhost:{}/info?httpVersion=HTTP/2&sslContextParameters=#sslMtlsAnonymous", go.port), "", "X-Tenant", "camel")
        .unwrap();
    let text = String::from_utf8_lossy(&Value::from(out).to_bytes()).to_string();
    assert!(text.starts_with("proto=HTTP/2.0 ") && text.contains("tenant=camel"), "{}", text);
    let out = t.request_body(&format!("https://localhost:{}/echo?httpVersion=HTTP/2&sslContextParameters=#sslMtlsAnonymous", go.port), "ping over h2").unwrap();
    assert_eq!(String::from_utf8_lossy(&Value::from(out).to_bytes()), "ping over h2");
    // and HTTP/1.1 (default) against the same server
    let out = t.request_body_and_header(&format!("https://localhost:{}/info?sslContextParameters=#sslMtlsAnonymous", go.port), "", "X-Tenant", "h1").unwrap();
    let text = String::from_utf8_lossy(&Value::from(out).to_bytes()).to_string();
    assert!(text.starts_with("proto=HTTP/1.1 ") && text.contains("tenant=h1"), "{}", text);
    ctx.stop().unwrap();
}

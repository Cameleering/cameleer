//! Official-client cross-verification of Cameleer's HTTP/2 and HTTPS stacks with Go's
//! net/http (golang.org/x/net/http2 bundled in the standard library) over crypto/tls:
//! Go's HTTP/2 client against simoom's HTTP/2 server (ALPN h2, 1 MiB flow-controlled
//! upload, concurrent streams on one connection, HTTP/1.1 clients answered on HTTP/1.1) and
//! against a camel https consumer (ALPN fallback to HTTP/1.1); simoom-client's HTTP/2 client and
//! the `https` producer with `httpVersion=HTTP/2` against Go's HTTP/2 server.
//! Runs only with `CAMEL_OFFICIAL_CLIENTS=1` (see `scripts/official-clients.sh`).

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use cameleer_core::prelude::*;
use cameleer_tls::testing::official::{self, run, Run};
use cameleer_tls::{TlsClientConfig, TlsServerConfig};

/// One request on `client`: the status and the whole body.
fn fetch(client: &simoom_client::Client, url: &str, body: Option<Vec<u8>>, secs: u64) -> (u16, Vec<u8>) {
    let req = match body {
        Some(b) => client.post(url).body(b),
        None => client.get(url),
    };
    let resp = req.timeout(Duration::from_secs(secs)).accept_any_status().send().unwrap();
    let status = resp.status();
    (status, resp.bytes().unwrap())
}

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
    let p = cameleer_tls::testing::mtls();
    let tls = TlsServerConfig::builder().cert_chain_pem(p.server.cert_pem.as_bytes()).private_key_pem(p.server.key_pem.as_bytes()).alpn_protocols(&["h2"]).build().unwrap();
    let service = |req: &mut simoom::Request<'_>, reply: simoom::Reply<'_>| -> simoom::Outcome {
        let target = String::from_utf8_lossy(req.target()).into_owned();
        let method = String::from_utf8_lossy(req.method()).into_owned();
        match target.split('?').next().unwrap_or("") {
            "/hello" => reply.header("content-type", "text/plain").body(format!("hello {} {}", method, target)),
            "/echo" => {
                // the body streams in when it is larger than the buffer
                reply.blocking(req, |mut r| {
                    let body = r.read_body().unwrap_or_default();
                    simoom::Response::new(200).header("content-type", "application/octet-stream").body(body)
                })
            }
            _ => reply.status(404).header("content-type", "text/plain").body_static(b"not found"),
        }
    };
    let server = simoom::Server::builder()
        .limits(simoom::Limits { max_body: 8 << 20, ..simoom::Limits::default() })
        .service(service)
        .listen_tls("127.0.0.1:0")
        .tls(cameleer_http::server_simoom::server_tls(&tls).unwrap())
        .start()
        .unwrap();
    let url = format!("https://localhost:{}", server.local_addr().port());
    let r = go_h2(&url, &pem.ca).ok();
    assert_eq!(r.value("proto:"), "HTTP/2.0", "{}\n{}", r.stdout, r.stderr);
    assert_eq!(r.value("get:"), "200 text/plain|hello GET /hello?x=1");
    assert!(r.value("tls:").contains("alpn=h2"), "{}", r.stdout);
    assert_eq!(r.value("echo:"), "200 1048576 same=true");
    assert_eq!(r.value("parallel:"), "16/16");
    assert_eq!(r.value("missing:"), "404");
    // an HTTP/1.1-only client (no h2 in ALPN) is served over HTTP/1.1 by the same server
    assert_eq!(r.value("h1:"), "HTTP/1.1 200", "{}", r.stdout);
}

#[test]
fn go_http_client_against_camel_https_consumer() {
    if !official::enabled() {
        return;
    }
    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    cameleer_tls::testing::bind_test_beans(&ctx);
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
    let port = cameleer_http::local_port(&ctx, uri).unwrap();
    let dir = official::temp_dir("go-https");
    let ca = dir.join("ca.pem");
    std::fs::write(&ca, &cameleer_tls::testing::pki().localhost.cert_pem).unwrap();
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
    let p = cameleer_tls::testing::mtls();
    let go = GoServer::start(&["-cert", pem.server_cert.to_str().unwrap(), "-key", pem.server_key.to_str().unwrap()]);
    // simoom-client's HTTP/2 client (TLS, ALPN h2)
    let tls = TlsClientConfig::builder().system_roots(false).add_root_pem(p.ca.cert_pem.as_bytes()).unwrap().alpn_protocols(&["h2"]).build();
    let mut cfg = simoom_client::Config::default();
    cfg.version = simoom_client::HttpVersion::Http2;
    let client = std::sync::Arc::new(
        simoom_client::ClientBuilder::default().config(cfg).tls(cameleer_http::client_engine::tls_options(Some(&tls), Some("localhost"), true).unwrap()).build(),
    );
    let base = format!("https://localhost:{}", go.port);
    let (status, body) = fetch(&client, &format!("{base}/info?a=1"), None, 10);
    assert_eq!(status, 200);
    let text = String::from_utf8_lossy(&body).to_string();
    assert!(text.starts_with("proto=HTTP/2.0 version=TLS 1.3 ") && text.contains("alpn=h2") && text.contains("path=/info?a=1"), "{}", text);
    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    let (status, body) = fetch(&client, &format!("{base}/echo"), Some(big.clone()), 30);
    assert_eq!((status, body.len()), (200, big.len()));
    assert!(body == big, "echoed body differs");
    let (_, body) = fetch(&client, &format!("{base}/stream"), None, 10);
    assert_eq!(String::from_utf8_lossy(&body).lines().count(), 5);
    assert_eq!(fetch(&client, &format!("{base}/missing"), None, 10).0, 404);
    // concurrent streams on the one connection
    let handles: Vec<_> = (0..12)
        .map(|i| {
            let (c, base) = (client.clone(), base.clone());
            std::thread::spawn(move || String::from_utf8_lossy(&fetch(&c, &format!("{base}/info?i={i}"), None, 20).1).contains(&format!("path=/info?i={i}")))
        })
        .collect();
    for h in handles {
        assert!(h.join().unwrap());
    }
    // the https producer with httpVersion=HTTP/2 (headers become request headers)
    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    cameleer_tls::testing::bind_mtls_beans(&ctx);
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

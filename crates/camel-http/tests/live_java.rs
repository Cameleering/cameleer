//! Live interop with the installed JDK's JSSE TLS stack (no network needed): the `https`
//! producer against a Java `HttpsServer` (TLS 1.3 and TLS 1.2, PKCS#12 key store made with
//! openssl, trust store made with keytool by `scripts/live-interop.sh`), and Java's
//! `HttpClient` (offering HTTP/2 via ALPN) against a rust-camel `jetty:https` consumer.
//! Runs only with `CAMEL_LIVE_INTEROP=1` and a `java` on the PATH.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use camel_core::prelude::*;
use camel_tls::testing::live;
use camel_tls::{SslContextParameters, TlsVersion};

fn java_source() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/java/JavaTls.java")
}

fn java_available() -> bool {
    Command::new("java").arg("-version").stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
}

fn java() -> Command {
    let mut c = Command::new("java");
    // the sandbox's JAVA_TOOL_OPTIONS (proxy, trust store) must not leak into the peer
    c.env_remove("JAVA_TOOL_OPTIONS");
    // UTF-8 command line arguments and stdout regardless of the sandbox locale
    c.env("LC_ALL", "C.UTF-8").arg("-Dstdout.encoding=UTF-8").arg("-Dsun.jnu.encoding=UTF-8");
    c
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

#[test]
fn live_java_jsse_https_server_and_client() {
    if !live::enabled() {
        return;
    }
    if !java_available() {
        eprintln!("skipped: no java on the PATH");
        return;
    }
    let certs = live::dir().join("certs");
    let ctx = CamelContext::new();
    camel_http::register(&ctx);
    live::bind_beans(&ctx);
    for (name, v) in [("tls13", TlsVersion::Tls13), ("tls12", TlsVersion::Tls12)] {
        ctx.registry().bind(name, live::client_params().versions(v, v));
    }

    // 1. rust-camel https producer -> JSSE HttpsServer, per protocol version
    for (proto, bean) in [("TLSv1.3", "tls13"), ("TLSv1.2", "tls12")] {
        let port = free_port();
        let mut server = java()
            .arg(java_source())
            .args(["server", &port.to_string(), certs.join("server.p12").to_str().unwrap(), live::PASSWORD, proto])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("java");
        let mut line = String::new();
        BufReader::new(server.stdout.take().unwrap()).read_line(&mut line).unwrap();
        assert_eq!(line.trim(), "READY", "java server did not start");
        ctx.start().unwrap();
        let t = ctx.create_producer_template();
        for i in 0..3 {
            let ex = t.request(&format!("https://localhost:{}/echo?sslContextParameters=#{}", port, bean), |ex| ex.set_body(format!("hello jsse {} ✓", i))).unwrap_or_else(|e| panic!("{}: {}", proto, e));
            assert_eq!(ex.body_as::<String>().unwrap(), format!("hello jsse {} ✓", i));
            assert_eq!(ex.header("X-tls").map(|v| v.to_string()).as_deref(), Some(proto), "{:?}", ex.message.headers);
            assert!(ex.header("X-cipher").map(|v| v.to_string()).unwrap_or_default().starts_with("TLS_"));
        }
        // a large body
        let big = "j".repeat(300_000);
        let ex = t.request(&format!("https://localhost:{}/echo?sslContextParameters=#{}", port, bean), |ex| ex.set_body(big.clone())).unwrap();
        assert_eq!(ex.body_as::<String>().unwrap().len(), big.len());
        // the JSSE server's certificate is not trusted by the unrelated test CA
        assert!(t.request(&format!("https://localhost:{}/echo?sslContextParameters=#liveUntrusted", port), |ex| ex.set_body("x")).is_err());
        ctx.stop().unwrap();
        drop(server.stdin.take());
        let _ = server.wait();
    }

    // 2. Java HttpClient (HTTP/2 preferred, ALPN) -> rust-camel jetty:https consumer
    let port = free_port();
    ctx.registry().bind("liveServer", SslContextParameters::new().key_pair_files(certs.join("server.pem"), certs.join("server.key")));
    let from = format!("jetty:https://localhost:{}/echo?sslContextParameters=#liveServer", port);
    ctx.add_routes(move |b| {
        b.from(&from).transform(simple("camel:${body}"));
    })
    .unwrap();
    ctx.start().unwrap();
    for version in ["HTTP_2", "HTTP_1_1"] {
        for body in [None, Some("from java ✓")] {
            let mut cmd = java();
            cmd.arg(java_source()).args(["client", &format!("https://localhost:{}/echo", port), certs.join("ca-trust.p12").to_str().unwrap(), live::PASSWORD, version]);
            if let Some(b) = body {
                cmd.arg(b);
            }
            let out = cmd.output().expect("java client");
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            assert!(out.status.success(), "{} {:?}: {}{}", version, body, stdout, String::from_utf8_lossy(&out.stderr));
            // the camel server speaks HTTP/1.1: ALPN falls back from h2
            let expected = format!("200 HTTP_1_1 - camel:{}", body.unwrap_or(""));
            assert_eq!(stdout.trim(), expected, "{} {:?}", version, body);
        }
    }
    ctx.stop().unwrap();
}

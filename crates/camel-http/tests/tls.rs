//! End-to-end TLS tests: https producer, jetty/platform-http https consumers,
//! HttpClient with proxy CONNECT tunnelling, REST DSL over https, netty ssl=true,
//! and (when `openssl` is installed) interop against `openssl s_server -www`.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use camel_core::prelude::*;
use camel_http::rest::RestDslExt;
use camel_http::{HttpClient, ProxyAddr, ProxyConfig};
use camel_tls::testing;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    camel_http::register(&ctx);
    testing::bind_test_beans(&ctx);
    ctx
}

fn tls_client() -> HttpClient {
    HttpClient { read_timeout: Duration::from_secs(20), ..HttpClient::new() }.with_tls(testing::client_params().client_config().unwrap())
}

fn echo_route(ctx: &CamelContext, from: &str) {
    let from = from.to_string();
    ctx.add_routes(move |b| {
        b.from(&from).process(|ex: &mut Exchange| {
            let body = ex.body().to_string();
            ex.set_body(format!("secure:{}", body));
            Ok(())
        });
    })
    .unwrap();
}

#[test]
fn https_producer_to_jetty_https_consumer() {
    let ctx = context();
    echo_route(&ctx, "jetty:https://127.0.0.1:0/secure?sslContextParameters=#ssl");
    ctx.start().unwrap();
    let port = camel_http::local_port(&ctx, "jetty:https://127.0.0.1:0/secure?sslContextParameters=#ssl").unwrap();

    let t = ctx.create_producer_template();
    let out = t.request_body(&format!("https://localhost:{}/secure?sslContextParameters=#ssl", port), "ping").unwrap();
    assert_eq!(out.to_string(), "secure:ping");

    // plain HTTP against the TLS port is not served
    let plain = HttpClient { read_timeout: Duration::from_secs(5), ..HttpClient::new() }.get(&format!("http://127.0.0.1:{}/secure", port));
    assert!(plain.is_err(), "plain http must not be answered by the https server");

    // untrusted certificate is rejected
    let err = t.request_body(&format!("https://localhost:{}/secure?sslContextParameters=#sslUntrusted", port), "x").unwrap_err();
    let root = err.root_cause();
    assert!(root.is_a("IOException") && root.is_a("SSLException"), "{:?}", err);
    assert!(root.message.contains("certificate"), "{}", root.message);

    // the default (system) trust store does not know the self-signed certificate either
    let err = t.request_body(&format!("https://localhost:{}/secure", port), "x").unwrap_err();
    assert!(err.root_cause().is_a("SSLException"), "{:?}", err);
    ctx.stop().unwrap();
}

#[test]
fn hostname_mismatch_is_rejected() {
    let ctx = context();
    echo_route(&ctx, "undertow:https://127.0.0.1:0/w?sslContextParameters=#sslWrongHost");
    ctx.start().unwrap();
    let port = camel_http::local_port(&ctx, "undertow:https://127.0.0.1:0/w?sslContextParameters=#sslWrongHost").unwrap();
    let t = ctx.create_producer_template();
    let err = t.request_body(&format!("https://localhost:{}/w?sslContextParameters=#ssl", port), "x").unwrap_err();
    let root = err.root_cause();
    assert!(root.is_a("SSLException"), "{:?}", err);
    assert!(root.message.contains("wrong.example") || root.message.to_lowercase().contains("hostname"), "{}", root.message);

    // with hostname verification off (chain still verified) the call succeeds
    ctx.registry().bind("sslNoHost", testing::client_params().hostname_verification(false));
    let out = t.request_body(&format!("https://localhost:{}/w?sslContextParameters=#sslNoHost", port), "y").unwrap();
    assert_eq!(out.to_string(), "secure:y");
    // ... or with the SNI/verification name overridden to the certificate's name
    ctx.registry().bind("sslSni", testing::client_params().with_server_name("wrong.example"));
    let out = t.request_body(&format!("https://localhost:{}/w?sslContextParameters=#sslSni", port), "z").unwrap();
    assert_eq!(out.to_string(), "secure:z");
    ctx.stop().unwrap();
}

#[test]
fn platform_http_with_camel_server_ssl_properties() {
    let (cert, key) = testing::write_localhost_pem_files("http");
    let ctx = context();
    ctx.properties().set("camel.server.port", "0");
    ctx.properties().set("camel.server.sslEnabled", "true");
    ctx.properties().set("camel.server.ssl.certFile", cert.display().to_string());
    ctx.properties().set("camel.server.ssl.keyFile", key.display().to_string());
    echo_route(&ctx, "platform-http:/p");
    ctx.start().unwrap();
    let port = camel_http::platform_http_port(&ctx).unwrap();
    let r = tls_client().post(&format!("https://localhost:{}/p", port), "text/plain", b"hi").unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(r.body_text(), "secure:hi");
    // keep-alive connection over TLS
    let mut conn = camel_http::HttpConnection::connect_with("localhost", port, Some(&testing::client_params().client_config().unwrap()), Duration::from_secs(10)).unwrap();
    assert!(conn.is_tls());
    for i in 0..3 {
        let r = conn.send("POST", "/p", &[("Content-Type", "text/plain")], Some(format!("n{}", i).as_bytes())).unwrap();
        assert_eq!(r.body_text(), format!("secure:n{}", i));
    }
    ctx.stop().unwrap();
}

#[test]
fn global_ssl_context_parameters_from_properties() {
    let (cert, key) = testing::write_localhost_pem_files("global");
    let ks = cert.with_file_name("keystore.pem");
    std::fs::write(&ks, format!("{}{}", std::fs::read_to_string(&cert).unwrap(), std::fs::read_to_string(&key).unwrap())).unwrap();
    let ctx = context();
    ctx.properties().set("camel.ssl.enabled", "true");
    ctx.properties().set("camel.ssl.keyStore", ks.display().to_string());
    ctx.properties().set("camel.ssl.trustStore", cert.display().to_string());
    ctx.properties().set("camel.server.port", "0");
    ctx.properties().set("camel.server.useGlobalSslContextParameters", "true");
    echo_route(&ctx, "platform-http:/g");
    ctx.start().unwrap();
    let port = camel_http::platform_http_port(&ctx).unwrap();
    // the https producer uses the global parameters (camel.ssl.enabled=true) for trust
    let t = ctx.create_producer_template();
    let out = t.request_body(&format!("https://localhost:{}/g?useGlobalSslContextParameters=true", port), "a").unwrap();
    assert_eq!(out.to_string(), "secure:a");
    let out = t.request_body(&format!("https://localhost:{}/g", port), "b").unwrap();
    assert_eq!(out.to_string(), "secure:b");
    ctx.stop().unwrap();
}

#[test]
fn rest_dsl_with_https_scheme() {
    let ctx = context();
    ctx.properties().set("camel.component.platform-http.sslContextParameters", "#ssl");
    ctx.add_routes(|b| {
        b.rest_configuration().component("platform-http").scheme("https").host("127.0.0.1").port(0);
        b.rest("/api").get("/hello/{name}").to("direct:hello");
        b.from("direct:hello").set_body(simple("hello ${header.name}"));
    })
    .unwrap();
    ctx.start().unwrap();
    let port = camel_http::platform_http_port(&ctx).unwrap();
    let r = tls_client().get(&format!("https://localhost:{}/api/hello/tls", port)).unwrap();
    assert_eq!(r.body_text(), "hello tls");
    // rest producer to an https host (trust from camel.component.https.sslContextParameters)
    ctx.properties().set("camel.component.https.sslContextParameters", "#ssl");
    let t = ctx.create_producer_template();
    let mut ex = Exchange::new(Some(ctx.clone()));
    ex.set_header("name", "rest");
    let ex = t.send(&format!("rest:get:api/hello/{{name}}?host=https://localhost:{}", port), ex);
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
    assert_eq!(ex.body().to_string(), "hello rest");
    ctx.stop().unwrap();
}

#[test]
fn netty_tcp_ssl_round_trip_and_rejections() {
    let ctx = context();
    let uri = "netty:tcp://127.0.0.1:0?textline=true&sync=true&ssl=true&sslContextParameters=#ssl";
    ctx.add_routes(|b| {
        b.from(uri).transform(simple("tls echo: ${body}"));
    })
    .unwrap();
    ctx.start().unwrap();
    let port = camel_http::local_port(&ctx, uri).unwrap();
    let t = ctx.create_producer_template();
    let out = t
        .request_body(&format!("netty:tcp://localhost:{}?textline=true&sync=true&ssl=true&sslContextParameters=#ssl&hostnameVerification=true&requestTimeout=10s", port), "one")
        .unwrap();
    assert_eq!(out.to_string(), "tls echo: one");
    let err = t
        .request_body(&format!("netty:tcp://localhost:{}?textline=true&sync=true&ssl=true&sslContextParameters=#sslUntrusted&requestTimeout=10s", port), "two")
        .unwrap_err();
    assert!(err.root_cause().is_a("SSLException"), "{:?}", err);
    // a plain client gets no reply from the TLS server
    let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    (&s).write_all(b"plain\n").unwrap();
    let mut line = String::new();
    let _ = BufReader::new(&s).read_line(&mut line);
    assert!(!line.contains("tls echo"));
    ctx.stop().unwrap();
}

/// Mutual TLS on platform-http / jetty https consumers (`needClientAuth=true`, `clientAuth=WANT`)
/// with the verified client certificate exposed as `CamelHttpSslClientCert*` headers.
#[test]
fn platform_http_mutual_tls_and_client_cert_headers() {
    let ctx = context();
    testing::bind_mtls_beans(&ctx);
    ctx.registry().bind("mtlsServerNoAuth", testing::mtls_server_params(camel_tls::ClientAuth::None));
    let need = "jetty:https://127.0.0.1:0/mtls?sslContextParameters=#mtlsServerNoAuth&needClientAuth=true";
    let want = "platform-http:/want?ssl=true&port=0&sslContextParameters=#mtlsServerNoAuth&clientAuth=WANT";
    for from in [need, want] {
        ctx.add_routes(move |b| {
            b.from(from).process(|ex: &mut Exchange| {
                let subject = ex.header("CamelHttpSslClientCertSubjectName").map(|v| v.to_string()).unwrap_or_else(|| "anonymous".into());
                let pem = ex.header("CamelHttpSslClientCert").map(|v| v.to_string()).unwrap_or_default();
                let issuer = ex.header("CamelHttpSslClientCertIssuerName").map(|v| v.to_string()).unwrap_or_default();
                ex.set_body(format!("{}|{}|{}", subject, issuer, pem.contains("BEGIN CERTIFICATE")));
                Ok(())
            });
        })
        .unwrap();
    }
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let port = camel_http::local_port(&ctx, need).unwrap();
    let out = t.request_body(&format!("https://localhost:{}/mtls?sslContextParameters=#sslMtlsClient", port), "x").unwrap();
    assert_eq!(out.to_string(), "CN=camel-client|CN=Camel mTLS Test CA|true");
    for bad in ["sslMtlsAnonymous", "sslMtlsUntrusted"] {
        let err = t.request_body(&format!("https://localhost:{}/mtls?sslContextParameters=#{}", port, bad), "x").unwrap_err();
        assert!(err.root_cause().is_a("IOException"), "{bad}: {:?}", err);
    }
    let port = camel_http::local_port(&ctx, want).unwrap();
    let out = t.request_body(&format!("https://localhost:{}/want?sslContextParameters=#sslMtlsAnonymous", port), "x").unwrap();
    assert!(out.to_string().starts_with("anonymous|"), "{}", out);
    let out = t.request_body(&format!("https://localhost:{}/want?sslContextParameters=#sslMtlsClient", port), "x").unwrap();
    assert!(out.to_string().starts_with("CN=camel-client|"), "{}", out);
    ctx.stop().unwrap();
}

/// netty `needClientAuth=true` with PKCS#12 key/trust stores (`keyStoreFile`, `trustStoreFile`,
/// `passphrase`, `keyStoreFormat`) and the `CamelNettySSLClientCert*` headers.
#[test]
fn netty_need_client_auth_with_pkcs12_stores() {
    use camel_tls::keystore::KeyStore;
    let pki = testing::mtls();
    let dir = std::env::temp_dir().join(format!("camel-http-netty-mtls-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let write = |name: &str, ks: KeyStore| {
        let p = dir.join(name);
        std::fs::write(&p, ks.to_pkcs12("changeit")).unwrap();
        p.display().to_string()
    };
    let server_ks = write("server.p12", KeyStore::from_key_entry("server", pki.server.key.clone(), vec![pki.server.cert_der.clone()]));
    let client_ks = write("client.p12", KeyStore::from_key_entry("client", pki.client.key.clone(), vec![pki.client.cert_der.clone()]));
    let other_ks = write("other.p12", KeyStore::from_key_entry("other", pki.untrusted_client.key.clone(), vec![pki.untrusted_client.cert_der.clone()]));
    let trust = write("trust.p12", KeyStore::new().add_cert("ca", pki.ca.cert_der.clone()));
    let ctx = context();
    let uri = format!(
        "netty:tcp://127.0.0.1:0?textline=true&sync=true&ssl=true&needClientAuth=true&keyStoreFile={}&trustStoreFile={}&passphrase=changeit&keyStoreFormat=PKCS12",
        server_ks, trust
    );
    let from = uri.clone();
    ctx.add_routes(move |b| {
        b.from(&from).transform(simple("${header.CamelNettySSLClientCertSubjectName}"));
    })
    .unwrap();
    ctx.start().unwrap();
    let port = camel_http::local_port(&ctx, &uri).unwrap();
    let t = ctx.create_producer_template();
    let client = |ks: &str| {
        format!("netty:tcp://localhost:{}?textline=true&sync=true&ssl=true&keyStoreFile={}&trustStoreFile={}&passphrase=changeit&requestTimeout=10s", port, ks, trust)
    };
    assert_eq!(t.request_body(&client(&client_ks), "hi").unwrap().to_string(), "CN=camel-client");
    assert!(t.request_body(&client(&other_ks), "hi").is_err(), "untrusted client certificate must be rejected");
    let anonymous = format!("netty:tcp://localhost:{}?textline=true&sync=true&ssl=true&trustStoreFile={}&passphrase=changeit&requestTimeout=10s", port, trust);
    assert!(t.request_body(&anonymous, "hi").is_err(), "a client without certificate must be rejected");
    ctx.stop().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A minimal HTTP CONNECT proxy: one tunnel per connection, counting tunnels.
fn start_connect_proxy() -> (u16, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let count = Arc::new(AtomicUsize::new(0));
    let c2 = count.clone();
    thread::spawn(move || {
        for s in l.incoming() {
            let Ok(client) = s else { continue };
            let count = c2.clone();
            thread::spawn(move || {
                let mut r = BufReader::new(client.try_clone().unwrap());
                let mut first = String::new();
                r.read_line(&mut first).unwrap();
                let mut auth = None;
                loop {
                    let mut h = String::new();
                    r.read_line(&mut h).unwrap();
                    if h.to_ascii_lowercase().starts_with("proxy-authorization:") {
                        auth = Some(h.trim().to_string());
                    }
                    if h.trim().is_empty() {
                        break;
                    }
                }
                let target = first.split_whitespace().nth(1).unwrap_or("").to_string();
                if !first.starts_with("CONNECT ") || auth.is_none() {
                    (&client).write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n").unwrap();
                    return;
                }
                count.fetch_add(1, Ordering::SeqCst);
                let upstream = TcpStream::connect(&target).unwrap();
                (&client).write_all(b"HTTP/1.1 200 Connection established\r\n\r\n").unwrap();
                let (mut c_r, mut u_w) = (client.try_clone().unwrap(), upstream.try_clone().unwrap());
                let t = thread::spawn(move || {
                    let _ = std::io::copy(&mut c_r, &mut u_w);
                    let _ = u_w.shutdown(std::net::Shutdown::Write);
                });
                let (mut u_r, mut c_w) = (upstream, client);
                let _ = std::io::copy(&mut u_r, &mut c_w);
                let _ = c_w.shutdown(std::net::Shutdown::Write);
                let _ = t.join();
            });
        }
    });
    (port, count)
}

#[test]
fn https_through_connect_proxy_and_no_proxy() {
    let ctx = context();
    echo_route(&ctx, "jetty:https://127.0.0.1:0/px?sslContextParameters=#ssl");
    ctx.start().unwrap();
    let port = camel_http::local_port(&ctx, "jetty:https://127.0.0.1:0/px?sslContextParameters=#ssl").unwrap();
    let (proxy_port, tunnels) = start_connect_proxy();
    let proxy = ProxyAddr { host: "127.0.0.1".into(), port: proxy_port, credentials: Some("user:secret".into()) };

    let client = tls_client().with_proxy(ProxyConfig::new(proxy.clone()));
    let r = client.post(&format!("https://localhost:{}/px", port), "text/plain", b"tunnel").unwrap();
    assert_eq!(r.body_text(), "secure:tunnel");
    assert_eq!(tunnels.load(Ordering::SeqCst), 1);

    // NO_PROXY: localhost bypasses the proxy
    let client = tls_client().with_proxy(ProxyConfig::new(proxy.clone()).with_no_proxy("example.org, localhost"));
    let r = client.post(&format!("https://localhost:{}/px", port), "text/plain", b"direct").unwrap();
    assert_eq!(r.body_text(), "secure:direct");
    assert_eq!(tunnels.load(Ordering::SeqCst), 1, "bypassed the proxy");

    // proxy refusing the tunnel (no credentials) is reported
    let bad = ProxyAddr { credentials: None, ..proxy };
    let err = tls_client().with_proxy(ProxyConfig::new(bad)).get(&format!("https://localhost:{}/px", port)).unwrap_err();
    assert!(err.message.contains("407"), "{}", err.message);

    // the producer's proxyHost/proxyPort options
    let t = ctx.create_producer_template();
    let out = t
        .request_body(
            &format!("https://localhost:{}/px?sslContextParameters=#ssl&proxyHost=127.0.0.1&proxyPort={}&proxyAuthUsername=user&proxyAuthPassword=secret", port, proxy_port),
            "opt",
        )
        .unwrap();
    assert_eq!(out.to_string(), "secure:opt");
    assert_eq!(tunnels.load(Ordering::SeqCst), 2);
    ctx.stop().unwrap();
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

#[test]
fn interop_with_openssl_s_server() {
    if std::process::Command::new("openssl").arg("version").output().map(|o| !o.status.success()).unwrap_or(true) {
        eprintln!("openssl not installed; skipping");
        return;
    }
    let (cert, key) = testing::write_localhost_pem_files("openssl");
    let port = free_port();
    let mut child = std::process::Command::new("openssl")
        .args(["s_server", "-www", "-quiet", "-accept", &port.to_string(), "-cert"])
        .arg(&cert)
        .arg("-key")
        .arg(&key)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "openssl s_server did not start");
        thread::sleep(Duration::from_millis(50));
    }
    let ctx = context();
    let t = ctx.create_producer_template();
    let result = t.request_body(&format!("https://localhost:{}/?sslContextParameters=#ssl&httpMethod=GET", port), Value::Null);
    let _ = child.kill();
    let _ = child.wait();
    let body = result.unwrap().to_string();
    assert!(body.contains("s_server") || body.contains("Ciphers"), "{}", body);
}

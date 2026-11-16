//! HTTPS (cameleer-tls) and proxy (CONNECT) transport of the REST client.

use cameleer_cloud::http::{HttpClient, Proxy, Request, Url};
use cameleer_cloud::testing::{AwsMock, ConnectProxy};
use cameleer_core::prelude::*;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_cloud::register(&ctx);
    ctx
}

#[test]
fn s3_over_https_with_a_trusted_certificate_and_through_a_connect_proxy() {
    let mock = AwsMock::start_tls().unwrap();
    assert!(mock.url().starts_with("https://localhost:"));
    mock.create_bucket("secure");
    let ctx = context();
    let t = ctx.create_producer_template();
    let base = format!("aws2-s3://secure?{}", mock.endpoint_options());
    // the self-signed certificate is not trusted by default
    let e = t.request_body_and_header(&base, "x", "CamelAwsS3Key", "k").unwrap_err();
    assert!(e.root_cause().is_a("SSLHandshakeException"), "{}", e);
    // trustAllCertificates is refused, and the message says how to trust the server
    let e = t.send_body_and_header(&format!("{}&trustAllCertificates=true", base), "x", "CamelAwsS3Key", "k").unwrap_err();
    let text = format!("{} / {}", e, e.root_cause());
    assert!(text.contains("always verifies") && text.contains("camel.ssl.trustStore"), "{}", text);

    // trusting the server's certificate through the global SSL settings
    let ctx = context();
    ctx.properties().set("camel.ssl.enabled", "true");
    ctx.properties().set("camel.ssl.trustStore", mock.cert_pem().unwrap());
    let t = ctx.create_producer_template();
    t.send_body_and_header(&base, "over tls", "CamelAwsS3Key", "k").unwrap();
    assert_eq!(mock.object("secure", "k").unwrap(), b"over tls");
    // explicit proxy: the request is tunnelled with CONNECT, then TLS end to end
    let proxy = ConnectProxy::start().unwrap();
    let uri = format!("{}&proxyHost=127.0.0.1&proxyPort={}&operation=getObject", base, proxy.port());
    let body = t.request_body_and_header(&uri, "", "CamelAwsS3Key", "k").unwrap();
    assert_eq!(body.to_string(), "over tls");
    assert_eq!(proxy.tunnel_count(), 1);
}

#[test]
fn https_with_verified_certificate_and_hostname() {
    let mock = AwsMock::start_tls().unwrap();
    let pem = mock.cert_pem().unwrap().to_string();
    let cfg = cameleer_tls::TlsClientConfig::builder().system_roots(false).add_root_pem(pem.as_bytes()).unwrap().build();
    let client = HttpClient::new().with_tls_config(cfg).with_proxy(Proxy::None);
    // unsigned request: the TLS handshake (with certificate + hostname verification) succeeds and
    // the emulator answers 403 MissingAuthenticationToken
    let resp = client.send(&Request::new("GET", Url::parse(&format!("{}/", mock.url())).unwrap())).unwrap();
    assert_eq!(resp.status, 403);
    assert!(resp.text().contains("MissingAuthenticationToken"));
    // a different (untrusted) root: certificate verification fails
    let other = cameleer_tls::self_signed("localhost");
    let cfg = cameleer_tls::TlsClientConfig::builder().system_roots(false).add_root_pem(other.cert_pem.as_bytes()).unwrap().build();
    let r = HttpClient::new().with_tls_config(cfg).send(&Request::new("GET", Url::parse(&format!("{}/", mock.url())).unwrap()));
    assert!(r.unwrap_err().is_a("SSLHandshakeException"));
}

#[test]
fn a_global_trust_all_setting_is_refused_too() {
    let mock = AwsMock::start_tls().unwrap();
    mock.create_bucket("secure");
    let ctx = context();
    ctx.properties().set("camel.ssl.enabled", "true");
    ctx.properties().set("camel.ssl.trustAllCertificates", "true");
    let t = ctx.create_producer_template();
    let e = t.send_body_and_header(&format!("aws2-s3://secure?{}", mock.endpoint_options()), "x", "CamelAwsS3Key", "k").unwrap_err();
    let text = format!("{} / {}", e, e.root_cause());
    assert!(text.contains("always verifies"), "{}", text);
}

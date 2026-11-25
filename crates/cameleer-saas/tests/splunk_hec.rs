//! `splunk-hec` against the embedded HTTP Event Collector mock (HTTP and HTTPS).

use std::sync::Arc;

use cameleer_core::prelude::*;
use cameleer_http::ServerConfig;
use cameleer_saas::splunk_hec::INDEX_TIME;
use cameleer_saas::testing::SplunkHecMock;

const TOKEN: &str = SplunkHecMock::TOKEN;

fn ctx() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    cameleer_saas::register(&ctx);
    ctx.start().unwrap();
    ctx
}

/// The error and its causes (the template wraps a route failure in `CamelExecutionException`).
fn err_text(e: CamelError) -> String {
    let mut s = format!("{}: {}", e.kind, e.message);
    let mut c = e.cause.as_deref();
    while let Some(x) = c {
        s.push_str(&format!(" / {}: {}", x.kind, x.message));
        c = x.cause.as_deref();
    }
    s
}

#[test]
fn posts_camel_envelope_with_token_header() {
    let mock = SplunkHecMock::start();
    let ctx = ctx();
    let uri = format!("splunk-hec:{}?token={}&https=false&host=web-01&index=main&source=app&sourceType=_json", mock.splunk_url(), TOKEN);
    let t = ctx.create_producer_template();
    t.send_body_and_header(&uri, "user logged in", "user", "ann").unwrap();
    let reqs = mock.server.requests();
    assert_eq!(reqs.len(), 1);
    let r = &reqs[0];
    assert_eq!(r.method, "POST");
    assert_eq!(r.path, "/services/collector/event");
    assert_eq!(r.header("Authorization"), Some(format!("Splunk {}", TOKEN).as_str()));
    assert!(r.header("Content-Type").unwrap().starts_with("application/json"));
    let ev = r.json();
    assert_eq!(ev.get("host"), Some(&Value::from("web-01")));
    assert_eq!(ev.get("index"), Some(&Value::from("main")));
    assert_eq!(ev.get("source"), Some(&Value::from("app")));
    assert_eq!(ev.get("sourcetype"), Some(&Value::from("_json")));
    assert!(ev.get("time").is_none(), "no time unless configured");
    let event = ev.get("event").unwrap();
    assert_eq!(event.get("body"), Some(&Value::from("user logged in")));
    assert_eq!(event.get("headers").and_then(|h| h.get("user")), Some(&Value::from("ann")));
    assert_eq!(mock.events().len(), 1);
}

#[test]
fn defaults_body_only_headers_only_and_time() {
    let mock = SplunkHecMock::start();
    let ctx = ctx();
    let t = ctx.create_producer_template();
    let base = format!("splunk-hec:{}?token={}&https=false", mock.splunk_url(), TOKEN);

    // defaults: index, source and sourcetype "camel", host = this machine
    let mut obj = cameleer_core::value::ObjectMap::new();
    obj.insert("level".into(), Value::from("WARN"));
    obj.insert("count".into(), Value::Int(3));
    t.send_body(&format!("{}&bodyOnly=true&time=1700000000", base), Value::Object(obj)).unwrap();
    let ev = mock.server.requests()[0].json();
    for k in ["index", "source", "sourcetype"] {
        assert_eq!(ev.get(k), Some(&Value::from("camel")), "{k}");
    }
    assert_eq!(ev.get("host").and_then(|h| h.as_str()), Some(cameleer_saas::splunk_hec::local_host_name().as_str()));
    assert_eq!(ev.get("time"), Some(&Value::Int(1700000000)));
    assert_eq!(ev.get("event").and_then(|e| e.get("count")), Some(&Value::Int(3)));
    assert!(ev.get("event").and_then(|e| e.get("headers")).is_none());

    // the header overrides the time option; bytes are sent as text
    t.send_body_and_header(&format!("{}&bodyOnly=true&time=1", base), Value::Bytes(b"raw line".to_vec()), INDEX_TIME, Value::Int(1700000123)).unwrap();
    let ev = mock.server.requests()[1].json();
    assert_eq!(ev.get("time"), Some(&Value::Int(1700000123)));
    assert_eq!(ev.get("event"), Some(&Value::from("raw line")));

    // headersOnly: the event is the headers
    t.send_body_and_header(&format!("{}&headersOnly=true", base), "ignored", "region", "eu").unwrap();
    let ev = mock.server.requests()[2].json();
    assert_eq!(ev.get("event").and_then(|e| e.get("region")), Some(&Value::from("eu")));
    assert!(ev.get("event").and_then(|e| e.get("body")).is_none());

    // a list body is one event holding the array
    t.send_body(&format!("{}&bodyOnly=true", base), Value::Array(vec![Value::Int(1), Value::Int(2)])).unwrap();
    let ev = mock.server.requests()[3].json();
    assert_eq!(ev.get("event"), Some(&Value::Array(vec![Value::Int(1), Value::Int(2)])));

    // splunkEndpoint with a query string, and the raw endpoint
    t.send_body(&format!("{}&bodyOnly=true&splunkEndpoint=/services/collector/event?auto_extract_timestamp=true", base), "x").unwrap();
    let r = &mock.server.requests()[4];
    assert_eq!(r.path, "/services/collector/event");
    assert_eq!(r.query_param("auto_extract_timestamp"), Some("true"));
    t.send_body(&format!("{}&bodyOnly=true&splunkEndpoint=/services/collector/raw", base), "raw").unwrap();
    assert_eq!(mock.raw().len(), 1);
    assert!(mock.raw()[0].contains("\"event\":\"raw\""));
}

#[test]
fn non_200_answers_fail_with_the_status_and_body() {
    let mock = SplunkHecMock::start();
    let ctx = ctx();
    let t = ctx.create_producer_template();
    let other = "99999999-2222-3333-4444-555555555555";
    let e = err_text(t.send_body(&format!("splunk-hec:{}?token={}&https=false", mock.splunk_url(), other), "x").unwrap_err());
    assert!(e.contains("RuntimeCamelException: Splunk HEC request failed: HTTP/1.1 403 Forbidden\n"), "{e}");
    assert!(e.contains("Invalid token"), "{e}");
    let e = err_text(t.send_body(&format!("splunk-hec:{}?token={}&https=false&index=nope", mock.splunk_url(), TOKEN), "x").unwrap_err());
    assert!(e.contains("400 Bad Request") && e.contains("Incorrect index"), "{e}");
    mock.set_busy(true);
    let e = err_text(t.send_body(&format!("splunk-hec:{}?token={}&https=false", mock.splunk_url(), TOKEN), "x").unwrap_err());
    assert!(e.contains("503") && e.contains("Server is busy"), "{e}");
    // the kind is a RuntimeException for onException
    assert!(cameleer_core::error::kind_is_a(cameleer_saas::splunk_hec::RUNTIME_CAMEL_EXCEPTION, "RuntimeException"));
}

#[test]
fn endpoint_validation_follows_camel_and_refuses_skip_tls_verify() {
    let ctx = ctx();
    // the URL and the token are checked when the producer is created (route start)
    let e = |uri: &str| match ctx.get_endpoint(uri) {
        Err(e) => err_text(e),
        Ok(ep) => err_text(ep.create_producer().err().expect(uri)),
    };
    assert!(e("splunk-hec:localhost:8088").contains("A token must be defined"));
    assert!(e("splunk-hec:localhost:8088?token=abc").contains("Invalid Splunk HEC token provided"));
    assert!(e(&format!("splunk-hec:localhost?token={}", TOKEN)).contains("Invalid Splunk URL"));
    assert!(e(&format!("splunk-hec:localhost:99999?token={}", TOKEN)).contains("Invalid port"));
    let s = e(&format!("splunk-hec:localhost:8088?token={}&skipTlsVerify=true", TOKEN));
    assert!(s.contains("skipTlsVerify is not supported"), "{s}");
    ctx.registry().bind("insecure", cameleer_tls::SslContextParameters::new().insecure(true));
    let s = e(&format!("splunk-hec:localhost:8088?token={}&sslContextParameters=#insecure", TOKEN));
    assert!(s.contains("skipTlsVerify is not supported"), "{s}");
    assert!(ctx.get_endpoint(&format!("splunk-hec://localhost:8088?token={}&skipTlsVerify=false", TOKEN)).unwrap().create_producer().is_ok());
}

#[test]
fn https_verifies_the_server_certificate() {
    let server = cameleer_tls::testing::server_params().server_config().unwrap();
    let mock = SplunkHecMock::start_with(ServerConfig { tls: Some(Arc::new(server)), ..ServerConfig::default() }, &["camel"]);
    let ctx = ctx();
    ctx.registry().bind("hecTrust", cameleer_tls::testing::client_params());
    let t = ctx.create_producer_template();
    t.send_body(&format!("splunk-hec:{}?token={}&sslContextParameters=#hecTrust", mock.splunk_url(), TOKEN), "over tls").unwrap();
    assert_eq!(mock.events()[0].get("event").and_then(|e| e.get("body")), Some(&Value::from("over tls")));
    // without trusting the test CA the handshake fails, and nothing is accepted
    ctx.registry().bind("other", cameleer_tls::testing::untrusting_client_params());
    let e = t.send_body(&format!("splunk-hec:{}?token={}&sslContextParameters=#other", mock.splunk_url(), TOKEN), "x").unwrap_err();
    let e = err_text(e);
    assert!(e.contains("SSL") || e.to_ascii_lowercase().contains("certificate"), "{e}");
    assert_eq!(mock.events().len(), 1);
}

//! Conformance tests for platform-http / http / rest following the Apache Camel 4 docs.

use std::time::Duration;

use camel_core::prelude::*;
use camel_http::rest::{RestBindingMode, RestDslExt};
use camel_http::HttpClient;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    camel_http::register(&ctx);
    ctx.properties().set("camel.server.port", "0");
    ctx
}

fn base(ctx: &CamelContext) -> String {
    format!("http://127.0.0.1:{}", camel_http::platform_http_port(ctx).expect("port"))
}

fn client() -> HttpClient {
    HttpClient { read_timeout: Duration::from_secs(10), ..HttpClient::new() }
}

#[test]
fn platform_http_and_http_producer_round_trip_with_headers() {
    let ctx = context();
    ctx.add_routes(|b| {
        b.from("platform-http:/greet").set_body(simple("Hello ${header.name} (${header.X-Custom}) ${body}")).set_header("X-Reply", constant("r"));
        b.from("direct:call").to_d("http://127.0.0.1:${exchangeProperty.port}/greet?name=${header.who}");
    })
    .unwrap();
    ctx.start().unwrap();
    let port = camel_http::platform_http_port(&ctx).unwrap();
    let t = ctx.create_producer_template();
    let ex = t
        .request("direct:call", |e| {
            e.set_property("port", port);
            e.set_body("body");
            e.set_header("who", "Camel");
            e.set_header("X-Custom", "cv");
        })
        .unwrap();
    assert_eq!(ex.body(), &Value::from("Hello Camel (cv) body"));
    assert_eq!(ex.header("CamelHttpResponseCode"), Some(&Value::Int(200)));
    assert_eq!(ex.header("X-Reply"), Some(&Value::from("r")));
    ctx.stop().unwrap();
}

#[test]
fn response_code_header_sets_http_status() {
    let ctx = context();
    ctx.add_routes(|b| {
        b.from("platform-http:/accepted").set_header("CamelHttpResponseCode", constant(202)).set_body(constant("queued"));
    })
    .unwrap();
    ctx.start().unwrap();
    let r = client().post(&format!("{}/accepted", base(&ctx)), "text/plain", b"job").unwrap();
    assert_eq!(r.status, 202);
    assert_eq!(r.body_text(), "queued");
    ctx.stop().unwrap();
}

#[test]
fn unhandled_exception_returns_500() {
    let ctx = context();
    ctx.add_routes(|b| {
        b.from("platform-http:/fail").throw_exception("IllegalStateException", "Forced failure");
    })
    .unwrap();
    ctx.start().unwrap();
    let r = client().get(&format!("{}/fail", base(&ctx))).unwrap();
    assert_eq!(r.status, 500);
    assert!(r.body_text().contains("Forced failure"), "{}", r.body_text());
    ctx.stop().unwrap();
}

#[test]
fn handled_exception_returns_200_with_handled_body() {
    let ctx = context();
    ctx.add_routes(|b| {
        b.on_exception(&["IllegalArgumentException"]).handled(true).set_body(simple("Invalid: ${exception.message}"));
        b.on_exception(&["IOException"])
            .handled(true)
            .set_header("CamelHttpResponseCode", constant(503))
            .set_body(constant("try later"));
        b.from("platform-http:/validate").throw_exception("IllegalArgumentException", "bad input");
        b.from("platform-http:/unavailable").throw_exception("IOException", "down");
    })
    .unwrap();
    ctx.start().unwrap();
    let c = client();
    let r = c.get(&format!("{}/validate", base(&ctx))).unwrap();
    assert_eq!((r.status, r.body_text().as_str()), (200, "Invalid: bad input"));
    let r = c.get(&format!("{}/unavailable", base(&ctx))).unwrap();
    assert_eq!((r.status, r.body_text().as_str()), (503, "try later"));
    ctx.stop().unwrap();
}

#[test]
fn http_producer_failure_handled_by_on_exception() {
    let ctx = context();
    ctx.add_routes(|b| {
        b.from("platform-http:/missing").set_header("CamelHttpResponseCode", constant(404)).set_body(constant("nope"));
        b.from("direct:client")
            .on_exception(&["HttpOperationFailedException"])
            .handled(true)
            .set_body(simple("fallback after ${exception.message}"))
            .end()
            .to_d("http://127.0.0.1:${exchangeProperty.port}/missing");
    })
    .unwrap();
    ctx.start().unwrap();
    let port = camel_http::platform_http_port(&ctx).unwrap();
    let ex = ctx.create_producer_template().request("direct:client", |e| e.set_property("port", port)).unwrap();
    let s = ex.body().to_string();
    assert!(s.starts_with("fallback after "), "{}", s);
    assert!(s.contains("404"), "{}", s);
    ctx.stop().unwrap();
}

#[test]
fn http_producer_failure_is_camel_execution_exception_with_cause() {
    let ctx = context();
    ctx.add_routes(|b| {
        b.from("platform-http:/err").throw_exception("IllegalStateException", "server side");
    })
    .unwrap();
    ctx.start().unwrap();
    let e = ctx.create_producer_template().request_body(&format!("{}/err", base(&ctx)), Value::Null).unwrap_err();
    assert_eq!(e.kind, kinds::CAMEL_EXECUTION);
    let cause = e.cause.unwrap();
    assert_eq!(cause.kind, "HttpOperationFailedException");
    assert_eq!(camel_http::HttpOperationFailed::status_code_of(&cause), Some(500));
    ctx.stop().unwrap();
}

#[test]
fn http_method_header_and_query_header_override() {
    let ctx = context();
    ctx.add_routes(|b| {
        b.from("platform-http:/echo").set_body(simple("${header.CamelHttpMethod} q=${header.q}"));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let url = format!("{}/echo", base(&ctx));
    let r = t.request_body_and_headers(&url, "x", vec![("CamelHttpMethod", Value::from("PUT")), ("CamelHttpQuery", Value::from("q=1"))]).unwrap();
    assert_eq!(r, Value::from("PUT q=1"));
    let r = t.request_body(&format!("{}?q=2", url), Value::Null).unwrap();
    assert_eq!(r, Value::from("GET q=2"));
    ctx.stop().unwrap();
}

#[test]
fn camel_internal_headers_are_not_sent_on_the_wire() {
    let ctx = context();
    ctx.add_routes(|b| {
        b.from("platform-http:/headers").process(|ex: &mut Exchange| {
            let has_internal = ex.header("CamelSecret").is_some();
            ex.set_body(format!("internal={} custom={}", has_internal, ex.header("X-Mine").map(|v| v.to_string()).unwrap_or_default()));
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let r = t
        .request_body_and_headers(&format!("{}/headers", base(&ctx)), Value::Null, vec![("CamelSecret", Value::from("s")), ("X-Mine", Value::from("m"))])
        .unwrap();
    assert_eq!(r, Value::from("internal=false custom=m"));
    ctx.stop().unwrap();
}

#[test]
fn rest_dsl_json_binding_round_trip_and_error_500() {
    let ctx = context();
    ctx.add_routes(|b| {
        b.rest_configuration().component("platform-http").host("127.0.0.1").port(0).binding_mode(RestBindingMode::Json);
        b.rest("/orders").post("").to("direct:create").get("/{id}").to("direct:get").get("/fail/now").to("direct:fail");
        b.from("direct:create").process(|ex: &mut Exchange| {
            let item = ex.body().get("item").cloned().unwrap_or_default();
            ex.set_body(Value::object([("item", item), ("status", Value::from("created"))]));
            Ok(())
        });
        b.from("direct:get").set_body(simple("${header.id}")).convert_body_to("Integer").process(|ex: &mut Exchange| {
            let id = ex.body().clone();
            ex.set_body(Value::object([("id", id)]));
            Ok(())
        });
        b.from("direct:fail").throw_exception("IllegalStateException", "rest failure");
    })
    .unwrap();
    ctx.start().unwrap();
    let port = camel_http::platform_http_port(&ctx).expect("rest port");
    let base = format!("http://127.0.0.1:{}", port);
    let c = client();
    let r = c.post(&format!("{}/orders", base), "application/json", br#"{"item":"book"}"#).unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(r.headers.get("Content-Type"), Some("application/json"));
    let v = camel_core::json::parse(&r.body_text()).unwrap();
    assert_eq!(v.get("item"), Some(&Value::from("book")));
    assert_eq!(v.get("status"), Some(&Value::from("created")));
    let r = c.get(&format!("{}/orders/7", base)).unwrap();
    assert_eq!(camel_core::json::parse(&r.body_text()).unwrap().get("id"), Some(&Value::Int(7)));
    let r = c.get(&format!("{}/orders/fail/now", base)).unwrap();
    assert_eq!(r.status, 500);
    ctx.stop().unwrap();
}

#[test]
fn https_endpoint_connection_failure_is_a_connect_exception() {
    // https is supported (see tests/tls.rs); an unreachable target fails like plain http
    let ctx = context();
    ctx.start().unwrap();
    let e = ctx.create_producer_template().request_body("https://127.0.0.1:1/x", Value::Null).unwrap_err();
    assert!(e.chain().any(|c| c.is_a("ConnectException")), "{:?}", e);
}

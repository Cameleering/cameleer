//! Service Call EIP with HTTP backends (default `http` uri building and uri templates).

use cameleer_core::prelude::*;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    ctx.properties().set("camel.server.port", "0");
    ctx
}

#[test]
fn service_call_round_robin_over_http_backends() {
    let ctx = context();
    ctx.add_routes(|b| {
        b.from("platform-http:/hello").transform(simple("hello ${body} via ${header.Host}"));
        b.from("platform-http:/other").transform(constant("other"));
    })
    .unwrap();
    ctx.start().unwrap();
    let p = cameleer_http::platform_http_port(&ctx).expect("port");
    let servers = format!("svc@127.0.0.1:{p},svc@localhost:{p}");
    let s2 = servers.clone();
    ctx.add_routes(move |b| {
        // default uri: http://{host}:{port}/{context path from the name}
        b.from("direct:default").service_call("svc/hello").static_service_discovery(&servers);
        // uri containing the service name: the name is replaced by host:port
        b.from("direct:named").service_call("svc").static_service_discovery(&s2).service_call_uri("http://svc/other");
    })
    .unwrap();
    let t = ctx.create_producer_template();
    let a = t.request_body("direct:default", "a").unwrap().to_string();
    let b = t.request_body("direct:default", "b").unwrap().to_string();
    assert_eq!(a, format!("hello a via 127.0.0.1:{}", p));
    assert_eq!(b, format!("hello b via localhost:{}", p));
    assert_eq!(t.request_body("direct:named", "x").unwrap().to_string(), "other");
}

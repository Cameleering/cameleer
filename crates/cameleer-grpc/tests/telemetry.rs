//! Trace context over gRPC metadata: the client's span context goes out as `traceparent`
//! metadata and the server route continues the trace.

use std::sync::Arc;
use std::time::Duration;

use cameleer_core::prelude::*;
use cameleer_core::telemetry::span::InMemorySpanProcessor;
use cameleer_core::telemetry::{self, AttrValue, TelemetryConfig};

const PROTO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/resources/greeter.proto");

#[test]
fn grpc_calls_carry_the_trace_in_their_metadata() {
    let ctx = CamelContext::new();
    cameleer_grpc::register(&ctx);
    let spans = Arc::new(InMemorySpanProcessor::new());
    telemetry::install(&ctx, TelemetryConfig::default(), spans.clone());
    let consumer = format!("grpc://127.0.0.1:0/helloworld.Greeter?protoFile={}", PROTO);
    let c2 = consumer.clone();
    ctx.add_routes(move |b| {
        b.from(&c2).route_id("greeter").process(|ex: &mut Exchange| {
            let name = ex.body().get("name").map(|v| v.to_string()).unwrap_or_default();
            ex.set_body(Value::object([("message", Value::from(format!("Hello {}", name)))]));
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let port = cameleer_grpc::local_port(&ctx, &consumer).unwrap();
    let uri = format!("grpc://127.0.0.1:{}/helloworld.Greeter?method=sayHello&protoFile={}&synchronous=true", port, PROTO);
    let u2 = uri.clone();
    ctx.add_routes(move |b| {
        b.from("direct:call").route_id("caller").to(u2.clone());
    })
    .unwrap();
    let reply = ctx.create_producer_template().request_body("direct:call", Value::object([("name", Value::from("Otel"))])).unwrap();
    assert_eq!(reply.get("message"), Some(&Value::from("Hello Otel")));
    let s = spans.wait_for(4, Duration::from_secs(5));
    let route = |id: &str| s.iter().find(|x| x.attribute("op") == Some(&AttrValue::from("EVENT_RECEIVED")) && x.attribute("camel.route.id") == Some(&AttrValue::from(id))).unwrap();
    let call = s.iter().find(|x| x.name == "grpc" && x.attribute("op") == Some(&AttrValue::from("EVENT_SENT"))).unwrap();
    let server = route("greeter");
    assert_eq!(call.parent_span_id, route("caller").span_id);
    assert_eq!(server.trace_id, call.trace_id);
    assert_eq!(server.parent_span_id, call.span_id);
    assert!(server.parent_remote);
    ctx.stop().unwrap();
}

//! Trace context over AMQP 1.0 (the `amqp` JMS component): the producer's span context goes
//! out as application properties and the consumer route continues the trace.

use std::sync::Arc;
use std::time::Duration;

use cameleer_amqp10::testing::Amqp10Broker;
use cameleer_core::prelude::*;
use cameleer_core::telemetry::span::InMemorySpanProcessor;
use cameleer_core::telemetry::{self, AttrValue, SpanKind, TelemetryConfig};

#[test]
fn amqp_messages_carry_the_trace_in_their_properties() {
    let broker = Amqp10Broker::start().unwrap();
    let ctx = CamelContext::new();
    cameleer_amqp::register(&ctx);
    cameleer_amqp10::register(&ctx);
    ctx.properties().set("camel.component.amqp.remoteAddress", broker.url());
    let spans = Arc::new(InMemorySpanProcessor::new());
    telemetry::install(&ctx, TelemetryConfig::default(), spans.clone());
    ctx.add_routes(|b| {
        b.from("direct:send").route_id("send").to("amqp:queue:orders");
        b.from("amqp:queue:orders").route_id("receive").to("mock:orders");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:orders").unwrap();
    ctx.create_producer_template().send_body("direct:send", "order-1").unwrap();
    for _ in 0..200 {
        if mock.received_counter() == 1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(mock.received_counter(), 1);
    let s = spans.wait_for(5, Duration::from_secs(5));
    let produce = s.iter().find(|x| x.kind == SpanKind::Producer).unwrap();
    let consume = s.iter().find(|x| x.kind == SpanKind::Consumer).unwrap_or_else(|| panic!("{s:#?}"));
    assert_eq!((produce.name.as_str(), consume.name.as_str()), ("orders", "orders"));
    assert_eq!(consume.attribute("camel.route.id"), Some(&AttrValue::from("receive")));
    assert_eq!(consume.attribute("messaging.destination.name"), Some(&AttrValue::from("orders")));
    assert_eq!(consume.trace_id, produce.trace_id);
    assert_eq!(consume.parent_span_id, produce.span_id);
    assert!(consume.parent_remote);
    ctx.stop().unwrap();
}

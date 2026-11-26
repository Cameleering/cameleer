//! Trace context over JMS on STOMP (the `jms` component): the producer's span context goes out
//! as message headers and the consumer route continues the trace.

use std::sync::Arc;
use std::time::Duration;

use cameleer_amqp::testing::StompBroker;
use cameleer_core::prelude::*;
use cameleer_core::telemetry::span::InMemorySpanProcessor;
use cameleer_core::telemetry::{self, SpanKind, TelemetryConfig};

#[test]
fn jms_messages_carry_the_trace_in_their_headers() {
    let broker = StompBroker::start().unwrap();
    let ctx = CamelContext::new();
    cameleer_amqp::register(&ctx);
    ctx.properties().set("camel.component.jms.brokerUrl", broker.url());
    let spans = Arc::new(InMemorySpanProcessor::new());
    telemetry::install(&ctx, TelemetryConfig::default(), spans.clone());
    ctx.add_routes(|b| {
        b.from("direct:send").route_id("send").to("jms:queue:invoices");
        b.from("jms:queue:invoices").route_id("receive").to("mock:invoices");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:invoices").unwrap();
    ctx.create_producer_template().send_body("direct:send", "invoice-1").unwrap();
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
    assert_eq!((produce.name.as_str(), consume.name.as_str()), ("invoices", "invoices"));
    assert_eq!(consume.trace_id, produce.trace_id);
    assert_eq!(consume.parent_span_id, produce.span_id);
    ctx.stop().unwrap();
}

//! OpenTelemetry end to end: a trace started by an HTTP client crosses an HTTP route, a Kafka
//! topic (embedded broker) and an HTTP call into another service, and every service exports its
//! spans over OTLP/HTTP to one receiver; and `Main` turns it on from properties.
#![cfg(all(feature = "opentelemetry", feature = "kafka-mqtt"))]

use std::time::Duration;

use cameleer_core::prelude::*;
use cameleer_core::telemetry::context::hex;
use cameleer_core::telemetry::{AttrValue, SpanKind, TelemetryConfig};
use cameleer_http::HttpClient;
use cameleer_kafka_mqtt::testing::KafkaBroker;
use cameleer_opentelemetry::proto::ReceivedSpan;
use cameleer_opentelemetry::testing::OtlpReceiver;
use cameleer_opentelemetry::OtlpConfig;

fn otlp(rx: &OtlpReceiver, service: &str) -> OtlpConfig {
    let endpoint = rx.endpoint();
    let service = service.to_string();
    let get = move |k: &str| match k {
        "otel.exporter.otlp.endpoint" => Some(endpoint.clone()),
        "otel.service.name" => Some(service.clone()),
        "otel.bsp.schedule.delay" => Some("50".to_string()),
        _ => None,
    };
    OtlpConfig::from_lookup(&get).unwrap()
}

fn service_of(s: &ReceivedSpan) -> String {
    s.resource.iter().find(|(k, _)| k == "service.name").map(|(_, v)| v.to_string()).unwrap_or_default()
}

const CLIENT_TRACE: &str = "0af7651916cd43dd8448eb211c80319c";
const CLIENT_SPAN: &str = "b7ad6b7169203331";

#[test]
fn one_trace_crosses_http_kafka_and_http() {
    let broker = KafkaBroker::start().unwrap();
    let rx = OtlpReceiver::start();

    // the edge service: HTTP in, Kafka out
    let edge = CamelContext::new();
    cameleer_http::register(&edge);
    cameleer_kafka_mqtt::register(&edge);
    edge.add_component("platform-http", cameleer_http::platform::HttpConsumerComponent::new("platform-http").with_port(0));
    let edge_otel = cameleer_opentelemetry::install_with(&edge, TelemetryConfig::default(), otlp(&rx, "edge")).unwrap();
    let to_kafka = format!("kafka:orders?brokers={}", broker.bootstrap());
    edge.add_routes(move |b| {
        b.from("platform-http:/orders").route_id("ingest").convert_body_to("String").to(to_kafka.clone()).set_body(constant("queued"));
    })
    .unwrap();
    edge.start().unwrap();
    let port = cameleer_http::platform_http_port(&edge).unwrap();

    // the billing service: Kafka in, HTTP out to the invoicing service
    let billing = CamelContext::new();
    cameleer_http::register(&billing);
    cameleer_kafka_mqtt::register(&billing);
    let billing_otel = cameleer_opentelemetry::install_with(&billing, TelemetryConfig::default(), otlp(&rx, "billing")).unwrap();
    let from_kafka = format!("kafka:orders?brokers={}&groupId=billing&autoOffsetReset=earliest", broker.bootstrap());
    let to_invoicing = format!("http://127.0.0.1:{port}/invoices");
    billing
        .add_routes(move |b| {
            b.from(from_kafka.clone()).route_id("bill").remove_headers("kafka*").to(to_invoicing.clone()).to("mock:billed");
        })
        .unwrap();
    billing.start().unwrap();

    // the invoicing service (on the edge's HTTP server)
    let invoicing = CamelContext::new();
    cameleer_http::register(&invoicing);
    invoicing.add_component("platform-http", cameleer_http::platform::HttpConsumerComponent::new("platform-http").with_port(port));
    let invoicing_otel = cameleer_opentelemetry::install_with(&invoicing, TelemetryConfig::default(), otlp(&rx, "invoicing")).unwrap();
    invoicing
        .add_routes(|b| {
            b.from("platform-http:/invoices").route_id("invoice").set_body(constant("invoiced"));
        })
        .unwrap();
    invoicing.start().unwrap();

    let traceparent = format!("00-{CLIENT_TRACE}-{CLIENT_SPAN}-01");
    let r = HttpClient::new()
        .request(
            "POST",
            &format!("http://127.0.0.1:{port}/orders"),
            &[("Content-Type", "application/json"), ("traceparent", &traceparent), ("tracestate", "vendor=x1"), ("baggage", "tenant=acme")],
            Some(b"{\"orderId\":1}"),
            Some(Duration::from_secs(20)),
        )
        .unwrap();
    assert_eq!((r.status, r.body_text()), (200, "queued".to_string()));
    let billed = billing.mock_endpoint("mock:billed").unwrap();
    for _ in 0..400 {
        if billed.received_counter() >= 1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(billed.received_counter(), 1);
    assert_eq!(billed.received_exchanges()[0].body().to_string(), "invoiced");
    for o in [&edge_otel, &billing_otel, &invoicing_otel] {
        assert!(o.force_flush(Duration::from_secs(10)));
    }
    let spans = rx.wait_for_spans(6, Duration::from_secs(10));
    assert_eq!(spans.len(), 6);
    let find = |service: &str, kind: SpanKind, route: &str| -> &ReceivedSpan {
        spans
            .iter()
            .find(|s| service_of(s) == service && s.span.kind == kind && s.span.attribute("camel.route.id") == Some(&AttrValue::from(route)))
            .unwrap_or_else(|| panic!("no {kind:?} span of {service}/{route} in {:#?}", spans.iter().map(|s| (service_of(s), &s.span.name, s.span.kind)).collect::<Vec<_>>()))
    };
    let ingest = find("edge", SpanKind::Server, "ingest");
    let produce = find("edge", SpanKind::Producer, "ingest");
    let consume = find("billing", SpanKind::Consumer, "bill");
    let call = find("billing", SpanKind::Client, "bill");
    let invoice = find("invoicing", SpanKind::Server, "invoice");

    // one trace, the client's, from end to end
    for s in [ingest, produce, consume, call, invoice] {
        assert_eq!(hex(&s.span.trace_id), CLIENT_TRACE, "{}", s.span.name);
        assert_eq!(s.span.trace_state, "vendor=x1", "{}", s.span.name);
    }
    // each hop the child of the one before, the cross-process ones marked remote
    assert_eq!(hex(&ingest.span.parent_span_id), CLIENT_SPAN);
    assert!(ingest.span.parent_remote);
    assert_eq!(produce.span.parent_span_id, ingest.span.span_id);
    assert!(!produce.span.parent_remote);
    assert_eq!(consume.span.parent_span_id, produce.span.span_id);
    assert!(consume.span.parent_remote);
    assert_eq!(call.span.parent_span_id, consume.span.span_id);
    assert_eq!(invoice.span.parent_span_id, call.span.span_id);
    assert!(invoice.span.parent_remote);

    // names and attributes of Camel's decorators
    assert_eq!(ingest.span.name, "POST");
    assert_eq!(ingest.span.attribute("http.method"), Some(&AttrValue::from("POST")));
    assert_eq!(produce.span.name, "orders");
    assert_eq!(produce.span.attribute("messaging.destination.name"), Some(&AttrValue::from("orders")));
    assert!(produce.span.attribute("kafka.offset").is_some(), "{:?}", produce.span.attributes);
    assert_eq!(consume.span.name, "orders");
    assert_eq!(consume.span.attribute("kafka.partition"), Some(&AttrValue::Int(0)));
    assert_eq!(call.span.name, "POST");
    assert_eq!(call.span.attribute("http.status_code"), Some(&AttrValue::Int(200)));
    assert_eq!(call.span.attribute("http.url"), Some(&AttrValue::String(format!("http://127.0.0.1:{port}/invoices"))));
    assert_eq!(invoice.span.attribute("component"), Some(&AttrValue::from("camel-platform-http")));

    for c in [&invoicing, &billing, &edge] {
        c.stop().unwrap();
    }
    for o in [edge_otel, billing_otel, invoicing_otel] {
        o.shutdown(Duration::from_secs(5));
    }
}

#[test]
fn main_turns_tracing_on_from_properties() {
    let rx = OtlpReceiver::start();
    let mut main = cameleer_main::new_main();
    main.set_property("camel.opentelemetry.enabled", "true");
    main.set_property("camel.opentelemetry.trace-processors", "true");
    main.set_property("otel.exporter.otlp.endpoint", rx.endpoint());
    main.set_property("otel.service.name", "main-test");
    main.set_property("otel.bsp.schedule.delay", "50");
    main.add_route_source(
        "otel.camel.yaml",
        r#"
- route:
    id: tick
    from:
      uri: timer:tick?repeatCount=1&delay=10
      steps:
        - setBody:
            constant: hello
        - to: mock:ticked
"#,
    );
    main.start().unwrap();
    let ticked = main.context().mock_endpoint("mock:ticked").unwrap();
    for _ in 0..200 {
        if ticked.received_counter() >= 1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(ticked.received_counter(), 1);
    main.stop().unwrap();
    // stopping flushed the exporter
    let spans = rx.spans();
    let names: Vec<&str> = spans.iter().map(|s| s.span.name.as_str()).collect();
    assert!(names.contains(&"timer") && names.contains(&"mock"), "{names:?}");
    assert!(names.iter().any(|n| n.ends_with("-setBody")), "{names:?}");
    assert!(spans.iter().all(|s| service_of(s) == "main-test"));
    let timer = spans.iter().find(|s| s.span.name == "timer").unwrap();
    assert!(timer.span.is_root());
    assert!(spans.iter().all(|s| s.span.trace_id == timer.span.trace_id));
}

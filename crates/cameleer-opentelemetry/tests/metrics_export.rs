//! OTLP metrics: the context's meters exported periodically over OTLP/HTTP to the embedded
//! receiver.

use std::time::Duration;

use cameleer_core::metrics::instrument::{self, MetricsConfig};
use cameleer_core::prelude::*;
use cameleer_core::telemetry::span::AttrValue;
use cameleer_opentelemetry::metrics::Point;
use cameleer_opentelemetry::testing::OtlpReceiver;

#[test]
fn meters_are_exported_over_otlp() {
    let rx = OtlpReceiver::start();
    let ctx = CamelContext::new();
    instrument::install(&ctx, MetricsConfig::default());
    let endpoint = rx.endpoint();
    let props = move |k: &str| match k {
        "otel.metrics.exporter" => Some("otlp".to_string()),
        "otel.exporter.otlp.endpoint" => Some(endpoint.clone()),
        "otel.service.name" => Some("billing".to_string()),
        "otel.metric.export.interval" => Some("50".to_string()),
        "otel.exporter.otlp.metrics.headers" => Some("x-team=pay".to_string()),
        _ => None,
    };
    let exp = cameleer_opentelemetry::install_metrics(&ctx, &props).unwrap().expect("enabled by OTEL_METRICS_EXPORTER=otlp");
    ctx.add_routes(|b| {
        b.from("direct:bill").route_id("bill").to("micrometer:counter:invoices?tags.currency=EUR").to("micrometer:summary:invoice.amount?value=${body}");
    })
    .unwrap();
    ctx.start().unwrap();
    for v in [10, 30] {
        ctx.create_producer_template().send_body("direct:bill", v).unwrap();
    }
    assert!(exp.force_flush(Duration::from_secs(10)));
    let exports = rx.metric_exports();
    let last = exports.last().expect("an export");
    let req = rx.requests().into_iter().rev().find(|r| r.path == "/v1/metrics").unwrap();
    assert_eq!(req.header("x-team"), Some("pay"));
    let m = |n: &str| last.iter().find(|m| m.name == n).unwrap_or_else(|| panic!("{n}: {:?}", last.iter().map(|m| &m.name).collect::<Vec<_>>()));
    assert!(last[0].resource.contains(&("service.name".to_string(), AttrValue::from("billing"))));
    assert_eq!(last[0].scope, "camel");
    let inv = m("invoices");
    assert_eq!((inv.kind, inv.monotonic), ("sum", true));
    assert_eq!(inv.points[0].1, Point::Double(2.0));
    assert!(inv.points[0].0.contains(&("currency".to_string(), AttrValue::from("EUR"))));
    match &m("invoice.amount").points[0].1 {
        Point::Histogram { count, sum, max, .. } => assert_eq!((*count, *sum, *max), (2, 40.0, 30.0)),
        p => panic!("{p:?}"),
    }
    let policy = m("camel.route.policy");
    assert_eq!((policy.kind, policy.unit.as_str()), ("histogram", "s"));
    assert!(policy.points.iter().any(|(a, _, _, _)| a.contains(&("routeId".to_string(), AttrValue::from("bill")))));
    assert!(m("camel.exchanges.total").points.iter().any(|(_, p, _, _)| *p == Point::Double(2.0)));
    // cumulative: every point starts when the registry was created
    let start = inv.points[0].2;
    assert!(last.iter().all(|m| m.points.iter().all(|p| p.2 == start && p.3 >= start)));
    ctx.stop().unwrap();
    exp.shutdown(Duration::from_secs(5));
    assert!(exp.stats().exports.load(std::sync::atomic::Ordering::Relaxed) >= 1);
}

#[test]
fn metrics_export_is_off_unless_asked_and_survives_a_dead_collector() {
    let ctx = CamelContext::new();
    assert!(cameleer_opentelemetry::install_metrics(&ctx, &|_: &str| None).unwrap().is_none());
    assert!(cameleer_opentelemetry::install_metrics(&ctx, &|k: &str| (k == "otel.metrics.exporter").then(|| "none".to_string())).unwrap().is_none());
    assert!(cameleer_opentelemetry::install_metrics(&ctx, &|k: &str| (k == "otel.metrics.exporter").then(|| "prometheus".to_string())).is_err());
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let props = move |k: &str| match k {
        "camel.opentelemetry.metrics.enabled" => Some("true".to_string()),
        "otel.exporter.otlp.metrics.endpoint" => Some(format!("http://127.0.0.1:{port}/custom")),
        "otel.metric.export.interval" => Some("20".to_string()),
        "otel.exporter.otlp.timeout" => Some("200".to_string()),
        _ => None,
    };
    let exp = cameleer_opentelemetry::install_metrics(&ctx, &props).unwrap().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    exp.force_flush(Duration::from_secs(5));
    let st = exp.stats();
    assert!(st.failures.load(std::sync::atomic::Ordering::Relaxed) >= 1);
    exp.shutdown(Duration::from_secs(5));
}

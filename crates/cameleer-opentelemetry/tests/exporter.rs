//! The OTLP/HTTP exporter against the embedded receiver: what goes on the wire, retries,
//! failures and the batch processor's limits.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use cameleer_core::prelude::*;
use cameleer_core::telemetry::span::{AttrValue, SpanData, SpanKind, StatusCode};
use cameleer_core::telemetry::TelemetryConfig;
use cameleer_opentelemetry::testing::OtlpReceiver;
use cameleer_opentelemetry::{ExportResult, OtlpConfig, OtlpHttpExporter, SpanExporter};

fn span(name: &str) -> SpanData {
    SpanData {
        trace_id: [7; 16],
        span_id: [8; 8],
        parent_span_id: [0; 8],
        parent_remote: false,
        trace_state: String::new(),
        flags: 1,
        name: name.into(),
        kind: SpanKind::Client,
        start_unix_nano: 10,
        end_unix_nano: 20,
        attributes: vec![("k".into(), AttrValue::from("v"))],
        shared_attributes: Default::default(),
        dropped_attributes: 0,
        events: Vec::new(),
        status: StatusCode::Unset,
        status_message: String::new(),
        scope_name: "camel".into(),
    }
}

fn config(rx: &OtlpReceiver) -> OtlpConfig {
    let get = |k: &str| match k {
        "otel.exporter.otlp.endpoint" => Some(rx.endpoint()),
        "otel.service.name" => Some("orders".to_string()),
        "otel.resource.attributes" => Some("deployment.environment=test".to_string()),
        _ => None,
    };
    let mut c = OtlpConfig::from_lookup(&get).unwrap();
    c.initial_backoff = Duration::from_millis(20);
    c.max_backoff = Duration::from_millis(50);
    c.timeout = Duration::from_secs(5);
    c
}

#[test]
fn routes_export_their_spans_with_resource_scope_gzip_and_headers() {
    let rx = OtlpReceiver::start();
    let mut cfg = config(&rx);
    cfg.gzip = true;
    cfg.headers = vec![("Authorization".into(), "Bearer t0ken".into())];
    cfg.schedule_delay = Duration::from_millis(50);
    let ctx = CamelContext::new();
    let otel = cameleer_opentelemetry::install_with(&ctx, TelemetryConfig::default(), cfg).unwrap();
    ctx.add_routes(|b| {
        b.from("direct:order").route_id("order").to("mock:stored");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:order", "o-1").unwrap();
    assert!(otel.force_flush(Duration::from_secs(10)));
    let got = rx.wait_for_spans(3, Duration::from_secs(10));
    assert_eq!(got.len(), 3, "{got:#?}");
    let r = &got[0];
    assert_eq!(r.resource[0], ("service.name".to_string(), AttrValue::from("orders")));
    assert!(r.resource.contains(&("deployment.environment".to_string(), AttrValue::from("test"))));
    assert!(r.resource.contains(&("telemetry.sdk.language".to_string(), AttrValue::from("rust"))));
    assert_eq!(r.scope_name, "camel");
    assert_eq!(r.scope_version, env!("CARGO_PKG_VERSION"));
    let names: Vec<&str> = got.iter().map(|s| s.span.name.as_str()).collect();
    assert!(names.contains(&"mock") && names.contains(&"order"), "{names:?}");
    let req = &rx.requests()[0];
    assert_eq!(req.path, "/v1/traces");
    assert_eq!(req.header("content-type"), Some("application/x-protobuf"));
    assert_eq!(req.header("content-encoding"), Some("gzip"));
    assert_eq!(req.header("authorization"), Some("Bearer t0ken"));
    assert_eq!(&req.body[..2], &[0x1f, 0x8b]);
    let (exported, dropped, failed, _) = otel.batch.as_ref().unwrap().stats().snapshot();
    assert_eq!((exported, dropped, failed), (3, 0, 0));
    ctx.stop().unwrap();
    otel.shutdown(Duration::from_secs(5));
}

#[test]
fn retryable_statuses_are_retried_honouring_retry_after() {
    let rx = OtlpReceiver::start();
    let ex = OtlpHttpExporter::new(&config(&rx)).unwrap();
    rx.script([(503, vec![("Retry-After".to_string(), "1".to_string())], Vec::new()), (429, Vec::new(), Vec::new())]);
    let t = Instant::now();
    assert_eq!(ex.export(&[span("a")], Duration::from_secs(10)), ExportResult::Success);
    // the first retry waited for Retry-After, not the 20 ms backoff
    assert!(t.elapsed() >= Duration::from_millis(950), "{:?}", t.elapsed());
    let statuses: Vec<u16> = rx.requests().iter().map(|r| r.status).collect();
    assert_eq!(statuses, vec![503, 429, 200]);
    assert_eq!(rx.spans().len(), 1);
}

#[test]
fn other_statuses_are_final_and_attempts_are_bounded() {
    let rx = OtlpReceiver::start();
    let ex = OtlpHttpExporter::new(&config(&rx)).unwrap();
    rx.script([(400, Vec::new(), b"bad".to_vec())]);
    match ex.export(&[span("a")], Duration::from_secs(10)) {
        ExportResult::Failure(m, false) => assert!(m.contains("400"), "{m}"),
        o => panic!("{o:?}"),
    }
    assert_eq!(rx.requests().len(), 1);
    // 502 every time: five attempts, then the batch is dropped
    rx.script((0..10).map(|_| (502, Vec::new(), Vec::new())));
    assert!(matches!(ex.export(&[span("b")], Duration::from_secs(10)), ExportResult::Failure(_, true)));
    assert_eq!(rx.requests().len(), 6);
    // the budget bounds the retries too
    let t = Instant::now();
    rx.script([(504, vec![("Retry-After".to_string(), "30".to_string())], Vec::new())]);
    assert!(matches!(ex.export(&[span("c")], Duration::from_secs(2)), ExportResult::Failure(_, true)));
    assert!(t.elapsed() < Duration::from_secs(2));
}

#[test]
fn a_partial_success_is_a_success() {
    let rx = OtlpReceiver::start();
    let ex = OtlpHttpExporter::new(&config(&rx)).unwrap();
    rx.script([(200, Vec::new(), cameleer_opentelemetry::proto::encode_partial_success(1, "span too large"))]);
    assert_eq!(ex.export(&[span("a")], Duration::from_secs(5)), ExportResult::Success);
    assert_eq!(rx.requests().len(), 1);
}

#[test]
fn a_slow_or_absent_collector_never_slows_the_route() {
    // a collector that answers after 3 s with a 1 s timeout
    let rx = OtlpReceiver::start();
    rx.set_delay(Duration::from_secs(3));
    let mut cfg = config(&rx);
    cfg.timeout = Duration::from_secs(1);
    let ex = OtlpHttpExporter::new(&cfg).unwrap();
    let t = Instant::now();
    assert!(matches!(ex.export(&[span("a")], Duration::from_secs(1)), ExportResult::Failure(_, true)));
    assert!(t.elapsed() < Duration::from_millis(2500), "{:?}", t.elapsed());

    // nothing listens: routes run at full speed, the queue fills up, spans are dropped and counted
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let get = |k: &str| match k {
        "otel.exporter.otlp.endpoint" => Some(format!("http://127.0.0.1:{port}")),
        "otel.bsp.max.queue.size" => Some("16".to_string()),
        "otel.bsp.max.export.batch.size" => Some("8".to_string()),
        "otel.bsp.schedule.delay" => Some("20".to_string()),
        "otel.exporter.otlp.timeout" => Some("200".to_string()),
        _ => None,
    };
    let ctx = CamelContext::new();
    let otel = cameleer_opentelemetry::install_with(&ctx, TelemetryConfig::default(), OtlpConfig::from_lookup(&get).unwrap()).unwrap();
    ctx.add_routes(|b| {
        b.from("direct:hot").to("mock:hot");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = Instant::now();
    let tpl = ctx.create_producer_template();
    for i in 0..2000 {
        tpl.send_body("direct:hot", i).unwrap();
    }
    assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
    assert_eq!(ctx.mock_endpoint("mock:hot").unwrap().received_counter(), 2000);
    let stats = otel.batch.as_ref().unwrap().stats();
    // How the 6,000 spans (3 per message) split between dropped (queue full) and failed (export
    // refused) depends on how fast a refused connect returns: slow on Windows, immediate on
    // Linux. Either way the queue overflows, nothing is exported and every span is counted.
    assert!(stats.dropped.load(Ordering::Relaxed) > 0, "{:?}", stats);
    otel.force_flush(Duration::from_secs(5));
    assert!(stats.failed.load(Ordering::Relaxed) > 0, "{:?}", stats);
    ctx.stop().unwrap();
    otel.shutdown(Duration::from_secs(2));
    assert_eq!(stats.exported.load(Ordering::Relaxed), 0);
    assert_eq!(stats.dropped.load(Ordering::Relaxed) + stats.failed.load(Ordering::Relaxed), 6000, "{:?}", stats);
}

#[test]
fn properties_and_environment_configure_install() {
    let rx = OtlpReceiver::start();
    let endpoint = rx.endpoint();
    let props = move |k: &str| match k {
        "camel.opentelemetry.enabled" => Some("true".to_string()),
        "camel.opentelemetry.instrumentationName" => Some("orders-app".to_string()),
        "camel.opentelemetry.exclude-patterns" => Some("mock:quiet*".to_string()),
        "otel.exporter.otlp.endpoint" => Some(endpoint.clone()),
        "otel.bsp.schedule.delay" => Some("20".to_string()),
        _ => None,
    };
    assert!(cameleer_opentelemetry::is_enabled(&|k: &str| (k == "camel.opentelemetry.traceProcessors").then(|| "true".to_string())).unwrap());
    assert!(!cameleer_opentelemetry::is_enabled(&|_: &str| None).unwrap());
    // camel-opentelemetry2's prefix; the legacy prefix wins when both are set
    assert!(cameleer_opentelemetry::is_enabled(&|k: &str| (k == "camel.opentelemetry2.enabled").then(|| "true".to_string())).unwrap());
    let both = |k: &str| match k {
        "camel.opentelemetry.excludePatterns" => Some("a*".to_string()),
        "camel.opentelemetry2.excludePatterns" => Some("b*".to_string()),
        "camel.opentelemetry2.trace-custom-id-only" => Some("true".to_string()),
        "camel.opentelemetry2.includePatterns" => Some("log*, direct*".to_string()),
        _ => None,
    };
    let c = cameleer_opentelemetry::telemetry_config(&both).unwrap();
    assert_eq!(c.exclude_patterns, vec!["a*".to_string()]);
    assert_eq!(c.include_patterns, vec!["log*".to_string(), "direct*".to_string()]);
    assert!(c.trace_custom_id_only);
    assert!(!cameleer_opentelemetry::is_enabled(&|k: &str| (k == "camel.opentelemetry.enabled").then(|| "false".to_string())).unwrap());
    let ctx = CamelContext::new();
    let otel = cameleer_opentelemetry::install(&ctx, &props).unwrap().unwrap();
    ctx.add_routes(|b| {
        b.from("direct:x").route_id("x").to("mock:quiet").to("mock:loud");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:x", "1").unwrap();
    otel.force_flush(Duration::from_secs(5));
    let got = rx.wait_for_spans(3, Duration::from_secs(5));
    assert_eq!(got.len(), 3, "{:?}", got.iter().map(|s| &s.span.name).collect::<Vec<_>>());
    assert!(got.iter().all(|s| s.scope_name == "orders-app"));
    assert!(got.iter().all(|s| s.span.attribute("camel.uri") != Some(&AttrValue::from("mock://quiet"))));
    ctx.stop().unwrap();
    otel.shutdown(Duration::from_secs(2));
}

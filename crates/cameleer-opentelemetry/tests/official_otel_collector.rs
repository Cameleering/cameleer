//! The official OpenTelemetry Collector (`otelcol`, core distribution) between Cameleer's
//! exporter and Cameleer's embedded receiver: Cameleer exports OTLP/HTTP (protobuf, gzip) to the
//! Collector's `otlp` receiver, the Collector decodes it and forwards it with its own `otlphttp`
//! exporter, and the spans that arrive must be the ones that were sent.
//!
//! Runs only with `CAMEL_OFFICIAL_CLIENTS=1`; `scripts/otel-collector.sh install` puts the
//! pinned, checksum-verified binary in place and `scripts/otel-collector.sh test` runs this
//! (`CAMEL_OTELCOL` names the binary).

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use cameleer_core::prelude::*;
use cameleer_core::telemetry::{AttrValue, SpanKind, StatusCode, TelemetryConfig};
use cameleer_opentelemetry::testing::OtlpReceiver;
use cameleer_opentelemetry::OtlpConfig;

struct Collector(Child);
impl Drop for Collector {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

#[test]
fn official_collector_accepts_and_forwards_cameleer_spans() {
    if std::env::var("CAMEL_OFFICIAL_CLIENTS").as_deref() != Ok("1") {
        return;
    }
    let bin = std::env::var("CAMEL_OTELCOL").unwrap_or_else(|_| format!("{}/.cache/cameleer/otelcol-0.165.0/otelcol", std::env::var("HOME").unwrap_or_default()));
    let downstream = OtlpReceiver::start();
    let port = free_port();
    let dir = std::env::temp_dir().join(format!("cameleer-otelcol-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        format!(
            r#"receivers:
  otlp:
    protocols:
      http:
        endpoint: 127.0.0.1:{port}
exporters:
  otlphttp:
    endpoint: {downstream}
    compression: none
    retry_on_failure:
      enabled: false
    sending_queue:
      enabled: false
service:
  telemetry:
    metrics:
      level: none
  pipelines:
    traces:
      receivers: [otlp]
      exporters: [otlphttp]
"#,
            downstream = downstream.endpoint()
        ),
    )
    .unwrap();
    let _collector = Collector(Command::new(&bin).arg("--config").arg(&config).stdout(Stdio::null()).stderr(Stdio::inherit()).spawn().unwrap_or_else(|e| panic!("cannot run {bin}: {e}")));
    let start = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(start.elapsed() < Duration::from_secs(20), "the Collector did not open port {port}");
        std::thread::sleep(Duration::from_millis(50));
    }

    let endpoint = format!("http://127.0.0.1:{port}");
    let get = move |k: &str| match k {
        "otel.exporter.otlp.endpoint" => Some(endpoint.clone()),
        "otel.exporter.otlp.compression" => Some("gzip".to_string()),
        "otel.service.name" => Some("cameleer-official".to_string()),
        "otel.resource.attributes" => Some("deployment.environment=ci".to_string()),
        "otel.bsp.schedule.delay" => Some("50".to_string()),
        _ => None,
    };
    let ctx = CamelContext::new();
    let otel = cameleer_opentelemetry::install_with(&ctx, TelemetryConfig::default(), OtlpConfig::from_lookup(&get).unwrap()).unwrap();
    ctx.add_routes(|b| {
        b.from("direct:order").route_id("order").to("mock:stored").to("direct:fail");
        b.from("direct:fail").route_id("fail").process(|_ex: &mut Exchange| Err(CamelError::illegal_argument("rejected order")));
    })
    .unwrap();
    ctx.start().unwrap();
    let mut ex = Exchange::with_body(Some(ctx.clone()), "o-1");
    ex.set_header("x", "y");
    let _ = ctx.create_producer_template().send("direct:order", ex);
    assert!(otel.force_flush(Duration::from_secs(20)));
    let (exported, dropped, failed, _) = otel.batch.as_ref().unwrap().stats().snapshot();
    assert_eq!((dropped, failed), (0, 0), "the Collector refused the export");
    let got = downstream.wait_for_spans(exported as usize, Duration::from_secs(20));
    assert_eq!(got.len() as u64, exported);
    assert_eq!(got.len(), 5, "{:#?}", got.iter().map(|s| &s.span.name).collect::<Vec<_>>());
    let r = &got[0];
    assert!(r.resource.contains(&("service.name".to_string(), AttrValue::from("cameleer-official"))));
    assert!(r.resource.contains(&("deployment.environment".to_string(), AttrValue::from("ci"))));
    assert_eq!(r.scope_name, "camel");
    let route = got.iter().find(|s| s.span.attribute("camel.route.id") == Some(&AttrValue::from("fail")) && s.span.attribute("op") == Some(&AttrValue::from("EVENT_RECEIVED"))).unwrap();
    assert_eq!(route.span.status, StatusCode::Error);
    assert_eq!(route.span.status_message, "rejected order");
    assert_eq!(route.span.events[0].name, "error");
    assert_eq!(route.span.kind, SpanKind::Internal);
    let root = got.iter().find(|s| s.span.is_root()).unwrap();
    assert!(got.iter().all(|s| s.span.trace_id == root.span.trace_id));
    for s in &got {
        assert!(s.span.end_unix_nano >= s.span.start_unix_nano);
        if !s.span.is_root() {
            assert!(got.iter().any(|p| p.span.span_id == s.span.parent_span_id), "orphan {}", s.span.name);
        }
    }
    ctx.stop().unwrap();
    otel.shutdown(Duration::from_secs(5));
}

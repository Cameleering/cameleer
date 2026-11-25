//! Tracing with OpenTelemetry semantics (`cameleer_core::telemetry`): which spans exist, how
//! they nest, what they carry, and how the context travels in the message headers.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use cameleer_core::prelude::*;
use cameleer_core::telemetry::span::InMemorySpanProcessor;
use cameleer_core::telemetry::{self, AttrValue, Sampler, SpanContext, SpanData, SpanKind, StatusCode, TelemetryConfig};

fn traced(cfg: TelemetryConfig) -> (CamelContext, Arc<InMemorySpanProcessor>) {
    let ctx = CamelContext::new();
    let spans = Arc::new(InMemorySpanProcessor::new());
    telemetry::install(&ctx, cfg, spans.clone());
    (ctx, spans)
}

fn by_name<'a>(spans: &'a [SpanData], name: &str, kind: SpanKind) -> &'a SpanData {
    spans.iter().find(|s| s.name == name && s.kind == kind).unwrap_or_else(|| panic!("no {kind:?} span {name} in {:?}", spans.iter().map(|s| (&s.name, s.kind)).collect::<Vec<_>>()))
}

fn attr<'a>(s: &'a SpanData, k: &str) -> &'a AttrValue {
    s.attribute(k).unwrap_or_else(|| panic!("no attribute {k} on {}: {:?}", s.name, s.attributes))
}

#[test]
fn spans_nest_across_direct_routes_and_the_headers_carry_the_context() {
    let (ctx, spans) = traced(TelemetryConfig::default());
    ctx.add_routes(|b| {
        b.from("direct:a").route_id("a").to("mock:x").to("direct:b");
        b.from("direct:b").route_id("b").to("mock:y");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:a", "hello").unwrap();
    let s = spans.wait_for(6, Duration::from_secs(5));
    assert_eq!(s.len(), 6, "{s:#?}");
    let send_a = s.iter().find(|x| x.name == "a" && x.attribute("op") == Some(&AttrValue::from("EVENT_SENT"))).unwrap();
    assert_eq!(send_a.kind, SpanKind::Internal);
    assert!(send_a.is_root());
    let routes: Vec<&SpanData> = s.iter().filter(|x| x.attribute("op") == Some(&AttrValue::from("EVENT_RECEIVED"))).collect();
    assert_eq!(routes.len(), 2);
    let route_a = routes.iter().find(|r| attr(r, "camel.route.id") == &AttrValue::from("a")).unwrap();
    let route_b = routes.iter().find(|r| attr(r, "camel.route.id") == &AttrValue::from("b")).unwrap();
    let mock_x = by_name(&s, "mock", SpanKind::Internal);
    let send_b = s.iter().find(|x| x.name == "b" && x.attribute("op") == Some(&AttrValue::from("EVENT_SENT"))).unwrap();
    let mock_y = s.iter().filter(|x| x.name == "mock").find(|x| x.parent_span_id == route_b.span_id).unwrap();
    // one trace; each span the child of the one that called it
    assert!(s.iter().all(|x| x.trace_id == send_a.trace_id));
    assert_eq!(route_a.parent_span_id, send_a.span_id);
    assert_eq!(mock_x.parent_span_id, route_a.span_id);
    assert_eq!(send_b.parent_span_id, route_a.span_id);
    assert_eq!(route_b.parent_span_id, send_b.span_id);
    assert_eq!(attr(mock_x, "component"), &AttrValue::from("camel-mock"));
    assert_eq!(attr(mock_x, "camel.uri"), &AttrValue::from("mock://x"));
    assert_eq!(attr(mock_x, "camel.route.id"), &AttrValue::from("a"));
    assert_eq!(&*mock_x.scope_name, "camel");
    assert!(mock_x.end_unix_nano >= mock_x.start_unix_nano && mock_x.start_unix_nano >= route_a.start_unix_nano);
    assert_eq!(mock_y.status, StatusCode::Unset);
    // the producer wrote its own context into the headers it sent
    let got = ctx.mock_endpoint("mock:y").unwrap().received_exchanges();
    let tp = got[0].header("traceparent").unwrap().to_string();
    let c = SpanContext::parse_traceparent(&tp).unwrap();
    assert_eq!((c.trace_id, c.span_id), (mock_y.trace_id, mock_y.span_id));
    assert!(c.is_sampled());
    // and the exchange's active span was restored when the spans ended
    ctx.stop().unwrap();
}

#[test]
fn headers_start_the_trace_when_the_exchange_has_no_span() {
    // an exchange made by a consumer has no span: the route span extracts the headers. Here the
    // sender's producer is created before tracing is installed, so it is not traced, as a
    // consumer is not
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("seda:work").route_id("work").to("mock:done");
    })
    .unwrap();
    let untraced = ctx.get_producer("seda:work").unwrap();
    let spans = Arc::new(InMemorySpanProcessor::new());
    telemetry::install(&ctx, TelemetryConfig::default(), spans.clone());
    ctx.start().unwrap();
    let mut ex = Exchange::with_body(Some(ctx.clone()), "x");
    ex.set_header("traceparent", Value::Bytes(b"00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_vec()));
    ex.set_header("tracestate", "rojo=00f067aa0ba902b7");
    ex.set_header("baggage", "tenant=acme%20corp");
    untraced.process(&mut ex).unwrap();
    let s = spans.wait_for(2, Duration::from_secs(5));
    let route = s.iter().find(|x| x.attribute("op") == Some(&AttrValue::from("EVENT_RECEIVED"))).unwrap();
    assert_eq!(cameleer_core::telemetry::context::hex(&route.trace_id), "4bf92f3577b34da6a3ce929d0e0e4736");
    assert_eq!(cameleer_core::telemetry::context::hex(&route.parent_span_id), "00f067aa0ba902b7");
    assert!(route.parent_remote);
    assert_eq!(route.trace_state, "rojo=00f067aa0ba902b7");
    let mock = s.iter().find(|x| x.name == "mock").unwrap();
    assert_eq!(mock.parent_span_id, route.span_id);
    let got = ctx.mock_endpoint("mock:done").unwrap().received_exchanges();
    assert_eq!(got[0].header("tracestate").unwrap().to_string(), "rojo=00f067aa0ba902b7");
    assert_eq!(got[0].header("baggage").unwrap().to_string(), "tenant=acme%20corp");
    ctx.stop().unwrap();
}

#[test]
fn failures_set_the_error_status_and_event() {
    let (ctx, spans) = traced(TelemetryConfig::default());
    ctx.add_routes(|b| {
        b.from("direct:fail").route_id("fail").process(|_ex: &mut Exchange| Err(CamelError::illegal_argument("bad order")));
    })
    .unwrap();
    ctx.start().unwrap();
    let r = ctx.create_producer_template().send_body("direct:fail", "x");
    assert!(r.is_err());
    let s = spans.wait_for(2, Duration::from_secs(5));
    let route = s.iter().find(|x| x.attribute("op") == Some(&AttrValue::from("EVENT_RECEIVED"))).unwrap();
    assert_eq!(route.status, StatusCode::Error);
    assert_eq!(route.status_message, "bad order");
    assert_eq!(attr(route, "error"), &AttrValue::Bool(true));
    let ev = &route.events[0];
    assert_eq!(ev.name, "error");
    assert!(ev.attributes.contains(&("error.kind".into(), AttrValue::from("Exception"))));
    assert!(ev.attributes.contains(&("message".into(), AttrValue::from("bad order"))));
    ctx.stop().unwrap();
}

#[test]
fn processor_spans_and_sampling() {
    let (ctx, spans) = traced(TelemetryConfig { trace_processors: true, ..Default::default() });
    ctx.add_routes(|b| {
        b.from("direct:p").route_id("p").set_header("a", constant("1")).id("setA").to("mock:p");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:p", "x").unwrap();
    let s = spans.wait_for(5, Duration::from_secs(5));
    let set = s.iter().find(|x| x.name == "setA-setHeader").unwrap_or_else(|| panic!("{:?}", s.iter().map(|x| &x.name).collect::<Vec<_>>()));
    let route = s.iter().find(|x| x.attribute("op") == Some(&AttrValue::from("EVENT_RECEIVED"))).unwrap();
    assert_eq!(set.parent_span_id, route.span_id);
    assert_eq!(attr(set, "op"), &AttrValue::from("EVENT_PROCESS"));
    // the producer span of `to` is the child of the `to` processor span
    let to = s.iter().find(|x| x.name.ends_with("-to")).unwrap();
    let mock = s.iter().find(|x| x.name == "mock").unwrap();
    assert_eq!(mock.parent_span_id, to.span_id);
    ctx.stop().unwrap();

    // never sampled: no spans, but the context still travels, unsampled
    let (ctx, spans) = traced(TelemetryConfig { sampler: Sampler::AlwaysOff, ..Default::default() });
    ctx.add_routes(|b| {
        b.from("direct:q").to("mock:q");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:q", "x").unwrap();
    std::thread::sleep(Duration::from_millis(50));
    assert!(spans.spans().is_empty());
    let got = ctx.mock_endpoint("mock:q").unwrap().received_exchanges();
    let c = SpanContext::parse_traceparent(&got[0].header("traceparent").unwrap().to_string()).unwrap();
    assert!(!c.is_sampled());
    ctx.stop().unwrap();
}

#[test]
fn seda_and_wire_tap_keep_the_parent_on_other_threads() {
    let (ctx, spans) = traced(TelemetryConfig::default());
    ctx.add_routes(|b| {
        b.from("direct:start").route_id("start").wire_tap("seda:tap").to("seda:next");
        b.from("seda:next").route_id("next").to("mock:next");
        b.from("seda:tap").route_id("tap").to("mock:tap");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "x").unwrap();
    let s = spans.wait_for(8, Duration::from_secs(5));
    assert_eq!(s.len(), 8, "{s:#?}");
    let root = s.iter().find(|x| x.is_root()).unwrap();
    assert!(s.iter().all(|x| x.trace_id == root.trace_id), "{s:#?}");
    let route = |id: &str| s.iter().find(|x| x.attribute("op") == Some(&AttrValue::from("EVENT_RECEIVED")) && x.attribute("camel.route.id") == Some(&AttrValue::from(id))).unwrap();
    let send = |n: &str| s.iter().find(|x| x.name == n && x.attribute("op") == Some(&AttrValue::from("EVENT_SENT"))).unwrap();
    assert_eq!(route("next").parent_span_id, send("next").span_id);
    assert_eq!(route("tap").parent_span_id, send("tap").span_id);
    assert_eq!(send("tap").parent_span_id, route("start").span_id);
    ctx.stop().unwrap();
}

#[test]
fn log_lines_inside_a_route_carry_the_trace_and_span_id() {
    let (ctx, spans) = traced(TelemetryConfig::default());
    let seen: Arc<Mutex<Vec<Option<([u8; 16], [u8; 8])>>>> = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    cameleer_core::log::add_sink(move |r| {
        if r.logger == "telemetry-log-test" {
            s2.lock().unwrap().push(r.trace);
        }
    });
    ctx.add_routes(|b| {
        b.from("direct:log").route_id("log").process(|_ex: &mut Exchange| {
            cameleer_core::camel_warn!("telemetry-log-test", "inside");
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:log", "x").unwrap();
    cameleer_core::camel_warn!("telemetry-log-test", "outside");
    let s = spans.wait_for(2, Duration::from_secs(5));
    let route = s.iter().find(|x| x.attribute("op") == Some(&AttrValue::from("EVENT_RECEIVED"))).unwrap();
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen, vec![Some((route.trace_id, route.span_id)), None]);
    ctx.stop().unwrap();
}

#[test]
fn encoding_writes_encoded_header_keys() {
    assert_eq!(telemetry::encode_key("x-b3-traceid"), "x_$dash$_b3_$dash$_traceid");
    assert_eq!(telemetry::decode_key("x_$dash$_b3_$dash$_traceid"), "x-b3-traceid");
    let (ctx, _spans) = traced(TelemetryConfig { encoding: true, ..Default::default() });
    ctx.add_routes(|b| {
        b.from("direct:e").to("mock:e");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:e", "x").unwrap();
    // the W3C names have no `-` or `.`, so they are unchanged
    assert!(ctx.mock_endpoint("mock:e").unwrap().received_exchanges()[0].header("traceparent").is_some());
    ctx.stop().unwrap();
}

#[test]
fn opentelemetry2_options_include_custom_ids_core_processors_and_headers() {
    // includePatterns: only matching endpoints, routes and processors are traced
    let (ctx, spans) = traced(TelemetryConfig { include_patterns: vec!["direct:in*".into(), "mock:kept".into()], ..Default::default() });
    ctx.add_routes(|b| {
        b.from("direct:in").route_id("in").to("mock:kept").to("mock:dropped");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:in", "x").unwrap();
    let s = spans.wait_for(3, Duration::from_secs(5));
    assert!(s.iter().all(|x| x.attribute("camel.uri") != Some(&AttrValue::from("mock://dropped"))), "{s:#?}");
    assert!(s.iter().any(|x| x.attribute("camel.uri") == Some(&AttrValue::from("mock://kept"))));
    ctx.stop().unwrap();

    // traceCustomIdOnly: a route with a generated id has no spans, nor do its sends; in a
    // traced route only processors with an id given by the author
    let (ctx, spans) = traced(TelemetryConfig { trace_custom_id_only: true, trace_processors: true, ..Default::default() });
    ctx.add_routes(|b| {
        b.from("direct:anon").to("mock:anon");
        b.from("direct:named").route_id("named").set_header("a", constant("1")).id("setA").set_header("b", constant("2")).to("mock:named");
    })
    .unwrap();
    ctx.start().unwrap();
    let tpl = ctx.create_producer_template();
    tpl.send_body("direct:anon", "x").unwrap();
    tpl.send_body("direct:named", "y").unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let s = spans.spans();
    assert!(s.iter().all(|x| x.attribute("camel.uri") != Some(&AttrValue::from("mock://anon"))), "{s:#?}");
    assert!(s.iter().any(|x| x.attribute("camel.route.id") == Some(&AttrValue::from("named")) && x.attribute("op") == Some(&AttrValue::from("EVENT_RECEIVED"))));
    let procs: Vec<&str> = s.iter().filter(|x| x.attribute("op") == Some(&AttrValue::from("EVENT_PROCESS"))).map(|x| x.name.as_str()).collect();
    assert_eq!(procs, vec!["setA-setHeader"]);
    ctx.stop().unwrap();

    // disableCoreProcessors: only `process` steps get processor spans
    let (ctx, spans) = traced(TelemetryConfig { trace_processors: true, disable_core_processors: true, trace_headers_inclusion: true, ..Default::default() });
    ctx.add_routes(|b| {
        b.from("direct:c").route_id("c").set_header("a", constant("1")).process(|_ex: &mut Exchange| Ok(())).id("custom").to("mock:c");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:c", "x").unwrap();
    let s = spans.wait_for(4, Duration::from_secs(5));
    let procs: Vec<&str> = s.iter().filter(|x| x.attribute("op") == Some(&AttrValue::from("EVENT_PROCESS"))).map(|x| x.name.as_str()).collect();
    assert_eq!(procs, vec!["custom-process"]);
    // traceHeadersInclusion: CAMEL_TRACE_ID and CAMEL_SPAN_ID name the span that sent to mock:c
    let got = ctx.mock_endpoint("mock:c").unwrap().received_exchanges();
    let mock = s.iter().find(|x| x.name == "mock").unwrap();
    assert_eq!(got[0].header("CAMEL_TRACE_ID").unwrap().to_string(), cameleer_core::telemetry::context::hex(&mock.trace_id));
    assert_eq!(got[0].header("CAMEL_SPAN_ID").unwrap().to_string(), cameleer_core::telemetry::context::hex(&mock.span_id));
    ctx.stop().unwrap();
}

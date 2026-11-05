use std::sync::{Arc, Mutex};

use cameleer_core::history::{format_message_history, message_history, MESSAGE_HISTORY};
use cameleer_core::prelude::*;
use cameleer_core::tracer::TraceEvent;

fn routes(ctx: &CamelContext) {
    ctx.add_routes(|b| {
        b.from("direct:start").route_id("tr").set_header("h", constant("v")).to("direct:sub").id("callSub").to("mock:out");
        b.from("direct:sub").route_id("sub").transform(simple("${body}!"));
        b.from("direct:fail")
            .route_id("failing")
            .to("mock:before")
            .process(|_: &mut Exchange| Err(CamelError::illegal_argument("kaboom")))
            .id("boom");
    })
    .unwrap();
}

#[test]
fn tracer_logs_each_step() {
    let ctx = CamelContext::new();
    routes(&ctx);
    ctx.start().unwrap();
    let events: Arc<Mutex<Vec<TraceEvent>>> = Arc::default();
    let e2 = events.clone();
    ctx.tracer().add_handler(move |e: &TraceEvent| e2.lock().unwrap().push(e.clone()));
    ctx.tracer().set_log_enabled(false);
    let t = ctx.create_producer_template();
    // disabled: nothing traced
    t.send_body("direct:start", "a").unwrap();
    assert!(events.lock().unwrap().is_empty());

    ctx.set_tracing(true);
    assert!(ctx.is_tracing());
    t.send_body("direct:start", "hello").unwrap();
    let ev = events.lock().unwrap().clone();
    let lines: Vec<&str> = ev.iter().map(|e| e.message.as_str()).collect();
    assert_eq!(ev.len(), 6, "{:#?}", lines);
    assert!(lines[0].starts_with(">>> (tr) from[direct://start] <<< Exchange["), "{}", lines[0]);
    assert!(lines[0].contains("Body: hello"), "{}", lines[0]);
    assert!(ev[0].first);
    assert!(lines[1].starts_with(">>> (tr) setHeader[h"), "{}", lines[1]);
    assert!(lines[2].starts_with(">>> (tr) to[direct:sub]"), "{}", lines[2]);
    assert!(lines[2].contains("h=v"), "headers are shown: {}", lines[2]);
    assert_eq!(ev[2].node_id, "callSub");
    assert!(lines[3].starts_with(">>> (sub) from[direct://sub]"), "{}", lines[3]);
    assert!(lines[4].starts_with(">>> (sub) transform["), "{}", lines[4]);
    assert!(lines[5].starts_with(">>> (tr) to[mock:out] <<< "), "{}", lines[5]);
    assert!(lines[5].contains("Body: hello!"), "{}", lines[5]);
    assert!(ev.iter().all(|e| !e.exchange_id.is_empty()));

    // trace pattern restricts to some routes
    events.lock().unwrap().clear();
    ctx.tracer().set_trace_pattern(Some("sub"));
    t.send_body("direct:start", "x").unwrap();
    assert!(events.lock().unwrap().iter().all(|e| e.route_id == "sub"));
    assert_eq!(events.lock().unwrap().len(), 2);
    ctx.tracer().set_trace_pattern(None);

    ctx.set_tracing(false);
    events.lock().unwrap().clear();
    t.send_body("direct:start", "x").unwrap();
    assert!(events.lock().unwrap().is_empty());
    ctx.stop().unwrap();
}

#[test]
fn tracer_writes_to_log() {
    let ctx = CamelContext::new();
    routes(&ctx);
    ctx.start().unwrap();
    let marker = format!("marker-{}", ctx.name());
    let logged: Arc<Mutex<Vec<String>>> = Arc::default();
    let l2 = logged.clone();
    let m2 = marker.clone();
    cameleer_core::log::add_sink(move |r| {
        if r.logger == cameleer_core::tracer::TRACING_LOGGER && r.message.contains(&m2) {
            l2.lock().unwrap().push(r.message.clone());
        }
    });
    ctx.set_tracing(true);
    ctx.create_producer_template().send_body("direct:sub", marker.as_str()).unwrap();
    let logged = logged.lock().unwrap();
    assert_eq!(logged.len(), 2, "{:?}", logged);
    assert!(logged[0].starts_with(">>> (sub) from[direct://sub] <<< Exchange[Id: "), "{}", logged[0]);
    ctx.stop().unwrap();
}

#[test]
fn backlog_tracer_keeps_last_events() {
    let ctx = CamelContext::new();
    routes(&ctx);
    ctx.start().unwrap();
    ctx.set_backlog_tracing(true);
    ctx.tracer().set_backlog_size(4);
    let t = ctx.create_producer_template();
    t.send_body("direct:sub", "one").unwrap();
    assert_eq!(ctx.tracer().backlog_events().len(), 2);
    t.send_body("direct:sub", "two").unwrap();
    t.send_body("direct:sub", "three").unwrap();
    let ev = ctx.tracer().backlog_events();
    assert_eq!(ev.len(), 4);
    assert_eq!(ev[0].body, "two");
    assert_eq!(ev[3].body, "three");
    assert!(ev.windows(2).all(|w| w[0].uid < w[1].uid));
    assert_eq!(ctx.tracer().backlog_events_for("sub").len(), 4);
    let v = ctx.tracer().backlog_value();
    assert_eq!(v.as_array().unwrap()[0].get("routeId").unwrap().as_str(), Some("sub"));
    assert_eq!(ctx.tracer().dump_backlog().len(), 4);
    assert!(ctx.tracer().backlog_events().is_empty());
    ctx.stop().unwrap();
}

#[test]
fn message_history_property() {
    let ctx = CamelContext::new();
    routes(&ctx);
    ctx.set_message_history(true);
    assert!(ctx.is_message_history());
    ctx.add_routes(|b| {
        b.from("direct:hist").route_id("hist").to("direct:sub").to("mock:hist");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:hist").unwrap();
    mock.expected_message_count(1);
    ctx.create_producer_template().send_body("direct:hist", "x").unwrap();
    mock.assert_is_satisfied().unwrap();
    let ex = mock.received_exchanges()[0].clone();
    let hist = ex.property(MESSAGE_HISTORY).unwrap().as_array().unwrap().clone();
    let steps: Vec<(String, String)> = hist
        .iter()
        .map(|v| (v.get("routeId").unwrap().to_string(), v.get("label").unwrap().to_string()))
        .collect();
    assert_eq!(steps[0], ("hist".to_string(), "from[direct://hist]".to_string()));
    assert_eq!(steps[1], ("hist".to_string(), "to[direct:sub]".to_string()));
    assert_eq!(steps[2], ("sub".to_string(), "from[direct://sub]".to_string()));
    assert_eq!(steps[3].0, "sub");
    assert!(steps[3].1.starts_with("transform"));
    assert_eq!(steps[4], ("hist".to_string(), "to[mock:hist]".to_string()));
    assert_eq!(steps.len(), 5);
    assert!(hist.iter().all(|v| v.get("elapsed").unwrap().as_i64().unwrap() >= 0 && v.get("nodeId").is_some()));
    let entries = message_history(&ex);
    assert_eq!(entries[0].node_id, "hist");
    ctx.stop().unwrap();
}

#[test]
fn message_history_formatter_on_failure() {
    let ctx = CamelContext::new();
    routes(&ctx);
    ctx.set_message_history(true);
    ctx.start().unwrap();
    let mut ex = ctx.create_exchange();
    ex.set_body("payload");
    let ex = ctx.create_producer_template().send("direct:fail", ex);
    assert!(ex.is_failed());
    let text = format_message_history(&ex);
    assert!(text.starts_with("Message History\n---"), "{}", text);
    assert!(text.contains("RouteId              ProcessorId          Processor"), "{}", text);
    assert!(text.contains("[failing           ] [failing           ] [from[direct://fail]"), "{}", text);
    assert!(text.contains("[failing           ] [boom              ] [process[Processor]"), "{}", text);
    assert!(text.contains("Body: payload"), "{}", text);
    assert!(text.contains("Stacktrace"), "{}", text);
    assert!(text.contains("kaboom"), "{}", text);

    // without history
    let ex = Exchange::with_body(None, "z");
    assert!(format_message_history(&ex).starts_with("Message History (message history is disabled)"));
    ctx.stop().unwrap();
}

#[test]
fn exhausted_failure_log_includes_message_history() {
    use std::sync::{Arc, Mutex};
    let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let c = captured.clone();
    cameleer_core::log::add_sink(move |r| {
        if r.logger == "DefaultErrorHandler" {
            c.lock().unwrap().push(r.message.clone());
        }
    });
    let ctx = CamelContext::new();
    ctx.set_message_history(true);
    ctx.add_routes(|b| {
        b.from("direct:mh-fail").route_id("mhFail").set_header("x", constant(1)).process(|_: &mut Exchange| Err(CamelError::illegal_argument("kaboom-mh")));
    })
    .unwrap();
    ctx.start().unwrap();
    assert!(ctx.create_producer_template().send_body("direct:mh-fail", "x").is_err());
    let logs = captured.lock().unwrap().clone();
    let msg = logs.iter().find(|m| m.contains("kaboom-mh")).expect("exhausted log");
    assert!(msg.contains("Message History"), "{}", msg);
    assert!(msg.contains("mhFail"), "{}", msg);
    ctx.stop().unwrap();
}

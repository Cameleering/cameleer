//! Conformance tests for CamelContext lifecycle, route management and property placeholders.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use cameleer_core::context::CamelEvent;
use cameleer_core::prelude::*;
use cameleer_core::spi::EventNotifier;

struct Recorder(Arc<Mutex<Vec<String>>>);
impl EventNotifier for Recorder {
    fn notify(&self, event: &CamelEvent) {
        let s = match event {
            CamelEvent::RouteStarted(id) => format!("started:{}", id),
            CamelEvent::RouteStopped(id) => format!("stopped:{}", id),
            CamelEvent::RouteRemoved(id) => format!("removed:{}", id),
            CamelEvent::ContextStarted => "context-started".into(),
            CamelEvent::ContextStopped => "context-stopped".into(),
            _ => return,
        };
        self.0.lock().unwrap().push(s);
    }
}

#[test]
fn add_routes_after_start_are_started_immediately() {
    let ctx = CamelContext::new();
    ctx.start().unwrap();
    ctx.add_routes(|b| {
        b.from("direct:late").route_id("late").transform(constant("late reply"));
    })
    .unwrap();
    assert_eq!(ctx.route_status("late"), Some(ServiceStatus::Started));
    assert_eq!(ctx.create_producer_template().request_body("direct:late", "x").unwrap(), Value::from("late reply"));
}

#[test]
fn generated_and_explicit_route_ids() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:a").to("mock:a");
        b.from("direct:b").route_id("myRoute").to("mock:b");
        b.from("direct:c").to("mock:c");
    })
    .unwrap();
    ctx.start().unwrap();
    let ids = ctx.route_ids();
    assert_eq!(ids.len(), 3);
    assert!(ids.contains(&"myRoute".to_string()));
    assert!(ids.iter().filter(|id| id.starts_with("route")).count() == 2, "{:?}", ids);
}

#[test]
fn duplicate_route_id_fails() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:a").route_id("dup").to("mock:a");
        b.from("direct:b").route_id("dup").to("mock:b");
    })
    .unwrap();
    assert!(ctx.start().is_err());
}

#[test]
fn startup_order_and_reverse_shutdown_order() {
    let ctx = CamelContext::new();
    let events = Arc::new(Mutex::new(Vec::new()));
    ctx.add_event_notifier(Recorder(events.clone()));
    ctx.add_routes(|b| {
        b.from("direct:c").route_id("c").startup_order(30).to("mock:c");
        b.from("direct:a").route_id("a").startup_order(10).to("mock:a");
        b.from("direct:b").route_id("b").startup_order(20).to("mock:b");
        b.from("direct:z").route_id("z").to("mock:z");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.stop().unwrap();
    let ev = events.lock().unwrap().clone();
    assert_eq!(
        ev,
        vec![
            "started:a", "started:b", "started:c", "started:z", "context-started", "stopped:z", "stopped:c", "stopped:b", "stopped:a",
            "context-stopped"
        ]
    );
}

#[test]
fn auto_startup_false_and_manual_start() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:manual").route_id("manual").auto_startup(false).to("mock:m");
    })
    .unwrap();
    ctx.start().unwrap();
    assert_ne!(ctx.route_status("manual"), Some(ServiceStatus::Started));
    assert!(ctx.create_producer_template().send_body("direct:manual", "x").is_err());
    ctx.start_route("manual").unwrap();
    ctx.create_producer_template().send_body("direct:manual", "x").unwrap();
    assert_eq!(ctx.mock_endpoint("mock:m").unwrap().received_counter(), 1);
}

#[test]
fn remove_route_requires_stopped_route() {
    let ctx = CamelContext::new();
    let events = Arc::new(Mutex::new(Vec::new()));
    ctx.add_event_notifier(Recorder(events.clone()));
    ctx.add_routes(|b| {
        b.from("direct:r").route_id("r").to("mock:r");
    })
    .unwrap();
    ctx.start().unwrap();
    assert!(!ctx.remove_route("r").unwrap(), "a started route cannot be removed");
    ctx.stop_route("r").unwrap();
    assert!(ctx.remove_route("r").unwrap());
    assert!(ctx.route("r").is_none());
    assert!(!ctx.remove_route("r").unwrap());
    assert!(events.lock().unwrap().contains(&"removed:r".to_string()));
    // the route id can be reused
    ctx.add_routes(|b| {
        b.from("direct:r").route_id("r").transform(constant("new"));
    })
    .unwrap();
    assert_eq!(ctx.create_producer_template().request_body("direct:r", "x").unwrap(), Value::from("new"));
}

#[test]
fn graceful_shutdown_completes_inflight_and_queued_seda_messages() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("seda:slow").delay(constant(20)).to("mock:done");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for i in 0..5 {
        t.send_body("seda:slow", i).unwrap();
    }
    std::thread::sleep(Duration::from_millis(5));
    ctx.stop().unwrap();
    assert_eq!(ctx.mock_endpoint("mock:done").unwrap().received_counter(), 5);
}

#[test]
fn graceful_shutdown_waits_for_inflight_direct_exchange() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:slow").delay(constant(150)).to("mock:done");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let h = t.async_send_body("direct:slow", "x");
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(ctx.inflight_count(), 1);
    ctx.stop().unwrap();
    assert_eq!(ctx.mock_endpoint("mock:done").unwrap().received_counter(), 1);
    h.join().unwrap().unwrap();
}

#[test]
fn suspend_and_resume_route() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("seda:s").route_id("s").to("mock:s");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.suspend_route("s").unwrap();
    assert_eq!(ctx.route_status("s"), Some(ServiceStatus::Suspended));
    ctx.create_producer_template().send_body("seda:s", "queued").unwrap();
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(ctx.mock_endpoint("mock:s").unwrap().received_counter(), 0);
    ctx.resume_route("s").unwrap();
    assert!(ctx.mock_endpoint("mock:s").unwrap().wait_for_messages(1, Duration::from_secs(5)));
    ctx.stop().unwrap();
}

#[test]
fn property_placeholders_in_uris_and_simple() {
    let ctx = CamelContext::new();
    ctx.properties().set("in", "direct:start");
    ctx.properties().set("out", "mock:result");
    ctx.properties().set("greeting", "Hello");
    ctx.properties().set("count", "2");
    ctx.add_routes(|b| {
        b.from("{{in}}")
            .set_body(simple("${properties:greeting} ${body}"))
            .set_header("count", simple("{{count}}"))
            .to("{{out}}");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "World").unwrap();
    let ex = ctx.mock_endpoint("mock:result").unwrap().exchange(0).unwrap();
    assert_eq!(ex.body(), &Value::from("Hello World"));
    assert_eq!(ex.header("count").map(|v| v.to_string()), Some("2".into()));
}

#[test]
fn property_placeholder_default_value_and_missing_property() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").to("mock:{{target:fallback}}");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "x").unwrap();
    assert_eq!(ctx.mock_endpoint("mock:fallback").unwrap().received_counter(), 1);

    let ctx2 = CamelContext::new();
    ctx2.add_routes(|b| {
        b.from("direct:start").to("mock:{{missing}}");
    })
    .unwrap();
    let e = ctx2.start().unwrap_err();
    assert!(format!("{} {:?}", e, e.cause).contains("missing"), "{}", e);
}

#[test]
fn property_placeholders_in_endpoint_options() {
    let ctx = CamelContext::new();
    ctx.properties().set("period", "10");
    ctx.add_routes(|b| {
        b.from("timer:t?period={{period}}&delay=0&repeatCount=2").to("mock:t");
    })
    .unwrap();
    ctx.start().unwrap();
    assert!(ctx.mock_endpoint("mock:t").unwrap().wait_for_messages(2, Duration::from_secs(5)));
    ctx.stop().unwrap();
}

#[test]
fn context_restart_after_stop() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:a").transform(constant("ok"));
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.stop().unwrap();
    assert_eq!(ctx.status(), ServiceStatus::Stopped);
    ctx.start().unwrap();
    assert_eq!(ctx.create_producer_template().request_body("direct:a", "x").unwrap(), Value::from("ok"));
    ctx.stop().unwrap();
}

#[test]
fn routes_from_multiple_builders_and_on_exception_scope() {
    // onException defined in one RouteBuilder only applies to routes of that builder
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.on_exception(&["IOException"]).handled(true).to("mock:b1-error");
        b.from("direct:b1").throw_exception("IOException", "x");
    })
    .unwrap();
    ctx.add_routes(|b| {
        b.from("direct:b2").throw_exception("IOException", "y");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:b1", 1).unwrap();
    assert!(t.send_body("direct:b2", 2).is_err());
    assert_eq!(ctx.mock_endpoint("mock:b1-error").unwrap().received_counter(), 1);
}

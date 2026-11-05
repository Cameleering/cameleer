use std::sync::{Arc, Mutex};

use cameleer_core::json;
use cameleer_core::prelude::*;
use cameleer_http::HttpClient;
use cameleer_main::console::{handle_request, ConsoleConfig};
use cameleer_main::Main;

fn main_with_console() -> Main {
    let mut main = cameleer_main::new_main();
    main.set_property("camel.main.name", "console-test");
    main.set_property("camel.server.enabled", "true");
    main.set_property("camel.server.devConsoleEnabled", "true");
    main.set_property("camel.server.healthCheckEnabled", "true");
    main.set_property("camel.server.metricsEnabled", "true");
    main.set_property("camel.server.host", "127.0.0.1");
    main.set_property("camel.server.port", "0");
    main.set_property("camel.main.backlog-tracing", "true");
    main.configure(|ctx| {
        ctx.add_routes(|b| {
            b.from("direct:hello").route_id("hello").set_body(simple("Hello ${body}")).to("mock:out");
            b.from("direct:other").route_id("other").to("mock:other");
        })
    });
    main
}

#[test]
fn management_endpoints_over_http() {
    let lines: Arc<Mutex<Vec<String>>> = Arc::default();
    let l2 = lines.clone();
    cameleer_core::log::add_sink(move |r| {
        if r.logger == "Main" {
            l2.lock().unwrap().push(r.message.clone());
        }
    });
    let mut main = main_with_console();
    main.start().unwrap();
    let ctx = main.context().clone();
    assert!(ctx.is_backlog_tracing());
    let port = main.console_port().expect("console started");
    let base = format!("http://127.0.0.1:{}", port);
    let c = HttpClient::new();

    // startup summary
    {
        let l = lines.lock().unwrap();
        let i = l.iter().position(|m| m == "Routes startup (total:2 started:2)").expect("summary logged");
        // other tests in this binary log concurrently through the global sink, so only
        // require the lines to appear after the summary
        assert!(l[i + 1..].iter().any(|m| m == "    Started hello (direct://hello)"), "{:?}", &l[i..]);
        assert!(l[i + 1..].iter().any(|m| m == "    Started other (direct://other)"), "{:?}", &l[i..]);
    }

    let t = ctx.create_producer_template();
    t.send_body("direct:hello", "a").unwrap();
    t.send_body("direct:hello", "b").unwrap();

    // health
    let r = c.get(&format!("{}/q/health", base)).unwrap();
    assert_eq!(r.status, 200, "{}", r.body_text());
    let v = json::parse(&r.body_text()).unwrap();
    assert_eq!(v.get("status").unwrap().as_str(), Some("UP"));
    assert_eq!(c.get(&format!("{}/q/health/ready", base)).unwrap().status, 200);
    assert_eq!(c.get(&format!("{}/q/health/live", base)).unwrap().status, 200);

    // metrics
    let r = c.get(&format!("{}/q/metrics", base)).unwrap();
    assert_eq!(r.status, 200);
    assert!(r.content_type().unwrap().starts_with("text/plain"));
    let text = r.body_text();
    assert!(text.contains(&format!("camel_exchanges_total{{camelContext=\"{}\",routeId=\"hello\"}} 2", ctx.name())), "{}", text);

    // dev consoles
    let v = json::parse(&c.get(&format!("{}/q/dev", base)).unwrap().body_text()).unwrap();
    assert!(v.get("consoles").unwrap().as_array().unwrap().iter().any(|c| c.get("id").unwrap().as_str() == Some("routes")));
    let v = json::parse(&c.get(&format!("{}/q/dev/context", base)).unwrap().body_text()).unwrap();
    assert_eq!(v.get("state").unwrap().as_str(), Some("Started"));
    assert_eq!(v.get("exchangesTotal").unwrap().as_i64(), Some(2));
    let routes_json = c.get(&format!("{}/q/dev/routes", base)).unwrap().body_text();
    let v = json::parse(&routes_json).unwrap();
    let routes = v.get("routes").unwrap().as_array().unwrap();
    assert_eq!(routes.len(), 2);
    assert_eq!(routes[0].get("statistics").unwrap().get("exchangesTotal").unwrap().as_i64(), Some(2));
    let v = json::parse(&c.get(&format!("{}/q/dev/route/hello", base)).unwrap().body_text()).unwrap();
    assert_eq!(v.get("from").unwrap().as_str(), Some("direct://hello"));
    assert_eq!(v.get("processors").unwrap().as_array().unwrap().len(), 2);
    assert_eq!(c.get(&format!("{}/q/dev/route/nope", base)).unwrap().status, 404);
    let v = json::parse(&c.get(&format!("{}/q/dev/endpoints", base)).unwrap().body_text()).unwrap();
    assert!(v.get("endpoints").unwrap().as_array().unwrap().iter().any(|e| e.get("uri").unwrap().as_str() == Some("mock://out")));
    let v = json::parse(&c.get(&format!("{}/q/dev/health", base)).unwrap().body_text()).unwrap();
    assert_eq!(v.get("status").unwrap().as_str(), Some("UP"));
    let v = json::parse(&c.get(&format!("{}/q/dev/trace", base)).unwrap().body_text()).unwrap();
    let events = v.get("events").unwrap().as_array().unwrap();
    assert!(events.iter().any(|e| e.get("body").unwrap().as_str() == Some("Hello b")), "{}", v.to_json());
    assert_eq!(c.get(&format!("{}/q/dev/unknown", base)).unwrap().status, 404);
    assert_eq!(c.get(&format!("{}/q/dev/route-controller", base)).unwrap().status, 200);

    // route operations; readiness goes DOWN (503) while a route is stopped
    let r = c.post(&format!("{}/q/dev/route/other/stop", base), "text/plain", b"").unwrap();
    assert_eq!(r.status, 200, "{}", r.body_text());
    assert_eq!(json::parse(&r.body_text()).unwrap().get("state").unwrap().as_str(), Some("Stopped"));
    assert_eq!(ctx.route_status("other"), Some(ServiceStatus::Stopped));
    let r = c.get(&format!("{}/q/health/ready", base)).unwrap();
    assert_eq!(r.status, 503);
    assert_eq!(json::parse(&r.body_text()).unwrap().get("status").unwrap().as_str(), Some("DOWN"));
    assert_eq!(c.get(&format!("{}/q/health/live", base)).unwrap().status, 200);
    // `camel get health` fails while DOWN, so it can serve as a container health check
    let health = |base: &str| cameleer_main::cli::run(&["get".to_string(), "health".to_string(), "--url".to_string(), base.to_string()], &mut Vec::new());
    assert!(health(&base).is_err(), "get health must fail while DOWN");
    assert_eq!(c.post(&format!("{}/q/dev/route/other/start", base), "text/plain", b"").unwrap().status, 200);
    assert_eq!(c.post(&format!("{}/q/dev/route/other/suspend", base), "text/plain", b"").unwrap().status, 200);
    assert_eq!(ctx.route_status("other"), Some(ServiceStatus::Suspended));
    assert_eq!(c.post(&format!("{}/q/dev/route/other/resume", base), "text/plain", b"").unwrap().status, 200);
    assert_eq!(ctx.route_status("other"), Some(ServiceStatus::Started));
    assert_eq!(c.post(&format!("{}/q/dev/route/other/explode", base), "text/plain", b"").unwrap().status, 400);
    assert_eq!(c.post(&format!("{}/q/dev/route/nope/start", base), "text/plain", b"").unwrap().status, 404);
    assert_eq!(c.get(&format!("{}/q/health/ready", base)).unwrap().status, 200);
    assert!(health(&base).is_ok(), "get health succeeds when UP");

    // `camel get routes --url ...`
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_camel")).args(["get", "routes", "--url", &base]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(lines[0].starts_with("ID     FROM            STATUS"), "{}", stdout);
    assert!(lines[1].starts_with("hello  direct://hello  Started"), "{}", stdout);
    assert!(lines[2].starts_with("other  direct://other  Started"), "{}", stdout);

    main.stop().unwrap();
    assert!(c.get(&format!("{}/q/health", base)).is_err(), "server stopped");
}

#[test]
fn disabled_features_are_not_exposed() {
    let ctx = CamelContext::new();
    ctx.start().unwrap();
    let cfg = ConsoleConfig { enabled: true, health: true, ..Default::default() };
    let get = |p: &str| handle_request(&ctx, &cfg, &cameleer_http::HttpRequest::new("GET", p)).status;
    assert_eq!(get("/q/health"), 200);
    assert_eq!(get("/q/metrics"), 404);
    assert_eq!(get("/q/dev/routes"), 404);
    ctx.stop().unwrap();
    assert_eq!(get("/q/health"), 503);
}

#[test]
fn console_not_started_by_default() {
    let mut main = cameleer_main::new_main();
    main.configure(|ctx| ctx.add_routes(|b| { b.from("direct:x").to("mock:x"); }));
    main.set_property("camel.main.tracing", "true");
    main.set_property("camel.main.message-history", "true");
    main.start().unwrap();
    assert!(main.console_port().is_none());
    assert!(main.context().is_tracing());
    assert!(main.context().is_message_history());
    main.stop().unwrap();
}

#[test]
fn statistics_level_property() {
    use cameleer_core::management::ManagementStatisticsLevel;
    for (value, level) in [("Default", ManagementStatisticsLevel::Default), ("off", ManagementStatisticsLevel::Off)] {
        let mut main = cameleer_main::new_main();
        main.configure(|ctx| ctx.add_routes(|b| { b.from("direct:x").to("mock:x"); }));
        main.set_property("camel.main.jmxManagementStatisticsLevel", value);
        main.start().unwrap();
        assert_eq!(main.context().statistics_level(), level);
        main.stop().unwrap();
    }
    let mut main = cameleer_main::new_main();
    main.set_property("camel.main.jmx-management-statistics-level", "nonsense");
    assert!(main.start().is_err());
}

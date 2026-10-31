use camel_core::prelude::*;

fn ctx_with_routes() -> CamelContext {
    let ctx = CamelContext::with_name("mgmt-test");
    ctx.add_routes(|b| {
        b.from("direct:start")
            .route_id("main")
            .route_description("The main route")
            .route_group("g1")
            .process(|ex: &mut Exchange| {
                if ex.body_as::<String>()? == "fail" {
                    return Err(CamelError::illegal_argument("boom"));
                }
                Ok(())
            })
            .id("check")
            .filter(body().is_equal_to("x"))
            .to("mock:filtered")
            .end()
            .to("mock:out");
        b.from("direct:other").route_id("other").to("mock:other");
    })
    .unwrap();
    ctx
}

#[test]
fn route_and_processor_statistics() {
    let ctx = ctx_with_routes();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for b in ["a", "x", "b"] {
        t.send_body("direct:start", b).unwrap();
    }
    assert!(t.send_body("direct:start", "fail").is_err());
    assert!(t.send_body("direct:start", "fail").is_err());

    let mc = ctx.managed_context();
    assert_eq!(mc.name(), "mgmt-test");
    assert_eq!(mc.status(), ServiceStatus::Started);
    assert_eq!(mc.exchanges_total(), 5);
    assert_eq!(mc.exchanges_failed(), 2);
    assert_eq!(mc.exchanges_completed(), 3);
    assert_eq!(mc.exchanges_inflight(), 0);

    let r = mc.route("main").unwrap();
    assert_eq!(r.endpoint_uri, "direct://start");
    assert_eq!(r.status, ServiceStatus::Started);
    assert_eq!(r.description.as_deref(), Some("The main route"));
    assert_eq!(r.group.as_deref(), Some("g1"));
    assert_eq!(r.stats.exchanges_total, 5);
    assert_eq!(r.stats.exchanges_failed, 2);
    assert_eq!(r.stats.exchanges_completed, 3);
    assert!(r.stats.last_exchange_timestamp.is_some());
    assert!(r.stats.last_exchange_failure_timestamp.is_some());
    assert!(r.stats.min_processing_time <= r.stats.max_processing_time);

    // node statistics in definition order with explicit and generated ids
    let ids: Vec<&str> = r.processors.iter().map(|p| p.node_id.as_str()).collect();
    assert_eq!(ids.len(), 4, "{:?}", ids);
    assert_eq!(ids[0], "check");
    assert!(ids[1].starts_with("filter"), "{:?}", ids);
    assert!(ids[2].starts_with("to"), "{:?}", ids);
    assert!(ids[3].starts_with("to"), "{:?}", ids);
    let check = &r.processors[0];
    assert_eq!((check.stats.exchanges_total, check.stats.exchanges_failed), (5, 2));
    let filter = &r.processors[1];
    assert_eq!(filter.stats.exchanges_total, 3);
    assert_eq!(filter.level, 0);
    let filtered_to = &r.processors[2];
    assert_eq!(filtered_to.level, 1);
    assert_eq!(filtered_to.label, "to[mock:filtered]");
    assert_eq!(filtered_to.stats.exchanges_total, 1);
    assert_eq!(r.processors[3].stats.exchanges_total, 3);
    assert!(mc.processor("check").is_some());
    assert!(r.dump.contains("check: process"), "{}", r.dump);
    assert!(r.dump.contains("    "), "{}", r.dump);

    // JSON export
    let v = r.to_value();
    assert_eq!(v.get("routeId").unwrap().as_str(), Some("main"));
    assert_eq!(v.get("statistics").unwrap().get("exchangesFailed").unwrap().as_i64(), Some(2));
    let cv = mc.to_value();
    assert_eq!(cv.get("exchangesTotal").unwrap().as_i64(), Some(5));
    assert_eq!(cv.get("totalRoutes").unwrap().as_i64(), Some(2));
    let dump = mc.dump_value();
    assert!(dump.get("endpoints").unwrap().as_array().unwrap().iter().any(|e| e.get("uri").unwrap().as_str() == Some("direct://start")));
    assert!(mc.components().contains(&"direct".to_string()));
    assert!(mc.endpoints().iter().any(|e| e.uri == "mock://out" && e.scheme == "mock"));
    assert_eq!(mc.endpoints().iter().find(|e| e.uri == "direct://start").unwrap().consumers, vec!["main".to_string()]);

    // reset
    mc.reset_statistics();
    let r = mc.route("main").unwrap();
    assert_eq!(r.stats.exchanges_total, 0);
    assert_eq!(r.processors[0].stats.exchanges_total, 0);
    assert_eq!(mc.exchanges_total(), 0);
    ctx.stop().unwrap();
}

#[test]
fn prometheus_exposition() {
    let ctx = ctx_with_routes();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:start", "a").unwrap();
    t.send_body("direct:start", "b").unwrap();
    let _ = t.send_body("direct:start", "fail");
    t.send_body("direct:other", "z").unwrap();
    let text = ctx.managed_context().prometheus();
    assert!(text.contains("# TYPE camel_exchanges_total counter"), "{}", text);
    assert!(text.contains("camel_exchanges_total{camelContext=\"mgmt-test\",routeId=\"main\"} 3"), "{}", text);
    assert!(text.contains("camel_exchanges_failed_total{camelContext=\"mgmt-test\",routeId=\"main\"} 1"), "{}", text);
    assert!(text.contains("camel_exchanges_succeeded_total{camelContext=\"mgmt-test\",routeId=\"other\"} 1"), "{}", text);
    assert!(text.contains("camel_exchanges_inflight{camelContext=\"mgmt-test\",routeId=\"main\"} 0"), "{}", text);
    assert!(text.contains("camel_route_policy_seconds_count{camelContext=\"mgmt-test\",routeId=\"main\"} 3"), "{}", text);
    assert!(text.contains("camel_route_policy_seconds_sum{camelContext=\"mgmt-test\",routeId=\"main\"} "), "{}", text);
    assert!(text.contains("camel_routes_running_routes{camelContext=\"mgmt-test\"} 2"), "{}", text);
    assert!(text.contains("camel_processor_exchanges_total{camelContext=\"mgmt-test\",routeId=\"main\",nodeId=\"check\"} 3"), "{}", text);
    // every sample line is "name{labels} value"
    for line in text.lines().filter(|l| !l.starts_with('#')) {
        let (_, v) = line.rsplit_once(' ').unwrap();
        assert!(v.parse::<f64>().is_ok(), "{}", line);
    }
    ctx.stop_route("other").unwrap();
    assert!(ctx.managed_context().prometheus().contains("camel_routes_running_routes{camelContext=\"mgmt-test\"} 1"));
    ctx.stop().unwrap();
}

#[test]
fn route_operations() {
    let ctx = ctx_with_routes();
    ctx.start().unwrap();
    let mc = ctx.managed_context();
    mc.stop_route("other").unwrap();
    assert_eq!(mc.route("other").unwrap().status, ServiceStatus::Stopped);
    mc.start_route("other").unwrap();
    assert_eq!(mc.route("other").unwrap().status, ServiceStatus::Started);
    assert!(mc.route("other").unwrap().uptime().ends_with('s'));
    mc.suspend_route("other").unwrap();
    assert_eq!(mc.route("other").unwrap().status, ServiceStatus::Suspended);
    mc.resume_route("other").unwrap();
    assert_eq!(mc.route("other").unwrap().status, ServiceStatus::Started);
    assert!(!mc.remove_route("other").unwrap(), "a started route cannot be removed");
    mc.stop_route("other").unwrap();
    assert!(mc.remove_route("other").unwrap());
    assert!(mc.route("other").is_none());
    assert_eq!(mc.routes().len(), 1);
    ctx.stop().unwrap();
}

#[test]
fn statistics_can_be_disabled() {
    let ctx = ctx_with_routes();
    ctx.set_statistics_enabled(false);
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:other", "x").unwrap();
    assert_eq!(ctx.managed_context().route("other").unwrap().stats.exchanges_total, 0);
    ctx.stop().unwrap();
}

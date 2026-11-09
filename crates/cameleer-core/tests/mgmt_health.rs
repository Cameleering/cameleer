use cameleer_core::health::{FnHealthCheck, HealthCheckResult, HealthState};
use cameleer_core::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[test]
fn builtin_checks_follow_lifecycle() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:a").route_id("a").to("mock:a");
        b.from("direct:b").route_id("b").auto_startup(false).to("mock:b");
    })
    .unwrap();
    // not started yet
    let r = ctx.health_readiness();
    assert_eq!(r.state, HealthState::Down);
    assert_eq!(r.check("context").unwrap().state, HealthState::Down);

    ctx.start().unwrap();
    let ready = ctx.health_readiness();
    assert!(ready.is_up(), "{:?}", ready);
    let ids: Vec<&str> = ready.checks.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, vec!["context", "routes", "consumers"]);
    let live = ctx.health_liveness();
    assert!(live.is_up());
    assert_eq!(live.checks.len(), 1, "only the context check is a liveness check");

    // stopping an auto-startup route makes readiness DOWN, liveness stays UP
    ctx.stop_route("a").unwrap();
    let ready = ctx.health_readiness();
    assert_eq!(ready.state, HealthState::Down);
    let routes = ready.check("routes").unwrap();
    assert_eq!(routes.state, HealthState::Down);
    assert!(routes.message.as_deref().unwrap().contains('a'));
    assert_eq!(routes.details.get("route.a").unwrap().as_str(), Some("Stopped"));
    assert_eq!(ready.check("consumers").unwrap().state, HealthState::Down);
    assert!(ctx.health_liveness().is_up());

    ctx.start_route("a").unwrap();
    assert!(ctx.health_readiness().is_up());

    let json = ctx.health().to_value();
    assert_eq!(json.get("status").unwrap().as_str(), Some("UP"));
    assert_eq!(json.get("checks").unwrap().as_array().unwrap()[0].get("name").unwrap().as_str(), Some("context"));

    ctx.stop().unwrap();
    assert_eq!(ctx.health_liveness().state, HealthState::Down);
}

#[test]
fn a_suspended_consumer_is_down_in_status_and_details() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("seda:ticks").route_id("ticks").to("mock:t");
    })
    .unwrap();
    ctx.start().unwrap();
    let c = ctx.health_readiness().check("consumers").unwrap().clone();
    assert_eq!(c.state, HealthState::Up);
    assert_eq!(c.details.get("consumer.ticks").unwrap().as_str(), Some("UP"));
    ctx.suspend_route("ticks").unwrap();
    let c = ctx.health_readiness().check("consumers").unwrap().clone();
    assert_eq!(c.state, HealthState::Down);
    // the detail agrees with the status (it used to say UP: the consumer object still exists)
    let d = c.details.get("consumer.ticks").unwrap().as_str().unwrap().to_string();
    assert!(d.starts_with("DOWN"), "{}", d);
    ctx.resume_route("ticks").unwrap();
    assert_eq!(ctx.health_readiness().check("consumers").unwrap().details.get("consumer.ticks").unwrap().as_str(), Some("UP"));
    ctx.stop().unwrap();
}

#[test]
fn custom_checks_and_registry() {
    let ctx = CamelContext::new();
    ctx.start().unwrap();
    let healthy = Arc::new(AtomicBool::new(true));
    let h = healthy.clone();
    ctx.register_health_check(
        FnHealthCheck::new("db", move || {
            if h.load(Ordering::SeqCst) {
                HealthCheckResult::up().with_detail("pool", 5)
            } else {
                HealthCheckResult::down("connection refused")
            }
        })
        .readiness_only()
        .group("infra"),
    );
    assert!(ctx.health_readiness().is_up());
    healthy.store(false, Ordering::SeqCst);
    let r = ctx.health_readiness();
    assert_eq!(r.state, HealthState::Down);
    assert_eq!(r.check("db").unwrap().message.as_deref(), Some("connection refused"));
    // liveness does not include the readiness-only check
    assert!(ctx.health_liveness().is_up());

    let reg = ctx.health_check_registry();
    reg.set_enabled("db", false);
    assert!(ctx.health_readiness().is_up());
    reg.set_enabled("db", true);
    assert!(!ctx.health_readiness().is_up());
    assert!(reg.unregister("db"));

    // dynamic repository and UNKNOWN does not fail the aggregate
    reg.add_repository(Arc::new(|| {
        vec![Arc::new(FnHealthCheck::new("dyn", HealthCheckResult::unknown)) as Arc<dyn cameleer_core::health::HealthCheck>]
    }));
    let r = ctx.health();
    assert!(r.is_up());
    assert_eq!(r.check("dyn").unwrap().state, HealthState::Unknown);
    assert_eq!(reg.call("context").unwrap().state, HealthState::Up);
    ctx.stop().unwrap();
}

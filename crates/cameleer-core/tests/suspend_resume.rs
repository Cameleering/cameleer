//! Suspend/resume: SuspendableService consumers are suspended in place.

use std::time::Duration;

use cameleer_core::prelude::*;

fn counter(ctx: &CamelContext, uri: &str) -> usize {
    ctx.mock_endpoint(uri).unwrap().received_counter()
}

#[test]
fn seda_suspended_in_place_keeps_messages_queued() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("seda:q?concurrentConsumers=2").route_id("q").to("mock:q");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.suspend_route("q").unwrap();
    assert!(ctx.is_route_suspended_in_place("q"));
    assert_eq!(ctx.route_status("q"), Some(ServiceStatus::Suspended));
    let t = ctx.create_producer_template();
    for i in 0..3 {
        t.send_body("seda:q", i).unwrap();
    }
    std::thread::sleep(Duration::from_millis(80));
    assert_eq!(counter(&ctx, "mock:q"), 0);
    ctx.resume_route("q").unwrap();
    assert_eq!(ctx.route_status("q"), Some(ServiceStatus::Started));
    assert!(ctx.mock_endpoint("mock:q").unwrap().wait_for_messages(3, Duration::from_secs(5)));
    ctx.stop().unwrap();
}

#[test]
fn timer_and_scheduler_skip_fires_while_suspended() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("timer:t?period=5").route_id("t").to("mock:t");
        b.from("scheduler:s?delay=5").route_id("s").to("mock:s");
    })
    .unwrap();
    ctx.start().unwrap();
    assert!(ctx.mock_endpoint("mock:t").unwrap().wait_for_messages(1, Duration::from_secs(5)));
    assert!(ctx.mock_endpoint("mock:s").unwrap().wait_for_messages(1, Duration::from_secs(5)));
    ctx.suspend().unwrap();
    assert_eq!(ctx.status(), ServiceStatus::Suspended);
    assert!(ctx.is_route_suspended_in_place("t") && ctx.is_route_suspended_in_place("s"));
    std::thread::sleep(Duration::from_millis(30));
    let (t0, s0) = (counter(&ctx, "mock:t"), counter(&ctx, "mock:s"));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!((counter(&ctx, "mock:t"), counter(&ctx, "mock:s")), (t0, s0));
    ctx.resume().unwrap();
    assert_eq!(ctx.status(), ServiceStatus::Started);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while (counter(&ctx, "mock:t") <= t0 || counter(&ctx, "mock:s") <= s0) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(counter(&ctx, "mock:t") > t0 && counter(&ctx, "mock:s") > s0);
    ctx.stop().unwrap();
}

#[test]
fn file_consumer_skips_polls_while_suspended() {
    let dir = std::env::temp_dir().join(format!("camel-suspend-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let ctx = CamelContext::new();
    let uri = format!("file:{}?delay=5&initialDelay=0", dir.display());
    ctx.add_routes(move |b| {
        b.from(uri.as_str()).route_id("f").to("mock:f");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.suspend_route("f").unwrap();
    assert!(ctx.is_route_suspended_in_place("f"));
    std::fs::write(dir.join("a.txt"), "A").unwrap();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(counter(&ctx, "mock:f"), 0);
    assert!(dir.join("a.txt").exists());
    ctx.resume_route("f").unwrap();
    assert!(ctx.mock_endpoint("mock:f").unwrap().wait_for_messages(1, Duration::from_secs(5)));
    ctx.stop().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn non_suspendable_consumer_is_stopped_and_restarted() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:d").route_id("d").to("mock:d");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.suspend_route("d").unwrap();
    assert_eq!(ctx.route_status("d"), Some(ServiceStatus::Suspended));
    assert!(!ctx.is_route_suspended_in_place("d"));
    assert!(ctx.create_producer_template().send_body("direct:d", "x").is_err());
    ctx.start_route("d").unwrap(); // start on a suspended route resumes it
    assert_eq!(ctx.route_status("d"), Some(ServiceStatus::Started));
    ctx.create_producer_template().send_body("direct:d", "x").unwrap();
    assert_eq!(counter(&ctx, "mock:d"), 1);
    // a suspended-in-place route can be stopped and removed
    ctx.stop_route("d").unwrap();
    assert!(ctx.remove_route("d").unwrap());
    ctx.stop().unwrap();
}

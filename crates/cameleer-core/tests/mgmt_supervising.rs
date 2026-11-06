use std::any::Any;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cameleer_core::health::HealthState;
use cameleer_core::prelude::*;
use cameleer_core::supervising::{SupervisedState, SupervisingRouteControllerConfig};

/// A component whose consumers fail to start `fail_times` times, then succeed.
struct FlakyComponent {
    fail_times: usize,
    attempts: Arc<AtomicUsize>,
}

struct FlakyEndpoint {
    uri: String,
    fail_times: usize,
    attempts: Arc<AtomicUsize>,
}

struct FlakyConsumer {
    fail_times: usize,
    attempts: Arc<AtomicUsize>,
}

impl Service for FlakyConsumer {
    fn start(&self) -> Result<()> {
        let n = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
        if n <= self.fail_times {
            Err(CamelError::io(format!("cannot connect (attempt {})", n)))
        } else {
            Ok(())
        }
    }
}

impl Endpoint for FlakyEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_consumer(&self, _p: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        Ok(Arc::new(FlakyConsumer { fail_times: self.fail_times, attempts: self.attempts.clone() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Component for FlakyComponent {
    fn create_endpoint(&self, _c: &CamelContext, uri: &str, _r: &str, _p: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        Ok(Arc::new(FlakyEndpoint { uri: uri.to_string(), fail_times: self.fail_times, attempts: self.attempts.clone() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn wait_until(timeout: Duration, f: impl Fn() -> bool) -> bool {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    f()
}

#[test]
fn without_supervision_context_start_fails() {
    let ctx = CamelContext::new();
    ctx.add_component("flaky", FlakyComponent { fail_times: 1, attempts: Arc::default() });
    ctx.add_routes(|b| {
        b.from("flaky:x").route_id("flaky").to("mock:out");
    })
    .unwrap();
    assert!(ctx.start().is_err());
    assert!(!ctx.route_controller_status().supervising);
}

#[test]
fn supervising_controller_retries_with_backoff() {
    let ctx = CamelContext::new();
    let attempts = Arc::new(AtomicUsize::new(0));
    ctx.add_component("flaky", FlakyComponent { fail_times: 2, attempts: attempts.clone() });
    ctx.add_routes(|b| {
        b.from("flaky:x").route_id("flaky").to("mock:out");
        b.from("direct:ok").route_id("ok").to("mock:ok");
    })
    .unwrap();
    ctx.enable_supervising_route_controller(
        SupervisingRouteControllerConfig::default().initial_delay(Duration::from_millis(20)).back_off_delay(Duration::from_millis(30)),
    );
    assert!(ctx.is_supervising_route_controller());
    ctx.start().unwrap();
    assert!(ctx.is_started());
    assert_eq!(ctx.route_status("ok"), Some(ServiceStatus::Started));

    let st = ctx.route_controller_status();
    assert!(st.supervising);
    let e = st.route("flaky").unwrap();
    assert_eq!(e.state, SupervisedState::Restarting);
    assert_eq!(e.attempts, 1);
    assert!(e.last_error.as_deref().unwrap().contains("cannot connect (attempt 1)"));
    assert_eq!(st.route("ok").unwrap().state, SupervisedState::Started);
    assert_eq!(st.failing().len(), 1);
    let v = st.to_value();
    assert_eq!(v.get("controller").unwrap().as_str(), Some("SupervisingRouteController"));
    // readiness reports the failing route
    let ready = ctx.health_readiness();
    assert_eq!(ready.state, HealthState::Down);
    assert!(ready.check("routes").unwrap().details.get("route.flaky.error").is_some());

    assert!(wait_until(Duration::from_secs(5), || ctx.route_status("flaky") == Some(ServiceStatus::Started)));
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    let st = ctx.route_controller_status();
    assert_eq!(st.route("flaky").unwrap().state, SupervisedState::Started);
    assert!(st.failing().is_empty());
    assert!(ctx.health_readiness().is_up());
    ctx.stop().unwrap();
}

#[test]
fn supervising_controller_gives_up_after_max_attempts() {
    let ctx = CamelContext::new();
    let attempts = Arc::new(AtomicUsize::new(0));
    ctx.add_component("flaky", FlakyComponent { fail_times: 100, attempts: attempts.clone() });
    ctx.add_routes(|b| {
        b.from("flaky:y").route_id("never").to("mock:out");
    })
    .unwrap();
    ctx.enable_supervising_route_controller(
        SupervisingRouteControllerConfig::default().back_off_delay(Duration::from_millis(5)).back_off_max_attempts(3),
    );
    ctx.start().unwrap();
    assert!(wait_until(Duration::from_secs(5), || ctx.route_controller_status().route("never").unwrap().state == SupervisedState::Exhausted));
    let e = ctx.route_controller_status().route("never").unwrap().clone();
    assert_eq!(e.attempts, 4, "initial attempt + 3 restarts");
    assert!(e.next_attempt_in.is_none());
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(attempts.load(Ordering::SeqCst), 4);
    ctx.stop().unwrap();
}

#[test]
fn excluded_routes_are_not_supervised() {
    let ctx = CamelContext::new();
    ctx.add_component("flaky", FlakyComponent { fail_times: 1, attempts: Arc::default() });
    ctx.add_routes(|b| {
        b.from("flaky:z").route_id("critical").to("mock:out");
    })
    .unwrap();
    ctx.enable_supervising_route_controller(SupervisingRouteControllerConfig::default().exclude_routes("crit*"));
    assert!(ctx.start().is_err());
}

#[test]
fn stopping_the_context_stops_retrying() {
    let ctx = CamelContext::new();
    let attempts = Arc::new(AtomicUsize::new(0));
    ctx.add_component("flaky", FlakyComponent { fail_times: 1000, attempts: attempts.clone() });
    ctx.add_routes(|b| {
        b.from("flaky:w").route_id("w").to("mock:out");
    })
    .unwrap();
    ctx.enable_supervising_route_controller(SupervisingRouteControllerConfig::default().back_off_delay(Duration::from_millis(2)));
    ctx.start().unwrap();
    assert!(wait_until(Duration::from_secs(5), || attempts.load(Ordering::SeqCst) >= 3));
    ctx.stop().unwrap();
    let n = attempts.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(attempts.load(Ordering::SeqCst), n);
}

#[test]
fn controller_thread_runs_only_while_routes_wait_for_a_restart() {
    use cameleer_core::supervising::controller_thread_running;
    let ctx = CamelContext::new();
    ctx.add_component("flaky", FlakyComponent { fail_times: 1, attempts: Arc::default() });
    ctx.add_component("flaky2", FlakyComponent { fail_times: 1, attempts: Arc::default() });
    ctx.add_routes(|b| {
        b.from("flaky:a").route_id("a").to("mock:out");
    })
    .unwrap();
    ctx.enable_supervising_route_controller(SupervisingRouteControllerConfig::default().back_off_delay(Duration::from_millis(20)));
    assert!(!controller_thread_running(&ctx), "no thread before a failure");
    ctx.start().unwrap();
    assert!(wait_until(Duration::from_secs(5), || ctx.route("a").unwrap().status() == ServiceStatus::Started));
    // the route recovered: nothing left to supervise, the thread exits
    assert!(wait_until(Duration::from_secs(5), || !controller_thread_running(&ctx)), "thread still running");
    // a new failure starts it again
    ctx.add_routes(|b| {
        b.from("flaky2:b").route_id("b").to("mock:out");
    })
    .unwrap();
    assert!(controller_thread_running(&ctx) || ctx.route("b").unwrap().status() == ServiceStatus::Started);
    assert!(wait_until(Duration::from_secs(5), || ctx.route("b").unwrap().status() == ServiceStatus::Started));
    assert!(wait_until(Duration::from_secs(5), || !controller_thread_running(&ctx)));
    ctx.stop().unwrap();
}

#[test]
fn controller_thread_exits_when_the_only_failing_route_is_exhausted() {
    use cameleer_core::supervising::controller_thread_running;
    let ctx = CamelContext::new();
    ctx.add_component("flaky", FlakyComponent { fail_times: 1000, attempts: Arc::default() });
    ctx.add_routes(|b| {
        b.from("flaky:e").route_id("e").to("mock:out");
    })
    .unwrap();
    ctx.enable_supervising_route_controller(SupervisingRouteControllerConfig::default().back_off_delay(Duration::from_millis(5)).back_off_max_attempts(2));
    ctx.start().unwrap();
    assert!(wait_until(Duration::from_secs(5), || ctx.route_controller_status().route("e").map(|r| r.state.as_str() == "Exhausted").unwrap_or(false)));
    assert!(wait_until(Duration::from_secs(5), || !controller_thread_running(&ctx)));
    ctx.stop().unwrap();
}

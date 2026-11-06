use std::time::{Duration, Instant};

use cameleer_core::component::scheduler::SCHEDULER_POLLED_MESSAGES;
use cameleer_core::prelude::*;

#[test]
fn timer_repeat_count_delivers_exact_number() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("timer:tick?repeatCount=3&period=10&delay=0").to("mock:out");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:out").unwrap();
    mock.expected_message_count(3);
    mock.set_assert_period(Duration::from_millis(60));
    mock.assert_is_satisfied().unwrap();
    let counters: Vec<i64> = mock.received_exchanges().iter().map(|e| e.property("CamelTimerCounter").unwrap().as_i64().unwrap()).collect();
    assert_eq!(counters, vec![1, 2, 3]);
    assert!(mock.received_exchanges().iter().all(|e| e.body().is_null()));
    ctx.stop().unwrap();
}

#[test]
fn timer_include_metadata_sets_headers() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("timer:meta?repeatCount=1&delay=0&includeMetadata=true&period=5").to("mock:meta");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:meta").unwrap();
    mock.expected_message_count(1);
    mock.assert_is_satisfied().unwrap();
    let ex = mock.exchange(0).unwrap();
    assert_eq!(ex.property("CamelTimerName").unwrap(), &Value::from("meta"));
    assert_eq!(ex.property("CamelTimerPeriod").unwrap().as_i64(), Some(5));
    assert!(ex.header("CamelTimerFiredTime").is_some());
    assert_eq!(ex.header("CamelTimerCounter").unwrap().as_i64(), Some(1));
    ctx.stop().unwrap();
}

#[test]
fn timer_without_metadata_has_no_name_property() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("timer:plain?repeatCount=1&delay=0").to("mock:plain");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:plain").unwrap();
    mock.expected_message_count(1);
    mock.assert_is_satisfied().unwrap();
    assert!(mock.exchange(0).unwrap().property("CamelTimerName").is_none());
    ctx.stop().unwrap();
}

#[test]
fn timer_stops_promptly() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("timer:slow?period=60000&delay=60000").to("mock:never");
    })
    .unwrap();
    ctx.start().unwrap();
    let start = Instant::now();
    ctx.stop().unwrap();
    assert!(start.elapsed() < Duration::from_millis(500), "stop took {:?}", start.elapsed());
}

#[test]
fn timer_fixed_rate_and_route_stop_start() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("timer:rate?period=5&delay=0&fixedRate=true").route_id("rate").to("mock:rate");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:rate").unwrap();
    assert!(mock.wait_for_messages(3, Duration::from_secs(5)));
    ctx.stop_route("rate").unwrap();
    let n = mock.received_counter();
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(mock.received_counter(), n, "no messages after stop");
    ctx.start_route("rate").unwrap();
    assert!(mock.wait_for_messages(n + 2, Duration::from_secs(5)));
    ctx.stop().unwrap();
}

#[test]
fn timer_bad_option_fails() {
    let ctx = CamelContext::new();
    assert!(ctx.get_endpoint("timer:x?period=abc").is_err());
    assert!(ctx.get_endpoint("timer:x?nope=1").is_err());
}

#[test]
fn scheduler_repeat_count_and_properties() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("scheduler:sched?initialDelay=0&delay=5&repeatCount=4").to("mock:sched");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:sched").unwrap();
    mock.expected_message_count(4);
    mock.set_assert_period(Duration::from_millis(50));
    mock.assert_is_satisfied().unwrap();
    assert_eq!(mock.exchange(0).unwrap().property("CamelTimerName").unwrap(), &Value::from("sched"));
    ctx.stop().unwrap();
}

#[test]
fn scheduler_greedy_and_backoff() {
    let ctx = CamelContext::new();
    // the route reports "nothing polled" -> after 1 idle poll the scheduler skips 1000 polls
    ctx.add_routes(|b| {
        b.from("scheduler:idle?initialDelay=0&delay=2&backoffMultiplier=1000&backoffIdleThreshold=1")
            .process(|ex: &mut Exchange| {
                ex.set_property(SCHEDULER_POLLED_MESSAGES, false);
                Ok(())
            })
            .to("mock:idle");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:idle").unwrap();
    mock.expected_message_count(1);
    mock.set_assert_period(Duration::from_millis(80));
    mock.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
}

#[test]
fn scheduler_cron_and_unknown_scheduler_rejected() {
    let ctx = CamelContext::new();
    // cron scheduling is supported (see component_cron.rs); invalid expressions are rejected
    assert!(ctx.get_endpoint("scheduler:c?scheduler=quartz&scheduler.cron=0+0+*+*+*").is_ok());
    assert!(ctx.get_endpoint("scheduler:c2?scheduler.cron=0+0+*+*").is_err());
    assert!(ctx.get_endpoint("scheduler:c3?scheduler=bogus").is_err());
    assert!(ctx.get_endpoint("scheduler:c?scheduler=spring").is_ok());
    assert!(ctx.get_endpoint("scheduler:d?timeUnit=SECONDS&delay=1").is_ok());
}

#[test]
fn scheduler_start_scheduler_false_does_not_fire() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("scheduler:manual?initialDelay=0&delay=1&startScheduler=false").to("mock:manual");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:manual").unwrap();
    mock.expected_message_count(0);
    mock.assert_is_satisfied_within(Duration::from_millis(50)).unwrap();
    ctx.stop().unwrap();
}

fn timer_threads(ctx: &CamelContext) -> usize {
    use cameleer_core::component::timer::TimerComponent;
    let c = ctx.component::<TimerComponent>("timer").expect("timer component");
    c.as_any().downcast_ref::<TimerComponent>().unwrap().thread_count()
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
fn timer_routes_of_one_name_share_one_thread() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        for i in 0..5 {
            b.from("timer:shared?period=60000&delay=60000").route_id(&format!("s{}", i)).to("mock:never");
        }
        b.from("timer:other?period=60000&delay=60000").route_id("o").to("mock:never");
    })
    .unwrap();
    ctx.start().unwrap();
    // one thread per timer name, as Camel's java.util.Timer per name
    assert_eq!(timer_threads(&ctx), 2);
    // stopping some consumers of a name keeps its thread for the others
    for i in 0..4 {
        ctx.stop_route(&format!("s{}", i)).unwrap();
    }
    assert_eq!(timer_threads(&ctx), 2);
    // the last consumer of a name stops: its thread is gone
    ctx.stop_route("s4").unwrap();
    assert_eq!(timer_threads(&ctx), 1);
    ctx.start_route("s4").unwrap();
    assert_eq!(timer_threads(&ctx), 2);
    ctx.stop().unwrap();
    assert_eq!(timer_threads(&ctx), 0);
}

#[test]
fn timer_tasks_of_one_name_keep_their_own_schedule_and_counter() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        let tag = |ex: &mut Exchange| {
            ex.set_property("thread", std::thread::current().name().unwrap_or("").to_string());
            Ok(())
        };
        b.from("timer:pair?period=10&delay=0&repeatCount=4").route_id("a").process(tag).to("mock:a");
        b.from("timer:pair?period=25&delay=5&repeatCount=3&includeMetadata=true").route_id("b").process(tag).to("mock:b");
    })
    .unwrap();
    ctx.start().unwrap();
    let (a, b) = (ctx.mock_endpoint("mock:a").unwrap(), ctx.mock_endpoint("mock:b").unwrap());
    assert!(a.wait_for_messages(4, Duration::from_secs(5)));
    assert!(b.wait_for_messages(3, Duration::from_secs(5)));
    let counters = |m: &MockEndpoint| m.received_exchanges().iter().map(|e| e.property("CamelTimerCounter").unwrap().as_i64().unwrap()).collect::<Vec<_>>();
    assert_eq!(counters(&a), vec![1, 2, 3, 4]);
    assert_eq!(counters(&b), vec![1, 2, 3]);
    assert!(b.received_exchanges().iter().all(|e| e.property("CamelTimerPeriod").unwrap().as_i64() == Some(25)));
    // both ran on the same thread
    let names: std::collections::BTreeSet<String> = a.received_exchanges().iter().chain(b.received_exchanges().iter()).map(|e| e.property("thread").unwrap().to_string()).collect();
    assert_eq!(names.len(), 1, "{names:?}");
    assert_eq!(names.iter().next().unwrap(), "Camel timer://pair");
    // all tasks done: the thread exits on its own
    assert!(wait_until(Duration::from_secs(5), || timer_threads(&ctx) == 0));
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(a.received_counter(), 4);
    assert_eq!(b.received_counter(), 3);
    ctx.stop().unwrap();
}

#[test]
fn a_slow_task_delays_the_other_tasks_of_its_timer_name() {
    // java.util.Timer runs the tasks of one timer one after the other
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("timer:busy?period=1000&delay=0&repeatCount=1").route_id("slow").process(|_: &mut Exchange| {
            std::thread::sleep(Duration::from_millis(300));
            Ok(())
        });
        b.from("timer:busy?period=1000&delay=20&repeatCount=1").route_id("fast").to("mock:fast");
        b.from("timer:free?period=1000&delay=20&repeatCount=1").route_id("free").to("mock:free");
    })
    .unwrap();
    let start = Instant::now();
    ctx.start().unwrap();
    let (fast, free) = (ctx.mock_endpoint("mock:fast").unwrap(), ctx.mock_endpoint("mock:free").unwrap());
    assert!(free.wait_for_messages(1, Duration::from_secs(5)));
    assert!(fast.wait_for_messages(1, Duration::from_secs(5)));
    let fast_at = start.elapsed();
    assert!(fast_at >= Duration::from_millis(250), "fast fired after {fast_at:?}");
    ctx.stop().unwrap();
}

#[test]
fn stopping_a_route_from_its_own_timer_exchange_does_not_hang() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("timer:self?period=10&delay=0").route_id("self").to("mock:self").to("controlbus:route?routeId=current&action=stop");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:self").unwrap();
    assert!(mock.wait_for_messages(1, Duration::from_secs(5)));
    assert!(wait_until(Duration::from_secs(5), || ctx.route("self").unwrap().status() == ServiceStatus::Stopped));
    assert!(wait_until(Duration::from_secs(5), || timer_threads(&ctx) == 0));
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(mock.received_counter(), 1);
    ctx.stop().unwrap();
}

#[test]
fn timer_with_a_huge_delay_or_period_never_fires_and_stops() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("timer:far?delay=9223372036854775807").to("mock:far");
        b.from("timer:once?delay=0&period=9223372036854775807").to("mock:once");
    })
    .unwrap();
    ctx.start().unwrap();
    let once = ctx.mock_endpoint("mock:once").unwrap();
    assert!(once.wait_for_messages(1, Duration::from_secs(5)));
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(once.received_counter(), 1);
    assert_eq!(ctx.mock_endpoint("mock:far").unwrap().received_counter(), 0);
    ctx.stop().unwrap();
    assert_eq!(timer_threads(&ctx), 0);
}

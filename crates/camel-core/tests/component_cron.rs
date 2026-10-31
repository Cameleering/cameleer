//! cron / quartz components and `scheduler=quartz` cron polling.

use std::time::Duration;

use camel_core::component::cron::{CronExpression, Trigger};
use camel_core::prelude::*;

#[test]
fn cron_route_fires_every_second() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        // `+` is decoded as space
        b.from("cron:tick?schedule=*/1+*+*+*+*+?").to("mock:cron");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:cron").unwrap();
    assert!(mock.wait_for_messages(2, Duration::from_millis(2600)), "expected at least 2 fires, got {}", mock.received_counter());
    ctx.stop().unwrap();
    let exs = mock.received_exchanges();
    let e0 = &exs[0];
    assert!(e0.body().is_null());
    let scheduled = e0.header("scheduledFireTime").unwrap().as_i64().unwrap();
    assert_eq!(scheduled % 1000, 0, "scheduled fire times are on second boundaries");
    assert!(e0.header("fireTime").unwrap().as_i64().unwrap() >= scheduled);
    assert_eq!(e0.header("nextFireTime").unwrap().as_i64().unwrap(), scheduled + 1000);
    assert_eq!(e0.header("triggerName").unwrap(), &Value::from("tick"));
    assert_eq!(e0.property("CamelTimerName").unwrap(), &Value::from("tick"));
    assert_eq!(e0.property("CamelTimerCounter").unwrap().as_i64(), Some(1));
    let e1 = &exs[1];
    assert_eq!(e1.header("previousFireTime").unwrap().as_i64(), Some(scheduled));
    assert!(e1.header("scheduledFireTime").unwrap().as_i64().unwrap() > scheduled);
}

#[test]
fn cron_with_spaces_and_timezone() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("cron:tz?schedule=0/1 * * * * ?&timeZone=GMT+02:00").to("mock:tz");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:tz").unwrap();
    assert!(mock.wait_for_messages(1, Duration::from_millis(1600)));
    ctx.stop().unwrap();
}

#[test]
fn cron_endpoint_validation() {
    let ctx = CamelContext::new();
    assert!(ctx.get_endpoint("cron:x").is_err(), "schedule is required");
    assert!(ctx.get_endpoint("cron:x?schedule=bad").is_err());
    assert!(ctx.get_endpoint("cron:x?schedule=0+0+12+*+*+?&timeZone=Mars/Base").is_err());
    assert!(ctx.get_endpoint("cron:x?schedule=@daily").is_ok());
    if camel_core::tz::zoneinfo_available() {
        assert!(ctx.get_endpoint("cron:paris?schedule=0+0+12+*+*+?&timeZone=Europe/Paris").is_ok());
        assert!(ctx.get_endpoint("quartz:g/paris?cron=0+0+12+*+*+?&trigger.timeZone=America/New_York").is_ok());
        assert!(ctx.get_endpoint("scheduler:p?scheduler=quartz&scheduler.cron=0+0+12+*+*+?&scheduler.timeZone=Asia/Tokyo").is_ok());
    }
    assert!(ctx.get_endpoint("quartz:g/n?cron=0+0+12+*+*+?&trigger.timeZone=UTC&deleteJob=false").is_ok());
}

#[test]
fn quartz_simple_trigger_repeat_count() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("quartz:myGroup/simple?trigger.repeatInterval=50&trigger.repeatCount=2&fireNow=true").to("mock:simple");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:simple").unwrap();
    mock.expected_message_count(3);
    mock.set_assert_period(Duration::from_millis(200));
    mock.assert_is_satisfied().unwrap();
    let ex = mock.exchange(0).unwrap();
    assert_eq!(ex.header("triggerGroup").unwrap(), &Value::from("myGroup"));
    assert_eq!(ex.header("triggerName").unwrap(), &Value::from("simple"));
    assert!(mock.exchange(2).unwrap().header("nextFireTime").is_none(), "last fire has no next fire time");
    ctx.stop().unwrap();
}

#[test]
fn quartz_cron_trigger() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("quartz:cronTimer?cron=*/1+*+*+*+*+?&triggerStartDelay=0").to("mock:qc");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:qc").unwrap();
    assert!(mock.wait_for_messages(2, Duration::from_millis(2600)));
    ctx.stop().unwrap();
    assert_eq!(mock.exchange(0).unwrap().header("triggerGroup").unwrap(), &Value::from("Camel"));
}

#[test]
fn quartz_endpoint_exposes_trigger() {
    let ctx = CamelContext::new();
    let ep = ctx.get_endpoint("quartz:g/t?cron=0+15+10+?+*+MON-FRI").unwrap();
    let te = ep.as_any().downcast_ref::<camel_core::component::cron::TriggerEndpoint>().unwrap();
    match &te.config().trigger {
        Trigger::Cron(c) => assert_eq!(c.expression(), "0 15 10 ? * MON-FRI"),
        other => panic!("unexpected trigger {:?}", other),
    }
    // schedule computation is available without running anything
    let c = CronExpression::parse("0 15 10 ? * MON-FRI").unwrap();
    // 2024-05-18 is a Saturday -> next fire Monday 2024-05-20 10:15:00 UTC
    let sat = camel_core::util_time::epoch_from_civil(2024, 5, 18, 12, 0, 0);
    assert_eq!(c.next_after(sat), Some(camel_core::util_time::epoch_from_civil(2024, 5, 20, 10, 15, 0)));
}

#[test]
fn scheduler_with_quartz_cron() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("scheduler:poll?scheduler=quartz&scheduler.cron=*/1+*+*+*+*+?&repeatCount=2").to("mock:sched");
    })
    .unwrap();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:sched").unwrap();
    mock.expected_message_count(2);
    mock.set_result_wait_time(Duration::from_millis(2600));
    mock.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
    assert_eq!(mock.exchange(0).unwrap().property("CamelTimerName").unwrap(), &Value::from("poll"));
}

#[test]
fn file_consumer_accepts_scheduler_cron() {
    let ctx = CamelContext::new();
    let dir = std::env::temp_dir().join(format!("camel-cron-file-{}", std::process::id()));
    let uri = format!("file:{}?scheduler=spring&scheduler.cron=0+0+*+*+*+?", dir.display());
    assert!(ctx.get_endpoint(&uri).is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

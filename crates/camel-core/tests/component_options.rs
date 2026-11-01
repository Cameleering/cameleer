//! `camel.component.<scheme>.<option>` component-level options.

use std::time::Duration;

use camel_core::prelude::*;

#[test]
fn component_options_are_endpoint_defaults() {
    let ctx = CamelContext::new();
    ctx.properties().set("camel.component.timer.period", "{{p}}");
    ctx.properties().set("p", "5");
    ctx.properties().set("camel.component.timer.repeat-count", "2");
    // unknown component options are ignored
    ctx.properties().set("camel.component.timer.notAnOption", "x");
    ctx.set_component_option("log", "showAll", "true");
    ctx.add_routes(|b| {
        b.from("timer:a").to("log:x").to("mock:a");
        b.from("timer:b?repeatCount=1").to("mock:b");
    })
    .unwrap();
    assert_eq!(ctx.component_options("timer").get("repeatCount").map(String::as_str), Some("2"));
    ctx.start().unwrap();
    assert!(ctx.mock_endpoint("mock:a").unwrap().wait_for_messages(2, Duration::from_secs(5)));
    assert!(ctx.mock_endpoint("mock:b").unwrap().wait_for_messages(1, Duration::from_secs(5)));
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(ctx.mock_endpoint("mock:a").unwrap().received_counter(), 2);
    assert_eq!(ctx.mock_endpoint("mock:b").unwrap().received_counter(), 1);
    // explicit unknown uri parameters still fail
    assert!(ctx.get_endpoint("timer:c?bogus=1").is_err());
    ctx.stop().unwrap();
}

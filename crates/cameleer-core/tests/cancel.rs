//! Cooperative cancellation: a cancelled exchange stops before its next pipeline step, and
//! copies made by EIPs share the token.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cameleer_core::prelude::*;

#[test]
fn a_cancelled_exchange_stops_before_the_next_step() {
    let ctx = CamelContext::new();
    let token = Arc::new(AtomicBool::new(false));
    let t2 = token.clone();
    ctx.add_routes(move |b| {
        let t2 = t2.clone();
        b.from("direct:start")
            .to("mock:first")
            .process(move |_ex: &mut Exchange| {
                t2.store(true, Ordering::Relaxed);
                Ok(())
            })
            .to("mock:second");
        b.from("direct:split").split(body()).to("mock:part").end().to("mock:after-split");
    })
    .unwrap();
    ctx.start().unwrap();
    let mut ex = Exchange::with_body(Some(ctx.clone()), "x");
    ex.set_cancel_token(token.clone());
    assert!(!ex.is_cancelled());
    let done = ctx.create_producer_template().send("direct:start", ex);
    assert!(done.is_cancelled());
    assert_eq!(ctx.mock_endpoint("mock:first").unwrap().received_counter(), 1);
    assert_eq!(ctx.mock_endpoint("mock:second").unwrap().received_counter(), 0);

    // copies share the token: a cancelled split runs no part
    let mut ex = Exchange::with_body(Some(ctx.clone()), Value::Array(vec![Value::from("a"), Value::from("b")]));
    let cancelled = Arc::new(AtomicBool::new(true));
    ex.set_cancel_token(cancelled);
    let copy = ex.clone();
    assert!(copy.is_cancelled() && copy.correlated_copy().is_cancelled());
    ctx.create_producer_template().send("direct:split", ex);
    assert_eq!(ctx.mock_endpoint("mock:part").unwrap().received_counter(), 0);
    assert_eq!(ctx.mock_endpoint("mock:after-split").unwrap().received_counter(), 0);
    ctx.stop().unwrap();
}

#[test]
fn a_passed_deadline_cancels_the_exchange_and_its_copies() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:slow").to("mock:before").delay(constant(150)).to("mock:after");
    })
    .unwrap();
    ctx.start().unwrap();
    let mut ex = Exchange::with_body(Some(ctx.clone()), "x");
    ex.set_deadline(std::time::Instant::now() + std::time::Duration::from_millis(50));
    assert!(!ex.is_cancelled());
    assert!(!ex.clone().is_cancelled(), "a copy shares the deadline but it has not passed");
    let done = ctx.create_producer_template().send("direct:slow", ex);
    assert!(done.is_cancelled());
    assert_eq!(ctx.mock_endpoint("mock:before").unwrap().received_counter(), 1);
    // the step after the delay did not run: the deadline passed during the delay
    assert_eq!(ctx.mock_endpoint("mock:after").unwrap().received_counter(), 0);
    // a deadline in the future changes nothing
    let mut ex = Exchange::with_body(Some(ctx.clone()), "y");
    ex.set_deadline(std::time::Instant::now() + std::time::Duration::from_secs(10));
    let done = ctx.create_producer_template().send("direct:slow", ex);
    assert!(!done.is_cancelled());
    assert_eq!(ctx.mock_endpoint("mock:after").unwrap().received_counter(), 1);
    ctx.stop().unwrap();
}

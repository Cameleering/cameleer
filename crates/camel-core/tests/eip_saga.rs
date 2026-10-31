//! Saga EIP: success/failure/timeout/nested/propagation/manual completion with the
//! in-memory service, and the LRA service against the embedded mock coordinator.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use camel_core::prelude::*;
use camel_core::processor::saga::lra::testing::{LraStatus, MockLraCoordinator};
use camel_core::processor::saga::{
    CamelContextSagaExt, InMemorySagaService, LraSagaService, SagaCompletionMode, SagaPropagation, SAGA_LONG_RUNNING_ACTION,
};

fn fail(msg: &'static str) -> impl Fn(&mut Exchange) -> Result<()> + Send + Sync {
    move |_ex: &mut Exchange| Err(CamelError::io(msg))
}

fn lra_of(ex: &Exchange) -> String {
    ex.header(SAGA_LONG_RUNNING_ACTION).map(|v| v.to_string()).unwrap_or_default()
}

fn header_values(ctx: &CamelContext, mock: &str, h: &str) -> Vec<String> {
    ctx.mock_endpoint(mock).unwrap().received_exchanges().iter().map(|e| e.header(h).map(|v| v.to_string()).unwrap_or_default()).collect()
}

fn saga_routes(ctx: &CamelContext) {
    ctx.add_routes(|b| {
        b.from("direct:start")
            .saga()
            .compensation("mock:compensate")
            .completion("mock:complete")
            .option("id", header("id"))
            .option("step", constant("outer"))
            .to("mock:work")
            .filter(header("fail").is_equal_to(true))
            .process(fail("boom"))
            .end()
            .end()
            .to("mock:after");
    })
    .unwrap();
}

#[test]
fn saga_completes_on_success() {
    let ctx = CamelContext::new();
    saga_routes(&ctx);
    ctx.start().unwrap();
    let ex = ctx.create_producer_template().request("direct:start", |ex| ex.set_header("id", 7)).unwrap();
    assert!(!ex.is_failed());
    let complete = ctx.mock_endpoint("complete").unwrap();
    assert_eq!(complete.received_counter(), 1);
    assert_eq!(ctx.mock_endpoint("compensate").unwrap().received_counter(), 0);
    let c = complete.exchange(0).unwrap();
    assert_eq!(c.header("id"), Some(&Value::from(7)));
    assert_eq!(c.header("step"), Some(&Value::from("outer")));
    assert_eq!(lra_of(&c), lra_of(&ex));
    assert!(!lra_of(&ex).is_empty());
    // the work saw the saga header
    assert_eq!(lra_of(&ctx.mock_endpoint("work").unwrap().exchange(0).unwrap()), lra_of(&ex));
    assert_eq!(ctx.mock_endpoint("after").unwrap().received_counter(), 1);
}

#[test]
fn saga_compensates_on_failure() {
    let ctx = CamelContext::new();
    saga_routes(&ctx);
    ctx.start().unwrap();
    let r = ctx.create_producer_template().request_body_and_headers("direct:start", "x", vec![("id", Value::from(9)), ("fail", Value::Bool(true))]);
    assert!(r.is_err());
    assert_eq!(ctx.mock_endpoint("complete").unwrap().received_counter(), 0);
    assert_eq!(header_values(&ctx, "compensate", "id"), vec!["9"]);
    assert_eq!(ctx.mock_endpoint("after").unwrap().received_counter(), 0);
    // compensation message: empty body, saga header set
    let c = ctx.mock_endpoint("compensate").unwrap().exchange(0).unwrap();
    assert!(c.body().is_null());
    assert!(!lra_of(&c).is_empty());
}

fn nested_routes(ctx: &CamelContext, inner: SagaPropagation) {
    ctx.add_routes(move |b| {
        b.from("direct:outer")
            .saga()
            .compensation("mock:compensate")
            .completion("mock:complete")
            .option("step", constant("outer"))
            .to("direct:inner")
            .to("mock:outerLra")
            .filter(header("fail").is_equal_to(true))
            .process(fail("outer failed"))
            .end()
            .end();
        b.from("direct:inner")
            .saga()
            .propagation(inner)
            .compensation("mock:compensate")
            .completion("mock:complete")
            .option("step", constant("inner"))
            .to("mock:innerLra")
            .end();
    })
    .unwrap();
}

#[test]
fn nested_required_saga_joins_and_compensates_in_reverse_order() {
    let ctx = CamelContext::new();
    nested_routes(&ctx, SagaPropagation::Required);
    ctx.start().unwrap();
    let r = ctx.create_producer_template().request_body_and_header("direct:outer", "x", "fail", true);
    assert!(r.is_err());
    assert_eq!(header_values(&ctx, "compensate", "step"), vec!["inner", "outer"]);
    assert_eq!(ctx.mock_endpoint("complete").unwrap().received_counter(), 0);
    let inner = lra_of(&ctx.mock_endpoint("innerLra").unwrap().exchange(0).unwrap());
    let outer = lra_of(&ctx.mock_endpoint("outerLra").unwrap().exchange(0).unwrap());
    assert_eq!(inner, outer);

    // success: both steps complete once, when the outer saga ends
    for m in ["compensate", "complete", "innerLra", "outerLra"] {
        ctx.mock_endpoint(m).unwrap().reset();
    }
    ctx.create_producer_template().request_body("direct:outer", "x").unwrap();
    assert_eq!(header_values(&ctx, "complete", "step"), vec!["inner", "outer"]);
}

#[test]
fn requires_new_saga_completes_independently() {
    let ctx = CamelContext::new();
    nested_routes(&ctx, SagaPropagation::RequiresNew);
    ctx.start().unwrap();
    let r = ctx.create_producer_template().request_body_and_header("direct:outer", "x", "fail", true);
    assert!(r.is_err());
    assert_eq!(header_values(&ctx, "complete", "step"), vec!["inner"]);
    assert_eq!(header_values(&ctx, "compensate", "step"), vec!["outer"]);
    let inner = lra_of(&ctx.mock_endpoint("innerLra").unwrap().exchange(0).unwrap());
    let outer = lra_of(&ctx.mock_endpoint("outerLra").unwrap().exchange(0).unwrap());
    assert_ne!(inner, outer);
    // the outer saga header is restored after the inner saga
    assert_eq!(lra_of(&ctx.mock_endpoint("compensate").unwrap().exchange(0).unwrap()), outer);
}

#[test]
fn propagation_mandatory_supports_not_supported_never() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:mandatory").saga().propagation(SagaPropagation::Mandatory).to("mock:m").end();
        b.from("direct:supports").saga().propagation(SagaPropagation::Supports).to("mock:s").end();
        b.from("direct:notSupported").saga().propagation(SagaPropagation::NotSupported).to("mock:ns").end().to("mock:nsAfter");
        b.from("direct:never").saga().propagation(SagaPropagation::Never).to("mock:n").end();
        b.from("direct:inSaga").saga().choice().when(header("target").is_not_null()).to_d("${header.target}").end().end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();

    let e = t.request_body("direct:mandatory", "x").unwrap_err();
    assert!(format!("{}", e).contains("no saga in progress"), "{}", e);
    t.request_body_and_header("direct:inSaga", "x", "target", "direct:mandatory").unwrap();
    assert!(!lra_of(&ctx.mock_endpoint("m").unwrap().exchange(0).unwrap()).is_empty());

    t.request_body("direct:supports", "x").unwrap();
    t.request_body_and_header("direct:inSaga", "x", "target", "direct:supports").unwrap();
    let s = ctx.mock_endpoint("s").unwrap().received_exchanges();
    assert!(lra_of(&s[0]).is_empty());
    assert!(!lra_of(&s[1]).is_empty());

    t.request_body_and_header("direct:inSaga", "x", "target", "direct:notSupported").unwrap();
    assert!(lra_of(&ctx.mock_endpoint("ns").unwrap().exchange(0).unwrap()).is_empty());
    assert!(!lra_of(&ctx.mock_endpoint("nsAfter").unwrap().exchange(0).unwrap()).is_empty());

    t.request_body("direct:never", "x").unwrap();
    let e = t.request_body_and_header("direct:inSaga", "x", "target", "direct:never").unwrap_err();
    assert!(format!("{}", e).contains("already in progress"), "{}", e);
}

#[test]
fn manual_completion_with_saga_component() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .saga()
            .completion_mode(SagaCompletionMode::Manual)
            .compensation("mock:compensate")
            .completion("mock:complete")
            .option("id", body())
            .to("mock:work")
            .end();
        b.from("direct:finish").to("saga:complete");
        b.from("direct:cancel").to("saga:compensate");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let ex = t.request("direct:start", |ex| ex.set_body("a")).unwrap();
    assert_eq!(ctx.mock_endpoint("complete").unwrap().received_counter(), 0);
    let lra = lra_of(&ex);
    t.request_body_and_header("direct:finish", "", SAGA_LONG_RUNNING_ACTION, lra.clone()).unwrap();
    assert_eq!(header_values(&ctx, "complete", "id"), vec!["a"]);

    let ex = t.request("direct:start", |ex| ex.set_body("b")).unwrap();
    t.request_body_and_header("direct:cancel", "", SAGA_LONG_RUNNING_ACTION, lra_of(&ex)).unwrap();
    assert_eq!(header_values(&ctx, "compensate", "id"), vec!["b"]);
    assert_eq!(ctx.mock_endpoint("complete").unwrap().received_counter(), 1);

    // not bound to a saga
    let e = t.request_body("direct:finish", "").unwrap_err();
    assert!(format!("{}", e).contains("not bound to a saga"), "{}", e);
}

#[test]
fn manual_saga_is_compensated_on_timeout() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .saga()
            .completion_mode(SagaCompletionMode::Manual)
            .saga_timeout(Duration::from_millis(100))
            .compensation("mock:compensate")
            .completion("mock:complete")
            .to("mock:work")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let compensate = ctx.mock_endpoint("compensate").unwrap();
    compensate.expected_message_count(1);
    let ex = ctx.create_producer_template().request("direct:start", |ex| ex.set_body("a")).unwrap();
    assert_eq!(compensate.received_counter(), 0, "compensated too early");
    compensate.assert_is_satisfied_within(Duration::from_secs(5)).unwrap();
    assert_eq!(lra_of(&compensate.exchange(0).unwrap()), lra_of(&ex));
    assert_eq!(ctx.mock_endpoint("complete").unwrap().received_counter(), 0);
}

#[test]
fn compensation_is_retried() {
    let ctx = CamelContext::new();
    ctx.set_saga_service(Arc::new(InMemorySagaService::new(&ctx).with_max_retry_attempts(3).with_retry_delay(Duration::from_millis(5))));
    let attempts = Arc::new(AtomicUsize::new(0));
    let a = attempts.clone();
    ctx.add_routes(move |b| {
        b.from("direct:start").saga().compensation("direct:undo").process(fail("work failed")).end();
        b.from("direct:undo").process(move |_ex: &mut Exchange| {
            if a.fetch_add(1, Ordering::SeqCst) < 2 {
                return Err(CamelError::io("undo not yet possible"));
            }
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    assert!(ctx.create_producer_template().request_body("direct:start", "x").is_err());
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
}

#[test]
fn auto_saga_compensated_by_timeout_is_not_completed() {
    // completing a saga that was already compensated by the timeout fails the exchange
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:slow")
            .saga()
            .saga_timeout(Duration::from_millis(30))
            .compensation("mock:compensate")
            .completion("mock:complete")
            .process(|_ex: &mut Exchange| {
                std::thread::sleep(Duration::from_millis(300));
                Ok(())
            })
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let e = ctx.create_producer_template().request_body("direct:slow", "x").unwrap_err();
    assert!(format!("{}", e).contains("Cannot complete"), "{}", e);
    assert_eq!(ctx.mock_endpoint("compensate").unwrap().received_counter(), 1);
    assert_eq!(ctx.mock_endpoint("complete").unwrap().received_counter(), 0);
}

#[test]
fn lra_saga_service_against_mock_coordinator() {
    let coordinator = MockLraCoordinator::start().unwrap();
    let ctx = CamelContext::new();
    let service = LraSagaService::new(&ctx, coordinator.url(), "http://127.0.0.1:0");
    ctx.set_saga_service(Arc::new(service.clone()));
    saga_routes(&ctx);
    ctx.start().unwrap();
    let t = ctx.create_producer_template();

    let ex = t.request("direct:start", |ex| ex.set_header("id", 7)).unwrap();
    assert!(!ex.is_failed());
    let lra = lra_of(&ex);
    assert!(lra.starts_with(&coordinator.url()), "{}", lra);
    assert_eq!(coordinator.status(&lra), Some(LraStatus::Closed));
    assert_eq!(coordinator.participants(&lra), 1);
    let complete = ctx.mock_endpoint("complete").unwrap();
    complete.expected_message_count(1);
    complete.assert_is_satisfied_within(Duration::from_secs(5)).unwrap();
    let c = complete.exchange(0).unwrap();
    assert_eq!(c.header("id"), Some(&Value::from("7")));
    assert_eq!(c.header("step"), Some(&Value::from("outer")));
    assert_eq!(lra_of(&c), lra);

    let r = t.request_body_and_headers("direct:start", "x", vec![("id", Value::from(8)), ("fail", Value::Bool(true))]);
    assert!(r.is_err());
    let compensate = ctx.mock_endpoint("compensate").unwrap();
    compensate.expected_message_count(1);
    compensate.assert_is_satisfied_within(Duration::from_secs(5)).unwrap();
    assert_eq!(header_values(&ctx, "compensate", "id"), vec!["8"]);
    let lras = coordinator.lras();
    assert_eq!(lras.len(), 2);
    assert!(lras.iter().any(|l| coordinator.status(l) == Some(LraStatus::Cancelled)));
    ctx.stop().unwrap();
    camel_core::spi::Service::stop(&service).unwrap();
}

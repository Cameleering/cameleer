use std::any::Any;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use cameleer_core::prelude::*;
use cameleer_core::processor::routing::strategies::{grouped_body, string};
use cameleer_core::spi::PollingConsumer;

// ---------------------------------------------------------------------------
// a tiny queue component supporting producers and polling consumers
// ---------------------------------------------------------------------------

type Queue = Arc<(Mutex<VecDeque<Exchange>>, Condvar)>;

#[derive(Default)]
struct QueueComponent {
    queues: Mutex<HashMap<String, Queue>>,
}

struct QueueEndpoint {
    uri: String,
    queue: Queue,
}

struct QueueProducer(Queue);
impl Service for QueueProducer {}
impl Processor for QueueProducer {
    fn process(&self, exchange: &mut Exchange) -> Result<()> {
        self.0 .0.lock().unwrap().push_back(exchange.clone());
        self.0 .1.notify_all();
        Ok(())
    }
}

struct QueuePoller(Queue);
impl Service for QueuePoller {}
impl PollingConsumer for QueuePoller {
    fn receive(&self, timeout: Option<Duration>) -> Result<Option<Exchange>> {
        let deadline = Instant::now() + timeout.unwrap_or(Duration::ZERO).min(Duration::from_secs(3600));
        let mut q = self.0 .0.lock().unwrap();
        loop {
            if let Some(e) = q.pop_front() {
                return Ok(Some(e));
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            q = self.0 .1.wait_timeout(q, deadline - now).unwrap().0;
        }
    }
}

impl Endpoint for QueueEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        Ok(Arc::new(QueueProducer(self.queue.clone())))
    }
    fn create_polling_consumer(&self) -> Result<Arc<dyn PollingConsumer>> {
        Ok(Arc::new(QueuePoller(self.queue.clone())))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Component for QueueComponent {
    fn create_endpoint(&self, _ctx: &CamelContext, uri: &str, remaining: &str, _params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let q = self.queues.lock().unwrap().entry(remaining.to_string()).or_default().clone();
        Ok(Arc::new(QueueEndpoint { uri: uri.to_string(), queue: q }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---------------------------------------------------------------------------
// recipientList
// ---------------------------------------------------------------------------

#[test]
fn recipient_list_from_header() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").recipient_list(header("to")).to("mock:after");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body_and_header("direct:start", "x", "to", "mock:a, mock:b").unwrap();
    t.send_body_and_header("direct:start", "y", "to", Value::from(vec!["mock:b"])).unwrap();
    assert_eq!(ctx.mock_endpoint("a").unwrap().received_bodies(), vec![Value::from("x")]);
    let b = ctx.mock_endpoint("b").unwrap();
    assert_eq!(b.received_bodies(), vec![Value::from("x"), Value::from("y")]);
    assert_eq!(b.received_exchanges()[0].property(props::RECIPIENT_LIST_ENDPOINT), Some(&Value::from("mock://b")));
    assert_eq!(ctx.mock_endpoint("after").unwrap().received_counter(), 2);
}

#[test]
fn recipient_list_options_delimiter_parallel_strategy() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .recipient_list(header("to"))
            .delimiter(";")
            .parallel_processing()
            .aggregation_strategy(string("|"))
            .to("mock:result");
        b.from("direct:x").set_body(constant("X"));
        b.from("direct:y").process(|ex: &mut Exchange| {
            std::thread::sleep(Duration::from_millis(20));
            ex.set_body("Y");
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let r = ctx.create_producer_template().request_body_and_header("direct:start", "in", "to", "direct:y;direct:x").unwrap();
    assert_eq!(r, Value::from("Y|X"));
}

#[test]
fn recipient_list_invalid_endpoints() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:strict").recipient_list(header("to"));
        b.from("direct:lenient").recipient_list(header("to")).ignore_invalid_endpoints();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let e = t.send_body_and_header("direct:strict", "x", "to", "mock:ok,nope:bad").unwrap_err();
    assert!(e.cause.unwrap().is_a(kinds::RESOLVE_ENDPOINT_FAILED));
    assert_eq!(ctx.mock_endpoint("ok").unwrap().received_counter(), 0);
    t.send_body_and_header("direct:lenient", "x", "to", "mock:ok,nope:bad").unwrap();
    assert_eq!(ctx.mock_endpoint("ok").unwrap().received_counter(), 1);
}

#[test]
fn recipient_list_inside_split_options_target_recipient_list() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .split(body())
            .aggregation_strategy(grouped_body())
            .recipient_list(constant("direct:a,direct:b"))
            .aggregation_strategy(string("+"))
            .end();
        b.from("direct:a").set_body(body().append("a"));
        b.from("direct:b").set_body(body().append("b"));
    })
    .unwrap();
    ctx.start().unwrap();
    let r = ctx.create_producer_template().request_body("direct:start", "1,2").unwrap();
    assert_eq!(r, Value::from(vec!["1a+1b", "2a+2b"]));
}

// ---------------------------------------------------------------------------
// routingSlip / dynamicRouter
// ---------------------------------------------------------------------------

#[test]
fn routing_slip_sequential() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").routing_slip(header("slip")).to("mock:result");
        b.from("direct:a").set_body(body().append("A"));
        b.from("direct:b").set_body(body().append("B"));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let r = t.request_body_and_header("direct:start", "x", "slip", "direct:a,direct:b,mock:slip").unwrap();
    assert_eq!(r, Value::from("xAB"));
    let s = ctx.mock_endpoint("slip").unwrap().received_exchanges();
    assert_eq!(s[0].body(), &Value::from("xAB"));
    assert_eq!(s[0].property(props::SLIP_ENDPOINT), Some(&Value::from("mock://slip")));
}

#[test]
fn routing_slip_stops_on_failure_and_custom_delimiter() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").routing_slip(header("slip")).delimiter("#").ignore_invalid_endpoints();
        b.from("direct:fail").process(|_: &mut Exchange| Err(CamelError::io("x")));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body_and_header("direct:start", "x", "slip", "mock:s1#bad:x#mock:s2").unwrap();
    assert_eq!(ctx.mock_endpoint("s1").unwrap().received_counter(), 1);
    assert_eq!(ctx.mock_endpoint("s2").unwrap().received_counter(), 1);
    assert!(t.send_body_and_header("direct:start", "x", "slip", "direct:fail#mock:s3").is_err());
    assert_eq!(ctx.mock_endpoint("s3").unwrap().received_counter(), 0);
}

#[test]
fn dynamic_router_until_null() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .dynamic_router(expr(|ex: &Exchange| {
                let last = ex.property(props::SLIP_ENDPOINT).map(|v| v.to_string());
                Ok(match last.as_deref() {
                    None => Value::from("direct:a"),
                    // CamelSlipEndpoint holds the normalized uri, as in Camel
                    Some("direct://a") => Value::from("mock:b,direct:c"),
                    Some("direct://c") => Value::from("mock:d"),
                    _ => Value::Null,
                })
            }))
            .to("mock:result");
        b.from("direct:a").set_body(body().append("a"));
        b.from("direct:c").set_body(body().append("c"));
    })
    .unwrap();
    ctx.start().unwrap();
    let r = ctx.create_producer_template().request_body("direct:start", "x").unwrap();
    assert_eq!(r, Value::from("xac"));
    assert_eq!(ctx.mock_endpoint("b").unwrap().received_bodies(), vec![Value::from("xa")]);
    assert_eq!(ctx.mock_endpoint("d").unwrap().received_bodies(), vec![Value::from("xac")]);
    assert_eq!(ctx.mock_endpoint("result").unwrap().received_counter(), 1);
}

#[test]
fn dynamic_router_failure_stops() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").dynamic_router(expr(|ex: &Exchange| {
            Ok(if ex.property(props::SLIP_ENDPOINT).is_none() { Value::from("direct:fail") } else { Value::from("mock:never") })
        }));
        b.from("direct:fail").process(|_: &mut Exchange| Err(CamelError::io("x")));
    })
    .unwrap();
    ctx.start().unwrap();
    assert!(ctx.create_producer_template().send_body("direct:start", "x").is_err());
    assert_eq!(ctx.mock_endpoint("never").unwrap().received_counter(), 0);
}

// ---------------------------------------------------------------------------
// wireTap
// ---------------------------------------------------------------------------

#[test]
fn wire_tap_is_async_and_copies() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").wire_tap("direct:tap").set_body(constant("main")).to("mock:result");
        b.from("direct:tap")
            .process(|_: &mut Exchange| {
                std::thread::sleep(Duration::from_millis(100));
                Ok(())
            })
            .to("mock:tap");
    })
    .unwrap();
    ctx.start().unwrap();
    let start = Instant::now();
    ctx.create_producer_template().send_body("direct:start", "orig").unwrap();
    assert!(start.elapsed() < Duration::from_millis(80));
    assert_eq!(ctx.mock_endpoint("result").unwrap().received_bodies(), vec![Value::from("main")]);
    let tap = ctx.mock_endpoint("tap").unwrap();
    tap.expected_bodies_received(vec!["orig"]);
    tap.assert_is_satisfied_within(Duration::from_secs(2)).unwrap();
    assert_eq!(tap.received_exchanges()[0].pattern, ExchangePattern::InOnly);
    ctx.stop().unwrap();
}

#[test]
fn wire_tap_new_exchange_and_dynamic_uri() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .wire_tap("mock:${header.target}")
            .new_exchange_body(constant("tapped"))
            .new_exchange_header("tap", constant(true))
            .to("mock:result");
        b.from("direct:fresh").wire_tap("mock:fresh").copy(false).on_prepare(|ex: &mut Exchange| {
            ex.set_header("prepared", "yes");
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body_and_header("direct:start", "orig", "target", "dyn").unwrap();
    t.send_body_and_header("direct:fresh", "orig", "h", "v").unwrap();
    let dynm = ctx.mock_endpoint("dyn").unwrap();
    dynm.expected_message_count(1);
    dynm.assert_is_satisfied_within(Duration::from_secs(2)).unwrap();
    let e = &dynm.received_exchanges()[0];
    assert_eq!(e.body(), &Value::from("tapped"));
    assert_eq!(e.header("tap"), Some(&Value::Bool(true)));
    assert_eq!(e.header("target"), Some(&Value::from("dyn")));
    assert_eq!(ctx.mock_endpoint("result").unwrap().received_bodies(), vec![Value::from("orig")]);
    let fresh = ctx.mock_endpoint("fresh").unwrap();
    fresh.expected_message_count(1);
    fresh.assert_is_satisfied_within(Duration::from_secs(2)).unwrap();
    let f = &fresh.received_exchanges()[0];
    assert!(f.body().is_null());
    assert!(f.header("h").is_none());
    assert_eq!(f.header("prepared"), Some(&Value::from("yes")));
    ctx.stop().unwrap();
}

#[test]
fn wire_tap_failure_does_not_affect_original() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").wire_tap("direct:tap").to("mock:result");
        b.from("direct:tap").process(|_: &mut Exchange| Err(CamelError::io("tap failed")));
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "x").unwrap();
    assert_eq!(ctx.mock_endpoint("result").unwrap().received_counter(), 1);
    ctx.stop().unwrap();
}

// ---------------------------------------------------------------------------
// enrich / pollEnrich
// ---------------------------------------------------------------------------

#[test]
fn enrich_default_replaces_body() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").enrich("direct:resource").to("mock:result");
        b.from("direct:resource").set_body(body().prepend("resource:")).set_header("enriched", constant(true));
    })
    .unwrap();
    ctx.start().unwrap();
    let r = ctx.create_producer_template().request_body("direct:start", "x").unwrap();
    assert_eq!(r, Value::from("resource:x"));
    let e = &ctx.mock_endpoint("result").unwrap().received_exchanges()[0];
    assert_eq!(e.header("enriched"), Some(&Value::Bool(true)));
    assert!(e.property(props::CORRELATION_ID).is_none());
}

#[test]
fn enrich_with_strategy_and_dynamic_uri() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .enrich_with("direct:${header.res}", |old: Option<Exchange>, new: Exchange| -> Result<Exchange> {
                let mut o = old.unwrap();
                let s = format!("{}+{}", o.body(), new.body());
                o.set_body(s);
                Ok(o)
            })
            .to("mock:result");
        b.from("direct:r1").set_body(constant("R1"));
    })
    .unwrap();
    ctx.start().unwrap();
    let r = ctx.create_producer_template().request_body_and_header("direct:start", "x", "res", "r1").unwrap();
    assert_eq!(r, Value::from("x+R1"));
}

#[test]
fn enrich_failure_and_aggregate_on_exception() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:plain").enrich("direct:fail").to("mock:plain");
        b.from("direct:handled")
            .enrich("direct:fail")
            .aggregation_strategy(|old: Option<Exchange>, new: Exchange| -> Result<Exchange> {
                let mut o = old.unwrap();
                if new.is_failed() {
                    o.set_body("fallback");
                }
                Ok(o)
            })
            .aggregate_on_exception()
            .to("mock:handled");
        b.from("direct:fail").process(|_: &mut Exchange| Err(CamelError::io("resource down")));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let e = t.send_body("direct:plain", "x").unwrap_err();
    assert!(e.cause.unwrap().is_a(kinds::IO));
    assert_eq!(ctx.mock_endpoint("plain").unwrap().received_counter(), 0);
    assert_eq!(t.request_body("direct:handled", "x").unwrap(), Value::from("fallback"));
}

#[test]
fn poll_enrich_from_queue() {
    let ctx = CamelContext::new();
    ctx.add_component("queue", QueueComponent::default());
    ctx.add_routes(|b| {
        b.from("direct:start").poll_enrich("queue:data", 50).to("mock:result");
        b.from("direct:merge")
            .poll_enrich_with("queue:data", 0, |old: Option<Exchange>, new: Exchange| -> Result<Exchange> {
                let mut o = old.unwrap();
                let s = format!("{}:{}", o.body(), new.body());
                o.set_body(s);
                Ok(o)
            })
            .to("mock:merged");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("queue:data", "polled").unwrap();
    assert_eq!(t.request_body("direct:start", "x").unwrap(), Value::from("polled"));
    // nothing in the queue -> null body after timeout
    let start = Instant::now();
    assert_eq!(t.request_body("direct:start", "x").unwrap(), Value::Null);
    assert!(start.elapsed() >= Duration::from_millis(40));
    // strategy, no wait
    t.send_body("queue:data", "q").unwrap();
    assert_eq!(t.request_body("direct:merge", "orig").unwrap(), Value::from("orig:q"));
    assert_eq!(t.request_body("direct:merge", "orig").unwrap(), Value::from("orig:"));
}

#[test]
fn poll_enrich_waits_for_message() {
    let ctx = CamelContext::new();
    ctx.add_component("queue", QueueComponent::default());
    ctx.add_routes(|b| {
        b.from("direct:start").poll_enrich("queue:later", -1);
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let t2 = t.clone();
    let h = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        t2.send_body("queue:later", "late").unwrap();
    });
    assert_eq!(t.request_body("direct:start", "x").unwrap(), Value::from("late"));
    h.join().unwrap();
}

#[test]
fn poll_enrich_unsupported_endpoint_fails() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").poll_enrich("direct:nopoll", 0);
    })
    .unwrap();
    ctx.start().unwrap();
    let e = ctx.create_producer_template().send_body("direct:start", "x").unwrap_err();
    assert!(e.cause.unwrap().is_a(kinds::UNSUPPORTED_OPERATION));
}

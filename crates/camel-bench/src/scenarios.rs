//! The benchmark routes. `benchmarks/java-camel/src/main/java/bench/Bench.java` defines the
//! same routes with the Java DSL; keep the two in step.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use camel_core::prelude::*;
use camel_core::processor::routing::strategies::grouped_body;

use crate::driver::{self, Outcome, Plan};

/// In-process scenarios, in report order.
pub const SCENARIOS: &[&str] = &["direct", "seda", "seda-inout", "cbr", "splitagg"];

/// Producer threads for the seda scenarios (the route has 4 concurrent consumers).
pub const SEDA_PRODUCERS: usize = 4;
/// Items per order in the splitter/aggregator scenario.
pub const ITEMS: usize = 10;

/// A sink that counts what reaches the end of a route (the cheapest equivalent of a
/// `mock`/`log` sink with logging off, and the same on both sides).
fn counter(n: &Arc<AtomicU64>) -> impl Fn(&mut Exchange) -> Result<()> + Send + Sync + 'static {
    let n = n.clone();
    move |_ex: &mut Exchange| {
        n.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

/// One order with `ITEMS` line items, as JSON text.
pub fn order_json(id: u64) -> String {
    let items: Vec<String> =
        (0..ITEMS).map(|i| format!("{{\"sku\":\"SKU-{i}\",\"qty\":{},\"price\":{}.99}}", i + 1, i + 3)).collect();
    format!("{{\"orderId\":{id},\"customer\":\"ACME\",\"items\":[{}]}}", items.join(","))
}

/// The HTTP consumer route (`netty-http`, the same component name as the Java side).
pub fn add_http_route(ctx: &CamelContext, port: u16) -> Result<()> {
    ctx.add_routes(|b| {
        b.from(&format!("netty-http:http://127.0.0.1:{port}/bench"))
            .route_id("http")
            .set_header("Content-Type", constant("application/json"))
            .set_body(constant("{\"status\":\"ok\"}"));
    })
}

/// Add every in-process route. `sink` counts messages leaving the routes.
pub fn add_routes(ctx: &CamelContext, sink: &Arc<AtomicU64>) -> Result<()> {
    ctx.add_routes(|b| {
        // 1. in-process routing overhead
        b.from("direct:direct")
            .route_id("direct")
            .process(|ex: &mut Exchange| {
                ex.message.set_header("seen", true);
                Ok(())
            })
            .process(counter(sink));

        // 2. seda hand-off with concurrent consumers
        b.from("seda:seda?concurrentConsumers=4&size=1000&blockWhenFull=true")
            .route_id("seda")
            .process(|ex: &mut Exchange| {
                ex.message.set_header("seen", true);
                Ok(())
            })
            .process(counter(sink));
        b.from("seda:inout?concurrentConsumers=4")
            .route_id("seda-inout")
            .process(|ex: &mut Exchange| {
                ex.message.set_header("seen", true);
                Ok(())
            })
            .process(counter(sink));

        // 3. content-based router, Simple filter, setHeader and transform
        b.from("direct:cbr")
            .route_id("cbr")
            .choice()
            .when(simple("${header.type} == 'gold'"))
            .set_header("tier", constant("A"))
            .when(simple("${header.type} == 'silver'"))
            .set_header("tier", constant("B"))
            .otherwise()
            .set_header("tier", constant("C"))
            .end()
            .filter(simple("${header.amount} > 100"))
            .transform(simple("${body}-${header.tier}"))
            .process(counter(sink))
            .end();

        // 4. splitter + aggregator over JSON
        b.from("direct:splitagg")
            .route_id("splitagg")
            .set_header("orderId", jsonpath("$.orderId"))
            .split(jsonpath("$.items"))
            .aggregate(header("orderId"), grouped_body())
            .completion_size(ITEMS)
            .process(counter(sink))
            .end()
            .end();
    })
}

/// Run one in-process scenario and return its outcome.
pub fn run(ctx: &CamelContext, sink: &Arc<AtomicU64>, scenario: &str, plan: Plan) -> Result<Outcome> {
    let t = ctx.create_producer_template();
    let out = match scenario {
        "direct" => driver::run(plan, 1, None, |_| {
            let t = t.clone();
            move || t.send_body("direct:direct", "hello world").expect("direct send")
        }),
        "seda" => driver::run(plan, SEDA_PRODUCERS, Some(sink.clone()), |_| {
            let t = t.clone();
            move || t.send_body("seda:seda?blockWhenFull=true", "hello world").expect("seda send")
        }),
        "seda-inout" => driver::run(plan, SEDA_PRODUCERS, None, |_| {
            let t = t.clone();
            move || {
                t.request_body("seda:inout", "hello world").expect("seda request");
            }
        }),
        "cbr" => driver::run(plan, 1, None, |_| {
            let t = t.clone();
            let mut i = 0u64;
            move || {
                let ty = ["gold", "silver", "bronze"][(i % 3) as usize];
                let amount = [50, 150, 250][(i % 3 + i / 3 % 3) as usize % 3];
                i += 1;
                t.send_body_and_headers("direct:cbr", "order", vec![("type", Value::from(ty)), ("amount", Value::from(amount))])
                    .expect("cbr send")
            }
        }),
        "splitagg" => driver::run(plan, 1, None, |_| {
            let t = t.clone();
            let mut i = 0u64;
            move || {
                i += 1;
                t.send_body("direct:splitagg", order_json(i)).expect("splitagg send")
            }
        }),
        other => return Err(CamelError::illegal_argument(format!("unknown scenario: {other}"))),
    };
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn every_scenario_routes_messages_to_the_sink() {
        let ctx = CamelContext::new();
        let sink = Arc::new(AtomicU64::new(0));
        add_routes(&ctx, &sink).unwrap();
        ctx.start().unwrap();
        let t = ctx.create_producer_template();

        t.send_body("direct:direct", "x").unwrap();
        assert_eq!(sink.load(Ordering::SeqCst), 1);

        // cbr: only amounts above 100 pass the filter
        t.send_body_and_headers("direct:cbr", "o", vec![("type", Value::from("gold")), ("amount", Value::from(150))]).unwrap();
        t.send_body_and_headers("direct:cbr", "o", vec![("type", Value::from("bronze")), ("amount", Value::from(50))]).unwrap();
        assert_eq!(sink.load(Ordering::SeqCst), 2);

        // splitagg: one aggregate per order of ITEMS items
        t.send_body("direct:splitagg", order_json(7)).unwrap();
        t.send_body("direct:splitagg", order_json(8)).unwrap();
        assert_eq!(sink.load(Ordering::SeqCst), 4);

        assert_eq!(t.request_body("seda:inout", "x").unwrap(), Value::from("x"));
        assert_eq!(sink.load(Ordering::SeqCst), 5);
        t.send_body("seda:seda", "x").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while sink.load(Ordering::SeqCst) < 6 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(sink.load(Ordering::SeqCst), 6);
        ctx.stop().unwrap();
    }

    #[test]
    fn short_run_reports_throughput_and_latency() {
        let ctx = CamelContext::new();
        let sink = Arc::new(AtomicU64::new(0));
        add_routes(&ctx, &sink).unwrap();
        ctx.start().unwrap();
        let plan = Plan { warmup: Duration::from_millis(20), iteration: Duration::from_millis(50), iters: 1 };
        for s in SCENARIOS {
            let out = run(&ctx, &sink, s, plan).unwrap();
            assert!(out.msgs > 0, "{s}: {out:?}");
            assert_eq!(out.latency.count() > 0, *s != "seda", "{s}");
        }
        ctx.stop().unwrap();
    }

    #[test]
    fn order_json_is_valid_json_with_items() {
        let v = camel_core::json::parse(&order_json(3)).unwrap();
        assert_eq!(v.to_json(), order_json(3));
    }
}

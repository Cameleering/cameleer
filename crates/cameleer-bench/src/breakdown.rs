//! `cameleer-bench breakdown`: per-stage cost of one `ProducerTemplate.sendBody` to a
//! `direct:` route, to find where the time goes without an external profiler.

use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Instant;

use cameleer_core::prelude::*;

fn time(label: &str, n: u64, mut f: impl FnMut()) {
    for _ in 0..n / 10 {
        f();
    }
    let t0 = Instant::now();
    for _ in 0..n {
        f();
    }
    println!("BREAKDOWN {label:<40} {:>9.1} ns/op", t0.elapsed().as_nanos() as f64 / n as f64);
}

pub fn run(n: u64) -> Result<()> {
    let ctx = CamelContext::new();
    let sink = Arc::new(AtomicU64::new(0));
    crate::scenarios::add_routes(&ctx, &sink)?;
    ctx.add_routes(|b| {
        b.from("direct:p1").process(|_: &mut Exchange| Ok(()));
        b.from("direct:split-only").split(cameleer_core::language::jsonpath("$.items")).process(|_: &mut Exchange| Ok(())).end();
        let r = b.from("direct:p6");
        for _ in 0..6 {
            r.process(|_: &mut Exchange| Ok(()));
        }
    })?;
    ctx.start()?;
    let t = ctx.create_producer_template();
    let producer = ctx.get_producer("direct:direct")?;

    time("clock::ticks", n, || {
        std::hint::black_box(cameleer_core::clock::ticks());
    });
    time("Instant::now", n, || {
        std::hint::black_box(Instant::now());
    });
    time("SystemTime::now", n, || {
        std::hint::black_box(std::time::SystemTime::now());
    });
    time("String alloc + free (16 bytes)", n, || {
        std::hint::black_box(String::from("0123456789abcdef"));
    });
    let p1 = ctx.get_producer("direct:p1")?;
    let p6 = ctx.get_producer("direct:p6")?;
    time("route with 1 no-op processor", n, || {
        let mut ex = ctx.create_exchange();
        p1.process(&mut ex).unwrap();
    });
    time("route with 6 no-op processors", n, || {
        let mut ex = ctx.create_exchange();
        p6.process(&mut ex).unwrap();
    });

    time("Exchange::new (ids, timestamp)", n, || {
        std::hint::black_box(ctx.create_exchange());
    });
    time("generate_id", n, || {
        std::hint::black_box(cameleer_core::message::generate_id());
    });
    time("headers set + get", n, || {
        let mut h = Headers::new();
        h.set("seen", true);
        std::hint::black_box(h.get("Seen"));
    });
    time("ctx.get_producer(\"direct:direct\")", n, || {
        std::hint::black_box(ctx.get_producer("direct:direct").unwrap());
    });
    time("ctx.get_endpoint(\"direct:direct\")", n, || {
        std::hint::black_box(ctx.get_endpoint("direct:direct").unwrap());
    });
    time("notify (no notifiers)", n, || {
        ctx.notify(&cameleer_core::context::CamelEvent::ExchangeSending { exchange_id: String::new(), endpoint: String::new() });
    });
    time("producer.process (route only)", n, || {
        let mut ex = ctx.create_exchange();
        ex.message.body = Value::from("hello world");
        producer.process(&mut ex).unwrap();
    });
    time("template.send_body (end to end)", n, || {
        t.send_body("direct:direct", "hello world").unwrap();
    });
    use cameleer_core::management::ManagementStatisticsLevel as Level;
    for level in [Level::RoutesOnly, Level::Default, Level::Off] {
        ctx.set_statistics_level(level);
        time(&format!("template.send_body cbr ({level:?})"), n, || {
            t.send_body_and_headers("direct:cbr", "order", vec![("type", Value::from("gold")), ("amount", Value::from(150))]).unwrap();
        });
    }
    ctx.set_statistics_level(Level::RoutesOnly);
    // the pieces of the cbr route
    {
        use cameleer_core::language::simple::{SimpleExpression, SimplePredicate};
        use cameleer_core::spi::{Expression, Predicate};
        let mut ex = ctx.create_exchange();
        ex.message.body = Value::from("order");
        ex.message.set_header("type", "silver");
        ex.message.set_header("amount", 150);
        ex.message.set_header("tier", "B");
        let p_eq = SimplePredicate::parse("${header.type} == 'gold'")?;
        let p_gt = SimplePredicate::parse("${header.amount} > 100")?;
        let tmpl = SimpleExpression::parse("${body}-${header.tier}")?;
        time("cbr: simple == (no match)", n, || {
            std::hint::black_box(p_eq.matches(&ex).unwrap());
        });
        time("cbr: simple > (match)", n, || {
            std::hint::black_box(p_gt.matches(&ex).unwrap());
        });
        time("cbr: simple template ${body}-${header.tier}", n, || {
            std::hint::black_box(tmpl.evaluate(&ex).unwrap());
        });
        time("cbr: exchange + 2 headers + drop", n, || {
            let mut e = ctx.create_exchange();
            e.message.body = Value::from("order");
            e.message.set_header("type", Value::from("gold"));
            e.message.set_header("amount", Value::from(150));
            std::hint::black_box(&e);
        });
        time("cbr: set header (key + constant)", n, || {
            let mut h = Headers::new();
            h.set("tier", Value::from("A"));
            std::hint::black_box(&h);
        });
    }
    let order = crate::scenarios::order_json(1);
    time("template.send_body splitagg", n / 10, || {
        t.send_body("direct:splitagg", order.clone()).unwrap();
    });
    time("template.send_body split only (no aggregate)", n / 10, || {
        t.send_body("direct:split-only", order.clone()).unwrap();
    });
    let jp = ctx.resolve_language("jsonpath")?.create_expression("$.items", &Default::default())?;
    time("jsonpath $.items on order text", n / 10, || {
        let mut ex = ctx.create_exchange();
        ex.message.body = Value::from(order.clone());
        std::hint::black_box(jp.evaluate(&ex).unwrap());
    });
    time("json::parse order", n / 10, || {
        std::hint::black_box(cameleer_core::json::parse(&order).unwrap());
    });
    ctx.stop()
}

#[cfg(test)]
mod size_probe {
    #[test]
    fn print_sizes() {
        println!("SIZE Value={} ObjectMap={} Exchange={} Message={} Headers={} Properties={} CamelError={}",
            std::mem::size_of::<cameleer_core::value::Value>(),
            std::mem::size_of::<cameleer_core::value::ObjectMap>(),
            std::mem::size_of::<cameleer_core::message::Exchange>(),
            std::mem::size_of::<cameleer_core::message::Message>(),
            std::mem::size_of::<cameleer_core::message::Headers>(),
            std::mem::size_of::<cameleer_core::message::Properties>(),
            std::mem::size_of::<cameleer_core::error::CamelError>());
    }
}

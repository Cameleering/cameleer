//! The metric registry, the `micrometer`, `metrics` and `opentelemetry-metrics` components, the
//! built-in `camel.metrics.*` meters and their Prometheus text.

use std::collections::BTreeSet;
use std::time::Duration;

use cameleer_core::metrics::instrument::{self, Flavor, MetricsConfig, PolicyLevel};
use cameleer_core::metrics::{prometheus, MeterKind, MeterRegistry};
use cameleer_core::prelude::*;

fn tag(k: &str, v: &str) -> (String, String) {
    (k.to_string(), v.to_string())
}

fn counter(r: &MeterRegistry, name: &str, tags: &[(String, String)]) -> f64 {
    match r.find(name, tags).map(|m| m.kind) {
        Some(MeterKind::Counter(c)) => c.count(),
        o => panic!("no counter {name} {tags:?}: {o:?}\n{:#?}", r.meters().iter().map(|(id, _)| id.clone()).collect::<Vec<_>>()),
    }
}

#[test]
fn micrometer_counter_summary_and_timer() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:order")
            .to("micrometer:counter:orders.placed?tags.region=${header.region}&tags.channel=web&metricsDescription=Orders placed")
            .to("micrometer:counter:orders.value?increment=${body}")
            .to("micrometer:summary:order.amount?value=${body}")
            .to("micrometer:timer:order.processing?action=start")
            .process(|_ex: &mut Exchange| {
                std::thread::sleep(Duration::from_millis(15));
                Ok(())
            })
            .to("micrometer:timer:order.processing?action=stop")
            .to("micrometer:plain")
            .to("mock:done");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for (region, amount) in [("eu", 10), ("eu", 20), ("us", 5)] {
        t.send_body_and_header("direct:order", amount, "region", region).unwrap();
    }
    let r = MeterRegistry::existing(&ctx).unwrap();
    let c = ctx.name().to_string();
    assert_eq!(counter(&r, "orders.placed", &[tag("region", "eu"), tag("channel", "web"), tag("camelContext", &c)]), 2.0);
    assert_eq!(counter(&r, "orders.placed", &[tag("region", "us"), tag("channel", "web"), tag("camelContext", &c)]), 1.0);
    assert_eq!(r.find("orders.placed", &[tag("region", "us"), tag("channel", "web"), tag("camelContext", &c)]).unwrap().description.as_deref(), Some("Orders placed"));
    assert_eq!(counter(&r, "orders.value", &[tag("camelContext", &c)]), 35.0);
    assert_eq!(counter(&r, "plain", &[tag("camelContext", &c)]), 3.0);
    match r.find("order.amount", &[tag("camelContext", &c)]).unwrap().kind {
        MeterKind::Summary(d) => assert_eq!((d.count(), d.total(), d.max()), (3, 35.0, 20.0)),
        o => panic!("{o:?}"),
    }
    match r.find("order.processing", &[tag("camelContext", &c)]).unwrap().kind {
        MeterKind::Timer(t) => {
            assert_eq!(t.0.count(), 3);
            assert!(t.0.total() >= 0.045, "{}", t.0.total());
        }
        o => panic!("{o:?}"),
    }
    // the timer's start is gone from the exchange, and so are the CamelMetrics* headers
    let got = ctx.mock_endpoint("mock:done").unwrap().received_exchanges();
    assert!(got[0].property("timer:order.processing").is_none());

    // headers override the options and are removed afterwards
    let mut ex = Exchange::with_body(Some(ctx.clone()), "x");
    ex.set_header("CamelMetricsName", "renamed");
    ex.set_header("CamelMetricsCounterIncrement", 4);
    ex.set_header("CamelMetricsTags", "source=header");
    let done = t.send("micrometer:counter:original", ex);
    assert!(done.exception.is_none());
    assert!(done.header("CamelMetricsName").is_none() && done.header("CamelMetricsTags").is_none());
    assert_eq!(counter(&r, "renamed", &[tag("source", "header"), tag("camelContext", &c)]), 4.0);

    // a value that is not a number fails the exchange; an unknown type fails the endpoint
    let ex = t.send("micrometer:summary:bad?value=${body}", Exchange::with_body(Some(ctx.clone()), "many"));
    assert_eq!(ex.exception.unwrap().kind, "TypeConversionException");
    assert!(ctx.get_endpoint("micrometer:gauge:x").is_err());
    ctx.stop().unwrap();
}

#[test]
fn dropwizard_metrics_component() {
    let ctx = CamelContext::new();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("metrics:counter:jobs?increment=5", "x").unwrap();
    t.send_body("metrics:counter:jobs?decrement=2", "x").unwrap();
    t.send_body("metrics:meter:hits", "x").unwrap();
    t.send_body("metrics:hits?mark=3", "x").unwrap();
    t.send_body("metrics:histogram:sizes?value=42", "x").unwrap();
    t.send_body_and_header("metrics:gauge:depth", "x", "CamelMetricsGaugeSubject", 7).unwrap();
    t.send_body_and_header("metrics:timer:job", "x", "CamelMetricsTimerAction", "start").unwrap();
    let r = MeterRegistry::existing(&ctx).unwrap();
    match r.find("jobs", &[]).unwrap().kind {
        MeterKind::UpDownCounter(g) => assert_eq!(g.value(), 3.0),
        o => panic!("{o:?}"),
    }
    match r.find("hits", &[]).unwrap().kind {
        MeterKind::Meter(m) => assert_eq!(m.count(), 4),
        o => panic!("{o:?}"),
    }
    match r.find("depth", &[]).unwrap().kind {
        MeterKind::Gauge(g) => assert_eq!(g.value(), 7.0),
        o => panic!("{o:?}"),
    }
    assert!(ctx.get_endpoint("metrics:bogus:x").is_err());
    let text = prometheus::render(&r, &BTreeSet::new());
    assert!(text.contains("jobs 3.0\n") && text.contains("hits_total 4.0\n") && text.contains("sizes_count 1.0\n"), "{text}");
    ctx.stop().unwrap();
}

#[test]
fn opentelemetry_metrics_component() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:o")
            .to("opentelemetry-metrics:COUNTER:jobs.queued?attributes.queue=${header.q}")
            .to("opentelemetry-metrics:counter:jobs.queued?decrement=1&attributes.queue=${header.q}")
            .to("opentelemetry-metrics:summary:job.size?value=${body}")
            .to("opentelemetry-metrics:timer:job.time?action=start")
            .to("opentelemetry-metrics:timer:job.time?action=stop&unit=MICROSECONDS");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body_and_header("direct:o", 12, "q", "fast").unwrap();
    let r = MeterRegistry::existing(&ctx).unwrap();
    let c = ctx.name().to_string();
    match r.find("jobs.queued", &[tag("queue", "fast"), tag("camelContext", &c)]).unwrap().kind {
        MeterKind::UpDownCounter(g) => assert_eq!(g.value(), 0.0),
        o => panic!("{o:?}"),
    }
    let h = r.find("job.time", &[tag("camelContext", &c)]).unwrap();
    assert_eq!(h.unit, "microseconds");
    match h.kind {
        MeterKind::Summary(d) => assert_eq!((d.count(), d.bounds()[1]), (1, 5.0)),
        o => panic!("{o:?}"),
    }
    ctx.stop().unwrap();
}

#[test]
fn built_in_meters_with_camels_names_and_tags() {
    let ctx = CamelContext::new();
    let r = instrument::install(&ctx, MetricsConfig { message_history: true, ..Default::default() });
    ctx.add_routes(|b| {
        b.from("direct:a").route_id("a").to("direct:b").id("toB");
        b.from("direct:b").route_id("b").filter(simple("${body} == 'boom'")).throw_exception("IllegalArgumentException", "boom").end().to("mock:b?retainFirst=1");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:a", "ok").unwrap();
    t.send_body("direct:a", "ok").unwrap();
    assert!(t.send_body("direct:a", "boom").is_err());
    let c = ctx.name().to_string();
    let route = |id: &str| vec![tag("camelContext", &c), tag("kind", "CamelRoute"), tag("routeId", id), tag("eventType", "route")];
    assert_eq!(counter(&r, "camel.exchanges.total", &route("a")), 3.0);
    assert_eq!(counter(&r, "camel.exchanges.succeeded", &route("a")), 2.0);
    assert_eq!(counter(&r, "camel.exchanges.failed", &route("a")), 1.0);
    assert_eq!(counter(&r, "camel.exchanges.total", &route("b")), 3.0);
    // the context counts each exchange once, in the route it started in
    let ctx_tags = vec![tag("camelContext", &c), tag("kind", "CamelRoute"), tag("routeId", ""), tag("eventType", "context")];
    assert_eq!(counter(&r, "camel.exchanges.total", &ctx_tags), 3.0);
    match r.find("camel.route.policy", &route("a")).unwrap().kind {
        MeterKind::Timer(t) => assert_eq!(t.0.count(), 3),
        o => panic!("{o:?}"),
    }
    // the exchange event notifier: completed/failed at the route the exchange started in, and sends
    let ev = |event: &str, failed: &str| vec![tag("camelContext", &c), tag("kind", "CamelExchangeEvent"), tag("eventType", event), tag("routeId", "a"), tag("endpointName", "direct://a"), tag("failed", failed)];
    let timer_count = |name: &str, tags: &[(String, String)]| match r.find(name, tags).map(|m| m.kind) {
        Some(MeterKind::Timer(t)) => t.0.count(),
        o => panic!("{name} {tags:?}: {o:?}"),
    };
    assert_eq!(timer_count("camel.exchange.event.notifier", &ev("ExchangeCompletedEvent", "false")), 2);
    assert_eq!(timer_count("camel.exchange.event.notifier", &ev("ExchangeFailedEvent", "true")), 1);
    let sent = vec![tag("camelContext", &c), tag("kind", "CamelExchangeEvent"), tag("eventType", "ExchangeSentEvent"), tag("routeId", "a"), tag("endpointName", "mock://b"), tag("failed", "false")];
    assert_eq!(timer_count("camel.exchange.event.notifier", &sent), 2);
    // message history: a timer per node
    assert_eq!(timer_count("camel.message.history", &[tag("camelContext", &c), tag("kind", "CamelMessageHistory"), tag("routeId", "a"), tag("nodeId", "toB")]), 3);

    // the Prometheus text: Camel's names, and the registry's families replace management's
    let text = prometheus::merge(&ctx.managed_context().prometheus(), &r);
    assert!(text.contains(&format!("camel_exchanges_total{{camelContext=\"{c}\",eventType=\"route\",kind=\"CamelRoute\",routeId=\"a\"}} 3.0")), "{text}");
    assert!(text.contains("# TYPE camel_route_policy_seconds summary"));
    assert!(text.contains("# TYPE camel_exchange_event_notifier_seconds summary"));
    assert!(text.contains(&format!("camel_routes_added_routes{{camelContext=\"{c}\",eventType=\"RouteEvent\",kind=\"CamelRoute\"}} 2.0")), "{text}");
    assert!(text.contains("camel_exchanges_last_timestamp "));
    assert!(text.contains("app_info{camel_context="));
    assert!(text.contains(&format!("camel_exchanges_inflight{{camelContext=\"{c}\",kind=\"CamelExchangeEvent\",routeId=\"a\"}} 0.0")));
    // each family has one TYPE line
    let types: Vec<&str> = text.lines().filter(|l| l.starts_with("# TYPE ")).collect();
    assert_eq!(types.len(), types.iter().collect::<BTreeSet<_>>().len(), "{text}");
    ctx.stop().unwrap();
}

#[test]
fn legacy_names_route_level_and_opentelemetry_flavor() {
    let ctx = CamelContext::new();
    let r = instrument::install(&ctx, MetricsConfig { legacy_names: true, route_policy_level: PolicyLevel::Route, exchange_event_notifier: false, route_event_notifier: false, skip_camel_info: true, ..Default::default() });
    ctx.add_routes(|b| {
        b.from("direct:l").route_id("l").to("mock:l");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:l", "x").unwrap();
    let names: BTreeSet<String> = r.meters().into_iter().map(|(id, _)| id.name).collect();
    assert_eq!(names, ["CamelExchangesExternalRedeliveries", "CamelExchangesFailed", "CamelExchangesFailuresHandled", "CamelExchangesSucceeded", "CamelExchangesTotal", "CamelRoutePolicy"].iter().map(|s| s.to_string()).collect());
    assert!(r.meters().iter().all(|(id, _)| id.tag("eventType") == Some("route")));
    ctx.stop().unwrap();

    let ctx = CamelContext::new();
    let r = instrument::install(&ctx, MetricsConfig { flavor: Flavor::OpenTelemetry, ..Default::default() });
    ctx.add_routes(|b| {
        b.from("direct:o").route_id("o").to("mock:o");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:o", "x").unwrap();
    let c = ctx.name().to_string();
    let h = r.find("camel.route.policy", &[tag("camelContext", &c), tag("kind", "CamelRoute"), tag("routeId", "o"), tag("eventType", "route")]).unwrap();
    assert_eq!(h.unit, "milliseconds");
    assert!(matches!(h.kind, MeterKind::Summary(_)));
    assert!(!r.find_all("camel.exchange.elapsed").is_empty());
    assert!(!r.find_all("camel.exchange.sent").is_empty());
    assert!(!r.find_all("camel.exchanges.last.time").is_empty());
    ctx.stop().unwrap();
}

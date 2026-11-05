//! Conformance tests for EIPs used in combination, following the Apache Camel 4 EIP docs.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cameleer_core::prelude::*;
use cameleer_core::processor::routing::strategies::grouped_body;

/// Camel docs "Splitter with AggregationStrategy" example strategy: concatenate with '+'.
struct ConcatStrategy;
impl AggregationStrategy for ConcatStrategy {
    fn aggregate(&self, old: Option<Exchange>, new: Exchange) -> Result<Exchange> {
        match old {
            None => Ok(new),
            Some(mut old) => {
                let b = format!("{}+{}", old.body(), new.body());
                old.set_body(b);
                Ok(old)
            }
        }
    }
}

fn bodies(ctx: &CamelContext, mock: &str) -> Vec<String> {
    ctx.mock_endpoint(mock).unwrap().received_bodies().iter().map(|v| v.to_string()).collect()
}

#[test]
fn splitter_with_aggregation_strategy_reply_is_aggregate() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .split(body().tokenize("@"))
            .aggregation_strategy(ConcatStrategy)
            .set_body(simple("${body.toUpperCase()}"))
            .to("mock:split")
            .end()
            .to("mock:result");
    })
    .unwrap();
    ctx.start().unwrap();
    let reply = ctx.create_producer_template().request_body("direct:start", "a@b@c").unwrap();
    assert_eq!(reply, Value::from("A+B+C"));
    assert_eq!(bodies(&ctx, "mock:split"), vec!["A", "B", "C"]);
    assert_eq!(bodies(&ctx, "mock:result"), vec!["A+B+C"]);
}

#[test]
fn split_without_strategy_keeps_original_body() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").split(body()).set_body(constant("changed")).end().to("mock:result");
    })
    .unwrap();
    ctx.start().unwrap();
    assert_eq!(ctx.create_producer_template().request_body("direct:start", "a,b").unwrap(), Value::from("a,b"));
}

#[test]
fn split_inside_choice() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .choice()
            .when(header("type").is_equal_to("batch"))
            .split(body())
            .to("mock:item")
            .end()
            .to("mock:batch")
            .otherwise()
            .to("mock:single")
            .end()
            .to("mock:after");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body_and_header("direct:start", "a,b,c", "type", "batch").unwrap();
    t.send_body_and_header("direct:start", "x", "type", "single").unwrap();
    assert_eq!(bodies(&ctx, "mock:item"), vec!["a", "b", "c"]);
    assert_eq!(bodies(&ctx, "mock:batch"), vec!["a,b,c"]);
    assert_eq!(bodies(&ctx, "mock:single"), vec!["x"]);
    assert_eq!(bodies(&ctx, "mock:after"), vec!["a,b,c", "x"]);
}

#[test]
fn nested_split_indexes_are_per_level() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .split(body().tokenize("\n"))
            .set_header("outer", simple("${exchangeProperty.CamelSplitIndex}"))
            .split(body().tokenize(","))
            .set_body(simple("${header.outer}.${exchangeProperty.CamelSplitIndex}=${body}"))
            .to("mock:inner")
            .end()
            .to("mock:outer")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "a,b\nc").unwrap();
    assert_eq!(bodies(&ctx, "mock:inner"), vec!["0.0=a", "0.1=b", "1.0=c"]);
    assert_eq!(bodies(&ctx, "mock:outer"), vec!["a,b", "c"]);
    let outer = ctx.mock_endpoint("mock:outer").unwrap().received_exchanges();
    assert_eq!(outer[1].property(props::SPLIT_INDEX), Some(&Value::Int(1)), "outer split properties restored after inner split");
}

#[test]
fn multicast_parallel_with_aggregation_strategy() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .multicast()
            .parallel_processing()
            .aggregation_strategy(grouped_body())
            .to("direct:a")
            .to("direct:b")
            .to("direct:c")
            .end()
            .to("mock:result");
        b.from("direct:a").delay(constant(60)).set_body(simple("A${body}"));
        b.from("direct:b").delay(constant(30)).set_body(simple("B${body}"));
        b.from("direct:c").set_body(simple("C${body}"));
    })
    .unwrap();
    ctx.start().unwrap();
    let start = std::time::Instant::now();
    let reply = ctx.create_producer_template().request_body("direct:start", "1").unwrap();
    assert!(start.elapsed() < Duration::from_millis(150), "parallel: {:?}", start.elapsed());
    // aggregated in the order the outputs were declared
    assert_eq!(reply, Value::from(vec!["A1", "B1", "C1"]));
}

#[test]
fn multicast_each_output_receives_copy_of_original() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").multicast().to("direct:a").to("mock:b").end().to("mock:after");
        b.from("direct:a").set_body(constant("changed"));
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "orig").unwrap();
    assert_eq!(bodies(&ctx, "mock:b"), vec!["orig"]);
    // Camel default for multicast without strategy: UseLatestAggregationStrategy
    assert_eq!(bodies(&ctx, "mock:after"), vec!["orig"]);
    let b = ctx.mock_endpoint("mock:b").unwrap().exchange(0).unwrap();
    assert_eq!(b.property(props::MULTICAST_INDEX), Some(&Value::Int(1)));
}

#[test]
fn recipient_list_from_header_with_delimiter() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").recipient_list(header("recipients")).end();
        b.from("direct:semi").recipient_list(header("recipients")).delimiter(";").end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body_and_header("direct:start", "x", "recipients", "mock:a,mock:b").unwrap();
    t.send_body_and_header("direct:semi", "y", "recipients", "mock:b;mock:c").unwrap();
    t.send_body_and_header("direct:start", "z", "recipients", Value::from(vec!["mock:c"])).unwrap();
    assert_eq!(bodies(&ctx, "mock:a"), vec!["x"]);
    assert_eq!(bodies(&ctx, "mock:b"), vec!["x", "y"]);
    assert_eq!(bodies(&ctx, "mock:c"), vec!["y", "z"]);
    let a = ctx.mock_endpoint("mock:a").unwrap().exchange(0).unwrap();
    // Camel normalizes the uri (URISupport.normalizeUri)
    assert_eq!(a.property(props::RECIPIENT_LIST_ENDPOINT), Some(&Value::from("mock://a")));
    assert_eq!(a.property(props::TO_ENDPOINT), Some(&Value::from("mock://a")));
}

#[test]
fn endpoint_uris_are_normalized_like_camel() {
    let ctx = CamelContext::new();
    // the same endpoint whatever form is used to look it up
    let a = ctx.get_endpoint("seda:foo?size=10&blockWhenFull=true").unwrap();
    assert_eq!(a.uri(), "seda://foo?blockWhenFull=true&size=10");
    let b = ctx.get_endpoint("seda://foo?blockWhenFull=true&size=10").unwrap();
    assert!(std::sync::Arc::ptr_eq(&a, &b));
    assert_eq!(ctx.get_endpoint("mock:result").unwrap().uri(), "mock://result");
    assert_eq!(ctx.get_endpoint("direct://start").unwrap().uri(), "direct://start");
    assert_eq!(ctx.normalize_endpoint_uri("log:foo?showAll=true&level=WARN"), "log://foo?level=WARN&showAll=true");
    // values are encoded like java.net.URLEncoder
    assert_eq!(ctx.normalize_endpoint_uri("log:a?marker=hello world"), "log://a?marker=hello+world");
    // invalid parameters are reported with the normalized uri
    let e = ctx.get_endpoint("seda:x?nope=1").err().unwrap();
    assert!(e.message.contains("seda://x?nope=1"), "{}", e);

    ctx.add_routes(|b| {
        b.from("direct:start").route_id("main").to("mock:a?retainFirst=5").to_d("mock:${header.next}").to("mock:end");
        b.from("direct:slip").routing_slip(header("slip")).end();
        b.from("direct:fail")
            .error_handler(dead_letter_channel("mock:dead"))
            .to("log:before?showAll=true&level=DEBUG")
            .throw_exception("IOException", "boom");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body_and_header("direct:start", "x", "next", "b").unwrap();
    let a = ctx.mock_endpoint("mock:a?retainFirst=5").unwrap();
    assert_eq!(a.uri(), "mock://a?retainFirst=5");
    assert_eq!(a.exchange(0).unwrap().property(props::TO_ENDPOINT), Some(&Value::from("mock://a?retainFirst=5")));
    let b = ctx.mock_endpoint("mock:b").unwrap();
    assert_eq!(b.exchange(0).unwrap().property(props::TO_ENDPOINT), Some(&Value::from("mock://b")));
    let end = ctx.mock_endpoint("mock://end").unwrap();
    assert_eq!(end.received_counter(), 1);
    assert_eq!(ctx.route("main").unwrap().endpoint_uri, "direct://start");

    t.send_body_and_header("direct:slip", "y", "slip", "mock:s1,mock://s2").unwrap();
    let s2 = ctx.mock_endpoint("s2").unwrap().exchange(0).unwrap();
    assert_eq!(s2.property(props::SLIP_ENDPOINT), Some(&Value::from("mock://s2")));

    t.send_body("direct:fail", "z").unwrap();
    let d = ctx.mock_endpoint("dead").unwrap().exchange(0).unwrap();
    assert_eq!(d.property(props::FAILURE_ENDPOINT), Some(&Value::from("log://before?level=DEBUG&showAll=true")));
}

#[test]
fn recipient_list_ignore_invalid_endpoints() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:ignore").recipient_list(header("r")).ignore_invalid_endpoints().end();
        b.from("direct:strict").recipient_list(header("r")).end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body_and_header("direct:ignore", "x", "r", "mock:a,xxx:bad,mock:b").unwrap();
    assert_eq!(bodies(&ctx, "mock:a"), vec!["x"]);
    assert_eq!(bodies(&ctx, "mock:b"), vec!["x"]);
    let e = t.send_body_and_header("direct:strict", "y", "r", "mock:a,xxx:bad").unwrap_err();
    let cause = e.cause.unwrap();
    assert!(cause.is_a("ResolveEndpointFailedException") || cause.is_a("NoSuchEndpointException"), "{}", cause);
}

#[test]
fn recipient_list_empty_header_is_noop() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").recipient_list(header("r")).end().to("mock:after");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "x").unwrap();
    assert_eq!(bodies(&ctx, "mock:after"), vec!["x"]);
}

#[test]
fn aggregate_completion_size_and_timeout_on_same_route() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .aggregate(header("id"), ConcatStrategy)
            .completion_size(3)
            .completion_timeout(Duration::from_millis(200))
            .to("mock:aggregated")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body_and_header("direct:start", "A", "id", 1).unwrap();
    t.send_body_and_header("direct:start", "X", "id", 2).unwrap();
    t.send_body_and_header("direct:start", "B", "id", 1).unwrap();
    t.send_body_and_header("direct:start", "C", "id", 1).unwrap();
    let m = ctx.mock_endpoint("mock:aggregated").unwrap();
    assert!(m.wait_for_messages(2, Duration::from_secs(5)));
    let ex = m.received_exchanges();
    assert_eq!(ex[0].body(), &Value::from("A+B+C"));
    assert_eq!(ex[0].property(props::AGGREGATED_COMPLETED_BY), Some(&Value::from("size")));
    assert_eq!(ex[0].property(props::AGGREGATED_SIZE), Some(&Value::Int(3)));
    assert_eq!(ex[1].body(), &Value::from("X"));
    assert_eq!(ex[1].property(props::AGGREGATED_COMPLETED_BY), Some(&Value::from("timeout")));
    assert_eq!(ex[1].property(props::AGGREGATED_CORRELATION_KEY).map(|v| v.to_string()), Some("2".to_string()));
    ctx.stop().unwrap();
}

#[test]
fn aggregate_missing_correlation_key_fails() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").aggregate(header("id"), ConcatStrategy).completion_size(2).to("mock:agg").end();
    })
    .unwrap();
    ctx.start().unwrap();
    let e = ctx.create_producer_template().send_body("direct:start", "no key").unwrap_err();
    assert!(e.cause.unwrap().is_a("CamelExchangeException"));
}

#[test]
fn wire_tap_does_not_affect_original() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").wire_tap("direct:tap").set_header("after", simple("${body}")).to("mock:result");
        b.from("direct:tap").set_body(constant("tapped")).set_header("tapHeader", constant(1)).to("mock:tap");
    })
    .unwrap();
    ctx.start().unwrap();
    let reply = ctx.create_producer_template().request_body("direct:start", "orig").unwrap();
    assert_eq!(reply, Value::from("orig"));
    let tap = ctx.mock_endpoint("mock:tap").unwrap();
    tap.expected_bodies_received(vec!["tapped"]);
    tap.assert_is_satisfied_within(Duration::from_secs(5)).unwrap();
    let r = ctx.mock_endpoint("mock:result").unwrap().exchange(0).unwrap();
    assert!(r.header("tapHeader").is_none());
    assert_eq!(r.header("after"), Some(&Value::from("orig")));
    // the tapped exchange is a different exchange (new id)
    assert_ne!(tap.exchange(0).unwrap().exchange_id, r.exchange_id);
    ctx.stop().unwrap();
}

#[test]
fn enrich_with_aggregation_strategy() {
    let ctx = CamelContext::new();
    struct Merge;
    impl AggregationStrategy for Merge {
        fn aggregate(&self, old: Option<Exchange>, new: Exchange) -> Result<Exchange> {
            let mut old = old.expect("enrich always passes the original");
            let b = format!("{} + {}", old.body(), new.body());
            old.set_body(b);
            Ok(old)
        }
    }
    ctx.add_routes(|b| {
        b.from("direct:start").enrich_with("direct:resource", Merge).to("mock:result");
        b.from("direct:resource").set_body(simple("resource for ${body}"));
        b.from("direct:plain").enrich("direct:resource").to("mock:plain");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    assert_eq!(t.request_body("direct:start", "order").unwrap(), Value::from("order + resource for order"));
    // default strategy: replace the body with the resource reply
    assert_eq!(t.request_body("direct:plain", "x").unwrap(), Value::from("resource for x"));
}

#[test]
fn loop_with_and_without_copy() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:nocopy").loop_(constant(3)).transform(simple("${body}B")).to("mock:nocopy").end().to("mock:nocopy-after");
        b.from("direct:copy").loop_(constant(3)).copy(true).transform(simple("${body}B")).to("mock:copy").end().to("mock:copy-after");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:nocopy", "A").unwrap();
    t.send_body("direct:copy", "A").unwrap();
    assert_eq!(bodies(&ctx, "mock:nocopy"), vec!["AB", "ABB", "ABBB"]);
    assert_eq!(bodies(&ctx, "mock:nocopy-after"), vec!["ABBB"]);
    assert_eq!(bodies(&ctx, "mock:copy"), vec!["AB", "AB", "AB"]);
    assert_eq!(bodies(&ctx, "mock:copy-after"), vec!["AB"]);
    let ex = ctx.mock_endpoint("mock:copy").unwrap().received_exchanges();
    assert_eq!(ex[2].property(props::LOOP_INDEX), Some(&Value::Int(2)));
}

#[test]
fn loop_count_from_header() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").loop_(header("n")).to("mock:loop").end();
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body_and_header("direct:start", "x", "n", "4").unwrap();
    assert_eq!(ctx.mock_endpoint("mock:loop").unwrap().received_counter(), 4);
}

#[test]
fn idempotent_consumer_with_skip_duplicate_false_and_filter() {
    // Camel docs: route duplicates to a separate endpoint using CamelDuplicateMessage
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .idempotent_consumer(header("messageId"), memory_idempotent_repository(200))
            .skip_duplicate(false)
            .filter(exchange_property(props::DUPLICATE_MESSAGE).is_equal_to(true))
            .to("mock:duplicate")
            .stop()
            .end()
            .to("mock:result")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for (id, body) in [(1, "one"), (2, "two"), (1, "one again"), (3, "three"), (2, "two again")] {
        t.send_body_and_header("direct:start", body, "messageId", id).unwrap();
    }
    assert_eq!(bodies(&ctx, "mock:result"), vec!["one", "two", "three"]);
    assert_eq!(bodies(&ctx, "mock:duplicate"), vec!["one again", "two again"]);
}

#[test]
fn idempotent_consumer_skips_duplicates_by_default() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").idempotent_consumer(header("messageId"), memory_idempotent_repository(200)).to("mock:result").end().to("mock:after");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for id in [1, 2, 1, 1, 3] {
        t.send_body_and_header("direct:start", id, "messageId", id).unwrap();
    }
    assert_eq!(bodies(&ctx, "mock:result"), vec!["1", "2", "3"]);
    // Camel: a skipped duplicate only skips the idempotent block, routing continues after it
    let after = ctx.mock_endpoint("mock:after").unwrap().received_exchanges();
    assert_eq!(after.len(), 5);
    assert_eq!(after[2].property(props::DUPLICATE_MESSAGE), Some(&Value::Bool(true)));
}

#[test]
fn do_try_inside_split() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .split(body())
            .do_try()
            .process(|ex: &mut Exchange| {
                if ex.body() == &Value::from("bad") {
                    return Err(CamelError::illegal_argument("bad item"));
                }
                Ok(())
            })
            .to("mock:ok")
            .do_catch(&["IllegalArgumentException"])
            .set_body(simple("caught ${exception.message}"))
            .to("mock:caught")
            .end_do_try()
            .to("mock:each")
            .end()
            .to("mock:after");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "a,bad,c").unwrap();
    assert_eq!(bodies(&ctx, "mock:ok"), vec!["a", "c"]);
    assert_eq!(bodies(&ctx, "mock:caught"), vec!["caught bad item"]);
    assert_eq!(bodies(&ctx, "mock:each"), vec!["a", "caught bad item", "c"]);
    assert_eq!(bodies(&ctx, "mock:after"), vec!["a,bad,c"]);
}

#[test]
fn stop_inside_split_only_stops_the_sub_exchange() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .split(body())
            .filter(body().is_equal_to("b"))
            .stop()
            .end()
            .to("mock:part")
            .end()
            .to("mock:after");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "a,b,c").unwrap();
    assert_eq!(bodies(&ctx, "mock:part"), vec!["a", "c"]);
    assert_eq!(bodies(&ctx, "mock:after"), vec!["a,b,c"]);
}

#[test]
fn stop_inside_split_with_aggregation_strategy_does_not_stop_parent() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .split(body())
            .aggregation_strategy(ConcatStrategy)
            .filter(body().is_equal_to("c"))
            .stop()
            .end()
            .to("mock:part")
            .end()
            .to("mock:after");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "a,b,c").unwrap();
    assert_eq!(bodies(&ctx, "mock:part"), vec!["a", "b"]);
    assert_eq!(bodies(&ctx, "mock:after"), vec!["a+b+c"]);
}

#[test]
fn stop_inside_multicast_branch_does_not_stop_parent() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").multicast().to("direct:x").to("mock:b").end().to("mock:after");
        b.from("direct:x").stop();
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "m").unwrap();
    assert_eq!(bodies(&ctx, "mock:b"), vec!["m"]);
    assert_eq!(bodies(&ctx, "mock:after"), vec!["m"]);
}

#[test]
fn stop_inside_direct_sub_route_stops_whole_exchange() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").to("direct:sub").to("mock:after");
        b.from("direct:sub").to("mock:sub").stop().to("mock:never");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "x").unwrap();
    assert_eq!(bodies(&ctx, "mock:sub"), vec!["x"]);
    assert_eq!(ctx.mock_endpoint("mock:never").unwrap().received_counter(), 0);
    assert_eq!(ctx.mock_endpoint("mock:after").unwrap().received_counter(), 0);
}

#[test]
fn filter_sets_filter_matched_property() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").filter(simple("${body} == 'yes'")).to("mock:inside").end().to("mock:after");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:start", "yes").unwrap();
    t.send_body("direct:start", "no").unwrap();
    assert_eq!(bodies(&ctx, "mock:inside"), vec!["yes"]);
    let after = ctx.mock_endpoint("mock:after").unwrap().received_exchanges();
    assert_eq!(after.len(), 2, "non matching exchanges continue after the filter");
    assert_eq!(after[0].property(props::FILTER_MATCHED), Some(&Value::Bool(true)));
    assert_eq!(after[1].property(props::FILTER_MATCHED), Some(&Value::Bool(false)));
}

#[test]
fn choice_without_otherwise_continues_after_choice() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").choice().when(body().is_equal_to("a")).to("mock:a").when(body().is_equal_to("b")).to("mock:b").end().to("mock:after");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for x in ["a", "b", "c"] {
        t.send_body("direct:start", x).unwrap();
    }
    assert_eq!(bodies(&ctx, "mock:a"), vec!["a"]);
    assert_eq!(bodies(&ctx, "mock:b"), vec!["b"]);
    assert_eq!(bodies(&ctx, "mock:after"), vec!["a", "b", "c"]);
}

#[test]
fn choice_first_matching_when_wins() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .choice()
            .when(simple("${body} > 10"))
            .to("mock:big")
            .when(simple("${body} > 5"))
            .to("mock:medium")
            .otherwise()
            .to("mock:small")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for x in [20, 7, 1] {
        t.send_body("direct:start", x).unwrap();
    }
    assert_eq!(bodies(&ctx, "mock:big"), vec!["20"]);
    assert_eq!(bodies(&ctx, "mock:medium"), vec!["7"]);
    assert_eq!(bodies(&ctx, "mock:small"), vec!["1"]);
}

#[test]
fn routing_slip_from_header() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").routing_slip(header("slip")).end();
        b.from("direct:x").set_body(simple("${body}x"));
        b.from("direct:y").set_body(simple("${body}y"));
    })
    .unwrap();
    ctx.start().unwrap();
    let r = ctx.create_producer_template().request_body_and_header("direct:start", "", "slip", "direct:x,direct:y,direct:x").unwrap();
    // routing slip pipes: the output of one step is the input of the next
    assert_eq!(r, Value::from("xyx"));
}

#[test]
fn dynamic_router_until_null() {
    let ctx = CamelContext::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let c2 = calls.clone();
    ctx.add_routes(move |b| {
        let c3 = c2.clone();
        b.from("direct:start")
            .dynamic_router(expr(move |ex: &Exchange| {
                c3.fetch_add(1, Ordering::SeqCst);
                let invoked = ex.property("CamelSlipEndpoint").map(|v| v.to_string());
                Ok(match invoked.as_deref() {
                    None => Value::from("mock:a"),
                    Some("mock://a") | Some("mock:a") => Value::from("mock:b"),
                    _ => Value::Null,
                })
            }))
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "x").unwrap();
    assert_eq!(bodies(&ctx, "mock:a"), vec!["x"]);
    assert_eq!(bodies(&ctx, "mock:b"), vec!["x"]);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn split_parallel_aggregation_keeps_input_order() {
    let ctx = CamelContext::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    ctx.add_routes(move |b| {
        let s3 = s2.clone();
        b.from("direct:start")
            .split(body())
            .parallel_processing()
            .aggregation_strategy(ConcatStrategy)
            .process(move |ex: &mut Exchange| {
                let n: i64 = ex.body_as()?;
                std::thread::sleep(Duration::from_millis((40 - n * 10) as u64));
                s3.lock().unwrap().push(n);
                Ok(())
            })
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let r = ctx.create_producer_template().request_body("direct:start", "1,2,3").unwrap();
    assert_eq!(r, Value::from("1+2+3"));
    ctx.stop().unwrap();
}

#[test]
fn to_d_with_simple_uri() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").to_d("mock:${header.target}");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body_and_header("direct:start", "1", "target", "x").unwrap();
    t.send_body_and_header("direct:start", "2", "target", "y").unwrap();
    assert_eq!(bodies(&ctx, "mock:x"), vec!["1"]);
    assert_eq!(bodies(&ctx, "mock:y"), vec!["2"]);
}

#[test]
fn split_empty_body_produces_no_sub_exchanges() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").split(body()).to("mock:part").end().to("mock:after");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:start", Value::Array(vec![])).unwrap();
    t.send_body("direct:start", Value::Null).unwrap();
    assert_eq!(ctx.mock_endpoint("mock:part").unwrap().received_counter(), 0);
    assert_eq!(ctx.mock_endpoint("mock:after").unwrap().received_counter(), 2);
}

#[test]
fn incoming_exchange_continues_after_aggregate_block() {
    // Camel: the aggregate outputs only see aggregated exchanges; after end() the
    // incoming exchange continues routing
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").aggregate(constant(true), ConcatStrategy).completion_size(2).to("mock:aggregated").end().to("mock:result");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:start", "a").unwrap();
    t.send_body("direct:start", "b").unwrap();
    assert_eq!(bodies(&ctx, "mock:aggregated"), vec!["a+b"]);
    assert_eq!(bodies(&ctx, "mock:result"), vec!["a", "b"]);
}

#[test]
fn aggregate_completion_timeout_is_inactivity_timeout() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").aggregate(constant(1), ConcatStrategy).completion_timeout(Duration::from_millis(500)).to("mock:agg").end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for x in ["a", "b", "c", "d"] {
        t.send_body("direct:start", x).unwrap();
        std::thread::sleep(Duration::from_millis(40));
    }
    let m = ctx.mock_endpoint("mock:agg").unwrap();
    assert!(m.wait_for_messages(1, Duration::from_secs(5)));
    assert_eq!(bodies(&ctx, "mock:agg"), vec!["a+b+c+d"], "each new message resets the completion timeout");
    ctx.stop().unwrap();
}

#[test]
fn aggregate_completion_predicate_on_aggregated_exchange() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .aggregate(header("id"), ConcatStrategy)
            .completion_predicate(simple("${body} contains 'END'"))
            .to("mock:agg")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for x in ["a", "b", "END"] {
        t.send_body_and_header("direct:start", x, "id", 1).unwrap();
    }
    assert_eq!(bodies(&ctx, "mock:agg"), vec!["a+b+END"]);
    let ex = ctx.mock_endpoint("mock:agg").unwrap().exchange(0).unwrap();
    assert_eq!(ex.property(props::AGGREGATED_COMPLETED_BY), Some(&Value::from("predicate")));
}

#[test]
fn aggregate_force_completion_on_stop() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").aggregate(constant(1), ConcatStrategy).completion_size(10).force_completion_on_stop().to("mock:agg").end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:start", "a").unwrap();
    t.send_body("direct:start", "b").unwrap();
    ctx.stop().unwrap();
    assert_eq!(bodies(&ctx, "mock:agg"), vec!["a+b"]);
    let ex = ctx.mock_endpoint("mock:agg").unwrap().exchange(0).unwrap();
    assert_eq!(ex.property(props::AGGREGATED_COMPLETED_BY), Some(&Value::from("force")));
}

#[test]
fn multicast_and_recipient_list_default_to_last_reply() {
    // Camel docs: "By default Camel will use the last reply as the outgoing message"
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:multicast").multicast().to("direct:a").to("direct:b").end().to("mock:multicast");
        b.from("direct:rl").recipient_list(constant("direct:a,direct:b")).end().to("mock:rl");
        b.from("direct:a").transform(constant("A"));
        b.from("direct:b").transform(constant("B"));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    assert_eq!(t.request_body("direct:multicast", "x").unwrap(), Value::from("B"));
    assert_eq!(t.request_body("direct:rl", "x").unwrap(), Value::from("B"));
}

#[test]
fn loop_do_while() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").loop_do_while(simple("${body.length()} <= 5")).transform(simple("${body}A")).to("mock:loop").end();
    })
    .unwrap();
    ctx.start().unwrap();
    assert_eq!(ctx.create_producer_template().request_body("direct:start", "AA").unwrap(), Value::from("AAAAAA"));
    assert_eq!(ctx.mock_endpoint("mock:loop").unwrap().received_counter(), 4);
}

#[test]
fn wire_tap_with_new_exchange_body() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").wire_tap("mock:tap").new_exchange_body(simple("Bye ${body}")).to("mock:result");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "World").unwrap();
    let tap = ctx.mock_endpoint("mock:tap").unwrap();
    assert!(tap.wait_for_messages(1, Duration::from_secs(5)));
    assert_eq!(bodies(&ctx, "mock:tap"), vec!["Bye World"]);
    assert_eq!(bodies(&ctx, "mock:result"), vec!["World"]);
    ctx.stop().unwrap();
}

#[test]
fn poll_enrich_from_seda_with_timeout() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").poll_enrich("seda:data", 100).to("mock:result");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("seda:data", "polled").unwrap();
    assert_eq!(t.request_body("direct:start", "x").unwrap(), Value::from("polled"));
    // nothing to poll: Camel sets an empty (null) body after the timeout
    assert_eq!(t.request_body("direct:start", "y").unwrap(), Value::Null);
    ctx.stop().unwrap();
}

#[test]
fn intercept_send_to_endpoint_skip() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.intercept_send_to_endpoint("mock:external*").skip_send_to_original_endpoint().transform(constant("intercepted")).to("mock:detour");
        b.from("direct:start").to("mock:externalService").to("mock:result");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "x").unwrap();
    assert_eq!(ctx.mock_endpoint("mock:externalService").unwrap().received_counter(), 0);
    assert_eq!(bodies(&ctx, "mock:detour"), vec!["intercepted"]);
    assert_eq!(bodies(&ctx, "mock:result"), vec!["intercepted"]);
}

#[test]
fn nested_choice_with_end_choice() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .choice()
            .when(header("a").is_equal_to(1))
            .choice()
            .when(header("b").is_equal_to(1))
            .to("mock:a1b1")
            .otherwise()
            .to("mock:a1")
            .end_choice()
            .end()
            .otherwise()
            .to("mock:other")
            .end()
            .to("mock:after");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body_and_headers("direct:start", "1", vec![("a", Value::Int(1)), ("b", Value::Int(1))]).unwrap();
    t.send_body_and_headers("direct:start", "2", vec![("a", Value::Int(1)), ("b", Value::Int(2))]).unwrap();
    t.send_body_and_headers("direct:start", "3", vec![("a", Value::Int(2))]).unwrap();
    assert_eq!(bodies(&ctx, "mock:a1b1"), vec!["1"]);
    assert_eq!(bodies(&ctx, "mock:a1"), vec!["2"]);
    assert_eq!(bodies(&ctx, "mock:other"), vec!["3"]);
    assert_eq!(bodies(&ctx, "mock:after"), vec!["1", "2", "3"]);
}

#[test]
fn split_sub_exchanges_see_parent_headers_but_changes_do_not_leak() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .set_header("parent", constant("p"))
            .set_property("parentProp", constant("pp"))
            .split(body())
            .set_body(simple("${body}-${header.parent}-${exchangeProperty.parentProp}"))
            .set_header("child", constant("c"))
            .set_property("childProp", constant("cp"))
            .to("mock:part")
            .end()
            .to("mock:after");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "a,b").unwrap();
    assert_eq!(bodies(&ctx, "mock:part"), vec!["a-p-pp", "b-p-pp"]);
    let after = ctx.mock_endpoint("mock:after").unwrap().exchange(0).unwrap();
    assert!(after.header("child").is_none());
    assert!(after.property("childProp").is_none());
    assert_eq!(after.header("parent"), Some(&Value::from("p")));
}

#[test]
fn split_sub_exchanges_have_own_exchange_id_and_correlation() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").split(body()).to("mock:part").end();
    })
    .unwrap();
    ctx.start().unwrap();
    let ex = ctx.create_producer_template().send_with("direct:start", ExchangePattern::InOnly, |e| e.set_body("a,b"));
    let parts = ctx.mock_endpoint("mock:part").unwrap().received_exchanges();
    assert_ne!(parts[0].exchange_id, ex.exchange_id);
    assert_ne!(parts[0].exchange_id, parts[1].exchange_id);
    assert_eq!(parts[0].property(props::CORRELATION_ID), Some(&Value::from(ex.exchange_id.to_string())));
}

#[test]
fn multicast_branch_failure_propagates_to_parent() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").multicast().to("mock:a").to("direct:fail").to("mock:c").end().to("mock:after");
        b.from("direct:fail").error_handler(no_error_handler()).throw_exception("IOException", "branch failed");
    })
    .unwrap();
    ctx.start().unwrap();
    let e = ctx.create_producer_template().send_body("direct:start", "x").unwrap_err();
    assert!(e.cause.unwrap().is_a("IOException"));
    assert_eq!(ctx.mock_endpoint("mock:after").unwrap().received_counter(), 0);
    // without stopOnException the other branches are still processed
    assert_eq!(ctx.mock_endpoint("mock:c").unwrap().received_counter(), 1);
}

#[test]
fn recipient_list_parallel_with_strategy() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").recipient_list(header("to")).parallel_processing().aggregation_strategy(ConcatStrategy).end();
        b.from("direct:x").delay(constant(40)).transform(constant("X"));
        b.from("direct:y").transform(constant("Y"));
    })
    .unwrap();
    ctx.start().unwrap();
    let r = ctx.create_producer_template().request_body_and_header("direct:start", "", "to", "direct:x,direct:y").unwrap();
    assert_eq!(r, Value::from("X+Y"));
    ctx.stop().unwrap();
}

#[test]
fn route_scoped_on_completion_overrides_global() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.on_completion().to("mock:global");
        b.from("direct:scoped").on_completion().to("mock:local").end().to("mock:a");
        b.from("direct:unscoped").to("mock:b");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:scoped", "1").unwrap();
    t.send_body("direct:unscoped", "2").unwrap();
    assert!(ctx.mock_endpoint("mock:local").unwrap().wait_for_messages(1, Duration::from_secs(5)));
    assert!(ctx.mock_endpoint("mock:global").unwrap().wait_for_messages(1, Duration::from_secs(5)));
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(bodies(&ctx, "mock:global"), vec!["2"]);
    assert_eq!(bodies(&ctx, "mock:local"), vec!["1"]);
}

#[test]
fn validate_eip_throws_predicate_validation_exception() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").validate(simple("${body} regex '^\\d+$'")).to("mock:valid");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:start", "123").unwrap();
    let e = t.send_body("direct:start", "abc").unwrap_err();
    assert_eq!(e.cause.unwrap().kind, "PredicateValidationException");
    assert_eq!(ctx.mock_endpoint("mock:valid").unwrap().received_counter(), 1);
}

#[test]
fn set_header_from_header_and_remove_headers_pattern() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").set_header("copy", header("orig")).remove_headers("tmp*").to("mock:out");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template()
        .send_body_and_headers("direct:start", "x", vec![("orig", Value::from("o")), ("tmpA", Value::Int(1)), ("TMPb", Value::Int(2)), ("keep", Value::Int(3))])
        .unwrap();
    let ex = ctx.mock_endpoint("mock:out").unwrap().exchange(0).unwrap();
    assert_eq!(ex.header("copy"), Some(&Value::from("o")));
    assert!(ex.header("tmpA").is_none());
    assert!(ex.header("keep").is_some());
}

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use camel_core::prelude::*;
use camel_core::processor::routing::strategies::{grouped_body, string, use_latest};

#[test]
fn split_array_keeps_original_and_sets_properties() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").split(body()).to("mock:split").end().to("mock:result");
    })
    .unwrap();
    ctx.start().unwrap();
    let reply = ctx.create_producer_template().request_body("direct:start", Value::from(vec!["a", "b", "c"])).unwrap();
    assert_eq!(reply, Value::from(vec!["a", "b", "c"]));
    let m = ctx.mock_endpoint("split").unwrap();
    assert_eq!(m.received_bodies(), vec![Value::from("a"), Value::from("b"), Value::from("c")]);
    let ex = m.received_exchanges();
    for (i, e) in ex.iter().enumerate() {
        assert_eq!(e.property(props::SPLIT_INDEX), Some(&Value::from(i)));
        assert_eq!(e.property(props::SPLIT_SIZE), Some(&Value::from(3)));
        assert_eq!(e.property(props::SPLIT_COMPLETE), Some(&Value::Bool(i == 2)));
        assert!(e.property(props::CORRELATION_ID).is_some());
    }
    let result = ctx.mock_endpoint("result").unwrap().received_exchanges();
    assert_eq!(result[0].body(), &Value::from(vec!["a", "b", "c"]));
    assert!(result[0].property(props::SPLIT_INDEX).is_none());
}

#[test]
fn split_string_default_comma_and_tokenize() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:comma").split(body()).to("mock:comma").end();
        b.from("direct:lines").split(body().tokenize("\n")).to("mock:lines").end();
        b.from("direct:nodelim").split(body()).delimiter("false").to("mock:nodelim").end();
        b.from("direct:semi").split(body()).delimiter(";").to("mock:semi").end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:comma", "a,b,c").unwrap();
    t.send_body("direct:lines", "l1\nl2").unwrap();
    t.send_body("direct:nodelim", "a,b").unwrap();
    t.send_body("direct:semi", "x;y").unwrap();
    assert_eq!(ctx.mock_endpoint("comma").unwrap().received_counter(), 3);
    assert_eq!(ctx.mock_endpoint("lines").unwrap().received_bodies(), vec![Value::from("l1"), Value::from("l2")]);
    assert_eq!(ctx.mock_endpoint("nodelim").unwrap().received_bodies(), vec![Value::from("a,b")]);
    assert_eq!(ctx.mock_endpoint("semi").unwrap().received_bodies(), vec![Value::from("x"), Value::from("y")]);
}

#[test]
fn split_with_aggregation_strategy_replaces_result() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .split(body())
            .aggregation_strategy(grouped_body())
            .set_body(body().map(|v| Ok(Value::from(v.to_string().to_uppercase()))))
            .end()
            .to("mock:result");
        b.from("direct:concat").split(body()).aggregation_strategy(string("+")).to("mock:x").end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let r = t.request_body("direct:start", Value::from(vec!["a", "b"])).unwrap();
    assert_eq!(r, Value::from(vec!["A", "B"]));
    assert_eq!(t.request_body("direct:concat", "x,y,z").unwrap(), Value::from("x+y+z"));
}

#[test]
fn split_failure_propagates_but_continues() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .split(body())
            .process(|ex: &mut Exchange| {
                if ex.body() == &Value::from("bad") {
                    return Err(CamelError::illegal_argument("bad part"));
                }
                Ok(())
            })
            .to("mock:part")
            .end()
            .to("mock:result");
    })
    .unwrap();
    ctx.start().unwrap();
    let err = ctx.create_producer_template().send_body("direct:start", "a,bad,c").unwrap_err();
    assert!(err.cause.unwrap().is_a(kinds::ILLEGAL_ARGUMENT));
    assert_eq!(ctx.mock_endpoint("part").unwrap().received_bodies(), vec![Value::from("a"), Value::from("c")]);
    assert_eq!(ctx.mock_endpoint("result").unwrap().received_counter(), 0);
}

#[test]
fn split_stop_on_exception() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .split(body())
            .stop_on_exception()
            .process(|ex: &mut Exchange| {
                if ex.body() == &Value::from("bad") {
                    return Err(CamelError::io("io"));
                }
                Ok(())
            })
            .to("mock:part")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let ex = ctx.create_producer_template().send_with("direct:start", ExchangePattern::InOnly, |e| e.set_body("a,bad,c"));
    let e = ex.exception.unwrap();
    assert!(e.is_a(kinds::CAMEL_EXCHANGE), "{}", e);
    assert!(e.message.contains("Sequential processing failed for number 1"), "{}", e);
    assert!(e.cause.unwrap().is_a(kinds::IO));
    assert_eq!(ctx.mock_endpoint("part").unwrap().received_bodies(), vec![Value::from("a")]);
}

#[test]
fn split_parallel_processing_keeps_order_in_aggregation() {
    let ctx = CamelContext::new();
    let threads = Arc::new(Mutex::new(std::collections::HashSet::new()));
    let th = threads.clone();
    ctx.add_routes(move |b| {
        b.from("direct:start")
            .split(body())
            .parallel_processing()
            .aggregation_strategy(grouped_body())
            .process(move |ex: &mut Exchange| {
                th.lock().unwrap().insert(format!("{:?}", std::thread::current().id()));
                let n: i64 = ex.body_as()?;
                // later elements finish first
                std::thread::sleep(Duration::from_millis((50 - n * 10) as u64));
                ex.set_body(n * 10);
                Ok(())
            })
            .to("mock:part")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let start = Instant::now();
    let r = ctx.create_producer_template().request_body("direct:start", ints(&[1, 2, 3, 4])).unwrap();
    assert_eq!(r, ints(&[10, 20, 30, 40]));
    assert!(start.elapsed() < Duration::from_millis(120), "{:?}", start.elapsed());
    assert!(threads.lock().unwrap().len() > 1);
    ctx.stop().unwrap();
}

#[test]
fn split_parallel_streaming_aggregates_in_completion_order() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .split(body())
            .parallel_processing()
            .streaming()
            .aggregation_strategy(grouped_body())
            .process(|ex: &mut Exchange| {
                let n: i64 = ex.body_as()?;
                std::thread::sleep(Duration::from_millis((n * 30) as u64));
                Ok(())
            })
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let r = ctx.create_producer_template().request_body("direct:start", ints(&[3, 1, 2])).unwrap();
    assert_eq!(r, ints(&[1, 2, 3]));
}

#[test]
fn split_parallel_timeout() {
    let ctx = CamelContext::new();
    let timeouts = Arc::new(AtomicUsize::new(0));
    struct S(Arc<AtomicUsize>);
    impl AggregationStrategy for S {
        fn aggregate(&self, old: Option<Exchange>, new: Exchange) -> Result<Exchange> {
            camel_core::processor::routing::strategies::StringAggregationStrategy::new(",").aggregate(old, new)
        }
        fn timeout(&self, _old: &Exchange, _index: usize, _total: usize, _ms: u64) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let t2 = timeouts.clone();
    ctx.add_routes(move |b| {
        b.from("direct:start")
            .split(body())
            .parallel_processing()
            .timeout(Duration::from_millis(100))
            .aggregation_strategy(S(t2))
            .process(|ex: &mut Exchange| {
                if ex.body() == &Value::from("slow") {
                    std::thread::sleep(Duration::from_millis(400));
                }
                Ok(())
            })
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let start = Instant::now();
    let r = ctx.create_producer_template().request_body("direct:start", "a,slow,b").unwrap();
    assert!(start.elapsed() < Duration::from_millis(300));
    assert_eq!(r, Value::from("a,b"));
    assert_eq!(timeouts.load(Ordering::SeqCst), 1);
}

#[test]
fn split_share_unit_of_work_runs_on_completions_with_parent() {
    let ctx = CamelContext::new();
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let l1 = log.clone();
    let l2 = log.clone();
    ctx.add_routes(move |b| {
        b.from("direct:shared")
            .split(body())
            .share_unit_of_work()
            .process(move |ex: &mut Exchange| {
                let l = l1.clone();
                let body = ex.body().to_string();
                ex.add_on_completion(move |_| l.lock().unwrap().push(format!("done {}", body)));
                Ok(())
            })
            .end()
            .process({
                let l = l2.clone();
                move |_: &mut Exchange| {
                    l.lock().unwrap().push("after split".into());
                    Ok(())
                }
            });
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:shared", "a,b").unwrap();
    assert_eq!(*log.lock().unwrap(), vec!["after split".to_string(), "done a".into(), "done b".into()]);
}

#[test]
fn split_empty_and_null() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").split(header("missing")).to("mock:part").end().to("mock:result");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "x").unwrap();
    assert_eq!(ctx.mock_endpoint("part").unwrap().received_counter(), 0);
    assert_eq!(ctx.mock_endpoint("result").unwrap().received_counter(), 1);
}

#[test]
fn nested_split() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .split(body())
            .split(body())
            .delimiter("-")
            .to("mock:inner")
            .end()
            .to("mock:outer")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", Value::from(vec!["a-b", "c"])).unwrap();
    assert_eq!(ctx.mock_endpoint("inner").unwrap().received_bodies(), vec![Value::from("a"), Value::from("b"), Value::from("c")]);
    let outer = ctx.mock_endpoint("outer").unwrap().received_exchanges();
    assert_eq!(outer.len(), 2);
    // outer split index restored after the nested split
    assert_eq!(outer[1].property(props::SPLIT_INDEX), Some(&Value::from(1)));
}

#[test]
fn multicast_default_uses_latest_reply() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .multicast()
            .to("direct:a")
            .to("direct:b")
            .end()
            .to("mock:result");
        b.from("direct:a").set_body(constant("A")).to("mock:a");
        b.from("direct:b").set_body(constant("B")).to("mock:b");
    })
    .unwrap();
    ctx.start().unwrap();
    let r = ctx.create_producer_template().request_body("direct:start", "x").unwrap();
    assert_eq!(r, Value::from("B"));
    let a = ctx.mock_endpoint("a").unwrap().received_exchanges();
    assert_eq!(a[0].property(props::MULTICAST_INDEX), Some(&Value::from(0)));
    assert_eq!(a[0].property(props::MULTICAST_COMPLETE), Some(&Value::Bool(false)));
    let b = ctx.mock_endpoint("b").unwrap().received_exchanges();
    assert_eq!(b[0].property(props::MULTICAST_COMPLETE), Some(&Value::Bool(true)));
    // each branch got a copy of the original
    assert_eq!(ctx.mock_endpoint("result").unwrap().received_bodies(), vec![Value::from("B")]);
}

#[test]
fn multicast_with_strategy_and_parallel() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .multicast()
            .parallel_processing()
            .aggregation_strategy(string(","))
            .to("direct:a")
            .to("direct:b")
            .to("direct:c")
            .end();
        b.from("direct:a").process(|ex: &mut Exchange| {
            std::thread::sleep(Duration::from_millis(30));
            ex.set_body("A");
            Ok(())
        });
        b.from("direct:b").set_body(constant("B"));
        b.from("direct:c").set_body(constant("C"));
    })
    .unwrap();
    ctx.start().unwrap();
    let r = ctx.create_producer_template().request_body("direct:start", "x").unwrap();
    assert_eq!(r, Value::from("A,B,C"));
}

#[test]
fn multicast_failure_and_stop_on_exception() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:continue").multicast().to("direct:fail").to("mock:c1").end();
        b.from("direct:stop").multicast().stop_on_exception().to("direct:fail").to("mock:c2").end();
        b.from("direct:fail").process(|_: &mut Exchange| Err(CamelError::io("down")));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let e = t.send_body("direct:continue", "x").unwrap_err();
    assert!(e.cause.unwrap().is_a(kinds::IO));
    assert_eq!(ctx.mock_endpoint("c1").unwrap().received_counter(), 1);
    let e = t.send_body("direct:stop", "x").unwrap_err();
    assert!(e.cause.unwrap().is_a(kinds::CAMEL_EXCHANGE));
    assert_eq!(ctx.mock_endpoint("c2").unwrap().received_counter(), 0);
}

#[test]
fn multicast_strategy_can_handle_failures() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .multicast()
            .aggregation_strategy(|old: Option<Exchange>, mut new: Exchange| -> Result<Exchange> {
                if new.is_failed() {
                    new.exception = None;
                    new.set_body("recovered");
                }
                Ok(match old {
                    Some(mut o) => {
                        let s = format!("{}|{}", o.body(), new.body());
                        o.set_body(s);
                        o
                    }
                    None => new,
                })
            })
            .to("direct:fail")
            .to("direct:ok")
            .end();
        b.from("direct:fail").process(|_: &mut Exchange| Err(CamelError::io("down")));
        b.from("direct:ok").set_body(constant("ok"));
    })
    .unwrap();
    ctx.start().unwrap();
    let r = ctx.create_producer_template().request_body("direct:start", "x").unwrap();
    assert_eq!(r, Value::from("recovered|ok"));
}

#[test]
fn aggregation_strategy_ref_from_registry() {
    let ctx = CamelContext::new();
    ctx.registry().bind_aggregation_strategy("latest", use_latest());
    ctx.registry().bind_aggregation_strategy("concat", string(""));
    ctx.add_routes(|b| {
        b.from("direct:start").split(body()).aggregation_strategy_ref("concat").end();
        b.from("direct:missing").split(body()).aggregation_strategy_ref("nope").end();
    })
    .unwrap();
    let e = ctx.start().unwrap_err();
    assert!(e.to_string().contains("nope"), "{}", e);
}

#[test]
fn split_restart_route_parallel() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").route_id("r").split(body()).parallel_processing().to("mock:p").end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:start", "a,b").unwrap();
    ctx.stop_route("r").unwrap();
    ctx.start_route("r").unwrap();
    t.send_body("direct:start", "c,d").unwrap();
    assert_eq!(ctx.mock_endpoint("p").unwrap().received_counter(), 4);
    ctx.stop().unwrap();
}

#[test]
fn option_in_wrong_place_is_build_error() {
    let ctx = CamelContext::new();
    let r = ctx.add_routes(|b| {
        b.from("direct:start").to("mock:x").parallel_processing();
    });
    assert!(r.is_err());
    let r = ctx.add_routes(|b| {
        b.from("direct:start2").completion_size(3);
    });
    assert!(r.is_err());
}

fn ints(v: &[i64]) -> Value {
    Value::Array(v.iter().map(|i| Value::from(*i)).collect())
}

#[test]
fn split_streaming_is_lazy_and_sets_size_on_last() {
    let ctx = CamelContext::new();
    ctx.add_routes(move |b| {
        b.from("direct:stream").split(body()).delimiter("\n").streaming().to("mock:part").end().to("mock:after");
    })
    .unwrap();
    ctx.start().unwrap();
    let body: String = (0..1000).map(|i| format!("line{}", i)).collect::<Vec<_>>().join("\n");
    ctx.create_producer_template().send_body("direct:stream", body.as_str()).unwrap();
    let parts = ctx.mock_endpoint("mock:part").unwrap();
    assert_eq!(parts.received_counter(), 1000);
    let exs = parts.received_exchanges();
    assert_eq!(exs[0].body(), &Value::from("line0"));
    assert_eq!(exs[999].body(), &Value::from("line999"));
    assert_eq!(exs[0].property(props::SPLIT_INDEX), Some(&Value::Int(0)));
    assert_eq!(exs[0].property(props::SPLIT_COMPLETE), Some(&Value::Bool(false)));
    // like Camel, the size is only known (and set) on the last part when streaming
    assert!(exs[0].property(props::SPLIT_SIZE).is_none());
    assert_eq!(exs[999].property(props::SPLIT_SIZE), Some(&Value::Int(1000)));
    assert_eq!(exs[999].property(props::SPLIT_COMPLETE), Some(&Value::Bool(true)));
    // the original message continues
    assert_eq!(ctx.mock_endpoint("mock:after").unwrap().exchange(0).unwrap().body(), &Value::from(body.as_str()));
}

#[test]
fn split_streaming_creates_parts_on_demand() {
    use camel_core::processor::routing::support::to_lazy_iter;
    // the lazy iterator yields parts one at a time and can be abandoned early
    let mut it = to_lazy_iter(Value::from("a,b,,c"), Some(","));
    assert_eq!(it.next(), Some(Value::from("a")));
    assert_eq!(it.next(), Some(Value::from("b")));
    assert_eq!(it.next(), Some(Value::from("")));
    assert_eq!(it.next(), Some(Value::from("c")));
    assert_eq!(it.next(), None);
    assert_eq!(to_lazy_iter(Value::from(""), Some(",")).count(), 0);
    assert_eq!(to_lazy_iter(Value::from("x"), None).collect::<Vec<_>>(), vec![Value::from("x")]);
    // stopOnException stops pulling parts
    let ctx = CamelContext::new();
    ctx.add_routes(move |b| {
        b.from("direct:stop")
            .split(body())
            .streaming()
            .stop_on_exception()
            .to("mock:seen")
            .filter(simple("${body} == 'b'"))
            .throw_exception("IllegalArgumentException", "bad part")
            .end()
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    assert!(ctx.create_producer_template().send_body("direct:stop", "a,b,c,d").is_err());
    assert_eq!(ctx.mock_endpoint("mock:seen").unwrap().received_counter(), 2);
}

#[test]
fn split_json_text_with_jsonpath_keeps_headers_and_the_original_body() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .set_header("orderId", jsonpath("$.orderId"))
            .split(jsonpath("$.items"))
            .to("mock:part")
            .end()
            .to("mock:result");
    })
    .unwrap();
    ctx.start().unwrap();
    let order = r#"{"orderId":7,"items":[{"sku":"a"},{"sku":"b"}]}"#;
    ctx.create_producer_template().send_body("direct:start", order).unwrap();
    let parts = ctx.mock_endpoint("part").unwrap();
    let bodies = parts.received_bodies();
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[0].as_object().and_then(|o| o.get("sku")), Some(&Value::from("a")));
    assert_eq!(bodies[1].as_object().and_then(|o| o.get("sku")), Some(&Value::from("b")));
    for ex in parts.received_exchanges() {
        assert_eq!(ex.message.header("orderId"), Some(&Value::from(7)));
    }
    // the exchange continues after the split with its original (text) body
    assert_eq!(ctx.mock_endpoint("result").unwrap().received_bodies(), vec![Value::from(order)]);
}

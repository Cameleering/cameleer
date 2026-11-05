use std::sync::Arc;
use std::time::{Duration, Instant};

use cameleer_core::prelude::*;
use cameleer_core::processor::routing::strategies::{
    flexible, grouped_body, grouped_exchange, string, use_latest, use_original, AGGREGATION_COMPLETE_ALL_GROUPS,
    AGGREGATION_COMPLETE_CURRENT_GROUP,
};
use cameleer_core::processor::routing::MemoryAggregationRepository;

fn send(t: &ProducerTemplate, body: &str, id: &str) {
    t.send_body_and_header("direct:start", body, "id", id).unwrap();
}

#[test]
fn completion_size_groups_by_correlation() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .aggregate(header("id"), string("+"))
            .completion_size(3)
            .to("mock:aggregated")
            .end()
            .to("mock:after");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for (b, id) in [("a", "1"), ("b", "2"), ("c", "1"), ("d", "1"), ("e", "2")] {
        send(&t, b, id);
    }
    let m = ctx.mock_endpoint("aggregated").unwrap();
    assert_eq!(m.received_bodies(), vec![Value::from("a+c+d")]);
    let ex = &m.received_exchanges()[0];
    assert_eq!(ex.property(props::AGGREGATED_SIZE), Some(&Value::from(3)));
    assert_eq!(ex.property(props::AGGREGATED_COMPLETED_BY), Some(&Value::from("size")));
    assert_eq!(ex.property(props::AGGREGATED_CORRELATION_KEY), Some(&Value::from("1")));
    // the incoming exchanges continue unchanged after the aggregate block
    assert_eq!(
        ctx.mock_endpoint("after").unwrap().received_bodies(),
        ["a", "b", "c", "d", "e"].iter().map(|s| Value::from(*s)).collect::<Vec<_>>()
    );
}

#[test]
fn completion_predicate_and_grouped_body() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .aggregate(header("id"), grouped_body())
            .completion_predicate(header("last").is_equal_to(true))
            // grouped strategies keep the first exchange as the aggregate, so check the incoming one
            .eager_check_completion()
            .to("mock:aggregated")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    send(&t, "a", "x");
    send(&t, "b", "x");
    t.send_body_and_headers("direct:start", "c", vec![("id", Value::from("x")), ("last", Value::from(true))]).unwrap();
    let m = ctx.mock_endpoint("aggregated").unwrap();
    assert_eq!(m.received_bodies(), vec![Value::from(vec!["a", "b", "c"])]);
    assert_eq!(m.received_exchanges()[0].property(props::AGGREGATED_COMPLETED_BY), Some(&Value::from("predicate")));
}

#[test]
fn eager_check_completion_uses_incoming() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .aggregate(header("id"), string(""))
            .completion_predicate(body().is_equal_to("END"))
            .eager_check_completion()
            .to("mock:aggregated")
            .end();
        b.from("direct:lazy")
            .aggregate(header("id"), string(""))
            .completion_predicate(body().is_equal_to("END"))
            .to("mock:lazy")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for b in ["a", "b", "END"] {
        send(&t, b, "k");
        t.send_body_and_header("direct:lazy", b, "id", "k").unwrap();
    }
    assert_eq!(ctx.mock_endpoint("aggregated").unwrap().received_bodies(), vec![Value::from("abEND")]);
    // without eager check the aggregated body "abEND" never equals END
    assert_eq!(ctx.mock_endpoint("lazy").unwrap().received_counter(), 0);
}

#[test]
fn completion_size_expression() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .aggregate(header("id"), string(","))
            .completion_size_expr(header("size"))
            .to("mock:aggregated")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for b in ["a", "b"] {
        t.send_body_and_headers("direct:start", b, vec![("id", Value::from("1")), ("size", Value::from(2))]).unwrap();
    }
    assert_eq!(ctx.mock_endpoint("aggregated").unwrap().received_bodies(), vec![Value::from("a,b")]);
}

#[test]
fn completion_timeout_completes_inactive_groups() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .aggregate(header("id"), string("+"))
            .completion_timeout(Duration::from_millis(100))
            .to("mock:aggregated")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let start = Instant::now();
    send(&t, "a", "1");
    send(&t, "b", "1");
    send(&t, "c", "2");
    let m = ctx.mock_endpoint("aggregated").unwrap();
    m.expected_message_count(2);
    m.expected_bodies_received_in_any_order(vec!["a+b", "c"]);
    m.assert_is_satisfied_within(Duration::from_secs(2)).unwrap();
    assert!(start.elapsed() >= Duration::from_millis(100));
    assert!(start.elapsed() < Duration::from_millis(600), "{:?}", start.elapsed());
    for e in m.received_exchanges() {
        assert_eq!(e.property(props::AGGREGATED_COMPLETED_BY), Some(&Value::from("timeout")));
    }
    ctx.stop().unwrap();
}

#[test]
fn completion_timeout_with_size_and_discard() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .aggregate(header("id"), string("+"))
            .completion_size(2)
            .completion_timeout(Duration::from_millis(50))
            .discard_on_completion_timeout()
            .to("mock:aggregated")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    send(&t, "a", "1");
    send(&t, "b", "1");
    send(&t, "lonely", "2");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(ctx.mock_endpoint("aggregated").unwrap().received_bodies(), vec![Value::from("a+b")]);
    ctx.stop().unwrap();
}

#[test]
fn completion_interval() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .aggregate(header("id"), string("+"))
            .completion_interval(Duration::from_millis(80))
            .to("mock:aggregated")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    send(&t, "a", "1");
    send(&t, "b", "2");
    send(&t, "c", "1");
    let m = ctx.mock_endpoint("aggregated").unwrap();
    m.expected_message_count(2);
    m.expected_bodies_received_in_any_order(vec!["a+c", "b"]);
    m.assert_is_satisfied_within(Duration::from_secs(2)).unwrap();
    assert_eq!(m.received_exchanges()[0].property(props::AGGREGATED_COMPLETED_BY), Some(&Value::from("interval")));
    ctx.stop().unwrap();
}

#[test]
fn completion_from_batch_consumer() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .aggregate(constant(true), string(","))
            .completion_from_batch_consumer()
            .to("mock:aggregated")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for (i, b) in ["x", "y", "z"].iter().enumerate() {
        t.send_with("direct:start", ExchangePattern::InOnly, |ex| {
            ex.set_body(*b);
            ex.set_property(props::BATCH_SIZE, 3);
            ex.set_property(props::BATCH_INDEX, i);
        });
    }
    let m = ctx.mock_endpoint("aggregated").unwrap();
    assert_eq!(m.received_bodies(), vec![Value::from("x,y,z")]);
    assert_eq!(m.received_exchanges()[0].property(props::AGGREGATED_COMPLETED_BY), Some(&Value::from("consumer")));
}

#[test]
fn completion_on_new_correlation_group() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .aggregate(header("id"), string(""))
            .completion_on_new_correlation_group()
            .completion_size(100)
            .force_completion_on_stop()
            .to("mock:aggregated")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for (b, id) in [("a", "1"), ("b", "1"), ("c", "2"), ("d", "3"), ("e", "3")] {
        send(&t, b, id);
    }
    let m = ctx.mock_endpoint("aggregated").unwrap();
    assert_eq!(m.received_bodies(), vec![Value::from("ab"), Value::from("c")]);
    ctx.stop().unwrap();
    assert_eq!(m.received_bodies(), vec![Value::from("ab"), Value::from("c"), Value::from("de")]);
    assert_eq!(m.received_exchanges()[2].property(props::AGGREGATED_COMPLETED_BY), Some(&Value::from("force")));
}

#[test]
fn invalid_correlation_keys() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:strict").aggregate(header("id"), string("")).completion_size(2).to("mock:strict").end();
        b.from("direct:lenient")
            .aggregate(header("id"), string(""))
            .completion_size(2)
            .ignore_invalid_correlation_keys()
            .to("mock:lenient")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let e = t.send_body("direct:strict", "x").unwrap_err();
    assert!(e.cause.unwrap().message.contains("Invalid correlation key"));
    t.send_body("direct:lenient", "x").unwrap();
    t.send_body_and_header("direct:lenient", "a", "id", "1").unwrap();
    t.send_body_and_header("direct:lenient", "b", "id", "1").unwrap();
    assert_eq!(ctx.mock_endpoint("lenient").unwrap().received_bodies(), vec![Value::from("ab")]);
}

#[test]
fn close_correlation_key_on_completion() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .aggregate(header("id"), string(""))
            .completion_size(2)
            .close_correlation_key_on_completion(100)
            .to("mock:aggregated")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    send(&t, "a", "1");
    send(&t, "b", "1");
    let e = t.send_body_and_header("direct:start", "c", "id", "1").unwrap_err();
    let cause = e.cause.unwrap();
    assert!(cause.is_a("ClosedCorrelationKeyException"));
    assert!(cause.is_a(kinds::CAMEL_EXCHANGE));
    send(&t, "d", "2");
}

#[test]
fn strategy_signals_completion_and_complete_all_groups() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .aggregate(header("id"), |old: Option<Exchange>, new: Exchange| -> Result<Exchange> {
                let mut r = string("").aggregate(old, new)?;
                if r.body().to_string().len() >= 3 {
                    r.set_property(AGGREGATION_COMPLETE_CURRENT_GROUP, true);
                }
                Ok(r)
            })
            .completion_size(10)
            .to("mock:aggregated")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for b in ["a", "b", "c"] {
        send(&t, b, "1");
    }
    send(&t, "x", "2");
    send(&t, "y", "3");
    let m = ctx.mock_endpoint("aggregated").unwrap();
    assert_eq!(m.received_bodies(), vec![Value::from("abc")]);
    assert_eq!(m.received_exchanges()[0].property(props::AGGREGATED_COMPLETED_BY), Some(&Value::from("strategy")));
    t.send_with("direct:start", ExchangePattern::InOnly, |ex| {
        ex.set_property(AGGREGATION_COMPLETE_ALL_GROUPS, true);
    });
    assert_eq!(m.received_bodies(), vec![Value::from("abc"), Value::from("x"), Value::from("y")]);
}

#[test]
fn aggregate_requires_completion_condition_and_strategy() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").aggregate(header("id"), string("")).to("mock:x").end();
    })
    .unwrap();
    assert!(ctx.start().is_err());
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").aggregate_correlation(header("id")).completion_size(2).to("mock:x").end();
    })
    .unwrap();
    assert!(ctx.start().unwrap_err().to_string().contains("AggregationStrategy"));
}

#[test]
fn aggregate_ref_and_custom_repository_and_strategies() {
    let ctx = CamelContext::new();
    ctx.registry().bind_aggregation_strategy("grouped", grouped_exchange());
    let repo = Arc::new(MemoryAggregationRepository::new());
    let repo2 = repo.clone();
    ctx.add_routes(move |b| {
        b.from("direct:start").aggregate_ref(header("id"), "grouped").completion_size(2).aggregation_repository(repo2).to("mock:grouped").end();
        b.from("direct:latest").aggregate(header("id"), use_latest()).completion_size(2).to("mock:latest").end();
        b.from("direct:original").aggregate(header("id"), use_original()).completion_size(2).to("mock:original").end();
        b.from("direct:flex")
            .aggregate_correlation(header("id"))
            .aggregation_strategy(flexible().pick(header("n")).store_in_header("ns"))
            .completion_size(3)
            .to("mock:flex")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    send(&t, "a", "1");
    use cameleer_core::processor::routing::AggregationRepository;
    assert_eq!(repo.keys(), vec!["1".to_string()]);
    send(&t, "b", "1");
    assert!(repo.keys().is_empty());
    let g = ctx.mock_endpoint("grouped").unwrap().received_exchanges();
    let list = g[0].body().as_array().unwrap().clone();
    assert_eq!(list.len(), 2);
    assert_eq!(list[1].get("body"), Some(&Value::from("b")));
    for name in ["latest", "original"] {
        for b in ["first", "second"] {
            t.send_body_and_header(&format!("direct:{}", name), b, "id", "1").unwrap();
        }
    }
    assert_eq!(ctx.mock_endpoint("latest").unwrap().received_bodies(), vec![Value::from("second")]);
    assert_eq!(ctx.mock_endpoint("original").unwrap().received_bodies(), vec![Value::from("first")]);
    for n in [1, 2, 3] {
        t.send_body_and_headers("direct:flex", "x", vec![("id", Value::from("1")), ("n", Value::from(n))]).unwrap();
    }
    let f = ctx.mock_endpoint("flex").unwrap().received_exchanges();
    assert_eq!(f[0].header("ns"), Some(&Value::Array(vec![Value::from(1), Value::from(2), Value::from(3)])));
}

#[test]
fn concurrent_aggregation_is_thread_safe() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").aggregate(header("id"), grouped_body()).completion_size(10).to("mock:aggregated").end();
    })
    .unwrap();
    ctx.start().unwrap();
    let mut handles = Vec::new();
    for th in 0..4 {
        let t = ctx.create_producer_template();
        handles.push(std::thread::spawn(move || {
            for i in 0..50 {
                t.send_body_and_header("direct:start", i, "id", (th * 50 + i) % 5).unwrap();
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    let m = ctx.mock_endpoint("aggregated").unwrap();
    assert_eq!(m.received_counter(), 20);
    for e in m.received_exchanges() {
        assert_eq!(e.body().as_array().unwrap().len(), 10);
    }
}

#[test]
fn aggregated_output_failure_does_not_fail_incoming() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .aggregate(header("id"), string(""))
            .completion_size(1)
            .process(|_: &mut Exchange| Err(CamelError::io("downstream")))
            .end()
            .to("mock:after");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body_and_header("direct:start", "a", "id", "1").unwrap();
    assert_eq!(ctx.mock_endpoint("after").unwrap().received_counter(), 1);
}

#[test]
fn memory_repository_take_moves_the_group_and_keeps_its_order() {
    use cameleer_core::processor::routing::AggregationRepository;
    let repo = MemoryAggregationRepository::new();
    repo.add("a", Exchange::with_body(None, "A"));
    repo.add("b", Exchange::with_body(None, "B"));
    let a = repo.take("a").unwrap();
    assert_eq!(a.message.body, Value::from("A"));
    // taken out, but still a group in progress, in its original position
    assert!(repo.get("a").is_none());
    assert_eq!(repo.keys(), vec!["a", "b"]);
    assert!(repo.add("a", Exchange::with_body(None, "A2")).is_none());
    assert_eq!(repo.keys(), vec!["a", "b"]);
    assert_eq!(repo.get("a").unwrap().message.body, Value::from("A2"));
    assert_eq!(repo.add("a", Exchange::with_body(None, "A3")).unwrap().message.body, Value::from("A2"));
    assert_eq!(repo.remove("a").unwrap().message.body, Value::from("A3"));
    assert_eq!(repo.keys(), vec!["b"]);
}

#[test]
fn grouped_body_groups_are_moved_not_copied_and_complete_in_order() {
    let ctx = CamelContext::new();
    let repo = Arc::new(MemoryAggregationRepository::new());
    let r = repo.clone();
    ctx.add_routes(move |b| {
        b.from("direct:start").aggregate(header("id"), grouped_body()).aggregation_repository(r.clone()).completion_size(3).to("mock:agg").end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for (b, id) in [("a", "1"), ("b", "2"), ("c", "1"), ("d", "2"), ("e", "1")] {
        send(&t, b, id);
    }
    {
        use cameleer_core::processor::routing::AggregationRepository;
        // group 2 is in progress and still readable from the repository
        assert_eq!(repo.keys(), vec!["2"]);
        assert_eq!(repo.get("2").unwrap().message.body, Value::from("b"));
    }
    send(&t, "f", "2");
    let m = ctx.mock_endpoint("agg").unwrap();
    assert_eq!(
        m.received_bodies(),
        vec![
            Value::Array(vec![Value::from("a"), Value::from("c"), Value::from("e")]),
            Value::Array(vec![Value::from("b"), Value::from("d"), Value::from("f")]),
        ]
    );
}

#[test]
fn a_failing_strategy_leaves_the_group_in_the_repository() {
    let ctx = CamelContext::new();
    let repo = Arc::new(MemoryAggregationRepository::new());
    let r = repo.clone();
    let strategy = |old: Option<Exchange>, new: Exchange| -> Result<Exchange> {
        if new.message.body == Value::from("bad") {
            return Err(CamelError::illegal_argument("bad body"));
        }
        Ok(match old {
            None => new,
            Some(mut o) => {
                o.message.body = Value::from(format!("{}+{}", o.message.body, new.message.body));
                o
            }
        })
    };
    ctx.add_routes(move |b| {
        b.from("direct:start").aggregate(header("id"), strategy).aggregation_repository(r.clone()).completion_size(3).to("mock:agg").end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    send(&t, "a", "1");
    send(&t, "b", "1");
    assert!(t.send_body_and_header("direct:start", "bad", "id", "1").is_err());
    {
        use cameleer_core::processor::routing::AggregationRepository;
        assert_eq!(repo.get("1").unwrap().message.body, Value::from("a+b"));
    }
    send(&t, "c", "1");
    assert_eq!(ctx.mock_endpoint("agg").unwrap().received_bodies(), vec![Value::from("a+b+c")]);
}

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use camel_core::prelude::*;
use camel_core::processor::routing::claim_check::ClaimCheckOperation;
use camel_core::processor::routing::LoadBalancer;

fn ints(v: &[i64]) -> Value {
    Value::Array(v.iter().map(|i| Value::from(*i)).collect())
}

// ---------------------------------------------------------------------------
// loop
// ---------------------------------------------------------------------------

#[test]
fn loop_count_sets_index_and_size() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").loop_(header("n")).set_body(body().append("x")).to("mock:iter").end().to("mock:result");
    })
    .unwrap();
    ctx.start().unwrap();
    let r = ctx.create_producer_template().request_body_and_header("direct:start", "", "n", 3).unwrap();
    assert_eq!(r, Value::from("xxx"));
    let it = ctx.mock_endpoint("iter").unwrap().received_exchanges();
    assert_eq!(it.len(), 3);
    for (i, e) in it.iter().enumerate() {
        assert_eq!(e.property(props::LOOP_INDEX), Some(&Value::from(i)));
        assert_eq!(e.property(props::LOOP_SIZE), Some(&Value::from(3)));
    }
}

#[test]
fn loop_copy_starts_each_iteration_from_input() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").loop_(constant(3)).copy(true).set_body(body().append("x")).to("mock:iter").end();
    })
    .unwrap();
    ctx.start().unwrap();
    let r = ctx.create_producer_template().request_body("direct:start", "a").unwrap();
    assert_eq!(r, Value::from("ax"));
    assert_eq!(ctx.mock_endpoint("iter").unwrap().received_bodies(), vec![Value::from("ax"); 3]);
}

#[test]
fn loop_do_while_and_failure() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:while")
            .loop_do_while(pred(|ex| Ok(ex.body().to_string().len() < 4)))
            .set_body(body().append("y"))
            .end();
        b.from("direct:fail")
            .loop_(constant(5))
            .process(|ex: &mut Exchange| {
                if ex.property(props::LOOP_INDEX) == Some(&Value::from(1)) {
                    return Err(CamelError::io("boom"));
                }
                Ok(())
            })
            .to("mock:iter")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    assert_eq!(t.request_body("direct:while", "a").unwrap(), Value::from("ayyy"));
    assert!(t.send_body("direct:fail", "a").is_err());
    assert_eq!(ctx.mock_endpoint("iter").unwrap().received_counter(), 1);
}

// ---------------------------------------------------------------------------
// loadBalance
// ---------------------------------------------------------------------------

#[test]
fn load_balance_round_robin() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").load_balance().round_robin().to("mock:a").to("mock:b").to("mock:c").end().to("mock:after");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for i in 0..7 {
        t.send_body("direct:start", i).unwrap();
    }
    assert_eq!(ctx.mock_endpoint("a").unwrap().received_bodies(), ints(&[0, 3, 6]).as_array().unwrap().clone());
    assert_eq!(ctx.mock_endpoint("b").unwrap().received_bodies(), ints(&[1, 4]).as_array().unwrap().clone());
    assert_eq!(ctx.mock_endpoint("c").unwrap().received_counter(), 2);
    assert_eq!(ctx.mock_endpoint("after").unwrap().received_counter(), 7);
}

#[test]
fn load_balance_random_and_topic() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:random").load_balance().random().to("mock:r1").to("mock:r2").end();
        b.from("direct:topic").load_balance().topic().to("mock:t1").to("mock:t2").end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for i in 0..100 {
        t.send_body("direct:random", i).unwrap();
    }
    let (r1, r2) = (ctx.mock_endpoint("r1").unwrap().received_counter(), ctx.mock_endpoint("r2").unwrap().received_counter());
    assert_eq!(r1 + r2, 100);
    assert!(r1 > 10 && r2 > 10, "{} {}", r1, r2);
    t.send_body("direct:topic", "all").unwrap();
    assert_eq!(ctx.mock_endpoint("t1").unwrap().received_counter(), 1);
    assert_eq!(ctx.mock_endpoint("t2").unwrap().received_counter(), 1);
}

#[test]
fn load_balance_sticky() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").load_balance().sticky(header("user")).to("mock:s1").to("mock:s2").end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for u in ["bob", "alice", "bob", "bob", "alice"] {
        t.send_body_and_header("direct:start", u, "user", u).unwrap();
    }
    let s1 = ctx.mock_endpoint("s1").unwrap().received_bodies();
    let s2 = ctx.mock_endpoint("s2").unwrap().received_bodies();
    assert!(s1.iter().all(|v| v == &s1[0]));
    assert!(s2.iter().all(|v| v == &s2[0]));
    assert_eq!(s1.len() + s2.len(), 5);
}

#[test]
fn load_balance_weighted() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:rr").load_balance().weighted(true, &[4, 2, 1]).to("mock:w1").to("mock:w2").to("mock:w3").end();
        b.from("direct:random").load_balance().weighted(false, &[2, 1]).to("mock:x1").to("mock:x2").end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for i in 0..14 {
        t.send_body("direct:rr", i).unwrap();
    }
    assert_eq!(ctx.mock_endpoint("w1").unwrap().received_counter(), 8);
    assert_eq!(ctx.mock_endpoint("w2").unwrap().received_counter(), 4);
    assert_eq!(ctx.mock_endpoint("w3").unwrap().received_counter(), 2);
    // Camel interleaving: 0,1,2,0,1,0,0
    assert_eq!(ctx.mock_endpoint("w3").unwrap().received_bodies(), ints(&[2, 9]).as_array().unwrap().clone());
    for i in 0..30 {
        t.send_body("direct:random", i).unwrap();
    }
    assert_eq!(ctx.mock_endpoint("x1").unwrap().received_counter(), 20);
    assert_eq!(ctx.mock_endpoint("x2").unwrap().received_counter(), 10);
}

#[test]
fn load_balance_weighted_ratio_mismatch_fails() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:rr").load_balance().weighted(true, &[1, 2]).to("mock:w1").end();
    })
    .unwrap();
    assert!(ctx.start().is_err());
}

#[test]
fn load_balance_failover() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").load_balance().failover(&[kinds::IO]).to("direct:bad").to("mock:good").end().to("mock:after");
        b.from("direct:other").load_balance().failover(&[kinds::IO]).to("direct:illegal").to("mock:good2").end();
        b.from("direct:bad").set_body(constant("dirty")).process(|_: &mut Exchange| Err(CamelError::new(kinds::CONNECT, "refused")));
        b.from("direct:illegal").process(|_: &mut Exchange| Err(CamelError::illegal_argument("no")));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:start", "x").unwrap();
    // failover used a fresh copy of the input
    assert_eq!(ctx.mock_endpoint("good").unwrap().received_bodies(), vec![Value::from("x")]);
    assert_eq!(ctx.mock_endpoint("after").unwrap().received_counter(), 1);
    // non matching exception: no failover
    assert!(t.send_body("direct:other", "x").is_err());
    assert_eq!(ctx.mock_endpoint("good2").unwrap().received_counter(), 0);
}

#[test]
fn load_balance_failover_round_robin_and_max_attempts() {
    let ctx = CamelContext::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let c2 = calls.clone();
    ctx.add_routes(move |b| {
        b.from("direct:rr").load_balance().failover_full(-1, true, false, &[]).to("direct:f1").to("mock:ok").end();
        b.from("direct:max").load_balance().failover_full(1, true, false, &[]).to("direct:f2").to("direct:f2").to("direct:f2").end();
        b.from("direct:f1").process(|_: &mut Exchange| Err(CamelError::io("f1")));
        b.from("direct:f2").process(move |_: &mut Exchange| {
            c2.fetch_add(1, Ordering::SeqCst);
            Err(CamelError::io("f2"))
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for _ in 0..4 {
        t.send_body("direct:rr", "x").unwrap();
    }
    assert_eq!(ctx.mock_endpoint("ok").unwrap().received_counter(), 4);
    assert!(t.send_body("direct:max", "x").is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn load_balance_custom() {
    struct First;
    impl LoadBalancer for First {
        fn process(&self, exchange: &mut Exchange, processors: &[Arc<dyn Processor>]) -> Result<()> {
            processors[processors.len() - 1].process(exchange)
        }
    }
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").load_balance().custom_load_balancer(First).to("mock:c1").to("mock:c2").end();
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "x").unwrap();
    assert_eq!(ctx.mock_endpoint("c2").unwrap().received_counter(), 1);
    assert_eq!(ctx.mock_endpoint("c1").unwrap().received_counter(), 0);
}

// ---------------------------------------------------------------------------
// resequence
// ---------------------------------------------------------------------------

#[test]
fn resequence_batch_by_size() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").resequence(body()).batch().batch_size(4).to("mock:result").end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for b in ["d", "b", "a", "c"] {
        t.send_body("direct:start", b).unwrap();
    }
    let m = ctx.mock_endpoint("result").unwrap();
    assert_eq!(m.received_bodies(), ["a", "b", "c", "d"].iter().map(|s| Value::from(*s)).collect::<Vec<_>>());
    ctx.stop().unwrap();
}

#[test]
fn resequence_batch_timeout_duplicates_reverse() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").resequence(header("seq")).batch_timeout(Duration::from_millis(80)).reverse().to("mock:result").end();
        b.from("direct:dups")
            .resequence(header("seq"))
            .batch_size(3)
            .allow_duplicates()
            .to("mock:dups")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for (b, s) in [("x", 2), ("y", 1), ("z", 3), ("dup", 2)] {
        t.send_body_and_header("direct:start", b, "seq", s).unwrap();
    }
    let m = ctx.mock_endpoint("result").unwrap();
    m.expected_message_count(3);
    m.assert_is_satisfied_within(Duration::from_secs(2)).unwrap();
    assert_eq!(m.received_bodies(), vec![Value::from("z"), Value::from("x"), Value::from("y")]);
    for (b, s) in [("a", 2), ("b", 1), ("c", 2)] {
        t.send_body_and_header("direct:dups", b, "seq", s).unwrap();
    }
    assert_eq!(ctx.mock_endpoint("dups").unwrap().received_bodies(), vec![Value::from("b"), Value::from("a"), Value::from("c")]);
    ctx.stop().unwrap();
}

#[test]
fn resequence_stream_mode() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start").resequence(header("seq")).stream().timeout(Duration::from_millis(60)).to("mock:result").end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let start = Instant::now();
    for s in [3, 1, 2] {
        t.send_body_and_header("direct:start", s, "seq", s).unwrap();
    }
    let m = ctx.mock_endpoint("result").unwrap();
    m.expected_message_count(3);
    m.assert_is_satisfied_within(Duration::from_secs(2)).unwrap();
    assert_eq!(m.received_bodies(), ints(&[1, 2, 3]).as_array().unwrap().clone());
    assert!(start.elapsed() < Duration::from_millis(500));
    // in-order successors are delivered immediately
    t.send_body_and_header("direct:start", 4, "seq", 4).unwrap();
    assert_eq!(m.received_counter(), 4);
    // gap: 6 waits for timeout, then is delivered
    t.send_body_and_header("direct:start", 6, "seq", 6).unwrap();
    assert_eq!(m.received_counter(), 4);
    assert!(m.wait_for_messages(5, Duration::from_secs(2)));
    ctx.stop().unwrap();
}

#[test]
fn resequence_stream_reject_old_and_capacity() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .resequence(header("seq"))
            .stream()
            .capacity(2)
            .reject_old()
            .timeout(Duration::from_secs(5))
            .to("mock:result")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for s in [5, 7, 9] {
        t.send_body_and_header("direct:start", s, "seq", s).unwrap();
    }
    // capacity 2 exceeded: the oldest (5) is released
    let m = ctx.mock_endpoint("result").unwrap();
    assert_eq!(m.received_bodies(), ints(&[5]).as_array().unwrap().clone());
    let e = t.send_body_and_header("direct:start", 4, "seq", 4).unwrap_err();
    assert!(e.cause.unwrap().is_a("MessageRejectedException"));
    t.send_body_and_header("direct:start", 6, "seq", 6).unwrap();
    assert_eq!(m.received_bodies(), ints(&[5, 6, 7]).as_array().unwrap().clone());
    // remaining buffered exchanges are flushed on stop
    ctx.stop().unwrap();
    assert_eq!(m.received_bodies(), ints(&[5, 6, 7, 9]).as_array().unwrap().clone());
}

// ---------------------------------------------------------------------------
// sort
// ---------------------------------------------------------------------------

#[test]
fn sort_natural_and_custom() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:natural").sort(body().tokenize(","));
        b.from("direct:numbers").sort(body());
        b.from("direct:custom").sort_with(body(), |a, b| b.to_string().cmp(&a.to_string()));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    assert_eq!(t.request_body("direct:natural", "c,a,b").unwrap(), Value::from(vec!["a", "b", "c"]));
    assert_eq!(t.request_body("direct:numbers", ints(&[10, 9, 100])).unwrap(), ints(&[9, 10, 100]));
    assert_eq!(t.request_body("direct:custom", Value::from(vec!["a", "c", "b"])).unwrap(), Value::from(vec!["c", "b", "a"]));
}

// ---------------------------------------------------------------------------
// claimCheck
// ---------------------------------------------------------------------------

#[test]
fn claim_check_set_get() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .set_header("h1", constant("orig"))
            .claim_check(ClaimCheckOperation::Set, "k")
            .set_body(constant("changed"))
            .set_header("h1", constant("new"))
            .set_header("h2", constant("added"))
            .to("mock:between")
            .claim_check(ClaimCheckOperation::Get, "k")
            .to("mock:restored")
            .claim_check(ClaimCheckOperation::GetAndRemove, "k")
            .set_body(constant("again"))
            .claim_check(ClaimCheckOperation::Get, "k")
            .to("mock:end");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "original").unwrap();
    assert_eq!(ctx.mock_endpoint("between").unwrap().received_bodies(), vec![Value::from("changed")]);
    let r = &ctx.mock_endpoint("restored").unwrap().received_exchanges()[0];
    assert_eq!(r.body(), &Value::from("original"));
    assert_eq!(r.header("h1"), Some(&Value::from("orig")));
    assert_eq!(r.header("h2"), Some(&Value::from("added")));
    // removed: Get finds nothing
    assert_eq!(ctx.mock_endpoint("end").unwrap().received_bodies(), vec![Value::from("again")]);
}

#[test]
fn claim_check_push_pop_and_filter() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .set_header("foo", constant(1))
            .set_header("bar", constant(1))
            .claim_check(ClaimCheckOperation::Push, "")
            .set_body(constant("b2"))
            .set_header("foo", constant(2))
            .set_header("bar", constant(2))
            .claim_check(ClaimCheckOperation::Push, "")
            .set_body(constant("b3"))
            .claim_check_with_filter(ClaimCheckOperation::Pop, "", Some("header:foo"))
            .to("mock:pop1")
            .claim_check_with_filter(ClaimCheckOperation::Pop, "", Some("-header:bar"))
            .to("mock:pop2");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:start", "b1").unwrap();
    let p1 = &ctx.mock_endpoint("pop1").unwrap().received_exchanges()[0];
    assert_eq!(p1.body(), &Value::from("b3"));
    assert_eq!(p1.header("foo"), Some(&Value::from(2)));
    let p2 = &ctx.mock_endpoint("pop2").unwrap().received_exchanges()[0];
    assert_eq!(p2.body(), &Value::from("b1"));
    assert_eq!(p2.header("foo"), Some(&Value::from(1)));
    assert_eq!(p2.header("bar"), Some(&Value::from(2)));
}

#[test]
fn claim_check_is_scoped_per_exchange() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:set").claim_check(ClaimCheckOperation::Set, "k");
        b.from("direct:get").claim_check(ClaimCheckOperation::Get, "k");
        b.from("direct:nokey").claim_check(ClaimCheckOperation::Set, "");
    })
    .unwrap();
    assert!(ctx.start().is_err());
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:set").claim_check(ClaimCheckOperation::Set, "k");
        b.from("direct:get").claim_check(ClaimCheckOperation::Get, "k");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:set", "stored").unwrap();
    assert_eq!(t.request_body("direct:get", "other").unwrap(), Value::from("other"));
}

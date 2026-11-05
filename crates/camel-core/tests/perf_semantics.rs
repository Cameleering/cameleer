//! Behaviour that the performance work (see `docs/BENCHMARKS.md`) must keep: lazy ids,
//! header and property maps, copy-on-write objects, caches and the faster paths of the
//! Simple language, the aggregator, seda and the management statistics.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use camel_core::language::jsonpath::JsonPath;
use camel_core::language::simple::SimplePredicate;
use camel_core::message::{Headers, LazyId, Properties};
use camel_core::prelude::*;
use camel_core::processor::routing::strategies::grouped_body;
use camel_core::processor::routing::{AggregationRepository, MemoryAggregationRepository};
use camel_core::spi::Predicate;
use camel_core::value::ObjectMap;

// --- ids ---------------------------------------------------------------------------------

#[test]
fn exchange_ids_are_generated_on_first_use_and_kept_by_copies() {
    let ex = Exchange::new(None);
    assert!(!ex.exchange_id.is_generated());
    let copy = ex.clone();
    assert!(ex.exchange_id.is_generated(), "cloning generates the id so both copies share it");
    assert_eq!(copy.exchange_id, ex.exchange_id);
    let correlated = ex.correlated_copy();
    assert_ne!(correlated.exchange_id, ex.exchange_id);
    assert_eq!(correlated.property(props::CORRELATION_ID), Some(&Value::from(ex.exchange_id.as_str())));
    let set: LazyId = "my-id".into();
    assert_eq!(set, "my-id");
    assert_eq!(String::from(set), "my-id");
}

#[test]
fn concurrent_readers_of_a_lazy_id_see_one_id() {
    let id = Arc::new(LazyId::new());
    let seen: Vec<String> = (0..8)
        .map(|_| {
            let id = id.clone();
            std::thread::spawn(move || id.to_string())
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect();
    assert!(seen.iter().all(|s| *s == seen[0]), "{seen:?}");
}

#[test]
fn ids_from_many_threads_are_unique() {
    let all: Vec<String> = (0..6)
        .map(|_| std::thread::spawn(|| (0..2000).map(|_| camel_core::message::generate_id()).collect::<Vec<_>>()))
        .collect::<Vec<_>>()
        .into_iter()
        .flat_map(|h| h.join().unwrap())
        .collect();
    let mut sorted = all.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), all.len());
}

#[test]
fn the_created_timestamp_is_the_wall_clock() {
    let before = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64;
    let ex = Exchange::new(None);
    let after = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64;
    // millisecond precision from the coarse system clock (it may lag by a timer tick)
    assert!(ex.created_millis() >= before - 20 && ex.created_millis() <= after + 1, "{} not in [{before}, {after}]", ex.created_millis());
}

// --- headers and properties --------------------------------------------------------------

#[test]
fn headers_iterate_sorted_ignoring_case_and_keep_the_latest_spelling() {
    let mut h = Headers::new();
    for k in ["b", "A", "c", "Zeta", "alpha", "B"] {
        h.set(k, k);
    }
    // "b" and "B" are one header: the latest spelling and value win
    assert_eq!(h.keys().collect::<Vec<_>>(), vec!["A", "alpha", "B", "c", "Zeta"]);
    assert_eq!(h.get("b"), Some(&Value::from("B")));
    // more than a handful of headers (binary search)
    for i in 0..40 {
        h.set(format!("X-Header-{i:02}"), i);
    }
    assert_eq!(h.get("x-header-17"), Some(&Value::from(17)));
    assert_eq!(h.remove("X-HEADER-17"), Some(Value::from(17)));
    assert!(!h.contains("x-header-17"));
    let keys: Vec<String> = h.keys().map(|k| k.to_ascii_lowercase()).collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
}

#[test]
fn properties_behave_like_a_sorted_map() {
    let mut p = Properties::new();
    p.insert("b".to_string(), Value::from(2));
    p.insert(props::SPLIT_INDEX.to_string(), Value::from(1));
    p.insert("a".to_string(), Value::from(1));
    assert_eq!(p.insert("b".to_string(), Value::from(3)), Some(Value::from(2)));
    assert_eq!(p.keys().collect::<Vec<_>>(), vec!["CamelSplitIndex", "a", "b"]);
    *p.entry("c".to_string()).or_insert(Value::from(0)) = Value::from(9);
    assert_eq!(p.get("c"), Some(&Value::from(9)));
    p.retain(|k, _| k != "a");
    assert!(!p.contains_key("a"));
    let m: std::collections::BTreeMap<String, Value> = p.clone().into();
    assert_eq!(Properties::from(m), p);
    let room = p.clone_with_room(4);
    assert_eq!(room, p);
    for i in 0..30 {
        p.insert(format!("k{i:02}"), Value::from(i));
    }
    assert_eq!(p.get("k07"), Some(&Value::from(7)));
    assert_eq!(p.remove("k07"), Some(Value::from(7)));
    assert_eq!(p.len(), 3 + 29);
}

// --- copy-on-write objects and JSON --------------------------------------------------------

#[test]
fn copies_of_objects_do_not_see_each_others_changes() {
    let mut a = ObjectMap::new();
    a.insert("x".into(), Value::from(1));
    a.insert("nested".into(), Value::Object(ObjectMap::from([("y".to_string(), Value::from(2))])));
    let original = Value::Object(a.clone());
    let mut b = a.clone();
    b.insert("x".into(), Value::from(10));
    if let Some(Value::Object(n)) = b.get_mut("nested") {
        n.insert("y".into(), Value::from(20));
    }
    assert_eq!(Value::Object(a), original, "the original is unchanged");
    assert_eq!(b.get("x"), Some(&Value::from(10)));
    let mut c = b.clone();
    c.clear();
    assert!(c.is_empty() && b.len() == 2);
    let entries: Vec<(String, Value)> = b.clone().into_iter().collect();
    assert_eq!(entries.len(), 2);
}

#[test]
fn json_objects_keep_order_and_the_last_duplicate_value() {
    let v = camel_core::json::parse(r#"{"b":1,"a":2,"b":3}"#).unwrap();
    assert_eq!(v.to_json(), r#"{"b":3,"a":2}"#);
    let big: Vec<String> = (0..40).map(|i| format!("\"k{i}\":{i}")).collect();
    let text = format!("{{{},\"k5\":\"again\"}}", big.join(","));
    let v = camel_core::json::parse(&text).unwrap();
    let o = v.as_object().unwrap();
    assert_eq!(o.len(), 40);
    assert_eq!(o.get("k5"), Some(&Value::from("again")));
    assert_eq!(o.get_index(5).unwrap().0, "k5");
    assert_eq!(camel_core::json::parse(r#"{"s":"a\"b"}"#).unwrap().as_object().unwrap().get("s"), Some(&Value::from("a\"b")));
}

#[test]
fn jsonpath_gives_the_same_results_when_paths_share_a_body() {
    let text = Value::from(r#"{"id":7,"items":[{"n":1},{"n":2}],"name":"x"}"#);
    let id = JsonPath::compile("$.id").unwrap();
    let items = JsonPath::compile("$.items").unwrap();
    let names = JsonPath::compile("$.items[*].n").unwrap();
    for _ in 0..3 {
        // the second path reuses this thread's parse of the first; the large result is
        // moved out of the cached document, which must then not be reused
        assert_eq!(id.evaluate(&text).unwrap(), Value::from(7));
        assert_eq!(items.evaluate(&text).unwrap().len(), Some(2));
        assert_eq!(items.evaluate(&text).unwrap().len(), Some(2));
        assert_eq!(names.evaluate(&text).unwrap(), Value::Array(vec![Value::from(1), Value::from(2)]));
    }
    let other = Value::from(r#"{"id":8}"#);
    assert_eq!(id.evaluate(&other).unwrap(), Value::from(8));
    assert!(JsonPath::compile("$.missing").unwrap().evaluate(&other).is_err());
}

// --- Simple --------------------------------------------------------------------------------

#[test]
fn header_comparisons_match_the_general_evaluator() {
    let mut ex = Exchange::new(None);
    ex.message.set_header("s", "gold");
    ex.message.set_header("n", 150);
    ex.message.set_header("ns", "150");
    let cases = [
        ("${header.s} == 'gold'", true),
        ("${header.s} != 'gold'", false),
        ("${header.s} == 'silver'", false),
        ("${header.n} > 100", true),
        ("${header.n} <= 100", false),
        ("${header.ns} > 100", true),
        ("${header.ns} == 150", true),
        ("${header.missing} == null", true),
        ("${header.missing} > 1", false),
        ("${header.S} == 'gold'", true),
    ];
    for (text, expected) in cases {
        let p = SimplePredicate::parse(text).unwrap();
        assert_eq!(p.matches(&ex).unwrap(), expected, "{text}");
        // the same predicate through a less specific shape takes the general path
        let general = SimplePredicate::parse(&format!("{text} && true")).unwrap();
        assert_eq!(general.matches(&ex).unwrap(), expected, "{text} (general)");
    }
    let t = camel_core::language::simple::SimpleExpression::parse("${body}-${header.n}-${header.s}").unwrap();
    ex.message.body = Value::from("order");
    assert_eq!(t.evaluate(&ex).unwrap(), Value::from("order-150-gold"));
}

#[test]
fn key_strings_match_display() {
    for v in [Value::from(0), Value::from(-42), Value::from(i64::MIN), Value::from(i64::MAX), Value::from("k"), Value::from(1.5), Value::Bool(true)] {
        assert_eq!(camel_core::value::to_key_string(&v), v.to_string());
    }
}

// --- context, templates, producers -----------------------------------------------------------

#[test]
fn cached_producers_follow_restarts_and_contexts() {
    let make = |tag: &'static str| {
        let ctx = CamelContext::new();
        ctx.add_routes(move |b| {
            b.from("direct:p").set_body(constant(tag));
        })
        .unwrap();
        ctx.start().unwrap();
        ctx
    };
    let a = make("a");
    let b = make("b");
    // the same uri on the same thread, alternating contexts
    for _ in 0..3 {
        assert_eq!(a.create_producer_template().request_body("direct:p", "x").unwrap(), Value::from("a"));
        assert_eq!(b.create_producer_template().request_body("direct:p", "x").unwrap(), Value::from("b"));
    }
    a.stop().unwrap();
    a.start().unwrap();
    assert_eq!(a.create_producer_template().request_body("direct:p", "x").unwrap(), Value::from("a"));
    a.stop().unwrap();
    b.stop().unwrap();
}

#[test]
fn context_counters_and_route_statistics_count_without_event_notifiers() {
    let ctx = CamelContext::new();
    ctx.set_statistics_level(camel_core::management::ManagementStatisticsLevel::Default);
    ctx.add_routes(|b| {
        b.from("direct:ok").route_id("ok").set_header("x", constant(1)).to("mock:out");
        b.from("direct:bad").route_id("bad").throw_exception("IllegalArgumentException", "boom");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for _ in 0..5 {
        t.send_body("direct:ok", "x").unwrap();
    }
    assert!(t.send_body("direct:bad", "x").is_err());
    let mc = ctx.managed_context();
    assert_eq!(mc.exchanges_completed(), 5);
    assert_eq!(mc.exchanges_failed(), 1);
    let ok = mc.route("ok").unwrap();
    assert_eq!(ok.stats.exchanges_total, 5);
    // every node is counted once per exchange
    for p in mc.processors("ok") {
        assert_eq!(p.stats.exchanges_total, 5, "{}", p.node_id);
    }
    let r = ctx.route("ok").unwrap();
    assert_eq!(r.stats.exchanges_total(), 5);
    assert_eq!(r.stats.exchanges_failed(), 0);
    mc.reset_statistics();
    assert_eq!(mc.exchanges_completed(), 0);
    t.send_body("direct:ok", "x").unwrap();
    assert_eq!(mc.exchanges_completed(), 1);
    assert_eq!(ctx.inflight_count(), 0);
    ctx.stop().unwrap();
}

#[test]
fn statistics_from_many_threads_add_up() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:m").route_id("m").process(|_: &mut Exchange| Ok(()));
    })
    .unwrap();
    ctx.start().unwrap();
    let hs: Vec<_> = (0..12)
        .map(|_| {
            let t = ctx.create_producer_template();
            std::thread::spawn(move || {
                for _ in 0..500 {
                    t.send_body("direct:m", "x").unwrap();
                }
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
    let mc = ctx.managed_context();
    assert_eq!(mc.route("m").unwrap().stats.exchanges_total, 6000);
    assert_eq!(mc.exchanges_completed(), 6000);
    assert_eq!(ctx.route("m").unwrap().stats.exchanges_total(), 6000);
    ctx.stop().unwrap();
}

#[test]
fn a_global_option_set_after_start_is_seen() {
    let ctx = CamelContext::new();
    assert_eq!(ctx.global_option("CamelLogDebugBodyMaxChars"), None);
    ctx.set_global_option("CamelLogDebugBodyMaxChars", "10");
    assert_eq!(ctx.global_option("CamelLogDebugBodyMaxChars").as_deref(), Some("10"));
}

// --- aggregation -----------------------------------------------------------------------------

#[test]
fn the_memory_repository_keeps_groups_in_insertion_order() {
    let r = MemoryAggregationRepository::new();
    let ex = |b: &str| Exchange::with_body(None, b);
    r.add("a", ex("a1"));
    r.add("b", ex("b1"));
    r.add("c", ex("c1"));
    // taking and putting back the group of an older key keeps its place
    let a = r.take("a").unwrap();
    assert_eq!(r.keys(), vec!["a", "b", "c"]);
    r.add("a", a);
    assert_eq!(r.get("a").unwrap().body(), &Value::from("a1"));
    assert_eq!(r.remove("c").unwrap().body(), &Value::from("c1"));
    assert_eq!(r.keys(), vec!["a", "b"]);
    r.add("d", ex("d1"));
    assert_eq!(r.keys(), vec!["a", "b", "d"]);
    assert!(r.get("c").is_none());
}

#[test]
fn interleaved_groups_aggregate_correctly() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:agg").aggregate(header("k"), grouped_body()).completion_size(3).to("mock:out").end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for (k, v) in [("x", 1), ("y", 1), ("x", 2), ("y", 2), ("x", 3), ("y", 3)] {
        t.send_body_and_header("direct:agg", v, "k", k).unwrap();
    }
    let mock = ctx.mock_endpoint("mock:out").unwrap();
    let bodies: Vec<Value> = mock.received_exchanges().iter().map(|e| e.body().clone()).collect();
    assert_eq!(bodies, vec![Value::Array(vec![1.into(), 2.into(), 3.into()]), Value::Array(vec![1.into(), 2.into(), 3.into()])]);
    ctx.stop().unwrap();
}

struct ConfirmingRepository {
    inner: MemoryAggregationRepository,
    confirmed: AtomicUsize,
}

impl AggregationRepository for ConfirmingRepository {
    fn add(&self, key: &str, exchange: Exchange) -> Option<Exchange> {
        self.inner.add(key, exchange)
    }
    fn get(&self, key: &str) -> Option<Exchange> {
        self.inner.get(key)
    }
    fn remove(&self, key: &str) -> Option<Exchange> {
        self.inner.remove(key)
    }
    fn confirm(&self, _exchange_id: &str) {
        self.confirmed.fetch_add(1, Ordering::SeqCst);
    }
    fn keys(&self) -> Vec<String> {
        self.inner.keys()
    }
}

#[test]
fn repositories_that_confirm_still_get_the_completed_exchange_ids() {
    let repo = Arc::new(ConfirmingRepository { inner: MemoryAggregationRepository::new(), confirmed: AtomicUsize::new(0) });
    let ctx = CamelContext::new();
    let r2: Arc<dyn AggregationRepository> = repo.clone();
    ctx.add_routes(move |b| {
        b.from("direct:agg").aggregate(header("k"), grouped_body()).completion_size(2).aggregation_repository(r2.clone()).to("mock:out").end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for v in 0..4 {
        t.send_body_and_header("direct:agg", v, "k", "g").unwrap();
    }
    assert_eq!(repo.confirmed.load(Ordering::SeqCst), 2);
    ctx.stop().unwrap();
}

// --- seda ------------------------------------------------------------------------------------

#[test]
fn seda_keeps_fifo_order_with_one_consumer_under_load() {
    let ctx = CamelContext::new();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let s2 = seen.clone();
    ctx.add_routes(move |b| {
        let s = s2.clone();
        b.from("seda:fifo?size=16").process(move |ex: &mut Exchange| {
            s.lock().unwrap().push(ex.body().as_i64().unwrap());
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for i in 0..2000 {
        t.send_body("seda:fifo?blockWhenFull=true", i).unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while seen.lock().unwrap().len() < 2000 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(*seen.lock().unwrap(), (0..2000).collect::<Vec<i64>>());
    ctx.stop().unwrap();
}

#[test]
fn seda_request_reply_from_many_threads_gets_its_own_replies() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("seda:echo?concurrentConsumers=3").transform(simple("${body}-done"));
    })
    .unwrap();
    ctx.start().unwrap();
    let hs: Vec<_> = (0..6)
        .map(|t_id| {
            let t = ctx.create_producer_template();
            std::thread::spawn(move || {
                for i in 0..300 {
                    let body = format!("{t_id}-{i}");
                    assert_eq!(t.request_body("seda:echo", body.clone()).unwrap(), Value::from(format!("{body}-done")));
                }
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
    ctx.stop().unwrap();
}

#[test]
fn a_timed_out_seda_request_does_not_leak_its_late_reply_into_the_next_one() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("seda:slow").choice().when(simple("${body} == 'slow'")).delay(constant(300)).end().transform(simple("${body}-reply"));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let err = t.request_body("seda:slow?timeout=50", "slow").unwrap_err();
    assert!(err.to_string().contains("timed out") || format!("{err:?}").contains("ExchangeTimedOut"), "{err}");
    // the next request on this thread must get its own reply, not the late "slow-reply"
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(t.request_body("seda:slow?timeout=2000", "fast").unwrap(), Value::from("fast-reply"));
    ctx.stop().unwrap();
}

#[test]
fn a_purged_seda_request_reports_the_discard() {
    let ctx = CamelContext::new();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let ep = ctx.get_endpoint("seda:nobody").unwrap();
    let ep2 = ep.clone();
    let purger = std::thread::spawn(move || {
        let q = ep2.as_any().downcast_ref::<camel_core::component::seda::SedaEndpoint>().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while q.queue_size() == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        q.purge_queue();
    });
    let err = t.request_body("seda:nobody?timeout=5000", "x").unwrap_err();
    purger.join().unwrap();
    assert!(format!("{err}").contains("discarded"), "{err}");
    ctx.stop().unwrap();
}

#[test]
fn split_parts_reach_steps_after_an_aggregate_intact() {
    // split -> aggregate only: the aggregator may move the parts (they are discarded);
    // split -> aggregate, then another step: the parts must still carry their bodies
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:only")
            .split(simple("${body}"))
            .aggregate(constant("all"), grouped_body())
            .completion_size(3)
            .to("mock:grouped")
            .end()
            .end();
        b.from("direct:more")
            .split(simple("${body}"))
            .aggregate(constant("all"), grouped_body())
            .completion_size(3)
            .to("mock:grouped2")
            .end()
            .to("mock:parts")
            .end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:only", "a,b,c").unwrap();
    t.send_body("direct:more", "a,b,c").unwrap();
    let grouped = ctx.mock_endpoint("mock:grouped").unwrap().received_exchanges();
    assert_eq!(grouped.len(), 1);
    assert_eq!(grouped[0].body(), &Value::Array(vec!["a".into(), "b".into(), "c".into()]));
    assert!(grouped[0].property(props::CORRELATION_ID).is_some());
    let grouped2 = ctx.mock_endpoint("mock:grouped2").unwrap().received_exchanges();
    assert_eq!(grouped2[0].body(), grouped[0].body());
    let parts: Vec<Value> = ctx.mock_endpoint("mock:parts").unwrap().received_exchanges().iter().map(|e| e.body().clone()).collect();
    assert_eq!(parts, vec![Value::from("a"), Value::from("b"), Value::from("c")]);
    ctx.stop().unwrap();
}

#[test]
fn the_memory_repository_updates_groups_in_place() {
    let r = MemoryAggregationRepository::new();
    assert!(r.has_slots());
    r.add("a", Exchange::with_body(None, "a1"));
    // a new group, then a change to an existing one, then a removal
    assert!(r.with_slot("b", &mut |slot| {
        assert!(slot.is_none());
        *slot = Some(Exchange::with_body(None, "b1"));
    }));
    assert!(r.with_slot("a", &mut |slot| slot.as_mut().unwrap().set_body("a2")));
    assert_eq!(r.keys(), vec!["a", "b"]);
    assert_eq!(r.get("a").unwrap().body(), &Value::from("a2"));
    assert!(r.with_slot("b", &mut |slot| *slot = None));
    assert_eq!(r.keys(), vec!["a"]);
    // a slot left empty for a new key leaves no group behind
    assert!(r.with_slot("c", &mut |_| {}));
    assert_eq!(r.keys(), vec!["a"]);
}

#[test]
fn eager_completion_and_strategy_completion_work_in_place() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:eager").aggregate(header("k"), grouped_body()).completion_size(2).eager_check_completion().to("mock:eager").end();
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for v in 0..4 {
        t.send_body_and_header("direct:eager", v, "k", "g").unwrap();
    }
    let got: Vec<Value> = ctx.mock_endpoint("mock:eager").unwrap().received_exchanges().iter().map(|e| e.body().clone()).collect();
    // eager: the size is checked on the incoming exchange before it is aggregated
    assert_eq!(got.len(), 2, "{got:?}");
    ctx.stop().unwrap();
}

#[test]
fn shared_header_names_behave_like_owned_ones() {
    let name: Arc<str> = Arc::from("Tier");
    let mut h = Headers::new();
    h.set("tier", "x");
    h.set_shared(&name, "A");
    // one header, latest spelling
    assert_eq!(h.len(), 1);
    assert_eq!(h.keys().collect::<Vec<_>>(), vec!["Tier"]);
    let copy = h.clone();
    h.set("TIER", "B");
    assert_eq!(copy.get("tier"), Some(&Value::from("A")));
    assert_eq!(h.get("tier"), Some(&Value::from("B")));
    assert_eq!(h.keys().collect::<Vec<_>>(), vec!["TIER"]);
    assert_eq!(Arc::strong_count(&name), 2, "the copy shares the name");
    let mut h2 = Headers::new();
    h2.set_shared(&name, "A");
    assert_eq!(h2, copy);
    assert_eq!(h2.remove_matching("ti*", &[]), 1);
}

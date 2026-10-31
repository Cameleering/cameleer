//! YAML DSL coverage of the Enterprise Integration Patterns (one or more tests per EIP),
//! mirroring the examples of the Camel YAML DSL documentation.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use camel_core::prelude::*;
use camel_core::processor::errors::misc::Policy;
use camel_core::spi::AggregationStrategy;
use camel_yaml::*;

fn started(yaml: &str) -> CamelContext {
    let ctx = CamelContext::new();
    start_with(&ctx, yaml);
    ctx
}

fn start_with(ctx: &CamelContext, yaml: &str) {
    add_routes_from_yaml(ctx, yaml).unwrap_or_else(|e| panic!("load failed: {}", e));
    ctx.start().unwrap();
}

fn mock(ctx: &CamelContext, uri: &str) -> camel_core::component::mock::MockEndpoint {
    ctx.mock_endpoint(uri).unwrap()
}

fn send(ctx: &CamelContext, uri: &str, body: impl Into<Value>) {
    ctx.create_producer_template().send_body(uri, body).unwrap();
}

fn send_h(ctx: &CamelContext, uri: &str, body: impl Into<Value>, k: &str, v: impl Into<Value>) {
    ctx.create_producer_template().send_body_and_header(uri, body, k, v).unwrap();
}

fn root_error(e: &CamelError) -> &CamelError {
    let mut cur = e;
    while cur.kind == "CamelExecutionException" {
        match &cur.cause {
            Some(c) => cur = &**c,
            None => break,
        }
    }
    cur
}

fn root_kind(e: &CamelError) -> String {
    root_error(e).kind.clone()
}

/// Concatenates bodies with `+` (old + new).
struct Concat;
impl AggregationStrategy for Concat {
    fn aggregate(&self, old: Option<Exchange>, mut new: Exchange) -> Result<Exchange> {
        if let Some(o) = old {
            let b = format!("{}+{}", o.body(), new.body());
            new.set_body(b);
        }
        Ok(new)
    }
}

// ---------------------------------------------------------------------------
// filter / choice / pipeline
// ---------------------------------------------------------------------------

#[test]
fn filter_inline_and_expression_forms() {
    let ctx = started(
        r##"
- from:
    uri: direct:filter
    steps:
      - filter:
          simple: "${header.ok}"
          steps:
            - to: mock:filtered
      - to: mock:after
- from:
    uri: direct:filter2
    steps:
      - filter:
          expression:
            header: ok
          steps:
            - to: mock:filtered2
"##,
    );
    let m = mock(&ctx, "mock:filtered");
    m.expected_bodies_received(vec!["yes"]);
    mock(&ctx, "mock:after").expected_message_count(2);
    mock(&ctx, "mock:filtered2").expected_bodies_received(vec!["b"]);
    send_h(&ctx, "direct:filter", "yes", "ok", true);
    send_h(&ctx, "direct:filter", "no", "ok", false);
    send_h(&ctx, "direct:filter2", "a", "ok", false);
    send_h(&ctx, "direct:filter2", "b", "ok", true);
    m.assert_is_satisfied().unwrap();
    mock(&ctx, "mock:after").assert_is_satisfied().unwrap();
    mock(&ctx, "mock:filtered2").assert_is_satisfied().unwrap();
}

#[test]
fn choice_when_otherwise() {
    let ctx = started(
        r##"
- from:
    uri: direct:choice
    steps:
      - choice:
          id: myChoice
          when:
            - simple: "${header.gold}"
              id: goldWhen
              steps:
                - to: mock:gold
            - expression:
                header: silver
              steps:
                - to: mock:silver
          otherwise:
            steps:
              - to: mock:other
      - to: mock:end
"##,
    );
    mock(&ctx, "mock:gold").expected_bodies_received(vec!["g"]);
    mock(&ctx, "mock:silver").expected_bodies_received(vec!["s"]);
    mock(&ctx, "mock:other").expected_bodies_received(vec!["o"]);
    mock(&ctx, "mock:end").expected_message_count(3);
    send_h(&ctx, "direct:choice", "g", "gold", true);
    send_h(&ctx, "direct:choice", "s", "silver", true);
    send(&ctx, "direct:choice", "o");
    for m in ["mock:gold", "mock:silver", "mock:other", "mock:end"] {
        mock(&ctx, m).assert_is_satisfied().unwrap();
    }
    let dump = ctx.route(&ctx.route_ids()[0]).unwrap().definition.lock().unwrap().as_ref().unwrap().dump();
    assert!(dump.contains("choice"), "{}", dump);
}

#[test]
fn choice_requires_when() {
    let ctx = CamelContext::new();
    let e = load_routes(&ctx, "- from:\n    uri: direct:x\n    steps:\n      - choice:\n          otherwise:\n            steps: [{to: mock:x}]\n").unwrap_err();
    assert!(e.message.contains("at least one 'when'"), "{}", e);
    assert!(e.message.contains("line 4"), "{}", e);
}

#[test]
fn pipeline_block() {
    let ctx = started(
        r##"
- from:
    uri: direct:pipe
    steps:
      - pipeline:
          steps:
            - setBody: {simple: "${body}-1"}
            - to: mock:p1
      - to: mock:p2
"##,
    );
    mock(&ctx, "mock:p1").expected_bodies_received(vec!["x-1"]);
    mock(&ctx, "mock:p2").expected_bodies_received(vec!["x-1"]);
    send(&ctx, "direct:pipe", "x");
    mock(&ctx, "mock:p1").assert_is_satisfied().unwrap();
    mock(&ctx, "mock:p2").assert_is_satisfied().unwrap();
}

// ---------------------------------------------------------------------------
// split / aggregate / multicast
// ---------------------------------------------------------------------------

#[test]
fn split_inline_expression_with_strategy() {
    let ctx = started(
        r##"
- from:
    uri: direct:split
    steps:
      - split:
          simple: "${body}"
          aggregationStrategy: "#class:org.apache.camel.processor.aggregate.GroupedBodyAggregationStrategy"
          stopOnException: true
          steps:
            - setBody: {simple: "<${body}>"}
            - to: mock:part
      - to: mock:joined
"##,
    );
    mock(&ctx, "mock:part").expected_bodies_received(vec!["<a>", "<b>", "<c>"]);
    mock(&ctx, "mock:joined").expected_bodies_received(vec![Value::Array(vec!["<a>".into(), "<b>".into(), "<c>".into()])]);
    send(&ctx, "direct:split", "a,b,c");
    mock(&ctx, "mock:part").assert_is_satisfied().unwrap();
    mock(&ctx, "mock:joined").assert_is_satisfied().unwrap();
}

#[test]
fn split_expression_form_parallel_with_registry_strategy() {
    let ctx = CamelContext::new();
    ctx.registry().bind_aggregation_strategy("concat", Concat);
    start_with(
        &ctx,
        r##"
- from:
    uri: direct:split
    steps:
      - split:
          expression:
            simple: "${body}"
          delimiter: ";"
          parallelProcessing: true
          aggregationStrategy: "#concat"
          timeout: 5000
          steps:
            - to: mock:part
      - to: mock:joined
"##,
    );
    mock(&ctx, "mock:part").expected_bodies_received_in_any_order(vec!["1", "2"]);
    mock(&ctx, "mock:joined").expected_message_count(1);
    send(&ctx, "direct:split", "1;2");
    mock(&ctx, "mock:part").assert_is_satisfied().unwrap();
    mock(&ctx, "mock:joined").assert_is_satisfied().unwrap();
    let b = mock(&ctx, "mock:joined").received_bodies()[0].to_string();
    assert!(b == "1+2" || b == "2+1", "{}", b);
}

#[test]
fn split_tokenize_shorthand_builds_definition() {
    let ctx = CamelContext::new();
    let routes = load_routes(&ctx, "- from:\n    uri: direct:t\n    steps:\n      - split:\n          tokenize: \",\"\n          steps: [{to: mock:t}]\n").unwrap();
    let dump = routes[0].dump();
    assert!(dump.contains("tokenize") && dump.contains("token"), "{}", dump);
}

#[test]
fn aggregate_with_completion_size_and_registry_strategy() {
    let ctx = CamelContext::new();
    ctx.registry().bind_aggregation_strategy("myStrategy", Concat);
    start_with(
        &ctx,
        r##"
- route:
    from:
      uri: direct:agg
      steps:
        - aggregate:
            correlationExpression:
              header: id
            aggregationStrategy: myStrategy
            completionSize: 2
            steps:
              - to: mock:aggregated
"##,
    );
    let m = mock(&ctx, "mock:aggregated");
    m.expected_bodies_received_in_any_order(vec!["a+b", "c+d"]);
    send_h(&ctx, "direct:agg", "a", "id", 1);
    send_h(&ctx, "direct:agg", "c", "id", 2);
    send_h(&ctx, "direct:agg", "b", "id", 1);
    send_h(&ctx, "direct:agg", "d", "id", 2);
    m.assert_is_satisfied().unwrap();
}

#[test]
fn aggregate_completion_predicate_timeout_and_builtin_strategy() {
    let ctx = started(
        r##"
- route:
    from:
      uri: direct:agg
      steps:
        - aggregate:
            correlationExpression:
              constant: all
            aggregationStrategy: "#class:org.apache.camel.processor.aggregate.GroupedBodyAggregationStrategy"
            completionPredicate:
              simple: "${header.last}"
            eagerCheckCompletion: true
            steps:
              - to: mock:pred
- route:
    from:
      uri: direct:aggTimeout
      steps:
        - aggregate:
            correlationExpression:
              header: id
            aggregationStrategy: UseLatestAggregationStrategy
            completionTimeout: 100
            forceCompletionOnStop: true
            ignoreInvalidCorrelationKeys: true
            closeCorrelationKeyOnCompletion: 100
            steps:
              - to: mock:timeout
"##,
    );
    let m = mock(&ctx, "mock:pred");
    m.expected_bodies_received(vec![Value::Array(vec!["1".into(), "2".into(), "3".into()])]);
    send(&ctx, "direct:agg", "1");
    send(&ctx, "direct:agg", "2");
    send_h(&ctx, "direct:agg", "3", "last", true);
    m.assert_is_satisfied().unwrap();

    let t = mock(&ctx, "mock:timeout");
    t.expected_bodies_received(vec!["y"]);
    send_h(&ctx, "direct:aggTimeout", "x", "id", "k");
    send_h(&ctx, "direct:aggTimeout", "y", "id", "k");
    send(&ctx, "direct:aggTimeout", "ignored-no-key");
    t.assert_is_satisfied_within(Duration::from_secs(5)).unwrap();
}

#[test]
fn aggregate_errors() {
    let ctx = CamelContext::new();
    let e = load_routes(&ctx, "- from:\n    uri: direct:x\n    steps:\n      - aggregate:\n          correlationExpression: {header: id}\n          completionSize: 2\n").unwrap_err();
    assert!(e.message.contains("missing required option 'aggregationStrategy'"), "{}", e);
    let e = load_routes(
        &ctx,
        "- from:\n    uri: direct:x\n    steps:\n      - aggregate:\n          correlationExpression: {header: id}\n          aggregationStrategy: '#nope'\n          completionSize: 2\n",
    )
    .unwrap_err();
    assert!(e.message.contains("No AggregationStrategy named 'nope'"), "{}", e);
    assert!(e.message.contains("line 4"), "{}", e);
}

#[test]
fn multicast_with_strategy() {
    let ctx = CamelContext::new();
    ctx.registry().bind_aggregation_strategy("concat", Concat);
    start_with(
        &ctx,
        r##"
- from:
    uri: direct:multi
    steps:
      - multicast:
          aggregationStrategy: concat
          steps:
            - setBody: {simple: "A${body}"}
            - setBody: {simple: "B${body}"}
      - to: mock:multi
- from:
    uri: direct:multi2
    steps:
      - multicast:
          parallelProcessing: true
          steps:
            - to: mock:m1
            - to: mock:m2
"##,
    );
    mock(&ctx, "mock:multi").expected_bodies_received(vec!["Ax+Bx"]);
    send(&ctx, "direct:multi", "x");
    mock(&ctx, "mock:multi").assert_is_satisfied().unwrap();
    mock(&ctx, "mock:m1").expected_bodies_received(vec!["y"]);
    mock(&ctx, "mock:m2").expected_bodies_received(vec!["y"]);
    send(&ctx, "direct:multi2", "y");
    mock(&ctx, "mock:m1").assert_is_satisfied().unwrap();
    mock(&ctx, "mock:m2").assert_is_satisfied().unwrap();
}

// ---------------------------------------------------------------------------
// recipientList / routingSlip / dynamicRouter / wireTap / enrich
// ---------------------------------------------------------------------------

#[test]
fn recipient_list_routing_slip_and_dynamic_router() {
    let ctx = started(
        r##"
- from:
    uri: direct:rl
    steps:
      - recipientList:
          header: recipients
          delimiter: ";"
          ignoreInvalidEndpoints: true
- from:
    uri: direct:slip
    steps:
      - routingSlip:
          expression:
            header: slip
          uriDelimiter: "#"
- from:
    uri: direct:dyn
    steps:
      - dynamicRouter:
          header: next
- from:
    uri: direct:hop
    steps:
      - setBody: {simple: "${body}!"}
      - removeHeader: next
      - to: mock:hop
"##,
    );
    mock(&ctx, "mock:r1").expected_bodies_received(vec!["x"]);
    mock(&ctx, "mock:r2").expected_bodies_received(vec!["x"]);
    send_h(&ctx, "direct:rl", "x", "recipients", "mock:r1;mock:r2");
    mock(&ctx, "mock:r1").assert_is_satisfied().unwrap();
    mock(&ctx, "mock:r2").assert_is_satisfied().unwrap();

    mock(&ctx, "mock:s2").expected_bodies_received(vec!["y!"]);
    send_h(&ctx, "direct:slip", "y", "slip", "direct:hop#mock:s2");
    mock(&ctx, "mock:s2").assert_is_satisfied().unwrap();

    let hop = mock(&ctx, "mock:hop");
    hop.reset();
    hop.expected_bodies_received(vec!["z!"]);
    send_h(&ctx, "direct:dyn", "z", "next", "direct:hop");
    hop.assert_is_satisfied().unwrap();
}

#[test]
fn wire_tap_with_parameters_and_new_body() {
    let ctx = started(
        r##"
- from:
    uri: direct:tap
    steps:
      - wireTap:
          uri: seda:tapped
          parameters:
            size: 10
      - wireTap:
          uri: mock:tap2
          body:
            constant: tapped-body
          setHeader:
            - name: tapped
              constant: "yes"
      - to: mock:main
- from:
    uri: seda:tapped
    steps:
      - to: mock:tap1
"##,
    );
    mock(&ctx, "mock:main").expected_bodies_received(vec!["m"]);
    mock(&ctx, "mock:tap1").expected_bodies_received(vec!["m"]);
    let t2 = mock(&ctx, "mock:tap2");
    t2.expected_bodies_received(vec!["tapped-body"]);
    t2.expected_header_received("tapped", "yes");
    send(&ctx, "direct:tap", "m");
    for m in ["mock:main", "mock:tap1", "mock:tap2"] {
        mock(&ctx, m).assert_is_satisfied().unwrap();
    }
}

#[test]
fn enrich_and_poll_enrich() {
    let ctx = CamelContext::new();
    ctx.registry().bind_aggregation_strategy("concat", Concat);
    start_with(
        &ctx,
        r##"
- from:
    uri: direct:enrich
    steps:
      - enrich:
          expression:
            constant: direct:resource
          aggregationStrategy: "#concat"
      - to: mock:enriched
- from:
    uri: direct:enrichUri
    steps:
      - enrich:
          uri: direct:resource
      - to: mock:enrichedUri
- from:
    uri: direct:resource
    steps:
      - setBody: {constant: res}
- from:
    uri: direct:poll
    steps:
      - pollEnrich:
          uri: seda:inbox
          timeout: 2000
          aggregationStrategy: concat
      - to: mock:polled
"##,
    );
    mock(&ctx, "mock:enriched").expected_bodies_received(vec!["orig+res"]);
    send(&ctx, "direct:enrich", "orig");
    mock(&ctx, "mock:enriched").assert_is_satisfied().unwrap();
    mock(&ctx, "mock:enrichedUri").expected_bodies_received(vec!["res"]);
    send(&ctx, "direct:enrichUri", "orig");
    mock(&ctx, "mock:enrichedUri").assert_is_satisfied().unwrap();
    send(&ctx, "seda:inbox", "mail");
    mock(&ctx, "mock:polled").expected_bodies_received(vec!["me+mail"]);
    send(&ctx, "direct:poll", "me");
    mock(&ctx, "mock:polled").assert_is_satisfied().unwrap();
}

// ---------------------------------------------------------------------------
// loop / loadBalance / resequence / sort / claimCheck
// ---------------------------------------------------------------------------

#[test]
fn loop_count_copy_and_do_while() {
    let ctx = started(
        r##"
- from:
    uri: direct:loop
    steps:
      - loop:
          constant: 3
          copy: true
          steps:
            - setBody: {simple: "${body}x"}
            - to: mock:loop
- from:
    uri: direct:while
    steps:
      - setHeader: {name: more, constant: "true"}
      - loop:
          expression:
            simple: "${header.more}"
          doWhile: true
          steps:
            - to: mock:while
            - setHeader: {name: more, constant: "false"}
"##,
    );
    mock(&ctx, "mock:loop").expected_bodies_received(vec!["ax", "ax", "ax"]);
    send(&ctx, "direct:loop", "a");
    mock(&ctx, "mock:loop").assert_is_satisfied().unwrap();
    mock(&ctx, "mock:while").expected_message_count(1);
    send(&ctx, "direct:while", "w");
    mock(&ctx, "mock:while").assert_is_satisfied().unwrap();
}

#[test]
fn load_balancers() {
    let ctx = started(
        r##"
- from:
    uri: direct:rr
    steps:
      - loadBalance:
          roundRobin: {}
          steps:
            - to: mock:rr1
            - to: mock:rr2
- from:
    uri: direct:weighted
    steps:
      - loadBalance:
          weighted:
            distributionRatio: "2,1"
            roundRobin: true
          steps:
            - to: mock:w1
            - to: mock:w2
- from:
    uri: direct:topic
    steps:
      - loadBalance:
          topicLoadBalancer: {}
          steps:
            - to: mock:t1
            - to: mock:t2
- from:
    uri: direct:failover
    steps:
      - loadBalance:
          failover:
            exception:
              - java.lang.IllegalStateException
            roundRobin: false
          steps:
            - to: direct:broken
            - to: mock:fo
- from:
    uri: direct:broken
    steps:
      - throwException:
          exceptionType: java.lang.IllegalStateException
          message: broken
- from:
    uri: direct:sticky
    steps:
      - loadBalance:
          sticky:
            correlationExpression:
              header: user
          steps:
            - to: mock:st1
            - to: mock:st2
- from:
    uri: direct:random
    steps:
      - loadBalance:
          random: {}
          steps:
            - to: mock:rnd
            - to: mock:rnd
"##,
    );
    mock(&ctx, "mock:rr1").expected_bodies_received(vec![1, 3]);
    mock(&ctx, "mock:rr2").expected_bodies_received(vec![2, 4]);
    for i in 1..=4 {
        send(&ctx, "direct:rr", i);
    }
    mock(&ctx, "mock:rr1").assert_is_satisfied().unwrap();
    mock(&ctx, "mock:rr2").assert_is_satisfied().unwrap();

    mock(&ctx, "mock:w1").expected_message_count(4);
    mock(&ctx, "mock:w2").expected_message_count(2);
    for i in 0..6 {
        send(&ctx, "direct:weighted", i);
    }
    mock(&ctx, "mock:w1").assert_is_satisfied().unwrap();
    mock(&ctx, "mock:w2").assert_is_satisfied().unwrap();

    mock(&ctx, "mock:t1").expected_bodies_received(vec!["t"]);
    mock(&ctx, "mock:t2").expected_bodies_received(vec!["t"]);
    send(&ctx, "direct:topic", "t");
    mock(&ctx, "mock:t1").assert_is_satisfied().unwrap();
    mock(&ctx, "mock:t2").assert_is_satisfied().unwrap();

    mock(&ctx, "mock:fo").expected_bodies_received(vec!["f"]);
    send(&ctx, "direct:failover", "f");
    mock(&ctx, "mock:fo").assert_is_satisfied().unwrap();

    for (u, b) in [("a", 1), ("b", 2), ("a", 3), ("b", 4)] {
        send_h(&ctx, "direct:sticky", b, "user", u);
    }
    let s1 = mock(&ctx, "mock:st1").received_bodies();
    let s2 = mock(&ctx, "mock:st2").received_bodies();
    assert_eq!(s1.len() + s2.len(), 4);
    assert!(s1 == vec![Value::from(1), Value::from(3)] || s1 == vec![Value::from(2), Value::from(4)], "{:?}", s1);

    mock(&ctx, "mock:rnd").expected_message_count(3);
    for i in 0..3 {
        send(&ctx, "direct:random", i);
    }
    mock(&ctx, "mock:rnd").assert_is_satisfied().unwrap();
}

#[test]
fn load_balancer_errors() {
    let ctx = CamelContext::new();
    let e = load_routes(&ctx, "- from:\n    uri: direct:x\n    steps:\n      - loadBalance:\n          bogus: {}\n          steps: [{to: mock:a}]\n").unwrap_err();
    assert!(e.message.contains("unknown load balancer 'bogus'"), "{}", e);
    let e = load_routes(&ctx, "- from:\n    uri: direct:x\n    steps:\n      - loadBalance:\n          random: {}\n          topic: {}\n").unwrap_err();
    assert!(e.message.contains("only one load balancer"), "{}", e);
}

#[test]
fn resequence_batch_and_stream() {
    let ctx = started(
        r##"
- from:
    uri: direct:reseq
    steps:
      - resequence:
          header: seq
          batchConfig:
            batchSize: 3
            batchTimeout: 200
          steps:
            - to: mock:reseq
- from:
    uri: direct:stream
    steps:
      - resequence:
          expression:
            header: seq
          streamConfig:
            capacity: 10
            timeout: 200
          steps:
            - to: mock:stream
"##,
    );
    let m = mock(&ctx, "mock:reseq");
    m.expected_bodies_received(vec!["a", "b", "c"]);
    send_h(&ctx, "direct:reseq", "c", "seq", 3);
    send_h(&ctx, "direct:reseq", "a", "seq", 1);
    send_h(&ctx, "direct:reseq", "b", "seq", 2);
    m.assert_is_satisfied_within(Duration::from_secs(5)).unwrap();
    let s = mock(&ctx, "mock:stream");
    s.expected_bodies_received(vec!["x", "y"]);
    send_h(&ctx, "direct:stream", "y", "seq", 2);
    send_h(&ctx, "direct:stream", "x", "seq", 1);
    s.assert_is_satisfied_within(Duration::from_secs(5)).unwrap();
}

#[test]
fn sort_and_claim_check() {
    let ctx = started(
        r##"
- from:
    uri: direct:sort
    steps:
      - sort:
          simple: "${body}"
      - to: mock:sorted
- from:
    uri: direct:claim
    steps:
      - claimCheck:
          operation: Push
      - setBody: {constant: changed}
      - to: mock:changed
      - claimCheck:
          operation: Pop
      - claimCheck:
          operation: Set
          key: k1
      - setBody: {constant: again}
      - claimCheck:
          operation: Get
          key: k1
          filter: body
      - to: mock:restored
"##,
    );
    mock(&ctx, "mock:sorted").expected_bodies_received(vec![Value::Array(vec![1.into(), 2.into(), 3.into()])]);
    send(&ctx, "direct:sort", Value::Array(vec![3.into(), 1.into(), 2.into()]));
    mock(&ctx, "mock:sorted").assert_is_satisfied().unwrap();
    mock(&ctx, "mock:changed").expected_bodies_received(vec!["changed"]);
    mock(&ctx, "mock:restored").expected_bodies_received(vec!["original"]);
    send(&ctx, "direct:claim", "original");
    mock(&ctx, "mock:changed").assert_is_satisfied().unwrap();
    mock(&ctx, "mock:restored").assert_is_satisfied().unwrap();
}

// ---------------------------------------------------------------------------
// doTry / throwException / validate / rollback
// ---------------------------------------------------------------------------

#[test]
fn do_try_catch_on_when_and_finally() {
    let ctx = started(
        r##"
- from:
    uri: direct:try
    steps:
      - doTry:
          steps:
            - to: mock:try
            - throwException:
                exceptionType: java.lang.IllegalArgumentException
                message: "bad ${body}"
          doCatch:
            - exception:
                - java.io.IOException
              steps:
                - to: mock:io
            - exception:
                - java.lang.IllegalArgumentException
              onWhen:
                simple: "${header.special}"
              steps:
                - to: mock:special
            - exception:
                - java.lang.Exception
              steps:
                - setBody: {simple: "caught ${body}"}
                - to: mock:catch
          doFinally:
            steps:
              - to: mock:finally
      - to: mock:result
"##,
    );
    mock(&ctx, "mock:try").expected_message_count(2);
    mock(&ctx, "mock:io").expected_message_count(0);
    mock(&ctx, "mock:special").expected_bodies_received(vec!["s"]);
    mock(&ctx, "mock:catch").expected_bodies_received(vec!["caught n"]);
    mock(&ctx, "mock:finally").expected_message_count(2);
    mock(&ctx, "mock:result").expected_message_count(2);
    send_h(&ctx, "direct:try", "s", "special", true);
    send(&ctx, "direct:try", "n");
    for m in ["mock:try", "mock:io", "mock:special", "mock:catch", "mock:finally", "mock:result"] {
        mock(&ctx, m).assert_is_satisfied().unwrap();
    }
}

#[test]
fn validate_rollback_and_throw_exception() {
    let ctx = started(
        r##"
- from:
    uri: direct:validate
    steps:
      - validate:
          simple: "${header.valid}"
      - to: mock:valid
- from:
    uri: direct:rollback
    steps:
      - rollback:
          message: "rolled back"
- from:
    uri: direct:rollbackOnly
    steps:
      - rollback:
          markRollbackOnly: true
      - to: mock:never
- from:
    uri: direct:throw
    steps:
      - throwException:
          exceptionType: java.lang.IllegalStateException
          message: "boom ${body}"
"##,
    );
    let t = ctx.create_producer_template();
    mock(&ctx, "mock:valid").expected_message_count(1);
    t.send_body_and_header("direct:validate", "ok", "valid", true).unwrap();
    let e = t.send_body_and_header("direct:validate", "ko", "valid", false).unwrap_err();
    assert_eq!(root_kind(&e), "PredicateValidationException");
    mock(&ctx, "mock:valid").assert_is_satisfied().unwrap();
    let e = t.send_body("direct:rollback", "x").unwrap_err();
    assert_eq!(root_kind(&e), "RollbackExchangeException");
    let ex = t.send("direct:rollbackOnly", ctx.create_exchange());
    assert!(ex.rollback_only);
    assert_eq!(mock(&ctx, "mock:never").received_counter(), 0);
    let e = t.send_body("direct:throw", "it").unwrap_err();
    let root = root_error(&e);
    assert_eq!((root.kind.as_str(), root.message.as_str()), ("IllegalStateException", "boom it"));
}

// ---------------------------------------------------------------------------
// throttle / delay / sample / idempotentConsumer / circuitBreaker
// ---------------------------------------------------------------------------

#[test]
fn throttle_delay_and_sample() {
    let ctx = started(
        r##"
- from:
    uri: direct:throttle
    steps:
      - throttle:
          constant: 100
          timePeriodMillis: 1000
      - to: mock:throttled
- from:
    uri: direct:throttleReject
    steps:
      - throttle:
          expression:
            constant: 1
          timePeriodMillis: 10000
          rejectExecution: true
      - to: mock:rejecting
- from:
    uri: direct:delay
    steps:
      - delay:
          constant: 50
      - to: mock:delayed
- from:
    uri: direct:sample
    steps:
      - sample:
          messageFrequency: 2
      - to: mock:sampled
"##,
    );
    mock(&ctx, "mock:throttled").expected_message_count(3);
    for i in 0..3 {
        send(&ctx, "direct:throttle", i);
    }
    mock(&ctx, "mock:throttled").assert_is_satisfied().unwrap();

    let t = ctx.create_producer_template();
    t.send_body("direct:throttleReject", 1).unwrap();
    let e = t.send_body("direct:throttleReject", 2).unwrap_err();
    assert_eq!(root_kind(&e), "ThrottlerRejectedExecutionException");

    let start = std::time::Instant::now();
    send(&ctx, "direct:delay", "d");
    assert!(start.elapsed() >= Duration::from_millis(45));
    mock(&ctx, "mock:delayed").expected_message_count(1);
    mock(&ctx, "mock:delayed").assert_is_satisfied().unwrap();

    mock(&ctx, "mock:sampled").expected_message_count(2);
    for i in 0..4 {
        send(&ctx, "direct:sample", i);
    }
    mock(&ctx, "mock:sampled").assert_is_satisfied().unwrap();
}

#[test]
fn throttle_requires_constant() {
    let ctx = CamelContext::new();
    let e = load_routes(&ctx, "- from:\n    uri: direct:x\n    steps:\n      - throttle:\n          header: max\n").unwrap_err();
    assert!(e.message.contains("constant number of maximum requests"), "{}", e);
}

#[test]
fn idempotent_consumer_with_and_without_repository() {
    let ctx = CamelContext::new();
    let repo = camel_core::processor::errors::idempotent::memory_idempotent_repository(100);
    ctx.registry().bind("myRepo", repo.clone());
    start_with(
        &ctx,
        r##"
- from:
    uri: direct:idem
    steps:
      - idempotentConsumer:
          header: id
          idempotentRepository: "#myRepo"
          steps:
            - to: mock:unique
- from:
    uri: direct:idem2
    steps:
      - idempotentConsumer:
          expression:
            header: id
          skipDuplicate: false
          eager: true
          steps:
            - to: mock:all
"##,
    );
    mock(&ctx, "mock:unique").expected_bodies_received(vec!["a", "b"]);
    for (b, id) in [("a", 1), ("dup", 1), ("b", 2)] {
        send_h(&ctx, "direct:idem", b, "id", id);
    }
    mock(&ctx, "mock:unique").assert_is_satisfied().unwrap();
    assert_eq!(repo.size(), 2);
    let all = mock(&ctx, "mock:all");
    all.expected_message_count(2);
    send_h(&ctx, "direct:idem2", "x", "id", 7);
    send_h(&ctx, "direct:idem2", "y", "id", 7);
    all.assert_is_satisfied().unwrap();
    assert_eq!(all.exchange(1).unwrap().property("CamelDuplicateMessage").map(|v| v.is_truthy()), Some(true));

    let e = load_routes(&ctx, "- from:\n    uri: direct:x\n    steps:\n      - idempotentConsumer:\n          header: id\n          idempotentRepository: nope\n").unwrap_err();
    assert!(e.message.contains("No IdempotentRepository named 'nope'"), "{}", e);
}

#[test]
fn circuit_breaker_with_fallback_and_configuration() {
    let ctx = started(
        r##"
- from:
    uri: direct:cb
    steps:
      - circuitBreaker:
          resilience4jConfiguration:
            minimumNumberOfCalls: 2
            slidingWindowSize: 2
            failureRateThreshold: 50
            waitDurationInOpenState: 60
          steps:
            - to: mock:protected
            - throwException:
                exceptionType: java.io.IOException
                message: down
          onFallback:
            steps:
              - setBody: {constant: fallback}
      - to: mock:cbResult
"##,
    );
    mock(&ctx, "mock:cbResult").expected_bodies_received(vec!["fallback", "fallback", "fallback"]);
    for i in 0..3 {
        send(&ctx, "direct:cb", i);
    }
    mock(&ctx, "mock:cbResult").assert_is_satisfied().unwrap();
    // the breaker opened after two failed calls
    assert_eq!(mock(&ctx, "mock:protected").received_counter(), 2);
}

// ---------------------------------------------------------------------------
// threads / transacted / policy / bean / script
// ---------------------------------------------------------------------------

struct CountingPolicy(Arc<AtomicUsize>);
impl Policy for CountingPolicy {
    fn wrap(&self, _rc: &camel_core::model::RouteContext, processor: Arc<dyn camel_core::spi::Processor>) -> Result<Arc<dyn camel_core::spi::Processor>> {
        let c = self.0.clone();
        Ok(Arc::new(move |ex: &mut Exchange| {
            c.fetch_add(1, Ordering::SeqCst);
            processor.process(ex)
        }))
    }
}

#[test]
fn threads_transacted_and_policy() {
    let ctx = CamelContext::new();
    let count = Arc::new(AtomicUsize::new(0));
    let p: Arc<dyn Policy> = Arc::new(CountingPolicy(count.clone()));
    ctx.registry().bind("myPolicy", p);
    start_with(
        &ctx,
        r##"
- from:
    uri: direct:threads
    steps:
      - threads:
          poolSize: 2
          threadName: worker
      - setBody: {simple: "${body} in pool"}
      - to: mock:threads
- from:
    uri: direct:tx
    steps:
      - transacted: {}
      - policy:
          ref: "#myPolicy"
          steps:
            - to: mock:policy
      - to: mock:afterPolicy
"##,
    );
    mock(&ctx, "mock:threads").expected_bodies_received(vec!["t in pool"]);
    ctx.create_producer_template().request_body("direct:threads", "t").unwrap();
    mock(&ctx, "mock:threads").assert_is_satisfied().unwrap();
    mock(&ctx, "mock:policy").expected_message_count(1);
    mock(&ctx, "mock:afterPolicy").expected_message_count(1);
    send(&ctx, "direct:tx", "x");
    mock(&ctx, "mock:policy").assert_is_satisfied().unwrap();
    mock(&ctx, "mock:afterPolicy").assert_is_satisfied().unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn bean_and_script_steps() {
    let ctx = CamelContext::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let c2 = calls.clone();
    camel_core::component::bean::bind_bean(ctx.registry(), "greeter", move |method: Option<&str>, ex: &mut Exchange| {
        c2.fetch_add(1, Ordering::SeqCst);
        let b = ex.body().to_string();
        ex.set_body(format!("{}:{}", method.unwrap_or("default"), b));
        Ok(())
    });
    start_with(
        &ctx,
        r##"
- from:
    uri: direct:bean
    steps:
      - bean:
          ref: greeter
          method: hello
      - script:
          simple: "${body} ignored"
      - to: mock:bean
"##,
    );
    mock(&ctx, "mock:bean").expected_bodies_received(vec!["hello:x"]);
    send(&ctx, "direct:bean", "x");
    mock(&ctx, "mock:bean").assert_is_satisfied().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

// ---------------------------------------------------------------------------
// route configuration and error handling
// ---------------------------------------------------------------------------

#[test]
fn route_configuration_on_exception_applies_by_id_and_default() {
    let ctx = started(
        r##"
- routeConfiguration:
    onException:
      - onException:
          exception:
            - java.lang.IllegalArgumentException
          handled:
            constant: true
          steps:
            - setBody: {simple: "default handled ${body}"}
            - to: mock:defaultHandled
- routeConfiguration:
    id: special
    onException:
      - onException:
          exception: [java.lang.Exception]
          redeliveryPolicy:
            maximumRedeliveries: 2
            redeliveryDelay: 0
            retryAttemptedLogLevel: WARN
          handled: true
          steps:
            - to: mock:specialHandled
- route:
    from:
      uri: direct:plain
      steps:
        - throwException: {exceptionType: java.lang.IllegalArgumentException, message: x}
- route:
    routeConfigurationId: special
    from:
      uri: direct:special
      steps:
        - to: mock:attempt
        - throwException: {exceptionType: java.lang.IllegalStateException, message: y}
"##,
    );
    let t = ctx.create_producer_template();
    mock(&ctx, "mock:defaultHandled").expected_bodies_received(vec!["default handled a"]);
    t.send_body("direct:plain", "a").unwrap();
    mock(&ctx, "mock:defaultHandled").assert_is_satisfied().unwrap();

    mock(&ctx, "mock:specialHandled").expected_message_count(1);
    mock(&ctx, "mock:specialHandled").expected_header_received("CamelRedeliveryCounter", 2);
    mock(&ctx, "mock:attempt").expected_message_count(1);
    t.send_body("direct:special", "b").unwrap();
    mock(&ctx, "mock:specialHandled").assert_is_satisfied().unwrap();
    mock(&ctx, "mock:attempt").assert_is_satisfied().unwrap();
    assert_eq!(mock(&ctx, "mock:defaultHandled").received_counter(), 1);
}

#[test]
fn route_configuration_continued_on_when_and_use_original_message() {
    let ctx = started(
        r##"
- routeConfiguration:
    onException:
      - onException:
          exception: [java.lang.IllegalStateException]
          onWhen:
            simple: "${header.cont}"
          continued:
            constant: true
      - onException:
          exception: [java.lang.IllegalStateException]
          useOriginalMessage: true
          handled: {constant: "true"}
          steps:
            - to: mock:original
- from:
    uri: direct:cont
    steps:
      - setBody: {simple: "changed ${body}"}
      - throwException: {exceptionType: java.lang.IllegalStateException, message: x}
      - to: mock:continued
"##,
    );
    mock(&ctx, "mock:continued").expected_bodies_received(vec!["changed a"]);
    mock(&ctx, "mock:original").expected_bodies_received(vec!["b"]);
    send_h(&ctx, "direct:cont", "a", "cont", true);
    send_h(&ctx, "direct:cont", "b", "cont", false);
    mock(&ctx, "mock:continued").assert_is_satisfied().unwrap();
    mock(&ctx, "mock:original").assert_is_satisfied().unwrap();
}

#[test]
fn top_level_on_exception_is_an_anonymous_route_configuration() {
    let ctx = started(
        r##"
- onException:
    exception: [java.lang.IllegalStateException]
    handled: {constant: true}
    steps:
      - setBody: {constant: recovered}
- from:
    uri: direct:x
    steps:
      - throwException: {exceptionType: java.lang.IllegalStateException, message: x}
"##,
    );
    assert_eq!(ctx.create_producer_template().request_body("direct:x", "a").unwrap(), Value::from("recovered"));
}

#[test]
fn route_configuration_error_handler_on_completion_and_intercepts() {
    let ctx = started(
        r##"
- routeConfiguration:
    id: cfg
    errorHandler:
      deadLetterChannel:
        deadLetterUri: mock:dlq
        redeliveryPolicy:
          maximumRedeliveries: 1
          redeliveryDelay: 0
    onCompletion:
      - onCompletion:
          mode: AfterConsumer
          steps:
            - to: mock:completed
    intercept:
      - intercept:
          steps:
            - to: mock:intercepted
    interceptFrom:
      - interceptFrom:
          uri: "direct:in*"
          steps:
            - setHeader: {name: from, constant: intercepted}
    interceptSendToEndpoint:
      - interceptSendToEndpoint:
          uri: "mock:skipped"
          skipSendToOriginalEndpoint: true
          steps:
            - to: mock:instead
- route:
    routeConfigurationId: cfg
    from:
      uri: direct:in
      steps:
        - to: mock:skipped
        - to: mock:out
        - throwException:
            exceptionType: java.lang.IllegalStateException
            message: fail
"##,
    );
    mock(&ctx, "mock:skipped").expected_message_count(0);
    mock(&ctx, "mock:instead").expected_message_count(1);
    let out = mock(&ctx, "mock:out");
    out.expected_message_count(1);
    out.expected_header_received("from", "intercepted");
    mock(&ctx, "mock:dlq").expected_message_count(1);
    // the dead letter channel handled the failure, so the exchange completed
    mock(&ctx, "mock:completed").expected_message_count(1);
    mock(&ctx, "mock:intercepted").expected_minimum_message_count(3);
    // the DLC handles the failure: no error for the caller
    ctx.create_producer_template().send_body("direct:in", "x").unwrap();
    for m in ["mock:skipped", "mock:instead", "mock:out", "mock:dlq", "mock:completed", "mock:intercepted"] {
        mock(&ctx, m).assert_is_satisfied().unwrap();
    }
}

#[test]
fn route_configuration_on_complete_only() {
    let ctx = started(
        r##"
- routeConfiguration:
    onCompletion:
      - onCompleteOnly: true
        steps:
          - to: mock:done
- from:
    uri: direct:ok
    steps:
      - to: mock:ok
"##,
    );
    mock(&ctx, "mock:done").expected_message_count(1);
    send(&ctx, "direct:ok", "x");
    mock(&ctx, "mock:done").assert_is_satisfied().unwrap();
}

#[test]
fn top_level_error_handler_dead_letter_channel() {
    let ctx = started(
        r##"
- errorHandler:
    deadLetterChannel:
      deadLetterUri: mock:dead
      useOriginalMessage: true
      redeliveryPolicy:
        maximumRedeliveries: 2
        redeliveryDelay: 0
- from:
    uri: direct:dlc
    steps:
      - setBody: {simple: "modified ${body}"}
      - to: mock:attempts
      - throwException: {exceptionType: java.io.IOException, message: io}
"##,
    );
    mock(&ctx, "mock:attempts").expected_message_count(1);
    mock(&ctx, "mock:dead").expected_bodies_received(vec!["orig"]);
    send(&ctx, "direct:dlc", "orig");
    mock(&ctx, "mock:attempts").assert_is_satisfied().unwrap();
    mock(&ctx, "mock:dead").assert_is_satisfied().unwrap();
}

#[test]
fn route_level_error_handler_overrides_context() {
    let ctx = CamelContext::new();
    let tries = Arc::new(AtomicUsize::new(0));
    let t2 = tries.clone();
    ctx.registry().bind_processor("failTwice", move |_ex: &mut Exchange| {
        t2.fetch_add(1, Ordering::SeqCst);
        Err(CamelError::io("io"))
    });
    start_with(
        &ctx,
        r##"
- errorHandler:
    deadLetterChannel:
      deadLetterUri: mock:dead
- route:
    errorHandler:
      noErrorHandler: {}
    from:
      uri: direct:noeh
      steps:
        - throwException: {exceptionType: java.io.IOException, message: io}
- route:
    errorHandler:
      defaultErrorHandler:
        redeliveryPolicy:
          maximumRedeliveries: 1
          redeliveryDelay: 0
    from:
      uri: direct:default
      steps:
        - process: {ref: failTwice}
"##,
    );
    let t = ctx.create_producer_template();
    assert!(t.send_body("direct:noeh", "x").is_err());
    assert!(t.send_body("direct:default", "x").is_err());
    assert_eq!(tries.load(Ordering::SeqCst), 2);
    assert_eq!(mock(&ctx, "mock:dead").received_counter(), 0);
}

#[test]
fn route_configuration_errors_have_locations() {
    let ctx = CamelContext::new();
    let e = load_routes(&ctx, "- routeConfiguration:\n    bogus: 1\n").unwrap_err();
    assert!(e.message.contains("Unknown routeConfiguration option 'bogus'"), "{}", e);
    let e = load_routes(&ctx, "- routeConfiguration:\n    onException:\n      - onException:\n          steps:\n            - nope: {}\n").unwrap_err();
    assert!(e.message.contains("Unknown step 'nope'"), "{}", e);
    assert!(e.message.contains("line 5"), "{}", e);
    let e = load_routes(&ctx, "- errorHandler:\n    deadLetterChannel: {}\n").unwrap_err();
    assert!(e.message.contains("deadLetterUri"), "{}", e);
}

#[test]
fn route_scoped_on_exception_step() {
    let ctx = started(
        r##"
- from:
    uri: direct:scoped
    steps:
      - onException:
          exception: [java.lang.IllegalStateException]
          handled: {constant: true}
          steps:
            - to: mock:scopedHandled
      - throwException: {exceptionType: java.lang.IllegalStateException, message: x}
"##,
    );
    mock(&ctx, "mock:scopedHandled").expected_message_count(1);
    send(&ctx, "direct:scoped", "x");
    mock(&ctx, "mock:scopedHandled").assert_is_satisfied().unwrap();
}

// ---------------------------------------------------------------------------
// templates
// ---------------------------------------------------------------------------

#[test]
fn templated_route_defaults_prefix_id_and_beans() {
    let ctx = CamelContext::new();
    let mut loader = YamlLoader::new();
    loader.register_bean_factory("com.example.Greeting", |ctx: &CamelContext, b: &BeanDefinition| {
        let text = b.properties.get("text").map(|v| v.to_string()).unwrap_or_default();
        ctx.registry().bind_processor(b.name.clone(), move |ex: &mut Exchange| {
            ex.set_header("greeting", text.clone());
            Ok(())
        });
        Ok(())
    });
    let routes = loader
        .load_routes(
            &ctx,
            r##"
- routeTemplate:
    id: myTemplate
    parameters:
      - name: name
      - name: greeting
        defaultValue: Hello
      - name: suffix
        required: false
    beans:
      - name: "{{name}}Bean"
        type: com.example.Greeting
        properties:
          text: "{{greeting}}"
    from:
      uri: "direct:{{name}}"
      steps:
        - process:
            ref: "{{name}}Bean"
        - setBody:
            id: setIt
            simple: "{{greeting}} ${body}{{suffix:!}}"
        - to:
            id: out
            uri: "mock:{{name}}"
- templatedRoute:
    routeTemplateRef: myTemplate
    routeId: first
    prefixId: p1-
    parameters:
      - name: name
        value: one
- templatedRoute:
    routeTemplateRef: myTemplate
    parameters:
      - name: name
        value: two
      - name: suffix
        value: "?"
"##,
        )
        .unwrap();
    assert_eq!(routes.len(), 2);
    assert_eq!(routes[0].id.as_deref(), Some("first"));
    assert_eq!(routes[0].properties.get("prefixId").map(|s| s.as_str()), Some("p1-"));
    assert_eq!(routes[0].outputs[1].id(), Some("p1-setIt"));
    assert_eq!(routes[1].outputs[1].id(), Some("setIt"));
    for r in routes {
        ctx.add_route_definition(r).unwrap();
    }
    ctx.start().unwrap();
    let one = mock(&ctx, "mock:one");
    one.expected_bodies_received(vec!["Hello a!"]);
    one.expected_header_received("greeting", "Hello");
    let two = mock(&ctx, "mock:two");
    two.expected_bodies_received(vec!["Hello b?"]);
    send(&ctx, "direct:one", "a");
    send(&ctx, "direct:two", "b");
    one.assert_is_satisfied().unwrap();
    two.assert_is_satisfied().unwrap();
    assert!(ctx.registry().contains("oneBean") && ctx.registry().contains("twoBean"));
}

// ---------------------------------------------------------------------------
// XML DSL with the new EIPs
// ---------------------------------------------------------------------------

#[test]
fn xml_dsl_split_choice_do_try_and_route_configuration() {
    let ctx = CamelContext::new();
    add_routes_from_xml(
        &ctx,
        r##"<?xml version="1.0" encoding="UTF-8"?>
<routes xmlns="http://camel.apache.org/schema/spring">
  <routeConfiguration id="xmlCfg">
    <onException>
      <exception>java.lang.IllegalStateException</exception>
      <handled><constant>true</constant></handled>
      <setBody><constant>xml handled</constant></setBody>
      <to uri="mock:xmlHandled"/>
    </onException>
  </routeConfiguration>
  <route id="xmlSplit">
    <from uri="direct:xmlSplit"/>
    <split>
      <simple>${body}</simple>
      <choice>
        <when>
          <simple>${header.big}</simple>
          <to uri="mock:big"/>
        </when>
        <otherwise>
          <to uri="mock:small"/>
        </otherwise>
      </choice>
    </split>
  </route>
  <route id="xmlTry">
    <from uri="direct:xmlTry"/>
    <doTry>
      <throwException exceptionType="java.lang.IllegalArgumentException" message="bad"/>
      <doCatch>
        <exception>java.lang.IllegalArgumentException</exception>
        <to uri="mock:xmlCatch"/>
      </doCatch>
      <doFinally>
        <to uri="mock:xmlFinally"/>
      </doFinally>
    </doTry>
  </route>
  <route id="xmlCfgRoute" routeConfigurationId="xmlCfg">
    <from uri="direct:xmlCfg"/>
    <throwException exceptionType="java.lang.IllegalStateException" message="x"/>
  </route>
  <route id="xmlAgg">
    <from uri="direct:xmlAgg"/>
    <aggregate aggregationStrategy="GroupedBodyAggregationStrategy" completionSize="2">
      <correlationExpression><header>id</header></correlationExpression>
      <to uri="mock:xmlAgg"/>
    </aggregate>
  </route>
  <route id="xmlLb">
    <from uri="direct:xmlLb"/>
    <loadBalance>
      <failover>
        <exception>java.lang.IllegalStateException</exception>
      </failover>
      <to uri="direct:xmlCfgFails"/>
      <to uri="mock:xmlLb"/>
    </loadBalance>
  </route>
  <route>
    <from uri="direct:xmlCfgFails"/>
    <throwException exceptionType="java.lang.IllegalStateException" message="x"/>
  </route>
</routes>"##,
    )
    .unwrap();
    ctx.start().unwrap();
    mock(&ctx, "mock:small").expected_bodies_received(vec!["a", "b"]);
    send(&ctx, "direct:xmlSplit", "a,b");
    mock(&ctx, "mock:big").expected_bodies_received(vec!["c"]);
    send_h(&ctx, "direct:xmlSplit", "c", "big", true);
    mock(&ctx, "mock:small").assert_is_satisfied().unwrap();
    mock(&ctx, "mock:big").assert_is_satisfied().unwrap();

    mock(&ctx, "mock:xmlCatch").expected_message_count(1);
    mock(&ctx, "mock:xmlFinally").expected_message_count(1);
    send(&ctx, "direct:xmlTry", "x");
    mock(&ctx, "mock:xmlCatch").assert_is_satisfied().unwrap();
    mock(&ctx, "mock:xmlFinally").assert_is_satisfied().unwrap();

    mock(&ctx, "mock:xmlHandled").expected_bodies_received(vec!["xml handled"]);
    send(&ctx, "direct:xmlCfg", "x");
    mock(&ctx, "mock:xmlHandled").assert_is_satisfied().unwrap();

    mock(&ctx, "mock:xmlAgg").expected_bodies_received(vec![Value::Array(vec!["1".into(), "2".into()])]);
    send_h(&ctx, "direct:xmlAgg", "1", "id", "k");
    send_h(&ctx, "direct:xmlAgg", "2", "id", "k");
    mock(&ctx, "mock:xmlAgg").assert_is_satisfied().unwrap();

    mock(&ctx, "mock:xmlLb").expected_message_count(1);
    send(&ctx, "direct:xmlLb", "x");
    mock(&ctx, "mock:xmlLb").assert_is_satisfied().unwrap();
}

#[test]
fn marshal_unmarshal_steps() {
    let ctx = camel_core::CamelContext::new();
    camel_yaml::add_routes_from_yaml(&ctx, r#"
- from:
    uri: direct:mu
    steps:
      - unmarshal:
          json: {}
      - setBody:
          simple: "${body[name]}"
      - to: mock:name
- from:
    uri: direct:csv
    steps:
      - unmarshal:
          csv:
            delimiter: ";"
      - marshal:
          json: {}
      - to: mock:csv
- from:
    uri: direct:b64
    steps:
      - marshal: base64
      - unmarshal:
          base64: {}
      - to: mock:b64
"#).unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("direct:mu", r#"{"name":"Camel"}"#).unwrap();
    t.send_body("direct:csv", "a;b\nc;d").unwrap();
    t.send_body("direct:b64", "hello").unwrap();
    assert_eq!(ctx.mock_endpoint("mock:name").unwrap().received_bodies(), vec![camel_core::Value::from("Camel")]);
    assert_eq!(ctx.mock_endpoint("mock:csv").unwrap().received_bodies()[0].to_string(), r#"[["a","b"],["c","d"]]"#);
    assert_eq!(ctx.mock_endpoint("mock:b64").unwrap().received_bodies()[0].to_string(), "hello");
    ctx.stop().unwrap();
}

#[test]
fn async_delayed_redelivery_and_delay_options() {
    let ctx = CamelContext::new();
    ctx.registry().bind_processor("failFirst", |ex: &mut Exchange| {
        if ex.header("CamelRedelivered").is_none() {
            return Err(CamelError::io("io"));
        }
        Ok(())
    });
    start_with(
        &ctx,
        r##"
- errorHandler:
    defaultErrorHandler:
      redeliveryPolicy:
        maximumRedeliveries: 1
        redeliveryDelay: 300
        asyncDelayedRedelivery: true
- from:
    uri: seda:redeliver
    steps:
      - to: mock:attempt
      - process: {ref: failFirst}
      - to: mock:ok
- from:
    uri: seda:delay
    steps:
      - to: mock:before
      - delay:
          constant: 300
          asyncDelayed: false
      - to: mock:after
"##,
    );
    let t = ctx.create_producer_template();
    for i in 0..3 {
        t.send_body("seda:redeliver", i).unwrap();
        t.send_body("seda:delay", i).unwrap();
    }
    // asynchronous redelivery: the three first attempts are not held back by the delays
    assert!(mock(&ctx, "mock:attempt").wait_for_messages(3, Duration::from_millis(250)));
    // synchronous delay: the consumer is blocked
    assert_eq!(mock(&ctx, "mock:before").received_counter(), 1);
    assert!(mock(&ctx, "mock:ok").wait_for_messages(3, Duration::from_secs(5)));
    assert!(mock(&ctx, "mock:after").wait_for_messages(3, Duration::from_secs(5)));
    ctx.stop().unwrap();
}

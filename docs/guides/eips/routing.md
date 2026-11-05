# Routing patterns

> Routing patterns decide where a message goes next: to one endpoint, to several, to a list
> computed at run time, or around a loop, using the same EIP names and options as Apache Camel.

This page covers the pipeline, message filter, content-based router, recipient list, routing
slip, dynamic router, multicast, wire tap, load balancer and loop. Each pattern has a YAML
route and the same route in the Rust DSL; both run in the test suite
(`crates/cameleer-main/tests/docs_guides.rs`). The index of all patterns is in
[the EIP guide](../eips.md).

## Pipeline: `to`, `toD` and `step`

A route is a pipeline: each step gets the output of the step before it. `to` sends to a fixed
endpoint, `toD` computes the endpoint URI from a Simple expression for every message, and
`step` groups steps under one id for statistics.

<!-- example: eip-pipeline-tod -->
```yaml
- route:
    id: pipeline
    from:
      uri: direct:orders
      steps:
        - step:
            id: normalize
            steps:
              - setHeader:
                  name: region
                  simple: "${body[region]}"
        - toD: "mock:region-${header.region}"
        - to: mock:all-orders
```

```rust
b.from("direct:orders")
    .route_id("pipeline")
    .step("normalize")
        .set_header("region", simple("${body[region]}"))
    .end()
    .to_d("mock:region-${header.region}")
    .to("mock:all-orders");
```

`toD` keeps a cache of the endpoints it created, as in Camel. Use it when the target depends on
the message (a tenant, a region, a model name); use `to` otherwise.

## Message filter

A filter lets a message through its block only when a predicate is true. After the block, the
route continues with every message. The exchange property `CamelFilterMatched` tells whether
the last filter matched.

<!-- example: eip-filter -->
```yaml
- route:
    from:
      uri: direct:payments
      steps:
        - filter:
            simple: "${header.amount} > 1000"
            steps:
              - to: mock:review
        - to: mock:ledger
```

```rust
b.from("direct:payments")
    .filter(simple("${header.amount} > 1000"))
        .to("mock:review")
    .end()
    .to("mock:ledger");
```

To drop non-matching messages entirely, put the rest of the route inside the filter block, or
use `stop` in a `choice`.

## Content-based router: `choice`

`choice` sends the message down the first `when` branch whose predicate is true, or to
`otherwise`. A `choice` without `otherwise` lets unmatched messages continue after the choice.

<!-- example: eip-choice -->
```yaml
- route:
    from:
      uri: direct:tickets
      steps:
        - choice:
            when:
              - simple: "${header.priority} == 'P1'"
                steps:
                  - to: mock:page-oncall
              - simple: "${header.priority} == 'P2'"
                steps:
                  - to: mock:team-queue
            otherwise:
              steps:
                - to: mock:backlog
```

```rust
b.from("direct:tickets")
    .choice()
        .when(simple("${header.priority} == 'P1'"))
            .to("mock:page-oncall")
        .when(simple("${header.priority} == 'P2'"))
            .to("mock:team-queue")
        .otherwise()
            .to("mock:backlog")
    .end();
```

A typical AI use: an LLM writes a label into a header, and `choice` routes on the label. See
[Cameleer for AI agents](../agents.md#classify-with-an-llm-and-route-on-the-answer).

## Recipient list

A recipient list sends a copy of the message to every endpoint named by an expression,
evaluated per message. The default delimiter is a comma.

<!-- example: eip-recipient-list -->
```yaml
- route:
    from:
      uri: direct:notify
      steps:
        - recipientList:
            simple: "${header.channels}"
```

```rust
b.from("direct:notify")
    .recipient_list(simple("${header.channels}"));
```

Send it a message with the header `channels` set to `mock:email,mock:sms` and both endpoints
get a copy. Options: `delimiter`, `parallelProcessing`, `ignoreInvalidEndpoints`,
`stopOnException`, `timeout`, `aggregationStrategy`.

## Routing slip

A routing slip sends the message through a sequence of endpoints, one after another, each
getting the output of the previous one. The list comes from an expression, often a header the
sender sets.

<!-- example: eip-routing-slip -->
```yaml
- route:
    from:
      uri: direct:process
      steps:
        - routingSlip:
            simple: "${header.slip}"
- route:
    from:
      uri: direct:resize
      steps:
        - setBody:
            simple: "${body}-resized"
- route:
    from:
      uri: direct:watermark
      steps:
        - setBody:
            simple: "${body}-watermarked"
```

```rust
b.from("direct:process")
    .routing_slip(simple("${header.slip}"));
b.from("direct:resize")
    .set_body(simple("${body}-resized"));
b.from("direct:watermark")
    .set_body(simple("${body}-watermarked"));
```

With `slip` set to `direct:resize,direct:watermark,mock:done`, the body `photo` arrives at
`mock:done` as `photo-resized-watermarked`. The option `uriDelimiter` changes the separator.

## Dynamic router

A dynamic router evaluates its expression again after every hop and stops when the expression
returns nothing. Here each step names the next one in the `next` header, so the route itself
decides how far the message goes.

<!-- example: eip-dynamic-router -->
```yaml
- route:
    from:
      uri: direct:workflow
      steps:
        - dynamicRouter:
            simple: "${header.next}"
        - to: mock:workflow-done
- route:
    from:
      uri: direct:validate
      steps:
        - setHeader:
            name: next
            constant: direct:approve
- route:
    from:
      uri: direct:approve
      steps:
        - removeHeader:
            name: next
        - setBody:
            simple: "${body} approved"
```

```rust
b.from("direct:workflow")
    .dynamic_router(simple("${header.next}"))
    .to("mock:workflow-done");
b.from("direct:validate")
    .set_header("next", constant("direct:approve"));
b.from("direct:approve")
    .remove_header("next")
    .set_body(simple("${body} approved"));
```

Send `invoice 7` with the header `next` set to `direct:validate`: the router calls
`direct:validate`, then `direct:approve`, which removes the header, so the router ends and
`mock:workflow-done` receives `invoice 7 approved`. An agent's planner can fill the `next`
header the same way: the route runs whatever hop it names until there is none.

## Multicast

A multicast sends the same message to several steps, one after the other or in parallel. Without
an aggregation strategy the reply of the last step wins, as in Camel.

<!-- example: eip-multicast -->
```yaml
- route:
    from:
      uri: direct:order-placed
      steps:
        - multicast:
            parallelProcessing: true
            steps:
              - to: mock:billing
              - to: mock:shipping
              - to: mock:analytics
```

```rust
b.from("direct:order-placed")
    .multicast()
        .parallel_processing()
        .to("mock:billing")
        .to("mock:shipping")
        .to("mock:analytics")
    .end();
```

Options: `parallelProcessing`, `stopOnException`, `timeout`, `shareUnitOfWork`,
`aggregationStrategy`. Use an aggregation strategy to combine the replies, for example to ask
several models the same question and keep all answers.

## Wire tap

A wire tap sends a copy of the message to another endpoint asynchronously, on a separate
exchange, and the route goes on at once. Use it for audit logs and side channels that must not
slow down or fail the main flow.

<!-- example: eip-wire-tap -->
```yaml
- route:
    from:
      uri: direct:prompts
      steps:
        - wireTap:
            uri: mock:audit
        - setBody:
            simple: "answered: ${body}"
        - to: mock:answers
```

```rust
b.from("direct:prompts")
    .wire_tap("mock:audit")
    .set_body(simple("answered: ${body}"))
    .to("mock:answers");
```

The tapped copy is taken when the wire tap runs, so `mock:audit` receives the original prompt.
`newExchangeBody` and `setHeader` entries build a new message for the tap instead of copying.

## Load balancer

A load balancer sends each message to one of its steps. Policies: `roundRobin`, `random`,
`sticky` (by a correlation expression), `weighted`, `topic` (all of them) and `failover` (the
next step when one fails), plus custom balancers from the registry.

<!-- example: eip-load-balancer -->
```yaml
- route:
    from:
      uri: direct:inference
      steps:
        - loadBalance:
            roundRobin: {}
            steps:
              - to: mock:gpu-1
              - to: mock:gpu-2
```

```rust
b.from("direct:inference")
    .load_balance()
        .round_robin()
        .to("mock:gpu-1")
        .to("mock:gpu-2")
    .end();
```

Failover moves to the next step when the current one throws one of the listed exceptions. With
`roundRobin: true` it also rotates the starting step:

<!-- example: eip-load-balancer-failover -->
```yaml
- route:
    from:
      uri: direct:call-model
      steps:
        - loadBalance:
            failover:
              exception:
                - java.io.IOException
            steps:
              - to: direct:primary-model
              - to: direct:backup-model
- route:
    from:
      uri: direct:primary-model
      steps:
        - throwException:
            exceptionType: java.io.IOException
            message: primary is down
- route:
    from:
      uri: direct:backup-model
      steps:
        - setBody:
            constant: answer from backup
```

```rust
b.from("direct:call-model")
    .load_balance()
        .failover(&["IOException"])
        .to("direct:primary-model")
        .to("direct:backup-model")
    .end();
b.from("direct:primary-model")
    .throw_exception("IOException", "primary is down");
b.from("direct:backup-model")
    .set_body(constant("answer from backup"));
```

Exception names are matched by their simple class name: `java.io.IOException` in YAML and
`IOException` in Rust are the same kind. For model calls, the
[circuit breaker](flow-control.md#circuit-breaker) is often the better tool, because it stops
calling a failing model for a while.

## Loop

`loop` runs its steps a fixed number of times, or while a predicate is true (`doWhile: true`).
The exchange property `CamelLoopIndex` holds the current iteration, starting at 0.

<!-- example: eip-loop -->
```yaml
- route:
    from:
      uri: direct:retry-poll
      steps:
        - loop:
            constant: 3
            steps:
              - setBody:
                  simple: "${body}+${exchangeProperty.CamelLoopIndex}"
        - to: mock:looped
```

```rust
b.from("direct:retry-poll")
    .loop_(constant(3))
        .set_body(simple("${body}+${exchangeProperty.CamelLoopIndex}"))
    .end()
    .to("mock:looped");
```

The body `x` comes out as `x+0+1+2`. With `copy: true` every iteration starts from a copy of
the original message instead of the previous iteration's output.

## Common questions

### How do I route messages based on their content?

Use `choice` with one `when` per case and an `otherwise` for the rest. Predicates are usually
Simple (`${header.type} == 'refund'`), but JSONPath, XPath and jq work too.

### What is the difference between a recipient list, a routing slip and a multicast?

A multicast and a recipient list send copies to several endpoints (a fixed list or one computed
per message); a routing slip sends one message through several endpoints in sequence.

### How do I send a message to an endpoint whose name is in a header?

Use `toD` with a Simple expression, for example `toD: "kafka:${header.topic}"`. For a list of
endpoints, use `recipientList`.

### Does a filter stop the route?

No. Messages that do not match skip the filter's block and continue after it. Put the remaining
steps inside the block, or use `stop`, to end the route for them.

## Related

- [All EIPs](../eips.md), [splitting and aggregating](split-aggregate.md),
  [flow control and resilience](flow-control.md)
- [Error handling](../error-handling.md)
- Apache Camel: [Enterprise Integration Patterns](https://camel.apache.org/components/next/eips/enterprise-integration-patterns.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.

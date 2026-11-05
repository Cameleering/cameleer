# Enterprise Integration Patterns in Cameleer

> Cameleer implements Apache Camel's Enterprise Integration Patterns (EIPs) with the same names,
> options and semantics; this page lists every one and links to a tested YAML example and its
> Rust DSL equivalent.

An EIP is a named solution to a messaging problem: filter, route, split, aggregate, retry. In a
route each EIP is a step. The YAML names are Camel's (`split`, `aggregate`, `circuitBreaker`);
the Rust DSL uses snake_case (`split`, `aggregate`, `circuit_breaker`) and closes blocks with
`.end()`. Every example on the linked pages runs twice in the test suite, once as YAML and once
as Rust, against the same assertions (`crates/cameleer-main/tests/docs_guides.rs`).

## Routing

| EIP | YAML | Rust DSL | page |
|---|---|---|---|
| Pipeline, To, Dynamic To, Step | `to`, `toD`, `step` | `to`, `to_d`, `step` | [routing](eips/routing.md#pipeline-to-tod-and-step) |
| Message Filter | `filter` | `filter` | [routing](eips/routing.md#message-filter) |
| Content-Based Router | `choice` / `when` / `otherwise` | `choice().when().otherwise().end()` | [routing](eips/routing.md#content-based-router-choice) |
| Recipient List | `recipientList` | `recipient_list` | [routing](eips/routing.md#recipient-list) |
| Routing Slip | `routingSlip` | `routing_slip` | [routing](eips/routing.md#routing-slip) |
| Dynamic Router | `dynamicRouter` | `dynamic_router` | [routing](eips/routing.md#dynamic-router) |
| Multicast | `multicast` | `multicast` | [routing](eips/routing.md#multicast) |
| Wire Tap | `wireTap` | `wire_tap` | [routing](eips/routing.md#wire-tap) |
| Load Balancer (round robin, random, sticky, weighted, topic, failover, custom) | `loadBalance` | `load_balance` | [routing](eips/routing.md#load-balancer) |
| Loop | `loop` | `loop_` | [routing](eips/routing.md#loop) |

## Splitting, aggregating and ordering

| EIP | YAML | Rust DSL | page |
|---|---|---|---|
| Splitter | `split` | `split` | [split and aggregate](eips/split-aggregate.md#splitter) |
| Composed Message Processor (split + aggregation strategy) | `split` with `aggregationStrategy` | `split(..).aggregation_strategy(..)` | [split and aggregate](eips/split-aggregate.md#split-and-aggregate-the-results) |
| Aggregator | `aggregate` | `aggregate` | [split and aggregate](eips/split-aggregate.md#aggregator) |
| Resequencer (batch, stream) | `resequence` | `resequence` | [split and aggregate](eips/split-aggregate.md#resequencer) |
| Sort | `sort` | `sort` | [split and aggregate](eips/split-aggregate.md#sort) |

## Transformation

| EIP | YAML | Rust DSL | page |
|---|---|---|---|
| Message Translator | `setBody`, `transform`, `setHeader`, `removeHeaders` | `set_body`, `transform`, `set_header`, `remove_headers` | [transformation](eips/transformation.md#message-translator-setbody-transform-setheader) |
| Marshal, Unmarshal (data formats) | `marshal`, `unmarshal` | `marshal`, `unmarshal`, `marshal_json` | [transformation](eips/transformation.md#marshal-and-unmarshal) |
| Convert Body | `convertBodyTo` | `convert_body_to` | [transformation](eips/transformation.md#convert-the-body-type) |
| Content Enricher | `enrich` | `enrich` | [transformation](eips/transformation.md#content-enricher-enrich) |
| Poll Enrich | `pollEnrich` | `poll_enrich` | [transformation](eips/transformation.md#poll-enrich-pollenrich) |
| Claim Check | `claimCheck` | `claim_check` | [transformation](eips/transformation.md#claim-check) |
| Script | `script` | `script`, `process(cameleer_script::script(..))` | [transformation](eips/transformation.md#script) |

## Flow control and resilience

| EIP | YAML | Rust DSL | page |
|---|---|---|---|
| Idempotent Consumer | `idempotentConsumer` | `idempotent_consumer` | [flow control](eips/flow-control.md#idempotent-consumer) |
| Throttle | `throttle` | `throttle` | [flow control](eips/flow-control.md#throttle) |
| Delay | `delay` | `delay` | [flow control](eips/flow-control.md#delay) |
| Sampling | `sample` | `sample`, `sample_every` | [flow control](eips/flow-control.md#sampling) |
| Circuit Breaker | `circuitBreaker` / `onFallback` | `circuit_breaker().on_fallback()` | [flow control](eips/flow-control.md#circuit-breaker) |
| Validate | `validate` | `validate` | [flow control](eips/flow-control.md#validate) |
| Threads | `threads` | `threads`, `threads_with` | [flow control](eips/flow-control.md#threads) |

## Interceptors, completions and reuse

| EIP | YAML | Rust DSL | page |
|---|---|---|---|
| Intercept Send To Endpoint | `interceptSendToEndpoint` | `intercept_send_to_endpoint` | [system](eips/system.md#intercept-sends-to-an-endpoint) |
| Intercept From, Intercept | `interceptFrom`, `intercept` | `intercept_from`, `intercept` | [system](eips/system.md#intercept-every-input) |
| On Completion | `onCompletion` | `on_completion` | [system](eips/system.md#on-completion) |
| Saga | `saga` | `saga` | [system](eips/system.md#saga) |
| Route Template | `routeTemplate`, `templatedRoute` | `add_route_template`, `TemplatedRouteBuilder` | [system](eips/system.md#route-templates) |
| Kamelet | `kamelet` | `kamelet` | [system](eips/system.md#kamelets) |
| Poll | `poll` | `poll` | [system](eips/system.md#poll) |
| Stop | `stop` | `stop` | [system](eips/system.md#stop) |

## Error handling

| EIP | YAML | Rust DSL | page |
|---|---|---|---|
| Dead Letter Channel, redelivery | `errorHandler: deadLetterChannel` | `error_handler(dead_letter_channel(..))` | [error handling](error-handling.md#dead-letter-channel-with-redelivery) |
| Exception clause | `onException` | `on_exception` | [error handling](error-handling.md#onexception-react-to-one-kind-of-failure) |
| Try, catch, finally | `doTry` / `doCatch` / `doFinally` | `do_try().do_catch().do_finally().end_do_try()` | [error handling](error-handling.md#dotry-docatch-and-dofinally) |
| Throw Exception, Rollback | `throwException`, `rollback` | `throw_exception`, `rollback`, `mark_rollback_only` | [error handling](error-handling.md#onexception-react-to-one-kind-of-failure) |

## Patterns without a guide example yet

These are implemented and covered by Cameleer's own test suite, but this guide has no example
for them yet; [FEATURES.md](../FEATURES.md#enterprise-integration-patterns) describes their
options.

| EIP | YAML | Rust DSL |
|---|---|---|
| Process, Bean (custom Rust code) | `process`, `bean` | `process`, `process_ref` (see [concepts](concepts.md#processors-and-eips)) |
| Transacted, Policy | `transacted`, `policy` | `transacted`, `transacted_ref`, `policy` |
| Route Policy | route `routePolicy` | `route_policy` |
| Service Call | `serviceCall` | `service_call` |
| Transformer, Validator (data types) | route `inputType`/`outputType`, `transformDataType` | `input_type`, `output_type`, `transform_data_type` |
| Set Exchange Pattern (InOnly, InOut) | `setExchangePattern` | `set_exchange_pattern`, `in_only`, `in_out` |
| Message History, Tracer | `camel.main.messageHistory`, `--trace` | `set_message_history`, `set_tracing` |

## Common questions

### Which EIPs does Cameleer support?

All of Apache Camel's EIPs listed above, from the filter and content-based router to the saga
and circuit breaker. Differences from Camel are noted on each page and in
[FEATURES.md](../FEATURES.md).

### Are EIP options the same as in Apache Camel?

Yes: names, defaults and semantics follow Camel 4 (for example `completionTimeout` is an
inactivity timeout and a split without an aggregation strategy continues with the original
message). Options Cameleer does not model, such as `executorService` on some EIPs, are accepted
and ignored in YAML.

### How do I write an EIP in Rust instead of YAML?

Use the method with the snake_case name on the route builder and close blocks with `.end()`:
`b.from("direct:a").split(jsonpath("$.items")).to("mock:item").end();`. Each pattern page shows
both forms.

## Related

- [Routing](eips/routing.md), [split and aggregate](eips/split-aggregate.md),
  [transformation](eips/transformation.md), [flow control](eips/flow-control.md),
  [interceptors and reuse](eips/system.md), [error handling](error-handling.md)
- [Concepts](concepts.md), [agents cookbook](agents.md), [edge cookbook](edge.md)
- Apache Camel: [EIP catalogue](https://camel.apache.org/components/next/eips/enterprise-integration-patterns.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.

# Flow control and resilience patterns

> Flow control and resilience patterns protect routes and the systems they call: drop
> duplicates (idempotent consumer), limit the rate (throttle, sample, delay), stop calling a
> failing service (circuit breaker), check messages (validate) and process in parallel
> (threads).

These are the patterns to put around paid or rate-limited APIs such as LLMs, payment providers
and SaaS endpoints. Every example runs in the test suite as YAML and as Rust DSL. See
[the EIP guide](../eips.md) for the other patterns and [error handling](../error-handling.md)
for retries.

## Idempotent consumer

The idempotent consumer remembers the keys it has seen and skips its block for a message whose
key was already processed. Use it so a redelivered webhook or Kafka message does not charge a
card or call a model twice.

<!-- example: eip-idempotent-consumer -->
```yaml
- route:
    from:
      uri: direct:payment-events
      steps:
        - idempotentConsumer:
            header: paymentId
            steps:
              - to: mock:charge
        - to: mock:seen
```

```rust
b.from("direct:payment-events")
    .idempotent_consumer(header("paymentId"), memory_idempotent_repository(1000))
        .to("mock:charge")
    .end()
    .to("mock:seen");
```

Two events with `paymentId` `p-1` and one with `p-2` give two charges. Duplicates skip only
the idempotent block, as in Camel, so `mock:seen` still gets all three; the exchange property
`CamelDuplicateMessage` is `true` on the duplicate. Options: `eager` (default true),
`skipDuplicate`, `removeOnFailure` (a failed message's key is forgotten so a retry can pass),
`completionEager`, and `idempotentRepository` (a bean). The default repository is an in-memory
LRU of 1000 keys; JDBC and Redis repositories are in `cameleer-sql` and `cameleer-datastores`
for keys that must survive restarts.

## Throttle

`throttle` lets at most N messages through per time period and delays the rest. It uses
Camel's classic time-period semantics (`timePeriodMillis`, default 1000).

<!-- example: eip-throttle -->
```yaml
- route:
    from:
      uri: direct:model-calls
      steps:
        - throttle:
            constant: 2
            timePeriodMillis: 500
        - to: mock:model
```

```rust
b.from("direct:model-calls")
    .throttle(2, Duration::from_millis(500))
    .to("mock:model");
```

Five calls in a row take at least a second: two pass at once, the next two after 500 ms, the
last after 1000 ms. `rejectExecution: true` fails the extra messages with
`ThrottlerRejectedExecutionException` instead of waiting, `correlationExpression` keeps a
separate limit per key (per tenant, per API key), and `asyncDelayed: true` releases the
consumer thread of asynchronous consumers while a message waits. The maximum must be a
constant.

## Delay

`delay` waits before the next step. The expression gives milliseconds.

<!-- example: eip-delay -->
```yaml
- route:
    from:
      uri: direct:slow-down
      steps:
        - delay:
            constant: 200
        - to: mock:delayed
```

```rust
b.from("direct:slow-down")
    .delay(constant(200))
    .to("mock:delayed");
```

`asyncDelayed` defaults to true, as in Camel: seda and timer consumers hand the waiting
exchange to another thread. Synchronous callers (`direct:`, a producer template) still wait.

## Sampling

`sample` lets one message through per period (`samplePeriod`, default one second) or one in
every N messages (`messageFrequency`) and drops the rest. Use it to thin out high-rate
telemetry.

<!-- example: eip-sample -->
```yaml
- route:
    from:
      uri: direct:telemetry
      steps:
        - sample:
            messageFrequency: 3
        - to: mock:sampled
```

```rust
b.from("direct:telemetry")
    .sample_every(3)
    .to("mock:sampled");
```

Of six readings, two reach `mock:sampled`.

## Circuit breaker

The circuit breaker counts failures of its steps in a sliding window. When the failure rate
reaches `failureRateThreshold`, it opens: calls go straight to `onFallback` without trying the
steps, for `waitDurationInOpenState` seconds. Then it lets a few calls through (half-open) to
test whether the service is back.

<!-- example: eip-circuit-breaker -->
```yaml
- route:
    from:
      uri: direct:ask-model
      steps:
        - circuitBreaker:
            resilience4jConfiguration:
              slidingWindowSize: 2
              minimumNumberOfCalls: 2
              failureRateThreshold: 50
              waitDurationInOpenState: 30
            steps:
              - to: direct:flaky-model
            onFallback:
              steps:
                - setBody:
                    constant: "The assistant is busy, please retry in a minute."
- route:
    from:
      uri: direct:flaky-model
      steps:
        - throwException:
            exceptionType: java.io.IOException
            message: model API timed out
```

```rust
b.from("direct:ask-model")
    .circuit_breaker()
        .sliding_window_size(2)
        .minimum_number_of_calls(2)
        .failure_rate_threshold(50.0)
        .wait_duration_in_open_state(Duration::from_secs(30))
        .to("direct:flaky-model")
    .on_fallback()
        .set_body(constant("The assistant is busy, please retry in a minute."))
    .end();
b.from("direct:flaky-model")
    .throw_exception("IOException", "model API timed out");
```

Every call gets the fallback answer. After two failed calls the breaker is open, and the third
call does not reach `direct:flaky-model` at all: its exchange property
`CamelCircuitBreakerResponseShortCircuited` is `true`. Other properties:
`CamelCircuitBreakerResponseFromFallback`, `CamelCircuitBreakerSuccessfulExecution`,
`CamelCircuitBreakerResponseTimedOut` and `CamelCircuitBreakerState`. `timeoutEnabled` and
`timeoutDuration` fail slow calls. The state machine is built in; the
`faultToleranceConfiguration` names are accepted too, but there is no Resilience4j or
MicroProfile library behind it.

## Validate

`validate` checks a predicate and fails the exchange with `PredicateValidationException` when
it is false. Pair it with `onException` or `doTry` to answer bad input with a clear error.

<!-- example: eip-validate -->
```yaml
- route:
    from:
      uri: direct:signup
      steps:
        - validate:
            simple: "${body} contains '@'"
        - to: mock:signups
```

```rust
b.from("direct:signup")
    .validate(simple("${body} contains '@'"))
    .to("mock:signups");
```

`ana@example.com` passes; `not-an-email` fails with `PredicateValidationException`. To check
JSON against a schema, use the [json-validator component](../../components/json-validator.md).

## Threads

`threads` hands the rest of the route to a thread pool, so the caller is free at once and
messages are processed in parallel.

<!-- example: eip-threads -->
```yaml
- route:
    from:
      uri: direct:fan-out
      steps:
        - threads:
            poolSize: 4
            maxPoolSize: 8
        - to: mock:parallel
```

```rust
b.from("direct:fan-out")
    .threads(4)
    .to("mock:parallel");
```

Options: `poolSize`, `maxPoolSize`, `keepAliveTime`, `maxQueueSize` (0 hands off directly, -1
is unbounded), `rejectedPolicy` (`Abort`, `CallerRuns`, `Discard`, `DiscardOldest`),
`threadName` and `executorService` (a shared pool bean). `threads_with(ThreadsOptions)` sets the
same options in Rust. The pool follows Java's `ThreadPoolExecutor` rules.

## Common questions

### How do I stop processing duplicate messages?

Wrap the side effect in `idempotentConsumer` with a key expression such as `header: orderId`
or `jsonpath: "$.id"`. Use a JDBC or Redis repository when the keys must survive restarts or be
shared between instances.

### How do I rate limit calls to an API?

Use `throttle` with the number of calls per `timePeriodMillis`. For a limit per customer or API
key, add `correlationExpression`.

### What is the difference between a circuit breaker and retries?

Retries (redelivery) try the same call again a few times; a circuit breaker stops trying for a
while after repeated failures and answers from the fallback. They combine well: retry briefly
inside the breaker, and let the breaker protect the service when it is really down.

### How do I process messages in parallel?

Use `threads` for the rest of a route, `parallelProcessing: true` on `split`, `multicast` or
`recipientList`, or `concurrentConsumers` on a `seda` endpoint.

## Related

- [All EIPs](../eips.md), [error handling](../error-handling.md)
- [Agents cookbook: rate limits and circuit breakers around model APIs](../agents.md#rate-limits-and-a-circuit-breaker-around-a-model-api)
- Apache Camel: [Idempotent Consumer](https://camel.apache.org/components/next/eips/idempotentConsumer-eip.html),
  [Throttle](https://camel.apache.org/components/next/eips/throttle-eip.html),
  [Circuit Breaker](https://camel.apache.org/components/next/eips/circuitBreaker-eip.html)

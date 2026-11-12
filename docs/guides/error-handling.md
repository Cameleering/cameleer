# Error handling

> Cameleer handles failures the way Apache Camel does: an error handler retries failed steps
> with a redelivery policy and then gives up or moves the message to a dead letter channel,
> `onException` reacts to specific exception types, and `doTry`/`doCatch`/`doFinally` handle
> errors inline.

Every example on this page runs in the test suite (`crates/cameleer-main/tests/docs_guides.rs`)
as YAML and as Rust DSL.

## How failures work

- A step fails by returning an error. The error has a **kind**, a Java-like exception class
  name such as `IOException` or `IllegalArgumentException`. Kinds have parents, like Java
  classes (`FileNotFoundException` is an `IOException`, every kind is an `Exception`), and
  `onException` and `doCatch` match a kind and its subkinds. Java class names in YAML
  (`java.io.IOException`) are matched by their simple name.
- The **error handler** decides what happens next. The default one (`defaultErrorHandler`)
  does not retry and fails the exchange; the caller gets the error. The
  **dead letter channel** sends the failed message to an endpoint instead and marks it handled.
  `noErrorHandler` does nothing.
- **Redelivery** happens at the point of failure: only the failed step runs again, not the
  whole route. The headers `CamelRedelivered`, `CamelRedeliveryCounter` and
  `CamelRedeliveryMaxCounter` tell the step which attempt it is.
- After the last attempt the exception is on the exchange and the exchange property
  `CamelExceptionCaught` holds it for `onException` and dead letter routes; `${exception.message}`
  reads its text in Simple.

The examples below call an inventory API bound as a processor bean. It fails with
`IOException` on the first call and the first retry, and succeeds on the second retry; the
body `poison` always fails:

```rust
fn bind_inventory_api(ctx: &CamelContext) {
    ctx.registry().bind_processor("inventoryApi", |ex: &mut Exchange| {
        let attempt = ex.header("CamelRedeliveryCounter").map(|v| v.to_string()).unwrap_or_default();
        if ex.body().to_string() == "poison" || attempt != "2" {
            return Err(CamelError::new("IOException", "connection reset by inventory API"));
        }
        ex.set_body(format!("reserved {}", ex.body()));
        Ok(())
    });
}
```

## Dead letter channel with redelivery

The dead letter channel retries the failed step according to its `redeliveryPolicy`, then sends
the message to `deadLetterUri`. With `useOriginalMessage: true` the dead letter endpoint gets
the message as it entered the route, not the half-processed one, so it can be replayed.

<!-- example: eh-dead-letter-channel -->
```yaml
- errorHandler:
    deadLetterChannel:
      deadLetterUri: mock:dead-letters
      useOriginalMessage: true
      redeliveryPolicy:
        maximumRedeliveries: 3
        redeliveryDelay: 10
        useExponentialBackOff: true
        backOffMultiplier: 2
- route:
    from:
      uri: direct:reserve
      steps:
        - process:
            ref: inventoryApi
        - to: mock:reserved
```

```rust
b.error_handler(
    dead_letter_channel("mock:dead-letters")
        .use_original_message()
        .maximum_redeliveries(3)
        .redelivery_delay(10)
        .use_exponential_back_off()
        .back_off_multiplier(2.0),
);
b.from("direct:reserve")
    .process_ref("inventoryApi")
    .to("mock:reserved");
```

`sku-1` fails twice, succeeds on the second retry and reaches `mock:reserved` as
`reserved sku-1`. `poison` fails four times (one call and three retries, 10 ms, 20 ms and 40 ms
apart) and lands on `mock:dead-letters`; the caller does not get an error, because the dead
letter channel handled it. The YAML `errorHandler` applies to every route of the context;
`b.error_handler(...)` applies to the routes of one builder, and a route can set its own with
`errorHandler:` in YAML or `.error_handler(...)` in Rust.

Redelivery policy options: `maximumRedeliveries` (`-1` = forever), `redeliveryDelay`,
`useExponentialBackOff`, `backOffMultiplier`, `maximumRedeliveryDelay`,
`useCollisionAvoidance`, `collisionAvoidanceFactor`, `delayPattern` (`0:1000;5:5000`),
`retryAttemptedLogLevel`, `retriesExhaustedLogLevel`, `logStackTrace`, `logExhausted`,
`logRetryAttempted` and `asyncDelayedRedelivery` (release seda and timer consumer threads during
the delay). A dead letter channel can also set `deadLetterHandleNewException`.

## `onException`: react to one kind of failure

`onException` catches the listed exception kinds (and their subkinds). `handled: true` stops
the failure from reaching the caller, and the clause's steps build the reply. It can have its
own redelivery policy, `onWhen` (a predicate), `continued: true` (ignore the error and go on
with the route), `useOriginalMessage` and `retryWhile`. Put it in a `routeConfiguration` to
apply it to every route, as in Camel 4.

<!-- example: eh-on-exception -->
```yaml
- routeConfiguration:
    onException:
      - onException:
          exception:
            - java.lang.IllegalArgumentException
          handled:
            constant: true
          steps:
            - setHeader:
                name: CamelHttpResponseCode
                constant: 400
            - setBody:
                simple: "rejected: ${exception.message}"
      - onException:
          exception:
            - java.io.IOException
          redeliveryPolicy:
            maximumRedeliveries: 1
          handled:
            constant: true
          steps:
            - setBody:
                constant: "inventory unavailable, order queued"
            - to: mock:retry-later
- route:
    from:
      uri: direct:order
      steps:
        - filter:
            simple: "${body} == ''"
            steps:
              - throwException:
                  exceptionType: java.lang.IllegalArgumentException
                  message: empty order
        - process:
            ref: inventoryApi
        - to: mock:orders
```

```rust
b.on_exception(&["IllegalArgumentException"])
    .handled(true)
    .set_header("CamelHttpResponseCode", constant(400))
    .set_body(simple("rejected: ${exception.message}"));
b.on_exception(&["IOException"])
    .maximum_redeliveries(1)
    .handled(true)
    .set_body(constant("inventory unavailable, order queued"))
    .to("mock:retry-later");
b.from("direct:order")
    .filter(simple("${body} == ''"))
        .throw_exception("IllegalArgumentException", "empty order")
    .end()
    .process_ref("inventoryApi")
    .to("mock:orders");
```

An empty order gets the reply `rejected: empty order` with response code 400. An order for
`sku-1` fails once, is retried once (not enough: the API needs two retries), and goes to
`mock:retry-later` with the reply `inventory unavailable, order queued`. The most specific
matching `onException` wins, as in Camel.

## `doTry`, `doCatch` and `doFinally`

`doTry` handles errors inline, like `try`/`catch`/`finally`. `doCatch` takes a list of
exception kinds and an optional `onWhen`; `doFinally` always runs. The error handler does not
redeliver inside a `doTry`.

<!-- example: eh-do-try -->
```yaml
- route:
    from:
      uri: direct:parse
      steps:
        - doTry:
            steps:
              - unmarshal:
                  json: {}
              - setBody:
                  simple: "order ${body[id]}"
            doCatch:
              - exception:
                  - java.lang.Exception
                steps:
                  - setBody:
                      simple: "not JSON: ${exception.message}"
            doFinally:
              steps:
                - to: mock:parse-attempts
        - to: mock:parsed
```

```rust
b.from("direct:parse")
    .do_try()
        .unmarshal_json()
        .set_body(simple("order ${body[id]}"))
    .do_catch(&["Exception"])
        .set_body(simple("not JSON: ${exception.message}"))
    .do_finally()
        .to("mock:parse-attempts")
    .end_do_try()
    .to("mock:parsed");
```

`{"id": 5}` becomes `order 5`; `hello` becomes `not JSON: ...` with the parser's message. Both
pass through `mock:parse-attempts` and continue to `mock:parsed`.

## Error handling for AI and edge routes

- Wrap model calls in redelivery for transient errors (`HttpOperationFailedException`,
  `IOException`) and in a
  [circuit breaker](eips/flow-control.md#circuit-breaker) for outages. See
  [the agents cookbook](agents.md#rate-limits-and-a-circuit-breaker-around-a-model-api).
- On a gateway with an unreliable uplink, use a dead letter channel to a local `file:`
  directory and a second route that replays it. See
  [store and forward](edge.md#store-and-forward-when-the-uplink-is-down).
- Use `useOriginalMessage: true` whenever the dead letter queue is meant for replay.

## Differences from Apache Camel

- Exceptions are string kinds with a parent hierarchy, not Java classes. Custom kinds are
  registered with `register_kind_parent("MyException", "IOException")` so `onException` for
  `IOException` catches them. Every built-in kind and its parent is listed in
  [Troubleshooting](troubleshooting.md#exception-kinds).
- With `asyncDelayedRedelivery`, the waiting exchange is handed to a released thread and a new
  consumer thread takes over, instead of a scheduler continuation. The effect for the route is
  the same.
- A failure from a `ProducerTemplate` call is returned as `CamelExecutionException` with the
  original exception as its cause, as in Camel.

## Common questions

### How do I retry a failed HTTP call in Cameleer?

Add a `redeliveryPolicy` with `maximumRedeliveries` and `redeliveryDelay` to the error handler
or to an `onException` for `HttpOperationFailedException` or `IOException`. Only the failed
call is retried.

### How do I send failed messages to a dead letter queue?

Declare `errorHandler: deadLetterChannel: deadLetterUri: kafka:orders-dlq` (or any endpoint),
with `useOriginalMessage: true` so the queue holds replayable input.

### How do I return a 400 error from a REST route?

Throw `IllegalArgumentException` (or validate with `validate`), and handle it with an
`onException` that sets `CamelHttpResponseCode` to 400 and the body to
`${exception.message}`.

### What is the difference between `handled` and `continued`?

`handled: true` ends the route after the `onException` steps and answers the caller normally;
`continued: true` ignores the error and continues the route after the failed step.

## Related

- [EIPs](eips.md), [flow control and resilience](eips/flow-control.md)
- Apache Camel: [Error handling](https://camel.apache.org/manual/error-handler.html),
  [Exception clause](https://camel.apache.org/manual/exception-clause.html),
  [Dead Letter Channel](https://camel.apache.org/components/next/eips/dead-letter-channel.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
